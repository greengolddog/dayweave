-- Private, server-authoritative execution progress evidence. Historical v19
-- deferred-placement evidence remains immutable and readable, while execution
-- Start and schedule publication cut over atomically to v20 fresh-index rules.

ALTER TABLE items
    ADD COLUMN execution_epoch bigint NOT NULL DEFAULT 1
        CHECK (execution_epoch > 0);

ALTER TABLE execution_sessions
    ADD COLUMN execution_epoch bigint NOT NULL DEFAULT 1
        CHECK (execution_epoch > 0);

CREATE INDEX execution_sessions_progress_history_idx
    ON execution_sessions (
        workspace_id,
        item_id,
        execution_epoch,
        occurrence_id,
        session_index,
        updated_at DESC,
        id DESC
    );

-- A schedule origin is recorded only when Start names an exact block in the
-- current published schedule and that revision's private result snapshot says
-- the block was composed from the same canonical item revision. Sessions with
-- no such row are legacy/passive history and do not become progress evidence.
CREATE TABLE execution_session_schedule_origins (
    workspace_id uuid NOT NULL,
    execution_session_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    source_block_id uuid NOT NULL,
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    execution_epoch bigint NOT NULL CHECK (execution_epoch > 0),
    occurrence_id uuid,
    session_index integer NOT NULL CHECK (session_index BETWEEN 0 AND 65535),
    planned_duration_seconds bigint NOT NULL CHECK (planned_duration_seconds > 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, execution_session_id),
    UNIQUE (workspace_id, schedule_revision_id, source_block_id),
    FOREIGN KEY (workspace_id, execution_session_id)
        REFERENCES execution_sessions(workspace_id, id),
    FOREIGN KEY (workspace_id, schedule_revision_id, source_block_id)
        REFERENCES schedule_blocks(
            workspace_id,
            schedule_revision_id,
            source_block_id
        ),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id)
);

CREATE INDEX execution_session_schedule_origins_item_idx
    ON execution_session_schedule_origins (
        workspace_id,
        item_id,
        execution_epoch,
        occurrence_id,
        session_index
    );

-- Defer closes its old physical index permanently. The replacement index is
-- allocated while execution_state is held and is immutable even before any
-- schedule revision places or consumes it.
CREATE TABLE execution_defer_replacement_claims (
    workspace_id uuid NOT NULL,
    source_deferred_session_id uuid NOT NULL,
    item_id uuid NOT NULL,
    source_item_revision bigint NOT NULL CHECK (source_item_revision > 0),
    execution_epoch bigint NOT NULL CHECK (execution_epoch > 0),
    occurrence_id uuid,
    source_session_index integer NOT NULL CHECK (source_session_index BETWEEN 0 AND 65535),
    replacement_session_index integer NOT NULL
        CHECK (replacement_session_index BETWEEN 0 AND 65535),
    planned_duration_seconds bigint NOT NULL CHECK (planned_duration_seconds > 0),
    planned_duration_source varchar(32) NOT NULL
        CHECK (planned_duration_source IN ('published_origin', 'legacy_move_window')),
    actionable boolean NOT NULL DEFAULT true,
    consumed_before_seconds bigint NOT NULL DEFAULT 0 CHECK (consumed_before_seconds >= 0),
    consumed_by_source_seconds bigint NOT NULL DEFAULT 0 CHECK (consumed_by_source_seconds >= 0),
    remaining_duration_seconds bigint NOT NULL CHECK (remaining_duration_seconds > 0),
    move_start timestamptz NOT NULL,
    move_end timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, source_deferred_session_id),
    FOREIGN KEY (workspace_id, source_deferred_session_id)
        REFERENCES execution_sessions(workspace_id, id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    CHECK (replacement_session_index > source_session_index),
    CHECK (move_start < move_end),
    CHECK (move_end <= move_start + interval '24 hours'),
    CHECK (consumed_before_seconds <= planned_duration_seconds),
    CHECK (
        consumed_by_source_seconds
            <= planned_duration_seconds - consumed_before_seconds
    ),
    CHECK (
        remaining_duration_seconds
            = planned_duration_seconds
                - consumed_before_seconds
                - consumed_by_source_seconds
    ),
    CHECK (
        EXTRACT(EPOCH FROM (move_end - move_start))
            = remaining_duration_seconds::numeric
    ),
    CHECK (
        planned_duration_source <> 'legacy_move_window'
        OR (
            consumed_before_seconds = 0
            AND consumed_by_source_seconds = 0
            AND EXTRACT(EPOCH FROM (move_end - move_start))
                = planned_duration_seconds::numeric
        )
    )
);

CREATE UNIQUE INDEX execution_defer_replacement_claims_physical_index_uq
    ON execution_defer_replacement_claims (
        workspace_id,
        item_id,
        occurrence_id,
        replacement_session_index
    ) NULLS NOT DISTINCT;

CREATE INDEX execution_defer_replacement_claims_item_idx
    ON execution_defer_replacement_claims (
        workspace_id,
        item_id,
        execution_epoch,
        occurrence_id,
        replacement_session_index
    );

-- This marker is deliberately one row per source claim and one row per
-- replacement execution. Start inserts it atomically when consuming a
-- published replacement placement.
CREATE TABLE execution_defer_replacement_consumptions (
    workspace_id uuid NOT NULL,
    source_deferred_session_id uuid NOT NULL,
    replacement_execution_session_id uuid NOT NULL,
    consumed_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, source_deferred_session_id),
    UNIQUE (workspace_id, replacement_execution_session_id),
    FOREIGN KEY (workspace_id, source_deferred_session_id)
        REFERENCES execution_defer_replacement_claims(
            workspace_id,
            source_deferred_session_id
        ),
    FOREIGN KEY (workspace_id, replacement_execution_session_id)
        REFERENCES execution_session_schedule_origins(
            workspace_id,
            execution_session_id
        )
);

-- Existing deferred rows become passive legacy claims. Their move window is
-- the only durable planned-duration evidence, and v19 bindings are not changed.
-- Allocate in stable updated_at/id order above every historical or currently
-- published physical index. Abort the whole migration instead of wrapping.
DO $backfill$
DECLARE
    replacement_index_overflow boolean;
