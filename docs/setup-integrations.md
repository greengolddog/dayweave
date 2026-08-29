# DayWeave integration setup

This document separates work Codex can perform automatically from browser
consent that only the owner can complete. Never paste credentials into chat or
commit them to this repository.

## Local prerequisites

- Treat every tracked file and build artifact as public: use synthetic fixtures
  only, and keep tokens, account exports, signing files, health records, CLI
  profile names, tenant inventory, and project identifiers out of the
  repository.
- Authenticate GitHub and Nebius CLIs locally with human profiles that are never
  copied into application configuration, CI, deployment manifests, or docs.
- Codex CLI 0.150.1 is the pinned App Server runtime. The macOS packaging step
  verifies its exact binary and generated schemas, preserves its Developer ID
  signature, and seals a private copy into the DayWeave app bundle. The app
  accepts managed ChatGPT device-code authentication only.
- Android packaging requires a local JDK/SDK; device validation additionally
  requires an ADB-visible device. Widgets, extensions, UI automation, and
  release entitlements require a selected full Xcode toolchain.

## Google Cloud project

Create one dedicated Google Cloud project for DayWeave rather than reusing an
unrelated project.

1. Enable Google Calendar API, Google Tasks API, and the Maps APIs selected by
   the travel implementation.
2. Configure the OAuth consent screen for the owner's account and add the owner
   as a test user until the app is ready to move to production status.
3. Create a Web application OAuth client for the private backend. Register the
   exact HTTPS callback under the final Nebius Tunnel origin; redirects must
   match exactly. The server rejects cleartext callback URLs in every
   environment, including loopback development and tests; local OAuth work must
   terminate TLS and register that exact HTTPS URI.
4. Store the client ID and client secret in Nebius MysteryBox or the VM's
   root-readable environment file, never in GitHub or client binaries.
   Configure `DAYWEAVE_GOOGLE_CREDENTIAL_KEYS` as the versioned server keyring,
   `DAYWEAVE_GOOGLE_ACTIVE_CREDENTIAL_KEY_VERSION` as the key used for new
   encrypted envelopes, and `DAYWEAVE_GOOGLE_IDENTITY_KEY_VERSION` as a
   separately pinned identity root. The identity version must remain configured
   and byte-identical for the lifetime of every published Calendar item; rotate
   only the active encryption version. The first outbound-enabled startup stores
   a scope-bound, domain-separated one-way verifier—not key material—in
   PostgreSQL. Later outbound-enabled startups must match both its version and
   verifier or initialization fails closed. Losing or changing the identity
   root can make crash recovery miss an existing provider event and is therefore
   a restore-blocking incident, not routine key retirement.
5. Request offline access and incremental authorization. The backend owns the
   encrypted refresh token; macOS and Android receive only a DayWeave session.
6. Start with Calendar read access during import/onboarding. Request write
   access when the owner enables the dedicated DayWeave calendar, and request
   Tasks write access only when Google Tasks sync is enabled.

The complete product needs these effective capabilities:

- read calendar lists, settings, event series, exceptions, attendees,
  conference data, free/busy state, event types, and incremental sync tokens;
- create the dedicated DayWeave calendar and manage DayWeave-owned events;
- update or delete other writable events only through the product's explicit
  preview/approval rules;
- read and write the selected Google Tasks lists.

Google documents `https://www.googleapis.com/auth/calendar` for complete
calendar editing and `https://www.googleapis.com/auth/tasks` for complete Tasks
sync. DayWeave keeps authorization incremental: an OAuth start with no
`services` requests `calendar_read_only` and `tasks_read_only`; request
`calendar` or `tasks` later against the existing `account_id` only when the
owner selects a writable collection. A broader scope replaces its narrower
equivalent in the request instead of asking Google for both.

### Google sync operator flow

All routes below require the normal DayWeave bearer token. Provider access and
refresh tokens never leave the server and must never appear in logs or API
responses.

