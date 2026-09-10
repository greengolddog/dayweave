# Controlled native completion convergence

Execution status: **VERIFIED on 2026-09-10**. One complete clean run passed all
nine native phases, the final independent SQL assertions and owned cleanup:
API, PostgreSQL and idle-sleep guard stopped, retained macOS runtime removed.
Compilation, skipped opt-in tests and helper-test success alone do not establish
this gate. The result covers the bounded synthetic scenario below, not full
device or product acceptance.

Full native regression evidence is: macOS 1,059 executed tests
plus three opt-in skips (1,062 total, 71 suites), with compiler warnings denied;
Android 1,654 passing JVM tests plus two opt-in skips (1,656 total, 136 suites),
lint zero errors/29 warnings, and both debug APK builds. The live completion
phases above were executed separately, not inferred from those skipped tests.
The Android full gate was refreshed after the precision fix; macOS sources are
unchanged from its full regression checkpoint. Android instrumentation
sources/APK were checked, not run on a device or emulator.

This gate connects the production macOS and Android completion stores and
transports to one real HTTP service and disposable PostgreSQL database. It tests
the [completion policy contract](item-completion-api.md) and
[hierarchy completion rules](hierarchy-completion.md), including immutable replay,
stale review, requiredness, automatic completion and exact reopening provenance.

## Run safely

On a development Mac with the repository's Rust/Swift/Android prerequisites and
`initdb`, `pg_ctl` and `psql` available, set `JAVA_HOME` to the supported JDK 17
installation and `ANDROID_HOME` or `ANDROID_SDK_ROOT` to its installed SDK. Run
the complete scenario only through:

```sh
python3 -B scripts/test-native-completion-convergence.py
```

The driver accepts no target, database or credential arguments. It generates a
new mode-0700 directory under `/tmp`, a new PostgreSQL cluster, random synthetic
workspace/item identities and temporary credentials. A synthetic legacy credential
bootstraps a real device enrollment in hybrid-auth mode; the resulting device
session supplies the exact item and schedule scopes for the run. The two test
stores share that session, so this is not a per-device authentication lifecycle
test. No auth rows or principal scope are fabricated. Config,
encryption keys, native journals, markers and logs remain private outside Git.
Both service listeners bind to literal `127.0.0.1` on temporary ports. The explicit
service environment disables Google integrations, MCP OAuth and the assistant;
it does not load the owner's integration environment. Proxies and redirects are
disabled. Existing production HTTPS requirements are unchanged.

Do not run concurrent native builds or change sources during the gate. The
driver prebuilds both native test targets before enrolling the short-lived
synthetic session. The macOS prebuild uses `scripts/test-macos.sh` with warnings
treated as errors; only a successful prebuild retains its generated private
Testing.framework copy and hands that exact path to the driver for cleanup.
Failed prebuilds retain no runtime copy, and the installed toolchain is unchanged.
Subsequent macOS phases use `--skip-build` while that owned copy remains available
at the binary's linked runtime path. Android phases use task-local `--rerun` for
the selected JVM test task, forcing its execution without rebuilding every
dependency through global `--rerun-tasks`. The driver removes the retained copy
during owned cleanup. Neither production app is launched; no emulator, owner
Keychain/Keystore record, provider connection or cloud deployment is used.
An owned `caffeinate -i` process prevents idle system sleep during the run, then
stops during cleanup (or when the driver exits). It does not prevent display
sleep/locking or change power settings. Explicit host suspension can still
expire the real 15-minute access credential and fail the gate.

## Nine ordered native phases

The seed is a blocked root goal with the manual reason `Synthetic waiting for
input`, a planned branch project, a planned required leaf under that branch and
a second planned leaf directly under the root. All initially use default
requiredness. The latter leaf becomes optional only through reviewed operation E.

| Native phase | Required evidence |
| --- | --- |
| macOS `prepare` | Read the real canonical tree and fresh completion GET; durably queue manual Complete operation A without a PUT. |
| Android `prepare_offline` | Independently review the original root and queue KeepOpen operation B. Persist its submitted marker, then simulate loss before the real PUT; the server is unchanged. |
| macOS `submit_lost` | Real PUT A commits root policy revision 1. A test decorator drops the successful reply, leaving the exact submitted journal and original cached observation intact. |
| Android `conflict_keep_open` | Replay B to the real stale-item 409 and retain it for review. Catch up, explicitly replace B with fresh operation C, and commit KeepOpen at policy revision 2. Persist the post-receipt catch-up latch while the canonical cache still shows A's completed root. |
| Android `catchup_automatic_optional` | Restart with that latch; reject a new GET before canonical catch-up. Restore the blocked root, commit Automatic operation D at root policy revision 3, then optional requiredness operation E at the second leaf's policy revision 1. |
| macOS `replay` | After the driver restarts the API against the same database, restore A, observe the newer policy, and replay its exact bytes. Settle the historical Complete receipt without replacing newer evidence or changing the current canonical tree/cursor. |
| macOS `verify_cascade` | After the driver's real canonical PUT completes the required leaf, observe automatic completion of branch and root, with the optional leaf still planned and the root's exact blocked reopening tuple retained. |
| Android `verify_cascade_and_child` | Observe that cascade, then create a new planned child under the completed branch through the normal native authoring pipeline. Verify its fresh scoped parent GET and submitted journal, successful real preview/publication with an injected sub-microsecond clock, and branch/root reopening. |
| macOS `verify_reopen` | Catch up to the same five-item tree: branch planned, root blocked with its original manual reason, required leaf completed and optional/new leaves planned. |

