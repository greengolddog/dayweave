# DayWeave security model

DayWeave is a personal system, but its calendar, task, health, location, and
assistant data are sensitive. The security boundary therefore assumes that the
internet, external providers, assistant clients, and copied backups are
untrusted. It does not assume that a private GitHub repository is a secret
store.

This document describes controls that exist in the repository and calls out
the remaining production gates explicitly. Architectural intent that has not
yet shipped is not presented as protection.

## Protected assets and trust boundaries

The highest-value assets are item content and attachments, Google and assistant
credentials, API/session credentials, database and backup encryption keys, APK
and update-signing keys, and audit history. The principal boundaries are:

- the unlocked macOS or Android process;
- the HTTPS ingress and authenticated API/MCP service;
- the private API/PostgreSQL container network;
- encrypted object storage and backups; and
- Google, model providers, Codex/ChatGPT MCP clients, Maps, weather, and future
  health providers.

Every provider is granted and sent only the data needed for the requested
operation. Provider failure must leave canonical data intact and external
effects must flow through an auditable proposal or outbox boundary.

## Controls implemented now

### Devices

- macOS planner snapshots use AES-GCM. The encryption key is stored in the
  login Keychain, and snapshot replacement is atomic.
- macOS has an opt-in app-wide presentation lock backed by device-owner
  authentication (Touch ID or the Mac login password). It fails closed on cold
  start, supports immediate or configured inactivity timeouts, gates the main,
  Settings, and menu-bar scenes plus keyboard commands, and pauses Codex and
  sync presentation while locked. Its versioned preferences contain no user
  content and malformed settings fail closed.
- Android planner state is held in a SQLCipher Room database. A random database
  passphrase is wrapped by a non-exportable Android Keystore AES-GCM key.
- First-party API bearer credentials use Keychain on macOS and a
  Keystore-wrapped value on Android. They are never written into the planner
  snapshot, logs, source, or build configuration.
- Both clients fail closed when encrypted storage cannot be restored; they do
  not silently replace an unreadable real data store with sample data.
- Android backup rules exclude the encrypted database, authentication material,
  and legacy app-lock preferences during migration. The authoritative app-lock
  record is a strict versioned `AtomicFile` in `noBackupFilesDir`, which is
  outside backup and device-transfer domains.
- Android has an opt-in app-wide presentation lock backed by the system
  BiometricPrompt. It accepts an enrolled biometric or device credential,
  locks on cold start and after a configurable background timeout, replaces the
  whole planner composition while locked, and keeps `FLAG_SECURE` set whenever
  protection is enabled or changing. Enabling and disabling both require a
  fresh generation-bound device authentication. Platform authentication is
  process-wide single-flight across Activity recreation; cancellation retains
  the slot until the exact terminal callback drains, and leaving during an
  authenticated settings transition immediately restores the locked
  presentation. App-lock settings contain no user content; malformed existing
  settings fail closed and require device authentication before a durable
  repair, without deleting the encrypted planner database.

### Service and protocol

- Non-health HTTP and MCP operations require a strict bearer credential. The
  API retains only SHA-256 token digests and compares them in constant time;
  configured credentials must be high-entropy values of at least the enforced
  minimum length.
- Each authenticated principal is resolved into an explicit user/workspace
  scope. PostgreSQL access includes this scope rather than relying on the
  personal single-user deployment assumption.
- Mutation retries use hashed idempotency keys. Canonical changes, audit rows,
  and external-effect outbox rows are designed to commit transactionally.
- MCP validates protocol version, media type, request size, origin, bearer
  authentication, and request IDs. External assistants can read granted data,
  simulate, and submit expiring proposals; they cannot directly mutate
  canonical state.
- Sensitive schedule material is redacted at the projection boundary, before
  it reaches MCP serialization.
- Database URLs and storage failures are redacted in application-facing errors.
  HTTP traces carry correlation metadata without deliberately logging request
  bodies, authorization headers, titles, notes, or assistant prompts.
- Production Codex process startup remains disabled until the client uses the
  exact pinned-runtime launcher and handles every server request fail-closed.
  The verifier uses canonical restricted roots, a private verified runtime copy,
  no process fork, no inbound or bind permission, and outbound access only for
  the managed Codex service. Protocol traffic and subprocesses are bounded.
  Managed ChatGPT device-code tokens must remain inside the isolated Codex home
  and never be backed up or synchronized to DayWeave or its backend.

### Deployment

