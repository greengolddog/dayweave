# Item invalidation and delta sync

`GET /v1/items/delta` is the authoritative item synchronization endpoint.
`GET /v1/items/stream` is only a near-real-time, content-free invalidation
channel that tells a client to drain the delta endpoint sooner than its normal
poll interval. An SSE frame is never an item mutation and never authorizes a
client to advance its durable cursor.

## Request contract

The stream accepts only native REST audiences (`device` and the legacy personal
token during its rollout window) carrying `items_read`. Native and OAuth MCP
audiences are rejected by the common REST authentication boundary.

Every request must send exactly one:

```http
Accept: text/event-stream
```

The value uses the normal case-insensitive media-type comparison, but media
ranges, comma-separated alternatives, parameters, and duplicate `Accept`
fields are rejected with `406`.

On reconnect, a client sends the exact opaque cursor from the last item delta
page that it applied and persisted in the same encrypted local transaction:

```http
Last-Event-ID: RFdJMQ...
```

The token is opaque. Clients must not decode it, derive ordering from it, or
replace it with an SSE-only value. Server-issued cursors are canonical,
single-line, transport-safe ASCII without whitespace, controls, quotes, or
backslashes, and are bounded to 256 bytes; clients may enforce that lexical
safety bound while otherwise retaining the token byte-for-byte. Omitting the
field means the initial internal sequence zero. Empty, noncanonical, damaged,
duplicate, or wrong-workspace values return `400`. A valid cursor beyond the
current durable item-change head returns `409`. The existing delta endpoint
continues to report its malformed/unsupported query cursor as `422`.

## Response and privacy contract

An invalidation contains one opaque cursor. The SSE ID and the sole JSON value
must agree exactly:

```text
id: RFdJMQ...
event: item-invalidation
data: {"cursor":"RFdJMQ..."}

```

Frames never contain items, tombstones, user/workspace identifiers, item IDs or
revisions, hierarchy, recurrence, titles, notes, sensitivity, status, timing,
or other user content. Notifications are coalesced and the cursor is only a
hint that durable changes exist; it is not a delta page.

Heartbeat comments contain no data and normally arrive every 15 seconds:

```text
: heartbeat

```

Successful responses use `Content-Type: text/event-stream`,
`Cache-Control: no-store, no-cache`, `Pragma: no-cache`, and
`X-Accel-Buffering: no`. Each connection ends after about five minutes so
credentials and network state are periodically revalidated. Per-process stream
capacity defaults to 32; exhausted capacity or an unavailable durable head
returns `503` before streaming starts.

## Atomic delta pages and delivery bounds

The delta request `limit` (1 through 200) is a target, not an absolute response
count. Direct native item transactions and proposal apply/undo transactions
give all of their direct item changes and implicit old/new-parent refreshes one
transaction-local change-group ID. A delta page never ends inside such a group.
This prevents a durable cursor from representing a hierarchy or dependency
state that never committed on the server. Google projection batches assign one
separate group per changed canonical item and its parent refreshes, so a large
provider page remains incrementally drainable without exposing a partial item
aggregate.

One proposal contains at most 100 commands and one command can produce at most
three item-change rows. A group is therefore limited to 300 rows. A response
can contain at most `requested limit - 1 + 300` changes: 499 at the public
maximum request limit, 349 for the native foreground limit of 50, and 300 for a
one-row probe. Clients must accept this bounded expansion rather than rejecting
a valid response merely because it contains more changes than requested.

Both each group and the complete selected page are limited to 8 MiB of compact
serialized change payload, leaving headroom within the native 12 MiB and 16 MiB
HTTP/decode envelopes for JSON structure and cursors. The server may stop a
page before its requested count at an independent row or group boundary to
respect that byte ceiling. A valid unit always makes progress. Write paths
check group count and payload before commit; reads independently fail closed on
an oversized, discontinuous, or cursor-split stored group rather than emitting
an undrainable or partial response. Proposal previews additionally reserve 1
KiB per simulated row for bounded timestamp growth before a later apply or
undo, while committed transactions retain the exact 8 MiB check.

The dependency-authority cutover leaves pre-cutover rows with a null group ID
readable as legacy history. A database trigger rejects every post-cutover
`item_changes` insert without a group, so an older queued writer fails closed
instead of publishing a partial or stale projection.

## Delivery and recovery semantics

### Bounded current-state bootstrap

A cold client or explicit cursor-replacement recovery opts in with
`GET /v1/items/delta?bootstrap=current&limit=200`, with no `cursor`. Plain
cursorless requests retain the legacy historical stream; old clients and
ordinary incremental cursors do not change behavior. Unsupported bootstrap
values, or combining the mode with a cursor, fail closed. Upgrade the server
before deploying clients that use this mode: a new cold client does not fall
back to lifetime history after a generic error from an older server.

