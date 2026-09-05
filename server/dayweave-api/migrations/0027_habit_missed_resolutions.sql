-- Durable missed-occurrence scheduling projections. Immutable publication
-- evidence remains untouched; this revisioned table records the derived
-- skip/carry/reduction action (or an ask-policy decision prompt) separately.

ALTER TABLE habit_occurrence_evidence
    ADD COLUMN recurrence_ordinal bigint GENERATED ALWAYS AS (
        CASE recurrence_identity->>'type'
            WHEN 'calendar_day' THEN (recurrence_identity->>'bucket_ordinal')::bigint
            WHEN 'calendar_week' THEN (recurrence_identity->>'bucket_ordinal')::bigint
            WHEN 'calendar_month' THEN (recurrence_identity->>'bucket_ordinal')::bigint
            WHEN 'rolling_minutes' THEN (recurrence_identity->>'index')::bigint
            WHEN 'after_completion' THEN 0
            WHEN 'rolling_month' THEN (recurrence_identity->>'index')::bigint
            WHEN 'custom' THEN 0
            WHEN 'custom_rule' THEN (recurrence_identity->>'sequence')::bigint
        END
    ) STORED NOT NULL,
    ADD CONSTRAINT habit_occurrence_evidence_recurrence_ordinal_check
        CHECK (recurrence_ordinal BETWEEN 0 AND 4294967295),
    ADD CONSTRAINT habit_occurrence_evidence_missed_source_uq
    UNIQUE (workspace_id, id, habit_id, planner_occurrence_id);

-- Occurrence outcomes and missed resolutions have independent component
-- revisions. The immutable delta sequence is the only total order for their
-- combined occurrence projection, so expose it as the aggregate revision and
-- retain the component coordinate separately.
ALTER TABLE habit_changes RENAME COLUMN entity_revision TO component_revision;
ALTER TABLE habit_changes RENAME CONSTRAINT habit_changes_entity_revision_check
    TO habit_changes_component_revision_check;
ALTER TABLE habit_changes
    ADD COLUMN entity_revision bigint GENERATED ALWAYS AS (sequence) STORED,
    ADD CONSTRAINT habit_changes_entity_revision_check CHECK (entity_revision > 0);

-- Current-publication membership is separate from immutable occurrence
-- evidence. A recurrence edit may remove a historically admitted occurrence;
-- reduction binding must never mistake that history for current generated work.
CREATE TABLE habit_occurrence_publications (
    workspace_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    occurrence_evidence_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    occurrence_state varchar(16) NOT NULL
        CHECK (occurrence_state IN ('generated', 'completed', 'paused', 'skipped')),
    recorded_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, schedule_revision_id, occurrence_evidence_id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    FOREIGN KEY (workspace_id, occurrence_evidence_id)
        REFERENCES habit_occurrence_evidence(workspace_id, id)
);

CREATE INDEX habit_occurrence_publications_current_idx
    ON habit_occurrence_publications
        (workspace_id, occurrence_evidence_id, schedule_revision_id, occurrence_state);

-- Recover exact membership for a v5 revision that was already current when
-- this migration arrived. Older/non-strict snapshots remain safely unbound
-- until their next exact publication backfills the projection in application code.
INSERT INTO habit_occurrence_publications (
    workspace_id, schedule_revision_id, occurrence_evidence_id,
    item_revision, occurrence_state, recorded_at
)
SELECT evidence.workspace_id, revision.id, evidence.id,
       (details.result_snapshot #>> ARRAY[
           'compose', 'source_item_revisions', evidence.habit_id::text
       ])::bigint,
       occurrence.value->>'state', revision.published_at
FROM schedule_revisions revision
JOIN schedule_revision_details details
  ON details.workspace_id = revision.workspace_id
 AND details.schedule_revision_id = revision.id
