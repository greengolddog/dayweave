# DayWeave MCP contract

The deployed server is authoritative for schemas returned during MCP discovery.
This reference describes semantic invariants the skill must preserve.

Tools are permission-filtered. `schedule:read` exposes the four read tools,
`schedule:simulate` exposes `simulate_plan`, and `suggestions:submit` exposes
`submit_proposal`. A missing tool means its permission was not granted; it does
not mean the corresponding data is absent.

## Read tools

- `get_schedule`: accepts a bounded date/time interval and a requested detail
  level (`busy_only`, `summary`, or `full`). The maximum interval is 90 days.
  Sensitive fields can be redacted by account policy.
- `search_items`: locates items by text, status, kind, project, goal, or date.
- `explain_placement`: returns optimizer reasons, active constraints, alternatives,
  and stability costs for a scheduled block.
- `get_conflicts`: returns hard violations, soft penalties, overload, and fragile
  deadline warnings.

## Planning tools

- `simulate_plan`: evaluates operations against a snapshot without mutation. Its
  response includes moved blocks, unscheduled work, violations, warnings, and a
  `simulation_token`. The token is opaque, single-use, and bound to the exact
  operations, assumptions, and base revision that were simulated.
- `submit_proposal`: stores a reviewable Suggestions Inbox entry and is the only
  proposal-writing tool exposed to external chats. Every request requires the
  exact unexpired `simulation_token` returned for its otherwise identical final
  content. Submission never applies, previews, or undoes canonical changes.

## Submission and retry

Call `simulate_plan` again whenever operations, assumptions, or the base revision
change. Do not modify a simulated payload while retaining its token.

Give each logical submission a stable client-generated idempotency key. After an
ambiguous transport or server response, retry only the exact same request body,
including the same idempotency key and `simulation_token`. Such a replay resolves
through idempotency and does not authorize a second token use. Reusing a token for
a different body or logical submission is an error. After a definitive expired,
consumed, mismatched-token, or stale-revision response, fetch current state,
simulate again, and start a new submission attempt.

## Typed actionability

The current actionable change-set schema is exactly
`dayweave.proposal-change-set/1`. It accepts at most 100 commands. Command IDs and
target item IDs must be unique. Unknown fields are rejected. Its command kinds are:

- `create_item`, carrying the complete server-defined `NewItem` value;
- `replace_item`, carrying the complete server-defined `ReplaceItem` value and a
  positive expected revision;
- `trash_item`, carrying a target item ID and positive expected revision; and
- `restore_item`, carrying a target item ID and positive expected revision.

Use MCP discovery for the submitted operation shape. External callers never
construct change-set commands or guess omitted replacement fields; the server
derives them without exposing private values from redacted schedule data.
Unsupported operations can remain an advisory proposal rather than being
coerced into the actionable schema.

The current simulation compiler can make only these homogeneous requests
application-ready: exactly one `create_item`; one or more `create_event`,
`complete_item`, `delete_item`, or `update_constraint` operations. The server
generates command/item IDs, reads complete canonical items, and derives expected
item revisions. `update_item`, `move_block`, `goal_breakdown`,
`replace_schedule`, mixed operation kinds, and provider-managed targets remain
advisory. A malformed nominally supported operation is rejected and must be
corrected and simulated again; it is not silently downgraded. Treat this matrix
as descriptive only and keep the submission response authoritative.

Every `submit_proposal` response has these stable actionability fields:

- `application_ready` is a boolean. `true` means the stored proposal includes a
  typed change set that an authorized DayWeave device may preview and apply after
  explicit review; it does not mean any canonical state changed.
- `change_set_schema` is a string or `null`. It is exactly
  `dayweave.proposal-change-set/1` when `application_ready` is `true`, and `null`
  when `application_ready` is `false`.

Interpret those output fields as authoritative. Do not infer actionability from
the requested operations, proposal prose, or a simulation result. The plugin
must never call or emulate device-only preview, apply, or undo paths.

## Proposal invariants

- A proposal is not an accepted change.
- Each submission has an idempotency key, exact single-use simulation token, and
  expiry.
- Reusing an idempotency key for different content is an error. Creating a new
  key after response loss can create a duplicate, so ambiguous retries must keep
  both the original key and body unchanged.
- The proposal records source, timestamp, and explanation. Its immutable receipt
  binds it to the complete simulated assumptions, operations, and base revision
  with content-free commitments; advisory payloads also retain that context for
  manual review.
- DayWeave revalidates a proposal against current state before acceptance.
- Calendar attendee changes, RSVP actions, deletions, deadline relaxation, and
  broad recomposition still require the app's explicit confirmation screens.
- Tool access and redaction follow the user's per-client permissions.

## Time conventions

Send supported instants as RFC 3339 with offsets and preserve the supplied
`timezone_name`. The current MCP item contract cannot represent floating times
or date-only deadlines. Ask for a concrete instant instead of silently
converting either form.
