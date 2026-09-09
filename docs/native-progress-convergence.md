# Controlled native progress convergence

Status: all six native phases and the service restart passed on 2026-09-09
against a fresh local PostgreSQL-backed service. Final database assertions and
owned API/PostgreSQL cleanup also passed.

This gate connects the production macOS and Android progress transports/stores
to the same actual HTTP service and PostgreSQL database. It complements the
[independent progress contract](item-progress.md), unit fixtures and inert UI
tests. It does not replace physical-device, foreground lifecycle, TLS, durable
device-authentication, or owner acceptance tests.

## Run safely

On a development Mac with the documented Rust/Swift/Android prerequisites and
local `initdb`, `pg_ctl` and `psql` available:

```sh
python3 -B scripts/test-native-progress-harness.py
python3 -B scripts/test-native-progress-convergence.py
```

Set `JAVA_HOME` explicitly to the JDK used by the existing Android build (JDK 17
for this checkpoint), and `ANDROID_HOME` or `ANDROID_SDK_ROOT` to its installed
SDK. The harness rejects missing paths before starting any services; it does not
install or select a different JDK/SDK automatically.

The second command takes no service, database or credential arguments. It:

- creates a new mode-0700 directory under `/tmp`, with private synthetic config,
  random test credentials, independent encrypted native state and private logs;
- creates a new PostgreSQL cluster and binds it and the real API to literal
  `127.0.0.1` on temporary ports, never an existing database or deployment;
- gives the API an explicit test environment with Google, MCP OAuth and the
  assistant disabled; it never reads the owner's integration environment;
- uses the clients' existing loopback test/development allowances, disables
  proxies and rejects redirects; production HTTPS policy is not relaxed;
- runs the macOS suite only through `scripts/test-macos.sh`, and forces the
  Android JVM phase to execute rather than reuse a cached Gradle result;
- stops only its own process groups and PostgreSQL cluster, including on
  failure/interruption, and records cleanup in the final private report.

The script does not launch either production app, use Keychain/Keystore owner
records, connect a provider, start an emulator, deploy, or incur cloud charges.
Runtime files and keys are outside the repository and must not be committed.
They remain in the private run directory for local inspection; the service's
temporary credential is no longer usable after its process is stopped.

## Ordered evidence

The service starts with a synthetic unscheduled goal and child task. Both clients
read the real canonical delta and establish independent revision-zero GET proof.

| Phase | Required result |
| --- | --- |
| macOS `prepare` | Native GET establishes zero; encrypted offline operation A records all three progress modes without a PUT. |
| Android `prepare` | A separate native process establishes zero and durably queues a competing operation B, also without a PUT. |
| macOS `submit_lost` | Real HTTP PUT A commits revision 1; a test-only decorator drops the successful reply before the store receives it. Exact submitted bytes remain in encrypted custody. |
| Android `conflict_update` | B receives the real stale-progress 409 and remains retained. A fresh GET sees A's values. Explicit re-review creates a new operation at revision 1; it commits the replacement values as revision 2. |
| Service restart | The same owned API is stopped/restarted against the same disposable database, retaining immutable progress receipts. |
| macOS `replay` | A new process restores submitted A, fetches revision 2, then replays A's exact bytes. Its historical revision-1 receipt settles A without replacing the newer observation. |
| Android `verify` | A new process restores its encrypted snapshot, fetches revision 2 and agrees with the service without issuing another PUT. |

The runner independently compares the full canonical delta before and after,
checks that the child's progress remains empty, and inspects PostgreSQL for
exactly one sidecar and two immutable successful receipts at revisions 1 and 2.
The replacement includes 100% and zero remaining time: neither silently completes
the goal or changes its scheduling metadata. Quantity values retain six-place
decimal precision and an explicit target direction.

The preparation path treats canonical deltas as ordered changes, not unique-item
snapshots: the seeded goal and child produce three changes for two current items.
Android retains the actual terminal delta cursor alongside those records, so
encrypted restart preserves complete-cache admission instead of inventing read
proof. Always-on tests cover this admission boundary, revision-safe folding,
partial opt-in rejection and encrypted restart/tamper behavior. Eight runner
safety tests cover environment isolation, redirect rejection, private artifacts,
timeouts, interruption and descendant-process cleanup.

The post-change default gates also passed: macOS discovered 976 tests in 62
suites with warnings treated as errors; Android discovered 1,567 JVM tests in
126 suites with no failures/errors, and lint reported no errors and 29 existing
warnings. Each default run intentionally skips its one live phase; the six
explicitly configured executions above passed separately. No production source,
database migration or app-distribution configuration changed in this checkpoint.

## Coverage boundary

macOS uses `DayWeaveAPIClient`, `ItemProgressStore`, `PlannerStore` and the real
encrypted planner persistence. Android uses the real OkHttp transports,
`ItemProgressSyncManager`, `PlannerStore` and production snapshot codec through a
test-only AES-GCM file-backed DAO. Android's JVM adapter is **not** evidence of
SQLCipher, Android Keystore or UI behavior; those have separate instrumented
checks. Static synthetic authentication here is **not** evidence of the final
credential-only device enrollment/refresh flow.

Selected-detail polling, foreground/reconnect orchestration, real Android-device
networking, privacy lock transitions, TLS, owner-device interaction and the
seven-day acceptance trial remain outside this harness. Its results must not be
used to mark unrelated hierarchy, habit, account-recovery or full-app gates done.
