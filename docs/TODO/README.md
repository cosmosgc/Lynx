# Lynx backlog

This directory is the forward-looking backlog for the Lynx MMORPG engine. It
tracks **what still has to be built**, in what order, and what "done" means.
Design intent lives in the documents under [`docs/`](../); delivery status lives
in [`README.md`](../../README.md#progress) and [`CHANGELOG.md`](../../CHANGELOG.md).
When those disagree with this backlog, the design documents and the code win —
then this backlog gets corrected in the same change.

The official phase scope and exit criteria remain the source of truth in
[project guide §23](../lightweight_mmorpg_engine_project_guide.md#23-suggested-implementation-roadmap).
[`00-roadmap.md`](00-roadmap.md) maps that roadmap onto concrete milestones and
the task IDs below.

## How to read this

| Status | Meaning |
| --- | --- |
| **Ready** | Actionable now; dependencies are met and the design is settled. |
| **In progress** | Work has started; acceptance criteria remain open. |
| **Blocked** | Actionable once the listed dependency lands. |
| **Proposed** | Needs an ADR, design review, or scope decision before implementation. |
| **Icebox** | Deliberately not scheduled; kept so the idea is not lost. |
| **Done** | Completed work, recorded under [`completed/`](completed/). |

| Priority | Meaning |
| --- | --- |
| **P0** | Blocks the next milestone or closes a correctness/security hole. Do next. |
| **P1** | Required for a trustworthy playable or operable milestone. |
| **P2** | Required for live operations, creator tooling, or Phase 8 breadth. |
| **P3** | Opportunistic, research, or post-slice expansion. |

A task is **done** only when the repository's definition of done is met: clear
authority boundary, documented schema/compatibility behavior, tested invalid
input and permissions, reconnect behavior where applicable, diagnostics,
unit/integration coverage, content-editability without an engine fork, and a
rollback or recovery path for persisted state. Every code change must pass:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests stay headless and server-authoritative. Neither Docker nor Nakama may be
required for the workspace test suite; integrations that need them skip cleanly
when unconfigured.

## Recommended current focus

This is the shortest credible path out of the server-only plateau. It is a
recommendation, not a commitment; re-order when a dependency or risk changes.

| Order | Task | Why now |
| --- | --- | --- |
| 0 | [OPS-006](60-operations-release.md#ops-006--ci-matrix-and-live-integration-coverage) — CI integration coverage | The migration head check and live PostgreSQL test are configured; confirm them in CI and complete the remaining platform coverage. |
| 1 | [CLT-004](40-bevy-client.md#clt-004--scene-camera-and-input-abstraction) — scene, camera, and input | The client workspace exists, but the scene is still an offline placeholder; the vertical slice needs an integrated client. |
| 2 | [SIM-002](10-simulation-gameplay.md#sim-002--client-prediction-and-reconciliation-contract) — movement reconciliation | Server movement authority is done; client reconciliation still needs networking, rendering, and reset integration. |
| 3 | [NET-001](30-networking-security.md#net-001--production-authentication-at-the-world-edge) — production auth | Blocked on the live-zone character population boundary; production admission and ownership validation remain important. |

Chat wiring ([SIM-008](10-simulation-gameplay.md#sim-008--world-chat-and-presence))
is the cheapest Phase 3 completion and can run in parallel.
Server-authoritative movement, server-derived combat, and account/character
persistence are complete under `SIM-001`, `SIM-003`, and `PST-001` respectively.
The latest workspace-wide Clippy check passed.

## Backlogs

| Document | Scope | IDs |
| --- | --- | --- |
| [`00-roadmap.md`](00-roadmap.md) | Milestones, sequencing, dependencies, decision gates, risks | — |
| [`10-simulation-gameplay.md`](10-simulation-gameplay.md) | Movement, combat, AI, chat/social, economy, progression, zones, calendar gameplay | `SIM-###` |
| [`20-persistence-backend.md`](20-persistence-backend.md) | Account/character/inventory SQL, zone leases, event replay, snapshots, migrations, backups | `PST-###` |
| [`30-networking-security.md`](30-networking-security.md) | Production auth, rate limits, TLS, protocol compatibility, moderation, abuse, secrets | `NET-###` |
| [`40-bevy-client.md`](40-bevy-client.md) | Client crates, login, scene, input, prediction, rendering, UI, packaging | `CLT-###` |
| [`50-content-editor-tooling.md`](50-content-editor-tooling.md) | Editors, MCP, CLI, templates, assets, localization, content migration | `CNT-###` |
| [`60-operations-release.md`](60-operations-release.md) | Metrics, backups, staging, rollout, CI matrix, packaging, runbooks | `OPS-###` |
| [`70-testing-quality.md`](70-testing-quality.md) | Test strategy, determinism, fuzzing, security tests, docs drift, technical debt | `QUA-###` |
| [`80-icebox-and-research.md`](80-icebox-and-research.md) | Parked research: scripting, UDP, browser, mobile, housing, sharding | `ICE-###` |
| [`completed/`](completed/) | Design records for finished work | — |

## Gap register

Every known gap that is documented elsewhere should have a task. This table is
the quick audit; update it when a gap is found or closed.

| Documented gap | Source | Task |
| --- | --- | --- |
| Client scene, input, and camera have an offline foundation and early online presentation; full integration is outstanding | [client backlog](40-bevy-client.md#clt-004--scene-camera-and-input-abstraction) | `CLT-004`–`CLT-014` |
| Client prediction and reconciliation are not integrated with networking or rendering | [simulation backlog](10-simulation-gameplay.md#sim-002--client-prediction-and-reconciliation-contract) | `SIM-002` |
| World chat exists in `game_api` but is not routed by the gateway | [README](../../README.md#phase-3--interaction-and-mind-system--in-progress) | `SIM-008` |
| Durable per-aggregate inventory and transfer ledger are not implemented | [persistence backlog](20-persistence-backend.md#pst-002--inventory-and-transfer-ledger-persistence) | `PST-002` |
| Persistent account/character adapter is not wired into world admission; other Nakama services remain incomplete | [persistence backlog](20-persistence-backend.md#pst-001--account-and-character-persistence), [networking backlog](30-networking-security.md#net-001--production-authentication-at-the-world-edge) | `PST-010`, `NET-001` |
| `zone_leases` and `zone_events` exist but are unused | migrations 0003 | `PST-004`, `PST-005` |
| Snapshot replay is not implemented; recovery target unproven | [architecture](../architecture.md#failure-and-recovery) | `PST-005` |
| PostgreSQL adapter is plain TCP, no TLS | [operations](../operations.md) | `PST-007`, `NET-003` |
| Sessions and resume windows are process-local | [networking](../networking.md#reconnect-and-recovery) | `PST-006` |
| Session and token abuse defenses, including revocation and attack alerting, remain incomplete | [networking backlog](30-networking-security.md#net-007--session-and-token-abuse-defenses) | `NET-007` |
| No metrics endpoint or dashboards | [operations](../operations.md#metrics-and-logs) | `OPS-001`, `OPS-002` |
| No editor, inspector, link graph, or navigation authoring | [content authoring](../content-authoring.md) | `CNT-001`–`CNT-007` |
| MCP server and external client are planned only | [AI tooling](../ai-tooling.md) | `CNT-009`, `CNT-010` |
| User content has schemas/moderation requirements but no implementation | [security](../security.md#content-and-user-generated-content) | `CNT-008`, `NET-009` |
| No staging publish, rollout, rollback, or upgrade workflow | [operations](../operations.md) | `OPS-004`, `OPS-010` |
| No backup/restore drills | [security](../security.md#abuse-prevention) | `OPS-003` |
| Platform matrix is build-supported, not CI-smoke-tested | [platforms](../platforms.md) | `OPS-006` |
| CI migration and live PostgreSQL checks await run confirmation; platform coverage remains incomplete | [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml), [issue detail](../issues/OPS-006-ci-migration-count-and-live-postgres-test.md) | `OPS-006`, `QUA-009` |
| ADR 0022 (dynamic classes and Treat targeting) is Proposed | [ADR 0022](../adr/0022-dynamic-character-classes-and-combat-targeting.md) | `SIM-004` |
| Guilds, quests, progression, economy, trading, parties | [README](../../README.md#phase-8--expand-and-stabilize--selected-systems-delivered-early) | `SIM-009`–`SIM-013` |
| Markdown link gate is configured; hosted CI confirmation and unsupported Markdown syntax remain open | [quality backlog](70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep) | `QUA-009` |
| No checked-in vulnerability reporting policy or documented private contact | [issue](../issues/AUTH-SecurityDocumentationGap.md) | `QUA-009` |

## Working conventions

- **One task, one intent.** A task may span crates, but it should be reviewable
  as one behavior change. Split it rather than mixing a refactor with a feature.
- **Reference the ID.** Put the task ID in the commit body and PR description;
  link the acceptance criteria that now pass.
- **Update status in place.** When a task lands, tick its acceptance boxes, set
  it to **Done**, and move durable design notes to `completed/`. Do not delete
  the task; the history explains why the code looks the way it does.
- **Dependencies are real.** A `Blocked` task may still be prototyped behind a
  flag, but it cannot close before its dependency is merged.
- **New gaps get an ID.** If a design document names a missing capability,
  either add a task here or say explicitly why it is out of scope.
- **No hidden work.** Temporary follow-ups belong in this directory, not in code
  comments, chat threads, or commit messages alone.
- **Keep the gates.** A task is not done with failing formatting, Clippy, or
  tests. Database/Nakama-dependent tests must skip cleanly when unconfigured.

## Out of scope

The backlog does not promise dates, staffing, or a public release. It also does
not treat every idea in the project guide as committed: entries marked
**Proposed** or **Icebox** need an explicit decision before they consume
schedule. Anything that would weaken server authority, require Docker for the
test suite, hide work from headless tests, or make content require an engine fork
is rejected by default.

## Completed

| Record | Summary |
| --- | --- |
| [`completed/world-persistence-fresh-start.md`](completed/world-persistence-fresh-start.md) | Durable whole-world snapshots and the `--fresh` authored-world reset. |