- PostgreSQL has no published host port. The API binds to host loopback and the
  intended production ingress is managed HTTPS through a Nebius Tunnel.
- The API container runs as an unprivileged user with a read-only root
  filesystem, all Linux capabilities dropped, and `no-new-privileges`.
- Backups are encrypted with an `age` public recipient before upload. The
  corresponding private identity must remain off the production VM.
- Deployment, runtime, tunnel, and backup identities are separate; the human
  `lol` profile is only a bootstrap identity and is never copied to the VM or
  repository.

### Software supply chain

The `Security` GitHub Actions workflow runs on every main-branch push, pull
request, weekly schedule, and manual invocation. It fails on:

- RustSec advisories in `Cargo.lock`;
- high or critical dependency findings in the repository;
- high or critical configuration findings;
- detected credentials or private secrets; and
- high or critical vulnerabilities or embedded secrets in the built API image.

Dependabot checks Cargo, Gradle, Swift Package Manager, Docker, and GitHub
Actions weekly, and Dependabot security updates are enabled. Findings are
updates to review, not authorization to bypass the normal build, test,
migration, or external-effect checks.

The public repository also has GitHub secret scanning and push protection
enabled. A checked-in, build-aware CodeQL workflow analyzes Actions, Rust,
Kotlin, and Swift with extended security queries. Those controls supplement the
checked-in scanner; they are not a reason to place a credential in a commit,
example file, issue, build log, or artifact. Dependency Review is not yet a
release gate. The existing scanner remains plan-independent so its core checks
do not depend on an optional hosted security product.

## Sensitive-item disclosure policy

`sensitive` is a data-flow policy, not just a visual badge. Unless the user has
granted a specific disclosure, sensitive values must be omitted from lock-screen
notifications, widgets while locked, Spotlight or Android indexing, MCP and
proactive assistant context, diagnostic output, attachment processing, and
exports. Busy/free projections may preserve time occupancy while removing the
title, notes, item ID, and type.

Tests for every new disclosure surface should use a recognizable canary title
and notes value and assert that neither appears in the forbidden output.

## External-effect and assistant policy

Local, reversible, single-item changes may be applied with undo. Bulk,
schedule-wide, destructive, deadline-relaxing, attendee-affecting, or other
provider mutations require preview and explicit approval. MCP and external chat
integrations are proposal-only. AI may interpret intent and explain solver
evidence, but it cannot weaken authentication, authorization, hard constraints,
or the confirmation policy.

## Secrets and key handling

Production values belong in device key stores, Nebius secret/identity services,
or protected CI secrets. They must not appear in `.env.example`, Git history,
issue text, screenshots, crash reports, or command output committed to the
repository. Required rotations are:

1. revoke the affected credential at its issuer;
2. issue a separate replacement rather than editing the leaked value in place;
3. update the authorized runtime or client store;
4. verify old-credential rejection and new-credential success; and
5. retain a content-free audit record of issuer, credential class, time, and
   reason.

Rewriting Git history does not make an exposed credential safe and is never a
substitute for revocation.

## Incident response

For suspected compromise, stop new external effects first, preserve logs and
audit identifiers without copying user content, revoke the narrowest affected
credentials, and isolate the service if scope is unknown. Restore only from a
verified encrypted backup into an isolated database, validate migrations and
row counts, rotate credentials and encryption wrapping keys as appropriate,
then re-enable integrations one at a time. Notify the user in-app and by the
configured private email channel with timestamps, affected capability, actions
taken, and any remaining decision.

## Production gates still open

The following controls are required before DayWeave is treated as production
ready:

- replace bootstrap static API credentials with revocable, expiring device
  sessions and scoped MCP client credentials;
- complete SEC-007 sensitive-item enforcement for lock-screen notifications,
  widgets while locked, external MCP access, proactive assistant context, and
  attachment analysis on both clients;
- add server-side envelope encryption for provider tokens and especially
  sensitive fields with a key held separately from PostgreSQL and backups;
- enforce ingress and per-principal rate limits and suspicious-authentication
  alerts;
- provision least-privilege Nebius identities, private versioned storage,
  tunnel HTTPS, budget alerts, and automated security patch/restart reporting;
- rehearse backup restore and credential/key rotation against the deployed
  topology;
- generate stable signing keys outside Git and verify signed macOS and Android
  update metadata; and
- complete authn/authz, sensitive-canary, input-fuzzing, rate-limit, backup,
  dependency, and container security tests in the release gate.

Until those gates pass, builds are development artifacts even when functional
tests and the security scanners are green.
