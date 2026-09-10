# Recurring task and routine member completion

Status: server/shared-engine implementation in progress for `HIE-004` and
`ROU`. This does not replace the [full requirements](product-requirements.md)
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

## Remaining integration

- Native macOS/Android protected review, strict transports, encrypted per-instance
  cache/outbox, frozen exact replay and terminal-only convergence.
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
