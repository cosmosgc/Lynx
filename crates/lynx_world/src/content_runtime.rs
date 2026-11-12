//! Loading and integrity checks for a deployed versioned content bundle.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use engine_content::{
    ContentKind, ContentLoader, ContentManifest, ContentSet, ContentVersionSnapshot,
    LinkAction as AuthoredLinkAction, PersistedInstanceMigrationPlan, ZoneLinkEvent, sha256_hex,
    starter_component_registry,
};
use engine_core::error::{EngineError, ErrorCode};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_core::schema::{
    ComponentInstance, ComponentKey, ComponentRegistry, PersistencePolicy, SchemaVersion,
};
use engine_sim::action::Verb;
use engine_sim::entity::{Door, EntityRecord, Interactable, MovementProfile, Transform};
use engine_sim::link::{EntityLink, LinkAction, LinkEvent};
use engine_sim::mind::Mind;
use engine_sim::nav::NavGrid;
use engine_sim::snapshot::QuarantinedRecord;
use engine_sim::zone::{Zone, ZoneConfig};

use crate::persistence::WorldSnapshot;

/// A content bundle loaded and checked against its manifest.
#[derive(Debug)]
pub struct RuntimeContent {
    /// Validated definitions and catalogs.
    pub set: ContentSet,
    /// Component schemas used to validate persisted component instances.
    pub component_registry: ComponentRegistry,
    /// Manifest whose declared files match the loaded content.
    pub manifest: ContentManifest,
    /// SHA-256 of the exact manifest bytes used to load this bundle.
    pub manifest_sha256: String,
    /// Version snapshot for readiness and metrics.
    pub version: ContentVersionSnapshot,
}

/// One runtime zone produced from a world-map placement and its zone definition.
#[derive(Debug)]
pub struct AuthoredZoneRuntime {
    /// World atlas that contains this zone placement.
    pub map: DefinitionId,
    /// Authored zone definition used to construct this instance.
    pub definition: DefinitionId,
    /// Deterministic instance ID derived from the map and zone definition.
    pub instance: StableId,
    /// Maximum sessions the zone gateway may admit.
    pub max_players: usize,
    /// Default safe entry position for new sessions and travel arrival policy.
    pub entry_point: Vec3,
    /// Authored passage triggers resolved to stable destination instances.
    pub travel_triggers: Vec<ResolvedTravelTrigger>,
    /// Constructed authoritative simulation zone.
    pub zone: Zone,
}

/// A validated zone-local trigger with its destination's safe arrival point.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedTravelTrigger {
    /// Stable trigger key within the source zone definition.
    pub id: String,
    /// Center of the activation area in the source zone.
    pub position: Vec3,
    /// Activation radius in meters.
    pub radius_m: f32,
    /// Stable destination zone instance.
    pub destination_zone: StableId,
    /// Destination zone's default safe entry point.
    pub arrival_point: Vec3,
}

