# Scheduling preview and publication contract

`POST /v1/schedule/preview` composes the active canonical item graph without
writing items, schedule blocks, or provider state. It requires the ordinary
DayWeave bearer token. The same canonical revisions, request, scheduler schema,
and required Calendar projection stamps produce the same `input_digest` and
plan.

The preview and publication routes each have a 16 MiB request-body ceiling.
Other API routes retain the service-wide 1 MiB ceiling. A body over its route
limit receives `413 payload_too_large` rather than being reclassified as
malformed JSON.

Successful preview is also the publication-persistability boundary. Fixed-block
titles must contain 1–500 Unicode scalar values and no control characters;
fixed/output identifiers must be unique; and every persisted JSON string and
key must satisfy the same bounded control-character rules. The complete durable
publication snapshot must be no larger than 8 MiB, leaving headroom beneath the
16 MiB route and database limits. A deterministic violation is rejected as
`422` during preview, before a client can journal a publish request.

Every response includes `source_item_revisions`, an object mapping every active
canonical item UUID to the exact revision used for composition (including items
reported as rejected). A client must compare the complete map with its delta
cache before persisting a preview. If it differs, the item delta and preview
were taken from different repository snapshots; discard the preview and retry
the pull/compose cycle.

The digest also binds that complete active-item revision map. A change to an
item that is rejected or otherwise produces no block therefore still makes a
previous preview stale. It also binds the explicit scheduler publication schema
version, so a preview cached across a solver/schema upgrade cannot acknowledge
and install different blocks. Effective sensitivity evidence is retained
internally for publication/redaction; it is deliberately not exposed as a
whole-item map in the preview JSON or OpenAPI schema.

Every selected `blocking` or `writable` Google Calendar whose policy can reserve
time must also have one complete expanded-occurrence generation under its
current collection revision, covering the entire requested horizon. Otherwise
preview returns `503` and no plan is produced. A completed generation is usable
for at most 30 minutes (twice the normal sync cadence); a stopped worker or an
account that can no longer refresh therefore cannot leave cached Google truth
trusted for months merely because its window still covers the horizon. The
digest binds sorted, content-free generation/window stamps on both sides of the
canonical item read; those internal stamps contain only DayWeave collection
UUIDs and are omitted
from the response and OpenAPI schema. Once an authoritative Calendar generation
is present, caller-supplied `google_calendar` fixed blocks are rejected to avoid
double booking. Context-only calendars do not form a capacity fence because
their canonical `calendar_context` items are accepted but intentionally emit no
work item or block.

## Explicit immutable publication

`POST /v1/schedule/publish` requires a native device credential carrying the
REST-only `schedule_publish` scope. Native MCP and OAuth MCP credentials can
never receive that scope. The request wraps the exact typed preview input:

```json
{
  "idempotency_key": "11111111-1111-4111-8111-111111111111",
  "expected_input_digest": "sha256:replace-with-the-64-lowercase-hex-preview-digest",
  "schedule": {
    "as_of": "2026-09-01T07:00:00Z",
    "horizon_start": "2026-09-01T00:00:00Z",
    "horizon_end": "2026-09-02T00:00:00Z",
    "timezone_name": "Europe/Madrid",
    "availability": [],
    "fixed_blocks": [],
    "previous_assignments": [],
    "config": {
      "slot_granularity_minutes": 5,
      "stability_weight": 4,
      "default_soft_weight": 100
    },
    "recurrence_context": {}
  }
}
```

Before recomposing, the server checks the durable `(workspace, user,
idempotency_key)` receipt against a domain-separated hash of the typed request.
This lets a client recover a lost successful response even after later item or
schedule changes. Exact replay returns the original (possibly superseded)
receipt and its original `published_at`; changed content under the same key is
`409 schedule_publication_idempotency_conflict`.

