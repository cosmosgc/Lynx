# Networking and security backlog

Scope: the world edge — authentication, admission, rate limits, transport
security, protocol compatibility, replication delivery, moderation, abuse
detection, secrets, and security audit.

**What exists today:** a binary WebSocket transport with a length-prefixed,
versioned envelope; a `session.hello` handshake with a 10-second timeout; static
hashed bearer tokens plus gateway-issued resume tokens; per-session interest
filtering, resume windows, and session-scoped idempotency; health/readiness
endpoints; frame, depth, and string bounds. There is no production identity
integration, rate limiting, TLS, origin allowlist, durable session store, or
moderation surface.

**Related design:** [networking](../networking.md), [security](../security.md),
[operations](../operations.md),
[ADR 0012](../adr/0012-interest-management-and-replication-audiences.md),
[ADR 0013](../adr/0013-world-gateway-and-reconnect.md),
[ADR 0014](../adr/0014-framed-transport-and-world-server.md).

---

## NET-001 — Production authentication at the world edge

- **Priority:** P0
- **Status:** Ready
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
- Keep static token mode explicitly development-only: require loopback binding
  or an opt-in flag and log a startup warning.
- Map identity failures to stable error codes without leaking backend details.
- Decide and document token lifetimes, clock skew, and revalidation cadence.
- Keep the gateway free of identity-provider types; pass trusted claims across
  the seam.

**Acceptance criteria**

- [ ] An expired, revoked, malformed, or unknown session is rejected before a
      gateway session is created.
- [ ] Selecting a character owned by another account is rejected and audited.
- [ ] Static-token mode cannot be enabled on a non-loopback listener without an
      explicit development flag.
- [ ] An integration test with a fake identity backend covers login, expiry, and
      ownership failure.
- [ ] No token or credential appears in logs, errors, or debug output.

---

## NET-002 — Rate limiting and message budgets

- **Priority:** P0
- **Status:** Ready
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

- [ ] A burst above the configured budget is rejected without affecting other
      sessions.
- [ ] Limits apply after authentication and use server-resolved identity, not
      client claims.
- [ ] Slow clients are detached with a documented reason before queues grow
      without bound.
- [ ] Rejections are counted by kind/error code for alerting.
- [ ] Tests cover token-bucket refill, burst, and reconnect behavior.

---

## NET-003 — TLS and reverse-proxy boundary

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** [networking §Transport](../networking.md#transport),
  [operations](../operations.md), `PST-007`

**Why:** The binary carries no TLS; the documented deployment terminates TLS at
a proxy. That is a valid design, but the repository lacks a concrete, tested
proxy configuration and a native-TLS escape hatch for non-proxy deployments.

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

- [ ] A reference proxy config upgrades `/world` and preserves the
      `Authorization` header.
- [ ] Spoofed forwarding headers from untrusted peers are ignored.
- [ ] Native TLS, if implemented, verifies certificates and refuses plaintext
      when configured.
- [ ] The deployment boundary and trust assumptions are documented.

---

## NET-004 — Protocol compatibility lifecycle

- **Priority:** P1
- **Status:** Ready
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

- [ ] Old/new client combinations have documented and tested outcomes.
- [ ] A breaking wire change cannot merge without a protocol version bump and
      changelog entry.
- [ ] Golden fixtures catch accidental wire changes.
- [ ] Downgrade/rollback behavior is documented.

---

## NET-005 — Multi-zone gateway routing

- **Priority:** P2
- **Status:** Blocked — `SIM-014`, `PST-004`
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

- [ ] A client cannot join a zone it is not routed to.
- [ ] Travel between zones is atomic from the client's perspective: one
      character exists in exactly one zone.
- [ ] Zone failure produces a recoverable error and does not strand sessions.
- [ ] Routing decisions and handoffs are observable in metrics/logs.

---

## NET-006 — Replication delivery SLO and backpressure

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
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

- [ ] Metrics expose queue depth and delivery outcomes per session.
- [ ] A slow client cannot increase another session's latency beyond the target.
- [ ] Overflow triggers a documented resync rather than silent state loss.
- [ ] A load test validates the byte/tick budget at the target entity count.

---

## NET-007 — Session and token abuse defenses

- **Priority:** P1
- **Status:** Ready
- **Depends on:** `NET-002`
- **References:** [networking §Reconnect and recovery](../networking.md#reconnect-and-recovery),
  ADR 0013

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

- [ ] Brute-force and replay attempts are rate-limited and audited.
- [ ] Two clients cannot hold the same session; the loser receives the
      documented conflict error.
- [ ] Revocation takes effect without waiting for the resume window.
- [ ] Alerts distinguish attack-like patterns from ordinary reconnects.

---

## NET-008 — Admin and moderation actions

- **Priority:** P2
- **Status:** Blocked — `NET-001`
- **Depends on:** `NET-001`
- **References:** [security §Moderation](../security.md), ADR 0002

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
- **Status:** Ready
- **Depends on:** —
- **References:** [operations](../operations.md),
  [security §Threat model](../security.md#threat-model)

**Scope**

- Prefer environment/OS secret stores over command-line secrets; document the
  process-list exposure of flags.
- Support rotation for database credentials, identity-provider secrets, and
  static development tokens.
- Hash credentials at rest where the protocol allows; never log plaintext.
- Define startup failure behavior for missing/weak secrets.
- Add a secret-redaction test that runs representative error paths.

**Acceptance criteria**

- [ ] No plaintext secret is required on the command line for production.
- [ ] Rotating a database credential does not require a code change.
- [ ] Logs and errors redact tokens, passwords, and DSNs.
- [ ] Weak/default development secrets are refused outside development mode.

---

## NET-011 — Anti-cheat and anomaly detection

- **Priority:** P2
- **Status:** Blocked — `NET-002`, `SIM-001`, `SIM-003`
- **Depends on:** `NET-002`, `SIM-001`, `SIM-003`
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

**Acceptance criteria**

- [ ] Replayed or forged inputs are rejected or flagged with evidence.
- [ ] Detection thresholds are configurable and documented.
- [ ] False-positive handling and manual review exist before punitive action.
- [ ] Tests simulate representative cheats and assert the outcome.

---

## NET-012 — Connection admission and origin policy

- **Priority:** P1
- **Status:** Ready
- **Depends on:** —
- **References:** [networking §Transport](../networking.md#transport),
  `crates/lynx_world/src/transport.rs`

**Why:** Handshake timeout and frame caps exist, but admission is still open to
connection floods and browser cross-origin upgrades. These are cheap controls to
add before a client is published.

**Scope**

- Maximum concurrent connections and per-IP connection caps.
- Idle timeout, handshake timeout (already present), and slow-read protection.
- WebSocket `Origin` allowlist for browser clients; reject unexpected origins.
- Bound header sizes and reject malformed upgrade requests before allocating
  session state.
- Readiness/draining behavior: stop accepting, finish or reject in-flight
  handshakes, and close cleanly.

**Acceptance criteria**

- [ ] A connection flood cannot exhaust memory or file descriptors.
- [ ] Browser clients from unlisted origins are rejected when origin policy is
      enabled.
- [ ] Draining stops new admissions while existing sessions complete or close
      predictably.
- [ ] Admission rejections have stable errors and metrics.

---

## NET-013 — Security audit logging and alerting

- **Priority:** P2
- **Status:** Ready
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
- [ ] Log retention and access are documented.
