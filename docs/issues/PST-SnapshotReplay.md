# PST-005 — Snapshot and event replay not implemented

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [PST-005](../TODO/20-persistence-backend.md#pst-005--snapshot-and-event-replay),
  [OPS-003](../TODO/60-operations-release.md#ops-003--backup-and-restore-automation)
- **Affected workflow:** `crates/world_server`, `crates/backend_memory`, recovery procedures

## Resolution

The architecture targets snapshot loading, event replay, reference validation, and a 60-second recovery time. The current implementation restores a whole-world snapshot but has no event-sourced recovery path; changes since the last successful snapshot can be lost.

Partial hardening: `engine_sim::Zone::restore` now rejects duplicate action,
link, and transfer idempotency keys, mismatched action cache keys, oversized
action/link caches, and transfer receipts with an empty key, zero quantity, or
non-entity endpoints. Tests cover corruption and a valid transfer round trip.
This is fail-closed snapshot validation, not event replay or quarantine. Transfer
endpoints may refer to historical entities, so their existence is not checked;
receipt slot history and other inventory/ownership invariants remain unverified.
The replay model and durable zone event log remain dependent on PST-004 and D4.

The persistence layer must implement:
- Deterministic replay of zone events against a snapshot baseline
- Reference validation after replay (entity tombstones, links, inventories)
- Quarantine mechanism for impossible entities instead of silent reset
- Measurable recovery time with a 60-second target on the reference dataset

## Defect

The `PostgresSnapshotStore` loads only one whole-world snapshot and provides no event sourcing or delta replay. This means:
- A process restart can restore a configured snapshot, but not changes made after it
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
