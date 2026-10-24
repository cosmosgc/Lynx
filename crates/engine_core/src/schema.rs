//! Versioned component schemas, validation, and migrations.
//!
//! Content and persistence never serialize Rust memory layouts. A component is
//! a key, a [`SchemaVersion`], and a JSON object validated against an explicit
//! [`ComponentSchema`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{EngineError, ErrorCode};
use crate::id::{DefinitionId, IdKind, StableId};

/// Component field data. JSON is the explicit serialization format; the schema
/// supplies types, defaults, and constraints.
pub type ComponentData = Map<String, Value>;

/// A `major.minor` schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaVersion {
    /// Breaking changes.
    pub major: u16,
    /// Additive, backward-compatible changes.
    pub minor: u16,
}

impl SchemaVersion {
    /// Create a version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Compatibility is defined by major version only.
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A stable component type key such as `core.interactable`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentKey(String);

impl ComponentKey {
    /// Validate and create a component key.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] unless the key is a lowercase,
    /// dot-separated identifier with at least one namespace segment.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        let invalid = |reason: &str| {
            EngineError::new(ErrorCode::InvalidRequest, "invalid component key")
                .with_detail("reason", reason)
                .with_detail("value", &value)
        };

        if value.is_empty() || value.len() > 96 {
            return Err(invalid("must be 1..=96 characters"));
        }
        if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
            return Err(invalid("must have non-empty dot-separated segments"));
        }
        if !value.contains('.') {
            return Err(invalid("must include a namespace, e.g. core.transform"));
        }
        if let Some(bad) = value.chars().find(|c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        }) {
            return Err(invalid(&format!("unsupported character {bad:?}")));
        }
        if !value.split('.').all(|segment| {
            segment
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
        }) {
            return Err(invalid("segments must start with a letter or digit"));
        }
        Ok(Self(value))
    }

    /// The key as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ComponentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who may read a component's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Replicated to authorized clients.
    Client,
    /// Server only; never sent to a gameplay client.
    ServerOnly,
    /// Admin tools only.
    AdminOnly,
}

/// How long a component's data survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistencePolicy {
    /// Removed when no longer needed; never saved.
    Transient,
    /// Lasts for a player session or activity.
    Session,
    /// Saved periodically and on important changes.
    PersistentEntity,
    /// Append-only durable event history.
    PersistentEventLog,
    /// Authored content loaded by version.
    StaticDefinition,
}

/// Who may mutate a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationPermission {
    /// Any validated request may change it.
    Public,
    /// The owner or a public action may change it.
    OwnerOrPublic,
    /// Only the owning character or server may change it.
    Owner,
    /// Only server systems may change it.
    Server,
    /// Only admin tools may change it.
    Admin,
}

/// How a component is replicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationMode {
    /// Not replicated.
    None,
    /// Only to the controlling principal.
    Owner,
    /// Interest-managed.
    Interest,
    /// To every client in the zone.
    Always,
}

/// Replication policy for a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicationPolicy {
    /// Delivery category.
    pub mode: ReplicationMode,
    /// Maximum update frequency, if the mode is not `None`.
    pub max_hz: Option<u16>,
}

impl ReplicationPolicy {
    /// Not replicated.
    pub const NONE: Self = Self {
        mode: ReplicationMode::None,
        max_hz: None,
    };

    /// Interest-managed at `hz` updates per second.
    #[must_use]
    pub const fn interest(hz: u16) -> Self {
        Self {
            mode: ReplicationMode::Interest,
            max_hz: Some(hz),
        }
    }
}

/// The type and constraints of a single field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldType {
    /// Boolean.
    Bool,
    /// Signed 64-bit integer with inclusive bounds.
    Int {
        /// Inclusive minimum.
        min: i64,
        /// Inclusive maximum.
        max: i64,
    },
    /// Finite floating point with inclusive bounds.
    Float {
        /// Inclusive minimum.
        min: f64,
        /// Inclusive maximum.
        max: f64,
    },
    /// UTF-8 string with a character budget.
    String {
        /// Maximum number of characters.
        max_len: usize,
    },
    /// One of a fixed set of strings.
    Enum {
        /// Allowed values, in display order.
        values: Vec<String>,
    },
    /// A reference to authored content, optionally scoped to a namespace.
    DefinitionRef {
        /// Required namespace, for example `item`.
        namespace: Option<String>,
    },
    /// A durable entity reference of a specific kind.
    EntityRef {
        /// Required identifier kind.
        kind: IdKind,
    },
    /// A homogeneous list.
    List {
        /// Element type.
        item: Box<Self>,
        /// Maximum number of elements.
        max_len: usize,
    },
    /// A string-keyed map.
    Map {
        /// Value type.
        value: Box<Self>,
        /// Maximum number of entries.
        max_entries: usize,
    },
}

