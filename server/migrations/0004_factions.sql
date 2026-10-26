-- 0004_factions.sql
--
-- Durable faction catalog, governance, membership, treasury ledger, territory
-- ownership, and append-only history (ADR 0020). Live ownership and wealth are
-- server state; they are never baked into map or zone content.

CREATE TABLE lynx.factions (
    faction_id text PRIMARY KEY,
    display_name text NOT NULL,
    description text,
    controller text NOT NULL,
    max_members integer NOT NULL,
    content_version_major integer NOT NULL,
    content_version_minor integer NOT NULL,
    power_version_major integer NOT NULL,
    power_version_minor integer NOT NULL,
    power_base numeric(20,0) NOT NULL DEFAULT 0,
    power_treasury_weight_bps integer NOT NULL DEFAULT 1000,
    power_reserve numeric(20,0) NOT NULL DEFAULT 0,
    power_cap numeric(20,0) NOT NULL DEFAULT 1000000,
    schema_version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT factions_id_valid CHECK (
        char_length(faction_id) <= 96
        AND faction_id ~ '^faction\.[a-z0-9][a-z0-9._-]*$'
        AND faction_id !~ '\.\.'
        AND faction_id !~ '\.$'
    ),
    CONSTRAINT factions_name_valid CHECK (
        btrim(display_name) <> ''
        AND display_name = btrim(display_name)
        AND char_length(display_name) <= 128
        AND display_name !~ '[[:cntrl:]]'
    ),
    CONSTRAINT factions_description_length CHECK (
        description IS NULL OR char_length(description) <= 1024
    ),
    CONSTRAINT factions_controller_valid CHECK (
        controller IN ('player', 'npc', 'system')
    ),
    CONSTRAINT factions_capacity_valid CHECK (
        max_members BETWEEN 1 AND 10000
    ),
    CONSTRAINT factions_content_version_valid CHECK (
        content_version_major >= 1 AND content_version_minor >= 0
    ),
    CONSTRAINT factions_power_version_valid CHECK (
        power_version_major >= 1 AND power_version_minor >= 0
    ),
    CONSTRAINT factions_power_values_valid CHECK (
        power_base >= 0
        AND power_reserve >= 0
        AND power_treasury_weight_bps BETWEEN 0 AND 10000
        AND power_cap >= power_base
    ),
    CONSTRAINT factions_schema_version CHECK (schema_version >= 1)
);

COMMENT ON TABLE lynx.factions IS
    'Authoritative faction catalog, governance limits, and power rules.';

CREATE TRIGGER factions_touch_updated_at
BEFORE UPDATE ON lynx.factions
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TABLE lynx.faction_role_permissions (
    faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE CASCADE,
    role text NOT NULL,
    permission text NOT NULL,
    PRIMARY KEY (faction_id, role, permission),
    CONSTRAINT faction_permissions_role_valid CHECK (
        role IN ('leader', 'officer', 'member')
    ),
    CONSTRAINT faction_permissions_permission_valid CHECK (
        permission IN (
            'invite_member', 'remove_member', 'set_role', 'transfer_leadership',
            'deposit_treasury', 'spend_treasury', 'claim_territory',
            'capture_territory', 'release_territory', 'declare_conflict'
        )
    )
);

COMMENT ON TABLE lynx.faction_role_permissions IS
    'Data-driven capabilities granted to each faction role.';

CREATE TABLE lynx.faction_spend_limits (
    faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE CASCADE,
    role text NOT NULL,
    max_amount numeric(20,0) NOT NULL,
    PRIMARY KEY (faction_id, role),
    CONSTRAINT faction_spend_limits_role_valid CHECK (
        role IN ('leader', 'officer', 'member')
    ),
    CONSTRAINT faction_spend_limits_positive CHECK (max_amount > 0)
);

COMMENT ON TABLE lynx.faction_spend_limits IS
    'Per-transaction treasury limits by role; roles without a row may not spend.';

CREATE TABLE lynx.faction_relations (
    source_faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE CASCADE,
    target_faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE CASCADE,
    alignment smallint NOT NULL,
    PRIMARY KEY (source_faction_id, target_faction_id),
    CONSTRAINT faction_relations_alignment_range CHECK (
        alignment BETWEEN -100 AND 100
    )
);

