#!/usr/bin/env python3
"""Explicit synthetic macOS-process / host-JNI routine display integration.

Builds host Rust artifacts and runs real native adapters against the committed
producer corpus. No application service, owner account, device, keychain, deployment or
provider is contacted. macOS signature admission is a test double; production
signing remains separately gated. Android runs host JNI, not an Android device.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import platform
import signal
import stat
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET


REPO = Path(__file__).resolve().parent.parent
ANDROID_TEST = "com.greengolddog.dayweave.scheduler.RoutinePlanningNativeBridgeTest"
MAC_TEST = "RoutinePlanningNativeHelperTests"
MAC_TEST_TITLE = "actual Rust process and native codec retain every qualified producer plan"
ANDROID_METHOD = "actualJniAndNativeCodecRetainEveryQualifiedProducerPlan"


class GateInterrupted(RuntimeError):
    pass


def interrupt_gate(number, _frame) -> None:
    raise GateInterrupted(f"Native helper gate interrupted by signal {number}")


def stop_owned_group(process) -> None:
    if process.returncode is not None:
        return  # Never signal an already reaped/reusable process identity.
    # Do not poll/reap the group leader between TERM and KILL: its retained PID
    # keeps this new session's process-group identity from being reused. Killing
    # only cargo/bash/gradlew would leave compiler/test descendants running.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    time.sleep(0.25)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=10)


def environment(java_home: str | None, android_home: str | None) -> dict[str, str]:
    # Never inherit owner integration switches, remote Cargo wrappers, signing
    # controls, JVM agents/options or Gradle opt-ins. The two test flags below
    # are set only by this explicit runner after the host build succeeds.
    result = {key: os.environ[key] for key in (
        "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR", "LANG"
    ) if key in os.environ}
    result["CARGO_INCREMENTAL"] = "0"
    for key, raw in (("JAVA_HOME", java_home), ("ANDROID_HOME", android_home)):
        if raw is not None:
            directory = Path(raw).resolve(strict=True)
            if not directory.is_dir():
                raise ValueError("A supplied toolchain location is not a directory")
            result[key] = str(directory)
    return result


def require_artifact(path: Path, *, executable: bool) -> Path:
    information = path.lstat()
    if (not stat.S_ISREG(information.st_mode) or information.st_nlink != 1
            or information.st_uid != os.getuid() or information.st_mode & 0o022
            or (executable and not information.st_mode & 0o111)):
        raise ValueError("The host build artifact is not a safe owned regular file")
    return path.resolve(strict=True)


def run(name: str, command: list[str], env: dict[str, str], logs: Path) -> str:
    log_path = logs / (name + ".log")
    print(f"Running {name}; private log: {log_path}", flush=True)
    with log_path.open("xb") as log:
        process = subprocess.Popen(command, cwd=REPO, env=env, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        previous = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
        try:
            for number in previous:
                signal.signal(number, interrupt_gate)
            code = process.wait(timeout=1800)
        except BaseException:
            # A second interruption must not bypass owned descendant cleanup.
            for number in previous:
                signal.signal(number, signal.SIG_IGN)
            stop_owned_group(process)
            raise
        finally:
            for number, handler in previous.items():
                signal.signal(number, handler)
    if code:
        raise RuntimeError(f"{name} failed; inspect its private log")
    return log_path.read_text(errors="replace")


def verify_android_report(path: Path) -> None:
    require_artifact(path, executable=False)
    suite = ET.parse(path).getroot()
    cases = suite.findall("testcase")
    if (suite.get("name") != ANDROID_TEST or suite.get("tests") != "1"
            or any(suite.get(key) != "0" for key in ("skipped", "failures", "errors"))
            or len(cases) != 1 or cases[0].get("classname") != ANDROID_TEST
            or cases[0].get("name") != ANDROID_METHOD
            or any(suite.findall(".//" + tag) for tag in ("skipped", "failure", "error"))):
        raise RuntimeError("Host JNI test did not execute successfully")


def archive_previous_android_report(path: Path, logs: Path) -> None:
    # Do not let an old passing XML satisfy this invocation. Preserve that
    # exact generated report privately instead of deleting broader test output.
    if path.exists() or path.is_symlink():
        require_artifact(path, executable=False)
        destination = logs / "previous-android-report.xml"
        if destination.exists() or destination.is_symlink():
            raise ValueError("Previous-report archive is already occupied")
        path.rename(destination)


def verify_mac_report(output: str) -> None:
    summaries = [line for line in output.splitlines() if "Test run with" in line]
    if (len(summaries) != 1 or "Test run with 1 test in 1 suite passed" not in summaries[0]
            or f'✔ Test "{MAC_TEST_TITLE}" passed' not in output
            or " skipped." in output or "✘" in output):
        raise RuntimeError("Real macOS helper test did not execute successfully")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="Explicitly opt into host builds and native tests")
    parser.add_argument("--client", choices=("both", "macos", "android"), default="both")
    parser.add_argument("--java-home", help="JDK directory for Android host tests")
    parser.add_argument("--android-home", help="Android SDK directory for Gradle configuration")
    args = parser.parse_args()
    if not args.run:
        parser.error("--run is required; this gate does not run implicitly")
    if platform.system() != "Darwin":
        parser.error("This host-native gate currently requires macOS")
    os.umask(0o077)
    logs = Path(tempfile.mkdtemp(prefix="dayweave-routine-native-helper-"))
    try:
        env = environment(args.java_home, args.android_home)
        run("host-build", ["cargo", "build", "--locked", "--offline",
            "-p", "dayweave-scheduler-helper", "-p", "dayweave-android-ffi"], env, logs)
        helper = require_artifact(REPO / "target/debug/dayweave-scheduler-helper", executable=True)
        library = require_artifact(REPO / "target/debug/libdayweave_android_ffi.dylib", executable=False)
        if args.client in ("both", "macos"):
            mac_env = dict(env, DAYWEAVE_ROUTINE_NATIVE_HELPER_TEST="1",
                           DAYWEAVE_ROUTINE_NATIVE_HELPER_PATH=str(helper))
            output = run("macos", [str(REPO / "scripts/test-macos.sh"),
                "-Xswiftc", "-warnings-as-errors", "--filter", MAC_TEST], mac_env, logs)
            verify_mac_report(output)
        if args.client in ("both", "android"):
            # JAVA_TOOL_OPTIONS is intentionally generated here, not inherited.
            # Quote the value so a workspace with spaces remains one JVM option.
            directory = str(library.parent)
            if any(character in directory for character in ('"', "\n", "\r")):
                raise ValueError("Host library directory cannot be represented safely")
            android_env = dict(env, DAYWEAVE_ROUTINE_NATIVE_BRIDGE_TEST="1",
                               JAVA_TOOL_OPTIONS=f'-Djava.library.path="{directory}"')
            report = REPO / "apps/android/app/build/test-results/testDebugUnitTest" / f"TEST-{ANDROID_TEST}.xml"
            archive_previous_android_report(report, logs)
            run("android", [str(REPO / "apps/android/gradlew"), "--project-dir",
                str(REPO / "apps/android"), "--no-daemon", "--no-configuration-cache",
                "--offline", ":app:testDebugUnitTest", "--tests", ANDROID_TEST, "--rerun"], android_env, logs)
            verify_android_report(report)
        print(f"PASS: {args.client} host helper/codec/display integration; private logs: {logs}")
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired, ET.ParseError) as error:
        print(f"FAIL: {error}; private logs retained: {logs}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
