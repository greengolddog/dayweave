#!/bin/bash

# Builds and verifies the dormant arm64 scheduler process bridge. This script
# intentionally leaves the helper under ignored target/ output; app packaging
# must not copy it until the Swift integration contract is implemented.

set -euo pipefail
IFS=$'\n\t'
umask 077

fail() {
  printf 'Scheduler helper build failed: %s\n' "$1" >&2
  exit 1
}

script_directory="$(
  CDPATH= cd -- "$(/usr/bin/dirname -- "${BASH_SOURCE[0]}")" && pwd -P
)" || fail 'the script directory could not be resolved'
readonly script_directory
repository_root="$(
  CDPATH= cd -- "${script_directory}/.." && pwd -P
)" || fail 'the repository root could not be resolved'
readonly repository_root
readonly target_triple='aarch64-apple-darwin'
readonly deployment_target='15.0'
readonly helper_path="${repository_root}/target/${target_triple}/release/dayweave-scheduler-helper"

test "$(/usr/bin/uname -s)" = Darwin || fail 'the host must be macOS'
test "$(/usr/bin/uname -m)" = arm64 || fail 'the host must be Apple Silicon'

for trusted_tool in \
  /usr/bin/awk /usr/bin/codesign /usr/bin/dirname /usr/bin/lipo /usr/bin/otool \
  /usr/bin/shasum /usr/bin/uname /usr/bin/vtool
do
  test -x "$trusted_tool" || fail "missing trusted tool ${trusted_tool}"
done
command -v cargo >/dev/null 2>&1 || fail 'cargo is unavailable'
command -v rustc >/dev/null 2>&1 || fail 'rustc is unavailable'
command -v rustup >/dev/null 2>&1 || fail 'rustup is unavailable'

rustc_version="$(
  cd "$repository_root"
  rustc --version
)" || fail 'rustc version could not be determined'
readonly rustc_version
case "$rustc_version" in
  'rustc 1.95.0 '*) ;;
  *) fail 'rustc must match the repository pin at 1.95.0' ;;
esac
installed_targets="$(
  cd "$repository_root"
  rustup target list --installed
)" || fail 'installed Rust targets could not be determined'
readonly installed_targets
case $'\n'"$installed_targets"$'\n' in
  *$'\n'"$target_triple"$'\n'*) ;;
  *) fail "the ${target_triple} Rust target is not installed" ;;
esac

(
  cd "$repository_root"
  CARGO_TARGET_DIR="${repository_root}/target" \
    MACOSX_DEPLOYMENT_TARGET="$deployment_target" \
    cargo build --locked --release \
      --target "$target_triple" \
      --package dayweave-scheduler-helper
)

test -f "$helper_path" && test ! -L "$helper_path" && test -x "$helper_path" \
  || fail 'cargo did not produce a regular executable helper'
test "$(/usr/bin/lipo -archs "$helper_path")" = arm64 \
  || fail 'the helper is not a single-architecture arm64 executable'

build_metadata="$(/usr/bin/vtool -show-build "$helper_path")" \
  || fail 'the helper Mach-O build metadata could not be read'
readonly build_metadata
platform_matches="$(
  /usr/bin/awk '$1 == "platform" && $2 == "MACOS" { count += 1 } END { print count + 0 }' \
    <<<"$build_metadata"
)"
platform_entries="$(
  /usr/bin/awk '$1 == "platform" { count += 1 } END { print count + 0 }' \
    <<<"$build_metadata"
)"
minimum_os_matches="$(
  /usr/bin/awk -v expected="$deployment_target" \
    '$1 == "minos" && $2 == expected { count += 1 } END { print count + 0 }' \
    <<<"$build_metadata"
)"
minimum_os_entries="$(
  /usr/bin/awk '$1 == "minos" { count += 1 } END { print count + 0 }' \
    <<<"$build_metadata"
)"
test "$platform_matches" = 1 && test "$platform_entries" = 1 \
  || fail 'the helper does not declare exactly one macOS build platform'
test "$minimum_os_matches" = 1 && test "$minimum_os_entries" = 1 \
  || fail "the helper does not declare exactly one ${deployment_target} minimum OS"

unexpected_libraries="$(
  /usr/bin/otool -L "$helper_path" \
    | /usr/bin/awk '
        NR > 1 {
          library = $1
          if (library !~ /^\/usr\/lib\// && library !~ /^\/System\/Library\//) {
            print library
          }
        }
      '
)"
test -z "$unexpected_libraries" \
  || fail 'the helper links a library outside the macOS system locations'

# The final app will sign nested code before its outer bundle. For this dormant
# build gate, an ad-hoc signature proves the standalone Mach-O is signable.
/usr/bin/codesign --force --sign - --timestamp=none "$helper_path" >/dev/null
/usr/bin/codesign --verify --strict "$helper_path" >/dev/null 2>&1 \
  || fail 'the helper does not pass strict code-signature verification'

printf 'Built and verified %s\n' "$helper_path"
/usr/bin/shasum -a 256 "$helper_path"
