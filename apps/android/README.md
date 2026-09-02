# DayWeave for Android

Native Jetpack Compose client for `com.greengolddog.dayweave`. The current client includes the Today timeline, five-destination navigation, canonical quick capture and detailed item authoring, active-session controls, assistant chat, and an authenticated Suggestions Inbox.

Planner state, including the last server proposal cache, is stored offline in a Room database encrypted by SQLCipher. A random 256-bit database passphrase is AES-GCM wrapped by a non-exportable Android Keystore key; plaintext key material is never written to storage. Startup restores the last atomic snapshot, blocks edits until restore finishes, and autosaves subsequent intents through one serialized writer.

Canonical focus sessions use the server-authoritative `/v1/execution` lease. Android reconciles that lease before every start, pause (duration, absolute end, or open-ended), resume, complete, and skip command. The app persists a stable random device UUID plus the exact request body and idempotency key before network I/O; an ambiguous response or process restart therefore retries byte-for-byte instead of creating local success or a second session. HTTP 401 keeps the exact retry fenced, while deterministic 404/409/422 responses trigger an authoritative snapshot/history reconciliation. Noncanonical preview blocks still use device-local timing, but canonical blocks never fall back to local execution while offline.

Schedule preview is side-effect-free. After Android validates a preview against the exact pulled item
revision map, it durably writes a versioned publication journal containing the full canonical
`POST /v1/schedule/publish` URL, method, security headers, body bytes/digest, idempotency key,
credential binding, and still-uninstalled candidate plan. Only an exact HTTP 200 receipt whose
revision, digest, horizon, and timezone match that journal can atomically install the candidate and
advance the delta cursor. A timeout, cancellation, process death, trusted-401 credential rotation,
or lost response retains and replays the same request. A strictly typed
`schedule_publication_stale` 409 durably discards only that candidate and performs one bounded fresh
pull/preview/new-key publication; generic and idempotency-conflict 409s remain journaled. An HTTP 200
with `replayed=true` proves the old request once committed but not that it is still the newest
revision, so Android resolves the journal without installing its candidate, invalidates the local
publication proof, and freshly recomposes before enabling the plan. Invalid responses leave the
exact journal intact and block silent credential replacement. Explicit local-only removal is the
warned escape hatch for irreconcilable ambiguity. Room 5-to-6 / JSON-v4-to-v5 adds this crash journal
and its published-revision receipt, and a pending journal always makes the cached plan non-current.
Before constructing or persisting that journal, Android caps the exact UTF-8 publication body at
12 MiB. This deliberately accommodates the separately bounded 8 MiB retained plan while leaving
headroom below the server's 16 MiB request limit; 12 MiB is accepted and even one additional byte
is rejected locally without staging durable publication state.

A timed break never resumes automatically. When its server deadline passes, the explicit Resume / Extend 10m / Keep paused choice appears. Keep paused durably acknowledges that exact break in the encrypted planner state without inventing a server mutation, cancels its delayed reminder, and survives restart; a later lease revision or deadline is a new break. Extending sends another revision-guarded pause command while the lease stays paused.

Canonical timed pauses also schedule one background WorkManager reminder from
the last planner generation confirmed written to SQLCipher. Work input, tags,
notification metadata, and the immutable one-shot tap intent contain only a
domain-separated SHA-256 identity; the title and body are fixed generic text
with no task title, notes, raw item/session identifier, or mutation action. At
delivery, the worker reopens the encrypted store, requires the exact still-
paused lease/revision/deadline, and atomically persists both the existing
resolution UI and an at-most-once delivery claim before calling Android's
notification service. Process death after that claim can lose the convenience
banner, but never replays an ambiguous alert; the in-app resolver remains the
reliable fallback. Permanent or ambiguous platform post failures are terminal,
and the pre-claim early-clock retry is bounded. Resume, completion, skip, defer,
Keep paused, lease replacement, and credential quarantine use an awaited
cancellation barrier before their destructive transition, then reconcile the
durable reminder again on success or failure. Enqueue and cancellation
operations are awaited, and a process-wide side-effect gate joins any already-
running post before the final fixed-ID cancellation returns; a cold process
retains an already active exact job, while a transient scheduler failure leaves
the same encrypted generation eligible for reconciliation retry. Android 13
and newer request notification permission only after the server has confirmed
and durably persisted a new canonical timed pause (including Extend), never for
a device-local/noncanonical pause. The contextual one-shot request remains in
ViewModel state across stop, app lock, and Activity recreation until a resumed,
unlocked UI revalidates it. If the process dies first, the same encrypted future-
break truth produces a visible **Enable reminders** affordance; a prior denial
or disabled channel routes explicitly to Android notification settings and is
never auto-prompted on launch. Denial or a disabled channel never blocks the
pause or causes a retry loop.

