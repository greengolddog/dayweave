-- Typed structural planning fields for projects, uncertain duration, calendar-date
-- deadlines, independent parent effort, and user-visible blocking causes.
--
-- Legacy HTTP/request shapes remain accepted and are normalized below. This
-- personal deployment migration uses stop-old/apply/start-new ordering; it does
-- not support concurrent pre-0024 writers. A pre-0024 binary is rollback-safe
-- only before any project, blocked, range, date-only, soft, or explicit
-- independent-effort write. After that boundary, restore the pre-migration
-- backup or roll forward. The mapped Google Task normalization in this
-- migration itself crosses that boundary when it creates a date-only row.
-- The checks below remain the final authority.

ALTER TABLE items
    DROP CONSTRAINT items_kind_check,
    ADD CONSTRAINT items_kind_check
        CHECK (kind IN ('task', 'event', 'habit', 'routine', 'goal', 'project', 'break')),
    DROP CONSTRAINT items_status_check,
    ADD CONSTRAINT items_status_check
        CHECK (status IN (
            'inbox', 'planned', 'scheduled', 'in_progress', 'paused',
            'completed', 'skipped', 'cancelled', 'blocked'
        )),
    ADD COLUMN duration_kind varchar(16),
    ADD COLUMN duration_min_seconds integer,
    ADD COLUMN duration_max_seconds integer,
    ADD COLUMN duration_source varchar(16),
    ADD COLUMN deadline_kind varchar(16),
    ADD COLUMN deadline_date date,
    ADD COLUMN deadline_strength varchar(16),
    ADD COLUMN deadline_soft_weight integer,
    ADD COLUMN has_own_effort boolean,
    ADD COLUMN blocked_reason_kind varchar(16),
    ADD COLUMN blocked_by_item_id uuid,
    ADD COLUMN blocked_reason varchar(1000);

UPDATE items
SET duration_kind = CASE
        WHEN duration_seconds IS NULL THEN 'unknown'
        ELSE 'exact'
    END,
    duration_min_seconds = duration_seconds,
    duration_max_seconds = duration_seconds,
    duration_source = CASE
        WHEN duration_seconds IS NULL THEN NULL
        WHEN jsonb_typeof(scheduling_constraints -> 'calendar_event') = 'object'
          OR jsonb_typeof(scheduling_constraints -> 'calendar_context') = 'object'
            THEN 'imported'
        ELSE 'user'
    END,
    deadline_kind = CASE
        WHEN kind = 'event' OR deadline_at IS NULL THEN 'none'
        ELSE 'date_time'
    END,
    deadline_strength = CASE
        WHEN kind = 'event' OR deadline_at IS NULL THEN NULL
        ELSE 'hard'
    END,
    has_own_effort = CASE
        WHEN jsonb_typeof(scheduling_constraints -> 'has_own_effort') = 'boolean'
            THEN (scheduling_constraints ->> 'has_own_effort')::boolean
        ELSE false
    END;

-- Before independent effort was explicit, a leaf Goal or Routine could be
-- started solely because it had no children. Silently inferring true here
-- would mutate canonical state without a pre-upgrade revision/delta that
-- already-synced clients can observe. Fail with an actionable repair instead:
-- while the old binary is still running, either finish the session or replace
-- the item with scheduling_constraints.has_own_effort=true (which advances its
-- ordinary item revision and delta), then retry this stop-the-world migration.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM execution_state AS state
        JOIN execution_sessions AS session
          ON session.workspace_id = state.workspace_id
         AND session.id = state.active_session_id
        JOIN items AS item
          ON item.workspace_id = session.workspace_id
         AND item.id = session.item_id
        WHERE item.kind IN ('goal', 'routine')
          AND NOT item.has_own_effort
          AND session.state IN ('active', 'paused')
    ) THEN
        RAISE EXCEPTION
            'migration 0024 found an active/paused Goal or Routine without explicit own effort; before retrying, use the pre-0024 server to finish the session or set scheduling_constraints.has_own_effort=true'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

-- A still-actionable deferred replacement is execution authority even though
-- its source session is closed. Do not change its container to non-executable
-- behind the promised replacement window. The pre-0024 repair options are to
-- consume/finish that deferred work or explicitly mark its independent effort.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM execution_defer_replacement_claims AS claim
        JOIN items AS item
          ON item.workspace_id = claim.workspace_id
         AND item.id = claim.item_id
        LEFT JOIN execution_defer_replacement_consumptions AS consumption
          ON consumption.workspace_id = claim.workspace_id
         AND consumption.source_deferred_session_id = claim.source_deferred_session_id
        WHERE item.kind IN ('goal', 'routine')
          AND NOT item.has_own_effort
          AND claim.actionable
          AND consumption.source_deferred_session_id IS NULL
          AND item.execution_epoch = claim.execution_epoch
    ) THEN
        RAISE EXCEPTION
            'migration 0024 found a live deferred Goal or Routine without explicit own effort; before retrying, use the pre-0024 server to consume or finish the deferred work or set scheduling_constraints.has_own_effort=true'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

-- Pre-0024 API projections described every non-deleted leaf Goal/Routine as
-- executable even though the planner already treated a container without
-- independent effort as non-demand. The new canonical projection resolves
-- that disagreement in favor of HIE-002/HIE-003. Advance every affected leaf
-- so clients converge through an ordinary complete delta rather than seeing a
-- same-revision replacement of derived executability.
CREATE TEMPORARY TABLE dayweave_container_executability_upgrades
ON COMMIT DROP
AS
SELECT item.workspace_id,
       item.id AS item_id,
       item.created_by_user_id AS actor_user_id,
       item.revision AS old_revision,
       item.revision + 1 AS new_revision,
       GREATEST(
           statement_timestamp(),
           item.updated_at + interval '1 microsecond'
       ) AS upgraded_at
