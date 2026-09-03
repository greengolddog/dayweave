# DayWeave discovery answer ledger

Status: accepted product direction  
Owner: personal workspace of `greengolddog`  
Last normalized: 2026-08-29

This file preserves the answers from the product-discovery conversation so that they can be reread while building DayWeave. The original assistant question text was not written into the repository, so every question below is a faithful **reconstructed paraphrase**. The answers and decisions are authoritative; the prompt wording is not claimed to be verbatim.

Repository-visibility amendment: after the numbered discovery answers, the owner made the
repository public. That later decision supersedes the earlier private-repository assumption for
all security and delivery work. No credentials, signing material, private endpoints, personal
calendar data, or other secrets may be committed; public source files must always be treated as
world-readable.

The conversation briefly reused question numbers 164 and 165 before returning to 161. This ledger normalizes that numbering into one sequence from 1 through 238. The earlier “until created” response is retained under delivery scope, and the later yes/no answers are retained under their applicable numbered decisions.

## 1–14 — product, platforms, and experience

1. **Who is the initial product for?** Personal use by one person.
2. **Should the first release be a thin MVP or a complete product?** Only the full version will be useful; do not define success as a demo-only MVP.
3. **Which client platforms are required?** macOS first and Android second, with the Android target being the owner's Google Pixel 11.
4. **Where should the source live?** Create a new private repository in the owner's GitHub account; choose the product and repository name. The chosen name is **DayWeave**, repository `dayweave`.
5. **Is there an existing codebase or design system to preserve?** No.
6. **What quality bar is expected?** Polished, daily-use quality.
7. **What is the initial Mac target?** Apple M4 running macOS 26.3, build 25D125.
8. **Does Android need the full feature set?** Yes; feature parity is required, subject only to platform-specific capabilities.
9. **May each client be native while sharing a visual language?** Yes. Use native SwiftUI on macOS and native Jetpack Compose on Android, with recognizably similar interaction and styling.
10. **Which macOS entry points are wanted?** The full app, menu-bar access, global quick add, widgets, Spotlight, and Shortcuts.
11. **Which primary views are wanted?** Today, week, month, backlog, habits, projects/goals, and statistics/reviews.
12. **Are direct-manipulation interactions wanted?** Yes: drag and drop, time-block resizing, pinning, and multi-select/bulk actions.
13. **What visual references are appropriate?** The current Gemini and Claude applications: calm, polished, assistant-native products rather than a dense enterprise dashboard.
14. **Language and accessibility constraints?** English initially. No special personal accessibility need was identified, but normal platform accessibility remains required.

## 15–30 — domain model, goals, routines, and hierarchy

15. **Which top-level concepts must exist?** Calendar events, habits, tasks, breaks, goals, and routines. A large goal may have no stated duration, while routines can recur after elapsed time or completion.
16. **How broad is calendar-event support?** Full event support, including special events such as birthdays and observances; those special events need appropriate non-work handling.
17. **Which organizational tools are needed?** Projects, unlimited nested subtasks, checklists, dependencies, tags, contexts, locations, and people.
18. **Are recurring tasks separate from habits?** Yes.
19. **How may progress be measured?** Percentage, tracked time, and custom quantitative units.
20. **What item metadata is needed?** Notes, URLs, files, locations, participants, and conferencing information.
21. **How can work be captured?** Structured forms, natural language, voice, the OS share extension/target, and dragging or sharing email, files, and URLs.
22. **Which lifecycle statuses are required?** Not started, scheduled, active, paused, completed, skipped, canceled, and blocked.
23. **How do unscheduled goals relate to the calendar?** A goal itself may be an outcome without a duration, but actionable work needed to succeed at it must be represented by calendar blocks.
24. **What goal metadata is needed?** Optional target dates, priority, milestones, notes, and one or more measurable outcomes.
25. **Should the assistant help decompose goals?** Yes. The user must be able to discuss a goal in chat, collaboratively adjust the plan, and approve the resulting decomposition before it mutates the plan.
26. **What is a routine?** An ordered sequence of steps. Each step is independently schedulable; recurrence may be calendar-based or measured from completion.
27. **Which recurrence bases are allowed for routines?** Either calendar recurrence or completion-relative recurrence, selected per routine.
28. **Should routines and habits remain separate concepts?** Yes.
29. **How should an overdue recurring step be handled?** Provide explicit buttons such as **Skipped** and **Will do later**.
30. **How deep can the work hierarchy be?** Unlimited depth; every subtask may itself contain subtasks.

