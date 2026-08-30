import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical authoring store", .serialized)
@MainActor
struct CanonicalAuthoringStoreTests {
    @Test("recoverable canonical trash follows the accepted thirty-day window")
    func trashRetentionPolicyIsThirtyDays() {
        #expect(PlannerStore.canonicalTrashRetentionInterval == 30 * 24 * 60 * 60)
    }

    @Test("scheduled Will do later journals an optimistic canonical earliest start")
    func scheduledMoveLaterUsesCanonicalReplacementJournal() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let itemID = UUID(uuidString: "aa050000-0000-4000-8000-000000000005")!
        let blockID = UUID(uuidString: "bb050000-0000-4000-8000-000000000005")!
        let item = try Self.item(id: itemID, revision: 4, deleted: false)
        let block = ScheduleBlock(
            id: blockID,
            title: item.title,
            kind: .task,
            start: now.addingTimeInterval(3_600),
            end: now.addingTimeInterval(5_400),
            status: .scheduled,
            project: nil,
            notes: item.notes ?? "",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: 4,
            occurrenceID: nil,
            sessionIndex: 0,
            syncOrigin: .canonicalPreview,
            placementReason: "Scheduled from canonical constraints",
            previewKind: "planned",
            occurrenceFullyScheduled: true
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        let chosenStart = now.addingTimeInterval(10_800)

        let mutation = try store.enqueueCanonicalMoveLater(
            blockID: blockID,
            earliestStart: chosenStart
        )

        #expect(mutation.operation == .replace)
        #expect(mutation.expectedRevision == 4)
        #expect(mutation.baseItem == item)
        #expect(mutation.draft?.earliestStartAt == chosenStart)
        #expect(mutation.draft?.recurrence == item.recurrence)
        #expect(store.blocks == [block])
        #expect(store.pendingCanonicalAuthoringMutations == [mutation])
    }

