-- 0009_inventory_transfer_ledger.sql
-- Durable, append-only transfer receipts make retries safe across restarts.

CREATE TABLE lynx.inventory_transfer_receipts (
    idempotency_key text PRIMARY KEY,
    source_owner_id text NOT NULL REFERENCES lynx.characters (character_id),
    destination_owner_id text NOT NULL REFERENCES lynx.characters (character_id),
    receipt jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT inventory_transfer_key_valid CHECK (
        char_length(idempotency_key) BETWEEN 1 AND 256
    ),
    CONSTRAINT inventory_transfer_distinct_owners CHECK (
        source_owner_id <> destination_owner_id
    ),
    CONSTRAINT inventory_transfer_receipt_object CHECK (
        jsonb_typeof(receipt) = 'object'
        AND receipt->>'key' = idempotency_key
        AND jsonb_typeof(receipt->'source') = 'string'
        AND receipt->>'source' ~ '^ent_[0-9A-HJKMNP-TV-Z]{26}$'
        AND jsonb_typeof(receipt->'source_slot') = 'number'
        AND jsonb_typeof(receipt->'destination') = 'string'
        AND receipt->>'destination' ~ '^ent_[0-9A-HJKMNP-TV-Z]{26}$'
        AND jsonb_typeof(receipt->'definition') = 'string'
        AND jsonb_typeof(receipt->'quantity') = 'number'
        AND (receipt->>'quantity')::numeric > 0
        AND jsonb_typeof(receipt->'destination_slots') = 'array'
    )
);

CREATE INDEX inventory_transfer_source_created
    ON lynx.inventory_transfer_receipts (source_owner_id, created_at);
CREATE INDEX inventory_transfer_destination_created
    ON lynx.inventory_transfer_receipts (destination_owner_id, created_at);

COMMENT ON TABLE lynx.inventory_transfer_receipts IS
    'Append-only authoritative transfer receipts and idempotency keys for inventory movement.';

CREATE TRIGGER inventory_transfer_receipts_append_only
BEFORE UPDATE OR DELETE ON lynx.inventory_transfer_receipts
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();
