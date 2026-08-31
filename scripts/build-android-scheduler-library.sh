#!/bin/bash

# Cross-compiles the bounded scheduler JNI bridge into variant-scoped,
# generated Android jniLibs directories. No native artifact is written into a
# tracked source directory.

set -euo pipefail
IFS=$'\n\t'
umask 077

readonly pinned_ndk_version='28.2.13676358'
readonly android_api_level='28'
readonly library_name='libdayweave_android_ffi.so'

fail() {
  printf 'Android scheduler library build failed: %s\n' "$1" >&2
  exit 1
}

if test "$#" -ne 1; then
  fail 'usage: build-android-scheduler-library.sh <debug|release>'
fi
readonly build_variant="$1"
case "$build_variant" in
  debug)
    cargo_profile='debug'
    # Keep this non-empty: macOS Bash 3.2 treats an empty array expansion as
    # unbound under `set -u` even when written as "${array[@]}".
    cargo_profile_arguments=(--locked)
    ;;
  release)
    cargo_profile='release'
    cargo_profile_arguments=(--locked --release)
    ;;
  *) fail 'the build variant must be debug or release' ;;
esac
readonly cargo_profile

readonly script_directory="$(
  CDPATH= cd -- "$(/usr/bin/dirname -- "${BASH_SOURCE[0]}")" && /bin/pwd -P
)"
readonly repository_root="$(CDPATH= cd -- "${script_directory}/.." && /bin/pwd -P)"
readonly output_root="${repository_root}/apps/android/app/build/generated/jniLibs/${build_variant}"
readonly cargo_target_root="${repository_root}/target/android-ffi"

ensure_unlinked_directory_path() {
  local candidate_path="$1"
  local label="$2"
  local component
  local remaining
  local walked_path=''

  case "$candidate_path" in
    /*) ;;
    *) fail "${label} must be absolute" ;;
  esac
  remaining="${candidate_path#/}"
  while test -n "$remaining"; do
    component="${remaining%%/*}"
    if test "$component" = "$remaining"; then
      remaining=''
    else
      remaining="${remaining#*/}"
    fi
    case "$component" in
      ''|'.') continue ;;
      '..') fail "${label} must not contain parent traversal" ;;
    esac
    walked_path="${walked_path}/${component}"
    if test -L "$walked_path"; then
      fail "${label} must not contain symbolic links"
    fi
    if test -e "$walked_path" && test ! -d "$walked_path"; then
      fail "${label} contains a non-directory path component"
    fi
  done
}

ensure_directory() {
  local candidate_path="$1"
  local label="$2"

  ensure_unlinked_directory_path "$candidate_path" "$label"
  /bin/mkdir -p -m 0700 -- "$candidate_path" \
    || fail "${label} could not be created"
  test -d "$candidate_path" && test ! -L "$candidate_path" \
    || fail "${label} is not a regular directory"
}

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
  || fail "Android NDK ${pinned_ndk_version} is required at ${ndk_root}"

case "$(/usr/bin/uname -s)-$(/usr/bin/uname -m)" in
  Darwin-arm64|Darwin-x86_64) ndk_host_tag='darwin-x86_64' ;;
  Linux-x86_64) ndk_host_tag='linux-x86_64' ;;
  *) fail 'the host is not supported by the pinned Android NDK toolchain' ;;
esac
readonly ndk_host_tag
readonly llvm_bin="${ndk_root}/toolchains/llvm/prebuilt/${ndk_host_tag}/bin"
test -d "$llvm_bin" || fail 'the pinned NDK LLVM toolchain is incomplete'
test -x "${llvm_bin}/llvm-ar" || fail 'the pinned NDK llvm-ar is missing'