CROSS JOIN LATERAL jsonb_array_elements(
    CASE
        WHEN jsonb_typeof(details.result_snapshot #> '{compose,plan,occurrences}') = 'array'
        THEN details.result_snapshot #> '{compose,plan,occurrences}'
        ELSE '[]'::jsonb
    END
) occurrence
JOIN habit_occurrence_evidence evidence
  ON evidence.workspace_id = revision.workspace_id
 AND occurrence.value->>'id' = evidence.planner_occurrence_id::text
 AND occurrence.value->>'series_item_id' = evidence.habit_id::text
WHERE revision.state = 'published'
  AND details.result_snapshot->>'schema_version' = '5'
  AND occurrence.value->>'state' IN ('generated', 'completed', 'paused', 'skipped')
  AND details.result_snapshot #>> ARRAY[
      'compose', 'source_item_revisions', evidence.habit_id::text
  ] ~ '^[1-9][0-9]*$';

CREATE TRIGGER habit_occurrence_publications_immutable
    BEFORE UPDATE OR DELETE ON habit_occurrence_publications
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();

-- A maximum reconcile page contains 200 fixed-shape resolution projections;
-- retain a finite receipt bound while allowing that exact idempotent response.
ALTER TABLE habit_operation_receipts
    DROP CONSTRAINT habit_operation_receipts_response_json_check1,
    ADD CONSTRAINT habit_operation_receipts_response_size_check
        CHECK (octet_length(response_json::text) <= 262144);

CREATE TABLE habit_missed_resolutions (
    workspace_id uuid NOT NULL,
    occurrence_evidence_id uuid NOT NULL,
    habit_id uuid NOT NULL,
    source_planner_occurrence_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    configured_policy varchar(24) NOT NULL
        CHECK (configured_policy IN ('skip', 'carry', 'reduce_frequency', 'ask')),
    action varchar(24) NOT NULL
        CHECK (action IN ('decision_required', 'reduction_pending', 'cancelled', 'skip', 'carry', 'reduce_frequency')),
    cancellation_reason varchar(24)
        CHECK (cancellation_reason IN (
            'source_completed', 'source_skipped', 'source_paused', 'source_obsolete'
        )),
    cancelled_resume_action varchar(24)
        CHECK (cancelled_resume_action IN (
            'decision_required', 'skip', 'carry', 'reduce_frequency'
        )),
    cancelled_explicit_selection boolean NOT NULL DEFAULT false,
    carry_window_start timestamptz,
    carry_window_end timestamptz,
    suppressed_planner_occurrence_ids uuid[] NOT NULL DEFAULT '{}',
    suppressed_planner_occurrence_id uuid GENERATED ALWAYS AS
        (suppressed_planner_occurrence_ids[1]) STORED,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, occurrence_evidence_id),
    FOREIGN KEY (workspace_id, occurrence_evidence_id)
        REFERENCES habit_occurrence_evidence(workspace_id, id),
    FOREIGN KEY (
        workspace_id,
        occurrence_evidence_id,
        habit_id,
        source_planner_occurrence_id
    ) REFERENCES habit_occurrence_evidence(
        workspace_id,
        id,
        habit_id,
        planner_occurrence_id
    ),
    FOREIGN KEY (workspace_id, habit_id) REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, habit_id, suppressed_planner_occurrence_id)
        REFERENCES habit_occurrence_evidence(workspace_id, habit_id, planner_occurrence_id),
    UNIQUE (workspace_id, habit_id, suppressed_planner_occurrence_id),
    CHECK (updated_at >= created_at),
    CHECK (
        (configured_policy = 'ask' AND action = 'decision_required')
        OR (configured_policy = 'ask' AND action <> 'decision_required' AND revision >= 2)
        OR (configured_policy = 'skip' AND action IN ('skip', 'cancelled'))
        OR (configured_policy = 'carry' AND action IN ('carry', 'cancelled'))
        OR (configured_policy = 'reduce_frequency'
            AND action IN ('reduction_pending', 'reduce_frequency', 'cancelled'))
    ),
    CHECK (
        (action = 'carry'
          AND cancellation_reason IS NULL AND cancelled_resume_action IS NULL
          AND NOT cancelled_explicit_selection
          AND carry_window_start = updated_at
          AND carry_window_end > carry_window_start
          AND carry_window_end <= carry_window_start + interval '366 days'
          AND cardinality(suppressed_planner_occurrence_ids) = 0)
        OR (action = 'reduce_frequency'
          AND cancellation_reason IS NULL AND cancelled_resume_action IS NULL
          AND NOT cancelled_explicit_selection
          AND carry_window_start IS NULL AND carry_window_end IS NULL
          AND cardinality(suppressed_planner_occurrence_ids) = 1
          AND array_ndims(suppressed_planner_occurrence_ids) = 1
          AND array_lower(suppressed_planner_occurrence_ids, 1) = 1
          AND array_upper(suppressed_planner_occurrence_ids, 1) = 1
          AND array_position(suppressed_planner_occurrence_ids, NULL) IS NULL
          AND suppressed_planner_occurrence_ids[1] IS NOT NULL
          AND suppressed_planner_occurrence_ids[1] <> '00000000-0000-0000-0000-000000000000'::uuid
          AND (get_byte(uuid_send(suppressed_planner_occurrence_ids[1]), 6) >> 4) = 5
          AND suppressed_planner_occurrence_ids[1] <> source_planner_occurrence_id)
        OR (action IN ('decision_required', 'reduction_pending', 'skip')
          AND cancellation_reason IS NULL AND cancelled_resume_action IS NULL
          AND NOT cancelled_explicit_selection
          AND carry_window_start IS NULL AND carry_window_end IS NULL
          AND cardinality(suppressed_planner_occurrence_ids) = 0)
        OR (action = 'cancelled'
          AND revision >= 2
          AND cancellation_reason IS NOT NULL AND cancelled_resume_action IS NOT NULL
          AND (NOT cancelled_explicit_selection
            OR (configured_policy = 'ask'
              AND cancelled_resume_action IN ('skip', 'carry', 'reduce_frequency')))
          AND carry_window_start IS NULL AND carry_window_end IS NULL
          AND cardinality(suppressed_planner_occurrence_ids) = 0)
    ),
    CHECK (
        configured_policy = 'ask'
        OR cancellation_reason IS NULL
        OR cancelled_resume_action = configured_policy
    )
);