For a new key, the server recomposes from canonical items and compares the
result with `expected_input_digest`. Inside the publication transaction it
serializes against item mutations and rechecks the complete active-item
revision set. It also share-locks every Calendar collection row, reconstructs
the required projection stamps, and rejects a changed generation, configuration,
coverage window, or newly enabled blocking source as
`409 schedule_publication_stale`. It then inserts a draft header, blocks, and
exactly one detail; supersedes the old current revision; seals the draft as
published; and writes
the receipt and audit row, all in one transaction. Content insertion is allowed
only while the parent is draft, and blocks/details become immutable after the
seal. A fresh key whose solver-versioned publication content is identical to the
current revision binds to that existing revision without revision churn.
An expected-digest mismatch or canonical item change during the transaction is
`409 schedule_publication_stale`. That stable code proves no publication was
committed and tells the client to discard the journal and recompose; generic,
transport, unavailable, and idempotency-conflict failures remain ambiguous and
must retain the exact journal for operator recovery or retry.

Both first publication and exact idempotent replay return `200`; `replayed` is
the sole distinction:

```json
{
  "revision": {
    "id": "22222222-2222-4222-8222-222222222222",
    "revision": "7:22222222-2222-4222-8222-222222222222",
    "revision_number": 7,
    "input_digest": "sha256:replace-with-the-64-lowercase-hex-preview-digest",
    "horizon_start": "2026-09-01T00:00:00Z",
    "horizon_end": "2026-09-02T00:00:00Z",
    "timezone_name": "Europe/Madrid",
    "published_at": "2026-09-01T07:00:03Z"
  },
  "replayed": false
}
```

The client must journal the complete publish request before I/O and clear it
only after validating status `200`, the exact digest, and the returned receipt.
When `replayed` is `true`, that receipt proves the old publication outcome and
may already be superseded; clear the matching journal, but do not make that
candidate current or actionable. Fresh-compose and publish again. A
`replayed:false` response for a new key may legitimately bind identical content
to the already-current revision and therefore carry an older `published_at`;
that response remains installable after the same exact validation.
Preview remains side-effect-free and never creates draft/publication rows.

## Published schedule reads and what-if simulations

With PostgreSQL configured, the production MCP dependency graph reads only the
current `published` revision. With no published revision it returns an explicit
not-found result; it never fabricates an empty schedule. Every query and
simulation requires principal `workspace_id` and `user_id` to equal the
configured personal database scope. Missing/static-legacy/cross-scope identity
fails closed.

Schedule blocks preserve `planned`, `pinned`, `calendar_event`, and
`external_fixed` semantics. Busy-only reads omit every ID/title; inherited
sensitive blocks become opaque busy intervals, sensitive item search results
are omitted, sensitive placement evidence is not found, and conflicts involving
any private or ambiguously related evidence are filtered. Search considers all
bounded goal links, not only the first.

`simulate_plan` requires the exact current revision. A move of a fixed, pinned,
calendar, or external block is explicitly `not_movable`; a move of a flexible
planned block is currently `not_modeled`, because the simulation adapter does
not yet prove availability, overlap, horizon, and hard-constraint feasibility.
It never returns a moved block while that proof is absent. Deletion is flagged
as requiring confirmation; every other unsupported operation receives an
explicit `not_modeled` warning and remains proposal-only. Simulation
capabilities use 32 random bytes, are stored only as domain-separated
token/subject hashes, expire within 15 minutes, are bounded per owner, survive
restart, and are consumed once under a database row lock. Proposal creation,
outbox/audit insertion, capability consumption, and the tenant/subject-scoped
exactly-once submission receipt commit in one PostgreSQL transaction.
Publication after simulation makes the token stale via an exact revision check.
Each durable simulation also carries internal, typed item/block reference sets
and a monotonic `sensitive_at_simulation` bit. This privacy evidence is never
returned by MCP, and consume/submission rechecks it against both the published
revision and the current canonical hierarchy. Missing, malformed, unknown, or
historically sensitive evidence fails closed without consuming the capability
or committing any proposal, outbox, audit, or receipt row. Active simulation
records created before this evidence existed must be simulated again.

