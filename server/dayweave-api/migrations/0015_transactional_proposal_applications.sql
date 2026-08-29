-- Transactional, grouped Suggestions Inbox preview/apply/undo evidence. Preview
-- capabilities and idempotency keys are persisted only as SHA-256 hashes. Item
-- snapshots are retained in PostgreSQL for exact undo and must never be copied
-- into general audit metadata, outbox payloads, or idempotency responses.

CREATE TABLE proposal_apply_previews (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    proposal_count smallint NOT NULL CHECK (proposal_count BETWEEN 1 AND 20),
    command_count smallint NOT NULL CHECK (command_count BETWEEN 1 AND 100),
    commands_hash bytea NOT NULL CHECK (octet_length(commands_hash) = 32),
    canonical_hash bytea NOT NULL CHECK (octet_length(canonical_hash) = 32),
    review_content_hash bytea NOT NULL CHECK (octet_length(review_content_hash) = 32),
    preview_hash bytea NOT NULL CHECK (octet_length(preview_hash) = 32),
    can_apply boolean NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    UNIQUE (workspace_id, user_id, id),
    UNIQUE (workspace_id, user_id, id, preview_hash),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= created_at + interval '15 minutes')
);

CREATE INDEX proposal_apply_previews_active_idx
    ON proposal_apply_previews (workspace_id, user_id, expires_at, id);

-- Members are proposals, not normalized commands. Repeating the complete
-- preview scope on every row prevents it from being accidentally joined to a
-- different tenant, user, proposal revision, or payload digest.
CREATE TABLE proposal_apply_preview_members (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    preview_id uuid NOT NULL,
    ordinal smallint NOT NULL CHECK (ordinal BETWEEN 0 AND 19),
    proposal_id uuid NOT NULL,
    proposal_revision bigint NOT NULL
        CHECK (proposal_revision BETWEEN 1 AND 9223372036854775806),
    proposal_payload_hash bytea NOT NULL
        CHECK (octet_length(proposal_payload_hash) = 32),
    PRIMARY KEY (workspace_id, user_id, preview_id, ordinal),
    UNIQUE (workspace_id, user_id, preview_id, proposal_id),
    FOREIGN KEY (workspace_id, user_id, preview_id)
        REFERENCES proposal_apply_previews(workspace_id, user_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, proposal_id)
        REFERENCES proposals(workspace_id, id)
);

CREATE INDEX proposal_apply_preview_members_proposal_idx
    ON proposal_apply_preview_members (workspace_id, proposal_id, preview_id);

CREATE TABLE proposal_applications (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    user_id uuid NOT NULL REFERENCES users(id),
    preview_id uuid NOT NULL,
    preview_hash bytea NOT NULL CHECK (octet_length(preview_hash) = 32),
    status varchar(16) NOT NULL DEFAULT 'applied'
        CHECK (status IN ('applied', 'undone')),
    revision smallint NOT NULL DEFAULT 1 CHECK (revision IN (1, 2)),
    effect_count smallint NOT NULL CHECK (effect_count BETWEEN 1 AND 100),
    fence_count integer NOT NULL CHECK (fence_count > 0),
    apply_audit_id uuid NOT NULL,
    undo_audit_id uuid,
    applied_at timestamptz NOT NULL,
    undo_expires_at timestamptz NOT NULL,
    undone_at timestamptz,
    UNIQUE (workspace_id, user_id, id),
    UNIQUE (workspace_id, user_id, preview_id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, user_id, preview_id, preview_hash)
        REFERENCES proposal_apply_previews(workspace_id, user_id, id, preview_hash),
    FOREIGN KEY (workspace_id, apply_audit_id)
        REFERENCES audit_operations(workspace_id, id),
    FOREIGN KEY (workspace_id, undo_audit_id)
        REFERENCES audit_operations(workspace_id, id),
    CHECK (undo_expires_at > applied_at),
    CHECK (
        (status = 'applied'
            AND revision = 1
            AND undo_audit_id IS NULL
            AND undone_at IS NULL)
        OR
        (status = 'undone'
            AND revision = 2
            AND undo_audit_id IS NOT NULL
            AND undone_at IS NOT NULL
            AND undone_at >= applied_at
            AND undone_at < undo_expires_at)
    )
);

