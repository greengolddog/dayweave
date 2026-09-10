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
| `GET /v1/routine-occurrences/lookup?series_item_id=…&occurrence_id=…` | Resolve an exact canonical recurring root and planner UUID-v5 occurrence to its current private ledger review, including instances absent from the bounded native cache. |
| `GET /v1/routine-occurrences/{occurrence_id}` | Full exact-instance review. The path ID is the ledger instance UUID, not the planner occurrence UUID. |
| `PUT /v1/routine-occurrences/{occurrence_id}/members/{item_id}` | Reviewed leaf outcome/reopening or member required-edge/parent manual policy. |

These routes require an owner-bound Device credential. Reads require `items_read`;
writes require both `items_write` and `items_read`, because a success or exact
retry returns the full private snapshot. Legacy and MCP credentials are not
accepted here. Missing PostgreSQL authority returns 503, never fabricated state.
Responses are non-cacheable and parser failures do not echo private content.

Lookup accepts exactly the two required UUID selectors: unknown, duplicate,
missing or malformed query fields return `400 invalid_query`; a nil series or
non-RFC4122 UUID-v5 planner identity returns `422 routine_occurrence_invalid`
before storage access. A valid absent pair returns
`404 routine_occurrence_missing`. Success has the same fresh snapshot as exact
ledger GET, with `Cache-Control: no-store, max-age=0`, `Pragma: no-cache` and no
`Idempotency-Replayed` header. Current source evidence is refreshed even after a
harmless source revision. Definition drift or missing source preserves historical
review with `fresh_edit_eligible=false`. Lookup never admits an instance, modifies
the template or occurrence ledger, or supplies a current-source planning witness.
Subsequent member GET/PUT uses the returned manifest ID, not the planner UUID.
Deploy the lookup-capable service before enabling the connected native controls.

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
publication retains strict head comparison. The connected native coordinators
perform terminal ledger catch-up followed by fresh remote composition; qualified
helper-v2 local planning remains separate unfinished work.

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

The wire checkpoint itself covers transport, not a connected user workflow.
The subsequent encrypted custody and connected review checkpoints are described
below; qualified native local composition remains unfinished.
No template status is changed to represent an occurrence outcome.

## Native encrypted custody foundation

The separate occurrence state is embedded in the existing protected planner
snapshot: schema 28 on macOS and Room 24 / JSON_V24 on Android. The database
upgrade adds no plaintext columns. Older-format snapshots cannot inject the new
authority; existing completion, progress, authoring and publication requests retain
their exact saved content through migration. Persisted observations are historical
display data, never fresh GET permission or current-source planning evidence.

There is at most one unresolved command per ledger instance, because different
members of that instance share an aggregate revision. The journal retains the
instance/member/operation identities, typed command and original request bytes.
Submitted ambiguous requests cannot be discarded as if no write happened. An
exact receipt removes only its matching journal and durably records the minimum
instance revision that a subsequent read must reach. Receipt settlement and that
catch-up target are one encrypted write; a failed save retains the prior custody.
An older or equal-revision receipt does not replace a newer review observation.

The observation cache is bounded to 256 complete instances, 20,000 retained
members and an 8 MiB serialized occurrence-state budget. Up to 64 commands share
a 1 MiB original-request budget. Journal and receipt-target observations are
pinned; only unpinned observations can be evicted. The containing planner's
existing envelope limits remain unchanged. Cache eviction does not imply loss of
an instance's server history, nor does consuming a terminal cursor imply every
instance remains cached.

Terminal installation checks an exact pre-read state capture, correct list/delta
mode, cursor progression and cycles, globally increasing change sequences and
immutable per-instance history. The entire supplied chain is bounded to 128
pages, 32 MiB of compact serialized wire data and 40,000 member visits. Every
outstanding receipt target must be covered by snapshots from that read chain;
existing cache or receipt snapshots cannot substitute. Incomplete, contradictory,
oversized or stale chains leave both the prior cache and cursor intact.

A changed terminal checkpoint durably requires remote schedule catch-up,
including changes that preserve all visible statuses. Clearing that latch requires
an exact captured state, no unresolved command/receipt targets and a terminal
checkpoint; the caller must additionally prove a fresh authenticated remote
composition completed against that capture. These pure persistence transitions
do not themselves fetch data, publish schedules or grant local helper authority.

