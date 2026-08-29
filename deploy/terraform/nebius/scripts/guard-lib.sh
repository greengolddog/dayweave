#!/usr/bin/env bash

# Shared fail-closed primitives for the plan/approve/apply workflow. Callers set
# `set -euo pipefail` before sourcing this file.

# These constants are consumed by scripts that source this library.
# shellcheck disable=SC2034
DAYWEAVE_MAX_PLAN_JSON_BYTES=33554432
# shellcheck disable=SC2034
DAYWEAVE_MAX_PLAN_BINARY_BYTES=67108864
# shellcheck disable=SC2034
DAYWEAVE_MAX_NEBIUS_BODY_BYTES=4194304
# shellcheck disable=SC2034
DAYWEAVE_MAX_CONTEXT_BYTES=65536

file_size_bytes() {
  if stat -f '%z' -- "$1" >/dev/null 2>&1; then
    stat -f '%z' -- "$1"
  else
    stat -c '%s' -- "$1"
  fi
}

file_security_fields() {
  if stat -f '%u %Lp %l' -- "$1" >/dev/null 2>&1; then
    stat -f '%u %Lp %l' -- "$1"
  else
    stat -c '%u %a %h' -- "$1"
  fi
}

directory_security_fields() {
  if stat -f '%u %Lp' -- "$1" >/dev/null 2>&1; then
    stat -f '%u %Lp' -- "$1"
  else
    stat -c '%u %a' -- "$1"
  fi
}

assert_private_directory() {
  local directory="$1"
  local description="$2"
  local owner_id mode

  if [[ -L "$directory" || ! -d "$directory" ]]; then
    echo "$description must be a private, non-symlink directory: $directory" >&2
    return 1
  fi
  read -r owner_id mode < <(directory_security_fields "$directory")
  if [[ "$owner_id" != "$(id -u)" || "$mode" != "700" ]]; then
    echo "$description must be owned by the current user with mode 0700: $directory" >&2
    return 1
  fi
}

assert_regular_bounded_file() {
  local file="$1"
  local max_bytes="$2"
  local description="$3"
  local size

  if [[ -L "$file" || ! -f "$file" ]]; then
    echo "$description must be a regular, non-symlink file: $file" >&2
    return 1
  fi
  size="$(file_size_bytes "$file")"
  if [[ ! "$size" =~ ^[0-9]+$ ]] || ((size == 0 || size > max_bytes)); then
    echo "$description has an invalid or excessive size: $file" >&2
    return 1
  fi
}

assert_private_artifact() {
  local file="$1"
  local max_bytes="$2"
  local description="$3"
  local owner_id mode link_count

  assert_regular_bounded_file "$file" "$max_bytes" "$description" || return 1
  read -r owner_id mode link_count < <(file_security_fields "$file")
  if [[ "$owner_id" != "$(id -u)" || "$mode" != "600" || "$link_count" != "1" ]]; then
    echo "$description must be owned by the current user, mode 0600, with one hard link: $file" >&2
    return 1
  fi
}

sha256_file() {
  openssl dgst -sha256 "$1" | awk '{print $NF}'
}

bounded_run_to_file() {
  local max_bytes="$1"
  local output_file="$2"
  shift 2
  # Leave one filesystem-limit block beyond the semantic ceiling so a writer
  # cannot appear successful after being truncated exactly at the accepted size.
  local block_limit=$(((max_bytes + 511) / 512 + 1))

  if [[ -e "$output_file" || -L "$output_file" ]]; then
    echo "Refusing to overwrite a bounded-command output: $output_file" >&2
    return 1
  fi
  (
    umask 077
    ulimit -f "$block_limit"
    exec "$@"
  ) >"$output_file"
  chmod 0600 "$output_file"
  assert_private_artifact "$output_file" "$max_bytes" "bounded command output" || return 1
}

