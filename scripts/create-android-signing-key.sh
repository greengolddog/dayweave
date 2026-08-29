#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/.." && pwd -P)"

for command_name in git keytool openssl python3; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "${command_name} is required." >&2
    exit 1
  fi
done

git_dir="$(git -C "${repo_root}" rev-parse --path-format=absolute --git-dir)"
git_common_dir="$(git -C "${repo_root}" rev-parse --path-format=absolute --git-common-dir)"

config_base="${XDG_CONFIG_HOME:-${HOME:?HOME is required}/.config}"
signing_dir="${DAYWEAVE_ANDROID_SIGNING_DIR:-${config_base}/dayweave/android-signing}"
keystore_file="${signing_dir}/dayweave-release.p12"
properties_file="${signing_dir}/release-signing.properties"
key_alias="dayweave-release"

if [[ "${signing_dir}" != /* || "${signing_dir}" == "/" ]]; then
  echo "The Android signing directory must be a specific absolute path." >&2
  exit 1
fi
if [[ "${signing_dir}" == *$'\n'* || "${signing_dir}" == *$'\r'* || "${signing_dir}" == *$'\t'* ]]; then
  echo "The Android signing directory contains unsupported control characters." >&2
  exit 1
fi

canonical_path() {
  local mode="$1"
  local candidate="$2"
  python3 -c \
    'import os, sys; print(os.path.abspath(sys.argv[2]) if sys.argv[1] == "lexical" else os.path.realpath(sys.argv[2]))' \
    "${mode}" "${candidate}"
}

path_is_within() {
  local candidate="$1"
  local parent="$2"
  [[ "${candidate}" == "${parent}" || "${candidate}" == "${parent}/"* ]]
}

assert_signing_dir_outside_repo() {
  local lexical_signing_dir
  local resolved_signing_dir
  local lexical_repo_root
  local resolved_repo_root
  local lexical_git_dir
  local resolved_git_dir
  local lexical_git_common_dir
  local resolved_git_common_dir
  lexical_signing_dir="$(canonical_path lexical "${signing_dir}")"
  resolved_signing_dir="$(canonical_path resolved "${signing_dir}")"
  lexical_repo_root="$(canonical_path lexical "${repo_root}")"
  resolved_repo_root="$(canonical_path resolved "${repo_root}")"
  lexical_git_dir="$(canonical_path lexical "${git_dir}")"
  resolved_git_dir="$(canonical_path resolved "${git_dir}")"
  lexical_git_common_dir="$(canonical_path lexical "${git_common_dir}")"
  resolved_git_common_dir="$(canonical_path resolved "${git_common_dir}")"

  if path_is_within "${lexical_signing_dir}" "${lexical_repo_root}" || \
    path_is_within "${resolved_signing_dir}" "${resolved_repo_root}" || \
    path_is_within "${lexical_signing_dir}" "${lexical_git_dir}" || \
    path_is_within "${resolved_signing_dir}" "${resolved_git_dir}" || \
    path_is_within "${lexical_signing_dir}" "${lexical_git_common_dir}" || \
    path_is_within "${resolved_signing_dir}" "${resolved_git_common_dir}"; then
    echo "Refusing to create Android signing material inside the Git worktree." >&2
    exit 1
  fi
}

assert_signing_dir_outside_repo
if [[ -L "${signing_dir}" ]]; then
  echo "The Android signing directory must not be a symlink." >&2
  exit 1
fi
if [[ -e "${keystore_file}" || -L "${keystore_file}" || -e "${properties_file}" || -L "${properties_file}" ]]; then
  echo "Refusing to replace existing Android signing material in ${signing_dir}." >&2
  exit 1
fi

install -d -m 0700 "${signing_dir}"
assert_signing_dir_outside_repo
generated_password="$(openssl rand -base64 48 | tr -d '\r\n')"
if [[ ${#generated_password} -lt 48 ]]; then
  echo "Failed to generate a sufficiently strong signing password." >&2
  exit 1
fi
export DAYWEAVE_ANDROID_GENERATED_PASSWORD="${generated_password}"

keytool -genkeypair \
  -alias "${key_alias}" \
  -keyalg RSA \
  -keysize 4096 \
  -sigalg SHA256withRSA \
  -validity 10950 \
  -dname "CN=DayWeave Private Release, O=DayWeave, C=ES" \
  -storetype PKCS12 \
  -keystore "${keystore_file}" \
  -storepass:env DAYWEAVE_ANDROID_GENERATED_PASSWORD \
  -keypass:env DAYWEAVE_ANDROID_GENERATED_PASSWORD \
  -noprompt >/dev/null

{
  printf 'storeFile=%s\n' "${keystore_file}"
  printf 'storePassword=%s\n' "${generated_password}"
  printf 'keyAlias=%s\n' "${key_alias}"
  printf 'keyPassword=%s\n' "${generated_password}"
} >"${properties_file}"
unset DAYWEAVE_ANDROID_GENERATED_PASSWORD generated_password
chmod 0600 "${keystore_file}" "${properties_file}"

echo "Created private Android release signing material in ${signing_dir}."
echo "Back up both files securely; losing this key prevents trusted in-place updates."
echo "Build with: DAYWEAVE_ANDROID_SIGNING_PROPERTIES=${properties_file} scripts/build-android-apk.sh"