1. Complete `/v1/integrations/google/oauth/start` and its browser callback.
   Start read-only unless a write feature is already enabled.
2. `POST /v1/integrations/google/accounts/{account_id}/collections/discover`.
   This exhausts provider pagination before atomically replacing the durable
   collection inventory. A failed partial discovery does not mark unseen lists
   as deleted.
3. Read the inventory with `GET .../collections`, then configure each source
   with `PUT .../collections/{collection_id}` and this body:

   ```json
   {
     "expected_revision": 1,
     "selected": true,
     "visible": true,
     "sync_role": "blocking"
   }
   ```

   `read_only` imports reference data without creating scheduling constraints;
   `blocking` imports busy Calendar records as fixed constraints; `writable`
   additionally permits guarded DayWeave-owned writes. Task lists support
   `read_only` and `writable`, not `blocking`. Calendar `writable` also requires
   Google `owner` or `writer` access and the broad Calendar scope. Changing
   visibility or role invalidates that collection's incremental cursor and
   requests a complete paginated replay. Collection revisions fence in-flight workers,
   so a worker using the prior redaction or blocking policy cannot commit items
   or a cursor after the configuration change.
   If Google later downgrades a Calendar from `owner`/`writer`, discovery
   atomically downgrades DayWeave's role to `read_only`, invalidates its cursor,
   and conflicts any unpublished outbound work for that collection, including a
   delivery claim that was already in progress.
4. `POST .../sync/refresh` durably requests a run and returns `202`; periodic
   reconciliation also runs every 15 minutes. `GET .../sync` reports the run,
   retry time, stable redacted run/outbound error codes, import conflicts, and
   outbound queue counts. Rate limiting and transient provider failures enter
   bounded backoff. A manual refresh advances backoff work for an explicit
   retry; after reauthorization, call it to resume retained outbound work.
   Invalid authorization and terminal durable failures make `/ready` false.

Calendar reconciliation deliberately uses two provider views. The unbounded
`singleEvents=false` lane owns Google's incremental sync token and retains only
series/version metadata; a live recurrence master is never mistaken for either
one fixed event or a deletion. After that lane is complete, a second
`singleEvents=true` scan expands the bounded planning window from 30 days before
the run through 120 days after it. DayWeave fetches every page before one
transaction replaces the occurrence generation. The bounded lane permits at
most 100 pages and 10,000 occurrences; duplicate IDs, token cycles, malformed
bounds/timezones, a recurrence master, any rejected occurrence, or an incomplete
page sequence leaves planning coverage failed instead of installing a partial
calendar.

Expanded exceptions retain their series ID in the restricted mapping store;
their original provider start remains covered by the restricted payload hash
and is never copied into canonical scheduling data. Canonical blocking occurrences contain exactly an
ID-free `calendar_event` time constraint; retained nonblocking occurrences
contain exactly an ID-free `calendar_context` constraint and do not consume
planner capacity. Both are non-recurring canonical event instances. All-day
bounds keep Google's exclusive end and are converted using the event/calendar
IANA timezone, including DST days. Birthdays and working-location events are
always context-only. Self-declined invitations are removed; another attendee's
decline does not hide the event. Out-of-office, focus-time, ordinary opaque,
tentative, transparent, and all-day behavior follows the configured collection
policy.

An invisible source or a provider-private/confidential occurrence is stored as
a sensitive `Busy` interval without title, notes, location, attendee counts,
conference/attachment flags, event type, or provider identifiers. That
provider-imposed sensitivity is monotonic while the mapping remains active.
Tasks continue to import completed, hidden, deleted, due, parent, and ordering
metadata. Provider cursors are AES-GCM sealed with account/collection AAD and
advance only after the source scan and complete occurrence generation commit.
Calendar 410 recovery, and every cursorless Calendar or Tasks run after
configuration invalidates a cursor, performs a complete paginated provider
scan. The unbounded source run fails before advancing its cursor if the
documented 100,000-item safety cap is exceeded. After a complete scan, mappings
absent from the snapshot are reconciled: unchanged external items move to
recoverable trash, local edits conflict, and DayWeave-owned records conflict
rather than being silently detached from their provider identity.

