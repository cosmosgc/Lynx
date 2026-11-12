//! Deterministic, preconditioned edits used by reviewed content migrations.

use std::collections::BTreeSet;

use engine_core::i18n::Locale;
use engine_core::id::DefinitionId;
use engine_core::schema::{ComponentInstance, ComponentKey, SchemaVersion};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bundle::is_sha256_hex;

/// A complete content-bundle transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentMigrationPlan {
    /// Stable lowercase identifier used for reports and rollback snapshots.
    pub id: String,
    /// Hash of the exact source bundle manifest.
    pub source_manifest_sha256: String,
    /// Content version the project must currently use.
    pub from_content_version: semver::Version,
    /// Content version written after a successful migration.
    pub to_content_version: semver::Version,
    /// Ordered definition transitions.
    #[serde(default)]
    pub definitions: Vec<DefinitionMigration>,
    /// Ordered locale-catalog transitions.
    #[serde(default)]
    pub catalogs: Vec<CatalogMigration>,
}

/// A schema-version transition for one stable definition ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefinitionMigration {
    /// Definition this step transforms.
    pub definition: DefinitionId,
    /// Exact schema version expected in the source content.
    pub from_version: SchemaVersion,
    /// Schema version assigned after all operations succeed.
    pub to_version: SchemaVersion,
    /// Ordered, preconditioned data edits.
    pub operations: Vec<MigrationOperation>,
}

/// A reviewed transition for persisted instances between exact content bundles.
///
/// This is separate from [`ContentMigrationPlan`]: its operations target the
/// mutable component data stored on entities, not authored definition files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedInstanceMigrationPlan {
    /// Stable lowercase identifier used in reports and rollback checkpoints.
    pub id: String,
    /// SHA-256 of the exact source bundle manifest.
    pub source_manifest_sha256: String,
    /// SHA-256 of the exact target bundle manifest.
    pub target_manifest_sha256: String,
    /// Content version required by the source world snapshot.
    pub from_content_version: semver::Version,
    /// Content version of the candidate bundle.
    pub to_content_version: semver::Version,
    /// Ordered component transformations applied to matching prototypes.
    #[serde(default)]
    pub components: Vec<PersistedComponentMigration>,
}

/// A schema transition for one component on instances of one prototype.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedComponentMigration {
    /// Stable authored prototype whose instances are transformed.
    pub prototype: DefinitionId,
    /// Component value to transform.
    pub component: ComponentKey,
    /// Exact component schema version expected in the source instance.
    pub from_version: SchemaVersion,
    /// Component schema version written by the transition.
    pub to_version: SchemaVersion,
    /// Ordered, preconditioned edits to the component's data object.
    pub operations: Vec<MigrationOperation>,
}

impl PersistedInstanceMigrationPlan {
    /// Validate transition identity and ensure each target is unambiguous.
    ///
    /// # Errors
    /// Returns [`MigrationError::InvalidPlan`] when the transition is stale,
    /// ambiguous, or does not advance its content and component versions.
    pub fn validate(&self) -> Result<(), MigrationError> {
        if !valid_migration_id(&self.id) {
            return Err(MigrationError::InvalidPlan(
                "id must use 1..=96 lowercase alphanumeric, dot, underscore, or hyphen characters"
                    .into(),
            ));
        }
        if !crate::bundle::is_sha256_hex(&self.source_manifest_sha256)
            || !crate::bundle::is_sha256_hex(&self.target_manifest_sha256)
        {
            return Err(MigrationError::InvalidPlan(
                "source and target manifest identities must be SHA-256 hex digests".into(),
            ));
        }
        if self.source_manifest_sha256 == self.target_manifest_sha256 {
            return Err(MigrationError::InvalidPlan(
                "source and target manifest identities must differ".into(),
            ));
        }
        if self.from_content_version >= self.to_content_version {
            return Err(MigrationError::InvalidPlan(
                "target content version must be newer than source".into(),
            ));
        }

        let mut targets = BTreeSet::new();
        for migration in &self.components {
            if !targets.insert((&migration.prototype, &migration.component)) {
                return Err(MigrationError::InvalidPlan(format!(
                    "duplicate persisted component target {}:{}",
                    migration.prototype, migration.component
                )));
            }
            if migration.to_version <= migration.from_version {
                return Err(MigrationError::InvalidPlan(format!(
                    "persisted component {}:{} target schema version must increase",
                    migration.prototype, migration.component
                )));
            }
            validate_operations(&migration.operations, OperationScope::Component)?;
        }
        if self.components.is_empty() {
            return Err(MigrationError::InvalidPlan(
                "plan must contain at least one persisted component transition".into(),
            ));
        }
        Ok(())
    }

