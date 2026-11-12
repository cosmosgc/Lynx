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
| 0 | [CLT-008](40-bevy-client.md#clt-008--interaction-inventory-and-ui-panels) — interaction and inventory panels | Server-authoritative inventory operations and panel actions are integrated; equipment, localization, and richer authored layouts remain. |
| 1 | [CLT-014](40-bevy-client.md#clt-014--client-integration-and-protocol-tests) — client integration and protocol tests | Headless multiplayer, reconnect, prediction faults, and versioned fixtures are in place; complete the account-flow integration validation when Nakama is configured. |

The one-zone chat client is delivered by
[CLT-009](40-bevy-client.md#clt-009--chat-and-presence-ui), and the reusable
headless multi-client harness is recorded under
[QUA-014](completed/qua-014-multi-client-headless-harness.md). Graphical
cross-client chat/presence/history validation remains in `CLT-014`; social
channels and moderation remain in `SIM-008`, `SIM-009`, and `NET-009`.
Server-authoritative movement, server-derived combat, and account/character
persistence are complete under `SIM-001`, `SIM-003`, and `PST-001` respectively.
The last recorded workspace-wide Clippy check passed before the current
uncommitted changes; those changes still need the repository's standard gates.

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
| Client scene, input, and camera have an offline foundation and early online presentation; graphical latency/rejection validation is tracked separately | [client backlog](40-bevy-client.md#clt-004--scene-camera-and-input-abstraction) | `CLT-004`–`CLT-017` |
| One-zone gateway and Nakama chat paths are integrated; graphical cross-client validation, social channels, and moderation remain; headless protocol coverage is shared by QUA-014 | [client backlog](40-bevy-client.md#clt-014--client-integration-and-protocol-tests), [SIM-008](10-simulation-gameplay.md#sim-008--world-chat-and-presence), [QUA-014](completed/qua-014-multi-client-headless-harness.md) | `CLT-014`, `SIM-008`, `SIM-009`, `NET-009` |
| Durable inventory and transfer-ledger storage exists behind the trusted persistence seam but is not yet routed by the live world service | [persistence backlog](20-persistence-backend.md#pst-002--inventory-and-transfer-ledger-persistence) | `PST-002` |
| Production admission requires an already-live actor; authoritative character population is incomplete; Nakama social/group REST and realtime operations exist but application-level channel/session integration and live contract validation remain | [persistence backlog](20-persistence-backend.md#pst-001--account-and-character-persistence), [Nakama coverage](20-persistence-backend.md#pst-010--nakama-adapter-coverage), [networking](../networking.md#world-edge-identity-admission) | `SIM-014`, `PST-010` |
| `zone_leases` and `zone_events` fence PostgreSQL authority; checkpoints bound retained history, and entity plus aggregate deltas replay after the checkpoint | [persistence backlog](20-persistence-backend.md#pst-004--zone-leases-fencing-and-event-log) | `PST-004`, `PST-005` |
| Replay includes every mutable snapshot aggregate, quarantine reporting, dry-run verification, checkpoint retention, and measured recovery timing | [architecture](../architecture.md#failure-and-recovery) | `PST-005` |
| PostgreSQL resume sessions survive a restart, but Nakama exposes no role-change feed; the integration that receives authoritative role updates must call the trusted revocation hook | [persistence backlog](20-persistence-backend.md#pst-006--durable-sessions-and-resume-windows) | `PST-006` |
| Session and token abuse defenses, including revocation and attack alerting, remain incomplete | [networking backlog](30-networking-security.md#net-007--session-and-token-abuse-defenses) | `NET-007` |
| Admin Prometheus metrics endpoint is implemented; dashboard definitions and exercised alerts remain missing | [operations](../operations.md#metrics-and-logs) | `OPS-002` |
| Demo GuardChase is active; authored behavior bindings are not yet hydrated from content | [simulation backlog](10-simulation-gameplay.md#sim-006--ai-behaviors-schedules-and-perception-budgets) | `SIM-006` |
| No editor, inspector, link graph, or navigation authoring | [content authoring](../content-authoring.md) | `CNT-001`–`CNT-007` |
| MCP write/publish tools and external client integration remain planned | [AI tooling](../ai-tooling.md) | `CNT-010` |
| User content has schemas/moderation requirements but no implementation | [security](../security.md#content-and-user-generated-content) | `CNT-008`, `NET-009` |
| No staging publish, rollout, rollback, or upgrade workflow | [operations](../operations.md) | `OPS-004`, `OPS-010` |
| No scheduled, encrypted off-host backups or target-environment restore drills | [operations](../operations.md#backups-and-recovery) | `PST-009`, `OPS-003` |
| Linux graphical CI smoke is deferred; Windows has a local account-screen screenshot helper; authenticated character-flow and gameplay rendering are not covered by either smoke | [platforms](../platforms.md), [CI](../../.github/workflows/ci.yml) | `OPS-016` |
| ADR 0022 (dynamic classes and Treat targeting) is Proposed | [ADR 0022](../adr/0022-dynamic-character-classes-and-combat-targeting.md) | `SIM-004` |
| Guilds, quests, progression, economy, trading, parties | [README](../../README.md#phase-8--expand-and-stabilize--selected-systems-delivered-early) | `SIM-009`–`SIM-013` |
| Zone-specific mechanics, shared utilities, player work, demand-based markets, and local-to-faction wealth transfers | [world simulation](../world-simulation.md#zone-systems-work-and-local-economy) | `SIM-019`, `SIM-020`, `CNT-018` |
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
| [`completed/qua-003-protocol-golden-compatibility.md`](completed/qua-003-protocol-golden-compatibility.md) | Versioned wire goldens, including a fake-only credential envelope, protocol compatibility coverage, and CI version/changelog enforcement. |
| [`completed/cnt-009-readonly-mcp-server.md`](completed/cnt-009-readonly-mcp-server.md) | Optional workspace-confined stdio MCP server with read-only project/content diagnostics and bounded JSON results. |
| [`completed/cnt-014-content-migration-tooling.md`](completed/cnt-014-content-migration-tooling.md) | Reviewed source-hash-bound content plans, validated dry-run/apply, and exact-byte rollback for authored content. |
| [`completed/cnt-011-cli-expansion.md`](completed/cnt-011-cli-expansion.md) | Stable CLI JSON failures and exit codes, command-tree shell completions, and a CI-checked generated command reference. |
| [`completed/ops-006-ci-matrix.md`](completed/ops-006-ci-matrix.md) | Hosted OS matrix, live PostgreSQL coverage, headless client smoke, and CI duration/outcome reporting. |
| [`completed/ops-001-metrics-endpoint.md`](completed/ops-001-metrics-endpoint.md) | Separate admin Prometheus endpoint with tested tick/session/action/pathfinding and adapter metrics; unsupported families documented. |
| [`completed/ops-013-correlation-observability.md`](completed/ops-013-correlation-observability.md) | Wire action correlation reaches structured logs and replayable action-result events, with documented redaction and unsampled outcomes. |
| [`completed/ops-008-dependency-audit-sbom.md`](completed/ops-008-dependency-audit-sbom.md) | Pinned cargo-deny policy and CI audit now pair with target-specific CycloneDX SBOMs in release archives and assets. |
| [`completed/clt-004-scene-camera-input.md`](completed/clt-004-scene-camera-input.md) | Offline zone scene, replaceable camera/input adapters, content checks, and presentation fallback policy. |
| [`completed/qua-009-docs-consistency.md`](completed/qua-009-docs-consistency.md) | PostgreSQL and README documentation reconciliation, migration-derived CI checks, and Markdown link-check coverage/limitations. |
| [`completed/sim-005-combat-depth.md`](completed/sim-005-combat-depth.md) | Tick-expiring status, typed mitigation, bounded combat audit, and server-owned death/respawn recovery. |
| [`completed/pst-007-postgres-hardening.md`](completed/pst-007-postgres-hardening.md) | Verified PostgreSQL TLS, bounded connection retries/pooling, loopback plaintext policy, and split migration/runtime roles. |
| [`completed/pst-008-schema-migration-gate.md`](completed/pst-008-schema-migration-gate.md) | Startup schema/checksum gate, read-only doctor inspection, and repeatable migration CI coverage. |
| [`completed/ops-014-docs-changelog-release.md`](completed/ops-014-docs-changelog-release.md) | Release-note and README metadata gates, indexed task completion records, and stale-doc corrections. |
| [`completed/clt-009-chat-presence-ui.md`](completed/clt-009-chat-presence-ui.md) | One-zone chat feed, composer, bounded history, and connected-member presence. |
| [`completed/world-persistence-fresh-start.md`](completed/world-persistence-fresh-start.md) | Durable whole-world snapshots and the `--fresh` authored-world reset. |
| [`completed/sim-001-server-authoritative-movement.md`](completed/sim-001-server-authoritative-movement.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/sim-002-client-prediction-and-reconciliation-contract.md`](completed/sim-002-client-prediction-and-reconciliation-contract.md) | Completion record; all 4 acceptance criteria are checked in the source task. |
| [`completed/sim-003-server-derived-combat-resolution.md`](completed/sim-003-server-derived-combat-resolution.md) | Completion record; all 4 acceptance criteria are checked in the source task. |
| [`completed/sim-007-navigation-budgets-and-replan-policy.md`](completed/sim-007-navigation-budgets-and-replan-policy.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/sim-018-determinism-and-tick-budget.md`](completed/sim-018-determinism-and-tick-budget.md) | Completion record; all 4 acceptance criteria are checked in the source task. |
| [`completed/pst-001-account-and-character-persistence.md`](completed/pst-001-account-and-character-persistence.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/pst-002-inventory-and-transfer-ledger-persistence.md`](completed/pst-002-inventory-and-transfer-ledger-persistence.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/pst-003-normalized-faction-and-calendar-persistence.md`](completed/pst-003-normalized-faction-and-calendar-persistence.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/pst-004-zone-leases-fencing-and-event-log.md`](completed/pst-004-zone-leases-fencing-and-event-log.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/pst-005-snapshot-and-event-replay.md`](completed/pst-005-snapshot-and-event-replay.md) | Completion record; all 6 acceptance criteria are checked in the source task. |
| [`completed/pst-006-durable-sessions-and-resume-windows.md`](completed/pst-006-durable-sessions-and-resume-windows.md) | Completion record; all 4 acceptance criteria are checked in the source task. |
| [`completed/net-001-production-authentication-at-the-world-edge.md`](completed/net-001-production-authentication-at-the-world-edge.md) | Completion record; all 6 acceptance criteria are checked in the source task. |
| [`completed/net-002-rate-limiting-and-message-budgets.md`](completed/net-002-rate-limiting-and-message-budgets.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/clt-001-client-workspace-and-crate-layout.md`](completed/clt-001-client-workspace-and-crate-layout.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/clt-005-network-client-loop.md`](completed/clt-005-network-client-loop.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
| [`completed/clt-006-entity-presentation-and-interpolation.md`](completed/clt-006-entity-presentation-and-interpolation.md) | Completion record; all 9 acceptance criteria are checked in the source task. |
| [`completed/clt-017-rejected-movement-and-desync-evidence.md`](completed/clt-017-rejected-movement-and-desync-evidence.md) | Completion record; all 3 acceptance criteria are checked in the source task. |
| [`completed/qua-004-postgresql-integration-matrix.md`](completed/qua-004-postgresql-integration-matrix.md) | Completion record; all 5 acceptance criteria are checked in the source task. |
