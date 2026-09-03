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
6. Start with Calendar and Tasks read access during import/onboarding. Request
   each service's write access separately and only when the owner enables a
   selected Calendar or Task list for publication.

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

#### macOS owner flow

The native Mac client exposes import and explicitly reviewed Calendar and Tasks
publication after durable DayWeave device enrollment is active:

1. Open **Settings → Accounts → Google** and choose **Connect Calendar & Tasks**.
   The client explicitly sends `"services":[]`; by server contract this requests
   Calendar read-only and Tasks read-only together. No Google client secret,
   provider token, callback code, or callback state belongs on the Mac.
2. Choose **Open Google**, complete consent in the external browser, then leave
   DayWeave unlocked while it checks the account or choose **Check connection**.
   Browser launch is not considered success. The protected accounts endpoint is
   authoritative account inventory, but it cannot bind a change to one exact
   browser attempt, so the client keeps that attempt's recovery journal until
   expiry. The authorization URL is never persisted.
3. Choose **Discover sources** for an active account. Select Calendar sources as
   reference-only or blocking and select Task lists as reference-only. Task
   lists never block calendar time.
4. Choose **Refresh import**. The returned `202` is only durable queue
   acceptance. DayWeave polls sync status and pulls/recomposes canonical items
   only after the accepted monotonic refresh generation is completed by an idle
   run. Its non-secret completion marker has
   no time-based expiry and is deleted only after fresh canonical composition
   reports success. Backoff or a still-running import can be checked later
   without replaying the mutation. If acceptance was never proved, the UI may
   safely replay the exact persisted request UUID; the server returns its
   original generation without queuing duplicate work. A terminal failed
   run—or a reauthorization-required run after
   authorization is repaired—can also be retried: before transport, the client
   replaces the marker with a new durable request UUID. Completion uses only
   monotonic generations, so API/client/worker clock skew cannot falsely prove
   the retry completed. If sync status requires reauthorization while the
   provider account still appears active, the Reauthorize action remains
   available and preserves the pending completion marker.
5. To publish Calendar events, choose **Enable Calendar publishing** for that
   existing account. The Mac requests exactly the broad Calendar service with
   forced incremental consent; it does not request Tasks write scope. After the
   authoritative account snapshot reports the grant, mark a selected Calendar
   **Publish**. The Mac offers this role only when Google reports `owner` or
   `writer` access. Confirmed busy timed events are enabled by default; all-day,
   tentative, and free event publication are separate collection switches.
6. To publish tasks, separately choose **Enable Tasks publishing**. This requests
   exactly the broad Tasks service against the existing account and does not
   expand Calendar access. After the authoritative account snapshot reports the
   grant, mark a selected Task list **Publish**. Blocking is never available for
   a Task list, and a Calendar-only grant cannot make one writable.
7. In the Items Inbox, select a supported synced, app-authored fixed event or
   task and choose its Google publication action. The client accepts only an
   owned `dayweave_firm_block` for Calendar, or a non-recurring Task whose
   canonical fields can round-trip safely. Imported, unsupported, skipped, and
   canceled Tasks remain local/import-only. A recoverably trashed mapped event
   or Task may be reviewed for deletion. The client requests the exact preview
   and displays the reviewed provider payload; DayWeave-only Task planning
   metadata is not sent, and server-managed Calendar ownership-proof values are
   redacted. **Approve & Queue** is the sole path that creates approval
   authority. Preview, approval, and enqueue require exact HTTP `200`, `200`,
   and `202` responses and validate every bound ID, revision, operation, hash,
   entity kind, and expiry. Expiry validation tolerates the supported
   five-minute device clock skew while locally elapsed authority remains
   non-actionable.
8. Android uses the same server preview/approval/outbox contract for a strict
   upsert-only subset. Configure the account's full Calendar grant and writable
   **Publish** destination on macOS first, refresh **More → Google sources** on
   Android, then use **Inbox → Items → Publish**. Android accepts only a current
   app-owned, non-all-day, confirmed, busy timed event; it never publishes task
   schedule blocks, recurrence, guests, meetings, attachments, Google Tasks, or
   deletion from this surface. Its secure review says **queued** after HTTP 202,
   while provider delivery remains asynchronous.