CREATE INDEX proposal_applications_history_idx
    ON proposal_applications (workspace_id, user_id, applied_at DESC, id DESC);

CREATE INDEX proposal_applications_undoable_idx
    ON proposal_applications (workspace_id, user_id, undo_expires_at, id)
    WHERE status = 'applied';

-- A proposal may participate in many expiring previews but in at most one
-- durable application. This claim is separate from mutable proposal status so
-- reconciliation remains unambiguous even under direct-SQL fault injection.
CREATE TABLE proposal_application_members (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    application_id uuid NOT NULL,
    ordinal smallint NOT NULL CHECK (ordinal BETWEEN 0 AND 19),
    proposal_id uuid NOT NULL,
    PRIMARY KEY (workspace_id, user_id, application_id, ordinal),
    UNIQUE (workspace_id, user_id, application_id, proposal_id),
    UNIQUE (workspace_id, user_id, proposal_id),
    FOREIGN KEY (workspace_id, user_id, application_id)
        REFERENCES proposal_applications(workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, proposal_id)
        REFERENCES proposals(workspace_id, id)
);

-- One effect is retained for each direct normalized command. Snapshots stay in
-- this restricted evidence table; ordinary receipts contain hashes and IDs.
CREATE TABLE proposal_application_effects (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    application_id uuid NOT NULL,
    ordinal smallint NOT NULL CHECK (ordinal BETWEEN 0 AND 99),
    action_id uuid NOT NULL,
    operation varchar(24) NOT NULL CHECK (operation IN (
        'create_item', 'replace_item', 'trash_item', 'restore_item'
    )),
    command_hash bytea NOT NULL CHECK (octet_length(command_hash) = 32),
    item_id uuid NOT NULL,
    expected_revision bigint CHECK (expected_revision IS NULL OR expected_revision > 0),
    before_revision bigint CHECK (before_revision IS NULL OR before_revision > 0),
    after_revision bigint NOT NULL CHECK (after_revision > 0),
    before_deleted boolean,
    after_deleted boolean NOT NULL,
    before_snapshot_hash bytea CHECK (
        before_snapshot_hash IS NULL OR octet_length(before_snapshot_hash) = 32
    ),
    after_snapshot_hash bytea NOT NULL CHECK (octet_length(after_snapshot_hash) = 32),
    before_snapshot jsonb,
    after_snapshot jsonb,
    snapshots_scrubbed_at timestamptz,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, user_id, application_id, ordinal),
    UNIQUE (workspace_id, user_id, application_id, action_id),
    UNIQUE (workspace_id, user_id, application_id, item_id),
    FOREIGN KEY (workspace_id, user_id, application_id)
        REFERENCES proposal_applications(workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id),
    CHECK (
        (operation = 'create_item' AND before_snapshot_hash IS NULL)
        OR (operation <> 'create_item' AND before_snapshot_hash IS NOT NULL)
    ),
    CHECK (snapshots_scrubbed_at IS NULL OR snapshots_scrubbed_at >= created_at),
    CHECK (
        (
            snapshots_scrubbed_at IS NULL
            AND after_snapshot IS NOT NULL
            AND jsonb_typeof(after_snapshot) = 'object'
            AND octet_length(after_snapshot::text) <= 1048576
            AND (
                (operation = 'create_item' AND before_snapshot IS NULL)
                OR (
                    operation <> 'create_item'
                    AND before_snapshot IS NOT NULL
                    AND jsonb_typeof(before_snapshot) = 'object'
                    AND octet_length(before_snapshot::text) <= 1048576
                )
            )
        )
        OR (
            snapshots_scrubbed_at IS NOT NULL
            AND before_snapshot IS NULL
            AND after_snapshot IS NULL
        )
    ),
    CHECK (
        (operation = 'create_item'
            AND expected_revision IS NULL
            AND before_revision IS NULL
            AND before_deleted IS NULL
            AND NOT after_deleted)
        OR
        (operation IN ('replace_item', 'trash_item', 'restore_item')
            AND expected_revision IS NOT NULL
            AND expected_revision = before_revision
            AND before_revision IS NOT NULL
            AND before_deleted IS NOT NULL
            AND after_revision > before_revision)
    ),
    CHECK (operation <> 'trash_item' OR (NOT before_deleted AND after_deleted)),
    CHECK (operation <> 'restore_item' OR (before_deleted AND NOT after_deleted)),
    CHECK (operation <> 'replace_item' OR (NOT before_deleted AND NOT after_deleted))
);

