-- A completed, expanded Google Calendar window is a scheduling safety fence.
-- Raw provider identities stay in provider-only mapping/rejection tables. A
-- generation is visible to scheduling only after every occurrence mutation
-- and absence tombstone commits in the same transaction.

ALTER TABLE google_sync_collections
    ADD COLUMN planning_projection_state varchar(16) NOT NULL DEFAULT 'uninitialized'
        CHECK (planning_projection_state IN ('uninitialized', 'complete', 'failed')),
    ADD COLUMN planning_generation bigint NOT NULL DEFAULT 0
        CHECK (planning_generation >= 0),
    ADD COLUMN planning_collection_revision bigint
        CHECK (planning_collection_revision IS NULL OR planning_collection_revision > 0),
    ADD COLUMN planning_window_start timestamptz,
    ADD COLUMN planning_window_end timestamptz,
    ADD COLUMN planning_window_refreshed_at timestamptz,
    ADD COLUMN planning_last_error_code varchar(64),
    ADD CONSTRAINT google_sync_collections_planning_projection_check CHECK (
        (collection_kind <> 'calendar'
            AND planning_projection_state = 'uninitialized'
            AND planning_generation = 0
            AND planning_collection_revision IS NULL
            AND planning_window_start IS NULL
            AND planning_window_end IS NULL
            AND planning_window_refreshed_at IS NULL
            AND planning_last_error_code IS NULL)
        OR
        (collection_kind = 'calendar' AND (
            (planning_projection_state = 'uninitialized'
                AND planning_collection_revision IS NULL
                AND planning_window_start IS NULL
                AND planning_window_end IS NULL
                AND planning_window_refreshed_at IS NULL
                AND planning_last_error_code IS NULL)
            OR
            (planning_projection_state = 'complete'
                AND planning_generation > 0
                AND planning_collection_revision = revision
                AND planning_window_start IS NOT NULL
                AND planning_window_end IS NOT NULL
                AND planning_window_start < planning_window_end
                AND planning_window_refreshed_at IS NOT NULL
                AND planning_last_error_code IS NULL)
            OR
            (planning_projection_state = 'failed'
                AND planning_collection_revision IS NULL
                AND planning_window_start IS NULL
                AND planning_window_end IS NULL
                AND planning_window_refreshed_at IS NULL
                AND planning_last_error_code IS NOT NULL
                AND btrim(planning_last_error_code) <> '')
        ))
    );

-- Any relevant discovery/configuration mutation invalidates coverage even if a
-- future call path forgets to clear it explicitly. The last generation number
-- remains monotonic and its occurrence mappings remain recoverable.
CREATE FUNCTION invalidate_google_calendar_projection()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.planning_generation < OLD.planning_generation THEN
        RAISE EXCEPTION 'Calendar planning generation cannot decrease'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.revision IS DISTINCT FROM NEW.revision
       OR OLD.provider_access_role IS DISTINCT FROM NEW.provider_access_role
       OR OLD.provider_deleted IS DISTINCT FROM NEW.provider_deleted
       OR OLD.selected IS DISTINCT FROM NEW.selected
       OR OLD.visible IS DISTINCT FROM NEW.visible
       OR OLD.sync_role IS DISTINCT FROM NEW.sync_role
       OR OLD.confirmed_busy_policy IS DISTINCT FROM NEW.confirmed_busy_policy
       OR OLD.tentative_policy IS DISTINCT FROM NEW.tentative_policy
       OR OLD.free_policy IS DISTINCT FROM NEW.free_policy
       OR OLD.all_day_policy IS DISTINCT FROM NEW.all_day_policy THEN
        NEW.planning_projection_state := 'uninitialized';
        NEW.planning_collection_revision := NULL;
        NEW.planning_window_start := NULL;
        NEW.planning_window_end := NULL;
        NEW.planning_window_refreshed_at := NULL;
        NEW.planning_last_error_code := NULL;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER google_sync_collections_invalidate_projection
