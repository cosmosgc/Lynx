//! Reproducible zone-tick latency report; run with
//! `cargo bench -p engine_sim --bench tick`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use engine_core::id::{DefinitionId, IdKind, StableId};
use engine_core::inventory::UniformItemRules;
use engine_core::math::Vec3;
use engine_sim::entity::EntityRecord;
use engine_sim::nav::NavGrid;
use engine_sim::zone::{Zone, ZoneConfig};

const WARMUP_TICKS: usize = 100;
const SAMPLES: usize = 1_000;
/// The default server tick deadline; the generous absolute gate tolerates
/// ordinary shared-runner noise while still catching severe regressions.
const MAX_P99_TICK_NS: u128 = 50_000_000;

fn stable_id(kind: IdKind, value: u128) -> StableId {
    StableId::from_parts(kind, 1, value)
}

fn zone_with_entities(entity_count: usize) -> Zone {
    let config = ZoneConfig::new(
        DefinitionId::new("zone.tick_benchmark").unwrap(),
        stable_id(IdKind::Zone, 1),
        Vec3::ZERO,
        Vec3::new(32.0, 1.0, 32.0),
    )
    .unwrap()
    .with_max_entities(entity_count);
    let nav = NavGrid::new(32, 32, 1.0, Vec3::ZERO).unwrap();
    let mut zone = Zone::new(config, nav, Arc::new(UniformItemRules::new(10))).unwrap();
    for index in 0..entity_count {
        let coordinate = f32::from(u8::try_from(index % 32).unwrap()) + 0.5;
        zone.spawn(
            EntityRecord::new(
                stable_id(IdKind::Entity, index as u128 + 1),
                DefinitionId::new("entity.tick_benchmark").unwrap(),
                Vec3::new(coordinate, 0.0, 0.5),
            )
            .with_health(10),
        )
        .unwrap();
    }
    zone
}

fn percentile(sorted_samples: &[u128], percentile: usize) -> u128 {
    let rank = (sorted_samples.len() * percentile).div_ceil(100);
    sorted_samples[rank.saturating_sub(1)]
}

fn main() {
    println!(
        "zone tick latency (release; {SAMPLES} measured ticks after {WARMUP_TICKS} warmup ticks; p99 budget {MAX_P99_TICK_NS} ns)"
    );
    println!(
        "entities,tick_p50_ns,tick_p95_ns,tick_p99_ns,effects_p99_ns,movement_p99_ns,navigation_p99_ns,ai_p99_ns,statistics_p99_ns"
    );
    for entity_count in [100, 1_000] {
        let mut zone = zone_with_entities(entity_count);
        for _ in 0..WARMUP_TICKS {
            black_box(zone.advance_movement_tick(50, 2.0));
        }

        let mut samples = Vec::with_capacity(SAMPLES);
        let mut effects = Vec::with_capacity(SAMPLES);
        let mut movement = Vec::with_capacity(SAMPLES);
        let mut navigation = Vec::with_capacity(SAMPLES);
        let mut ai = Vec::with_capacity(SAMPLES);
        let mut statistics = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let start = Instant::now();
            black_box(zone.advance_movement_tick(50, 2.0));
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
            "{},{},{},{},{},{},{},{},{}",
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
            "{}-entity tick p99 {p99} ns exceeds the {} ns authoritative tick budget",
            zone.entity_count(),
            MAX_P99_TICK_NS,
        );
    }
}