The connected review/replay coordinators described below supply foreground and
reconnect handling, schedule invalidation, process-local leases and operation
generations. Serialized state alone cannot authorize a fresh edit or local
composition.

## Connected native review and recovery

The macOS calendar inspector and Android protected occurrence sheet resolve the
producer-issued recurring root and planner identity through exact lookup. They
show the complete member tree, including members with no calendar block, with
required-descendant counts, leaf Done/Skipped/exact reopening and qualified
required-edge/parent policy choices. Trees are flattened iteratively rather than
rendered recursively. Known Habit/Event roots do not enter this separate review.
Recovery remains reachable when the source or calendar placement disappears:
macOS Settings lists saved requests, while Android Calendar exposes bound saved
occurrence history. All occurrence-derived content remains protected, even when
the canonical template itself is public.

Historical encrypted observations cannot authorize a new edit after restart.
Current review leases bind selection ownership, credential configuration, privacy,
canonical/execution evidence and a monotonic process-local occurrence generation.
The first delivery re-reads exact current evidence and saves the submitted marker
before PUT. A submitted request can replay its original bytes without a current
template, selected block or preliminary GET. Only a named definitive PUT rejection
makes submitted intent discardable; a GET failure never proves that PUT had no
effect. Receipt settlement preserves newer observations and creates durable
minimum-revision catch-up targets.

Terminal reads run even while a command remains unresolved. Installation requires
the exact captured ledger and read coverage of every receipt target. A failed
delta can try one bounded cold list without replacing its previous durable cursor
or dropping custody. Changed history, including status-preserving policy changes,
requires a fresh authenticated remote composition before the schedule catch-up
latch clears. Recovering an old publication receipt is insufficient. Android's
fresh-composition witness also permits a genuinely new publication request that
deduplicates to the same current revision; a changed revision ID is not used as a
substitute for fresh composition.

Helper v1 cannot represent recurring Task/Routine authority. The separate
[authenticated planning-witness endpoint](routine-planning-witness.md) qualifies
current-source inputs, but native helper-v2 adapters remain disabled pending
encrypted witness custody and installation/recovery fencing.
Native preview/install/publication and fresh execution-Defer checks are fenced
against occurrence reads and mutations; already frozen publication and execution
requests retain exact replay custody. An empty never-managed occurrence checkpoint
does not by itself disable local composition for a nonrecurring workspace. This
does not implement per-step partial inactive deferral, cadence or semantic rebase.

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

The subsequent encrypted-custody checkpoint passes 1,100 executed macOS tests
with warnings denied, including 17 new persistence tests; three opt-in
integration tests are skipped (1,103 total). Its focused occurrence gate passes
41 tests across wire, transport and persistence. Android verification for this
checkpoint passes 1,697 JVM tests, including 23 new state/persistence regressions;
two opt-in integration tests are skipped (1,699 total). Room's generated schema
24 retains the single encrypted snapshot table. The new SQLCipher migration
instrumentation test compiles; it has not yet run on an emulator or physical
device. These are durable-model and encrypted-store gates, not live routine
replay, two-client/service convergence or owner-device acceptance.
Android lint completes with zero errors and the same 29 warnings in existing
files; none are in the occurrence additions. Temporary macOS test-runtime copies
were moved to Trash after verification; the installed toolchain was unchanged.

## Exact lookup verification

The exact-calendar lookup checkpoint passes all 679 API tests against a new
disposable PostgreSQL database, with database-only cases enabled and zero ignored
tests. The maintenance-only wire fixture emitter is deliberately filtered out.
Its focused gate passes eight private HTTP tests, seven real PostgreSQL scenarios
and four shared wire-contract regressions. Coverage includes malformed/duplicate
selectors before storage, read-only scope, foreign-workspace isolation, uncached
identity resolution, unchanged canonical/ledger state, refreshed evidence after
harmless source edits and historical review after definition drift or missing
source. Workspace all-target/all-feature Clippy with warnings denied and
formatting pass. The owned database was stopped and its absence verified.

The lookup server gate alone does not establish native interaction or
two-client/service convergence.