BEFORE UPDATE ON google_sync_collections
FOR EACH ROW
EXECUTE FUNCTION invalidate_google_calendar_projection();

CREATE INDEX google_sync_collections_planning_window_idx
    ON google_sync_collections (
        workspace_id,
        user_id,
        planning_window_start,
        planning_window_end
    )
    WHERE selected
      AND NOT provider_deleted
      AND collection_kind = 'calendar'
      AND sync_role IN ('blocking', 'writable')
      AND planning_projection_state = 'complete';

-- Recurrence-series mirror rows and expanded occurrences are separate durable
-- identities. Existing full-series sweeps continue to select entity_kind
-- 'item' and therefore cannot remove an expanded occurrence.
ALTER TABLE provider_sync_mappings
    DROP CONSTRAINT provider_sync_mappings_entity_kind_check,
    ADD CONSTRAINT provider_sync_mappings_entity_kind_check CHECK (
        entity_kind IN (
            'item', 'calendar_occurrence', 'schedule_block', 'calendar', 'task_list'
        )
    ),
    ADD COLUMN projection_generation bigint,
    ADD COLUMN provider_forced_sensitive boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT provider_sync_mappings_projection_generation_check CHECK (
        (entity_kind = 'calendar_occurrence'
            AND collection_id IS NOT NULL
            AND ownership = 'external'
            AND projection_generation IS NOT NULL
            AND projection_generation > 0)
        OR
        (entity_kind <> 'calendar_occurrence'
            AND projection_generation IS NULL
            AND NOT provider_forced_sensitive)
    );

-- Encode the active provider sensitivity floor as a foreign key to the exact
-- canonical (workspace, item, sensitivity) tuple. PostgreSQL's FK row locks
-- close the write-skew window between a direct item declassification and a
-- concurrent mapping insert/update; the generated key is NULL when the floor
-- is inactive, so ordinary mappings do not constrain item sensitivity.
ALTER TABLE items
    ADD CONSTRAINT items_workspace_id_sensitivity_uq
        UNIQUE (workspace_id, id, is_sensitive);

ALTER TABLE provider_sync_mappings
    ADD COLUMN sensitivity_floor_item_id uuid GENERATED ALWAYS AS (
        CASE
            WHEN provider_forced_sensitive AND tombstoned_at IS NULL
                THEN local_entity_id
            ELSE NULL
        END
    ) STORED,
    ADD CONSTRAINT provider_sync_mappings_sensitivity_floor_target_check CHECK (
        NOT provider_forced_sensitive
        OR tombstoned_at IS NOT NULL
        OR local_entity_id IS NOT NULL
    ),
    ADD CONSTRAINT provider_sync_mappings_sensitivity_floor_item_fk
        FOREIGN KEY (workspace_id, sensitivity_floor_item_id, provider_forced_sensitive)
        REFERENCES items(workspace_id, id, is_sensitive);

CREATE INDEX provider_sync_mappings_calendar_projection_idx
    ON provider_sync_mappings (
        workspace_id,
        provider_account_id,
        collection_id,
        projection_generation,
        remote_resource_id
    )
    WHERE entity_kind = 'calendar_occurrence'
      AND tombstoned_at IS NULL
      AND sync_state <> 'deleted_remote';

-- The pre-projection metadata cursor may point beyond unchanged legacy
-- canonical Calendar rows. Force exactly one cursorless full metadata scan so
-- recurring masters become metadata-only and legacy event projections retire.
DELETE FROM provider_sync_cursors cursor
USING google_sync_collections collection
WHERE cursor.workspace_id = collection.workspace_id
  AND cursor.provider_account_id = collection.provider_account_id
  AND collection.collection_kind = 'calendar'
  AND cursor.collection_key = 'calendar:' || collection.id::text;