CREATE INDEX habit_missed_resolutions_habit_idx
    ON habit_missed_resolutions (workspace_id, habit_id, updated_at, occurrence_evidence_id);

CREATE INDEX habit_missed_resolutions_pending_idx
    ON habit_missed_resolutions (workspace_id, habit_id, created_at, occurrence_evidence_id)
    WHERE action = 'reduction_pending';

CREATE INDEX habit_occurrence_evidence_reduction_target_idx
    ON habit_occurrence_evidence
        (workspace_id, habit_id, nominal_start, recurrence_ordinal,
         planner_occurrence_id, window_start);

CREATE TABLE habit_missed_resolution_versions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    occurrence_evidence_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    operation_id uuid NOT NULL,
    previous_snapshot jsonb,
    resolution_snapshot jsonb NOT NULL,
    recorded_at timestamptz NOT NULL,
    UNIQUE (workspace_id, occurrence_evidence_id, revision),
    UNIQUE (workspace_id, operation_id),
    FOREIGN KEY (workspace_id, occurrence_evidence_id)
        REFERENCES habit_missed_resolutions(workspace_id, occurrence_evidence_id),
    CHECK (previous_snapshot IS NULL OR jsonb_typeof(previous_snapshot) = 'object'),
    CHECK (jsonb_typeof(resolution_snapshot) = 'object'),
    CHECK (octet_length(COALESCE(previous_snapshot, '{}'::jsonb)::text) <= 16384),
    CHECK (octet_length(resolution_snapshot::text) <= 16384)
);

CREATE INDEX habit_missed_resolution_versions_history_idx
    ON habit_missed_resolution_versions
        (workspace_id, occurrence_evidence_id, revision DESC);

CREATE TRIGGER habit_missed_resolutions_no_delete
    BEFORE DELETE ON habit_missed_resolutions
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();

CREATE TRIGGER habit_missed_resolution_versions_immutable
    BEFORE UPDATE OR DELETE ON habit_missed_resolution_versions
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();

CREATE FUNCTION guard_habit_missed_resolution_insert() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF NEW.revision <> 1
       OR (NEW.configured_policy = 'ask' AND NEW.action <> 'decision_required') THEN
        RAISE EXCEPTION 'habit missed resolution initial projection is invalid';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER habit_missed_resolution_insert_guard
    BEFORE INSERT ON habit_missed_resolutions
    FOR EACH ROW EXECUTE FUNCTION guard_habit_missed_resolution_insert();

