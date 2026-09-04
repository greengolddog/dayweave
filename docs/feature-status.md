# DayWeave feature status

Last updated: **2026-09-04**

Tracking baseline: the `main` commit containing this ledger revision

This is the maintained implementation ledger for DayWeave. It answers two
questions: what exists in the repository now, and what still has to be added
before the full app is complete.

The authoritative scope remains [product-requirements.md](product-requirements.md),
which derives from the complete [discovery answer ledger](discovery-answers.md).
This file does not weaken or replace any requirement. A row marked implemented
means only the capability described in that row is committed and has automated
coverage; it does **not** mean that every requirement in the related product area
is complete.

## Status meanings

| Status | Meaning |
| --- | --- |
| **Implemented + covered** | The stated slice is committed and exercised by automated tests or a guarded build check. |
| **In progress** | Implementation exists on the active development path, but the slice is not yet fully integrated, verified, and committed. |
| **Planned** | Required by the product specification and still to be built or completed. |
| **External gate** | Code can progress, but final verification needs owner-controlled credentials, infrastructure approval, hardware, or real-life use. |

## Release targets

- macOS: polished native app distributed as a verified local build.
- Android: polished native app distributed as a signed APK, with final testing
  on the owner's Pixel 11.
- Backend: private DayWeave service on an owner-approved Nebius deployment,
  kept within the USD 50/month ceiling.
- Integrations: bidirectional Google Calendar and Google Tasks, embedded Codex,
  and a permissioned MCP/skill path for ChatGPT/Codex suggestions.

## Current implemented feature slices

| Capability available in the repository | Related scope | Evidence |
| --- | --- | --- |
| Deterministic, offline Rust scheduling with hard and soft constraints, task splitting, recurrence materialization, bounded composition, and stable schedule output | `CON`, `SCH`, parts of `HIE` | [`dayweave-core`](../crates/dayweave-core/src/scheduler.rs), [`dayweave-compose`](../crates/dayweave-compose/src/prepare.rs), and their [scheduler tests](../crates/dayweave-core/tests/scheduler.rs) |
| Sandboxed, bounded scheduler-helper protocol used by native clients, including strict JSON handling and packaged macOS/Android build paths | `SCH-001`, `MAINT-001`, parts of `PERF` and `SEC` | [`dayweave-scheduler-helper`](../crates/dayweave-scheduler-helper/src/lib.rs), [macOS helper build](../scripts/build-macos-scheduler-helper.sh), and [Android scheduler build](../scripts/build-android-scheduler-library.sh) |
| Canonical item authoring and synchronization, rich scheduling metadata, recoverable 30-day trash, structural goal/project/task semantics, unlimited-depth cycle prevention, and four dependency types | `DOM`, `HIE`, `GOAL`, parts of `SYNC` | [item API](../server/dayweave-api/src/items), [structural domain tests](../crates/dayweave-core/tests/structural_domain.rs), and [item-sync contract](item-sync.md) |
| Server-authoritative schedule preview/publication replication with revision fencing, immutable published read models, and content-free invalidation streams | `SCH-005`–`SCH-010`, `SYNC-007`–`SYNC-008` | [scheduling API](../server/dayweave-api/src/scheduling), [schedule tests](../server/dayweave-api/tests/schedule_preview.rs), and [scheduling contract](scheduling-api.md) |
| Cross-device active execution protocol with one active item, pause/resume, timed and indefinite breaks, safe “will do later,” durable replacement schedules, and execution invalidation/catch-up | `EXE-001`, `EXE-003`–`EXE-007`, parts of `NOT-001` | [execution API](../server/dayweave-api/src/execution), [execution tests](../server/dayweave-api/tests/execution_api.rs), and [execution-sync contract](execution-sync.md) |
| Encrypted local planner state on macOS and Android, durable offline mutation journals, idempotent replay, visible reviewed conflicts, credential-generation fencing, and sensitive-item containment | `SYNC-001`–`SYNC-006`, `SEC-006`–`SEC-010` | [macOS persistence](../apps/macos/Sources/DayWeaveMac/Store/EncryptedPlannerPersistence.swift), [Android persistence](../apps/android/app/src/main/java/com/greengolddog/dayweave/data/PlannerStateRepository.kt), and [security model](security.md) |
| Native app lock on both clients, backed by Keychain/Keystore proof, plus privacy boundaries for lock screens, widgets, assistants, and external integrations | `SEC-006`–`SEC-010` | [macOS app lock](../apps/macos/Sources/DayWeaveMac/Store/AppLockController.swift), [Android app lock](../apps/android/app/src/main/java/com/greengolddog/dayweave/security/AppLockController.kt), and their adjacent tests |
| Google OAuth custody and reauthentication foundations, Calendar source import, Google Tasks import, reviewed private-event publication, generated-schedule publication, and crash-safe outbound journals | Parts of `ID`, `GCAL`, `GTASK`, and `SYNC` | [Google service](../server/dayweave-api/src/google_sync), [macOS integration store](../apps/macos/Sources/DayWeaveMac/Store/GoogleIntegrationStore.swift), [Android Google coordinators](../apps/android/app/src/main/java/com/greengolddog/dayweave/sync), and [integration setup](setup-integrations.md) |
| Authenticated Suggestions Inbox, typed proposal preview/apply/undo, private MCP proposal tools, least-privilege client scopes, and fail-closed OAuth account linking | Parts of `AI` and `MCP` | [proposal application service](../server/dayweave-api/src/proposals/application.rs), [MCP tools](../server/dayweave-api/src/mcp/tools.rs), and [proposal contract](proposal-applications.md) |
| Managed Codex App Server login/runtime foundation and planner-aware conversations on macOS, plus authenticated advisory assistant transport on Android | `ID-003`–`ID-004`, parts of `AI-001`–`AI-009` | [macOS Codex controller](../apps/macos/Sources/DayWeaveMac/Store/CodexConversationController.swift), [Android assistant manager](../apps/android/app/src/main/java/com/greengolddog/dayweave/sync/AssistantManager.kt), and [auth rollout](auth-rollout.md) |
| Native SwiftUI and Jetpack Compose shells with Today/Calendar/Inbox/Assistant navigation, canonical item editors, planning profiles, privacy-first onboarding, and Google review surfaces | Parts of `UX-001`–`UX-012` | [macOS views](../apps/macos/Sources/DayWeaveMac/Views), [Android UI](../apps/android/app/src/main/java/com/greengolddog/dayweave/ui), and platform test suites |
| Health Connect energy-signal provider on Android with explicit permissions, private projection, and manual-safe fallback boundaries | `CTX-006`, future boundary for `CTX-008` | [Health Connect integration](../apps/android/app/src/main/java/com/greengolddog/dayweave/health) |
| Reproducible local macOS and signed-APK build paths, guarded signing boundaries, CI security scans, container builds, encrypted backup scripts, and a cost-guarded Nebius Terraform plan | Parts of `OPS`, `SEC-004`, `SEC-011`, and `DATA-003`–`DATA-006` | [release workflow](../.github/workflows/release.yml), [local build scripts](../scripts), and [deployment assets](../deploy) |
| Shared habit recurrence and lifecycle core: bounded custom rules, rolling and completion-relative cadence, stable occurrence identity and move proof, missed/spacing policies, corrections, partial-progress demand, pauses, streaks, analytics, and scheduler-helper budgets; plus strict Android habit API transport | Core of `HAB-001`–`HAB-009`, parts of `CON` and `SCH` | [habit domain](../crates/dayweave-core/src/habits.rs), [recurrence engine](../crates/dayweave-core/src/recurrence.rs), [custom recurrence](../crates/dayweave-core/src/custom_recurrence.rs), [scheduler tests](../crates/dayweave-core/tests/scheduler.rs), and [habit ledger design](habit-occurrence-ledger.md) |
| macOS habit occurrence and statistics experience with strict transport, encrypted origin-bound offline persistence, exact replay, conflict review, done/partial/skipped corrections, pause/resume, trends, streaks, quantities, content-free invalidation/catch-up with polling fallback, and privacy scrubbing | Client slice of `HAB-006`–`HAB-009`, parts of `SYNC` and `SEC` | [habit models](../apps/macos/Sources/DayWeaveMac/Models/HabitModels.swift), [habit sync store](../apps/macos/Sources/DayWeaveMac/Store/HabitSyncStore.swift), [habit invalidation stream](../apps/macos/Sources/DayWeaveMac/Store/HabitInvalidationStream.swift), and adjacent tests |

