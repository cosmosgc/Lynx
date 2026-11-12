# QUA-003 — Protocol golden and compatibility tests

- **Status:** Done
- **Completed:** 2026-11-12
- **Source task:** [Protocol golden and compatibility tests](../70-testing-quality.md#qua-003--protocol-golden-and-compatibility-tests)

## Delivered

- Checked-in protocol-versioned fixtures for the credential-bearing
  `world.token` envelope, using the non-authenticating sentinel
  `FAKE_LYNX_TEST_TOKEN_DO_NOT_AUTHENTICATE`.
- The golden test requires the exact sentinel value, verifies wire encoding and
  decoding, and confirms `Envelope` debug output redacts the credential body.
- Existing handshake, error, negotiation, replication, request/reply, and
  compatibility matrix fixtures remain enforced.
- CI requires a protocol version increment and changelog entry when protocol
  contract source paths change.

## Verification

- `cargo test -p world_server --test protocol_golden`
- `cargo clippy -p world_server --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
