-- Fenced, asynchronous account-deletion foundation.
--
-- Lifecycle and fence rows intentionally have no foreign keys to tenant
-- identity/content rows: they must survive local purging and backup restores.
-- Every digest is opaque, fixed-width evidence; this schema retains no names,
-- tokens, provider payloads, item content, or free-form operator notes.

CREATE TABLE account_deletion_lifecycles (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    owner_subject_hash bytea NOT NULL,
    prepare_request_hash bytea NOT NULL,
    explicit_approval_digest bytea NOT NULL,
    principal_rate_limit_evidence_hash bytea NOT NULL,
    external_tombstone_evidence_hash bytea,
    authorizing_session_id uuid NOT NULL,
    authorizing_session_revision bigint NOT NULL,
    authorizing_credential_issued_at timestamptz NOT NULL,
    authorizing_recovery_code_id uuid NOT NULL,
    authorizing_recovery_code_revision bigint NOT NULL,
    authorizing_recovery_code_created_at timestamptz NOT NULL,
    confirming_session_id uuid,
    confirming_session_revision bigint,
    confirming_credential_issued_at timestamptz,
    confirming_approval_digest bytea,
    confirmed_at timestamptz,
    status varchar(32) NOT NULL DEFAULT 'prepared' CHECK (status IN (
        'prepared', 'fence_committing', 'fenced', 'provider_cleanup',
        'purge', 'backup_wait', 'complete', 'cancelled', 'failed'
    )),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    prepared_at timestamptz NOT NULL,
    fence_committing_at timestamptz,
    fenced_at timestamptz,
    provider_cleanup_at timestamptz,
    purge_at timestamptz,
    local_purge_completed_at timestamptz,
    backup_wait_at timestamptz,
    backup_erasure_evidence_hash bytea,
    completed_at timestamptz,
    cancelled_at timestamptz,
    failed_at timestamptz,
    failure_code varchar(64),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    CHECK (octet_length(owner_subject_hash) = 32),
    CHECK (octet_length(prepare_request_hash) = 32),
    CHECK (octet_length(explicit_approval_digest) = 32),
    CHECK (octet_length(principal_rate_limit_evidence_hash) = 32),
    CHECK (external_tombstone_evidence_hash IS NULL
        OR octet_length(external_tombstone_evidence_hash) = 32),
    CHECK (backup_erasure_evidence_hash IS NULL
        OR octet_length(backup_erasure_evidence_hash) = 32),
    CHECK (authorizing_session_revision > 0),
    CHECK (authorizing_recovery_code_revision > 0),
    CHECK (authorizing_credential_issued_at <= prepared_at),
    CHECK (authorizing_recovery_code_created_at <= prepared_at - interval '24 hours'),
    CHECK (confirming_session_revision IS NULL OR confirming_session_revision > 0),
    CHECK (confirming_approval_digest IS NULL
        OR octet_length(confirming_approval_digest) = 32),
    CHECK ((confirming_session_id IS NULL
        AND confirming_session_revision IS NULL
        AND confirming_credential_issued_at IS NULL
        AND confirming_approval_digest IS NULL
        AND confirmed_at IS NULL) OR (
        confirming_session_id IS NOT NULL
        AND confirming_session_revision IS NOT NULL
        AND confirming_credential_issued_at IS NOT NULL
        AND confirming_approval_digest IS NOT NULL
        AND confirmed_at IS NOT NULL)),
    CHECK (confirmed_at IS NULL OR confirmed_at >= prepared_at + interval '24 hours'),
    CHECK (confirming_credential_issued_at IS NULL
        OR confirming_credential_issued_at <= confirmed_at),
    CHECK (created_at = prepared_at),
    CHECK (updated_at >= created_at),
    CHECK (failure_code IS NULL OR failure_code IN (
        'external_gate_unavailable', 'provider_cleanup_exhausted',
        'backup_erasure_unverified', 'operator_intervention_required'
    )),
    CHECK ((status = 'cancelled') = (cancelled_at IS NOT NULL)),
    CHECK ((status = 'failed') = (failed_at IS NOT NULL AND failure_code IS NOT NULL)),
    CHECK ((status = 'complete') =
        (completed_at IS NOT NULL AND backup_erasure_evidence_hash IS NOT NULL)),
    CHECK (local_purge_completed_at IS NULL OR backup_wait_at IS NOT NULL),
    CHECK (completed_at IS NULL OR backup_wait_at IS NOT NULL)
);

