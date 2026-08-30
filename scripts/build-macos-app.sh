#!/bin/bash -p

# Enter once with an account-derived HOME/TMPDIR and an otherwise empty
# environment. Privileged Bash mode prevents BASH_ENV and exported functions
# from running before this bootstrap can reject them.
case $- in
  *p*) ;;
  *)
    builtin printf '%s\n' \
      'macOS app build failed: invoke this executable directly (privileged Bash mode is required)' >&2
    builtin exit 126
    ;;
esac

bootstrap_fail() {
  builtin printf 'macOS app build failed: %s\n' "$1" >&2
  builtin exit 1
}

bootstrap_user_id="$(/usr/bin/id -u)" \
  || bootstrap_fail 'the current user could not be determined'

bootstrap_mode_is_unsafe() {
  case "$1" in
    ''|*[!0-7]*) return 0 ;;
  esac
  (( (8#$1 & 0022) != 0 ))
}

if test "${DAYWEAVE_APP_BUILDER_CLEAN_ENV:-}" = 1; then
  bootstrap_invoked_path="${DAYWEAVE_APP_BUILDER_SCRIPT_PATH:-}"
else
  bootstrap_working_directory="$(/bin/pwd -P)" \
    || bootstrap_fail 'the working directory could not be resolved'
  case "${BASH_SOURCE[0]}" in
    /*) bootstrap_invoked_path="${BASH_SOURCE[0]}" ;;
    *) bootstrap_invoked_path="${bootstrap_working_directory}/${BASH_SOURCE[0]}" ;;
  esac
fi
case "$bootstrap_invoked_path" in
  /*) ;;
  *) bootstrap_fail 'the invoked script path is not absolute' ;;
esac
case "$bootstrap_invoked_path" in
  *$'\n'*|*$'\r'*|*$'\t'*) \
    bootstrap_fail 'the invoked script path contains a control character' ;;
esac

bootstrap_remaining="${bootstrap_invoked_path#/}"
bootstrap_walked_path=''
while test -n "$bootstrap_remaining"; do
  bootstrap_component="${bootstrap_remaining%%/*}"
  if test "$bootstrap_component" = "$bootstrap_remaining"; then
    bootstrap_remaining=''
  else
    bootstrap_remaining="${bootstrap_remaining#*/}"
  fi
  case "$bootstrap_component" in
    ''|'.') continue ;;
    '..') bootstrap_fail 'the invoked script path contains a parent traversal' ;;
  esac
  bootstrap_walked_path="${bootstrap_walked_path}/${bootstrap_component}"
  test ! -L "$bootstrap_walked_path" \
    || bootstrap_fail 'the invoked script path contains a symbolic link'
  if test -n "$bootstrap_remaining"; then
    bootstrap_metadata="$(
      /usr/bin/stat -f '%HT|%u|%Lp' -- "$bootstrap_walked_path"
    )" || bootstrap_fail 'an invoked-script directory is unreadable'
    IFS='|' read -r \
      bootstrap_type bootstrap_owner bootstrap_mode <<<"$bootstrap_metadata"
    test "$bootstrap_type" = Directory \
      || bootstrap_fail 'the invoked script has a non-directory path component'
    case "$bootstrap_owner" in
      0|"$bootstrap_user_id") ;;
      *) bootstrap_fail 'an invoked-script directory has an untrusted owner' ;;
    esac
    if bootstrap_mode_is_unsafe "$bootstrap_mode"; then
      bootstrap_fail 'an invoked-script directory is group- or world-writable'
    fi
  fi
done

test -f "$bootstrap_invoked_path" && test ! -L "$bootstrap_invoked_path" \
  && test -x "$bootstrap_invoked_path" \
  || bootstrap_fail 'the invoked script is not a regular executable'
bootstrap_metadata="$(
  /usr/bin/stat -f '%HT|%l|%u|%Lp|%d:%i' -- "$bootstrap_invoked_path"
)" || bootstrap_fail 'the invoked script metadata could not be read'
IFS='|' read -r \
  bootstrap_type bootstrap_links bootstrap_owner bootstrap_mode \
  bootstrap_script_identity <<<"$bootstrap_metadata"
test "$bootstrap_type" = 'Regular File' && test "$bootstrap_links" = 1 \
  || bootstrap_fail 'the invoked script must be a single-link regular file'
test "$bootstrap_owner" = "$bootstrap_user_id" \
  || bootstrap_fail 'the invoked script must be owned by the current user'
if bootstrap_mode_is_unsafe "$bootstrap_mode"; then
  bootstrap_fail 'the invoked script is group- or world-writable'
fi
bootstrap_script_directory="$(
  CDPATH= cd -- "${bootstrap_invoked_path%/*}" && /bin/pwd -P
)" || bootstrap_fail 'the invoked script directory could not be resolved'
bootstrap_script_path="${bootstrap_script_directory}/${bootstrap_invoked_path##*/}"
bootstrap_canonical_identity="$(
  /usr/bin/stat -f '%d:%i' -- "$bootstrap_script_path"
)" || bootstrap_fail 'the canonical script identity could not be read'
test "$bootstrap_canonical_identity" = "$bootstrap_script_identity" \
  || bootstrap_fail 'the invoked script changed while its path was resolved'