COMMENT ON TABLE lynx.faction_relations IS
    'Directional faction-to-faction alignment in -100..=100.';

CREATE TABLE lynx.faction_members (
    faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE CASCADE,
    member_id text NOT NULL,
    role text NOT NULL,
    joined_at timestamptz NOT NULL DEFAULT now(),
    schema_version integer NOT NULL DEFAULT 1,
    PRIMARY KEY (faction_id, member_id),
    CONSTRAINT faction_members_id_valid CHECK (
        member_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT faction_members_role_valid CHECK (
        role IN ('leader', 'officer', 'member')
    ),
    CONSTRAINT faction_members_schema_version CHECK (schema_version >= 1)
);

CREATE UNIQUE INDEX faction_members_one_faction
    ON lynx.faction_members (member_id);
CREATE UNIQUE INDEX faction_members_one_leader
    ON lynx.faction_members (faction_id)
    WHERE role = 'leader';
CREATE INDEX faction_members_roster_idx
    ON lynx.faction_members (faction_id, role, joined_at);

COMMENT ON TABLE lynx.faction_members IS
    'One faction per member and exactly one leader per faction, checked at commit.';

CREATE FUNCTION lynx.assert_faction_leader(target text) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM lynx.factions WHERE faction_id = target
    ) THEN
        RETURN;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM lynx.faction_members
        WHERE faction_id = target AND role = 'leader'
    ) THEN
        RAISE EXCEPTION 'faction % must have exactly one leader', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

CREATE FUNCTION lynx.require_faction_leader() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        PERFORM lynx.assert_faction_leader(OLD.faction_id);
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        PERFORM lynx.assert_faction_leader(NEW.faction_id);
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER faction_members_require_leader
AFTER INSERT OR UPDATE OR DELETE ON lynx.faction_members
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_faction_leader();

CREATE TABLE lynx.faction_treasury_entries (
    entry_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE CASCADE,
    entry_key text NOT NULL,
    kind text NOT NULL,
    amount numeric(20,0) NOT NULL,
    counterparty_faction_id text
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    reason text NOT NULL,
    occurred_at timestamptz NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT faction_treasury_entries_key_valid CHECK (
        entry_key <> ''
        AND char_length(entry_key) <= 128
        AND entry_key !~ '[[:cntrl:]]'
    ),
    CONSTRAINT faction_treasury_entries_kind_valid CHECK (
        kind IN ('deposit', 'spend', 'transfer_out', 'transfer_in', 'loss', 'reward')
    ),
    CONSTRAINT faction_treasury_entries_amount_positive CHECK (amount > 0),
    CONSTRAINT faction_treasury_entries_reason_valid CHECK (
        btrim(reason) <> ''
        AND char_length(reason) <= 512
        AND reason !~ '[[:cntrl:]]'
    ),
    CONSTRAINT faction_treasury_entries_counterparty_consistent CHECK (
        ((kind IN ('transfer_out', 'transfer_in')) = (counterparty_faction_id IS NOT NULL))
        AND (counterparty_faction_id IS NULL OR counterparty_faction_id <> faction_id)
    ),
    CONSTRAINT faction_treasury_entries_schema_version CHECK (schema_version >= 1),
    UNIQUE (faction_id, entry_key)
);

CREATE INDEX faction_treasury_entries_faction_idx
    ON lynx.faction_treasury_entries (faction_id, entry_id);
CREATE INDEX faction_treasury_entries_key_idx
    ON lynx.faction_treasury_entries (entry_key);

COMMENT ON TABLE lynx.faction_treasury_entries IS
    'Append-only signed treasury ledger. Balance is the sum of signed amounts.';

