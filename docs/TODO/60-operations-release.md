# Operations and release backlog

Scope: keeping a world service healthy and shipping it — metrics, dashboards,
backups, staging, rollout/rollback, CI, packaging, runbooks, upgrades, secrets,
correlation, docs, and capacity.

**What exists today:** structured logs, health/readiness endpoints, graceful
shutdown with a final snapshot, a checksummed migration runner, native
setup/run scripts, and a CI workflow that runs formatting, Clippy, tests, a
generated-project check, migrations on Ubuntu, and a PostgreSQL 17
service. Partial metrics exist; dashboards, backup drills, staging/rollback automation, client
packaging, load testing, and runbooks are still missing.

**Related design:** [operations](../operations.md),
[platforms](../platforms.md), [getting started](../getting-started.md),
[security](../security.md), `PST-009`, `NET-013`.

---

## OPS-001 — Metrics endpoint

- **Priority:** P1
- **Status:** In progress
- **Depends on:** —
- **References:** [operations §Metrics and logs](../operations.md#metrics-and-logs)

**Why:** The metrics endpoint covers only part of the documented minimum;
replication pressure, session churn details, and database latency remain
invisible until players complain.

**Scope**

- Expose a Prometheus text endpoint (separate from the gameplay listener or
  bound to an admin interface) with documented names and units.
- Cover tick duration/deadlines, entities per zone, sessions/retained windows,
  action throughput and rejections by `ErrorCode`, replication bytes/queue depth,
  resume outcomes, pathfinding/AI budgets, database latency/errors, content
  version, and missing localization keys.
- Keep labels bounded (no session IDs, character names, or secrets).
- Document scrape configuration and a minimal alert set.

**Acceptance criteria**

- [ ] The endpoint returns valid Prometheus text and is covered by a test.
- [ ] Metrics reflect a real tick/session/action scenario.
- [ ] Labels are bounded and contain no PII or secrets.
- [ ] Missing metrics are documented as unsupported, not silently omitted.
- [ ] Readiness and metrics can be scraped without opening the world port.

Partial implementation: the admin listener exports tick execution time, scheduler
deadline misses, overruns and failures, entities, attached and retained sessions, authority inputs and action
requests/rejections, resume success/failure, outbound queue-full events, and
bounded admission and rate-limit rejections, snapshot write outcomes, and store write latency. The remaining metric families and limitations
are listed in operations.
This task remains in progress until the full scope is verified.

---

## OPS-002 — Dashboards and alerts

- **Priority:** P2
- **Status:** Blocked — `OPS-001`
- **Depends on:** `OPS-001`
- **References:** [operations §Metrics and logs](../operations.md#metrics-and-logs)

**Scope**

- Versioned dashboard definitions for zone health, session/reconnect health,
  action errors, replication, database, and content version.
- Alert rules for tick deadline misses, queue saturation, session-expiry spikes,
  auth failures, DB errors, and backup failures.
- SLOs for availability, tick latency, and recovery time.
- A staging exercise that fires each alert.

**Acceptance criteria**

- [ ] Dashboards load from the repository without manual editing.
- [ ] Each alert has a runbook link and a tested threshold.
- [ ] Alerts distinguish a real incident from a deploy/restart.
- [ ] SLOs are documented and reviewed.

---

## OPS-003 — Backup and restore automation

- **Priority:** P1
- **Status:** Blocked — `PST-009`
- **Depends on:** `PST-009`
- **References:** `PST-009`, [security §Abuse prevention](../security.md#abuse-prevention)

**Scope**

- Scheduled full/logical backups plus snapshot export, with retention and
  off-host storage.
- Encryption at rest/in transit and least-privilege access.
- Automated restore verification into a scratch environment.
- Alerting on backup failure or verification mismatch.
- Documented RPO/RTO and a quarterly restore drill.

**Acceptance criteria**

- [ ] A scheduled job produces a restorable backup and records its checksum.
- [ ] Restore verification starts the service and passes a smoke check.
- [ ] Failed backups/restores alert and do not silently age out good backups.
- [ ] Retention and drill results are documented.

---

## OPS-004 — Staging publish, rollout, and rollback

- **Priority:** P2
- **Status:** Blocked — `CNT-014`, `OPS-005`
- **Depends on:** `CNT-014`, `OPS-005`
- **References:** [operations](../operations.md), ADR 0017

**Scope**

- A staging environment that mirrors production topology and content versions.
- Candidate publish flow: validate, diff, approve, deploy, smoke, promote.
- Rollback to the previous engine/content pair with a traceable record.
- Canary/gradual rollout or a documented maintenance-window alternative.
- Audit of who approved/published what and when.

**Acceptance criteria**

- [ ] A content change is validated and smoked in staging before production.
- [ ] Rollback restores the previous compatible engine/content pair.
- [ ] Every publish/rollback is audited with operator, diff, and result.
- [ ] A failed rollout stops and leaves a known-good state.

---

## OPS-005 — Release artifacts and packaging

- **Priority:** P2
- **Status:** In progress — hosted Linux, Windows, PostgreSQL, and Markdown gates confirmed; smoke, macOS, and timing work remain
- **Depends on:** —
- **References:** [platforms](../platforms.md), `CLT-012`

**Scope**

- Versioned server binaries for Linux (and Windows for development), with
  checksums, signatures, and a build stamp (engine/content/protocol).
- Client packages once `CLT-012` lands.
- Reproducible-enough builds and a documented toolchain.
- Release notes generated from `CHANGELOG.md` and the task IDs in commits.
- An install/upgrade/uninstall path for server operators.

**Acceptance criteria**

- [ ] A tagged release produces downloadable artifacts with checksums.
- [ ] The build stamp matches the source tag and content version.
- [ ] A clean machine can install, run, upgrade, and uninstall.
- [ ] Release notes list breaking changes and migrations.

---

## OPS-006 — CI matrix and live-integration coverage

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml),
  [platforms](../platforms.md)

**Why:** CI now confirms Linux and Windows checks plus PostgreSQL migration and
live snapshot coverage. macOS, a true client connect/render/move smoke, and
automated duration/flakiness tracking are still absent. The migration check
derives its head from the migration files.

**Scope**

- Run the workspace test suite with `LYNX_PG_*` configured so the live
  snapshot/store test executes, not just migrations.
- Replace the hardcoded migration count with a head-version check derived from
  `server/migrations`.
- Add a macOS build/test job once a runner and dependencies are available.
- Add a client smoke job once the client exists (launch headless, connect,
  render, move, quit).
- Cache dependencies and keep the pipeline within a documented duration.
- Ensure the "no Docker required" local path stays separate from CI services.

**Acceptance criteria**

- [x] The live PostgreSQL integration test runs and passes in CI.
- [x] Migration head is derived, not hardcoded; adding `0007` does not break CI
      by itself.
- [x] Linux and Windows pass fmt/Clippy/tests in hosted CI; macOS is not enabled.
- [ ] Client smoke coverage runs on at least one platform.
- [ ] CI duration and flakiness are tracked.

**Observed hosted baseline (2026-10-29):** the green run took just under ten
minutes overall; Ubuntu workspace tests took about three minutes and Clippy
about 90 seconds. Windows workspace tests exceeded ten minutes. This is a
manual baseline, not automated duration/flakiness tracking.

---

## OPS-007 — Load and soak harness

- **Priority:** P2
- **Status:** Blocked — `QUA-005`
- **Depends on:** `QUA-005`
- **References:** `QUA-005`, `SIM-018`, `NET-006`

**Scope**

- Synthetic clients that authenticate, move, chat, interact, and reconnect.
- Ramp scenarios for zone population, message rate, entity count, and
  replication bytes.
- Soak runs that detect leaks, queue growth, and latency drift.
- Repeatable reports with hardware/topology and pass/fail thresholds.
- Nightly or pre-release execution.

**Acceptance criteria**

- [ ] A documented command runs a load scenario end to end.
- [ ] Results include tick latency, memory, network, and error rates.
- [ ] Soak runs detect leaks over the target duration.
- [ ] Thresholds are tied to the capacity plan (`OPS-015`).

---

## OPS-008 — Dependency and license auditing

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** [security §Abuse prevention](../security.md#abuse-prevention)

**Scope**

- Run `cargo audit`/`cargo deny` (or equivalents) for vulnerabilities, licenses,
  duplicate crates, and banned sources.
- Pin/update policy and a cadence for security patches.
- SBOM generation for releases.
- Triage and exception process with expiry.

**Acceptance criteria**

- [ ] CI fails on known vulnerable or disallowed dependencies.
- [ ] Exceptions are documented with an owner and expiry.
- [ ] Release artifacts include an SBOM.
- [ ] Security updates have a documented turnaround target.

---

## OPS-009 — Incident runbooks

- **Priority:** P2
- **Status:** Blocked — `OPS-003`
- **Depends on:** `OPS-003`
- **References:** [operations](../operations.md), `PST-005`, `NET-013`

**Scope**

- Startup failure: unmigrated schema, missing secret, corrupt snapshot.
- Database outage/degradation, pool exhaustion, failed migration.
- Zone crash/recovery from snapshot + events.
- Token leak/revocation, auth outage, abusive client, moderation incident.
- Rollback of a bad content/engine release.
- Contact/escalation, evidence capture, and post-incident review template.

**Acceptance criteria**

- [ ] Each runbook has symptoms, diagnosis, safe actions, and rollback.
- [ ] A tabletop or staging exercise validates the top three runbooks.
- [ ] Runbooks are linked from alerts.
- [ ] Post-incident actions become backlog tasks with IDs.

---

## OPS-010 — Engine and content upgrade workflow

- **Priority:** P2
- **Status:** Blocked — `OPS-004`
- **Depends on:** `OPS-004`
- **References:** [world simulation §Engine features and game upgrades](../world-simulation.md#engine-features-and-game-upgrades),
  ADR 0001, ADR 0017

**Scope**

- Declare engine compatibility ranges in generated projects.
- Validate engine/content/protocol pairs before deploy.
- Stage upgrades with migration dry runs and rollback.
- Publish upgrade notes and adoption tracking for game projects.
- Define how feature flags may be used without replacing versioning.

**Acceptance criteria**

- [ ] An incompatible engine/content pair is refused with a clear error.
- [ ] A compatible upgrade preserves content and persistent state or presents an
      explicit migration failure.
- [ ] Rollback restores the previous pair.
- [ ] Every engine release documents upgrade steps.

---

## OPS-011 — Production topology and deployment

- **Priority:** P2
- **Status:** Ready
- **Depends on:** `OPS-001`
- **References:** [operations](../operations.md), [platforms](../platforms.md)

**Scope**

- Reference topology: reverse proxy/TLS, world process(es), PostgreSQL, backups,
  monitoring, log aggregation.
- Native systemd/Windows service examples; containers optional, never required
  for development or tests.
- Resource limits, restart policy, graceful drain, and port exposure.
- Network boundaries and least-privilege database roles.
- A deployment checklist and a staging/production diff.

**Acceptance criteria**

- [ ] A clean host can be deployed from the documented steps.
- [ ] The service drains and restarts without losing the durable world.
- [ ] Only intended ports are exposed; the database is not public.
- [ ] Deployment does not require Docker.

---

## OPS-012 — Production secret injection and rotation

- **Priority:** P1
- **Status:** Blocked — `NET-010`
- **Depends on:** `NET-010`
- **References:** `NET-010`, [operations](../operations.md)

**Scope**

- Inject secrets from an OS/cloud secret store at runtime; do not bake them into
  images or pass them on the command line.
- Rotation runbooks for database, identity, and TLS credentials.
- Startup validation and failure behavior for missing/expired secrets.
- Access audit and break-glass procedure.

**Acceptance criteria**

- [ ] Production can rotate database credentials without downtime.
- [ ] Missing secrets fail startup with an actionable error.
- [ ] Secret access is auditable and least-privilege.
- [ ] No secret is stored in the repository or release artifact.

---

## OPS-013 — Correlation and observability plumbing

- **Priority:** P1
- **Status:** Blocked — `OPS-001`
- **Depends on:** `OPS-001`
- **References:** [operations §Metrics and logs](../operations.md#metrics-and-logs)

**Scope**

- Propagate correlation/request/session IDs through transport, gateway,
  simulation, persistence, and audit logs.
- Structured log schema with service, zone, entity (where safe), action,
  outcome, and duration.
- Sampling and redaction rules; never log tokens, passwords, or private chat.
- Trace export or a documented alternative for latency investigations.

**Acceptance criteria**

- [ ] A single action can be followed from wire receipt to persistence/audit.
- [ ] Logs use a documented schema and stable field names.
- [ ] Sensitive fields are redacted in a tested path.
- [ ] Sampling does not hide errors/audit events.

---

## OPS-014 — Documentation, changelog, and release discipline

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** [`CHANGELOG.md`](../../CHANGELOG.md),
  [`docs/TODO/README.md`](README.md)

**Scope**

- Keep `CHANGELOG.md` current for every user-visible or breaking change.
- Keep the README progress/test counts/ADR index accurate; automate what can be
  automated.
- Require docs updates in the same change as behavior/architecture changes.
- Keep this backlog's gap register and completed records current.
- Document deprecations and migration notes.

**Acceptance criteria**

- [ ] A release cannot ship without changelog and migration notes.
- [ ] README counts/links are checked in CI or reviewed in the release checklist.
- [ ] Closed tasks are moved to `completed/` with their design record.
- [ ] Stale docs are treated as defects, not cleanup.

---

## OPS-015 — Capacity planning

- **Priority:** P3
- **Status:** Proposed
- **Depends on:** `OPS-007`
- **References:** [platforms](../platforms.md), `SIM-018`

**Scope**

- Measure sessions, entities, tick cost, and bytes per world process on
  reference hardware.
- Publish sizing guidance for small/medium zones and database load.
- Define scale-up/scale-out triggers and the multi-zone path (`NET-005`).
- Track cost per concurrent player for hosting decisions.

**Acceptance criteria**

- [ ] Sizing guidance is backed by measured load tests.
- [ ] Thresholds for adding zones/processes are documented.
- [ ] The plan is reviewed when content or protocol changes materially.
- [ ] Assumptions and hardware are recorded with the numbers.