reviewed_source_digests_json() {
  local terraform_dir="$1"
  local cloud_init main versions variables outputs lock verify estimate discover guard approve apply plan
  local reviewed_source

  for reviewed_source in \
    "$terraform_dir/cloud-init.yaml.tftpl" \
    "$terraform_dir/main.tf" \
    "$terraform_dir/versions.tf" \
    "$terraform_dir/variables.tf" \
    "$terraform_dir/outputs.tf" \
    "$terraform_dir/.terraform.lock.hcl" \
    "$terraform_dir/scripts/verify-plan.sh" \
    "$terraform_dir/scripts/estimate-cost.sh" \
    "$terraform_dir/scripts/discover-context.sh" \
    "$terraform_dir/scripts/guard-lib.sh" \
    "$terraform_dir/scripts/approve-nebius.sh" \
    "$terraform_dir/scripts/apply-nebius.sh" \
    "$terraform_dir/scripts/plan-nebius.sh"; do
    assert_regular_bounded_file "$reviewed_source" 1048576 "reviewed deployment source" || return 1
  done

  cloud_init="$(sha256_file "$terraform_dir/cloud-init.yaml.tftpl")"
  main="$(sha256_file "$terraform_dir/main.tf")"
  versions="$(sha256_file "$terraform_dir/versions.tf")"
  variables="$(sha256_file "$terraform_dir/variables.tf")"
  outputs="$(sha256_file "$terraform_dir/outputs.tf")"
  lock="$(sha256_file "$terraform_dir/.terraform.lock.hcl")"
  verify="$(sha256_file "$terraform_dir/scripts/verify-plan.sh")"
  estimate="$(sha256_file "$terraform_dir/scripts/estimate-cost.sh")"
  discover="$(sha256_file "$terraform_dir/scripts/discover-context.sh")"
  guard="$(sha256_file "$terraform_dir/scripts/guard-lib.sh")"
  approve="$(sha256_file "$terraform_dir/scripts/approve-nebius.sh")"
  apply="$(sha256_file "$terraform_dir/scripts/apply-nebius.sh")"
  plan="$(sha256_file "$terraform_dir/scripts/plan-nebius.sh")"

  jq -cn \
    --arg cloud_init "$cloud_init" \
    --arg main "$main" \
    --arg versions "$versions" \
    --arg variables "$variables" \
    --arg outputs "$outputs" \
    --arg lock "$lock" \
    --arg verify "$verify" \
    --arg estimate "$estimate" \
    --arg discover "$discover" \
    --arg guard "$guard" \
    --arg approve "$approve" \
    --arg apply "$apply" \
    --arg plan "$plan" \
    '{
      "cloud-init.yaml.tftpl": $cloud_init,
      "main.tf": $main,
      "versions.tf": $versions,
      "variables.tf": $variables,
      "outputs.tf": $outputs,
      ".terraform.lock.hcl": $lock,
      "scripts/verify-plan.sh": $verify,
      "scripts/estimate-cost.sh": $estimate,
      "scripts/discover-context.sh": $discover,
      "scripts/guard-lib.sh": $guard,
      "scripts/approve-nebius.sh": $approve,
      "scripts/apply-nebius.sh": $apply,
      "scripts/plan-nebius.sh": $plan
    }'
}

assert_source_digests_match() {
  local terraform_dir="$1"
  local receipt_file="$2"
  local actual_file expected_file
  actual_file="$(mktemp "${TMPDIR:-/tmp}/dayweave-source-digests.XXXXXXXX")"
  expected_file="$(mktemp "${TMPDIR:-/tmp}/dayweave-source-digests.XXXXXXXX")"
  chmod 0600 "$actual_file" "$expected_file"
  reviewed_source_digests_json "$terraform_dir" | jq -S . >"$actual_file"
  jq -S '.source_digests' "$receipt_file" >"$expected_file"
  if ! cmp -s "$actual_file" "$expected_file"; then
    rm -f -- "$actual_file" "$expected_file"
    echo "A reviewed Terraform, guard, or provider-lock source changed after planning." >&2
    return 1
  fi
  rm -f -- "$actual_file" "$expected_file"
}

assert_context_matches_plan() {
  local context_file="$1"
  local plan_json="$2"

  jq -e --slurpfile context "$context_file" '
    ($context | length) == 1 and
    ($context[0] | keys | sort) == [
      "nebius_profile", "project_id", "ssh_public_key", "subnet_id", "tenant_id"
    ] and
    .variables.nebius_profile.value == $context[0].nebius_profile and
    .variables.project_id.value == $context[0].project_id and
    .variables.tenant_id.value == $context[0].tenant_id and
    .variables.subnet_id.value == $context[0].subnet_id and
    .variables.ssh_public_key.value == $context[0].ssh_public_key
  ' "$plan_json" >/dev/null
}
