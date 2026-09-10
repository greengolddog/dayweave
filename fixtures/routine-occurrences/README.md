# Routine occurrence wire fixtures, version 1

`wire-v1.json` contains synthetic, public test data only:

```text
{schema_version: 1, valid: [{name, kind, value}], invalid: [{name, kind, value}]}
```

The exact kinds are `snapshot`, `command`, `mutation`, and `page`. Discover
cases dynamically; names, not array positions or a fixed case count, identify
specific cross-message examples. Never submit these values to an owner's service.

## Real producer and portable admission

`server/dayweave-api/tests/routine_occurrence_contract.rs` generates the valid
values using the public recurrence expander, occurrence initialization, reviewed
commands, and whole-instance snapshot planner. The synthetic tree is a recurring
Routine (`...000001`), a structural Task (`...000002`), its required leaf
(`...000003`), and an optional sibling leaf (`...000004`). The Routine's exact
initial reopening state is Blocked with the manual reason “Synthetic waiting for
input”. Definitions and runtime states are separate authorities.

The ledger instance ID is `00000000-0000-0000-0000-000000000064`. It is the HTTP
instance route identity, **not** the independently generated planner occurrence
UUID-v5. Calendar and rolling identity examples use the real recurrence
expander. The clipped and disjoint Move examples preserve that expander's nominal
identity independently of its effective window. The repeated definition hash is
a synthetic repository-supplied semantic hash; snapshot evidence hashes are
actually computed by the domain. Neither value is a client-recomputable proof of
the current canonical source forest.

Production typed command/aggregate validation is the semantic oracle. The test
also checks portable UTC spelling, snapshot identity/evaluation completeness,
exact derived counts and qualification, mutation shape, and page limits/order.
Plain response deserialization does not establish these invariants by itself.
All valid cases must pass both native strict validators and this Rust admission
helper. Malformed cases exercise individual shape, topology, revision, policy,
identity, timestamp, and evaluation failures.

## Closed shapes and invariants

- Snapshot: exactly `schema_version`, `aggregate`, `evidence_hash`,
  `fresh_edit_eligible`, and `members` (evaluations).
- Aggregate: exactly `manifest`, `revision`, and `members` (runtime states).
- Manifest: exactly `schema_version`, `id`, `series_item_id`, `occurrence_id`,
  `identity`, `nominal_start`, `nominal_end`, `window_start`, `window_end`,
  `timezone_name`, `definition_hash`, and `members` (immutable definitions).
- Definition: exactly `item_id`, `parent_id`, `source_revision`, `title`, `kind`,
  `recurs`, `sibling_order`, `required_for_parent`, and `initial_open`.
- Runtime member: exactly `item_id`, `revision`, `status`,
  `required_for_parent`, `mode`, `open`, `provenance`, `completed_at`, and
  `updated_at`.
- Evaluation: exactly `item_id`, `counts`, `occurrence_evidence_required`, and
  `reason`. Counts have exactly `required_descendants`, `completed`,
  `incomplete`, and `occurrence_evidence_required`.
- Command: exactly `schema_version`, `operation_id`,
  `expected_instance_revision`, `expected_member_revision`,
  `expected_evidence_hash`, and `action`.
- Mutation: exactly `operation_id`, `replayed`, and `occurrence` (snapshot).
- Page: exactly `schema_version`, `changes`, `cursor`, and `has_more`. Each
  change has exactly `sequence` and `occurrence` (snapshot).

Every nullable key is required: definition `parent_id`, member `provenance` and
`completed_at`, and all three blocker fields of every reopening value. Reopening
has exactly `status`, `blocked_reason_kind`, `blocked_by_item_id`, and
`blocked_reason`; provenance has exactly `kind` and `reopen`.

All IDs are non-nil. Revisions are positive JSON integers bounded by signed
64-bit storage; member revision cannot exceed aggregate revision. The complete
acyclic tree has exactly one root, all parents are present, and definition,
runtime, and evaluation identity sets match exactly. The root is a recurring
Task or Routine, never a Habit. Every count is bounded by the complete tree and
checked arithmetic enforces:

```text
required_descendants = completed + incomplete + occurrence_evidence_required
```

Counts include all required descendants, not just leaves. An optional edge
excludes its entire branch from ancestor requirements without completing or
cancelling that branch. Nested independently recurring members and their
descendants remain unqualified in the outer occurrence. The per-member
qualification boolean is independent of that member's descendant counts.

