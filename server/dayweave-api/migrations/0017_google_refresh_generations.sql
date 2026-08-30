-- Manual Google refreshes need a causal fence that is independent of wall
-- clocks. API and worker nodes may disagree about time, so timestamps remain
-- presentation metadata while these monotonic generations prove that a
-- successful run incorporated an accepted request.
ALTER TABLE google_sync_runs
    ADD COLUMN refresh_generation bigint NOT NULL DEFAULT 0
        CHECK (refresh_generation >= 0),
    ADD COLUMN claimed_refresh_generation bigint NOT NULL DEFAULT 0
        CHECK (claimed_refresh_generation >= 0),
    ADD COLUMN completed_refresh_generation bigint NOT NULL DEFAULT 0
        CHECK (completed_refresh_generation >= 0),
    ADD CONSTRAINT google_sync_runs_claimed_not_ahead_of_refresh_check
        CHECK (claimed_refresh_generation <= refresh_generation),
    ADD CONSTRAINT google_sync_runs_completed_not_ahead_of_claimed_check
        CHECK (completed_refresh_generation <= claimed_refresh_generation);

-- A durable request identity makes a response-loss retry exact. Replaying the
-- same request returns its original timestamp and generation without queuing a
-- second run; a terminal-run retry deliberately uses a new request identity.
CREATE TABLE google_sync_refresh_requests (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    provider_account_id uuid NOT NULL,
    request_id uuid NOT NULL,
    refresh_generation bigint NOT NULL CHECK (refresh_generation > 0),
    requested_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, provider_account_id, request_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id)
);

CREATE INDEX google_sync_refresh_requests_account_idx
    ON google_sync_refresh_requests
        (workspace_id, user_id, provider_account_id, refresh_generation);
