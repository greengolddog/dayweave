# DayWeave product requirements

Version: 0.1 (build baseline)  
Status: accepted  
Source of truth: [discovery-answers.md](discovery-answers.md)

## 1. Product statement

DayWeave is an AI-native personal scheduling system that converts fixed events, flexible tasks, habits, routines, goals, breaks, availability, and real-world context into an executable day. It continuously adapts the plan while keeping the user in control and keeping Google Calendar and Google Tasks consistent.

The product is complete only when it is safe and pleasant enough to run the owner's real schedule for seven consecutive days. A prototype, a chat wrapper, a static calendar, or a scheduler that requires an online language model does not satisfy this specification.

## 2. Scope and release order

- One personal user is the initial deployment scope. The data model and authorization boundaries must not prevent later multi-user support.
- macOS is the first complete client. Android follows with functional parity except where a capability is inherently platform-specific.
- The initial target devices are an Apple M4 Mac on macOS 26.3 (25D125) and a Pixel 11 whose Android version will be detected during device setup.
- The initial installation path is a local macOS build and a directly installed signed Android APK. App Store notarization and Google Play distribution are not release blockers.
- English is the initial product language.
- WHOOP is a planned extension and is not in the completion gate. Android Health Connect is in scope.

## 3. Product principles

1. **Deterministic core, intelligent interface.** Planning must work locally without an LLM. AI translates, estimates, explains, discusses, and proposes.
2. **Control is visible.** Every material movement has a reason, constraints are inspectable, external effects are previewed, and changes can be undone.
3. **The plan survives reality.** Pauses, interruptions, early or late finishes, travel, calendar changes, and offline work are normal states.
4. **Privacy follows the item.** Sensitive content stays protected in notifications, integrations, assistant context, search surfaces, and diagnostics.
5. **One coherent schedule.** Events, goals, tasks, routines, habits, breaks, and free time share one constraint model rather than competing subsystems.
6. **Native clients, shared behavior.** macOS and Android use native UI conventions while presenting the same concepts and decisions.

## 4. Functional requirements

Requirement IDs are stable. “Must” and “shall” are acceptance requirements.

### 4.1 Identity and accounts

- **ID-001** DayWeave shall support one allowlisted Google user in the initial deployment and store provider account identifiers separately from internal user IDs.
- **ID-002** The schema shall permit multiple Google accounts and per-account calendar/task-list selection even if onboarding initially connects one account.
- **ID-003** Codex/ChatGPT authentication shall remain distinct from Google identity.
- **ID-004** Embedded Codex shall use the supported Codex App Server managed browser/device-code login when available, with an API-key fallback.
- **ID-005** The user shall be able to view active DayWeave sessions, revoke any remote session, disconnect/reconnect integrations, and delete the account.
- **ID-006** Google reauthentication shall be supported without losing local work; a recovery code shall provide an account-recovery path for the private deployment.

### 4.2 Common item model

- **DOM-001** The domain shall represent calendar events, tasks, habits, routines, breaks, goals, and projects as distinct item kinds sharing stable IDs, timestamps, revisions, ownership, audit metadata, and optional privacy flags.
- **DOM-002** Supported statuses shall be not started, scheduled, active, paused, completed, skipped, canceled, and blocked.
- **DOM-003** Items shall support notes, URLs, attachments, locations, participants/people, conferencing data, tags, contexts, and source provenance where applicable.
- **DOM-004** Progress shall support percentage, elapsed/remaining time, and arbitrary named quantitative units.
- **DOM-005** A task shall support exact, ranged, or unknown duration. Unknown duration may receive an assistant estimate that remains visibly editable.
- **DOM-006** A deadline shall support date-only or exact date-time and independently be hard or soft.
- **DOM-007** Priority shall store importance and urgency components and expose a numeric default score derived from importance × urgency.
- **DOM-008** Energy/focus demand shall support low, medium, and deep-focus values plus unknown.
- **DOM-009** Items shall support templates. Templates shall cover every item kind and complete weekly setups.
- **DOM-010** Bulk operations shall include scheduling, status, tags, move, delete, constraints, and export where meaningful.
- **DOM-011** Deletion shall move records to a recoverable trash for thirty days. Completed and canceled records shall remain searchable without automatic expiry.
- **DOM-012** Every mutation shall produce an audit entry with actor/source, timestamp, previous value or inverse operation, and correlation ID.
- **DOM-013** A task may have no deadline and may optionally recur; recurring tasks remain distinct from habits and retain task completion semantics.
- **DOM-014** The absence of duration, deadline, priority, energy, or scheduling restrictions shall be represented explicitly and shall not prevent Inbox capture.

