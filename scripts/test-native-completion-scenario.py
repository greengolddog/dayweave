#!/usr/bin/env python3
"""Pure synthetic fixture/evidence tests. No subprocesses, network or credentials."""
import copy
import unittest
import uuid

import native_completion_scenario as scenario


IDS = {name: str(uuid.UUID(int=index)) for index, name in enumerate(
    ("root", "branch", "required", "optional", "new_child"), 1)}
STAMP = "2026-09-09T10:00:00.123456Z"
HASH = "sha256:" + "a" * 64


def item(request, revision=1):
    return dict(copy.deepcopy(request), revision=revision, is_executable=request["kind"] == "task",
                created_at=STAMP, updated_at=STAMP, completed_at=None, deleted_at=None)


def page(changes, cursor="opaque-terminal", more=False):
    return {"changes": changes, "next_cursor": cursor, "has_more": more}


def upsert(value):
    return {"type": "upsert", "item": value}


def state(identity, revision=0, mode="automatic", required=True, provenance=None):
    return {"item_id": identity, "revision": revision, "mode": mode, "required_for_parent": required,
            "provenance": copy.deepcopy(provenance), "updated_at": STAMP if revision else None}


def evidence_fixture():
    reopen = {"status": "blocked", "blocked_reason_kind": "manual", "blocked_by_item_id": None,
              "blocked_reason": scenario.BLOCKED_REASON}
    manual = {"kind": "manual", "reopen": reopen}
    auto = {"kind": "automatic", "reopen": reopen}
    branch_auto = {"kind": "automatic", "reopen": dict(reopen, status="planned", blocked_reason_kind=None, blocked_reason=None)}
    chains = {
        "root": [state(IDS["root"]), state(IDS["root"], 1, "complete", provenance=manual),
                 state(IDS["root"], 2, "keep_open"), state(IDS["root"], 3),
                 state(IDS["root"], 4, provenance=auto), state(IDS["root"], 5)],
        "branch": [state(IDS["branch"]), state(IDS["branch"], 1, provenance=branch_auto), state(IDS["branch"], 2)],
        "optional": [state(IDS["optional"]), state(IDS["optional"], 1, required=False)],
    }
    effects = []
    for name, after_revisions in (("root", [4, 5, 6, 7, 8]), ("branch", [3, 5]), ("optional", [2])):
        for index, revision in enumerate(after_revisions, 1):
            effects.append({"item_id": IDS[name], "completion_revision": index, "before_item_revision": revision - 1,
                            "after_item_revision": revision, "before_state": chains[name][index - 1], "after_state": chains[name][index]})
    commands, operations = {}, []
    for index, (label, name, mode, required, ir, cr) in enumerate([
        ("A", "root", "complete", True, 3, 0), ("B", "root", "keep_open", True, 3, 0),
        ("C", "root", "keep_open", True, 4, 1), ("D", "root", "automatic", True, 5, 2),
        ("E", "optional", "automatic", False, 1, 0)], 101):
        command = {"schema_version": 1, "operation_id": str(uuid.UUID(int=index)), "expected_item_revision": ir,
                   "expected_completion_revision": cr, "expected_evidence_hash": HASH,
                   "required_for_parent": required, "mode": mode, "reopening": None}
        commands[label] = command
        if label == "B":
            continue
        result = {"schema_version": 1, "item_id": IDS[name], "item_revision": ir + 1,
                  "state": chains[name][cr + 1], "evidence_hash": HASH,
                  "counts": {"required_descendants": 3 if name == "root" else 0, "completed": 0,
                             "incomplete": 3 if name == "root" else 0, "occurrence_evidence_required": 0},
                  "occurrence_evidence_required": False}
        operations.append({"operation_id": command["operation_id"], "item_id": IDS[name], "request": command, "result": result})
    values = [item(value) for value in scenario.seed_requests(IDS)] + [item(scenario.new_child_request(IDS))]
    for value, revision in zip(values, (8, 5, 2, 2, 1)):
        value["revision"] = revision
    values[2]["status"] = "completed"
    evidence = {"items": scenario.projection(values), "states": [chain[-1] for chain in chains.values()],
                "operations": operations, "effects": effects,
                "evaluations": [{"cause_kind": "policy_command", "effect_count": 1, "execution_revision": 0}] * 4
                    + [{"cause_kind": "canonical_write", "effect_count": 2, "execution_revision": 0}] * 2,
                "execution_state": [{"revision": 0, "active_session_id": None}],
                "untouched": {table: 0 for table in scenario.UNTOUCHED_TABLES}}
    return copy.deepcopy(evidence), copy.deepcopy(commands), copy.deepcopy(operations[0]["result"])


