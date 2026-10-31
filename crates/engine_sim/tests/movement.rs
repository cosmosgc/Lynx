use std::sync::Arc;

use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_sim::entity::{EntityRecord, MovementProfile, MovementStatus};
use engine_sim::mind::{ControlCapability, Mind};
use engine_sim::movement::MovementIntent;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{MOVEMENT_DIAGNOSTIC_LIMIT, Zone, ZoneConfig};

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

fn clear_setup() -> Zone {
    let config = ZoneConfig::new(
        DefinitionId::new("zone.clear_test").unwrap(),
        id(IdKind::Zone, 1),
        pos(0.0, 0.0),
        Vec3::new(4.0, 1.0, 4.0),
    )
    .unwrap();
    let nav = NavGrid::new(4, 4, 1.0, Vec3::ZERO).unwrap();
    let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap();
    spawn_player(
        &mut zone,
        1,
        1,
        "session",
        pos(0.5, 0.5),
        MovementProfile::default(),
    );
    zone
}

fn spawn_player(
    zone: &mut Zone,
    entity_seed: u128,
    character_seed: u128,
    session: &str,
    position: Vec3,
    movement: MovementProfile,
) -> StableId {
    let actor = id(IdKind::Entity, entity_seed);
    zone.spawn(
        EntityRecord::new(actor, DefinitionId::new("player.test").unwrap(), position)
            .with_health(10)
            .with_mind(Mind::player(id(IdKind::Character, character_seed), session))
            .with_movement(movement),
    )
    .unwrap();
    actor
}

