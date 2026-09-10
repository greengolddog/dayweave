#!/usr/bin/env python3
"""Real native completion/reopening convergence on an owned disposable service.

No target arguments, existing database, owner application profile or integration
credentials are accepted. Runtime artifacts remain private and outside Git.
"""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlencode
import uuid

import native_completion_scenario as scenario


_spec = importlib.util.spec_from_file_location(
    "completion_gate_process_support", Path(__file__).with_name("test-native-progress-convergence.py"))
support = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(support)
REPO = Path(__file__).resolve().parent.parent
PHASES = {
    "macos": {"prepare", "submit_lost", "replay", "verify_cascade", "verify_reopen"},
    "android": {"prepare_offline", "conflict_keep_open", "catchup_automatic_optional", "verify_cascade_and_child"},
}
MARKER_KEYS = {"schema_version", "run_id", "phase", "status", "pending_count",
               "needs_canonical_catch_up", "items", "root_completion"}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise support.GateFailure(message)


def native_command(client: str, phase: str, *, prebuild: bool = False) -> list[str]:
    require(client in PHASES and phase in PHASES[client], "Unsupported native completion phase")
    if client == "macos":
        return [str(REPO / "scripts/test-macos.sh"), "-Xswiftc", "-warnings-as-errors",
                *([] if prebuild else ["--skip-build"]),
                "--filter", "NativeCompletionConvergenceTests"]
    return [str(REPO / "apps/android/gradlew"), "--project-dir", str(REPO / "apps/android"),
            "--no-daemon", "--no-configuration-cache",
            *(["--rerun-tasks"] if prebuild else []), ":app:testDebugUnitTest",
            *([] if prebuild else ["--rerun"]),
            "--tests", "*.NativeCompletionConvergenceTest"]


def runtime_identity(path: Path) -> tuple[Path, int, int]:
    """Admit only a private, generated top-level SDK copy, never an SDK itself."""
    require(path.is_absolute() and path.parent in {Path("/tmp"), Path("/tmp").resolve()}
            and re.fullmatch(r"dayweave-testing-frameworks\.[A-Za-z0-9_]{6,32}", path.name) is not None,
            "Unexpected retained macOS runtime location")
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid()
            and stat.S_IMODE(info.st_mode) == 0o700,
            "Unsafe retained macOS runtime directory")
    return path, info.st_dev, info.st_ino


def retained_runtime(log: Path) -> tuple[Path, int, int] | None:
    require(log.stat().st_size <= 16 * 1024 * 1024, "Oversized macOS prebuild log")
    prefix = "DAYWEAVE_TESTING_RUNTIME="
    receipts = [line[len(prefix):] for line in log.read_text().splitlines() if line.startswith(prefix)]
    require(len(receipts) == 1, "Missing or ambiguous macOS runtime retention receipt")
    return None if receipts[0] == "system" else runtime_identity(Path(receipts[0]))


def remove_retained_runtime(identity: tuple[Path, int, int] | None) -> None:
    if identity is None:
        return
    path = identity[0]
    require(runtime_identity(path) == identity, "Retained macOS runtime identity changed before cleanup")
    # Only directory permissions matter for unlinking contents. Do not follow
    # framework symlinks or chmod their targets (which could be outside the copy).
    for directory, _, _ in os.walk(path, followlinks=False):
        current = Path(directory)
        if not current.is_symlink():
            current.chmod(stat.S_IMODE(current.stat().st_mode) | 0o700)
    shutil.rmtree(path)
    require(not path.exists() and not path.is_symlink(), "Retained macOS runtime cleanup did not finish")


def finish_runtime_cleanup(identity: tuple[Path, int, int] | None, *, log: Path,
                           handoff_started: bool, handoff_complete: bool) -> None:
    # Interruption may occur after the wrapper wrote its successful receipt but
    # before run_logged returned or the driver admitted the identity. Recover
    # that exact handoff from our private log; never scan for runtime directories.
    if handoff_started and not handoff_complete:
        identity = retained_runtime(log)
    remove_retained_runtime(identity)