BEGIN
    WITH valid_published_block_indices AS (
        SELECT block.workspace_id,
               block.item_id,
               CASE
                   WHEN block.constraint_snapshot ->> 'occurrence_id' IS NULL THEN NULL
                   ELSE (block.constraint_snapshot ->> 'occurrence_id')::uuid
               END AS occurrence_id,
               (block.constraint_snapshot ->> 'session_index')::integer AS session_index
          FROM schedule_blocks AS block
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = block.workspace_id
           AND revision.id = block.schedule_revision_id
         WHERE revision.state = 'published'
           AND block.item_id IS NOT NULL
           AND (
               block.constraint_snapshot ->> 'occurrence_id' IS NULL
               OR block.constraint_snapshot ->> 'occurrence_id' ~*
                  '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
           )
           AND block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$'
           AND (block.constraint_snapshot ->> 'session_index')::numeric
                BETWEEN 0 AND 65535
    ),
    semantic_high_water AS (
        SELECT workspace_id, item_id, occurrence_id,
               MAX(session_index)::bigint AS session_index
          FROM (
              SELECT workspace_id, item_id, occurrence_id, session_index
                FROM execution_sessions
              UNION ALL
              SELECT workspace_id, item_id, occurrence_id, session_index
                FROM valid_published_block_indices
          ) AS reserved
         GROUP BY workspace_id, item_id, occurrence_id
    ),
    ranked_sessions AS (
        SELECT session.*,
               item.execution_epoch AS current_execution_epoch,
               item.trashed_at IS NULL
                   AND item.status NOT IN ('completed', 'skipped', 'cancelled')
                   AND NOT EXISTS (
                       SELECT 1
                         FROM item_hierarchy AS edge
                         JOIN items AS child
                           ON child.workspace_id = edge.workspace_id
                          AND child.id = edge.child_item_id
                        WHERE edge.workspace_id = item.workspace_id
                          AND edge.parent_item_id = item.id
                          AND child.trashed_at IS NULL
                   ) AS current_item_executable,
               ROW_NUMBER() OVER (
                   PARTITION BY session.workspace_id, session.item_id,
                       session.occurrence_id, session.session_index
                   ORDER BY session.updated_at DESC, session.id DESC
               ) AS semantic_rank
          FROM execution_sessions AS session
          JOIN items AS item
            ON item.workspace_id = session.workspace_id
           AND item.id = session.item_id
    ),
    ordered_defers AS (
        SELECT session.workspace_id,
               COALESCE(high_water.session_index, -1)
                   + ROW_NUMBER() OVER (
                       PARTITION BY session.workspace_id, session.item_id, session.occurrence_id
                       ORDER BY session.updated_at, session.id
                   ) AS replacement_session_index
          FROM ranked_sessions AS session
          LEFT JOIN semantic_high_water AS high_water
            ON high_water.workspace_id = session.workspace_id
           AND high_water.item_id = session.item_id
           AND high_water.occurrence_id IS NOT DISTINCT FROM session.occurrence_id
         WHERE session.state = 'deferred'
    )
    SELECT EXISTS (
        SELECT 1 FROM ordered_defers WHERE replacement_session_index > 65535
    ) INTO replacement_index_overflow;

    IF replacement_index_overflow THEN
        RAISE EXCEPTION
            'execution replacement session index space is exhausted during migration';
    END IF;

    WITH valid_published_block_indices AS (
        SELECT block.workspace_id,
               block.item_id,
               CASE
                   WHEN block.constraint_snapshot ->> 'occurrence_id' IS NULL THEN NULL
                   ELSE (block.constraint_snapshot ->> 'occurrence_id')::uuid
               END AS occurrence_id,
               (block.constraint_snapshot ->> 'session_index')::integer AS session_index
          FROM schedule_blocks AS block
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = block.workspace_id
           AND revision.id = block.schedule_revision_id
         WHERE revision.state = 'published'
           AND block.item_id IS NOT NULL
           AND (
               block.constraint_snapshot ->> 'occurrence_id' IS NULL
               OR block.constraint_snapshot ->> 'occurrence_id' ~*
                  '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
           )
           AND block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$'
           AND (block.constraint_snapshot ->> 'session_index')::numeric
                BETWEEN 0 AND 65535
    ),
    semantic_high_water AS (
        SELECT workspace_id, item_id, occurrence_id,
               MAX(session_index)::bigint AS session_index
          FROM (
              SELECT workspace_id, item_id, occurrence_id, session_index
                FROM execution_sessions
              UNION ALL
              SELECT workspace_id, item_id, occurrence_id, session_index
                FROM valid_published_block_indices
          ) AS reserved
         GROUP BY workspace_id, item_id, occurrence_id
    ),
    ranked_sessions AS (
        SELECT session.*,
               item.execution_epoch AS current_execution_epoch,
               item.trashed_at IS NULL
                   AND item.status NOT IN ('completed', 'skipped', 'cancelled')
                   AND NOT EXISTS (
                       SELECT 1
                         FROM item_hierarchy AS edge
                         JOIN items AS child
                           ON child.workspace_id = edge.workspace_id
                          AND child.id = edge.child_item_id
                        WHERE edge.workspace_id = item.workspace_id
                          AND edge.parent_item_id = item.id
                          AND child.trashed_at IS NULL
                   ) AS current_item_executable,
               ROW_NUMBER() OVER (
                   PARTITION BY session.workspace_id, session.item_id,
                       session.occurrence_id, session.session_index
                   ORDER BY session.updated_at DESC, session.id DESC
               ) AS semantic_rank
          FROM execution_sessions AS session
          JOIN items AS item
            ON item.workspace_id = session.workspace_id
           AND item.id = session.item_id
    ),
    ordered_defers AS (
        SELECT session.*,
               COALESCE(high_water.session_index, -1)
                   + ROW_NUMBER() OVER (
                       PARTITION BY session.workspace_id, session.item_id, session.occurrence_id
                       ORDER BY session.updated_at, session.id
                   ) AS replacement_session_index
          FROM ranked_sessions AS session
          LEFT JOIN semantic_high_water AS high_water
            ON high_water.workspace_id = session.workspace_id
           AND high_water.item_id = session.item_id
           AND high_water.occurrence_id IS NOT DISTINCT FROM session.occurrence_id
         WHERE session.state = 'deferred'
    )
    INSERT INTO execution_defer_replacement_claims (
        workspace_id,
        source_deferred_session_id,
        item_id,
        source_item_revision,
        execution_epoch,
        occurrence_id,
        source_session_index,
        replacement_session_index,
        planned_duration_seconds,
        planned_duration_source,
        actionable,
        consumed_before_seconds,
        consumed_by_source_seconds,
        remaining_duration_seconds,
        move_start,
        move_end,
        created_at
    )
    SELECT workspace_id,
           id,
           item_id,
           item_revision,
           execution_epoch,
           occurrence_id,
           session_index,
           replacement_session_index::integer,
           EXTRACT(EPOCH FROM (move_end - move_start))::bigint,
           'legacy_move_window',
           semantic_rank = 1
               AND execution_epoch = current_execution_epoch
               AND current_item_executable,
           0,
           0,
           EXTRACT(EPOCH FROM (move_end - move_start))::bigint,
           move_start,
           move_end,
           updated_at
      FROM ordered_defers
     ORDER BY workspace_id, item_id, occurrence_id NULLS FIRST, replacement_session_index;
END
$backfill$;

CREATE FUNCTION guard_execution_schedule_origin() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    origin_is_exact boolean;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution schedule origins are immutable';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM execution_sessions AS session
          JOIN items AS item
            ON item.workspace_id = session.workspace_id
           AND item.id = session.item_id
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = session.workspace_id
           AND revision.id = NEW.schedule_revision_id
           AND revision.state = 'published'
          JOIN schedule_blocks AS block
            ON block.workspace_id = revision.workspace_id
           AND block.schedule_revision_id = revision.id
           AND block.source_block_id = NEW.source_block_id
          WHERE session.workspace_id = NEW.workspace_id
            AND session.id = NEW.execution_session_id
            AND session.item_id = NEW.item_id
            AND session.item_revision = NEW.item_revision
            AND session.execution_epoch = NEW.execution_epoch
            AND session.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
            AND session.session_index = NEW.session_index
            AND session.planned_block_id = NEW.source_block_id
            AND item.revision = NEW.item_revision
            AND item.execution_epoch = NEW.execution_epoch
            AND block.item_id = NEW.item_id
            AND block.block_kind IN ('planned', 'pinned')
            AND block.is_fixed = (block.block_kind = 'pinned')
            AND block.constraint_snapshot ->> 'source_block_id'
                = NEW.source_block_id::text
            AND block.constraint_snapshot ->> 'core_kind' = block.block_kind
            AND block.constraint_snapshot ->> 'session_index'
                = NEW.session_index::text
            AND block.constraint_snapshot ->> 'occurrence_id'
                IS NOT DISTINCT FROM NEW.occurrence_id::text
            AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))
                = NEW.planned_duration_seconds::numeric
            AND EXISTS (
                SELECT 1
                  FROM schedule_revision_details AS detail
                 WHERE detail.workspace_id = NEW.workspace_id
                   AND detail.schedule_revision_id = NEW.schedule_revision_id
                   AND detail.result_snapshot -> 'compose' -> 'source_item_revisions'
                        ->> NEW.item_id::text = NEW.item_revision::text
            )
    ) INTO origin_is_exact;

    IF NOT origin_is_exact THEN
        RAISE EXCEPTION 'execution schedule origin does not match current published evidence';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_session_schedule_origins_guard
    BEFORE INSERT OR UPDATE OR DELETE ON execution_session_schedule_origins
    FOR EACH ROW EXECUTE FUNCTION guard_execution_schedule_origin();