## 31–50 — constraints, priority, splitting, and planning horizon

31. **Where are availability, sleep, and schedule profiles configured?** Inside the app.
32. **Where are scheduling restrictions configured?** Inside the app, globally and per item.
33. **Can every scheduling restriction be hard or soft?** Yes.
34. **How are durations represented?** Exact, ranged, or unknown; the assistant may estimate and learn from actuals.
35. **How are deadlines represented?** Date-only or exact time, and independently hard or soft.
36. **Should prioritization offer a richer model than a few labels?** Yes; expose more choice.
37. **What is the default priority formula?** A numeric score derived from importance × urgency.
38. **Should energy/focus influence scheduling?** Yes: low, medium, and deep-focus demands, initially editable by hand and later estimated using WHOOP and other signals.
39. **What is the default scheduling precedence?** In order: immutable events and hard constraints; sleep; hard deadlines; goal progress; habits/routines; priority and soft deadlines; energy/context fit; reduced context switching; balance and free space. The order must be configurable.
40. **Which controls are required for splittable tasks?** Minimum and maximum session length, maximum session count, spacing, eligible days, setup cost, and whether parts must remain ordered.
41. **Does partial completion reduce remaining work?** Yes.
42. **Are buffers first-class constraints?** Yes: preparation, travel, decompression, and context-switching buffers, globally and per item.
43. **Should the planner maintain rolling firm and tentative horizons?** Yes.
44. **What are the initial horizons?** A firm rolling seven days and a tentative rolling ninety days.
45. **Where are tentative blocks published by default?** They stay app-only; firm blocks publish to the selected Google Calendar.
46. **Are freeze-horizon and related planning controls configurable?** Yes, in Settings.
47. **Can individual blocks be pinned/frozen?** Yes.
48. **May the user override a hard or conflicting placement?** Yes, after a clear warning.
49. **Is a dedicated overload-resolution experience required?** Yes.
50. **Must protected free time be configurable?** Yes, globally and per schedule profile.

## 51–62 — scheduler behavior and explainability

51. **Must core scheduling work without AI or a network?** Yes. Use a deterministic constraint optimizer offline.
52. **What is the AI's scheduling role?** Translate natural language, explain outcomes, estimate uncertain values, and propose changes; do not make the core solver depend on a language model.
53. **Which events may trigger recomposition?** Calendar changes, early/late completion, pause/skip, new work or deadlines, a configured daily time, and manual requests; triggers are selectable.
54. **Should stability be an explicit optimization objective?** Yes; avoid unnecessary movement.
55. **How are schedule changes communicated?** Summarize material movements and provide undo.
56. **Can a user ask why an item is in a particular slot?** Yes; provide a traceable “Why here?” explanation.
57. **Should the planner offer alternatives and what-if modes?** Yes: alternative schedules, previews, what-if simulations, and comparable trade-offs.
58. **Can goals reserve weekly time?** Yes, with minimum and maximum weekly allocations.
59. **Are daily and category caps supported?** Yes.
60. **Should fragile or likely-to-miss deadlines be flagged?** Yes.
61. **Are learned preferences controllable?** Yes: visible, editable, lockable, and resettable.
62. **How are overlapping immutable events handled?** Keep them visible as an explicit conflict rather than silently dropping or moving either event.

## 63–71 — habits

63. **Which habit schedules are needed?** Weekdays; N times per day, week, or month; every N units; completion-relative; and custom schedules.
64. **Can a habit require spacing between occurrences?** Yes.
65. **How is “N times” evaluated?** Selectable calendar period or rolling window.
66. **What duration models apply to habits?** Exact, ranged, or AI-estimated, with optional splitting.
67. **Can habit windows be hard or preferred?** Yes.
68. **Is missed-occurrence behavior configurable per habit?** Yes.
69. **Which habit analytics are required?** Streaks, statistics, trends, and supportive—not punitive—encouragement.
70. **Can a habit be paused without damaging its statistics?** Yes.
71. **Can an occurrence record a partial quantity, note, or later correction?** Yes.

