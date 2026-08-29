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

    func testQuickAddEnforcesUnicodeScalarContractAndSupportsRecovery() throws {
        let store = PlannerStore(restoreFromPersistence: false)
        let valid = String(repeating: "é", count: PlannerStore.maximumCanonicalTitleScalars)
        let invalid = valid + "x"

        XCTAssertFalse(store.quickAdd(title: invalid, kind: .task, minutes: 20))
        XCTAssertTrue(store.blocks.isEmpty)
        XCTAssertTrue(store.quickAdd(title: "  \(valid)  ", kind: .task, minutes: 20))
        let block = try XCTUnwrap(store.blocks.first)
        XCTAssertEqual(block.title, valid)

        store.quarantineLocalCapture(block.id, diagnostic: "Needs a repair")
        XCTAssertFalse(store.updateLocalCapture(block.id, title: invalid))
        XCTAssertEqual(store.localCaptureDiagnostics[block.id], "Needs a repair")
        XCTAssertTrue(store.updateLocalCapture(block.id, title: "Recovered"))
        XCTAssertNil(store.localCaptureDiagnostics[block.id])
        store.deleteLocalCapture(block.id)
        XCTAssertTrue(store.blocks.isEmpty)
    }

    func testTodayIncludesAnOvernightBlockCrossingDayStart() throws {
        let calendar = Calendar.autoupdatingCurrent
        let dayStart = calendar.startOfDay(for: Date())
        let overnight = ScheduleBlock(
            id: UUID(),
            title: "Overnight duty",
            kind: .event,
            start: dayStart.addingTimeInterval(-1_800),
            end: dayStart.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: false,
            isHardConstraint: true,
            actualMinutes: nil
        )
        let store = PlannerStore(
            blocks: [overnight],
            restoreFromPersistence: false,
            now: { dayStart.addingTimeInterval(60) }
        )

        XCTAssertEqual(store.todaysBlocks.map(\.id), [overnight.id])
    }

    func testRecurrenceHistoryUsesBoundedNewestCompleteOccurrenceGroups() {
        let itemID = UUID()
        let base = Date(timeIntervalSince1970: 1_700_000_000)
        let outcomes = (0...PlannerStore.maximumRecurrenceSessionOutcomes).map { index in
            RecurrenceSessionOutcome(
                itemID: itemID,
                occurrenceID: UUID(),
                sessionIndex: 0,
                disposition: .completed,
                occurredAt: base.addingTimeInterval(TimeInterval(index)),
                occurrenceFullyScheduled: true
            )
        }

        let store = PlannerStore(
            recurrenceSessionOutcomes: outcomes,
            restoreFromPersistence: false
        )

        XCTAssertEqual(
            store.recurrenceSessionOutcomes.count,
            PlannerStore.maximumRecurrenceSessionOutcomes
        )
        XCTAssertFalse(store.recurrenceSessionOutcomes.contains { $0.occurredAt == base })
        XCTAssertTrue(store.recurrenceSessionOutcomes.contains {
            $0.occurredAt == base.addingTimeInterval(
                TimeInterval(PlannerStore.maximumRecurrenceSessionOutcomes)
            )
        })
    }

    func testBoundConfigurationMigratesOnlyCanonicalFullIdentifierSpelling() throws {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let legacy = "HTTPS://API.EXAMPLE.COM:443/gateway/"
        let canonical = "https://api.example.com/gateway"
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: legacy,
            generatedAt: now,
            asOf: now,
            horizonStart: now,
            horizonEnd: now.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let store = PlannerStore(
            canonicalDeltaCursor: "remote-cursor",
            canonicalConfigurationIdentifier: legacy,
            schedulePreviewProvenance: provenance,
            restoreFromPersistence: false
        )

        XCTAssertNoThrow(try store.prepareCanonicalSync(configurationIdentifier: canonical))
        XCTAssertEqual(store.canonicalConfigurationIdentifier, canonical)
        XCTAssertEqual(store.schedulePreviewProvenance?.configurationIdentifier, canonical)
    }

    func testBoundConfigurationDoesNotEquatePathOrEncodingChanges() {
        for (saved, requested) in [
            ("https://api.example.com/a%2Fb", "https://api.example.com/a/b"),
            ("https://api.example.com/a/../b", "https://api.example.com/b"),
            ("https://api.example.com/a//", "https://api.example.com/a/"),
            ("https://api.example.com/a", "https://api.example.com/b"),
            ("https://api.example.com/a", "https://other.example.com/a"),
            ("not a URL", "https://api.example.com/a"),
        ] {
            let store = PlannerStore(
                canonicalDeltaCursor: "remote-cursor",
                canonicalConfigurationIdentifier: saved,
                restoreFromPersistence: false
            )
            XCTAssertThrowsError(try store.prepareCanonicalSync(configurationIdentifier: requested))
            XCTAssertEqual(store.canonicalConfigurationIdentifier, saved)
        }

        let unbound = PlannerStore(
            canonicalDeltaCursor: "remote-cursor",
            restoreFromPersistence: false
        )
        XCTAssertThrowsError(
            try unbound.prepareCanonicalSync(configurationIdentifier: "https://api.example.com/a")
        )
        XCTAssertNil(unbound.canonicalConfigurationIdentifier)
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

    func testCompletingARecurringOccurrencePersistsItsStableIdentity() {
        let occurrenceID = UUID()
        let start = Date(timeIntervalSince1970: 1_700_000_000)
        let block = ScheduleBlock(
            id: UUID(),
            title: "Daily walk",
            kind: .habit,
            start: start,
            end: start.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .low,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: UUID(),
            sourceItemRevision: 3,
            occurrenceID: occurrenceID,
            sessionIndex: 0,
            syncOrigin: .local
        )
        let store = PlannerStore(blocks: [block], restoreFromPersistence: false)

        store.complete(block.id)

        XCTAssertTrue(store.completedOccurrenceIDs.contains(occurrenceID))
    }

    func testRecurringSplitRequiresEverySessionAndSupportsCorrection() {
        let itemID = UUID()
        let occurrenceID = UUID()
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        func block(session: UInt16, offset: TimeInterval) -> ScheduleBlock {
            ScheduleBlock(
                id: UUID(),
                title: "Split habit",
                kind: .habit,
                start: now.addingTimeInterval(offset),
                end: now.addingTimeInterval(offset + 900),
                status: .scheduled,
                project: nil,
                notes: "",
                energy: .low,
                isFlexible: true,
                isHardConstraint: false,
                actualMinutes: nil,
                sourceItemID: itemID,
                sourceItemRevision: 4,
                occurrenceID: occurrenceID,
                sessionIndex: session,
                syncOrigin: .local,
                previewKind: "planned"
            )
        }
        let first = block(session: 0, offset: 0)
        let second = block(session: 1, offset: 1_800)
        let store = PlannerStore(
            blocks: [first, second],
            restoreFromPersistence: false,
            now: { now }
        )

        store.complete(first.id)
        XCTAssertFalse(store.completedOccurrenceIDs.contains(occurrenceID))
        XCTAssertEqual(store.recurrenceSessionOutcomes.count, 1)

        store.complete(second.id)
        XCTAssertTrue(store.completedOccurrenceIDs.contains(occurrenceID))
        XCTAssertEqual(store.recurrenceCompletionAnchors()[itemID], now)

        store.start(first.id)
        XCTAssertFalse(store.completedOccurrenceIDs.contains(occurrenceID))
        XCTAssertEqual(store.recurrenceSessionOutcomes.count, 1)

        store.skip(first.id)
        XCTAssertFalse(store.completedOccurrenceIDs.contains(occurrenceID))
        XCTAssertEqual(store.skippedOccurrenceIDs, Set([occurrenceID]))
    }

    func testCanonicalSyncLockDisablesUserEditsButAllowsSyncStateApplication() {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let block = ScheduleBlock(
            id: UUID(), title: "Locked work", kind: .task, start: now,
            end: now.addingTimeInterval(900), status: .scheduled, project: nil,
            notes: "", energy: .medium, isFlexible: true,
            isHardConstraint: false, actualMinutes: nil
        )
        let store = PlannerStore(blocks: [block], restoreFromPersistence: false)

        XCTAssertTrue(store.beginCanonicalSync())
        XCTAssertFalse(store.canMutatePlan)
        store.complete(block.id)
        XCTAssertEqual(store.blocks.first?.status, .scheduled)
        store.applyCanonicalDelta([], nextCursor: "sync-cursor")
        XCTAssertEqual(store.canonicalDeltaCursor, "sync-cursor")

        store.endCanonicalSync()
        store.complete(block.id)
        XCTAssertEqual(store.blocks.first?.status, .completed)
    }
}
#elseif canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Planner store safety")
@MainActor
struct PlannerStoreTestingTests {
    @Test("Quick Add enforces scalar limits and supports local recovery")
    func quickAddRecovery() throws {
        let store = PlannerStore(restoreFromPersistence: false)
        let valid = String(repeating: "é", count: PlannerStore.maximumCanonicalTitleScalars)
        #expect(!store.quickAdd(title: valid + "x", kind: .task, minutes: 20))
        #expect(store.quickAdd(title: "  \(valid)  ", kind: .task, minutes: 20))
        let block = try #require(store.blocks.first)
        #expect(block.title == valid)
        store.quarantineLocalCapture(block.id, diagnostic: "Needs a repair")
        #expect(!store.updateLocalCapture(block.id, title: valid + "x"))
        #expect(store.updateLocalCapture(block.id, title: "Recovered"))
        #expect(store.localCaptureDiagnostics[block.id] == nil)
        store.deleteLocalCapture(block.id)
        #expect(store.blocks.isEmpty)
    }

