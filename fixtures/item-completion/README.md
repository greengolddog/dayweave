# Completion wire fixtures, version 1

`wire-v1.json` contains public, synthetic data only:

```text
{schema_version: 1, valid: [{name, kind, value}], invalid: [{name, kind, value}]}
```

The exact `kind` names are `state`, `snapshot`, `command`, and `receipt`.
Consumers must discover cases dynamically rather than depending on case counts.
Every command is interpreted for route item
`00000000-0000-0000-0000-000000000001`; this identity is deliberately not an
additional command-body field. The other item and operation UUIDs are synthetic.
The repeated `sha256:1111…` value checks opaque hash format, not real server
review authority. Never submit these fixtures to an owner's service.

## Validation layers

The fixtures describe the closed, portable producer/consumer contract, not a
claim that `serde_json::from_value` alone validates every response invariant.

- State has exactly six keys: `item_id`, `revision`, `required_for_parent`,
  `mode`, `provenance`, and `updated_at`.
- Command has exactly eight keys: `schema_version`, `operation_id`,
  `expected_item_revision`, `expected_completion_revision`,
  `expected_evidence_hash`, `required_for_parent`, `mode`, and `reopening`.
- Snapshot has exactly seven keys: `schema_version`, `item_id`, `item_revision`,
  `state`, `evidence_hash`, `counts`, and `occurrence_evidence_required`.
- Receipt has exactly three keys: `operation_id`, `replayed`, and `completion`.
  `completion` is a snapshot, not a replacement canonical Item.
- Provenance has exactly `kind` and `reopen`. Reopen has exactly `status`,
  `blocked_reason_kind`, `blocked_by_item_id`, and `blocked_reason`.
- Counts has exactly `required_descendants`, `completed`, `incomplete`, and
  `occurrence_evidence_required`.

All nullable keys must be present. UUIDs must be valid and non-nil. Revisions
are JSON integers bounded by signed 64-bit storage: canonical/expected-item
revisions are positive, completion/expected-completion revisions may be zero.
Revision zero state is exactly required, automatic, null provenance, null time.
Written state has a timestamp. Automatic permits null or automatic provenance;
Complete requires manual provenance; Keep open requires null provenance.

Reopening status is only Inbox, Planned, or Blocked. An open non-blocked status
has an entirely null blocker tuple. A dependency blocker requires a non-nil
identity different from the route/state item and may have a null reason. Manual
and external blockers require a reason and forbid a blocker identity. Reasons
are 1–1,000 Unicode scalars, without controls or leading/trailing Unicode
whitespace. Database workspace existence of a dependency is a separate admission
check and cannot be established by these standalone values.

Timestamps use a valid four-digit civil year and UTC RFC3339: `Z` or `+00:00`,
with zero to six fractional digits. Accepted UTC spellings may normalize when
re-serialized. Non-UTC offsets, invalid dates, nonfinite text, and nanoseconds
are rejected by the portable wire checker. The Rust state validator checks
timestamp precision after decoding; the test additionally checks this raw UTC
wire shape. Timestamps do not establish revision ordering.

Snapshot identity must match its nested state. Evidence is exactly `sha256:`
plus 64 lowercase hexadecimal digits. Counts are nonnegative JSON integers,
with checked addition and this partition:

```text
required_descendants = completed + incomplete + occurrence_evidence_required
```

These are every required descendant, not just leaves. The outer occurrence
boolean concerns the selected node itself: a recurring leaf may have `true`
and zero descendant counts; a one-off ancestor may have `false` and unknown
recurring descendants. Native response admission also bounds each count to
20,000, matching the current full-forest producer limit. This extra resource
check belongs to the Rust test's response helper and native validators; the
server's plain snapshot serde type and SQL arithmetic validator are broader.

The standalone state and command fixtures include signed-64-bit revision
limits. Such a retained request is well-formed even if a fresh mutation cannot
advance those revisions. This file does not add an unproven cross-revision
inequality between standalone snapshot fields.

## Review and replay context

`review_keep_open_command` pairs with `review_keep_open_receipt` and
`review_keep_open_historical_replay`: operation identity is identical, canonical
revision advances 7→8, completion revision advances 2→3, and reviewed
requiredness/mode match. The historical receipt retains the original snapshot;
only `replayed` changes. Rust tests verify that pair with checked `+1` arithmetic.
Other standalone valid values are not a complete admitted forest or a mutually
related operation sequence.

Native transport tests must separately require exact item/operation/request
binding, expected revisions +1 without overflow, reviewed policy equality, and
the `Idempotency-Replayed` header matching the receipt's boolean. A successful
receipt has positive written state and canonical revision at least two; it is
not a fresh GET proof and cannot authorize a new review or overwrite a newer
cache. Full CAS, execution, recurrence scope, account binding, HTTP scopes,
privacy, and replay custody remain integration tests, not JSON shape tests.

Raw duplicate keys and fractional/exponent integer spellings must be tested on
the original response bytes before any dictionary/JSON-value parser can erase
them. A parsed fixture object cannot represent duplicate keys. The Rust contract
suite includes raw command decoding regressions; native suites must exercise
their own raw response scanners as well.

`server/dayweave-api/tests/item_completion_contract.rs` checks real typed state
and command validation, adds explicit snapshot/receipt semantic admission, and
compares synthetic real-planner output to representative fixture states. It
does not change production serde or widen the existing API.
