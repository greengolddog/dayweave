#!/usr/bin/env python3
"""Opt-in macOS/Android JVM convergence against a fresh local PostgreSQL API.

All runtime state is synthetic and stays in a new private /tmp directory. No
existing database, app profile, credential, provider, or deployment is used.
The service gets an explicit environment, never inherited integration settings.
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid


REPO = Path(__file__).resolve().parent.parent
PHASE_TIMEOUT = 600


class GateFailure(Exception):
    """A content-free failure suitable for terminal output."""


class GateInterrupted(BaseException):
    """Cannot be swallowed by ordinary request-failure retry handlers."""


def private_json(path: Path, value: object) -> None:
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write("\n")
    path.chmod(0o600)


def loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def build_environment() -> dict[str, str]:
    # Do not forward application/provider/signing credentials to child tools.
    allowed = {"PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "JAVA_HOME",
               "ANDROID_HOME", "ANDROID_SDK_ROOT", "RUSTUP_HOME", "CARGO_HOME",
               "DEVELOPER_DIR", "SDKROOT"}
    return {key: value for key, value in os.environ.items() if key in allowed}


def service_environment(token: str, database_url: str, port: int,
                        user_id: str, workspace_id: str) -> dict[str, str]:
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "DAYWEAVE_ENVIRONMENT": "test",
        "DAYWEAVE_AUTH_MODE": "legacy_static",
        "DAYWEAVE_BIND_ADDRESS": f"127.0.0.1:{port}",
        "DAYWEAVE_API_TOKEN": token,
        "DAYWEAVE_DATABASE_URL": database_url,
        "DAYWEAVE_DEFAULT_USER_ID": user_id,
        "DAYWEAVE_DEFAULT_WORKSPACE_ID": workspace_id,
        "DAYWEAVE_DEFAULT_TIMEZONE": "UTC",
        "DAYWEAVE_OWNER_SUBJECT": f"synthetic-native-convergence-{user_id}",
        "DAYWEAVE_GOOGLE_OAUTH_ENABLED": "false",
        "DAYWEAVE_GOOGLE_OUTBOUND_ENABLED": "false",
        "DAYWEAVE_GOOGLE_SCHEDULE_OUTBOUND_ENABLED": "false",
        "DAYWEAVE_MCP_OAUTH_ENABLED": "false",
        "DAYWEAVE_ASSISTANT_ENABLED": "false",
        "RUST_LOG": "dayweave_api=warn,tower_http=warn",
    }


def run_logged(command: list[str], log: Path, env: dict[str, str], timeout: int = PHASE_TIMEOUT) -> None:
    with log.open("xb") as stream:
        process = subprocess.Popen(command, cwd=REPO, env=env, stdout=stream,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            result = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise GateFailure(f"Timed out; inspect {log.name}") from error
        finally:
            stop_owned_group(process)
    if result:
        raise GateFailure(f"Command failed; inspect {log.name}")


class RejectRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise GateFailure("Local API redirects are not permitted")


class LocalAPI:
    def __init__(self, base_url: str, token: str):
        self.base_url = base_url
        self.token = token
        # Environment proxies must not receive even synthetic authentication.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), RejectRedirects())

    def call(self, path: str, method: str = "GET", body: object | None = None) -> object:
        headers = {"Authorization": f"Bearer {self.token}", "Accept": "application/json"}
        data = None
        if body is not None:
            data = json.dumps(body, separators=(",", ":")).encode()
            headers.update({"Content-Type": "application/json", "Idempotency-Key": str(uuid.uuid4())})
        request = urllib.request.Request(self.base_url + path, data=data, method=method, headers=headers)
        try:
            with self.opener.open(request, timeout=5) as response:
                return json.load(response)
        except (urllib.error.URLError, ValueError) as error:
            raise GateFailure(f"Local API request failed ({method} {path.split('?')[0]})") from error


def owned_group_alive(process: subprocess.Popen[bytes]) -> bool:
    if getattr(process, "_dayweave_group_retired", False):
        return False
    process.poll()  # Reap the leader independently of any remaining descendants.
    # Darwin can return EPERM for a disappeared group during a signal-zero
    # probe. Inspect only IDs/state (never command lines or environments), and
    # exclude zombies that cannot run or receive signals.
    inventory = subprocess.run(["/bin/ps", "-axo", "pgid=,uid=,stat="],
                               capture_output=True, timeout=5, check=False)
    if inventory.returncode:
        raise GateFailure("Cannot inspect owned process-group cleanup")
    members = [line.split() for line in inventory.stdout.decode().splitlines()]
    live = [row for row in members if len(row) == 3 and int(row[0]) == process.pid and not row[2].startswith("Z")]
    if any(int(row[1]) != os.getuid() for row in live):
        raise GateFailure("Owned process group contains an inaccessible member")
    if not live:
        process._dayweave_group_retired = True
    return bool(live)


def stop_owned_group(process: subprocess.Popen[bytes] | None, grace_seconds: float = 3) -> None:
    if process is None or not owned_group_alive(process):
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        deadline = time.monotonic() + grace_seconds
        while owned_group_alive(process) and time.monotonic() < deadline:
            time.sleep(0.05)
        if owned_group_alive(process):
            os.killpg(process.pid, signal.SIGKILL)
            deadline = time.monotonic() + 5
            while owned_group_alive(process) and time.monotonic() < deadline:
                time.sleep(0.05)
        process.wait(timeout=5)
        if owned_group_alive(process):
            raise GateFailure("Owned process group did not stop")
    except (ProcessLookupError, PermissionError):
        if owned_group_alive(process):
            raise GateFailure("Cannot stop owned process group") from None
        process.wait(timeout=5)


def interrupted(_signum, _frame) -> None:
    raise GateInterrupted()


def main() -> int:
    if sys.platform != "darwin":
        raise GateFailure("This cross-client gate requires macOS")
    if len(sys.argv) != 1:
        raise GateFailure("Run without arguments; existing service/database targets are not accepted")
    java_home = os.environ.get("JAVA_HOME", "")
    android_sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT", "")
    if not java_home or not (Path(java_home) / "bin/java").is_file() or not android_sdk or not Path(android_sdk).is_dir():
        raise GateFailure("Set JAVA_HOME to the supported build JDK and ANDROID_HOME to the installed test SDK")
    executables = {name: shutil.which(name) for name in ("initdb", "pg_ctl", "psql", "cargo")}
    if not all(executables.values()):
        raise GateFailure("Install local PostgreSQL tools and Rust before running this gate")
    os.umask(0o077)
    signal.signal(signal.SIGTERM, interrupted)
    root = Path(tempfile.mkdtemp(prefix="dayweave-native-convergence.", dir="/tmp")).resolve()
    root.chmod(0o700)
    print(f"Synthetic native convergence artifacts: {root}", flush=True)
    pg_data = root / "postgres"
    pg_port, api_port = loopback_port(), loopback_port()
    while api_port == pg_port:
        api_port = loopback_port()
    token = "native-convergence-" + secrets.token_urlsafe(32)
    user_id, workspace_id, item_id, child_id, run_id = (str(uuid.uuid4()) for _ in range(5))
    db_url = f"postgres://dayweave_native_test@127.0.0.1:{pg_port}/postgres"
    env = build_environment()
    service_env = service_environment(token, db_url, api_port, user_id, workspace_id)
    base_url = f"http://127.0.0.1:{api_port}/"
    api = LocalAPI(base_url, token)
    fixture = json.loads((REPO / "fixtures/item-progress/components-v1.json").read_text())
    initial = next(case["components"] for case in fixture["valid"] if case["name"] == "all_modes")
    replacement = copy.deepcopy(initial)
    replacement[0]["value"]["basis_points"] = 10000
    replacement[1]["value"].update(elapsed_seconds=7200, remaining_seconds=0)
    replacement[2]["value"].update(current="4.125001", target={"value": "2.5", "direction": "at_most"})
    config_path = root / "config.json"
    private_json(config_path, {
        "schema_version": 1, "run_id": run_id, "base_url": base_url, "bearer_token": token,
        "item_id": item_id, "child_id": child_id, "work_directory": str(root),
        "initial_components": initial, "replacement_components": replacement,
    })
    server: subprocess.Popen[bytes] | None = None
    server_logs: list[object] = []
    pg_started = False
    report: dict[str, object] = {"schema_version": 1, "run_id": run_id, "status": "failed", "phases": []}

    def start_service(label: str) -> subprocess.Popen[bytes]:
        nonlocal server
        log = (root / f"api-{label}.log").open("xb")
        server_logs.append(log)
        process = subprocess.Popen([str(REPO / "target/debug/dayweave-api")], cwd=root,
                                   env=service_env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        server = process
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise GateFailure(f"Local service stopped during {label}; inspect its log")
            try:
                api.call("readyz")
                # Authenticated scope proof prevents accepting another listener.
                api.call("v1/items/delta")
                return process
            except GateFailure:
                time.sleep(0.2)
        stop_owned_group(process)
        raise GateFailure(f"Local service did not become ready during {label}")

    def phase(client: str, name: str, revision: int, pending: int) -> None:
        print(f"Running {client}/{name}", flush=True)
        phase_env = dict(env, DAYWEAVE_NATIVE_CONVERGENCE_CONFIG=str(config_path),
                         DAYWEAVE_NATIVE_CONVERGENCE_PHASE=name)
        if client == "macos":
            command = [str(REPO / "scripts/test-macos.sh"), "-Xswiftc", "-warnings-as-errors",
                       "--filter", "NativeProgressConvergenceTests"]
        else:
            command = [str(REPO / "apps/android/gradlew"), "--project-dir", str(REPO / "apps/android"),
                       "--no-daemon", "--no-configuration-cache", "--rerun-tasks", ":app:testDebugUnitTest",
                       "--tests", "*.NativeProgressConvergenceTest"]
        run_logged(command, root / f"{client}-{name}.log", phase_env)
        marker = json.loads((root / client / f"{name}.json").read_text())
        if (marker.get("run_id") != run_id or marker.get("phase") != name or marker.get("status") != "passed"
                or marker.get("progress_revision") != revision or marker.get("pending_count") != pending):
            raise GateFailure(f"Invalid result marker for {client}/{name}")
        report["phases"].append({"client": client, "phase": name, "status": "passed"})
        print(f"Passed {client}/{name}", flush=True)

    try:
        print("Building local service", flush=True)
        run_logged([executables["cargo"], "build", "--locked", "-p", "dayweave-api"], root / "api-build.log", env)
        run_logged([executables["initdb"], "-D", str(pg_data), "-U", "dayweave_native_test", "--no-locale",
                    "--encoding=UTF8", "--auth-local=trust", "--auth-host=trust"], root / "initdb.log", env)
        pg_started = True  # A failed/interrupted start may still have spawned PostgreSQL.
        run_logged([executables["pg_ctl"], "-D", str(pg_data), "-l", str(root / "postgres.log"),
                    "-o", f"-h 127.0.0.1 -p {pg_port} -k {root}", "-w", "start"], root / "pg-start.log", env)
        server = start_service("initial")
        goal = {"id": item_id, "kind": "goal", "status": "planned", "title": "Synthetic convergence goal",
                "timezone_name": "UTC", "duration_seconds": None, "flexible_constraints": {}, "has_own_effort": False,
                "split_policy": {"type": "indivisible"}, "importance": 50, "urgency": 50,
                "is_sensitive": False, "parent_id": None, "sibling_order": 0}
        child = dict(goal, id=child_id, kind="task", title="Synthetic convergence child", parent_id=item_id,
                     duration_seconds=1800)
        api.call("v1/items", "POST", goal)
        api.call("v1/items", "POST", child)
        canonical_before = api.call("v1/items/delta")
        private_json(root / "canonical-before.json", canonical_before)
        phase("macos", "prepare", 0, 1)
        phase("android", "prepare", 0, 1)
        phase("macos", "submit_lost", 0, 1)
        first = api.call(f"v1/items/{item_id}/progress")
        if first["revision"] != 1 or first["components"] != initial:
            raise GateFailure("Lost-response simulation did not commit the reviewed server values")
        phase("android", "conflict_update", 2, 0)
        stop_owned_group(server)
        server = start_service("restarted")
        phase("macos", "replay", 2, 0)
        phase("android", "verify", 2, 0)
        final = api.call(f"v1/items/{item_id}/progress")
        if final["revision"] != 2 or final["components"] != replacement:
            raise GateFailure("Final service progress differs from the reviewed replacement")
        if api.call("v1/items/delta") != canonical_before:
            raise GateFailure("Independent progress changed canonical item or delta state")
        child_progress = api.call(f"v1/items/{child_id}/progress")
        if child_progress["revision"] != 0 or child_progress["components"]:
            raise GateFailure("Parent independent progress changed the child sidecar")
        sql = ("SELECT json_build_object('receipts', (SELECT count(*) FROM item_progress_operations), "
               "'sidecars', (SELECT count(*) FROM item_progress), "
               "'revisions', (SELECT json_agg(progress_revision ORDER BY progress_revision) FROM item_progress_operations))")
        check = subprocess.run([executables["psql"], db_url, "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", sql],
                               env=env, capture_output=True, timeout=10, check=False)
        if check.returncode or json.loads(check.stdout) != {"receipts": 2, "sidecars": 1, "revisions": [1, 2]}:
            raise GateFailure("Durable SQL custody contains extra/missing progress operations")
        report.update(status="passed", final_progress_revision=2, immutable_receipts=2,
                      canonical_and_child_unchanged=True, service_restart=True,
                      final_components_sha256=hashlib.sha256(json.dumps(replacement, sort_keys=True).encode()).hexdigest())
    finally:
        # Finish bounded cleanup after interruption instead of abandoning children
        # on a second signal. Only handles/data directories created above are used.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            stop_owned_group(server)
        except (GateFailure, OSError, subprocess.TimeoutExpired):
            report["status"] = "failed"
        for log in server_logs:
            log.close()
        if pg_started:
            try:
                with (root / "pg-stop.log").open("xb") as log:
                    status = subprocess.run([executables["pg_ctl"], "-D", str(pg_data), "status"], env=env,
                                            stdout=log, stderr=subprocess.STDOUT, timeout=5, check=False)
                    if status.returncode == 0:
                        subprocess.run([executables["pg_ctl"], "-D", str(pg_data), "-m", "fast", "-w", "-t", "15", "stop"],
                                       env=env, stdout=log, stderr=subprocess.STDOUT, timeout=20, check=False)
                    status = subprocess.run([executables["pg_ctl"], "-D", str(pg_data), "status"], env=env,
                                            stdout=log, stderr=subprocess.STDOUT, timeout=5, check=False)
                report["postgres_stopped"] = status.returncode == 3
            except (OSError, subprocess.TimeoutExpired):
                report["postgres_stopped"] = False
            if not report["postgres_stopped"]:
                report["status"] = "failed"
        report["service_stopped"] = server is None or server.poll() is not None
        private_json(root / "report.json", report)
    if report["status"] != "passed":
        raise GateFailure("Owned PostgreSQL cleanup failed; inspect the private run report")
    print("Native progress convergence passed; owned API and PostgreSQL stopped", flush=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyboardInterrupt, GateInterrupted):
        print("Native convergence interrupted; owned-process cleanup attempted", file=sys.stderr)
        raise SystemExit(130) from None
    except (GateFailure, OSError, ValueError, KeyError, StopIteration) as error:
        # Never print arbitrary HTTP/JSON/config content or inherited credentials.
        message = str(error) if isinstance(error, GateFailure) else "Invalid local gate configuration or artifact"
        print(f"Native convergence failed: {message}", file=sys.stderr)
        raise SystemExit(1) from None
