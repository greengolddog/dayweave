# DayWeave for Android

Native Jetpack Compose client for `com.greengolddog.dayweave`. The current foundation includes the Today timeline, five-destination navigation, quick capture, active-session controls, assistant chat, and an authenticated proposal-only Suggestions Inbox.

Planner state, including the last server proposal cache, is stored offline in a Room database encrypted by SQLCipher. A random 256-bit database passphrase is AES-GCM wrapped by a non-exportable Android Keystore key; plaintext key material is never written to storage. Startup restores the last atomic snapshot, blocks edits until restore finishes, and autosaves subsequent intents through one serialized writer.

The Suggestions tab connects to the existing `/v1/suggestions` API with OkHttp and a bearer token. The token is separately AES-GCM wrapped with its own non-exportable Android Keystore key and is never placed in the Room planner snapshot or application logs. The connection preference and both encrypted databases are excluded from backup and device transfer.

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

The debug APK is written to `app/build/outputs/apk/debug/app-debug.apk`.

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

## Persistence safety

The encrypted database, its WAL files, and the wrapped passphrase are excluded from cloud backup and device transfer because Android Keystore keys are device-bound. If the wrapping key is unavailable or the snapshot format cannot be decoded, persistence fails closed instead of silently replacing the existing database.

Room schema history is exported under `app/schemas`, and explicit migrations are required—there is no destructive migration fallback.

## Next integration gates

- Add an account-facing control if users should be able to opt out of periodic suggestion refresh while retaining API credentials.
- Configure Google OAuth credentials and isolated Calendar/Tasks test resources.
- Configure release signing outside version control before producing the direct-download APK.
