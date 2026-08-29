# DayWeave for Android

Native Jetpack Compose client for `com.greengolddog.dayweave`. The current foundation includes the Today timeline, five-destination navigation, quick capture, active-session controls, assistant chat, and an authenticated proposal-only Suggestions Inbox.

Planner state, including the last server proposal cache, is stored offline in a Room database encrypted by SQLCipher. A random 256-bit database passphrase is AES-GCM wrapped by a non-exportable Android Keystore key; plaintext key material is never written to storage. Startup restores the last atomic snapshot, blocks edits until restore finishes, and autosaves subsequent intents through one serialized writer.

Canonical focus sessions use the server-authoritative `/v1/execution` lease. Android reconciles that lease before every start, pause (duration, absolute end, or open-ended), resume, complete, and skip command. The app persists a stable random device UUID plus the exact request body and idempotency key before network I/O; an ambiguous response or process restart therefore retries byte-for-byte instead of creating local success or a second session. HTTP 401 keeps the exact retry fenced, while deterministic 404/409/422 responses trigger an authoritative snapshot/history reconciliation. Noncanonical preview blocks still use device-local timing, but canonical blocks never fall back to local execution while offline.

A timed break never resumes automatically. When its server deadline passes, the explicit Resume / Extend 10m / Keep paused choice appears. Keep paused closes the message without inventing a server mutation, and extending sends another revision-guarded pause command while the lease stays paused.

While the UI is visible, one lifecycle-bound job refreshes the execution lease immediately and every 30 seconds. The process action gate coalesces that work with taps, settings changes, and composition so foreground polling cannot create duplicate command jobs. If a cached lease disappears, Android pages the complete execution history between two equal execution snapshots before applying its terminal outcome; a racing, incomplete, or malformed history read retains any command fence and leaves execution locked instead of guessing. The encrypted snapshot keeps a rolling 100-session history window for display plus a lifetime terminal-outcome ledger for schedule correctness, so old completed work cannot reappear after enough newer sessions.

The Suggestions tab connects to the existing `/v1/suggestions` API with OkHttp and a bearer token. The token is separately AES-GCM wrapped with its own non-exportable Android Keystore key and is never placed in the Room planner snapshot or application logs. The connection preference and both encrypted databases are excluded from backup and device transfer.

## Sensitive-item authoring

Quick capture has an explicit **Sensitive** switch, and captured drafts retain that classification
inside the encrypted planner snapshot. Sensitive drafts and composed blocks have visible privacy
indicators. For canonical items, **More → Appearance & privacy → Sensitive items** shows both the
item's own setting and effective protection inherited from any ancestor. Unscheduled goals and
other non-executable parents are included, so protecting a parent immediately protects every
cached descendant block without changing its placement.

Canonical privacy changes use a complete item replacement guarded by the current server revision.
Android durably journals the exact body, idempotency key, status, and target sensitivity before the
request leaves the device. A lost response is replayed byte-for-byte; a newer conflicting revision
is shown for review and is never silently rebased into a declassification. Removing an own
sensitive label requires confirmation bound to the exact reviewed revision, and the dialog
explicitly says when parent protection will continue to apply. A pending promotion immediately
hardens the target and every cached descendant because its response may already have committed;
a pending removal never lowers local protection. The Room 4-to-5 and JSON-v3-to-v4 migration
preserves the old strict sensitivity
contract, adds explicit draft/write-target fields, and derives any in-flight target only from its
already-journaled replacement body.

This authoring surface does not itself grant disclosure. Sensitive values remain excluded by
default from assistant/MCP and future locked notification, widget, indexing, attachment, and export
surfaces; those surfaces require their own tested policy gate before they can be enabled.

## Build

Requirements:

- JDK 17
- Android SDK Platform 36
- Android SDK Build Tools and Platform Tools

On this Mac, the Homebrew SDK is at `/opt/homebrew/share/android-commandlinetools`. Build with:

```sh
export JAVA_HOME="/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home"
export ANDROID_HOME="/opt/homebrew/share/android-commandlinetools"
./gradlew testDebugUnitTest lint assembleDebug assembleRelease compileDebugAndroidTestKotlin
```

The debug APK is written to `app/build/outputs/apk/debug/app-debug.apk`. A signed minified release is produced with `scripts/build-android-apk.sh`, using the private external signing properties created by `scripts/create-android-signing-key.sh`. The generator rejects lexical or symlink-resolved destinations inside the repository and its Git metadata before invoking any private-key tool.

