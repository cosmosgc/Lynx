-- 0005_world_events.sql
--
-- Durable world calendar, bounded faction conflicts, enrollment, and
-- deterministic results (ADR 0021). Calendar and conflict state are server
-- state; they never rewrite authored zone content.

CREATE TABLE lynx.world_events (
    event_id text PRIMARY KEY,
    title text NOT NULL,
    description text,
    enrollment_opens_at timestamptz NOT NULL,
    enrollment_closes_at timestamptz NOT NULL,
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    target_zone_id text NOT NULL
        REFERENCES lynx.faction_territories (zone_id) ON DELETE RESTRICT,
    status text NOT NULL DEFAULT 'draft',
    enrollment_requires_faction_membership boolean NOT NULL DEFAULT true,
    enrollment_allow_unaffiliated boolean NOT NULL DEFAULT false,
    enrollment_max_participants_per_side integer,
    conflict_rule_version_major integer NOT NULL DEFAULT 1,
    conflict_rule_version_minor integer NOT NULL DEFAULT 0,
    conflict_metric text NOT NULL DEFAULT 'participants',
    conflict_minimum_participants_per_side integer NOT NULL DEFAULT 0,
    cancellation_policy text NOT NULL DEFAULT 'any_time_before_resolution',
    schema_version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT world_events_id_valid CHECK (
        char_length(event_id) <= 96
        AND event_id ~ '^event\.[a-z0-9][a-z0-9._-]*$'
        AND event_id !~ '\.\.'
        AND event_id !~ '\.$'
    ),
    CONSTRAINT world_events_title_valid CHECK (
        btrim(title) <> ''
        AND title = btrim(title)
        AND char_length(title) <= 128
        AND title !~ '[[:cntrl:]]'
    ),
    CONSTRAINT world_events_description_length CHECK (
        description IS NULL OR char_length(description) <= 1024
    ),
    CONSTRAINT world_events_description_controls CHECK (
        description IS NULL OR description !~ '[[:cntrl:]]'
    ),
    CONSTRAINT world_events_schedule_order CHECK (
        enrollment_opens_at < enrollment_closes_at
        AND enrollment_closes_at <= starts_at
        AND starts_at < ends_at
    ),
    CONSTRAINT world_events_window_length CHECK (
        ends_at - starts_at <= interval '315576000000 milliseconds'
    ),
    CONSTRAINT world_events_status_valid CHECK (
        status IN ('draft', 'scheduled', 'open', 'active', 'suspended', 'cancelled', 'resolved')
    ),
    CONSTRAINT world_events_enrollment_rules_valid CHECK (
        NOT (enrollment_requires_faction_membership AND enrollment_allow_unaffiliated)
        AND (enrollment_max_participants_per_side IS NULL
             OR enrollment_max_participants_per_side > 0)
    ),
    CONSTRAINT world_events_conflict_rule_valid CHECK (
        conflict_rule_version_major >= 1
        AND conflict_rule_version_minor >= 0
        AND conflict_metric IN ('participants', 'faction_power')
        AND conflict_minimum_participants_per_side >= 0
    ),
    CONSTRAINT world_events_cancellation_policy_valid CHECK (
        cancellation_policy IN ('before_start_only', 'any_time_before_resolution')
    ),
    CONSTRAINT world_events_schema_version CHECK (schema_version >= 1)
);

CREATE UNIQUE INDEX world_events_reserved_target
    ON lynx.world_events (target_zone_id)
    WHERE status IN ('scheduled', 'open', 'active', 'suspended');
CREATE INDEX world_events_calendar_idx
    ON lynx.world_events (starts_at, event_id)
    WHERE status <> 'draft';

COMMENT ON TABLE lynx.world_events IS
    'Authored calendar events and the bounded conflict attached to each one.';

CREATE TRIGGER world_events_touch_updated_at
BEFORE UPDATE ON lynx.world_events
FOR EACH ROW EXECUTE FUNCTION lynx.touch_updated_at();

CREATE TABLE lynx.world_event_sides (
    event_id text NOT NULL
        REFERENCES lynx.world_events (event_id) ON DELETE CASCADE,
    side_id text NOT NULL,
    role text NOT NULL,
    display_name text NOT NULL,
    faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    PRIMARY KEY (event_id, side_id),
    CONSTRAINT world_event_sides_id_valid CHECK (
        char_length(side_id) <= 96
        AND side_id ~ '^side\.[a-z0-9][a-z0-9._-]*$'
        AND side_id !~ '\.\.'
        AND side_id !~ '\.$'
    ),
    CONSTRAINT world_event_sides_role_valid CHECK (role IN ('attacker', 'defender')),
    CONSTRAINT world_event_sides_name_valid CHECK (
        btrim(display_name) <> ''
        AND display_name = btrim(display_name)
        AND char_length(display_name) <= 128
        AND display_name !~ '[[:cntrl:]]'
    ),
    UNIQUE (event_id, faction_id)
);

