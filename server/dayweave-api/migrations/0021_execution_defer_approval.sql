-- Durable, server-authoritative assessment and approval evidence for execution
-- Defer. Historical v20 claims remain readable as legacy v0 rows, but every
-- claim created after this migration must be authorized by one exact, live v1
-- assessment.

-- This key lets an assessment reference the complete immutable schedule origin
-- tuple, rather than only the execution session half of the attestation.
ALTER TABLE execution_session_schedule_origins
    ADD CONSTRAINT execution_session_schedule_origins_exact_origin_uq
        UNIQUE (
            workspace_id,
            execution_session_id,
            schedule_revision_id,
            source_block_id
        );

CREATE TABLE execution_defer_assessments (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    schema_version smallint NOT NULL CHECK (schema_version = 1),
    execution_state_revision bigint NOT NULL CHECK (execution_state_revision > 0),
    source_execution_session_id uuid NOT NULL,
    source_execution_session_revision bigint NOT NULL
        CHECK (source_execution_session_revision > 0),
    source_schedule_revision_id uuid NOT NULL,
    source_block_id uuid NOT NULL,
    current_schedule_revision_id uuid NOT NULL,
    current_schedule_revision_number bigint NOT NULL
        CHECK (current_schedule_revision_number > 0),
    current_publication_hash bytea NOT NULL
        CHECK (octet_length(current_publication_hash) = 32),
    item_id uuid NOT NULL,
    source_item_revision bigint NOT NULL CHECK (source_item_revision > 0),
    current_item_revision bigint NOT NULL CHECK (current_item_revision > 0),
    execution_epoch bigint NOT NULL CHECK (execution_epoch > 0),
    occurrence_id uuid,
    source_session_index integer NOT NULL
        CHECK (source_session_index BETWEEN 0 AND 65535),
    replacement_session_index integer NOT NULL
        CHECK (replacement_session_index BETWEEN 0 AND 65535),
    planned_duration_seconds bigint NOT NULL
        CHECK (planned_duration_seconds > 0),
    credited_before_seconds bigint NOT NULL
        CHECK (credited_before_seconds >= 0),
    effective_actual_seconds bigint NOT NULL
        CHECK (effective_actual_seconds >= 0),
    credited_after_seconds bigint NOT NULL
        CHECK (credited_after_seconds >= 0),
    credited_source_seconds bigint NOT NULL
        CHECK (credited_source_seconds >= 0),
    remaining_duration_seconds bigint NOT NULL
        CHECK (remaining_duration_seconds BETWEEN 60 AND 86400),
    scheduler_slot_seconds integer NOT NULL
        CHECK (
            scheduler_slot_seconds BETWEEN 60 AND 3600
            AND scheduler_slot_seconds % 60 = 0
        ),
    target_start timestamptz NOT NULL,
    target_end timestamptz NOT NULL,
    environment_digest bytea NOT NULL CHECK (octet_length(environment_digest) = 32),
    assessment_digest bytea NOT NULL CHECK (octet_length(assessment_digest) = 32),
    approval_required boolean NOT NULL,
    private_context jsonb NOT NULL,
    violations jsonb NOT NULL,
    assessed_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, user_id, assessment_digest),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (
        workspace_id,
        source_execution_session_id,
        source_schedule_revision_id,
        source_block_id
    ) REFERENCES execution_session_schedule_origins(
        workspace_id,
        execution_session_id,
        schedule_revision_id,
        source_block_id
    ),
    FOREIGN KEY (workspace_id, current_schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    CHECK (replacement_session_index > source_session_index),
    CHECK (source_item_revision = current_item_revision),
    CHECK (planned_duration_seconds % 60 = 0),
    CHECK (
        credited_after_seconds::numeric
            = credited_before_seconds::numeric + effective_actual_seconds::numeric
    ),
    CHECK (credited_source_seconds % 60 = 0),
    CHECK (
        credited_source_seconds::numeric
            = LEAST(
                planned_duration_seconds::numeric,
                60 * (
                    CEIL(credited_after_seconds::numeric / 60)
                        - CEIL(credited_before_seconds::numeric / 60)
                )
            )
    ),
    CHECK (remaining_duration_seconds % 60 = 0),
    -- Scheduling rounds aggregate execution credit once. This avoids double
    -- rounding when a previous session ended partway through a minute.
    CHECK (
        remaining_duration_seconds::numeric
            = planned_duration_seconds::numeric
                - credited_source_seconds::numeric
    ),
    CHECK (target_start > expires_at),
    CHECK (target_end > target_start),
    CHECK (target_end <= target_start + interval '24 hours'),
    CHECK (date_trunc('minute', target_start) = target_start),
    CHECK (date_trunc('minute', target_end) = target_end),
    CHECK (
        EXTRACT(EPOCH FROM (target_end - target_start))
            = remaining_duration_seconds::numeric
    ),
    -- Start is aligned to the exact retained scheduler grid. The end is the
    -- normalized remainder and intentionally need not land on that grid.
    CHECK (
        MOD(
            EXTRACT(EPOCH FROM target_start)::numeric,
            scheduler_slot_seconds::numeric
        ) = 0
    ),
    CHECK (jsonb_typeof(private_context) = 'object'),
    CHECK (octet_length(private_context::text) <= 1048576),
    CHECK (jsonb_typeof(violations) = 'array'),
    CHECK (jsonb_array_length(violations) <= 4096),
    CHECK (octet_length(violations::text) <= 1048576),
    CHECK (approval_required = (jsonb_array_length(violations) > 0)),
    CHECK (expires_at > assessed_at),
    CHECK (expires_at <= assessed_at + interval '5 minutes')
);

