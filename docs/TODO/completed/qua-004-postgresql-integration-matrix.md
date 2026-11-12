# QUA-004 — PostgreSQL integration matrix

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Run persistence tests against PostgreSQL 17 in CI with `LYNX_PG_*` set.
- Cover migrations from empty, idempotent re-run, snapshot save/load/delete,
  and account/character CRUD as those land.
- Test failure paths: checksum mismatch, missing schema, corrupt payload,
  serialization failure, and retry.
- Keep local runs skippable when unconfigured; never silently pass a broken
  configured database.
- Isolate test data per run and clean up deterministically.

## Acceptance evidence

- [x] Live persistence tests run in CI, not only locally.
- [x] A configured-but-unreachable database fails loudly.
- [x] Tests are isolated and parallel-safe or explicitly serialized.
- [x] Migration/schema failures produce actionable diagnostics.
- [x] No test depends on Docker for local execution.

**Progress:** `crates/lynx_world/tests/postgres_snapshot.rs` includes a
database-free regression that connects a snapshot store to a released loopback
port and asserts an internal error without echoing the password; a configured
but unreachable database therefore fails instead of skipping. Every writing test
uses per-run world/zone IDs incorporating a process ID and atomic sequence. A
scoped cleanup guard transactionally removes snapshots, events, leases, zone
snapshots, and quarantine rows on normal completion and during unwinding. A live
PostgreSQL regression catches a simulated assertion panic and verifies its
snapshot, event, and lease rows were removed. The full live test binary passed
with `LYNX_REQUIRE_POSTGRES_TEST=1` and its default parallel test execution.

## Source

See [`70-testing-quality.md`](../70-testing-quality.md) for the full task history and implementation notes.