Only Inbox, Planned, and Blocked are explicit reopening states. Non-blocked open
states have three null blocker fields. A dependency blocker requires a non-nil
other item ID; manual/external blockers require a trimmed nonempty reason and no
blocker ID. Reasons have at most 1,000 Unicode scalars and no controls. Titles
are trimmed, nonempty, control-free, and at most 500 Unicode scalars.

Completed members require `completed_at`; all other statuses require it to be
null. Repeated Done and policy reviews preserve an existing completion anchor.
Leaves use Automatic and no provenance. A Completed parent requires qualified
Automatic or Manual provenance, matching its mode and exact retained reopening
tuple. Skipped and Cancelled do not mean Done. An execution-owned member status
is not an occurrence outcome. Definition `required_for_parent` and
`initial_open` are immutable initial values: do not require equality with their
runtime counterparts after a reviewed change. Do not infer initial defaults
solely from a standalone revision number.

Manifest and runtime timestamps use valid four-digit civil years and UTC
RFC3339 (`Z` or `+00:00`) with zero to six fractional digits. Revision ordering
does not imply wall-clock ordering. Identity anchors follow the core's separate
RFC3339 offset contract (including offsets up to ±23:59), still with portable
microsecond precision. A nominal interval and an effective window must each be
positive; neither must contain the other. Identity validation uses nominal
local date/timezone, not the moved/clipped effective window.

The reason enum is closed, but a mutation may retain its transition reason
while a later GET uses the current evaluation's reason. Do not reject a valid
mutation merely because its reason differs from a newly evaluated GET reason.

## Request, replay, and page context

Standalone command validation uses target member
`00000000-0000-0000-0000-000000000003`, particularly for self-dependency
rejection. Contextual command/mutation pairs instead use these explicit routes:

- `parent_*`: member `...000002`.
- `leaf_required_edge_changed`: member `...000004`.
- All other paired transitions: member `...000003`.

`required_done_command`, `required_done_mutation`, and
`required_done_historical_replay` bind the same operation and instance, with
aggregate and target-member revisions 1→2. Only the historical wrapper's
`replayed` value differs. Later snapshots have greater aggregate revisions and
different review hashes; this never invalidates exact receipt custody. Contextual
transport tests must separately check route, operation, both checked `+1` CAS
relationships, reviewed action/result, and the single matching
`Idempotency-Replayed` header. Standalone well-formed old receipts are not
invalid simply because a newer observation exists. A receipt is never fresh GET
permission or authority to overwrite newer cache state.

Changed definitions and missing sources remain readable with
`fresh_edit_eligible: false`; harmless current-source revision changes can remain
eligible after a fresh review. A full snapshot exposes no current-source
revision map. Its opaque hash must not be substituted for a local planning
witness. All occurrence-derived content is private.

Pages contain at most 100 whole snapshots within 8 MiB, with positive increasing
change sequences. The generic delta fixture deliberately includes successive
revisions of the same instance; the older delta entry is ineligible for fresh
review, while its original historical mutation receipt remains unchanged.
A current-list transport additionally rejects
repeated instance IDs. Intermediate pages must not install partial state or
become delta checkpoints. Cursor contents are opaque to clients; the synthetic
examples use the server's encoding shape but do not prove any real workspace
authority. Cursors contain 1–512 ASCII graphic bytes (33–126). A nonterminal page
cannot be empty, and any nonempty page must advance the request cursor, including
the terminal page. An empty terminal response may retain its checkpoint.

Raw duplicate object keys cannot be represented in this JSON corpus. Rust and
native raw-byte tests must reject them before dictionary conversion, and must
reject fractional/exponent spellings of integer fields. Native fixture readers
must pass exact value bytes to admission rather than normalize number spellings
through a platform JSON parse/re-encode. Whole-snapshot byte
limits count UTF-8 bytes, not characters or UTF-16 units. The Rust suite generates
a 5,001-level tree and oversize member/UTF-8 payload cases in memory instead of
checking in huge fixtures.

## Regeneration

The ignored maintenance test prints one `ROUTINE_OCCURRENCE_FIXTURE=` JSON line
to stdout and never writes files:

```sh
cargo test -p dayweave-api --test routine_occurrence_contract \
  emit_shared_routine_occurrence_fixture -- --ignored --nocapture
```

After reviewing an intentional contract change, install that emitted JSON in
`wire-v1.json`. The ordinary contract test independently regenerates the corpus
and requires exact equality; ordinary CI does not depend on running the ignored
maintenance generator. No live database, native app, account, or network is used.