CREATE FUNCTION guard_execution_defer_replacement_claim() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    source_row record;
    source_origin_duration bigint;
    semantic_high_water integer;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution defer replacement claims are immutable';
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, NEW.created_at)
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT item_id, item_revision, execution_epoch, occurrence_id, session_index,
           state, actual_seconds, move_start, move_end
      INTO source_row
      FROM execution_sessions
     WHERE workspace_id = NEW.workspace_id
       AND id = NEW.source_deferred_session_id
     FOR SHARE;

    IF NOT FOUND
       OR source_row.state <> 'deferred'
       OR source_row.item_id IS DISTINCT FROM NEW.item_id
       OR source_row.item_revision IS DISTINCT FROM NEW.source_item_revision
       OR source_row.execution_epoch IS DISTINCT FROM NEW.execution_epoch
       OR source_row.occurrence_id IS DISTINCT FROM NEW.occurrence_id
       OR source_row.session_index IS DISTINCT FROM NEW.source_session_index
       OR source_row.move_start IS DISTINCT FROM NEW.move_start
       OR source_row.move_end IS DISTINCT FROM NEW.move_end
    THEN
        RAISE EXCEPTION 'execution defer replacement claim does not match its source';
    END IF;

    SELECT planned_duration_seconds
      INTO source_origin_duration
      FROM execution_session_schedule_origins
     WHERE workspace_id = NEW.workspace_id
       AND execution_session_id = NEW.source_deferred_session_id
     FOR SHARE;

    IF NEW.planned_duration_source = 'published_origin' THEN
        IF source_origin_duration IS NULL
           OR NEW.planned_duration_seconds <> source_origin_duration
           OR NEW.consumed_before_seconds <> 0
           OR NEW.consumed_by_source_seconds
                <> LEAST(source_row.actual_seconds, source_origin_duration)
        THEN
            RAISE EXCEPTION 'execution defer replacement claim has invalid origin duration';
        END IF;
    ELSIF source_origin_duration IS NOT NULL THEN
        RAISE EXCEPTION 'attested execution defer cannot discard its origin duration';
    END IF;

    WITH current_published_block_indices AS (
        SELECT CASE
                   WHEN block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$'
                    AND (block.constraint_snapshot ->> 'session_index')::numeric
                          BETWEEN 0 AND 65535
                   THEN (block.constraint_snapshot ->> 'session_index')::integer
               END AS session_index
          FROM schedule_blocks AS block
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = block.workspace_id
           AND revision.id = block.schedule_revision_id
         WHERE revision.workspace_id = NEW.workspace_id
           AND revision.state = 'published'
           AND block.item_id = NEW.item_id
           AND block.constraint_snapshot ->> 'occurrence_id'
               IS NOT DISTINCT FROM NEW.occurrence_id::text
    )
    SELECT GREATEST(
               COALESCE((
                   SELECT MAX(session_index)
                     FROM execution_sessions
                    WHERE workspace_id = NEW.workspace_id
                      AND item_id = NEW.item_id
                      AND occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
               ), -1),
               COALESCE((
                   SELECT MAX(replacement_session_index)
                     FROM execution_defer_replacement_claims
                    WHERE workspace_id = NEW.workspace_id
                      AND item_id = NEW.item_id
                      AND occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
               ), -1),
               COALESCE((
                   SELECT MAX(session_index) FROM current_published_block_indices
               ), -1)
           )
      INTO semantic_high_water;
    IF NEW.replacement_session_index <= semantic_high_water THEN
        RAISE EXCEPTION 'execution defer replacement index is not fresh';
    END IF;

    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_defer_replacement_claims_guard
    BEFORE INSERT OR UPDATE OR DELETE ON execution_defer_replacement_claims
    FOR EACH ROW EXECUTE FUNCTION guard_execution_defer_replacement_claim();

CREATE FUNCTION guard_execution_defer_replacement_consumption() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    consumption_is_exact boolean;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution defer replacement consumptions are immutable';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims AS claim
          JOIN execution_sessions AS replacement
            ON replacement.workspace_id = claim.workspace_id
           AND replacement.id = NEW.replacement_execution_session_id
          JOIN execution_session_schedule_origins AS origin
            ON origin.workspace_id = replacement.workspace_id
           AND origin.execution_session_id = replacement.id
         WHERE claim.workspace_id = NEW.workspace_id
           AND claim.source_deferred_session_id = NEW.source_deferred_session_id
           AND replacement.item_id = claim.item_id
           AND replacement.execution_epoch = claim.execution_epoch
           AND replacement.occurrence_id IS NOT DISTINCT FROM claim.occurrence_id
           AND replacement.session_index = claim.replacement_session_index
           AND origin.planned_duration_seconds = claim.remaining_duration_seconds
    ) INTO consumption_is_exact;

    IF NOT consumption_is_exact THEN
        RAISE EXCEPTION 'execution defer replacement consumption does not match its claim';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_defer_replacement_consumptions_guard
    BEFORE INSERT OR UPDATE OR DELETE ON execution_defer_replacement_consumptions
    FOR EACH ROW EXECUTE FUNCTION guard_execution_defer_replacement_consumption();

-- A claim copies terminal source facts, so the source row must not drift after
-- insertion. Terminal execution rows have no legitimate later transition.
CREATE FUNCTION protect_execution_defer_claim_source() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims
         WHERE workspace_id = OLD.workspace_id
           AND source_deferred_session_id = OLD.id
    ) THEN
        RAISE EXCEPTION 'claimed deferred execution sessions are immutable';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE TRIGGER execution_sessions_protect_defer_claim_source
    BEFORE UPDATE OR DELETE ON execution_sessions
    FOR EACH ROW EXECUTE FUNCTION protect_execution_defer_claim_source();

-- Physical execution indices belong to one item occurrence forever. Existing
-- installations may already contain duplicate legacy rows across item
-- revisions, so the migration seeds one ownership marker per distinct tuple.
CREATE TABLE execution_physical_indices (
    workspace_id uuid NOT NULL,
    item_id uuid NOT NULL,
    occurrence_id uuid,
    session_index integer NOT NULL CHECK (session_index BETWEEN 0 AND 65535),
    reservation_kind varchar(32) NOT NULL CHECK (
        reservation_kind IN (
            'historical_session',
            'execution_start',
            'defer_replacement'
        )
    ),
    execution_session_id uuid,
    source_deferred_session_id uuid,
    reserved_at timestamptz NOT NULL,
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, execution_session_id)
        REFERENCES execution_sessions(workspace_id, id),
    FOREIGN KEY (workspace_id, source_deferred_session_id)
        REFERENCES execution_defer_replacement_claims(
            workspace_id,
            source_deferred_session_id
        ),
    CHECK (
        (reservation_kind = 'historical_session'
            AND execution_session_id IS NULL
            AND source_deferred_session_id IS NULL)
        OR (reservation_kind = 'execution_start'
            AND execution_session_id IS NOT NULL
            AND source_deferred_session_id IS NULL)
        OR (reservation_kind = 'defer_replacement'
            AND execution_session_id IS NULL
            AND source_deferred_session_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX execution_physical_indices_identity_uq
    ON execution_physical_indices (
        workspace_id,
        item_id,
        occurrence_id,
        session_index
    ) NULLS NOT DISTINCT;

CREATE UNIQUE INDEX execution_physical_indices_session_uq
    ON execution_physical_indices (workspace_id, execution_session_id)
    WHERE execution_session_id IS NOT NULL;

CREATE UNIQUE INDEX execution_physical_indices_claim_uq
    ON execution_physical_indices (workspace_id, source_deferred_session_id)
    WHERE source_deferred_session_id IS NOT NULL;

INSERT INTO execution_physical_indices (
    workspace_id,
    item_id,
    occurrence_id,
    session_index,
    reservation_kind,
    execution_session_id,
    source_deferred_session_id,
    reserved_at
)
SELECT workspace_id,
       item_id,
       occurrence_id,
       session_index,
       'historical_session',
       NULL,
       NULL,
       MIN(created_at)
  FROM execution_sessions
 GROUP BY workspace_id, item_id, occurrence_id, session_index;

INSERT INTO execution_physical_indices (
    workspace_id,
    item_id,
    occurrence_id,
    session_index,
    reservation_kind,
    execution_session_id,
    source_deferred_session_id,
    reserved_at
)
SELECT workspace_id,
       item_id,
       occurrence_id,
       replacement_session_index,
       'defer_replacement',
       NULL,
       source_deferred_session_id,
       created_at
  FROM execution_defer_replacement_claims;

CREATE FUNCTION guard_execution_physical_index() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution physical indices are immutable';
    END IF;
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'execution physical indices require an execution allocation';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_physical_indices_guard
    BEFORE INSERT OR UPDATE OR DELETE ON execution_physical_indices
    FOR EACH ROW EXECUTE FUNCTION guard_execution_physical_index();

-- New v20 schedule evidence maps an immutable deferred source to its fresh
-- replacement index. v19 source-index bindings remain untouched and readable.
CREATE TABLE schedule_defer_replacement_placements (
    workspace_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    source_deferred_session_id uuid NOT NULL,
    source_block_id uuid NOT NULL,
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    execution_epoch bigint NOT NULL CHECK (execution_epoch > 0),
    occurrence_id uuid,
    replacement_session_index integer NOT NULL
        CHECK (replacement_session_index BETWEEN 0 AND 65535),
    remaining_duration_seconds bigint NOT NULL
        CHECK (remaining_duration_seconds > 0),
    move_start timestamptz NOT NULL,
    move_end timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (
        workspace_id,
        schedule_revision_id,
        source_deferred_session_id
    ),
    UNIQUE (workspace_id, schedule_revision_id, source_block_id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    FOREIGN KEY (workspace_id, schedule_revision_id, source_block_id)
        REFERENCES schedule_blocks(
            workspace_id,
            schedule_revision_id,
            source_block_id
        ),
    FOREIGN KEY (workspace_id, source_deferred_session_id)
        REFERENCES execution_defer_replacement_claims(
            workspace_id,
            source_deferred_session_id
        ),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    CHECK (move_start < move_end),
    CHECK (move_end <= move_start + interval '24 hours'),
    CHECK (
        EXTRACT(EPOCH FROM (move_end - move_start))
            = remaining_duration_seconds::numeric
    )
);

CREATE INDEX schedule_defer_replacement_placements_start_idx
    ON schedule_defer_replacement_placements (
        workspace_id,
        source_deferred_session_id,
        source_block_id,
        replacement_session_index
    );

CREATE FUNCTION guard_schedule_defer_replacement_placement() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    placement_is_exact boolean;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'schedule defer replacement placements are immutable';
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, NEW.created_at)
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims AS claim
          JOIN execution_sessions AS source
            ON source.workspace_id = claim.workspace_id
           AND source.id = claim.source_deferred_session_id
          JOIN items AS item
            ON item.workspace_id = claim.workspace_id
           AND item.id = claim.item_id
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = claim.workspace_id
           AND revision.id = NEW.schedule_revision_id
          JOIN schedule_blocks AS block
            ON block.workspace_id = revision.workspace_id
           AND block.schedule_revision_id = revision.id
           AND block.source_block_id = NEW.source_block_id
         WHERE claim.workspace_id = NEW.workspace_id
           AND claim.source_deferred_session_id = NEW.source_deferred_session_id
           AND claim.actionable
           AND NOT EXISTS (
               SELECT 1
                 FROM execution_defer_replacement_consumptions AS consumption
                WHERE consumption.workspace_id = claim.workspace_id
                  AND consumption.source_deferred_session_id =
                      claim.source_deferred_session_id
           )
           AND source.state = 'deferred'
           AND claim.item_id = NEW.item_id
           AND claim.execution_epoch = NEW.execution_epoch
           AND claim.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
           AND claim.replacement_session_index = NEW.replacement_session_index
           AND claim.remaining_duration_seconds = NEW.remaining_duration_seconds
           AND claim.move_start = NEW.move_start
           AND claim.move_end = NEW.move_end
           AND item.revision = NEW.item_revision
           AND item.execution_epoch = NEW.execution_epoch
           AND item.trashed_at IS NULL
           AND item.status NOT IN ('completed', 'skipped', 'cancelled')
           AND NOT EXISTS (
               SELECT 1
                 FROM item_hierarchy AS edge
                 JOIN items AS child
                   ON child.workspace_id = edge.workspace_id
                  AND child.id = edge.child_item_id
                WHERE edge.workspace_id = item.workspace_id
                  AND edge.parent_item_id = item.id
                  AND child.trashed_at IS NULL
           )
           AND revision.state = 'draft'
           AND revision.horizon_start <= NEW.move_start
           AND revision.horizon_end >= NEW.move_end
           AND block.item_id = NEW.item_id
           AND block.block_kind = 'pinned'
           AND block.is_fixed
           AND block.starts_at = NEW.move_start
           AND block.ends_at = NEW.move_end
           AND block.constraint_snapshot ->> 'source_block_id'
               = NEW.source_block_id::text
           AND block.constraint_snapshot ->> 'occurrence_id'
               IS NOT DISTINCT FROM NEW.occurrence_id::text
           AND block.constraint_snapshot ->> 'session_index'
               = NEW.replacement_session_index::text
           AND block.constraint_snapshot ->> 'core_kind' = 'pinned'
           AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))
               = NEW.remaining_duration_seconds::numeric
    ) INTO placement_is_exact;

    IF NOT placement_is_exact THEN
        RAISE EXCEPTION 'schedule defer replacement placement is not exact';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER schedule_defer_replacement_placements_guard
    BEFORE INSERT OR UPDATE OR DELETE ON schedule_defer_replacement_placements
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_defer_replacement_placement();

