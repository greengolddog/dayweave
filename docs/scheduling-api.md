# Scheduling preview contract

`POST /v1/schedule/preview` composes the active canonical item graph without
writing items, schedule blocks, or provider state. It requires the ordinary
DayWeave bearer token. The same canonical revisions and request produce the
same `input_digest` and plan.

Every response includes `source_item_revisions`, an object mapping every active
canonical item UUID to the exact revision used for composition (including items
reported as rejected). A client must compare the complete map with its delta
cache before persisting a preview. If it differs, the item delta and preview
were taken from different repository snapshots; discard the preview and retry
the pull/compose cycle.

All timestamps at the HTTP boundary are RFC 3339. The API resolves local day
boundaries from `timezone_name`, including 23- and 25-hour DST days. A horizon
must be positive and no longer than 90 days.

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
      "source": "google_calendar"
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
- `calendar_event` for an `event` item;
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

An event requires this metadata (recurring Google series are expanded by the
calendar integration before composition):

```json
{
  "calendar_event": {
    "start": "2026-09-01T10:00:00+02:00",
    "end": "2026-09-01T11:00:00+02:00",
    "immutable": true,
    "all_day": false,
    "source_calendar_id": "primary"
  }
}
```

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
