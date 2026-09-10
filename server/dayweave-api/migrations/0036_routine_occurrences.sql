-- Publication-qualified recurring task/routine instances. Templates remain
-- unchanged; immutable manifests and operation/change evidence retain private
-- titles and reopening reasons until guarded account purge, with no TTL.
CREATE TABLE routine_occurrences (
    workspace_id uuid NOT NULL REFERENCES workspaces(id),
    id uuid NOT NULL CHECK(id<>'00000000-0000-0000-0000-000000000000'::uuid),
    user_id uuid NOT NULL,
    series_item_id uuid NOT NULL,
    occurrence_id uuid NOT NULL,
    definition_hash text NOT NULL CHECK(definition_hash ~ '^sha256:[0-9a-f]{64}$'),
    manifest_json jsonb NOT NULL CHECK(jsonb_typeof(manifest_json)='object' AND octet_length(manifest_json::text)<=8388608),
    member_count integer NOT NULL CHECK(member_count BETWEEN 1 AND 10000),
    first_schedule_revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK(isfinite(created_at)),
    capture_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    PRIMARY KEY(workspace_id,id),
    UNIQUE(workspace_id,series_item_id,occurrence_id),
    FOREIGN KEY(workspace_id,user_id) REFERENCES workspace_members(workspace_id,user_id),
    FOREIGN KEY(workspace_id,series_item_id) REFERENCES items(workspace_id,id),
    FOREIGN KEY(workspace_id,first_schedule_revision_id) REFERENCES schedule_revisions(workspace_id,id)
);

CREATE TABLE routine_occurrence_members (
    workspace_id uuid NOT NULL,
    instance_id uuid NOT NULL,
    item_id uuid NOT NULL,
    parent_item_id uuid,
    source_revision bigint NOT NULL CHECK(source_revision>0),
    PRIMARY KEY(workspace_id,instance_id,item_id),
    FOREIGN KEY(workspace_id,instance_id) REFERENCES routine_occurrences(workspace_id,id),
    FOREIGN KEY(workspace_id,instance_id,parent_item_id) REFERENCES routine_occurrence_members(workspace_id,instance_id,item_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(workspace_id,item_id,source_revision) REFERENCES item_changes(workspace_id,item_id,item_revision)
);
CREATE INDEX routine_occurrence_member_source ON routine_occurrence_members(workspace_id,item_id,source_revision);

CREATE TABLE routine_occurrence_state (
    workspace_id uuid NOT NULL,
    instance_id uuid NOT NULL,
    revision bigint NOT NULL CHECK(revision>0),
    aggregate_json jsonb NOT NULL CHECK(jsonb_typeof(aggregate_json)='object' AND octet_length(aggregate_json::text)<=8388608),
    updated_at timestamptz NOT NULL CHECK(isfinite(updated_at)),
    PRIMARY KEY(workspace_id,instance_id),
    FOREIGN KEY(workspace_id,instance_id) REFERENCES routine_occurrences(workspace_id,id)
);

CREATE TABLE routine_occurrence_changes (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    workspace_id uuid NOT NULL,
    instance_id uuid NOT NULL,
    revision bigint NOT NULL CHECK(revision>0),
    operation_id uuid,
    before_json jsonb CHECK(before_json IS NULL OR (jsonb_typeof(before_json)='object' AND octet_length(before_json::text)<=8388608)),
    aggregate_json jsonb NOT NULL CHECK(jsonb_typeof(aggregate_json)='object' AND octet_length(aggregate_json::text)<=8388608),
    effects_json jsonb NOT NULL CHECK(jsonb_typeof(effects_json)='array' AND octet_length(effects_json::text)<=8388608),
    changed_at timestamptz NOT NULL CHECK(isfinite(changed_at)),
    capture_xid xid8 NOT NULL DEFAULT pg_current_xact_id(),
    UNIQUE(workspace_id,sequence),
    UNIQUE(workspace_id,instance_id,revision),
    FOREIGN KEY(workspace_id,instance_id) REFERENCES routine_occurrences(workspace_id,id),
    CHECK((revision=1)=(before_json IS NULL)),
    CHECK((revision=1)=(operation_id IS NULL))
);
CREATE INDEX routine_occurrence_changes_delta ON routine_occurrence_changes(workspace_id,sequence);

CREATE TABLE routine_occurrence_operations (
    workspace_id uuid NOT NULL,
    operation_id uuid NOT NULL CHECK(operation_id<>'00000000-0000-0000-0000-000000000000'::uuid),
    instance_id uuid NOT NULL,
    member_item_id uuid NOT NULL,
    actor_user_id uuid NOT NULL,
    actor_session_id uuid,
    change_sequence bigint NOT NULL,
    request_json jsonb NOT NULL CHECK(jsonb_typeof(request_json)='object' AND octet_length(request_json::text)<=1048576),
    result_json jsonb NOT NULL CHECK(jsonb_typeof(result_json)='object' AND octet_length(result_json::text)<=8388608),
    recorded_at timestamptz NOT NULL CHECK(isfinite(recorded_at)),
    PRIMARY KEY(workspace_id,operation_id),
    FOREIGN KEY(workspace_id,instance_id,member_item_id) REFERENCES routine_occurrence_members(workspace_id,instance_id,item_id),
    FOREIGN KEY(workspace_id,change_sequence) REFERENCES routine_occurrence_changes(workspace_id,sequence),
    FOREIGN KEY(workspace_id,actor_user_id) REFERENCES workspace_members(workspace_id,user_id)
);

CREATE TABLE routine_occurrence_publications (
    workspace_id uuid NOT NULL,
    instance_id uuid NOT NULL,
    schedule_revision_id uuid NOT NULL,
    source_revisions jsonb NOT NULL CHECK(jsonb_typeof(source_revisions)='object' AND octet_length(source_revisions::text)<=1048576),
    recorded_at timestamptz NOT NULL CHECK(isfinite(recorded_at)),
    PRIMARY KEY(workspace_id,instance_id,schedule_revision_id),
    FOREIGN KEY(workspace_id,instance_id) REFERENCES routine_occurrences(workspace_id,id),
    FOREIGN KEY(workspace_id,schedule_revision_id) REFERENCES schedule_revisions(workspace_id,id)
);

CREATE FUNCTION guard_routine_occurrence_immutable() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'routine occurrence evidence is immutable'; END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||NEW.workspace_id::text,0));
    RETURN NEW;
