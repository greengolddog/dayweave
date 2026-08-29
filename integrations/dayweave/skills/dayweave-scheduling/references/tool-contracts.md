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
  simulation token.
- `submit_proposal`: stores a reviewable Suggestions Inbox entry. It consumes a
  simulation token when available and is the only proposal-writing tool exposed
  to external chats. Its idempotency key is also mirrored by the MCP transport;
  retry the exact content with the exact key after an ambiguous response.

## Proposal invariants

- A proposal is not an accepted change.
- Each submission has an idempotency key and expiry.
- Reusing an idempotency key for different content is an error. Creating a new
  key after response loss can create a duplicate, so ambiguous retries must keep
  both the original key and body unchanged.
- The proposal records source, timestamp, explanation, assumptions, operations,
  and the base revision used for simulation.
- DayWeave revalidates a proposal against current state before acceptance.
- Calendar attendee changes, RSVP actions, deletions, deadline relaxation, and
  broad recomposition still require the app's explicit confirmation screens.
- Tool access and redaction follow the user's per-client permissions.

## Time conventions

Send instants as RFC 3339 with offsets. Preserve each item's absolute-versus-
floating timezone behavior. A date-only deadline must remain date-only.
