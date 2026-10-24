//! Loading and cross-file validation for content sets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use engine_core::i18n::{
    CatalogSet, DEFAULT_LOCALE, Locale, MessageCatalog, MessageKey, validate_placeholders,
};
use engine_core::id::DefinitionId;
use engine_core::schema::{ComponentInstance, ComponentRegistry, ComponentSchema, FieldType};
use engine_core::{EngineError, ErrorCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::definition::{ContentDefinition, MinigameDefinition, UiPanelDefinition};
use crate::diagnostic::{Diagnostic, ValidationReport};

/// Directories scanned for component-backed definition files, relative to a
/// content root.
pub const DEFINITION_DIRS: [&str; 5] = ["definitions", "items", "entities", "zones", "behaviors"];

/// Directories scanned for declarative UI panel definitions.
pub const PANEL_DIRS: [&str; 2] = ["ui", "panels"];

/// Directories scanned for minigame definitions.
pub const MINIGAME_DIRS: [&str; 1] = ["minigames"];

/// Maximum directory nesting when scanning content.
const MAX_SCAN_DEPTH: usize = 32;

/// Definitions and catalogs loaded from a project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContentSet {
    /// Component-backed definitions keyed by ID.
    pub definitions: BTreeMap<DefinitionId, ContentDefinition>,
    /// Declarative UI panels keyed by ID.
    pub panels: BTreeMap<DefinitionId, UiPanelDefinition>,
    /// Minigames keyed by ID.
    pub minigames: BTreeMap<DefinitionId, MinigameDefinition>,
    /// Localization catalogs.
    pub catalogs: CatalogSet,
    /// Source path for each component-backed definition.
    pub sources: BTreeMap<DefinitionId, String>,
    /// Source path for each UI panel.
    pub panel_sources: BTreeMap<DefinitionId, String>,
    /// Source path for each minigame.
    pub minigame_sources: BTreeMap<DefinitionId, String>,
    /// Source path for each catalog.
    pub catalog_paths: BTreeMap<Locale, String>,
}

impl ContentSet {
    /// Create an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a definition with its relative source path.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate ID or path.
    pub fn insert_definition(
        &mut self,
        path: impl Into<String>,
        definition: ContentDefinition,
    ) -> Result<(), EngineError> {
        let path = path.into();
        self.check_unique(&definition.id, &path)?;
        self.sources.insert(definition.id.clone(), path.clone());
        self.definitions.insert(definition.id.clone(), definition);
        Ok(())
    }

    /// Add a UI panel with its relative source path.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate ID or path.
    pub fn insert_panel(
        &mut self,
        path: impl Into<String>,
        panel: UiPanelDefinition,
    ) -> Result<(), EngineError> {
        let path = path.into();
        self.check_unique(&panel.id, &path)?;
        self.panel_sources.insert(panel.id.clone(), path.clone());
        self.panels.insert(panel.id.clone(), panel);
        Ok(())
    }

    /// Add a minigame with its relative source path.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate ID or path.
    pub fn insert_minigame(
        &mut self,
        path: impl Into<String>,
        minigame: MinigameDefinition,
    ) -> Result<(), EngineError> {
        let path = path.into();
        self.check_unique(&minigame.id, &path)?;
        self.minigame_sources
            .insert(minigame.id.clone(), path.clone());
        self.minigames.insert(minigame.id.clone(), minigame);
        Ok(())
    }

