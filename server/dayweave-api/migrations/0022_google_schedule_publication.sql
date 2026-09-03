-- Approval-bound publication of immutable generated-schedule blocks to Google
-- Calendar. Provider payloads and identities stay in this dedicated boundary;
-- status and audit surfaces contain only IDs, counts, timestamps, and codes.

-- schedule_block already belongs to the provider mapping kind allowlist. Its
-- local entity is the workspace-bound stable logical slot UUID. Generated
-- active identity columns let the typed origin use deferrable foreign keys
-- across the retire/tombstone transaction.
ALTER TABLE provider_sync_mappings
    ADD COLUMN active_schedule_block_mapping_id uuid GENERATED ALWAYS AS (
        CASE WHEN entity_kind = 'schedule_block'
                   AND ownership = 'dayweave'
                   AND tombstoned_at IS NULL
                   AND sync_state <> 'deleted_remote'
             THEN id END
    ) STORED,
    ADD CONSTRAINT provider_sync_mappings_schedule_identity_uq UNIQUE (
        workspace_id, id, provider_account_id, collection_id,
        entity_kind, local_entity_id, remote_resource_id
    ),
    ADD CONSTRAINT provider_sync_mappings_active_schedule_identity_uq UNIQUE (
        workspace_id, active_schedule_block_mapping_id, provider_account_id,
        collection_id, local_entity_id, remote_resource_id
    ),
    ADD CONSTRAINT provider_sync_mappings_schedule_block_shape_check CHECK (
        entity_kind <> 'schedule_block' OR (
            collection_id IS NOT NULL
            AND local_entity_id IS NOT NULL
            AND ownership = 'dayweave'
            AND remote_etag IS NOT NULL
            AND btrim(remote_etag) <> ''
            AND remote_payload_hash IS NOT NULL
            AND octet_length(remote_payload_hash) = 32
            AND projection_generation IS NULL
            AND NOT provider_forced_sensitive
        )
    );

