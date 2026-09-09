import AppKit
import Foundation
import SwiftUI
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Completion durable review and recovery", .serialized)
@MainActor
struct ItemCompletionStoreTests {
    private static let itemID = ItemCompletionTestFixtures.itemID
    private static let binding = "https://api.example.test/|auth=static-v1:" + String(repeating: "a", count: 64)
    private static let hash = ItemCompletionTestFixtures.hash
    private static let otherHash = ItemCompletionTestFixtures.otherHash

    @Test("synthetic completion controls render with explicit review and no live services")
    func syntheticReviewRender() async throws {
        let context = try Context(); defer { context.remove() }
        context.planner.applyCanonicalDelta([.upsert(try Self.item(id: UUID(), revision: 1,
            parentID: Self.itemID))], nextCursor: "synthetic-render-tree")
        let snapshot = ItemCompletionSnapshot(itemID: Self.itemID, itemRevision: 7,
            state: .empty(itemID: Self.itemID), evidenceHash: Self.hash,
            counts: .init(requiredDescendants: 1, incomplete: 1))
        let client = CompletionStoreTransport(snapshot: snapshot)
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        let owner = UUID()
        store.showDetail(Self.itemID, owner: owner)
        let lease = try #require(store.reviewLease(itemID: Self.itemID, owner: owner))
        let surface = ItemCompletionReviewView(context: .init(baseline: snapshot, lease: lease, owner: owner,
            requiredForParent: true, mode: .automatic, sensitive: false))
            .environmentObject(context.planner).environmentObject(store)
            .background(Color(nsColor: .windowBackgroundColor)).environment(\.colorScheme, .light)
        let host = NSHostingView(rootView: surface)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 570, height: 600),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.appearance = NSAppearance(named: .aqua)
        window.contentView = host
        defer { window.close() }
        host.frame = NSRect(x: 0, y: 0, width: 570, height: 600)
        host.layoutSubtreeIfNeeded()
        if ProcessInfo.processInfo.environment["DAYWEAVE_ITEM_COMPLETION_RENDER_DIRECTORY"] != nil {
            window.orderFrontRegardless()
            try await Task.sleep(for: .milliseconds(200))
            host.displayIfNeeded()
        }
        let bitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        #expect(bitmap.pixelsWide >= 570 && bitmap.pixelsHigh >= 600)
        if let path = ProcessInfo.processInfo.environment["DAYWEAVE_ITEM_COMPLETION_RENDER_DIRECTORY"] {
            let png = try #require(bitmap.representation(using: .png, properties: [:]))
            try png.write(to: URL(fileURLWithPath: path).appendingPathComponent("macos-completion-synthetic.png"))
        }
        #expect(await client.bodies().isEmpty)
    }

    @Test("never-submitted intent survives offline restart without fabricated rejection")
    func offlineFirstSendRestart() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let first = Self.store(context.planner, client)
        #expect(await first.refresh(Self.itemID))
        try Self.queue(first)
        let original = try #require(context.planner.itemCompletionState.journals.first)
        first.suspendForPrivacyBoundary()

        let restored = PlannerStore(persistence: context.persistence)
        let resumed = Self.store(restored, client)
        defer { resumed.suspendForPrivacyBoundary() }
        #expect(!resumed.canReview(Self.itemID))
        await client.setReadFailure(.unavailable)
        #expect(await resumed.replayPending() == false)
        #expect(restored.itemCompletionState.journals == [original])
        #expect(await client.bodies().isEmpty)
        await client.setReadFailure(nil)
        await client.setFailure(nil)
        #expect(await resumed.replayPending() == false, "confirmed receipt still requires terminal canonical catch-up")
        #expect(restored.itemCompletionState.journals.isEmpty)
        #expect(restored.itemCompletionState.needsCanonicalCatchUp)
        #expect(await client.bodies() == [original.requestBody])
        #expect(restored.canonicalItems.first?.revision == 7)
        #expect(!restored.canMutatePlan)
        #expect(try context.persistence.load()?.itemCompletionState == restored.itemCompletionState)
    }

    @Test("ambiguous submitted requests replay unchanged after newer canonical state")
    func historicalReplayNeverRollsBackCanonical() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let first = Self.store(context.planner, client)
        #expect(await first.refresh(Self.itemID))
        try Self.queue(first)
        #expect(await first.replayPending() == false)
        let submitted = try #require(context.planner.itemCompletionState.journals.first)
        #expect(submitted.hasBeenSubmitted && submitted.noEffectCode == nil)
        let newer = try Self.item(revision: 50)
        context.planner.applyCanonicalDelta([.upsert(newer)], nextCursor: "synthetic-newer-head")
        context.planner.flushPersistence()
        first.suspendForPrivacyBoundary()

        let restored = PlannerStore(persistence: context.persistence)
        let resumed = Self.store(restored, client, catchUp: { true })
        defer { resumed.suspendForPrivacyBoundary() }
        await client.setReadFailure(.unavailable)
        await client.setFailure(nil)
        let reads = await client.readCount()
        #expect(await resumed.replayPending())
        #expect(await client.readCount() == reads, "submitted replay never requires a fresh GET")
        #expect(await client.bodies() == [submitted.requestBody, submitted.requestBody])
        #expect(restored.canonicalItems == [newer])
        #expect(restored.itemCompletionState.journals.isEmpty)
        #expect(!restored.itemCompletionState.needsCanonicalCatchUp)
        #expect(restored.itemCompletionState.observations.first?.isReadProof == false)
        #expect(!resumed.canReview(Self.itemID))
    }

    @Test("known full-forest drift rejects a frozen first send without silently rebasing")
    func firstSendDetectsEvidenceDriftAfterRestart() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let first = Self.store(context.planner, client)
        #expect(await first.refresh(Self.itemID))
        try Self.queue(first)
        let original = try #require(context.planner.itemCompletionState.journals.first)
        first.suspendForPrivacyBoundary()
        await client.setSnapshot(Self.baseline(hash: Self.otherHash))
        let resumed = Self.store(PlannerStore(persistence: context.persistence), client)
        defer { resumed.suspendForPrivacyBoundary() }
        #expect(await resumed.replayPending())
        let rejected = try #require(resumed.journal(for: Self.itemID))
        #expect(!rejected.hasBeenSubmitted && rejected.noEffectCode == "item_completion_evidence_stale")
        #expect(rejected.command == original.command && rejected.requestBody == original.requestBody)
        #expect(await client.bodies().isEmpty)
    }

    @Test("unrelated canonical and execution changes invalidate an open review lease")
    func globalAuthorityInvalidatesReview() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        let owner = UUID()
        store.showDetail(Self.itemID, owner: owner)
        let lease = try #require(store.reviewLease(itemID: Self.itemID, owner: owner))
        let other = try Self.item(id: UUID(), revision: 1)
        context.planner.applyCanonicalDelta([.upsert(other)], nextCursor: "synthetic-unrelated-change")
        #expect(!store.canReview(Self.itemID))
        #expect(!store.reviewIsCurrent(lease, baseline: Self.baseline()))
        #expect(throws: ItemCompletionError.staleReview) {
            try store.queueReviewed(lease: lease, baseline: Self.baseline(), requiredForParent: false, mode: .automatic)
        }
        store.hideDetail(owner: owner)
        #expect(await store.refresh(Self.itemID))
        var execution = context.planner.executionState
        execution.bindingIdentifier = Self.binding
        execution.deviceID = UUID()
        execution.revision = 1
        try context.planner.persistExecutionState(execution)
        #expect(!store.canReview(Self.itemID))
        #expect(context.planner.itemCompletionState.journals.isEmpty)
    }

    @Test("in-flight GET cannot grant authority after canonical evidence changes")
    func changedDuringRead() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        await client.holdNextRead()
        let read = Task { await store.refresh(Self.itemID) }
        try #require(await eventuallyCompletion { await client.isReadHeld() })
        context.planner.applyCanonicalDelta([.upsert(try Self.item(id: UUID(), revision: 1))],
            nextCursor: "synthetic-during-read")
        await client.releaseRead()
        #expect(await read.value == false)
        #expect(!store.canReview(Self.itemID))
        #expect(context.planner.itemCompletionReadAdmissions.isEmpty)
    }

    @Test("unchanged polling checkpoints do not repeatedly invalidate an open review")
    func unchangedPollingRetainsAuthority() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        let generation = context.planner.itemCompletionEvidenceGeneration
        context.planner.applyCanonicalDelta([], nextCursor: "synthetic-completion-head")
        try context.planner.persistExecutionState(context.planner.executionState)
        #expect(context.planner.itemCompletionEvidenceGeneration == generation)
        #expect(store.canReview(Self.itemID))
    }

    @Test("known missing or newer GET evidence invalidates other task reviews too")
    func readDriftInvalidatesAllAdmissions() async throws {
        for missing in [true, false] {
            let context = try Context(); defer { context.remove() }
            let otherID = UUID()
            context.planner.applyCanonicalDelta([.upsert(try Self.item(id: otherID, revision: 7))],
                nextCursor: "synthetic-two-items")
            let client = CompletionStoreTransport(snapshot: Self.baseline())
            let store = Self.store(context.planner, client)
            defer { store.suspendForPrivacyBoundary() }
            #expect(await store.refresh(Self.itemID))
            await client.setSnapshot(.init(itemID: otherID, itemRevision: 7,
                state: .empty(itemID: otherID), evidenceHash: Self.hash))
            #expect(await store.refresh(otherID))
            #expect(store.canReview(Self.itemID) && store.canReview(otherID))
            if missing {
                await client.setReadFailure(.definitive("item_completion_item_missing"))
            } else {
                await client.setSnapshot(.init(itemID: Self.itemID, itemRevision: 8,
                    state: .empty(itemID: Self.itemID), evidenceHash: Self.otherHash))
            }
            #expect(await store.refresh(Self.itemID) == false)
            #expect(!store.canReview(otherID), "failed catch-up cannot revive the earlier global proof")
            #expect(context.planner.itemCompletionReadAdmissions.isEmpty)
        }
    }

    @Test("staging and cancelling a Defer intent never revives an old review lease")
    func deferIntentInvalidatesReviewGeneration() async throws {
        let context = try Context(); defer { context.remove() }
        let blockID = UUID()
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
        let object: [String: Any] = [
            "id": UUID().uuidString, "item_id": Self.itemID.uuidString, "item_revision": 7,
            "occurrence_id": NSNull(), "session_index": 0, "planned_block_id": blockID.uuidString,
            "source_device_id": UUID().uuidString, "status": "paused", "revision": 2,
            "accumulated_seconds": 300, "actual_seconds": NSNull(),
            "started_at": "2026-09-09T10:00:00Z", "running_since": NSNull(),
            "paused_at": "2026-09-09T10:05:00Z", "pause_until": NSNull(), "pause_reason": NSNull(),
            "ended_at": NSNull(), "created_at": "2026-09-09T10:00:00Z", "updated_at": "2026-09-09T10:05:00Z"
        ]
        let session = try decoder.decode(DayWeaveExecutionSession.self,
            from: JSONSerialization.data(withJSONObject: object))
        var execution = context.planner.executionState
        execution.bindingIdentifier = Self.binding
        execution.deviceID = session.sourceDeviceID
        execution.revision = 2
        execution.activeSession = session
        execution.historyWindow = [session]
        execution.historyWindowRevision = 2
        try context.planner.persistExecutionState(execution)
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        let owner = UUID()
        store.showDetail(Self.itemID, owner: owner)
        let lease = try #require(store.reviewLease(itemID: Self.itemID, owner: owner))
        let moveStart = session.startedAt.addingTimeInterval(3_600)
        let intent = DayWeavePendingExecutionDeferIntent(identity: .init(session: session),
            focusedBlockID: blockID, sourceStart: session.startedAt,
            sourceEnd: session.startedAt.addingTimeInterval(1_800), moveStart: moveStart,
            approvedMoveEnd: moveStart, approvedDeadlines: [], deadlineConflictApproved: false,
            approvedFixedConflicts: [], fixedConflictApproved: false, sourceOverrideApproved: false,
            createdAt: session.startedAt, expiresAt: moveStart)
        try context.planner.persistExecutionDeferIntent(intent)
        try context.planner.cancelExecutionDeferIntent(intent)
        #expect(context.planner.pendingExecutionDeferIntent == nil)
        #expect(!store.canReview(Self.itemID))
        #expect(!store.reviewIsCurrent(lease, baseline: Self.baseline()))
    }

    @Test("receipt settlement keeps a durable catch-up fence across restart")
    func durableCatchUpFence() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        await client.setFailure(nil)
        let first = Self.store(context.planner, client)
        #expect(await first.refresh(Self.itemID))
        try Self.queue(first)
        #expect(await first.replayPending() == false)
        first.suspendForPrivacyBoundary()
        let restored = PlannerStore(persistence: context.persistence)
        #expect(restored.itemCompletionState.needsCanonicalCatchUp && !restored.canMutatePlan)
        let current = try Self.item(revision: 8)
        let resumed = Self.store(restored, client, catchUp: {
            restored.applyCanonicalDelta([.upsert(current)], nextCursor: "synthetic-complete-head")
            restored.flushPersistence()
            return restored.persistenceError == nil
        })
        defer { resumed.suspendForPrivacyBoundary() }
        #expect(await resumed.replayPending())
        #expect(restored.canonicalItems == [current])
        #expect(!restored.itemCompletionState.needsCanonicalCatchUp && restored.canMutatePlan)
        #expect(try context.persistence.load()?.itemCompletionState?.needsCanonicalCatchUp == false)
        #expect(!resumed.canReview(Self.itemID), "terminal catch-up alone is not a GET review")
    }

    @Test("privacy suspension during submission preserves exact ambiguous custody")
    func privacyRevocationDuringReply() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        #expect(await store.refresh(Self.itemID))
        try Self.queue(store)
        await client.setFailure(nil)
        await client.setBeforeReply { store.suspendForPrivacyBoundary() }
        #expect(await store.replayPending() == false)
        #expect(context.planner.itemCompletionState.journals.first?.hasBeenSubmitted == true)
        #expect(store.observation(for: Self.itemID) == nil && store.journal(for: Self.itemID) == nil)
        #expect(store.recoveryEntries.isEmpty)
        #expect(!context.planner.isCanonicalSyncLocked && !store.isWorking)
        #expect(context.planner.hasExecutionCredentialReplacementBlocker)
    }

    @Test("authoritative missing GET does not settle or discard an existing command")
    func missingReadRetainsIntent() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try Self.queue(store)
        let retained = context.planner.itemCompletionState.journals
        await client.setReadFailure(.definitive("item_completion_item_missing"))
        #expect(await store.refresh(Self.itemID) == false)
        #expect(!store.canReview(Self.itemID))
        #expect(context.planner.itemCompletionState.journals == retained)
        #expect(await client.bodies().isEmpty)
    }

    @Test("private descendants protect parent policy and harden queued intent permanently")
    func descendantPrivacyIsSticky() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try Self.queue(store)
        let original = try #require(context.planner.itemCompletionState.journals.first)
        let childID = UUID()
        let parent = try Self.item(revision: 8, executable: false)
        let child = try Self.item(id: childID, revision: 1, parentID: Self.itemID, sensitive: true)
        context.planner.applyCanonicalDelta([.upsert(parent), .upsert(child)], nextCursor: "synthetic-private-child")
        context.planner.flushPersistence()
        #expect(context.planner.canonicalSensitivityPresentationIndex()[Self.itemID] == .standard)
        #expect(store.isSensitive(Self.itemID))
        #expect(context.planner.itemCompletionState.journals.first?.wasSensitive == true)
        let moved = try Self.item(id: childID, revision: 2, sensitive: false)
        context.planner.applyCanonicalDelta([.upsert(moved)], nextCursor: "synthetic-unlinked-child")
        context.planner.flushPersistence()
        let retained = try #require(PlannerStore(persistence: context.persistence).itemCompletionState.journals.first)
        #expect(retained.wasSensitive && retained.retainsCustody(of: original))
        #expect(retained.requestBody == original.requestBody)
    }

    @Test("definitive PUT conflicts remain visible and require operation-bound discard")
    func definitiveConflictAndDiscard() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        try Self.queue(store)
        await client.setFailure(.definitive("item_completion_revision_stale"))
        #expect(await store.replayPending())
        let journal = try #require(store.journal(for: Self.itemID))
        #expect(journal.noEffectCode == "item_completion_revision_stale")
        #expect(!store.canReview(Self.itemID), "a definitive rejection requires a fresh GET")
        #expect(context.planner.itemCompletionReadAdmissions.isEmpty)
        #expect(await store.refresh(Self.itemID))
        #expect(store.canReview(Self.itemID))
        #expect(store.journal(for: Self.itemID)?.requestBody == journal.requestBody)
        #expect(throws: ItemCompletionError.staleReview) {
            try store.discardReviewedIntent(Self.itemID, expectedOperationID: UUID())
        }
        try store.discardReviewedIntent(Self.itemID, expectedOperationID: journal.id)
        #expect(context.planner.itemCompletionState.journals.isEmpty)
        #expect(try context.persistence.load()?.itemCompletionState?.journals.isEmpty == true)
    }

    @Test("saved private aggregates stay protected after their descendant leaves the subtree")
    func staleAggregatePrivacy() async throws {
        let context = try Context(); defer { context.remove() }
        let childID = UUID()
        context.planner.applyCanonicalDelta([.upsert(try Self.item(id: childID, revision: 1,
            parentID: Self.itemID, sensitive: true))], nextCursor: "synthetic-private-subtree")
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        #expect(store.isSensitive(Self.itemID))
        context.planner.applyCanonicalDelta([.upsert(try Self.item(id: childID, revision: 2))],
            nextCursor: "synthetic-public-subtree")
        #expect(!context.planner.itemCompletionRequiresSensitivePresentation(Self.itemID))
        #expect(store.observation(for: Self.itemID) != nil)
        #expect(store.isSensitive(Self.itemID), "cached aggregate is not reclassified from a newer graph")
        #expect(await store.refresh(Self.itemID))
        #expect(store.isSensitive(Self.itemID), "V1 GET cannot prove that the local public subtree matches server privacy")
    }

    @Test("fresh completion counts and intent stay protected even with a locally public forest")
    func remoteDescendantPrivacyCannotBeInferredFromParentRevision() async throws {
        let context = try Context(); defer { context.remove() }
        let client = CompletionStoreTransport(snapshot: Self.baseline())
        let store = Self.store(context.planner, client)
        defer { store.suspendForPrivacyBoundary() }
        #expect(await store.refresh(Self.itemID))
        #expect(!context.planner.itemCompletionRequiresSensitivePresentation(Self.itemID))
        #expect(store.canReview(Self.itemID) && store.isSensitive(Self.itemID))
        try Self.queue(store)
        #expect(context.planner.itemCompletionState.journals.first?.wasSensitive == true)
    }

    private static func baseline(hash: String = hash) -> ItemCompletionSnapshot {
        .init(itemID: itemID, itemRevision: 7, state: .empty(itemID: itemID), evidenceHash: hash)
    }
    private static func queue(_ store: ItemCompletionStore) throws {
        try store.queue(itemID: itemID, baseline: baseline(), requiredForParent: false, mode: .automatic)
    }
    private static func store(_ planner: PlannerStore, _ client: CompletionStoreTransport,
                              catchUp: @escaping @MainActor () async -> Bool = { false }) -> ItemCompletionStore {
        let store = ItemCompletionStore(planner: planner, connection: { client }, catchUp: catchUp,
            now: { ItemCompletionTestFixtures.date }, sleep: { _ in throw CancellationError() }, automaticOutbox: false)
        store.activate()
        return store
    }
    private static func item(id: UUID = itemID, revision: UInt64, parentID: UUID? = nil,
                             sensitive: Bool = false, executable: Bool = true) throws -> DayWeaveCanonicalItem {
        let object: [String: Any] = [
            "id": id.uuidString, "kind": "task", "status": "planned", "is_sensitive": sensitive,
            "title": "Synthetic completion item", "notes": NSNull(), "timezone_name": "UTC",
            "duration_seconds": 1800, "deadline_at": NSNull(), "earliest_start_at": NSNull(),
            "recurrence": NSNull(), "flexible_constraints": [:], "split_policy": ["type": "indivisible"],
            "importance": 50, "urgency": 50, "parent_id": parentID.map { $0.uuidString as Any } ?? NSNull(),
            "sibling_order": 0, "is_executable": executable, "revision": revision,
            "created_at": "2026-09-09T10:00:00Z", "updated_at": "2026-09-09T10:01:00Z",
            "completed_at": NSNull(), "deleted_at": NSNull()
        ]
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: JSONSerialization.data(withJSONObject: object))
    }
    @MainActor
    private struct Context {
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        init() throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveCompletionStore-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            persistence = EncryptedPlannerPersistence(fileURL: directory.appendingPathComponent("synthetic.encrypted"),
                key: try PlannerEncryptionKey(data: Data(repeating: 61, count: 32)))
            planner = PlannerStore(canonicalItems: [try ItemCompletionStoreTests.item(revision: 7)],
                canonicalDeltaCursor: "synthetic-completion-head",
                canonicalConfigurationIdentifier: ItemCompletionStoreTests.binding,
                persistence: persistence, restoreFromPersistence: false)
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}