CREATE FUNCTION guard_habit_missed_resolution_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.occurrence_evidence_id IS DISTINCT FROM OLD.occurrence_evidence_id
       OR NEW.habit_id IS DISTINCT FROM OLD.habit_id
       OR NEW.source_planner_occurrence_id IS DISTINCT FROM OLD.source_planner_occurrence_id
       OR NEW.configured_policy IS DISTINCT FROM OLD.configured_policy
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NOT (CASE
           WHEN OLD.action = 'decision_required' THEN
               (NEW.action IN ('skip', 'carry', 'reduction_pending', 'reduce_frequency')
                   AND NOT NEW.cancelled_explicit_selection)
               OR (NEW.action = 'cancelled' AND (
                   (NEW.cancelled_resume_action = 'decision_required'
                       AND NOT NEW.cancelled_explicit_selection)
                   OR (NEW.cancelled_resume_action IN ('skip', 'carry', 'reduce_frequency')
                       AND NEW.cancelled_explicit_selection)))
           WHEN OLD.action = 'reduction_pending' THEN
               NEW.action = 'reduce_frequency'
               OR (NEW.action = 'cancelled'
                   AND NEW.cancelled_resume_action = 'reduce_frequency'
                   AND NOT NEW.cancelled_explicit_selection)
           WHEN OLD.action = 'reduce_frequency' THEN
               NEW.action = 'reduction_pending'
               OR (NEW.action = 'cancelled'
                   AND NEW.cancelled_resume_action = 'reduce_frequency'
                   AND NOT NEW.cancelled_explicit_selection)
           WHEN OLD.action = 'skip' THEN
               NEW.action = 'cancelled' AND NEW.cancelled_resume_action = 'skip'
                   AND NOT NEW.cancelled_explicit_selection
           WHEN OLD.action = 'carry' THEN
               NEW.action IN ('carry', 'decision_required')
               OR (NEW.action = 'cancelled' AND NEW.cancelled_resume_action = 'carry'
                   AND NOT NEW.cancelled_explicit_selection)
           WHEN OLD.action = 'cancelled' THEN
               NOT NEW.cancelled_explicit_selection AND (
               (OLD.cancelled_resume_action = 'decision_required' AND NEW.action = 'decision_required')
               OR (OLD.cancelled_resume_action = 'skip' AND NEW.action = 'skip')
               OR (OLD.cancelled_resume_action = 'carry' AND NEW.action = 'carry')
               OR (OLD.cancelled_resume_action = 'reduce_frequency'
                   AND NEW.action IN ('reduction_pending', 'reduce_frequency')))
           ELSE false
       END)
       OR NEW.revision <> OLD.revision + 1
       OR NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'habit missed resolution projection transition is invalid';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER habit_missed_resolution_update_guard
    BEFORE UPDATE ON habit_missed_resolutions
    FOR EACH ROW EXECUTE FUNCTION guard_habit_missed_resolution_update();