Each Calendar refresh invalidates its prior planning coverage before discovery
or provider I/O, so network/protocol failure cannot leave the old generation
trusted indefinitely. Deselecting or provider-deleting a calendar, downgrading
its blocking role/policy, pausing the Google account, or starting disconnect
retires its active projected occurrences to recoverable trash in the same
canonical transaction while retaining restricted mapping identity. Resume or a
later policy upgrade must complete a fresh expanded generation before the
scheduler uses that source again. A database fallback also refuses scheduling
if an active blocking occurrence survives any missed teardown transition.

Teardown never trashes an occurrence whose canonical revision changed after its
last import. That item remains an unchanged local fork, while the historical
provider mapping is retired as a local-only conflict so it cannot keep the
Calendar safety fence active. A future provider occurrence receives a fresh
active mapping and cannot overwrite the fork. Ordinary absent occurrences are
retired once; later complete generations leave their dormant mappings untouched
until the occurrence reappears, keeping refresh work bounded by the current
window instead of all retained Calendar history.

Provider changes never silently overwrite a locally edited canonical item. The
mapping records the last imported local revision: a provider update or deletion
applies only while that revision still matches. Otherwise it becomes an
aggregate operator-visible conflict. `GET .../sync` reports conflict counts and
the latest redacted outbox code, but the current API does not yet enumerate the
specific mapping/outbox IDs or conflict metadata; item-level diagnosis currently
requires restricted database/operator tooling, and a conflict-detail/retry API
remains an explicit client-workflow gap. Provider deletion moves an unchanged item to
recoverable trash; if Google restores that same record before a local edit, the
next import restores the mapped canonical item as well. A move between Calendar
or Tasks collections is represented as deletion in the old collection plus
import in the new collection, preserving any conflicting local edit rather than
guessing identity across sources.

External publication remains deployment-disabled by default. Set
`DAYWEAVE_GOOGLE_OUTBOUND_ENABLED=true` only after OAuth, PostgreSQL, storage
encryption, backups, and operator monitoring are ready. Approval lifetimes use
`DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES` (default 10, accepted range
1–30). The complete mutation flow is intentionally three-step:

1. `POST .../outbound/previews` validates the canonical revision, selected
   writable collection, provider account, exact full write scope, publication
   policy, ownership, remote resource ID, and retained ETag. It returns the
   redacted provider payload, provider target/version, expiry, and review hash.
2. `POST .../outbound/previews/{preview_id}/approve` must echo that hash. The
   server records an audit operation and returns one OS-CSPRNG capability. Only
   its SHA-256 hash is stored; request/response types carrying it have no debug
   representation.
3. `POST .../outbound` presents the capability with the exact account,
   collection, item revision, and operation. Consumption is atomic. An exact
   retry before expiry returns the same outbox ID; swaps or mutations fail and
   do not consume authority. Expired capabilities are rejected, including
   retries.

The reviewed intent binds workspace user, account, collection ID and revision,
remote collection ID, collection kind, full write scope, canonical item and
revision, operation, provider resource ID and ETag, and payload. Immediately
before each Google mutation, OAuth token acquisition/refresh and complete HTTP
request construction finish without sending. A short database statement then
revalidates all reviewed values plus the exact unexpired parent sync-run claim
and its monotonic generation, account status, scope, collection policy/role,
current item revision, ownership mapping, and ETag. It records a nonce whose
30-second lifetime is only a provider-write initiation deadline; the prepared
transport checks it at the last local instruction before network I/O. A response
that finishes later may commit only while the same nonce, child claim, parent
claim/generation, and every guardian still match.
No database transaction or row lock is held across provider network I/O. The
post-response transaction requires the nonce and repeats the guardians; a
concurrent pause, revocation, configuration change, item edit, or provider
mapping change is surfaced as conflict/superseded work rather than silently
committed. Parent-run takeover atomically cancels child claims and nonces; the
schema's required run-claim columns also reject unsafe mixed-version workers
that predate this fence.

