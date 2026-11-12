# PST-002 — Inventory and transfer-ledger persistence

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Persist inventory slots and slot versions transactionally.
- Persist transfer receipts/idempotency keys so retries after a restart cannot
  duplicate or lose value.
- Enforce container rules, capacity, and ownership in the same transaction as
  the slot update.
- Emit an append-only ledger entry for value movement where the economy design
  requires it.
- Provide reconciliation queries for operators.

## Acceptance evidence

- [x] A transfer applied twice returns the original result and moves value once.
- [x] A failed transaction leaves both inventories unchanged.
- [x] Restarting between request and retry preserves idempotency.
- [x] Slot versions prevent lost updates under concurrency.
- [x] Inventory state and audit/ledger entries agree after fault injection.

**Implementation:** `PostgresInventoryStore` atomically persists validated
inventory snapshots and optimistic versions. `commit_transfer` locks both
character aggregates in a stable order, checks both versions, replaces both
snapshots, advances both versions, and writes the append-only receipt in one
transaction. Idempotency keys are serialized with a transaction advisory lock
and are bound to the original owners and receipt; retries after reconnect return
the original receipt. `reconcile_character` reports slot/quantity/version counts
alongside transfer history counts. Migrations `0008` and `0009` add revision and
append-only receipt tables.

**Validation:** Live PostgreSQL integration coverage verifies stale-version
conflicts, rollback after a destination-side unique-item failure, concurrent
same-key retries, concurrent stale-version writes, replay after reconnect,
receipt immutability outside the purge path, and reconciliation counts. Both
`cargo test -p backend_memory -p backend_nakama` (with
`LYNX_REQUIRE_POSTGRES_TEST=1`) and scoped Clippy/fmt checks pass.

## Source

See [`20-persistence-backend.md`](../20-persistence-backend.md) for the full task history and implementation notes.