private actor CompletionStoreTransport: ItemCompletionTransport {
    nonisolated let configurationIdentifier = "https://api.example.test/|auth=static-v1:" + String(repeating: "a", count: 64)
    private var snapshot: ItemCompletionSnapshot
    private var failure: ItemCompletionError? = .unavailable
    private var readFailure: ItemCompletionError?
    private var sent: [Data] = []
    private var reads = 0
    private var holdRead = false
    private var readContinuation: CheckedContinuation<ItemCompletionSnapshot, Never>?
    private var beforeReply: (@MainActor @Sendable () -> Void)?
    init(snapshot: ItemCompletionSnapshot) { self.snapshot = snapshot }
    func itemCompletion(_ itemID: UUID) async throws -> ItemCompletionSnapshot {
        reads += 1
        if let readFailure { throw readFailure }
        if holdRead {
            holdRead = false
            return await withCheckedContinuation { readContinuation = $0 }
        }
        return snapshot
    }
    func holdNextRead() { holdRead = true }
    func isReadHeld() -> Bool { readContinuation != nil }
    func releaseRead() { readContinuation?.resume(returning: snapshot); readContinuation = nil }
    func readCount() -> Int { reads }
    func setSnapshot(_ value: ItemCompletionSnapshot) { snapshot = value }
    func setReadFailure(_ value: ItemCompletionError?) { readFailure = value }
    func setFailure(_ value: ItemCompletionError?) { failure = value }
    func bodies() -> [Data] { sent }
    func setBeforeReply(_ value: @escaping @MainActor @Sendable () -> Void) { beforeReply = value }
    func putItemCompletion(_ itemID: UUID, requestBody: Data) async throws -> ItemCompletionReceipt {
        sent.append(requestBody)
        if let beforeReply { await beforeReply() }
        if let failure { throw failure }
        let command = try ItemCompletionValidation.decode(ItemCompletionCommand.self, from: requestBody)
        let provenance: ItemCompletionProvenance? = command.mode == .complete
            ? .init(kind: .manual, reopen: .init(status: .planned)) : nil
        let policy = ItemCompletionPolicy(itemID: itemID, revision: command.expectedCompletionRevision + 1,
            requiredForParent: command.requiredForParent, mode: command.mode, provenance: provenance,
            updatedAt: "2026-09-09T10:02:00.123456Z")
        return .init(operationID: command.operationID, replayed: sent.count > 1,
            completion: .init(itemID: itemID, itemRevision: command.expectedItemRevision + 1, state: policy,
                evidenceHash: ItemCompletionTestFixtures.otherHash))
    }
}

@MainActor
private func eventuallyCompletion(_ condition: () async -> Bool) async -> Bool {
    for _ in 0..<500 {
        if await condition() { return true }
        await Task.yield()
    }
    return false
}
#endif
