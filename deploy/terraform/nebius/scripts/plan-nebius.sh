#!/usr/bin/env bash
set -euo pipefail
umask 077

terraform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=guard-lib.sh
# shellcheck disable=SC1091
source "${terraform_dir}/scripts/guard-lib.sh"

terraform_bin="${DAYWEAVE_TERRAFORM_BIN:-terraform}"
plan_file="${terraform_dir}/dayweave.tfplan"
plan_json="${terraform_dir}/dayweave.tfplan.json"
review_file="${terraform_dir}/dayweave.tfplan.review.json"
approval_file="${terraform_dir}/.dayweave-plan-approval.json"
pending_plan="${terraform_dir}/dayweave.pending.tfplan"
pending_plan_json="${terraform_dir}/dayweave.pending.tfplan.json"
pending_review="${terraform_dir}/dayweave.pending.tfplan.review.json"
context_file="${terraform_dir}/local.auto.tfvars.json"

cleanup() {
  rm -f -- "$pending_plan" "$pending_plan_json" "$pending_review"
}
trap cleanup EXIT

for command_name in "$terraform_bin" jq nebius openssl cmp; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "$command_name is required." >&2
    exit 1
  fi
done

# A new plan invalidates every unconsumed approval for an older artifact set.
rm -f -- "$plan_file" "$plan_json" "$review_file" "$approval_file" \
  "$pending_plan" "$pending_plan_json" "$pending_review"
"${terraform_dir}/scripts/discover-context.sh"
assert_private_artifact "$context_file" "$DAYWEAVE_MAX_CONTEXT_BYTES" "discovered context"

export TF_IN_AUTOMATION=1
"$terraform_bin" -chdir="$terraform_dir" init -input=false -lockfile=readonly
"$terraform_bin" -chdir="$terraform_dir" fmt -check -recursive
"$terraform_bin" -chdir="$terraform_dir" validate
(
  ulimit -f 262144
  "$terraform_bin" -chdir="$terraform_dir" plan \
    -input=false -no-color -out="$pending_plan" >/dev/null
)
chmod 0600 "$pending_plan"
assert_private_artifact "$pending_plan" "$DAYWEAVE_MAX_PLAN_BINARY_BYTES" "Terraform binary plan"
bounded_run_to_file "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "$pending_plan_json" \
  "$terraform_bin" -chdir="$terraform_dir" show -json "$pending_plan"
"${terraform_dir}/scripts/verify-plan.sh" "$pending_plan_json"
"${terraform_dir}/scripts/estimate-cost.sh" "$pending_plan_json"
assert_context_matches_plan "$context_file" "$pending_plan_json"

plan_sha256="$(sha256_file "$pending_plan")"
plan_json_sha256="$(sha256_file "$pending_plan_json")"
context_sha256="$(sha256_file "$context_file")"
ssh_public_key_sha256="$(jq -jr '.variables.ssh_public_key.value' "$pending_plan_json" | openssl dgst -sha256 | awk '{print $NF}')"
plan_timestamp="$(jq -er '.timestamp | fromdateiso8601' "$pending_plan_json")"
source_digests="$(reviewed_source_digests_json "$terraform_dir")"

jq -n \
  --arg schema "dayweave-nebius-plan-review-v1" \
  --arg plan_sha256 "$plan_sha256" \
  --arg plan_json_sha256 "$plan_json_sha256" \
  --arg context_sha256 "$context_sha256" \
  --arg ssh_public_key_sha256 "$ssh_public_key_sha256" \
  --argjson plan_timestamp "$plan_timestamp" \
  --argjson source_digests "$source_digests" \
  '{
    schema: $schema,
    plan_sha256: $plan_sha256,
    plan_json_sha256: $plan_json_sha256,
    context_sha256: $context_sha256,
    ssh_public_key_sha256: $ssh_public_key_sha256,
    plan_timestamp: $plan_timestamp,
    source_digests: $source_digests
  }' >"$pending_review"
chmod 0600 "$pending_review"
assert_private_artifact "$pending_review" 131072 "plan review receipt"

mv -- "$pending_plan_json" "$plan_json"
mv -- "$pending_plan" "$plan_file"
mv -- "$pending_review" "$review_file"
trap - EXIT

echo "Saved a verified, unapplied 13-create plan. No resource was created."
echo "Review dayweave.tfplan and dayweave.tfplan.json, then approve the exact binary SHA-256:"
echo "$plan_sha256"
