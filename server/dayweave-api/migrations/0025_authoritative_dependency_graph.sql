-- Make the normalized edge table the only durable dependency authority.
-- The one-time cutover accepts the previously validated portable metadata as
-- its source, rejects any contradictory dormant relational rows, and leaves
-- the API free to project edges back into item snapshots at read time.
-- HARD CUTOVER: every pre-0025 API/worker process must be drained before this
-- migration starts and may not be restarted against the migrated database.

-- Serialize the cutover with every pre-0025 item/dependency writer. A writer
-- already holding a row lock finishes before the snapshot; one queued behind
-- this lock resumes only after the post-cutover projection guard exists.
LOCK TABLE items, item_hierarchy, item_dependencies IN SHARE ROW EXCLUSIVE MODE;

-- Projection order is non-semantic, but retaining it prevents the cutover from
-- changing a serialized Item without a matching item revision and delta. New
-- aggregate writes use deterministic UUID order.
ALTER TABLE item_dependencies
    ADD COLUMN projection_ordinal integer;

-- Item-delta consumers must never persist the middle of a multi-item
-- aggregate mutation. Existing history remains nullable, while every writer
-- that resumes after this cutover must opt into one bounded group or fail
-- closed. This also prevents a queued pre-0025 binary from publishing a stale,
-- ungrouped projection after the normalized dependency authority is live.
ALTER TABLE item_changes
    ADD COLUMN change_group_id uuid;

CREATE INDEX item_changes_workspace_group_idx
    ON item_changes (workspace_id, change_group_id, sequence)
    WHERE change_group_id IS NOT NULL;

CREATE FUNCTION require_item_change_group()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.change_group_id IS NULL THEN
        RAISE EXCEPTION 'new item changes require an atomic delivery group'
            USING ERRCODE = '23514', CONSTRAINT = 'item_changes_group_required';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER item_changes_group_required
BEFORE INSERT ON item_changes
FOR EACH ROW EXECUTE FUNCTION require_item_change_group();

