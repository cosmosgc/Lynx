-- Multi-zone resume tokens need a stable owner so routing can preflight the
-- zone before atomically consuming a token. Existing rows predate that
-- contract and are revoked rather than assigned to an arbitrary zone.
UPDATE lynx.resume_sessions
SET revoked = true,
    connected = false,
    expires_at_ms = NULL,
    generation = generation + 1;

ALTER TABLE lynx.resume_sessions
    ADD COLUMN zone_id text NOT NULL DEFAULT '';

ALTER TABLE lynx.resume_sessions
    ALTER COLUMN zone_id DROP DEFAULT;

CREATE INDEX resume_sessions_zone_connected_idx
    ON lynx.resume_sessions (zone_id, connected)
    WHERE NOT revoked;