Every native process writes a private, typed checkpoint marker only after its
assertions pass. The driver compares canonical identity/revision/status/parent
and blocker tuples plus the complete root completion snapshot against the
expected service checkpoint. Historical receipts are deliberately not treated as
fresh GET permission. Restart checks retain encrypted journals/catch-up state
but do not restore ephemeral review authority.

## Independent service and storage evidence

The driver folds ordinary ordered delta pages to a terminal cursor, preserving
multiple increasing revisions of one item in primary/derived mutation groups.
It requests 200 changes per ordinary page; the helper allows the service's
expanded atomic groups up to 300 records. Bounded page/byte counts, revision
ordering and complete final hierarchy checks reject partial or contradictory
evidence.

Final read-only SQL is scoped to the fresh workspace and must establish:

- exactly four successful immutable policy operations: A, C, D and E; no receipt
  for rejected B, and typed request equality with the native captured commands;
- A's original Complete result remains identical after later policy changes,
  service restart, replay, automatic completion and reopening;
- eight completion effects across six evaluations: four one-effect reviewed
  policy commands and two two-effect canonical cascades, with exact before/after
  policy and canonical revisions;
- final root canonical/policy revisions 8/5, branch 5/2 and optional leaf 2/1;
  required leaf canonical revision 2 and new child revision 1; no current
  completion provenance after reopening, while immutable effects retain the
  original blocked/planned reopening states;
- no execution, independent-progress or habit writes, and execution revision
  remains zero. Schedule preview/publication records are permitted because the
  normal Android child-authoring pipeline uses them.

Native exact-byte journal assertions are separate from SQL's typed UUID
comparison: server UUID normalization does not excuse any other request change.

The pure scenario and harness checks can be run independently:

```sh
python3 -B scripts/test-native-completion-scenario.py
python3 -B scripts/test-native-completion-harness.py
python3 -B scripts/test-native-progress-harness.py
scripts/tests/test-macos-runtime-retention.sh
```

The first suite has 12 synthetic constructor/fold/SQL-evidence tests, including a
5,000-node iterative fold, duplicate JSON keys and invalid integer revisions.
The second has 19 completion-specific marker, private-artifact, command,
scoped-enrollment, bounded-read and retained-runtime handoff/cleanup tests. The
third has eight reused process/environment/HTTP safety
tests, including timeout, interruption and surviving descendant cleanup. None
of these suites constitutes a live native convergence result. The shell regression
adds 17 mocked wrapper invocations, including catchable signals during the final
EXIT handler. All four run in the existing macOS CI job; the full cross-client
service run remains opt-in.

## Cleanup and coverage boundary

Failure or interruption triggers bounded cleanup of only the owned API process
group, PostgreSQL cluster, idle-sleep guard and admitted retained runtime copy.
Runtime cleanup checks the private generated path's owner, mode and original
device/inode identity, without following framework symlinks. An interrupted
handoff is recovered only from its exact private receipt; an unresolved handoff
cannot report successful cleanup. The driver retains private artifacts for diagnosis,
records cleanup in `report.json`, and prints PASS only after successful cleanup;
partial phases or cleanup failure cannot become a passing report. Forced process
termination that prevents Python cleanup is not covered by that guarantee.
Never commit the run directory, credentials, journals or logs.

macOS uses `DayWeaveAPIClient`, `ItemCompletionStore`, `CanonicalSyncStore`,
`PlannerStore` and `EncryptedPlannerPersistence`. Android uses the production
OkHttp transports, `ItemCompletionSyncManager`, `CanonicalSyncManager`,
`PlannerStore` and `RoomPlannerStateRepository` snapshot codec through a test-only
AES-GCM file-backed DAO. This is not a physical Room/SQLCipher or Android Keystore
test. Test decorators inject the two delivery losses; successful writes and
conflicts still use the real service.

The live scenario grows from four to five nodes, not a deep native cascade. The
helper's 5,000-node fold must not be represented as such a cascade. Device UI,
foreground/reconnect timing, privacy-lock interaction, TLS, durable device
authentication, qualified recurring occurrences, active-execution interactions,
provider synchronization and owner acceptance remain separate gates. The shared
synthetic session and these selected production-store calls do not verify
those boundaries or complete the full app.

The 2026-09-10 repeat also passed with Android's canonical manager clock forced
to sub-microsecond precision on every sample. Real child-authoring preview and
publication succeeded, and the installed generation timestamp was microsecond
aligned. New composition input and provenance are normalized together; already
saved requests are not rewritten. This strengthens the clock boundary without
changing any of the scenario's completion or cleanup assertions.

All 150 Android sync-manager tests passed, including five focused regressions
for remote/local precision, restart under a different nanosecond clock, exact
legacy nanosecond-request custody after a simulated rejection, and one-nanosecond
backward clock changes at both local commit fences. Four normalization-dependent
regressions were first run with the old clock behavior and failed as expected.
The local fences retain the original full-resolution capture, independently of
the normalized scheduler timestamp. No persistence schema or request serializer
was changed.
