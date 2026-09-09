import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Independent progress durable store", .serialized)
@MainActor
struct ItemProgressStoreTests {
    @Test("GET establishes zero and a reviewed update survives an uncertain response and restart")
    func durableReplayAfterRestart() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(!store.canReview(Self.itemID))
        #expect(await store.refresh(Self.itemID))
        #expect(store.canReview(Self.itemID))
        let canonicalBefore = context.planner.canonicalItems
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        let staged = try #require(context.planner.itemProgressState.journals.first)
        #expect(!staged.hasBeenSubmitted)
        #expect(await store.replayPending() == false)
        let submitted = try #require(context.persistence.load()?.itemProgressState?.journals.first)
        #expect(submitted.hasBeenSubmitted)
        #expect(submitted.requestBody == staged.requestBody)
        #expect(context.planner.canonicalItems == canonicalBefore)
        #expect(!store.canReview(Self.itemID))
        store.suspendForPrivacyBoundary()

        let restored = PlannerStore(persistence: context.persistence)
        #expect(restored.itemProgressState.journals == [submitted])
        let resumed = Self.store(planner: restored, transport: transport)
        defer { resumed.suspendForPrivacyBoundary() }
        await transport.setFailure(nil)
        #expect(await resumed.replayPending())
        #expect(restored.itemProgressState.journals.isEmpty)
        #expect(await transport.bodies() == [submitted.requestBody, submitted.requestBody])
        #expect(restored.itemProgressState.observations.first?.isReadProof == false)
        #expect(restored.canonicalItems == canonicalBefore)
        #expect(try context.persistence.load()?.itemProgressState == restored.itemProgressState)
    }

    @Test("definitive rejection retains reviewed values until explicit discard")
    func explicitConflictReview() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        await transport.setFailure(.definitive("item_progress_revision_stale"))
        #expect(await store.replayPending())
        let rejected = try #require(store.journal(for: Self.itemID))
        #expect(rejected.noEffectCode == "item_progress_revision_stale")
        #expect(rejected.command.components == Self.components)
        try store.discardReviewedIntent(Self.itemID, expectedOperationID: rejected.id)
        #expect(context.planner.itemProgressState.journals.isEmpty)
        #expect(try context.persistence.load()?.itemProgressState?.journals.isEmpty == true)
    }

    @Test("uncertain submitted custody blocks reset and discard")
    func pendingCustodyCannotBeForgotten() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        #expect(await store.replayPending() == false)
        let pending = context.planner.itemProgressState
        let pendingID = try #require(pending.journals.first?.id)
        #expect(throws: ItemProgressError.self) { try store.discardReviewedIntent(Self.itemID, expectedOperationID: pendingID) }
        context.planner.resetCanonicalSyncState()
        #expect(context.planner.itemProgressState == pending)
        #expect(context.planner.canonicalConfigurationIdentifier == Self.configuration)
        #expect(context.planner.hasExecutionCredentialReplacementBlocker)
    }

    @Test("privacy suspension conceals cache and queued content without deleting either")
    func privacyBoundaryRetainsCustody() async throws {
        let context = try Context(sensitive: true)
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        #expect(store.journal(for: Self.itemID)?.wasSensitive == true)
        let retained = context.planner.itemProgressState
        store.suspendForPrivacyBoundary()
        #expect(store.observation(for: Self.itemID) == nil)
        #expect(store.journal(for: Self.itemID) == nil)
        #expect(!store.canReview(Self.itemID))
        #expect(await store.replayPending() == false)
        #expect(context.planner.itemProgressState == retained)
        #expect(await transport.bodies().isEmpty)
    }

    @Test("a mismatched canonical GET cannot become edit authority")
    func staleCanonicalBaselineIsNotAdmitted() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline(itemRevision: 8))
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID) == false)
        #expect(store.observation(for: Self.itemID) == nil)
        #expect(!store.canReview(Self.itemID))
        #expect(await transport.readCount() == 2)
        #expect(context.planner.canonicalItems.first?.revision == 7)
    }

    @Test("queued privacy survives a pending mark, its cancellation, and restart without changing bytes")
    func stickyPendingSensitivity() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        let original = try #require(context.planner.itemProgressState.journals.first)
        #expect(!original.wasSensitive)
        #expect(context.planner.setCanonicalItemSensitivity(Self.itemID, isSensitive: true))
        #expect(context.planner.itemProgressState.journals.first?.wasSensitive == true)
        #expect(context.planner.setCanonicalItemSensitivity(Self.itemID, isSensitive: false))
        #expect(context.planner.canonicalSensitivityPresentationIndex()[Self.itemID] == .standard)
        #expect(store.isSensitive(Self.itemID))
        let restored = PlannerStore(persistence: context.persistence)
        let hardened = try #require(restored.itemProgressState.journals.first)
        #expect(hardened.wasSensitive && hardened.retainsCustody(of: original))
        #expect(hardened.requestBody == original.requestBody)
        #expect(!original.retainsCustody(of: hardened))
    }

    @Test("sensitive ancestry hardens retained progress before a later reparent downgrade")
    func stickyInheritedSensitivity() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        let original = try #require(context.planner.itemProgressState.journals.first)
        let item = try #require(context.planner.canonicalItems.first)
        let parentID = UUID()
        let parent = try Self.revised(item, id: parentID, revision: 1, sensitive: true)
        let child = try Self.revised(item, revision: 8, sensitive: false, parentID: parentID)
        context.planner.applyCanonicalDelta([.upsert(parent), .upsert(child)], nextCursor: "synthetic-private-ancestry")
        context.planner.flushPersistence()
        #expect(try context.persistence.load()?.itemProgressState?.journals.first?.wasSensitive == true)
        context.planner.applyCanonicalDelta([.upsert(try Self.revised(item, revision: 9, sensitive: false))],
            nextCursor: "synthetic-public-ancestry")
        context.planner.flushPersistence()
        #expect(context.planner.canonicalSensitivityPresentationIndex()[Self.itemID] == .standard)
        #expect(store.isSensitive(Self.itemID))
        #expect(context.planner.itemProgressState.journals.first?.retainsCustody(of: original) == true)
    }

    @Test("in-flight success and rejection accept only exact custody plus monotonic privacy")
    func inflightPrivacyHardening() async throws {
        for rejected in [false, true] {
            let context = try Context()
            defer { context.remove() }
            let transport = ProgressTestTransport(snapshot: Self.baseline())
            let store = context.store(transport)
            defer { store.suspendForPrivacyBoundary() }
            #expect(await store.refresh(Self.itemID))
            try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
            let original = try #require(context.planner.itemProgressState.journals.first)
            let planner = context.planner
            let item = try #require(planner.canonicalItems.first)
            let protected = try Self.revised(item, revision: 8, sensitive: true)
            let downgraded = try Self.revised(item, revision: 9, sensitive: false)
            await transport.setBeforeReply {
                planner.applyCanonicalDelta([.upsert(protected)], nextCursor: "synthetic-inflight-private")
                planner.flushPersistence()
                planner.applyCanonicalDelta([.upsert(downgraded)], nextCursor: "synthetic-inflight-public")
                planner.flushPersistence()
            }
            await transport.setFailure(rejected ? .definitive("item_progress_item_stale") : nil)
            #expect(await store.replayPending())
            #expect(await transport.bodies() == [original.requestBody])
            if rejected {
                let retained = try #require(planner.itemProgressState.journals.first)
                #expect(retained.wasSensitive && retained.hasBeenSubmitted)
                #expect(retained.noEffectCode == "item_progress_item_stale")
                #expect(retained.requestBody == original.requestBody)
            } else {
                #expect(planner.itemProgressState.journals.isEmpty)
                #expect(planner.itemProgressState.observations.first?.isReadProof == false)
            }
            #expect(try context.persistence.load()?.itemProgressState == planner.itemProgressState)
        }
    }

    @Test("missing items have content-free recovery and retain uncertain bytes until definitive rejection")
    func missingItemRecovery() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        #expect(await store.replayPending() == false)
        let original = try #require(context.planner.itemProgressState.journals.first)
        context.planner.replaceCanonicalState(changes: [], nextCursor: "synthetic-missing-item")
        context.planner.flushPersistence()
        #expect(store.journal(for: Self.itemID) == nil && store.observation(for: Self.itemID) == nil)
        #expect(store.recoveryEntries == [.init(id: original.id, itemID: Self.itemID,
            isRejected: false, canDiscard: false)])
        #expect(throws: ItemProgressError.self) { try store.discardReviewedIntent(Self.itemID, expectedOperationID: original.id) }
        await transport.setFailure(.definitive("item_progress_item_missing"))
        #expect(await store.replayPending())
        #expect(await transport.bodies() == [original.requestBody, original.requestBody])
        #expect(store.recoveryEntries.first?.canDiscard == true)
        try store.discardReviewedIntent(Self.itemID, expectedOperationID: original.id)
        #expect(store.recoveryEntries.isEmpty)
        #expect(try context.persistence.load()?.itemProgressState?.journals.isEmpty == true)
    }

    @Test("failed encrypted save rolls back optimistic progress before any transmission")
    func failedSaveRetainsDurableProgress() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        let durable = context.planner.itemProgressState
        // A synthetic second local writer advances the encrypted file CAS.
        let competitor = PlannerStore(persistence: context.persistence)
        competitor.flushPersistence()
        #expect(throws: PlannerPersistenceError.self) {
            try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        }
        #expect(context.planner.persistenceError != nil)
        #expect(context.planner.itemProgressState == durable)
        #expect(try context.persistence.load()?.itemProgressState == durable)
        #expect(await store.replayPending())
        #expect(await transport.bodies().isEmpty)
    }

    @Test("a stale discard action cannot delete newly re-reviewed progress for the same item")
    func discardBindsExactReviewedOperation() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = ProgressTestTransport(snapshot: Self.baseline())
        let store = context.store(transport)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: Self.components)
        let originalID = try #require(store.recoveryEntries.first?.id)
        try store.queue(itemID: Self.itemID, baseline: Self.baseline(), components: [])
        let replacement = context.planner.itemProgressState
        let replacementID = try #require(store.recoveryEntries.first?.id)
        #expect(originalID != replacementID)
        #expect(throws: ItemProgressError.staleReview) {
            try store.discardReviewedIntent(Self.itemID, expectedOperationID: originalID)
        }
        #expect(context.planner.itemProgressState == replacement)
        #expect(try context.persistence.load()?.itemProgressState == replacement)
        try store.discardReviewedIntent(Self.itemID, expectedOperationID: replacementID)
        #expect(store.recoveryEntries.isEmpty)
    }

    private static func revised(_ item: DayWeaveCanonicalItem, id: UUID? = nil,
                                revision: UInt64, sensitive: Bool, parentID: UUID? = nil) throws -> DayWeaveCanonicalItem {
        let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .iso8601
        var raw = try #require(JSONSerialization.jsonObject(with: encoder.encode(item)) as? [String: Any])
        raw["id"] = (id ?? item.id).uuidString
        raw["revision"] = revision
        raw["is_sensitive"] = sensitive
        raw["parent_id"] = parentID.map { $0.uuidString as Any } ?? NSNull()
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: JSONSerialization.data(withJSONObject: raw))
    }

    private static let itemID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    private static let configuration = "https://api.example.test/|auth=static-v1:" + String(repeating: "a", count: 64)
    private static let components = [ItemProgressComponent(
        id: UUID(uuidString: "22222222-2222-4222-8222-222222222222")!,
        name: "Synthetic progress", value: .percentage(basisPoints: 4_250))]

    private static func baseline(itemRevision: UInt64 = 7) -> ItemProgressSnapshot {
        .init(itemID: itemID, itemRevision: itemRevision, revision: 0, components: [], updatedAt: nil)
    }

    private static func store(planner: PlannerStore, transport: ProgressTestTransport) -> ItemProgressStore {
        let store = ItemProgressStore(planner: planner, connection: { transport },
            now: { Date(timeIntervalSince1970: 1_788_854_400) },
            sleep: { _ in throw CancellationError() })
        store.activate()
        return store
    }

    @MainActor
    private struct Context {
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        init(sensitive: Bool = false) throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveProgressTests-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            persistence = EncryptedPlannerPersistence(fileURL: directory.appendingPathComponent("synthetic.encrypted"),
                key: try PlannerEncryptionKey(data: Data(repeating: 43, count: 32)))
            let json = #"""
            {"id":"11111111-1111-4111-8111-111111111111","is_sensitive":\#(sensitive),
            "kind":"task","status":"planned","title":"Synthetic progress item","notes":null,
            "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,"earliest_start_at":null,
            "recurrence":null,"flexible_constraints":{},"split_policy":{"type":"indivisible"},
            "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,"is_executable":true,
            "revision":7,"created_at":"2026-09-08T10:00:00Z","updated_at":"2026-09-08T10:00:00Z",
            "completed_at":null,"deleted_at":null}
            """#
            let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
            let item = try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
            planner = PlannerStore(canonicalItems: [item], canonicalDeltaCursor: "synthetic-progress-cursor",
                canonicalConfigurationIdentifier: configuration, persistence: persistence, restoreFromPersistence: false)
        }
        func store(_ transport: ProgressTestTransport) -> ItemProgressStore {
            ItemProgressStoreTests.store(planner: planner, transport: transport)
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}

private actor ProgressTestTransport: ItemProgressTransport {
    nonisolated let configurationIdentifier = "https://api.example.test/|auth=static-v1:" + String(repeating: "a", count: 64)
    private var snapshot: ItemProgressSnapshot
    private var failure: ItemProgressError? = .unavailable
    private var sent: [Data] = []
    private var reads = 0
    private var beforeReply: (@MainActor @Sendable () -> Void)?
    init(snapshot: ItemProgressSnapshot) { self.snapshot = snapshot }
    func itemProgress(_ itemID: UUID) async throws -> ItemProgressSnapshot { reads += 1; return snapshot }
    func setFailure(_ value: ItemProgressError?) { failure = value }
    func bodies() -> [Data] { sent }
    func readCount() -> Int { reads }
    func setBeforeReply(_ value: @escaping @MainActor @Sendable () -> Void) { beforeReply = value }
    func putItemProgress(_ itemID: UUID, requestBody: Data) async throws -> ItemProgressReceipt {
        sent.append(requestBody)
        if let beforeReply { await beforeReply() }
        if let failure { throw failure }
        let command = try JSONDecoder().decode(ItemProgressCommand.self, from: requestBody)
        let progress = ItemProgressSnapshot(itemID: itemID, itemRevision: command.expectedItemRevision,
            revision: command.expectedProgressRevision + 1, components: command.components,
            updatedAt: "2026-09-08T10:01:00Z")
        let object: [String: Any] = ["operation_id": command.operationID.uuidString.lowercased(), "replayed": true,
            "progress": try JSONSerialization.jsonObject(with: JSONEncoder().encode(progress))]
        return try JSONDecoder().decode(ItemProgressReceipt.self, from: JSONSerialization.data(withJSONObject: object))
    }
}
#endif
