# DayWeave integration setup

This document separates work Codex can perform automatically from browser
consent that only the owner can complete. Never paste credentials into chat or
commit them to this repository.

## Current local access

- GitHub CLI is authenticated as `greengolddog` and the repository is private.
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
   match exactly.
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
   Google `owner` or `writer` access and the broad Calendar scope.
4. `POST .../sync/refresh` durably requests a run and returns `202`; periodic
   reconciliation also runs every 15 minutes. `GET .../sync` reports the run,
   retry time, stable redacted error code, import conflicts, and outbound queue
   counts. Rate limiting and transient provider failures enter bounded backoff;
   invalid authorization and terminal durable failures make `/ready` false.

Calendar pages include tombstones and whole recurrence series. Exceptions retain
their series ID and original start. All-day bounds keep Google's exclusive end
and are converted using the event/calendar IANA timezone, including DST days.
Birthdays, working-location events, transparent events, and self-declined events
never block. Out-of-office and ordinary opaque events block only for `blocking`
or `writable` sources. An invisible source imports only a redacted label and
time bounds. Tasks import completed, hidden, deleted, due, parent, and ordering
metadata. Provider cursors are AES-GCM sealed with account/collection AAD and
advance only after every page and canonical mutation commits. Calendar 410
recovery performs a bounded full scan from 366 days in the past through 730 days
in the future; older history remains at Google unless the product adds an
explicit archival import.

Provider changes never silently overwrite a locally edited canonical item. The
mapping records the last imported local revision: a provider update or deletion
applies only while that revision still matches. Otherwise it becomes an
operator-visible conflict. Provider deletion moves an unchanged item to
recoverable trash. A move between Calendar or Tasks collections is represented
as deletion in the old collection plus import in the new collection, preserving
any conflicting local edit rather than guessing identity across sources.

Outbound publication uses `POST .../outbound` with `collection_id`, `item_id`,
`expected_item_revision`, and `operation` (`upsert` or `delete`). It accepts only
selected writable collections and DayWeave-owned mappings. Calendar insertion
also requires canonical `dayweave_firm_block` ownership and increasing RFC 3339
`starts_at`/`ends_at` bounds; deterministic Google event IDs make crash retries
idempotent. Tasks carry a visible `[DayWeave item:…]` note marker so a crash
between provider acceptance and local acknowledgement can be recovered without
duplicating the task. Updates and deletes use the last provider ETag; a 412 is a
durable conflict. Deletion is refused until the canonical item is in recoverable
trash. External or attendee-bearing event edits are deliberately forbidden:
there is not yet a server-minted preview/approval audit primitive, so supplying
arbitrary material event JSON is not an API capability.

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
