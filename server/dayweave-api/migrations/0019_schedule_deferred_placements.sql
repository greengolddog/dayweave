-- Bind a terminal execution defer to the exact immutable schedule block that
-- may restart its semantic session. The binding is inserted while a schedule
-- revision is still a draft and remains admissible after that revision is
-- published or superseded.

CREATE TABLE schedule_deferred_placements (
    workspace_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    deferred_execution_session_id uuid NOT NULL,
    source_block_id uuid NOT NULL,
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    occurrence_id uuid,
    session_index integer NOT NULL CHECK (session_index BETWEEN 0 AND 65535),
    move_start timestamptz NOT NULL,
    move_end timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (
        workspace_id,
        schedule_revision_id,
        deferred_execution_session_id
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
    FOREIGN KEY (workspace_id, deferred_execution_session_id)
        REFERENCES execution_sessions(workspace_id, id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    CHECK (move_start < move_end),
    CHECK (move_end <= move_start + interval '24 hours')
);

-- Start looks up a binding by the terminal session and the newly selected
-- scheduler block. The copied semantic identity remains part of the evidence
-- checked by both the insertion and Start guards.
CREATE INDEX schedule_deferred_placements_start_guard_idx
    ON schedule_deferred_placements (
        workspace_id,
        deferred_execution_session_id,
        source_block_id
    );

-- An authoritative execution_state pointer wins even when its wall clock is
-- older. Within the non-authoritative history, this index supports the stable
-- updated_at/id semantic-head ordering used by publication and Start.
CREATE INDEX execution_sessions_semantic_head_idx
    ON execution_sessions (
        workspace_id,
        item_id,
        item_revision,
        occurrence_id,
        session_index,
        updated_at DESC,
        id DESC
    );

CREATE FUNCTION guard_schedule_deferred_placement() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    revision_state varchar(24);
    revision_horizon_start timestamptz;
    revision_horizon_end timestamptz;
    deferred_row record;
    block_row record;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'schedule deferred placement evidence is immutable';
    END IF;

    SELECT state, horizon_start, horizon_end
      INTO revision_state, revision_horizon_start, revision_horizon_end
      FROM schedule_revisions
     WHERE workspace_id = NEW.workspace_id
       AND id = NEW.schedule_revision_id
     FOR SHARE;
    IF NOT FOUND OR revision_state <> 'draft' THEN
        RAISE EXCEPTION 'schedule deferred placements require a draft revision';
    END IF;
    IF revision_horizon_start > NEW.move_start
       OR revision_horizon_end < NEW.move_end
    THEN
        RAISE EXCEPTION 'schedule deferred placement lies outside the revision horizon';
    END IF;

    SELECT item_id, item_revision, occurrence_id, session_index, state,
           move_start, move_end
      INTO deferred_row
      FROM execution_sessions
     WHERE workspace_id = NEW.workspace_id
       AND id = NEW.deferred_execution_session_id
     FOR SHARE;
    IF NOT FOUND
       OR deferred_row.state <> 'deferred'
       OR deferred_row.item_id IS DISTINCT FROM NEW.item_id
       OR deferred_row.item_revision IS DISTINCT FROM NEW.item_revision
       OR deferred_row.occurrence_id IS DISTINCT FROM NEW.occurrence_id
       OR deferred_row.session_index IS DISTINCT FROM NEW.session_index
       OR deferred_row.move_start IS DISTINCT FROM NEW.move_start
       OR deferred_row.move_end IS DISTINCT FROM NEW.move_end
    THEN
        RAISE EXCEPTION 'schedule deferred placement does not match the deferred execution session';
    END IF;

    SELECT item_id, block_kind, starts_at, ends_at, is_fixed,
           constraint_snapshot
      INTO block_row
      FROM schedule_blocks
     WHERE workspace_id = NEW.workspace_id
       AND schedule_revision_id = NEW.schedule_revision_id
       AND source_block_id = NEW.source_block_id
     FOR SHARE;
    IF NOT FOUND
       OR block_row.item_id IS DISTINCT FROM NEW.item_id
       OR block_row.block_kind <> 'pinned'
       OR NOT block_row.is_fixed
       OR block_row.starts_at IS DISTINCT FROM NEW.move_start
       OR block_row.ends_at IS DISTINCT FROM NEW.move_end
       OR block_row.constraint_snapshot ->> 'source_block_id'
            IS DISTINCT FROM NEW.source_block_id::text
       OR block_row.constraint_snapshot ->> 'occurrence_id'
            IS DISTINCT FROM NEW.occurrence_id::text
       OR block_row.constraint_snapshot ->> 'session_index'
            IS DISTINCT FROM NEW.session_index::text
       OR block_row.constraint_snapshot ->> 'core_kind'
            IS DISTINCT FROM 'pinned'
    THEN
        RAISE EXCEPTION 'schedule deferred placement does not match an exact pinned block';
    END IF;

    RETURN NEW;
END
$guard$;

CREATE TRIGGER schedule_deferred_placements_guard
    BEFORE INSERT OR UPDATE OR DELETE ON schedule_deferred_placements
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_deferred_placement();

-- Once referenced, the exact session and schedule block snapshots cannot be
-- changed underneath their immutable binding. Revision state may still move
-- from draft to published and later to superseded through the existing seal.
CREATE FUNCTION protect_schedule_deferred_placement_sources() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_TABLE_NAME = 'schedule_blocks' THEN
        IF EXISTS (
            SELECT 1
              FROM schedule_deferred_placements
             WHERE workspace_id = OLD.workspace_id
               AND schedule_revision_id = OLD.schedule_revision_id
               AND source_block_id = OLD.source_block_id
        ) THEN
            RAISE EXCEPTION 'bound deferred schedule blocks are immutable';
        END IF;
    ELSIF TG_TABLE_NAME = 'execution_sessions' THEN
        IF EXISTS (
            SELECT 1
              FROM schedule_deferred_placements
             WHERE workspace_id = OLD.workspace_id
               AND deferred_execution_session_id = OLD.id
        ) THEN
            RAISE EXCEPTION 'bound deferred execution sessions are immutable';
        END IF;
    ELSE
        RAISE EXCEPTION 'invalid deferred placement source guard target';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE TRIGGER schedule_blocks_protect_deferred_placement
    BEFORE UPDATE OR DELETE ON schedule_blocks
    FOR EACH ROW EXECUTE FUNCTION protect_schedule_deferred_placement_sources();

CREATE TRIGGER execution_sessions_protect_deferred_placement
    BEFORE UPDATE OR DELETE ON execution_sessions
    FOR EACH ROW EXECUTE FUNCTION protect_schedule_deferred_placement_sources();

-- This trigger is a database backstop for writers that bypass the repository.
-- It follows the canonical lock order by materializing and locking
-- execution_state before consulting the semantic history or schedule evidence.
-- The database revision-seal trigger itself intentionally does not acquire
-- execution_state. The Start lookup can rely on the copied evidence because
-- the referenced deferred session and pinned block are protected above.
CREATE FUNCTION guard_execution_session_semantic_start() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    authoritative_session_id uuid;
    semantic_head_id uuid;
    semantic_head_state varchar(24);
    has_binding boolean;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        -- Resume mutates the existing paused lease back to active. A terminal
        -- row, however, is history and must never be rewritten into a Start by
        -- a SQL writer that bypasses the repository's insert-only protocol.
        IF NEW.state = 'active' AND OLD.state NOT IN ('active', 'paused') THEN
            RAISE EXCEPTION 'terminal execution semantics cannot be rewritten as active';
        END IF;
        RETURN NEW;
    END IF;

    IF NEW.state <> 'active' THEN
        RETURN NEW;
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, COALESCE(NEW.updated_at, current_timestamp))
    ON CONFLICT (workspace_id) DO NOTHING;

    SELECT active_session_id
      INTO authoritative_session_id
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT session.id, session.state
      INTO semantic_head_id, semantic_head_state
      FROM execution_sessions AS session
     WHERE session.workspace_id = NEW.workspace_id
       AND session.item_id = NEW.item_id
       AND session.item_revision = NEW.item_revision
       AND session.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
       AND session.session_index = NEW.session_index
     ORDER BY COALESCE(session.id = authoritative_session_id, false) DESC,
              session.updated_at DESC,
              session.id DESC
     LIMIT 1
     FOR SHARE;

    -- Existing installs may have no history for this exact semantic session.
    -- Their first Start remains valid without a schedule attestation.
    IF NOT FOUND THEN
        RETURN NEW;
    END IF;

    IF semantic_head_state IN ('completed', 'skipped') THEN
        RAISE EXCEPTION 'completed or skipped execution semantics cannot be restarted';
    END IF;
    IF semantic_head_state IN ('active', 'paused') THEN
        RAISE EXCEPTION 'execution semantic session is already open';
    END IF;
    IF semantic_head_state <> 'deferred' OR NEW.planned_block_id IS NULL THEN
        RAISE EXCEPTION 'deferred execution requires an exact published schedule binding';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM schedule_deferred_placements AS placement
          JOIN schedule_revisions AS revision
            ON revision.workspace_id = placement.workspace_id
           AND revision.id = placement.schedule_revision_id
         WHERE placement.workspace_id = NEW.workspace_id
           AND placement.deferred_execution_session_id = semantic_head_id
           AND placement.source_block_id = NEW.planned_block_id
           AND placement.item_id = NEW.item_id
           AND placement.item_revision = NEW.item_revision
           AND placement.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
           AND placement.session_index = NEW.session_index
           AND revision.state IN ('published', 'superseded')
    ) INTO has_binding;
    IF NOT has_binding THEN
        RAISE EXCEPTION 'deferred execution requires an exact published schedule binding';
    END IF;

    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_sessions_guard_semantic_start
    BEFORE INSERT OR UPDATE ON execution_sessions
    FOR EACH ROW EXECUTE FUNCTION guard_execution_session_semantic_start();
