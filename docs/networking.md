# Networking

## Protocol versioning

The current server emits protocol **1.0.0** and accepts major **1**, any
non-negative minor and patch. This is a parser compatibility promise, not a
claim that future, unimplemented features work. No support window for a future
major has been established; release owners must publish one before shipping it.
This range is enforced by major equality, not by a bounded lag window: older
and newer minors are admitted alike. Semantic-version prerelease and build
metadata are not compared by negotiation; production releases should use stable
versions and validate interoperability rather than rely on metadata rejection.
The handshake carries independent protocol, engine and content semantic
versions, locale (default `pt-BR`), and a set of named features (not a bitmap).

| Client against 1.0 server | Outcome |
|---|---|
| 1.0.x | accepted; patch is informational |
| 1.N.x, N > 0 | accepted with a minor warning if the handshake is readable; only shared known features are negotiated |
| other protocol major | `version_mismatch` before session creation |
| different content major | `version_mismatch` before session creation |
| same content major, different minor | accepted with warning; content patch is informational |

The negotiated protocol and content values are the **server's**, not the
minimum of both peers. Engine major differences warn only. Features are the
intersection of known advertised names; `degraded` indicates client-requested
known features absent on the server. Unknown feature names currently fail
deserialization rather than degrading gracefully. Negotiation does not itself
gate all message handlers or replication; do not assume a feature flag enforces
security or disables a wire shape. Authority remains server-side. Warnings are
diagnostic English strings, not localized notices. Mismatch errors carry stable
`version_mismatch` plus `kind`, `client`, and `server` parameters for client
localization; envelope-major errors have `received` and `supported_major`.
Content versions are independently compared, but the handshake does not verify
bundle hashes or prove the client actually loaded the advertised bundle.

### Lifecycle and rollback

Record every wire change in the protocol changelog below, with compatibility,
feature gating and migration notes. Under ADR 0006, optional additions require
a minor bump; removals, renames, retypes, changed authority semantics, or any
addition rejected by an older strict reader require a major bump unless a
separately negotiated, backward-compatible representation is provided. Never
reuse a feature name or change its meaning. Advertise a feature only when its
implementation and safe fallback exist; clients must not send gated messages
without mutual support. Strict request bodies cannot gain fields compatibly
without explicit versioned handling. Unknown envelope fields and fields in
non-strict response structures are currently ignored; this is not permission
to ignore unknown authority-bearing input. Request parameter maps are defined
by their action validators, not arbitrary authority claims.

Before removing support, announce the replacement and a dated minimum support
window in release notes, provide migration guidance and telemetry, and retain
the old path throughout that window. A concrete duration is not yet decided.
Until such a window is published, no client-lag duration is promised; the
current implementation admits all same-major minors but does not guarantee that
an unknown future capability can be used. Introducing a bounded support range
requires implementing and testing it before announcing enforcement.
For rollback, deploy an older server only after confirming client messages and
content remain compatible with it; disable new features first, retain matching
content bundles, and test admission and resume. A major rollback rejects clients
of the other major; provide a matching client or restore the newer server.
In-memory resume sessions do not survive a server restart. Never roll back
persisted state or bypass server authority merely to regain compatibility.

### Protocol changelog

- **1.0.0** — Existing JSON envelope and handshake baseline, independent content
  major check, named feature intersection, and current world message kinds.
  Nested calendar action unknown fields are rejected as an authority-boundary
  validation correction; no new wire fields or features are introduced.

The `engine_net` golden tests pin the hello and version-error envelopes and the
negotiated parameters payload. They do not yet cover every world message kind
or automatically detect every breaking change. A workspace test checks that the
version constants agree and that the current version has a changelog entry; it
does not compare changes against the merge base or require a bump when wire code
changes. Reviewers must classify wire changes and require the appropriate bump,
entry, and compatibility tests. Retain old fixtures when introducing new
versions; do not update a fixture in place to conceal an incompatible change.

## Envelope

Every message is a JSON object:

```json
{
  "v": "1.0.0",
  "id": "req_01J8Z...",
  "kind": "action.request",
  "body": { },
  "correlation": "corr_01J8Z..."
}
```

