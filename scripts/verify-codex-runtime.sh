#!/bin/bash -p

# This verifier is a development attestation boundary. It must be executed
# directly so the kernel supplies `-p` before Bash considers BASH_ENV or
# exported functions. An explicit `bash verify-codex-runtime.sh` is unsupported
# and fails before the verifier performs any work.
case $- in
  *p*) ;;
  *)
    builtin printf '%s\n' \
      'Codex runtime verification failed: invoke this executable directly (privileged Bash mode is required)' >&2
    builtin exit 126
    ;;
esac

# Re-exec once with a minimal environment. The whitelist check makes a forged
# marker harmless: any additional inherited entry causes another clean re-exec.
dw_bootstrap_environment_is_clean=true
if test "${DW_CODEX_VERIFIER_CLEAN_ENV:-}" != 1 \
  || test "${PATH:-}" != /usr/bin:/bin:/usr/sbin:/sbin \
  || test "${LANG:-}" != en_US.UTF-8 \
  || test "${LC_ALL:-}" != C; then
  dw_bootstrap_environment_is_clean=false
else
  while IFS= read -r -d '' dw_bootstrap_entry; do
    dw_bootstrap_name=${dw_bootstrap_entry%%=*}
    case "$dw_bootstrap_name" in
      DW_CODEX_VERIFIER_CLEAN_ENV|HOME|LANG|LC_ALL|OLDPWD|PATH|PWD|SHLVL|_) ;;
      *) dw_bootstrap_environment_is_clean=false ;;
    esac
  done < <(/usr/bin/env -0)
fi

if test "$dw_bootstrap_environment_is_clean" != true; then
  test -n "${HOME:-}" || {
    builtin printf '%s\n' \
      'Codex runtime verification failed: HOME must be set' >&2
    builtin exit 1
  }
  exec /usr/bin/env -i \
    HOME="$HOME" \
    LANG=en_US.UTF-8 \
    LC_ALL=C \
    PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    DW_CODEX_VERIFIER_CLEAN_ENV=1 \
    /bin/bash -p "$0" "$@"
  builtin printf '%s\n' \
    'Codex runtime verification failed: could not enter the clean environment' >&2
  builtin exit 1
fi

unset dw_bootstrap_entry dw_bootstrap_name dw_bootstrap_environment_is_clean

set -euo pipefail
# In a non-interactive shell this gives each asynchronous job a distinct
# process group. The verifier validates that invariant before registering it.
set -m

umask 077

readonly DW_JQ=/usr/bin/jq
readonly DW_SHASUM=/usr/bin/shasum
readonly DW_CODESIGN=/usr/bin/codesign
readonly DW_SANDBOX_EXEC=/usr/bin/sandbox-exec
readonly DW_STAT=/usr/bin/stat
readonly DW_MKTEMP=/usr/bin/mktemp
readonly DW_MKFIFO=/usr/bin/mkfifo
readonly DW_FIND=/usr/bin/find
readonly DW_CMP=/usr/bin/cmp
readonly DW_AWK=/usr/bin/awk
readonly DW_DU=/usr/bin/du
readonly DW_ID=/usr/bin/id
readonly DW_DIRNAME=/usr/bin/dirname
readonly DW_WC=/usr/bin/wc
readonly DW_TR=/usr/bin/tr
readonly DW_ENV=/usr/bin/env
readonly DW_PWD=/bin/pwd
readonly DW_MKDIR=/bin/mkdir
readonly DW_CHMOD=/bin/chmod
readonly DW_CP=/bin/cp
readonly DW_RM=/bin/rm
readonly DW_KILL=/bin/kill
readonly DW_SLEEP=/bin/sleep
readonly DW_DATE=/bin/date
readonly DW_CAT=/bin/cat
readonly DW_PS=/bin/ps

readonly DW_EXPECTED_SOURCE=/opt/homebrew/Caskroom/codex/0.150.1/bin/codex
readonly DW_EXPECTED_VERSION='codex-cli 0.150.1'
readonly DW_EXPECTED_BINARY_HASH=a14f9a907c12c8812878b70e6b7d65f81c39ed795513e46a55817d7428c0ca6b
readonly DW_EXPECTED_TEAM=2DC432GLL2
readonly DW_EXPECTED_DESIGNATED='identifier codex and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = 2DC432GLL2'
readonly DW_EXPECTED_REQUIREMENT='identifier "codex" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "2DC432GLL2"'
readonly DW_EXPECTED_MANIFEST_HASH=e95c31a03fe867f7242d995ad099ca6903c432876ef70068d90385b1d5230084
readonly DW_LEGACY_SCHEMA=codex_app_server_protocol.schemas.json
readonly DW_LEGACY_SCHEMA_HASH=18ba0e2282f69f7b3a05ffdc8ab0801c1468f25d72de3b4a37f1c8be67432a1d
readonly DW_V2_SCHEMA=codex_app_server_protocol.v2.schemas.json
readonly DW_V2_SCHEMA_HASH=8cdccfc35582696d7141e7f916e0d5a664ab5b5e90b732f104284d2507f369f8

fail() {
  printf 'Codex runtime verification failed: %s\n' "$1" >&2
  exit 1
}

test "$#" -eq 0 || fail "the verifier does not accept arguments"
case $- in
  *p*) ;;
  *) fail "required privileged shell mode is unavailable" ;;
esac
case $- in
  *m*) ;;
  *) fail "required job-control shell mode is unavailable" ;;
esac