bootstrap_account_home="$(
  LC_ALL=C /usr/bin/dscacheutil -q user -a uid "$bootstrap_user_id" \
    | /usr/bin/awk '
        $1 == "dir:" {
          sub(/^dir:[[:space:]]*/, "")
          value = $0
          count += 1
        }
        END {
          if (count != 1 || value == "") exit 1
          print value
        }
      '
)" || bootstrap_fail 'the account home directory could not be determined'
bootstrap_temporary_directory="$(/usr/bin/getconf DARWIN_USER_TEMP_DIR)" \
  || bootstrap_fail 'the account temporary directory could not be determined'
bootstrap_temporary_directory="${bootstrap_temporary_directory%/}"
for bootstrap_path in "$bootstrap_account_home" "$bootstrap_temporary_directory"; do
  case "$bootstrap_path" in
    /*) ;;
    *) bootstrap_fail 'an account directory is not absolute' ;;
  esac
  case "$bootstrap_path" in
    *$'\n'*|*$'\r'*|*$'\t'*) \
      bootstrap_fail 'an account directory contains a control character' ;;
  esac
  test -d "$bootstrap_path" && test ! -L "$bootstrap_path" \
    || bootstrap_fail 'an account directory is unavailable or symbolic-linked'
done

bootstrap_environment_is_clean=true
if test "${DAYWEAVE_APP_BUILDER_CLEAN_ENV:-}" != 1 \
  || test "${DAYWEAVE_APP_BUILDER_SCRIPT_PATH:-}" != "$bootstrap_script_path" \
  || test "${DAYWEAVE_APP_BUILDER_SCRIPT_IDENTITY:-}" != "$bootstrap_script_identity" \
  || test "${HOME:-}" != "$bootstrap_account_home" \
  || test "${TMPDIR:-}" != "$bootstrap_temporary_directory" \
  || test "${PATH:-}" != /usr/bin:/bin:/usr/sbin:/sbin \
  || test "${LANG:-}" != en_US.UTF-8 \
  || test "${LC_ALL:-}" != C; then
  bootstrap_environment_is_clean=false
else
  while IFS= read -r -d '' bootstrap_entry; do
    bootstrap_name="${bootstrap_entry%%=*}"
    case "$bootstrap_name" in
      DAYWEAVE_APP_BUILDER_CLEAN_ENV|DAYWEAVE_APP_BUILDER_SCRIPT_IDENTITY|DAYWEAVE_APP_BUILDER_SCRIPT_PATH|HOME|LANG|LC_ALL|OLDPWD|PATH|PWD|SHLVL|TMPDIR|_) ;;
      *) bootstrap_environment_is_clean=false ;;
    esac
  done < <(/usr/bin/env -0)
fi

if test "$bootstrap_environment_is_clean" != true; then
  exec 9< "$bootstrap_script_path" \
    || bootstrap_fail 'the validated script could not be pinned for clean re-execution'
  bootstrap_descriptor_inode="$(/usr/bin/stat -f '%i' -- /dev/fd/9)" \
    || bootstrap_fail 'the pinned script descriptor could not be inspected'
  test "$bootstrap_descriptor_inode" = "${bootstrap_script_identity##*:}" \
    || bootstrap_fail 'the invoked script changed before clean re-execution'
  exec /usr/bin/env -i \
    "HOME=${bootstrap_account_home}" \
    'LANG=en_US.UTF-8' \
    'LC_ALL=C' \
    'PATH=/usr/bin:/bin:/usr/sbin:/sbin' \
    "TMPDIR=${bootstrap_temporary_directory}" \
    'DAYWEAVE_APP_BUILDER_CLEAN_ENV=1' \
    "DAYWEAVE_APP_BUILDER_SCRIPT_IDENTITY=${bootstrap_script_identity}" \
    "DAYWEAVE_APP_BUILDER_SCRIPT_PATH=${bootstrap_script_path}" \
    /bin/bash -p /dev/fd/9 "$@"
  bootstrap_fail 'could not enter the clean build environment'
fi

bootstrap_descriptor_inode="$(/usr/bin/stat -f '%i' -- /dev/fd/9)" \
  || bootstrap_fail 'the clean script descriptor could not be inspected'
test "$bootstrap_descriptor_inode" = "${bootstrap_script_identity##*:}" \
  || bootstrap_fail 'the clean script descriptor does not match the trusted script'
validated_invoked_script_path="$bootstrap_script_path"
validated_invoked_script_identity="$bootstrap_script_identity"
readonly validated_invoked_script_path validated_invoked_script_identity
unset DAYWEAVE_APP_BUILDER_CLEAN_ENV DAYWEAVE_APP_BUILDER_SCRIPT_IDENTITY \
  DAYWEAVE_APP_BUILDER_SCRIPT_PATH

unset bootstrap_account_home bootstrap_canonical_identity bootstrap_component \
  bootstrap_descriptor_inode bootstrap_entry bootstrap_environment_is_clean \
  bootstrap_invoked_path bootstrap_links bootstrap_metadata bootstrap_mode \
  bootstrap_name bootstrap_owner bootstrap_path bootstrap_remaining \
  bootstrap_script_directory bootstrap_script_identity bootstrap_script_path \
  bootstrap_temporary_directory bootstrap_type bootstrap_user_id \
  bootstrap_walked_path bootstrap_working_directory
unset -f bootstrap_fail bootstrap_mode_is_unsafe

set -euo pipefail
IFS=$'\n\t'
umask 077

fail() {
  printf 'macOS app build failed: %s\n' "$1" >&2
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
  local candidate_mode="$1"

  case "$candidate_mode" in
    ''|*[!0-7]*) return 0 ;;
  esac
  (( (8#${candidate_mode} & 0022) != 0 ))
}

current_user_id="$(/usr/bin/id -u)" \
  || fail 'the current user could not be determined'
readonly current_user_id

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

assert_trusted_path_directories() {
  local candidate_path="$1"
  local label="$2"
  local include_final="$3"
  local component
  local directory_identity
  local directory_links
  local directory_metadata
  local directory_mode
  local directory_owner
  local directory_type
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
      directory_type directory_links directory_owner directory_mode \
      directory_identity <<<"$directory_metadata"
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

assert_owned_safe_directory() {
  local directory_path="$1"
  local label="$2"

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

assert_input_tree() {
  local directory_path="$1"
  local label="$2"
  local entry

  assert_owned_safe_directory "$directory_path" "$label"
  for entry in \
    "$directory_path"/* \
    "$directory_path"/.[!.]* \
    "$directory_path"/..?*
  do
    if test ! -e "$entry" && test ! -L "$entry"; then
      continue
    fi
    if test -L "$entry"; then
      fail "$label must not contain symbolic links"
    elif test -d "$entry"; then
      assert_input_tree "$entry" "$label"
    elif test -f "$entry"; then
      assert_regular_single_link_file "$entry" "${label} file" no
    else
      fail "$label must contain only regular files and directories"
    fi
  done
}

ensure_owned_safe_directory() {
  local directory_path="$1"
  local label="$2"
  local parent_path="${directory_path%/*}"

  if test -e "$directory_path" || test -L "$directory_path"; then
    assert_owned_safe_directory "$directory_path" "$label"
  else
    assert_owned_safe_directory "$parent_path" "the parent of ${label}"
    /bin/mkdir -m 0700 -- "$directory_path" \
      || fail "$label could not be created"
    assert_owned_safe_directory "$directory_path" "$label"
  fi
}

assert_regular_single_link_file() {
  local candidate_path="$1"
  local label="$2"
  local executable_required="$3"

  assert_no_symlink_components "$candidate_path" "$label"
  assert_trusted_path_directories "$candidate_path" "$label" no
  test -f "$candidate_path" && test ! -L "$candidate_path" \
    || fail "$label must be a regular, non-symlink file"
  load_path_metadata "$candidate_path" "$label"
  test "$path_type" = 'Regular File' \
    || fail "$label must be a regular, non-symlink file"
  test "$path_links" = 1 \
    || fail "$label must not have hard-linked aliases"
  test "$path_owner" = "$current_user_id" \
    || fail "$label must be owned by the current user"
  if mode_is_group_or_world_writable "$path_mode"; then
    fail "$label must not be group- or world-writable"
  fi
  if test "$executable_required" = yes; then
    test -x "$candidate_path" || fail "$label must be executable"
  fi
}

# Homebrew's Caskroom is intentionally group-writable on a standard local
# installation. Treat that external tree as mutable, while still rejecting a
# symlinked or aliased executable and pinning the exact bytes independently.
assert_mutable_parent_regular_single_link_file() {
  local candidate_path="$1"
  local label="$2"

  assert_no_symlink_components "$candidate_path" "$label"
  test -f "$candidate_path" && test ! -L "$candidate_path" \
    || fail "$label must be a regular, non-symlink file"
  load_path_metadata "$candidate_path" "$label"
  test "$path_type" = 'Regular File' \
    || fail "$label must be a regular, non-symlink file"
  test "$path_links" = 1 \
    || fail "$label must not have hard-linked aliases"
  case "$path_owner" in
    0|"$current_user_id") ;;
    *) fail "$label must be owned by root or the current user" ;;
  esac
  if mode_is_group_or_world_writable "$path_mode"; then
    fail "$label must not be group- or world-writable"
  fi
  test -x "$candidate_path" || fail "$label must be executable"
}

sha256_file() {
  local digest

  digest="$(
    "${private_tool_environment[@]}" /usr/bin/shasum -a 256 -- "$1" \
      | /usr/bin/awk 'NR == 1 { print $1 }'
  )" || fail "$2 SHA-256 could not be calculated"
  test "${#digest}" = 64 || fail "$2 SHA-256 has an invalid length"
  case "$digest" in
    *[!0-9a-f]*) fail "$2 SHA-256 has an invalid format" ;;
  esac
  printf '%s\n' "$digest"
}

readonly helper_identifier='com.greengolddog.dayweave.scheduler-helper'
readonly helper_identifier_requirement="identifier \"${helper_identifier}\""
readonly helper_designated_requirement="designated => identifier \"${helper_identifier}\""
readonly deployment_target='15.0'
readonly codex_requirement='identifier "codex" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "2DC432GLL2"'

verify_arm64_system_macho() {
  local macho_path="$1"
  local label="$2"
  local build_metadata
  local minimum_os_entries
  local minimum_os_matches
  local platform_entries
  local platform_matches
  local unexpected_libraries

  assert_regular_single_link_file "$macho_path" "$label" yes
  test "$(/usr/bin/lipo -archs "$macho_path")" = arm64 \
    || fail "$label must be a thin arm64 executable"

  build_metadata="$(/usr/bin/vtool -show-build "$macho_path")" \
    || fail "$label Mach-O build metadata could not be read"
  platform_matches="$(
    /usr/bin/awk \
      '$1 == "platform" && $2 == "MACOS" { count += 1 } END { print count + 0 }' \
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
    || fail "$label must declare exactly one macOS build platform"
  test "$minimum_os_matches" = 1 && test "$minimum_os_entries" = 1 \
    || fail "$label must declare exactly one ${deployment_target} minimum OS"

  unexpected_libraries="$(
    /usr/bin/otool -L "$macho_path" \
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
    || fail "$label links a library outside the macOS system locations"
}

verify_helper_binary() {
  local helper_path="$1"
  local label="$2"
  local displayed_requirement

  verify_arm64_system_macho "$helper_path" "$label"
  "${private_tool_environment[@]}" \
    /usr/bin/codesign --verify --strict "$helper_path" >/dev/null 2>&1 \
    || fail "$label does not pass strict code-signature verification"
  "${private_tool_environment[@]}" /usr/bin/codesign --verify --strict \
    -R="$helper_identifier_requirement" "$helper_path" >/dev/null 2>&1 \
    || fail "$label does not pass identifier-bound signature verification"
  displayed_requirement="$(
    "${private_tool_environment[@]}" \
      /usr/bin/codesign --display --requirements - "$helper_path" 2>&1
  )" || fail "$label designated requirement could not be read"
  test "$displayed_requirement" = \
    "Executable=${helper_path}"$'\n'"${helper_designated_requirement}" \
    || fail "$label does not have the exact designated requirement"
}

verify_codex_runtime() {
  local runtime_path="$1"
  local label="$2"

  assert_regular_single_link_file "$runtime_path" "$label" yes
  "${private_tool_environment[@]}" \
    /usr/bin/codesign --verify --strict -R="$codex_requirement" \
    "$runtime_path" >/dev/null 2>&1 \
    || fail "$label does not retain the pinned Developer ID signature"
}

for trusted_tool in \
  /bin/chmod /bin/cp /bin/mkdir /bin/mv /bin/pwd /bin/rm \
  /usr/bin/awk /usr/bin/codesign /usr/bin/dirname /usr/bin/ditto \
  /usr/bin/dscacheutil /usr/bin/env /usr/bin/find /usr/bin/getconf /usr/bin/id \
  /usr/bin/jq /usr/bin/lipo /usr/bin/mktemp /usr/bin/otool \
  /usr/bin/shasum /usr/bin/stat /usr/bin/swift /usr/bin/uname \
  /usr/bin/unzip /usr/bin/vtool
do
  test -x "$trusted_tool" || fail "missing trusted tool ${trusted_tool}"
done

test "$(/usr/bin/uname -s)" = Darwin || fail 'the host must be macOS'
test "$(/usr/bin/uname -m)" = arm64 || fail 'the host must be Apple Silicon'

script_directory="$(
  CDPATH= cd -- "${validated_invoked_script_path%/*}" && /bin/pwd -P
)" || fail 'the script directory could not be resolved'
readonly script_directory
readonly script_path="$validated_invoked_script_path"
assert_regular_single_link_file "$script_path" 'the app build script' yes
test "$path_identity" = "$validated_invoked_script_identity" \
  || fail 'the app build script changed after clean re-execution'
repository_root="$(
  CDPATH= cd -- "${script_directory}/.." && /bin/pwd -P
)" || fail 'the repository root could not be resolved'
readonly repository_root
assert_owned_safe_directory "$repository_root" 'the repository root'

readonly package_directory="${repository_root}/apps/macos"
readonly output_parent="${repository_root}/dist"
readonly output_root="${output_parent}/macos"
readonly app_path="${output_root}/DayWeave.app"
readonly archive_path="${output_root}/DayWeave-macOS.zip"
readonly runtime_version='0.150.1'
readonly runtime_source="/opt/homebrew/Caskroom/codex/${runtime_version}/bin/codex"
readonly runtime_contract="${repository_root}/vendor/codex-app-server/${runtime_version}"
readonly runtime_verifier="${repository_root}/scripts/verify-codex-runtime.sh"
readonly helper_builder="${repository_root}/scripts/build-macos-scheduler-helper.sh"
readonly helper_source="${repository_root}/target/aarch64-apple-darwin/release/dayweave-scheduler-helper"

assert_regular_single_link_file "$helper_builder" 'the scheduler helper build script' yes
assert_regular_single_link_file "$runtime_verifier" 'the Codex runtime verifier' yes
assert_owned_safe_directory "$package_directory" 'the Swift package directory'
assert_regular_single_link_file \
  "${package_directory}/Package.swift" 'the Swift package manifest' no
if test -e "${package_directory}/Package.resolved" || \
  test -L "${package_directory}/Package.resolved"; then
  assert_regular_single_link_file \
    "${package_directory}/Package.resolved" 'the Swift package resolution' no
fi
assert_input_tree \
  "${package_directory}/Sources" 'the Swift package source tree'
assert_input_tree \
  "${package_directory}/Resources" 'the Swift package resource tree'
for contract_file in \
  "${runtime_contract}/manifest.json" \
  "${runtime_contract}/codex_app_server_protocol.schemas.json" \
  "${runtime_contract}/codex_app_server_protocol.v2.schemas.json"
do
  assert_regular_single_link_file "$contract_file" 'a pinned runtime contract' no
done

readonly target_directory="${repository_root}/target"
ensure_owned_safe_directory "$target_directory" 'the target directory'
swift_build_root="$(
  /usr/bin/mktemp -d "${target_directory}/.DayWeave-swift.XXXXXXXX"
)" || fail 'a private Swift build directory could not be created'
assert_owned_safe_directory "$swift_build_root" 'the private Swift build directory'
test "$path_mode" = 700 \
  || fail 'the private Swift build directory must have mode 0700'
swift_build_root_identity="$path_identity"
readonly swift_build_root_identity

cleanup_swift_build_root() {
  local swift_cleanup_identity
  local swift_cleanup_links
  local swift_cleanup_metadata
  local swift_cleanup_mode
  local swift_cleanup_owner
  local swift_cleanup_type

  test -n "${swift_build_root:-}" || return 0
  case "$swift_build_root" in
    "${target_directory}"/.DayWeave-swift.*) ;;
    *) return 1 ;;
  esac
  path_has_no_symlink_components "$swift_build_root" \
    && test -d "$swift_build_root" && test ! -L "$swift_build_root" \
    || return 1
  swift_cleanup_metadata="$(
    /usr/bin/stat -f '%HT|%l|%u|%Lp|%d:%i' -- "$swift_build_root" 2>/dev/null
  )" || return 1
  IFS='|' read -r \
    swift_cleanup_type swift_cleanup_links swift_cleanup_owner \
    swift_cleanup_mode swift_cleanup_identity <<<"$swift_cleanup_metadata"
  test "$swift_cleanup_type" = Directory \
    && test "$swift_cleanup_owner" = "$current_user_id" \
    && test "$swift_cleanup_mode" = 700 \
    && test "$swift_cleanup_identity" = "$swift_build_root_identity" \
    || return 1
  /bin/rm -rf -- "$swift_build_root"
}

cleanup_swift_build_on_exit() {
  local status=$?

  trap - EXIT
  trap '' HUP INT TERM
  cleanup_swift_build_root || {
    printf 'macOS app build failed: retained unsafe Swift build data at %s\n' \
      "$swift_build_root" >&2
    status=1
  }
  exit "$status"
}
trap cleanup_swift_build_on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

readonly private_tool_home="${swift_build_root}/home"
readonly private_tool_temporary_directory="${swift_build_root}/tmp"
readonly swift_scratch_directory="${swift_build_root}/scratch"
/bin/mkdir -m 0700 -- \
  "$private_tool_home" "$private_tool_temporary_directory" \
  "$swift_scratch_directory"
for private_directory in \
  "$private_tool_home" "$private_tool_temporary_directory" \
  "$swift_scratch_directory"
do
  assert_owned_safe_directory "$private_directory" 'a private build directory'
  test "$path_mode" = 700 \
    || fail 'a private build directory must have mode 0700'
done
private_tool_environment=(
  /usr/bin/env -i
  "HOME=${private_tool_home}"
  'LANG=en_US.UTF-8'
  'LC_ALL=C'
  'PATH=/usr/bin:/bin:/usr/sbin:/sbin'
  "TMPDIR=${private_tool_temporary_directory}"
)
readonly private_tool_environment

"${private_tool_environment[@]}" /usr/bin/swift build \
  --package-path "$package_directory" \
  --configuration release \
  --scratch-path "$swift_scratch_directory" \
  -Xswiftc -warnings-as-errors

binary_path="$(
  "${private_tool_environment[@]}" /usr/bin/swift build \
    --package-path "$package_directory" \
    --configuration release \
    --scratch-path "$swift_scratch_directory" \
    --show-bin-path
)/DayWeave"
readonly binary_path
verify_arm64_system_macho "$binary_path" 'the DayWeave release binary'
binary_source_identity="$path_identity"
binary_source_hash="$(sha256_file "$binary_path" 'the DayWeave release binary')"
readonly binary_source_identity binary_source_hash

"$helper_builder"
verify_helper_binary "$helper_source" 'the published scheduler helper'
helper_source_identity="$path_identity"
helper_source_hash="$(sha256_file "$helper_source" 'the published scheduler helper')"
readonly helper_source_identity helper_source_hash

"$runtime_verifier"
expected_runtime_hash="$(
  /usr/bin/jq -er '
    .executable.sha256
    | select(type == "string" and test("^[0-9a-f]{64}$"))
  ' "${runtime_contract}/manifest.json"
)" || fail 'the pinned Codex runtime SHA-256 could not be read'
readonly expected_runtime_hash
assert_mutable_parent_regular_single_link_file \
  "$runtime_source" 'the verified Codex runtime source'
runtime_source_identity="$path_identity"
test "$(sha256_file "$runtime_source" 'the verified Codex runtime source')" = \
  "$expected_runtime_hash" \
  || fail 'the Codex runtime source no longer matches its verified contract'
readonly runtime_source_identity

ensure_owned_safe_directory "$output_parent" 'the distribution directory'
ensure_owned_safe_directory "$output_root" 'the macOS distribution directory'

build_root="$(/usr/bin/mktemp -d "${output_root}/.DayWeave-build.XXXXXXXX")" \
  || fail 'a private app build directory could not be created'
assert_owned_safe_directory "$build_root" 'the private app build directory'
test "$path_mode" = 700 \
  || fail 'the private app build directory must have mode 0700'
build_root_identity="$path_identity"
readonly build_root_identity
app_publish_pending=false
app_publish_committed=false
had_previous_app=false
previous_app=''
previous_app_identity=''
archive_publish_pending=false
archive_publish_committed=false
had_previous_archive=false
previous_archive=''
previous_archive_identity=''

cleanup() {
  local status=$?
  local cleanup_allowed=true
  local cleanup_identity
  local cleanup_links
  local cleanup_metadata
  local cleanup_mode
  local cleanup_owner
  local cleanup_type
  local previous_archive_is_retained=false
  local previous_is_retained=false
  local rejected_app
  local rejected_archive
  local restoration_identity

  trap - EXIT
  trap '' HUP INT TERM
  if test "${archive_publish_pending:-false}" = true && \
    test "${archive_publish_committed:-false}" != true; then
    rejected_archive="${build_root}/Rejected.zip"
    if test -e "$previous_archive" || test -L "$previous_archive"; then
      previous_archive_is_retained=true
      restoration_identity="$(
        /usr/bin/stat -f '%d:%i' -- "$previous_archive" 2>/dev/null
      )" || restoration_identity=''
      if test "$restoration_identity" != "$previous_archive_identity"; then
        printf '%s\n' \
          'macOS app build failed: refusing to restore a changed prior archive' >&2
        cleanup_allowed=false
        status=1
      fi
    fi
    if test "$cleanup_allowed" = true && \
      { test "${had_previous_archive:-false}" != true || \
        test "$previous_archive_is_retained" = true; }; then
      if test -e "$archive_path" || test -L "$archive_path"; then
        if test -e "$rejected_archive" || test -L "$rejected_archive" || \
          ! /bin/mv -- "$archive_path" "$rejected_archive"; then
          printf '%s\n' \
            'macOS app build failed: could not move the rejected archive aside' >&2
          cleanup_allowed=false
          status=1
        fi
      fi
    fi
    if test "$cleanup_allowed" = true && \
      test "$previous_archive_is_retained" = true; then
      if test -e "$archive_path" || test -L "$archive_path" || \
        ! /bin/mv -- "$previous_archive" "$archive_path"; then
        printf '%s\n' \
          'macOS app build failed: could not restore the previous archive' >&2
        cleanup_allowed=false
        status=1
      else
        printf '%s\n' \
          'Restored the previous archive after packaging failed.' >&2
      fi
    fi
  fi
  if test "${app_publish_pending:-false}" = true && \
    test "${app_publish_committed:-false}" != true; then
    rejected_app="${build_root}/Rejected.app"
    if test -e "$previous_app" || test -L "$previous_app"; then
      previous_is_retained=true
      restoration_identity="$(
        /usr/bin/stat -f '%d:%i' -- "$previous_app" 2>/dev/null
      )" || restoration_identity=''
      if test "$restoration_identity" != "$previous_app_identity"; then
        printf '%s\n' \
          'macOS app build failed: refusing to restore a changed prior app' >&2
        cleanup_allowed=false
        status=1
      fi
    fi
    # If a prior app was expected but Previous.app is absent, the signal or
    # failure arrived before the retaining rename committed. Leave app_path
    # untouched. Otherwise app_path is the newly published candidate.
    if test "$cleanup_allowed" = true && \
      { test "${had_previous_app:-false}" != true || \
        test "$previous_is_retained" = true; }; then
      if test -e "$app_path" || test -L "$app_path"; then
        if test -e "$rejected_app" || test -L "$rejected_app" || \
          ! /bin/mv -- "$app_path" "$rejected_app"; then
          printf '%s\n' \
            'macOS app build failed: could not move the rejected app aside' >&2
          cleanup_allowed=false
          status=1
        fi
      fi
    fi
    if test "$cleanup_allowed" = true && \
      test "$previous_is_retained" = true; then
      if test -e "$app_path" || test -L "$app_path" || \
        ! /bin/mv -- "$previous_app" "$app_path"; then
        printf '%s\n' \
          'macOS app build failed: could not restore the previous app bundle' >&2
        cleanup_allowed=false
        status=1
      else
        printf '%s\n' \
          'Restored the previous app bundle after packaging failed.' >&2
      fi
    fi
  fi
  if test -n "${build_root:-}"; then
    case "$build_root" in
      "${output_root}"/.DayWeave-build.*)
        if path_has_no_symlink_components "$build_root" && \
          test -d "$build_root" && test ! -L "$build_root"; then
          cleanup_metadata="$(
            /usr/bin/stat -f '%HT|%l|%u|%Lp|%d:%i' -- "$build_root" 2>/dev/null
          )" || cleanup_metadata=''
          IFS='|' read -r \
            cleanup_type cleanup_links cleanup_owner cleanup_mode cleanup_identity \
            <<<"$cleanup_metadata"
          if test "$cleanup_allowed" = true && \
            test "$cleanup_type" = Directory && \
            test "$cleanup_owner" = "$current_user_id" && \
            test "$cleanup_mode" = 700 && \
            test "$cleanup_identity" = "$build_root_identity"; then
            /bin/rm -rf -- "$build_root" || status=1
          else
            if test "$cleanup_allowed" = true; then
              printf '%s\n' \
                'macOS app build failed: refusing unsafe build-directory cleanup' >&2
            else
              printf 'macOS app build failed: retained recovery data at %s\n' \
                "$build_root" >&2
            fi
            status=1
          fi
        elif test -e "$build_root" || test -L "$build_root"; then
          printf '%s\n' \
            'macOS app build failed: refusing unsafe build-directory cleanup' >&2
          status=1
        fi
        ;;
      *)
        printf '%s\n' \
          'macOS app build failed: refusing out-of-bound build-directory cleanup' >&2
        status=1
        ;;
    esac
  fi
  if ! cleanup_swift_build_root; then
    printf 'macOS app build failed: retained unsafe Swift build data at %s\n' \
      "$swift_build_root" >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

readonly staged_app="${build_root}/DayWeave.app"
readonly staged_helper="${staged_app}/Contents/Helpers/dayweave-scheduler-helper"
readonly staged_runtime="${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex"

/bin/mkdir -m 0700 -- "$staged_app"
/bin/mkdir -m 0700 -- \
  "${staged_app}/Contents" \
  "${staged_app}/Contents/MacOS" \
  "${staged_app}/Contents/Helpers" \
  "${staged_app}/Contents/Resources" \
  "${staged_app}/Contents/Resources/CodexRuntime" \
  "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}"
assert_owned_safe_directory "${staged_app}/Contents/Helpers" \
  'the staged Helpers directory'

/bin/cp -p "$binary_path" "${staged_app}/Contents/MacOS/DayWeave"
verify_arm64_system_macho \
  "${staged_app}/Contents/MacOS/DayWeave" 'the staged DayWeave executable'
test "$(
  sha256_file \
    "${staged_app}/Contents/MacOS/DayWeave" 'the staged DayWeave executable'
)" = "$binary_source_hash" \
  || fail 'the staged DayWeave executable differs from the verified build'
verify_arm64_system_macho "$binary_path" 'the DayWeave release binary'
test "$path_identity" = "$binary_source_identity" \
  || fail 'the DayWeave release binary changed while it was copied'
test "$(sha256_file "$binary_path" 'the DayWeave release binary')" = \
  "$binary_source_hash" \
  || fail 'the DayWeave release binary changed while it was copied'
/bin/cp -p "${package_directory}/Resources/Info.plist" \
  "${staged_app}/Contents/Info.plist"
/bin/cp -p "$runtime_source" "$staged_runtime"
/bin/cp -p \
  "${runtime_contract}/manifest.json" \
  "${runtime_contract}/codex_app_server_protocol.schemas.json" \
  "${runtime_contract}/codex_app_server_protocol.v2.schemas.json" \
  "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/"
/bin/chmod 500 "$staged_runtime"
verify_codex_runtime "$staged_runtime" 'the staged Codex runtime'
runtime_hash="$(sha256_file "$staged_runtime" 'the staged Codex runtime')"
test "$runtime_hash" = "$expected_runtime_hash" \
  || fail 'the staged Codex runtime differs from its verified contract'
readonly runtime_hash
assert_mutable_parent_regular_single_link_file \
  "$runtime_source" 'the verified Codex runtime source'
test "$path_identity" = "$runtime_source_identity" \
  || fail 'the Codex runtime source changed while it was copied'
test "$(sha256_file "$runtime_source" 'the verified Codex runtime source')" = \
  "$expected_runtime_hash" \
  || fail 'the Codex runtime source changed while it was copied'

# Revalidate the exact published inode immediately before copying it. The
# destination is a fresh private directory, and the post-copy digest proves
# that the embedded executable is the artifact verified above.
verify_helper_binary "$helper_source" 'the published scheduler helper'
test "$path_identity" = "$helper_source_identity" \
  || fail 'the published scheduler helper changed before it could be copied'
test "$(sha256_file "$helper_source" 'the published scheduler helper')" = \
  "$helper_source_hash" \
  || fail 'the published scheduler helper changed before it could be copied'
test ! -e "$staged_helper" && test ! -L "$staged_helper" \
  || fail 'the staged scheduler helper destination must not already exist'
/bin/cp -p "$helper_source" "$staged_helper"
/bin/chmod 500 "$staged_helper"
verify_helper_binary "$staged_helper" 'the staged scheduler helper'
staged_helper_identity="$path_identity"
test "$staged_helper_identity" != "$helper_source_identity" \
  || fail 'the staged scheduler helper must not alias the published inode'
test "$(sha256_file "$staged_helper" 'the staged scheduler helper')" = \
  "$helper_source_hash" \
  || fail 'the staged scheduler helper differs from the verified source'
verify_helper_binary "$helper_source" 'the published scheduler helper'
test "$path_identity" = "$helper_source_identity" \
  || fail 'the published scheduler helper changed while it was copied'
test "$(sha256_file "$helper_source" 'the published scheduler helper')" = \
  "$helper_source_hash" \
  || fail 'the published scheduler helper changed while it was copied'

# Direct local distribution uses a reproducible ad-hoc outer signature until
# the owner enrolls in the Apple Developer Program. Nested code is already
# signed and verified above. Never use --deep while signing: that would replace
# the pinned Developer ID Codex signature and the helper's exact requirement.
"${private_tool_environment[@]}" \
  /usr/bin/codesign --force --sign - --timestamp=none "$staged_app" >/dev/null
"${private_tool_environment[@]}" \
  /usr/bin/codesign --verify --deep --strict --verbose=2 "$staged_app"
verify_codex_runtime "$staged_runtime" 'the signed app Codex runtime'
test "$(sha256_file "$staged_runtime" 'the signed app Codex runtime')" = \
  "$runtime_hash" \
  || fail 'outer app signing changed the Codex runtime'
verify_helper_binary "$staged_helper" 'the signed app scheduler helper'
test "$path_identity" = "$staged_helper_identity" \
  || fail 'outer app signing replaced the scheduler helper inode'
test "$(sha256_file "$staged_helper" 'the signed app scheduler helper')" = \
  "$helper_source_hash" \
  || fail 'outer app signing changed the scheduler helper'
assert_owned_safe_directory "$staged_app" 'the verified staged app bundle'
staged_app_identity="$path_identity"
staged_app_device="${staged_app_identity%%:*}"
assert_owned_safe_directory "$output_root" 'the macOS distribution directory'
output_device="${path_identity%%:*}"
test "$staged_app_device" = "$output_device" \
  || fail 'the staged app and publication directory must share a filesystem'

previous_app="${build_root}/Previous.app"
if test -e "$app_path" || test -L "$app_path"; then
  assert_owned_safe_directory "$app_path" 'the existing app bundle'
  previous_app_identity="$path_identity"
  app_publish_pending=true had_previous_app=true
  /bin/mv -- "$app_path" "$previous_app" \
    || fail 'the previous app bundle could not be retained for rollback'
  assert_owned_safe_directory "$previous_app" 'the retained previous app bundle'
  test "$path_identity" = "$previous_app_identity" \
    || fail 'the retained previous app is not the original bundle'
else
  app_publish_pending=true had_previous_app=false
fi
/bin/mv -- "$staged_app" "$app_path" \
  || fail 'the verified app bundle could not be published'
assert_owned_safe_directory "$app_path" 'the published app bundle'
test "$path_identity" = "$staged_app_identity" \
  || fail 'the published app is not the verified staged bundle'

readonly archive_candidate="${build_root}/DayWeave-macOS.zip"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent \
  "$app_path" "$archive_candidate"
assert_regular_single_link_file "$archive_candidate" \
  'the staged macOS archive' no
/usr/bin/unzip -tq "$archive_candidate"

readonly archive_validation_root="${build_root}/archive-validation"
/bin/mkdir -m 0700 -- "$archive_validation_root"
/usr/bin/ditto -x -k "$archive_candidate" "$archive_validation_root"
readonly archived_app="${archive_validation_root}/DayWeave.app"
top_level_entries="$(
  /usr/bin/find "$archive_validation_root" -mindepth 1 -maxdepth 1 -print
)" || fail 'the extracted archive contents could not be listed'
test "$top_level_entries" = "$archived_app" \
  || fail 'the archive must contain exactly one top-level DayWeave.app bundle'
assert_owned_safe_directory "$archived_app" 'the archived app bundle'
"${private_tool_environment[@]}" \
  /usr/bin/codesign --verify --deep --strict --verbose=2 "$archived_app"
verify_codex_runtime \
  "${archived_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex" \
  'the archived Codex runtime'
test "$(
  sha256_file \
    "${archived_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex" \
    'the archived Codex runtime'
)" = "$runtime_hash" || fail 'the archive changed the Codex runtime'
verify_helper_binary \
  "${archived_app}/Contents/Helpers/dayweave-scheduler-helper" \
  'the archived scheduler helper'
test "$(
  sha256_file \
    "${archived_app}/Contents/Helpers/dayweave-scheduler-helper" \
    'the archived scheduler helper'
)" = "$helper_source_hash" \
  || fail 'the archive changed the scheduler helper'

if test -e "$archive_path" || test -L "$archive_path"; then
  assert_regular_single_link_file "$archive_path" \
    'the existing macOS archive' no
  previous_archive_identity="$path_identity"
fi
archive_candidate_hash="$(sha256_file "$archive_candidate" 'the staged macOS archive')"
assert_regular_single_link_file "$archive_candidate" \
  'the validated staged macOS archive' no
archive_candidate_identity="$path_identity"
archive_candidate_device="${archive_candidate_identity%%:*}"
test "$archive_candidate_device" = "$output_device" \
  || fail 'the staged archive and publication directory must share a filesystem'
previous_archive="${build_root}/Previous.zip"
if test -e "$archive_path" || test -L "$archive_path"; then
  archive_publish_pending=true had_previous_archive=true
  /bin/mv -- "$archive_path" "$previous_archive" \
    || fail 'the previous macOS archive could not be retained for rollback'
  assert_regular_single_link_file "$previous_archive" \
    'the retained previous macOS archive' no
  test "$path_identity" = "$previous_archive_identity" \
    || fail 'the retained previous archive is not the original artifact'
else
  archive_publish_pending=true had_previous_archive=false
fi
# Ignore termination signals only across the final atomic archive rename and
# its in-memory commit marker. This prevents a signal from restoring the prior
# app after the new archive has already replaced its predecessor.
trap '' HUP INT TERM
if ! /bin/mv -- "$archive_candidate" "$archive_path"; then
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  fail 'the validated macOS archive could not be published'
fi
app_publish_committed=true
archive_publish_committed=true
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if test -e "$previous_app" || test -L "$previous_app"; then
  assert_owned_safe_directory "$previous_app" 'the previous app bundle'
  /bin/rm -rf -- "$previous_app"
fi
if test -e "$previous_archive" || test -L "$previous_archive"; then
  assert_regular_single_link_file "$previous_archive" \
    'the previous macOS archive' no
  /bin/rm -- "$previous_archive"
fi

printf 'Built and verified %s\n' "$app_path"
printf '%s  %s\n' "$archive_candidate_hash" "$archive_path"
printf 'Built and verified %s\n' "$archive_path"
