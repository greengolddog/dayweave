-- Authoritative habit occurrence evidence, correction history, pause intervals,
-- and a content-free durable delta head. Occurrence identities are admitted only
-- from an immutable published schedule; native clients cannot mint ledger rows.

CREATE TABLE habit_occurrence_evidence (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    habit_id uuid NOT NULL,
    planner_occurrence_id uuid NOT NULL,
    source_schedule_revision_id uuid NOT NULL,
    source_item_revision bigint NOT NULL CHECK (source_item_revision > 0),
    policy_fingerprint bytea NOT NULL CHECK (octet_length(policy_fingerprint) = 32),
    recurrence_identity jsonb NOT NULL,
    nominal_start timestamptz NOT NULL,
    nominal_end timestamptz NOT NULL,
    window_start timestamptz NOT NULL,
    window_end timestamptz NOT NULL,
    local_date date NOT NULL,
    timezone_name varchar(100) NOT NULL,
    expected_duration_seconds bigint CHECK (
        expected_duration_seconds IS NULL
        OR expected_duration_seconds BETWEEN 1 AND 31622400
    ),
    expected_quantity bigint
        CHECK (expected_quantity IS NULL OR expected_quantity BETWEEN 1 AND 1000000000000),
    expected_unit varchar(200),
    is_sensitive boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT current_timestamp,
    last_published_at timestamptz NOT NULL DEFAULT current_timestamp,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, habit_id, planner_occurrence_id),
    FOREIGN KEY (workspace_id, habit_id) REFERENCES items(workspace_id, id),
    FOREIGN KEY (workspace_id, source_schedule_revision_id)
        REFERENCES schedule_revisions(workspace_id, id),
    CHECK (jsonb_typeof(recurrence_identity) = 'object'),
    CHECK (octet_length(recurrence_identity::text) <= 4096),
    CHECK (nominal_end > nominal_start),
    CHECK (window_end > window_start),
    CHECK (nominal_start >= window_start AND nominal_end <= window_end),
    CHECK (btrim(timezone_name) <> ''),
    CHECK ((expected_quantity IS NULL) = (expected_unit IS NULL)),
    CHECK (expected_unit IS NULL OR btrim(expected_unit) <> '')
);

CREATE INDEX habit_occurrence_evidence_range_idx
    ON habit_occurrence_evidence (workspace_id, habit_id, local_date, nominal_start, id);

CREATE INDEX habit_occurrence_evidence_planner_idx
    ON habit_occurrence_evidence (workspace_id, planner_occurrence_id, habit_id);

CREATE TABLE habit_occurrence_outcomes (
    workspace_id uuid NOT NULL,
    occurrence_evidence_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    status varchar(24) NOT NULL
        CHECK (status IN ('unresolved', 'partial', 'completed', 'skipped')),
    progress_basis_points integer NOT NULL CHECK (progress_basis_points BETWEEN 0 AND 10000),
    quantity bigint CHECK (
        quantity IS NULL OR quantity BETWEEN -1000000000000 AND 1000000000000
    ),
    unit varchar(200),
    actual_seconds bigint CHECK (
        actual_seconds IS NULL OR actual_seconds BETWEEN 0 AND 31622400
    ),
    note text,
    occurred_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, occurrence_evidence_id),
    FOREIGN KEY (workspace_id, occurrence_evidence_id)
        REFERENCES habit_occurrence_evidence(workspace_id, id),
    CHECK ((quantity IS NULL) = (unit IS NULL)),
    CHECK (unit IS NULL OR btrim(unit) <> ''),
    CHECK (note IS NULL OR char_length(note) <= 10000),
    CHECK (
        (status = 'unresolved' AND progress_basis_points = 0
          AND quantity IS NULL AND actual_seconds IS NULL AND note IS NULL)
        OR (status = 'partial' AND progress_basis_points BETWEEN 1 AND 9999)
        OR (status = 'completed' AND progress_basis_points = 10000)
        OR (status = 'skipped' AND progress_basis_points BETWEEN 0 AND 9999)
    )
);

CREATE TABLE habit_occurrence_versions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    occurrence_evidence_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    operation_id uuid NOT NULL,
    previous_snapshot jsonb,
    outcome_snapshot jsonb NOT NULL,
    occurred_at timestamptz NOT NULL,
    recorded_at timestamptz NOT NULL,
    UNIQUE (workspace_id, occurrence_evidence_id, revision),
    UNIQUE (workspace_id, operation_id),
    FOREIGN KEY (workspace_id, occurrence_evidence_id)
        REFERENCES habit_occurrence_evidence(workspace_id, id),
    CHECK (previous_snapshot IS NULL OR jsonb_typeof(previous_snapshot) = 'object'),
    CHECK (jsonb_typeof(outcome_snapshot) = 'object'),
    CHECK (octet_length(COALESCE(previous_snapshot, '{}'::jsonb)::text) <= 65536),
    CHECK (octet_length(outcome_snapshot::text) <= 65536)
);

CREATE INDEX habit_occurrence_outcomes_status_idx
    ON habit_occurrence_outcomes (workspace_id, status, occurred_at DESC, occurrence_evidence_id);

CREATE INDEX habit_occurrence_versions_history_idx
    ON habit_occurrence_versions
        (workspace_id, occurrence_evidence_id, revision DESC);

CREATE TABLE habit_pauses (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    habit_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    started_at timestamptz NOT NULL,
    ended_at timestamptz,
    preserves_streak boolean NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, habit_id) REFERENCES items(workspace_id, id),
    CHECK (ended_at IS NULL OR ended_at > started_at)
);

CREATE UNIQUE INDEX habit_pauses_one_open_uq
    ON habit_pauses (workspace_id, habit_id) WHERE ended_at IS NULL;