CREATE FUNCTION protect_v20_schedule_evidence_sources() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM execution_session_schedule_origins
         WHERE workspace_id = OLD.workspace_id
           AND schedule_revision_id = OLD.schedule_revision_id
           AND source_block_id = OLD.source_block_id
    ) OR EXISTS (
        SELECT 1
          FROM schedule_defer_replacement_placements
         WHERE workspace_id = OLD.workspace_id
           AND schedule_revision_id = OLD.schedule_revision_id
           AND source_block_id = OLD.source_block_id
    ) THEN
        RAISE EXCEPTION 'execution-bound schedule blocks are immutable';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE TRIGGER execution_schedule_blocks_protect_v20_sources
    BEFORE UPDATE OR DELETE ON schedule_blocks
    FOR EACH ROW EXECUTE FUNCTION protect_v20_schedule_evidence_sources();

-- A future draft may allocate a previously unused index. A durable execution
-- or claim reservation, however, owns its index permanently. The only block
-- exception is the exact pinned window for one live replacement claim.
CREATE FUNCTION guard_execution_schedule_block_index() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    block_occurrence_id uuid;
    block_session_index integer;
    physical_row record;
    exact_in_flight_block boolean;
    exact_claim_block boolean;
BEGIN
    INSERT INTO execution_state (workspace_id)
    VALUES (NEW.workspace_id)
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    IF NEW.item_id IS NULL OR NEW.block_kind NOT IN ('planned', 'pinned') THEN
        RETURN NEW;
    END IF;
    IF NEW.constraint_snapshot ->> 'session_index' !~ '^[0-9]+$'
       OR (NEW.constraint_snapshot ->> 'session_index')::numeric
            NOT BETWEEN 0 AND 65535
       OR (
           NEW.constraint_snapshot ->> 'occurrence_id' IS NOT NULL
           AND NEW.constraint_snapshot ->> 'occurrence_id' !~*
               '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
       )
    THEN
        RAISE EXCEPTION 'schedule block has invalid execution identity';
    END IF;
    block_session_index :=
        (NEW.constraint_snapshot ->> 'session_index')::integer;
    block_occurrence_id :=
        (NEW.constraint_snapshot ->> 'occurrence_id')::uuid;

    SELECT reservation_kind, source_deferred_session_id
      INTO physical_row
      FROM execution_physical_indices
     WHERE workspace_id = NEW.workspace_id
       AND item_id = NEW.item_id
       AND occurrence_id IS NOT DISTINCT FROM block_occurrence_id
       AND session_index = block_session_index
     FOR SHARE;
    IF NOT FOUND THEN
        RETURN NEW;
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM execution_sessions AS session
          JOIN execution_session_schedule_origins AS origin
            ON origin.workspace_id = session.workspace_id
           AND origin.execution_session_id = session.id
          JOIN schedule_blocks AS source
            ON source.workspace_id = origin.workspace_id
           AND source.schedule_revision_id = origin.schedule_revision_id
           AND source.source_block_id = origin.source_block_id
          JOIN items AS item
            ON item.workspace_id = session.workspace_id
           AND item.id = session.item_id
         WHERE session.workspace_id = NEW.workspace_id
           AND session.item_id = NEW.item_id
           AND session.occurrence_id IS NOT DISTINCT FROM block_occurrence_id
           AND session.session_index = block_session_index
           AND session.state IN ('active', 'paused')
           AND session.execution_epoch = item.execution_epoch
           AND item.trashed_at IS NULL
           AND item.status NOT IN ('completed', 'skipped', 'cancelled')
           AND NOT EXISTS (
               SELECT 1
                 FROM item_hierarchy AS edge
                 JOIN items AS child
                   ON child.workspace_id = edge.workspace_id
                  AND child.id = edge.child_item_id
                WHERE edge.workspace_id = item.workspace_id
                  AND edge.parent_item_id = item.id
                  AND child.trashed_at IS NULL
           )
           AND NEW.source_block_id = origin.source_block_id
           AND NEW.starts_at = source.starts_at
           AND NEW.ends_at = source.ends_at
           AND EXTRACT(EPOCH FROM (NEW.ends_at - NEW.starts_at))
               = origin.planned_duration_seconds::numeric
           AND NEW.block_kind = 'pinned'
           AND NEW.is_fixed
           AND NEW.constraint_snapshot ->> 'source_block_id'
               = NEW.source_block_id::text
           AND NEW.constraint_snapshot ->> 'occurrence_id'
               IS NOT DISTINCT FROM block_occurrence_id::text
           AND NEW.constraint_snapshot ->> 'session_index'
               = block_session_index::text
           AND NEW.constraint_snapshot ->> 'core_kind' = 'pinned'
    ) INTO exact_in_flight_block;
    IF exact_in_flight_block THEN
        RETURN NEW;
    END IF;
    IF physical_row.reservation_kind <> 'defer_replacement' THEN
        RAISE EXCEPTION 'schedule block reuses a historical execution index';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims AS claim
          JOIN items AS item
            ON item.workspace_id = claim.workspace_id
           AND item.id = claim.item_id
         WHERE claim.workspace_id = NEW.workspace_id
           AND claim.source_deferred_session_id =
               physical_row.source_deferred_session_id
           AND claim.actionable
           AND claim.item_id = NEW.item_id
           AND claim.occurrence_id IS NOT DISTINCT FROM block_occurrence_id
           AND claim.replacement_session_index = block_session_index
           AND claim.move_start = NEW.starts_at
           AND claim.move_end = NEW.ends_at
           AND EXTRACT(EPOCH FROM (NEW.ends_at - NEW.starts_at))
               = claim.remaining_duration_seconds::numeric
           AND item.execution_epoch = claim.execution_epoch
           AND item.trashed_at IS NULL
           AND item.status NOT IN ('completed', 'skipped', 'cancelled')
           AND NOT EXISTS (
               SELECT 1
                 FROM item_hierarchy AS edge
                 JOIN items AS child
                   ON child.workspace_id = edge.workspace_id
                  AND child.id = edge.child_item_id
                WHERE edge.workspace_id = item.workspace_id
                  AND edge.parent_item_id = item.id
                  AND child.trashed_at IS NULL
           )
           AND NEW.block_kind = 'pinned'
           AND NEW.is_fixed
           AND NEW.constraint_snapshot ->> 'source_block_id'
               = NEW.source_block_id::text
           AND NEW.constraint_snapshot ->> 'core_kind' = 'pinned'
           AND NOT EXISTS (
               SELECT 1
                 FROM execution_defer_replacement_consumptions AS consumption
                WHERE consumption.workspace_id = claim.workspace_id
                  AND consumption.source_deferred_session_id =
                      claim.source_deferred_session_id
           )
    ) INTO exact_claim_block;
    IF NOT exact_claim_block THEN
        RAISE EXCEPTION 'schedule block does not match its reserved replacement index';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_schedule_blocks_physical_index_guard
    BEFORE INSERT OR UPDATE ON schedule_blocks
    FOR EACH ROW EXECUTE FUNCTION guard_execution_schedule_block_index();