    fn check_unique(&self, id: &DefinitionId, path: &str) -> Result<(), EngineError> {
        if self.definitions.contains_key(id)
            || self.panels.contains_key(id)
            || self.minigames.contains_key(id)
        {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate definition id")
                    .with_detail("id", id.to_string())
                    .with_detail("path", path),
            );
        }
        if self
            .sources
            .values()
            .chain(self.panel_sources.values())
            .chain(self.minigame_sources.values())
            .any(|existing| existing == path)
        {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate definition path")
                    .with_detail("path", path),
            );
        }
        Ok(())
    }

    /// Add a localization catalog with its relative source path.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] for a duplicate locale or path.
    pub fn insert_catalog(
        &mut self,
        path: impl Into<String>,
        catalog: MessageCatalog,
    ) -> Result<(), EngineError> {
        let path = path.into();
        if self
            .catalog_paths
            .values()
            .any(|existing| existing == &path)
        {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate catalog path")
                    .with_detail("path", &path),
            );
        }
        let locale = catalog.locale().clone();
        if self.catalog_paths.contains_key(&locale) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate locale catalog")
                    .with_detail("locale", locale.to_string())
                    .with_detail("path", &path),
            );
        }
        self.catalog_paths.insert(locale, path);
        self.catalogs.insert(catalog)
    }

    /// Validate definitions, references, and localization.
    ///
    /// Missing translations outside the `pt-BR` source catalog are warnings.
    #[must_use]
    pub fn validate(&self, registry: &ComponentRegistry) -> ValidationReport {
        self.validate_inner(registry, false)
    }

    /// Validate as a release bundle: missing translations in any declared
    /// locale are errors.
    #[must_use]
    pub fn validate_release(&self, registry: &ComponentRegistry) -> ValidationReport {
        self.validate_inner(registry, true)
    }

    fn validate_inner(
        &self,
        registry: &ComponentRegistry,
        strict_locales: bool,
    ) -> ValidationReport {
        let mut report = ValidationReport::new();
        for (id, definition) in &self.definitions {
            let path = self.sources.get(id).map_or("", String::as_str);
            self.validate_definition(&mut report, registry, definition, path);
        }
        for (id, panel) in &self.panels {
            let path = self.panel_sources.get(id).map_or("", String::as_str);
            for issue in panel.validate_shape() {
                report.push(Diagnostic::from_issue(path, issue));
            }
        }
        for (id, minigame) in &self.minigames {
            let path = self.minigame_sources.get(id).map_or("", String::as_str);
            for issue in minigame.validate_shape() {
                report.push(Diagnostic::from_issue(path, issue));
            }
            for (index, reward) in minigame.rewards.iter().enumerate() {
                if !self.definitions.contains_key(reward) {
                    report.error(
                        "content.unknown_reference",
                        format!("{path}:rewards[{index}]"),
                        format!("reward {reward} from {} does not resolve", minigame.id),
                    );
                }
            }
        }
        self.validate_localization(&mut report, strict_locales);
        report
    }

    fn validate_definition(
        &self,
        report: &mut ValidationReport,
        registry: &ComponentRegistry,
        definition: &ContentDefinition,
        path: &str,
    ) {
        for issue in definition.validate_shape() {
            report.push(Diagnostic::from_issue(path, issue));
        }
        for (key, data) in &definition.components {
            let Some(schema) = registry.get(key) else {
                report.error(
                    "content.unknown_component",
                    format!("{path}:components.{key}"),
                    format!("component {key} is not registered"),
                );
                continue;
            };
            let instance = ComponentInstance::new(key.clone(), schema.version, data.clone());
            match registry.prepare(&instance) {
                Ok(prepared) => {
                    for issue in prepared.issues {
                        report.push(Diagnostic::from_issue(path, issue));
                    }
                }
                Err(error) => {
                    report.error(
                        "content.invalid_component",
                        format!("{path}:components.{key}"),
                        error.to_string(),
                    );
                }
            }
            self.validate_references(report, definition, key.as_str(), schema, data, path);
        }
    }

    fn validate_references(
        &self,
        report: &mut ValidationReport,
        definition: &ContentDefinition,
        component: &str,
        schema: &ComponentSchema,
        data: &serde_json::Map<String, Value>,
        path: &str,
    ) {
        for field in &schema.fields {
            let Some(value) = data.get(&field.name) else {
                continue;
            };
            let mut references = Vec::new();
            collect_definition_refs(
                &field.ty,
                value,
                &format!("components.{component}.{}", field.name),
                &mut references,
            );
            for (field_path, reference) in references {
                let Ok(reference_id) = DefinitionId::new(reference.clone()) else {
                    continue;
                };
                if self.definitions.contains_key(&reference_id) {
                    continue;
                }
                report.error(
                    "content.unknown_reference",
                    format!("{path}:{field_path}"),
                    format!(
                        "reference {reference_id} from {} does not resolve",
                        definition.id
                    ),
                );
            }
        }
    }

    fn validate_localization(&self, report: &mut ValidationReport, strict_locales: bool) {
        let source_locale = Locale::default();
        let Some(source) = self.catalogs.catalog(&source_locale) else {
            report.error(
                "i18n.missing_source_catalog",
                "localization",
                format!("the source catalog ({DEFAULT_LOCALE}) is missing"),
            );
            return;
        };

        let used = self.used_message_keys();
        let mut used_keys = BTreeSet::new();
        for (key, key_path) in &used {
            used_keys.insert(key.clone());
            if source.get(key).is_none() {
                report.error(
                    "i18n.missing_key",
                    key_path,
                    format!("key {key} is missing from the {DEFAULT_LOCALE} catalog"),
                );
            }
        }

        for (locale, catalog) in self.catalogs.iter() {
            if locale == &source_locale {
                continue;
            }
            self.validate_catalog(report, source, locale, catalog, &used, strict_locales);
        }

        for (key, _) in source.iter() {
            if !used_keys.contains(key) {
                report.warning(
                    "i18n.unused_key",
                    self.catalog_paths
                        .get(&source_locale)
                        .map_or("localization", String::as_str),
                    format!("key {key} is not referenced by any definition"),
                );
            }
        }
    }

    fn used_message_keys(&self) -> Vec<(MessageKey, String)> {
        let mut used = Vec::new();
        for (id, definition) in &self.definitions {
            let path = self.sources.get(id).map_or("", String::as_str);
            used.push((
                definition.display.name.clone(),
                format!("{path}:display.name"),
            ));
            used.push((
                definition.display.description.clone(),
                format!("{path}:display.description"),
            ));
        }
        for (id, panel) in &self.panels {
            let path = self.panel_sources.get(id).map_or("", String::as_str);
            used.push((panel.display.name.clone(), format!("{path}:display.name")));
            used.push((
                panel.display.description.clone(),
                format!("{path}:display.description"),
            ));
            for (index, action) in panel.actions.iter().enumerate() {
                used.push((
                    action.label.clone(),
                    format!("{path}:actions[{index}].label"),
                ));
            }
        }
        for (id, minigame) in &self.minigames {
            let path = self.minigame_sources.get(id).map_or("", String::as_str);
            used.push((
                minigame.display.name.clone(),
                format!("{path}:display.name"),
            ));
            used.push((
                minigame.display.description.clone(),
                format!("{path}:display.description"),
            ));
        }
        used
    }

    fn validate_catalog(
        &self,
        report: &mut ValidationReport,
        source: &MessageCatalog,
        locale: &Locale,
        catalog: &MessageCatalog,
        used: &[(MessageKey, String)],
        strict_locales: bool,
    ) {
        let path = self
            .catalog_paths
            .get(locale)
            .map_or("localization", String::as_str);
        for (key, _key_path) in used {
            let Some(source_text) = source.get(key) else {
                continue;
            };
            let Some(translation) = catalog.get(key) else {
                let code = "i18n.missing_translation";
                if strict_locales {
                    report.error(
                        code,
                        format!("{path}:{key}"),
                        format!("{locale} is missing a translation for {key}"),
                    );
                } else {
                    report.warning(
                        code,
                        format!("{path}:{key}"),
                        format!("{locale} is missing a translation for {key}"),
                    );
                }
                continue;
            };
            if let Err(error) = validate_placeholders(source_text, translation) {
                report.error(
                    "i18n.placeholder_mismatch",
                    format!("{path}:{key}"),
                    error.to_string(),
                );
            }
        }
    }

    /// Keys referenced by display fields, without diagnostics.
    #[must_use]
    pub fn referenced_keys(&self) -> BTreeSet<MessageKey> {
        let mut keys = BTreeSet::new();
        for definition in self.definitions.values() {
            keys.insert(definition.display.name.clone());
            keys.insert(definition.display.description.clone());
        }
        for panel in self.panels.values() {
            keys.insert(panel.display.name.clone());
            keys.insert(panel.display.description.clone());
            for action in &panel.actions {
                keys.insert(action.label.clone());
            }
        }
        for minigame in self.minigames.values() {
            keys.insert(minigame.display.name.clone());
            keys.insert(minigame.display.description.clone());
        }
        keys
    }
}