Before preview transport, the Mac synchronously saves the exact intent to its
encrypted planner snapshot. It persists the returned preview before display and
persists the expiring approval capability before enqueue. Relaunch recovery may
replay an intent or approved enqueue exactly, but it never approves a preview
automatically. Because capability issuance is one-shot, the Mac also persists an
approval-attempt fence before that request. If its response is lost, it does not
offer approval again or enqueue anything; recovery remains until the reviewed
preview expires. The record is cleared only after authoritative outbox acceptance.
Lock/sleep redacts the preview and fences late results. API credential changes,
Google account/source mutations, imports, and canonical cache reset are blocked
while live recovery authority remains. An expired preview or capability is
server-unusable; destructive discard waits one additional five-minute skew window
and requires a separate warning confirmation plus exact journal comparison. An
approved-stage warning also explains that a prior enqueue response may have been
lost and asks the owner to verify Calendar or server state before retrying. This
recovery can be discarded after its old authentication binding is unavailable. No Google token,
approval capability, callback material, or provider credential belongs in this
public repository or in plaintext planner state.

OAuth start keeps only a non-secret, expiring request/idempotency journal for an
exact lost-response retry. Disconnect separately retains its exact account,
revision, and idempotency identity until authoritative revocation; it never
ages that identity out while the server's revocation fence exists, and it is
cleared only after a verified fresh canonical composition removes retired
imports. An exact, endpoint-bound revision-conflict response proves a stale
disconnect had no effect. The client retires that obsolete request identity
only after an authoritative read still shows a usable account; if the account
is absent or revoked, it retains the marker through verified canonical
composition. App lock,
sleep, and inactivity cancel work and redact all in-memory Google labels and
browser authority. Destructive or cross-base DayWeave credential replacement
is blocked while recovery exists; a same-API-base authentication repair may
rebind recovery only after the protected account identity is visible. If the
old session is no longer recoverable and the protected account is absent, the
app offers a destructive, explicitly confirmed abandonment of only that
orphaned local marker. Cleanup revocation fences and operator-recovery state
block new connect and reauthorization attempts before an OAuth request is
persisted. Resetting unreadable disconnect/import recovery first requires a
verified fresh composition. Tests use injected transports and synthetic
identities; they never open a browser or contact Google.

#### Server/operator API sequence

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
4. `POST .../sync/refresh` accepts a persist-before-send `request_id`, durably
   increments a monotonic refresh generation, and returns both in `202`;
   replaying the same ID returns its original acceptance without duplicate work.
   Periodic
   reconciliation also runs every 15 minutes. `GET .../sync` reports the run,
   retry time, accepted/claimed/completed refresh generations, stable redacted
   run/outbound error codes, import conflicts, and outbound queue counts. Rate
   limiting and transient provider failures enter
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

The same preservation rule applies when an occurrence is the target of the
open execution lease. Calendar deselection, provider removal or downgrade,
account pause, disconnect, and operator recovery remain authority fences and
must complete immediately: DayWeave leaves the executing canonical item and its
revision unchanged, retires the restricted mapping as an execution-active
conflict, and keeps the lease intact. The retained item currently requires
later user cleanup; automatic post-execution retirement remains a client UX gap
until it has its own durable, reviewable cleanup intent. Ordinary inbound
completion or deletion is different: it rolls back without changing the item,
mapping, cursor, audit record, or outbox and retries after a short backoff once
the execution lease closes.

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
1–30). Generated-schedule publication has a second, narrower default-off gate:
set `DAYWEAVE_GOOGLE_SCHEDULE_OUTBOUND_ENABLED=true` only when the general
outbound gate is also enabled. Configuration is rejected if the schedule gate
is enabled on its own. The complete single-item mutation flow is intentionally
three-step:

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
   retry returns the same outbox ID; after successful consumption this remains
   a receipt lookup even after expiry and cannot create another outbox row.
   Swaps or mutations fail and do not consume authority. An expired capability
   that was never consumed is rejected.

