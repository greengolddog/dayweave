#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
package_dir="${repo_root}/apps/macos"
output_root="${repo_root}/dist/macos"
app_dir="${output_root}/DayWeave.app"
runtime_version="0.150.1"
runtime_source="/opt/homebrew/Caskroom/codex/${runtime_version}/bin/codex"
runtime_contract="${repo_root}/vendor/codex-app-server/${runtime_version}"

swift build --package-path "${package_dir}" --configuration release -Xswiftc -warnings-as-errors

binary_path="$(swift build --package-path "${package_dir}" --configuration release --show-bin-path)/DayWeave"
if [[ ! -x "${binary_path}" ]]; then
    echo "DayWeave release binary was not produced at ${binary_path}" >&2
    exit 1
fi

"${repo_root}/scripts/verify-codex-runtime.sh"

mkdir -p "${output_root}"
build_root="$(mktemp -d "${output_root}/.DayWeave-build.XXXXXX")"
staged_app="${build_root}/DayWeave.app"
cleanup() {
    rm -rf -- "${build_root}"
}
trap cleanup EXIT HUP INT TERM

mkdir -p \
    "${staged_app}/Contents/MacOS" \
    "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}"
cp "${binary_path}" "${staged_app}/Contents/MacOS/DayWeave"
cp "${package_dir}/Resources/Info.plist" "${staged_app}/Contents/Info.plist"
cp "${runtime_source}" \
    "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex"
cp "${runtime_contract}/manifest.json" \
    "${runtime_contract}/codex_app_server_protocol.schemas.json" \
    "${runtime_contract}/codex_app_server_protocol.v2.schemas.json" \
    "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/"
chmod 500 "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex"
codesign --verify --strict \
    -R='identifier "codex" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "2DC432GLL2"' \
    "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex"

# Direct local distribution uses a reproducible ad-hoc signature until the owner
# chooses to enroll in the Apple Developer Program. Do not use --deep while
# signing: the embedded Codex runtime must retain its pinned Developer ID.
codesign --force --sign - "${staged_app}"
codesign --verify --deep --strict --verbose=2 "${staged_app}"
codesign --verify --strict \
    -R='identifier "codex" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "2DC432GLL2"' \
    "${staged_app}/Contents/Resources/CodexRuntime/${runtime_version}/codex"

previous_app="${build_root}/Previous.app"
if [[ -e "${app_dir}" || -L "${app_dir}" ]]; then
    mv "${app_dir}" "${previous_app}"
fi
if ! mv "${staged_app}" "${app_dir}"; then
    if [[ -e "${previous_app}" || -L "${previous_app}" ]]; then
        mv "${previous_app}" "${app_dir}"
    fi
    exit 1
fi
if [[ -e "${previous_app}" || -L "${previous_app}" ]]; then
    rm -rf -- "${previous_app}"
fi

echo "Built ${app_dir}"
