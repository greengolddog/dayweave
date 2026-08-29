-- One Google identity supplies both Calendar and Tasks scopes. OAuth callback
-- state is one-use and only its SHA-256 digest is durable. Secrets are sealed
-- by the application with AES-256-GCM and row-identity AAD before persistence.

ALTER TABLE provider_accounts DROP CONSTRAINT provider_accounts_provider_check;
ALTER TABLE provider_accounts DROP CONSTRAINT provider_accounts_status_check;
DROP INDEX provider_accounts_external_uq;

ALTER TABLE provider_accounts ALTER COLUMN encrypted_credentials DROP NOT NULL;
ALTER TABLE provider_accounts ALTER COLUMN credential_key_version DROP NOT NULL;
ALTER TABLE provider_accounts
    ADD COLUMN disconnect_claim_id uuid,
    ADD COLUMN disconnect_claimed_at timestamptz,
    ADD COLUMN disconnect_operation_hash bytea,
    ADD COLUMN disconnected_at timestamptz,
    ADD COLUMN revocation_error_at timestamptz,
    ADD COLUMN is_default boolean NOT NULL DEFAULT false;

-- The split legacy providers cannot safely share credentials with the unified
-- adapter, and their opaque envelopes are not assumed to be decryptable by the
-- new adapter. Preserve them in an explicit recovery quarantine. They remain
-- visibly operator-recovery-required until the operator revokes the affected
-- Google project grants outside DayWeave and acknowledges that action through
-- the authenticated recovery endpoint. Readiness remains false until then.
CREATE TABLE google_oauth_legacy_credential_quarantine (
    source_account_id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    legacy_provider varchar(32) NOT NULL
        CHECK (legacy_provider IN ('google_calendar', 'google_tasks')),
    external_account_id varchar(500),
    encrypted_credentials bytea,
    credential_key_version integer CHECK (credential_key_version > 0),
    quarantined_at timestamptz NOT NULL,
    recovery_confirmed_at timestamptz,
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, source_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    CHECK (
        (recovery_confirmed_at IS NULL
            AND encrypted_credentials IS NOT NULL
            AND octet_length(encrypted_credentials) > 0
            AND credential_key_version IS NOT NULL)
        OR
        (recovery_confirmed_at IS NOT NULL
            AND encrypted_credentials IS NULL
            AND credential_key_version IS NULL)
    )
);

INSERT INTO google_oauth_legacy_credential_quarantine (
    source_account_id, workspace_id, user_id, legacy_provider,
    external_account_id, encrypted_credentials, credential_key_version, quarantined_at
)
SELECT id, workspace_id, user_id, provider, external_account_id,
       encrypted_credentials, credential_key_version, current_timestamp
FROM provider_accounts
WHERE provider IN ('google_calendar', 'google_tasks')
  AND encrypted_credentials IS NOT NULL
  AND credential_key_version IS NOT NULL;

UPDATE provider_accounts
SET status = 'operator_recovery_required',
    sync_enabled = false,
    is_default = false,
    updated_at = current_timestamp
WHERE provider IN ('google_calendar', 'google_tasks');

-- The earlier schema allowed revoked rows to retain credentials. Scrub every
-- terminal row before enforcing the stronger credential-state invariant.
UPDATE provider_accounts
SET sync_enabled = false,
    encrypted_credentials = NULL,
    credential_key_version = NULL,
    granted_scopes = '{}',
    token_expires_at = NULL,
    is_default = false,
    disconnected_at = COALESCE(disconnected_at, updated_at, current_timestamp)
WHERE status = 'revoked';

ALTER TABLE provider_accounts ADD CONSTRAINT provider_accounts_provider_check
    CHECK (provider IN ('google', 'google_calendar', 'google_tasks', 'whoop'));
ALTER TABLE provider_accounts ADD CONSTRAINT provider_accounts_status_check
    CHECK (status IN (
        'active', 'reauthorization_required', 'paused', 'disconnecting',
        'revocation_failed', 'operator_recovery_required', 'revoked'
    ));