- `id` is generated by the sender and used for deduplication and replies.
- `correlation` groups a request with its effects and audit records.
- Unknown `kind` values return a structured error; they never panic or close
  the connection silently.
- Message size, nesting depth, and string lengths are capped. Oversized or
  malformed messages increment a counter and are rejected.

## Framing

`engine_net::framing` puts an envelope on a byte stream:

```text
┌────────────────────┬──────────────────────────────┐
│ length (4 bytes BE)│ compact JSON envelope        │
└────────────────────┴──────────────────────────────┘
```

- The payload cap is the same 64 KiB cap as envelope validation.
- Zero-length, oversized, truncated, non-UTF-8, and non-JSON frames are
  rejected. A partial frame is an error, never a clean disconnect.
- A clean end of stream is allowed only before any header byte is read.
- WebSocket transports use the same codec inside binary frames. Text frames
  carrying a bare envelope are accepted for inspection tools; the canonical
  wire form is binary and length-prefixed.

## Admission, rate limits, and slow clients

### World-edge identity admission

`lynx-world serve --auth-mode nakama` validates `Authorization: Bearer` with
Nakama and resolves the resulting trusted account against the Lynx PostgreSQL
character store before the request upgrades. A fresh production admission must
also carry exactly one `X-Lynx-Character: chr_…` header. That header selects a
character; it grants no authority. The server checks current active ownership
and constructs non-staff `SessionClaims` itself. Invalid, expired, revoked, or
unknown sessions receive HTTP 401 `invalid_session`; missing, malformed, or
foreign selections receive HTTP 403 `character_forbidden`; an identity-service
failure receives HTTP 503 `identity_unavailable`. Responses, logs, and metrics
never include the bearer credential or a backend failure detail. A denied valid
character selection creates the structured `world_admission_denied` audit log
event with only its stable reason and selected character ID.

The production adapter validates Nakama on every **fresh** bearer admission;
the client refreshes an expired Nakama credential through the normal Nakama
session-refresh flow, then opens a fresh world connection. Gateway-issued
resume tokens are independent, opaque, and rotate on use. A resume does not
contact Nakama, so revocation or expiry is observed no later than the configured
resume TTL (30 seconds by default) or the next fresh admission. No client clock
or token claim supplies account, character, staff, or expiry authority.

Production admission verifies the selected character against Nakama/PostgreSQL,
then provisions a missing player actor in the authoritative zone. Its entity ID
is deterministically derived from the character ID. Before the gateway creates
a session, the service persists a world snapshot containing that actor; admission
fails closed if durable snapshot storage is unavailable or the write fails. A
retry therefore reuses the same actor, and a restart restores it from the
snapshot. Actor placement uses an unblocked, unoccupied navigation cell. The
demo loader is not used for production characters; multi-zone routing remains
separate work.

`lynx-world` applies bounded token buckets before work reaches the authority
loop:

- Per TCP peer IP: 20 upgrade handshakes bursting at 10/s, five failed
  credentials at 1/s, and 10 valid connection/resume churn attempts at 2/s.
  Peer IP is taken from the listener, not client forwarding headers. The
  identity map is capped at 16,384 idle entries and entries expire after five
  minutes.
- Per authenticated server-issued session: 100 total messages at 100/s; action
  20/s; attack 8/s; chat 5/s; movement 30/s; calendar 5/s; and UI 10/s. Each
  family has its own burst capacity and also consumes the total budget. Attack
  consumes both its action-family and action-type budget. Session buckets stay
  through the configured resume window.
- `--message-burst` and `--message-rate` tune only the total authenticated
  bucket (both must be 1–10,000). Per-family and admission limits deliberately
  remain bounded server defaults; a broad tuning knob cannot disable abuse
  protection. `/metrics` returns rejection counts by normalized family and
  stable error code.

The TCP listener caps accepted HTTP connections at 1,024 before parsing a
request; when full, the operating-system listener backlog applies backpressure
instead of allocating another file descriptor for a slow-header peer. Before
upgrade, the world route independently limits pending/live WebSockets to 1,024
globally and 16 per peer IP, and rejects aggregate request headers larger than
16 KiB with HTTP 431 (`request_headers_too_large`). These checks run before
session admission. Capacity failures return HTTP 503 with `total_capacity` or
`ip_capacity`; once draining starts, new attempts receive HTTP 503 with
`draining`. Capacity and draining counters use fixed reason labels in
`/metrics`. A request without `Origin` remains permitted for native clients;
configured browser origins must match exactly.

