#!/usr/bin/env bash
set -euo pipefail
umask 077

terraform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
deploy_dir="$(cd "${terraform_dir}/../.." && pwd)"
# shellcheck source=../scripts/guard-lib.sh
# shellcheck disable=SC1091
source "${terraform_dir}/scripts/guard-lib.sh"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-guard-tests.XXXXXXXX")"
cleanup() {
  case "$work_dir" in
    "${TMPDIR:-/tmp}"/dayweave-guard-tests.*) rm -rf -- "$work_dir" ;;
    *) echo "Refusing to clean an unexpected test path." >&2 ;;
  esac
}
trap cleanup EXIT

private_file="$work_dir/private"
printf 'safe\n' >"$private_file"
chmod 0600 "$private_file"
assert_private_artifact "$private_file" 16 "test private artifact"

chmod 0644 "$private_file"
if assert_private_artifact "$private_file" 16 "test public artifact" 2>/dev/null; then
  echo "A public artifact unexpectedly passed the private-file guard." >&2
  exit 1
fi
chmod 0600 "$private_file"
ln -s "$private_file" "$work_dir/link"
if assert_private_artifact "$work_dir/link" 16 "test symlink" 2>/dev/null; then
  echo "A symlink unexpectedly passed the private-file guard." >&2
  exit 1
fi

private_directory="$work_dir/private-directory"
mkdir -m 0700 "$private_directory"
assert_private_directory "$private_directory" "test private directory"
chmod 0755 "$private_directory"
if assert_private_directory "$private_directory" "test public directory" 2>/dev/null; then
  echo "A mode-0755 directory unexpectedly passed the private-directory guard." >&2
  exit 1
fi

bounded_file="$work_dir/bounded"
bounded_run_to_file 1024 "$bounded_file" printf 'bounded\n'
oversize_file="$work_dir/oversize"
if bounded_run_to_file 1024 "$oversize_file" sh -c 'dd if=/dev/zero bs=2048 count=1 2>/dev/null' 2>/dev/null; then
  echo "An oversized command body unexpectedly passed the bound." >&2
  exit 1
fi

env_file="$work_dir/dayweave.env"
printf 'POSTGRES_PASSWORD=test-only\n' >"$env_file"
chmod 0644 "$env_file"
if "${deploy_dir}/scripts/verify-production-env.sh" "$env_file" 2>/dev/null; then
  echo "A mode-0644 production environment unexpectedly passed." >&2
  exit 1
fi
chmod 0600 "$env_file"
if [[ "$(id -u)" == "0" ]]; then
  "${deploy_dir}/scripts/verify-production-env.sh" "$env_file"
elif "${deploy_dir}/scripts/verify-production-env.sh" "$env_file" 2>/dev/null; then
  echo "A non-root-owned production environment unexpectedly passed." >&2
  exit 1
fi

if DAYWEAVE_NEBIUS_APPROVAL=wrong \
  "${terraform_dir}/scripts/approve-nebius.sh" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  >/dev/null 2>&1; then
  echo "Approval unexpectedly accepted the wrong explicit phrase." >&2
  exit 1
fi
if DAYWEAVE_NEBIUS_APPLY=wrong "${terraform_dir}/scripts/apply-nebius.sh" >/dev/null 2>&1; then
  echo "Apply unexpectedly accepted the wrong charge phrase." >&2
  exit 1
fi

if rg -n 'tunnel list|--all --format json' "${terraform_dir}/cloud-init.yaml.tftpl" >/dev/null; then
  echo "Cloud-init unexpectedly grants itself tunnel discovery behavior." >&2
  exit 1
fi
rg -F 'tunnel_id=${jsonencode(tunnel_id)}' "${terraform_dir}/cloud-init.yaml.tftpl" >/dev/null
rg -F 'Requires=dayweave-host-security.service' "${terraform_dir}/cloud-init.yaml.tftpl" >/dev/null
rg -F 'ExecStartPre=/usr/local/sbin/assert-dayweave-ssh-loopback' "${terraform_dir}/cloud-init.yaml.tftpl" >/dev/null
rg -F 'ExecStartPre=/opt/dayweave/deploy/scripts/verify-production-env.sh' \
  "${deploy_dir}/systemd/dayweave.service" >/dev/null

echo "Nebius deployment guard tests passed without running plan or apply."
