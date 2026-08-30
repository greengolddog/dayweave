#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/.." && pwd -P)"
android_dir="${repo_root}/apps/android"
output_dir="${repo_root}/dist/android"
output_apk="${output_dir}/DayWeave-release.apk"
config_base="${XDG_CONFIG_HOME:-${HOME:?HOME is required}/.config}"
default_properties="${config_base}/dayweave/android-signing/release-signing.properties"
signing_properties="${DAYWEAVE_ANDROID_SIGNING_PROPERTIES:-${default_properties}}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required." >&2
  exit 1
fi
if [[ "${signing_properties}" != /* ]]; then
  signing_properties="${PWD}/${signing_properties}"
fi
python3 "${script_dir}/check-android-signing-boundary.py" \
  --repo-root "${repo_root}" \
  --properties "${signing_properties}" \
  --keystore-base "${android_dir}/app"

android_sdk_root="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [[ -z "${android_sdk_root}" || ! -d "${android_sdk_root}/build-tools" ]]; then
  echo "ANDROID_HOME or ANDROID_SDK_ROOT must point to an Android SDK." >&2
  exit 1
fi
apksigner="$(find "${android_sdk_root}/build-tools" -type f -name apksigner -print | sort -V | tail -n 1)"
if [[ -z "${apksigner}" || ! -x "${apksigner}" ]]; then
  echo "Android SDK apksigner is required." >&2
  exit 1
fi

export DAYWEAVE_ANDROID_SIGNING_PROPERTIES="${signing_properties}"
"${android_dir}/gradlew" --project-dir "${android_dir}" \
  --no-daemon --no-build-cache --no-configuration-cache clean assembleRelease
unset DAYWEAVE_ANDROID_SIGNING_PROPERTIES

built_apk="${android_dir}/app/build/outputs/apk/release/app-release.apk"
if [[ ! -f "${built_apk}" ]]; then
  echo "A signed release APK was not produced at ${built_apk}." >&2
  exit 1
fi
verification_output="$("${apksigner}" verify --verbose "${built_apk}")"
printf '%s\n' "${verification_output}"
assert_verification_line() {
  local expected="$1"
  if [[ "$(grep -Fxc -- "${expected}" <<<"${verification_output}")" != "1" ]]; then
    echo "APK signature verification did not report exactly: ${expected}" >&2
    exit 1
  fi
}
assert_verification_line "Verified using v1 scheme (JAR signing): false"
assert_verification_line "Verified using v2 scheme (APK Signature Scheme v2): false"
assert_verification_line "Verified using v3 scheme (APK Signature Scheme v3): true"
assert_verification_line "Number of signers: 1"
install -d -m 0755 "${output_dir}"
install -m 0644 "${built_apk}" "${output_apk}"
shasum -a 256 "${output_apk}"
echo "Built and verified ${output_apk}"