for dw_tool in \
  "$DW_JQ" "$DW_SHASUM" "$DW_CODESIGN" "$DW_SANDBOX_EXEC" "$DW_STAT" \
  "$DW_MKTEMP" "$DW_MKFIFO" "$DW_FIND" "$DW_CMP" "$DW_AWK" "$DW_DU" "$DW_ID" \
  "$DW_DIRNAME" "$DW_WC" "$DW_TR" "$DW_ENV" "$DW_PWD" \
  "$DW_MKDIR" "$DW_CHMOD" "$DW_CP" "$DW_RM" "$DW_KILL" "$DW_SLEEP" \
  "$DW_DATE" "$DW_CAT" "$DW_PS"
do
  test -x "$dw_tool" || fail "missing trusted tool $dw_tool"
done

require_safe_profile_path() {
  case "$1" in
    *'"'*|*'\'*|*$'\n'*|*$'\r'*|*$'\t'*) fail "unsupported character in sandbox path" ;;
  esac
}

require_no_symlink_components() {
  local dw_path=$1
  local dw_remaining
  local dw_component
  local dw_current=

  case "$dw_path" in
    /|/*/) fail "unsafe path shape: $dw_path" ;;
    /*) ;;
    *) fail "path must be absolute: $dw_path" ;;
  esac
  case "$dw_path" in
    *'//'*) fail "path contains an empty component: $dw_path" ;;
  esac

  dw_remaining=${dw_path#/}
  while test -n "$dw_remaining"; do
    case "$dw_remaining" in
      */*)
        dw_component=${dw_remaining%%/*}
        dw_remaining=${dw_remaining#*/}
        ;;
      *)
        dw_component=$dw_remaining
        dw_remaining=
        ;;
    esac
    case "$dw_component" in
      ''|.|..) fail "unsafe path component in $dw_path" ;;
    esac
    dw_current="$dw_current/$dw_component"
    test ! -L "$dw_current" || fail "symlink path component: $dw_current"
    test -e "$dw_current" || fail "missing path component: $dw_current"
  done
}

require_owned_directory() {
  local dw_path=$1
  local dw_expected_owner=$2

  require_no_symlink_components "$dw_path"
  test -d "$dw_path" || fail "not a directory: $dw_path"
  test "$($DW_STAT -f %u "$dw_path")" = "$dw_expected_owner" \
    || fail "directory has an unexpected owner: $dw_path"
}

require_private_directory() {
  local dw_path=$1
  local dw_expected_owner=$2

  require_owned_directory "$dw_path" "$dw_expected_owner"
  test "$($DW_STAT -f %Lp "$dw_path")" = 700 \
    || fail "directory must have mode 0700: $dw_path"
}