FROM items AS item
WHERE item.kind IN ('goal', 'routine')
  AND NOT item.has_own_effort
  AND item.trashed_at IS NULL
  AND NOT EXISTS (
      SELECT 1
      FROM item_hierarchy AS edge
      JOIN items AS child
        ON child.workspace_id = edge.workspace_id
       AND child.id = edge.child_item_id
      WHERE edge.workspace_id = item.workspace_id
        AND edge.parent_item_id = item.id
        AND child.trashed_at IS NULL
  );

CREATE UNIQUE INDEX dayweave_container_executability_upgrades_item_uq
    ON dayweave_container_executability_upgrades (workspace_id, item_id);

UPDATE items AS item
SET revision = upgrade.new_revision,
    updated_at = upgrade.upgraded_at
FROM dayweave_container_executability_upgrades AS upgrade
WHERE item.workspace_id = upgrade.workspace_id
  AND item.id = upgrade.item_id
  AND item.revision = upgrade.old_revision;

INSERT INTO item_changes (
    workspace_id,
    item_id,
    item_revision,
    change_kind,
    payload,
    changed_at
)
SELECT item.workspace_id,
       item.id,
       item.revision,
       'upsert',
       jsonb_build_object(
           'id', item.id,
           'is_sensitive', item.is_sensitive,
           'kind', item.kind,
           'status', item.status,
           'title', item.title,
           'notes', item.notes,
           'timezone_name', item.timezone_name,
           'duration_kind', item.duration_kind,
           'duration_seconds', item.duration_seconds,
           'duration_min_seconds', item.duration_min_seconds,
           'duration_max_seconds', item.duration_max_seconds,
           'duration_source', item.duration_source,
           'deadline_kind', item.deadline_kind,
           'deadline_date', item.deadline_date,
           'deadline_at', item.deadline_at,
           'deadline_strength', item.deadline_strength,
           'deadline_soft_weight', item.deadline_soft_weight,
           'earliest_start_at', item.earliest_start_at,
           'recurrence', item.recurrence,
           'flexible_constraints', item.scheduling_constraints,
           'has_own_effort', item.has_own_effort,
           'split_policy', CASE
               WHEN item.split_allowed THEN jsonb_build_object(
                   'type', 'splittable',
                   'minimum_chunk_seconds', item.minimum_chunk_seconds,
                   'maximum_chunk_seconds', item.maximum_chunk_seconds
               )
               ELSE jsonb_build_object('type', 'indivisible')
           END,
           'importance', item.importance,
           'urgency', item.urgency,
           'parent_id', hierarchy.parent_item_id,
           'sibling_order', COALESCE(hierarchy.position, item.sibling_order),
           'is_executable', false,
           'revision', item.revision,
           'created_at', item.created_at,
           'updated_at', item.updated_at,
           'completed_at', item.completed_at,
           'deleted_at', item.trashed_at,
           'blocked_reason_kind', item.blocked_reason_kind,
           'blocked_by_item_id', item.blocked_by_item_id,
           'blocked_reason', item.blocked_reason
       ),
       item.updated_at
FROM dayweave_container_executability_upgrades AS upgrade
JOIN items AS item
  ON item.workspace_id = upgrade.workspace_id
 AND item.id = upgrade.item_id
LEFT JOIN item_hierarchy AS hierarchy
  ON hierarchy.workspace_id = item.workspace_id
 AND hierarchy.child_item_id = item.id;

INSERT INTO outbox_messages (
    id,
    workspace_id,
    aggregate_type,
    aggregate_id,
    aggregate_revision,
    event_type,
    deduplication_key,
    payload,
    available_at,
    created_at,
    updated_at
)
SELECT md5(
           'dayweave:0024:container-executability-outbox:'
           || upgrade.workspace_id::text || ':' || upgrade.item_id::text
       )::uuid,
       upgrade.workspace_id,
       'item',
       upgrade.item_id,
       upgrade.new_revision,
       'item.container_executability_upgraded',
       'item.container_executability_upgraded:'
           || upgrade.item_id::text || ':' || upgrade.new_revision::text,
       jsonb_build_object(
           'item_id', upgrade.item_id,
           'revision', upgrade.new_revision,
           'change', 'upsert'
       ),
       upgrade.upgraded_at,
       upgrade.upgraded_at,
       upgrade.upgraded_at
FROM dayweave_container_executability_upgrades AS upgrade;

INSERT INTO audit_operations (
    id,
    workspace_id,
    actor_user_id,
    operation_type,
    entity_type,
    entity_id,
    base_revision,
    result_revision,
    outcome,
    metadata,
    occurred_at
)
SELECT md5(
           'dayweave:0024:container-executability-audit:'
           || upgrade.workspace_id::text || ':' || upgrade.item_id::text
       )::uuid,
       upgrade.workspace_id,
       upgrade.actor_user_id,
       'item.container_executability_upgraded',
       'item',
       upgrade.item_id,
       upgrade.old_revision,
       upgrade.new_revision,
       'succeeded',
       jsonb_build_object(
           'source', 'migration',
           'reason', 'semantic_container_requires_own_effort'
       ),
       upgrade.upgraded_at
FROM dayweave_container_executability_upgrades AS upgrade;

