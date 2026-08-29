-- Durable, revocable credential foundation. Runtime HTTP/MCP authentication
-- remains on the existing static-token path until a separately reviewed
-- cutover. Version 0 preserves unused legacy rows but is never accepted by the
-- new repository; only version 1 rows satisfy the strict contracts below.

ALTER TABLE sessions
    ADD COLUMN auth_version smallint NOT NULL DEFAULT 0,
    ADD COLUMN client_instance_id uuid,
    ADD COLUMN refresh_token_hash bytea,
    ADD COLUMN scopes text[] NOT NULL DEFAULT '{}',
    ADD COLUMN refresh_idle_expires_at timestamptz,
    ADD COLUMN absolute_expires_at timestamptz,
    ADD COLUMN credential_issued_at timestamptz,
    ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD CONSTRAINT sessions_workspace_user_id_uq UNIQUE (workspace_id, user_id, id),
    ADD CONSTRAINT sessions_auth_version_check CHECK (auth_version IN (0, 1)),
    ADD CONSTRAINT sessions_v1_shape_check CHECK (
        auth_version <> 1 OR (
            client_instance_id IS NOT NULL
            AND client_kind IN ('macos', 'android')
            AND device_label IS NOT NULL
            AND btrim(device_label) <> ''
            AND octet_length(token_hash) = 32
            AND refresh_token_hash IS NOT NULL
            AND octet_length(refresh_token_hash) = 32
            AND cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'suggestions_read',
                'suggestions_write',
                'schedule_read',
                'schedule_simulate',
                'suggestions_submit'
            ]::text[]
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
    );

CREATE UNIQUE INDEX sessions_v1_refresh_token_hash_uq
    ON sessions (refresh_token_hash)
    WHERE auth_version = 1;

CREATE UNIQUE INDEX sessions_v1_active_device_uq
    ON sessions (workspace_id, user_id, client_instance_id)
    WHERE auth_version = 1 AND revoked_at IS NULL;

CREATE INDEX sessions_v1_access_lookup_idx
    ON sessions (workspace_id, user_id, token_hash)
    WHERE auth_version = 1 AND revoked_at IS NULL;

CREATE TABLE device_enrollments (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    client_instance_id uuid NOT NULL,
    client_kind varchar(32) NOT NULL CHECK (client_kind IN ('macos', 'android')),
    device_label varchar(200) NOT NULL,
    token_hash bytea NOT NULL UNIQUE,
    scopes text[] NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    consumed_session_id uuid,
    revoked_at timestamptz,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    UNIQUE (workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, user_id, consumed_session_id)
        REFERENCES sessions(workspace_id, user_id, id),
    CHECK (btrim(device_label) <> ''),
    CHECK (octet_length(token_hash) = 32),
    CHECK (cardinality(scopes) > 0),
    CHECK (scopes <@ ARRAY[
        'suggestions_read',
        'suggestions_write',
        'schedule_read',
        'schedule_simulate',
        'suggestions_submit'
    ]::text[]),
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '600 seconds'),
    CHECK (consumed_at IS NULL OR consumed_at >= created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at),
    CHECK (NOT (consumed_at IS NOT NULL AND revoked_at IS NOT NULL)),
    CHECK ((consumed_at IS NULL) = (consumed_session_id IS NULL))
);

CREATE INDEX device_enrollments_lookup_idx
    ON device_enrollments (workspace_id, user_id, token_hash)
    WHERE consumed_at IS NULL AND revoked_at IS NULL;

ALTER TABLE mcp_clients
    ADD COLUMN auth_version smallint NOT NULL DEFAULT 0,
    ADD CONSTRAINT mcp_clients_auth_version_check CHECK (auth_version IN (0, 1)),
    ADD CONSTRAINT mcp_clients_v1_shape_check CHECK (
        auth_version <> 1 OR (
            credential_hash IS NOT NULL
            AND octet_length(credential_hash) = 32
            AND cardinality(scopes) > 0
            AND scopes <@ ARRAY[
                'suggestions_read',
                'suggestions_write',
                'schedule_read',
                'schedule_simulate',
                'suggestions_submit'
            ]::text[]
            AND cardinality(allowed_origins) <= 100
            AND array_position(allowed_origins, NULL) IS NULL
            AND expires_at IS NOT NULL
            AND expires_at > created_at
            AND expires_at <= created_at + interval '31536000 seconds'
        )
    );

CREATE UNIQUE INDEX mcp_clients_v1_credential_hash_uq
    ON mcp_clients (credential_hash)
    WHERE auth_version = 1;

CREATE INDEX mcp_clients_v1_auth_lookup_idx
    ON mcp_clients (workspace_id, created_by_user_id, credential_hash)
    WHERE auth_version = 1 AND status = 'active';