CREATE UNIQUE INDEX world_event_sides_one_attacker
    ON lynx.world_event_sides (event_id)
    WHERE role = 'attacker';
CREATE UNIQUE INDEX world_event_sides_one_defender
    ON lynx.world_event_sides (event_id)
    WHERE role = 'defender';

COMMENT ON TABLE lynx.world_event_sides IS
    'Exactly one attacker and one defender faction per bounded conflict.';

CREATE FUNCTION lynx.is_world_event_transition(
    action text,
    from_status text,
    to_status text
) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE action
        WHEN 'created' THEN from_status IS NULL AND to_status = 'draft'
        WHEN 'scheduled' THEN from_status = 'draft' AND to_status = 'scheduled'
        WHEN 'opened' THEN from_status = 'scheduled' AND to_status = 'open'
        WHEN 'activated' THEN from_status = 'open' AND to_status = 'active'
        WHEN 'suspended' THEN from_status = 'active' AND to_status = 'suspended'
        WHEN 'resumed' THEN from_status = 'suspended' AND to_status = 'active'
        WHEN 'cancelled' THEN from_status IN (
            'draft', 'scheduled', 'open', 'active', 'suspended'
        ) AND to_status = 'cancelled'
        WHEN 'resolved' THEN from_status = 'active' AND to_status = 'resolved'
        ELSE false
    END;
$$;

COMMENT ON FUNCTION lynx.is_world_event_transition IS
    'The single allowed lifecycle transition table shared by Rust and SQL.';