All timestamps at the HTTP boundary are RFC 3339 and must be aligned to
microsecond precision, matching PostgreSQL `timestamptz`; finer fractions are
rejected with `422` before digesting or journaling. The API resolves local day
boundaries from `timezone_name`, including 23- and 25-hour DST days. A horizon
must be positive and no longer than 90 days.

Production publication, immutable reads, and transactional MCP proposal
submission require migrations through
`0014_google_calendar_projection.sql`.

An upgrade from migrations 1–11 safely seals any legacy published revision but
cannot invent the missing durable detail/evidence snapshot. Schedule and item
reads remain available from its immutable blocks; conflict queries and
simulation return the stable `republish_required` result until the native app
previews and publishes one fresh revision. Operators must complete that fresh
publication before enabling remote MCP access. Legacy drafts remain drafts and
may be discarded normally; they are never promoted by the migration.

The publication `idempotency_key` is a random client-generated UUID and a
non-secret correlation identifier, not a bearer capability. It is intentionally
stored verbatim in the publication receipt and content-free audit metadata.
MCP simulation capabilities and external string proposal retry keys remain
domain-hashed and are never persisted raw.

Every current canonical item carries a required `is_sensitive` boolean. It is
the item's own classification; preview blocks and rejected-item entries carry
the effective value after cycle-safe ancestor propagation. Missing ancestors
and malformed hierarchy cycles fail closed as sensitive. The flag is output
metadata only and does not influence placement or scoring. Clients must compare
each canonical preview block with the effective value computed from the exact
delta snapshot and reject the complete preview on any mismatch.

Deployment must fence the contract before sensitive classification is enabled:
reject client versions that predate the required field and expire their active
sessions. Unknown-field tolerance in an older client is not a privacy boundary;
such a client could otherwise treat sensitive content as ordinary planner data.

```json
{
  "as_of": "2026-09-01T07:00:00Z",
  "horizon_start": "2026-09-01T00:00:00Z",
  "horizon_end": "2026-09-02T00:00:00Z",
  "timezone_name": "Europe/Madrid",
  "availability": [
    {
      "start": "2026-09-01T07:00:00Z",
      "end": "2026-09-01T16:00:00Z",
      "contexts": ["computer"],
      "location": "home",
      "energy": "deep"
    }
  ],
  "fixed_blocks": [
    {
      "id": "44444444-4444-4444-8444-444444444444",
      "is_sensitive": true,
      "title": "Private appointment",
      "start": "2026-09-01T08:00:00Z",
      "end": "2026-09-01T08:30:00Z",
      "source": "protected_time"
    }
  ],
  "previous_assignments": [],
  "config": {
    "slot_granularity_minutes": 5,
    "stability_weight": 4,
    "default_soft_weight": 100
  },
  "recurrence_context": {}
}
```

Previous assignments are stability hints, not authoritative schedule state.
Each carries `item_id`, `item_revision`, optional `occurrence_id`, `pinned`, and
`blocks`. A missing or changed canonical revision is returned under
`ignored_previous_assignments` and never pinned accidentally.

Fixed-block objects also require `is_sensitive`; the same value is copied to
their `external_fixed` output blocks. A client must bind returned external
blocks to the exact fixed-block identifiers and classifications it submitted,
and must reject a preview unless every submitted block intersecting the horizon
is returned exactly once.

## Canonical scheduling metadata

The canonical item fields remain the source of truth for duration, deadline,
earliest start, priority, hierarchy, recurrence, and split bounds. Optional
advanced data lives in `flexible_constraints`. Its top-level schema is strict;
unknown fields reject that item from the preview rather than being ignored.

Supported metadata keys are:

