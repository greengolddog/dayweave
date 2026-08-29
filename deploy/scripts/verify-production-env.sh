#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "Usage: $0 <production-env-file>" >&2
  exit 64
fi

env_file="$1"
if [[ -L "$env_file" || ! -f "$env_file" ]]; then
  echo "The production environment must be a regular, non-symlink file: $env_file" >&2
  exit 1
fi

if stat -c '%u %g %a %h' -- "$env_file" >/dev/null 2>&1; then
  read -r owner_id group_id mode link_count < <(stat -c '%u %g %a %h' -- "$env_file")
else
  read -r owner_id group_id mode link_count < <(stat -f '%u %g %Lp %l' -- "$env_file")
fi
if [[ "$owner_id" != "0" || "$group_id" != "0" || "$mode" != "600" || "$link_count" != "1" ]]; then
  echo "The production environment must be root:root, mode 0600, with one hard link: $env_file" >&2
  exit 1
fi

if [[ ! -s "$env_file" ]]; then
  echo "The production environment is empty: $env_file" >&2
  exit 1
fi
