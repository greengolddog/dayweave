import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical planner sync", .serialized)
@MainActor
struct CanonicalSyncStoreTests {
    @Test("sync publishes a local capture and renders a side-effect-free preview")
    func testSyncVerticalSlice() async throws {
        let token = "canonical-sync-test-token"
        let itemID = UUID(uuidString: "deaddead-2222-4333-8444-beefbeefbeef")!
        let previewBlockID = UUID(uuidString: "10000000-2222-4333-8444-200000000000")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let start = now.addingTimeInterval(3_600)
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"cursor-0","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 201,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 1))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.previewObject(itemID: itemID, blockID: previewBlockID).utf8)
            )
        )
        let local = ScheduleBlock(
            id: itemID,
            title: "Write launch plan",
            kind: .task,
            start: start,
            end: start.addingTimeInterval(2_700),
            status: .scheduled,
            project: nil,
            notes: "Private local notes",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            syncOrigin: .local
        )
        let planner = PlannerStore(blocks: [local], restoreFromPersistence: false)
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: "https://api.example.com/gateway"),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            now: { now }
        )

        await sync.sync()

        #expect(planner.canonicalItems.count == 1)
        #expect(planner.canonicalDeltaCursor == "cursor-0")
        #expect(planner.blocks.count == 1)
        #expect(planner.blocks[0].id == previewBlockID)
        #expect(planner.blocks[0].sourceItemID == itemID)
        #expect(planner.blocks[0].sourceItemRevision == 1)
        #expect(planner.blocks[0].placementReason == "Placed in the earliest matching opening.")
        #expect(sync.lastPreview?.inputDigest == "sha256:test")
        if case .online = sync.status {} else { Issue.record("Expected online sync status") }

        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.map(\.method) == ["GET", "POST", "POST"])
        #expect(requests.map(\.url.path) == [
            "/gateway/v1/items/delta",
            "/gateway/v1/items",
            "/gateway/v1/schedule/preview",
        ])
        #expect(requests[1].headers["Idempotency-Key"] == "mac-create-\(itemID.uuidString.lowercased())")
        let previewBody = try #require(requests[2].jsonBody)
        let previous = try #require(previewBody["previous_assignments"] as? [[String: Any]])
        // A locally guessed placement is not a server-authored stability hint.
        #expect(previous.isEmpty)

        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"cursor-0","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.previewObject(itemID: itemID, blockID: previewBlockID).utf8)
            )
        )
        await sync.sync()
        if case .online = sync.status {} else {
            Issue.record("An unchanged terminal delta cursor must remain a successful no-op")
        }
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == [
            "GET", "POST", "POST", "GET", "POST",
        ])
    }

    @Test("a create response with the wrong identity is rejected without rebinding local intent")
    func testCreateResponseIdentityIsValidated() async throws {
        let token = "canonical-create-identity-token"
        let localID = UUID(uuidString: "20000000-2222-4333-8444-200000000000")!
        let wrongID = UUID(uuidString: "20000000-2222-4333-8444-200000000001")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let local = ScheduleBlock(
            id: localID,
            title: "Keep this capture",
            kind: .task,
            start: now.addingTimeInterval(3_600),
            end: now.addingTimeInterval(5_400),
            status: .scheduled,
            project: nil,
            notes: "Local intent",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            syncOrigin: .local
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"create-check","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 201,
                body: Data("{\"item\":\(Self.itemObject(id: wrongID, revision: 1))}".utf8)
            )
        )
        let planner = PlannerStore(blocks: [local], restoreFromPersistence: false, now: { now })
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(planner.blocks.count == 1)
        #expect(planner.blocks[0].id == localID)
        #expect(planner.blocks[0].sourceItemID == nil)
        #expect(planner.canonicalItems.isEmpty)
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "POST"])
    }

    @Test("stale cursor recovery is multipage and tombstones prevent stale resurrection")
    func testStaleCursorMultipageTombstoneRecovery() async throws {
        let token = "canonical-422-token"
        let itemID = UUID(uuidString: "21000000-2222-4333-8444-200000000000")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 422, body: Data(#"{"error":{"code":"invalid_cursor","message":"expired"}}"#.utf8)),
            .init(statusCode: 200, body: Data("""
            {"changes":[{"type":"upsert","item":\(Self.itemObject(id: itemID, revision: 1))}],
             "next_cursor":"recovery-1","has_more":true}
            """.utf8)),
            .init(statusCode: 200, body: Data("""
            {"changes":[{"type":"tombstone","tombstone":{"id":"\(itemID.uuidString.lowercased())",
             "revision":2,"deleted_at":"2026-08-29T08:00:01Z","parent_id":null}}],
             "next_cursor":"recovery-2","has_more":false}
            """.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8))
        )
        let planner = PlannerStore(
            canonicalDeltaCursor: "expired-cursor",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            restoreFromPersistence: false,
            now: { now }
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(planner.canonicalItems.isEmpty)
        #expect(planner.canonicalDeltaCursor == "recovery-2")
        #expect(planner.canonicalTombstoneRevisions[itemID] == 2)
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "GET", "GET", "POST"])

        let stale = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        planner.applyCanonicalDelta([.upsert(stale)], nextCursor: "later")
        #expect(planner.canonicalItems.isEmpty)
        #expect(planner.canonicalTombstoneRevisions[itemID] == 2)
    }

    @Test("aggregate delta change budget fails closed")
    func testDeltaChangeBudget() async throws {
        let token = "canonical-delta-budget-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let tombstones = (0...CanonicalSyncStore.maximumDeltaChanges).map { index in
            let id = String(format: "00000000-0000-4000-8000-%012llx", UInt64(index))
            return #"{"type":"tombstone","tombstone":{"id":"\#(id)","revision":1,"deleted_at":"2026-08-29T08:00:00Z","parent_id":null}}"#
        }.joined(separator: ",")
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data("{\"changes\":[\(tombstones)],\"next_cursor\":\"too-large\",\"has_more\":false}".utf8)
            )
        )
        let planner = PlannerStore(restoreFromPersistence: false, now: { now })
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(planner.canonicalDeltaCursor == nil)
        #expect(URLProtocolStub.storage.requests(for: token).count == 1)
    }

    @Test("409 preserves the local mutation as a durable conflict")
    func testConflictRetainsMutation() async throws {
        let token = "canonical-conflict-token"
        let itemID = UUID(uuidString: "22000000-2222-4333-8444-200000000000")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let block = Self.block(
            itemID: itemID,
            revision: 1,
            start: now.addingTimeInterval(3_600),
            status: .paused
        )
        let planner = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            canonicalDeltaCursor: "before-conflict",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"same","has_more":false}"#.utf8)),
            .init(statusCode: 409, body: Data(#"{"error":{"code":"conflict","message":"revision changed"}}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 1]).utf8))
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        let mutation = try #require(planner.pendingCanonicalMutations.first)
        #expect(mutation.itemID == itemID)
        #expect(mutation.desiredStatus == .paused)
        #expect(mutation.disposition == .conflicted)
        #expect(mutation.diagnostic?.contains("stale") == true)
        #expect(planner.canRetryCanonicalMutation(mutation))
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "PUT", "POST"])

        planner.retryConflictedCanonicalMutation(mutation.id)
        let retried = try #require(planner.pendingCanonicalMutations.first)
        #expect(retried.id == mutation.id)
        #expect(retried.desiredStatus == .paused)
        #expect(retried.baseRevision == 1)
        #expect(retried.disposition == .pending)
        #expect(retried.diagnostic == nil)
        #expect(planner.lastScheduleMessage.contains("sync to retry"))
    }

    @Test("one split session never completes the whole canonical item")
    func testSplitSessionStatusIsRetainedWithoutFullReplacement() async throws {
        let token = "canonical-split-session-token"
        let itemID = UUID(uuidString: "22500000-2222-4333-8444-200000000000")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(Self.itemObject(
            id: itemID,
            revision: 1,
            splitPolicy: #"{"type":"splittable","minimum_chunk_seconds":900,"maximum_chunk_seconds":1800}"#
        ))
        let block = Self.block(
            itemID: itemID,
            revision: 1,
            start: now.addingTimeInterval(3_600),
            status: .completed
        )
        let planner = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            canonicalDeltaCursor: "split-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"split-after","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 1]).utf8))
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        let mutation = try #require(planner.pendingCanonicalMutations.first)
        #expect(mutation.itemID == itemID)
        #expect(mutation.desiredStatus == .completed)
        #expect(mutation.disposition == .conflicted)
        #expect(!planner.canRetryCanonicalMutation(mutation))
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "POST"])
    }

    @Test("recurrence context is per session and mixed freeze groups are not overpinned")
    func testRecurrenceContextAndPinning() async throws {
        let token = "canonical-recurrence-token"
        let itemID = UUID(uuidString: "23000000-2222-4333-8444-200000000000")!
        let completedOccurrence = UUID(uuidString: "23000000-2222-4333-8444-200000000001")!
        let skippedOccurrence = UUID(uuidString: "23000000-2222-4333-8444-200000000002")!
        let frozenOccurrence = UUID(uuidString: "23000000-2222-4333-8444-200000000003")!
        let mixedOccurrence = UUID(uuidString: "23000000-2222-4333-8444-200000000004")!
        let removedItemID = UUID(uuidString: "23000000-2222-4333-8444-200000000005")!
        let removedCompletedOccurrence = UUID(uuidString: "23000000-2222-4333-8444-200000000006")!
        let removedSkippedOccurrence = UUID(uuidString: "23000000-2222-4333-8444-200000000007")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        var completed = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(1_800))
        completed.occurrenceID = completedOccurrence
        var skipped = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(1_200))
        skipped.occurrenceID = skippedOccurrence
        var frozen = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(2_400))
        frozen.occurrenceID = frozenOccurrence
        var mixedEarly = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(1_200))
        mixedEarly.occurrenceID = mixedOccurrence
        var mixedLater = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(10_800))
        mixedLater.occurrenceID = mixedOccurrence
        mixedLater.sessionIndex = 1
        let planner = PlannerStore(
            blocks: [completed, skipped, frozen, mixedEarly, mixedLater],
            canonicalItems: [item],
            canonicalDeltaCursor: "recurrence-before",
            completedOccurrenceIDs: [removedCompletedOccurrence],
            recurrenceSessionOutcomes: [
                .init(
                    itemID: removedItemID,
                    occurrenceID: removedCompletedOccurrence,
                    sessionIndex: 0,
                    disposition: .completed,
                    occurredAt: now,
                    occurrenceFullyScheduled: true
                ),
                .init(
                    itemID: removedItemID,
                    occurrenceID: removedSkippedOccurrence,
                    sessionIndex: 0,
                    disposition: .skipped,
                    occurredAt: now,
                    occurrenceFullyScheduled: true
                ),
            ],
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            schedulePreviewProvenance: Self.provenance(now: now),
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        planner.complete(completed.id)
        planner.skip(skipped.id)
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"recurrence-after","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 1]).utf8))
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        #expect(planner.completedOccurrenceIDs == [completedOccurrence])
        let request = try #require(URLProtocolStub.storage.requests(for: token).last)
        let body = try #require(request.jsonBody)
        let context = try #require(body["recurrence_context"] as? [String: Any])
        let completedIDs = try #require(context["completed_occurrence_ids"] as? [String])
        #expect(completedIDs == [completedOccurrence.uuidString.lowercased()])
        let anchors = try #require(context["completion_anchors"] as? [String: String])
        #expect(anchors[itemID.uuidString.lowercased()] != nil)
        let exceptions = try #require(context["exceptions"] as? [[String: Any]])
        #expect(exceptions.count == 1)
        let selector = try #require(exceptions[0]["selector"] as? [String: Any])
        #expect(selector["id"] as? String == skippedOccurrence.uuidString.lowercased())

        let assignments = try #require(body["previous_assignments"] as? [[String: Any]])
        let byOccurrence: [String: Bool] = Dictionary(
            uniqueKeysWithValues: assignments.compactMap { assignment -> (String, Bool)? in
            guard let occurrence = assignment["occurrence_id"] as? String,
                  let pinned = assignment["pinned"] as? Bool else { return nil }
            return (occurrence, pinned)
            }
        )
        #expect(byOccurrence[frozenOccurrence.uuidString.lowercased()] == true)
        #expect(byOccurrence[mixedOccurrence.uuidString.lowercased()] == false)
    }

    @Test("preview revision mismatches trigger bounded delta retry")
    func testPreviewRevisionMismatchRetriesDelta() async throws {
        let token = "canonical-revision-retry-token"
        let itemID = UUID(uuidString: "24000000-2222-4333-8444-200000000000")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let initial = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let planner = PlannerStore(
            canonicalItems: [initial],
            canonicalDeltaCursor: "revision-1",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"revision-1","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 2]).utf8)),
            .init(statusCode: 200, body: Data("""
            {"changes":[{"type":"upsert","item":\(Self.itemObject(id: itemID, revision: 2))}],
             "next_cursor":"revision-2","has_more":false}
            """.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 2]).utf8))
        )

        let sync = Self.makeSync(planner: planner, token: token, now: now)
        await sync.sync()

        #expect(planner.canonicalItem(id: itemID)?.revision == 2)
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "POST", "GET", "POST"])
        #expect(sync.warnings.contains { $0.contains("different item revisions") })
    }

    @Test("invalid preview quarantines blocks made stale by the preceding delta")
    func testInvalidPreviewQuarantinesStaleBlocks() async throws {
        let token = "canonical-invalid-preview-token"
        let itemID = UUID(uuidString: "24500000-2222-4333-8444-200000000000")!
        let blockID = UUID(uuidString: "24500000-2222-4333-8444-200000000001")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let staleBlock = Self.block(
            itemID: itemID,
            revision: 1,
            start: now.addingTimeInterval(3_600)
        )
        let planner = PlannerStore(
            blocks: [staleBlock],
            canonicalItems: [item],
            canonicalDeltaCursor: "before-invalid-preview",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            schedulePreviewProvenance: Self.provenance(now: now),
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        var preview = try #require(
            JSONSerialization.jsonObject(
                with: Data(Self.previewObject(itemID: itemID, blockID: blockID).utf8)
            ) as? [String: Any]
        )
        var plan = try #require(preview["plan"] as? [String: Any])
        var blocks = try #require(plan["blocks"] as? [[String: Any]])
        blocks.append(try #require(blocks.first))
        plan["blocks"] = blocks
        preview["plan"] = plan
        let invalidPreview = try JSONSerialization.data(withJSONObject: preview)
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data("""
            {"changes":[{"type":"upsert","item":\(Self.itemObject(id: itemID, revision: 2))}],
             "next_cursor":"revision-2","has_more":false}
            """.utf8)),
            .init(statusCode: 200, body: invalidPreview)
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(planner.canonicalItem(id: itemID)?.revision == 2)
        #expect(!planner.canMutate(staleBlock))
    }

    @Test("canonical hierarchy order is transitive and deterministic")
    func testHierarchyOrdering() throws {
        let parentID = UUID(uuidString: "25000000-2222-4333-8444-200000000000")!
        let childID = UUID(uuidString: "25000000-2222-4333-8444-200000000001")!
        let grandchildID = UUID(uuidString: "25000000-2222-4333-8444-200000000002")!
        let rootSiblingID = UUID(uuidString: "25000000-2222-4333-8444-200000000003")!
        let parent = try Self.decodeItem(Self.itemObject(id: parentID, revision: 1, siblingOrder: 0))
        let child = try Self.decodeItem(Self.itemObject(id: childID, revision: 1, parentID: parentID))
        let grandchild = try Self.decodeItem(Self.itemObject(id: grandchildID, revision: 1, parentID: childID))
        let sibling = try Self.decodeItem(Self.itemObject(id: rootSiblingID, revision: 1, siblingOrder: 1))
        let planner = PlannerStore(restoreFromPersistence: false)

        planner.applyCanonicalDelta(
            [.upsert(grandchild), .upsert(sibling), .upsert(child), .upsert(parent)],
            nextCursor: "hierarchy"
        )

        #expect(planner.canonicalItems.map(\.id) == [parentID, childID, grandchildID, rootSiblingID])
    }

    @Test("deep canonical hierarchy is ordered without recursive stack growth")
    func testDeepHierarchyOrdering() throws {
        let depth = 5_000
        var expected: [UUID] = []
        var changes: [DayWeaveItemDeltaChange] = []
        var parentID: UUID?
        for index in 0..<depth {
            let id = try #require(UUID(
                uuidString: String(format: "26000000-2222-4333-8444-%012llx", UInt64(index))
            ))
            expected.append(id)
            let item = try Self.decodeItem(Self.itemObject(
                id: id,
                revision: 1,
                parentID: parentID
            ))
            changes.append(.upsert(item))
            parentID = id
        }
        let planner = PlannerStore(restoreFromPersistence: false)

        planner.applyCanonicalDelta(Array(changes.reversed()), nextCursor: "deep-hierarchy")

        #expect(planner.canonicalItems.map(\.id) == expected)
    }

    @Test("credential rotation cancels the old generation and fences its result")
    func testCredentialRotationFencesOldSync() async throws {
        let oldToken = "canonical-credential-old"
        let newToken = "canonical-credential-new"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let configuration = RotatingAPIConfigurationStore(baseURL: "https://old.example/gateway")
        let tokens = TestBearerTokenStore(token: oldToken, origin: "https://old.example")
        let planner = PlannerStore(restoreFromPersistence: false, now: { now })
        URLProtocolStub.storage.reset(key: oldToken)
        URLProtocolStub.storage.reset(key: newToken)
        URLProtocolStub.storage.enqueue(
            key: oldToken,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"must-not-apply","has_more":false}"#.utf8),
                delay: 0.2
            )
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: configuration,
            tokenStore: tokens,
            session: URLProtocolStub.makeSession(),
            now: { now }
        )

        let oldRun = Task { await sync.sync() }
        try await Task.sleep(for: .milliseconds(30))
        tokens.saveToken(newToken, origin: "https://new.example")
        configuration.saveBaseURL("https://new.example/gateway")
        sync.configurationDidChange()
        await oldRun.value

        #expect(planner.canonicalDeltaCursor == nil)
        #expect(!planner.isCanonicalSyncLocked)
        let oldRequest = try #require(URLProtocolStub.storage.requests(for: oldToken).first)
        #expect(oldRequest.url.host == "old.example")

        URLProtocolStub.storage.enqueue(
            key: newToken,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"new-generation","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8))
        )
        await sync.sync()

        #expect(planner.canonicalDeltaCursor == "new-generation")
        let newRequests = URLProtocolStub.storage.requests(for: newToken)
        #expect(newRequests.count == 2)
        #expect(newRequests.allSatisfy { $0.url.host == "new.example" })
    }

    @Test("canonical cache cannot cross API configurations")
    func testCanonicalCacheConfigurationBinding() async throws {
        let token = "canonical-configuration-binding-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let planner = PlannerStore(
            canonicalDeltaCursor: "old-server-cursor",
            canonicalConfigurationIdentifier: "https://old.example/gateway",
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(planner.canonicalDeltaCursor == "old-server-cursor")
        #expect(URLProtocolStub.storage.requests(for: token).isEmpty)
    }

    @Test("invalid legacy captures are quarantined without wedging valid publication")
    func testInvalidLegacyCaptureIsSkippedWithRecoveryDiagnostic() async throws {
        let token = "canonical-invalid-capture-token"
        let validID = UUID(uuidString: "27000000-2222-4333-8444-200000000000")!
        let invalidID = UUID(uuidString: "27000000-2222-4333-8444-200000000001")!
        let previewBlockID = UUID(uuidString: "27000000-2222-4333-8444-200000000002")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        func local(id: UUID, title: String, offset: TimeInterval) -> ScheduleBlock {
            ScheduleBlock(
                id: id, title: title, kind: .task,
                start: now.addingTimeInterval(offset),
                end: now.addingTimeInterval(offset + 1_800), status: .scheduled,
                project: nil, notes: "", energy: .medium, isFlexible: true,
                isHardConstraint: false, actualMinutes: nil, syncOrigin: .local
            )
        }
        let planner = PlannerStore(
            blocks: [
                local(
                    id: invalidID,
                    title: String(repeating: "x", count: PlannerStore.maximumCanonicalTitleScalars + 1),
                    offset: 1_800
                ),
                local(id: validID, title: "Valid capture", offset: 3_600),
            ],
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"capture","has_more":false}"#.utf8)),
            .init(statusCode: 201, body: Data("{\"item\":\(Self.itemObject(id: validID, revision: 1))}".utf8)),
            .init(statusCode: 200, body: Data(Self.previewObject(itemID: validID, blockID: previewBlockID).utf8))
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure == false)
        #expect(planner.localCaptureDiagnostics[invalidID]?.contains("1–500") == true)
        #expect(planner.blocks.contains { $0.id == invalidID && $0.sourceItemID == nil })
        #expect(planner.canonicalItem(id: validID) != nil)
        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.map(\.method) == ["GET", "POST", "POST"])
        #expect((requests[1].jsonBody?["id"] as? String) == validID.uuidString.lowercased())
    }

    @Test("create pushes resume after a per-sync safety cap")
    func testCreatePushCapIsResumable() async throws {
        let token = "canonical-create-cap-token"
        let firstID = UUID(uuidString: "27100000-2222-4333-8444-200000000000")!
        let secondID = UUID(uuidString: "27100000-2222-4333-8444-200000000001")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        func local(id: UUID, offset: TimeInterval) -> ScheduleBlock {
            ScheduleBlock(
                id: id, title: "Capture \(id.uuidString.suffix(1))", kind: .task,
                start: now.addingTimeInterval(offset),
                end: now.addingTimeInterval(offset + 1_800), status: .scheduled,
                project: nil, notes: "", energy: .medium, isFlexible: true,
                isHardConstraint: false, actualMinutes: nil, syncOrigin: .local
            )
        }
        let planner = PlannerStore(
            blocks: [local(id: firstID, offset: 1_800), local(id: secondID, offset: 3_600)],
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"cap-1","has_more":false}"#.utf8)),
            .init(statusCode: 201, body: Data("{\"item\":\(Self.itemObject(id: firstID, revision: 1))}".utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [firstID: 1]).utf8)),
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"cap-2","has_more":false}"#.utf8)),
            .init(statusCode: 201, body: Data("{\"item\":\(Self.itemObject(id: secondID, revision: 1))}".utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [firstID: 1, secondID: 1]).utf8))
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.configurationIdentifier),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            createPushLimit: 1,
            now: { now }
        )

        await sync.sync()
        #expect(planner.blocks.contains { $0.id == secondID && $0.sourceItemID == nil })
        #expect(sync.warnings.contains { $0.contains("request safety cap") })
        await sync.sync()

        #expect(planner.canonicalItems.map(\.id).sorted { $0.uuidString < $1.uuidString } == [firstID, secondID])
        #expect(planner.blocks.allSatisfy { $0.sourceItemID != nil })
        let createIDs = URLProtocolStub.storage.requests(for: token)
            .filter { $0.method == "POST" && $0.url.path.hasSuffix("/v1/items") }
            .compactMap { $0.jsonBody?["id"] as? String }
        #expect(createIDs == [firstID.uuidString.lowercased(), secondID.uuidString.lowercased()])
    }

    @Test("status pushes and previous assignments obey deterministic semantic caps")
    func testStatusAndPreviousAssignmentCaps() async throws {
        let token = "canonical-semantic-cap-token"
        let firstID = UUID(uuidString: "27200000-2222-4333-8444-200000000000")!
        let secondID = UUID(uuidString: "27200000-2222-4333-8444-200000000001")!
        let thirdID = UUID(uuidString: "27200000-2222-4333-8444-200000000002")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let ids = [firstID, secondID, thirdID]
        let items = try ids.map { try Self.decodeItem(Self.itemObject(id: $0, revision: 1)) }
        let blocks = ids.enumerated().map { index, id -> ScheduleBlock in
            var block = Self.block(
                itemID: id,
                revision: 1,
                start: now.addingTimeInterval(TimeInterval(1_800 + index * 3_600)),
                status: index < 2 ? .paused : .scheduled
            )
            block.occurrenceFullyScheduled = true
            return block
        }
        let mutations = [firstID, secondID].map { id in
            PendingCanonicalMutation(
                id: UUID(), itemID: id, occurrenceID: nil, sessionIndex: 0,
                desiredStatus: .paused, baseRevision: 1, createdAt: now,
                disposition: .pending, diagnostic: nil
            )
        }
        let planner = PlannerStore(
            blocks: blocks,
            canonicalItems: items,
            canonicalDeltaCursor: "before-caps",
            pendingCanonicalMutations: mutations,
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            schedulePreviewProvenance: Self.provenance(now: now),
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"after-caps","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data("{\"item\":\(Self.itemObject(id: firstID, revision: 2, status: "paused"))}".utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [firstID: 2, secondID: 1, thirdID: 1]).utf8))
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.configurationIdentifier),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            statusPushLimit: 1,
            previousAssignmentLimit: 2,
            previousAssignmentBlockLimit: 1,
            now: { now }
        )

        await sync.sync()

        #expect(planner.pendingCanonicalMutations.map(\.itemID) == [secondID])
        #expect(sync.warnings.contains { $0.contains("status pushes") })
        #expect(sync.warnings.contains { $0.contains("2-assignment/1-block") })
        let previewRequest = try #require(
            URLProtocolStub.storage.requests(for: token).last?.jsonBody
        )
        let assignments = try #require(previewRequest["previous_assignments"] as? [[String: Any]])
        #expect(assignments.count == 1)
        #expect(assignments[0]["item_id"] as? String == secondID.uuidString.lowercased())
    }

    @Test("configuration changes and reset clear sync-owned preview diagnostics")
    func testConfigurationChangeAndResetClearSyncOwnedState() async throws {
        let token = "canonical-reset-owned-state-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let invalid = ScheduleBlock(
            id: UUID(),
            title: String(repeating: "x", count: PlannerStore.maximumCanonicalTitleScalars + 1),
            kind: .task, start: now, end: now.addingTimeInterval(1_800),
            status: .scheduled, project: nil, notes: "", energy: .medium,
            isFlexible: true, isHardConstraint: false, actualMinutes: nil,
            syncOrigin: .local
        )
        let planner = PlannerStore(blocks: [invalid], restoreFromPersistence: false, now: { now })
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"owned-1","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)),
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"owned-2","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8))
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()
        #expect(sync.lastPreview != nil)
        #expect(!sync.warnings.isEmpty)
        sync.configurationDidChange()
        #expect(sync.lastPreview == nil)
        #expect(sync.warnings.isEmpty)
        #expect(sync.status == .ready)

        await sync.sync()
        #expect(sync.lastPreview != nil)
        #expect(!sync.warnings.isEmpty)
        sync.resetCanonicalSyncState()
        #expect(sync.lastPreview == nil)
        #expect(sync.warnings.isEmpty)
        #expect(sync.status == .ready)
        #expect(planner.canonicalDeltaCursor == nil)
    }

    @Test("client construction failure invalidates old preview actionability first")
    func testFailedClientConstructionInvalidatesPreview() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let itemID = UUID(uuidString: "27300000-2222-4333-8444-200000000000")!
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let block = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(3_600))
        let planner = PlannerStore(
            blocks: [block], canonicalItems: [item], canonicalDeltaCursor: "actionable",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            schedulePreviewProvenance: Self.provenance(now: now),
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(planner.canMutate(block))
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.configurationIdentifier),
            tokenStore: TestBearerTokenStore(token: "wrong-origin", origin: "https://other.example.com"),
            session: URLProtocolStub.makeSession(),
            now: { now }
        )

        await sync.sync()

        if case .configurationRequired = sync.status {} else {
            Issue.record("Credential mismatch should remain a configuration-required state")
        }
        #expect(!planner.canMutate(block))
        #expect(planner.canonicalPreviewFreshnessIssue != nil)
    }

    @Test("overlapping work and inconsistent score totals reject a preview")
    func testPreviewOverlapAndScoreValidation() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let itemID = UUID(uuidString: "27400000-2222-4333-8444-200000000000")!
        let blockID = UUID(uuidString: "27400000-2222-4333-8444-200000000001")!
        for variant in ["overlap", "score"] {
            let token = "canonical-invalid-preview-\(variant)"
            let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
            let planner = PlannerStore(
                canonicalItems: [item], canonicalDeltaCursor: "invalid-\(variant)",
                canonicalConfigurationIdentifier: Self.configurationIdentifier,
                restoreFromPersistence: false,
                now: { now }
            )
            var object = try #require(
                JSONSerialization.jsonObject(
                    with: Data(Self.previewObject(itemID: itemID, blockID: blockID).utf8)
                ) as? [String: Any]
            )
            var plan = try #require(object["plan"] as? [String: Any])
            if variant == "overlap" {
                var blocks = try #require(plan["blocks"] as? [[String: Any]])
                var second = try #require(blocks.first)
                second["id"] = UUID().uuidString.lowercased()
                second["session_index"] = 1
                blocks.append(second)
                plan["blocks"] = blocks
                var score = try #require(plan["score"] as? [String: Any])
                score["scheduled_minutes"] = 90
                plan["score"] = score
            } else {
                var score = try #require(plan["score"] as? [String: Any])
                score["scheduled_minutes"] = 44
                plan["score"] = score
            }
            object["plan"] = plan
            URLProtocolStub.storage.reset(key: token)
            URLProtocolStub.storage.enqueue(
                key: token,
                .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"invalid-after","has_more":false}"#.utf8)),
                .init(statusCode: 200, body: try JSONSerialization.data(withJSONObject: object))
            )
            let sync = Self.makeSync(planner: planner, token: token, now: now)

            await sync.sync()

            #expect(sync.status.isFailure)
            #expect(sync.lastPreview == nil)
        }
    }

    private static func itemObject(
        id: UUID,
        revision: UInt64,
        status: String = "scheduled",
        parentID: UUID? = nil,
        siblingOrder: UInt32 = 0,
        splitPolicy: String = #"{"type":"indivisible"}"#
    ) -> String {
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        return """
        {"id":"\(id.uuidString.lowercased())","kind":"task","status":"\(status)",
         "title":"Write launch plan","notes":"Private local notes","timezone_name":"Europe/Madrid",
         "duration_seconds":2700,"deadline_at":null,"earliest_start_at":null,"recurrence":null,
         "flexible_constraints":{"energy":"deep"},"split_policy":\(splitPolicy),
         "importance":50,"urgency":50,"parent_id":\(parent),"sibling_order":\(siblingOrder),"is_executable":true,
         "revision":\(revision),"created_at":"2026-08-29T08:00:00Z",
         "updated_at":"2026-08-29T08:00:00Z","completed_at":null,"deleted_at":null}
        """
    }

    private static func previewObject(itemID: UUID, blockID: UUID) -> String {
        let asOf = Date(timeIntervalSince1970: 1_787_990_400)
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: asOf)
        let horizonEnd = calendar.date(byAdding: .day, value: 7, to: horizonStart)
            ?? horizonStart.addingTimeInterval(7 * 86_400)
        let blockStart = asOf.addingTimeInterval(3_600)
        let blockEnd = blockStart.addingTimeInterval(2_700)
        return """
        {"input_digest":"sha256:test","source_item_count":1,"accepted_item_count":1,
         "source_item_revisions":{"\(itemID.uuidString.lowercased())":1},
         "rejected_items":[],"ignored_previous_assignments":[],"plan":{
           "as_of":"\(wireTimestamp(asOf))","horizon_start":"\(wireTimestamp(horizonStart))",
           "horizon_end":"\(wireTimestamp(horizonEnd))","blocks":[{
             "id":"\(blockID.uuidString.lowercased())","item_id":"\(itemID.uuidString.lowercased())",
             "occurrence_id":null,"external_block_id":null,"title":"Write launch plan",
             "start":"\(wireTimestamp(blockStart))","end":"\(wireTimestamp(blockEnd))",
             "session_index":0,"kind":"planned","explanations":[
               {"code":"earliest_available","message":"Placed in the earliest matching opening."}
             ]}],"unscheduled":[],"decisions":[],"violations":[],
           "score":{"scheduled_minutes":45,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},
           "occurrences":[]}}
        """
    }

    private static func emptyPreviewObject(sourceRevisions: [UUID: UInt64]) -> String {
        let asOf = Date(timeIntervalSince1970: 1_787_990_400)
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: asOf)
        let horizonEnd = calendar.date(byAdding: .day, value: 7, to: horizonStart)
            ?? horizonStart.addingTimeInterval(7 * 86_400)
        let revisions = sourceRevisions
            .sorted { $0.key.uuidString < $1.key.uuidString }
            .map { "\"\($0.key.uuidString.lowercased())\":\($0.value)" }
            .joined(separator: ",")
        return """
        {"input_digest":"sha256:empty","source_item_count":\(sourceRevisions.count),
         "accepted_item_count":\(sourceRevisions.count),"source_item_revisions":{\(revisions)},
         "rejected_items":[],"ignored_previous_assignments":[],"plan":{
           "as_of":"\(wireTimestamp(asOf))","horizon_start":"\(wireTimestamp(horizonStart))",
           "horizon_end":"\(wireTimestamp(horizonEnd))","blocks":[],"unscheduled":[],
           "decisions":[],"violations":[],"score":{"scheduled_minutes":0,
           "unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}
        """
    }

    private static func wireTimestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }

    private static func decodeItem(_ object: String) throws -> DayWeaveCanonicalItem {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(object.utf8))
    }

    private static func block(
        itemID: UUID,
        revision: UInt64,
        start: Date,
        status: PlannerItemStatus = .scheduled
    ) -> ScheduleBlock {
        ScheduleBlock(
            id: UUID(),
            title: "Canonical work",
            kind: .task,
            start: start,
            end: start.addingTimeInterval(1_800),
            status: status,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: revision,
            syncOrigin: .canonicalPreview,
            previewKind: "planned"
        )
    }

    private static func makeSync(
        planner: PlannerStore,
        token: String,
        now: Date
    ) -> CanonicalSyncStore {
        CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: "https://api.example.com/gateway"),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            now: { now }
        )
    }

    private static let configurationIdentifier = "https://api.example.com/gateway"

    private static func provenance(now: Date) -> SchedulePreviewProvenance {
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: now)
        return .init(
            configurationIdentifier: configurationIdentifier,
            generatedAt: now,
            asOf: now,
            horizonStart: horizonStart,
            horizonEnd: calendar.date(byAdding: .day, value: 7, to: horizonStart)
                ?? horizonStart.addingTimeInterval(7 * 86_400),
            timezoneName: TimeZone.autoupdatingCurrent.identifier == "GMT"
                ? "UTC"
                : TimeZone.autoupdatingCurrent.identifier
        )
    }
}

private struct FixedAPIConfigurationStore: SuggestionAPIConfigurationStoring {
    let baseURL: String
    func loadBaseURL() -> String? { baseURL }
    func saveBaseURL(_ value: String) {}
}

private final class RotatingAPIConfigurationStore: SuggestionAPIConfigurationStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var baseURL: String

    init(baseURL: String) {
        self.baseURL = baseURL
    }

    func loadBaseURL() -> String? {
        lock.withLock { baseURL }
    }

    func saveBaseURL(_ value: String) {
        lock.withLock { baseURL = value }
    }
}
#endif
