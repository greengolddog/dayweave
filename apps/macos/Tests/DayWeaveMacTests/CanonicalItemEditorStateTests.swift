import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical item editor state")
struct CanonicalItemEditorStateTests {
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
        state.energy = .low

        let draft = state.draft

        #expect(state.validationIssue == nil)
        #expect(draft.durationSeconds == 5_400)
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
            #expect(childOnly["has_own_effort"] == .bool(false))
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

    @Test("unrepresentable recurrence and calendar bindings fail read-only")
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
        #expect(customState.readOnlyDiagnostic != nil)

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
}
#endif
