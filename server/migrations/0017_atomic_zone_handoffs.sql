-- 0017_atomic_zone_handoffs.sql
-- Durable, idempotent evidence for a handoff committed with both zone states.

CREATE TABLE lynx.zone_handoffs (
    request_key text PRIMARY KEY,
    source_zone_id text NOT NULL,
    destination_zone_id text NOT NULL,
    character_id text NOT NULL,
    entity_id text NOT NULL,
    receipt jsonb NOT NULL,
    receipt_hash text NOT NULL,
    committed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT zone_handoffs_request_key_valid CHECK (
        octet_length(request_key) BETWEEN 1 AND 128
    ),
    CONSTRAINT zone_handoffs_distinct_zones CHECK (
        source_zone_id <> destination_zone_id
    ),
    CONSTRAINT zone_handoffs_source_zone_prefix CHECK (
        source_zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_handoffs_destination_zone_prefix CHECK (
        destination_zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_handoffs_character_prefix CHECK (
        character_id ~ '^chr_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_handoffs_entity_prefix CHECK (
        entity_id ~ '^ent_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_handoffs_receipt_object CHECK (
        jsonb_typeof(receipt) = 'object'
    ),
    CONSTRAINT zone_handoffs_hash_sha256 CHECK (
        receipt_hash ~ '^[0-9a-f]{64}$'
    )
);

CREATE TRIGGER zone_handoffs_append_only
BEFORE UPDATE OR DELETE ON lynx.zone_handoffs
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE INDEX zone_handoffs_character_time_idx
    ON lynx.zone_handoffs (character_id, committed_at DESC);

CREATE INDEX zone_handoffs_source_time_idx
    ON lynx.zone_handoffs (source_zone_id, committed_at DESC);

CREATE INDEX zone_handoffs_destination_time_idx
    ON lynx.zone_handoffs (destination_zone_id, committed_at DESC);

COMMENT ON TABLE lynx.zone_handoffs IS
    'Append-only handoff receipts committed atomically with source and destination zone checkpoints.';
