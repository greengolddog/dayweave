#!/bin/bash

# Installs the exact signed Codex runtime consumed by the macOS bundle gate.
# This is intentionally restricted to GitHub's ephemeral Apple Silicon runner:
# developers should use the normal Codex installer on their own machines.

set -euo pipefail
IFS=$'\n\t'
umask 077

fail() {
  printf 'Codex CI runtime installation failed: %s\n' "$1" >&2
  exit 1
}

readonly archive_url='https://github.com/openai/codex/releases/download/rust-v0.150.1/codex-package-aarch64-apple-darwin.tar.gz'
readonly archive_sha256='3ecaec1e7dd7873fac5e505533618a92a7e3bf12de7869b6130c0e3cc7faf677'
readonly binary_sha256='a14f9a907c12c8812878b70e6b7d65f81c39ed795513e46a55817d7428c0ca6b'
readonly signing_requirement='identifier "codex" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "2DC432GLL2"'
readonly target_root='/opt/homebrew/Caskroom/codex/0.150.1'
readonly target_directory="${target_root}/bin"
readonly target_binary="${target_directory}/codex"

test "${GITHUB_ACTIONS:-}" = true \
  || fail 'this installer may run only in GitHub Actions'
test "${RUNNER_OS:-}" = macOS \
  || fail 'the runner must report macOS'
test "${RUNNER_ARCH:-}" = ARM64 \
  || fail 'the runner must report ARM64'
test "$(/usr/bin/uname -m)" = arm64 \
  || fail 'the host architecture must be arm64'

for trusted_tool in \
  /usr/bin/awk /usr/bin/codesign /usr/bin/curl /usr/bin/mktemp \
  /usr/bin/shasum /usr/bin/tar /bin/chmod /bin/cp /bin/ln \
  /bin/mkdir /bin/rm
do
  test -x "$trusted_tool" || fail "missing trusted tool ${trusted_tool}"
done

sha256_file() {
  /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{print $1}'
}

verify_binary() {
  local candidate=$1
  test -f "$candidate" && test ! -L "$candidate" && test -x "$candidate" \
    || fail 'the runtime candidate is not a regular executable'
  test "$(sha256_file "$candidate")" = "$binary_sha256" \
    || fail 'the runtime executable does not match the pinned digest'
  /usr/bin/codesign --verify --strict -R="$signing_requirement" \
    "$candidate" >/dev/null 2>&1 \
    || fail 'the runtime executable does not match the pinned Developer ID'
}

if test -e "$target_binary" || test -L "$target_binary"; then
  verify_binary "$target_binary"
  printf '%s\n' 'The exact pinned Codex runtime is already installed.'
  exit 0
fi

for trusted_parent in /opt /opt/homebrew /opt/homebrew/Caskroom; do
  test -d "$trusted_parent" && test ! -L "$trusted_parent" \
    || fail "unsafe installation parent ${trusted_parent}"
done

runner_temp=${RUNNER_TEMP:-}
test -n "$runner_temp" && test -d "$runner_temp" && test ! -L "$runner_temp" \
  || fail 'RUNNER_TEMP must be an existing non-symlink directory'
case "$runner_temp" in
  /*) ;;
  *) fail 'RUNNER_TEMP must be absolute' ;;
esac
runner_temp=${runner_temp%/}

temporary_root=$(/usr/bin/mktemp -d "${runner_temp}/dayweave-codex.XXXXXX")
case "$temporary_root" in
  "${runner_temp}"/dayweave-codex.*) ;;
  *) fail 'mktemp returned a path outside RUNNER_TEMP' ;;
esac
readonly temporary_root

cleanup() {
  case "$temporary_root" in
    "${runner_temp}"/dayweave-codex.*)
      /bin/rm -rf -- "$temporary_root"
      ;;
    *)
      printf '%s\n' 'Refusing to clean an unexpected temporary path.' >&2
      ;;
  esac
}
trap cleanup EXIT INT TERM HUP

readonly archive_path="${temporary_root}/codex.tar.gz"
readonly extraction_root="${temporary_root}/extracted"
/bin/mkdir "$extraction_root"

/usr/bin/curl \
  --fail \
  --location \
  --proto '=https' \
  --proto-redir '=https' \
  --retry 3 \
  --retry-delay 2 \
  --connect-timeout 20 \
  --max-time 300 \
  --output "$archive_path" \
  "$archive_url"

test "$(sha256_file "$archive_path")" = "$archive_sha256" \
  || fail 'the downloaded archive does not match the pinned digest'

/usr/bin/tar -xzf "$archive_path" -C "$extraction_root" bin/codex
readonly extracted_binary="${extraction_root}/bin/codex"
/bin/chmod 0555 "$extracted_binary"
verify_binary "$extracted_binary"

/bin/mkdir -p "$target_directory"
for installed_parent in \
  /opt/homebrew/Caskroom/codex \
  "$target_root" \
  "$target_directory"
do
  test -d "$installed_parent" && test ! -L "$installed_parent" \
    || fail "unsafe installation directory ${installed_parent}"
done

readonly staged_binary="${target_directory}/.dayweave-codex-${GITHUB_RUN_ID:-unknown}-${GITHUB_RUN_ATTEMPT:-unknown}"
test ! -e "$staged_binary" && test ! -L "$staged_binary" \
  || fail 'the staging path already exists'
test ! -e "$target_binary" && test ! -L "$target_binary" \
  || fail 'the target appeared during installation'

/bin/cp "$extracted_binary" "$staged_binary"
/bin/chmod 0555 "$staged_binary"
verify_binary "$staged_binary"
# A hard link creates the final name atomically and refuses to replace a file
# that appeared after the checks above.
/bin/ln "$staged_binary" "$target_binary"
/bin/rm "$staged_binary"
verify_binary "$target_binary"

printf '%s\n' 'Installed the exact pinned Codex runtime for the macOS bundle gate.'
