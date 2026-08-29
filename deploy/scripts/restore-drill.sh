#!/usr/bin/env bash
set -euo pipefail

deploy_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
drill_dir="$(mktemp -d /tmp/dayweave-restore-drill.XXXXXXXX)"
project_name="dayweave_restore_drill_$$"
compose_file="${deploy_dir}/compose.restore-drill.yaml"

cleanup() {
  docker compose --project-name "${project_name}" --file "${compose_file}" \
    down --remove-orphans >/dev/null 2>&1 || true
  rm -rf "${drill_dir}"
}
trap cleanup EXIT

: "${DAYWEAVE_BACKUP_BUCKET:?DAYWEAVE_BACKUP_BUCKET is required}"
: "${DAYWEAVE_BACKUP_IDENTITY_FILE:?DAYWEAVE_BACKUP_IDENTITY_FILE is required}"
: "${AWS_ENDPOINT_URL:?AWS_ENDPOINT_URL is required}"

if [[ ! "${project_name}" =~ ^dayweave_restore_drill_[0-9]+$ ]]; then
  echo "Refusing to use an unexpected Compose project name" >&2
  exit 1
fi
if [[ ! -r "${DAYWEAVE_BACKUP_IDENTITY_FILE}" ]]; then
  echo "The backup identity file is not readable" >&2
  exit 1
fi

latest_key="$(aws --endpoint-url "${AWS_ENDPOINT_URL}" s3api list-objects-v2 \
  --bucket "${DAYWEAVE_BACKUP_BUCKET}" --prefix postgres/ \
  --query 'sort_by(Contents,&LastModified)[-1].Key' --output text)"

if [[ -z "${latest_key}" || "${latest_key}" == "None" ]]; then
  echo "No PostgreSQL backup objects were found" >&2
  exit 1
fi

encrypted_path="${drill_dir}/latest.dump.age"
aws --endpoint-url "${AWS_ENDPOINT_URL}" s3 cp \
  "s3://${DAYWEAVE_BACKUP_BUCKET}/${latest_key}" "${encrypted_path}" --only-show-errors

docker compose --project-name "${project_name}" --file "${compose_file}" \
  up --detach --wait

age --decrypt --identity "${DAYWEAVE_BACKUP_IDENTITY_FILE}" "${encrypted_path}" \
  | docker compose --project-name "${project_name}" --file "${compose_file}" \
      exec -T postgres pg_restore --exit-on-error --no-owner --no-acl \
        --username dayweave_restore --dbname dayweave_restore

table_count="$(docker compose --project-name "${project_name}" --file "${compose_file}" \
  exec -T postgres psql --username dayweave_restore --dbname dayweave_restore \
    --tuples-only --no-align \
    --command "SELECT count(*) FROM pg_catalog.pg_tables WHERE schemaname = 'public';")"

if [[ ! "${table_count}" =~ ^[0-9]+$ || "${table_count}" -lt 1 ]]; then
  echo "Restore completed without any public tables" >&2
  exit 1
fi

docker compose --project-name "${project_name}" --file "${compose_file}" \
  exec -T postgres psql --username dayweave_restore --dbname dayweave_restore \
    --no-psqlrc --set ON_ERROR_STOP=1 \
    --command "SELECT count(*) AS applied_migrations FROM _sqlx_migrations;" >/dev/null

echo "Restored and queried ${latest_key} in an isolated disposable PostgreSQL instance"
