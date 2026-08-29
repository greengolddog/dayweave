#!/bin/bash
set -euo pipefail

fail() {
  printf 'macOS test setup failed: %s\n' "$1" >&2
  exit 1
}

dw_script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
dw_package_dir="$dw_script_dir/../apps/macos"
dw_testing_copy=""

cleanup() {
  case "${dw_testing_copy:-}" in
    /tmp/dayweave-testing-frameworks.*)
      chmod -R u+rwX "$dw_testing_copy" 2>/dev/null || true
      rm -rf -- "$dw_testing_copy"
      ;;
  esac
}
trap cleanup EXIT INT TERM

# macOS 26.x CLT can type-check `import Testing` while still omitting Testing
# from the test runner's runtime search paths. Always use an isolated framework
# copy when the developer toolchain provides one; this covers both that runtime
# defect and the dangling _Testing_Foundation cross-import overlay without ever
# modifying the installed toolchain.
dw_developer_dir=$(xcode-select -p)
dw_framework_root="$dw_developer_dir/Library/Developer/Frameworks"
dw_testing_source="$dw_framework_root/Testing.framework"
if test ! -d "$dw_testing_source"; then
  exec swift test --package-path "$dw_package_dir" "$@"
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
    -Xswiftc -F \
    -Xswiftc "$dw_testing_copy" \
    -Xlinker -F \
    -Xlinker "$dw_testing_copy" \
    -Xlinker -rpath \
    -Xlinker "$dw_testing_copy" \
    "$@"
