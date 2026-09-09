# Authoritative parent completion

This server checkpoint implements the one-off portion of [parent completion](hierarchy-completion.md).
It does not finish `HIE-004`: native review/override controls, encrypted offline
completion intent, qualified recurring-instance integration, and owner-device
acceptance remain separate gates. Do not describe the full feature as released
or deploy this server checkpoint as a completed native experience.

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

The existing native Add Subtask/parent-selection guards still reject terminal
parents. They must be updated to use fresh, revision-bound completion policy
before exposing policy-qualified completed parents. Server admission alone is
not evidence that this native workflow is usable.

The next native slice is a separate completion review and encrypted intent
ledger, then qualified Add Subtask/parent selection. It must invalidate review
permission after any admitted canonical or execution change, not merely a change
to the selected item's revision. Preserve entered fields while requiring a fresh
review; never silently replace the evidence in an already-submitted request.
An exact historical receipt settles only its original intent and is not a current
GET proof or permission to overwrite the current forest. Complete canonical
catch-up must remain available while that intent is being recovered. Keep legacy
terminal-item content editing read-only rather than widening its existing draft
contract as part of policy controls.

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

## Verification

The complete `dayweave-api` regression passes against an isolated live PostgreSQL
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

All-target `dayweave-api` Clippy with warnings denied and workspace formatting
checks pass. Staged/history/outgoing credential scans gate commit and push;
private database files and diagnostic logs are not repository artifacts.
Native controls and cross-client/device acceptance remain unfinished; these
server checks do not establish completion of the full feature.