CREATE TABLE google_schedule_publication_mapping_origins (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    mapping_id uuid,
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    mapping_entity_kind varchar(32) NOT NULL DEFAULT 'schedule_block'
        CHECK (mapping_entity_kind = 'schedule_block'),
    slot_id uuid NOT NULL,
    item_id uuid NOT NULL,
    occurrence_id uuid,
    session_index integer NOT NULL CHECK (session_index BETWEEN 0 AND 65535),
    incarnation bigint NOT NULL CHECK (incarnation BETWEEN 1 AND 4294967295),
    source_schedule_revision_id uuid NOT NULL,
    source_block_id uuid NOT NULL,
    remote_resource_id varchar(1000) NOT NULL,
    last_starts_at timestamptz NOT NULL,
    last_ends_at timestamptz NOT NULL,
    last_desired_hash bytea NOT NULL CHECK (octet_length(last_desired_hash) = 32),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    retired_at timestamptz,
    retirement_reason varchar(32),
    active_mapping_id uuid GENERATED ALWAYS AS (
        CASE WHEN retired_at IS NULL THEN mapping_id END
    ) STORED,
    active_remote_resource_id varchar(1000) GENERATED ALWAYS AS (
        CASE WHEN retired_at IS NULL THEN remote_resource_id END
    ) STORED,
    PRIMARY KEY (workspace_id, mapping_id, incarnation),
    UNIQUE (workspace_id, mapping_id),
    UNIQUE (workspace_id, provider_account_id, collection_id, slot_id, incarnation),
    UNIQUE (
        workspace_id, mapping_id, provider_account_id, collection_id, slot_id, incarnation
    ),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    FOREIGN KEY (workspace_id, provider_account_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, provider_account_id, id),
    FOREIGN KEY (workspace_id, user_id, provider_account_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, user_id, provider_account_id, id),
    FOREIGN KEY (
        workspace_id, mapping_id, provider_account_id, collection_id,
        mapping_entity_kind, slot_id, remote_resource_id
    ) REFERENCES provider_sync_mappings (
        workspace_id, id, provider_account_id, collection_id,
        entity_kind, local_entity_id, remote_resource_id
    ) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (
        workspace_id, active_mapping_id, provider_account_id, collection_id,
        slot_id, active_remote_resource_id
    ) REFERENCES provider_sync_mappings (
        workspace_id, active_schedule_block_mapping_id, provider_account_id,
        collection_id, local_entity_id, remote_resource_id
    ) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, source_schedule_revision_id, source_block_id)
        REFERENCES schedule_blocks(workspace_id, schedule_revision_id, source_block_id),
    CHECK (slot_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (item_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (occurrence_id IS NULL
        OR occurrence_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (btrim(remote_resource_id) <> ''),
    CHECK (last_ends_at > last_starts_at),
    CHECK (updated_at >= created_at),
    CHECK ((retired_at IS NULL) = (retirement_reason IS NULL)),
    CHECK (retired_at IS NULL OR retired_at >= created_at),
    CHECK (retired_at IS NULL OR retired_at <= updated_at),
    CHECK (retirement_reason IS NULL OR retirement_reason IN (
        'provider_deleted', 'identity_conflict', 'operator_retired', 'elapsed_history'
    ))
);

CREATE UNIQUE INDEX google_schedule_publication_origins_active_mapping_uq
    ON google_schedule_publication_mapping_origins(workspace_id, mapping_id)
    WHERE retired_at IS NULL;

CREATE UNIQUE INDEX google_schedule_publication_origins_active_slot_uq
    ON google_schedule_publication_mapping_origins(
        workspace_id, provider_account_id, collection_id, slot_id
    ) WHERE retired_at IS NULL;

CREATE UNIQUE INDEX google_schedule_publication_origins_semantic_incarnation_uq
    ON google_schedule_publication_mapping_origins(
        workspace_id, provider_account_id, collection_id, item_id,
        COALESCE(occurrence_id, '00000000-0000-0000-0000-000000000000'::uuid),
        session_index, incarnation
    );

CREATE INDEX google_schedule_publication_origins_slot_history_idx
    ON google_schedule_publication_mapping_origins(
        workspace_id, provider_account_id, collection_id, slot_id, incarnation DESC
    );

CREATE TABLE google_schedule_publication_previews (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    collection_revision bigint NOT NULL CHECK (collection_revision > 0),
    collection_remote_id varchar(1000) NOT NULL,
    collection_display_name varchar(500) NOT NULL,
    required_scope varchar(200) NOT NULL,
    schedule_revision_id uuid NOT NULL,
    schedule_revision_number bigint NOT NULL CHECK (schedule_revision_number > 0),
    schedule_publication_hash bytea NOT NULL
        CHECK (octet_length(schedule_publication_hash) = 32),
    desired_set_hash bytea NOT NULL CHECK (octet_length(desired_set_hash) = 32),
    timezone_name varchar(100) NOT NULL,
    horizon_start timestamptz NOT NULL,
    horizon_end timestamptz NOT NULL,
    preview_hash bytea NOT NULL CHECK (octet_length(preview_hash) = 32),
    change_count integer NOT NULL CHECK (change_count BETWEEN 0 AND 10000),
    create_count integer NOT NULL CHECK (create_count >= 0),
    update_count integer NOT NULL CHECK (update_count >= 0),
    delete_count integer NOT NULL CHECK (delete_count >= 0),
    noop_count integer NOT NULL CHECK (noop_count >= 0),
    expires_at timestamptz NOT NULL,
    approved_at timestamptz,
    capability_hash bytea UNIQUE
        CHECK (capability_hash IS NULL OR octet_length(capability_hash) = 32),
    approval_audit_id uuid,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (
        workspace_id, user_id, id, provider_account_id,
        collection_id, schedule_revision_id
    ),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    FOREIGN KEY (workspace_id, user_id, provider_account_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, user_id, provider_account_id, id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    FOREIGN KEY (workspace_id, approval_audit_id)
        REFERENCES audit_operations(workspace_id, id),
    CHECK (btrim(collection_remote_id) <> ''),
    CHECK (btrim(collection_display_name) <> ''),
    CHECK (required_scope = 'https://www.googleapis.com/auth/calendar'),
    CHECK (btrim(timezone_name) <> ''),
    CHECK (horizon_end > horizon_start),
    CHECK (change_count = create_count + update_count + delete_count + noop_count),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= created_at + interval '30 minutes'),
    CHECK (updated_at >= created_at),
    CHECK (approved_at IS NULL OR approved_at >= created_at),
    CHECK (consumed_at IS NULL OR consumed_at >= approved_at),
    CHECK (approved_at IS NULL OR approved_at <= updated_at),
    CHECK (consumed_at IS NULL OR consumed_at <= updated_at),
    CHECK (
        (approved_at IS NULL AND capability_hash IS NULL AND approval_audit_id IS NULL)
        OR
        (approved_at IS NOT NULL AND capability_hash IS NOT NULL AND approval_audit_id IS NOT NULL
            AND approved_at < expires_at)
    ),
    CHECK (consumed_at IS NULL OR (approved_at IS NOT NULL AND consumed_at < expires_at))
);

CREATE TABLE google_schedule_publication_preview_changes (
    id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    preview_id uuid NOT NULL,
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    slot_id uuid NOT NULL,
    source_block_id uuid,
    item_id uuid NOT NULL,
    occurrence_id uuid,
    session_index integer NOT NULL CHECK (session_index BETWEEN 0 AND 65535),
    incarnation bigint NOT NULL CHECK (incarnation BETWEEN 1 AND 4294967295),
    operation varchar(16) NOT NULL
        CHECK (operation IN ('create', 'update', 'delete', 'noop')),
    mapping_id uuid,
    remote_resource_id varchar(1000),
    expected_etag varchar(1000),
    desired_payload_hash bytea NOT NULL CHECK (octet_length(desired_payload_hash) = 32),
    intent_hash bytea NOT NULL CHECK (octet_length(intent_hash) = 32),
    provider_payload jsonb NOT NULL CHECK (jsonb_typeof(provider_payload) = 'object'),
    review_summary jsonb NOT NULL CHECK (jsonb_typeof(review_summary) = 'object'),
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, id),
    UNIQUE (workspace_id, preview_id, ordinal),
    UNIQUE (workspace_id, preview_id, slot_id),
    UNIQUE (workspace_id, preview_id, mapping_id),
    UNIQUE (workspace_id, preview_id, id, ordinal),
    UNIQUE (workspace_id, preview_id, id, ordinal, operation),
    FOREIGN KEY (
        workspace_id, user_id, preview_id, provider_account_id,
        collection_id, schedule_revision_id
    ) REFERENCES google_schedule_publication_previews(
        workspace_id, user_id, id, provider_account_id,
        collection_id, schedule_revision_id
    ) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, schedule_revision_id, source_block_id)
        REFERENCES schedule_blocks(workspace_id, schedule_revision_id, source_block_id),
    FOREIGN KEY (
        workspace_id, mapping_id, provider_account_id, collection_id, slot_id, incarnation
    ) REFERENCES google_schedule_publication_mapping_origins(
        workspace_id, mapping_id, provider_account_id, collection_id, slot_id, incarnation
    ),
    CHECK (slot_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (item_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (occurrence_id IS NULL
        OR occurrence_id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CHECK (ends_at > starts_at),
    CHECK (octet_length(provider_payload::text) <= 1048576),
    CHECK (octet_length(review_summary::text) <= 65536),
    CHECK ((remote_resource_id IS NULL) = (expected_etag IS NULL)),
    CHECK (remote_resource_id IS NULL OR btrim(remote_resource_id) <> ''),
    CHECK (expected_etag IS NULL OR btrim(expected_etag) <> ''),
    CHECK (
        (operation = 'create' AND source_block_id IS NOT NULL
            AND mapping_id IS NULL AND remote_resource_id IS NULL)
        OR
        (operation IN ('update', 'noop') AND source_block_id IS NOT NULL
            AND mapping_id IS NOT NULL AND remote_resource_id IS NOT NULL)
        OR
        (operation = 'delete' AND source_block_id IS NULL
            AND mapping_id IS NOT NULL AND remote_resource_id IS NOT NULL)
    )
);

CREATE INDEX google_schedule_publication_previews_active_idx
    ON google_schedule_publication_previews(
        workspace_id, user_id, provider_account_id, expires_at
    ) WHERE consumed_at IS NULL;

CREATE INDEX google_schedule_publication_previews_reuse_idx
    ON google_schedule_publication_previews(
        workspace_id, user_id, provider_account_id, collection_id,
        schedule_revision_id, desired_set_hash, expires_at DESC
    ) WHERE approved_at IS NULL AND consumed_at IS NULL;

CREATE INDEX google_schedule_publication_preview_changes_mapping_idx
    ON google_schedule_publication_preview_changes(
        workspace_id, provider_account_id, collection_id, mapping_id
    ) WHERE mapping_id IS NOT NULL;

CREATE INDEX google_schedule_publication_preview_changes_create_remote_idx
    ON google_schedule_publication_preview_changes(
        workspace_id, user_id, provider_account_id, collection_id,
        ((provider_payload ->> 'id'))
    ) WHERE operation = 'create';

-- The publication/batch ID is deliberately the preview ID. A consumed
-- capability therefore replays only the same exact tuple and receipt.
CREATE TABLE google_schedule_publication_batches (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    provider_account_id uuid NOT NULL,
    collection_id uuid NOT NULL,
    collection_revision bigint NOT NULL CHECK (collection_revision > 0),
    collection_remote_id varchar(1000) NOT NULL,
    required_scope varchar(200) NOT NULL,
    schedule_revision_id uuid NOT NULL,
    schedule_revision_number bigint NOT NULL CHECK (schedule_revision_number > 0),
    schedule_publication_hash bytea NOT NULL
        CHECK (octet_length(schedule_publication_hash) = 32),
    state varchar(32) NOT NULL CHECK (state IN (
        'pending', 'delivering', 'backoff', 'partially_published',
        'published', 'conflict', 'failed', 'superseded'
    )),
    total_count integer NOT NULL CHECK (total_count BETWEEN 0 AND 10000),
    create_count integer NOT NULL CHECK (create_count >= 0),
    update_count integer NOT NULL CHECK (update_count >= 0),
    delete_count integer NOT NULL CHECK (delete_count >= 0),
    noop_count integer NOT NULL CHECK (noop_count >= 0),
    pending_count integer NOT NULL CHECK (pending_count >= 0),
    delivering_count integer NOT NULL CHECK (delivering_count >= 0),
    backoff_count integer NOT NULL CHECK (backoff_count >= 0),
    published_count integer NOT NULL CHECK (published_count >= 0),
    conflict_count integer NOT NULL CHECK (conflict_count >= 0),
    failed_count integer NOT NULL CHECK (failed_count >= 0),
    superseded_count integer NOT NULL CHECK (superseded_count >= 0),
    last_error_code varchar(64),
    last_error_at timestamptz,
    started_at timestamptz,
    completed_at timestamptz,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, user_id, id),
    UNIQUE (workspace_id, user_id, id, provider_account_id, collection_id),
    FOREIGN KEY (
        workspace_id, user_id, id, provider_account_id,
        collection_id, schedule_revision_id
    ) REFERENCES google_schedule_publication_previews(
        workspace_id, user_id, id, provider_account_id,
        collection_id, schedule_revision_id
    ),
    FOREIGN KEY (workspace_id, provider_account_id)
        REFERENCES provider_accounts(workspace_id, id),
    FOREIGN KEY (workspace_id, provider_account_id, collection_id)
        REFERENCES google_sync_collections(workspace_id, provider_account_id, id),
    FOREIGN KEY (workspace_id, schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    CHECK (btrim(collection_remote_id) <> ''),
    CHECK (required_scope = 'https://www.googleapis.com/auth/calendar'),
    CHECK (total_count = create_count + update_count + delete_count + noop_count),
    CHECK (total_count = pending_count + delivering_count + backoff_count
        + published_count + conflict_count + failed_count + superseded_count),
    CHECK ((completed_at IS NULL)
        = (pending_count + delivering_count + backoff_count > 0)),
    CHECK ((last_error_code IS NULL) = (last_error_at IS NULL)),
    CHECK (last_error_code IS NULL OR btrim(last_error_code) <> ''),
    CHECK (updated_at >= created_at),
    CHECK (started_at IS NULL OR started_at >= created_at),
    CHECK (completed_at IS NULL OR completed_at >= created_at),
    CHECK (last_error_at IS NULL OR last_error_at = updated_at),
    CHECK (started_at IS NULL OR started_at <= updated_at),
    CHECK (completed_at IS NULL OR completed_at <= updated_at),
    CHECK (
        (delivering_count > 0 AND state = 'delivering')
        OR (delivering_count = 0 AND pending_count > 0 AND state = 'pending')
        OR (delivering_count = 0 AND pending_count = 0 AND backoff_count > 0
            AND state = 'backoff')
        OR (pending_count + delivering_count + backoff_count = 0
            AND published_count = total_count AND state = 'published')
        OR (pending_count + delivering_count + backoff_count = 0
            AND published_count > 0 AND published_count < total_count
            AND state = 'partially_published')
        OR (pending_count + delivering_count + backoff_count + published_count = 0
            AND conflict_count > 0 AND state = 'conflict')
        OR (pending_count + delivering_count + backoff_count
                + published_count + conflict_count = 0
            AND failed_count > 0 AND state = 'failed')
        OR (total_count > 0 AND total_count = superseded_count AND state = 'superseded')
    )
);

CREATE INDEX google_schedule_publication_batches_status_idx
    ON google_schedule_publication_batches(
        workspace_id, user_id, provider_account_id, updated_at DESC, id DESC
    );

CREATE TABLE google_schedule_publication_outbox (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    publication_id uuid NOT NULL,
    change_id uuid NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    operation varchar(16) NOT NULL
        CHECK (operation IN ('create', 'update', 'delete', 'noop')),
    state varchar(24) NOT NULL CHECK (state IN (
        'pending', 'delivering', 'backoff',
        'published', 'conflict', 'failed', 'superseded'
    )),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claim_id uuid,
    claimed_at timestamptz,
    run_claim_id uuid,
    run_claim_generation bigint CHECK (run_claim_generation IS NULL OR run_claim_generation > 0),
    dispatch_nonce uuid,
    dispatch_provider_write boolean,
    dispatch_authorized_at timestamptz,
    dispatch_expires_at timestamptz,
    provider_post_may_have_started boolean NOT NULL DEFAULT false,
    send_started_at timestamptz,
    no_effect_observation_count integer NOT NULL DEFAULT 0
        CHECK (no_effect_observation_count >= 0),
    last_no_effect_observed_at timestamptz,
    available_at timestamptz NOT NULL,
    last_error_code varchar(64),
    terminal_at timestamptz,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, publication_id, ordinal),
    UNIQUE (workspace_id, id, publication_id, ordinal),
    UNIQUE (workspace_id, id, publication_id, ordinal, user_id),
    FOREIGN KEY (workspace_id, user_id, publication_id)
        REFERENCES google_schedule_publication_batches(workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, publication_id, change_id, ordinal, operation)
        REFERENCES google_schedule_publication_preview_changes(
            workspace_id, preview_id, id, ordinal, operation
        ),
    CHECK ((state = 'delivering') = (
        claim_id IS NOT NULL AND claimed_at IS NOT NULL
        AND run_claim_id IS NOT NULL AND run_claim_generation IS NOT NULL
    )),
    CHECK (
        (dispatch_nonce IS NULL AND dispatch_provider_write IS NULL
            AND dispatch_authorized_at IS NULL AND dispatch_expires_at IS NULL)
        OR
        (state = 'delivering' AND dispatch_nonce IS NOT NULL
            AND dispatch_provider_write IS NOT NULL
            AND dispatch_authorized_at IS NOT NULL AND dispatch_expires_at IS NOT NULL
            AND dispatch_expires_at > dispatch_authorized_at)
    ),
    CHECK (provider_post_may_have_started = (send_started_at IS NOT NULL)),
    CHECK (dispatch_provider_write IS DISTINCT FROM true
        OR provider_post_may_have_started),
    CHECK ((no_effect_observation_count = 0) = (last_no_effect_observed_at IS NULL)),
    CHECK ((terminal_at IS NULL) = (state IN ('pending', 'delivering', 'backoff'))),
    CHECK (operation <> 'noop' OR (
        state = 'published' AND attempts = 0
        AND NOT provider_post_may_have_started AND terminal_at IS NOT NULL
    )),
    CHECK (last_error_code IS NULL OR btrim(last_error_code) <> ''),
    CHECK (updated_at >= created_at),
    CHECK (claimed_at IS NULL OR claimed_at BETWEEN created_at AND updated_at),
    CHECK (dispatch_authorized_at IS NULL
        OR dispatch_authorized_at BETWEEN claimed_at AND updated_at),
    CHECK (send_started_at IS NULL OR send_started_at BETWEEN created_at AND updated_at),
    CHECK (last_no_effect_observed_at IS NULL
        OR last_no_effect_observed_at BETWEEN send_started_at AND updated_at),
    CHECK (terminal_at IS NULL OR terminal_at BETWEEN created_at AND updated_at)
);

CREATE INDEX google_schedule_publication_outbox_delivery_idx
    ON google_schedule_publication_outbox(
        workspace_id, available_at, created_at, id
    ) WHERE state IN ('pending', 'backoff');

CREATE INDEX google_schedule_publication_outbox_batch_idx
    ON google_schedule_publication_outbox(
        workspace_id, publication_id, state, ordinal
    );

CREATE TABLE google_schedule_publication_observations (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    outbox_id uuid NOT NULL,
    publication_id uuid NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    mapping_id uuid,
    incarnation bigint NOT NULL CHECK (incarnation BETWEEN 1 AND 4294967295),
    dispatch_nonce uuid NOT NULL,
    observation_source varchar(32) NOT NULL
        CHECK (observation_source IN ('provider_response', 'reconciliation_read')),
    result_kind varchar(24) NOT NULL
        CHECK (result_kind IN ('event_present', 'event_absent')),
    remote_resource_id varchar(1000) NOT NULL,
    remote_etag varchar(1000),
    remote_updated_at timestamptz,
    payload_hash bytea NOT NULL CHECK (octet_length(payload_hash) = 32),
    schedule_was_current boolean NOT NULL,
    observed_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, outbox_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, outbox_id, publication_id, ordinal, user_id)
        REFERENCES google_schedule_publication_outbox(
            workspace_id, id, publication_id, ordinal, user_id
        ),
    FOREIGN KEY (workspace_id, mapping_id, incarnation)
        REFERENCES google_schedule_publication_mapping_origins(
            workspace_id, mapping_id, incarnation
        ),
    CHECK (btrim(remote_resource_id) <> ''),
    CHECK (remote_etag IS NULL OR btrim(remote_etag) <> ''),
    CHECK (
        (result_kind = 'event_present' AND remote_etag IS NOT NULL)
        OR (result_kind = 'event_absent' AND remote_etag IS NULL)
    )
);

-- Origin identity is immutable. Within one incarnation, only a successful
-- provider observation may advance the exact source schedule block and its
-- semantic desired hash; retirement is one-way.
CREATE FUNCTION guard_google_schedule_publication_origin() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    block_row record;
    expected_incarnation bigint;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Google schedule publication origins are durable';
    END IF;
    IF TG_OP = 'UPDATE' AND (
        OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
        OR OLD.user_id IS DISTINCT FROM NEW.user_id
        OR OLD.mapping_id IS DISTINCT FROM NEW.mapping_id
        OR OLD.provider_account_id IS DISTINCT FROM NEW.provider_account_id
        OR OLD.collection_id IS DISTINCT FROM NEW.collection_id
        OR OLD.mapping_entity_kind IS DISTINCT FROM NEW.mapping_entity_kind
        OR OLD.slot_id IS DISTINCT FROM NEW.slot_id
        OR OLD.item_id IS DISTINCT FROM NEW.item_id
        OR OLD.occurrence_id IS DISTINCT FROM NEW.occurrence_id
        OR OLD.session_index IS DISTINCT FROM NEW.session_index
        OR OLD.incarnation IS DISTINCT FROM NEW.incarnation
        OR OLD.remote_resource_id IS DISTINCT FROM NEW.remote_resource_id
        OR OLD.created_at IS DISTINCT FROM NEW.created_at
        OR NEW.updated_at < OLD.updated_at
        OR (OLD.retired_at IS NOT NULL AND NEW.retired_at IS DISTINCT FROM OLD.retired_at)
        OR (OLD.retirement_reason IS NOT NULL
            AND NEW.retirement_reason IS DISTINCT FROM OLD.retirement_reason)
    ) THEN
        RAISE EXCEPTION 'Google schedule publication origin identity is immutable';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM pg_advisory_xact_lock(hashtextextended(
            NEW.workspace_id::text || ':' || NEW.provider_account_id::text || ':'
                || NEW.collection_id::text || ':' || NEW.slot_id::text,
            0
        ));
        SELECT COALESCE(MAX(origin.incarnation), 0) + 1
          INTO expected_incarnation
          FROM google_schedule_publication_mapping_origins origin
         WHERE origin.workspace_id = NEW.workspace_id
           AND origin.provider_account_id = NEW.provider_account_id
           AND origin.collection_id = NEW.collection_id
           AND origin.slot_id = NEW.slot_id;
        IF NEW.incarnation <> expected_incarnation THEN
            RAISE EXCEPTION 'Google schedule publication incarnation is not monotonic';
        END IF;
    END IF;

    SELECT block.item_id, block.block_kind, block.starts_at, block.ends_at,
           block.constraint_snapshot, revision.state AS revision_state
      INTO block_row
      FROM schedule_blocks block
      JOIN schedule_revisions revision
        ON revision.workspace_id = block.workspace_id
       AND revision.id = block.schedule_revision_id
     WHERE block.workspace_id = NEW.workspace_id
       AND block.schedule_revision_id = NEW.source_schedule_revision_id
       AND block.source_block_id = NEW.source_block_id
     FOR SHARE;
    IF NOT FOUND
       OR block_row.item_id IS DISTINCT FROM NEW.item_id
       OR block_row.revision_state NOT IN ('published', 'superseded')
       OR block_row.block_kind NOT IN ('planned', 'pinned')
       OR block_row.starts_at IS DISTINCT FROM NEW.last_starts_at
       OR block_row.ends_at IS DISTINCT FROM NEW.last_ends_at
       OR block_row.constraint_snapshot ->> 'source_block_id'
            IS DISTINCT FROM NEW.source_block_id::text
       OR block_row.constraint_snapshot ->> 'occurrence_id'
            IS DISTINCT FROM NEW.occurrence_id::text
       OR block_row.constraint_snapshot ->> 'session_index'
            IS DISTINCT FROM NEW.session_index::text
       OR block_row.constraint_snapshot ->> 'core_kind'
            IS DISTINCT FROM block_row.block_kind
    THEN
        RAISE EXCEPTION 'Google schedule publication origin does not match its source block';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER google_schedule_publication_origins_guard
    BEFORE INSERT OR UPDATE OR DELETE
    ON google_schedule_publication_mapping_origins
    FOR EACH ROW EXECUTE FUNCTION guard_google_schedule_publication_origin();

CREATE FUNCTION validate_google_schedule_publication_origin_completeness() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    scope_workspace_id uuid;
BEGIN
    IF TG_OP = 'DELETE' THEN
        scope_workspace_id := OLD.workspace_id;
    ELSE
        scope_workspace_id := NEW.workspace_id;
    END IF;
    IF EXISTS (
        SELECT 1
          FROM provider_sync_mappings mapping
          LEFT JOIN google_schedule_publication_mapping_origins origin
            ON origin.workspace_id = mapping.workspace_id
           AND origin.mapping_id = mapping.id
           AND origin.retired_at IS NULL
         WHERE mapping.workspace_id = scope_workspace_id
           AND mapping.entity_kind = 'schedule_block'
           AND (
                (mapping.active_schedule_block_mapping_id IS NOT NULL
                    AND origin.mapping_id IS NULL)
                OR (mapping.active_schedule_block_mapping_id IS NULL
                    AND origin.mapping_id IS NOT NULL)
           )
    ) THEN
        RAISE EXCEPTION 'Google schedule-block mapping/origin completeness violated';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER provider_schedule_mapping_origin_complete
    AFTER INSERT OR UPDATE OR DELETE ON provider_sync_mappings
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_origin_completeness();

CREATE CONSTRAINT TRIGGER schedule_mapping_origin_mapping_complete
    AFTER INSERT OR UPDATE OR DELETE ON google_schedule_publication_mapping_origins
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_origin_completeness();

CREATE FUNCTION guard_google_schedule_publication_preview() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.consumed_at IS NOT NULL
           OR OLD.expires_at > statement_timestamp()
           OR EXISTS (
                SELECT 1 FROM google_schedule_publication_batches batch
                 WHERE batch.workspace_id = OLD.workspace_id AND batch.id = OLD.id
           )
        THEN
            RAISE EXCEPTION 'non-expired or consumed Google schedule publication previews are durable';
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.provider_account_id IS DISTINCT FROM NEW.provider_account_id
       OR OLD.collection_id IS DISTINCT FROM NEW.collection_id
       OR OLD.collection_revision IS DISTINCT FROM NEW.collection_revision
       OR OLD.collection_remote_id IS DISTINCT FROM NEW.collection_remote_id
       OR OLD.collection_display_name IS DISTINCT FROM NEW.collection_display_name
       OR OLD.required_scope IS DISTINCT FROM NEW.required_scope
       OR OLD.schedule_revision_id IS DISTINCT FROM NEW.schedule_revision_id
       OR OLD.schedule_revision_number IS DISTINCT FROM NEW.schedule_revision_number
       OR OLD.schedule_publication_hash IS DISTINCT FROM NEW.schedule_publication_hash
       OR OLD.desired_set_hash IS DISTINCT FROM NEW.desired_set_hash
       OR OLD.timezone_name IS DISTINCT FROM NEW.timezone_name
       OR OLD.horizon_start IS DISTINCT FROM NEW.horizon_start
       OR OLD.horizon_end IS DISTINCT FROM NEW.horizon_end
       OR OLD.preview_hash IS DISTINCT FROM NEW.preview_hash
       OR OLD.change_count IS DISTINCT FROM NEW.change_count
       OR OLD.create_count IS DISTINCT FROM NEW.create_count
       OR OLD.update_count IS DISTINCT FROM NEW.update_count
       OR OLD.delete_count IS DISTINCT FROM NEW.delete_count
       OR OLD.noop_count IS DISTINCT FROM NEW.noop_count
       OR OLD.expires_at IS DISTINCT FROM NEW.expires_at
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR NEW.updated_at < OLD.updated_at
    THEN
        RAISE EXCEPTION 'Google schedule publication preview content is immutable';
    END IF;
    IF OLD.approved_at IS NULL THEN
        IF NEW.approved_at IS NULL OR NEW.capability_hash IS NULL
           OR NEW.approval_audit_id IS NULL OR NEW.consumed_at IS NOT NULL
        THEN
            RAISE EXCEPTION 'invalid Google schedule publication approval transition';
        END IF;
    ELSIF OLD.consumed_at IS NULL THEN
        IF NEW.approved_at IS DISTINCT FROM OLD.approved_at
           OR NEW.capability_hash IS DISTINCT FROM OLD.capability_hash
           OR NEW.approval_audit_id IS DISTINCT FROM OLD.approval_audit_id
           OR NEW.consumed_at IS NULL
        THEN
            RAISE EXCEPTION 'invalid Google schedule publication consumption transition';
        END IF;
    ELSE
        RAISE EXCEPTION 'consumed Google schedule publication preview is immutable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER google_schedule_publication_previews_guard
    BEFORE UPDATE OR DELETE ON google_schedule_publication_previews
    FOR EACH ROW EXECUTE FUNCTION guard_google_schedule_publication_preview();

CREATE FUNCTION guard_google_schedule_publication_content_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' AND EXISTS (
        SELECT 1 FROM google_schedule_publication_previews preview
         WHERE preview.workspace_id = OLD.workspace_id
           AND preview.user_id = OLD.user_id
           AND preview.id = OLD.preview_id
           AND preview.provider_account_id = OLD.provider_account_id
           AND preview.collection_id = OLD.collection_id
           AND preview.schedule_revision_id = OLD.schedule_revision_id
           AND preview.consumed_at IS NULL
           AND preview.expires_at <= statement_timestamp()
           AND NOT EXISTS (
                SELECT 1 FROM google_schedule_publication_batches batch
                 WHERE batch.workspace_id = preview.workspace_id
                   AND batch.id = preview.id
           )
    ) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'Google schedule publication content is immutable';
