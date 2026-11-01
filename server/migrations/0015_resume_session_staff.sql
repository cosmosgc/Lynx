-- Preserve the authentication-time staff claim across a process restart.
-- Existing 0014 rows do not contain trustworthy staff metadata, so revoke
-- those credentials rather than guessing whether they carried privileges.
UPDATE lynx.resume_sessions
SET revoked = true, connected = false, expires_at_ms = NULL, generation = generation + 1;

ALTER TABLE lynx.resume_sessions
    ADD COLUMN staff boolean NOT NULL DEFAULT false;