def read_private_json(path: Path) -> object:
    info = path.lstat()
    require(path.is_file() and not path.is_symlink() and info.st_uid == os.getuid()
            and info.st_mode & 0o777 == 0o600 and 0 < info.st_size <= 1024 * 1024,
            "Unsafe or oversized native completion artifact")
    return scenario.strict_json(path.read_bytes())


def validate_marker(marker: object, *, run_id: str, phase: str, pending: int,
                    catch_up: bool, expected_items: list[dict], expected_completion: dict | None) -> None:
    require(type(marker) is dict and set(marker) == MARKER_KEYS, "Invalid native completion result shape")
    require(type(marker["schema_version"]) is int and marker["schema_version"] == 1
            and marker["run_id"] == run_id and marker["phase"] == phase and marker["status"] == "passed",
            "Foreign or incomplete native completion result")
    require(type(marker["pending_count"]) is int and marker["pending_count"] == pending
            and type(marker["needs_canonical_catch_up"]) is bool and marker["needs_canonical_catch_up"] == catch_up,
            "Native completion custody or catch-up fence differs")
    require(same_json(marker["items"], expected_items), "Native canonical projection differs from authoritative evidence")
    require(same_json(marker["root_completion"], expected_completion), "Native completion evidence differs from its expected checkpoint")


def same_json(actual: object, expected: object) -> bool:
    """JSON evidence is typed: true and 1 are not interchangeable revisions."""
    if type(actual) is not type(expected):
        return False
    if isinstance(actual, dict):
        return actual.keys() == expected.keys() and all(same_json(actual[key], expected[key]) for key in actual)
    if isinstance(actual, list):
        return len(actual) == len(expected) and all(same_json(a, b) for a, b in zip(actual, expected))
    return actual == expected


def capture_current(api: support.LocalAPI) -> scenario.DeltaProjection:
    pages, cursor = [], None
    for _ in range(100):
        # Ordinary delta requests cap at 200; atomic change groups may expand
        # the response up to the separately enforced 300-member wire bound.
        query = {"limit": 200}
        if cursor is not None:
            query["cursor"] = cursor
        page = api.call("v1/items/delta?" + urlencode(query))
        require(type(page) is dict, "Malformed service delta checkpoint")
        pages.append(page)
        if page.get("has_more") is False:
            return scenario.fold_delta_pages(pages)
        require(page.get("has_more") is True, "Service delta lacks terminal evidence")
        cursor = page.get("next_cursor")
        require(isinstance(cursor, str) and cursor, "Service delta lacks a continuation cursor")
    raise support.GateFailure("Service delta checkpoint exceeded its bounded page budget")


def completion(api: support.LocalAPI, item_id: str) -> dict:
    value = api.call(f"v1/items/{scenario.canonical_uuid(item_id)}/completion")
    require(type(value) is dict, "Missing service completion snapshot")
    return value


def assert_root(snapshot: dict, ids: dict[str, str], *, item_revision: int, policy_revision: int,
                mode: str, required: int, completed: int, provenance: str | None) -> None:
    require(set(snapshot) == {"schema_version", "item_id", "item_revision", "state", "evidence_hash",
                              "counts", "occurrence_evidence_required"}, "Unexpected root completion envelope")
    state = snapshot["state"]
    require(snapshot["schema_version"] == 1 and snapshot["item_id"] == ids["root"]
            and snapshot["item_revision"] == item_revision and snapshot["occurrence_evidence_required"] is False,
            "Root completion identity, revision or occurrence boundary differs")
    require(state["item_id"] == ids["root"] and state["revision"] == policy_revision
            and state["mode"] == mode and state["required_for_parent"] is True,
            "Root completion policy differs from the reviewed scenario")
    expected_provenance = None if provenance is None else {
        "kind": provenance,
        "reopen": {"status": "blocked", "blocked_reason_kind": "manual",
                   "blocked_by_item_id": None, "blocked_reason": scenario.BLOCKED_REASON},
    }
    require(state["provenance"] == expected_provenance, "Root did not retain its exact reopening custody")
    require(snapshot["counts"] == {"required_descendants": required, "completed": completed,
                                   "incomplete": required - completed, "occurrence_evidence_required": 0},
            "Optional/required descendant counts differ")