    /// Transform a matching component without changing the supplied instance.
    ///
    /// `Ok(None)` means this plan does not target the prototype/component pair.
    /// A matching component must be at exactly the declared source version;
    /// callers should quarantine the containing entity on any error. The
    /// returned instance still requires validation against the target schema.
    ///
    /// # Errors
    /// Returns [`MigrationError::InvalidPlan`] if the plan is invalid,
    /// [`MigrationError::VersionMismatch`] if the component version does not
    /// match the declared source, or an operation error when a precondition
    /// fails.
    pub fn migrate_component(
        &self,
        prototype: &DefinitionId,
        instance: &ComponentInstance,
    ) -> Result<Option<ComponentInstance>, MigrationError> {
        self.validate()?;
        let Some(migration) = self.components.iter().find(|migration| {
            &migration.prototype == prototype && migration.component == instance.key
        }) else {
            return Ok(None);
        };
        if instance.version != migration.from_version {
            return Err(MigrationError::VersionMismatch(format!(
                "persisted component {}:{} expected source version {}, found {}",
                migration.prototype, migration.component, migration.from_version, instance.version
            )));
        }

        let mut data = Value::Object(instance.data.clone());
        apply_operations(&mut data, &migration.operations)?;
        let Value::Object(data) = data else {
            return Err(MigrationError::InvalidPlan(
                "persisted component migration changed its data object into another JSON type"
                    .into(),
            ));
        };
        Ok(Some(ComponentInstance::new(
            instance.key.clone(),
            migration.to_version,
            data,
        )))
    }
}

/// A set of message-key edits for one locale catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogMigration {
    /// Canonical locale to transform.
    pub locale: Locale,
    /// Ordered, preconditioned data edits under `/messages`.
    pub operations: Vec<MigrationOperation>,
}

impl ContentMigrationPlan {
    /// Validate plan identity, version transitions, targets, and operation paths.
    ///
    /// # Errors
    /// Returns a [`MigrationError`] for stale or ambiguous plan metadata.
    pub fn validate(&self) -> Result<(), MigrationError> {
        if !valid_migration_id(&self.id) {
            return Err(MigrationError::InvalidPlan(
                "id must use 1..=96 lowercase alphanumeric, dot, underscore, or hyphen characters"
                    .into(),
            ));
        }
        if !is_sha256_hex(&self.source_manifest_sha256) {
            return Err(MigrationError::InvalidPlan(
                "source_manifest_sha256 must be a SHA-256 hex digest".into(),
            ));
        }
        if self.from_content_version >= self.to_content_version {
            return Err(MigrationError::InvalidPlan(
                "target content version must be newer than source".into(),
            ));
        }

        let mut definitions = BTreeSet::new();
        for migration in &self.definitions {
            if !definitions.insert(&migration.definition) {
                return Err(MigrationError::InvalidPlan(format!(
                    "duplicate definition target {}",
                    migration.definition
                )));
            }
            if migration.to_version <= migration.from_version {
                return Err(MigrationError::InvalidPlan(format!(
                    "definition {} target schema version must increase",
                    migration.definition
                )));
            }
            validate_operations(&migration.operations, OperationScope::Definition)?;
        }

        let mut catalogs = BTreeSet::new();
        for migration in &self.catalogs {
            if !catalogs.insert(&migration.locale) {
                return Err(MigrationError::InvalidPlan(format!(
                    "duplicate locale target {}",
                    migration.locale
                )));
            }
            validate_operations(&migration.operations, OperationScope::Catalog)?;
        }
        if self.definitions.is_empty() && self.catalogs.is_empty() {
            return Err(MigrationError::InvalidPlan(
                "plan must contain at least one definition or catalog transition".into(),
            ));
        }
        Ok(())
    }
}

