# Persistence and backend backlog

Scope: durable state behind `game_api` and the world service — accounts,
characters, inventories, factions, calendar, zone leases, event replay,
snapshots, migrations, backups, and adapter coverage.

**What exists today:** migrations `0001`–`0006` define accounts, characters,
versioned components, inventory, property, idempotency keys, audit, content
bundles, zone leases/snapshots/events, factions, the world calendar, and
whole-world snapshots. `lynx-world` can persist a whole world to a JSON file or
to `lynx.world_snapshots` through `PostgresSnapshotStore`. `backend_memory`
implements every `game_api` service; `backend_nakama` implements only health and
authentication services, with separate, unwired character and identity SQL
repositories. Most normalized tables are defined but not yet used by the runtime.

**Related design:** [architecture §Persistence model](../architecture.md#persistence-model),
[ADR 0003](../adr/0003-native-nakama-and-postgresql.md),
[ADR 0005](../adr/0005-persistence-and-transactions.md),
[ADR 0018](../adr/0018-nakama-adapter-seam-and-replacement-criteria.md),
[operations](../operations.md), [migrations README](../../server/migrations/README.md).

---

## PST-001 — Account and character persistence

- **Priority:** P0
- **Status:** Done — Nakama-authenticated PostgreSQL character persistence
- **Depends on:** -
- **References:** migration `0001_core.sql`, `crates/game_api/src/service.rs`,
  `character.rs`, `crates/backend_memory`

**Implemented:** `backend_nakama::PostgresCharacterStore` provides
account-scoped list, active-character lookup, transactional character creation,
idempotent tombstoning, and character-bound world entry/leave against
`lynx.characters`. Creation serializes on the account row, enforces an
eight-character limit, and writes a retry receipt and audit event atomically;
receipts survive deletion and adapter reconnection. `PostgresIdentityStore`
accepts only canonical Nakama UUIDs and `provision_account` creates or resolves
the canonical Lynx account mapping without accepting a client account field.
`NakamaPostgresBackend` composes validated Nakama sessions with both stores and
implements `AuthService`, `CharacterService`, `CharacterIntentService`, and
`CharacterWorldService`. `CharacterWorldService` is character-bound rather than
overloading the legacy zone-only `WorldService`; entry/leave persist a selected
owned, active character's server-routed zone with an audit event.
`backend_memory` implements the same non-durable contract for offline tests.

**Validation:** The shared contract and ownership/restart scenarios pass against
both adapters. The live PostgreSQL run uses a migrated isolated database and
`LYNX_REQUIRE_POSTGRES_TEST=1`; it covers reopen persistence, duplicate
creation retries, name reuse after deletion, cross-account select/delete
rejection, concurrent deletion, and character-bound world entry/leave.
`cargo test -p backend_nakama -p backend_memory -p backend_contract_tests`
passes with that database configured, and scoped clippy/fmt are clean. Live
`lynx_world` admission still uses development grants; replacing that admission
path with this adapter is tracked by NET-001, not by the persistence criteria
below.

**Why:** The durable world snapshot restores zones, calendar, and factions, but
accounts and characters previously lived only in the in-memory backend, so a
restart lost creation, selection, and ownership even when the world survived.

**Scope**

- Implement `AuthService`, `CharacterService`, and the `WorldService`
  zone-entry/leave calls used by character selection against PostgreSQL, or
  through the chosen Nakama-backed path. Record the choice in ADR 0018 or a
  successor.
- Use migrations `0001` tables; add a migration only for missing fields.
- Preserve ownership checks: a session may only select/create characters it
  owns; character IDs are server-generated stable IDs.
- Hash or delegate credentials; never store plaintext secrets. Keep tokens out
  of logs and error details.
- Define transaction boundaries for create/select/delete and make retries
  idempotent.
- Provide a contract test suite shared by memory and persistent adapters, and
  skip the live PostgreSQL test cleanly when `LYNX_PG_*` is unset.

**Acceptance criteria**

- [x] Creating a character, restarting the service, and selecting it again
      returns the same character with the same owner.
- [x] A session cannot select, rename, or delete another account's character.
- [x] Duplicate create/select retries do not duplicate rows.
- [x] The persistent adapter and `backend_memory` pass the same contract tests.
- [x] No credential or session secret appears in logs or error chains.

---

## PST-002 — Inventory and transfer-ledger persistence

- **Priority:** P1
- **Status:** Blocked — `PST-001`
- **Depends on:** `PST-001`
- **References:** migration `0001_core.sql`, `crates/engine_sim` inventory,
  `crates/game_api/src/inventory.rs`

**Why:** Inventory transfers are idempotent in the simulation and snapshot, but
there is no durable per-aggregate inventory store or ledger behind `game_api`.

**Scope**

- Persist inventory slots and slot versions transactionally.
- Persist transfer receipts/idempotency keys so retries after a restart cannot
  duplicate or lose value.
- Enforce container rules, capacity, and ownership in the same transaction as
  the slot update.
- Emit an append-only ledger entry for value movement where the economy design
  requires it.
- Provide reconciliation queries for operators.

**Acceptance criteria**

- [ ] A transfer applied twice returns the original result and moves value once.
- [ ] A failed transaction leaves both inventories unchanged.
- [ ] Restarting between request and retry preserves idempotency.
- [ ] Slot versions prevent lost updates under concurrency.
- [ ] Inventory state and audit/ledger entries agree after fault injection.

---

## PST-003 — Normalized faction and calendar persistence

- **Priority:** P1
- **Status:** Blocked — `PST-001`, `PST-005`
- **Depends on:** `PST-001`, `PST-005`
- **References:** migrations `0004_factions.sql`, `0005_world_events.sql`,
  ADRs [0020](../adr/0020-faction-governance-treasury-and-territory.md),
  [0021](../adr/0021-world-calendar-and-bounded-conflicts.md)

**Why:** Factions and the calendar are persisted only inside the whole-world
snapshot, even though normalized tables and consistency triggers exist. That
makes cross-aggregate queries, operator inspection, and partial recovery harder
than the schema intends.

**Scope**

- Implement the `FactionStore` and `CalendarStore` seams against the existing
  tables.
- Keep calendar/faction paired updates in one transaction, including territory
  changes, enrollments, resolutions, and treasury entries.
- Backfill from an existing whole-world snapshot and verify equality before
  switching the authority.
- Decide whether the whole-world snapshot remains a cache/backup or is retired
  for these aggregates; document it.
- Preserve append-only history and audit triggers.

**Acceptance criteria**

- [ ] A restart restores factions, memberships, treasury, territory, events,
      enrollments, and results from normalized tables.
- [ ] A failed cross-aggregate update changes neither aggregate.
- [ ] Snapshot and normalized views agree after backfill.
- [ ] Append-only tables still reject update/delete outside the purge path.
- [ ] Operator queries can inspect current state and history.

---

## PST-004 — Zone leases, fencing, and event log

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** migration `0003_zone_state.sql`, ADR 0005

**Why:** `zone_leases`, `zone_snapshots`, and `zone_events` exist, but the
runtime still behaves as a single process-local authority. There is no fencing
token or event sequence that proves which process owns a zone after a crash.

**Scope**

- Acquire/renew/release a zone lease with an expiry and a monotonically
  increasing fencing token.
- Refuse authoritative writes when the lease is lost or superseded.
- Append zone events with `(zone_id, sequence)` ordering in the same transaction
  as the state change they describe.
- Write periodic snapshots with tick, schema version, payload hash, and a
  well-defined retention policy.
- Define crash recovery: expired lease takeover, replay from the last snapshot,
  and idempotent event application.

**Acceptance criteria**

- [ ] Two processes cannot both hold an unexpired lease for one zone.
- [ ] A stale owner's writes are rejected after takeover.
- [ ] Event sequences are gap-free and ordered per zone.
- [ ] Snapshot + replay reconstructs the same state as the live process.
- [ ] Lease expiry/takeover is covered by an integration test.

---

## PST-005 — Snapshot and event replay

- **Priority:** P1
- **Status:** Blocked — `PST-004`
- **Depends on:** `PST-004`
- **References:** [architecture §Failure and recovery](../architecture.md#failure-and-recovery),
  migration `0003_zone_state.sql`

**Why:** The architecture promises "load last snapshot, replay events after the
snapshot, validate references, then accept sessions" with a 60-second recovery
target. Replay is not implemented; `PostgresSnapshotStore` loads a single
whole-world snapshot and nothing after it.

**Scope**

- Choose the replay model: event-sourced zone recovery, whole-world snapshot plus
  delta events, or a documented hybrid. Record the decision (gate D4).
- Implement deterministic replay with schema-version migration hooks.
- Validate references, entity tombstones, links, inventories, and ownership
  after replay; quarantine impossible entities and report diagnostics instead of
  silently resetting them.
- Measure recovery time and enforce the 60-second target with a test.
- Expose a dry-run/verify command for operators.

**Acceptance criteria**

- [ ] Recovery from snapshot + events restores the same state as before the
      crash for a multi-tick scenario.
- [ ] Corrupt or missing events fail loudly with a recoverable diagnostic.
- [ ] Impossible entities are quarantined, not silently dropped.
- [ ] Recovery completes within the documented target on the reference dataset.
- [ ] Replay is deterministic across repeated runs.

---

## PST-006 — Durable sessions and resume windows

- **Priority:** P2
- **Status:** Blocked — `PST-001`, `NET-001`
- **Depends on:** `PST-001`, `NET-001`
- **References:** [networking §Reconnect and recovery](../networking.md#reconnect-and-recovery),
  ADR 0013

**Why:** Resume tokens and windows are process-local. A zone restart invalidates
every resume window even though the world and characters survive.

**Scope**

- Persist session/resume metadata with hashed tokens, expiry, single-use
  rotation, and a generation/fencing value.
- Decide what may survive a restart: character attachment, last acknowledged
  tick, and interest set are candidates; open sockets obviously cannot.
- Sweep expired windows durably; revoke on logout, ban, or role change.
- Keep token material out of logs, snapshots, and debug output.

**Acceptance criteria**

- [ ] A reconnect after a service restart resumes within the configured window.
- [ ] Replayed, expired, revoked, and already-connected tokens are rejected with
      the documented error codes.
- [ ] Token rotation is atomic and survives a crash between validation and
      rotation.
- [ ] Expired windows are swept without unbounded table growth.

---

## PST-007 — PostgreSQL connection hardening

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** `crates/lynx_world/src/persistence/postgres.rs`,
  [operations](../operations.md)

**Why:** `PostgresSnapshotStore` uses plain TCP (`NoTls`) with no pool, explicit
timeouts, or retry policy. That is acceptable for a loopback development
database and not acceptable for a remote one.

**Scope**

- Add TLS support (rustls or native-tls) and a configuration switch; keep plain
  TCP only for loopback/trusted development with an explicit opt-in.
- Add connection pooling, connect/read/write timeouts, and bounded retry with
  backoff for retryable failures.
- Separate migration/setup credentials from the runtime role; document least
  privilege.
- Make failures observable without leaking DSNs, passwords, or payload data.
- Test TLS and retry behavior against a local PostgreSQL instance; skip cleanly
  when unconfigured.

**Acceptance criteria**

- [ ] TLS is verified (not just enabled) against a test server or documented
      trust root.
- [ ] A remote DSN without TLS is refused unless explicitly allowed.
- [ ] Transient failures retry within a bounded budget and then surface a
      structured error.
- [ ] Pool exhaustion and shutdown do not hang the authority loop.
- [ ] Credentials never appear in logs or error details.

---

## PST-008 — Schema version checks and migration integration

- **Priority:** P1
- **Status:** In progress
- **Depends on:** —
- **References:** [`scripts/db-migrate.ps1`](../../scripts/db-migrate.ps1),
  [migrations README](../../server/migrations/README.md)

**Why:** Migrations have a checksummed, advisory-locked runner, but the service
does not verify at startup that the database schema matches the binary. A
partially migrated database fails later, in a less obvious place.

**Scope**

- Record the schema version(s) the binary expects and check
  `lynx.schema_migrations` at startup before opening the world.
- Fail startup with a clear, structured error when migrations are missing,
  pending, or newer than the binary supports.
- Add `lynx doctor --backend` output for schema version, pending migrations, and
  checksum mismatches.
- Run the runner against a fresh database in CI and assert `0001`–`0006` apply
  idempotently.
- Document the upgrade order: backup, migrate, start new binary, roll back.

**Acceptance criteria**

- [ ] Starting against an unmigrated database fails with an actionable message.
- [ ] Starting against a newer schema is refused unless compatibility is
      explicitly declared.
- [ ] `doctor` reports pending/applied versions without changing state.
- [ ] CI proves a fresh database migrates to head and a second run is a no-op.
- [ ] Applied-migration checksum changes are rejected.

---

## PST-009 — Backup and restore verification

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** [operations §Backups](../operations.md),
  [security §Abuse prevention](../security.md#abuse-prevention)

**Why:** Backups are only real when a restore has been verified. The repository
has setup/migration scripts but no documented, exercised restore drill for the
world database or snapshots.

**Scope**

- Define RPO/RTO for characters, inventories, factions/calendar, audit logs, and
  whole-world snapshots.
- Script a full logical backup and restore (for example `pg_dump`/`pg_restore`)
  plus snapshot export/import.
- Run a restore drill that starts the service against the restored database and
  verifies world, characters, and audit continuity.
- Document retention, off-host storage, encryption, and access control.
- Add a CI or scheduled job for a small restore smoke test.

**Acceptance criteria**

- [ ] A documented command restores a backup into a scratch database.
- [ ] The restored service starts, loads the world, and passes a smoke check.
- [ ] Audit and append-only tables survive restore with triggers intact.
- [ ] RPO/RTO and retention are documented and tested against the target.
- [ ] Backup failures alert rather than failing silently.

---

## PST-010 — Nakama adapter coverage

- **Priority:** P1
- **Status:** Proposed — depends on the D1 auth/backend decision
- **Depends on:** `PST-001`, `PST-002`, D1
- **References:** [ADR 0018](../adr/0018-nakama-adapter-seam-and-replacement-criteria.md),
  `crates/backend_nakama`

**Why:** `backend_nakama` implements `HealthService` and `AuthService` only.
Character, inventory, chat, social, faction, calendar, and admin calls cannot use
Nakama today. If Nakama remains the backend of record, the adapter is incomplete;
if direct SQL becomes the backend of record, this task should be closed with
that decision instead.

**Scope**

- Implement the remaining `game_api` service traits over Nakama's HTTP API
  behind the existing seam.
- Map Nakama errors to `ApiError` without leaking HTTP bodies or tokens.
- Keep Nakama types out of `game_api` and engine crates.
- Add shared contract tests against a test Nakama instance; skip when
  unconfigured.
- Document which adapter is authoritative for which data and how migrations are
  handled.

**Acceptance criteria**

- [ ] Every implemented service passes the shared contract suite.
- [ ] Auth, character, and inventory ownership checks match the SQL adapter's
      behavior.
- [ ] Errors map to stable `ErrorCode` categories and retryability.
- [ ] The adapter can be replaced without changing gameplay crates.
- [ ] The decision to keep or retire the adapter is recorded in ADR 0018.

---

## PST-011 — Retention, audit, and privacy

- **Priority:** P2
- **Status:** Blocked — `PST-014`
- **Depends on:** `PST-014`
- **References:** [security](../security.md), migration audit tables

**Scope**

- Define retention for audit events, chat, combat logs, economy entries, and
  moderation records.
- Implement the documented purge path that temporarily allows append-only
  deletion inside a transaction.
- Minimize PII; redact secrets and private chat before write.
- Provide export/delete handling for account data where policy requires it.
- Verify purges do not break foreign keys, history views, or reconciliation.

**Acceptance criteria**

- [ ] Retention jobs are idempotent and audited.
- [ ] Ordinary code still cannot update/delete append-only rows.
- [ ] Purges preserve referential integrity and operator queries.
- [ ] The policy is documented and linked from the security doc.

---

## PST-012 — Multi-world scoping

- **Priority:** P3
- **Status:** Proposed
- **Depends on:** `SIM-014`, `NET-005`
- **References:** migration `0006_world_snapshots.sql`, ADR 0002

**Scope**

- Audit every table for a world/shard key; add `world_id` where missing.
- Scope unique constraints, leases, events, snapshots, and idempotency keys.
- Define routing and configuration for multiple worlds per database.
- Provide a migration path for existing single-world data.

**Acceptance criteria**

- [ ] Two worlds can share a database without ID or lease collisions.
- [ ] Snapshot load/save is scoped to one world.
- [ ] Operator queries cannot accidentally cross worlds.
- [ ] Migration/backfill is documented and reversible.

---

## PST-013 — Content version pinning and migration

- **Priority:** P2
- **Status:** Blocked — `CNT-014`
- **Depends on:** `CNT-014`
- **References:** [architecture §Persistence model](../architecture.md#persistence-model),
  ADR 0017

**Why:** Instances are supposed to record the content version they were created
from, and obsolete component data must migrate or quarantine. The content
pipeline versions bundles, but persisted entities need an enforced link to that
version.

**Scope**

- Record the content bundle/definition version on created instances and
  components.
- On load, migrate known versions through explicit steps or quarantine
  incompatible entities with diagnostics.
- Validate that a zone's content version matches the deployed bundle before
  accepting sessions.
- Provide dry-run migration reports and rollback.

**Acceptance criteria**

- [ ] A world saved under version N loads under a compatible N+1 or is refused
      with a clear migration path.
- [ ] Incompatible entities are quarantined, not silently reset.
- [ ] The deployed content version is visible in readiness/metrics.
- [ ] Rollback restores the previous compatible pair.

---

## PST-014 — Economy and audit ledger

- **Priority:** P2
- **Status:** Blocked — `PST-002`
- **Depends on:** `PST-002`
- **References:** migration `0004_factions.sql` treasury ledger,
  [security §Audit](../security.md)

**Scope**

- Extend the append-only ledger pattern from faction treasury to player
  currency/item value where the economy requires it.
- Record actor, reason, correlation ID, before/after summary, and timestamp.
- Provide reconciliation queries and invariant checks (no negative balances, no
  value created without a source).
- Tie reward/loot/trade grants to idempotency keys.
- Keep the ledger queryable without exposing private data to non-operators.

**Acceptance criteria**

- [ ] Every value movement is traceable to a source and actor.
- [ ] Replaying a grant cannot duplicate value.
- [ ] Reconciliation detects drift between aggregate balances and ledger sums.
- [ ] Operators can query by account, character, correlation ID, and time.
