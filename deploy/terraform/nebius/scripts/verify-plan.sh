#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "Usage: $0 <terraform-plan-json>" >&2
  exit 64
fi

plan_json="$1"
terraform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=guard-lib.sh
# shellcheck disable=SC1091
source "${terraform_dir}/scripts/guard-lib.sh"
cloud_init_template="${terraform_dir}/cloud-init.yaml.tftpl"
expected_cloud_init_template_sha256="22ce2bb7528d8a7c0b10b6e663fdcc08af0b74a1bea44d39e5fda8068948328b"
expected_main_tf_sha256="47fd64afaa9130b50a6c46bf54400ae84e237a77f7acba4e3f0c39b7b91ba568"
expected_versions_tf_sha256="740c8325be3aa973a984b32632736abb8f42681c6be5d78c7dd6fb223d9d4ccc"
expected_variables_tf_sha256="b0d5144a87092971c7df65989c6dc396033d15611f64200c221a7be6dd547a37"
expected_outputs_tf_sha256="2398924278dafd5d82d2be4bc9e744342f8841ff8ea01bb6173255894ce126bd"
expected_lock_file_sha256="0a92a22310ce7e61f2266e3c924d85b022318e3f1f9efef7a340f1c34b803b0a"

for command_name in jq openssl cmp; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "${command_name} is required." >&2
    exit 1
  fi
done
assert_regular_bounded_file "${plan_json}" "${DAYWEAVE_MAX_PLAN_JSON_BYTES}" "plan JSON"

verify_reviewed_digest() {
  local source_file="$1"
  local expected_digest="$2"
  local description="$3"
  local actual_digest

  if [[ ! -f "${source_file}" || -L "${source_file}" ]]; then
    echo "The reviewed ${description} must be a regular, non-symlink file." >&2
    return 1
  fi
  actual_digest="$(openssl dgst -sha256 "${source_file}" | awk '{print $NF}')"
  if [[ "${actual_digest}" != "${expected_digest}" ]]; then
    echo "The reviewed ${description} digest changed; refusing the plan." >&2
    return 1
  fi
}

verify_reviewed_digest \
  "${cloud_init_template}" "${expected_cloud_init_template_sha256}" "cloud-init template"
verify_reviewed_digest \
  "${terraform_dir}/main.tf" "${expected_main_tf_sha256}" "Terraform resource source"
verify_reviewed_digest \
  "${terraform_dir}/versions.tf" "${expected_versions_tf_sha256}" "Terraform provider source"
verify_reviewed_digest \
  "${terraform_dir}/variables.tf" "${expected_variables_tf_sha256}" "Terraform variable source"
verify_reviewed_digest \
  "${terraform_dir}/outputs.tf" "${expected_outputs_tf_sha256}" "Terraform output source"
verify_reviewed_digest \
  "${terraform_dir}/.terraform.lock.hcl" "${expected_lock_file_sha256}" "provider lock file"

