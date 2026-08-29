# Pinned Codex App Server contract

DayWeave embeds only a runtime whose executable content, Developer ID team,
version output, and generated App Server schemas match exact constants in the
verifier and the checked-in manifest. A version directory is immutable:
upgrading Codex requires generating a new directory and re-running the
containment and contract tests.

This is a development attestation, not the production launcher. Run it only as
the executable `./scripts/verify-codex-runtime.sh`; invoking it through `bash`
or another interpreter is rejected. Its `#!/bin/bash -p` entry prevents
`BASH_ENV` and exported functions from running before the first verifier line,
then it re-executes itself through a minimal `env -i /bin/bash -p` environment.
Production must instead launch a runtime sealed inside the signed app bundle.

The two combined schema files are the complete legacy and v2 bundles emitted by
the pinned executable with:

```text
codex app-server generate-json-schema --out <empty-directory>
```

The macOS client must fail closed before launch when any pin differs. The
verifier rejects symlinked source path components, verifies the source hash and
Developer ID before executing anything, copies the runtime into a fresh,
mode-0700 DayWeave-owned `CODEX_HOME`, and verifies the copy's inode, hash, and
Developer ID immediately before every contained execution. The copied runtime
itself is read-only to the contained process.

The deny-by-default Seatbelt profile allows writes only below the isolated home,
allows no `process-fork`, and permits `process-exec` only for the pinned runtime
copy. Because child creation needs `process-fork`, App Server tool commands
remain blocked even if a request asks for `dangerFullAccess`. The client clears
the inherited environment, uses file-based credentials inside the isolated
home, disables updates, analytics, agents, and web search, and selects
`never`/`read-only` defaults.

The App Server profile permits outbound network connections for the managed
Codex service. It has no `network-bind` or `network-inbound` permission. Embedded
authentication therefore supports managed ChatGPT device-code login only;
browser callback login must not be offered by this contained runtime.

The containment probe must demonstrate all of the following on the target macOS
release before a pin is enabled:

1. the copied runtime reports the exact pinned version inside the offline
   profile;
2. fresh schema generation inside the offline profile is nonempty, bounded, and
   produces combined bundles byte-identical to both checked-in pins;
3. `initialize` reports macOS and the exact isolated `codexHome`;
4. `account/read` succeeds without reading the user's normal Codex identity;
5. in-home `fs/readFile` and `fs/writeFile` controls succeed, proving the request
   shapes are valid;
6. reads and writes outside the isolated home fail with an OS permission denial;
7. a `command/exec` request for `/bin/cat` is rejected by the outer sandbox even
   when its inner policy requests `dangerFullAccess`; and
8. a `command/exec` request for the pinned runtime copy is also rejected, proving
   that the target cannot create descendants.

The manifest's exact bytes are SHA-256 pinned before JSON parsing, so alternate
encodings and duplicate-key JSON cannot pass through parser normalization.
Every subprocess probe has a wall-clock deadline and byte bounds. JSONL checks
require the exact unique response IDs and expected success or OS-denial shapes.
Each runner and stdin feeder has its own globally registered process group. All
normal and signal exits terminate, bounded-wait, escalate, and reap registered
children and require their groups to be empty before ownership-, mode-,
symlink-, and inode-checked cleanup. The first INT, TERM, HUP, or QUIT status is
latched; launch registration finishes before that status can initiate shutdown,
and further catchable lifecycle signals are ignored until cleanup completes.
SIGKILL cannot be trapped.

At registration, the verifier requires the direct child PID to be its process
group ID. That ID remains registered while either the child or a descendant is
present and is retired immediately after the group is observed empty, limiting
PID-reuse exposure. Deliberate PID-namespace racing by an already-compromised
same-user process remains part of the documented same-user threat limit.

The runtime's Homebrew source and the app run as the same macOS user. Copying,
rechecking, and executing a private inode closes ordinary replacement windows,
but Unix ownership cannot protect against an already-compromised concurrent
process with the same user identity. Production packaging must seal the runtime
as a signed app resource, and this same-user limitation remains part of the
local-device threat model.

`sandbox-exec` and the system profile imports are macOS private compatibility
surfaces. If Apple removes or changes them, DayWeave treats Codex as unavailable
instead of falling back to an uncontained process.