CREATE UNIQUE INDEX account_deletion_lifecycles_workspace_active_uq
    ON account_deletion_lifecycles (workspace_id)
    WHERE status <> 'cancelled';

CREATE UNIQUE INDEX account_deletion_lifecycles_user_active_uq
    ON account_deletion_lifecycles (user_id)
    WHERE status <> 'cancelled';

CREATE UNIQUE INDEX account_deletion_lifecycles_subject_active_uq
    ON account_deletion_lifecycles (owner_subject_hash)
    WHERE status <> 'cancelled';

CREATE UNIQUE INDEX account_deletion_lifecycles_request_uq
    ON account_deletion_lifecycles (workspace_id, prepare_request_hash);

CREATE TABLE account_deletion_transition_receipts (
    deletion_id uuid NOT NULL REFERENCES account_deletion_lifecycles(id),
    request_hash bytea NOT NULL,
    from_status varchar(32) NOT NULL,
    to_status varchar(32) NOT NULL,
    expected_revision bigint NOT NULL CHECK (expected_revision > 0),
    result_revision bigint NOT NULL CHECK (result_revision = expected_revision + 1),
    occurred_at timestamptz NOT NULL,
    failure_code varchar(64),
    confirming_session_id uuid,
    confirming_session_revision bigint,
    confirming_approval_digest bytea,
    PRIMARY KEY (deletion_id, request_hash),
    UNIQUE (deletion_id, result_revision),
    CHECK (octet_length(request_hash) = 32),
    CHECK (from_status IN (
        'prepared', 'fence_committing', 'fenced', 'provider_cleanup',
        'purge', 'backup_wait', 'complete', 'cancelled', 'failed'
    )),
    CHECK (to_status IN (
        'prepared', 'fence_committing', 'fenced', 'provider_cleanup',
        'purge', 'backup_wait', 'complete', 'cancelled', 'failed'
    )),
    CHECK (failure_code IS NULL OR failure_code IN (
        'external_gate_unavailable', 'provider_cleanup_exhausted',
        'backup_erasure_unverified', 'operator_intervention_required'
    )),
    CHECK ((to_status = 'failed') = (failure_code IS NOT NULL))
    ,CHECK (confirming_session_revision IS NULL OR confirming_session_revision > 0)
    ,CHECK (confirming_approval_digest IS NULL
        OR octet_length(confirming_approval_digest) = 32)
    ,CHECK ((to_status = 'fence_committing') = (
        confirming_session_id IS NOT NULL
        AND confirming_session_revision IS NOT NULL
        AND confirming_approval_digest IS NOT NULL))
);

CREATE TABLE account_deletion_fences (
    deletion_id uuid PRIMARY KEY REFERENCES account_deletion_lifecycles(id),
    workspace_id uuid NOT NULL UNIQUE,
    user_id uuid NOT NULL UNIQUE,
    owner_subject_hash bytea NOT NULL UNIQUE,
    lifecycle_revision bigint NOT NULL CHECK (lifecycle_revision > 1),
    fenced_at timestamptz NOT NULL,
    CHECK (octet_length(owner_subject_hash) = 32)
);