CREATE TRIGGER faction_treasury_entries_append_only
BEFORE UPDATE OR DELETE ON lynx.faction_treasury_entries
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE TABLE lynx.faction_treasury_transfers (
    transfer_key text PRIMARY KEY,
    source_faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    target_faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    amount numeric(20,0) NOT NULL,
    occurred_at timestamptz NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT faction_treasury_transfers_key_valid CHECK (
        transfer_key <> ''
        AND char_length(transfer_key) <= 128
        AND transfer_key !~ '[[:cntrl:]]'
    ),
    CONSTRAINT faction_treasury_transfers_distinct CHECK (
        source_faction_id <> target_faction_id
    ),
    CONSTRAINT faction_treasury_transfers_amount_positive CHECK (amount > 0),
    CONSTRAINT faction_treasury_transfers_schema_version CHECK (schema_version >= 1)
);

CREATE INDEX faction_treasury_transfers_source_idx
    ON lynx.faction_treasury_transfers (source_faction_id, occurred_at);
CREATE INDEX faction_treasury_transfers_target_idx
    ON lynx.faction_treasury_transfers (target_faction_id, occurred_at);

COMMENT ON TABLE lynx.faction_treasury_transfers IS
    'Append-only receipts for inter-faction treasury transfers.';

CREATE TRIGGER faction_treasury_transfers_append_only
BEFORE UPDATE OR DELETE ON lynx.faction_treasury_transfers
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE FUNCTION lynx.assert_treasury_balance_non_negative(target text) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    balance numeric;
BEGIN
    SELECT COALESCE(
        SUM(
            CASE kind
                WHEN 'deposit' THEN amount
                WHEN 'reward' THEN amount
                WHEN 'transfer_in' THEN amount
                ELSE -amount
            END
        ),
        0
    )
    INTO balance
    FROM lynx.faction_treasury_entries
    WHERE faction_id = target;

    IF balance < 0 THEN
        RAISE EXCEPTION 'faction % treasury balance cannot become negative', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

CREATE FUNCTION lynx.require_non_negative_treasury() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM lynx.assert_treasury_balance_non_negative(NEW.faction_id);
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER faction_treasury_non_negative
AFTER INSERT ON lynx.faction_treasury_entries
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_non_negative_treasury();

CREATE FUNCTION lynx.assert_transfer_entry_matches_receipt() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.kind = 'transfer_out' THEN
        IF NOT EXISTS (
            SELECT 1 FROM lynx.faction_treasury_transfers
            WHERE transfer_key = NEW.entry_key
              AND source_faction_id = NEW.faction_id
              AND target_faction_id = NEW.counterparty_faction_id
              AND amount = NEW.amount
        ) THEN
            RAISE EXCEPTION 'treasury entry % has no matching outbound transfer', NEW.entry_key
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.kind = 'transfer_in' THEN
        IF NOT EXISTS (
            SELECT 1 FROM lynx.faction_treasury_transfers
            WHERE transfer_key = NEW.entry_key
              AND target_faction_id = NEW.faction_id
              AND source_faction_id = NEW.counterparty_faction_id
              AND amount = NEW.amount
        ) THEN
            RAISE EXCEPTION 'treasury entry % has no matching inbound transfer', NEW.entry_key
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER faction_treasury_entry_matches_transfer
AFTER INSERT ON lynx.faction_treasury_entries
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW
WHEN (NEW.kind IN ('transfer_out', 'transfer_in'))
EXECUTE FUNCTION lynx.assert_transfer_entry_matches_receipt();

CREATE FUNCTION lynx.require_transfer_entries() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM lynx.faction_treasury_entries
        WHERE faction_id = NEW.source_faction_id
          AND entry_key = NEW.transfer_key
          AND kind = 'transfer_out'
          AND amount = NEW.amount
          AND counterparty_faction_id = NEW.target_faction_id
    ) THEN
        RAISE EXCEPTION 'treasury transfer % is missing its outbound entry', NEW.transfer_key
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM lynx.faction_treasury_entries
        WHERE faction_id = NEW.target_faction_id
          AND entry_key = NEW.transfer_key
          AND kind = 'transfer_in'
          AND amount = NEW.amount
          AND counterparty_faction_id = NEW.source_faction_id
    ) THEN
        RAISE EXCEPTION 'treasury transfer % is missing its inbound entry', NEW.transfer_key
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER faction_treasury_transfer_requires_entries
AFTER INSERT ON lynx.faction_treasury_transfers
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_transfer_entries();