The Mac persists each transition in its encrypted planner state before the
corresponding request. If an approved enqueue response is lost, recovery sends
only the saved capability and exact bound tuple. It may do so after the Mac's
observed expiry: server time may still consume that already-approved request
once within the clock-skew window, but after authoritative expiry the server
can only return an already-consumed receipt or reject it without new work.
Preview and uncertain approval-attempt stages never repeat approval
automatically. Expired recovery can be discarded only with explicit
confirmation after the supported five-minute clock-skew window.

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

#### Generated firm-schedule publication

The generated-schedule API is a separate server-first batch flow. It does not
reuse the Inbox's single-item `dayweave_firm_block` request. The caller supplies
one selected writable Calendar and the expected ID of the current immutable
published schedule. Outbound creates and updates are admitted only for its
not-yet-elapsed generated firm `planned` and `pinned` blocks. Imported
`external_fixed`/Calendar blocks are never re-exported, exact elapsed instances
can only be no-ops, and this slice has no tentative-block publication path.

The four REST steps are:

1. `POST /v1/integrations/google/accounts/{account_id}/schedule-publications/previews`
   with `collection_id` and `expected_schedule_revision_id`. The server
   revalidates the current published revision, publication hash, exact writable
   collection revision and remote ID, full Calendar scope, retained mappings,
   and ETags. It returns an expiring review-safe batch with create, update,
   delete, and no-op counts and an exact preview hash.
2. `POST .../schedule-publications/previews/{preview_id}/approve` with
   `expected_preview_hash`. Approval is an explicit action by an eligible,
   tenant-bound native device and returns one expiring OS-CSPRNG capability
   exactly once. Only its hash is stored, and request/response types carrying it
   are excluded from debug output.
3. `POST .../schedule-publications` with the same `preview_id`, `collection_id`,
   `expected_schedule_revision_id`, and `approval_capability`. Consumption is
   atomic and content-bound. Success is HTTP `202`; an exact replay returns the
   same publication receipt and cannot enqueue a second batch.
4. `GET .../schedule-publications/{publication_id}` returns only aggregate
   pending, delivering, published, conflicted, failed, and superseded counts.
   `pending_count` includes both work awaiting its first attempt and retryable
   work waiting in durable backoff; `delivering_count` is in-flight work. The
   response contains no block title or provider payload and is the authoritative
   delivery check after queue acceptance.

Preview admission is serialized across each provider account. A retry can reuse
the newest live, unconsumed preview only after the server revalidates every
stored child against the current schedule, collection, and mapping state. An
account may retain at most eight active unconsumed previews and 20,000 summed
active change rows; the next request returns HTTP `429` without adding storage.
Expired, unconsumed preview payloads that have no publication batch are pruned,
including previously approved ones, while their approval audit evidence is
retained. Consumed previews and publication history remain immutable. The exact
direct JSON preview must fit within 16 MiB before it is newly persisted; a
larger projection returns HTTP `502`, matching the bounded macOS and Android
transports instead of creating a review neither client can load.

The mutation routes require a DayWeave device principal with both Google write
authority and `schedule_read`; status requires Google read authority plus the
same device/schedule scope and exact user/workspace binding. All responses use
`Cache-Control: no-store`. This API currently has no native macOS or Android
trigger, so it is directly API/test accessible to an eligible device credential
rather than an owner-facing workflow.

Each generated session has a stable logical slot derived from workspace, item,
occurrence, and session index rather than from its placement-dependent schedule
block UUID. A move in a later schedule therefore updates the same Google event.
A previously published future slot absent from the new firm schedule becomes a
reviewed conditional delete. Elapsed events are immutable Calendar history and
are never rewritten, deleted, or reused; once their mapping is retired, any
later reuse of the logical slot receives a new incarnation and provider event
ID. Provider event IDs and authenticated ownership proofs are account/calendar
scoped, keyed, and non-reversible.

