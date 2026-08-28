# DayWeave for Android

Native Jetpack Compose client for `com.greengolddog.dayweave`. The current foundation includes the Today timeline, five-destination navigation, quick capture, active-session controls, assistant chat, and a proposal-only Suggestions Inbox.

Planner state is stored offline in a Room database encrypted by SQLCipher. A random 256-bit database passphrase is AES-GCM wrapped by a non-exportable Android Keystore key; plaintext key material is never written to storage. Startup restores the last atomic snapshot, replays any input received during loading, and autosaves subsequent intents through one serialized writer.

## Build

Requirements:

- JDK 17
- Android SDK Platform 36
- Android SDK Build Tools and Platform Tools

On this Mac, the Homebrew SDK is at `/opt/homebrew/share/android-commandlinetools`. Build with:

```sh
export JAVA_HOME="/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home"
export ANDROID_HOME="/opt/homebrew/share/android-commandlinetools"
./gradlew testDebugUnitTest assembleDebug
```

The debug APK is written to `app/build/outputs/apk/debug/app-debug.apk`.

For the device smoke test, start an API 35 emulator or connect the Pixel with USB debugging enabled, then run:

```sh
./gradlew connectedDebugAndroidTest
```

## Safety boundary

`PlannerStore.approveSuggestion` is intentionally proposal-only. Accepting a ChatGPT, Codex, or in-app assistant proposal creates a reviewable Inbox draft and does not mutate the schedule. Unit tests lock this behavior down before remote integrations are connected.

## Persistence safety

The encrypted database, its WAL files, and the wrapped passphrase are excluded from cloud backup and device transfer because Android Keystore keys are device-bound. If the wrapping key is unavailable or the snapshot format cannot be decoded, persistence fails closed instead of silently replacing the existing database.

Room schema history is exported under `app/schemas`, and explicit migrations are required—there is no destructive migration fallback.

## Next integration gates

- Connect the shared scheduler API and background sync worker.
- Configure Google OAuth credentials and isolated Calendar/Tasks test resources.
- Configure release signing outside version control before producing the direct-download APK.
