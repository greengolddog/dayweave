# Routine planning witness producer corpus, version 1

`wire-v1.json` is synthetic public test data. It contains no owner account,
Calendar data, tokens or usable credentials. Never submit it to an owner's
service. Discover cases by name and iterate the arrays rather than fixing their
counts or positions:

```text
{
  schema_version: 1,
  qualified: [{name, canonical_items, request, response, helper_request, helper_response}],
  remote_required: [{name, response}],
  invalid: [{name, base, response}],
  raw_invalid: [{name, base, raw}]
}
```

`base` names the qualified case whose request and complete canonical inputs
belong to a negative response. `raw` is an exact JSON string: pass those bytes
directly to the validator, before parsing could erase duplicate keys or number
spellings. Ordinary semantic cases likewise retain integer token distinctions.

## What produces the accepted cases

[The Rust producer](../../server/dayweave-api/src/scheduling/planning_witness_fixtures.rs)
uses real canonical constructors, recurrence expansion, authoritative schedule
normalization, lifecycle composition, server qualification and the helper-v2
byte protocol. Both the server's qualified fingerprint and the independently
computed helper response are retained. The ordinary golden test regenerates
this corpus and requires exact equality; a second test rejects every semantic
and raw malformed response using contextual admission and helper computation.

The larger case includes an outer Project, two independent instances of a
Routine, a structural Task, required and optional leaves, an Inbox descendant,
a blocked member and an instance-specific Skipped member. It retains all ten
current canonical sources, even those that have no schedule blocks. Actual
authoritative Habit normalization removes spoofed caller outcomes, and a fixed
Calendar event constrains capacity despite a conflicting retained assignment.
The second case has positive historical occurrence head with no current
instances; it must never be coerced to v1 or a zero head.

Canonical source timestamps and the original planning clock include
microseconds. Preserve them through native decoding and helper encoding. The
schedule's `recurrence_context.calendar.days[].local_date` follows the Rust
`time::Date` encoding `[year, ordinal_day]`; lifecycle identity dates follow
their distinct ISO civil-date contract. Neither encoding may be guessed from
the other. Request and normalized witness schedule are intentionally different:
only the documented authoritative fields may change.

## Trust and coverage boundaries

These fixtures prove producer/consumer compatibility, not authentication,
current server state or an offline lease. Scope IDs, publication IDs, terminal
cursors and Calendar generations are deterministic synthetic values. The
[HTTP/PostgreSQL suite](../../docs/routine-planning-witness.md) separately covers
authenticated capture, concurrency and rollback-only custody.

Native consumers must pair the returned witness with the exact owned request,
complete current canonical sources and credential configuration. Server request
and capture hashes are opaque Rust-serialization fingerprints, not native JSON
hashes. Recomputing helper-v2 must match `local_input_fingerprint`; none of these
hashes grants execution, publication, mutation or reusable review authority.

Negative cases cover scope/source/cursor pairing, complete membership (including
Inbox), topology, status, revision and integer grammar, unknown/missing fields,
duplicate UUID aliases, hash domains, immutable request changes and duplicate
raw keys. Native suites add transport bounds, cancellation and their local
helper framing checks. This small corpus does not replace deep-tree tests,
rolling/custom recurrence coverage, encrypted custody, connected workflows or
physical-device acceptance.

## Intentional regeneration

After reviewing a deliberate producer/contract change, this maintenance-only
test rewrites exactly `fixtures/routine-planning-witness/wire-v1.json`:

```sh
cargo test --locked -p dayweave-api --lib \
  emit_shared_routine_planning_witness_fixture -- --ignored --nocapture
cargo test --locked -p dayweave-api --lib \
  routine_planning_witness_shared_fixture
```

It needs no database, network, native app or credentials. Ordinary test runs
must not regenerate fixtures. Full database gates that enable ignored tests
must explicitly skip this maintenance emitter (and the separate occurrence
corpus emitter).
