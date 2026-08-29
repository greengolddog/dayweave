---
name: dayweave-scheduling
description: "Inspect a user's DayWeave schedule, explain placement and conflicts, simulate plans, and submit simulation-bound typed or advisory Suggestions Inbox proposals for device review. Use when a user asks about their plan, availability, workload, goals, habits, routines, deadlines, or requests any DayWeave change."
---

# DayWeave Scheduling

Use the DayWeave MCP tools as the source of truth for live schedule data. A chat is
an advisory surface: never claim that a proposed change has been applied.

If the DayWeave tools are unavailable or authentication fails, stop before
discussing live state and read [references/connection.md](references/connection.md).
Never ask the user to paste a DayWeave credential into the conversation.

## Safety boundary

- Read schedule data only to answer the user's current request.
- Treat sensitive items as unavailable unless the tool explicitly returns them.
- Do not infer the title, notes, kind, or identity of redacted schedule occupancy.
- Never create, edit, move, complete, delete, or RSVP to an item directly.
- Submit every requested change through `submit_proposal` for review in the app.
- An application-ready proposal is still only a proposal. Only an authorized
  DayWeave device may preview or apply it; never invoke or emulate device-only
  preview, apply, or undo actions.
- Clearly distinguish current state, simulated state, and a submitted proposal.
- Ask before including extra private detail in a proposal explanation.
- Do not weaken deadlines, hard constraints, sleep, or attendee commitments unless
  the user's request explicitly includes that change.

## Workflow

1. Call `get_schedule` for the smallest date range and detail level that answers
   the request. Use `search_items` only when a named item is not in that range.
   If a needed tool is not listed, explain that the connection lacks its scope;
   do not substitute a different tool or claim that the schedule is empty.
2. For explanation requests, call `explain_placement` or `get_conflicts`; do not
   invent optimizer reasons.
3. For a requested change, formulate the operations and assumptions, then call
   `simulate_plan`. Summarize important moves, missed constraints, deadline risk,
   and preserved free time.
4. Refine the simulation conversationally until it matches the user's intent.
   Any change to the operations, assumptions, or base revision invalidates its
   `simulation_token`; simulate the exact final content again.
5. When the user wants the change saved, call `submit_proposal` once with the
   exact final operations, assumptions, base revision, and single-use
   `simulation_token`, plus a concise rationale. Tell the user it is waiting in
   the DayWeave Suggestions Inbox for review, editing, acceptance, or rejection.
6. Interpret actionability only from the submission response. Report a typed
   proposal as application-ready only when `application_ready` is `true` and
   `change_set_schema` is exactly `dayweave.proposal-change-set/1`; otherwise
   describe it as an advisory proposal. Never say that either kind was applied.
7. After an ambiguous submission response, retry only the identical body,
   `simulation_token`, and idempotency key. After a definitive token, revision,
   or content-mismatch error, read current state, simulate again, and make a new
   submission attempt; do not reuse the failed attempt as an ambiguous retry.

## Proposal contents

Include a stable client-generated idempotency key, the exact `simulation_token`,
the affected item identifiers, the requested operations, assumptions, source
conversation label, and an expiry.
Prefer the shortest reasonable expiry and never exceed the server's configured
maximum. If an exact safe expiry cannot be computed, omit it and let DayWeave use
its configured default. A proposal can be application-ready only when the server
accepts a typed `dayweave.proposal-change-set/1` change set. Plans outside that
schema may still be submitted as advisory proposals for review; do not invent
replacement fields or expose redacted content to force application-readiness.

Read [references/tool-contracts.md](references/tool-contracts.md) when constructing
or interpreting a proposal or simulation payload.
