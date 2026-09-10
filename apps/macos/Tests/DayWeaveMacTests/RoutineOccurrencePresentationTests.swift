import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Protected routine occurrence presentation")
struct RoutineOccurrencePresentationTests {
    private typealias F = RoutineOccurrenceTestFixtures

    @Test("calendar selection retains root and exact planner identity across descendant placement changes")
    func selectedIdentityIsNotADateOrLeaf() throws {
        var block = block()
        let selection = try #require(RoutineOccurrenceSelection(block: block))
        #expect(selection.seriesItemID == F.rootID)
        #expect(selection.occurrenceID == F.plannerID)
        #expect(selection.seriesItemID != block.sourceItemID)
        #expect(selection.matches(F.snapshot()))

        block.start = block.start.addingTimeInterval(86_400)
        block.end = block.end.addingTimeInterval(86_400)
        block.sourceItemID = F.id(9)
        #expect(RoutineOccurrenceSelection(block: block) == selection)

        block.occurrenceID = UUID(uuidString: "00000000-0000-5000-8000-000000000002")!
        #expect(RoutineOccurrenceSelection(block: block) != selection)
        #expect(RoutineOccurrenceSelection(block: block)?.matches(F.snapshot()) == false)
    }

    @Test("missing root or invalid planner identity cannot fall back to source item or calendar date")
    func missingIdentityFailsClosed() {
        var block = block()
        block.recurrenceSeriesItemID = nil
        #expect(RoutineOccurrenceSelection(block: block) == nil)
        block.recurrenceSeriesItemID = F.rootID
        block.occurrenceID = UUID()
        #expect(RoutineOccurrenceSelection(block: block) == nil)
        block.occurrenceID = nil
        #expect(RoutineOccurrenceSelection(block: block) == nil)
        block.occurrenceID = F.plannerID
        block.recurrenceSeriesItemID = RoutineOccurrenceValidation.nilID
        #expect(RoutineOccurrenceSelection(block: block) == nil)
    }

    @Test("a deep complete manifest renders as flat rows including unscheduled canonical members")
    func deepMembersStayFlatAndComplete() {
        let snapshot = F.snapshot(count: 5_000)
        let rows = RoutineOccurrencePresentation.rows(snapshot)
        #expect(rows.count == 5_000)
        #expect(rows.first?.id == F.rootID)
        #expect(rows.first?.evaluation.counts.requiredDescendants == 4_999)
        #expect(rows.last?.id == F.id(5_000))
        #expect(rows.last?.depth == 4_999)
        #expect(rows.last?.hasChildren == false)
        #expect(rows.dropLast().allSatisfy { $0.hasChildren })
    }

    @Test("known non-task roots stay in their own workflows while missing templates retain exact lookup")
    func knownHabitAndEventRootsExcluded() {
        for kind in [DayWeaveCanonicalItemKind.habit, .event, .goal, .project, .breakTime] {
            #expect(RoutineOccurrenceSelection(block: block(), currentSeriesKind: kind) == nil)
        }
        #expect(RoutineOccurrenceSelection(block: block(), currentSeriesKind: .task) != nil)
        #expect(RoutineOccurrenceSelection(block: block(), currentSeriesKind: .routine) != nil)
        #expect(RoutineOccurrenceSelection(block: block(), currentSeriesKind: nil) != nil)
    }

    @Test("reopening review retains the exact blocked state and parent outcomes remain unavailable")
    func exactOpenStateAndParentAuthority() throws {
        let rows = RoutineOccurrencePresentation.rows(F.snapshot())
        let parent = try #require(rows.first)
        let child = try #require(rows.last)
        #expect(!RoutineOccurrencePresentation.canApply(.setOutcome(status: .completed), to: parent))
        #expect(RoutineOccurrencePresentation.canApply(.setPolicy(requiredForParent: false, mode: .complete), to: parent))
        #expect(!RoutineOccurrencePresentation.canApply(.setPolicy(requiredForParent: true, mode: .keepOpen), to: child))
        let open = ItemCompletionReopenState(status: .blocked, blockedReasonKind: .dependency,
            blockedByItemID: F.id(77), blockedReason: "Waiting for the reviewed dependency")
        let blocked = RoutineOccurrenceMemberRow(definition: child.definition,
            state: .init(itemID: child.id, revision: child.state.revision, status: .skipped,
                requiredForParent: true, mode: .automatic, open: open, provenance: nil,
                completedAt: nil, updatedAt: F.now), evaluation: child.evaluation,
            depth: child.depth, hasChildren: false)
        #expect(RoutineOccurrencePresentation.canApply(.reopen(open: open), to: blocked))
        #expect(!RoutineOccurrencePresentation.canApply(.reopen(open: .init(status: .planned)), to: blocked))
        #expect(!RoutineOccurrencePresentation.canApply(.reopen(open: .init(status: .blocked,
            blockedReasonKind: .dependency, blockedByItemID: F.id(78), blockedReason: open.blockedReason)), to: blocked))
    }

    private func block() -> ScheduleBlock {
        .init(id: F.id(300), title: "Scheduled child", kind: .task,
            start: Date(timeIntervalSince1970: 1_788_000_000), end: Date(timeIntervalSince1970: 1_788_003_600),
            status: .scheduled, project: nil, notes: "", energy: .medium,
            isFlexible: true, isHardConstraint: false, actualMinutes: nil,
            sourceItemID: F.childID, sourceItemRevision: 1, occurrenceID: F.plannerID,
            recurrenceSeriesItemID: F.rootID)
    }
}
#endif