-- Before expanded occurrences, inbound Calendar records were canonical items
-- under entity_kind 'item'. Retire a still-active provider-owned legacy item
-- only when every active external Calendar mapping agrees with the canonical
-- revision. A mismatch is a local edit, including a missing mapping revision;
-- preserve that item and detach all of its legacy Calendar mappings so the
-- cursorless metadata scan can advance without trying to retire it again.
-- Provider resource identities remain confined to provider_sync_mappings.
LOCK TABLE items, item_hierarchy IN SHARE ROW EXCLUSIVE MODE;

CREATE TEMPORARY TABLE dayweave_0014_legacy_calendar_divergences (
    mapping_id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    item_id uuid NOT NULL,
    actor_user_id uuid NOT NULL,
    item_revision bigint NOT NULL,
    mapping_local_revision bigint,
    mapping_revision_matches boolean NOT NULL,
    preservation_reason varchar(64) NOT NULL CHECK (preservation_reason IN (
        'calendar_projection_upgrade_local_revision_diverged',
        'calendar_projection_upgrade_shared_canonical_item'
    )),
    observed_at timestamptz NOT NULL
) ON COMMIT DROP;

CREATE TEMPORARY TABLE dayweave_0014_legacy_calendar_retirements (
    workspace_id uuid NOT NULL,
    item_id uuid NOT NULL,
    actor_user_id uuid NOT NULL,
    old_revision bigint NOT NULL,
    new_revision bigint NOT NULL,
    parent_id uuid,
    retired_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, item_id)
) ON COMMIT DROP;

-- Once any relevant mapping diverges, every relevant mapping for that item is
-- detached. Leaving a matching sibling attached would allow a later metadata
-- row to trash the locally edited item through that sibling.
INSERT INTO dayweave_0014_legacy_calendar_divergences (
    mapping_id,
    workspace_id,
    item_id,
    actor_user_id,
    item_revision,
    mapping_local_revision,
    mapping_revision_matches,
    preservation_reason,
    observed_at
)
SELECT
    mapping.id,
    item.workspace_id,
    item.id,
    item.created_by_user_id,
    item.revision,
    mapping.local_revision,
    mapping.local_revision IS NOT DISTINCT FROM item.revision,
    CASE
        WHEN EXISTS (
            SELECT 1
            FROM provider_sync_mappings divergent_mapping
            JOIN google_sync_collections divergent_collection
              ON divergent_collection.workspace_id = divergent_mapping.workspace_id
             AND divergent_collection.provider_account_id =
                 divergent_mapping.provider_account_id
             AND divergent_collection.id = divergent_mapping.collection_id
            WHERE divergent_mapping.workspace_id = item.workspace_id
              AND divergent_mapping.local_entity_id = item.id
              AND divergent_mapping.entity_kind = 'item'
              AND divergent_mapping.ownership = 'external'
              AND divergent_mapping.tombstoned_at IS NULL
              AND divergent_collection.collection_kind = 'calendar'
              AND divergent_mapping.local_revision IS DISTINCT FROM item.revision
        ) THEN 'calendar_projection_upgrade_local_revision_diverged'
        ELSE 'calendar_projection_upgrade_shared_canonical_item'
    END,
    current_timestamp
FROM provider_sync_mappings mapping
JOIN google_sync_collections collection
  ON collection.workspace_id = mapping.workspace_id
 AND collection.provider_account_id = mapping.provider_account_id
 AND collection.id = mapping.collection_id
JOIN items item
  ON item.workspace_id = mapping.workspace_id
 AND item.id = mapping.local_entity_id
