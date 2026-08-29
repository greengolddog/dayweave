#!/usr/bin/env python3
"""Redacting credential scan over Git index and object data.

The scanner deliberately never opens worktree paths. In particular, it never
follows a symlink or reads an ignored/untracked credential file. Findings name
only rule IDs, object kinds/OIDs, and SHA-256 path/ref fingerprints.
"""

from __future__ import annotations

import argparse
import dataclasses
import fnmatch
import hashlib
import os
import re
import subprocess
import sys
from collections.abc import Iterable, Sequence
from pathlib import Path


EXIT_FINDINGS = 1
EXIT_OPERATIONAL_ERROR = 2
MAX_OBJECT_BYTES = 64 * 1024 * 1024
MAX_SCAN_BYTES = 512 * 1024 * 1024
MAX_TREE_LIST_BYTES = 256 * 1024 * 1024
MAX_PRINTED_FINDINGS = 100
OID_RE = re.compile(rb"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
ZERO_OID_RE = re.compile(rb"^0+$")

CONTENT_RULES: tuple[tuple[str, re.Pattern[bytes]], ...] = (
    (
        "private-key",
        re.compile(
            rb"-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----"
            rb"|-----BEGIN PGP " rb"PRIVATE KEY BLOCK-----"
        ),
    ),
    (
        "github-token",
        re.compile(rb"(?:github_pat_[A-Za-z0-9_]{20,}|gh[pousr]_[A-Za-z0-9]{20,})"),
    ),
    (
        "google-credential",
        re.compile(
            rb"(?:AIza[0-9A-Za-z_-]{20,}|ya29\.[0-9A-Za-z_-]{10,}"
            rb"|GOCSPX-[0-9A-Za-z_-]{20,}|1//[0-9A-Za-z_-]{20,})"
        ),
    ),
    ("aws-access-key", re.compile(rb"(?:AKIA|ASIA)[0-9A-Z]{16}")),
    (
        "openai-key",
        re.compile(rb"sk-(?!(?:ant)-)(?:proj-|svcacct-)?[0-9A-Za-z_-]{20,}"),
    ),
    ("anthropic-key", re.compile(rb"sk-ant-[0-9A-Za-z_-]{20,}")),
    ("slack-token", re.compile(rb"xox[baprs]-[0-9A-Za-z-]{10,}")),
    ("stripe-live-key", re.compile(rb"(?:sk|rk)_live_[0-9A-Za-z]{16,}")),
    ("gitlab-token", re.compile(rb"glpat-[0-9A-Za-z_-]{20,}")),
    ("npm-token", re.compile(rb"npm_[0-9A-Za-z]{20,}")),
    (
        "pypi-token",
        re.compile(rb"pypi-AgEIcHlwaS5vcmc[0-9A-Za-z_-]{20,}"),
    ),
    (
        "sendgrid-key",
        re.compile(rb"SG\.[0-9A-Za-z_-]{16,}\.[0-9A-Za-z_-]{16,}"),
    ),
    ("digitalocean-token", re.compile(rb"dop_v1_[0-9A-Fa-f]{32,}")),
    (
        "dayweave-credential",
        re.compile(rb"dw_(?:da1|dr1|en1|mc1)_[0-9A-Za-z_-]{40,}"),
    ),
    (
        "jwt",
        re.compile(
            rb"eyJ[0-9A-Za-z_-]{8,}\.[0-9A-Za-z_-]{8,}"
            rb"\.[0-9A-Za-z_-]{8,}"
        ),
    ),
)

# Exact synthetic protocol fixture retained by server parser tests. The scanner
# exempts only this match digest, never its file or credential class.
EXACT_MATCH_ALLOWLIST: dict[str, frozenset[str]] = {
    "dayweave-credential": frozenset(
        {"f98197d069a87deafbc0a03fc44efc3a2cc0bf434a7490f8f0b65fed74e5c8c5"}
    ),
}

RISKY_SUFFIXES = (
    ".jks",
    ".keystore",
    ".key",
    ".pem",
    ".p12",
    ".pfx",
    ".pkcs12",
    ".mobileprovision",
)
RISKY_EXACT_NAMES = {
    ".netrc",
    "application_default_credentials.json",
    "googleservice-info.plist",
    "google-services.json",
    "keystore.properties",
    "local.properties",
    "release-signing.properties",
}
RISKY_GLOBS = (
    "*signing*.properties",
    "client_secret*.json",
    "credentials*.json",
    "service-account*.json",
    "service_account*.json",
)


class ScanError(RuntimeError):
    """A content-free operational failure suitable for redacted output."""


@dataclasses.dataclass(frozen=True, order=True)
class Finding:
    rule_id: str
    source: str
    object_type: str = "none"
    oid: str = "none"
    location_kind: str = "none"
    location_fingerprint: str = "none"


@dataclasses.dataclass(frozen=True)
class ObjectInfo:
    oid: bytes
    object_type: bytes
    size: int


def git(
    repo: Path,
    arguments: Sequence[str],
    *,
    input_bytes: bytes | None = None,
) -> bytes:
    environment = os.environ.copy()
    environment["GIT_OPTIONAL_LOCKS"] = "0"
    environment["GIT_NO_LAZY_FETCH"] = "1"
    environment["LC_ALL"] = "C"
    completed = subprocess.run(
        ["git", "-C", os.fspath(repo), *arguments],
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env=environment,
    )
    if completed.returncode != 0:
        command = arguments[0] if arguments else "unknown"
        raise ScanError(f"Git operation failed ({command})")
    return completed.stdout


def repository_root(candidate: str) -> Path:
    output = git(Path(candidate), ["rev-parse", "--show-toplevel"])
    try:
        return Path(os.fsdecode(output.rstrip(b"\n"))).resolve(strict=True)
    except (OSError, ValueError) as error:
        raise ScanError("Repository root could not be resolved") from error


def fingerprint(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()[:16]


def content_rule_ids(data: bytes) -> Iterable[str]:
    for rule_id, pattern in CONTENT_RULES:
        allowed_digests = EXACT_MATCH_ALLOWLIST.get(rule_id, frozenset())
        for match in pattern.finditer(data):
            match_digest = hashlib.sha256(match.group(0)).hexdigest()
            if match_digest not in allowed_digests:
                yield rule_id
                break


def risky_path_rule(path: bytes) -> str | None:
    normalized = path.replace(b"\\", b"/")
    lowered = normalized.lower()
    parts = lowered.split(b"/")
    basename = parts[-1] if parts else lowered

    if b"secrets" in parts:
        return "secrets-directory"
    if basename == b".env.example":
        return None
    if basename == b".env" or basename.startswith(b".env."):
        return "environment-file"
    if basename.endswith(tuple(value.encode("ascii") for value in RISKY_SUFFIXES)):
        return "key-or-signing-file"
    try:
        basename_text = basename.decode("utf-8", "surrogateescape")
    except UnicodeError:
        return "unsafe-filename"
    if basename_text in RISKY_EXACT_NAMES:
        return "credential-config-file"
    if any(fnmatch.fnmatchcase(basename_text, pattern) for pattern in RISKY_GLOBS):
        return "credential-config-file"
    if basename_text.endswith(
        (".tfvars", ".tfvars.json", ".auto.tfvars", ".auto.tfvars.json")
    ):
        return "terraform-secret-input"
    if ".tfstate" in basename_text or ".tfplan" in basename_text:
        return "terraform-state-or-plan"
    if lowered == b"deploy/tunnel/config.yaml":
        return "tunnel-credential-config"
    return None


class CredentialScanner:
    def __init__(self, repo: Path, scope: str) -> None:
        self.repo = repo
        self.scope = scope
        self.findings: set[Finding] = set()
        self.scanned_objects = 0
        self.scanned_paths = 0
        self.scanned_bytes = 0

    def add_content_findings(
        self,
        data: bytes,
        *,
        source: str,
        object_type: str,
        oid: bytes | None,
        path: bytes | None = None,
    ) -> None:
        location_kind = "path" if path is not None else "none"
        location_fingerprint = fingerprint(path) if path is not None else "none"
        oid_text = oid.decode("ascii")[:12] if oid is not None else "none"
        for rule_id in content_rule_ids(data):
            self.findings.add(
                Finding(
                    rule_id=rule_id,
                    source=source,
                    object_type=object_type,
                    oid=oid_text,
                    location_kind=location_kind,
                    location_fingerprint=location_fingerprint,
                )
            )

    def add_path_findings(self, path: bytes, *, source: str, oid: bytes | None) -> None:
        self.scanned_paths += 1
        path_fingerprint = fingerprint(path)
        oid_text = oid.decode("ascii")[:12] if oid is not None else "none"
        risky_rule = risky_path_rule(path)
        if risky_rule is not None:
            self.findings.add(
                Finding(
                    rule_id=risky_rule,
                    source=source,
                    object_type="path",
                    oid=oid_text,
                    location_kind="path",
                    location_fingerprint=path_fingerprint,
                )
            )
        for rule_id in content_rule_ids(path):
            self.findings.add(
                Finding(
                    rule_id=f"credential-in-path-{rule_id}",
                    source=source,
                    object_type="path",
                    oid=oid_text,
                    location_kind="path",
                    location_fingerprint=path_fingerprint,
                )
            )

    def add_ref_findings(self, ref_name: bytes, oid: bytes | None) -> None:
        ref_fingerprint = fingerprint(ref_name)
        oid_text = oid.decode("ascii")[:12] if oid is not None else "none"
        for rule_id in content_rule_ids(ref_name):
            self.findings.add(
                Finding(
                    rule_id=f"credential-in-ref-{rule_id}",
                    source="ref",
                    object_type="ref",
                    oid=oid_text,
                    location_kind="ref",
                    location_fingerprint=ref_fingerprint,
                )
            )

    def object_info(self, object_ids: Iterable[bytes]) -> dict[bytes, ObjectInfo]:
        ordered = sorted(set(object_ids))
        if not ordered:
            return {}
        for oid in ordered:
            if not OID_RE.fullmatch(oid):
                raise ScanError("Git returned an invalid object identifier")
        output = git(
            self.repo,
            ["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"],
            input_bytes=b"\n".join(ordered) + b"\n",
        )
        result: dict[bytes, ObjectInfo] = {}
        total_scan_bytes = 0
        for line in output.splitlines():
            fields = line.split(b" ")
            if len(fields) != 3 or not OID_RE.fullmatch(fields[0]):
                raise ScanError("A required Git object is missing or malformed")
            try:
                size = int(fields[2])
            except ValueError as error:
                raise ScanError("Git returned an invalid object size") from error
            if size < 0 or size > MAX_OBJECT_BYTES:
                raise ScanError("A scannable Git object exceeds the safety limit")
            if fields[1] in {b"blob", b"commit", b"tag"}:
                total_scan_bytes += size
            result[fields[0]] = ObjectInfo(fields[0], fields[1], size)
        if set(result) != set(ordered):
            raise ScanError("Not every required Git object was resolved")
        if total_scan_bytes > MAX_SCAN_BYTES:
            raise ScanError("The credential scan exceeds the aggregate safety limit")
        return result

    def read_objects(self, infos: Iterable[ObjectInfo]) -> Iterable[tuple[ObjectInfo, bytes]]:
        selected = sorted(
            (info for info in infos if info.object_type in {b"blob", b"commit", b"tag"}),
            key=lambda item: item.oid,
        )
        if not selected:
            return
        output = git(
            self.repo,
            ["cat-file", "--batch"],
            input_bytes=b"\n".join(info.oid for info in selected) + b"\n",
        )
        offset = 0
        for expected in selected:
            newline = output.find(b"\n", offset)
            if newline == -1:
                raise ScanError("Git object batch output is truncated")
            header = output[offset:newline].split(b" ")
            offset = newline + 1
            if len(header) != 3 or header[0] != expected.oid or header[1] != expected.object_type:
                raise ScanError("Git object batch output is inconsistent")
            try:
                size = int(header[2])
            except ValueError as error:
                raise ScanError("Git object batch output has an invalid size") from error
            if size != expected.size or offset + size >= len(output):
                raise ScanError("Git object batch output has an invalid boundary")
            data = output[offset : offset + size]
            offset += size
            if output[offset : offset + 1] != b"\n":
                raise ScanError("Git object batch output lacks a delimiter")
            offset += 1
            self.scanned_objects += 1
            self.scanned_bytes += size
            yield expected, data
        if offset != len(output):
            raise ScanError("Git object batch output has unexpected trailing data")

    def scan_object_set(
        self,
        object_ids: Iterable[bytes],
        *,
        source: str,
        known_paths: dict[bytes, bytes] | None = None,
    ) -> dict[bytes, ObjectInfo]:
        infos = self.object_info(object_ids)
        paths = known_paths or {}
        for info, data in self.read_objects(infos.values()):
            path = paths.get(info.oid) if info.object_type == b"blob" else None
            self.add_content_findings(
                data,
                source=source,
                object_type=info.object_type.decode("ascii"),
                oid=info.oid,
                path=path,
            )
        return infos

    def collect_tree_paths(
        self, commits: Iterable[bytes], *, source: str
    ) -> dict[bytes, bytes]:
        blob_paths: dict[bytes, bytes] = {}
        total_tree_bytes = 0
        for commit in sorted(set(commits)):
            output = git(
                self.repo,
                ["ls-tree", "-r", "-z", "--full-tree", commit.decode("ascii")],
            )
            total_tree_bytes += len(output)
            if total_tree_bytes > MAX_TREE_LIST_BYTES:
                raise ScanError("Historical path enumeration exceeds the safety limit")
            for record in output.split(b"\0"):
                if not record:
                    continue
                try:
                    metadata, path = record.split(b"\t", 1)
                    mode, object_type, oid = metadata.split(b" ", 2)
                except ValueError as error:
                    raise ScanError("Git returned a malformed tree entry") from error
                if not OID_RE.fullmatch(oid):
                    raise ScanError("Git returned an invalid tree object identifier")
                if object_type not in {b"blob", b"commit"}:
                    raise ScanError("Git returned an unsupported tree entry type")
                if object_type == b"blob" and mode not in {b"100644", b"100755", b"120000"}:
                    raise ScanError("Git returned an unsupported blob mode")
                if object_type == b"commit" and mode != b"160000":
                    raise ScanError("Git returned an unsupported gitlink mode")
                self.add_path_findings(path, source=source, oid=oid)
                if object_type == b"blob":
                    blob_paths.setdefault(oid, path)
        return blob_paths

    def scan_staged(self) -> None:
        changed_output = git(
            self.repo,
            [
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--name-only",
                "-z",
                "--diff-filter=ACMRTUXB",
                "--",
            ],
        )
        changed_paths = {path for path in changed_output.split(b"\0") if path}
        if not changed_paths:
            return

        entries_by_path: dict[bytes, list[tuple[bytes, bytes, bytes]]] = {}
        index_output = git(self.repo, ["ls-files", "--stage", "-z"])
        for record in index_output.split(b"\0"):
            if not record:
                continue
            try:
                metadata, path = record.split(b"\t", 1)
                mode, oid, stage = metadata.split(b" ", 2)
            except ValueError as error:
                raise ScanError("Git returned a malformed index entry") from error
            if path in changed_paths:
                entries_by_path.setdefault(path, []).append((mode, oid, stage))

        object_paths: dict[bytes, bytes] = {}
        for path in sorted(changed_paths):
            entries = entries_by_path.get(path)
            if entries is None or len(entries) != 1:
                raise ScanError("A staged path is unresolved or missing from the index")
            mode, oid, stage = entries[0]
            if stage != b"0":
                raise ScanError("An unmerged index entry cannot be credential-scanned")
            if mode not in {b"100644", b"100755", b"120000"}:
                raise ScanError("An unsupported staged entry cannot be credential-scanned")
            if not OID_RE.fullmatch(oid):
                raise ScanError("A staged entry has an invalid object identifier")
            self.add_path_findings(path, source="index", oid=oid)
            object_paths.setdefault(oid, path)
        self.scan_object_set(
            object_paths,
            source="index",
            known_paths=object_paths,
        )

    def ensure_full_history(self) -> None:
        shallow = git(self.repo, ["rev-parse", "--is-shallow-repository"]).strip()
        if shallow != b"false":
            raise ScanError("Full-history credential scanning requires a non-shallow repository")

    def ref_roots(self) -> tuple[set[bytes], set[bytes]]:
        roots: set[bytes] = set()
        refs: set[bytes] = set()
        output = git(self.repo, ["for-each-ref", "--format=%(refname)%00%(objectname)"])
        for line in output.splitlines():
            try:
                ref_name, oid = line.split(b"\0", 1)
            except ValueError as error:
                raise ScanError("Git returned malformed ref metadata") from error
            if not OID_RE.fullmatch(oid):
                raise ScanError("Git returned an invalid ref object identifier")
            refs.add(ref_name)
            roots.add(oid)
            self.add_ref_findings(ref_name, oid)
        return roots, refs

    def reachable_objects(self, revisions: Sequence[str]) -> set[bytes]:
        output = git(
            self.repo,
            ["rev-list", "--objects", "--no-object-names", "--missing=print", *revisions],
        )
        object_ids: set[bytes] = set()
        for line in output.splitlines():
            if line.startswith(b"?"):
                raise ScanError("Reachable history contains a missing object")
            if not OID_RE.fullmatch(line):
                raise ScanError("Git returned malformed reachable-object metadata")
            object_ids.add(line)
        return object_ids

    def scan_reachable(self, roots: set[bytes], *, revisions: Sequence[str], source: str) -> None:
        object_ids = self.reachable_objects(revisions) | roots
        infos = self.object_info(object_ids)
        commits = {oid for oid, info in infos.items() if info.object_type == b"commit"}
        paths = self.collect_tree_paths(commits, source=source)
        self.scan_object_set(object_ids, source=source, known_paths=paths)

    def scan_history(self) -> None:
        self.ensure_full_history()
        roots, _ = self.ref_roots()
        self.scan_reachable(roots, revisions=["--all"], source="history")

    def scan_outgoing_head(self) -> None:
        self.ensure_full_history()
        head = git(self.repo, ["rev-parse", "--verify", "HEAD"]).strip()
        if not OID_RE.fullmatch(head):
            raise ScanError("HEAD does not resolve to a supported Git object")
        self.scan_reachable({head}, revisions=[head.decode("ascii")], source="outgoing")

    def scan_pre_push(self, update_stream: bytes) -> None:
        self.ensure_full_history()
        roots: set[bytes] = set()
        local_ref_names: set[bytes] = set()
        remote_ref_names: set[bytes] = set()
        for line in update_stream.splitlines():
            fields = line.split(b" ")
            if len(fields) != 4:
                raise ScanError("Pre-push input is malformed")
            local_ref, local_oid, remote_ref, remote_oid = fields
            if (
                not local_ref
                or not remote_ref
                or not remote_oid
                or (
                    not ZERO_OID_RE.fullmatch(remote_oid)
                    and not OID_RE.fullmatch(remote_oid)
                )
            ):
                raise ScanError("Pre-push input contains invalid metadata")
            local_ref_names.add(local_ref)
            remote_ref_names.add(remote_ref)
            if ZERO_OID_RE.fullmatch(local_oid):
                continue
            if not OID_RE.fullmatch(local_oid):
                raise ScanError("Pre-push input contains an invalid local object")
            roots.add(local_oid)

        for ref_name in local_ref_names:
            self.add_ref_findings(ref_name, None)
        for ref_name in remote_ref_names:
            self.add_ref_findings(ref_name, None)
        if not roots:
            return
        revisions = [oid.decode("ascii") for oid in sorted(roots)]
        self.scan_reachable(roots, revisions=revisions, source="outgoing")

    def print_result(self) -> int:
        if not self.findings:
            print(
                "credential scan: PASS "
                f"scope={self.scope} objects={self.scanned_objects} "
                f"paths={self.scanned_paths} bytes={self.scanned_bytes}"
            )
            return 0

        ordered = sorted(self.findings)
        print(f"credential scan: FAIL scope={self.scope} findings={len(ordered)}")
        for finding in ordered[:MAX_PRINTED_FINDINGS]:
            print(
                "  "
                f"rule={finding.rule_id} source={finding.source} "
                f"object_type={finding.object_type} oid={finding.oid} "
                f"{finding.location_kind}_sha256={finding.location_fingerprint}"
            )
        if len(ordered) > MAX_PRINTED_FINDINGS:
            print(
                "  additional_findings="
                f"{len(ordered) - MAX_PRINTED_FINDINGS} (details suppressed)"
            )
        return EXIT_FINDINGS


def parse_arguments(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Scan Git index/object data for credential material without printing matches."
    )
    parser.add_argument(
        "scope",
        choices=("staged", "outgoing", "pre-push", "history", "all"),
    )
    parser.add_argument("--repo", default=".", help="Git worktree to scan (default: current directory)")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(argv if argv is not None else sys.argv[1:])
    try:
        repo = repository_root(arguments.repo)
        scanner = CredentialScanner(repo, arguments.scope)
        if arguments.scope == "staged":
            scanner.scan_staged()
        elif arguments.scope == "outgoing":
            scanner.scan_outgoing_head()
        elif arguments.scope == "pre-push":
            scanner.scan_pre_push(sys.stdin.buffer.read())
        elif arguments.scope == "history":
            scanner.scan_history()
        else:
            scanner.scan_staged()
            scanner.scan_history()
        return scanner.print_result()
    except ScanError as error:
        print(f"credential scan: ERROR scope={arguments.scope}: {error}", file=sys.stderr)
        return EXIT_OPERATIONAL_ERROR
    except Exception:
        print(
            f"credential scan: ERROR scope={arguments.scope}: unexpected internal failure",
            file=sys.stderr,
        )
        return EXIT_OPERATIONAL_ERROR


if __name__ == "__main__":
    raise SystemExit(main())