-- Fences include every item revision touched by the transaction, including
-- implicit parent refreshes. Undo validates the entire set before changing a
-- single canonical row. The post-undo revision is evidence only and is added
-- during the same transaction as the application state transition.
CREATE TABLE proposal_application_fences (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    application_id uuid NOT NULL,
    item_id uuid NOT NULL,
    applied_revision bigint NOT NULL CHECK (applied_revision > 0),
    applied_deleted boolean NOT NULL,
    undo_revision bigint CHECK (
        undo_revision IS NULL OR undo_revision > applied_revision
    ),
    PRIMARY KEY (workspace_id, user_id, application_id, item_id),
    FOREIGN KEY (workspace_id, user_id, application_id)
        REFERENCES proposal_applications(workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES items(workspace_id, id)
);

-- Applying and undoing use distinct namespaces but identical hash-only
-- receipts. The response is reconstructed from immutable application evidence;
-- no raw key, proposal payload, command, or item snapshot is copied here.
CREATE TABLE proposal_application_requests (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    operation varchar(16) NOT NULL CHECK (operation IN ('apply', 'undo')),
    key_hash bytea NOT NULL CHECK (octet_length(key_hash) = 32),
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    application_id uuid NOT NULL,
    completed_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, user_id, operation, key_hash),
    UNIQUE (workspace_id, user_id, application_id, operation),
    FOREIGN KEY (workspace_id, user_id, application_id)
        REFERENCES proposal_applications(workspace_id, user_id, id),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id)
);

CREATE INDEX proposal_application_requests_application_idx
    ON proposal_application_requests (workspace_id, user_id, application_id);

CREATE FUNCTION reject_proposal_application_evidence_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    RAISE EXCEPTION 'proposal application evidence is immutable'
        USING ERRCODE = '23514';
END
$guard$;

CREATE FUNCTION guard_proposal_apply_preview_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'UPDATE'
       OR OLD.expires_at > clock_timestamp()
       OR EXISTS (
            SELECT 1 FROM proposal_applications
             WHERE workspace_id=OLD.workspace_id
               AND user_id=OLD.user_id
               AND preview_id=OLD.id
       )
    THEN
        RAISE EXCEPTION 'live or applied proposal previews are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$guard$;

CREATE TRIGGER proposal_apply_previews_guard_mutation
    BEFORE UPDATE OR DELETE ON proposal_apply_previews
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_apply_preview_mutation();

CREATE FUNCTION guard_proposal_apply_preview_member_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'UPDATE' OR EXISTS (
        SELECT 1 FROM proposal_apply_previews
         WHERE workspace_id=OLD.workspace_id
           AND user_id=OLD.user_id
           AND id=OLD.preview_id
    ) THEN
        RAISE EXCEPTION 'proposal preview membership is immutable'
            USING ERRCODE = '23514';
    END IF;
    -- The only permitted delete is the FK cascade after an expired,
    -- unapplied preview header passed its own guarded delete.
    RETURN OLD;
END
$guard$;

CREATE TRIGGER proposal_apply_preview_members_guard_mutation
    BEFORE UPDATE OR DELETE ON proposal_apply_preview_members
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_apply_preview_member_mutation();