-- Future claims reserve their physical index inside the claim transaction.
CREATE FUNCTION reserve_execution_defer_replacement_index() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    INSERT INTO execution_physical_indices (
        workspace_id,
        item_id,
        occurrence_id,
        session_index,
        reservation_kind,
        execution_session_id,
        source_deferred_session_id,
        reserved_at
    ) VALUES (
        NEW.workspace_id,
        NEW.item_id,
        NEW.occurrence_id,
        NEW.replacement_session_index,
        'defer_replacement',
        NULL,
        NEW.source_deferred_session_id,
        NEW.created_at
    );
    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_defer_replacement_claims_reserve_index
    AFTER INSERT ON execution_defer_replacement_claims
    FOR EACH ROW EXECUTE FUNCTION reserve_execution_defer_replacement_index();

CREATE OR REPLACE FUNCTION guard_execution_defer_replacement_claim() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    source_row record;
    source_origin_duration bigint;
    semantic_high_water integer;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution defer replacement claims are immutable';
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, NEW.created_at)
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT session.item_id, session.item_revision, session.execution_epoch,
           session.occurrence_id, session.session_index, session.state,
           session.actual_seconds, session.move_start, session.move_end,
           item.revision AS current_item_revision,
           item.execution_epoch AS current_execution_epoch,
           item.trashed_at IS NULL
               AND item.status NOT IN ('completed', 'skipped', 'cancelled')
               AND NOT EXISTS (
                   SELECT 1
                     FROM item_hierarchy AS edge
                     JOIN items AS child
                       ON child.workspace_id = edge.workspace_id
                      AND child.id = edge.child_item_id
                    WHERE edge.workspace_id = item.workspace_id
                      AND edge.parent_item_id = item.id
                      AND child.trashed_at IS NULL
               ) AS current_item_executable
      INTO source_row
      FROM execution_sessions AS session
      JOIN items AS item
        ON item.workspace_id = session.workspace_id
       AND item.id = session.item_id
     WHERE session.workspace_id = NEW.workspace_id
       AND session.id = NEW.source_deferred_session_id
     FOR SHARE OF session, item;

    IF NOT FOUND
       OR NOT NEW.actionable
       OR source_row.state <> 'deferred'
       OR source_row.item_id IS DISTINCT FROM NEW.item_id
       OR source_row.item_revision IS DISTINCT FROM NEW.source_item_revision
       OR source_row.execution_epoch IS DISTINCT FROM NEW.execution_epoch
       OR source_row.occurrence_id IS DISTINCT FROM NEW.occurrence_id
       OR source_row.session_index IS DISTINCT FROM NEW.source_session_index
       OR source_row.move_start IS DISTINCT FROM NEW.move_start
       OR source_row.move_end IS DISTINCT FROM NEW.move_end
       OR source_row.current_item_revision IS DISTINCT FROM NEW.source_item_revision
       OR source_row.current_execution_epoch IS DISTINCT FROM NEW.execution_epoch
       OR NOT source_row.current_item_executable
    THEN
        RAISE EXCEPTION 'execution defer replacement claim does not match its source';
    END IF;

    SELECT planned_duration_seconds
      INTO source_origin_duration
      FROM execution_session_schedule_origins
     WHERE workspace_id = NEW.workspace_id
       AND execution_session_id = NEW.source_deferred_session_id
     FOR SHARE;

    IF NEW.planned_duration_source = 'published_origin' THEN
        IF source_origin_duration IS NULL
           OR NEW.planned_duration_seconds <> source_origin_duration
           OR NEW.consumed_before_seconds <> 0
           OR NEW.consumed_by_source_seconds
                <> LEAST(source_row.actual_seconds, source_origin_duration)
           OR NEW.remaining_duration_seconds <= 0
           OR EXTRACT(EPOCH FROM (NEW.move_end - NEW.move_start))
                <> NEW.remaining_duration_seconds::numeric
        THEN
            RAISE EXCEPTION 'execution defer replacement claim has invalid origin duration';
        END IF;
    ELSIF source_origin_duration IS NOT NULL THEN
        RAISE EXCEPTION 'attested execution defer cannot discard its origin duration';
    END IF;

    WITH current_published_block_indices AS (
        SELECT CASE
                   WHEN block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$'
                    AND (block.constraint_snapshot ->> 'session_index')::numeric
                          BETWEEN 0 AND 65535
                   THEN (block.constraint_snapshot ->> 'session_index')::integer
               END AS session_index
          FROM schedule_blocks AS block
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = block.workspace_id
           AND revision.id = block.schedule_revision_id
         WHERE revision.workspace_id = NEW.workspace_id
           AND revision.state = 'published'
           AND block.item_id = NEW.item_id
           AND block.constraint_snapshot ->> 'occurrence_id'
               IS NOT DISTINCT FROM NEW.occurrence_id::text
    )
    SELECT GREATEST(
               COALESCE((
                   SELECT MAX(session_index)
                     FROM execution_physical_indices
                    WHERE workspace_id = NEW.workspace_id
                      AND item_id = NEW.item_id
                      AND occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
               ), -1),
               COALESCE((
                   SELECT MAX(session_index) FROM current_published_block_indices
               ), -1)
           )
      INTO semantic_high_water;
    IF NEW.replacement_session_index <= semantic_high_water THEN
        RAISE EXCEPTION 'execution defer replacement index is not fresh';
    END IF;

    RETURN NEW;
END
$guard$;

