# CLT-005 — Network client loop

**Status: Done.** The backlog records this outcome: background reconnect, Nakama access-token refresh, and terminal recovery are implemented and tested. All acceptance criteria below are checked complete.

## Original task scope

- Connect, perform the handshake, read/write framed envelopes on a background
  task, and feed snapshots/deltas into the presentation layer.
- Track server tick acknowledgements and send them on resume.
- Reconnect with exponential backoff and the rotated resume token; distinguish
  resumable from terminal failures.
- Bound queues and surface telemetry (RTT, loss, queue depth) in a debug overlay.
- Never block the render thread on network I/O.

## Acceptance evidence

- [x] The client applies a full snapshot and subsequent deltas correctly.
- [x] A dropped connection reconnects and resumes within the server window.
- [x] Terminal errors (expired/revoked/conflict) show a clear path forward.
- [x] A stalled server does not freeze the client.
- [x] Debug telemetry exposes tick, movement round-trip time, and queue depth.

**Progress:** The online loop stores rotated resume tokens in memory, retries
transient failures with bounded exponential backoff, and captures the Tokio
runtime handle for retries from Bevy systems. Two consecutive resumes and
post-resume movement pass against an in-process server. The HUD reports queue
depths and movement round-trip time (including authority processing), not a
network-only RTT or measured packet loss. An in-process integration test verifies
rotated-token resume and movement after resume. A Windows local demo session was
also visually confirmed active, with tick 807, six entities,
accepted movement, zero reconnects, and empty transport queues (developer-provided
screenshot, 2026-11-04). This is static-token demo coverage, not Nakama account
login. Terminal failures show a category-specific explanation and recovery step
in the online HUD. A rejected Nakama access token first triggers one background
refresh; if its refresh credential is expired or revoked, the user is directed to
sign in again (`R` retries). A session conflict advises waiting for the other
session before retry, and a forbidden character directs the user back to character
selection. Other failures advise checking connectivity and pressing `R`. Unit
tests cover the recovery guidance. An in-process TCP proxy now injects a real
transport drop: the client notices it without an explicit
`reconnect()`, enters the automatic backoff path, resumes with the rotated
token, and keeps moving from the same authoritative baseline. Account refresh
now runs on a worker after the world edge rejects a saved Nakama access token.
The rotated refresh token is persisted to the OS credential store before the
client retries admission; expired/revoked refresh credentials retain an
actionable sign-in recovery path. A mock-edge integration test verifies the
refreshed bearer is used on retry without blocking the Bevy polling loop.

## Source

See [`40-bevy-client.md`](../40-bevy-client.md) for the full task history and implementation notes.
