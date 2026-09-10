# Recurring task and routine member completion

Status: server/shared-engine and native wire implementation in progress for
`HIE-004` and `ROU`. This does not replace the [full requirements](product-requirements.md)
or imply that the native routine experience is complete. The detailed choices
below are implementation defaults, not additional answers attributed to the
owner.

## Separate occurrence authority

A recurring template remains unchanged when a step is Done, Skipped or reopened.
The first eligible publication admits a complete immutable subtree for an exact
series/occurrence identity. Membership includes canonical children omitted from
planning, such as Inbox children; emitted calendar blocks are not a complete
manifest. Initial member revisions, topology, titles, explicit open states and
required-edge defaults are retained with their original canonical history.

Each occurrence has an independent revision and each member has its own revision,
status, required-edge/manual policy, exact reopening state and provenance. A
separate completion timestamp changes only when entering Completed; reviewing
policy while still Completed does not move that timestamp. Skipped and Cancelled
never mean Done. Optional edges exclude their branches from ancestor requirements
without cancelling the work. Manually completing a parent does not complete its
children or stop a child's timer. Unqualified independently recurring descendants
cannot inherit completion authority from an outer routine.

Fresh commands bind the complete reviewed evidence hash, instance/member revisions,
current semantic definition and execution revision. Changes affecting an exact
live member are rejected. Immutable receipts resolve an exact retry before fresh
eligibility checks; reusing an operation ID for different content or a different
target conflicts. A historical receipt is not permission to replace newer cache
state.

## Private device API

| Route | Purpose |
| --- | --- |
| `GET /v1/routine-occurrences` | Whole-instance current-state pages pinned to an immutable change head. |
| `GET /v1/routine-occurrences/delta` | Ordered whole-instance changes after a terminal checkpoint. |
| `GET /v1/routine-occurrences/{occurrence_id}` | Full exact-instance review. The path ID is the ledger instance UUID, not the planner occurrence UUID. |
| `PUT /v1/routine-occurrences/{occurrence_id}/members/{item_id}` | Reviewed leaf outcome/reopening or member required-edge/parent manual policy. |

These routes require an owner-bound Device credential. Reads require `items_read`;
writes require both `items_write` and `items_read`, because a success or exact
retry returns the full private snapshot. Legacy and MCP credentials are not
accepted here. Missing PostgreSQL authority returns 503, never fabricated state.
Responses are non-cacheable and parser failures do not echo private content.

Wire schema 1 is closed to unknown fields. `set_outcome` accepts Completed or
Skipped for leaves; `reopen` requires the exact supported open state and blocker
fields. `set_policy` changes the required edge and the Automatic/Keep open/Complete
mode; non-Automatic modes require a parent. A complete instance is bounded to
10,000 members and 8 MiB. Pages contain at most 100 whole instances within 8 MiB.
Current-state installation waits for the terminal page; its cursor resumes delta.
Intermediate list continuations cannot be used as delta checkpoints.

A current template-wide Blocked restriction continues to block open instances.
Reopening an individual instance does not silently remove that shared restriction;
terminal occurrence history remains separate evidence. Timer start checks both
canonical eligibility and the exact managed member, so an old calendar block
cannot restart an occurrence member that is Done, Skipped or Blocked.

## Scheduling and publication

Preview discovers exact generated recurring Task/Routine identities, reads the
durable ledger and removes caller whole-completion/partial-progress claims for
managed occurrences. Habit authority remains separate. Member statuses are
applied only to matching materialized members, before execution planning, without
injecting omitted canonical work or removing completed dependency evidence.
Remaining optional steps continue to occupy the calendar even when their parent
is Complete. Moves retain nominal identity independently from their effective
window; clipping a window to the horizon does not change that identity.

The complete private lifecycle context and its change head bind the input digest.
Publication rechecks them under execution → canonical → habit → occurrence →
publication-owner locking. Deferred-work assessment and authorization use the
same lifecycle-aware scheduler and require current occurrence evidence.

An empty, never-used ledger retains publication schema `/5` and the previous
input digest. A positive occurrence head selects private publication schema `/6`,
including when no managed instances occur in that horizon. Readers explicitly
accept valid legacy `/5` capsules; `/6` requires its matching version and lifecycle
context. Public preview JSON and the existing local helper v1 protocol are
unchanged. Exact historical publication receipts remain immutable.

Initial publication admission advances the ledger after the preview that created
it. That publication's proof is not rewritten. Assessed Defer can adopt only the
unchanged initial records captured by that exact publication: every intervening
change must be revision 1 with no operation, every newly adopted instance needs
that witness, existing instances must remain exact, and recomputing the complete
plan with the original execution evidence must give the same result. The current
context then binds assessment and authorization. Any later reviewed policy or
outcome change—even one preserving status—invalidates the old assessment. Fresh
publication retains strict head comparison. Native ledger catch-up remains open.