### 4.3 Hierarchy, goals, projects, routines, and dependencies

- **HIE-001** Projects, tasks, and subtasks shall allow unlimited logical nesting with cycle prevention.
- **HIE-002** Only leaf execution components shall create flexible schedule demand. Tasks are executable by default; a leaf project, goal, or routine requires an explicit independent-effort component. Fixed events retain their own interval, and parent duration/progress roll-up shall never duplicate descendant demand.
- **HIE-003** A parent may carry an explicit independent progress component in addition to child roll-up.
- **HIE-004** A parent shall auto-complete when all required descendants complete, unless a user-visible manual override is active.
- **HIE-005** Dependencies shall support finish-to-start, start-to-start, finish-to-finish, and start-to-finish relations, each with a nonnegative lag and hard or soft strength.
- **HIE-006** A blocked item shall display every known dependency or other cause that blocks it.
- **HIE-007** A dependency into a materialized recurring subtree shall be valid only from within that same subtree, where both endpoints receive occurrence-local identities; external and cross-series references shall be rejected.
- **GOAL-001** A goal may be an unscheduled outcome with no duration. Every executable action required for the goal shall be represented by one or more leaf tasks and calendar blocks.
- **GOAL-002** Goals shall support optional target dates, priority, milestones, notes, and multiple named measures.
- **GOAL-003** A goal shall support minimum and maximum weekly time allocation.
- **GOAL-004** Goals, projects, tasks, routines, and habits shall be linkable without forcing them into the same lifecycle.
- **ROU-001** A routine shall be an ordered set of independently schedulable steps.
- **ROU-002** A routine shall recur by calendar rule or by elapsed time after completion.
- **ROU-003** An overdue routine step shall expose at least **Skipped** and **Will do later** actions.

### 4.4 Scheduling constraints

- **CON-001** The app shall provide editable availability, sleep, work/free-time, travel, and other schedule profiles.
- **CON-002** Every schedulable item shall support earliest start, latest finish, allowed and forbidden weekdays, preferred and forbidden windows, location, context, advance notice, dependencies, and daily/weekly occurrence limits where meaningful.
- **CON-003** Every applicable restriction shall be independently classifiable as hard or soft.
- **CON-004** Tasks shall be non-splittable by default.
- **CON-005** A splittable task shall support minimum and maximum session duration, maximum number of sessions, minimum spacing, eligible days, setup cost, and ordered/unordered execution.
- **CON-006** Recording partial work shall reduce remaining demand and preserve actual elapsed time.
- **CON-007** Preparation, travel, decompression, and context-switch buffers shall be configurable globally, by profile, and per item.
- **CON-008** Protected free time, meals, sleep, and breaks shall be represented as visible constraints rather than unlabelled gaps.
- **CON-009** The user may manually violate a constraint only after the app explains the conflict and records the acknowledged override.
- **CON-010** Pinning shall keep a chosen block stable until the user unpins it or approves a conflicting edit.
- **CON-011** Daily total caps and category-specific caps shall be supported.

### 4.5 Deterministic scheduler

