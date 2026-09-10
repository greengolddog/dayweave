#!/usr/bin/env python3
"""Network-free safety and evidence checks for the completion gate.

The reused process/HTTP/environment primitives are additionally covered by
test-native-progress-harness.py. This suite starts no service or native build.
"""
import copy
import importlib.util
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "native_completion_gate", Path(__file__).with_name("test-native-completion-convergence.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class NativeCompletionHarnessTests(unittest.TestCase):
    def setUp(self):
        self.items = [{"id": "synthetic-item", "revision": 1, "status": "planned"}]
        self.snapshot = {"state": {"revision": 1, "required_for_parent": True}, "counts": {"completed": 0}}
        self.marker = dict(schema_version=1, run_id="synthetic-run", phase="prepare", status="passed",
                           pending_count=1, needs_canonical_catch_up=False,
                           items=self.items, root_completion=self.snapshot)

    def validate(self, marker):
        gate.validate_marker(marker, run_id="synthetic-run", phase="prepare", pending=1, catch_up=False,
                             expected_items=self.items, expected_completion=self.snapshot)

    def test_exact_checkpoint_is_accepted(self):
        self.validate(copy.deepcopy(self.marker))

    def test_foreign_extra_missing_and_incomplete_markers_are_rejected(self):
        for field, value in (("schema_version", True), ("run_id", "foreign"), ("phase", "replay"),
                             ("status", "running"), ("pending_count", True), ("pending_count", 0),
                             ("needs_canonical_catch_up", 0), ("needs_canonical_catch_up", True), ("extra", None)):
            marker = copy.deepcopy(self.marker); marker[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)
        for field in self.marker:
            marker = copy.deepcopy(self.marker); del marker[field]
            with self.subTest(missing=field), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)

    def test_stale_optimistic_or_wrongly_typed_canonical_evidence_is_rejected(self):
        for field, value in (("revision", 2), ("revision", True), ("revision", 1.0), ("status", "completed")):
            marker = copy.deepcopy(self.marker); marker["items"][0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)

    def test_historical_missing_and_wrongly_typed_completion_evidence_is_rejected(self):
        for value in (None, {}, {"state": {"revision": True, "required_for_parent": True}, "counts": {"completed": 0}},
                      {"state": {"revision": 1, "required_for_parent": 1}, "counts": {"completed": 0}}):
            marker = copy.deepcopy(self.marker); marker["root_completion"] = value
            with self.subTest(value=value), self.assertRaises(gate.support.GateFailure):
                self.validate(marker)

    def test_native_commands_use_only_selected_phase_test_and_force_android_execution(self):
        mac = gate.native_command("macos", "prepare")
        self.assertEqual(mac[-2:], ["--filter", "NativeCompletionConvergenceTests"])
        self.assertIn("-warnings-as-errors", mac)
        self.assertIn("--skip-build", mac)
        mac_prebuild = gate.native_command("macos", "prepare", prebuild=True)
        self.assertEqual([arg for arg in mac if arg != "--skip-build"], mac_prebuild)
        for phase in gate.PHASES["macos"]:
            self.assertEqual(gate.native_command("macos", phase), mac)
            self.assertEqual(gate.native_command("macos", phase, prebuild=True), mac_prebuild)
        android = gate.native_command("android", "prepare_offline")
        self.assertNotIn("--rerun-tasks", android)
        self.assertEqual(android.count("--rerun"), 1)
        self.assertEqual(android[android.index(":app:testDebugUnitTest") + 1], "--rerun")
        self.assertIn("--no-configuration-cache", android)
        self.assertIn("--no-daemon", android)
        self.assertEqual(android[-2:], ["--tests", "*.NativeCompletionConvergenceTest"])
        android_prebuild = gate.native_command("android", "prepare_offline", prebuild=True)
        self.assertEqual(android_prebuild.count("--rerun-tasks"), 1)
        self.assertNotIn("--rerun", android_prebuild)
        self.assertEqual([arg for arg in android if arg != "--rerun"],
                         [arg for arg in android_prebuild if arg != "--rerun-tasks"])
        for phase in gate.PHASES["android"]:
            self.assertEqual(gate.native_command("android", phase), android)
            self.assertEqual(gate.native_command("android", phase, prebuild=True), android_prebuild)
        for client, phase in (("android", "prepare"), ("macos", "prepare_offline"), ("foreign", "prepare")):
            for prebuild in (False, True):
                with self.subTest(client=client, phase=phase, prebuild=prebuild), self.assertRaises(gate.support.GateFailure):
                    gate.native_command(client, phase, prebuild=prebuild)

    def runtime_directory(self):
        temporary = tempfile.TemporaryDirectory(prefix="dayweave-testing-frameworks.", dir="/tmp")
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name)
        path.chmod(0o700)
        return path

    def runtime_log(self, content):
        temporary = tempfile.TemporaryDirectory(prefix="dayweave-completion-harness-test-", dir="/tmp")
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / "prebuild.log"
        with path.open("x") as stream:
            stream.write(content)
        path.chmod(0o600)
        return path

    def test_runtime_admission_requires_exact_generated_namespace_owner_and_private_directory(self):
        runtime = self.runtime_directory()
        info = runtime.lstat()
        self.assertEqual(gate.runtime_identity(runtime), (runtime, info.st_dev, info.st_ino))
        resolved = runtime.resolve()
        self.assertEqual(gate.runtime_identity(resolved), (resolved, info.st_dev, info.st_ino))
        invalid = (Path(runtime.name), runtime / runtime.name, runtime.parent,
                   runtime.with_name("dayweave-testing-frameworks.a"),
                   runtime.with_name("dayweave-testing-frameworks." + "a" * 33),
                   runtime.with_name("dayweave-testing-frameworks.abcdef-"),
                   runtime.with_name("foreign-" + runtime.name))
        for path in invalid:
            with self.subTest(path=path), self.assertRaises(gate.support.GateFailure):
                gate.runtime_identity(path)
        for mode in (0o755, 0o750, 0o600, 0o1700):
            runtime.chmod(mode)
            with self.subTest(mode=oct(mode)), self.assertRaises(gate.support.GateFailure):
                gate.runtime_identity(runtime)
        runtime.chmod(0o700)
        with mock.patch.object(gate.os, "getuid", return_value=info.st_uid + 1), \
                self.assertRaises(gate.support.GateFailure):
            gate.runtime_identity(runtime)

    def test_runtime_admission_rejects_top_level_symlink_and_regular_file(self):
        runtime, external = self.runtime_directory(), self.runtime_directory()
        runtime.rmdir()
        runtime.symlink_to(external, target_is_directory=True)
        self.addCleanup(lambda: runtime.unlink(missing_ok=True))
        with self.assertRaises(gate.support.GateFailure):
            gate.runtime_identity(runtime)
        runtime.unlink()
        with runtime.open("x") as stream:
            stream.write("synthetic non-directory")
        runtime.chmod(0o700)
        with self.assertRaises(gate.support.GateFailure):
            gate.runtime_identity(runtime)
        self.assertTrue(external.is_dir())

    def test_runtime_receipt_requires_one_exact_system_or_admitted_directory_line(self):
        runtime = self.runtime_directory()
        receipt = f"DAYWEAVE_TESTING_RUNTIME={runtime}\n"
        self.assertEqual(gate.retained_runtime(self.runtime_log("synthetic build output\n" + receipt)),
                         gate.runtime_identity(runtime))
        self.assertIsNone(gate.retained_runtime(self.runtime_log("DAYWEAVE_TESTING_RUNTIME=system\n")))
        invalid = ("", receipt + receipt, "DAYWEAVE_TESTING_RUNTIME=system\n" + receipt,
                   "DAYWEAVE_TESTING_RUNTIME=system\n" * 2, "DAYWEAVE_TESTING_RUNTIME=\n",
                   "DAYWEAVE_TESTING_RUNTIME=system \n", " " + receipt,
                   "DAYWEAVE_TESTING_RUNTIME=" + str(runtime / runtime.name) + "\n")
        for content in invalid:
            with self.subTest(content=content), self.assertRaises(gate.support.GateFailure):
                gate.retained_runtime(self.runtime_log(content))
        oversized = self.runtime_log(receipt)
        with oversized.open("r+b") as stream:
            stream.truncate(16 * 1024 * 1024 + 1)
        with self.assertRaises(gate.support.GateFailure):
            gate.retained_runtime(oversized)

    def test_runtime_cleanup_checks_exact_device_inode_and_preserves_mismatches(self):
        runtime = self.runtime_directory()
        evidence = runtime / "synthetic-evidence"
        evidence.write_bytes(b"retained until exact identity is admitted")
        identity = gate.runtime_identity(runtime)
        for changed in ((runtime, identity[1] + 1, identity[2]), (runtime, identity[1], identity[2] + 1)):
            with self.subTest(identity=changed), mock.patch.object(gate.shutil, "rmtree") as remove, \
                    self.assertRaises(gate.support.GateFailure):
                gate.remove_retained_runtime(changed)
            remove.assert_not_called()
            self.assertEqual(evidence.read_bytes(), b"retained until exact identity is admitted")
        with mock.patch.object(gate.shutil, "rmtree") as remove:
            gate.remove_retained_runtime(None)
        remove.assert_not_called()
        gate.remove_retained_runtime(identity)
        self.assertFalse(runtime.exists())
        self.assertFalse(runtime.is_symlink())

    def test_runtime_cleanup_removes_owned_read_only_tree_without_touching_external_symlink_targets(self):
        runtime, external = self.runtime_directory(), self.runtime_directory()
        external_file = external / "synthetic-external-content"
        external_file.write_bytes(b"must remain unchanged")
        external_file.chmod(0o400)
        external.chmod(0o500)
        external_info, file_info = external.stat(), external_file.stat()
        framework = runtime / "Synthetic.framework"
        framework.mkdir(mode=0o700)
        (framework / "external-directory").symlink_to(external, target_is_directory=True)
        (framework / "external-file").symlink_to(external_file)
        (framework / "owned-content").write_bytes(b"owned synthetic framework")
        framework.chmod(0o500)
        gate.remove_retained_runtime(gate.runtime_identity(runtime))
        self.assertFalse(runtime.exists())
        self.assertEqual(external_file.read_bytes(), b"must remain unchanged")
        for path, before in ((external, external_info), (external_file, file_info)):
            after = path.stat()
            self.assertEqual((after.st_dev, after.st_ino, stat.S_IMODE(after.st_mode)),
                             (before.st_dev, before.st_ino, stat.S_IMODE(before.st_mode)))

    def test_interrupted_runtime_handoff_recovers_only_exact_successful_receipt(self):
        runtime = self.runtime_directory()
        log = self.runtime_log(f"synthetic build completed\nDAYWEAVE_TESTING_RUNTIME={runtime}\n")
        gate.finish_runtime_cleanup(None, log=log, handoff_started=True, handoff_complete=False)
        self.assertFalse(runtime.exists())
        self.assertFalse(runtime.is_symlink())

    def test_interrupted_runtime_handoff_rejects_missing_or_malformed_receipt_without_removal(self):
        runtime = self.runtime_directory()
        for content in ("synthetic interrupted output\n", "DAYWEAVE_TESTING_RUNTIME=invalid\n",
                        f"DAYWEAVE_TESTING_RUNTIME={runtime}\nDAYWEAVE_TESTING_RUNTIME=system\n"):
            log = self.runtime_log(content)
            with self.subTest(content=content), mock.patch.object(gate, "remove_retained_runtime") as remove, \
                    self.assertRaises(gate.support.GateFailure):
                gate.finish_runtime_cleanup(None, log=log, handoff_started=True, handoff_complete=False)
            remove.assert_not_called()
            self.assertTrue(runtime.is_dir())
        missing = self.runtime_log("")
        missing.unlink()
        with mock.patch.object(gate, "remove_retained_runtime") as remove, self.assertRaises(FileNotFoundError):
            gate.finish_runtime_cleanup(None, log=missing, handoff_started=True, handoff_complete=False)
        remove.assert_not_called()

    def test_interrupted_system_runtime_handoff_never_removes_a_directory(self):
        log = self.runtime_log("DAYWEAVE_TESTING_RUNTIME=system\n")
        with mock.patch.object(gate.shutil, "rmtree") as remove:
            gate.finish_runtime_cleanup(None, log=log, handoff_started=True, handoff_complete=False)
        remove.assert_not_called()

    def test_completed_or_unstarted_handoff_does_not_read_another_receipt(self):
        runtime = self.runtime_directory()
        log = self.runtime_log("invalid log must not supersede admitted identity")
        with mock.patch.object(gate, "retained_runtime") as read:
            gate.finish_runtime_cleanup(gate.runtime_identity(runtime), log=log,
                                        handoff_started=True, handoff_complete=True)
            gate.finish_runtime_cleanup(None, log=log, handoff_started=False, handoff_complete=False)
        read.assert_not_called()
        self.assertFalse(runtime.exists())

    def test_external_targets_are_rejected_before_creating_artifacts_or_starting_processes(self):
        with mock.patch.object(sys, "platform", "darwin"), mock.patch.object(sys, "argv", ["gate", "--database=foreign"]), \
                mock.patch.object(gate.tempfile, "mkdtemp") as create, mock.patch.object(gate.subprocess, "Popen") as start:
            with self.assertRaises(gate.support.GateFailure):
                gate.main()
            create.assert_not_called(); start.assert_not_called()

    def test_private_json_rejects_permissions_symlinks_duplicate_keys_and_oversized_files(self):
        with tempfile.TemporaryDirectory(prefix="dayweave-completion-harness-test-") as directory:
            path = Path(directory) / "marker.json"
            gate.support.private_json(path, self.marker)
            self.assertEqual(gate.read_private_json(path), self.marker)
            path.chmod(0o644)
            with self.assertRaises(gate.support.GateFailure):
                gate.read_private_json(path)
            path.chmod(0o600)
            link = Path(directory) / "link.json"; link.symlink_to(path)
            with self.assertRaises(gate.support.GateFailure):
                gate.read_private_json(link)
            duplicate = Path(directory) / "duplicate.json"
            with duplicate.open("xb") as stream:
                stream.write(b'{"revision":1,"revision":2}')
            duplicate.chmod(0o600)
            with self.assertRaises(gate.scenario.ScenarioError):
                gate.read_private_json(duplicate)
            oversized = Path(directory) / "oversized.json"
            with oversized.open("xb") as stream:
                stream.truncate(1024 * 1024 + 1)
            oversized.chmod(0o600)
            with self.assertRaises(gate.support.GateFailure):
                gate.read_private_json(oversized)

    def test_delta_requires_terminal_evidence_and_a_bounded_page_count(self):
        api = mock.Mock()
        api.call.return_value = {"changes": [], "next_cursor": "opaque", "has_more": True}
        with self.assertRaises(gate.support.GateFailure):
            gate.capture_current(api)
        self.assertEqual(api.call.call_count, 100)
        self.assertEqual(api.call.call_args_list[0], mock.call("v1/items/delta?limit=200"))
        api.call.return_value = {"changes": [], "next_cursor": "opaque", "has_more": 0}
        with self.assertRaises(gate.support.GateFailure):
            gate.capture_current(api)

    def test_scoped_session_is_obtained_through_enrollment_and_never_fabricated(self):
        identities = ["enrollment", "instance", "session"]
        bootstrap = mock.Mock(base_url="http://127.0.0.1:54321/")
        bootstrap.call.return_value = dict(id="enrollment", replayed=False, client_contract_version=2)
        scopes = ["items_read", "items_write", "schedule_read", "schedule_simulate", "schedule_publish"]
        consumer = mock.Mock()
        consumer.call.return_value = dict(replayed=False, session=dict(id="session", client_instance_id="instance",
            client_kind="macos", scopes=scopes, access_expires_at="synthetic-expiry"))
        with mock.patch.object(gate.uuid, "uuid4", side_effect=identities), \
                mock.patch.object(gate.support, "LocalAPI", side_effect=[consumer, "scoped-api"]) as factory:
            api, metadata = gate.enroll_synthetic_session(bootstrap, "synthetic-access")
        self.assertEqual(api, "scoped-api")
        self.assertEqual(bootstrap.call.call_args.args[:2], ("v1/auth/device-enrollments", "POST"))
        self.assertEqual(bootstrap.call.call_args.args[2]["scopes"], scopes)
        self.assertEqual(consumer.call.call_args.args[:2], ("v1/auth/device-enrollments/consume", "POST"))
        self.assertEqual(consumer.call.call_args.args[2]["access_token"], "synthetic-access")
        self.assertEqual(factory.call_args_list[-1], mock.call(bootstrap.base_url, "synthetic-access"))
        self.assertEqual(set(metadata), {"session_id", "client_instance_id", "scopes", "access_expires_at", "shared_test_principal"})

    def test_enrollment_replay_or_changed_scope_cannot_be_accepted(self):
        bootstrap = mock.Mock(base_url="http://127.0.0.1:54321/")
        bootstrap.call.return_value = dict(id="enrollment", replayed=True, client_contract_version=2)
        with mock.patch.object(gate.uuid, "uuid4", side_effect=["enrollment", "instance", "session"]), \
                mock.patch.object(gate.support, "LocalAPI") as factory, self.assertRaises(gate.support.GateFailure):
            gate.enroll_synthetic_session(bootstrap, "synthetic-access")
        factory.assert_not_called()
        bootstrap.call.return_value["replayed"] = False
        consumer = mock.Mock()
        consumer.call.return_value = dict(replayed=False, session=dict(id="session", client_instance_id="instance",
            client_kind="macos", scopes=["items_read"], access_expires_at="synthetic-expiry"))
        with mock.patch.object(gate.uuid, "uuid4", side_effect=["enrollment", "instance", "session"]), \
                mock.patch.object(gate.support, "LocalAPI", return_value=consumer), self.assertRaises(gate.support.GateFailure):
            gate.enroll_synthetic_session(bootstrap, "synthetic-access")


if __name__ == "__main__":
    unittest.main()
