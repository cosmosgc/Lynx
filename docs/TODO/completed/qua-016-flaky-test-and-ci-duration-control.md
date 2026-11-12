# QUA-016 — Flaky-test and CI-duration control

- **Status:** Done
- **Completed:** 2026-11-12
- **Source task:** [Flaky-test and CI-duration control](../70-testing-quality.md#qua-016--flaky-test-and-ci-duration-control)

## Delivered

- CI publishes elapsed time for Ubuntu, Windows, macOS, and PostgreSQL
  migration jobs, with explicit job time limits and a median-based regression
  gate after ten successful historical samples.
- A manual flake policy records exact test name, platform/workflow, first and
  latest failing runs, owner, and an expiry no more than 30 days out. A
  quarantine requires an explicit skip/filter linked to that record; CI never
  retries failures automatically.
- No flaky tests or quarantines are currently listed. Maintainers use test
  output from failed CI runs to identify and record any future flaky test.
- The timing audit classifies intentional waits, polling loops, documented
  scheduler margins, and injected-delay tests. A failed-auth audit regression
  uses a fixed `Instant` and checks credential-denial and rate-limit events
  separately.

## Verification

- Hosted duration history and thresholds are recorded in the source task,
  including the successful sample counts available on 2026-11-12.
- CI workflow review confirms there is no automatic test retry step.