- **SCH-001** The scheduling engine shall be deterministic for identical input, configuration, and solver seed, and shall run without network access or AI.
- **SCH-002** Hard constraints, except explicitly acknowledged manual overrides, shall never be violated by automatic planning.
- **SCH-003** Default optimization precedence shall be: immutable events/hard constraints; sleep; hard deadlines; goal progress; habits/routines; priority/soft deadlines; energy/context fit; reduced context switching; balance/free space.
- **SCH-004** The precedence and objective weights shall be user-configurable and resettable to defaults.
- **SCH-005** The scheduler shall maintain a rolling firm plan, defaulting to seven days and configurable within a safe bounded range, plus a ninety-day tentative plan.
- **SCH-006** Firm blocks shall sync to the configured Google Calendar. Tentative blocks shall remain app-only and publish automatically when they enter the firm horizon.
- **SCH-007** Freeze horizon, firm/tentative horizons, automatic publish, and automatic recomposition triggers shall be configurable.
- **SCH-008** Recomposition triggers shall include imported calendar changes, early/late completion, pause, skip, new item, changed deadline, a configured daily time, and a manual command.
- **SCH-009** Schedule stability shall be an explicit objective. A valid existing placement shall not move unless the change improves a higher-priority objective enough to exceed the configured movement cost.
- **SCH-010** After recomposition, the app shall list material block movements, their reasons, and an atomic undo action.
- **SCH-011** “Why here?” shall explain the binding constraints and objective contributions for a block using solver facts, not an unsupported model-generated rationale.
- **SCH-012** The scheduler shall generate comparable alternative schedules and side-effect-free what-if simulations.
- **SCH-013** Overload shall open a resolution view that identifies unsatisfied demand and offers explicit choices such as relax, defer, split, reduce, skip, or protect current commitments.
- **SCH-014** Overlapping immutable events shall remain visible as a conflict.
- **SCH-015** The system shall warn when a deadline is fragile because little or no feasible slack remains.
- **SCH-016** Learned scheduling preferences shall be inspectable, editable, lockable, and resettable.

The current `SCH-006` implementation checkpoint is not acceptance of the full
requirement. The backend can explicitly preview, approve, enqueue, and report
durable delivery of the current immutable published revision's generated
not-yet-elapsed firm `planned` and `pinned` blocks to one selected writable
Google Calendar. This server-first path is disabled by default behind both the
general and schedule-specific outbound gates, uses stable logical
slot/incarnation mappings, and publishes private/redacted events with reminders
and attendee updates suppressed. Elapsed published events are never rewritten,
deleted, or reused. The macOS and Android clients now expose explicit owner
review, approval, enqueue, recovery, and aggregate status for this flow. No
automatic horizon process triggers it, and inbound Google edits, moves, or
deletions are not interpreted yet; tentative blocks remain app-only as required.
`SCH-006` remains open until automation and bidirectional behaviors satisfy the
requirement.

### 4.6 Habits

- **HAB-001** Habit recurrence shall support selected weekdays; N times per day/week/month; every N time units; completion-relative recurrence; and custom rules.
- **HAB-002** Frequency counting shall be selectable between calendar periods and rolling windows.
- **HAB-003** Habits shall support minimum spacing between occurrences.
- **HAB-004** Habit duration shall be exact, ranged, unknown/estimated, and optionally splittable.
- **HAB-005** Habit time windows shall support hard and preferred semantics.
- **HAB-006** Missed behavior shall be configured per habit, including skip, carry, reduce frequency, or ask.
- **HAB-007** A habit may be paused without breaking streak or adherence statistics.
- **HAB-008** Each occurrence shall support completed, skipped, partial quantity, note, and later correction.
- **HAB-009** Habit analytics shall include streaks, adherence, trends, and supportive messaging without punitive dark patterns.

### 4.7 Active execution, timers, pauses, and breaks

