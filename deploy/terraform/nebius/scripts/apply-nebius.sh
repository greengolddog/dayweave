#!/usr/bin/env bash
set -euo pipefail
umask 077

required_confirmation="I_ACCEPT_CHARGES_UP_TO_USD_50_PER_MONTH"
if [[ "${DAYWEAVE_NEBIUS_APPLY:-}" != "$required_confirmation" ]]; then
  echo "Refusing to apply. Supply the charge confirmation only after creating a one-time exact-plan approval." >&2
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
consuming_approval="${terraform_dir}/.dayweave-plan-approval.consuming.$$.json"
consumed_approvals_dir="${terraform_dir}/.dayweave-consumed-approvals"
consumed_approval=""
apply_lock="${terraform_dir}/.dayweave-apply.lock"
snapshot_dir=""
lock_held=false

cleanup() {
  rm -f -- "$consuming_approval"
  if [[ -n "$snapshot_dir" ]]; then
    case "$snapshot_dir" in
      "${TMPDIR:-/tmp}"/dayweave-nebius-apply.*) rm -rf -- "$snapshot_dir" ;;
      *) echo "Refusing to clean an unexpected apply snapshot path." >&2 ;;
    esac
  fi
  if [[ "$lock_held" == true ]]; then
    rmdir -- "$apply_lock" 2>/dev/null || true
  fi
}
trap cleanup EXIT

for command_name in "$terraform_bin" jq nebius openssl cmp; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "$command_name is required." >&2
    exit 1
  fi
done
if ! mkdir -m 700 -- "$apply_lock" 2>/dev/null; then
  echo "Another apply workflow is active, or the apply lock path is unsafe." >&2
  exit 1
fi
lock_held=true

# Moving the approval before validation makes it single-use even when a later
# safety check fails. A fresh explicit review is required for another attempt.
assert_private_artifact "$approval_file" 16384 "one-time apply approval"
mv -- "$approval_file" "$consuming_approval"
assert_private_artifact "$consuming_approval" 16384 "consuming one-time approval"
approval_nonce="$(jq -er '.nonce | strings | select(test("^[0-9a-f]{64}$"))' "$consuming_approval")"
if [[ ! -e "$consumed_approvals_dir" && ! -L "$consumed_approvals_dir" ]]; then
  mkdir -m 700 -- "$consumed_approvals_dir" 2>/dev/null || true
fi
assert_private_directory "$consumed_approvals_dir" "consumed-approval ledger"
nonce_ledger_dir="${consumed_approvals_dir}/${approval_nonce}"
if ! mkdir -m 700 -- "$nonce_ledger_dir" 2>/dev/null; then
  echo "This exact one-time approval nonce was already consumed." >&2
  exit 1
fi
assert_private_directory "$nonce_ledger_dir" "one-time nonce ledger entry"
consumed_approval="${nonce_ledger_dir}/approval.json"
mv -- "$consuming_approval" "$consumed_approval"
assert_private_artifact "$consumed_approval" 16384 "consumed one-time approval"

assert_private_artifact "$plan_file" "$DAYWEAVE_MAX_PLAN_BINARY_BYTES" "reviewed binary plan"
assert_private_artifact "$plan_json" "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "reviewed plan JSON"
assert_private_artifact "$review_file" 131072 "plan review receipt"

snapshot_dir="$(mktemp -d "${TMPDIR:-/tmp}/dayweave-nebius-apply.XXXXXXXX")"
chmod 0700 "$snapshot_dir"
snapshot_plan="$snapshot_dir/dayweave.tfplan"
snapshot_json="$snapshot_dir/dayweave.tfplan.json"
snapshot_review="$snapshot_dir/dayweave.tfplan.review.json"
snapshot_approval="$snapshot_dir/approval.json"
cp -p -- "$plan_file" "$snapshot_plan"
cp -p -- "$plan_json" "$snapshot_json"
cp -p -- "$review_file" "$snapshot_review"
cp -p -- "$consumed_approval" "$snapshot_approval"
assert_private_artifact "$snapshot_plan" "$DAYWEAVE_MAX_PLAN_BINARY_BYTES" "snapshotted binary plan"
assert_private_artifact "$snapshot_json" "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "snapshotted plan JSON"
assert_private_artifact "$snapshot_review" 131072 "snapshotted review receipt"
assert_private_artifact "$snapshot_approval" 16384 "snapshotted one-time approval"

if [[ "$(sha256_file "$plan_file")" != "$(sha256_file "$snapshot_plan")" ||
      "$(sha256_file "$plan_json")" != "$(sha256_file "$snapshot_json")" ||
      "$(sha256_file "$review_file")" != "$(sha256_file "$snapshot_review")" ]]; then
  echo "A reviewed artifact changed while the private apply snapshot was being made." >&2
  exit 1
fi