The response remains exactly `changes`, `next_cursor`, and `has_more`. The
snapshot contains every nontrashed current item, including completed and
cancelled work, plus bodyless tombstones inside a fixed server-clock 720-hour
recovery window. Older tombstones are omitted only from this replacement view;
the historical stream is not pruned. Local restore/replay journals retain their
minimum recovery evidence even if an old deletion is absent from the snapshot.

PostgreSQL migration `0034` adds short-lived, workspace/owner-scoped immutable
manifests referencing exact canonical change sequences, not copies of item
payloads. Capture joins current item revisions to their unique change rows
under the canonical workspace lock. A fixed head and cutoff make every page
consistent across concurrent writes. Scoped foreign keys, original-transaction
membership and deferred completeness checks seal each manifest. Live manifests
pin their source rows against mutation. Account-deletion fences and the guarded
purge inventory include both manifest tables.

Each snapshot is bounded to 20,000 records and 32 MiB, with at most 300 records
and 8 MiB of compact change payload per page. This is a replacement batch, not
a fabricated historical atomic change group; all snapshot pages must be
buffered before admission. Native aggregate limits remain unchanged and may
reject a snapshot that exceeds their own retained-state budget. Tickets expire
after ten minutes, with at most sixteen live tickets per workspace; repeated
initial reads reuse a valid owner/workspace/head ticket instead of consuming
capacity for lost replies. Expired manifests are cleaned during a subsequent
capture, or by the scoped account purge.

Intermediate cursors identify a manifest and ordinal. Continue using only the
returned `cursor`, without repeating `bootstrap=current`. They are opaque and
must not become a durable complete-cache cursor or an SSE `Last-Event-ID` (the
stream rejects them with `400`). Only the terminal page returns an ordinary
delta cursor at the captured head. The next incremental drain returns all
changes committed after that head, including changes made while the snapshot
was downloading.

Missing/expired tickets return `409 item_bootstrap_expired`; oversized snapshots
return `413 item_bootstrap_too_large`; ticket exhaustion returns
`503 item_bootstrap_capacity`; malformed/wrong-scope snapshot cursors or mode
combinations return `422 item_bootstrap_cursor_invalid`. Failed or incomplete
downloads never replace the encrypted offline cache. A retry may start a new
snapshot; it must not concatenate pages from different tickets.

Both clients fold the complete terminal read before replacing current state.
macOS preserves submitted authoring retry eligibility and same-deletion local
retention anchors. Android carries bodyless deletion evidence through the
existing encrypted trash store, including canonical refresh and historical
receipt recovery. Before staging a new schedule publication, any such evidence
is durably installed with the complete preflight; it is not added to the exact
serialized publication journal. This preserves recovery across a restart even
when the first successful publication response is not an idempotent replay.

Verification on 2026-09-09:

- PostgreSQL-backed API gates passed 568 default tests plus all 29 explicitly
  opt-in database tests, with zero failures. All-target API Clippy passed with
  warnings denied. The focused fixture has 35,014 historical revisions and a
  5,000-item current chain plus one recent tombstone delivered in seventeen
  pages. It covers concurrent reparent/delete, repository restart, lossless
  post-head catch-up, corrupted source evidence, deferred manifest sealing,
  scoped membership, history pinning and expiry cleanup. Separate tests cover
  real count/byte/page bounds and ticket scope/tampering/capacity/reuse.
- The macOS full runner reported 1,011 tests in 65 suites with no failures and
  one unrelated opt-in progress test skipped. The new opt-in bootstrap test ran
  against a real loopback PostgreSQL-backed item service: the actual native
  loader admitted all 5,000 levels and bodyless trash, preserved the encrypted
  state across restart and resumed an empty ordinary delta at the same head.
  Unit coverage additionally checks interrupted downloads, stale cursorless
  cache replacement, submitted journal custody and retention anchors.
- Android passed 1,618 JVM tests in 130 suites, with one unrelated opt-in skip;
  lint reported zero errors and 29 existing warnings. Coverage includes a
  deepest-first 5,000-level chain, a separate 300-sibling page, terminal-only
  installation, failure custody, historical receipt/deletion contradictions,
  bodyless restore recovery and encrypted publication restart. These are JVM
  transport/store checks, not an APK/emulator or physical-device acceptance run.

The real native fixture is deliberately item-only. It does not verify schedule
composition, completion-cascade writers, production TLS/device authentication,
Google or assistant integrations, or owner-device acceptance. No owner data,
credentials or provider accounts are used. The guarded native test consumes a
private `DAYWEAVE_NATIVE_BOOTSTRAP_CONFIG`; its synthetic secrets and database
artifacts are never repository inputs.

