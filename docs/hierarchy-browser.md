# Canonical goals and projects browser

This client slice implements navigation for `GOAL-001`, `HIE-001`, and
`UX-002`–`UX-004`. It does not claim to implement parent auto-completion,
independent progress measures, project authoring, weekly goal allocation, or
new execution authority. Those remain in the feature ledger.

## Source and identity

- Browse the complete delta-hydrated, nondeleted canonical cache and the
  encrypted authoring journal. Never derive the tree from schedule blocks,
  lifecycle-filtered Inbox buckets, or one limited `/v1/items` response.
- Build the effective topology before filtering by kind, status, or query.
  Preserve every canonical status, including completed, skipped, cancelled,
  and unsupported future statuses. Exclude pending trash from the active tree;
  keep its established recovery path in Trash.
- Apply reviewed pending create/replace drafts without confusing an explicit
  `parent_id: null` with an absent draft. Preserve submitted/conflicted state
  and existing replacement restrictions. Do not treat a pending restore with
  no retained body as an invented complete item.
- Keep item identity, canonical revision, and mutation identity unchanged.
  Selecting a row must use the existing canonical inspector/review route,
  never a synthetic schedule block or a newly inferred write permission.

## Shared projection rules

The two scopes are **Goals** and **Projects**. A scope includes each item of
that kind and all its descendants, regardless of descendant kind, status,
duration, or schedule placement. Include existing ancestors outside that set
as parent context so a nested goal or project retains its location. Nested
matching containers appear once, not as duplicated independent trees.

Sibling and root order is `(sibling_order, lowercase UUID)`. Titles do not
change manual sibling ordering or introduce locale-dependent differences.
Hierarchy traversal, ancestor closure, filtering, and collapse are iterative.
There is no added logical depth limit. Display indentation and breadcrumbs are
bounded independently of exact depth; lists render lazily.

An empty query shows the scoped tree with descendants of collapsed nodes
hidden. A nonempty trimmed query matches titles case-insensitively **within the
scope and its descendant set**, then shows only the matches and their existing
ancestor paths. An outside-scope ancestor is context, not a separate search
match. Search temporarily ignores collapse without changing stored disclosure
state; clearing it restores the user's collapsed branches. A matching parent
does not automatically make all its nonmatching descendants search results.

Missing ancestors and malformed cycles must terminate deterministically and
remain discoverable when a matching scoped item or descendant exists. Mark
self-parent cycles as well as multi-node cycles. Do not silently drop cycle
members, imply a complete ancestor path, or enable edits on malformed rows.

`fixtures/hierarchy-browser/projection-v1.json` contains synthetic effective
rows and shared expected results for ordinary trees. Both native model suites
consume it. Native tests additionally exercise the real canonical/journal
overlay, malformed graphs, deep chains, and authoritative refreshes.
Fixture case IDs abbreviate the decimal-padded UUID suffix: `10` denotes the
literal `00000000-0000-0000-0000-000000000010`, not a UUID with numeric value ten.

## Navigation and interaction

macOS uses the existing Projects and Goals sidebar destinations and canonical
right inspector. Android reaches Projects and Goals from More and remembers
the selected static destination using the existing preference persistence.
The Android bottom bar still has exactly Today, Calendar, Inbox, Assistant,
and More; nested destinations highlight More and provide Back to More.

Rows show kind, lifecycle/sync state, useful timing metadata, and hierarchy
diagnostics without inventing progress percentages from today's block count.
Disclosure controls, search, and selection have semantic labels and stable
test identifiers. Existing reviewed editing remains available only where the
canonical authoring contract already supports it. Read-only project, terminal,
unsupported, submitted, or conflicted rows remain inspectable.

## Hydration, privacy, and verification

A missing cursor, unbound cache, failed restore, or incomplete initial sync is
not evidence of an empty workspace. Explain that distinction. Previously
admitted offline/stale cache and local queued drafts may remain browsable with
their actual state; refresh failures must not erase them or relabel them synced.
Before an account binding exists, suppress legacy canonical rows, retained
trash, and bound/submitted journals. Only truly local, unbound, never-submitted
create drafts enter the browser. A bound cache with no completed delta cursor
can show available rows but must still be described as incomplete. Inspector
selection uses the same admitted source and scope as the browser.

Use existing sensitivity resolution across both confirmed and proposed
ancestry. A pending move out of a sensitive parent cannot declassify a row;
missing or cyclic ancestry remains protected. Protect titles, breadcrumbs,
search text, diagnostics, and accessibility descriptions inside the existing
app-lock/private-presentation boundary. New query, collapsed IDs, and local
detail state are transient and clear with lock/account binding changes; they
are not logged, sent to AI/network search, or stored in preferences or
saved-instance state. The existing macOS canonical inspector selection retains
its encrypted-snapshot semantics and existing account/lock fences; this browser
adds no new selection persistence channel.

The full-source privacy adapter builds a batch ancestry index. It retains both
old and proposed parent paths, resolves acyclic ancestry once, and treats
unresolved missing/cyclic paths as sensitive. Regression tests compare the
batch result against the existing independent per-item resolver. Graph-input
memoization prevents timer, selection, search, and disclosure changes from
rebuilding the source. Android also prepares the source off the UI thread and
displays only results matching the current source inputs, never stale privacy
or topology while a replacement result is being prepared.

Acceptance covers both shared fixture projections and native overlay/routing
tests, 5,000-level trees, all statuses, malformed graphs, search/collapse,
privacy inheritance, and static-destination persistence. Visual/UI checks use
synthetic data with inert callbacks and no production application startup or
owner accounts. Full native tests/builds remain required; these checks do not
replace controlled service or owner-device acceptance.

## Verification checkpoint

The implementation checkpoint on 2026-09-06 passed:

- The full macOS wrapper, `scripts/test-macos.sh -Xswiftc -warnings-as-errors`:
  920 tests in 55 suites, including 12 browser and seven batch-privacy tests.
- Android's full unit/lint/debug-APK/test-APK and instrumentation-compilation
  gates: 1,475 unit tests in 113 suites, with no failures or skipped tests and
  no lint errors. Existing lint warnings remain outside this slice.
- Seven `CanonicalHierarchyBrowserUiTest` interactions on a fresh isolated
  API 35 emulator, using synthetic data and the test-host activity only.
- A full native macOS browser snapshot through a non-presented synthetic
  `NSHostingView` window. It exercises the real search field and lazy row
  surface without starting the production app or provider services.

The native tests consume the same ten ordinary-tree fixture cases and also
exercise 5,000-level full source projections and 10,000-level privacy indexes.
These are stress-case regression checks, not proof of every release performance
budget. No owner accounts, real calendar data, physical devices, or paid cloud
resources were used. Controlled service convergence, owner-device acceptance,
and the full-product release gates remain open.
