-- Completion policy is a sidecar, not a change to the legacy Item wire shape.
-- Permanent command/evaluation evidence retains reopening reasons for exact
-- replay and account-lifetime audit. Only proposal undo companions expire.
DO $preflight$
DECLARE incompatible_count bigint;
BEGIN
    LOCK TABLE items, item_hierarchy IN SHARE ROW EXCLUSIVE MODE;
    SELECT count(*) INTO incompatible_count FROM items parent
    WHERE parent.trashed_at IS NULL AND parent.status IN ('completed','skipped','cancelled')
      AND EXISTS(SELECT 1 FROM item_hierarchy edge JOIN items child
          ON child.workspace_id=edge.workspace_id AND child.id=edge.child_item_id
          WHERE edge.workspace_id=parent.workspace_id AND edge.parent_item_id=parent.id
            AND child.trashed_at IS NULL);
    IF incompatible_count > 0 THEN
        RAISE EXCEPTION 'completion migration requires reviewed reopening of % legacy terminal structural parents', incompatible_count
            USING ERRCODE='23514';
    END IF;
END
$preflight$;

-- Historical rows remain NULL without a table rewrite. New appends retain
-- their original transaction identity, independent of later unpinned UPDATEs.
ALTER TABLE item_changes ADD COLUMN completion_capture_xid xid8;
ALTER TABLE item_changes ALTER COLUMN completion_capture_xid SET DEFAULT pg_current_xact_id();
CREATE FUNCTION guard_item_change_completion_capture() RETURNS trigger
LANGUAGE plpgsql AS $capture$
BEGIN
    IF TG_OP='INSERT' AND NEW.completion_capture_xid IS DISTINCT FROM pg_current_xact_id() THEN
        RAISE EXCEPTION 'canonical append requires its current transaction identity';
    END IF;
    IF TG_OP='UPDATE' AND NEW.completion_capture_xid IS DISTINCT FROM OLD.completion_capture_xid THEN
        RAISE EXCEPTION 'canonical append transaction identity is immutable';
    END IF;
    RETURN NEW;
END
$capture$;
CREATE TRIGGER item_change_completion_capture_guard BEFORE INSERT OR UPDATE ON item_changes
    FOR EACH ROW EXECUTE FUNCTION guard_item_change_completion_capture();

CREATE FUNCTION valid_item_completion_revision(value jsonb, minimum_value bigint) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT AS $revision$
    SELECT coalesce(jsonb_typeof(value)='number' AND value::text ~ '^(0|[1-9][0-9]*)$'
        AND value::text::numeric BETWEEN minimum_value AND 9223372036854775807,false)
$revision$;

CREATE FUNCTION valid_item_completion_reopen(value jsonb, item_id uuid) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $reopen$
DECLARE reason_kind text; blocker uuid;
BEGIN
    IF NOT coalesce(jsonb_typeof(value)='object'
        AND value ?& ARRAY['status','blocked_reason_kind','blocked_by_item_id','blocked_reason']
        AND value-ARRAY['status','blocked_reason_kind','blocked_by_item_id','blocked_reason']='{}'::jsonb
        AND jsonb_typeof(value->'status')='string' AND value->>'status' IN ('inbox','planned','blocked'),false)
    THEN RETURN false; END IF;
    IF value->'blocked_reason' <> 'null'::jsonb AND (
        jsonb_typeof(value->'blocked_reason') <> 'string'
        OR NOT valid_item_progress_label(value->>'blocked_reason',1000)) THEN RETURN false; END IF;
    IF value->>'status' <> 'blocked' THEN
        RETURN value->'blocked_reason_kind'='null'::jsonb
            AND value->'blocked_by_item_id'='null'::jsonb AND value->'blocked_reason'='null'::jsonb;
    END IF;
    IF jsonb_typeof(value->'blocked_reason_kind') <> 'string' THEN RETURN false; END IF;
    reason_kind := value->>'blocked_reason_kind';
    IF reason_kind IN ('manual','external') THEN
        RETURN value->'blocked_by_item_id'='null'::jsonb AND jsonb_typeof(value->'blocked_reason')='string';
    END IF;
    IF reason_kind <> 'dependency' OR jsonb_typeof(value->'blocked_by_item_id') <> 'string'
        OR value->>'blocked_by_item_id' !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    THEN RETURN false; END IF;
    blocker := (value->>'blocked_by_item_id')::uuid;
    RETURN blocker <> '00000000-0000-0000-0000-000000000000'::uuid AND blocker <> item_id;
END
$reopen$;

