#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/.." && pwd -P)"
temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-build-signing-guard.XXXXXXXX")"

cleanup() {
  if [[ "${temporary_dir}" == "${TMPDIR:-/tmp}/dayweave-build-signing-guard."* ]]; then
    rm -rf -- "${temporary_dir}"
  fi
}
trap cleanup EXIT

main_repo="${temporary_dir}/main"
linked_worktree="${temporary_dir}/linked"
outside_dir="${temporary_dir}/outside"
invocation_marker="${temporary_dir}/gradle-or-sign-was-invoked"
install -d -m 0700 "${main_repo}" "${outside_dir}"
git -C "${main_repo}" init -q
git -C "${main_repo}" config user.email dayweave-boundary-test@example.invalid
git -C "${main_repo}" config user.name "DayWeave boundary test"
printf '%s\n' synthetic >"${main_repo}/seed"
git -C "${main_repo}" add seed
git -C "${main_repo}" commit -q -m seed
git -C "${main_repo}" worktree add -q --detach "${linked_worktree}" HEAD

install -d -m 0700 \
  "${linked_worktree}/scripts" \
  "${linked_worktree}/apps/android/app"
install -m 0700 \
  "${repo_root}/scripts/build-android-apk.sh" \
  "${linked_worktree}/scripts/build-android-apk.sh"
install -m 0600 \
  "${repo_root}/scripts/check-android-signing-boundary.py" \
  "${linked_worktree}/scripts/check-android-signing-boundary.py"
{
  printf '%s\n' '#!/usr/bin/env bash'
  printf '%s\n' 'set -euo pipefail'
  printf '%s\n' ': >"${DAYWEAVE_TEST_INVOCATION_MARKER:?}"'
  printf '%s\n' 'exit 97'
} >"${linked_worktree}/apps/android/gradlew"
chmod 0700 "${linked_worktree}/apps/android/gradlew"

git_dir="$(git -C "${linked_worktree}" rev-parse --path-format=absolute --git-dir)"
git_common_dir="$(git -C "${linked_worktree}" rev-parse --path-format=absolute --git-common-dir)"
safe_keystore="${outside_dir}/safe-release.p12"
printf '%s\n' synthetic-keystore >"${safe_keystore}"
chmod 0600 "${safe_keystore}"

write_properties() {
  local destination="$1"
  local store_file="$2"
  install -d -m 0700 "$(dirname "${destination}")"
  {
    printf 'storeFile=%s\n' "${store_file}"
    printf '%s\n' 'storePassword=synthetic-password-never-log'
    printf '%s\n' 'keyAlias=synthetic-alias-never-log'
    printf '%s\n' 'keyPassword=synthetic-password-never-log'
  } >"${destination}"
  chmod 0600 "${destination}"
}

assert_rejected() {
  local label="$1"
  local properties_file="$2"
  local expected_message="$3"
  local output_log="${temporary_dir}/${label}.log"
  local status
  rm -f -- "${invocation_marker}"
  set +e
  DAYWEAVE_ANDROID_SIGNING_PROPERTIES="${properties_file}" \
    DAYWEAVE_TEST_INVOCATION_MARKER="${invocation_marker}" \
    ANDROID_HOME="${temporary_dir}/missing-sdk" \
    "${linked_worktree}/scripts/build-android-apk.sh" >"${output_log}" 2>&1
  status=$?
  set -e

  if [[ ${status} -eq 0 ]]; then
    echo "An unsafe Android build signing path was not refused: ${label}." >&2
    exit 1
  fi
  if [[ -e "${invocation_marker}" ]]; then
    echo "Gradle or a signing tool ran before an unsafe path was refused: ${label}." >&2
    exit 1
  fi
  if grep -Fq -- 'synthetic-password-never-log' "${output_log}" || \
    grep -Fq -- 'synthetic-alias-never-log' "${output_log}"; then
    echo "The Android build signing guard exposed a synthetic secret: ${label}." >&2
    exit 1
  fi
  if ! grep -Fqx -- "${expected_message}" "${output_log}"; then
    echo "The Android build signing guard returned the wrong failure: ${label}." >&2
    sed -n '1,4p' "${output_log}" >&2
    exit 1
  fi
}

