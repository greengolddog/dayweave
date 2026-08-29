-- Durable, content-bound Google outbound approvals and explicit Calendar
-- projection/publication policy. Approval capabilities are stored only as
-- SHA-256 hashes; provider-visible payloads remain inside the existing
-- sensitive PostgreSQL boundary.

ALTER TABLE google_sync_collections
    ADD COLUMN confirmed_busy_policy varchar(24) NOT NULL DEFAULT 'blocking'
        CHECK (confirmed_busy_policy IN ('ignore', 'visible_nonblocking', 'blocking')),
    ADD COLUMN tentative_policy varchar(24) NOT NULL DEFAULT 'visible_nonblocking'
        CHECK (tentative_policy IN ('ignore', 'visible_nonblocking', 'blocking')),
    ADD COLUMN free_policy varchar(24) NOT NULL DEFAULT 'visible_nonblocking'
        CHECK (free_policy IN ('ignore', 'visible_nonblocking', 'blocking')),
    ADD COLUMN all_day_policy varchar(24) NOT NULL DEFAULT 'visible_nonblocking'
        CHECK (all_day_policy IN ('ignore', 'visible_nonblocking', 'blocking')),
    ADD COLUMN publish_all_day boolean NOT NULL DEFAULT false,
    ADD COLUMN publish_tentative boolean NOT NULL DEFAULT false,
    ADD COLUMN publish_free boolean NOT NULL DEFAULT false;

-- The provider-ID HMAC root is deliberately independent from rotating
-- credential encryption. Only a domain-separated one-way verifier is stored;
-- startup refuses outbound publication if either the configured version or
-- key bytes drift after this binding is first established.
CREATE TABLE google_provider_identity_roots (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    provider varchar(32) NOT NULL CHECK (provider = 'google'),
    identity_key_version bigint NOT NULL
        CHECK (identity_key_version BETWEEN 1 AND 4294967295),
    root_verifier bytea NOT NULL CHECK (octet_length(root_verifier) = 32),
    created_at timestamptz NOT NULL,
    last_verified_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, user_id, provider),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id)
);

-- A monotonic generation prevents claim-ID reuse (ABA) from reviving work
-- owned by an earlier sync run. It changes only when a new parent run claims
-- the account, not on heartbeats or ordinary run metadata updates.
ALTER TABLE google_sync_runs
    ADD COLUMN claim_generation bigint NOT NULL DEFAULT 0
        CHECK (claim_generation >= 0);

CREATE TABLE google_outbound_previews (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    collection_revision bigint NOT NULL CHECK (collection_revision > 0),
    collection_remote_id varchar(1000) NOT NULL,
    item_id uuid NOT NULL,
    item_revision bigint NOT NULL CHECK (item_revision > 0),
    entity_kind varchar(24) NOT NULL CHECK (entity_kind IN ('calendar_event', 'task')),
    operation varchar(16) NOT NULL CHECK (operation IN ('upsert', 'delete')),
    required_scope varchar(200) NOT NULL,
    provider_resource_id varchar(1000),
    expected_etag varchar(1000),
    intent_hash bytea NOT NULL CHECK (octet_length(intent_hash) = 32),
    preview_hash bytea NOT NULL CHECK (octet_length(preview_hash) = 32),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    expires_at timestamptz NOT NULL,
    approved_at timestamptz,
    capability_hash bytea CHECK (capability_hash IS NULL OR octet_length(capability_hash) = 32),
    consumed_at timestamptz,
    outbox_id uuid,
    approval_audit_id uuid,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (capability_hash),
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
    CHECK (btrim(collection_remote_id) <> ''),
    CHECK (btrim(required_scope) <> ''),
    CHECK ((provider_resource_id IS NULL) = (expected_etag IS NULL)),
    CHECK (provider_resource_id IS NULL OR btrim(provider_resource_id) <> ''),
    CHECK (expected_etag IS NULL OR btrim(expected_etag) <> ''),
    CHECK (operation <> 'delete' OR provider_resource_id IS NOT NULL),
    CHECK ((approved_at IS NULL) = (capability_hash IS NULL)),
    CHECK (consumed_at IS NULL OR (approved_at IS NOT NULL AND outbox_id IS NOT NULL)),
    CHECK (approval_audit_id IS NULL OR approved_at IS NOT NULL)
);

CREATE INDEX google_outbound_previews_expiry_idx
    ON google_outbound_previews (workspace_id, expires_at, id)
    WHERE consumed_at IS NULL;

