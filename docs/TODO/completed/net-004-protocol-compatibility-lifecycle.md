# NET-004 — Protocol compatibility lifecycle

**Status:** Done

The active-major compatibility policy, content-major rejection, feature
degradation, rollback guidance, and version/changelog CI gate are documented.
Golden request/response coverage now spans all 27 live message kinds. The
tests exercise the protocol, gateway, calendar, world-server, and world-service
serialization or parsing paths, while the catalog test asserts the complete
kind set. The `world.token` golden is compared with the production token
envelope builder after substituting only the synthetic
`FAKE_LYNX_TEST_TOKEN_DO_NOT_AUTHENTICATE` sentinel; debug output remains
redacted.

Verification passed:

- `cargo test -p lynx-world --lib service::tests::a_static_token_admits_one_character_and_returns_a_resume_secret`
- `cargo test -p world_server --test server` (17 tests)
- `cargo test -p world_server --test protocol_golden` (3 tests)
- `cargo test -p world_gateway --test gateway` (53 tests)
- `cargo test -p world_gateway --test calendar` (8 tests)
- `cargo test -p engine_net --test protocol_golden` (8 tests)
