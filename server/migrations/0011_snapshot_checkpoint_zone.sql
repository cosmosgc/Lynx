-- 0011_snapshot_checkpoint_zone.sql
-- Event sequences are zone-local, so pin each checkpoint to its owning zone.

ALTER TABLE lynx.world_snapshots
    ADD COLUMN checkpoint_zone_id text;

ALTER TABLE lynx.world_snapshots
    ADD CONSTRAINT world_snapshots_checkpoint_zone_valid CHECK (
        checkpoint_zone_id IS NULL
        OR checkpoint_zone_id ~ '^zone_[0-9A-HJKMNP-TV-Z]{26}$'
    );

WITH candidate_zones AS (
    SELECT DISTINCT snapshots.world_id, events.zone_id
    FROM lynx.world_snapshots AS snapshots
    JOIN lynx.zone_events AS events
      ON events.sequence = snapshots.event_sequence
     AND events.kind = 'world.snapshot'
     AND events.payload->>'world_id' = snapshots.world_id
     AND events.payload->>'snapshot_hash' = snapshots.payload_hash
    WHERE snapshots.event_sequence IS NOT NULL
), unique_zone_matches AS (
    SELECT world_id, MIN(zone_id) AS zone_id
    FROM candidate_zones
    GROUP BY world_id
    HAVING COUNT(*) = 1
)
UPDATE lynx.world_snapshots AS snapshots
SET checkpoint_zone_id = matches.zone_id
FROM unique_zone_matches AS matches
WHERE matches.world_id = snapshots.world_id;

COMMENT ON COLUMN lynx.world_snapshots.checkpoint_zone_id IS
    'Zone owning the event_sequence checkpoint; NULL for legacy or ambiguous historical rows.';
