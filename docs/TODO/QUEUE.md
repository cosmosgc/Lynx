# Backlog queue

Working view of the actionable backlog; task documents remain the source of truth.
Items are ordered by priority, then by declared dependency and milestone leverage.
Independent tasks at the same priority can proceed in parallel. Dependencies are
copied from the task entries; a dash means none are declared.

## Ready

| ID | Task | Priority | Status | Dependencies | Sequencing note |
| --- | --- | --- | --- | --- | --- |
| [OPS-006](60-operations-release.md#ops-006--ci-matrix-and-live-integration-coverage) | CI matrix and live-integration coverage | P1 | Ready | — | Close client-smoke and CI-duration gaps; useful parallel quality gate. |
| [SIM-007](10-simulation-gameplay.md#sim-007--navigation-budgets-and-replan-policy) | Navigation budgets and replan policy | P1 | Ready | `SIM-001` | Formalize bounded navigation before authoring integration. |
| [SIM-018](10-simulation-gameplay.md#sim-018--determinism-and-tick-budget) | Determinism and tick budget | P1 | Ready | — | Establish measured simulation limits alongside feature work. |
| [PST-007](20-persistence-backend.md#pst-007--postgresql-connection-hardening) | PostgreSQL connection hardening | P1 | Ready | — | Harden remote database access independently. |
| [PST-009](20-persistence-backend.md#pst-009--backup-and-restore-verification) | Backup and restore verification | P1 | Ready | — | Define recovery guarantees before OPS-003 automation. |
| [NET-006](30-networking-security.md#net-006--replication-delivery-slo-and-backpressure) | Replication delivery SLO and backpressure | P1 | Ready | — | Measure delivery limits; informs performance/load work. |
| [NET-007](30-networking-security.md#net-007--session-and-token-abuse-defenses) | Session and token abuse defenses | P1 | Ready | `NET-002` | Build on existing rate limits; security priority. |
| [NET-010](30-networking-security.md#net-010--secret-management-and-rotation) | Secret management and rotation | P1 | Ready | — | Upstream for blocked OPS-012. |
| [QUA-002](70-testing-quality.md#qua-002--determinism-and-property-based-tests) | Determinism and property-based tests | P1 | Ready | — | Parallel with SIM-018; adds invariant/replay assurance. |
| [QUA-004](70-testing-quality.md#qua-004--postgresql-integration-matrix) | PostgreSQL integration matrix | P1 | Ready | `PST-001` | Expand live persistence coverage in parallel with persistence work. |
| [QUA-014](70-testing-quality.md#qua-014--multi-client-headless-harness) | Multi-client headless harness | P1 | Ready | `CLT-005` | Start when client network loop lands; unblocks QUA-014 consumers. |
| [OPS-014](60-operations-release.md#ops-014--documentation-changelog-and-release-discipline) | Documentation, changelog, and release discipline | P1 | Ready | — | Continuous process task; keep docs aligned with delivery. |
| [OPS-011](60-operations-release.md#ops-011--production-topology-and-deployment) | Production topology and deployment | P2 | Ready | `OPS-001` | Follows completion of metrics foundation. |
| [PST-006](20-persistence-backend.md#pst-006--durable-sessions-and-resume-windows) | Durable sessions and resume windows | P2 | Ready | `PST-001`, `NET-001` | Identity and world-edge admission foundations are complete. |
| [CNT-014](50-content-editor-tooling.md#cnt-014--content-migration-and-upgrade-tooling) | Content migration and upgrade tooling | P2 | Ready | `CNT-011` | Continue after CLI expansion; prerequisite for OPS-004/PST-013. |
| [CNT-009](50-content-editor-tooling.md#cnt-009--mcp-server-read-only-tools) | MCP server: read-only tools | P2 | Ready | `CNT-011` | Reuse the CLI automation contracts; security-bounded read-only scope. |
| [CNT-013](50-content-editor-tooling.md#cnt-013--asset-pipeline-and-import-policy) | Asset pipeline and import policy | P2 | Ready | `CNT-005` | Depends on definition-editor foundation. |
| [CNT-015](50-content-editor-tooling.md#cnt-015--localization-tooling) | Localization tooling | P2 | Ready | `CLT-010` | Follows client localization foundation. |
| [QUA-005](70-testing-quality.md#qua-005--load-and-soak-benchmarks) | Load and soak benchmarks | P2 | Ready | — | Establish repeatable baseline; prerequisite for OPS-007. |
| [QUA-007](70-testing-quality.md#qua-007--coverage-reporting) | Coverage reporting | P2 | Ready | — | Add visibility, then target critical-path gaps. |
| [QUA-008](70-testing-quality.md#qua-008--fuzzing) | Fuzzing | P2 | Ready | — | Independent bounded security/robustness work. |
| [QUA-010](70-testing-quality.md#qua-010--error-taxonomy-and-diagnostics-audit) | Error taxonomy and diagnostics audit | P2 | Ready | — | Improve diagnostics across concurrent feature work. |
| [QUA-011](70-testing-quality.md#qua-011--toolchain-and-bevy-upgrade-rehearsal) | Toolchain and Bevy upgrade rehearsal | P2 | Ready | `CLT-001` | Client foundation exists; keep upgrade work isolated. |
| [NET-013](30-networking-security.md#net-013--security-audit-logging-and-alerting) | Security audit logging and alerting | P2 | Ready | `NET-001`, `NET-002` | Builds on existing admission and rate-limit foundations. |
| [QUA-017](70-testing-quality.md#qua-017--remaining-architecture-decision-records) | Remaining architecture decision records | P2 | Ready | — | Resolve documented decision gates before dependent design work. |
| [CNT-017](50-content-editor-tooling.md#cnt-017--schema-reference-and-documentation-generator) | Schema reference and documentation generator | P3 | Ready | `CNT-005` | Lower-priority follow-on to definition schemas. |

## Resume — In progress

| ID | Task | Priority | Status | Dependencies | Sequencing note |
| --- | --- | --- | --- | --- | --- |
| [SIM-002](10-simulation-gameplay.md#sim-002--client-prediction-and-reconciliation-contract) | Client prediction and reconciliation contract | P0 | In progress | `SIM-001`, `CLT-005` | Resume visual validation when client/network environment is available. |
| [CLT-002](40-bevy-client.md#clt-002--login-and-session-bootstrap) | Login and session bootstrap | P0 | In progress | `NET-001`, `CLT-001` | Finish server profiles and world-session lifecycle. |
| [CLT-003](40-bevy-client.md#clt-003--character-selection-and-creation) | Character selection and creation | P0 | In progress | `PST-001`, `CLT-002` | Verify full Bevy onboarding and restart flow against a live server. |
| [SIM-006](10-simulation-gameplay.md#sim-006--ai-behaviors-schedules-and-perception-budgets) | AI behaviors, schedules, and perception budgets | P1 | In progress | `SIM-001` | Continue bounded behavior/content hydration; prerequisite of SIM-015. |
| [SIM-008](10-simulation-gameplay.md#sim-008--world-chat-and-presence) | World chat and presence | P1 | In progress | — | Complete authorized routing/moderation path; enables social/client tasks. |
| [NET-003](30-networking-security.md#net-003--tls-and-reverse-proxy-boundary) | TLS and reverse-proxy boundary | P1 | In progress | — | Validate proxy configuration and deployed certificate boundary. |
| [NET-004](30-networking-security.md#net-004--protocol-compatibility-lifecycle) | Protocol compatibility lifecycle | P1 | In progress | — | Finish world-message fixtures and breaking-change gate. |
| [OPS-001](60-operations-release.md#ops-001--metrics-endpoint) | Metrics endpoint | P1 | In progress | — | Close or formally scope pathfinding/localization metric gaps. |
| [CNT-011](50-content-editor-tooling.md#cnt-011--cli-expansion) | CLI expansion | P1 | In progress | — | Complete generated reference and shell completion/automation contracts. |
| [QUA-001](70-testing-quality.md#qua-001--movement-and-combat-integration-suite) | Movement and combat integration suite | P0 | In progress | `SIM-001`, `SIM-003` | Complete remaining invariant/budget matrix. |
| [QUA-003](70-testing-quality.md#qua-003--protocol-golden-and-compatibility-tests) | Protocol golden and compatibility tests | P1 | In progress | — | Add all world-message fixtures and breaking-change enforcement. |
| [QUA-006](70-testing-quality.md#qua-006--security-abuse-test-suite) | Security abuse test suite | P1 | In progress | — | Expand abuse matrix and secret-redaction/side-effect assertions. |
| [OPS-005](60-operations-release.md#ops-005--release-artifacts-and-packaging) | Release artifacts and packaging | P2 | In progress | — | Verify tagged release; client packages/macOS remain later work. |
| [OPS-008](60-operations-release.md#ops-008--dependency-and-license-auditing) | Dependency and license auditing | P1 | In progress | — | Add release SBOM generation/attachment. |
| [PST-005](20-persistence-backend.md#pst-005--snapshot-and-event-replay) | Snapshot and event replay | P1 | In progress | `PST-004` | Entity delta replay, durable quarantine, dry-run verification, and a measured 60-second reference-recovery test are in place; next add non-entity delta coverage and event retention. |
| [QUA-016](70-testing-quality.md#qua-016--flaky-test-and-ci-duration-control) | Flaky-test and CI-duration control | P2 | In progress | — | Automate duration/flakiness reporting and audit timing-sensitive tests. |

## Blocked

Dependencies below are the task's declared blockers; do not treat this list as
permission to start or to alter task status.

| ID | Task | Priority | Status | Dependencies | Sequencing note |
| --- | --- | --- | --- | --- | --- |
| [PST-003](20-persistence-backend.md#pst-003--normalized-faction-and-calendar-persistence) | Normalized faction and calendar persistence | P1 | Blocked | `PST-001`, `PST-005` | Requires replay/persistence foundations. |
| [OPS-002](60-operations-release.md#ops-002--dashboards-and-alerts) | Dashboards and alerts | P2 | Blocked | `OPS-001` | Follows metrics completion. |
| [OPS-003](60-operations-release.md#ops-003--backup-and-restore-automation) | Backup and restore automation | P1 | Blocked | `PST-009` | Follows verified backup/restore procedure. |
| [OPS-004](60-operations-release.md#ops-004--staging-publish-rollout-and-rollback) | Staging publish, rollout, and rollback | P2 | Blocked | `CNT-014`, `OPS-005` | Requires content migration and release workflow. |
| [NET-005](30-networking-security.md#net-005--multi-zone-gateway-routing) | Multi-zone gateway routing | P2 | Blocked | `SIM-014`, `PST-004` | Wait for travel and lease foundations. |
| [NET-008](30-networking-security.md#net-008--admin-and-moderation-actions) | Admin and moderation actions | P2 | Blocked | `NET-001` | Requires production identity/roles. |
| [NET-009](30-networking-security.md#net-009--chat-and-content-moderation-pipeline) | Chat and content moderation pipeline | P2 | Blocked | `SIM-008`, `CNT-008` | Requires chat routing and content policy. |
| [NET-011](30-networking-security.md#net-011--anti-cheat-and-anomaly-detection) | Anti-cheat and anomaly detection | P2 | Blocked | `NET-002`, `SIM-001`, `SIM-003` | Depends on rate limits and server-authoritative gameplay. |
| [SIM-005](10-simulation-gameplay.md#sim-005--combat-depth-status-mitigation-death-respawn) | Combat depth: status, mitigation, death, respawn | P2 | Blocked | `SIM-003` | Builds on server-derived combat. |
| [SIM-009](10-simulation-gameplay.md#sim-009--parties-and-social-graph) | Parties and social graph | P2 | Blocked | `SIM-008`, `PST-006` | Requires chat/presence and durable sessions. |
| [SIM-010](10-simulation-gameplay.md#sim-010--trade-and-economy-integrity) | Trade and economy integrity | P2 | Blocked | `SIM-009`, `PST-002`, `PST-014` | Requires social, inventory, and ledger work. |
| [SIM-013](10-simulation-gameplay.md#sim-013--loot-and-encounter-rewards) | Loot and encounter rewards | P2 | Blocked | `SIM-003`, `PST-014` | Requires combat credit and durable ledger. |
| [SIM-014](10-simulation-gameplay.md#sim-014--additional-zones-and-travel) | Additional zones and travel | P2 | Blocked | `SIM-001`, `PST-005` | Requires authoritative movement and replay. |
| [SIM-015](10-simulation-gameplay.md#sim-015--calendar-objectives-and-npc-participation) | Calendar objectives and NPC participation | P2 | Blocked | `SIM-001`, `SIM-006` | Requires bounded AI behavior. |
| [SIM-016](10-simulation-gameplay.md#sim-016--faction-and-territory-edge-hardening) | Faction and territory edge hardening | P2 | Blocked | `PST-003` | Follows normalized faction persistence. |
| [SIM-017](10-simulation-gameplay.md#sim-017--minigame-and-panel-expansion) | Minigame and panel expansion | P3 | Blocked | `CNT-005` | Requires definition/panel authoring. |
| [SIM-019](10-simulation-gameplay.md#sim-019--zone-systems-and-shared-utilities) | Zone systems and shared utilities | P2 | Blocked | `SIM-014`, `PST-005`, `SIM-018`, `CNT-005` | Foundation for systemic zones; requires multi-zone authority, durable recovery, measured tick budgets, and definition schemas. |
| [SIM-020](10-simulation-gameplay.md#sim-020--zone-work-dynamic-markets-and-faction-contribution) | Zone work, dynamic markets, and faction contribution | P2 | Blocked | `SIM-019`, `SIM-010`, `SIM-011`, `SIM-014`, `PST-003`, `PST-014` | Builds local jobs/trade on shared zone systems and durable faction/economy accounting. |
| [CNT-018](50-content-editor-tooling.md#cnt-018--zone-systems-work-and-market-authoring) | Zone systems, work, and market authoring | P2 | Blocked | `CNT-005`, `SIM-019`, `SIM-020` | Adds creator-facing schemas and validation after runtime contracts exist. |
| [PST-011](20-persistence-backend.md#pst-011--retention-audit-and-privacy) | Retention, audit, and privacy | P2 | Blocked | `PST-014` | Follows economy/audit ledger. |
| [PST-013](20-persistence-backend.md#pst-013--content-version-pinning-and-migration) | Content version pinning and migration | P2 | Blocked | `CNT-014` | Requires content migration tooling. |
| [PST-014](20-persistence-backend.md#pst-014--economy-and-audit-ledger) | Economy and audit ledger | P2 | Blocked | `PST-002` | Requires durable inventory transfers. |
| [CNT-002](50-content-editor-tooling.md#cnt-002--world-atlas-and-map-editor) | World atlas and map editor | P1 | Blocked | `CNT-001`, `SIM-014` | Requires editor and multi-zone travel foundations. |
| [CNT-003](50-content-editor-tooling.md#cnt-003--entity-placement-and-inspector) | Entity placement and inspector | P1 | Blocked | `CNT-001` | Requires editor shell. |
| [CNT-005](50-content-editor-tooling.md#cnt-005--definition-editor) | Definition editor | P1 | Blocked | `CNT-001` | Requires editor shell. |
| [CNT-004](50-content-editor-tooling.md#cnt-004--link-graph-editor) | Link graph editor | P2 | Blocked | `CNT-003` | Follows entity placement/inspection. |
| [CNT-006](50-content-editor-tooling.md#cnt-006--navigation-authoring-and-visualization) | Navigation authoring and visualization | P2 | Blocked | `CNT-002`, `SIM-007` | Requires atlas authoring and nav policy. |
| [CNT-007](50-content-editor-tooling.md#cnt-007--behavior-graph-editor-and-runtime-debugger) | Behavior graph editor and runtime debugger | P3 | Blocked | `SIM-006` | Follows content-defined AI behaviors. |
| [CNT-010](50-content-editor-tooling.md#cnt-010--mcp-server-write-and-publish-tools) | MCP server: write and publish tools | P3 | Blocked | `CNT-009`, `OPS-004` | Requires read-only MCP and staging publish workflow. |
| [CNT-012](50-content-editor-tooling.md#cnt-012--playable-mmorpg-template) | Playable MMORPG template | P1 | Blocked | `CLT-009`, `SIM-008`, `CNT-011` | Requires chat and CLI foundations. |
| [CNT-016](50-content-editor-tooling.md#cnt-016--calendar-and-faction-editor-overlays) | Calendar and faction editor overlays | P2 | Blocked | `CNT-002`, `SIM-015` | Requires atlas editor and calendar objectives. |
| [CLT-005](40-bevy-client.md#clt-005--network-client-loop) | Network client loop | P0 | Blocked | `CLT-001`, `CLT-002` | Unblocks client presentation and harness work. |
| [CLT-006](40-bevy-client.md#clt-006--entity-presentation-and-interpolation) | Entity presentation and interpolation | P0 | Blocked | `CLT-004`, `CLT-005` | Requires online client loop. |
| [CLT-007](40-bevy-client.md#clt-007--local-movement-and-prediction) | Local movement and prediction | P0 | Blocked | `SIM-001`, `SIM-002`, `CLT-005` | Requires client transport and reconciliation. |
| [CLT-008](40-bevy-client.md#clt-008--interaction-inventory-and-ui-panels) | Interaction, inventory, and UI panels | P1 | Blocked | `CLT-006`, `SIM-008` | Requires entity presentation and chat/presence foundation. |
| [CLT-009](40-bevy-client.md#clt-009--chat-and-presence-ui) | Chat and presence UI | P1 | Blocked | `SIM-008`, `CLT-008` | Requires server chat and panel foundation. |
| [CLT-010](40-bevy-client.md#clt-010--localization-and-accessibility) | Localization and accessibility | P1 | Blocked | `CLT-008` | Follows core client UI. |
| [CLT-011](40-bevy-client.md#clt-011--settings-diagnostics-and-recovery-ux) | Settings, diagnostics, and recovery UX | P2 | Blocked | `CLT-005`, `CLT-007` | Requires networking and movement UX. |
| [CLT-012](40-bevy-client.md#clt-012--packaging-and-platform-builds) | Packaging and platform builds | P2 | Blocked | `OPS-005`, `CLT-004` | Requires release-artifact workflow. |
| [CLT-013](40-bevy-client.md#clt-013--client-performance-profiling) | Client performance profiling | P2 | Blocked | `CLT-006`, `SIM-018` | Requires online presentation and simulation budget. |
| [CLT-014](40-bevy-client.md#clt-014--client-integration-and-protocol-tests) | Client integration and protocol tests | P1 | Blocked | `CLT-005`, `QUA-014` | Requires network loop and reusable harness. |
| [OPS-007](60-operations-release.md#ops-007--load-and-soak-harness) | Load and soak harness | P2 | Blocked | `QUA-005` | Follows benchmark harness. |
| [OPS-009](60-operations-release.md#ops-009--incident-runbooks) | Incident runbooks | P2 | Blocked | `OPS-003` | Use verified restore procedures. |
| [OPS-010](60-operations-release.md#ops-010--engine-and-content-upgrade-workflow) | Engine and content upgrade workflow | P2 | Blocked | `OPS-004` | Requires staging rollout/rollback. |
| [OPS-012](60-operations-release.md#ops-012--production-secret-injection-and-rotation) | Production secret injection and rotation | P1 | Blocked | `NET-010` | Follows secret-management contract. |
| [OPS-013](60-operations-release.md#ops-013--correlation-and-observability-plumbing) | Correlation and observability plumbing | P1 | Blocked | `OPS-001` | Follows metrics/observability foundation. |
| [QUA-012](70-testing-quality.md#qua-012--benchmark-regression-gates) | Benchmark regression gates | P3 | Blocked | `QUA-005` | Follows repeatable benchmarks. |
| [QUA-013](70-testing-quality.md#qua-013--accessibility-and-localization-verification) | Accessibility and localization verification | P2 | Blocked | `CLT-010` | Follows client localization/accessibility implementation. |
