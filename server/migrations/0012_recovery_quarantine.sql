-- 0012_recovery_quarantine.sql
-- Durable evidence for saved entries isolated during recovery. A quarantined
-- entity is skipped from the adopted world but never silently dropped: the
-- report names its section, stable failure code, and explanation so operators
-- can inspect and repair the source snapshot.

CREATE TABLE lynx.recovery_quarantine (
    world_id text NOT NULL,
    zone_id text NOT NULL,
    entity_id text NOT NULL,
    section text NOT NULL,
    code text NOT NULL,
    message text NOT NULL,
    detected_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (world_id, zone_id, entity_id, section, code),
    CONSTRAINT recovery_quarantine_world_valid CHECK (
        world_id ~ '^[a-z0-9][a-z0-9._-]{0,63}$'
    ),
    CONSTRAINT recovery_quarantine_zone_prefix CHECK (
        zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT recovery_quarantine_entity_valid CHECK (
        entity_id = 'unknown'
        OR entity_id ~ '^(ent|chr|zone|prop|itm)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT recovery_quarantine_section_valid CHECK (
        section <> '' AND char_length(section) <= 64
    ),
    CONSTRAINT recovery_quarantine_code_valid CHECK (
        code <> '' AND char_length(code) <= 64
    ),
    CONSTRAINT recovery_quarantine_message_valid CHECK (
        message <> '' AND char_length(message) <= 2048
    )
);

CREATE INDEX recovery_quarantine_recent_idx
    ON lynx.recovery_quarantine (world_id, zone_id, detected_at DESC);

COMMENT ON TABLE lynx.recovery_quarantine IS
    'Entries isolated during snapshot recovery, with a stable code and explanation. Repeats refresh detected_at instead of growing without bound.';
