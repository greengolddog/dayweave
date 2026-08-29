# DayWeave for macOS

The native SwiftUI client keeps the local planner usable without a server. A
new production profile starts with an empty plan and restores the encrypted
snapshot synchronously before exposing actions; preview fixtures are used only
by `PlannerStore.preview` and tests.

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
entered same-origin base path and never a replacement tuple. Older pending
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

Suggestions, canonical cache/cursors/mutations, and execution recovery are
fenced to the authentication binding as well as the URL. Replacing a session at
the same origin therefore quarantines stale work instead of sending it under a
new identity. Legacy raw-token records have no trustworthy origin and require
explicit re-entry. Credentials are device-only and are never added to the
Codable planner snapshot or application diagnostics.

The Inbox fetches pending proposals and supports refresh, edit, accept, and
reject with the proposal's optimistic `expected_revision`. Remote proposals
remain a separate, in-memory review feed. Accepting one records the decision at
the API and intentionally does **not** create or mutate a schedule block. Local
suggestions and all local planning remain available when the API is absent or
offline, with the last request state shown in the Inbox.

The same authenticated configuration powers canonical planner sync. A sync:

1. pulls the ordered `/v1/items/delta` stream using its opaque cursor;
2. publishes local Quick Add captures with stable idempotency keys;
3. sends revision-guarded privacy and status replacements only when every
   canonical field can be round-tripped without loss; and
4. requests the side-effect-free `/v1/schedule/preview` composition.

Canonical items, tombstone revision watermarks, the delta cursor, durable
pending/conflicted edits, per-session recurrence outcomes, and rendered blocks
live in the schema-v7 AES-GCM encrypted planner snapshot. Schema-v1 through v4
snapshots are migrated once with explicit legacy sensitivity defaults; schema
v5 remains sensitivity-strict and migrates with no invented privacy edit.
Schema-v6 retained privacy edits migrate as conservatively submitted because an
older snapshot cannot prove that no request bytes were sent. Older binaries
reject schema v7 instead of rewriting away new state. A sibling-file
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
Quick Add trims titles and enforces the API's 500-Unicode-scalar limit. Invalid
legacy captures are skipped individually, kept locally with a persistent
diagnostic, and can be edited or deleted in the inspector. Create/privacy/status
pushes resume across syncs after bounded per-run request caps; stability hints are
trimmed deterministically to the API's assignment and block-count limits.
Status publication for an item waits behind that item's complete privacy-edit
chain, so a bounded privacy run cannot strand the final choice on a stale
revision.

Quick Add and local-capture recovery expose an own-item **Sensitive** marker.
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
Canonical privacy authoring currently starts from a selected rendered block;
an unscheduled canonical item has no general item-browser editor yet. Any
already-durable privacy conflict remains visible and recoverable from Today
diagnostics even when the item has no rendered block.

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
remote approval leaves the schedule unchanged. Canonical coverage adds stale
cursor and multipage recovery, tombstones, credential snapshots, exact integer
JSON, fail-closed replacement, conflict retention, recurrence correction and
rollup, conservative pinning, transitive hierarchy order, encrypted schema
migration, revision-map retries, and preview rendering.
Additional regressions cover scoped IPv6 configuration, full base-path binding,
invalid-capture recovery, mutation/assignment caps, malformed mutation results,
preview overlap and score validation, overnight blocks, recurrence-history
pruning, and stale multi-process snapshot writers.
