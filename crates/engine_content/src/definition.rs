//! Authored content definitions.
//!
//! Definitions are immutable for a content version. Player-facing text is
//! stored as message keys and resolved through a locale catalog; Brazilian
//! Portuguese is the source locale.

use std::collections::{BTreeMap, BTreeSet};

use engine_core::i18n::MessageKey;
use engine_core::id::DefinitionId;
use engine_core::math::Vec3;
use engine_core::schema::{ComponentData, ComponentKey, SchemaVersion, ValidationIssue};
use engine_core::time::DailySchedule;
use serde::{Deserialize, Serialize};

/// The maximum accepted tag length.
pub const MAX_TAG_LEN: usize = 32;
/// Maximum route triggers authored in one zone definition.
pub const MAX_TRAVEL_TRIGGERS_PER_ZONE: usize = 64;

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
    /// A world map or atlas definition.
    Map,
    /// A behavior graph definition.
    Behavior,
    /// A UI panel definition.
    Ui,
    /// A minigame definition.
    Minigame,
    /// A typed link definition.
    Link,
}

impl ContentKind {
    /// The wire and manifest name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Entity => "entity",
            Self::Zone => "zone",
            Self::Map => "map",
            Self::Behavior => "behavior",
            Self::Ui => "ui",
            Self::Minigame => "minigame",
            Self::Link => "link",
        }
    }

    /// Infer the kind from the ID's first namespace segment.
    #[must_use]
    pub fn infer(id: &DefinitionId) -> Self {
        match id.namespace() {
            Some("item") => Self::Item,
            Some("zone") => Self::Zone,
            Some("map" | "world") => Self::Map,
            Some("behavior" | "ai") => Self::Behavior,
            Some("ui" | "panel") => Self::Ui,
            Some("minigame") => Self::Minigame,
            Some("link") => Self::Link,
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

/// A stable position on the authored world atlas, measured in map cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapPosition {
    /// Horizontal atlas cell.
    pub x: i32,
    /// Vertical atlas cell.
    pub y: i32,
}

/// The kind of transfer a zone route supports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// A walkable passage or road.
    #[default]
    Passage,
    /// A gateway that may require an explicit interaction.
    Gateway,
    /// A magical or technological portal.
    Portal,
}

impl RouteKind {
    /// The wire and authoring name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passage => "passage",
            Self::Gateway => "gateway",
            Self::Portal => "portal",
        }
    }
}

/// One zone placed on a world map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapZone {
    /// Stable zone definition ID.
    pub zone: DefinitionId,
    /// Position on the atlas.
    pub position: MapPosition,
}

/// An explicit route between two zones on the same map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneRoute {
    /// The zone where travel starts.
    pub from: DefinitionId,
    /// The zone where travel ends.
    pub to: DefinitionId,
    /// Whether the route works in both directions.
    #[serde(default)]
    pub bidirectional: bool,
    /// The transfer style.
    #[serde(default)]
    pub kind: RouteKind,
}

/// An authored world atlas: stable zone placements and the routes between them.
///
/// The atlas owns static topology only. Live territory ownership, conflict
/// state, and ownership history are runtime state and must not be baked into a
/// content bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldMapDefinition {
    /// Stable map key, for example `map.overworld`.
    pub id: DefinitionId,
    /// Version of the map schema and topology.
    pub version: SchemaVersion,
    /// Localized name and description.
    pub display: DisplayText,
    /// Zones placed on this map.
    pub zones: Vec<MapZone>,
    /// Explicit routes, including transfer points, between placed zones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<ZoneRoute>,
}

impl WorldMapDefinition {
    /// Resolve an explicitly authored travel route between two zone IDs.
    /// Reverse travel is available only when the route declares
    /// `bidirectional`; absence means callers must reject the handoff.
    #[must_use]
    pub fn route_kind(&self, from: &DefinitionId, to: &DefinitionId) -> Option<RouteKind> {
        self.routes.iter().find_map(|route| {
            ((route.from == *from && route.to == *to)
                || (route.bidirectional && route.from == *to && route.to == *from))
                .then_some(route.kind)
        })
    }

