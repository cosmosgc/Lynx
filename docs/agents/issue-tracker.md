# Issue tracker: GitHub

Issues and specs for this repo live as GitHub issues. Use the `gh` CLI for all operations.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --json number,title,body,labels,comments`.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Make an issue a sub-issue of a parent**: `gh issue create --parent <parent> ...`, or `gh issue edit <parent> --add-sub-issue <child>` afterwards (`gh` 2.94+). Older `gh`: `gh api --method POST repos/<owner>/<repo>/issues/<parent>/sub_issues -F sub_issue_id=<child-db-id>` (database id, as in **Blocking** below). Without sub-issues, put `Part of #<parent>` at the top of the child body.
- **Blocking**: GitHub's **native issue dependencies**, the canonical, UI-visible representation. Add an edge with `gh issue create --blocked-by <n>,<n> ...` or `gh issue edit <child> --add-blocked-by <blocker>` (`gh` 2.94+). Older `gh`: `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Count open blockers**: `gh api repos/<owner>/<repo>/issues/<n> --jq .issue_dependencies_summary.blocked_by` (open blockers only; 0 means startable). `gh issue view <n> --json blockedBy` also lists closed blockers, so filter it: `--jq '[.blockedBy.nodes[] | select(.state == "OPEN")] | length'`.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: post the explanation with `gh issue comment <number> --body "..."`, then `gh issue close <number>`. On an already-closed issue, `gh issue close --comment` drops the comment and still exits 0.

Infer the repo from `git remote -v`; `gh` does this automatically when run inside a clone.

