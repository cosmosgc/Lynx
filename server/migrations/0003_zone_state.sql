-- 0003_zone_state.sql
--
-- Single-writer zone leases, periodic snapshots, and the append-only event log
-- used for recovery within the target window (ADR 0005).

CREATE TABLE lynx.zone_leases (
    zone_id text PRIMARY KEY,
    owner text NOT NULL,
    acquired_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CONSTRAINT zone_leases_zone_prefix CHECK (
        zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_leases_owner_valid CHECK (
        owner <> '' AND char_length(owner) <= 256
    ),
    CONSTRAINT zone_leases_expiry_order CHECK (expires_at > acquired_at)
);

COMMENT ON TABLE lynx.zone_leases IS
    'One live owner per zone. Expired leases may be taken over after a crash.';

CREATE TABLE lynx.zone_snapshots (
    zone_id text NOT NULL,
    tick bigint NOT NULL,
    taken_at timestamptz NOT NULL DEFAULT now(),
    schema_version integer NOT NULL,
    payload jsonb NOT NULL,
    payload_hash text NOT NULL,
    PRIMARY KEY (zone_id, tick),
    CONSTRAINT zone_snapshots_zone_prefix CHECK (
        zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_snapshots_tick_valid CHECK (tick >= 0),
    CONSTRAINT zone_snapshots_schema_version CHECK (schema_version >= 1),
    CONSTRAINT zone_snapshots_payload_object CHECK (
        jsonb_typeof(payload) = 'object'
    ),
    CONSTRAINT zone_snapshots_hash_sha256 CHECK (
        payload_hash ~ '^[0-9a-f]{64}$'
    )
);

CREATE INDEX zone_snapshots_recent_idx
    ON lynx.zone_snapshots (zone_id, tick DESC);

COMMENT ON TABLE lynx.zone_snapshots IS
    'Periodic authoritative snapshots; transforms are not persisted every tick.';

CREATE TABLE lynx.zone_events (
    zone_id text NOT NULL,
    sequence bigint NOT NULL,
    tick bigint NOT NULL,
    occurred_at timestamptz NOT NULL,
    kind text NOT NULL,
    actor_id text,
    payload jsonb NOT NULL,
    schema_version integer NOT NULL,
    PRIMARY KEY (zone_id, sequence),
    CONSTRAINT zone_events_zone_prefix CHECK (
        zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_events_sequence_valid CHECK (sequence >= 0),
    CONSTRAINT zone_events_tick_valid CHECK (tick >= 0),
    CONSTRAINT zone_events_kind_valid CHECK (
        kind <> '' AND char_length(kind) <= 128
    ),
    CONSTRAINT zone_events_actor_prefix CHECK (
        actor_id IS NULL
        OR actor_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT zone_events_payload_object CHECK (
        jsonb_typeof(payload) = 'object'
    ),
    CONSTRAINT zone_events_schema_version CHECK (schema_version >= 1)
);

CREATE INDEX zone_events_tick_idx
    ON lynx.zone_events (zone_id, tick, sequence);

COMMENT ON TABLE lynx.zone_events IS
    'Append-only zone event log replayed after the last snapshot on recovery.';

CREATE TRIGGER zone_events_append_only
BEFORE UPDATE OR DELETE ON lynx.zone_events
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();
