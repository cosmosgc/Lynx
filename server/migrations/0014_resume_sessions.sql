-- Durable resume-session metadata. Bearer tokens are never stored here; only
-- fixed-size SHA-256 digests cross the persistence boundary.
CREATE TABLE lynx.resume_sessions (
    session_id text PRIMARY KEY,
    account_id text NOT NULL,
    principal_id text NOT NULL,
    actor_id text NOT NULL,
    token_digest bytea NOT NULL UNIQUE CHECK (octet_length(token_digest) = 32),
    expires_at_ms bigint,
    connected boolean NOT NULL,
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    revoked boolean NOT NULL DEFAULT false,
    CHECK ((connected AND expires_at_ms IS NULL) OR (NOT connected AND expires_at_ms IS NOT NULL) OR revoked)
);

CREATE INDEX resume_sessions_expiry_idx
    ON lynx.resume_sessions (expires_at_ms)
    WHERE NOT connected AND NOT revoked;
