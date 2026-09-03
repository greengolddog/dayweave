# DayWeave API

The Rust API owns canonical PostgreSQL state, authenticated REST/SSE surfaces,
Google integration orchestration, and durable external-effect approval. Product
and deployment contracts remain in
[the architecture](../../docs/architecture.md) and
[integration setup](../../docs/setup-integrations.md).

## Generated firm-schedule Google publication

The first milestone toward `SCH-006` is a server-first, explicitly approved
batch publisher. It accepts the exact current immutable published schedule and
one selected writable Google Calendar. It creates or updates only
not-yet-elapsed generated firm `planned` and `pinned` blocks. It does not
publish imported/external fixed blocks; exact elapsed instances can only be
no-ops, and tentative blocks remain app-only with no publication path.

The feature is disabled by default. Starting new publication work requires both:

```text
DAYWEAVE_GOOGLE_OUTBOUND_ENABLED=true
DAYWEAVE_GOOGLE_SCHEDULE_OUTBOUND_ENABLED=true
```

The schedule-specific flag is invalid unless the general Google outbound gate
is also enabled. It shares the bounded
`DAYWEAVE_GOOGLE_OUTBOUND_APPROVAL_TTL_MINUTES` lifetime. Do not enable either
write gate before OAuth identity-key continuity, PostgreSQL encryption/access,
backups, monitoring, and an isolated Google test Calendar are ready.

The REST contract is intentionally explicit:

1. `POST /v1/integrations/google/accounts/{account_id}/schedule-publications/previews`
   binds `collection_id` and `expected_schedule_revision_id` to an expiring,
   review-safe create/update/delete/no-op batch and preview hash.
2. `POST .../schedule-publications/previews/{preview_id}/approve` accepts that
   exact hash and returns one expiring approval capability exactly once. The
   server persists only its hash.
3. `POST .../schedule-publications` consumes the capability with the exact
   preview, collection, and schedule revision. HTTP `202` means durable queue
   acceptance, not completed Google delivery; an exact replay returns the same
   receipt.
4. `GET .../schedule-publications/{publication_id}` returns content-free
   aggregate delivery state. Its `pending_count` includes both first-attempt
   work and retryable work waiting in durable backoff; `delivering_count` is
   reported separately.

Preview admission is serialized per Google account. The server reuses the
newest still-live, unconsumed preview only after revalidating every stored
change against the current schedule and mapping state. Each account is limited
to eight active unconsumed previews and 20,000 active change rows; exceeding a
limit returns HTTP `429` without storing another preview. Expired, unconsumed
preview payloads with no publication batch are pruned while approval audit
records are retained. The exact serialized review response is capped at 16 MiB
before a new preview is persisted, matching both native transports; an
oversized projection fails closed as HTTP `502`.

Mutation calls require Google write scope, `schedule_read`, a native-device
principal, and the service's exact user/workspace binding. Status requires
Google read scope with the same device and binding checks. Schedule-publication
responses are `no-store`.

Logical slot identity uses workspace + item + occurrence + session index and
does not include the placement-dependent schedule block UUID. A rescheduled
session therefore updates its existing event. Private provider identity adds
account, Calendar, and incarnation binding so an old deleted event cannot be
silently adopted as a new slot. A missing future slot is eligible for reviewed
conditional deletion. Elapsed events are immutable Calendar history and are
never rewritten, deleted, or reused; later reuse of the logical slot advances
the incarnation and receives a distinct provider event ID.

Google receives a confirmed, opaque, private, attendee-free timed event. A
sensitive title is replaced with `Busy`; only a bounded title is sent for other
blocks. Descriptions, locations, recurrence, conference data, attachments, and
raw DayWeave identifiers are omitted. Reminders are explicitly disabled with
`reminders.useDefault=false` and no overrides. Create, update, and delete calls
use `sendUpdates=none`, suppressing attendee notifications.

The Google sync worker delivers non-no-op changes from durable PostgreSQL work.
It fences every send with the approved intent, current schedule and collection,
mapping ETag, parent-run generation, lease, and dispatch nonce. Deterministic
create IDs permit GET-and-adopt recovery only for a complete authenticated
match; update/delete use `If-Match`. Backoff, partial completion, conflicts,
supersession, and lost-response observations remain durable and visible through
aggregate status.

Time is rechecked when work is claimed and again in the final dispatch
transaction. For updates and deletes, the deadline is the earlier of the
reviewed desired end and the immutable mapped event end. Definitely unsent work
that has elapsed is superseded; work with possible-send evidence may perform
read-only reconciliation but can never initiate a new write after that
deadline. An unusable identity or oversized success response remains active in
backoff and continues to fence successor publication until exact reconciliation.

If a create may have reached Google but an authenticated read still does not
find its deterministic ID, elapsed time or repeated negative reads never prove
that the write had no effect. That work remains durably unresolved in bounded
backoff (at most one hour between probes), blocks later publication for that
selected Google account/calendar target, and requires a later positive
authenticated observation. This checkpoint exposes no schedule-specific
operator-reconciliation API or supported database-intervention runbook.

No native client invokes this batch contract yet, no scheduler or firm-horizon
automation enqueues it, and inbound edits, moves, or deletions of its generated
Google events are not interpreted. Tentative blocks remain app-only as
required, and `SCH-006` remains open.