Generated events are confirmed, opaque, private, timed, and attendee-free. A
sensitive block is titled `Busy`; a non-sensitive block receives only its
bounded title. Description, location, recurrence, conference data,
attachments, and raw DayWeave identifiers are omitted. Reminders are explicitly
disabled with `reminders.useDefault=false` and an empty `overrides` list, and
create, update, and delete requests use `sendUpdates=none` so Google does not
send attendee notifications. The private ownership proof stays in the
server-side provider payload rather than the reviewed summary.

After enqueue, the existing Google sync worker leases each non-no-op change
from durable PostgreSQL state. Immediately before network I/O it rechecks the
current schedule, account/scope, collection revision and role, mapping/ETag,
parent-run generation, claim, intent hash, and short-lived dispatch nonce.
Creates use deterministic Google IDs: after an ambiguous result, recovery first
reads that ID and adopts it only when the complete event and authenticated proof
still match. Updates and deletes use `If-Match`. Retryable provider failures
back off durably, partial batches remain observable, and a newer schedule
supersedes work that has not been sent. A response observed after a guardian
changes is retained as conflict/reconciliation evidence instead of being
discarded or blindly repeated.

When a create may have reached Google and the exact authenticated lookup remains
negative, neither elapsed time nor a number of negative reads is evidence that
the mutation had no effect. The row remains durably unresolved with exponential
backoff capped at one hour and prevents later publication for that selected
Google account/calendar target. A later positive authenticated observation can
release that fence. This checkpoint exposes no schedule-specific operator
reconciliation endpoint or supported database-intervention runbook.

Claim and final dispatch authorization independently recheck whether the block
has become Calendar history. For update/delete, the deadline is the earlier of
the reviewed desired end and the immutable mapped event end. Definitely unsent
elapsed work becomes superseded. Possible-send work may still perform an exact
read and adopt the observed effect, but it cannot receive a new write permit.
A success response with an unusable identity or a body over the processing cap
stays in active backoff, preserves that reason through worker/account recovery,
and continues to block successor publication until reconciled.

This server milestone does not complete `SCH-006`. Neither native client has a
review/recovery journal or trigger for this API, no scheduler or firm-horizon
automation enqueues it, and inbound edits to these generated Google events are
not supported. Google move → local pin and delete → local unschedule
interpretation plus the firm/tentative transition model are still required.
Tentative blocks remain app-only.

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
command, file-change, and approval requests are denied. A completed final-answer
message may append one strict, duplicate-key-free
`<dayweave-item-drafts-v1>` envelope containing at most five editable canonical
item bodies and no identifiers, status, hierarchy, sensitivity, revision, or
configuration authority. Interrupted, failed, streaming, malformed, oversized,
or non-final output cannot route a draft.

DayWeave validates the complete envelope atomically, generates every item and
mutation identity locally, forces each draft to private Inbox state, and saves
it only in the encrypted planner snapshot with a seven-day expiry. An
identity-bound monotonic deadline runs for each pending record, and the app
commits an encrypted wall-clock high-water checkpoint every five minutes while
a private draft remains, so a quiet process and later clock rollback cannot
silently restart its full retention window. Hidden sub-minute values, repeated
DST-fold offsets, and all-day bounds outside local midnight are rejected before
the review Inbox. The local Suggestions Inbox opens the ordinary typed item
editor; nothing is created until the owner reviews every field and chooses
**Create item**. Approval saves the accepted linkage and exact canonical create
journal in one encrypted transition. It does not change the composed schedule
or make a network request; the existing canonical sync later publishes that
immutable idempotent journal. Rejecting or expiring a draft scrubs its retained
body. Legacy local prose suggestions remain advisory, and **Mark reviewed**
never creates an item.