assert_safe_pair_reaches_sdk_gate() {
  local properties_file="$1"
  local output_log="${temporary_dir}/safe-pair.log"
  local status
  rm -f -- "${invocation_marker}"
  set +e
  DAYWEAVE_ANDROID_SIGNING_PROPERTIES="${properties_file}" \
    DAYWEAVE_TEST_INVOCATION_MARKER="${invocation_marker}" \
    ANDROID_HOME="${temporary_dir}/missing-sdk" \
    "${linked_worktree}/scripts/build-android-apk.sh" >"${output_log}" 2>&1
  status=$?
  set -e

  if [[ ${status} -eq 0 ]] || \
    ! grep -Fqx -- 'ANDROID_HOME or ANDROID_SDK_ROOT must point to an Android SDK.' "${output_log}"; then
    echo "A safe external signing pair did not reach the Android SDK gate." >&2
    exit 1
  fi
  if [[ -e "${invocation_marker}" ]]; then
    echo "The signing boundary regression invoked Gradle or a signing tool." >&2
    exit 1
  fi
}

boundaries=(
  "worktree:${linked_worktree}"
  "git-dir:${git_dir}"
  "git-common-dir:${git_common_dir}"
)

for boundary_entry in "${boundaries[@]}"; do
  label="${boundary_entry%%:*}"
  boundary="${boundary_entry#*:}"
  properties_name="dayweave-${label}-properties"
  properties_inside="${boundary}/${properties_name}"
  write_properties "${properties_inside}" "${safe_keystore}"
  assert_rejected \
    "properties-lexical-${label}" \
    "${properties_inside}" \
    'Signing properties must be outside the Git worktree and metadata.'

  boundary_link="${outside_dir}/${label}-boundary-link"
  ln -s "${boundary}" "${boundary_link}"
  assert_rejected \
    "properties-resolved-${label}" \
    "${boundary_link}/${properties_name}" \
    'Signing properties must be outside the Git worktree and metadata.'

  keystore_name="dayweave-${label}-keystore.p12"
  keystore_inside="${boundary}/${keystore_name}"
  printf '%s\n' synthetic-keystore >"${keystore_inside}"
  chmod 0600 "${keystore_inside}"
  direct_properties="${outside_dir}/keystore-lexical-${label}.properties"
  write_properties "${direct_properties}" "${keystore_inside}"
  assert_rejected \
    "keystore-lexical-${label}" \
    "${direct_properties}" \
    'Android release keystore must be outside the Git worktree and metadata.'

  resolved_properties="${outside_dir}/keystore-resolved-${label}.properties"
  write_properties "${resolved_properties}" "${boundary_link}/${keystore_name}"
  assert_rejected \
    "keystore-resolved-${label}" \
    "${resolved_properties}" \
    'Android release keystore must be outside the Git worktree and metadata.'
done

outside_escape="${linked_worktree}/synthetic-external-link"
ln -s "${outside_dir}" "${outside_escape}"
outside_properties="${outside_dir}/lexically-external.properties"
write_properties "${outside_properties}" "${safe_keystore}"
assert_rejected \
  properties-lexical-symlink-escape \
  "${outside_escape}/lexically-external.properties" \
  'Signing properties must be outside the Git worktree and metadata.'

escaped_keystore_properties="${outside_dir}/lexically-external-keystore.properties"
write_properties "${escaped_keystore_properties}" "${outside_escape}/safe-release.p12"
assert_rejected \
  keystore-lexical-symlink-escape \
  "${escaped_keystore_properties}" \
  'Android release keystore must be outside the Git worktree and metadata.'

