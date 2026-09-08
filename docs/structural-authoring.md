# Structural item authoring and hierarchy capture

This work extends native authoring for projects and the common deadline and
independent-effort fields required by `DOM-001`, `DOM-006`, `HIE-001`,
`HIE-002`, and `GOAL-001`. It uses the existing canonical item API and durable
authoring journals. It does not provide parent auto-completion, independent
progress measures, weekly allocations, or a new execution authority.

## Modern request fields

A modern authoring request explicitly carries:

- `deadline_kind`: `none`, `date`, or `date_time`;
- `deadline_date`: a civil date for `date`, otherwise null;
- `deadline_strength`: `hard` or `soft` when a deadline exists, otherwise null;
- `deadline_soft_weight`: an integer from 0 through 1,000,000 for a soft
  deadline, otherwise null;
- `has_own_effort`: an explicit boolean, independent of whether children exist.

`deadline_at` remains the existing canonical timestamp field. Date-only
requests leave it null; they do not convert a civil date into a UTC timestamp
in the stored request. The scheduling boundary is the next civil date's local
midnight in the item's timezone. A nonexistent midnight is rejected, and an
ambiguous midnight uses the earlier instant. This is not the same as adding
24 hours or silently advancing through a timezone gap.

Fixed events are the exception: their `deadline_kind` remains `none`, while
`deadline_at` can retain the event end matching its timing metadata. Editing a
different field must not turn that end into a task deadline or erase it.

The typed own-effort value must agree with a legacy
`flexible_constraints.has_own_effort` member when one exists. The server
normalizes a true top-level value with an absent legacy member by inserting a
true member. Exact outcome matching must account for this documented
normalization, without accepting unrelated field changes.

Projects may have unknown, exact, or ranged duration and optional deadlines.
Duration alone does not give a project flexible schedule demand. Only a leaf
with explicit own effort is eligible; adding children never creates duplicate
parent demand. Projects and goals cannot have task recurrence; use a routine,
habit, or recurring task instead. Retained unsupported structures remain
inspectable without becoming lossy full-item replacements.

## Durable request versions

Structural request versioning is independent of duration request versioning.
Do not reinterpret the existing legacy/rich duration marker as permission to
add structural fields.

| Structural request version | Wire behavior |
| --- | --- |
| `1` — legacy | Omit all five new fields and retain the original legacy-inferred deadline/effort representation. |
| `2` — modern | Emit all five fields explicitly, including null deadline optionals and the boolean effort flag. |

Both clients store `structuralRequestShapeVersion` in each authoring journal.
Pre-upgrade journals receive legacy version 1 during validated migration;
missing version markers are not an invitation to upgrade an existing request.
New snapshot formats require the marker and a complete typed draft shape.
Old formats reject injected modern journal markers or typed authoring-draft
fields instead of changing the meaning of retained requests. This restriction
does not remove typed fields already supported by canonical cached items.

A submitted, configuration-bound, or conflicted request keeps its exact
request identity, revision, body, and duration/structural versions through
restart, migration, retry, and ambiguous-outcome recovery. Only a fresh review
of an eligible unbound, unsubmitted pending draft can replace it with a modern
request. Valid historical macOS Project-create journals remain replayable even
though the old UI did not expose project creation. The stricter background
status/privacy replacement rules are not relaxed by this authoring upgrade.

## Creating from the hierarchy browser

New Goal and New Project open an ordinary reviewed create form. Add Subtask
opens the same flow with Task as the initial kind and the selected parent ID.
These are presets, not immediate writes. They start with a blank title, Inbox
placement, the profile timezone, and no copied parent duration, constraints,
recurrence, or progress. The user can review the kind, parent, and other fields
before saving. They never reuse the onboarding-first-item designation.

Saving creates one ordinary encrypted CREATE journal. A newly queued local
parent and its child retain their distinct stable IDs; synchronization waits
for parent authority before sending the child. No client invents a canonical
revision or bypasses a pending recovery record.

Parent attachment is distinct from replacing the parent's body. The server
permits Inbox, Planned, and Blocked parents. A blocked parent can therefore
receive a child while its own unsupported lifecycle/body remains read-only.
Missing, trashed, unadmitted, cyclic, terminal, or active parent paths cannot
become new attachment targets. Conflicted or otherwise unsafe pending parent
operations keep their recovery restrictions. Eligibility is rechecked against
current state at save and synchronization, not just when the form opens.