-- Completed idempotency responses are immutable historical receipts, but the
-- same pre-upgrade request may replay after deployment. Add the explicit
-- structural fields and corrected derived projection without changing its
-- historical item revision or request fingerprint.
UPDATE idempotency_keys AS replay
SET response_json = replay.response_json || jsonb_build_object(
        'duration_kind', CASE
            WHEN replay.response_json ->> 'duration_seconds' IS NULL THEN 'unknown'
            ELSE 'exact'
        END,
        'duration_min_seconds', replay.response_json -> 'duration_seconds',
        'duration_max_seconds', replay.response_json -> 'duration_seconds',
        'duration_source', CASE
            WHEN replay.response_json ->> 'duration_seconds' IS NULL THEN NULL
            ELSE 'user'
        END,
        'deadline_kind', CASE
            WHEN replay.response_json ->> 'deadline_at' IS NULL THEN 'none'
            ELSE 'date_time'
        END,
        'deadline_date', NULL,
        'deadline_strength', CASE
            WHEN replay.response_json ->> 'deadline_at' IS NULL THEN NULL
            ELSE 'hard'
        END,
        'deadline_soft_weight', NULL,
        'has_own_effort', false,
        'is_executable', false,
        'blocked_reason_kind', NULL,
        'blocked_by_item_id', NULL,
        'blocked_reason', NULL
    ),
    updated_at = upgrade.upgraded_at
FROM dayweave_container_executability_upgrades AS upgrade
WHERE replay.workspace_id = upgrade.workspace_id
  AND replay.resource_type = 'item'
  AND replay.resource_id = upgrade.item_id
  AND replay.state = 'completed'
  AND replay.response_json IS NOT NULL
  AND replay.response_json ->> 'kind' IN ('goal', 'routine')
  AND COALESCE(
      (replay.response_json -> 'flexible_constraints' ->> 'has_own_effort')::boolean,
      false
  ) = false;

-- Google Tasks exposes a due calendar date in an RFC 3339 midnight wrapper.
-- Convert both provider-owned and DayWeave-owned active task-list mappings
-- when the legacy value has Google's canonical midnight shape, so upgraded
-- rows can continue to round-trip through the lossless Date policy. An owned
-- non-midnight deadline could carry local exact-time intent and is deliberately
-- preserved for explicit review rather than silently weakened to a date.
-- Recoverably trashed mapped items are converted too and receive a tombstone
-- delta, so a later local/provider restore cannot revive the old DateTime
-- meaning while offline caches continue to treat the item as deleted.
CREATE TEMPORARY TABLE dayweave_google_task_deadline_upgrades
ON COMMIT DROP
AS
SELECT item.workspace_id,
       item.id AS item_id,
       item.created_by_user_id AS actor_user_id,
       item.revision AS old_revision,
       item.revision + 1 AS new_revision,
       item.deadline_at AS old_deadline_at,
       (item.deadline_at AT TIME ZONE 'UTC')::date AS deadline_date,
       item.trashed_at,
       GREATEST(
           statement_timestamp(),
           item.updated_at + interval '1 microsecond'
       ) AS upgraded_at
  FROM items AS item
 WHERE item.kind = 'task'
   AND item.deadline_at IS NOT NULL
   AND item.deadline_at AT TIME ZONE 'UTC'
       = date_trunc('day', item.deadline_at AT TIME ZONE 'UTC')
   AND EXISTS (
       SELECT 1
       FROM provider_sync_mappings AS mapping
       JOIN google_sync_collections AS collection
         ON collection.workspace_id = mapping.workspace_id
        AND collection.id = mapping.collection_id
       WHERE mapping.workspace_id = item.workspace_id
         AND mapping.local_entity_id = item.id
         AND mapping.entity_kind = 'item'
         AND mapping.tombstoned_at IS NULL
         AND collection.collection_kind = 'task_list'
   );

CREATE UNIQUE INDEX dayweave_google_task_deadline_upgrades_item_uq
    ON dayweave_google_task_deadline_upgrades (workspace_id, item_id);

-- Date-only deadlines resolve to the next local midnight. The portable
-- canonical range therefore stops one day before PostgreSQL/Chrono's maximum
-- date. Preserve the legacy DateTime row until the user/provider can repair an
-- out-of-range due value rather than failing later with an opaque CHECK error.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dayweave_google_task_deadline_upgrades
        WHERE deadline_date < DATE '0001-01-01'
           OR deadline_date > DATE '9999-12-30'
    ) THEN
        RAISE EXCEPTION
            'migration 0024 found a mapped Google Task midnight deadline outside the supported date-only range 0001-01-01 through 9999-12-30; before retrying, use the pre-0024 server or provider to move that due date into the supported range'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

-- An active/paused session binds the item revision it started against. A
-- deadline-only revision advance remains safe for pause/resume/complete, but it
-- would invalidate the immutable revision attestation needed by Defer. Let the
-- pre-0024 server finish or defer first rather than stranding that authority.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dayweave_google_task_deadline_upgrades AS upgrade
        JOIN execution_state AS state
          ON state.workspace_id = upgrade.workspace_id
        JOIN execution_sessions AS session
          ON session.workspace_id = state.workspace_id
         AND session.id = state.active_session_id
         AND session.item_id = upgrade.item_id
        WHERE session.state IN ('active', 'paused')
    ) THEN
        RAISE EXCEPTION
            'migration 0024 found an active/paused mapped Google Task whose deadline requires date-only normalization; before retrying, use the pre-0024 server to finish or defer that session'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

UPDATE items AS item
SET deadline_kind = 'date',
    deadline_date = upgrade.deadline_date,
    deadline_at = NULL,
    deadline_strength = 'hard',
    deadline_soft_weight = NULL,
    revision = upgrade.new_revision,
    updated_at = upgrade.upgraded_at
