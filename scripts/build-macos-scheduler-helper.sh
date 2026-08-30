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

path_has_no_symlink_components() {
  local candidate_path="$1"
  local component
  local remaining
  local walked_path=''

  case "$candidate_path" in
    /*) ;;
    *) return 1 ;;
  esac
  case "$candidate_path" in
    *$'\n'*|*$'\r'*|*$'\t'*) return 1 ;;
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
      '..') return 1 ;;
    esac
    walked_path="${walked_path}/${component}"
    test ! -L "$walked_path" || return 1
    if test -n "$remaining" && test ! -d "$walked_path"; then
      return 1
    fi
  done
}

assert_no_symlink_components() {
  path_has_no_symlink_components "$1" \
    || fail "$2 must be an absolute path without symbolic-link components"
}

mode_is_group_or_world_writable() {
  case "$1" in
    ?[2367]?|??[2367]) return 0 ;;
    *) return 1 ;;
  esac
}

assert_trusted_path_directories() {
  local candidate_path="$1"
  local label="$2"
  local include_final="$3"
  local component
  local directory_metadata
  local directory_mode
  local directory_owner
  local directory_type
  local ignored_identity
  local ignored_links
  local remaining
  local walked_path=''

  assert_no_symlink_components "$candidate_path" "$label"
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
    esac
    walked_path="${walked_path}/${component}"
    if test -z "$remaining" && test "$include_final" = no; then
      continue
    fi
    directory_metadata="$(
      /usr/bin/stat -f '%HT|%l|%u|%Lp|%d:%i' -- "$walked_path"
    )" || fail "$label has an unreadable path component"
    IFS='|' read -r \
      directory_type ignored_links directory_owner directory_mode ignored_identity \
      <<<"$directory_metadata"
    test "$directory_type" = Directory \
      || fail "$label has a non-directory path component"
    case "$directory_owner" in
      0|"$current_user_id") ;;
      *) fail "$label has a path component owned by another user" ;;
    esac
    if mode_is_group_or_world_writable "$directory_mode"; then
      fail "$label has a group- or world-writable path component"
    fi
  done
}

path_type=''
path_links=''
path_owner=''
path_mode=''
path_identity=''
load_path_metadata() {
  local metadata
  metadata="$(/usr/bin/stat -f '%HT|%l|%u|%Lp|%d:%i' -- "$1")" \
    || fail "$2 metadata could not be read"
  IFS='|' read -r path_type path_links path_owner path_mode path_identity \
    <<<"$metadata"
}

assert_regular_single_link_file() {
  local candidate_path="$1"
  local label="$2"
  local executable_required="$3"
  local owner_required="$4"

  assert_no_symlink_components "$candidate_path" "$label"
  assert_trusted_path_directories "$candidate_path" "$label" no
  test -f "$candidate_path" && test ! -L "$candidate_path" \
    || fail "$label must be a regular, non-symlink file"
  load_path_metadata "$candidate_path" "$label"
  test "$path_type" = 'Regular File' \
    || fail "$label must be a regular, non-symlink file"
  test "$path_links" = 1 \
    || fail "$label must not have hard-linked aliases"
  if test "$executable_required" = yes; then
    test -x "$candidate_path" || fail "$label must be executable"
  fi
  if test "$owner_required" = yes; then
    test "$path_owner" = "$current_user_id" \
      || fail "$label must be owned by the current user"
    if mode_is_group_or_world_writable "$path_mode"; then
      fail "$label must not be group- or world-writable"
    fi
  fi
}

assert_input_tree() {
  local directory_path="$1"
  local entry

  assert_owned_safe_directory "$directory_path" 'a Rust source directory'

  for entry in \
    "$directory_path"/* \
    "$directory_path"/.[!.]* \
    "$directory_path"/..?*
  do
    if test ! -e "$entry" && test ! -L "$entry"; then
      continue
    fi
    if test -L "$entry"; then
      fail 'a Rust build input must not be a symbolic link'
    elif test -d "$entry"; then
      assert_input_tree "$entry"
    elif test -f "$entry"; then
      assert_regular_single_link_file "$entry" 'a Rust build input' no yes
    else
      fail 'a Rust build input must be a regular file or directory'
    fi
  done
}

assert_owned_safe_directory() {
  local directory_path="$1"
  local label="$2"

  assert_no_symlink_components "$directory_path" "$label"
  assert_trusted_path_directories "$directory_path" "$label" yes
  test -d "$directory_path" && test ! -L "$directory_path" \
    || fail "$label must be a regular directory"
  load_path_metadata "$directory_path" "$label"
  test "$path_type" = Directory \
    || fail "$label must be a regular directory"
  test "$path_owner" = "$current_user_id" \
    || fail "$label must be owned by the current user"
  if mode_is_group_or_world_writable "$path_mode"; then
    fail "$label must not be group- or world-writable"
  fi
}

ensure_output_directory() {
  local directory_path="$1"
  local label="$2"

  if test -e "$directory_path" || test -L "$directory_path"; then
    assert_owned_safe_directory "$directory_path" "$label"
  else
    assert_no_symlink_components "$directory_path" "$label"
    /bin/mkdir -m 0700 -- "$directory_path" \
      || fail "$label could not be created"
    assert_owned_safe_directory "$directory_path" "$label"
  fi
}

assert_no_cargo_control_files() {
  local cargo_control_root="$1"
  local cargo_control_file

  for cargo_control_file in \
    "${cargo_control_root}/config" \
    "${cargo_control_root}/config.toml" \
    "${cargo_control_root}/credentials" \
    "${cargo_control_root}/credentials.toml"
  do
    if test -e "$cargo_control_file" || test -L "$cargo_control_file"; then
      fail 'the private Cargo home contains a configuration or credential file'
    fi
  done
}

seed_verified_registry_archives() {
  local archive
  local archive_checksum
  local archive_name
  local cache_namespace
  local destination
  local destination_directory
  local locked_archives_file="${temporary_root}/locked-registry-archives.tsv"
  local package_checksum
  local package_name
  local package_version

  if test -L "$canonical_registry_cache"; then
    fail 'the canonical Cargo registry cache must not be a symbolic link'
  fi
  test -d "$canonical_registry_cache" || return 0
  assert_owned_safe_directory \
    "$canonical_registry_cache" 'the canonical Cargo registry cache'

  /usr/bin/awk -F ' = ' '
    function unquote(value) {
      sub(/^"/, "", value)
      sub(/"$/, "", value)
      return value
    }
    $1 == "name" { name = unquote($2) }
    $1 == "version" { version = unquote($2) }
    $1 == "source" { source = unquote($2) }
    $1 == "checksum" {
      checksum = unquote($2)
      if (source == "registry+https://github.com/rust-lang/crates.io-index") {
        print name "\t" version "\t" checksum
      }
      name = version = source = checksum = ""
    }
  ' "${repository_root}/Cargo.lock" >"$locked_archives_file" \
    || fail 'locked registry archive metadata could not be read'

  ensure_output_directory \
    "${private_cargo_home}/registry" 'the private Cargo registry directory'
  ensure_output_directory \
    "${private_cargo_home}/registry/cache" 'the private Cargo archive cache'

  while IFS=$'\t' read -r package_name package_version package_checksum; do
    case "$package_name" in
      ''|*[!A-Za-z0-9_-]*) fail 'Cargo.lock contains an invalid registry package name' ;;
    esac
    case "$package_version" in
      ''|*[!A-Za-z0-9.+-]*) fail 'Cargo.lock contains an invalid registry package version' ;;
    esac
    test "${#package_checksum}" = 64 \
      || fail 'Cargo.lock contains an invalid registry checksum length'
    case "$package_checksum" in
      *[!0-9a-f]*) fail 'Cargo.lock contains an invalid registry checksum' ;;
    esac

    for archive in \
      "${canonical_registry_cache}"/*/"${package_name}-${package_version}.crate"
    do
      if test ! -e "$archive" && test ! -L "$archive"; then
        continue
      fi
      assert_regular_single_link_file \
        "$archive" 'a canonical Cargo registry archive' no yes
      cache_namespace="${archive%/*}"
      cache_namespace="${cache_namespace##*/}"
      case "$cache_namespace" in
        index.crates.io-*) ;;
        *) fail 'a Cargo registry archive uses an unexpected cache namespace' ;;
      esac
      case "$cache_namespace" in
        *[!A-Za-z0-9._-]*) fail 'a Cargo registry cache namespace is invalid' ;;
      esac
      archive_checksum="$(
        "${system_environment[@]}" /usr/bin/shasum -a 256 -- "$archive" \
          | /usr/bin/awk 'NR == 1 { print $1 }'
      )" || fail 'a canonical Cargo registry archive could not be hashed'
      if test "$archive_checksum" != "$package_checksum"; then
        continue
      fi

      destination_directory="${private_cargo_home}/registry/cache/${cache_namespace}"
      ensure_output_directory \
        "$destination_directory" 'a private Cargo archive namespace'
      destination="${destination_directory}/${package_name}-${package_version}.crate"
      /usr/bin/install -m 0600 "$archive" "$destination" \
        || fail 'a verified Cargo registry archive could not be copied'
      assert_regular_single_link_file \
        "$destination" 'a private Cargo registry archive' no yes
      archive_checksum="$(
        "${system_environment[@]}" /usr/bin/shasum -a 256 -- "$destination" \
          | /usr/bin/awk 'NR == 1 { print $1 }'
      )" || fail 'a copied Cargo registry archive could not be hashed'
      test "$archive_checksum" = "$package_checksum" \
        || fail 'a copied Cargo registry archive failed checksum verification'
    done
  done <"$locked_archives_file"
}