CREATE FUNCTION guard_proposal_application_effect_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    expiry timestamptz;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.snapshots_scrubbed_at IS NOT NULL THEN
            RAISE EXCEPTION 'new proposal effects require undo snapshots'
                USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'proposal application effects are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF ROW(
        OLD.workspace_id, OLD.user_id, OLD.application_id, OLD.ordinal,
        OLD.action_id, OLD.operation, OLD.command_hash, OLD.item_id,
        OLD.expected_revision, OLD.before_revision, OLD.after_revision,
        OLD.before_deleted, OLD.after_deleted, OLD.before_snapshot_hash,
        OLD.after_snapshot_hash, OLD.created_at
    ) IS DISTINCT FROM ROW(
        NEW.workspace_id, NEW.user_id, NEW.application_id, NEW.ordinal,
        NEW.action_id, NEW.operation, NEW.command_hash, NEW.item_id,
        NEW.expected_revision, NEW.before_revision, NEW.after_revision,
        NEW.before_deleted, NEW.after_deleted, NEW.before_snapshot_hash,
        NEW.after_snapshot_hash, NEW.created_at
    )
       OR OLD.snapshots_scrubbed_at IS NOT NULL
       OR NEW.before_snapshot IS NOT NULL
       OR NEW.after_snapshot IS NOT NULL
    THEN
        RAISE EXCEPTION 'proposal effect permits only one-way snapshot scrubbing'
            USING ERRCODE = '23514';
    END IF;
    SELECT undo_expires_at INTO expiry
      FROM proposal_applications
     WHERE workspace_id = OLD.workspace_id
       AND user_id = OLD.user_id
       AND id = OLD.application_id
     FOR KEY SHARE;
    IF NOT FOUND OR clock_timestamp() < expiry THEN
        RAISE EXCEPTION 'proposal effect snapshots are still required for undo'
            USING ERRCODE = '23514';
    END IF;
    NEW.snapshots_scrubbed_at := clock_timestamp();
    RETURN NEW;
END
$guard$;

CREATE TRIGGER proposal_application_effects_guard_mutation
    BEFORE INSERT OR UPDATE OR DELETE ON proposal_application_effects
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_application_effect_mutation();

CREATE TRIGGER proposal_application_requests_immutable
    BEFORE UPDATE OR DELETE ON proposal_application_requests
    FOR EACH ROW EXECUTE FUNCTION reject_proposal_application_evidence_mutation();

CREATE TRIGGER proposal_application_members_immutable
    BEFORE UPDATE OR DELETE ON proposal_application_members
    FOR EACH ROW EXECUTE FUNCTION reject_proposal_application_evidence_mutation();

CREATE FUNCTION guard_proposal_application_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.status = 'applied'
           AND NEW.revision = 1
           AND NEW.undo_audit_id IS NULL
           AND NEW.undone_at IS NULL
        THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'proposal applications must be inserted as applied'
            USING ERRCODE = '23514';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'proposal applications are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.status = 'applied'
       AND NEW.status = 'undone'
       AND OLD.revision = 1
       AND NEW.revision = 2
       AND OLD.id IS NOT DISTINCT FROM NEW.id
       AND OLD.workspace_id IS NOT DISTINCT FROM NEW.workspace_id
       AND OLD.user_id IS NOT DISTINCT FROM NEW.user_id
       AND OLD.preview_id IS NOT DISTINCT FROM NEW.preview_id
       AND OLD.preview_hash IS NOT DISTINCT FROM NEW.preview_hash
       AND OLD.effect_count IS NOT DISTINCT FROM NEW.effect_count
       AND OLD.fence_count IS NOT DISTINCT FROM NEW.fence_count
       AND OLD.apply_audit_id IS NOT DISTINCT FROM NEW.apply_audit_id
       AND OLD.undo_audit_id IS NULL
       AND NEW.undo_audit_id IS NOT NULL
       AND OLD.applied_at IS NOT DISTINCT FROM NEW.applied_at
       AND OLD.undo_expires_at IS NOT DISTINCT FROM NEW.undo_expires_at
       AND OLD.undone_at IS NULL
       AND NEW.undone_at IS NOT NULL
    THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'proposal applications permit only applied to undone'
        USING ERRCODE = '23514';
END
$guard$;

CREATE TRIGGER proposal_applications_guard_mutation
    BEFORE INSERT OR UPDATE OR DELETE ON proposal_applications
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_application_mutation();