Calendar writes require a canonical `dayweave_firm_block`. New events use an
account/calendar/user-bound non-reversible event ID and an AES-GCM authenticated
private ownership proof; raw DayWeave UUIDs are never published. A lost create
response is recovered only when the event at that deterministic ID still
matches the complete reviewed semantics and proof. Any provider-side edit is a
conflict, not an overwrite. Google Tasks exposes no private marker or
client-selected ID, so DayWeave neither publishes nor trusts note markers and
strips its legacy visible marker on import and export. A task create is attempted
only once after the final transaction records the explicit
`provider_post_may_have_started` marker. Pre-token, preparation, policy, and
expired-initiation failures do not consume that attempt. A crash, transport
failure, provider 5xx, lost response, malformed or otherwise unusable 2xx body,
missing/invalid provider ID or ETag, invalid provider update timestamp, or
unexpected response variant after that marker therefore becomes durable
`provider_identity_unresolved` evidence. The database's send-start marker is
authoritative even if a caller reports a generic protocol error. Recovery
requires operator reconciliation, never title matching or a blind second POST.
The unresolved identity fence follows the same item and target across later
canonical revisions; a revision change alone cannot authorize another create.

Every edit and delete sends the retained ETag with `If-Match`; missing versions
fail before network access, 412 and 404 responses become durable conflicts, and
deletion is refused until the canonical item is in recoverable trash. Rate
limits and temporary provider failures use bounded retry/backoff; non-idempotent
Tasks creates are the deliberate fail-closed exception. Older unpublished item
revisions are durably marked `superseded`.

Calendar planning policy is stored per collection. Safe defaults block only
confirmed opaque busy events; tentative, transparent/free, birthdays, and
all-day events remain visible but nonblocking. Out-of-office and focus-time
events block by default. Each category can instead be ignored, retained as
nonblocking context, or blocking. Publishing all-day, tentative, and free blocks
is independently opt-in. All-day bounds use Google-exclusive local dates and
the configured IANA time zone, so 23- and 25-hour DST days retain the correct
calendar dates.

Provider cursors are encrypted, but durable preview/outbox JSON contains the
selected DayWeave item's title and notes in PostgreSQL plaintext. Production
deployment therefore still depends on restricted database access and the
documented database/storage encryption gate; these payloads are sensitive.

Two canonical-model gaps remain explicit. Imported attendee identity,
conference, and attachment entities have no first-class canonical tables, so
the occurrence projection does not claim round-trip fidelity for those values.
Google Tasks parent/order values are retained as provider metadata but are not
silently projected into the canonical hierarchy until a conflict-aware
hierarchy reconciliation primitive exists. Google Calendar fixed events are no
longer such a gap: a complete generation is consumed automatically by schedule
preview and publication, and clients must not submit duplicate
`google_calendar` fixed blocks when that authoritative projection is active.

Official references:

- <https://developers.google.com/workspace/calendar/api/auth>
- <https://developers.google.com/workspace/calendar/api/guides/create-events>
- <https://developers.google.com/workspace/tasks/auth>
- <https://developers.google.com/identity/protocols/oauth2/web-server>

## Android Health Connect

The Android client uses the current stable
`androidx.health.connect:connect-client:1.1.0`. Health Connect requires a mobile
device on Android 9/API 28 or newer with Google Play services. It is a system
component on Android 14 and newer; Android 13 and older use the Health Connect
Play Store app. Work-profile contexts are unsupported by Health Connect.

The first CTX-006 slice is deliberately read-only and foreground-only:

