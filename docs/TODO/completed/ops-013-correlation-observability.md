# OPS-013 — Correlation and observability plumbing

**Completed:** 2026-11-12

Validated wire message IDs now follow action requests from the world transport
through the gateway into simulation state. Each bounded action-result snapshot
cache entry can retain its wire correlation ID beside its scoped idempotency
request ID. `zone.action-results.replace` persists both maps in one event, and
replay restores them. The correlation map is an optional additive JSON field on
the existing event kind, so older replay readers can continue to process action
results.

The `world_action_processed` structured event records stable service, zone,
connection, session, actor, message kind, correlation, outcome, and duration
fields. Applied actions are logged at info; every non-applied outcome is logged
at warn with a stable reason when available. Events are unsampled and omit
request bodies. `docs/operations.md` documents the schema, collector handling,
redaction, a durable event lookup query, and stdout log search as the current
trace investigation path.

**Verification:**

- `cargo test -p lynx-world --lib action_correlation_connects_wire_id_to_durable_zone_event` — passed. It follows a real wire action through structured fields, the snapshot, restore, and durable event payload.
- `cargo test -p lynx-world --lib replay_applies_non_entity_state_replacements` — passed. It replays the correlation map from the compatible action-result replacement event.
- `authority_rejection_audit_is_correlated_and_redacted` exercises correlation logging without private error details.
- `cargo fmt --all` completed.

All four OPS-013 acceptance criteria are checked in
[`60-operations-release.md`](../60-operations-release.md).