CREATE INDEX execution_defer_assessments_lookup_idx
    ON execution_defer_assessments (
        workspace_id,
        user_id,
        assessment_digest,
        expires_at
    );

CREATE INDEX execution_defer_assessments_source_idx
    ON execution_defer_assessments (
        workspace_id,
        source_execution_session_id,
        expires_at,
        id
    );

CREATE INDEX execution_defer_assessments_expiry_idx
    ON execution_defer_assessments (expires_at, workspace_id, id);

-- Existing claims predate assessment authority. Give them an explicit v0
-- shape, then remove the defaults so an older writer fails closed instead of
-- silently minting another legacy claim after the migration.
ALTER TABLE execution_defer_replacement_claims
    ADD COLUMN authorization_schema_version smallint NOT NULL DEFAULT 0,
    ADD COLUMN authorization_kind varchar(32) NOT NULL DEFAULT 'legacy_unassessed',
    ADD COLUMN assessment_id uuid,
    ADD COLUMN authorized_by_user_id uuid,
    ADD COLUMN environment_digest bytea,
    ADD COLUMN assessment_digest bytea,
    ADD COLUMN approved_assessment_digest bytea,
    ADD COLUMN assessment_expires_at timestamptz;

ALTER TABLE execution_defer_replacement_claims
    ALTER COLUMN authorization_schema_version DROP DEFAULT,
    ALTER COLUMN authorization_kind DROP DEFAULT,
    ADD CONSTRAINT execution_defer_claims_authorization_schema_check
        CHECK (authorization_schema_version IN (0, 1)),
    ADD CONSTRAINT execution_defer_claims_authorization_kind_check
        CHECK (authorization_kind IN (
            'legacy_unassessed',
            'conflict_free',
            'explicit_approval'
        )),
    ADD CONSTRAINT execution_defer_claims_environment_digest_check
        CHECK (environment_digest IS NULL OR octet_length(environment_digest) = 32),
    ADD CONSTRAINT execution_defer_claims_assessment_digest_check
        CHECK (assessment_digest IS NULL OR octet_length(assessment_digest) = 32),
    ADD CONSTRAINT execution_defer_claims_approved_digest_check
        CHECK (
            approved_assessment_digest IS NULL
            OR octet_length(approved_assessment_digest) = 32
        ),
    ADD CONSTRAINT execution_defer_claims_authorization_shape_check
        CHECK (
            (
                authorization_schema_version = 0
                AND authorization_kind = 'legacy_unassessed'
                AND assessment_id IS NULL
                AND authorized_by_user_id IS NULL
                AND environment_digest IS NULL
                AND assessment_digest IS NULL
                AND approved_assessment_digest IS NULL
                AND assessment_expires_at IS NULL
            )
            OR (
                authorization_schema_version = 1
                AND authorization_kind IN ('conflict_free', 'explicit_approval')
                AND assessment_id IS NOT NULL
                AND authorized_by_user_id IS NOT NULL
                AND environment_digest IS NOT NULL
                AND assessment_digest IS NOT NULL
                AND assessment_expires_at IS NOT NULL
                AND (
                    (
                        authorization_kind = 'conflict_free'
                        AND approved_assessment_digest IS NULL
                    )
                    OR (
                        authorization_kind = 'explicit_approval'
                        AND approved_assessment_digest = assessment_digest
                    )
                )
            )
        ),
    ADD CONSTRAINT execution_defer_claims_assessment_fk
        FOREIGN KEY (workspace_id, assessment_id)
        REFERENCES execution_defer_assessments(workspace_id, id),
    ADD CONSTRAINT execution_defer_claims_authorizing_user_fk
        FOREIGN KEY (workspace_id, authorized_by_user_id)
        REFERENCES workspace_members(workspace_id, user_id);