### Incremental invalidation

After a direct `ItemService` create, replace, trash, or restore returns a
successful commit or exact replay, it performs a content-free process-local
poke. The poke does no repository I/O, so a successfully committed mutation
cannot be turned into an error by notification delivery. Every woken stream
re-reads the durable item-change head before emitting; a replay with no new head
therefore remains silent. Failed validation, concurrency, authorization, and
repository operations do not poke the hub.

Opening a stream subscribes before reading the authoritative head. A commit
during that read is consequently either visible in the read or retained as a
pending wake. The coalescing hub is deliberately process-local, not a durable
broker. Each open stream also probes the shared item-change head at least every
five seconds. That recovers Google projection, proposal transaction, direct
database, other-process, and lost-local-wakeup changes without coupling those
writers to an in-memory publisher. A failed probe after HTTP 200 ends the
content-free stream; the client reconnects with bounded backoff.

A client should:

1. open the stream with its last durably applied delta cursor;
2. treat any valid invalidation only as a request to synchronize;
3. call `/v1/items/delta` with its durable cursor and apply pages until
   `has_more` is false;
4. atomically apply the fully buffered drain and store its terminal `next_cursor`; and
5. use only that stored delta cursor as `Last-Event-ID` on reconnect.

Clients may keep their existing bounded item-delta poll as a fallback. A `404`
during a mixed-version rollout may disable stream attempts for the current app
activation without disabling polling. A `400`/`409` requires explicit binding
or rebootstrap recovery rather than silently replacing encrypted local state.
The content-free stream itself adds no database schema or migration.

## Historical authoring receipts

A successful create, replace, trash or restore receipt belongs to its exact
submitted journal, not necessarily to the newest canonical revision. After a
lost reply or restart, another writer may already have changed the item's
content, lifecycle, privacy or deletion state. Newer cache divergence alone is
not proof that the original operation failed. Submitted requests retain their
original body, revision, idempotency key and configuration binding for exact
replay; fresh-edit preflight must not turn that replay into a new write or a
conflict merely because the current item has advanced.

Receipt admission still checks operation identity, reviewed content, deletion
shape and the operation's successful revision. Equal-revision contradictions fail
closed. When admitted active, trash or tombstone evidence is newer than a valid
historical receipt, settling the journal preserves that newer state. It must
not reselect obsolete content, revive a deleted item, downgrade privacy, or
promote an old onboarding designation into current canonical evidence. A
local-only designation that depended on the settled create journal is cleared
instead. Failed encrypted persistence restores both the exact pending journal
and its previous projections.

Android also repairs a legacy preflight-cache ambiguity: an older pending
replace/trash base could be retained while the saved delta cursor advanced past
newer server changes. An already-submitted request therefore replays before
fresh-edit preflight. Its historical item must not replace current data. A
bounded full canonical rebuild repairs the uncertain baseline before new
authoring or publication. Keep the exact submitted journal, including its
privacy protection, until the admitted fresh evidence and receipt settlement
can be saved together. If rebuild fails, the original request remains available
for exact replay after restart. Do not fabricate canonical sensitivity
fields/revisions or remove another saved intent to hide uncertainty.
If the refreshed hierarchy cannot represent a remaining saved draft, retain
all affected journals rather than partially installing the refresh or silently
rewriting that draft.
The existing hydration limits remain in force. Cold historical-receipt recovery
uses the bounded current-state bootstrap described above; it does not implement
server parent-completion cascades or their policy controls.

A delayed equal-revision trash receipt must preserve the earliest local
retention anchor. Server deletion timestamps may be ahead of the local clock,
so reclamping the same receipt to a later observation must not extend the
trash retention period.

These semantics are a prerequisite for future derived ancestor-completion
updates; they do not implement those updates. See
[parent completion](hierarchy-completion.md) for the remaining integration.

Verification on 2026-09-09: the warnings-denied macOS wrapper reports 1,004
tests across 64 suites passing; Android's full JVM gate reports 1,609 tests
across 130 suites with no failures or errors. Android lint passes with zero
errors and 29 warnings. Each default native gate leaves
its opt-in controlled-service phase skipped. Coverage includes lost responses,
newer active/deleted state, exact replay through encrypted restart, own and
inherited privacy, dependent-journal custody, equal-revision contradictions,
local-only onboarding-anchor recovery and trash retention. The dedicated
restore test checks repository/serializer restart; separate manager tests
exercise synthetic encrypted-disk restart. These gates do not establish a new
live-service/two-device acceptance run or final macOS/APK release acceptance.