impl FieldType {
    /// A required string type with a 256 character budget.
    pub fn short_string() -> Self {
        Self::String { max_len: 256 }
    }

    /// Validate a JSON value at a diagnostic path, collecting issues.
    #[expect(
        clippy::too_many_lines,
        reason = "one branch per wire type keeps the validation rules together"
    )]
    pub fn validate(&self, value: &Value, path: &str, issues: &mut Vec<ValidationIssue>) {
        match self {
            Self::Bool => {
                if !value.is_boolean() {
                    issues.push(ValidationIssue::type_error(path, "bool", value));
                }
            }
            Self::Int { min, max } => match as_i64(value) {
                Some(number) if number >= *min && number <= *max => {}
                Some(number) => issues.push(ValidationIssue::new(
                    "schema.range",
                    path,
                    format!("{number} is outside the allowed range {min}..={max}"),
                )),
                None => issues.push(ValidationIssue::type_error(path, "integer", value)),
            },
            Self::Float { min, max } => match value.as_f64() {
                Some(number) if number.is_finite() && number >= *min && number <= *max => {}
                Some(number) => issues.push(ValidationIssue::new(
                    "schema.range",
                    path,
                    format!("{number} is outside the allowed range {min}..={max}"),
                )),
                None => issues.push(ValidationIssue::type_error(path, "number", value)),
            },
            Self::String { max_len } => match value.as_str() {
                Some(text) if text.chars().count() <= *max_len => {}
                Some(text) => issues.push(ValidationIssue::new(
                    "schema.too_long",
                    path,
                    format!(
                        "string has {} characters; maximum is {max_len}",
                        text.chars().count()
                    ),
                )),
                None => issues.push(ValidationIssue::type_error(path, "string", value)),
            },
            Self::Enum { values } => match value.as_str() {
                Some(text) if values.iter().any(|allowed| allowed == text) => {}
                Some(text) => issues.push(ValidationIssue::new(
                    "schema.enum",
                    path,
                    format!("{text:?} is not one of {values:?}"),
                )),
                None => issues.push(ValidationIssue::type_error(path, "string", value)),
            },
            Self::DefinitionRef { namespace } => match value.as_str() {
                Some(text) => match DefinitionId::new(text) {
                    Ok(id) => {
                        if let Some(required) = namespace
                            && !id.is_in(required)
                        {
                            issues.push(ValidationIssue::new(
                                "schema.namespace",
                                path,
                                format!("{id} must be in the {required:?} namespace"),
                            ));
                        }
                    }
                    Err(error) => issues.push(ValidationIssue::new(
                        "schema.definition_id",
                        path,
                        error.message().to_owned(),
                    )),
                },
                None => issues.push(ValidationIssue::type_error(path, "definition id", value)),
            },
            Self::EntityRef { kind } => match value.as_str() {
                Some(text) => match StableId::parse(text) {
                    Ok(id) if id.kind() == *kind => {}
                    Ok(id) => issues.push(ValidationIssue::new(
                        "schema.id_kind",
                        path,
                        format!("expected a {} id, got {}", kind.label(), id.kind().label()),
                    )),
                    Err(error) => issues.push(ValidationIssue::new(
                        "schema.entity_id",
                        path,
                        error.message().to_owned(),
                    )),
                },
                None => issues.push(ValidationIssue::type_error(path, "entity id", value)),
            },
            Self::List { item, max_len } => match value.as_array() {
                Some(items) if items.len() <= *max_len => {
                    for (index, element) in items.iter().enumerate() {
                        item.validate(element, &format!("{path}[{index}]"), issues);
                    }
                }
                Some(items) => issues.push(ValidationIssue::new(
                    "schema.too_many",
                    path,
                    format!("list has {} elements; maximum is {max_len}", items.len()),
                )),
                None => issues.push(ValidationIssue::type_error(path, "list", value)),
            },
            Self::Map {
                value: item,
                max_entries,
            } => match value.as_object() {
                Some(map) if map.len() <= *max_entries => {
                    for (key, element) in map {
                        item.validate(element, &format!("{path}.{key}"), issues);
                    }
                }
                Some(map) => issues.push(ValidationIssue::new(
                    "schema.too_many",
                    path,
                    format!("map has {} entries; maximum is {max_entries}", map.len()),
                )),
                None => issues.push(ValidationIssue::type_error(path, "map", value)),
            },
        }
    }
}