The notification service synchronously commits an app-private, opaque issued-
route capability before posting the banner. Its immutable one-shot PendingIntent
targets the non-exported `MainActivity` directly. The only exported launcher is
a no-history boundary that constructs a fresh `ACTION_MAIN` intent and forwards
none of a caller's action, data, categories, clip data, flags, or extras. On a
real tap, `MainActivity` atomically moves the issued digest into a private durable
mailbox before composition and immediately replaces its own stored intent with a
route-free value. This retains the tap across process restore and app lock while
preventing another installed app, Activity recreation, or an old task-base intent
from forging/replaying it. The encrypted planner receipt then revalidates both
durable and live exact lease truth before the mailbox is cleared. A stale alert
cannot surface a newer break: it leaves a generic changed-reminder fence until
the user explicitly chooses **Review current break** or **Not now**, and a failed
mailbox clear leaves that choice retryable. Resume and Extend keep the resolver
visible until authoritative state actually changes, and no path auto-resumes.

WorkManager timing is OS best-effort and may be delayed by Doze. Final
acceptance on the target Pixel must cover permission grant/denial, process
death, reboot, Doze delivery, lock/unlock tap routing, launcher/Recents behavior,
and real banner/sound appearance. A resume performed on another device can cancel this reminder only
after Android receives that newer lease; near-real-time background execution
updates remain part of the separate push-sync requirement.

While the unlocked UI is visible, lifecycle-bound work refreshes the execution lease immediately
and every 30 seconds. Alongside that unchanged fallback, Android opens the authenticated
`/v1/execution/stream` and treats its content-free revision events only as memory-only wakeups for
the same stable snapshot/history reconciliation. The resume header always comes from the last
encrypted durable execution revision, never from an event. A missing endpoint disables streaming
only for that foreground activation; malformed streams fail closed, transient disconnects back
off, and background, lock, or credential replacement cancels and drains the exact OkHttp call.
The process action gate coalesces that work with taps, settings changes, and composition so an
invalidation observed while another action is busy remains queued instead of creating duplicate
command jobs. If a cached lease disappears, Android pages the complete execution history between
two equal execution snapshots before applying its terminal outcome; a racing, incomplete, or
malformed history read retains any command fence and leaves execution locked instead of guessing.
The encrypted snapshot keeps a rolling 100-session history window for display plus a lifetime
terminal-outcome ledger for schedule correctness, so old completed work cannot reappear after
enough newer sessions.

Canonical items use the same unlocked, lifecycle-bound invalidation boundary. Android opens
`/v1/items/stream` with only the exact opaque delta cursor from the last SQLCipher-confirmed
planner generation; an unbound first sync omits `Last-Event-ID`, and event cursors are never saved
or ordered by the client. Opaque events are coalesced by a local generation and enter the existing
execution reconciliation → complete item delta → composition → idempotent schedule publication
path through the process action gate. A successful own item write whose emitted cursor is already
covered by that durable commit is cleared without a second composition. Independently, a
`limit=1` delta probe runs once on foreground activation and at most every 30 seconds, so a missing
stream endpoint or missed wakeup still converges without periodically composing an unchanged day.
Item streaming and probing stop on background, app lock, or credential replacement; replacement
drains their exact calls before the encrypted canonical cache is quarantined.

Published schedules also replicate natively between devices. On a clean startup and while the
unlocked UI is foregrounded, Android reads `/v1/schedule/current`, drains canonical item deltas,
refetches the immutable head to close a publication/item race, and validates the exact revision,
strong ETag, input digest, horizon, timezone, source revision map, occurrences, blocks, scores, and
manual-placement evidence. The complete plan and its execution proof are installed together in one
SQLCipher generation only if the credential binding and last durable planner snapshot are still
exact. A trusted typed 404 can clear obsolete publication authority; a generic 404 cannot. Startup
always replays any durable publication, authoring, execution, defer, or projection journal before
this read-only recovery path, so process death never strands an ambiguous write behind polling.

While foregrounded, `/v1/schedule/stream` supplies only content-free unsigned revision hints. Its
`Last-Event-ID` comes exclusively from the encrypted installed proof, never from memory or an SSE
event. A strict cursor-ahead 409 stops that stream and forces the authoritative GET; a missing
stream falls back to the independent immediate/30-second GET. Events are coalesced through the
process action gate, reconnect with bounded backoff, and cannot mutate state themselves. App lock,
backgrounding, or credential quarantine stops new polling and cancels the SSE call; credential
replacement additionally drains old-binding work before quarantining the encrypted cache.

