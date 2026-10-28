# NET-002 — Rate limiting and per-session message budgets

- **Status:** Open
- **Reported:** 2026-10-28
- **Resolved:** —
- **Related tasks:** [NET-003](../TODO/30-networking-security.md#net-003--transport-agnostic-gateway-and-world-server), 
  [OPS-007](..//TODO/60-operations-release.md#ops-007--metrics-and-logs)
- **Affected workflow:** `crates/engine_net`, `crates/world_gateway`

## Resolution

Rate limiting and per-session message budgets are documented as a requirement in the security model but are not yet implemented. Without them, clients can exhaust server resources through flood attacks, excessive request rates, or malformed protocol frames.

The world gateway and engine net layer must enforce:
- Per-session message count limits with configurable windows (e.g., 100 messages per second)
- Per-IP rate limiting for unauthenticated connections
- Per-action-type budgets to prevent specific abuse patterns (login spam, inventory queries, etc.)
- Payload size, nesting depth, and string length caps at the framing layer

## Defect

The current implementation has no rate limiting or message budget enforcement. The `engine_net` crate documents that "length-prefixed frames are size-bounded" (line 46 of security.md), but there is no limit on how many such frames a single connection can send, nor any per-session or per-IP throttling. This leaves the server vulnerable to:
- Connection exhaustion attacks
- Resource denial through excessive requests
- Protocol frame flooding that could impact the authority loop

## Impact

- No protection against network-level abuse that could degrade service for all users
- The security model explicitly requires rate limits and budgets but they are not enforced
- This is a P1 risk: without rate limiting, a compromised or misbehaving client can exhaust server resources
- Affects all protocol adapters (Nakama, memory, PostgreSQL)

## Acceptance criteria

- [ ] Per-session message budget configurable per action type with expiry window
- [ ] Per-IP rate limits for unauthenticated connections (e.g., 10 requests/second)
- [ ] Payload size caps enforced at the framing layer (not just JSON parsing)
- [ ] Rate limit exceeded errors returned as `protocol.error` with retry-after headers
- [ ] Diagnostics emitted for rate-limit rejections without leaking sensitive data
- [ ] Integration tests simulate flood conditions and verify enforcement

## Notes

This issue is linked to `NET-002` in the TODO backlog. The implementation should be modular so it can be applied consistently across all protocol adapters. Consider using a token-bucket or leaky-bucket algorithm for per-session budgets.