command -v cargo >/dev/null 2>&1 || fail 'cargo is required'
command -v rustup >/dev/null 2>&1 || fail 'rustup is required'
readonly cargo_path="$(command -v cargo)"
readonly rustup_path="$(command -v rustup)"
case "$cargo_path" in
  /*) ;;
  *) fail 'cargo must resolve to an absolute executable path' ;;
esac
case "$rustup_path" in
  /*) ;;
  *) fail 'rustup must resolve to an absolute executable path' ;;
esac
test -x "$cargo_path" || fail 'cargo is not executable'
test -x "$rustup_path" || fail 'rustup is not executable'
rustc_path="$(command -v rustc)"
case "$rustc_path" in
  /*) ;;
  *) fail 'rustc must resolve to an absolute executable path' ;;
esac
test -x "$rustc_path" || fail 'rustc is not executable'
readonly rustc_path
rust_release="$("$rustc_path" --version --verbose | /usr/bin/awk '$1 == "release:" { print $2 }')"
test "$rust_release" = '1.95.0' \
  || fail 'the repository-pinned Rust 1.95.0 toolchain is required'

assert_no_cargo_control_files() {
  local directory="$1"
  local control_file

  for control_file in "${directory}/.cargo/config" "${directory}/.cargo/config.toml"; do
    if test -e "$control_file" || test -L "$control_file"; then
      fail "ambient Cargo configuration is not permitted at ${control_file}"
    fi
  done
}

# Cargo searches the working directory and its ancestors independently of --manifest-path. Run
# from the physical repository root and reject every ambient config location on that search path.
cargo_control_root="$repository_root"
while :; do
  assert_no_cargo_control_files "$cargo_control_root"
  test "$cargo_control_root" = '/' && break
  cargo_control_root="${cargo_control_root%/*}"
  test -n "$cargo_control_root" || cargo_control_root='/'
done
effective_cargo_home="${CARGO_HOME:-}"
if test -z "$effective_cargo_home" && test -n "${HOME:-}"; then
  effective_cargo_home="${HOME}/.cargo"
fi
if test -n "$effective_cargo_home"; then
  for cargo_home_control_file in \
    "${effective_cargo_home}/config" "${effective_cargo_home}/config.toml"
  do
    if test -e "$cargo_home_control_file" || test -L "$cargo_home_control_file"; then
      fail "ambient Cargo configuration is not permitted at ${cargo_home_control_file}"
    fi
  done
fi

readonly linker_flags='-C link-arg=-Wl,-soname,libdayweave_android_ffi.so'\
' -C link-arg=-Wl,-z,max-page-size=16384'\
' -C link-arg=-Wl,-z,common-page-size=16384'\
' -C link-arg=-Wl,-z,relro'\
' -C link-arg=-Wl,-z,now'\
' -C link-arg=-Wl,-z,noexecstack'\
' -C link-arg=-Wl,--exclude-libs,libgcc.a'\
' -C link-arg=-Wl,--exclude-libs,libgcc_real.a'\
' -C link-arg=-Wl,--exclude-libs,libunwind.a'
readonly installed_targets="$($rustup_path target list --installed)"

ensure_directory "$cargo_target_root" 'the Android Rust target directory'
ensure_directory "$output_root" 'the generated Android jniLibs directory'

build_target() {
  local abi="$1"
  local rust_target="$2"
  local cargo_linker_variable="$3"
  local cargo_rustflags_variable="$4"
  local cc_variable="$5"
  local ar_variable="$6"
  local linker="${llvm_bin}/${rust_target}${android_api_level}-clang"
  local source_library="${cargo_target_root}/${rust_target}/${cargo_profile}/${library_name}"
  local destination_directory="${output_root}/${abi}"
  local destination_library="${destination_directory}/${library_name}"
  local temporary_library

  printf '%s\n' "$installed_targets" | /usr/bin/grep -Fxq -- "$rust_target" \
    || fail "Rust target ${rust_target} is not installed"
  test -x "$linker" || fail "the NDK linker for ${rust_target} is missing"

  (
    cd "$repository_root"
    /usr/bin/env \
      -u CARGO_BUILD_RUSTC \
      -u CARGO_BUILD_RUSTC_WRAPPER \
      -u CARGO_BUILD_RUSTFLAGS \
      -u CARGO_BUILD_TARGET \
      -u CARGO_ENCODED_RUSTFLAGS \
      -u CARGO_PROFILE_DEV_PANIC \
      -u CARGO_PROFILE_RELEASE_PANIC \
      -u RUSTC_BOOTSTRAP \
      -u RUSTC_WRAPPER \
      -u RUSTC_WORKSPACE_WRAPPER \
      -u RUSTDOC \
      -u RUSTFLAGS \
      "CARGO_PROFILE_DEV_PANIC=unwind" \
      "CARGO_PROFILE_RELEASE_PANIC=unwind" \
      "CARGO_TARGET_DIR=${cargo_target_root}" \
      "RUSTC=${rustc_path}" \
      "${cargo_linker_variable}=${linker}" \
      "${cargo_rustflags_variable}=${linker_flags}" \
      "${cc_variable}=${linker}" \
      "${ar_variable}=${llvm_bin}/llvm-ar" \
      "$cargo_path" build \
        --manifest-path "${repository_root}/Cargo.toml" \
        --package dayweave-android-ffi \
        --target "$rust_target" \
        "${cargo_profile_arguments[@]}"
  ) || fail "cargo did not build ${rust_target}"

  test -f "$source_library" && test ! -L "$source_library" \
    || fail "cargo did not produce ${source_library}"
  ensure_directory "$destination_directory" "the ${abi} jniLibs directory"
  temporary_library="$(/usr/bin/mktemp "${destination_directory}/.${library_name}.XXXXXXXX")" \
    || fail "a temporary ${abi} library could not be created"
  /usr/bin/install -m 0555 "$source_library" "$temporary_library" \
    || fail "the ${abi} library could not be copied"
  /bin/mv -f -- "$temporary_library" "$destination_library" \
    || fail "the ${abi} library could not be published"
}

build_target \
  arm64-v8a \
  aarch64-linux-android \
  CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER \
  CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS \
  CC_aarch64_linux_android \
  AR_aarch64_linux_android
build_target \
  x86_64 \
  x86_64-linux-android \
  CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER \
  CARGO_TARGET_X86_64_LINUX_ANDROID_RUSTFLAGS \
  CC_x86_64_linux_android \
  AR_x86_64_linux_android

"${repository_root}/scripts/tests/test-android-scheduler-library-native-security.sh" \
  "$build_variant"
printf 'Built and verified Android scheduler JNI libraries for %s.\n' "$build_variant"
