#!/bin/bash

# Read-only verification of generated scheduler JNI libraries. This test does
# not build, rewrite, strip, sign, or otherwise mutate an artifact.

set -euo pipefail
IFS=$'\n\t'

readonly pinned_ndk_version='28.2.13676358'
readonly library_name='libdayweave_android_ffi.so'
readonly jni_symbol='Java_com_greengolddog_dayweave_scheduler_RustSchedulerNative_process'

fail() {
  printf 'Android scheduler native security test failed: %s\n' "$1" >&2
  exit 1
}

if test "$#" -ne 1; then
  fail 'usage: test-android-scheduler-library-native-security.sh <debug|release>'
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
readonly output_root="${repository_root}/apps/android/app/build/generated/jniLibs/${build_variant}"

sdk_candidate=''
if test -n "${ANDROID_HOME:-}"; then
  sdk_candidate="$ANDROID_HOME"
fi
if test -n "${ANDROID_SDK_ROOT:-}"; then
  if test -n "$sdk_candidate"; then
    android_home_physical="$(CDPATH= cd -- "$sdk_candidate" 2>/dev/null && /bin/pwd -P)" \
      || fail 'ANDROID_HOME is not an accessible directory'
    android_sdk_root_physical="$(CDPATH= cd -- "$ANDROID_SDK_ROOT" 2>/dev/null && /bin/pwd -P)" \
      || fail 'ANDROID_SDK_ROOT is not an accessible directory'
    test "$android_home_physical" = "$android_sdk_root_physical" \
      || fail 'ANDROID_HOME and ANDROID_SDK_ROOT identify different SDKs'
  else
    sdk_candidate="$ANDROID_SDK_ROOT"
  fi
fi
test -n "$sdk_candidate" \
  || fail 'ANDROID_HOME or ANDROID_SDK_ROOT must identify the Android SDK'
android_sdk_root="$(CDPATH= cd -- "$sdk_candidate" 2>/dev/null && /bin/pwd -P)" \
  || fail 'the Android SDK is not an accessible directory'
readonly android_sdk_root
readonly ndk_root="${android_sdk_root}/ndk/${pinned_ndk_version}"
test -d "$ndk_root" && test ! -L "$ndk_root" \
  || fail "Android NDK ${pinned_ndk_version} is required"

case "$(/usr/bin/uname -s)-$(/usr/bin/uname -m)" in
  Darwin-arm64|Darwin-x86_64) ndk_host_tag='darwin-x86_64' ;;
  Linux-x86_64) ndk_host_tag='linux-x86_64' ;;
  *) fail 'the host is not supported by the pinned Android NDK toolchain' ;;
esac
readonly llvm_bin="${ndk_root}/toolchains/llvm/prebuilt/${ndk_host_tag}/bin"
readonly readelf_path="${llvm_bin}/llvm-readelf"
readonly nm_path="${llvm_bin}/llvm-nm"
test -x "$readelf_path" || fail 'the pinned NDK llvm-readelf is missing'
test -x "$nm_path" || fail 'the pinned NDK llvm-nm is missing'
test -d "$output_root" && test ! -L "$output_root" \
  || fail 'the generated jniLibs directory is missing or linked'