-- v20 replaces the v19 source-index restart rule. A physical index with any
-- history is closed. Only a pre-reserved claim index may Start, and then only
-- through its exact current-published replacement placement.
CREATE OR REPLACE FUNCTION guard_execution_session_semantic_start() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    physical_row record;
    replacement_is_exact boolean;
    item_is_current boolean;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.state = 'active' AND OLD.state NOT IN ('active', 'paused') THEN
            RAISE EXCEPTION 'terminal execution semantics cannot be rewritten as active';
        END IF;
        RETURN NEW;
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, COALESCE(NEW.updated_at, current_timestamp))
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    IF NEW.state <> 'active' THEN
        RETURN NEW;
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM items AS item
         WHERE item.workspace_id = NEW.workspace_id
           AND item.id = NEW.item_id
           AND item.revision = NEW.item_revision
           AND item.execution_epoch = NEW.execution_epoch
           AND item.trashed_at IS NULL
           AND item.status NOT IN ('completed', 'skipped', 'cancelled')
           AND NOT EXISTS (
               SELECT 1
                 FROM item_hierarchy AS edge
                 JOIN items AS child
                   ON child.workspace_id = edge.workspace_id
                  AND child.id = edge.child_item_id
                WHERE edge.workspace_id = NEW.workspace_id
                  AND edge.parent_item_id = NEW.item_id
                  AND child.trashed_at IS NULL
           )
    ) INTO item_is_current;
    IF NOT item_is_current THEN
        RAISE EXCEPTION 'execution Start item is not current and executable';
    END IF;

    SELECT reservation_kind, source_deferred_session_id
      INTO physical_row
      FROM execution_physical_indices
     WHERE workspace_id = NEW.workspace_id
       AND item_id = NEW.item_id
       AND occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
       AND session_index = NEW.session_index
     FOR SHARE;
    IF NOT FOUND THEN
        RETURN NEW;
    END IF;
    IF physical_row.reservation_kind <> 'defer_replacement'
       OR physical_row.source_deferred_session_id IS NULL
       OR NEW.planned_block_id IS NULL
    THEN
        RAISE EXCEPTION 'execution physical session index is already reserved';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims AS claim
          JOIN schedule_defer_replacement_placements AS placement
            ON placement.workspace_id = claim.workspace_id
           AND placement.source_deferred_session_id =
               claim.source_deferred_session_id
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = placement.workspace_id
           AND revision.id = placement.schedule_revision_id
          JOIN schedule_blocks AS block
            ON block.workspace_id = placement.workspace_id
           AND block.schedule_revision_id = placement.schedule_revision_id
           AND block.source_block_id = placement.source_block_id
         WHERE claim.workspace_id = NEW.workspace_id
           AND claim.source_deferred_session_id =
               physical_row.source_deferred_session_id
           AND claim.actionable
           AND claim.item_id = NEW.item_id
           AND claim.execution_epoch = NEW.execution_epoch
           AND claim.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
           AND claim.replacement_session_index = NEW.session_index
           AND placement.item_id = NEW.item_id
           AND placement.item_revision = NEW.item_revision
           AND placement.execution_epoch = NEW.execution_epoch
           AND placement.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
           AND placement.replacement_session_index = NEW.session_index
           AND placement.remaining_duration_seconds = claim.remaining_duration_seconds
           AND placement.move_start = claim.move_start
           AND placement.move_end = claim.move_end
           AND placement.source_block_id = NEW.planned_block_id
           AND revision.state = 'published'
           AND block.item_id = NEW.item_id
           AND block.starts_at = claim.move_start
           AND block.ends_at = claim.move_end
           AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))
               = claim.remaining_duration_seconds::numeric
           AND NOT EXISTS (
               SELECT 1
                 FROM execution_defer_replacement_consumptions AS consumption
                WHERE consumption.workspace_id = claim.workspace_id
                  AND consumption.source_deferred_session_id =
                      claim.source_deferred_session_id
           )
    ) INTO replacement_is_exact;
    IF NOT replacement_is_exact THEN
        RAISE EXCEPTION 'execution replacement requires an exact current published binding';
    END IF;
    RETURN NEW;
END
$guard$;

-- Origin rows are captured only by the immediate Start trigger. Direct or
-- retroactive attachment is rejected even when the copied block identity is
-- otherwise plausible.
CREATE OR REPLACE FUNCTION guard_execution_schedule_origin() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    origin_is_exact boolean;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution schedule origins are immutable';
    END IF;
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'execution schedule origin must be captured by Start';
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, NEW.created_at)
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT EXISTS (
        SELECT 1
          FROM execution_sessions AS session
          JOIN items AS item
            ON item.workspace_id = session.workspace_id
           AND item.id = session.item_id
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = session.workspace_id
           AND revision.id = NEW.schedule_revision_id
           AND revision.state = 'published'
          JOIN schedule_blocks AS block
            ON block.workspace_id = revision.workspace_id
           AND block.schedule_revision_id = revision.id
           AND block.source_block_id = NEW.source_block_id
         WHERE session.workspace_id = NEW.workspace_id
           AND session.id = NEW.execution_session_id
           AND session.item_id = NEW.item_id
           AND session.item_revision = NEW.item_revision
           AND session.execution_epoch = NEW.execution_epoch
           AND session.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
           AND session.session_index = NEW.session_index
           AND session.planned_block_id = NEW.source_block_id
           AND session.state = 'active'
           AND session.revision = 1
           AND session.accumulated_seconds = 0
           AND session.actual_seconds IS NULL
           AND session.started_at = NEW.created_at
           AND session.created_at = NEW.created_at
           AND session.updated_at = NEW.created_at
           AND session.running_since = NEW.created_at
           AND session.paused_at IS NULL
           AND session.pause_until IS NULL
           AND session.move_start IS NULL
           AND session.move_end IS NULL
           AND session.ended_at IS NULL
           AND item.revision = NEW.item_revision
           AND item.execution_epoch = NEW.execution_epoch
           AND block.item_id = NEW.item_id
           AND block.block_kind IN ('planned', 'pinned')
           AND block.is_fixed = (block.block_kind = 'pinned')
           AND block.constraint_snapshot ->> 'source_block_id'
               = NEW.source_block_id::text
           AND block.constraint_snapshot ->> 'core_kind' = block.block_kind
           AND block.constraint_snapshot ->> 'session_index'
               = NEW.session_index::text
           AND block.constraint_snapshot ->> 'occurrence_id'
               IS NOT DISTINCT FROM NEW.occurrence_id::text
           AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))
               = NEW.planned_duration_seconds::numeric
           AND EXISTS (
               SELECT 1
                 FROM schedule_revision_details AS detail
                WHERE detail.workspace_id = NEW.workspace_id
                  AND detail.schedule_revision_id = NEW.schedule_revision_id
                  AND detail.result_snapshot -> 'compose' -> 'source_item_revisions'
                       ->> NEW.item_id::text = NEW.item_revision::text
           )
    ) INTO origin_is_exact;
    IF NOT origin_is_exact THEN
        RAISE EXCEPTION 'execution schedule origin does not match current published evidence';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE OR REPLACE FUNCTION guard_execution_defer_replacement_consumption() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    consumption_is_exact boolean;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution defer replacement consumptions are immutable';
    END IF;
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'execution defer replacement consumption must be captured by Start';
    END IF;

    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims AS claim
          JOIN execution_sessions AS replacement
            ON replacement.workspace_id = claim.workspace_id
           AND replacement.id = NEW.replacement_execution_session_id
          JOIN execution_session_schedule_origins AS origin
            ON origin.workspace_id = replacement.workspace_id
           AND origin.execution_session_id = replacement.id
          JOIN schedule_defer_replacement_placements AS placement
            ON placement.workspace_id = claim.workspace_id
           AND placement.source_deferred_session_id =
               claim.source_deferred_session_id
           AND placement.schedule_revision_id = origin.schedule_revision_id
           AND placement.source_block_id = origin.source_block_id
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = placement.workspace_id
           AND revision.id = placement.schedule_revision_id
          JOIN schedule_blocks AS block
            ON block.workspace_id = placement.workspace_id
           AND block.schedule_revision_id = placement.schedule_revision_id
           AND block.source_block_id = placement.source_block_id
         WHERE claim.workspace_id = NEW.workspace_id
           AND claim.source_deferred_session_id = NEW.source_deferred_session_id
           AND claim.actionable
           AND replacement.state = 'active'
           AND replacement.revision = 1
           AND replacement.accumulated_seconds = 0
           AND replacement.actual_seconds IS NULL
           AND replacement.item_id = claim.item_id
           AND replacement.execution_epoch = claim.execution_epoch
           AND replacement.occurrence_id IS NOT DISTINCT FROM claim.occurrence_id
           AND replacement.session_index = claim.replacement_session_index
           AND replacement.planned_block_id = placement.source_block_id
           AND replacement.created_at = NEW.consumed_at
           AND replacement.started_at = NEW.consumed_at
           AND replacement.updated_at = NEW.consumed_at
           AND origin.created_at = NEW.consumed_at
           AND origin.planned_duration_seconds = claim.remaining_duration_seconds
           AND placement.item_id = claim.item_id
           AND placement.execution_epoch = claim.execution_epoch
           AND placement.occurrence_id IS NOT DISTINCT FROM claim.occurrence_id
           AND placement.replacement_session_index = claim.replacement_session_index
           AND placement.remaining_duration_seconds = claim.remaining_duration_seconds
           AND placement.move_start = claim.move_start
           AND placement.move_end = claim.move_end
           AND revision.state = 'published'
           AND block.item_id = claim.item_id
           AND block.starts_at = claim.move_start
           AND block.ends_at = claim.move_end
           AND block.constraint_snapshot ->> 'session_index'
               = claim.replacement_session_index::text
           AND block.constraint_snapshot ->> 'occurrence_id'
               IS NOT DISTINCT FROM claim.occurrence_id::text
           AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))
               = claim.remaining_duration_seconds::numeric
    ) INTO consumption_is_exact;
    IF NOT consumption_is_exact THEN
        RAISE EXCEPTION 'execution defer replacement consumption does not match its claim';
    END IF;
    RETURN NEW;
END
$guard$;

-- The single AFTER INSERT trigger is the only origin/consumption writer. It
-- also records ordinary fresh indices, making raw SQL obey the same cutover.
CREATE FUNCTION record_execution_start_evidence() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    physical_row record;
    origin_count bigint;