for trusted_tool in \
  /bin/mkdir /bin/mv /bin/pwd /bin/rm \
  /usr/bin/awk /usr/bin/codesign /usr/bin/dirname /usr/bin/env /usr/bin/id \
  /usr/bin/install /usr/bin/lipo /usr/bin/mktemp /usr/bin/otool /usr/bin/shasum \
  /usr/bin/stat /usr/bin/uname /usr/bin/vtool
do
  test -x "$trusted_tool" || fail "missing trusted tool ${trusted_tool}"
done
system_environment=(
  /usr/bin/env -i
  'LC_ALL=C'
  'PATH=/usr/bin:/bin:/usr/sbin:/sbin'
)
readonly system_environment

test "$(/usr/bin/uname -s)" = Darwin || fail 'the host must be macOS'
test "$(/usr/bin/uname -m)" = arm64 || fail 'the host must be Apple Silicon'
current_user_id="$(/usr/bin/id -u)" || fail 'the current user could not be determined'
readonly current_user_id

working_directory="$(/bin/pwd -P)" \
  || fail 'the working directory could not be resolved'
case "${BASH_SOURCE[0]}" in
  /*) invoked_script_path="${BASH_SOURCE[0]}" ;;
  *) invoked_script_path="${working_directory}/${BASH_SOURCE[0]}" ;;
esac
assert_regular_single_link_file \
  "$invoked_script_path" 'the build script' yes yes
invoked_script_identity="$path_identity"

script_directory="$(
  CDPATH= cd -- "$(/usr/bin/dirname -- "$invoked_script_path")" && /bin/pwd -P
)" || fail 'the script directory could not be resolved'
readonly script_directory
readonly script_path="${script_directory}/${invoked_script_path##*/}"
assert_regular_single_link_file "$script_path" 'the build script' yes yes
test "$path_identity" = "$invoked_script_identity" \
  || fail 'the build script changed while its canonical path was resolved'

