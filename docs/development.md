# DayWeave development guide

## Prerequisites

- Rust 1.95 with `rustfmt` and `clippy`
- Swift 6.2 or newer; full Xcode for UI tests, widgets, entitlements, and
  extension targets
- JDK 17 or newer and the Android SDK for the Android client
- Docker Engine/Compose for the local backend stack
- Codex CLI for embedded App Server development
- Nebius CLI only for deployment work

## Fast verification

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
swift build --package-path apps/macos
swift test --package-path apps/macos
apps/android/gradlew --project-dir apps/android test lint assembleDebug
```

The same checks run remotely with PostgreSQL integration coverage. The
`Security` workflow additionally audits Rust advisories and scans repository
dependencies, configuration, secrets, and the built API container. Do not
silence a finding without documenting why it cannot affect this deployment and
what compensating control remains.

The PostgreSQL repository tests create and drop isolated schemas when
`DAYWEAVE_TEST_DATABASE_URL` is set. Without it they remain compiled and report
an explicit skip. To exercise migrations, transactional item CRUD, workspace
isolation, hierarchy cycle prevention, idempotency, audit/outbox writes, and
delta tombstones locally, point the variable at a disposable PostgreSQL
database before running `cargo test -p dayweave-api --test items_postgres`.

Build the direct macOS bundle with:

```sh
scripts/build-macos-app.sh
open dist/macos/DayWeave.app
```

The output uses an ad-hoc signature because no Apple Developer membership is in
scope. Do not mistake that for notarization.

Create the private Android release key once, outside the repository, then build
the signed direct-install APK with:

```sh
scripts/create-android-signing-key.sh
scripts/build-android-apk.sh
```

The first script refuses to overwrite an existing key and stores its PKCS#12
keystore plus build properties with mode `0600` under the user's configuration
directory. Back up both files securely. The release build disables Gradle's
configuration cache so signing passwords are not retained there, verifies the
APK signature, and writes the ignored artifact to
`dist/android/DayWeave-release.apk`. Signing material and generated binaries
must never be committed.

## Local API

```sh
cp deploy/.env.example deploy/.env
docker compose -f deploy/compose.yaml -f deploy/compose.dev.yaml up --build
```

Replace every placeholder first. PostgreSQL is private to the Compose network;
the API binds to host loopback. The production tunnel is not needed for local
development.

## Commit discipline

Each completed, independently verified slice receives its own conventional
commit. Generated binaries, credentials, OAuth client files, signing keys,
provider tokens, local databases, and deployment state never enter Git.