CREATE INDEX habit_pauses_range_idx
    ON habit_pauses (workspace_id, habit_id, started_at, ended_at);

CREATE TABLE habit_pause_versions (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    pause_id uuid NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    operation_id uuid NOT NULL,
    previous_snapshot jsonb,
    pause_snapshot jsonb NOT NULL,
    recorded_at timestamptz NOT NULL,
    UNIQUE (workspace_id, pause_id, revision),
    UNIQUE (workspace_id, operation_id),
    FOREIGN KEY (workspace_id, pause_id) REFERENCES habit_pauses(workspace_id, id),
    CHECK (previous_snapshot IS NULL OR jsonb_typeof(previous_snapshot) = 'object'),
    CHECK (jsonb_typeof(pause_snapshot) = 'object'),
    CHECK (octet_length(COALESCE(previous_snapshot, '{}'::jsonb)::text) <= 16384),
    CHECK (octet_length(pause_snapshot::text) <= 16384)
);

CREATE TABLE habit_changes (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    change_kind varchar(24) NOT NULL
        CHECK (change_kind IN ('occurrence_upsert', 'pause_upsert')),
    entity_id uuid NOT NULL,
    entity_revision bigint NOT NULL CHECK (entity_revision > 0),
    payload jsonb NOT NULL,
    changed_at timestamptz NOT NULL DEFAULT current_timestamp,
    CHECK (jsonb_typeof(payload) = 'object'),
    CHECK (octet_length(payload::text) <= 65536)
);

CREATE INDEX habit_changes_workspace_delta_idx
    ON habit_changes (workspace_id, sequence);

-- Permanent operation receipts close the retry-after-offline-window hole left
-- by the generic 24-hour idempotency cache. Only hashes, never raw keys, persist.
CREATE TABLE habit_operation_receipts (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    namespace varchar(100) NOT NULL,
    key_hash bytea NOT NULL CHECK (octet_length(key_hash) = 32),
    operation_id uuid NOT NULL,
    request_fingerprint bytea NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    response_json jsonb NOT NULL,
    completed_at timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, namespace, key_hash),
    UNIQUE (workspace_id, operation_id),
    CHECK (btrim(namespace) <> ''),
    CHECK (jsonb_typeof(response_json) = 'object'),
    CHECK (octet_length(response_json::text) <= 65536)
);

-- Evidence and correction versions are historical facts. Outcome and pause
-- projections may advance, but deleting any ledger row would break audit/undo.
CREATE FUNCTION reject_habit_history_delete() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    RAISE EXCEPTION 'habit history is append-only';
END
$guard$;

CREATE TRIGGER habit_occurrence_evidence_no_delete
    BEFORE DELETE ON habit_occurrence_evidence
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();
CREATE TRIGGER habit_occurrence_outcomes_no_delete
    BEFORE DELETE ON habit_occurrence_outcomes
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();
CREATE TRIGGER habit_pauses_no_delete
    BEFORE DELETE ON habit_pauses
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();
CREATE TRIGGER habit_occurrence_versions_immutable
    BEFORE UPDATE OR DELETE ON habit_occurrence_versions
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();
CREATE TRIGGER habit_pause_versions_immutable
    BEFORE UPDATE OR DELETE ON habit_pause_versions
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();
CREATE TRIGGER habit_changes_immutable
    BEFORE UPDATE OR DELETE ON habit_changes
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();
CREATE TRIGGER habit_operation_receipts_immutable
    BEFORE UPDATE OR DELETE ON habit_operation_receipts
    FOR EACH ROW EXECUTE FUNCTION reject_habit_history_delete();

-- The publisher may only advance the observation timestamp of immutable
-- occurrence evidence. Identity, schedule provenance, policy and local-time
-- snapshots are never rewritten by a later publication.
CREATE FUNCTION guard_habit_occurrence_evidence_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF (to_jsonb(NEW) - 'last_published_at') IS DISTINCT FROM
       (to_jsonb(OLD) - 'last_published_at')
       OR NEW.last_published_at < OLD.last_published_at THEN
        RAISE EXCEPTION 'habit occurrence evidence is immutable';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER habit_occurrence_evidence_update_guard
    BEFORE UPDATE ON habit_occurrence_evidence
    FOR EACH ROW EXECUTE FUNCTION guard_habit_occurrence_evidence_update();

-- Current projections may only move forward one revision. Full before/after
-- evidence is inserted into the append-only version table by the same
-- transaction before the mutation commits.
CREATE FUNCTION guard_habit_outcome_projection_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.occurrence_evidence_id IS DISTINCT FROM OLD.occurrence_evidence_id
       OR NEW.revision <> OLD.revision + 1
       OR NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'habit outcome projection revision is invalid';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER habit_occurrence_outcome_update_guard
    BEFORE UPDATE ON habit_occurrence_outcomes
    FOR EACH ROW EXECUTE FUNCTION guard_habit_outcome_projection_update();

-- Pause intervals are opened once and closed once. Historical versions carry
-- both snapshots, while the current row cannot be reopened or retargeted.
CREATE FUNCTION guard_habit_pause_projection_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
       OR NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.habit_id IS DISTINCT FROM OLD.habit_id
       OR NEW.started_at IS DISTINCT FROM OLD.started_at
       OR NEW.preserves_streak IS DISTINCT FROM OLD.preserves_streak
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR OLD.ended_at IS NOT NULL
       OR NEW.ended_at IS NULL
       OR NEW.revision <> OLD.revision + 1
       OR NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'habit pause projection transition is invalid';
    END IF;
    RETURN NEW;
END
$guard$;

CREATE TRIGGER habit_pause_update_guard
    BEFORE UPDATE ON habit_pauses
    FOR EACH ROW EXECUTE FUNCTION guard_habit_pause_projection_update();