## 72–84 — execution, timers, pauses, and breaks

72. **Can habits be linked to goals?** Yes.
73. **Is there one active item across all devices?** Yes; active execution state syncs.
74. **Can starting work also start a timer and optional Focus mode?** Yes.
75. **Which pause choices are supported?** Presets, custom duration, pause-until time, indefinite pause, and an optional reason.
76. **What happens when a timed break ends?** Notify and ask whether to resume, extend, or choose something else.
77. **What happens during an indefinite pause?** Tentatively push affected work and recompose definitively when the user returns.
78. **Should a break always auto-resume without asking?** No. Auto-resume may be an opt-in setting, never the only behavior.
79. **When returning, may the planner recommend an alternative to the paused item?** Yes; default to resuming it but explain better alternatives when useful.
80. **Does finishing early or late trigger adaptation?** Yes, according to configured recomposition rules.
81. **Are actual durations retained and learned from?** Yes, permanently unless the user deletes the data.
82. **Can the app detect lock, sleep, and inactivity?** Yes, with privacy-respecting platform integration.
83. **Are Pomodoro and mandatory-break rules required?** Yes.
84. **Are protected meals, work periods, and free-time breaks integrated into scheduling?** Yes; make them visible and integrate them rather than treating breaks as gaps.

## 85–96 — embedded AI and Codex

85. **Should one universal assistant cover the full application?** Yes.
86. **Should goals and projects also have persistent scoped chats?** Yes.
87. **May the assistant create and edit all item types?** Yes, subject to the confirmation policy.
88. **May it parse natural-language and voice capture?** Yes.
89. **May it infer constraints, duration, energy, and context?** Yes, while exposing the inference for correction.
90. **May it explain overloads, conflicts, and placements?** Yes.
91. **Should it conduct daily and weekly reviews?** Yes.
92. **Should assistant memory be user-controlled?** Yes: visible, editable, disableable, and deletable.
93. **May the assistant use web search?** Yes, with source visibility and applicable privacy controls.
94. **May calendar, notes, and history be sent to models?** Yes, but access must be configurable and sensitive items excluded by default.
95. **Should model and reasoning settings be exposed?** Yes, with sensible automatic defaults and an advanced override.
96. **May the assistant make proactive suggestions?** Yes; a proactive style is preferred, constrained by quiet hours and suggestion limits.

## 97–106 — external ChatGPT/Codex suggestions

97. **Should DayWeave expose a private MCP integration?** Yes.
98. **What must external assistants be able to read?** The user's schedule, within granted detail and scope.
99. **May external assistants propose changes?** Yes.
100. **Which proposal types are allowed?** Items, goal decompositions, constraints, events, full schedules, what-if results, and plain recommendations.
101. **May an external chat mutate DayWeave directly?** No; it submits a proposal to a Suggestions Inbox for review, editing, acceptance, or rejection.
102. **Is what-if simulation available externally?** Yes.
103. **What happens to stale proposals?** They expire under a configurable retention policy.
104. **How much schedule detail can an integration read?** Configurable access, per tool/integration.
105. **Does every proposal need source provenance?** Yes: source conversation/tool, explanation, and timestamp.
106. **Can DayWeave open or continue a ChatGPT/Codex conversation with selected context?** Yes.

## 107–118 — cloud, sync, storage, and operations

107. **Does Android require a private backend?** Yes.
108. **Is offline-first local storage required?** Yes; the backend remains the cross-device source of truth.
109. **Which cloud provider should host the personal service?** Nebius. Recommend one regular `cpu-e2` VM with 2 vCPU and 8 GiB RAM in `eu-north1`, using a small SSD initially and scaling only when measured demand requires it.
110. **What is the infrastructure budget?** USD 50/month including tax.
111. **Must infrastructure be designed to minimize spend?** Yes.
112. **Is self-hosted PostgreSQL acceptable?** Yes; managed PostgreSQL is outside the budget.
113. **Which backend workloads belong on the VM?** API, MCP endpoint, background worker, PostgreSQL, and an HTTPS reverse proxy/tunnel.
114. **Which data belongs in object storage?** Attachments plus encrypted backup artifacts.
115. **What backup retention is initially acceptable?** Seven days.
116. **Should DayWeave hold copies of attachments rather than only references?** Yes, except deliberately external large-file links.
117. **Is near-real-time multi-device sync required?** Yes.
118. **Must sync expose status, history, conflict resolution, and undo?** Yes.