/// One field in a component schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSchema {
    /// Field name, used as the JSON key.
    pub name: String,
    /// Type and constraints.
    pub ty: FieldType,
    /// Whether the field must be present (or have a default).
    pub required: bool,
    /// Value inserted when the field is missing.
    pub default: Option<Value>,
    /// Human-readable authoring note.
    pub description: Option<String>,
}

impl FieldSchema {
    /// A required field with no default.
    #[must_use]
    pub fn required(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
            default: None,
            description: None,
        }
    }

    /// An optional field with no default.
    #[must_use]
    pub fn optional(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: false,
            default: None,
            description: None,
        }
    }

    /// Attach a default value.
    #[must_use]
    pub fn with_default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }

    /// Attach an authoring note.
    #[must_use]
    pub fn described_as(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// A versioned, validated component definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentSchema {
    /// Stable type key.
    pub key: ComponentKey,
    /// Current schema version.
    pub version: SchemaVersion,
    /// Fields, in authoring order.
    pub fields: Vec<FieldSchema>,
    /// Client visibility.
    pub visibility: Visibility,
    /// Persistence lifetime.
    pub persistence: PersistencePolicy,
    /// Replication policy.
    pub replication: ReplicationPolicy,
    /// Who may mutate values.
    pub mutation: MutationPermission,
    /// Ordered migrations from older versions to this one.
    pub migrations: Vec<Migration>,
    /// Optional authoring note.
    pub description: Option<String>,
}

impl ComponentSchema {
    /// Create a schema with no migrations.
    #[must_use]
    pub fn new(
        key: ComponentKey,
        version: SchemaVersion,
        fields: Vec<FieldSchema>,
        visibility: Visibility,
        persistence: PersistencePolicy,
    ) -> Self {
        Self {
            key,
            version,
            fields,
            visibility,
            persistence,
            replication: ReplicationPolicy::NONE,
            mutation: MutationPermission::Server,
            migrations: Vec::new(),
            description: None,
        }
    }

    /// Set the replication policy.
    #[must_use]
    pub fn with_replication(mut self, replication: ReplicationPolicy) -> Self {
        self.replication = replication;
        self
    }

    /// Set the mutation permission.
    #[must_use]
    pub fn with_mutation(mut self, mutation: MutationPermission) -> Self {
        self.mutation = mutation;
        self
    }

    /// Set the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Attach a migration step.
    #[must_use]
    pub fn with_migration(mut self, migration: Migration) -> Self {
        self.migrations.push(migration);
        self
    }

    /// Find a field by name.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldSchema> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Insert defaults for missing fields.
    pub fn apply_defaults(&self, data: &mut ComponentData) {
        for field in &self.fields {
            if !data.contains_key(&field.name)
                && let Some(default) = &field.default
            {
                data.insert(field.name.clone(), default.clone());
            }
        }
    }

    /// Validate component data, reporting every issue rather than stopping at
    /// the first.
    #[must_use]
    pub fn validate(&self, data: &ComponentData) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        for field in &self.fields {
            let path = format!("components.{}.{}", self.key, field.name);
            match data.get(&field.name) {
                Some(value) => field.ty.validate(value, &path, &mut issues),
                None if field.default.is_none() && field.required => {
                    issues.push(ValidationIssue::new(
                        "schema.missing_field",
                        &path,
                        "required field is missing",
                    ));
                }
                None => {}
            }
        }

