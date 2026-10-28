# NET-002 — Rate limiting and per-session message budgets

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [NET-002](../TODO/30-networking-security.md#net-002--rate-limiting-and-message-budgets),
  [OPS-001](../TODO/60-operations-release.md#ops-001--metrics-endpoint)
- **Affected workflow:** `crates/engine_net`, `crates/world_gateway`

## Resolution

Partial mitigation: the WebSocket transport enforces a post-admission, per-connection fixed-window budget before decoding text or binary messages and before forwarding them to the authority loop. The default is 100 messages per second; embedders can configure 1–10,000 messages per window of 1 ms–60 seconds via `MessageBudgetConfig`. A rejected message produces `protocol.error` with code `budget_exhausted` and a `retry_after_ms` parameter, then closes the connection. A warning records the code without message contents or credentials.

This is not a per-session or per-IP limit: reconnecting or resuming creates a fresh budget, and fixed windows permit bursts across window boundaries. Ping/pong frames, the handshake, and HTTP admission are outside this budget. There are no per-action budgets, rejection counters, or HTTP retry-after headers. Rate limiting remains incomplete and this issue stays open.

The world gateway and engine net layer must enforce:
- Per-session message count limits with configurable windows (e.g., 100 messages per second)
- Per-IP rate limiting for unauthenticated connections
- Per-action-type budgets to prevent specific abuse patterns (login spam, inventory queries, etc.)
- Payload size, nesting depth, and string length caps at the framing layer

## Defect

The WebSocket connection budget is a partial mitigation; there is still no persistent per-session or per-IP throttling. This leaves the server vulnerable to:
- Connection exhaustion attacks
- Resource denial through excessive requests
- Protocol frame flooding that could impact the authority loop

## Impact

- Protection against network-level abuse is incomplete and can be bypassed by reconnecting
- The security model requires additional rate limits and budgets
- This is a P1 risk: without rate limiting, a compromised or misbehaving client can exhaust server resources
- Affects network admission and message handling at the world edge; backend
  storage adapters are not protocol transports.

## Acceptance criteria

- [ ] Per-session message budget configurable per action type with expiry window
- [ ] Per-IP rate limits for unauthenticated connections (e.g., 10 requests/second)
- [x] Payload size caps enforced at the framing layer (not just JSON parsing)
- [ ] Rate limit exceeded errors returned as `protocol.error` with retry-after headers
- [ ] Diagnostics emitted for rate-limit rejections without leaking sensitive data
- [ ] Integration tests simulate flood conditions and verify enforcement across sessions, reconnects, and admission (a WebSocket test covers exceeding the per-connection budget)

## Notes

This issue is linked to `NET-002` in the TODO backlog. The implementation should be modular so it can be applied consistently across all protocol adapters. Consider using a token-bucket or leaky-bucket algorithm for per-session budgets.
