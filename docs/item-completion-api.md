# Authoritative parent completion

The verified server checkpoint implements the one-off portion of
[parent completion](hierarchy-completion.md). Native review/override controls,
encrypted completion intent and policy-qualified parent admission now pass the
native automated regression/build gates described below. A controlled nine-phase
cross-client/service run on a four-to-five-item tree also passed on 2026-09-10.
This does not finish `HIE-004`: deep native cascade acceptance, qualified recurring
instances and physical-device acceptance remain separate gates. Do not describe
the full feature as released or treat server coverage as native acceptance.

## Review and commands

`GET /v1/items/{item_id}/completion` requires `items:read` and returns schema
version 1, current canonical revision, completion state, required-descendant
counts, unresolved-occurrence indication, and an opaque `sha256:` evidence
hash. The evidence covers the complete active canonical forest, the completion
policies of that active forest, and current execution revision/live identity. The initial implementation
uses a conservative workspace-wide review: even an unrelated current item or
execution change may require refreshing the review.

`PUT` on the same route requires `items:write`. The closed request contains:

| Field | Meaning |
| --- | --- |
| `schema_version` | Exactly `1`. |
| `operation_id` | Client-generated non-nil UUID; one permanent identity across all items in this workspace. |
| `expected_item_revision` | Exact positive canonical revision reviewed by the owner. |
| `expected_completion_revision` | Exact completion-policy revision; absence is revision `0`. |
| `expected_evidence_hash` | Exact complete-forest/execution proof from the review. |
| `required_for_parent` | Whether this edge and its branch are required by ancestors. Defaults to true only for absent state. |
| `mode` | `automatic`, `keep_open`, or `complete`. Nonautomatic policy requires structural parent authority or retained previously managed state. |
| `reopening` | Required nullable field. Normal clients send null. The adapter validates explicit known reopening evidence, but this checkpoint does not expose a repair-review read for already-corrupt terminal parents. |

The result includes `operation_id`, `replayed`, and the original committed
completion snapshot. Exact replay is checked before fresh revision/evidence
checks. Reusing the operation UUID with any different target or request fails.
Successful retries keep the original result even after later edits or trash;
they do not rewrite current canonical state. The `idempotency-replayed` response
header agrees with the body. Responses are private and `no-store`; JSON errors
never echo private blocker text.

There is no new field in the legacy `Item` JSON or existing item/proposal
requests. Every completion-policy/provenance write advances both the independent
completion revision and the canonical item revision, even when status remains
completed. Existing item and proposal freshness checks therefore notice a
policy-only change.

## Exact reopening custody

Stored state has exactly `item_id`, `revision`, `required_for_parent`, `mode`,
`provenance`, and `updated_at`. Both nullable fields remain required in JSON.
Absent state means revision zero, required, automatic, no provenance or timestamp.
Written state starts at one and never resets during ordinary replacement,
trash/restore, or proposal undo.
Revisions establish ordering, not wall-clock timestamps. A waiting writer may
have captured its time before another transaction committed; exact evidence
timestamps are retained without requiring wall-clock time to advance. The
planner and database adapters normalize the clock to PostgreSQL microseconds
before writing canonical status, completion state, and evaluation/effect evidence.

Provenance distinguishes automatic from manual completion and retains the exact
previous `status`, `blocked_reason_kind`, `blocked_by_item_id`, and `blocked_reason`.
Completion clears the canonical blocker tuple; reopening restores it rather
than changing a blocked parent to planned. Prior status must be `inbox`,
`planned`, or `blocked`; reopening cannot fabricate scheduled, active, or paused
execution. Blocker reasons retain the existing 1,000-scalar trimmed/no-control
contract and dependency identities remain workspace scoped, including trashed
historical blockers.

Ordinary full-item status replacement cannot silently release a managed policy.
Completed parents are admitted for child edits only when retained completion
provenance qualifies them. Actively executing and ambiguous terminal parents
remain inadmissible. An unqualified recurring template or descendant is never
completed using template status, session history, elapsed time, or independent
percentage/quantity progress.

## Atomic writer integration

All supported canonical writers lock execution before the canonical workspace,
evaluate the whole active forest synchronously, and commit primary and derived
changes in one transaction. A live execution identity cannot undergo a derived
lifecycle change. The primary delta group is closed first; derived effects use
separate contiguous groups of at most 300 records and 8 MiB. Preview reserves
delivery overhead too. Complete-forest admission is bounded to 20,000 items and
32 MiB before evaluation; hashing streams rather than allocating another full
serialized body.

Server startup reconciles the configured personal workspace before readiness
or request consumers are constructed. A valid legacy open parent whose required
descendants are already completed therefore does not wait for another edit.
Repeated startup is a no-op once reconciled. Initialization failures prevent
readiness; startup does not silently update provider mapping baselines or old
mutation receipts.