        let known: BTreeSet<&str> = self
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        for key in data.keys() {
            if !known.contains(key.as_str()) {
                issues.push(ValidationIssue::new(
                    "schema.unknown_field",
                    format!("components.{}.{key}", self.key),
                    "field is not declared by the schema",
                ));
            }
        }
        issues
    }

    /// Apply the migration chain from `from` to this schema's version.
    fn migrate_data(
        &self,
        from: SchemaVersion,
        data: &mut ComponentData,
    ) -> Result<(), EngineError> {
        let mut current = from;
        let mut guard = 0;
        while current < self.version {
            guard += 1;
            if guard > self.migrations.len() + 1 {
                return Err(EngineError::new(
                    ErrorCode::InvalidState,
                    "component migration chain does not terminate",
                )
                .with_detail("component", self.key.to_string())
                .with_detail("version", current.to_string()));
            }
            let migration = self
                .migrations
                .iter()
                .find(|migration| migration.from == current)
                .ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::VersionMismatch,
                        "no migration path for component data",
                    )
                    .with_detail("component", self.key.to_string())
                    .with_detail("from", current.to_string())
                    .with_detail("to", self.version.to_string())
                })?;
            migration.apply(data)?;
            current = migration.to;
        }
        Ok(())
    }
}

/// A validation problem, suitable for content diagnostics and UI errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// Stable machine-readable code, for example `schema.range`.
    pub code: String,
    /// Diagnostic path such as `components.core.interactable.range_m`.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
}

impl ValidationIssue {
    /// Create an issue.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }

    fn type_error(path: &str, expected: &str, actual: &Value) -> Self {
        Self::new(
            "schema.type",
            path,
            format!("expected {expected}, found {}", json_type_name(actual)),
        )
    }
}

/// One step in a component migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MigrationStep {
    /// Rename a field, preserving its value.
    RenameField {
        /// Old field name.
        from: String,
        /// New field name.
        to: String,
    },
    /// Insert a default when the field is absent.
    SetDefault {
        /// Field name.
        field: String,
        /// Value to insert.
        value: Value,
    },
    /// Remove an obsolete field.
    DropField {
        /// Field name.
        field: String,
    },
    /// Replace one enum string with another.
    MapEnumValue {
        /// Field name.
        field: String,
        /// Value to replace.
        from: String,
        /// Replacement value.
        to: String,
    },
}

/// A migration from one schema version to another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    /// Source version.
    pub from: SchemaVersion,
    /// Destination version.
    pub to: SchemaVersion,
    /// Ordered steps applied to component data.
    pub steps: Vec<MigrationStep>,
}

impl Migration {
    /// Create a migration.
    #[must_use]
    pub fn new(from: SchemaVersion, to: SchemaVersion, steps: Vec<MigrationStep>) -> Self {
        Self { from, to, steps }
    }

    /// Apply all steps to `data`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] when a rename collides with an existing
    /// field, or [`ErrorCode::InvalidRequest`] when the migration is
    /// malformed.
    pub fn apply(&self, data: &mut ComponentData) -> Result<(), EngineError> {
        if self.to <= self.from {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "migration target must be newer than source",
            )
            .with_detail("from", self.from.to_string())
            .with_detail("to", self.to.to_string()));
        }

        for step in &self.steps {
            match step {
                MigrationStep::RenameField { from, to } => {
                    if let Some(value) = data.remove(from) {
                        if data.contains_key(to) {
                            return Err(EngineError::new(
                                ErrorCode::Conflict,
                                "migration rename target already exists",
                            )
                            .with_detail("field", to)
                            .with_detail("version", self.to.to_string()));
                        }
                        data.insert(to.clone(), value);
                    }
                }
                MigrationStep::SetDefault { field, value } => {
                    data.entry(field).or_insert_with(|| value.clone());
                }
                MigrationStep::DropField { field } => {
                    data.remove(field);
                }
                MigrationStep::MapEnumValue { field, from, to } => {
                    if let Some(Value::String(current)) = data.get_mut(field)
                        && current == from
                    {
                        current.clone_from(to);
                    }
                }
            }
        }
        Ok(())
    }
}

/// A concrete component value with its schema version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentInstance {
    /// Component type key.
    pub key: ComponentKey,
    /// Version of the data below.
    pub version: SchemaVersion,
    /// Explicit field data.
    pub data: ComponentData,
}

