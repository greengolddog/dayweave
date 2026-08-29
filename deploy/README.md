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

The `lol` CLI profile can read the active tenant and its projects. The target is
the existing `eu-north1` default project. Keep IDs in local deployment state or
CI secrets rather than committed files. No paid resource is created by the
repository scripts without an explicit apply step.

## Local service

1. Copy `.env.example` to `.env` and replace every placeholder.
2. Run `docker compose -f compose.yaml -f compose.dev.yaml up --build`.
3. Check `http://127.0.0.1:8787/health` and `/ready`.

The API is only bound to loopback on the host. PostgreSQL has no published port.

## Production outline

1. Create project-scoped runtime and backup service accounts with least-privilege
   roles. Do not deploy with the human `lol` profile.
2. Create a private versioned Object Storage bucket with a seven-day lifecycle.
3. Create a Nebius tunnel and grant its dedicated service account only
   `applicationtunnel.agent` for that tunnel.
4. Create the 32 GiB VM, attach the runtime service account, and install Docker,
   the AWS CLI, and `age` through cloud-init.
5. Put this checkout at `/opt/dayweave`, create `deploy/.env`, and copy
   `tunnel/config.yaml.example` to the ignored `tunnel/config.yaml`.
6. Install the units in `systemd/`, start `dayweave.service`, then enable
   `dayweave-backup.timer`.
7. Set `DAYWEAVE_PUBLIC_BASE_URL` to the generated tunnel HTTPS URL and run the
   health, OAuth callback, WebSocket, backup, and restore drills.

The production URL format is
`https://dayweave-<tunnel-mask>.tunnel.applications.eu-north1.nebius.cloud`.
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
