# Nebius infrastructure

This module describes the accepted personal-use production footprint without
creating it implicitly:

- one private `cpu-e2` / `2vcpu-8gb` VM;
- one deletion-protected 32 GiB Network SSD with Ubuntu 24.04 LTS;
- one private, versioned Standard bucket capped at 10 GiB;
- seven-day expiry for the encrypted `postgres/` backup prefix and separately
  scoped `attachments/*` object access;
- one Nebius Tunnel (free during preview); and
- separate runtime and backup identities with resource-scoped access.

The VM has no public IP. Terraform state and cloud-init contain no private key,
API token, database password, Google credential, Codex credential, or Object
Storage secret. The tunnel agent uses short-lived VM service-account metadata
tokens, so it needs no copied tunnel credential.

## Plan without spending

Install Terraform 1.10 or later, `jq`, OpenSSL, OpenSSH (`ssh-keygen`), and the
Nebius CLI. Ensure the `lol` profile is authenticated and has the intended
eu-north1 project selected. Set `DAYWEAVE_SSH_PUBLIC_KEY_FILE` if the
administrative public key is not at `~/.ssh/id_ed25519.pub`, then run:

```sh
./scripts/plan-nebius.sh
```

The script verifies the selected project is active in `eu-north1`, discovers its
tenant and one unambiguous subnet (preferring the unique subnet named `default`),
and writes them only to ignored `local.auto.tfvars.json`. It then validates the
module and creates an unapplied binary plan, its exact JSON rendering, and an
ignored cryptographic review receipt. `verify-plan.sh` permits only the 13
expected create actions and rejects public or secondary disks, a public IP, a
larger VM or bucket, widened bucket paths, weaker backup retention, a broader
tunnel role, wrong project/tenant/subnet wiring, or IAM that is not ready before
VM creation.
It also rejects child modules, extra providers, data resources, provisioners,
unexpected outputs, and cloud-init that differs from the reviewed template and
public inputs. The guard also pins the reviewed resource, provider, variable,
output, and provider-lock sources by SHA-256, so removing `prevent_destroy` or
adding an ignored lifecycle change cannot hide behind plan JSON. Intentional
changes to those files require a new review and digest update. `estimate-cost.sh`
builds the calculator request from the same binary-plan-derived VM and disk
values and fails closed above 42 monthly billing units, reserving at least eight
units of headroom below the accepted USD 50 ceiling. Because the new standalone
disk has no ID until creation, the estimate models that exact disk once as the
instance's managed boot disk; sending both forms would double-count it.

Cloud-init downloads the Linux x86_64 Nebius Tunnel agent 1.0.0 from its fixed
release URL, limits the response and process resources, and verifies its pinned
SHA-256 digest before installation. Terraform injects the exact ID of the tunnel
it creates into the VM user data. The sandboxed agent uses short-lived metadata
authentication only for that ID, so the runtime identity has no broad tunnel-list
permission and stores neither a token nor a private key. It announces `web` at
`localhost:8080` plus `ssh` at `localhost:22`; its systemd cgroup caps memory,
swap, and task count.

A required host-security unit runs on every boot before the tunnel. It disables
and masks Ubuntu SSH socket activation, validates the sshd configuration,
restarts regular SSH, and requires at least one port-22 listener with every such
listener bound to loopback. The tunnel also repeats the listener assertion on
every agent start. Both application and SSH listeners are therefore loopback-only,
so private-subnet peers cannot bypass the tunnel.

Discovery requires the exact subnet, network, and assigned default route table
to be ready in the target project, and requires that table to contain exactly
one default-egress route. The subnet response exposes no separate region field,
so its region is established by its exact parentage under the independently
verified active `eu-north1` project rather than by guessing an undocumented
field. The read-only APIs cannot preflight DNS resolution, metadata reachability
from the future guest, remote TLS endpoints, or a routing change after planning.
Blocking any of those paths makes bounded bootstrap fail closed; it is not a
reason to assign the VM a public IP.

Planning performs read-only Nebius API calls. It does not create a paid
resource. Both the constraint and checked-in lock file pin the exact Nebius
provider schema reviewed for this module, and initialization refuses to rewrite
that lock file.