ALTER TABLE provider_accounts
    ADD CONSTRAINT provider_accounts_credentials_state_check CHECK (
        (status = 'revoked'
            AND encrypted_credentials IS NULL
            AND credential_key_version IS NULL
            AND disconnected_at IS NOT NULL)
        OR
        (status <> 'revoked'
            AND encrypted_credentials IS NOT NULL
            AND credential_key_version IS NOT NULL)
    ),
    ADD CONSTRAINT provider_accounts_disconnect_claim_check CHECK (
        (status = 'disconnecting'
            AND disconnect_claim_id IS NOT NULL
            AND disconnect_claimed_at IS NOT NULL)
        OR
        (status <> 'disconnecting'
            AND disconnect_claim_id IS NULL
            AND disconnect_claimed_at IS NULL)
    ),
    ADD CONSTRAINT provider_accounts_disconnect_operation_check CHECK (
        (status IN ('disconnecting', 'revocation_failed')
            AND disconnect_operation_hash IS NOT NULL
            AND octet_length(disconnect_operation_hash) = 32)
        OR
        (status NOT IN ('disconnecting', 'revocation_failed')
            AND disconnect_operation_hash IS NULL)
    ),
    ADD CONSTRAINT provider_accounts_default_state_check CHECK (
        NOT is_default OR (provider = 'google' AND status <> 'revoked' AND tombstoned_at IS NULL)
    ),
    ADD CONSTRAINT provider_accounts_legacy_recovery_check CHECK (
        provider NOT IN ('google_calendar', 'google_tasks')
        OR (status = 'operator_recovery_required' AND NOT sync_enabled AND NOT is_default)
    );

CREATE UNIQUE INDEX provider_accounts_external_uq
    ON provider_accounts (workspace_id, user_id, provider, external_account_id)
    WHERE external_account_id IS NOT NULL
      AND status <> 'revoked'
      AND tombstoned_at IS NULL;

CREATE UNIQUE INDEX provider_accounts_one_default_google_uq
    ON provider_accounts (workspace_id, user_id, provider)
    WHERE provider = 'google' AND is_default AND status <> 'revoked' AND tombstoned_at IS NULL;

CREATE TABLE google_oauth_sessions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    owner_subject_hash bytea NOT NULL,
    state_hash bytea NOT NULL UNIQUE,
    encrypted_pkce_verifier bytea,
    verifier_key_version integer CHECK (verifier_key_version > 0),
    encrypted_authorization_url bytea,
    authorization_url_key_version integer CHECK (authorization_url_key_version > 0),
    requested_scopes text[] NOT NULL,
    expected_account_id uuid,
    expected_account_revision bigint CHECK (expected_account_revision > 0),
    make_default boolean NOT NULL DEFAULT false,
    status varchar(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'exchanging', 'staged', 'consumed', 'failed')),
    account_id uuid,
    staged_account_id uuid,
    staged_external_account_id varchar(500),
    staged_display_label varchar(200),
    staged_encrypted_credentials bytea,
    staged_credential_key_version integer CHECK (staged_credential_key_version > 0),
    staged_granted_scopes text[],
    staged_token_expires_at timestamptz,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    exchange_started_at timestamptz,
    staged_at timestamptz,
    consumed_at timestamptz,
    failed_at timestamptz,
    UNIQUE (workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, expected_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    FOREIGN KEY (workspace_id, account_id)
        REFERENCES provider_accounts(workspace_id, id),
    CHECK (octet_length(owner_subject_hash) = 32),
    CHECK (octet_length(state_hash) = 32),
    CHECK ((encrypted_authorization_url IS NULL) = (authorization_url_key_version IS NULL)),
    CHECK (status NOT IN ('pending', 'exchanging', 'staged')
        OR (encrypted_authorization_url IS NOT NULL
            AND octet_length(encrypted_authorization_url) >= 36)),
    CHECK (
        (status IN ('pending', 'exchanging')
            AND encrypted_pkce_verifier IS NOT NULL
            AND verifier_key_version IS NOT NULL
            AND octet_length(encrypted_pkce_verifier) >= 36)
        OR
        (status IN ('staged', 'consumed', 'failed')
            AND encrypted_pkce_verifier IS NULL
            AND verifier_key_version IS NULL)
    ),
    CHECK ((expected_account_id IS NULL) = (expected_account_revision IS NULL)),
    CHECK (cardinality(requested_scopes) > 0),
    CHECK (expires_at > created_at),
    CHECK (
        (status = 'staged'
            AND staged_account_id IS NOT NULL
            AND staged_external_account_id IS NOT NULL
            AND btrim(staged_external_account_id) <> ''
            AND staged_display_label IS NOT NULL
            AND btrim(staged_display_label) <> ''
            AND staged_encrypted_credentials IS NOT NULL
            AND octet_length(staged_encrypted_credentials) >= 36
            AND staged_credential_key_version IS NOT NULL
            AND staged_granted_scopes IS NOT NULL
            AND cardinality(staged_granted_scopes) > 0
            AND staged_token_expires_at IS NOT NULL
            AND staged_at IS NOT NULL)
        OR
        (status <> 'staged'
            AND staged_account_id IS NULL
            AND staged_external_account_id IS NULL
            AND staged_display_label IS NULL
            AND staged_encrypted_credentials IS NULL
            AND staged_credential_key_version IS NULL
            AND staged_granted_scopes IS NULL
            AND staged_token_expires_at IS NULL
            AND staged_at IS NULL)
    ),
    CHECK ((status = 'pending') =
        (exchange_started_at IS NULL AND consumed_at IS NULL AND failed_at IS NULL)),
    CHECK (status <> 'exchanging'
        OR (exchange_started_at IS NOT NULL AND consumed_at IS NULL AND failed_at IS NULL)),
    CHECK (status <> 'staged'
        OR (exchange_started_at IS NOT NULL AND consumed_at IS NULL AND failed_at IS NULL)),
    CHECK (status <> 'consumed' OR consumed_at IS NOT NULL),
    CHECK (status <> 'failed' OR failed_at IS NOT NULL)
);

