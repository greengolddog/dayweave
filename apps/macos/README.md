# DayWeave for macOS

The native SwiftUI client keeps the local planner usable without a server. A
new production profile starts with an empty plan and restores the encrypted
snapshot synchronously before exposing actions; preview fixtures are used only
by `PlannerStore.preview` and tests.

## Guided onboarding

First launch presents a resumable **Welcome & privacy → DayWeave API → Google
resources → Schedule profile → Notifications → First item → First plan →
Ready** flow. Until the privacy and approval boundaries are acknowledged,
workspace surfaces remain hidden, mutation commands are unavailable, and
network and contained Codex services stay paused. Readiness after that boundary
is always derived from the authoritative stores rather than trusted from the
onboarding checkpoint.

- The API step requires an origin-bound credential plus a completed
  authenticated request for the current process and configuration. Merely
  storing a credential—or retaining an older publication proof—is not enough;
  relaunch requires a fresh check so a previously rejected credential cannot
  regain readiness offline.
- The Google step requires saved, selected Calendar and Tasks resources on
  active, sync-enabled accounts. Every participating account must complete an
  initial import, and each selected Calendar must reach the planning projection
  at its saved collection revision.
- Schedule profile and notification access are informational confirmations.
  The profile page summarizes the valid encrypted profile that will be used;
  the notification page reports allowed, denied, or deferred permission and
  never invents a break or opens a permission prompt during onboarding.
- The first-item step creates or follows one reviewed planning-demand item. A
  content-free anchor to that exact item and, after sync, its canonical revision
  lives with the item in the encrypted planner snapshot. The first-plan step
  succeeds only when the current exact publication proof matches the preview,
  the complete published plan, the current API configuration, and a block for
  that anchored item revision.

**Set up later** dismisses the flow without completing it or weakening any
gate. The current step is retained and can be resumed from the privacy
backdrop, Today, or Settings. If the strict onboarding checkpoint is corrupt or
from a future unsupported version, DayWeave leaves it untouched, blocks
checkpoint writes, and offers a confirmed reset of setup progress only; planner
data, credentials, accounts, recoveries, items, and schedules are not reset.

## Appearance

**Settings → Appearance** supports the system theme plus explicit light and
dark modes, with blue, indigo, purple, pink, orange, green, and teal accents.
The versioned preference is applied consistently to the main window, Settings,
menu-bar surface, and locked screen. An invalid preference resets only the
appearance; it cannot replace or modify the encrypted planner snapshot.

## App lock

**Settings → Privacy & app lock** can require Touch ID or the Mac login
password before any DayWeave content is shown. Enabling and disabling the lock
both require device-owner authentication. An enabled profile starts locked on
every cold launch, and the automatic lock delay can be immediate, 1, 5, 15, or
60 minutes after the app becomes inactive. macOS session-lock and sleep events
also enter the same inactivity boundary.

While locked, the main window, Settings window, menu-bar details, keyboard
commands, foreground sync, and contained Codex runtime are unavailable or
redacted. Authentication cancellations and stale successes after a lifecycle
change fail closed. Preferences contain no schedule or credential material and
are stored as one versioned `UserDefaults` record; a malformed existing record
is treated as enabled so it can be recovered only after authentication.

## Execution timers and break reminders

Canonical focus sessions use the server-authoritative execution lease and are
reconciled before every start, pause, resume, completion, skip, or defer. A
timed pause never resumes automatically. At its deadline the in-app resolution
offers **Resume**, **Extend 10 minutes**, **Choose another item**, or **Keep
paused**; extension is a new revision-guarded pause command. The two
keep-paused choices first capture the exact paused revision and opaque reminder
digest, verify Notification Center removal while the encrypted lease is still
intact, revalidate that exact lease after the await, and only then record the
acknowledgment in the encrypted execution snapshot. Cancellation failure,
lease drift, or a storage failure leaves the resolver open, restores
reconciliation for whichever authoritative reminder remains, and never routes
or invents a server mutation.

