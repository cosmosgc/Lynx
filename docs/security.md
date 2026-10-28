# Security and moderation

## Threat model

Trusted: the world service process, the database, and the operator's admin
session.

Untrusted: clients, client input, client clocks, client content claims, player
chat, user-authored content, MCP tool metadata and results, and external tool
servers.

## Authority invariants

- A client cannot grant itself items, currency, experience, admin roles, or
  control of an entity.
- Every action is validated server-side for actor control, distance,
  visibility, permissions, state, cooldown, and cost.
- The acting entity, character principal, staff flag, session ID, and view
  radius are resolved from the authenticated session, never from the message.
  `action.request` bodies that include `actor`, `principal`, `session`, or any
  other unknown field are rejected before the simulation runs.
- Per-session request IDs are scoped with the session ID before reaching the
  simulation's idempotency cache, so one session cannot replay another
  session's cached result.
- Replication is filtered per session. Public entity snapshots exclude
  inventories, minds, tags, and extension components; owner-only payloads are
  delivered only to the owning principal.
- Inventory and economy mutations are transactional and idempotent. Replayed
  requests cannot duplicate or destroy items.
- Stable IDs are server-assigned. Client-supplied entity IDs are rejected.
- Content bundles are hashed and version-checked; the client cannot authorize
  gameplay from local content.

## Combat authority

- An `attack` request carries no combat parameters. In particular, a `damage`
  field is rejected before an effect is applied.
- Formula v1 is `max(minimum_damage, attacker.attack - target.defense)`. The
  attacker and target values are server-owned `CombatStats`: content and
  authoritative equipment/status systems resolve base stats and loadout effects
  into them before the action pipeline runs. Older snapshots without the
  additive component use the server's v1 default (`attack: 1`, `defense: 0`),
  never a client fallback.
- Each zone persists versioned `CombatRules` with its snapshot. Content may tune
  `minimum_damage`, friendly-fire policy, and directional faction attack rules
  without changing the engine. Rules with a zero major version, zero minimum
  damage, an unsupported formula major version, invalid faction IDs, or duplicate
  directional pairs are refused at zone startup.
- Attack validation still requires server-resolved control capability, target
  interaction/range, line of sight, target liveness, and cooldown. Same-faction
  attacks are forbidden by default; authored directional rules may allow or
  forbid a pair.
- `Damaged` and `EntityDied` effects include the server-resolved source and
  target. On death, the target controller and lease are released before the
  `EntityDied` effect is emitted, so replication, loot, credit, and audit
  consumers receive an authoritative hook rather than client-chosen results.

## Authentication and sessions

- Player authentication is delegated to Nakama. The world gateway validates the
  session token on connect and on token refresh.
- Until that integration lands, `lynx-world` accepts static bearer tokens for
  development. Configuration stores only SHA-256 hashes, compared in constant
  time; plaintext tokens are never written by the service. Each hash maps to one
  server-chosen demo account and character, and a character has at most one
  connected session. This path is not production authentication and must be
  replaced by Nakama session validation before exposure.
- Length-prefixed frames are size-bounded before JSON parsing, and a partial
  frame is treated as an error rather than a clean disconnect. Handshakes have a
  deadline, and frames that fail envelope validation close the connection after
  a correlated `protocol.error`.
- `world_gateway` accepts claims only from trusted code that has already
  authenticated the player. A client cannot construct a session for another
  account or character, claim a staff role, or choose its own session ID.
- The current gateway permits at most one connected session per character.
  The planned server configuration can enforce uniqueness per character or per
  account; the account-wide policy must use the authenticated account identity,
  never a client-supplied account value. Retained sessions count against the
  session cap, so reconnect windows cannot bypass admission limits.
- Resume tokens are opaque, server-issued, short-lived (default 30 s),
  single-use, and rotated on every resume. They are redacted from `Debug` and
  must never be logged or embedded in error details. The transport delivers the
  current token once in `world.token`; that message must not reach logs,
  traces, or crash reports. Unknown, expired, or replayed tokens fail with
  `SessionExpired`; a still-connected session fails with `Conflict`.
- The client's acknowledged tick is clamped to the authoritative tick.
- Admission is token-bucket limited per peer IP for upgrade handshakes, failed
  credentials, and connection churn. The limiter has a fixed maximum identity
  map and evicts idle entries; it uses the TCP peer address, not a forwarding
  header supplied by an untrusted client.
- After successful admission, total and message-family token buckets are keyed
  by the server-issued session ID. `action.request`, attack actions, chat,
  movement, calendar, and UI control each have separate budgets. A resumed
  session keeps its bucket through its resume window, so reconnecting cannot
  reset a burst. Rate errors use the stable `rate_limited` code and include a
  safe `retry_after_ms` parameter; they contain no bearer or resume secret.
- The `/metrics` endpoint exposes bounded rejection counters grouped by message
  family and stable error code for alerting. It never uses an attacker-provided
  kind as a metric label.
- Expired resume windows are swept on connect, resume, and every tick. A
  missing actor invalidates the session instead of resurrecting a stale
  character.
- Admin authentication is separate from player authentication and uses its own
  role model. Optional MFA or identity-provider integration is required before
  production admin exposure.
- Admin sessions are short-lived and revocable. Every admin operation is
  authorized server-side, regardless of what the UI shows.

## Admin roles (initial)

| Role | Capabilities |
|---|---|
| `observer` | read-only inspection, live maps, metrics |
| `moderator` | mute, kick, quarantine, report resolution |
| `operator` | zone restart, safe content publish, rollback |
| `engineer` | diagnostics, feature flags, configuration |
| `owner` | role management, break-glass, destructive actions |

Destructive or economy-affecting actions require a stronger role, explicit
confirmation, a captured reason, and an audit record. Read-only is the default
mode for every admin client.

## Audit log

- Append-only. Ordinary admins cannot update or delete entries.
- Records actor, role, action, target, before/after summary, reason,
  correlation ID, timestamp, and source address.
- Secrets, tokens, and unnecessary private chat data are redacted before write.
- Writes happen in the same transaction as the state change, or the change is
  rejected.

## Content and user-generated content

- User content is data, not code. It is schema-validated, size-bounded, and
  scanned for disallowed asset types before publishing.
- Ownership, visibility, moderation status, and revision history are required
  metadata.
- Publishing is explicit and reversible. Revocation removes content from new
  sessions without deleting the audit trail.
- Placement and property changes are validated for bounds, collision, budget,
  ownership, and zone rules.

## AI and MCP tools

- MCP support is opt-in and disabled by default.
- Tools are workspace-scoped and least-privilege. File writes are confined to
  the project; shell execution uses an allowlist or explicit approval.
- Destructive, privileged, publishing, and production actions require approval
  and display the exact command and diff.
- Tool outputs are untrusted input: validate paths, reject traversal, cap
  output size, and never execute returned shell text automatically.
- Credentials are never exposed to tools or written to logs. Audit AI-initiated
  changes with the tool name, inputs, and outcome.

## Abuse prevention

- Rate limits per session, per IP, and per action type.
- Payload size, nesting depth, and string length caps.
- Chat and content reporting, blocking, muting, and moderation queues.
- Anti-duplication checks for trades, mail, and reward claims.
- Backups are verified by restore drills, not by job success alone.
- Dependencies are audited and updated; build artifacts are reproducible where
  practical.

## Reporting a vulnerability

See the interim [security reporting guidance](../SECURITY.md). A verified
private contact and response policy remain undocumented; see
[AUTH-001](issues/AUTH-SecurityDocumentationGap.md).
