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
    /// A minigame definition.
    Minigame,
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
            Self::Minigame => "minigame",
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
            Some("minigame") => Self::Minigame,
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

/// An action a client may trigger from a declarative UI panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPanelAction {
    /// Stable action name. Example: `close`, `take`, `submit`.
    pub name: String,
    /// Localized label to render in the interface.
    pub label: MessageKey,
    /// Optional guard that must be true before the action is offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_by: Option<String>,
}

/// A data field bound into a panel body or modal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiFieldBinding {
    /// Stable field key, such as `inventory.count` or `door.state`.
    pub key: String,
    /// Data source path from the entity or context.
    pub source: String,
    /// Optional formatter or display hint, such as `count` or `percentage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatter: Option<String>,
}

/// A declarative UI panel definition. Panels do not execute code; they describe
/// the schema, fields, and permitted actions the client can render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPanelDefinition {
    /// Stable definition key, for example `ui.inventory` or `panel.terminal`.
    pub id: DefinitionId,
    /// Version of the panel schema and content contract.
    pub version: SchemaVersion,
    /// Localized name and description.
    pub display: DisplayText,
    /// Panel schema name. The client resolves this to a renderer.
    pub schema: String,
    /// Actions available in this panel.
    pub actions: Vec<UiPanelAction>,
    /// Fields to display or bind from entity state.
    pub fields: Vec<UiFieldBinding>,
    /// Optional fallback schema when a client lacks native support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_schema: Option<String>,
}

impl UiPanelDefinition {
    /// Validate panel metadata and binding shape.
    #[must_use]
    pub fn validate_shape(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.version.major == 0 {
            issues.push(ValidationIssue::new(
                "content.invalid_version",
                "version.major",
                "panel versions must have a major version of at least one",
            ));
        }
        validate_display(&self.display, &mut issues);
        if !self.id.is_in("ui") && !self.id.is_in("panel") {
            issues.push(ValidationIssue::new(
                "ui.invalid_id",
                "id",
                "panel ids must use the ui or panel namespace",
            ));
        }
        if !is_namespaced(&self.schema) {
            issues.push(ValidationIssue::new(
                "ui.invalid_schema",
                "schema",
                "panel schema must be a namespaced identifier such as ui.inventory",
            ));
        }
        if self.actions.is_empty() && self.fallback_schema.is_none() {
            issues.push(ValidationIssue::new(
                "ui.empty_actions",
                "actions",
                "a panel must expose at least one action or a read-only fallback",
            ));
        }
        if let Some(fallback) = &self.fallback_schema
            && !is_namespaced(fallback)
        {
            issues.push(ValidationIssue::new(
                "ui.invalid_schema",
                "fallback_schema",
                "fallback schemas must be namespaced identifiers",
            ));
        }
        self.validate_actions(&mut issues);
        self.validate_fields(&mut issues);
        issues
    }

    fn validate_actions(&self, issues: &mut Vec<ValidationIssue>) {
        let mut action_names = BTreeSet::new();
        for (index, action) in self.actions.iter().enumerate() {
            if action.name.is_empty() || action.name.contains(char::is_whitespace) {
                issues.push(ValidationIssue::new(
                    "ui.invalid_action_name",
                    format!("actions[{index}].name"),
                    "action names must be compact identifiers without spaces",
                ));
            } else if !action_names.insert(action.name.as_str()) {
                issues.push(ValidationIssue::new(
                    "ui.duplicate_action",
                    format!("actions[{index}].name"),
                    format!("action {} is declared more than once", action.name),
                ));
            }
            if !is_namespaced(action.label.as_str()) {
                issues.push(ValidationIssue::new(
                    "i18n.invalid_key",
                    format!("actions[{index}].label"),
                    "action labels must be namespaced message keys",
                ));
            }
            if action
                .gated_by
                .as_deref()
                .is_some_and(|gate| gate.trim().is_empty())
            {
                issues.push(ValidationIssue::new(
                    "ui.invalid_gate",
                    format!("actions[{index}].gated_by"),
                    "gated_by must name a condition when present",
                ));
            }
        }
    }