For a future timed-pause deadline, macOS schedules one local Notification
Center request only after validating the encrypted session shape. The request
identifier is a domain-separated SHA-256 value over the session revision and
deadline. Its fixed title and body contain no task title, notes, raw item or
session identifier, `userInfo`, or direct mutation action. Reconciliation after
launch and each execution transition replaces or removes stale pending and
delivered requests; resume, completion, skip, defer, replacement, and an
explicit **Keep paused** or **Choose another item** acknowledgment therefore
cancel the old reminder.
Each stale opaque identifier is removed from both Notification Center
collections so a request firing during cancellation cannot escape by moving
from pending to delivered. Concurrent reconciliations coalesce to the newest
lease, but every initiating caller remains suspended until that newest state
has converged.
Because the system removal calls themselves return before daemon-side state is
observable, cancellation uses a bounded remove-and-requery barrier across both
collections. Credential replacement and canonical-cache reset proceed only
after that barrier proves every owned identifier absent. A timeout is visible
in the app and preserves the encrypted execution lease and credentials for a
safe retry. Already-authorized scheduling and verified cancellation are awaited
before the initiating execution or local acknowledgment finishes. DayWeave
never opens a system permission prompt during launch, restoration, polling, or
a server pause: a future timed break instead shows an explicit **Enable
reminders** control (and a System Settings remediation link after denial), so
permission denial or an unavailable notification service never blocks the
authoritative pause. Permission-service, scheduling, and cancellation failures
remain visible with an explicit retry control; an authorized-but-failed request
is not silently presented as configured.

A notification click retains only that opaque identifier, waits behind the app
lock, revalidates the exact still-paused expired session, consumes stale clicks,
and activates the single-instance DayWeave window with the existing explicit
resolution UI. The clicked digest is bound to the lease generation observed at
routing time. While that digest remains in the process-lifetime tap mailbox,
the main window suppresses expired-break alerts; exact routing installs the
store presentation token before consuming the mailbox. Stale response A
therefore cannot briefly present a newer expired break B during closed-window
activation or unlock. A rejected tap shows a generic stale-reminder banner; the
user must explicitly choose **Review current break** before B's ordinary
resolver can appear, so the old click is never silently retargeted and cannot
leave B unreachable. The suppression is bound to the observed break digest, so
a different later deadline or lease returns to ordinary clock-driven
presentation; with no notification response, that presentation is unchanged.
Foreground banners are suppressed only for owned break reminders because the
app delegate has no access to decrypted lease state; other app notification
categories keep their foreground banner and sound. An owned delivery emits
only a process-local counter—no identifier or planner content—to invalidate the
in-app resolver. The execution store also owns an exact local deadline wake-up,
so an expired break becomes actionable without waiting for network polling or a
foreground notification callback. A resume performed on another device can
cancel this reminder only after the Mac receives the newer lease. While the app
is foregrounded and unlocked, an execution invalidation stream now requests
that reconciliation promptly; the independent 30-second poll remains the
durable catch-up path.
Final acceptance still needs one bundled-app smoke test for permission UI,
background delivery, lock/unlock routing, background banner/sound appearance,
and Notification Center click behavior.

After a successful **Choose another item** acknowledgment, the main window
routes to Today and shows a dedicated handoff panel. It derives candidates
freshly from the complete current publication proof and lists only scheduled,
executable canonical leaves backed by exact `planned` blocks. It excludes the
paused item, events, breaks, local or helper-only blocks, unpublished or stale
placements, pinned/fixed/hard blocks, hierarchy parents, pending or conflicted
items, terminal sessions, and anything that would otherwise fail the canonical
Start checks. Missing hierarchy, freshness, revision, or whole-plan evidence
fails closed. Candidates remain in schedule order, the first is labeled **Next
in plan**, and bounded control-free scheduler placement reasoning is shown when
available.

Choosing a candidate only selects and highlights its existing Today block; it
does not start, resume, complete, skip, defer, or publish anything. The
authoritative paused lease continues to disable the ordinary Start controls
until the owner moves that session later, completes it, or skips it. Schedule
refreshes recompute the panel from current proof and clear a selection that is
no longer eligible. With no safe candidates, the panel explains that the
current item remains paused and must be resolved before another item can start.
The handoff state is intentionally process-local, while its exact expired-break
acknowledgment is durable, so restart does not reopen the resolved dialog. The
panel lives inside `RootView`; the existing external app-lock boundary hides it
along with all other schedule content.

## DayWeave API

Open **Settings → DayWeave API** and provide:

