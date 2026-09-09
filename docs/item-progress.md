# Independent item progress

Status: backend and both native clients implemented and covered for the
independent-component slice of `DOM-004` and `HIE-003`; controlled cross-client
convergence and owner-device acceptance remain open. This is separate
from [recorded descendant summaries](hierarchy-progress.md). Automatic parent
completion and required-component policy remain subsequent authoritative work.

## Component contract

An item may have zero through sixteen independent components. Each component
has a stable, non-nil UUID `id`, a nonblank `name` (at most 80 Unicode scalars),
and a tagged `value`. IDs are unique within the item. Array order is presentation
order. Names and units have no leading/trailing whitespace or control characters
(U+0000–001F or U+007F–009F). They inherit the item's current and sticky pending
privacy; they are never diagnostic/log fields.

Supported values:

- `{"type":"percentage","basis_points":4250}` represents 42.50%, with an
  integer range of 0 through 10000.
- `{"type":"time","elapsed_seconds":3600,"remaining_seconds":null}` records
  self-reported elapsed time and optional remaining time. Each integer is between
  0 and 3155760000 seconds (100 Julian years). Missing knowledge is explicit
  `null`, not zero. These values are independent records, not execution-session
  credit, timer corrections, or scheduling-demand overrides.
- `{"type":"quantity","current":"3.5","unit":"chapters","target":{"value":"12","direction":"at_least"}}`
  records a named quantity. `target` may be `null`; direction is `at_least` or
  `at_most`. Units have at most 32 Unicode scalars. Decimal values use canonical
  plain strings: optional minus, integer part without leading zeros, optional
  fraction of one through six digits without trailing zeros, no exponent,
  whitespace, plus sign, or negative zero. Maximum absolute value is
  999999999999.999999. No floating-point conversion is permitted.

These are explicit product defaults for new progress authoring, not newly
invented discovery answers. In particular, quantity targets describe direction
without assuming that every measure increases. Unlike units are never summed.
Do not silently import or overwrite existing `goal_measures` or habit outcomes.
100%, zero remaining, or meeting a quantitative target never changes lifecycle
in this checkpoint. Parent own progress and descendant totals remain separate.

## HTTP and persistence contract

`GET /v1/items/{item_id}/progress` returns a complete item-scoped snapshot:

```json
{
  "schema_version": 1,
  "item_id": "00000000-0000-4000-8000-000000000001",
  "item_revision": 7,
  "revision": 0,
  "components": [],
  "updated_at": null
}
```

The item must exist and be active (not trashed). `item_revision` is the current
joined canonical revision from the same read snapshot, not the revision when
progress was last edited. Only an admitted GET can prove the initial empty
revision-zero state. Positive progress revisions have a server timestamp;
clearing components retains a positive monotonic revision and audit history.

`PUT /v1/items/{item_id}/progress` replaces the reviewed component collection:

```json
{
  "schema_version": 1,
  "operation_id": "00000000-0000-4000-8000-000000000002",
  "expected_item_revision": 7,
  "expected_progress_revision": 0,
  "components": []
}
```

Success returns `{ "operation_id": UUID, "replayed": false, "progress": SNAPSHOT }`.
The same operation/body returns its exact original progress result, with
`replayed: true`, even after a newer edit or item deletion. Different content
under the same operation ID conflicts. Operation identity is workspace-wide,
bound to item identity and exact semantic request content; no TTL may silently
turn an ambiguous old replay into a new write. Existing standard no-store,
authentication, request-size, and replay-header conventions apply.

The successful PUT must include exactly one `Idempotency-Replayed` header whose
`true` or `false` value matches the body. Both routes use `Cache-Control: no-store,
max-age=0` and `Pragma: no-cache`. These specific error codes identify rejected
progress intent; a generic response with the same HTTP status does not:

| HTTP | Code | Meaning |
| --- | --- | --- |
| 409 | `item_progress_item_stale` | The canonical item revision no longer matches. |
| 409 | `item_progress_revision_stale` | Another progress edit changed the baseline. |
| 409 | `item_progress_operation_reused` | That operation identity belongs to different content. |
| 404 | `item_progress_item_missing` | No live item exists for a fresh write. |
| 422 | `item_progress_invalid` | The progress command violates the supported contract. |

Malformed/duplicate-key responses, inconsistent replay headers, authentication
failures and gateway/transport failures retain submitted custody. Definitive
rejection permits an explicit re-review or discard; it never silently rebases the
saved request.

A single transaction holds the account-deletion and canonical workspace/item
authority, checks both revisions, stores the sidecar, and records an immutable
operation receipt with before/after audit values. It does **not** increment the
canonical item revision, alter its timestamps/status/effort, publish a synthetic
item delta, invalidate the schedule, or change active execution. All known item
kinds and nondeleted statuses, including parents, blocked/terminal items, and
active timers, can have independent progress. Legacy full replacement, proposal
undo, imports, trash, and restoration preserve sidecar values. Physical purge
must include the new guarded tenant tables in the established deletion policy.

