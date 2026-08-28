#!/usr/bin/env bash
set -euo pipefail

deploy_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
backup_dir="$(mktemp -d /tmp/dayweave-backup.XXXXXXXX)"
trap 'rm -rf "${backup_dir}"' EXIT

: "${DAYWEAVE_BACKUP_BUCKET:?DAYWEAVE_BACKUP_BUCKET is required}"
: "${DAYWEAVE_BACKUP_RECIPIENT:?DAYWEAVE_BACKUP_RECIPIENT is required}"
: "${AWS_ENDPOINT_URL:?AWS_ENDPOINT_URL is required}"
: "${POSTGRES_DB:=dayweave}"
: "${POSTGRES_USER:=dayweave}"

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
plain_path="${backup_dir}/dayweave-${timestamp}.dump"
encrypted_path="${plain_path}.age"

docker compose --env-file "${deploy_dir}/.env" -f "${deploy_dir}/compose.yaml" \
  exec -T postgres pg_dump --username "${POSTGRES_USER}" --dbname "${POSTGRES_DB}" \
  --format custom --no-owner --no-acl > "${plain_path}"

age --recipient "${DAYWEAVE_BACKUP_RECIPIENT}" --output "${encrypted_path}" "${plain_path}"
aws --endpoint-url "${AWS_ENDPOINT_URL}" s3 cp \
  "${encrypted_path}" "s3://${DAYWEAVE_BACKUP_BUCKET}/postgres/$(basename "${encrypted_path}")" \
  --only-show-errors

echo "Uploaded encrypted backup postgres/$(basename "${encrypted_path}")"

