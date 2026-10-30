//! The built-in demo zone.
//!
//! One character, one door, one barrel, one owner-only crate, and one locked
//! vault are spawned per configured token. Characters are spaced far enough
//! apart that interest filtering is visible with the default view radius: a
//! player does not receive another player's props, and the owner-only crate
//! never appears in a stranger's snapshot. The vault opens a server-validated
//! lockpick minigame instead of reporting a locked door.
//!
//! Every demo entity is marked `PersistentEntity` so a whole-world snapshot
//! restores it; when a saved snapshot exists, [`restore_zone`] rebuilds the
//! zone from it instead of spawning fresh entities.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use engine_core::error::{EngineError, ErrorCode};
use engine_core::faction::{
    Faction, FactionDefinition, FactionWorld, GovernancePolicy, PowerRules, TerritoryRules,
};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::{ItemRules, UniformItemRules};
use engine_core::math::Vec3;
use engine_core::schema::PersistencePolicy;
use engine_core::time::{DurationMs, Millis};
use engine_core::world_event::{
    CancellationPolicy, ConflictRule, EnrollmentRules, EventSchedule, EventSide, EventSideRole,
    WORLD_EVENT_SCHEMA_VERSION, WorldCalendar, WorldEvent, WorldEventStatus,
};
use engine_net::protocol::Handshake;
use engine_sim::action::Verb;
use engine_sim::combat::CombatStats;
use engine_sim::entity::{Door, EntityRecord, Interactable, UiProvider};
use engine_sim::mind::Mind;
use engine_sim::minigame::Minigame;
use engine_sim::nav::NavGrid;
use engine_sim::snapshot::ZoneSnapshot;
use engine_sim::zone::{Zone, ZoneConfig};
use world_gateway::VISIBILITY_OWNER_ONLY_TAG;

use crate::auth::Grant;

/// The stable row name the runnable service uses for its world snapshot.
pub const DEMO_WORLD_ID: &str = "demo";

/// The durable zone instance every demo world serves.
///
/// The ID is fixed so the zone lease, the snapshot rows, and the event log name
/// the same zone before any snapshot is loaded.
#[must_use]
pub fn zone_instance() -> StableId {
    StableId::from_parts(IdKind::Zone, 1, 1)
}

const CHARACTER_SPACING_M: f32 = 120.0;
const COLUMNS: usize = 6;
const BASE_HEIGHT_M: u16 = 240;
const WIDTH_M: u16 = 800;
const MAX_CHARACTERS: usize = 32;
/// The demo conflict opens for enrollment one minute before the fight.
const DEMO_CONFLICT_WINDOW_MS: i64 = 60_000;
/// Navigation cell size in meters, shared by the demo and its restored zones.
const DEMO_CELL_SIZE_M: f32 = 2.0;

/// The authoritative world-clock timestamp, in epoch milliseconds.
///
/// Authoritative systems take explicit time; nothing reads a clock inside the
/// simulation crates.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(i64::MAX, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Build the demo zone with one character per authenticated grant.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidRequest`] when no grant is configured or a
/// definition identifier is malformed, [`ErrorCode::CapacityExceeded`] above
/// [`MAX_CHARACTERS`], and propagates zone spawn failures.
pub fn build_zone(grants: &[Grant]) -> Result<Zone, EngineError> {
    if grants.len() > MAX_CHARACTERS {
        return Err(EngineError::new(
            ErrorCode::CapacityExceeded,
            "the demo zone supports at most 32 characters",
        )
        .with_detail("configured", grants.len().to_string())
        .with_detail("max", MAX_CHARACTERS.to_string()));
    }
    let rows = grants.len().div_ceil(COLUMNS).max(1);
    let rows = u16::try_from(rows)
        .map_err(|_| EngineError::new(ErrorCode::CapacityExceeded, "too many demo rows"))?;
    let height_m = BASE_HEIGHT_M + (rows - 1) * 120;

    let config = ZoneConfig::new(
        definition("zone.demo")?,
        zone_instance(),
        Vec3::ZERO,
        Vec3::new(f32::from(WIDTH_M), 4.0, f32::from(height_m)),
    )?;
    let nav = demo_nav(&config)?;
    let mut zone = Zone::new(config, nav, demo_rules())?;
    let definitions = DemoDefinitions {
        character: definition("character.player")?,
        door: definition("prop.door")?,
        barrel: definition("prop.barrel")?,
        owner_crate: definition("prop.owner_crate")?,
        vault: definition("prop.vault")?,
        lockpick_schema: definition("ui.lockpick")?,
        lockpick_minigame: definition("minigame.lockpick")?,
    };

    for (index, grant) in grants.iter().enumerate() {
        let column = u16::try_from(index % COLUMNS).expect("column fits in u16");
        let row = u16::try_from(index / COLUMNS).expect("row fits in u16");
        let position = Vec3::new(
            60.0 + f32::from(column) * CHARACTER_SPACING_M,
            0.0,
            60.0 + f32::from(row) * CHARACTER_SPACING_M,
        );
        spawn_grant(&mut zone, grant, position, &definitions)?;
    }

    Ok(zone)
}