repository_root="$(
  CDPATH= cd -- "${script_directory}/.." && /bin/pwd -P
)" || fail 'the repository root could not be resolved'
readonly repository_root
assert_owned_safe_directory "$repository_root" 'the repository root'

readonly target_triple='aarch64-apple-darwin'
readonly deployment_target='15.0'
readonly signing_identifier='com.greengolddog.dayweave.scheduler-helper'
readonly designated_requirement='designated => identifier "com.greengolddog.dayweave.scheduler-helper"'
readonly identifier_requirement='identifier "com.greengolddog.dayweave.scheduler-helper"'
readonly output_root="${repository_root}/target"
readonly output_architecture_directory="${output_root}/${target_triple}"
readonly output_release_directory="${output_architecture_directory}/release"
readonly helper_path="${output_release_directory}/dayweave-scheduler-helper"

for input_file in \
  "${repository_root}/Cargo.lock" \
  "${repository_root}/Cargo.toml" \
  "${repository_root}/rust-toolchain.toml" \
  "${repository_root}/crates/dayweave-codex/Cargo.toml" \
  "${repository_root}/crates/dayweave-compose/Cargo.toml" \
  "${repository_root}/crates/dayweave-core/Cargo.toml" \
  "${repository_root}/crates/dayweave-google/Cargo.toml" \
  "${repository_root}/crates/dayweave-scheduler-helper/Cargo.toml" \
  "${repository_root}/server/dayweave-api/Cargo.toml"
