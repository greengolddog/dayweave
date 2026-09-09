-- Immutable, short-lived current-state manifests. Payloads remain in the
-- canonical history; tickets never duplicate private item content.
ALTER TABLE item_changes
    ADD CONSTRAINT item_changes_workspace_sequence_unique UNIQUE(workspace_id,sequence);

CREATE TABLE item_bootstrap_snapshots (
    id uuid PRIMARY KEY CHECK(id <> '00000000-0000-0000-0000-000000000000'::uuid),
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL,
    head_sequence bigint NOT NULL CHECK(head_sequence >= 0),
    created_at timestamptz NOT NULL,
    cutoff_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    member_count integer NOT NULL CHECK(member_count BETWEEN 0 AND 20000),
    payload_bytes bigint NOT NULL CHECK(payload_bytes BETWEEN 0 AND 33554432),
    capture_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    UNIQUE(workspace_id,id),
    FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id),
    CHECK(isfinite(created_at) AND isfinite(cutoff_at) AND isfinite(expires_at)),
    CHECK(cutoff_at = created_at - interval '720 hours'),
    CHECK(expires_at = created_at + interval '10 minutes'),
    CHECK((member_count = 0) = (payload_bytes = 0))
);

CREATE INDEX item_bootstrap_snapshots_workspace_expiry_idx
    ON item_bootstrap_snapshots(workspace_id,expires_at,id);

CREATE TABLE item_bootstrap_members (
    snapshot_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    ordinal integer NOT NULL CHECK(ordinal BETWEEN 1 AND 20000),
    change_sequence bigint NOT NULL,
    PRIMARY KEY(snapshot_id,ordinal),
    UNIQUE(snapshot_id,change_sequence),
    FOREIGN KEY(workspace_id,snapshot_id)
        REFERENCES item_bootstrap_snapshots(workspace_id,id),
    FOREIGN KEY(workspace_id,change_sequence)
        REFERENCES item_changes(workspace_id,sequence)
);

CREATE INDEX item_bootstrap_members_source_idx
    ON item_bootstrap_members(change_sequence);

CREATE FUNCTION guard_item_bootstrap_snapshot() RETURNS trigger
LANGUAGE plpgsql AS $snapshot$
DECLARE
    operation_at timestamptz;
    current_head bigint;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'item bootstrap snapshot is immutable';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.items.v1:' || CASE WHEN TG_OP='DELETE' THEN OLD.workspace_id ELSE NEW.workspace_id END::text,0));
    operation_at := clock_timestamp();
    IF TG_OP = 'DELETE' THEN
        IF OLD.expires_at > operation_at OR EXISTS(
            SELECT 1 FROM item_bootstrap_members WHERE snapshot_id=OLD.id
        ) THEN RAISE EXCEPTION 'item bootstrap snapshot is still retained'; END IF;
        RETURN OLD;
    END IF;

    -- Held through capture/seal, including every member FK and payload read.
    -- The source-history statement guard takes its exclusive counterpart
    -- before source row locks, preventing mutable-payload pinning races.
    PERFORM pg_advisory_xact_lock_shared(hashtextextended(
        'dayweave.item-bootstrap.history-immutability.v1',0));
    IF NEW.capture_xid <> pg_current_xact_id()
       OR NEW.created_at < transaction_timestamp()
       OR NEW.created_at > operation_at
       OR NEW.expires_at <= operation_at
       OR NEW.expires_at <> NEW.created_at + interval '10 minutes'
       OR NEW.cutoff_at <> NEW.created_at - interval '720 hours'
    THEN RAISE EXCEPTION 'invalid item bootstrap capture lifetime'; END IF;
    IF NOT EXISTS(
        SELECT 1 FROM workspace_members member
        JOIN workspaces workspace ON workspace.id=member.workspace_id
        JOIN users owner ON owner.id=workspace.owner_user_id
        WHERE member.workspace_id=NEW.workspace_id AND member.user_id=NEW.user_id
          AND member.role='owner' AND member.removed_at IS NULL
          AND workspace.owner_user_id=NEW.user_id
          AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL
          AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL
    ) THEN RAISE EXCEPTION 'item bootstrap owner is unavailable'; END IF;
    SELECT coalesce(max(sequence),0) INTO current_head
      FROM item_changes WHERE workspace_id=NEW.workspace_id;
    IF NEW.head_sequence <> current_head THEN
        RAISE EXCEPTION 'item bootstrap head is not current';
    END IF;
    IF (SELECT count(*) FROM item_bootstrap_snapshots
         WHERE workspace_id=NEW.workspace_id AND expires_at>operation_at) >= 16
    THEN RAISE EXCEPTION 'item bootstrap snapshot capacity is exhausted'; END IF;
    RETURN NEW;