impl ComponentInstance {
    /// Create an instance at a specific version.
    #[must_use]
    pub fn new(key: ComponentKey, version: SchemaVersion, data: ComponentData) -> Self {
        Self { key, version, data }
    }
}

/// The result of preparing component data for use.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedComponent {
    /// Migrated, defaulted, and validated data.
    pub instance: ComponentInstance,
    /// Non-fatal issues. An empty vector means the data is valid.
    pub issues: Vec<ValidationIssue>,
}

/// All known component schemas.
#[derive(Debug, Clone, Default)]
pub struct ComponentRegistry {
    schemas: BTreeMap<ComponentKey, ComponentSchema>,
}

impl ComponentRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a schema.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for duplicate keys, duplicate field
    /// names, or overlapping migrations; [`ErrorCode::InvalidRequest`] for a
    /// migration chain that does not reach the schema version.
    pub fn register(&mut self, schema: ComponentSchema) -> Result<(), EngineError> {
        if self.schemas.contains_key(&schema.key) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "component schema is already registered",
            )
            .with_detail("component", schema.key.to_string()));
        }

        let mut names = BTreeSet::new();
        for field in &schema.fields {
            if !is_valid_field_name(&field.name) {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "invalid component field name",
                )
                .with_detail("component", schema.key.to_string())
                .with_detail("field", &field.name));
            }
            if !names.insert(field.name.as_str()) {
                return Err(EngineError::new(
                    ErrorCode::Conflict,
                    "duplicate component field name",
                )
                .with_detail("component", schema.key.to_string())
                .with_detail("field", &field.name));
            }
            if let Some(default) = &field.default {
                let mut probe = ComponentData::new();
                probe.insert(field.name.clone(), default.clone());
                let mut issues = Vec::new();
                field.ty.validate(default, &field.name, &mut issues);
                if !issues.is_empty() {
                    return Err(EngineError::new(
                        ErrorCode::InvalidRequest,
                        "component field default does not satisfy its type",
                    )
                    .with_detail("component", schema.key.to_string())
                    .with_detail("field", &field.name)
                    .with_detail("issue", issues[0].message.clone()));
                }
            }
        }

        let mut migrations = schema.migrations.clone();
        migrations.sort_by_key(|migration| migration.from);
        let mut current = migrations
            .first()
            .map_or(schema.version, |migration| migration.from);
        for migration in &migrations {
            if migration.from != current {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "component migrations must form a contiguous chain",
                )
                .with_detail("component", schema.key.to_string())
                .with_detail("expected_from", current.to_string())
                .with_detail("actual_from", migration.from.to_string()));
            }
            if migration.to <= migration.from {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "component migration must advance the version",
                )
                .with_detail("component", schema.key.to_string()));
            }
            current = migration.to;
        }
        if current != schema.version {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "component migrations must end at the schema version",
            )
            .with_detail("component", schema.key.to_string())
            .with_detail("chain_end", current.to_string())
            .with_detail("schema_version", schema.version.to_string()));
        }

        self.schemas.insert(schema.key.clone(), schema);
        Ok(())
    }

    /// Look up a schema.
    #[must_use]
    pub fn get(&self, key: &ComponentKey) -> Option<&ComponentSchema> {
        self.schemas.get(key)
    }

    /// All schemas in key order.
    pub fn iter(&self) -> impl Iterator<Item = &ComponentSchema> {
        self.schemas.values()
    }

    /// The number of registered schemas.
    #[must_use]
    pub fn len(&self) -> usize {
        self.schemas.len()
    }

    /// Whether no schemas are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }

    /// Migrate an instance to the latest schema version, apply defaults, and
    /// validate it.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unknown component,
    /// [`ErrorCode::VersionMismatch`] when the instance is newer than the
    /// registry or no migration path exists.
    pub fn prepare(&self, instance: &ComponentInstance) -> Result<PreparedComponent, EngineError> {
        let schema = self
            .get(&instance.key)
            .ok_or_else(|| EngineError::not_found("component schema", &instance.key))?;

        if instance.version > schema.version {
            return Err(EngineError::new(
                ErrorCode::VersionMismatch,
                "component data is newer than this engine supports",
            )
            .with_detail("component", schema.key.to_string())
            .with_detail("data_version", instance.version.to_string())
            .with_detail("max_version", schema.version.to_string()));
        }

        let mut data = instance.data.clone();
        schema.migrate_data(instance.version, &mut data)?;
        schema.apply_defaults(&mut data);
        let issues = schema.validate(&data);

        Ok(PreparedComponent {
            instance: ComponentInstance {
                key: instance.key.clone(),
                version: schema.version,
                data,
            },
            issues,
        })
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "map",
    }
}

