# SIM-018 — Determinism and tick budget

**Status: Done.** The backlog records this outcome: release benchmark covers idle, movement, combat, guard, and chase workloads; seeded multi-tick replay covers movement, timed combat effects, and AI. All acceptance criteria below are checked complete.

## Original task scope

- Instrument tick phases (controllers, actions, effects, replication prep) and
  define per-phase budgets.
- Remove iteration-order nondeterminism (hash maps, unordered sets) from
  simulation-visible paths.
- Add seeded property tests/replays for multi-tick scenarios.
- Define overload behavior: shed cosmetic work before authoritative work, and
  never silently drop player actions.

**Overload policy:** Authoritative actions are validated and reported to their
requester; queue admission rejects excess movement with a correlated error
instead of dropping accepted input silently. AI controller scans, perception,
pathfinding, and replication queues have explicit caps and fixed diagnostics;
exhausted AI/path budgets defer or reject that work without overrunning the
authoritative tick. This simulation currently has no cosmetic tick work to shed.

## Acceptance evidence

- [x] A release benchmark reports p50/p95/p99 tick duration and entity counts.
- [x] Replaying the same inputs produces byte-identical snapshots.
- [x] Overload behavior is documented and tested.
- [x] The CI benchmark fails when p99 exceeds the authoritative 50 ms tick
      deadline.

**Measured baseline:** `cargo bench -p engine_sim --bench tick` reports one row
per scenario and entity count. A representative local Windows release run:

| Scenario | Entities | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: |
| idle | 100 | 1.5 µs | 1.6 µs | 1.6 µs |
| idle | 1,000 | 14.9 µs | 15.1 µs | 15.2 µs |
| movement | 100 | 202.4 µs | 213.3 µs | 223.9 µs |
| movement | 500 | 5.57 ms | 5.83 ms | 6.49 ms |
| combat | 100 | 1.6 µs | 1.7 µs | 1.8 µs |
| combat | 1,000 | 15.4 µs | 21.6 µs | 44.1 µs |
| ai_guard | 64 | 220.1 µs | 225.1 µs | 236.8 µs |
| ai_guard | 256 | 479.4 µs | 510.4 µs | 551.1 µs |
| ai_chase | 64 | 220.0 µs | 225.8 µs | 236.3 µs |
| ai_chase | 256 | 483.6 µs | 514.7 µs | 552.5 µs |

The movement workload queues one intent per entity per tick and therefore
exercises contested-space and body-collision checks at O(n²); it is capped at
500 entities so the shared-runner gate keeps headroom. Combat applies one
server-validated attack per actor per tick against a high-health target. AI
scenarios use 64 and 256 entities with the runtime's bounded controller,
perception, and path budgets, so they measure bounded work rather than linear
entity scaling. The benchmark reports p99 for effects, movement, navigation, AI,
and statistics phases; on the 1,000-entity idle row those were 11.9/0.1/0.1/0.1/3.0
µs, and the 256-entity AI rows spent about 0.54 ms in the AI phase. Runtime phase
measurements are diagnostics only and never enter snapshots or influence
simulation decisions. Numbers vary between runs; the gate is the absolute 50 ms
deadline, not a microbenchmark threshold.

`seeded_combat_and_ai_replay_is_byte_identical` builds two zones with opposite
spawn order and drives 48 ticks of movement, attacks, periodic damage, shields,
and AI decisions; snapshots, AI reports, diagnostics, and the combat audit stay
byte-identical. `repeated_runs_of_the_same_seed_match` proves a fresh run of the
same seed reproduces the same snapshot. The CI gate remains the 50 ms server tick
deadline rather than a machine-specific microbenchmark threshold.

## Source

See [`10-simulation-gameplay.md`](../10-simulation-gameplay.md) for the full task history and implementation notes.