END
$snapshot$;

CREATE FUNCTION guard_item_bootstrap_member() RETURNS trigger
LANGUAGE plpgsql AS $member$
DECLARE
    snapshot item_bootstrap_snapshots%ROWTYPE;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'item bootstrap member is immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_advisory_xact_lock(hashtextextended(
            'dayweave.items.v1:' || OLD.workspace_id::text,0));
        -- Page reads hold FOR SHARE on this header until their payload read
        -- finishes. Expiry cannot remove a subset of an admitted page.
        SELECT * INTO snapshot FROM item_bootstrap_snapshots
          WHERE id=OLD.snapshot_id AND workspace_id=OLD.workspace_id FOR UPDATE;
        IF NOT FOUND OR snapshot.expires_at > clock_timestamp() THEN
            RAISE EXCEPTION 'item bootstrap member is still retained';
        END IF;
        RETURN OLD;
    END IF;
    SELECT * INTO snapshot FROM item_bootstrap_snapshots
      WHERE id=NEW.snapshot_id AND workspace_id=NEW.workspace_id;
    IF NOT FOUND OR snapshot.capture_xid <> pg_current_xact_id()
       OR snapshot.expires_at <= clock_timestamp()
       OR NEW.ordinal > snapshot.member_count
       OR NEW.change_sequence > snapshot.head_sequence
    THEN RAISE EXCEPTION 'item bootstrap member requires its original capture'; END IF;
    RETURN NEW;
END
$member$;

-- One deferred check per header, not a full scan for each of 20,000 members.
-- Its transaction ID fence prevents any later append to an already sealed
-- manifest. No mutable application flag or caller-controlled GUC is authority.
CREATE FUNCTION verify_item_bootstrap_capture() RETURNS trigger
LANGUAGE plpgsql AS $seal$
DECLARE
    actual_count bigint;
    actual_bytes bigint;
    expected_count bigint;
BEGIN
    IF NEW.expires_at <= clock_timestamp() THEN
        RAISE EXCEPTION 'item bootstrap expired before capture committed';
    END IF;
    SELECT count(*),coalesce(sum(octet_length(change.payload::text)),0)
      INTO actual_count,actual_bytes
      FROM item_bootstrap_members member
      JOIN item_changes change ON change.workspace_id=member.workspace_id
        AND change.sequence=member.change_sequence
      WHERE member.snapshot_id=NEW.id AND member.workspace_id=NEW.workspace_id;
    SELECT count(*) INTO expected_count FROM items
      WHERE workspace_id=NEW.workspace_id
        AND (trashed_at IS NULL OR trashed_at>=NEW.cutoff_at);
    IF actual_count<>NEW.member_count OR actual_bytes<>NEW.payload_bytes
       OR expected_count<>NEW.member_count
       OR EXISTS(
           SELECT 1 FROM (
               SELECT ordinal,row_number() OVER(ORDER BY change_sequence) AS expected_ordinal
                 FROM item_bootstrap_members WHERE snapshot_id=NEW.id
           ) ordered WHERE ordinal<>expected_ordinal
       )
       OR EXISTS(
           SELECT 1 FROM item_bootstrap_members member
           JOIN item_changes change ON change.workspace_id=member.workspace_id
             AND change.sequence=member.change_sequence
           LEFT JOIN items item ON item.workspace_id=change.workspace_id AND item.id=change.item_id
           WHERE member.snapshot_id=NEW.id AND (
               item.id IS NULL OR item.revision<>change.item_revision
               OR (item.trashed_at IS NOT NULL AND item.trashed_at<NEW.cutoff_at)
               OR (item.trashed_at IS NULL AND change.change_kind<>'upsert')
               OR (item.trashed_at IS NOT NULL AND change.change_kind<>'tombstone')
           )
       )
    THEN RAISE EXCEPTION 'item bootstrap capture is incomplete or inconsistent'; END IF;
    RETURN NEW;
