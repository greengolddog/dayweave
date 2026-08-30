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

profile_64="$(printf 'p%.0s' {1..64})"
profile_65="$(printf 'p%.0s' {1..65})"
for valid_profile in owner-profile Team.Profile_2 a "$profile_64"; do
  require_nebius_profile "$valid_profile"
done
for invalid_profile in \
  "" \
  -leading \
  .leading \
  _leading \
  "contains space" \
  contains/slash \
  'contains:semicolon' \
  $'contains\nnewline' \
  "$profile_65"; do
  if require_nebius_profile "$invalid_profile" 2>/dev/null; then
    echo "An invalid Nebius profile name unexpectedly passed validation." >&2
    exit 1
  fi
done

profile_plan="$work_dir/profile-plan.json"
profile_context="$work_dir/profile-context.json"
jq -n \
  --arg profile "owner-profile" \
  --arg project "project-test" \
  --arg tenant "tenant-test" \
  --arg subnet "vpcsubnet-test" \
  --arg ssh_key "ssh-ed25519 test" '
    {
      variables: {
        nebius_profile: {value: $profile},
        project_id: {value: $project},
        tenant_id: {value: $tenant},
        subnet_id: {value: $subnet},
        ssh_public_key: {value: $ssh_key}
      }
    }
  ' >"$profile_plan"
jq -n \
  --arg profile "owner-profile" \
  --arg project "project-test" \
  --arg tenant "tenant-test" \
  --arg subnet "vpcsubnet-test" \
  --arg ssh_key "ssh-ed25519 test" '
    {
      nebius_profile: $profile,
      project_id: $project,
      tenant_id: $tenant,
      subnet_id: $subnet,
      ssh_public_key: $ssh_key
    }
  ' >"$profile_context"
assert_nebius_profile_matches_plan "owner-profile" "$profile_plan"
assert_context_matches_plan "$profile_context" "$profile_plan"
if assert_nebius_profile_matches_plan "different-profile" "$profile_plan" 2>/dev/null; then
  echo "An apply profile different from the approved plan unexpectedly passed." >&2
  exit 1
fi
if jq '.variables.nebius_profile.value = "owner-profile\n"' "$profile_plan" |
  nebius_profile_from_plan /dev/stdin >/dev/null 2>&1; then
  echo "A plan profile with a trailing newline unexpectedly passed." >&2
  exit 1
fi
if jq '.nebius_profile = "different-profile"' "$profile_context" |
  assert_context_matches_plan /dev/stdin "$profile_plan" 2>/dev/null; then
  echo "A discovered profile different from the plan unexpectedly passed." >&2
  exit 1
fi
if (
  unset DAYWEAVE_NEBIUS_PROFILE
  "${terraform_dir}/scripts/discover-context.sh" >/dev/null 2>&1
); then
  echo "Discovery unexpectedly accepted a missing explicit profile." >&2
  exit 1
fi
if (
  unset DAYWEAVE_NEBIUS_PROFILE
  "${terraform_dir}/scripts/plan-nebius.sh" >/dev/null 2>&1
); then
  echo "Planning unexpectedly accepted a missing explicit profile." >&2
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