def enroll_synthetic_session(bootstrap: support.LocalAPI, access_token: str) -> tuple[support.LocalAPI, dict]:
    """Use the real enrollment API; never fabricate tenant scope or insert auth rows.

    One temporary device principal is shared by the two independent test stores.
    This verifies store convergence, not per-device credential lifecycle behavior.
    """
    enrollment_id, instance_id, session_id = (str(uuid.uuid4()) for _ in range(3))
    enrollment_token = "dw_en1_" + secrets.token_urlsafe(32)
    refresh_token = "dw_dr1_" + secrets.token_urlsafe(32)
    scopes = ["items_read", "items_write", "schedule_read", "schedule_simulate", "schedule_publish"]
    enrolled = bootstrap.call("v1/auth/device-enrollments", "POST", {
        "id": enrollment_id, "enrollment_token": enrollment_token, "client_instance_id": instance_id,
        "client_kind": "macos", "device_label": "Synthetic completion convergence",
        "scopes": scopes, "client_contract_version": 2, "client_version": "synthetic-completion-1",
        "client_capabilities": [],
    })
    require(enrolled.get("id") == enrollment_id and enrolled.get("replayed") is False
            and enrolled.get("client_contract_version") == 2,
            "Synthetic device enrollment did not create the expected contract")
    consumed = support.LocalAPI(bootstrap.base_url, enrollment_token).call(
        "v1/auth/device-enrollments/consume", "POST",
        {"session_id": session_id, "access_token": access_token, "refresh_token": refresh_token})
    session = consumed.get("session", {})
    require(consumed.get("replayed") is False and session.get("id") == session_id
            and session.get("client_instance_id") == instance_id and session.get("client_kind") == "macos"
            and sorted(session.get("scopes", [])) == sorted(scopes),
            "Synthetic device consumption did not retain the exact requested scope")
    return support.LocalAPI(bootstrap.base_url, access_token), {
        "session_id": session_id, "client_instance_id": instance_id, "scopes": scopes,
        "access_expires_at": session["access_expires_at"], "shared_test_principal": True,
    }


