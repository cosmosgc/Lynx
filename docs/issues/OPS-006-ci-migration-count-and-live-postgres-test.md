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

The PostgreSQL migration job asserts that exactly five migrations have been
applied, but migration `0006` exists. The live PostgreSQL integration test is
also skipped in CI because the workspace test job does not run with the
PostgreSQL test configuration.

## Impact

The hardcoded migration count becomes stale as migrations are added, and CI
does not verify the live PostgreSQL integration path.

## Acceptance criteria

- [x] CI derives the expected migration head from `server/migrations` instead
  of asserting a fixed count.
- [x] CI configures the live PostgreSQL integration test and fails if its
  password is missing.
- [ ] The integration test is confirmed to execute (not skip) in CI.
- [ ] Applying migrations twice remains successful and the database reports
  the expected migration head.