The supported floor is Android 9 / API 28, so the direct-download release APK intentionally uses APK Signature Scheme v3 only. Gradle disables v1, v2, and the uncopied v4 sidecar; the release script runs verbose `apksigner` verification before copying `dist/android/DayWeave-release.apk`. Release acceptance requires v3 `true`, v1/v2 `false`, and exactly one signer in that output.

## App lock

**More → Appearance & privacy → App lock** enables an opt-in presentation lock.
Both enabling and disabling it require a fresh, successful system-owned
AndroidX BiometricPrompt; reaching an already-unlocked settings screen is not
enough to remove protection. On Android 11 and newer, the prompt accepts a
strong biometric or the enrolled device PIN, pattern, or password. Android 9
and 10 use a strong biometric because those releases cannot combine a
cryptographic prompt with device-credential fallback. DayWeave follows the
[official BiometricPrompt guide](https://developer.android.com/identity/sign-in/biometric-auth)
and uses the [current stable AndroidX Biometric 1.1.0 release](https://developer.android.com/jetpack/androidx/releases/biometric)
with an auth-per-use, non-exportable Android Keystore signing key. A prompt
callback unlocks the app only after its `CryptoObject` signs a fresh random,
attempt-bound challenge that verifies against that exact key; a bare or forged
success callback fails closed. DayWeave never receives or stores biometric or
device-credential material.

When enabled, every cold start is locked. Leaving the app locks immediately or
after the selected 1, 5, 15, or 30 minute timeout measured by the monotonic
clock. Generation-bound state rejects late callbacks and timer/foreground-return
races. A process-wide single-flight fence
keeps a cancelled platform attempt occupied until its terminal callback drains;
every platform callback retains its immutable attempt identity, so it cannot
complete a replacement request. Configuration changes recreate BiometricPrompt
early and transfer ownership of the same attempt without marking a background
transition or starting a second prompt. The launcher Activity is `singleTask`,
so one owner accounts for real foreground/background transitions; a stale
recreated Activity cannot cancel the transferred prompt.

A real Activity stop during device-credential fallback immediately replaces an
unlocked protected composition with the lock screen while retaining the exact
prompt. A terminal success is never applied while stopped and expires unless
the prompt returns to the foreground within a short one-shot handoff; a delayed
or missing terminal result cannot restore content, and cancellation keeps or
restores the lock. The locked composition is created before the planner
ViewModel and contains no task, calendar, assistant, title, dialog, or account
content. `FLAG_SECURE` protects the activity from screenshots, screen sharing,
non-secure displays, and recent-app thumbnails throughout protection and every
authentication transition.

Only the non-secret enabled flag and timeout live in a strict, versioned
`AtomicFile` under `noBackupFilesDir`; planner data remains in the existing
device-bound SQLCipher store. A genuinely absent atomic and legacy record means
the first-install opt-in feature is off. Zero-length, truncated, malformed,
unknown-schema, trailing-data, and partial legacy records fail closed. A
complete legacy preferences record is copied durably before its source is
cleared. Any corrupt record can be repaired only after successful device
authentication and a durable atomic rewrite; recovery never clears or replaces
the encrypted planner database.

The JVM tests use only a fake authenticator, fake monotonic clock, and synthetic
settings. Final acceptance on the target Pixel must enable the lock, exercise a
biometric and device-credential fallback, cancel and retry the prompt, rotate
or recreate the activity during a prompt, verify every timeout/background path,
remove and re-enroll device authentication, and inspect the recent-apps surface.
Do not record, screenshot, or commit real biometric, device, account, or planner
data during that pass.

## Health Connect energy context

**More → Health & context** provides an opt-in Health Connect connection using
the stable AndroidX Health Connect 1.1.0 client. It checks official SDK
availability, offers the provider update route when required, requests only
read-sleep access through the Health Connect activity-result contract, and links
back to Health Connect for access management. No write, background, health
history, heart-rate, account, or Google OAuth permission is requested.

While the app is foregrounded, the provider reads only the 24-hour aggregate
sleep duration and immediately reduces it to Low/Medium/Deep energy plus a broad
recovery band. The encrypted planner snapshot retains only those bands and the
calculation time; raw records, session timestamps/stages, titles, notes, and IDs
are neither returned across the provider boundary nor persisted or uploaded.
Disabling sync, denying/revoking access, provider unavailability, or a read error
clears the derived estimate and leaves core planning fully usable.

Today always offers a manual Low/Medium/Deep check-in. Selecting another value
corrects the check-in; clearing it returns to a non-stale Health Connect estimate
when one exists. The current band powers only a “best current fit” hint against
the existing schedule. The server compose API has no current-energy field yet,
so this slice never silently changes the plan and is not a medical feature.

The JVM suite uses `FakeEnergySignalProvider` with synthetic bands. A final
physical-device pass on the target Pixel must cover install/update/unavailable,
grant, deny, revoke, rationale, Manage access, and a synthetic sleep aggregate.
Never use, export, screenshot, or commit real health data. Play distribution also
requires the Health apps declaration to match the in-app rationale exactly.

## Suggestions API configuration

The API base URL can be supplied at build time and must use HTTPS:

```sh
./gradlew -PdayweaveApiBaseUrl=https://api.example.com/ assembleDebug
```

It can also be set or replaced from **Inbox → Suggestions → Connection**. Enter the bearer token there; it is never accepted as a Gradle property or compiled into `BuildConfig`. A runtime URL overrides the optional build-time default. Cleartext traffic is disabled in the production manifest, and API redirects are rejected rather than replaying authentication. The Inbox reports missing configuration, authentication failures, network/offline failures, server errors, and the timestamp of the last successful sync without discarding the encrypted cached proposals.

When both an HTTPS endpoint and usable device-bound bearer token are present, WorkManager maintains one unique periodic suggestion refresh. It uses a conservative 12-hour interval with a 2-hour flex window, requires a connected network, and applies exponential retry backoff beginning at 30 minutes. A separate unique one-time refresh is kept on configured app startup; saving connection settings replaces that one-time request so the new configuration is guaranteed a subsequent refresh even if startup work was already running. Network failures, HTTP 408/429, and HTTP 5xx responses retry; authentication, configuration, protocol, other HTTP, and local encrypted-storage failures complete as failures. WorkManager does not support a per-result retry delay, so a server `Retry-After` value cannot replace the configured exponential backoff. Forgetting credentials first awaits WorkManager's cancellation operations for both unique paths; its serialized credential clear also waits behind any in-flight sync. It then independently attempts durable ciphertext removal and destruction of its Keystore key. Either successful removal makes stale ciphertext unusable after restart; any incomplete cleanup remains visible as a failed Forget action. No work is enqueued without both ciphertext and its device-bound key. WorkManager input and output data contain no credentials or proposal content.

For the device smoke test, start an API 35 emulator or connect the Pixel with USB debugging enabled, then run:

```sh
./gradlew connectedDebugAndroidTest
```

## Safety boundary

Accepting a ChatGPT, Codex, or in-app assistant proposal first records the revision-aware API decision and then creates a reviewable Inbox draft. It never mutates the schedule. On the next refresh, any accepted server proposal is reconciled idempotently into a draft, so an interrupted client response cannot bypass review or lose the accepted proposal locally.

A refresh or proposal mutation reports success only after the exact replaced/reconciled planner generation has completed its encrypted Room save. That acknowledgement is limited to server sync; ordinary UI intents remain non-blocking and are serialized by the same writer.

Execution completion is durably block-scoped first: the exact terminal lease identity is retained independently of schedule composition, so a fresh preview or restart cannot resurrect it. For a non-recurring, indivisible executable leaf represented by one fully scheduled block, Android then projects completion/skip through the existing durable, idempotent canonical item-replacement fence before recomposing. If another client edited the item first, Android safely rebases onto the latest revision while preserving every latest field; an already-matching terminal item or tombstone resolves without another write. A latest item that became recurring, split, otherwise ineligible, or oppositely terminal produces a durable review card with **Retry reconciliation** and **Keep latest as new work** actions, and remains non-startable until explicitly resolved. Recurring and split work remains exact-occurrence/session only and never silently completes its parent or siblings. The server execution API still has no atomic “release this lease and defer/recompose this occurrence” command, so **Will do later** remains disabled while a canonical lease is open; occurrence-level cross-device parent projection remains a server API gap, not a local fallback.

## Persistence safety

Application backup and device transfer are disabled in the manifest; the encrypted database, its WAL files, and the wrapped passphrase are also named in exclusion rules as defense in depth because Android Keystore keys are device-bound. If the wrapping key is unavailable or the snapshot format cannot be decoded, persistence fails closed instead of silently replacing the existing database.

Room schema history is exported under `app/schemas`, and explicit migrations are required—there is no destructive migration fallback.

## Next integration gates

- Add an account-facing control if users should be able to opt out of periodic suggestion refresh while retaining API credentials.
- Configure Google OAuth credentials and isolated Calendar/Tasks test resources.
- Complete the Health Connect/Play declaration and target-Pixel physical-device gate using synthetic data only.
- Configure release signing outside version control before producing the direct-download APK.
