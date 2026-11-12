# QUA-006 — Security abuse test suite

- **Status:** Done
- **Completed:** 2026-11-12
- **Source task:** [Security abuse test suite](../70-testing-quality.md#qua-006--security-abuse-test-suite)

## Delivered

- WebSocket integration coverage rejects hostile/duplicate origins, missing or
  invalid identity, forged movement authority fields, duplicate active
  sessions, guessed/replayed resume tokens, truncated frames, oversized frame
  headers, and over-depth envelopes.
- Slow peers that upgrade but omit `session.hello` are closed before gateway
  admission. A per-IP flood test fills the 16-connection limit, then verifies
  the next upgrade receives `ip_capacity` before session admission.
- Rejection cases assert stable public codes and applicable side-effect
  counters/events. Error responses and structured security audit events are
  checked for bearer, candidate-token, and private backend-detail leakage.
- MCP workspace path confinement rejects parent traversal and absolute paths;
  bounded result tests cover oversized and over-depth output.
- The workspace test suite runs in the Ubuntu, Windows, and macOS CI jobs.

## Verification

- `cargo test -p lynx-world --test websocket connection_flood_is_bounded_before_session_admission`
- `cargo test -p lynx-world security_audit_paths_redact_credentials_and_backend_details`
- `cargo test -p lynx-world --test websocket` (7 passed)
