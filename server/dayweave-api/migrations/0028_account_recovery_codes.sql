-- One-time, owner-scoped account recovery credentials.
--
-- Plaintext recovery credentials are generated and journaled by a native
-- client. The server stores only a domain-separated digest. Every terminal
-- recovery-code row links to its successor so an exact consume/rotation retry
-- can recover a committed response without retaining plaintext.

CREATE TABLE account_recovery_codes (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    token_hash bytea NOT NULL UNIQUE,
    predecessor_code_id uuid,
    predecessor_revision bigint,
    replacement_code_id uuid,
    recovered_session_id uuid,
    created_at timestamptz NOT NULL,
    consumed_at timestamptz,
    revoked_at timestamptz,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    UNIQUE (workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, user_id, predecessor_code_id)
        REFERENCES account_recovery_codes(workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id, replacement_code_id)
        REFERENCES account_recovery_codes(workspace_id, user_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (workspace_id, user_id, recovered_session_id)
        REFERENCES sessions(workspace_id, user_id, id),
    CHECK (octet_length(token_hash) = 32),
    CHECK (predecessor_code_id IS NULL OR predecessor_code_id <> id),
    CHECK (predecessor_revision IS NULL OR predecessor_revision > 0),
    CHECK ((predecessor_code_id IS NULL) = (predecessor_revision IS NULL)),
    CHECK (replacement_code_id IS NULL OR replacement_code_id <> id),
    CHECK (consumed_at IS NULL OR consumed_at >= created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at),
    CHECK (NOT (consumed_at IS NOT NULL AND revoked_at IS NOT NULL)),
    CHECK ((consumed_at IS NULL) = (recovered_session_id IS NULL)),
    CHECK (
        (consumed_at IS NULL AND revoked_at IS NULL) =
        (replacement_code_id IS NULL)
    )
);

CREATE UNIQUE INDEX account_recovery_codes_one_active_owner_uq
    ON account_recovery_codes (workspace_id, user_id)
    WHERE consumed_at IS NULL AND revoked_at IS NULL;

CREATE INDEX account_recovery_codes_scoped_token_lookup_idx
    ON account_recovery_codes (workspace_id, user_id, token_hash);
