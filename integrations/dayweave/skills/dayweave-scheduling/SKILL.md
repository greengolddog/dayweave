---
name: dayweave-scheduling
description: "Inspect a user's DayWeave schedule, explain placement and conflicts, run what-if planning, and draft changes as reviewable Suggestions Inbox proposals. Use when a user asks about their plan, availability, workload, goals, habits, routines, deadlines, or requests any DayWeave change."
---

# DayWeave Scheduling

Use the DayWeave MCP tools as the source of truth for live schedule data. A chat is
an advisory surface: never claim that a proposed change has been applied.

## Safety boundary

- Read schedule data only to answer the user's current request.
- Treat sensitive items as unavailable unless the tool explicitly returns them.
- Never create, edit, move, complete, delete, or RSVP to an item directly.
- Submit every requested change through `submit_proposal` for review in the app.
- Clearly distinguish current state, simulated state, and a submitted proposal.
- Ask before including extra private detail in a proposal explanation.
- Do not weaken deadlines, hard constraints, sleep, or attendee commitments unless
  the user's request explicitly includes that change.

## Workflow

1. Call `get_schedule` for the smallest date range and detail level that answers
   the request. Use `search_items` only when a named item is not in that range.
2. For explanation requests, call `explain_placement` or `get_conflicts`; do not
   invent optimizer reasons.
3. For planning requests, call `simulate_plan` first. Summarize important moves,
   missed constraints, deadline risk, and preserved free time.
4. Refine the simulation conversationally until it matches the user's intent.
5. When the user wants the change saved, call `submit_proposal` once with the
   final proposal and a concise rationale. Tell the user it is waiting in the
   DayWeave Suggestions Inbox for review, editing, acceptance, or rejection.

## Proposal contents

Include a stable client-generated idempotency key, the affected item identifiers,
the requested operations, assumptions, source conversation label, and an expiry.
Prefer the shortest reasonable expiry and never exceed the server's configured
maximum. Proposal operations may cover tasks, events, habits, routines, goals,
constraints, dependencies, or complete schedule alternatives.

Read [references/tool-contracts.md](references/tool-contracts.md) when constructing
or interpreting a proposal or simulation payload.