Separately, a server-backed `dayweave.proposal-change-set/1` suggestion can be
simulated, reviewed field by field, explicitly approved, applied atomically,
and undone through the native device workflow documented in
[proposal-applications.md](proposal-applications.md).
Chat requires a signed-in ChatGPT account and network access; there is no offline
model fallback.

For development attestation, run `./scripts/verify-codex-runtime.sh` directly.
Do not invoke it as `bash scripts/verify-codex-runtime.sh`: privileged Bash
startup and the verifier's clean-environment re-exec are part of the check.
`./scripts/build-macos-app.sh` repeats the verifier, seals the runtime, and checks
the final local app signature. Notarized distribution still requires the owner's
Apple Developer signing setup.

Official reference: <https://learn.chatgpt.com/docs/app-server>.

## Advisory assistant on Android

Android uses the authenticated DayWeave API as a separate remote-provider boundary. It does not
copy the macOS Codex runtime, ChatGPT browser state, managed OAuth tokens, or an OpenAI key onto the
phone. `POST /v1/assistant/turns` accepts only a native device session with both schedule-read and
item-read access (or the legacy owner credential during migration); MCP credentials are rejected.

The endpoint is disabled by default. To enable it in a reviewed deployment, place the OpenAI API
key only in the root-owned production environment or secret manager, optionally choose a bounded
model name, and add `deploy/compose.assistant.yaml` to the normal Compose invocation. The overlay
sets `DAYWEAVE_ASSISTANT_ENABLED=true`, requires `DAYWEAVE_OPENAI_API_KEY`, and defaults
`DAYWEAVE_OPENAI_MODEL` to `gpt-5.6-luna`. It also defaults to six requests per minute per
principal, two concurrent provider calls, a one-million-token rolling per-process daily budget,
and bounded API CPU, memory, and process counts. Keep all subordinate OpenAI settings absent while
the feature is disabled. Never put the key in an APK, Gradle property, repository file, shell
output, issue, or chat transcript.

Each manual turn is non-streaming and advisory-only in this milestone. Android first commits the
user message to SQLCipher, then sends at most 8 KiB of input, up to 10 completed in-process turn
pairs from the same native device binding, and a deterministic context of at most 64 KiB. Failed,
stopped, or context-aborted prompts never become later history; history eligibility is cleared on
lock, background, restart, or binding change. The encrypted transcript stays visible as local
reference, and the Assistant tab explains that those boundaries start a new provider context.
Public blocks and nonsensitive items use ephemeral references.
Sensitive content becomes occupancy-only busy spans; sensitive titles, all notes, stable IDs,
provider identities, revisions, raw recurrence, and raw constraints are omitted. The disclosure
counts remain visible in the Assistant tab.

The server calls the fixed official Responses API endpoint with response storage disabled,
explicit prompt-cache mode and no cache breakpoints, no tools, no background mode, low reasoning,
and a bounded output. It accepts exactly one completed assistant text result and reconciles its
conservative preflight token reservation against the provider's validated usage totals. Provider
credentials, upstream bodies, prompts, and planner content are not logged. Android does not
schedule assistant work, automatically replay a failed call, or retain a partial response.
Locking, backgrounding, stopping, or replacing the device binding cancels and generation-fences
the turn. The provider has no item, schedule, proposal, Google, or execution mutation handle;
existing reviewed proposal/application workflows remain the only mutation path.

The in-process daily token budget is a fail-safe, not a currency-denominated billing guarantee and
it resets when the API process restarts. Before enabling the overlay, set an OpenAI project budget
and alert at or below the owner's spending limit and use a project with the required data-retention
or Zero Data Retention policy. Keep the feature disabled until both controls are verified.

Official references:

- <https://developers.openai.com/api/reference/cli/resources/responses/methods/create>
- <https://developers.openai.com/api/docs/models/gpt-5.6-luna>

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
- `submit_proposal` requires and atomically consumes the exact simulation token,
  then may add either a typed application-ready or advisory Suggestions Inbox
  proposal;
- only an authorized DayWeave device can preview or apply a typed proposal;
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