fn player_intent(
    actor: StableId,
    character_seed: u128,
    session: &str,
    sequence: u64,
    destination: Vec3,
) -> MovementIntent {
    MovementIntent {
        actor,
        principal: id(IdKind::Character, character_seed),
        session: session.into(),
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
fn seeded_multi_entity_replay_has_byte_identical_snapshots() {
    const ACTORS: u128 = 8;
    const TICKS: u64 = 24;

    let mut forward = clear_setup();
    let mut reverse = clear_setup();
    let actor_ids: Vec<_> = (1..=ACTORS)
        .map(|seed| {
            let actor = id(IdKind::Entity, seed);
            if seed != 1 {
                spawn_player(
                    &mut forward,
                    seed,
                    seed,
                    &format!("session-{seed}"),
                    pos(0.5, 0.5),
                    MovementProfile::default(),
                );
            }
            actor
        })
        .collect();
    for seed in (2..=ACTORS).rev() {
        spawn_player(
            &mut reverse,
            seed,
            seed,
            &format!("session-{seed}"),
            pos(0.5, 0.5),
            MovementProfile::default(),
        );
    }

    // A small fixed-seed generator keeps this workload reproducible without
    // introducing a random dependency into the simulation crate.
    let mut state = 0x5eed_u64;
    let mut sequences = [0_u64; ACTORS as usize];
    for _ in 0..TICKS {
        for (index, actor) in actor_ids.iter().enumerate() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let x = f32::from(u8::try_from((state >> 32) % 4).unwrap()) + 0.5;
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let z = f32::from(u8::try_from((state >> 32) % 4).unwrap()) + 0.5;
            sequences[index] += 1;
            let sequence = sequences[index];
            let session = if index == 0 {
                "session".to_owned()
            } else {
                format!("session-{}", index + 1)
            };
            for zone in [&mut forward, &mut reverse] {
                zone.queue_movement(player_intent(
                    *actor,
                    index as u128 + 1,
                    &session,
                    sequence,
                    pos(x, z),
                ))
                .unwrap();
            }
        }

        let left = forward.advance_movement_tick(500, 2.0);
        let right = reverse.advance_movement_tick(500, 2.0);
        assert_eq!(left, right);
        let left_snapshot = serde_json::to_vec(&forward.snapshot()).unwrap();
        let right_snapshot = serde_json::to_vec(&reverse.snapshot()).unwrap();
        assert_eq!(left_snapshot, right_snapshot);
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

#[test]
fn stunned_and_zero_speed_actors_cannot_advance() {
    let mut z = clear_setup();
    let actor = id(IdKind::Entity, 1);
    let profile = |status, speed_multiplier| MovementProfile {
        status,
        speed_multiplier,
        ..MovementProfile::default()
    };
    z.entity_mut(actor).unwrap().movement = Some(profile(MovementStatus::Stunned, 1.0));
    assert!(z.queue_movement(intent(1, pos(1.0, 0.5))).is_err());

    z.entity_mut(actor).unwrap().movement = Some(profile(MovementStatus::Mobile, 1.0));
    z.queue_movement(intent(1, pos(1.0, 0.5))).unwrap();
    z.entity_mut(actor).unwrap().movement = Some(profile(MovementStatus::Stunned, 1.0));
    let (_, outcomes) = z.advance_movement_tick(500, 2.0);
    assert!(!outcomes[0].applied);
    assert_eq!(z.entity(actor).unwrap().position(), pos(0.5, 0.5));
    assert!(
        z.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "movement.rejected")
    );

    z.entity_mut(actor).unwrap().movement = Some(profile(MovementStatus::Mobile, 0.0));
    assert!(z.queue_movement(intent(2, pos(1.0, 0.5))).is_err());
    z.entity_mut(actor).unwrap().movement = Some(profile(MovementStatus::Mobile, 1.0));
    z.queue_movement(intent(2, pos(1.0, 0.5))).unwrap();
    assert!(z.advance_movement_tick(500, 2.0).1[0].applied);
}

#[test]
fn only_controlled_entities_can_move() {
    let mut z = clear_setup();
    let actor = id(IdKind::Entity, 1);
    let prop = id(IdKind::Entity, 9);
    z.spawn(
        EntityRecord::new(
            prop,
            DefinitionId::new("prop.crate").unwrap(),
            pos(2.5, 2.5),
        )
        .with_movement(MovementProfile::default()),
    )
    .unwrap();
    assert!(
        z.queue_movement(player_intent(prop, 9, "session", 1, pos(3.5, 2.5)))
            .is_err()
    );

    z.queue_movement(intent(1, pos(1.0, 0.5))).unwrap();
    z.entity_mut(actor).unwrap().mind = None;
    assert!(!z.advance_movement_tick(500, 2.0).1[0].applied);
    assert_eq!(z.entity(actor).unwrap().position(), pos(0.5, 0.5));

    z.entity_mut(actor).unwrap().mind = Some(Mind::player(id(IdKind::Character, 1), "session"));
    assert!(
        z.queue_movement(MovementIntent {
            session: "other-session".into(),
            ..intent(2, pos(1.0, 0.5))
        })
        .is_err()
    );
}

#[test]
fn destination_cell_occupancy_and_body_collision_reject_moves() {
    let mut z = clear_setup();
    let actor = id(IdKind::Entity, 1);
    let other = spawn_player(
        &mut z,
        2,
        2,
        "other",
        pos(2.9, 2.9),
        MovementProfile::default(),
    );

    // The destination cell is occupied even though the bodies would not touch.
    z.entity_mut(actor).unwrap().transform.position = pos(1.5, 1.5);
    z.queue_movement(intent(1, pos(2.1, 2.1))).unwrap();
    assert!(!z.advance_movement_tick(500, 2.0).1[0].applied);
    z.entity_mut(other).unwrap().movement = Some(MovementProfile {
        occupies: false,
        ..MovementProfile::default()
    });
    z.queue_movement(intent(2, pos(2.1, 2.1))).unwrap();
    assert!(z.advance_movement_tick(500, 2.0).1[0].applied);

    // A body intersects the segment although the destination cell is clear.
    z.entity_mut(actor).unwrap().transform.position = pos(0.5, 0.5);
    z.entity_mut(other).unwrap().transform.position = pos(1.5, 0.5);
    z.entity_mut(other).unwrap().movement = Some(MovementProfile::new(0.5));
    z.queue_movement(intent(3, pos(1.2, 1.2))).unwrap();
    assert!(!z.advance_movement_tick(500, 2.0).1[0].applied);
    z.entity_mut(other).unwrap().movement = Some(MovementProfile {
        occupies: false,
        radius_m: 0.5,
        ..MovementProfile::default()
    });
    z.queue_movement(intent(4, pos(1.2, 1.2))).unwrap();
    assert!(z.advance_movement_tick(500, 2.0).1[0].applied);
    assert_eq!(z.entity(actor).unwrap().position(), pos(1.2, 1.2));
    // The rejected actor never moved onto the body.
    assert_eq!(z.entity(other).unwrap().position(), pos(1.5, 0.5));
}

#[test]
fn contested_destinations_have_one_stable_winner() {
    let mut a = clear_setup();
    let mut b = clear_setup();
    for zone in [&mut a, &mut b] {
        spawn_player(
            zone,
            2,
            2,
            "other",
            pos(2.5, 0.5),
            MovementProfile::default(),
        );
    }
    let first = intent(1, pos(1.5, 1.5));
    let second = player_intent(id(IdKind::Entity, 2), 2, "other", 1, pos(1.5, 1.5));
    for zone in [&mut a, &mut b] {
        zone.queue_movement(first.clone()).unwrap();
        zone.queue_movement(second.clone()).unwrap();
    }
    let (stats_a, outcomes_a) = a.advance_movement_tick(1_000, 2.0);
    let (stats_b, outcomes_b) = b.advance_movement_tick(1_000, 2.0);
    assert_eq!(stats_a, stats_b);
    assert_eq!(outcomes_a, outcomes_b);
    assert!(outcomes_a[0].applied);
    assert!(!outcomes_a[1].applied);
    assert_eq!(outcomes_a[0].position, pos(1.5, 1.5));
    assert_eq!(outcomes_a[1].position, pos(2.5, 0.5));
    assert_eq!(
        a.entity(id(IdKind::Entity, 1)).unwrap().position(),
        b.entity(id(IdKind::Entity, 1)).unwrap().position()
    );
    assert_eq!(
        a.entity(id(IdKind::Entity, 2)).unwrap().position(),
        b.entity(id(IdKind::Entity, 2)).unwrap().position()
    );
}

#[test]
fn movement_diagnostics_are_bounded_and_destination_free() {
    let mut z = clear_setup();
    let actor = id(IdKind::Entity, 1);
    for sequence in 1..=MOVEMENT_DIAGNOSTIC_LIMIT as u64 + 44 {
        z.queue_movement(intent(sequence, pos(1.5, 0.5))).unwrap();
        assert!(!z.advance_movement_tick(0, 2.0).1[0].applied);
    }
    let diagnostics: Vec<_> = z
        .diagnostics()
        .into_iter()
        .filter(|diagnostic| diagnostic.code == "movement.rejected")
        .collect();
    assert_eq!(diagnostics.len(), MOVEMENT_DIAGNOSTIC_LIMIT);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.entity == Some(actor))
    );
    assert!(diagnostics.iter().all(|diagnostic| {
        !diagnostic.message.contains("1.5") && !diagnostic.message.contains("destination")
    }));
    assert_eq!(z.entity(actor).unwrap().position(), pos(0.5, 0.5));
}
