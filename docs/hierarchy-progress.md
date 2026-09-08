# Hierarchy progress and completion

Status: implementation in progress. This document defines the read-only roll-up
checkpoint, not acceptance of `DOM-004`, `HIE-003`, or `HIE-004`.

The full scope remains [product-requirements.md](product-requirements.md) and
[discovery answers 203–207](discovery-answers.md). Independent percentage,
time, and named-unit progress; required-child policy; automatic completion;
and visible manual overrides still require authoritative commands and native
review flows. A count of completed leaves is not a substitute for those features.

## Read-only roll-up contract

1. Derive summaries from the complete admitted canonical forest, never schedule
   blocks, a bounded execution-history window, search results, or collapsed rows.
   Every structural leaf contributes once to its own subtree and each ancestor.
   The root's lifecycle badge remains its recorded lifecycle, independent of this
   summary. This projection never mutates items or grants execution permission.
2. Count nonrecurring structural leaves by recorded lifecycle: completed,
   skipped, cancelled, and open (all other statuses). A structural container
   without children is still a leaf item even without independent effort.
   Label this explicitly as **leaf items**, not required work or weighted goal
   progress. Skipped/cancelled leaves do not count as completed. Do not derive
   completion from durations, goal measures, or execution-session counts.
3. A leaf with recurrence on itself or any ancestor is a recurring leaf. Count
   it separately, outside the one-off lifecycle denominator and duration totals;
   template status does not describe occurrence achievement or series progress.
4. Sum recorded duration estimates for nonrecurring flexible executable leaves
   only, regardless of lifecycle. Task/habit/break leaves contribute; leaf
   project/goal/routine nodes require explicit own effort. Never include stored
   flexible-parent effort. Preserve minimum, expected, and maximum estimates.
   Missing/unknown duration contributes an unknown-estimate count, not zero
   work. These are **recorded leaf effort estimates**, not remaining time,
   actual elapsed work, calendar occupancy, or a prediction of completion.
   Sum canonical seconds exactly before formatting; do not round individual
   leaves into the scheduler's minute units or substitute a remaining override.
   Preserve subminute ranges such as 1/30/59 seconds.
5. Fixed events are not flexible effort. Count nonrecurring fixed events
   separately, including events with children; their authoritative intervals
   and existing scheduling semantics are unchanged. A recurring fixed-event
   leaf appears only in the recurring-leaf count, not one-off event totals.
6. Retained trashed records do not participate in the active forest. Validate
   duplicates, missing parents, cycles, and explicitly incomplete topology
   before publishing totals. Invalid input has unavailable totals, not zeros.
   Evaluation must be iterative and support at least 5,000 nested nodes.
   Exact integer sums must not wrap; overflow makes totals unavailable.
   The shared count and seconds ceiling is the signed 64-bit maximum. Unknown
   future kinds/statuses and malformed duration shapes are unavailable input,
   not permission to guess at their meaning.
7. Native displays use the admitted cached forest. Never imply current server
   authority while offline. If hydration/completeness is unproven, withhold
   totals. Queued authoring/status changes that can affect the graph or these
   totals must not masquerade as confirmed progress: conservatively withhold
   affected summaries (with a clear pending-review/sync explanation), including
   old and proposed ancestors after reparenting. Global withholding while any
   such queue exists is an acceptable conservative initial implementation.
   This includes unresolved proposals, execution commands, and canonical-status
   projections. A known cached item revision behind an applied projection
   receipt also awaits catch-up. Resolved historical receipts are not permanent
   existence requirements: retained deletion records are bounded, and a complete
   cursor-scope rebuild may omit old deleted items. Do not freeze the active
   forest forever because such a historical item is absent. No elapsed work or
   item lifecycle is inferred from those receipts.
8. Privacy applies to aggregate data too. If an included subtree has sensitive
   or unsafe content, conceal its numerical summary in nonsensitive ancestors
   as well as its own protected row. Do not let pending moves or sensitivity
   edits disclose formerly protected data. Reuse existing authority/binding and
   sticky sensitivity rules; queued privacy downgrades do not release summaries.
