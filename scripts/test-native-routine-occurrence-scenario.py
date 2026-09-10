#!/usr/bin/env python3
"""Inert unit tests: synthetic JSON only, no HTTP, SQL, filesystem or clock IO."""
from __future__ import annotations

import copy
from datetime import datetime, timedelta
import unittest
import uuid

import native_routine_occurrence_scenario as scenario


def uid(number: int) -> str:
    return str(uuid.UUID(int=number))


IDS = {name: uid(index) for index, name in enumerate(
    ("root", "branch", "required", "optional", "inbox", "blocked"), start=1)}
INSTANCE, SENTINEL, EXTRA = uid(101), uid(102), uid(103)
FIRST_PUBLICATION, FRESH_PUBLICATION = uid(201), uid(202)
HASH = "sha256:" + "a" * 64


def stamp(offset: int = 0) -> str:
    return (datetime(2026, 9, 10, 0, 0, 0, 123456) + timedelta(seconds=offset)).isoformat() + "Z"


def open_state(name: str) -> dict:
    return {"status": "blocked" if name == "blocked" else "inbox" if name == "inbox" else "planned",
            "blocked_reason_kind": "manual" if name == "blocked" else None,
            "blocked_by_item_id": None, "blocked_reason": scenario.BLOCKED_REASON if name == "blocked" else None}


def aggregate(instance: str = INSTANCE, day: int = 11) -> dict:
    definitions, members = [], []
    for name, item_id in IDS.items():
        required = name in {"root", "branch", "required"}
        definitions.append({"item_id": item_id,
            "parent_id": None if name == "root" else IDS["branch" if name == "required" else "root"],
            "source_revision": 2 if name in {"optional", "inbox", "blocked"} else 1,
            "title": scenario.TITLE_BY_NAME[name], "kind": "routine" if name == "root" else "task",
            "recurs": name == "root", "sibling_order": 0, "required_for_parent": required,
            "initial_open": open_state(name)})
        members.append({"item_id": item_id, "revision": 1, "status": open_state(name)["status"],
            "required_for_parent": required, "mode": "automatic", "open": open_state(name),
            "provenance": None, "completed_at": None, "updated_at": stamp()})
    return {"revision": 1, "manifest": {"schema_version": 1, "id": instance,
        "series_item_id": IDS["root"], "occurrence_id": str(uuid.uuid5(uuid.UUID(IDS["root"]), f"test-day-{day}")),
        "identity": {"type": "calendar_day", "date": f"2026-09-{day:02}", "bucket_ordinal": 0},
        "nominal_start": f"2026-09-{day:02}T00:00:00Z", "nominal_end": f"2026-09-{day + 1:02}T00:00:00Z",
        "window_start": f"2026-09-{day:02}T00:00:00Z", "window_end": f"2026-09-{day + 1:02}T00:00:00Z",
        "timezone_name": "UTC", "definition_hash": HASH, "members": definitions}, "members": members}


def by_member(value: dict, name: str) -> dict:
    return next(member for member in value["members"] if member["item_id"] == IDS[name])


def lifecycle() -> dict[int, dict]:
    values = {1: aggregate()}
    for revision in range(2, 7):
        value = copy.deepcopy(values[revision - 1])
        value["revision"] = revision
        names = ("root", "branch", "required") if revision == 2 else ("root",) if revision < 5 else ("blocked",)
        for name in names:
            member = by_member(value, name)
            member["revision"] += 1
            member["updated_at"] = stamp(revision - 1)
            if revision in {2, 4}:
                member.update(status="completed", completed_at=stamp(revision - 1), mode="automatic",
                    provenance=None if name == "required" else {"kind": "automatic", "reopen": open_state(name)})
            elif revision == 3:
                member.update(status="planned", completed_at=None, mode="keep_open", provenance=None)
            else:
                member["status"] = "skipped" if revision == 5 else "blocked"
        values[revision] = value
    return values


def snapshot(value: dict) -> dict:
    evaluations = []
    for name, item_id in IDS.items():
        count = 2 if name == "root" else 1 if name == "branch" else 0
        evaluations.append({"item_id": item_id, "occurrence_evidence_required": False, "reason": "unchanged",
            "counts": {"required_descendants": count, "completed": count if value["revision"] >= 2 else 0,
                "incomplete": 0 if value["revision"] >= 2 else count, "occurrence_evidence_required": 0}})
    return {"schema_version": 1, "aggregate": copy.deepcopy(value), "evidence_hash": HASH,
            "fresh_edit_eligible": True, "members": evaluations}