CREATE FUNCTION valid_item_completion_state(value jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $state$
DECLARE item_id uuid; provenance jsonb;
BEGIN
    IF NOT coalesce(jsonb_typeof(value)='object'
        AND value ?& ARRAY['item_id','revision','required_for_parent','mode','provenance','updated_at']
        AND value-ARRAY['item_id','revision','required_for_parent','mode','provenance','updated_at']='{}'::jsonb
        AND jsonb_typeof(value->'item_id')='string'
        AND value->>'item_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        AND value->>'item_id' <> '00000000-0000-0000-0000-000000000000'
        AND valid_item_completion_revision(value->'revision',0)
        AND jsonb_typeof(value->'required_for_parent')='boolean'
        AND jsonb_typeof(value->'mode')='string' AND value->>'mode' IN ('automatic','keep_open','complete'),false)
    THEN RETURN false; END IF;
    item_id := (value->>'item_id')::uuid;
    provenance := value->'provenance';
    IF provenance <> 'null'::jsonb AND NOT coalesce(jsonb_typeof(provenance)='object'
        AND provenance ?& ARRAY['kind','reopen'] AND provenance-ARRAY['kind','reopen']='{}'::jsonb
        AND jsonb_typeof(provenance->'kind')='string' AND provenance->>'kind' IN ('automatic','manual')
        AND valid_item_completion_reopen(provenance->'reopen',item_id),false)
    THEN RETURN false; END IF;
    IF provenance<>'null'::jsonb AND NOT (
        (value->>'mode'='automatic' AND provenance->>'kind'='automatic')
        OR (value->>'mode'='complete' AND provenance->>'kind'='manual'))
    THEN RETURN false; END IF;
    IF value->>'mode'='complete' AND provenance='null'::jsonb THEN RETURN false; END IF;
    IF value->'revision'='0'::jsonb THEN
        RETURN value->'required_for_parent'='true'::jsonb AND value->>'mode'='automatic'
            AND provenance='null'::jsonb AND value->'updated_at'='null'::jsonb;
    END IF;
    RETURN coalesce(jsonb_typeof(value->'updated_at')='string'
        AND value->>'updated_at' ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?(Z|\+00:00)$'
        AND isfinite((value->>'updated_at')::timestamptz),false);
EXCEPTION WHEN invalid_text_representation OR datetime_field_overflow OR invalid_datetime_format THEN RETURN false;
END
$state$;

CREATE FUNCTION valid_item_completion_snapshot(value jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $snapshot$
DECLARE counts jsonb; field_name text;
BEGIN
    IF NOT coalesce(jsonb_typeof(value)='object'
        AND value ?& ARRAY['schema_version','item_id','item_revision','state','evidence_hash','counts','occurrence_evidence_required']
        AND value-ARRAY['schema_version','item_id','item_revision','state','evidence_hash','counts','occurrence_evidence_required']='{}'::jsonb
        AND value->'schema_version'='1'::jsonb AND valid_item_completion_state(value->'state')
        AND value->'item_id'=value->'state'->'item_id' AND valid_item_completion_revision(value->'item_revision',1)
        AND jsonb_typeof(value->'evidence_hash')='string' AND value->>'evidence_hash' ~ '^sha256:[0-9a-f]{64}$'
        AND jsonb_typeof(value->'occurrence_evidence_required')='boolean',false)
    THEN RETURN false; END IF;
    counts := value->'counts';
    IF NOT coalesce(jsonb_typeof(counts)='object'
        AND counts ?& ARRAY['required_descendants','completed','incomplete','occurrence_evidence_required']
        AND counts-ARRAY['required_descendants','completed','incomplete','occurrence_evidence_required']='{}'::jsonb,false)
    THEN RETURN false; END IF;
    FOREACH field_name IN ARRAY ARRAY['required_descendants','completed','incomplete','occurrence_evidence_required'] LOOP
        IF NOT valid_item_completion_revision(counts->field_name,0) THEN RETURN false; END IF;
    END LOOP;
    RETURN (counts->>'completed')::numeric+(counts->>'incomplete')::numeric
        +(counts->>'occurrence_evidence_required')::numeric=(counts->>'required_descendants')::numeric;
END
$snapshot$;

CREATE TABLE item_completion_state (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    item_id uuid NOT NULL,
    revision bigint NOT NULL CHECK(revision>0),
    state_json jsonb NOT NULL CHECK(valid_item_completion_state(state_json)),
    updated_at timestamptz NOT NULL CHECK(isfinite(updated_at)),
    PRIMARY KEY(workspace_id,item_id),
    FOREIGN KEY(workspace_id,item_id) REFERENCES items(workspace_id,id),
    CHECK(state_json->>'item_id'=item_id::text AND state_json->>'revision'=revision::text
        AND (state_json->>'updated_at')::timestamptz=updated_at),
    CHECK(octet_length(state_json::text)<=16384)
);

CREATE TABLE item_completion_evaluations (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    evaluation_id uuid NOT NULL CHECK(evaluation_id<>'00000000-0000-0000-0000-000000000000'::uuid),
    cause_kind text NOT NULL CHECK(cause_kind IN ('canonical_write','policy_command','snapshot_restore')),
    cause_id uuid,
    evidence_hash text NOT NULL CHECK(evidence_hash ~ '^sha256:[0-9a-f]{64}$'),
    execution_revision bigint NOT NULL CHECK(execution_revision>=0),
    effect_count integer NOT NULL CHECK(effect_count BETWEEN 1 AND 20000),
    recorded_at timestamptz NOT NULL CHECK(isfinite(recorded_at)),
    capture_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    PRIMARY KEY(workspace_id,evaluation_id),
    CHECK((cause_kind='policy_command' AND cause_id IS NOT NULL
        AND cause_id<>'00000000-0000-0000-0000-000000000000'::uuid)
        OR (cause_kind<>'policy_command' AND cause_id IS NULL))
);

CREATE TABLE item_completion_effects (
    workspace_id uuid NOT NULL,
    evaluation_id uuid NOT NULL,
    item_id uuid NOT NULL,
    completion_revision bigint NOT NULL CHECK(completion_revision>0),
    before_item_revision bigint NOT NULL CHECK(before_item_revision>0),
    after_item_revision bigint NOT NULL CHECK(after_item_revision>before_item_revision
        AND after_item_revision::numeric=before_item_revision::numeric+1),
    before_state_json jsonb NOT NULL CHECK(valid_item_completion_state(before_state_json)),
    after_state_json jsonb NOT NULL CHECK(valid_item_completion_state(after_state_json)),
    reason text NOT NULL CHECK(reason IN ('policy_reviewed','unchanged','occurrence_evidence_required',
        'automatically_completed','automatically_reopened','manually_completed','manually_kept_open',
        'manual_completion_released','snapshot_restored')),
    recorded_at timestamptz NOT NULL CHECK(isfinite(recorded_at)),
    PRIMARY KEY(workspace_id,evaluation_id,item_id),
    UNIQUE(workspace_id,item_id,completion_revision),
    FOREIGN KEY(workspace_id,evaluation_id) REFERENCES item_completion_evaluations(workspace_id,evaluation_id),
    FOREIGN KEY(workspace_id,item_id) REFERENCES items(workspace_id,id),
    FOREIGN KEY(workspace_id,item_id,before_item_revision) REFERENCES item_changes(workspace_id,item_id,item_revision),
    FOREIGN KEY(workspace_id,item_id,after_item_revision) REFERENCES item_changes(workspace_id,item_id,item_revision),
    CHECK(before_state_json->>'item_id'=item_id::text AND after_state_json->>'item_id'=item_id::text
        AND after_state_json->>'revision'=completion_revision::text
        AND before_state_json->>'revision'=(completion_revision-1)::text
        AND (after_state_json->>'updated_at')::timestamptz=recorded_at),
    CHECK(octet_length(before_state_json::text)<=16384 AND octet_length(after_state_json::text)<=16384)
);

CREATE TABLE item_completion_operations (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    operation_id uuid NOT NULL CHECK(operation_id<>'00000000-0000-0000-0000-000000000000'::uuid),
    item_id uuid NOT NULL,
    actor_user_id uuid NOT NULL,
    actor_session_id uuid,
    evaluation_id uuid NOT NULL,
    request_json jsonb NOT NULL,
    result_json jsonb NOT NULL CHECK(valid_item_completion_snapshot(result_json)),
    recorded_at timestamptz NOT NULL CHECK(isfinite(recorded_at)),
    PRIMARY KEY(workspace_id,operation_id),
    UNIQUE(workspace_id,evaluation_id),
    FOREIGN KEY(workspace_id,item_id) REFERENCES items(workspace_id,id),
    FOREIGN KEY(workspace_id,actor_user_id) REFERENCES workspace_members(workspace_id,user_id),
    FOREIGN KEY(workspace_id,evaluation_id,item_id) REFERENCES item_completion_effects(workspace_id,evaluation_id,item_id),
    CHECK(octet_length(request_json::text)<=16384 AND octet_length(result_json::text)<=32768),
    CHECK(coalesce(jsonb_typeof(request_json)='object'
        AND request_json ?& ARRAY['schema_version','operation_id','expected_item_revision','expected_completion_revision','expected_evidence_hash','required_for_parent','mode','reopening']
        AND request_json-ARRAY['schema_version','operation_id','expected_item_revision','expected_completion_revision','expected_evidence_hash','required_for_parent','mode','reopening']='{}'::jsonb
        AND request_json->'schema_version'='1'::jsonb AND request_json->>'operation_id'=operation_id::text
        AND valid_item_completion_revision(request_json->'expected_item_revision',1)
        AND valid_item_completion_revision(request_json->'expected_completion_revision',0)
        AND jsonb_typeof(request_json->'expected_evidence_hash')='string'
        AND request_json->>'expected_evidence_hash' ~ '^sha256:[0-9a-f]{64}$'
        AND jsonb_typeof(request_json->'required_for_parent')='boolean'
        AND jsonb_typeof(request_json->'mode')='string' AND request_json->>'mode' IN ('automatic','keep_open','complete')
        AND (request_json->'reopening'='null'::jsonb OR valid_item_completion_reopen(request_json->'reopening',item_id))
        AND result_json->>'item_id'=item_id::text,false))
);

CREATE FUNCTION guard_item_completion_state() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'completion policy retains revision custody'; END IF;
    IF TG_OP='INSERT' AND NEW.revision<>1 THEN RAISE EXCEPTION 'completion policy begins at revision one'; END IF;
    IF TG_OP='UPDATE' AND (NEW.workspace_id<>OLD.workspace_id OR NEW.item_id<>OLD.item_id
        OR NEW.revision::numeric<>OLD.revision::numeric+1)
    THEN RAISE EXCEPTION 'invalid completion policy transition'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION guard_item_completion_evaluation() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'completion evaluations are immutable'; END IF;
    IF NEW.capture_xid<>pg_current_xact_id() THEN RAISE EXCEPTION 'completion evaluation transaction differs'; END IF;
    -- Same barrier as snapshot capture: permanent revision references cannot
    -- race a previously admitted rewrite/delete of their source history.
    PERFORM pg_advisory_xact_lock_shared(hashtextextended('dayweave.item-bootstrap.history-immutability.v1',0));
    RETURN NEW;
END
$guard$;

CREATE FUNCTION guard_item_completion_evidence() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE evaluation item_completion_evaluations%ROWTYPE;
BEGIN
    IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'completion evidence is immutable'; END IF;
    SELECT * INTO evaluation FROM item_completion_evaluations
        WHERE workspace_id=NEW.workspace_id AND evaluation_id=NEW.evaluation_id;
    IF NOT FOUND OR evaluation.capture_xid<>pg_current_xact_id() OR evaluation.recorded_at<>NEW.recorded_at
    THEN RAISE EXCEPTION 'completion evidence requires its original evaluation transaction'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION verify_item_completion_state() RETURNS trigger
LANGUAGE plpgsql AS $verify$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM item_completion_effects effect
        WHERE effect.workspace_id=NEW.workspace_id AND effect.item_id=NEW.item_id
          AND effect.completion_revision=NEW.revision AND effect.after_state_json=NEW.state_json
          AND effect.recorded_at=NEW.updated_at
          AND ((TG_OP='INSERT' AND effect.before_state_json->'revision'='0'::jsonb)
            OR (TG_OP='UPDATE' AND effect.before_state_json=OLD.state_json)))
    THEN RAISE EXCEPTION 'completion state requires exact atomic transition evidence'; END IF;
    IF NEW.state_json#>>'{provenance,reopen,blocked_reason_kind}'='dependency'
        AND NOT EXISTS(SELECT 1 FROM items WHERE workspace_id=NEW.workspace_id
            AND id=(NEW.state_json#>>'{provenance,reopen,blocked_by_item_id}')::uuid)
    THEN RAISE EXCEPTION 'completion reopening dependency is outside the workspace'; END IF;
    RETURN NEW;
END
$verify$;

CREATE FUNCTION verify_item_completion_effect() RETURNS trigger
LANGUAGE plpgsql AS $verify$
DECLARE current_state item_completion_state%ROWTYPE; current_item items%ROWTYPE;
    source item_changes%ROWTYPE; evaluation_cause text; evaluation_capture xid8;
BEGIN
    SELECT * INTO current_state FROM item_completion_state
        WHERE workspace_id=NEW.workspace_id AND item_id=NEW.item_id;
    IF NOT FOUND OR current_state.revision<NEW.completion_revision THEN
        RAISE EXCEPTION 'completion effect requires committed sidecar custody';
    END IF;
    IF current_state.revision=NEW.completion_revision THEN
        IF current_state.state_json<>NEW.after_state_json THEN
            RAISE EXCEPTION 'final completion state contradicts transition evidence';
        END IF;
    ELSIF NOT EXISTS(SELECT 1 FROM item_completion_effects next_effect
        WHERE next_effect.workspace_id=NEW.workspace_id AND next_effect.item_id=NEW.item_id
          AND next_effect.completion_revision::numeric=NEW.completion_revision::numeric+1
          AND next_effect.before_state_json=NEW.after_state_json
          AND next_effect.before_item_revision>=NEW.after_item_revision) THEN
        RAISE EXCEPTION 'completion transition chain is incomplete';
    END IF;
    IF NEW.completion_revision>1 AND NOT EXISTS(SELECT 1 FROM item_completion_effects previous_effect
        WHERE previous_effect.workspace_id=NEW.workspace_id AND previous_effect.item_id=NEW.item_id
          AND previous_effect.completion_revision=NEW.completion_revision-1
          AND previous_effect.after_state_json=NEW.before_state_json
          AND previous_effect.after_item_revision<=NEW.before_item_revision) THEN
        RAISE EXCEPTION 'completion transition has no exact predecessor';
    END IF;
    SELECT * INTO current_item FROM items WHERE workspace_id=NEW.workspace_id AND id=NEW.item_id;
    IF NOT FOUND OR current_item.revision<NEW.after_item_revision THEN
        RAISE EXCEPTION 'completion transition exceeds canonical authority';
    END IF;
    IF current_state.state_json->'provenance'<>'null'::jsonb AND current_item.trashed_at IS NULL
        AND current_item.status<>'completed' THEN
        RAISE EXCEPTION 'completion provenance contradicts final live lifecycle';
    END IF;
    SELECT * INTO source FROM item_changes WHERE workspace_id=NEW.workspace_id
        AND item_id=NEW.item_id AND item_revision=NEW.after_item_revision;
    IF NOT FOUND THEN RAISE EXCEPTION 'completion transition lacks its canonical delta'; END IF;
    SELECT cause_kind,capture_xid INTO evaluation_cause,evaluation_capture FROM item_completion_evaluations
        WHERE workspace_id=NEW.workspace_id AND evaluation_id=NEW.evaluation_id;
    IF source.completion_capture_xid IS DISTINCT FROM evaluation_capture THEN
        RAISE EXCEPTION 'completion transition requires a new canonical append in its transaction';
    END IF;
    IF source.change_kind='tombstone' THEN
        IF evaluation_cause<>'snapshot_restore' THEN
            RAISE EXCEPTION 'completion lifecycle effects require an upsert delta';
        END IF;
    ELSIF source.change_kind<>'upsert' OR NOT coalesce(
        source.payload->>'id'=NEW.item_id::text
        AND source.payload->>'revision'=NEW.after_item_revision::text
        AND (source.payload->>'updated_at')::timestamptz=NEW.recorded_at
        AND (NEW.after_state_json->'provenance'='null'::jsonb OR (
            source.payload->>'status'='completed' AND source.payload->'completed_at'<>'null'::jsonb
            AND source.payload->'blocked_reason_kind'='null'::jsonb
            AND source.payload->'blocked_by_item_id'='null'::jsonb
            AND source.payload->'blocked_reason'='null'::jsonb)),false)
    THEN RAISE EXCEPTION 'completion transition contradicts its canonical delta'; END IF;
    RETURN NEW;
END
$verify$;

-- Check the final live row even when a subsequent canonical writer changes
-- status without touching this sidecar. Intermediate restore/finalizer states
-- may differ; the deferred query deliberately inspects the final transaction.
CREATE FUNCTION verify_item_completion_canonical_state() RETURNS trigger
LANGUAGE plpgsql AS $verify$
DECLARE target_workspace uuid; target_item uuid;
BEGIN
    target_workspace:=CASE WHEN TG_OP='DELETE' THEN OLD.workspace_id ELSE NEW.workspace_id END;
    target_item:=CASE WHEN TG_OP='DELETE' THEN OLD.id ELSE NEW.id END;
    IF EXISTS(SELECT 1 FROM item_completion_state completion JOIN items item
        ON item.workspace_id=completion.workspace_id AND item.id=completion.item_id
        WHERE completion.workspace_id=target_workspace AND completion.item_id=target_item
          AND completion.state_json->'provenance'<>'null'::jsonb
          AND item.trashed_at IS NULL AND item.status<>'completed')
    THEN RAISE EXCEPTION 'canonical lifecycle contradicts retained completion provenance'; END IF;
    IF TG_OP='DELETE' AND NOT EXISTS(SELECT 1 FROM items WHERE workspace_id=target_workspace AND id=target_item)
        AND EXISTS(SELECT 1 FROM item_completion_state WHERE workspace_id=target_workspace
            AND state_json#>>'{provenance,reopen,blocked_by_item_id}'=target_item::text)
    THEN RAISE EXCEPTION 'canonical deletion removes retained reopening dependency'; END IF;
    RETURN NULL;
END
$verify$;

CREATE FUNCTION verify_item_completion_evaluation() RETURNS trigger
LANGUAGE plpgsql AS $verify$
BEGIN
    IF (SELECT count(*) FROM item_completion_effects
        WHERE workspace_id=NEW.workspace_id AND evaluation_id=NEW.evaluation_id)<>NEW.effect_count
    THEN RAISE EXCEPTION 'completion evaluation effects are incomplete'; END IF;
    IF NEW.cause_kind='policy_command' AND NOT EXISTS(SELECT 1 FROM item_completion_operations
        WHERE workspace_id=NEW.workspace_id AND evaluation_id=NEW.evaluation_id AND operation_id=NEW.cause_id)
    THEN RAISE EXCEPTION 'reviewed completion evaluation requires permanent operation custody'; END IF;
    RETURN NEW;
END
$verify$;

CREATE FUNCTION verify_item_completion_operation() RETURNS trigger
LANGUAGE plpgsql AS $verify$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM item_completion_evaluations evaluation JOIN item_completion_effects effect
        ON effect.workspace_id=evaluation.workspace_id AND effect.evaluation_id=evaluation.evaluation_id
        WHERE evaluation.workspace_id=NEW.workspace_id AND evaluation.evaluation_id=NEW.evaluation_id
          AND evaluation.cause_kind='policy_command' AND evaluation.cause_id=NEW.operation_id
          AND evaluation.evidence_hash=NEW.request_json->>'expected_evidence_hash'
          AND effect.item_id=NEW.item_id
          AND effect.before_item_revision::text=NEW.request_json->>'expected_item_revision'
          AND effect.before_state_json->'revision'=NEW.request_json->'expected_completion_revision'
          AND effect.after_state_json->'required_for_parent'=NEW.request_json->'required_for_parent'
          AND effect.after_state_json->'mode'=NEW.request_json->'mode'
          AND effect.after_state_json=NEW.result_json->'state'
          AND effect.after_item_revision::text=NEW.result_json->>'item_revision')
    THEN RAISE EXCEPTION 'completion operation does not match its original reviewed transition'; END IF;
    -- Deliberately no equality to the latest canonical revision here: later
    -- effects in this transaction may have advanced it beyond this receipt.
    RETURN NEW;
END
$verify$;

CREATE TRIGGER item_completion_state_guard BEFORE INSERT OR UPDATE OR DELETE ON item_completion_state
    FOR EACH ROW EXECUTE FUNCTION guard_item_completion_state();
CREATE CONSTRAINT TRIGGER item_completion_state_evidence AFTER INSERT OR UPDATE ON item_completion_state
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_item_completion_state();
CREATE CONSTRAINT TRIGGER item_completion_canonical_consistent AFTER INSERT OR UPDATE OR DELETE ON items
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_item_completion_canonical_state();
CREATE TRIGGER item_completion_evaluations_guard BEFORE INSERT OR UPDATE OR DELETE ON item_completion_evaluations
    FOR EACH ROW EXECUTE FUNCTION guard_item_completion_evaluation();
CREATE CONSTRAINT TRIGGER item_completion_evaluations_complete AFTER INSERT ON item_completion_evaluations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_item_completion_evaluation();
CREATE TRIGGER item_completion_effects_guard BEFORE INSERT OR UPDATE OR DELETE ON item_completion_effects
    FOR EACH ROW EXECUTE FUNCTION guard_item_completion_evidence();
CREATE CONSTRAINT TRIGGER item_completion_effects_complete AFTER INSERT ON item_completion_effects
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_item_completion_effect();
CREATE TRIGGER item_completion_operations_guard BEFORE INSERT OR UPDATE OR DELETE ON item_completion_operations
    FOR EACH ROW EXECUTE FUNCTION guard_item_completion_evidence();
CREATE CONSTRAINT TRIGGER item_completion_operations_complete AFTER INSERT ON item_completion_operations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_item_completion_operation();

-- Server-only proposal companions never enter legacy Item/proposal JSON or
-- receipts. Full normalized revision-zero state represents an absent sidecar.
CREATE TABLE proposal_application_completion_evidence (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    application_id uuid NOT NULL,
    post_apply_evidence_hash text NOT NULL CHECK(post_apply_evidence_hash ~ '^sha256:[0-9a-f]{64}$'),
    execution_revision bigint NOT NULL CHECK(execution_revision>=0),
    created_at timestamptz NOT NULL CHECK(isfinite(created_at)),
    capture_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    PRIMARY KEY(workspace_id,user_id,application_id),
    FOREIGN KEY(workspace_id,user_id,application_id) REFERENCES proposal_applications(workspace_id,user_id,id),
    FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id)
);

CREATE TABLE proposal_application_completion_states (
    workspace_id uuid NOT NULL,
    user_id uuid NOT NULL,
    application_id uuid NOT NULL,
    ordinal smallint NOT NULL CHECK(ordinal BETWEEN 0 AND 99),
    item_id uuid NOT NULL,
    before_state_json jsonb,
    before_state_hash bytea NOT NULL CHECK(octet_length(before_state_hash)=32),
    snapshots_scrubbed_at timestamptz,
    created_at timestamptz NOT NULL CHECK(isfinite(created_at)),
    PRIMARY KEY(workspace_id,user_id,application_id,ordinal),
    UNIQUE(workspace_id,user_id,application_id,item_id),
    FOREIGN KEY(workspace_id,user_id,application_id) REFERENCES proposal_application_completion_evidence(workspace_id,user_id,application_id),
    FOREIGN KEY(workspace_id,user_id,application_id,ordinal) REFERENCES proposal_application_effects(workspace_id,user_id,application_id,ordinal),
    FOREIGN KEY(workspace_id,item_id) REFERENCES items(workspace_id,id),
    FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id),
    CHECK((snapshots_scrubbed_at IS NULL AND before_state_json IS NOT NULL
            AND valid_item_completion_state(before_state_json) AND before_state_json->>'item_id'=item_id::text
            AND octet_length(before_state_json::text)<=16384)
        OR (snapshots_scrubbed_at IS NOT NULL AND isfinite(snapshots_scrubbed_at)
            AND snapshots_scrubbed_at>=created_at AND before_state_json IS NULL))
);

CREATE FUNCTION guard_proposal_completion_evidence() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'proposal completion evidence is immutable'; END IF;
    IF NEW.capture_xid<>pg_current_xact_id() OR NOT EXISTS(SELECT 1 FROM proposal_applications application
        WHERE application.workspace_id=NEW.workspace_id AND application.user_id=NEW.user_id
          AND application.id=NEW.application_id AND application.status='applied'
          AND application.applied_at=NEW.created_at
          AND application.xmin::text::numeric=mod(pg_current_xact_id()::text::numeric,4294967296))
    THEN RAISE EXCEPTION 'completion proof must be captured with the original proposal application'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION guard_proposal_completion_state() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE expiry timestamptz;
BEGIN
    IF TG_OP='INSERT' THEN
        IF NEW.snapshots_scrubbed_at IS NOT NULL OR NOT EXISTS(
            SELECT 1 FROM proposal_application_completion_evidence evidence
            JOIN proposal_application_effects effect ON effect.workspace_id=evidence.workspace_id
                AND effect.user_id=evidence.user_id AND effect.application_id=evidence.application_id
            WHERE evidence.workspace_id=NEW.workspace_id AND evidence.user_id=NEW.user_id
              AND evidence.application_id=NEW.application_id AND evidence.capture_xid=pg_current_xact_id()
              AND evidence.created_at=NEW.created_at AND effect.ordinal=NEW.ordinal
              AND effect.item_id=NEW.item_id AND effect.snapshots_scrubbed_at IS NULL)
        THEN RAISE EXCEPTION 'proposal completion state requires original scoped direct effect'; END IF;
        RETURN NEW;
    END IF;
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'proposal completion companions retain immutable hashes'; END IF;
    IF ROW(NEW.workspace_id,NEW.user_id,NEW.application_id,NEW.ordinal,NEW.item_id,NEW.before_state_hash,NEW.created_at)
        IS DISTINCT FROM ROW(OLD.workspace_id,OLD.user_id,OLD.application_id,OLD.ordinal,OLD.item_id,OLD.before_state_hash,OLD.created_at)
        OR OLD.snapshots_scrubbed_at IS NOT NULL OR NEW.before_state_json IS NOT NULL THEN
        RAISE EXCEPTION 'proposal completion companion permits only one-way text scrubbing';
    END IF;
    SELECT undo_expires_at INTO expiry FROM proposal_applications
        WHERE workspace_id=OLD.workspace_id AND user_id=OLD.user_id AND id=OLD.application_id FOR KEY SHARE;
    IF NOT FOUND OR clock_timestamp()<expiry THEN
        RAISE EXCEPTION 'proposal completion state is still required for undo';
    END IF;
    NEW.snapshots_scrubbed_at:=clock_timestamp();
    RETURN NEW;
END
$guard$;

CREATE FUNCTION verify_proposal_completion_evidence() RETURNS trigger
LANGUAGE plpgsql AS $verify$
DECLARE expected_count integer;
BEGIN
    SELECT effect_count INTO expected_count FROM proposal_applications
        WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id AND id=NEW.application_id;
    IF NOT FOUND OR expected_count<>(SELECT count(*) FROM proposal_application_completion_states
        WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id AND application_id=NEW.application_id)
    THEN RAISE EXCEPTION 'proposal completion companions are incomplete'; END IF;
    RETURN NEW;
END
$verify$;

CREATE TRIGGER proposal_completion_evidence_guard BEFORE INSERT OR UPDATE OR DELETE ON proposal_application_completion_evidence
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_completion_evidence();
CREATE CONSTRAINT TRIGGER proposal_completion_evidence_complete AFTER INSERT ON proposal_application_completion_evidence
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_proposal_completion_evidence();
CREATE TRIGGER proposal_completion_state_guard BEFORE INSERT OR UPDATE OR DELETE ON proposal_application_completion_states
    FOR EACH ROW EXECUTE FUNCTION guard_proposal_completion_state();

-- A completion-aware application also fences unchanged evaluated ancestors.
-- Only such non-direct fences may retain their revision after undo. Legacy and
-- direct-effect fences continue to require an actual newer canonical revision.
DO $replace_undo_check$
DECLARE check_name name; matches integer;
BEGIN
    -- PostgreSQL names a column CHECK referring to another column as a table
    -- check on some versions. Resolve the exact two-column predicate instead
    -- of assuming the generated identifier.
    SELECT count(*), min(constraint_row.conname::text)::name INTO matches,check_name
    FROM pg_constraint constraint_row
    WHERE constraint_row.conrelid='proposal_application_fences'::regclass
      AND constraint_row.contype='c'
      AND pg_get_constraintdef(constraint_row.oid)='CHECK (((undo_revision IS NULL) OR (undo_revision > applied_revision)))';
    IF matches<>1 THEN RAISE EXCEPTION 'proposal undo revision constraint is not the known legacy predicate'; END IF;
    EXECUTE format('ALTER TABLE proposal_application_fences DROP CONSTRAINT %I',check_name);
END
$replace_undo_check$;
ALTER TABLE proposal_application_fences ADD CONSTRAINT proposal_application_fences_undo_revision_check
    CHECK(undo_revision IS NULL OR undo_revision>=applied_revision);
CREATE OR REPLACE FUNCTION guard_proposal_application_fence_update() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP='UPDATE'
        AND ROW(NEW.workspace_id,NEW.user_id,NEW.application_id,NEW.item_id,NEW.applied_revision,NEW.applied_deleted)
            IS NOT DISTINCT FROM ROW(OLD.workspace_id,OLD.user_id,OLD.application_id,OLD.item_id,OLD.applied_revision,OLD.applied_deleted)
        AND OLD.undo_revision IS NULL AND NEW.undo_revision IS NOT NULL
        AND (NEW.undo_revision>OLD.applied_revision OR (
            NEW.undo_revision=OLD.applied_revision AND EXISTS(SELECT 1 FROM proposal_application_completion_evidence
                WHERE workspace_id=NEW.workspace_id AND user_id=NEW.user_id AND application_id=NEW.application_id)
            AND NOT EXISTS(SELECT 1 FROM proposal_application_effects WHERE workspace_id=NEW.workspace_id
                AND user_id=NEW.user_id AND application_id=NEW.application_id AND item_id=NEW.item_id)))
    THEN RETURN NEW; END IF;
    RAISE EXCEPTION 'proposal application fences permit only a qualified undo revision' USING ERRCODE='23514';
END
$guard$;

-- Source history retained by completion effects is permanently immutable, just
-- as temporary bootstrap members pin it for their bounded snapshot lifetime.
CREATE OR REPLACE FUNCTION lock_item_bootstrap_history_mutation() RETURNS trigger
LANGUAGE plpgsql AS $history$
BEGIN
    PERFORM pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1',0));
    PERFORM pg_advisory_xact_lock(hashtextextended('dayweave.item-bootstrap.history-immutability.v1',0));
    IF TG_OP='TRUNCATE' AND (EXISTS(SELECT 1 FROM item_bootstrap_members) OR EXISTS(SELECT 1 FROM item_completion_effects))
    THEN RAISE EXCEPTION 'retained evidence still pins canonical history'; END IF;
    RETURN NULL;
END
$history$;

CREATE OR REPLACE FUNCTION reject_item_bootstrap_pinned_change() RETURNS trigger
LANGUAGE plpgsql AS $pinned$
BEGIN
    IF EXISTS(SELECT 1 FROM item_bootstrap_members WHERE change_sequence=OLD.sequence)
        OR EXISTS(SELECT 1 FROM item_completion_effects WHERE workspace_id=OLD.workspace_id
            AND item_id=OLD.item_id AND (before_item_revision=OLD.item_revision OR after_item_revision=OLD.item_revision))
    THEN RAISE EXCEPTION 'retained evidence still pins canonical history'; END IF;
    RETURN CASE WHEN TG_OP='DELETE' THEN OLD ELSE NEW END;
END
$pinned$;

CREATE FUNCTION reject_item_completion_truncate() RETURNS trigger
LANGUAGE plpgsql AS $truncate$
BEGIN RAISE EXCEPTION 'completion evidence requires guarded account purge'; END
$truncate$;

DO $guards$
DECLARE table_name text; function_name text; trusted_schema name:=current_schema();
BEGIN
    FOREACH table_name IN ARRAY ARRAY['item_completion_state','item_completion_evaluations','item_completion_effects',
        'item_completion_operations','proposal_application_completion_evidence','proposal_application_completion_states'] LOOP
        EXECUTE format('CREATE TRIGGER account_deletion_fence_guard BEFORE INSERT OR UPDATE OR DELETE ON %I.%I FOR EACH ROW EXECUTE FUNCTION %I.reject_fenced_workspace_mutation()',trusted_schema,table_name,trusted_schema);
        EXECUTE format('CREATE TRIGGER item_completion_no_truncate BEFORE TRUNCATE ON %I.%I FOR EACH STATEMENT EXECUTE FUNCTION %I.reject_item_completion_truncate()',trusted_schema,table_name,trusted_schema);
    END LOOP;
    FOREACH function_name IN ARRAY ARRAY['guard_item_change_completion_capture()',
        'valid_item_completion_revision(jsonb,bigint)',
        'valid_item_completion_reopen(jsonb,uuid)','valid_item_completion_state(jsonb)','valid_item_completion_snapshot(jsonb)',
        'guard_item_completion_state()','guard_item_completion_evaluation()','guard_item_completion_evidence()',
        'verify_item_completion_state()','verify_item_completion_effect()','verify_item_completion_evaluation()',
        'verify_item_completion_operation()','verify_item_completion_canonical_state()',
        'guard_proposal_completion_evidence()','guard_proposal_completion_state()',
        'verify_proposal_completion_evidence()','guard_proposal_application_fence_update()',
        'lock_item_bootstrap_history_mutation()','reject_item_bootstrap_pinned_change()','reject_item_completion_truncate()'] LOOP
        EXECUTE format('ALTER FUNCTION %I.%s SET search_path TO %I, pg_catalog, pg_temp',trusted_schema,function_name,trusted_schema);
        EXECUTE format('REVOKE ALL ON FUNCTION %I.%s FROM PUBLIC',trusted_schema,function_name);
    END LOOP;
END
$guards$;

-- Preserve the existing guarded purge algorithm, extending only its tenant inventories.
CREATE OR REPLACE FUNCTION purge_fenced_personal_account_scope(
    requested_deletion_id uuid,
    requested_expected_revision bigint,
    requested_request_hash bytea
) RETURNS TABLE (result_revision bigint, replayed boolean)
LANGUAGE plpgsql
SECURITY INVOKER
AS $purge$
DECLARE
    lifecycle account_deletion_lifecycles%ROWTYPE;
    receipt account_deletion_transition_receipts%ROWTYPE;
    target_table text;
    operation_at timestamptz;
    tenant_schema name := current_schema();
    lock_tables constant text[] := ARRAY[
        'account_recovery_codes', 'audit_operations', 'device_enrollments',
        'execution_defer_assessments', 'execution_defer_replacement_claims',
        'execution_defer_replacement_consumptions', 'execution_physical_indices',
        'execution_session_schedule_origins', 'execution_sessions', 'execution_state',
        'google_calendar_projection_rejections', 'google_oauth_cleanup_tokens',
        'google_oauth_guardian_resolutions', 'google_oauth_legacy_credential_quarantine',
        'google_oauth_scope_state', 'google_oauth_sessions', 'google_outbound_previews',
        'google_provider_identity_roots', 'google_schedule_publication_batches',
        'google_schedule_publication_mapping_origins',
        'google_schedule_publication_observations', 'google_schedule_publication_outbox',
        'google_schedule_publication_preview_changes',
        'google_schedule_publication_previews', 'google_sync_collections',
        'google_sync_outbox', 'google_sync_refresh_requests', 'google_sync_runs',
        'habit_changes', 'habit_missed_resolution_versions', 'habit_missed_resolutions',
        'habit_occurrence_evidence', 'habit_occurrence_outcomes',
        'habit_occurrence_publications', 'habit_occurrence_versions',
        'habit_operation_receipts', 'habit_pause_versions', 'habit_pauses',
        'idempotency_keys', 'item_bootstrap_members', 'item_bootstrap_snapshots',
        'item_changes', 'item_dependencies', 'item_hierarchy', 'items',
        'item_completion_state', 'item_completion_evaluations',
        'item_completion_effects', 'item_completion_operations',
        'proposal_application_completion_evidence', 'proposal_application_completion_states',
        'item_progress', 'item_progress_operations',
        'mcp_clients', 'mcp_proposal_submissions', 'outbox_messages',
        'proposal_application_effects', 'proposal_application_fences',
        'proposal_application_members', 'proposal_application_requests',
        'proposal_applications', 'proposal_apply_preview_members', 'proposal_apply_previews',
        'proposals', 'provider_accounts', 'provider_sync_cursors', 'provider_sync_mappings',
        'schedule_blocks', 'schedule_defer_replacement_placements',
        'schedule_deferred_placements', 'schedule_publication_requests',
        'schedule_revision_details', 'schedule_revisions', 'schedule_simulations',
        'sessions', 'users', 'workspace_members', 'workspaces'
    ];
    delete_order constant text[] := ARRAY[
        'proposal_application_completion_states', 'proposal_application_completion_evidence',
        'item_completion_operations', 'item_completion_effects',
        'item_completion_evaluations', 'item_completion_state',
        'item_bootstrap_members', 'item_bootstrap_snapshots',
        'google_schedule_publication_observations',
        'schedule_defer_replacement_placements',
        'google_schedule_publication_outbox',
        'execution_physical_indices',
        'execution_defer_replacement_consumptions',
        'google_schedule_publication_preview_changes',
        'execution_defer_replacement_claims',
        'proposal_application_requests', 'proposal_application_members',
        'proposal_application_fences', 'proposal_application_effects',
        'habit_missed_resolution_versions',
        'google_schedule_publication_mapping_origins',
        'google_schedule_publication_batches',
        'google_schedule_publication_previews',
        'execution_defer_assessments', 'schedule_deferred_placements',
        'provider_sync_mappings', 'proposal_applications', 'habit_pause_versions',
        'habit_occurrence_versions', 'habit_occurrence_publications',
        'habit_occurrence_outcomes', 'habit_missed_resolutions',
        'google_sync_outbox', 'google_outbound_previews',
        'google_oauth_guardian_resolutions', 'google_oauth_cleanup_tokens',
        'google_calendar_projection_rejections', 'execution_state',
        'execution_session_schedule_origins', 'schedule_simulations',
        'schedule_revision_details', 'schedule_publication_requests', 'schedule_blocks',
        'provider_sync_cursors', 'proposal_apply_preview_members',
        'mcp_proposal_submissions', 'item_hierarchy', 'item_dependencies',
        'habit_pauses', 'habit_occurrence_evidence', 'google_sync_runs',
        'google_sync_refresh_requests', 'google_sync_collections', 'google_oauth_sessions',
        'google_oauth_legacy_credential_quarantine', 'execution_sessions',
        'device_enrollments', 'audit_operations', 'account_recovery_codes', 'sessions',
        'schedule_revisions', 'provider_accounts', 'proposals', 'proposal_apply_previews',
        'item_progress_operations', 'item_progress',
        'mcp_clients', 'items', 'google_provider_identity_roots',
        'google_oauth_scope_state', 'workspace_members', 'outbox_messages',
        'item_changes', 'idempotency_keys', 'habit_operation_receipts', 'habit_changes'
    ];
BEGIN
    IF requested_deletion_id IS NULL
       OR requested_expected_revision IS NULL
       OR requested_expected_revision <= 0
       OR requested_request_hash IS NULL
       OR octet_length(requested_request_hash) <> 32
    THEN
        RAISE EXCEPTION USING ERRCODE = 'DWREQ', MESSAGE = 'invalid purge request';
    END IF;

    SELECT * INTO receipt
      FROM account_deletion_transition_receipts
     WHERE deletion_id = requested_deletion_id
       AND request_hash = requested_request_hash;
    IF FOUND THEN
        IF receipt.from_status <> 'purge'
           OR receipt.to_status <> 'backup_wait'
           OR receipt.expected_revision <> requested_expected_revision
        THEN
            RAISE EXCEPTION USING ERRCODE = 'DWCON', MESSAGE = 'purge replay conflicts';
        END IF;
        result_revision := receipt.result_revision;
        replayed := true;
        RETURN NEXT;
        RETURN;
    END IF;

    SELECT * INTO lifecycle
      FROM account_deletion_lifecycles
     WHERE id = requested_deletion_id
     FOR UPDATE;
    IF NOT FOUND
       OR lifecycle.status <> 'purge'
       OR lifecycle.revision <> requested_expected_revision
    THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON', MESSAGE = 'purge state conflicts';
    END IF;

    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.global-mutation-barrier.v1', 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.subject.v1:' || encode(lifecycle.owner_subject_hash, 'hex'), 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.user.v1:' || lifecycle.user_id::text, 0
    ));
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'dayweave.account-deletion.workspace.v1:' || lifecycle.workspace_id::text, 0
    ));

    IF NOT EXISTS (
        SELECT 1 FROM account_deletion_fences
         WHERE deletion_id = lifecycle.id
           AND workspace_id = lifecycle.workspace_id
           AND user_id = lifecycle.user_id
           AND owner_subject_hash = lifecycle.owner_subject_hash
    ) OR NOT EXISTS (
        SELECT 1 FROM workspaces
         WHERE id = lifecycle.workspace_id AND owner_user_id = lifecycle.user_id
    ) OR (SELECT count(*) FROM workspaces WHERE owner_user_id = lifecycle.user_id) <> 1
      OR (SELECT count(*) FROM workspace_members
           WHERE workspace_id = lifecycle.workspace_id) <> 1
      OR (SELECT count(*) FROM workspace_members
           WHERE user_id = lifecycle.user_id) <> 1
      OR NOT EXISTS (
        SELECT 1 FROM workspace_members
         WHERE workspace_id = lifecycle.workspace_id
           AND user_id = lifecycle.user_id
           AND role = 'owner' AND removed_at IS NULL
    ) THEN
        RAISE EXCEPTION USING ERRCODE = 'DWSCP', MESSAGE = 'purge scope is not personal';
    END IF;
    operation_at := clock_timestamp();

    FOREACH target_table IN ARRAY lock_tables LOOP
        EXECUTE format(
            'LOCK TABLE %I.%I IN ACCESS EXCLUSIVE MODE', tenant_schema, target_table
        );
    END LOOP;
    SET CONSTRAINTS ALL DEFERRED;
    FOREACH target_table IN ARRAY lock_tables LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I DISABLE TRIGGER USER', tenant_schema, target_table
        );
    END LOOP;

    FOREACH target_table IN ARRAY delete_order LOOP
        EXECUTE format(
            'DELETE FROM %I.%I WHERE workspace_id = $1', tenant_schema, target_table
        )
        USING lifecycle.workspace_id;
    END LOOP;
    DELETE FROM workspaces WHERE id = lifecycle.workspace_id;
    DELETE FROM users WHERE id = lifecycle.user_id;

    -- Drain deferred FK events before changing trigger enablement again.
    -- Failure here rolls the whole purge and every DISABLE TRIGGER back.
    SET CONSTRAINTS ALL IMMEDIATE;
    FOREACH target_table IN ARRAY lock_tables LOOP
        EXECUTE format(
            'ALTER TABLE %I.%I ENABLE TRIGGER USER', tenant_schema, target_table
        );
    END LOOP;

    UPDATE account_deletion_lifecycles
       SET status = 'backup_wait',
           revision = revision + 1,
           local_purge_completed_at = operation_at,
           backup_wait_at = operation_at,
           updated_at = operation_at
     WHERE id = lifecycle.id
       AND status = 'purge'
       AND revision = requested_expected_revision;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING ERRCODE = 'DWCON', MESSAGE = 'purge state conflicts';
    END IF;

    INSERT INTO account_deletion_transition_receipts (
        deletion_id, request_hash, from_status, to_status,
        expected_revision, result_revision, occurred_at, failure_code
    ) VALUES (
        lifecycle.id, requested_request_hash, 'purge', 'backup_wait',
        requested_expected_revision, requested_expected_revision + 1, operation_at, NULL
    );

    result_revision := requested_expected_revision + 1;
    replayed := false;
    RETURN NEXT;
END
$purge$;

DO $pin_purge$ BEGIN
    EXECUTE format('ALTER FUNCTION %I.purge_fenced_personal_account_scope(uuid,bigint,bytea) SET search_path TO %I, pg_catalog, pg_temp', current_schema(),current_schema());
END $pin_purge$;
REVOKE ALL ON FUNCTION purge_fenced_personal_account_scope(uuid,bigint,bytea) FROM PUBLIC;
