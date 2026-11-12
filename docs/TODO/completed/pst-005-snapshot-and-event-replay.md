# PST-005 — Snapshot and event replay

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Choose the replay model: event-sourced zone recovery, whole-world snapshot plus
  delta events, or a documented hybrid. Record the decision (gate D4).
- Implement deterministic replay with schema-version migration hooks.
- Validate references, entity tombstones, links, inventories, and ownership
  after replay; quarantine impossible entities and report diagnostics instead of
  silently resetting them.
- Measure recovery time and enforce the 60-second target with a test.
- Expose a dry-run/verify command for operators.

## Acceptance evidence

- [x] Recovery from snapshot + events restores the same state as before the
      crash for a multi-tick scenario (entities, links, cooldowns, transfer
      receipts, calendar, and factions).
- [x] Corrupt or missing events fail loudly with a recoverable diagnostic.
- [x] Impossible entities are quarantined, not silently dropped.
- [x] Recovery completes within the documented target on the reference dataset.
- [x] Replay is deterministic across repeated runs.
- [x] Event-log retention is bounded by the latest checkpoint.

**Decision (D4):** Use a hybrid whole-world checkpoint plus append-only event
log. The snapshot row remains authoritative; each fenced save atomically records
the exact sequence of its `world.snapshot` checkpoint event. After each
successful authoritative tick, the service compares persistent entity records
and the replayable zone/world aggregates with the previous checkpoint/event
state and appends all changed/created/removed records in one fenced transaction:
entities, links, action and link cooldowns, transfer receipts, paired
calendar/faction aggregates, action/link idempotency caches, and zone counters.

**Implementation:** Migration `0010_snapshot_event_checkpoints.sql` pins each
snapshot row to its checkpoint event; `0011_snapshot_checkpoint_zone.sql` adds
the zone identity because event sequences are zone-local. Both migrations
backfill only unambiguous matching historical data. Fenced loads reject a
missing checkpoint row/event, a stale checkpoint sequence, an unknown or
mismatched zone, an event hash mismatch, sequence gaps, corrupt payloads, or a
replay that disagrees with the stored snapshot. Legacy rows without a sequence
are accepted only when no matching checkpoint event exists. Fenced `save()`
routes through the atomic snapshot-and-event write path. `lynx-world
verify-snapshot` provides read-only point-in-time verification in a
repeatable-read transaction without acquiring a lease; it distinguishes
verified, legacy-uncheckpointed, and absent state, and performs a dry-run zone
restore that reports quarantined entries. The reducer accepts version-1 events
through one schema gate, validates tick/actor/entity identity and persistence
policy, requires non-decreasing ticks, applies entities through a stable-ID map,
and fails closed on any unsupported event schema. The service emits event
batches atomically per tick; append failure stops the authority loop.

`Zone::restore_quarantining` validates entity identity and persistence policy,
inventories, health ranges, links, cooldowns, and transfer receipts after replay.
Structurally invalid snapshots still fail closed, but individually impossible
entries are skipped and returned in a report. Migration
`0012_recovery_quarantine.sql` stores that report in
`lynx.recovery_quarantine` (deduplicated and refreshed on repeat), and the file
store writes a `*.quarantine.json` sidecar. Startup logs each record, persists
the report before serving, and aborts if the report cannot be stored. The
reference recovery dataset is 1,000 persistent entities with 5,000 upserts and
400 removals; `recovery_reference_dataset_completes_within_the_documented_target`
measures replay and enforces the 60-second target, and startup logs the measured
recovery time. The manual production-scale check replays 55,001 events and
measured a p95 of 1.71 s. A checkpoint save prunes every earlier zone event, so
retention is one snapshot interval. The multi-tick PostgreSQL test
`postgres_recovers_multi_tick_aggregates_and_prunes_replayed_history` covers
entity, link, cooldown, link-cooldown, transfer, calendar, and faction deltas
across a crash, then verifies replay after checkpoint pruning.

## Source

See [`20-persistence-backend.md`](../20-persistence-backend.md) for the full task history and implementation notes.