    fn validate_fields(&self, issues: &mut Vec<ValidationIssue>) {
        let mut field_keys = BTreeSet::new();
        for (index, field) in self.fields.iter().enumerate() {
            if field.key.is_empty() || field.key.contains(char::is_whitespace) {
                issues.push(ValidationIssue::new(
                    "ui.invalid_field_key",
                    format!("fields[{index}].key"),
                    "field keys must be compact identifiers without spaces",
                ));
            } else if !field_keys.insert(field.key.as_str()) {
                issues.push(ValidationIssue::new(
                    "ui.duplicate_field",
                    format!("fields[{index}].key"),
                    format!("field {} is declared more than once", field.key),
                ));
            }
            if field.source.trim().is_empty() {
                issues.push(ValidationIssue::new(
                    "ui.invalid_field_source",
                    format!("fields[{index}].source"),
                    "field sources must refer to a bound value or state path",
                ));
            }
            if let Some(formatter) = &field.formatter
                && formatter.trim().is_empty()
            {
                issues.push(ValidationIssue::new(
                    "ui.invalid_formatter",
                    format!("fields[{index}].formatter"),
                    "formatter hints must not be blank",
                ));
            }
        }
    }
}

/// A server-authored minigame or challenge with explicit authority and rewards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinigameDefinition {
    /// Stable definition key, for example `minigame.lockpick`.
    pub id: DefinitionId,
    /// Version of the game rules and reward rules.
    pub version: SchemaVersion,
    /// Localized name and description.
    pub display: DisplayText,
    /// Panel schema the client opens to play the minigame.
    pub panel_schema: String,
    /// Where authority for result resolution lives.
    pub authority: String,
    /// Optional reward definitions granted by a successful outcome.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rewards: Vec<DefinitionId>,
}