/// One data edit in a content migration plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MigrationOperation {
    /// Replace an existing object field when it still has the expected value.
    Replace {
        /// JSON Pointer to the field being changed.
        path: String,
        /// Required prior value; protects against stale migration plans.
        expected: Value,
        /// Replacement value.
        value: Value,
    },
    /// Remove an existing object field when it still has the expected value.
    Remove {
        /// JSON Pointer to the field being removed.
        path: String,
        /// Required prior value; protects against stale migration plans.
        expected: Value,
    },
    /// Rename an object field without changing its value.
    Rename {
        /// JSON Pointer to the existing field.
        from: String,
        /// JSON Pointer to the new sibling field.
        to: String,
        /// Required prior value; protects against stale migration plans.
        expected: Value,
    },
}

/// A migration operation failed its path or precondition checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrationError {
    /// Plan metadata is invalid or targets conflict.
    #[error("invalid migration plan: {0}")]
    InvalidPlan(String),
    /// A persisted instance did not match the declared migration source.
    #[error("persisted component migration version mismatch: {0}")]
    VersionMismatch(String),
    /// The pointer is malformed or does not address an object field.
    #[error("migration path is not a valid object-field JSON Pointer: {0}")]
    InvalidPointer(String),
    /// The parent value is not a JSON object.
    #[error("migration path parent is not an object: {0}")]
    ParentNotObject(String),
    /// The expected source field is absent.
    #[error("migration source field does not exist: {0}")]
    MissingField(String),
    /// The source field differs from the plan's expected value.
    #[error("migration source value differs from expected value: {0}")]
    UnexpectedValue(String),
    /// A rename destination is already present.
    #[error("migration rename destination already exists: {0}")]
    DestinationExists(String),
    /// A rename must remain inside one object so its scope stays explicit.
    #[error("migration rename must target a sibling field: {0} -> {1}")]
    RenameAcrossObjects(String, String),
}

#[derive(Clone, Copy)]
enum OperationScope {
    Definition,
    Catalog,
    Component,
}