END
$seal$;

CREATE FUNCTION lock_item_bootstrap_history_mutation() RETURNS trigger
LANGUAGE plpgsql AS $history$
BEGIN
    PERFORM pg_advisory_xact_lock_shared(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1',0));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.item-bootstrap.history-immutability.v1',0));
    IF TG_OP='TRUNCATE' AND EXISTS(SELECT 1 FROM item_bootstrap_members) THEN
        RAISE EXCEPTION 'item bootstrap still pins canonical history';
    END IF;
    RETURN NULL;
END
$history$;

CREATE FUNCTION reject_item_bootstrap_pinned_change() RETURNS trigger
LANGUAGE plpgsql AS $pinned$
BEGIN
    IF EXISTS(SELECT 1 FROM item_bootstrap_members WHERE change_sequence=OLD.sequence) THEN
        RAISE EXCEPTION 'item bootstrap still pins canonical history';
    END IF;
    RETURN CASE WHEN TG_OP='DELETE' THEN OLD ELSE NEW END;
END
$pinned$;

CREATE FUNCTION reject_item_bootstrap_truncate() RETURNS trigger
LANGUAGE plpgsql AS $truncate$
BEGIN
    RAISE EXCEPTION 'item bootstrap cleanup requires guarded expiry deletion';
END
$truncate$;

CREATE TRIGGER account_deletion_fence_guard
    BEFORE INSERT OR UPDATE OR DELETE ON item_bootstrap_snapshots
    FOR EACH ROW EXECUTE FUNCTION reject_fenced_workspace_mutation();
CREATE TRIGGER account_deletion_fence_guard
    BEFORE INSERT OR UPDATE OR DELETE ON item_bootstrap_members
    FOR EACH ROW EXECUTE FUNCTION reject_fenced_workspace_mutation();
CREATE TRIGGER item_bootstrap_snapshots_guard
    BEFORE INSERT OR UPDATE OR DELETE ON item_bootstrap_snapshots
    FOR EACH ROW EXECUTE FUNCTION guard_item_bootstrap_snapshot();
CREATE CONSTRAINT TRIGGER item_bootstrap_snapshots_complete
    AFTER INSERT ON item_bootstrap_snapshots DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION verify_item_bootstrap_capture();
CREATE TRIGGER item_bootstrap_members_guard
    BEFORE INSERT OR UPDATE OR DELETE ON item_bootstrap_members
    FOR EACH ROW EXECUTE FUNCTION guard_item_bootstrap_member();
CREATE TRIGGER item_bootstrap_snapshots_no_truncate
    BEFORE TRUNCATE ON item_bootstrap_snapshots
    FOR EACH STATEMENT EXECUTE FUNCTION reject_item_bootstrap_truncate();
CREATE TRIGGER item_bootstrap_members_no_truncate
    BEFORE TRUNCATE ON item_bootstrap_members
    FOR EACH STATEMENT EXECUTE FUNCTION reject_item_bootstrap_truncate();
CREATE TRIGGER item_bootstrap_history_mutation_lock
    BEFORE UPDATE OR DELETE OR TRUNCATE ON item_changes
    FOR EACH STATEMENT EXECUTE FUNCTION lock_item_bootstrap_history_mutation();
CREATE TRIGGER item_bootstrap_pinned_change_guard
    BEFORE UPDATE OR DELETE ON item_changes
    FOR EACH ROW EXECUTE FUNCTION reject_item_bootstrap_pinned_change();

DO $pin$
DECLARE function_name text; trusted_schema name := current_schema();
BEGIN
    FOREACH function_name IN ARRAY ARRAY[
        'guard_item_bootstrap_snapshot()','guard_item_bootstrap_member()',
        'verify_item_bootstrap_capture()','lock_item_bootstrap_history_mutation()',
        'reject_item_bootstrap_pinned_change()','reject_item_bootstrap_truncate()'
    ] LOOP
        EXECUTE format('ALTER FUNCTION %I.%s SET search_path TO %I, pg_catalog, pg_temp',
            trusted_schema,function_name,trusted_schema);
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
        'idempotency_keys', 'item_bootstrap_members', 'item_bootstrap_snapshots',
        'item_changes', 'item_dependencies', 'item_hierarchy', 'items',
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
        'item_bootstrap_members', 'item_bootstrap_snapshots',
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