impl MinigameDefinition {
    /// Validate minigame metadata and rules with a small but explicit contract.
    #[must_use]
    pub fn validate_shape(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.version.major == 0 {
            issues.push(ValidationIssue::new(
                "content.invalid_version",
                "version.major",
                "minigame versions must have a major version of at least one",
            ));
        }
        validate_display(&self.display, &mut issues);
        if !self.id.is_in("minigame") {
            issues.push(ValidationIssue::new(
                "minigame.invalid_id",
                "id",
                "minigame ids must use the minigame namespace",
            ));
        }
        if !is_namespaced(&self.panel_schema) {
            issues.push(ValidationIssue::new(
                "ui.invalid_schema",
                "panel_schema",
                "minigame panels must use a namespaced schema id",
            ));
        }
        if !matches!(self.authority.as_str(), "server" | "hybrid" | "client") {
            issues.push(ValidationIssue::new(
                "minigame.invalid_authority",
                "authority",
                "authority must be server, hybrid, or client",
            ));
        }
        if self.authority == "client" && !self.rewards.is_empty() {
            issues.push(ValidationIssue::new(
                "minigame.client_authority_rewards",
                "rewards",
                "client-authoritative minigames must not grant rewards",
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, reward) in self.rewards.iter().enumerate() {
            if reward.namespace().is_none() {
                issues.push(ValidationIssue::new(
                    "content.invalid_reference",
                    format!("rewards[{index}]"),
                    "reward references must include a namespace",
                ));
            }
            if !seen.insert(reward.as_str()) {
                issues.push(ValidationIssue::new(
                    "content.duplicate_reference",
                    format!("rewards[{index}]"),
                    format!("reward {reward} is declared more than once"),
                ));
            }
        }
        issues
    }
}

/// Shared display-key validation for non-component definitions.
fn validate_display(display: &DisplayText, issues: &mut Vec<ValidationIssue>) {
    if display.name == display.description {
        issues.push(ValidationIssue::new(
            "content.duplicate_display_key",
            "display",
            "name and description must use different message keys",
        ));
    }
    for (field, key) in [
        ("name", &display.name),
        ("description", &display.description),
    ] {
        if !is_namespaced(key.as_str()) {
            issues.push(ValidationIssue::new(
                "i18n.invalid_key",
                format!("display.{field}"),
                "message keys must be namespaced, for example ui.inventory.title",
            ));
        }
    }
}

/// Whether a value is a dot-separated, whitespace-free identifier with at least
/// two non-empty segments.
fn is_namespaced(value: &str) -> bool {
    let mut segments = value.split('.');
    let first = segments.next().unwrap_or_default();
    let second = segments.next().unwrap_or_default();
    !first.is_empty()
        && !second.is_empty()
        && !value.chars().any(char::is_whitespace)
        && segments.all(|segment| !segment.is_empty())
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
        validate_display(&self.display, &mut issues);
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
            ("minigame.lockpick", ContentKind::Minigame),
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

    #[test]
    fn ui_panel_definitions_require_a_schema_and_namespaced_labels() {
        let panel = UiPanelDefinition {
            id: DefinitionId::new("ui.terminal").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("ui.terminal.title"),
                description: key("ui.terminal.description"),
            },
            schema: "ui.terminal".to_string(),
            actions: vec![UiPanelAction {
                name: "close".to_string(),
                label: key("ui.action.close"),
                gated_by: None,
            }],
            fields: vec![UiFieldBinding {
                key: "status".to_string(),
                source: "entity.state.status".to_string(),
                formatter: Some("status".to_string()),
            }],
            fallback_schema: Some("ui.inspect".to_string()),
        };

        assert_eq!(panel.validate_shape(), Vec::new());

        let mut invalid = panel.clone();
        invalid.schema = "terminal".to_string();
        invalid.actions[0].label = key("close");
        let codes: Vec<_> = invalid
            .validate_shape()
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"ui.invalid_schema".to_string()));
        assert!(codes.contains(&"i18n.invalid_key".to_string()));
    }

    #[test]
    fn read_only_panels_need_a_fallback_and_duplicates_are_reported() {
        let mut panel = UiPanelDefinition {
            id: DefinitionId::new("panel.terminal").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("ui.terminal.title"),
                description: key("ui.terminal.description"),
            },
            schema: "ui.terminal".to_string(),
            actions: Vec::new(),
            fields: vec![UiFieldBinding {
                key: "status".to_string(),
                source: "entity.state.status".to_string(),
                formatter: None,
            }],
            fallback_schema: None,
        };

        assert!(
            panel
                .validate_shape()
                .iter()
                .any(|issue| issue.code == "ui.empty_actions")
        );

        panel.fallback_schema = Some("ui.inspect".to_string());
        assert_eq!(panel.validate_shape(), Vec::new());

        panel.fields.push(panel.fields[0].clone());
        panel.actions = vec![
            UiPanelAction {
                name: "close".to_string(),
                label: key("ui.action.close"),
                gated_by: None,
            },
            UiPanelAction {
                name: "close".to_string(),
                label: key("ui.action.close"),
                gated_by: None,
            },
        ];
        let codes: Vec<_> = panel
            .validate_shape()
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"ui.duplicate_field".to_string()));
        assert!(codes.contains(&"ui.duplicate_action".to_string()));
    }

    #[test]
    fn minigames_require_explicit_authority() {
        let game = MinigameDefinition {
            id: DefinitionId::new("minigame.lockpick").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("minigame.lockpick.name"),
                description: key("minigame.lockpick.description"),
            },
            panel_schema: "ui.lockpick".to_string(),
            authority: "server".to_string(),
            rewards: vec![DefinitionId::new("item.dust").unwrap()],
        };

        assert_eq!(game.validate_shape(), Vec::new());

        let mut invalid = game;
        invalid.authority = "godmode".to_string();
        assert!(
            invalid
                .validate_shape()
                .iter()
                .any(|issue| issue.code == "minigame.invalid_authority")
        );
    }

    #[test]
    fn client_authoritative_minigames_cannot_grant_rewards() {
        let mut game = MinigameDefinition {
            id: DefinitionId::new("minigame.dice").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("minigame.dice.name"),
                description: key("minigame.dice.description"),
            },
            panel_schema: "ui.dice".to_string(),
            authority: "client".to_string(),
            rewards: Vec::new(),
        };
        assert_eq!(game.validate_shape(), Vec::new());

        game.rewards = vec![DefinitionId::new("item.dust").unwrap()];
        assert!(
            game.validate_shape()
                .iter()
                .any(|issue| issue.code == "minigame.client_authority_rewards")
        );

        game.authority = "server".to_string();
        game.rewards.push(game.rewards[0].clone());
        let codes: Vec<_> = game
            .validate_shape()
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"content.duplicate_reference".to_string()));
    }
}
