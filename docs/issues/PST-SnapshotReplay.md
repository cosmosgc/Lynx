# PST-005 — Snapshot and event replay

- **Status:** Resolved
- **Reported:** 2026-10-28
- **Resolved:** 2026-10-31
- **Related tasks:** [PST-005](../TODO/20-persistence-backend.md#pst-005--snapshot-and-event-replay),
  [OPS-003](../TODO/60-operations-release.md#ops-003--backup-and-restore-automation)
- **Affected workflow:** `crates/world_server`, `crates/backend_memory`, recovery procedures

## Resolution

The persistence model is explicitly hybrid: the whole-world snapshot is the
authoritative checkpoint, and the ordered zone event log records lifecycle
markers, full snapshot checkpoints, and replayable per-tick deltas. Migrations
`0010_snapshot_event_checkpoints.sql` and
`0011_snapshot_checkpoint_zone.sql` store the checkpoint sequence and zone with
the snapshot row and backfill only unambiguous matching checkpoints. Fenced
startup verifies the exact row/event pair, replays the ordered checkpoint
history, and fails closed on gaps, missing or stale checkpoints, corruption, or
mismatches. Untracked legacy snapshots are accepted only when the event log
contains no checkpoint event for that world. Operators can run `lynx-world
verify-snapshot` for a read-only point-in-time check without taking the zone
lease.

After each successful authoritative tick, the service emits one atomic, fenced
batch containing persistent entity upserts/removals, link replacements,
action and link cooldown replacements, newly applied transfer receipts, and the
paired calendar/faction aggregates. Append failure stops the authority loop
rather than continuing with unrecorded state. The reducer validates
tick/actor/entity identity, persistence policy, reference existence, and
non-decreasing tick order, applies events through a stable-ID map so repeated
runs produce canonical state, and fails closed on any unsupported event schema.
Action/link idempotency caches and zone counters are replayed in the same batch.

Every checkpoint save deletes earlier events for the zone under the append-only
purge guard. Retention is therefore bounded to one snapshot interval
(`--snapshot-interval-ms`), and the retained log begins with the newest
checkpoint.

Recovery is quarantine-aware. `Zone::restore_quarantining` re-validates entity
identity and persistence policy, inventories, health ranges, links, cooldowns,
and transfer receipts. Structurally corrupt snapshots still fail closed, but
individually impossible entries are skipped and returned in a report. Migration
`0012_recovery_quarantine.sql` stores that report in
`lynx.recovery_quarantine`; the file store writes a `*.quarantine.json` sidecar.
Startup logs each record, persists the report before serving, and aborts if it
cannot be stored, so quarantined entries never disappear silently.
`lynx-world verify-snapshot` performs the same replay and dry-run restore
read-only and prints both newly detected and previously stored quarantine
records.

Recovery time is measured at startup and logged. The reference dataset (1,000
persistent entities, 5,000 upserts, and 400 removals) is replayed by
`recovery_reference_dataset_completes_within_the_documented_target`, which
asserts the 60-second target and deterministic repeated output. The manual
production-scale check (55,001 events, five runs) measured a p95 of 1.71 s.
`postgres_recovers_multi_tick_aggregates_and_prunes_replayed_history` proves a
multi-tick crash/restart across every replayed aggregate, then verifies replay
and recovery after checkpoint pruning.

The persistence layer implements:
- [x] Deterministic replay of typed persistent-entity upsert/removal events after a snapshot baseline
- [x] Deterministic replay of links, cooldowns, link cooldowns, transfer receipts, and paired calendar/faction state
- [x] Reference validation after replay (entity identity, persistence, inventories, health, links, cooldowns, transfer receipts)
- [x] Quarantine mechanism for impossible entities instead of silent reset
- [x] Measurable recovery time with a 60-second target on the reference dataset
- [x] Bounded event-log retention anchored to the latest checkpoint
- [x] Fail-closed version gate for unsupported event schemas

## Defect

None for the replay and retention scope above. Durable session/resume state after
a world restart remains open under `PST-006`.

## Impact

The authoritative simulation can recover entity, link, cooldown, transfer,
calendar, faction, idempotency-cache, and counter changes made between
whole-world checkpoints, and the event log no longer grows without a documented
boundary.

## Acceptance criteria

- [x] Zone recovery loads the last snapshot and replays all supported delta events to the current tick
- [x] Replay validates references, quarantines impossible entries, and reports diagnostics
- [x] Recovery completes within 60 seconds on the reference dataset
- [x] Repeated recovery produces identical state (deterministic replay)
- [x] Quarantined entities are documented in recovery reports and do not silently disappear
- [x] Recovery replays non-entity aggregates to the current tick
- [x] Event-log retention is defined and enforced