CREATE FUNCTION guard_account_deletion_lifecycle_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'account deletion lifecycle evidence is immutable';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'prepared'
           OR NEW.revision <> 1
           OR NEW.created_at <> NEW.prepared_at
           OR NEW.updated_at <> NEW.prepared_at
           OR NEW.fence_committing_at IS NOT NULL
           OR NEW.fenced_at IS NOT NULL
           OR NEW.provider_cleanup_at IS NOT NULL
           OR NEW.purge_at IS NOT NULL
           OR NEW.local_purge_completed_at IS NOT NULL
           OR NEW.backup_wait_at IS NOT NULL
           OR NEW.external_tombstone_evidence_hash IS NOT NULL
           OR NEW.backup_erasure_evidence_hash IS NOT NULL
           OR NEW.completed_at IS NOT NULL
           OR NEW.cancelled_at IS NOT NULL
           OR NEW.failed_at IS NOT NULL
           OR NEW.failure_code IS NOT NULL
           OR NEW.confirming_session_id IS NOT NULL
           OR NEW.confirming_session_revision IS NOT NULL
           OR NEW.confirming_credential_issued_at IS NOT NULL
           OR NEW.confirming_approval_digest IS NOT NULL
           OR NEW.confirmed_at IS NOT NULL
        THEN
            RAISE EXCEPTION 'invalid initial account deletion lifecycle';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.owner_subject_hash IS DISTINCT FROM NEW.owner_subject_hash
       OR OLD.prepare_request_hash IS DISTINCT FROM NEW.prepare_request_hash
       OR OLD.explicit_approval_digest IS DISTINCT FROM NEW.explicit_approval_digest
       OR OLD.principal_rate_limit_evidence_hash
            IS DISTINCT FROM NEW.principal_rate_limit_evidence_hash
       OR OLD.authorizing_session_id IS DISTINCT FROM NEW.authorizing_session_id
       OR OLD.authorizing_session_revision IS DISTINCT FROM NEW.authorizing_session_revision
       OR OLD.authorizing_credential_issued_at
            IS DISTINCT FROM NEW.authorizing_credential_issued_at
       OR OLD.authorizing_recovery_code_id IS DISTINCT FROM NEW.authorizing_recovery_code_id
       OR OLD.authorizing_recovery_code_revision
            IS DISTINCT FROM NEW.authorizing_recovery_code_revision
       OR OLD.authorizing_recovery_code_created_at
            IS DISTINCT FROM NEW.authorizing_recovery_code_created_at
       OR OLD.prepared_at IS DISTINCT FROM NEW.prepared_at
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR NEW.revision <> OLD.revision + 1
       OR NEW.updated_at < OLD.updated_at
    THEN
        RAISE EXCEPTION 'account deletion lifecycle evidence is immutable';
    END IF;

    IF OLD.status = 'prepared' AND NEW.status = 'fence_committing' THEN
        IF OLD.fence_committing_at IS NOT NULL
           OR NEW.fence_committing_at IS NULL
           OR NEW.fence_committing_at <> NEW.updated_at
           OR OLD.confirming_session_id IS NOT NULL
           OR NEW.confirming_session_id IS NULL
           OR NEW.confirming_session_revision IS NULL
           OR NEW.confirming_credential_issued_at IS NULL
           OR NEW.confirming_approval_digest IS NULL
           OR NEW.confirmed_at IS NULL
           OR NEW.confirmed_at <> NEW.updated_at
        THEN
            RAISE EXCEPTION 'invalid account deletion fence transition';
        END IF;
    ELSIF OLD.status = 'fence_committing' AND NEW.status = 'fenced' THEN
        IF OLD.fenced_at IS NOT NULL
           OR NEW.fenced_at IS NULL
           OR NEW.fenced_at <> NEW.updated_at
           OR OLD.external_tombstone_evidence_hash IS NOT NULL
           OR NEW.external_tombstone_evidence_hash IS NULL
        THEN
            RAISE EXCEPTION 'invalid account deletion fenced transition';
        END IF;
    ELSIF OLD.status = 'fenced' AND NEW.status = 'provider_cleanup' THEN
        IF OLD.provider_cleanup_at IS NOT NULL
           OR NEW.provider_cleanup_at IS NULL
           OR NEW.provider_cleanup_at <> NEW.updated_at
        THEN
            RAISE EXCEPTION 'invalid account deletion provider cleanup transition';
        END IF;
    ELSIF OLD.status = 'provider_cleanup' AND NEW.status = 'purge' THEN
        IF OLD.purge_at IS NOT NULL OR NEW.purge_at IS NULL OR NEW.purge_at <> NEW.updated_at THEN
            RAISE EXCEPTION 'invalid account deletion purge transition';
        END IF;
    ELSIF OLD.status = 'purge' AND NEW.status = 'backup_wait' THEN
        IF OLD.local_purge_completed_at IS NOT NULL
           OR OLD.backup_wait_at IS NOT NULL
           OR NEW.local_purge_completed_at IS NULL
           OR NEW.backup_wait_at IS NULL
           OR NEW.local_purge_completed_at <> NEW.updated_at
           OR NEW.backup_wait_at <> NEW.updated_at
        THEN
            RAISE EXCEPTION 'invalid account deletion local purge transition';
        END IF;
    ELSIF OLD.status = 'backup_wait' AND NEW.status = 'complete' THEN
        IF OLD.completed_at IS NOT NULL
           OR OLD.backup_erasure_evidence_hash IS NOT NULL
           OR NEW.completed_at IS NULL
           OR NEW.completed_at <> NEW.updated_at
           OR NEW.backup_erasure_evidence_hash IS NULL
        THEN
            RAISE EXCEPTION 'account deletion completion requires backup erasure evidence';
        END IF;
    ELSIF OLD.status = 'prepared' AND NEW.status = 'cancelled' THEN
        IF OLD.cancelled_at IS NOT NULL
           OR NEW.cancelled_at IS NULL
           OR NEW.cancelled_at <> NEW.updated_at
        THEN
            RAISE EXCEPTION 'invalid account deletion cancellation';
        END IF;
    ELSE
        RAISE EXCEPTION 'invalid account deletion lifecycle transition';
    END IF;

    -- A transition may set only its own phase evidence. Base evidence is
    -- checked above; all unrelated phase evidence must remain byte-for-byte.
    IF (NEW.status <> 'fence_committing'
            AND (OLD.fence_committing_at IS DISTINCT FROM NEW.fence_committing_at
                OR OLD.confirming_session_id IS DISTINCT FROM NEW.confirming_session_id
                OR OLD.confirming_session_revision
                    IS DISTINCT FROM NEW.confirming_session_revision
                OR OLD.confirming_credential_issued_at
                    IS DISTINCT FROM NEW.confirming_credential_issued_at
                OR OLD.confirming_approval_digest
                    IS DISTINCT FROM NEW.confirming_approval_digest
                OR OLD.confirmed_at IS DISTINCT FROM NEW.confirmed_at))
       OR (NEW.status <> 'fenced' AND OLD.fenced_at IS DISTINCT FROM NEW.fenced_at)
       OR (NEW.status <> 'fenced' AND OLD.external_tombstone_evidence_hash
            IS DISTINCT FROM NEW.external_tombstone_evidence_hash)
       OR (NEW.status <> 'provider_cleanup'
            AND OLD.provider_cleanup_at IS DISTINCT FROM NEW.provider_cleanup_at)
       OR (NEW.status <> 'purge' AND OLD.purge_at IS DISTINCT FROM NEW.purge_at)
       OR (NEW.status <> 'backup_wait' AND (
            OLD.local_purge_completed_at IS DISTINCT FROM NEW.local_purge_completed_at
            OR OLD.backup_wait_at IS DISTINCT FROM NEW.backup_wait_at))
       OR (NEW.status <> 'complete' AND (
            OLD.backup_erasure_evidence_hash
                IS DISTINCT FROM NEW.backup_erasure_evidence_hash
            OR OLD.completed_at IS DISTINCT FROM NEW.completed_at))
       OR (NEW.status <> 'cancelled' AND OLD.cancelled_at IS DISTINCT FROM NEW.cancelled_at)
       OR (NEW.status <> 'failed' AND (
            OLD.failed_at IS DISTINCT FROM NEW.failed_at
            OR OLD.failure_code IS DISTINCT FROM NEW.failure_code))
    THEN
        RAISE EXCEPTION 'unrelated account deletion evidence changed';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER account_deletion_lifecycles_guard
    BEFORE INSERT OR UPDATE OR DELETE ON account_deletion_lifecycles
    FOR EACH ROW EXECUTE FUNCTION guard_account_deletion_lifecycle_mutation();