/// Build every placed zone in a validated map using its authored `core.zone`
/// configuration.
///
/// The instance ID is stable for a given map/definition pair, which lets zone
/// leases and checkpoints keep addressing the same zone across restarts. Static
/// entity placements are reconstructed from the bundle at startup and excluded
/// from snapshots. Travel triggers resolve to the destination's default entry.
///
/// # Errors
/// Returns an error when the map or a referenced definition is missing, when a
/// zone has no valid `core.zone` component, when geometry exceeds the runtime
/// grid bounds, or when an entry point is outside/inside a blocked cell.
pub fn build_authored_zones(
    content: &RuntimeContent,
    map_id: &DefinitionId,
) -> Result<Vec<AuthoredZoneRuntime>, EngineError> {
    let map = content
        .set
        .maps
        .get(map_id)
        .ok_or_else(|| EngineError::not_found("world map", map_id))?;
    let component_key = ComponentKey::new("core.zone")?;
    let mut instances = BTreeSet::new();
    let mut zones = Vec::with_capacity(map.zones.len());
    for placement in &map.zones {
        let definition = content
            .set
            .definitions
            .get(&placement.zone)
            .ok_or_else(|| EngineError::not_found("zone definition", &placement.zone))?;
        let component = definition.components.get(&component_key).ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "a placed zone definition has no core.zone component",
            )
            .with_detail("zone", placement.zone.to_string())
        })?;
        let schema = content
            .component_registry
            .get(&component_key)
            .ok_or_else(|| {
                EngineError::new(
                    ErrorCode::InvalidState,
                    "the core.zone schema is unavailable",
                )
            })?;
        let prepared = content.component_registry.prepare(&ComponentInstance::new(
            component_key.clone(),
            schema.version,
            component.clone(),
        ))?;
        if let Some(issue) = prepared.issues.first() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "a placed zone has invalid runtime configuration",
            )
            .with_detail("zone", placement.zone.to_string())
            .with_detail("field", &issue.path)
            .with_detail("reason", &issue.message));
        }
        let data = &prepared.instance.data;
        let max_players = required_usize(data, "max_players", &placement.zone)?;
        let max_entities = required_usize(data, "max_entities", &placement.zone)?;
        let width = required_usize(data, "width_cells", &placement.zone)?;
        let height = required_usize(data, "height_cells", &placement.zone)?;
        let cell_size = required_f32(data, "cell_size_m", &placement.zone)?;
        let origin_x = required_f32(data, "origin_x_m", &placement.zone)?;
        let origin_z = required_f32(data, "origin_z_m", &placement.zone)?;
        let floor_y = required_f32(data, "floor_y_m", &placement.zone)?;
        let ceiling_y = required_f32(data, "ceiling_y_m", &placement.zone)?;
        if max_entities < max_players
            || ceiling_y <= floor_y
            || width
                .checked_mul(height)
                .is_none_or(|cells| cells > 262_144)
        {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "zone geometry exceeds supported bounds",
            )
            .with_detail("zone", placement.zone.to_string()));
        }

        let min = Vec3::new(origin_x, floor_y, origin_z);
        let width_cells =
            u16::try_from(width).map_err(|_| invalid_zone_field(&placement.zone, "width_cells"))?;
        let height_cells = u16::try_from(height)
            .map_err(|_| invalid_zone_field(&placement.zone, "height_cells"))?;
        let width_m = f32::from(width_cells) * cell_size;
        let height_m = f32::from(height_cells) * cell_size;
        let max = Vec3::new(origin_x + width_m, ceiling_y, origin_z + height_m);
        if !max.x.is_finite() || !max.z.is_finite() {
            return Err(EngineError::new(
                ErrorCode::InvalidRequest,
                "zone geometry produces non-finite bounds",
            )
            .with_detail("zone", placement.zone.to_string()));
        }
        let instance = zone_instance_id(map_id, &placement.zone)?;
        if !instances.insert(instance) {
            return Err(EngineError::new(
                ErrorCode::Conflict,
                "authored zone instance IDs collided",
            )
            .with_detail("zone", placement.zone.to_string()));
        }
        let config = ZoneConfig::new(placement.zone.clone(), instance, min, max)?
            .with_max_entities(max_entities);
        let mut nav = NavGrid::new(width, height, cell_size, min).ok_or_else(|| {
            EngineError::new(ErrorCode::InvalidRequest, "zone navigation grid is invalid")
                .with_detail("zone", placement.zone.to_string())
        })?;
        let blocked = data
            .get("blocked_cells")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid_zone_field(&placement.zone, "blocked_cells"))?;
        let cell_count = width * height;
        let mut blocked_seen = BTreeSet::new();
        for value in blocked {
            let index = value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < cell_count)
                .ok_or_else(|| invalid_zone_field(&placement.zone, "blocked_cells"))?;
            if !blocked_seen.insert(index) {
                return Err(invalid_zone_field(&placement.zone, "blocked_cells"));
            }
            nav.set_blocked(index % width, index / width, true);
        }
        let center = Vec3::new(
            min.x + (f32::from(width_cells) * 0.5 + 0.5) * cell_size,
            floor_y,
            min.z + (f32::from(height_cells) * 0.5 + 0.5) * cell_size,
        );
        let entry_point = Vec3::new(
            optional_f32(data, "entry_x_m").unwrap_or(center.x),
            optional_f32(data, "entry_y_m").unwrap_or(floor_y),
            optional_f32(data, "entry_z_m").unwrap_or(center.z),
        );
        let Some((entry_x, entry_z)) = nav.cell_of(entry_point) else {
            return Err(invalid_zone_field(&placement.zone, "entry_point"));
        };
        if nav.is_blocked(entry_x, entry_z) || !config.contains(entry_point) {
            return Err(invalid_zone_field(&placement.zone, "entry_point"));
        }
        let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::default()))?;
        let placements = definition.entity_placements.iter().collect::<Vec<_>>();
        let mut occupied_cells = BTreeSet::new();
        for entity_placement in placements {
            let entity_definition = content
                .set
                .definitions
                .get(&entity_placement.definition)
                .ok_or_else(|| {
                    EngineError::not_found("entity definition", &entity_placement.definition)
                })?;
            let Some((x, z)) = zone.nav().cell_of(entity_placement.position) else {
                return Err(invalid_zone_field(
                    &placement.zone,
                    "entity_placements.position",
                ));
            };
            if zone.nav().is_blocked(x, z)
                || !zone.config().contains(entity_placement.position)
                || !occupied_cells.insert((x, z))
            {
                return Err(invalid_zone_field(
                    &placement.zone,
                    "entity_placements.position",
                ));
            }
            let mut entity = EntityRecord::new(
                placed_entity_id(map_id, &placement.zone, &entity_placement.id)?,
                entity_definition.id.clone(),
                entity_placement.position,
            )
            .with_persistence(PersistencePolicy::StaticDefinition);
            entity.transform =
                Transform::new(entity_placement.position).with_yaw(entity_placement.yaw_degrees);
            entity.tags = entity_definition.tags.clone();
            for (key, data) in &entity_definition.components {
                let schema = content.component_registry.get(key).ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::InvalidState,
                        "a placed entity uses an unavailable component schema",
                    )
                    .with_detail("component", key.to_string())
                })?;
                let prepared = content.component_registry.prepare(&ComponentInstance::new(
                    key.clone(),
                    schema.version,
                    data.clone(),
                ))?;
                if let Some(issue) = prepared.issues.first() {
                    return Err(EngineError::new(
                        ErrorCode::InvalidRequest,
                        "a placed entity has invalid component data",
                    )
                    .with_detail("placement", &entity_placement.id)
                    .with_detail("entity", entity_definition.id.to_string())
                    .with_detail("field", &issue.path)
                    .with_detail("reason", &issue.message));
                }
                let interactable = if key.as_str() == "core.interactable" {
                    Some(build_interactable(
                        &prepared.instance.data,
                        &entity_definition.id,
                    )?)
                } else {
                    None
                };
                let door = if key.as_str() == "core.door" {
                    Some(build_door(&prepared.instance.data, &entity_definition.id)?)
                } else {
                    None
                };
                let health = if key.as_str() == "core.health" {
                    Some(build_health(
                        &prepared.instance.data,
                        &entity_definition.id,
                    )?)
                } else {
                    None
                };
                let movement = if key.as_str() == "core.movement" {
                    Some(build_movement(
                        &prepared.instance.data,
                        &entity_definition.id,
                    )?)
                } else {
                    None
                };
                let mind = if key.as_str() == "core.behavior" {
                    Some(build_ai_mind(
                        &prepared.instance.data,
                        &entity_definition.id,
                        &content.set,
                    )?)
                } else {
                    None
                };
                entity = entity.with_component(prepared.instance)?;
                if let Some(interactable) = interactable {
                    entity = entity.with_interactable(interactable);
                }
                if let Some(door) = door {
                    entity = entity.with_door(door);
                }
                if let Some(max_health) = health {
                    entity = entity.with_health(max_health);
                }
                if let Some(movement) = movement {
                    entity = entity.with_movement(movement);
                }
                if let Some(mind) = mind {
                    entity = entity.with_mind(mind);
                }
            }
            if let Some(profile) = entity.movement_profile().filter(|profile| profile.occupies)
                && zone.entities().any(|existing| {
                    existing.movement_profile().is_some_and(|other| {
                        other.occupies
                            && entity.position().distance_xz(existing.position())
                                < profile.radius_m + other.radius_m
                    })
                })
            {
                return Err(invalid_zone_field(
                    &placement.zone,
                    "entity_placements.position",
                ));
            }
            zone.spawn(entity)?;
        }
        for authored_link in &definition.entity_links {
            let link_contract = content
                .set
                .definitions
                .get(&authored_link.link_type)
                .and_then(|definition| definition.link.as_ref())
                .ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::InvalidState,
                        "validated authored link definition is missing at runtime",
                    )
                    .with_detail("link_type", authored_link.link_type.to_string())
                })?;
            let events = authored_link.events.iter().map(|event| match event {
                ZoneLinkEvent::Activated => LinkEvent::Activated,
                ZoneLinkEvent::Opened => LinkEvent::Opened,
                ZoneLinkEvent::Closed => LinkEvent::Closed,
                ZoneLinkEvent::Triggered => LinkEvent::Triggered,
            });
            let mut link = EntityLink::new(
                authored_link.link_type.clone(),
                placed_entity_id(map_id, &placement.zone, &authored_link.source)?,
                placed_entity_id(map_id, &placement.zone, &authored_link.target)?,
                events,
            );
            link.action = match link_contract.action {
                AuthoredLinkAction::ToggleDoor => LinkAction::ToggleDoor,
                AuthoredLinkAction::OpenDoor => LinkAction::OpenDoor,
                AuthoredLinkAction::CloseDoor => LinkAction::CloseDoor,
            };
            link.enabled = authored_link.enabled;
            link.persistence = PersistencePolicy::StaticDefinition;
            link.cooldown_ticks = authored_link.cooldown_ticks;
            zone.add_link(link)?;
        }
        zones.push(AuthoredZoneRuntime {
            map: map_id.clone(),
            definition: placement.zone.clone(),
            instance,
            max_players,
            entry_point,
            travel_triggers: Vec::new(),
            zone,
        });
    }
    for index in 0..zones.len() {
        let source_id = zones[index].definition.clone();
        let triggers = content
            .set
            .definitions
            .get(&source_id)
            .ok_or_else(|| EngineError::not_found("zone definition", &source_id))?
            .travel_triggers
            .clone();
        for trigger in triggers {
            let Some((x, z)) = zones[index].zone.nav().cell_of(trigger.position) else {
                return Err(invalid_zone_field(&source_id, "travel_triggers.position"));
            };
            if zones[index].zone.nav().is_blocked(x, z)
                || !zones[index].zone.config().contains(trigger.position)
            {
                return Err(invalid_zone_field(&source_id, "travel_triggers.position"));
            }
            let destination = zones
                .iter()
                .find(|zone| zone.definition == trigger.destination)
                .ok_or_else(|| {
                    EngineError::new(
                        ErrorCode::InvalidRequest,
                        "travel trigger destination is not placed on this map",
                    )
                    .with_detail("zone", source_id.to_string())
                    .with_detail("destination", trigger.destination.to_string())
                })?;
            let resolved = ResolvedTravelTrigger {
                id: trigger.id,
                position: trigger.position,
                radius_m: trigger.radius_m,
                destination_zone: destination.instance,
                arrival_point: destination.entry_point,
            };
            zones[index].travel_triggers.push(resolved);
        }
    }
    Ok(zones)
}