FROM dayweave_google_task_deadline_upgrades AS upgrade
WHERE item.workspace_id = upgrade.workspace_id
  AND item.id = upgrade.item_id
  AND item.revision = upgrade.old_revision;

-- A mapping advances only when it described the exact pre-upgrade item
-- revision. NULL/older mappings remain visibly stale and must reconcile.
UPDATE provider_sync_mappings AS mapping
SET local_revision = upgrade.new_revision,
    updated_at = upgrade.upgraded_at
FROM dayweave_google_task_deadline_upgrades AS upgrade,
     google_sync_collections AS collection
WHERE mapping.workspace_id = upgrade.workspace_id
  AND mapping.local_entity_id = upgrade.item_id
  AND mapping.entity_kind = 'item'
  AND mapping.tombstoned_at IS NULL
  AND mapping.local_revision = upgrade.old_revision
  AND collection.workspace_id = mapping.workspace_id
  AND collection.id = mapping.collection_id
  AND collection.collection_kind = 'task_list';

-- The provider wire value is unchanged, but outbound approval hashes bind the
-- old canonical revision. Do not forge a replacement user authorization.
-- Stop-old/apply/start-new ordering prevents a currently running old worker,
-- but durable send-start evidence can survive a crash or un-drained shutdown.
-- Retire old-revision delivery authority and require fresh approval for later
-- publication. A prior markerless Task POST that may already have started
-- remains an explicit identity-unresolved conflict: superseding it would bury
-- evidence needed to prevent a duplicate create after restart.
UPDATE google_sync_outbox AS outbound
SET state = CASE
        WHEN outbound.operation = 'upsert'
         AND outbound.remote_resource_id IS NULL
         AND outbound.provider_post_may_have_started
            THEN 'conflict'
        ELSE 'superseded'
    END,
    claim_id = NULL,
    claimed_at = NULL,
    run_claim_id = NULL,
    run_claim_generation = NULL,
    dispatch_nonce = NULL,
    dispatch_authorized_at = NULL,
    dispatch_expires_at = NULL,
    last_error_code = CASE
        WHEN outbound.operation = 'upsert'
         AND outbound.remote_resource_id IS NULL
         AND outbound.provider_post_may_have_started
            THEN 'provider_identity_unresolved'
        ELSE 'canonical_deadline_semantics_upgraded'
    END,
    updated_at = upgrade.upgraded_at
FROM dayweave_google_task_deadline_upgrades AS upgrade
WHERE outbound.workspace_id = upgrade.workspace_id
  AND outbound.item_id = upgrade.item_id
  AND outbound.item_revision = upgrade.old_revision
  AND outbound.entity_kind = 'task'
  AND outbound.state IN ('pending', 'delivering', 'backoff');

-- Append one complete canonical upsert. Its new item revision makes every
-- pre-upgrade compose snapshot stale, while the new delta head lets native
-- clients observe the semantic conversion without a full reconciliation.
INSERT INTO item_changes (
    workspace_id,
    item_id,
    item_revision,
    change_kind,
    payload,
    changed_at
)
SELECT item.workspace_id,
       item.id,
       item.revision,
       CASE WHEN item.trashed_at IS NULL THEN 'upsert' ELSE 'tombstone' END,
       CASE WHEN item.trashed_at IS NOT NULL THEN jsonb_build_object(
           'id', item.id,
           'revision', item.revision,
           'deleted_at', item.trashed_at,
           'parent_id', hierarchy.parent_item_id
       ) ELSE jsonb_build_object(
           'id', item.id,
           'is_sensitive', item.is_sensitive,
           'kind', item.kind,
           'status', item.status,
           'title', item.title,
           'notes', item.notes,
           'timezone_name', item.timezone_name,
           'duration_kind', item.duration_kind,
           'duration_seconds', item.duration_seconds,
           'duration_min_seconds', item.duration_min_seconds,
           'duration_max_seconds', item.duration_max_seconds,
           'duration_source', item.duration_source,
           'deadline_kind', item.deadline_kind,
           'deadline_date', item.deadline_date,
           'deadline_at', item.deadline_at,
           'deadline_strength', item.deadline_strength,
           'deadline_soft_weight', item.deadline_soft_weight,
           'earliest_start_at', item.earliest_start_at,
           'recurrence', item.recurrence,
           'flexible_constraints', item.scheduling_constraints,
           'has_own_effort', item.has_own_effort,
           'split_policy', CASE
               WHEN item.split_allowed THEN jsonb_build_object(
                   'type', 'splittable',
                   'minimum_chunk_seconds', item.minimum_chunk_seconds,
                   'maximum_chunk_seconds', item.maximum_chunk_seconds
               )
               ELSE jsonb_build_object('type', 'indivisible')
           END,
           'importance', item.importance,
           'urgency', item.urgency,
           'parent_id', hierarchy.parent_item_id,
           'sibling_order', COALESCE(hierarchy.position, item.sibling_order),
           'is_executable',
               item.trashed_at IS NULL
               AND (item.kind NOT IN ('project', 'goal', 'routine') OR item.has_own_effort)
               AND NOT EXISTS (
                   SELECT 1
                   FROM item_hierarchy AS children
                   JOIN items AS child
                     ON child.workspace_id = children.workspace_id
                    AND child.id = children.child_item_id
                   WHERE children.workspace_id = item.workspace_id
                     AND children.parent_item_id = item.id
                     AND child.trashed_at IS NULL
               ),
           'revision', item.revision,
           'created_at', item.created_at,
           'updated_at', item.updated_at,
           'completed_at', item.completed_at,
           'deleted_at', item.trashed_at,
           'blocked_reason_kind', item.blocked_reason_kind,
           'blocked_by_item_id', item.blocked_by_item_id,
           'blocked_reason', item.blocked_reason
       ) END,
       item.updated_at
  FROM dayweave_google_task_deadline_upgrades AS upgrade
  JOIN items AS item
    ON item.workspace_id = upgrade.workspace_id
   AND item.id = upgrade.item_id
  LEFT JOIN item_hierarchy AS hierarchy
    ON hierarchy.workspace_id = item.workspace_id
   AND hierarchy.child_item_id = item.id;

