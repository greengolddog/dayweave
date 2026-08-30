#!/usr/bin/env bash
set -euo pipefail
umask 077

compute_disk_ceiling="42"

if [[ "$#" -ne 1 ]]; then
  echo "Usage: $0 <verified-terraform-plan-json>" >&2
  exit 64
fi
plan_json="$1"
terraform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=guard-lib.sh
# shellcheck disable=SC1091
source "${terraform_dir}/scripts/guard-lib.sh"

for command_name in jq nebius openssl; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "$command_name is required." >&2
    exit 1
  fi
done
assert_regular_bounded_file "$plan_json" "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "verified plan JSON"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-cost-estimate.XXXXXXXX")"
chmod 0700 "$work_dir"
cleanup() {
  case "$work_dir" in
    "${TMPDIR:-/tmp}"/dayweave-cost-estimate.*) rm -rf -- "$work_dir" ;;
    *) echo "Refusing to clean an unexpected estimate path." >&2 ;;
  esac
}
trap cleanup EXIT

if ! profile="$(nebius_profile_from_plan "$plan_json")"; then
  echo "The verified plan does not contain an allowed Nebius profile name." >&2
  exit 1
fi
resource_specs_file="$work_dir/resource-specs.json"
jq -ec '
  def after($address):
    [.resource_changes[] | select(.mode == "managed" and .address == $address)]
    | if length == 1 then .[0].change.after else error("missing planned resource") end;
  after("nebius_compute_v1_instance.app") as $vm |
  after("nebius_compute_v1_disk.boot") as $disk |
  [
    {
      compute_instance_spec: {
        metadata: {parent_id: $vm.parent_id},
        spec: {
          resources: {
            platform: $vm.resources.platform,
            preset: $vm.resources.preset
          },
          boot_disk: {
            attach_mode: ($vm.boot_disk.attach_mode | ascii_downcase),
            device_id: $vm.boot_disk.device_id,
            managed_disk: {
              name: $disk.name,
              labels: $disk.labels,
              spec: {
                type: ($disk.type | ascii_downcase),
                size_gibibytes: $disk.size_gibibytes,
                block_size_bytes: $disk.block_size_bytes,
                forbid_deletion: $disk.forbid_deletion,
                source_image_family: {
                  image_family: $disk.source_image_family.image_family
                }
              }
            }
          },
          network_interfaces: [
            {
              name: $vm.network_interfaces[0].name,
              subnet_id: $vm.network_interfaces[0].subnet_id,
              ip_address: {}
            }
          ],
          stopped: $vm.stopped,
          recovery_policy: ($vm.recovery_policy | ascii_downcase)
        }
      }
    }
  ]' "$plan_json" >"$resource_specs_file"
chmod 0600 "$resource_specs_file"
assert_private_artifact "$resource_specs_file" 65536 "calculator request"
resource_specs="$(<"$resource_specs_file")"

estimate_file="$work_dir/estimate.json"
bounded_run_to_file "$DAYWEAVE_MAX_NEBIUS_BODY_BYTES" "$estimate_file" \
  nebius --profile "$profile" --no-browser --no-check-update \
    billing v1alpha1 calculator estimate-batch \
    --resource-specs "$resource_specs" --format json
unset resource_specs
monthly_cost="$(jq -er '.monthly_cost.general.total.cost | tonumber | select(. >= 0)' "$estimate_file")"

if ! jq -en \
  --argjson monthly_cost "$monthly_cost" \
  --argjson ceiling "$compute_disk_ceiling" \
  '$monthly_cost <= $ceiling' >/dev/null; then
  echo "Refusing to continue: Nebius reports compute and disk monthly cost ${monthly_cost}, above the reserved-headroom ceiling ${compute_disk_ceiling}." >&2
  exit 1
fi

echo "Live calculator guard passed: compute and disk monthly amount ${monthly_cost} is at or below ${compute_disk_ceiling}."
echo "The alpha calculator excludes bucket operations, egress, taxes, and future tunnel pricing; manually confirm the full USD 50 ceiling before apply."
