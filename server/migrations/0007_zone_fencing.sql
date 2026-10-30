-- 0007_zone_fencing.sql
-- A durable generation fences writers across lease expiry and takeover.

ALTER TABLE lynx.zone_leases
    ADD COLUMN fencing_token bigint NOT NULL DEFAULT 1,
    ADD CONSTRAINT zone_leases_fencing_token_valid CHECK (fencing_token > 0);

COMMENT ON COLUMN lynx.zone_leases.fencing_token IS
    'Monotonically increasing generation; authoritative writes must present the current token.';
