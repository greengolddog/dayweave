# DayWeave for Android

Native Jetpack Compose client for `com.greengolddog.dayweave`. The current foundation includes the Today timeline, five-destination navigation, quick capture, active-session controls, assistant chat, and a proposal-only Suggestions Inbox.

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

## Next integration gates

- Replace preview state with the encrypted offline database and repository layer.
- Connect the shared scheduler API and background sync worker.
- Configure Google OAuth credentials and isolated Calendar/Tasks test resources.
- Configure release signing outside version control before producing the direct-download APK.