## 119–128 — privacy, security, backup, and observability

119. **Should the system avoid spending more when a smaller resource works?** Yes; measure before scaling.
120. **Must data be encrypted in transit and at rest?** Yes, with application-level protection for especially sensitive fields.
121. **Is true user-held-key end-to-end encryption required initially?** No; strong server-side/application-layer protection is acceptable because server features need authorized access.
122. **Is biometric app lock required?** Yes, with configurable automatic locking.
123. **Is a per-item sensitive flag required?** Yes.
124. **How are sensitive items treated while locked or exported externally?** Hide content in locked notifications/widgets and exclude it from external MCP, proactive AI, and attachment analysis unless explicitly allowed.
125. **Which export formats are needed?** Encrypted backup, JSON, CSV, ICS, and Markdown.
126. **Can sessions be reviewed and revoked remotely?** Yes; accounts can also be deleted and reconnected.
127. **Are anonymous crash and performance diagnostics allowed?** Yes, provided they contain no user content.
128. **Is product analytics enabled by default?** No; keep it off by default.

## 129–140 — notifications and OS integration

129. **Are actionable notifications required?** Yes.
130. **Do notification actions include start, done, pause, skip, later, and replan?** Yes.
131. **Should dismissals synchronize across devices?** Yes.
132. **Do AI notifications obey quiet hours, urgency, and daily caps?** Yes.
133. **May schedule categories integrate with Focus/DND?** Yes.
134. **Are macOS widgets required?** Yes.
135. **Are Android home-screen widgets required?** Yes.
136. **Does Android need a persistent active-timer notification?** Yes.
137. **Is WHOOP part of initial completion?** No; it is a planned extension after the complete core product.
138. **Is Android Health Connect integration required?** Yes.
139. **Is a manual energy check-in required?** Yes.
140. **Should weather influence outdoor work suggestions?** Yes.

## 141–149 — Nebius deployment and data protection

141. **May the existing local Nebius CLI profile be used?** Yes.
142. **Should routine deploys and migrations be automated?** Yes.
143. **Should the personal login bootstrap least-privilege service accounts?** Yes.
144. **Must runtime and deployment credentials be project-scoped and revocable?** Yes.
145. **Must credentials stay out of Git?** Yes.
146. **Are continuous/incremental encrypted backups plus daily snapshots required?** Yes.
147. **Where should ambiguous captures and external proposals land?** A unified Inbox.
148. **Are uptime, backup, budget, authentication, and certificate alerts required?** Yes.
149. **Should alerts be delivered both by email and in-app?** Yes.

## 150–168 — ownership, distribution, identifiers, CI, and release

150. **How may Nebius be accessed for setup?** Through the existing local profile as the owner's user; bootstrap scoped service accounts afterward.
151. **Where should the private GitHub repository live?** Under the owner's personal `greengolddog` account.
152. **Is an Apple Developer Program membership available?** No.
153. **Is a Google Play Console account available?** No.
154. **Which client is built first?** macOS first.
155. **Should local development and release automation both be supported?** Yes, both.
156. **How must public service traffic be exposed?** HTTPS.
157. **Which Google Cloud project should be used?** Configure a new dedicated project.
158. **Should CI secrets and infrastructure configuration be kept securely with the private repository/project?** Yes.
159. **How long should autonomous construction continue?** Until the complete product is created; there is no abbreviated demo stopping point.
160. **Will the owner assist when a login, OAuth consent, or physical-device step is truly required?** Yes.
161. **Should lack of Apple membership block a local macOS build?** No.
162. **Should lack of Play Console block an installable Android build?** No.
163. **Are a local macOS app build and signed APK acceptable?** Yes.
164. **Should the initial macOS build be locally signed/unnotarized?** Yes.
165. **Should Android use a securely held signing key for direct APK installation and updates?** Yes.
166. **Are development, beta, and stable channels required?** Yes.
167. **Is GitHub Actions CI required?** Yes.
168. **Should stable bundle/package IDs be chosen now?** Yes: use `com.greengolddog.dayweave` and platform-specific suffixes only where necessary.