- the server root URL, such as `https://dayweave.example.com` or the local
  development endpoint `http://127.0.0.1:8787`; the app appends
  versioned endpoint paths itself;
- either a legacy bootstrap bearer for a one-time upgrade or an already-minted
  one-time enrollment code beginning with `dw_en1_`.

Remote URLs must use HTTPS. Plain HTTP is limited to loopback development, and
redirects are not followed so a credential cannot be redirected to another
origin. The base URL is ordinary configuration stored in `UserDefaults`.
Authentication authority lives in one canonical, versioned Keychain envelope
bound to the normalized API origin and stable client/session identity. Explicit
states cover legacy upgrade, enrollment creation prepared for retry, enrollment
consumption prepared for retry, active rotating credentials, refresh prepared
for retry, reauthentication, and incompatible recovery. Every
credential-bearing transition is compare-and-swap protected across app
processes and verified by an exact Keychain readback.

The bootstrap path generates a stable client ID, enrollment ID, one-time
`dw_en1_` credential, session ID, and access/refresh pair locally. Before its
first network send, it journals the complete enrollment-creation request: the
canonical full URL including base path, method, security headers, exact body
bytes and digest, bootstrap authority binding, and preparation time. A first
`201` response must exactly echo the proposed ID and credential with
`replayed:false`; an exact retry must return the same public fields as `200`
with `replayed:true`. Only then does the app journal and consume the proposed
session tuple. The direct-code path remains distinct: it skips enrollment
creation and journals the supplied `dw_en1_` credential with its locally
generated tuple before its first consume request.

A crash, timeout, cancellation, or lost response retains the exact pending
request. Restart recovery uses the journaled target and bytes, never a newly
entered API base and never a replacement tuple. Older pending
records without a complete request fence are quarantined instead of being
retargeted. Access refresh follows the same persist-before-send rule for a
material-distinct next pair. Proactive refresh and a strictly validated API 401
share one coordinator and one in-flight rotation. The original API request is
replayed with only its Authorization value changed, and only while its stable
authentication binding remains unchanged.

After durable activation, DayWeave never falls back to a legacy bearer. A live,
pending, ambiguous, or incompatible session cannot be replaced—even at another
origin—until the existing authority is handled. Normal sign-out first refreshes
if necessary and sends an authenticated `DELETE /v1/auth/sessions/{session_id}`;
only a strict `204` response permits exact local deletion. If the server cannot
be reached, **Forget only on this Mac** is a separate destructive confirmation.
It retains a no-secret tombstone that records the local-only decision and warns
that server session or bootstrap authority may still exist. Same-origin
re-enrollment is also available after a definitive expired/rejected state;
cross-origin replacement requires the explicit local-only tombstone.
The current device contract is v2 and adds the REST-only `schedule_publish`
scope. A stored v1 session is deliberately not upgraded in place because the
server never granted it that authority; it is quarantined and requires the
existing explicit local-forget warning plus re-enrollment before schedule
publication can run.

Suggestions, canonical cache/cursors/mutations, and execution recovery are
fenced to the authentication binding as well as the URL. Replacing a session at
the same origin therefore quarantines stale work instead of sending it under a
new identity. Legacy raw-token records have no trustworthy origin and require
explicit re-entry. Credentials are device-only and are never added to the
Codable planner snapshot or application diagnostics.

Foreground execution sync opens `GET /v1/execution/stream` with
`Accept: text/event-stream`, `Accept-Encoding: identity`, and the exact
encrypted execution revision as `Last-Event-ID`. Successful frames must contain
one canonical integer `id`, the event name `execution-invalidation`, and the
exact matching `{"revision":N}` payload; the only accepted comment frame is a
standalone `: heartbeat`. The byte parser bounds every line, frame, total frame
count, and event count; rejects empty or mixed comment frames, malformed UTF-8,
control bytes, duplicate or unknown fields, noncanonical JSON integers, and any
revision that does not advance beyond the durable resume cursor. Redirects
remain disabled, successful responses require the strict event-stream media
type and an absent or single exact identity content encoding, non-success bodies
have a small independent bound, and durable authentication gets the same single
exact 401 recovery attempt and binding checks as normal API calls. One independent
330-second absolute watchdog covers the whole public stream call—including
that recovery attempt—so heartbeat or byte progress cannot extend the
connection indefinitely; expiration and task cancellation close the underlying
URLSession task.