- **EXE-001** Exactly one item may be active for the user at a time; active state shall synchronize across devices with deterministic conflict resolution.
- **EXE-002** Starting an item shall optionally start a timer and the mapped platform Focus/DND mode.
- **EXE-003** Pause shall offer presets, a custom duration, until a chosen time, indefinite, and an optional reason.
- **EXE-004** A timed break ending shall notify the user and offer resume, extend, or select another item. Unconditional auto-resume shall be opt-in only.
- **EXE-005** An indefinite pause shall tentatively push affected work. Return shall trigger final recomposition.
- **EXE-006** Resume shall default to the paused item while allowing the scheduler to explain a preferable alternative.
- **EXE-007** Finishing early or late shall record actual time, update remaining demand, and trigger recomposition when enabled.
- **EXE-008** Actual-duration history shall be retained and used for transparent estimates until the user deletes or resets it.
- **EXE-009** macOS lock/sleep/inactivity and Android equivalent signals shall help distinguish active work from abandoned timers, with a correction flow.
- **EXE-010** Pomodoro patterns and mandatory-break rules shall be configurable.
- **EXE-011** “Will do later” shall pause first, derive its replacement duration
  and conflicts from the current authoritative schedule, require explicit
  approval for the exact reported conflict set, and never transfer approval to
  changed or expired evidence.

### 4.8 Google Calendar

- **GCAL-001** Google Calendar shall synchronize bidirectionally and continue through an offline operation queue.
- **GCAL-002** Each connected calendar shall have independent hidden/visible, read-only, blocking, and writable roles.
- **GCAL-003** On setup, DayWeave shall offer to create a dedicated writable calendar.
- **GCAL-004** Moving a DayWeave block in Google Calendar shall update and pin its local placement. Deleting the event shall unschedule the work without deleting the underlying item.
- **GCAL-005** Imported events shall be fixed by default. App-created private events may be configured as flexible.
- **GCAL-006** Sync shall support recurring series, attendees, RSVP, conferencing, attachments, event visibility, free/busy availability, time zones, and daylight-saving transitions.
- **GCAL-007** Birthdays and observances shall be visible and nonblocking by default and shall not generate tasks automatically.
- **GCAL-008** Vacation and out-of-office shall block availability by default. Declined events shall be ignored; free events shall be visible/nonblocking; tentative and other all-day behavior shall be configurable.
- **GCAL-009** External changes shall synchronize in the background and invoke the configured recomposition behavior.
- **GCAL-010** An event with attendees shall never be created or materially edited without an explicit preview and approval.
- **GCAL-011** Only private events marked flexible may move automatically. Attendee events require explicit confirmation for time changes.
- **GCAL-012** The event editor shall support occurrence, this-and-following, and entire-series edit scopes.
- **GCAL-013** The app shall support in-app RSVP and one-click joining of conference links.
- **GCAL-014** The app shall warn about attendee conflicts, travel infeasibility, and meeting density.
- **GCAL-015** The assistant may propose meeting preparation/follow-up only when relevant, and creation requires approval.
- **GCAL-016** Notification ownership shall be configurable as DayWeave, Google, or both by category.

### 4.9 Google Tasks

- **GTASK-001** The user shall select Google Task lists to synchronize.
- **GTASK-002** Supported common fields and completion state shall sync bidirectionally; rich DayWeave-only scheduling metadata shall remain in DayWeave.
- **GTASK-003** External completion shall immediately remove remaining DayWeave calendar blocks for the task.
- **GTASK-004** External deletion shall move the DayWeave item to recoverable trash.
- **GTASK-005** An external due-date change shall update local constraints and invoke configured recomposition.
- **GTASK-006** Imported tasks lacking duration or other required planning data shall enter the unified Inbox.
- **GTASK-007** Google Tasks shall use the same offline queue, retry, audit, conflict, and undo model as Calendar.

### 4.10 Capture, Inbox, files, and search