END
$guard$;

CREATE FUNCTION guard_routine_occurrence_manifest() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||NEW.workspace_id::text,0));
    PERFORM pg_advisory_xact_lock_shared(hashtextextended('dayweave.item-bootstrap.history-immutability.v1',0));
    IF NEW.capture_xid<>pg_current_xact_id()
       OR NEW.manifest_json->>'id' IS DISTINCT FROM NEW.id::text
       OR NEW.manifest_json->>'series_item_id' IS DISTINCT FROM NEW.series_item_id::text
       OR NEW.manifest_json->>'occurrence_id' IS DISTINCT FROM NEW.occurrence_id::text
       OR NEW.manifest_json->>'definition_hash' IS DISTINCT FROM NEW.definition_hash
       OR NEW.manifest_json->'schema_version' IS DISTINCT FROM '1'::jsonb
       OR jsonb_typeof(NEW.manifest_json->'members') IS DISTINCT FROM 'array'
       OR jsonb_array_length(NEW.manifest_json->'members')<>NEW.member_count
       OR NOT EXISTS(SELECT 1 FROM items item WHERE item.workspace_id=NEW.workspace_id AND item.id=NEW.series_item_id
            AND item.trashed_at IS NULL AND item.kind IN ('task','routine') AND item.recurrence IS NOT NULL
            AND NEW.manifest_json->>'timezone_name'=item.timezone_name)
       OR NOT EXISTS(SELECT 1 FROM workspace_members member JOIN workspaces workspace ON workspace.id=member.workspace_id JOIN users owner ON owner.id=workspace.owner_user_id
            WHERE member.workspace_id=NEW.workspace_id AND member.user_id=NEW.user_id AND member.role='owner' AND member.removed_at IS NULL AND workspace.owner_user_id=NEW.user_id
            AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL)
    THEN RAISE EXCEPTION 'invalid routine occurrence manifest capture'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION guard_routine_occurrence_member() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE manifest routine_occurrences%ROWTYPE;
BEGIN
    SELECT * INTO manifest FROM routine_occurrences WHERE workspace_id=NEW.workspace_id AND id=NEW.instance_id;
    IF NOT FOUND OR manifest.capture_xid<>pg_current_xact_id()
       OR NOT EXISTS(SELECT 1 FROM items item JOIN item_changes change
          ON change.workspace_id=item.workspace_id AND change.item_id=item.id AND change.item_revision=item.revision
          WHERE item.workspace_id=NEW.workspace_id AND item.id=NEW.item_id AND item.revision=NEW.source_revision
            AND item.trashed_at IS NULL AND change.change_kind='upsert')
       OR NOT EXISTS(SELECT 1 FROM jsonb_array_elements(manifest.manifest_json->'members') value
          WHERE value->>'item_id'=NEW.item_id::text AND value->>'source_revision'=NEW.source_revision::text
            AND value->>'parent_id' IS NOT DISTINCT FROM NEW.parent_item_id::text)
       OR NOT EXISTS(SELECT 1 FROM items item JOIN LATERAL jsonb_array_elements(manifest.manifest_json->'members') value ON value->>'item_id'=item.id::text
           WHERE item.workspace_id=NEW.workspace_id AND item.id=NEW.item_id
             AND item.status IN ('inbox','planned','blocked')
             AND value->>'title'=item.title AND value->>'kind'=item.kind
             AND value->'recurs'=to_jsonb(item.recurrence IS NOT NULL)
             AND value->>'sibling_order'=item.sibling_order::text
             AND value->'initial_open'=jsonb_build_object('status',item.status,'blocked_reason_kind',item.blocked_reason_kind,'blocked_by_item_id',item.blocked_by_item_id,'blocked_reason',item.blocked_reason)
             AND value->'required_for_parent'=COALESCE((SELECT state_json->'required_for_parent' FROM item_completion_state WHERE workspace_id=NEW.workspace_id AND item_id=NEW.item_id),'true'::jsonb))
    THEN RAISE EXCEPTION 'routine occurrence member lacks exact current capture'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION verify_routine_occurrence_manifest() RETURNS trigger
LANGUAGE plpgsql AS $verify$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||NEW.workspace_id::text,0));
    IF (SELECT count(*) FROM routine_occurrence_members WHERE workspace_id=NEW.workspace_id AND instance_id=NEW.id)<>NEW.member_count
       OR (SELECT count(*) FROM routine_occurrence_members WHERE workspace_id=NEW.workspace_id AND instance_id=NEW.id AND parent_item_id IS NULL)<>1
       OR NOT EXISTS(SELECT 1 FROM routine_occurrence_members WHERE workspace_id=NEW.workspace_id AND instance_id=NEW.id AND item_id=NEW.series_item_id AND parent_item_id IS NULL)
       OR NOT EXISTS(SELECT 1 FROM routine_occurrence_state WHERE workspace_id=NEW.workspace_id AND instance_id=NEW.id)
       OR NOT EXISTS(SELECT 1 FROM routine_occurrence_publications WHERE workspace_id=NEW.workspace_id AND instance_id=NEW.id AND schedule_revision_id=NEW.first_schedule_revision_id)
    THEN RAISE EXCEPTION 'routine occurrence manifest is incomplete'; END IF;
    -- Every admitted member must include all current direct children, and each
    -- nonroot edge must be the canonical edge. This seals the complete subtree.
    IF EXISTS(SELECT 1 FROM routine_occurrence_members member JOIN item_hierarchy edge
        ON edge.workspace_id=member.workspace_id AND edge.parent_item_id=member.item_id
        JOIN items child ON child.workspace_id=edge.workspace_id AND child.id=edge.child_item_id AND child.trashed_at IS NULL
        WHERE member.workspace_id=NEW.workspace_id AND member.instance_id=NEW.id
          AND NOT EXISTS(SELECT 1 FROM routine_occurrence_members included WHERE included.workspace_id=NEW.workspace_id AND included.instance_id=NEW.id AND included.item_id=child.id))
       OR EXISTS(SELECT 1 FROM routine_occurrence_members member WHERE member.workspace_id=NEW.workspace_id AND member.instance_id=NEW.id AND member.parent_item_id IS NOT NULL
          AND NOT EXISTS(SELECT 1 FROM item_hierarchy edge WHERE edge.workspace_id=member.workspace_id AND edge.child_item_id=member.item_id AND edge.parent_item_id=member.parent_item_id))
    THEN RAISE EXCEPTION 'routine occurrence manifest is not the complete canonical subtree'; END IF;
    RETURN NEW;