## Active implementation

The current milestone is the complete habit path across the shared core,
authoritative backend, scheduling input, and Android, followed by
cross-platform end-to-end closure. The macOS client slice is implemented and
covered; it remains part of the final integrated verification:

- persist and synchronize occurrence outcomes, partial progress, corrections,
  missed behavior, and streak-neutral pauses;
- make habit duration and remaining demand participate in deterministic day
  composition without double-counting;
- add polished Today/Habits controls on Android for done, partial, skipped,
  correct, pause, resume, and analytics;
- finish invalidation/catch-up, offline replay, conflict review, and end-to-end
  PostgreSQL/client coverage.

These items remain **in progress** until the complete slice passes the Rust,
backend, macOS, Android, persistence, and protocol test gates and is committed.

## Features still to add or close

Every requirement remains mandatory unless the owner explicitly changes
[product-requirements.md](product-requirements.md). This table summarizes the
remaining work without pretending that partially implemented areas are done.

| Product area | Status | Work still required for full acceptance |
| --- | --- | --- |
| Identity and accounts (`ID`) | **In progress** | Finish session inventory/revocation UI, recovery flow, integration disconnect/reconnect parity, and final managed Codex login verification on both platforms. |
| Common item model (`DOM`) | **In progress** | Complete every rich field, template, bulk operation, progress mode, audit/undo presentation, and cross-platform editing path. |
| Hierarchy, goals, projects, routines, dependencies (`HIE`, `GOAL`, `ROU`) | **In progress** | Add polished hierarchy navigation, roll-ups, milestones/measures, weekly goal allocation, routine authoring/execution, and complete dependency conflict explanations. |
| Scheduling restrictions and profiles (`CON`) | **In progress** | Close every per-item hard/soft restriction, buffers, caps, pinning, partial-work accounting, acknowledged overrides, and profile precedence case. |
| Scheduler intelligence (`SCH`) | **In progress** | Complete configurable objective ordering, movement-cost tuning, alternatives/what-if, overload resolution, fragile-deadline warnings, “Why here?”, learned preference controls, and 90-day performance proof. |
| Habits (`HAB`) | **In progress** | Finish the active milestone described above, then verify all nine habit requirements end to end. |
| Active execution (`EXE`) | **In progress** | Add Focus/DND mappings, inactivity correction, duration-learning controls, Pomodoro/mandatory-break settings, and full cross-device/UI coverage. |
| Google Calendar (`GCAL`) | **In progress** | Complete full bidirectional parity for series scopes, attendees/RSVP, conferencing, attachments, flexible-event moves, birthdays/observances, OOO/free/tentative policy, conflicts, travel, density, and notification ownership. |
| Google Tasks (`GTASK`) | **In progress** | Finish bidirectional field/list parity, external completion/deletion/due-date reactions, shared conflict/undo behavior, and controlled real-account tests. |
| Capture, Inbox, files, and search (`CAP`, `SEA`) | **In progress** | Add voice, global/menu/share/shortcut/drag capture, attachment storage/OCR, URL snapshots, duplicate review, full-text/history search, and privacy-preserving semantic search. |
| Embedded assistant (`AI`) | **In progress** | Complete universal and goal/project chats, all-item natural-language operations, overload/review workflows, visible memory/model/privacy controls, sourced web search, grounded explanations, offline queueing, and proactive limits. |
| External MCP, Codex skill, Suggestions Inbox (`MCP`) | **In progress** | Complete permission configuration UI, all proposal kinds, proposal editing/bulk handling/expiry, per-client revocation, conversation continuation, and end-to-end ChatGPT/Codex verification. |
| Offline synchronization and conflicts (`SYNC`) | **In progress** | Extend the proven journal/invalidation pattern to every entity, finish field-level conflict UI and safe synchronized undo, and meet the ten-second convergence target. |
| Time, travel, location, health, weather (`CTX`) | **In progress** | Add travel-zone profiles, absolute/floating time UX, Maps travel modes, location/geofences, manual energy correction, weather suggestions, and the planned WHOOP provider extension. |
| Notifications and platform integration (`NOT`) | **In progress** | Complete synchronized notification actions, privacy-safe lock presentation, macOS menu bar/widgets/Spotlight/Shortcuts/share/login helper, Android timer notification/tile/widgets/share/shortcuts/actions, and Focus/DND mappings. |
| Client polish and accessibility (`UX`) | **In progress** | Complete every primary view, adaptive macOS inspector, Android More destinations, theming, timeline zoom, dim/hide completed work, drag/resize/pin/multi-select, command palette/shortcuts, accessibility, and demo workspace. |
| Export, backup, and recovery (`DATA`) | **In progress** | Add encrypted full backup plus JSON/CSV/ICS/Markdown export, attachment object storage, production-shaped migration checks, and timed restore/RPO/RTO evidence. |
| Security and privacy (`SEC`) | **In progress** | Close production key separation, session/client revocation, telemetry controls, runtime scanning, automatic security maintenance, alert delivery, and full adversarial security tests. |
| Operations and distribution (`OPS`) | **In progress** | Provision only after approval, configure private HTTPS/monitoring/alerts, exercise dev/beta/stable release and rollback paths, and produce final local macOS and signed Android artifacts with provenance. |
| Performance, reliability, and complete verification (`PERF`, `REL`, `TEST`) | **Planned** | Run all explicit launch/UI/scheduler/sync budgets, property and provider suites, production-shaped migrations, destructive restore rehearsal, security tests, and complete end-to-end acceptance. |

## External verification gates

These are not permission to spend money or use private data. They are tracked
separately because final acceptance needs owner-controlled resources:

- **External gate:** approval before creating or changing paid Nebius resources;
- **External gate:** Google Cloud credentials plus isolated Calendar and Tasks
  test data before any controlled real-account check;
- **External gate:** installation and acceptance testing on the owner's Pixel 11;
- **External gate:** owner verification of the final local macOS build;
- **External gate:** the required seven-consecutive-day real-life trial, followed
  by fixes and retesting of every critical or major issue.

## Maintenance rules

1. A feature commit must update this ledger when it changes the truth of any
   row. Status moves only after the corresponding tests or guarded build pass.
2. New requirements are added first to the discovery/requirements source of
   truth, then reflected here. This ledger never silently drops scope.
3. “Implemented + covered” describes only the exact slice stated; full product
   completion is governed by the definition of complete in
   [product-requirements.md](product-requirements.md#9-definition-of-complete).
4. The date and tracking baseline are refreshed whenever status changes.
5. Credentials, tokens, private endpoints, tenant identifiers, signing keys,
   personal calendar content, and other private data must never be recorded in
   this public ledger.
