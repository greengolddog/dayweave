-- Make simulation-backed MCP proposals provable after their short-lived
-- capability rows are pruned. Existing simulation tokens deliberately expire
-- at migration time rather than being upgraded without compilation evidence.

DELETE FROM schedule_simulations;

ALTER TABLE schedule_simulations
    ADD COLUMN evidence_schema smallint NOT NULL,
    ADD COLUMN request_hash bytea NOT NULL,
    ADD COLUMN evidence_hash bytea NOT NULL,
    ADD COLUMN compilation_outcome varchar(24) NOT NULL,
    ADD COLUMN compiled_payload_hash bytea,
    ADD CONSTRAINT schedule_simulations_evidence_schema_check
        CHECK (evidence_schema = 1),
    ADD CONSTRAINT schedule_simulations_request_hash_check
        CHECK (octet_length(request_hash) = 32),
    ADD CONSTRAINT schedule_simulations_digest_prefix_check
        CHECK (request_digest = substring(request_hash FROM 1 FOR 16)),
    ADD CONSTRAINT schedule_simulations_evidence_hash_check
        CHECK (octet_length(evidence_hash) = 32),
    ADD CONSTRAINT schedule_simulations_compilation_check
        CHECK (
            jsonb_typeof(result_snapshot #> '{proposal_evidence}')
                IS NOT DISTINCT FROM 'object'
            AND COALESCE(result_snapshot #>> '{proposal_evidence,schema_version}', '') = '1'
            AND (
                (
                    compilation_outcome = 'actionable'
                    AND jsonb_typeof(result_snapshot #> '{proposal_evidence,change_set}')
                        IS NOT DISTINCT FROM 'object'
                    AND compiled_payload_hash IS NOT NULL
                    AND octet_length(compiled_payload_hash) = 32
                )
                OR (
                    compilation_outcome = 'manual_review'
                    AND jsonb_typeof(result_snapshot #> '{proposal_evidence,change_set}')
                        IS NOT DISTINCT FROM 'null'
                    AND compiled_payload_hash IS NULL
                )
            )
        ),
    ADD CONSTRAINT schedule_simulations_consumption_window_check
        CHECK (consumed_at IS NULL OR (consumed_at >= created_at AND consumed_at < expires_at));

CREATE OR REPLACE FUNCTION guard_schedule_simulation_mutation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    mutation_time timestamptz := clock_timestamp();
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.consumed_at IS NULL AND OLD.expires_at > mutation_time THEN
            RAISE EXCEPTION 'a live unconsumed simulation cannot be deleted';
        END IF;
        RETURN OLD;
    END IF;

    IF OLD.id IS DISTINCT FROM NEW.id
       OR OLD.workspace_id IS DISTINCT FROM NEW.workspace_id
       OR OLD.user_id IS DISTINCT FROM NEW.user_id
       OR OLD.token_hash IS DISTINCT FROM NEW.token_hash
       OR OLD.subject_hash IS DISTINCT FROM NEW.subject_hash
       OR OLD.request_digest IS DISTINCT FROM NEW.request_digest
       OR OLD.base_revision_id IS DISTINCT FROM NEW.base_revision_id
       OR OLD.base_revision_label IS DISTINCT FROM NEW.base_revision_label
       OR OLD.result_snapshot IS DISTINCT FROM NEW.result_snapshot
       OR OLD.created_at IS DISTINCT FROM NEW.created_at
       OR OLD.expires_at IS DISTINCT FROM NEW.expires_at
       OR OLD.evidence_schema IS DISTINCT FROM NEW.evidence_schema
       OR OLD.request_hash IS DISTINCT FROM NEW.request_hash
       OR OLD.evidence_hash IS DISTINCT FROM NEW.evidence_hash
       OR OLD.compilation_outcome IS DISTINCT FROM NEW.compilation_outcome
       OR OLD.compiled_payload_hash IS DISTINCT FROM NEW.compiled_payload_hash
       OR OLD.consumed_at IS NOT NULL
       OR NEW.consumed_at IS NULL
       OR OLD.expires_at <= mutation_time
       OR NEW.consumed_at > mutation_time
       OR NEW.consumed_at < OLD.created_at
       OR NEW.consumed_at >= OLD.expires_at
    THEN
        RAISE EXCEPTION 'simulation evidence is immutable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER schedule_simulations_evidence_guard
    BEFORE UPDATE OR DELETE ON schedule_simulations
    FOR EACH ROW EXECUTE FUNCTION guard_schedule_simulation_mutation();

ALTER TABLE mcp_proposal_submissions
    ADD COLUMN simulation_id uuid,
    ADD COLUMN simulation_subject_hash bytea,
    ADD COLUMN simulation_request_digest bytea,
    ADD COLUMN simulation_request_hash bytea,
    ADD COLUMN simulation_base_revision_id uuid,
    ADD COLUMN simulation_created_at timestamptz,
    ADD COLUMN simulation_expires_at timestamptz,
    ADD COLUMN simulation_evidence_schema smallint,
    ADD COLUMN simulation_evidence_hash bytea,
    ADD COLUMN compilation_outcome varchar(24),
    ADD COLUMN compiled_payload_hash bytea,
    ADD COLUMN proposal_payload_hash bytea,
    ADD CONSTRAINT mcp_proposal_submissions_simulation_revision_fk
        FOREIGN KEY (workspace_id, simulation_base_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    ADD CONSTRAINT mcp_proposal_submissions_simulation_proof_check CHECK (
        (
            simulation_id IS NULL
            AND num_nonnulls(
                simulation_subject_hash,
                simulation_request_digest,
                simulation_request_hash,
                simulation_base_revision_id,
                simulation_created_at,
                simulation_expires_at,
                simulation_evidence_schema,
                simulation_evidence_hash,
                compilation_outcome,
                compiled_payload_hash,
                proposal_payload_hash
            ) = 0
        )
        OR (
            simulation_id IS NOT NULL
            AND num_nulls(
                simulation_subject_hash,
                simulation_request_digest,
                simulation_request_hash,
                simulation_base_revision_id,
                simulation_created_at,
                simulation_expires_at,
                simulation_evidence_schema,
                simulation_evidence_hash,
                compilation_outcome,
                proposal_payload_hash
            ) = 0
            AND octet_length(simulation_subject_hash) = 32
            AND octet_length(simulation_request_digest) = 16
            AND octet_length(simulation_request_hash) = 32
            AND simulation_request_digest = substring(simulation_request_hash FROM 1 FOR 16)
            AND simulation_evidence_schema = 1
            AND octet_length(simulation_evidence_hash) = 32
            AND octet_length(proposal_payload_hash) = 32
            AND completed_at >= simulation_created_at
            AND completed_at < simulation_expires_at
            AND (
                (
                    compilation_outcome = 'actionable'
                    AND compiled_payload_hash IS NOT NULL
                    AND octet_length(compiled_payload_hash) = 32
                    AND compiled_payload_hash = proposal_payload_hash
                )
                OR (
                    compilation_outcome = 'manual_review'
                    AND compiled_payload_hash IS NULL
                )
            )
        )
    );

CREATE UNIQUE INDEX mcp_proposal_submissions_simulation_uq
    ON mcp_proposal_submissions (workspace_id, user_id, simulation_id)
    WHERE simulation_id IS NOT NULL;

CREATE OR REPLACE FUNCTION verify_mcp_simulation_proof() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE
    simulation schedule_simulations%ROWTYPE;
    stored_proposal_payload jsonb;
    stored_proposal_source varchar(32);
    stored_proposal_kind varchar(32);
    stored_proposal_status varchar(24);
    stored_proposal_created_at timestamptz;
BEGIN
    IF NEW.simulation_id IS NULL THEN
        RAISE EXCEPTION 'new MCP proposal submissions require simulation proof';
    END IF;

    SELECT * INTO simulation
      FROM schedule_simulations
     WHERE workspace_id = NEW.workspace_id
       AND user_id = NEW.user_id
       AND id = NEW.simulation_id
     FOR UPDATE;

    IF NOT FOUND
       OR simulation.subject_hash IS DISTINCT FROM NEW.simulation_subject_hash
       OR simulation.request_digest IS DISTINCT FROM NEW.simulation_request_digest
       OR simulation.request_hash IS DISTINCT FROM NEW.simulation_request_hash
       OR simulation.base_revision_id IS DISTINCT FROM NEW.simulation_base_revision_id
       OR simulation.created_at IS DISTINCT FROM NEW.simulation_created_at
       OR simulation.expires_at IS DISTINCT FROM NEW.simulation_expires_at
       OR simulation.evidence_schema IS DISTINCT FROM NEW.simulation_evidence_schema
       OR simulation.evidence_hash IS DISTINCT FROM NEW.simulation_evidence_hash
       OR simulation.compilation_outcome IS DISTINCT FROM NEW.compilation_outcome
       OR simulation.compiled_payload_hash IS DISTINCT FROM NEW.compiled_payload_hash
       OR simulation.consumed_at IS DISTINCT FROM NEW.completed_at
    THEN
        RAISE EXCEPTION 'MCP proposal simulation proof does not match consumed evidence';
    END IF;

    SELECT payload, source, kind, status, created_at
      INTO stored_proposal_payload, stored_proposal_source, stored_proposal_kind,
           stored_proposal_status, stored_proposal_created_at
      FROM proposals
     WHERE workspace_id = NEW.workspace_id
       AND id = NEW.proposal_id
     FOR KEY SHARE;

    IF NOT FOUND
       OR stored_proposal_source IS DISTINCT FROM 'external_mcp'
       OR stored_proposal_status IS DISTINCT FROM 'pending'
       OR stored_proposal_created_at IS DISTINCT FROM NEW.completed_at
       OR (
            NEW.compilation_outcome = 'actionable'
            AND (
                simulation.result_snapshot #>> '{proposal_evidence,proposal_kind}'
                    IS DISTINCT FROM stored_proposal_kind
                OR simulation.result_snapshot #> '{proposal_evidence,change_set}'
                    IS DISTINCT FROM stored_proposal_payload
            )
       )
       OR (
            NEW.compilation_outcome = 'manual_review'
            AND stored_proposal_kind IS DISTINCT FROM 'schedule_plan'
       )
    THEN
        RAISE EXCEPTION 'MCP proposal does not match its consumed simulation';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER mcp_proposal_submissions_verify_simulation
    BEFORE INSERT ON mcp_proposal_submissions
    FOR EACH ROW EXECUTE FUNCTION verify_mcp_simulation_proof();
