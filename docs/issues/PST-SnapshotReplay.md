# PST-005 — Snapshot and event replay not implemented

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

This does not yet replay gameplay delta events: state after the latest committed
checkpoint can still be lost unless the event log contains the supported
`zone.entity.upsert` / `zone.entity.remove` records. The reducer applies those
version-1 events after the latest checkpoint and fails closed on invalid,
non-persistent, misidentified, or pre-checkpoint entity changes. The live world
does not yet emit these events for gameplay mutations, so this is a replay
contract and reducer, not end-to-end delta durability. Replayed state is still
passed through strict zone restoration; invalid entities fail startup rather
than being silently dropped. A durable quarantine record and operator-visible
recovery report are not implemented. Reference quarantine, end-to-end event
emission, dry-run verification, and the measured 60-second recovery target
remain open PST-005 work.

Partial hardening: `engine_sim::Zone::restore` now rejects duplicate action,
link, and transfer idempotency keys, mismatched action cache keys, oversized
action/link caches, and transfer receipts with an empty key, zero quantity, or
non-entity endpoints. Tests cover corruption and a valid transfer round trip.
This is fail-closed snapshot validation, not event replay or quarantine. Transfer
endpoints may refer to historical entities, so their existence is not checked;
receipt slot history and other inventory/ownership invariants remain unverified.
The replay model and durable zone event log were established by PST-004 and D4;
PST-005 continues with delta replay, quarantine, operator verification, and
recovery-time measurement.

The persistence layer must implement:
- [x] Deterministic replay of typed persistent-entity upsert/removal events after a snapshot baseline
- Reference validation after replay (entity tombstones, links, inventories)
- Quarantine mechanism for impossible entities instead of silent reset
- Measurable recovery time with a 60-second target on the reference dataset

## Defect

The `PostgresSnapshotStore` replays and validates full snapshot checkpoints but does not yet apply gameplay delta events. This means:
- A process restart restores the last committed snapshot, but not changes made after it
- There is no incremental recovery for specific zones or characters
- The documented 60-second recovery target has not been demonstrated
- Reconnect after a crash loses all session state even though the world survives

## Impact

- Server crashes lose zone-specific state that should have been recoverable
- Recovery time for large worlds is unmeasured
- The documented failure-and-recovery model is not implementable with current code
- This is a P1 risk: unrecoverable data loss on restart violates the authoritative simulation contract

## Acceptance criteria

- [ ] Zone recovery loads last snapshot and replays all events to current tick
- [ ] Replay validates references, quarantine impossible entities, and reports diagnostics
- [ ] Recovery completes within 60 seconds on the reference dataset (documented P95)
- [ ] Repeated recovery produces identical state (deterministic replay)
- [ ] Quarantined entities are clearly documented in recovery reports and do not silently disappear