assert_library() {
  local abi="$1"
  local expected_machine="$2"
  local library_path="${output_root}/${abi}/${library_name}"
  local header
  local dynamic
  local program_headers
  local symbols
  local load_line
  local alignment
  local alignment_digits
  local needed
  local needed_count=0

  test -f "$library_path" && test ! -L "$library_path" \
    || fail "the ${abi} library is missing or linked"
  header="$($readelf_path -hW "$library_path")" \
    || fail "the ${abi} ELF header could not be read"
  printf '%s\n' "$header" | /usr/bin/grep -Eq 'Class:[[:space:]]+ELF64$' \
    || fail "the ${abi} library is not ELF64"
  printf '%s\n' "$header" | /usr/bin/grep -Eq 'Data:[[:space:]]+2.s complement, little endian$' \
    || fail "the ${abi} library is not little-endian"
  printf '%s\n' "$header" | /usr/bin/grep -Eq 'Type:[[:space:]]+DYN ' \
    || fail "the ${abi} library is not a shared object"
  printf '%s\n' "$header" | /usr/bin/grep -Fq -- "Machine:                           ${expected_machine}" \
    || fail "the ${abi} library has the wrong machine type"

  dynamic="$($readelf_path -dW "$library_path")" \
    || fail "the ${abi} dynamic section could not be read"
  if printf '%s\n' "$dynamic" | /usr/bin/grep -Eq '\((RPATH|RUNPATH|TEXTREL)\)'; then
    fail "the ${abi} library contains a forbidden dynamic tag"
  fi
  printf '%s\n' "$dynamic" | /usr/bin/grep -Eq 'BIND_NOW|FLAGS_1.*NOW' \
    || fail "the ${abi} library does not enable immediate binding"
  printf '%s\n' "$dynamic" | /usr/bin/grep -Fq -- "Library soname: [${library_name}]" \
    || fail "the ${abi} library has an unexpected or missing SONAME"

  while IFS= read -r needed; do
    test -n "$needed" || continue
    needed_count=$((needed_count + 1))
    case "$needed" in
      libc.so|libdl.so|liblog.so|libm.so) ;;
      *) fail "the ${abi} library unexpectedly depends on ${needed}" ;;
    esac
  done < <(
    printf '%s\n' "$dynamic" \
      | /usr/bin/sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p'
  )
  test "$needed_count" -gt 0 \
    || fail "the ${abi} library has no recorded Android dependencies"

  program_headers="$($readelf_path -lW "$library_path")" \
    || fail "the ${abi} program headers could not be read"
  printf '%s\n' "$program_headers" | /usr/bin/grep -q 'GNU_RELRO' \
    || fail "the ${abi} library has no GNU_RELRO segment"
  if printf '%s\n' "$program_headers" | /usr/bin/grep -Eq 'GNU_STACK.*RWE'; then
    fail "the ${abi} library requests an executable stack"
  fi
  printf '%s\n' "$program_headers" | /usr/bin/grep -q 'GNU_STACK' \
    || fail "the ${abi} library has no GNU_STACK declaration"

  while IFS= read -r load_line; do
    test -n "$load_line" || continue
    alignment="${load_line##* }"
    case "$alignment" in
      0x*)
        alignment_digits="${alignment#0x}"
        case "$alignment_digits" in
          ''|*[!0-9a-fA-F]*)
            fail "the ${abi} library has an unreadable LOAD alignment"
            ;;
        esac
        ;;
      *) fail "the ${abi} library has an unreadable LOAD alignment" ;;
    esac
    if (( alignment < 0x4000 )); then
      fail "the ${abi} library is not compatible with 16 KiB pages"
    fi
  done < <(printf '%s\n' "$program_headers" | /usr/bin/awk '$1 == "LOAD"')

  symbols="$($nm_path -D --defined-only --format=posix "$library_path")" \
    || fail "the ${abi} dynamic symbols could not be read"
  test "$(printf '%s\n' "$symbols" | /usr/bin/awk -v symbol="$jni_symbol" '$1 == symbol { count++ } END { print count + 0 }')" -eq 1 \
    || fail "the ${abi} library does not export exactly one expected JNI symbol"
  test "$(printf '%s\n' "$symbols" | /usr/bin/awk '$1 ~ /^Java_/ { count++ } END { print count + 0 }')" -eq 1 \
    || fail "the ${abi} library exports an unexpected Java native method"
}

assert_library arm64-v8a AArch64
assert_library x86_64 'Advanced Micro Devices X86-64'

if /usr/bin/find "$output_root" -type l -print | /usr/bin/grep -q .; then
  fail 'the generated jniLibs tree contains a symbolic link'
fi
actual_libraries="$(
  /usr/bin/find "$output_root" -type f -name '*.so' -print | LC_ALL=C /usr/bin/sort
)"
expected_libraries="$(
  printf '%s\n' \
    "${output_root}/arm64-v8a/${library_name}" \
    "${output_root}/x86_64/${library_name}" \
    | LC_ALL=C /usr/bin/sort
)"
test "$actual_libraries" = "$expected_libraries" \
  || fail 'the generated jniLibs tree contains an unexpected native library or ABI'

printf 'Android scheduler native security checks passed for %s.\n' "$build_variant"
