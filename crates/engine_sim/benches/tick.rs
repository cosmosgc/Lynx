//! Reproducible zone-tick latency report; run with
//! `cargo bench -p engine_sim --bench tick`.
//!
//! Each scenario is a fixed, seeded workload so the release p99 gate catches
//! broad simulation regressions rather than only an empty tick. AI scenarios
//! deliberately use the same bounded per-tick caps as the runtime; their cost
//! measures bounded work, not unbounded entity scaling.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use engine_core::faction::{Alignment, FactionCatalog, Thresholds};
use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_core::time::Millis;
use engine_sim::action::{ActionRequest, Verb};
use engine_sim::ai::{AiBehaviorDefinition, AiBehaviorKind, AiTickPolicy, PerceptionBudget};
use engine_sim::combat::CombatStats;
use engine_sim::entity::{EntityRecord, FactionMembership, Interactable};
use engine_sim::mind::Mind;
use engine_sim::movement::MovementIntent;
use engine_sim::nav::{NavGrid, ReplanPolicy};
use engine_sim::zone::{TickBudget, Zone, ZoneConfig};

const WARMUP_TICKS: usize = 100;
const SAMPLES: usize = 1_000;
/// The default server tick deadline; the generous absolute gate tolerates
/// ordinary shared-runner noise while still catching severe regressions.
const MAX_P99_TICK_NS: u128 = 50_000_000;
const TICK_MS: u32 = 500;
const SPEED_M_PER_S: f32 = 2.0;
const GRID_CELLS: usize = 64;

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    entity_counts: &'static [usize],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "idle",
        entity_counts: &[100, 1_000],
    },
    Scenario {
        name: "movement",
        entity_counts: &[100, 500],
    },
    Scenario {
        name: "combat",
        entity_counts: &[100, 1_000],
    },
    Scenario {
        name: "ai_guard",
        entity_counts: &[64, 256],
    },
    Scenario {
        name: "ai_chase",
        entity_counts: &[64, 256],
    },
];

fn stable_id(kind: IdKind, value: u128) -> StableId {
    StableId::from_parts(kind, 1, value)
}

fn text(value: &str) -> DefinitionId {
    DefinitionId::new(value).unwrap()
}

fn cell(value: usize) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let value = value as f32;
    value + 0.5
}

fn grid_extent() -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let extent = GRID_CELLS as f32;
    extent
}

fn base_zone(entity_count: usize, budget: TickBudget) -> Zone {
    let config = ZoneConfig::new(
        text("zone.tick_benchmark"),
        stable_id(IdKind::Zone, 1),
        Vec3::ZERO,
        Vec3::new(grid_extent(), 1.0, grid_extent()),
    )
    .unwrap()
    .with_max_entities(entity_count)
    .with_budget(budget);
    let nav = NavGrid::new(GRID_CELLS, GRID_CELLS, 1.0, Vec3::ZERO).unwrap();
    Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap()
}

fn idle_zone(entity_count: usize) -> Zone {
    let mut zone = base_zone(entity_count, TickBudget::default());
    for index in 0..entity_count {
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, index as u128 + 1),
                text("entity.tick_benchmark"),
                movement_origin(index),
            )
            .with_health(10),
        )
        .unwrap();
    }
    zone
}

fn movement_origin(index: usize) -> Vec3 {
    let column = index % 32;
    let row = index / 32;
    Vec3::new(cell(column * 2), 0.0, cell(row))
}

fn movement_destination(index: usize, tick: usize) -> Vec3 {
    let origin = movement_origin(index);
    if tick.is_multiple_of(2) {
        Vec3::new(origin.x + 1.0, origin.y, origin.z)
    } else {
        origin
    }
}

fn movement_zone(entity_count: usize) -> Zone {
    let budget = TickBudget {
        max_actions: entity_count,
        ..TickBudget::default()
    };
    let mut zone = base_zone(entity_count, budget);
    for index in 0..entity_count {
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, index as u128 + 1),
                text("character.tick_benchmark"),
                movement_origin(index),
            )
            .with_health(10)
            .with_mind(Mind::player(
                stable_id(IdKind::Character, index as u128 + 1),
                format!("session-{index}"),
            )),
        )
        .unwrap();
    }
    zone
}

fn combat_pair(index: usize) -> (Vec3, Vec3) {
    let column = index % 32;
    let row = index / 32;
    let actor = Vec3::new(cell(column * 2), 0.0, cell(row));
    let target = Vec3::new(actor.x + 1.0, 0.0, actor.z);
    (actor, target)
}

