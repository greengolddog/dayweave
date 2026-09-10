# Private fixed-input routine preview

This checkpoint extends [saved routine inputs](routine-planning-input.md) with
an explicit local recomputation and a protected, read-only preview on both
native clients. It does **not** complete the broader offline planning,
editing or execution requirements in [the product specification](product-requirements.md).
The root-run verification below covers this bounded checkpoint; broader live
service, provider and physical-device acceptance remain open.

## What the action means

After connected preparation, the user can recompute the **original** saved
request without another server request. The helper receives exactly the retained
canonical sources and qualified v2 witness. There is no v1 fallback, new witness
POST, request rebuilding, advance of `as_of`, or expansion of the fixed horizon.
The separately recorded computation time describes when the preview was made,
not a new scheduling input clock.

The result shows scheduled blocks, unscheduled work and planning diagnostics.
It has no Start, Done, Skipped, Move, Defer, publication or provider-write
callbacks. Closing the preview changes no task. It is not a source for widgets,
notifications, assistant context or execution identity. The original canonical
schedule, publication proof, occurrence checkpoint and every recovery journal
remain unchanged.

Separate display custody is intentional: the ordinary local-plan installers
replace schedule/publication fields that are themselves part of the saved
request's input comparison. Reusing those installers would stale the capsule
and could imply action authority. This preview is a step toward full offline
planning, not a replacement for it.

## Admission, expiry and private presentation

Every recomputation requires healthy encrypted storage, the same local
credential binding and full API base path, unchanged complete input state,
settled recovery work and a current private foreground operation. Saved
credentials identify the local binding; offline code does not assert that the
server has freshly authenticated them or that external services remain current.

The operation checks source/read generations, scope, profile, occurrence and
Habit checkpoints, pending intents, durable state and clock continuity before
and after helper work and around serialization/storage. The original planning
day, timezone and horizon must still be eligible. Clock rollback, background,
lock, changed input or unresolved recovery withdraws presentation permission.

Encrypted restart restores data only. A new explicit private recomputation is
required before displaying it; no presentation admission is serialized. macOS
grants local foreground ownership after the existing unlock/onboarding gate,
independently of whether execution refresh succeeds. That local ownership is
not a fresh execution or publication proof. Stop/lock revokes it.

Normal startup also withdraws execution-history freshness before attempting a
remote read. Display-only input comparison may tolerate the loss of those two
freshness flags, while retaining exact execution binding, revision, history,
outcomes and pending-command comparisons. It never restores the former flags
or uses them to enable execution. The live operation still captures and checks
the complete current state, including its now-withdrawn flags.

For an already terminal Habit cache with no pending work, automatic missed-habit
reconciliation now starts with the ordinary read-only delta path. A failed cold
offline read keeps the terminal encrypted cache and does not manufacture a new
automatic write journal. Existing pending requests are still replayed first
with unchanged recovery rules. After a successful read, new automatic writes
are journaled normally and followed by terminal catch-up; response loss retains
that journal and blocks reuse as before.

A malformed or contradictory response is not treated as an offline read. It
withdraws the old terminal verdict in memory and attempts to persist that
withdrawal against the original state/revision, without advancing the cursor or
discarding history or pending work. A competing writer is never reloaded and
overwritten merely to record withdrawal. Failed storage keeps the current
process fenced; a successful withdrawal also survives restart.

The screens are actionless and protected even when every individual source is
marked nonsensitive. A retained but stale artifact may remain encrypted while
its rows are hidden. A result saved just before cancellation may likewise
remain inert; post-save cancellation never creates presentation permission.

## Exact result custody and capacity

Both adapters retain the exact validated helper-v2 output, including original
timestamp strings. Display formatting may use native date types, but their
potentially lossy values are never re-encoded as the retained helper result.
The result is revalidated against the capsule's helper fingerprint, complete
source accounting and positive-or-empty occurrence head on restore/use.

