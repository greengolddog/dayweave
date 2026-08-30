#!/usr/bin/env python3
"""Fail closed unless Android release signing inputs are private and outside Git."""

from __future__ import annotations

import argparse
import os
import re
import stat
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path


class BoundaryError(Exception):
    """A sanitized signing-boundary failure safe to print to a build log."""


def fail(message: str) -> None:
    raise BoundaryError(message)


def require_supported_path(candidate: str, description: str) -> None:
    if not candidate or any(character in candidate for character in "\n\r\t\0"):
        fail(f"{description} path is invalid.")


def require_private_regular_file(candidate: str, description: str) -> None:
    try:
        metadata = os.lstat(candidate)
    except OSError:
        fail(f"{description} must be a regular, non-symlink file.")
    if not stat.S_ISREG(metadata.st_mode):
        fail(f"{description} must be a regular, non-symlink file.")
    if metadata.st_nlink != 1:
        fail(f"{description} must not have hard-linked aliases.")
    if stat.S_IMODE(metadata.st_mode) != 0o600:
        fail(f"{description} must have mode 0600.")


def git_metadata_path(repo_root: str, option: str) -> str:
    try:
        result = subprocess.run(
            [
                "git",
                "-C",
                repo_root,
                "rev-parse",
                "--path-format=absolute",
                option,
            ],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        fail("Unable to resolve the Git signing boundary.")
    resolved = result.stdout.rstrip("\n")
    require_supported_path(resolved, "Git metadata")
    return resolved


def ancestors(candidate: str) -> Iterator[str]:
    current = os.path.abspath(candidate)
    while True:
        yield current
        parent = os.path.dirname(current)
        if parent == current:
            return
        current = parent


def same_file(left: str, right: str) -> bool:
    try:
        return os.path.samefile(left, right)
    except OSError:
        return False


def path_enters_boundary(candidate: str, boundary: str) -> bool:
    # The lexical walk catches a path that starts inside Git and exits through a
    # symlink. The resolved walk catches an external spelling that enters Git.
    return any(same_file(part, boundary) for part in ancestors(candidate)) or any(
        same_file(part, boundary) for part in ancestors(os.path.realpath(candidate))
    )


def require_outside_git(
    candidate: str,
    description: str,
    boundaries: tuple[str, ...],
) -> None:
    if any(path_enters_boundary(candidate, boundary) for boundary in boundaries):
        fail(f"{description} must be outside the Git worktree and metadata.")


def logical_lines(text: str) -> Iterator[str]:
    pending = ""
    continuing = False
    for physical in re.split(r"\r\n?|\n", text):
        if continuing:
            physical = physical.lstrip(" \t\f")
        line = pending + physical
        trailing_slashes = len(line) - len(line.rstrip("\\"))
        if trailing_slashes % 2 == 1:
            pending = line[:-1]
            continuing = True
            continue
        yield line
        pending = ""
        continuing = False
    if continuing:
        yield pending


def decode_property(value: str) -> str:
    result: list[str] = []
    index = 0
    escapes = {"t": "\t", "n": "\n", "r": "\r", "f": "\f"}
    while index < len(value):
        current = value[index]
        if current != "\\":
            result.append(current)
            index += 1
            continue
        index += 1
        if index == len(value):
            raise ValueError("dangling escape")
        escaped = value[index]
        index += 1
        if escaped == "u":
            if index + 4 > len(value):
                raise ValueError("short unicode escape")
            result.append(chr(int(value[index : index + 4], 16)))
            index += 4
        else:
            result.append(escapes.get(escaped, escaped))
    return "".join(result)


def split_property(line: str) -> tuple[str, str] | None:
    start = 0
    while start < len(line) and line[start] in " \t\f":
        start += 1
    if start == len(line) or line[start] in "#!":
        return None

    escaped = False
    separator = len(line)
    separator_is_whitespace = False
    for index in range(start, len(line)):
        current = line[index]
        if current == "\\":
            escaped = not escaped
            continue
        if not escaped and (current in "=:" or current in " \t\f"):
            separator = index
            separator_is_whitespace = current in " \t\f"
            break
        escaped = False

    value_start = separator
    if value_start < len(line):
        if separator_is_whitespace:
            while value_start < len(line) and line[value_start] in " \t\f":
                value_start += 1
            if value_start < len(line) and line[value_start] in "=:":
                value_start += 1
        else:
            value_start += 1
        while value_start < len(line) and line[value_start] in " \t\f":
            value_start += 1
    return decode_property(line[start:separator]), decode_property(line[value_start:])


def read_keystore_path(properties_path: str) -> str:
    try:
        text = Path(properties_path).read_bytes().decode("iso-8859-1")
        store_file: str | None = None
        for logical in logical_lines(text):
            parsed = split_property(logical)
            if parsed is not None and parsed[0] == "storeFile":
                store_file = parsed[1]
    except (OSError, UnicodeError, ValueError):
        fail("Signing properties must define a valid storeFile.")
    if not store_file:
        fail("Signing properties must define a valid storeFile.")
    return store_file


def validate(repo_root: str, properties_path: str, keystore_base: str) -> None:
    require_supported_path(repo_root, "Git worktree")
    require_supported_path(properties_path, "Signing properties")
    require_supported_path(keystore_base, "Android release keystore base")
    boundaries = (
        repo_root,
        git_metadata_path(repo_root, "--git-dir"),
        git_metadata_path(repo_root, "--git-common-dir"),
    )

    require_private_regular_file(properties_path, "Signing properties")
    require_outside_git(properties_path, "Signing properties", boundaries)
    keystore_path = read_keystore_path(properties_path)
    require_supported_path(keystore_path, "Android release keystore")
    if not os.path.isabs(keystore_path):
        keystore_path = os.path.join(keystore_base, keystore_path)
    require_private_regular_file(keystore_path, "Android release keystore")
    require_outside_git(keystore_path, "Android release keystore", boundaries)


def main() -> int:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--repo-root", required=True)
    parser.add_argument("--properties", required=True)
    parser.add_argument("--keystore-base", required=True)
    arguments = parser.parse_args()
    try:
        validate(
            repo_root=arguments.repo_root,
            properties_path=arguments.properties,
            keystore_base=arguments.keystore_base,
        )
    except BoundaryError as error:
        print(error, file=sys.stderr)
        return 1
    except Exception:
        print("Unable to validate the Android signing boundary.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