## 169–180 — Google Calendar and temporal behavior

169. **Is Google Calendar bidirectional sync required?** Yes.
170. **Should the first account work while the model supports multiple accounts?** Yes.
171. **Can each calendar be hidden, visible, read-only, blocking, or writable?** Yes.
172. **Should DayWeave create a dedicated writable calendar?** Yes.
173. **What happens when a DayWeave block is moved in Google Calendar?** Update the DayWeave schedule and treat the explicit placement as pinned until changed.
174. **What happens when a DayWeave block is deleted in Google Calendar?** Unschedule the underlying item without deleting it.
175. **Are imported external events fixed by default?** Yes; app-created private events may be configured as flexible.
176. **Must recurring events, attendees, RSVP, conferencing, attachments, time zones, and DST be supported?** Yes.
177. **How are birthdays and observances treated?** Show them, normally nonblocking, and do not manufacture birthday tasks automatically.
178. **How are vacation and out-of-office events treated?** As availability-blocking events.
179. **How are declined, free, tentative, and other all-day events treated?** Declined is ignored; free is visible/nonblocking; tentative and ordinary all-day behavior are configurable.
180. **Do external changes sync in the background and trigger recomposition?** Yes.

## 181–192 — Google Tasks, offline sync, time, travel, and location

181. **Is Google Tasks integration required?** Yes.
182. **Can the user select which Google Task lists sync?** Yes.
183. **Should app tasks sync supported fields while rich constraints remain in DayWeave?** Yes.
184. **Should completion propagate immediately in both directions?** Yes.
185. **What happens when a synced task is deleted externally?** Move the DayWeave task to its recoverable trash.
186. **What happens when an external due date changes?** Update constraints and recompose.
187. **Where do Google Tasks without enough scheduling information go?** The unified Inbox.
188. **Does completing a synced task remove its remaining calendar blocks?** Yes.
189. **Do Google Tasks use the same offline queue, conflict handling, and audit trail?** Yes.
190. **What is the initial time-zone behavior?** Start in Europe/Madrid, detect travel automatically, and allow each item to be absolute-time or floating-time.
191. **What locale conventions apply?** Monday-first weeks and 24-hour time.
192. **Are travel profiles and route modes required?** Yes: driving, public transit, walking, and cycling.

## 193–202 — external effects, meetings, safety, and capture

193. **May DayWeave create or edit events with attendees?** Yes, but only after explicit approval.
194. **Are attendee events automatically movable?** No; only private flexible events may move automatically. Attendee-event changes require confirmation.
195. **Must external effects show a preview before confirmation?** Yes.
196. **Can RSVP be handled inside DayWeave?** Yes.
197. **May AI suggest meeting preparation or follow-up work?** Yes, only when useful and with approval before creation.
198. **Should attachments be sent to AI automatically?** Only when relevant and permitted by the item's privacy settings.
199. **Is one-click conference joining required?** Yes.
200. **Which recurrence edit scopes are required?** This occurrence, this and following, and the whole series.
201. **Are visibility and availability controls required on events?** Yes.
202. **Are warnings required for attendee conflicts, travel feasibility, and meeting density?** Yes.

## 203–212 — hierarchy, dependencies, lifecycle, and retention

203. **Which nodes occupy schedule time in a hierarchy?** Only leaf tasks.
204. **Do parent durations and progress roll up from descendants?** Yes.
205. **Can a parent also carry an independent measurable component?** Yes.
206. **Does a parent auto-complete when all required children complete?** Yes.
207. **Can parent completion be manually overridden?** Yes.
208. **Are typed dependencies required?** Yes: finish-to-start and other useful temporal types.
209. **Can dependencies be hard or soft?** Yes.
210. **Do blocked states and dependency causes remain visible?** Yes.
211. **Does ordinary deletion go to recoverable trash?** Yes, for thirty days.
212. **Are completed and canceled records retained and searchable indefinitely?** Yes.

## 213–220 — capture, search, files, onboarding, and background operation