require_not_writable_by_others() {
  local dw_path=$1
  local dw_mode

  dw_mode=$($DW_STAT -f %Lp "$dw_path")
  test $((8#$dw_mode & 0022)) -eq 0 \
    || fail "directory is writable by group or other users: $dw_path"
}

sha256_file() {
  "$DW_SHASUM" -a 256 "$1" | "$DW_AWK" '{print $1}'
}

file_size() {
  if test -f "$1"; then
    "$DW_STAT" -f %z "$1"
  else
    printf '0\n'
  fi
}

path_identity() {
  "$DW_STAT" -f '%d:%i:%u:%Lp' "$1"
}

dw_probe_home=
dw_probe_identity=
dw_verification_root=
dw_verification_root_identity=
dw_outside_probe=
# Index zero is an inert sentinel, avoiding empty-array edge cases in the
# system Bash 3.2 shipped by macOS.
dw_child_pids=(0)
dw_child_pgids=(0)
dw_child_labels=(sentinel)
dw_shutdown_started=false
dw_shutdown_status=0
dw_first_signal_status=0

latch_signal_status() {
  local dw_candidate_status=$1
  if test "$dw_first_signal_status" -eq 0; then
    dw_first_signal_status=$dw_candidate_status
  fi
}

handle_lifecycle_signal() {
  latch_signal_status "$1"
  # If EXIT processing has begun, the controller must finish reap and cleanup.
  # The shutdown prologue will make these dispositions SIG_IGN permanently.
  test "$dw_shutdown_started" = false || return 0
  trap '' INT TERM HUP QUIT
  exit "$dw_first_signal_status"
}

process_state() {
  { "$DW_PS" -o state= -p "$1" 2>/dev/null || true; } \
    | "$DW_AWK" 'NR == 1 { print substr($1, 1, 1) }'
}

process_group_has_members() {
  local dw_group=$1
  "$DW_PS" -axo pgid= | "$DW_AWK" -v group="$dw_group" '
    $1 == group { found = 1 }
    END { exit(found ? 0 : 1) }
  '
}

register_child_group() {
  local dw_pid=$1
  local dw_label=$2
  local dw_group
  local dw_index

  dw_index=${#dw_child_pids[@]}
  dw_child_pids[$dw_index]=$dw_pid
  dw_child_pgids[$dw_index]=$dw_pid
  dw_child_labels[$dw_index]=$dw_label

  # `set -m` makes the asynchronous job its own group leader. If the short-
  # lived job has already exited, wait still reaps the registered PID. If it is
  # live, require the exact group before continuing.
  if "$DW_KILL" -0 "$dw_pid" >/dev/null 2>&1; then
    dw_group=$("$DW_PS" -o pgid= -p "$dw_pid" 2>/dev/null | "$DW_TR" -d ' ')
    test "$dw_group" = "$dw_pid" \
      || fail "could not isolate $dw_label in its own process group"
  fi
}

unregister_child() {
  local dw_pid=$1
  local dw_index
  for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
    if test "${dw_child_pids[$dw_index]}" = "$dw_pid"; then
      if process_group_has_members "${dw_child_pgids[$dw_index]}"; then
        return 2
      fi
      dw_child_pids[$dw_index]=0
      dw_child_pgids[$dw_index]=0
      dw_child_labels[$dw_index]=reaped
      return 0
    fi
  done
  return 1
}

reap_ready_children() {
  local dw_index
  local dw_pid
  local dw_state
  local dw_wait_status

  for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
    dw_pid=${dw_child_pids[$dw_index]}
    test "$dw_pid" -gt 0 || continue
    dw_state=$(process_state "$dw_pid")
    if test -z "$dw_state" || test "$dw_state" = Z; then
      set +e
      wait "$dw_pid" >/dev/null 2>&1
      dw_wait_status=$?
      set -e
      # Any status is expected during shutdown; wait's purpose here is reaping.
      : "$dw_wait_status"
      dw_child_pids[$dw_index]=0
      if ! process_group_has_members "${dw_child_pgids[$dw_index]}"; then
        dw_child_pgids[$dw_index]=0
        dw_child_labels[$dw_index]=reaped
      fi
    fi
  done
}

terminate_all_children() {
  local dw_index
  local dw_pid
  local dw_group
  local dw_deadline
  local dw_now
  local dw_pending

  # First ask every still-owned process group to stop. Negative PIDs address
  # the runner/feeder and every descendant in that group.
  for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
    dw_pid=${dw_child_pids[$dw_index]}
    dw_group=${dw_child_pgids[$dw_index]}
    test "$dw_pid" -gt 0 && test "$dw_group" -gt 0 || continue
    "$DW_KILL" -TERM -- "-$dw_group" >/dev/null 2>&1 || true
  done

  dw_deadline=$(( $("$DW_DATE" +%s) + 3 ))
  while :; do
    reap_ready_children
    dw_pending=false
    for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
      dw_pid=${dw_child_pids[$dw_index]}
      test "$dw_pid" -gt 0 || continue
      dw_pending=true
      break
    done
    test "$dw_pending" = true || break
    dw_now=$("$DW_DATE" +%s)
    test "$dw_now" -lt "$dw_deadline" || break
    "$DW_SLEEP" 0.1
  done

  # Escalate every group whose direct child did not become reapable.
  for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
    dw_pid=${dw_child_pids[$dw_index]}
    dw_group=${dw_child_pgids[$dw_index]}
    test "$dw_pid" -gt 0 && test "$dw_group" -gt 0 || continue
    "$DW_KILL" -KILL -- "-$dw_group" >/dev/null 2>&1 || true
  done

  dw_deadline=$(( $("$DW_DATE" +%s) + 3 ))
  while :; do
    reap_ready_children
    dw_pending=false
    for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
      dw_pid=${dw_child_pids[$dw_index]}
      test "$dw_pid" -gt 0 || continue
      dw_pending=true
      break
    done
    test "$dw_pending" = true || break
    dw_now=$("$DW_DATE" +%s)
    test "$dw_now" -lt "$dw_deadline" || break
    "$DW_SLEEP" 0.1
  done

  # Reap every remaining direct child. SIGKILL should make this immediate. In
  # the pathological case of an uninterruptible kernel wait, remaining blocked
  # here is safer than exiting and orphaning a verifier-owned child to PID 1.
  for ((dw_index = 1; dw_index < ${#dw_child_pids[@]}; dw_index++)); do
    dw_pid=${dw_child_pids[$dw_index]}
    test "$dw_pid" -gt 0 || continue
    dw_group=${dw_child_pgids[$dw_index]}
    "$DW_KILL" -KILL -- "-$dw_group" >/dev/null 2>&1 || true
    set +e
    wait "$dw_pid" >/dev/null 2>&1
    set -e
    dw_child_pids[$dw_index]=0
    if ! process_group_has_members "$dw_group"; then
      dw_child_pgids[$dw_index]=0
      dw_child_labels[$dw_index]=reaped
    fi
  done

  # A registered group must be completely empty, including grandchildren,
  # before cleanup. This also catches a sandbox wrapper that exited first.
  dw_deadline=$(( $("$DW_DATE" +%s) + 2 ))
  while :; do
    dw_pending=false
    for ((dw_index = 1; dw_index < ${#dw_child_pgids[@]}; dw_index++)); do
      dw_group=${dw_child_pgids[$dw_index]}
      test "$dw_group" -gt 0 || continue
      if process_group_has_members "$dw_group"; then
        dw_pending=true
        "$DW_KILL" -KILL -- "-$dw_group" >/dev/null 2>&1 || true
      else
        dw_child_pgids[$dw_index]=0
        dw_child_labels[$dw_index]=reaped
      fi
    done
    test "$dw_pending" = true || break
    dw_now=$("$DW_DATE" +%s)
    test "$dw_now" -lt "$dw_deadline" || break
    "$DW_SLEEP" 0.1
  done
  # Do not exit while a descendant remains. The sandbox contract forbids the
  # runtime from forking, so reaching this fail-stop loop would indicate a
  # wrapper/kernel anomaly; repeated SIGKILL avoids creating a PID-1 survivor.
  while test "$dw_pending" = true; do
    for ((dw_index = 1; dw_index < ${#dw_child_pgids[@]}; dw_index++)); do
      dw_group=${dw_child_pgids[$dw_index]}
      test "$dw_group" -gt 0 || continue
      process_group_has_members "$dw_group" || continue
      "$DW_KILL" -KILL -- "-$dw_group" >/dev/null 2>&1 || true
    done
    "$DW_SLEEP" 0.1
    dw_pending=false
    for ((dw_index = 1; dw_index < ${#dw_child_pgids[@]}; dw_index++)); do
      dw_group=${dw_child_pgids[$dw_index]}
      test "$dw_group" -gt 0 || continue
      if process_group_has_members "$dw_group"; then
        dw_pending=true
        break
      else
        dw_child_pgids[$dw_index]=0
        dw_child_labels[$dw_index]=reaped
      fi
    done
  done

  for ((dw_index = 1; dw_index < ${#dw_child_pgids[@]}; dw_index++)); do
    dw_child_pids[$dw_index]=0
    dw_child_pgids[$dw_index]=0
  done
  return 0
}

cleanup() {
  local dw_cleanup_status=0
  local dw_current_identity

  if test -n "${dw_outside_probe:-}" \
    && { test -e "$dw_outside_probe" || test -L "$dw_outside_probe"; }; then
    if test -f "$dw_outside_probe" || test -L "$dw_outside_probe"; then
      "$DW_RM" -- "$dw_outside_probe" || dw_cleanup_status=1
    else
      printf 'Refusing to remove unexpected outside-probe object: %s\n' \
        "$dw_outside_probe" >&2
      dw_cleanup_status=1
    fi
  fi

  if test -n "${dw_probe_home:-}" \
    && { test -e "$dw_probe_home" || test -L "$dw_probe_home"; }; then
    case "$dw_probe_home" in
      "$dw_verification_root"/probe.*) ;;
      *)
        printf 'Refusing cleanup outside verification root: %s\n' "$dw_probe_home" >&2
        dw_cleanup_status=1
        ;;
    esac
    if test "$dw_cleanup_status" -eq 0; then
      if test -L "$dw_probe_home"; then
        "$DW_RM" -- "$dw_probe_home" || dw_cleanup_status=1
      elif test -d "$dw_probe_home"; then
        dw_current_identity=$(path_identity "$dw_probe_home" 2>/dev/null)
        if test "$dw_current_identity" != "$dw_probe_identity" \
          || test "$(path_identity "$dw_verification_root" 2>/dev/null)" \
            != "$dw_verification_root_identity"; then
          printf 'Refusing cleanup after directory identity changed: %s\n' \
            "$dw_probe_home" >&2
          dw_cleanup_status=1
        else
          "$DW_FIND" -P "$dw_probe_home" -depth -delete || dw_cleanup_status=1
        fi
      else
        printf 'Refusing cleanup of unexpected probe object: %s\n' "$dw_probe_home" >&2
        dw_cleanup_status=1
      fi
    fi
  fi

  return "$dw_cleanup_status"
}

shutdown() {
  local dw_original_status=$dw_shutdown_status
  local dw_final_status
  local dw_child_status=0
  local dw_cleanup_status=0

  # EXIT is removed solely to prevent recursion. Catchable lifecycle signals
  # remain ignored for the complete terminate/reap/fail-stop/cleanup sequence.
  trap - EXIT
  trap '' INT TERM HUP QUIT
  if test "$dw_first_signal_status" -ne 0; then
    dw_final_status=$dw_first_signal_status
  else
    dw_final_status=$dw_original_status
  fi

  set +e
  terminate_all_children || dw_child_status=$?
  if test "$dw_child_status" -eq 0; then
    cleanup || dw_cleanup_status=$?
  else
    printf '%s\n' \
      'Refusing verifier cleanup because a registered child survived' >&2
  fi

  if test "$dw_final_status" -eq 0 \
    && { test "$dw_child_status" -ne 0 || test "$dw_cleanup_status" -ne 0; }; then
    dw_final_status=1
  fi
  if test "$dw_final_status" -eq 0 \
    && { test -e "$dw_probe_home" || test -L "$dw_probe_home"; }; then
    printf '%s\n' 'Runtime verification directory remained after cleanup' >&2
    dw_final_status=1
  fi
  if test "$dw_final_status" -eq 0; then
    printf '%s\n' \
      'Pinned Codex App Server runtime verified with outbound-only, deny-by-default macOS containment.'
  fi
  exit "$dw_final_status"
}

begin_child_launch() {
  trap 'latch_signal_status 130' INT
  trap 'latch_signal_status 143' TERM
  trap 'latch_signal_status 129' HUP
  trap 'latch_signal_status 131' QUIT
}

finish_child_launch() {
  local dw_signal_status
  trap 'handle_lifecycle_signal 130' INT
  trap 'handle_lifecycle_signal 143' TERM
  trap 'handle_lifecycle_signal 129' HUP
  trap 'handle_lifecycle_signal 131' QUIT
  dw_signal_status=$dw_first_signal_status
  test "$dw_signal_status" -eq 0 \
    || handle_lifecycle_signal "$dw_signal_status"
}

trap 'dw_shutdown_status=$? dw_shutdown_started=true; shutdown' EXIT
trap 'handle_lifecycle_signal 130' INT
trap 'handle_lifecycle_signal 143' TERM
trap 'handle_lifecycle_signal 129' HUP
trap 'handle_lifecycle_signal 131' QUIT

run_bounded() {
  local dw_label=$1
  local dw_deadline_seconds=$2
  local dw_stdout_cap=$3
  local dw_stderr_cap=$4
  local dw_file_blocks=$5
  local dw_stdin_path=$6
  local dw_stdout_path=$7
  local dw_stderr_path=$8
  local dw_runner_pid
  local dw_feeder_pid=0
  local dw_feeder_status=0
  local dw_fifo=
  local dw_started
  local dw_now
  local dw_exit_status
  shift 8

  : >"$dw_stdout_path"
  : >"$dw_stderr_path"
  "$DW_CHMOD" 600 "$dw_stdout_path" "$dw_stderr_path"
  # Re-open and verify the exact private inode after all launch bookkeeping and
  # immediately before sandbox-exec opens it.
  verify_runtime_copy

  if test "$dw_stdin_path" = /dev/null; then
    begin_child_launch
    (
      trap - EXIT INT TERM HUP QUIT
      ulimit -f "$dw_file_blocks"
      CDPATH= cd -- "$dw_probe_home"
      exec "$@"
    ) </dev/null >"$dw_stdout_path" 2>"$dw_stderr_path" &
    dw_runner_pid=$!
    register_child_group "$dw_runner_pid" "$dw_label runner"
    finish_child_launch
  else
    dw_fifo="$dw_probe_home/run-${#dw_child_pids[@]}.stdin"
    test ! -e "$dw_fifo" && test ! -L "$dw_fifo" \
      || fail "$dw_label FIFO path already exists"
    "$DW_MKFIFO" -m 600 "$dw_fifo"
    test -p "$dw_fifo" && test ! -L "$dw_fifo" \
      || fail "$dw_label FIFO was not created safely"

    begin_child_launch
    (
      trap - EXIT INT TERM HUP QUIT
      ulimit -f "$dw_file_blocks"
      CDPATH= cd -- "$dw_probe_home"
      exec "$@"
    ) <"$dw_fifo" >"$dw_stdout_path" 2>"$dw_stderr_path" &
    dw_runner_pid=$!
    register_child_group "$dw_runner_pid" "$dw_label runner"
    finish_child_launch

    begin_child_launch
    (
      trap - EXIT INT TERM HUP QUIT
      "$DW_CAT" "$dw_stdin_path"
      "$DW_SLEEP" 2
    ) >"$dw_fifo" &
    dw_feeder_pid=$!
    register_child_group "$dw_feeder_pid" "$dw_label feeder"
    finish_child_launch
  fi
  dw_started=$("$DW_DATE" +%s)

  while test -n "$(process_state "$dw_runner_pid")" \
    && test "$(process_state "$dw_runner_pid")" != Z; do
    if test "$(file_size "$dw_stdout_path")" -gt "$dw_stdout_cap" \
      || test "$(file_size "$dw_stderr_path")" -gt "$dw_stderr_cap"; then
      fail "$dw_label exceeded its output bound"
    fi
    dw_now=$("$DW_DATE" +%s)
    if test $((dw_now - dw_started)) -ge "$dw_deadline_seconds"; then
      fail "$dw_label exceeded its deadline"
    fi
    "$DW_SLEEP" 1
  done

  set +e
  wait "$dw_runner_pid"
  dw_exit_status=$?
  set -e
  unregister_child "$dw_runner_pid" \
    || fail "$dw_label runner group still has an unaccounted process"

  if test "$dw_feeder_pid" -gt 0; then
    set +e
    wait "$dw_feeder_pid"
    dw_feeder_status=$?
    set -e
    unregister_child "$dw_feeder_pid" \
      || fail "$dw_label feeder group still has an unaccounted process"
    test "$dw_feeder_status" -eq 0 \
      || fail "$dw_label feeder exited with status $dw_feeder_status"
  fi

  test "$dw_exit_status" -eq 0 || fail "$dw_label exited with status $dw_exit_status"
  test "$(file_size "$dw_stdout_path")" -le "$dw_stdout_cap" \
    || fail "$dw_label exceeded its stdout bound"
  test "$(file_size "$dw_stderr_path")" -le "$dw_stderr_cap" \
    || fail "$dw_label exceeded its stderr bound"
}

dw_script_dir=$(CDPATH= cd -- "$("$DW_DIRNAME" -- "$0")" && "$DW_PWD" -P)
dw_repo_root=$(CDPATH= cd -- "$dw_script_dir/.." && "$DW_PWD" -P)
dw_pin_dir="$dw_repo_root/vendor/codex-app-server/0.150.1"
dw_manifest="$dw_pin_dir/manifest.json"
dw_legacy_pin="$dw_pin_dir/$DW_LEGACY_SCHEMA"
dw_v2_pin="$dw_pin_dir/$DW_V2_SCHEMA"

require_safe_profile_path "$dw_repo_root"
require_no_symlink_components "$dw_manifest"
test -f "$dw_manifest" && test ! -L "$dw_manifest" \
  || fail "missing regular runtime manifest"
test "$(sha256_file "$dw_manifest")" = "$DW_EXPECTED_MANIFEST_HASH" \
  || fail "runtime manifest byte pin mismatch"

"$DW_JQ" -e \
  --arg source "$DW_EXPECTED_SOURCE" \
  --arg version "$DW_EXPECTED_VERSION" \
  --arg binary_hash "$DW_EXPECTED_BINARY_HASH" \
  --arg team "$DW_EXPECTED_TEAM" \
  --arg designated "$DW_EXPECTED_DESIGNATED" \
  --arg requirement "$DW_EXPECTED_REQUIREMENT" \
  --arg legacy "$DW_LEGACY_SCHEMA" \
  --arg legacy_hash "$DW_LEGACY_SCHEMA_HASH" \
  --arg v2 "$DW_V2_SCHEMA" \
  --arg v2_hash "$DW_V2_SCHEMA_HASH" '
    type == "object"
    and (keys | sort) == ["cli_version_output", "containment", "executable", "format_version", "schemas", "transport"]
    and .format_version == 1
    and .cli_version_output == $version
    and .executable == {
      "candidate_paths": [$source],
      "sha256": $binary_hash,
      "identifier": "codex",
      "team_identifier": $team,
      "designated_requirement": $designated,
      "codesign_requirement": $requirement
    }
    and .schemas == [
      {"path": $legacy, "sha256": $legacy_hash},
      {"path": $v2, "sha256": $v2_hash}
    ]
    and (.schemas | map(.path) | unique | length) == 2
    and all(.schemas[];
      (.path | type == "string" and test("^[A-Za-z0-9._-]+$") and
        contains("..") == false)
      and (.sha256 | type == "string" and test("^[0-9a-f]{64}$"))
    )
    and .transport == "stdio"
    and .containment == {
      "runner": "/usr/bin/sandbox-exec",
      "profile_version": 1,
      "deny_by_default": true,
      "allow_process_fork": false,
      "writable_scope": "isolated_codex_home_only",
      "network": "outbound_only",
      "login": "device_code_only",
      "same_user_threat_limit": "The verifier detects ordinary replacement and mutation but cannot exclude a malicious concurrent process already running as the same macOS user."
    }
  ' "$dw_manifest" >/dev/null || fail "runtime manifest is not the exact pinned contract"

for dw_schema_spec in \
  "$dw_legacy_pin:$DW_LEGACY_SCHEMA_HASH:CodexAppServerProtocol" \
  "$dw_v2_pin:$DW_V2_SCHEMA_HASH:CodexAppServerProtocolV2"
do
  dw_schema_path=${dw_schema_spec%%:*}
  dw_schema_tail=${dw_schema_spec#*:}
  dw_schema_hash=${dw_schema_tail%%:*}
  dw_schema_title=${dw_schema_tail#*:}
  require_no_symlink_components "$dw_schema_path"
  test -f "$dw_schema_path" && test ! -L "$dw_schema_path" \
    || fail "missing regular pinned schema $dw_schema_path"
  test "$(sha256_file "$dw_schema_path")" = "$dw_schema_hash" \
    || fail "schema pin mismatch for $dw_schema_path"
  "$DW_JQ" -e --arg title "$dw_schema_title" '
    type == "object"
    and .title == $title
    and .type == "object"
    and (.definitions | type == "object" and length > 0)
    and ([.. | objects | .["$ref"]?
      | select(type == "string" and (startswith("#/definitions/") | not))]
      | length) == 0
  ' "$dw_schema_path" >/dev/null || fail "schema bundle is invalid or not self-contained"
done

require_no_symlink_components "$DW_EXPECTED_SOURCE"
test -f "$DW_EXPECTED_SOURCE" && test -x "$DW_EXPECTED_SOURCE" \
  || fail "pinned executable is not installed as a regular executable"
test "$(sha256_file "$DW_EXPECTED_SOURCE")" = "$DW_EXPECTED_BINARY_HASH" \
  || fail "source executable content pin mismatch"
"$DW_CODESIGN" --verify --strict -R="$DW_EXPECTED_REQUIREMENT" \
  "$DW_EXPECTED_SOURCE" >/dev/null 2>&1 \
  || fail "source executable Developer ID requirement mismatch"

test -n "${HOME:-}" || fail "HOME must be set"
require_no_symlink_components "$HOME"
dw_user_home=$(CDPATH= cd -- "$HOME" && "$DW_PWD" -P)
test "$dw_user_home" = "$HOME" || fail "HOME must already be canonical"
case "$dw_user_home" in
  /Users/*) ;;
  *) fail "HOME must be a private macOS user home" ;;
esac
dw_user_id=$("$DW_ID" -u)
require_owned_directory "$dw_user_home" "$dw_user_id"
require_not_writable_by_others /Users
require_not_writable_by_others "$dw_user_home"
require_safe_profile_path "$dw_user_home"

dw_library="$dw_user_home/Library"
dw_application_support="$dw_library/Application Support"
dw_dayweave_support="$dw_application_support/DayWeave"
dw_verification_root="$dw_dayweave_support/RuntimeVerification"

require_owned_directory "$dw_library" "$dw_user_id"
require_not_writable_by_others "$dw_library"
if ! test -e "$dw_application_support" && ! test -L "$dw_application_support"; then
  "$DW_MKDIR" -m 700 "$dw_application_support"
fi
require_owned_directory "$dw_application_support" "$dw_user_id"
require_not_writable_by_others "$dw_application_support"
if ! test -e "$dw_dayweave_support" && ! test -L "$dw_dayweave_support"; then
  "$DW_MKDIR" -m 700 "$dw_dayweave_support"
fi
require_private_directory "$dw_dayweave_support" "$dw_user_id"
if ! test -e "$dw_verification_root" && ! test -L "$dw_verification_root"; then
  "$DW_MKDIR" -m 700 "$dw_verification_root"
fi
require_private_directory "$dw_verification_root" "$dw_user_id"
dw_verification_root_identity=$(path_identity "$dw_verification_root")

dw_probe_home=$("$DW_MKTEMP" -d "$dw_verification_root/probe.XXXXXX")
test ! -L "$dw_probe_home" || fail "mktemp returned a symlink"
require_private_directory "$dw_probe_home" "$dw_user_id"
dw_probe_identity=$(path_identity "$dw_probe_home")
dw_outside_probe="$dw_repo_root/.dayweave-codex-containment-probe-$$"
test ! -e "$dw_outside_probe" && test ! -L "$dw_outside_probe" \
  || fail "outside probe path already exists"

dw_tmp_dir="$dw_probe_home/tmp"
dw_runtime_dir="$dw_probe_home/runtime"
dw_generated_dir="$dw_probe_home/generated-schemas"
"$DW_MKDIR" -m 700 "$dw_tmp_dir" "$dw_runtime_dir" "$dw_generated_dir"
require_private_directory "$dw_tmp_dir" "$dw_user_id"
require_private_directory "$dw_runtime_dir" "$dw_user_id"
require_private_directory "$dw_generated_dir" "$dw_user_id"
dw_runtime_copy="$dw_runtime_dir/codex"
"$DW_CP" "$DW_EXPECTED_SOURCE" "$dw_runtime_copy"
"$DW_CHMOD" 500 "$dw_runtime_copy"
test -f "$dw_runtime_copy" && test ! -L "$dw_runtime_copy" \
  || fail "runtime copy is not a regular file"
test "$($DW_STAT -f %u "$dw_runtime_copy")" = "$dw_user_id" \
  || fail "runtime copy has an unexpected owner"
dw_runtime_identity=$(path_identity "$dw_runtime_copy")

require_safe_profile_path "$dw_probe_home"
require_safe_profile_path "$dw_runtime_copy"

dw_profile_common="(version 1)
(deny default)
(deny dynamic-code-generation)
(import \"system.sb\")
(import \"com.apple.corefoundation.sb\")
(allow process-info* (target self))
(allow process-info-codesignature)
(allow file-read-metadata
  (literal \"/\")
  (literal \"/Users\")
  (literal \"$dw_user_home\")
  (literal \"$dw_library\")
  (literal \"$dw_application_support\")
  (literal \"$dw_dayweave_support\")
  (literal \"$dw_verification_root\")
  (subpath \"$dw_probe_home\")
  (subpath \"/System\")
  (subpath \"/usr/lib\")
  (subpath \"/usr/share\")
  (subpath \"/Library/Apple\")
  (subpath \"/private/etc/ssl\")
  (literal \"/private/etc/hosts\")
  (literal \"/private/etc/resolv.conf\")
  (literal \"/private/etc/services\")
  (literal \"/dev/null\")
  (literal \"/dev/random\")
  (literal \"/dev/urandom\")
  (literal \"/dev/zero\"))
(allow file-read*
  (subpath \"/System\")
  (subpath \"/usr/lib\")
  (subpath \"/usr/share\")
  (subpath \"/Library/Apple\")
  (subpath \"/private/etc/ssl\")
  (literal \"/private/etc/hosts\")
  (literal \"/private/etc/resolv.conf\")
  (literal \"/private/etc/services\")
  (literal \"/dev/null\")
  (literal \"/dev/random\")
  (literal \"/dev/urandom\")
  (literal \"/dev/zero\")
  (subpath \"$dw_probe_home\"))
(allow file-write* (subpath \"$dw_probe_home\"))
(deny file-write* (subpath \"$dw_runtime_dir\"))
(allow process-exec (literal \"$dw_runtime_copy\"))
(allow signal (target self))
(allow system-socket)
(allow sysctl-read)"
dw_offline_profile="$dw_profile_common"
dw_app_server_profile="$dw_profile_common
(allow network-outbound)"

verify_runtime_copy() {
  test -f "$dw_runtime_copy" && test ! -L "$dw_runtime_copy" \
    || fail "runtime copy changed type"
  test "$(path_identity "$dw_runtime_copy")" = "$dw_runtime_identity" \
    || fail "runtime copy identity changed"
  test "$(sha256_file "$dw_runtime_copy")" = "$DW_EXPECTED_BINARY_HASH" \
    || fail "runtime copy content pin mismatch"
  "$DW_CODESIGN" --verify --strict -R="$DW_EXPECTED_REQUIREMENT" \
    "$dw_runtime_copy" >/dev/null 2>&1 \
    || fail "runtime copy Developer ID requirement mismatch"
}

dw_version_stdout="$dw_probe_home/version.stdout"
dw_version_stderr="$dw_probe_home/version.stderr"
run_bounded "contained version probe" 8 4096 8192 32 /dev/null \
  "$dw_version_stdout" "$dw_version_stderr" \
  "$DW_ENV" -i \
    CODEX_HOME="$dw_probe_home" HOME="$dw_probe_home" TMPDIR="$dw_tmp_dir" \
    LANG=en_US.UTF-8 \
    "$DW_SANDBOX_EXEC" -p "$dw_offline_profile" \
    "$dw_runtime_copy" --version
test "$("$DW_CAT" "$dw_version_stdout")" = "$DW_EXPECTED_VERSION" \
  || fail "contained CLI version mismatch"
verify_runtime_copy

dw_schema_stdout="$dw_probe_home/schema.stdout"
dw_schema_stderr="$dw_probe_home/schema.stderr"
run_bounded "contained schema generation" 20 65536 65536 2048 /dev/null \
  "$dw_schema_stdout" "$dw_schema_stderr" \
  "$DW_ENV" -i \
    CODEX_HOME="$dw_probe_home" HOME="$dw_probe_home" TMPDIR="$dw_tmp_dir" \
    LANG=en_US.UTF-8 \
    "$DW_SANDBOX_EXEC" -p "$dw_offline_profile" \
    "$dw_runtime_copy" app-server generate-json-schema --out "$dw_generated_dir"
verify_runtime_copy

dw_generated_count=$("$DW_FIND" -P "$dw_generated_dir" -type f | "$DW_WC" -l | "$DW_TR" -d ' ')
test "$dw_generated_count" -ge 2 && test "$dw_generated_count" -le 512 \
  || fail "schema generation produced an invalid file count"
dw_generated_directory_count=$("$DW_FIND" -P "$dw_generated_dir" -type d | "$DW_WC" -l | "$DW_TR" -d ' ')
test "$dw_generated_directory_count" -ge 1 && test "$dw_generated_directory_count" -le 16 \
  || fail "schema generation produced an invalid directory count"
test -z "$("$DW_FIND" -P "$dw_generated_dir" -type l -print -quit)" \
  || fail "schema generation produced a symlink"
test -z "$("$DW_FIND" -P "$dw_generated_dir" ! -type d ! -type f -print -quit)" \
  || fail "schema generation produced a non-file object"
dw_generated_kib=$("$DW_DU" -sk "$dw_generated_dir" | "$DW_AWK" '{print $1}')
test "$dw_generated_kib" -le 16384 || fail "schema generation exceeded its size bound"

for dw_generated_spec in \
  "$dw_generated_dir/$DW_LEGACY_SCHEMA:$dw_legacy_pin:$DW_LEGACY_SCHEMA_HASH" \
  "$dw_generated_dir/$DW_V2_SCHEMA:$dw_v2_pin:$DW_V2_SCHEMA_HASH"
do
  dw_generated_path=${dw_generated_spec%%:*}
  dw_generated_tail=${dw_generated_spec#*:}
  dw_pinned_path=${dw_generated_tail%%:*}
  dw_expected_hash=${dw_generated_tail#*:}
  test -f "$dw_generated_path" && test ! -L "$dw_generated_path" \
    || fail "schema generator omitted a combined bundle"
  test "$(sha256_file "$dw_generated_path")" = "$dw_expected_hash" \
    || fail "generated schema hash mismatch"
  "$DW_CMP" -s "$dw_generated_path" "$dw_pinned_path" \
    || fail "generated schema is not byte-identical to its pin"
done

dw_positive_read="$dw_probe_home/fs-read-positive.txt"
dw_positive_write="$dw_probe_home/fs-write-positive.txt"
printf 'x' >"$dw_positive_read"
"$DW_CHMOD" 600 "$dw_positive_read"
test ! -e "$dw_positive_write" && test ! -L "$dw_positive_write" \
  || fail "positive write probe path already exists"

dw_requests="$dw_probe_home/requests.jsonl"
"$DW_JQ" -cn \
  --arg home "$dw_probe_home" \
  --arg positive_read "$dw_positive_read" \
  --arg positive_write "$dw_positive_write" \
  --arg outside_read "$dw_repo_root/README.md" \
  --arg outside_write "$dw_outside_probe" \
  --arg runtime "$dw_runtime_copy" '
  [
    {method:"initialize", id:1, params:{
      clientInfo:{name:"dayweave-containment-probe", title:"DayWeave containment probe", version:"0.1.0"},
      capabilities:{experimentalApi:false}
    }},
    {method:"initialized"},
    {method:"account/read", id:2, params:{refreshToken:false}},
    {method:"fs/readFile", id:3, params:{path:$positive_read}},
    {method:"fs/writeFile", id:4, params:{path:$positive_write, dataBase64:"eA=="}},
    {method:"fs/readFile", id:5, params:{path:$outside_read}},
    {method:"fs/writeFile", id:6, params:{path:$outside_write, dataBase64:"eA=="}},
    {method:"command/exec", id:7, params:{
      command:["/bin/cat", $outside_read], sandboxPolicy:{type:"dangerFullAccess"},
      timeoutMs:1000, outputBytesCap:128
    }},
    {method:"command/exec", id:8, params:{
      command:[$runtime, "--version"], sandboxPolicy:{type:"dangerFullAccess"},
      timeoutMs:1000, outputBytesCap:128
    }}
  ][]' >"$dw_requests"
"$DW_CHMOD" 600 "$dw_requests"
test "$(file_size "$dw_requests")" -le 32768 || fail "probe request set is too large"

dw_probe_stdout="$dw_probe_home/app-server.stdout"
dw_probe_stderr="$dw_probe_home/app-server.stderr"
run_bounded "App Server containment probe" 15 262144 65536 2048 \
  "$dw_requests" "$dw_probe_stdout" "$dw_probe_stderr" \
  "$DW_ENV" -i \
    CODEX_HOME="$dw_probe_home" HOME="$dw_probe_home" TMPDIR="$dw_tmp_dir" \
    LANG=en_US.UTF-8 \
    "$DW_SANDBOX_EXEC" -p "$dw_app_server_profile" \
    "$dw_runtime_copy" app-server --stdio --strict-config \
      -c 'cli_auth_credentials_store="file"' \
      -c 'check_for_update_on_startup=false' \
      -c 'analytics.enabled=false' \
      -c 'agents.enabled=false' \
      -c 'tools.web_search=false' \
      -c 'approval_policy="never"' \
      -c 'sandbox_mode="read-only"' \
      -c 'allow_login_shell=false' \
      -c 'shell_environment_policy.inherit="none"' \
      -c 'shell_environment_policy.ignore_default_excludes=false'
verify_runtime_copy

"$DW_AWK" '
  length($0) == 0 || length($0) > 65536 { exit 1 }
  END { if (NR < 8 || NR > 64) exit 1 }
' "$dw_probe_stdout" || fail "App Server emitted invalid JSONL bounds"

"$DW_JQ" -e -s --arg home "$dw_probe_home" '
  def response($id): first(.[] | select(.id? == $id));
  def succeeded: has("result") and (has("error") | not);
  def denied_file:
    (has("result") | not)
    and .error.code == -32603
    and .error.message == "Operation not permitted (os error 1)";
  def denied_process:
    (has("result") | not)
    and .error.code == -32603
    and .error.message == "failed to spawn command: Operation not permitted (os error 1)";

  length >= 8 and length <= 64
  and all(.[]; type == "object")
  and ([.[] | select(has("id")) | .id] | sort) == [1,2,3,4,5,6,7,8]
  and (response(1) | succeeded
    and .result.platformOs == "macos"
    and .result.codexHome == $home
    and (.result.userAgent | type == "string"))
  and (response(2) | succeeded
    and (.result.account // null) == null
    and (.result.requiresOpenaiAuth | type == "boolean"))
  and (response(3) | succeeded and .result.dataBase64 == "eA==")
  and (response(4) | succeeded and (.result | type == "object"))
  and (response(5) | denied_file)
  and (response(6) | denied_file)
  and (response(7) | denied_process)
  and (response(8) | denied_process)
' "$dw_probe_stdout" >/dev/null || fail "App Server containment response assertion failed"

test "$("$DW_CAT" "$dw_positive_write")" = x \
  || fail "positive in-home write probe failed"
test ! -e "$dw_outside_probe" && test ! -L "$dw_outside_probe" \
  || fail "outer sandbox allowed a write outside the isolated home"

exit 0