CREATE FUNCTION guard_proposal_application_fence_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'proposal application fences are immutable'
            USING ERRCODE = '23514';
    END IF;
    IF OLD.workspace_id IS NOT DISTINCT FROM NEW.workspace_id
       AND OLD.user_id IS NOT DISTINCT FROM NEW.user_id
       AND OLD.application_id IS NOT DISTINCT FROM NEW.application_id
       AND OLD.item_id IS NOT DISTINCT FROM NEW.item_id
       AND OLD.applied_revision IS NOT DISTINCT FROM NEW.applied_revision
       AND OLD.applied_deleted IS NOT DISTINCT FROM NEW.applied_deleted
       AND OLD.undo_revision IS NULL
       AND NEW.undo_revision IS NOT NULL
    THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'proposal application fences permit only an undo revision'
        USING ERRCODE = '23514';
END
$guard$;

CREATE TRIGGER proposal_application_fences_guard_update
    BEFORE UPDATE OR DELETE ON proposal_application_fences
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_application_fence_update();

-- A preview is valid only as a complete, contiguous 1..20 proposal group. The
-- constraint is deferred so a transaction can insert the immutable header
-- before its immutable members without exposing an incomplete preview.
CREATE FUNCTION validate_proposal_apply_preview_membership() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    scope_workspace_id uuid;
    scope_user_id uuid;
    scope_preview_id uuid;
    expected_count smallint;
    actual_count bigint;
    first_ordinal smallint;
    last_ordinal smallint;
    preview_created_at timestamptz;
    preview_expires_at timestamptz;
BEGIN
    IF TG_OP = 'DELETE' THEN
        scope_workspace_id := OLD.workspace_id;
        scope_user_id := OLD.user_id;
        IF TG_TABLE_NAME = 'proposal_apply_previews' THEN
            scope_preview_id := OLD.id;
        ELSE
            scope_preview_id := OLD.preview_id;
        END IF;
    ELSE
        scope_workspace_id := NEW.workspace_id;
        scope_user_id := NEW.user_id;
        IF TG_TABLE_NAME = 'proposal_apply_previews' THEN
            scope_preview_id := NEW.id;
        ELSE
            scope_preview_id := NEW.preview_id;
        END IF;
    END IF;

    SELECT proposal_count, created_at, expires_at
      INTO expected_count, preview_created_at, preview_expires_at
      FROM proposal_apply_previews
     WHERE workspace_id = scope_workspace_id
       AND user_id = scope_user_id
       AND id = scope_preview_id;
    IF NOT FOUND AND TG_OP = 'DELETE' THEN
        RETURN NULL;
    ELSIF NOT FOUND THEN
        RAISE EXCEPTION 'proposal preview header is missing'
            USING ERRCODE = '23514';
    END IF;

    SELECT COUNT(*), MIN(ordinal), MAX(ordinal)
      INTO actual_count, first_ordinal, last_ordinal
      FROM proposal_apply_preview_members
     WHERE workspace_id = scope_workspace_id
       AND user_id = scope_user_id
       AND preview_id = scope_preview_id;
    IF actual_count <> expected_count
       OR first_ordinal <> 0
       OR last_ordinal <> expected_count - 1
    THEN
        RAISE EXCEPTION 'proposal preview membership is incomplete'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM proposal_apply_preview_members AS member
          JOIN proposals AS proposal
            ON proposal.workspace_id = member.workspace_id
           AND proposal.id = member.proposal_id
         WHERE member.workspace_id = scope_workspace_id
           AND member.user_id = scope_user_id
           AND member.preview_id = scope_preview_id
           AND (
                proposal.revision <> member.proposal_revision
                OR proposal.status <> 'pending'
                OR proposal.trashed_at IS NOT NULL
                OR proposal.tombstoned_at IS NOT NULL
                OR proposal.expires_at <= preview_created_at
                OR proposal.expires_at < preview_expires_at
           )
    ) THEN
        RAISE EXCEPTION 'proposal preview member is stale or unavailable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER proposal_apply_previews_membership_complete
    AFTER INSERT ON proposal_apply_previews
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_apply_preview_membership();

