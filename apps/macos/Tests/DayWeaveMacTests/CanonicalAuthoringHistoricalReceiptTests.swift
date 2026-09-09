import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical authoring historical receipts", .serialized)
@MainActor
struct CanonicalAuthoringHistoricalReceiptTests {
    @Test("historical create and replace receipts preserve newer lifecycle and content")
    func newerActiveSettlesOnlyItsJournal() throws {
        for operation in [CanonicalAuthoringOperation.create, .replace] {
            let context = try Context()
            defer { context.remove() }
            let response = try item(revision: operation == .create ? 1 : 2)
            let newer = try item(revision: 4, status: "completed", title: "Newer synthetic content", sensitive: true)
            let mutation = try submitted(operation, response: response)
            let store = try context.store(items: [newer], mutation: mutation,
                anchor: .init(itemID: Self.itemID, canonicalRevision: newer.revision))
            let before = Projection(store)
            #expect(store.beginCanonicalSync())
            try store.applyCanonicalAuthoringResponse(mutation.id, item: response)
            store.endCanonicalSync()
            #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
            #expect(Projection(store) == before)
            #expect(store.canonicalItemRequiresSensitivePresentation(itemID: Self.itemID))
            let restarted = PlannerStore(persistence: context.persistence, now: { Self.now })
            #expect(restarted.loadState == .ready)
            #expect(restarted.pendingCanonicalAuthoringMutations.isEmpty)
            #expect(Projection(restarted) == before)
        }
    }

    @Test("historical trash cannot remove a newer restored item or change selection")
    func historicalTrashAfterRestore() throws {
        let context = try Context()
        defer { context.remove() }
        let response = try item(revision: 2, deleted: true)
        let newer = try item(revision: 3, title: "Restored newer content", sensitive: true)
        let mutation = try submitted(.trash, response: response)
        let store = try context.store(items: [newer], mutation: mutation)
        let before = Projection(store)
        #expect(store.beginCanonicalSync())
        try store.applyCanonicalAuthoringResponse(mutation.id, item: response)
        store.endCanonicalSync()
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(Projection(store) == before)
    }

    @Test("historical restore cannot erase a newer trash body or tombstone")
    func historicalRestoreAfterNewerTrash() throws {
        let context = try Context()
        defer { context.remove() }
        let response = try item(revision: 3)
        let newer = try item(revision: 4, title: "Newer synthetic deletion", sensitive: true, deleted: true)
        let mutation = try submitted(.restore, response: response)
        let store = try context.store(trash: [.init(item: newer)],
            tombstones: [Self.itemID: newer.revision], mutation: mutation)
        let before = Projection(store)
        #expect(store.beginCanonicalSync())
        try store.applyCanonicalAuthoringResponse(mutation.id, item: response)
        store.endCanonicalSync()
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(Projection(store) == before)
        #expect(store.canonicalTrashEntry(id: Self.itemID)?.isSensitive == true)
    }

    @Test("a superseding tombstone without retained content survives historical settlement")
    func supersedingTombstoneOnly() throws {
        for operation in [CanonicalAuthoringOperation.create, .replace, .trash] {
            let context = try Context()
            defer { context.remove() }
            let response = try item(revision: operation == .create ? 1 : 2, deleted: operation == .trash)
            let mutation = try submitted(operation, response: response)
            let store = try context.store(tombstones: [Self.itemID: 5], mutation: mutation)
            let before = Projection(store)
            #expect(store.beginCanonicalSync())
            try store.applyCanonicalAuthoringResponse(mutation.id, item: response)
            store.endCanonicalSync()
            #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
            #expect(Projection(store) == before)
            #expect(store.canonicalItem(id: Self.itemID) == nil)
            #expect(store.canonicalTrashEntry(id: Self.itemID) == nil)
        }
    }

