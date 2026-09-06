-- Deployment-wide admission tracks unfinished provider operations by opaque
-- runtime/operation UUIDs. Rows have no tenant FK, provider identity, payload,
-- secret, expiry, or automatic reaping. An unresolved row survives a crashed
-- runtime and is retained until explicit settlement; elapsed time is not proof
-- that an outbound operation has stopped. Scope closure remains after purge.

CREATE TABLE provider_admission_scopes (
    workspace_id uuid NOT NULL
        CHECK (workspace_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    user_id uuid NOT NULL
        CHECK (user_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    closed_for_deletion_id uuid REFERENCES account_deletion_lifecycles(id),
    closed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (workspace_id, user_id),
    CHECK ((closed_for_deletion_id IS NULL AND closed_at IS NULL)
        OR (closed_for_deletion_id IS NOT NULL AND closed_at IS NOT NULL
            AND closed_at >= created_at))
);

CREATE TABLE provider_admission_operations (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    runtime_id uuid NOT NULL
        CHECK (runtime_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    operation_id uuid PRIMARY KEY
        CHECK (operation_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    registered_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES provider_admission_scopes(workspace_id, user_id)
);

CREATE INDEX provider_admission_operations_scope_idx
    ON provider_admission_operations(workspace_id, user_id, runtime_id);

-- Statement-level acquisition precedes target-row locks even for direct SQL
-- writers. Fence installation takes the exclusive mode of this same barrier.
CREATE FUNCTION lock_provider_admission_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING ERRCODE = 'DWREQ',
            MESSAGE = 'provider admission requires read committed';
    END IF;
    PERFORM pg_advisory_xact_lock_shared(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    -- Account fences cover every row sharing either the user or workspace.
    -- A short registry-wide mutex serializes closure against registration even
    -- when an ownership change produced a different exact scope pair.
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.provider-admission.global-registry.v1', 0
    ));
    RETURN NULL;
END
$guard$;

CREATE TRIGGER provider_admission_scopes_mutation_barrier
    BEFORE INSERT OR UPDATE OR DELETE ON provider_admission_scopes
    FOR EACH STATEMENT EXECUTE FUNCTION lock_provider_admission_mutation();
CREATE TRIGGER provider_admission_operations_mutation_barrier
    BEFORE INSERT OR UPDATE OR DELETE ON provider_admission_operations
    FOR EACH STATEMENT EXECUTE FUNCTION lock_provider_admission_mutation();

CREATE FUNCTION guard_provider_admission_scope() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON',
            MESSAGE = 'provider admission scope is retained';
    END IF;
    -- A direct UPDATE already holds this row while the adapter acquires scope
    -- ownership first. Never wait with that reverse row order: retry the short
    -- transaction instead of deadlocking an adapter that owns the scope.
    IF NOT pg_try_advisory_xact_lock(hashtextextended(
        'dayweave.provider-admission.scope.v1:' || NEW.workspace_id::text
            || ':' || NEW.user_id::text, 0
    )) THEN
        RAISE EXCEPTION USING ERRCODE = '40001',
            MESSAGE = 'provider admission scope mutation is contended';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.closed_for_deletion_id IS NOT NULL OR NEW.closed_at IS NOT NULL THEN
            RAISE EXCEPTION USING ERRCODE = 'DWCON',
                MESSAGE = 'provider admission scope must start open';
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM workspaces AS workspace
            JOIN users AS owner ON owner.id = workspace.owner_user_id
            JOIN workspace_members AS member
                ON member.workspace_id = workspace.id AND member.user_id = owner.id
            WHERE workspace.id = NEW.workspace_id AND owner.id = NEW.user_id
                AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL
                AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL
                AND member.role = 'owner' AND member.removed_at IS NULL
        ) THEN
            RAISE EXCEPTION USING ERRCODE = 'DWSCP',
                MESSAGE = 'provider admission scope is not a current owner';
        END IF;
        NEW.created_at := clock_timestamp();
        RETURN NEW;
    END IF;

    IF OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR OLD.closed_for_deletion_id IS NOT NULL
       OR NEW.closed_for_deletion_id IS NULL
    THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON',
            MESSAGE = 'provider admission closure is immutable';
    END IF;
    -- Do not add an explicit lifecycle row lock after the shared global
    -- barrier. The FK below takes only KEY SHARE, compatible with the fence
    -- repository's NO KEY UPDATE lock before its exclusive barrier.
    IF NOT EXISTS (
        SELECT 1 FROM account_deletion_lifecycles
        WHERE id = NEW.closed_for_deletion_id
            AND workspace_id = NEW.workspace_id AND user_id = NEW.user_id
            AND status <> 'cancelled'
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON',
            MESSAGE = 'provider admission closure does not match its deletion';
    END IF;
    IF EXISTS (
        SELECT 1 FROM provider_admission_scopes
        WHERE (workspace_id = NEW.workspace_id OR user_id = NEW.user_id)
            AND closed_for_deletion_id IS NOT NULL
            AND closed_for_deletion_id <> NEW.closed_for_deletion_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON',
            MESSAGE = 'provider admission closure overlaps another deletion';
    END IF;
    NEW.closed_at := clock_timestamp();
    RETURN NEW;
END
$guard$;

CREATE TRIGGER provider_admission_scopes_guard
    BEFORE INSERT OR UPDATE OR DELETE ON provider_admission_scopes
    FOR EACH ROW EXECUTE FUNCTION guard_provider_admission_scope();

CREATE FUNCTION guard_provider_admission_operation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    affected_workspace uuid;
    affected_user uuid;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION USING ERRCODE = 'DWOPR',
            MESSAGE = 'provider admission operation ownership is immutable';
    END IF;
    affected_workspace := CASE WHEN TG_OP = 'DELETE' THEN OLD.workspace_id ELSE NEW.workspace_id END;
    affected_user := CASE WHEN TG_OP = 'DELETE' THEN OLD.user_id ELSE NEW.user_id END;
    IF NOT pg_try_advisory_xact_lock(hashtextextended(
        'dayweave.provider-admission.scope.v1:' || affected_workspace::text
            || ':' || affected_user::text, 0
    )) THEN
        RAISE EXCEPTION USING ERRCODE = '40001',
            MESSAGE = 'provider admission operation mutation is contended';
    END IF;
    IF TG_OP = 'DELETE' THEN
        -- Explicit exact-owner settlement remains possible after closure.
        -- There is deliberately no age/lease-based settlement path.
        RETURN OLD;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM provider_admission_scopes
        WHERE workspace_id = NEW.workspace_id AND user_id = NEW.user_id
            AND closed_for_deletion_id IS NULL
    ) OR EXISTS (
        SELECT 1 FROM account_deletion_fences
        WHERE workspace_id = NEW.workspace_id OR user_id = NEW.user_id
    ) OR EXISTS (
        SELECT 1 FROM provider_admission_scopes
        WHERE (workspace_id = NEW.workspace_id OR user_id = NEW.user_id)
            AND closed_for_deletion_id IS NOT NULL
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWADM',
            MESSAGE = 'provider operation admission is closed';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM workspaces AS workspace
        JOIN users AS owner ON owner.id = workspace.owner_user_id
        JOIN workspace_members AS member
            ON member.workspace_id = workspace.id AND member.user_id = owner.id
        WHERE workspace.id = NEW.workspace_id AND owner.id = NEW.user_id
            AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL
            AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL
            AND member.role = 'owner' AND member.removed_at IS NULL
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWSCP',
            MESSAGE = 'provider admission scope is not a current owner';
    END IF;
    NEW.registered_at := clock_timestamp();
    RETURN NEW;
END
$guard$;

CREATE TRIGGER provider_admission_operations_guard
    BEFORE INSERT OR UPDATE OR DELETE ON provider_admission_operations
    FOR EACH ROW EXECUTE FUNCTION guard_provider_admission_operation();

CREATE FUNCTION reject_provider_admission_truncation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    RAISE EXCEPTION USING ERRCODE = 'DWOPR',
        MESSAGE = 'provider admission requires explicit operation settlement';
END
$guard$;

CREATE TRIGGER provider_admission_scopes_no_truncate
    BEFORE TRUNCATE ON provider_admission_scopes
    FOR EACH STATEMENT EXECUTE FUNCTION reject_provider_admission_truncation();
CREATE TRIGGER provider_admission_operations_no_truncate
    BEFORE TRUNCATE ON provider_admission_operations
    FOR EACH STATEMENT EXECUTE FUNCTION reject_provider_admission_truncation();

CREATE FUNCTION require_provider_admission_drained_for_fence() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING ERRCODE = 'DWREQ',
            MESSAGE = 'provider admission fence requires read committed';
    END IF;
    -- This guard runs before the older fence validator and therefore acquires
    -- the exclusive barrier itself. Registry checks take fresh snapshots after
    -- all admitted registration/settlement transactions have committed.
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    IF NOT EXISTS (
        SELECT 1 FROM provider_admission_scopes AS admission
        JOIN account_deletion_lifecycles AS lifecycle
            ON lifecycle.id = admission.closed_for_deletion_id
        WHERE admission.workspace_id = NEW.workspace_id
            AND admission.user_id = NEW.user_id
            AND admission.closed_for_deletion_id = NEW.deletion_id
            AND lifecycle.workspace_id = NEW.workspace_id
            AND lifecycle.user_id = NEW.user_id AND lifecycle.status <> 'cancelled'
    ) OR EXISTS (
        SELECT 1 FROM provider_admission_operations
        WHERE workspace_id = NEW.workspace_id OR user_id = NEW.user_id
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON',
            MESSAGE = 'provider admission is not closed and drained for this deletion';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER account_deletion_fences_provider_admission_validate
    BEFORE INSERT ON account_deletion_fences
    FOR EACH ROW EXECUTE FUNCTION require_provider_admission_drained_for_fence();

DO $pin_provider_admission_guards$
DECLARE
    trusted_schema name := current_schema();
    function_name name;
BEGIN
    FOREACH function_name IN ARRAY ARRAY[
        'lock_provider_admission_mutation',
        'guard_provider_admission_scope',
        'guard_provider_admission_operation',
        'reject_provider_admission_truncation',
        'require_provider_admission_drained_for_fence'
    ]::name[]
    LOOP
        EXECUTE format(
            'ALTER FUNCTION %I.%I() SET search_path TO %I, pg_catalog, pg_temp',
            trusted_schema, function_name, trusted_schema
        );
        EXECUTE format('REVOKE ALL ON FUNCTION %I.%I() FROM PUBLIC',
            trusted_schema, function_name);
    END LOOP;
END
$pin_provider_admission_guards$;