-- A semantic reinterpretation is still an item mutation: mirror the ordinary
-- repository's atomic delta/outbox/audit envelope. These records do not ask
-- Google to mutate (the provider-facing midnight value is unchanged); they
-- notify internal consumers that the canonical deadline meaning advanced.
INSERT INTO outbox_messages (
    id,
    workspace_id,
    aggregate_type,
    aggregate_id,
    aggregate_revision,
    event_type,
    deduplication_key,
    payload,
    available_at,
    created_at,
    updated_at
)
SELECT md5(
           'dayweave:0024:google-task-deadline-outbox:'
           || upgrade.workspace_id::text || ':' || upgrade.item_id::text
       )::uuid,
       upgrade.workspace_id,
       'item',
       upgrade.item_id,
       upgrade.new_revision,
       'item.google_task_deadline_semantics_upgraded',
       'item.google_task_deadline_semantics_upgraded:'
           || upgrade.item_id::text || ':' || upgrade.new_revision::text,
       jsonb_build_object(
           'item_id', upgrade.item_id,
           'revision', upgrade.new_revision,
           'change', CASE WHEN upgrade.trashed_at IS NULL THEN 'upsert' ELSE 'tombstone' END
       ),
       upgrade.upgraded_at,
       upgrade.upgraded_at,
       upgrade.upgraded_at
  FROM dayweave_google_task_deadline_upgrades AS upgrade;

INSERT INTO audit_operations (
    id,
    workspace_id,
    actor_user_id,
    operation_type,
    entity_type,
    entity_id,
    base_revision,
    result_revision,
    outcome,
    metadata,
    occurred_at
)
SELECT md5(
           'dayweave:0024:google-task-deadline-audit:'
           || upgrade.workspace_id::text || ':' || upgrade.item_id::text
       )::uuid,
       upgrade.workspace_id,
       upgrade.actor_user_id,
       'item.google_task_deadline_semantics_upgraded',
       'item',
       upgrade.item_id,
       upgrade.old_revision,
       upgrade.new_revision,
       'succeeded',
       jsonb_build_object(
           'source', 'migration',
           'reason', 'google_task_date_semantics'
       ),
       upgrade.upgraded_at
  FROM dayweave_google_task_deadline_upgrades AS upgrade;

-- Exact legacy idempotency replays retain their historical revision but must
-- expose the same date semantics as the operation originally represented.
UPDATE idempotency_keys AS replay
SET response_json = replay.response_json || jsonb_build_object(
        'deadline_kind', 'date',
        'deadline_date',
            ((replay.response_json ->> 'deadline_at')::timestamptz
                AT TIME ZONE 'UTC')::date,
        'deadline_at', NULL,
        'deadline_strength', 'hard',
        'deadline_soft_weight', NULL
    ),
    updated_at = upgrade.upgraded_at
FROM dayweave_google_task_deadline_upgrades AS upgrade
WHERE replay.workspace_id = upgrade.workspace_id
  AND replay.resource_type = 'item'
  AND replay.resource_id = upgrade.item_id
  AND replay.state = 'completed'
  AND replay.response_json IS NOT NULL
  AND replay.response_json ->> 'kind' = 'task'
  AND replay.response_json ->> 'deadline_at' IS NOT NULL
  AND (replay.response_json ->> 'deadline_at')::timestamptz AT TIME ZONE 'UTC'
      = date_trunc(
          'day',
          (replay.response_json ->> 'deadline_at')::timestamptz AT TIME ZONE 'UTC'
      );

-- Preserve the legacy projection whenever independent effort is true. An
-- absent member continues to mean false to old clients.
UPDATE items
SET scheduling_constraints = jsonb_set(
        scheduling_constraints,
        '{has_own_effort}',
        'true'::jsonb,
        true
    )
WHERE has_own_effort
  AND NOT (scheduling_constraints ? 'has_own_effort');

