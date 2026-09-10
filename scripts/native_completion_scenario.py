"""Pure synthetic completion-scenario wire and evidence helpers; no IO or services.

Delta has no exposed sequence number: order checks prove per-identity monotonic
revisions and terminal pagination, not a fabricated globally decoded cursor.
"""
from __future__ import annotations

import copy
from dataclasses import dataclass
from datetime import datetime
import json
import re
import uuid

I64_MAX = (1 << 63) - 1
BLOCKED_REASON = "Synthetic waiting for input"
ID_KEYS = {"root", "branch", "required", "optional", "new_child"}
PROJECTION_KEYS = ("id", "revision", "status", "parent_id", "blocked_reason_kind", "blocked_by_item_id", "blocked_reason")
ITEM_FIELDS = (
    "is_sensitive", "kind", "status", "title", "notes", "timezone_name",
    "duration_kind", "duration_seconds", "duration_min_seconds", "duration_max_seconds", "duration_source",
    "deadline_kind", "deadline_date", "deadline_at", "deadline_strength", "deadline_soft_weight",
    "earliest_start_at", "recurrence", "flexible_constraints", "has_own_effort", "split_policy",
    "importance", "urgency", "parent_id", "sibling_order", "blocked_reason_kind", "blocked_by_item_id", "blocked_reason",
)
ITEM_KEYS = set(ITEM_FIELDS) | {"id", "revision", "is_executable", "created_at", "updated_at", "completed_at", "deleted_at"}
STATUSES = {"inbox", "planned", "scheduled", "in_progress", "paused", "completed", "skipped", "cancelled", "blocked"}


class ScenarioError(ValueError):
    """Content-free validation failure: never embeds payloads or configuration."""


def require(condition: bool, diagnostic: str) -> None:
    if not condition:
        raise ScenarioError(diagnostic)


def canonical_uuid(value: object) -> str:
    require(isinstance(value, str), "Expected synthetic UUID")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as error:
        raise ScenarioError("Invalid synthetic UUID") from error
    require(parsed.int != 0 and str(parsed) == value, "UUID must be nonzero and canonical")
    return value


def checked_revision(value: object) -> int:
    require(type(value) is int and 0 < value <= I64_MAX, "Invalid canonical revision")
    return value


def strict_json(data: str | bytes) -> object:
    require(isinstance(data, (str, bytes)) and len(data.encode() if isinstance(data, str) else data) <= 32 * 1024 * 1024,
            "JSON input exceeds scenario bound")
    def pairs(values):
        result = {}
        for key, value in values:
            require(key not in result, "Duplicate JSON key")
            result[key] = value
        return result

    def nonfinite(_):
        raise ScenarioError("Nonfinite JSON number")

    try:
        return json.loads(data, object_pairs_hook=pairs, parse_constant=nonfinite)
    except (ValueError, UnicodeError) as error:
        raise ScenarioError("Invalid or ambiguous JSON") from error


def validate_ids(ids: dict[str, str]) -> None:
    require(type(ids) is dict and set(ids) == ID_KEYS, "Unexpected scenario identity set")
    require(len({canonical_uuid(value) for value in ids.values()}) == len(ID_KEYS), "Duplicate scenario identity")


def create_request(item_id: str, *, kind: str, title: str, parent_id: str | None = None,
                   blocked: bool = False) -> dict:
    canonical_uuid(item_id)
    if parent_id is not None:
        canonical_uuid(parent_id)
        require(parent_id != item_id, "Self-parent request")
    require(kind in {"goal", "project", "task"}, "Unsupported scenario kind")
    require(isinstance(title, str) and title.startswith("Synthetic ") and len(title) <= 100,
            "Scenario title must be synthetic")
    return {
        "id": item_id, "is_sensitive": False, "kind": kind, "status": "blocked" if blocked else "planned",
        "title": title, "notes": None, "timezone_name": "UTC",
        "duration_kind": "exact" if kind == "task" else "unknown",
        "duration_seconds": 60 if kind == "task" else None, "duration_min_seconds": None,
        "duration_max_seconds": None, "duration_source": "user" if kind == "task" else None,
        "deadline_kind": "none", "deadline_date": None, "deadline_at": None, "deadline_strength": None,
        "deadline_soft_weight": None, "earliest_start_at": None, "recurrence": None,
        "flexible_constraints": {}, "has_own_effort": False, "split_policy": {"type": "indivisible"},
        "importance": 50, "urgency": 50, "parent_id": parent_id, "sibling_order": 0,
        "blocked_reason_kind": "manual" if blocked else None, "blocked_by_item_id": None,
        "blocked_reason": BLOCKED_REASON if blocked else None,
    }