END
$guard$;

CREATE TRIGGER google_schedule_publication_preview_changes_immutable
    BEFORE UPDATE OR DELETE ON google_schedule_publication_preview_changes
    FOR EACH ROW EXECUTE FUNCTION guard_google_schedule_publication_content_mutation();

CREATE FUNCTION validate_google_schedule_publication_preview_complete() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    scope_workspace_id uuid;
    scope_preview_id uuid;
    header record;
    actual record;
BEGIN
    scope_workspace_id := NEW.workspace_id;
    IF TG_TABLE_NAME = 'google_schedule_publication_previews' THEN
        scope_preview_id := NEW.id;
    ELSE
        scope_preview_id := NEW.preview_id;
    END IF;
    SELECT preview.change_count, preview.create_count, preview.update_count,
           preview.delete_count, preview.noop_count
      INTO header
      FROM google_schedule_publication_previews preview
     WHERE preview.workspace_id = scope_workspace_id AND preview.id = scope_preview_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    SELECT count(*)::integer AS change_count,
           count(*) FILTER (WHERE operation = 'create')::integer AS create_count,
           count(*) FILTER (WHERE operation = 'update')::integer AS update_count,
           count(*) FILTER (WHERE operation = 'delete')::integer AS delete_count,
           count(*) FILTER (WHERE operation = 'noop')::integer AS noop_count,
           min(ordinal) AS min_ordinal, max(ordinal) AS max_ordinal
      INTO actual
      FROM google_schedule_publication_preview_changes change
     WHERE change.workspace_id = scope_workspace_id AND change.preview_id = scope_preview_id;
    IF actual.change_count <> header.change_count
       OR actual.create_count <> header.create_count
       OR actual.update_count <> header.update_count
       OR actual.delete_count <> header.delete_count
       OR actual.noop_count <> header.noop_count
       OR (actual.change_count > 0 AND (
            actual.min_ordinal <> 0 OR actual.max_ordinal <> actual.change_count - 1
       ))
    THEN
        RAISE EXCEPTION 'Google schedule publication preview children are incomplete';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER google_schedule_publication_preview_header_complete
    AFTER INSERT ON google_schedule_publication_previews
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_preview_complete();

