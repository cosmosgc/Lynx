//! Authored content definitions.
//!
//! Definitions are immutable for a content version. Player-facing text is
//! stored as message keys and resolved through a locale catalog; Brazilian
//! Portuguese is the source locale.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::i18n::MessageKey;
use engine_core::id::DefinitionId;
use engine_core::schema::{ComponentData, ComponentKey, SchemaVersion, ValidationIssue};
use serde::{Deserialize, Serialize};

/// The maximum accepted tag length.
pub const MAX_TAG_LEN: usize = 32;

/// The kind of definition, inferred from its ID namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// An item definition.
    Item,
    /// An entity or NPC definition.
    Entity,
    /// A zone definition.
    Zone,
    /// A behavior graph definition.
    Behavior,
    /// A UI panel definition.
    Ui,
}

impl ContentKind {
    /// The wire and manifest name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Entity => "entity",
            Self::Zone => "zone",
            Self::Behavior => "behavior",
            Self::Ui => "ui",
        }
    }

    /// Infer the kind from the ID's first namespace segment.
    #[must_use]
    pub fn infer(id: &DefinitionId) -> Self {
        match id.namespace() {
            Some("item") => Self::Item,
            Some("zone") => Self::Zone,
            Some("behavior" | "ai") => Self::Behavior,
            Some("ui" | "panel") => Self::Ui,
            _ => Self::Entity,
        }
    }
}

impl std::fmt::Display for ContentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Localized display text, stored as message keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayText {
    /// Localized name key.
    pub name: MessageKey,
    /// Localized description key.
    pub description: MessageKey,
}

/// How a definition is presented by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresentationMode {
    /// A rigged 3D model.
    Rigged3d,
    /// A camera-facing 2D sprite.
    Billboard2d,
    /// A 3D model with 2D embellishment.
    Hybrid,
    /// A symbolic or icon-only representation.
    IconAbstract,
}

/// Presentation assets and mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presentation {
    /// Rendering mode.
    pub mode: PresentationMode,
    /// 3D model path, relative to the content root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Portrait or sprite path, relative to the content root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portrait: Option<String>,
    /// Animation set path, relative to the content root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation_set: Option<String>,
}

impl Presentation {
    /// Check that the mode has the assets it requires and that all paths are
    /// safe relative paths.
    #[must_use]
    pub fn validate(&self, path: &str) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        match self.mode {
            PresentationMode::Rigged3d if self.model.is_none() => {
                issues.push(ValidationIssue::new(
                    "content.missing_asset",
                    format!("{path}.model"),
                    "rigged 3D presentation requires a model",
                ));
            }
            PresentationMode::Billboard2d if self.portrait.is_none() => {
                issues.push(ValidationIssue::new(
                    "content.missing_asset",
                    format!("{path}.portrait"),
                    "billboard presentation requires a portrait",
                ));
            }
            PresentationMode::Hybrid => {
                if self.model.is_none() {
                    issues.push(ValidationIssue::new(
                        "content.missing_asset",
                        format!("{path}.model"),
                        "hybrid presentation requires a model",
                    ));
                }
                if self.portrait.is_none() {
                    issues.push(ValidationIssue::new(
                        "content.missing_asset",
                        format!("{path}.portrait"),
                        "hybrid presentation requires a portrait",
                    ));
                }
            }
            _ => {}
        }

        for (field, value) in [
            ("model", self.model.as_deref()),
            ("portrait", self.portrait.as_deref()),
            ("animation_set", self.animation_set.as_deref()),
        ] {
            if let Some(value) = value
                && !is_safe_asset_path(value)
            {
                issues.push(ValidationIssue::new(
                    "content.invalid_asset_path",
                    format!("{path}.{field}"),
                    "asset path must be relative and must not contain '..'",
                ));
            }
        }
        issues
    }
}

/// An authored content definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentDefinition {
    /// Stable definition key, for example `door.wooden`.
    pub id: DefinitionId,
    /// Definition version. A major change requires a content migration.
    pub version: SchemaVersion,
    /// Localized name and description keys.
    pub display: DisplayText,
    /// Component data keyed by component type.
    #[serde(default)]
    pub components: BTreeMap<ComponentKey, ComponentData>,
    /// Optional presentation assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Presentation>,
    /// Free-form authoring tags.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<String>,
}

