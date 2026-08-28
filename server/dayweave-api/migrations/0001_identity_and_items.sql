-- DayWeave durable identity, workspace, provider, and canonical item model.
-- Every timestamp is an absolute instant; user-facing timezone names are stored
-- separately and validated by the application against the IANA database.

CREATE TABLE users (
    id uuid PRIMARY KEY,
    auth_subject varchar(500) NOT NULL UNIQUE,
    display_name varchar(200) NOT NULL,
    timezone_name varchar(100) NOT NULL DEFAULT 'UTC',
    locale varchar(35) NOT NULL DEFAULT 'en',
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    trashed_at timestamptz,
    tombstoned_at timestamptz,
    CHECK (btrim(auth_subject) <> ''),
    CHECK (btrim(display_name) <> ''),
    CHECK (btrim(timezone_name) <> ''),
    CHECK (tombstoned_at IS NULL OR trashed_at IS NOT NULL)
);

CREATE TABLE workspaces (
    id uuid PRIMARY KEY,
    owner_user_id uuid NOT NULL REFERENCES users(id),
    slug varchar(100) NOT NULL,
    name varchar(200) NOT NULL,
    timezone_name varchar(100) NOT NULL DEFAULT 'UTC',
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    trashed_at timestamptz,
    tombstoned_at timestamptz,
    UNIQUE (id, owner_user_id),
    CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,99}$'),
    CHECK (btrim(name) <> ''),
    CHECK (btrim(timezone_name) <> ''),
    CHECK (tombstoned_at IS NULL OR trashed_at IS NOT NULL)
);

CREATE UNIQUE INDEX workspaces_active_slug_uq
    ON workspaces (owner_user_id, lower(slug))
    WHERE trashed_at IS NULL;

CREATE TABLE workspace_members (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    role varchar(32) NOT NULL DEFAULT 'owner'
        CHECK (role IN ('owner', 'member', 'viewer')),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    joined_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    removed_at timestamptz,
    PRIMARY KEY (workspace_id, user_id)
);

CREATE INDEX workspace_members_user_idx
    ON workspace_members (user_id, workspace_id)
    WHERE removed_at IS NULL;

CREATE TABLE provider_accounts (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    provider varchar(32) NOT NULL
        CHECK (provider IN ('google_calendar', 'google_tasks', 'whoop')),
    external_account_id varchar(500),
    display_label varchar(200) NOT NULL,
    encrypted_credentials bytea NOT NULL,
    credential_key_version integer NOT NULL CHECK (credential_key_version > 0),
    granted_scopes text[] NOT NULL DEFAULT '{}',
    status varchar(32) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'reauthorization_required', 'paused', 'revoked')),
    sync_enabled boolean NOT NULL DEFAULT true,
    token_expires_at timestamptz,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    trashed_at timestamptz,
    tombstoned_at timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (btrim(display_label) <> ''),
    CHECK (octet_length(encrypted_credentials) > 0),
    CHECK (tombstoned_at IS NULL OR trashed_at IS NOT NULL)
);

CREATE UNIQUE INDEX provider_accounts_external_uq
    ON provider_accounts (workspace_id, provider, external_account_id)
    WHERE external_account_id IS NOT NULL AND tombstoned_at IS NULL;

CREATE INDEX provider_accounts_sync_idx
    ON provider_accounts (workspace_id, provider, status)
    WHERE sync_enabled AND trashed_at IS NULL;

CREATE TABLE items (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    created_by_user_id uuid NOT NULL REFERENCES users(id),
    kind varchar(32) NOT NULL
        CHECK (kind IN ('task', 'event', 'habit', 'routine', 'goal', 'break')),
    status varchar(32) NOT NULL DEFAULT 'inbox'
        CHECK (status IN ('inbox', 'planned', 'scheduled', 'in_progress', 'paused', 'completed', 'skipped', 'cancelled')),
    title varchar(500) NOT NULL,
    notes text,
    timezone_name varchar(100) NOT NULL DEFAULT 'UTC',
    duration_seconds integer CHECK (duration_seconds IS NULL OR duration_seconds > 0),
    deadline_at timestamptz,
    earliest_start_at timestamptz,
    recurrence jsonb,
    scheduling_constraints jsonb NOT NULL DEFAULT '{}'::jsonb,
    split_allowed boolean NOT NULL DEFAULT false,
    minimum_chunk_seconds integer,
    maximum_chunk_seconds integer,
    importance smallint NOT NULL DEFAULT 0 CHECK (importance BETWEEN 0 AND 100),
    urgency smallint NOT NULL DEFAULT 0 CHECK (urgency BETWEEN 0 AND 100),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    completed_at timestamptz,
    trashed_at timestamptz,
    tombstoned_at timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, created_by_user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (btrim(title) <> ''),
    CHECK (btrim(timezone_name) <> ''),
    CHECK (recurrence IS NULL OR jsonb_typeof(recurrence) = 'object'),
    CHECK (jsonb_typeof(scheduling_constraints) = 'object'),
    CHECK (
        (NOT split_allowed AND minimum_chunk_seconds IS NULL AND maximum_chunk_seconds IS NULL)
        OR (
            split_allowed
            AND duration_seconds IS NOT NULL
            AND minimum_chunk_seconds > 0
            AND maximum_chunk_seconds >= minimum_chunk_seconds
            AND duration_seconds >= minimum_chunk_seconds
        )
    ),
    CHECK (earliest_start_at IS NULL OR deadline_at IS NULL OR earliest_start_at < deadline_at),
    CHECK (tombstoned_at IS NULL OR trashed_at IS NOT NULL)
);

CREATE INDEX items_workspace_status_idx
    ON items (workspace_id, status, deadline_at, updated_at DESC)
    WHERE trashed_at IS NULL;

CREATE INDEX items_workspace_kind_idx
    ON items (workspace_id, kind, updated_at DESC)
    WHERE trashed_at IS NULL;

CREATE INDEX items_due_idx
    ON items (workspace_id, deadline_at)
    WHERE deadline_at IS NOT NULL AND trashed_at IS NULL
      AND status NOT IN ('completed', 'cancelled');

-- Adjacency edges allow arbitrarily deep item trees. Cycle rejection belongs in
-- the transactional hierarchy service, where the recursive lookup can lock the
-- affected path before inserting an edge.
CREATE TABLE item_hierarchy (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    parent_item_id uuid NOT NULL,
    child_item_id uuid NOT NULL,
    position integer NOT NULL DEFAULT 0 CHECK (position >= 0),
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, child_item_id),
    FOREIGN KEY (workspace_id, parent_item_id)
        REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, child_item_id)
        REFERENCES items(workspace_id, id),
    CHECK (parent_item_id <> child_item_id)
);

CREATE INDEX item_hierarchy_parent_idx
    ON item_hierarchy (workspace_id, parent_item_id, position, child_item_id);

CREATE TABLE item_dependencies (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    predecessor_item_id uuid NOT NULL,
    successor_item_id uuid NOT NULL,
    dependency_kind varchar(32) NOT NULL DEFAULT 'finish_to_start'
        CHECK (dependency_kind IN ('finish_to_start', 'start_to_start', 'finish_to_finish')),
    lag_seconds integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    PRIMARY KEY (workspace_id, predecessor_item_id, successor_item_id),
    FOREIGN KEY (workspace_id, predecessor_item_id)
        REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, successor_item_id)
        REFERENCES items(workspace_id, id),
    CHECK (predecessor_item_id <> successor_item_id)
);

CREATE INDEX item_dependencies_successor_idx
    ON item_dependencies (workspace_id, successor_item_id);
