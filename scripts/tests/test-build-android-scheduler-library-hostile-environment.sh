#!/bin/bash

# Builds through deliberately hostile ambient Rust controls. The JNI crate's compile-time panic
# strategy assertion and the builder's explicit environment must still produce verified ELFs.

set -euo pipefail
IFS=$'\n\t'
umask 077

fail() {
  printf 'Android scheduler hostile-environment regression failed: %s\n' "$1" >&2
  exit 1
}

if test "$#" -ne 1; then
  fail 'usage: test-build-android-scheduler-library-hostile-environment.sh <debug|release>'
fi
readonly build_variant="$1"
case "$build_variant" in
  debug|release) ;;
  *) fail 'the build variant must be debug or release' ;;
esac

readonly script_directory="$(
  CDPATH= cd -- "$(/usr/bin/dirname -- "${BASH_SOURCE[0]}")" && /bin/pwd -P
)"
readonly repository_root="$(CDPATH= cd -- "${script_directory}/../.." && /bin/pwd -P)"
readonly builder="${repository_root}/scripts/build-android-scheduler-library.sh"
test -x "$builder" || fail 'the Android scheduler builder is not executable'

temporary_root="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/dayweave-android-env.XXXXXXXX")" \
  || fail 'a private temporary directory could not be created'
cleanup() {
  /bin/rm -rf -- "$temporary_root"
}
trap cleanup EXIT HUP INT TERM
/bin/mkdir -m 0700 -- "${temporary_root}/cargo-home"
printf '%s\n' '[build]' 'rustc = "/bin/false"' \
  >"${temporary_root}/cargo-home/config.toml"

set +e
CARGO_HOME="${temporary_root}/cargo-home" "$builder" "$build_variant" \
  >"${temporary_root}/config-rejection.log" 2>&1
config_status=$?
set -e
test "$config_status" -ne 0 || fail 'an ambient CARGO_HOME config was accepted'
/usr/bin/grep -Fq -- 'ambient Cargo configuration is not permitted' \
  "${temporary_root}/config-rejection.log" \
  || fail 'ambient Cargo configuration did not fail with the fixed diagnostic'

/bin/mkdir -m 0700 -p -- "${temporary_root}/home/.cargo"
printf '%s\n' '[build]' 'rustc = "/bin/false"' \
  >"${temporary_root}/home/.cargo/config.toml"
set +e
/usr/bin/env -u CARGO_HOME HOME="${temporary_root}/home" \
  "$builder" "$build_variant" >"${temporary_root}/default-config-rejection.log" 2>&1
default_config_status=$?
set -e
test "$default_config_status" -ne 0 || fail 'the default HOME Cargo config was accepted'
/usr/bin/grep -Fq -- 'ambient Cargo configuration is not permitted' \
  "${temporary_root}/default-config-rejection.log" \
  || fail 'the default HOME Cargo config did not fail with the fixed diagnostic'

CARGO_BUILD_RUSTC=/bin/false \
CARGO_BUILD_RUSTC_WRAPPER=/bin/false \
CARGO_PROFILE_DEV_PANIC=abort \
CARGO_PROFILE_RELEASE_PANIC=abort \
RUSTC=/bin/false \
RUSTC_BOOTSTRAP=1 \
RUSTC_WRAPPER=/bin/false \
RUSTC_WORKSPACE_WRAPPER=/bin/false \
RUSTFLAGS='--definitely-invalid-rustflag' \
"$builder" "$build_variant"

printf 'Android scheduler hostile-environment regression passed for %s.\n' "$build_variant"