- Direct create/replace/trash/restore retains the exact original primary item
  receipt, even if reconciliation emits a newer revision of the same identity.
- Proposal preview/apply/undo runs completion after the entire command batch.
  Derived lifecycle changes appear in preview differences. Server-only inverse
  companions retain completion state and restore semantic policy with monotonic
  revisions, without widening old proposal snapshots or receipts.
- Fresh proposal undo compares complete post-apply completion/execution evidence,
  including unchanged required branches. Historical successful undo still replays
  first. An old application lacking completion evidence may freshly undo only
  while the workspace has no stored completion policies; otherwise a new review
  is needed. Direct undo fences still require a newer revision; only qualified
  non-direct fences may remain unchanged.
- Google import/projection/sweep finalization occurs after provider batch mapping
  writes. Only an exact matching external mapping revision follows a derived
  revision. Local forks, conflicts, and existing outbound approval payloads are
  not silently rebased.

Native consumers must still finish a terminal delta/cold-bootstrap cursor before
installing a complete forest. The existing [bounded current-state bootstrap](item-sync.md#bounded-current-state-bootstrap)
and historical-receipt recovery are prerequisites, not native acceptance of this
new completion policy.

## Native client checkpoint

macOS and Android now have separate completion models, strict GET/PUT transports,
protected review controls, encrypted intent and recovery integration. Required-edge
editing is independent of full-item replacement; manual modes remain limited to
structural parents or retained managed state. Completed-item content editing is
still read-only. These are verified implementation slices, not a finished
native release or acceptance of the full completion requirement.

A saved observation is not permission to write. Current review requires a
process-local GET proof bound to the exact connection, canonical item revision,
and complete local canonical/execution evidence generation. Canonical or execution
changes and pending authority transitions invalidate that proof, even when the
selected item's revision is unchanged. A restart, lock or account change cannot
restore permission from encrypted observations or successful PUT receipts. The
server's opaque evidence hash is retained, never recreated from local counts.

Review retains the exact chosen requiredness and mode with item/policy CAS and
the original evidence hash. Submitted request bytes and operation identity are
immutable across retries and restart. A definitive no-effect result permits
explicit re-review or discard; ambiguous failures retain custody. Stale reviews
must preserve entered choices while requiring an explicit fresh review, not
silently replace the evidence inside an already-submitted request. Foreground
detail refresh and the outbox have separate lifetimes, so recovery does not
depend on leaving an item inspector open.

Receipt settlement removes only its exact intent and durably records the need
for canonical catch-up. It does not install lifecycle from the receipt or
overwrite a newer item, trash entry or deletion record. The catch-up path remains
available under that recovery fence and clears it only after a terminal
delta/current-bootstrap result is durable. Another completion review needs a
new GET after catch-up.

Completed-parent selection and new child attachment require revision-matching
completion provenance, not the status label alone. Immediately before the first
send, the authoring pipeline refreshes the selected Completed parent's completion
under its existing operation. The resulting proof is scoped to that exact
still-unsubmitted child intent and unchanged local evidence; it excludes only
that intent from pending-authority checks and grants no general policy review
or unrelated child permission. Submitted children retain exact replay without
fresh parent preflight. Unknown or malformed ancestry, conflicting/pending
authority and active execution remain fenced. There is no parent-revision CAS
field in the existing child-create request: atomic server parent admission is
the final authority, not a guarantee supplied by the preceding GET.

Completion-derived summaries, policy and reopening review, and retained intent
are always protected as sensitive on both native clients, including a fresh GET
over an apparently public local tree. Version 1 carries neither a privacy witness
nor a canonical-cursor binding: a remote sensitive descendant can change ancestor
counts without changing that ancestor's item revision. The opaque evidence hash
does not establish that the local subtree contains every privacy-relevant change.
A fresh GET therefore cannot relax aggregate privacy. This conservative
presentation and intent rule does not change authentication, revision/evidence
CAS, or scoped parent admission, and does not add fields to the closed wire.
Protection also survives stale observations, refreshed review and restart.
Lock/account boundaries clear transient review authority; unavailable items
remain recoverable without exposing their old content in the outbox.

## Migration and retention

[Migration 0035](../server/dayweave-api/migrations/0035_item_completion.sql) stores
current state, immutable evaluation/effect evidence, permanent reviewed-operation
receipts, and server-only proposal undo companions. Deferred checks bind exact
state transitions to canonical history while permitting more than one valid
revision in the same transaction. Policy-only changes cannot bypass canonical
freshness. Retained completion evidence pins its referenced canonical history.

Migration fails without modifying rows if active terminal structural parents
already exist without known completion custody. The redacted diagnostic reports
a count, not titles or private bodies. Such legacy records require owner-reviewed
reopening through the existing canonical API before retrying migration; no
prior status is invented. This is an explicit migration compatibility check,
not automatic data repair.

If incompatible terminal state appears after migration, completion reads and
writes fail closed. Although the pure command adapter can validate explicit
reopening evidence, the current GET cannot issue a review hash for that invalid
forest. A supported in-app post-corruption repair workflow is not claimed.

Exact policy request/result and derived reopening evidence are retained until
guarded account purge for replay and audit. Proposal-specific inverse text is
scrubbed by existing maintenance after undo expiry while immutable hashes remain. All new
tables participate in deletion fencing and scoped purge inventories; this
does not activate the otherwise unfinished account-deletion workflow.

The native development slice adds macOS encrypted snapshot schema 27 and Android
encrypted JSON payload V23. Predecessor snapshots migrate to an empty completion
ledger while retaining existing canonical, progress and integration journals;
they cannot inject completion authority by relabeling a new payload. Current
snapshots require the completion ledger's explicit nested fields, exact request
custody, enclosing connection binding and sensitivity facts. Runtime GET proof
is never restored. Pending completion identity participates in existing retention,
credential replacement and account teardown fences. Automated migration and
recovery coverage is listed below; Android instrumented migration execution
and owner-device verification remain open.

## Verification

The prior server checkpoint's complete `dayweave-api` regression passes against an isolated live PostgreSQL
instance: 597 default tests and all 29 separately gated tests (626 passed, zero
failures). This includes 392 server-library tests, five completion HTTP tests,
five completion PostgreSQL tests, all 29 proposal application tests, and the real
Google import/cascade/mapping-CAS regression. The deep PostgreSQL scenario runs
four 5,000-level complete/reopen cascades (19,996 derived effects), validates
every delivery group, then hydrates 5,000 current items in 17 pages after more
than 20,000 historical changes and verifies the ordinary cursor handoff.

Nanosecond Google completion/reopening clocks produce identical microsecond
canonical, policy, and audit timestamps. Legacy proposal and execution-claim
fixtures now complete their schema upgrades before using current write
repositories; the proposal fixture proves that the historical snapshot and
its stored hash remain unchanged before a successful real undo.

At that prior server checkpoint, all-target `dayweave-api` Clippy with warnings
denied and workspace formatting checks passed. Staged/history/outgoing credential scans gate commit and push;
private database files and diagnostic logs are not repository artifacts.
The 2026-09-10 full native regression gates passed after the harness addition:

- macOS: 1,059 executed tests passed across 71 suites, including 50 completion
  regressions; three opt-in tests were skipped (1,062 total). Compiler warnings
  are treated as errors. The completion opt-in ran separately in the nine-phase
  live gate below. A synthetic completion review was previously rendered
  and visually inspected; it used no owner data or live service.
- Android: 1,654 passing JVM tests across 136 suites, with two opt-in tests
  skipped (1,656 total), after the composition-clock precision fix. Lint reports
  zero errors and 29 warnings. Both debug APKs and the
  Android instrumentation sources build successfully. The ten completion UI tests
  and Room 22→23 migration test are compiled, not yet executed on a device/emulator.
- Shared wire: 24 valid and 105 invalid cases in
  [fixtures/item-completion](../fixtures/item-completion/README.md) pass native
  admission and seven Rust producer/contract tests. All-target server Clippy
  with warnings denied and workspace formatting checks also pass.

Coverage includes predecessor upgrades without changing existing exact intent,
offline/ambiguous restart replay, definitive conflict custody, stale global
evidence and pending-intent ABA, durable terminal catch-up, privacy revocation,
always-protected completion evidence, and real native authoring pipelines for
first-send parent preflight and exact submitted child replay. These regression
results remain distinct from live convergence and instrumented native acceptance.

On 2026-09-10, the complete
[controlled native completion convergence](native-completion-convergence.md)
command passed all nine macOS/Android phases against one fresh PostgreSQL-backed
service. The four-to-five-item scenario verifies competing reviews and a real
stale 409, a lost successful receipt, encrypted restart and exact historical
replay, durable post-receipt canonical catch-up, optional requiredness, real
automatic completion, and native child creation that reopens both ancestors
with exact provenance. Final scoped SQL verifies four successful reviewed
operations, eight effects and six evaluations, including the unchanged original
lost-response receipt. API, PostgreSQL and idle-sleep guard shutdown and removal of the retained
test runtime all passed before the command reported success.

This is a production-store/transport test, not a physical-device run. The two
independent stores share one legitimately enrolled synthetic device principal;
Android uses the production snapshot codec through a test-only AES-GCM file DAO,
not physical Room/SQLCipher or Keystore instrumentation. Deep native cascades and
large/cold-client acceptance, qualified recurring-instance integration, physical
UI/privacy-lock interaction, production device-auth/TLS behavior and owner
acceptance remain open. The controlled small-tree result does not close those
native or full-feature gates.