fn zone_instance_id(map: &DefinitionId, zone: &DefinitionId) -> Result<StableId, EngineError> {
    let digest = sha256_hex(format!("lynx-zone-instance-v1:{map}:{zone}").as_bytes());
    let random = u128::from_str_radix(&digest[..20], 16).map_err(|_| {
        EngineError::new(
            ErrorCode::Internal,
            "the authored zone instance ID could not be derived",
        )
    })?;
    Ok(StableId::from_parts(IdKind::Zone, 1, random))
}

fn placed_entity_id(
    map: &DefinitionId,
    zone: &DefinitionId,
    placement: &str,
) -> Result<StableId, EngineError> {
    let digest = sha256_hex(format!("lynx-authored-entity-v1:{map}:{zone}:{placement}").as_bytes());
    let random = u128::from_str_radix(&digest[..20], 16).map_err(|_| {
        EngineError::new(
            ErrorCode::Internal,
            "an authored entity ID could not be derived",
        )
    })?;
    Ok(StableId::from_parts(IdKind::Entity, 1, random))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the core.interactable schema limits range_m to 20 meters"
)]
fn build_interactable(
    data: &serde_json::Map<String, serde_json::Value>,
    definition: &DefinitionId,
) -> Result<Interactable, EngineError> {
    let range_m = data
        .get("range_m")
        .and_then(serde_json::Value::as_f64)
        .map(|value| value as f32)
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| {
            EngineError::new(
                ErrorCode::InvalidRequest,
                "an authored interactable has an invalid range",
            )
            .with_detail("entity", definition.to_string())
            .with_detail("component", "core.interactable.range_m")
        })?;
    let verb_names = data
        .get("verbs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_entity_component(definition, "core.interactable.verbs"))?;
    let mut verbs = Vec::with_capacity(verb_names.len());
    for value in verb_names {
        let name = value
            .as_str()
            .ok_or_else(|| invalid_entity_component(definition, "core.interactable.verbs"))?;
        let verb = Verb::parse(name)
            .ok_or_else(|| invalid_entity_component(definition, "core.interactable.verbs"))?;
        verbs.push(verb);
    }
    let requires_line_of_sight = data
        .get("requires_line_of_sight")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            invalid_entity_component(definition, "core.interactable.requires_line_of_sight")
        })?;
    let mut interactable = Interactable::new(range_m, verbs);
    interactable.requires_line_of_sight = requires_line_of_sight;
    Ok(interactable)
}

