#!/usr/bin/env bash
set -euo pipefail
umask 077

terraform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=guard-lib.sh
# shellcheck disable=SC1091
source "${terraform_dir}/scripts/guard-lib.sh"

profile="lol"
ssh_key_file="${DAYWEAVE_SSH_PUBLIC_KEY_FILE:-${HOME}/.ssh/id_ed25519.pub}"
context_file="${terraform_dir}/local.auto.tfvars.json"
work_dir=""

cleanup() {
  if [[ -n "$work_dir" ]]; then
    case "$work_dir" in
      "${TMPDIR:-/tmp}"/dayweave-nebius-discovery.*) rm -rf -- "$work_dir" ;;
      *) echo "Refusing to clean an unexpected discovery path." >&2 ;;
    esac
  fi
}
trap cleanup EXIT

for command_name in nebius jq ssh-keygen openssl; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "$command_name is required." >&2
    exit 1
  fi
done
assert_regular_bounded_file "$ssh_key_file" 16384 "SSH public key"
if [[ "$(awk 'END { print NR }' "$ssh_key_file")" -ne 1 ]]; then
  echo "The SSH public key must contain exactly one line." >&2
  exit 1
fi
ssh_key_type="$(awk 'NR == 1 { print $1 }' "$ssh_key_file")"
case "$ssh_key_type" in
  ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp256) ;;
  *)
    echo "The SSH public key type is not allowed." >&2
    exit 1
    ;;
esac
if ! ssh-keygen -l -f "$ssh_key_file" >/dev/null 2>&1; then
  echo "The SSH public key is malformed." >&2
  exit 1
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-nebius-discovery.XXXXXXXX")"
chmod 0700 "$work_dir"

parent_file="$work_dir/parent-id"
bounded_run_to_file "$DAYWEAVE_MAX_CONTEXT_BYTES" "$parent_file" \
  nebius --profile "$profile" --no-browser --no-check-update config get parent-id
project_id="$(awk 'NR == 1 { print; exit }' "$parent_file")"
if [[ "$(awk 'END { print NR }' "$parent_file")" -ne 1 || ! "$project_id" =~ ^project-[a-z0-9]+$ ]]; then
  echo "The lol profile did not return exactly one valid project identifier." >&2
  exit 1
fi

project_file="$work_dir/project.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$project_file" \
  nebius --profile "$profile" --no-browser --no-check-update \
    iam project get "$project_id" --format json
if ! jq -e --arg project_id "$project_id" '
  .metadata.id == $project_id and
  (.metadata.parent_id | type == "string" and startswith("tenant-")) and
  .spec.region == "eu-north1" and
  .status.region == "eu-north1" and
  .status.container_state == "ACTIVE" and
  .status.suspension_state == "NONE"
' "$project_file" >/dev/null; then
  echo "The selected profile parent is not the exact active eu-north1 project." >&2
  exit 1
fi
tenant_id="$(jq -er '.metadata.parent_id' "$project_file")"

subnets_file="$work_dir/subnets.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$subnets_file" \
  nebius --profile "$profile" --no-browser --no-check-update vpc subnet list \
    --parent-id "$project_id" --all --format json
subnet_id="$(jq -r --arg project_id "$project_id" '
  if (.items | type) != "array" or
      (all(.items[]; .metadata.parent_id == $project_id) | not)
  then empty
  else
    [.items[] | select(.metadata.name == "default") | .metadata.id] as $defaults
    | if ($defaults | length) == 1 then $defaults[0]
      elif (.items | length) == 1 then .items[0].metadata.id
      else empty
      end
  end
' "$subnets_file")"
if [[ -z "$subnet_id" || ! "$subnet_id" =~ ^vpcsubnet-[a-z0-9]+$ ]]; then
  echo "Could not choose one project subnet safely. Ensure only one exists or name exactly one 'default'." >&2
  exit 1
fi

subnet_file="$work_dir/subnet.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$subnet_file" \
  nebius --profile "$profile" --no-browser --no-check-update \
    vpc subnet get "$subnet_id" --format json