CREATE VIEW lynx.faction_treasury_balances AS
SELECT
    f.faction_id,
    COALESCE(
        SUM(
            CASE e.kind
                WHEN 'deposit' THEN e.amount
                WHEN 'reward' THEN e.amount
                WHEN 'transfer_in' THEN e.amount
                ELSE -e.amount
            END
        ),
        0
    )::numeric(20,0) AS balance
FROM lynx.factions f
LEFT JOIN lynx.faction_treasury_entries e ON e.faction_id = f.faction_id
GROUP BY f.faction_id;

COMMENT ON VIEW lynx.faction_treasury_balances IS
    'Inspectable treasury balance derived from the ledger.';

CREATE VIEW lynx.faction_roster AS
SELECT
    m.faction_id,
    f.display_name,
    m.member_id,
    m.role,
    m.joined_at
FROM lynx.faction_members m
JOIN lynx.factions f ON f.faction_id = m.faction_id;

COMMENT ON VIEW lynx.faction_roster IS
    'Faction members with their stable role and join time.';

CREATE TABLE lynx.faction_world (
    singleton boolean PRIMARY KEY DEFAULT true,
    claim_enabled boolean NOT NULL,
    capture_enabled boolean NOT NULL,
    claim_protection_ms bigint NOT NULL,
    capture_cooldown_ms bigint NOT NULL,
    minimum_capture_power numeric(20,0) NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT faction_world_singleton CHECK (singleton),
    CONSTRAINT faction_world_windows_valid CHECK (
        claim_protection_ms BETWEEN 0 AND 315576000000
        AND capture_cooldown_ms BETWEEN 0 AND 315576000000
    ),
    CONSTRAINT faction_world_power_valid CHECK (minimum_capture_power >= 0),
    CONSTRAINT faction_world_schema_version CHECK (schema_version >= 1)
);

INSERT INTO lynx.faction_world (
    singleton,
    claim_enabled,
    capture_enabled,
    claim_protection_ms,
    capture_cooldown_ms,
    minimum_capture_power,
    schema_version
)
VALUES (true, true, true, 86400000, 3600000, 0, 1);

COMMENT ON TABLE lynx.faction_world IS
    'Single-row global territory claim and capture configuration.';

CREATE TABLE lynx.faction_territories (
    zone_id text PRIMARY KEY,
    owner_faction_id text
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    contested boolean NOT NULL DEFAULT false,
    claimed_at timestamptz,
    protection_until timestamptz,
    cooldown_until timestamptz,
    revision bigint NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT faction_territories_zone_valid CHECK (
        char_length(zone_id) <= 96
        AND zone_id ~ '^zone\.[a-z0-9][a-z0-9._-]*$'
        AND zone_id !~ '\.\.'
        AND zone_id !~ '\.$'
    ),
    CONSTRAINT faction_territories_revision_positive CHECK (revision >= 1),
    CONSTRAINT faction_territories_owner_consistent CHECK (
        (owner_faction_id IS NOT NULL) = (claimed_at IS NOT NULL)
    ),
    CONSTRAINT faction_territories_windows_order CHECK (
        (protection_until IS NULL OR protection_until >= claimed_at)
        AND (cooldown_until IS NULL OR cooldown_until >= claimed_at)
    ),
    CONSTRAINT faction_territories_unclaimed_windows CHECK (
        owner_faction_id IS NOT NULL OR protection_until IS NULL
    ),
    CONSTRAINT faction_territories_schema_version CHECK (schema_version >= 1)
);

CREATE INDEX faction_territories_owner_idx
    ON lynx.faction_territories (owner_faction_id)
    WHERE owner_faction_id IS NOT NULL;

COMMENT ON TABLE lynx.faction_territories IS
    'Current ownership of authored zones; ownership never rewrites zone content.';