fn build_door(
    data: &serde_json::Map<String, serde_json::Value>,
    definition: &DefinitionId,
) -> Result<Door, EngineError> {
    let open = data
        .get("open")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| invalid_entity_component(definition, "core.door.open"))?;
    let mut door = Door::new();
    door.open = open;
    Ok(door)
}

fn build_health(
    data: &serde_json::Map<String, serde_json::Value>,
    definition: &DefinitionId,
) -> Result<i32, EngineError> {
    data.get("max")
        .and_then(serde_json::Value::as_i64)
        .and_then(|maximum| i32::try_from(maximum).ok())
        .filter(|maximum| *maximum > 0)
        .ok_or_else(|| invalid_entity_component(definition, "core.health.max"))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the core.movement schema bounds radius_m to 10 meters"
)]
fn build_movement(
    data: &serde_json::Map<String, serde_json::Value>,
    definition: &DefinitionId,
) -> Result<MovementProfile, EngineError> {
    let radius_m = data
        .get("radius_m")
        .and_then(serde_json::Value::as_f64)
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid_entity_component(definition, "core.movement.radius_m"))?;
    let occupies = data
        .get("occupies")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| invalid_entity_component(definition, "core.movement.occupies"))?;
    let mut movement = MovementProfile::new(radius_m);
    movement.occupies = occupies;
    movement.validate()?;
    Ok(movement)
}