CREATE TABLE lynx.world_event_transitions (
    transition_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transition_key text NOT NULL UNIQUE,
    event_id text NOT NULL
        REFERENCES lynx.world_events (event_id) ON DELETE CASCADE,
    action text NOT NULL,
    from_status text,
    to_status text NOT NULL,
    actor_id text NOT NULL,
    occurred_at timestamptz NOT NULL,
    reason text,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT world_event_transitions_key_valid CHECK (
        transition_key <> ''
        AND char_length(transition_key) <= 128
        AND transition_key !~ '[[:cntrl:]]'
    ),
    CONSTRAINT world_event_transitions_action_valid CHECK (
        action IN (
            'created', 'scheduled', 'opened', 'activated',
            'suspended', 'resumed', 'cancelled', 'resolved'
        )
    ),
    CONSTRAINT world_event_transitions_status_valid CHECK (
        (from_status IS NULL OR from_status IN (
            'draft', 'scheduled', 'open', 'active', 'suspended', 'cancelled', 'resolved'
        ))
        AND to_status IN (
            'draft', 'scheduled', 'open', 'active', 'suspended', 'cancelled', 'resolved'
        )
    ),
    CONSTRAINT world_event_transitions_valid CHECK (
        lynx.is_world_event_transition(action, from_status, to_status)
    ),
    CONSTRAINT world_event_transitions_actor_valid CHECK (
        actor_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT world_event_transitions_reason_valid CHECK (
        reason IS NULL OR (
            btrim(reason) <> ''
            AND char_length(reason) <= 512
            AND reason !~ '[[:cntrl:]]'
        )
    ),
    CONSTRAINT world_event_transitions_reason_required CHECK (
        action NOT IN ('suspended', 'cancelled') OR reason IS NOT NULL
    ),
    CONSTRAINT world_event_transitions_schema_version CHECK (schema_version >= 1)
);

CREATE INDEX world_event_transitions_event_idx
    ON lynx.world_event_transitions (event_id, transition_id);

COMMENT ON TABLE lynx.world_event_transitions IS
    'Append-only lifecycle audit chain for every calendar event.';

CREATE TRIGGER world_event_transitions_append_only
BEFORE UPDATE OR DELETE ON lynx.world_event_transitions
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE TABLE lynx.world_event_enrollments (
    event_id text NOT NULL,
    participant_id text NOT NULL,
    side_id text NOT NULL,
    faction_id text
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    enrolled_at timestamptz NOT NULL,
    schema_version integer NOT NULL DEFAULT 1,
    PRIMARY KEY (event_id, participant_id),
    CONSTRAINT world_event_enrollments_side_fk
        FOREIGN KEY (event_id, side_id)
        REFERENCES lynx.world_event_sides (event_id, side_id)
        ON DELETE CASCADE,
    CONSTRAINT world_event_enrollments_participant_valid CHECK (
        participant_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT world_event_enrollments_schema_version CHECK (schema_version >= 1)
);

CREATE INDEX world_event_enrollments_side_idx
    ON lynx.world_event_enrollments (event_id, side_id);

COMMENT ON TABLE lynx.world_event_enrollments IS
    'Immutable participant-to-side enrollments; a participant may not change sides.';

CREATE TRIGGER world_event_enrollments_append_only
BEFORE UPDATE OR DELETE ON lynx.world_event_enrollments
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE TABLE lynx.world_event_results (
    event_id text PRIMARY KEY
        REFERENCES lynx.world_events (event_id) ON DELETE CASCADE,
    resolver_id text NOT NULL,
    resolved_at timestamptz NOT NULL,
    rule_version_major integer NOT NULL,
    rule_version_minor integer NOT NULL,
    metric text NOT NULL,
    minimum_participants_per_side integer NOT NULL,
    winning_side_id text NOT NULL,
    winning_faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    outcome text NOT NULL,
    territory_change_key text UNIQUE
        REFERENCES lynx.faction_territory_history (change_key) ON DELETE RESTRICT,
    schema_version integer NOT NULL DEFAULT 1,
    CONSTRAINT world_event_results_resolver_valid CHECK (
        resolver_id ~ '^(chr|ent)_[0-9A-HJKMNP-TV-Z]{26}$'
    ),
    CONSTRAINT world_event_results_rule_valid CHECK (
        rule_version_major >= 1
        AND rule_version_minor >= 0
        AND metric IN ('participants', 'faction_power')
        AND minimum_participants_per_side >= 0
    ),
    CONSTRAINT world_event_results_outcome_valid CHECK (
        outcome IN ('attacker_victory', 'defender_victory')
    ),
    CONSTRAINT world_event_results_territory_consistent CHECK (
        (outcome = 'attacker_victory' AND territory_change_key IS NOT NULL)
        OR (outcome = 'defender_victory' AND territory_change_key IS NULL)
    ),
    CONSTRAINT world_event_results_schema_version CHECK (schema_version >= 1),
    CONSTRAINT world_event_results_winning_side_fk
        FOREIGN KEY (event_id, winning_side_id)
        REFERENCES lynx.world_event_sides (event_id, side_id)
        ON DELETE CASCADE
);

COMMENT ON TABLE lynx.world_event_results IS
    'One durable, idempotent conflict result per resolved event.';

CREATE TRIGGER world_event_results_append_only
BEFORE UPDATE OR DELETE ON lynx.world_event_results
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE TABLE lynx.world_event_result_sides (
    event_id text NOT NULL,
    side_id text NOT NULL,
    role text NOT NULL,
    faction_id text NOT NULL
        REFERENCES lynx.factions (faction_id) ON DELETE RESTRICT,
    participants integer NOT NULL,
    faction_power numeric(20,0) NOT NULL,
    score numeric(20,0) NOT NULL,
    PRIMARY KEY (event_id, side_id),
    CONSTRAINT world_event_result_sides_event_fk
        FOREIGN KEY (event_id)
        REFERENCES lynx.world_event_results (event_id)
        ON DELETE CASCADE,
    CONSTRAINT world_event_result_sides_side_fk
        FOREIGN KEY (event_id, side_id)
        REFERENCES lynx.world_event_sides (event_id, side_id)
        ON DELETE CASCADE,
    CONSTRAINT world_event_result_sides_role_valid CHECK (role IN ('attacker', 'defender')),
    CONSTRAINT world_event_result_sides_counts_valid CHECK (
        participants >= 0 AND faction_power >= 0 AND score >= 0
    )
);

COMMENT ON TABLE lynx.world_event_result_sides IS
    'Recorded inputs and scores for both sides of a resolved conflict.';

CREATE TRIGGER world_event_result_sides_append_only
BEFORE UPDATE OR DELETE ON lynx.world_event_result_sides
FOR EACH ROW EXECUTE FUNCTION lynx.reject_append_only_mutation();

CREATE FUNCTION lynx.assert_world_event_sides(target text) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    total integer;
    attackers integer;
    defenders integer;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM lynx.world_events WHERE event_id = target
    ) THEN
        RETURN;
    END IF;
    SELECT
        count(*),
        count(*) FILTER (WHERE role = 'attacker'),
        count(*) FILTER (WHERE role = 'defender')
    INTO total, attackers, defenders
    FROM lynx.world_event_sides
    WHERE event_id = target;
    IF total <> 2 OR attackers <> 1 OR defenders <> 1 THEN
        RAISE EXCEPTION
            'event % must have exactly two sides: one attacker and one defender', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

CREATE FUNCTION lynx.require_world_event_sides() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM lynx.assert_world_event_sides(OLD.event_id);
    ELSE
        PERFORM lynx.assert_world_event_sides(NEW.event_id);
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER world_event_sides_bounded
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_event_sides
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_sides();

CREATE FUNCTION lynx.assert_world_event_enrollment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    side_faction text;
    requires_membership boolean;
    allows_unaffiliated boolean;
BEGIN
    SELECT
        s.faction_id,
        e.enrollment_requires_faction_membership,
        e.enrollment_allow_unaffiliated
    INTO side_faction, requires_membership, allows_unaffiliated
    FROM lynx.world_event_sides s
    JOIN lynx.world_events e ON e.event_id = s.event_id
    WHERE s.event_id = NEW.event_id AND s.side_id = NEW.side_id;

    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    IF requires_membership AND NEW.faction_id IS DISTINCT FROM side_faction THEN
        RAISE EXCEPTION
            'event % participant % must belong to side faction %',
            NEW.event_id, NEW.participant_id, side_faction
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.faction_id IS NULL AND NOT allows_unaffiliated THEN
        RAISE EXCEPTION
            'event % does not allow unaffiliated participant %',
            NEW.event_id, NEW.participant_id
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.faction_id IS NOT NULL AND EXISTS (
        SELECT 1 FROM lynx.world_event_sides
        WHERE event_id = NEW.event_id
          AND faction_id = NEW.faction_id
          AND side_id <> NEW.side_id
    ) THEN
        RAISE EXCEPTION
            'event % participant faction fights on the other side', NEW.event_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER world_event_enrollments_valid
AFTER INSERT ON lynx.world_event_enrollments
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.assert_world_event_enrollment();

CREATE FUNCTION lynx.assert_world_event_lifecycle(target text) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    current_status text;
    first_action text;
    first_from text;
    first_to text;
    latest_to text;
BEGIN
    SELECT status INTO current_status
    FROM lynx.world_events WHERE event_id = target;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT action, from_status, to_status
    INTO first_action, first_from, first_to
    FROM lynx.world_event_transitions
    WHERE event_id = target
    ORDER BY transition_id
    LIMIT 1;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'event % has no lifecycle history', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF first_action <> 'created' OR first_from IS NOT NULL OR first_to <> 'draft' THEN
        RAISE EXCEPTION 'event % history must begin with creation', target
            USING ERRCODE = 'check_violation';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM (
            SELECT
                from_status,
                to_status,
                lag(to_status) OVER (ORDER BY transition_id) AS previous_to,
                occurred_at,
                lag(occurred_at) OVER (ORDER BY transition_id) AS previous_at
            FROM lynx.world_event_transitions
            WHERE event_id = target
        ) chain
        WHERE previous_to IS DISTINCT FROM from_status
           OR (previous_at IS NOT NULL AND occurred_at < previous_at)
    ) THEN
        RAISE EXCEPTION 'event % history is not a contiguous transition chain', target
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT to_status INTO latest_to
    FROM lynx.world_event_transitions
    WHERE event_id = target
    ORDER BY transition_id DESC
    LIMIT 1;
    IF latest_to IS DISTINCT FROM current_status THEN
        RAISE EXCEPTION 'event % status does not match its lifecycle history', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

