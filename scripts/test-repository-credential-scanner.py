#!/usr/bin/env python3
"""Isolated integration tests for the redacting Git credential scanner."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("scan-repository-credentials.py").resolve()
SOURCE_ROOT = SCRIPT.parent.parent
ZERO_OID = "0" * 40


def github_canary(fill: str = "A") -> str:
    return "gh" + "p_" + (fill * 36)


def private_key_canary() -> bytes:
    return b"-----BEGIN " + b"PRIVATE KEY-----"


class Repository:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.git("init", "--quiet", "--initial-branch=main")
        self.git("config", "user.name", "Synthetic Scanner Test")
        self.git("config", "user.email", "scanner-test@invalid.example")

    def git(
        self,
        *arguments: str,
        input_bytes: bytes | None = None,
        check: bool = True,
    ) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            ["git", "-C", os.fspath(self.path), *arguments],
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=check,
            env={**os.environ, "LC_ALL": "C"},
        )

    def write(self, relative_path: str, data: bytes) -> Path:
        destination = self.path / relative_path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        return destination

    def commit(self, message: bytes = b"synthetic test commit") -> str:
        self.git("commit", "--quiet", "--allow-empty", "--file=-", input_bytes=message)
        return self.git("rev-parse", "HEAD").stdout.decode("ascii").strip()

    def scan(
        self, scope: str, *, input_bytes: bytes | None = None
    ) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            ["python3", "-B", os.fspath(SCRIPT), scope, "--repo", os.fspath(self.path)],
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env={**os.environ, "LC_ALL": "C"},
        )


class CredentialScannerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dayweave-scanner-test-")
        self.addCleanup(self.temporary.cleanup)

    def new_repository(self, name: str = "repo") -> Repository:
        path = Path(self.temporary.name) / name
        path.mkdir()
        return Repository(path)

    def assert_redacted(
        self,
        result: subprocess.CompletedProcess[bytes],
        *forbidden_values: str | bytes,
    ) -> None:
        output = result.stdout + result.stderr
        for value in forbidden_values:
            encoded = value if isinstance(value, bytes) else value.encode()
            self.assertNotIn(encoded, output)

    def test_clean_index_and_history_pass(self) -> None:
        repository = self.new_repository()
        repository.write("README.md", b"synthetic clean repository\n")
        repository.git("add", "README.md")
        self.assertEqual(repository.scan("staged").returncode, 0)
        repository.commit()
        self.assertEqual(repository.scan("history").returncode, 0)

    def test_staged_scan_reads_index_blob_not_worktree_copy(self) -> None:
        repository = self.new_repository()
        canary = github_canary()
        path = "payload.bin"
        repository.write(path, b"before\x00" + canary.encode() + b"\x00after")
        repository.git("add", path)
        repository.write(path, b"clean worktree replacement\n")

        result = repository.scan("staged")

        self.assertEqual(result.returncode, 1)
        self.assert_redacted(result, canary, path)

    def test_staged_symlink_does_not_read_ignored_target(self) -> None:
        repository = self.new_repository()
        repository.write(".gitignore", b"ignored/\n")
        target = repository.write("ignored/private.pem", private_key_canary())
        target.chmod(0)
        os.symlink("ignored/private.pem", repository.path / "safe-link")
        repository.git("add", ".gitignore", "safe-link")

        result = repository.scan("staged")

        target.chmod(0o600)
        self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_force_added_ignored_risky_file_is_blocked(self) -> None:
        repository = self.new_repository()
        risky_path = "release-signing.properties"
        repository.write(".gitignore", b"*signing*.properties\n")
        repository.write(risky_path, b"synthetic=true\n")
        repository.git("add", ".gitignore")
        repository.git("add", "--force", risky_path)

        result = repository.scan("staged")

        self.assertEqual(result.returncode, 1)
        self.assert_redacted(result, risky_path)

    def test_staged_deletion_has_no_final_blob_and_passes(self) -> None:
        repository = self.new_repository()
        risky_path = "release-signing.properties"
        repository.write(risky_path, b"synthetic=true\n")
        repository.git("add", risky_path)
        repository.commit()
        repository.git("rm", "--quiet", risky_path)

        result = repository.scan("staged")

        self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_pre_push_scans_an_earlier_commit_deleted_at_tip(self) -> None:
        repository = self.new_repository()
        repository.write("README.md", b"base\n")
        repository.git("add", "README.md")
        base = repository.commit()
        canary = github_canary("B")
        path = "temporary.txt"
        repository.write(path, canary.encode())
        repository.git("add", path)
        repository.commit()
        repository.git("rm", "--quiet", path)
        head = repository.commit()
        update = f"refs/heads/main {head} refs/heads/main {base}\n".encode()

        result = repository.scan("pre-push", input_bytes=update)

        self.assertEqual(result.returncode, 1)
        self.assert_redacted(result, canary, path, "refs/heads/main")
        outgoing_result = repository.scan("outgoing")
        self.assertEqual(outgoing_result.returncode, 1)
        self.assert_redacted(outgoing_result, canary, path)

    def test_history_scans_commit_and_annotated_tag_metadata(self) -> None:
        commit_repository = self.new_repository("commit-metadata")
        commit_canary = github_canary("C")
        commit_repository.commit(commit_canary.encode())
        commit_result = commit_repository.scan("history")
        self.assertEqual(commit_result.returncode, 1)
        self.assert_redacted(commit_result, commit_canary)

        tag_repository = self.new_repository("tag-metadata")
        tag_repository.commit()
        tag_canary = github_canary("D")
        tag_repository.git("tag", "--annotate", "v1", "--file=-", input_bytes=tag_canary.encode())
        tag_result = tag_repository.scan("history")
        self.assertEqual(tag_result.returncode, 1)
        self.assert_redacted(tag_result, tag_canary)

    def test_history_scans_ref_names_and_old_tree_paths(self) -> None:
        ref_repository = self.new_repository("ref-name")
        head = ref_repository.commit()
        ref_canary = github_canary("E")
        ref_name = "refs/heads/" + ref_canary
        ref_repository.git("update-ref", ref_name, head)
        ref_result = ref_repository.scan("history")
        self.assertEqual(ref_result.returncode, 1)
        self.assert_redacted(ref_result, ref_canary, ref_name)

        path_repository = self.new_repository("old-path")
        risky_path = "client_secret_fixture.json"
        path_repository.write(risky_path, b"synthetic only\n")
        path_repository.git("add", risky_path)
        path_repository.commit()
        path_repository.git("rm", "--quiet", risky_path)
        path_repository.commit()
        path_result = path_repository.scan("history")
        self.assertEqual(path_result.returncode, 1)
        self.assert_redacted(path_result, risky_path)

    def test_pre_push_scans_remote_ref_name_and_delete_only_passes(self) -> None:
        repository = self.new_repository()
        head = repository.commit()
        ref_canary = github_canary("F")
        update = f"(delete) {ZERO_OID} refs/heads/{ref_canary} {head}\n".encode()

        result = repository.scan("pre-push", input_bytes=update)

        self.assertEqual(result.returncode, 1)
        self.assert_redacted(result, ref_canary)

        clean_update = f"(delete) {ZERO_OID} refs/heads/obsolete {head}\n".encode()
        self.assertEqual(repository.scan("pre-push", input_bytes=clean_update).returncode, 0)

    def test_malformed_pre_push_input_fails_without_echo(self) -> None:
        repository = self.new_repository()
        repository.commit()
        untrusted = github_canary("G")

        result = repository.scan("pre-push", input_bytes=untrusted.encode() + b"\n")

        self.assertEqual(result.returncode, 2)
        self.assert_redacted(result, untrusted)

    def test_shallow_history_and_outgoing_scans_fail_closed(self) -> None:
        source = self.new_repository("source")
        source.commit()
        source.commit()
        shallow_path = Path(self.temporary.name) / "shallow"
        subprocess.run(
            ["git", "clone", "--quiet", "--depth=1", source.path.as_uri(), os.fspath(shallow_path)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        shallow = Repository.__new__(Repository)
        shallow.path = shallow_path

        self.assertEqual(shallow.scan("history").returncode, 2)
        self.assertEqual(shallow.scan("outgoing").returncode, 2)

    def test_explicit_hook_installer_and_pre_commit_work_from_subdirectory(self) -> None:
        repository = self.new_repository()
        repository.write("README.md", b"clean\n")
        repository.git("add", "README.md")
        repository.commit()
        for relative_path in (
            "scripts/scan-repository-credentials.py",
            "scripts/install-git-hooks.sh",
            ".githooks/pre-commit",
            ".githooks/pre-push",
        ):
            destination = repository.path / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(SOURCE_ROOT / relative_path, destination)
        nested = repository.path / "nested" / "directory"
        nested.mkdir(parents=True)

        install_result = subprocess.run(
            [os.fspath(repository.path / "scripts/install-git-hooks.sh")],
            cwd=nested,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

        self.assertEqual(install_result.returncode, 0, install_result.stderr.decode())
        configured = repository.git("config", "--local", "--get", "core.hooksPath")
        self.assertEqual(configured.stdout.strip(), b".githooks")

        canary = github_canary("H")
        path = "staged-payload.bin"
        repository.write(path, canary.encode())
        repository.git("add", path)
        hook_result = subprocess.run(
            [os.fspath(repository.path / ".githooks/pre-commit")],
            cwd=nested,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(hook_result.returncode, 1)
        self.assert_redacted(hook_result, canary, path)

        repository.git("config", "--local", "core.hooksPath", "custom-hooks")
        refusal = subprocess.run(
            [os.fspath(repository.path / "scripts/install-git-hooks.sh")],
            cwd=nested,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertNotEqual(refusal.returncode, 0)
        configured = repository.git("config", "--local", "--get", "core.hooksPath")
        self.assertEqual(configured.stdout.strip(), b"custom-hooks")


if __name__ == "__main__":
    unittest.main(verbosity=2)
