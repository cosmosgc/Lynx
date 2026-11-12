# OPS-001 — Metrics endpoint

**Completed:** 2026-11-12

The admin listener exposes Prometheus text metrics separately from the gameplay
listener. Supported families cover tick execution/deadlines, current entities
and sessions, inputs/actions/rejections, replication queue and byte pressure,
resume outcomes, AI/pathfinding budgets, snapshot writes, content integration
state, transport admission, and Nakama/PostgreSQL adapter operation counts,
errors, and durations. Labels use fixed error, outcome, budget, adapter, and
transport categories rather than user identifiers or secrets.

`docs/operations.md` documents scrape configuration, metric names and units,
candidate alerts, and every unsupported or incomplete metric family. The world
service does not resolve localized catalogs, so localization fallback and
missing-key metrics are explicitly unsupported until a server-side catalog
consumer exists. Per-query/pool telemetry and staging alert tuning remain
separate operational work.

**Verification:**

- `cargo test -p lynx-world --lib metrics` — 7 passed, 0 failed. This includes
  HTTP exposure on the separate admin listener, Prometheus text/redaction,
  bounded backend labels, and a real session/input/tick/pathfinding scenario.
- All five OPS-001 acceptance criteria are checked in
  [`60-operations-release.md`](../60-operations-release.md).