CREATE CONSTRAINT TRIGGER google_schedule_publication_preview_changes_complete
    AFTER INSERT ON google_schedule_publication_preview_changes
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_preview_complete();

CREATE FUNCTION guard_google_schedule_publication_batch() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Google schedule publication batches are durable';
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.provider_account_id IS DISTINCT FROM NEW.provider_account_id
       OR OLD.collection_id IS DISTINCT FROM NEW.collection_id
       OR OLD.collection_revision IS DISTINCT FROM NEW.collection_revision
       OR OLD.collection_remote_id IS DISTINCT FROM NEW.collection_remote_id
       OR OLD.required_scope IS DISTINCT FROM NEW.required_scope
       OR OLD.schedule_revision_id IS DISTINCT FROM NEW.schedule_revision_id
       OR OLD.schedule_revision_number IS DISTINCT FROM NEW.schedule_revision_number
       OR OLD.schedule_publication_hash IS DISTINCT FROM NEW.schedule_publication_hash
       OR OLD.total_count IS DISTINCT FROM NEW.total_count
       OR OLD.create_count IS DISTINCT FROM NEW.create_count
       OR OLD.update_count IS DISTINCT FROM NEW.update_count
       OR OLD.delete_count IS DISTINCT FROM NEW.delete_count
       OR OLD.noop_count IS DISTINCT FROM NEW.noop_count
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR NEW.updated_at < OLD.updated_at
    THEN
        RAISE EXCEPTION 'Google schedule publication batch identity is immutable';
    END IF;
    IF OLD.state IN ('partially_published', 'published', 'conflict', 'failed', 'superseded')
       AND NEW IS DISTINCT FROM OLD
    THEN
        RAISE EXCEPTION 'terminal Google schedule publication batch is immutable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER google_schedule_publication_batches_guard
    BEFORE UPDATE OR DELETE ON google_schedule_publication_batches
    FOR EACH ROW EXECUTE FUNCTION guard_google_schedule_publication_batch();

