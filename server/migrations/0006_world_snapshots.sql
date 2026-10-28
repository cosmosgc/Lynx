-- 0006_world_snapshots.sql
--
-- Whole-world snapshots for the runnable world service. Unlike
-- lynx.zone_snapshots, which keeps per-zone history, this table holds the one
-- cross-validated aggregate that a single world process resumes from: the zone
-- plus its calendar and faction snapshots. The payload is the serialized
-- WorldSnapshot and is replaced atomically by an upsert, so a crash leaves the
-- previous good world in place.

CREATE TABLE lynx.world_snapshots (
    world_id text PRIMARY KEY,
    schema_major integer NOT NULL,
    schema_minor integer NOT NULL,
    saved_at_ms bigint NOT NULL,
    payload jsonb NOT NULL,
    payload_hash text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT world_snapshots_id_valid CHECK (
        world_id ~ '^[a-z0-9][a-z0-9._-]{0,63}$'
    ),
    CONSTRAINT world_snapshots_schema_major CHECK (schema_major >= 1),
    CONSTRAINT world_snapshots_schema_minor CHECK (schema_minor >= 0),
    CONSTRAINT world_snapshots_saved_at_valid CHECK (saved_at_ms >= 0),
    CONSTRAINT world_snapshots_payload_object CHECK (
        jsonb_typeof(payload) = 'object'
    ),
    CONSTRAINT world_snapshots_hash_sha256 CHECK (
        payload_hash ~ '^[0-9a-f]{64}$'
    )
);

CREATE TRIGGER world_snapshots_touch_updated_at
BEFORE UPDATE ON lynx.world_snapshots
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

COMMENT ON TABLE lynx.world_snapshots IS
    'One resumable whole-world snapshot per world; replaced atomically by an upsert.';