CREATE TEMP TABLE dayweave_dependency_cutover (
    workspace_id uuid NOT NULL,
    predecessor_item_id uuid NOT NULL,
    successor_item_id uuid NOT NULL,
    dependency_kind varchar(32) NOT NULL CHECK (
        dependency_kind IN (
            'finish_to_start', 'start_to_start', 'finish_to_finish', 'start_to_finish'
        )
    ),
    lag_seconds integer NOT NULL CHECK (
        lag_seconds BETWEEN 0 AND 31622400 AND lag_seconds % 60 = 0
    ),
    dependency_strength varchar(16) NOT NULL CHECK (
        dependency_strength IN ('hard', 'soft')
    ),
    dependency_soft_weight integer,
    projection_ordinal integer NOT NULL CHECK (projection_ordinal >= 0),
    PRIMARY KEY (workspace_id, predecessor_item_id, successor_item_id),
    UNIQUE (workspace_id, successor_item_id, projection_ordinal),
    CHECK (predecessor_item_id <> successor_item_id),
    CHECK (
        (dependency_strength = 'hard' AND dependency_soft_weight IS NULL)
        OR (
            dependency_strength = 'soft'
            AND dependency_soft_weight BETWEEN 0 AND 1000000
        )
    )
) ON COMMIT DROP;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM items
        WHERE scheduling_constraints #> '{constraints,dependencies}' IS NOT NULL
          AND jsonb_typeof(scheduling_constraints #> '{constraints,dependencies}') <> 'array'
    ) THEN
        RAISE EXCEPTION
            'legacy constraints.dependencies must be an array before migration 0025'
            USING ERRCODE = '23514';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM items AS item
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(item.scheduling_constraints #> '{constraints,dependencies}', '[]'::jsonb)
        ) AS dependency(value)
        WHERE jsonb_typeof(dependency.value) <> 'object'
           OR jsonb_typeof(dependency.value -> 'item_id') <> 'string'
           OR dependency.value ->> 'item_id' = '00000000-0000-0000-0000-000000000000'
           OR dependency.value ->> 'relation' NOT IN (
               'finish_to_start', 'start_to_start', 'finish_to_finish', 'start_to_finish'
           )
           OR jsonb_typeof(dependency.value -> 'minimum_lag') <> 'number'
           OR (dependency.value ->> 'minimum_lag') !~ '^[0-9]+$'
           OR jsonb_typeof(dependency.value -> 'strength') <> 'object'
           OR dependency.value #>> '{strength,level}' NOT IN ('hard', 'soft')
           OR (
               dependency.value #>> '{strength,level}' = 'hard'
               AND dependency.value #> '{strength,weight}' IS NOT NULL
           )
           OR (
               dependency.value #>> '{strength,level}' = 'soft'
               AND (
                   jsonb_typeof(dependency.value #> '{strength,weight}') <> 'number'
                   OR (dependency.value #>> '{strength,weight}') !~ '^[0-9]+$'
               )
           )
    ) THEN
        RAISE EXCEPTION
            'legacy constraints.dependencies contains an invalid edge before migration 0025'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

INSERT INTO dayweave_dependency_cutover (
    workspace_id,
    predecessor_item_id,
    successor_item_id,
    dependency_kind,
    lag_seconds,
    dependency_strength,
    dependency_soft_weight,
    projection_ordinal
)
SELECT
    item.workspace_id,
    (dependency.value ->> 'item_id')::uuid,
    item.id,
    dependency.value ->> 'relation',
    ((dependency.value ->> 'minimum_lag')::bigint * 60)::integer,
    dependency.value #>> '{strength,level}',
    CASE
        WHEN dependency.value #>> '{strength,level}' = 'soft'
            THEN (dependency.value #>> '{strength,weight}')::integer
        ELSE NULL
    END,
    (dependency.ordinal - 1)::integer
FROM items AS item
CROSS JOIN LATERAL jsonb_array_elements(
    COALESCE(item.scheduling_constraints #> '{constraints,dependencies}', '[]'::jsonb)
) WITH ORDINALITY AS dependency(value, ordinal);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dayweave_dependency_cutover AS dependency
        LEFT JOIN items AS predecessor
          ON predecessor.workspace_id = dependency.workspace_id
         AND predecessor.id = dependency.predecessor_item_id
        WHERE predecessor.id IS NULL
    ) THEN
        RAISE EXCEPTION
            'legacy constraints.dependencies references a missing workspace item before migration 0025'
            USING ERRCODE = '23503';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM item_dependencies AS existing
        LEFT JOIN dayweave_dependency_cutover AS dependency
          ON dependency.workspace_id = existing.workspace_id
         AND dependency.predecessor_item_id = existing.predecessor_item_id
         AND dependency.successor_item_id = existing.successor_item_id
        WHERE dependency.workspace_id IS NULL
           OR dependency.dependency_kind <> existing.dependency_kind
           OR dependency.lag_seconds <> existing.lag_seconds
           OR dependency.dependency_strength <> existing.dependency_strength
           OR dependency.dependency_soft_weight IS DISTINCT FROM existing.dependency_soft_weight
    ) THEN
        RAISE EXCEPTION
            'item_dependencies conflicts with the legacy metadata authority before migration 0025'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

INSERT INTO item_dependencies (
    workspace_id,
    predecessor_item_id,
    successor_item_id,
    dependency_kind,
    lag_seconds,
    dependency_strength,
    dependency_soft_weight,
    projection_ordinal
)
SELECT
    workspace_id,
    predecessor_item_id,
    successor_item_id,
    dependency_kind,
    lag_seconds,
    dependency_strength,
    dependency_soft_weight,
    projection_ordinal
FROM dayweave_dependency_cutover
ON CONFLICT (workspace_id, predecessor_item_id, successor_item_id) DO UPDATE
SET projection_ordinal = EXCLUDED.projection_ordinal;

ALTER TABLE item_dependencies
    ALTER COLUMN projection_ordinal SET DEFAULT 0,
    ALTER COLUMN projection_ordinal SET NOT NULL,
    ADD CONSTRAINT item_dependencies_projection_ordinal_check
        CHECK (projection_ordinal >= 0);

DO $$
BEGIN
    IF EXISTS (
        WITH RECURSIVE ordered_children AS (
            SELECT
                hierarchy.workspace_id,
                hierarchy.child_item_id AS predecessor_item_id,
                lead(hierarchy.child_item_id) OVER (
                    PARTITION BY hierarchy.workspace_id, hierarchy.parent_item_id
                    ORDER BY hierarchy.position, hierarchy.child_item_id
                ) AS successor_item_id
            FROM item_hierarchy AS hierarchy
            JOIN items AS routine
              ON routine.workspace_id = hierarchy.workspace_id
             AND routine.id = hierarchy.parent_item_id
             AND routine.trashed_at IS NULL
             AND routine.kind = 'routine'
             AND routine.scheduling_constraints -> 'routine_ordered' = 'true'::jsonb
            JOIN items AS child
              ON child.workspace_id = hierarchy.workspace_id
             AND child.id = hierarchy.child_item_id
             AND child.trashed_at IS NULL
        ), graph_edges AS (
            SELECT workspace_id, predecessor_item_id, successor_item_id
            FROM item_dependencies
            UNION
            SELECT workspace_id, predecessor_item_id, successor_item_id
            FROM ordered_children
            WHERE successor_item_id IS NOT NULL
        ), reach(workspace_id, start_item_id, current_item_id) AS (
            SELECT workspace_id, predecessor_item_id, successor_item_id
            FROM graph_edges
            UNION
            SELECT reach.workspace_id, reach.start_item_id, edge.successor_item_id
            FROM reach
            JOIN graph_edges AS edge
              ON edge.workspace_id = reach.workspace_id
             AND edge.predecessor_item_id = reach.current_item_id
        )
        SELECT 1 FROM reach WHERE start_item_id = current_item_id
    ) THEN
        RAISE EXCEPTION
            'item dependency graph contains a cycle before migration 0025'
            USING ERRCODE = '23514', CONSTRAINT = 'item_dependencies_acyclic';
    END IF;
END;
$$;

-- A pre-0025 proposal effect intentionally keeps its original Item snapshot:
-- the snapshot hash is durable audit evidence and the undo path projects its
-- embedded dependencies into the normalized table. Before removing the live
-- JSON authority, prove that every potentially actionable historical undo has
-- a well-formed dependency projection and would leave the complete explicit +
-- ordered-routine graph acyclic. An application whose fence already diverged
-- is not actionable and will continue to fail with undo_diverged before its
-- snapshot is read. Provider ownership and inverse-parent validity are not
-- durable exclusions: either can change before the undo expires, so every
-- applied, unexpired, unscrubbed application with exact fences is preflighted.
CREATE TEMP TABLE dayweave_actionable_legacy_undos (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    application_id uuid NOT NULL,
    PRIMARY KEY (workspace_id, user_id, application_id)
) ON COMMIT DROP;

INSERT INTO dayweave_actionable_legacy_undos (
    workspace_id,
    user_id,
    application_id
)
SELECT application.workspace_id,
       application.user_id,
       application.id
FROM proposal_applications AS application
WHERE application.status = 'applied'
  AND application.undo_expires_at > clock_timestamp()
  AND NOT EXISTS (
      SELECT 1
      FROM proposal_application_effects AS effect
      WHERE effect.workspace_id = application.workspace_id
        AND effect.user_id = application.user_id
        AND effect.application_id = application.id
        AND effect.snapshots_scrubbed_at IS NOT NULL
  )
  AND NOT EXISTS (
      SELECT 1
      FROM proposal_application_fences AS fence
      LEFT JOIN items AS item
        ON item.workspace_id = fence.workspace_id
       AND item.id = fence.item_id
      WHERE fence.workspace_id = application.workspace_id
        AND fence.user_id = application.user_id
        AND fence.application_id = application.id
        AND (
            item.id IS NULL
            OR item.revision <> fence.applied_revision
            OR (item.trashed_at IS NOT NULL) <> fence.applied_deleted
        )
  );

DO $$
DECLARE
    application_row record;
    estimated_payload_bytes bigint;
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dayweave_actionable_legacy_undos AS actionable
        JOIN proposal_application_effects AS effect
          ON effect.workspace_id = actionable.workspace_id
         AND effect.user_id = actionable.user_id
         AND effect.application_id = actionable.application_id
        WHERE effect.operation <> 'create_item'
          AND (
              effect.before_snapshot IS NULL
              OR (
                  effect.before_snapshot #> '{flexible_constraints,constraints,dependencies}'
                      IS NOT NULL
                  AND jsonb_typeof(
                      effect.before_snapshot
                          #> '{flexible_constraints,constraints,dependencies}'
                  ) <> 'array'
              )
          )
    ) THEN
        RAISE EXCEPTION
            'an actionable pre-0025 proposal undo contains a non-array dependency snapshot; undo or let it expire before migration 0025'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM dayweave_actionable_legacy_undos AS actionable
        JOIN proposal_application_effects AS effect
          ON effect.workspace_id = actionable.workspace_id
         AND effect.user_id = actionable.user_id
         AND effect.application_id = actionable.application_id
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(
                effect.before_snapshot
                    #> '{flexible_constraints,constraints,dependencies}',
                '[]'::jsonb
            )
        ) AS dependency(value)
        WHERE effect.operation <> 'create_item'
          AND (
              jsonb_typeof(dependency.value) <> 'object'
              OR jsonb_typeof(dependency.value -> 'item_id') <> 'string'
              OR (dependency.value ->> 'item_id') !~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
              OR dependency.value ->> 'item_id' = '00000000-0000-0000-0000-000000000000'
              OR dependency.value ->> 'relation' NOT IN (
                  'finish_to_start', 'start_to_start',
                  'finish_to_finish', 'start_to_finish'
              )
              OR jsonb_typeof(dependency.value -> 'minimum_lag') <> 'number'
              OR (dependency.value ->> 'minimum_lag') !~ '^[0-9]+$'
              OR (dependency.value ->> 'minimum_lag')::numeric > 527040
              OR jsonb_typeof(dependency.value -> 'strength') <> 'object'
              OR dependency.value #>> '{strength,level}' NOT IN ('hard', 'soft')
              OR (
                  dependency.value #>> '{strength,level}' = 'hard'
                  AND dependency.value #> '{strength,weight}' IS NOT NULL
              )
              OR (
                  dependency.value #>> '{strength,level}' = 'soft'
                  AND (
                      jsonb_typeof(dependency.value #> '{strength,weight}') <> 'number'
                      OR (dependency.value #>> '{strength,weight}') !~ '^[0-9]+$'
                      OR (dependency.value #>> '{strength,weight}')::numeric > 1000000
                  )
              )
          )
    ) THEN
        RAISE EXCEPTION
            'an actionable pre-0025 proposal undo contains an invalid dependency snapshot; undo or let it expire before migration 0025'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM dayweave_actionable_legacy_undos AS actionable
        JOIN proposal_application_effects AS effect
          ON effect.workspace_id = actionable.workspace_id
         AND effect.user_id = actionable.user_id
         AND effect.application_id = actionable.application_id
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(
                effect.before_snapshot
                    #> '{flexible_constraints,constraints,dependencies}',
                '[]'::jsonb
            )
        ) AS dependency(value)
        LEFT JOIN items AS predecessor
          ON predecessor.workspace_id = actionable.workspace_id
         AND predecessor.id = (dependency.value ->> 'item_id')::uuid
        WHERE effect.operation <> 'create_item'
          AND (
              predecessor.id IS NULL
              OR predecessor.id = effect.item_id
          )
    ) THEN
        RAISE EXCEPTION
            'an actionable pre-0025 proposal undo contains a missing or self dependency; repair, undo, or let it expire before migration 0025'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM dayweave_actionable_legacy_undos AS actionable
        JOIN proposal_application_effects AS effect
          ON effect.workspace_id = actionable.workspace_id
         AND effect.user_id = actionable.user_id
         AND effect.application_id = actionable.application_id
        CROSS JOIN LATERAL jsonb_array_elements(
            COALESCE(
                effect.before_snapshot
                    #> '{flexible_constraints,constraints,dependencies}',
                '[]'::jsonb
            )
        ) AS dependency(value)
        WHERE effect.operation <> 'create_item'
        GROUP BY actionable.workspace_id,
                 actionable.user_id,
                 actionable.application_id,
                 effect.item_id,
                 dependency.value ->> 'item_id'
        HAVING COUNT(*) > 1
    ) THEN
        RAISE EXCEPTION
            'an actionable pre-0025 proposal undo contains duplicate dependency predecessors; repair, undo, or let it expire before migration 0025'
            USING ERRCODE = '23514';
    END IF;

    -- `proposal_applications.effect_count` is constrained to at most 100 and
    -- effect item IDs are unique within an application. One inverse command
    -- emits at most its direct item plus its distinct old and new parents, so
    -- every historical undo is already bounded to 300 delta rows. Payload
    -- bytes need a separate check: 100 individually valid Items can exceed the
    -- 8 MiB atomic-page limit. Parent refreshes project the normalized
    -- dependency graph into their Item payload, so reconstruct that member
    -- instead of depending on the soon-to-be-removed embedded copy.
    -- `to_jsonb(items)` contains more persisted keys than the portable Item
    -- projection; the extra 1 KiB per row covers field-name differences and
    -- revision/timestamp growth. An unfenced inverse parent can still change
    -- (or be created) before expiry, so reserve 1 MiB for every such refresh;
    -- this exceeds the canonical Item maximum while an oversized legacy row
    -- already present at cutover is measured directly.
    FOR application_row IN
        SELECT workspace_id, user_id, application_id
        FROM dayweave_actionable_legacy_undos
        ORDER BY workspace_id, user_id, application_id
    LOOP
        WITH effect_shapes AS (
            SELECT effect.ordinal,
                   effect.item_id,
                   effect.operation,
                   effect.before_snapshot,
                   hierarchy.parent_item_id AS current_parent_id,
                   COALESCE(hierarchy.position, item.sibling_order)
                       AS current_sibling_order,
                   item.trashed_at IS NOT NULL AS current_deleted,
                   CASE
                       WHEN effect.operation <> 'create_item'
                           THEN (effect.before_snapshot ->> 'parent_id')::uuid
                       ELSE NULL
                   END AS before_parent_id,
                   CASE
                       WHEN effect.operation <> 'create_item'
                           THEN (effect.before_snapshot ->> 'sibling_order')::integer
                       ELSE NULL
                   END AS before_sibling_order,
                   CASE
                       WHEN effect.operation <> 'create_item' THEN
                           effect.before_snapshot -> 'deleted_at' IS NOT NULL
                           AND effect.before_snapshot -> 'deleted_at' <> 'null'::jsonb
                       ELSE NULL
                   END AS before_deleted
            FROM proposal_application_effects AS effect
            JOIN items AS item
              ON item.workspace_id = effect.workspace_id
             AND item.id = effect.item_id
            LEFT JOIN item_hierarchy AS hierarchy
              ON hierarchy.workspace_id = item.workspace_id
             AND hierarchy.child_item_id = item.id
            WHERE effect.workspace_id = application_row.workspace_id
              AND effect.user_id = application_row.user_id
              AND effect.application_id = application_row.application_id
        ), direct_payloads AS (
            SELECT CASE
                       WHEN operation = 'create_item' THEN 1024::bigint
                       ELSE octet_length(before_snapshot::text)::bigint + 1024
                   END AS payload_bytes
            FROM effect_shapes
        ), refresh_targets AS (
            SELECT effect.ordinal, effect.current_parent_id AS parent_item_id
            FROM effect_shapes AS effect
            WHERE effect.operation = 'create_item'
              AND effect.current_parent_id IS NOT NULL
            UNION ALL
            SELECT effect.ordinal, candidate.parent_item_id
            FROM effect_shapes AS effect
            CROSS JOIN LATERAL (
                SELECT DISTINCT parent_item_id
                FROM unnest(ARRAY[
                    effect.current_parent_id,
                    effect.before_parent_id
                ]) AS parent(parent_item_id)
                WHERE parent_item_id IS NOT NULL
            ) AS candidate
            WHERE effect.operation <> 'create_item'
              AND (
                  effect.current_parent_id IS DISTINCT FROM effect.before_parent_id
                  OR effect.current_sibling_order
                      IS DISTINCT FROM effect.before_sibling_order
                  OR effect.current_deleted IS DISTINCT FROM effect.before_deleted
              )
        ), parent_payloads AS (
            SELECT GREATEST(
                       COALESCE(
                           octet_length((
                               to_jsonb(parent_item) || jsonb_build_object(
                                   'scheduling_constraints',
                                   CASE
                                       WHEN jsonb_array_length(
                                           projected_dependencies.dependencies
                                       ) = 0
                                       THEN parent_item.scheduling_constraints
                                           #- '{constraints,dependencies}'
                                       ELSE parent_item.scheduling_constraints
                                           || jsonb_build_object(
                                               'constraints',
                                               COALESCE(
                                                   parent_item.scheduling_constraints
                                                       -> 'constraints',
                                                   '{}'::jsonb
                                               ) || jsonb_build_object(
                                                   'dependencies',
                                                   projected_dependencies.dependencies
                                               )
                                           )
                                   END
                               )
                           )::text),
                           0
                       )::bigint,
                       CASE
                           WHEN parent_effect.operation <> 'create_item' THEN
                               octet_length(parent_effect.before_snapshot::text)::bigint
                           ELSE 0
                       END,
                       CASE
                           WHEN parent_fence.item_id IS NULL THEN 1048576::bigint
                           ELSE 0
                       END
                   ) + 1024 AS payload_bytes
            FROM refresh_targets AS refresh
            LEFT JOIN items AS parent_item
              ON parent_item.workspace_id = application_row.workspace_id
             AND parent_item.id = refresh.parent_item_id
            LEFT JOIN effect_shapes AS parent_effect
              ON parent_effect.item_id = refresh.parent_item_id
            LEFT JOIN proposal_application_fences AS parent_fence
              ON parent_fence.workspace_id = application_row.workspace_id
             AND parent_fence.user_id = application_row.user_id
             AND parent_fence.application_id = application_row.application_id
             AND parent_fence.item_id = refresh.parent_item_id
            LEFT JOIN LATERAL (
                SELECT COALESCE(
                           jsonb_agg(
                               jsonb_build_object(
                                   'item_id', dependency.predecessor_item_id,
                                   'relation', dependency.dependency_kind,
                                   'minimum_lag', dependency.lag_seconds / 60,
                                   'strength', CASE
                                       WHEN dependency.dependency_strength = 'hard'
                                           THEN jsonb_build_object('level', 'hard')
                                       ELSE jsonb_build_object(
                                           'level', 'soft',
                                           'weight', dependency.dependency_soft_weight
                                       )
                                   END
                               ) ORDER BY dependency.projection_ordinal,
                                          dependency.predecessor_item_id
                           ),
                           '[]'::jsonb
                       ) AS dependencies
                FROM item_dependencies AS dependency
                WHERE dependency.workspace_id = application_row.workspace_id
                  AND dependency.successor_item_id = refresh.parent_item_id
            ) AS projected_dependencies ON true
        )
        SELECT COALESCE((SELECT SUM(payload_bytes) FROM direct_payloads), 0)
             + COALESCE((SELECT SUM(payload_bytes) FROM parent_payloads), 0)
          INTO estimated_payload_bytes;

        IF estimated_payload_bytes > 8388608 THEN
            RAISE EXCEPTION
                'actionable pre-0025 proposal undo % can exceed the 8 MiB atomic delta payload bound (% estimated bytes); undo it or let it expire before migration 0025',
                application_row.application_id,
                estimated_payload_bytes
                USING ERRCODE = '23514';
        END IF;
    END LOOP;

    FOR application_row IN
        SELECT workspace_id, user_id, application_id
        FROM dayweave_actionable_legacy_undos
        ORDER BY workspace_id, user_id, application_id
    LOOP
        IF EXISTS (
            WITH RECURSIVE application_effects AS (
                SELECT effect.item_id,
                       effect.operation,
                       effect.before_snapshot
                FROM proposal_application_effects AS effect
                WHERE effect.workspace_id = application_row.workspace_id
                  AND effect.user_id = application_row.user_id
                  AND effect.application_id = application_row.application_id
            ), hypothetical_items AS (
                SELECT item.id,
                       CASE
                           WHEN effect.operation = 'create_item' THEN true
                           WHEN effect.operation IS NOT NULL THEN
                               effect.before_snapshot -> 'deleted_at' IS NOT NULL
                               AND effect.before_snapshot -> 'deleted_at' <> 'null'::jsonb
                           ELSE item.trashed_at IS NOT NULL
                       END AS deleted,
                       CASE
                           WHEN effect.operation <> 'create_item'
                               THEN effect.before_snapshot ->> 'kind'
                           ELSE item.kind
                       END AS kind,
                       CASE
                           WHEN effect.operation <> 'create_item' THEN
                               effect.before_snapshot
                                   #> '{flexible_constraints,routine_ordered}' = 'true'::jsonb
                           ELSE item.scheduling_constraints -> 'routine_ordered' = 'true'::jsonb
                       END AS routine_ordered,
                       CASE
                           WHEN effect.operation <> 'create_item'
                               THEN (effect.before_snapshot ->> 'parent_id')::uuid
                           ELSE hierarchy.parent_item_id
                       END AS parent_item_id,
                       CASE
                           WHEN effect.operation <> 'create_item'
                               THEN (effect.before_snapshot ->> 'sibling_order')::integer
                           ELSE COALESCE(hierarchy.position, item.sibling_order)
                       END AS sibling_order
                FROM items AS item
                LEFT JOIN item_hierarchy AS hierarchy
                  ON hierarchy.workspace_id = item.workspace_id
                 AND hierarchy.child_item_id = item.id
                LEFT JOIN application_effects AS effect
                  ON effect.item_id = item.id
                WHERE item.workspace_id = application_row.workspace_id
            ), snapshot_edges AS (
                SELECT (dependency.value ->> 'item_id')::uuid AS predecessor_item_id,
                       effect.item_id AS successor_item_id
                FROM application_effects AS effect
                CROSS JOIN LATERAL jsonb_array_elements(
                    COALESCE(
                        effect.before_snapshot
                            #> '{flexible_constraints,constraints,dependencies}',
                        '[]'::jsonb
                    )
                ) AS dependency(value)
                WHERE effect.operation <> 'create_item'
            ), explicit_edges AS (
                SELECT dependency.predecessor_item_id,
                       dependency.successor_item_id
                FROM item_dependencies AS dependency
                WHERE dependency.workspace_id = application_row.workspace_id
                  AND NOT EXISTS (
                      SELECT 1
                      FROM application_effects AS effect
                      WHERE effect.operation <> 'create_item'
                        AND effect.item_id = dependency.successor_item_id
                  )
                UNION
                SELECT predecessor_item_id, successor_item_id
                FROM snapshot_edges
            ), ordered_children AS (
                SELECT child.id AS predecessor_item_id,
                       lead(child.id) OVER (
                           PARTITION BY child.parent_item_id
                           ORDER BY child.sibling_order, child.id
                       ) AS successor_item_id
                FROM hypothetical_items AS child
                JOIN hypothetical_items AS routine
                  ON routine.id = child.parent_item_id
                WHERE NOT child.deleted
                  AND NOT routine.deleted
                  AND routine.kind = 'routine'
                  AND routine.routine_ordered
            ), graph_edges AS (
                SELECT predecessor_item_id, successor_item_id
                FROM explicit_edges
                UNION
                SELECT predecessor_item_id, successor_item_id
                FROM ordered_children
                WHERE successor_item_id IS NOT NULL
            ), reach(start_item_id, current_item_id) AS (
                SELECT predecessor_item_id, successor_item_id
                FROM graph_edges
                UNION
                SELECT reach.start_item_id, edge.successor_item_id
                FROM reach
                JOIN graph_edges AS edge
                  ON edge.predecessor_item_id = reach.current_item_id
            )
            SELECT 1
            FROM reach
            WHERE start_item_id = current_item_id
        ) THEN
            RAISE EXCEPTION
                'actionable pre-0025 proposal undo % would create a dependency cycle; repair or undo it, or let it expire before migration 0025',
                application_row.application_id
                USING ERRCODE = '23514', CONSTRAINT = 'item_dependencies_acyclic';
        END IF;
    END LOOP;
