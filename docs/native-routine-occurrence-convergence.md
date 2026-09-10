# Controlled native recurring-instance convergence

Execution status: **verified on 2026-09-10**. All nine native phases, independent
SQL assertions, the final current-publication read and owned cleanup passed in
one complete fresh-service run. A skipped opt-in test, a successful build or pure
helper tests alone are not a live convergence result.

This gate exercises the production macOS and Android recurring-instance stores,
encrypted snapshot codecs and HTTP transports against a new local service and
PostgreSQL cluster. It extends the [occurrence contract](routine-occurrence-completion.md)
without changing the [full product requirements](product-requirements.md).

## Safe invocation

On a development Mac with the repository's Rust, Swift and Android prerequisites,
`initdb`, `pg_ctl` and `psql` on PATH, and `JAVA_HOME` / `ANDROID_HOME` pointing to
the supported installed tools:

```sh
python3 -B scripts/test-native-routine-occurrence-convergence.py
```

The driver accepts no target arguments. It creates a private mode-0700 generated
directory under `/tmp`, new synthetic identities and a fresh loopback database.
A temporary bootstrap credential creates a real scoped Device enrollment;
both independent native test stores share its short-lived session. This verifies
store convergence, not independent device authentication lifecycles. Configuration,
tokens, encryption keys, exact request bodies, markers and logs remain mode-0600
outside Git. Never commit those artifacts.

The service uses an explicit environment with Google, MCP OAuth and assistant
integrations disabled. Neither the owner's application profile, Keychain/Keystore,
calendar, Nebius deployment nor external service is used. HTTP proxies and
redirects are forbidden. The synthetic HTTP exception does not change production
HTTPS requirements.

Run with no concurrent builds or source edits. The driver builds both native
targets before enrolling the fifteen-minute access credential and capturing a
whole-second scenario clock. Publication timestamps must remain within the
production five-minute clock-skew allowance; suspension or an unusually slow
run fails rather than weakening that check. Native phases reuse their build,
with task-local Android `--rerun` and macOS `--skip-build`. Only the exact private
retained Testing.framework copy is removed during cleanup; the installed
toolchain remains unchanged. A run-scoped idle-sleep assertion does not prevent
display sleep/locking or change power settings.

## Nine native phases

The six-item seed contains a daily Routine, an intermediate Task, its required
leaf, and three optional leaves directly under the root: planned, Inbox, and
manually Blocked with `Synthetic waiting for input`. Optional-edge policies are
set through the real canonical completion API before initial publication. A real
preview/publication admits complete immutable manifests, including the Inbox
member without a scheduled block. The first two producer-issued planner
identities resolve to separate private ledger IDs; the second instance is an
unchanged sentinel throughout the run.

| Phase | Required result |
| --- | --- |
| macOS `prepare` | Bootstrap canonical state, terminal occurrence state and fresh remote scheduling; review and durably queue A, Done on the required leaf. |
| Android `prepare_offline` | Independently review revision 1 and queue B, Skipped on that same leaf. Persist the submitted marker, then fail before any real PUT. |
| macOS `submit_lost` | Real A commits aggregate revision 2 and automatic branch/root completion; drop its successful reply and retain the submitted exact request. |
| Android `conflict_keep_open` | Submitted B reaches the real stale-instance rejection; explicitly discard it after that definitive result. Freshly review and submit C, Keep open on the root, producing revision 3. Complete terminal and fresh schedule catch-up. |
| macOS `replay_lost_publication` | After an API restart, observe revision 3, replay A's exact bytes without a preliminary PUT review, and settle its immutable revision-2 receipt without rollback. Terminal catch-up succeeds, but lose a real successful publication reply, retaining publication custody and the schedule latch. |
| macOS `recover` | Restart with exact publication custody. Recover the old operation, then require a new remote composition/publication operation before clearing the latch. |
| Android `finish` | Freshly review D, root Automatic; E, Skipped on the blocked leaf; and F, exact reopening to its original manual blocker. Each operation gets terminal and fresh remote schedule catch-up; aggregate revision reaches 6. |
| macOS `verify` | Restart and converge to the complete final occurrence, unchanged sentinel and unchanged canonical templates. |
| Android `verify` | Independently restart and converge to the same evidence with no unresolved intent, receipt target or schedule latch. |

Native wrappers inject delivery loss only around real production requests. They
verify durable submitted state before sending, byte-for-byte replay, rejected
request custody, full-manifest membership, lost-publication recovery and the
absence of restored runtime review authority. Fresh catch-up must dispatch a new
publication operation; an unchanged publication revision ID may legitimately be
deduplicated and is not alone evidence of stale work. Local helper composition
is forbidden in this gate.

Every phase writes a closed, typed private marker. The driver checks full
occurrence aggregates, the sentinel, canonical revision/status/topology/blocker
projections, pending/submitted/receipt-target counts, terminal cursor, schedule
latch and publication custody against the corresponding authoritative checkpoint.
Settled publication operation IDs must link through immutable SQL receipts to
the exact revision retained by that client; a lost-reply journal may retain an
older real proof until recovery.
UTC timestamp normalization permits equivalent wire representations without
equating booleans, floating-point revisions or different request bodies. SQL
sessions explicitly use UTC and disabled owner password/service files, so the
developer machine's timezone cannot change the evidence representation.