    @Test("equal-revision active, trash and tombstone contradictions are rejected")
    func equalRevisionContradictionsRemainRejected() throws {
        for shape in ["active", "trash-receipt", "trash-body", "tombstone"] {
            let context = try Context()
            defer { context.remove() }
            let isTrashReceipt = shape == "trash-receipt" || shape == "trash-body"
            let response = try item(revision: 2, deleted: isTrashReceipt)
            let mutation = try submitted(isTrashReceipt ? .trash : .replace, response: response)
            let contradictory = try item(revision: 2, title: "Conflicting equal revision", deleted: shape == "trash-body")
            let store = try context.store(items: shape == "active" || shape == "trash-receipt" ? [contradictory] : [],
                trash: shape == "trash-body" ? [.init(item: contradictory)] : [],
                tombstones: shape == "trash-body" || shape == "tombstone" ? [Self.itemID: 2] : [:],
                mutation: mutation)
            let before = Projection(store)
            #expect(store.beginCanonicalSync())
            #expect(throws: PlannerCanonicalAuthoringError.invalidRemoteResponse) {
                try store.applyCanonicalAuthoringResponse(mutation.id, item: response)
            }
            store.endCanonicalSync()
            #expect(store.pendingCanonicalAuthoringMutations == [mutation])
            #expect(Projection(store) == before)
        }
    }

    @Test("newer observations do not bypass exact receipt content or minimum revision")
    func supersessionDoesNotRelaxReceiptValidation() throws {
        let context = try Context()
        defer { context.remove() }
        let response = try item(revision: 2)
        let mutation = try submitted(.replace, response: response)
        let newer = try item(revision: 4, status: "completed", sensitive: true)
        let store = try context.store(items: [newer], mutation: mutation)
        let before = Projection(store)
        #expect(store.beginCanonicalSync())
        for invalid in [try item(revision: 2, title: "Not the reviewed receipt"), try item(revision: 1)] {
            #expect(throws: PlannerCanonicalAuthoringError.invalidRemoteResponse) {
                try store.applyCanonicalAuthoringResponse(mutation.id, item: invalid)
            }
            #expect(store.pendingCanonicalAuthoringMutations == [mutation])
            #expect(Projection(store) == before)
        }
        store.endCanonicalSync()
    }

    @Test("historical create cannot promote an unconfirmed onboarding designation")
    func obsoleteLocalAnchorIsNotPromoted() throws {
        let context = try Context()
        defer { context.remove() }
        let response = try item(revision: 1, status: "planned")
        let mutation = try submitted(.create, response: response)
        let newer = try item(revision: 4, status: "completed", title: "Later completed item", sensitive: true)
        let store = try context.store(items: [newer], mutation: mutation,
            anchor: .init(itemID: Self.itemID, canonicalRevision: nil))
        #expect(store.onboardingFirstItemAnchor?.canonicalRevision == nil)
        #expect(store.onboardingFirstItemAnchor != nil)
        let before = Projection(store)
        #expect(store.beginCanonicalSync())
        try store.applyCanonicalAuthoringResponse(mutation.id, item: response)
        store.endCanonicalSync()
        #expect(store.onboardingFirstItemAnchor == nil)
        #expect(store.canonicalItems == before.items)
        #expect(store.canonicalTrash == before.trash)
        #expect(store.canonicalTombstoneRevisions == before.tombstones)
        #expect(store.selectedCanonicalItemID == before.selection)
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        let restarted = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(restarted.loadState == .ready)
        #expect(restarted.onboardingFirstItemAnchor == nil)
    }

    @Test("failed historical settlement rolls back all state and restart retains exact replay")
    func failedSaveAndEncryptedRestartPreserveExactCustody() throws {
        let context = try Context()
        defer { context.remove() }
        let response = try item(revision: 2)
        let mutation = try submitted(.replace, response: response)
        let newer = try item(revision: 4, status: "completed", title: "Newer private canonical record", sensitive: true)
        let seed = try context.store(items: [newer], mutation: mutation,
            anchor: .init(itemID: Self.itemID, canonicalRevision: 4))
        let before = Projection(seed)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let exactJournal = try encoder.encode(seed.pendingCanonicalAuthoringMutations)
        let stale = PlannerStore(persistence: context.persistence, now: { Self.now })
        let writer = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(try encoder.encode(stale.pendingCanonicalAuthoringMutations) == exactJournal)
        writer.lastScheduleMessage = "Synthetic concurrent writer"
        writer.flushPersistence()
        #expect(stale.beginCanonicalSync())
        #expect(throws: PlannerPersistenceError.concurrentModification) {
            try stale.applyCanonicalAuthoringResponse(mutation.id, item: response)
        }
        stale.endCanonicalSync()
        #expect(stale.loadState == .persistenceFailed)
        #expect(Projection(stale) == before)
        #expect(try encoder.encode(stale.pendingCanonicalAuthoringMutations) == exactJournal)
        let resumed = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(try encoder.encode(resumed.pendingCanonicalAuthoringMutations) == exactJournal)
        #expect(Projection(resumed) == before)
        #expect(resumed.beginCanonicalSync())
        try resumed.applyCanonicalAuthoringResponse(mutation.id, item: response)
        resumed.endCanonicalSync()
        #expect(resumed.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(Projection(resumed) == before)
        let final = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(final.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(Projection(final) == before)
    }

    @Test("equal-revision trash replay cannot extend a clamped retention deadline")
    func replayPreservesFirstObservedTrashRetention() throws {
        let context = try Context()
        defer { context.remove() }
        let remoteDeletion = Self.now.addingTimeInterval(90 * 86_400)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        var object = try #require(JSONSerialization.jsonObject(with:
            encoder.encode(item(revision: 2, deleted: true))) as? [String: Any])
        object["deleted_at"] = ISO8601DateFormatter().string(from: remoteDeletion)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let response = try decoder.decode(DayWeaveCanonicalItem.self,
            from: JSONSerialization.data(withJSONObject: object))
        let mutation = try submitted(.trash, response: response)
        let seed = try context.store(trash: [.init(item: response)],
            tombstones: [Self.itemID: 2], mutation: mutation)
        #expect(seed.canonicalTrashEntry(id: Self.itemID)?.deletedAt == Self.now)
        let replayDate = Self.now.addingTimeInterval(10 * 86_400)
        let replay = PlannerStore(persistence: context.persistence, now: { replayDate })
        #expect(replay.beginCanonicalSync())
        try replay.applyCanonicalAuthoringResponse(mutation.id, item: response)
        replay.endCanonicalSync()
        #expect(replay.canonicalTrashEntry(id: Self.itemID)?.deletedAt == Self.now)
        #expect(replay.pendingCanonicalAuthoringMutations.isEmpty)
        let expiredDate = Self.now.addingTimeInterval(PlannerStore.canonicalTrashRetentionInterval + 1)
        let expired = PlannerStore(persistence: context.persistence, now: { expiredDate })
        #expect(expired.loadState == .ready)
        #expect(expired.canonicalTrashEntry(id: Self.itemID) == nil)
        #expect(expired.canonicalTombstoneRevisions[Self.itemID] == 2)
    }

    private static let itemID = UUID(uuidString: "aa770000-0000-4000-8000-000000000001")!
    private static let otherID = UUID(uuidString: "aa770000-0000-4000-8000-000000000002")!
    nonisolated private static let now = Date(timeIntervalSince1970: 1_800_100_000)
    private static let configuration =
        "https://api.example.test/|auth=static-v1:\(String(repeating: "c", count: 64))"

    private func submitted(_ operation: CanonicalAuthoringOperation,
        response: DayWeaveCanonicalItem
    ) throws -> DayWeavePendingCanonicalAuthoringMutation {
        let base = operation == .create ? nil : try item(revision: response.revision - 1,
            deleted: operation == .restore)
        return DayWeavePendingCanonicalAuthoringMutation(itemID: Self.itemID, operation: operation,
            draft: operation == .create || operation == .replace ? .init(item: response) : nil,
            expectedRevision: base?.revision, baseItem: base, createdAt: Self.now,
            configurationIdentifier: Self.configuration, hasBeenSubmitted: true)
    }

    private func item(id: UUID = Self.itemID, revision: UInt64, status: String = "inbox",
        title: String = "Synthetic reviewed item", sensitive: Bool = false, deleted: Bool = false
    ) throws -> DayWeaveCanonicalItem {
        let deletedAt = deleted ? #""2027-01-15T12:00:00Z""# : "null"
        let completedAt = status == "completed" ? #""2027-01-15T12:00:00Z""# : "null"
        let data = Data(#"""
        {"id":"\#(id.uuidString.lowercased())","is_sensitive":\#(sensitive),
        "kind":"task","status":"\#(status)","title":"\#(title)","notes":null,
        "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,
        "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
        "parent_id":null,"sibling_order":0,"is_executable":true,"revision":\#(revision),
        "created_at":"2027-01-15T10:00:00Z","updated_at":"2027-01-15T12:00:00Z",
        "completed_at":\#(completedAt),"deleted_at":\#(deletedAt)}
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private struct Projection: Equatable {
        let items: [DayWeaveCanonicalItem]
        let trash: [DayWeaveCanonicalTrashEntry]
        let tombstones: [UUID: UInt64]
        let anchor: DayWeaveOnboardingFirstItemAnchor?
        let selection: UUID?
        let cursor: String?
        @MainActor init(_ store: PlannerStore) {
            items = store.canonicalItems
            trash = store.canonicalTrash
            tombstones = store.canonicalTombstoneRevisions
            anchor = store.onboardingFirstItemAnchor
            selection = store.selectedCanonicalItemID
            cursor = store.canonicalDeltaCursor
        }
    }

    private struct Context {
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        init() throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent(
                "DayWeaveHistoricalReceiptTests-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            persistence = EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: try PlannerEncryptionKey(data: Data(repeating: 57, count: 32)))
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
        @MainActor func store(items: [DayWeaveCanonicalItem] = [], trash: [DayWeaveCanonicalTrashEntry] = [],
            tombstones: [UUID: UInt64] = [:], mutation: DayWeavePendingCanonicalAuthoringMutation,
            anchor: DayWeaveOnboardingFirstItemAnchor? = nil
        ) throws -> PlannerStore {
            let other = try CanonicalAuthoringHistoricalReceiptTests().item(id: SelfOuter.otherID, revision: 1)
            let store = PlannerStore(canonicalItems: items + [other], canonicalDeltaCursor: "synthetic-terminal-cursor",
                canonicalTombstoneRevisions: tombstones, canonicalConfigurationIdentifier: SelfOuter.configuration,
                onboardingFirstItemAnchor: anchor, pendingCanonicalAuthoringMutations: [mutation],
                canonicalTrash: trash, selectedCanonicalItemID: SelfOuter.otherID,
                persistence: persistence, restoreFromPersistence: false, now: { SelfOuter.now })
            #expect(store.loadState == .ready)
            #expect(store.pendingCanonicalAuthoringMutations == [mutation])
            store.flushPersistence()
            #expect(store.persistenceError == nil)
            return store
        }
        private typealias SelfOuter = CanonicalAuthoringHistoricalReceiptTests
    }
}
#endif