CREATE UNIQUE INDEX execution_defer_claims_assessment_uq
    ON execution_defer_replacement_claims (workspace_id, assessment_id)
    WHERE assessment_id IS NOT NULL;

CREATE FUNCTION guard_execution_defer_assessment() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    state_row record;
    source_row record;
    item_row record;
    publication_row record;
    slot_minutes_text text;
    expected_slot_seconds integer;
    semantic_high_water integer;
    invalid_violation boolean;
    canonical_credited_before numeric;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'execution defer assessments are immutable';
    ELSIF TG_OP = 'DELETE' THEN
        IF OLD.expires_at > statement_timestamp() THEN
            RAISE EXCEPTION 'live execution defer assessments cannot be deleted';
        END IF;
        IF EXISTS (
            SELECT 1
              FROM execution_defer_replacement_claims AS claim
             WHERE claim.workspace_id = OLD.workspace_id
               AND claim.assessment_id = OLD.id
        ) THEN
            RAISE EXCEPTION 'applied execution defer assessments cannot be deleted';
        END IF;
        RETURN OLD;
    ELSIF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'invalid execution defer assessment operation';
    END IF;

    IF NEW.assessed_at > statement_timestamp()
       OR NEW.expires_at <= statement_timestamp()
    THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DW001',
            MESSAGE = 'execution defer assessment lifetime is invalid';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM jsonb_array_elements(NEW.violations) AS violation(value)
         WHERE jsonb_typeof(violation.value) <> 'object'
    ) INTO invalid_violation;
    IF invalid_violation THEN
        RAISE EXCEPTION 'execution defer assessment violations must be objects';
    END IF;

    SELECT revision, active_session_id, updated_at
      INTO state_row
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR SHARE;
    IF NOT FOUND
       OR state_row.revision IS DISTINCT FROM NEW.execution_state_revision
       OR state_row.active_session_id IS DISTINCT FROM NEW.source_execution_session_id
       OR NEW.target_start <= (CASE
            WHEN state_row.updated_at >= NEW.assessed_at
                THEN state_row.updated_at + interval '1 microsecond'
            ELSE NEW.assessed_at
          END)
    THEN
        RAISE EXCEPTION 'execution defer assessment execution state is stale';
    END IF;

    SELECT session.item_id,
           session.item_revision,
           session.execution_epoch,
           session.occurrence_id,
           session.session_index,
           session.state,
           session.revision,
           session.planned_block_id,
           session.accumulated_seconds,
           session.actual_seconds,
           origin.schedule_revision_id,
           origin.source_block_id,
           origin.planned_duration_seconds
      INTO source_row
      FROM execution_sessions AS session
      JOIN execution_session_schedule_origins AS origin
        ON origin.workspace_id = session.workspace_id
       AND origin.execution_session_id = session.id
     WHERE session.workspace_id = NEW.workspace_id
       AND session.id = NEW.source_execution_session_id
     FOR SHARE OF session, origin;
    IF NOT FOUND
       OR source_row.state <> 'paused'
       OR source_row.actual_seconds IS NOT NULL
       OR source_row.item_id IS DISTINCT FROM NEW.item_id
       OR source_row.item_revision IS DISTINCT FROM NEW.source_item_revision
       OR source_row.execution_epoch IS DISTINCT FROM NEW.execution_epoch
       OR source_row.occurrence_id IS DISTINCT FROM NEW.occurrence_id
       OR source_row.session_index IS DISTINCT FROM NEW.source_session_index
       OR source_row.revision IS DISTINCT FROM NEW.source_execution_session_revision
       OR source_row.planned_block_id IS DISTINCT FROM NEW.source_block_id
       OR source_row.schedule_revision_id IS DISTINCT FROM NEW.source_schedule_revision_id
       OR source_row.source_block_id IS DISTINCT FROM NEW.source_block_id
       OR source_row.planned_duration_seconds IS DISTINCT FROM NEW.planned_duration_seconds
    THEN
        RAISE EXCEPTION 'execution defer assessment source origin is stale';
    END IF;

    SELECT COALESCE(SUM(session.actual_seconds), 0)
      INTO canonical_credited_before
      FROM execution_sessions AS session
      JOIN execution_session_schedule_origins AS origin
        ON origin.workspace_id = session.workspace_id
       AND origin.execution_session_id = session.id
     WHERE session.workspace_id = NEW.workspace_id
       AND session.item_id = NEW.item_id
       AND session.execution_epoch = NEW.execution_epoch
       AND session.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
       AND session.state IN ('completed', 'deferred');
    IF canonical_credited_before IS DISTINCT FROM NEW.credited_before_seconds::numeric THEN
        RAISE EXCEPTION 'execution defer assessment progress credit is stale';
    END IF;

    SELECT item.revision,
           item.execution_epoch,
           item.status,
           item.trashed_at,
           EXISTS (
               SELECT 1
                 FROM item_hierarchy AS edge
                 JOIN items AS child
                   ON child.workspace_id = edge.workspace_id
                  AND child.id = edge.child_item_id
                WHERE edge.workspace_id = item.workspace_id
                  AND edge.parent_item_id = item.id
                  AND child.trashed_at IS NULL
           ) AS has_live_children
      INTO item_row
      FROM items AS item
     WHERE item.workspace_id = NEW.workspace_id
       AND item.id = NEW.item_id
     FOR SHARE;
    IF NOT FOUND
       OR item_row.revision IS DISTINCT FROM NEW.current_item_revision
       OR item_row.execution_epoch IS DISTINCT FROM NEW.execution_epoch
       OR item_row.trashed_at IS NOT NULL
       OR item_row.status IN ('completed', 'skipped', 'cancelled')
       OR item_row.has_live_children
    THEN
        RAISE EXCEPTION 'execution defer assessment item state is stale';
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM workspace_members AS member
         WHERE member.workspace_id = NEW.workspace_id
           AND member.user_id = NEW.user_id
           AND member.role = 'owner'
           AND member.removed_at IS NULL
    ) THEN
        RAISE EXCEPTION 'execution defer assessment user is not an active member';
    END IF;

    SELECT revision.revision_number,
           revision.created_by_user_id,
           revision.state,
           revision.publication_hash,
           revision.horizon_start,
           revision.horizon_end,
           detail.result_snapshot #>>
               '{planning_request,config,slot_granularity}' AS slot_minutes_text,
           detail.result_snapshot ->> 'schema_version' AS snapshot_schema,
           detail.result_snapshot ->> 'scheduler_publication_schema'
               AS scheduler_publication_schema,
           detail.result_snapshot -> 'compose' -> 'source_item_revisions'
               ->> NEW.item_id::text AS planned_item_revision
      INTO publication_row
      FROM schedule_revisions AS revision
      JOIN schedule_revision_details AS detail
        ON detail.workspace_id = revision.workspace_id
       AND detail.schedule_revision_id = revision.id
       AND detail.user_id = revision.created_by_user_id
     WHERE revision.workspace_id = NEW.workspace_id
       AND revision.id = NEW.current_schedule_revision_id
     FOR SHARE OF revision, detail;
    IF NOT FOUND
       OR publication_row.state <> 'published'
       OR publication_row.created_by_user_id IS DISTINCT FROM NEW.user_id
       OR publication_row.revision_number
            IS DISTINCT FROM NEW.current_schedule_revision_number
       OR publication_row.publication_hash
            IS DISTINCT FROM NEW.current_publication_hash
       OR publication_row.snapshot_schema IS DISTINCT FROM '5'
       OR publication_row.scheduler_publication_schema
            IS DISTINCT FROM 'dayweave-scheduler-publication/5'
       OR publication_row.planned_item_revision
            IS DISTINCT FROM NEW.current_item_revision::text
       OR publication_row.horizon_start > NEW.target_start
       OR publication_row.horizon_end < NEW.target_end
    THEN
        RAISE EXCEPTION 'execution defer assessment publication is stale';
    END IF;
    slot_minutes_text := publication_row.slot_minutes_text;
    IF slot_minutes_text IS NULL
       OR slot_minutes_text !~ '^[0-9]+$'
       OR slot_minutes_text::numeric NOT BETWEEN 1 AND 60
    THEN
        RAISE EXCEPTION 'execution defer assessment publication has invalid scheduler policy';
    END IF;
    expected_slot_seconds := slot_minutes_text::integer * 60;
    IF NEW.scheduler_slot_seconds <> expected_slot_seconds THEN
        RAISE EXCEPTION 'execution defer assessment scheduler policy is stale';
    END IF;

    WITH current_published_block_indices AS (
        SELECT CASE
                   WHEN block.constraint_snapshot ->> 'session_index' ~ '^[0-9]+$'
                    AND (block.constraint_snapshot ->> 'session_index')::numeric
                          BETWEEN 0 AND 65535
                   THEN (block.constraint_snapshot ->> 'session_index')::integer
               END AS session_index
          FROM schedule_blocks AS block
         WHERE block.workspace_id = NEW.workspace_id
           AND block.schedule_revision_id = NEW.current_schedule_revision_id
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
                   SELECT MAX(session_index)
                     FROM current_published_block_indices
               ), -1)
           )
      INTO semantic_high_water;
    IF semantic_high_water >= 65535
       OR NEW.replacement_session_index <> semantic_high_water + 1
    THEN
        RAISE EXCEPTION 'execution defer assessment replacement index is not fresh';
    END IF;

    RETURN NEW;
