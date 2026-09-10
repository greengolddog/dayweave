#!/usr/bin/env python3
"""Inert driver admission checks; no services, native builds or owner credentials."""
import copy
import importlib.util
from pathlib import Path
import unittest
import uuid
from unittest import mock


spec = importlib.util.spec_from_file_location(
    "native_routine_gate", Path(__file__).with_name("test-native-routine-occurrence-convergence.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class NativeRoutineHarnessTests(unittest.TestCase):
    def setUp(self):
        self.items = [{"id": str(uuid.UUID(int=1)), "revision": 1, "status": "planned"}]
        self.aggregate = {"revision": 1, "updated_at": "2026-09-10T10:00:00.123456Z"}
        self.sentinel = {"revision": 1, "updated_at": "2026-09-10T10:00:00Z"}
        self.marker = dict(schema_version=1, run_id="synthetic-run", phase="prepare", status="passed",
            pending_count=1, submitted_count=0, receipt_target_count=0,
            needs_remote_schedule_catch_up=False, has_pending_publication=False,
            terminal_cursor="opaque-terminal", items=self.items, occurrence=self.aggregate,
            sentinel=self.sentinel, publication_operation_id=str(uuid.UUID(int=2)),
            publication_revision_id=str(uuid.UUID(int=3)))

    def validate(self, marker, **overrides):
        options = dict(run_id="synthetic-run", phase="prepare", pending=1, submitted=0,
            targets=0, catch_up=False, publication_pending=False, expected_items=self.items,
            expected_occurrence=self.aggregate, expected_sentinel=self.sentinel)
        options.update(overrides)
        gate.validate_marker(marker, **options)

    def test_exact_full_checkpoint_and_timestamp_equivalence(self):
        self.validate(copy.deepcopy(self.marker))
        value = copy.deepcopy(self.marker)
        value["occurrence"]["updated_at"] = "2026-09-10T10:00:00.123456+00:00"
        value["sentinel"]["updated_at"] = "2026-09-10T10:00:00.000000Z"
        self.validate(value)

    def test_closed_marker_shape_and_run_phase_identity(self):
        for field, value in (("schema_version", True), ("schema_version", 1.0), ("run_id", "foreign"),
                             ("phase", "verify"), ("status", "running"), ("future", None)):
            marker = copy.deepcopy(self.marker); marker[field] = value
            with self.subTest(field=field), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)
        for field in self.marker:
            marker = copy.deepcopy(self.marker); del marker[field]
            with self.subTest(missing=field), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)

    def test_custody_counts_are_exact_integers(self):
        for field in ("pending_count", "submitted_count", "receipt_target_count"):
            for value in (True, False, 1.0, "1", -1, 100):
                marker = copy.deepcopy(self.marker); marker[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(gate.support.GateFailure):
                    self.validate(marker)

    def test_recovery_latches_are_exact_booleans(self):
        for field in ("needs_remote_schedule_catch_up", "has_pending_publication"):
            for value in (None, 0, "false", True):
                marker = copy.deepcopy(self.marker); marker[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(gate.support.GateFailure):
                    self.validate(marker)

    def test_terminal_cursor_is_required_and_bounded(self):
        for value in (None, "", 1, [], "x" * 4097):
            marker = copy.deepcopy(self.marker); marker["terminal_cursor"] = value
            with self.subTest(value_type=type(value)), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)

    def test_templates_must_match_exactly_with_typed_revisions(self):
        for value in (True, 1.0, 2):
            marker = copy.deepcopy(self.marker); marker["items"][0]["revision"] = value
            with self.subTest(value=value), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)

    def test_full_occurrence_and_sentinel_cannot_drift(self):
        for field in ("occurrence", "sentinel"):
            for value in (None, {}, {"revision": 1}, dict(self.aggregate, revision=True),
                          dict(self.aggregate, revision=2), dict(self.aggregate, future=False)):
                marker = copy.deepcopy(self.marker); marker[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(gate.support.GateFailure):
                    self.validate(marker)

    def test_publication_identity_is_canonical_not_a_digest_or_boolean(self):
        for field in ("publication_operation_id", "publication_revision_id"):
            for value in (True, "", "sha256:" + "a" * 64, str(uuid.UUID(int=0)),
                          "AAAAAAAA-AAAA-4AAA-AAAA-AAAAAAAAAAAA"):
                marker = copy.deepcopy(self.marker); marker[field] = value
                with self.subTest(field=field), self.assertRaises((gate.support.GateFailure, gate.scenario.ScenarioError)):
                    self.validate(marker)

    def test_pending_publication_requires_its_operation_and_recovered_state_requires_proof(self):
        marker = copy.deepcopy(self.marker)
        marker.update(has_pending_publication=True, needs_remote_schedule_catch_up=True,
                      publication_operation_id=None)
        with self.assertRaises(gate.support.GateFailure):
            self.validate(marker, publication_pending=True, catch_up=True)
        marker = copy.deepcopy(self.marker); marker["publication_revision_id"] = None
        with self.assertRaises(gate.support.GateFailure):
            self.validate(marker)

    def test_native_phase_commands_are_narrow_and_force_execution(self):
        for client, phases in gate.PHASES.items():
            for phase in phases:
                command = gate.native_command(client, phase)
                prebuild = gate.native_command(client, phase, prebuild=True)
                if client == "macos":
                    self.assertEqual(command[-2:], ["--filter", "NativeRoutineOccurrenceConvergenceTests"])
                    self.assertIn("-warnings-as-errors", command)
                    self.assertEqual([part for part in command if part != "--skip-build"], prebuild)
                else:
                    self.assertEqual(command[-2:], ["--tests", "*.NativeRoutineOccurrenceConvergenceTest"])
                    self.assertIn("--no-daemon", command)
                    self.assertIn("--no-configuration-cache", command)
                    self.assertEqual(command.count("--rerun"), 1)
                    self.assertEqual(prebuild.count("--rerun-tasks"), 1)
                    self.assertEqual([part for part in command if part != "--rerun"],
                                     [part for part in prebuild if part != "--rerun-tasks"])
        for client, phase in (("android", "prepare"), ("macos", "finish"), ("foreign", "verify")):
            with self.subTest(client=client, phase=phase), self.assertRaises(gate.support.GateFailure):
                gate.native_command(client, phase)

    def preview(self):
        return {"plan": {"occurrences": [
            {"id": str(uuid.uuid5(uuid.NAMESPACE_URL, f"synthetic/{index}")),
             "series_item_id": str(uuid.UUID(int=1)), "nominal_start": f"2026-09-{11+index}T00:00:00Z"}
            for index in range(2)]}}

    def test_identities_are_extracted_in_nominal_order_not_fabricated(self):
        preview = self.preview()
        expected = tuple(row["id"] for row in preview["plan"]["occurrences"])
        preview["plan"]["occurrences"].reverse()
        self.assertEqual(gate.published_identities(preview, str(uuid.UUID(int=1))), expected)

    def test_missing_duplicate_wrong_root_and_non_v5_identities_are_rejected(self):
        malformed = [None, {}, {"plan": {}}, {"plan": {"occurrences": []}}]
        for change in ("duplicate", "wrong-root", "v4", "missing-second", "overflow"):
            preview = self.preview(); rows = preview["plan"]["occurrences"]
            if change == "duplicate": rows[1]["id"] = rows[0]["id"]
            if change == "wrong-root": rows[0]["series_item_id"] = str(uuid.UUID(int=2))
            if change == "v4": rows[0]["id"] = str(uuid.uuid4())
            if change == "missing-second": rows.pop()
            if change == "overflow": rows.extend(copy.deepcopy(rows[0]) for _ in range(100))
            malformed.append(preview)
        for value in malformed:
            with self.subTest(value=value), self.assertRaises(gate.support.GateFailure):
                gate.published_identities(value, str(uuid.UUID(int=1)))

    def test_existing_target_arguments_reject_before_any_filesystem_or_service_work(self):
        with mock.patch.object(gate.sys, "platform", "darwin"), \
                mock.patch.object(gate.sys, "argv", ["driver", "https://owner.invalid"]), \
                mock.patch.object(gate.tempfile, "mkdtemp") as directory, \
                mock.patch.object(gate.subprocess, "Popen") as process, \
                self.assertRaises(gate.support.GateFailure):
            gate.main()
        directory.assert_not_called()
        process.assert_not_called()

    def test_primitives_reuse_already_tested_owned_cleanup_and_environment(self):
        with mock.patch.dict(gate.os.environ, {"PATH": "/synthetic/bin", "HOME": "/synthetic/home",
                "DAYWEAVE_NATIVE_ROUTINE_CONFIG": "/owner/config", "DAYWEAVE_GOOGLE_OAUTH_ENABLED": "true",
                "DAYWEAVE_DATABASE_URL": "owner-database", "HTTPS_PROXY": "owner-proxy"}, clear=True):
            self.assertEqual(gate.support.build_environment(), {"PATH": "/synthetic/bin", "HOME": "/synthetic/home"})
        self.assertIs(gate.completion.support, gate.support)

    def test_postgres_json_timezone_and_credential_files_are_explicit(self):
        original = {"PATH": "/synthetic/bin", "PGTZ": "Europe/Moscow", "PGPASSFILE": "/owner/passwords",
                    "PGSERVICEFILE": "/owner/services"}
        self.assertEqual(gate.postgres_environment(original), {"PATH": "/synthetic/bin", "PGTZ": "UTC",
                         "PGPASSFILE": "/dev/null", "PGSERVICEFILE": "/dev/null"})
        self.assertEqual(original["PGTZ"], "Europe/Moscow")

    def publication_evidence(self):
        return {"schedule_revisions": [{"id": "old"}, {"id": "fresh"}],
                "schedule_requests": [{"idempotency_key": "old-operation", "schedule_revision_id": "old"},
                                      {"idempotency_key": "fresh-operation", "schedule_revision_id": "fresh"}]}

    def test_settled_native_proof_is_linked_to_its_real_sql_request(self):
        marker = dict(publication_operation_id="fresh-operation", publication_revision_id="fresh", has_pending_publication=False)
        gate.validate_publication_custody([marker], self.publication_evidence())
        for changes in ({"publication_revision_id": "random"}, {"publication_revision_id": "old"},
                        {"publication_operation_id": "random"}, {"publication_revision_id": None}):
            with self.subTest(changes=changes), self.assertRaises(gate.support.GateFailure):
                gate.validate_publication_custody([dict(marker, **changes)], self.publication_evidence())

    def test_lost_reply_can_retain_old_proof_but_not_fabricated_request(self):
        marker = dict(publication_operation_id="fresh-operation", publication_revision_id="old", has_pending_publication=True)
        gate.validate_publication_custody([marker], self.publication_evidence())
        for changes in ({"publication_operation_id": None}, {"publication_operation_id": "random"},
                        {"publication_revision_id": "random"}):
            with self.subTest(changes=changes), self.assertRaises(gate.support.GateFailure):
                gate.validate_publication_custody([dict(marker, **changes)], self.publication_evidence())

    def test_no_publication_call_can_retain_a_real_older_proof(self):
        marker = dict(publication_operation_id=None, publication_revision_id="old", has_pending_publication=False)
        gate.validate_publication_custody([marker], self.publication_evidence())

    def test_sql_publication_mapping_rejects_duplicate_keys(self):
        for table in ("schedule_revisions", "schedule_requests"):
            evidence = self.publication_evidence(); evidence[table].append(copy.deepcopy(evidence[table][0]))
            with self.subTest(table=table), self.assertRaises(gate.support.GateFailure):
                gate.validate_publication_custody([], evidence)

    def current_publication_fixture(self):
        digest = "sha256:" + "a" * 64
        evidence = {"schedule_revisions": [
            dict(id="old", revision_number=1, input_digest=digest, schema="6",
                 publication_schema="dayweave-scheduler-publication/6", occurrence_head=2),
            dict(id="fresh", revision_number=2, input_digest=digest, schema="6",
                 publication_schema="dayweave-scheduler-publication/6", occurrence_head=9)],
            "changes": [{"sequence": 2}, {"sequence": 9}]}
        published = dict(revision=dict(id="fresh", revision_number=2, input_digest=digest),
            schedule=dict(input_digest=digest, plan=dict(blocks=[dict(item_id="optional", occurrence_id="instance")])))
        marker = dict(publication_revision_id="fresh")
        ids = {key: key for key in ("optional", "required", "inbox", "blocked")}
        return published, evidence, marker, ids

    def test_current_publication_links_final_native_sql_and_terminal_head(self):
        gate.validate_current_publication(*self.current_publication_fixture(), "instance")

    def test_same_blocks_do_not_allow_stale_current_revision_or_native_proof(self):
        for defect in ("reader", "native", "both", "head", "schema"):
            published, evidence, marker, ids = self.current_publication_fixture()
            if defect in ("reader", "both"):
                published["revision"].update(id="old", revision_number=1)
            if defect in ("native", "both"):
                marker["publication_revision_id"] = "old"
            if defect == "head":
                evidence["schedule_revisions"][-1]["occurrence_head"] = 2
            if defect == "schema":
                evidence["schedule_revisions"][-1]["schema"] = "5"
            with self.subTest(defect=defect), self.assertRaises(gate.support.GateFailure):
                gate.validate_current_publication(published, evidence, marker, ids, "instance")

    def test_current_revision_number_and_digest_must_match_sql(self):
        for defect in ("boolean", "number", "revision-digest", "schedule-digest"):
            published, evidence, marker, ids = self.current_publication_fixture()
            if defect in ("boolean", "number"):
                published["revision"]["revision_number"] = True if defect == "boolean" else 1
            else:
                published["revision" if defect == "revision-digest" else "schedule"]["input_digest"] = "different"
            with self.subTest(defect=defect), self.assertRaises(gate.support.GateFailure):
                gate.validate_current_publication(published, evidence, marker, ids, "instance")

    def test_current_schedule_requires_optional_work_and_excludes_completed_inbox_blocked(self):
        for defect in ("missing", "required", "inbox", "blocked", "foreign-instance"):
            published, evidence, marker, ids = self.current_publication_fixture()
            blocks = published["schedule"]["plan"]["blocks"]
            if defect == "missing":
                blocks.clear()
            elif defect == "foreign-instance":
                blocks[0]["occurrence_id"] = "other"
            else:
                blocks.append(dict(item_id=defect, occurrence_id="instance"))
            with self.subTest(defect=defect), self.assertRaises(gate.support.GateFailure):
                gate.validate_current_publication(published, evidence, marker, ids, "instance")


if __name__ == "__main__":
    unittest.main()
