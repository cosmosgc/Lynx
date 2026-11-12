# QUA-010 — Error taxonomy and diagnostics audit

**Status: Done.** All acceptance criteria are checked in the source task.

## Acceptance evidence

- [x] Boundary errors have stable codes and documented retryability.
- [x] Player-facing interaction errors resolve stable codes from the negotiated
      locale catalog with `pt-BR` fallback.
- [x] Secret and private-data redaction is tested at the serialized error
      payload boundary.
- [x] An operator diagnostics catalog exists.

`ErrorCode::ALL` is the maintained inventory for the protocol error taxonomy.
The catalog regression checks every code's documented default retryability and
its `en` and `pt-BR` localization keys. The character API mapping regression
checks every `ApiError` variant's public HTTP status/code and verifies that
backend messages are not returned. Error payload serialization permits only
bounded, code-specific display parameters; transport tests cover identity,
admission, capacity, origin, and protocol errors.

## Verification

- `cargo test -p engine_net --lib every_stable_error_code_has_retry_and_locale_catalog_entries`: passed.
- `cargo test -p engine_net --lib error_payload_serialization_filters_private_and_unsafe_details`: passed.
- `cargo test -p lynx-world --lib character_api_error_mapping_matches_catalog_and_redacts_backend_text`: passed.

## Source

See [`70-testing-quality.md`](../70-testing-quality.md) and the
[error catalog](../../error-catalog.md) for the implementation details.