    @Test("a sensitive title-only Inbox draft is encrypted and restart-safe")
    func encryptedOfflineCreateRoundTrip() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "aa100000-0000-4000-8000-000000000001")!
        let title = "AUTHORING-SECRET-OFFLINE-TASK"
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )

        let mutation = try store.enqueueCanonicalCreate(
            itemID: itemID,
            draft: .init(
                isSensitive: true,
                title: title,
                notes: "Private offline authoring notes",
                timezoneName: "Europe/Madrid"
            )
        )

        #expect(store.blocks.isEmpty)
        #expect(store.pendingCanonicalAuthoringMutations == [mutation])
        #expect(store.selectedCanonicalItemID == itemID)
        let ciphertext = try Data(contentsOf: context.fileURL)
        #expect(ciphertext.range(of: Data(title.utf8)) == nil)

        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.pendingCanonicalAuthoringMutations == [mutation])
        #expect(restarted.selectedCanonicalItemID == itemID)
        #expect(restarted.blocks.isEmpty)
    }

    @Test("submitted authoring is immutable and sync transitions require the owned fence")
    func submittedMutationFenceAndRestart() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "aa200000-0000-4000-8000-000000000002")!
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        let queued = try store.enqueueCanonicalCreate(
            itemID: itemID,
            draft: .init(title: "Restart-safe create", timezoneName: "UTC")
        )

        #expect(throws: PlannerCanonicalAuthoringError.mutationFenceActive) {
            try store.bindCanonicalAuthoringMutation(
                queued.id,
                configurationIdentifier: Self.configurationIdentifier
            )
        }
        #expect(store.beginCanonicalSync())
        try store.prepareCanonicalSync(configurationIdentifier: Self.configurationIdentifier)
        _ = try store.bindCanonicalAuthoringMutation(
            queued.id,
            configurationIdentifier: Self.configurationIdentifier
        )
        let submitted = try store.markCanonicalAuthoringMutationSubmitted(queued.id)
        store.endCanonicalSync()

        #expect(submitted.hasBeenSubmitted)
        #expect(throws: PlannerCanonicalAuthoringError.submittedMutationIsImmutable) {
            try store.updateCanonicalAuthoringDraft(
                submitted.id,
                draft: .init(title: "Changed", timezoneName: "UTC")
            )
        }
        #expect(throws: PlannerCanonicalAuthoringError.submittedMutationIsImmutable) {
            try store.discardCanonicalAuthoringMutation(submitted.id)
        }
        #expect(throws: PlannerCanonicalAuthoringError.mutationFenceActive) {
            try store.markCanonicalAuthoringMutationConflicted(
                submitted.id,
                diagnostic: "Must own the sync fence"
            )
        }

        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.pendingCanonicalAuthoringMutations == [submitted])
        #expect(restarted.canonicalConfigurationIdentifier == Self.configurationIdentifier)
    }

    @Test("trash and restore retain full content and reject a changed restore response")
    func trashRestoreValidatesFullResponse() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID(uuidString: "aa300000-0000-4000-8000-000000000003")!
        let active = try Self.item(id: itemID, revision: 1, deleted: false)
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let restored = try Self.item(id: itemID, revision: 3, deleted: false)
        let store = PlannerStore(
            canonicalItems: [active],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )

        let trash = try store.enqueueCanonicalTrash(itemID: itemID)
        #expect(store.beginCanonicalSync())
        _ = try store.bindCanonicalAuthoringMutation(
            trash.id,
            configurationIdentifier: Self.configurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(trash.id)
        try store.applyCanonicalAuthoringResponse(trash.id, item: deleted)
        store.endCanonicalSync()

        #expect(store.canonicalItem(id: itemID) == nil)
        #expect(store.canonicalTrashEntry(id: itemID)?.lastKnownItem == deleted)

        let restore = try store.enqueueCanonicalRestore(itemID: itemID)
        #expect(store.beginCanonicalSync())
        _ = try store.bindCanonicalAuthoringMutation(
            restore.id,
            configurationIdentifier: Self.configurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(restore.id)
        let changed = try Self.item(
            id: itemID,
            revision: 3,
            deleted: false,
            title: "Unexpected replacement content"
        )
        #expect(throws: PlannerCanonicalAuthoringError.invalidRemoteResponse) {
            try store.applyCanonicalAuthoringResponse(restore.id, item: changed)
        }
        #expect(store.canonicalTrashEntry(id: itemID)?.lastKnownItem == deleted)
        #expect(store.canonicalAuthoringMutation(id: restore.id)?.hasBeenSubmitted == true)

        try store.applyCanonicalAuthoringResponse(restore.id, item: restored)
        store.endCanonicalSync()
        #expect(store.canonicalItem(id: itemID) == restored)
        #expect(store.canonicalTrash.isEmpty)
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
    }

    @Test("leaf-only deletion rejects either ordering with a child restore")
    func leafDeletionAndChildRestoreAreMutuallyExclusive() throws {
        let parentID = UUID()
        let childID = UUID()
        let parent = try Self.item(id: parentID, revision: 1, deleted: false)
        let deletedChild = try Self.item(
            id: childID,
            revision: 2,
            deleted: true,
            parentID: parentID
        )

        do {
            let context = try Self.makePersistence()
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let store = PlannerStore(
                canonicalItems: [parent],
                canonicalTombstoneRevisions: [childID: 2],
                canonicalTrash: [.init(item: deletedChild)],
                persistence: context.persistence,
                restoreFromPersistence: false
            )
            _ = try store.enqueueCanonicalRestore(itemID: childID)
            #expect(throws: PlannerCanonicalAuthoringError.invalidDraft) {
                try store.enqueueCanonicalTrash(itemID: parentID)
            }
        }

        do {
            let context = try Self.makePersistence()
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let store = PlannerStore(
                canonicalItems: [parent],
                canonicalTombstoneRevisions: [childID: 2],
                canonicalTrash: [.init(item: deletedChild)],
                persistence: context.persistence,
                restoreFromPersistence: false
            )
            _ = try store.enqueueCanonicalTrash(itemID: parentID)
            #expect(throws: PlannerCanonicalAuthoringError.invalidDraft) {
                try store.enqueueCanonicalRestore(itemID: childID)
            }
        }
    }

    @Test("a cross-device restore resolves or conflicts without corrupting restart state")
    func crossDeviceRestoreKeepsJournalInvariant() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let entry = DayWeaveCanonicalTrashEntry(item: deleted)
        let store = PlannerStore(
            canonicalTombstoneRevisions: [itemID: 2],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            canonicalTrash: [entry],
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let restore = try store.enqueueCanonicalRestore(itemID: itemID)
        let changed = try Self.item(
            id: itemID,
            revision: 3,
            deleted: false,
            title: "Restored and changed elsewhere"
        )

        store.applyCanonicalDelta([.upsert(changed)], nextCursor: "cross-device-restore")
        store.flushPersistence()

        #expect(store.loadState == .ready)
        #expect(store.canonicalItem(id: itemID) == changed)
        #expect(store.canonicalTrashEntry(id: itemID) == nil)
        #expect(store.canonicalAuthoringMutation(id: restore.id)?.disposition == .conflicted)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalItem(id: itemID) == changed)
        #expect(restarted.canonicalTrashEntry(id: itemID) == nil)
        #expect(restarted.canonicalAuthoringMutation(id: restore.id)?.disposition == .conflicted)
    }

    @Test("an exact cross-device restore clears the local restore journal")
    func exactCrossDeviceRestoreReconcilesJournal() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let restored = try Self.item(id: itemID, revision: 3, deleted: false)
        let store = PlannerStore(
            canonicalTombstoneRevisions: [itemID: 2],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let restore = try store.enqueueCanonicalRestore(itemID: itemID)

        store.applyCanonicalDelta([.upsert(restored)], nextCursor: "exact-restore")
        store.flushPersistence()

        #expect(store.loadState == .ready)
        #expect(store.canonicalItem(id: itemID) == restored)
        #expect(store.canonicalAuthoringMutation(id: restore.id) == nil)
        #expect(store.canonicalTrashEntry(id: itemID) == nil)
        #expect(PlannerCanonicalAuthoringJournalValidator.isValidState(
            mutations: store.pendingCanonicalAuthoringMutations,
            trash: store.canonicalTrash,
            canonicalItems: store.canonicalItems,
            tombstoneRevisions: store.canonicalTombstoneRevisions,
            configurationIdentifier: store.canonicalConfigurationIdentifier
        ))
    }

    @Test("a later tombstone in one delta page preserves queued restore intent")
    func finalDeltaTombstonePreservesRestore() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let restored = try Self.item(id: itemID, revision: 3, deleted: false)
        let store = PlannerStore(
            canonicalTombstoneRevisions: [itemID: 2],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let restore = try store.enqueueCanonicalRestore(itemID: itemID)
        let deletedAgainAt = Date(timeIntervalSince1970: 1_800_000_100)

        store.applyCanonicalDelta([
            .upsert(restored),
            .tombstone(.init(
                id: itemID,
                revision: 4,
                deletedAt: deletedAgainAt,
                parentID: nil
            )),
        ], nextCursor: "restore-then-delete")
        store.flushPersistence()

        #expect(store.loadState == .ready)
        #expect(store.canonicalItem(id: itemID) == nil)
        #expect(store.canonicalTrashEntry(id: itemID)?.revision == 4)
        #expect(store.canonicalAuthoringMutation(id: restore.id) != nil)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalAuthoringMutation(id: restore.id) != nil)
    }

    @Test("a final remote tombstone resolves queued trash and its body expires")
    func remoteTombstoneResolvesTrashJournal() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let active = try Self.item(id: itemID, revision: 1, deleted: false)
        let deletedAt = Date(timeIntervalSince1970: 1_800_000_000)
        let store = PlannerStore(
            canonicalItems: [active],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { deletedAt.addingTimeInterval(60) }
        )
        let trash = try store.enqueueCanonicalTrash(itemID: itemID)

        store.applyCanonicalDelta([.tombstone(.init(
            id: itemID,
            revision: 2,
            deletedAt: deletedAt,
            parentID: nil
        ))], nextCursor: "remote-trash")
        store.flushPersistence()

        #expect(store.canonicalAuthoringMutation(id: trash.id) == nil)
        #expect(store.canonicalTrashEntry(id: itemID)?.lastKnownItem == active)
        let expiredNow = deletedAt.addingTimeInterval(
            PlannerStore.canonicalTrashRetentionInterval + 60
        )
        let restarted = PlannerStore(
            persistence: context.persistence,
            now: { expiredNow }
        )
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalAuthoringMutation(id: trash.id) == nil)
        #expect(restarted.canonicalTrashEntry(id: itemID) == nil)
        #expect(try context.persistence.load()?.pendingCanonicalAuthoringMutations?.isEmpty == true)
        #expect(try context.persistence.load()?.canonicalTrash?.isEmpty == true)
    }

    @Test("an empty authoritative rebuild resolves only unsubmitted trash intent")
    func cursorRebuildResolvesUnsubmittedTrash() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let active = try Self.item(id: itemID, revision: 1, deleted: false)
        let store = PlannerStore(
            canonicalItems: [active],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let trash = try store.enqueueCanonicalTrash(itemID: itemID)

        store.replaceCanonicalState(changes: [], nextCursor: "rebuilt-empty")
        store.flushPersistence()

        #expect(store.canonicalAuthoringMutation(id: trash.id) == nil)
        #expect(store.canonicalItem(id: itemID) == nil)
        #expect(PlannerStore(persistence: context.persistence)
            .canonicalAuthoringMutation(id: trash.id) == nil)
    }

    @Test("an empty rebuild preserves submitted trash evidence for exact replay")
    func cursorRebuildPreservesSubmittedTrashEvidence() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let active = try Self.item(id: itemID, revision: 1, deleted: false)
        let store = PlannerStore(
            canonicalItems: [active],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let trash = try store.enqueueCanonicalTrash(itemID: itemID)
        #expect(store.beginCanonicalSync())
        _ = try store.bindCanonicalAuthoringMutation(
            trash.id,
            configurationIdentifier: Self.configurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(trash.id)
        store.endCanonicalSync()

        store.replaceCanonicalState(changes: [], nextCursor: "rebuilt-empty")
        store.flushPersistence()

        let retained = try #require(store.canonicalAuthoringMutation(id: trash.id))
        #expect(retained.hasBeenSubmitted)
        #expect(store.canonicalTrashEntry(id: itemID)?.revision == 2)
        #expect(store.canonicalTombstoneRevisions[itemID] == 2)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalAuthoringMutation(id: trash.id)?.hasBeenSubmitted == true)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.revision == 2)
    }

    @Test("cursor rebuild retains deleted evidence pinned by a restore journal")
    func cursorRebuildPreservesRestoreEvidence() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let store = PlannerStore(
            canonicalTombstoneRevisions: [itemID: 2],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let restore = try store.enqueueCanonicalRestore(itemID: itemID)

        store.replaceCanonicalState(changes: [], nextCursor: "rebuilt-empty")
        store.flushPersistence()

        #expect(store.loadState == .ready)
        #expect(store.canonicalTombstoneRevisions[itemID] == 2)
        #expect(store.canonicalTrashEntry(id: itemID)?.revision == 2)
        #expect(store.canonicalAuthoringMutation(id: restore.id) != nil)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.revision == 2)
        #expect(restarted.canonicalAuthoringMutation(id: restore.id) != nil)
    }

    @Test("cursor rebuild normalizes active restore conflicts into recovery metadata")
    func cursorRebuildPreservesActiveRestoreConflict() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let changed = try Self.item(
            id: itemID,
            revision: 3,
            deleted: false,
            title: "Different active version"
        )
        let store = PlannerStore(
            canonicalTombstoneRevisions: [itemID: 2],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        let restore = try store.enqueueCanonicalRestore(itemID: itemID)
        store.applyCanonicalDelta([.upsert(changed)], nextCursor: "active-conflict")
        #expect(store.canonicalTrashEntry(id: itemID) == nil)
        #expect(store.canonicalAuthoringMutation(id: restore.id)?.disposition == .conflicted)

        store.replaceCanonicalState(changes: [], nextCursor: "rebuilt-empty")
        store.flushPersistence()

        #expect(store.loadState == .ready)
        #expect(store.canonicalItem(id: itemID) == nil)
        #expect(store.canonicalTrashEntry(id: itemID)?.revision == 2)
        #expect(store.canonicalTrashEntry(id: itemID)?.lastKnownItem == deleted)
        #expect(store.canonicalAuthoringMutation(id: restore.id)?.disposition == .conflicted)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.revision == 2)
        #expect(restarted.canonicalAuthoringMutation(id: restore.id)?.disposition == .conflicted)
    }

    @Test("a conflicted submitted draft copies to a detached editable Inbox identity")
    func conflictedDraftCopyPreservesRecovery() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sourceID = UUID(uuidString: "aa400000-0000-4000-8000-000000000004")!
        let copyID = UUID(uuidString: "aa400000-0000-4000-8000-000000000005")!
        let parentID = UUID(uuidString: "aa400000-0000-4000-8000-000000000006")!
        let parent = try Self.item(id: parentID, revision: 1, deleted: false)
        let store = PlannerStore(
            canonicalItems: [parent],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
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
        #expect(store.beginCanonicalSync())
        try store.prepareCanonicalSync(configurationIdentifier: Self.configurationIdentifier)
        _ = try store.bindCanonicalAuthoringMutation(
            source.id,
            configurationIdentifier: Self.configurationIdentifier
        )
        _ = try store.markCanonicalAuthoringMutationSubmitted(source.id)
        let conflict = try store.markCanonicalAuthoringMutationConflicted(
            source.id,
            diagnostic: "Server rejected the retained contract"
        )
        store.endCanonicalSync()

        let copy = try store.duplicateConflictedCanonicalDraft(source.id, as: copyID)

        #expect(store.canonicalAuthoringMutation(id: source.id) == conflict)
        #expect(copy.itemID == copyID)
        #expect(copy.idempotencyKey != source.idempotencyKey)
        #expect(!copy.hasBeenSubmitted)
        #expect(copy.configurationIdentifier == nil)
        #expect(copy.draft?.status == .inbox)
        #expect(copy.draft?.parentID == nil)
        #expect(copy.draft?.siblingOrder == 0)
        #expect(copy.draft?.title == "Preserve this exact draft")
    }

    @Test("a concurrent persistence failure rolls authoring state back in memory")
    func enqueueRollbackOnConcurrentWriter() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let seed = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        seed.flushPersistence()
        let stale = PlannerStore(persistence: context.persistence)
        let writer = PlannerStore(persistence: context.persistence)
        writer.lastScheduleMessage = "A newer writer committed first"
        writer.flushPersistence()

        #expect(throws: PlannerPersistenceError.concurrentModification) {
            try stale.enqueueCanonicalCreate(
                draft: .init(title: "Must roll back", timezoneName: "UTC")
            )
        }
        #expect(stale.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(stale.selectedCanonicalItemID == nil)
        #expect(stale.loadState == .persistenceFailed)
    }

    @Test("failed discard restores expired metadata pinned by a restore journal")
    func discardRestoreRollbackKeepsValidatorEvidence() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let deletedAt = try #require(deleted.deletedAt)
        let expiredNow = deletedAt.addingTimeInterval(
            PlannerStore.canonicalTrashRetentionInterval + 60
        )
        let restore = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .restore,
            expectedRevision: deleted.revision,
            baseItem: deleted,
            createdAt: deletedAt.addingTimeInterval(60)
        )
        let seed = PlannerStore(
            canonicalTombstoneRevisions: [itemID: deleted.revision],
            pendingCanonicalAuthoringMutations: [restore],
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { expiredNow }
        )
        seed.flushPersistence()
        let stale = PlannerStore(persistence: context.persistence, now: { expiredNow })
        let writer = PlannerStore(persistence: context.persistence, now: { expiredNow })
        writer.lastScheduleMessage = "Concurrent writer"
        writer.flushPersistence()

        #expect(throws: PlannerPersistenceError.concurrentModification) {
            try stale.discardCanonicalAuthoringMutation(restore.id)
        }

        #expect(stale.canonicalAuthoringMutation(id: restore.id) != nil)
        #expect(stale.canonicalTrashEntry(id: itemID) != nil)
        #expect(PlannerCanonicalAuthoringJournalValidator.isValidState(
            mutations: stale.pendingCanonicalAuthoringMutations,
            trash: stale.canonicalTrash,
            canonicalItems: stale.canonicalItems,
            tombstoneRevisions: stale.canonicalTombstoneRevisions,
            configurationIdentifier: stale.canonicalConfigurationIdentifier
        ))
        let restarted = PlannerStore(persistence: context.persistence, now: { expiredNow })
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalAuthoringMutation(id: restore.id) != nil)
        #expect(restarted.canonicalTrashEntry(id: itemID) != nil)
    }

    @Test("recently deleted metadata and retained bodies are age, count, and byte bounded")
    func recentlyDeletedRetentionIsBounded() throws {
        let referenceDate = Date(timeIntervalSince1970: 1_800_100_000)
        var entries: [DayWeaveCanonicalTrashEntry] = []
        for offset in 0..<(PlannerStore.maximumCanonicalTrashEntries + 5) {
            entries.append(.init(
                id: UUID(),
                revision: 1,
                deletedAt: referenceDate.addingTimeInterval(-TimeInterval(offset)),
                parentID: nil,
                lastKnownItem: nil
            ))
        }
        let expiredID = UUID()
        entries.append(.init(
            id: expiredID,
            revision: 1,
            deletedAt: referenceDate.addingTimeInterval(
                -PlannerStore.canonicalTrashRetentionInterval - 1
            ),
            parentID: nil,
            lastKnownItem: nil
        ))
        let pinnedExpiredID = UUID()
        entries.append(.init(
            id: pinnedExpiredID,
            revision: 1,
            deletedAt: referenceDate.addingTimeInterval(
                -PlannerStore.canonicalTrashRetentionInterval - 2
            ),
            parentID: nil,
            lastKnownItem: nil
        ))
        let oversizedID = UUID()
        let oversized = try Self.item(
            id: oversizedID,
            revision: 1,
            deleted: true,
            notes: String(
                repeating: "x",
                count: PlannerStore.maximumCanonicalTrashItemBytes + 1
            )
        )
        entries.append(.init(
            id: oversizedID,
            revision: 1,
            deletedAt: referenceDate.addingTimeInterval(1),
            parentID: nil,
            lastKnownItem: oversized
        ))
        let tombstones = Dictionary(uniqueKeysWithValues: entries.map { ($0.id, $0.revision) })

        let store = PlannerStore(
            canonicalTombstoneRevisions: tombstones,
            pendingCanonicalAuthoringMutations: [
                .init(
                    itemID: pinnedExpiredID,
                    operation: .restore,
                    expectedRevision: 1
                ),
            ],
            canonicalTrash: entries,
            restoreFromPersistence: false,
            now: { referenceDate }
        )

        #expect(store.canonicalTrash.count == PlannerStore.maximumCanonicalTrashEntries)
        #expect(store.canonicalTrash.first(where: { $0.id == oversizedID })?.lastKnownItem == nil)
        #expect(!store.canonicalTrash.contains { $0.id == expiredID })
        #expect(store.canonicalTrash.contains { $0.id == pinnedExpiredID })
        #expect(store.canonicalAuthoringMutation(itemID: pinnedExpiredID)?.operation == .restore)
        #expect(store.canonicalTombstoneRevisions[expiredID] == 1)
    }

    @Test("startup durably expires every retained restore body while preserving request identity")
    func startupRewritesExpiredTrashBody() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let deletedAt = try #require(deleted.deletedAt)
        let restore = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .restore,
            expectedRevision: deleted.revision,
            baseItem: deleted,
            createdAt: deletedAt.addingTimeInterval(60)
        )
        let seed = PlannerStore(
            canonicalTombstoneRevisions: [itemID: deleted.revision],
            pendingCanonicalAuthoringMutations: [restore],
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { deletedAt.addingTimeInterval(24 * 60 * 60) }
        )
        seed.flushPersistence()
        #expect(try context.persistence.load()?.canonicalTrash?.first?.lastKnownItem != nil)

        let expiredNow = deletedAt.addingTimeInterval(
            PlannerStore.canonicalTrashRetentionInterval + 60
        )
        let restarted = PlannerStore(
            persistence: context.persistence,
            now: { expiredNow }
        )

        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.lastKnownItem == nil)
        let boundedRestore = try #require(restarted.canonicalAuthoringMutation(id: restore.id))
        #expect(boundedRestore.itemID == restore.itemID)
        #expect(boundedRestore.idempotencyKey == restore.idempotencyKey)
        #expect(boundedRestore.expectedRevision == restore.expectedRevision)
        #expect(boundedRestore.baseItem == nil)
        #expect(try context.persistence.load()?.canonicalTrash?.first?.lastKnownItem == nil)
        #expect(
            try context.persistence.load()?
                .pendingCanonicalAuthoringMutations?.first?.baseItem == nil
        )
    }

    @Test("future remote deletion times cannot extend retained body lifetime")
    func futureDeletedAtUsesLocalRetentionAnchor() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let observedAt = Date(timeIntervalSince1970: 1_788_112_000)
        let restore = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .restore,
            expectedRevision: deleted.revision,
            baseItem: deleted,
            createdAt: observedAt
        )
        let seed = PlannerStore(
            canonicalTombstoneRevisions: [itemID: deleted.revision],
            pendingCanonicalAuthoringMutations: [restore],
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { observedAt }
        )
        seed.flushPersistence()

        #expect(seed.canonicalTrashEntry(id: itemID)?.deletedAt == observedAt)
        #expect(seed.canonicalTrashEntry(id: itemID)?.lastKnownItem != nil)
        let expired = observedAt.addingTimeInterval(
            PlannerStore.canonicalTrashRetentionInterval + 60
        )
        let restarted = PlannerStore(persistence: context.persistence, now: { expired })

        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.lastKnownItem == nil)
        #expect(restarted.canonicalAuthoringMutation(id: restore.id)?.baseItem == nil)
    }

    @Test("submitted trash keeps replay identity but expires its retained body")
    func submittedTrashBodyExpiresWithoutLosingReplay() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let active = try Self.item(id: itemID, revision: 1, deleted: false)
        let deleted = try Self.item(id: itemID, revision: 2, deleted: true)
        let createdAt = Date(timeIntervalSince1970: 1_800_000_000)
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: itemID,
            operation: .trash,
            expectedRevision: active.revision,
            baseItem: active,
            createdAt: createdAt,
            configurationIdentifier: Self.configurationIdentifier,
            hasBeenSubmitted: true
        )
        let seed = PlannerStore(
            canonicalTombstoneRevisions: [itemID: deleted.revision],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            pendingCanonicalAuthoringMutations: [mutation],
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { createdAt.addingTimeInterval(60) }
        )
        seed.flushPersistence()

        let expired = createdAt.addingTimeInterval(
            PlannerStore.canonicalTrashRetentionInterval + 60
        )
        let restarted = PlannerStore(persistence: context.persistence, now: { expired })
        let retained = try #require(restarted.canonicalAuthoringMutation(id: mutation.id))
        #expect(retained.idempotencyKey == mutation.idempotencyKey)
        #expect(retained.hasBeenSubmitted)
        #expect(retained.baseItem == nil)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.lastKnownItem == nil)

        #expect(restarted.beginCanonicalSync())
        try restarted.applyCanonicalAuthoringResponse(mutation.id, item: deleted)
        restarted.endCanonicalSync()
        #expect(restarted.canonicalAuthoringMutation(id: mutation.id) == nil)
        #expect(restarted.canonicalTrashEntry(id: itemID)?.revision == 2)
    }

    @Test("the authoring journal byte budget rejects before persistence is wedged")
    func authoringJournalCapacityIsPreflighted() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let largeNotes = String(repeating: "\u{0001}", count: 100_000)
        let createdAt = Date(timeIntervalSince1970: 1_800_000_000)
        var retained: [DayWeavePendingCanonicalAuthoringMutation] = []
        var rejected: DayWeavePendingCanonicalAuthoringMutation?
        for index in 0..<PlannerCanonicalAuthoringJournalValidator.maximumMutations {
            let mutation = DayWeavePendingCanonicalAuthoringMutation(
                itemID: UUID(),
                operation: .create,
                draft: .init(
                    title: "Large offline item \(index)",
                    notes: largeNotes,
                    timezoneName: "UTC"
                ),
                createdAt: createdAt.addingTimeInterval(TimeInterval(index))
            )
            let candidate = retained + [mutation]
            if PlannerCanonicalAuthoringJournalValidator.isValidState(
                mutations: candidate,
                trash: [],
                canonicalItems: [],
                tombstoneRevisions: [:],
                configurationIdentifier: nil
            ) {
                retained = candidate
            } else {
                rejected = mutation
                break
            }
        }
        let overflow = try #require(rejected)
        #expect(!retained.isEmpty)
        #expect(retained.count < PlannerCanonicalAuthoringJournalValidator.maximumMutations)

        let store = PlannerStore(
            pendingCanonicalAuthoringMutations: retained,
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        store.flushPersistence()
        #expect(store.loadState == .ready)
        #expect(store.persistenceError == nil)

        #expect(throws: PlannerCanonicalAuthoringError.journalCapacityReached) {
            try store.enqueueCanonicalCreate(
                itemID: overflow.itemID,
                draft: try #require(overflow.draft)
            )
        }
        #expect(store.pendingCanonicalAuthoringMutations == retained)
        #expect(store.loadState == .ready)
        #expect(store.persistenceError == nil)
        #expect(try context.persistence.load()?.pendingCanonicalAuthoringMutations == retained)
    }

    @Test("whole-snapshot capacity rejects authoring without poisoning persistence")
    func wholeSnapshotCapacityIsPreflighted() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let createdAt = Date(timeIntervalSince1970: 1_800_000_000)
        let retainedMessage = AssistantMessage(
            id: UUID(),
            role: .user,
            text: String(
                repeating: "x",
                count: EncryptedPlannerPersistence.maximumPlaintextBytes - 300_000
            ),
            createdAt: createdAt
        )
        let store = PlannerStore(
            assistantMessages: [retainedMessage],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { createdAt }
        )
        store.flushPersistence()
        let retainedSnapshot = try #require(try context.persistence.load())
        #expect(store.loadState == .ready)

        #expect(throws: PlannerPersistenceError.snapshotTooLarge(
            limitBytes: EncryptedPlannerPersistence.maximumPlaintextBytes
        )) {
            try store.enqueueCanonicalCreate(
                draft: .init(
                    title: "Large but individually valid authoring body",
                    notes: String(repeating: "\u{0001}", count: 100_000),
                    timezoneName: "UTC"
                )
            )
        }

        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(store.loadState == .ready)
        #expect(store.persistenceError == nil)
        #expect(try context.persistence.load() == retainedSnapshot)
    }

    @Test("configuration reset detaches server parents but preserves local hierarchy")
    func resetDetachesOnlyServerParentLinks() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let serverParentID = UUID()
        let localParentID = UUID()
        let localChildID = UUID()
        let detachedChildID = UUID()
        let serverParent = try Self.item(
            id: serverParentID,
            revision: 1,
            deleted: false,
            isSensitive: true
        )
        let store = PlannerStore(
            canonicalItems: [serverParent],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        _ = try store.enqueueCanonicalCreate(
            itemID: localParentID,
            draft: .init(title: "Local parent", timezoneName: "UTC")
        )
        _ = try store.enqueueCanonicalCreate(
            itemID: localChildID,
            draft: .init(
                title: "Local child",
                timezoneName: "UTC",
                parentID: localParentID,
                siblingOrder: 7
            )
        )
        _ = try store.enqueueCanonicalCreate(
            itemID: detachedChildID,
            draft: .init(
                title: "Was under server parent",
                timezoneName: "UTC",
                parentID: serverParentID,
                siblingOrder: 9
            )
        )

        store.resetCanonicalSyncState()

        #expect(store.canonicalItems.isEmpty)
        #expect(store.canonicalConfigurationIdentifier == nil)
        #expect(store.canonicalAuthoringMutation(itemID: localChildID)?.draft?.parentID == localParentID)
        #expect(store.canonicalAuthoringMutation(itemID: localChildID)?.draft?.siblingOrder == 7)
        #expect(store.canonicalAuthoringMutation(itemID: detachedChildID)?.draft?.parentID == nil)
        #expect(store.canonicalAuthoringMutation(itemID: detachedChildID)?.draft?.siblingOrder == 0)
        #expect(store.canonicalAuthoringMutation(itemID: detachedChildID)?.draft?.isSensitive == true)
    }

    @Test("credential quarantine promotes detached inherited privacy and persists it")
    func credentialQuarantineHardensDetachedLocalCreate() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let parentID = UUID()
        let childID = UUID()
        let parent = try Self.item(
            id: parentID,
            revision: 1,
            deleted: false,
            isSensitive: true
        )
        let store = PlannerStore(
            canonicalItems: [parent],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        _ = try store.enqueueCanonicalCreate(
            itemID: childID,
            draft: .init(
                title: "Inherited private local draft",
                timezoneName: "UTC",
                parentID: parentID,
                siblingOrder: 4
            )
        )

        try store.prepareForExecutionCredentialReplacement()

        let preserved = try #require(store.canonicalAuthoringMutation(itemID: childID))
        #expect(preserved.draft?.parentID == nil)
        #expect(preserved.draft?.siblingOrder == 0)
        #expect(preserved.draft?.isSensitive == true)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalAuthoringMutation(itemID: childID)?.draft?.isSensitive == true)
    }

    @Test("copying an inherited-sensitive conflict promotes privacy before detaching")
    func conflictedCopyHardensInheritedSensitivity() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let parentID = UUID()
        let sourceID = UUID()
        let copyID = UUID()
        let parent = try Self.item(
            id: parentID,
            revision: 1,
            deleted: false,
            isSensitive: true
        )
        var source = DayWeavePendingCanonicalAuthoringMutation(
            itemID: sourceID,
            operation: .create,
            draft: .init(
                title: "Private through ancestry",
                timezoneName: "UTC",
                parentID: parentID
            )
        )
        source.disposition = .conflicted
        source.diagnostic = "Synthetic conflict"
        let store = PlannerStore(
            canonicalItems: [parent],
            pendingCanonicalAuthoringMutations: [source],
            persistence: context.persistence,
            restoreFromPersistence: false
        )

        let copy = try store.duplicateConflictedCanonicalDraft(source.id, as: copyID)

        #expect(copy.itemID == copyID)
        #expect(copy.draft?.parentID == nil)
        #expect(copy.draft?.isSensitive == true)
        let restarted = PlannerStore(persistence: context.persistence)
        #expect(restarted.loadState == .ready)
        #expect(restarted.canonicalAuthoringMutation(itemID: copyID)?.draft?.isSensitive == true)
    }

    @Test("queued authoring blocks execution edits and reverse status capture")
    func authoringJournalExcludesExecutionStatusIntent() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let item = try Self.item(id: itemID, revision: 1, deleted: false)
        let start = Date(timeIntervalSince1970: 1_800_000_000)
        let block = ScheduleBlock(
            id: UUID(),
            title: item.title,
            kind: .task,
            start: start,
            end: start.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: item.revision,
            syncOrigin: .local,
            previewKind: "planned"
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            persistence: context.persistence,
            restoreFromPersistence: false
        )
        var draft = DayWeaveCanonicalItemDraft(item: item)
        draft.title = "Queued replacement"
        _ = try store.enqueueCanonicalReplace(itemID: itemID, draft: draft)

        #expect(!store.canMutate(block))
        store.complete(block.id)
        #expect(store.blocks.first?.status == .scheduled)
        store.capturePendingCanonicalMutations()
        #expect(store.pendingCanonicalMutations.isEmpty)
    }

    @Test("Quick Capture remains available while a different item is executing")
    func unrelatedCaptureIsAllowedDuringExecution() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let activeItemID = UUID()
        let captureID = UUID()
        let activeItem = try Self.item(id: activeItemID, revision: 1, deleted: false)
        let session = try Self.activeExecutionSession(itemID: activeItemID)
        var executionState = DayWeaveExecutionDurableState.empty
        executionState.deviceID = session.sourceDeviceID
        executionState.revision = session.revision
        executionState.activeSession = session
        let store = PlannerStore(
            canonicalItems: [activeItem],
            executionState: executionState,
            persistence: context.persistence,
            restoreFromPersistence: false
        )

        let capture = try store.enqueueCanonicalCreate(
            itemID: captureID,
            draft: .init(title: "Remember this while focused", timezoneName: "UTC")
        )

        #expect(capture.itemID == captureID)
        #expect(store.canonicalAuthoringMutation(id: capture.id) == capture)
        var activeDraft = DayWeaveCanonicalItemDraft(item: activeItem)
        activeDraft.title = "Do not rewrite the executing item"
        #expect(throws: PlannerCanonicalAuthoringError.activeExecution) {
            try store.enqueueCanonicalReplace(itemID: activeItemID, draft: activeDraft)
        }
    }

    private static let configurationIdentifier =
        "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"

    private static func makePersistence() throws -> (
        directory: URL,
        fileURL: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveCanonicalAuthoringStoreTests-\(UUID().uuidString)",
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

    private static func item(
        id: UUID,
        revision: UInt64,
        deleted: Bool,
        title: String = "Canonical authoring item",
        notes: String = "Retained notes",
        parentID: UUID? = nil,
        isSensitive: Bool = false
    ) throws -> DayWeaveCanonicalItem {
        let deletedAt = deleted ? #""2027-01-15T12:00:00Z""# : "null"
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        let data = Data(#"""
        {
          "id":"\#(id.uuidString.lowercased())","is_sensitive":\#(isSensitive),
          "kind":"task","status":"inbox","title":"\#(title)",
          "notes":"\#(notes)","timezone_name":"UTC","duration_seconds":1800,
          "deadline_at":null,"earliest_start_at":null,"recurrence":null,
          "flexible_constraints":{"energy":"deep"},
          "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
          "parent_id":\#(parent),"sibling_order":0,"is_executable":true,
          "revision":\#(revision),"created_at":"2027-01-15T10:00:00Z",
          "updated_at":"2027-01-15T12:00:00Z","completed_at":null,
          "deleted_at":\#(deletedAt)
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func activeExecutionSession(
        itemID: UUID
    ) throws -> DayWeaveExecutionSession {
        let sessionID = UUID()
        let deviceID = UUID()
        let data = Data(#"""
        {
          "id":"\#(sessionID.uuidString.lowercased())",
          "item_id":"\#(itemID.uuidString.lowercased())","item_revision":1,
          "occurrence_id":null,"session_index":0,"planned_block_id":null,
          "source_device_id":"\#(deviceID.uuidString.lowercased())",
          "status":"active","revision":1,"accumulated_seconds":0,
          "actual_seconds":null,"started_at":"2027-01-15T10:00:00Z",
          "running_since":"2027-01-15T10:00:00Z","paused_at":null,
          "pause_until":null,"pause_reason":null,"ended_at":null,
          "created_at":"2027-01-15T10:00:00Z",
          "updated_at":"2027-01-15T10:00:00Z"
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveExecutionSession.self, from: data)
    }
}
#endif
