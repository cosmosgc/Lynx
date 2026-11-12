# PST-004 — Zone leases, fencing, and event log

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Acquire/renew/release a zone lease with an expiry and a monotonically
  increasing fencing token.
- Refuse authoritative writes when the lease is lost or superseded.
- Append zone events with `(zone_id, sequence)` ordering in the same transaction
  as the state change they describe.
- Write periodic snapshots with tick, schema version, payload hash, and a
  well-defined retention policy.
- Define crash recovery: expired lease takeover, replay from the last snapshot,
  and idempotent event application.

## Acceptance evidence

- [x] Two processes cannot both hold an unexpired lease for one zone.
- [x] A stale owner's writes are rejected after takeover.
- [x] Event sequences are gap-free and ordered per zone.
- [x] Snapshot + replay reconstructs the same state as the live process.
- [x] Lease expiry/takeover is covered by an integration test.

**Implementation:** `0007_zone_fencing.sql` adds the durable fencing token and
`PostgresZoneLeaseStore` implements acquire, renew, release, and fenced,
gap-free event append/read. With `--state-postgres`, `lynx-world` acquires the
demo zone lease before loading the snapshot and refuses to start when another
live process owns it; `--lease-ttl-ms` (default 30 s) sets the lifetime. A
renewal task renews every third of the TTL and turns readiness false and stops
the process when the lease is taken over or can no longer be renewed before
expiry. A PostgreSQL snapshot write now persists the `world.snapshot` event,
including an integrity-checked complete snapshot payload, in the same fenced
transaction as the snapshot row. Fenced startup replays the ordered event log
and rejects a gap, corrupt payload, or replay/store mismatch before serving.
The file-backed and memory-only stores are unchanged: without a shared database
there is nothing to fence against, and they run without a lease. Event schema
migration, quarantine, retention, and recovery-performance work are covered by
`PST-005`.

## Source

See [`20-persistence-backend.md`](../20-persistence-backend.md) for the full task history and implementation notes.
