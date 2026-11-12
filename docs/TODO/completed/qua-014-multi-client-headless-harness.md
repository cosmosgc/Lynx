# QUA-014 — Multi-client headless harness

**Status: Completed (2026-11-10).** `tools/test_harness` provides a reusable
GPU-free in-process world server and headless client built on the production
`lynx_client_net` transport. The client records bounded typed snapshots and
deltas, sends action, movement, and zone-chat requests, optionally selects a
character, and reconnects using the latest rotated resume token. Credential
messages are excluded from recorded protocol history and debug output.

The deterministic fault injector delays or drops application envelopes at the
test-client boundary; it is not a TCP packet-level network emulator. The
multi-client integration test covers shared authoritative combat state,
session-private movement reconciliation, chat delivery, reconnect, and both
inbound and outbound delay/loss. `clients/lynx_client/tests/multiplayer.rs` also
reuses the in-process server fixture instead of duplicating gateway setup.

**Acceptance criteria**

- [x] Two harness clients observe the same authoritative state.
- [x] Reconnect and deterministic simulated network conditions are supported.
- [x] The harness is reusable from package integration tests without duplicating
      transport protocol code.
- [x] Integration waits and server shutdown use bounded timeouts for CI.

**Validation:** `cargo test -p lynx_test_harness` and
`cargo clippy -p lynx_test_harness --all-targets -- -D warnings` passed. The
existing graphical-client multiplayer scenario also passed via
`cargo test -p lynx_client --no-default-features --test multiplayer`, reusing
the shared server fixture without enabling a window or GPU.
