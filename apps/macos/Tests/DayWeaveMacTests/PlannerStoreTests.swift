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

    func testCanonicalCreateDraftIsEncryptedOfflineAndRestoresWithoutSchedulePlacement() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let itemID = UUID(uuidString: "aa100000-0000-4000-8000-000000000001")!
        let draft = DayWeaveCanonicalItemDraft(
            isSensitive: true,
            title: "AUTHORING-SECRET-OFFLINE-TASK",
            notes: "Private offline authoring notes",
            timezoneName: "Europe/Madrid",
            durationSeconds: nil,
            deadlineAt: now.addingTimeInterval(86_400)
        )
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )

        let mutation = try store.enqueueCanonicalCreate(itemID: itemID, draft: draft)

        XCTAssertTrue(store.blocks.isEmpty)
        XCTAssertEqual(store.pendingCanonicalAuthoringMutations, [mutation])
        XCTAssertEqual(store.selectedCanonicalItemID, itemID)
        let encrypted = try Data(contentsOf: context.fileURL)
        XCTAssertNil(encrypted.range(of: Data("AUTHORING-SECRET-OFFLINE-TASK".utf8)))

        let restored = PlannerStore(persistence: context.persistence)
        XCTAssertEqual(restored.pendingCanonicalAuthoringMutations, [mutation])
        XCTAssertEqual(restored.selectedCanonicalItemID, itemID)
        XCTAssertTrue(restored.blocks.isEmpty)
    }

    func testSubmittedCanonicalDraftIsConfigurationBoundImmutableAndRestartSafe() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "aa200000-0000-4000-8000-000000000002")!
        let binding = authoringConfigurationIdentifier
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Date(timeIntervalSince1970: 1_800_000_100) }
        )
        let queued = try store.enqueueCanonicalCreate(
            itemID: itemID,
            draft: .init(title: "Restart-safe create", timezoneName: "UTC")
        )
        XCTAssertTrue(store.beginCanonicalSync())
        try store.prepareCanonicalSync(configurationIdentifier: binding)
        _ = try store.bindCanonicalAuthoringMutation(
            queued.id,
            configurationIdentifier: binding
        )
        let submitted = try store.markCanonicalAuthoringMutationSubmitted(queued.id)

        XCTAssertTrue(submitted.hasBeenSubmitted)
        XCTAssertEqual(submitted.configurationIdentifier, binding)
        store.endCanonicalSync()
        XCTAssertThrowsError(try store.updateCanonicalAuthoringDraft(
            submitted.id,
            draft: .init(title: "Changed", timezoneName: "UTC")
        ))
        XCTAssertThrowsError(try store.discardCanonicalAuthoringMutation(submitted.id))

        let restored = PlannerStore(persistence: context.persistence)
        XCTAssertEqual(restored.pendingCanonicalAuthoringMutations, [submitted])
        XCTAssertEqual(restored.canonicalConfigurationIdentifier, binding)
    }

    func testCanonicalTrashResponseAndRestoreRoundTripFullDeletedItem() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "aa300000-0000-4000-8000-000000000003")!
        let active = try authoringItem(id: itemID, revision: 1, deleted: false)
        let deleted = try authoringItem(id: itemID, revision: 2, deleted: true)
        let restoredItem = try authoringItem(id: itemID, revision: 3, deleted: false)
        let store = PlannerStore(
            canonicalItems: [active],
            canonicalConfigurationIdentifier: authoringConfigurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )

        let trash = try store.enqueueCanonicalTrash(itemID: itemID)
        XCTAssertTrue(store.beginCanonicalSync())
        _ = try store.bindCanonicalAuthoringMutation(
            trash.id,
            configurationIdentifier: authoringConfigurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(trash.id)
        try store.applyCanonicalAuthoringResponse(trash.id, item: deleted)

        XCTAssertNil(store.canonicalItem(id: itemID))
        XCTAssertEqual(store.canonicalTrash.first?.lastKnownItem, deleted)
        XCTAssertEqual(store.canonicalTombstoneRevisions[itemID], 2)
        XCTAssertEqual(store.selectedCanonicalItemID, itemID)
        store.endCanonicalSync()

        let restore = try store.enqueueCanonicalRestore(itemID: itemID)
        XCTAssertTrue(store.beginCanonicalSync())
        _ = try store.bindCanonicalAuthoringMutation(
            restore.id,
            configurationIdentifier: authoringConfigurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(restore.id)
        let changedRestoredItem = try authoringItem(
            id: itemID,
            revision: 3,
            deleted: false,
            title: "Unexpected replacement content"
        )
        XCTAssertThrowsError(
            try store.applyCanonicalAuthoringResponse(restore.id, item: changedRestoredItem)
        ) { error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .invalidRemoteResponse)
        }
        XCTAssertEqual(store.canonicalTrash.first?.lastKnownItem, deleted)
        XCTAssertEqual(store.canonicalAuthoringMutation(id: restore.id)?.hasBeenSubmitted, true)
        try store.applyCanonicalAuthoringResponse(restore.id, item: restoredItem)

        XCTAssertEqual(store.canonicalItem(id: itemID), restoredItem)
        XCTAssertTrue(store.canonicalTrash.isEmpty)
        XCTAssertNil(store.canonicalTombstoneRevisions[itemID])
        XCTAssertTrue(store.pendingCanonicalAuthoringMutations.isEmpty)
        store.endCanonicalSync()

        let restarted = PlannerStore(persistence: context.persistence)
        XCTAssertEqual(restarted.canonicalItem(id: itemID), restoredItem)
        XCTAssertTrue(restarted.canonicalTrash.isEmpty)
    }

    func testCanonicalDeltaRetainsTrashAndNewerUpsertClearsIt() throws {
        let itemID = UUID(uuidString: "aa400000-0000-4000-8000-000000000004")!
        let active = try authoringItem(id: itemID, revision: 1, deleted: false)
        let newer = try authoringItem(id: itemID, revision: 3, deleted: false)
        let deletedAt = Date(timeIntervalSince1970: 1_800_000_300)
        let store = PlannerStore(canonicalItems: [active], restoreFromPersistence: false)

        store.applyCanonicalDelta(
            [.tombstone(.init(
                id: itemID,
                revision: 2,
                deletedAt: deletedAt,
                parentID: nil
            ))],
            nextCursor: "trash"
        )
        XCTAssertEqual(store.canonicalTrash.first?.lastKnownItem, active)
        XCTAssertEqual(store.canonicalTrash.first?.revision, 2)
        XCTAssertNil(store.canonicalItem(id: itemID))

        store.applyCanonicalDelta([.upsert(newer)], nextCursor: "restored")
        XCTAssertEqual(store.canonicalItem(id: itemID), newer)
        XCTAssertTrue(store.canonicalTrash.isEmpty)
    }

    func testCanonicalAuthoringRejectsDuplicatesSelfParentAndMutationFence() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = PlannerStore(persistence: context.persistence, restoreFromPersistence: false)
        let itemID = UUID(uuidString: "aa500000-0000-4000-8000-000000000005")!
        _ = try store.enqueueCanonicalCreate(
            itemID: itemID,
            draft: .init(title: "First", timezoneName: "UTC")
        )

        XCTAssertThrowsError(try store.enqueueCanonicalCreate(
            itemID: itemID,
            draft: .init(title: "Duplicate", timezoneName: "UTC")
        )) { error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .duplicateItemOperation)
        }
        let selfParentID = UUID()
        XCTAssertThrowsError(try store.enqueueCanonicalCreate(
            itemID: selfParentID,
            draft: .init(
                title: "Self parent",
                timezoneName: "UTC",
                parentID: selfParentID
            )
        ))

        XCTAssertTrue(store.beginCanonicalSync())
        XCTAssertThrowsError(try store.enqueueCanonicalCreate(
            draft: .init(title: "Fenced", timezoneName: "UTC")
        )) { error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .mutationFenceActive)
        }
        store.endCanonicalSync()

        let memoryOnly = PlannerStore(restoreFromPersistence: false)
        XCTAssertThrowsError(try memoryOnly.enqueueCanonicalCreate(
            draft: .init(title: "Not durable", timezoneName: "UTC")
        )) { error in
            XCTAssertEqual(
                error as? PlannerCanonicalAuthoringError,
                .encryptedPersistenceRequired
            )
        }
    }

    func testCanonicalAuthoringSyncTransitionsRequireTheOwnedFence() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "aa510000-0000-4000-8000-000000000005")!
        let response = try authoringItem(id: itemID, revision: 1, deleted: false)
        let store = PlannerStore(persistence: context.persistence, restoreFromPersistence: false)
        let queued = try store.enqueueCanonicalCreate(
            itemID: itemID,
            draft: DayWeaveCanonicalItemDraft(item: response)
        )

        XCTAssertThrowsError(try store.bindCanonicalAuthoringMutation(
            queued.id,
            configurationIdentifier: authoringConfigurationIdentifier
        )) { error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .mutationFenceActive)
        }

        XCTAssertTrue(store.beginCanonicalSync())
        try store.prepareCanonicalSync(
            configurationIdentifier: authoringConfigurationIdentifier
        )
        _ = try store.bindCanonicalAuthoringMutation(
            queued.id,
            configurationIdentifier: authoringConfigurationIdentifier
        )
        store.endCanonicalSync()

        XCTAssertThrowsError(try store.markCanonicalAuthoringMutationSubmitted(queued.id)) {
            error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .mutationFenceActive)
        }

        XCTAssertTrue(store.beginCanonicalSync())
        _ = try store.markCanonicalAuthoringMutationSubmitted(queued.id)
        store.endCanonicalSync()

        XCTAssertThrowsError(try store.markCanonicalAuthoringMutationConflicted(
            queued.id,
            diagnostic: "Must own sync fence"
        )) { error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .mutationFenceActive)
        }
        XCTAssertThrowsError(try store.applyCanonicalAuthoringResponse(
            queued.id,
            item: response
        )) { error in
            XCTAssertEqual(error as? PlannerCanonicalAuthoringError, .mutationFenceActive)
        }

        XCTAssertTrue(store.beginCanonicalSync())
        try store.applyCanonicalAuthoringResponse(queued.id, item: response)
        store.endCanonicalSync()
        XCTAssertEqual(store.canonicalItem(id: itemID), response)
        XCTAssertTrue(store.pendingCanonicalAuthoringMutations.isEmpty)
    }

    func testConflictedSubmittedDraftCanBeCopiedWithoutDestroyingTheOriginal() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sourceID = UUID(uuidString: "aa520000-0000-4000-8000-000000000005")!
        let copyID = UUID(uuidString: "aa520000-0000-4000-8000-000000000006")!
        let parentID = UUID(uuidString: "aa520000-0000-4000-8000-000000000007")!
        let parent = try authoringItem(id: parentID, revision: 1, deleted: false)
        let store = PlannerStore(
            canonicalItems: [parent],
            canonicalConfigurationIdentifier: authoringConfigurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let source = try store.enqueueCanonicalCreate(
            itemID: sourceID,
            draft: .init(
                status: .planned,
                title: "Preserve this exact draft",
                timezoneName: "UTC",
                parentID: parentID,
                siblingOrder: 42
            )
        )
        XCTAssertTrue(store.beginCanonicalSync())
        try store.prepareCanonicalSync(
            configurationIdentifier: authoringConfigurationIdentifier
        )
        _ = try store.bindCanonicalAuthoringMutation(
            source.id,
            configurationIdentifier: authoringConfigurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(source.id)
        let conflicted = try store.markCanonicalAuthoringMutationConflicted(
            source.id,
            diagnostic: "Server rejected the retained contract"
        )
        store.endCanonicalSync()

        let copy = try store.duplicateConflictedCanonicalDraft(source.id, as: copyID)

        XCTAssertEqual(store.canonicalAuthoringMutation(id: source.id), conflicted)
        XCTAssertTrue(conflicted.hasBeenSubmitted)
        XCTAssertEqual(conflicted.disposition, .conflicted)
        XCTAssertEqual(copy.itemID, copyID)
        XCTAssertNotEqual(copy.idempotencyKey, source.idempotencyKey)
        XCTAssertFalse(copy.hasBeenSubmitted)
        XCTAssertNil(copy.configurationIdentifier)
        XCTAssertEqual(copy.draft?.status, .inbox)
        XCTAssertNil(copy.draft?.parentID)
        XCTAssertEqual(copy.draft?.siblingOrder, 0)
        XCTAssertEqual(copy.draft?.title, "Preserve this exact draft")
    }

    func testCanonicalAuthoringEnqueueRollsBackOnConcurrentPersistenceFailure() throws {
        let context = try makeAuthoringPersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let seed = PlannerStore(persistence: context.persistence, restoreFromPersistence: false)
        seed.flushPersistence()
        let stale = PlannerStore(persistence: context.persistence)
        let writer = PlannerStore(persistence: context.persistence)
        writer.lastScheduleMessage = "A newer writer committed first"
        writer.flushPersistence()

        XCTAssertThrowsError(try stale.enqueueCanonicalCreate(
            draft: .init(title: "Must roll back", timezoneName: "UTC")
        )) { error in
            XCTAssertEqual(error as? PlannerPersistenceError, .concurrentModification)
        }
        XCTAssertTrue(stale.pendingCanonicalAuthoringMutations.isEmpty)
        XCTAssertNil(stale.selectedCanonicalItemID)
        XCTAssertEqual(stale.loadState, .persistenceFailed)
    }

    private var authoringConfigurationIdentifier: String {
        "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"
    }

    private func makeAuthoringPersistence() throws -> (
        directory: URL,
        fileURL: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveAuthoringStoreTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data(repeating: 41, count: 32))
        return (
            directory,
            fileURL,
            EncryptedPlannerPersistence(fileURL: fileURL, key: key)
        )
    }

    private func authoringItem(
        id: UUID,
        revision: UInt64,
        deleted: Bool,
        title: String = "Canonical authoring item"
    ) throws -> DayWeaveCanonicalItem {
        let deletedAt = deleted ? #""2027-01-15T12:00:00Z""# : "null"
        let data = Data(#"""
        {
          "id":"\#(id.uuidString.lowercased())","is_sensitive":false,
          "kind":"task","status":"inbox","title":"\#(title)",
          "notes":"Retained notes","timezone_name":"UTC","duration_seconds":1800,
          "deadline_at":null,"earliest_start_at":null,"recurrence":null,
          "flexible_constraints":{"energy":"deep"},
          "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
          "parent_id":null,"sibling_order":0,"is_executable":true,
          "revision":\#(revision),"created_at":"2027-01-15T10:00:00Z",
          "updated_at":"2027-01-15T12:00:00Z","completed_at":null,
          "deleted_at":\#(deletedAt)
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
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