def seed_requests(ids: dict[str, str]) -> list[dict]:
    validate_ids(ids)
    return [
        create_request(ids["root"], kind="goal", title="Synthetic root goal", blocked=True),
        create_request(ids["branch"], kind="project", title="Synthetic branch project", parent_id=ids["root"]),
        create_request(ids["required"], kind="task", title="Synthetic required leaf", parent_id=ids["branch"]),
        create_request(ids["optional"], kind="task", title="Synthetic optional leaf", parent_id=ids["root"]),
    ]


def new_child_request(ids: dict[str, str]) -> dict:
    validate_ids(ids)
    return create_request(ids["new_child"], kind="task", title="Synthetic newly required child", parent_id=ids["branch"])


def project_item(item: dict) -> dict:
    require(type(item) is dict and set(PROJECTION_KEYS) <= item.keys(), "Incomplete item projection")
    canonical_uuid(item["id"])
    checked_revision(item["revision"])
    require(isinstance(item["status"], str) and item["status"] in STATUSES, "Unknown item status")
    if item["parent_id"] is not None:
        canonical_uuid(item["parent_id"])
        require(item["parent_id"] != item["id"], "Self-parent projection")
    kind, blocker, reason = (item[key] for key in PROJECTION_KEYS[4:])
    if item["status"] != "blocked":
        require((kind, blocker, reason) == (None, None, None), "Nonblocked item retains blocker tuple")
    elif kind == "dependency":
        canonical_uuid(blocker)
        require(blocker != item["id"], "Self-blocked projection")
        require(reason is None or _valid_reason(reason), "Invalid dependency reason")
    else:
        require(kind in {"manual", "external"} and blocker is None and _valid_reason(reason), "Invalid blocker tuple")
    return {key: copy.deepcopy(item[key]) for key in PROJECTION_KEYS}


def _valid_reason(value: object) -> bool:
    return isinstance(value, str) and 0 < len(value) <= 1000 and value == value.strip() and all(
        ord(c) >= 32 and not 127 <= ord(c) <= 159 for c in value)


def _timestamp(value: object) -> None:
    require(isinstance(value, str) and re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?(?:Z|\+00:00)", value) is not None,
        "Invalid UTC microsecond timestamp")
    try:
        datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ScenarioError("Invalid civil timestamp") from error


def projection(items) -> list[dict]:
    values = list(items.values()) if isinstance(items, dict) else list(items)
    rows = [project_item(item) for item in values]
    require(len({item["id"] for item in rows}) == len(rows), "Duplicate projected identity")
    return sorted(rows, key=lambda item: item["id"])


def replace_status_request(current_item: dict, status: str) -> dict:
    require(type(current_item) is dict and set(current_item) == ITEM_KEYS, "Incomplete or future replacement body")
    project_item(current_item)
    require(current_item["deleted_at"] is None and status in {"planned", "completed"}, "Invalid scenario replacement")
    require(current_item["kind"] == "task" and current_item["recurrence"] is None, "Replace only a one-off scenario leaf")
    fields = {key: copy.deepcopy(current_item[key]) for key in ITEM_FIELDS}
    fields.update(status=status, blocked_reason_kind=None, blocked_by_item_id=None, blocked_reason=None)
    return {"expected_revision": current_item["revision"], "item": fields}


@dataclass(frozen=True)
class DeltaProjection:
    items: dict[str, dict]
    tombstones: dict[str, dict]
    cursor: str


