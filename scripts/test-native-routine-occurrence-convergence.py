#!/usr/bin/env python3
"""Opt-in two-native-client occurrence convergence on an owned local service.

Accepts no target arguments or owner configuration. Builds precede synthetic
device enrollment. Runtime credentials, encrypted stores and evidence remain in
one private generated directory outside the repository.
"""
from __future__ import annotations

from datetime import datetime, timezone
import importlib.util
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlencode
import uuid

import native_routine_occurrence_scenario as scenario


_spec = importlib.util.spec_from_file_location(
    "routine_gate_completion_support", Path(__file__).with_name("test-native-completion-convergence.py"))
completion = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(completion)
support = completion.support
REPO = Path(__file__).resolve().parent.parent
PHASES = {
    "macos": {"prepare", "submit_lost", "replay_lost_publication", "recover", "verify"},
    "android": {"prepare_offline", "conflict_keep_open", "finish", "verify"},
}
SERVER_REVISIONS = {"prepare": 1, "prepare_offline": 1, "submit_lost": 2,
                    "conflict_keep_open": 3, "replay_lost_publication": 3,
                    "recover": 3, "finish": 6, "verify": 6}
MARKER_KEYS = {
    "schema_version", "run_id", "phase", "status", "pending_count", "submitted_count",
    "receipt_target_count", "needs_remote_schedule_catch_up", "has_pending_publication",
    "terminal_cursor", "items", "occurrence", "sentinel", "publication_operation_id", "publication_revision_id",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise support.GateFailure(message)


def postgres_environment(environment: dict[str, str]) -> dict[str, str]:
    # SQL's timestamptz JSON must use the same explicit UTC wire convention,
    # independently from the developer machine's session timezone.
    return dict(environment, PGPASSFILE="/dev/null", PGSERVICEFILE="/dev/null", PGTZ="UTC")


def native_command(client: str, phase: str, *, prebuild: bool = False) -> list[str]:
    require(client in PHASES and phase in PHASES[client], "Unsupported native routine phase")
    if client == "macos":
        return [str(REPO / "scripts/test-macos.sh"), "-Xswiftc", "-warnings-as-errors",
                *([] if prebuild else ["--skip-build"]), "--filter", "NativeRoutineOccurrenceConvergenceTests"]
    return [str(REPO / "apps/android/gradlew"), "--project-dir", str(REPO / "apps/android"),
            "--no-daemon", "--no-configuration-cache", *(["--rerun-tasks"] if prebuild else []),
            ":app:testDebugUnitTest", *([] if prebuild else ["--rerun"]),
            "--tests", "*.NativeRoutineOccurrenceConvergenceTest"]


def validate_marker(marker: object, *, run_id: str, phase: str, pending: int, submitted: int,
                    targets: int, catch_up: bool, publication_pending: bool,
                    expected_items: list[dict], expected_occurrence: dict, expected_sentinel: dict) -> None:
    require(type(marker) is dict and set(marker) == MARKER_KEYS, "Invalid native routine result shape")
    require(type(marker["schema_version"]) is int and marker["schema_version"] == 1
            and marker["run_id"] == run_id and marker["phase"] == phase and marker["status"] == "passed",
            "Foreign or incomplete native routine result")
    for field, expected in (("pending_count", pending), ("submitted_count", submitted),
                            ("receipt_target_count", targets)):
        require(type(marker[field]) is int and marker[field] == expected, "Native routine custody differs")
    for field, expected in (("needs_remote_schedule_catch_up", catch_up),
                            ("has_pending_publication", publication_pending)):
        require(type(marker[field]) is bool and marker[field] == expected, "Native routine recovery fence differs")
    require(isinstance(marker["terminal_cursor"], str) and 0 < len(marker["terminal_cursor"]) <= 4096,
            "Missing bounded terminal occurrence cursor")
    require(completion.same_json(marker["items"], expected_items), "Native recurring templates changed")
    require(scenario.semantic_equal(marker["occurrence"], expected_occurrence),
            "Native full occurrence aggregate differs from authoritative checkpoint")
    require(scenario.semantic_equal(marker["sentinel"], expected_sentinel), "Native sentinel occurrence changed")
    for field in ("publication_operation_id", "publication_revision_id"):
        require(marker[field] is None or scenario.canonical_uuid(marker[field]) == marker[field],
                "Invalid native publication identity")
    require(not publication_pending or marker["publication_operation_id"] is not None,
            "Missing exact pending publication identity")
    require(catch_up or marker["publication_revision_id"] is not None,
            "Cleared schedule latch has no durable publication proof")


def occurrence(api, series_id: str, planner_id: str) -> dict:
    value = api.call("v1/routine-occurrences/lookup?" + urlencode({
        "series_item_id": scenario.canonical_uuid(series_id), "occurrence_id": scenario.canonical_uuid(planner_id)}))
    require(type(value) is dict and type(value.get("aggregate")) is dict, "Missing exact service occurrence")
    return value


def validate_publication_custody(markers: list[dict], evidence: dict) -> None:
    """Link each native proof to actual scoped SQL publication custody.

    A lost successful reply has a committed request but may still retain an
    older durable proof. A settled fresh publication must retain its own proof.
    """
    revisions = {row["id"] for row in evidence["schedule_revisions"]}
    requests = {row["idempotency_key"]: row["schedule_revision_id"] for row in evidence["schedule_requests"]}
    require(len(revisions) == len(evidence["schedule_revisions"])
            and len(requests) == len(evidence["schedule_requests"]), "Duplicate SQL publication custody")
    for marker in markers:
        operation, revision = marker["publication_operation_id"], marker["publication_revision_id"]
        require(revision is None or revision in revisions, "Native proof is not an actual scoped publication")
        if operation is not None:
            require(operation in requests, "Native publication request has no immutable service receipt")
            if not marker["has_pending_publication"]:
                require(requests[operation] == revision, "Native publication proof does not match its settled operation")
        else:
            require(not marker["has_pending_publication"], "Pending publication lost its exact operation identity")


def validate_current_publication(published: dict, evidence: dict, final_marker: dict,
                                 ids: dict[str, str], planner_id: str) -> None:
    """The public reader must serve the final proof, not an older similar plan."""
    require(type(published) is dict and type(published.get("revision")) is dict
            and type(published.get("schedule")) is dict, "Missing current native publication")
    revision, schedule = published["revision"], published["schedule"]
    rows = evidence["schedule_revisions"]
    require(bool(rows) and bool(evidence["changes"]), "Missing final publication or occurrence history")
    latest = max(rows, key=lambda row: row["revision_number"])
    require(revision.get("id") == final_marker["publication_revision_id"] == latest["id"],
            "Current publication does not match the final native and SQL proof")
    require(type(revision.get("revision_number")) is int
            and revision["revision_number"] == latest["revision_number"]
            and revision.get("input_digest") == schedule.get("input_digest") == latest["input_digest"],
            "Current publication revision or digest differs from immutable SQL")
    require(latest["schema"] == "6" and latest["publication_schema"] == "dayweave-scheduler-publication/6"
            and type(latest["occurrence_head"]) is int
            and latest["occurrence_head"] >= max(row["sequence"] for row in evidence["changes"]),
            "Current publication does not cover the final occurrence change head")
    require(type(schedule.get("plan")) is dict, "Missing current native plan")
    blocks = schedule["plan"].get("blocks")
    require(type(blocks) is list and all(type(block) is dict for block in blocks),
            "Invalid current native blocks")
    require(any(block.get("item_id") == ids["optional"]
                and block.get("occurrence_id") == planner_id for block in blocks),
            "Completed routine parent removed remaining optional calendar work")
    require(not any(block.get("item_id") in {ids["required"], ids["inbox"], ids["blocked"]}
                    and block.get("occurrence_id") == planner_id for block in blocks),
            "Completed, Inbox or Blocked occurrence member leaked into scheduled work")


def published_identities(preview: dict, root_id: str) -> tuple[str, str]:
    require(type(preview) is dict and type(preview.get("plan")) is dict, "Missing real preview plan")
    rows = preview["plan"].get("occurrences")
    require(type(rows) is list and 2 <= len(rows) <= 100, "Missing bounded published daily occurrences")
    require(all(type(row) is dict and row.get("series_item_id") == root_id for row in rows),
            "Unexpected recurring root in the synthetic plan")
    ordered = sorted(rows, key=lambda row: row["nominal_start"])
    ids = [scenario.canonical_uuid(row["id"]) for row in ordered]
    require(len(set(ids)) == len(ids) and all(uuid.UUID(value).version == 5 for value in ids),
            "Occurrence identities are not unique producer UUID-v5 values")
    return ids[0], ids[1]


def main() -> int:
    require(sys.platform == "darwin", "This cross-client gate requires macOS")
    require(len(sys.argv) == 1, "Run without arguments; existing targets are not accepted")
    java = os.environ.get("JAVA_HOME", "")
    sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT", "")
    require(bool(java) and (Path(java) / "bin/java").is_file() and bool(sdk) and Path(sdk).is_dir(),
            "Set JAVA_HOME and ANDROID_HOME to supported installed build tools")
    executables = {name: shutil.which(name) for name in ("initdb", "pg_ctl", "psql", "cargo")}
    require(all(executables.values()), "Install local PostgreSQL tools and Rust before this gate")
    require(Path("/usr/bin/caffeinate").is_file(), "Run-scoped idle-sleep guard unavailable")
    os.umask(0o077)
    signal.signal(signal.SIGTERM, support.interrupted)
    root = Path(tempfile.mkdtemp(prefix="dayweave-native-routine.", dir="/tmp")).resolve()
    root.chmod(0o700)
    print(f"Synthetic native routine artifacts: {root}", flush=True)
    pg_data = root / "postgres"
    pg_port, api_port = support.loopback_port(), support.loopback_port()
    while pg_port == api_port:
        api_port = support.loopback_port()
    bootstrap_token = "native-routine-bootstrap-" + secrets.token_urlsafe(32)
    access_token = "dw_da1_" + secrets.token_urlsafe(32)
    user_id, workspace_id, run_id = (str(uuid.uuid4()) for _ in range(3))
    ids = {key: str(uuid.uuid4()) for key in sorted(scenario.ID_KEYS)}
    db_url = f"postgres://dayweave_routine_test@127.0.0.1:{pg_port}/postgres"
    env = dict(support.build_environment(), CARGO_INCREMENTAL="0")
    pg_env = postgres_environment(env)
    service_env = dict(support.service_environment(bootstrap_token, db_url, api_port, user_id, workspace_id),
                       DAYWEAVE_AUTH_MODE="hybrid")
    base_url = f"http://127.0.0.1:{api_port}/"
    api = support.LocalAPI(base_url, bootstrap_token)
    config_path = root / "config.json"
    server, idle_guard, pg_started = None, None, False
    runtime = None
    handoff_started, handoff_complete = False, False
    mac_build_log = root / "macos-prebuild.log"
    server_logs = []
    report = {"schema_version": 1, "run_id": run_id, "status": "failed", "phases": []}
    phase_markers = []

    def start_service(label: str):
        nonlocal server
        log = (root / f"api-{label}.log").open("xb")
        server_logs.append(log)
        server = subprocess.Popen([str(REPO / "target/debug/dayweave-api")], cwd=root, env=service_env,
                                  stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            require(server.poll() is None, f"Owned API stopped during {label}; inspect private log")
            try:
                api.call("readyz")
                api.call("v1/items/delta")  # Fresh-token authenticated listener proof.
                return
            except support.GateFailure:
                time.sleep(0.2)
        raise support.GateFailure("Owned API readiness timed out")

    def sql_evidence(label: str) -> dict:
        result = subprocess.run([executables["psql"], db_url, "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1",
                                 "-c", scenario.final_sql(workspace_id)], env=pg_env,
                                capture_output=True, timeout=10, check=False)
        with (root / f"sql-{label}.log").open("xb") as log:
            log.write(result.stderr)
        require(result.returncode == 0, "Cannot inspect owned routine SQL custody")
        require(len(result.stdout) <= 32 * 1024 * 1024, "Oversized routine SQL evidence")
        value = scenario.strict_json(result.stdout)
        require(type(value) is dict, "Missing routine SQL evidence object")
        support.private_json(root / f"sql-{label}.json", value)
        return value

    def phase(client: str, name: str, *, expected: dict | None = None,
              pending: int = 0, submitted: int = 0, targets: int = 0,
              catch_up: bool = False, publication_pending: bool = False):
        print(f"Running {client}/{name}", flush=True)
        support.run_logged(native_command(client, name), root / f"{client}-{name}.log",
                           dict(env, DAYWEAVE_NATIVE_ROUTINE_CONFIG=str(config_path), DAYWEAVE_NATIVE_ROUTINE_PHASE=name))
        current = occurrence(api, ids["root"], planner_id)
        sentinel = occurrence(api, ids["root"], sentinel_planner_id)
        scenario.assert_snapshot(current, ids, SERVER_REVISIONS[name],
                                 initial_manifest=initial["aggregate"]["manifest"])
        scenario.assert_sentinel(sentinel["aggregate"], initial_sentinel["aggregate"], ids)
        require(completion.same_json(completion.capture_current(api).items, initial_items.items),
                "An occurrence-only phase changed canonical templates")
        marker = completion.read_private_json(root / client / f"{name}.json")
        validate_marker(marker, run_id=run_id, phase=name, pending=pending, submitted=submitted, targets=targets,
                        catch_up=catch_up, publication_pending=publication_pending,
                        expected_items=scenario.projection(initial_items.items),
                        expected_occurrence=current["aggregate"] if expected is None else expected,
                        expected_sentinel=initial_sentinel["aggregate"])
        report["phases"].append({"client": client, "phase": name, "status": "passed"})
        phase_markers.append(marker)
        print(f"Passed {client}/{name}", flush=True)
        return current, marker

    try:
        idle_guard = subprocess.Popen(["/usr/bin/caffeinate", "-i", "-w", str(os.getpid())], env=env,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        print("Building owned local API", flush=True)
        support.run_logged([executables["cargo"], "build", "--locked", "-p", "dayweave-api"], root / "api-build.log", env)
        print("Prebuilding macOS routine tests (no opt-in credentials)", flush=True)
        handoff_started = True
        support.run_logged(native_command("macos", "prepare", prebuild=True), mac_build_log,
                           dict(env, DAYWEAVE_TESTING_RETAIN_RUNTIME="1"))
        runtime = completion.retained_runtime(mac_build_log)
        handoff_complete = True
        print("Prebuilding Android routine tests (no opt-in credentials)", flush=True)
        support.run_logged(native_command("android", "prepare_offline", prebuild=True), root / "android-prebuild.log", env)
        support.run_logged([executables["initdb"], "-D", str(pg_data), "-U", "dayweave_routine_test", "--no-locale",
                            "--encoding=UTF8", "--auth-local=trust", "--auth-host=trust"], root / "initdb.log", pg_env)
        pg_started = True
        support.run_logged([executables["pg_ctl"], "-D", str(pg_data), "-l", str(root / "postgres.log"), "-o",
                            f"-h 127.0.0.1 -p {pg_port} -k {root} -c max_locks_per_transaction=512", "-w", "start"],
                           root / "pg-start.log", pg_env)
        start_service("initial")
        api, authentication = completion.enroll_synthetic_session(api, access_token)
        support.private_json(root / "synthetic-session.json", authentication)
        for request in scenario.seed_requests(ids):
            api.call("v1/items", "POST", request)
        for name in ("optional", "inbox", "blocked"):
            review = api.call(f"v1/items/{ids[name]}/completion")
            api.call(f"v1/items/{ids[name]}/completion", "PUT",
                     scenario.optional_completion_command(review, str(uuid.uuid4())))
        as_of = datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
        request = scenario.preview_request(as_of)
        preview = api.call("v1/schedule/preview", "POST", request)
        first_publication = api.call("v1/schedule/publish", "POST",
                                     scenario.publish_request(request, preview, str(uuid.uuid4())))
        require(first_publication.get("replayed") is False, "Initial publication unexpectedly replayed")
        planner_id, sentinel_planner_id = published_identities(preview, ids["root"])
        initial = occurrence(api, ids["root"], planner_id)
        initial_sentinel = occurrence(api, ids["root"], sentinel_planner_id)
        instance_id, sentinel_id = (value["aggregate"]["manifest"]["id"] for value in (initial, initial_sentinel))
        require(len({instance_id, sentinel_id, planner_id, sentinel_planner_id}) == 4,
                "Planner and private ledger identities collided")
        scenario.assert_snapshot(initial, ids, 1, instance_id=instance_id, occurrence_id=planner_id)
        scenario.assert_snapshot(initial_sentinel, ids, 1, instance_id=sentinel_id, occurrence_id=sentinel_planner_id)
        initial_items = completion.capture_current(api)
        baseline = sql_evidence("baseline")
        support.private_json(root / "initial-occurrence.json", initial)
        support.private_json(root / "initial-sentinel.json", initial_sentinel)
        support.private_json(config_path, {
            "schema_version": 1, "run_id": run_id, "base_url": base_url, "bearer_token": access_token,
            "work_directory": str(root), "root_id": ids["root"], "branch_id": ids["branch"],
            "required_leaf_id": ids["required"], "optional_leaf_id": ids["optional"],
            "inbox_leaf_id": ids["inbox"], "blocked_leaf_id": ids["blocked"], "as_of": as_of,
            "occurrence_id": planner_id, "sentinel_occurrence_id": sentinel_planner_id,
            "instance_id": instance_id, "sentinel_instance_id": sentinel_id,
        })
        phase("macos", "prepare", pending=1)
        phase("android", "prepare_offline", pending=1, submitted=1)
        original_a, _ = phase("macos", "submit_lost", pending=1, submitted=1, expected=initial["aggregate"])
        scenario.assert_snapshot(original_a, ids, 2, initial_manifest=initial["aggregate"]["manifest"])
        support.private_json(root / "original-a-snapshot.json", original_a)
        kept_open, _ = phase("android", "conflict_keep_open")
        scenario.assert_snapshot(kept_open, ids, 3, initial_manifest=initial["aggregate"]["manifest"])
        support.stop_owned_group(server)
        start_service("restarted")
        replayed, lost_marker = phase("macos", "replay_lost_publication", catch_up=True, publication_pending=True)
        require(scenario.semantic_equal(replayed["aggregate"], kept_open["aggregate"]),
                "Historical receipt changed the newer occurrence")
        recovered, recovery_marker = phase("macos", "recover")
        require(scenario.semantic_equal(recovered["aggregate"], kept_open["aggregate"]),
                "Publication recovery changed occurrence state")
        require(recovery_marker["publication_operation_id"] is not None
                and recovery_marker["publication_operation_id"] != lost_marker["publication_operation_id"],
                "Old publication retry was mistaken for fresh composition")
        finished, _ = phase("android", "finish")
        scenario.assert_snapshot(finished, ids, 6, initial_manifest=initial["aggregate"]["manifest"])
        phase("macos", "verify")
        _, final_marker = phase("android", "verify")
        commands = {"A": completion.read_private_json(root / "macos/operation-a.json")}
        commands.update({letter.upper(): completion.read_private_json(root / f"android/operation-{letter}.json")
                         for letter in ("b", "c", "d", "e", "f")})
        final = sql_evidence("final")
        scenario.assert_final_sql(final, ids, baseline, commands, original_a,
                                  instance_id=instance_id, sentinel_id=sentinel_id)
        validate_publication_custody(phase_markers, final)
        published = api.call("v1/schedule/current")
        validate_current_publication(published, final, final_marker, ids, planner_id)
        report.update(status="passed", immutable_occurrence_operations=5, final_instance_revision=6,
            exact_blocker_reopening=True, optional_and_inbox_members_preserved=True,
            historical_receipt_no_rollback=True, exact_lost_publication_recovery=True,
            fresh_publication_required=True, service_restart=True, scoped_synthetic_session=True)
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
                    state = subprocess.run([executables["pg_ctl"], "-D", str(pg_data), "status"], env=pg_env,
                                           stdout=log, stderr=subprocess.STDOUT, timeout=5, check=False)
                    if state.returncode == 0:
                        subprocess.run([executables["pg_ctl"], "-D", str(pg_data), "-m", "fast", "-w", "-t", "15", "stop"],
                                       env=pg_env, stdout=log, stderr=subprocess.STDOUT, timeout=20, check=False)
                    state = subprocess.run([executables["pg_ctl"], "-D", str(pg_data), "status"], env=pg_env,
                                           stdout=log, stderr=subprocess.STDOUT, timeout=5, check=False)
                report["postgres_stopped"] = state.returncode == 3
            except (OSError, subprocess.TimeoutExpired):
                report["postgres_stopped"] = False
            if not report["postgres_stopped"]:
                report["status"] = "failed"
        report["service_stopped"] = server is None or server.poll() is not None
        try:
            completion.finish_runtime_cleanup(runtime, log=mac_build_log, handoff_started=handoff_started,
                                              handoff_complete=handoff_complete)
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
    require(report["status"] == "passed", "Owned routine cleanup failed; inspect private report")
    print("Native routine convergence passed; owned services stopped and retained test runtime removed", flush=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyboardInterrupt, support.GateInterrupted):
        print("Native routine convergence interrupted; owned cleanup attempted", file=sys.stderr)
        raise SystemExit(130) from None
    except (support.GateFailure, scenario.ScenarioError, OSError, ValueError, KeyError, TypeError) as error:
        message = str(error) if isinstance(error, support.GateFailure) else "Invalid synthetic routine configuration or evidence"
        print(f"Native routine convergence failed: {message}", file=sys.stderr)
        raise SystemExit(1) from None