## Apply only after review

Read `dayweave.tfplan` with `terraform show` and review the checked-in sources
whose digests appear in `dayweave.tfplan.review.json`. Approve the exact binary
digest printed by the planning command:

```sh
DAYWEAVE_NEBIUS_APPROVAL=I_REVIEWED_THE_EXACT_PLAN_SHA256 \
  ./scripts/approve-nebius.sh <the-printed-64-character-plan-sha256>
```

The approval file is ignored, mode `0600`, bound to the binary plan, its exact
JSON, context and SSH-key digests, reviewed sources, provider lock, and a random
nonce. Apply consumes it before doing anything else, including on a failed safety
check, and records the nonce in a private ignored replay ledger, so restoring a
copied approval cannot reuse it. Another attempt requires another explicit
approval. Direct
`terraform apply` can bypass this procedural interlock and is forbidden.

Immediately before apply, the helper privately snapshots the approved artifacts,
renders the binary again and byte-compares its JSON, verifies the 13 expected
creates, checks every source digest, freshly rediscovers the `lol` tenant/project/
subnet and SSH key, and requires exact semantic and cryptographic equality. It
reruns the bounded live estimate and then passes that same private binary snapshot
to Terraform. Plans older than one hour are rejected:

```sh
DAYWEAVE_NEBIUS_APPLY=I_ACCEPT_CHARGES_UP_TO_USD_50_PER_MONTH \
  ./scripts/apply-nebius.sh
```

Never set that variable in shell startup files or CI. The apply script reruns
the live estimate, but it is not a price guarantee: Nebius labels the endpoint
`v1alpha1`, its response has no currency field, and it excludes bucket
operations, egress, taxes, and future tunnel pricing. Confirm those items and
the tunnel preview status before every apply, and configure billing alerts.

This is deliberately a bootstrap-only gate: after the first creation, a
maintenance plan will fail the create-only assertion. Add narrowly reviewed
update rules for a specific maintenance change; never weaken the bootstrap
guard into a generic allow-list, and never permit deletion or replacement of
the protected disk through this path.

## Connect and install post-provisioning secrets

The tunnel agent starts during cloud-init, before the application is installed.
After apply, read the local Terraform outputs and connect to its TCP route:

```sh
route="$(terraform output -raw tunnel_ssh_route)"
user="$(terraform output -raw ssh_user)"
./scripts/ssh-nebius.sh "$route" "$user"
```

The helper validates the route and invokes `nebius tunnel connect --stdio` as an
SSH `ProxyCommand`, with browser authentication and update checks disabled. That
connect operation makes no Nebius API call and requires no profile or cloud
credential. Normal SSH host-key checking remains enabled.

After apply, create an S3-compatible access key for the output
`backup_service_account_id` and put it only in the VM's protected environment
file. Install the application checkout and `deploy/.env` through the SSH tunnel.
Install that environment as a non-symlink, single-link `root:root` file with mode
`0600`; `dayweave.service` refuses to start otherwise, before Docker Compose can
read it.
These values are intentionally excluded from Terraform because they would
otherwise be retained in state or instance metadata. The runtime identity has
only the exact tunnel-agent permit and `attachments/*` object access; the backup
identity has only `postgres/*` object access.

If attachment persistence is enabled, create a different S3-compatible key for
`runtime_service_account_id` and expose it only to the application process. Do
not reuse the backup key or place either secret in Terraform, cloud-init, or the
tunnel-agent service.

The boot disk has both Nebius deletion protection and Terraform
`prevent_destroy`, and the backup bucket also has Terraform `prevent_destroy`.
Removing the VM preserves its boot disk. Recovery or final deletion requires an
explicit, separately reviewed change to those safeguards.

Official references:

- <https://docs.nebius.com/terraform-provider/install>
- <https://docs.nebius.com/terraform-provider/manage/migrate>
- <https://docs.nebius.com/terraform-provider/authentication>
- <https://docs.nebius.com/compute/virtual-machines/manage>
- <https://docs.nebius.com/tunnels/quickstart>
- <https://docs.nebius.com/iam/authorization/roles>