END;
$$;

UPDATE items
SET scheduling_constraints = CASE
    WHEN (scheduling_constraints -> 'constraints') - 'dependencies' = '{}'::jsonb
        THEN scheduling_constraints - 'constraints'
    ELSE jsonb_set(
        scheduling_constraints,
        '{constraints}',
        (scheduling_constraints -> 'constraints') - 'dependencies',
        false
    )
END
WHERE scheduling_constraints #> '{constraints,dependencies}' IS NOT NULL;

-- Old binaries that were waiting on the cutover lock must fail closed instead
-- of resurrecting the removed JSON authority after this migration commits.
CREATE FUNCTION reject_embedded_item_dependencies()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.scheduling_constraints #> '{constraints,dependencies}' IS NOT NULL THEN
        RAISE EXCEPTION
            'dependencies must be changed through the authoritative item dependency graph'
            USING ERRCODE = '23514', CONSTRAINT = 'items_dependency_projection_forbidden';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER items_dependency_projection_forbidden
BEFORE INSERT OR UPDATE OF scheduling_constraints ON items
FOR EACH ROW EXECUTE FUNCTION reject_embedded_item_dependencies();

CREATE FUNCTION guard_item_dependency_aggregate_write()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF current_setting('dayweave.item_dependency_write', true) IS DISTINCT FROM 'aggregate-v1' THEN
        RAISE EXCEPTION
            'item_dependencies may only change through a revisioned item aggregate mutation'
            USING ERRCODE = '42501', CONSTRAINT = 'item_dependencies_aggregate_write_guard';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER item_dependencies_aggregate_write_guard
