# OPS-006 / QUA-009 — Migration count and live PostgreSQL test

- **Status:** Open — CI run confirmation pending
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [OPS-006](../TODO/60-operations-release.md#ops-006--ci-matrix-and-live-integration-coverage),
  [QUA-009](../TODO/70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep)
- **Affected workflow:** [CI](../../.github/workflows/ci.yml)

## Resolution

The CI workflow was updated so the Ubuntu Rust job starts a PostgreSQL 17
service, applies the migrations, and runs the workspace test suite with
`LYNX_PG_*` configured. This allows the live PostgreSQL snapshot test to
execute instead of skipping. The migration check now derives the expected head
from `server/migrations` and compares it to `MAX(version)` in
`lynx.schema_migrations`, so adding new migrations no longer breaks CI with a
hardcoded count.

The Rust job additionally requires the PostgreSQL test configuration, so a
missing password fails rather than silently skipping. The issue remains open
until a CI run confirms the live test and migration checks pass.

## Defect

Previously, the PostgreSQL migration job asserted exactly five migrations
even though migration `0006` existed, and the workspace CI test job lacked the
configuration needed to run the live PostgreSQL integration test.

## Impact

The former hardcoded count became stale as migrations were added. The workflow
now configures the live path, but execution in hosted CI remains unconfirmed.

## Acceptance criteria

- [x] CI derives the expected migration head from `server/migrations` instead
  of asserting a fixed count.
- [x] CI configures the live PostgreSQL integration test and fails if its
  password is missing.
- [ ] The integration test is confirmed to execute (not skip) in CI.
- [ ] Applying migrations twice remains successful and the database reports
  the expected migration head.