impl ContentDefinition {
    /// The inferred definition kind.
    #[must_use]
    pub fn kind(&self) -> ContentKind {
        ContentKind::infer(&self.id)
    }

    /// Validate structure that does not depend on the component registry.
    ///
    /// Cross-definition references and catalog completeness are checked by
    /// [`crate::validate::ContentSet`].
    #[must_use]
    pub fn validate_shape(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.version.major == 0 {
            issues.push(ValidationIssue::new(
                "content.invalid_version",
                "version.major",
                "content versions must have a major version of at least one",
            ));
        }
        if self.display.name == self.display.description {
            issues.push(ValidationIssue::new(
                "content.duplicate_display_key",
                "display",
                "name and description must use different message keys",
            ));
        }
        for (field, key) in [
            ("name", &self.display.name),
            ("description", &self.display.description),
        ] {
            if !key.as_str().contains('.') {
                issues.push(ValidationIssue::new(
                    "i18n.invalid_key",
                    format!("display.{field}"),
                    "message keys must be namespaced, for example entity.door.name",
                ));
            }
        }
        if self.components.is_empty() {
            issues.push(ValidationIssue::new(
                "content.empty_components",
                "components",
                "definition declares no components",
            ));
        }
        if let Some(presentation) = &self.presentation {
            issues.extend(presentation.validate("presentation"));
        }
        for tag in &self.tags {
            if !is_valid_tag(tag) {
                issues.push(ValidationIssue::new(
                    "content.invalid_tag",
                    format!("tags.{tag}"),
                    "tags must be lowercase alphanumeric with '_' or '-', up to 32 characters",
                ));
            }
        }
        issues
    }
}

fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_TAG_LEN
        && tag
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// Whether an asset path is relative, uses forward slashes, and cannot escape
/// the bundle root.
#[must_use]
pub fn is_safe_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::i18n::MessageKey;

    fn key(value: &str) -> MessageKey {
        MessageKey::parse(value).unwrap()
    }

    fn definition(mode: PresentationMode) -> ContentDefinition {
        ContentDefinition {
            id: DefinitionId::new("door.wooden").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("entity.door.wooden.name"),
                description: key("entity.door.wooden.description"),
            },
            components: BTreeMap::new(),
            presentation: Some(Presentation {
                mode,
                model: None,
                portrait: None,
                animation_set: None,
            }),
            tags: BTreeSet::new(),
        }
    }

    #[test]
    fn kinds_are_inferred_from_namespaces() {
        for (id, expected) in [
            ("item.torch", ContentKind::Item),
            ("door.wooden", ContentKind::Entity),
            ("zone.tavern", ContentKind::Zone),
            ("behavior.guard", ContentKind::Behavior),
            ("ui.inventory", ContentKind::Ui),
        ] {
            assert_eq!(
                ContentKind::infer(&DefinitionId::new(id).unwrap()),
                expected
            );
        }
    }

    #[test]
    fn presentation_requires_assets_for_its_mode() {
        let issues = definition(PresentationMode::Rigged3d).validate_shape();
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "content.missing_asset")
        );

        let mut definition = definition(PresentationMode::IconAbstract);
        definition.components.insert(
            ComponentKey::new("core.interactable").unwrap(),
            ComponentData::new(),
        );
        let issues = definition.validate_shape();
        assert_eq!(issues, Vec::new());
    }

    #[test]
    fn unsafe_asset_paths_are_rejected() {
        let mut definition = definition(PresentationMode::Rigged3d);
        definition.presentation = Some(Presentation {
            mode: PresentationMode::Rigged3d,
            model: Some("../outside.glb".into()),
            portrait: None,
            animation_set: None,
        });
        assert!(
            definition
                .validate_shape()
                .iter()
                .any(|issue| issue.code == "content.invalid_asset_path")
        );
        assert!(!is_safe_asset_path("C:/models/door.glb"));
        assert!(is_safe_asset_path("models/door.glb"));
    }

    #[test]
    fn invalid_versions_and_tags_are_reported() {
        let mut definition = definition(PresentationMode::IconAbstract);
        definition.version = SchemaVersion::new(0, 1);
        definition.tags.insert("Not A Tag".into());
        let codes: Vec<_> = definition
            .validate_shape()
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"content.invalid_version".to_string()));
        assert!(codes.contains(&"content.invalid_tag".to_string()));
        assert!(codes.contains(&"content.empty_components".to_string()));
    }
}