CREATE FUNCTION guard_google_schedule_publication_outbox() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    dispatch_deadline timestamptz;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Google schedule publication outbox evidence is durable';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.attempts <> 0
           OR NEW.no_effect_observation_count <> 0
           OR NEW.last_no_effect_observed_at IS NOT NULL
           OR NEW.last_error_code IS NOT NULL
           OR (
                NEW.operation <> 'noop'
                AND (
                    NEW.state <> 'pending'
                    OR NEW.claim_id IS NOT NULL
                    OR NEW.dispatch_nonce IS NOT NULL
                    OR NEW.provider_post_may_have_started
                    OR NEW.terminal_at IS NOT NULL
                )
           )
        THEN
            RAISE EXCEPTION 'invalid initial Google schedule publication outbox state';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.dispatch_provider_write THEN
        SELECT CASE
                   WHEN change.operation = 'create' THEN change.ends_at
                   ELSE LEAST(change.ends_at, origin.last_ends_at)
               END
          INTO dispatch_deadline
          FROM google_schedule_publication_preview_changes change
          LEFT JOIN google_schedule_publication_mapping_origins origin
            ON origin.workspace_id = change.workspace_id
           AND origin.mapping_id = change.mapping_id
           AND origin.provider_account_id = change.provider_account_id
           AND origin.collection_id = change.collection_id
           AND origin.slot_id = change.slot_id
           AND origin.incarnation = change.incarnation
         WHERE change.workspace_id = NEW.workspace_id
           AND change.id = NEW.change_id
           AND change.preview_id = NEW.publication_id
           AND change.ordinal = NEW.ordinal
           AND change.operation = NEW.operation;
        IF dispatch_deadline IS NULL
           OR NEW.dispatch_authorized_at >= dispatch_deadline
           OR NEW.dispatch_expires_at > dispatch_deadline
        THEN
            RAISE EXCEPTION 'Google schedule publication write permit exceeds block lifetime';
        END IF;
    END IF;
    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.publication_id IS DISTINCT FROM NEW.publication_id
       OR OLD.change_id IS DISTINCT FROM NEW.change_id
       OR OLD.ordinal IS DISTINCT FROM NEW.ordinal
       OR OLD.operation IS DISTINCT FROM NEW.operation
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR NEW.updated_at < OLD.updated_at
       OR NEW.attempts < OLD.attempts
       OR NEW.no_effect_observation_count < OLD.no_effect_observation_count
       OR (
            NEW.no_effect_observation_count = OLD.no_effect_observation_count
            AND NEW.last_no_effect_observed_at IS DISTINCT FROM OLD.last_no_effect_observed_at
       )
       OR (
            NEW.no_effect_observation_count <> OLD.no_effect_observation_count
            AND NOT (
                OLD.state = 'delivering'
                AND NEW.state = 'backoff'
                AND OLD.provider_post_may_have_started
                AND NEW.provider_post_may_have_started
                AND NEW.no_effect_observation_count = OLD.no_effect_observation_count + 1
                AND NEW.last_no_effect_observed_at IS NOT DISTINCT FROM NEW.updated_at
            )
       )
       OR (OLD.dispatch_nonce IS NOT NULL
           AND NEW.state = 'delivering'
           AND (
                NEW.dispatch_nonce IS DISTINCT FROM OLD.dispatch_nonce
                OR NEW.dispatch_provider_write IS DISTINCT FROM OLD.dispatch_provider_write
                OR NEW.dispatch_authorized_at IS DISTINCT FROM OLD.dispatch_authorized_at
                OR NEW.dispatch_expires_at IS DISTINCT FROM OLD.dispatch_expires_at
           ))
       OR (OLD.provider_post_may_have_started
           AND NOT NEW.provider_post_may_have_started
           AND NOT (
                OLD.state = 'delivering'
                AND NEW.state = 'backoff'
                AND OLD.dispatch_nonce IS NOT NULL
                AND OLD.dispatch_expires_at IS NOT NULL
                AND OLD.dispatch_expires_at <= NEW.updated_at
                AND NEW.send_started_at IS NULL
           ))
       OR (NOT OLD.provider_post_may_have_started
           AND NEW.provider_post_may_have_started
           AND NOT (
                OLD.state = 'delivering'
                AND NEW.state = 'delivering'
                AND OLD.dispatch_nonce IS NULL
                AND NEW.dispatch_nonce IS NOT NULL
                AND NEW.dispatch_provider_write
                AND NEW.send_started_at IS NOT DISTINCT FROM NEW.dispatch_authorized_at
           ))
    THEN
        RAISE EXCEPTION 'Google schedule publication outbox identity is immutable';
    END IF;
    IF NOT (
        (OLD.state = 'pending'
            AND NEW.state IN ('pending', 'delivering', 'conflict', 'failed', 'superseded'))
        OR (OLD.state = 'backoff'
            AND NEW.state IN ('backoff', 'delivering', 'conflict', 'failed', 'superseded'))
        OR (OLD.state = 'delivering'
            AND NEW.state IN ('delivering', 'backoff', 'published', 'conflict', 'failed', 'superseded'))
        OR (OLD.state IN ('published', 'conflict', 'failed', 'superseded')
            AND NEW IS NOT DISTINCT FROM OLD)
    ) THEN
        RAISE EXCEPTION 'invalid Google schedule publication outbox transition';
    END IF;
    IF (NEW.state = 'published'
            OR (NEW.state = 'superseded' AND NEW.provider_post_may_have_started))
       AND NEW.operation <> 'noop'
       AND NOT EXISTS (
            SELECT 1 FROM google_schedule_publication_observations observation
             WHERE observation.workspace_id = NEW.workspace_id
               AND observation.outbox_id = NEW.id
       )
    THEN
        RAISE EXCEPTION 'provider-observed schedule publication cannot lose its observation';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER google_schedule_publication_outbox_guard
    BEFORE INSERT OR UPDATE OR DELETE ON google_schedule_publication_outbox
    FOR EACH ROW EXECUTE FUNCTION guard_google_schedule_publication_outbox();