BEFORE INSERT OR UPDATE OR DELETE ON item_dependencies
FOR EACH ROW EXECUTE FUNCTION guard_item_dependency_aggregate_write();

CREATE FUNCTION reject_item_dependency_cycle()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF EXISTS (
        WITH RECURSIVE reachable(item_id) AS (
            SELECT dependency.successor_item_id
            FROM item_dependencies AS dependency
            WHERE dependency.workspace_id = NEW.workspace_id
              AND dependency.predecessor_item_id = NEW.successor_item_id
            UNION
            SELECT dependency.successor_item_id
            FROM reachable
            JOIN item_dependencies AS dependency
              ON dependency.workspace_id = NEW.workspace_id
             AND dependency.predecessor_item_id = reachable.item_id
        )
        SELECT 1 FROM reachable WHERE item_id = NEW.predecessor_item_id
    ) THEN
        RAISE EXCEPTION
            'item dependency graph would contain a cycle'
            USING ERRCODE = '23514', CONSTRAINT = 'item_dependencies_acyclic';
    END IF;
    RETURN NEW;
END;
$$;

CREATE CONSTRAINT TRIGGER item_dependencies_acyclic
AFTER INSERT OR UPDATE ON item_dependencies
DEFERRABLE INITIALLY IMMEDIATE
FOR EACH ROW EXECUTE FUNCTION reject_item_dependency_cycle();