if ! jq -e --arg subnet_id "$subnet_id" --arg project_id "$project_id" '
  .metadata.id == $subnet_id and
  .metadata.parent_id == $project_id and
  .status.state == "READY" and
  ((.spec.network_id | type) == "string") and
  ((.spec.network_id | length) > 0) and
  ((.status.ipv4_private_cidrs | type) == "array") and
  ((.status.ipv4_private_cidrs | length) > 0) and
  .status.route_table.default == true and
  ((.status.route_table.id | type) == "string") and
  ((.status.route_table.id | length) > 0)
' "$subnet_file" >/dev/null; then
  echo "The selected subnet is not an exact ready private subnet in the target project." >&2
  exit 1
fi
network_id="$(jq -er '.spec.network_id' "$subnet_file")"
route_table_id="$(jq -er '.status.route_table.id' "$subnet_file")"

network_file="$work_dir/network.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$network_file" \
  nebius --profile "$profile" --no-browser --no-check-update \
    vpc network get "$network_id" --format json
if ! jq -e \
  --arg network_id "$network_id" \
  --arg project_id "$project_id" \
  --arg route_table_id "$route_table_id" '
    .metadata.id == $network_id and
    .metadata.parent_id == $project_id and
    .status.state == "READY" and
    .status.default_route_table_id == $route_table_id
  ' "$network_file" >/dev/null; then
  echo "The selected subnet network is not ready with its exact default route table." >&2
  exit 1
fi

route_table_file="$work_dir/route-table.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$route_table_file" \
  nebius --profile "$profile" --no-browser --no-check-update \
    vpc route-table get "$route_table_id" --format json
if ! jq -e \
  --arg route_table_id "$route_table_id" \
  --arg project_id "$project_id" \
  --arg network_id "$network_id" \
  --arg subnet_id "$subnet_id" '
    .metadata.id == $route_table_id and
    .metadata.parent_id == $project_id and
    .spec.network_id == $network_id and
    .status.state == "READY" and
    .status.default == true and
    ((.status.assignment.subnets | type) == "array") and
    ((.status.assignment.subnets | index($subnet_id)) != null)
  ' "$route_table_file" >/dev/null; then
  echo "The selected subnet is not assigned to the exact ready default route table." >&2
  exit 1
fi

routes_file="$work_dir/routes.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$routes_file" \
  nebius --profile "$profile" --no-browser --no-check-update \
    vpc route list --parent-id "$route_table_id" --all --format json
if ! jq -e --arg route_table_id "$route_table_id" '
  (.items | type) == "array" and
  (.items | length) == 1 and
  .items[0].metadata.parent_id == $route_table_id and
  .items[0].spec.destination == {"cidr": "0.0.0.0/0"} and
  .items[0].spec.next_hop == {"default_egress_gateway": true}
' "$routes_file" >/dev/null; then
  echo "The selected subnet default route table does not have exactly one default-egress route." >&2
  exit 1
fi

temporary_file="$(mktemp "${terraform_dir}/local.auto.tfvars.json.XXXXXX")"
jq -n \
  --arg nebius_profile "$profile" \
  --arg project_id "$project_id" \
  --arg tenant_id "$tenant_id" \
  --arg subnet_id "$subnet_id" \
  --arg ssh_public_key "$(awk 'NR == 1 { print; exit }' "$ssh_key_file")" \
  '{
    nebius_profile: $nebius_profile,
    project_id: $project_id,
    tenant_id: $tenant_id,
    subnet_id: $subnet_id,
    ssh_public_key: $ssh_public_key
  }' >"$temporary_file"
chmod 0600 "$temporary_file"
assert_private_artifact "$temporary_file" "$DAYWEAVE_MAX_CONTEXT_BYTES" "discovered Terraform context"
mv -f -- "$temporary_file" "$context_file"

echo "Wrote freshly discovered lol context to ignored local.auto.tfvars.json without printing IDs."