    /// Validate map topology without resolving zone definitions.
    ///
    /// Zone existence and unmapped-zone diagnostics require a loaded
    /// [`crate::validate::ContentSet`].
    #[must_use]
    pub fn validate_shape(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.version.major == 0 {
            issues.push(ValidationIssue::new(
                "content.invalid_version",
                "version.major",
                "map versions must have a major version of at least one",
            ));
        }
        validate_display(&self.display, &mut issues);
        if !self.id.is_in("map") && !self.id.is_in("world") {
            issues.push(ValidationIssue::new(
                "world.invalid_id",
                "id",
                "map ids must use the map or world namespace",
            ));
        }
        if self.zones.is_empty() {
            issues.push(ValidationIssue::new(
                "world.empty_map",
                "zones",
                "a world map must place at least one zone",
            ));
        }

        let mut zone_ids = BTreeSet::new();
        let mut positions = BTreeSet::new();
        for (index, zone) in self.zones.iter().enumerate() {
            if !zone.zone.is_in("zone") {
                issues.push(ValidationIssue::new(
                    "world.invalid_zone_reference",
                    format!("zones[{index}].zone"),
                    "map placements must reference a zone definition",
                ));
            }
            if !zone_ids.insert(zone.zone.as_str()) {
                issues.push(ValidationIssue::new(
                    "world.duplicate_zone",
                    format!("zones[{index}].zone"),
                    format!("zone {} is placed more than once", zone.zone),
                ));
            }
            if !positions.insert((zone.position.x, zone.position.y)) {
                issues.push(ValidationIssue::new(
                    "world.duplicate_position",
                    format!("zones[{index}].position"),
                    format!(
                        "another zone already occupies atlas cell ({}, {})",
                        zone.position.x, zone.position.y
                    ),
                ));
            }
        }

        let mut routes = BTreeSet::new();
        for (index, route) in self.routes.iter().enumerate() {
            if route.from == route.to {
                issues.push(ValidationIssue::new(
                    "world.self_route",
                    format!("routes[{index}]"),
                    "a zone route must connect two different zones",
                ));
            }
            for (field, endpoint) in [("from", &route.from), ("to", &route.to)] {
                if !zone_ids.contains(endpoint.as_str()) {
                    issues.push(ValidationIssue::new(
                        "world.unknown_route_endpoint",
                        format!("routes[{index}].{field}"),
                        format!("route endpoint {endpoint} is not placed on this map"),
                    ));
                }
            }
            if !routes.insert((route.from.as_str(), route.to.as_str())) {
                issues.push(ValidationIssue::new(
                    "world.duplicate_route",
                    format!("routes[{index}]"),
                    format!(
                        "route {} -> {} is declared more than once",
                        route.from, route.to
                    ),
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
    /// Bounded simulation behavior for `behavior.*` definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorDefinition>,
    /// Zone-local activation points for authored map routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub travel_triggers: Vec<ZoneTravelTrigger>,
    /// Static entity instances authored in this zone definition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_placements: Vec<ZoneEntityPlacement>,
    /// Directed links between placements in this zone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_links: Vec<ZoneEntityLink>,
    /// Typed endpoint and action contract for `link.*` definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkDefinition>,
}

/// Compatibility and simulation contract for one typed link definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkDefinition {
    /// Required component on every source entity.
    pub source_component: ComponentKey,
    /// Required component on every target entity.
    pub target_component: ComponentKey,
    /// Events accepted by links of this type.
    pub supported_events: Vec<ZoneLinkEvent>,
    /// Server-side operation performed for a supported event.
    pub action: LinkAction,
}

/// Operations implemented by the authoritative entity-link dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkAction {
    /// Toggle an unlocked target door.
    ToggleDoor,
    /// Set the target door to open.
    OpenDoor,
    /// Set the target door to closed.
    CloseDoor,
}

/// One stable placement of an entity definition in a zone's local coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneEntityPlacement {
    /// Stable placement key, unique within its zone definition.
    pub id: String,
    /// Entity definition used as the instance prototype.
    pub definition: DefinitionId,
    /// Zone-local world position in meters.
    pub position: Vec3,
    /// Facing angle in degrees, clockwise from north.
    #[serde(default)]
    pub yaw_degrees: f32,
}

/// One directed, typed link between two placement IDs in the same zone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneEntityLink {
    /// Link type, for example `link.lever_to_door`.
    pub link_type: DefinitionId,
    /// Source placement ID that emits the event.
    pub source: String,
    /// Target placement ID that receives the event.
    pub target: String,
    /// Events carried by this link.
    pub events: Vec<ZoneLinkEvent>,
    /// Whether the authored link starts enabled.
    #[serde(default = "authored_link_enabled")]
    pub enabled: bool,
    /// Server ticks before the link may fire again.
    #[serde(default)]
    pub cooldown_ticks: u64,
}

const fn authored_link_enabled() -> bool {
    true
}

/// Event carried by an authored zone entity link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneLinkEvent {
    /// A general activation, such as a lever pull.
    Activated,
    /// A door was opened by an authoritative action.
    Opened,
    /// A door was closed by an authoritative action.
    Closed,
    /// An actor entered a server-authored trigger area.
    Triggered,
}