## Independent storage checks

Read-only SQL scoped to the disposable workspace compares seed and final
evidence: immutable manifests and first-publication witnesses, complete source
membership linked to canonical history, contiguous occurrence changes with exact
before/after state, and five immutable successful operation receipts A/C/D/E/F.
Rejected B must have no operation. A's original result must remain unchanged.
Native raw-byte assertions complement SQL's typed JSON comparison. Additional
instances admitted by the native rolling horizon may exist only at their initial
revision; the selected sentinel is unchanged.

The occurrence workflow must leave canonical templates/history/completion,
execution, independent progress, habit and provider state unchanged from its
post-seed baseline. Schedule preview and publication writes are expected.

The final public current-schedule read must match both the last native durable
proof and the highest scoped SQL publication revision, including its revision
number and input digest. That exact v6 revision must cover the final occurrence
change head. Matching calendar blocks alone cannot pass this check: an older
publication may have similar blocks but stale lifecycle evidence. The remaining
optional leaf must still be scheduled for the selected occurrence, while the
completed required leaf and Inbox/Blocked leaves must not be scheduled.

## Published subtree reader

The live gate exposed a production reader defect: recurrence expansion correctly
assigned descendant work the outer root's occurrence identity, but the public
snapshot validator accepted only references to the root itself. The reader now
derives membership from the immutable retained planning request, validates its
complete input digest and captured source revisions, and verifies exact shared
recurrence expansion and lifecycle joins. It never uses today's canonical tree
to authorize a historical publication.

An indexed iterative forest walk establishes outermost-root membership. Unrelated
roots and omitted Inbox/context members cannot gain scheduled-reference authority.
Root-only legacy v5 snapshots without a retained request remain compatible, while
descendant references and v6 snapshots require valid retained evidence. Private
planning and lifecycle evidence remain absent from the public response.

Eight new reader regressions cover both schemas, every output-reference category,
malformed topology, source/lifecycle drift, nested recurrence identity, legacy
compatibility and a 5,000-level hierarchy with real resolved UTC day boundaries.
The existing strict-shape regression also passes. All production resource budgets
remain unchanged.

## Reusable harness checks

Pure checks can run without native builds or network access:

```sh
python3 -B scripts/test-native-routine-occurrence-scenario.py
python3 -B scripts/test-native-routine-occurrence-harness.py
python3 -B scripts/test-native-completion-scenario.py
python3 -B scripts/test-native-completion-harness.py
python3 -B scripts/test-native-progress-harness.py
scripts/tests/test-macos-runtime-retention.sh
```

CI runs the pure checks; the complete two-client service gate remains opt-in.

## Verification record

The final clean gate passed all nine phases against a newly initialized local
PostgreSQL cluster and API, including an API restart. It independently verified
five immutable successful member operations, the rejected competing request's
absence, complete immutable manifests and source/publication joins, unchanged
canonical templates and sentinel state, exact blocker reopening, historical
receipt recovery without rollback, and a fresh final publication covering the
terminal occurrence head. PostgreSQL, API and idle-sleep guard shutdown and exact
retained macOS runtime removal were all confirmed. Private evidence remains
outside Git.

Supporting regression gates pass:

- 687 API tests against fresh disposable PostgreSQL, with database-only cases
  enabled, zero failures/ignored tests and only the maintenance fixture emitter
  excluded. The nine reader tests are included in this total.
- Workspace all-target/all-feature Clippy with warnings denied and formatting.
- 1,122 executed macOS tests with warnings denied; four opt-in tests skipped
  (1,126 total). Android passes 1,717 JVM tests with three opt-in skips
  (1,720 total). The new native opt-in test on each platform was subsequently
  executed by the complete nine-phase gate, not counted as live coverage from
  these skipped prebuild/full-suite runs.
- 46 new pure scenario/harness checks, 39 reused completion/progress checks,
  and 17 mocked macOS runtime-retention invocations.

An earlier full API attempt exhausted PostgreSQL's default lock table during
parallel migration setup/teardown. The successful fresh-cluster rerun used
`max_locks_per_transaction=512`, also used by the live driver; no production
scheduling limits or assertions were relaxed. An earlier live attempt completed
the nine native phases but failed the final public reader check. That failure
led to the retained-subtree reader fix above; it is not counted as a passing gate.

## Cleanup and coverage limits

Failure or interruption attempts bounded cleanup of only the owned API process
group, PostgreSQL cluster, idle-sleep guard and exact retained framework copy.
Framework ownership, mode and original device/inode are checked without following
symlinks. An unresolved runtime handoff or cleanup failure cannot yield PASS.
Private diagnostic files remain outside the repository. Uncatchable forced
termination cannot guarantee cleanup.

Android uses the real encrypted snapshot codec through a test-only file-backed
DAO, not physical Room/SQLCipher or Android Keystore. Neither native UI is
launched. This six-node scenario is not a deep native cascade, privacy/foreground
timing test, active execution test, provider synchronization test, production
TLS/auth lifecycle trial or owner acceptance. The separate authenticated
[planning-witness endpoint](routine-planning-witness.md) now qualifies exact
current-source inputs. Native witness custody/use and enabled helper-v2 local
composition remain open.
Completion-relative cadence, template rebase, nested recurrence, step-specific
deferral and other full-product work are not completed by this gate.