CREATE UNIQUE INDEX google_oauth_sessions_one_open_uq
    ON google_oauth_sessions (workspace_id, user_id)
    WHERE status IN ('pending', 'exchanging', 'staged');

CREATE INDEX google_oauth_sessions_expiry_idx
    ON google_oauth_sessions (workspace_id, user_id, expires_at)
    WHERE status IN ('pending', 'exchanging');

CREATE INDEX google_oauth_sessions_owner_idx
    ON google_oauth_sessions (workspace_id, user_id, owner_subject_hash, created_at DESC);

CREATE TABLE google_oauth_cleanup_tokens (
    session_id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    encrypted_refresh_token bytea NOT NULL,
    key_version integer NOT NULL CHECK (key_version > 0),
    external_account_id varchar(500),
    status varchar(24) NOT NULL DEFAULT 'held'
        CHECK (status IN ('held', 'pending', 'revoking', 'operator_required')),
    claim_id uuid,
    claimed_at timestamptz,
    attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    next_attempt_at timestamptz NOT NULL,
    last_failure_at timestamptz,
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, user_id, session_id)
        REFERENCES google_oauth_sessions(workspace_id, user_id, id),
    CHECK (octet_length(encrypted_refresh_token) >= 36),
    CHECK (external_account_id IS NULL OR btrim(external_account_id) <> ''),
    CHECK (
        (status = 'revoking' AND claim_id IS NOT NULL AND claimed_at IS NOT NULL)
        OR
        (status <> 'revoking' AND claim_id IS NULL AND claimed_at IS NULL)
    )
);

CREATE INDEX google_oauth_cleanup_tokens_retry_idx
    ON google_oauth_cleanup_tokens (workspace_id, user_id, status, next_attempt_at, updated_at);

-- One durable row serializes every Google credential installation against an
-- outbound cleanup revocation for the same workspace/user scope. The
-- generation changes in the same transaction as every credential install.
CREATE TABLE google_oauth_scope_state (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    credential_generation bigint NOT NULL DEFAULT 0 CHECK (credential_generation >= 0),
    revocation_kind varchar(16)
        CHECK (revocation_kind IN ('cleanup', 'disconnect', 'guardian', 'recovery')),
    revocation_owner_id uuid,
    revocation_claim_id uuid,
    revocation_claimed_at timestamptz,
    revocation_generation bigint CHECK (revocation_generation >= 0),
    PRIMARY KEY (workspace_id, user_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (
        (revocation_kind IS NULL
            AND revocation_owner_id IS NULL
            AND revocation_claim_id IS NULL
            AND revocation_claimed_at IS NULL
            AND revocation_generation IS NULL)
        OR
        (revocation_kind IS NOT NULL
            AND revocation_owner_id IS NOT NULL
            AND revocation_claim_id IS NOT NULL
            AND revocation_claimed_at IS NOT NULL
            AND revocation_generation IS NOT NULL)
    )
);

-- A guardian may lose the database acknowledgement after the transaction
-- that clears its fence commits. Keep the exact claim outcome so a retry can
-- distinguish that replay from a mismatched/stolen claim without contacting
-- Google again or abandoning a durable hold.
CREATE TABLE google_oauth_guardian_resolutions (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    session_id uuid NOT NULL,
    claim_id uuid NOT NULL,
    credential_generation bigint NOT NULL CHECK (credential_generation >= 0),
    outcome varchar(16) NOT NULL CHECK (outcome IN ('revoked', 'released')),
    resolved_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, user_id, session_id, claim_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, user_id, session_id)
        REFERENCES google_oauth_sessions(workspace_id, user_id, id)
);

INSERT INTO google_oauth_scope_state (workspace_id, user_id)
SELECT workspace_id, user_id FROM workspace_members
ON CONFLICT (workspace_id, user_id) DO NOTHING;