CREATE FUNCTION reject_account_deletion_evidence_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    RAISE EXCEPTION 'account deletion evidence is immutable';
END
$guard$;

CREATE TRIGGER account_deletion_transition_receipts_immutable
    BEFORE UPDATE OR DELETE ON account_deletion_transition_receipts
    FOR EACH ROW EXECUTE FUNCTION reject_account_deletion_evidence_mutation();

CREATE TRIGGER account_deletion_fences_immutable
    BEFORE UPDATE OR DELETE ON account_deletion_fences
    FOR EACH ROW EXECUTE FUNCTION reject_account_deletion_evidence_mutation();

CREATE FUNCTION validate_account_deletion_fence() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    -- The trigger, rather than only the current repository caller, owns the
    -- race-closing lock contract so future/direct fence installers cannot let
    -- a mutation that already passed its guard commit after this fence.
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.subject.v1:' || encode(NEW.owner_subject_hash, 'hex'), 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.user.v1:' || NEW.user_id::text, 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.workspace.v1:' || NEW.workspace_id::text, 0
    ));
    IF NOT EXISTS (
        SELECT 1
          FROM account_deletion_lifecycles AS lifecycle
         WHERE lifecycle.id = NEW.deletion_id
           AND lifecycle.workspace_id = NEW.workspace_id
           AND lifecycle.user_id = NEW.user_id
           AND lifecycle.owner_subject_hash = NEW.owner_subject_hash
           AND lifecycle.status = 'fence_committing'
           AND lifecycle.revision = NEW.lifecycle_revision
           AND lifecycle.fence_committing_at = NEW.fenced_at
    ) OR NOT EXISTS (
        SELECT 1 FROM users
         WHERE id = NEW.user_id
           AND sha256(convert_to(auth_subject, 'UTF8')) = NEW.owner_subject_hash
    ) OR NOT EXISTS (
        SELECT 1 FROM workspaces
         WHERE id = NEW.workspace_id AND owner_user_id = NEW.user_id
    ) OR (SELECT count(*) FROM workspaces WHERE owner_user_id = NEW.user_id) <> 1
      OR (SELECT count(*) FROM workspace_members
           WHERE workspace_id = NEW.workspace_id) <> 1
      OR (SELECT count(*) FROM workspace_members WHERE user_id = NEW.user_id) <> 1
      OR NOT EXISTS (
        SELECT 1 FROM workspace_members
         WHERE workspace_id = NEW.workspace_id
           AND user_id = NEW.user_id
           AND role = 'owner' AND removed_at IS NULL
    ) THEN
        RAISE EXCEPTION 'account deletion fence is not bound to its lifecycle';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER account_deletion_fences_validate
    BEFORE INSERT ON account_deletion_fences
    FOR EACH ROW EXECUTE FUNCTION validate_account_deletion_fence();

