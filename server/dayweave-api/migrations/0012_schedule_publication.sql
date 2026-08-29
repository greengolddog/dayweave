-- Durable, immutable canonical schedule publication and bounded simulation
-- capabilities. Publication receipts are retained with their revision so an
-- offline client can replay an old idempotency key without publishing again.

ALTER TABLE schedule_blocks DROP CONSTRAINT schedule_blocks_block_kind_check;
ALTER TABLE schedule_blocks
    ADD CONSTRAINT schedule_blocks_block_kind_check CHECK (block_kind IN (
        'item', 'calendar_event', 'break', 'buffer', 'focus', 'unavailable',
        'planned', 'pinned', 'external_fixed'
    ));

-- The scheduler's stable block id may recur in more than one immutable
-- revision, while the legacy table used it as a globally unique row id.
ALTER TABLE schedule_blocks ADD COLUMN source_block_id uuid;
UPDATE schedule_blocks SET source_block_id = id;
ALTER TABLE schedule_blocks ALTER COLUMN source_block_id SET NOT NULL;
ALTER TABLE schedule_blocks
    ADD CONSTRAINT schedule_blocks_revision_source_uq
        UNIQUE (workspace_id, schedule_revision_id, source_block_id);

ALTER TABLE schedule_revisions
    ADD COLUMN publication_hash bytea,
    ADD CONSTRAINT schedule_revisions_publication_hash_check
        CHECK (publication_hash IS NULL OR octet_length(publication_hash) = 32);

CREATE TABLE schedule_revision_details (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    result_snapshot jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, user_id, schedule_revision_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    CHECK (jsonb_typeof(result_snapshot) = 'object'),
    CHECK (octet_length(result_snapshot::text) <= 16777216)
);

CREATE TABLE schedule_publication_requests (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    idempotency_key uuid NOT NULL,
    request_hash bytea NOT NULL,
    schedule_revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, user_id, idempotency_key),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    CHECK (octet_length(request_hash) = 32)
);

CREATE INDEX schedule_publication_revision_idx
    ON schedule_publication_requests (workspace_id, user_id, schedule_revision_id);

CREATE TABLE schedule_simulations (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    token_hash bytea NOT NULL,
    subject_hash bytea NOT NULL,
    request_digest bytea NOT NULL,
    base_revision_id uuid NOT NULL,
    base_revision_label varchar(100) NOT NULL,
    result_snapshot jsonb NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    UNIQUE (workspace_id, user_id, token_hash),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, base_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    CHECK (octet_length(token_hash) = 32),
    CHECK (octet_length(subject_hash) = 32),
    CHECK (octet_length(request_digest) = 16),
    CHECK (btrim(base_revision_label) <> ''),
    CHECK (jsonb_typeof(result_snapshot) = 'object'),
    CHECK (octet_length(result_snapshot::text) <= 1048576),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= created_at + interval '15 minutes'),
    CHECK (consumed_at IS NULL OR consumed_at >= created_at)
);

CREATE INDEX schedule_simulations_active_idx
    ON schedule_simulations (workspace_id, user_id, expires_at, id)
    WHERE consumed_at IS NULL;

-- Content rows and evidence are immutable. A revision header permits exactly
-- one transition: current published -> superseded. This preserves history
-- while allowing a new revision to become current in the same transaction.
CREATE FUNCTION reject_schedule_content_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    RAISE EXCEPTION 'published schedule content is immutable';
END
$guard$;

CREATE TRIGGER schedule_blocks_immutable
    BEFORE UPDATE OR DELETE ON schedule_blocks
    FOR EACH ROW EXECUTE FUNCTION reject_schedule_content_mutation();

CREATE TRIGGER schedule_revision_details_immutable
    BEFORE UPDATE OR DELETE ON schedule_revision_details
    FOR EACH ROW EXECUTE FUNCTION reject_schedule_content_mutation();

CREATE TRIGGER schedule_publication_requests_immutable
    BEFORE UPDATE OR DELETE ON schedule_publication_requests
    FOR EACH ROW EXECUTE FUNCTION reject_schedule_content_mutation();

CREATE FUNCTION guard_schedule_revision_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'published schedule revisions are immutable';
    END IF;
    IF OLD.state <> 'published'
       OR NEW.state <> 'superseded'
       OR OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.revision_number IS DISTINCT FROM NEW.revision_number
       OR OLD.parent_revision_id IS DISTINCT FROM NEW.parent_revision_id
       OR OLD.horizon_start IS DISTINCT FROM NEW.horizon_start
       OR OLD.horizon_end IS DISTINCT FROM NEW.horizon_end
       OR OLD.timezone_name IS DISTINCT FROM NEW.timezone_name
       OR OLD.solver_version IS DISTINCT FROM NEW.solver_version
       OR OLD.input_digest IS DISTINCT FROM NEW.input_digest
       OR OLD.publication_hash IS DISTINCT FROM NEW.publication_hash
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR OLD.published_at IS DISTINCT FROM NEW.published_at
       OR OLD.superseded_at IS NOT NULL
       OR NEW.superseded_at IS NULL
       OR NEW.superseded_at < OLD.published_at
    THEN
        RAISE EXCEPTION 'published schedule revisions are immutable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER schedule_revisions_immutable
    BEFORE UPDATE OR DELETE ON schedule_revisions
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_revision_update();