WHERE collection.collection_kind = 'calendar'
  AND mapping.entity_kind = 'item'
  AND mapping.ownership = 'external'
  AND mapping.tombstoned_at IS NULL
  AND item.trashed_at IS NULL
  -- Every active mapping on the canonical item participates in the safety
  -- decision, not just Calendar mappings. Any ownership/non-Calendar sibling
  -- means this is a shared local item and therefore is not migration-owned.
  AND EXISTS (
      SELECT 1
      FROM provider_sync_mappings active_mapping
      WHERE active_mapping.workspace_id = item.workspace_id
        AND active_mapping.local_entity_id = item.id
        AND active_mapping.tombstoned_at IS NULL
        AND (
            active_mapping.entity_kind = 'item'
            AND active_mapping.ownership = 'external'
            AND active_mapping.local_revision = item.revision
            AND EXISTS (
                SELECT 1
                FROM google_sync_collections active_collection
                WHERE active_collection.workspace_id = active_mapping.workspace_id
                  AND active_collection.provider_account_id =
                      active_mapping.provider_account_id
                  AND active_collection.id = active_mapping.collection_id
                  AND active_collection.collection_kind = 'calendar'
            )
        ) IS NOT TRUE
  );

INSERT INTO dayweave_0014_legacy_calendar_retirements (
    workspace_id,
    item_id,
    actor_user_id,
    old_revision,
    new_revision,
    parent_id,
    retired_at
)
SELECT DISTINCT ON (item.workspace_id, item.id)
    item.workspace_id,
    item.id,
    item.created_by_user_id,
    item.revision,
    item.revision + 1,
    hierarchy.parent_item_id,
    current_timestamp
FROM provider_sync_mappings mapping
JOIN google_sync_collections collection
  ON collection.workspace_id = mapping.workspace_id
 AND collection.provider_account_id = mapping.provider_account_id
 AND collection.id = mapping.collection_id
JOIN items item
  ON item.workspace_id = mapping.workspace_id
 AND item.id = mapping.local_entity_id
LEFT JOIN item_hierarchy hierarchy
  ON hierarchy.workspace_id = item.workspace_id
 AND hierarchy.child_item_id = item.id
WHERE collection.collection_kind = 'calendar'
  AND mapping.entity_kind = 'item'
  AND mapping.ownership = 'external'
  AND mapping.tombstoned_at IS NULL
  AND item.trashed_at IS NULL
  AND mapping.local_revision = item.revision
  AND NOT EXISTS (
      SELECT 1
      FROM provider_sync_mappings active_mapping
      WHERE active_mapping.workspace_id = item.workspace_id
        AND active_mapping.local_entity_id = item.id
        AND active_mapping.tombstoned_at IS NULL
        AND (
            active_mapping.entity_kind = 'item'
            AND active_mapping.ownership = 'external'
            AND active_mapping.local_revision = item.revision
            AND EXISTS (
                SELECT 1
                FROM google_sync_collections active_collection
                WHERE active_collection.workspace_id = active_mapping.workspace_id
                  AND active_collection.provider_account_id =
                      active_mapping.provider_account_id
                  AND active_collection.id = active_mapping.collection_id
                  AND active_collection.collection_kind = 'calendar'
            )
        ) IS NOT TRUE
  )
ORDER BY item.workspace_id, item.id, mapping.id;

UPDATE items item
SET revision = retirement.new_revision,
    updated_at = retirement.retired_at,
    trashed_at = retirement.retired_at
FROM dayweave_0014_legacy_calendar_retirements retirement
WHERE item.workspace_id = retirement.workspace_id
  AND item.id = retirement.item_id
  AND item.revision = retirement.old_revision
  AND item.trashed_at IS NULL;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM dayweave_0014_legacy_calendar_retirements retirement
        LEFT JOIN items item
          ON item.workspace_id = retirement.workspace_id
         AND item.id = retirement.item_id
         AND item.revision = retirement.new_revision
         AND item.trashed_at = retirement.retired_at
        WHERE item.id IS NULL
    ) THEN
        RAISE EXCEPTION 'legacy Calendar projection retirement was not atomic';
    END IF;
END;
$$;

