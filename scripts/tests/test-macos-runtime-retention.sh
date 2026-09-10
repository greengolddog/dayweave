#!/bin/bash
set -euo pipefail
umask 077

# All toolchain and Swift invocations are mocked. This test never compiles,
# opens an app, or reads/modifies the installed developer frameworks.
fail() { printf 'macOS test runtime retention regression failed: %s\n' "$1" >&2; exit 1; }
dw_test_scripts=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
dw_test_root=$(mktemp -d /tmp/dayweave-test-macos-wrapper.XXXXXX)

cleanup() {
  local status=$?
  trap - EXIT HUP INT TERM
  if test -f "$dw_test_root/runtime-paths"; then
    while IFS= read -r runtime; do
      case "$runtime" in
        /tmp/dayweave-testing-frameworks.*)
          if test -d "$runtime" && test ! -L "$runtime"; then
            chmod -R u+rwX "$runtime"
            rm -rf -- "$runtime"
          fi
          ;;
        *) status=1 ;;
      esac
    done < "$dw_test_root/runtime-paths"
  fi
  case "$dw_test_root" in
    /tmp/dayweave-test-macos-wrapper.*)
      if test -d "$dw_test_root" && test ! -L "$dw_test_root"; then rm -rf -- "$dw_test_root"; fi ;;
    *) status=1 ;;
  esac
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir "$dw_test_root/bin" "$dw_test_root/developer" "$dw_test_root/system"
dw_framework="$dw_test_root/developer/Library/Developer/Frameworks/Testing.framework"
dw_overlay='Versions/A/Modules/Testing.swiftcrossimport/Foundation.swiftoverlay'
mkdir -p "$dw_framework/${dw_overlay%/*}"
printf '%s\n' 'Synthetic dangling overlay' > "$dw_framework/$dw_overlay"

printf '%s\n' '#!/bin/bash' 'set -euo pipefail' \
  'test "$#" = 1 && test "$1" = -p' \
  'printf "%s\n" invoked > "$DW_MOCK_CASE/xcode-called"' \
  'printf "%s\n" "$DW_MOCK_DEVELOPER"' > "$dw_test_root/bin/xcode-select"
printf '%s\n' '#!/bin/bash' 'set -euo pipefail' \
  'printf "%s\n" "$@" > "$DW_MOCK_CASE/swift-args"' \
  'for argument in "$@"; do' \
  '  case "$argument" in' \
  '    /tmp/dayweave-testing-frameworks.*)' \
  '      printf "%s\n" "$argument" >> "$DW_MOCK_ROOT/runtime-paths"' \
  '      printf "%s\n" "$argument" > "$DW_MOCK_CASE/runtime" ;;' \
  '  esac' \
  'done' \
  'case "$DW_MOCK_RESULT" in' \
  '  success|exit-interrupt|exit-terminate|exit-hangup) printf "%s\n" "Synthetic Swift success" ;;' \
  '  failure) exit 23 ;;' \
  '  interrupt) kill -INT "$PPID" ;;' \
  '  terminate) kill -TERM "$PPID" ;;' \
  '  hangup) kill -HUP "$PPID" ;;' \
  '  *) exit 97 ;;' \
  'esac' > "$dw_test_root/bin/swift"
chmod 700 "$dw_test_root/bin/xcode-select" "$dw_test_root/bin/swift"

# Interpose only the retained EXIT guard, after Swift succeeds and its receipt
# is printed. This deterministically exercises a signal inside an EXIT trap;
# timing-based sleeps cannot reliably reach that boundary. The helper is sent
# solely to our mocked wrapper invocation, never to an installed toolchain.
test() {
  if [[ "${FUNCNAME[1]:-}" == cleanup && "${dw_testing_retained:-0}" == 1 ]]; then
    case "${DW_MOCK_RESULT:-}" in
      exit-interrupt) kill -INT "$$" ;;
      exit-terminate) kill -TERM "$$" ;;
      exit-hangup) kill -HUP "$$" ;;
    esac
  fi
  builtin test "$@"
}
export -f test

run_case() {
  local name=$1 retain=$2 result=$3 developer=$4
  dw_case="$dw_test_root/$name"
  mkdir "$dw_case"
  local -a command=(env -u DAYWEAVE_TESTING_RETAIN_RUNTIME)
  if test "$retain" != absent; then command+=("DAYWEAVE_TESTING_RETAIN_RUNTIME=$retain"); fi
  set +e
  "${command[@]}" PATH="$dw_test_root/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    DW_MOCK_ROOT="$dw_test_root" DW_MOCK_CASE="$dw_case" DW_MOCK_RESULT="$result" \
    DW_MOCK_DEVELOPER="$developer" \
    /bin/bash "$dw_test_scripts/test-macos.sh" --skip-build --filter SyntheticCase \
    > "$dw_case/output" 2>&1
  dw_status=$?
  set -e
}

