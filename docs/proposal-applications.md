# Transactional AI proposal applications

Status: implemented PostgreSQL contract
Schema: `dayweave.proposal-change-set/1`

DayWeave treats AI output as a proposal, never as authority to mutate canonical
items. A durable device client may review a strictly typed proposal change set,
approve the exact review it saw, inspect the resulting receipt, and request one
bounded undo. External MCP clients remain proposal-only.

## Simulation-backed MCP bridge

An external assistant cannot author this executable payload directly. Before
every submission it must simulate the exact final revision, operations, and
assumptions and return the opaque single-use token. While holding the canonical
item lock, DayWeave derives item revisions and complete replacements from its
own state. Supported homogeneous operations become hidden typed evidence;
unsupported, mixed, sensitive, or provider-managed work never becomes a partial
change set.

Submission atomically consumes that evidence and either stores its exact typed
payload or a deliberately non-executable manual-review payload. The MCP result
reports `application_ready` and `change_set_schema`, but exposes neither commands
nor any preview/apply/undo capability. An immutable receipt preserves the
compilation outcome plus hashes of the complete request, hidden evidence, and
submitted payload after the short-lived simulation row is removed. Device preview still
revalidates all canonical and provider state before any apply.

## Executable change-set contract

An executable proposal payload has exactly this top-level shape:

```json
{
  "schema": "dayweave.proposal-change-set/1",
  "commands": [
    {
      "operation": "trash_item",
      "command_id": "11111111-1111-4111-8111-111111111111",
      "item_id": "22222222-2222-4222-8222-222222222222",
      "expected_revision": 7
    }
  ]
}
```

The supported commands are `create_item`, `replace_item`, `trash_item`, and
`restore_item`. Create carries a complete `NewItem`; replace carries a complete
`ReplaceItem`. The generated OpenAPI document at `/openapi.json` is the source
of truth for those item fields.

The parser rejects unknown fields, empty or oversized command lists, duplicate
command IDs, duplicate target item IDs, and zero expected revisions. One
change set and one combined review contain at most 100 commands. Proposal kind
also constrains its commands:

- `create_item` is exactly one create;
- `goal_breakdown` contains only creates;
- `calendar_event` contains only event creates;
- `update_item` and `constraint_change` contain only replace, trash, or restore;
- `schedule_plan` and `recommendation` are not executable item change sets.

Every selected proposal must still be pending, unexpired, and at the requested
revision. Merely using a reserved schema name does not make a payload
executable: only the complete, currently supported typed contract is accepted.

## REST workflow

These routes require PostgreSQL, an owner-scoped durable device principal, and
the listed scopes. Legacy static principals and both native and OAuth MCP
principals cannot use them.

| Operation | Route | Required scopes |
| --- | --- | --- |
| Preview | `POST /v1/suggestions/application-previews` | `suggestions_read`, `suggestions_write`, `items_read` |
| Apply | `POST /v1/suggestions/application-previews/{id}/apply` | `suggestions_write`, `items_write` |
| Get | `GET /v1/suggestions/applications/{id}` | `suggestions_read`, `items_read` |
| Get by proposal | `GET /v1/suggestions/{id}/application` | `suggestions_read`, `items_read` |
| Undo | `POST /v1/suggestions/applications/{id}/undo` | `suggestions_write`, `items_write` |

### Preview

The preview request selects one to twenty whole proposals and supplies the
expected revision of each:

```json
{
  "proposals": [
    {
      "proposal_id": "33333333-3333-4333-8333-333333333333",
      "expected_revision": 4
    }
  ]
}
```

Commands cannot be cherry-picked. DayWeave simulates the complete group in
order, then rolls the simulation back. The response shows direct diffs,
implicit hierarchy/parent diffs, conflicts, risks, the maximum risk, whether
explicit approval is required, and `can_apply`.

The returned `review_hash` binds the ordered proposal IDs, revisions and
payload hashes; normalized command hash; visible review content; `can_apply`;
workspace and user; canonical item revisions/deletion state; active
provider-managed item set; and preview lifetime. Command content is not copied
into preview storage. A preview expires after at most 15 minutes, or sooner when
one of its proposals expires. Any bound state change requires a fresh preview.
At most 100 active previews are retained for the owner at once.

A preview may return successfully with conflicts and `can_apply: false`; it is
review evidence, not permission to apply. Such a preview can never produce an
application.

### Apply

Apply must echo the exact hash shown during review:

```json
{
  "expected_review_hash": "sha256:replace-with-the-reviewed-64-hex-digest"
}
```

The server locks the workspace mutation boundary, reconstructs commands from
the current proposal payloads, rechecks every proposal/member/hash and the
canonical/provider digest, then executes all commands in one transaction. It
also records direct effects, every direct or implicit affected-item fence,
proposal acceptance, content-free audit/outbox evidence, and the idempotency
receipt in that transaction.