do
  assert_regular_single_link_file "$input_file" 'a Rust build input' no yes
done
for input_directory in \
  "${repository_root}/crates/dayweave-compose/src" \
  "${repository_root}/crates/dayweave-core/src" \
  "${repository_root}/crates/dayweave-scheduler-helper/src"
do
  assert_input_tree "$input_directory"
done
for crate_directory in \
  "${repository_root}/crates/dayweave-compose" \
  "${repository_root}/crates/dayweave-core" \
  "${repository_root}/crates/dayweave-scheduler-helper"
do
  if test -e "${crate_directory}/build.rs" || test -L "${crate_directory}/build.rs"; then
    assert_regular_single_link_file \
      "${crate_directory}/build.rs" 'a Rust build input' no yes
  fi
done
unexpected_lock_sources="$(
  /usr/bin/awk -F ' = ' '
    $1 == "source" && $2 != "\"registry+https://github.com/rust-lang/crates.io-index\"" {
      print $2
    }
  ' "${repository_root}/Cargo.lock"
)" || fail 'Cargo.lock registry sources could not be validated'
test -z "$unexpected_lock_sources" \
  || fail 'Cargo.lock contains a non-crates.io external source'

ensure_output_directory "$output_root" 'the target output directory'
ensure_output_directory \
  "$output_architecture_directory" 'the architecture output directory'
ensure_output_directory "$output_release_directory" 'the release output directory'

initial_helper_identity='absent'
if test -e "$helper_path" || test -L "$helper_path"; then
  assert_regular_single_link_file "$helper_path" 'the existing helper output' yes yes
  initial_helper_identity="$path_identity"
fi
readonly initial_helper_identity

temporary_root=''
temporary_root_identity=''
cleanup() {
  local status=$?
  local cleanup_metadata
  local cleanup_type
  local cleanup_links
  local cleanup_owner
  local cleanup_mode
  local cleanup_identity

  trap - EXIT HUP INT TERM
  if test -n "$temporary_root"; then
    case "$temporary_root" in
      "${output_root}"/.dayweave-scheduler-helper.*)
        if path_has_no_symlink_components "$temporary_root" && \
          test -d "$temporary_root" && test ! -L "$temporary_root"; then
          cleanup_metadata="$(
            /usr/bin/stat -f '%HT|%l|%u|%Lp|%d:%i' -- "$temporary_root" 2>/dev/null
          )" || cleanup_metadata=''
          IFS='|' read -r \
            cleanup_type cleanup_links cleanup_owner cleanup_mode cleanup_identity \
            <<<"$cleanup_metadata"
          if test "$cleanup_type" = Directory && \
            test "$cleanup_owner" = "$current_user_id" && \
            test "$cleanup_mode" = 700 && \
            test "$cleanup_identity" = "$temporary_root_identity"; then
            /bin/rm -rf -- "$temporary_root" || status=1
          else
            printf '%s\n' \
              'Scheduler helper build failed: refusing unsafe temporary-directory cleanup' >&2
            status=1
          fi
        elif test -e "$temporary_root" || test -L "$temporary_root"; then
          printf '%s\n' \
            'Scheduler helper build failed: refusing unsafe temporary-directory cleanup' >&2
          status=1
        fi
        ;;
      *)
        printf '%s\n' \
          'Scheduler helper build failed: refusing out-of-bound temporary-directory cleanup' >&2
        status=1
        ;;
    esac
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

temporary_root="$(
  /usr/bin/mktemp -d "${output_root}/.dayweave-scheduler-helper.XXXXXXXX"
)" || fail 'a private temporary build directory could not be created'
assert_owned_safe_directory "$temporary_root" 'the temporary build directory'
test "$path_mode" = 700 \
  || fail 'the temporary build directory must have mode 0700'
temporary_root_identity="$path_identity"

readonly private_target_directory="${temporary_root}/cargo-target"
readonly private_cargo_home="${temporary_root}/cargo-home"
readonly private_home_directory="${temporary_root}/home"
readonly private_temporary_directory="${temporary_root}/tmp"
for private_directory in \
  "$private_target_directory" "$private_cargo_home" \
  "$private_home_directory" "$private_temporary_directory"