213. **Should voice capture prefer on-device transcription?** Yes, with an OpenAI fallback when configured.
214. **What happens to a voice recording after transcription?** Delete it by default, with an explicit retain option.
215. **What is the default attachment limit?** 50 MB; larger files can remain external links.
216. **Are OCR, text extraction, attachment search, and relevant AI analysis required?** Yes.
217. **Are URL metadata, previews, and optional snapshots required?** Yes.
218. **Should duplicate detection offer a merge rather than silently deduplicate?** Yes.
219. **Is guided onboarding required?** Yes, covering account, calendars, availability, privacy, notifications, and initial planning.
220. **Is an optional synthetic demo workspace required?** Yes.

## 221–228 — UX details and completion gates

221. **What is the default launch view?** Today; remember a deliberate switch to another view.
222. **What is the macOS layout?** Sidebar, central timeline/content, and a right inspector/assistant panel.
223. **What is the Android navigation?** Bottom navigation for Today, Calendar, Inbox, and Assistant, plus More.
224. **Which themes are required?** System light/dark, manual override, accent choice, and configurable colors by item type/project/calendar/priority.
225. **What is the timeline granularity?** Fifteen minutes by default, zoomable from five-minute detail to a full-day overview.
226. **How are completed items shown?** Visible and dimmed by default, with a hide option.
227. **Are full search, semantic search, command palette, and configurable keyboard shortcuts required?** Yes.
228. **Must the complete product pass an extended daily-use trial?** Yes: a seven-day real-life trial, with all critical and major issues fixed before completion.

## 229–238 — final implementation and acceptance decisions

229. **What Android version is installed on the Pixel 11?** Unknown; detect it from the physical device with ADB when available.
230. **Should tests use dedicated Google calendars and task lists?** Yes.
231. **Should real-account integration wait until mocks and isolated tests pass?** Yes.
232. **Are scheduler unit and property tests required?** Yes.
233. **Are database migration and integration-contract tests required?** Yes.
234. **Are Google, Codex, and MCP contract tests required?** Yes.
235. **Are offline/conflict sync and UI automation required?** Yes.
236. **Are end-to-end deployment, backup, and restore tests required?** Yes.
237. **Are performance and security tests required?** Yes.
238. **How autonomously should construction proceed now?** Build as complete a product as possible without waiting; the owner may leave it running overnight. Use mocks and synthetic data for gated integrations and stop only at genuinely user-only authentication or physical-device actions.

## Cross-cutting decisions that apply to the full ledger

- **AI login:** use the supported Codex App Server path with managed ChatGPT browser/device-code login as the primary experience and an API-key fallback. Codex identity remains separate from the Google application identity.
- **Google identity:** start with one allowlisted Google account, while preserving multi-account data structures.
- **Confirmation policy:** harmless single-item changes may execute with undo; bulk changes and recomposition show a preview; deletion, deadline relaxation, and external-calendar effects always require confirmation.
- **Offline contract:** local planning, viewing, timers, and edits continue offline. Google, sync, and optional AI actions queue with visible status.
- **Google notification source:** notification ownership may be app, Google, or both, configurable by category.
- **Background behavior:** macOS uses a launch-at-login helper where needed; Android uses normal background and push mechanisms.
- **Network scope:** the product is allowed to use full remote AI and Google services under explicit account/privacy controls.
- **License:** publicly visible and proprietary; no open-source license grant.
- **Performance targets:** cold launch under 2 seconds on target hardware; smooth 60 fps interaction; typical day recomposition under 1 second; complex 90-day recomposition under 10 seconds; online changes visible across devices within 10 seconds under normal connectivity.
- **Documentation:** architecture, setup, operations, recovery, security, and user documentation are part of the product—not optional handoff work.
- **Definition of complete:** backend, polished macOS and Android clients, deterministic scheduler, Google Calendar and Tasks, embedded Codex assistant, external MCP/skill integration, deployment, automated tests, documentation, and the seven-day trial. WHOOP is explicitly deferred; Android Health Connect is not.

## Post-discovery repository update

239. **What is the repository visibility now?** The owner changed the DayWeave
     GitHub repository from private to public. This supersedes only the earlier
     visibility choice; the product remains publicly visible and proprietary.
     No private credential, secret, token, key, signed credential bundle, or
     real-account fixture may ever be committed or pushed. Runtime and CI
     credentials must remain externally injected, project-scoped, and
     revocable.
