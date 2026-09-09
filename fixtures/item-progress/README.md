# Independent progress parity fixtures

These are synthetic scalar/component inputs for the
[independent progress contract](../../docs/item-progress.md), not live account
data or complete HTTP requests.

- `values-v1.json`, schema `dayweave.item-progress-values/1`, contains 37 decimal
  and 21 name/unit cases. Valid decimal expectations use **string-encoded signed
  millionths**, never floating-point JSON numbers. Names and units count Unicode
  scalars, not grapheme clusters or UTF-16 code units. Terminal LF/CR/CRLF are
  invalid decimals; U+200B, U+FEFF and U+180E are not Unicode White_Space.
- `components-v1.json`, schema `dayweave.item-progress-components/1`, contains five
  valid and 35 invalid component collections. Validation includes collection
  limits, unique stable identities, exact tagged shapes, scalar bounds, explicit
  nullable fields, and unknown-field rejection. Consumers must validate the
  whole collection, not just deserialize a compatible subset.

Nullable `remaining_seconds` and quantity `target` keys must be present. A null
remaining duration means unknown, not zero. A null quantity target means no
threshold. Missing keys, invalid targets and unsupported tags cannot silently
acquire defaults. Percentages and seconds are JSON integers, not strings or
fractional values. Additional raw-transport tests should reject duplicate keys
and noncanonical integer spellings.

The quantity direction is explicit; components do not infer lifecycle,
execution credit, or schedule demand. Targets and independent measures with
different units never add together. Existing goal metadata and habit outcomes
are not migrated into these fixtures' component model.
