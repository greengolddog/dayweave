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

Migration `0027_habit_missed_resolutions.sql` adds an independent, revisioned missed-resolution
projection and immutable version history. It also records exact occurrence membership for every
published schedule. Historical source evidence is never deleted and does not need to reappear in
each moving publication horizon. Its current policy fingerprint must still match. When the current
publication fully covers a selected reduction target's window, that target must still be the exact
generated member (or the same already-bound skipped member); outside that horizon, the historical
edge remains effective so a later source cannot cascade through it. Reduction targets are exactly
one distinct RFC 4122 UUID-v5 planner occurrence; clients cannot choose or widen that set.

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

## Missed-occurrence lifecycle

The server uses its own clock to reconcile a bounded, workspace-wide page of overdue evidence.
Only active, executable leaf habits with recurrence participate. Selection is stable and bounded so
one busy habit cannot indefinitely prevent another habit from being examined. The persisted
`habit_missed_policy` determines the initial action:

- `skip` makes the source occurrence scheduling-inactive;
- `carry` keeps the same stable occurrence identity and supplies a server-derived future window;
- `reduce_frequency` considers exactly the immediate next occurrence in the current published
  series. It suppresses that occurrence when eligible, or remains `reduction_pending`; an
  ineligible immediate occurrence is never skipped in favor of a later candidate;
- `ask` creates `decision_required`; the user may then choose skip, carry, or reduce frequency.

Clients send only the explicit ask choice. Carry windows and reduction targets are always derived
from authoritative evidence. A source completion, explicit skip, overlapping pause, non-executable
or non-leaf hierarchy state, deletion, terminal item status, lost recurrence, policy edit, or stale
publication membership cancels an active projection with a reason. The projection retains its
resume action, so a later outcome correction or eligible policy state can restore the same action
without inventing a second decision.

Outcome and missed-resolution revisions are independent coordinates. Their combined
`occurrence_upsert` is totally ordered only by the habit delta cursor; accepting a response for one
coordinate must preserve a concurrently advanced value on the other coordinate. Local and server
composition apply the same precedence: completion or explicit outcome skip, then an active pause,
then active missed skip/carry/reduction. Any stored manual occurrence move owned by an active skip,
carry, or reduction is removed before recurrence context is built, so stale local placement cannot
resurrect server-suppressed work.

## REST contract

All routes are under `/v1`, require the normal native/legacy bearer audience, and return
`Cache-Control: no-store`. Read routes require `items_read` and mutations require `items_write`;
the workspace-wide missed reconciliation mutation requires both `items_write` and `items_read`
because its response exposes resolution metadata across habits.

- `GET /habits/{habit_id}/occurrences?start_date=&end_date=&cursor=&limit=`
- `PUT /habits/{habit_id}/occurrences/{occurrence_id}`
- `POST /habits/missed/reconcile?limit=`
- `PUT /habits/{habit_id}/occurrences/{evidence_id}/missed-resolution`
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

Missed reconciliation accepts only `{operation_id}` and returns a bounded array of current
resolution projections plus `has_more`. Explicit ask resolution accepts
`{operation_id, expected_revision, action}`, where `action` is `skip`, `carry`, or
`reduce_frequency`. It uses exact compare-and-swap revision fencing. A simultaneous outcome or
policy change may instead return the matching server cancellation; it cannot be mistaken for the
requested active action. Both routes require the same strict idempotency identities as other habit
mutations.

Changed or nonterminal reconcile pages keep permanent exact-response receipts because losing such
a response would lose durable authority. A terminal empty scan uses the shared expiring receipt
store: it is retained for 24 hours, capped at 4,096 entries per workspace, and never evicts an entry
younger than the clients' 12-hour retry lease. If that protected capacity is exhausted, the no-op
transaction fails instead of weakening exact retry semantics. A client with a lost response cannot
know which receipt class the server stored, so after 12 hours it may rotate any still-unresolved
automatic-reconcile journal. It first durably leaves terminal delta authority false, then stages a
fresh scan; any changed or nonterminal original receipt remains permanently replayable and the
ordinary delta drain recovers its effects.

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
    { "type": "occurrence_upsert", "occurrence": { "outcome": {}, "missed_resolution": {} } },
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
The occurrence envelope may advance its outcome coordinate, its missed-resolution coordinate, or
both; clients validate exact reachable transitions for each coordinate independently.
Every authenticated occurrence object includes the `missed_resolution` member, using explicit
`null` when no projection exists. Native network decoders reject an omitted member so an older or
malformed terminal page cannot silently erase scheduling authority. Legacy encrypted-snapshot
migration remains separately versioned and strips fields that the stored schema could not have
owned before rewriting it to the current format.

