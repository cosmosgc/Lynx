use std::sync::Arc;

use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_sim::entity::EntityRecord;
use engine_sim::mind::{ControlCapability, Mind};
use engine_sim::movement::MovementIntent;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};

fn id(kind: IdKind, n: u128) -> StableId {
    StableId::from_parts(kind, 1, n)
}
fn pos(x: f32, z: f32) -> Vec3 {
    Vec3::new(x, 0.0, z)
}
fn setup() -> Zone {
    let config = ZoneConfig::new(
        DefinitionId::new("zone.test").unwrap(),
        id(IdKind::Zone, 1),
        pos(0.0, 0.0),
        Vec3::new(4.0, 1.0, 4.0),
    )
    .unwrap();
    let mut nav = NavGrid::new(4, 4, 1.0, Vec3::ZERO).unwrap();
    nav.set_blocked(2, 0, true);
    let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap();
    zone.spawn(
        EntityRecord::new(
            id(IdKind::Entity, 1),
            DefinitionId::new("player.test").unwrap(),
            pos(0.5, 0.5),
        )
        .with_health(10)
        .with_mind(Mind::player(id(IdKind::Character, 1), "session")),
    )
    .unwrap();
    zone
}
fn intent(sequence: u64, destination: Vec3) -> MovementIntent {
    MovementIntent {
        actor: id(IdKind::Entity, 1),
        principal: id(IdKind::Character, 1),
        session: "session".into(),
        sequence,
        destination,
    }
}

#[test]
fn authorization_budget_walls_and_revalidation() {
    let mut z = setup();
    let mut other = intent(1, pos(1.5, 0.5));
    other.principal = id(IdKind::Character, 2);
    assert!(z.queue_movement(other).is_err());
    z.queue_movement(intent(1, pos(1.5, 0.5))).unwrap();
    assert!(z.queue_movement(intent(2, pos(1.5, 0.5))).is_err());
    assert!(!z.advance_movement_tick(499, 2.0).1[0].applied);
    assert!(z.queue_movement(intent(1, pos(1.5, 0.5))).is_err());
    z.queue_movement(intent(2, pos(1.5, 0.5))).unwrap();
    assert!(z.advance_movement_tick(500, 2.0).1[0].applied);
    z.queue_movement(intent(3, pos(2.5, 0.5))).unwrap();
    assert!(!z.advance_movement_tick(1000, 2.0).1[0].applied);
    z.queue_movement(intent(4, pos(1.5, 1.5))).unwrap();
    z.entity_mut(id(IdKind::Entity, 1))
        .unwrap()
        .health
        .as_mut()
        .unwrap()
        .current = 0;
    assert!(!z.advance_movement_tick(1000, 2.0).1[0].applied);
    assert!(z.queue_movement(intent(5, pos(1.5, 1.5))).is_err());
    let e = z.entity_mut(id(IdKind::Entity, 1)).unwrap();
    e.health.as_mut().unwrap().current = 10;
    e.mind
        .as_mut()
        .unwrap()
        .capabilities
        .remove(&ControlCapability::Move);
    assert!(z.queue_movement(intent(5, pos(1.5, 1.5))).is_err());
    assert_eq!(
        z.entity(id(IdKind::Entity, 1)).unwrap().position(),
        pos(1.5, 0.5)
    );
}

#[test]
fn identical_inputs_produce_identical_results() {
    let mut a = setup();
    let mut b = setup();
    for (seq, dest) in [(1, pos(1.0, 0.5)), (2, pos(1.0, 1.0)), (3, pos(2.0, 1.0))] {
        a.queue_movement(intent(seq, dest)).unwrap();
        b.queue_movement(intent(seq, dest)).unwrap();
        assert_eq!(
            a.advance_movement_tick(500, 2.0),
            b.advance_movement_tick(500, 2.0)
        );
        assert_eq!(
            a.entity(id(IdKind::Entity, 1)).unwrap().transform,
            b.entity(id(IdKind::Entity, 1)).unwrap().transform
        );
    }
}

#[test]
fn cancellation_is_session_scoped_and_keeps_sequence_consumed() {
    let mut z = setup();
    let actor = id(IdKind::Entity, 1);
    let principal = id(IdKind::Character, 1);
    z.queue_movement(intent(1, pos(1.5, 0.5))).unwrap();
    z.cancel_movement(actor, principal, "other-session");
    assert!(z.advance_movement_tick(1000, 2.0).1[0].applied);
    z.queue_movement(intent(2, pos(1.5, 1.5))).unwrap();
    z.cancel_movement(actor, principal, "session");
    assert_eq!(z.advance_movement_tick(1000, 2.0).1.len(), 0);
    assert!(z.queue_movement(intent(2, pos(1.5, 1.5))).is_err());
    assert_eq!(z.entity(actor).unwrap().position(), pos(1.5, 0.5));
}

#[test]
fn outcomes_and_exhausted_sequences_are_session_scoped() {
    let mut z = setup();
    let actor = id(IdKind::Entity, 1);
    let principal = id(IdKind::Character, 1);
    assert_eq!(z.last_movement_sequence(actor, principal, "session"), None);
    z.queue_movement(intent(u64::MAX, pos(3.5, 0.5))).unwrap();
    let (stats, results) = z.advance_movement_tick(50, 2.0);
    assert_eq!(results[0].tick, stats.tick);
    assert!(!results[0].applied);
    assert_eq!(results[0].position, pos(0.5, 0.5));
    assert_eq!(
        z.last_movement_sequence(actor, principal, "session"),
        Some(u64::MAX)
    );
    assert_eq!(z.last_movement_sequence(actor, principal, "other"), None);
    assert!(z.queue_movement(intent(u64::MAX, pos(0.5, 0.5))).is_err());
}