def main() -> int:
    require(sys.platform == "darwin", "This cross-client gate requires macOS")
    require(len(sys.argv) == 1, "Run without arguments; existing service/database targets are not accepted")
    java = os.environ.get("JAVA_HOME", "")
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT", "")
    require(bool(java) and (Path(java) / "bin/java").is_file() and bool(sdk) and Path(sdk).is_dir(),
            "Set JAVA_HOME and ANDROID_HOME to the supported installed native build tools")
    tools = {name: shutil.which(name) for name in ("initdb", "pg_ctl", "psql", "cargo")}
    require(all(tools.values()), "Install local PostgreSQL tools and Rust before running this gate")
    require(Path("/usr/bin/caffeinate").is_file(), "The macOS run-scoped idle-sleep guard is unavailable")
    os.umask(0o077)
    signal.signal(signal.SIGTERM, support.interrupted)
    root = Path(tempfile.mkdtemp(prefix="dayweave-native-completion.", dir="/tmp")).resolve()
    root.chmod(0o700)
    print(f"Synthetic native completion artifacts: {root}", flush=True)
    pg_data = root / "postgres"
    pg_port, api_port = support.loopback_port(), support.loopback_port()
    while pg_port == api_port:
        api_port = support.loopback_port()
    bootstrap_token = "native-completion-bootstrap-" + secrets.token_urlsafe(32)
    token = "dw_da1_" + secrets.token_urlsafe(32)
    user_id, workspace_id, run_id = (str(uuid.uuid4()) for _ in range(3))
    ids = {key: str(uuid.uuid4()) for key in sorted(scenario.ID_KEYS)}
    db_url = f"postgres://dayweave_completion_test@127.0.0.1:{pg_port}/postgres"
    env = support.build_environment()
    pg_env = dict(env, PGPASSFILE="/dev/null", PGSERVICEFILE="/dev/null")
    service_env = dict(support.service_environment(bootstrap_token, db_url, api_port, user_id, workspace_id),
                       DAYWEAVE_AUTH_MODE="hybrid")
    base_url = f"http://127.0.0.1:{api_port}/"
    api = support.LocalAPI(base_url, bootstrap_token)
    config = {"schema_version": 1, "run_id": run_id, "base_url": base_url, "bearer_token": token,
              "work_directory": str(root), "root_id": ids["root"], "branch_id": ids["branch"],
              "required_leaf_id": ids["required"], "optional_leaf_id": ids["optional"], "new_child_id": ids["new_child"]}
    config_path = root / "config.json"
    support.private_json(config_path, config)
    server, idle_guard, pg_started = None, None, False
    runtime = None
    runtime_handoff_started, runtime_handoff_complete = False, False
    mac_build_log = root / "macos-prebuild.log"
    server_logs = []
    report = {"schema_version": 1, "run_id": run_id, "status": "failed", "phases": []}

    def start_service(label: str):
        nonlocal server
        log = (root / f"api-{label}.log").open("xb")
        server_logs.append(log)
        server = subprocess.Popen([str(REPO / "target/debug/dayweave-api")], cwd=root, env=service_env,
                                  stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            require(server.poll() is None, f"Owned API stopped during {label}; inspect its private log")
            try:
                api.call("readyz")
                api.call("v1/items/delta")  # Authenticated fresh-token listener proof.
                return
            except support.GateFailure:
                time.sleep(0.2)
        raise support.GateFailure(f"Owned API did not become ready during {label}")

    def phase(client: str, name: str, pending: int, catch_up: bool,
              *, expected_items=None, expected_completion=None, capture_after: bool = False):
        print(f"Running {client}/{name}", flush=True)
        phase_env = dict(env, DAYWEAVE_NATIVE_COMPLETION_CONFIG=str(config_path), DAYWEAVE_NATIVE_COMPLETION_PHASE=name)
        support.run_logged(native_command(client, name),
                           root / f"{client}-{name}.log", phase_env)
        if capture_after:
            expected_items = scenario.projection(capture_current(api).items)
            expected_completion = completion(api, ids["root"])
        marker = read_private_json(root / client / f"{name}.json")
        validate_marker(marker, run_id=run_id, phase=name, pending=pending, catch_up=catch_up,
                        expected_items=expected_items, expected_completion=expected_completion)
        report["phases"].append({"client": client, "phase": name, "status": "passed"})
        print(f"Passed {client}/{name}", flush=True)

    try:
        # Preserve a real, short-lived scoped session across native phases. This
        # assertion prevents idle system sleep only, not display sleep/locking,
        # and expires with this driver even if ordinary cleanup is interrupted.
        idle_guard = subprocess.Popen(["/usr/bin/caffeinate", "-i", "-w", str(os.getpid())],
            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True)
        print("Building owned local API", flush=True)
        support.run_logged([tools["cargo"], "build", "--locked", "-p", "dayweave-api"], root / "api-build.log", env)
        # Compile before enrollment; the real device access token lasts only
        # fifteen minutes. Neither prebuild receives the opt-in phase/config.
        print("Prebuilding macOS completion tests", flush=True)
        runtime_handoff_started = True
        support.run_logged(native_command("macos", "prepare", prebuild=True), mac_build_log,
                           dict(env, DAYWEAVE_TESTING_RETAIN_RUNTIME="1"))
        runtime = retained_runtime(mac_build_log)
        runtime_handoff_complete = True
        print("Prebuilding Android completion tests", flush=True)
        support.run_logged(native_command("android", "prepare_offline", prebuild=True),
                           root / "android-prebuild.log", env)
        support.run_logged([tools["initdb"], "-D", str(pg_data), "-U", "dayweave_completion_test", "--no-locale",
                            "--encoding=UTF8", "--auth-local=trust", "--auth-host=trust"], root / "initdb.log", pg_env)
        pg_started = True  # Even an interrupted start can leave an owned child.
        support.run_logged([tools["pg_ctl"], "-D", str(pg_data), "-l", str(root / "postgres.log"), "-o",
                            f"-h 127.0.0.1 -p {pg_port} -k {root} -c max_locks_per_transaction=512", "-w", "start"],
                           root / "pg-start.log", pg_env)
        start_service("initial")
        api, authentication = enroll_synthetic_session(api, token)
        support.private_json(root / "synthetic-session.json", authentication)
        for request in scenario.seed_requests(ids):
            api.call("v1/items", "POST", request)
        initial = capture_current(api)
        initial_root = completion(api, ids["root"])
        assert_root(initial_root, ids, item_revision=3, policy_revision=0, mode="automatic", required=3, completed=0, provenance=None)
        support.private_json(root / "initial-items.json", initial.items)
        phase("macos", "prepare", 1, False, expected_items=scenario.projection(initial.items), expected_completion=initial_root)
        phase("android", "prepare_offline", 1, False, expected_items=scenario.projection(initial.items), expected_completion=initial_root)
        require(capture_current(api).items == initial.items, "Offline preparation unexpectedly changed server canonical state")
        phase("macos", "submit_lost", 1, False, expected_items=scenario.projection(initial.items), expected_completion=initial_root)
        manual_items, manual_root = capture_current(api), completion(api, ids["root"])
        assert_root(manual_root, ids, item_revision=4, policy_revision=1, mode="complete", required=3, completed=0, provenance="manual")
        support.private_json(root / "original-a-snapshot.json", manual_root)
        # C's receipt is current policy evidence, but its canonical body must
        # remain at A until the separately restarted terminal catch-up phase.
        print("Running android/conflict_keep_open", flush=True)
        support.run_logged(native_command("android", "conflict_keep_open"), root / "android-conflict_keep_open.log",
                           dict(env, DAYWEAVE_NATIVE_COMPLETION_CONFIG=str(config_path), DAYWEAVE_NATIVE_COMPLETION_PHASE="conflict_keep_open"))
        kept_open = completion(api, ids["root"])
        assert_root(kept_open, ids, item_revision=5, policy_revision=2, mode="keep_open", required=3, completed=0, provenance=None)
        validate_marker(read_private_json(root / "android/conflict_keep_open.json"), run_id=run_id,
                        phase="conflict_keep_open", pending=0, catch_up=True,
                        expected_items=scenario.projection(manual_items.items), expected_completion=kept_open)
        report["phases"].append({"client": "android", "phase": "conflict_keep_open", "status": "passed"})
        print("Passed android/conflict_keep_open", flush=True)
        phase("android", "catchup_automatic_optional", 0, False, capture_after=True)
        automatic_items, automatic_root = capture_current(api), completion(api, ids["root"])
        assert_root(automatic_root, ids, item_revision=6, policy_revision=3, mode="automatic", required=2, completed=0, provenance=None)
        optional = completion(api, ids["optional"])
        require(optional["state"]["required_for_parent"] is False and optional["state"]["revision"] == 1,
                "Optional branch was not independently reviewed")
        support.stop_owned_group(server)
        start_service("restarted")
        phase("macos", "replay", 0, False, expected_items=scenario.projection(automatic_items.items), expected_completion=automatic_root)
        after_replay = capture_current(api)
        require(after_replay.items == automatic_items.items and after_replay.cursor == automatic_items.cursor,
                "Historical policy replay changed current canonical state or its terminal cursor")
        api.call(f"v1/items/{ids['required']}", "PUT", scenario.replace_status_request(after_replay.items[ids["required"]], "completed"))
        cascade_items, cascade_root = capture_current(api), completion(api, ids["root"])
        assert_root(cascade_root, ids, item_revision=7, policy_revision=4, mode="automatic", required=2, completed=2, provenance="automatic")
        require(cascade_items.items[ids["root"]]["status"] == "completed"
                and cascade_items.items[ids["branch"]]["status"] == "completed"
                and cascade_items.items[ids["optional"]]["status"] == "planned",
                "Real leaf completion did not respect required and optional branches")
        phase("macos", "verify_cascade", 0, False, expected_items=scenario.projection(cascade_items.items), expected_completion=cascade_root)
        phase("android", "verify_cascade_and_child", 0, False, capture_after=True)
        reopened_items, reopened_root = capture_current(api), completion(api, ids["root"])
        assert_root(reopened_root, ids, item_revision=8, policy_revision=5, mode="automatic", required=3, completed=1, provenance=None)
        require(len(reopened_items.items) == 5 and reopened_items.items[ids["new_child"]]["parent_id"] == ids["branch"]
                and reopened_items.items[ids["new_child"]]["status"] == "planned"
                and reopened_items.items[ids["branch"]]["status"] == "planned"
                and reopened_items.items[ids["root"]]["status"] == "blocked"
                and reopened_items.items[ids["root"]]["blocked_reason"] == scenario.BLOCKED_REASON,
                "Native child creation did not reopen the complete ancestry exactly")
        phase("macos", "verify_reopen", 0, False, expected_items=scenario.projection(reopened_items.items), expected_completion=reopened_root)
        commands = {"A": read_private_json(root / "macos/operation-a.json")}
        commands.update({letter.upper(): read_private_json(root / f"android/operation-{letter}.json")
                         for letter in ("b", "c", "d", "e")})
        check = subprocess.run([tools["psql"], db_url, "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c",
                                scenario.final_sql(workspace_id)], env=pg_env, capture_output=True, timeout=10, check=False)
        with (root / "sql-check.log").open("xb") as log:
            log.write(check.stderr)
        require(check.returncode == 0, "Cannot inspect owned completion SQL custody")
        sql_evidence = scenario.strict_json(check.stdout)
        scenario.assert_final_sql(sql_evidence, ids, manual_root, commands)
        support.private_json(root / "sql-final.json", sql_evidence)
        support.private_json(root / "final-items.json", reopened_items.items)
        report.update(status="passed", immutable_policy_receipts=4, root_completion_revision=5,
                      exact_blocker_reopening=True, optional_branch_preserved=True,
                      historical_receipt_no_rollback=True, durable_catch_up_restart=True, service_restart=True,
                      canonical_parent_create=True, scoped_synthetic_session=True)
    finally:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            support.stop_owned_group(server)
        except (support.GateFailure, OSError, subprocess.TimeoutExpired):
            report["status"] = "failed"
        for log in server_logs:
            log.close()
        if pg_started:
            try:
                with (root / "pg-stop.log").open("xb") as log:
                    state = subprocess.run([tools["pg_ctl"], "-D", str(pg_data), "status"], env=pg_env,
                                           stdout=log, stderr=subprocess.STDOUT, timeout=5, check=False)
                    if state.returncode == 0:
                        subprocess.run([tools["pg_ctl"], "-D", str(pg_data), "-m", "fast", "-w", "-t", "15", "stop"],
                                       env=pg_env, stdout=log, stderr=subprocess.STDOUT, timeout=20, check=False)
                    state = subprocess.run([tools["pg_ctl"], "-D", str(pg_data), "status"], env=pg_env,
                                           stdout=log, stderr=subprocess.STDOUT, timeout=5, check=False)
                report["postgres_stopped"] = state.returncode == 3
            except (OSError, subprocess.TimeoutExpired):
                report["postgres_stopped"] = False
            if not report["postgres_stopped"]:
                report["status"] = "failed"
        report["service_stopped"] = server is None or server.poll() is not None
        try:
            finish_runtime_cleanup(runtime, log=mac_build_log, handoff_started=runtime_handoff_started,
                                   handoff_complete=runtime_handoff_complete)
            report["retained_macos_runtime_removed"] = True
        except (support.GateFailure, OSError):
            report["retained_macos_runtime_removed"] = False
            report["status"] = "failed"
        try:
            support.stop_owned_group(idle_guard)
            report["idle_sleep_guard_stopped"] = True
        except (support.GateFailure, OSError, subprocess.TimeoutExpired):
            report["idle_sleep_guard_stopped"] = False
            report["status"] = "failed"
        support.private_json(root / "report.json", report)
    require(report["status"] == "passed", "Owned native completion cleanup failed; inspect the private report")
    print("Native completion convergence passed; owned services stopped and retained runtime removed", flush=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyboardInterrupt, support.GateInterrupted):
        print("Native completion convergence interrupted; owned cleanup attempted", file=sys.stderr)
        raise SystemExit(130) from None
    except (support.GateFailure, scenario.ScenarioError, OSError, ValueError, KeyError, TypeError) as error:
        message = str(error) if isinstance(error, support.GateFailure) else "Invalid synthetic completion configuration or evidence"
        print(f"Native completion convergence failed: {message}", file=sys.stderr)
        raise SystemExit(1) from None