## Connected native verification

The connected client checkpoint passes 1,122 executed macOS tests with warnings
denied (three opt-in skips; 1,125 total) and 1,717 Android JVM tests (two opt-in
skips; 1,719 total). The focused macOS occurrence run passes 58 tests across five
suites. New coverage includes exact uncached lookup, encrypted first-send/restart
custody, lost replies, sibling instance CAS, historical receipts without rollback,
late privacy/selection/canonical changes, terminal revision coverage and bounded
cold recovery. Android additionally exercises a real canonical-sync coordinator
with mocked HTTP transport to prove that a fresh new-key publish can clear the
latch even when the service deduplicates to the same revision ID and timestamp.
Existing whole-occurrence moves still go through remote composition/publication;
helper-v1 local provenance and unresolved occurrence custody cannot authorize them.
Managed execution-Defer review loses permission after occurrence invalidation or
restart, while already submitted bytes retain their exact replay path.

Android lint has zero errors and 34 warnings in existing dependency/platform/UI
files; none are in the occurrence additions. The three new inert-host Compose
tests and existing Room migration test compile, and both debug app and test APKs
assemble. Instrumentation has not run on an emulator or physical device. The
retained macOS test-runtime copy was moved to Trash after verification; the
installed toolchain and owner services were unchanged.

These are deterministic native transport/store/presentation and build gates, not
a two-native-client live-service convergence run, visual acceptance, production
device authentication/TLS trial, final signed release or owner-device acceptance.
The subsequent controlled service gate below supplies a separate bounded live
convergence result, without changing these device and release limits.

## Controlled native service convergence

The [nine-phase macOS/Android gate](native-routine-occurrence-convergence.md)
passes against one fresh disposable service and PostgreSQL cluster. It covers
real first publication of a six-member subtree, competing member outcomes,
lost successful replies, API/client restarts, exact historical receipts without
rollback, parent Keep open/Automatic changes, exact manual blocker reopening,
terminal catch-up and lost-publication recovery followed by fresh composition.
Independent SQL joins every immutable command and publication receipt and
confirms unchanged canonical templates, complete manifests and an unchanged
second-instance sentinel. The public current-schedule read matches the final
native/SQL proof and terminal occurrence head while retaining optional work.

This gate found and fixed a public-reader root-only reference check. Descendant
references now require the retained, digest-bound planning topology and exact
recurrence/lifecycle membership rather than today's canonical tree. Omitted Inbox
members remain in full lifecycle evidence without gaining planned references;
legacy root-only v5 compatibility remains. The private witness is not exposed by
the reader. Eight new regressions include both schemas and a 5,000-level tree.

The full API gate now passes 687 tests with database-only cases enabled and the
maintenance fixture emitter excluded; workspace warnings-denied Clippy passes.
Full native regression results are 1,122 executed macOS tests plus four opt-in
skips and 1,717 Android JVM tests plus three opt-in skips. The nine native live
phases run separately from those opt-in skips. All owned services and the exact
retained test runtime were cleaned up. This is not physical Room/Keystore, native
UI, production TLS/auth lifecycle or owner-device acceptance; helper-v2 native
composition remains disabled pending native integration of qualified
current-source evidence.

## Remaining integration

- Expand the bounded six-member service gate to deep native cascades and
  physical UI/lifecycle/owner-device acceptance. The controlled first-admission,
  competing choices, lost replies, restarts and immutable SQL gate is now covered
  separately above; it does not establish these broader acceptance conditions.
- Native helper-v2 adapter integration and encrypted current-source witness custody.
  Existing native local-composition and publication fences conservatively require
  remote composition; helper v1 cannot consume occurrence authority.
  The private occurrence review API retains first-source revisions and opaque
  current eligibility. The separate [planning-witness endpoint](routine-planning-witness.md)
  qualifies an exact current-source/terminal snapshot with authoritative
  normalization and helper parity; it neither publishes nor advances the ledger.
  A native client must not guess that witness from its canonical cache or mutate
  template statuses to feed helper v1. Nonempty execution evidence and manual
  placement policy still need wider helper representation.
  The connected controls use remote recomposition until the versioned local
  bridge has fully qualified evidence and native installation fences.
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