All authenticated Android APIs share one process-wide device-auth coordinator. Contract version 2
adds the REST-only `schedule_publish` scope to the Android enrollment tuple; it is not an MCP scope.
An older active or pending contract cannot gain that authority through refresh and therefore fails
closed until the user explicitly revokes/removes and re-enrolls. A first installation either
upgrades with the reviewed hybrid bootstrap credential or consumes an already-minted one-time
`dw_en1_` enrollment code bound to the client ID displayed by Android. The resulting access and
refresh credentials live only in a versioned whole-state envelope encrypted by a non-exportable
Android Keystore key; they are never placed in the Room planner snapshot, WorkManager data, logs,
or UI errors. The auth envelope, connection preference, and encrypted databases are excluded from
backup and device transfer.

## Google Calendar import

**More → Calendar sources** exposes Android's inbound-only Google Calendar configuration. After a
read-only Google connection is active, the app discovers every calendar and lets each source be
**Off**, **Show only**, or **Block time**. Existing writable source settings created on another
DayWeave client remain visible as **Writable · managed on another device**, but Android never
offers or sends the writable role. Google Tasks selection and outbound Calendar publication are
separate planned surfaces and are not implied by these controls.

Import refresh is crash-safe. Android writes a credential-bound request UUID outside backup before
the refresh POST, records the exact accepted server generation, and requires an authoritative idle
status at or beyond that generation. It then refreshes canonical items, composes and publishes the
schedule, and waits for the encrypted planner generation before removing the exact import marker.
A timeout, cancellation, process death, server backoff, or lost response retains the marker for a
bounded **Check import** recovery with the same identity. The marker contains no bearer token,
calendar name, event payload, or calendar identifier. Ordinary API credential replacement plus
Google account pause and disconnect are fenced while import recovery is outstanding; resuming a
paused account remains available so the saved import can finish. Only the existing explicitly
confirmed local-destruction flow can abandon an irrecoverable marker.

## Canonical authoring and recovery

Android has a typed, encrypted, offline-first path for canonical create, replace, trash, and restore
intent. Quick capture writes title-only tasks, routines, goals, and breaks directly to Inbox after
durable local persistence. Habits and events continue into the detailed editor so recurrence and
exact timing are never guessed. The editor covers task, habit, routine, goal, event, and break
items; recurrence, fixed event timing, split policy, priority, energy and spacing constraints,
privacy, and unbounded parent/child hierarchy are validated before an intent can enter the journal.

Inbox separates canonical Inbox, Planned, conflicts, and Recently Deleted while retaining older
local review drafts. Accepted assistant/proposal drafts and older captures have a **Review as
item** path that carries their title, context, and sensitivity into the typed editor; successful
conversion atomically removes the review draft in the same encrypted generation that creates its
canonical journal. Queued creates and replacements remain editable before submission; deletion
requires confirmation; conflicts can be retained, discarded, or copied to a fresh standalone
Inbox identity. Each network request receives a stable idempotency identity and is persisted before
its first byte can leave the device. A successful local save schedules best-effort background sync
without making network availability part of local success, and **Retry sync** remains explicit.
Sync pulls an authoritative preflight snapshot, submits parent/child operations in dependency
order, rebases unsent parent revisions after proven child-only hierarchy side effects, and accepts
only an exact response matching the durable draft and expected revision. Ambiguous responses retain
their byte-equivalent retry, whereas trusted deterministic conflicts—including an exact server
not-found response after remote deletion—become visible review records. Submitted uncertainty,
refresh overlays, and schedule-proof invalidation survive process death.

Recently Deleted recovery bodies and the duplicate bases held by trash/restore journals expire
after 30 days, with separate count and byte limits. A local retention anchor prevents a future
provider timestamp from extending that window; a quiet-process timer strips expired bodies and
durably rewrites the encrypted snapshot even when the user does nothing. Minimal bodyless restore
metadata and request identity remain available for safe replay, while sensitivity fails closed.
Room 7-to-8 / JSON-v6-to-v7 is a rollback fence for this encrypted payload contract; no authoring
content or provider credential is added to a plaintext database column.

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

## On-device schedule composition

