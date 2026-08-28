#!/usr/bin/env bash
set -euo pipefail

verify_dir="$(mktemp -d /tmp/dayweave-restore-check.XXXXXXXX)"
trap 'rm -rf "${verify_dir}"' EXIT

: "${DAYWEAVE_BACKUP_BUCKET:?DAYWEAVE_BACKUP_BUCKET is required}"
: "${DAYWEAVE_BACKUP_IDENTITY_FILE:?DAYWEAVE_BACKUP_IDENTITY_FILE is required}"
: "${AWS_ENDPOINT_URL:?AWS_ENDPOINT_URL is required}"

latest_key="$(aws --endpoint-url "${AWS_ENDPOINT_URL}" s3api list-objects-v2 \
  --bucket "${DAYWEAVE_BACKUP_BUCKET}" --prefix postgres/ \
  --query 'sort_by(Contents,&LastModified)[-1].Key' --output text)"

if [[ -z "${latest_key}" || "${latest_key}" == "None" ]]; then
    echo "No PostgreSQL backup objects were found" >&2
    exit 1
fi

encrypted_path="${verify_dir}/latest.dump.age"
plain_path="${verify_dir}/latest.dump"
aws --endpoint-url "${AWS_ENDPOINT_URL}" s3 cp \
  "s3://${DAYWEAVE_BACKUP_BUCKET}/${latest_key}" "${encrypted_path}" --only-show-errors
age --decrypt --identity "${DAYWEAVE_BACKUP_IDENTITY_FILE}" \
  --output "${plain_path}" "${encrypted_path}"
pg_restore --list "${plain_path}" >/dev/null

echo "Verified decryptability and PostgreSQL archive structure for ${latest_key}"