- **CAP-001** Capture shall be available through structured forms, natural-language text, voice, macOS global quick add, menu bar, share extension, Android share target, launcher shortcuts, and drag/drop where supported.
- **CAP-002** The unified Inbox shall contain unprocessed captures, ambiguous imports, external assistant suggestions, and Google Tasks missing required planning data.
- **CAP-003** Voice transcription shall prefer on-device recognition and may use a configured OpenAI fallback.
- **CAP-004** Voice recordings shall be deleted after a successful transcript by default; retaining audio requires explicit configuration.
- **CAP-005** DayWeave shall store attachments up to 50 MB by default. Larger files shall be represented by an external link unless the limit is changed.
- **CAP-006** Stored attachments shall support OCR/text extraction, search, and relevant assistant analysis when privacy permits.
- **CAP-007** URLs shall support fetched title/preview and an optional retained snapshot.
- **CAP-008** Duplicate detection shall propose a reviewable merge and never silently destroy one record.
- **SEA-001** Search shall cover titles, metadata, notes, extracted attachment text, history, completed items, and canceled items.
- **SEA-002** Semantic search shall respect the same sensitivity, authorization, and offline visibility rules as ordinary search.

### 4.11 Embedded assistant

- **AI-001** DayWeave shall provide a universal assistant and persistent chats scoped to individual goals and projects.
- **AI-002** The assistant shall parse natural language/voice, create or edit every item kind, infer draft duration/constraints/energy/context, discuss and decompose goals, explain schedules, resolve overloads, conduct reviews, and suggest improvements.
- **AI-003** Goal decomposition shall remain a proposal until reviewed and approved.
- **AI-004** A harmless single-item change may execute immediately with undo. Bulk changes and recomposition require preview. Deletion, relaxed deadlines/hard constraints, and any external-calendar side effect always require confirmation.
- **AI-005** Assistant memory shall be visible, editable, disableable, and deletable.
- **AI-006** Model and reasoning selection shall have an automatic default and an advanced visible override.
- **AI-007** Web search may be used with source attribution and user-configured access.
- **AI-008** Calendar, notes, history, and attachments may enter model context only within configurable privacy permissions. Sensitive items are excluded by default.
- **AI-009** The app shall remain useful offline. Optional assistant requests shall queue visibly or fail gracefully without blocking local changes.
- **AI-010** Proactive suggestions shall obey quiet hours, urgency thresholds, category controls, and daily limits.
- **AI-011** AI output that purports to explain solver behavior shall be grounded in structured solver traces.

### 4.12 External MCP, Codex skill, and Suggestions Inbox

- **MCP-001** The private backend shall expose an authenticated, least-privilege MCP service for ChatGPT/Codex-capable clients.
- **MCP-002** An accompanying scheduling skill/plugin shall document supported resources, proposal tools, privacy behavior, and review workflow.
- **MCP-003** External clients may read schedule detail only at the configured per-client permission level.
- **MCP-004** External clients shall never directly mutate canonical data. They may submit proposals to the Suggestions Inbox.
- **MCP-005** A proposal may contain items, goal decompositions, constraints, events, full schedule alternatives, what-if simulations, or plain recommendations.
- **MCP-006** The user shall be able to inspect, edit, accept, reject, or bulk-handle proposals.
- **MCP-007** Each proposal shall store source integration/conversation, creation time, explanation, requested permissions, expiry, and resulting audit link.
- **MCP-008** Proposal expiry shall be configurable.
- **MCP-009** The user shall be able to revoke each external client's access without affecting other sessions.
- **MCP-010** DayWeave shall offer to open or continue a ChatGPT/Codex conversation with explicitly selected context.

### 4.13 Offline-first storage, synchronization, and conflicts

- **SYNC-001** Each client shall keep an encrypted local database sufficient for offline viewing, capture, planning, execution, and editing.
- **SYNC-002** The backend shall be the cross-device canonical source and shall retain revisions, operation IDs, audit records, and tombstones needed for convergence.
- **SYNC-003** Local mutations shall be durably queued before being presented as saved.
- **SYNC-004** Retry shall be idempotent. Duplicate delivery shall not duplicate items, time blocks, notifications, or external events.
- **SYNC-005** Sync state—offline, pending, failed, conflicted, or current—shall be visible.
- **SYNC-006** Conflict handling shall preserve both user-authored values when automatic field-level merging is unsafe and shall provide a review UI.
- **SYNC-007** Successful online changes shall normally become visible on another online device within ten seconds.
- **SYNC-008** The service shall use push/WebSocket-style near-real-time updates with durable catch-up after disconnect.
- **SYNC-009** Undo shall work across synchronized changes when the inverse remains safe; otherwise it shall present the external effects requiring confirmation.