    @Test("Today includes overnight blocks crossing day start")
    func overnightBlock() {
        let calendar = Calendar.autoupdatingCurrent
        let dayStart = calendar.startOfDay(for: Date())
        let overnight = ScheduleBlock(
            id: UUID(), title: "Overnight duty", kind: .event,
            start: dayStart.addingTimeInterval(-1_800),
            end: dayStart.addingTimeInterval(1_800), status: .scheduled,
            project: nil, notes: "", energy: .medium, isFlexible: false,
            isHardConstraint: true, actualMinutes: nil
        )
        let store = PlannerStore(
            blocks: [overnight], restoreFromPersistence: false,
            now: { dayStart.addingTimeInterval(60) }
        )
        #expect(store.todaysBlocks.map(\.id) == [overnight.id])
    }

    @Test("Bound configuration spelling migrates, but path identity does not")
    func canonicalConfigurationMigration() throws {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let legacy = "HTTPS://API.EXAMPLE.COM:443/gateway/"
        let canonical = "https://api.example.com/gateway"
        let store = PlannerStore(
            canonicalDeltaCursor: "remote-cursor",
            canonicalConfigurationIdentifier: legacy,
            schedulePreviewProvenance: .init(
                configurationIdentifier: legacy, generatedAt: now, asOf: now,
                horizonStart: now, horizonEnd: now.addingTimeInterval(86_400),
                timezoneName: "UTC"
            ),
            restoreFromPersistence: false
        )
        try store.prepareCanonicalSync(configurationIdentifier: canonical)
        #expect(store.canonicalConfigurationIdentifier == canonical)
        #expect(store.schedulePreviewProvenance?.configurationIdentifier == canonical)

        for (saved, requested) in [
            ("https://api.example.com/a%2Fb", "https://api.example.com/a/b"),
            ("https://api.example.com/a/../b", "https://api.example.com/b"),
            ("https://api.example.com/a//", "https://api.example.com/a/"),
            ("https://api.example.com/a", "https://other.example.com/a"),
            ("not a URL", "https://api.example.com/a"),
        ] {
            let mismatch = PlannerStore(
                canonicalDeltaCursor: "remote", canonicalConfigurationIdentifier: saved,
                restoreFromPersistence: false
            )
            #expect(throws: (any Error).self) {
                try mismatch.prepareCanonicalSync(configurationIdentifier: requested)
            }
        }
        let unbound = PlannerStore(canonicalDeltaCursor: "remote", restoreFromPersistence: false)
        #expect(throws: (any Error).self) {
            try unbound.prepareCanonicalSync(configurationIdentifier: canonical)
        }
    }

    @Test("Recurrence history retains only the newest bounded outcomes")
    func boundedRecurrenceHistory() {
        let itemID = UUID()
        let base = Date(timeIntervalSince1970: 1_700_000_000)
        let outcomes = (0...PlannerStore.maximumRecurrenceSessionOutcomes).map { index in
            RecurrenceSessionOutcome(
                itemID: itemID, occurrenceID: UUID(), sessionIndex: 0,
                disposition: .completed,
                occurredAt: base.addingTimeInterval(TimeInterval(index)),
                occurrenceFullyScheduled: true
            )
        }
        let store = PlannerStore(
            recurrenceSessionOutcomes: outcomes,
            restoreFromPersistence: false
        )
        #expect(store.recurrenceSessionOutcomes.count == PlannerStore.maximumRecurrenceSessionOutcomes)
        #expect(!store.recurrenceSessionOutcomes.contains { $0.occurredAt == base })
    }
}
#endif