CREATE CONSTRAINT TRIGGER proposal_apply_preview_members_membership_complete
    AFTER INSERT OR DELETE ON proposal_apply_preview_members
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_apply_preview_membership();

-- Applications, direct effects, all-item fences, accepted proposal decisions,
-- and hash-only request receipts form one deferred invariant. Either the whole
-- apply/undo evidence commits, or none of it does.
CREATE FUNCTION validate_proposal_application_evidence() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    scope_workspace_id uuid;
    scope_user_id uuid;
    scope_application_id uuid;
    application_row proposal_applications%ROWTYPE;
    preview_row proposal_apply_previews%ROWTYPE;
    actual_member_count bigint;
    first_member_ordinal smallint;
    last_member_ordinal smallint;
    actual_effect_count bigint;
    first_effect_ordinal smallint;
    last_effect_ordinal smallint;
    actual_fence_count bigint;
    apply_request_count bigint;
    undo_request_count bigint;
BEGIN
    IF TG_TABLE_NAME = 'proposal_applications' THEN
        scope_workspace_id := NEW.workspace_id;
        scope_user_id := NEW.user_id;
        scope_application_id := NEW.id;
    ELSE
        scope_workspace_id := NEW.workspace_id;
        scope_user_id := NEW.user_id;
        scope_application_id := NEW.application_id;
    END IF;

    SELECT * INTO application_row
      FROM proposal_applications
     WHERE workspace_id = scope_workspace_id
       AND user_id = scope_user_id
       AND id = scope_application_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'proposal application header is missing'
            USING ERRCODE = '23514';
    END IF;

    SELECT * INTO preview_row
      FROM proposal_apply_previews
     WHERE workspace_id = application_row.workspace_id
       AND user_id = application_row.user_id
       AND id = application_row.preview_id
       AND preview_hash = application_row.preview_hash;
    IF NOT FOUND
       OR NOT preview_row.can_apply
       OR application_row.applied_at < preview_row.created_at
       OR application_row.applied_at >= preview_row.expires_at
       OR application_row.effect_count <> preview_row.command_count
    THEN
        RAISE EXCEPTION 'proposal application is not bound to a live preview'
            USING ERRCODE = '23514';
    END IF;

    SELECT COUNT(*), MIN(ordinal), MAX(ordinal)
      INTO actual_member_count, first_member_ordinal, last_member_ordinal
      FROM proposal_application_members
     WHERE workspace_id = application_row.workspace_id
       AND user_id = application_row.user_id
       AND application_id = application_row.id;
    IF actual_member_count <> preview_row.proposal_count
       OR first_member_ordinal <> 0
       OR last_member_ordinal <> preview_row.proposal_count - 1
       OR EXISTS (
            SELECT 1
              FROM proposal_application_members AS member
             WHERE member.workspace_id = application_row.workspace_id
               AND member.user_id = application_row.user_id
               AND member.application_id = application_row.id
               AND NOT EXISTS (
                    SELECT 1
                      FROM proposal_apply_preview_members AS preview_member
                     WHERE preview_member.workspace_id = member.workspace_id
                       AND preview_member.user_id = member.user_id
                       AND preview_member.preview_id = application_row.preview_id
                       AND preview_member.ordinal = member.ordinal
                       AND preview_member.proposal_id = member.proposal_id
               )
       )
    THEN
        RAISE EXCEPTION 'proposal application members do not match the reviewed proposals'
            USING ERRCODE = '23514';
    END IF;

    SELECT COUNT(*), MIN(ordinal), MAX(ordinal)
      INTO actual_effect_count, first_effect_ordinal, last_effect_ordinal
      FROM proposal_application_effects
     WHERE workspace_id = application_row.workspace_id
       AND user_id = application_row.user_id
       AND application_id = application_row.id;
    IF actual_effect_count <> application_row.effect_count
       OR first_effect_ordinal <> 0
       OR last_effect_ordinal <> application_row.effect_count - 1
    THEN
        RAISE EXCEPTION 'proposal application effects are incomplete'
            USING ERRCODE = '23514';
    END IF;

    SELECT COUNT(*) INTO actual_fence_count
      FROM proposal_application_fences
     WHERE workspace_id = application_row.workspace_id
       AND user_id = application_row.user_id
       AND application_id = application_row.id;
    IF actual_fence_count <> application_row.fence_count THEN
        RAISE EXCEPTION 'proposal application fences are incomplete'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM proposal_application_effects AS effect
          LEFT JOIN proposal_application_fences AS fence
            ON fence.workspace_id = effect.workspace_id
           AND fence.user_id = effect.user_id
           AND fence.application_id = effect.application_id
           AND fence.item_id = effect.item_id
         WHERE effect.workspace_id = application_row.workspace_id
           AND effect.user_id = application_row.user_id
           AND effect.application_id = application_row.id
           AND (
                fence.item_id IS NULL
                OR fence.applied_revision <> effect.after_revision
                OR fence.applied_deleted <> effect.after_deleted
           )
    ) THEN
        RAISE EXCEPTION 'proposal application effect is missing its exact fence'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM proposal_apply_preview_members AS member
          JOIN proposals AS proposal
            ON proposal.workspace_id = member.workspace_id
           AND proposal.id = member.proposal_id
         WHERE member.workspace_id = application_row.workspace_id
           AND member.user_id = application_row.user_id
           AND member.preview_id = application_row.preview_id
           AND (
                proposal.status <> 'accepted'
                OR proposal.revision <> member.proposal_revision + 1
                OR proposal.decided_at IS NULL
           )
    ) THEN
        RAISE EXCEPTION 'proposal application members were not accepted exactly once'
            USING ERRCODE = '23514';
    END IF;

    SELECT
        COUNT(*) FILTER (WHERE operation = 'apply'),
        COUNT(*) FILTER (WHERE operation = 'undo')
      INTO apply_request_count, undo_request_count
      FROM proposal_application_requests
     WHERE workspace_id = application_row.workspace_id
       AND user_id = application_row.user_id
       AND application_id = application_row.id;
    IF apply_request_count <> 1
       OR (application_row.status = 'applied' AND undo_request_count <> 0)
       OR (application_row.status = 'undone' AND undo_request_count <> 1)
    THEN
        RAISE EXCEPTION 'proposal application request evidence is incomplete'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM proposal_application_requests AS request
         WHERE request.workspace_id = application_row.workspace_id
           AND request.user_id = application_row.user_id
           AND request.application_id = application_row.id
           AND (
                (request.operation = 'apply'
                    AND request.completed_at < application_row.applied_at)
                OR
                (request.operation = 'undo'
                    AND (application_row.undone_at IS NULL
                        OR request.completed_at < application_row.undone_at))
           )
    ) THEN
        RAISE EXCEPTION 'proposal application request evidence predates its result'
            USING ERRCODE = '23514';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM proposal_application_fences AS fence
         WHERE fence.workspace_id = application_row.workspace_id
           AND fence.user_id = application_row.user_id
           AND fence.application_id = application_row.id
           AND (
                (application_row.status = 'applied' AND fence.undo_revision IS NOT NULL)
                OR
                (application_row.status = 'undone' AND fence.undo_revision IS NULL)
           )
    ) THEN
        RAISE EXCEPTION 'proposal application fence state does not match undo state'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$guard$;

CREATE CONSTRAINT TRIGGER proposal_applications_evidence_complete
    AFTER INSERT OR UPDATE ON proposal_applications
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_application_evidence();

CREATE CONSTRAINT TRIGGER proposal_application_effects_evidence_complete
    AFTER INSERT ON proposal_application_effects
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_application_evidence();

CREATE CONSTRAINT TRIGGER proposal_application_members_evidence_complete
    AFTER INSERT ON proposal_application_members
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_application_evidence();

CREATE CONSTRAINT TRIGGER proposal_application_fences_evidence_complete
    AFTER INSERT OR UPDATE ON proposal_application_fences
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_application_evidence();

CREATE CONSTRAINT TRIGGER proposal_application_requests_evidence_complete
    AFTER INSERT ON proposal_application_requests
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION validate_proposal_application_evidence();
