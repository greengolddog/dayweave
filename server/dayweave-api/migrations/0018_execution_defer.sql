-- Atomic execution defer closes the current lease while preserving the exact
-- future window that a later scheduler/client revision may consume.

ALTER TABLE execution_sessions
    ADD COLUMN move_start timestamptz,
    ADD COLUMN move_end timestamptz,
    ADD COLUMN observed_running_since timestamptz;

-- Keep the public running_since protocol shape unchanged for older strict
-- clients while retaining a private wall-clock anchor for elapsed accounting.
UPDATE execution_sessions
SET observed_running_since = running_since
WHERE state = 'active';

-- Older servers kept the protocol clock monotonic only inside one session. If
-- the host clock moved backward between sessions, execution_state.updated_at
-- could therefore lag an immutable history row. Repair that rollout state
-- before new commands advance the workspace clock from this value.
UPDATE execution_state AS state
SET updated_at = latest.updated_at
FROM (
    SELECT workspace_id, max(updated_at) AS updated_at
    FROM execution_sessions
    GROUP BY workspace_id
) AS latest
WHERE state.workspace_id = latest.workspace_id
  AND state.updated_at < latest.updated_at;

-- Migration 0005 used generated names for several checks involving `state`.
-- Drop exactly those checks by their constrained column rather than relying on
-- PostgreSQL's generated-name suffixes, then replace them with explicit names.
DO $$
DECLARE
    constraint_name text;
BEGIN
    FOR constraint_name IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS state_column
          ON state_column.attrelid = constraint_row.conrelid
         AND state_column.attname = 'state'
        WHERE constraint_row.conrelid = 'execution_sessions'::regclass
          AND constraint_row.contype = 'c'
          AND state_column.attnum = ANY (constraint_row.conkey)
    LOOP
        EXECUTE format(
            'ALTER TABLE execution_sessions DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;
END
$$;

ALTER TABLE execution_sessions
    ADD CONSTRAINT execution_sessions_state_check
        CHECK (state IN ('active', 'paused', 'completed', 'skipped', 'deferred')),
    ADD CONSTRAINT execution_sessions_state_shape_check
        CHECK (
            (state = 'active'
                AND running_since IS NOT NULL
                AND observed_running_since IS NOT NULL
                AND paused_at IS NULL
                AND ended_at IS NULL)
            OR (state = 'paused'
                AND running_since IS NULL
                AND observed_running_since IS NULL
                AND paused_at IS NOT NULL
                AND ended_at IS NULL)
            OR (state IN ('completed', 'skipped', 'deferred')
                AND running_since IS NULL
                AND observed_running_since IS NULL
                AND ended_at IS NOT NULL)
        ),
    ADD CONSTRAINT execution_sessions_actual_seconds_state_check
        CHECK (
            (state IN ('completed', 'skipped', 'deferred'))
                = (actual_seconds IS NOT NULL)
        ),
    ADD CONSTRAINT execution_sessions_deferred_move_window_check
        CHECK (
            (state = 'deferred'
                AND move_start IS NOT NULL
                AND move_end IS NOT NULL
                AND ended_at = updated_at
                AND move_start > ended_at
                AND move_start > updated_at
                AND move_start < move_end
                AND move_end <= move_start + interval '24 hours')
            OR (state <> 'deferred'
                AND move_start IS NULL
                AND move_end IS NULL)
        );