Either every command and every proposal commits, or none does. There is no
partial application, no per-command acceptance, and only one application may
exist for a preview. A stale review, changed item, hierarchy failure, provider
boundary, or late command failure rolls the entire transaction back.

### Get

A successful apply returns a durable, content-free receipt. It contains the
application ID and revision, `applied` or `undone` status, applied proposal
revisions, command IDs, all directly and implicitly affected item IDs,
application time, undo deadline, and optional undo time. The same receipt is
available by application ID or by any member proposal ID.

The application state is monotonic: revision 1 is `applied`, and revision 2 is
`undone`. Undo reverses canonical item effects but does not reopen the accepted
proposals or erase their decision history.

### Undo

Undo supplies the exact current application revision:

```json
{
  "expected_application_revision": 1
}
```

The undo window is 24 hours from apply. Before changing anything, the server
requires the application to remain at revision 1 and every affected-item fence
to match the exact applied revision and deletion state. Any later direct or
implicit change fails the whole undo with a conflict. A newly provider-managed
affected item also blocks undo rather than silently crossing the integration
boundary.

Inverse commands run in reverse order. Undo trashes an item created by the
proposal; replace, trash, and restore use the retained before-snapshot to
recover the prior semantic item state, including completion and deletion
timestamps. Canonical revisions and audit timestamps advance rather than
rewinding history. The item changes, derived parent refreshes, fences, audit,
outbox, idempotency receipt, and `applied` to `undone` transition commit
together.

## Provider-managed boundary

An active provider mapping for an item or expanded calendar occurrence makes
that item non-mutable by an AI proposal. Preview reports a
`provider_managed_item` conflict for either a direct target or an implicitly
affected parent. Apply repeats the check over every item changed by the actual
transaction, and undo checks every fence.

This is deliberately stricter than ordinary user-driven integration flows.
Google-owned state must be changed through the provider-aware command and sync
path, where remote versions, ownership, confirmation, and outbox delivery are
available.

## Idempotency and concurrency

Apply and undo each require an `Idempotency-Key` of 8 to 128 URL-safe ASCII
characters and use independent operation namespaces. Raw keys are not stored;
operation-scoped SHA-256 hashes and request hashes are retained. An exact retry
returns the existing receipt with `replayed: true`. Reusing a key for different
content returns a conflict.

Expected proposal, item, application, review, and fence revisions are all
optimistic concurrency boundaries. A workspace advisory lock serializes
canonical item mutation, application, undo, and provider-mapping changes so a
successful response cannot represent a partial interleaving. Expiry decisions
sample PostgreSQL's wall clock after the relevant locks are acquired, keeping
all API nodes and database mutation guards on one time authority.

## Retention, scrubbing, and provenance

Before/after item snapshots exist only in the restricted application-effect
table for exact undo. They are hashed, are never copied into ordinary receipts,
audit metadata, outbox payloads, or idempotency rows, and cannot be modified.
After the undo deadline, proposal maintenance scrubs snapshot content in a
one-way database transition. Maintenance runs during API startup, every hour,
and opportunistically when proposal previews are created. Snapshot hashes,
command hashes, revisions, fences, application state, and content-free audit
evidence remain.

Expired unapplied previews are eligible for deletion, while a preview linked
to an application is retained as immutable approval evidence. Scrubbing a live
database row does not retroactively remove bytes from retained backups or WAL;
their separate retention policy still applies.

Each proposal retains its submitting subject, trusted source classification,
optional source reference, kind, explanation, expiry, and decision history.
Device REST submissions are classified as `app_assistant` regardless of a
caller-supplied source hint. MCP submissions are classified as `external_mcp`
and bind the authenticated subject plus their bounded conversation reference.
Application audit evidence additionally identifies the applying device
credential without copying proposal or item content into general logs.

## Legacy and MCP behavior

Generic legacy proposal objects and advisory MCP schedule-operation payloads do
not match `dayweave.proposal-change-set/1`; the application preview rejects them
as non-executable. For the supported homogeneous MCP subset, the server may
compile the hidden simulation evidence directly into that typed schema. The
legacy accept route may mark an ordinary proposal accepted, but that status
change does not execute payload instructions or mutate items. Payloads in the
reserved `dayweave.proposal-change-set/*` namespace cannot use legacy acceptance
and must pass the transactional application path.

MCP tools can read granted schedule data, simulate, and submit an expiring
Inbox proposal. They expose no preview, apply, or undo tool and cannot
authenticate to the device-only application routes. An application-ready MCP
suggestion already contains the exact server-derived typed payload; the
user-facing device must still preview and explicitly approve it through the
complete workflow above. Advisory suggestions remain non-executable until a
separate, explicitly reviewed proposal is authored.