CREATE TRIGGER faction_territories_touch_updated_at
BEFORE UPDATE ON lynx.faction_territories
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TABLE lynx.faction_territory_history (
    change_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    change_key text NOT NULL UNIQUE,
    zone_id text NOT NULL
        REFERENCES lynx.faction_territories (zone_id) ON DELETE RESTRICT,
    action text NOT NULL,
    from_faction_id text
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    to_faction_id text
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    actor_id text NOT NULL,
    occurred_at timestamptz NOT NULL,
    revision bigint NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT faction_territory_history_key_valid CHECK (
        change_key <> ''
        AND char_length(change_key) <= 128
        AND change_key !~ '[[:cntrl:]]'
    ),
    CONSTRAINT faction_territory_history_action_valid CHECK (
        action IN ('claim', 'capture', 'release')
    ),
    CONSTRAINT faction_territory_history_actor_valid CHECK (
        actor_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT faction_territory_history_revision_positive CHECK (revision >= 1),
    CONSTRAINT faction_territory_history_transition_consistent CHECK (
        (action = 'claim' AND from_faction_id IS NULL AND to_faction_id IS NOT NULL)
        OR (action = 'capture' AND from_faction_id IS NOT NULL
            AND to_faction_id IS NOT NULL AND from_faction_id <> to_faction_id)
        OR (action = 'release' AND from_faction_id IS NOT NULL AND to_faction_id IS NULL)
    ),
    CONSTRAINT faction_territory_history_schema_version CHECK (schema_version >= 1),
    UNIQUE (zone_id, revision)
);

CREATE INDEX faction_territory_history_occurred_idx
    ON lynx.faction_territory_history (occurred_at DESC);

COMMENT ON TABLE lynx.faction_territory_history IS
    'Append-only territory ownership transitions; the newest revision is current.';

CREATE TRIGGER faction_territory_history_append_only
BEFORE UPDATE OR DELETE ON lynx.faction_territory_history
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE FUNCTION lynx.assert_territory_history_consistent(target text) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    current_owner text;
    current_revision bigint;
    latest_owner text;
    latest_revision bigint;
BEGIN
    SELECT owner_faction_id, revision
    INTO current_owner, current_revision
    FROM lynx.faction_territories
    WHERE zone_id = target;

    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT to_faction_id, revision
    INTO latest_owner, latest_revision
    FROM lynx.faction_territory_history
    WHERE zone_id = target
    ORDER BY revision DESC
    LIMIT 1;

    IF latest_revision IS NULL THEN
        IF current_owner IS NOT NULL OR current_revision <> 1 THEN
            RAISE EXCEPTION 'territory % has ownership state without history', target
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN;
    END IF;

    IF latest_revision <> current_revision THEN
        RAISE EXCEPTION 'territory % revision % does not match history %',
            target, current_revision, latest_revision
            USING ERRCODE = 'check_violation';
    END IF;
    IF latest_owner IS DISTINCT FROM current_owner THEN
        RAISE EXCEPTION 'territory % owner does not match its latest history', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

CREATE FUNCTION lynx.require_territory_history_consistency() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'faction_territories' THEN
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_territory_history_consistent(OLD.zone_id);
        ELSE
            PERFORM lynx.assert_territory_history_consistent(NEW.zone_id);
        END IF;
    ELSE
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_territory_history_consistent(OLD.zone_id);
        ELSE
            PERFORM lynx.assert_territory_history_consistent(NEW.zone_id);
        END IF;
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER faction_territories_history_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.faction_territories
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_territory_history_consistency();

CREATE CONSTRAINT TRIGGER faction_territory_history_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.faction_territory_history
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_territory_history_consistency();

CREATE VIEW lynx.faction_territory_owners AS
SELECT
    t.zone_id,
    t.owner_faction_id,
    f.display_name AS owner_display_name,
    t.contested,
    t.claimed_at,
    t.protection_until,
    t.cooldown_until,
    t.revision
FROM lynx.faction_territories t
LEFT JOIN lynx.factions f ON f.faction_id = t.owner_faction_id;

COMMENT ON VIEW lynx.faction_territory_owners IS
    'Current territory ownership with the owning faction display name.';

-- Authored world maps are a content kind (ADR 0019); earlier constraints did
-- not include them.
ALTER TABLE lynx.content_bundle_definitions
    DROP CONSTRAINT content_definitions_kind_valid;
ALTER TABLE lynx.content_bundle_definitions
    ADD CONSTRAINT content_definitions_kind_valid CHECK (
        kind IN ('entity', 'item', 'zone', 'behavior', 'ui', 'minigame', 'map')
    );