1. Open **More → Health & context → Health Connect** and turn sync on.
2. DayWeave checks `HealthConnectClient.getSdkStatus`. An unsupported device
   remains manual-only; an absent or old provider offers the official Play Store
   install/update route.
3. The Health Connect activity-result contract requests only
   `android.permission.health.READ_SLEEP`. DayWeave requests no write,
   background-read, history-read, heart-rate, account, or Google identity scope.
4. With access granted, DayWeave reads the aggregate sleep duration for the last
   24 hours while the UI is foregrounded. It converts that aggregate into only a
   Low/Medium/Deep energy band, a broad recovery band, and a calculation time.
   Raw records, record IDs, session bounds, stages, titles, and notes do not cross
   the provider boundary and are never persisted, logged, uploaded, or placed in
   test fixtures.
5. **Manage access** opens Health Connect settings. Turning sync off, revoking or
   denying access, provider unavailability, and read failure all remove the
   automatic estimate. Manual energy check-in and correction continue to work,
   and none of these conditions blocks capture, viewing, execution, or planning.

The derived bands and manual check-in live only in the existing encrypted local
planner snapshot. The application manifest disables Android backup entirely;
the backup exclusion rules remain defense in depth. The Today screen uses the
current non-stale band only to show a non-mutating best-fit hint against existing
task energy demands. The server composition contract does not yet accept a
current-energy signal, so Health Connect never silently changes or recomposes a
schedule in this slice.

The exported Health Connect rationale activity contains the on-device privacy
explanation. Before Play distribution, copy that exact data-use statement into
the Health apps declaration/privacy policy and obtain approval; no Play Console
credential or declaration artifact belongs in Git.

JVM tests use the deterministic synthetic provider. Final acceptance still has
a physical-device gate: on the target Pixel, verify unavailable/update/available
states, grant/deny/revoke, the rationale and Manage access entry points, and one
synthetic sleep aggregate without using or recording the owner's real health
data. Then rerun `./gradlew connectedDebugAndroidTest`.

Official references:

- <https://developer.android.com/health-and-fitness/health-connect/availability>
- <https://developer.android.com/health-and-fitness/health-connect/get-started>
- <https://developer.android.com/health-and-fitness/health-connect/ui/permissions>
- <https://developer.android.com/jetpack/androidx/releases/health-connect>

## Codex inside the macOS app

The macOS client launches only its sealed Codex 0.150.1 runtime as
`codex app-server --stdio`. Before launch it verifies the pinned executable,
manifest, generated schemas, and Developer ID requirement, then runs a private
copy inside an outbound-only, deny-by-default Seatbelt profile. The App Server
wire contract remains version-specific. After the `initialize` handshake,
DayWeave calls `account/read`; managed ChatGPT device-code login
(`chatgptDeviceCode`) is the only accepted authentication mode. Browser callback
and API-key fallback login are intentionally unavailable.

Codex owns managed OAuth tokens and refreshes them inside a private, device-local
`CODEX_HOME`. DayWeave does not extract, back up, or sync those tokens to its
backend. The Assistant starts an ephemeral App Server thread, sends turns, streams
validated agent-message events, and supports `turn/interrupt`. Each turn carries
an explicit, bounded planner snapshot that omits notes, credentials, storage
paths, raw constraints, revisions, and stable planner identifiers. The runtime
has no direct access to app storage.

Conversation history currently lasts for the running app session only; the UI
does not yet expose model or reasoning controls. All server-initiated tool,
command, file-change, and approval requests are denied. A reply may include a
strict bounded change-proposal envelope, but DayWeave can only route valid entries
to the local Suggestions Inbox through an app-owned router. The conversation
controller never mutates `PlannerStore` directly, and accepting a proposal
currently records the review decision without changing the schedule.
Chat requires a signed-in ChatGPT account and network access; there is no offline
model fallback.

