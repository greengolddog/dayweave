#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/.." && pwd -P)"
temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-signing-guard.XXXXXXXX")"
inside_link=""

cleanup() {
  if [[ -n "${inside_link}" && -L "${inside_link}" ]]; then
    unlink -- "${inside_link}"
  fi
  if [[ "${temporary_dir}" == "${TMPDIR:-/tmp}/dayweave-signing-guard."* ]]; then
    rm -rf -- "${temporary_dir}"
  fi
}
trap cleanup EXIT

stub_dir="${temporary_dir}/bin"
invocation_marker="${temporary_dir}/private-tool-was-invoked"
install -d -m 0700 "${stub_dir}"
for command_name in keytool openssl; do
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' 'set -euo pipefail'
    printf '%s\n' ': >"${DAYWEAVE_TEST_PRIVATE_TOOL_MARKER:?}"'
    printf '%s\n' 'exit 97'
  } >"${stub_dir}/${command_name}"
  chmod 0700 "${stub_dir}/${command_name}"
done

assert_rejected_without_private_tool() {
  local label="$1"
  local candidate="$2"
  local must_remain_absent="$3"
  local output_log="${temporary_dir}/${label}.log"
  local status
  set +e
  PATH="${stub_dir}:${PATH}" \
    DAYWEAVE_TEST_PRIVATE_TOOL_MARKER="${invocation_marker}" \
    DAYWEAVE_ANDROID_SIGNING_DIR="${candidate}" \
    "${repo_root}/scripts/create-android-signing-key.sh" >"${output_log}" 2>&1
  status=$?
  set -e

  if [[ ${status} -eq 0 ]]; then
    echo "An unsafe Android signing location was not refused." >&2
    exit 1
  fi
  if [[ "${must_remain_absent}" == "yes" && ( -e "${candidate}" || -L "${candidate}" ) ]]; then
    echo "A rejected Android signing directory was created." >&2
    exit 1
  fi
  if [[ -e "${invocation_marker}" ]]; then
    echo "A private key tool ran before an unsafe location was rejected." >&2
    exit 1
  fi
}

lexical_candidate="${repo_root}/apps/../.dayweave-signing-guard-test"
assert_rejected_without_private_tool lexical "${lexical_candidate}" yes

ln -s "${repo_root}" "${temporary_dir}/repo-link"
resolved_candidate="${temporary_dir}/repo-link/.dayweave-signing-guard-test"
assert_rejected_without_private_tool resolved "${resolved_candidate}" yes

install -d -m 0755 "${repo_root}/dist"
inside_link="${repo_root}/dist/.dayweave-signing-guard-outside-link"
if [[ -e "${inside_link}" || -L "${inside_link}" ]]; then
  echo "The signing containment test link already exists." >&2
  exit 1
fi
ln -s "${temporary_dir}" "${inside_link}"
lexical_inside_candidate="${inside_link}/outside-signing"
assert_rejected_without_private_tool lexical-inside "${lexical_inside_candidate}" yes

git_candidate="${repo_root}/.git/.dayweave-signing-guard-test"
assert_rejected_without_private_tool git-metadata "${git_candidate}" yes

occupied_candidate="${temporary_dir}/occupied"
install -d -m 0700 "${occupied_candidate}"
ln -s "${temporary_dir}/missing-keystore" "${occupied_candidate}/dayweave-release.p12"
assert_rejected_without_private_tool dangling-output "${occupied_candidate}" no

if [[ -e "${lexical_candidate}" || -L "${lexical_candidate}" ]]; then
  echo "Rejected Android signing material remains in the repository." >&2
  exit 1
fi

echo "Android signing containment regression: PASS"
"${repo_root}/scripts/test-build-android-signing-boundary.sh"
