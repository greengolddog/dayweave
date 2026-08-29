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
python3 -B scripts/test-repository-credential-scanner.py
python3 -B scripts/scan-repository-credentials.py all
scripts/test-create-android-signing-key.sh
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
must never be committed. The generator rejects both lexically in-worktree paths
and external paths that resolve through a symlink into the worktree or its Git
metadata before it invokes OpenSSL or `keytool`.

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

The checked-in credential scanner reads staged index blobs and Git objects
directly. It never opens worktree paths, follows symlinks, or reads ignored or
untracked files. Install its defense-in-depth pre-commit and pre-push hooks for
this worktree explicitly with:

```sh
scripts/install-git-hooks.sh
```

The installer is idempotent and refuses to replace an existing custom
`core.hooksPath`; it is never run automatically. The pre-push hook scans the
complete closure of every non-delete local ref, including earlier commits whose
files were later removed. A hook can be bypassed with Git's `--no-verify`, so
the `Security` workflow independently scans every reachable blob, raw commit
and annotated-tag object, ref name, and historical tree path from a
non-shallow checkout. Findings expose only rule IDs, abbreviated object IDs,
counts, and path/ref fingerprints—never matched content.