CREATE FUNCTION dayweave_normalize_item_structural_fields()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    legacy_own_effort boolean;
    legacy_duration_write boolean;
    legacy_deadline_write boolean;
    legacy_own_effort_write boolean;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        legacy_duration_write := NEW.duration_seconds IS DISTINCT FROM OLD.duration_seconds
            AND NEW.duration_kind IS NOT DISTINCT FROM OLD.duration_kind
            AND NEW.duration_min_seconds IS NOT DISTINCT FROM OLD.duration_min_seconds
            AND NEW.duration_max_seconds IS NOT DISTINCT FROM OLD.duration_max_seconds
            AND NEW.duration_source IS NOT DISTINCT FROM OLD.duration_source
            AND (
                (
                    OLD.duration_kind = 'unknown'
                    AND OLD.duration_seconds IS NULL
                    AND OLD.duration_min_seconds IS NULL
                    AND OLD.duration_max_seconds IS NULL
                    AND OLD.duration_source IS NULL
                )
                OR (
                    OLD.duration_kind = 'exact'
                    AND OLD.duration_min_seconds = OLD.duration_seconds
                    AND OLD.duration_max_seconds = OLD.duration_seconds
                    AND (
                        (OLD.duration_source = 'imported'
                            AND (
                                jsonb_typeof(OLD.scheduling_constraints -> 'calendar_event') = 'object'
                                OR jsonb_typeof(OLD.scheduling_constraints -> 'calendar_context') = 'object'
                            ))
                        OR (OLD.duration_source = 'user'
                            AND NOT COALESCE((
                                jsonb_typeof(OLD.scheduling_constraints -> 'calendar_event') = 'object'
                                OR jsonb_typeof(OLD.scheduling_constraints -> 'calendar_context') = 'object'
                            ), false))
                    )
                )
            );
    ELSE
        legacy_duration_write := false;
    END IF;

    IF NEW.duration_kind IS NULL OR legacy_duration_write THEN
        IF NEW.duration_seconds IS NULL THEN
            NEW.duration_kind := 'unknown';
            NEW.duration_min_seconds := NULL;
            NEW.duration_max_seconds := NULL;
            NEW.duration_source := NULL;
        ELSE
            NEW.duration_kind := 'exact';
            NEW.duration_min_seconds := NEW.duration_seconds;
            NEW.duration_max_seconds := NEW.duration_seconds;
            NEW.duration_source := CASE
                WHEN jsonb_typeof(NEW.scheduling_constraints -> 'calendar_event') = 'object'
                  OR jsonb_typeof(NEW.scheduling_constraints -> 'calendar_context') = 'object'
                    THEN 'imported'
                ELSE 'user'
            END;
        END IF;
    END IF;

    IF TG_OP = 'UPDATE' THEN
        legacy_deadline_write := (
                NEW.deadline_at IS DISTINCT FROM OLD.deadline_at
                OR NEW.kind IS DISTINCT FROM OLD.kind
            )
            AND NEW.deadline_kind IS NOT DISTINCT FROM OLD.deadline_kind
            AND NEW.deadline_date IS NOT DISTINCT FROM OLD.deadline_date
            AND NEW.deadline_strength IS NOT DISTINCT FROM OLD.deadline_strength
            AND NEW.deadline_soft_weight IS NOT DISTINCT FROM OLD.deadline_soft_weight
            AND (
                (
                    OLD.kind = 'event'
                    AND OLD.deadline_kind = 'none'
                    AND OLD.deadline_date IS NULL
                    AND OLD.deadline_strength IS NULL
                    AND OLD.deadline_soft_weight IS NULL
                )
                OR (
                    OLD.kind <> 'event'
                    AND OLD.deadline_kind = 'none'
                    AND OLD.deadline_at IS NULL
                    AND OLD.deadline_date IS NULL
                    AND OLD.deadline_strength IS NULL
                    AND OLD.deadline_soft_weight IS NULL
                )
                OR (
                    OLD.kind <> 'event'
                    AND OLD.deadline_kind = 'date_time'
                    AND OLD.deadline_at IS NOT NULL
                    AND OLD.deadline_date IS NULL
                    AND OLD.deadline_strength = 'hard'
                    AND OLD.deadline_soft_weight IS NULL
                )
            );
    ELSE
        legacy_deadline_write := false;
    END IF;

    IF NEW.deadline_kind IS NULL OR legacy_deadline_write THEN
        IF NEW.kind = 'event' OR NEW.deadline_at IS NULL THEN
            NEW.deadline_kind := 'none';
            NEW.deadline_date := NULL;
            NEW.deadline_strength := NULL;
            NEW.deadline_soft_weight := NULL;
        ELSE
            NEW.deadline_kind := 'date_time';
            NEW.deadline_date := NULL;
            NEW.deadline_strength := 'hard';
            NEW.deadline_soft_weight := NULL;
        END IF;
    END IF;

    IF NEW.scheduling_constraints ? 'has_own_effort' THEN
        IF jsonb_typeof(NEW.scheduling_constraints -> 'has_own_effort') <> 'boolean' THEN
            RAISE EXCEPTION 'scheduling_constraints.has_own_effort must be boolean'
                USING ERRCODE = '23514';
        END IF;
        legacy_own_effort := (NEW.scheduling_constraints ->> 'has_own_effort')::boolean;
    ELSE
        legacy_own_effort := false;
    END IF;

    IF TG_OP = 'UPDATE' THEN
        legacy_own_effort_write :=
            NEW.scheduling_constraints IS DISTINCT FROM OLD.scheduling_constraints
            AND NEW.has_own_effort IS NOT DISTINCT FROM OLD.has_own_effort;
    ELSE
        legacy_own_effort_write := false;
    END IF;

    IF NEW.has_own_effort IS NULL OR legacy_own_effort_write THEN
        NEW.has_own_effort := legacy_own_effort;
    ELSIF NEW.scheduling_constraints ? 'has_own_effort'
          AND NEW.has_own_effort <> legacy_own_effort THEN
        RAISE EXCEPTION 'typed and legacy has_own_effort values disagree'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.has_own_effort
       AND NOT (NEW.scheduling_constraints ? 'has_own_effort') THEN
        NEW.scheduling_constraints := jsonb_set(
            NEW.scheduling_constraints,
            '{has_own_effort}',
            'true'::jsonb,
            true
        );
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER items_normalize_structural_fields
BEFORE INSERT OR UPDATE OF
    kind,
    duration_seconds,
    duration_kind,
    duration_min_seconds,
    duration_max_seconds,
    duration_source,
    deadline_at,
    deadline_kind,
    deadline_date,
    deadline_strength,
    deadline_soft_weight,
    scheduling_constraints,
    has_own_effort
ON items
FOR EACH ROW
EXECUTE FUNCTION dayweave_normalize_item_structural_fields();

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM items
        WHERE duration_seconds > 31622400
    ) THEN
        RAISE EXCEPTION
            'items contains duration_seconds above the supported 31622400-second maximum; repair the rows before migration 0024'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