-- Proposal graph batches can execute parent commands before their submitted
-- position and must undo in reverse execution order. Preserve the separately
-- reviewed order for receipts while the existing ordinal becomes the durable
-- execution/undo order. Historical applications used one order for both.
ALTER TABLE proposal_application_effects
    ADD COLUMN review_ordinal smallint;

DROP TRIGGER proposal_application_effects_guard_mutation
    ON proposal_application_effects;

UPDATE proposal_application_effects
SET review_ordinal = ordinal;

-- Historical execution ordinals were the reviewed order. Reusing them is
-- safe only if the old deferred evidence invariant is still complete for
-- every application; prove that invariant during the backfill rather than
-- relying on a new INSERT-only constraint trigger to revisit old rows.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM proposal_applications AS application
        LEFT JOIN LATERAL (
            SELECT COUNT(*) AS effect_count,
                   MIN(effect.ordinal) AS first_execution_ordinal,
                   MAX(effect.ordinal) AS last_execution_ordinal,
                   MIN(effect.review_ordinal) AS first_review_ordinal,
                   MAX(effect.review_ordinal) AS last_review_ordinal,
                   COUNT(*) FILTER (
                       WHERE effect.review_ordinal <> effect.ordinal
                   ) AS mismatched_ordinals
            FROM proposal_application_effects AS effect
            WHERE effect.workspace_id = application.workspace_id
              AND effect.user_id = application.user_id
              AND effect.application_id = application.id
        ) AS evidence ON true
        WHERE evidence.effect_count <> application.effect_count
           OR evidence.first_execution_ordinal <> 0
           OR evidence.last_execution_ordinal <> application.effect_count - 1
           OR evidence.first_review_ordinal <> 0
           OR evidence.last_review_ordinal <> application.effect_count - 1
           OR evidence.mismatched_ordinals <> 0
    ) THEN
        RAISE EXCEPTION
            'historical proposal effect order is incomplete before migration 0025'
            USING ERRCODE = '23514';
    END IF;
