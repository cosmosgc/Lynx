-- 0002_content.sql
--
-- Immutable content bundles, their definition and locale indexes, and the
-- append-only moderation history. Bundle JSON mirrors the manifest written by
-- `lynx content hash`.

CREATE TABLE lynx.content_bundles (
    bundle_id text PRIMARY KEY,
    manifest_hash text NOT NULL,
    engine_requirement text NOT NULL,
    manifest jsonb NOT NULL,
    status text NOT NULL DEFAULT 'stored',
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    activated_at timestamptz,
    CONSTRAINT content_bundles_version_semver CHECK (
        bundle_id ~ '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
    ),
    CONSTRAINT content_bundles_hash_sha256 CHECK (
        manifest_hash ~ '^[0-9a-f]{64}$'
    ),
    CONSTRAINT content_bundles_engine_valid CHECK (
        engine_requirement <> '' AND char_length(engine_requirement) <= 128
    ),
    CONSTRAINT content_bundles_manifest_object CHECK (
        jsonb_typeof(manifest) = 'object'
    ),
    CONSTRAINT content_bundles_status_valid CHECK (
        status IN ('stored', 'active', 'superseded', 'rejected')
    ),
    CONSTRAINT content_bundles_author_valid CHECK (
        created_by <> '' AND char_length(created_by) <= 128
        AND created_by !~ '[[:space:][:cntrl:]]'
    ),
    CONSTRAINT content_bundles_activation_consistent CHECK (
        (status = 'active') = (activated_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX content_bundles_single_active
    ON lynx.content_bundles ((1))
    WHERE status = 'active';

COMMENT ON TABLE lynx.content_bundles IS
    'Immutable manifest snapshots. At most one bundle may be active at a time.';

CREATE TABLE lynx.content_bundle_definitions (
    bundle_id text NOT NULL
        REFERENCES lynx.content_bundles (bundle_id) ON DELETE CASCADE,
    path text NOT NULL,
    kind text NOT NULL,
    definition_id text NOT NULL,
    definition_version text NOT NULL,
    sha256 text NOT NULL,
    PRIMARY KEY (bundle_id, path),
    CONSTRAINT content_definitions_path_safe CHECK (
        path <> ''
        AND char_length(path) <= 512
        AND path !~ '^/'
        AND path !~ '\.\.'
        AND path !~ '\\'
    ),
    CONSTRAINT content_definitions_kind_valid CHECK (
        kind IN ('entity', 'item', 'zone', 'behavior', 'ui', 'minigame')
    ),
    CONSTRAINT content_definitions_id_valid CHECK (
        char_length(definition_id) <= 96
        AND definition_id ~ '^[a-z0-9][a-z0-9._-]*$'
        AND definition_id !~ '\.\.'
        AND definition_id !~ '\.$'
    ),
    CONSTRAINT content_definitions_version_semver CHECK (
        definition_version ~ '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
    ),
    CONSTRAINT content_definitions_hash_sha256 CHECK (
        sha256 ~ '^[0-9a-f]{64}$'
    )
);

CREATE INDEX content_definitions_id_idx
    ON lynx.content_bundle_definitions (definition_id, definition_version);

COMMENT ON TABLE lynx.content_bundle_definitions IS
    'Per-file index of a bundle; content bytes live in the published artifact store.';

CREATE TABLE lynx.content_bundle_locales (
    bundle_id text NOT NULL
        REFERENCES lynx.content_bundles (bundle_id) ON DELETE CASCADE,
    locale text NOT NULL,
    path text NOT NULL,
    sha256 text NOT NULL,
    PRIMARY KEY (bundle_id, locale),
    CONSTRAINT content_locales_tag_valid CHECK (
        locale ~ '^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$'
    ),
    CONSTRAINT content_locales_path_safe CHECK (
        path <> ''
        AND char_length(path) <= 512
        AND path !~ '^/'
        AND path !~ '\.\.'
        AND path !~ '\\'
    ),
    CONSTRAINT content_locales_hash_sha256 CHECK (
        sha256 ~ '^[0-9a-f]{64}$'
    )
);

COMMENT ON TABLE lynx.content_bundle_locales IS
    'Localization catalogs shipped with a bundle, one row per BCP 47 tag.';

CREATE TABLE lynx.content_moderation_events (
    event_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bundle_id text NOT NULL
        REFERENCES lynx.content_bundles (bundle_id) ON DELETE CASCADE,
    revision bigint NOT NULL,
    state text NOT NULL,
    author_id text NOT NULL,
    reviewer_id text,
    note text,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT moderation_revision_valid CHECK (revision >= 0),
    CONSTRAINT moderation_state_valid CHECK (
        state IN ('draft', 'in_review', 'approved', 'rejected', 'withdrawn')
    ),
    CONSTRAINT moderation_author_valid CHECK (
        author_id <> '' AND char_length(author_id) <= 128
        AND author_id !~ '[[:space:][:cntrl:]]'
    ),
    CONSTRAINT moderation_reviewer_valid CHECK (
        reviewer_id IS NULL
        OR (reviewer_id <> '' AND char_length(reviewer_id) <= 128
            AND reviewer_id !~ '[[:space:][:cntrl:]]')
    ),
    CONSTRAINT moderation_note_length CHECK (
        note IS NULL OR char_length(note) <= 512
    ),
    CONSTRAINT moderation_review_requires_reviewer CHECK (
        state NOT IN ('approved', 'rejected') OR reviewer_id IS NOT NULL
    ),
    CONSTRAINT moderation_rejection_requires_note CHECK (
        state <> 'rejected' OR note IS NOT NULL
    )
);

CREATE UNIQUE INDEX content_moderation_revision_unique
    ON lynx.content_moderation_events (bundle_id, revision);
CREATE INDEX content_moderation_recent_idx
    ON lynx.content_moderation_events (bundle_id, occurred_at DESC);

COMMENT ON TABLE lynx.content_moderation_events IS
    'Append-only moderation history; the current state is the newest revision.';

CREATE TRIGGER content_moderation_append_only
BEFORE UPDATE OR DELETE ON lynx.content_moderation_events
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();
