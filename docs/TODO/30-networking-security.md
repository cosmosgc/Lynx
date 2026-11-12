# Networking and security backlog

Scope: the world edge — authentication, admission, rate limits, transport
security, protocol compatibility, replication delivery, moderation, abuse
detection, secrets, and security audit.

**What exists today:** a binary WebSocket transport with a length-prefixed,
versioned envelope; a `session.hello` handshake with a 10-second timeout; static
hashed bearer tokens plus gateway-issued resume tokens; per-session interest
filtering, resume windows, and session-scoped idempotency; health/readiness
endpoints; frame, depth, and string bounds; bounded admission, per-session and
per-IP rate limits, and an optional exact-match Origin allowlist. There is no
production identity integration, TLS, durable session store, or moderation
surface.

**Related design:** [networking](../networking.md), [security](../security.md),
[operations](../operations.md),
[ADR 0012](../adr/0012-interest-management-and-replication-audiences.md),
[ADR 0013](../adr/0013-world-gateway-and-reconnect.md),
[ADR 0014](../adr/0014-framed-transport-and-world-server.md).

---

## NET-001 — Production authentication at the world edge

- **Priority:** P0
- **Status:** Done
- **Completion record:** [net-001-production-authentication-at-the-world-edge.md](completed/net-001-production-authentication-at-the-world-edge.md)
- **Depends on:** —
- **References:** [networking §Transport](../networking.md#transport),
  [architecture §What is deliberately not here yet](../architecture.md#what-is-deliberately-not-here-yet),
  `crates/lynx_world/src/auth.rs`, ADR 0018

**Why:** The service accepts static hashed bearer tokens and spawns one demo
character per token. That is a controlled-development mechanism, not a login.
Production sessions must validate a Nakama (or chosen) session and resolve which
characters the account may control.

**Scope**

- Validate the presented session with the identity backend on admission,
  including expiry, revocation, and refresh semantics.
- Resolve account → owned characters → selected character; reject sessions that
  try to control a character they do not own.
- Add a server-side concurrent-session policy with `per_character` and
  `per_account` options. Keep the current per-character behavior as the
  default; enforce per-account limits using only authenticated account claims,
  including retained resume sessions, without replacing an existing session.
- Keep static token mode explicitly development-only: require loopback binding
  or an opt-in flag and log a startup warning.
- Map identity failures to stable error codes without leaking backend details.
- Decide and document token lifetimes, clock skew, and revalidation cadence.
- Keep the gateway free of identity-provider types; pass trusted claims across
  the seam.

**Implementation:** `--auth-mode nakama` constructs the selected
`NakamaPostgresBackend` behind `auth::IdentityBackend`. Fresh admissions validate
the Nakama bearer session, parse exactly one `X-Lynx-Character` `chr_` selection,
and resolve active ownership in the account-scoped PostgreSQL store before
creating trusted non-staff gateway claims. Identity failures have stable redacted
HTTP codes and foreign selections emit a structured admission audit record.
Verification runs on Tokio's blocking pool, so provider and PostgreSQL I/O do
not block socket tasks. Resume tokens remain local; their configured TTL bounds
the upstream-revocation observation gap. The deliberate population policy is to
admit only an already-present authoritative actor; persistent rows neither spawn
actors nor fall back to demo characters. New-character loading and multi-zone
routing remain separate backlog work. The client now persists its device ID and
rotating refresh token in the OS credential store, refreshes on login, supports
`--logout`, and can list/create/select characters through the authenticated
world HTTP API. Character creation and selection do not yet provision or load a
live actor, and the client has no character onboarding UI.

**Acceptance criteria**

- [x] An expired, revoked, malformed, or unknown session is rejected before a
      gateway session is created.
- [x] Selecting a character owned by another account is rejected and audited.
- [x] The configured session policy rejects a duplicate character or account
      session as appropriate, including during resume windows, without
      disconnecting the existing session.
- [x] Static-token mode cannot be enabled on a non-loopback listener without an
      explicit development flag.
- [x] An integration test with a fake identity backend covers login, expiry, and
      ownership failure.
- [x] No token or credential appears in logs, errors, or debug output.

---

## NET-002 — Rate limiting and message budgets

- **Priority:** P0
- **Status:** Done
- **Completion record:** [net-002-rate-limiting-and-message-budgets.md](completed/net-002-rate-limiting-and-message-budgets.md)
- **Depends on:** —
- **References:** [security §Abuse prevention](../security.md#abuse-prevention),
  `crates/engine_net/src/envelope.rs`

**Why:** The transport enforces frame and envelope sizes and a session cap, but
there is no per-message rate limit. A connected client can spam actions, chat, or
movement requests and starve the authority loop.

**Scope**

- Per-IP admission limits (handshakes, failed auth, connection churn).
- Per-session token buckets for total messages and per-kind budgets (actions,
  chat, movement, calendar, UI).
- Per-action-type limits and duplicate-request collapse where safe.
- Bounded outbound queues; detach slow clients rather than stalling the tick.
- Stable `CapacityExceeded`/`RateLimited` errors, close reasons, and metrics.
- Configuration with safe defaults and documentation; no unbounded knobs.

**Acceptance criteria**

- [x] A burst above the configured budget is rejected without affecting other
      sessions.
- [x] Limits apply after authentication and use server-resolved identity, not
      client claims.
- [x] Slow clients are detached with a documented reason before queues grow
      without bound.
- [x] Rejections are counted by kind/error code for alerting.
- [x] Tests cover token-bucket refill, burst, and reconnect behavior.

---

## NET-003 — TLS and reverse-proxy boundary

- **Priority:** P1
- **Status:** In progress — the reference Nginx config passes local syntax validation and is CI-gated; certificate-backed staging validation remains
- **Depends on:** —
- **References:** [networking §Transport](../networking.md#transport),
  [operations](../operations.md), `PST-007`

**Why:** The binary carries no TLS; the documented deployment terminates TLS at
a proxy. A reference Nginx configuration and trust-boundary documentation now
exist. Native TLS remains unimplemented and the proxy configuration has not yet
been validated in a deployed/test Nginx environment.

**Scope**

- Document and test a reference reverse-proxy configuration for WebSocket
  upgrade, timeouts, header limits, and forwarded client identity.
- Trust forwarded headers only from configured proxies; never from arbitrary
  clients.
- Decide whether to add optional native TLS (rustls) for single-process
  deployments; if yes, implement it behind a feature/flag.
- Define HSTS/redirect behavior and secure cookie/token handling for browser
  clients.
- Keep loopback plaintext as the development default.

**Acceptance criteria**

- [x] A reference proxy config upgrades `/world` and preserves the
      `Authorization` header.
- [x] Spoofed forwarding headers from untrusted peers are ignored (the service
      does not consume forwarding headers; the reference proxy strips common
      variants).
- [ ] Native TLS, if implemented, verifies certificates and refuses plaintext
      when configured.
- [x] The deployment boundary and trust assumptions are documented.

**Progress:** `deploy/nginx/lynx-world.conf` is a TLS-terminating `/world`
reference with WebSocket upgrade, Authorization and character-header pass-through,
bounded header handling, and explicit timeouts. The service uses the socket peer
IP and does not trust forwarding headers; proxy-origin client IP support is not
implemented. HSTS/redirect and bearer-token handling guidance is documented.
Native TLS is deliberately not added. CI runs `nginx -t` with a generated
short-lived certificate, and local Nginx 1.31.8 accepted the configuration with
the same isolated certificate substitution. A real certificate-backed staging
deployment and WebSocket upgrade check remain outstanding.

---

## NET-004 — Protocol compatibility lifecycle

- **Priority:** P1
- **Status:** Done
- **Completion record:** [net-004-protocol-compatibility-lifecycle.md](completed/net-004-protocol-compatibility-lifecycle.md)
- **Depends on:** —
- **References:** [networking](../networking.md), `engine_net::protocol`,
  [ADR 0006](../adr/0006-content-and-protocol-formats.md)

**Why:** Envelopes carry a semantic version and the handshake negotiates major,
minor, and features. What is missing is a published compatibility policy:
which versions the server supports, how long clients may lag, how features
degrade, and how breaking changes are staged.

**Scope**

- Define supported major/minor ranges and feature-flag degradation rules.
- Reject incompatible clients with actionable, localized errors before world
  entry.
- Publish a deprecation policy and changelog entries for protocol changes.
- Add golden request/response fixtures for every wire kind, checked in CI.
- Ensure unknown fields are rejected where authority depends on it and ignored
  only where the contract explicitly allows.
- Version the content bundle independently and check both at the boundary.

**Acceptance criteria**

- [x] Old/new client combinations have documented and tested outcomes.
- [x] A breaking wire change cannot merge without a protocol version bump and
      changelog entry.
- [x] Golden fixtures are compared with production serialization or parsing
      paths for every live wire kind; the CI catalog asserts all 27 kinds are
      covered, and the bearer fixture uses only a synthetic token.
- [x] Downgrade/rollback behavior is documented.

**Progress:** The networking policy documents current major/minor/content
negotiation, feature degradation, deprecation, and rollback limits. Protocol
tests cover the version/content matrix, unknown feature rejection, and golden
hello, mismatch error, negotiated parameters, and world messages. The CI
pull-request gate now compares the merge base and requires a protocol version
increase plus a changelog entry when client request/decoding, envelope,
handshake, replication, or world-server contract sources change. A safe golden
for the credential-bearing `world.token` envelope uses the explicit
`FAKE_LYNX_TEST_TOKEN_DO_NOT_AUTHENTICATE` sentinel and verifies debug redaction.
The fixture is in `crates/world_server/tests/protocol_golden.rs` and CI runs it;
no real bearer token is checked in. Protocol and content major mismatches are
rejected at negotiation; minor versions within the active major remain
accepted, and a major cutover has no overlap window.
`crates/world_server/tests/fixtures/wire-kinds-1.1.0.json` now catalogs all 27
live message kinds, including chat results, `session.ack`, and the synthetic
token envelope. Versioned goldens now flow through the protocol, gateway,
calendar, world-server, and world-service production paths. The service token
test substitutes only the fixture sentinel into the actual token envelope and
compares its encoded form, preserving secret redaction. NET-004's acceptance
criteria are complete.

---

## NET-005 — Multi-zone gateway routing

- **Priority:** P2
- **Status:** In progress — authored-map zone ownership, capacities, identity routing, and zone-scoped resume routing are wired; live handoff still depends on an authored in-zone trigger contract
- **Depends on:** `SIM-014`, `PST-004`
- **References:** [architecture §Simulation model](../architecture.md#simulation-model),
  `SIM-014`, ADR 0002

**Scope**

- A zone registry with ownership, capacity, and health.
- Admission routing to the zone that owns the selected character.
- Handoff protocol and redirect semantics for travel; no client-selected zone
  authority.
- Cross-zone presence/chat/party routing.
- Failure behavior when a zone is full, draining, or unreachable.

**Acceptance criteria**

- [x] A client cannot join a zone it is not routed to: existing characters
      and durable resumes are resolved to their owning zone, and new
      characters enter only through the map's default zone.
- [ ] Travel between zones is atomic from the client's perspective: one
      character exists in exactly one zone.
- [ ] Zone failure produces a recoverable error and does not strand sessions.
- [x] Admission routing and zone-local authority decisions include the zone ID
      in structured logs; handoff-specific observability remains with travel.

---

## NET-006 — Replication delivery SLO and backpressure

- **Priority:** P1
- **Status:** In progress — aggregate queue/write telemetry, configurable queue bounds, and slow-peer isolation are implemented; per-session diagnostics and measured delivery targets remain
- **Depends on:** `QUA-005` for measured SLO/load validation; the telemetry and
  isolation slice can proceed independently.
- **References:** [ADR 0012](../adr/0012-interest-management-and-replication-audiences.md),
  [networking §Transport](../networking.md#transport)

**Why:** Interest filtering and per-connection queues exist, but there is no
measured delivery target or documented slow-client policy beyond "detach". As
entity counts and movement grow, replication will become the first bottleneck.

**Scope**

- Define per-tick byte budgets and delivery targets for snapshots/deltas.
- Measure queue depth, dropped updates, detach counts, and bytes per session.
- Prioritize authoritative/critical updates over cosmetic ones under pressure.
- Define delta coalescing and snapshot resynchronization after overflow.
- Document the slow-client policy and make it configurable within safe bounds.

**Acceptance criteria**

- [x] Metrics expose aggregate queue depth, observed high-water marks, and
  snapshot/delta WebSocket write outcomes and completed bytes with bounded labels.
- [x] Per-session delivery counts/bytes and queue occupancy are available through
  a bounded, cardinality-safe diagnostic surface.
- [x] A full slow-peer queue is detached without blocking a healthy peer's queue;
  the synchronous authority test covers isolation, but not a numeric latency SLO.
- [x] Overflow recovery is documented and tested: detach the slow peer; on
  reconnect/resume, send an authoritative snapshot before later deltas.
- [ ] A load test validates a measured byte/tick budget at the target entity
  count and establishes the delivery target (`QUA-005`).
- [x] Queue capacity is configurable within safe server-owned bounds:
  `--outbound-queue` accepts 8–256 frames (default 64) and rejects values
  outside that range at startup.
- [ ] Critical-update priority and delta coalescing are designed and tested.

**Implementation state:** `lynx_world` exports queue-depth samples, aggregate
and maximum single-connection observed high-water gauges, and bounded
snapshot/delta socket-write frame outcomes and completed byte counters. The
per-connection queue is sized by the validated `--outbound-queue` flag (8–256
frames, default 64), so operators can trade worst-case per-session memory
against tolerance for scheduler stalls without changing the code. The transport
integration test observes a completed snapshot send. A service test fills one
peer's queue, verifies it is detached, and verifies another peer still receives
its frame. Resume coverage asserts an authoritative snapshot is queued.
Session-ID-labeled measurements are intentionally not added to Prometheus
because session churn creates unbounded label cardinality. The private admin
listener now serves `/diagnostics/replication`, a fixed 256-entry recency view
with opaque session IDs, per-session enqueue/write frame and byte counts, queue
occupancy/capacity, and an eviction count. It excludes account, character, IP,
and credential data. Numeric SLOs and byte budgets remain unmeasured pending
`QUA-005` load work; no target is invented.

---

## NET-007 — Session and token abuse defenses

- **Priority:** P1
- **Status:** In progress — resume-token attempts have an independent bounded per-IP bucket and safe audit records; duplicate-session conflict and immediate trusted revocation are covered; PromQL thresholds are documented, with a staging alert exercise remaining
- **Depends on:** `NET-002`
- **References:** [networking §Reconnect and recovery](../networking.md#reconnect-and-recovery),
  ADR 0013

**Progress (2026-11-10):** Syntactically valid resume credentials now consume a
separate per-peer-IP token bucket before the gateway is asked to resolve the
token. It has bounded defaults (10 attempts, refilling at 2/s), emits the
normalized `resume_attempt`/`rate_limited` counter, and does not log or expose the
credential. Unit and WebSocket integration tests verify bucket independence and
that a second guessed token is denied before upgrade. Failed credentials and
resume admissions now emit structured records containing only peer IP and a
stable reason; attempts beyond the authentication budget remain aggregated in
bounded metrics. Existing gateway/service tests cover duplicate-session
conflicts and trusted immediate revocation, including invalidating retained
resume claims. PromQL alert examples are documented in `docs/operations.md`; a
staging alert exercise remains.

**Scope**

- Detect reconnect storms, token guessing, duplicate connections, and rapid
  resume/rotate cycles.
- Rate-limit failed handshakes and resume attempts independently of successful
  traffic.
- Ensure a stolen resume token cannot race the legitimate client indefinitely;
  define the conflict/rotation behavior under concurrency.
- Alert on spikes in `SessionExpired`, `Conflict`, and failed admissions.
- Provide operator tooling to revoke a session/account immediately.

**Acceptance criteria**

- [x] Brute-force and replay attempts are rate-limited and audited without
      recording bearer credentials.
- [x] Two clients cannot hold the same session; the loser receives the
      documented conflict error.
- [x] Revocation takes effect without waiting for the resume window.
- [x] Example alerts distinguish failed/throttled resume attempts from successful reconnects.
- [ ] Alert thresholds are exercised and tuned against staging traffic.

---

## NET-008 — Admin and moderation actions

- **Priority:** P2
- **Status:** Blocked — production OIDC issuer, role-claims mapping, MFA policy, and callback/origin settings are not configured
- **Depends on:** `NET-001`
- **References:** [security §Moderation](../security.md), ADR 0002,
  [ADR 0029](../adr/0029-admin-identity-and-roles.md)

**Scope**

- Role-checked admin actions: inspect session/zone, kick, mute, ban, revoke,
  transfer character, and adjust moderation state.
- Spectator/read-only inspection that cannot mutate world state.
- Every action audited with actor, role, target, reason, before/after, and
  correlation ID.
- No admin authority from client-supplied roles or flags.
- A minimal operator interface (CLI or web) with deliberate confirmation for
  destructive actions.

**Acceptance criteria**

- [ ] Role checks are enforced server-side and covered by tests per role.
- [ ] Admin actions are written to the append-only audit log in the same
      transaction as the mutation where applicable.
- [ ] Spectator mode cannot issue gameplay or admin mutations.
- [ ] Revocation/kick takes effect on the live connection promptly.

---

## NET-009 — Chat and content moderation pipeline

- **Priority:** P2
- **Status:** Blocked — `SIM-008`, `CNT-008`
- **Depends on:** `SIM-008`, `CNT-008`
- **References:** [security §Content and user-generated content](../security.md#content-and-user-generated-content),
  `SIM-008`

**Scope**

- Report, block, mute, and moderation-queue flows for chat and user content.
- Evidence retention with privacy redaction and access control.
- Moderator decisions with reasons, reversibility, and audit.
- Revocation removes content from new sessions without deleting audit history.
- Appeal/expiry policy for temporary restrictions.

**Acceptance criteria**

- [ ] Reports are rate-limited and cannot be used to harass.
- [ ] Mutes/blocks apply across reconnect and are visible to the sender only as
      allowed by policy.
- [ ] Moderation actions are auditable and reversible.
- [ ] Revoked content is not delivered to new sessions.

---

## NET-010 — Secret management and rotation

- **Priority:** P1
- **Status:** In progress — environment injection, mounted secret-file injection, sanitized database/provider errors, CLI debug redaction, and an operational rotation procedure are covered; provider-specific integration and production rotation drills remain
- **Depends on:** —
- **References:** [operations](../operations.md),
  [security §Threat model](../security.md#threat-model)

**Progress (2026-11-10):** `ServeArgs` debug output now reports only the number
of configured token hashes, and a sentinel test guards against exposing their
values. The existing unreachable-PostgreSQL and Nakama error tests cover
password/provider-error redaction. `docs/operations.md` now describes environment
injection and staged/rolling rotation for database roles, Nakama keys, and
overlapping development-token hashes. Managed secret-store integration and
production rotation drills remain open.

**Mounted secret files (2026-11-12):** The Rust service accepts `_FILE` forms
for PostgreSQL password, demo token hashes, and Nakama HTTP key. The shared
PowerShell and Bash setup helpers also support PostgreSQL application and
migration passwords, and `run-nakama.*` reads every Nakama credential from a
mounted file. A non-empty direct value cannot be combined with its file form;
blank template values count as unset. Read failures avoid secret values, and
terminal line endings are removed. The runner still writes its protected
Nakama overlay because Nakama consumes YAML configuration. Provider SDK
integration remains deployment-specific and is intentionally not selected by
the repository.

**Scope**

- Prefer environment/OS secret stores over command-line secrets; document the
  process-list exposure of flags.
- Support rotation for database credentials, identity-provider secrets, and
  static development tokens.
- Hash credentials at rest where the protocol allows; never log plaintext.
- Define startup failure behavior for missing/weak secrets.
- Add a secret-redaction test that runs representative error paths.

**Acceptance criteria**

- [x] No plaintext secret is required on the command line for production;
      production identity and database credentials are injected through the
      environment, while the token-hash CLI accepts only digests.
- [x] Rotating a database credential does not require a code change; the
      documented role rotation switches injected credentials during rollout.
- [x] Logs and errors redact tokens, passwords, and DSNs; sentinel tests cover
      CLI debug output and representative authentication, provider, and storage
      error paths.
- [x] Weak/default development secrets are refused outside development mode;
      static demo authentication is loopback-only unless the explicit
      dangerous development flag is supplied.

---

## NET-011 — Anti-cheat and anomaly detection

- **Priority:** P2
- **Status:** Blocked — `NET-008` review and moderation surface is not implemented
- **Depends on:** `NET-002`, `SIM-001`, `SIM-003`, `NET-008`
- **References:** [security §Authority invariants](../security.md#authority-invariants),
  `SIM-001`, `SIM-003`

**Why:** Once movement and combat exist, the server must detect impossible
behavior rather than only rejecting individual malformed messages.

**Scope**

- Movement anomaly checks: speed, teleport deltas, collision/occupancy, input
  cadence.
- Combat anomaly checks: range/LoS, damage source, cooldown, target selection,
  impossible stat combinations.
- Economy anomaly checks: duplication patterns, trade rings, reward replay.
- Risk scoring and review queue; avoid automatic bans without evidence.
- Privacy-aware retention of evidence and appeal path.

**Progress (2026-11-12):** Simulation now retains a bounded server-only record
for rejected client actions and movement inputs. Movement evidence distinguishes
unauthorized control, replayed sequence, pending-input, budget, and position
validation rejections without retaining coordinates or request payloads. Action
evidence records the actor and stable error code. The 256-entry ring and tests
cover forged damage and replayed movement. It is diagnostic evidence only; a
staff review surface, configurable risk threshold, time-based retention, and
appeal path depend on NET-008 and remain open.

**Acceptance criteria**

- [x] Replayed or forged inputs are rejected or flagged with bounded,
      server-only evidence.
- [ ] Detection thresholds are configurable and documented.
- [ ] False-positive handling and manual review exist before punitive action.
- [x] Tests simulate representative cheats and assert the outcome.

---

## NET-012 — Connection admission and origin policy

- **Priority:** P1
- **Status:** Done — bounded admission, origin enforcement, HTTP header and WebSocket idle deadlines, and bounded writes are implemented and tested
- **Depends on:** —
- **References:** [networking §Transport](../networking.md#transport),
  `crates/lynx_world/src/transport.rs`

**Why:** Admission controls must bound resource use and enforce the browser
origin boundary before a client is published.

**Scope**

- Maximum concurrent connections and per-IP connection caps.
- Idle timeout, handshake timeout (already present), and slow-read protection.
- WebSocket `Origin` allowlist for browser clients; reject unexpected origins.
- Bound header sizes and reject malformed upgrade requests before allocating
  session state.
- Readiness/draining behavior: stop accepting, finish or reject in-flight
  handshakes, and close cleanly.

**Completion (2026-11-12):** `LimitedStream` enforces an absolute 10-second raw
HTTP header deadline before Hyper parses a request. A WebSocket 101 response
disables that HTTP timer so upgraded connections use their separate idle
policy. An admitted-client integration test completes the handshake, receives
ready/snapshot/token, then observes closure after the configured idle deadline.
The shared WebSocket write-timeout path has a deterministic stalled-write
regression. The current `lynx-world` library suite passes (122 passed, one
ignored) and the WebSocket integration test passes (8 tests).

**Implemented slice:** `lynx-world serve --allow-origin https://example.com`
enables an exact-match Origin allowlist; repeat the option for additional
origins. Unlisted, malformed, and multiple Origin headers receive HTTP 403
`origin_forbidden` before upgrade. Requests without Origin remain allowed for
native clients; Origin is not authentication. With no options, origin checking
is disabled. The TCP listener also caps accepted HTTP connections at 1,024
before a request is parsed, so a slow-header flood receives listener-backlog
backpressure instead of consuming unbounded file descriptors. The service caps
live/pending upgraded connections at 1,024 total
and 16 per peer IP, rejects admission once draining begins, bounds aggregate
world-route headers at 16 KiB, and enforces an absolute 10-second raw HTTP
header-read deadline before the request is parsed. HTTP keep-alive reads get a
fresh deadline after each response; a WebSocket 101 upgrade disables the HTTP
timer and retains the 120-second WebSocket idle timeout. The service also
limits socket writes and authority-queue waits to five seconds. Rejections are
exposed through bounded-cardinality metrics. The slow-write timeout is tested
at the exact helper used for socket writes; an authority-queue timeout closes
cleanly but does not emit a distinct metric.

**Acceptance criteria**

- [x] A connection flood cannot exhaust memory or file descriptors: accepted
      TCP/HTTP connections, upgraded connections, headers, and per-connection
      queues are independently bounded.
- [x] Browser clients from unlisted origins are rejected when origin policy is
      enabled.
- [x] Draining stops new admissions; existing connections close on client
      closure, idle timeout, transport failure, or process shutdown.
- [x] Capacity and draining admission rejections have stable reasons and
      bounded-cardinality metrics; rate-limit rejections retain stable codes.
- [x] An incomplete raw HTTP header is closed after an absolute read deadline;
      WebSocket upgrades use the separate WebSocket idle timeout.

---

## NET-013 — Security audit logging and alerting

- **Priority:** P2
- **Status:** In progress — structured denial, rate-limit, principal-revocation, and correlated authority-rejection records exist; durable collection, admin-action coverage, and staging alert exercise remain.
- **Depends on:** `NET-001`, `NET-002`
- **References:** [security §Audit](../security.md),
  [operations §Metrics and logs](../operations.md#metrics-and-logs)

**Scope**

- Audit authentication failures, authorization denials, rate-limit trips,
  admin/moderation actions, token revocation, and suspicious replay.
- Correlate with request/session IDs without logging secrets or private chat.
- Define retention and access control for security logs.
- Alert thresholds for failed admissions, `SessionExpired` spikes, and admin
  action anomalies.

**Acceptance criteria**

- [ ] Security-relevant events are queryable by actor, IP, session, and time.
- [ ] Secrets and private content are redacted.
- [ ] Alerts fire on the documented thresholds in a staging exercise.
- [x] Log retention and access requirements are documented.

**Progress (2026-11-12):** The transport writes bounded-field structured
events for admission denials, pre-authentication and session rate limits,
principal revocation, and protocol/action-result rejections. Authority
rejections include source IP, server-issued session, server-resolved actor,
request correlation ID, and stable error code. A missing bearer credential now
has its own stable `credential_missing` reason. Pre-authentication rate-limit
events record only source IP, a fixed bucket class (`handshake`, `failed_auth`,
`resume_attempt`, or `connection_churn`), and `rate_limited`; secret values and
backend details are excluded. Existing sentinel regressions cover redaction.
`docs/operations.md` lists the event schema and collector requirements. Durable
collector extraction/retention, admin action events (NET-008), and staging
alert exercise remain; see the local completion blocker in
[issue-tracker.md](../agents/issue-tracker.md).