END
$guard$;

CREATE TRIGGER execution_defer_assessments_guard
    BEFORE INSERT OR UPDATE OR DELETE ON execution_defer_assessments
    FOR EACH ROW EXECUTE FUNCTION guard_execution_defer_assessment();

-- Replace the v20 guard with a version-aware v21 guard. Existing v0 rows are
-- never revalidated or rewritten. A direct SQL writer cannot insert v0, and a
-- v1 insert must copy every authorization and planning fact from one live
-- assessment before the pre-existing source/fresh-index checks are applied.
CREATE OR REPLACE FUNCTION guard_execution_defer_replacement_claim() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    source_row record;
    source_origin_duration bigint;
    semantic_high_water integer;
    assessment_row record;
    state_row record;
    normalized_source_credit bigint;
    current_publication_is_exact boolean;
    current_item_is_exact boolean;
    canonical_credited_before numeric;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'execution defer replacement claims are immutable';
    END IF;
    IF NEW.authorization_schema_version <> 1 THEN
        RAISE EXCEPTION 'new execution defer replacement claims require v1 authorization';
    END IF;

    INSERT INTO execution_state (workspace_id, updated_at)
    VALUES (NEW.workspace_id, NEW.created_at)
    ON CONFLICT (workspace_id) DO NOTHING;
    SELECT revision, active_session_id
      INTO state_row
      FROM execution_state
     WHERE workspace_id = NEW.workspace_id
     FOR UPDATE;

    SELECT assessment.*
      INTO assessment_row
      FROM execution_defer_assessments AS assessment
     WHERE assessment.workspace_id = NEW.workspace_id
       AND assessment.id = NEW.assessment_id
     FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'execution defer replacement assessment is missing';
    END IF;
    IF statement_timestamp() >= assessment_row.expires_at THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DW002',
            MESSAGE = 'execution defer replacement assessment is expired';
    END IF;
    IF assessment_row.user_id IS DISTINCT FROM NEW.authorized_by_user_id
       OR assessment_row.environment_digest IS DISTINCT FROM NEW.environment_digest
       OR assessment_row.assessment_digest IS DISTINCT FROM NEW.assessment_digest
       OR assessment_row.expires_at IS DISTINCT FROM NEW.assessment_expires_at
       OR (
           assessment_row.approval_required
           AND (
               NEW.authorization_kind <> 'explicit_approval'
               OR NEW.approved_assessment_digest
                    IS DISTINCT FROM assessment_row.assessment_digest
           )
       )
       OR (
           NOT assessment_row.approval_required
           AND (
               NEW.authorization_kind <> 'conflict_free'
               OR NEW.approved_assessment_digest IS NOT NULL
           )
       )
    THEN
        RAISE EXCEPTION 'execution defer replacement authorization is invalid or expired';
    END IF;

    IF state_row.revision IS DISTINCT FROM assessment_row.execution_state_revision
       OR state_row.active_session_id
            IS DISTINCT FROM assessment_row.source_execution_session_id
    THEN
        RAISE EXCEPTION 'execution defer replacement assessment is stale';
    END IF;

    SELECT item_id, item_revision, execution_epoch, occurrence_id, session_index,
           state, revision, actual_seconds, move_start, move_end
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

    IF NEW.planned_duration_source <> 'published_origin'
       OR source_origin_duration IS NULL
       OR NEW.planned_duration_seconds <> source_origin_duration
       OR NEW.consumed_before_seconds <> 0
       OR source_row.actual_seconds IS NULL
    THEN
        RAISE EXCEPTION 'execution defer replacement claim has invalid origin duration';
    END IF;
    SELECT COALESCE(SUM(session.actual_seconds), 0)
      INTO canonical_credited_before
      FROM execution_sessions AS session
      JOIN execution_session_schedule_origins AS origin
        ON origin.workspace_id = session.workspace_id
       AND origin.execution_session_id = session.id
     WHERE session.workspace_id = NEW.workspace_id
       AND session.item_id = NEW.item_id
       AND session.execution_epoch = NEW.execution_epoch
       AND session.occurrence_id IS NOT DISTINCT FROM NEW.occurrence_id
       AND session.state IN ('completed', 'deferred')
       AND session.id <> NEW.source_deferred_session_id;
    normalized_source_credit := LEAST(
        source_origin_duration::numeric,
        60 * (
            CEIL(
                (canonical_credited_before + source_row.actual_seconds::numeric) / 60
            ) - CEIL(canonical_credited_before / 60)
        )
    )::bigint;
    IF canonical_credited_before
            IS DISTINCT FROM assessment_row.credited_before_seconds::numeric
       OR NEW.consumed_by_source_seconds <> normalized_source_credit
       OR normalized_source_credit <> assessment_row.credited_source_seconds
    THEN
        RAISE EXCEPTION 'execution defer replacement claim has invalid normalized duration';
    END IF;

    IF assessment_row.source_execution_session_id
            IS DISTINCT FROM NEW.source_deferred_session_id
       OR assessment_row.source_execution_session_revision + 1
            IS DISTINCT FROM source_row.revision
       OR assessment_row.item_id IS DISTINCT FROM NEW.item_id
       OR assessment_row.source_item_revision IS DISTINCT FROM NEW.source_item_revision
       OR assessment_row.execution_epoch IS DISTINCT FROM NEW.execution_epoch
       OR assessment_row.occurrence_id IS DISTINCT FROM NEW.occurrence_id
       OR assessment_row.source_session_index IS DISTINCT FROM NEW.source_session_index
       OR assessment_row.replacement_session_index
            IS DISTINCT FROM NEW.replacement_session_index
       OR assessment_row.planned_duration_seconds
            IS DISTINCT FROM NEW.planned_duration_seconds
       OR assessment_row.effective_actual_seconds
            IS DISTINCT FROM source_row.actual_seconds
       OR assessment_row.credited_after_seconds::numeric
            IS DISTINCT FROM (
                canonical_credited_before + source_row.actual_seconds::numeric
            )
       OR assessment_row.remaining_duration_seconds
            IS DISTINCT FROM NEW.remaining_duration_seconds
       OR assessment_row.target_start IS DISTINCT FROM NEW.move_start
       OR assessment_row.target_end IS DISTINCT FROM NEW.move_end
    THEN
        RAISE EXCEPTION 'execution defer replacement claim does not match its assessment';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM workspace_members AS member
         WHERE member.workspace_id = NEW.workspace_id
           AND member.user_id = NEW.authorized_by_user_id
           AND member.role = 'owner'
           AND member.removed_at IS NULL
    ) INTO current_item_is_exact;
    IF NOT current_item_is_exact THEN
        RAISE EXCEPTION 'execution defer replacement authorizer is no longer active';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM items AS item
         WHERE item.workspace_id = NEW.workspace_id
           AND item.id = assessment_row.item_id
           AND item.revision = assessment_row.current_item_revision
           AND item.execution_epoch = assessment_row.execution_epoch
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
    ) INTO current_item_is_exact;
    IF NOT current_item_is_exact THEN
        RAISE EXCEPTION 'execution defer replacement item state is stale';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM schedule_revisions AS revision
         WHERE revision.workspace_id = NEW.workspace_id
           AND revision.id = assessment_row.current_schedule_revision_id
           AND revision.revision_number
                = assessment_row.current_schedule_revision_number
           AND revision.state = 'published'
           AND revision.publication_hash
                = assessment_row.current_publication_hash
    ) INTO current_publication_is_exact;
    IF NOT current_publication_is_exact THEN
        RAISE EXCEPTION 'execution defer replacement publication is stale';
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