do
  /bin/mkdir -m 0700 -- "$private_directory" \
    || fail 'a private build subdirectory could not be created'
  assert_owned_safe_directory "$private_directory" 'a private build subdirectory'
  test "$path_mode" = 700 \
    || fail 'a private build subdirectory must have mode 0700'
done

account_record="$(/usr/bin/id -P)" \
  || fail 'the current account record could not be read'
account_home="$(
  /usr/bin/awk -F: -v expected_uid="$current_user_id" \
    '$3 == expected_uid { print $(NF - 1) }' <<<"$account_record"
)" || fail 'the current account home could not be read'
test -n "$account_home" \
  || fail 'the current account does not have a home directory'
case "$account_home" in
  /*) ;;
  *) fail 'the current account home must be an absolute path' ;;
esac
case "$account_home" in
  *$'\n'*|*$'\r'*|*$'\t'*) fail 'the current account home contains unsupported control characters' ;;
esac
readonly account_home
assert_owned_safe_directory "$account_home" 'the current account home'

readonly canonical_cargo_root="${account_home}/.cargo"
readonly canonical_registry_cache="${canonical_cargo_root}/registry/cache"
readonly rustup_home="${account_home}/.rustup"
readonly rustup_command="${canonical_cargo_root}/bin/rustup"
readonly pinned_toolchain_root="${rustup_home}/toolchains/1.95.0-aarch64-apple-darwin"
readonly expected_rustc_command="${pinned_toolchain_root}/bin/rustc"
readonly expected_cargo_command="${pinned_toolchain_root}/bin/cargo"
assert_owned_safe_directory "$canonical_cargo_root" 'the canonical Cargo directory'
assert_owned_safe_directory \
  "${canonical_cargo_root}/bin" 'the canonical Cargo binary directory'
assert_owned_safe_directory "$rustup_home" 'the canonical Rustup directory'
assert_regular_single_link_file "$rustup_command" 'the canonical rustup executable' yes yes
for root_cargo_control_file in /.cargo/config /.cargo/config.toml; do
  if test -e "$root_cargo_control_file" || test -L "$root_cargo_control_file"; then
    fail 'Cargo configuration at the filesystem root is not permitted'
  fi
done
assert_no_cargo_control_files "$private_cargo_home"
seed_verified_registry_archives
assert_no_cargo_control_files "$private_cargo_home"

rust_environment=(
  /usr/bin/env -i
  "CARGO_HOME=${private_cargo_home}"
  "HOME=${private_home_directory}"
  'PATH=/usr/bin:/bin:/usr/sbin:/sbin'
  "RUSTUP_HOME=${rustup_home}"
  'RUSTUP_TOOLCHAIN=1.95.0'
  "TMPDIR=${private_temporary_directory}"
)
readonly rust_environment

rustc_command="$(
  cd /
  "${rust_environment[@]}" "$rustup_command" which rustc
)" || fail 'the pinned rustc executable could not be located'
cargo_command="$(
  cd /
  "${rust_environment[@]}" "$rustup_command" which cargo
)" || fail 'the pinned cargo executable could not be located'
readonly rustc_command cargo_command
test "$rustc_command" = "$expected_rustc_command" \
  || fail 'rustup resolved rustc outside the pinned standard toolchain'
test "$cargo_command" = "$expected_cargo_command" \
  || fail 'rustup resolved cargo outside the pinned standard toolchain'
assert_regular_single_link_file \
  "$rustc_command" 'the pinned rustc executable' yes yes
assert_regular_single_link_file \
  "$cargo_command" 'the pinned cargo executable' yes yes

rustc_version="$("${rust_environment[@]}" "$rustc_command" --version)" \
  || fail 'rustc version could not be determined'
readonly rustc_version
case "$rustc_version" in
  'rustc 1.95.0 '*) ;;
  *) fail 'rustc must match the repository pin at 1.95.0' ;;
esac
installed_targets="$(
  cd /
  "${rust_environment[@]}" "$rustup_command" target list --installed
)" || fail 'installed Rust targets could not be determined'
readonly installed_targets
case $'\n'"$installed_targets"$'\n' in
  *$'\n'"$target_triple"$'\n'*) ;;
  *) fail "the ${target_triple} Rust target is not installed" ;;
esac

(
  cd /
  for root_cargo_control_file in /.cargo/config /.cargo/config.toml; do
    test ! -e "$root_cargo_control_file" && test ! -L "$root_cargo_control_file" \
      || fail 'Cargo configuration appeared at the filesystem root'
  done
  assert_no_cargo_control_files "$private_cargo_home"
  "${rust_environment[@]}" \
    CARGO_INCREMENTAL=0 \
    CARGO_NET_GIT_FETCH_WITH_CLI=false \
    CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse \
    CARGO_TARGET_DIR="$private_target_directory" \
    CARGO_TERM_COLOR=never \
    MACOSX_DEPLOYMENT_TARGET="$deployment_target" \
    RUSTC="$rustc_command" \
    "$cargo_command" build --locked --release \
      --manifest-path "${repository_root}/Cargo.toml" \
      --target "$target_triple" \
      --package dayweave-scheduler-helper
) || fail 'cargo did not complete the scheduler helper build'
assert_no_cargo_control_files "$private_cargo_home"

readonly staged_helper_path="${private_target_directory}/${target_triple}/release/dayweave-scheduler-helper"
assert_regular_single_link_file \
  "$staged_helper_path" 'the staged helper' yes yes
test "$("${system_environment[@]}" /usr/bin/lipo -archs "$staged_helper_path")" = arm64 \
  || fail 'the helper is not a single-architecture arm64 executable'

build_metadata="$(
  "${system_environment[@]}" /usr/bin/vtool -show-build "$staged_helper_path"
)" \
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
  "${system_environment[@]}" /usr/bin/otool -L "$staged_helper_path" \
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
"${system_environment[@]}" /usr/bin/codesign \
  --force --sign - --identifier "$signing_identifier" --timestamp=none \
  --requirements "=${designated_requirement}" \
  "$staged_helper_path" >/dev/null
assert_regular_single_link_file \
  "$staged_helper_path" 'the signed staged helper' yes yes
staged_helper_device="${path_identity%%:*}"
"${system_environment[@]}" /usr/bin/codesign \
  --verify --strict "$staged_helper_path" >/dev/null 2>&1 \
  || fail 'the helper does not pass strict code-signature verification'
"${system_environment[@]}" /usr/bin/codesign \
  --verify --strict -R="$identifier_requirement" "$staged_helper_path" >/dev/null 2>&1 \
  || fail 'the helper does not pass strict identifier-bound signature verification'
displayed_requirement="$(
  "${system_environment[@]}" \
    /usr/bin/codesign --display --requirements - "$staged_helper_path" 2>&1
)" || fail 'the helper designated requirement could not be read'
test "$displayed_requirement" = \
  "Executable=${staged_helper_path}"$'\n'"${designated_requirement}" \
  || fail 'the helper does not have the expected designated identifier requirement'
staged_helper_hash="$(
  "${system_environment[@]}" /usr/bin/shasum -a 256 -- "$staged_helper_path" \
    | /usr/bin/awk 'NR == 1 { print $1 }'
)" || fail 'the staged helper SHA-256 could not be calculated'
readonly staged_helper_hash
test "${#staged_helper_hash}" = 64 \
  || fail 'the staged helper SHA-256 has an invalid length'
case "$staged_helper_hash" in
  *[!0-9a-f]*) fail 'the staged helper SHA-256 has an invalid format' ;;
esac

assert_owned_safe_directory "$output_root" 'the target output directory'
assert_owned_safe_directory \
  "$output_architecture_directory" 'the architecture output directory'
assert_owned_safe_directory "$output_release_directory" 'the release output directory'
test "${path_identity%%:*}" = "$staged_helper_device" \
  || fail 'the staged helper and output directory must use the same filesystem'
if test "$initial_helper_identity" = absent; then
  if test -e "$helper_path" || test -L "$helper_path"; then
    fail 'the helper output appeared while the private build was running'
  fi
else
  assert_regular_single_link_file "$helper_path" 'the existing helper output' yes yes
  test "$path_identity" = "$initial_helper_identity" \
    || fail 'the existing helper output changed while the private build was running'
fi

/bin/mv -fh -- "$staged_helper_path" "$helper_path" \
  || fail 'the verified helper could not be published'

printf 'Built and verified %s\n' "$helper_path"
printf '%s  %s\n' "$staged_helper_hash" "$helper_path"