INSERT INTO item_changes (
    workspace_id,
    item_id,
    item_revision,
    change_kind,
    payload,
    changed_at
)
SELECT
    retirement.workspace_id,
    retirement.item_id,
    retirement.new_revision,
    'tombstone',
    jsonb_build_object(
        'id', retirement.item_id,
        'revision', retirement.new_revision,
        'deleted_at', retirement.retired_at,
        'parent_id', retirement.parent_id
    ),
    retirement.retired_at
FROM dayweave_0014_legacy_calendar_retirements retirement;

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
SELECT
    md5('dayweave:0014:legacy-calendar-outbox:' || retirement.workspace_id::text || ':' ||
        retirement.item_id::text)::uuid,
    retirement.workspace_id,
    'item',
    retirement.item_id,
    retirement.new_revision,
    'item.google_calendar_legacy_projection_retired_on_upgrade',
    'item.google_calendar_legacy_projection_retired_on_upgrade:' ||
        retirement.item_id::text || ':' || retirement.new_revision::text,
    jsonb_build_object(
        'item_id', retirement.item_id,
        'revision', retirement.new_revision,
        'change', 'tombstone'
    ),
    retirement.retired_at,
    retirement.retired_at,
    retirement.retired_at
FROM dayweave_0014_legacy_calendar_retirements retirement;

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
SELECT
    md5('dayweave:0014:legacy-calendar-audit:' || retirement.workspace_id::text || ':' ||
        retirement.item_id::text)::uuid,
    retirement.workspace_id,
    retirement.actor_user_id,
    'item.google_calendar_legacy_projection_retired_on_upgrade',
    'item',
    retirement.item_id,
    retirement.old_revision,
    retirement.new_revision,
    'succeeded',
    '{"source":"google_sync","reason":"calendar_projection_upgrade"}'::jsonb,
    retirement.retired_at
FROM dayweave_0014_legacy_calendar_retirements retirement;

UPDATE provider_sync_mappings mapping
SET local_revision = retirement.new_revision,
    sync_state = CASE
        WHEN mapping.sync_state = 'deleted_remote' THEN 'deleted_remote'
        ELSE 'pending_pull'
    END,
    conflict_metadata = NULL,
    updated_at = retirement.retired_at
FROM dayweave_0014_legacy_calendar_retirements retirement
WHERE mapping.workspace_id = retirement.workspace_id
  AND mapping.local_entity_id = retirement.item_id
  AND mapping.entity_kind = 'item'
  AND mapping.ownership = 'external'
  AND mapping.tombstoned_at IS NULL
  AND EXISTS (
      SELECT 1
      FROM google_sync_collections collection
      WHERE collection.workspace_id = mapping.workspace_id
        AND collection.provider_account_id = mapping.provider_account_id
        AND collection.id = mapping.collection_id
        AND collection.collection_kind = 'calendar'
  );

-- The restricted mapping row carries bounded, per-mapping conflict context
-- until the cursorless metadata scan observes that provider object. The audit
-- row is the durable evidence after the scan clears mapping conflict_metadata;
-- it contains only local UUIDs and revisions, never remote resource IDs.
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
SELECT
    md5('dayweave:0014:legacy-calendar-divergence-audit:' ||
        divergence.workspace_id::text || ':' || divergence.mapping_id::text)::uuid,
    divergence.workspace_id,
    divergence.actor_user_id,
    'item.google_calendar_legacy_projection_preserved_on_upgrade',
    'item',
    divergence.item_id,
    divergence.mapping_local_revision,
    divergence.item_revision,
    'conflicted',
    jsonb_build_object(
        'source', 'google_sync',
        'reason', divergence.preservation_reason,
        'mapping_id', divergence.mapping_id,
        'item_id', divergence.item_id,
        'item_revision', divergence.item_revision,
        'mapping_local_revision', divergence.mapping_local_revision,
        'mapping_revision_matches', divergence.mapping_revision_matches
    ),
    divergence.observed_at
FROM dayweave_0014_legacy_calendar_divergences divergence;

