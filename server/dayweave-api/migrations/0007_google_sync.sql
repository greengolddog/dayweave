-- Durable Google Calendar/Tasks collection selection and reconciliation state.
-- Provider cursors remain encrypted in provider_sync_cursors. Outbox JSON can
-- contain canonical titles and notes, so PostgreSQL/storage access remains a
-- sensitive-data boundary even though OAuth credentials are stored separately.

CREATE TABLE google_sync_collections (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    provider_account_id uuid NOT NULL,
    collection_kind varchar(16) NOT NULL
        CHECK (collection_kind IN ('calendar', 'task_list')),
    remote_collection_id varchar(1000) NOT NULL,
    display_name varchar(500) NOT NULL,
    provider_access_role varchar(32),
    provider_primary boolean NOT NULL DEFAULT false,
    provider_selected boolean NOT NULL DEFAULT false,
    provider_hidden boolean NOT NULL DEFAULT false,
    provider_deleted boolean NOT NULL DEFAULT false,
    selected boolean NOT NULL DEFAULT false,
    visible boolean NOT NULL DEFAULT true,
    sync_role varchar(16) NOT NULL DEFAULT 'read_only'
        CHECK (sync_role IN ('read_only', 'blocking', 'writable')),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    discovered_at timestamptz NOT NULL,
    configured_at timestamptz,
    last_import_at timestamptz,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, provider_account_id, collection_kind, remote_collection_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    CHECK (btrim(remote_collection_id) <> ''),
    CHECK (btrim(display_name) <> ''),
    CHECK (NOT provider_deleted OR NOT selected),
    CHECK (collection_kind = 'calendar' OR sync_role <> 'blocking'),
    CHECK (collection_kind <> 'calendar' OR sync_role <> 'writable'
        OR (provider_access_role IS NOT NULL
            AND provider_access_role IN ('owner', 'writer')))
);

CREATE INDEX google_sync_collections_selected_idx
    ON google_sync_collections (workspace_id, provider_account_id, collection_kind, id)
    WHERE selected AND NOT provider_deleted;

CREATE TABLE google_sync_runs (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    provider_account_id uuid NOT NULL,
    state varchar(32) NOT NULL DEFAULT 'idle'
        CHECK (state IN ('idle', 'running', 'backoff', 'reauthorization_required', 'failed')),
    claim_id uuid,
    lease_until timestamptz,
    requested_at timestamptz,
    started_at timestamptz,
    completed_at timestamptz,
    next_attempt_at timestamptz NOT NULL,
    consecutive_failures integer NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    last_error_code varchar(64),
    last_error_at timestamptz,
    imported_count bigint NOT NULL DEFAULT 0 CHECK (imported_count >= 0),
    updated_count bigint NOT NULL DEFAULT 0 CHECK (updated_count >= 0),
    deleted_count bigint NOT NULL DEFAULT 0 CHECK (deleted_count >= 0),
    conflict_count bigint NOT NULL DEFAULT 0 CHECK (conflict_count >= 0),
    rejected_count bigint NOT NULL DEFAULT 0 CHECK (rejected_count >= 0),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, provider_account_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    CHECK ((state = 'running') = (claim_id IS NOT NULL AND lease_until IS NOT NULL))
);

CREATE INDEX google_sync_runs_due_idx
    ON google_sync_runs (workspace_id, next_attempt_at, provider_account_id)
    WHERE state IN ('idle', 'backoff');

-- Composite provider-owned foreign keys retain the workspace boundary. The
-- audit row ID is globally unique already, so this is migration-safe for
-- existing data and gives PostgreSQL the exact referenced key shape.
ALTER TABLE audit_operations
    ADD CONSTRAINT audit_operations_workspace_id_uq UNIQUE (workspace_id, id);