The new routes use existing item-read/item-write scopes. Definitive stale-item,
stale-progress, missing-item, and operation-reuse failures must be distinguishable
from generic gateway/transport errors; clients must never discard uncertain
requests on a generic status code. Exact outcome custody is checked before fresh
item-state preconditions on replay.

## Native editing and synchronization

The initial review entry is the selected canonical item's detail/inspector,
including Goals/Projects. It is independent of full-item replacement eligibility.
Require admitted identity, safe ancestry/privacy, trusted storage and a known
progress baseline. An unsubmitted local item must sync its creation before a
server progress baseline can exist. Show independent components separately from
child totals, stored estimates, goal metadata and execution-derived time.

Keep confirmed observations and reviewed queued values distinct. Journals retain
exact request bytes, operation ID, item/progress revisions, request version and
credential binding before transmission. Recheck authority before first send;
ambiguous/submitted bytes never rebase. Late success settles only its exact
operation and never replaces a newer observation. Conflict resolution requires
an explicit new review; refresh never erases local intent.

The encrypted journal also retains whether the reviewed intent was sensitive.
This sticky presentation protection survives an intervening reparenting or
sensitivity downgrade and is preserved through explicit re-review. It is local
privacy metadata, not an extra PUT field. Recovery remains accessible for a
deleted/missing item without disclosing its component names or values while
protected. Ordinary quantity input such as `3.50` may normalize to `3.5` before
reviewed bytes are frozen; stored and received decimal strings remain strict.

There is no sidecar invalidation stream in this checkpoint. On selection,
foreground/reconnect, and approximately every five seconds while its protected
detail is visible, refresh that item's GET. Cancel view refresh on lock, account
change or disappearance. Replay the existing bound outbox through the normal
foreground/service lifecycle, not only when a detail is open. Back off on errors;
never claim cached values are current server data or turn a failed GET into empty
progress. A current-item revision mismatch triggers canonical catch-up before
new editing; historical replay confirmation is distinct from current read proof.

Persist the cache and journal only inside the encrypted planner snapshot. Bound
confirmed-cache eviction must not evict pending/ambiguous custody. Include new
state in credential-transition blockers, reset/teardown/quarantine, privacy
generations, restart replay and rollback on failed durable writes. Older snapshots
migrate to explicitly empty progress state only after rejecting injected new
fields; new snapshots must require complete validated state. Existing submitted
authoring/execution/proposal bytes and version markers stay unchanged.

## Verification and remaining work

The checkpoint was verified on 2026-09-09 using synthetic data and disposable
local services only. No production app session, owner calendar, paid deployment,
or live provider account was used.

- [Shared fixtures](../fixtures/item-progress/README.md) cover 37 exact decimal
  strings, 21 Unicode labels, five valid component collections and 35 invalid
  collections. Rust, Swift and Kotlin enforce the same contract, including
  terminal newlines, supplementary scalars and required nullable fields.
- The full Rust workspace/PostgreSQL gate passed 914 tests in 51 test groups,
  including normally ignored live-database tests. Focused final HTTP/OpenAPI,
  strict SQL, all-kind/status and fixture checks also passed. Core and API
  Clippy passed with warnings denied. Coverage includes concurrent two-revision
  CAS, permanent historical replay, forged audit/sidecar rejection, tenant and
  account-deletion fences, active execution and canonical-state noninterference.
- The full macOS gate passed 975 tests in 61 suites with compiler warnings
  treated as errors. The 29 progress tests cover strict transport, exact values,
  encrypted migration to snapshot 26, duplicate-key rejection, restart/replay,
  retained conflicts, privacy suspension and a synthetic three-mode editor
  whose persistent field labels were visually inspected. Pending and inherited
  sensitivity survive downgrade/restart; exact in-flight success or rejection
  permits only monotonic privacy changes. Missing-item recovery is content-free,
  and discard binds the displayed operation ID rather than just the item.
  All 25 older snapshot labels reject injected progress state before migration.
- Android passed 1,557 JVM tests in 123 suites, including 44 progress tests,
  plus lint (zero errors; 29 pre-existing warnings) and both debug APK builds.
  Nine inert Compose tests and the SQLCipher/Room 21-to-22 migration passed on
  an isolated emulator. These cover entry for a completed goal, exact editing,
  unknown time, unavailable-item recovery, lock cancellation and sticky
  `FLAG_SECURE` protection after a sensitivity downgrade. Synthetic observation
  and editor screenshots were inspected; the test emulator was stopped.
- Native journals preserve reviewed bytes across storage upgrades and uncertain
  replay. Failed saves cannot promote optimistic progress into durable proof;
  uncertainty blocks destructive credential replacement, reset and discard.
  A generic HTTP status is not accepted as definitive rejection.

These are separate native and service checks, not proof of a real two-client
session. Controlled client/service convergence, refresh/offline timing and
owner-device acceptance remain open. General/weighted progress aggregation,
execution-derived reconciliation, required-component policy and automatic
completion must not be claimed by this independent-component checkpoint.