UPDATE provider_sync_mappings mapping
SET local_entity_id = NULL,
    local_revision = NULL,
    sync_state = 'conflict',
    conflict_metadata = jsonb_build_object(
        'reason', divergence.preservation_reason,
        'local_item_id', divergence.item_id,
        'item_revision', divergence.item_revision,
        'mapping_local_revision', divergence.mapping_local_revision,
        'mapping_revision_matches', divergence.mapping_revision_matches
    ),
    updated_at = divergence.observed_at
FROM dayweave_0014_legacy_calendar_divergences divergence
WHERE mapping.workspace_id = divergence.workspace_id
  AND mapping.id = divergence.mapping_id
  AND mapping.tombstoned_at IS NULL;

DROP TABLE dayweave_0014_legacy_calendar_divergences;
DROP TABLE dayweave_0014_legacy_calendar_retirements;

CREATE FUNCTION protect_google_occurrence_sensitivity()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    -- Do not acquire the workspace advisory lock from this row trigger: the
    -- UPDATE may already own the item tuple lock, which would invert the
    -- canonical advisory-lock-before-row-lock order. The composite FK above
    -- supplies concurrency serialization; this check supplies the domain
    -- error for the uncontended path.
    IF OLD.is_sensitive AND NOT NEW.is_sensitive AND EXISTS (
        SELECT 1 FROM provider_sync_mappings mapping
        WHERE mapping.workspace_id = OLD.workspace_id
          AND mapping.local_entity_id = OLD.id
          AND mapping.entity_kind = 'calendar_occurrence'
          AND mapping.provider_forced_sensitive
          AND mapping.tombstoned_at IS NULL
    ) THEN
        RAISE EXCEPTION 'provider-sensitive Calendar occurrence cannot be declassified'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER items_protect_google_occurrence_sensitivity
BEFORE UPDATE OF is_sensitive ON items
FOR EACH ROW
EXECUTE FUNCTION protect_google_occurrence_sensitivity();

CREATE FUNCTION preserve_google_occurrence_sensitivity_floor()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'UPDATE'
       AND OLD.provider_forced_sensitive
       AND NOT NEW.provider_forced_sensitive THEN
        RAISE EXCEPTION 'provider Calendar sensitivity floor is monotonic'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.provider_forced_sensitive AND NEW.tombstoned_at IS NULL THEN
        -- Mapping mutations take the same workspace lock as canonical item
        -- repositories, before taking the referenced item key lock. Direct
        -- item SQL never waits on this advisory lock from a row trigger, which
        -- avoids an item-row -> advisory-lock cycle with canonical callers.
        PERFORM pg_advisory_xact_lock(hashtextextended(
            'dayweave.items.v1:' || NEW.workspace_id::text,
            0
        ));
        PERFORM 1
        FROM items item
        WHERE item.workspace_id = NEW.workspace_id
          AND item.id = NEW.local_entity_id
          AND item.is_sensitive
        FOR KEY SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'provider Calendar sensitivity floor requires a sensitive item'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER provider_sync_mappings_validate_sensitivity_floor_insert
BEFORE INSERT ON provider_sync_mappings
FOR EACH ROW
EXECUTE FUNCTION preserve_google_occurrence_sensitivity_floor();

CREATE TRIGGER provider_sync_mappings_preserve_sensitivity_floor
BEFORE UPDATE OF provider_forced_sensitive, local_entity_id, workspace_id, tombstoned_at
ON provider_sync_mappings
FOR EACH ROW
EXECUTE FUNCTION preserve_google_occurrence_sensitivity_floor();