-- Resolve the currently effective reduction graph rather than treating every
-- stored reduce-frequency projection as live. Edges always point to a later
-- occurrence; alternating a forward chain prevents a suppressed source from
-- cascading its own reduction (A -> B disables B -> C, so C -> D may apply).
-- The bounded path guard fails closed if pre-migration corruption introduced
-- a cycle despite application validation.
CREATE FUNCTION habit_effective_reduction_targets(
    p_workspace_id uuid,
    p_habit_id uuid,
    p_policy_fingerprint bytea
) RETURNS TABLE (planner_occurrence_id uuid)
LANGUAGE sql STABLE AS $effective$
WITH RECURSIVE base_edges AS MATERIALIZED (
    SELECT source.planner_occurrence_id AS source_id,
           target.planner_occurrence_id AS target_id
    FROM habit_missed_resolutions resolution
    JOIN habit_occurrence_evidence source
      ON source.workspace_id = resolution.workspace_id
     AND source.id = resolution.occurrence_evidence_id
    JOIN habit_occurrence_evidence target
      ON target.workspace_id = resolution.workspace_id
     AND target.habit_id = resolution.habit_id
     AND target.planner_occurrence_id = resolution.suppressed_planner_occurrence_id
    JOIN items item
      ON item.workspace_id = source.workspace_id
     AND item.id = source.habit_id
    LEFT JOIN habit_occurrence_outcomes source_outcome
      ON source_outcome.workspace_id = source.workspace_id
     AND source_outcome.occurrence_evidence_id = source.id
    LEFT JOIN habit_occurrence_outcomes target_outcome
      ON target_outcome.workspace_id = target.workspace_id
     AND target_outcome.occurrence_evidence_id = target.id
    WHERE resolution.workspace_id = p_workspace_id
      AND resolution.habit_id = p_habit_id
      AND resolution.action = 'reduce_frequency'
      AND item.kind = 'habit'
      AND item.recurrence IS NOT NULL
      AND item.trashed_at IS NULL
      AND item.status NOT IN ('completed', 'skipped', 'cancelled', 'blocked')
      AND NOT EXISTS (
          SELECT 1 FROM item_hierarchy child_edge
          JOIN items child
            ON child.workspace_id = child_edge.workspace_id
           AND child.id = child_edge.child_item_id
          WHERE child_edge.workspace_id = item.workspace_id
            AND child_edge.parent_item_id = item.id
            AND child.trashed_at IS NULL
      )
      AND source.policy_fingerprint = p_policy_fingerprint
      AND target.policy_fingerprint = p_policy_fingerprint
      AND (source_outcome.status IS NULL
        OR source_outcome.status NOT IN ('completed', 'skipped'))
      AND (target_outcome.status IS NULL OR target_outcome.status = 'unresolved')
      AND NOT EXISTS (
          SELECT 1 FROM habit_pauses source_pause
          WHERE source_pause.workspace_id = source.workspace_id
            AND source_pause.habit_id = source.habit_id
            AND source_pause.started_at < source.window_end
            AND (source_pause.ended_at IS NULL
              OR source_pause.ended_at > source.window_start)
      )
      AND NOT EXISTS (
          SELECT 1 FROM habit_pauses target_pause
          WHERE target_pause.workspace_id = target.workspace_id
            AND target_pause.habit_id = target.habit_id
            AND target_pause.started_at < target.window_end
            AND (target_pause.ended_at IS NULL
              OR target_pause.ended_at > target.window_start)
      )
      AND NOT EXISTS (
          SELECT 1 FROM schedule_revisions current_revision
          WHERE current_revision.workspace_id = target.workspace_id
            AND current_revision.state = 'published'
            AND current_revision.horizon_start <= target.window_start
            AND current_revision.horizon_end >= target.window_end
            AND NOT EXISTS (
                SELECT 1 FROM habit_occurrence_publications publication
                WHERE publication.workspace_id = target.workspace_id
                  AND publication.schedule_revision_id = current_revision.id
                  AND publication.occurrence_evidence_id = target.id
                  AND publication.occurrence_state IN ('generated', 'skipped')
            )
      )
), walk(source_id, target_id, is_effective, path) AS (
    SELECT edge.source_id, edge.target_id, true,
           ARRAY[edge.source_id, edge.target_id]::uuid[]
    FROM base_edges edge
    WHERE NOT EXISTS (
        SELECT 1 FROM base_edges parent WHERE parent.target_id = edge.source_id
    )
    UNION ALL
    SELECT child.source_id, child.target_id, NOT walk.is_effective,
           walk.path || child.target_id
    FROM walk
    JOIN base_edges child ON child.source_id = walk.target_id
    WHERE NOT (child.target_id = ANY(walk.path))
)
SELECT walk.target_id FROM walk WHERE walk.is_effective
$effective$;