fn build_ai_mind(
    data: &serde_json::Map<String, serde_json::Value>,
    definition: &DefinitionId,
    content: &ContentSet,
) -> Result<Mind, EngineError> {
    let behavior_text = data
        .get("behavior")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_entity_component(definition, "core.behavior.behavior"))?;
    let behavior = DefinitionId::new(behavior_text)?;
    let Some(behavior_definition) = content.definitions.get(&behavior) else {
        return Err(EngineError::not_found("behavior definition", &behavior));
    };
    if behavior_definition.kind() != ContentKind::Behavior {
        return Err(invalid_entity_component(
            definition,
            "core.behavior.behavior",
        ));
    }
    Ok(Mind::ai(behavior))
}

fn invalid_entity_component(definition: &DefinitionId, field: &str) -> EngineError {
    EngineError::new(
        ErrorCode::InvalidRequest,
        "an authored entity has invalid runtime component data",
    )
    .with_detail("entity", definition.to_string())
    .with_detail("field", field)
}

fn required_usize(
    data: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    zone: &DefinitionId,
) -> Result<usize, EngineError> {
    data.get(field)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_zone_field(zone, field))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "zone cell dimensions are capped at 512 and cell size at 64 meters"
)]
fn required_f32(
    data: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    zone: &DefinitionId,
) -> Result<f32, EngineError> {
    data.get(field)
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid_zone_field(zone, field))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "validated zone component values are finite and bounded"
)]
fn optional_f32(data: &serde_json::Map<String, serde_json::Value>, field: &str) -> Option<f32> {
    data.get(field)
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value as f32)
        .filter(|value| value.is_finite())
}