CREATE FUNCTION reject_google_schedule_publication_observation_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    RAISE EXCEPTION 'Google schedule publication observations are immutable';
END
$guard$;

CREATE TRIGGER google_schedule_publication_observations_immutable
    BEFORE UPDATE OR DELETE ON google_schedule_publication_observations
    FOR EACH ROW EXECUTE FUNCTION reject_google_schedule_publication_observation_mutation();

CREATE FUNCTION validate_google_schedule_publication_observation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    expected record;
BEGIN
    SELECT outbox.dispatch_nonce, outbox.dispatch_provider_write,
           outbox.dispatch_authorized_at, outbox.state,
           outbox.operation, outbox.user_id, outbox.provider_post_may_have_started,
           change.mapping_id, change.incarnation, change.remote_resource_id,
           change.expected_etag, change.provider_payload, change.provider_account_id,
           change.collection_id, change.slot_id, change.item_id,
           change.occurrence_id, change.session_index
      INTO expected
      FROM google_schedule_publication_outbox outbox
      JOIN google_schedule_publication_preview_changes change
        ON change.workspace_id = outbox.workspace_id
       AND change.id = outbox.change_id
     WHERE outbox.workspace_id = NEW.workspace_id
       AND outbox.id = NEW.outbox_id
       AND outbox.publication_id = NEW.publication_id
       AND outbox.ordinal = NEW.ordinal
     FOR SHARE OF outbox;
    IF NOT FOUND OR expected.state <> 'delivering'
       OR expected.dispatch_nonce IS DISTINCT FROM NEW.dispatch_nonce
       OR expected.user_id IS DISTINCT FROM NEW.user_id
       OR NEW.observed_at < expected.dispatch_authorized_at
       OR (NEW.observation_source = 'provider_response'
            AND NOT expected.dispatch_provider_write)
       OR (expected.operation <> 'create'
            AND NEW.remote_resource_id IS DISTINCT FROM expected.remote_resource_id)
       OR (expected.mapping_id IS NOT NULL
            AND expected.mapping_id IS DISTINCT FROM NEW.mapping_id)
       OR expected.incarnation IS DISTINCT FROM NEW.incarnation
       OR NEW.mapping_id IS NULL
       OR (NEW.mapping_id IS NOT NULL AND NOT EXISTS (
            SELECT 1
              FROM google_schedule_publication_mapping_origins origin
             WHERE origin.workspace_id = NEW.workspace_id
               AND origin.mapping_id = NEW.mapping_id
               AND origin.incarnation = NEW.incarnation
               AND origin.provider_account_id = expected.provider_account_id
               AND origin.collection_id = expected.collection_id
               AND origin.slot_id = expected.slot_id
               AND origin.item_id = expected.item_id
               AND origin.occurrence_id IS NOT DISTINCT FROM expected.occurrence_id
               AND origin.session_index = expected.session_index
               AND origin.remote_resource_id = NEW.remote_resource_id
       ))
    THEN
        RAISE EXCEPTION 'Google schedule publication observation is not dispatch-bound';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER google_schedule_publication_observations_validate
    BEFORE INSERT ON google_schedule_publication_observations
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_observation();

