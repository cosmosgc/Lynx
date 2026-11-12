-- 0016_player_inventory_value_ledger.sql
-- Immutable, queryable item-quantity provenance for player inventories.

CREATE TABLE lynx.inventory_value_events (
    event_key text PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    source text NOT NULL,
    actor_kind text NOT NULL,
    actor_id text,
    reason text NOT NULL,
    correlation_id text NOT NULL,
    CONSTRAINT inventory_value_event_key_valid CHECK (
        char_length(event_key) BETWEEN 1 AND 384
        AND event_key !~ '[[:cntrl:]]'
    ),
    CONSTRAINT inventory_value_source_valid CHECK (
        char_length(source) BETWEEN 1 AND 128
        AND source !~ '[[:cntrl:]]'
    ),
    CONSTRAINT inventory_value_actor_valid CHECK (
        actor_kind IN ('character', 'system')
        AND ((actor_kind = 'system' AND actor_id IS NULL)
             OR (actor_kind = 'character' AND actor_id IS NOT NULL
                 AND actor_id ~ '^chr_[0-9A-HJKMNP-TV-Z]{26}$'))
    ),
    CONSTRAINT inventory_value_reason_valid CHECK (
        char_length(reason) BETWEEN 1 AND 1024
        AND reason !~ '[[:cntrl:]]'
    ),
    CONSTRAINT inventory_value_correlation_valid CHECK (
        char_length(correlation_id) BETWEEN 1 AND 256
        AND correlation_id !~ '[[:cntrl:]]'
    )
);

CREATE INDEX inventory_value_events_correlation_idx
    ON lynx.inventory_value_events (correlation_id, occurred_at DESC);
CREATE INDEX inventory_value_events_time_idx
    ON lynx.inventory_value_events (occurred_at DESC);

CREATE TABLE lynx.inventory_value_entries (
    entry_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_key text NOT NULL REFERENCES lynx.inventory_value_events (event_key),
    owner_character_id text NOT NULL REFERENCES lynx.characters (character_id),
    item_definition text NOT NULL,
    quantity_delta bigint NOT NULL,
    before_quantity bigint NOT NULL,
    after_quantity bigint NOT NULL,
    CONSTRAINT inventory_value_item_valid CHECK (
        char_length(item_definition) BETWEEN 1 AND 96
        AND item_definition ~ '^[a-z0-9][a-z0-9._-]*$'
        AND item_definition !~ '\.\.'
        AND item_definition !~ '\.$'
    ),
    CONSTRAINT inventory_value_quantities_valid CHECK (
        quantity_delta <> 0
        AND before_quantity >= 0
        AND after_quantity >= 0
        AND after_quantity - before_quantity = quantity_delta
    ),
    CONSTRAINT inventory_value_entry_unique UNIQUE (
        event_key, owner_character_id, item_definition
    )
);

CREATE INDEX inventory_value_entries_owner_item_idx
    ON lynx.inventory_value_entries (owner_character_id, item_definition, entry_id);
CREATE INDEX inventory_value_entries_character_idx
    ON lynx.inventory_value_entries (owner_character_id, entry_id);

-- Establish an explicit reconciliation baseline. Legacy transfer receipts are
-- retained as history; the new signed ledger starts from this cutover state.
INSERT INTO lynx.inventory_value_events (
    event_key, source, actor_kind, actor_id, reason, correlation_id
) VALUES (
    'opening-balance:0016', 'migration.0016', 'system', NULL,
    'opening inventory snapshot at ledger cutover', 'opening-balance:0016'
);

INSERT INTO lynx.inventory_value_entries (
    event_key, owner_character_id, item_definition, quantity_delta,
    before_quantity, after_quantity
)
SELECT 'opening-balance:0016', slots.owner_id, slots.item_definition,
       SUM(slots.quantity)::bigint, 0, SUM(slots.quantity)::bigint
FROM lynx.inventory_slots AS slots
JOIN lynx.characters AS characters
  ON characters.character_id = slots.owner_id
GROUP BY slots.owner_id, slots.item_definition;

CREATE TRIGGER inventory_value_events_append_only
BEFORE UPDATE OR DELETE ON lynx.inventory_value_events
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE TRIGGER inventory_value_entries_append_only
BEFORE UPDATE OR DELETE ON lynx.inventory_value_entries
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

COMMENT ON TABLE lynx.inventory_value_events IS
    'Idempotent, append-only player inventory movements with actor, source, reason, and correlation.';
COMMENT ON TABLE lynx.inventory_value_entries IS
    'Signed per-character item quantity changes; current balances reconcile as opening quantities plus deltas.';
