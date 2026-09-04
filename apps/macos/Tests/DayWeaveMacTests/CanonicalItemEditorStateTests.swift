import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical item editor state")
struct CanonicalItemEditorStateTests {
    @Test("new drafts use the injected profile timezone while edits retain their own")
    func testProfileTimezoneInjection() {
        let newState = CanonicalItemEditorState(
            itemID: Self.id(40),
            timezoneName: "America/New_York"
        )
        let invalidProfileState = CanonicalItemEditorState(
            itemID: Self.id(41),
            timezoneName: "PST"
        )
        let existing = DayWeaveCanonicalItemDraft(
            title: "Existing timezone",
            timezoneName: "Europe/Madrid"
        )
        let editedState = CanonicalItemEditorState(
            itemID: Self.id(42),
            draft: existing,
            timezoneName: "America/New_York"
        )

        #expect(newState.timezoneName == "America/New_York")
        #expect(invalidProfileState.timezoneName == "UTC")
        #expect(editedState.timezoneName == "Europe/Madrid")
        #expect(CanonicalItemEditorState.defaultTimezoneName == "UTC")
    }

    @Test("title-only capture creates an unscheduled Inbox item")
    func testTitleOnlyCapture() {
        var state = CanonicalItemEditorState(
            itemID: Self.id(1),
            now: Self.date("2026-08-30T09:00:00Z"),
            timezoneName: "Europe/Madrid"
        )
        state.title = "  Read the saved article  "

        let draft = state.draft

        #expect(state.validationIssue == nil)
        #expect(draft.title == "Read the saved article")
        #expect(draft.status == .inbox)
        #expect(draft.kind == .task)
        #expect(draft.durationSeconds == nil)
        #expect(draft.recurrence == nil)
        #expect(draft.flexibleConstraints == .object([:]))
        #expect(draft.splitPolicy == .indivisible)
    }

