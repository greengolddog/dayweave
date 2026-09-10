# Authenticated routine planning witness

Status: **server checkpoint implemented and verified**. This is a server-side
prerequisite for the full offline planning requirement, not completed native
offline scheduling. It does not replace the [product requirements](product-requirements.md)
or [recorded discovery answers](discovery-answers.md).

## Private read-only endpoint

`POST /v1/routine-occurrences/planning-witness` requires an owner-bound Device
credential with both `items_read` and `schedule_simulate`. Legacy and MCP
credentials cannot use it. It requires PostgreSQL; an unavailable repository
never produces empty planning authority. Responses and errors are `no-store`.
URL query selectors are rejected; the request has one JSON content type and a
closed versioned body:

| Field | Meaning |
| --- | --- |
| `schema_version` | Exactly `1`. |
| `schedule` | Exact existing `ComposeScheduleRequest`, including horizon, clock, timezone and constraints. |
| `expected_source_item_revisions` | Complete active canonical revision map, including unscheduled and Inbox members. |
| `terminal_cursor` | Terminal occurrence list/delta checkpoint already installed by the client. |

The body is capped at 16 MiB without widening the ordinary member-mutation
route. The shared scheduler-helper JSON parser rejects duplicate decoded keys,
invalid UTF-8, excessive nesting and aggregate resource use while preserving
integer versus floating-point tokens. UUID aliases cannot overwrite source-map
entries. Source revisions must be positive portable integers; the map retains
the 10,000-item limit and the cursor is bounded to 4,096 ASCII bytes.

The endpoint never admits an occurrence, rebases history, updates a template,
publishes a schedule, creates execution state, writes an operation receipt or
persists a witness. Capture transactions explicitly roll back. Reading this
endpoint does not advance a client's occurrence checkpoint.

## Qualified versus remote-required

The response has `schema_version: 1` and a closed `result` tagged by `status`:

- `qualified` contains `witness` with the owner/workspace IDs, complete source
  revision map, unchanged terminal cursor, normalized schedule input, complete
  current-source `occurrence_lifecycle`, execution revision, habit head, current
  publication ID and non-publishable fingerprints.
- `remote_required` contains only a typed `reason`, with no witness or planning
  authority. Reasons distinguish missing first publication, unrepresented
  execution evidence, manual-placement policy, ineligible sources, incomplete
  Calendar projection and unsupported composition.

Stale source maps or stale/foreign/intermediate cursors are conflicts, not
remote-required success. A missing generated instance requires normal remote
publication/admission, terminal ledger catch-up and a new witness request.
The narrow first-publication Defer exception cannot refresh this evidence.
A positive ledger head is retained even if the requested horizon has no
generated instances; it never silently becomes a zero-head/v1 input.

## Consistent authority and exact helper inputs

Capture follows account/owner admission, existing execution-state locking,
canonical item serialization and shared active-item row locks, then Habit and
occurrence locks and a compatible shared owner/publication fence. It does not
insert a missing execution row or reacquire an execution-row lock after entering
canonical space. The shared item rows fence an actual first execution Start;
the shared owner fence avoids a lock cycle with progress admission.
Whichever operation acquires conflicting item rows first determines the capture
order: a witness can qualify before a queued Start, or wait and observe the
committed execution evidence. A qualified response is not a lease against a
later Start.

The server compares the complete source revision map and exact terminal head.
It derives generated identities and current lifecycle/member revisions itself.
The same authoritative normalization used by remote composition supplies Habit
state, retained assignment policy and Calendar safety checks. Capture rechecks
execution, Habit, Calendar freshness, sources and the occurrence head before
returning evidence.

Qualification invokes the existing helper-v2 boundary in-process, not through a
native process or external service. It requires exact prepared planning input,
full plan and source-accounting parity with authoritative composition. The
response also carries the successful `local_input_fingerprint` so a future
native adapter can compare its independent helper computation to this exact
qualified input.

The current helper-v2 protocol uses a default execution context. Nonempty
execution work units—including credit, consumed session indices, dispositions
or reservations—therefore require remote composition even without an active
timer. Requested or retained manual-placement policy also remains remote-required.
These are unfinished integration limits, not a reduction of the product's
offline planning/execution requirements.

## Fingerprints are not capabilities

`request_fingerprint` binds the complete original request and scope.
`calendar_projection_fingerprint` binds scoped Calendar generations without
exposing private provider state. `witness_fingerprint` additionally binds all
captured canonical, execution, Habit, Calendar, publication and normalized
planning evidence, including the expected helper fingerprint. These use separate
`routine-witness-…-sha256:` prefixes/domains; helper output retains `local-sha256:`.
Neither is a publishable `sha256:` input digest.

Authenticity comes from the authenticated response, not from possessing a hash.
The witness grants no mutation, execution, publication or persistent review
lease. It cannot qualify a changed source snapshot, horizon or request. Native
consumers must still bind credential configuration and privacy, retain exact
pending-operation custody, and revalidate captured generations around helper
execution and durable installation. Native helper-v2 adapters remain disabled
until those integration and recovery gates are implemented and verified.

## Verification and remaining work

Focused verification passes 13 wire/backend unit tests, 10 HTTP boundary tests
and 17 real PostgreSQL scenarios. These cover complete sources and terminal
cursors, unchanged authority/history/receipts, current versus first-source
revisions, instance-specific outcomes, positive empty-horizon heads, first
publication, owner lifecycle, both first-Start/capture orderings and progress
lock ordering. Additional cases exercise successful real-router responses with
synthetic Device
authentication, real Habit outcomes and stripped caller completion/progress
claims, plus configured Calendar capacity, incomplete/stale/future coverage and
generation-bound fingerprints. Calendar fixtures exercise authoritative storage
and production invalidation triggers, not Google ingestion or OAuth.

The [shared producer corpus](../fixtures/routine-planning-witness/README.md)
adds two deterministic golden/admission tests. It records exact request,
normalized response and helper-v2 bytes for complete current-source routine,
Habit and Calendar inputs, plus a positive history with no current instances.
All six remote-required reasons, 17 semantic mismatches and four raw malformed
messages are discovered dynamically. These synthetic cases establish wire
compatibility, not live authentication or native custody.

The strict helper decoder and unchanged v1/v2 protocol regressions pass 76 tests.
The [nine-phase native/service convergence gate](native-routine-occurrence-convergence.md)
also passes after the shared remote-normalization refactor, including immutable
SQL/current-reader checks and verified cleanup. No native client is enabled by
this server checkpoint. The full API gate including the producer corpus passes 729 tests against a fresh
disposable PostgreSQL service, with database-only cases enabled and the
two maintenance-only fixture emitters excluded; no tests fail or remain ignored.
All-target, all-feature workspace Clippy with warnings denied and formatting
checks pass. Owned test services and the retained native runtime were cleaned up.

Remaining full-product work includes encrypted native witness storage and
versioned adapters, source/generation/privacy/pending-intent fences, process and
cross-client recovery, wider local execution/manual-policy representation and
physical-device acceptance. Routine cadence/rebase/nested recurrence and
step-specific deferral retain their separate unfinished requirements.
