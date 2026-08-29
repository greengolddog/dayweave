-- Server-authoritative execution lease and immutable session history.
-- One active-or-paused row per workspace enforces the cross-device invariant.

CREATE TABLE execution_sessions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    occurrence_id uuid,
    session_index integer NOT NULL CHECK (session_index BETWEEN 0 AND 65535),
    planned_block_id uuid,
    source_device_id uuid NOT NULL,
    state varchar(24) NOT NULL
        CHECK (state IN ('active', 'paused', 'completed', 'skipped')),
    revision bigint NOT NULL CHECK (revision > 0),
    accumulated_seconds bigint NOT NULL DEFAULT 0 CHECK (accumulated_seconds >= 0),
    actual_seconds bigint CHECK (actual_seconds IS NULL OR actual_seconds >= 0),
    started_at timestamptz NOT NULL,
    running_since timestamptz,
    paused_at timestamptz,
    pause_until timestamptz,
    pause_reason varchar(500),
    ended_at timestamptz,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, item_id) REFERENCES items(workspace_id, id),
    CHECK (btrim(COALESCE(pause_reason, 'x')) <> ''),
    CHECK (
        (state = 'active' AND running_since IS NOT NULL AND paused_at IS NULL AND ended_at IS NULL)
        OR (state = 'paused' AND running_since IS NULL AND paused_at IS NOT NULL AND ended_at IS NULL)
        OR (state IN ('completed', 'skipped') AND running_since IS NULL AND ended_at IS NOT NULL)
    ),
    CHECK (pause_until IS NULL OR paused_at IS NOT NULL),
    CHECK (pause_until IS NULL OR pause_until > paused_at),
    CHECK ((state IN ('completed', 'skipped')) = (actual_seconds IS NOT NULL))
);

CREATE UNIQUE INDEX execution_sessions_one_open_uq
    ON execution_sessions (workspace_id)
    WHERE state IN ('active', 'paused');

CREATE INDEX execution_sessions_history_idx
    ON execution_sessions (workspace_id, updated_at DESC, id DESC);

CREATE INDEX execution_sessions_item_idx
    ON execution_sessions (workspace_id, item_id, updated_at DESC);

CREATE TABLE execution_state (
    workspace_id uuid PRIMARY KEY REFERENCES workspaces(id),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    active_session_id uuid,
    updated_at timestamptz NOT NULL DEFAULT current_timestamp,
    FOREIGN KEY (workspace_id, active_session_id)
        REFERENCES execution_sessions(workspace_id, id),
    CHECK ((revision = 0) = (active_session_id IS NULL) OR revision > 0)
);