**Compose on this device** creates one deterministic local-day composition from the exact
encrypted canonical cache. It invokes the bounded Rust scheduler through a byte-array JNI boundary
and never sends a composition request, advances a delta cursor, publishes a schedule, or changes an
execution lease. The displayed composition is explicitly local-only: Start, skip, and move-later
remain fail-closed until the day is synced and a server-published execution-authoritative plan is
installed.

The encrypted Room snapshot persists an exact local provenance record: the credential binding and
cursor, source item revisions, local input fingerprint, request fingerprint, day/time zone, and
the resulting block revisions. The app displays a local plan only while every one of those inputs
still matches. Item, recurrence, availability, execution, binding, or time-zone changes invalidate
it rather than reusing stale blocks. It also cancels local composition and discards a late native
result whenever the UI stops, the app locks, or credential/binding state changes.

The JNI library is generated at build time and is not checked in. Android builds only
`arm64-v8a` and `x86_64` scheduler libraries, with Android NDK `28.2.13676358`, Rust `1.95.0`, and
the `aarch64-linux-android` and `x86_64-linux-android` Rust targets. With `ANDROID_HOME` (or
`ANDROID_SDK_ROOT`) pointing at an SDK containing that NDK, build either variant directly:

```sh
rustup target add --toolchain 1.95.0 aarch64-linux-android x86_64-linux-android
scripts/build-android-scheduler-library.sh debug
scripts/build-android-scheduler-library.sh release
scripts/tests/test-build-android-scheduler-library-hostile-environment.sh debug
```

Gradle runs the corresponding generated-library build before debug or release JNI packaging, so
the normal `assembleDebug`, `assembleRelease`, and signed
`scripts/build-android-apk.sh` paths require the same NDK and Rust targets. Generated `.so` files
remain under `app/build/generated/jniLibs/`; never commit them or any signing/credential material.

## Build

Requirements:

- JDK 17
- Android SDK Platform 36
- Android SDK Build Tools and Platform Tools
- Android NDK 28.2.13676358
- Rust 1.95.0 with `aarch64-linux-android` and `x86_64-linux-android` targets

On this Mac, the Homebrew SDK is at `/opt/homebrew/share/android-commandlinetools`. Build with:

```sh
export JAVA_HOME="/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home"
export ANDROID_HOME="/opt/homebrew/share/android-commandlinetools"
rustup target add --toolchain 1.95.0 aarch64-linux-android x86_64-linux-android
./gradlew testDebugUnitTest lint assembleDebug assembleRelease compileDebugAndroidTestKotlin
```

The debug APK is written to `app/build/outputs/apk/debug/app-debug.apk`. A signed minified release is produced with `scripts/build-android-apk.sh`, using the private external signing properties created by `scripts/create-android-signing-key.sh`. The generator rejects lexical or symlink-resolved destinations inside the repository and its Git metadata before invoking any private-key tool. The build entry point independently applies that same boundary to both the properties file and its Java-properties-decoded `storeFile`, and requires each input to be a regular, single-link, non-symlink file with mode `0600` before Gradle can run.

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

It can also be set from **Inbox → Suggestions → Connection**. The dialog shows the installation's stable client ID and offers two deliberately separate enrollment paths: paste a one-time `dw_en1_` code minted for that exact ID, or perform the reviewed hybrid migration with a bootstrap credential. Neither value is accepted as a Gradle property or compiled into `BuildConfig`. Healthy active or refresh-pending credentials cannot be replaced in place. A runtime URL overrides the optional build-time default. Cleartext traffic is disabled in the production manifest, cross-origin/path escape is rejected, and API redirects are rejected rather than replaying authentication.

Bootstrap creation first journals a client-proposed enrollment ID, fresh one-time token, stable client ID, and the exact canonical URL, method, security headers, and encoded body before network I/O. It accepts only the server's exact echoed tuple as a 201 first result or 200 semantic replay; a crash cannot reconstruct or retarget that request from changed settings. Enrollment consumption and refresh likewise durably journal the complete random credential tuple, client/session binding, and preparation time, then retry that exact tuple after a timeout or process restart. Successful rotation is installed with an exact compare-and-set; proactive refresh and the single permitted trusted-401 refresh are coalesced by the process coordinator. The retried application request is byte-identical and is returned only if the full auth-envelope identity and account/session binding still match. Legacy bootstrap authority is enrollment-only, and once durable enrollment has activated Android never falls back to a legacy static bearer for ordinary API calls, including during reauthentication or an ambiguous recovery.