fn is_valid_field_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::assert_is_empty)]

    use super::*;
    use serde_json::json;

    fn key(value: &str) -> ComponentKey {
        ComponentKey::new(value).unwrap()
    }

    fn health_schema() -> ComponentSchema {
        ComponentSchema::new(
            key("game.health"),
            SchemaVersion::new(2, 0),
            vec![
                FieldSchema::required("current", FieldType::Int { min: 0, max: 1_000 })
                    .with_default(json!(100)),
                FieldSchema::required("max", FieldType::Int { min: 1, max: 1_000 })
                    .with_default(json!(100)),
                FieldSchema::optional(
                    "state",
                    FieldType::Enum {
                        values: vec!["alive".into(), "dead".into()],
                    },
                )
                .with_default(json!("alive")),
            ],
            Visibility::Client,
            PersistencePolicy::PersistentEntity,
        )
        .with_replication(ReplicationPolicy::interest(10))
        .with_mutation(MutationPermission::Server)
        .with_migration(Migration::new(
            SchemaVersion::new(1, 0),
            SchemaVersion::new(2, 0),
            vec![
                MigrationStep::RenameField {
                    from: "hp".into(),
                    to: "current".into(),
                },
                MigrationStep::SetDefault {
                    field: "max".into(),
                    value: json!(100),
                },
            ],
        ))
    }

    #[test]
    fn keys_and_fields_are_validated() {
        assert!(ComponentKey::new("core.transform").is_ok());
        assert!(ComponentKey::new("core").is_err());
        assert!(ComponentKey::new("Core.Transform").is_err());
        assert!(ComponentKey::new("core..transform").is_err());
        assert!(ComponentKey::new(".core.transform").is_err());
    }

    #[test]
    fn defaults_are_applied_and_required_fields_enforced() {
        let schema = health_schema();
        let mut data = ComponentData::new();
        schema.apply_defaults(&mut data);
        assert_eq!(data.get("current"), Some(&json!(100)));
        assert!(schema.validate(&data).is_empty());

        let empty = ComponentData::new();
        let issues = schema.validate(&empty);
        // Defaults satisfy required fields, so an empty object is valid.
        assert!(issues.is_empty());
    }

    #[test]
    fn validation_reports_ranges_enums_and_unknown_fields() {
        let schema = health_schema();
        let data: ComponentData = serde_json::from_value(json!({
            "current": -5,
            "max": 10,
            "state": "undead",
            "extra": true
        }))
        .unwrap();
        let issues = schema.validate(&data);
        let codes: Vec<_> = issues.iter().map(|issue| issue.code.as_str()).collect();
        assert!(codes.contains(&"schema.range"));
        assert!(codes.contains(&"schema.enum"));
        assert!(codes.contains(&"schema.unknown_field"));
        assert!(
            issues
                .iter()
                .any(|issue| issue.path == "components.game.health.current")
        );
    }

    #[test]
    fn nested_types_validate_with_indexed_paths() {
        let schema = ComponentSchema::new(
            key("game.inventory"),
            SchemaVersion::new(1, 0),
            vec![FieldSchema::required(
                "slots",
                FieldType::List {
                    item: Box::new(FieldType::Map {
                        value: Box::new(FieldType::Int { min: 1, max: 99 }),
                        max_entries: 4,
                    }),
                    max_len: 3,
                },
            )],
            Visibility::ServerOnly,
            PersistencePolicy::PersistentEntity,
        );
        let data: ComponentData = serde_json::from_value(json!({
            "slots": [
                {"item": 1},
                {"item": 100},
                {"item": 2}
            ]
        }))
        .unwrap();
        let issues = schema.validate(&data);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, "components.game.inventory.slots[1].item");

        let data: ComponentData = serde_json::from_value(
            json!({"slots": [{"item": 1}, {"item": 2}, {"item": 3}, {"item": 4}]}),
        )
        .unwrap();
        assert!(
            schema
                .validate(&data)
                .iter()
                .any(|issue| issue.code == "schema.too_many")
        );
    }

    #[test]
    fn reference_types_check_kind_and_namespace() {
        let schema = ComponentSchema::new(
            key("game.links"),
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::required(
                    "owner",
                    FieldType::EntityRef {
                        kind: IdKind::Character,
                    },
                ),
                FieldSchema::required(
                    "item",
                    FieldType::DefinitionRef {
                        namespace: Some("item".into()),
                    },
                ),
            ],
            Visibility::Client,
            PersistencePolicy::PersistentEntity,
        );
        let character = StableId::from_parts(IdKind::Character, 1, 1).to_string();
        let entity = StableId::from_parts(IdKind::Entity, 1, 2).to_string();

        let valid: ComponentData = serde_json::from_value(json!({
            "owner": character,
            "item": "item.torch"
        }))
        .unwrap();
        assert!(schema.validate(&valid).is_empty());

        let wrong_kind: ComponentData = serde_json::from_value(json!({
            "owner": entity,
            "item": "door.wooden"
        }))
        .unwrap();
        let codes: Vec<_> = schema
            .validate(&wrong_kind)
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"schema.id_kind".to_string()));
        assert!(codes.contains(&"schema.namespace".to_string()));
    }

    #[test]
    fn registry_migrates_and_defaults_old_data() {
        let mut registry = ComponentRegistry::new();
        registry.register(health_schema()).unwrap();

        let old = ComponentInstance::new(
            key("game.health"),
            SchemaVersion::new(1, 0),
            serde_json::from_value(json!({"hp": 42})).unwrap(),
        );
        let prepared = registry.prepare(&old).unwrap();
        assert_eq!(prepared.instance.version, SchemaVersion::new(2, 0));
        assert_eq!(prepared.instance.data.get("current"), Some(&json!(42)));
        assert_eq!(prepared.instance.data.get("max"), Some(&json!(100)));
        assert_eq!(prepared.instance.data.get("state"), Some(&json!("alive")));
        assert!(prepared.issues.is_empty());
    }

    #[test]
    fn registry_rejects_duplicates_and_broken_chains() {
        let mut registry = ComponentRegistry::new();
        registry.register(health_schema()).unwrap();
        assert_eq!(
            registry.register(health_schema()).unwrap_err().code(),
            ErrorCode::Conflict
        );

        let broken = ComponentSchema::new(
            key("game.broken"),
            SchemaVersion::new(3, 0),
            vec![],
            Visibility::Client,
            PersistencePolicy::Transient,
        )
        .with_migration(Migration::new(
            SchemaVersion::new(1, 0),
            SchemaVersion::new(2, 0),
            vec![],
        ));
        let error = ComponentRegistry::new().register(broken).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidRequest);
        assert!(error.message().contains("end at the schema version"));
    }

    #[test]
    fn registry_refuses_newer_data_and_unknown_components() {
        let mut registry = ComponentRegistry::new();
        registry.register(health_schema()).unwrap();

        let future = ComponentInstance::new(
            key("game.health"),
            SchemaVersion::new(3, 0),
            ComponentData::new(),
        );
        assert_eq!(
            registry.prepare(&future).unwrap_err().code(),
            ErrorCode::VersionMismatch
        );

        let unknown = ComponentInstance::new(
            key("game.unknown"),
            SchemaVersion::new(1, 0),
            ComponentData::new(),
        );
        assert_eq!(
            registry.prepare(&unknown).unwrap_err().code(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn migration_collision_is_a_conflict() {
        let mut data: ComponentData = serde_json::from_value(json!({"a": 1, "b": 2})).unwrap();
        let migration = Migration::new(
            SchemaVersion::new(1, 0),
            SchemaVersion::new(2, 0),
            vec![MigrationStep::RenameField {
                from: "a".into(),
                to: "b".into(),
            }],
        );
        assert_eq!(
            migration.apply(&mut data).unwrap_err().code(),
            ErrorCode::Conflict
        );
    }
}
