#!/usr/bin/env python3
"""Pure controls for the opt-in host helper/codec gate; no native build is run."""
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, call, patch

SPEC = importlib.util.spec_from_file_location("routine_helper_gate",
    Path(__file__).with_name("test-native-routine-planning-helper.py"))
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class RoutineNativeHelperHarnessTests(unittest.TestCase):
    def test_timeout_stops_exact_owned_group_before_reaping_leader(self):
        process = Mock(pid=8123, returncode=None)
        process.wait.side_effect = [subprocess.TimeoutExpired("synthetic", 1800), 0]
        with tempfile.TemporaryDirectory() as directory, patch.object(gate.subprocess, "Popen", return_value=process) as spawn, \
                patch.object(gate.os, "killpg") as kill, patch.object(gate.time, "sleep"):
            with self.assertRaises(subprocess.TimeoutExpired):
                gate.run("timeout", ["synthetic-no-execution"], {}, Path(directory))
            self.assertTrue(spawn.call_args.kwargs["start_new_session"])
            self.assertEqual(kill.call_args_list, [call(8123, signal.SIGTERM), call(8123, signal.SIGKILL)])
            self.assertEqual(process.wait.call_args_list, [call(timeout=1800), call(timeout=10)])
            process.poll.assert_not_called()

    def test_cleanup_never_signals_a_reaped_process_identity(self):
        with patch.object(gate.os, "killpg") as kill, patch.object(gate.time, "sleep") as sleep:
            process = Mock(pid=8123, returncode=0)
            gate.stop_owned_group(process)
            kill.assert_not_called(); sleep.assert_not_called(); process.wait.assert_not_called()

    def test_interruption_runs_owned_cleanup_and_restores_signal_handlers(self):
        process = Mock(pid=8123, returncode=None)
        process.wait.side_effect = [gate.GateInterrupted("synthetic interruption"), 0]
        original = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
        with tempfile.TemporaryDirectory() as directory, patch.object(gate.subprocess, "Popen", return_value=process), \
                patch.object(gate.os, "killpg") as kill, patch.object(gate.time, "sleep"):
            with self.assertRaises(gate.GateInterrupted):
                gate.run("interrupted", ["synthetic-no-execution"], {}, Path(directory))
            self.assertEqual(kill.call_args_list, [call(8123, signal.SIGTERM), call(8123, signal.SIGKILL)])
        self.assertEqual(original, {number: signal.getsignal(number) for number in original})

    def test_environment_never_inherits_owner_or_injected_runtime_controls(self):
        hostile = dict(PATH="/synthetic/bin", HOME="/synthetic/user", CARGO_INCREMENTAL="1",
            JAVA_TOOL_OPTIONS="-javaagent:/synthetic/agent", DYLD_INSERT_LIBRARIES="/synthetic/library",
            RUSTC_WRAPPER="/synthetic/wrapper", RUSTFLAGS="--cfg synthetic", GRADLE_OPTS="synthetic",
            DAYWEAVE_ROUTINE_NATIVE_BRIDGE_TEST="1", DAYWEAVE_ROUTINE_NATIVE_HELPER_TEST="1",
            DAYWEAVE_DATABASE_TEST_URL="synthetic-private", DAYWEAVE_ANDROID_SIGNING_PROPERTIES="synthetic-private")
        with patch.dict(os.environ, hostile, clear=True):
            self.assertEqual(gate.environment(None, None),
                dict(PATH="/synthetic/bin", HOME="/synthetic/user", CARGO_INCREMENTAL="0"))

    def test_toolchain_directories_are_explicit_and_existing(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "not-a-directory"; file.write_text("synthetic")
            with self.assertRaises(ValueError): gate.environment(str(file), None)
            value = gate.environment(directory, directory)
            self.assertEqual(value["JAVA_HOME"], str(Path(directory).resolve()))
            self.assertEqual(value["ANDROID_HOME"], str(Path(directory).resolve()))

    def test_artifacts_require_safe_owned_single_link_regular_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); file = root / "synthetic-helper"
            file.write_text("synthetic"); file.chmod(0o500)
            self.assertEqual(gate.require_artifact(file, executable=True), file.resolve())
            link = root / "symlink"; link.symlink_to(file)
            with self.assertRaises(ValueError): gate.require_artifact(link, executable=True)
            alias = root / "hardlink"; os.link(file, alias)
            with self.assertRaises(ValueError): gate.require_artifact(file, executable=True)
            alias.unlink(); file.chmod(0o700 | 0o020)
            with self.assertRaises(ValueError): gate.require_artifact(file, executable=True)
            file.chmod(0o400)
            with self.assertRaises(ValueError): gate.require_artifact(file, executable=True)
            self.assertEqual(gate.require_artifact(file, executable=False), file.resolve())

    def test_mac_requires_one_executed_passing_test_without_skips_or_issues(self):
        value = f'✔ Test "{gate.MAC_TEST_TITLE}" passed after 0.5 seconds.\n✔ Test run with 1 test in 1 suite passed after 0.5 seconds.'
        gate.verify_mac_report(value)
        for changed in ("", value.replace("1 test", "0 tests"), value.replace("1 suite", "2 suites"),
                        value.replace(gate.MAC_TEST_TITLE, "foreign test"),
                        value + "\n✔ Test run with 1 test in 1 suite passed after 0.5 seconds.",
                        value + "\n➜ Test synthetic skipped.", value + "\n✘ Test synthetic failed"):
            with self.subTest(changed=changed), self.assertRaises(RuntimeError): gate.verify_mac_report(changed)

    def test_android_requires_exact_named_executed_test(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.xml"
            baseline = dict(name=gate.ANDROID_TEST, tests="1", skipped="0", failures="0", errors="0")
            def write(values, *, case=True, method=gate.ANDROID_METHOD, error=None):
                suite = gate.ET.Element("testsuite", values)
                if case:
                    entry = gate.ET.SubElement(suite, "testcase", dict(classname=gate.ANDROID_TEST, name=method))
                    if error: gate.ET.SubElement(entry, error)
                gate.ET.ElementTree(suite).write(report)
            write(baseline); gate.verify_android_report(report)
            for key, changed in (("name", "ForeignTest"), ("tests", "0"), ("tests", "2"),
                                 ("skipped", "1"), ("failures", "1"), ("errors", "1")):
                write(dict(baseline, **{key: changed}))
                with self.subTest(key=key), self.assertRaises(RuntimeError): gate.verify_android_report(report)
            for changed in (dict(case=False), dict(method="foreign"), dict(error="failure"), dict(error="skipped")):
                write(baseline, **changed)
                with self.subTest(changed=changed), self.assertRaises(RuntimeError): gate.verify_android_report(report)

    def test_previous_report_is_archived_and_cannot_satisfy_new_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); logs = root / "logs"; logs.mkdir()
            report = root / "report.xml"; report.write_text("prior generated report")
            gate.archive_previous_android_report(report, logs)
            self.assertFalse(report.exists())
            self.assertEqual((logs / "previous-android-report.xml").read_text(), "prior generated report")
            with self.assertRaises(FileNotFoundError): gate.verify_android_report(report)
            report.symlink_to(logs / "previous-android-report.xml")
            with self.assertRaises(ValueError): gate.archive_previous_android_report(report, logs)


if __name__ == "__main__":
    unittest.main()