An invalidation is never persisted as execution state. The store coalesces its
highest new revision and runs the existing authoritative snapshot/history plus
deferred-publication reconciliation. A connection can provoke only one such
coalesced refresh when an advertised revision stays unreachable; the 30-second
poll retains durable catch-up. An immediate EOF is a transient failure and
joins other transient failures in bounded exponential 1-to-30-second backoff;
a connection that demonstrates bounded heartbeat or event liveness resets that
backoff before a normal five-minute EOF reconnects from the newest durable
revision. A 404 disables only that foreground activation. Streaming begins only
after a successful poll proves the current binding reached healthy encrypted
persistence, and later polls retry readiness if an earlier poll could not bind.
Stream health is silent and cannot replace the poll's user-facing status.
Leaving the foreground, app lock, API configuration changes, or credential
replacement cancels the stream and its URLSession task immediately.

## Google Calendar and Tasks

**Settings → Accounts → Google** connects through the configured DayWeave API.
The Mac sends an explicit empty `services` array, which the server defines as
Calendar read-only plus Tasks read-only. Google access and refresh credentials,
the OAuth callback code, and callback state remain server-only. The app accepts
only a short-lived `https://accounts.google.com/o/oauth2/v2/auth` page, consumes
that in-memory capability before asking macOS to open it, and never saves the
URL or exposes it through SwiftUI or diagnostics.

Before OAuth start, DayWeave saves a non-secret exact-retry journal containing
the request, idempotency key, DayWeave authentication binding, baseline account
revisions, and expiry with a synchronized read-back before transport. A timeout
or lost response can therefore replay the same request rather than creating
another authorization session. The URL itself is memory-only. Because the
accounts endpoint cannot identify one particular browser attempt, an account
change is candidate evidence only; the exact journal remains recoverable until
it expires. Lock, sleep, inactivity, or API credential replacement cancels the
active operation, clears account/source labels and pending browser authority,
and rejects late results. Unlocking reloads them only under the current durable
DayWeave session; legacy static bearers are not used by the live Google client.

Connected accounts expose discovery, pause/resume, reauthorization, and an
explicitly confirmed disconnect. Calendars can be imported as visible reference
data or complete blocking constraints. Task lists import as reference data and
never block calendar time. **Enable Calendar publishing** and **Enable Tasks
publishing** are separate incremental-consent upgrades for an existing account.
Only a selected Calendar for which Google reports `owner` or `writer` access,
or a selected Task list under an account with the full Tasks grant, can then be
marked **Publish**. A grant for one service never unlocks the other. Blocking
Task lists and unsupported provider-publication policies are rejected locally.
Collection changes use optimistic revisions; a lost response or conflict is
reconciled with an authoritative GET before the app permits another conclusion.
Disconnect persists its exact account, revision, and idempotency key before the
request and retains them without a time-based expiry until an authoritative
snapshot proves revocation. A same-API-base DayWeave authentication repair may
rebind that record after account identity is proved, while destructive or
cross-base credential replacement stays blocked. Proven revocation keeps the
record as a completion fence until a fresh canonical pull and composition
succeeds. A strict endpoint-bound revision conflict can instead prove that an
obsolete disconnect request made no change, but the record is retired only
after an authoritative account read. If the account is already absent or
revoked, that same record remains the crash-safe canonical-composition fence.
If repaired authentication cannot
prove an absent account from the previous session, Settings offers an explicit
warning and confirmation before abandoning only that orphaned local marker.

**Refresh import** durably queues provider reconciliation and polls its status.
It does not treat HTTP `202` as completed work. The Mac persists a non-secret
request UUID before transport; replaying that UUID returns the same server
timestamp and monotonic refresh generation without queuing duplicate work.
Only an idle run whose completed generation covers the accepted generation
triggers canonical planner sync and a new composition. This proof is independent
of Mac/API/worker clock skew. The completion marker survives restart and has no
time-based expiry; it is removed only after fresh canonical pull/composition
reports success. If a response is lost, the owner can safely replay the exact
coalescing read-only request from that recovery slot. Terminal failed runs, and
authorization-required runs after authorization repair, advance to a new
persist-before-send request UUID and generation so an older run cannot clear the
marker. If the worker reports authorization-required before the account record
does, **Reauthorize** remains available without dropping that marker. Backoff,
reauthorization, conflicts,
offline state, and still-queued work remain visible without displaying provider
IDs, scopes, tokens, or raw error codes. Server cleanup fences block new OAuth
starts before a local request journal is created.