    @Test("typed recurrence controls emit every supported simple contract")
    func testTypedRecurrenceContracts() {
        let itemID = Self.id(2)
        var state = CanonicalItemEditorState(
            itemID: itemID,
            timezoneName: "UTC"
        )
        state.title = "Practice"
        state.kind = .habit

        state.recurrence = .daily
        state.recurrenceCount = 2
        #expect(state.draft.recurrence == .object([
            "type": .string("daily"),
            "times_per_day": .number(JSONNumber(UInt64(2))),
        ]))

        state.recurrence = .weekly
        state.recurrenceCount = 3
        state.weekdays = [.monday, .wednesday, .friday]
        #expect(state.draft.recurrence == .object([
            "type": .string("weekly"),
            "times_per_week": .number(JSONNumber(UInt64(3))),
            "weekdays": .array([
                .string("monday"), .string("wednesday"), .string("friday"),
            ]),
        ]))

        state.recurrence = .monthly
        state.recurrenceCount = 4
        #expect(state.draft.recurrence == .object([
            "type": .string("monthly"),
            "times_per_month": .number(JSONNumber(UInt64(4))),
        ]))

        state.recurrence = .everyInterval
        state.recurrenceIntervalMinutes = 180
        #expect(state.draft.recurrence == .object([
            "type": .string("every_interval"),
            "interval": .number(JSONNumber(UInt64(180))),
        ]))

        state.recurrence = .afterCompletion
        state.recurrenceIntervalMinutes = 1_440
        #expect(state.draft.recurrence == .object([
            "type": .string("after_completion"),
            "interval": .number(JSONNumber(UInt64(1_440))),
        ]))
        #expect(state.validationIssue == nil)
    }

    @Test("frequency and qualified constraint controls emit the composer contract")
    func testRichConstraintContract() throws {
        var state = CanonicalItemEditorState(
            itemID: Self.id(50),
            now: Self.date("2026-09-03T06:00:00Z"),
            timezoneName: "Europe/Paris"
        )
        state.title = "Write proposal"
        state.readyStatus = .planned
        state.hasDuration = true
        state.durationSeconds = 7_200
        state.recurrence = .frequency
        state.recurrenceCount = 3
        state.recurrencePeriod = .week
        state.recurrenceSemantics = .calendar
        state.weekdays = [.monday, .wednesday, .friday]
        state.recurrenceMinimumSpacingMinutes = 1_440
        state.hasEarliestStart = true
        state.earliestStart = Self.date("2026-09-03T06:00:00Z")
        state.earliestStartStrength = .soft
        state.earliestStartSoftWeight = 250
        state.hasDeadline = true
        state.deadline = Self.date("2026-09-30T16:00:00Z")
        state.deadlineStrength = .hard
        state.hasMinimumNotice = true
        state.minimumNoticeMinutes = 30
        state.hasAllowedWeekdays = true
        state.allowedWeekdays = [.monday, .tuesday, .wednesday, .thursday, .friday]
        state.preferredDailyWindows = [.init(
            weekdays: [.monday, .wednesday],
            startMinute: 540,
            endMinute: 720,
            strength: .soft,
            softWeight: 125
        )]
        state.preferredAbsoluteWindows = [.init(
            start: Self.date("2026-09-07T07:00:00Z"),
            end: Self.date("2026-09-07T10:00:00Z"),
            strength: .soft,
            softWeight: 75
        )]
        state.forbiddenWindows = [.init(
            start: Self.date("2026-09-09T10:00:00Z"),
            end: Self.date("2026-09-09T11:00:00Z"),
            strength: .hard
        )]
        state.requiredContexts = [
            .init(value: "computer", strength: .hard),
            .init(value: "quiet", strength: .soft, softWeight: 50),
        ]
        state.hasRequiredLocation = true
        state.requiredLocation = "home"
        state.requiredLocationStrength = .soft
        state.requiredLocationSoftWeight = 40
        state.hasBuffers = true
        state.bufferBeforeMinutes = 10
        state.bufferAfterMinutes = 15
        state.bufferStrength = .soft
        state.bufferSoftWeight = 90
        state.hasMaximumDailyWork = true
        state.maximumDailyWorkMinutes = 180
        state.hasMaximumWeeklyWork = true
        state.maximumWeeklyWorkMinutes = 480
        state.maximumWeeklyWorkStrength = .soft
        state.maximumWeeklyWorkSoftWeight = 60
        state.energy = .deep
        state.energyStrength = .hard
        state.tags = [.init(value: "focus"), .init(value: "writing")]
        state.hasPreferredStartMinute = true
        state.preferredStartMinute = 600
        state.isSplittable = true
        state.minimumChunkSeconds = 1_800
        state.maximumChunkSeconds = 3_600
        state.hasMaximumSessions = true
        state.maximumSessions = 3
        state.minimumGapMinutes = 30
        state.hasMaximumSplitDays = true
        state.maximumSplitDays = 2

        let draft = state.draft
        #expect(state.validationIssue == nil)
        #expect(draft.earliestStartAt == nil)
        #expect(draft.deadlineAt == Self.date("2026-09-30T16:00:00Z"))
        #expect(draft.recurrence == .object([
            "type": .string("frequency"),
            "target": .number(JSONNumber(UInt64(3))),
            "period": .string("week"),
            "semantics": .string("calendar"),
            "weekdays": .array([.string("monday"), .string("wednesday"), .string("friday")]),
            "minimum_spacing": .number(JSONNumber(UInt64(1_440))),
        ]))
        guard case let .object(metadata) = draft.flexibleConstraints,
              case let .object(constraints)? = metadata["constraints"] else {
            Issue.record("Expected typed scheduling metadata")
            return
        }
        #expect(metadata["energy"] == .object([
            "value": .string("deep"),
            "strength": .object(["level": .string("hard")]),
        ]))
        #expect(metadata["tags"] == .array([.string("focus"), .string("writing")]))
        #expect(metadata["preferred_start_minute"] == .number(JSONNumber(UInt64(600))))
        #expect(metadata["maximum_sessions"] == .number(JSONNumber(UInt64(3))))
        #expect(metadata["minimum_gap_minutes"] == .number(JSONNumber(UInt64(30))))
        #expect(metadata["maximum_split_days"] == .number(JSONNumber(UInt64(2))))
        #expect(constraints["earliest_start"] != nil)
        #expect(constraints["latest_finish"] == nil)
        #expect(constraints["preferred_daily_windows"] != nil)
        #expect(constraints["preferred_absolute_windows"] != nil)
        #expect(constraints["forbidden_windows"] != nil)
        #expect(constraints["required_contexts"] != nil)
        #expect(constraints["required_location"] != nil)
        #expect(constraints["buffers"] != nil)

        let reopened = CanonicalItemEditorState(itemID: state.itemID, draft: draft)
        #expect(reopened.readOnlyDiagnostic == nil)
        #expect(reopened.hasPreferredStartMinute)
        #expect(reopened.preferredStartMinute == 600)
        #expect(reopened.draft == draft)
    }

    @Test("unrelated edits preserve whitespace in server-valid rich strings")
    func testRichStringsRemainVerbatim() throws {
        let taskMetadata: JSONValue = .object([
            "constraints": .object([
                "required_contexts": .array([
                    .object([
                        "value": .string("  desk, focused  "),
                        "strength": .object(["level": .string("hard")]),
                    ]),
                ]),
                "required_location": .object([
                    "value": .string("  north room  "),
                    "strength": .object(["level": .string("hard")]),
                ]),
            ]),
        ])
        var task = CanonicalItemEditorState(
            itemID: Self.id(71),
            draft: .init(
                title: "Context task",
                timezoneName: "UTC",
                flexibleConstraints: taskMetadata
            )
        )
        task.title = "Renamed context task"
        #expect(task.validationIssue == nil)
        #expect(task.draft.flexibleConstraints == taskMetadata)

        let habitMetadata: JSONValue = .object([
            "habit_target": .object([
                "amount": .number(JSONNumber(UInt64(2))),
                "unit": .string("  glasses  "),
            ]),
        ])
        var habit = CanonicalItemEditorState(
            itemID: Self.id(72),
            draft: .init(
                kind: .habit,
                title: "Hydrate",
                timezoneName: "UTC",
                flexibleConstraints: habitMetadata
            )
        )
        habit.title = "Hydrate today"
        #expect(habit.validationIssue == nil)
        #expect(habit.draft.flexibleConstraints == habitMetadata)

        let goalMetadata: JSONValue = .object([
            "goal_measures": .array([
                .object([
                    "name": .string("  chapters  "),
                    "target": .number(JSONNumber(integerLiteral: 10)),
                    "current": .number(JSONNumber(integerLiteral: 2)),
                    "unit": .string("  pages, reviewed  "),
                ]),
            ]),
        ])
        var goal = CanonicalItemEditorState(
            itemID: Self.id(73),
            draft: .init(
                kind: .goal,
                title: "Read",
                timezoneName: "UTC",
                flexibleConstraints: goalMetadata
            )
        )
        goal.title = "Read more"
        #expect(goal.validationIssue == nil)
        #expect(goal.draft.flexibleConstraints == goalMetadata)
    }

    @Test("habit routine goal and break details are typed and kind-scoped")
    func testKindSpecificMetadataContracts() throws {
        var habit = CanonicalItemEditorState(itemID: Self.id(51), timezoneName: "UTC")
        habit.title = "Hydrate"
        habit.kind = .habit
        habit.normalizeForKindChange()
        habit.hasHabitTarget = true
        habit.habitTargetAmount = 8
        habit.habitTargetUnit = "glasses"
        habit.preservesStreakWhenPaused = false
        #expect(habit.validationIssue == nil)
        guard case let .object(habitMetadata) = habit.draft.flexibleConstraints else {
            Issue.record("Expected habit metadata")
            return
        }
        #expect(habitMetadata["habit_target"] == .object([
            "amount": .number(JSONNumber(UInt64(8))),
            "unit": .string("glasses"),
        ]))
        #expect(habitMetadata["preserves_streak_when_paused"] == .bool(false))

        var routine = CanonicalItemEditorState(itemID: Self.id(52), timezoneName: "UTC")
        routine.title = "Shutdown"
        routine.kind = .routine
        routine.normalizeForKindChange()
        routine.routineOrdered = true
        guard case let .object(routineMetadata) = routine.draft.flexibleConstraints else {
            Issue.record("Expected routine metadata")
            return
        }
        #expect(routineMetadata["routine_ordered"] == .bool(true))
        #expect(routineMetadata["has_own_effort"] == nil)

        var goal = CanonicalItemEditorState(itemID: Self.id(53), timezoneName: "UTC")
        goal.title = "Read books"
        goal.kind = .goal
        goal.normalizeForKindChange()
        goal.hasOwnEffort = true
        goal.hasDuration = true
        goal.durationSeconds = 3_600
        goal.goalMeasures = [.init(name: "chapters", target: 12, current: 3, unit: "chapters")]
        goal.hasGoalWeeklyAllocation = true
        goal.goalWeeklyMinimumMinutes = 120
        goal.hasGoalWeeklyMaximum = true
        goal.goalWeeklyMaximumMinutes = 300
        guard case let .object(goalMetadata) = goal.draft.flexibleConstraints else {
            Issue.record("Expected goal metadata")
            return
        }
        #expect(goalMetadata["has_own_effort"] == .bool(true))
        #expect(goalMetadata["goal_measures"] != nil)
        #expect(goalMetadata["goal_weekly_allocation"] == .object([
            "minimum": .number(JSONNumber(UInt64(120))),
            "maximum": .number(JSONNumber(UInt64(300))),
        ]))

        var rest = CanonicalItemEditorState(itemID: Self.id(54), timezoneName: "UTC")
        rest.title = "Walk"
        rest.kind = .breakTime
        rest.normalizeForKindChange()
        rest.breakCategory = .movement
        rest.breakMandatory = true
        rest.breakPromptToResume = false
        guard case let .object(breakMetadata) = rest.draft.flexibleConstraints else {
            Issue.record("Expected break metadata")
            return
        }
        #expect(breakMetadata["break_category"] == .string("movement"))
        #expect(breakMetadata["break_mandatory"] == .bool(true))
        #expect(breakMetadata["break_prompt_to_resume"] == .bool(false))
    }

    @Test("event controls emit exact blocking metadata and duration")
    func testEventMetadata() throws {
        var state = CanonicalItemEditorState(
            itemID: Self.id(3),
            now: Self.date("2026-08-30T09:00:00Z"),
            timezoneName: "Europe/Madrid"
        )
        state.title = "Dentist"
        state.kind = .event
        state.normalizeForKindChange()
        state.eventStart = Self.date("2026-09-02T08:15:00Z")
        state.eventEnd = Self.date("2026-09-02T09:45:00Z")
        state.eventIsImmutable = true
        state.eventIsAllDay = false

        let draft = state.draft

        #expect(state.validationIssue == nil)
        #expect(draft.durationSeconds == 5_400)
        #expect(draft.earliestStartAt == Self.date("2026-09-02T08:15:00Z"))
        #expect(draft.deadlineAt == Self.date("2026-09-02T09:45:00Z"))
        #expect(draft.splitPolicy == .indivisible)
        #expect(draft.recurrence == nil)
        guard case let .object(constraints) = draft.flexibleConstraints,
              case let .object(event)? = constraints["dayweave_firm_block"] else {
            Issue.record("Expected strict owned firm-block metadata")
            return
        }
        #expect(Set(constraints.keys) == ["dayweave_firm_block"])
        #expect(event["owned"] == .bool(true))
        #expect(event["starts_at"] == .string("2026-09-02T08:15:00.000Z"))
        #expect(event["ends_at"] == .string("2026-09-02T09:45:00.000Z"))
        #expect(event["all_day"] == .bool(false))
        #expect(event["tentative"] == .bool(false))
        #expect(event["busy"] == .bool(true))
    }

    @Test("all-day controls normalize to DST-safe exclusive local dates")
    func testAllDayEventNormalization() throws {
        var state = CanonicalItemEditorState(
            itemID: Self.id(30),
            now: Self.date("2026-03-29T10:00:00Z"),
            timezoneName: "Europe/Madrid"
        )
        state.title = "DST Sunday"
        state.kind = .event
        state.normalizeForKindChange()
        state.setEventIsAllDay(true)

        #expect(state.eventStart == Self.date("2026-03-28T23:00:00Z"))
        #expect(state.eventEnd == Self.date("2026-03-29T22:00:00Z"))
        #expect(try #require(state.draft.durationSeconds) == UInt32(23 * 60 * 60))
        #expect(state.validationIssue == nil)

        state.setEventStart(Self.date("2026-10-25T12:00:00Z"))
        #expect(state.eventStart == Self.date("2026-10-24T22:00:00Z"))
        #expect(state.eventEnd == Self.date("2026-10-25T23:00:00Z"))
        #expect(try #require(state.draft.durationSeconds) == UInt32(25 * 60 * 60))

        state.setEventEnd(state.eventStart)
        #expect(state.eventEnd == Self.date("2026-10-25T23:00:00Z"))
        guard case let .object(constraints) = state.draft.flexibleConstraints,
              case let .object(event)? = constraints["dayweave_firm_block"] else {
            Issue.record("Expected normalized all-day metadata")
            return
        }
        #expect(event["starts_at"] == .string("2026-10-24T22:00:00.000Z"))
        #expect(event["ends_at"] == .string("2026-10-25T23:00:00.000Z"))
        #expect(event["all_day"] == .bool(true))

        state.eventStart = Self.date("2026-10-25T12:00:00Z")
        #expect(state.validationIssue?.contains("local midnight") == true)
    }

    @Test("changing an all-day timezone preserves its local calendar dates")
    func testAllDayTimezoneChangePreservesDates() throws {
        var state = CanonicalItemEditorState(
            itemID: Self.id(31),
            now: Self.date("2026-03-29T10:00:00Z"),
            timezoneName: "Europe/Madrid"
        )
        state.title = "One local day"
        state.kind = .event
        state.normalizeForKindChange()
        state.setEventIsAllDay(true)

        state.setTimezoneName("America/New_York")

        #expect(state.timezoneName == "America/New_York")
        #expect(state.eventStart == Self.date("2026-03-29T04:00:00Z"))
        #expect(state.eventEnd == Self.date("2026-03-30T04:00:00Z"))
        #expect(state.validationIssue == nil)
    }

    @Test("incremental timezone typing preserves the last valid all-day dates")
    func testAllDayTimezoneTypingPreservesDates() {
        var state = CanonicalItemEditorState(
            itemID: Self.id(32),
            now: Self.date("2026-08-30T10:00:00Z"),
            timezoneName: "Europe/Madrid"
        )
        state.title = "Local Sunday"
        state.kind = .event
        state.normalizeForKindChange()
        state.setEventIsAllDay(true)

        var typed = ""
        for character in "America/New_York" {
            typed.append(character)
            state.setTimezoneName(typed)
        }

        #expect(state.timezoneName == "America/New_York")
        #expect(state.eventStart == Self.date("2026-08-30T04:00:00Z"))
        #expect(state.eventEnd == Self.date("2026-08-31T04:00:00Z"))
        #expect(state.validationIssue == nil)
    }

    @Test("new routine and goal duration can explicitly contribute own effort")
    func testContainerOwnEffortContract() {
        for kind in [DayWeaveCanonicalItemKind.routine, .goal] {
            var state = CanonicalItemEditorState(
                itemID: UUID(),
                timezoneName: "UTC"
            )
            state.title = "Container work"
            state.kind = kind
            state.normalizeForKindChange()
            state.readyStatus = .planned
            state.hasDuration = true
            state.durationSeconds = 1_800
            if kind == .routine { state.recurrence = .daily }

            #expect(!state.hasOwnEffort)
            state.hasOwnEffort = true

            guard case let .object(constraints) = state.draft.flexibleConstraints else {
                Issue.record("Expected typed container constraints")
                continue
            }
            #expect(constraints["has_own_effort"] == .bool(true))
            #expect(state.validationIssue == nil)

            state.hasOwnEffort = false
            guard case let .object(childOnly) = state.draft.flexibleConstraints else {
                Issue.record("Expected typed child-only constraints")
                continue
            }
            #expect(childOnly["has_own_effort"] == nil)
        }
    }

    @Test("editing energy preserves supported constraints outside the form")
    func testRetainedConstraints() {
        let source = DayWeaveCanonicalItemDraft(
            kind: .routine,
            status: .planned,
            title: "Morning sequence",
            notes: nil,
            timezoneName: "UTC",
            durationSeconds: 2_400,
            flexibleConstraints: .object([
                "energy": .string("low"),
                "has_own_effort": .bool(true),
                "routine_ordered": .bool(true),
            ])
        )
        var state = CanonicalItemEditorState(itemID: Self.id(4), draft: source)
        state.energy = .deep

        #expect(state.draft.flexibleConstraints == .object([
            "energy": .string("deep"),
            "has_own_effort": .bool(true),
            "routine_ordered": .bool(true),
        ]))
        #expect(state.validationIssue == nil)
    }

    @Test("opening an event does not normalize untouched wire metadata")
    func testUntouchedEventRoundTrip() {
        let source = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Untouched event",
            notes: nil,
            timezoneName: "UTC",
            durationSeconds: nil,
            flexibleConstraints: .object([
                "calendar_event": .object([
                    "start": .string("2026-09-02T08:15:00Z"),
                    "end": .string("2026-09-02T09:15:00Z"),
                    "immutable": .bool(true),
                    "all_day": .bool(false),
                ]),
            ])
        )

        let state = CanonicalItemEditorState(itemID: Self.id(8), draft: source)

        #expect(state.readOnlyDiagnostic?.contains("source calendar") == true)
        #expect(state.draft == source)
    }

    @Test("custom recurrence is retained read-only while calendar bindings fail read-only")
    func testReadOnlyDiagnostics() {
        let custom = DayWeaveCanonicalItemDraft(
            kind: .task,
            status: .planned,
            title: "Custom cadence",
            notes: nil,
            timezoneName: "UTC",
            recurrence: .object([
                "type": .string("custom"),
                "rrule": .string("FREQ=YEARLY"),
            ])
        )
        let customState = CanonicalItemEditorState(itemID: Self.id(5), draft: custom)
        #expect(customState.readOnlyDiagnostic?.contains("recurrence") == true)
        #expect(customState.recurrence == .custom)
        #expect(customState.customRecurrenceRule == "FREQ=YEARLY")
        #expect(customState.draft == custom)
        #expect(!CanonicalItemEditorRecurrence.authorableCases.contains(.custom))

        let linkedEvent = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Calendar event",
            notes: nil,
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "calendar_event": .object([
                    "start": .string("2026-09-02T08:15:00Z"),
                    "end": .string("2026-09-02T09:15:00Z"),
                    "immutable": .bool(true),
                    "all_day": .bool(false),
                    "source_calendar_id": .string("primary"),
                ]),
            ])
        )
        let eventState = CanonicalItemEditorState(itemID: Self.id(6), draft: linkedEvent)
        #expect(eventState.readOnlyDiagnostic?.contains("source calendar") == true)
    }

    @Test("incomplete Inbox timing stays optional and structural authority stays read-only")
    func testInboxAndStructuralReadOnlyBoundaries() {
        let inboxEvent = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .inbox,
            title: "Unclear appointment",
            timezoneName: "Europe/Paris"
        )
        var inboxState = CanonicalItemEditorState(itemID: Self.id(55), draft: inboxEvent)
        #expect(inboxState.readOnlyDiagnostic == nil)
        #expect(!inboxState.hasEventTiming)
        #expect(inboxState.draft == inboxEvent)
        inboxState.readyStatus = .planned
        #expect(inboxState.validationIssue?.contains("timing") == true)

        let linkedGoal = DayWeaveCanonicalItemDraft(
            status: .planned,
            title: "Linked elsewhere",
            timezoneName: "UTC",
            durationSeconds: 1_800,
            flexibleConstraints: .object([
                "goal_ids": .array([.string("00000000-0000-0000-0000-000000000180")]),
            ])
        )
        let linkedState = CanonicalItemEditorState(itemID: Self.id(56), draft: linkedGoal)
        #expect(linkedState.readOnlyDiagnostic != nil)
        #expect(linkedState.draft == linkedGoal)

        let normalizedNullWindow = DayWeaveCanonicalItemDraft(
            title: "Unmaterialized task",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object(["occurrence_window": .null]),
            ])
        )
        let normalizedState = CanonicalItemEditorState(
            itemID: Self.id(57),
            draft: normalizedNullWindow
        )
        #expect(normalizedState.readOnlyDiagnostic == nil)
        #expect(normalizedState.draft.flexibleConstraints == .object([
            "constraints": .object(["occurrence_window": .null]),
        ]))
    }

    @Test("incomplete event metadata stays visible until an explicit clear permits timing")
    func testIncompleteEventMetadataRequiresExplicitClear() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let source = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .inbox,
            title: "Candidate meeting",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "buffers": .object([
                        "before": .number(JSONNumber(UInt64(5))),
                        "after": .number(JSONNumber(UInt64(0))),
                        "strength": .null,
                    ]),
                    "occurrence_window": .null,
                ]),
                "energy": .string("low"),
                "has_own_effort": .bool(false),
                "tags": .array([.string("family,calendar")]),
            ])
        )
        var state = CanonicalItemEditorState(itemID: Self.id(70), draft: source, now: now)

        #expect(state.readOnlyDiagnostic == nil)
        #expect(!state.hasEventTiming)
        #expect(state.hasEventFlexibleMetadata)
        #expect(state.eventFlexibleMetadataPresentation?.summary
            == "Retained fields: constraints, energy, has_own_effort, tags")
        #expect(state.eventFlexibleMetadataPresentation?.details.contains("family,calendar") == true)

        state.title = "Candidate meeting, reviewed"
        var ordinaryEdit = source
        ordinaryEdit.title = state.title
        #expect(state.draft == ordinaryEdit)

        let blockedTimingChange = state.setEventTimingEnabled(true)
        #expect(!blockedTimingChange)
        #expect(!state.hasEventTiming)
        state.hasEventTiming = true
        #expect(state.validationIssue
            == "Clear the retained flexible metadata before setting exact event timing.")
        #expect(state.draft.flexibleConstraints == source.flexibleConstraints)
        #expect(state.draft.earliestStartAt == nil)
        #expect(state.draft.deadlineAt == nil)

        let disabledTiming = state.setEventTimingEnabled(false)
        #expect(disabledTiming)
        state.clearEventFlexibleMetadata()
        #expect(!state.hasEventFlexibleMetadata)
        #expect(state.eventFlexibleMetadataPresentation == nil)
        let enabledTiming = state.setEventTimingEnabled(true)
        #expect(enabledTiming)
        state.readyStatus = .planned
        #expect(state.validationIssue == nil)
        guard case let .object(metadata) = state.draft.flexibleConstraints else {
            Issue.record("Expected owned event timing after explicit metadata clear")
            return
        }
        #expect(Set(metadata.keys) == ["dayweave_firm_block"])
    }

    @Test("shared scheduling metadata fixtures expose dependencies while retaining goal associations")
    func testSharedSchedulingMetadataFixtures() throws {
        let daily = try Self.fixtureFields(named: "legacy_daily_default_count")
        let dailyDraft = DayWeaveCanonicalItemDraft(
            kind: .task,
            status: .inbox,
            title: "Legacy daily",
            timezoneName: "UTC",
            recurrence: daily["recurrence"]
        )
        let dailyState = CanonicalItemEditorState(itemID: Self.id(58), draft: dailyDraft)
        #expect(dailyState.readOnlyDiagnostic == nil)
        #expect(dailyState.recurrence == .daily)
        #expect(dailyState.recurrenceCount == 1)

        let weekly = try Self.fixtureFields(named: "legacy_weekly_default_count")
        let weeklyDraft = DayWeaveCanonicalItemDraft(
            kind: .habit,
            status: .planned,
            title: "Legacy weekly",
            timezoneName: "UTC",
            recurrence: weekly["recurrence"]
        )
        let weeklyState = CanonicalItemEditorState(itemID: Self.id(59), draft: weeklyDraft)
        #expect(weeklyState.readOnlyDiagnostic == nil)
        #expect(weeklyState.recurrenceCount == 2)
        #expect(weeklyState.weekdays == [.tuesday, .thursday])

        let rich = try Self.fixtureFields(
            named: "frequency_task_with_rich_constraints_and_split_policy"
        )
        let richDraft = DayWeaveCanonicalItemDraft(
            kind: .task,
            status: .planned,
            title: "Rich graph task",
            timezoneName: "Europe/Paris",
            durationSeconds: 7_200,
            recurrence: rich["recurrence"],
            flexibleConstraints: rich["flexible_constraints"] ?? .object([:]),
            splitPolicy: .splittable(
                minimumChunkSeconds: 1_800,
                maximumChunkSeconds: 3_600
            )
        )
        let richState = CanonicalItemEditorState(itemID: Self.id(61), draft: richDraft)
        #expect(richState.readOnlyDiagnostic != nil)
        #expect(richState.dependencies.map(\.predecessorID) == [
            UUID(uuidString: "00000000-0000-0000-0000-000000000199")!,
        ])
        #expect(richState.draft == richDraft)
    }

    @Test("partial Inbox event timing remains lossless and read-only")
    func testPartialInboxEventTimingIsReadOnly() {
        let source = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .inbox,
            title: "Partial appointment",
            timezoneName: "UTC",
            durationSeconds: 900
        )

        let state = CanonicalItemEditorState(itemID: Self.id(62), draft: source)

        #expect(state.readOnlyDiagnostic?.contains("Incomplete Inbox") == true)
        #expect(!state.hasEventTiming)
        #expect(state.draft == source)
    }

    @Test("event state retains microseconds and omits a fractional duration")
    func testExactEventMicrosecondsAndFractionalDuration() throws {
        let source = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Precise event",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "dayweave_firm_block": .object([
                    "owned": .bool(true),
                    "starts_at": .string("2026-09-02T10:15:00.123456+02:00"),
                    "ends_at": .string("2026-09-02T10:15:02.023455+02:00"),
                ]),
            ])
        )
        let state = CanonicalItemEditorState(itemID: Self.id(63), draft: source)
        let draft = state.draft

        #expect(state.readOnlyDiagnostic == nil)
        #expect(draft.durationSeconds == nil)
        #expect(draft.earliestStartAt == nil)
        #expect(draft.deadlineAt == nil)
        guard case let .object(metadata) = draft.flexibleConstraints,
              case let .object(block)? = metadata["dayweave_firm_block"] else {
            Issue.record("Expected precise owned event metadata")
            return
        }
        #expect(block["starts_at"] == .string("2026-09-02T10:15:00.123456+02:00"))
        #expect(block["ends_at"] == .string("2026-09-02T10:15:02.023455+02:00"))

        let distant = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .planned,
            title: "Distant event",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "dayweave_firm_block": .object([
                    "owned": .bool(true),
                    "starts_at": .string("9999-12-30T10:00:00.000001Z"),
                    "ends_at": .string("9999-12-30T11:00:00.000001Z"),
                ]),
            ])
        )
        let distantState = CanonicalItemEditorState(itemID: Self.id(69), draft: distant)
        #expect(distantState.readOnlyDiagnostic != nil)
        #expect(distantState.draft == distant)
    }

    @Test("event metadata may span more than 366 days when canonical duration is absent")
    func testLongEventMetadataWithoutDuration() {
        var state = CanonicalItemEditorState(
            itemID: Self.id(74),
            now: Self.date("2026-01-01T09:00:00Z"),
            timezoneName: "UTC"
        )
        state.title = "Long-running exhibition"
        state.kind = .event
        state.normalizeForKindChange()
        state.eventStart = Self.date("2026-01-01T09:00:00Z")
        state.eventEnd = Self.date("2028-01-02T09:00:00Z")

        #expect(state.validationIssue == nil)
        #expect(state.draft.durationSeconds == nil)
        #expect(state.draft.earliestStartAt == state.eventStart)
        #expect(state.draft.deadlineAt == state.eventEnd)
    }

    @Test("Rust defaults normalize safely while explicit inactive state is retained")
    func testRustDefaultConstraintRoundTrip() {
        let source = DayWeaveCanonicalItemDraft(
            kind: .event,
            status: .inbox,
            title: "Default policies",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "energy": .null,
                "preferred_start_minute": .null,
                "maximum_sessions": .null,
                "maximum_split_days": .null,
                "calendar_event": .null,
                "calendar_context": .null,
                "dayweave_firm_block": .null,
                "goal_ids": .array([]),
                "constraints": .object([
                    "earliest_start": .null,
                    "latest_finish": .null,
                    "minimum_notice": .null,
                    "allowed_weekdays": .null,
                    "preferred_daily_windows": .array([]),
                    "preferred_absolute_windows": .array([]),
                    "forbidden_windows": .array([]),
                    "required_contexts": .array([]),
                    "required_location": .null,
                    "dependencies": .array([]),
                    "maximum_daily_work": .null,
                    "maximum_weekly_work": .null,
                    "buffers": .object([
                        "before": .number(JSONNumber(UInt64(0))),
                        "after": .number(JSONNumber(UInt64(0))),
                        "strength": .null,
                    ]),
                    "occurrence_window": .null,
                ]),
            ])
        )
        var state = CanonicalItemEditorState(itemID: Self.id(64), draft: source)

        #expect(state.readOnlyDiagnostic == nil)
        #expect(state.hasBuffers)
        #expect(!state.bufferHasStrength)
        state.title = "Edited default policies"
        #expect(state.validationIssue == nil)
        #expect(state.draft.flexibleConstraints == .object([
            "constraints": .object([
                "buffers": .object([
                    "before": .number(JSONNumber(UInt64(0))),
                    "after": .number(JSONNumber(UInt64(0))),
                    "strength": .null,
                ]),
                "occurrence_window": .null,
            ]),
        ]))

        for (kind, optionKey) in [
            (DayWeaveCanonicalItemKind.habit, "habit_target"),
            (.goal, "goal_weekly_allocation"),
            (.breakTime, "break_category"),
        ] {
            let optionSource = DayWeaveCanonicalItemDraft(
                kind: kind,
                status: .inbox,
                title: "Null \(optionKey)",
                timezoneName: "UTC",
                flexibleConstraints: .object([optionKey: .null])
            )
            let optionState = CanonicalItemEditorState(itemID: UUID(), draft: optionSource)
            #expect(optionState.readOnlyDiagnostic == nil, "Unexpected null gate: \(optionKey)")
            #expect(optionState.draft.flexibleConstraints == .object([:]))
        }
    }

    @Test("comma-containing tags stay one tag through unrelated edits")
    func testCommaTagRoundTrip() {
        let source = DayWeaveCanonicalItemDraft(
            title: "Research",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "tags": .array([.string("research,deep"), .string("writing")]),
            ])
        )
        var state = CanonicalItemEditorState(itemID: Self.id(65), draft: source)
        state.title = "Research deeply"

        #expect(state.tags.map(\.value).contains("research,deep"))
        #expect(state.validationIssue == nil)
        #expect(state.draft.flexibleConstraints == .object([
            "tags": .array([.string("research,deep"), .string("writing")]),
        ]))
    }

    @Test("Planned unknown-duration forms remain editable")
    func testPlannedUnknownDurationForms() {
        for kind in [
            DayWeaveCanonicalItemKind.task,
            .habit,
            .breakTime,
            .goal,
            .routine,
        ] {
            var state = CanonicalItemEditorState(itemID: UUID(), timezoneName: "UTC")
            state.title = "Estimate later"
            state.kind = kind
            state.normalizeForKindChange()
            state.readyStatus = .planned
            state.hasDuration = false
            if kind == .habit { state.recurrence = .daily }
            if kind == .goal || kind == .routine { state.hasOwnEffort = true }

            #expect(state.validationIssue == nil)
            #expect(state.draft.durationSeconds == nil)
        }
    }

    @Test("bounded scheduling offsets reject one minute past the shared maximum")
    func testSchedulingOffsetMaximum() {
        var state = CanonicalItemEditorState(itemID: Self.id(66), timezoneName: "UTC")
        state.title = "Bounded offsets"
        state.recurrence = .everyInterval
        state.recurrenceIntervalMinutes =
            CanonicalItemEditorState.maximumSchedulingOffsetMinutes + 1
        #expect(state.validationIssue?.contains("Repeat interval") == true)

        state.recurrence = .none
        state.hasMinimumNotice = true
        state.minimumNoticeMinutes = CanonicalItemEditorState.maximumSchedulingOffsetMinutes + 1
        #expect(state.validationIssue?.contains("Minimum notice") == true)

        state.hasMinimumNotice = false
        state.hasBuffers = true
        state.bufferBeforeMinutes = CanonicalItemEditorState.maximumSchedulingOffsetMinutes + 1
        #expect(state.validationIssue?.contains("Buffers") == true)

        state.hasBuffers = false
        state.hasDuration = true
        state.durationSeconds = 3_600
        state.isSplittable = true
        state.minimumChunkSeconds = 1_800
        state.maximumChunkSeconds = 3_600
        state.minimumGapMinutes = CanonicalItemEditorState.maximumSchedulingOffsetMinutes + 1
        #expect(state.validationIssue?.contains("Minimum split gap") == true)
    }

    @Test("parent choices stay linear, exact-depth, bounded, and cycle-safe")
    func testDeepParentChoices() throws {
        let count = 5_000
        var mutations: [DayWeavePendingCanonicalAuthoringMutation] = []
        mutations.reserveCapacity(count)
        for index in 0..<count {
            let itemID = Self.id(index + 10_000)
            let parentID = index == 0 ? nil : Self.id(index + 9_999)
            let draft = DayWeaveCanonicalItemDraft(
                kind: .task,
                status: .inbox,
                title: "Node \(index)",
                notes: nil,
                timezoneName: "UTC",
                parentID: parentID
            )
            mutations.append(.init(
                id: Self.id(index + 20_000),
                itemID: itemID,
                operation: .create,
                draft: draft,
                createdAt: Self.date("2026-08-30T09:00:00Z")
            ))
        }

        let leafID = Self.id(count - 1 + 10_000)
        let leafChoices = CanonicalItemEditorState.parentOptions(
            canonicalItems: [],
            pendingMutations: mutations,
            excluding: leafID
        )
        let deepest = try #require(leafChoices.last)
        #expect(leafChoices.count == count - 1)
        #expect(deepest.depth == count - 2)
        #expect(deepest.breadcrumb.count == CanonicalItemEditorState.maximumParentBreadcrumbDepth)
        #expect(deepest.breadcrumb.last == "Node \(count - 3)")

        let middleIndex = 2_500
        let middleChoices = CanonicalItemEditorState.parentOptions(
            canonicalItems: [],
            pendingMutations: mutations,
            excluding: Self.id(middleIndex + 10_000)
        )
        #expect(middleChoices.count == middleIndex)
        #expect(!middleChoices.contains { $0.id == Self.id(middleIndex + 1 + 10_000) })
    }

    @Test("executing-state items are never offered as parents")
    func testParentStatusEligibility() {
        let inboxID = Self.id(30_001)
        let plannedID = Self.id(30_002)
        let scheduledID = Self.id(30_003)
        let mutations = [
            Self.createMutation(itemID: inboxID, status: .inbox),
            Self.createMutation(itemID: plannedID, status: .planned),
            Self.createMutation(itemID: scheduledID, status: .scheduled),
        ]

        let options = CanonicalItemEditorState.parentOptions(
            canonicalItems: [],
            pendingMutations: mutations,
            excluding: Self.id(30_004)
        )

        #expect(Set(options.map(\.id)) == Set([inboxID, plannedID]))
    }

    @Test("typed dependencies round-trip every relation and validate their edge identity")
    func testTypedDependencyContract() throws {
        let ownerID = Self.id(31_000)
        let predecessorIDs = (31_001...31_004).map(Self.id)
        let source = DayWeaveCanonicalItemDraft(
            title: "Ship release",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([
                        Self.dependencyValue(
                            predecessorID: predecessorIDs[0],
                            relation: .finishToStart,
                            lag: 15,
                            strength: .hard
                        ),
                        Self.dependencyValue(
                            predecessorID: predecessorIDs[1],
                            relation: .startToStart,
                            lag: 0,
                            strength: .soft(weight: 75)
                        ),
                        Self.dependencyValue(
                            predecessorID: predecessorIDs[2],
                            relation: .finishToFinish,
                            lag: 30,
                            strength: .hard
                        ),
                        Self.dependencyValue(
                            predecessorID: predecessorIDs[3],
                            relation: .startToFinish,
                            lag: CanonicalItemEditorState.maximumSchedulingOffsetMinutes,
                            strength: .soft(weight: CanonicalItemEditorState.maximumSoftWeight)
                        ),
                    ]),
                ]),
            ])
        )

        var state = CanonicalItemEditorState(itemID: ownerID, draft: source)
        #expect(state.readOnlyDiagnostic == nil)
        #expect(state.validationIssue == nil)
        #expect(state.dependencies.map(\.relation) == CanonicalDependencyRelation.allCases)
        #expect(state.draft == source)

        state.dependencies.append(.init())
        #expect(state.validationIssue == "Choose a predecessor for every dependency.")
        state.dependencies.removeLast()

        state.dependencies[0].predecessorID = ownerID
        #expect(state.validationIssue == "An item cannot depend on itself.")
        state.dependencies[0].predecessorID = predecessorIDs[1]
        #expect(state.validationIssue == "Each predecessor can appear only once.")
        state.dependencies[0].predecessorID = predecessorIDs[0]
        state.dependencies[0].minimumLagMinutes =
            CanonicalItemEditorState.maximumSchedulingOffsetMinutes + 1
        #expect(state.validationIssue?.contains("Dependency lag") == true)
        state.dependencies[0].minimumLagMinutes = 0
        state.dependencies[0].strength = .soft
        state.dependencies[0].softWeight = CanonicalItemEditorState.maximumSoftWeight + 1
        #expect(state.validationIssue?.contains("Soft constraint weights") == true)
    }

    @Test("dependency graph warning catches explicit and ordered-routine cycles locally")
    func testDependencyCycleWarning() {
        let firstID = Self.id(32_001)
        let secondID = Self.id(32_002)
        let first = DayWeavePendingCanonicalAuthoringMutation(
            itemID: firstID,
            operation: .create,
            draft: .init(
                title: "First",
                timezoneName: "UTC",
                flexibleConstraints: .object([
                    "constraints": .object([
                        "dependencies": .array([Self.dependencyValue(
                            predecessorID: secondID,
                            relation: .finishToStart,
                            lag: 0,
                            strength: .hard
                        )]),
                    ]),
                ])
            )
        )
        let secondDraft = DayWeaveCanonicalItemDraft(
            title: "Second",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([Self.dependencyValue(
                        predecessorID: firstID,
                        relation: .startToStart,
                        lag: 5,
                        strength: .soft(weight: 40)
                    )]),
                ]),
            ])
        )
        #expect(CanonicalDependencyCatalog.cycleWarning(
            canonicalItems: [],
            pendingMutations: [first],
            replacing: secondID,
            with: secondDraft
        ) != nil)

        let routineID = Self.id(32_010)
        let earlierID = Self.id(32_011)
        let laterID = Self.id(32_012)
        let routine = DayWeavePendingCanonicalAuthoringMutation(
            itemID: routineID,
            operation: .create,
            draft: .init(
                kind: .routine,
                title: "Ordered routine",
                timezoneName: "UTC",
                flexibleConstraints: .object(["routine_ordered": .bool(true)])
            )
        )
        let earlier = DayWeavePendingCanonicalAuthoringMutation(
            itemID: earlierID,
            operation: .create,
            draft: .init(
                title: "Earlier",
                timezoneName: "UTC",
                parentID: routineID,
                siblingOrder: 0
            )
        )
        let later = DayWeavePendingCanonicalAuthoringMutation(
            itemID: laterID,
            operation: .create,
            draft: .init(
                title: "Later",
                timezoneName: "UTC",
                parentID: routineID,
                siblingOrder: 1
            )
        )
        let cyclicEarlier = DayWeaveCanonicalItemDraft(
            title: "Earlier",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([Self.dependencyValue(
                        predecessorID: laterID,
                        relation: .finishToStart,
                        lag: 0,
                        strength: .hard
                    )]),
                ]),
            ]),
            parentID: routineID,
            siblingOrder: 0
        )
        #expect(CanonicalDependencyCatalog.cycleWarning(
            canonicalItems: [],
            pendingMutations: [routine, earlier, later],
            replacing: earlierID,
            with: cyclicEarlier
        ) != nil)

        let opaqueID = Self.id(32_020)
        let bridgeID = Self.id(32_021)
        let editedID = Self.id(32_022)
        let opaque = DayWeavePendingCanonicalAuthoringMutation(
            itemID: opaqueID,
            operation: .create,
            draft: .init(
                title: "Newer dependency metadata",
                timezoneName: "UTC",
                flexibleConstraints: .object([
                    "constraints": .object([
                        "dependencies": .string("newer-format"),
                    ]),
                ])
            )
        )
        let bridge = DayWeavePendingCanonicalAuthoringMutation(
            itemID: bridgeID,
            operation: .create,
            draft: .init(
                title: "Known bridge",
                timezoneName: "UTC",
                flexibleConstraints: .object([
                    "constraints": .object([
                        "dependencies": .array([Self.dependencyValue(
                            predecessorID: opaqueID,
                            relation: .finishToStart,
                            lag: 0,
                            strength: .hard
                        )]),
                    ]),
                ])
            )
        )
        let edited = DayWeaveCanonicalItemDraft(
            title: "Edited item",
            timezoneName: "UTC",
            flexibleConstraints: .object([
                "constraints": .object([
                    "dependencies": .array([Self.dependencyValue(
                        predecessorID: bridgeID,
                        relation: .finishToStart,
                        lag: 0,
                        strength: .hard
                    )]),
                ]),
            ])
        )
        let opaqueReference = CanonicalDependencyCatalog.references(
            canonicalItems: [],
            pendingMutations: [opaque, bridge]
        ).first { $0.id == opaqueID }
        #expect(opaqueReference?.hasOpaqueDependencies == true)
        #expect(opaqueReference?.isSelectableDependencyCandidate == false)
        let opaqueRow = CanonicalInboxPresentation.build(
            activeItems: [],
            pendingMutations: [opaque],
            trashEntries: []
        ).inbox.first
        #expect(opaqueRow?.hasOpaqueDependencies == true)
        #expect(opaqueRow?.isReadOnly == true)
        #expect(CanonicalDependencyCatalog.cycleWarning(
            canonicalItems: [],
            pendingMutations: [opaque, bridge],
            replacing: editedID,
            with: edited
        ) == "Dependency safety cannot be verified because a related item uses newer metadata.")
    }

    @Test("local validation rejects impossible bounds")
    func testLocalValidation() {
        var state = CanonicalItemEditorState(itemID: Self.id(7), timezoneName: "UTC")
        state.title = "Deep work"
        state.hasDuration = true
        state.durationSeconds = 30 * 60
        state.isSplittable = true
        state.minimumChunkSeconds = 20 * 60
        state.maximumChunkSeconds = 45 * 60
        #expect(state.validationIssue?.contains("Split bounds") == true)

        state.isSplittable = false
        state.hasEarliestStart = true
        state.earliestStart = Self.date("2026-09-03T09:00:00Z")
        state.hasDeadline = true
        state.deadline = Self.date("2026-09-03T08:00:00Z")
        #expect(state.validationIssue?.contains("Earliest start") == true)
    }

    private static func id(_ value: Int) -> UUID {
        UUID(uuidString: String(format: "00000000-0000-4000-8000-%012llx", Int64(value)))!
    }

    private static func createMutation(
        itemID: UUID,
        status: DayWeaveCanonicalItemStatus
    ) -> DayWeavePendingCanonicalAuthoringMutation {
        DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(),
            itemID: itemID,
            operation: .create,
            draft: .init(
                kind: .goal,
                status: status,
                title: status.wireValue,
                notes: nil,
                timezoneName: "UTC"
            ),
            createdAt: date("2026-08-30T09:00:00Z")
        )
    }

    private static func date(_ value: String) -> Date {
        ISO8601DateFormatter().date(from: value)!
    }

    private static func dependencyValue(
        predecessorID: UUID,
        relation: CanonicalDependencyRelation,
        lag: UInt32,
        strength: CanonicalDependencyStrength
    ) -> JSONValue {
        CanonicalDependencyEdge(
            predecessorID: predecessorID,
            relation: relation,
            minimumLagMinutes: lag,
            strength: strength
        ).jsonValue
    }

    private static func fixtureFields(named name: String) throws -> [String: JSONValue] {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        let data = try Data(contentsOf: repository.appendingPathComponent(
            "fixtures/scheduling-metadata/valid-rich-items.json"
        ))
        let root = try JSONDecoder().decode(JSONValue.self, from: data)
        guard case let .object(document) = root,
              document["schema"] == .string("dayweave.scheduling-metadata-fixtures/1"),
              case let .array(cases)? = document["cases"] else {
            throw FixtureError.invalidDocument
        }
        for fixture in cases {
            guard case let .object(entry) = fixture,
                  entry["name"] == .string(name) else { continue }
            guard case let .object(fields)? = entry["fields"] else {
                throw FixtureError.invalidDocument
            }
            return fields
        }
        throw FixtureError.missingCase
    }

    private enum FixtureError: Error {
        case invalidDocument
        case missingCase
    }
}

private extension Date {
    var canonicalMicrosecondString: String? {
        CanonicalRFC3339Instant(date: self)?.canonicalUTCString
    }
}
#endif