END;
$$;

ALTER TABLE proposal_application_effects
    ALTER COLUMN review_ordinal SET NOT NULL,
    ADD CONSTRAINT proposal_application_effects_review_ordinal_check
        CHECK (review_ordinal BETWEEN 0 AND 99),
    ADD CONSTRAINT proposal_application_effects_review_ordinal_uq
        UNIQUE (workspace_id, user_id, application_id, review_ordinal);

CREATE OR REPLACE FUNCTION guard_proposal_application_effect_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $guard$
DECLARE
    expiry timestamptz;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.snapshots_scrubbed_at IS NOT NULL THEN
            RAISE EXCEPTION 'new proposal effects require undo snapshots'
                USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'proposal application effects are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF ROW(
        OLD.workspace_id, OLD.user_id, OLD.application_id, OLD.ordinal,
        OLD.review_ordinal, OLD.action_id, OLD.operation, OLD.command_hash,
        OLD.item_id, OLD.expected_revision, OLD.before_revision,
        OLD.after_revision, OLD.before_deleted, OLD.after_deleted,
        OLD.before_snapshot_hash, OLD.after_snapshot_hash, OLD.created_at
    ) IS DISTINCT FROM ROW(
        NEW.workspace_id, NEW.user_id, NEW.application_id, NEW.ordinal,
        NEW.review_ordinal, NEW.action_id, NEW.operation, NEW.command_hash,
        NEW.item_id, NEW.expected_revision, NEW.before_revision,
        NEW.after_revision, NEW.before_deleted, NEW.after_deleted,
        NEW.before_snapshot_hash, NEW.after_snapshot_hash, NEW.created_at
    )
       OR OLD.snapshots_scrubbed_at IS NOT NULL
       OR NEW.before_snapshot IS NOT NULL
       OR NEW.after_snapshot IS NOT NULL
    THEN
        RAISE EXCEPTION 'proposal effect permits only one-way snapshot scrubbing'
            USING ERRCODE = '23514';
    END IF;
    SELECT undo_expires_at INTO expiry
      FROM proposal_applications
     WHERE workspace_id = OLD.workspace_id
       AND user_id = OLD.user_id
       AND id = OLD.application_id
     FOR KEY SHARE;
    IF NOT FOUND OR clock_timestamp() < expiry THEN
        RAISE EXCEPTION 'proposal effect snapshots are still required for undo'
            USING ERRCODE = '23514';
    END IF;
    NEW.snapshots_scrubbed_at := clock_timestamp();
    RETURN NEW;
