import Foundation
import Testing

@testable import DayWeaveMac

@Suite("Will-do-later timing")
struct WillDoLaterTimingTests {
    @Test("execution targets retain the assessment TTL plus one full slot")
    func executionTargetSafetyBoundary() throws {
        let onGrid = try #require(
            ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z")
        )
        let justAfterGrid = onGrid.addingTimeInterval(1)

        #expect(
            WillDoLaterTiming.minimumExecutionMoveStart(after: onGrid)
                == onGrid.addingTimeInterval(10 * 60)
        )
        #expect(
            WillDoLaterTiming.minimumExecutionMoveStart(after: justAfterGrid)
                == onGrid.addingTimeInterval(15 * 60)
        )
        #expect(!DayWeaveExecutionDeferTiming.isValidNewMoveStart(
            onGrid.addingTimeInterval(5 * 60),
            now: onGrid
        ))
        #expect(DayWeaveExecutionDeferTiming.isValidNewMoveStart(
            onGrid.addingTimeInterval(10 * 60),
            now: onGrid
        ))
    }

    @Test("tomorrow morning follows the profile timezone across spring DST")
    func profileTomorrowAcrossDST() throws {
        let reference = try #require(ISO8601DateFormatter().date(from: "2026-03-28T22:00:00Z"))
        let minimum = reference.addingTimeInterval(60)

        let morning = WillDoLaterTiming.tomorrowMorning(
            after: reference,
            minimum: minimum,
            timezoneName: "Europe/Madrid"
        )

        #expect(ISO8601DateFormatter().string(from: morning) == "2026-03-29T07:00:00Z")
    }

    @Test("latest finish compares the proposed end rather than its start")
    func deadlineUsesMoveEnd() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        let block = makeBlock(start: start, end: start.addingTimeInterval(3_600))
        let moved = try #require(WillDoLaterTiming.proposedWindow(
            for: block,
            moveStart: start.addingTimeInterval(7_200),
            allBlocks: [block],
            accumulatedSeconds: nil
        ))

        #expect(!WillDoLaterTiming.finishesAfterLatestFinish(
            moved,
            latestFinish: moved.end
        ))
        #expect(WillDoLaterTiming.finishesAfterLatestFinish(
            moved,
            latestFinish: moved.end.addingTimeInterval(-1)
        ))
    }

    @Test("active moves use authoritative remaining seconds")
    func activeRemainingWindow() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        var block = makeBlock(start: start, end: start.addingTimeInterval(3_600))
        block.status = .active
        let moveStart = start.addingTimeInterval(10_800)

        let moved = try #require(WillDoLaterTiming.proposedWindow(
            for: block,
            moveStart: moveStart,
            allBlocks: [block],
            accumulatedSeconds: 900
        ))

        #expect(moved.start == moveStart)
        #expect(moved.end == moveStart.addingTimeInterval(2_700))
    }

    @Test("split recurring occurrence stays anchored to the chosen session")
    func recurringOccurrenceWindow() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        let itemID = UUID()
        let occurrenceID = UUID()
        var first = makeBlock(start: start, end: start.addingTimeInterval(1_800))
        first.sourceItemID = itemID
        first.sourceItemRevision = 4
        first.occurrenceID = occurrenceID
        first.recurrenceSeriesItemID = itemID
        first.sessionIndex = 0
        first.recurrenceMoveSource = recurrenceSource
        var second = makeBlock(
            start: start.addingTimeInterval(3_600),
            end: start.addingTimeInterval(5_400)
        )
        second.sourceItemID = itemID
        second.sourceItemRevision = 4
        second.occurrenceID = occurrenceID
        second.recurrenceSeriesItemID = itemID
        second.sessionIndex = 1
        second.recurrenceMoveSource = recurrenceSource
        let chosenSecondStart = start.addingTimeInterval(10_800)

        let moved = try #require(WillDoLaterTiming.proposedWindow(
            for: second,
            moveStart: chosenSecondStart,
            allBlocks: [first, second],
            accumulatedSeconds: nil
        ))

        #expect(moved.start == start.addingTimeInterval(7_200))
        #expect(moved.end == start.addingTimeInterval(12_600))
        #expect(moved.movedBlockIDs == [first.id, second.id])
    }

    @Test("scheduled recurrence moves are recomposed rather than treated as exact placements")
    func recurringPlacementSemantics() throws {
        let start = try #require(
            ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z")
        )
        var recurring = makeBlock(start: start, end: start.addingTimeInterval(1_800))
        recurring.sourceItemID = UUID()
        recurring.sourceItemRevision = 1
        recurring.occurrenceID = UUID()
        recurring.recurrenceSeriesItemID = recurring.sourceItemID
        recurring.recurrenceMoveSource = recurrenceSource

        #expect(!WillDoLaterTiming.usesExactPlacement(for: recurring))
        recurring.status = .active
        #expect(WillDoLaterTiming.usesExactPlacement(for: recurring))

        var canonical = recurring
        canonical.status = .scheduled
        canonical.occurrenceID = nil
        canonical.recurrenceSeriesItemID = nil
        canonical.recurrenceMoveSource = nil
        #expect(!WillDoLaterTiming.usesExactPlacement(for: canonical))

        let local = makeBlock(start: start, end: start.addingTimeInterval(1_800))
        #expect(WillDoLaterTiming.usesExactPlacement(for: local))
    }

    @Test("exact windows report intersections with fixed time")
    func fixedOverlap() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        let movable = makeBlock(start: start, end: start.addingTimeInterval(1_800))
        var fixed = makeBlock(
            start: start.addingTimeInterval(3_900),
            end: start.addingTimeInterval(5_100)
        )
        fixed.isFlexible = false
        fixed.isHardConstraint = true
        let moved = try #require(WillDoLaterTiming.proposedWindow(
            for: movable,
            moveStart: start.addingTimeInterval(3_600),
            allBlocks: [movable, fixed],
            accumulatedSeconds: nil
        ))

        #expect(WillDoLaterTiming.fixedConflicts(with: moved, in: [movable, fixed]) == [fixed])
    }

    @Test("recurring hierarchy moves every descendant leaf under the shared series")
    func recurringHierarchyWindow() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        let seriesID = UUID()
        let occurrenceID = UUID()
        var first = makeBlock(start: start, end: start.addingTimeInterval(900))
        first.sourceItemID = UUID()
        first.sourceItemRevision = 2
        first.occurrenceID = occurrenceID
        first.recurrenceSeriesItemID = seriesID
        first.sessionIndex = 0
        first.recurrenceMoveSource = recurrenceSource
        var second = makeBlock(
            start: start.addingTimeInterval(1_800),
            end: start.addingTimeInterval(2_700)
        )
        second.sourceItemID = UUID()
        second.sourceItemRevision = 5
        second.occurrenceID = occurrenceID
        second.recurrenceSeriesItemID = seriesID
        second.sessionIndex = 0
        second.recurrenceMoveSource = recurrenceSource

        let moved = try #require(WillDoLaterTiming.proposedWindow(
            for: second,
            moveStart: start.addingTimeInterval(7_200),
            allBlocks: [first, second],
            accumulatedSeconds: nil
        ))

        #expect(moved.start == start.addingTimeInterval(5_400))
        #expect(moved.end == start.addingTimeInterval(8_100))
        #expect(moved.movedBlockIDs == [first.id, second.id])
    }

    @Test("flexible pinned blocks are protected conflicts and sensitive titles are redacted")
    func flexiblePinnedConflictIsPrivate() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        let movable = makeBlock(start: start, end: start.addingTimeInterval(900))
        var pinned = makeBlock(
            start: start.addingTimeInterval(3_600),
            end: start.addingTimeInterval(4_500)
        )
        pinned.title = "Private medical appointment"
        pinned.isSensitive = true
        pinned.isFlexible = true
        pinned.isHardConstraint = false
        pinned.previewKind = "pinned"
        let moved = try #require(WillDoLaterTiming.proposedWindow(
            for: movable,
            moveStart: start.addingTimeInterval(3_600),
            allBlocks: [movable, pinned],
            accumulatedSeconds: nil
        ))

        #expect(WillDoLaterTiming.fixedConflicts(with: moved, in: [movable, pinned]) == [pinned])
        let label = WillDoLaterTiming.conflictLabel(pinned, timezoneName: "UTC")
        #expect(label.contains("Sensitive busy time"))
        #expect(!label.contains("Private medical appointment"))
    }

    @Test("deadline policy binds every moved leaf but active work uses its focused leaf")
    func hierarchicalDeadlinePolicy() throws {
        let start = try #require(ISO8601DateFormatter().date(from: "2026-08-30T10:00:00Z"))
        let rootID = UUID()
        let firstID = UUID()
        let secondID = UUID()
        let occurrenceID = UUID()
        let root = try canonicalItem(id: rootID, revision: 7, parentID: nil, constraints: "{}")
        let firstItem = try canonicalItem(
            id: firstID,
            revision: 2,
            parentID: rootID,
            constraints: latestFinish("2026-08-30T15:00:00Z", level: "hard")
        )
        let secondItem = try canonicalItem(
            id: secondID,
            revision: 5,
            parentID: rootID,
            constraints: latestFinish("2026-08-30T14:00:00Z", level: "soft")
        )
        var first = makeBlock(start: start, end: start.addingTimeInterval(900))
        first.sourceItemID = firstID
        first.sourceItemRevision = 2
        first.occurrenceID = occurrenceID
        first.recurrenceSeriesItemID = rootID
        first.recurrenceMoveSource = RecurrenceMoveSource(
            itemRevision: 7,
            identity: recurrenceSource.identity,
            nominalStart: recurrenceSource.nominalStart,
            nominalEnd: recurrenceSource.nominalEnd,
            localDate: recurrenceSource.localDate,
            ordinal: recurrenceSource.ordinal
        )
        var second = first
        second.sourceItemID = secondID
        second.sourceItemRevision = 5

        let scheduled = try #require(DayWeaveMoveDeadlinePolicy.identities(
            for: first,
            movingWholeOccurrence: true,
            allBlocks: [first, second],
            canonicalItems: [root, firstItem, secondItem]
        ))
        let active = try #require(DayWeaveMoveDeadlinePolicy.identities(
            for: first,
            movingWholeOccurrence: false,
            allBlocks: [first, second],
            canonicalItems: [root, firstItem, secondItem]
        ))

        #expect(Set(scheduled.map(\.itemID)) == [firstID, secondID])
        #expect(Set(active.map(\.itemID)) == [firstID])
        let outerWindow = WillDoLaterMoveWindow(
            start: start.addingTimeInterval(10_800),
            end: start.addingTimeInterval(21_600),
            movedBlockIDs: [first.id, second.id]
        )
        #expect(WillDoLaterTiming.crossedDeadlines(
            scheduled,
            window: outerWindow
        ) == scheduled)
        #expect(WillDoLaterTiming.crossedDeadlines(
            active,
            window: outerWindow
        ) == active)
    }

    private var recurrenceSource: RecurrenceMoveSource {
        RecurrenceMoveSource(
            itemRevision: 4,
            identity: .calendarDay(date: "2026-08-30", bucketOrdinal: 0),
            nominalStart: "2026-08-30T10:00:00Z",
            nominalEnd: "2026-08-30T12:00:00Z",
            localDate: "2026-08-30",
            ordinal: 0
        )
    }

    private func latestFinish(_ value: String, level: String) -> String {
        let strength = level == "soft"
            ? #"{"level":"soft","weight":100}"#
            : #"{"level":"hard"}"#
        return #"{"constraints":{"latest_finish":{"value":"\#(value)","strength":\#(strength)}}}"#
    }

    private func canonicalItem(
        id: UUID,
        revision: UInt64,
        parentID: UUID?,
        constraints: String
    ) throws -> DayWeaveCanonicalItem {
        let parent = parentID.map { #""\#($0.uuidString.lowercased())""# } ?? "null"
        let kind = parentID == nil ? "routine" : "task"
        let duration = parentID == nil ? "null" : "900"
        let recurrence = parentID == nil ? #"{"type":"daily","times_per_day":1}"# : "null"
        let executable = parentID == nil ? "false" : "true"
        let json = #"""
        {"id":"\#(id.uuidString.lowercased())","is_sensitive":false,
         "kind":"\#(kind)","status":"scheduled","title":"Item","notes":null,
         "timezone_name":"UTC","duration_seconds":\#(duration),"deadline_at":null,
         "earliest_start_at":null,"recurrence":\#(recurrence),
         "flexible_constraints":\#(constraints),"split_policy":{"type":"indivisible"},
         "importance":50,"urgency":50,"parent_id":\#(parent),"sibling_order":0,
         "is_executable":\#(executable),"revision":\#(revision),
         "created_at":"2027-01-15T08:00:00Z","updated_at":"2027-01-15T08:00:00Z",
         "completed_at":null,"deleted_at":null}
        """#
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }

    private func makeBlock(start: Date, end: Date) -> ScheduleBlock {
        ScheduleBlock(
            id: UUID(),
            title: "Work",
            kind: .task,
            start: start,
            end: end,
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil
        )
    }
}
