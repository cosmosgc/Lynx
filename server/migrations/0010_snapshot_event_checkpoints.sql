-- 0010_snapshot_event_checkpoints.sql
-- Pin each fenced world snapshot row to the exact event that committed with it.

ALTER TABLE lynx.world_snapshots
    ADD COLUMN event_sequence bigint;

ALTER TABLE lynx.world_snapshots
    ADD CONSTRAINT world_snapshots_event_sequence_valid CHECK (
        event_sequence IS NULL OR event_sequence >= 0
    );

WITH latest_checkpoints AS (
    SELECT DISTINCT ON (payload->>'world_id')
        payload->>'world_id' AS world_id,
        sequence,
        payload->>'snapshot_hash' AS snapshot_hash
    FROM lynx.zone_events
    WHERE kind = 'world.snapshot'
    ORDER BY payload->>'world_id', sequence DESC
)
UPDATE lynx.world_snapshots AS snapshots
SET event_sequence = checkpoints.sequence
FROM latest_checkpoints AS checkpoints
WHERE checkpoints.world_id = snapshots.world_id
  AND checkpoints.snapshot_hash = snapshots.payload_hash;

COMMENT ON COLUMN lynx.world_snapshots.event_sequence IS
    'Sequence of the atomic world.snapshot event; NULL identifies a legacy or unfenced snapshot.';
