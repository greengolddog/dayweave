# Scheduler helper process contract

`dayweave-scheduler-helper` is a one-shot, deterministic process bridge to
`dayweave-core`. It is deliberately dormant: the macOS app does not bundle or
invoke it yet. Shipping the helper requires a separate Swift integration,
bundle-signing, cancellation, and fallback slice.

## Protocol v1

The process accepts no command-line arguments. It reads one UTF-8 JSON value to
EOF on stdin, writes one compact JSON value plus a newline on stdout, and never
writes diagnostics to stderr. It reads no files, environment values, network
resources, clocks, or random sources.

A request has this envelope:

```json
{
  "protocol": "dayweave.scheduler.helper",
  "version": 1,
  "operation": "plan",
  "request": { "as_of": "2026-09-01T07:00:00Z" }
}
```

`request` is the complete `dayweave-core` `PlanRequest`; the shortened value
above only illustrates the envelope. The checked-in synthetic golden fixture
is the executable reference request.

A successful response has a tagged result:

```json
{
  "protocol": "dayweave.scheduler.helper",
  "version": 1,
  "result": { "type": "plan", "plan": {} }
}
```

Every timestamp in a plan is encoded as RFC 3339 text. The bridge never relies
on the core types' default `OffsetDateTime` representation.

A rejected request has a fixed, non-echoing error:

```json
{
  "protocol": "dayweave.scheduler.helper",
  "version": 1,
  "result": {
    "type": "error",
    "error": {
      "code": "invalid_request",
      "message": "Request does not match the scheduler contract."
    }
  }
}
```

Exit code `0` means a plan was produced, `2` means the request was safely
rejected, and `70` means the process encountered an internal or stdout I/O
failure. Error output never includes a supplied field name, value, item title,
identifier, or parser message.

## Acceptance limits

The bridge rejects work before invoking the scheduler when any of these bounds
would be exceeded:

- 16 MiB stdin and 16 MiB stdout, each including its protocol framing; response
  serialization itself stops at the stdout cap;
- 64 nested JSON containers, with duplicate decoded keys rejected at any depth;
- 500,000 decoded JSON values and container entries, plus 16 MiB of decoded
  string data, before schema decoding;
- a positive planning horizon of at most 90 days and minute granularity in
  `1..=60`;
- 10,000 items, availability windows, fixed blocks, or previous assignments;
- 50,000 total previous blocks and 50,000 total constraint-list entries;
- 10,000 recurrence-context entries and 92 resolved calendar days;
- 500 non-control characters in item and fixed-block titles;
- scheduler and soft-constraint weights no greater than 1,000,000;
- 10,000 estimated occurrences and 10,000 estimated materialized items;
- hierarchy depth no greater than 256, 100,000 occurrence-weighted collection
  entries, 16 MiB of occurrence/session-weighted cloned and retained string
  data (including scheduled, fixed, and pinned titles plus context/location
  messages), and 10,000 immutable-overlap violations;
- 128 MiB of estimated candidate-time context/location string formatting; and
- 10,000,000 conservative recurrence, ordering, split-attempt, busy/block scan,
  constraint, and candidate-slot evaluations.

All instants must use microsecond precision. Recurrence references must point to
items in the same request. The estimator accounts for recurring-root subtree
cloning, generated recurrence/routine dependencies, repeated preferred-window
passes, split shrink attempts, and retained per-session messages before
recurrence expansion and scheduler search begin.
Rolling-minute anchors are also bounded before the core alignment loop so a
far-old anchor cannot create an unbounded index catch-up.

## Schema behavior

The envelope must contain exactly `protocol`, `version`, `operation`, and
`request`. The v1 request rejects unknown fields, including fields on Serde's
internally tagged item-kind and split-policy variants. Invalid schema values
map to the single stable `invalid_request` code; classification never examines
or returns a parser's display text.

Other stable codes distinguish encoding, protocol, resource, and scheduler
failures: `request_too_large`, `invalid_utf8`, `invalid_json`,
`duplicate_json_key`, `json_depth_exceeded`, `unsupported_protocol`,
`unsupported_version`, `unsupported_operation`, `resource_limit_exceeded`,
`response_too_large`, `invalid_horizon`, `invalid_granularity`,
`duplicate_item`, `invalid_item`, `invalid_window`, `missing_previous_item`,
`invalid_hierarchy`, `invalid_recurrence`, and `internal_failure`.

## Build boundary

Run:

```sh
scripts/build-macos-scheduler-helper.sh
```

The verifier builds with Rust 1.95.0, `MACOSX_DEPLOYMENT_TARGET=15.0`, and the
`aarch64-apple-darwin` target. It requires a regular arm64 Mach-O, permits only
macOS system dynamic libraries, applies an ad-hoc signature, verifies that
signature, and prints its SHA-256. The binary remains under ignored `target/`
output and must not be staged or uploaded as a source artifact.