ALTER TABLE items
    ALTER COLUMN duration_kind SET NOT NULL,
    ALTER COLUMN deadline_kind SET NOT NULL,
    ALTER COLUMN has_own_effort SET NOT NULL,
    ADD CONSTRAINT items_duration_shape_check CHECK (
        (
            duration_kind = 'unknown'
            AND duration_seconds IS NULL
            AND duration_min_seconds IS NULL
            AND duration_max_seconds IS NULL
            AND duration_source IS NULL
        )
        OR (
            duration_kind = 'exact'
            AND duration_seconds IS NOT NULL
            AND duration_min_seconds IS NOT NULL
            AND duration_max_seconds IS NOT NULL
            AND duration_source IS NOT NULL
            AND duration_seconds > 0
            AND duration_seconds <= 31622400
            AND duration_min_seconds = duration_seconds
            AND duration_max_seconds = duration_seconds
            AND duration_source IN ('user', 'assistant', 'learned', 'imported')
        )
        OR (
            duration_kind = 'range'
            AND duration_seconds IS NOT NULL
            AND duration_min_seconds IS NOT NULL
            AND duration_max_seconds IS NOT NULL
            AND duration_source IS NOT NULL
            AND duration_min_seconds > 0
            AND duration_min_seconds <= 31622400
            AND duration_seconds <= 31622400
            AND duration_max_seconds <= 31622400
            AND duration_min_seconds <= duration_seconds
            AND duration_seconds <= duration_max_seconds
            AND duration_min_seconds < duration_max_seconds
            AND duration_source IN ('user', 'assistant', 'learned', 'imported')
        )
    ),
    ADD CONSTRAINT items_deadline_shape_check CHECK (
        (
            deadline_kind = 'none'
            AND deadline_date IS NULL
            AND deadline_strength IS NULL
            AND deadline_soft_weight IS NULL
            AND (deadline_at IS NULL OR kind = 'event')
        )
        OR (
            deadline_kind = 'date'
            AND kind <> 'event'
            AND deadline_date IS NOT NULL
            AND deadline_date BETWEEN DATE '0001-01-01' AND DATE '9999-12-30'
            AND deadline_at IS NULL
            AND deadline_strength IS NOT NULL
            AND (
                (deadline_strength = 'hard' AND deadline_soft_weight IS NULL)
                OR (
                    deadline_strength = 'soft'
                    AND deadline_soft_weight IS NOT NULL
                    AND deadline_soft_weight BETWEEN 0 AND 1000000
                )
            )
        )
        OR (
            deadline_kind = 'date_time'
            AND kind <> 'event'
            AND deadline_date IS NULL
            AND deadline_at IS NOT NULL
            AND deadline_strength IS NOT NULL
            AND (
                (deadline_strength = 'hard' AND deadline_soft_weight IS NULL)
                OR (
                    deadline_strength = 'soft'
                    AND deadline_soft_weight IS NOT NULL
                    AND deadline_soft_weight BETWEEN 0 AND 1000000
                )
            )
        )
    ),
    ADD CONSTRAINT items_own_effort_projection_check CHECK (
        NOT (scheduling_constraints ? 'has_own_effort')
        OR scheduling_constraints -> 'has_own_effort' = to_jsonb(has_own_effort)
    ),
    ADD CONSTRAINT items_blocked_reason_shape_check CHECK (
        (
            status <> 'blocked'
            AND blocked_reason_kind IS NULL
            AND blocked_by_item_id IS NULL
            AND blocked_reason IS NULL
        )
        OR (
            status = 'blocked'
            AND blocked_reason_kind IS NOT NULL
            AND blocked_reason_kind = 'dependency'
            AND blocked_by_item_id IS NOT NULL
            AND blocked_by_item_id <> id
            AND (
                blocked_reason IS NULL
                OR (
                    blocked_reason = btrim(blocked_reason)
                    AND blocked_reason <> ''
                    AND blocked_reason !~ '[[:cntrl:]]'
                )
            )
        )
        OR (
            status = 'blocked'
            AND blocked_reason_kind IS NOT NULL
            AND blocked_reason_kind IN ('manual', 'external')
            AND blocked_by_item_id IS NULL
            AND blocked_reason IS NOT NULL
            AND blocked_reason = btrim(blocked_reason)
            AND blocked_reason <> ''
            AND blocked_reason !~ '[[:cntrl:]]'
        )
    ),
    ADD CONSTRAINT items_blocked_by_item_fk
        FOREIGN KEY (workspace_id, blocked_by_item_id)
        REFERENCES items(workspace_id, id);

CREATE INDEX items_workspace_blocked_idx
    ON items (workspace_id, blocked_reason_kind, updated_at DESC)
    WHERE status = 'blocked' AND trashed_at IS NULL;

CREATE INDEX items_workspace_deadline_date_idx
    ON items (workspace_id, deadline_date, updated_at DESC)
    WHERE deadline_kind = 'date' AND trashed_at IS NULL;