CREATE FUNCTION lynx.require_world_event_lifecycle() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'world_events' THEN
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_world_event_lifecycle(OLD.event_id);
        ELSE
            PERFORM lynx.assert_world_event_lifecycle(NEW.event_id);
        END IF;
    ELSE
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_world_event_lifecycle(OLD.event_id);
        ELSE
            PERFORM lynx.assert_world_event_lifecycle(NEW.event_id);
        END IF;
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER world_events_lifecycle_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_events
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_lifecycle();

CREATE CONSTRAINT TRIGGER world_event_transitions_lifecycle_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_event_transitions
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_lifecycle();

CREATE FUNCTION lynx.assert_world_event_result(target text) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    status text;
    winning_role text;
    outcome text;
    territory_change_key text;
    result_sides integer;
BEGIN
    SELECT status INTO status
    FROM lynx.world_events WHERE event_id = target;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF status = 'resolved' THEN
        IF NOT EXISTS (
            SELECT 1 FROM lynx.world_event_results WHERE event_id = target
        ) THEN
            RAISE EXCEPTION 'resolved event % is missing its conflict result', target
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM lynx.world_event_results WHERE event_id = target
    ) THEN
        RAISE EXCEPTION 'unresolved event % has a conflict result', target
            USING ERRCODE = 'check_violation';
    ELSE
        RETURN;
    END IF;

    SELECT outcome, territory_change_key
    INTO outcome, territory_change_key
    FROM lynx.world_event_results
    WHERE event_id = target;

    SELECT s.role INTO winning_role
    FROM lynx.world_event_results r
    JOIN lynx.world_event_sides s
      ON s.event_id = r.event_id AND s.side_id = r.winning_side_id
    WHERE r.event_id = target;

    IF winning_role IS NULL THEN
        RAISE EXCEPTION 'event % winning side is missing', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF (winning_role = 'attacker' AND outcome <> 'attacker_victory')
        OR (winning_role = 'defender' AND outcome <> 'defender_victory') THEN
        RAISE EXCEPTION 'event % outcome does not match its winning side', target
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT count(*) INTO result_sides
    FROM lynx.world_event_result_sides WHERE event_id = target;
    IF result_sides <> 2 THEN
        RAISE EXCEPTION 'event % result must record both sides', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM lynx.world_event_result_sides r
        LEFT JOIN lynx.world_event_sides s
          ON s.event_id = r.event_id AND s.side_id = r.side_id
        WHERE r.event_id = target
          AND (s.side_id IS NULL
               OR s.faction_id <> r.faction_id
               OR s.role <> r.role)
    ) THEN
        RAISE EXCEPTION 'event % result side does not match the event sides', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM lynx.world_event_result_sides r
        WHERE r.event_id = target
          AND r.participants <> (
              SELECT count(*) FROM lynx.world_event_enrollments e
              WHERE e.event_id = r.event_id AND e.side_id = r.side_id
          )
    ) THEN
        RAISE EXCEPTION 'event % result participant count does not match enrollment', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF outcome = 'attacker_victory' AND territory_change_key IS NULL THEN
        RAISE EXCEPTION 'attacker victory for event % has no territory change', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;