fn collect_definition_refs(
    ty: &FieldType,
    value: &Value,
    path: &str,
    out: &mut Vec<(String, String)>,
) {
    match ty {
        FieldType::DefinitionRef { .. } => {
            if let Some(text) = value.as_str() {
                out.push((path.to_string(), text.to_string()));
            }
        }
        FieldType::List { item, .. } => {
            if let Some(items) = value.as_array() {
                for (index, element) in items.iter().enumerate() {
                    collect_definition_refs(item, element, &format!("{path}[{index}]"), out);
                }
            }
        }
        FieldType::Map { value: item, .. } => {
            if let Some(map) = value.as_object() {
                for (key, element) in map {
                    collect_definition_refs(item, element, &format!("{path}.{key}"), out);
                }
            }
        }
        _ => {}
    }
}

/// Loads definitions and catalogs from a content root and validates them.
#[derive(Debug, Clone)]
pub struct ContentLoader {
    registry: ComponentRegistry,
}

impl ContentLoader {
    /// Create a loader with a component registry.
    #[must_use]
    pub fn new(registry: ComponentRegistry) -> Self {
        Self { registry }
    }

    /// The registry used for component validation.
    #[must_use]
    pub fn registry(&self) -> &ComponentRegistry {
        &self.registry
    }

    /// Load every known definition directory and the localization catalogs,
    /// then validate the resulting set.
    #[must_use]
    pub fn load_project(&self, root: impl AsRef<Path>) -> (ContentSet, ValidationReport) {
        let root = root.as_ref();
        let mut set = ContentSet::new();
        let mut report = ValidationReport::new();
        if !root.is_dir() {
            report.error(
                "content.io",
                root.display().to_string(),
                "content root is not a directory",
            );
            return (set, report);
        }

        Self::load_catalogs(root, &mut set, &mut report);
        Self::load_definitions(root, &mut set, &mut report);
        Self::load_panels(root, &mut set, &mut report);
        Self::load_minigames(root, &mut set, &mut report);

        let validation = set.validate(&self.registry);
        report.extend(validation);
        (set, report)
    }