/// A bounded, zone-owned activation point for one explicitly authored map route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneTravelTrigger {
    /// Stable trigger key, unique within its zone definition.
    pub id: String,
    /// Destination zone definition on the same world map.
    pub destination: DefinitionId,
    /// Center of the activation area in this zone's local world coordinates.
    pub position: Vec3,
    /// Activation radius in meters.
    pub radius_m: f32,
}

/// Declarative inputs for the bounded built-in AI behavior primitives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorDefinition {
    /// Runtime primitive selected by this content definition.
    pub kind: BehaviorKind,
    /// Maximum perception and engagement range in meters.
    pub range_m: f32,
    /// Optional recurring UTC window. Omission keeps the behavior active all day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<DailySchedule>,
    /// Optional stable group key for allied AI target sharing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggro_group: Option<DefinitionId>,
    /// Optional bounded instruction graph. Omission selects the legacy primitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<BehaviorGraph>,
    /// Ordered patrol points used by `patrol`; the route loops after its last point.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patrol_route: Vec<Vec3>,
}

/// Maximum authored nodes in one behavior graph.
pub const MAX_BEHAVIOR_GRAPH_NODES: usize = 64;
/// Maximum authored points in one patrol route.
pub const MAX_BEHAVIOR_PATROL_POINTS: usize = 64;

/// A directed behavior graph evaluated from its entry for each AI decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorGraph {
    /// First node evaluated each tick.
    pub entry: DefinitionId,
    /// Bounded set of reachable or reserved nodes.
    pub nodes: Vec<BehaviorNode>,
}

impl BehaviorGraph {
    /// Whether node IDs and transition references form a valid bounded graph.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.nodes.is_empty() || self.nodes.len() > MAX_BEHAVIOR_GRAPH_NODES {
            return false;
        }
        let ids: BTreeSet<_> = self.nodes.iter().map(|node| node.id.clone()).collect();
        ids.len() == self.nodes.len()
            && ids.contains(&self.entry)
            && self.nodes.iter().all(|node| {
                node.on_success
                    .as_ref()
                    .is_none_or(|next| ids.contains(next))
                    && node
                        .on_failure
                        .as_ref()
                        .is_none_or(|next| ids.contains(next))
                    && (node.instruction != BehaviorInstruction::Wait
                        || (node.on_success.is_none() && node.on_failure.is_none()))
            })
    }
}

/// One bounded instruction in a behavior graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorNode {
    /// Stable identifier used by graph transitions.
    pub id: DefinitionId,
    /// Operation evaluated at this node.
    pub instruction: BehaviorInstruction,
    /// Next node when the operation succeeds; `None` ends this decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success: Option<DefinitionId>,
    /// Next node when the operation fails; `None` ends this decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<DefinitionId>,
}

/// Operations supported by the bounded behavior graph interpreter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorInstruction {
    /// Find the nearest visible hostile within the behavior's authored range.
    PerceiveHostiles,
    /// Attack the target selected earlier in this graph decision.
    AttackTarget,
    /// Follow the selected target using zone path and movement budgets.
    ChaseTarget,
    /// Move away from the selected target using authoritative movement checks.
    FleeTarget,
    /// Use the selected target through the authoritative interaction pipeline.
    InteractTarget,
    /// Move toward the next point in the behavior's looping patrol route.
    Patrol,
    /// End the current decision without acting.
    Wait,
}

