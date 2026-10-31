-- 0013_fix_event_result_trigger.sql
-- Qualify columns and use distinct PL/pgSQL variable names in the deferred
-- result assertion. The original function's `status`/`outcome` variables
-- shadowed identically named columns, causing valid event updates to fail at
-- commit with SQLSTATE 42702.

CREATE OR REPLACE FUNCTION lynx.assert_world_event_result(target text) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    event_status text;
    winning_role text;
    result_outcome text;
    result_territory_change_key text;
    result_sides integer;
BEGIN
    SELECT event_row.status INTO event_status
    FROM lynx.world_events AS event_row
    WHERE event_row.event_id = target;
    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF event_status = 'resolved' THEN
        IF NOT EXISTS (
            SELECT 1 FROM lynx.world_event_results AS result_row
            WHERE result_row.event_id = target
        ) THEN
            RAISE EXCEPTION 'resolved event % is missing its conflict result', target
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM lynx.world_event_results AS result_row
        WHERE result_row.event_id = target
    ) THEN
        RAISE EXCEPTION 'unresolved event % has a conflict result', target
            USING ERRCODE = 'check_violation';
    ELSE
        RETURN;
    END IF;

    SELECT result_row.outcome, result_row.territory_change_key
    INTO result_outcome, result_territory_change_key
    FROM lynx.world_event_results AS result_row
    WHERE result_row.event_id = target;

    SELECT side_row.role INTO winning_role
    FROM lynx.world_event_results AS result_row
    JOIN lynx.world_event_sides AS side_row
      ON side_row.event_id = result_row.event_id
     AND side_row.side_id = result_row.winning_side_id
    WHERE result_row.event_id = target;

    IF winning_role IS NULL THEN
        RAISE EXCEPTION 'event % winning side is missing', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF (winning_role = 'attacker' AND result_outcome <> 'attacker_victory')
        OR (winning_role = 'defender' AND result_outcome <> 'defender_victory') THEN
        RAISE EXCEPTION 'event % outcome does not match its winning side', target
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT count(*) INTO result_sides
    FROM lynx.world_event_result_sides AS result_side
    WHERE result_side.event_id = target;
    IF result_sides <> 2 THEN
        RAISE EXCEPTION 'event % result must record both sides', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM lynx.world_event_result_sides AS result_side
        LEFT JOIN lynx.world_event_sides AS event_side
          ON event_side.event_id = result_side.event_id
         AND event_side.side_id = result_side.side_id
        WHERE result_side.event_id = target
          AND (event_side.side_id IS NULL
               OR event_side.faction_id <> result_side.faction_id
               OR event_side.role <> result_side.role)
    ) THEN
        RAISE EXCEPTION 'event % result side does not match the event sides', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM lynx.world_event_result_sides AS result_side
        WHERE result_side.event_id = target
          AND result_side.participants <> (
              SELECT count(*)
              FROM lynx.world_event_enrollments AS enrollment
              WHERE enrollment.event_id = result_side.event_id
                AND enrollment.side_id = result_side.side_id
          )
    ) THEN
        RAISE EXCEPTION 'event % result participant count does not match enrollment', target
            USING ERRCODE = 'check_violation';
    END IF;
    IF result_outcome = 'attacker_victory'
       AND result_territory_change_key IS NULL THEN
        RAISE EXCEPTION 'attacker victory for event % has no territory change', target
            USING ERRCODE = 'check_violation';
    END IF;
END;
$$;