    fn load_catalogs(root: &Path, set: &mut ContentSet, report: &mut ValidationReport) {
        let directory = root.join("localization");
        if !directory.is_dir() {
            report.error(
                "i18n.missing_source_catalog",
                "localization",
                "localization directory is missing",
            );
            return;
        }
        let mut files = Vec::new();
        collect_ron_files(&directory, &mut files, 0);
        for path in files {
            let relative = relative_path(root, &path);
            match read_ron::<MessageCatalog>(&path) {
                Ok(catalog) => {
                    if let Err(error) = set.insert_catalog(relative.clone(), catalog) {
                        let code = if error.details().contains_key("locale") {
                            "content.duplicate_locale"
                        } else {
                            "content.duplicate_path"
                        };
                        report.error(code, relative, error.to_string());
                    }
                }
                Err(error) => report.error("content.parse", relative, error.to_string()),
            }
        }
    }

    fn load_definitions(root: &Path, set: &mut ContentSet, report: &mut ValidationReport) {
        for path in collect_files(root, &DEFINITION_DIRS) {
            let relative = relative_path(root, &path);
            match read_ron::<ContentDefinition>(&path) {
                Ok(definition) => {
                    if let Err(error) = set.insert_definition(relative.clone(), definition) {
                        let code = if error.details().contains_key("id") {
                            "content.duplicate_id"
                        } else {
                            "content.duplicate_path"
                        };
                        report.error(code, relative, error.to_string());
                    }
                }
                Err(error) => report.error("content.parse", relative, error.to_string()),
            }
        }
    }