class ScenarioTests(unittest.TestCase):
    def test_seed_shapes_and_order(self):
        requests = scenario.seed_requests(IDS)
        self.assertEqual([value["id"] for value in requests], [IDS[key] for key in ("root", "branch", "required", "optional")])
        self.assertEqual(requests[0]["blocked_reason"], scenario.BLOCKED_REASON)
        self.assertEqual(requests[0]["blocked_reason_kind"], "manual")
        self.assertTrue(all(value["recurrence"] is None and value["has_own_effort"] is False for value in requests))
        self.assertTrue(all("required_for_parent" not in value for value in requests))
        self.assertEqual(requests[1]["kind"], "project")
        self.assertEqual(scenario.new_child_request(IDS)["parent_id"], IDS["branch"])
        with self.assertRaises(scenario.ScenarioError):
            scenario.seed_requests(dict(IDS, optional=IDS["required"]))

    def test_full_replace_is_lossless_and_does_not_send_response_fields(self):
        original = item(scenario.seed_requests(IDS)[2], 17)
        original["notes"] = "Synthetic retained notes"
        before = copy.deepcopy(original)
        result = scenario.replace_status_request(original, "completed")
        self.assertEqual(result["expected_revision"], 17)
        self.assertEqual(result["item"], {key: "completed" if key == "status" else before[key] for key in scenario.ITEM_FIELDS})
        self.assertEqual(original, before)
        with self.assertRaises(scenario.ScenarioError):
            scenario.replace_status_request(dict(original, future=True), "completed")
        with self.assertRaises(scenario.ScenarioError):
            scenario.replace_status_request({key: value for key, value in original.items() if key != "deadline_kind"}, "completed")

    def test_ordered_history_allows_primary_then_derived_revision(self):
        root = item(scenario.seed_requests(IDS)[0])
        later = dict(root, revision=2)
        result = scenario.fold_delta_pages([page([upsert(root)], "next", True), page([upsert(later)])])
        self.assertEqual(result.items[root["id"]]["revision"], 2)
        self.assertEqual(root["revision"], 1)
        self.assertEqual(result.cursor, "opaque-terminal")
        for changes in ([upsert(root), upsert(root)], [upsert(later), upsert(root)]):
            with self.assertRaises(scenario.ScenarioError):
                scenario.fold_delta_pages([page(changes)])
        with self.assertRaises(scenario.ScenarioError):
            scenario.fold_delta_pages([page([upsert(root), upsert(later)])], snapshot=True)

    def test_terminal_cursor_and_page_bounds(self):
        root = item(scenario.seed_requests(IDS)[0])
        malformed = [[], [page([], more=True)], [page([]), page([])],
                     [page([upsert(root)], "same", True), page([], "same")],
                     [page([upsert(root)] * 301)], [dict(page([]), future=True)],
                     [page([], "")], [dict(page([]), has_more=0)]]
        for pages in malformed:
            with self.subTest(pages=len(pages)), self.assertRaises(scenario.ScenarioError):
                scenario.fold_delta_pages(pages)
        result = scenario.fold_delta_pages([page([], "unchanged")], initial_items=[root], initial_cursor="unchanged")
        self.assertEqual(result.items[root["id"]], root)

    def test_child_first_pages_validate_only_complete_terminal_forest(self):
        root, branch, required, optional = [item(value) for value in scenario.seed_requests(IDS)]
        result = scenario.fold_delta_pages([page([upsert(required), upsert(optional)], "next", True),
                                            page([upsert(branch), upsert(root)])], snapshot=True)
        self.assertEqual(len(result.items), 4)
        with self.assertRaises(scenario.ScenarioError):
            scenario.fold_delta_pages([page([upsert(required)])])
        with self.assertRaises(scenario.ScenarioError):
            scenario.fold_delta_pages([page([upsert(dict(root, parent_id=branch["id"])), upsert(branch)])])

    def test_deep_tree_is_iterative_and_wire_bytes_are_bounded(self):
        values = []
        for index in range(5000):
            request = scenario.create_request(str(uuid.UUID(int=index + 10000)), kind="task", title="Synthetic deep item",
                parent_id=None if index == 0 else str(uuid.UUID(int=index + 9999)))
            values.append(upsert(item(request)))
        values.reverse()
        chunks = [values[index:index + 300] for index in range(0, len(values), 300)]
        pages = [page(chunk, f"deep-{index}", index < len(chunks) - 1) for index, chunk in enumerate(chunks)]
        self.assertEqual(len(scenario.fold_delta_pages(pages, snapshot=True).items), 5000)
        oversized = item(scenario.seed_requests(IDS)[0]); oversized["notes"] = "x" * (8 * 1024 * 1024)
        with self.assertRaises(scenario.ScenarioError):
            scenario.fold_delta_pages([page([upsert(oversized)])])

    def test_bodyless_tombstone_and_restore_preserve_revision_order(self):
        root = item(scenario.seed_requests(IDS)[0])
        tombstone = {"type": "tombstone", "tombstone": {"id": root["id"], "revision": 2, "parent_id": None, "deleted_at": STAMP}}
        result = scenario.fold_delta_pages([page([upsert(root), tombstone])])
        self.assertFalse(result.items)
        self.assertEqual(result.tombstones[root["id"]]["revision"], 2)
        restored = scenario.fold_delta_pages([page([upsert(root), tombstone, upsert(dict(root, revision=3))])])
        self.assertFalse(restored.tombstones)
        self.assertEqual(restored.items[root["id"]]["revision"], 3)
        for stamp in ("", "2026-02-30T00:00:00Z", "2026-09-09T10:00:00.1234567Z", "2026-09-09T10:00:00+01:00"):
            invalid = copy.deepcopy(tombstone); invalid["tombstone"]["deleted_at"] = stamp
            with self.assertRaises(scenario.ScenarioError):
                scenario.fold_delta_pages([page([invalid])])

    def test_raw_duplicate_and_noninteger_revisions_reject(self):
        for raw in ('{"revision":1,"revision":2}', '{"revision":1,"revi\\u0073ion":2}', '{"revision":NaN}'):
            with self.assertRaises(scenario.ScenarioError):
                scenario.strict_json(raw)
        for raw in ('1.0', '1e0', 'true', '9223372036854775808'):
            with self.assertRaises(scenario.ScenarioError):
                scenario.checked_revision(scenario.strict_json(raw))
        self.assertEqual(scenario.checked_revision(scenario.strict_json('9223372036854775807')), scenario.I64_MAX)

    def test_sql_is_scoped_and_rejects_interpolation(self):
        workspace = str(uuid.UUID(int=1000))
        sql = scenario.final_sql(workspace)
        self.assertEqual(sql.count(" WHERE "), sql.count("workspace_id='" + workspace + "'::uuid"))
        self.assertIn("FROM items AS item LEFT JOIN item_hierarchy AS hierarchy", sql)
        self.assertIn("ON hierarchy.workspace_id=item.workspace_id AND hierarchy.child_item_id=item.id", sql)
        self.assertIn("WHERE item.workspace_id='" + workspace + "'::uuid", sql)
        self.assertIn("'parent_id',hierarchy.parent_item_id", sql)
        self.assertNotIn("item.parent_id", sql)
        self.assertNotIn("'parent_id',parent_id", sql)
        self.assertNotIn("schedule", sql)
        for invalid in ("' OR true --", "00000000-0000-0000-0000-000000000000", str(uuid.UUID(int=1000)).upper() + " "):
            with self.assertRaises(scenario.ScenarioError):
                scenario.final_sql(invalid)

    def test_final_evidence_rejects_extra_receipts_and_historical_rollback(self):
        evidence, commands, original = evidence_fixture()
        scenario.assert_final_sql(evidence, IDS, original, commands)
        scenario.assert_final_sql(evidence, IDS, original)
        mutations = [lambda x: x["operations"].append(copy.deepcopy(x["operations"][0])),
                     lambda x: x["items"][0].update(status="completed", blocked_reason_kind=None, blocked_reason=None),
                     lambda x: x["operations"][0]["result"].update(evidence_hash="sha256:" + "b" * 64),
                     lambda x: x["states"][0].update(revision=4),
                     lambda x: x["effects"].pop(),
                     lambda x: x["untouched"].update(item_progress_operations=1),
                     lambda x: x["execution_state"][0].update(revision=1),
                     lambda x: x["evaluations"].append({"cause_kind": "canonical_write", "effect_count": 1, "execution_revision": 0})]
        for mutate in mutations:
            changed = copy.deepcopy(evidence); mutate(changed)
            with self.assertRaises(scenario.ScenarioError):
                scenario.assert_final_sql(changed, IDS, original, commands)

    def test_native_uuid_case_is_typed_without_relaxing_other_request_fields(self):
        evidence, commands, original = evidence_fixture()
        # Include hexadecimal letters so this is an actual case difference.
        for label, command in commands.items():
            old_id = command["operation_id"]
            new_id = "abcdefab-cdef-4abc-8def-" + old_id[-12:]
            command["operation_id"] = new_id.upper()
            for row in evidence["operations"]:
                if row["operation_id"] == old_id:
                    row["operation_id"] = new_id; row["request"]["operation_id"] = new_id
        scenario.assert_final_sql(evidence, IDS, original, commands)
        changed = copy.deepcopy(commands); changed["A"]["required_for_parent"] = False
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_final_sql(evidence, IDS, original, changed)
        changed = copy.deepcopy(commands); changed["A"]["expected_evidence_hash"] = "sha256:" + "c" * 64
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_final_sql(evidence, IDS, original, changed)

    def test_branch_effects_follow_unchanged_edge_then_new_child_refresh(self):
        evidence, commands, original = evidence_fixture()
        branch = [row for row in evidence["effects"] if row["item_id"] == IDS["branch"]]
        self.assertEqual([(row["before_item_revision"], row["after_item_revision"]) for row in branch], [(2, 3), (4, 5)])
        scenario.assert_final_sql(evidence, IDS, original, commands)
        # Each fabricated pair remains +1, but cites the wrong primary/derived
        # boundary. The final projection alone cannot detect the first error.
        for index, before, after in ((0, 3, 4), (1, 5, 6), (1, 3, 4)):
            changed = copy.deepcopy(evidence)
            effects = [row for row in changed["effects"] if row["item_id"] == IDS["branch"]]
            effects[index].update(before_item_revision=before, after_item_revision=after)
            with self.subTest(index=index, before=before, after=after), self.assertRaises(scenario.ScenarioError):
                scenario.assert_final_sql(changed, IDS, original, commands)


if __name__ == "__main__":
    unittest.main()
