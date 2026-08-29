#!/usr/bin/env bash
set -euo pipefail
umask 077

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
android_dir="${repo_root}/apps/android"
output_dir="${repo_root}/dist/android"
output_apk="${output_dir}/DayWeave-release.apk"
config_base="${XDG_CONFIG_HOME:-${HOME:?HOME is required}/.config}"
default_properties="${config_base}/dayweave/android-signing/release-signing.properties"
signing_properties="${DAYWEAVE_ANDROID_SIGNING_PROPERTIES:-${default_properties}}"

if [[ ! -f "${signing_properties}" || -L "${signing_properties}" ]]; then
  echo "Signing properties must be a regular, non-symlink file: ${signing_properties}" >&2
  echo "Run scripts/create-android-signing-key.sh once if no private key exists." >&2
  exit 1
fi
if [[ "$(stat -f '%Lp' "${signing_properties}")" != "600" ]]; then
  echo "Signing properties must have mode 0600." >&2
  exit 1
fi

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
"${apksigner}" verify --verbose "${built_apk}"
install -d -m 0755 "${output_dir}"
install -m 0644 "${built_apk}" "${output_apk}"
shasum -a 256 "${output_apk}"
echo "Built and verified ${output_apk}"
