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
# Apple Silicon macOS only:
cargo test --locked --package dayweave-scheduler-helper --all-targets --all-features --target aarch64-apple-darwin
scripts/build-macos-scheduler-helper.sh
swift build --package-path apps/macos
./scripts/test-macos.sh -Xswiftc -warnings-as-errors
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

The script verifies the pinned Codex runtime and final signatures, then writes
both `dist/macos/DayWeave.app` and an integrity-tested
`dist/macos/DayWeave-macOS.zip` with a SHA-256 checksum. Both outputs are
ignored by Git.

Build and verify the dormant local scheduler process bridge with:

```sh
scripts/build-macos-scheduler-helper.sh
```

The script requires an Apple Silicon Mac, Rust 1.95.0, and the
`aarch64-apple-darwin` target. It emits an ad-hoc-signed, macOS 15-compatible
Mach-O under ignored `target/` output and rejects non-system dynamic-library
links. The helper is not copied into `DayWeave.app` or invoked by Swift yet;
this gate establishes the bounded process contract without presenting local
composition as a shipped feature. See the
[scheduler helper process contract](scheduler-helper.md).

The outer app uses an ad-hoc signature because no Apple Developer membership is
in scope. It is suitable for a trusted build made and launched by the same Mac
user, but it is neither notarized nor a stable automatic-update identity. Keep
the app owned by the launching user; a `sudo` copy can make the embedded Codex
runtime fail its owner check. If macOS quarantines a transferred ZIP or app,
first verify its checksum and source, then use **Control-click → Open** (or
**System Settings → Privacy & Security → Open Anyway**) for that exact copy.
Do not broadly remove quarantine metadata. Back up planner data before replacing
an ad-hoc build because Keychain continuity across changing ad-hoc identities is
not yet a release guarantee.

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
metadata before it invokes OpenSSL or `keytool`. The build script repeats this
check before Gradle for its properties file and the referenced keystore,
including linked-worktree Git and common directories; both inputs must remain
regular, single-link, non-symlink files with mode `0600`. The signing
containment regression uses only synthetic temporary files and refuses the
unsafe paths before any Gradle or signing command can run.

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