END
$guard$;

CREATE TRIGGER proposal_application_effects_guard_mutation
    BEFORE INSERT OR UPDATE OR DELETE ON proposal_application_effects
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_application_effect_mutation();

-- Execution ordinals were already proven contiguous by the application
-- evidence validator. Review ordinals now have the same deferred completeness
-- proof, while their unique constraint prevents duplicates.
CREATE FUNCTION validate_proposal_application_review_order()
RETURNS trigger
LANGUAGE plpgsql
AS $guard$
DECLARE
    expected_count smallint;
    actual_count bigint;
    first_ordinal smallint;
    last_ordinal smallint;
BEGIN
    SELECT effect_count INTO expected_count
      FROM proposal_applications
     WHERE workspace_id = NEW.workspace_id
       AND user_id = NEW.user_id
       AND id = NEW.application_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'proposal application review order has no application'
            USING ERRCODE = '23514';
    END IF;

    SELECT COUNT(*), MIN(review_ordinal), MAX(review_ordinal)
      INTO actual_count, first_ordinal, last_ordinal
      FROM proposal_application_effects
     WHERE workspace_id = NEW.workspace_id
       AND user_id = NEW.user_id
       AND application_id = NEW.application_id;
    IF actual_count <> expected_count
       OR first_ordinal <> 0
       OR last_ordinal <> expected_count - 1
    THEN
        RAISE EXCEPTION 'proposal application review order is incomplete'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER proposal_application_effects_review_complete
    AFTER INSERT ON proposal_application_effects
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_application_review_order();