### 4.14 Time, travel, location, health, and weather

- **CTX-001** The initial zone shall be Europe/Madrid. Travel detection shall suggest or apply zone changes according to settings.
- **CTX-002** Each relevant item shall support absolute-time and floating-time semantics.
- **CTX-003** The default locale presentation shall be Monday-first and 24-hour time.
- **CTX-004** Travel estimation shall support driving, public transit, walking, and cycling using Google Maps services.
- **CTX-005** Device location may power geofenced suggestions and live travel recomposition under explicit permissions.
- **CTX-006** Android Health Connect shall be integrated for supported energy/recovery signals, with manual correction and manual energy check-in.
- **CTX-007** Weather shall influence outdoor-task suggestions without silently canceling the task.
- **CTX-008** The signal model shall have a provider boundary for future WHOOP integration.
- **CTX-009** Travel profiles shall allow temporary availability, sleep, location, and transport-mode defaults without overwriting the home profile.

### 4.15 Notifications and system integration

- **NOT-001** Notifications shall support start, done, pause, skip, later, resume, and replan actions where relevant.
- **NOT-002** Notification action and dismissal state shall synchronize between devices.
- **NOT-003** Locked-device notifications for sensitive items shall use generic text and omit content.
- **NOT-004** AI notifications shall obey quiet hours and category/daily limits.
- **NOT-005** macOS shall support app, menu bar, global quick add, widgets, Spotlight, Shortcuts/Siri, share extension, and optional launch-at-login helper.
- **NOT-006** Android shall support the full app, persistent active-timer notification, Quick Settings tile, home-screen widgets, share target, launcher shortcuts, and Android/Gemini actions where available.
- **NOT-007** Schedule categories may map to platform Focus/DND modes, subject to platform permissions.

### 4.16 Client UX

- **UX-001** Today shall be the initial default view. A deliberate user switch shall be remembered.
- **UX-002** Primary views shall include Today, week, month, backlog, habits, projects/goals, statistics/reviews, Calendar, Inbox, and Assistant.
- **UX-003** macOS shall use a sidebar, central timeline/content surface, and right inspector/assistant panel that can adapt to narrower windows.
- **UX-004** Android shall use bottom navigation for Today, Calendar, Inbox, and Assistant, with other destinations under More.
- **UX-005** Both clients shall support system light/dark, manual theme override, accent selection, and configurable colors by type, project, calendar, and priority.
- **UX-006** The timeline shall default to fifteen-minute increments and zoom from five-minute detail to a whole-day overview.
- **UX-007** Completed work shall remain visible and dimmed by default with a hide control.
- **UX-008** Direct manipulation shall include drag/drop, resize, pin, multi-select, and context-appropriate bulk actions.
- **UX-009** macOS shall provide a command palette and configurable keyboard shortcuts.
- **UX-010** Baseline platform accessibility shall include semantic labels, keyboard navigation, scalable text where supported, sufficient contrast, reduced-motion behavior, and screen-reader order.
- **UX-011** Guided onboarding shall configure identity, Google integration, calendars/task lists, availability/sleep, privacy, notifications, and the first generated plan.
- **UX-012** A synthetic demo workspace shall be optional and must never mix data with the real workspace without explicit import.

### 4.17 Data export, backup, and recovery

- **DATA-001** Export shall support encrypted full backup, JSON, CSV, ICS, and Markdown.
- **DATA-002** Attachments shall be copied into DayWeave-controlled object storage unless deliberately represented as external links.
- **DATA-003** The backend shall produce encrypted incremental backups at least every fifteen minutes and encrypted daily snapshots.
- **DATA-004** Backups shall be versioned and retained for seven days initially.
- **DATA-005** Restore tooling shall support a clean replacement deployment and a validation-only restore rehearsal.
- **DATA-006** The target recovery point is at most fifteen minutes and target recovery time is at most two hours.

