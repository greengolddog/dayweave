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

A request has exactly four envelope fields. Protocol v1 supports the unchanged
`plan` operation and the local canonical-snapshot `compose` operation.

### Plan operation

The existing direct-planning request remains:

```json
{
  "protocol": "dayweave.scheduler.helper",
  "version": 1,
  "operation": "plan",
  "request": { "as_of": "2026-09-01T07:00:00Z" }
}
```

For `operation: "plan"`, `request` is the complete `dayweave-core`
`PlanRequest`; the shortened value above only illustrates the envelope. The
checked-in synthetic golden fixtures remain the byte-for-byte executable
reference request and response.

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

### Compose operation

The `compose` operation prepares a canonical item snapshot and a scheduling
request locally, applies the normal core preflight limits to the resulting
`PlanRequest`, and then runs the same scheduler as `plan`:

```json
{
  "protocol": "dayweave.scheduler.helper",
  "version": 1,
  "operation": "compose",
  "request": {
    "canonical_items": [],
    "schedule": {
      "as_of": "2026-09-01T07:00:00Z",
      "horizon_start": "2026-09-01T07:00:00Z",
      "horizon_end": "2026-09-02T07:00:00Z",
      "timezone_name": "UTC"
    }
  }
}
```

`request` contains exactly `canonical_items` and `schedule`.
`canonical_items` must be the caller's complete active, nondeleted canonical
snapshot, not a subset prefiltered to items that appear schedulable. `schedule`
is the complete `dayweave-compose` `ComposeScheduleRequest`; omitted collection
and configuration fields use that type's documented defaults.

A successful composition has its own result variant and is not a server
schedule preview:

```json
{
  "protocol": "dayweave.scheduler.helper",
  "version": 1,
  "result": {
    "type": "composition",
    "composition": {
      "local_input_fingerprint": "local-sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "source_item_count": 1,
      "source_item_revisions": {
        "00000000-0000-0000-0000-000000000001": 1
      },
      "accepted_item_count": 1,
      "rejected_items": [],
      "ignored_previous_assignments": [],
      "plan": {}
    }
  }
}
```

Those are the seven composition fields. `source_item_count` and
`source_item_revisions` describe the complete supplied snapshot, including
Inbox, context-only, and rejected items. `accepted_item_count` includes items
accepted without schedulable work. Rejected items and ignored previous
assignments retain the preparation diagnostics needed by the caller. Internal
effective-sensitivity bookkeeping is not exposed.

The helper computes `local_input_fingerprint` only after composition and core
preflight succeed. It SHA-256 hashes a deterministic serialization of:

- the domain string
  `dayweave.scheduler-helper.local-composition.v1`;
- the planning timezone name;
- the complete, UUID-sorted source item revision map; and
- the normalized prepared `PlanRequest`.

Consequently JSON whitespace, object-key order, canonical-item input order,
and equivalent timestamp spellings do not affect it, while a source revision,
timezone, or normalized planning input does. The wire value is
`local-sha256:` followed by 64 lowercase hexadecimal characters.

This fingerprint is intentionally non-publishable. It is not an
`input_digest`, uses a distinct domain and prefix, and the server's `sha256:`
digest parser does not accept it. Local composition also has no authoritative
Calendar projection generation fence and does not perform the server's durable
publication-persistability validation. A caller may use the composition under
its local display or execution policy, but must never use its fingerprint to
authorize publication. The helper cannot establish that a caller's snapshot
is complete or current; that is an integration responsibility.

### Errors and exit status

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

Exit code `0` means a plan or composition was produced, `2` means the request
was safely rejected, and `70` means the process encountered an internal or
stdout I/O failure. Error output never includes a supplied field name, value,
item title, identifier, or parser message.

Canonical preparation errors have fixed mappings and are classified by their
variants, never by matching their display text:

| Preparation failure | Helper code | Exit code |
| --- | --- | ---: |
| `InvalidRequest(_)` | `invalid_request` | 2 |
| `TooManyItems` | `resource_limit_exceeded` | 2 |
| `DuplicateCanonicalItem(_)` | `duplicate_item` | 2 |
| `InvalidCanonicalItem(_)` | `invalid_item` | 2 |
| `AccountingOverflow` | `internal_failure` | 70 |

The fixed error messages remain non-echoing even when the underlying
preparation error contains an item UUID or caller-supplied text. Existing
direct-planning preflight and scheduler error mappings are unchanged.

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

For `compose`, canonical preparation first enforces its snapshot and accounting
bounds. The helper then applies all of the core limits above to the normalized
`PlanRequest`; preparation does not replace hierarchy, precision, recurrence,
materialization, candidate-scan, or retained-string preflight. The final
composition response, including its metadata and trailing newline, remains
subject to the 16 MiB stdout limit. If it cannot fit, the helper discards the
partial response and emits the fixed `response_too_large` error.

All instants must use microsecond precision. Recurrence references must point to
items in the same request. The estimator accounts for recurring-root subtree
cloning, generated recurrence/routine dependencies, repeated preferred-window
passes, split shrink attempts, and retained per-session messages before
recurrence expansion and scheduler search begin.
Rolling-minute anchors are also bounded before the core alignment loop so a
far-old anchor cannot create an unbounded index catch-up.

## Schema behavior

The envelope must contain exactly `protocol`, `version`, `operation`, and
`request`. Each operation has a distinct strict request shape: a `plan` request
is not accepted by `compose`, and a `compose` request is not accepted by
`plan`. Unknown fields are rejected in the compose wrapper, schedule,
canonical-item DTOs, and internally tagged item-kind and split-policy variants.
Arbitrary JSON remains arbitrary only in canonical contract fields designed to
carry it. Invalid schema values map to the single stable `invalid_request`
code; classification never examines or returns a parser's display text.

Other stable codes distinguish encoding, protocol, resource, and scheduler
failures: `request_too_large`, `invalid_utf8`, `invalid_json`,
`duplicate_json_key`, `json_depth_exceeded`, `unsupported_protocol`,
`unsupported_version`, `unsupported_operation`, `resource_limit_exceeded`,
`response_too_large`, `invalid_horizon`, `invalid_granularity`,
`duplicate_item`, `invalid_item`, `invalid_window`, `missing_previous_item`,
`invalid_hierarchy`, `invalid_recurrence`, and `internal_failure`.

## Sensitive data handling

Stdin can contain canonical titles, notes, constraints, calendar blocks, and
other sensitive scheduling data. A successful composition's stdout can contain
titles and diagnostic text in addition to the produced plan. Treat both streams
as sensitive: do not place them in logs, crash reports, command-line arguments,
or environment values. Error responses are deliberately fixed and non-echoing,
but a successful response is not sanitized.

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
