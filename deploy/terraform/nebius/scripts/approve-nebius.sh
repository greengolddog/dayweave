#!/usr/bin/env bash
set -euo pipefail
umask 077

required_confirmation="I_REVIEWED_THE_EXACT_PLAN_SHA256"
if [[ "$#" -ne 1 ]]; then
  echo "Usage: DAYWEAVE_NEBIUS_APPROVAL=${required_confirmation} $0 <reviewed-plan-sha256>" >&2
  exit 64
fi
reviewed_sha256="$1"
if [[ "${DAYWEAVE_NEBIUS_APPROVAL:-}" != "$required_confirmation" || ! "$reviewed_sha256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Refusing approval. Supply the exact displayed plan digest and explicit review phrase." >&2
  exit 1
fi

terraform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=guard-lib.sh
# shellcheck disable=SC1091
source "${terraform_dir}/scripts/guard-lib.sh"
terraform_bin="${DAYWEAVE_TERRAFORM_BIN:-terraform}"
plan_file="${terraform_dir}/dayweave.tfplan"
plan_json="${terraform_dir}/dayweave.tfplan.json"
review_file="${terraform_dir}/dayweave.tfplan.review.json"
context_file="${terraform_dir}/local.auto.tfvars.json"
approval_file="${terraform_dir}/.dayweave-plan-approval.json"
work_dir=""

cleanup() {
  if [[ -n "$work_dir" ]]; then
    case "$work_dir" in
      "${TMPDIR:-/tmp}"/dayweave-nebius-approval.*) rm -rf -- "$work_dir" ;;
      *) echo "Refusing to clean an unexpected approval path." >&2 ;;
    esac
  fi
}
trap cleanup EXIT

for command_name in "$terraform_bin" jq openssl cmp; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "$command_name is required." >&2
    exit 1
  fi
done
if [[ -e "$approval_file" || -L "$approval_file" ]]; then
  echo "An unconsumed approval already exists; generate a new plan to invalidate it." >&2
  exit 1
fi
assert_private_artifact "$plan_file" "$DAYWEAVE_MAX_PLAN_BINARY_BYTES" "reviewed binary plan"
assert_private_artifact "$plan_json" "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "reviewed plan JSON"
assert_private_artifact "$review_file" 131072 "plan review receipt"
assert_private_artifact "$context_file" "$DAYWEAVE_MAX_CONTEXT_BYTES" "planned context"

if ! jq -e '
  .schema == "dayweave-nebius-plan-review-v1" and
  (.plan_sha256 | test("^[0-9a-f]{64}$")) and
  (.plan_json_sha256 | test("^[0-9a-f]{64}$")) and
  (.context_sha256 | test("^[0-9a-f]{64}$")) and
  (.ssh_public_key_sha256 | test("^[0-9a-f]{64}$")) and
  (.plan_timestamp | type == "number") and
  (.source_digests | type == "object")
' "$review_file" >/dev/null; then
  echo "The plan review receipt is malformed." >&2
  exit 1
fi
receipt_plan_sha256="$(jq -er '.plan_sha256' "$review_file")"
if [[ "$reviewed_sha256" != "$receipt_plan_sha256" || "$(sha256_file "$plan_file")" != "$receipt_plan_sha256" ]]; then
  echo "The explicitly reviewed digest does not identify this exact binary plan." >&2
  exit 1
fi
if [[ "$(sha256_file "$plan_json")" != "$(jq -er '.plan_json_sha256' "$review_file")" ]]; then
  echo "The reviewed plan JSON changed after planning." >&2
  exit 1
fi
if [[ "$(sha256_file "$context_file")" != "$(jq -er '.context_sha256' "$review_file")" ]]; then
  echo "The planned local context changed after planning." >&2
  exit 1
fi
if [[ "$(jq -jr '.variables.ssh_public_key.value' "$plan_json" | openssl dgst -sha256 | awk '{print $NF}')" != "$(jq -er '.ssh_public_key_sha256' "$review_file")" ]]; then
  echo "The reviewed SSH public-key digest does not match the plan." >&2
  exit 1
fi
assert_context_matches_plan "$context_file" "$plan_json"
assert_source_digests_match "$terraform_dir" "$review_file"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-nebius-approval.XXXXXXXX")"
chmod 0700 "$work_dir"
rendered_json="$work_dir/rendered-plan.json"
bounded_run_to_file "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "$rendered_json" \
  "$terraform_bin" -chdir="$terraform_dir" show -json "$plan_file"
if ! cmp -s "$rendered_json" "$plan_json"; then
  echo "The reviewed JSON is not the exact rendering of the reviewed binary plan." >&2
  exit 1
fi
"${terraform_dir}/scripts/verify-plan.sh" "$rendered_json"

receipt_sha256="$(sha256_file "$review_file")"
nonce="$(openssl rand -hex 32)"
approved_at="$(date +%s)"
temporary_approval="$work_dir/approval.json"
jq -n \
  --arg schema "dayweave-nebius-one-time-approval-v1" \
  --arg receipt_sha256 "$receipt_sha256" \
  --arg plan_sha256 "$receipt_plan_sha256" \
  --arg plan_json_sha256 "$(sha256_file "$plan_json")" \
  --arg nonce "$nonce" \
  --argjson approved_at "$approved_at" \
  '{
    schema: $schema,
    receipt_sha256: $receipt_sha256,
    plan_sha256: $plan_sha256,
    plan_json_sha256: $plan_json_sha256,
    nonce: $nonce,
    approved_at: $approved_at
  }' >"$temporary_approval"
chmod 0600 "$temporary_approval"
assert_private_artifact "$temporary_approval" 16384 "one-time approval"
mv -- "$temporary_approval" "$approval_file"

echo "Created one one-time approval bound to the exact reviewed plan. No resource was created."
