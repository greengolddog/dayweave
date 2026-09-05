# DayWeave feature status

Last updated: **2026-09-05**

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
| Canonical item authoring and synchronization, rich scheduling metadata, anchor-safe bounded custom recurrence with canonical storage, recoverable 30-day trash, structural goal/project/task semantics, unlimited-depth cycle prevention, and four dependency types | `DOM`, `HIE`, `GOAL`, parts of `SYNC` and `ROU` | [item API](../server/dayweave-api/src/items), [item API tests](../server/dayweave-api/tests/items_api.rs), [structural domain tests](../crates/dayweave-core/tests/structural_domain.rs), and [item-sync contract](item-sync.md) |
| Server-authoritative schedule preview/publication replication with revision fencing, immutable published read models, and content-free invalidation streams | `SCH-005`–`SCH-010`, `SYNC-007`–`SYNC-008` | [scheduling API](../server/dayweave-api/src/scheduling), [schedule tests](../server/dayweave-api/tests/schedule_preview.rs), and [scheduling contract](scheduling-api.md) |
| Cross-device active execution protocol with one active item, pause/resume, timed and indefinite breaks, safe “will do later,” durable replacement schedules, and execution invalidation/catch-up | `EXE-001`, `EXE-003`–`EXE-007`, parts of `NOT-001` | [execution API](../server/dayweave-api/src/execution), [execution tests](../server/dayweave-api/tests/execution_api.rs), and [execution-sync contract](execution-sync.md) |
| Encrypted local planner state on macOS and Android, durable offline mutation journals, idempotent replay, visible reviewed conflicts, credential-generation fencing, and sensitive-item containment | `SYNC-001`–`SYNC-006`, `SEC-006`–`SEC-010` | [macOS persistence](../apps/macos/Sources/DayWeaveMac/Store/EncryptedPlannerPersistence.swift), [Android persistence](../apps/android/app/src/main/java/com/greengolddog/dayweave/data/PlannerStateRepository.kt), and [security model](security.md) |
| Native app lock on both clients, backed by Keychain/Keystore proof, plus privacy boundaries for lock screens, widgets, assistants, and external integrations | `SEC-006`–`SEC-010` | [macOS app lock](../apps/macos/Sources/DayWeaveMac/Store/AppLockController.swift), [Android app lock](../apps/android/app/src/main/java/com/greengolddog/dayweave/security/AppLockController.kt), and their adjacent tests |
| Google OAuth custody and reauthentication foundations, Calendar source import, Google Tasks import, reviewed private-event publication, generated-schedule publication, and crash-safe outbound journals | Parts of `ID`, `GCAL`, `GTASK`, and `SYNC` | [Google service](../server/dayweave-api/src/google_sync), [macOS integration store](../apps/macos/Sources/DayWeaveMac/Store/GoogleIntegrationStore.swift), [Android Google coordinators](../apps/android/app/src/main/java/com/greengolddog/dayweave/sync), and [integration setup](setup-integrations.md) |
| Authenticated Suggestions Inbox, typed proposal preview/apply/undo, private MCP proposal tools, least-privilege client scopes, and fail-closed OAuth account linking | Parts of `AI` and `MCP` | [proposal application service](../server/dayweave-api/src/proposals/application.rs), [MCP tools](../server/dayweave-api/src/mcp/tools.rs), and [proposal contract](proposal-applications.md) |
| Managed Codex App Server login/runtime foundation and planner-aware conversations on macOS, plus authenticated advisory assistant transport on Android | `ID-003`–`ID-004`, parts of `AI-001`–`AI-009` | [macOS Codex controller](../apps/macos/Sources/DayWeaveMac/Store/CodexConversationController.swift), [Android assistant manager](../apps/android/app/src/main/java/com/greengolddog/dayweave/sync/AssistantManager.kt), and [auth rollout](auth-rollout.md) |
| Owner-visible active-device inventory and remote revocation on macOS and Android, with exact session UUID/client-instance/client-kind binding, current-row-derived write authority, memory-only stale-state fencing, confirmed remote revocation with ambiguous-result reconciliation, immediate access/refresh rejection, and transactional 16-session/16-pending-enrollment server bounds | `ID-005`, `SEC-008` | [credential-auth repository](../server/dayweave-api/src/persistence/credential_auth_repository.rs), [PostgreSQL acceptance tests](../server/dayweave-api/tests/credential_auth_postgres.rs), [macOS active-device UI](../apps/macos/Sources/DayWeaveMac/Views/DeviceSessionSettingsView.swift), [Android session manager](../apps/android/app/src/main/java/com/greengolddog/dayweave/sync/DeviceSessionManager.kt), and [auth rollout](auth-rollout.md) |
| Native SwiftUI and Jetpack Compose shells with Today/Calendar/Inbox/Assistant navigation, canonical item editors, planning profiles, privacy-first onboarding, and Google review surfaces | Parts of `UX-001`–`UX-012` | [macOS views](../apps/macos/Sources/DayWeaveMac/Views), [Android UI](../apps/android/app/src/main/java/com/greengolddog/dayweave/ui), and platform test suites |
| Health Connect energy-signal provider on Android with explicit permissions, private projection, and manual-safe fallback boundaries | `CTX-006`, future boundary for `CTX-008` | [Health Connect integration](../apps/android/app/src/main/java/com/greengolddog/dayweave/health) |
| Reproducible local macOS and signed-APK build paths, guarded signing boundaries, CI security scans, container builds, encrypted backup scripts, and a cost-guarded Nebius Terraform plan | Parts of `OPS`, `SEC-004`, `SEC-011`, and `DATA-003`–`DATA-006` | [release workflow](../.github/workflows/release.yml), [local build scripts](../scripts), and [deployment assets](../deploy) |
| Shared habit recurrence and lifecycle core: bounded custom rules, rolling and completion-relative cadence, stable occurrence identity and move proof, missed/spacing policies, corrections, partial-progress demand, pauses, streaks, analytics, and scheduler-helper budgets; plus strict Android habit API transport | Core of `HAB-001`–`HAB-009`, parts of `CON` and `SCH` | [habit domain](../crates/dayweave-core/src/habits.rs), [recurrence engine](../crates/dayweave-core/src/recurrence.rs), [custom recurrence](../crates/dayweave-core/src/custom_recurrence.rs), [scheduler tests](../crates/dayweave-core/tests/scheduler.rs), and [habit ledger design](habit-occurrence-ledger.md) |
| Authoritative habit backend with published-schedule evidence admission, immutable PostgreSQL outcome/pause/missed-resolution history, server-clock skip/carry/reduce/ask reconciliation, current-publication binding, strict idempotent replay, ordered delta and content-free invalidation, private analytics, and lifecycle hydration that rejects spoofed, obsolete, or non-leaf authority | Server slice of `HAB-006`–`HAB-009`, `SYNC`, and `SEC` | [habit service](../server/dayweave-api/src/habits), [PostgreSQL repository](../server/dayweave-api/src/persistence/habit_repository.rs), [ledger migration 0026](../server/dayweave-api/migrations/0026_habit_occurrence_ledger.sql), [missed-resolution migration 0027](../server/dayweave-api/migrations/0027_habit_missed_resolutions.sql), [API tests](../server/dayweave-api/tests/habits_api.rs), and [PostgreSQL tests](../server/dayweave-api/tests/habits_postgres.rs) |
| macOS habit occurrence, missed-review, and statistics experience with strict transport, encrypted origin-bound offline persistence, exact replay leases and response binding, conflict review, independent outcome/resolution coordinates, terminal delta authority, done/partial/skipped corrections, skip/carry/reduce actions, pause/resume, trends, streaks, quantities, content-free invalidation/catch-up, habit-aware local composition, and privacy scrubbing | Client slice of `HAB-006`–`HAB-009`, parts of `SYNC`, `SEC`, `SCH`, and `UX` | [habit models](../apps/macos/Sources/DayWeaveMac/Models/HabitModels.swift), [habit sync store](../apps/macos/Sources/DayWeaveMac/Store/HabitSyncStore.swift), [canonical composition store](../apps/macos/Sources/DayWeaveMac/Store/CanonicalSyncStore.swift), [habit UI](../apps/macos/Sources/DayWeaveMac/Views/PlanningViews.swift), and adjacent tests |
| Android habit occurrence, missed-review, and statistics experience with strict origin-bound transport, an encrypted Room V20 offline ledger, exact replay leases and reviewed failures, independent outcome/resolution coordinates, terminal delta checkpoints, content-free invalidation with polling fallback, durability-first done/partial/skipped corrections, skip/carry/reduce actions, pause/resume, bounded analytics, correction-safe retention, current-publication occurrence fencing, habit-aware local composition, and finite custom-RRULE authoring | Client slice of `HAB-006`–`HAB-009`, parts of `SYNC`, `SEC`, `SCH`, and `UX` | [habit models](../apps/android/app/src/main/java/com/greengolddog/dayweave/model/HabitModels.kt), [habit sync manager](../apps/android/app/src/main/java/com/greengolddog/dayweave/sync/HabitSyncManager.kt), [habit UI](../apps/android/app/src/main/java/com/greengolddog/dayweave/ui/screens/HabitSections.kt), [Room V19 migration test](../apps/android/app/src/androidTest/java/com/greengolddog/dayweave/HabitMissedResolutionMigrationTest.kt), [Room V20 migration test](../apps/android/app/src/androidTest/java/com/greengolddog/dayweave/PublishedOccurrenceMembershipMigrationTest.kt), and adjacent unit tests |