/// Simulation behavior primitives that content may select without code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorKind {
    /// Perceive and attack visible hostiles without pursuing them.
    Guard,
    /// Perceive and attack visible hostiles, pursuing with bounded paths.
    GuardChase,
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
        let mut placement_ids = BTreeSet::new();
        for (index, placement) in self.entity_placements.iter().enumerate() {
            let path = format!("entity_placements[{index}]");
            if self.kind() != ContentKind::Zone {
                issues.push(ValidationIssue::new(
                    "world.entity_placement_on_non_zone",
                    &path,
                    "entity placements may only be declared by zone definitions",
                ));
            }
            if !is_valid_tag(&placement.id) || !placement_ids.insert(&placement.id) {
                issues.push(ValidationIssue::new(
                    "world.invalid_entity_placement_id",
                    format!("{path}.id"),
                    "placement IDs must be unique lowercase tags within the zone",
                ));
            }
            if !placement.definition.is_in("entity") {
                issues.push(ValidationIssue::new(
                    "world.invalid_placed_entity_definition",
                    format!("{path}.definition"),
                    "placed entity definitions must use the entity namespace",
                ));
            }
            if !placement.position.x.is_finite()
                || !placement.position.y.is_finite()
                || !placement.position.z.is_finite()
            {
                issues.push(ValidationIssue::new(
                    "world.invalid_entity_placement_position",
                    format!("{path}.position"),
                    "entity placement positions must be finite",
                ));
            }
            if !placement.yaw_degrees.is_finite() {
                issues.push(ValidationIssue::new(
                    "world.invalid_entity_placement_yaw",
                    format!("{path}.yaw_degrees"),
                    "entity placement facing must be finite",
                ));
            }
        }
        let placement_ids: BTreeSet<&str> = self
            .entity_placements
            .iter()
            .map(|placement| placement.id.as_str())
            .collect();
        let mut link_keys = BTreeSet::new();
        let mut endpoint_events = BTreeSet::new();
        for (index, link) in self.entity_links.iter().enumerate() {
            let path = format!("entity_links[{index}]");
            if self.kind() != ContentKind::Zone {
                issues.push(ValidationIssue::new(
                    "world.entity_link_on_non_zone",
                    &path,
                    "entity links may only be declared by zone definitions",
                ));
            }
            if !link.link_type.is_in("link") {
                issues.push(ValidationIssue::new(
                    "world.invalid_entity_link_type",
                    format!("{path}.link_type"),
                    "entity link types must use the link namespace",
                ));
            }
            if !placement_ids.contains(link.source.as_str()) {
                issues.push(ValidationIssue::new(
                    "world.unknown_link_source",
                    format!("{path}.source"),
                    "entity link source must name a placement in this zone",
                ));
            }
            if !placement_ids.contains(link.target.as_str()) {
                issues.push(ValidationIssue::new(
                    "world.unknown_link_target",
                    format!("{path}.target"),
                    "entity link target must name a placement in this zone",
                ));
            }
            if link.source == link.target {
                issues.push(ValidationIssue::new(
                    "world.entity_link_self_reference",
                    &path,
                    "entity links cannot target their own source",
                ));
            }
            if link.events.is_empty()
                || link.events.iter().copied().collect::<BTreeSet<_>>().len() != link.events.len()
            {
                issues.push(ValidationIssue::new(
                    "world.invalid_entity_link_events",
                    format!("{path}.events"),
                    "entity links must declare a non-empty set of unique events",
                ));
            }
            if link.cooldown_ticks > 1_000_000 {
                issues.push(ValidationIssue::new(
                    "world.invalid_entity_link_cooldown",
                    format!("{path}.cooldown_ticks"),
                    "entity link cooldown cannot exceed 1,000,000 ticks",
                ));
            }
            if !link_keys.insert((
                link.link_type.clone(),
                link.source.as_str(),
                link.target.as_str(),
            )) {
                issues.push(ValidationIssue::new(
                    "world.duplicate_entity_link",
                    &path,
                    "entity links cannot repeat the same type and endpoints",
                ));
            }
            for event in &link.events {
                if !endpoint_events.insert((link.source.as_str(), link.target.as_str(), *event)) {
                    issues.push(ValidationIssue::new(
                        "world.duplicate_entity_link_event",
                        &path,
                        "a source and target cannot receive the same event through multiple links",
                    ));
                }
            }
        }
        let mut in_degrees: BTreeMap<&str, usize> =
            placement_ids.iter().map(|id| (*id, 0)).collect();
        let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for link in &self.entity_links {
            if link.events.iter().any(|event| {
                matches!(
                    event,
                    ZoneLinkEvent::Activated
                        | ZoneLinkEvent::Opened
                        | ZoneLinkEvent::Closed
                        | ZoneLinkEvent::Triggered
                )
            }) && in_degrees.contains_key(link.source.as_str())
                && in_degrees.contains_key(link.target.as_str())
            {
                outgoing
                    .entry(link.source.as_str())
                    .or_default()
                    .push(link.target.as_str());
                *in_degrees.entry(link.target.as_str()).or_default() += 1;
            }
        }
        let mut ready: Vec<&str> = in_degrees
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
            .collect();
        let mut visited = 0;
        while let Some(source) = ready.pop() {
            visited += 1;
            for target in outgoing.get(source).into_iter().flatten() {
                if let Some(degree) = in_degrees.get_mut(target) {
                    *degree -= 1;
                    if *degree == 0 {
                        ready.push(target);
                    }
                }
            }
        }
        if visited < in_degrees.len() {
            issues.push(ValidationIssue::new(
                "world.entity_link_cycle",
                "entity_links",
                "activation links cannot form a cycle",
            ));
        }
        let mut trigger_ids = BTreeSet::new();
        if self.travel_triggers.len() > MAX_TRAVEL_TRIGGERS_PER_ZONE {
            issues.push(ValidationIssue::new(
                "world.travel_trigger_limit",
                "travel_triggers",
                "a zone may declare at most 64 travel triggers",
            ));
        }
        for (index, trigger) in self.travel_triggers.iter().enumerate() {
            let path = format!("travel_triggers[{index}]");
            if self.kind() != ContentKind::Zone {
                issues.push(ValidationIssue::new(
                    "world.travel_trigger_on_non_zone",
                    &path,
                    "travel triggers may only be declared by zone definitions",
                ));
            }
            if !is_valid_tag(&trigger.id) || !trigger_ids.insert(&trigger.id) {
                issues.push(ValidationIssue::new(
                    "world.invalid_travel_trigger_id",
                    format!("{path}.id"),
                    "trigger IDs must be unique lowercase tags within the zone",
                ));
            }
            if !trigger.destination.is_in("zone") {
                issues.push(ValidationIssue::new(
                    "world.invalid_travel_destination",
                    format!("{path}.destination"),
                    "travel trigger destinations must use the zone namespace",
                ));
            }
            if !trigger.position.x.is_finite()
                || !trigger.position.y.is_finite()
                || !trigger.position.z.is_finite()
            {
                issues.push(ValidationIssue::new(
                    "world.invalid_travel_trigger_position",
                    format!("{path}.position"),
                    "travel trigger positions must be finite",
                ));
            }
            if !trigger.radius_m.is_finite() || trigger.radius_m <= 0.0 || trigger.radius_m > 32.0 {
                issues.push(ValidationIssue::new(
                    "world.invalid_travel_trigger_radius",
                    format!("{path}.radius_m"),
                    "travel trigger radius must be finite and within (0, 32] meters",
                ));
            }
        }
        if self.version.major == 0 {
            issues.push(ValidationIssue::new(
                "content.invalid_version",
                "version.major",
                "content versions must have a major version of at least one",
            ));
        }
        validate_display(&self.display, &mut issues);
        if self.components.is_empty() && self.kind() != ContentKind::Link {
            issues.push(ValidationIssue::new(
                "content.empty_components",
                "components",
                "definition declares no components",
            ));
        }
        match (self.kind(), &self.link) {
            (ContentKind::Link, None) => issues.push(ValidationIssue::new(
                "content.missing_link",
                "link",
                "link definitions must declare endpoint compatibility and an action",
            )),
            (ContentKind::Link, Some(link)) => {
                let source_matches_events = link.supported_events.iter().all(|event| match event {
                    ZoneLinkEvent::Activated => {
                        link.source_component.as_str() == "core.interactable"
                    }
                    ZoneLinkEvent::Opened | ZoneLinkEvent::Closed => {
                        link.source_component.as_str() == "core.door"
                    }
                    ZoneLinkEvent::Triggered => link.source_component.as_str() == "core.trigger",
                });
                if !source_matches_events || link.target_component.as_str() != "core.door" {
                    issues.push(ValidationIssue::new(
                        "content.unsupported_link_contract",
                        "link",
                        "activated links require core.interactable sources; opened/closed links require core.door sources; triggered links require core.trigger sources; all supported actions require core.door targets",
                    ));
                }
                if link.supported_events.is_empty()
                    || link
                        .supported_events
                        .iter()
                        .copied()
                        .collect::<BTreeSet<_>>()
                        .len()
                        != link.supported_events.len()
                {
                    issues.push(ValidationIssue::new(
                        "content.invalid_link_events",
                        "link.supported_events",
                        "link definitions must declare a non-empty set of unique supported events",
                    ));
                }
            }
            (_, Some(_)) => issues.push(ValidationIssue::new(
                "content.unexpected_link",
                "link",
                "only link definitions may declare a link contract",
            )),
            _ => {}
        }
        match (self.kind(), &self.behavior) {
            (ContentKind::Behavior, None) => issues.push(ValidationIssue::new(
                "content.missing_behavior",
                "behavior",
                "behavior definitions must declare bounded simulation semantics",
            )),
            (ContentKind::Behavior, Some(behavior))
                if !behavior.range_m.is_finite() || behavior.range_m <= 0.0 =>
            {
                issues.push(ValidationIssue::new(
                    "content.invalid_behavior_range",
                    "behavior.range_m",
                    "behavior range must be finite and positive",
                ));
            }
            (ContentKind::Behavior, Some(_)) => {}
            (_, Some(_)) => issues.push(ValidationIssue::new(
                "content.unexpected_behavior",
                "behavior",
                "only behavior definitions may declare simulation behavior",
            )),
            _ => {}
        }
        if self.kind() == ContentKind::Behavior
            && self
                .behavior
                .as_ref()
                .and_then(|behavior| behavior.schedule)
                .is_some_and(|schedule| !schedule.is_valid())
        {
            issues.push(ValidationIssue::new(
                "content.invalid_behavior_schedule",
                "behavior.schedule",
                "daily schedule must use distinct UTC minute bounds within 0..1440",
            ));
        }
        if self
            .behavior
            .as_ref()
            .and_then(|behavior| behavior.graph.as_ref())
            .is_some_and(|graph| !graph.is_valid())
        {
            issues.push(ValidationIssue::new(
                "content.invalid_behavior_graph",
                "behavior.graph",
                "behavior graph must have unique nodes, a valid entry, and valid transitions within the node limit",
            ));
        }
        if let Some(behavior) = self.behavior.as_ref() {
            let has_patrol_instruction = behavior.graph.as_ref().is_some_and(|graph| {
                graph
                    .nodes
                    .iter()
                    .any(|node| node.instruction == BehaviorInstruction::Patrol)
            });
            let route_invalid = behavior.patrol_route.len() > MAX_BEHAVIOR_PATROL_POINTS
                || behavior.patrol_route.iter().any(|point| {
                    !point.x.is_finite() || !point.y.is_finite() || !point.z.is_finite()
                })
                || (has_patrol_instruction && behavior.patrol_route.len() < 2);
            if route_invalid {
                issues.push(ValidationIssue::new(
                    "content.invalid_behavior_patrol_route",
                    "behavior.patrol_route",
                    "patrol routes must contain 2..64 finite points when used by a patrol instruction",
                ));
            }
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

#[cfg_attr(coverage_nightly, coverage(off))]
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
            behavior: None,
            travel_triggers: Vec::new(),
            entity_placements: Vec::new(),
            entity_links: Vec::new(),
            link: None,
        }
    }

    #[test]
    fn kinds_are_inferred_from_namespaces() {
        for (id, expected) in [
            ("item.torch", ContentKind::Item),
            ("door.wooden", ContentKind::Entity),
            ("zone.tavern", ContentKind::Zone),
            ("map.overworld", ContentKind::Map),
            ("world.overworld", ContentKind::Map),
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
    fn behavior_definitions_validate_bounded_runtime_semantics() {
        let mut definition = definition(PresentationMode::Billboard2d);
        definition.id = DefinitionId::new("behavior.demo_guard").unwrap();
        assert_eq!(definition.kind(), ContentKind::Behavior);
        assert!(
            definition
                .validate_shape()
                .iter()
                .any(|issue| { issue.code == "content.missing_behavior" })
        );

        definition.behavior = Some(BehaviorDefinition {
            kind: BehaviorKind::GuardChase,
            range_m: 12.0,
            schedule: None,
            aggro_group: None,
            graph: None,
            patrol_route: Vec::new(),
        });
        let authored = ron::to_string(&definition).unwrap();
        let parsed: ContentDefinition = ron::from_str(&authored).unwrap();
        assert_eq!(parsed, definition);
        let valid_issues = definition.validate_shape();
        assert!(
            !valid_issues
                .iter()
                .any(|issue| issue.code.contains("behavior")),
            "unexpected behavior issues: {valid_issues:?}"
        );

        definition.behavior.as_mut().unwrap().range_m = f32::INFINITY;
        assert!(
            definition
                .validate_shape()
                .iter()
                .any(|issue| issue.code == "content.invalid_behavior_range")
        );

        definition.id = DefinitionId::new("entity.guard").unwrap();
        definition.behavior = Some(BehaviorDefinition {
            kind: BehaviorKind::Guard,
            range_m: 12.0,
            schedule: None,
            aggro_group: None,
            graph: None,
            patrol_route: Vec::new(),
        });
        assert!(
            definition
                .validate_shape()
                .iter()
                .any(|issue| issue.code == "content.unexpected_behavior")
        );
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

    fn map_zone(zone: &str, x: i32, y: i32) -> MapZone {
        MapZone {
            zone: DefinitionId::new(zone).unwrap(),
            position: MapPosition { x, y },
        }
    }

    #[test]
    fn world_maps_validate_placements_and_routes() {
        let mut map = WorldMapDefinition {
            id: DefinitionId::new("map.overworld").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("map.overworld.name"),
                description: key("map.overworld.description"),
            },
            zones: vec![
                map_zone("zone.starter", 0, 0),
                map_zone("zone.market", 1, 0),
            ],
            routes: vec![ZoneRoute {
                from: DefinitionId::new("zone.starter").unwrap(),
                to: DefinitionId::new("zone.market").unwrap(),
                bidirectional: true,
                kind: RouteKind::Passage,
            }],
        };
        assert_eq!(map.validate_shape(), Vec::new());
        let starter = DefinitionId::new("zone.starter").unwrap();
        let market = DefinitionId::new("zone.market").unwrap();
        let missing = DefinitionId::new("zone.unrouted").unwrap();
        assert_eq!(map.route_kind(&starter, &market), Some(RouteKind::Passage));
        assert_eq!(map.route_kind(&market, &starter), Some(RouteKind::Passage));
        assert_eq!(map.route_kind(&starter, &missing), None);
        map.routes[0].bidirectional = false;
        assert_eq!(map.route_kind(&market, &starter), None);

        map.zones.push(map_zone("zone.starter", 0, 0));
        map.routes.push(ZoneRoute {
            from: DefinitionId::new("zone.starter").unwrap(),
            to: DefinitionId::new("zone.missing").unwrap(),
            bidirectional: false,
            kind: RouteKind::Portal,
        });
        map.routes.push(ZoneRoute {
            from: DefinitionId::new("zone.starter").unwrap(),
            to: DefinitionId::new("zone.market").unwrap(),
            bidirectional: false,
            kind: RouteKind::Gateway,
        });
        let codes: Vec<_> = map
            .validate_shape()
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"world.duplicate_zone".to_string()));
        assert!(codes.contains(&"world.duplicate_position".to_string()));
        assert!(codes.contains(&"world.unknown_route_endpoint".to_string()));
        assert!(codes.contains(&"world.duplicate_route".to_string()));
    }

    #[test]
    fn world_maps_require_placements_and_the_map_namespace() {
        let map = WorldMapDefinition {
            id: DefinitionId::new("overworld").unwrap(),
            version: SchemaVersion::new(1, 0),
            display: DisplayText {
                name: key("map.overworld.name"),
                description: key("map.overworld.description"),
            },
            zones: Vec::new(),
            routes: Vec::new(),
        };
        let codes: Vec<_> = map
            .validate_shape()
            .into_iter()
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&"world.invalid_id".to_string()));
        assert!(codes.contains(&"world.empty_map".to_string()));
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
