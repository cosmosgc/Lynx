# ADR 0005: Persistence and transaction boundaries

- **Status:** Accepted
- **Date:** 2026-10-23

## Context

Not all state has the same lifetime or consistency requirement. Saving every
transform every tick is wasteful; saving inventory changes lazily risks
duplication.

## Decision

- PostgreSQL is the durable store for characters, inventories, property,
  factions, moderation, and audit records.
- Every persistent record carries a stable ID, a schema version, and a zone or
  account scope.
- Persistence policies are explicit:
  `Transient`, `Session`, `PersistentEntity`, `PersistentEventLog`,
  `StaticDefinition`.
- Rapidly changing state (transforms, velocity) is snapshotted periodically and
  on meaningful transitions, not every tick.
- Economy and ownership changes are transactional and idempotent. Every
  mutation that can be retried carries a client- or server-generated
  idempotency key; the server rejects duplicates.
- Inventory is stored as rows keyed by owner and slot, with a conservation
  invariant tested in `engine_core`. Item stacks are never stored as an opaque
  blob without a schema version.
- Audit records are append-only and written in the same transaction as the
  state change they describe.
- Migrations live in `server/migrations/` with monotonically increasing
  versions and are applied by `scripts/db-migrate.*`. Destructive migrations
  require a backup step and a documented rollback path.
- Zone recovery target: within 60 seconds of a process restart, reload the last
  snapshot plus the append-only event log, and reject stale client sessions.

## Consequences

- The database schema is a first-class, reviewed artifact.
- Multi-step economy operations cannot be "best effort".
- Snapshot cadence is a tunable operational parameter, not gameplay logic.
