-- Close the publication seal race and make MCP proposal submission exactly
-- once across process restarts. Raw MCP subjects, simulation tokens, and
-- idempotency keys are not stored in the new receipt table.

CREATE TABLE mcp_proposal_submissions (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    subject_hash bytea NOT NULL,
    key_hash bytea NOT NULL,
    request_fingerprint bytea NOT NULL,
    proposal_id uuid NOT NULL,
    completed_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, user_id, subject_hash, key_hash),
    FOREIGN KEY (workspace_id, user_id)
        REFERENCES workspace_members(workspace_id, user_id),
    FOREIGN KEY (workspace_id, proposal_id)
        REFERENCES proposals(workspace_id, id),
    CHECK (octet_length(subject_hash) = 32),
    CHECK (octet_length(key_hash) = 32),
    CHECK (octet_length(request_fingerprint) = 32)
);

CREATE UNIQUE INDEX mcp_proposal_submissions_proposal_uq
    ON mcp_proposal_submissions (workspace_id, proposal_id);

CREATE TRIGGER mcp_proposal_submissions_immutable
    BEFORE UPDATE OR DELETE ON mcp_proposal_submissions
    FOR EACH ROW EXECUTE FUNCTION reject_schedule_content_mutation();

DROP TRIGGER schedule_blocks_immutable ON schedule_blocks;
DROP TRIGGER schedule_revision_details_immutable ON schedule_revision_details;
DROP TRIGGER schedule_revisions_immutable ON schedule_revisions;

CREATE OR REPLACE FUNCTION guard_schedule_content_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    revision_state varchar(24);
    revision_workspace uuid;
    revision_id uuid;
BEGIN
    IF TG_OP = 'UPDATE'
       AND (OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
            OR OLD.schedule_revision_id IS DISTINCT FROM NEW.schedule_revision_id)
    THEN
        RAISE EXCEPTION 'schedule content cannot move between revisions';
    END IF;
    revision_workspace := CASE WHEN TG_OP = 'INSERT' THEN NEW.workspace_id ELSE OLD.workspace_id END;
    revision_id := CASE WHEN TG_OP = 'INSERT' THEN NEW.schedule_revision_id ELSE OLD.schedule_revision_id END;

    SELECT state INTO revision_state
      FROM schedule_revisions
     WHERE workspace_id = revision_workspace AND id = revision_id
     FOR SHARE;
    IF revision_state IS NULL OR revision_state <> 'draft' THEN
        RAISE EXCEPTION 'sealed schedule content is immutable';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END
$guard$;

CREATE TRIGGER schedule_blocks_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON schedule_blocks
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_content_mutation();

CREATE TRIGGER schedule_revision_details_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON schedule_revision_details
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_content_mutation();

CREATE OR REPLACE FUNCTION guard_schedule_revision_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    detail_count bigint;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.state <> 'draft'
           OR NEW.published_at IS NOT NULL
           OR NEW.superseded_at IS NOT NULL
        THEN
            RAISE EXCEPTION 'schedule revisions must be inserted as drafts';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'sealed schedule revisions are immutable';
    END IF;

    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.revision_number IS DISTINCT FROM NEW.revision_number
       OR OLD.parent_revision_id IS DISTINCT FROM NEW.parent_revision_id
       OR OLD.horizon_start IS DISTINCT FROM NEW.horizon_start
       OR OLD.horizon_end IS DISTINCT FROM NEW.horizon_end
       OR OLD.timezone_name IS DISTINCT FROM NEW.timezone_name
       OR OLD.solver_version IS DISTINCT FROM NEW.solver_version
       OR OLD.input_digest IS DISTINCT FROM NEW.input_digest
       OR OLD.publication_hash IS DISTINCT FROM NEW.publication_hash
       OR OLD.created_by_user_id IS DISTINCT FROM NEW.created_by_user_id
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
    THEN
        RAISE EXCEPTION 'sealed schedule revision fields are immutable';
    END IF;

    IF OLD.state = 'draft' AND NEW.state = 'published' THEN
        SELECT COUNT(*) INTO detail_count
          FROM schedule_revision_details
         WHERE workspace_id = OLD.workspace_id
           AND schedule_revision_id = OLD.id;
        IF detail_count <> 1
           OR OLD.published_at IS NOT NULL
           OR NEW.published_at IS NULL
           OR OLD.superseded_at IS NOT NULL
           OR NEW.superseded_at IS NOT NULL
        THEN
            RAISE EXCEPTION 'schedule revision cannot be sealed';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.state = 'published' AND NEW.state = 'superseded' THEN
        IF OLD.published_at IS NULL
           OR NEW.published_at IS DISTINCT FROM OLD.published_at
           OR OLD.superseded_at IS NOT NULL
           OR NEW.superseded_at IS NULL
           OR NEW.superseded_at < OLD.published_at
        THEN
            RAISE EXCEPTION 'published schedule revision cannot be superseded';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.state = 'draft' AND NEW.state = 'discarded' THEN
        IF NEW.published_at IS DISTINCT FROM OLD.published_at
           OR NEW.superseded_at IS DISTINCT FROM OLD.superseded_at
        THEN
            RAISE EXCEPTION 'draft schedule revision cannot be discarded';
        END IF;
        RETURN NEW;
    END IF;

    RAISE EXCEPTION 'sealed schedule revisions are immutable';
END
$guard$;

CREATE TRIGGER schedule_revisions_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON schedule_revisions
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_revision_update();