ALTER TABLE provider_sync_mappings
    ADD COLUMN collection_id uuid,
    ADD COLUMN remote_parent_id varchar(1000),
    ADD COLUMN remote_payload_hash bytea,
    -- Separates the provider representation from the canonical projection.
    -- Visibility/role changes can therefore re-project an unchanged Google
    -- record without weakening the provider-change fence on app-owned rows.
    ADD COLUMN remote_projection_hash bytea,
    ADD COLUMN ownership varchar(16) NOT NULL DEFAULT 'external'
        CHECK (ownership IN ('external', 'dayweave')),
    ADD COLUMN approval_audit_id uuid;

ALTER TABLE provider_sync_mappings
    ADD CONSTRAINT provider_sync_mappings_collection_fk
        FOREIGN KEY (workspace_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, id),
    ADD CONSTRAINT provider_sync_mappings_payload_hash_check
        CHECK (remote_payload_hash IS NULL OR octet_length(remote_payload_hash) = 32),
    ADD CONSTRAINT provider_sync_mappings_projection_hash_check
        CHECK (remote_projection_hash IS NULL OR octet_length(remote_projection_hash) = 32),
    ADD CONSTRAINT provider_sync_mappings_approval_check
        CHECK (approval_audit_id IS NULL OR ownership = 'dayweave');

DROP INDEX provider_sync_mappings_remote_uq;
DROP INDEX provider_sync_mappings_local_uq;

CREATE UNIQUE INDEX provider_sync_mappings_remote_uq
    ON provider_sync_mappings
        (workspace_id, provider_account_id, collection_id, entity_kind, remote_resource_id)
    WHERE collection_id IS NOT NULL AND tombstoned_at IS NULL;

-- Preserve the uniqueness contract for pre-sync mappings. PostgreSQL treats a
-- NULL collection as distinct, so the collection-aware indexes above do not
-- protect legacy rows on their own.
CREATE UNIQUE INDEX provider_sync_mappings_legacy_remote_uq
    ON provider_sync_mappings
        (workspace_id, provider_account_id, entity_kind, remote_resource_id)
    WHERE collection_id IS NULL AND tombstoned_at IS NULL;

CREATE UNIQUE INDEX provider_sync_mappings_local_uq
    ON provider_sync_mappings
        (workspace_id, provider_account_id, collection_id, entity_kind, local_entity_id)
    WHERE collection_id IS NOT NULL AND local_entity_id IS NOT NULL AND tombstoned_at IS NULL;

CREATE UNIQUE INDEX provider_sync_mappings_legacy_local_uq
    ON provider_sync_mappings
        (workspace_id, provider_account_id, entity_kind, local_entity_id)
    WHERE collection_id IS NULL AND local_entity_id IS NOT NULL AND tombstoned_at IS NULL;

CREATE TABLE google_sync_outbox (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    entity_kind varchar(24) NOT NULL CHECK (entity_kind IN ('calendar_event', 'task')),
    operation varchar(16) NOT NULL CHECK (operation IN ('upsert', 'delete')),
    remote_resource_id varchar(1000),
    expected_etag varchar(1000),
    app_owned boolean NOT NULL,
    approval_audit_id uuid,
    payload jsonb NOT NULL,
    state varchar(24) NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'delivering', 'backoff', 'conflict', 'published', 'failed',
            'superseded')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claim_id uuid,
    claimed_at timestamptz,
    available_at timestamptz NOT NULL,
    last_error_code varchar(64),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, collection_id, item_id, item_revision, operation),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    FOREIGN KEY (workspace_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, approval_audit_id)
        REFERENCES audit_operations(workspace_id, id),
    CHECK (jsonb_typeof(payload) = 'object'),
    CHECK ((state = 'delivering') = (claim_id IS NOT NULL AND claimed_at IS NOT NULL)),
    CHECK (app_owned OR approval_audit_id IS NOT NULL),
    CHECK (app_owned OR remote_resource_id IS NOT NULL)
);

CREATE INDEX google_sync_outbox_delivery_idx
    ON google_sync_outbox (workspace_id, available_at, created_at, id)
    WHERE state IN ('pending', 'backoff');