A rate rejection returns HTTP 429 before an upgrade or a `protocol.error` with
`code: "rate_limited"` after admission, then closes the WebSocket. It includes
`retry_after_ms`; it never includes a credential. Each connection's outbound
queue is capped at 64 frames. A full queue detaches that slow client with the
`slow_client_outbound_queue_full` reason rather than blocking the authoritative
tick; a resume uses a fresh snapshot. A connection closes after 120 seconds
without an incoming WebSocket message. Socket writes and waits to enqueue an
inbound frame to the authority are each bounded to five seconds; a timed-out
authority enqueue closes that connection. Draining rejects new upgrades while
existing connections close through normal service shutdown or these transport
limits.

## World service wire messages

`world_server` owns the connection-oriented message kinds:

| Kind | Direction | Body |
|---|---|---|
| `session.hello` | client → server | client handshake and last acknowledged tick |
| `world.ready` | server → client | session ID, actor, resumed flag, negotiated protocol |
| `world.snapshot` | server → client | tick, full interest view delta, filtered replication |
| `chat.channels` | client → server | empty object; authenticated discovery currently returns `chat.channels.result` with an empty `channels` array |
| `chat.send`, `chat.history` | client → server | unavailable; rejected without storing or echoing player content |
| `world.delta` | server → client | tick, interest delta, filtered replication |
| `movement.request` / `movement.queued` | client → server / server → client | endpoint intent and queue admission (not processing) |
| `world.token` | server → client | the session's current resume secret |
| `action.request` / `action.result` | both | gameplay intent and its outcome |
| `calendar.command` / `calendar.result` | both | world calendar intent (enroll; staff lifecycle) and its correlated outcome |
| `calendar.query` / `calendar.state` | both | read request and the player-visible calendar (drafts hidden) |
| `ui.action` | client → server | schema, panel action name, parameters, optional client tick |
| `ui.close` | client → server | empty body |
| `ui.state` | server → client | authoritative tick plus the open panel (or `null`) |
| `protocol.error` | server → client | stable code, parameters, correlation ID |

`world.snapshot` and `world.delta` share the `WorldUpdate` body: authoritative
tick, `InterestUpdate`, and an audience-filtered `ReplicationBatch`. The
recipient is implied by the connection, so the body never repeats a session ID.

`world.token` is a control message, not replication. It is sent once after a
successful open or resume and carries the rotated single-use secret. Transports
must keep it out of logs, traces, and error details.

### Attack requests