When an active device session is present, WorkManager maintains one unique periodic suggestion refresh. It uses a conservative 12-hour interval with a 2-hour flex window, requires a connected network, and applies exponential retry backoff beginning at 30 minutes. A separate unique one-time refresh is kept on configured app startup. Network failures, HTTP 408/425/429, and HTTP 5xx responses retry; deterministic authentication, configuration, protocol, other HTTP, and encrypted-storage failures do not spin indefinitely. WorkManager input and output data contain no credentials or proposal content.

The stable device-session binding fences every API client and API-derived cache. A binding change first durably quarantines canonical schedule/session state, remote suggestions, and external inbox proposals so an in-flight response from an old account cannot repopulate the new context. Normal sign-out requires the authenticated current-session revoke to succeed with the server's exact empty 204 response before local credentials are removed. A failed or ambiguous revoke retains the local state for retry. A separate, explicit **Local-only removal** confirmation can destroy the encrypted envelope and wrapping key as a last resort; its warning makes clear that the server session and reviewed bootstrap authority may remain active. If obsolete-key deletion fails after credential ciphertext is durably removed, a fail-closed tombstone retries cleanup and the UI reports that precise partial outcome rather than claiming credentials remain.

For the device smoke test, start an API 35 emulator or connect the Pixel with USB debugging enabled, then run:

```sh
./gradlew connectedDebugAndroidTest
```

## Safety boundary

Accepting an ordinary advisory ChatGPT, Codex, or in-app assistant proposal
first records the revision-aware API decision and then creates a reviewable
Inbox draft. It never mutates the schedule. On the next refresh, any such
accepted server proposal is reconciled idempotently into a draft, so an
interrupted client response cannot bypass review or lose the accepted proposal
locally.

A supported `dayweave.proposal-change-set/1` proposal uses a separate fail-closed
path: Android validates and displays every exact direct and implicit changed
value, binds explicit approval to the proposal revision plus preview ID/hash,
persists the exact non-secret apply or undo request before network I/O, and
stores only a content-free receipt. Unknown reserved schema versions cannot use
legacy acceptance. An uncertain result blocks canonical/execution mutation
until authoritative lookup or exact idempotent replay resolves it.

A refresh or proposal mutation reports success only after the exact replaced/reconciled planner generation has completed its encrypted Room save. That acknowledgement is limited to server sync; ordinary UI intents remain non-blocking and are serialized by the same writer.

Execution completion is durably block-scoped first: the exact terminal lease identity is retained independently of schedule composition, so a fresh preview or restart cannot resurrect it. For a non-recurring, indivisible executable leaf represented by one fully scheduled block, Android then projects completion/skip through the existing durable, idempotent canonical item-replacement fence before recomposing. If another client edited the item first, Android safely rebases onto the latest revision while preserving every latest field; an already-matching terminal item or tombstone resolves without another write. A latest item that became recurring, split, otherwise ineligible, or oppositely terminal produces a durable review card with **Retry reconciliation** and **Keep latest as new work** actions, and remains non-startable until explicitly resolved. Recurring and split work remains exact-occurrence/session only and never silently completes its parent or siblings. **Will do later** uses the server's two-phase authoritative Defer flow: Android durably retains the selected target, pauses first, records the exact assessment and any required explicit approval, and submits only the matching move evidence. Lost responses and relaunches resume that journal idempotently. A confirmed or recovered Defer immediately enters the canonical refresh, composition, and publication sequence; the closed source cannot restart, and its replacement cannot become executable until the moved schedule is durably published.

## Persistence safety

Application backup and device transfer are disabled in the manifest; the encrypted database, its WAL files, and the wrapped passphrase are also named in exclusion rules as defense in depth because Android Keystore keys are device-bound. If the wrapping key is unavailable or the snapshot format cannot be decoded, persistence fails closed instead of silently replacing the existing database.

Room schema history is exported under `app/schemas`, and explicit migrations are required—there is no destructive migration fallback.

Timed-break delivery, exact-tap, stale-tap, and Keep-paused receipts add no table
or column: they live inside the existing encrypted singleton JSON payload. That
receipt history advanced the payload to `JSON_V9`. The encrypted local-composition
provenance and scheduling profile advance both the payload and Room schema to
version 10 without adding a table or column. V1–V9 discard injected V10 provenance
or profile authority during upgrade; a V10 payload missing either required field,
or any receipt field introduced by V9, fails closed.

## Next integration gates

- Add an account-facing control if users should be able to opt out of periodic suggestion refresh while retaining API credentials.
- Configure Google OAuth credentials and isolated Calendar/Tasks test resources.
- Complete the Health Connect/Play declaration and target-Pixel physical-device gate using synthetic data only.
- Configure release signing outside version control before producing the direct-download APK.
