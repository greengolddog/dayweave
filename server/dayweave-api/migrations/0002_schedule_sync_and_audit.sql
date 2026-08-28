-- Immutable schedule revisions, provider sync state, sessions, and audit trail.

CREATE TABLE schedule_revisions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    revision_number bigint NOT NULL CHECK (revision_number > 0),
    parent_revision_id uuid,
    state varchar(24) NOT NULL DEFAULT 'draft'
        CHECK (state IN ('draft', 'published', 'superseded', 'discarded')),
    horizon_start timestamptz NOT NULL,
    horizon_end timestamptz NOT NULL,
    timezone_name varchar(100) NOT NULL,
    solver_version varchar(100),
    input_digest bytea NOT NULL,
    created_by_user_id uuid NOT NULL REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    published_at timestamptz,
    superseded_at timestamptz,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, revision_number),
    FOREIGN KEY (workspace_id, parent_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    FOREIGN KEY (workspace_id, created_by_user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (horizon_end > horizon_start),
    CHECK (btrim(timezone_name) <> ''),
    CHECK (octet_length(input_digest) >= 16),
    CHECK ((state = 'published') = (published_at IS NOT NULL) OR state IN ('superseded', 'discarded'))
);

CREATE UNIQUE INDEX schedule_revisions_one_published_uq
    ON schedule_revisions (workspace_id)
    WHERE state = 'published';

CREATE INDEX schedule_revisions_history_idx
    ON schedule_revisions (workspace_id, revision_number DESC);

CREATE TABLE schedule_blocks (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    schedule_revision_id uuid NOT NULL,
    item_id uuid,
    block_kind varchar(32) NOT NULL
        CHECK (block_kind IN ('item', 'calendar_event', 'break', 'buffer', 'focus', 'unavailable')),
    title_snapshot varchar(500),
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    timezone_name varchar(100) NOT NULL,
    ordinal integer NOT NULL DEFAULT 0 CHECK (ordinal >= 0),
    is_fixed boolean NOT NULL DEFAULT false,
    is_sensitive boolean NOT NULL DEFAULT false,
    constraint_snapshot jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    CHECK (ends_at > starts_at),
    CHECK (btrim(timezone_name) <> ''),
    CHECK (jsonb_typeof(constraint_snapshot) = 'object')
);

CREATE INDEX schedule_blocks_timeline_idx
    ON schedule_blocks (workspace_id, schedule_revision_id, starts_at, ends_at, ordinal);

CREATE INDEX schedule_blocks_item_idx
    ON schedule_blocks (workspace_id, item_id, schedule_revision_id)
    WHERE item_id IS NOT NULL;

CREATE TABLE provider_sync_mappings (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    provider_account_id uuid NOT NULL,
    entity_kind varchar(32) NOT NULL
        CHECK (entity_kind IN ('item', 'schedule_block', 'calendar', 'task_list')),
    local_entity_id uuid,
    remote_resource_id varchar(1000) NOT NULL,
    remote_etag varchar(1000),
    remote_updated_at timestamptz,
    local_revision bigint CHECK (local_revision IS NULL OR local_revision > 0),
    sync_state varchar(32) NOT NULL DEFAULT 'synced'
        CHECK (sync_state IN ('synced', 'pending_push', 'pending_pull', 'conflict', 'deleted_remote')),
    conflict_metadata jsonb,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    tombstoned_at timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    CHECK (btrim(remote_resource_id) <> ''),
    CHECK (conflict_metadata IS NULL OR jsonb_typeof(conflict_metadata) = 'object')
);

CREATE UNIQUE INDEX provider_sync_mappings_remote_uq
    ON provider_sync_mappings
        (workspace_id, provider_account_id, entity_kind, remote_resource_id)
    WHERE tombstoned_at IS NULL;

CREATE UNIQUE INDEX provider_sync_mappings_local_uq
    ON provider_sync_mappings
        (workspace_id, provider_account_id, entity_kind, local_entity_id)
    WHERE local_entity_id IS NOT NULL AND tombstoned_at IS NULL;

CREATE INDEX provider_sync_mappings_pending_idx
    ON provider_sync_mappings (workspace_id, provider_account_id, sync_state, updated_at)
    WHERE sync_state <> 'synced';

CREATE TABLE provider_sync_cursors (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    provider_account_id uuid NOT NULL,
    collection_key varchar(500) NOT NULL,
    encrypted_cursor bytea NOT NULL,
    cursor_key_version integer NOT NULL CHECK (cursor_key_version > 0),
    watermark_at timestamptz,
    last_success_at timestamptz,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, provider_account_id, collection_key),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    CHECK (btrim(collection_key) <> ''),
    CHECK (octet_length(encrypted_cursor) > 0)
);

CREATE TABLE sessions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    token_hash bytea NOT NULL UNIQUE,
    client_kind varchar(32) NOT NULL
        CHECK (client_kind IN ('macos', 'android', 'web', 'mcp', 'service')),
    device_label varchar(200),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    last_seen_at timestamptz NOT NULL DEFAULT current_timestamp,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (octet_length(token_hash) >= 32),
    CHECK (jsonb_typeof(metadata) = 'object'),
    CHECK (expires_at > created_at)
);

CREATE INDEX sessions_active_user_idx
    ON sessions (workspace_id, user_id, expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE audit_operations (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    actor_user_id uuid,
    actor_session_id uuid,
    request_id varchar(200),
    operation_type varchar(100) NOT NULL,
    entity_type varchar(100) NOT NULL,
    entity_id uuid,
    base_revision bigint CHECK (base_revision IS NULL OR base_revision > 0),
    result_revision bigint CHECK (result_revision IS NULL OR result_revision > 0),
    outcome varchar(24) NOT NULL
        CHECK (outcome IN ('succeeded', 'rejected', 'conflicted', 'failed')),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    occurred_at timestamptz NOT NULL DEFAULT current_timestamp,
    FOREIGN KEY (workspace_id, actor_user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, actor_session_id)
        REFERENCES sessions(workspace_id, id),
    CHECK (btrim(operation_type) <> ''),
    CHECK (btrim(entity_type) <> ''),
    CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE INDEX audit_operations_entity_idx
    ON audit_operations (workspace_id, entity_type, entity_id, occurred_at DESC);

CREATE INDEX audit_operations_actor_idx
    ON audit_operations (workspace_id, actor_user_id, occurred_at DESC)
    WHERE actor_user_id IS NOT NULL;

CREATE INDEX audit_operations_request_idx
    ON audit_operations (workspace_id, request_id)
    WHERE request_id IS NOT NULL;
