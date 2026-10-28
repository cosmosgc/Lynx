# OPS-006 / QUA-009 — Migration count and live PostgreSQL test

- **Status:** Open
- **Reported:** 2026-10-28
- **Related tasks:** [OPS-006](../TODO/60-operations-release.md#ops-006--ci-matrix-and-live-integration-coverage),
  [QUA-009](../TODO/70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep)
- **Affected workflow:** [CI](../../.github/workflows/ci.yml)

## Defect

The PostgreSQL migration job asserts that exactly five migrations have been
applied, but migration `0006` exists. The live PostgreSQL integration test is
also skipped in CI because the workspace test job does not run with the
PostgreSQL test configuration.

## Impact

The hardcoded migration count becomes stale as migrations are added, and CI
does not verify the live PostgreSQL integration path.

## Acceptance criteria

- [ ] CI derives the expected migration head from `server/migrations` instead
  of asserting a fixed count.
- [ ] CI runs the live PostgreSQL integration test with PostgreSQL configured.
- [ ] The integration test is confirmed to execute (not skip) in CI.
- [ ] Applying migrations twice remains successful and the database reports
  the expected migration head.