fn invalid_zone_field(zone: &DefinitionId, field: &str) -> EngineError {
    EngineError::new(
        ErrorCode::InvalidRequest,
        "a placed zone has invalid runtime configuration",
    )
    .with_detail("zone", zone.to_string())
    .with_detail("field", field)
}

/// Result of preparing a complete content-migration candidate without writing it.
#[derive(Debug)]
pub struct PreparedInstanceMigration {
    /// Exact pre-migration snapshot, retained to preserve quarantined payloads
    /// and to form the rollback pair at commit time.
    pub source: WorldSnapshot,
    /// Candidate snapshot containing only migrated, target-valid entities.
    pub candidate: WorldSnapshot,
    /// Diagnostics for entity and link records isolated from the candidate.
    pub quarantined: Vec<QuarantinedRecord>,
    /// Number of entities with at least one transformed component.
    pub migrated_entities: usize,
}

/// Build a dry-run candidate for one exact persisted-instance transition.
///
/// The source snapshot is never mutated. A matching component is transformed
/// by the reviewed plan and then prepared against the target registry. Any
/// missing prototype, unknown component, schema mismatch, or failed
/// precondition quarantines the complete entity; links touching quarantined
/// entities are also removed from the candidate. The original complete
/// snapshot remains available in `source` for the persistence checkpoint.
///
/// # Errors
/// Returns an error when the plan does not match the source snapshot and target
/// bundle exactly, or when the plan itself is invalid.
pub fn prepare_instance_migration(
    source: &WorldSnapshot,
    plan: &PersistedInstanceMigrationPlan,
    target: &RuntimeContent,
    now_ms: i64,
) -> Result<PreparedInstanceMigration, EngineError> {
    plan.validate().map_err(|error| {
        EngineError::new(
            ErrorCode::InvalidRequest,
            "persisted instance migration plan is invalid",
        )
        .with_detail("reason", error.to_string())
    })?;
    let source_version = source.content_version.as_deref().ok_or_else(|| {
        EngineError::new(
            ErrorCode::VersionMismatch,
            "the source snapshot has no content version to migrate",
        )
    })?;
    let source_manifest = source.content_manifest_sha256.as_deref().ok_or_else(|| {
        EngineError::new(
            ErrorCode::VersionMismatch,
            "the source snapshot has no manifest identity to migrate",
        )
    })?;
    if source_version != plan.from_content_version.to_string()
        || source_manifest != plan.source_manifest_sha256
        || target.manifest.content_version != plan.to_content_version
        || target.manifest_sha256 != plan.target_manifest_sha256
    {
        return Err(EngineError::new(
            ErrorCode::VersionMismatch,
            "persisted instance migration does not match the source snapshot and target bundle",
        )
        .with_detail("source_content_version", source_version)
        .with_detail("source_manifest_sha256", source_manifest)
        .with_detail(
            "plan_source_content_version",
            plan.from_content_version.to_string(),
        )
        .with_detail("plan_source_manifest_sha256", &plan.source_manifest_sha256)
        .with_detail(
            "target_content_version",
            target.manifest.content_version.to_string(),
        )
        .with_detail(
            "plan_target_content_version",
            plan.to_content_version.to_string(),
        )
        .with_detail("target_manifest_sha256", &target.manifest_sha256)
        .with_detail("plan_target_manifest_sha256", &plan.target_manifest_sha256));
    }

    let mut candidate = source.clone();
    let mut retained = Vec::with_capacity(candidate.zone.entities.len());
    let mut quarantined = Vec::new();
    let mut quarantined_entities = std::collections::BTreeSet::new();
    let mut migrated_entities = 0;
    for mut entity in candidate.zone.entities.drain(..) {
        match migrate_entity_components(&mut entity, plan, target) {
            Ok(changed) => {
                migrated_entities += usize::from(changed);
                retained.push(entity);
            }
            Err(reason) => {
                quarantined_entities.insert(entity.id);
                quarantined.push(QuarantinedRecord {
                    section: "entity".to_owned(),
                    entity: Some(entity.id),
                    code: "content.migration_failed".to_owned(),
                    message: reason,
                });
            }
        }
    }
    candidate.zone.entities = retained;
    candidate.zone.links.retain(|link| {
        let affected = quarantined_entities.contains(&link.source)
            || quarantined_entities.contains(&link.target);
        if affected {
            quarantined.push(QuarantinedRecord {
                section: "link".to_owned(),
                entity: Some(link.source),
                code: "content.migration_dependency".to_owned(),
                message: format!(
                    "link {} -> {} references an entity quarantined by content migration",
                    link.source, link.target
                ),
            });
        }
        !affected
    });
    candidate.content_version = Some(target.manifest.content_version.to_string());
    candidate.content_manifest_sha256 = Some(target.manifest_sha256.clone());
    candidate.saved_at_ms = now_ms;
    candidate.attach_content_migration_checkpoint(
        source,
        plan.id.clone(),
        quarantined.clone(),
        migrated_entities,
    )?;

    Ok(PreparedInstanceMigration {
        source: source.clone(),
        candidate,
        quarantined,
        migrated_entities,
    })
}