CREATE FUNCTION validate_google_schedule_publication_batch_aggregate() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    scope_workspace_id uuid;
    scope_publication_id uuid;
    batch_row record;
    actual record;
BEGIN
    scope_workspace_id := NEW.workspace_id;
    IF TG_TABLE_NAME = 'google_schedule_publication_batches' THEN
        scope_publication_id := NEW.id;
    ELSE
        scope_publication_id := NEW.publication_id;
    END IF;
    SELECT * INTO batch_row
      FROM google_schedule_publication_batches batch
     WHERE batch.workspace_id = scope_workspace_id
       AND batch.id = scope_publication_id;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    SELECT count(*)::integer AS total_count,
           count(*) FILTER (WHERE operation = 'create')::integer AS create_count,
           count(*) FILTER (WHERE operation = 'update')::integer AS update_count,
           count(*) FILTER (WHERE operation = 'delete')::integer AS delete_count,
           count(*) FILTER (WHERE operation = 'noop')::integer AS noop_count,
           count(*) FILTER (WHERE state = 'pending')::integer AS pending_count,
           count(*) FILTER (WHERE state = 'delivering')::integer AS delivering_count,
           count(*) FILTER (WHERE state = 'backoff')::integer AS backoff_count,
           count(*) FILTER (WHERE state = 'published')::integer AS published_count,
           count(*) FILTER (WHERE state = 'conflict')::integer AS conflict_count,
           count(*) FILTER (WHERE state = 'failed')::integer AS failed_count,
           count(*) FILTER (WHERE state = 'superseded')::integer AS superseded_count,
           (array_agg(last_error_code ORDER BY updated_at DESC, id DESC)
                FILTER (WHERE last_error_code IS NOT NULL))[1] AS last_error_code
      INTO actual
      FROM google_schedule_publication_outbox outbox
     WHERE outbox.workspace_id = scope_workspace_id
       AND outbox.publication_id = scope_publication_id;
    IF (batch_row.total_count, batch_row.create_count, batch_row.update_count,
        batch_row.delete_count, batch_row.noop_count, batch_row.pending_count,
        batch_row.delivering_count, batch_row.backoff_count, batch_row.published_count,
        batch_row.conflict_count, batch_row.failed_count, batch_row.superseded_count,
        batch_row.last_error_code)
       IS DISTINCT FROM
       (actual.total_count, actual.create_count, actual.update_count,
        actual.delete_count, actual.noop_count, actual.pending_count,
        actual.delivering_count, actual.backoff_count, actual.published_count,
        actual.conflict_count, actual.failed_count, actual.superseded_count,
        actual.last_error_code)
    THEN
        RAISE EXCEPTION 'Google schedule publication batch aggregate is inconsistent';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER google_schedule_publication_batch_aggregate_exact
    AFTER INSERT OR UPDATE ON google_schedule_publication_batches
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_batch_aggregate();

CREATE CONSTRAINT TRIGGER google_schedule_publication_outbox_batch_aggregate_exact
    AFTER INSERT OR UPDATE ON google_schedule_publication_outbox
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_google_schedule_publication_batch_aggregate();