def commands() -> dict[str, dict]:
    result = {}
    for offset, label in enumerate("ABCDEF", start=301):
        action = ({"type": "set_outcome", "status": "completed"} if label == "A" else
            {"type": "set_outcome", "status": "skipped"} if label in {"B", "E"} else
            {"type": "reopen", "open": open_state("blocked")} if label == "F" else
            {"type": "set_policy", "required_for_parent": True, "mode": "keep_open" if label == "C" else "automatic"})
        result[label] = {"schema_version": 1, "operation_id": uid(offset),
            "expected_instance_revision": {"A": 1, "B": 1, "C": 2, "D": 3, "E": 4, "F": 5}[label],
            "expected_member_revision": {"A": 1, "B": 1, "C": 2, "D": 3, "E": 1, "F": 2}[label],
            "expected_evidence_hash": HASH, "action": action}
    return result


def append_admission(evidence: dict, value: dict, sequence: int, publication: str) -> None:
    manifest = value["manifest"]
    instance = manifest["id"]
    evidence["manifests"].append({"id": instance, "series_item_id": IDS["root"],
        "occurrence_id": manifest["occurrence_id"], "definition_hash": HASH, "manifest": copy.deepcopy(manifest),
        "member_count": 6, "first_schedule_revision_id": publication})
    evidence["members"].extend({"instance_id": instance, "item_id": row["item_id"],
        "parent_item_id": row["parent_id"], "source_revision": row["source_revision"]} for row in manifest["members"])
    evidence["states"].append({"instance_id": instance, "revision": 1, "aggregate": copy.deepcopy(value)})
    evidence["changes"].append({"sequence": sequence, "instance_id": instance, "revision": 1,
        "operation_id": None, "before": None, "aggregate": copy.deepcopy(value), "effects": [], "changed_at": stamp()})
    evidence["publications"].append({"instance_id": instance, "schedule_revision_id": publication,
        "source_revisions": {row["item_id"]: row["source_revision"] for row in manifest["members"]}})


def sql_fixture(*, extra: bool = False) -> tuple[dict, dict, dict, dict]:
    values, saved = lifecycle(), commands()
    baseline = {key: [] for key in scenario.SQL_KEYS}
    baseline["untouched"] = dict.fromkeys(scenario.UNTOUCHED_TABLES, 0)
    baseline["execution_state"] = [{"revision": 0, "active_session_id": None}]
    sequence = 0
    for request in scenario.seed_requests(IDS):
        name = next(name for name in IDS if IDS[name] == request["id"])
        revision = 2 if name in {"optional", "inbox", "blocked"} else 1
        item = dict(request, revision=revision)
        baseline["items"].append({key: item[key] for key in scenario.PROJECTION_KEYS})
        for source_revision in range(1, revision + 1):
            sequence += 1
            baseline["item_changes"].append({"sequence": sequence, "item_id": item["id"],
                "revision": source_revision, "kind": "upsert", "payload": dict(item, revision=source_revision)})
        if revision == 2:
            baseline["completion_states"].append({"item_id": item["id"], "revision": 1,
                "required_for_parent": False, "mode": "automatic", "provenance": None, "updated_at": stamp()})
    baseline["schedule_revisions"] = [{"id": FIRST_PUBLICATION, "revision_number": 1, "input_digest": HASH,
        "schema": "5", "publication_schema": "dayweave-scheduler-publication/5", "occurrence_head": None}]
    baseline["schedule_requests"] = [{"idempotency_key": uid(401), "schedule_revision_id": FIRST_PUBLICATION,
        "request_hash": "b" * 64}]
    append_admission(baseline, values[1], 1, FIRST_PUBLICATION)
    append_admission(baseline, aggregate(SENTINEL, 12), 2, FIRST_PUBLICATION)
    final = copy.deepcopy(baseline)
    for label, revision in scenario.SUCCESS_PHASES.items():
        old, new = values[revision - 1], values[revision]
        old_members = {row["item_id"]: row for row in old["members"]}
        effects = [{"before": copy.deepcopy(old_members[row["item_id"]]), "after": copy.deepcopy(row),
                    "reason": "outcome_recorded" if label in {"A", "E"} else "reopened" if label == "F" else "policy_reviewed"}
                   for row in new["members"] if row != old_members[row["item_id"]]]
        final["changes"].append({"sequence": revision + 1, "instance_id": INSTANCE, "revision": revision,
            "operation_id": saved[label]["operation_id"], "before": copy.deepcopy(old), "aggregate": copy.deepcopy(new),
            "effects": effects, "changed_at": stamp(revision - 1)})
        target = IDS["required" if label == "A" else "root" if label in {"C", "D"} else "blocked"]
        final["operations"].append({"operation_id": saved[label]["operation_id"], "instance_id": INSTANCE,
            "member_item_id": target, "actor_user_id": uid(501), "actor_session_id": uid(502),
            "change_sequence": revision + 1, "request": copy.deepcopy(saved[label]), "result": snapshot(new),
            "recorded_at": stamp(revision - 1)})
    final["states"][0].update(revision=6, aggregate=copy.deepcopy(values[6]))
    final["schedule_revisions"].append({"id": FRESH_PUBLICATION, "revision_number": 2, "input_digest": HASH,
        "schema": "6", "publication_schema": "dayweave-scheduler-publication/6", "occurrence_head": 8 if extra else 7})
    final["schedule_requests"].append({"idempotency_key": uid(402), "schedule_revision_id": FRESH_PUBLICATION,
        "request_hash": "c" * 64})
    final["publications"].extend(dict(copy.deepcopy(row), schedule_revision_id=FRESH_PUBLICATION)
                                 for row in baseline["publications"])
    if extra:
        append_admission(final, aggregate(EXTRA, 13), 8, FRESH_PUBLICATION)
    return final, baseline, saved, snapshot(values[2])


