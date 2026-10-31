# PST-005 — Snapshot and event replay incomplete

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [PST-005](../TODO/20-persistence-backend.md#pst-005--snapshot-and-event-replay),
  [OPS-003](../TODO/60-operations-release.md#ops-003--backup-and-restore-automation)
- **Affected workflow:** `crates/world_server`, `crates/backend_memory`, recovery procedures

## Resolution

The persistence model is now explicitly hybrid: the whole-world snapshot is the
authoritative checkpoint, and the ordered zone event log records lifecycle
markers plus full snapshot checkpoints. Migrations `0010_snapshot_event_checkpoints.sql`
and `0011_snapshot_checkpoint_zone.sql` store the checkpoint sequence and zone
with the snapshot row and backfill only unambiguous matching checkpoints. Fenced startup verifies the exact row/event
pair, replays the ordered checkpoint history, and fails closed on gaps, missing
or stale checkpoints, corruption, or mismatches. Untracked legacy snapshots are
accepted only when the event log contains no checkpoint event for that world.
Operators can run `lynx-world verify-snapshot` for a read-only point-in-time
check without taking the zone lease.

The service now emits `zone.entity.upsert` / `zone.entity.remove` records for
changed persistent entity records after each successful authoritative tick.
Each tick's event batch is atomic and fenced; append failure stops the authority
loop rather than continuing with unrecorded state. The reducer validates event
tick/actor/entity identity, persistence policy, and non-decreasing tick order,
and applies events through a stable-ID map so repeated runs produce the same
canonical state. This is deliberately bounded: links, action caches, cooldowns,
transfer receipts, calendar, and faction changes are still only captured by
whole-world checkpoints, so complete multi-tick recovery is not yet provided.

Recovery is now quarantine-aware. `Zone::restore_quarantining` re-validates
entity identity and persistence policy, inventories, health ranges, links,
cooldowns, and transfer receipts. Structurally corrupt snapshots still fail
closed, but individually impossible entries are skipped and returned in a
report. Migration `0012_recovery_quarantine.sql` stores that report in
`lynx.recovery_quarantine`; the file store writes a `*.quarantine.json` sidecar.
Startup logs each record, persists the report before serving, and aborts if it
cannot be stored, so quarantined entries never disappear silently.
`lynx-world verify-snapshot` performs the same replay and dry-run restore
read-only and prints both newly detected and previously stored quarantine
records.

Recovery time is measured at startup and logged. The reference dataset (1,000
persistent entities, 5,000 upserts, and 400 removals) is replayed by
`recovery_reference_dataset_completes_within_the_documented_target`, which
asserts the 60-second target and deterministic repeated output. Full aggregate
event coverage (links, cooldowns, transfers, calendar, factions) and event-log
retention remain open PST-005 work.

The persistence layer must implement:
- [x] Deterministic replay of typed persistent-entity upsert/removal events after a snapshot baseline
- [x] Reference validation after replay (entity identity, persistence, inventories, health, links, cooldowns, transfer receipts)
- [x] Quarantine mechanism for impossible entities instead of silent reset
- [x] Measurable recovery time with a 60-second target on the reference dataset
- [ ] Per-tick delta events for non-entity aggregates
- [ ] Event-log retention policy

## Defect

`PostgresSnapshotStore` now replays persistent-entity delta events, but recovery
is still incomplete:
- Links, cooldowns, transfer receipts, calendar state, and faction state are
  captured only by whole-world checkpoints, not per-tick events
- The event log has no retention or purge policy, so it grows with every tick
  that changes a persistent entity
- The 60-second target is enforced on the reference replay dataset, but no
  production-scale P95 has been measured
- Reconnect after a crash still loses session state even though the world
  survives

## Impact

- A crash between whole-world checkpoints can lose non-entity aggregate changes
- Event storage grows without a documented retention boundary
- Production recovery headroom is unknown beyond the reference dataset
- This remains a P1 durability risk for the authoritative simulation contract

## Acceptance criteria

- [x] Zone recovery loads the last snapshot and replays persistent-entity events to the current tick
- [x] Replay validates references, quarantines impossible entries, and reports diagnostics
- [x] Recovery completes within 60 seconds on the reference dataset
- [x] Repeated recovery produces identical state (deterministic replay)
- [x] Quarantined entities are documented in recovery reports and do not silently disappear
- [ ] Recovery replays non-entity aggregates to the current tick
- [ ] Event-log retention is defined and enforced