fn migrate_entity_components(
    entity: &mut EntityRecord,
    plan: &PersistedInstanceMigrationPlan,
    target: &RuntimeContent,
) -> Result<bool, String> {
    if !target.set.definitions.contains_key(&entity.prototype) {
        return Err(format!(
            "prototype {} is absent from the target content bundle",
            entity.prototype
        ));
    }
    let mut changed = false;
    for component in entity.components.values_mut() {
        if let Some(migrated) = plan
            .migrate_component(&entity.prototype, component)
            .map_err(|error| error.to_string())?
        {
            changed |= migrated.version != component.version || migrated.data != component.data;
            *component = migrated;
        }
        let prepared = target
            .component_registry
            .prepare(component)
            .map_err(|error| error.message().to_owned())?;
        if !prepared.issues.is_empty() {
            let first = &prepared.issues[0];
            return Err(format!("{}: {}", first.code, first.message));
        }
        changed |= prepared.instance.version != component.version
            || prepared.instance.data != component.data;
        *component = prepared.instance;
    }
    Ok(changed)
}

#[derive(Debug)]
struct LoadedFile {
    id: DefinitionId,
    kind: ContentKind,
    version: SchemaVersion,
}

/// Load a bundle directory, validate authored data, and verify every manifest
/// path, identity, schema version, and file hash against its loaded contents.
///
/// # Errors
///
/// Returns an error when the manifest or content is invalid, engine versions
/// are incompatible, or a manifest file is missing or differs from its hash.
pub fn load_bundle(root: impl AsRef<Path>) -> Result<RuntimeContent> {
    let root = root.as_ref();
    let manifest_path = ContentManifest::path_in(root);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("failed to read content bundle {}", manifest_path.display()))?;
    let manifest: ContentManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("failed to read content bundle {}", manifest_path.display()))?;
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    let manifest_report = manifest.validate();
    if manifest_report.has_errors() {
        bail!("content manifest validation failed: {}", manifest_report);
    }
    let engine_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .context("lynx-world has an invalid package version")?;
    ensure!(
        manifest.engine.matches(&engine_version),
        "content bundle requires engine {}, running engine is {}",
        manifest.engine,
        engine_version
    );

    let component_registry = starter_component_registry()?;
    let loader = ContentLoader::new(component_registry.clone());
    let (set, report) = loader.load_project(root);
    if report.has_errors() {
        bail!("content bundle validation failed: {report}");
    }

    let mut loaded_files = BTreeMap::<PathBuf, LoadedFile>::new();
    for (id, definition) in &set.definitions {
        loaded_files.insert(
            PathBuf::from(&set.sources[id]),
            LoadedFile {
                id: id.clone(),
                kind: definition.kind(),
                version: definition.version,
            },
        );
    }
    for (id, panel) in &set.panels {
        loaded_files.insert(
            PathBuf::from(&set.panel_sources[id]),
            LoadedFile {
                id: id.clone(),
                kind: ContentKind::Ui,
                version: panel.version,
            },
        );
    }
    for (id, minigame) in &set.minigames {
        loaded_files.insert(
            PathBuf::from(&set.minigame_sources[id]),
            LoadedFile {
                id: id.clone(),
                kind: ContentKind::Minigame,
                version: minigame.version,
            },
        );
    }
    for (id, map) in &set.maps {
        loaded_files.insert(
            PathBuf::from(&set.map_sources[id]),
            LoadedFile {
                id: id.clone(),
                kind: ContentKind::Map,
                version: map.version,
            },
        );
    }
    ensure!(
        loaded_files.len() == manifest.definitions.len(),
        "content manifest definition list does not match the loaded content files"
    );
    for entry in &manifest.definitions {
        let relative_path = PathBuf::from(&entry.path);
        let loaded = loaded_files.get(&relative_path).with_context(|| {
            format!(
                "manifest entry {} is not a loaded content definition",
                entry.path
            )
        })?;
        ensure!(
            loaded.id == entry.id && loaded.kind == entry.kind && loaded.version == entry.version,
            "content manifest metadata does not match {}",
            entry.path
        );
        let bytes = std::fs::read(root.join(&relative_path))
            .with_context(|| format!("failed to read content file {}", entry.path))?;
        ensure!(
            sha256_hex(&bytes) == entry.sha256,
            "content hash does not match manifest entry {}",
            entry.path
        );
    }

    ensure!(
        set.catalog_paths.len() == manifest.locales.len(),
        "content manifest locale list does not match the loaded catalogs"
    );
    for entry in &manifest.locales {
        let relative_path = set
            .catalog_paths
            .get(&entry.locale)
            .with_context(|| format!("manifest locale {} is not loaded", entry.locale))?;
        ensure!(
            relative_path == &entry.path,
            "content manifest locale path does not match {}",
            entry.locale
        );
        let bytes = std::fs::read(root.join(relative_path))
            .with_context(|| format!("failed to read locale catalog {}", entry.path))?;
        ensure!(
            sha256_hex(&bytes) == entry.sha256,
            "content hash does not match manifest locale {}",
            entry.path
        );
    }

    let version = ContentVersionSnapshot::from_loaded_manifest(&manifest)
        .context("content version exceeds the supported 128-byte limit")?;
    Ok(RuntimeContent {
        set,
        component_registry,
        manifest,
        manifest_sha256,
        version,
    })
}
