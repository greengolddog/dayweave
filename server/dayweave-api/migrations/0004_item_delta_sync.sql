-- Canonical item sibling ordering and an append-only offline delta stream.
-- Item mutations, audit rows, outbox messages, idempotency completion, and these
-- change records are committed by the item repository in one transaction.

ALTER TABLE items
    ADD COLUMN sibling_order integer NOT NULL DEFAULT 0
        CHECK (sibling_order BETWEEN 0 AND 1000000);

CREATE TABLE item_changes (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    change_kind varchar(24) NOT NULL
        CHECK (change_kind IN ('upsert', 'tombstone')),
    payload jsonb NOT NULL,
    changed_at timestamptz NOT NULL DEFAULT current_timestamp,
    UNIQUE (workspace_id, item_id, item_revision),
    CHECK (jsonb_typeof(payload) = 'object')
);

CREATE INDEX item_changes_workspace_delta_idx
    ON item_changes (workspace_id, sequence);

-- The table predates its API adapter. Backfill any production-shaped rows so a
-- first cursor observes the complete canonical state rather than only new writes.
INSERT INTO item_changes (
    workspace_id,
    item_id,
    item_revision,
    change_kind,
    payload,
    changed_at
)
SELECT
    item.workspace_id,
    item.id,
    item.revision,
    CASE WHEN item.trashed_at IS NULL THEN 'upsert' ELSE 'tombstone' END,
    CASE
        WHEN item.trashed_at IS NULL THEN jsonb_build_object(
            'id', item.id,
            'kind', item.kind,
            'status', item.status,
            'title', item.title,
            'notes', item.notes,
            'timezone_name', item.timezone_name,
            'duration_seconds', item.duration_seconds,
            'deadline_at', item.deadline_at,
            'earliest_start_at', item.earliest_start_at,
            'recurrence', item.recurrence,
            'flexible_constraints', item.scheduling_constraints,
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
            'is_executable', NOT EXISTS (
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
            'deleted_at', item.trashed_at
        )
        ELSE jsonb_build_object(
            'id', item.id,
            'revision', item.revision,
            'deleted_at', item.trashed_at,
            'parent_id', hierarchy.parent_item_id
        )
    END,
    item.updated_at
FROM items AS item
LEFT JOIN item_hierarchy AS hierarchy
  ON hierarchy.workspace_id = item.workspace_id
 AND hierarchy.child_item_id = item.id;