9. Cache summaries with their graph/authority/privacy inputs. Clock ticks,
   search, selection, and disclosure changes must not rebuild the forest.

## Authoritative completion work that follows

Canonical status currently cannot safely stand in for general progress:
non-leaf terminal replacement is forbidden, unchanged parent membership is
revalidated against terminal parent status, and prerequisite consumers trust
`Completed`. Therefore an eventual ancestor worker alone would be unsafe when
a child reopens. Requiredness, independent progress, completion provenance,
manual override, and exact revision evidence need their own explicit contract.
Legacy full-item replacement and already submitted encrypted journals must not
silently reset that new authority.

All canonical writers—not only HTTP routes—must share completion invalidation:
direct CRUD, proposal apply/undo, snapshot restoration, and integration imports.
Unlimited nesting must coexist with the existing bounded atomic item-change
groups. Scheduling/publication must not accept stale auto-completion while a
bounded cascade is pending. The user-visible policies and their defaults will
be recorded here as implementation decisions, not invented discovery answers.

## Reference implementation and fixtures

The shared-core [canonical-second reducer](../crates/dayweave-core/src/hierarchy_progress.rs)
accepts normalized records before planning. It deliberately does not accept a
pruned schedule as proof of a complete forest. Its estimate units are seconds,
not the scheduler's rounded minute units. It changes no canonical lifecycle and
does not itself establish caller authority or release protected information.

The [cross-platform fixture](../fixtures/hierarchy-progress/projection-v1.json)
contains seven valid complete forests and fifteen invalid cases. Every valid
case specifies the exact summary for every node. Tests exercise normal and
reversed input order; separate native tests cover hydration, journals, privacy,
and presentation. The portable-boundary cases intentionally exceed per-item
canonical authoring limits to test checked arithmetic in the normalized reducer.
They are not sample API requests.

## Verification checkpoint

On 2026-09-08, all 161 shared-core tests passed, including the new fixture and
5,000-level hierarchy regressions. All-target/all-feature core Clippy passed
with warnings denied. An independent upward-ancestry calculation also matched
all seven valid expected fixture maps using exact integers.

The same frozen-source checkpoint passed:

- The full macOS wrapper, `scripts/test-macos.sh -Xswiftc -warnings-as-errors`:
  946 tests in 57 suites. The [new native suite](../apps/macos/Tests/DayWeaveMacTests/CanonicalHierarchyRollupTests.swift)
  covers exact fixtures, full-cache admission, storage failure, queued intent,
  sticky privacy, execution-receipt catch-up, retained trash, and deep caches.
- Android's full unit/lint/debug-APK/test-APK and instrumentation compilation
  gates: 1,513 tests in 118 suites, no failures/errors/skips, no lint errors,
  and 29 existing lint warnings. Both debug JNI architecture libraries were
  rebuilt and verified against the unchanged core during the gate sequence.
- Ten isolated Android UI checks: three new [summary interactions](../apps/android/app/src/androidTest/java/com/greengolddog/dayweave/CanonicalHierarchyRollupUiTest.kt)
  and seven existing browser interactions. Tests cover exact offline details,
  private/queued summary withholding, hydration invalidation, and existing
  search/disclosure/lock/editor behavior. These use the synthetic test host,
  not the production activity; the disposable emulator was stopped afterward.
- Inspected native macOS browser/detail rendering through a non-presented
  synthetic `NSHostingView` window, plus the Android detail screenshot. Both
  show recorded lifecycle separately from aggregates, explain excluded parent
  estimates and unknown work, and conceal protected aggregate numbers.

The normalized reducers and native wrappers exercise at least 5,000 levels.
Recorded stress-test timings are not proof of every release performance budget.
No owner data, physical devices, provider calls, or paid infrastructure were
used. Existing canonical status, execution credit, and encrypted journal/schema
semantics are unchanged. Controlled client/service convergence and owner-device
acceptance remain open; the general progress and authoritative completion
features above are unfinished.