## 5. Security and privacy requirements

- **SEC-001** All remote traffic shall use HTTPS/TLS. The initial public endpoint may use Nebius Tunnel-generated HTTPS.
- **SEC-002** Persistent disks, object storage, backups, and sensitive local databases shall be encrypted at rest.
- **SEC-003** Especially sensitive fields shall have application-level encryption with keys separated from the database backup.
- **SEC-004** Stable credentials, private keys, tokens, and OAuth secrets shall never be committed to Git.
- **SEC-005** Deployment and runtime shall use separate project-scoped Nebius service accounts bootstrapped through a locally authenticated owner profile.
- **SEC-006** App lock shall support platform biometrics and a configurable auto-lock timeout.
- **SEC-007** A sensitive item shall be excluded by default from lock-screen detail, widgets while locked, external MCP access, proactive assistant context, and attachment analysis.
- **SEC-008** External clients shall receive the minimum configured resource and field scope. Revocation shall take effect without redeployment.
- **SEC-009** Logs, traces, crash reports, and performance telemetry shall exclude user content and credentials.
- **SEC-010** Anonymous crash/performance diagnostics may be opt-in or clearly disclosed; behavioral/product analytics shall be off by default.
- **SEC-011** Dependency, image, configuration, and secret scanning shall run in CI.
- **SEC-012** Security updates shall apply automatically with an overnight maintenance/restart policy and visible status.
- **SEC-013** Alerts shall cover uptime, failed/stale backup, budget threshold, suspicious authentication, and certificate/TLS failure by email and in-app delivery.
- **SEC-014** Events with attendees, destructive actions, relaxed hard deadlines, and external mutations shall always cross an explicit confirmation boundary.

## 6. Service and deployment requirements

- **OPS-001** The initial production deployment shall fit within USD 50/month including estimated tax. Scale-up requires measured need and an updated cost estimate.
- **OPS-002** The baseline topology shall fit on one regular Nebius `cpu-e2` VM with 2 vCPU and 8 GiB RAM in `eu-north1`, a right-sized SSD beginning around 32 GiB, and Standard Object Storage.
- **OPS-003** The VM shall run isolated containers/services for API/MCP, worker, PostgreSQL, and HTTPS ingress/tunnel.
- **OPS-004** PostgreSQL shall not be publicly reachable. Administrative access shall use the private deployment path/tunnel.
- **OPS-005** Health checks, structured content-free logs, metrics, and correlation IDs shall cover every service.
- **OPS-006** Infrastructure configuration and deployment shall be reproducible from the public repository, with secrets supplied only by CI/project secret stores and never committed.
- **OPS-007** CI/CD shall support development, beta, and stable channels with migration checks, rollback information, artifact hashes, and provenance.
- **OPS-008** macOS shall provide a private update feed; Android shall provide signed APK download and update metadata. Stable signing keys shall be held outside Git.
- **OPS-009** Bundle/package identifiers shall derive from `com.greengolddog.dayweave`.
- **OPS-010** A dedicated Google Cloud project shall contain OAuth, Calendar, Tasks, Maps, and related configuration.

## 7. Non-functional requirements

### 7.1 Performance

- **PERF-001** Cold launch shall reach an interactive primary view within two seconds on the target Mac and a healthy target Pixel under the release test profile.
- **PERF-002** Primary scrolling, drag, resize, and navigation interactions shall sustain a perceptually smooth 60 fps under the standard dataset.
- **PERF-003** A normal one-day recomposition shall complete within one second at the 95th percentile on the target Mac.
- **PERF-004** A complex ninety-day recomposition shall complete within ten seconds at the 95th percentile on the target Mac and report progress/cancellation when it exceeds one second.
- **PERF-005** Under normal connectivity, synchronized mutations shall reach another online device within ten seconds at the 95th percentile.

