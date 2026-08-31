# Execution invalidation stream

`GET /v1/execution/stream` is the near-real-time invalidation channel for the
canonical execution lease. It is an authenticated Server-Sent Events endpoint,
not a second execution read model. Clients continue to fetch
`GET /v1/execution` for the authoritative snapshot.

## Request contract

The endpoint accepts only native REST audiences (`device` and the legacy
personal token during its rollout window) carrying `execution_read`. Native or
OAuth MCP audiences are rejected by the common REST authentication boundary.

Every request must send:

```http
Accept: text/event-stream
```

The value is intentionally strict (with the normal case-insensitive media-type
comparison): media ranges, comma-separated alternatives, parameters, and
duplicate `Accept` fields are rejected with `406`.

On reconnect, send the last execution revision that the client has applied to
its encrypted local store:

```http
Last-Event-ID: 42
```

The value must be canonical unsigned decimal: `0`, or a nonzero value without a
leading zero. Signs, whitespace, fractions, duplicates, empty values, and
overflow are rejected with `400`. Omitting the field means cursor `0`, so a
fresh client immediately receives a coalesced invalidation when any execution
revision already exists. A cursor beyond the authoritative repository head is
rejected with `409`; the client must recover through an explicit execution
snapshot rather than treating a server rollback or wrong-environment cursor as
ordinary progress.

## Response and privacy contract

A revision notification contains exactly one revision in the data object and
the same decimal value as the SSE event ID:

```text
id: 43
event: execution-invalidation
data: {"revision":43}

```

Frames never contain lease, session, item, occurrence, block, device, title,
status, timing, or other user-content fields. Revisions are coalesced: a client
may receive revision `43` after revision `40` without separate frames for every
intermediate commit. The revision means “refresh to at least this head,” not
“apply this event as a mutation.”

Heartbeat comments contain no data and normally arrive every 15 seconds:

```text
: heartbeat

```

Successful responses use `Content-Type: text/event-stream`,
`Cache-Control: no-store, no-cache`, `Pragma: no-cache`, and
`X-Accel-Buffering: no`. Each connection ends after about five minutes so
credentials and network state are periodically revalidated. Clients reconnect
with bounded backoff and their last durably applied revision. Per-process stream
capacity is bounded; temporary exhaustion returns `503` before streaming starts.

## Delivery and recovery semantics

The execution service advances a monotonic, process-local wakeup high-water only
after its repository returns a successfully committed or exactly replayed
mutation. Validation, authorization, concurrency, and repository failures do
not publish an invalidation. Opening a stream subscribes to this wakeup before
reading the authoritative head, closing the local subscribe/read race.

The wakeup hub is deliberately process-local; it is not presented as a durable
broker. While a connection is open, the stream probes the shared execution
repository at least every five seconds. That probe recovers a commit made by a
different API instance and the narrow case where a request is canceled after a
database commit but before its local publisher runs. The repository revision is
the durable catch-up source in all cases.

A client should:

1. open the stream with its last durably applied execution revision;
2. on revision `R`, fetch `GET /v1/execution`;
3. require the snapshot revision to be at least `R`;
4. atomically store the snapshot and its revision; and
5. use that stored revision as `Last-Event-ID` after reconnect.

Clients that do not use the stream remain compatible and may continue bounded
polling. The endpoint adds no database schema or migration.
