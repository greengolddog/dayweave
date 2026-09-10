"""Pure, bounded synthetic routine-occurrence wire and SQL evidence helpers.

No IO, credentials, clocks or services are consulted. The disposable driver owns
HTTP and SQL execution. UUID spelling and equivalent UTC microsecond timestamps
are normalized for native comparisons; JSON types and closed shapes stay strict.
"""
from __future__ import annotations

import copy
from datetime import datetime, timedelta
import json
import re
import uuid

from native_completion_scenario import (
    BLOCKED_REASON, I64_MAX, PROJECTION_KEYS, ScenarioError, canonical_uuid,
    checked_revision, create_request, fold_delta_pages, projection, require, strict_json,
)

ID_KEYS = {"root", "branch", "required", "optional", "inbox", "blocked"}
COMMAND_KEYS = {"schema_version", "operation_id", "expected_instance_revision",
                "expected_member_revision", "expected_evidence_hash", "action"}
MANIFEST_KEYS = {"schema_version", "id", "series_item_id", "occurrence_id", "identity",
                 "nominal_start", "nominal_end", "window_start", "window_end", "timezone_name",
                 "definition_hash", "members"}
DEFINITION_KEYS = {"item_id", "parent_id", "source_revision", "title", "kind", "recurs",
                   "sibling_order", "required_for_parent", "initial_open"}
MEMBER_KEYS = {"item_id", "revision", "status", "required_for_parent", "mode", "open",
              "provenance", "completed_at", "updated_at"}
SNAPSHOT_KEYS = {"schema_version", "aggregate", "evidence_hash", "fresh_edit_eligible", "members"}
COUNT_KEYS = {"required_descendants", "completed", "incomplete", "occurrence_evidence_required"}
OPEN_KEYS = {"status", "blocked_reason_kind", "blocked_by_item_id", "blocked_reason"}
REASONS = {"unchanged", "outcome_recorded", "reopened", "policy_reviewed", "occurrence_evidence_required",
           "automatically_completed", "automatically_reopened", "manually_completed", "manually_kept_open",
           "manual_completion_released"}
UUID_KEYS = {"id", "item_id", "parent_id", "parent_item_id", "series_item_id", "occurrence_id",
             "instance_id", "operation_id", "member_item_id", "actor_user_id", "actor_session_id",
             "schedule_revision_id", "first_schedule_revision_id", "user_id", "workspace_id",
             "blocked_by_item_id", "active_session_id", "idempotency_key", "evaluation_id", "cause_id"}
TIME_KEYS = {"nominal_start", "nominal_end", "window_start", "window_end", "completed_at", "updated_at",
             "created_at", "deleted_at", "recorded_at", "changed_at", "published_at", "as_of",
             "horizon_start", "horizon_end", "start", "end", "earliest_start_at", "deadline_at"}
TITLE_BY_NAME = {"root": "Synthetic recurring routine", "branch": "Synthetic branch task",
                 "required": "Synthetic required leaf", "optional": "Synthetic optional leaf",
                 "inbox": "Synthetic Inbox leaf", "blocked": "Synthetic blocked leaf"}
SUCCESS_PHASES = {"A": 2, "C": 3, "D": 4, "E": 5, "F": 6}
BASELINE_KEYS = {"items", "item_changes", "completion_states", "completion_operations",
                 "completion_effects", "completion_evaluations", "execution_state", "untouched"}
SQL_KEYS = BASELINE_KEYS | {"manifests", "members", "states", "changes", "operations", "publications",
                            "schedule_revisions", "schedule_requests"}
UNTOUCHED_TABLES = (
    "execution_sessions", "item_progress", "item_progress_operations", "habit_occurrence_evidence",
    "habit_occurrence_outcomes", "habit_occurrence_versions", "habit_pauses", "habit_pause_versions",
    "habit_changes", "habit_operation_receipts", "habit_occurrence_publications", "habit_missed_resolutions",
    "habit_missed_resolution_versions", "provider_accounts", "provider_sync_mappings", "provider_sync_cursors",
)


def _keys(value: object, keys: set[str], message: str) -> dict:
    require(type(value) is dict and set(value) == keys, message)
    return value