`action.request` with `verb: "attack"` names a target but accepts no parameters;
client-supplied damage is rejected. The server derives physical damage from
versioned zone combat rules and server-owned attacker/target combat stats after
normal action validation. Attacks against dead targets are rejected and do not
emit another death event. See [security §Combat authority](security.md#combat-authority).

### UI panel lifecycle

Entity-provided interfaces are server state, not a client-owned overlay.

- `action.request` may produce an `ActionEffect::UiOpened` effect. The gateway
  records that panel for the session from the authoritative effect: the target
  entity, the schema, and the title. It then sends `ui.state` so the client has
  the canonical panel state in addition to the action result.
- `ui.state` has the body `{ "tick": <tick>, "panel": <panel or null> }`, where
  a panel is `{ "target": <entity>, "schema": <definition id>, "title": <text> }`.
  A `null` panel tells the client to close whatever it is showing.
- `ui.action` has the body
  `{ "schema": <definition id>, "action": <name>, "params": { ... }, "client_tick": <tick> }`.
  The client cannot name a target, actor, principal, or session. The gateway
  looks up the session's open panel and uses its server-resolved target. The
  `schema` is an optimistic concurrency check: acting on a replaced panel
  returns `Conflict`.
- The action name is parsed into an interaction verb and routed through the
  normal action pipeline. Range, visibility, permission, target state,
  capabilities, and cooldowns are re-validated. A client that moves away from
  the provider cannot keep acting on it.
- `ui.close` has an empty body. Closing an already closed panel is idempotent
  and still returns `ui.state`.
- A disconnect clears the session's panel; a resume starts with no panel.
  A panel action whose provider is gone clears the panel instead of leaving a
  stale interface open.

Panel targets, schemas, and titles are never accepted from a client. See
[ADR 0015](adr/0015-ui-panel-session-state.md).

### Server-validated minigames

The reference minigame is a combination lock attached to an entity-provided
interface. It runs inside `engine_sim::minigame`; the gateway only routes panel
actions.

- `use` or `activate` on a locked provider with a minigame opens the panel and
  returns `UiOpened` plus `MinigameStarted`, carrying the definition, length,
  remaining attempts, solved flag, and any lockout tick.
- The combination is derived from a server-held seed and the authenticated
  principal. Neither the seed nor the combination is replicated, and a client
  cannot choose its own code. The seed must come from server entropy, not from a
  content bundle the client also downloads.
- Guesses arrive as `ui.action` with `action: "play"` and `params.guess`. A
  direct `action.request` with `verb: "play"` is rejected with `Unsupported`;
  minigame input must use the panel channel so the server resolves the provider
  and re-validates range, visibility, and permission.
- Each accepted guess returns `MinigameAttempt` with a per-digit
  `exact`/`higher`/`lower` hint and the remaining attempts. Malformed guesses are
  rejected without consuming an attempt.
- Solving emits `MinigameSolved`, unlocks the provider's door with
  `DoorUnlocked`, and closes the panel with `UiClosed`. Exhausting the budget
  emits `MinigameFailed` and sets a server-tick lockout; further guesses return
  `Cooldown` until it elapses.
- Attempt budgets, progress, and lockouts are per principal, so one player
  cannot consume another player's attempts. Minigame progress is server-only and
  is absent from interest and owner snapshots.

See [ADR 0016](adr/0016-server-validated-minigame.md).

See [ADR 0014](adr/0014-framed-transport-and-world-server.md).

## Replication categories

| Category | Examples | Delivery |
|---|---|---|
| Reliable events | inventory, trade, chat, ownership, quests | ordered, acknowledged |
| State snapshots | position, rotation, velocity | unreliable, latest-wins |
| Interest-managed | nearby entities, zone objects | filtered by zone/distance/visibility |
| Owner-only | private inventory, control leases | authorized recipient only |
| Property-scoped | placed objects, housing details | access policy |
| Admin | diagnostics, AI state | staff roles only |

The server never broadcasts a whole zone to every client. Interest management
starts with zone + distance + explicit visibility, and gains spatial indexes
when measurements require them.

## Interest management

`engine_net::interest` computes one view per session and filters each
replication batch for its recipient. The rules are deliberately boring and
deterministic:

- An entity **enters** a view at the observer's horizontal view radius
  (default 64 m) and **leaves** at that radius plus the exit margin (default
  12.5%). The gap prevents appear/disappear churn at the boundary.
- Visibility is checked before distance: `public`, `hidden` (owner or staff),
  `owner_only`, and `staff_only`.
- At most 512 entities are retained per observer by default. The nearest win;
  ties break on stable entity ID.
- Session-directed updates name their `session`. An update without one is
  dropped, never broadcast.
- Owner-only updates require the audience's principal to own the entity.
  Property-scoped updates require ownership or an explicit access entry.
  Admin updates require a staff session.
- Missing entities or principals fail closed.
- Filtering also collapses superseded state snapshots, so a position is not
  resent twice in one batch.

The world gateway supplies the observer position, principal, and staff flag.
Clients cannot request a larger radius or choose their own audience. See
[ADR 0012](adr/0012-interest-management-and-replication-audiences.md).

## World gateway sessions

`world_gateway` is the transport-agnostic boundary between an authenticated
connection and the authoritative zone. It adds no socket and no runtime.

- **Connect.** Trusted transport code authenticates the player, then calls
  `WorldGateway::connect` with `SessionClaims` (account, character principal,
  staff flag) and the client handshake. The server issues the session ID and
  the resume token. The server's configured concurrent-session policy is
  either per character or per account; a duplicate under that policy returns
   `Conflict` and does not replace the existing session, including while the
   existing session is detached within its resume window. Per-character policy
  allows separate clients to connect under one account using different
  characters; per-account policy allows only one active character session for
   that account. `--session-policy per-character` is the default;
   `--session-policy per-account` enables account-wide uniqueness. An incompatible handshake
  creates no session.
- **Action bodies are intent only.** `action.request` accepts `verb`, optional
  `target`, optional `params`, and optional `client_tick`. Unknown fields —
  including `actor`, `principal`, or `session` — are rejected with
  `InvalidRequest` before the simulation runs. Actor and principal come from
  the session.
- **Session-scoped idempotency.** The gateway prefixes the client request ID
  with the session ID before calling the simulator, then rewrites
  `ActionResult::request_id` to the client's ID. Retries within a session are
  idempotent; the same ID in another session cannot reuse the cached result.
- **Calendar bodies are intent only.** `calendar.command` accepts a caller
  `request_id` and one `CalendarAction`. Unknown fields — including `actor`,
  `principal`, or `staff` — are rejected, and those values come from the
  session. Enrollment is open to any connected character; lifecycle
  transitions are staff-gated inside the simulation stage. A repeated request
  ID returns the cached outcome and never re-applies or re-persists the
  command.
- **Acknowledgements.** `session.ack` records the highest server tick the
  client has processed. The value is clamped to the authoritative tick, so a
  client cannot pin the server forward or replay a stale cursor.
- **Panels are session state.** When a validated action opens an
  entity-provided interface, the gateway stores the server-resolved target,
  schema, and title for that session. `ui.action` and `ui.close` operate on
  that stored panel; a client cannot redirect them to another entity. Panel
  state is cleared on disconnect and when a panel action finds its provider
  gone. See
  [ADR 0015](adr/0015-ui-panel-session-state.md).
- **Errors stay correlated.** Unknown kinds and malformed bodies produce a
  `protocol.error` reply with `correlation` set to the request ID. They do not
  close the session. Gameplay rejections arrive as `action.result`.
- **Replication.** `WorldGateway::tick` advances the zone and returns one
  `SessionDelivery` per connected session, containing that session's interest
  delta plus only the replication updates its principal and staff flag admit.
  Empty deliveries are omitted. There is no whole-zone broadcast path.

See [ADR 0013](adr/0013-world-gateway-and-reconnect.md).

## Authority rules

- Clients send intent (`action.request`), not state patches. The body names a
  verb, target, and parameters; it cannot name the acting entity.
- Panel actions (`ui.action`) carry an action name and parameters, never a
  target. The server resolves the provider from the session's open panel and
  re-validates range and permission before running the action.
- Minigame outcomes are computed on the server. The client submits a guess, not
  a result; hints, attempt counts, lockouts, and unlocks are server effects.
- Queued movement is checked against a server-owned speed budget, nav-grid
  transitions, zone bounds, destination-cell occupancy, circular body
  collisions, and a server-owned mobile/stunned status. A rejected endpoint
  never updates the authoritative transform; see
  [SIM-001](TODO/10-simulation-gameplay.md#sim-001--server-authoritative-movement).
- Combat, inventory, currency, progression, and placement outcomes are
  computed on the server.
- Client raycasts are selection hints. The server re-checks range, visibility,
  and permission before applying an action.
- Client clocks are never trusted for cooldowns or rewards; the server uses its
  tick and a monotonic session clock.
- Player-facing failures return a stable `ErrorCode` plus typed parameters, not
  a pre-localized sentence. The client renders `error.<code>` from its catalog;
  the default source catalog is Brazilian Portuguese.

## Localization on the wire

- The server replicates definition IDs, message keys, and parameters. It does
  not broadcast translated strings for ordinary UI.
- System chat notices, mail, and offline events store a key plus JSON
  parameters and render in the reader's locale.
- Names chosen by players are transmitted verbatim and displayed without
  translation.
- Locale is part of the session, not of each message. Changing it requires a
  `SetLocale` request or a new session and takes effect on the next frame.
- Missing keys render as the key and are logged once; they never close a
  connection. See [`internationalization.md`](internationalization.md).

## Prediction and reconciliation

- Local prediction is optional per action and limited to movement and
  presentation.
- Movement requests contain only `sequence` (u64) and `destination` (`x`, `y`,
  `z` in meters). The server resolves actor and session, permits one pending
  input per actor and at most the tick action budget in total, and validates
  using a fixed server interval and provisional 2 m/s speed, the actor's
  server-owned status and body, nav-grid transitions, zone bounds, and
  destination-cell occupancy. A correlated `movement.queued` means admission
  only; malformed, stale, unauthorized or over-budget requests return
  `protocol.error` and do not consume the sequence.
- `world.snapshot` includes a private `movement` baseline with `tick`,
  authoritative `position`, `last_consumed_sequence` (number or null), and
  `applied: null`. A `world.delta` includes this field for a processed input,
  with its tick, resulting position, sequence and `applied: true` or `false`.
  Rejections do not explain geometry. This field goes only to the exact control
  session, never to other observers or staff; ordinary public position updates
  still use interest filtering. No movement field is sent on unrelated deltas.
- Every queue-accepted sequence is consumed, even if processing rejects it or a
  disconnect cancels it. Resume resets prediction from the baseline, including
  cancelled input. Sequences must strictly increase within a session, may start
  at zero, and never wrap: after `u64::MAX`, a new session is required. New
  sessions and zone restarts have independent, non-persistent sequence state.
  Sequence values are JSON integers; clients must preserve the full u64 range
  rather than round them through an IEEE-754 number.
- The movement field is bounded to one result per session per tick. Batch and
  envelope limits still apply. An unsendable delta detaches that connection; a
  tick construction failure detaches affected sessions. Resume provides a new
  baseline rather than replaying individual results. The transport must detach
  on outbound backpressure. This is not an acknowledged reliable event stream.
- On a processing rejection, a client should correct from the supplied position;
  admission errors are structured protocol errors. No reward is ever granted
  from predicted state.
- The reference client retains one in-flight endpoint because the server admits
  one pending input per actor. That one-entry replay buffer is bounded under
  latency or loss: a client must not retry an ambiguously delivered endpoint.
  It keeps authoritative state separate from a presentation position. A private
  result replaces authoritative state; visual error at or below the configured
  prediction distance is interpolated toward that result, while a larger error
  snaps immediately. This local threshold is not a server speed, collision, or
  teleport rule.
- Teleport, death, zone transfer, disconnect, and any new-session baseline
  discard the pending endpoint and snap presentation to the new private
  snapshot. A resumed session preserves the highest local/server sequence
  watermark; a new control session begins a new sequence domain. Messages from
  an older connection generation are discarded before reconciliation.
- Remote entities interpolate; the client never extrapolates authoritative
  outcomes such as damage or loot.
- Velocity is not currently transmitted or used by the reference client. It
  must remain server-derived if a future protocol version adds it.

## Reconnect and recovery

- A disconnect detaches the transport but keeps the character in the zone and
  opens a server-configured resume window (default 30 s). The window is counted
  against the session cap.
- Sessions carry a server-issued resume token. Tokens are opaque, redacted from
  logs and debug output, single-use, and rotated on every successful resume; a
  captured token cannot be replayed.
- On reconnect the client presents the token, its handshake, and its last
  acknowledged server tick. The server clamps the tick to its authoritative
  value, negotiates the handshake again, and replaces the session's handshake
  record.
- The reply is always a full owner snapshot (inventories and owner-scoped state)
  plus a fresh interest snapshot. Subsequent ticks send deltas. There is no
  client-chosen "since" cursor.
- An unknown, expired, replayed, or already-connected token returns
  `ErrorCode::SessionExpired` (or `Conflict` for the connected case) and no
  session state is handed out. Expired windows are swept on connect, resume,
  and every tick.
- Duplicate action IDs after a retry return the original result instead of
  applying the action twice. IDs are scoped to the session that sent them.
- Sessions live in memory today. A zone restart invalidates resume windows;
  durable session storage is future work.

## Transport

`lynx-world` is the reference transport adapter and binary:

- `GET /healthz` reports process liveness. `GET /readyz` reports readiness and
  the number of configured characters; readiness turns false while draining.
- `GET /world` requires an HTTP upgrade and `Authorization: Bearer <token>`.
  The token is either a configured static token or a gateway-issued resume
  token. Missing credentials are rejected with `401` before the upgrade.
- After the upgrade, the first frame must be `session.hello`. The body carries
  the client `Handshake` and an optional `last_ack_tick`; the server supplies
  session identity. A handshake that does not arrive within ten seconds closes
  the connection with a correlated `protocol.error`.
- Messages travel as binary WebSocket frames containing one length-prefixed
  envelope. The transport caps frame size to the envelope limit.
- One authority loop owns the world. Transport tasks send admission, frames,
  and disconnects over a bounded channel; the loop owns the clock, the tick
  interval, and per-connection outbound queues. A client that cannot drain its
  queue is detached rather than stalling the tick.
- Disconnects keep the session resumable. The transport reports the last
  acknowledged tick from the session, calls `WorldGateway::disconnect`, and
  closes the socket. Reconnecting with the resume token produces a fresh
  `world.ready`, a full owner plus interest snapshot, and a rotated
  `world.token`.
- Immediate revocation uses `WorldGateway::close_session`, which removes the
  session and its interest set without a resume window.
- Static tokens are hashed with SHA-256 in configuration; plaintext tokens are
  never stored by the service. This is a development-only mechanism, not
  production authentication. The service warns at startup and rejects
  non-loopback binds unless `--dangerous-allow-non-loopback-demo-auth` is
  explicitly supplied for controlled development. This opt-in does not secure
  credentials or provide account ownership, expiry, or revocation. A loopback
  listener can still be exposed through a proxy or tunnel; operators must
  control access to those too. Production authentication must validate a real
  session and look up character ownership before opening a gateway session.
- The binary carries no TLS. Terminate TLS at a reverse proxy or equivalent
  transport, and keep the listener on loopback while developing. The repository
  reference is [`deploy/nginx/lynx-world.conf`](../deploy/nginx/lynx-world.conf):
  it upgrades only `/world`, passes the bearer `Authorization` and character
  selection headers, bounds header handling, and sets proxy timeouts above the
  server's 120-second idle timeout. Replace the example hostname and certificate
  paths and validate the configuration with `nginx -t` before deployment.

### TLS termination and proxy trust boundary

The reference topology is client HTTPS/WSS → Nginx → `lynx-world` over loopback
HTTP. Bind the world listener to `127.0.0.1` (or an equivalently isolated private
interface) so untrusted clients cannot bypass TLS termination. Nginx must be
the only reachable proxy to that listener. The proxy passes `Authorization`
unchanged because it is the authentication credential; do not log it. TLS
certificates and renewal are owned by the deployment, not this service.

`lynx-world` does not currently read `Forwarded`, `X-Forwarded-For`, or
`X-Real-IP`. Rate limits and audit peer addresses therefore use the TCP peer
address observed by the service; behind a proxy, this is the proxy address, not
the browser. The reference config strips common forwarding headers and does not
attempt to assert client IP. Do not trust client-supplied forwarding headers or
use them for access control. If client-IP propagation is added later, it must
be explicitly enabled with a configured trusted-proxy set and tested to ignore
those headers from all other peers.

The sample provides TLS but no HTTP listener, redirect, HSTS, or cookies. If a
deployment adds a public HTTP listener, redirect only at its controlled edge
and emit HSTS only on HTTPS after confirming the domain policy (including
subdomains/preload implications). World authentication uses bearer headers,
not cookies; clients must protect bearer and resume tokens as secrets and avoid
placing them in URLs, logs, or browser-persistent storage. Native TLS in
`lynx-world` is not implemented; non-proxy deployments must provide an
equivalent TLS transport boundary rather than expose plaintext publicly.

Raw UDP with a reliable layer remains a possible future transport. Gameplay
messages do not change: only the framing and socket adapter do. The WebSocket
transport enforces a per-connection fixed-window budget on binary and text
messages after admission (default 100 per second); exceeding it ends the
connection with `BudgetExhausted` and a retry hint. This is not a per-account
or gameplay-specific abuse limit, and the initial handshake is handled separately.