CREATE FUNCTION lynx.require_world_event_result() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'world_events' THEN
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_world_event_result(OLD.event_id);
        ELSE
            PERFORM lynx.assert_world_event_result(NEW.event_id);
        END IF;
    ELSIF TG_TABLE_NAME = 'world_event_sides' THEN
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_world_event_result(OLD.event_id);
        ELSE
            PERFORM lynx.assert_world_event_result(NEW.event_id);
        END IF;
    ELSE
        IF TG_OP = 'DELETE' THEN
            PERFORM lynx.assert_world_event_result(OLD.event_id);
        ELSE
            PERFORM lynx.assert_world_event_result(NEW.event_id);
        END IF;
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER world_events_result_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_events
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_result();

CREATE CONSTRAINT TRIGGER world_event_sides_result_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_event_sides
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_result();

CREATE CONSTRAINT TRIGGER world_event_results_result_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_event_results
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_result();

CREATE CONSTRAINT TRIGGER world_event_result_sides_result_consistent
AFTER INSERT OR UPDATE OR DELETE ON lynx.world_event_result_sides
DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION lynx.require_world_event_result();

CREATE VIEW lynx.world_event_calendar AS
SELECT
    e.event_id,
    e.title,
    e.description,
    e.status,
    e.target_zone_id,
    e.enrollment_opens_at,
    e.enrollment_closes_at,
    e.starts_at,
    e.ends_at,
    e.enrollment_requires_faction_membership,
    e.enrollment_allow_unaffiliated,
    e.enrollment_max_participants_per_side,
    e.conflict_metric,
    e.cancellation_policy
FROM lynx.world_events e
WHERE e.status <> 'draft';

COMMENT ON VIEW lynx.world_event_calendar IS
    'Player-facing calendar; drafts are excluded.';

CREATE VIEW lynx.world_event_participants AS
SELECT
    e.event_id,
    e.title,
    e.status,
    s.side_id,
    s.role,
    s.faction_id AS side_faction_id,
    en.participant_id,
    en.faction_id AS participant_faction_id,
    en.enrolled_at
FROM lynx.world_event_enrollments en
JOIN lynx.world_events e ON e.event_id = en.event_id
JOIN lynx.world_event_sides s
  ON s.event_id = en.event_id AND s.side_id = en.side_id;

COMMENT ON VIEW lynx.world_event_participants IS
    'Current enrollment with side and participant faction IDs.';