Creating, moving, removing, or reordering a child can increment direct parent
revisions in the same atomic delta group. Clients must reconcile those implicit
updates while preserving exact submitted journals. A stale replay can return
its original response rather than the current topology; it must not roll the
cache backward. Dependency and recurring-subtree validation still apply to
otherwise valid hierarchy edits.

## Privacy and verification

Inherited sensitivity does not silently turn into an own mark on a new child.
The review surface nevertheless retains its original sensitive context while
the user edits or reparents the unsaved draft. Parent-picker labels, dependency
labels, hierarchy details, and accessibility content use the same conservative
admitted-source privacy rules. Lock or account-binding changes clear transient
review content through the existing private UI boundary.

The shared fixture is
[`requests-v1.json`](../fixtures/structural-authoring/requests-v1.json), schema
`dayweave.structural-authoring-fixtures/1`. Each valid case contains a raw
canonical CREATE body, with optional expected normalized constraints. Remove
only `id` to construct the corresponding replacement item. Invalid cases must
be rejected without changing canonical state. The fixture includes all three
duration shapes, date-only and timestamp deadlines, both strengths and weight
boundaries, modern own-effort normalization, and fixed-event end preservation.
“Valid” denotes the HTTP contract, not permission for a native editor to
replace an imported event. Native tests keep imported `calendar_event` bodies
read-only and separately exercise editable owned event timing without changing
its source-ownership rules.

Native tests additionally cover legacy request omissions, version migration,
tampering rejection, restart/exact replay, editor round-trips, queued parent-child
ordering, stale parent changes, sensitive reparenting, and leaf-only demand.
HTTP tests exercise the same structural cases against memory and PostgreSQL,
including parent revision groups, blocked-parent attachment, root detach,
cycles, and replay behavior. These checks do not replace controlled native
client/service convergence or owner-device acceptance.

## Verification checkpoint

The 2026-09-08 native checkpoint passed:

- `scripts/test-macos.sh -Xswiftc -warnings-as-errors`: 933 tests in 56
  suites, including 13 structural authoring tests and the existing migration,
  synchronization, editor, and hierarchy regressions.
- Android `testDebugUnitTest`, `lint`, `assembleDebug`,
  `assembleDebugAndroidTest`, and `compileDebugAndroidTestKotlin`: 1,496 unit
  tests in 116 suites, no failures or skipped tests, and no lint errors.
  The 29 existing lint warnings remain outside this slice. The new unit
  coverage includes six structural, seven hierarchy-authoring, and eight
  persistence tests.
- Thirteen combined Android instrumentation tests on a fresh isolated API 35
  emulator: seven hierarchy-browser checks, five structural editor/browser
  checks, and the SQLCipher Room 20-to-21 migration. They exercise real typed
  Project form-to-journal saving, both root presets, blocked-parent child
  capture, current-source action revocation, and the actual secure-window flag
  after sensitive detachment without changing the draft's own privacy mark.
  Native screenshots show the deadline and leaf-effort controls. Only inert
  component hosts ran, and the emulator was stopped afterward.
- Native macOS Project editor and hierarchy snapshots through a non-presented
  synthetic `NSHostingView` window. The editor shows date-only/soft deadline,
  own-effort, and ordinary Inbox controls; the browser exposes creation and
  eligible Add Subtask controls while terminal rows stay read-only.

The unchanged shared request fixture and HTTP tests also passed the full Rust
workspace gate on 2026-09-06: 895 tests, no failures or ignored tests, using
`cargo test --workspace --all-features -- --include-ignored --test-threads=1`
with disposable local PostgreSQL schemas. Workspace/all-targets/all-features
Clippy passed with warnings denied. Both memory and PostgreSQL implementations
exercise the 11 valid and 11 invalid shared request cases through CREATE and
REPLACE, plus nested authority, parent revision groups, and replay checks.

These are implementation and synthetic acceptance checkpoints, not a signed
release, controlled two-client/service convergence proof, or the required
owner-device/seven-day trial. No owner accounts, real calendar data, production
application startup, physical devices, or paid cloud resources were used.
