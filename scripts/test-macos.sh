#!/bin/bash
set -euo pipefail

fail() {
  printf 'macOS test setup failed: %s\n' "$1" >&2
  exit 1
}

dw_retain_runtime="${DAYWEAVE_TESTING_RETAIN_RUNTIME-0}"
case "$dw_retain_runtime" in
  0|1) ;;
  *) fail 'DAYWEAVE_TESTING_RETAIN_RUNTIME must be absent, 0, or 1' ;;
esac

dw_script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
dw_package_dir="$dw_script_dir/../apps/macos"
dw_testing_copy=""
dw_testing_retained=0

cleanup() {
  if test "$dw_testing_retained" = 1; then
    return
  fi
  case "${dw_testing_copy:-}" in
    /tmp/dayweave-testing-frameworks.*)
      chmod -R u+rwX "$dw_testing_copy" 2>/dev/null || true
      rm -rf -- "$dw_testing_copy"
      ;;
  esac
}
interrupted() {
  local dw_signal_status=$1
  # EXIT traps do not re-enter when a signal interrupts the EXIT handler itself.
  # Clean directly, and do not let a second catchable signal skip that cleanup.
  trap - EXIT
  trap '' HUP INT TERM
  dw_testing_retained=0
  cleanup
  exit "$dw_signal_status"
}
trap cleanup EXIT
trap 'interrupted 129' HUP
trap 'interrupted 130' INT
trap 'interrupted 143' TERM

# macOS 26.x CLT can type-check `import Testing` while still omitting Testing
# from the test runner's runtime search paths. Always use an isolated framework
# copy when the developer toolchain provides one; this covers both that runtime
# defect and the dangling _Testing_Foundation cross-import overlay without ever
# modifying the installed toolchain.
dw_developer_dir=$(xcode-select -p)
dw_framework_root="$dw_developer_dir/Library/Developer/Frameworks"
dw_testing_source="$dw_framework_root/Testing.framework"
if test ! -d "$dw_testing_source"; then
  # Swift Testing otherwise schedules independent suites concurrently even when
  # SwiftPM reports its legacy XCTest default as non-parallel. Many DayWeave
  # integration tests intentionally share the main actor, so serialize the
  # runner to prevent unrelated suites from starving their deterministic gates.
  if test "$dw_retain_runtime" = 0; then
    exec swift test --package-path "$dw_package_dir" --no-parallel "$@"
  fi
  swift test --package-path "$dw_package_dir" --no-parallel "$@"
  printf '%s\n' 'DAYWEAVE_TESTING_RUNTIME=system'
  exit 0
fi

dw_testing_copy=$(mktemp -d /tmp/dayweave-testing-frameworks.XXXXXX)
cp -R "$dw_testing_source" "$dw_testing_copy/"
dw_dangling_overlay="$dw_testing_copy/Testing.framework/Versions/A/Modules/Testing.swiftcrossimport/Foundation.swiftoverlay"
dw_foundation_modules="$dw_framework_root/_Testing_Foundation.framework/Modules"
if test -f "$dw_dangling_overlay" && test ! -d "$dw_foundation_modules"; then
  unlink "$dw_dangling_overlay"
fi

dw_runtime_frameworks="$dw_testing_copy"
if test -n "${DYLD_FRAMEWORK_PATH:-}"; then
  dw_runtime_frameworks="$dw_runtime_frameworks:$DYLD_FRAMEWORK_PATH"
fi

DYLD_FRAMEWORK_PATH="$dw_runtime_frameworks" \
  swift test \
    --package-path "$dw_package_dir" \
    --no-parallel \
    -Xswiftc -F \
    -Xswiftc "$dw_testing_copy" \
    -Xlinker -F \
    -Xlinker "$dw_testing_copy" \
    -Xlinker -rpath \
    -Xlinker "$dw_testing_copy" \
    "$@"

if test "$dw_retain_runtime" = 1; then
  # A successful prebuild may be reused with --skip-build while its embedded
  # rpath remains present. The caller owns cleanup of this exact generated
  # directory; no caller-provided runtime path is ever accepted by this wrapper.
  printf 'DAYWEAVE_TESTING_RUNTIME=%s\n' "$dw_testing_copy"
  dw_testing_retained=1
fi
