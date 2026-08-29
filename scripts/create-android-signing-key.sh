#!/usr/bin/env bash
set -euo pipefail
umask 077

for command_name in keytool openssl; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "${command_name} is required." >&2
    exit 1
  fi
done

config_base="${XDG_CONFIG_HOME:-${HOME:?HOME is required}/.config}"
signing_dir="${DAYWEAVE_ANDROID_SIGNING_DIR:-${config_base}/dayweave/android-signing}"
keystore_file="${signing_dir}/dayweave-release.p12"
properties_file="${signing_dir}/release-signing.properties"
key_alias="dayweave-release"

if [[ "${signing_dir}" != /* || "${signing_dir}" == "/" ]]; then
  echo "The Android signing directory must be a specific absolute path." >&2
  exit 1
fi
if [[ -e "${keystore_file}" || -e "${properties_file}" ]]; then
  echo "Refusing to replace existing Android signing material in ${signing_dir}." >&2
  exit 1
fi

install -d -m 0700 "${signing_dir}"
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
