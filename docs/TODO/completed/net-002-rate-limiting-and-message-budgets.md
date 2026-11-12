# NET-002 — Rate limiting and message budgets

**Status: Done.** All acceptance criteria below are checked complete in the source task.

## Original task scope

- Per-IP admission limits (handshakes, failed auth, connection churn).
- Per-session token buckets for total messages and per-kind budgets (actions,
  chat, movement, calendar, UI).
- Per-action-type limits and duplicate-request collapse where safe.
- Bounded outbound queues; detach slow clients rather than stalling the tick.
- Stable `CapacityExceeded`/`RateLimited` errors, close reasons, and metrics.
- Configuration with safe defaults and documentation; no unbounded knobs.

## Acceptance evidence

- [x] A burst above the configured budget is rejected without affecting other
      sessions.
- [x] Limits apply after authentication and use server-resolved identity, not
      client claims.
- [x] Slow clients are detached with a documented reason before queues grow
      without bound.
- [x] Rejections are counted by kind/error code for alerting.
- [x] Tests cover token-bucket refill, burst, and reconnect behavior.

## Source

See [`30-networking-security.md`](../30-networking-security.md) for the full task history and implementation notes.