- `constraints`: the portable core constraint object;
- `has_own_effort`, `goal_ids`, `tags`, and `energy`;
- `calendar_event` for an event that reserves capacity;
- `calendar_context` for a retained, nonblocking provider occurrence;
- `dayweave_firm_block` for a legacy DayWeave-owned immutable event;
- `habit_target` and `preserves_streak_when_paused`;
- `routine_ordered`;
- `goal_measures` and `goal_weekly_allocation`;
- `break_category`, `break_mandatory`, and `break_prompt_to_resume`;
- `maximum_sessions`, `minimum_gap_minutes`, and `maximum_split_days`;
- `preferred_start_minute`, retained as a strict legacy convenience.

Energy can be a simple level such as `"deep"`, which becomes a soft
preference, or a qualified value:

```json
{
  "energy": {
    "value": "deep",
    "strength": { "level": "hard" }
  },
  "constraints": {
    "preferred_absolute_windows": [
      {
        "value": {
          "start": "2026-09-01T09:00:00+02:00",
          "end": "2026-09-01T11:00:00+02:00"
        },
        "strength": { "level": "soft", "weight": 25 }
      }
    ],
    "required_contexts": [
      {
        "value": "computer",
        "strength": { "level": "hard" }
      }
    ],
    "buffers": {
      "before": 5,
      "after": 10,
      "strength": { "level": "soft", "weight": 50 }
    }
  }
}
```

Other core constraint keys include `earliest_start`, `latest_finish`,
`minimum_notice`, `allowed_weekdays`, `preferred_daily_windows`,
`forbidden_windows`, `required_location`, `dependencies`,
`maximum_daily_work`, and `maximum_weekly_work`. Canonical
`earliest_start_at` and `deadline_at` become hard bounds; defining the same
bound again in metadata rejects the item as ambiguous.

A blocking event uses this metadata (recurring Google series are expanded by
the calendar integration before composition):

```json
{
  "calendar_event": {
    "start": "2026-09-01T10:00:00+02:00",
    "end": "2026-09-01T11:00:00+02:00",
    "immutable": true,
    "all_day": false,
    "source_calendar_id": null
  }
}
```

Authoritative Google projections always set `source_calendar_id` to `null`;
provider identifiers remain in the sync mapping layer and never enter scheduling
digest or publication evidence. The nullable field remains part of the generic
and legacy manual-event schema for compatibility.

A retained nonblocking provider occurrence is a recurrence-free root `event`
whose sole metadata key is the strict, identifier-free context shape below. It
counts as an accepted canonical item but emits no work item or capacity block:

```json
{
  "calendar_context": {
    "start": "2026-09-01T10:00:00+02:00",
    "end": "2026-09-01T11:00:00+02:00",
    "all_day": false
  }
}
```

Existing DayWeave-owned Calendar blocks use `dayweave_firm_block` as their sole
metadata key. It requires `owned: true`, `starts_at`, and `ends_at`; `all_day`
and `tentative` default to `false`, while `busy` defaults to `true`. New provider
projections do not emit this legacy shape.

## Recurrence

Tasks with recurrence become recurring tasks. Habits require recurrence;
routines may have it. Supported forms are `daily`, `weekly`, `monthly`,
`every_interval`, `after_completion`, `frequency`, and `custom` (RFC 5545
`rrule`). Daily/monthly counts default to one. Weekly count defaults to the
number of selected weekdays, or one when no weekdays are selected.

```json
{
  "type": "weekly",
  "times_per_week": 3,
  "weekdays": ["monday", "wednesday", "friday"]
}
```

Generated occurrence identities are stable across overlapping horizons.
`recurrence_context` can supply completion/rolling anchors, spacing,
completed occurrence IDs, pauses, and exceptions. References to unavailable
canonical items fail the request rather than silently changing recurrence
semantics.

## Partial item rejection

Malformed legacy metadata is isolated under `rejected_items`, and descendants
of a rejected parent are rejected too. Valid independent items still compose.
Malformed request-level availability, fixed blocks, recurrence context, bounds,
or scheduler configuration fails the whole request with `422`; this prevents a
caller from mistaking a partially interpreted request for a complete plan.