if ! jq -e '
  (keys | sort) == [
    "approved_at", "nonce", "plan_json_sha256", "plan_sha256",
    "receipt_sha256", "schema"
  ] and
  .schema == "dayweave-nebius-one-time-approval-v1" and
  (.receipt_sha256 | test("^[0-9a-f]{64}$")) and
  (.plan_sha256 | test("^[0-9a-f]{64}$")) and
  (.plan_json_sha256 | test("^[0-9a-f]{64}$")) and
  (.nonce | test("^[0-9a-f]{64}$")) and
  (.approved_at | type == "number")
' "$snapshot_approval" >/dev/null; then
  echo "The consumed one-time approval is malformed." >&2
  exit 1
fi
if ! jq -e '
  .schema == "dayweave-nebius-plan-review-v1" and
  (.plan_sha256 | test("^[0-9a-f]{64}$")) and
  (.plan_json_sha256 | test("^[0-9a-f]{64}$")) and
  (.context_sha256 | test("^[0-9a-f]{64}$")) and
  (.ssh_public_key_sha256 | test("^[0-9a-f]{64}$")) and
  (.plan_timestamp | type == "number") and
  (.source_digests | type == "object")
' "$snapshot_review" >/dev/null; then
  echo "The snapshotted review receipt is malformed." >&2
  exit 1
fi
if [[ "$(jq -er '.receipt_sha256' "$snapshot_approval")" != "$(sha256_file "$snapshot_review")" ||
      "$(jq -er '.plan_sha256' "$snapshot_approval")" != "$(sha256_file "$snapshot_plan")" ||
      "$(jq -er '.plan_json_sha256' "$snapshot_approval")" != "$(sha256_file "$snapshot_json")" ||
      "$(jq -er '.plan_sha256' "$snapshot_review")" != "$(sha256_file "$snapshot_plan")" ||
      "$(jq -er '.plan_json_sha256' "$snapshot_review")" != "$(sha256_file "$snapshot_json")" ]]; then
  echo "The one-time approval, review receipt, binary plan, and JSON are not cryptographically bound." >&2
  exit 1
fi

rendered_json="$snapshot_dir/rendered-plan.json"
bounded_run_to_file "$DAYWEAVE_MAX_PLAN_JSON_BYTES" "$rendered_json" \
  "$terraform_bin" -chdir="$terraform_dir" show -json "$snapshot_plan"
if ! cmp -s "$rendered_json" "$snapshot_json"; then
  echo "The approved JSON is not the exact rendering of the approved binary plan." >&2
  exit 1
fi

plan_created_at="$(jq -er '.timestamp | fromdateiso8601' "$snapshot_json")"
receipt_created_at="$(jq -er '.plan_timestamp' "$snapshot_review")"
approved_at="$(jq -er '.approved_at' "$snapshot_approval")"
now="$(date +%s)"
plan_age_seconds=$((now - plan_created_at))
if ((receipt_created_at != plan_created_at || approved_at < plan_created_at || approved_at > now || plan_age_seconds < 0 || plan_age_seconds > 3600)); then
  echo "The exact approved plan is stale or has inconsistent timestamps; create and review a fresh plan." >&2
  exit 1
fi

assert_source_digests_match "$terraform_dir" "$snapshot_review"
"${terraform_dir}/scripts/verify-plan.sh" "$snapshot_json"
"${terraform_dir}/scripts/estimate-cost.sh" "$snapshot_json"

# This read-only discovery is intentionally repeated immediately before apply.
# It overwrites only the ignored context file and must match every planned ID and
# the complete SSH key, while the receipt separately binds the key digest.
"${terraform_dir}/scripts/discover-context.sh"
assert_private_artifact "$context_file" "$DAYWEAVE_MAX_CONTEXT_BYTES" "freshly rediscovered lol context"
assert_context_matches_plan "$context_file" "$snapshot_json"
if [[ "$(sha256_file "$context_file")" != "$(jq -er '.context_sha256' "$snapshot_review")" ]]; then
  echo "The freshly rediscovered lol tenant/project/subnet/key context differs from the approved context." >&2
  exit 1
fi
if [[ "$(jq -jr '.variables.ssh_public_key.value' "$snapshot_json" | openssl dgst -sha256 | awk '{print $NF}')" != "$(jq -er '.ssh_public_key_sha256' "$snapshot_review")" ]]; then
  echo "The freshly approved SSH public-key digest does not match the plan receipt." >&2
  exit 1
fi

assert_source_digests_match "$terraform_dir" "$snapshot_review"
assert_context_matches_plan "$context_file" "$snapshot_json"

export TF_IN_AUTOMATION=1
"$terraform_bin" -chdir="$terraform_dir" apply -input=false "$snapshot_plan"

echo "Nebius infrastructure applied from the one-time approved snapshot. Complete the documented application-secret bootstrap."