File issues only into the user's own repo. Never file into `mattpocock/skills` (the repo these skills come from) unless the user owns it. In a fork, `gh` can default to the upstream parent, so before the first `gh issue create` of a session, check the target with `gh repo view --json nameWithOwner` and confirm it with the user if it is not their repo.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number>`, then `gh pr view <number> --comments` (off a TTY, `--comments` prints only the comments, not the body), and `gh pr diff <number>` for the diff.
- **List external PRs for triage**: `gh api --paginate 'repos/{owner}/{repo}/pulls?state=open' --jq '.[] | select(.author_association | IN("OWNER","MEMBER","COLLABORATOR") | not) | {number, title, author: .user.login, author_association, labels: [.labels[].name]}'`.
- **Comment / label / close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either: resolve with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Read it as in **Read an issue** above.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (see **Make an issue a sub-issue of a parent**). Where sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: see **Blocking** under Conventions.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (see **Count open blockers**, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me`, the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.

## Local completion blockers

Record backlog work that cannot be completed in the current environment here,
with the missing external dependency and the next action needed. Remove an entry
when that dependency is available and the work resumes.

### 2026-11-12

- **NET-006 — Measured replication delivery budgets:** A bounded admin route
  now retains per-session frame/byte counts and queue occupancy for up to 256
  recently active opaque session IDs; Prometheus metrics remain fixed-cardinality.
  The local checkout cannot establish the target byte/tick budget or numeric
  delivery SLO without the reference-topology load/soak run. Run `lynx-load`
  against provisioned staging, review repeated queue/write and tick measurements
  at the target entity count, then set and document the delivery targets.

- **NET-003 — Certificate-backed proxy deployment:** The reference Nginx
  configuration passed `nginx -t` locally with Nginx 1.31.8 and an ephemeral
  self-signed certificate; CI now repeats the syntax check with Ubuntu Nginx.
  This proves configuration parsing but does not verify the public certificate,
  proxy routing, timeouts, or WebSocket upgrade in a deployed topology. Exercise
  the HTTPS endpoint and `/world` upgrade in staging using the deployment
  certificate, then record the Nginx version and handshake result before closing
  NET-003.

- **CNT-001 — Interactive editor acceptance:** The standalone Bevy editor now
  compiles with project loading, map placement edits, shared validation,
  undo/redo, recovery autosave, and offline-client preview. A fresh desktop
  inventory returned no native apps or browsers; native control failed to
  connect to its pipe and the browser host was unavailable. The open/edit/
  undo/redo/save/reopen and crash-recovery scenarios therefore remain
  unexercised. Resume those acceptance steps in a working Windows desktop
  session, capture the results, then close CNT-001 or record any concrete
  defects found.

- **SIM-006 — Content activation, seasonal time, and privileged AI inspection:**
  The bounded graph runtime supports looping `patrol`, `flee_target`, and
  validated `interact_target` instructions. `lynx-world --content-bundle` now
  loads verified behavior definitions and merges matching IDs into the running
  AI policy. The current world calendar has no season identity or
  season-boundary contract; define that calendar concept before adding seasonal
  behavior windows. The retained AI tick report is server-side runtime
  inspection, but a durable privileged inspection endpoint depends on the
  production identity, role mapping, and audit path tracked by NET-008. Resume
  those pieces after NET-008 provides its runtime seam.

- **SIM-014 — Multi-zone runtime and independent crash recovery:** Authored
  route validation, staged transfer coordination, leases, fair scheduling,
  per-zone snapshot adapters, stable zone construction, and static entity
  placements are present.
  `lynx-world serve --world-map <id> --content-bundle <path> --state-postgres`
  now acquires every zone lease, restores each fenced checkpoint, and serves
  `/world/{zone_id}` through `MultiZoneService`; `/world` routes authenticated
  characters to the zone containing their persistent actor and admits new
  characters only to the stable first zone ID. Durable resume records now
  include their owning zone, allowing non-consuming route lookup and
  zone-scoped stale-session cleanup; a wrong explicit zone is rejected before
  token rotation. Migration 0018 revokes pre-zone resume rows instead of
  assigning them to an arbitrary zone. Authored behavior bindings, faction
  relation refresh, and trigger-driven travel through `handoff_player_durable`
  are now connected. Calendar mutation still lacks a map-wide aggregate owner.
  PostgreSQL 17 migrations 0001–0018 applied cleanly on an isolated cluster;
  snapshot recovery/lease tests passed 10/10, and resume/session recovery passed
  6/6. The remaining SIM-014 acceptance is a service-level handoff crash-window
  run that checks both snapshots, durable route update, receipt, readiness, and
  survivor-zone continuity; the current database suites do not exercise those
  handoff commit boundaries together.

- **SIM-014 — Live travel handoff integration:** Zone definitions now carry
  bounded `travel_triggers`; bundle validation requires each destination to be
  placed on the same map and connected by an explicit directed route, while
  runtime construction checks that trigger positions lie inside the source
  zone on unblocked cells and resolves the destination's safe entry point.
  This keeps ADR 0019's topology-only atlas contract intact. Client proximity
  checks use server-replicated actor position and send only a trigger ID;
  the service revalidates range and route. The authority loop calls
  `handoff_player_durable`, and PostgreSQL commits both zone snapshots, the
  receipt, and durable resume-session zone routing in one transaction. The old
  socket closes after commit so the client reconnects through the destination
  route. Isolated PostgreSQL migration, snapshot recovery, lease-fencing, and
  resume-session checks passed on 2026-11-12. The service-level handoff crash
  window and survivor-zone acceptance remains open under the recovery item
  below.

- **SIM-014 — Typed runtime bindings for placed entities:** Zone definitions
  can now place stable static instances of entity definitions. Startup validates
  each placement's bounds/navigation cell, copies validated extension component
  data and tags, binds `core.interactable` to the simulation's authoritative
  interaction component, `core.door` to open/closed door state, `core.health`
  to damageable hit points, and `core.movement` to collision radius and
  occupancy. `core.behavior` assigns a validated behavior definition to an AI
  mind, and authored zones install the bounded AI policy; patrol is active, but
  hostile target selection uses the faction catalog loaded at authored-map
  startup. It rebuilds
  `static_definition` instances after snapshot recovery using stable IDs, so
  damaged authored health resets to full on restart. Locked door key/minigame
  policy and per-placement persistence/overrides remain unimplemented.
  Zone-local authored activation links now resolve typed `link.*` definitions,
  validate source/target components, and rebuild after snapshot recovery.
  Cross-zone links are not supported. Do not treat preserved extension data as
  active gameplay behavior.

- **SIM-014 — Expanded authored link actions and cross-zone links:** `link.*`
  definitions now declare source/target components, supported events, and the
  authoritative action. `activated`, `opened`, `closed`, and `triggered` events
  are validated against endpoint components; interactable events also require
  their matching verbs. Server-accepted player and AI movement fires a
  `triggered` link when entering a `core.trigger` radius. `toggle_door`,
  `open_door`, and `close_door` dispatch only to unlocked doors and respect the
  authored enabled flag and cooldown. Link-driven door changes do not
  recursively emit another door event. Both CLI templates scaffold a
  lever-to-door definition and connected zone example. `help_requested` remains
  deferred: group aggro already shares same-tick hostile targets, while an
  explicit help link has no defined receiver goal state or persistence/expiry
  contract. Define those semantics before adding a second assistance path.
  Cross-zone links need explicit route authorization and durable multi-zone
  coordination.

- **SIM-014 — Live faction relation refresh:** ADR 0009 makes faction relations
  database-authoritative. Directional relations are now part of the validated
  `FactionSnapshot`, normalized PostgreSQL reads/writes, and the shared AI
  catalog loaded at authored-map startup. Hostile target selection uses those
  persisted relations across all zones. Authored-map mode now polls the
  normalized world state every five seconds and submits changed catalogs to the
  authority loop, which replaces every zone's AI policy in one serialized
  operation. Relations stay out of zone content. Live PostgreSQL verification
  of propagation after a committed relation edit remains open because this
  environment has no dedicated database configured for the acceptance run.

- **SIM-014 — Live PostgreSQL recovery acceptance (2026-11-12):** A dedicated
  local PostgreSQL 17 cluster on port 55439 was migrated through 0018; status
  reported 18 applied and 0 pending. The snapshot suite passed 10/10, including
  replay after restart and lease takeover/fencing. The durable resume suite
  passed 6/6, including process-restart resume and single-use token rotation.
  The normalized faction/calendar round-trip also passed. Remaining acceptance:
  run the authored multi-zone service and interrupt a zone owner around the
  durable handoff commit; verify source/destination snapshots, the durable
  session route and receipt, readiness, and survivor-zone continuity. Existing
  PostgreSQL suites do not cover that integrated crash window yet.

- **CLT-008 — Equipment action contract:** The client inventory can submit
  split/merge operations, but the authoritative `equip` verb is explicitly
  unsupported. `ItemStack` has no equipment-slot metadata, and entities have no
  persisted equipped-state component or owner-only replication shape. Define
  item slot/compatibility rules and the snapshot/event/replication contract,
  implement the server transaction, then expose it in the inventory UI.

- **PST-010 — Live Nakama contract validation:** Authenticated player routes,
  `CharacterApiClient`, and the account UI cover friend request and block
  lifecycle plus group discovery, create, join/request, list, leave, roster
  viewing, join-request decisions, member changes, and role changes. Authenticated
  chat destination discovery now drives direct/group realtime tabs and routing.
  This checkout has no Nakama instance or two pre-created test accounts. Run
  the Nakama HTTP/realtime contract scenario with two accounts and migrated
  PostgreSQL before closing PST-010.

- **PST-013 — PostgreSQL content-migration rehearsal:** Reviewed persisted-
  instance plans now prepare and validate a whole-world candidate, preserve
  quarantined payloads in a rollback checkpoint, and commit through the snapshot
  transaction. The local environment has no configured PostgreSQL database, so
  the lease-fenced save, restart recovery, and rollback path cannot be exercised
  against PostgreSQL here. Run dry-run, apply, restart, and pre-gameplay rollback
  with an isolated database migrated to head, then remove this blocker.

- **SIM-008 — Party chat and zone binding validation:** The client now discovers
  account-authorized direct and group destinations through the world API and
  joins those Nakama channels on demand. Party channels are absent from the
  `ChatChannelTarget` contract, and configured room access is not yet bound to
  Lynx zone admission. Resolve party channel authorization and zone-admission
  policy, then validate direct/group joins and history against a live Nakama
  instance before closing SIM-008.

- **SIM-009 — Party reconnect contract:** The current `RealtimePartySession`
  explicitly models Nakama parties as transient and ends membership when its
  socket closes, while SIM-009 requires party state to survive reconnect and be
  cleaned up on logout/expiry. The repository has no chosen lifecycle contract
  for leader disconnect, last-member disconnect, or reconnect after expiry.
  Resolve whether parties are durable across reconnect or session-bound with a
  documented rejoin window, then align the game API, Nakama adapter, and
  acceptance criteria before claiming SIM-009 complete.

- **SIM-010 — Trade and economy integrity:** The task depends on the unresolved
  SIM-009 party/social lifecycle contract, durable inventory transactions
  (`PST-002`), and live PostgreSQL economy-ledger validation (`PST-014`). The
  last dependency requires an isolated migrated database that is not configured
  in this environment. Resume two-phase trade work after SIM-009 defines the
  participant/session contract and the persistence/ledger dependencies are
  available.

- **OPS-005 — Hosted tagged release verification:** Linux/Windows release jobs
  now package version-checked archives, checksums, build stamps, and
  target-specific SBOMs; this checkout has no existing release tag, and running
  the workflow publishes public release assets. Exercise the workflow on the
  next approved version tag, verify each archive and detached checksum on a
  clean host, and retain the run link before closing OPS-005. Client packages
  and macOS are separate follow-up scope after their declared platform work.

- **OPS-002 — Dashboard and alert validation:** Grafana provisioning, a versioned
  dashboard, Prometheus alert rules, runbook links, and proposed SLOs are checked
  in. CI now installs Ubuntu's Prometheus package and runs `promtool check rules`;
  a checksum-verified Prometheus 3.16.0 Windows release also accepted all seven
  rules locally. This checkout has no staging Prometheus/Grafana deployment, and
  Lynx does not emit backup-job metrics. Validate dashboard provisioning with
  the deployment's Grafana version, exercise every threshold during staging
  including restart transitions, tune the proposed SLOs against measured
  traffic, and connect the backup alert to the deployment's backup exporter
  before closing OPS-002.

- **NET-008 — Production admin identity configuration:** ADR 0029 selects an
  external OIDC issuer and requires Lynx-side role authorization, MFA, short
  sessions, and revocation. The repository has no production issuer, deployed
  role-claims mapping, MFA policy, or callback/origin configuration, and the
  admin listener currently exposes only health/readiness/metrics. Adding
  privileged routes without a verified identity source would violate the ADR.
  Configure the deployment issuer, claims mapping, MFA policy, and allowed
  callback/origins; then implement the verifier, role checks, audit path, and
  operator review surface.

- **QUA-002 — Hosted replay and ledger-property confirmation:** Core and
  simulation property suites pass locally (105 `engine_core` tests and 83
  `engine_sim` unit tests), including generated persistent-zone snapshot JSON
  and restore round trips. The 48-tick combat/AI/movement replay is pinned to a
  SHA-256 snapshot golden and passes on Windows; this checkout cannot execute
  Linux or macOS binaries, so confirm the same golden in hosted Ubuntu/macOS
  jobs. A 12-case generated PostgreSQL inventory value-ledger test checks
  quantity conservation, signed entries, reconciliation, and idempotent
  replay; it compiles but skips locally because no dedicated test database is
  configured. Run it in migrated PostgreSQL CI before closing QUA-002.
- **QUA-001 — Remaining invariant-matrix evidence:** The real gateway suite
  covers currently available movement and combat authority cases, including
  queue/budget boundaries, dead/stunned processing, speed, geometry, cooldown,
  faction policy, controller changes, reconnect, and deterministic replay. The
  historical failing-before evidence cannot be reconstructed after these fixes,
  and zone-transfer coverage depends on `SIM-014` runtime transfer support,
  which is not available yet. Keep QUA-001 open; add transfer coverage when
  that seam lands and retain failing-before evidence for future regressions.

- **NET-007 — Staging alert exercise:** The Prometheus expressions are now
  documented, but this checkout has no staging Prometheus endpoint, alert
  manager, or staging traffic access. Exercise and tune the examples from
  `docs/operations.md` in staging before marking the final acceptance criterion
  complete.
- **NET-010 — Provider-specific secret-store integration and rotation drill:**
  The Rust service and PowerShell/Bash setup scripts now accept mounted secret
  files for PostgreSQL credentials, demo token hashes, and Nakama credentials
  through `<NAME>_FILE`, alongside environment injection. The repository does
  not select a cloud secret-store provider or target
  deployment, and this environment has no managed secret-store credentials or
  production rotation window. Select the deployment provider, provision its
  isolated test environment, and exercise a staged credential rotation before
  removing this blocker.
- **NET-011 — Anomaly review and appeal path:** Simulation now retains bounded,
  server-only evidence for rejected forged actions and movement sequences, but
  there is no staff review surface or moderation audit trail yet. NET-008 owns
  the role-checked inspection/moderation interface; complete that path before
  NET-011 can offer manual review. Define time-based evidence retention and an
  appeal workflow as part of that work, then add configurable risk thresholds
  without introducing automatic punitive actions.
- **CLT-014 — Nakama account-flow validation:** The local protocol fixtures and
  in-process tests cover the client and world gateway, but this environment has
  no configured Nakama service or test account. Run the account login and
  character-flow scenario against a configured Nakama test instance before
  closing the remaining integration validation.
- **CLT-002 — Live multi-server session lifecycle:** Saved credentials are now
  isolated by world-profile key and Nakama origin in the OS credential store.
  This checkout has no configured Nakama service with multiple world endpoints
  and accounts, so it cannot validate reconnecting between servers, concurrent
  client instances under each server session policy, or a full logout/resume
  lifecycle against live services. Resume with a provisioned Nakama test
  deployment and at least two world profiles/accounts.
- **CLT-003 — Live onboarding and restart recovery:** The Bevy account screen,
  character ownership routes, deterministic actor provisioning, and durable
  snapshot-before-admission path are implemented. This environment has no
  configured Nakama/PostgreSQL staging services or two test accounts, so it
  cannot verify account creation, character entry, two-instance session policy,
  and actor reuse after a real server restart. Run the documented account flow
  and restart scenario on a provisioned integration deployment before closing
  those criteria.
- **PST-010 — Nakama live contract:** The adapter and client HTTP path compile,
  but the live contract test needs a configured Nakama endpoint, two test
  device accounts, and PostgreSQL test settings; the application event/session
  bridge also needs to be exercised with that service. This environment has no
  configured Nakama instance or test accounts. Resume with
  `LYNX_REQUIRE_NAKAMA_TEST=1` and the documented test variables after the test
  environment is provisioned.
- **QUA-005 — Deployed bottleneck and soak validation:** `lynx-load` now shares
  the service metrics registry with WebSocket transport and reports queue
  occupancy/capacity/full events, replication enqueue bytes, snapshot/delta
  socket writes, and per-run tick duration. One-minute eight- and 16-client
  movement runs each accepted 100 ops/s without tick failures, overruns, queue
  rejections, or full queues; mean tick work was 0.4101/0.5447 ms and RSS grew
  11.8/17.8 MiB in the combined server/client process. The reports are in
  `docs/benchmarks/`. A 32-client attempt exceeds the fixed 16-connection
  per-IP transport cap; a 16-client 500 ops/s attempt also ended after the
  movement workload exceeded the server's authenticated rate budget. The
  checkout has no staging service/database topology or agreed latency and
  memory budgets, and the local samples do not identify a target-scale
  bottleneck. Resume with a provisioned reference topology, database latency
  injection, and sustained repeated scenarios, then review bottlenecks and set
  budgets from those measurements.
- **QUA-007 — Hosted coverage artifact and trend review:** The nightly coverage
  workflow and isolated PostgreSQL setup are present in this checkout, but
  `gh run list --workflow coverage.yml` returned HTTP 404 because the workflow
  is not present on the GitHub default branch. This environment therefore has
  no hosted workspace report to inspect; the local workspace coverage attempt
  is also unsafe because `.env` points at the application database. Publish the
  workflow on the default branch, run it with the isolated CI database, then
  review the uploaded artifact and record a workspace baseline/critical-path
  gaps.
- **CNT-013 — Asset pipeline and import policy:** The task depends on `CNT-005`,
  whose definition-editor acceptance remains blocked by `CNT-001`; the source
  task still has no no-hand-edit authored-item workflow, reference picker, or
  diff-assisted version bump. Starting the import UI now would leave its asset
  references disconnected from the definition editor. Resume after `CNT-005`
  provides the authored asset-reference seam, then implement validation,
  hashing, budgets, previews, and publish checks under ADR 0023.
- **CNT-015 — Localization tooling:** The task depends on `CLT-010`, which is
  blocked by `CLT-008` and `CLT-009`; the client has not yet adopted catalog
  keys across its player-facing widgets or established locale selection and
  persistence. Resume after the client localization contract lands, then add
  completeness/placeholder checks, pseudo-locale generation, and translator
  import/export around the same catalogs.
- **CLT-008 — Authoritative equipment interaction:** The inventory UI and
  server-side take/insert/split/merge paths are present, but `Verb::Equip` is
  explicitly unsupported. The current item schema has stack size and weight,
  with no equipment-slot compatibility or stat-effect contract, and owner-only
  replication carries inventory alone. Do not expose an equip button until the
  item-slot/effect model, character equipment state, persistence representation,
  and owner replication contract are defined together. Resume with that
  authoritative simulation/persistence slice, then add the client control.
- **QUA-011 — Actual Rust/Bevy upgrade rehearsal:** The current pins and CI
  generated-project gate are present, and the upgrade and rollback process is
  documented in `docs/upgrade-rehearsal.md`. This checkout has not changed the
  Rust or Bevy version, so it cannot demonstrate the dedicated-upgrade commit,
  validate a migration against an old generated project, or perform the
  graphical smoke checklist. Resume those acceptance checks with the next
  planned version upgrade on a supported desktop environment.
- **NET-013 — Durable security event collection and alert exercise:** The world
  emits structured authentication/resume denials, rate-limit trips, trusted
  session revocations, and correlated authority rejections. Pre-authentication
  rate-limit events now identify a fixed limiter class without recording
  credentials; missing credentials have an explicit stable denial reason.
  Existing sentinel regressions verify private details are excluded.
  `docs/operations.md` defines safe fields, access, and 90-day searchable
  retention. This checkout has no configured log collector, central query
  surface, alert receiver, or staging traffic. Complete field extraction and
  retention in the deployment collector, cover remaining admin and moderation
  actions when those APIs exist, then tune and exercise the documented PromQL
  alerts in staging.
- **QUA-008 — First fuzz execution:** Installed nightly Rust 1.102.0 and
  `cargo-fuzz` 0.13.2; all seven targets compile on this Windows checkout, but
  `envelope_decode` exits before fuzzing with `STATUS_DLL_NOT_FOUND`
  (`0xc0000135`). A fresh `wsl --list --quiet` returned no installed
  distribution. `gh run list --workflow fuzz.yml` also returned HTTP 404
  because the workflow is absent from the GitHub default branch, so there is no
  hosted run to inspect and no corpus run to triage. Run the bounded targets on
  Linux or publish the workflow first; preserve any minimized input as a
  regression.
- **OPS-011 — Clean-host deployment drill:** The reference topology, `systemd`
  unit, WinSW service example, deployment checklist, and Unix SIGTERM shutdown
  handling are in the checkout. This Windows development environment has no
  clean Linux deployment host or configured private PostgreSQL/Nakama services,
  so it cannot verify the install, firewall boundaries, service-account
  permissions, readiness, or restart recovery; only the Windows Rust target is
  installed, so the Unix SIGTERM branch also needs Linux compilation. Exercise
  the checklist on a provisioned Linux staging host; keep production traffic off
  until the external route and durable restart are verified.
- **CLT-007 / CLT-016 / CLT-019 — Graphical movement validation:** The local
  online demo launcher and deterministic headless latency/correction fixtures
  are available, but this session's Windows desktop-control runtime cannot
  connect to its native pipe (`failed to connect native pipe`, OS error 2), so
  the rendered client could not be inspected or operated. No visual tuning or
  graphical convergence evidence is claimed. Resume the WASD movement scenario
  at 100/200 ms and a controlled authoritative correction in a working Windows
  desktop session, capture the rendered result and environment, then update the
  remaining graphical acceptance criteria in `docs/TODO/40-bevy-client.md`.
- **PST-014 — Live PostgreSQL ledger validation:** Migration 0016 and its
  integration assertions now cover opening balances, signed transfer entries,
  and drift detection, including the generated 12-case quantity-conservation,
  signed-entry, reconciliation, and replay property. The environment currently
  lacks the complete `LYNX_PG_DATABASE`, `LYNX_PG_USER`, and
  `LYNX_PG_PASSWORD` configuration, so the PostgreSQL scenario cannot run.
  Configure a dedicated database migrated to head, run with
  `LYNX_REQUIRE_POSTGRES_TEST=1`, and verify the backfill, append-only triggers,
  transfer idempotency, and reconciliation query before closing PST-014 and
  QUA-002's persistent-aggregate criterion.
- **QUA-013 — Accessibility and localization verification:** Client locale
  preference and accessibility controls are not implemented, and `CLT-010`
  remains the declared dependency. The localization catalogs and negotiated
  error rendering exist, but pseudo-locale/layout, text scaling, keyboard and
  screen-reader flows, and persisted accessibility settings cannot be verified
  until the client surfaces land. Resume QUA-013 after `CLT-010`.