-- Blocked work is dormant across every database-authoritative execution path,
-- not only in the Rust planner. These trigger functions originated in 0020
-- and 0021; rewrite their exact terminal-status predicates in place so an
-- upgraded installation retains all of the existing evidence checks while
-- adding the new non-startable state. Exact occurrence counts make this fail
-- closed if an earlier authority definition ever diverges.
DO $rewrite$
DECLARE
    authority_function_name text;
    expected_rewrites integer;
    actual_rewrites integer;
    definition text;
    rewritten text;
    negative_predicate constant text :=
        'item.status NOT IN (''completed'', ''skipped'', ''cancelled'')';
    blocked_negative_predicate constant text :=
        'item.status NOT IN (''completed'', ''skipped'', ''cancelled'', ''blocked'')';
    positive_predicate constant text :=
        'item.status IN (''completed'', ''skipped'', ''cancelled'')';
    blocked_positive_predicate constant text :=
        'item.status IN (''completed'', ''skipped'', ''cancelled'', ''blocked'')';
    assessment_predicate constant text :=
        'item_row.status IN (''completed'', ''skipped'', ''cancelled'')';
    blocked_assessment_predicate constant text :=
        'item_row.status IN (''completed'', ''skipped'', ''cancelled'', ''blocked'')'
        || E'\n       OR EXISTS (SELECT 1 FROM items AS component_item'
        || E'\n                    WHERE component_item.workspace_id = NEW.workspace_id'
        || E'\n                      AND component_item.id = NEW.item_id'
        || E'\n                      AND component_item.kind IN (''project'', ''goal'', ''routine'')'
        || E'\n                      AND NOT component_item.has_own_effort)';
    executable_component_predicate constant text :=
        '(item.kind NOT IN (''project'', ''goal'', ''routine'') OR item.has_own_effort)';
    missing_executable_component_predicate constant text :=
        '(item.kind IN (''project'', ''goal'', ''routine'') AND NOT item.has_own_effort)';
BEGIN
    FOR authority_function_name, expected_rewrites IN
        SELECT * FROM (VALUES
            ('guard_execution_defer_assessment', 1),
            ('guard_execution_defer_replacement_claim', 1),
            ('guard_schedule_defer_replacement_placement', 1),
            ('guard_execution_schedule_block_index', 2),
            ('guard_execution_session_semantic_start', 1),
            ('guard_schedule_revision_update', 3)
        ) AS authority_functions(name, rewrite_count)
    LOOP
        SELECT pg_get_functiondef(procedure.oid)
          INTO definition
          FROM pg_proc AS procedure
          JOIN pg_namespace AS namespace
            ON namespace.oid = procedure.pronamespace
         WHERE namespace.nspname = current_schema()
           AND procedure.proname = authority_function_name
           AND procedure.pronargs = 0;
        IF definition IS NULL THEN
            RAISE EXCEPTION
                'migration 0024 cannot find execution authority function %',
                authority_function_name
                USING ERRCODE = '23514';
        END IF;

        actual_rewrites :=
            (length(definition) - length(replace(definition, negative_predicate, '')))
                / length(negative_predicate)
            + (length(definition) - length(replace(definition, positive_predicate, '')))
                / length(positive_predicate)
            + (length(definition) - length(replace(definition, assessment_predicate, '')))
                / length(assessment_predicate);
        IF actual_rewrites <> expected_rewrites THEN
            RAISE EXCEPTION
                'migration 0024 expected % blocked-state rewrites in %, found %',
                expected_rewrites,
                authority_function_name,
                actual_rewrites
                USING ERRCODE = '23514';
        END IF;

        rewritten := replace(definition, negative_predicate, blocked_negative_predicate);
        rewritten := replace(rewritten, positive_predicate, blocked_positive_predicate);
        rewritten := replace(rewritten, assessment_predicate, blocked_assessment_predicate);
        IF authority_function_name IN (
            'guard_execution_defer_replacement_claim',
            'guard_schedule_defer_replacement_placement',
            'guard_execution_schedule_block_index',
            'guard_execution_session_semantic_start',
            'guard_schedule_revision_update'
        ) THEN
            -- Every execution/defer seal must agree with the canonical
            -- executable-component projection, including direct SQL writers.
            rewritten := replace(
                rewritten,
                blocked_negative_predicate,
                blocked_negative_predicate
                    || E'\n           AND ' || executable_component_predicate
            );
        END IF;
        IF authority_function_name = 'guard_schedule_revision_update' THEN
            -- The stale-placement arm uses the positive/inverse predicate.
            rewritten := replace(
                rewritten,
                blocked_positive_predicate,
                blocked_positive_predicate
                    || E'\n                   OR ' || missing_executable_component_predicate
            );
        END IF;
        EXECUTE rewritten;
    END LOOP;
END
$rewrite$;

-- Prepare the existing dependency table for the later authoritative graph
-- slice without changing API ownership in this migration.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM item_dependencies
        WHERE lag_seconds < 0
           OR lag_seconds > 31622400
           OR lag_seconds % 60 <> 0
    ) THEN
        RAISE EXCEPTION
            'item_dependencies contains lag_seconds outside the supported 0..31622400 whole-minute range; repair the rows before migration 0024'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

ALTER TABLE item_dependencies
    DROP CONSTRAINT item_dependencies_dependency_kind_check,
    ADD CONSTRAINT item_dependencies_dependency_kind_check CHECK (
        dependency_kind IN (
            'finish_to_start', 'start_to_start', 'finish_to_finish', 'start_to_finish'
        )
    ),
    ADD COLUMN dependency_strength varchar(16) NOT NULL DEFAULT 'hard',
    ADD COLUMN dependency_soft_weight integer,
    ADD CONSTRAINT item_dependencies_lag_seconds_check
        CHECK (lag_seconds BETWEEN 0 AND 31622400 AND lag_seconds % 60 = 0),
    ADD CONSTRAINT item_dependencies_strength_check CHECK (
        (dependency_strength = 'hard' AND dependency_soft_weight IS NULL)
        OR (
            dependency_strength = 'soft'
            AND dependency_soft_weight IS NOT NULL
            AND dependency_soft_weight BETWEEN 0 AND 1000000
        )
    );