The Inbox inspector can publish a synced, app-authored event containing an owned
`dayweave_firm_block`, or a supported synced, app-authored task. A recoverably
trashed mapped event or task can instead produce a reviewed deletion. Imported,
recurring, unsupported, unsynchronized, skipped, or canceled tasks are not
offered as outbound candidates. DayWeave-only hierarchy, recurrence, split, and
scheduling metadata remains local rather than being flattened into Google
Tasks. The client first requests and displays the exact reviewed Calendar or
Tasks payload; the server redacts Calendar ownership-proof values from that
view. Nothing is queued until the owner chooses **Approve & Queue** on that
exact preview.
The client then obtains one expiring, preview-bound capability and submits the
same account, collection, item revision, and operation to the durable server
outbox. Preview, approval, and enqueue accept exactly HTTP `200`, `200`, and
`202`; identity, revision, hash, operation, entity, collection, and expiry are
validated before any local trust promotion.

The complete intent is synchronously saved in the encrypted planner snapshot
before the first request. A preview is never approved during automatic
recovery. A one-shot approval-attempt fence is persisted before requesting the
capability, so a lost response never enables a second approval ceremony or an
enqueue without that capability. An already approved capability is replayed
exactly after a timeout or restart and is cleared only after authoritative outbox acceptance. App lock
redacts the preview and fences late results. API credentials, Google account
policy, source roles, imports, and canonical cache reset remain blocked while
live recovery authority exists. Expiry checks tolerate five minutes of device
clock skew while locally elapsed authority stays non-actionable. Once authority
has expired, destructive discard waits another five-minute skew window and then
requires an explicit warning confirmation and exact-record comparison so canonical
sync cannot remain stranded. Approved-stage recovery warns that a prior enqueue
response may have been lost before the owner retries. Every recovery record is
bound to its entity kind, so a Calendar intent cannot be recovered or approved
as a Task intent, or vice versa. Google Tasks create, update/completion, and
delete use the same reviewed outbox flow. Because Google Tasks does not support
a client-selected resource identifier, a new Task is attempted only once; an
ambiguous provider result is retained for reconciliation and is never blindly
posted again.

The unified Inbox separates **Items** from **Suggestions**. Items are canonical,
encrypted local drafts: Quick Capture needs only a title, while the detailed
editor supports Inbox/Planned state, type, recurrence, constraints, hierarchy,
privacy, deletion, restore, and explicit conflict recovery. The Items lifecycle
also keeps
scheduled/running/paused rows reachable as read-only **Active** entries and
shows completed rows dimmed by default behind the persisted **Show completed**
switch, so reviewed Google Tasks updates do not disappear when execution state
changes. Suggestions fetches pending proposals and supports refresh, edit,
accept, and reject with the
proposal's optimistic `expected_revision`. Remote proposals remain a separate,
in-memory review feed. Accepting an ordinary advisory
proposal records the decision at the API and intentionally does **not** create
or mutate a schedule block. A supported
`dayweave.proposal-change-set/1` proposal instead exposes a complete exact-diff
review, explicit content-bound approval, transactional apply receipt, and
bounded undo. Its exact non-secret mutation request is persisted before send
and recovered before canonical or execution synchronization may continue.
Local suggestions and all local planning remain available when the API is
absent or offline, with the last request state shown in the Inbox.

The contained Codex assistant may also return one trailing, versioned typed-item
envelope in a completed final answer. Its parser rejects duplicate or unknown
keys, unsupported editor forms, unsafe text, non-integer numbers, invalid dates,
hidden DST-fold offsets, non-midnight all-day bounds, more than five drafts, or
an envelope above 64 KiB. The app supplies stable item and mutation identities,
forces private Inbox state, and persists pending drafts only in the encrypted
snapshot for seven days. Identity-bound monotonic deadlines and a five-minute
encrypted clock high-water checkpoint keep a quiet process or later wall-clock
rollback from silently granting another review lifetime. **Review item draft…**
opens the same complete canonical editor used by manual authoring. **Create
item** commits the exact edited create journal and accepted-suggestion linkage
atomically; no schedule or network mutation occurs until normal canonical sync.
Rejection and expiry scrub the draft body, while migrated prose-only suggestions
stay non-actionable.