ALTER TABLE google_sync_outbox
    ADD COLUMN approval_id uuid,
    ADD COLUMN intent_hash bytea,
    ADD COLUMN collection_revision bigint,
    ADD COLUMN target_remote_collection_id varchar(1000),
    ADD COLUMN required_scope varchar(200),
    ADD COLUMN run_claim_id uuid,
    ADD COLUMN run_claim_generation bigint,
    ADD COLUMN dispatch_nonce uuid,
    ADD COLUMN dispatch_authorized_at timestamptz,
    ADD COLUMN dispatch_expires_at timestamptz,
    ADD COLUMN provider_post_may_have_started boolean NOT NULL DEFAULT false,
    ADD COLUMN send_started_at timestamptz;

-- Fence any delivery owned by a pre-generation binary before validating the
-- new state invariant. After this migration an old binary cannot transition a
-- row back to `delivering`, because it does not populate the parent-run fields.
UPDATE google_sync_outbox
SET state = 'backoff', claim_id = NULL, claimed_at = NULL,
    run_claim_id = NULL, run_claim_generation = NULL,
    dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL,
    available_at = now(), last_error_code = 'outbound_worker_upgrade_required',
    updated_at = now()
WHERE state = 'delivering';

ALTER TABLE google_sync_outbox
    ADD CONSTRAINT google_sync_outbox_approval_fk
        FOREIGN KEY (workspace_id, approval_id)
        REFERENCES google_outbound_previews(workspace_id, id),
    ADD CONSTRAINT google_sync_outbox_intent_hash_check
        CHECK (intent_hash IS NULL OR octet_length(intent_hash) = 32),
    ADD CONSTRAINT google_sync_outbox_collection_revision_check
        CHECK (collection_revision IS NULL OR collection_revision > 0),
    ADD CONSTRAINT google_sync_outbox_target_check
        CHECK (target_remote_collection_id IS NULL OR btrim(target_remote_collection_id) <> ''),
    ADD CONSTRAINT google_sync_outbox_scope_check
        CHECK (required_scope IS NULL OR btrim(required_scope) <> ''),
    ADD CONSTRAINT google_sync_outbox_run_claim_check
        CHECK ((state = 'delivering') = (run_claim_id IS NOT NULL
                AND run_claim_generation IS NOT NULL)),
    ADD CONSTRAINT google_sync_outbox_run_generation_check
        CHECK (run_claim_generation IS NULL OR run_claim_generation >= 1),
    ADD CONSTRAINT google_sync_outbox_dispatch_lease_check
        CHECK ((dispatch_nonce IS NULL AND dispatch_authorized_at IS NULL
                AND dispatch_expires_at IS NULL)
            OR (dispatch_nonce IS NOT NULL AND dispatch_authorized_at IS NOT NULL
                AND dispatch_expires_at IS NOT NULL
                AND dispatch_expires_at > dispatch_authorized_at));

ALTER TABLE google_sync_outbox
    ADD CONSTRAINT google_sync_outbox_send_start_check
        CHECK (provider_post_may_have_started = (send_started_at IS NOT NULL));

ALTER TABLE google_sync_outbox
    ADD CONSTRAINT google_sync_outbox_approval_binding_check
        CHECK ((approval_id IS NULL AND intent_hash IS NULL AND collection_revision IS NULL
                AND target_remote_collection_id IS NULL AND required_scope IS NULL)
            OR (approval_id IS NOT NULL AND intent_hash IS NOT NULL AND collection_revision IS NOT NULL
                AND target_remote_collection_id IS NOT NULL AND required_scope IS NOT NULL));

-- Rows from a pre-capability build can never pass the new dispatch fence. Make
-- their terminal state explicit without deleting potentially useful evidence.
UPDATE google_sync_outbox
SET state = 'conflict', claim_id = NULL, claimed_at = NULL,
    run_claim_id = NULL, run_claim_generation = NULL,
    dispatch_nonce = NULL, dispatch_authorized_at = NULL, dispatch_expires_at = NULL,
    last_error_code = 'legacy_unapproved_outbound', updated_at = now()
WHERE approval_id IS NULL AND state IN ('pending', 'delivering', 'backoff');

CREATE UNIQUE INDEX google_sync_outbox_approval_uq
    ON google_sync_outbox (workspace_id, approval_id)
    WHERE approval_id IS NOT NULL;

ALTER TABLE google_outbound_previews
    ADD CONSTRAINT google_outbound_previews_outbox_fk
        FOREIGN KEY (outbox_id) REFERENCES google_sync_outbox(id);
