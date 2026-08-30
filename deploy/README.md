# DayWeave deployment

The production target is one regular Nebius `cpu-e2` `2vcpu-8gb` VM in
`eu-north1`, using a 32 GiB Network SSD. It runs the API, worker tasks, and
self-hosted PostgreSQL with Docker Compose. Attachments and encrypted backups
use same-region Standard Object Storage. A Nebius Tunnel supplies managed HTTPS
without opening an inbound port or assigning a public IP.

The VM is intentionally single-node and not highly available. This is the only
architecture that fits the accepted personal-use budget; the documented
smallest managed PostgreSQL tier alone exceeds it.

## Verified account context

An authenticated Nebius CLI profile selected explicitly through
`DAYWEAVE_NEBIUS_PROFILE` can read the active tenant and its projects. The target
is the existing `eu-north1` default project. The profile name is not a secret,
but keep personalized names and discovered IDs in shell configuration, ignored
local deployment state, or CI configuration rather than committed files. No
paid resource is created by the repository scripts without an explicit apply
step.

## Local service

1. Copy `.env.example` to `.env` and replace every placeholder.
2. Run `docker compose -f compose.yaml -f compose.dev.yaml up --build`.
3. Check `http://127.0.0.1:8080/health` and `/ready`.

The API is only bound to loopback on the host. PostgreSQL has no published port.

## Production outline

Start with `terraform/nebius/README.md`. Its plan-only workflow discovers the
explicitly selected authenticated profile context without committing IDs,
enforces the fixed budget footprint, and its apply helper refuses to continue
unless the explicit monthly-charge phrase is provided. Direct Terraform apply
bypasses that advisory interlock and is forbidden by the deployment procedure.
The profile bootstraps scoped service accounts; the running service does not use
the human identity.

1. Create project-scoped runtime and backup service accounts with least-privilege
   roles. Do not run the service with the human administrative CLI profile.
2. Create a private versioned Object Storage bucket. The backup identity can edit
   only `postgres/*`; the runtime identity can edit only `attachments/*`. Keep
   their S3-compatible credentials separate and outside Terraform.
3. Create a Nebius tunnel and grant the runtime identity only
   `applicationtunnel.agent` for that exact tunnel.
4. Create the private 32 GiB VM. Cloud-init installs the checksum-pinned tunnel
   agent and injects the exact Terraform-created tunnel ID; the runtime identity
   never needs project-wide tunnel listing. A boot-time prerequisite disables
   SSH socket activation, restarts regular SSH, and proves every port-22 listener
   is loopback-only before exposing HTTP `localhost:8080` and SSH `localhost:22`.
5. Use the Terraform `tunnel_ssh_route` output with
   `terraform/nebius/scripts/ssh-nebius.sh`, put this checkout at `/opt/dayweave`,
   and create `deploy/.env` as a root-owned regular file with mode `0600`. The SSH
   helper performs no Nebius API call and needs no CLI profile.
6. Install the units in `systemd/`, start `dayweave.service`, then enable
   `dayweave-backup.timer`.
7. Set `DAYWEAVE_PUBLIC_BASE_URL` to the generated tunnel HTTPS URL and run the
   health, OAuth callback, WebSocket, backup, and restore drills.

The production URL format is
`https://web-<tunnel-mask>.tunnel.applications.eu-north1.nebius.cloud` and is
available directly as the Terraform `tunnel_http_url` output.
The tunnel protects transport only; DayWeave authentication remains mandatory.

## Recovery targets

- Database backups run every 15 minutes for the accepted 15-minute RPO.
- `pg_dump` is streamed directly through `age`; no plaintext archive is written
  to the VM filesystem.
- Object Storage lifecycle retains versions for seven days.
- A daily job must run `verify-latest-backup.sh` from a host that has the age
  private identity; the production VM stores only the public recipient.
- Run `scripts/restore-drill.sh` from that verification host to download the
  newest encrypted archive, restore it into an internal-network-only PostgreSQL
  container backed by disposable `tmpfs`, and query its migration history. The
  drill uses a unique Compose project and never connects to production. It must
  pass before launch and on a recurring schedule. The accepted RTO is two hours.

## Cost guardrail

Do not scale above `cpu-e2/2vcpu-8gb`, increase disk/object storage, add a public
IP, or enable another paid service until the Nebius estimate plus tax remains at
or below USD 50/month. Billing alerts should fire at 60%, 80%, and 95% of the
monthly budget.
