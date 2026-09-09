-- Independent records, never scheduling demand, execution credit or lifecycle.
CREATE FUNCTION valid_item_progress_label(value text, maximum_length integer) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT AS $label$
    SELECT char_length(value) BETWEEN 1 AND maximum_length
       AND value !~ U&'[\0001-\001F\007F-\009F]'
       AND left(value,1) !~ U&'[\0009-\000D\0020\0085\00A0\1680\2000-\200A\2028\2029\202F\205F\3000]'
       AND right(value,1) !~ U&'[\0009-\000D\0020\0085\00A0\1680\2000-\200A\2028\2029\202F\205F\3000]'
$label$;

CREATE FUNCTION valid_item_progress_decimal(value text) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT AS $decimal$
    SELECT value ~ '^-?(0|[1-9][0-9]{0,11})(\.[0-9]{0,5}[1-9])?$' AND value <> '-0'
$decimal$;

CREATE FUNCTION valid_item_progress_components(components jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $components$
DECLARE
    component jsonb;
    value jsonb;
    target jsonb;
    component_id uuid;
    seen uuid[] := ARRAY[]::uuid[];
    kind text;
BEGIN
    IF jsonb_typeof(components) <> 'array' OR jsonb_array_length(components) > 16 THEN RETURN false; END IF;
    FOR component IN SELECT jsonb_array_elements(components) LOOP
        IF jsonb_typeof(component) <> 'object' OR NOT component ?& ARRAY['id','name','value']
           OR component - ARRAY['id','name','value'] <> '{}'::jsonb
           OR jsonb_typeof(component->'id') <> 'string'
           OR (component->>'id') !~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
           OR jsonb_typeof(component->'name') <> 'string'
           OR NOT valid_item_progress_label(component->>'name',80)
        THEN RETURN false; END IF;
        component_id := (component->>'id')::uuid;
        IF component_id = '00000000-0000-0000-0000-000000000000'::uuid OR component_id = ANY(seen) THEN RETURN false; END IF;
        seen := array_append(seen,component_id);
        value := component->'value';
        IF jsonb_typeof(value) <> 'object' OR jsonb_typeof(value->'type') IS DISTINCT FROM 'string' THEN RETURN false; END IF;
        kind := value->>'type';
        IF kind = 'percentage' THEN
            IF NOT value ?& ARRAY['type','basis_points'] OR value - ARRAY['type','basis_points'] <> '{}'::jsonb
               OR jsonb_typeof(value->'basis_points') <> 'number'
               OR (value->>'basis_points') !~ '^(0|[1-9][0-9]*)$'
               OR (value->>'basis_points')::numeric > 10000 THEN RETURN false; END IF;
        ELSIF kind = 'time' THEN
            IF NOT value ?& ARRAY['type','elapsed_seconds','remaining_seconds']
               OR value - ARRAY['type','elapsed_seconds','remaining_seconds'] <> '{}'::jsonb
               OR jsonb_typeof(value->'elapsed_seconds') <> 'number'
               OR (value->>'elapsed_seconds') !~ '^(0|[1-9][0-9]*)$'
               OR (value->>'elapsed_seconds')::numeric > 3155760000 THEN RETURN false; END IF;
            IF value->'remaining_seconds' <> 'null'::jsonb AND (
                jsonb_typeof(value->'remaining_seconds') <> 'number'
                OR (value->>'remaining_seconds') !~ '^(0|[1-9][0-9]*)$'
                OR (value->>'remaining_seconds')::numeric > 3155760000
            ) THEN RETURN false; END IF;
        ELSIF kind = 'quantity' THEN
            IF NOT value ?& ARRAY['type','current','unit','target']
               OR value - ARRAY['type','current','unit','target'] <> '{}'::jsonb
               OR jsonb_typeof(value->'current') <> 'string'
               OR NOT valid_item_progress_decimal(value->>'current')
               OR jsonb_typeof(value->'unit') <> 'string'
               OR NOT valid_item_progress_label(value->>'unit',32) THEN RETURN false; END IF;
            target := value->'target';
            IF target <> 'null'::jsonb AND (
                jsonb_typeof(target) <> 'object' OR NOT target ?& ARRAY['value','direction']
                OR target - ARRAY['value','direction'] <> '{}'::jsonb
                OR jsonb_typeof(target->'value') <> 'string'
                OR NOT valid_item_progress_decimal(target->>'value')
                OR jsonb_typeof(target->'direction') <> 'string'
                OR target->>'direction' NOT IN ('at_least','at_most')
            ) THEN RETURN false; END IF;
        ELSE RETURN false;
        END IF;
    END LOOP;
    RETURN true;
END
$components$;

CREATE FUNCTION valid_item_progress_snapshot(snapshot jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $snapshot$
BEGIN
    IF NOT coalesce(jsonb_typeof(snapshot) = 'object'
        AND snapshot ?& ARRAY['schema_version','item_id','item_revision','revision','components','updated_at']
        AND snapshot - ARRAY['schema_version','item_id','item_revision','revision','components','updated_at'] = '{}'::jsonb
        AND snapshot->'schema_version' = '1'::jsonb
        AND jsonb_typeof(snapshot->'item_id') = 'string'
        AND snapshot->>'item_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        AND snapshot->>'item_id' <> '00000000-0000-0000-0000-000000000000'
        AND jsonb_typeof(snapshot->'item_revision') = 'number'
        AND snapshot->>'item_revision' ~ '^[1-9][0-9]*$'
        AND (snapshot->>'item_revision')::numeric <= 9223372036854775807
        AND jsonb_typeof(snapshot->'revision') = 'number'
        AND snapshot->>'revision' ~ '^(0|[1-9][0-9]*)$'
        AND (snapshot->>'revision')::numeric <= 9223372036854775807
        AND valid_item_progress_components(snapshot->'components'),false)
    THEN RETURN false; END IF;
    IF snapshot->'revision' = '0'::jsonb THEN
        RETURN snapshot->'components' = '[]'::jsonb AND snapshot->'updated_at' = 'null'::jsonb;
    END IF;
    RETURN jsonb_typeof(snapshot->'updated_at') = 'string'
        AND snapshot->>'updated_at' ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?(Z|\+00:00)$'
        AND isfinite((snapshot->>'updated_at')::timestamptz);
END
$snapshot$;

CREATE TABLE item_progress (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    item_id uuid NOT NULL,
    revision bigint NOT NULL CHECK(revision > 0),
    components jsonb NOT NULL CHECK(valid_item_progress_components(components)),
    updated_at timestamptz NOT NULL,
    PRIMARY KEY(workspace_id,item_id),
    FOREIGN KEY(workspace_id,item_id) REFERENCES items(workspace_id,id),
    CHECK(item_id <> '00000000-0000-0000-0000-000000000000'::uuid)
);

-- Permanent, workspace-wide operation custody also stores immutable pre/post audit values.
CREATE TABLE item_progress_operations (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    operation_id uuid NOT NULL CHECK(operation_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    item_id uuid NOT NULL,
    actor_user_id uuid NOT NULL,
    actor_session_id uuid,
    progress_revision bigint NOT NULL CHECK(progress_revision > 0),
    request_json jsonb NOT NULL CHECK(jsonb_typeof(request_json) = 'object'),
    before_json jsonb NOT NULL CHECK(jsonb_typeof(before_json) = 'object'),
    result_json jsonb NOT NULL CHECK(jsonb_typeof(result_json) = 'object'),
    recorded_at timestamptz NOT NULL,
    PRIMARY KEY(workspace_id,operation_id),
    UNIQUE(workspace_id,item_id,progress_revision),
    FOREIGN KEY(workspace_id,item_id) REFERENCES items(workspace_id,id),
    FOREIGN KEY(workspace_id,actor_user_id) REFERENCES workspace_members(workspace_id,user_id),
    CHECK(octet_length(request_json::text) <= 65536 AND octet_length(before_json::text) <= 65536 AND octet_length(result_json::text) <= 65536),
    CHECK(request_json ?& ARRAY['schema_version','operation_id','expected_item_revision','expected_progress_revision','components']
        AND request_json - ARRAY['schema_version','operation_id','expected_item_revision','expected_progress_revision','components'] = '{}'::jsonb),
    CHECK(before_json ?& ARRAY['schema_version','item_id','item_revision','revision','components','updated_at']
        AND result_json ?& ARRAY['schema_version','item_id','item_revision','revision','components','updated_at']),
    CHECK(jsonb_typeof(request_json->'components') = 'array' AND jsonb_typeof(before_json->'components') = 'array' AND jsonb_typeof(result_json->'components') = 'array'),
    CHECK(request_json->>'schema_version' = '1' AND result_json->>'schema_version' = '1' AND before_json->>'schema_version' = '1'),
    CHECK(request_json->>'operation_id' = operation_id::text AND result_json->>'item_id' = item_id::text AND before_json->>'item_id' = item_id::text),
    CHECK(result_json->>'revision' = progress_revision::text AND before_json->>'revision' = (progress_revision-1)::text),
    CHECK(request_json->>'expected_progress_revision' = (progress_revision-1)::text),
    CHECK(result_json->>'item_revision' = request_json->>'expected_item_revision' AND before_json->>'item_revision' = result_json->>'item_revision'),
    CHECK(valid_item_progress_components(request_json->'components') AND valid_item_progress_components(before_json->'components') AND valid_item_progress_components(result_json->'components')),
    CHECK(result_json->'components' = request_json->'components'),
    CHECK(valid_item_progress_snapshot(before_json) AND valid_item_progress_snapshot(result_json)),
    CHECK(coalesce(request_json->'schema_version' = '1'::jsonb
        AND jsonb_typeof(request_json->'operation_id') = 'string'
        AND jsonb_typeof(request_json->'expected_item_revision') = 'number'
        AND jsonb_typeof(request_json->'expected_progress_revision') = 'number'
        AND request_json->'expected_item_revision' = result_json->'item_revision'
        AND request_json->'expected_progress_revision' = before_json->'revision',false))
);

CREATE FUNCTION guard_item_progress_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'item progress retains revision history'; END IF;
    IF TG_OP = 'INSERT' AND NEW.revision <> 1 THEN RAISE EXCEPTION 'item progress begins at revision one'; END IF;
    IF TG_OP = 'UPDATE' AND (NEW.workspace_id <> OLD.workspace_id OR NEW.item_id <> OLD.item_id
       OR NEW.revision <> OLD.revision+1 OR NEW.updated_at < OLD.updated_at)
    THEN RAISE EXCEPTION 'invalid item progress transition'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION verify_item_progress_operation() RETURNS trigger
LANGUAGE plpgsql AS $verify$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM item_progress_operations operation
        WHERE operation.workspace_id=NEW.workspace_id AND operation.item_id=NEW.item_id
          AND operation.progress_revision=NEW.revision
          AND operation.result_json->'components'=NEW.components
          AND operation.recorded_at=NEW.updated_at
          AND (operation.result_json->>'updated_at')::timestamptz=NEW.updated_at
          AND ((TG_OP='INSERT' AND operation.before_json->'components'='[]'::jsonb
                AND operation.before_json->'updated_at'='null'::jsonb)
            OR (TG_OP='UPDATE' AND operation.before_json->'components'=OLD.components
                AND (operation.before_json->>'updated_at')::timestamptz=OLD.updated_at)))
    THEN RAISE EXCEPTION 'item progress requires atomic operation custody'; END IF;
    RETURN NEW;
END
$verify$;

-- Reverse custody: a receipt cannot be manufactured without its exact current
-- sidecar and live canonical revision. The deferred sidecar guard checks OLD.
CREATE FUNCTION guard_item_progress_operation() RETURNS trigger
LANGUAGE plpgsql AS $operation$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM item_progress progress JOIN items item
        ON item.workspace_id=progress.workspace_id AND item.id=progress.item_id
        WHERE progress.workspace_id=NEW.workspace_id AND progress.item_id=NEW.item_id
          AND item.trashed_at IS NULL
          AND item.revision::text=NEW.request_json->>'expected_item_revision'
          AND progress.revision=NEW.progress_revision
          AND progress.components=NEW.result_json->'components'
          AND progress.updated_at=NEW.recorded_at
          AND progress.updated_at=(NEW.result_json->>'updated_at')::timestamptz)
    THEN RAISE EXCEPTION 'item progress receipt requires exact current authority'; END IF;
    RETURN NEW;
END
$operation$;

CREATE TRIGGER item_progress_guard BEFORE INSERT OR UPDATE OR DELETE ON item_progress
    FOR EACH ROW EXECUTE FUNCTION guard_item_progress_mutation();
CREATE CONSTRAINT TRIGGER item_progress_operation_guard AFTER INSERT OR UPDATE ON item_progress
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_item_progress_operation();
CREATE TRIGGER item_progress_operations_immutable BEFORE UPDATE OR DELETE ON item_progress_operations
    FOR EACH ROW EXECUTE FUNCTION reject_account_deletion_evidence_mutation();
CREATE TRIGGER item_progress_operations_guard BEFORE INSERT ON item_progress_operations
    FOR EACH ROW EXECUTE FUNCTION guard_item_progress_operation();
CREATE TRIGGER account_deletion_fence_guard BEFORE INSERT OR UPDATE OR DELETE ON item_progress
    FOR EACH ROW EXECUTE FUNCTION reject_fenced_workspace_mutation();
CREATE TRIGGER account_deletion_fence_guard BEFORE INSERT OR UPDATE OR DELETE ON item_progress_operations
    FOR EACH ROW EXECUTE FUNCTION reject_fenced_workspace_mutation();

DO $pin$
DECLARE function_name text; trusted_schema name := current_schema();
BEGIN
    FOREACH function_name IN ARRAY ARRAY['valid_item_progress_label(text,integer)',
        'valid_item_progress_decimal(text)','valid_item_progress_components(jsonb)',
        'valid_item_progress_snapshot(jsonb)','guard_item_progress_operation()',
        'guard_item_progress_mutation()','verify_item_progress_operation()'] LOOP
        EXECUTE format('ALTER FUNCTION %I.%s SET search_path TO %I, pg_catalog, pg_temp',trusted_schema,function_name,trusted_schema);
        EXECUTE format('REVOKE ALL ON FUNCTION %I.%s FROM PUBLIC',trusted_schema,function_name);
    END LOOP;
END
$pin$;

-- Preserve the existing guarded purge algorithm, extending only its tenant inventories.
CREATE OR REPLACE FUNCTION purge_fenced_personal_account_scope(
    requested_deletion_id uuid,
    requested_expected_revision bigint,
    requested_request_hash bytea
) RETURNS TABLE (result_revision bigint, replayed boolean)
LANGUAGE plpgsql
SECURITY INVOKER
AS $purge$
DECLARE
    lifecycle account_deletion_lifecycles%ROWTYPE;
    receipt account_deletion_transition_receipts%ROWTYPE;
    target_table text;
    operation_at timestamptz;
    tenant_schema name := current_schema();
    lock_tables constant text[] := ARRAY[
        'account_recovery_codes', 'audit_operations', 'device_enrollments',
        'execution_defer_assessments', 'execution_defer_replacement_claims',
        'execution_defer_replacement_consumptions', 'execution_physical_indices',
        'execution_session_schedule_origins', 'execution_sessions', 'execution_state',
        'google_calendar_projection_rejections', 'google_oauth_cleanup_tokens',
        'google_oauth_guardian_resolutions', 'google_oauth_legacy_credential_quarantine',
        'google_oauth_scope_state', 'google_oauth_sessions', 'google_outbound_previews',
        'google_provider_identity_roots', 'google_schedule_publication_batches',
        'google_schedule_publication_mapping_origins',
        'google_schedule_publication_observations', 'google_schedule_publication_outbox',
        'google_schedule_publication_preview_changes',
        'google_schedule_publication_previews', 'google_sync_collections',
        'google_sync_outbox', 'google_sync_refresh_requests', 'google_sync_runs',
        'habit_changes', 'habit_missed_resolution_versions', 'habit_missed_resolutions',
        'habit_occurrence_evidence', 'habit_occurrence_outcomes',
        'habit_occurrence_publications', 'habit_occurrence_versions',
        'habit_operation_receipts', 'habit_pause_versions', 'habit_pauses',
        'idempotency_keys', 'item_changes', 'item_dependencies', 'item_hierarchy', 'items',
        'item_progress', 'item_progress_operations',
        'mcp_clients', 'mcp_proposal_submissions', 'outbox_messages',
        'proposal_application_effects', 'proposal_application_fences',
        'proposal_application_members', 'proposal_application_requests',
        'proposal_applications', 'proposal_apply_preview_members', 'proposal_apply_previews',
        'proposals', 'provider_accounts', 'provider_sync_cursors', 'provider_sync_mappings',
        'schedule_blocks', 'schedule_defer_replacement_placements',
        'schedule_deferred_placements', 'schedule_publication_requests',
        'schedule_revision_details', 'schedule_revisions', 'schedule_simulations',
        'sessions', 'users', 'workspace_members', 'workspaces'
    ];
    delete_order constant text[] := ARRAY[
        'google_schedule_publication_observations',
        'schedule_defer_replacement_placements',
        'google_schedule_publication_outbox',
        'execution_physical_indices',
        'execution_defer_replacement_consumptions',
        'google_schedule_publication_preview_changes',
        'execution_defer_replacement_claims',
        'proposal_application_requests', 'proposal_application_members',
        'proposal_application_fences', 'proposal_application_effects',
        'habit_missed_resolution_versions',
        'google_schedule_publication_mapping_origins',
        'google_schedule_publication_batches',
        'google_schedule_publication_previews',
        'execution_defer_assessments', 'schedule_deferred_placements',
        'provider_sync_mappings', 'proposal_applications', 'habit_pause_versions',
        'habit_occurrence_versions', 'habit_occurrence_publications',
        'habit_occurrence_outcomes', 'habit_missed_resolutions',
        'google_sync_outbox', 'google_outbound_previews',
        'google_oauth_guardian_resolutions', 'google_oauth_cleanup_tokens',
        'google_calendar_projection_rejections', 'execution_state',
        'execution_session_schedule_origins', 'schedule_simulations',
        'schedule_revision_details', 'schedule_publication_requests', 'schedule_blocks',
        'provider_sync_cursors', 'proposal_apply_preview_members',
        'mcp_proposal_submissions', 'item_hierarchy', 'item_dependencies',
        'habit_pauses', 'habit_occurrence_evidence', 'google_sync_runs',
        'google_sync_refresh_requests', 'google_sync_collections', 'google_oauth_sessions',
        'google_oauth_legacy_credential_quarantine', 'execution_sessions',
        'device_enrollments', 'audit_operations', 'account_recovery_codes', 'sessions',
        'schedule_revisions', 'provider_accounts', 'proposals', 'proposal_apply_previews',
        'item_progress_operations', 'item_progress',
        'mcp_clients', 'items', 'google_provider_identity_roots',
        'google_oauth_scope_state', 'workspace_members', 'outbox_messages',
        'item_changes', 'idempotency_keys', 'habit_operation_receipts', 'habit_changes'
    ];
BEGIN
    IF requested_deletion_id IS NULL
       OR requested_expected_revision IS NULL
       OR requested_expected_revision <= 0
       OR requested_request_hash IS NULL
       OR octet_length(requested_request_hash) <> 32
    THEN
        RAISE EXCEPTION USING ERRCODE = 'DWREQ', MESSAGE = 'invalid purge request';
    END IF;

    SELECT * INTO receipt
      FROM account_deletion_transition_receipts
     WHERE deletion_id = requested_deletion_id
       AND request_hash = requested_request_hash;
    IF FOUND THEN
        IF receipt.from_status <> 'purge'
           OR receipt.to_status <> 'backup_wait'
           OR receipt.expected_revision <> requested_expected_revision
        THEN
            RAISE EXCEPTION USING ERRCODE = 'DWCON', MESSAGE = 'purge replay conflicts';
        END IF;
        result_revision := receipt.result_revision;
        replayed := true;
        RETURN NEXT;
        RETURN;
    END IF;

    SELECT * INTO lifecycle
      FROM account_deletion_lifecycles
     WHERE id = requested_deletion_id
     FOR UPDATE;
    IF NOT FOUND
       OR lifecycle.status <> 'purge'
       OR lifecycle.revision <> requested_expected_revision
    THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON', MESSAGE = 'purge state conflicts';
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.subject.v1:' || encode(lifecycle.owner_subject_hash, 'hex'), 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.user.v1:' || lifecycle.user_id::text, 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.workspace.v1:' || lifecycle.workspace_id::text, 0
    ));

    IF NOT EXISTS (
        SELECT 1 FROM account_deletion_fences
         WHERE deletion_id = lifecycle.id
           AND workspace_id = lifecycle.workspace_id
           AND user_id = lifecycle.user_id
           AND owner_subject_hash = lifecycle.owner_subject_hash
    ) OR NOT EXISTS (
        SELECT 1 FROM workspaces
         WHERE id = lifecycle.workspace_id AND owner_user_id = lifecycle.user_id
    ) OR (SELECT count(*) FROM workspaces WHERE owner_user_id = lifecycle.user_id) <> 1
      OR (SELECT count(*) FROM workspace_members
           WHERE workspace_id = lifecycle.workspace_id) <> 1
      OR (SELECT count(*) FROM workspace_members
           WHERE user_id = lifecycle.user_id) <> 1
      OR NOT EXISTS (
        SELECT 1 FROM workspace_members
         WHERE workspace_id = lifecycle.workspace_id
           AND user_id = lifecycle.user_id
           AND role = 'owner' AND removed_at IS NULL
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWSCP', MESSAGE = 'purge scope is not personal';
    END IF;
    operation_at := clock_timestamp();

    FOREACH target_table IN ARRAY lock_tables LOOP
        EXECUTE format(
            'LOCK TABLE %I.%I IN ACCESS EXCLUSIVE MODE', tenant_schema, target_table
        );
    END LOOP;
    SET CONSTRAINTS ALL DEFERRED;
    FOREACH target_table IN ARRAY lock_tables LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I DISABLE TRIGGER USER', tenant_schema, target_table
        );
    END LOOP;

    FOREACH target_table IN ARRAY delete_order LOOP
        EXECUTE format(
            'DELETE FROM %I.%I WHERE workspace_id = $1', tenant_schema, target_table
        )
        USING lifecycle.workspace_id;
    END LOOP;
    DELETE FROM workspaces WHERE id = lifecycle.workspace_id;
    DELETE FROM users WHERE id = lifecycle.user_id;

    -- Drain deferred FK events before changing trigger enablement again.
    -- Failure here rolls the whole purge and every DISABLE TRIGGER back.
    SET CONSTRAINTS ALL IMMEDIATE;
    FOREACH target_table IN ARRAY lock_tables LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I ENABLE TRIGGER USER', tenant_schema, target_table
        );
    END LOOP;

    UPDATE account_deletion_lifecycles
       SET status = 'backup_wait',
           revision = revision + 1,
           local_purge_completed_at = operation_at,
           backup_wait_at = operation_at,
           updated_at = operation_at
     WHERE id = lifecycle.id
       AND status = 'purge'
       AND revision = requested_expected_revision;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON', MESSAGE = 'purge state conflicts';
    END IF;

    INSERT INTO account_deletion_transition_receipts (
        deletion_id, request_hash, from_status, to_status,
        expected_revision, result_revision, occurred_at, failure_code
    ) VALUES (
        lifecycle.id, requested_request_hash, 'purge', 'backup_wait',
        requested_expected_revision, requested_expected_revision + 1, operation_at, NULL
    );

    result_revision := requested_expected_revision + 1;
    replayed := false;
    RETURN NEXT;
END
$purge$;

DO $pin_purge$ BEGIN
    EXECUTE format('ALTER FUNCTION %I.purge_fenced_personal_account_scope(uuid,bigint,bytea) SET search_path TO %I, pg_catalog, pg_temp', current_schema(),current_schema());
END $pin_purge$;
REVOKE ALL ON FUNCTION purge_fenced_personal_account_scope(uuid,bigint,bytea) FROM PUBLIC;