/// Build a navigation grid covering `config` with the demo cell size.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidRequest`] when the bounds are too small for
/// even one cell.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the demo zone keeps positive whole-meter bounds, so the cell count is exact"
)]
fn demo_nav(config: &ZoneConfig) -> Result<NavGrid, EngineError> {
    let width_m = config.max.x - config.min.x;
    let height_m = config.max.z - config.min.z;
    NavGrid::new(
        (width_m / DEMO_CELL_SIZE_M) as usize,
        (height_m / DEMO_CELL_SIZE_M) as usize,
        DEMO_CELL_SIZE_M,
        config.min,
    )
    .ok_or_else(|| EngineError::new(ErrorCode::InvalidRequest, "demo navigation grid is invalid"))
}

/// Rebuild the demo zone from a saved snapshot.
///
/// The snapshot supplies the zone configuration, so a restored world keeps the
/// bounds it was saved with even when the configured token count has changed.
/// New tokens resolve to no character until the zone is reset.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidRequest`] for an incompatible snapshot version
/// or invalid bounds, and every error [`Zone::restore`] can produce.
pub fn restore_zone(snapshot: &ZoneSnapshot) -> Result<Zone, EngineError> {
    let nav = demo_nav(&snapshot.config)?;
    Zone::restore(snapshot, nav, demo_rules())
}

fn demo_rules() -> Arc<dyn ItemRules + Send + Sync> {
    Arc::new(UniformItemRules::new(99))
}

/// Content definitions the demo zone needs, resolved once at build time.
struct DemoDefinitions {
    character: DefinitionId,
    door: DefinitionId,
    barrel: DefinitionId,
    owner_crate: DefinitionId,
    vault: DefinitionId,
    lockpick_schema: DefinitionId,
    lockpick_minigame: DefinitionId,
}

/// Spawn one player's character and nearby props.
fn spawn_grant(
    zone: &mut Zone,
    grant: &Grant,
    position: Vec3,
    definitions: &DemoDefinitions,
) -> Result<(), EngineError> {
    let seed = grant.principal.random_bits();
    let actor = StableId::from_parts(IdKind::Entity, 1, seed);
    zone.spawn(
        EntityRecord::new(actor, definitions.character.clone(), position)
            .with_health(100)
            .with_combat(CombatStats::default())
            .with_mind(Mind::player(grant.principal, "unbound"))
            .with_persistence(PersistencePolicy::PersistentEntity),
    )?;

    zone.spawn(
        EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 1, seed ^ 0x100),
            definitions.door.clone(),
            position.translated(Vec3::new(2.0, 0.0, 0.0)),
        )
        .with_interactable(Interactable::new(3.0, [Verb::Open]).without_line_of_sight())
        .with_door(Door::new())
        .with_persistence(PersistencePolicy::PersistentEntity),
    )?;

    zone.spawn(
        EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 1, seed ^ 0x200),
            definitions.barrel.clone(),
            position.translated(Vec3::new(-2.0, 0.0, 2.0)),
        )
        .with_health(100)
        .with_interactable(
            Interactable::new(4.0, [Verb::Inspect, Verb::Attack]).without_line_of_sight(),
        )
        .with_persistence(PersistencePolicy::PersistentEntity),
    )?;

    zone.spawn(
        EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 1, seed ^ 0x300),
            definitions.owner_crate.clone(),
            position.translated(Vec3::new(0.0, 0.0, -2.0)),
        )
        .owned_by(grant.principal)
        .with_tag(VISIBILITY_OWNER_ONLY_TAG)
        .with_persistence(PersistencePolicy::PersistentEntity),
    )?;

    zone.spawn(
        EntityRecord::new(
            StableId::from_parts(IdKind::Entity, 1, seed ^ 0x400),
            definitions.vault.clone(),
            position.translated(Vec3::new(0.0, 0.0, 4.0)),
        )
        .with_interactable(
            Interactable::new(3.0, [Verb::Use, Verb::Play, Verb::Open]).without_line_of_sight(),
        )
        .with_door(Door {
            open: false,
            locked: true,
            key: None,
        })
        .with_ui(UiProvider::new(
            definitions.lockpick_schema.clone(),
            "Vault Lock",
        ))
        .with_minigame(Minigame::new(
            definitions.lockpick_minigame.clone(),
            3,
            8,
            100,
            random_seed()?,
        )?)
        .with_persistence(PersistencePolicy::PersistentEntity),
    )?;

    Ok(())
}

