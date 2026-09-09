#!/usr/bin/env python3
"""Local safety regressions for the disposable native convergence runner."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import urllib.request


spec = importlib.util.spec_from_file_location(
    "native_progress_gate", Path(__file__).with_name("test-native-progress-convergence.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class NativeProgressHarnessTests(unittest.TestCase):
    def test_build_environment_does_not_forward_provider_signing_or_proxy_settings(self):
        values = {"PATH": "/usr/bin:/bin", "HOME": "/synthetic/build-home", "JAVA_HOME": "/synthetic/java",
                  "DAYWEAVE_API_TOKEN": "synthetic-owner-token", "DAYWEAVE_ANDROID_SIGNING_PROPERTIES": "/synthetic/key",
                  "GOOGLE_APPLICATION_CREDENTIALS": "/synthetic/google", "OPENAI_API_KEY": "synthetic-provider-value",
                  "HTTPS_PROXY": "http://proxy.invalid", "CARGO_TARGET_DIR": "/unrelated/build"}
        with mock.patch.dict(os.environ, values, clear=True):
            self.assertEqual(gate.build_environment(), {name: values[name] for name in ("PATH", "HOME", "JAVA_HOME")})

    def test_service_environment_is_explicit_local_and_provider_disabled(self):
        with mock.patch.dict(os.environ, {"DAYWEAVE_ASSISTANT_ENABLED": "true", "DAYWEAVE_GOOGLE_OAUTH_ENABLED": "true"}):
            env = gate.service_environment("synthetic-test-token", "postgres://127.0.0.1/test", 54321, "user", "workspace")
        self.assertEqual(env["DAYWEAVE_BIND_ADDRESS"], "127.0.0.1:54321")
        self.assertEqual(env["DAYWEAVE_ENVIRONMENT"], "test")
        for key in ("DAYWEAVE_GOOGLE_OAUTH_ENABLED", "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED",
                    "DAYWEAVE_GOOGLE_SCHEDULE_OUTBOUND_ENABLED", "DAYWEAVE_MCP_OAUTH_ENABLED", "DAYWEAVE_ASSISTANT_ENABLED"):
            self.assertEqual(env[key], "false")
        self.assertNotIn("HOME", env)

    def test_redirects_are_rejected_before_following_an_external_target(self):
        request = urllib.request.Request("http://127.0.0.1:54321/v1/items", headers={"Authorization": "Bearer synthetic"})
        with self.assertRaises(gate.GateFailure):
            gate.RejectRedirects().redirect_request(request, None, 302, "redirect", {}, "https://example.invalid/")

    def test_private_artifact_write_does_not_replace_an_existing_file(self):
        with tempfile.TemporaryDirectory(prefix="dayweave-native-harness-test-") as directory:
            path = Path(directory) / "report.json"
            gate.private_json(path, {"status": "synthetic"})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            original = path.read_bytes()
            with self.assertRaises(FileExistsError):
                gate.private_json(path, {"status": "overwritten"})
            self.assertEqual(path.read_bytes(), original)

    def test_phase_timeout_stops_its_owned_process(self):
        processes = []
        actual_popen = subprocess.Popen

        def record(*args, **kwargs):
            process = actual_popen(*args, **kwargs)
            if kwargs.get("start_new_session"):
                processes.append(process)
            return process

        with tempfile.TemporaryDirectory(prefix="dayweave-native-harness-test-") as directory:
            with mock.patch.object(gate.subprocess, "Popen", side_effect=record):
                with self.assertRaises(gate.GateFailure):
                    gate.run_logged([sys.executable, "-c", "import time; time.sleep(30)"],
                                    Path(directory) / "timeout.log", gate.build_environment(), timeout=0.1)
            self.assertEqual(len(processes), 1)
            self.assertIsNotNone(processes[0].poll())

    def test_external_database_or_service_arguments_are_not_accepted(self):
        with mock.patch.object(sys, "platform", "darwin"), mock.patch.object(sys, "argv", ["gate", "https://example.invalid"]):
            with self.assertRaises(gate.GateFailure):
                gate.main()

    def test_signal_handler_enters_normal_failure_cleanup(self):
        with self.assertRaises(gate.GateInterrupted):
            gate.interrupted(15, None)
        self.assertFalse(issubclass(gate.GateInterrupted, gate.GateFailure))

    def test_cleanup_drains_a_descendant_after_the_group_leader_exits(self):
        with tempfile.TemporaryDirectory(prefix="dayweave-native-harness-test-") as directory:
            ready = Path(directory) / "ready"
            child = "import signal,time,pathlib; signal.signal(signal.SIGTERM, signal.SIG_IGN); pathlib.Path(__import__('sys').argv[1]).touch(); time.sleep(30)"
            parent = "import subprocess,sys; subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2]])"
            process = subprocess.Popen([sys.executable, "-c", parent, child, str(ready)], start_new_session=True)
            try:
                process.wait(timeout=5)
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(ready.exists())
                self.assertTrue(gate.owned_group_alive(process))
                gate.stop_owned_group(process, grace_seconds=0.1)
                self.assertFalse(gate.owned_group_alive(process))
            finally:
                gate.stop_owned_group(process, grace_seconds=0.1)


if __name__ == "__main__":
    unittest.main()