The same authenticated configuration powers canonical planner sync. A sync:

1. pulls the ordered `/v1/items/delta` stream using its opaque cursor;
2. publishes durable canonical create/replace/trash/restore journals from the
   Items Inbox with stable idempotency keys, followed by retained legacy captures;
3. sends revision-guarded privacy and status replacements only when every
   canonical field can be round-tripped without loss; and
4. requests and fully validates the side-effect-free
   `/v1/schedule/preview` composition; and
5. journals the exact canonical publish body and its SHA-256 digest in the
   encrypted snapshot, then sends `POST /v1/schedule/publish` before marking
   that preview current locally. Publication accepts exactly HTTP `200` for
   both the first result and an idempotent replay.

After each foreground activation sync attempt, the Mac starts a guarded
delivery manager for the optional
`GET /v1/items/stream` invalidation channel with `Accept: text/event-stream`,
`Accept-Encoding: identity`, and the exact encrypted opaque delta cursor copied
unchanged into `Last-Event-ID`. The cursor is never decoded, numerically or
lexicographically ordered, or persisted from SSE. The parser accepts only a
standalone `: heartbeat` or one `item-invalidation` frame whose transport-safe
ASCII `id` exactly equals the sole `{"cursor":"…"}` JSON value. It applies the
same strict UTF-8, control-byte, line/frame/event bounds, redirect rejection,
content-type/content-encoding checks, bounded error body, binding checks,
single durable-auth retry, cancellation, and whole-call 330-second watchdog as
the execution stream. A 404 disables only this optional stream for the current
foreground activation; transient closes reconnect with bounded 1-to-30-second
backoff. The manager opens neither SSE nor its probe until the encrypted cursor
and binding exactly match the current connection and persistence is healthy;
this lets an already valid binding recover from a transient activation-sync
failure without exposing an in-memory-only or stale cursor.

Item events are process-local observation generations, not ordered cursor
values. A generation is settled only by a complete authoritative delta drain
that began after it was observed, or when an in-flight drain returns the exact
latest opaque hint and thereby proves that observation was covered. Scheduler
input changes are followed immediately by validated preview, durable
publication journaling, publication, and atomic proof installation. Events
arriving during that work retain a later generation for one bounded follow-up
unless that exact-cursor equality proves coverage. Cursor-only own echoes are
flushed to the encrypted snapshot without invalidating or republishing an
otherwise exact schedule. Real canonical or recurrence-input changes
synchronously revoke the durable publication proof before their single
encrypted delta commit; a failed save restores the complete in-memory preimage,
and a failed preview/publication remains queued for repair. Independently, a foreground
30-second `items/delta?limit=1` probe detects missed events. An unchanged page
does nothing and never recomposes; evidence of change starts one full bounded
delta drain from the still-durable cursor. App lock/background, configuration
or credential replacement, and foreground-service shutdown cancel item stream,
poll, drain, and any stream-originated reconciliation.

The same foreground lifetime also maintains the trusted native schedule
replica for durable device-v1 credentials. `GET /v1/schedule/current` accepts
only the exact non-cacheable `{revision,schedule}` JSON contract (including
duplicate-key rejection and the optional public manual-placement assessments),
then validates the revision label, digest, horizon, timezone, complete canonical
revision map, titles, inherited sensitivity, recurrence metadata, block
identity, overlap, and score before rendering. Installation and the endpoint's
exact typed `404 not_found` absence are separate atomic AES-GCM planner
transitions; generic errors can neither replace nor clear a projection. An
exact pending publication journal or another canonical/execution mutation
fence always wins.

Foreground activation is read-first: after recovering any genuine durable
write journal, the Mac catches up canonical items and reads the exact current
publication before starting schedule delivery. An exact authenticated `404`
is a read-only empty-replica result; activation does not compose or publish,
because the publish API has no expected-head precondition. Transient,
malformed, or binding-invalid reads retain the encrypted prior replica and
perform no schedule write. Explicit Sync, onboarding, import, and proposal
workflows keep their fresh-composition behavior.

