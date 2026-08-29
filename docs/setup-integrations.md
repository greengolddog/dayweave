# DayWeave integration setup

This document separates work Codex can perform automatically from browser
consent that only the owner can complete. Never paste credentials into chat or
commit them to this repository.

## Current local access

- GitHub CLI is authenticated as `greengolddog` and the repository is public.
  Treat every tracked file and build artifact as public: use synthetic fixtures
  only, and keep tokens, account exports, signing files, and health records out
  of the repository.
- Nebius CLI profile `lol` is active and can read the owner's non-suspended
  tenant and its `eu-north1` project. The target project currently has a default
  subnet and no VM or Object Storage bucket, so deployment can start cleanly.
- Codex CLI 0.150.1 is the pinned App Server runtime. The macOS packaging step
  verifies its exact binary and generated schemas, preserves its Developer ID
  signature, and seals a private copy into the DayWeave app bundle. The app
  accepts managed ChatGPT device-code authentication only.
- No Android device is currently visible through ADB.
- The Command Line Tools build the Swift package, but full Xcode is not selected;
  widgets, extensions, UI automation, and release entitlements therefore remain
  an explicit local-toolchain gate.

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

Calendar pages include tombstones and whole recurrence series. Exceptions retain
their series ID and original start. All-day bounds keep Google's exclusive end
and are converted using the event/calendar IANA timezone, including DST days.
Birthdays, working-location events, and transparent events never block.
Self-declined invitations are ignored; if an already imported invitation becomes
self-declined, reconciliation moves that mapped item to recoverable trash, while
another attendee's decline does not hide it. Out-of-office and ordinary opaque
events block only for `blocking` or `writable` sources. An invisible source
redacts the user-facing title, notes, and location while retaining time bounds
and bounded structural sync metadata such as recurrence, response/count flags,
event type, and conference/attachment presence. Tasks import completed, hidden,
deleted, due, parent, and ordering metadata. Provider cursors are AES-GCM sealed
with account/collection AAD and advance only after every page and canonical
mutation commits. Calendar 410 recovery, and every cursorless Calendar or Tasks
run after configuration invalidates a cursor, performs a complete paginated
provider scan. The run fails before advancing its cursor if the documented
100,000-item safety cap is exceeded. After a complete scan, mappings absent from
the snapshot are reconciled: unchanged external items move to recoverable trash,
local edits conflict, and DayWeave-owned records conflict rather than being
silently detached from their provider identity.

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

The outbound foundation exposes `POST .../outbound` with `collection_id`,
`item_id`, `expected_item_revision`, and `operation` (`upsert` or `delete`), but
the service currently fails closed with `external publication requires a
server-minted approval` after validating the candidate. Bearer authentication
plus a revision is not treated as human confirmation. Enabling this path requires
a server-minted, expiring approval bound to the exact preview, item revision,
provider target, and payload, with a durable audit record. Two additional
prerequisites are mandatory before removing either unconditional enqueue or
delivery gate: every ownership marker must be authenticated and ambiguity must
be resolved atomically across the complete provider collection (or marker-based
crash recovery must remain disabled), and the worker must lock and revalidate
the stored write scope immediately before every provider mutation. An earlier
service snapshot and the post-provider completion guardian are not substitutes
for that final scope fence. These gates apply to all external Calendar and Tasks
publication, including DayWeave-owned records; they are not limited to attendee
edits.