The subsequent [helper v2 bridge](scheduler-helper.md#occurrence-aware-composition-protocol-v2)
can compose an explicitly supplied lifecycle context, checking complete current
canonical membership/revisions and binding normalized context/head to a separate
local fingerprint. Its nine focused regressions and the opaque Android bridge
bring the shared Rust gate to 333 passing tests. This supplies the bounded engine
entry point, not authenticated native planning evidence or enabled client adapters.

## Native wire checkpoint

macOS and Android now have separate closed occurrence models and authenticated
review, list/delta and exact-member mutation transports. Both validate complete
iterative trees, member/state correspondence, required/optional counts, parent
completion fixed points, retained reopening provenance and immutable manifests
across repeated delta instances. Ledger path IDs remain distinct from planner
occurrence IDs. UTC timestamps retain microsecond precision, recurrence anchors
retain their original offsets, and unsupported timezone aliases fail admission.

Mutation receipts bind the exact operation, ledger/member targets, checked
instance/member revision increments and requested action. An immutable historical
receipt can settle the matching intent; it is not a current review or cache
installation witness. HTTP admission checks JSON media type, non-cacheable
responses and the exact replay header/body agreement. Only an explicitly named
error-code/status pair in the closed error envelope proves a definitive rejection;
malformed or ambiguous replies do not prove that a write had no effect. Private
response content is not copied into parser errors.

Responses are capped while reading at 8 MiB. Programmatically constructed pages
also stop at the byte budget without allocating an oversized whole-page buffer.
Android performs body reading and validation off the caller thread and retains
cancellation ownership after headers arrive, including error bodies and an
authentication retry. macOS reuses the existing authenticated request,
credential-refresh, redirect and cancellation boundaries.

The [shared synthetic wire corpus](../fixtures/routine-occurrences/README.md)
contains 48 accepted and 115 rejected cases produced and checked by Rust, then
consumed by both native suites. Fixture values preserve their exact JSON bytes:
floating-point spellings, exponent notation and 64-bit boundaries are not
normalized by a platform JSON round-trip before validation. Additional native
tests exercise raw duplicate keys, bounded deep trees, transport metadata,
historical receipt binding and cancellation. The corpus contains no owner data.

This checkpoint does not yet connect the transports to native protected review
controls, encrypted caches/outboxes, terminal convergence or local composition.
No template status is changed to represent an occurrence outcome.

## Verification

The 2026-09-10 server/shared checkpoint passes:

- 323 tests across core (220), composition (39), scheduler helper (60) and
  Android FFI (4), with no failures or ignored tests.
- 671 API tests across all targets/features against a fresh disposable PostgreSQL
  service, with database-only tests enabled and no failures or ignored tests.
- Workspace all-target/all-feature Clippy with warnings denied, and formatting.

Focused coverage across these gates includes 21 core lifecycle regressions,
14 domain cases, six scheduling cases, six private HTTP cases and five real
database scenarios. They cover immutable admission/history, reviewed exact retry, current
definition and execution fences, direct SQL corruption rejection, deletion-table
inventory, legacy upgrades, 5,000-level traversal, optional work retention and
first-publication Defer without rewriting its sealed evidence. A later
status-preserving policy review invalidates an already-issued Defer assessment
without changing execution or creating a replacement claim.

The disposable database was stopped after verification; owner services were not
modified. These are server/shared gates, not new native UI, device, or deployment
acceptance results.

The subsequent native-wire/shared-corpus checkpoint passes:

- 1,083 executed macOS tests with warnings denied, including 24 focused
  occurrence tests; three opt-in integration tests are skipped (1,086 total).
- 1,674 Android JVM tests, including 20 focused occurrence tests; two opt-in
  integration tests are skipped (1,676 total). Android lint reports zero errors
  and 29 warnings in existing dependency/platform/UI files, none in the new
  occurrence files.
- Four new Rust wire-contract regressions; the maintenance-only fixture emitter
  is ignored in ordinary test runs and passed when explicitly invoked separately.
  Workspace all-target/all-feature Clippy with warnings denied also passes.

These runs use synthetic fixtures and mock HTTP services. They do not claim
native/service convergence, physical-device interaction, a new installable APK
or a production deployment for occurrence completion.

## Remaining integration

- Native macOS/Android protected review, encrypted per-instance cache/outbox,
  frozen exact replay and terminal-only convergence; the strict models and
  transports above are implemented separately from those integrations.
- Native local-composition and pending-publication fencing against the occurrence
  head; helper v1 cannot consume this authority, while helper v2 still requires
  an admitted current-source witness and native adapter integration.
  The private API retains first-source revisions and opaque current eligibility,
  not a current-source planning witness. A native client must not guess that
  witness from its canonical cache or mutate template statuses to feed helper v1.
  Initial native controls need remote recomposition and a local-composition fence
  until the versioned local bridge has fully qualified evidence and native fences.
- Explicit review/rebase for semantic template changes. Harmless title/notes/
  estimate changes can retain authority; tree/rule changes do not silently rebind
  history. Ineligible initial terminal or execution-owned template states withhold
  automatic admission instead of inventing reopening custody.
- Completion-relative cadence and retention of unfinished optional work in older
  occurrences; the stored completion timestamp is not yet a cadence bridge.
- Qualified execution-credit/Done integration, inactive per-step Will do later,
  nested independent recurrence, complete routine authoring and owner acceptance.

Source: [domain](../server/dayweave-api/src/routine_occurrences/domain.rs),
[repository](../server/dayweave-api/src/persistence/routine_occurrence_repository.rs),
[migration 0036](../server/dayweave-api/migrations/0036_routine_occurrences.sql),
[scheduler overlay](../crates/dayweave-core/src/occurrence_lifecycle.rs).