-- Contract v2 introduces the REST-only schedule_publish capability. Existing
-- v1 credentials remain readable/revocable, but every newly issued credential
-- must present the v2 client contract enforced by the application service.
ALTER TABLE sessions
    DROP CONSTRAINT sessions_client_contract_version_check,
    ALTER COLUMN client_contract_version SET DEFAULT 2,
    ADD CONSTRAINT sessions_client_contract_version_check
        CHECK (client_contract_version IN (1, 2)),
    DROP CONSTRAINT sessions_v1_runtime_shape_check;

ALTER TABLE sessions
    ADD CONSTRAINT sessions_v1_runtime_shape_check CHECK (
        auth_version <> 1 OR revoked_at IS NOT NULL OR (
            client_instance_id IS NOT NULL
            AND client_kind IN ('macos', 'android')
            AND device_label IS NOT NULL
            AND btrim(device_label) <> ''
            AND octet_length(token_hash) = 32
            AND refresh_token_hash IS NOT NULL
            AND octet_length(refresh_token_hash) = 32
            AND cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'suggestions_read', 'suggestions_write',
                'schedule_read', 'schedule_simulate', 'schedule_publish',
                'items_read', 'items_write',
                'execution_read', 'execution_write',
                'google_read', 'google_write',
                'auth_sessions_read', 'auth_sessions_write',
                'auth_mcp_clients_read', 'auth_mcp_clients_write'
            ]::text[]
            AND (client_contract_version = 2 OR NOT ('schedule_publish' = ANY(scopes)))
            AND refresh_idle_expires_at IS NOT NULL
            AND absolute_expires_at IS NOT NULL
            AND credential_issued_at IS NOT NULL
            AND credential_issued_at >= created_at
            AND credential_issued_at < absolute_expires_at
            AND last_seen_at >= credential_issued_at
            AND expires_at > credential_issued_at
            AND expires_at <= credential_issued_at + interval '900 seconds'
            AND refresh_idle_expires_at > credential_issued_at
            AND refresh_idle_expires_at <= credential_issued_at + interval '2592000 seconds'
            AND expires_at <= absolute_expires_at
            AND refresh_idle_expires_at <= absolute_expires_at
            AND absolute_expires_at <= created_at + interval '15552000 seconds'
            AND (revoked_at IS NULL OR revoked_at >= created_at)
        )
    ) NOT VALID;

-- Contract/scope coupling applies even to historical or revoked rows. The
-- broader runtime-shape constraint intentionally exempts those states, so keep
-- this security property in an independent database check.
ALTER TABLE sessions
    ADD CONSTRAINT sessions_schedule_publish_contract_check CHECK (
        client_contract_version = 2 OR NOT ('schedule_publish' = ANY(scopes))
    ) NOT VALID;

ALTER TABLE device_enrollments
    DROP CONSTRAINT device_enrollments_client_contract_version_check,
    ALTER COLUMN client_contract_version SET DEFAULT 2,
    ADD CONSTRAINT device_enrollments_client_contract_version_check
        CHECK (client_contract_version IN (1, 2)),
    DROP CONSTRAINT device_enrollments_runtime_scopes_check;

ALTER TABLE device_enrollments
    ADD CONSTRAINT device_enrollments_runtime_scopes_check CHECK (
        consumed_at IS NOT NULL OR revoked_at IS NOT NULL OR (
            cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'suggestions_read', 'suggestions_write',
                'schedule_read', 'schedule_simulate', 'schedule_publish',
                'items_read', 'items_write',
                'execution_read', 'execution_write',
                'google_read', 'google_write',
                'auth_sessions_read', 'auth_sessions_write',
                'auth_mcp_clients_read', 'auth_mcp_clients_write'
            ]::text[]
            AND (client_contract_version = 2 OR NOT ('schedule_publish' = ANY(scopes)))
        )
    ) NOT VALID;

ALTER TABLE device_enrollments
    ADD CONSTRAINT device_enrollments_schedule_publish_contract_check CHECK (
        client_contract_version = 2 OR NOT ('schedule_publish' = ANY(scopes))
    ) NOT VALID;

ALTER TABLE mcp_clients
    DROP CONSTRAINT mcp_clients_client_contract_version_check,
    ALTER COLUMN client_contract_version SET DEFAULT 1,
    ADD CONSTRAINT mcp_clients_client_contract_version_check
        CHECK (client_contract_version = 1);

ALTER TABLE mcp_clients
    ADD CONSTRAINT mcp_clients_contract_scopes_check CHECK (
        scopes <@ ARRAY[
            'schedule_read', 'schedule_simulate', 'suggestions_submit'
        ]::text[]
    ) NOT VALID;