-- Reduction always addresses the exact next occurrence in the current
-- publication, and that occurrence must be generated to become a new target.
-- A target previously skipped by this same reduction remains addressable while
-- it is re-pending. Eligibility is deliberately checked only after the exact
-- occurrence is selected: a partial, completed, paused, stale, or already
-- reserved immediate target must keep the source pending instead of shifting
-- the reduction to a later occurrence.
CREATE FUNCTION habit_available_reduction_target(
    p_workspace_id uuid,
    p_habit_id uuid,
    p_source_evidence_id uuid,
    p_source_nominal_start timestamptz,
    p_source_recurrence_ordinal bigint,
    p_source_planner_occurrence_id uuid,
    p_policy_fingerprint bytea,
    p_now timestamptz
) RETURNS TABLE (planner_occurrence_id uuid)
LANGUAGE sql STABLE AS $available$
WITH exact_target AS MATERIALIZED (
    SELECT target.id,
           target.workspace_id,
           target.habit_id,
           target.planner_occurrence_id,
           target.policy_fingerprint,
           target.window_start,
           target.window_end,
           publication.occurrence_state
    FROM habit_occurrence_evidence target
    JOIN habit_occurrence_publications publication
      ON publication.workspace_id = target.workspace_id
     AND publication.occurrence_evidence_id = target.id
    JOIN schedule_revisions current_revision
      ON current_revision.workspace_id = publication.workspace_id
     AND current_revision.id = publication.schedule_revision_id
     AND current_revision.state = 'published'
     AND current_revision.horizon_start <= p_now
    WHERE target.workspace_id = p_workspace_id
      AND target.habit_id = p_habit_id
      AND (target.nominal_start, target.recurrence_ordinal, target.planner_occurrence_id)
        > (p_source_nominal_start, p_source_recurrence_ordinal,
           p_source_planner_occurrence_id)
    ORDER BY target.nominal_start, target.recurrence_ordinal,
             target.planner_occurrence_id
    LIMIT 1
)
SELECT target.planner_occurrence_id
FROM exact_target target
LEFT JOIN habit_occurrence_outcomes target_outcome
  ON target_outcome.workspace_id = target.workspace_id
 AND target_outcome.occurrence_evidence_id = target.id
WHERE target.policy_fingerprint = p_policy_fingerprint
  AND (
      target.occurrence_state = 'generated'
      OR (
          target.occurrence_state = 'skipped'
          AND (
              EXISTS (
                  SELECT 1 FROM habit_missed_resolutions current_resolution
                  WHERE current_resolution.workspace_id = p_workspace_id
                    AND current_resolution.occurrence_evidence_id = p_source_evidence_id
                    AND current_resolution.action = 'reduce_frequency'
                    AND current_resolution.suppressed_planner_occurrence_id =
                        target.planner_occurrence_id
              )
              OR target.planner_occurrence_id = (
                  SELECT (
                      version.previous_snapshot #>>
                      '{action,suppressed_planner_occurrence_ids,0}'
                  )::uuid
                  FROM habit_missed_resolution_versions version
                  WHERE version.workspace_id = p_workspace_id
                    AND version.occurrence_evidence_id = p_source_evidence_id
                    AND version.previous_snapshot #>> '{action,type}' =
                        'reduce_frequency'
                  ORDER BY version.revision DESC
                  LIMIT 1
              )
          )
      )
  )
  AND target.window_end > p_now
  AND (target_outcome.status IS NULL OR target_outcome.status = 'unresolved')
  AND NOT EXISTS (
      SELECT 1 FROM habit_pauses target_pause
      WHERE target_pause.workspace_id = target.workspace_id
        AND target_pause.habit_id = target.habit_id
        AND target_pause.started_at < target.window_end
        AND (target_pause.ended_at IS NULL
          OR target_pause.ended_at > target.window_start)
  )
  AND NOT EXISTS (
      SELECT 1 FROM habit_effective_reduction_targets(
          target.workspace_id, target.habit_id, p_policy_fingerprint
      ) effective
      WHERE effective.planner_occurrence_id = target.planner_occurrence_id
  )
  AND NOT EXISTS (
      SELECT 1 FROM habit_missed_resolutions reservation
      WHERE reservation.workspace_id = target.workspace_id
        AND reservation.action = 'reduce_frequency'
        AND reservation.suppressed_planner_occurrence_id = target.planner_occurrence_id
        AND reservation.occurrence_evidence_id <> p_source_evidence_id
  )
$available$;

-- Empty reconcile responses need exact retry semantics, but unlike mutation
-- receipts they do not represent immutable evidence. Keep them in the shared
-- expiring idempotency store and make operation IDs unique for this namespace.
CREATE UNIQUE INDEX habit_missed_reconcile_ephemeral_operation_idx
    ON idempotency_keys (workspace_id, resource_id)
    WHERE namespace = 'habits.missed.reconcile'
      AND resource_type = 'habit_missed_reconcile_receipt';