-- The fence is a database-level write barrier, not merely an HTTP policy.
-- Mutations take shared transaction-scoped advisory locks; fence installation
-- takes the matching exclusive locks. This closes the pre-commit race without
-- introducing lock-order cycles between existing domain mutation mutexes.
CREATE FUNCTION reject_fenced_workspace_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    affected_workspace uuid;
    affected_user uuid;
    affected_workspaces uuid[];
    affected_users uuid[];
    old_row jsonb := '{}'::jsonb;
    new_row jsonb := '{}'::jsonb;
BEGIN
    -- This global shared barrier is always the first deletion lock acquired
    -- by a mutation. Scope locks may therefore be discovered row by row or in
    -- later statements without deadlocking a pending exclusive fence.
    PERFORM pg_advisory_xact_lock_shared(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    IF TG_OP <> 'INSERT' THEN
        old_row := to_jsonb(OLD);
    END IF;
    IF TG_OP <> 'DELETE' THEN
        new_row := to_jsonb(NEW);
    END IF;

    SELECT coalesce(array_agg(candidate ORDER BY candidate), ARRAY[]::uuid[])
      INTO affected_users
      FROM (
        SELECT DISTINCT value::uuid AS candidate
          FROM (
            SELECT key, value FROM jsonb_each_text(old_row)
            UNION ALL
            SELECT key, value FROM jsonb_each_text(new_row)
          ) AS fields
         WHERE value IS NOT NULL
           AND (key = 'user_id' OR right(key, 8) = '_user_id')
      ) AS identities;
    SELECT coalesce(array_agg(candidate ORDER BY candidate), ARRAY[]::uuid[])
      INTO affected_workspaces
      FROM (
        SELECT DISTINCT value::uuid AS candidate
          FROM (
            SELECT key, value FROM jsonb_each_text(old_row)
            UNION ALL
            SELECT key, value FROM jsonb_each_text(new_row)
          ) AS fields
         WHERE value IS NOT NULL AND key = 'workspace_id'
      ) AS identities;

    -- Match lifecycle lock ordering: principal, then workspace. Inspecting
    -- both OLD and NEW closes cross-scope reassignment and membership bypasses.
    FOREACH affected_user IN ARRAY affected_users
    LOOP
        PERFORM pg_advisory_xact_lock_shared(hashtextextended(
            'dayweave.account-deletion.user.v1:' || affected_user::text, 0
        ));
    END LOOP;
    FOREACH affected_workspace IN ARRAY affected_workspaces
    LOOP
        PERFORM pg_advisory_xact_lock_shared(hashtextextended(
            'dayweave.account-deletion.workspace.v1:' || affected_workspace::text, 0
        ));
    END LOOP;
    IF EXISTS (
        SELECT 1 FROM account_deletion_fences
         WHERE workspace_id = ANY(affected_workspaces)
            OR user_id = ANY(affected_users)
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWDEL',
            MESSAGE = 'account deletion fence is active';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE FUNCTION reject_fenced_user_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    affected_user uuid;
    affected_subject_hash bytea;
    old_user uuid;
    new_user uuid;
    old_subject_hash bytea;
    new_subject_hash bytea;
BEGIN
    PERFORM pg_advisory_xact_lock_shared(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    IF TG_OP <> 'INSERT' THEN
        old_user := OLD.id;
        old_subject_hash := sha256(convert_to(OLD.auth_subject, 'UTF8'));
    END IF;
    IF TG_OP <> 'DELETE' THEN
        new_user := NEW.id;
        new_subject_hash := sha256(convert_to(NEW.auth_subject, 'UTF8'));
    END IF;
    FOR affected_subject_hash IN
        SELECT DISTINCT candidate
          FROM unnest(ARRAY[old_subject_hash, new_subject_hash]) AS candidate
         WHERE candidate IS NOT NULL
         ORDER BY candidate
    LOOP
        PERFORM pg_advisory_xact_lock_shared(hashtextextended(
            'dayweave.account-deletion.subject.v1:' || encode(affected_subject_hash, 'hex'), 0
        ));
    END LOOP;
    FOR affected_user IN
        SELECT DISTINCT candidate
          FROM unnest(ARRAY[old_user, new_user]) AS candidate
         WHERE candidate IS NOT NULL
         ORDER BY candidate
    LOOP
        PERFORM pg_advisory_xact_lock_shared(hashtextextended(
            'dayweave.account-deletion.user.v1:' || affected_user::text, 0
        ));
    END LOOP;
    IF EXISTS (
        SELECT 1 FROM account_deletion_fences
         WHERE user_id = old_user OR user_id = new_user
            OR owner_subject_hash = old_subject_hash
            OR owner_subject_hash = new_subject_hash
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWDEL',
            MESSAGE = 'account deletion fence is active';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE FUNCTION reject_fenced_workspace_root_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    affected_workspace uuid;
    affected_user uuid;
    old_workspace uuid;
    new_workspace uuid;
    old_owner_user uuid;
    new_owner_user uuid;
BEGIN
    PERFORM pg_advisory_xact_lock_shared(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    IF TG_OP <> 'INSERT' THEN
        old_workspace := OLD.id;
        old_owner_user := OLD.owner_user_id;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        new_workspace := NEW.id;
        new_owner_user := NEW.owner_user_id;
    END IF;
    FOR affected_user IN
        SELECT DISTINCT candidate
          FROM unnest(ARRAY[old_owner_user, new_owner_user]) AS candidate
         WHERE candidate IS NOT NULL
         ORDER BY candidate
    LOOP
        PERFORM pg_advisory_xact_lock_shared(hashtextextended(
            'dayweave.account-deletion.user.v1:' || affected_user::text, 0
        ));
    END LOOP;
    FOR affected_workspace IN
        SELECT DISTINCT candidate
          FROM unnest(ARRAY[old_workspace, new_workspace]) AS candidate
         WHERE candidate IS NOT NULL
         ORDER BY candidate
    LOOP
        PERFORM pg_advisory_xact_lock_shared(hashtextextended(
            'dayweave.account-deletion.workspace.v1:' || affected_workspace::text, 0
        ));
    END LOOP;
    IF EXISTS (
        SELECT 1 FROM account_deletion_fences
         WHERE workspace_id = old_workspace OR workspace_id = new_workspace
            OR user_id = old_owner_user OR user_id = new_owner_user
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = 'DWDEL',
            MESSAGE = 'account deletion fence is active';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE TRIGGER account_deletion_fence_guard
    BEFORE INSERT OR UPDATE OR DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION reject_fenced_user_mutation();

CREATE TRIGGER account_deletion_fence_guard
    BEFORE INSERT OR UPDATE OR DELETE ON workspaces
    FOR EACH ROW EXECUTE FUNCTION reject_fenced_workspace_root_mutation();

DO $install_fence_guards$
DECLARE
    target_table text;
BEGIN
    FOREACH target_table IN ARRAY ARRAY[
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
        'mcp_clients', 'mcp_proposal_submissions', 'outbox_messages',
        'proposal_application_effects', 'proposal_application_fences',
        'proposal_application_members', 'proposal_application_requests',
        'proposal_applications', 'proposal_apply_preview_members', 'proposal_apply_previews',
        'proposals', 'provider_accounts', 'provider_sync_cursors', 'provider_sync_mappings',
        'schedule_blocks', 'schedule_defer_replacement_placements',
        'schedule_deferred_placements', 'schedule_publication_requests',
        'schedule_revision_details', 'schedule_revisions', 'schedule_simulations',
        'sessions', 'workspace_members'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER account_deletion_fence_guard '
            'BEFORE INSERT OR UPDATE OR DELETE ON %I '
            'FOR EACH ROW EXECUTE FUNCTION reject_fenced_workspace_mutation()',
            target_table
        );
    END LOOP;
END
$install_fence_guards$;

-- These two edges form the only cross-table FK cycle in the current schema.
-- Deferring both preserves the evidence relationship during normal writes and
-- permits their two rows to be removed in one verified purge transaction.
ALTER TABLE google_sync_outbox
    DROP CONSTRAINT google_sync_outbox_approval_fk,
    ADD CONSTRAINT google_sync_outbox_approval_fk
        FOREIGN KEY (workspace_id, approval_id)
        REFERENCES google_outbound_previews(workspace_id, id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE google_sync_outbox
    ADD CONSTRAINT google_sync_outbox_workspace_id_uq UNIQUE (workspace_id, id);

ALTER TABLE google_outbound_previews
    DROP CONSTRAINT google_outbound_previews_outbox_fk,
    ADD CONSTRAINT google_outbound_previews_outbox_fk
        FOREIGN KEY (workspace_id, outbox_id)
        REFERENCES google_sync_outbox(workspace_id, id)
        DEFERRABLE INITIALLY DEFERRED;

-- Removes only the already-fenced personal scope. All table locks are acquired
-- before user triggers are transactionally disabled, so concurrent operations
-- cannot observe a partially disabled guard set. Foreign-key triggers remain
-- enabled; the explicit child-to-parent order is therefore executable proof of
-- the current schema dependency graph. Any error rolls back deletes and DDL.
CREATE FUNCTION purge_fenced_personal_account_scope(
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

-- Capture the migration's trusted schema explicitly and put pg_temp last.
-- The current application role also owns/migrates the tables, so an invoker
-- function adds no privilege; splitting runtime/migration roles is required
-- before granting this narrow primitive to a separate deployment role.
DO $pin_deletion_function_search_paths$
DECLARE
    trusted_schema name := current_schema();
BEGIN
    EXECUTE format(
        'ALTER FUNCTION %I.guard_account_deletion_lifecycle_mutation() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'ALTER FUNCTION %I.reject_account_deletion_evidence_mutation() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'ALTER FUNCTION %I.validate_account_deletion_fence() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'ALTER FUNCTION %I.reject_fenced_workspace_mutation() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'ALTER FUNCTION %I.reject_fenced_user_mutation() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'ALTER FUNCTION %I.reject_fenced_workspace_root_mutation() '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
    EXECUTE format(
        'ALTER FUNCTION %I.purge_fenced_personal_account_scope(uuid, bigint, bytea) '
        'SET search_path TO %I, pg_catalog, pg_temp',
        trusted_schema,
        trusted_schema
    );
END
$pin_deletion_function_search_paths$;

REVOKE ALL ON FUNCTION guard_account_deletion_lifecycle_mutation() FROM PUBLIC;
REVOKE ALL ON FUNCTION reject_account_deletion_evidence_mutation() FROM PUBLIC;
REVOKE ALL ON FUNCTION validate_account_deletion_fence() FROM PUBLIC;
REVOKE ALL ON FUNCTION reject_fenced_workspace_mutation() FROM PUBLIC;
REVOKE ALL ON FUNCTION reject_fenced_user_mutation() FROM PUBLIC;
REVOKE ALL ON FUNCTION reject_fenced_workspace_root_mutation() FROM PUBLIC;

REVOKE ALL ON FUNCTION purge_fenced_personal_account_scope(
    uuid, bigint, bytea
) FROM PUBLIC;