For development attestation, run `./scripts/verify-codex-runtime.sh` directly.
Do not invoke it as `bash scripts/verify-codex-runtime.sh`: privileged Bash
startup and the verifier's clean-environment re-exec are part of the check.
`./scripts/build-macos-app.sh` repeats the verifier, seals the runtime, and checks
the final local app signature. Notarized distribution still requires the owner's
Apple Developer signing setup.

Official reference: <https://learn.chatgpt.com/docs/app-server>.

## DayWeave MCP and ChatGPT/Codex plugin

The private MCP endpoint uses Streamable HTTP. The checked-in development plugin
targets loopback and reads a separately scoped MCP bearer credential only from
the `DAYWEAVE_MCP_TOKEN` process environment. Never put its value in the repo,
the plugin manifest, an `.env` file, shell history, or chat. That bearer path is
for local Codex/ChatGPT desktop development only; it is not ChatGPT web account
linking.

Published ChatGPT/Codex clients instead use the server's separate Auth0-backed
OAuth 2.1 resource-server boundary. It is disabled by default and handles no
Auth0 client secret or token issuance. When enabled through the reviewed Compose
overlay, it publishes both protected-resource metadata paths and exact OAuth
challenges, advertises one required OAuth scope per tool, and accepts only the
configured owner, client, issuer, and exact `/mcp` audience. Follow
[`mcp-oauth.md`](mcp-oauth.md) for the fixed token/JWKS contract, stable ChatGPT
CIMD registration, guarded DCR alternative, configuration, preflight, and
rollback.

The deployed endpoint will use the Nebius Tunnel HTTPS URL.
Its external contract is intentionally asymmetric:

- clients may read only the schedule detail permitted for that client;
- clients may simulate plans;
- `submit_proposal` may add a reviewable Suggestions Inbox proposal;
- no external conversation can mutate canonical schedule/calendar/task state.

ChatGPT web does not accept a custom DayWeave API key. Remote activation still
requires the stable Nebius Tunnel HTTPS URL, the complete Auth0 preflight, and
an independent deployed audit of the wired PostgreSQL MCP schedule-query,
redaction, simulation, and atomic proposal-submission flows. OAuth account
linking alone must not be presented as live schedule access. After those gates
pass:

1. activate the explicit OAuth Compose overlay and run every preflight in
   [`mcp-oauth.md`](mcp-oauth.md);
2. enable ChatGPT Developer mode;
3. register the exact public `/mcp` HTTPS URL and complete its Auth0 connection;
4. copy the resulting `plugin_asdk_app…` technical ID;
5. add that ID to the plugin's `.app.json` and manifest using the plugin creator;
6. validate, install from the repository source, and test in a new chat.

The repo plugin already contains its validated skill and local `.mcp.json`.
Do not perform remote registration or claim schedule access before the activation
gates above pass.

Official references:

- <https://learn.chatgpt.com/docs/extend/mcp>
- <https://developers.openai.com/plugins/build/auth>
- <https://developers.openai.com/plugins/build/plugins>

## Nebius access and deployment identity

A locally authenticated human profile can bootstrap resources; it must not be
copied to the VM or CI. Deployment creates separate least-privilege identities:

- a runtime service account attached to the VM;
- a tunnel-agent account with only `applicationtunnel.agent` on its tunnel;
- a backup account restricted to the private Object Storage bucket;
- an optional CI deployment account restricted to updating DayWeave resources.

The cost-conscious starting recommendation is `cpu-e2/2vcpu-8gb` with a 32 GiB
Network SSD in the owner's chosen project region. One VM runs
API/worker/PostgreSQL. Resolve tenant, project, subnet, and region from the local
CLI profile at deployment time; never record them or a credential file here.

Nebius Tunnel provides the generated DNS name and TLS while accepting only an
outbound agent connection. It does not replace application authentication.

Official references:

- <https://docs.nebius.com/tunnels/overview>
- <https://docs.nebius.com/tunnels/quickstart>
- <https://docs.nebius.com/compute/virtual-machines/types>
