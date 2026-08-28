#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
package_dir="${repo_root}/apps/macos"
output_root="${repo_root}/dist/macos"
app_dir="${output_root}/DayWeave.app"

swift build --package-path "${package_dir}" --configuration release

binary_path="$(swift build --package-path "${package_dir}" --configuration release --show-bin-path)/DayWeave"
if [[ ! -x "${binary_path}" ]]; then
    echo "DayWeave release binary was not produced at ${binary_path}" >&2
    exit 1
fi

mkdir -p "${app_dir}/Contents/MacOS" "${app_dir}/Contents/Resources"
cp "${binary_path}" "${app_dir}/Contents/MacOS/DayWeave"
cp "${package_dir}/Resources/Info.plist" "${app_dir}/Contents/Info.plist"

# Direct local distribution uses a reproducible ad-hoc signature until the owner
# chooses to enroll in the Apple Developer Program.
codesign --force --deep --sign - "${app_dir}"
codesign --verify --deep --strict --verbose=2 "${app_dir}"

echo "Built ${app_dir}"