jq -e '
  def changes: [.resource_changes[]];
  def managed: [changes[] | select(.mode == "managed")];
  def change($address):
    [managed[] | select(.address == $address)]
    | if length == 1 then .[0].change else null end;
  def one($address):
    change($address) | if . == null then null else .after end;
  def configured($address):
    [.configuration.root_module.resources[] | select(.address == $address)]
    | if length == 1 then .[0] else null end;
  def references($address; $field):
    configured($address)
    | if . == null then [] else (.expressions[$field].references // []) end;
  def configured_expression_keys:
    .configuration.root_module.resources
    | map({key: .address, value: (.expressions | keys)})
    | from_entries;
  def configured_output_references:
    .configuration.root_module.outputs
    | to_entries
    | map({key: .key, value: (.value.expression.references // [])})
    | from_entries;
  def labels: {
    "application": "dayweave",
    "environment": "production",
    "managed_by": "terraform"
  };
  def expected_addresses: [
    "nebius_compute_v1_disk.boot",
    "nebius_compute_v1_instance.app",
    "nebius_iam_v1_access_permit.tunnel_agent",
    "nebius_iam_v1_group.attachment_writers",
    "nebius_iam_v1_group.backup_writers",
    "nebius_iam_v1_group.tunnel_agents",
    "nebius_iam_v1_group_membership.attachments",
    "nebius_iam_v1_group_membership.backup",
    "nebius_iam_v1_group_membership.tunnel_agent",
    "nebius_iam_v1_service_account.backup",
    "nebius_iam_v1_service_account.runtime",
    "nebius_storage_v1_bucket.data",
    "nebius_tunnel_v1_tunnel.api"
  ];

  .variables.project_id.value as $project_id |
  .variables.tenant_id.value as $tenant_id |
  .variables.subnet_id.value as $subnet_id |
  .variables.ssh_user.value as $ssh_user |
  .variables.ssh_public_key.value as $ssh_public_key |

  (.format_version == "1.2") and
  ((.timestamp | fromdateiso8601) | type == "number") and
  (.variables | keys | sort == [
    "nebius_profile", "project_id", "ssh_public_key",
    "ssh_user", "subnet_id", "tenant_id"
  ]) and
  (.variables.nebius_profile.value == "lol") and
  ($project_id | type == "string" and startswith("project-")) and
  ($tenant_id | type == "string" and startswith("tenant-")) and
  ($subnet_id | type == "string" and startswith("vpcsubnet-")) and
  ($ssh_user | type == "string" and test("^[a-z_][a-z0-9_-]{0,30}$")) and
  ([
    "_apt", "admin", "backup", "bin", "daemon", "games", "gnats", "irc",
    "landscape", "list", "lp", "mail", "man", "messagebus", "news",
    "nobody", "pollinate", "proxy", "root", "sshd", "sync", "sys",
    "syslog", "systemd-network", "systemd-timesync", "tcpdump", "tss",
    "ubuntu", "uucp", "uuidd", "www-data"
  ] | index($ssh_user) == null) and
  ($ssh_public_key | type == "string") and
  ($ssh_public_key | test("[\\r\\n]") | not) and
  ($ssh_public_key | test("^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp256) [A-Za-z0-9+/]+={0,3}( [^\\r\\n]+)?$")) and

  (.configuration.provider_config == {
    "nebius": {
      "name": "nebius",
      "full_name": "registry.terraform.io/nebius/nebius",
      "version_constraint": "0.6.48",
      "expressions": {
        "profile": {"references": ["var.nebius_profile"]}
      }
    }
  }) and
  (changes | all(.mode == "managed")) and
  (managed | map(.address) | sort == expected_addresses) and
  (managed | all(.change.actions == ["create"])) and
  ((.configuration.root_module.module_calls // {}) | length == 0) and
  (.configuration.root_module.resources | map(.address) | sort == expected_addresses) and
  (.configuration.root_module.resources | all(.mode == "managed" and .provider_config_key == "nebius")) and
  (.configuration.root_module.resources | all((.provisioners // []) | length == 0)) and
  (.configuration.root_module.resources | all(
    if .address == "nebius_compute_v1_instance.app" then
      .depends_on == [
        "nebius_iam_v1_access_permit.tunnel_agent",
        "nebius_iam_v1_group_membership.tunnel_agent"
      ]
    else
      ((.depends_on // []) | length == 0)
    end
  )) and
  (configured_expression_keys == {
    "nebius_compute_v1_disk.boot": [
      "block_size_bytes", "forbid_deletion", "labels", "name", "parent_id",
      "size_gibibytes", "source_image_family", "type"
    ],
    "nebius_compute_v1_instance.app": [
      "boot_disk", "cloud_init_user_data", "hostname", "labels", "name",
      "network_interfaces", "parent_id", "recovery_policy", "resources",
      "service_account_id", "stopped"
    ],
    "nebius_iam_v1_access_permit.tunnel_agent": [
      "labels", "name", "parent_id", "resource_id", "role"
    ],
    "nebius_iam_v1_group.attachment_writers": ["labels", "name", "parent_id"],
    "nebius_iam_v1_group.backup_writers": ["labels", "name", "parent_id"],
    "nebius_iam_v1_group.tunnel_agents": ["labels", "name", "parent_id"],
    "nebius_iam_v1_group_membership.attachments": [
      "labels", "member_id", "name", "parent_id"
    ],
    "nebius_iam_v1_group_membership.backup": [
      "labels", "member_id", "name", "parent_id"
    ],
    "nebius_iam_v1_group_membership.tunnel_agent": [
      "labels", "member_id", "name", "parent_id"
    ],
    "nebius_iam_v1_service_account.backup": [
      "description", "labels", "name", "parent_id"
    ],
    "nebius_iam_v1_service_account.runtime": [
      "description", "labels", "name", "parent_id"
    ],
    "nebius_storage_v1_bucket.data": [
      "bucket_policy", "default_storage_class", "force_storage_class", "labels",
      "lifecycle_configuration", "max_size_bytes", "name", "object_audit_logging",
      "parent_id", "versioning_policy"
    ],
    "nebius_tunnel_v1_tunnel.api": [
      "description", "labels", "name", "parent_id", "title"
    ]
  }) and
  (.configuration.root_module.outputs | keys | sort == [
    "backup_bucket_name", "backup_service_account_id", "instance_id",
    "runtime_service_account_id", "ssh_user", "tunnel_http_url", "tunnel_id",
    "tunnel_ssh_route"
  ]) and
  (.configuration.root_module.outputs | to_entries | all((.value.sensitive // false) == false)) and
  (configured_output_references == {
    "backup_bucket_name": [
      "nebius_storage_v1_bucket.data.name", "nebius_storage_v1_bucket.data"
    ],
    "backup_service_account_id": [
      "nebius_iam_v1_service_account.backup.id",
      "nebius_iam_v1_service_account.backup"
    ],
    "instance_id": [
      "nebius_compute_v1_instance.app.id", "nebius_compute_v1_instance.app"
    ],
    "runtime_service_account_id": [
      "nebius_iam_v1_service_account.runtime.id",
      "nebius_iam_v1_service_account.runtime"
    ],
    "ssh_user": ["var.ssh_user"],
    "tunnel_http_url": [
      "nebius_tunnel_v1_tunnel.api.id", "nebius_tunnel_v1_tunnel.api"
    ],
    "tunnel_id": [
      "nebius_tunnel_v1_tunnel.api.id", "nebius_tunnel_v1_tunnel.api"
    ],
    "tunnel_ssh_route": [
      "nebius_tunnel_v1_tunnel.api.id", "nebius_tunnel_v1_tunnel.api"
    ]
  }) and
  ([managed[].change.after_sensitive | .. | select(. == true)] | length == 1) and

  (one("nebius_compute_v1_instance.app") as $vm |
    $vm != null and
    $vm.name == "dayweave" and
    $vm.hostname == "dayweave" and
    $vm.parent_id == $project_id and
    $vm.labels == labels and
    $vm.resources == {"platform": "cpu-e2", "preset": "2vcpu-8gb"} and
    $vm.stopped == false and
    $vm.recovery_policy == "RECOVER" and
    $vm.boot_disk.attach_mode == "READ_WRITE" and
    $vm.boot_disk.device_id == "dayweave-root" and
    $vm.boot_disk.existing_disk == {} and
    $vm.boot_disk.managed_disk == null and
    (($vm.secondary_disks // []) | length == 0) and
    $vm.filesystems == null and
    $vm.local_disks == null and
    $vm.gpu_cluster == null and
    $vm.nvl_instance_group_id == null and
    $vm.preemptible == null and
    $vm.reservation_policy == null and
    $vm.cloud_init_user_data == null and
    (change("nebius_compute_v1_instance.app").after_unknown.cloud_init_user_data == true) and
    ($vm.network_interfaces | length == 1) and
    ($vm.network_interfaces[0].name == "eth0") and
    ($vm.network_interfaces[0].subnet_id == $subnet_id) and
    ($vm.network_interfaces | all(
      .public_ip_address == null and
      .aliases == null and
      .security_groups == null
    )) and
    (references("nebius_compute_v1_instance.app"; "parent_id") == ["var.project_id"]) and
    (references("nebius_compute_v1_instance.app"; "network_interfaces") == ["var.subnet_id"]) and
    (references("nebius_compute_v1_instance.app"; "boot_disk") == [
      "nebius_compute_v1_disk.boot.id", "nebius_compute_v1_disk.boot"
    ]) and
    (references("nebius_compute_v1_instance.app"; "service_account_id") == [
      "nebius_iam_v1_service_account.runtime.id",
      "nebius_iam_v1_service_account.runtime"
    ]) and
    (references("nebius_compute_v1_instance.app"; "cloud_init_user_data") == [
      "path.module", "var.ssh_user", "var.ssh_public_key",
      "nebius_tunnel_v1_tunnel.api.id", "nebius_tunnel_v1_tunnel.api"
    ])) and

  (one("nebius_compute_v1_disk.boot") as $disk |
    $disk != null and
    $disk.name == "dayweave-boot" and
    $disk.parent_id == $project_id and
    $disk.labels == labels and
    $disk.type == "NETWORK_SSD" and
    $disk.size_gibibytes == 32 and
    $disk.block_size_bytes == 4096 and
    $disk.forbid_deletion == true and
    $disk.source_image_family.image_family == "ubuntu24.04-driverless" and
    (references("nebius_compute_v1_disk.boot"; "parent_id") == ["var.project_id"])) and

  (one("nebius_storage_v1_bucket.data") as $bucket |
    $bucket != null and
    ($bucket.name | test("^dayweave-[0-9a-f]{12}$")) and
    $bucket.parent_id == $project_id and
    $bucket.labels == labels and
    $bucket.default_storage_class == "STANDARD" and
    $bucket.force_storage_class == true and
    $bucket.max_size_bytes == 10737418240 and
    $bucket.object_audit_logging == "MUTATE_ONLY" and
    $bucket.versioning_policy == "ENABLED" and
    ($bucket.bucket_policy.rules | length == 2) and
    ($bucket.bucket_policy.rules[0].anonymous == null) and
    ($bucket.bucket_policy.rules[0].paths == ["postgres/*"]) and
    ($bucket.bucket_policy.rules[0].roles == ["storage.object-editor"]) and
    ($bucket.bucket_policy.rules[1].anonymous == null) and
    ($bucket.bucket_policy.rules[1].paths == ["attachments/*"]) and
    ($bucket.bucket_policy.rules[1].roles == ["storage.object-editor"]) and
    (references("nebius_storage_v1_bucket.data"; "parent_id") == ["var.project_id"]) and
    (references("nebius_storage_v1_bucket.data"; "bucket_policy") == [
      "nebius_iam_v1_group.backup_writers.id",
      "nebius_iam_v1_group.backup_writers",
      "nebius_iam_v1_group.attachment_writers.id",
      "nebius_iam_v1_group.attachment_writers"
    ]) and
    (references("nebius_storage_v1_bucket.data"; "lifecycle_configuration") == []) and
    ($bucket.lifecycle_configuration.rules | length == 1) and
    ($bucket.lifecycle_configuration.rules | all(
      .id == "expire-encrypted-postgres-backups" and
      .status == "ENABLED" and
      .filter == {"prefix": "postgres/"} and
      .expiration == {
        "date": null,
        "days": 7,
        "expired_object_delete_marker": null
      } and
      .noncurrent_version_expiration == {
        "newer_noncurrent_versions": null,
        "noncurrent_days": 7
      } and
      .abort_incomplete_multipart_upload == {"days_after_initiation": 1} and
      .transition == null and
      .noncurrent_version_transition == null
    ))) and

  (one("nebius_tunnel_v1_tunnel.api") as $tunnel |
    $tunnel != null and
    $tunnel.name == "dayweave-api" and
    $tunnel.title == "DayWeave API" and
    $tunnel.description == "Managed HTTP and SSH ingress for the private DayWeave VM." and
    $tunnel.parent_id == $project_id and
    $tunnel.labels == labels and
    (references("nebius_tunnel_v1_tunnel.api"; "parent_id") == ["var.project_id"])) and

  (one("nebius_iam_v1_access_permit.tunnel_agent") as $permit |
    $permit != null and
    $permit.name == "dayweave-tunnel-connect" and
    $permit.role == "applicationtunnel.agent" and
    $permit.labels == labels and
    (references("nebius_iam_v1_access_permit.tunnel_agent"; "resource_id") == [
      "nebius_tunnel_v1_tunnel.api.id", "nebius_tunnel_v1_tunnel.api"
    ]) and
    (references("nebius_iam_v1_access_permit.tunnel_agent"; "parent_id") == [
      "nebius_iam_v1_group.tunnel_agents.id",
      "nebius_iam_v1_group.tunnel_agents"
    ])) and

  (one("nebius_iam_v1_service_account.runtime") as $runtime |
    $runtime.name == "dayweave-runtime" and
    $runtime.parent_id == $project_id and
    $runtime.description == "Identity attached to the DayWeave VM; no broad project role." and
    $runtime.labels == labels and
    (references("nebius_iam_v1_service_account.runtime"; "parent_id") == ["var.project_id"])) and
  (one("nebius_iam_v1_service_account.backup") as $backup |
    $backup.name == "dayweave-backup" and
    $backup.parent_id == $project_id and
    $backup.description == "Identity restricted to DayWeave backup objects." and
    $backup.labels == labels and
    (references("nebius_iam_v1_service_account.backup"; "parent_id") == ["var.project_id"])) and

  (one("nebius_iam_v1_group.attachment_writers") as $group |
    $group.name == "dayweave-attachment-writers" and
    $group.parent_id == $tenant_id and
    $group.labels == labels and
    references("nebius_iam_v1_group.attachment_writers"; "parent_id") == ["var.tenant_id"]) and
  (one("nebius_iam_v1_group.backup_writers") as $group |
    $group.name == "dayweave-backup-writers" and
    $group.parent_id == $tenant_id and
    $group.labels == labels and
    references("nebius_iam_v1_group.backup_writers"; "parent_id") == ["var.tenant_id"]) and
  (one("nebius_iam_v1_group.tunnel_agents") as $group |
    $group.name == "dayweave-tunnel-agents" and
    $group.parent_id == $tenant_id and
    $group.labels == labels and
    references("nebius_iam_v1_group.tunnel_agents"; "parent_id") == ["var.tenant_id"]) and

  (references("nebius_iam_v1_group_membership.attachments"; "parent_id") == [
    "nebius_iam_v1_group.attachment_writers.id",
    "nebius_iam_v1_group.attachment_writers"
  ]) and
  (references("nebius_iam_v1_group_membership.attachments"; "member_id") == [
    "nebius_iam_v1_service_account.runtime.id",
    "nebius_iam_v1_service_account.runtime"
  ]) and
  (references("nebius_iam_v1_group_membership.backup"; "parent_id") == [
    "nebius_iam_v1_group.backup_writers.id",
    "nebius_iam_v1_group.backup_writers"
  ]) and
  (references("nebius_iam_v1_group_membership.backup"; "member_id") == [
    "nebius_iam_v1_service_account.backup.id",
    "nebius_iam_v1_service_account.backup"
  ]) and
  (references("nebius_iam_v1_group_membership.tunnel_agent"; "parent_id") == [
    "nebius_iam_v1_group.tunnel_agents.id",
    "nebius_iam_v1_group.tunnel_agents"
  ]) and
  (references("nebius_iam_v1_group_membership.tunnel_agent"; "member_id") == [
    "nebius_iam_v1_service_account.runtime.id",
    "nebius_iam_v1_service_account.runtime"
  ]) and
  (one("nebius_iam_v1_group_membership.attachments").labels == labels) and
  (one("nebius_iam_v1_group_membership.backup").labels == labels) and
  (one("nebius_iam_v1_group_membership.tunnel_agent").labels == labels) and
  (one("nebius_iam_v1_group_membership.attachments").name == "dayweave-runtime-attachments") and
  (one("nebius_iam_v1_group_membership.backup").name == "dayweave-backup") and
  (one("nebius_iam_v1_group_membership.tunnel_agent").name == "dayweave-tunnel-agent")
' "${plan_json}" >/dev/null

ssh_user="$(jq -er '.variables.ssh_user.value | strings' "${plan_json}")"
ssh_public_key="$(jq -er '.variables.ssh_public_key.value | strings' "${plan_json}")"
ssh_public_key_json="$(jq -cn --arg key "${ssh_public_key}" '$key')"
tunnel_id_json='"applicationtunnel-e00reviewedplaceholder"'

render_expected_cloud_init() {
  local line
  while IFS= read -r line || [[ -n "${line}" ]]; do
    # Literal Terraform template markers are intentionally matched here.
    # shellcheck disable=SC2016
    case "${line}" in
      '  - name: ${ssh_user}') printf '  - name: %s\n' "${ssh_user}" ;;
      '      - ${jsonencode(ssh_public_key)}') printf '      - %s\n' "${ssh_public_key_json}" ;;
      '      User=${ssh_user}') printf '      User=%s\n' "${ssh_user}" ;;
      '      Group=${ssh_user}') printf '      Group=%s\n' "${ssh_user}" ;;
      '      usermod -aG docker ${ssh_user}') printf '      usermod -aG docker %s\n' "${ssh_user}" ;;
      '      tunnel_id=${jsonencode(tunnel_id)}') printf '      tunnel_id=%s\n' "${tunnel_id_json}" ;;
      '  - [usermod, -aG, docker, ${ssh_user}]') printf '  - [usermod, -aG, docker, %s]\n' "${ssh_user}" ;;
      *'${'*)
        echo "The reviewed cloud-init template contains an unknown interpolation." >&2
        return 1
        ;;
      *) printf '%s\n' "${line}" ;;
    esac
  done <"${cloud_init_template}"
}

rendered_review="$(mktemp "${TMPDIR:-/tmp}/dayweave-reviewed-cloud-init.XXXXXXXX")"
trap 'rm -f -- "${rendered_review}"' EXIT
render_expected_cloud_init >"${rendered_review}"
chmod 0600 "${rendered_review}"
assert_private_artifact "${rendered_review}" 262144 "rendered reviewed cloud-init"
if ! grep -Fqx '      tunnel_id="applicationtunnel-e00reviewedplaceholder"' "${rendered_review}"; then
  echo "The reviewed cloud-init does not inject the exact Terraform tunnel identifier." >&2
  exit 1
fi

if grep -Eq -- '-----BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY-----|AWS_SECRET_ACCESS_KEY[[:space:]]*=|DAYWEAVE_API_TOKEN[[:space:]]*=|POSTGRES_PASSWORD[[:space:]]*=' "${rendered_review}"; then
  echo "The reviewed cloud-init contains forbidden secret material." >&2
  exit 1
fi
trap - EXIT
rm -f -- "${rendered_review}"

echo "Plan guard passed: exact private VM/tunnel IAM, protected 32-GiB SSD, and scoped 10-GiB bucket."
