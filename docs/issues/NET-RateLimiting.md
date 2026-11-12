# NET-002 — Rate limiting and per-session message budgets

- **Status:** Resolved — implemented by NET-002; this file records the original defect
- **Reported:** 2026-10-28
- **Resolved:** 2026-11-12
- **Related tasks:** [NET-002](../TODO/30-networking-security.md#net-002--rate-limiting-and-message-budgets),
  [OPS-001](../TODO/60-operations-release.md#ops-001--metrics-endpoint)
- **Affected workflow:** `crates/engine_net`, `crates/world_gateway`

## Resolution

The transport now enforces bounded per-IP admission buckets for handshakes,
failed authentication, resume attempts, and connection churn. Authenticated
sessions have token buckets for total messages and the action, attack, chat,
movement, calendar, and UI families. Rejections carry stable codes and retry
delays; WebSocket rejections close the connection, while pre-upgrade limits
return HTTP 429. Fixed-label counters expose rejection counts without session
IDs or bearer values. Per-connection outbound queues are bounded and slow
clients are detached. The implementation and current acceptance criteria are
tracked in [NET-002](../TODO/30-networking-security.md#net-002--rate-limiting-and-message-budgets).

The world gateway and engine net layer must enforce:
- Per-session message count limits with configurable windows (e.g., 100 messages per second)
- Per-IP rate limiting for unauthenticated connections
- Per-action-type budgets to prevent specific abuse patterns (login spam, inventory queries, etc.)
- Payload size, nesting depth, and string length caps at the framing layer

## Defect

The original report identified a connection-only budget. It left these risks:
- Connection exhaustion attacks
- Resource denial through excessive requests
- Protocol frame flooding that could impact the authority loop

## Impact

The original connection-only budget could be bypassed by reconnecting. NET-002
now covers admission, authenticated message families, and slow-client queues.
Backend storage adapters remain outside the protocol transport boundary.

## Acceptance criteria

- [x] Per-session message budgets by bounded message family with refill windows
- [x] Per-IP rate limits for unauthenticated admission and resume attempts
- [x] Payload size caps enforced at the framing layer (not just JSON parsing)
- [x] Rate-limit errors include retry information before and after upgrade
- [x] Diagnostics count rejections without recording credentials or session IDs
- [x] Integration coverage exercises burst limits, session isolation, reconnect, and admission

## Notes

The implementation uses bounded token buckets in `lynx_world` and keeps
`engine_net` protocol framing transport-agnostic. See NET-002 for current
configuration, metrics, and test coverage.