fn combat_zone(entity_count: usize) -> Zone {
    let mut zone = base_zone(entity_count, TickBudget::default());
    let pairs = entity_count / 2;
    for index in 0..pairs {
        let (actor_position, target_position) = combat_pair(index);
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, index as u128 + 1),
                text("character.tick_benchmark"),
                actor_position,
            )
            .with_health(1_000_000)
            .with_combat(CombatStats::new(2, 0))
            .with_mind(Mind::player(
                stable_id(IdKind::Character, index as u128 + 1),
                format!("session-{index}"),
            )),
        )
        .unwrap();
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, pairs as u128 + index as u128 + 1),
                text("npc.tick_benchmark"),
                target_position,
            )
            .with_health(1_000_000)
            .with_combat(CombatStats::new(0, 1))
            .with_interactable(Interactable::new(3.0, [Verb::Attack])),
        )
        .unwrap();
    }
    zone
}

fn faction(name: &str) -> DefinitionId {
    text(name)
}

fn ai_catalog() -> FactionCatalog {
    let guards = faction("faction.bench_guards");
    let raiders = faction("faction.bench_raiders");
    let mut catalog = FactionCatalog::new();
    catalog.set_bidirectional(guards.clone(), guards.clone(), Alignment::new(80));
    catalog.set_bidirectional(raiders.clone(), raiders.clone(), Alignment::new(60));
    catalog.set(guards.clone(), raiders.clone(), Alignment::new(-90));
    catalog.set(raiders, guards, Alignment::new(-90));
    catalog
}

fn ai_pair(index: usize, chase: bool) -> (Vec3, Vec3) {
    let column = index % 12;
    let row = index / 12;
    let raider = Vec3::new(cell(column * 4), 0.0, cell(row * 2));
    let offset = if chase { 5.0 } else { 2.0 };
    let guard = Vec3::new(raider.x + offset, 0.0, raider.z);
    (guard, raider)
}

fn ai_zone(entity_count: usize, chase: bool) -> Zone {
    let budget = TickBudget {
        max_actions: 64,
        max_ai_updates: 64,
        max_path_expansions: if chase { 4_096 } else { 0 },
        max_path_requests: if chase { 64 } else { 0 },
    };
    let mut zone = base_zone(entity_count, budget);
    let pairs = entity_count / 2;
    let behavior_id = if chase {
        text("behavior.bench_chase")
    } else {
        text("behavior.bench_guard")
    };
    zone.set_ai_tick_policy(Some(AiTickPolicy {
        factions: ai_catalog(),
        thresholds: Thresholds::default(),
        range_m: 8.0,
        perception: PerceptionBudget {
            max_candidates: 64,
            max_line_of_sight: 64,
            max_targets: 1,
        },
        max_controller_candidates: entity_count,
        max_group_aggro_shares: 0,
        epoch: Millis::new(0),
        tick_ms: TICK_MS,
        behaviors: vec![AiBehaviorDefinition {
            id: behavior_id.clone(),
            kind: if chase {
                AiBehaviorKind::GuardChase
            } else {
                AiBehaviorKind::Guard
            },
            range_m: 8.0,
            schedule: None,
            aggro_group: None,
            graph: None,
            patrol_route: Vec::new(),
        }],
        navigation_speed_m_per_s: SPEED_M_PER_S,
        navigation_replan: ReplanPolicy::default(),
    }));

    for index in 0..pairs {
        let (guard_position, raider_position) = ai_pair(index, chase);
        // Raiders keep lower IDs so a bounded perception scan reaches them
        // before the guard controllers it is evaluating.
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, index as u128 + 1),
                text("npc.bench_raider"),
                raider_position,
            )
            .with_health(1_000_000)
            .with_combat(CombatStats::new(0, 1))
            .with_faction(FactionMembership::new(faction("faction.bench_raiders")))
            .with_interactable(Interactable::new(3.0, [Verb::Attack])),
        )
        .unwrap();
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, pairs as u128 + index as u128 + 1),
                text("npc.bench_guard"),
                guard_position,
            )
            .with_health(1_000_000)
            .with_combat(CombatStats::new(2, 0))
            .with_mind(Mind::ai(behavior_id.clone()))
            .with_faction(FactionMembership::new(faction("faction.bench_guards")))
            .with_interactable(Interactable::new(3.0, [Verb::Attack])),
        )
        .unwrap();
    }
    zone
}

fn setup_zone(scenario: Scenario, entity_count: usize) -> Zone {
    match scenario.name {
        "movement" => movement_zone(entity_count),
        "combat" => combat_zone(entity_count),
        "ai_guard" => ai_zone(entity_count, false),
        "ai_chase" => ai_zone(entity_count, true),
        _ => idle_zone(entity_count),
    }
}