### 7.2 Reliability

- **REL-001** Client writes shall survive process termination immediately after the save acknowledgement.
- **REL-002** Network retry shall be idempotent and ordered per entity where ordering matters.
- **REL-003** Google and assistant outages shall not prevent local schedule use or corrupt the outbound queue.
- **REL-004** Database migrations shall be forward-tested on a production-shaped snapshot and shall document rollback or restore behavior.
- **REL-005** The service shall meet the 15-minute RPO and two-hour RTO in a timed restore drill.

### 7.3 Maintainability

- **MAINT-001** Domain and scheduler behavior shall be shared across backend and clients through a stable core boundary rather than duplicated algorithms.
- **MAINT-002** Provider integrations shall sit behind contract-tested interfaces with fakes for offline development.
- **MAINT-003** Architecture decisions, setup, operations, backup/restore, security model, and end-user workflows shall be documented in the repository.
- **MAINT-004** Every release shall be traceable to a Git commit and reproducible configuration.

## 8. Required automated verification

- **TEST-001 — scheduler unit tests:** hard/soft constraints, splitting, buffers, horizons, priorities, time zones, recurrence, stability, and explanations.
- **TEST-002 — scheduler property tests:** determinism, no unapproved hard-constraint violations, no overlapping exclusive blocks, demand conservation across splits, and stable round trips.
- **TEST-003 — persistence tests:** schema migrations, local durability, tombstones, audit/undo, encryption boundaries, and backup compatibility.
- **TEST-004 — provider contract tests:** Google Calendar, Google Tasks, Codex App Server, Maps, Health Connect boundary, weather boundary, and MCP protocol.
- **TEST-005 — sync tests:** offline edits, duplicate delivery, reordering, simultaneous edits, active-item conflict, external deletion, retries, and reconnect catch-up.
- **TEST-006 — UI tests:** onboarding, capture, scheduling, pause/resume, approval boundaries, overload resolution, Inbox proposals, accessibility, and platform navigation.
- **TEST-007 — end-to-end tests:** clean deployment, account setup against fakes, backup, destructive-loss simulation, restore, update channels, and rollback/forward recovery.
- **TEST-008 — performance tests:** all explicit performance budgets with standard and stress datasets.
- **TEST-009 — security tests:** authn/authz, scope revocation, sensitive redaction, token/secret scanning, input validation, rate limiting, attachment isolation, and external-effect confirmations.
- **TEST-010 — real integration isolation:** use dedicated Google test calendars and task lists. Do not touch the user's real data until fakes and isolated integration suites pass.

## 9. Definition of complete

DayWeave is complete only when all of the following are true:

1. The deterministic scheduler satisfies every `SCH`, `CON`, `HAB`, `HIE`, and `EXE` requirement and passes its unit/property/performance suites.
2. The backend is deployed to the approved Nebius topology within budget, monitored, backed up, and proven recoverable in a timed restore test.
3. The polished macOS client implements all required views and platform surfaces and is usable through a local release build.
4. The polished Android client has functional parity, passes on the owner's Pixel 11, and is available as a signed release APK.
5. Google Calendar and Google Tasks pass bidirectional, recurring, offline, conflict, and external-effect safety tests on isolated test data, then a controlled real-account check.
6. Embedded Codex login, assistant workflows, privacy controls, confirmation policy, grounded schedule explanations, and offline degradation are verified.
7. The private MCP service and scheduling skill/plugin allow permissioned reading and proposal submission without direct external mutation.
8. Security controls, exports, session revocation, app lock, sensitive-item behavior, audit, alerts, and content-free diagnostics are verified.
9. Setup, architecture, user, security, deployment, monitoring, backup, restore, and recovery documentation is current and reproducible.
10. The owner completes a seven-consecutive-day real-life trial. All critical and major defects found in the trial are fixed and retested.

The absence of Apple Developer Program or Play Console memberships does not block completion under the approved local macOS and direct-APK distribution model. WHOOP does not block completion; Android Health Connect does.