def _uuid(value: object) -> str:
    require(type(value) is str and re.fullmatch(
        r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", value) is not None,
        "Invalid typed UUID")
    return canonical_uuid(value.lower())


def _time(value: object) -> str:
    require(type(value) is str and re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?(?:Z|\+00:00)", value) is not None,
        "Invalid UTC microsecond timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ScenarioError("Invalid civil UTC timestamp") from error
    return parsed.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _hash(value: object) -> str:
    require(type(value) is str and re.fullmatch(r"sha256:[0-9a-f]{64}", value) is not None,
            "Invalid evidence hash")
    return value


def _integer(value: object, maximum: int = I64_MAX, minimum: int = 0) -> int:
    require(type(value) is int and minimum <= value <= maximum, "Invalid bounded integer")
    return value


def semantic_json(value: object) -> object:
    """Normalize only typed identity/time positions, with iterative work bounds."""
    visits = 0

    def visit(current, key="", depth=0):
        nonlocal visits
        visits += 1
        require(visits <= 500_000 and depth <= 40, "Semantic JSON bound exceeded")
        if current is None or type(current) is bool:
            return current
        if type(current) is int:
            return _integer(current)
        if type(current) is str:
            require(len(current.encode()) <= 8 * 1024 * 1024, "Oversized string")
            if key in UUID_KEYS:
                return _uuid(current)
            if key in TIME_KEYS:
                return _time(current)
            return current
        if type(current) is list:
            require(len(current) <= 40_000, "Oversized JSON array")
            result = [visit(item, "", depth + 1) for item in current]
            if key == "members" and all(type(item) is dict and "item_id" in item for item in result):
                require(all(type(item["item_id"]) is str for item in result), "Invalid member identity type")
                # SQL captures the same canonical member in multiple immutable
                # instances; wire aggregate/evaluation members belong to one.
                if any("instance_id" in item for item in result):
                    require(all(type(item.get("instance_id")) is str for item in result),
                            "Incomplete SQL member instance identity")
                    identity = lambda item: (item["instance_id"], item["item_id"])
                else:
                    identity = lambda item: item["item_id"]
                require(len({identity(item) for item in result}) == len(result), "Duplicate member identity")
                result.sort(key=identity)
            return result
        require(type(current) is dict and len(current) <= 20_000, "Unsupported JSON type")
        result = {}
        for name, item in current.items():
            require(type(name) is str, "JSON object key must be text")
            normalized = _uuid(name) if key == "source_revisions" else name
            require(normalized not in result, "Duplicate normalized key")
            result[normalized] = visit(item, name, depth + 1)
        return result

    result = visit(value)
    require(len(json.dumps(result, separators=(",", ":"), ensure_ascii=False).encode()) <= 32 * 1024 * 1024,
            "Semantic JSON byte bound exceeded")
    return result


def _typed_equal(actual: object, expected: object) -> bool:
    if type(actual) is not type(expected):
        return False
    if isinstance(actual, dict):
        return actual.keys() == expected.keys() and all(_typed_equal(actual[key], expected[key]) for key in actual)
    if isinstance(actual, list):
        return len(actual) == len(expected) and all(_typed_equal(left, right) for left, right in zip(actual, expected))
    return actual == expected


def same_json(actual: object, expected: object) -> bool:
    """False for malformed/oversized JSON or type drift; never accepts bool as int."""
    try:
        return _typed_equal(semantic_json(actual), semantic_json(expected))
    except ScenarioError:
        return False


def validate_ids(ids: dict[str, str]) -> None:
    _keys(ids, ID_KEYS, "Unexpected scenario identity set")
    require(len({canonical_uuid(value) for value in ids.values()}) == 6, "Duplicate scenario identity")


def seed_requests(ids: dict[str, str]) -> list[dict]:
    validate_ids(ids)
    requests = []
    for name in ("root", "branch", "required", "optional", "inbox", "blocked"):
        parent = None if name == "root" else ids["branch" if name == "required" else "root"]
        request = create_request(ids[name], kind="task", title=TITLE_BY_NAME[name], parent_id=parent,
                                 blocked=name == "blocked")
        if name in {"root", "branch"}:
            request.update(duration_kind="unknown", duration_seconds=None, duration_source=None, has_own_effort=False)
        if name == "root":
            request.update(kind="routine", recurrence={"type": "daily", "times_per_day": 1})
        if name == "inbox":
            request["status"] = "inbox"
        requests.append(request)
    return requests


def optional_policy_request(snapshot: dict, operation_id: str) -> dict:
    """Make optional/inbox/blocked canonical edges optional before publication."""
    _keys(snapshot, {"schema_version", "item_id", "item_revision", "state", "evidence_hash", "counts",
                     "occurrence_evidence_required"}, "Incomplete canonical completion snapshot")
    require(type(snapshot["schema_version"]) is int and snapshot["schema_version"] == 1,
            "Unsupported completion snapshot")
    _uuid(snapshot["item_id"]); checked_revision(snapshot["item_revision"]); _hash(snapshot["evidence_hash"])
    state = snapshot["state"]
    require(type(state) is dict and _uuid(state.get("item_id")) == _uuid(snapshot["item_id"]), "Wrong completion target")
    _integer(state.get("revision"))
    return {"schema_version": 1, "operation_id": canonical_uuid(operation_id),
            "expected_item_revision": snapshot["item_revision"], "expected_completion_revision": state["revision"],
            "expected_evidence_hash": snapshot["evidence_hash"], "required_for_parent": False,
            "mode": "automatic", "reopening": None}


def preview_request(as_of: str) -> dict:
    """Two full UTC daily buckets; identities must come from the returned plan."""
    instant = datetime.fromisoformat(_time(as_of).replace("Z", "+00:00"))
    require(instant.microsecond == 0, "Scenario as-of must be a whole second")
    start = instant.replace(hour=0, minute=0, second=0) + timedelta(days=1)
    end = start + timedelta(days=2)
    stamp = lambda value: value.isoformat(timespec="seconds").replace("+00:00", "Z")
    return {"as_of": stamp(instant), "horizon_start": stamp(start), "horizon_end": stamp(end), "timezone_name": "UTC",
            "availability": [{"start": stamp(start), "end": stamp(end), "contexts": [], "location": None, "energy": "deep"}],
            "fixed_blocks": [], "previous_assignments": [], "recurrence_context": {}}


def publication_request(preview: dict, schedule: dict, operation_id: str) -> dict:
    require(type(preview) is dict, "Missing schedule preview")
    _hash(preview.get("input_digest"))
    expected_keys = set(preview_request("2026-01-01T00:00:00Z"))
    _keys(schedule, expected_keys, "Unexpected schedule request fields")
    semantic_json(schedule)
    return {"idempotency_key": canonical_uuid(operation_id), "expected_input_digest": preview["input_digest"],
            "schedule": copy.deepcopy(schedule)}


# Driver-facing spellings. All helpers remain pure; the driver owns transport.
optional_completion_command = optional_policy_request
semantic_equal = same_json


def publish_request(schedule: dict, preview: dict, operation_id: str) -> dict:
    return publication_request(preview, schedule, operation_id)


def _open(name: str) -> dict:
    return {"status": "blocked" if name == "blocked" else "inbox" if name == "inbox" else "planned",
            "blocked_reason_kind": "manual" if name == "blocked" else None, "blocked_by_item_id": None,
            "blocked_reason": BLOCKED_REASON if name == "blocked" else None}


def _rows(rows: object, key: str, keys: set[str] | None = None) -> dict[str, dict]:
    require(type(rows) is list and len(rows) <= 40_000, "Invalid evidence row array")
    result = {}
    for row in rows:
        require(type(row) is dict and key in row, "Incomplete evidence row")
        if keys is not None:
            _keys(row, keys, "Unexpected evidence row fields")
        identity = _uuid(row[key])
        require(identity not in result, "Duplicate evidence identity")
        result[identity] = row
    return result


def _aggregate(value: dict) -> dict:
    if type(value) is dict and set(value) == {"operation_id", "replayed", "occurrence"}:
        _uuid(value["operation_id"])
        require(type(value["replayed"]) is bool, "Invalid receipt replay flag")
        value = value["occurrence"]
    if type(value) is dict and set(value) == SNAPSHOT_KEYS:
        return value["aggregate"]
    return value


def assert_aggregate(aggregate: dict, ids: dict[str, str], revision: int = 1,
                     initial: dict | None = None, *, instance_id: str | None = None,
                     occurrence_id: str | None = None, initial_manifest: dict | None = None) -> dict:
    """Validate the entire six-member scenario state at aggregate revisions 1..6."""
    validate_ids(ids); _integer(revision, 6, 1)
    value = semantic_json(aggregate)
    _keys(value, {"manifest", "revision", "members"}, "Incomplete occurrence aggregate")
    require(type(value["revision"]) is int and value["revision"] == revision, "Wrong occurrence revision")
    manifest = _keys(value["manifest"], MANIFEST_KEYS, "Incomplete occurrence manifest")
    require(type(manifest["schema_version"]) is int and manifest["schema_version"] == 1
            and manifest["series_item_id"] == ids["root"] and manifest["id"] != manifest["occurrence_id"]
            and manifest["timezone_name"] == "UTC", "Wrong manifest identity or schema")
    planner = uuid.UUID(_uuid(manifest["occurrence_id"]))
    require(planner.version == 5 and planner.variant == uuid.RFC_4122, "Planner identity must be RFC4122 UUID-v5")
    _uuid(manifest["id"]); _hash(manifest["definition_hash"])
    if instance_id is not None:
        require(manifest["id"] == canonical_uuid(instance_id), "Wrong exact ledger instance")
    if occurrence_id is not None:
        require(manifest["occurrence_id"] == canonical_uuid(occurrence_id), "Wrong exact planner occurrence")
    if initial_manifest is not None:
        require(same_json(manifest, initial_manifest), "Immutable occurrence manifest changed")
    identity = _keys(manifest["identity"], {"type", "date", "bucket_ordinal"}, "Unexpected scenario recurrence identity")
    require(identity["type"] == "calendar_day" and type(identity["bucket_ordinal"]) is int
            and identity["bucket_ordinal"] == 0 and type(identity["date"]) is str,
            "Wrong daily occurrence identity")
    start, end, window_start, window_end = (_time(manifest[key]) for key in
        ("nominal_start", "nominal_end", "window_start", "window_end"))
    require(start < end and window_start < window_end and identity["date"] == start[:10], "Invalid occurrence window")
    require(datetime.fromisoformat(end.replace("Z", "+00:00")) - datetime.fromisoformat(start.replace("Z", "+00:00"))
            == timedelta(days=1), "Scenario daily nominal window differs")
    definitions = _rows(manifest["members"], "item_id", DEFINITION_KEYS)
    members = _rows(value["members"], "item_id", MEMBER_KEYS)
    require(set(definitions) == set(ids.values()) == set(members), "Manifest or aggregate omits a scenario member")
    for name, item_id in ids.items():
        definition, member = definitions[item_id], members[item_id]
        parent = None if name == "root" else ids["branch" if name == "required" else "root"]
        require(definition["parent_id"] == parent and definition["kind"] == ("routine" if name == "root" else "task")
                and definition["title"] == TITLE_BY_NAME[name], "Changed manifest topology or title")
        checked_revision(definition["source_revision"]); _integer(definition["sibling_order"], 1_000_000)
        require(type(definition["recurs"]) is bool and definition["recurs"] == (name == "root"), "Wrong recurrence qualification")
        required = name in {"root", "branch", "required"}
        require(type(definition["required_for_parent"]) is bool and definition["required_for_parent"] == required,
                "Incorrect captured optional branch")
        require(same_json(definition["initial_open"], _open(name)) and same_json(member["open"], _open(name)),
                "Exact retained opening state changed")
        require(type(member["required_for_parent"]) is bool and member["required_for_parent"] == required,
                "Occurrence policy changed a required edge")
        status = _open(name)["status"]
        member_revision, mode, provenance = 1, "automatic", None
        if revision >= 2 and name in {"root", "branch", "required"}:
            status, member_revision = "completed", 2
            if name != "required":
                provenance = {"kind": "automatic", "reopen": _open(name)}
        if name == "root" and revision == 3:
            status, member_revision, mode, provenance = "planned", 3, "keep_open", None
        if name == "root" and revision >= 4:
            member_revision = 4
        if name == "blocked" and revision >= 5:
            status, member_revision = ("skipped", 2) if revision == 5 else ("blocked", 3)
        require(type(member["revision"]) is int and member["revision"] == member_revision
                and member["status"] == status and member["mode"] == mode
                and same_json(member["provenance"], provenance), "Occurrence member lifecycle or revision differs")
        _time(member["updated_at"])
        require((member["completed_at"] is not None) == (status == "completed"), "Invalid completion timestamp custody")
        if member["completed_at"] is not None:
            require(_time(member["completed_at"]) <= _time(member["updated_at"]), "Completion time exceeds member update")
    if initial is not None:
        original = assert_aggregate(_aggregate(initial), ids, revision=1)
        require(same_json(manifest, original["manifest"]), "Immutable occurrence manifest changed")
        originals = _rows(original["members"], "item_id")
        for name in ("optional", "inbox"):
            require(same_json(members[ids[name]], originals[ids[name]]), "Untouched optional member changed")
        for item_id, member in members.items():
            require(_time(member["updated_at"]) >= _time(originals[item_id]["updated_at"]), "Member update time regressed")
    return value


def assert_snapshot(snapshot: dict, ids: dict[str, str], revision: int = 1,
                    initial: dict | None = None, *, instance_id: str | None = None,
                    occurrence_id: str | None = None, initial_manifest: dict | None = None) -> dict:
    value = semantic_json(snapshot)
    _keys(value, SNAPSHOT_KEYS, "Incomplete private occurrence snapshot")
    require(type(value["schema_version"]) is int and value["schema_version"] == 1
            and type(value["fresh_edit_eligible"]) is bool, "Invalid private snapshot controls")
    _hash(value["evidence_hash"])
    assert_aggregate(value["aggregate"], ids, revision=revision, initial=initial,
                     instance_id=instance_id, occurrence_id=occurrence_id, initial_manifest=initial_manifest)
    evaluations = _rows(value["members"], "item_id", {"item_id", "counts", "occurrence_evidence_required", "reason"})
    require(set(evaluations) == set(ids.values()), "Incomplete occurrence evaluation members")
    for name, item_id in ids.items():
        row = evaluations[item_id]
        require(row["occurrence_evidence_required"] is False and type(row["reason"]) is str
                and row["reason"] in REASONS, "Unexpected occurrence evaluation")
        count = 2 if name == "root" else 1 if name == "branch" else 0
        expected = {"required_descendants": count, "completed": count if revision >= 2 else 0,
                    "incomplete": 0 if revision >= 2 else count, "occurrence_evidence_required": 0}
        _keys(row["counts"], COUNT_KEYS, "Incomplete required descendant counts")
        require(same_json(row["counts"], expected), "Occurrence required descendant counts differ")
    return value


def assert_sentinel(actual: dict, initial: dict, ids: dict[str, str]) -> None:
    assert_aggregate(actual, ids, revision=1, initial=initial)
    require(same_json(actual, _aggregate(initial)), "A separate planner occurrence changed")


def _command(command: dict, ids: dict[str, str], label: str) -> dict:
    value = semantic_json(command)
    _keys(value, COMMAND_KEYS, "Incomplete exact occurrence command")
    require(type(value["schema_version"]) is int and value["schema_version"] == 1, "Invalid command schema")
    _uuid(value["operation_id"]); _hash(value["expected_evidence_hash"])
    expected_revision = {"A": 1, "B": 1, "C": 2, "D": 3, "E": 4, "F": 5}[label]
    expected_member = {"A": 1, "B": 1, "C": 2, "D": 3, "E": 1, "F": 2}[label]
    require(type(value["expected_instance_revision"]) is int and value["expected_instance_revision"] == expected_revision
            and type(value["expected_member_revision"]) is int and value["expected_member_revision"] == expected_member,
            "Saved occurrence command was rebased")
    action = ({"type": "set_outcome", "status": "completed"} if label == "A" else
              {"type": "set_outcome", "status": "skipped"} if label in {"B", "E"} else
              {"type": "reopen", "open": _open("blocked")} if label == "F" else
              {"type": "set_policy", "required_for_parent": True, "mode": "keep_open" if label == "C" else "automatic"})
    require(same_json(value["action"], action), "Saved occurrence action differs")
    return value


def final_sql(workspace_id: str) -> str:
    """Read-only scoped evidence; never interpolate unvalidated text or execute SQL."""
    scope = "workspace_id='" + canonical_uuid(workspace_id) + "'::uuid"
    def rows(table, body, order):
        return f"(SELECT COALESCE(jsonb_agg({body} ORDER BY {order}),'[]'::jsonb) FROM {table} WHERE {scope})"
    def obj(**fields):
        return "jsonb_build_object(" + ",".join(f"'{name}',{value}" for name, value in fields.items()) + ")"
    item = obj(**{key: "hierarchy.parent_item_id" if key == "parent_id" else f"item.{key}" for key in PROJECTION_KEYS})
    items = (f"(SELECT COALESCE(jsonb_agg({item} ORDER BY item.id),'[]'::jsonb) FROM items AS item "
             "LEFT JOIN item_hierarchy AS hierarchy ON hierarchy.workspace_id=item.workspace_id "
             f"AND hierarchy.child_item_id=item.id WHERE item.{scope})")
    fields = {
        "items": items,
        "item_changes": rows("item_changes", obj(sequence="sequence", item_id="item_id", revision="item_revision",
                            kind="change_kind", payload="payload"), "sequence"),
        "completion_states": rows("item_completion_state", "state_json", "item_id"),
        "completion_operations": rows("item_completion_operations", "to_jsonb(item_completion_operations)", "operation_id"),
        "completion_effects": rows("item_completion_effects", "to_jsonb(item_completion_effects)", "item_id,completion_revision"),
        "completion_evaluations": rows("item_completion_evaluations", "to_jsonb(item_completion_evaluations)", "evaluation_id"),
        "execution_state": rows("execution_state", obj(revision="revision", active_session_id="active_session_id"), "workspace_id"),
        "untouched": "jsonb_build_object(" + ",".join(f"'{table}',(SELECT count(*) FROM {table} WHERE {scope})"
                                                       for table in UNTOUCHED_TABLES) + ")",
        "manifests": rows("routine_occurrences", obj(id="id", series_item_id="series_item_id", occurrence_id="occurrence_id",
                          definition_hash="definition_hash", manifest="manifest_json", member_count="member_count",
                          first_schedule_revision_id="first_schedule_revision_id"), "id"),
        "members": rows("routine_occurrence_members", obj(instance_id="instance_id", item_id="item_id",
                        parent_item_id="parent_item_id", source_revision="source_revision"), "instance_id,item_id"),
        "states": rows("routine_occurrence_state", obj(instance_id="instance_id", revision="revision", aggregate="aggregate_json"), "instance_id"),
        "changes": rows("routine_occurrence_changes", obj(sequence="sequence", instance_id="instance_id", revision="revision",
                        operation_id="operation_id", before="before_json", aggregate="aggregate_json", effects="effects_json",
                        changed_at="changed_at"), "sequence"),
        "operations": rows("routine_occurrence_operations", obj(operation_id="operation_id", instance_id="instance_id",
                           member_item_id="member_item_id", actor_user_id="actor_user_id", actor_session_id="actor_session_id",
                           change_sequence="change_sequence", request="request_json", result="result_json", recorded_at="recorded_at"), "operation_id"),
        "publications": rows("routine_occurrence_publications", obj(instance_id="instance_id", schedule_revision_id="schedule_revision_id",
                             source_revisions="source_revisions"), "instance_id,schedule_revision_id"),
        "schedule_revisions": ("(SELECT COALESCE(jsonb_agg(" + obj(id="revision.id", revision_number="revision.revision_number",
            input_digest="'sha256:'||encode(revision.input_digest,'hex')", schema="detail.result_snapshot->>'schema_version'",
            publication_schema="detail.result_snapshot->>'scheduler_publication_schema'",
            occurrence_head="detail.result_snapshot#>'{evidence,occurrence_lifecycle,snapshot_revision}'")
            + " ORDER BY revision.revision_number),'[]'::jsonb) FROM schedule_revisions revision JOIN schedule_revision_details detail "
            "ON detail.workspace_id=revision.workspace_id AND detail.schedule_revision_id=revision.id "
            f"WHERE revision.{scope})"),
        "schedule_requests": rows("schedule_publication_requests", obj(idempotency_key="idempotency_key",
                                schedule_revision_id="schedule_revision_id", request_hash="encode(request_hash,'hex')"), "idempotency_key"),
    }
    return "SELECT jsonb_build_object(" + ",".join(f"'{key}',{value}" for key, value in fields.items()) + ")"


def assert_final_sql(evidence: dict, ids: dict[str, str], baseline: dict,
                     commands: dict[str, dict], original_a: dict, *, instance_id: str, sentinel_id: str) -> None:
    """Audit all immutable evidence independently from native PASS markers.

    SQL request_json/result_json are typed semantic custody; native wrappers must
    independently check original request bytes and HTTP replay headers. Result
    rows contain the occurrence snapshot, not the HTTP replay wrapper.
    """
    validate_ids(ids)
    final = semantic_json(_keys(evidence, SQL_KEYS, "Incomplete final SQL evidence"))
    before = semantic_json(_keys(baseline, SQL_KEYS, "Incomplete initial SQL evidence"))
    baseline_states = _rows(before["states"], "instance_id", {"instance_id", "revision", "aggregate"})
    require(canonical_uuid(instance_id) in baseline_states and canonical_uuid(sentinel_id) in baseline_states,
            "Exact initial occurrence or sentinel is missing")
    initial = assert_aggregate(baseline_states[instance_id]["aggregate"], ids, revision=1, instance_id=instance_id)
    sentinel = assert_aggregate(baseline_states[sentinel_id]["aggregate"], ids, revision=1, instance_id=sentinel_id)
    first_id, sentinel_id = initial["manifest"]["id"], sentinel["manifest"]["id"]
    require(first_id != sentinel_id and initial["manifest"]["occurrence_id"] != sentinel["manifest"]["occurrence_id"],
            "Occurrence and sentinel identities overlap")
    for key in BASELINE_KEYS:
        require(same_json(final[key], before[key]), "Occurrence operation changed canonical or unrelated baseline")
    require({row["id"] for row in projection(final["items"])} == set(ids.values()), "Canonical baseline target set differs")
    _keys(final["untouched"], set(UNTOUCHED_TABLES), "Incomplete unrelated-table audit")
    for count in final["untouched"].values():
        require(type(count) is int and count == 0, "Unexpected unrelated service effect")
    manifests = _rows(final["manifests"], "id", {"id", "series_item_id", "occurrence_id", "definition_hash", "manifest",
                                                     "member_count", "first_schedule_revision_id"})
    old_manifests = _rows(before["manifests"], "id")
    require(first_id in old_manifests and sentinel_id in old_manifests and set(old_manifests) <= set(manifests),
            "Missing original admitted occurrences")
    for instance_id, row in old_manifests.items():
        require(same_json(row, manifests[instance_id]), "Immutable admitted manifest changed")
    states = _rows(final["states"], "instance_id", {"instance_id", "revision", "aggregate"})
    old_states = _rows(before["states"], "instance_id")
    require(set(states) == set(manifests), "Incomplete current occurrence states")
    require(same_json(old_states[first_id]["aggregate"], initial)
            and same_json(old_states[sentinel_id]["aggregate"], sentinel), "Initial SQL does not match selected occurrences")
    pairs, source_history = set(), {}
    for row in final["item_changes"]:
        _keys(row, {"sequence", "item_id", "revision", "kind", "payload"}, "Invalid item history row")
        checked_revision(row["sequence"]); checked_revision(row["revision"])
        key = (row["item_id"], row["revision"])
        require(key not in source_history and row["kind"] == "upsert", "Invalid immutable source history")
        source_history[key] = row["payload"]
    member_rows = {}
    for row in final["members"]:
        _keys(row, {"instance_id", "item_id", "parent_item_id", "source_revision"}, "Incomplete manifest member source")
        checked_revision(row["source_revision"])
        key = (row["instance_id"], row["item_id"])
        require(key not in member_rows, "Duplicate captured member source")
        member_rows[key] = row
    for instance_id, row in manifests.items():
        require(row["series_item_id"] == ids["root"] and row["id"] == row["manifest"]["id"]
                and row["occurrence_id"] == row["manifest"]["occurrence_id"]
                and row["definition_hash"] == row["manifest"]["definition_hash"]
                and type(row["member_count"]) is int and row["member_count"] == 6, "Manifest SQL envelope differs")
        pair = (row["series_item_id"], row["occurrence_id"])
        require(pair not in pairs, "Duplicate admitted calendar identity"); pairs.add(pair)
        revision = 6 if instance_id == first_id else 1
        state = states[instance_id]
        require(type(state["revision"]) is int and state["revision"] == revision, "Wrong SQL occurrence head")
        aggregate = assert_aggregate(state["aggregate"], ids, revision=revision, initial=initial if instance_id == first_id else None)
        require(same_json(aggregate["manifest"], row["manifest"]), "SQL state names a different manifest")
        if instance_id != first_id and instance_id in old_states:
            require(same_json(state, old_states[instance_id]), "Untouched admitted occurrence changed")
        for definition in row["manifest"]["members"]:
            captured = member_rows.get((instance_id, definition["item_id"]))
            require(captured is not None and captured["parent_item_id"] == definition["parent_id"]
                    and captured["source_revision"] == definition["source_revision"], "Captured canonical member differs")
            source = source_history.get((definition["item_id"], definition["source_revision"]))
            require(type(source) is dict and source.get("title") == definition["title"]
                    and source.get("kind") == definition["kind"] and source.get("parent_id") == definition["parent_id"],
                    "Manifest source lacks exact immutable canonical history")
    require(len(member_rows) == len(manifests) * 6, "Extraneous captured member")
    assert_sentinel(states[sentinel_id]["aggregate"], sentinel, ids)
    _keys(commands, set("ABCDEF"), "Incomplete native command custody")
    saved = {label: _command(command, ids, label) for label, command in commands.items()}
    require(len({command["operation_id"] for command in saved.values()}) == 6, "Operation identities were reused")
    operations = _rows(final["operations"], "operation_id", {"operation_id", "instance_id", "member_item_id", "actor_user_id",
                                                             "actor_session_id", "change_sequence", "request", "result", "recorded_at"})
    require(set(operations) == {saved[label]["operation_id"] for label in SUCCESS_PHASES}, "Missing receipt or stale B acquired receipt")
    require(before["operations"] == [], "Initial publication unexpectedly has member operations")
    changes_by_instance, sequence_rows, previous_sequence = {}, {}, 0
    for change in final["changes"]:
        _keys(change, {"sequence", "instance_id", "revision", "operation_id", "before", "aggregate", "effects", "changed_at"},
              "Incomplete occurrence change")
        sequence = checked_revision(change["sequence"])
        checked_revision(change["revision"])
        _time(change["changed_at"])
        require(sequence > previous_sequence and change["instance_id"] in manifests, "Invalid global change ordering")
        previous_sequence = sequence; sequence_rows[sequence] = change
        changes_by_instance.setdefault(change["instance_id"], []).append(change)
    for instance_id, state in states.items():
        chain = changes_by_instance.get(instance_id, [])
        require([row["revision"] for row in chain] == list(range(1, state["revision"] + 1)), "Incomplete immutable instance revision chain")
        for index, change in enumerate(chain):
            assert_aggregate(change["aggregate"], ids, revision=index + 1,
                             initial=initial if instance_id == first_id else None)
            require(same_json(change["aggregate"]["manifest"], manifests[instance_id]["manifest"]), "Change manifest drift")
            if index == 0:
                require(change["before"] is None and change["operation_id"] is None and change["effects"] == [],
                        "Initial publication contains a command effect")
            else:
                require(same_json(change["before"], chain[index - 1]["aggregate"]), "Broken immutable before/after chain")
                old_members = _rows(change["before"]["members"], "item_id")
                new_members = _rows(change["aggregate"]["members"], "item_id")
                effects = change["effects"]
                require(type(effects) is list, "Invalid member effects")
                changed_ids = {item_id for item_id in old_members if not same_json(old_members[item_id], new_members[item_id])}
                found = set()
                for effect in effects:
                    _keys(effect, {"before", "after", "reason"}, "Invalid member effect")
                    _keys(effect["after"], MEMBER_KEYS, "Incomplete effect member")
                    member_id = effect["after"]["item_id"]
                    require(member_id not in found and member_id in changed_ids and type(effect["reason"]) is str
                            and effect["reason"] in REASONS
                            and same_json(effect["before"], old_members[member_id])
                            and same_json(effect["after"], new_members[member_id]), "Incorrect member effect evidence")
                    require(new_members[member_id]["revision"] == old_members[member_id]["revision"] + 1,
                            "Member effect skipped a revision")
                    require(_time(new_members[member_id]["updated_at"]) == _time(change["changed_at"]), "Effect timestamp mismatch")
                    found.add(member_id)
                require(found == changed_ids, "Missing member effect")
        require(same_json(chain[-1]["aggregate"], state["aggregate"]), "Current state is not immutable history head")
    old_changes = {row["sequence"]: row for row in before["changes"]}
    require(all(sequence in sequence_rows and same_json(row, sequence_rows[sequence]) for sequence, row in old_changes.items()),
            "Initial immutable changes were rewritten")
    for label, revision in SUCCESS_PHASES.items():
        command = saved[label]; operation = operations[command["operation_id"]]
        target = ids["required" if label == "A" else "root" if label in {"C", "D"} else "blocked"]
        require(operation["instance_id"] == first_id and operation["member_item_id"] == target
                and same_json(operation["request"], command), "Receipt target or saved request differs")
        require(operation["actor_session_id"] is not None, "Receipt lacks Device credential attribution")
        _uuid(operation["actor_user_id"]); _uuid(operation["actor_session_id"])
        checked_revision(operation["change_sequence"])
        change = sequence_rows.get(operation["change_sequence"])
        require(change is not None and change["operation_id"] == command["operation_id"]
                and change["instance_id"] == first_id and change["revision"] == revision, "Receipt does not own its change")
        require(_time(operation["recorded_at"]) == _time(change["changed_at"]), "Receipt recording time differs")
        result = assert_snapshot(operation["result"], ids, revision=revision, initial=initial)
        require(result["fresh_edit_eligible"] is True and same_json(result["aggregate"], change["aggregate"]),
                "Receipt result differs from committed aggregate")
        before_member = _rows(change["before"]["members"], "item_id")[target]
        require(command["expected_member_revision"] == before_member["revision"], "Receipt lost target CAS")
        if label == "A":
            require(same_json(result["aggregate"], _aggregate(original_a)), "Historical A receipt rolled forward or changed")
    schedules = _rows(final["schedule_revisions"], "id", {"id", "revision_number", "input_digest", "schema", "publication_schema", "occurrence_head"})
    for key, row in _rows(before["schedule_revisions"], "id").items():
        require(same_json(schedules.get(key), row), "Original immutable publication changed")
    latest_change = previous_sequence
    require(any(row["schema"] == "6" and row["publication_schema"] == "dayweave-scheduler-publication/6"
                and type(row["occurrence_head"]) is int and row["occurrence_head"] >= latest_change for row in schedules.values()),
            "No freshly published schedule includes the terminal occurrence head")
    for row in schedules.values():
        checked_revision(row["revision_number"]); _hash(row["input_digest"])
    publications = {}
    for row in final["publications"]:
        _keys(row, {"instance_id", "schedule_revision_id", "source_revisions"}, "Incomplete occurrence publication link")
        key = (row["instance_id"], row["schedule_revision_id"])
        require(key not in publications and row["instance_id"] in manifests and row["schedule_revision_id"] in schedules,
                "Invalid occurrence publication link")
        revisions = row["source_revisions"]
        require(type(revisions) is dict and set(revisions) == set(ids.values()), "Incomplete publication source revisions")
        for item_id, revision in revisions.items():
            checked_revision(revision)
            require((item_id, revision) in source_history, "Publication source revision has no canonical history")
        publications[key] = row
    for instance_id, row in manifests.items():
        require((instance_id, row["first_schedule_revision_id"]) in publications, "First admission publication witness is missing")
    for row in before["publications"]:
        require(same_json(publications.get((row["instance_id"], row["schedule_revision_id"])), row), "Immutable publication witness changed")
    requests = _rows(final["schedule_requests"], "idempotency_key", {"idempotency_key", "schedule_revision_id", "request_hash"})
    require(requests and all(row["schedule_revision_id"] in schedules and type(row["request_hash"]) is str
                            and re.fullmatch(r"[0-9a-f]{64}", row["request_hash"]) for row in requests.values()),
            "Invalid exact publication receipt")
    for key, row in _rows(before["schedule_requests"], "idempotency_key").items():
        require(same_json(requests.get(key), row), "Original exact publication receipt changed")
