import Foundation
#if canImport(XCTest)
import XCTest
@testable import DayWeaveMac

@MainActor
final class PlannerStoreTests: XCTestCase {
    func testStartingAnItemPausesThePreviousOne() throws {
        let store = PlannerStore.preview(now: Date(timeIntervalSince1970: 1_700_000_000))
        let firstActive = try XCTUnwrap(store.activeItem)
        let replacement = try XCTUnwrap(store.blocks.first(where: { $0.id != firstActive.id && $0.status == .scheduled }))

        store.start(replacement.id)

        XCTAssertEqual(store.activeItem?.id, replacement.id)
        XCTAssertEqual(store.blocks.first(where: { $0.id == firstActive.id })?.status, .paused)
    }

    func testQuickAddCreatesFlexibleTaskAfterExistingWork() throws {
        let store = PlannerStore.preview(now: Date(timeIntervalSince1970: 1_700_000_000))
        let previousEnd = try XCTUnwrap(store.blocks.map(\.end).max())

        store.quickAdd(title: "Write release notes", kind: .task, minutes: 25)

        let added = try XCTUnwrap(store.blocks.first(where: { $0.title == "Write release notes" }))
        XCTAssertEqual(added.durationMinutes, 25)
        XCTAssertTrue(added.isFlexible)
        XCTAssertGreaterThanOrEqual(added.start, previousEnd)
        XCTAssertEqual(store.selectedBlockID, added.id)
    }

    func testAcceptingSuggestionNeverMutatesScheduleDirectly() throws {
        let store = PlannerStore.preview(now: Date(timeIntervalSince1970: 1_700_000_000))
        let snapshot = store.blocks
        let suggestion = try XCTUnwrap(store.suggestions.first)

        store.acceptSuggestion(suggestion.id)

        XCTAssertEqual(store.blocks, snapshot)
        XCTAssertEqual(store.suggestions.first?.state, .accepted)
    }

    func testProductionStartupWithoutSnapshotIsEmptyInsteadOfPreviewSeeded() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeaveLiveStoreTests-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let key = try PlannerEncryptionKey(data: Data(repeating: 7, count: 32))
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: key
        )

        let store = PlannerStore.live(persistence: persistence)

        XCTAssertTrue(store.blocks.isEmpty)
        XCTAssertTrue(store.suggestions.isEmpty)
        XCTAssertTrue(store.assistantMessages.isEmpty)
        XCTAssertNil(store.selectedBlockID)
        XCTAssertEqual(store.lastScheduleMessage, "No schedule yet — add an item when you’re ready")
    }
}
#endif