Native retention also preserves every completion that may become a correction fallback, every
physical non-cancelled reduction source and target needed for correction or reactivation, every
pending mutation reference, every occurrence named by a retained manual move, and all linked open
or overlapping closed pauses needed to evaluate those rows. If those mandatory rows exceed a cache
limit, the delta page fails closed without advancing its cursor or terminal-checkpoint bit.

Both native clients persist exact current-publication occurrence membership separately from
rendered-block authority. They also persist the highest authenticated schedule revision observed
for the active credential binding before accepting an SSE hint. A proof below that durable
high-water cannot suppress work, authorize block execution, or start a generated-schedule provider
write after a failed fetch or relaunch. Already-submitted ambiguous writes retain only their
recovery path. Only an authenticated cursor-ahead response may open the one-shot path that installs
or clears a lower head after a server revision-epoch reset; ordinary current responses and publish
receipts cannot lower it.

The SSE endpoint requires `Accept: text/event-stream` exactly and accepts an opaque cursor in
`Last-Event-ID`. A `habit-invalidation` event contains only `{"cursor":"opaque"}`. Heartbeats are
comments; connections are bounded and expire so clients reconnect. The cursor tells the client to
drain delta—it never carries a title, note, item ID, occurrence ID, or outcome.

## Audit, privacy, and analytics

Current projections advance by exact optimistic revision. Every accepted outcome correction or
missed-resolution transition stores an append-only version with prior/current snapshots, operation
identity, semantic occurrence time where applicable, and record time. Audit metadata and outbox
notifications contain only IDs, revisions, status/action names, and change sequence. Notes are
permitted only in authenticated occurrence detail, full delta, version history, and exact
idempotency receipts; they are absent from audit metadata, outbox/SSE, and analytics.

Analytics deterministically reports expected, eligible, completed, partial, skipped, missed,
excused, and unresolved counts; actual-time and signed quantity totals; current/longest streak;
day/week/month trends; and a bounded supportive fact-code enum. Adherence is the half-up rounded
mean of explicit basis points across eligible occurrences. Completed contributes 10000,
missed/unresolved contributes zero, and skipped partial evidence remains credited. A preserving
pause makes an overlapping occurrence excused and removes it from adherence and streak
denominators without discarding recorded time or quantity.

Analytics applies the current-policy missed lifecycle before calculating those aggregates. An
effective frequency reduction removes its selected target from expected, eligible, missed, trend,
and streak demand while the missed source remains counted; a stale-policy or inactive reduction
does not suppress its target. Carry retains the source's original local-date bucket, but its derived
window controls pause overlap and streak due-time. It remains unresolved until that carried window
expires and becomes missed only afterward.

## Verification

`fixtures/habit-protocol/occurrence-evidence-v1.json` freezes the shared evidence envelope,
identity variants, portable bounds, and invalid-case matrix consumed by Rust, macOS, and Android.
Each case applies a shallow top-level replacement patch to `base_evidence`; nested values are
replaced as complete values rather than recursively merged. Case names are unique across the valid
and invalid sets, and invalid acceptance means either decoding or semantic validation must fail.

`tests/habits_api.rs` freezes REST identity, replay, correction, missed-policy reconciliation,
ask decisions, cancellation/restoration, privacy, cursor/SSE, pause, range, and analytics behavior
against the in-memory adapter. `tests/habits_postgres.rs` exercises atomic publication admission,
current-publication reduction binding, durable and bounded replay, concurrent compare-and-swap,
preview invalidation, recomposition, audit/outbox privacy, append-only guards, DST-local dates, and
workspace isolation when `DAYWEAVE_TEST_DATABASE_URL` is configured. Its scoped-device HTTP
scenario also drives the real router and PostgreSQL adapters from item creation through
preview/publication, evidence read, partial outcome and exact replay, stale-publication rejection,
missed resolution, reduced and terminal recomposition, analytics, pause/resume, and ordered delta
catch-up. Native suites freeze encrypted migration, offline replay, independent revision merging,
retention, user actions, and local composition parity on macOS and Android.
