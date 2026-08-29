#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 0 ]]; then
  echo "This installer accepts no arguments." >&2
  exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/.." && pwd -P)"
discovered_root="$(git -C "${repo_root}" rev-parse --show-toplevel)"
discovered_root="$(cd "${discovered_root}" && pwd -P)"
if [[ "${discovered_root}" != "${repo_root}" ]]; then
  echo "Hook installer is not running from its owning worktree." >&2
  exit 1
fi

for hook_name in pre-commit pre-push; do
  hook_path="${repo_root}/.githooks/${hook_name}"
  if [[ ! -f "${hook_path}" || -L "${hook_path}" || ! -x "${hook_path}" ]]; then
    echo "Checked-in hooks must be regular executable files." >&2
    exit 1
  fi
done

configured_path="$(git -C "${repo_root}" config --local --get core.hooksPath || true)"
if [[ -n "${configured_path}" && "${configured_path}" != ".githooks" ]]; then
  echo "Refusing to replace an existing custom core.hooksPath." >&2
  exit 1
fi
if [[ "${configured_path}" == ".githooks" ]]; then
  echo "DayWeave Git hooks are already configured."
  exit 0
fi

git -C "${repo_root}" config --local core.hooksPath .githooks
if [[ "$(git -C "${repo_root}" config --local --get core.hooksPath)" != ".githooks" ]]; then
  echo "Git did not retain the checked-in hook path." >&2
  exit 1
fi
echo "Configured this worktree to use the checked-in DayWeave Git hooks."