`GET /v1/schedule/stream` carries only monotonically increasing
`schedule-invalidation` revision hints. It resumes from the revision number in
the encrypted publication proof (or canonical `0` before a first publication),
requires the exact event-stream/no-store/no-buffering headers, uses the same
bounded parser, authentication replay, 330-second watchdog, cancellation, and
1-to-30-second reconnect behavior as the item/execution streams, and never
persists an SSE value. Each hint coalesces into an authoritative current-schedule
GET, while an independent 30-second GET supplies polling catch-up. A strict
cursor-ahead `409` after an authoritative restore also recovers only through
GET. If publication wins the independent item-invalidation race, that same
fenced drain catches up `/v1/items/delta`, refetches the current immutable head,
and installs it without waiting for the next poll.

Canonical items, tombstone revision watermarks, the delta cursor, durable
pending/conflicted edits, per-session recurrence outcomes, and rendered blocks
live in the schema-v10 AES-GCM encrypted planner snapshot. Schema-v1 through v4
snapshots are migrated once with explicit legacy sensitivity defaults; schema
v5 remains sensitivity-strict and migrates with no invented privacy edit.
Schema-v6 retained privacy edits migrate as conservatively submitted because an
older snapshot cannot prove that no request bytes were sent. Schema v7 adds no
invented publication when it migrates; schema v8 retains the bounded exact
publication body, accepted preview, configuration binding, and idempotency
UUID needed after a crash. Schema v9 adds proposal-application recovery and
content-free receipts; schema v10 adds canonical authoring journals, recent
deletions, and canonical selection. Older binaries reject newer snapshots
instead of rewriting away new state. Recently Deleted keeps 30 days and at
most 500 metadata records; full item bodies are retained newest-first within
per-item and aggregate byte budgets, while independent tombstone watermarks
continue preventing stale resurrection. A sibling-file
lock and ciphertext compare-and-swap revision stop a second app process from
silently overwriting a newer snapshot. Unknown future
item fields and nested split-policy fields are
retained and make that item read-only instead of being silently discarded.
Decoded arbitrary JSON numbers are conservatively marked as server-originated,
and that read-only provenance survives encrypted save/restore even if
Foundation normalizes a token such as `1.0` or `1e2`. A stale cursor is
recovered by staging a complete, resource-bounded delta before replacing the
cache. Network, contract, and revision failures keep recoverable local intent
and are shown in the Today diagnostics. Conflicted edits remain encrypted and
can be explicitly rebased from the selected block after a fresh preview.
Quick Capture trims titles, enforces the API's 500-Unicode-scalar limit, and
durably encrypts the complete canonical draft before it appears saved. Invalid
legacy captures are still skipped individually, kept locally with a persistent
diagnostic, and can be edited or deleted in the inspector. Create/privacy/status
pushes resume across syncs after bounded per-run request caps; stability hints are
trimmed deterministically to the API's assignment and block-count limits.
Status publication for an item waits behind that item's complete privacy-edit
chain, so a bounded privacy run cannot strand the final choice on a stale
revision.

Schedule publication is a local two-phase boundary. Validation and rendering
happen in memory first; the exact request is durably flushed before its first
byte is sent. A timeout, cancellation, process death after remote commit, or
lost response leaves that journal intact. The next planner sync replays the
same body and UUID before pulling or mutating anything else, validates the
published revision/digest/horizon/timezone response, and atomically clears the
journal. A non-replayed result installs the candidate in that same local
commit. An idempotent replay can name a remotely superseded revision, so it
never installs the retained candidate: the client keeps the prior plan
non-current and makes exactly one fresh composition/publication attempt in the
same sync. Only the server's exact JSON `schedule_publication_stale` conflict
envelope proves that a candidate was not published; it is cleared atomically
and permits only one fresh retry. A second stale result is cleared and surfaced
without an unbounded loop. Configuration replacement and local cache reset
remain blocked
while an exact result is ambiguous, with Settings directing the user to restore
the original configuration and authentication and run Planner sync. A failed
publication never marks the candidate preview current.