CREATE FUNCTION invalidate_projection_for_occurrence_item_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE google_sync_collections collection
    SET planning_projection_state = 'uninitialized',
        planning_collection_revision = NULL,
        planning_window_start = NULL,
        planning_window_end = NULL,
        planning_window_refreshed_at = NULL,
        planning_last_error_code = NULL
    WHERE collection.workspace_id = OLD.workspace_id
      AND collection.collection_kind = 'calendar'
      AND EXISTS (
          SELECT 1 FROM provider_sync_mappings mapping
          WHERE mapping.workspace_id = OLD.workspace_id
            AND mapping.collection_id = collection.id
            AND mapping.entity_kind = 'calendar_occurrence'
            AND mapping.local_entity_id = OLD.id
            AND mapping.tombstoned_at IS NULL
      );
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER items_invalidate_calendar_projection
AFTER UPDATE OR DELETE ON items
FOR EACH ROW
EXECUTE FUNCTION invalidate_projection_for_occurrence_item_mutation();

CREATE FUNCTION invalidate_projection_for_occurrence_mapping_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') AND OLD.entity_kind = 'calendar_occurrence' THEN
        UPDATE google_sync_collections
        SET planning_projection_state = 'uninitialized',
            planning_collection_revision = NULL,
            planning_window_start = NULL,
            planning_window_end = NULL,
            planning_window_refreshed_at = NULL,
            planning_last_error_code = NULL
        WHERE workspace_id = OLD.workspace_id
          AND id = OLD.collection_id
          AND collection_kind = 'calendar';
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') AND NEW.entity_kind = 'calendar_occurrence' THEN
        UPDATE google_sync_collections
        SET planning_projection_state = 'uninitialized',
            planning_collection_revision = NULL,
            planning_window_start = NULL,
            planning_window_end = NULL,
            planning_window_refreshed_at = NULL,
            planning_last_error_code = NULL
        WHERE workspace_id = NEW.workspace_id
          AND id = NEW.collection_id
          AND collection_kind = 'calendar';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER provider_sync_mappings_invalidate_calendar_projection
AFTER INSERT OR UPDATE OR DELETE ON provider_sync_mappings
FOR EACH ROW
EXECUTE FUNCTION invalidate_projection_for_occurrence_mapping_mutation();

-- Strengthen the pre-existing collection FK with exact account binding. This
-- also protects direct SQL callers from crossing two Google accounts in the
-- same personal workspace.
ALTER TABLE google_sync_collections
    ADD CONSTRAINT google_sync_collections_account_id_uq
        UNIQUE (workspace_id, provider_account_id, id),
    ADD CONSTRAINT google_sync_collections_owner_id_uq
        UNIQUE (workspace_id, user_id, provider_account_id, id);

ALTER TABLE provider_sync_mappings
    ADD CONSTRAINT provider_sync_mappings_account_collection_fk
        FOREIGN KEY (workspace_id, provider_account_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, provider_account_id, id);

-- Rejected occurrences invalidate coverage and are retained only in this
-- restricted provider table. General audit, item-change and scheduling
-- evidence stores only bounded aggregate reason codes/counts.
CREATE TABLE google_calendar_projection_rejections (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    collection_revision bigint NOT NULL CHECK (collection_revision > 0),
    remote_resource_id varchar(1000) NOT NULL,
    reason_code varchar(64) NOT NULL CHECK (reason_code IN (
        'canonical_item_invalid',
        'dayweave_marker_invalid',
        'event_bounds_invalid',
        'event_bounds_missing',
        'event_date_invalid',
        'event_duration_invalid',
        'event_timezone_invalid',
        'invalid_remote_id',
        'provider_metadata_invalid',
        'provider_payload_invalid',
        'timestamp_invalid',
        'unauthenticated_dayweave_marker'
    )),
    observed_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, provider_account_id, collection_id, remote_resource_id),
    FOREIGN KEY (workspace_id, user_id, provider_account_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, user_id, provider_account_id, id),
    CHECK (btrim(remote_resource_id) <> ''),
    CHECK (remote_resource_id !~ '[[:cntrl:]]')
);

CREATE INDEX google_calendar_projection_rejections_collection_idx
    ON google_calendar_projection_rejections (
        workspace_id,
        user_id,
        provider_account_id,
        collection_id,
        observed_at DESC
    );
