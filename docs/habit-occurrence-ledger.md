# Habit occurrence ledger

DayWeave treats a habit definition and an occurrence outcome as different facts. An item owns the
habit policy; a successfully published schedule admits immutable occurrence evidence; native
clients may then record and correct outcomes against that evidence. A client-supplied UUID can
never create an occurrence.

## Authority and identity

Migration `0026_habit_occurrence_ledger.sql` stores each admitted occurrence with:

- a server ledger `id`, which is the only UUID accepted by the outcome route;
- the scheduler's `planner_occurrence_id`, used only to join schedule and habit views;
- the first source schedule revision and exact source item revision;
- a SHA-256 fingerprint of recurrence-affecting policy;
- the full recurrence identity and nominal/window instants;
- a local-date and IANA-timezone snapshot, including DST-safe date derivation for rolling
  occurrences;
- scheduled duration and optional quantity target/unit.

Authoritative evidence uses the scheduler core's exact tagged recurrence
identity union: `calendar_day`, `calendar_week`, `calendar_month`,
`rolling_minutes`, `after_completion`, `rolling_month`, or `custom_rule`.
The legacy `custom` placeholder is readable only inside old move envelopes and
is never valid as newly admitted habit evidence. The planner occurrence ID and
custom-rule ID must use UUID-v5 with the RFC 4122 variant. Identity JSON must
equal the core's canonical typed serialization, including canonical base-10
integers and RFC 3339 anchors; value-equivalent alternate wire spellings are
not accepted. Every evidence envelope binds its nominal start to the recorded
local date and canonical server-supported IANA timezone; calendar and
custom-rule identities additionally bind their embedded period/date to the
exclusive nominal interval. Rolling identity fields retain the core's exact
shape and globally provable bounds; full policy-aware identity and UUID-v5
proof happens when the core creates the published schedule.
Calendar bucket ordinals and rolling-month indexes are limited to `0...65534`,
custom-rule sequences to `0...9999`, rolling indexes to the unsigned 32-bit
range, and rolling-month cycles to `0...2147483647`. Identity anchors must be
RFC 3339 instants in years `0001...9999` with no precision beyond PostgreSQL's
microseconds and offsets no larger than `±18:00`. The four evidence envelope
instants use the same year and precision bounds. Evidence local dates remain
inside the supported `1900...2200` habit horizon.

Schedule publication writes this evidence in the same transaction as schedule blocks and the
private result snapshot. Reusing a planner identity with different policy, identity, time window,
duration, or target rejects the entire publication. An exact re-publication is a no-op for existing
evidence: it does not advance the habit delta or invalidate another schedule.

Preview composition replaces caller-provided completion, skip, completion-anchor, and pause state
for persisted habits with the authoritative ledger projection. The ledger change head is included
in the private input digest and rechecked while publication holds the canonical item lock, so an
outcome or pause committed after preview makes publication stale. Explicit move exceptions remain
user scheduling inputs. Because completion IDs do not encode their owning item, the server derives
the current habit-owned set from recurrence materialization: caller lifecycle claims are removed
only for those habit occurrences, while legitimate recurring-task completions remain intact.
Ledger outcomes whose stable IDs are no longer generated after a recurrence edit are ignored;
unchanged stable IDs survive unrelated item revision bumps. For timed habits, authoritative partial
progress is hydrated for the exact planner occurrence and reduces only that occurrence's remaining
duration using integer, ceiling-rounded basis-point arithmetic. Quantity-only habits retain partial
evidence without inventing a time estimate.

## REST contract

All routes are under `/v1`, require the normal native/legacy bearer audience, reuse `items_read` or
`items_write`, and return `Cache-Control: no-store`.

- `GET /habits/{habit_id}/occurrences?start_date=&end_date=&cursor=&limit=`
- `PUT /habits/{habit_id}/occurrences/{occurrence_id}`
- `GET /habits/occurrences/delta?cursor=&limit=`
- `GET /habits/stream`
- `POST /habits/{habit_id}/pauses`
- `POST /habits/{habit_id}/pauses/{pause_id}/resume`
- `GET /habits/{habit_id}/analytics?start_date=&end_date=&bucket=day|week|month`

An occurrence PUT is create-or-correct and always returns HTTP 200:

```json
{
  "operation_id": "00000000-0000-0000-0000-000000000001",
  "expected_revision": 0,
  "outcome": {
    "status": "partial",
    "progress_basis_points": 5000,
    "quantity": 10,
    "unit": "pages",
    "actual_seconds": 900,
    "note": "optional private note",
    "occurred_at": "2026-09-04T12:00:00Z"
  }
}
```