assert_no_receipt() {
  if rg -q '^DAYWEAVE_TESTING_RUNTIME=' "$dw_case/output"; then fail 'failure/default invocation emitted a runtime receipt'; fi
}

assert_removed() {
  local runtime
  IFS= read -r runtime < "$dw_case/runtime"
  test ! -e "$runtime" && test ! -L "$runtime" || fail 'failed/default invocation retained its copy'
}

for setting in absent 0; do
  run_case "default-$setting" "$setting" success "$dw_test_root/developer"
  test "$dw_status" = 0 || fail 'default invocation failed'
  assert_no_receipt
  assert_removed
done

run_case retained 1 success "$dw_test_root/developer"
test "$dw_status" = 0 || fail 'opt-in success failed'
test "$(rg -c '^DAYWEAVE_TESTING_RUNTIME=' "$dw_case/output")" = 1 || fail 'expected exactly one receipt'
dw_receipt=$(sed -n 's/^DAYWEAVE_TESTING_RUNTIME=//p' "$dw_case/output")
IFS= read -r dw_runtime < "$dw_case/runtime"
test "$dw_receipt" = "$dw_runtime" || fail 'receipt did not name the invocation copy'
case "$dw_receipt" in /tmp/dayweave-testing-frameworks.*) ;; *) fail 'unsafe receipt path' ;; esac
test -d "$dw_receipt/Testing.framework" && test ! -L "$dw_receipt" || fail 'successful copy was not retained'
test "$(stat -f %Lp "$dw_receipt")" = 700 || fail 'retained runtime is not private'
test ! -e "$dw_receipt/Testing.framework/$dw_overlay" || fail 'dangling copied overlay was not removed'
test -f "$dw_framework/$dw_overlay" || fail 'source framework was modified'
rg -qx -- '--no-parallel' "$dw_case/swift-args" || fail 'serialization flag lost'
rg -qx -- '--skip-build' "$dw_case/swift-args" || fail 'caller arguments lost'
rg -qx -- '-rpath' "$dw_case/swift-args" || fail 'generated runtime is not embedded in rpath'

for result in failure interrupt terminate hangup; do
  run_case "$result" 1 "$result" "$dw_test_root/developer"
  case "$result" in failure) expected=23 ;; interrupt) expected=130 ;; terminate) expected=143 ;; hangup) expected=129 ;; esac
  test "$dw_status" = "$expected" || fail "$result did not preserve nonzero exit semantics"
  assert_no_receipt
  assert_removed
done

for result in exit-interrupt exit-terminate exit-hangup; do
  run_case "$result" 1 "$result" "$dw_test_root/developer"
  case "$result" in exit-interrupt) expected=130 ;; exit-terminate) expected=143 ;; exit-hangup) expected=129 ;; esac
  test "$dw_status" = "$expected" || fail "$result lost the interrupt exit status"
  # A printed line is not a completed handoff. The driver must also require
  # terminal exit zero; a subsequent signal removes the already-named copy.
  test "$(rg -c '^DAYWEAVE_TESTING_RUNTIME=' "$dw_case/output")" = 1 || fail 'EXIT boundary was not exercised'
  assert_removed
done

for setting in '' true 2 '/tmp/caller-selected-runtime'; do
  run_case "invalid-${#setting}" "$setting" success "$dw_test_root/developer"
  test "$dw_status" != 0 || fail 'invalid opt-in value was accepted'
  test ! -e "$dw_case/xcode-called" && test ! -e "$dw_case/swift-args" || fail 'invalid value reached toolchain discovery'
  assert_no_receipt
done

run_case system-default absent success "$dw_test_root/system"
test "$dw_status" = 0 || fail 'default system fallback failed'
assert_no_receipt
test ! -e "$dw_case/runtime" || fail 'system fallback created a copy'
run_case system-retained 1 success "$dw_test_root/system"
test "$dw_status" = 0 || fail 'opt-in system fallback failed'
test "$(rg -c '^DAYWEAVE_TESTING_RUNTIME=' "$dw_case/output")" = 1 || fail 'system fallback receipt count changed'
rg -qx 'DAYWEAVE_TESTING_RUNTIME=system' "$dw_case/output" || fail 'system fallback receipt is wrong'
test ! -e "$dw_case/runtime" || fail 'opt-in system fallback created a copy'
run_case system-failure 1 failure "$dw_test_root/system"
test "$dw_status" = 23 || fail 'system fallback failure status changed'
assert_no_receipt

printf '%s\n' 'macOS test runtime retention regression: PASS (17 mocked invocations)'