    fn load_panels(root: &Path, set: &mut ContentSet, report: &mut ValidationReport) {
        for path in collect_files(root, &PANEL_DIRS) {
            let relative = relative_path(root, &path);
            match read_ron::<UiPanelDefinition>(&path) {
                Ok(panel) => {
                    if let Err(error) = set.insert_panel(relative.clone(), panel) {
                        let code = if error.details().contains_key("id") {
                            "content.duplicate_id"
                        } else {
                            "content.duplicate_path"
                        };
                        report.error(code, relative, error.to_string());
                    }
                }
                Err(error) => report.error("content.parse", relative, error.to_string()),
            }
        }
    }

    fn load_minigames(root: &Path, set: &mut ContentSet, report: &mut ValidationReport) {
        for path in collect_files(root, &MINIGAME_DIRS) {
            let relative = relative_path(root, &path);
            match read_ron::<MinigameDefinition>(&path) {
                Ok(minigame) => {
                    if let Err(error) = set.insert_minigame(relative.clone(), minigame) {
                        let code = if error.details().contains_key("id") {
                            "content.duplicate_id"
                        } else {
                            "content.duplicate_path"
                        };
                        report.error(code, relative, error.to_string());
                    }
                }
                Err(error) => report.error("content.parse", relative, error.to_string()),
            }
        }
    }
}

fn collect_files(root: &Path, directories: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for directory in directories {
        let path = root.join(directory);
        if path.is_dir() {
            collect_ron_files(&path, &mut files, 0);
        }
    }
    files.sort();
    files.dedup();
    files
}

fn read_ron<T: DeserializeOwned>(path: &Path) -> Result<T, EngineError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        EngineError::new(ErrorCode::ContentInvalid, "failed to read content file")
            .with_detail("path", path.display().to_string())
            .with_detail("error", error.to_string())
    })?;
    ron::from_str(&text).map_err(|error| {
        EngineError::new(ErrorCode::ContentInvalid, "failed to parse content file")
            .with_detail("path", path.display().to_string())
            .with_detail("error", error.to_string())
    })
}