`Idempotency-Key` is required for every mutation. Keys are 8–128 URL-safe ASCII characters and
are stored only as hashes. `operation_id` is also globally one-use within the workspace. An exact
retry returns the original value with `replayed: true` and `Idempotency-Replayed: true`, even after
the item is later removed. Reusing either identity for different content returns HTTP 409.

Outcome constraints are:

- `unresolved`: exactly 0 basis points and no quantity, time, or note evidence;
- `partial`: 1–9999 basis points;
- `completed`: exactly 10000 basis points;
- `skipped`: 0–9999 basis points and may retain partial quantity/time/note evidence.

Quantity and unit are paired. Quantity is signed and bounded to ±1,000,000,000,000; unit is
non-blank, limited to 200 Unicode scalar values, excludes Unicode control (`Cc`) scalars, and must
match the scheduled target unit when one exists. Actual time is bounded to 31,622,400 seconds and
notes to 10,000 Unicode scalar values. Corrections may increase, decrease, replace, or remove
evidence subject to the status shape.

Pause creation uses `{operation_id, pause_id, expected_revision: 0, started_at}`. Resume uses
`{operation_id, expected_revision, ended_at}` with a positive exact revision. A habit has at most
one open pause, intervals cannot overlap, and a closed pause cannot reopen. `preserves_streak` is
copied from authoritative habit policy rather than accepted from the client.

Date ranges are inclusive, limited to 366 days, and restricted to years 1900–2200. Page limits
default to 100 and are capped at 200. Cursors are opaque, checksum-protected, and bound to the
workspace and exact query. The analytics service refuses to aggregate more than 50,000
occurrences.

## Offline delta and invalidation

Delta returns ordered full upserts and an opaque continuation:

```json
{
  "changes": [
    { "type": "occurrence_upsert", "occurrence": {} },
    { "type": "pause_upsert", "pause": {} }
  ],
  "next_cursor": "opaque",
  "has_more": false
}
```

There are no destructive tombstones: clearing an outcome is an unresolved upsert and closing a
pause is a higher-revision pause upsert. Clients must persist every returned cursor atomically with
its applied changes and a separate terminal-checkpoint bit: a page with `has_more: true` persists
the bit as false, and only a page with `has_more: false` sets it true. An intermediate cursor may
resume the drain efficiently, but it cannot authorize local composition or stream-only readiness.

The SSE endpoint requires `Accept: text/event-stream` exactly and accepts an opaque cursor in
`Last-Event-ID`. A `habit-invalidation` event contains only `{"cursor":"opaque"}`. Heartbeats are
comments; connections are bounded and expire so clients reconnect. The cursor tells the client to
drain delta—it never carries a title, note, item ID, occurrence ID, or outcome.

## Audit, privacy, and analytics

Current projections advance by exact optimistic revision. Every accepted correction stores an
append-only version with prior/current snapshots, operation identity, semantic occurrence time,
and record time. Audit metadata and outbox notifications contain only IDs, revisions, status names,
and change sequence. Notes are permitted only in authenticated occurrence detail, full delta,
version history, and exact idempotency receipts; they are absent from audit metadata, outbox/SSE,
and analytics.

Analytics deterministically reports expected, eligible, completed, partial, skipped, missed,
excused, and unresolved counts; actual-time and signed quantity totals; current/longest streak;
day/week/month trends; and a bounded supportive fact-code enum. Adherence is the half-up rounded
mean of explicit basis points across eligible occurrences. Completed contributes 10000,
missed/unresolved contributes zero, and skipped partial evidence remains credited. A preserving
pause makes an overlapping occurrence excused and removes it from adherence and streak
denominators without discarding recorded time or quantity.

## Verification

`fixtures/habit-protocol/occurrence-evidence-v1.json` freezes the shared evidence envelope,
identity variants, portable bounds, and invalid-case matrix consumed by Rust, macOS, and Android.
Each case applies a shallow top-level replacement patch to `base_evidence`; nested values are
replaced as complete values rather than recursively merged. Case names are unique across the valid
and invalid sets, and invalid acceptance means either decoding or semantic validation must fail.

`tests/habits_api.rs` freezes REST identity, replay, correction, privacy, cursor/SSE, pause, range,
and analytics behavior against the in-memory adapter. `tests/habits_postgres.rs` exercises atomic
publication admission, durable replay, concurrent compare-and-swap, preview invalidation,
recomposition, audit/outbox privacy, append-only guards, DST-local dates, and workspace isolation
when `DAYWEAVE_TEST_DATABASE_URL` is configured.