## Active implementation

The current milestone is full-product closure after the cross-platform habit
core. The active slice adds owner-visible active-device inventory and remote
revocation across the authoritative backend and both native clients. The
bounded authority contract and native privacy, identity, scope, confirmation,
and reconciliation paths are implemented and covered; controlled multi-client
and owner-device acceptance remain.

Every feature slice whose status is **In progress** has its own approximate
completion value below. These are evidence-based scope estimates, not elapsed
time or delivery forecasts. A percentage advances only after a material code,
integration, or verification checkpoint; the notes state what prevents 100%.

| In-progress feature slice | Approx. complete | Evidence and next checkpoint |
| --- | ---: | --- |
| Native active-device inventory and session revocation | **95%** | The bounded PostgreSQL contract and both native account surfaces have strict identity, scope, privacy, stale-state, and proof-based ambiguous-result recovery for remote and current-device revocation; close a controlled two-client/service run plus Android instrumented and owner-device UI acceptance. |
| Native rich duration shape and provenance | **95%** | Exact, ranged, and unknown durations, provenance, rollback-safe persistence, and request replay are covered by the full macOS and Android gates; close the controlled native/service contract run and device acceptance. |
| General habit minimum spacing | **95%** | Both native authoring paths and every shared recurrence family enforce the stricter applicable floor with explicit unmet demand; close the controlled native/service run and device acceptance. |
| Missed-occurrence skip/carry/reduce/ask workflow | **90%** | Durable server-clock reconciliation, cancellation/restoration, current-publication scheduling effects, encrypted offline replay, migration hardening, and native review/action surfaces pass the Rust/PostgreSQL, macOS, and Android automated gates; close controlled native/service convergence and owner-device acceptance. |
| Controlled native-client/service habit integration | **35%** | The real HTTP/PostgreSQL service path is covered independently and native transports have contract coverage; run both native clients against the same controlled service and prove convergence. |
| Full habit lifecycle on macOS and Android | **75%** | Occurrence, correction, missed-policy, pause, analytics, invalidation, offline replay, conflict, migration, retention, and recomposition behavior has broad automated coverage; close the controlled-service and native UI acceptance matrix. |
| Final macOS build and Pixel 11 APK verification | **45%** | Reproducible local-build and signed-APK paths exist; finish release gates, produce final artifacts, install them, and verify them on the owner's devices. |

