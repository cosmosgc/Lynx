# PST-005 — Snapshot and event replay not implemented

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [PST-004](../TODO/20-persistence-backend.md#pst-004--zone-leases-and-fencing-and-event-log), 
  [OPS-003](..//TODO/60-operations-release.md#ops-003--backups-and-restore-drills)
- **Affected workflow:** `crates/world_server`, `crates/backend_memory`, recovery procedures

## Resolution

The architecture explicitly requires "load last snapshot, replay events after the snapshot, validate references, then accept sessions" with a 60-second recovery target (line 175 of README). However, the current implementation only loads a single whole-world snapshot and discards all subsequent state. There is no event-sourced recovery path for zones.

The persistence layer must implement:
- Deterministic replay of zone events against a snapshot baseline
- Reference validation after replay (entity tombstones, links, inventories)
- Quarantine mechanism for impossible entities instead of silent reset
- Measurable recovery time with a 60-second target on the reference dataset

## Defect

The `PostgresSnapshotStore` loads only one whole-world snapshot and provides no event sourcing or delta replay. This means:
- Zone crashes cannot be recovered from snapshots alone; the entire world must restart from scratch
- There is no incremental recovery for specific zones or characters
- The documented 60-second recovery target cannot be met for large worlds
- Reconnect after a crash loses all session state even though the world survives

## Impact

- Server crashes lose zone-specific state that should have been recoverable
- Recovery time for large worlds would exceed acceptable bounds (hours vs. minutes)
- The documented failure-and-recovery model is not implementable with current code
- This is a P1 risk: unrecoverable data loss on restart violates the authoritative simulation contract

## Acceptance criteria

- [ ] Zone recovery loads last snapshot and replays all events to current tick
- [ ] Replay validates references, quarantine impossible entities, and reports diagnostics
- [ ] Recovery completes within 60 seconds on the reference dataset (documented P95)
- [ ] Repeated recovery produces identical state (deterministic replay)
- [ ] Quarantined entities are clearly documented in recovery reports and do not silently disappear