BEGIN
    SELECT reservation_kind, execution_session_id, source_deferred_session_id
      INTO physical_row
      FROM execution_physical_indices
     WHERE workspace_id = NEW.workspace_id
       AND item_id = NEW.item_id
       AND occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
       AND session_index = NEW.session_index
     FOR SHARE;
    IF NOT FOUND THEN
        INSERT INTO execution_physical_indices (
            workspace_id,
            item_id,
            occurrence_id,
            session_index,
            reservation_kind,
            execution_session_id,
            source_deferred_session_id,
            reserved_at
        ) VALUES (
            NEW.workspace_id,
            NEW.item_id,
            NEW.occurrence_id,
            NEW.session_index,
            'execution_start',
            NEW.id,
            NULL,
            NEW.created_at
        );
        physical_row.reservation_kind := 'execution_start';
        physical_row.execution_session_id := NEW.id;
        physical_row.source_deferred_session_id := NULL;
    ELSIF NEW.state <> 'active'
       OR physical_row.reservation_kind <> 'defer_replacement'
       OR physical_row.source_deferred_session_id IS NULL
    THEN
        RAISE EXCEPTION 'execution physical session index is already reserved';
    END IF;

    IF NEW.state <> 'active' THEN
        RETURN NEW;
    END IF;

    INSERT INTO execution_session_schedule_origins (
        workspace_id,
        execution_session_id,
        schedule_revision_id,
        source_block_id,
        item_id,
        item_revision,
        execution_epoch,
        occurrence_id,
        session_index,
        planned_duration_seconds,
        created_at
    )
    SELECT NEW.workspace_id,
           NEW.id,
           revision.id,
           block.source_block_id,
           NEW.item_id,
           NEW.item_revision,
           NEW.execution_epoch,
           NEW.occurrence_id,
           NEW.session_index,
           EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))::bigint,
           NEW.created_at
      FROM schedule_revisions AS revision
      JOIN schedule_blocks AS block
        ON block.workspace_id = revision.workspace_id
       AND block.schedule_revision_id = revision.id
     WHERE revision.workspace_id = NEW.workspace_id
       AND revision.state = 'published'
       AND block.source_block_id = NEW.planned_block_id
       AND block.item_id = NEW.item_id
       AND block.block_kind IN ('planned', 'pinned')
       AND block.is_fixed = (block.block_kind = 'pinned')
       AND block.constraint_snapshot ->> 'source_block_id'
           = block.source_block_id::text
       AND block.constraint_snapshot ->> 'core_kind' = block.block_kind
       AND block.constraint_snapshot ->> 'occurrence_id'
           IS NOT DISTINCT FROM NEW.occurrence_id::text
       AND block.constraint_snapshot ->> 'session_index'
           = NEW.session_index::text
       AND EXISTS (
           SELECT 1
             FROM schedule_revision_details AS detail
            WHERE detail.workspace_id = revision.workspace_id
              AND detail.schedule_revision_id = revision.id
              AND detail.result_snapshot -> 'compose' -> 'source_item_revisions'
                   ->> NEW.item_id::text = NEW.item_revision::text
       );
    GET DIAGNOSTICS origin_count = ROW_COUNT;
    IF origin_count > 1 THEN
        RAISE EXCEPTION 'execution Start matched multiple published schedule origins';
    END IF;

    IF physical_row.reservation_kind = 'defer_replacement' THEN
        IF origin_count <> 1 THEN
            RAISE EXCEPTION 'execution replacement Start lacks its schedule origin';
        END IF;
        INSERT INTO execution_defer_replacement_consumptions (
            workspace_id,
            source_deferred_session_id,
            replacement_execution_session_id,
            consumed_at
        ) VALUES (
            NEW.workspace_id,
            physical_row.source_deferred_session_id,
            NEW.id,
            NEW.created_at
        );
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_sessions_record_start_evidence
    AFTER INSERT ON execution_sessions
    FOR EACH ROW EXECUTE FUNCTION record_execution_start_evidence();

-- Repository-driven Defer updates the session first and inserts its claim in
-- the same transaction. A deferred constraint lets that ordered write finish
-- while ensuring raw SQL cannot commit a deferred promise without its exact
-- fresh-index claim.
CREATE FUNCTION require_execution_defer_replacement_claim() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM execution_defer_replacement_claims AS claim
         WHERE claim.workspace_id = NEW.workspace_id
           AND claim.source_deferred_session_id = NEW.id
           AND claim.item_id = NEW.item_id
           AND claim.source_item_revision = NEW.item_revision
           AND claim.execution_epoch = NEW.execution_epoch
           AND claim.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
           AND claim.source_session_index = NEW.session_index
           AND claim.move_start = NEW.move_start
           AND claim.move_end = NEW.move_end
    ) THEN
        RAISE EXCEPTION 'deferred execution session lacks its replacement claim';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE CONSTRAINT TRIGGER execution_sessions_require_defer_claim
    AFTER INSERT OR UPDATE ON execution_sessions
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN (NEW.state = 'deferred')
    EXECUTE FUNCTION require_execution_defer_replacement_claim();