fn collect_ron_files(directory: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            let hidden = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'));
            if !hidden {
                collect_ron_files(&path, out, depth + 1);
            }
        } else if path.extension().is_some_and(|extension| extension == "ron") {
            out.push(path);
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::i18n::MessageKey;
    use engine_core::schema::{ComponentData, ComponentKey, FieldSchema, SchemaVersion};
    use std::collections::BTreeMap;

    fn key(value: &str) -> MessageKey {
        MessageKey::parse(value).unwrap()
    }

    fn catalog(locale: &str, messages: &[(&str, &str)]) -> MessageCatalog {
        let mut catalog = MessageCatalog::new(Locale::parse(locale).unwrap());
        for (key, text) in messages {
            catalog.insert(self::key(key), *text).unwrap();
        }
        catalog
    }

    fn registry() -> ComponentRegistry {
        let mut registry = ComponentRegistry::new();
        let schema = engine_core::schema::ComponentSchema::new(
            ComponentKey::new("core.door").unwrap(),
            SchemaVersion::new(1, 0),
            vec![
                FieldSchema::optional(
                    "key",
                    FieldType::DefinitionRef {
                        namespace: Some("item".into()),
                    },
                ),
                FieldSchema::optional("locked", FieldType::Bool),
            ],
            engine_core::schema::Visibility::Client,
            engine_core::schema::PersistencePolicy::StaticDefinition,
        );
        registry.register(schema).unwrap();
        registry
    }

    fn definition(id: &str, key_ref: Option<&str>) -> ContentDefinition {
        let mut components = BTreeMap::new();
        let mut data = ComponentData::new();
        if let Some(key_ref) = key_ref {
            data.insert("key".into(), Value::String(key_ref.into()));
        }
        components.insert(ComponentKey::new("core.door").unwrap(), data);
        ContentDefinition {
            id: DefinitionId::new(id).unwrap(),
            version: SchemaVersion::new(1, 0),
            display: crate::definition::DisplayText {
                name: key(&format!("entity.{id}.name")),
                description: key(&format!("entity.{id}.description")),
            },
            components,
            presentation: None,
            tags: BTreeSet::new(),
        }
    }

    fn set(definitions: Vec<ContentDefinition>, catalogs: Vec<MessageCatalog>) -> ContentSet {
        let mut set = ContentSet::new();
        for definition in definitions {
            let path = format!("entities/{}.ron", definition.id);
            set.insert_definition(path, definition).unwrap();
        }
        for catalog in catalogs {
            let path = format!("localization/{}.ron", catalog.locale());
            set.insert_catalog(path, catalog).unwrap();
        }
        set
    }

    #[test]
    fn valid_sets_have_no_errors() {
        let set = set(
            vec![
                definition("door.wooden", Some("item.key")),
                definition("item.key", None),
            ],
            vec![
                catalog(
                    "pt-BR",
                    &[
                        ("entity.door.wooden.name", "Porta de madeira"),
                        ("entity.door.wooden.description", "Uma porta."),
                        ("entity.item.key.name", "Chave"),
                        ("entity.item.key.description", "Uma chave."),
                        ("ui.unused", "Não usado"),
                    ],
                ),
                catalog(
                    "en",
                    &[
                        ("entity.door.wooden.name", "Wooden door"),
                        ("entity.door.wooden.description", "A door."),
                    ],
                ),
            ],
        );
        let report = set.validate(&registry());
        let errors: Vec<_> = report.errors().collect();
        assert!(errors.is_empty(), "{report}");
        assert!(report.warnings().any(|item| item.code == "i18n.unused_key"));
        assert!(
            report
                .warnings()
                .any(|item| item.code == "i18n.missing_translation")
        );
    }

    #[test]
    fn missing_source_keys_and_references_are_errors() {
        let set = set(
            vec![definition("door.wooden", Some("item.missing"))],
            vec![catalog(
                "pt-BR",
                &[
                    ("entity.door.wooden.name", "Porta de madeira"),
                    ("entity.door.wooden.description", "Uma porta."),
                ],
            )],
        );
        let report = set.validate(&registry());
        let codes: Vec<_> = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert!(codes.contains(&"content.unknown_reference"));
        assert!(!codes.contains(&"i18n.missing_key"));
    }

    #[test]
    fn missing_display_key_is_reported() {
        let set = set(
            vec![definition("door.wooden", None)],
            vec![catalog("pt-BR", &[("entity.door.wooden.name", "Porta")])],
        );
        let report = set.validate(&registry());
        assert!(
            report
                .errors()
                .any(|diagnostic| diagnostic.code == "i18n.missing_key")
        );
    }

    #[test]
    fn missing_translation_is_a_release_error() {
        let set = set(
            vec![definition("door.wooden", None)],
            vec![
                catalog(
                    "pt-BR",
                    &[
                        ("entity.door.wooden.name", "Porta"),
                        ("entity.door.wooden.description", "Uma porta."),
                    ],
                ),
                catalog("en", &[("entity.door.wooden.name", "Door")]),
            ],
        );
        let registry = registry();
        let development = set.validate(&registry);
        assert!(development.is_ok());
        let release = set.validate_release(&registry);
        assert!(
            release
                .errors()
                .any(|diagnostic| diagnostic.code == "i18n.missing_translation")
        );
    }

    #[test]
    fn placeholder_mismatches_are_errors() {
        let mut door = definition("door.wooden", None);
        door.display.description = key("entity.door.wooden.description");
        let set = set(
            vec![door],
            vec![
                catalog(
                    "pt-BR",
                    &[
                        ("entity.door.wooden.name", "Porta {name}"),
                        ("entity.door.wooden.description", "Uma {kind}."),
                    ],
                ),
                catalog(
                    "en",
                    &[
                        ("entity.door.wooden.name", "Door {label}"),
                        ("entity.door.wooden.description", "A {kind}."),
                    ],
                ),
            ],
        );
        let report = set.validate(&registry());
        assert!(
            report
                .errors()
                .any(|diagnostic| diagnostic.code == "i18n.placeholder_mismatch")
        );
    }

    #[test]
    fn loader_reports_parse_and_duplicate_errors() {
        let root = std::env::temp_dir().join(format!(
            "lynx-content-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("entities")).unwrap();
        std::fs::create_dir_all(root.join("localization")).unwrap();
        std::fs::write(root.join("entities/door.ron"), "this is not ron").unwrap();
        std::fs::write(
            root.join("localization/pt-BR.ron"),
            "(locale: \"pt-BR\", messages: {})",
        )
        .unwrap();

        let loader = ContentLoader::new(registry());
        let (set, report) = loader.load_project(&root);
        assert!(set.definitions.is_empty());
        assert!(report.errors().any(|item| item.code == "content.parse"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loader_reads_declarative_panels_and_minigames() {
        let root = std::env::temp_dir().join(format!(
            "lynx-content-ui-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("ui")).unwrap();
        std::fs::create_dir_all(root.join("minigames")).unwrap();
        std::fs::create_dir_all(root.join("localization")).unwrap();
        std::fs::write(
            root.join("ui/terminal.ron"),
            r#"(
                id: "ui.terminal",
                version: (major: 1, minor: 0),
                display: (name: "ui.terminal.title", description: "ui.terminal.description"),
                schema: "ui.terminal",
                actions: [
                    (name: "close", label: "ui.action.close"),
                ],
                fields: [
                    (key: "status", source: "entity.state.status"),
                ],
            )"#,
        )
        .unwrap();
        std::fs::write(
            root.join("minigames/lockpick.ron"),
            r#"(
                id: "minigame.lockpick",
                version: (major: 1, minor: 0),
                display: (name: "minigame.lockpick.name", description: "minigame.lockpick.description"),
                panel_schema: "ui.lockpick",
                authority: "server",
                rewards: ["item.dust"],
            )"#,
        )
        .unwrap();
        std::fs::write(
            root.join("localization/pt-BR.ron"),
            r#"(
                locale: "pt-BR",
                messages: {
                    "ui.terminal.title": "Terminal",
                    "ui.terminal.description": "Um terminal.",
                    "ui.action.close": "Fechar",
                    "minigame.lockpick.name": "Gazua",
                    "minigame.lockpick.description": "Abra a fechadura.",
                },
            )"#,
        )
        .unwrap();

        let loader = ContentLoader::new(registry());
        let (set, report) = loader.load_project(&root);
        assert_eq!(set.panels.len(), 1);
        assert_eq!(set.minigames.len(), 1);
        let panel_id = DefinitionId::new("ui.terminal").unwrap();
        assert_eq!(
            set.panel_sources.get(&panel_id).map(String::as_str),
            Some("ui/terminal.ron")
        );
        assert!(
            report
                .errors()
                .any(|item| item.code == "content.unknown_reference"),
            "missing reward definitions are reported: {report}"
        );
        assert!(
            set.referenced_keys()
                .iter()
                .any(|key| key.as_str() == "ui.action.close")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
