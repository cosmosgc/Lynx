//! Loads the canonical calendar fixture and verifies recovery invariants.
//!
//! The fixture is durable server state: it records a scheduled siege, five
//! enrollments split across the two sides, the full transition chain, and an
//! attacker victory that captures the town square.

use engine_core::faction::{FactionSnapshot, FactionWorld};
use engine_core::time::Millis;
use engine_core::world_event::{
    CalendarSnapshot, ConflictOutcome, EventSideRole, WorldCalendar, WorldEventStatus,
};
use engine_core::{DefinitionId, ErrorCode, IdKind, StableId};

const CALENDAR: &str = include_str!("fixtures/calendar_v1.json");
const FACTIONS_AFTER_SIEGE: &str = include_str!("fixtures/faction_world_after_siege_v1.json");
const FACTIONS_BEFORE_SIEGE: &str = include_str!("fixtures/faction_world_v1.json");

fn id(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn actor(seed: u128) -> StableId {
    StableId::from_parts(IdKind::Character, 0, seed)
}

fn restored() -> (CalendarSnapshot, WorldCalendar, FactionWorld) {
    let snapshot: CalendarSnapshot =
        serde_json::from_str(CALENDAR).expect("fixture must be valid calendar JSON");
    snapshot
        .validate()
        .expect("fixture must pass recovery validation");
    let calendar = WorldCalendar::from_snapshot(snapshot.clone()).expect("fixture must rebuild");
    let factions: FactionSnapshot = serde_json::from_str(FACTIONS_AFTER_SIEGE).unwrap();
    let world = FactionWorld::from_snapshot(factions).unwrap();
    calendar
        .validate_against(&world)
        .expect("calendar must agree with the post-siege faction fixture");
    (snapshot, calendar, world)
}

#[test]
fn fixture_deserializes_validates_and_round_trips() {
    let (snapshot, calendar, _) = restored();
    assert_eq!(calendar.snapshot(), snapshot);

    let encoded = serde_json::to_string_pretty(&snapshot).unwrap();
    let reparsed: CalendarSnapshot = serde_json::from_str(&encoded).unwrap();
    assert_eq!(reparsed, snapshot);
}

#[test]
fn fixture_records_the_full_lifecycle_and_enrollment() {
    let (_, calendar, _) = restored();
    let event = id("event.siege_town_square");

    let definition = calendar.event(&event).expect("fixture event");
    assert_eq!(definition.status, WorldEventStatus::Resolved);
    assert_eq!(definition.sides.len(), 2);
    assert_eq!(calendar.transitions().len(), 5);
    assert_eq!(calendar.participants(&event, &id("side.attackers")), 3);
    assert_eq!(calendar.participants(&event, &id("side.defenders")), 2);
    assert_eq!(calendar.enrollments(&event).count(), 5);
    assert!(
        calendar
            .transitions()
            .windows(2)
            .all(|pair| pair[1].from == Some(pair[0].to))
    );
    assert_eq!(
        calendar
            .enrollment(&event, StableId::from_parts(IdKind::Entity, 0, 3))
            .unwrap()
            .faction,
        Some(id("faction.road_bandits"))
    );
    assert_eq!(calendar.enrollment(&event, actor(4)).unwrap().faction, None);
}

#[test]
fn fixture_attacker_victory_captured_the_target() {
    let (_, calendar, world) = restored();
    let event = id("event.siege_town_square");
    let bandits = id("faction.road_bandits");
    let resolution = calendar.resolution(&event).expect("fixture result");

    assert_eq!(resolution.outcome, ConflictOutcome::AttackerVictory);
    assert_eq!(resolution.winning_faction, bandits);
    assert_eq!(resolution.sides.len(), 2);
    assert_eq!(
        resolution
            .sides
            .iter()
            .find(|side| side.role == EventSideRole::Attacker)
            .unwrap()
            .score,
        3
    );
    assert_eq!(
        world.territory(&id("zone.town_square")).unwrap().owner,
        Some(bandits.clone())
    );
    assert_eq!(
        resolution.territory_change.as_ref().unwrap().to,
        Some(bandits)
    );
    assert_eq!(resolution.resolved_at, Millis::new(90_000_000));
}

#[test]
fn fixture_cross_validation_rejects_the_pre_siege_world() {
    let snapshot: CalendarSnapshot = serde_json::from_str(CALENDAR).unwrap();
    let calendar = WorldCalendar::from_snapshot(snapshot).unwrap();
    let factions: FactionSnapshot = serde_json::from_str(FACTIONS_BEFORE_SIEGE).unwrap();
    let world = FactionWorld::from_snapshot(factions).unwrap();
    assert_eq!(
        calendar.validate_against(&world).unwrap_err().code(),
        ErrorCode::InvalidState
    );
}

#[test]
fn fixture_recovery_rejects_tampered_results() {
    let (snapshot, _, _) = restored();

    let mut broken = snapshot.clone();
    broken.resolutions.clear();
    assert_eq!(
        WorldCalendar::from_snapshot(broken).unwrap_err().code(),
        ErrorCode::InvalidState
    );

    let mut broken = snapshot.clone();
    broken.resolutions[0].sides[0].participants = 4;
    assert_eq!(
        WorldCalendar::from_snapshot(broken).unwrap_err().code(),
        ErrorCode::InvalidState
    );

    let mut broken = snapshot;
    broken.transitions[2].from = Some(WorldEventStatus::Active);
    assert_eq!(
        WorldCalendar::from_snapshot(broken).unwrap_err().code(),
        ErrorCode::InvalidState
    );
}