/// The server handshake advertised to clients.
///
/// # Errors
///
/// Returns an error only if the crate version is not valid semver.
pub fn handshake() -> Result<Handshake, EngineError> {
    let version = env!("CARGO_PKG_VERSION");
    let version = semver::Version::parse(version).map_err(|error| {
        EngineError::new(ErrorCode::Internal, "invalid crate version")
            .with_detail("error", error.to_string())
    })?;
    Ok(Handshake::new(version.clone(), version))
}

/// The faction of demo characters that defend the demo territory.
pub(crate) const DEMO_DEFENDERS: &str = "faction.guardas";
/// The faction of demo characters that attack the demo territory.
pub(crate) const DEMO_ATTACKERS: &str = "faction.assaltantes";
/// The one demo conflict, published and open at startup.
pub(crate) const DEMO_EVENT: &str = "event.assalto_praca";

pub(crate) fn system_actor() -> StableId {
    StableId::from_parts(IdKind::Character, 2, 1)
}

/// The distinct founder character for demo faction `seed`.
///
/// Founders become their faction's first member, so every demo faction uses
/// its own character.
fn faction_founder(seed: u128) -> StableId {
    StableId::from_parts(IdKind::Character, 2, 10 + seed)
}

/// Build the demo faction world with the demo territory owned by
/// [`DEMO_DEFENDERS`].
///
/// # Errors
///
/// Returns an error when a built-in identifier is malformed or a faction
/// operation is rejected.
pub fn build_faction_world(now_ms: i64) -> Result<FactionWorld, EngineError> {
    let now = Millis::new(now_ms);
    let rules = TerritoryRules {
        // The demo rotates rules fast so a full conflict cycle is reachable
        // within one session.
        claim_protection: DurationMs::new(0),
        capture_cooldown: DurationMs::new(0),
        minimum_capture_power: 0,
        ..TerritoryRules::default()
    };
    let mut world = FactionWorld::new(rules);
    // Each faction needs its own founder: becoming a founder makes the
    // character that faction's first member, and one character may belong to
    // at most one faction.
    for (faction_id, name, founder_seed) in [
        (definition(DEMO_DEFENDERS)?, "Guardas da Praça", 1u128),
        (definition(DEMO_ATTACKERS)?, "Assaltantes", 2),
    ] {
        world.add_faction(
            Faction::new(
                FactionDefinition::new(faction_id, name),
                GovernancePolicy::default(),
                PowerRules::default(),
                faction_founder(founder_seed),
                now,
            )
            .map_err(|error| {
                EngineError::new(ErrorCode::Internal, "the demo faction is invalid")
                    .with_detail("error", error.to_string())
            })?,
        )?;
    }
    world.register_territory(definition("zone.demo")?)?;
    // Claims require faction membership, so the defenders' founder — their
    // first member and leader — performs the demo's initial claim.
    world.claim_territory(
        "claim-demo",
        faction_founder(1),
        &definition(DEMO_DEFENDERS)?,
        &definition("zone.demo")?,
        now,
    )?;
    Ok(world)
}

/// Build the demo calendar: one bounded, open conflict over the demo zone.
///
/// Enrollment opens immediately and the conflict window is short, so a staff
/// session can drive `activate` and `resolve` within a single demo run.
///
/// # Errors
///
/// Returns an error when a built-in identifier is malformed or the calendar
/// rejects the demo event.
pub fn build_calendar(
    factions: &mut FactionWorld,
    now_ms: i64,
) -> Result<WorldCalendar, EngineError> {
    let author = system_actor();
    let now = Millis::new(now_ms);
    let mut calendar = WorldCalendar::new();
    let event = WorldEvent {
        id: definition(DEMO_EVENT)?,
        title: "Assalto à praça demo".into(),
        description: Some(
            "Um único conflito limitado entre Guardas e Assaltantes pela praça demo.".into(),
        ),
        schedule: EventSchedule {
            enrollment_opens_at: now,
            enrollment_closes_at: Millis::new(now_ms + DEMO_CONFLICT_WINDOW_MS),
            starts_at: Millis::new(now_ms + DEMO_CONFLICT_WINDOW_MS),
            ends_at: Millis::new(now_ms + 2 * DEMO_CONFLICT_WINDOW_MS),
        },
        target_zone: definition("zone.demo")?,
        sides: vec![
            EventSide {
                id: definition("side.assaltantes")?,
                role: EventSideRole::Attacker,
                display_name: "Assaltantes".into(),
                faction: definition(DEMO_ATTACKERS)?,
            },
            EventSide {
                id: definition("side.guardas")?,
                role: EventSideRole::Defender,
                display_name: "Guardas da Praça".into(),
                faction: definition(DEMO_DEFENDERS)?,
            },
        ],
        enrollment: EnrollmentRules {
            // The demo spawns unaffiliated characters; anyone may join either
            // side so a single player can exercise both roles across restarts.
            requires_faction_membership: false,
            allow_unaffiliated: true,
            max_participants_per_side: None,
        },
        conflict: ConflictRule::default(),
        cancellation: CancellationPolicy::AnyTimeBeforeResolution,
        status: WorldEventStatus::Draft,
        schema_version: WORLD_EVENT_SCHEMA_VERSION,
    };
    let event_id = event.id.clone();
    calendar.create(event, author, now)?;
    calendar.schedule(factions, &event_id, author, now)?;
    calendar.open(&event_id, author, now)?;
    Ok(calendar)
}

