//! Seeded, multi-tick determinism coverage across movement, combat effects, and
//! tick-driven AI.
//!
//! The two zones are built from identical server-owned inputs but spawn their
//! entities in opposite orders. Stable-ID iteration must make every observable
//! tick result byte-identical.

use std::sync::Arc;
use std::{fmt::Write as _, string::String};

use engine_core::faction::{Alignment, FactionCatalog, Thresholds};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_core::time::Millis;
use engine_sim::action::{ActionRequest, Verb};
use engine_sim::ai::{AiBehaviorDefinition, AiBehaviorKind, AiTickPolicy, PerceptionBudget};
use engine_sim::combat::{CombatStats, DamageType};
use engine_sim::entity::{EntityRecord, FactionMembership, Interactable};
use engine_sim::mind::Mind;
use engine_sim::movement::MovementIntent;
use engine_sim::nav::{NavGrid, ReplanPolicy};
use engine_sim::zone::{TickBudget, Zone, ZoneConfig};
use sha2::{Digest, Sha256};

const TICKS: u64 = 48;

fn id(kind: IdKind, value: u128) -> StableId {
    StableId::from_parts(kind, 1, value)
}

fn text(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn position(x: f32, z: f32) -> Vec3 {
    Vec3::new(x, 0.0, z)
}

fn faction(name: &str) -> DefinitionId {
    text(name)
}

fn catalog() -> FactionCatalog {
    let guards = faction("faction.guards");
    let raiders = faction("faction.raiders");
    let mut catalog = FactionCatalog::new();
    catalog.set_bidirectional(guards.clone(), guards.clone(), Alignment::new(80));
    catalog.set_bidirectional(raiders.clone(), raiders.clone(), Alignment::new(60));
    catalog.set(guards.clone(), raiders.clone(), Alignment::new(-90));
    catalog.set(raiders, guards, Alignment::new(-90));
    catalog
}

fn guard(seed: u128, x: f32, z: f32) -> EntityRecord {
    EntityRecord::new(id(IdKind::Entity, seed), text("npc.guard"), position(x, z))
        .with_health(200)
        .with_combat(CombatStats::new(3, 1))
        .with_mind(Mind::ai(text("behavior.guard")))
        .with_faction(FactionMembership::new(faction("faction.guards")))
        .with_interactable(Interactable::new(3.0, [Verb::Attack]))
}

fn raider(seed: u128, x: f32, z: f32) -> EntityRecord {
    EntityRecord::new(id(IdKind::Entity, seed), text("npc.raider"), position(x, z))
        .with_health(200)
        .with_combat(CombatStats::new(1, 1))
        .with_faction(FactionMembership::new(faction("faction.raiders")))
        .with_interactable(Interactable::new(3.0, [Verb::Attack]))
}

fn player() -> EntityRecord {
    EntityRecord::new(
        id(IdKind::Entity, 5),
        text("character.player"),
        position(9.5, 6.5),
    )
    .with_health(200)
    .with_combat(CombatStats::new(4, 1))
    .with_mind(Mind::player(id(IdKind::Character, 5), "player-session"))
    .with_faction(FactionMembership::new(faction("faction.raiders")))
    .with_interactable(Interactable::new(3.0, [Verb::Attack]))
}

fn setup(reverse_spawn: bool) -> Zone {
    let config = ZoneConfig::new(
        text("zone.determinism"),
        id(IdKind::Zone, 1),
        Vec3::ZERO,
        Vec3::new(16.0, 1.0, 16.0),
    )
    .unwrap()
    .with_max_entities(16)
    .with_budget(TickBudget {
        max_actions: 32,
        max_ai_updates: 4,
        max_path_expansions: 512,
        max_path_requests: 4,
    });
    let nav = NavGrid::new(16, 16, 1.0, Vec3::ZERO).unwrap();
    let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap();
    zone.set_ai_tick_policy(Some(AiTickPolicy {
        factions: catalog(),
        thresholds: Thresholds::default(),
        range_m: 8.0,
        perception: PerceptionBudget {
            max_candidates: 16,
            max_line_of_sight: 16,
            max_targets: 1,
        },
        max_controller_candidates: 16,
        max_group_aggro_shares: 16,
        epoch: Millis::new(0),
        tick_ms: 500,
        behaviors: vec![AiBehaviorDefinition {
            id: text("behavior.guard"),
            kind: AiBehaviorKind::Guard,
            range_m: 8.0,
            schedule: None,
            aggro_group: None,
            graph: None,
            patrol_route: Vec::new(),
        }],
        navigation_speed_m_per_s: 2.0,
        navigation_replan: ReplanPolicy::default(),
    }));

    let mut entities = vec![
        raider(1, 2.5, 2.5),
        raider(2, 6.5, 6.5),
        guard(3, 4.5, 2.5),
        guard(4, 8.5, 6.5),
        player(),
    ];
    if reverse_spawn {
        entities.reverse();
    }
    for entity in entities {
        zone.spawn(entity).unwrap();
    }

    // Server-owned timed effects exercise the effect phase on every tick.
    zone.apply_damage_over_time(
        id(IdKind::Entity, 1),
        id(IdKind::Entity, 5),
        40,
        2,
        2,
        DamageType::Fire,
    )
    .unwrap();
    zone.apply_shield(id(IdKind::Entity, 2), id(IdKind::Entity, 5), 40, 10)
        .unwrap();
    zone
}

#[test]
fn seeded_combat_and_ai_replay_is_byte_identical() {
    let mut forward = setup(false);
    let mut reverse = setup(true);

    for tick in 0..TICKS {
        let destination = if tick.is_multiple_of(2) {
            position(10.5, 6.5)
        } else {
            position(9.5, 6.5)
        };
        let intent = MovementIntent {
            actor: id(IdKind::Entity, 5),
            principal: id(IdKind::Character, 5),
            session: "player-session".into(),
            sequence: tick + 1,
            destination,
        };
        for zone in [&mut forward, &mut reverse] {
            zone.queue_movement(intent.clone()).unwrap();
        }

        if tick.is_multiple_of(4) {
            let request = ActionRequest::new(
                format!("determinism-attack-{tick}"),
                id(IdKind::Entity, 5),
                Verb::Attack,
            )
            .on(id(IdKind::Entity, 3))
            .by(id(IdKind::Character, 5));
            let now = Millis::new(i64::try_from(tick * 500).unwrap());
            let left = forward.apply_action(&request, now);
            let right = reverse.apply_action(&request, now);
            assert_eq!(left, right);
        }

        let left_stats = forward.advance_movement_tick(500, 2.0);
        let right_stats = reverse.advance_movement_tick(500, 2.0);
        assert_eq!(left_stats, right_stats, "tick {tick} stats diverged");
        assert_eq!(
            forward.ai_tick_report(),
            reverse.ai_tick_report(),
            "tick {tick} AI report diverged"
        );
        assert_eq!(
            forward.ai_diagnostics(),
            reverse.ai_diagnostics(),
            "tick {tick} AI diagnostics diverged"
        );
        assert_eq!(
            forward.combat_audit(),
            reverse.combat_audit(),
            "tick {tick} combat audit diverged"
        );
        assert_eq!(
            serde_json::to_vec(&forward.snapshot()).unwrap(),
            serde_json::to_vec(&reverse.snapshot()).unwrap(),
            "tick {tick} snapshot diverged"
        );
    }

    let snapshot = serde_json::to_vec(&forward.snapshot()).unwrap();
    let digest = Sha256::digest(snapshot);
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut fingerprint, "{byte:02x}").unwrap();
    }
    assert_eq!(
        fingerprint, "a55947cc6f2ae87aca8d124bb14d1d7e36606a6cea07cbe1a39138621e5c322e",
        "the seeded replay snapshot is the cross-platform golden"
    );
}

#[test]
fn repeated_runs_of_the_same_seed_match() {
    let run = || {
        let mut zone = setup(false);
        for tick in 0..TICKS {
            if tick.is_multiple_of(3) {
                let request = ActionRequest::new(
                    format!("repeat-attack-{tick}"),
                    id(IdKind::Entity, 5),
                    Verb::Attack,
                )
                .on(id(IdKind::Entity, 4))
                .by(id(IdKind::Character, 5));
                let now = Millis::new(i64::try_from(tick * 500).unwrap());
                let _ = zone.apply_action(&request, now);
            }
            zone.advance_movement_tick(500, 2.0);
        }
        serde_json::to_vec(&zone.snapshot()).unwrap()
    };

    assert_eq!(run(), run());
}
