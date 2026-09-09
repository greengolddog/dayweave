# Required descendants and parent completion

Status: implementation in progress for `HIE-004`. A pure iterative engine and
the [versioned one-off server checkpoint](item-completion-api.md) implement
completion policy and synchronous canonical reconciliation. Native completion
review, required-edge/manual-mode controls, encrypted intent and qualified parent
admission now pass native automated regression/build gates. The full requirement
still includes controlled durable
cross-client behavior, qualified recurring instances and owner-device acceptance.

The accepted source is [requirements](product-requirements.md#43-hierarchy-goals-projects-routines-and-dependencies)
and [discovery answers 203–207](discovery-answers.md). The detailed defaults below
are implementation decisions; they are not additional answers attributed to the
owner. [Recorded summaries](hierarchy-progress.md) and
[independent progress components](item-progress.md) remain separate concepts.

## Completion rules

- A child relationship is required by default. The owner may explicitly make
  that relationship optional. An optional edge excludes its entire branch from
  that ancestor's completion requirements; the branch still evaluates its own
  requirements independently.
- Automatic completion requires at least one required descendant, and every
  descendant reachable through required edges must be completed. A newly empty
  goal, or a parent whose children are all optional, must not complete merely
  because there is no required work. Neither skipped nor cancelled means done.
- Evaluate bottom-up. An automatically completed child can satisfy its own
  required position in the next ancestor. An open required grandchild still
  prevents automatic ancestor completion even if its immediate parent was
  manually marked complete. Manual overrides affect only their own item; they
  do not silently waive another ancestor's requirements or mutate descendants.
- The explicit modes are **Automatic**, **Keep open**, and **Mark complete**.
  A manual mode remains visible and persists until the owner explicitly returns
  to Automatic. Mark complete requires review of the unfinished requirements;
  it does not stop a child's timer, cancel remaining tasks, or rewrite recorded
  work. Keep open suppresses automatic completion of its item.
- Record completion provenance and the previous open status. When requirements
  regress, an automatically completed parent reopens to that recorded status.
  Clearing a manual completion also needs an explicit preserved reopening
  status; do not invent one from a completed record. Ordinary completed leaves
  without automatic provenance remain ordinary recorded completion.
- A structural parent with an ambiguous terminal status and no completion
  provenance requires an explicit policy/reopening review. It must not become
  an invisible permanent override. In particular, preserving ordinary terminal
  leaves is not permission to silently preserve a cancelled/skipped parent or a
  completed parent with unfinished required descendants. Canonical CRUD admits
  completed parents only with retained completion provenance; actively executing
  and ambiguous terminal parents remain inadmissible. Migration never invents
  reopening evidence.
- Reopening, reparenting, restoring, removing a required relationship, and
  changing a manual mode must evaluate affected old and new ancestry. No stale
  automatically completed prerequisite may authorize scheduling or publication.
- Percentage, elapsed/remaining time and quantity targets remain informational
  for this rule. Reaching 100%, zero remaining or a quantity target is not a
  lifecycle command. A configurable required-component policy would need its
  own explicit contract; it must not be inferred from the independent sidecar.

## Recurrence and execution boundaries

A canonical recurring template is not evidence that any particular occurrence
has been completed. One-off evaluation reports unresolved occurrence evidence
for a required recurring branch instead of converting template statuses into
achievement. A separate evaluation may use an explicitly qualified occurrence
and the complete corresponding tree, with lifecycle supplied for that exact
occurrence. Nested independent recurrence must not inherit proof from an outer
occurrence merely because it is a descendant.

An unqualified recurring node emits no lifecycle or provenance transition,
including manual completion or reopening. Reporting unknown occurrence evidence
only in an ancestor's counts is insufficient: the node's own decision must also
withhold that mutation. An otherwise qualified one-off ancestor may still reopen
when its required branch becomes unproven.

Occurrence results never complete a permanent series or another occurrence.
Authoritative occurrence outcomes, not a bounded execution-history window,
must supply the evidence. This engine does not award time, settle execution
commands, stop reservations, or infer completion from estimated duration.

## Decision engine boundary

The shared engine takes a complete normalized active forest, explicit policy,
recorded lifecycle/provenance and evaluation scope. It validates duplicate IDs,
missing parents, cycles and incomplete topology before producing decisions.
Traversal must remain iterative for at least 5,000 levels. Counts must be
checked, and the result must not depend on input order.

Results describe proposed status/provenance, reasons and required-descendant
counts. They are not authenticated read proof, a persisted completion receipt,
or permission to release sensitive aggregate data. A caller must establish a
coherent authoritative snapshot and apply decisions through the canonical
mutation boundary.

Implementation: [shared evaluator](../crates/dayweave-core/src/hierarchy_completion.rs)
and [regressions](../crates/dayweave-core/tests/hierarchy_completion.rs). The
types are Rust-only and do not change an existing wire or persistence schema.

Verification on 2026-09-09: all 22 focused completion tests and all 187 core tests
passed, with no failures or ignored tests. All-target/all-feature core Clippy
passed with warnings denied. Coverage includes a 5,000-level completion/reopen
chain, input-order independence, optional branches, non-vacuous completion,
every open/skipped/cancelled leaf status, manual release/pinning, ambiguous
terminal-parent rejection, qualified/nested recurrence boundaries, invalid
topology and retained provenance. Independent semantic review identified and
checked the terminal-parent and unqualified-occurrence regressions. These are
pure-engine results, not server or physical-device acceptance.

## Server checkpoint and native integration in progress

The [server policy/read/command contract](item-completion-api.md) binds explicit
review to the item, policy and evaluated requirement/execution evidence, with
exact durable operation replay. It preserves policy and completion provenance
across legacy full-item replacement, proposal apply/undo, imports and snapshot
restoration. No old submitted request may silently reset the new policy.

All canonical writers now use the same synchronous completion boundary.
Unlimited-depth derived updates coexist with the existing 300-record/8-MiB
atomic delta-group bounds. Server transaction grouping and historical receipts
have focused live tests; final verification is recorded in the API checkpoint.
Native terminal-cursor hydration after these actual cascades remains open. A
background ancestor worker alone is not sufficient.

The application strategy is synchronous evaluation under the existing
execution/workspace lock, before the transaction commits. Keep the primary
direct or compound delta group intact, close it, then partition derived ancestor
changes into separately bounded, contiguous groups in the same transaction.
This avoids exposing a committed child change alongside stale prerequisite
completion. Startup reconciles valid legacy current state before readiness.
The implemented writer boundaries are:

| Writer | Server boundary |
| --- | --- |
| [Direct item CRUD](../server/dayweave-api/src/persistence/item_repository.rs) | Finalize after the direct group, retain the exact direct-operation receipt, evaluate old and new ancestry, and publish derived status changes before commit. Do not replace the transaction-local group setting inside an unfinished primary group. |
| [Proposal preview/apply/undo](../server/dayweave-api/src/persistence/proposal_application_repository.rs) | Evaluate only after the entire command batch, before final snapshot/diff/fence capture. Preview and undo simulation must use the same path. Preserve changed-item and undo-fence accounting even when an affected ancestor recomputes to an unchanged value. |
| [Calendar/Tasks imports](../server/dayweave-api/src/persistence/google_sync_repository.rs) | Derive affected ancestry from changed item identity and old/new membership, not the topology-only parent-refresh iterator. Reconcile provider-mapping revisions when completion changes a mapped item after the direct import result. |
| Native receipt settlement | An exact historical successful response may settle its original journal after newer canonical state arrives. Never require that newer state to still match the older draft; never replace newer status, deletion or privacy evidence with the historical receipt. |

Both native delta loaders collect through a terminal cursor before installing a
new complete forest. This is the relevant hydration boundary for multiple
bounded groups in one server transaction; a response page by itself must not be
treated as proof that all ancestor changes have arrived.

Per-group bounds do not remove the total hydration bounds: macOS currently
limits one catch-up to 20,000 changes/32 MiB retained data/100 pages; Android
limits it to 25,600 changes/512 pages and a 24-MiB folded canonical cache, with
at most 10,000 active-plus-retained-tombstone identities in that folded state.
Repeated deep complete/reopen cascades can exhaust a fresh client's historical
bootstrap budget even when its current forest is small enough. The separate
[current-state bootstrap](item-sync.md#bounded-current-state-bootstrap) captures
a bounded immutable current forest and recent tombstones, then resumes the
ordinary stream at the captured head. Its history-heavy server and deep native
tests are a prerequisite, not evidence of native convergence after completion cascades.
The integrated completion feature still needs cold-client acceptance after real
required-descendant complete/reopen transactions. Truncated history must never
masquerade as a complete forest.

Existing full-authoring receipts require the exact reviewed draft and revision.
An automatic side effect must not rewrite that response into an incompatible
successful receipt. Canonical response fields are also compatibility-sensitive:
Android rejects unknown fields, and macOS retains them as read-only. Completion
policy and override commands use an independently versioned contract rather
than widening old status journals or silently changing their wire bytes.

Structural admission distinguishes policy-qualified parent completion from
executable lifecycle. The shared `is_executing_state` predicate still includes
Completed, Skipped and Cancelled; server parent/non-leaf guards now permit only
provenance-qualified Completed parents. The native development paths now use
current completion GET authority for parent selection and attachment. Before
the first child send, a fresh parent read is scoped to that exact queued intent
and unchanged local evidence; the child cannot invalidate its own admission,
but no other pending authority is waived. The proof does not authorize another
child or policy review. Submitted child requests still replay exactly without
fresh parent preflight. This does not add a parent-revision CAS field to the
legacy request or permit terminal-item content replacement; the server's atomic
parent guard remains authoritative. Do not broadly permit actively executing,
ambiguous terminal or unqualified recurring parents.

The native [historical receipt recovery](item-sync.md#historical-authoring-receipts)
checkpoint addresses exact replay after newer state has arrived. Its store and
sync regressions are prerequisites only; they do not verify server completion
cascades or proposal undo. Bounded cold-client hydration is a separate sync
checkpoint, not an increase in the completed parent-policy scope.

Both native clients now contain protected completion explanations, required-edge
editing, reviewed override/resume-automatic controls, encrypted offline intent,
conflict/retry/discard custody and explicit storage upgrades. Their current GET
authority is process-local and invalidated by canonical/execution evidence or
pending-authority changes, not merely the selected item revision. Restart and
historical receipts do not restore it. Entered choices must survive stale review;
only an explicit refresh/re-review can replace the baseline for a new request.
Submitted bytes and operation identity remain unchanged.

A successful receipt durably fences further review until complete canonical
catch-up; it never locally fabricates lifecycle or overwrites newer canonical
state. Outbox recovery is independent of visible detail. Completion-derived
summaries, policy/reopening review and retained intent are always protected as
sensitive, even after a fresh GET when the cached tree appears public. Version 1
has no privacy or canonical-cursor witness, and remote sensitive descendants can
change counts without changing the selected ancestor's revision. A fresh opaque
review hash cannot prove local privacy completeness. This conservative rule
does not change authorization, revision/evidence CAS or the closed wire, and
continues across stale observations, refresh and restart. The
[native API and migration boundary](item-completion-api.md#native-client-checkpoint)
describes this checkpoint and its remaining acceptance limits.

Full macOS warning-as-errors tests and Android JVM/lint/debug-build gates pass,
including encrypted migration/restart, privacy, stale-review and first-send/exact
replay coverage. The synthetic macOS review was visually inspected. Ten Android
completion UI tests and its Room migration test compile but have not yet run on
a device/emulator. Controlled cross-client mutation/cascade hydration,
qualified recurring instances and owner-device acceptance remain part of the
full feature's completion gate. Keep the separately verified pure-engine and
server counts above distinct from native tests and unfinished acceptance gates.