END
$verify$;

CREATE FUNCTION valid_routine_occurrence_timestamp(value jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $instant$
BEGIN
    RETURN coalesce(jsonb_typeof(value)='string'
        AND value#>>'{}' ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?(Z|\+00:00)$'
        AND isfinite((value#>>'{}')::timestamptz),false);
EXCEPTION WHEN invalid_text_representation OR datetime_field_overflow OR invalid_datetime_format THEN RETURN false;
END
$instant$;

CREATE FUNCTION valid_routine_occurrence_member_state(value jsonb, is_parent boolean) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $member$
DECLARE item_id uuid;
BEGIN
    IF NOT coalesce(jsonb_typeof(value)='object'
        AND value ?& ARRAY['item_id','revision','status','required_for_parent','mode','open','provenance','completed_at','updated_at']
        AND value-ARRAY['item_id','revision','status','required_for_parent','mode','open','provenance','completed_at','updated_at']='{}'::jsonb
        AND valid_item_completion_state(value-ARRAY['status','open','completed_at'])
        AND valid_item_completion_revision(value->'revision',1)
        AND jsonb_typeof(value->'status')='string'
        AND value->>'status' IN ('inbox','planned','blocked','completed','skipped','cancelled'),false)
    THEN RETURN false; END IF;
    item_id:=(value->>'item_id')::uuid;
    IF NOT valid_item_completion_reopen(value->'open',item_id)
       OR (value->>'status' IN ('inbox','planned','blocked') AND value->'status' IS DISTINCT FROM value#>'{open,status}')
       OR (value->>'status'='completed') IS DISTINCT FROM (value->'completed_at'<>'null'::jsonb)
       OR (value->'completed_at'<>'null'::jsonb AND NOT valid_routine_occurrence_timestamp(value->'completed_at'))
       OR (NOT is_parent AND (value->>'mode'<>'automatic' OR value->'provenance'<>'null'::jsonb))
       OR (is_parent AND value->>'status' IN ('completed','skipped','cancelled') AND value->'provenance'='null'::jsonb)
       OR (value->'provenance'<>'null'::jsonb AND (value->>'status'<>'completed' OR value#>'{provenance,reopen}' IS DISTINCT FROM value->'open'))
    THEN RETURN false; END IF;
    RETURN true;
END
$member$;

CREATE FUNCTION valid_routine_occurrence_aggregate(value jsonb, manifest jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $aggregate$
DECLARE members jsonb; definitions jsonb; parents jsonb; member jsonb; member_id text; member_count integer;
BEGIN
    IF NOT coalesce(jsonb_typeof(value)='object'
        AND value ?& ARRAY['manifest','revision','members'] AND value-ARRAY['manifest','revision','members']='{}'::jsonb
        AND value->'manifest'=manifest AND valid_item_completion_revision(value->'revision',1)
        AND jsonb_typeof(value->'members')='array' AND jsonb_typeof(manifest->'members')='array',false)
    THEN RETURN false; END IF;
    member_count:=jsonb_array_length(value->'members');
    IF member_count NOT BETWEEN 1 AND 10000 OR member_count<>jsonb_array_length(manifest->'members')
       OR EXISTS(SELECT 1 FROM jsonb_array_elements(value->'members') entry WHERE jsonb_typeof(entry->'item_id') IS DISTINCT FROM 'string')
       OR EXISTS(SELECT 1 FROM jsonb_array_elements(manifest->'members') entry WHERE jsonb_typeof(entry->'item_id') IS DISTINCT FROM 'string')
    THEN RETURN false; END IF;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO members FROM jsonb_array_elements(value->'members') entry;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO definitions FROM jsonb_array_elements(manifest->'members') entry;
    SELECT coalesce(jsonb_object_agg(entry->>'parent_id',true) FILTER(WHERE entry->>'parent_id' IS NOT NULL),'{}'::jsonb)
        INTO parents FROM jsonb_array_elements(manifest->'members') entry;
    IF (SELECT count(*) FROM jsonb_object_keys(members))<>member_count
       OR (SELECT count(*) FROM jsonb_object_keys(definitions))<>member_count
    THEN RETURN false; END IF;
    FOR member_id,member IN SELECT * FROM jsonb_each(members) LOOP
        IF NOT definitions ? member_id OR NOT valid_routine_occurrence_member_state(member,parents ? member_id)
           OR (member->>'revision')::numeric>(value->>'revision')::numeric
        THEN RETURN false; END IF;
    END LOOP;
    RETURN true;
END
$aggregate$;

CREATE FUNCTION guard_routine_occurrence_change() RETURNS trigger
LANGUAGE plpgsql AS $guard$
DECLARE current_state routine_occurrence_state%ROWTYPE; manifest routine_occurrences%ROWTYPE;
    definitions jsonb; previous_members jsonb; effect_map jsonb; member jsonb; previous jsonb; effect jsonb;
    member_id text; changed_count integer:=0;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('dayweave.routine-occurrences.v1:'||NEW.workspace_id::text,0));
    SELECT * INTO manifest FROM routine_occurrences WHERE workspace_id=NEW.workspace_id AND id=NEW.instance_id;
    IF NOT FOUND OR NEW.capture_xid<>pg_current_xact_id()
       OR NEW.aggregate_json->>'revision' IS DISTINCT FROM NEW.revision::text
       OR NOT valid_routine_occurrence_aggregate(NEW.aggregate_json,manifest.manifest_json)
    THEN RAISE EXCEPTION 'routine occurrence change has invalid immutable identity'; END IF;
    SELECT * INTO current_state FROM routine_occurrence_state WHERE workspace_id=NEW.workspace_id AND instance_id=NEW.instance_id;
    IF NEW.revision=1 THEN
        IF FOUND OR manifest.capture_xid<>pg_current_xact_id() OR NEW.changed_at<>manifest.created_at OR NEW.effects_json<>'[]'::jsonb
        THEN RAISE EXCEPTION 'routine occurrence already initialized or lacks first capture'; END IF;
        SELECT jsonb_object_agg(entry->>'item_id',entry) INTO definitions FROM jsonb_array_elements(manifest.manifest_json->'members') entry;
        FOR member IN SELECT * FROM jsonb_array_elements(NEW.aggregate_json->'members') LOOP
            previous:=definitions->(member->>'item_id');
            IF member->'revision'<>'1'::jsonb OR member->'status' IS DISTINCT FROM previous#>'{initial_open,status}'
               OR member->'required_for_parent' IS DISTINCT FROM previous->'required_for_parent'
               OR member->>'mode'<>'automatic' OR member->'open' IS DISTINCT FROM previous->'initial_open'
               OR member->'provenance'<>'null'::jsonb OR member->'completed_at'<>'null'::jsonb
               OR (member->>'updated_at')::timestamptz<>NEW.changed_at
            THEN RAISE EXCEPTION 'routine occurrence initial member differs from exact open defaults'; END IF;
        END LOOP;
    ELSIF NOT FOUND OR NEW.revision<>current_state.revision+1 OR NEW.before_json IS DISTINCT FROM current_state.aggregate_json THEN
        RAISE EXCEPTION 'routine occurrence change is not the next exact state';
    ELSE
        IF NOT valid_routine_occurrence_aggregate(NEW.before_json,manifest.manifest_json)
           OR jsonb_array_length(NEW.effects_json) NOT BETWEEN 1 AND manifest.member_count
           OR EXISTS(SELECT 1 FROM jsonb_array_elements(NEW.effects_json) entry WHERE NOT coalesce(
                jsonb_typeof(entry)='object' AND entry ?& ARRAY['before','after','reason'] AND entry-ARRAY['before','after','reason']='{}'::jsonb
                AND jsonb_typeof(entry#>'{after,item_id}')='string' AND jsonb_typeof(entry->'reason')='string'
                AND entry->>'reason' IN ('unchanged','outcome_recorded','reopened','policy_reviewed','occurrence_evidence_required',
                    'automatically_completed','automatically_reopened','manually_completed','manually_kept_open','manual_completion_released'),false))
        THEN RAISE EXCEPTION 'routine occurrence effect list is invalid'; END IF;
        SELECT jsonb_object_agg(entry->>'item_id',entry) INTO previous_members FROM jsonb_array_elements(NEW.before_json->'members') entry;
        SELECT jsonb_object_agg(entry#>>'{after,item_id}',entry) INTO effect_map FROM jsonb_array_elements(NEW.effects_json) entry;
        IF (SELECT count(*) FROM jsonb_object_keys(effect_map))<>jsonb_array_length(NEW.effects_json)
        THEN RAISE EXCEPTION 'routine occurrence repeats a member effect'; END IF;
        FOR member IN SELECT * FROM jsonb_array_elements(NEW.aggregate_json->'members') LOOP
            member_id:=member->>'item_id'; previous:=previous_members->member_id; effect:=effect_map->member_id;
            IF member=previous THEN
                IF effect IS NOT NULL THEN RAISE EXCEPTION 'unchanged occurrence member has an effect'; END IF;
            ELSE
                changed_count:=changed_count+1;
                IF (member->>'revision')::numeric<>(previous->>'revision')::numeric+1
                   OR (member->>'updated_at')::timestamptz<>NEW.changed_at
                   OR effect->'before' IS DISTINCT FROM previous OR effect->'after' IS DISTINCT FROM member
                   OR (member->>'status'='completed' AND member->'completed_at' IS DISTINCT FROM
                        CASE WHEN previous->'completed_at'<>'null'::jsonb THEN previous->'completed_at' ELSE member->'updated_at' END)
                THEN RAISE EXCEPTION 'routine occurrence member lacks its exact next effect'; END IF;
            END IF;
        END LOOP;
        IF changed_count<>jsonb_array_length(NEW.effects_json)
        THEN RAISE EXCEPTION 'routine occurrence effect set differs from changed members'; END IF;
    END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION guard_routine_occurrence_state() RETURNS trigger
LANGUAGE plpgsql AS $guard$
BEGIN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'routine occurrence state requires guarded purge'; END IF;
    IF TG_OP='UPDATE' AND (ROW(NEW.workspace_id,NEW.instance_id) IS DISTINCT FROM ROW(OLD.workspace_id,OLD.instance_id) OR NEW.revision<>OLD.revision+1)
    THEN RAISE EXCEPTION 'routine occurrence state revision must advance exactly once'; END IF;
    IF NOT EXISTS(SELECT 1 FROM routine_occurrence_changes change WHERE change.workspace_id=NEW.workspace_id AND change.instance_id=NEW.instance_id
        AND change.revision=NEW.revision AND change.aggregate_json=NEW.aggregate_json AND change.changed_at=NEW.updated_at AND change.capture_xid=pg_current_xact_id())
    THEN RAISE EXCEPTION 'routine occurrence state lacks current transaction evidence'; END IF;
    RETURN NEW;
END
$guard$;

CREATE FUNCTION verify_routine_occurrence_change() RETURNS trigger
LANGUAGE plpgsql AS $verify$
BEGIN
    IF NOT EXISTS(SELECT 1 FROM routine_occurrence_state state WHERE state.workspace_id=NEW.workspace_id AND state.instance_id=NEW.instance_id AND state.revision>=NEW.revision)
       OR (NEW.operation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM routine_occurrence_operations operation WHERE operation.workspace_id=NEW.workspace_id AND operation.operation_id=NEW.operation_id AND operation.change_sequence=NEW.sequence))
    THEN RAISE EXCEPTION 'routine occurrence change lacks atomic state or receipt'; END IF;
    RETURN NEW;
END
$verify$;

CREATE FUNCTION valid_routine_occurrence_snapshot(value jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE STRICT AS $snapshot$
DECLARE evaluations jsonb; states jsonb; evaluation jsonb; counts jsonb; member_id text; field_name text; member_count integer;
BEGIN
    IF NOT coalesce(jsonb_typeof(value)='object'
        AND value ?& ARRAY['schema_version','aggregate','evidence_hash','fresh_edit_eligible','members']
        AND value-ARRAY['schema_version','aggregate','evidence_hash','fresh_edit_eligible','members']='{}'::jsonb
        AND value->'schema_version'='1'::jsonb
        AND valid_routine_occurrence_aggregate(value->'aggregate',value#>'{aggregate,manifest}')
        AND jsonb_typeof(value->'evidence_hash')='string' AND value->>'evidence_hash' ~ '^sha256:[0-9a-f]{64}$'
        AND jsonb_typeof(value->'fresh_edit_eligible')='boolean'
        AND jsonb_typeof(value->'members')='array',false)
    THEN RETURN false; END IF;
    member_count:=jsonb_array_length(value#>'{aggregate,members}');
    IF jsonb_array_length(value->'members')<>member_count
       OR EXISTS(SELECT 1 FROM jsonb_array_elements(value->'members') entry WHERE jsonb_typeof(entry->'item_id') IS DISTINCT FROM 'string')
    THEN RETURN false; END IF;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO evaluations FROM jsonb_array_elements(value->'members') entry;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO states FROM jsonb_array_elements(value#>'{aggregate,members}') entry;
    IF (SELECT count(*) FROM jsonb_object_keys(evaluations))<>member_count THEN RETURN false; END IF;
    FOR member_id,evaluation IN SELECT * FROM jsonb_each(evaluations) LOOP
        IF NOT coalesce(states ? member_id
            AND evaluation ?& ARRAY['item_id','counts','occurrence_evidence_required','reason']
            AND evaluation-ARRAY['item_id','counts','occurrence_evidence_required','reason']='{}'::jsonb
            AND jsonb_typeof(evaluation->'occurrence_evidence_required')='boolean'
            AND jsonb_typeof(evaluation->'reason')='string'
            AND evaluation->>'reason' IN ('unchanged','outcome_recorded','reopened','policy_reviewed','occurrence_evidence_required',
                'automatically_completed','automatically_reopened','manually_completed','manually_kept_open','manual_completion_released'),false)
        THEN RETURN false; END IF;
        counts:=evaluation->'counts';
        IF NOT coalesce(jsonb_typeof(counts)='object'
            AND counts ?& ARRAY['required_descendants','completed','incomplete','occurrence_evidence_required']
            AND counts-ARRAY['required_descendants','completed','incomplete','occurrence_evidence_required']='{}'::jsonb,false)
        THEN RETURN false; END IF;
        FOREACH field_name IN ARRAY ARRAY['required_descendants','completed','incomplete','occurrence_evidence_required'] LOOP
            IF NOT coalesce(valid_item_completion_revision(counts->field_name,0),false) THEN RETURN false; END IF;
        END LOOP;
        IF (counts->>'required_descendants')::numeric>=member_count
           OR (counts->>'completed')::numeric+(counts->>'incomplete')::numeric+(counts->>'occurrence_evidence_required')::numeric<>(counts->>'required_descendants')::numeric
        THEN RETURN false; END IF;
    END LOOP;
    RETURN true;
END
$snapshot$;

CREATE FUNCTION verify_routine_occurrence_operation() RETURNS trigger
LANGUAGE plpgsql AS $verify$
DECLARE change routine_occurrence_changes%ROWTYPE; before_members jsonb; after_members jsonb; evaluations jsonb;
    previous jsonb; member jsonb; effect jsonb; action jsonb; is_parent boolean;
BEGIN
    SELECT * INTO change FROM routine_occurrence_changes WHERE workspace_id=NEW.workspace_id AND sequence=NEW.change_sequence;
    IF NOT FOUND OR change.capture_xid<>pg_current_xact_id()
       OR change.instance_id<>NEW.instance_id OR change.operation_id IS DISTINCT FROM NEW.operation_id
       OR change.changed_at<>NEW.recorded_at
       OR NOT coalesce(NEW.request_json ?& ARRAY['schema_version','operation_id','expected_instance_revision','expected_member_revision','expected_evidence_hash','action']
            AND NEW.request_json-ARRAY['schema_version','operation_id','expected_instance_revision','expected_member_revision','expected_evidence_hash','action']='{}'::jsonb
            AND NEW.request_json->'schema_version'='1'::jsonb
            AND NEW.request_json->>'operation_id'=NEW.operation_id::text
            AND valid_item_completion_revision(NEW.request_json->'expected_instance_revision',1)
            AND valid_item_completion_revision(NEW.request_json->'expected_member_revision',1)
            AND jsonb_typeof(NEW.request_json->'expected_evidence_hash')='string'
            AND NEW.request_json->>'expected_evidence_hash' ~ '^sha256:[0-9a-f]{64}$',false)
       OR NEW.request_json->>'expected_instance_revision' IS DISTINCT FROM (change.revision-1)::text
       OR NEW.result_json->'aggregate' IS DISTINCT FROM change.aggregate_json
       OR NOT valid_routine_occurrence_snapshot(NEW.result_json)
       OR NEW.result_json->'fresh_edit_eligible' IS DISTINCT FROM 'true'::jsonb
    THEN RAISE EXCEPTION 'routine occurrence operation does not match its exact transition'; END IF;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO before_members FROM jsonb_array_elements(change.before_json->'members') entry;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO after_members FROM jsonb_array_elements(change.aggregate_json->'members') entry;
    SELECT jsonb_object_agg(entry->>'item_id',entry) INTO evaluations FROM jsonb_array_elements(NEW.result_json->'members') entry;
    previous:=before_members->NEW.member_item_id::text; member:=after_members->NEW.member_item_id::text;
    IF previous IS NULL OR member IS NULL
       OR NEW.request_json->'expected_member_revision' IS DISTINCT FROM previous->'revision'
       OR (member->>'revision')::numeric<>(previous->>'revision')::numeric+1
       OR NOT EXISTS(SELECT 1 FROM jsonb_array_elements(change.effects_json) entry WHERE entry->'before'=previous AND entry->'after'=member)
    THEN RAISE EXCEPTION 'routine occurrence operation lacks exact target member CAS'; END IF;
    SELECT EXISTS(SELECT 1 FROM jsonb_array_elements(change.aggregate_json#>'{manifest,members}') entry WHERE entry->>'parent_id'=NEW.member_item_id::text) INTO is_parent;
    action:=NEW.request_json->'action';
    IF jsonb_typeof(action) IS DISTINCT FROM 'object' OR jsonb_typeof(action->'type') IS DISTINCT FROM 'string'
    THEN RAISE EXCEPTION 'routine occurrence action is invalid'; END IF;
    CASE action->>'type'
    WHEN 'set_outcome' THEN
        IF is_parent OR NOT action ?& ARRAY['type','status'] OR action-ARRAY['type','status']<>'{}'::jsonb
           OR action->>'status' NOT IN ('completed','skipped') OR member->'status' IS DISTINCT FROM action->'status'
           OR member->'open' IS DISTINCT FROM previous->'open'
           OR member->'mode' IS DISTINCT FROM previous->'mode'
           OR member->'required_for_parent' IS DISTINCT FROM previous->'required_for_parent'
        THEN RAISE EXCEPTION 'routine occurrence outcome does not match its target'; END IF;
    WHEN 'reopen' THEN
        IF is_parent OR NOT action ?& ARRAY['type','open'] OR action-ARRAY['type','open']<>'{}'::jsonb
           OR NOT coalesce(valid_item_completion_reopen(action->'open',NEW.member_item_id),false)
           OR member->'open' IS DISTINCT FROM action->'open' OR member->'status' IS DISTINCT FROM action#>'{open,status}'
           OR member->'mode' IS DISTINCT FROM previous->'mode'
           OR member->'required_for_parent' IS DISTINCT FROM previous->'required_for_parent'
        THEN RAISE EXCEPTION 'routine occurrence reopening does not match its target'; END IF;
    WHEN 'set_policy' THEN
        IF NOT coalesce(action ?& ARRAY['type','required_for_parent','mode'] AND action-ARRAY['type','required_for_parent','mode']='{}'::jsonb
            AND jsonb_typeof(action->'required_for_parent')='boolean' AND jsonb_typeof(action->'mode')='string'
            AND action->>'mode' IN ('automatic','keep_open','complete'),false)
           OR member->'required_for_parent' IS DISTINCT FROM action->'required_for_parent' OR member->'mode' IS DISTINCT FROM action->'mode'
           OR member->'open' IS DISTINCT FROM previous->'open' OR (NOT is_parent AND action->>'mode'<>'automatic')
        THEN RAISE EXCEPTION 'routine occurrence policy does not match its target'; END IF;
    ELSE RAISE EXCEPTION 'routine occurrence action is invalid';
    END CASE;
    -- Derived completion can change lifecycle/provenance, never another
    -- member's independently reviewed policy or retained reopening tuple.
    FOR effect IN SELECT * FROM jsonb_array_elements(change.effects_json) LOOP
        IF effect#>>'{after,item_id}'<>NEW.member_item_id::text AND (
            effect#>'{after,open}' IS DISTINCT FROM effect#>'{before,open}'
            OR effect#>'{after,mode}' IS DISTINCT FROM effect#>'{before,mode}'
            OR effect#>'{after,required_for_parent}' IS DISTINCT FROM effect#>'{before,required_for_parent}')
        THEN RAISE EXCEPTION 'derived occurrence effect rewrites independent member policy'; END IF;
        IF evaluations->(effect#>>'{after,item_id}')->'reason' IS DISTINCT FROM effect->'reason'
        THEN RAISE EXCEPTION 'routine occurrence receipt reason differs from its effect'; END IF;
    END LOOP;
    RETURN NEW;
END
$verify$;

CREATE FUNCTION verify_routine_occurrence_publication() RETURNS trigger
LANGUAGE plpgsql AS $verify$
DECLARE manifest routine_occurrences%ROWTYPE;
BEGIN
    SELECT * INTO manifest FROM routine_occurrences WHERE workspace_id=NEW.workspace_id AND id=NEW.instance_id;
    IF NOT FOUND OR NOT EXISTS(SELECT 1 FROM schedule_revisions revision JOIN schedule_revision_details detail
        ON detail.workspace_id=revision.workspace_id AND detail.schedule_revision_id=revision.id
        WHERE revision.workspace_id=NEW.workspace_id AND revision.id=NEW.schedule_revision_id AND revision.state IN ('published','superseded')
          AND EXISTS(SELECT 1 FROM jsonb_array_elements(detail.result_snapshot#>'{compose,plan,occurrences}') occurrence
              WHERE occurrence->>'id'=manifest.occurrence_id::text AND occurrence->>'series_item_id'=manifest.series_item_id::text
                AND occurrence->'identity'=manifest.manifest_json->'identity'
                AND (NEW.schedule_revision_id<>manifest.first_schedule_revision_id OR (
                    (occurrence->>'nominal_start')::timestamptz=(manifest.manifest_json->>'nominal_start')::timestamptz
                    AND (occurrence->>'nominal_end')::timestamptz=(manifest.manifest_json->>'nominal_end')::timestamptz
                    AND (occurrence->>'window_start')::timestamptz=(manifest.manifest_json->>'window_start')::timestamptz
                    AND (occurrence->>'window_end')::timestamptz=(manifest.manifest_json->>'window_end')::timestamptz))))
       OR (SELECT count(*) FROM jsonb_object_keys(NEW.source_revisions))<>manifest.member_count
       OR EXISTS(SELECT 1 FROM routine_occurrence_members member WHERE member.workspace_id=NEW.workspace_id AND member.instance_id=NEW.instance_id
          AND NOT EXISTS(SELECT 1 FROM schedule_revision_details detail WHERE detail.workspace_id=NEW.workspace_id AND detail.schedule_revision_id=NEW.schedule_revision_id
             AND detail.result_snapshot#>'{compose,source_item_revisions}'->member.item_id::text=NEW.source_revisions->member.item_id::text
             AND valid_item_completion_revision(NEW.source_revisions->member.item_id::text,member.source_revision)))
    THEN RAISE EXCEPTION 'routine occurrence lacks exact immutable publication membership'; END IF;
    RETURN NEW;
END
$verify$;

CREATE FUNCTION reject_routine_occurrence_truncate() RETURNS trigger
LANGUAGE plpgsql AS $guard$ BEGIN RAISE EXCEPTION 'routine occurrence evidence requires guarded purge'; END $guard$;

DO $guards$
DECLARE table_name text; function_name text; trusted_schema name:=current_schema();
BEGIN
    FOREACH table_name IN ARRAY ARRAY['routine_occurrences','routine_occurrence_members','routine_occurrence_changes','routine_occurrence_operations','routine_occurrence_publications'] LOOP
        EXECUTE format('CREATE TRIGGER routine_occurrence_immutable BEFORE INSERT OR UPDATE OR DELETE ON %I.%I FOR EACH ROW EXECUTE FUNCTION %I.guard_routine_occurrence_immutable()',trusted_schema,table_name,trusted_schema);
    END LOOP;
    FOREACH table_name IN ARRAY ARRAY['routine_occurrences','routine_occurrence_members','routine_occurrence_state','routine_occurrence_changes','routine_occurrence_operations','routine_occurrence_publications'] LOOP
        EXECUTE format('CREATE TRIGGER account_deletion_fence_guard BEFORE INSERT OR UPDATE OR DELETE ON %I.%I FOR EACH ROW EXECUTE FUNCTION %I.reject_fenced_workspace_mutation()',trusted_schema,table_name,trusted_schema);
        EXECUTE format('CREATE TRIGGER routine_occurrence_no_truncate BEFORE TRUNCATE ON %I.%I FOR EACH STATEMENT EXECUTE FUNCTION %I.reject_routine_occurrence_truncate()',trusted_schema,table_name,trusted_schema);
    END LOOP;
    FOREACH function_name IN ARRAY ARRAY['guard_routine_occurrence_immutable','guard_routine_occurrence_manifest','guard_routine_occurrence_member','verify_routine_occurrence_manifest','guard_routine_occurrence_change','guard_routine_occurrence_state','verify_routine_occurrence_change','verify_routine_occurrence_operation','verify_routine_occurrence_publication','reject_routine_occurrence_truncate'] LOOP
        EXECUTE format('ALTER FUNCTION %I.%I() SET search_path TO %I, pg_catalog, pg_temp',trusted_schema,function_name,trusted_schema);
        EXECUTE format('REVOKE ALL ON FUNCTION %I.%I() FROM PUBLIC',trusted_schema,function_name);
    END LOOP;
    FOREACH function_name IN ARRAY ARRAY['valid_routine_occurrence_timestamp(jsonb)','valid_routine_occurrence_member_state(jsonb,boolean)','valid_routine_occurrence_aggregate(jsonb,jsonb)','valid_routine_occurrence_snapshot(jsonb)'] LOOP
        EXECUTE format('ALTER FUNCTION %I.%s SET search_path TO %I, pg_catalog, pg_temp',trusted_schema,function_name,trusted_schema);
        EXECUTE format('REVOKE ALL ON FUNCTION %I.%s FROM PUBLIC',trusted_schema,function_name);
    END LOOP;
END
$guards$;
CREATE TRIGGER routine_occurrence_manifest_guard BEFORE INSERT ON routine_occurrences FOR EACH ROW EXECUTE FUNCTION guard_routine_occurrence_manifest();
CREATE TRIGGER routine_occurrence_member_guard BEFORE INSERT ON routine_occurrence_members FOR EACH ROW EXECUTE FUNCTION guard_routine_occurrence_member();
CREATE TRIGGER routine_occurrence_change_guard BEFORE INSERT ON routine_occurrence_changes FOR EACH ROW EXECUTE FUNCTION guard_routine_occurrence_change();
CREATE TRIGGER routine_occurrence_state_guard BEFORE INSERT OR UPDATE OR DELETE ON routine_occurrence_state FOR EACH ROW EXECUTE FUNCTION guard_routine_occurrence_state();
CREATE CONSTRAINT TRIGGER routine_occurrence_manifest_complete AFTER INSERT ON routine_occurrences DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_routine_occurrence_manifest();
CREATE CONSTRAINT TRIGGER routine_occurrence_change_complete AFTER INSERT ON routine_occurrence_changes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_routine_occurrence_change();
CREATE CONSTRAINT TRIGGER routine_occurrence_operation_complete AFTER INSERT ON routine_occurrence_operations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_routine_occurrence_operation();
CREATE CONSTRAINT TRIGGER routine_occurrence_publication_complete AFTER INSERT ON routine_occurrence_publications DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION verify_routine_occurrence_publication();

CREATE OR REPLACE FUNCTION lock_item_bootstrap_history_mutation() RETURNS trigger
LANGUAGE plpgsql AS $history$
BEGIN
    PERFORM pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1',0));
    PERFORM pg_advisory_xact_lock(hashtextextended('dayweave.item-bootstrap.history-immutability.v1',0));
    IF TG_OP='TRUNCATE' AND (EXISTS(SELECT 1 FROM item_bootstrap_members) OR EXISTS(SELECT 1 FROM item_completion_effects) OR EXISTS(SELECT 1 FROM routine_occurrence_members))
    THEN RAISE EXCEPTION 'retained evidence still pins canonical history'; END IF;
    RETURN NULL;
END
$history$;
CREATE OR REPLACE FUNCTION reject_item_bootstrap_pinned_change() RETURNS trigger
LANGUAGE plpgsql AS $pinned$
BEGIN
    IF EXISTS(SELECT 1 FROM item_bootstrap_members WHERE change_sequence=OLD.sequence)
       OR EXISTS(SELECT 1 FROM item_completion_effects WHERE workspace_id=OLD.workspace_id AND item_id=OLD.item_id AND (before_item_revision=OLD.item_revision OR after_item_revision=OLD.item_revision))
       OR EXISTS(SELECT 1 FROM routine_occurrence_members WHERE workspace_id=OLD.workspace_id AND item_id=OLD.item_id AND source_revision=OLD.item_revision)
    THEN RAISE EXCEPTION 'retained evidence still pins canonical history'; END IF;
    RETURN CASE WHEN TG_OP='DELETE' THEN OLD ELSE NEW END;
END
$pinned$;

-- CREATE OR REPLACE resets proconfig even when the function's ACL survives.
-- Reinstall both existing history guards' trusted paths explicitly.
DO $history_hardening$
DECLARE trusted_schema name:=current_schema(); function_name text;
BEGIN
    FOREACH function_name IN ARRAY ARRAY['lock_item_bootstrap_history_mutation','reject_item_bootstrap_pinned_change'] LOOP
        EXECUTE format('ALTER FUNCTION %I.%I() SET search_path TO %I, pg_catalog, pg_temp',trusted_schema,function_name,trusted_schema);
        EXECUTE format('REVOKE ALL ON FUNCTION %I.%I() FROM PUBLIC',trusted_schema,function_name);
    END LOOP;
END
$history_hardening$;

-- Extend the exact existing guarded purge inventories without replacing its
-- lifecycle/admission algorithm or loosening any immutable-table guards.
DO $purge_inventory$
DECLARE definition text; updated text;
BEGIN
    SELECT pg_get_functiondef('purge_fenced_personal_account_scope(uuid,bigint,bytea)'::regprocedure) INTO definition;
    updated:=replace(definition,'lock_tables constant text[] := ARRAY[',E'lock_tables constant text[] := ARRAY[\n        ''routine_occurrences'', ''routine_occurrence_members'', ''routine_occurrence_state'',\n        ''routine_occurrence_changes'', ''routine_occurrence_operations'', ''routine_occurrence_publications'',');
    IF updated=definition THEN RAISE EXCEPTION 'known guarded purge lock inventory not found'; END IF;
    definition:=updated;
    updated:=replace(definition,'delete_order constant text[] := ARRAY[',E'delete_order constant text[] := ARRAY[\n        ''routine_occurrence_operations'', ''routine_occurrence_publications'', ''routine_occurrence_state'',\n        ''routine_occurrence_changes'', ''routine_occurrence_members'', ''routine_occurrences'',');
    IF updated=definition THEN RAISE EXCEPTION 'known guarded purge deletion inventory not found'; END IF;
    EXECUTE updated;
END
$purge_inventory$;

-- Assessed Defer keeps every original publication/origin/approval check. Only
-- the paired private schema predicate expands, with a positive v6 ledger head.
DO $defer_schema$
DECLARE definition text; updated text;
BEGIN
    SELECT pg_get_functiondef('guard_execution_defer_assessment()'::regprocedure) INTO definition;
    updated:=replace(definition,E'OR publication_row.snapshot_schema IS DISTINCT FROM ''5''\n       OR publication_row.scheduler_publication_schema\n            IS DISTINCT FROM ''dayweave-scheduler-publication/5''',
        E'OR ((publication_row.snapshot_schema = ''5'' AND publication_row.scheduler_publication_schema = ''dayweave-scheduler-publication/5'') OR (publication_row.snapshot_schema = ''6'' AND publication_row.scheduler_publication_schema = ''dayweave-scheduler-publication/6'' AND EXISTS(SELECT 1 FROM schedule_revision_details occurrence_detail WHERE occurrence_detail.workspace_id=NEW.workspace_id AND occurrence_detail.schedule_revision_id=NEW.current_schedule_revision_id AND valid_item_completion_revision(occurrence_detail.result_snapshot#>''{evidence,occurrence_lifecycle,snapshot_revision}'',1)))) IS NOT TRUE');
    IF updated=definition THEN RAISE EXCEPTION 'known assessed Defer publication schema predicate not found'; END IF;
    EXECUTE updated;
END
$defer_schema$;