fn valid_migration_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id.as_bytes()[0].is_ascii_lowercase()
        && !id.ends_with('.')
        && !id.contains("..")
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn validate_operations(
    operations: &[MigrationOperation],
    scope: OperationScope,
) -> Result<(), MigrationError> {
    if operations.is_empty() {
        return Err(MigrationError::InvalidPlan(
            "each migration target must contain at least one operation".into(),
        ));
    }
    for operation in operations {
        let paths = match operation {
            MigrationOperation::Replace { path, .. } | MigrationOperation::Remove { path, .. } => {
                vec![path.as_str()]
            }
            MigrationOperation::Rename { from, to, .. } => vec![from.as_str(), to.as_str()],
        };
        for path in paths {
            let (_, key) = split_pointer(path)?;
            match scope {
                OperationScope::Definition if matches!(path, "/id" | "/version") => {
                    return Err(MigrationError::InvalidPlan(
                        "definition ID and schema version are controlled by the migration step"
                            .into(),
                    ));
                }
                OperationScope::Catalog if !path.starts_with("/messages/") => {
                    return Err(MigrationError::InvalidPlan(format!(
                        "catalog operation must address a message under /messages (found {path})"
                    )));
                }
                OperationScope::Catalog if key.is_empty() => {
                    return Err(MigrationError::InvalidPlan(
                        "catalog operation must name a message key".into(),
                    ));
                }
                OperationScope::Component if key.is_empty() => {
                    return Err(MigrationError::InvalidPlan(
                        "component operation must name a data field".into(),
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Apply an ordered set of edits atomically to one serialized content value.
///
/// Each operation checks its expected source value. If any operation fails,
/// `document` remains unchanged.
/// Paths use RFC 6901 escaping and must address fields in JSON objects.
///
/// # Errors
/// Returns a [`MigrationError`] when a path is malformed, an expected value no
/// longer matches, or a rename destination already exists.
pub fn apply_operations(
    document: &mut Value,
    operations: &[MigrationOperation],
) -> Result<(), MigrationError> {
    let mut candidate = document.clone();
    for operation in operations {
        apply_one(&mut candidate, operation)?;
    }
    *document = candidate;
    Ok(())
}

fn apply_one(document: &mut Value, operation: &MigrationOperation) -> Result<(), MigrationError> {
    match operation {
        MigrationOperation::Replace {
            path,
            expected,
            value,
        } => {
            let (parent, key) = parent_and_key_mut(document, path)?;
            let current = parent
                .get(&key)
                .ok_or_else(|| MigrationError::MissingField(path.clone()))?;
            if current != expected {
                return Err(MigrationError::UnexpectedValue(path.clone()));
            }
            parent.insert(key, value.clone());
        }
        MigrationOperation::Remove { path, expected } => {
            let (parent, key) = parent_and_key_mut(document, path)?;
            let current = parent
                .get(&key)
                .ok_or_else(|| MigrationError::MissingField(path.clone()))?;
            if current != expected {
                return Err(MigrationError::UnexpectedValue(path.clone()));
            }
            parent.remove(&key);
        }
        MigrationOperation::Rename { from, to, expected } => {
            let (from_parent, from_key) = split_pointer(from)?;
            let (to_parent, to_key) = split_pointer(to)?;
            if from_parent != to_parent {
                return Err(MigrationError::RenameAcrossObjects(
                    from.clone(),
                    to.clone(),
                ));
            }
            let parent = object_at_mut(document, &from_parent, from)?;
            let current = parent
                .get(&from_key)
                .ok_or_else(|| MigrationError::MissingField(from.clone()))?;
            if current != expected {
                return Err(MigrationError::UnexpectedValue(from.clone()));
            }
            if parent.contains_key(&to_key) {
                return Err(MigrationError::DestinationExists(to.clone()));
            }
            let value = parent
                .remove(&from_key)
                .expect("source field existence was checked above");
            parent.insert(to_key, value);
        }
    }
    Ok(())
}

fn parent_and_key_mut<'a>(
    document: &'a mut Value,
    path: &str,
) -> Result<(&'a mut Map<String, Value>, String), MigrationError> {
    let (parent_path, key) = split_pointer(path)?;
    let parent = object_at_mut(document, &parent_path, path)?;
    Ok((parent, key))
}

fn object_at_mut<'a>(
    document: &'a mut Value,
    parent_path: &str,
    full_path: &str,
) -> Result<&'a mut Map<String, Value>, MigrationError> {
    let parent = if parent_path.is_empty() {
        document
    } else {
        document
            .pointer_mut(parent_path)
            .ok_or_else(|| MigrationError::MissingField(parent_path.to_owned()))?
    };
    parent
        .as_object_mut()
        .ok_or_else(|| MigrationError::ParentNotObject(full_path.to_owned()))
}

fn split_pointer(path: &str) -> Result<(String, String), MigrationError> {
    if !path.starts_with('/') || path == "/" {
        return Err(MigrationError::InvalidPointer(path.to_owned()));
    }
    if path[1..]
        .split('/')
        .any(|segment| decode_pointer_segment(segment).is_none())
    {
        return Err(MigrationError::InvalidPointer(path.to_owned()));
    }
    let (parent, encoded_key) = path.rsplit_once('/').expect("leading slash was checked");
    let key = decode_pointer_segment(encoded_key)
        .ok_or_else(|| MigrationError::InvalidPointer(path.to_owned()))?;
    if key.is_empty() {
        return Err(MigrationError::InvalidPointer(path.to_owned()));
    }
    let parent = if parent.is_empty() {
        String::new()
    } else {
        parent.to_owned()
    };
    Ok((parent, key))
}

fn decode_pointer_segment(segment: &str) -> Option<String> {
    let mut decoded = String::with_capacity(segment.len());
    let mut chars = segment.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        match chars.next()? {
            '0' => decoded.push('~'),
            '1' => decoded.push('/'),
            _ => return None,
        }
    }
    Some(decoded)
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::{
        CatalogMigration, ContentMigrationPlan, DefinitionMigration, MigrationError,
        MigrationOperation, apply_operations,
    };
    use engine_core::i18n::Locale;
    use engine_core::id::DefinitionId;
    use engine_core::schema::SchemaVersion;
    use serde_json::{Value, json};

    #[test]
    fn operations_replace_remove_and_rename_fields() {
        let mut value = json!({
            "components": {
                "core.interactable": { "range_m": 2.5, "legacy": true }
            }
        });
        apply_operations(
            &mut value,
            &[
                MigrationOperation::Replace {
                    path: "/components/core.interactable/range_m".into(),
                    expected: json!(2.5),
                    value: json!(3.0),
                },
                MigrationOperation::Rename {
                    from: "/components/core.interactable/legacy".into(),
                    to: "/components/core.interactable/enabled".into(),
                    expected: json!(true),
                },
                MigrationOperation::Remove {
                    path: "/components/core.interactable/enabled".into(),
                    expected: json!(true),
                },
            ],
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"components":{"core.interactable":{"range_m":3.0}}})
        );
    }

    #[test]
    fn failed_sequence_does_not_change_the_input() {
        let mut value = json!({"display":{"name":"item.old"}});
        let before = value.clone();
        let result = apply_operations(
            &mut value,
            &[
                MigrationOperation::Replace {
                    path: "/display/name".into(),
                    expected: json!("item.old"),
                    value: json!("item.new"),
                },
                MigrationOperation::Remove {
                    path: "/display/missing".into(),
                    expected: Value::Null,
                },
            ],
        );
        assert_eq!(
            result,
            Err(MigrationError::MissingField("/display/missing".into()))
        );
        assert_eq!(value, before);
    }

    #[test]
    fn stale_values_and_colliding_renames_are_rejected() {
        let mut stale = json!({"field":"new"});
        assert!(matches!(
            apply_operations(
                &mut stale,
                &[MigrationOperation::Remove {
                    path: "/field".into(),
                    expected: json!("old"),
                }]
            ),
            Err(MigrationError::UnexpectedValue(_))
        ));

        let mut collision = json!({"old":1,"new":2});
        assert!(matches!(
            apply_operations(
                &mut collision,
                &[MigrationOperation::Rename {
                    from: "/old".into(),
                    to: "/new".into(),
                    expected: json!(1),
                }]
            ),
            Err(MigrationError::DestinationExists(_))
        ));
        assert_eq!(collision, json!({"old":1,"new":2}));
    }

    #[test]
    fn pointer_escaping_and_malformed_pointers_are_handled() {
        let mut value = json!({"a/b":{"tilde~key":1}});
        apply_operations(
            &mut value,
            &[MigrationOperation::Replace {
                path: "/a~1b/tilde~0key".into(),
                expected: json!(1),
                value: json!(2),
            }],
        )
        .unwrap();
        assert_eq!(value["a/b"]["tilde~key"], 2);

        assert!(matches!(
            apply_operations(
                &mut value,
                &[MigrationOperation::Remove {
                    path: "/a~2b".into(),
                    expected: json!(2),
                }]
            ),
            Err(MigrationError::InvalidPointer(_))
        ));
    }

    #[test]
    fn migration_plan_requires_new_versions_unique_targets_and_scoped_paths() {
        let definition = DefinitionMigration {
            definition: DefinitionId::new("item.torch").unwrap(),
            from_version: SchemaVersion::new(1, 0),
            to_version: SchemaVersion::new(1, 1),
            operations: vec![MigrationOperation::Replace {
                path: "/display/name".into(),
                expected: json!("item.old"),
                value: json!("item.new"),
            }],
        };
        let catalog = CatalogMigration {
            locale: Locale::parse("pt-BR").unwrap(),
            operations: vec![MigrationOperation::Rename {
                from: "/messages/item.old".into(),
                to: "/messages/item.new".into(),
                expected: json!("Tocha"),
            }],
        };
        let plan = ContentMigrationPlan {
            id: "rename-item-message".into(),
            source_manifest_sha256: "a".repeat(64),
            from_content_version: semver::Version::new(1, 0, 0),
            to_content_version: semver::Version::new(1, 1, 0),
            definitions: vec![definition.clone()],
            catalogs: vec![catalog],
        };
        assert!(plan.validate().is_ok());

        let mut invalid = plan.clone();
        invalid.definitions.push(definition);
        assert!(matches!(
            invalid.validate(),
            Err(MigrationError::InvalidPlan(message)) if message.contains("duplicate definition")
        ));

        let mut protected = plan;
        protected.definitions[0].operations[0] = MigrationOperation::Replace {
            path: "/version".into(),
            expected: json!({"major":1,"minor":0}),
            value: json!({"major":1,"minor":1}),
        };
        assert!(matches!(
            protected.validate(),
            Err(MigrationError::InvalidPlan(message)) if message.contains("controlled by the migration step")
        ));
    }
}
