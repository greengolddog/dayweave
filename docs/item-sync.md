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

## Delivery and recovery semantics

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
4. atomically apply each page and store that page's `next_cursor`; and
5. use only that stored delta cursor as `Last-Event-ID` on reconnect.

Clients may keep their existing bounded item-delta poll as a fallback. A `404`
during a mixed-version rollout may disable stream attempts for the current app
activation without disabling polling. A `400`/`409` requires explicit binding
or rebootstrap recovery rather than silently replacing encrypted local state.
The endpoint adds no database schema or migration.