fn prepare_zone(scenario: Scenario, zone: &mut Zone, entity_count: usize, tick: usize) {
    match scenario.name {
        "movement" => {
            for index in 0..entity_count {
                let actor = stable_id(IdKind::Entity, index as u128 + 1);
                zone.queue_movement(MovementIntent {
                    actor,
                    principal: stable_id(IdKind::Character, index as u128 + 1),
                    session: format!("session-{index}"),
                    sequence: tick as u64 + 1,
                    destination: movement_destination(index, tick),
                })
                .unwrap_or_else(|error| {
                    panic!("movement queue rejected for entity {index} at tick {tick}: {error:?}")
                });
            }
        }
        "combat" => {
            let pairs = entity_count / 2;
            for index in 0..pairs {
                let actor = stable_id(IdKind::Entity, index as u128 + 1);
                let target = stable_id(IdKind::Entity, pairs as u128 + index as u128 + 1);
                let request =
                    ActionRequest::new(format!("bench-attack-{tick}-{index}"), actor, Verb::Attack)
                        .on(target)
                        .by(stable_id(IdKind::Character, index as u128 + 1));
                let _ = zone.apply_action(&request, Millis::new(0));
            }
        }
        _ => {}
    }
}

fn percentile(sorted_samples: &[u128], percentile: usize) -> u128 {
    let rank = (sorted_samples.len() * percentile).div_ceil(100);
    sorted_samples[rank.saturating_sub(1)]
}

fn verify_workload(scenario: Scenario, zone: &Zone, entity_count: usize, applied_moves: usize) {
    match scenario.name {
        "movement" => assert!(
            applied_moves > 0,
            "movement workload never applied an authoritative move"
        ),
        "combat" => {
            let target = stable_id(IdKind::Entity, (entity_count / 2) as u128 + 1);
            assert!(
                zone.entity(target)
                    .and_then(|entity| entity.health)
                    .is_some_and(|health| health.current < 1_000_000),
                "combat workload never applied damage"
            );
        }
        "ai_guard" | "ai_chase" => {
            let report = zone.ai_tick_report();
            assert!(
                report.action_attempts > 0
                    || report
                        .decisions
                        .iter()
                        .any(|decision| decision.navigation.is_some()),
                "AI workload never attempted an action or a path"
            );
        }
        _ => {}
    }
}

fn run_scenario(scenario: Scenario, entity_count: usize) {
    let mut zone = setup_zone(scenario, entity_count);
    let mut applied_moves = 0;
    for tick in 0..WARMUP_TICKS {
        prepare_zone(scenario, &mut zone, entity_count, tick);
        let (_, outcomes) = zone.advance_movement_tick(TICK_MS, SPEED_M_PER_S);
        applied_moves += outcomes.iter().filter(|outcome| outcome.applied).count();
    }
    verify_workload(scenario, &zone, entity_count, applied_moves);

    let mut samples = Vec::with_capacity(SAMPLES);
    let mut effects = Vec::with_capacity(SAMPLES);
    let mut movement = Vec::with_capacity(SAMPLES);
    let mut navigation = Vec::with_capacity(SAMPLES);
    let mut ai = Vec::with_capacity(SAMPLES);
    let mut statistics = Vec::with_capacity(SAMPLES);
    for tick in WARMUP_TICKS..(WARMUP_TICKS + SAMPLES) {
        prepare_zone(scenario, &mut zone, entity_count, tick);
        let start = Instant::now();
        black_box(zone.advance_movement_tick(TICK_MS, SPEED_M_PER_S));
        samples.push(start.elapsed().as_nanos());
        let phases = zone.tick_phase_timings();
        effects.push(phases.effects_ns);
        movement.push(phases.movement_ns);
        navigation.push(phases.navigation_ns);
        ai.push(phases.ai_ns);
        statistics.push(phases.statistics_ns);
    }
    samples.sort_unstable();
    effects.sort_unstable();
    movement.sort_unstable();
    navigation.sort_unstable();
    ai.sort_unstable();
    statistics.sort_unstable();
    let p99 = percentile(&samples, 99);
    println!(
        "{},{},{},{},{},{},{},{},{},{}",
        scenario.name,
        zone.entity_count(),
        percentile(&samples, 50),
        percentile(&samples, 95),
        p99,
        percentile(&effects, 99),
        percentile(&movement, 99),
        percentile(&navigation, 99),
        percentile(&ai, 99),
        percentile(&statistics, 99),
    );
    assert!(
        p99 <= MAX_P99_TICK_NS,
        "{}-entity {} tick p99 {p99} ns exceeds the {} ns authoritative tick budget",
        zone.entity_count(),
        scenario.name,
        MAX_P99_TICK_NS,
    );
}

fn main() {
    println!(
        "zone tick latency (release; {SAMPLES} measured ticks after {WARMUP_TICKS} warmup ticks; p99 budget {MAX_P99_TICK_NS} ns)"
    );
    println!(
        "scenario,entities,tick_p50_ns,tick_p95_ns,tick_p99_ns,effects_p99_ns,movement_p99_ns,navigation_p99_ns,ai_p99_ns,statistics_p99_ns"
    );
    for scenario in SCENARIOS {
        for entity_count in scenario.entity_counts {
            run_scenario(*scenario, *entity_count);
        }
    }
}
