# Error and operator diagnostic catalog

`ErrorCode::as_str()` is the stable machine-readable category. The protocol
serializes it as `ErrorPayload.code`; `ErrorPayload.retryable` carries the
operation-specific retry decision. The client contract is to render
`error.<code>` from a locale catalog and use only explicitly safe `params` for
placeholders. `diagnostic` is English operator context and must never be shown
as player-facing text. See [`engine_core::error`](../crates/engine_core/src/error.rs)
and [`ErrorPayload`](../crates/engine_net/src/protocol.rs).

## Stable codes and default retry behavior

`retryable` on an individual `EngineError` is authoritative. It defaults to the
value in `ErrorCode::is_retryable()`, but adapters may override it when their
failure semantics are more specific (for example, a transient backend
transport failure mapped to `internal`). Do not infer retry behavior from the
code when the response includes an explicit flag.

| Code | Default retryable | Player action | Operator response |
| --- | --- | --- | --- |
| `invalid_request` | No | Correct the input or update an incompatible client. | Check the request family and validation context; repeated occurrences may indicate a client defect. |
| `not_found` | No | Refresh the view; the object may have been removed. | Check object lifecycle and request correlation. Avoid revealing whether a private object exists. |
| `out_of_range` | No | Move into the advertised interaction range. | Check authoritative position, range, and units if reports persist. |
| `not_visible` | No | Refresh nearby state or move into view. | Check interest and visibility policy; do not widen visibility to silence the error. |
| `permission_denied` | No | Select an allowed action or account. | Check principal, ownership, and permission policy. |
| `invalid_state` | No | Refresh state and choose an action valid for the current state. | Inspect the state transition and invariant diagnostics. |
| `session_expired` | No | Authenticate again. | Check session expiry/revocation flow; unknown and expired resume secrets intentionally share this code. |
| `cooldown` | Yes | Wait until the server permits another attempt. | Check cooldown policy and tick progress; honor any safe retry parameter. |
| `capacity_exceeded` | No | Free a slot or choose a smaller operation. | Check configured capacity and queue/slot utilization. |
| `version_mismatch` | No | Install a compatible client or content bundle. | Compare negotiated protocol, engine, and content versions. |
| `content_invalid` | No | No player retry is expected. | Fix the referenced authored content and validate the bundle before release. |
| `conflict` | Yes | Refresh the affected state, then submit a new request. | Check concurrent updates and idempotency/replay handling. Retrying an unchanged stale payload may repeat the conflict. |
| `budget_exhausted` | Yes | Retry after backing off or reduce the requested work. | Check bounded work budgets and load. |
| `rate_limited` | Yes | Wait for `retry_after_ms` when present, then retry. | Check the bounded limiter class and source; avoid high-cardinality identity labels. |
| `unsupported` | No | Update the client or choose a supported operation. | Check feature negotiation and server capability rollout. |
| `internal` | No | Retry only if the response explicitly marks the particular failure retryable. | Use the correlated diagnostic and server logs; do not retry-loop or expose internal text to players. |

## Boundary and redaction rules

- WebSocket `protocol.error` and `action.result` carry stable codes. The
  protocol diagnostic is for operator context; clients must branch on the
  code and never display that diagnostic directly.
- Character HTTP endpoints intentionally collapse foreign, missing, and
  unavailable identity details into safe public codes. Keep that equivalence
  when adding endpoints.
- Backend adapter errors map to `ErrorCode`; provider response bodies are not
  propagated because they may echo request data or credentials.
- Error details and localized parameters must be allowlisted, bounded, and
  free of credentials, resume tokens, passwords, and private player data.
  Metrics use fixed codes and bounded classes, never raw diagnostics or
  arbitrary detail values as labels.
- `ErrorPayload` now allowlists display parameters by code, caps each value at
  128 bytes, caps the parameter set at eight entries, and drops control
  characters. Internal `EngineError` details remain available to trusted
  diagnostics but do not cross this wire conversion by default.
- `ErrorCode::is_retryable()` defaults are defined in code. Any explicit
  `with_retryable` override must reflect the concrete operation and be covered
  in the adapter's boundary documentation.

## World HTTP and WebSocket admission responses

The public world edge also has HTTP errors that are not `ErrorPayload`s. They
use a JSON object with an `error` code; these codes are stable within the
endpoint contract and must not include backend messages or credentials.

| Code | HTTP status | Retry behavior and boundary meaning |
| --- | --- | --- |
| `unauthorized` | 401 | Authenticate again with a valid bearer session. Used for missing or malformed credentials on character routes and rejected demo credentials. |
| `character_not_found` | 404 | Do not retry unchanged. Missing, deleted, suspended, and foreign characters intentionally share this response. |
| `invalid_request` | 400 | Correct the request. Character API validation failures do not serialize their internal message. |
| `idempotency_key_required` | 400 | Supply the required idempotency header before retrying character creation. |
| `conflict` | 409 | Refresh the affected state before submitting a new request. |
| `rate_limited` | 429 | Retry after the supplied `retry_after_ms` when present; admission responses also set `Retry-After` when a valid delay is available. |
| `not_found` | 404 | Character service is not configured on this world instance; the response does not expose configuration details. |
| `character_service_unavailable` | 503 | Retry with backoff after the character backend or worker becomes available. Provider details are deliberately omitted. |
| `request_headers_too_large` | 431 | Reduce request headers before retrying. |
| `origin_forbidden` | 403 | Use an allowed browser origin. |
| `capacity_exceeded` | 503 | Retry after admission capacity becomes available; a retry delay may be included. |
| `draining` | 503 | The world is shutting down or draining connections; reconnect to a ready instance. |
| `total_capacity` | 503 | The world has reached its connection limit; retry after capacity becomes available. |
| `ip_capacity` | 503 | The source IP has reached its connection limit; retry after a connection closes. |
| `invalid_session` | 401 | Authenticate again. The credential itself and identity-provider failure details are not returned. |
| `character_forbidden` | 403 | Select a character currently owned by the authenticated account; identity-provider details are not returned. |
| `identity_unavailable` | 503 | Retry with backoff when the identity service is available. |

Character API handlers map internal `ApiError`s to this allowlisted set and
serialize only the code. WebSocket upgrade admission maps rate limits and
admission identity capacity to their stable `ErrorCode` names, while active
connection caps use the bounded `draining`, `total_capacity`, and
`ip_capacity` reasons. Identity admission uses the three identity codes above.
Keep these HTTP status/code pairs consistent with the mapping in
`lynx_world::transport`; do not expose raw error messages or diagnostics.

## Current localization gap

The wire DTO carries `error.<code>` semantics, and the online interaction
client resolves all stable codes through the `pt-BR` and `en` catalogs in
`content/localization/`. It selects the requested locale from the handshake and
falls back to `pt-BR`. The account/world path still lacks a player-facing locale
selector and persistence. Do not treat the
operator `diagnostic` as a fallback player string. Complete this through the
client localization integration tracked by `CLT-010`, then require every
player-facing code key in the source catalog.
