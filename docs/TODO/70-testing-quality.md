# Testing, quality, and technical debt backlog

Scope: the test strategy and the engineering hygiene that keeps the guarantees
true — authority/security tests, determinism, protocol fixtures, database
integration, load/soak, fuzzing, coverage, accessibility, documentation drift,
toolchain upgrades, and flaky-test control.

**Current baseline:** unit and integration tests, a zero-unsafe workspace,
fmt/Clippy/test gates, a generated-project CI check, and a PostgreSQL migration
job. The Ubuntu Rust job configures the live PostgreSQL test and requires its
configuration; local runs can skip it when unconfigured. Hosted CI confirmed
the PostgreSQL, Markdown, Ubuntu, and Windows gates on 2026-10-29. There is no
coverage report, fuzz target, or load harness. A Markdown link checker and its
tests run locally and in CI.

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

---

## QUA-002 — Determinism and property-based tests

- **Priority:** P1
- **Status:** Ready
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

- [ ] Core invariants have property tests with generators for edge cases.
- [ ] Replay tests are deterministic across runs and machines.
- [ ] A failure produces a minimal, reproducible seed/case.
- [ ] New persistent aggregates add invariants to the suite.

---

## QUA-003 — Protocol golden and compatibility tests

- **Priority:** P1
- **Status:** In progress — handshake/error negotiation goldens and compatibility/unknown-field behavior are covered; world-message fixtures and breaking-change gate remain open
- **Depends on:** —
- **References:** `NET-004`, `engine_net::protocol`

**Scope**

- Checked-in fixtures for every client/server wire kind, including errors and
  handshake negotiation.
- Major/minor/feature compatibility matrix tests.
- Unknown-field behavior tests (reject vs. ignore by contract).
- A diff gate that requires a protocol version bump/changelog for breaking
  changes.

**Acceptance criteria**

- [ ] Every wire kind has at least one golden fixture.
- [ ] Accidental field/format changes fail CI.
- [ ] Compatibility matrix outcomes match the documented policy.
- [ ] Fixtures are versioned with the protocol.

**Progress:** `crates/engine_net/tests/protocol_golden.rs` pins hello, version
mismatch error, and negotiated handshake wire forms and exercises the major,
minor, content, and feature compatibility outcomes. It now also records the
serde contract that unknown envelope and handshake fields are ignored while
unknown feature names are rejected. This is intentionally bounded: the protocol
does not yet have checked-in fixtures for all world-message kinds, nor a diff
gate requiring a version/changelog update for breaking wire changes. Fixtures
currently use a `1.0.0` filename but are not yet an exhaustive per-kind,
versioned fixture set.

---

## QUA-004 — PostgreSQL integration matrix

- **Priority:** P1
- **Status:** Ready
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
- [ ] A configured-but-unreachable database fails loudly.
- [ ] Tests are isolated and parallel-safe or explicitly serialized.
- [x] Migration/schema failures produce actionable diagnostics.
- [x] No test depends on Docker for local execution.

---

## QUA-005 — Load and soak benchmarks

- **Priority:** P2
- **Status:** Ready
- **Depends on:** —
- **References:** `OPS-007`, `SIM-018`, `NET-006`

**Scope**

- A harness that simulates many sessions/entities and records tick latency,
  throughput, memory, and network bytes.
- Short benchmark runs in CI/nightly and longer soak runs before release.
- Scenarios: idle crowd, movement storm, chat burst, action spam, reconnect
  storm, and database latency injection.
- Baseline results stored with hardware/topology.

**Acceptance criteria**

- [ ] Benchmarks are repeatable and documented.
- [ ] Results identify the first bottleneck (tick, replication, DB, transport).
- [ ] Soak runs detect leaks and latency drift.
- [ ] Regressions beyond the agreed budget are visible.

---

## QUA-006 — Security abuse test suite

- **Priority:** P1
- **Status:** In progress — duplicate-Origin rejection regression added; abuse matrix remains incomplete
- **Depends on:** —
- **References:** [security](../security.md), `NET-002`, `NET-007`, `NET-011`

**Scope**

- Forged identity/role/actor fields, oversized/deep payloads, malformed frames,
  replay, token guessing, and duplicate sessions.
- Rate-limit bursts, slow clients, connection floods, and origin violations.
- Content/path traversal for tools and MCP once they exist.
- Assert stable error codes and no secret leakage in responses/logs.

**Acceptance criteria**

- [ ] Each documented abuse case has a test or an explicit gap task.
- [ ] Tests assert both rejection and the absence of side effects.
- [ ] Secret-redaction assertions cover error/log paths.
- [ ] The suite runs in CI and is extended with each security task.

**Progress:** `crates/lynx_world/tests/websocket.rs` now checks that a request
with multiple `Origin` headers is rejected with the stable `origin_forbidden`
response and never reaches gateway admission. This covers the documented
origin-violation case where one header is allowlisted and another is hostile.
The broader abuse matrix, state-side-effect checks for other rejection paths,
and comprehensive secret-redaction assertions remain open.

---

## QUA-007 — Coverage reporting

- **Priority:** P2
- **Status:** Ready
- **Depends on:** —
- **References:** `QUA-001`, `QUA-006`

**Scope**

- Generate line/region coverage with `cargo-llvm-cov` or equivalent.
- Publish a trend and per-crate report; avoid vanity thresholds.
- Identify untested critical paths: authority, persistence, replay, recovery.
- Add targeted tests for risky gaps rather than blanket percentage goals.

**Acceptance criteria**