The following remaining checkpoints are not yet marked in progress:

- complete the required seven-consecutive-day trial, fix every critical or
  major issue, and rerun affected acceptance paths.

These items remain **in progress** until the integrated client/service gates,
controlled device checks, and real-life acceptance trial are complete.

## Features still to add or close

Every requirement remains mandatory unless the owner explicitly changes
[product-requirements.md](product-requirements.md). This table summarizes the
remaining work without pretending that partially implemented areas are done.

“Approx. complete” is a coarse, evidence-based estimate of implemented agreed
scope. It is not elapsed-time progress, remaining effort, or a delivery promise;
areas differ greatly in size and the values must not be averaged into an app-wide
percentage.

| Product area | Status | Approx. complete | Work still required for full acceptance |
| --- | --- | ---: | --- |
| Identity and accounts (`ID`) | **In progress** | **65%** | Close controlled active-device and credential-only cutover acceptance; finish integration disconnect/reconnect parity, account deletion, the recovery-code flow, and final managed Codex login verification on both platforms. |
| Common item model (`DOM`) | **In progress** | **70%** | Complete the remaining rich fields, templates, bulk operations, progress modes, audit/undo presentation, and cross-platform editing paths. |
| Hierarchy, goals, projects, routines, dependencies (`HIE`, `GOAL`, `ROU`) | **In progress** | **40%** | Add polished hierarchy navigation, roll-ups, milestones/measures, weekly goal allocation, routine authoring/execution, and complete dependency conflict explanations. |
| Scheduling restrictions and profiles (`CON`) | **In progress** | **65%** | Close the remaining per-item hard/soft restrictions, buffers, caps, pinning, partial-work accounting, acknowledged overrides, and profile precedence cases. |
| Scheduler intelligence (`SCH`) | **In progress** | **55%** | Complete configurable objective ordering, movement-cost tuning, alternatives/what-if, overload resolution, fragile-deadline warnings, “Why here?”, learned preference controls, and 90-day performance proof. |
| Habits (`HAB`) | **In progress** | **90%** | Run native-client/service E2E, verify the local macOS build and Pixel 11 APK, close the native UI acceptance matrix, and finish the seven-day trial. |
| Active execution (`EXE`) | **In progress** | **65%** | Add Focus/DND mappings, inactivity correction, duration-learning controls, Pomodoro/mandatory-break settings, and full cross-device/UI coverage. |
| Google Calendar (`GCAL`) | **In progress** | **45%** | Complete full bidirectional parity for series scopes, attendees/RSVP, conferencing, attachments, flexible-event moves, birthdays/observances, OOO/free/tentative policy, conflicts, travel, density, and notification ownership. |
| Google Tasks (`GTASK`) | **In progress** | **45%** | Finish bidirectional field/list parity, external completion/deletion/due-date reactions, shared conflict/undo behavior, and controlled real-account tests. |
| Capture, Inbox, files, and search (`CAP`, `SEA`) | **In progress** | **25%** | Add voice, global/menu/share/shortcut/drag capture, attachment storage/OCR, URL snapshots, duplicate review, full-text/history search, and privacy-preserving semantic search. |
| Embedded assistant (`AI`) | **In progress** | **40%** | Complete universal and goal/project chats, all-item natural-language operations, overload/review workflows, visible memory/model/privacy controls, sourced web search, grounded explanations, offline queueing, and proactive limits. |
| External MCP, Codex skill, Suggestions Inbox (`MCP`) | **In progress** | **55%** | Complete permission configuration UI, all proposal kinds, proposal editing/bulk handling/expiry, per-client revocation, conversation continuation, and end-to-end ChatGPT/Codex verification. |
| Offline synchronization and conflicts (`SYNC`) | **In progress** | **58%** | Extend the proven journal/invalidation/replay-lease pattern to every entity, finish field-level conflict UI and safe synchronized undo, and meet the ten-second convergence target. |
| Time, travel, location, health, weather (`CTX`) | **In progress** | **25%** | Add travel-zone profiles, absolute/floating time UX, Maps travel modes, location/geofences, manual energy correction, weather suggestions, and the planned WHOOP provider extension. |
| Notifications and platform integration (`NOT`) | **In progress** | **20%** | Complete synchronized notification actions, privacy-safe lock presentation, macOS menu bar/widgets/Spotlight/Shortcuts/share/login helper, Android timer notification/tile/widgets/share/shortcuts/actions, and Focus/DND mappings. |
| Client polish and accessibility (`UX`) | **In progress** | **35%** | Complete every primary view, adaptive macOS inspector, Android More destinations, theming, timeline zoom, dim/hide completed work, drag/resize/pin/multi-select, command palette/shortcuts, accessibility, and demo workspace. |
| Export, backup, and recovery (`DATA`) | **In progress** | **30%** | Add encrypted full backup plus JSON/CSV/ICS/Markdown export, attachment object storage, production-shaped migration checks, and timed restore/RPO/RTO evidence. |
| Security and privacy (`SEC`) | **In progress** | **65%** | Finish production key separation and envelope encryption, remaining sensitive-item boundaries, external-client permission/revocation UX and live OAuth review, telemetry controls, ingress/per-principal rate limits and suspicious-authentication alerts, automatic maintenance/runtime scanning, alert delivery, and full adversarial security acceptance. |
| Operations and distribution (`OPS`) | **In progress** | **40%** | Provision only after approval, configure private HTTPS/monitoring/alerts, exercise dev/beta/stable release and rollback paths, and produce final local macOS and signed Android artifacts with provenance. |
| Performance, reliability, and complete verification (`PERF`, `REL`, `TEST`) | **Planned** | — | Run all explicit launch/UI/scheduler/sync budgets, property and provider suites, production-shaped migrations, destructive restore rehearsal, security tests, and complete end-to-end acceptance. |

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
6. Approximate percentages change only when implementation or agreed scope
   materially changes. They describe scope completeness, never elapsed time or
   a delivery commitment, and do not trigger unsolicited chat estimates.
7. Every individual feature slice marked **In progress** must have its own
   approximate completion percentage and a concrete next checkpoint. Broad
   product-area percentages remain separate and are never calculated by simply
   averaging feature rows.
