-- Suggestions Inbox persistence, MCP client registration, durable idempotency,
-- and a transactional outbox. Raw bearer tokens and idempotency keys are never
-- stored; only hashes belong in these tables.

CREATE TABLE proposals (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    revision bigint NOT NULL CHECK (revision > 0),
    submitted_by_user_id uuid REFERENCES users(id),
    submitted_by_subject varchar(500) NOT NULL,
    source varchar(32) NOT NULL
        CHECK (source IN ('app_assistant', 'chat_gpt', 'codex', 'external_mcp')),
    source_reference varchar(500),
    kind varchar(32) NOT NULL
        CHECK (kind IN ('create_item', 'update_item', 'goal_breakdown', 'constraint_change', 'calendar_event', 'schedule_plan', 'recommendation')),
    status varchar(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'rejected', 'expired')),
    title varchar(200) NOT NULL,
    explanation varchar(4000),
    payload jsonb NOT NULL,
    decision_note varchar(1000),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    decided_at timestamptz,
    trashed_at timestamptz,
    tombstoned_at timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, submitted_by_user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (btrim(submitted_by_subject) <> ''),
    CHECK (btrim(title) <> ''),
    CHECK (jsonb_typeof(payload) = 'object'),
    CHECK (expires_at > created_at),
    CHECK (
        (status = 'pending' AND decided_at IS NULL)
        OR (status IN ('accepted', 'rejected') AND decided_at IS NOT NULL)
        OR status = 'expired'
    ),
    CHECK (tombstoned_at IS NULL OR trashed_at IS NOT NULL)
);

CREATE INDEX proposals_inbox_idx
    ON proposals (workspace_id, status, created_at DESC, id DESC)
    WHERE trashed_at IS NULL;

CREATE INDEX proposals_source_idx
    ON proposals (workspace_id, source, created_at DESC)
    WHERE trashed_at IS NULL;

CREATE INDEX proposals_expiration_idx
    ON proposals (workspace_id, expires_at)
    WHERE status = 'pending' AND trashed_at IS NULL;

CREATE TABLE mcp_clients (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    created_by_user_id uuid NOT NULL REFERENCES users(id),
    client_identifier varchar(300) NOT NULL,
    display_name varchar(200) NOT NULL,
    credential_hash bytea,
    scopes text[] NOT NULL DEFAULT '{}',
    allowed_origins text[] NOT NULL DEFAULT '{}',
    status varchar(24) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'paused', 'revoked')),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    last_seen_at timestamptz,
    expires_at timestamptz,
    revoked_at timestamptz,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, client_identifier),
    FOREIGN KEY (workspace_id, created_by_user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (btrim(client_identifier) <> ''),
    CHECK (btrim(display_name) <> ''),
    CHECK (credential_hash IS NULL OR octet_length(credential_hash) >= 32),
    CHECK ((status = 'revoked') = (revoked_at IS NOT NULL))
);

CREATE INDEX mcp_clients_active_idx
    ON mcp_clients (workspace_id, client_identifier)
    WHERE status = 'active';

CREATE TABLE idempotency_keys (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    namespace varchar(100) NOT NULL,
    key_hash bytea NOT NULL,
    request_fingerprint bytea NOT NULL,
    state varchar(24) NOT NULL DEFAULT 'in_progress'
        CHECK (state IN ('in_progress', 'completed')),
    resource_type varchar(100),
    resource_id uuid,
    response_json jsonb,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, namespace, key_hash),
    CHECK (btrim(namespace) <> ''),
    CHECK (octet_length(key_hash) = 32),
    CHECK (octet_length(request_fingerprint) >= 16),
    CHECK (expires_at > created_at),
    CHECK (
        (state = 'in_progress' AND response_json IS NULL)
        OR state = 'completed'
    )
);

CREATE INDEX idempotency_keys_expiry_idx
    ON idempotency_keys (expires_at);

CREATE TABLE outbox_messages (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    aggregate_type varchar(100) NOT NULL,
    aggregate_id uuid NOT NULL,
    aggregate_revision bigint CHECK (aggregate_revision IS NULL OR aggregate_revision > 0),
    event_type varchar(150) NOT NULL,
    deduplication_key varchar(500),
    payload jsonb NOT NULL,
    headers jsonb NOT NULL DEFAULT '{}'::jsonb,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at timestamptz NOT NULL DEFAULT current_timestamp,
    claimed_by varchar(200),
    claimed_at timestamptz,
    published_at timestamptz,
    last_error_code varchar(100),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    CHECK (btrim(aggregate_type) <> ''),
    CHECK (btrim(event_type) <> ''),
    CHECK (jsonb_typeof(payload) = 'object'),
    CHECK (jsonb_typeof(headers) = 'object'),
    CHECK ((claimed_by IS NULL) = (claimed_at IS NULL)),
    CHECK (published_at IS NULL OR claimed_by IS NOT NULL)
);

CREATE UNIQUE INDEX outbox_messages_deduplication_uq
    ON outbox_messages (workspace_id, deduplication_key)
    WHERE deduplication_key IS NOT NULL;

CREATE INDEX outbox_messages_delivery_idx
    ON outbox_messages (workspace_id, available_at, created_at, id)
    WHERE published_at IS NULL;

CREATE INDEX outbox_messages_claim_idx
    ON outbox_messages (workspace_id, claimed_at)
    WHERE published_at IS NULL AND claimed_at IS NOT NULL;
