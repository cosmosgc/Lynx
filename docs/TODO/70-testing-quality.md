# Testing, quality, and technical debt backlog

Scope: the test strategy and the engineering hygiene that keeps the guarantees
true — authority/security tests, determinism, protocol fixtures, database
integration, load/soak, fuzzing, coverage, accessibility, documentation drift,
toolchain upgrades, and flaky-test control.

**Current baseline:** unit and integration tests, a zero-unsafe workspace,
fmt/Clippy/test gates, a generated-project CI check, PostgreSQL migration
coverage, and a nightly multi-client load workflow. The Ubuntu Rust job
configures the live PostgreSQL test and requires its configuration; local runs
can skip it when unconfigured. Hosted CI confirmed the PostgreSQL, Markdown,
Ubuntu, and Windows gates on 2026-10-29. The coverage workflow is configured,
but its first report is pending; there is no fuzz target. A Markdown link
checker and its tests run locally and in CI.

**Related design:** [project guide §26 definition of done](../lightweight_mmorpg_engine_project_guide.md#26-definition-of-done),
[security](../security.md), [testing conventions](../../README.md#design-rules).

---

## QUA-001 — Movement and combat integration suite

- **Priority:** P0
- **Status:** In progress — gateway authority, replay, and reconnect coverage expanded; invariant matrix incomplete
- **Depends on:** `SIM-001`, `SIM-003`
- **References:** `SIM-001`, `SIM-003`

**Scope**

- Headless tests that drive movement and attacks through the real gateway and
  simulation, not a copied path.
- Authority cases: forged damage, out-of-range, wall clipping, speed burst,
  dead actor/target, cooldown, wrong controller.
- Reconnect during movement/combat; zone transfer once available.
- Deterministic replay assertions for the same input sequence.

**Acceptance criteria**

- [ ] Every authority invariant in `SIM-001`/`SIM-003` has a failing-before,
      passing-after test.
- [x] Tests exercise the wire/gateway seam and the simulation together.
- [x] Reconnect and death paths are covered.
- [x] The suite runs in hosted CI without a GPU or external service.

**Coverage and remaining gaps:** `crates/world_gateway/tests/gateway.rs` drives
real protocol envelopes through the gateway and zone. It covers forged combat
fields, server-derived damage, cooldown, range and line of sight, death and
controller release, pending movement at death, wall clipping, speed budget,
session-bound inputs, observer agreement, reconnect during movement and combat,
idempotent attack retries, and a repeated mixed-input replay. It also checks
stunned and zero-speed actors at both queue and processing time, occupied cells
and swept-body collision, same-faction denial and different-faction attacks, and
stale/pending sequence rejection without consuming a rejected newer sequence. A
new gateway-level regression (`gateway_enforces_speed_budget_and_dead_actor_combat_authority`)
also checks an over-budget movement endpoint and a dead actor's attack without
state mutation. A further gateway-seam case applies an authored directional
`guard -> bandit` deny rule and proves the reverse pairing remains attackable.
Re-verified locally on 2026-10-30 with
`cargo test -p world_gateway --test gateway` (36 passed).
Contested destinations resolve in stable actor order regardless of request
arrival order, and revoking Move after queuing causes processing-time rejection
and consumes the sequence. The updated gateway test suite passed locally. The
tests require no GPU or service; CI's workspace test command includes them. The
full invariant matrix and historical failing-before evidence remain open. In
particular, integrated coverage still needs broader queue/budget boundaries.
Zone transfer is not yet available. Simulation unit
tests cover some of these separately, but do not substitute for this suite.

**Queue-budget boundary slice (2026-11-07):** Added real-gateway tests with
`max_actions` set to zero and one. A zero-budget request is rejected without
consuming its sequence; with a one-intent budget, a second session is rejected
while the global pending queue is full, then succeeds with the same sequence
after the next tick drains the queue. The broader authority/invariant matrix
remains open.

**Controller-change boundary slice (2026-11-07):** Added a real-gateway combat
regression where Alice's authenticated session remains active after the server
changes her actor's controller to Bob. Alice's attack is denied by simulation
authority and leaves target health unchanged. This covers the wrong-controller
case beyond rejecting client-forged actor/principal fields.

**Movement geometry boundary slice (2026-11-08):** Added a real-gateway request
that changes only the destination height. It is accepted into the queue but
rejected during authoritative processing; the result consumes the accepted
sequence and reports the unchanged server position.

**Configured zone-boundary slice (2026-11-08):** Added a gateway case where the
configured world bound is narrower than the navigation grid. A short same-cell
move is nav-valid but crosses the world bound, so authoritative processing
rejects it without changing position.

**Processing-time liveness slice (2026-11-08):** Added a gateway case that
queues a valid move, then makes the actor dead while leaving the controller and
session unchanged. Processing rejects the intent, consumes its accepted
sequence, and preserves the authoritative position, isolating the liveness
recheck from controller-release behavior.

---

## QUA-002 — Determinism and property-based tests

- **Priority:** P1
- **Status:** In progress — inventory, treasury, territory, and calendar replay invariants have shrinking properties; seeded simulation replay now has a fixed SHA-256 snapshot golden, with hosted cross-platform confirmation pending
- **Depends on:** —
- **References:** [architecture §Simulation model](../architecture.md#simulation-model),
  `SIM-018`

**Scope**

- Property tests for inventory conservation, faction treasury/territory
  invariants, calendar lifecycle, and snapshot round trips.
- Seeded multi-tick replays with identical inputs producing identical snapshots.
- Shrinking failing cases into minimal reproductions.
- Guard against unordered iteration leaking into simulation-visible state.

**Acceptance criteria**

- [x] Core invariants have property tests with generators for edge cases.
- [ ] Replay tests are deterministic across runs and machines.
- [x] A failure produces a minimal, reproducible seed/case.
- [ ] New persistent aggregates add invariants to the suite.

**Progress:** `engine_core::inventory` has `proptest` properties over random
inventories and transfer attempts. They assert that a successful transfer
conserves every item definition across both inventories, a failed transfer
leaves both inventories byte-for-byte unchanged, and a replayed key returns the
original receipt without touching either inventory. Generated inventories mix
stackable and unique items and round-trip through JSON with validation, matching
the snapshot restore path. Proptest shrinks any failing case to a minimal
inventory/transfer pair and prints the seed. `faction::treasury` generates
deposit/spend sequences and checks balance conservation, idempotent replay,
ledger recovery, and JSON round trips. `faction::territory` generates capture
windows and power thresholds, asserting eligible ownership transitions and
atomic rejection otherwise; the earlier deterministic territory sequence loop
remains. `engine_sim::calendar` generates enrollment identities and replay
counts, verifying an enrollment is applied once and the result remains stable.
The zone snapshot suite also generates persistent-entity counts, health bounds,
and tick counts, then verifies JSON decoding and restore reproduce the exact
snapshot. This closes the previously example-only snapshot round-trip gap. The
48-tick combat/AI/movement replay now pins the serialized final snapshot to
SHA-256 `a55947cc6f2ae87aca8d124bb14d1d7e36606a6cea07cbe1a39138621e5c322e`.
This test runs in the workspace CI matrix on Ubuntu, Windows, and macOS; the
current Windows run passes. Confirm the same golden on hosted Ubuntu and macOS
before checking the cross-machine criterion. The new player inventory value
ledger now has a 12-case generated PostgreSQL test for source/destination
conservation, signed event-entry sums, reconciliation, and idempotent replay.
Its test target compiles and Clippy passes, but local execution skips because
no dedicated PostgreSQL test database is configured. Run it in the migrated
PostgreSQL CI environment before checking the new-persistent-aggregate
criterion.

---

## QUA-003 — Protocol golden and compatibility tests

- **Priority:** P1
- **Status:** Done
- **Completed:** 2026-11-12
- **Depends on:** —
- **References:** `NET-004`, `engine_net::protocol`
- **Completion record:** [QUA-003](completed/qua-003-protocol-golden-compatibility.md)

**Scope**

- Checked-in fixtures for every client/server wire kind, including errors and
  handshake negotiation.
- Major/minor/feature compatibility matrix tests.
- Unknown-field behavior tests (reject vs. ignore by contract).
- A diff gate that requires a protocol version bump/changelog for breaking
  changes.

**Acceptance criteria**

- [x] Every wire kind has at least one golden fixture.
- [x] Accidental field/format changes fail CI.
- [x] Compatibility matrix outcomes match the documented policy.
- [x] Fixtures are versioned with the protocol.

**Progress:** `crates/engine_net/tests/protocol_golden.rs` pins hello, version
mismatch error, and negotiated handshake wire forms and exercises the major,
minor, content, and feature compatibility outcomes. It also records the serde
contract that unknown envelope and handshake fields are ignored while unknown
feature names are rejected. **2026-11-10:** `crates/world_server/tests/`
now pins protocol-versioned `world.ready`, `world.snapshot`, `world.delta`,
`action.result`, `movement.queued`, `chat.message`, `chat.send.result`,
`chat.channels.result`, `chat.presence.result`, `chat.history.result`, and
`ui.state` envelopes. The gateway calendar integration also pins
`calendar.result` and `calendar.state`; client request fixtures pin action,
movement, chat, calendar, acknowledgement, and UI command shapes. World
replication fixtures exercise the typed server DTO; request / reply fixtures are
emitted by real gateway flows. Every fixture is encoding-checked and decoded back
into the originating envelope. The `world.token` golden now contains only a
fixed non-authenticating sentinel. The `protocol-compatibility` pull-request job
now
requires `PROTOCOL_VERSION` to increase and `CHANGELOG.md` to change whenever
client request/decoding, `engine_net` envelope/handshake/replication, gateway
replication, or world-server runtime contract sources change. This is a
conservative source-path gate: reviewers still determine whether the version
increment follows the documented compatibility policy.

The `world.token` fixture uses the unmistakably fake
`FAKE_LYNX_TEST_TOKEN_DO_NOT_AUTHENTICATE` sentinel. The test requires exact
sentinel equality and confirms envelope debug output redacts it.

---

## QUA-004 — PostgreSQL integration matrix

- **Priority:** P1
- **Status:** Done
- **Completion record:** [qua-004-postgresql-integration-matrix.md](completed/qua-004-postgresql-integration-matrix.md)
- **Depends on:** `PST-001`
- **References:** `PST-001`, `PST-008`, `OPS-006`

**Why:** The Ubuntu CI job now runs the live snapshot round-trip against
PostgreSQL 17. Persistence work will add more live tests; they need a reliable,
non-flaky matrix.

**Scope**

- Run persistence tests against PostgreSQL 17 in CI with `LYNX_PG_*` set.
- Cover migrations from empty, idempotent re-run, snapshot save/load/delete,
  and account/character CRUD as those land.
- Test failure paths: checksum mismatch, missing schema, corrupt payload,
  serialization failure, and retry.
- Keep local runs skippable when unconfigured; never silently pass a broken
  configured database.
- Isolate test data per run and clean up deterministically.

**Acceptance criteria**

- [x] Live persistence tests run in CI, not only locally.
- [x] A configured-but-unreachable database fails loudly.
- [x] Tests are isolated and parallel-safe or explicitly serialized.
- [x] Migration/schema failures produce actionable diagnostics.
- [x] No test depends on Docker for local execution.

**Progress:** `crates/lynx_world/tests/postgres_snapshot.rs` includes a
database-free regression that connects a snapshot store to a released loopback
port and asserts an internal error without echoing the password; a configured
but unreachable database therefore fails instead of skipping. Every writing test
uses per-run world/zone IDs incorporating a process ID and atomic sequence. A
scoped cleanup guard transactionally removes snapshots, events, leases, zone
snapshots, and quarantine rows on normal completion and during unwinding. A live
PostgreSQL regression catches a simulated assertion panic and verifies its
snapshot, event, and lease rows were removed. The full live test binary passed
with `LYNX_REQUIRE_POSTGRES_TEST=1` and its default parallel test execution.

---

## QUA-005 — Load and soak benchmarks

- **Priority:** P2
- **Status:** In progress — local multi-client workloads report operation/window latency, process RSS, envelope and replication bytes, queue occupancy/full events, socket writes, and per-run authority tick duration/overruns; nightly CI stores scenario/sample CSVs; DB fault injection, sustained soak review, bottleneck attribution, and regression budgets remain
- **Depends on:** —
- **References:** `OPS-007`, `SIM-018`, `NET-006`

**Scope**

- A harness that simulates many sessions/entities and records tick latency,
  throughput, memory, and network bytes.
- Short benchmark runs in nightly/manual CI and longer soak runs before release.
- Scenarios: idle crowd, movement storm, chat burst, action spam, reconnect
  storm, and database latency injection.
- Baseline results stored with hardware/topology.

**Acceptance criteria**

- [x] A repeatable local load run is documented with machine topology and raw CSV output.
- [ ] Results identify the first bottleneck (tick, replication, DB, transport).
- [ ] Soak runs detect leaks and latency drift.
- [ ] Regressions beyond the agreed budget are visible.

**Windowed resource and replication sampling (2026-11-12):** `lynx-load` now samples the load
harness process's resident memory and reply p95 every five seconds by default,
keeps per-window sample counts, reports the first/last non-empty p95 windows,
and includes tick execution time/overruns in its summary. `--samples-csv` writes
the per-window series, and the nightly/manual workflow uploads a separate
series file for each scenario. The process contains both the in-process server
and clients, so memory is a combined harness measurement. The harness now shares
the world's metrics registry with its WebSocket transport and reports queue
high-water/capacity, queue-full events, replication enqueue bytes, and snapshot
and delta socket-write counts/bytes. A five-second Windows release movement
smoke run and its sample rows are recorded in
[`load-soak-smoke-2026-11-12.md`](../benchmarks/load-soak-smoke-2026-11-12.md).
It observed 50 successful delta writes (23,660 bytes), a per-session queue
high-water of 3/64 frames, zero full-queue events, and 0.1367 ms mean tick
work. This proves the metric path works under a light in-process workload; it
does not identify a capacity bottleneck or substitute for a sustained soak,
database latency injection, or a reviewed regression budget.

**60-second local movement samples (2026-11-12):** Eight clients offered and
accepted 5,999 operations at 100 ops/s, with zero queue rejections, tick
failures, overruns, or queue-full events; mean tick work was 0.4101 ms. A
16-client run accepted 6,000 operations at the same rate, with zero tick/queue
pressure and 0.5447 ms mean tick work. Overall reply p95 was 31.29 ms and first
and last five-second windows were 31.00 ms and 31.37 ms. Combined server/client
RSS grew 17.8 MiB without a visible plateau. The reports are
[`load-soak-60s-2026-11-12.md`](../benchmarks/load-soak-60s-2026-11-12.md) and
[`load-soak-16-client-2026-11-12.md`](../benchmarks/load-soak-16-client-2026-11-12.md).
The harness now raises only its bounded handshake/churn buckets to let its
configured localhost client burst start, and caps clients at the transport's
16-per-IP limit. Database fault injection, target-scale sustained soak,
bottleneck attribution, and regression budgets remain open.

---

## QUA-006 — Security abuse test suite

- **Priority:** P1
- **Status:** Done — transport abuse matrix, side-effect checks, and response/log redaction coverage are recorded
- **Completion record:** [QUA-006](completed/qua-006-security-abuse-test-suite.md)
- **Depends on:** —
- **References:** [security](../security.md), `NET-002`, `NET-007`, `NET-011`

**Scope**

- Forged identity/role/actor fields, oversized/deep payloads, malformed frames,
  replay, token guessing, and duplicate sessions.
- Rate-limit bursts, slow clients, connection floods, and origin violations.
- Content/path traversal for tools and MCP once they exist.
- Assert stable error codes and no secret leakage in responses/logs.

**Acceptance criteria**

- [x] Each documented abuse case has a test or an explicit gap task.
- [x] Tests assert both rejection and the absence of side effects.
- [x] Secret-redaction assertions cover error/log paths.
- [x] The suite runs in CI and is extended with each security task.

**Completion:** The current scope is covered across the WebSocket, gateway,
service, transport, and read-only MCP suites. Cases assert stable rejection,
no applicable admission/action side effects, and response or structured-log
redaction. See the [completion record](completed/qua-006-security-abuse-test-suite.md)
for the covered abuse surfaces and verification commands.

**Malformed-frame slice (2026-11-08):** Added an authenticated WebSocket case
that sends a truncated binary frame before `session.hello`. The server returns a
stable `invalid_request`, closes the socket, does not attach a gateway session or
apply an action, and does not echo the bearer token in its error response.

**Identity-rejection slice (2026-11-08):** Production-admission coverage now
asserts that invalid credentials and foreign/missing character selections leave
session-attach and applied-action metrics at zero before the valid control case.
Repeated invalid credentials also exhaust the configured per-IP failed-auth
bucket, return `rate_limited`, and never echo the guessed token.

**Duplicate-session slice (2026-11-08):** A second WebSocket using an active
credential receives `conflict` without increasing session attachments; the
original connection remains usable after the rejected attempt.

**Resume-guessing slice (2026-11-10):** Syntactically valid resume-token
attempts now use an independent peer-IP token bucket. A real WebSocket test
verifies that an invalid token receives the stable expiry response once and a
second attempt is rate-limited before upgrade, without echoing the candidate
secret.

**Connection-flood and audit-redaction slice (2026-11-12):** The WebSocket
integration suite fills all 16 per-IP connection slots without sending
`session.hello`, then verifies the next upgrade is rejected promptly with
`ip_capacity`, no bearer echo, and no authority event. A tracing-capture
regression exercises failed-credential, character-admission, resume-denial,
and session-rate-limit audit emitters with secret sentinels in credential or
backend error inputs; stable reason and classification fields remain while the
credential and private backend details are absent. Read-only MCP path
confinement (including `../` and absolute paths) is covered in
`tools/project_cli/src/mcp_main.rs`; protocol forgery, replay, origin,
rate-limit, malformed-frame, and duplicate-session cases are covered by the
gateway, service, and WebSocket suites. Focused flood/redaction checks and the
complete WebSocket integration suite passed locally; CI runs the workspace
tests.

---

## QUA-007 — Coverage reporting

- **Priority:** P2
- **Status:** In progress — nightly-filtered workspace coverage is configured; hosted report visibility and trend review are pending
- **Depends on:** —
- **References:** `QUA-001`, `QUA-006`

**Scope**

- Generate line/region coverage with `cargo-llvm-cov` or equivalent.
- Publish a trend and per-crate report; avoid vanity thresholds.
- Identify untested critical paths: authority, persistence, replay, recovery.
- Add targeted tests for risky gaps rather than blanket percentage goals.

Coverage review focuses on invariants, not a workspace percentage: `engine_sim`
authority must exercise authorization, validation, and atomic failure paths;
`world_gateway` must exercise owner isolation and stale-session rejection;
`lynx_world` persistence/replay must exercise recovery and failure handling.
Coverage for generated code, fixtures, and test-only modules should not be used
to claim production-path coverage.

**Acceptance criteria**

- [ ] Coverage is generated in CI/nightly and visible to contributors.
- [x] Critical modules have explicit invariant-based coverage expectations.
- [ ] New code does not reduce critical-path coverage without justification.
- [x] Reports exclude generated/test-only code appropriately.

`cargo-llvm-cov` excludes source under `tests/`, `tests.rs`/`*_tests.rs`,
examples, benches, and its target directory from the default source report.
There is no generated Rust source in the current workspace file inventory.
Inline `#[cfg(test)]` modules are now annotated with
`#[cfg_attr(coverage_nightly, coverage(off))]`; a workspace source scan found
97 annotated modules and no unannotated inline test modules. The coverage
workflow uses nightly Rust for this attribute, while stable builds leave it
inactive. The local filtered `engine_net` report generated successfully at
`target/llvm-cov/engine-net-nightly/html/index.html` after all 38 unit tests
passed; it reports 48.33% line and 27.46% region coverage over production
source. These scoped numbers are not a workspace baseline.

The [coverage workflow](../../.github/workflows/coverage.yml) is configured to
run daily and on
manual dispatch, configures the required isolated PostgreSQL integration
database, and uploads the HTML source report as a 90-day artifact. The full
local workspace run reached the PostgreSQL integration suite but failed because
the repository `.env` database is missing migration `0016`; it was not migrated
because it is configured as the application database, not a dedicated coverage
scratch database. `gh run list --workflow coverage.yml` returned HTTP 404 because
the workflow is not on the GitHub default branch, so hosted artifact visibility,
a reviewed workspace baseline, and critical-path regression history remain
pending. See the local blocker in `docs/agents/issue-tracker.md`.

---

## QUA-008 — Fuzzing

- **Priority:** P2
- **Status:** In progress — seven bounded parser targets build with nightly on Windows and the nightly corpus-cached workflow is defined; local execution is blocked by a missing runtime DLL, while hosted execution and crash triage remain.
- **Depends on:** —
- **References:** `engine_net` envelope/framing, `engine_content`

**Scope**

- Fuzz envelope/frame decoding, content parsing, snapshot decoding, and command
  payloads.
- Maintain a corpus of minimized cases; run bounded fuzzing in nightly CI.
- Assert no panics, unbounded allocation, or secret leakage.
- Triage crashes into regression tests.

**Acceptance criteria**

- [x] All fuzz targets compile locally.
- [ ] Run locally and nightly, then triage the first crash or hosted result.
- [ ] A crash produces a reproducible input and a regression test.
- [x] The scheduled runner enforces per-input time, total run, input size, and RSS limits.
- [x] The nightly workflow caches the evolving corpus.

**Local execution attempt (2026-11-12):** Installed nightly Rust 1.102.0 and
`cargo-fuzz` 0.13.2; `cargo +nightly fuzz build --fuzz-dir fuzz` compiled all
seven targets. Running `envelope_decode` exited immediately with Windows
`STATUS_DLL_NOT_FOUND` (`0xc0000135`), before the fuzzer started. `wsl --list
--quiet` returned no installed distribution, and the repository's fuzz workflow
does not exist on the current GitHub default branch yet, so no hosted run could
be inspected. Resume execution on Linux or after the workflow is published.

---

## QUA-009 — Documentation and code consistency sweep

- **Priority:** P1
- **Status:** Done — documentation sweep and hosted Markdown/CI verification completed; see [completion record](completed/qua-009-docs-consistency.md)
- **Completion record:** [qua-009-docs-consistency.md](completed/qua-009-docs-consistency.md)
- **Depends on:** —
- **References:** [architecture §Persistence model](../architecture.md#persistence-model),
  [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)

**Why:** Documentation drift leads to wrong decisions. Architecture and issue
records have been corrected for the PostgreSQL snapshot store and current
recovery gaps. CI derives the migration head, runs the live test, and runs the
Markdown checker and its tests; the hosted run was confirmed green on
2026-10-29.
Run `python scripts/check_markdown_links.py` locally and
`python scripts/test_check_markdown_links.py` for its tests; both use only the
Python standard library. The checker covers inline relative links and images,
ATX heading anchors (including duplicates), and explicit HTML anchor IDs. It
ignores code blocks and external URLs. It is not a full Markdown parser:
reference-style links, setext headings, nested parentheses in destinations,
and non-Markdown target anchors are not checked. It scans Markdown files under
the repository except `.git`, `.lynx`, `target`, and `__pycache__`.

**Scope**

- Update architecture/persistence wording for `PostgresSnapshotStore`,
  `--state-postgres`, and what remains file-only.
- Fix the CI migration count to derive the head version; add a live PostgreSQL
  test job (see `OPS-006`).
- Reconcile README progress, test counts, ADR counts, and "no client" statements.
- Add a link checker for relative Markdown links and anchors.
- Add a docs review step to the release checklist (`OPS-014`).
- Record the remaining ADR topics in `QUA-017`.

**Acceptance criteria**

- [x] No doc claims the SQL adapter is future work.
- [x] README counts/links and phase statements match the repository.
- [x] CI migration assertions derive from `server/migrations`.
- [x] Broken relative links/anchors fail CI for the checker's supported syntax.
- [x] The gap register in the TODO index is current.

---

## QUA-010 — Error taxonomy and diagnostics audit

- **Priority:** P2
- **Status:** Done — public error codes, retryability, localized keys, HTTP mappings, and redaction are cataloged and regression-tested
- **Completion record:** [qua-010-error-taxonomy-diagnostics-audit.md](completed/qua-010-error-taxonomy-diagnostics-audit.md)
- **Depends on:** —
- **References:** [architecture §Failure and recovery](../architecture.md#failure-and-recovery),
  `engine_core::error`

**Scope**

- Audit every public error path for a stable `ErrorCode`, actionable message,
  and safe details.
- Remove free-form/stringly errors at boundaries; keep internal context.
- Ensure retryability is explicit and correct.
- Verify localization keys exist for player-facing errors.
- Add diagnostics for impossible/quarantined state rather than silent resets.

**Acceptance criteria**

- [x] Boundary errors have stable codes and documented retryability.
- [x] Player-facing interaction errors resolve all stable codes from the negotiated locale catalog with `pt-BR` fallback.
- [x] Secret/PII redaction is tested at the error-payload serialization boundary.
- [x] A diagnostics catalog exists for operators.

The [error and operator diagnostic catalog](../error-catalog.md) documents all
`ErrorCode` defaults and the public HTTP/WebSocket boundary codes. The protocol
contract names `error.<code>` keys, and the interaction client resolves them
from the `content/localization/` catalogs using the requested handshake locale.
It falls back to `pt-BR` when a requested translation is unavailable.

**Completion (2026-11-12):** Protocol 1.1 carries an explicit per-error
`retryable` value. `ErrorPayload` forwards only code-specific display
parameters, capped at eight entries and 128 bytes each, and filters control
characters instead of cloning internal details. Versioned 1.1 wire fixtures
retain the 1.0 contract. `ErrorCode::ALL` drives a regression that checks every
stable code's documented retryability row and `en`/`pt-BR` localization key. A
character-API mapping test checks every `ApiError` variant against its public
HTTP status/code and verifies backend text is redacted. The transport and
serialization boundary suites cover the remaining public error paths.

**Redaction regression (2026-11-12):** `engine_net` serializes an
`OutOfRange` error carrying a synthetic credential, private actor ID, oversized
value, and control characters; the regression asserts only the approved safe
range parameters reach the wire. The focused test and full `engine_net` library
suite pass.

---

## QUA-011 — Toolchain and Bevy upgrade rehearsal

- **Priority:** P2
- **Status:** In progress — upgrade policy, rehearsal checklist, and scheduled dependency updates are documented; a real version upgrade and visual smoke run remain.
- **Depends on:** `CLT-001`
- **References:** ADR 0001, `OPS-010`

**Scope**

- A repeatable process for Rust/Bevy/dependency upgrades in a dedicated change.
- Visual smoke checklist for Bevy upgrades; headless tests for engine crates.
- Compatibility notes and rollback for generated projects.
- Automated dependency update PRs with the full gate run.
- Verify the Bevy pin actually exists once the client lands.

**Acceptance criteria**

- [ ] A Bevy upgrade follows the documented dedicated-commit process.
- [x] Engine crates build without Bevy; focused headless checks passed for `engine_core`, `engine_sim`, `engine_content`, and `game_api`.
- [ ] An actual generated project is migrated and validated during a version upgrade.
- [x] Rollback to the previous toolchain is documented.

---

## QUA-012 — Benchmark regression gates

- **Priority:** P3
- **Status:** Blocked — `QUA-005`
- **Depends on:** `QUA-005`
- **References:** `SIM-018`, `CLT-013`

**Scope**

- Track tick, gateway, persistence, and client frame benchmarks over time.
- Define noise-aware thresholds and a review path for accepted regressions.
- Run on stable reference hardware or a documented approximation.
- Block only clear, repeatable regressions; avoid flaky performance gates.

**Acceptance criteria**

- [ ] Benchmarks run on a schedule and store history.
- [ ] A repeatable regression fails or requires explicit acceptance.
- [ ] Results include environment metadata.
- [ ] Thresholds are reviewed when hardware changes.

---

## QUA-013 — Accessibility and localization verification

- **Priority:** P2
- **Status:** Blocked — `CLT-010`
- **Depends on:** `CLT-010`
- **References:** [internationalization](../internationalization.md), `CNT-015`

**Scope**

- Automated checks for hardcoded player-facing strings and missing keys.
- Pseudo-locale layout tests and text-scaling checks.
- Keyboard-only and screen-reader-friendly flows where the client supports it.
- Colorblind-safe status indicators and reduced-motion options.
- Localized error/calendar formatting tests.

**Acceptance criteria**

- [ ] Hardcoded strings and missing keys fail a release check.
- [ ] Pseudo-locale and text scaling do not break core flows.
- [ ] Keyboard-only navigation covers login, character select, movement, chat,
      inventory, and panels.
- [ ] Accessibility settings persist.

---

## QUA-015 — Mutation testing

- **Priority:** P3
- **Status:** Proposed
- **Depends on:** `QUA-007`
- **References:** `QUA-001`, `QUA-006`

**Scope**

- Run mutation testing on critical modules (authority checks, persistence
  invariants, protocol validation).
- Triage surviving mutants into missing tests or equivalent behavior.
- Keep runtime bounded; do not block every PR.

**Acceptance criteria**

- [ ] A documented command runs mutation tests on selected modules.
- [ ] Surviving mutants are triaged, not ignored.
- [ ] Critical authority checks have high mutation coverage.
- [ ] The process runs nightly or on demand.

---

## QUA-016 — Flaky-test and CI-duration control

- **Priority:** P2
- **Status:** Done — hosted job-duration gates, a manual no-retry flake policy, and an intentional-wait audit are in place
- **Completion record:** [QUA-016](completed/qua-016-flaky-test-and-ci-duration-control.md)
- **Depends on:** —
- **References:** `OPS-006`

**Scope**

- Detect and quarantine flaky tests with an owner and expiry.
- Measure CI duration per job and fail on unexplained growth.
- Stabilize timing-sensitive tests with deterministic clocks/injection.
- Avoid retries as a permanent fix; require a root cause.

**Acceptance criteria**

- [x] Flaky tests are tracked, not silently retried.
- [x] CI duration is visible and bounded.
- [x] Timing-sensitive tests use injected time or documented tolerances.
- [x] Quarantined tests have an owner and expiry.

**Tracking policy:** A flaky test is recorded here with its exact test name,
platform/workflow, first and latest observed failing run, issue/owner, and an
expiry date no more than 30 days out. Quarantine requires an explicit skip or
filter and a link to the tracking entry; CI must not silently retry it. The
owner removes the quarantine after fixing the cause, or renews it with a new
reason and expiry. No flaky tests or quarantines are currently listed. This is
a manual policy; job summaries expose failed runs and the test output remains
available for maintainers to record exact failing test names and run links.
CI does not automatically retry failures.

**Hosted duration policy (2026-11-12):** OPS-006 publishes elapsed time for
the Ubuntu, Windows, macOS, and migration jobs in each Actions run summary; all
CI jobs have explicit time limits. The `ci-history` job compares the current
run with successful runs on the pull request's base branch (or the default
branch for pushes) and reports previous outcome counts. After ten successful
samples, a successful current job fails the workflow when its duration exceeds
the median by more than the larger of 50% of the median or five minutes. Failed/cancelled
historical jobs do not influence the duration median, and a missing GitHub API
history produces a visible warning while skipping the gate. The history
available on 2026-11-12 provides at least ten successful samples for Ubuntu,
Windows, and migration jobs; macOS remains in the warm-up period.

| Job | Successful samples | Median | Gate after warm-up |
| --- | ---: | ---: | ---: |
| Rust checks (Ubuntu) | 20 | 9m 12s | 14m 12s |
| Rust checks (Windows) | 29 | 8m 57s | 13m 57s |
| PostgreSQL 17 migrations | 40 | 1m 16s | 6m 16s |
| Rust checks (macOS) | 6 | 5m 58s | Warming up until 10 samples |

The medians were computed from the previous 50 completed `main` workflow runs
available on 2026-11-12; failed/cancelled runs are included only in the outcome
report. The report was exercised against a completed hosted run before enabling
the gate in the workflow. Flaky tests remain manually tracked under the policy
above; CI does not silently retry tests.

**Timing audit (2026-11-12):** `movement_is_session_bound_ticked_replicated_and_cancelled_on_disconnect`
in `crates/lynx_world/tests/websocket.rs` uses a 1,000 ms service tick and waits
1,200 ms before tick-dependent assertions, leaving a 200 ms scheduling margin.
`postgres_zone_lease_expiry_allows_takeover` in
`crates/lynx_world/tests/postgres_snapshot.rs` waits 700 ms after a 500 ms
lease TTL, also leaving 200 ms for scheduler/database-clock granularity. These
are documented wall-clock tolerances, not injected clocks; their external
service behavior still needs execution evidence. Other socket and reconnect
tests use bounded 5–15 second deadlines as failure guards and short polling
intervals for readiness.

The remaining intentional waits are also classified: PostgreSQL resume-session
shutdown polls `pg_stat_activity` every 10 ms until a 5-second deadline; the
snapshot timing unit test injects a store that sleeps 5 ms to exercise histogram
measurement; and the snapshot cadence test waits 120 ms for a 1 ms interval.
These are polling or measurement tolerances, not assertions that a scheduler
must run at an exact instant. The failed-auth audit test now supplies one fixed
`Instant` to all attempts and separately asserts five credential-denial events
and one rate-limit event; the first full instrumented run exposed that the old
test incorrectly counted both event classes as credential denials. No CI job
uses automatic retry for test failures.

---

## QUA-017 — Remaining architecture decision records

- **Priority:** P2
- **Status:** Done — ADRs 0023–0030 record the remaining asset/client, editor, user-content, platform, backup, admin identity, and property scope decisions; see [completion record](completed/qua-017-remaining-architecture-decisions.md).
- **Depends on:** —
- **References:** [project guide §25](../lightweight_mmorpg_engine_project_guide.md#25-architecture-decisions-to-record),
  [`docs/adr/`](../adr/)

**Why:** The project guide lists decisions that should not be accidental. ADRs
0023–0030 now record the client and asset boundary, editor approach,
user-content policy, backup objectives, platform policy, admin identity, and
property scope so dependent work does not invent those contracts.

**Scope**

- Record missing project decisions in short ADRs with context, decision,
  consequences, and status.
- Cross-link them from the affected docs/tasks.
- Keep D2/D5/D6 and the platform gate linked to their explicit decisions.
- Keep ADR numbering and the index/README current.

**Acceptance criteria**

- [x] Each remaining topic has an accepted or explicitly deferred ADR.
- [x] The client/editor/ops tasks link to the decisions recorded here.
- [x] ADR index and README counts include accepted and proposed records.
- [x] No milestone proceeds on an undocumented architecture choice; player property is explicitly deferred.

**Oversized-frame slice (2026-11-12):** The malformed-handshake WebSocket test
now also sends a frame header declaring `MAX_FRAME_BYTES + 1`. The transport
returns stable `capacity_exceeded`, closes the connection, does not attach a
session or apply an action, and does not echo the bearer credential. The test
passes locally.

**Deep-payload slice (2026-11-12):** The same authenticated WebSocket test now
also sends a correctly framed envelope with a body nested beyond
`MAX_JSON_DEPTH`. It receives stable `capacity_exceeded`, closes before
admission, and leaves session/action counters at zero without echoing the
bearer. Connection-flood, systematic log-path redaction, and a
complete case-by-case abuse matrix remain; see `docs/agents/issue-tracker.md`.

**Slow-handshake slice (2026-11-12):** A transport unit test gives the
`session.hello` deadline a 25 ms test value while production retains the
10-second default. A peer that completes the WebSocket upgrade but sends no
hello receives stable `invalid_request` and is closed; no authority admission
event or session attachment occurs, and the bearer token is not echoed.
Connection-flood and systematic log-path redaction cases remain open.