fn definition(value: &str) -> Result<DefinitionId, EngineError> {
    DefinitionId::new(value).map_err(|error| {
        EngineError::new(
            ErrorCode::Internal,
            "invalid built-in definition identifier",
        )
        .with_detail("error", error.to_string())
    })
}

/// Generate a server-held minigame seed from operating-system entropy.
///
/// The seed must not be derived from a client-visible value such as the
/// character's stable ID, or a modified client could compute the combination.
fn random_seed() -> Result<u64, EngineError> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| {
        EngineError::new(ErrorCode::Internal, "could not generate a minigame seed")
            .with_detail("error", error.to_string())
    })?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Authenticator, digest};

    fn grants(count: usize) -> Vec<Grant> {
        let hashes: Vec<[u8; 32]> = (0..count)
            .map(|index| digest(&format!("token-{index}")))
            .collect();
        Authenticator::new(&hashes)
            .unwrap()
            .grants()
            .cloned()
            .collect()
    }

    #[test]
    fn every_grant_gets_a_character_and_nearby_props() {
        let grants = grants(2);
        let zone = build_zone(&grants).unwrap();
        // Per grant: character, door, barrel, owner-only crate, locked vault.
        assert_eq!(zone.entity_count(), grants.len() * 5);
        for grant in &grants {
            let seed = grant.principal.random_bits();
            let actor = StableId::from_parts(IdKind::Entity, 1, seed);
            let entity = zone.entity(actor).expect("character is spawned");
            assert_eq!(
                entity.mind.as_ref().unwrap().controller.player_character(),
                Some(grant.principal)
            );
            assert!(entity.position().x >= 0.0);
        }
    }

    #[test]
    fn every_grant_gets_a_locked_vault_with_a_minigame() {
        let grants = grants(1);
        let zone = build_zone(&grants).unwrap();
        let seed = grants[0].principal.random_bits();
        let vault = zone
            .entity(StableId::from_parts(IdKind::Entity, 1, seed ^ 0x400))
            .expect("vault is spawned");
        assert!(vault.door.as_ref().unwrap().locked);
        assert_eq!(vault.ui.as_ref().unwrap().schema.as_str(), "ui.lockpick");
        let minigame = vault.minigame.as_ref().expect("vault has a minigame");
        assert_eq!(minigame.definition.as_str(), "minigame.lockpick");
        assert_eq!(minigame.length, 3);
        assert!(minigame.attempts > 0);
    }

    #[test]
    fn characters_are_far_enough_apart_for_a_sixty_meter_view() {
        let grants = grants(2);
        let zone = build_zone(&grants).unwrap();
        let first = StableId::from_parts(IdKind::Entity, 1, grants[0].principal.random_bits());
        let second = StableId::from_parts(IdKind::Entity, 1, grants[1].principal.random_bits());
        let distance = zone
            .entity(first)
            .unwrap()
            .position()
            .distance(zone.entity(second).unwrap().position());
        assert!(distance > 60.0, "characters must not share a view");
    }

    #[test]
    fn the_handshake_is_valid_semver() {
        let handshake = handshake().unwrap();
        assert_eq!(handshake.protocol, engine_net::protocol::protocol_version());
    }

    #[test]
    fn every_demo_entity_is_enduring_so_snapshots_keep_the_world() {
        let zone = build_zone(&grants(2)).unwrap();
        assert!(
            zone.entities()
                .all(|entity| entity.persistence == PersistencePolicy::PersistentEntity)
        );
    }

    #[test]
    fn a_demo_zone_round_trips_through_restore_zone() {
        let grants = grants(2);
        let mut built = build_zone(&grants).unwrap();
        for _ in 0..5 {
            built.advance_tick();
        }
        let snapshot = built.snapshot();

        let restored = restore_zone(&snapshot).unwrap();
        assert_eq!(restored.entity_count(), built.entity_count());
        assert_eq!(restored.tick(), built.tick());
        assert_eq!(restored.config(), &snapshot.config);
        let seed = grants[0].principal.random_bits();
        let vault = StableId::from_parts(IdKind::Entity, 1, seed ^ 0x400);
        assert!(
            restored
                .entity(vault)
                .unwrap()
                .door
                .as_ref()
                .unwrap()
                .locked
        );
    }
}