class RoutineScenarioTests(unittest.TestCase):
    def audit(self, final, baseline, saved, original_a):
        scenario.assert_final_sql(final, IDS, baseline, saved, original_a,
                                  instance_id=INSTANCE, sentinel_id=SENTINEL)

    def reject_sql(self, mutate):
        final, baseline, saved, original = sql_fixture()
        mutate(final, baseline, saved, original)
        with self.assertRaises(scenario.ScenarioError):
            self.audit(final, baseline, saved, original)

    def test_seed_exact_synthetic_templates(self):
        values = scenario.seed_requests(IDS)
        self.assertEqual([row["id"] for row in values], list(IDS.values()))
        self.assertEqual(values[0]["kind"], "routine")
        self.assertEqual(values[0]["recurrence"], {"type": "daily", "times_per_day": 1})
        for row in values[:2]:
            self.assertEqual(row["duration_kind"], "unknown")
            self.assertIsNone(row["duration_seconds"])
            self.assertIsNone(row["duration_source"])
            self.assertIs(row["has_own_effort"], False)
        for row in values[2:]:
            self.assertEqual((row["duration_kind"], row["duration_seconds"]), ("exact", 60))
        self.assertEqual(values[2]["parent_id"], IDS["branch"])
        self.assertEqual(values[4]["status"], "inbox")
        self.assertEqual({key: values[5][key] for key in scenario.OPEN_KEYS}, open_state("blocked"))
        values[0]["recurrence"]["times_per_day"] = 9
        self.assertEqual(scenario.seed_requests(IDS)[0]["recurrence"]["times_per_day"], 1)

    def test_seed_rejects_extra_missing_duplicate_or_noncanonical_identity(self):
        for value in (dict(IDS, extra=uid(9)), {key: value for key, value in IDS.items() if key != "inbox"},
                      dict(IDS, optional=IDS["required"]), dict(IDS, root="not-a-uuid")):
            with self.subTest(value=value), self.assertRaises(scenario.ScenarioError):
                scenario.seed_requests(value)

    def test_preview_and_publication_are_exact_http_bodies_without_guessed_identity(self):
        request = scenario.preview_request("2026-09-10T23:59:59+00:00")
        self.assertEqual(request["as_of"], "2026-09-10T23:59:59Z")
        self.assertEqual(request["horizon_start"], "2026-09-11T00:00:00Z")
        self.assertEqual(request["horizon_end"], "2026-09-13T00:00:00Z")
        self.assertEqual(request["availability"], [{"start": request["horizon_start"], "end": request["horizon_end"],
            "contexts": [], "location": None, "energy": "deep"}])
        body = scenario.publish_request(request, {"input_digest": HASH}, uid(701))
        self.assertEqual(set(body), {"idempotency_key", "expected_input_digest", "schedule"})
        self.assertEqual(body["expected_input_digest"], HASH)
        self.assertEqual(body["schedule"], request)
        body["schedule"]["availability"].clear()
        self.assertEqual(len(request["availability"]), 1)

    def test_preview_rejects_fractional_or_non_utc_as_of(self):
        for value in ("2026-09-10T00:00:00.000001Z", "2026-09-10T03:00:00+03:00",
                      "2026-02-30T00:00:00Z", "2026-09-10", True):
            with self.subTest(value=value), self.assertRaises(scenario.ScenarioError):
                scenario.preview_request(value)

    def test_publication_rejects_missing_digest_and_unknown_fields(self):
        request = scenario.preview_request("2026-09-10T00:00:00Z")
        with self.assertRaises(scenario.ScenarioError):
            scenario.publish_request(request, {}, uid(701))
        with self.assertRaises(scenario.ScenarioError):
            scenario.publish_request(dict(request, occurrence_id=uid(99)), {"input_digest": HASH}, uid(701))

    def test_optional_command_uses_reviewed_canonical_revisions(self):
        review = {"schema_version": 1, "item_id": IDS["optional"], "item_revision": 9,
            "state": {"item_id": IDS["optional"], "revision": 0}, "evidence_hash": HASH,
            "counts": {}, "occurrence_evidence_required": False}
        command = scenario.optional_completion_command(review, uid(702))
        self.assertEqual((command["expected_item_revision"], command["expected_completion_revision"]), (9, 0))
        self.assertIs(command["required_for_parent"], False)
        self.assertEqual(command["mode"], "automatic")
        self.assertIsNone(command["reopening"])
        for mutate in (lambda row: row.update(item_revision=True),
                       lambda row: row["state"].update(revision=False),
                       lambda row: row["state"].update(item_id=IDS["blocked"])):
            broken = copy.deepcopy(review)
            mutate(broken)
            with self.assertRaises(scenario.ScenarioError):
                scenario.optional_completion_command(broken, uid(703))

    def test_semantic_comparison_normalizes_only_uuid_and_utc_time_positions(self):
        canonical = {"item_id": "abcdefab-1234-4234-8234-123456789abc", "updated_at": "2026-09-10T00:00:00Z"}
        equivalent = {"updated_at": "2026-09-10T00:00:00.000000+00:00", "item_id": canonical["item_id"].upper()}
        self.assertTrue(scenario.semantic_equal(canonical, equivalent))
        self.assertFalse(scenario.semantic_equal({"title": "ABC"}, {"title": "abc"}))
        self.assertTrue(scenario.same_json({"members": [canonical, {"item_id": IDS["root"]}]},
                                           {"members": [{"item_id": IDS["root"]}, equivalent]}))

    def test_semantic_comparison_rejects_type_drift_and_resource_abuse(self):
        for actual, expected in ((True, 1), ({"revision": True}, {"revision": 1}), (1.0, 1.0),
                                 (scenario.I64_MAX + 1, scenario.I64_MAX + 1),
                                 ({"updated_at": "2026-09-10T00:00:00.0000001Z"},
                                  {"updated_at": "2026-09-10T00:00:00.0000001Z"})):
            self.assertFalse(scenario.same_json(actual, expected))
        nested = {}
        for _ in range(42):
            nested = {"child": nested}
        self.assertFalse(scenario.same_json(nested, nested))
        duplicates = {"members": [{"item_id": IDS["root"]}, {"item_id": IDS["root"]}]}
        self.assertFalse(scenario.same_json(duplicates, duplicates))
        malformed = {"members": [{"item_id": []}]}
        self.assertFalse(scenario.same_json(malformed, malformed))
        with self.assertRaises(scenario.ScenarioError):
            scenario.strict_json('{"revision":1,"revision":2}')

    def test_sql_member_normalization_keys_by_instance_and_item(self):
        first = {"instance_id": INSTANCE, "item_id": IDS["root"], "parent_item_id": None, "source_revision": 1}
        second = dict(first, instance_id=SENTINEL)
        valid = {"members": [first, second]}
        self.assertTrue(scenario.semantic_equal(valid, {"members": [second, first]}))
        self.assertEqual(len(scenario.semantic_json(valid)["members"]), 2)
        duplicate_pair = {"members": [first, second, copy.deepcopy(first)]}
        self.assertFalse(scenario.semantic_equal(duplicate_pair, duplicate_pair))
        with self.assertRaises(scenario.ScenarioError):
            scenario.semantic_json(duplicate_pair)
        mixed = {"members": [first, {"item_id": IDS["branch"]}]}
        self.assertFalse(scenario.semantic_equal(mixed, mixed))

    def test_all_complete_aggregates_and_snapshots(self):
        values = lifecycle()
        for revision, value in values.items():
            with self.subTest(revision=revision):
                scenario.assert_aggregate(value, IDS, revision, initial=values[1], instance_id=INSTANCE,
                    occurrence_id=values[1]["manifest"]["occurrence_id"], initial_manifest=values[1]["manifest"])
                scenario.assert_snapshot(snapshot(value), IDS, revision, initial_manifest=values[1]["manifest"])

    def test_aggregate_rejects_missing_inbox_target_mix_and_typed_revisions(self):
        for mutate in (lambda row: row["members"].pop(4), lambda row: row["manifest"]["members"].pop(4),
                       lambda row: row.update(revision=True), lambda row: row["members"][0].update(revision=True),
                       lambda row: row["manifest"].update(series_item_id=IDS["branch"]),
                       lambda row: row["manifest"]["members"][0].update(required_for_parent=1),
                       lambda row: row.update(unreviewed=True)):
            broken = aggregate()
            mutate(broken)
            with self.assertRaises(scenario.ScenarioError):
                scenario.assert_aggregate(broken, IDS, 1)
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_aggregate(aggregate(), IDS, 1, instance_id=SENTINEL)
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_aggregate(aggregate(), IDS, 1, occurrence_id=aggregate(SENTINEL, 12)["manifest"]["occurrence_id"])

    def test_exact_blocked_reopen_and_unchanged_optional_state(self):
        initial, final = lifecycle()[1], lifecycle()[6]
        by_member(final, "blocked")["open"]["blocked_reason"] = "A guessed replacement"
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_aggregate(final, IDS, 6, initial=initial)
        final = lifecycle()[6]
        by_member(final, "inbox")["updated_at"] = stamp(99)
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_aggregate(final, IDS, 6, initial=initial)

    def test_snapshot_checks_full_counts_and_typed_control(self):
        for mutate in (lambda row: row.update(fresh_edit_eligible=1),
                       lambda row: row["members"][0]["counts"].update(required_descendants=True),
                       lambda row: row["members"].pop(4), lambda row: row["members"][0].update(reason=[])):
            broken = snapshot(aggregate())
            mutate(broken)
            with self.assertRaises(scenario.ScenarioError):
                scenario.assert_snapshot(broken, IDS, 1)

    def test_sentinel_is_entire_aggregate_not_just_status(self):
        initial = aggregate(SENTINEL, 12)
        scenario.assert_sentinel(copy.deepcopy(initial), initial, IDS)
        changed = copy.deepcopy(initial)
        by_member(changed, "optional")["updated_at"] = stamp(1)
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_sentinel(changed, initial, IDS)

    def test_sql_is_single_scoped_read_only_statement(self):
        sql = scenario.final_sql(uid(801))
        self.assertTrue(sql.startswith("SELECT jsonb_build_object("))
        self.assertNotIn(";", sql)
        self.assertNotIn("UPDATE", sql)
        self.assertNotIn("DELETE", sql)
        self.assertIn("workspace_id='" + uid(801) + "'::uuid", sql)
        for name in ("routine_occurrences", "routine_occurrence_members", "routine_occurrence_state",
                     "routine_occurrence_changes", "routine_occurrence_operations", "routine_occurrence_publications",
                     "item_changes", "item_completion_state", "schedule_revision_details", "schedule_publication_requests"):
            self.assertIn(name, sql)
        for value in ("' OR true --", uid(801) + "; DELETE FROM items", str(uuid.UUID(int=0)), True):
            with self.subTest(value=value), self.assertRaises(scenario.ScenarioError):
                scenario.final_sql(value)

    def test_full_sql_custody_and_additional_untouched_admissions_pass(self):
        self.audit(*sql_fixture())
        self.audit(*sql_fixture(extra=True))

    def test_sql_requires_full_shape_and_exact_known_initial_targets(self):
        self.reject_sql(lambda final, *_: final.pop("item_changes"))
        self.reject_sql(lambda final, *_: final.update(unrelated=True))
        final, baseline, saved, original = sql_fixture()
        with self.assertRaises(scenario.ScenarioError):
            scenario.assert_final_sql(final, IDS, baseline, saved, original,
                                      instance_id=EXTRA, sentinel_id=SENTINEL)

    def test_sql_rejects_stale_b_receipt_or_missing_successful_receipt(self):
        def stale_b(final, baseline, saved, original):
            extra = copy.deepcopy(final["operations"][0])
            extra.update(operation_id=saved["B"]["operation_id"], request=copy.deepcopy(saved["B"]))
            final["operations"].append(extra)
        self.reject_sql(stale_b)
        self.reject_sql(lambda final, *_: final["operations"].pop())

    def test_sql_rejects_rebased_or_wrong_command_and_wrong_target(self):
        self.reject_sql(lambda final, baseline, saved, original: saved["B"].update(expected_instance_revision=2))
        self.reject_sql(lambda final, baseline, saved, original: saved["F"]["action"]["open"].update(status="planned"))
        self.reject_sql(lambda final, baseline, saved, original: saved["C"]["action"].update(required_for_parent=1))
        self.reject_sql(lambda final, *_: final["operations"][0]["request"].update(expected_evidence_hash="sha256:" + "b" * 64))
        self.reject_sql(lambda final, *_: final["operations"][0].update(member_item_id=IDS["optional"]))
        self.reject_sql(lambda final, *_: final["operations"][0].update(instance_id=SENTINEL))

    def test_sql_preserves_canonical_history_completion_and_unrelated_tables(self):
        self.reject_sql(lambda final, *_: final["items"][0].update(revision=2))
        self.reject_sql(lambda final, *_: final["item_changes"][0]["payload"].update(title="Synthetic drift"))
        self.reject_sql(lambda final, *_: final["completion_states"][0].update(required_for_parent=True))
        self.reject_sql(lambda final, *_: final["untouched"].update(execution_sessions=1))

    def test_sql_manifest_source_and_first_publication_are_immutable(self):
        self.reject_sql(lambda final, *_: final["manifests"][0].update(first_schedule_revision_id=FRESH_PUBLICATION))
        self.reject_sql(lambda final, *_: final["members"].pop(4))
        self.reject_sql(lambda final, *_: final["members"][0].update(source_revision=True))
        self.reject_sql(lambda final, *_: final["members"][0].update(parent_item_id=IDS["optional"]))
        self.reject_sql(lambda final, *_: final["publications"][0]["source_revisions"].pop(IDS["inbox"]))
        self.reject_sql(lambda final, *_: final["schedule_requests"][0].update(request_hash="d" * 64))

    def test_sql_checks_change_chain_effects_sentinel_and_exact_a_receipt(self):
        self.reject_sql(lambda final, *_: final["changes"].pop(3))
        self.reject_sql(lambda final, *_: final["changes"][2].update(revision=True))
        self.reject_sql(lambda final, *_: final["changes"][2]["effects"].pop())
        self.reject_sql(lambda final, *_: final["changes"][3]["before"].update(revision=1))
        self.reject_sql(lambda final, *_: by_member(final["states"][1]["aggregate"], "inbox").update(updated_at=stamp(99)))
        self.reject_sql(lambda final, baseline, saved, original: original["aggregate"]["members"][0].update(updated_at=stamp(99)))
        self.reject_sql(lambda final, *_: final["operations"][0].update(result=snapshot(lifecycle()[6])))
        self.reject_sql(lambda final, *_: final["operations"][0].update(actor_session_id=None))

    def test_sql_requires_new_v6_publication_with_terminal_ledger_head(self):
        self.reject_sql(lambda final, *_: final["schedule_revisions"][1].update(occurrence_head=6))
        self.reject_sql(lambda final, *_: final["schedule_revisions"][1].update(occurrence_head=True))
        self.reject_sql(lambda final, *_: final["schedule_revisions"][1].update(schema="5"))
        self.reject_sql(lambda final, *_: final["schedule_revisions"][1].update(publication_schema="dayweave-scheduler-publication/5"))
        self.reject_sql(lambda final, *_: final["schedule_revisions"][0].update(input_digest="sha256:" + "e" * 64))


if __name__ == "__main__":
    unittest.main()
