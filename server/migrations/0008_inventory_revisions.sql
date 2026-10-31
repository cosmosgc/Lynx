-- 0008_inventory_revisions.sql
-- Optimistic aggregate revisions keep concurrent inventory replacements from
-- silently overwriting one another, including initially empty inventories.

CREATE TABLE lynx.inventory_revisions (
    owner_id text PRIMARY KEY,
    version bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT inventory_revisions_owner_prefix CHECK (
        owner_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT inventory_revisions_version_nonnegative CHECK (version >= 0)
);

COMMENT ON TABLE lynx.inventory_revisions IS
    'Optimistic aggregate versions for inventory snapshots, including empty inventories.';
