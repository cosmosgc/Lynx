-- 0001_core.sql
--
-- Accounts, characters, components, inventory, property, idempotency, and the
-- append-only audit log. The `lynx` schema is created and owned by
-- scripts/setup-postgres.*; migrations run as the application role.

CREATE TABLE lynx.accounts (
    account_id text PRIMARY KEY,
    nakama_user_id uuid NOT NULL UNIQUE,
    locale text NOT NULL DEFAULT 'pt-BR',
    schema_version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT accounts_id_valid CHECK (
        account_id <> ''
        AND char_length(account_id) <= 128
        AND account_id !~ '[[:space:][:cntrl:]]'
    ),
    CONSTRAINT accounts_locale_valid CHECK (
        locale ~ '^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$'
    ),
    CONSTRAINT accounts_schema_version CHECK (schema_version >= 1)
);

COMMENT ON TABLE lynx.accounts IS
    'Game account linked to a Nakama user; the game never stores credentials.';

CREATE TABLE lynx.characters (
    character_id text PRIMARY KEY,
    account_id text NOT NULL REFERENCES lynx.accounts (account_id) ON DELETE CASCADE,
    name text NOT NULL,
    zone_id text,
    position_x double precision NOT NULL DEFAULT 0,
    position_y double precision NOT NULL DEFAULT 0,
    position_z double precision NOT NULL DEFAULT 0,
    heading real NOT NULL DEFAULT 0,
    status text NOT NULL DEFAULT 'active',
    schema_version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    CONSTRAINT characters_id_prefix CHECK (
        character_id ~ '^chr_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT characters_name_length CHECK (char_length(name) BETWEEN 1 AND 32),
    CONSTRAINT characters_name_trimmed CHECK (name = btrim(name)),
    CONSTRAINT characters_name_no_control CHECK (name !~ '[[:cntrl:]]'),
    CONSTRAINT characters_zone_prefix CHECK (
        zone_id IS NULL OR zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT characters_position_finite CHECK (
        position_x = position_x
        AND position_y = position_y
        AND position_z = position_z
        AND abs(position_x) < 1e9
        AND abs(position_y) < 1e9
        AND abs(position_z) < 1e9
    ),
    CONSTRAINT characters_heading_finite CHECK (heading = heading),
    CONSTRAINT characters_status_valid CHECK (
        status IN ('active', 'suspended', 'deleted')
    ),
    CONSTRAINT characters_schema_version CHECK (schema_version >= 1),
    CONSTRAINT characters_deleted_consistent CHECK (
        (status = 'deleted') = (deleted_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX characters_name_unique
    ON lynx.characters (lower(name))
    WHERE deleted_at IS NULL;
CREATE INDEX characters_account_idx
    ON lynx.characters (account_id)
    WHERE deleted_at IS NULL;

COMMENT ON TABLE lynx.characters IS
    'Player characters. Names are unique case-insensitively among live rows.';

CREATE TABLE lynx.character_components (
    character_id text NOT NULL
        REFERENCES lynx.characters (character_id) ON DELETE CASCADE,
    component_key text NOT NULL,
    schema_version integer NOT NULL,
    data jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (character_id, component_key),
    CONSTRAINT character_components_key_valid CHECK (
        component_key ~ '^[a-z][a-z0-9]*(\.[a-z][a-z0-9_]*)+$'
    ),
    CONSTRAINT character_components_schema_version CHECK (schema_version >= 1),
    CONSTRAINT character_components_object CHECK (jsonb_typeof(data) = 'object')
);

COMMENT ON TABLE lynx.character_components IS
    'Versioned component payloads keyed by the engine component registry.';

CREATE TABLE lynx.inventory_slots (
    owner_id text NOT NULL,
    slot integer NOT NULL,
    item_definition text NOT NULL,
    quantity integer NOT NULL,
    instance_id text,
    schema_version integer NOT NULL DEFAULT 1,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_id, slot),
    CONSTRAINT inventory_owner_prefix CHECK (
        owner_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT inventory_slot_range CHECK (slot >= 0 AND slot < 512),
    CONSTRAINT inventory_quantity_positive CHECK (quantity > 0),
    CONSTRAINT inventory_definition_valid CHECK (
        char_length(item_definition) <= 96
        AND item_definition ~ '^[a-z0-9][a-z0-9._-]*$'
        AND item_definition !~ '\.\.'
        AND item_definition !~ '\.$'
    ),
    CONSTRAINT inventory_instance_prefix CHECK (
        instance_id IS NULL OR instance_id ~ '^itm_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT inventory_unique_is_single CHECK (
        instance_id IS NULL OR quantity = 1
    ),
    CONSTRAINT inventory_schema_version CHECK (schema_version >= 1)
);

CREATE UNIQUE INDEX inventory_instance_unique
    ON lynx.inventory_slots (instance_id)
    WHERE instance_id IS NOT NULL;

COMMENT ON TABLE lynx.inventory_slots IS
    'Slot rows keyed by owner and slot; unique item instances appear once.';

CREATE TABLE lynx.property_ownership (
    property_id text PRIMARY KEY,
    owner_character_id text
        REFERENCES lynx.characters (character_id) ON DELETE SET NULL,
    zone_id text NOT NULL,
    acquired_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT property_id_prefix CHECK (
        property_id ~ '^prop_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT property_zone_prefix CHECK (
        zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT property_release_after_acquire CHECK (
        released_at IS NULL OR released_at >= acquired_at
    ),
    CONSTRAINT property_schema_version CHECK (schema_version >= 1)
);

CREATE INDEX property_owner_idx
    ON lynx.property_ownership (owner_character_id)
    WHERE released_at IS NULL;

COMMENT ON TABLE lynx.property_ownership IS
    'Durable ownership of authored property; released rows are kept for history.';

CREATE TABLE lynx.idempotency_keys (
    scope text NOT NULL,
    key text NOT NULL,
    operation text NOT NULL,
    result jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    PRIMARY KEY (scope, key),
    CONSTRAINT idempotency_scope_valid CHECK (
        scope <> '' AND char_length(scope) <= 128
    ),
    CONSTRAINT idempotency_key_valid CHECK (
        key <> '' AND char_length(key) <= 128
    ),
    CONSTRAINT idempotency_operation_valid CHECK (
        operation <> '' AND char_length(operation) <= 128
    ),
    CONSTRAINT idempotency_result_object CHECK (jsonb_typeof(result) = 'object'),
    CONSTRAINT idempotency_expiry_order CHECK (
        expires_at IS NULL OR expires_at > created_at
    )
);

CREATE INDEX idempotency_expiry_idx
    ON lynx.idempotency_keys (expires_at)
    WHERE expires_at IS NOT NULL;

COMMENT ON TABLE lynx.idempotency_keys IS
    'Receipts for retryable mutations; duplicate keys return the stored result.';

CREATE TABLE lynx.audit_events (
    event_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at timestamptz NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    actor_kind text NOT NULL,
    actor_id text,
    actor_role text,
    action text NOT NULL,
    target_id text,
    outcome text NOT NULL,
    correlation_id text NOT NULL,
    reason text,
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT audit_actor_kind_valid CHECK (
        actor_kind IN ('character', 'ai', 'system', 'admin')
    ),
    CONSTRAINT audit_actor_id_consistent CHECK (
        (actor_kind = 'system' AND actor_id IS NULL)
        OR (actor_kind <> 'system' AND actor_id IS NOT NULL)
    ),
    CONSTRAINT audit_role_consistent CHECK (
        (actor_kind = 'admin') = (actor_role IS NOT NULL)
    ),
    CONSTRAINT audit_action_valid CHECK (
        action <> '' AND char_length(action) <= 128
    ),
    CONSTRAINT audit_target_prefix CHECK (
        target_id IS NULL
        OR target_id ~ '^(chr|ent|itm|prop|zone)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT audit_outcome_valid CHECK (
        outcome IN ('attempted', 'succeeded', 'rejected', 'failed')
    ),
    CONSTRAINT audit_correlation_valid CHECK (
        correlation_id <> '' AND char_length(correlation_id) <= 128
    ),
    CONSTRAINT audit_reason_length CHECK (
        reason IS NULL OR char_length(reason) <= 1024
    ),
    CONSTRAINT audit_details_object CHECK (jsonb_typeof(details) = 'object')
);

CREATE INDEX audit_events_target_idx
    ON lynx.audit_events (target_id, occurred_at DESC)
    WHERE target_id IS NOT NULL;
CREATE INDEX audit_events_correlation_idx
    ON lynx.audit_events (correlation_id);
CREATE INDEX audit_events_occurred_idx
    ON lynx.audit_events (occurred_at DESC);

COMMENT ON TABLE lynx.audit_events IS
    'Append-only audit log. Retention purges must set lynx.allow_append_only_purge=on in the transaction.';

CREATE FUNCTION lynx.reject_append_only_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('lynx.allow_append_only_purge', true) = 'on' THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
        RETURN NEW;
    END IF;
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER audit_events_append_only
BEFORE UPDATE OR DELETE ON lynx.audit_events
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE FUNCTION lynx.touch_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

CREATE TRIGGER accounts_touch_updated_at
BEFORE UPDATE ON lynx.accounts
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TRIGGER characters_touch_updated_at
BEFORE UPDATE ON lynx.characters
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TRIGGER character_components_touch_updated_at
BEFORE UPDATE ON lynx.character_components
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TRIGGER inventory_slots_touch_updated_at
BEFORE UPDATE ON lynx.inventory_slots
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TRIGGER property_ownership_touch_updated_at
BEFORE UPDATE ON lynx.property_ownership
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();
