# Hierarchy progress parity fixtures

`projection-v1.json` uses schema `dayweave.hierarchy-progress-fixtures/1`.
Each valid case supplies the complete normalized active forest in `items` and
an `expected` summary for every item ID. The separate `invalid_cases` array
supplies an `error` code for each invalid case;
the entire forest is unavailable, not a partial map or zero-valued summary.
Input order is insignificant; consumers should also test reversed input.

Nodes use canonical kind/status spellings. `event` maps to the core scheduler's
`CalendarEvent`, not flexible effort. `recurs` means recurrence on that node;
the reducer propagates it through ancestors. Duration is either unknown (`null`)
or a positive ordered minimum/expected/maximum in exact integer seconds. These
are original recorded estimates, never remaining work or rounded minutes.

All summary counts and sums are exact integers from zero through 2^63-1. The
boundary/overflow cases deliberately exceed canonical per-item duration limits
to exercise the normalized reducer's arithmetic; they are not valid canonical
authoring requests. Do not parse these integers through floating-point values.

Every nonrecurring structural leaf counts in exactly one lifecycle bucket.
Nonrecurring fixed-event leaves also count as events but never as effort;
event parents count as events without contributing their own lifecycle.
Recurring leaves count only in `recurring_leaf_items`. Empty containers count
as leaf items; their estimates contribute only with explicit own effort.
Known effort totals exclude unknown estimates, reported separately.

Authority, complete hydration, queued mutations, trash filtering and aggregate
privacy are native admission checks outside this numerical fixture. This
projection neither proves required-work completion nor mutates lifecycle.
