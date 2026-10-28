# ADR 0014: Framed transport, world server loop, and runnable binary

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

ADR 0013 left `world_gateway` transport-agnostic and tested, but there was no
way to run it: no socket, no process loop, and no defined byte framing. A
transport would have repeated the same connection bookkeeping, wire encoding,
and session-close rules, and each repetition would be a place to get reconnect
or audience filtering wrong.

The first runnable service also needs a way to turn an authenticated client into
the `SessionClaims` the gateway accepts. Nakama integration for character
ownership is not complete, so the increment needs an explicit, disposable
development authentication path rather than a client-chosen identity.

## Decision

- **Length-prefixed framing belongs to `engine_net`.** `engine_net::framing`
  writes a four-byte big-endian length followed by one compact JSON envelope.
  The maximum payload is the existing 64 KiB message cap. Zero-length,
  oversized, truncated, non-UTF-8, and non-JSON frames are rejected. A stream
  ends cleanly only when zero header bytes are read, so a partial frame cannot
  masquerade as a deliberate disconnect.
- **`world_server` owns connection-oriented work.** It allocates server-side
  connection IDs, maps them to gateway sessions, encodes `world.ready`,
  `world.snapshot`, and `world.delta` envelopes, returns correlated action
  replies, and closes connections whose sessions expired. It owns no socket,
  thread, async runtime, or clock. A transport supplies explicit `Millis` and
  trusted claims, exactly as with `world_gateway`.
- **Immediate revocation is separate from disconnect.** `close_session` removes
  a session and its interest set without a resume window. An ordinary transport
  drop still calls `disconnect`, which keeps the character in the zone and the
  token resumable.
- **`lynx-world` is the first transport adapter and binary.** It uses Axum 0.8
  and Tokio for a WebSocket endpoint at `/world`, plus `/healthz` and
  `/readyz`. One authority loop owns `WorldServer`; transport tasks send
  admission, frames, and disconnects over one channel. The loop owns the clock
  and the tick interval.
- **Admission is a two-step handshake.** The HTTP upgrade carries
  `Authorization: Bearer <token>`. After the upgrade, the first WebSocket frame
  must be a `session.hello` envelope containing the client handshake and its
  last acknowledged tick. There is a short handshake deadline.
- **The transport delivers the resume secret once, as a control message.** A
  successful open or resume sends `world.token` with the new resume token. The
  token is not part of a snapshot or replication body, is redacted from
  `Debug`, and must never be logged. The client presents it as the bearer token
  on reconnect; the server rotates it on every successful resume.
- **Development authentication is static and hashed.** Operators configure
  SHA-256 hashes of high-entropy bearer tokens, never plaintext tokens. Each
  hash maps to one deterministic demo account and character. The client cannot
  choose an account, principal, staff flag, session ID, view radius, or token.
  A duplicate login for a character already connected returns `Conflict`.
  This fixed token-to-character mapping is development-only and does not
  provide the multi-character account login flow needed for multiple client
  instances to use different characters under one account.
- **Backpressure is explicit.** Each connection has a bounded outbound queue.
  A client that cannot drain it is detached rather than stalling the authority
  loop. The global inbound event channel applies socket backpressure.
- **No persistence in this increment.** Zone state, sessions, and demo
  characters are in memory and reset on restart. Nakama token validation,
  durable character ownership, rate limiting, metrics, and TLS termination
  remain future work.
- **Graceful shutdown drains rather than saves.** On `Ctrl+C`, readiness turns
  false, the listener stops accepting, the authority loop closes connections,
  and the process exits. Because state is in memory, nothing is persisted.

## Consequences

- All transports share one implementation of wire kinds, session closure, and
  snapshot/delta encoding, so a future UDP or native transport only owns
  socket-specific concerns.
- The gateway remains testable without a runtime, and the server loop is
  testable without a socket. The binary has an integration test that exercises
  the real HTTP upgrade, handshake, replication messages, and correlated
  errors.
- Static hashed tokens are suitable for local play and controlled deployments,
  not for production identity. An attacker who obtains a token hash still needs
  the high-entropy token, but weak tokens remain guessable offline. Production
  must validate a Nakama session and look up character ownership.
- Putting the resume secret on the wire is unavoidable if clients are to
  reconnect. It is isolated in a single control envelope so it is easy to
  exclude from logs, traces, and crash reports; transports must enforce that.
- Plain WebSocket carries no transport security. Production deployment needs
  TLS termination at a reverse proxy or an equivalent transport.
- Without persistence, a process restart still logs every player out. This is
  accepted until the session and snapshot stores land.
- The demo's fixed character mapping means token order is operationally
  significant. Operators must not reorder configured hashes across restarts
  once durable characters exist.