Quick Capture, the canonical item editor, and legacy-capture recovery expose an
own-item **Sensitive** marker.
For canonical items, the inspector distinguishes a marker set on that item from
effective sensitivity inherited through an ancestor. Privacy edits are stored
as encrypted, revision-bound intent with explicit conflict recovery and stable
idempotency keys. The attempted state is flushed before transport; a submitted
replacement cannot be canceled or inverted until its exact outcome is observed
or replayed, and a changed user choice is retained as a follow-up replacement.
Marking an item hardens its block and Codex redaction boundary immediately; if
either stage of an ambiguous submitted/follow-up chain marks the item, that
redaction fence remains active until the chain is reconciled.
Removing a marker can change canonical-content eligibility only after server
acceptance is validated: either an exact base-plus-one replacement response
with complete mutable-field equality, or an authoritative later canonical
revision that reconciles a submitted request whose response was lost. The
rendered block remains shielded until a sensitivity-consistent preview is
applied. Status replacements require the exact response validation, including
hierarchy fields. An inherited marker cannot be overridden on a child. The
locked main window, Settings, and menu-bar surfaces continue to expose no
schedule content.
The Items Inbox is the general browser and typed editor for both unscheduled
and Planned canonical items, including own/inherited privacy presentation,
queued changes, conflicts, and bounded Recently Deleted recovery. Sensitive
titles and notes stay privacy-marked while editing, including through pending
hierarchy changes. Quick Capture is also available as an independent window,
so its menu-bar command remains usable after every main planner window closes.

The seven-day preview validates the server's complete `source_item_revisions`
map and performs a bounded delta-plus-preview retry if it raced a write. It uses
planned and pinned blocks as placement-stability hints, and pins an assignment
group only when the entire group is fully inside the current freeze horizon.
This prevents a prior freeze-generated `pinned` result from remaining pinned
forever. Today shows the current day; Calendar exposes later preview days.
Unscheduled, rejected, ignored, decision, violation, and conflict details are
available without truncation in the diagnostics disclosure.

Cached previews are executable only after validation during the current app
launch and only while their API configuration, item revisions, local time zone,
generated day, freshness window, and schedule horizon still match. Changing
the API origin requires a replacement token and invalidates the preview before
the next request. A separately confirmed reset is available when intentionally
moving this Mac to a different canonical server; it does not delete server data
or local-only captures.

## Verification

Use a full Xcode Swift toolchain to build and execute the test bundle:

```sh
swift build --package-path apps/macos -Xswiftc -warnings-as-errors
swift test --package-path apps/macos -Xswiftc -warnings-as-errors
```

On the current Command Line Tools-only development host, plain `swift test`
does not provide a valid executable test result: depending on the invocation,
guarded Swift Testing bodies may be omitted, or the linked runner cannot load
`Testing.framework`. A successful link is therefore **not** a test pass. Run
the repository workaround instead:

```sh
./scripts/test-macos.sh -Xswiftc -warnings-as-errors
```

It uses an isolated copy of the CLT framework, removes only the dangling
cross-import overlay when necessary, adds the runtime search path, and executes
the test bodies without modifying the installed toolchain. Debug and release
warnings-as-errors builds, diff whitespace, `Info.plist`, and temporary ad-hoc
app signing checks are also available on this host. The current tree does not
pass `swift-format lint`, so formatting is not claimed as a verification result.

The API tests use a deterministic `URLProtocol` transport and injected token
stores, so they require neither a live server nor access to the user's
Keychain. Coverage includes contract decoding, authenticated request shape,
revision-guarded actions, structured errors, origin-bound credential lifecycle,
interrupted configuration updates, legacy-token refusal, configuration separation,
offline behavior, restore-failure mutation gating, and the invariant that a
legacy remote approval leaves the schedule unchanged. Transactional proposal
coverage separately exercises exact preview approval, encrypted apply/undo
journals, lost-result recovery, content-free receipts, shared mutation fences,
and canonical reconciliation. Canonical coverage adds stale
cursor and multipage recovery, tombstones, credential snapshots, exact integer
JSON, fail-closed replacement, conflict retention, recurrence correction and
rollup, conservative pinning, transitive hierarchy order, encrypted schema
migration, revision-map retries, and preview rendering.
Additional regressions cover scoped IPv6 configuration, full base-path binding,
invalid-capture recovery, mutation/assignment caps, malformed mutation results,
preview overlap and score validation, overnight blocks, recurrence-history
pruning, stale multi-process snapshot writers, exact schedule-publication
replay after restart, publication-response rejection, and auth-v2 scope
re-enrollment fencing.