def fold_delta_pages(pages, *, initial_items=(), initial_cursor: str | None = None,
                     snapshot: bool = False) -> DeltaProjection:
    """Return only a bounded terminal forest; never mutate caller-owned input."""
    pages = list(pages)
    require(0 < len(pages) <= 100, "Missing or excessive delta pages")
    initial = list(initial_items.values()) if isinstance(initial_items, dict) else list(initial_items)
    require(not snapshot or not initial, "Replacement snapshot must not inherit stale items")
    projection(initial)
    items = {item["id"]: copy.deepcopy(item) for item in initial}
    tombstones, revisions = {}, {item["id"]: item["revision"] for item in initial}
    cursors = {initial_cursor} if initial_cursor is not None else set()
    seen_snapshot, count, total_bytes = set(), 0, 0
    for index, page in enumerate(pages):
        require(type(page) is dict and set(page) == {"changes", "next_cursor", "has_more"}, "Invalid delta envelope")
        cursor, more, changes = page["next_cursor"], page["has_more"], page["changes"]
        require(isinstance(cursor, str) and 0 < len(cursor.encode()) <= 2048 and not any(c.isspace() for c in cursor), "Invalid delta cursor")
        require(type(more) is bool and more == (index < len(pages) - 1), "Missing or premature terminal page")
        require(type(changes) is list and len(changes) <= 300 and (not more or bool(changes)), "Invalid delta page count")
        require(cursor not in cursors or (not more and not changes and index == 0 and cursor == initial_cursor), "Repeated delta cursor")
        cursors.add(cursor)
        size = len(json.dumps(page, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode())
        count += len(changes); total_bytes += size
        require(size <= 8 * 1024 * 1024 and total_bytes <= 32 * 1024 * 1024 and count <= 20_000, "Delta bound exceeded")
        for change in changes:
            require(type(change) is dict, "Invalid delta change")
            if change.get("type") == "upsert":
                require(set(change) == {"type", "item"}, "Invalid upsert envelope")
                item = change["item"]
                require(type(item) is dict and set(item) == ITEM_KEYS and item["deleted_at"] is None, "Invalid active item body")
                project_item(item)
                _timestamp(item["created_at"]); _timestamp(item["updated_at"])
                if item["completed_at"] is not None:
                    _timestamp(item["completed_at"])
            else:
                require(set(change) == {"type", "tombstone"} and change["type"] == "tombstone", "Unknown delta change")
                item = change["tombstone"]
                require(type(item) is dict and set(item) == {"id", "revision", "deleted_at", "parent_id"}, "Invalid tombstone")
                canonical_uuid(item["id"]); checked_revision(item["revision"])
                _timestamp(item["deleted_at"])
                if item["parent_id"] is not None:
                    canonical_uuid(item["parent_id"])
                    require(item["parent_id"] != item["id"], "Self-parent tombstone")
            identity, revision = item["id"], item["revision"]
            require(revision > revisions.get(identity, 0), "Duplicate or out-of-order item revision")
            require(not snapshot or identity not in seen_snapshot, "Duplicate current snapshot member")
            seen_snapshot.add(identity); revisions[identity] = revision
            if change["type"] == "upsert":
                items[identity] = copy.deepcopy(item); tombstones.pop(identity, None)
            else:
                tombstones[identity] = copy.deepcopy(item); items.pop(identity, None)
    require(len(items) + len(tombstones) <= 20_000, "Final forest bound exceeded")
    _validate_forest(items)
    return DeltaProjection(items, tombstones, pages[-1]["next_cursor"])


def _validate_forest(items: dict[str, dict]) -> None:
    children, queue = {}, []
    for identity, item in items.items():
        parent = item["parent_id"]
        if parent is None:
            queue.append(identity)
        else:
            require(parent in items, "Incomplete terminal ancestry")
            children.setdefault(parent, []).append(identity)
    offset = 0
    while offset < len(queue):
        queue.extend(children.get(queue[offset], ())); offset += 1
    require(len(queue) == len(items), "Cyclic terminal ancestry")


UNTOUCHED_TABLES = (
    "execution_sessions", "item_progress", "item_progress_operations", "habit_occurrence_evidence",
    "habit_occurrence_outcomes", "habit_occurrence_versions", "habit_pauses", "habit_pause_versions",
    "habit_changes", "habit_operation_receipts", "habit_occurrence_publications", "habit_missed_resolutions",
    "habit_missed_resolution_versions",
)


def final_sql(workspace_id: str) -> str:
    """Read-only query, scoped to one validated fresh synthetic workspace."""
    scope = "workspace_id='" + canonical_uuid(workspace_id) + "'::uuid"

    def rows(table, body, order):
        return f"(SELECT COALESCE(json_agg({body} ORDER BY {order}),'[]'::json) FROM {table} WHERE {scope})"

    item = "json_build_object(" + ",".join(
        f"'{key}'," + ("hierarchy.parent_item_id" if key == "parent_id" else f"item.{key}")
        for key in PROJECTION_KEYS) + ")"
    # Parentage is normalized; LEFT JOIN preserves roots with no edge. Both
    # identity and workspace must match so the join cannot cross tenant scope.
    item_rows = (f"(SELECT COALESCE(json_agg({item} ORDER BY item.id),'[]'::json) FROM items AS item "
                 "LEFT JOIN item_hierarchy AS hierarchy ON hierarchy.workspace_id=item.workspace_id "
                 f"AND hierarchy.child_item_id=item.id WHERE item.{scope})")
    operations = "json_build_object('operation_id',operation_id,'item_id',item_id,'request',request_json,'result',result_json)"
    effects = "json_build_object('item_id',item_id,'completion_revision',completion_revision,'before_item_revision',before_item_revision,'after_item_revision',after_item_revision,'before_state',before_state_json,'after_state',after_state_json)"
    evaluations = "json_build_object('cause_kind',cause_kind,'effect_count',effect_count,'execution_revision',execution_revision)"
    untouched = "json_build_object(" + ",".join(
        f"'{table}',(SELECT count(*) FROM {table} WHERE {scope})" for table in UNTOUCHED_TABLES) + ")"
    return "SELECT json_build_object(" + ",".join((
        "'items'," + item_rows,
        "'states'," + rows("item_completion_state", "state_json", "item_id"),
        "'operations'," + rows("item_completion_operations", operations, "operation_id"),
        "'effects'," + rows("item_completion_effects", effects, "item_id,completion_revision"),
        "'evaluations'," + rows("item_completion_evaluations", evaluations, "cause_kind,effect_count"),
        "'execution_state'," + rows("execution_state", "json_build_object('revision',revision,'active_session_id',active_session_id)", "workspace_id"),
        "'untouched'," + untouched,
    )) + ")"


def assert_final_sql(evidence: dict, ids: dict[str, str], original_a_snapshot: dict,
                     operation_commands: dict[str, dict] | None = None) -> None:
    """Validate DB custody independently of native PASS markers.

    When available, commands A..E must come from the exact client journals, not
    be reconstructed from these queried receipt rows. UUID fields compare by
    typed identity (Swift may emit uppercase); other fields remain exact. Native
    tests independently prove raw request-byte replay. B must be absent.
    Schedule preview/publication is legitimate and intentionally not forbidden.
    """
    validate_ids(ids)
    require(type(evidence) is dict and set(evidence) == {
        "items", "states", "operations", "effects", "evaluations", "execution_state", "untouched"}, "Incomplete SQL evidence")
    expected = {
        "root": (8, "blocked", None, "manual", None, BLOCKED_REASON),
        "branch": (5, "planned", ids["root"], None, None, None),
        "required": (2, "completed", ids["branch"], None, None, None),
        "optional": (2, "planned", ids["root"], None, None, None),
        "new_child": (1, "planned", ids["branch"], None, None, None),
    }
    expected_items = sorted([dict(zip(PROJECTION_KEYS, (ids[name], *values))) for name, values in expected.items()], key=lambda x: x["id"])
    require(projection(evidence["items"]) == expected_items, "Final canonical projection differs from exact scenario")
    states = _unique_rows(evidence["states"], "item_id")
    require(set(states) == {ids["root"], ids["branch"], ids["optional"]}, "Unexpected completion sidecars")
    for name, revision in (("root", 5), ("branch", 2), ("optional", 1)):
        state = states[ids[name]]
        require(state["revision"] == revision and type(state["revision"]) is int
                and state["mode"] == "automatic" and state["provenance"] is None
                and state["required_for_parent"] is (name != "optional"), "Final completion policy mismatch")
    operations = _unique_rows(evidence["operations"], "operation_id")
    require(len(operations) == 4, "Expected exactly four successful completion operations")
    expected_commands = {"A": ("root", "complete", True, 3, 0), "C": ("root", "keep_open", True, 4, 1),
                         "D": ("root", "automatic", True, 5, 2), "E": ("optional", "automatic", False, 1, 0)}
    if operation_commands is not None:
        require(set(operation_commands) == {"A", "B", "C", "D", "E"}, "Incomplete client command custody")
        operation_commands = {label: _typed_command(command) for label, command in operation_commands.items()}
        operation_ids = {canonical_uuid(command["operation_id"]) for command in operation_commands.values()}
        require(len(operation_ids) == 5 and operation_commands["B"]["operation_id"] not in operations, "Failed command acquired a receipt")
    by_label = {}
    for label, (name, mode, required, item_revision, completion_revision) in expected_commands.items():
        candidates = [row for row in operations.values() if row["item_id"] == ids[name]
                      and row["request"].get("mode") == mode
                      and row["request"].get("expected_completion_revision") == completion_revision]
        require(len(candidates) == 1, "Missing or duplicated reviewed command")
        row = candidates[0]; command, result = row["request"], row["result"]
        require(set(command) == {"schema_version", "operation_id", "expected_item_revision", "expected_completion_revision",
                                "expected_evidence_hash", "required_for_parent", "mode", "reopening"}, "Unexpected command shape")
        require(type(command["schema_version"]) is int and command["schema_version"] == 1
                and command["operation_id"] == row["operation_id"]
                and type(command["expected_item_revision"]) is int and command["expected_item_revision"] == item_revision
                and type(command["expected_completion_revision"]) is int and command["expected_completion_revision"] == completion_revision
                and command["required_for_parent"] is required and command["reopening"] is None
                and isinstance(command["expected_evidence_hash"], str)
                and re.fullmatch(r"sha256:[0-9a-f]{64}", command["expected_evidence_hash"]) is not None,
                "Reviewed command evidence mismatch")
        require(result["item_id"] == ids[name] and result["item_revision"] == item_revision + 1
                and result["state"]["item_id"] == ids[name] and result["state"]["revision"] == completion_revision + 1
                and result["state"]["mode"] == mode and result["state"]["required_for_parent"] is required,
                "Historical result does not match reviewed revisions")
        if operation_commands is not None:
            require(command == operation_commands[label], "Receipt differs from exact client command")
        by_label[label] = result
    require(by_label["A"] == original_a_snapshot, "Original successful A snapshot changed")
    reopen = {"status": "blocked", "blocked_reason_kind": "manual", "blocked_by_item_id": None, "blocked_reason": BLOCKED_REASON}
    require(by_label["A"]["state"]["provenance"] == {"kind": "manual", "reopen": reopen}, "A lost exact blocked reopening custody")
    effects = evidence["effects"]
    require(len(effects) == 8, "Unexpected derived effect count")
    # Completing an existing child does not refresh its unchanged parent edge:
    # branch 2 -> 3 is the first derived effect. Creating a new child later
    # refreshes branch 3 -> 4 before its separate reopening effect 4 -> 5.
    expected_item_transitions = {
        "root": [(3, 4), (4, 5), (5, 6), (6, 7), (7, 8)],
        "branch": [(2, 3), (4, 5)],
        "optional": [(1, 2)],
    }
    for name, count in (("root", 5), ("branch", 2), ("optional", 1)):
        chain = sorted([row for row in effects if row["item_id"] == ids[name]], key=lambda row: row["completion_revision"])
        require([row["completion_revision"] for row in chain] == list(range(1, count + 1)), "Missing or duplicate effect revision")
        require([(checked_revision(row["before_item_revision"]), checked_revision(row["after_item_revision"]))
                 for row in chain] == expected_item_transitions[name], "Effect canonical revisions differ from exact writer sequence")
        for index, row in enumerate(chain):
            require(row["after_item_revision"] == row["before_item_revision"] + 1
                    and row["before_state"]["revision"] == index and row["after_state"]["revision"] == index + 1,
                    "Effect does not retain exact revision chain")
            if index:
                require(row["before_state"] == chain[index - 1]["after_state"], "Effect provenance chain changed")
        require(chain[-1]["after_state"] == states[ids[name]], "Final sidecar is not last retained effect")
        if name in {"root", "branch"}:
            automatic = chain[-2]["after_state"]["provenance"]
            expected_reopen = reopen if name == "root" else dict(reopen, status="planned", blocked_reason_kind=None, blocked_reason=None)
            require(automatic == {"kind": "automatic", "reopen": expected_reopen}, "Automatic completion lost reopening tuple")
    require(sorted((row["cause_kind"], row["effect_count"], row["execution_revision"]) for row in evidence["evaluations"])
            == [("canonical_write", 2, 0)] * 2 + [("policy_command", 1, 0)] * 4, "Unexpected evaluation causes or execution credit")
    require(evidence["untouched"] == {table: 0 for table in UNTOUCHED_TABLES}, "Scenario changed execution/progress/habit data")
    require(len(evidence["execution_state"]) <= 1 and all(row == {"revision": 0, "active_session_id": None}
            for row in evidence["execution_state"]), "Scenario changed execution state")


def _unique_rows(rows: list, key: str) -> dict:
    require(type(rows) is list and all(type(row) is dict and key in row for row in rows), "Invalid SQL evidence rows")
    result = {canonical_uuid(row[key]): row for row in rows}
    require(len(result) == len(rows), "Duplicate SQL evidence identity")
    return result


def _typed_command(command: dict) -> dict:
    def identity(value):
        require(isinstance(value, str) and re.fullmatch(
            r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", value) is not None, "Invalid typed UUID")
        return canonical_uuid(value.lower())

    result = copy.deepcopy(command)
    result["operation_id"] = identity(result["operation_id"])
    if isinstance(result.get("reopening"), dict) and result["reopening"].get("blocked_by_item_id") is not None:
        result["reopening"]["blocked_by_item_id"] = identity(result["reopening"]["blocked_by_item_id"])
    return result