-- Extend the durable revision seal with claim coverage and the inverse
-- physical-index fence. The application already owns execution_state and the
-- canonical advisory lock; direct SQL acquires both here and fails closed.
CREATE OR REPLACE FUNCTION guard_schedule_revision_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    detail_count bigint;
    invalid_blocks boolean;
    missing_claims boolean;
    stale_placements boolean;
    canonical_lock_acquired boolean;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.state <> 'draft'
           OR NEW.published_at IS NOT NULL
           OR NEW.superseded_at IS NOT NULL
        THEN
            RAISE EXCEPTION 'schedule revisions must be inserted as drafts';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'sealed schedule revisions are immutable';
    END IF;

    INSERT INTO execution_state (workspace_id)
    VALUES (OLD.workspace_id)
    ON CONFLICT (workspace_id) DO NOTHING;
    PERFORM workspace_id
      FROM execution_state
     WHERE workspace_id = OLD.workspace_id
     FOR UPDATE NOWAIT;
    SELECT pg_try_advisory_xact_lock(
        hashtextextended('dayweave.items.v1:' || OLD.workspace_id::text, 0)
    ) INTO canonical_lock_acquired;
    IF NOT canonical_lock_acquired THEN
        RAISE EXCEPTION 'schedule revision seal did not acquire canonical lock order';
    END IF;

    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.revision_number IS DISTINCT FROM NEW.revision_number
       OR OLD.parent_revision_id IS DISTINCT FROM NEW.parent_revision_id
       OR OLD.horizon_start IS DISTINCT FROM NEW.horizon_start
       OR OLD.horizon_end IS DISTINCT FROM NEW.horizon_end
       OR OLD.timezone_name IS DISTINCT FROM NEW.timezone_name
       OR OLD.solver_version IS DISTINCT FROM NEW.solver_version
       OR OLD.input_digest IS DISTINCT FROM NEW.input_digest
       OR OLD.publication_hash IS DISTINCT FROM NEW.publication_hash
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
    THEN
        RAISE EXCEPTION 'sealed schedule revision fields are immutable';
    END IF;

    IF OLD.state = 'draft' AND NEW.state = 'published' THEN
        SELECT COUNT(*) INTO detail_count
          FROM schedule_revision_details
         WHERE workspace_id = OLD.workspace_id
           AND schedule_revision_id = OLD.id;
        IF detail_count <> 1
           OR OLD.published_at IS NOT NULL
           OR NEW.published_at IS NULL
           OR OLD.superseded_at IS NOT NULL
           OR NEW.superseded_at IS NOT NULL
        THEN
            RAISE EXCEPTION 'schedule revision cannot be sealed';
        END IF;

        WITH execution_blocks AS (
            SELECT block.*,
                   CASE
                       WHEN block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$'
                        AND (block.constraint_snapshot ->> 'session_index')::numeric
                              BETWEEN 0 AND 65535
                       THEN (block.constraint_snapshot ->> 'session_index')::integer
                   END AS parsed_session_index,
                   CASE
                       WHEN block.constraint_snapshot ->> 'occurrence_id' IS NULL THEN NULL
                       WHEN block.constraint_snapshot ->> 'occurrence_id' ~*
                            '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                       THEN (block.constraint_snapshot ->> 'occurrence_id')::uuid
                   END AS parsed_occurrence_id
              FROM schedule_blocks AS block
             WHERE block.workspace_id = OLD.workspace_id
               AND block.schedule_revision_id = OLD.id
               AND block.item_id IS NOT NULL
               AND block.block_kind IN ('planned', 'pinned')
        ), duplicate_indices AS (
            SELECT item_id, parsed_occurrence_id, parsed_session_index
              FROM execution_blocks
             GROUP BY item_id, parsed_occurrence_id, parsed_session_index
            HAVING COUNT(*) <> 1
        )
        SELECT EXISTS (
            SELECT 1
              FROM execution_blocks AS block
              LEFT JOIN execution_physical_indices AS physical
                ON physical.workspace_id = block.workspace_id
               AND physical.item_id = block.item_id
               AND physical.occurrence_id IS NOT DISTINCT FROM block.parsed_occurrence_id
               AND physical.session_index = block.parsed_session_index
             WHERE block.parsed_session_index IS NULL
                OR (
                    block.constraint_snapshot ->> 'occurrence_id' IS NOT NULL
                    AND block.parsed_occurrence_id IS NULL
                )
                OR EXISTS (SELECT 1 FROM duplicate_indices)
                OR (
                    physical.reservation_kind IS NOT NULL
                    AND NOT (
                        EXISTS (
                            SELECT 1
                              FROM execution_sessions AS session
                              JOIN execution_session_schedule_origins AS origin
                                ON origin.workspace_id = session.workspace_id
                               AND origin.execution_session_id = session.id
                              JOIN schedule_blocks AS source
                                ON source.workspace_id = origin.workspace_id
                               AND source.schedule_revision_id = origin.schedule_revision_id
                               AND source.source_block_id = origin.source_block_id
                              JOIN items AS item
                                ON item.workspace_id = session.workspace_id
                               AND item.id = session.item_id
                             WHERE session.workspace_id = block.workspace_id
                               AND session.item_id = block.item_id
                               AND session.occurrence_id IS NOT DISTINCT FROM
                                   block.parsed_occurrence_id
                               AND session.session_index = block.parsed_session_index
                               AND session.state IN ('active', 'paused')
                               AND session.execution_epoch = item.execution_epoch
                               AND item.trashed_at IS NULL
                               AND item.status NOT IN ('completed', 'skipped', 'cancelled')
                               AND NOT EXISTS (
                                   SELECT 1
                                     FROM item_hierarchy AS edge
                                     JOIN items AS child
                                       ON child.workspace_id = edge.workspace_id
                                      AND child.id = edge.child_item_id
                                    WHERE edge.workspace_id = item.workspace_id
                                      AND edge.parent_item_id = item.id
                                      AND child.trashed_at IS NULL
                               )
                               AND block.source_block_id = origin.source_block_id
                               AND block.starts_at = source.starts_at
                               AND block.ends_at = source.ends_at
                               AND EXTRACT(EPOCH FROM (block.ends_at - block.starts_at))
                                   = origin.planned_duration_seconds::numeric
                               AND block.block_kind = 'pinned'
                               AND block.is_fixed
                               AND block.constraint_snapshot ->> 'source_block_id'
                                   = block.source_block_id::text
                               AND block.constraint_snapshot ->> 'core_kind' = 'pinned'
                        )
                        OR (
                            physical.reservation_kind = 'defer_replacement'
                            AND EXISTS (
                            SELECT 1
                              FROM schedule_defer_replacement_placements AS placement
                             WHERE placement.workspace_id = block.workspace_id
                               AND placement.schedule_revision_id = block.schedule_revision_id
                               AND placement.source_deferred_session_id =
                                   physical.source_deferred_session_id
                               AND placement.source_block_id = block.source_block_id
                               AND placement.item_id = block.item_id
                               AND placement.occurrence_id IS NOT DISTINCT FROM
                                   block.parsed_occurrence_id
                               AND placement.replacement_session_index =
                                   block.parsed_session_index
                               AND placement.move_start = block.starts_at
                               AND placement.move_end = block.ends_at
                            )
                        )
                    )
                )
        ) INTO invalid_blocks;
        IF invalid_blocks THEN
            RAISE EXCEPTION 'schedule revision collides with execution physical history';
        END IF;

        SELECT EXISTS (
            SELECT 1
              FROM execution_defer_replacement_claims AS claim
              JOIN items AS item
                ON item.workspace_id = claim.workspace_id
               AND item.id = claim.item_id
             WHERE claim.workspace_id = OLD.workspace_id
               AND claim.actionable
               AND item.execution_epoch = claim.execution_epoch
               AND item.trashed_at IS NULL
               AND item.status NOT IN ('completed', 'skipped', 'cancelled')
               AND NOT EXISTS (
                   SELECT 1
                     FROM item_hierarchy AS edge
                     JOIN items AS child
                       ON child.workspace_id = edge.workspace_id
                      AND child.id = edge.child_item_id
                    WHERE edge.workspace_id = item.workspace_id
                      AND edge.parent_item_id = item.id
                      AND child.trashed_at IS NULL
               )
               AND claim.move_start < OLD.horizon_end
               AND claim.move_end > OLD.horizon_start
               AND NOT EXISTS (
                   SELECT 1
                     FROM execution_defer_replacement_consumptions AS consumption
                    WHERE consumption.workspace_id = claim.workspace_id
                      AND consumption.source_deferred_session_id =
                          claim.source_deferred_session_id
               )
               AND NOT EXISTS (
                   SELECT 1
                     FROM schedule_defer_replacement_placements AS placement
                    WHERE placement.workspace_id = claim.workspace_id
                      AND placement.schedule_revision_id = OLD.id
                      AND placement.source_deferred_session_id =
                          claim.source_deferred_session_id
                      AND placement.item_id = claim.item_id
                      AND placement.item_revision = item.revision
                      AND placement.execution_epoch = claim.execution_epoch
                      AND placement.occurrence_id IS NOT DISTINCT FROM claim.occurrence_id
                      AND placement.replacement_session_index =
                          claim.replacement_session_index
                      AND placement.remaining_duration_seconds =
                          claim.remaining_duration_seconds
                      AND placement.move_start = claim.move_start
                      AND placement.move_end = claim.move_end
               )
        ) INTO missing_claims;
        IF missing_claims THEN
            RAISE EXCEPTION 'schedule revision omits a live defer replacement claim';
        END IF;

        SELECT EXISTS (
            SELECT 1
              FROM schedule_defer_replacement_placements AS placement
              JOIN execution_defer_replacement_claims AS claim
                ON claim.workspace_id = placement.workspace_id
               AND claim.source_deferred_session_id =
                   placement.source_deferred_session_id
              JOIN items AS item
                ON item.workspace_id = claim.workspace_id
               AND item.id = claim.item_id
              JOIN schedule_revision_details AS detail
                ON detail.workspace_id = placement.workspace_id
               AND detail.schedule_revision_id = placement.schedule_revision_id
             WHERE placement.workspace_id = OLD.workspace_id
               AND placement.schedule_revision_id = OLD.id
               AND (
                   NOT claim.actionable
                   OR item.execution_epoch <> claim.execution_epoch
                   OR item.revision <> placement.item_revision
                   OR item.trashed_at IS NOT NULL
                   OR item.status IN ('completed', 'skipped', 'cancelled')
                   OR EXISTS (
                       SELECT 1
                         FROM item_hierarchy AS edge
                         JOIN items AS child
                           ON child.workspace_id = edge.workspace_id
                          AND child.id = edge.child_item_id
                        WHERE edge.workspace_id = item.workspace_id
                          AND edge.parent_item_id = item.id
                          AND child.trashed_at IS NULL
                   )
                   OR detail.result_snapshot -> 'compose' -> 'source_item_revisions'
                        ->> placement.item_id::text
                        IS DISTINCT FROM placement.item_revision::text
                   OR EXISTS (
                       SELECT 1
                         FROM execution_defer_replacement_consumptions AS consumption
                        WHERE consumption.workspace_id = claim.workspace_id
                          AND consumption.source_deferred_session_id =
                              claim.source_deferred_session_id
                   )
               )
        ) INTO stale_placements;
        IF stale_placements THEN
            RAISE EXCEPTION 'schedule revision contains stale defer replacement evidence';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.state = 'published' AND NEW.state = 'superseded' THEN
        IF OLD.published_at IS NULL
           OR NEW.published_at IS DISTINCT FROM OLD.published_at
           OR OLD.superseded_at IS NOT NULL
           OR NEW.superseded_at IS NULL
           OR NEW.superseded_at < OLD.published_at
        THEN
            RAISE EXCEPTION 'published schedule revision cannot be superseded';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.state = 'draft' AND NEW.state = 'discarded' THEN
        IF NEW.published_at IS DISTINCT FROM OLD.published_at
           OR NEW.superseded_at IS DISTINCT FROM OLD.superseded_at
        THEN
            RAISE EXCEPTION 'draft schedule revision cannot be discarded';
        END IF;
        RETURN NEW;
    END IF;

    RAISE EXCEPTION 'sealed schedule revisions are immutable';
END
$guard$;