- [ ] Coverage is generated in CI/nightly and visible to contributors.
- [ ] Critical modules have explicit coverage expectations.
- [ ] New code does not reduce critical-path coverage without justification.
- [ ] Reports exclude generated/test-only code appropriately.

---

## QUA-008 — Fuzzing

- **Priority:** P2
- **Status:** Ready
- **Depends on:** —
- **References:** `engine_net` envelope/framing, `engine_content`

**Scope**

- Fuzz envelope/frame decoding, content parsing, snapshot decoding, and command
  payloads.
- Maintain a corpus of minimized cases; run bounded fuzzing in nightly CI.
- Assert no panics, unbounded allocation, or secret leakage.
- Triage crashes into regression tests.

**Acceptance criteria**

- [ ] Fuzz targets build and run locally and nightly.
- [ ] A crash produces a reproducible input and a regression test.
- [ ] Memory/time limits are enforced.
- [ ] The corpus is checked in or cached.

---

## QUA-009 — Documentation and code consistency sweep

- **Priority:** P1
- **Status:** Done — documentation sweep and hosted Markdown/CI verification completed
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
- **Status:** Ready
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

- [ ] Boundary errors have stable codes and documented retryability.
- [ ] Player-facing errors resolve to locale keys.
- [ ] Secret/PII redaction is tested.
- [ ] A diagnostics catalog exists for operators.

---

## QUA-011 — Toolchain and Bevy upgrade rehearsal

- **Priority:** P2
- **Status:** Ready
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
- [ ] Engine crates still build without Bevy.
- [ ] Generated projects receive a migration note and validation.
- [ ] Rollback to the previous toolchain is documented.

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

## QUA-014 — Multi-client headless harness

- **Priority:** P1
- **Status:** Ready
- **Depends on:** `CLT-005`
- **References:** `CLT-014`, `NET-004`

**Why:** Phase 2's exit criterion is two clients observing consistent state, and
many later tests need more than one session. Building the harness once avoids
copy-pasted socket code in every integration test.

**Scope**

- A reusable headless client that connects, handshakes, selects a character,
  sends actions/movement/chat, and records snapshots/deltas.
- Helpers for latency/loss injection, reconnect, and token rotation.
- Assertions for consistency across observers and per-session filtering.
- Runs against an in-process or spawned server without a GPU.

**Acceptance criteria**

- [ ] Two harness clients observe the same authoritative state.
- [ ] The harness supports reconnect and simulated network conditions.
- [ ] It is usable from `crates/*/tests` without duplicating protocol code.
- [ ] It runs in CI within a bounded time.

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
- **Status:** In progress — tracking policy and hosted baseline documented; automated reporting and timing-test audit remain open
- **Depends on:** —
- **References:** `OPS-006`

**Scope**

- Detect and quarantine flaky tests with an owner and expiry.
- Measure CI duration per job and fail on unexplained growth.
- Stabilize timing-sensitive tests with deterministic clocks/injection.
- Avoid retries as a permanent fix; require a root cause.

**Acceptance criteria**

- [ ] Flaky tests are tracked, not silently retried.
- [ ] CI duration is visible and bounded.
- [ ] Timing-sensitive tests use injected time or documented tolerances.
- [ ] Quarantined tests have an owner and expiry.

**Tracking policy:** A flaky test is recorded here with its exact test name,
platform/workflow, first and latest observed failing run, issue/owner, and an
expiry date no more than 30 days out. Quarantine requires an explicit skip or
filter and a link to the tracking entry; CI must not silently retry it. The
owner removes the quarantine after fixing the cause, or renews it with a new
reason and expiry. No tests are currently listed as quarantined. This is a
manual policy until automated flaky-test reporting is introduced.

**Hosted duration baseline (2026-10-29):** OPS-006 recorded just under ten
minutes overall, approximately three minutes for Ubuntu workspace tests and
90 seconds for Clippy; Windows workspace tests exceeded ten minutes. These
observations are a historical baseline. OPS-006 now publishes elapsed time for
the Ubuntu and Windows Rust jobs in each Actions run summary and bounds either
job with a 30-minute timeout. This is visibility and a hard ceiling, not
automated trend detection or an unexplained-growth failure threshold. No
threshold is inferred from the single historical run. Flaky failures remain
manually tracked under the policy above; CI does not silently retry tests.

---

## QUA-017 — Remaining architecture decision records

- **Priority:** P2
- **Status:** Ready
- **Depends on:** —
- **References:** [project guide §25](../lightweight_mmorpg_engine_project_guide.md#25-architecture-decisions-to-record),
  [`docs/adr/`](../adr/)

**Why:** The project guide lists decisions that should not be accidental. Most
exist as ADRs 0001–0022, but several topics are still only prose or absent:
asset formats/import, camera/input abstraction, admin identity/role model,
initial platforms/minimum hardware, backup retention/RPO/RTO, property
ownership/lease, and user-content publishing/moderation. These decisions block
the client, editor, and operations milestones.

**Scope**

- Write short ADRs for each missing topic, with context, decision, consequences,
  and status.
- Cross-link them from the affected docs/tasks.
- Resolve the D2/D5/D6 gates with an explicit decision.
- Keep ADR numbering and the index/README current.

**Acceptance criteria**

- [ ] Each missing topic has an accepted or explicitly rejected ADR.
- [ ] The client/editor/ops tasks link to the decision they depend on.
- [ ] ADR index and README counts include proposed records.
- [ ] No milestone proceeds on an undocumented architecture choice.