The dormant delivery machinery accepts only selected writable collections and
DayWeave-owned mappings. Calendar insertion also requires canonical
`dayweave_firm_block` ownership and increasing RFC 3339 `starts_at`/`ends_at`
bounds; deterministic Google event IDs provide useful retry correlation. The
Calendar UUID extended-property marker and the visible Tasks
`[DayWeave item:…]` note marker are correlation values only, not authenticated
proof of DayWeave ownership. A copied or forged marker can match a current
durable intent, and the current recovery code must therefore not be activated
for outbound publication. While publication remains disabled, malformed,
unrecognized, repeated, or conflicting markers become import conflicts and do
not create a second canonical item. A production outbound implementation must
use authenticated ownership evidence and reject multiple matching provider
resources before adopting any identity. Because Google Tasks has no
client-chosen task ID, the dormant implementation performs at most one insert
attempt per durable revision. If a failed attempt cannot be matched by marker,
it reports
`provider_identity_unresolved` instead of risking a duplicate. Inspect the
selected Google list, then revise and enqueue the canonical item only after the
operator has established that no accepted task remains. The implementation uses
the general durable delivery-attempt counter: even a transient marker-list
failure before the POST can conservatively consume that revision and require a
new revision, trading availability for duplicate prevention. Updates and deletes use
the last provider ETag; a 412 is a durable conflict. To recover another terminal
or conflicted publication, reconcile provider state, revise the canonical item,
and explicitly enqueue the newer revision; the durable outbox marks every older
unpublished revision as `superseded` instead of later replaying stale content.
Deletion is refused until the canonical item is in recoverable trash. A
provider-side deletion or material edit of a DayWeave-owned record conflicts any
queued publication and never silently trashes or overwrites the canonical item.
External or attendee-bearing event edits are additionally forbidden: there is
not yet a server-minted preview/approval audit primitive, so supplying arbitrary
material event JSON is not an API capability.

Provider cursors are encrypted, but durable outbound JSON currently contains
the selected DayWeave item's title and notes in PostgreSQL plaintext. Production
deployment therefore still depends on restricted database access and the
documented database/storage encryption gate; the migration does not classify
outbox payloads as non-sensitive metadata.

Two canonical-model gaps remain explicit. Imported attendee identity,
conference, and attachment entities have no first-class canonical tables; the
safe import retains the self response and bounded counts but does not claim
round-trip fidelity. Also, published schedule blocks and imported fixed events
are not yet automatically fed into the side-effect-free schedule-preview input;
the current preview contract still requires callers to supply fixed blocks.
Likewise, Google Tasks parent/order values are retained as provider metadata but
are not silently projected into the canonical hierarchy until a conflict-aware
hierarchy reconciliation primitive exists. These gaps must be closed before
claiming automatic schedule publication or full-fidelity bidirectional sync.

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

The private MCP endpoint uses Streamable HTTP over the Nebius Tunnel HTTPS URL.
Its external contract is intentionally asymmetric:

- clients may read only the schedule detail permitted for that client;
- clients may simulate plans;
- `submit_proposal` may add a reviewable Suggestions Inbox proposal;
- no external conversation can mutate canonical schedule/calendar/task state.

After the deployed MCP server supports OAuth:

1. enable ChatGPT Developer mode;
2. register the MCP HTTPS URL and complete its OAuth connection;
3. copy the resulting `plugin_asdk_app…` technical ID;
4. add that ID to the plugin's `.app.json` and manifest using the plugin creator;
5. validate, install from the private repository source, and test in a new chat.

The repo plugin already contains its validated skill and local `.mcp.json`.
Remote registration cannot be completed before the stable tunnel URL and OAuth
metadata exist.

Official references:

- <https://developers.openai.com/codex/mcp/>
- <https://developers.openai.com/plugins/build/plugins>

## Nebius access and deployment identity

The human `lol` profile is sufficient to bootstrap resources; it must not be
copied to the VM or CI. Deployment creates separate least-privilege identities:

- a runtime service account attached to the VM;
- a tunnel-agent account with only `applicationtunnel.agent` on its tunnel;
- a backup account restricted to the private Object Storage bucket;
- an optional CI deployment account restricted to updating DayWeave resources.

The selected instance is `cpu-e2/2vcpu-8gb` in `eu-north1` with a 32 GiB Network
SSD. One VM runs API/worker/PostgreSQL. The account profile check is complete;
there is no need to share a tenant token or credential file.

Nebius Tunnel provides the generated DNS name and TLS while accepting only an
outbound agent connection. It does not replace application authentication.

Official references:

- <https://docs.nebius.com/tunnels/overview>
- <https://docs.nebius.com/tunnels/quickstart>
- <https://docs.nebius.com/compute/virtual-machines/types>