- macOS schema **30** adds a separate `RoutinePlanningDisplayPlan` and preserves
  the schema-29 capsule and all prior journals. Its binding currently retains
  the complete value-semantic capsule again. This costs extra encrypted space;
  the existing ordinary **16 MiB whole-snapshot limit** is not raised. The
  display is also bounded intrinsically. A valid input may therefore fit alone
  but require more space than preview admission permits. Admission reports the
  failure rather than trimming sources or recovery work. A future compact
  binding must preserve exact semantics across unordered native containers.
- Android Room/JSON **26** adds `RoutinePlanningDisplaySnapshot`, holding exact
  helper-response text and explicit original-request, input, witness, helper
  request, source/head and capture/computation bindings. New/replacement
  admission checks the **16 MiB prospective snapshot budget**. Ordinary later
  saves with unchanged artifacts keep their pre-existing behavior; the preview
  does not add a global ceiling that could prevent outbox custody.
- Predecessor formats cannot acquire the new field through relabeling. Process
  admissions cannot be restored from payloads. Exact-state and encrypted-write
  failures preserve the previous durable artifact and existing recovery bytes;
  no failure discards a pending operation to make room.

## Verification boundary

New native tests exercise model/codec fidelity, migration, exact saved-input
recomputation, restart/foreground ownership, private actionless presentation,
stale-generation/cancellation rejection and durable/capacity failures.

Root-run verification passes:

- macOS warnings-denied application/test compilation, **200 focused executed
  tests** (one opt-in skip), and the full suite with **1,195 executed tests**,
  five opt-in skips and no failures.
- The full Android JVM suite: **1,780 executed tests**, four opt-in skips and
  no failures or errors, across 153 suites (1,784 total tests).
- Actual Rust helper/codec/display integration: **one executed macOS process
  test and one executed Android host-JNI test**, each consuming every qualified
  shared producer case, with neither skipped. These run separately from the
  ordinary suites above, where their explicit opt-ins remain disabled.
- **Nine pure harness tests**, including environment isolation, exact fresh
  test-report admission and owned process-group cleanup on interruption.
- Android lint with **zero errors and 34 existing warnings**, plus debug app
  and test APK builds and SQLCipher migration/UI instrumentation source
  compilation. The instrumentation has **not** run on a device.

The native regression runs include real macOS execution/Habit/service startup
against failing synthetic transports, encrypted restart after rejected Habit
responses, original-state/CAS rejection and failed withdrawal storage. They do
not contact the owner's accounts. The retained macOS test framework was moved
to Trash after the full run, and its original runtime path was verified absent.

The opt-in [host helper gate](../scripts/test-native-routine-planning-helper.py)
builds the actual Rust helper and JNI library and consumes every qualified case
in the [shared producer corpus](../fixtures/routine-planning-witness/README.md).
macOS uses the real POSIX runner and v2 codec with a test locator/signature
admission; Android uses the actual JNI entry point in a host JVM. Both retain,
serialize and revalidate the real result in the display artifact. This does
not establish packaged-host signing, Android-device execution, service/TLS
preparation, provider behavior or visual/device acceptance.

Run explicitly on macOS with already available dependencies:

```sh
python3 scripts/test-native-routine-planning-helper.py --run \
  --java-home /path/to/jdk --android-home /path/to/android-sdk
```

The harness scrubs inherited owner integration/signing/runtime switches,
builds with offline dependency resolution, serializes native builds and writes
private logs. Its report checks reject skipped, failed or foreign test results.
The exact previous generated Android report is moved into the private gate log
directory before execution; only a new report containing the expected executed
testcase can satisfy the gate.

## Remaining full-product work

Arbitrary offline clocks/horizons, source edits, qualified execution credit and
manual-placement policy remain required and unfinished. A preview never
converts the witness into a reusable remote authority lease. Completion-relative
cadence, unfinished optional-instance retention, semantic template rebase,
nested recurrence, step-specific defer, controlled real-service preparation and
physical-device interaction acceptance retain their independent open scope.