unsafe_keystore="${linked_worktree}/unicode-escaped-keystore.p12"
printf '%s\n' synthetic-keystore >"${unsafe_keystore}"
chmod 0600 "${unsafe_keystore}"
encoded_unsafe_keystore="$(python3 -c 'import sys; print(sys.argv[1].replace("/", r"\u002f"))' "${unsafe_keystore}")"
unicode_properties="${outside_dir}/unicode-store-file.properties"
write_properties "${unicode_properties}" "${encoded_unsafe_keystore}"
assert_rejected \
  keystore-unicode-escape \
  "${unicode_properties}" \
  'Android release keystore must be outside the Git worktree and metadata.'

duplicate_properties="${outside_dir}/duplicate-store-file.properties"
write_properties "${duplicate_properties}" "${safe_keystore}"
printf 'storeFile=%s\n' "${unsafe_keystore}" >>"${duplicate_properties}"
assert_rejected \
  keystore-last-property-wins \
  "${duplicate_properties}" \
  'Android release keystore must be outside the Git worktree and metadata.'

properties_target="${outside_dir}/properties-target"
write_properties "${properties_target}" "${safe_keystore}"
properties_symlink="${outside_dir}/properties-symlink"
ln -s "${properties_target}" "${properties_symlink}"
assert_rejected \
  properties-final-symlink \
  "${properties_symlink}" \
  'Signing properties must be a regular, non-symlink file.'

public_properties="${outside_dir}/public-properties"
write_properties "${public_properties}" "${safe_keystore}"
chmod 0644 "${public_properties}"
assert_rejected \
  properties-public-mode \
  "${public_properties}" \
  'Signing properties must have mode 0600.'

hard_linked_properties="${outside_dir}/hard-linked-properties"
write_properties "${hard_linked_properties}" "${safe_keystore}"
ln "${hard_linked_properties}" "${linked_worktree}/hard-linked-properties"
assert_rejected \
  properties-hard-link-alias \
  "${hard_linked_properties}" \
  'Signing properties must not have hard-linked aliases.'

keystore_target="${outside_dir}/keystore-target.p12"
printf '%s\n' synthetic-keystore >"${keystore_target}"
chmod 0600 "${keystore_target}"
keystore_symlink="${outside_dir}/keystore-symlink.p12"
ln -s "${keystore_target}" "${keystore_symlink}"
keystore_symlink_properties="${outside_dir}/keystore-symlink.properties"
write_properties "${keystore_symlink_properties}" "${keystore_symlink}"
assert_rejected \
  keystore-final-symlink \
  "${keystore_symlink_properties}" \
  'Android release keystore must be a regular, non-symlink file.'

public_keystore="${outside_dir}/public-keystore.p12"
printf '%s\n' synthetic-keystore >"${public_keystore}"
chmod 0644 "${public_keystore}"
public_keystore_properties="${outside_dir}/public-keystore.properties"
write_properties "${public_keystore_properties}" "${public_keystore}"
assert_rejected \
  keystore-public-mode \
  "${public_keystore_properties}" \
  'Android release keystore must have mode 0600.'

hard_linked_keystore="${outside_dir}/hard-linked-keystore.p12"
printf '%s\n' synthetic-keystore >"${hard_linked_keystore}"
chmod 0600 "${hard_linked_keystore}"
ln "${hard_linked_keystore}" "${linked_worktree}/hard-linked-keystore.p12"
hard_linked_keystore_properties="${outside_dir}/hard-linked-keystore.properties"
write_properties "${hard_linked_keystore_properties}" "${hard_linked_keystore}"
assert_rejected \
  keystore-hard-link-alias \
  "${hard_linked_keystore_properties}" \
  'Android release keystore must not have hard-linked aliases.'

safe_properties="${outside_dir}/safe-release-signing.properties"
write_properties "${safe_properties}" "${safe_keystore}"
assert_safe_pair_reaches_sdk_gate "${safe_properties}"

echo "Android build signing boundary regression: PASS"
