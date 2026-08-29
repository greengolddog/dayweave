import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical planner sync", .serialized)
@MainActor
struct CanonicalSyncStoreTests {
    @Test("sync publishes a local capture, previews without side effects, then publishes the schedule")
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
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 1, isSensitive: true))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.previewObject(
                    itemID: itemID,
                    blockID: previewBlockID,
                    itemIsSensitive: true
                ).utf8)
            )
        )
        let local = ScheduleBlock(
            id: itemID,
            isSensitive: true,
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
        #expect(planner.blocks[0].isSensitive)
        #expect(planner.canonicalItems[0].isSensitive)
        #expect(planner.blocks[0].placementReason == "Placed in the earliest matching opening.")
        #expect(sync.lastPreview?.inputDigest == "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        if case .online = sync.status {} else { Issue.record("Expected online sync status") }

        let requests = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        )
        #expect(requests.map(\.method) == ["GET", "POST", "POST", "POST"])
        #expect(requests.map(\.url.path) == [
            "/gateway/v1/items/delta",
            "/gateway/v1/items",
            "/gateway/v1/schedule/preview",
            "/gateway/v1/schedule/publish",
        ])
        #expect(requests[1].headers["Idempotency-Key"] == "mac-create-\(itemID.uuidString.lowercased())")
        #expect(requests[1].jsonBody?["is_sensitive"] as? Bool == true)
        let previewBody = try #require(requests[2].jsonBody)
        let previous = try #require(previewBody["previous_assignments"] as? [[String: Any]])
        // A locally guessed placement is not a server-authored stability hint.
        #expect(previous.isEmpty)
        let publicationBody = try #require(requests[3].jsonBody)
        #expect(publicationBody["expected_input_digest"] as? String == sync.lastPreview?.inputDigest)
        #expect(publicationBody["schedule"] as? [String: Any] != nil)
        #expect(UUID(uuidString: try #require(publicationBody["idempotency_key"] as? String)) != nil)

        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"cursor-0","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.previewObject(
                    itemID: itemID,
                    blockID: previewBlockID,
                    itemIsSensitive: true
                ).utf8)
            )
        )
        await sync.sync()
        if case .online = sync.status {} else {
            Issue.record("An unchanged terminal delta cursor must remain a successful no-op")
        }
        #expect(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).map(\.method) == [
            "GET", "POST", "POST", "POST", "GET", "POST", "POST",
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
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

    @Test("privacy edits are revision-guarded, stable, and safely rebase a local status edit")
    func testSensitivityEditRebasesStatusIntent() async throws {
        let token = "canonical-sensitive-edit-token"
        let itemID = UUID(uuidString: "22100000-2222-4333-8444-200000000000")!
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
            canonicalDeltaCursor: "privacy-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(planner.setCanonicalItemSensitivity(itemID, isSensitive: true))
        #expect(planner.blocks[0].isSensitive)
        #expect(planner.pendingCanonicalSensitivityMutations.count == 1)

        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"privacy-same","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 2, isSensitive: true))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 3, status: "paused", isSensitive: true))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 3]).utf8)
            )
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        #expect(planner.canonicalItem(id: itemID)?.revision == 3)
        #expect(planner.canonicalItem(id: itemID)?.isSensitive == true)
        #expect(planner.canonicalItem(id: itemID)?.status == .paused)
        #expect(planner.pendingCanonicalSensitivityMutations.isEmpty)
        #expect(planner.pendingCanonicalMutations.isEmpty)

        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.map(\.method) == ["GET", "PUT", "PUT", "POST"])
        let privacyBody = try #require(requests[1].jsonBody)
        #expect((privacyBody["expected_revision"] as? NSNumber)?.uint64Value == 1)
        #expect((privacyBody["item"] as? [String: Any])?["is_sensitive"] as? Bool == true)
        #expect(
            requests[1].headers["Idempotency-Key"]
                == "mac-sensitive-\(itemID.uuidString.lowercased())-r1-private"
        )
        let statusBody = try #require(requests[2].jsonBody)
        #expect((statusBody["expected_revision"] as? NSNumber)?.uint64Value == 2)
        #expect((statusBody["item"] as? [String: Any])?["is_sensitive"] as? Bool == true)
        #expect((statusBody["item"] as? [String: Any])?["status"] as? String == "paused")
    }

    @Test("a stale privacy edit stays encrypted as an explicit conflict")
    func testSensitivityConflictRetainsIntent() async throws {
        let token = "canonical-sensitive-conflict-token"
        let itemID = UUID(uuidString: "22100000-2222-4333-8444-200000000001")!
        let blockID = UUID(uuidString: "22100000-2222-4333-8444-200000000002")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let planner = PlannerStore(
            canonicalItems: [item],
            canonicalDeltaCursor: "privacy-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(planner.setCanonicalItemSensitivity(itemID, isSensitive: true))
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"privacy-same","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 409,
                body: Data(#"{"error":{"code":"conflict","message":"revision changed"}}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.previewObject(
                    itemID: itemID,
                    blockID: blockID,
                    itemIsSensitive: false
                ).utf8)
            )
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        let mutation = try #require(planner.pendingCanonicalSensitivityMutations.first)
        #expect(mutation.desiredIsSensitive)
        #expect(mutation.baseRevision == 1)
        #expect(mutation.disposition == .conflicted)
        #expect(mutation.diagnostic?.contains("stale") == true)
        #expect(planner.blocks.first?.isSensitive == true)
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "PUT", "POST"])

        planner.retryConflictedCanonicalSensitivityMutation(mutation.id)
        let retried = try #require(planner.pendingCanonicalSensitivityMutations.first)
        #expect(retried.id == mutation.id)
        #expect(retried.disposition == .pending)
        #expect(retried.diagnostic == nil)
    }

    @Test("a lost declassification response cannot erase a later reclassification")
    func testAmbiguousDeclassificationQueuesExactReclassification() async throws {
        let token = "canonical-sensitive-ambiguous-token"
        let itemID = UUID(uuidString: "22100000-2222-4333-8444-200000000003")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(
            Self.itemObject(id: itemID, revision: 1, isSensitive: true)
        )
        let planner = PlannerStore(
            canonicalItems: [item],
            canonicalDeltaCursor: "privacy-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(planner.setCanonicalItemSensitivity(itemID, isSensitive: false))

        URLProtocolStub.storage.reset(key: token)
        // The replacement request is recorded, but the transport loses its
        // response. The next delta models that the server did apply it.
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"privacy-ambiguous","has_more":false}"#.utf8)
            )
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)
        await sync.sync()

        #expect(sync.status.isFailure)
        var mutation = try #require(planner.pendingCanonicalSensitivityMutations.first)
        #expect(mutation.desiredIsSensitive == false)
        #expect(mutation.hasBeenSubmitted)
        #expect(mutation.followUpIsSensitive == nil)

        // Cached state is still sensitive=true. Reclassifying must preserve
        // the ambiguous removal and durably queue a follow-up mark.
        #expect(planner.setCanonicalItemSensitivity(itemID, isSensitive: true))
        mutation = try #require(planner.pendingCanonicalSensitivityMutations.first)
        #expect(mutation.desiredIsSensitive == false)
        #expect(mutation.hasBeenSubmitted)
        #expect(mutation.followUpIsSensitive == true)
        #expect(mutation.requestedIsSensitive)

        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data("""
                {"changes":[{"type":"upsert","item":\(Self.itemObject(
                    id: itemID,
                    revision: 2,
                    isSensitive: false
                ))}],"next_cursor":"privacy-observed","has_more":false}
                """.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 3, isSensitive: true))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 3]).utf8)
            )
        )
        await sync.sync()

        #expect(planner.canonicalItem(id: itemID)?.revision == 3)
        #expect(planner.canonicalItem(id: itemID)?.isSensitive == true)
        #expect(planner.pendingCanonicalSensitivityMutations.isEmpty)
        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.map(\.method) == ["GET", "PUT", "GET", "PUT", "POST"])
        let lostRemoval = try #require(requests[1].jsonBody)
        #expect((lostRemoval["expected_revision"] as? NSNumber)?.uint64Value == 1)
        #expect((lostRemoval["item"] as? [String: Any])?["is_sensitive"] as? Bool == false)
        let restoration = try #require(requests[3].jsonBody)
        #expect((restoration["expected_revision"] as? NSNumber)?.uint64Value == 2)
        #expect((restoration["item"] as? [String: Any])?["is_sensitive"] as? Bool == true)
    }

    @Test("status waits behind a capped privacy follow-up on the same item")
    func testStatusWaitsBehindCappedPrivacyFollowUp() async throws {
        let token = "canonical-sensitive-follow-up-cap-token"
        let itemID = UUID(uuidString: "22100000-2222-4333-8444-200000000005")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(
            Self.itemObject(id: itemID, revision: 1, isSensitive: true)
        )
        let block = Self.block(
            itemID: itemID,
            revision: 1,
            start: now.addingTimeInterval(3_600),
            status: .paused
        )
        let statusMutation = PendingCanonicalMutation(
            id: UUID(uuidString: "22100000-2222-4333-8444-200000000006")!,
            itemID: itemID,
            occurrenceID: nil,
            sessionIndex: nil,
            desiredStatus: .paused,
            baseRevision: 1,
            createdAt: now,
            disposition: .pending,
            diagnostic: nil
        )
        let privacyMutation = PendingCanonicalSensitivityMutation(
            id: UUID(uuidString: "22100000-2222-4333-8444-200000000007")!,
            itemID: itemID,
            desiredIsSensitive: false,
            baseRevision: 1,
            createdAt: now,
            disposition: .pending,
            diagnostic: nil,
            hasBeenSubmitted: true,
            followUpIsSensitive: true
        )
        let planner = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            canonicalDeltaCursor: "privacy-cap-before",
            pendingCanonicalMutations: [statusMutation],
            pendingCanonicalSensitivityMutations: [privacyMutation],
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"privacy-cap-one","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 2, isSensitive: false))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 2]).utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"privacy-cap-two","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 3, isSensitive: true))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 4, status: "paused", isSensitive: true))}".utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [itemID: 4]).utf8)
            )
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.baseURLString),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            statusPushLimit: 1,
            now: { now }
        )

        await sync.sync()

        let followUp = try #require(planner.pendingCanonicalSensitivityMutations.first)
        #expect(followUp.desiredIsSensitive)
        #expect(followUp.baseRevision == 2)
        #expect(followUp.disposition == .pending)
        #expect(planner.pendingCanonicalMutations.first?.baseRevision == 2)
        #expect(planner.canonicalItem(id: itemID)?.revision == 2)
        #expect(planner.blocks.first?.isSensitive == true)
        #expect(sync.warnings.contains { $0.contains("status edit was deferred safely") })
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "PUT", "POST"])

        await sync.sync()

        #expect(planner.pendingCanonicalSensitivityMutations.isEmpty)
        #expect(planner.pendingCanonicalMutations.isEmpty)
        #expect(planner.canonicalItem(id: itemID)?.revision == 4)
        #expect(planner.canonicalItem(id: itemID)?.isSensitive == true)
        #expect(planner.canonicalItem(id: itemID)?.status == .paused)
        #expect(
            URLProtocolStub.storage.requests(for: token).map(\.method)
                == ["GET", "PUT", "POST", "GET", "PUT", "PUT", "POST"]
        )
    }

    @Test("status replacement cannot remove a sensitive parent")
    func testStatusReplacementRejectsParentRemoval() async throws {
        let token = "canonical-status-parent-removal-token"
        let parentID = UUID(uuidString: "22200000-2222-4333-8444-200000000000")!
        let childID = UUID(uuidString: "22200000-2222-4333-8444-200000000001")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let parent = try Self.decodeItem(Self.itemObject(
            id: parentID,
            revision: 1,
            isSensitive: true
        ))
        let child = try Self.decodeItem(Self.itemObject(
            id: childID,
            revision: 1,
            parentID: parentID
        ))
        var block = Self.block(
            itemID: childID,
            revision: 1,
            start: now.addingTimeInterval(3_600),
            status: .paused
        )
        block.isSensitive = true
        let planner = PlannerStore(
            blocks: [block],
            canonicalItems: [parent, child],
            canonicalDeltaCursor: "status-parent-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"status-parent-same","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: childID, revision: 2, status: "paused", parentID: nil))}".utf8)
            )
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        #expect(planner.canonicalItem(id: childID)?.parentID == parentID)
        #expect(planner.pendingCanonicalMutations.first?.desiredStatus == .paused)
        #expect(planner.blocks.first?.isSensitive == true)
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "PUT"])
    }

    @Test("status replacement requires exactly base revision plus one")
    func testStatusReplacementRejectsRevisionJump() async throws {
        let token = "canonical-status-revision-jump-token"
        let itemID = UUID(uuidString: "22200000-2222-4333-8444-200000000002")!
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
            canonicalDeltaCursor: "status-jump-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"status-jump-same","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data("{\"item\":\(Self.itemObject(id: itemID, revision: 3, status: "paused"))}".utf8)
            )
        )

        await Self.makeSync(planner: planner, token: token, now: now).sync()

        #expect(planner.canonicalItem(id: itemID)?.revision == 1)
        #expect(planner.pendingCanonicalMutations.first?.desiredStatus == .paused)
        #expect(URLProtocolStub.storage.requests(for: token).map(\.method) == ["GET", "PUT"])
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: Self.provenance(now: now, token: token),
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: Self.provenance(now: now, token: token),
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

    @Test("preview sensitivity downgrade fails closed without replacing the prior plan")
    func testPreviewSensitivityDowngradeKeepsPriorPlan() async throws {
        let token = "canonical-sensitivity-downgrade-token"
        let itemID = UUID(uuidString: "24600000-2222-4333-8444-200000000000")!
        let blockID = UUID(uuidString: "24600000-2222-4333-8444-200000000001")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(
            Self.itemObject(id: itemID, revision: 1, isSensitive: true)
        )
        var priorBlock = Self.block(
            itemID: itemID,
            revision: 1,
            start: now.addingTimeInterval(3_600)
        )
        priorBlock.isSensitive = true
        priorBlock.title = "SYNTHETIC-SENSITIVE-PRIOR-PLAN-MACOS"
        let planner = PlannerStore(
            blocks: [priorBlock],
            canonicalItems: [item],
            canonicalDeltaCursor: "sensitivity-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: Self.provenance(now: now, token: token),
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"sensitivity-after","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(
                    Self.previewObject(
                        itemID: itemID,
                        blockID: blockID,
                        itemIsSensitive: false
                    ).utf8
                )
            )
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(sync.lastPreview == nil)
        #expect(planner.blocks == [priorBlock])
        #expect(planner.blocks[0].isSensitive)
    }

    @Test("preview must return every intersecting fixed block")
    func testPreviewRequiresCompleteFixedBlockCoverage() throws {
        let horizonStart = Date(timeIntervalSince1970: 1_788_033_600)
        let intersectingID = UUID(uuidString: "24600000-2222-4333-8444-200000000010")!
        let outsideID = UUID(uuidString: "24600000-2222-4333-8444-200000000011")!
        let request = DayWeaveSchedulePreviewRequest(
            asOf: horizonStart,
            horizonStart: horizonStart,
            horizonEnd: horizonStart.addingTimeInterval(3_600),
            timezoneName: "UTC",
            availability: [],
            fixedBlocks: [
                .init(
                    id: intersectingID,
                    isSensitive: true,
                    title: "SYNTHETIC-INTERSECTING-FIXED-CANARY",
                    start: horizonStart.addingTimeInterval(600),
                    end: horizonStart.addingTimeInterval(1_200),
                    source: "google_calendar"
                ),
                .init(
                    id: outsideID,
                    isSensitive: false,
                    title: "SYNTHETIC-OUTSIDE-FIXED-CANARY",
                    start: horizonStart.addingTimeInterval(7_200),
                    end: horizonStart.addingTimeInterval(7_800),
                    source: "manual"
                ),
            ],
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )

        #expect(throws: (any Error).self) {
            try CanonicalSyncStore.validateFixedBlockCoverage(
                returnedExternalBlockIDs: [],
                request: request
            )
        }
        try CanonicalSyncStore.validateFixedBlockCoverage(
            returnedExternalBlockIDs: [intersectingID],
            request: request
        )
        let duplicateRequest = DayWeaveSchedulePreviewRequest(
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            timezoneName: request.timezoneName,
            availability: request.availability,
            fixedBlocks: [request.fixedBlocks[0], request.fixedBlocks[0]],
            previousAssignments: request.previousAssignments,
            config: request.config,
            recurrenceContext: request.recurrenceContext
        )
        #expect(throws: (any Error).self) {
            try CanonicalSyncStore.validateFixedBlockCoverage(
                returnedExternalBlockIDs: [intersectingID],
                request: duplicateRequest
            )
        }
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

    @Test("publication failure keeps the prior plan non-current and retains exact recovery")
    func testPublicationFailureRetainsJournalAndPriorPlan() async throws {
        let token = "canonical-publication-failure-token"
        let itemID = UUID(uuidString: "26800000-2222-4333-8444-200000000000")!
        let priorBlock = Self.block(
            itemID: itemID,
            revision: 1,
            start: Date(timeIntervalSince1970: 1_787_994_000)
        )
        let newBlockID = UUID(uuidString: "26800000-2222-4333-8444-200000000001")!
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let priorProvenance = Self.provenance(now: now, token: token)
        let planner = PlannerStore(
            blocks: [priorBlock],
            canonicalItems: [item],
            canonicalDeltaCursor: "publication-before",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: priorProvenance,
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"publication-after","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.previewObject(itemID: itemID, blockID: newBlockID).utf8))
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    now: now,
                    replayed: false
                )
            )
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(sync.lastPreview == nil)
        #expect(planner.blocks == [priorBlock])
        #expect(planner.schedulePreviewProvenance == priorProvenance)
        #expect(planner.pendingSchedulePublication != nil)
        #expect(planner.canonicalPreviewFreshnessIssue != nil)
        let retained = planner.pendingSchedulePublication
        sync.resetCanonicalSyncState()
        #expect(planner.pendingSchedulePublication == retained)
        #expect(sync.status.message.contains("recover"))
    }

    @Test("publication rejects every non-200 success status and durably retains recovery")
    func testPublicationRequiresExactSuccessStatus() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-publication-status-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        for statusCode in [201, 202, 204] {
            let token = "canonical-publication-status-\(statusCode)-token"
            let persistence = EncryptedPlannerPersistence(
                fileURL: directory
                    .appendingPathComponent("\(statusCode)", isDirectory: true)
                    .appendingPathComponent("planner.snapshot.encrypted"),
                key: PlannerEncryptionKey.random()
            )
            let planner = PlannerStore(
                persistence: persistence,
                restoreFromPersistence: true,
                autosaveDelay: .seconds(60),
                now: { now }
            )
            URLProtocolStub.storage.reset(key: token)
            URLProtocolStub.storage.enqueue(
                key: token,
                .init(
                    statusCode: 200,
                    body: Data(#"{"changes":[],"next_cursor":"status-cursor","has_more":false}"#.utf8)
                ),
                .init(
                    statusCode: 200,
                    body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
                )
            )
            URLProtocolStub.storage.enqueueSchedulePublication(
                key: token,
                .init(
                    statusCode: statusCode,
                    body: statusCode == 204 ? Data() : Self.publicationResponse(
                        inputDigest: Self.emptyInputDigest,
                        now: now,
                        replayed: false
                    )
                )
            )
            let sync = Self.makeSync(planner: planner, token: token, now: now)

            await sync.sync()

            let pending = try #require(planner.pendingSchedulePublication)
            #expect(sync.status.isFailure)
            #expect(sync.status.message.contains("\(statusCode)"))
            #expect(sync.lastPreview == nil)
            #expect(planner.schedulePreviewProvenance == nil)
            #expect(planner.canonicalPreviewFreshnessIssue != nil)
            #expect(planner.persistenceError == nil)
            #expect(try persistence.load()?.pendingSchedulePublication == pending)
        }
    }

    @Test("a fresh key accepts an identical current revision with an old publication time")
    func testCurrentRevisionDedupeAcceptsOldPublishedAt() async throws {
        let token = "canonical-publication-current-dedupe-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let planner = PlannerStore(restoreFromPersistence: false, now: { now })
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"dedupe-current","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: Self.emptyInputDigest,
                    now: now,
                    replayed: false,
                    publishedAt: now.addingTimeInterval(-30 * 86_400)
                )
            )
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        if case .online = sync.status {} else {
            Issue.record("A fresh key bound to the identical current revision should be accepted")
        }
        #expect(sync.lastPreview?.inputDigest == Self.emptyInputDigest)
        #expect(planner.pendingSchedulePublication == nil)
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
    }

    @Test("one explicit stale publication is cleared and recomposed exactly once")
    func testStalePublicationGetsOneBoundedRetry() async throws {
        let token = "canonical-publication-stale-retry-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let planner = PlannerStore(restoreFromPersistence: false, now: { now })
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"stale-before","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"stale-after","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 409,
                headers: ["Content-Type": "application/json"],
                body: Self.stalePublicationResponse
            )
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        let publications = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
        #expect(publications.count == 2)
        if publications.count == 2 {
            #expect(publications[0].body != publications[1].body)
            #expect(
                publications[0].jsonBody?["idempotency_key"] as? String
                    != publications[1].jsonBody?["idempotency_key"] as? String
            )
        }
        #expect(planner.pendingSchedulePublication == nil)
        #expect(sync.lastPreview?.inputDigest == Self.emptyInputDigest)
        #expect(sync.warnings.contains { $0.contains("recomposed once") })
        if case .online = sync.status {} else {
            Issue.record("The single bounded stale retry should publish the fresh candidate")
        }
    }

    @Test("only the exact typed stale envelope may abandon a publication journal")
    func testUntrustedStaleLookalikesRetainJournal() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-publication-untrusted-stale-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let stale = Self.stalePublicationResponse
        let cases: [(statusCode: Int, headers: [String: String], body: Data)] = [
            (409, [:], stale),
            (409, ["Content-Type": "text/plain"], stale),
            (409, ["Content-Type": "application/json"], Data(#"{"error":"#.utf8)),
            (409, ["Content-Type": "application/json"], Data(#"{"error":{}}"#.utf8)),
            (
                409,
                ["Content-Type": "application/json"],
                Data(#"{"error":{"code":"conflict","message":"Generic conflict"}}"#.utf8)
            ),
            (
                409,
                ["Content-Type": "application/json"],
                Data(#"{"error":{"code":"schedule_publication_idempotency_conflict","message":"Tuple conflict"}}"#.utf8)
            ),
            (
                409,
                ["Content-Type": "application/json"],
                Data(#"{"error":{"code":"schedule_publication_stale","message":"Changed","future":true}}"#.utf8)
            ),
            (
                409,
                ["Content-Type": "application/json"],
                Data(#"{"error":{"code":"schedule_publication_stale","message":"Changed"},"future":true}"#.utf8)
            ),
            (422, ["Content-Type": "application/json"], stale),
        ]

        for (index, response) in cases.enumerated() {
            let token = "canonical-untrusted-stale-\(index)-token"
            let persistence = EncryptedPlannerPersistence(
                fileURL: directory
                    .appendingPathComponent(String(index), isDirectory: true)
                    .appendingPathComponent("planner.snapshot.encrypted"),
                key: PlannerEncryptionKey.random()
            )
            let planner = PlannerStore(
                persistence: persistence,
                restoreFromPersistence: true,
                autosaveDelay: .seconds(60),
                now: { now }
            )
            URLProtocolStub.storage.reset(key: token)
            URLProtocolStub.storage.enqueue(
                key: token,
                .init(
                    statusCode: 200,
                    body: Data(#"{"changes":[],"next_cursor":"untrusted-stale","has_more":false}"#.utf8)
                ),
                .init(
                    statusCode: 200,
                    body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
                )
            )
            URLProtocolStub.storage.enqueueSchedulePublication(
                key: token,
                .init(
                    statusCode: response.statusCode,
                    headers: response.headers,
                    body: response.body
                )
            )
            let sync = Self.makeSync(planner: planner, token: token, now: now)

            await sync.sync()

            let retained = try #require(planner.pendingSchedulePublication)
            #expect(sync.status.isFailure)
            #expect(sync.lastPreview == nil)
            #expect(planner.canonicalPreviewFreshnessIssue != nil)
            #expect(try persistence.load()?.pendingSchedulePublication == retained)
            let publications = URLProtocolStub.storage.requests(
                for: token,
                includingSchedulePublication: true
            ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
            #expect(publications.count == 1)
            #expect(URLProtocolStub.storage.requests(for: token).count == 2)
        }
    }

    @Test("a second stale publication clears its journal and surfaces a bounded failure")
    func testSecondStalePublicationClearsAndStops() async throws {
        let token = "canonical-publication-second-stale-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-publication-second-stale-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: PlannerEncryptionKey.random()
        )
        let planner = PlannerStore(
            persistence: persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"twice-before","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"twice-after","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 409,
                headers: ["Content-Type": "application/json"],
                body: Self.stalePublicationResponse
            ),
            .init(
                statusCode: 409,
                headers: ["Content-Type": "application/json"],
                body: Self.stalePublicationResponse
            )
        )
        let sync = Self.makeSync(planner: planner, token: token, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(sync.status.message.contains("both bounded publication attempts"))
        #expect(sync.lastPreview == nil)
        #expect(planner.pendingSchedulePublication == nil)
        #expect(planner.canonicalPreviewFreshnessIssue != nil)
        #expect(try persistence.load()?.pendingSchedulePublication == nil)
        let publications = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
        #expect(publications.count == 2)
    }

    @Test("a recent replay stays non-current until one fresh publication succeeds")
    func testRecentReplayClearsWithoutInstallingBeforeFreshPublication() async throws {
        let token = "canonical-publication-recent-replay-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-publication-recent-replay-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: PlannerEncryptionKey.random()
        )
        let priorBlock = ScheduleBlock(
            id: UUID(uuidString: "26900000-2222-4333-8444-200000000000")!,
            title: "Prior external hold",
            kind: .breakTime,
            start: now.addingTimeInterval(1_800),
            end: now.addingTimeInterval(2_700),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .low,
            isFlexible: false,
            isHardConstraint: true,
            actualMinutes: nil,
            syncOrigin: .externalPreview,
            previewKind: "external_fixed"
        )
        let planner = PlannerStore(
            blocks: [priorBlock],
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: Self.provenance(now: now, token: token),
            previewValidatedForCurrentLaunch: true,
            persistence: persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"recent-before","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    now: now,
                    replayed: false
                )
            )
        )
        let firstSync = Self.makeSync(planner: planner, token: token, now: now)
        await firstSync.sync()
        let pending = try #require(planner.pendingSchedulePublication)
        #expect(planner.blocks == [priorBlock])

        let replayNow = now.addingTimeInterval(60)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"recent-fresh","has_more":false}"#.utf8),
                delay: 0.25
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(
                    sourceRevisions: [:],
                    asOf: replayNow
                ).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: Self.emptyInputDigest,
                    now: now,
                    replayed: true
                )
            )
        )
        let replaySync = Self.makeSync(planner: planner, token: token, now: replayNow)
        let run = Task { await replaySync.sync() }
        for _ in 0..<100 {
            let deltaCount = URLProtocolStub.storage.requests(for: token)
                .count { $0.url.path.hasSuffix("/v1/items/delta") }
            if deltaCount >= 2, planner.pendingSchedulePublication == nil { break }
            try await Task.sleep(for: .milliseconds(5))
        }

        #expect(planner.pendingSchedulePublication == nil)
        #expect(planner.blocks == [priorBlock])
        #expect(planner.canonicalPreviewFreshnessIssue != nil)
        let acknowledgedSnapshot = try #require(try persistence.load())
        #expect(acknowledgedSnapshot.pendingSchedulePublication == nil)
        #expect(acknowledgedSnapshot.blocks == [priorBlock])
        let acknowledgedPublications = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
        let replayRequest = try #require(acknowledgedPublications.dropFirst().first)
        #expect(pending.preparedRequest.body == replayRequest.body)

        await run.value

        #expect(planner.blocks.isEmpty)
        #expect(planner.pendingSchedulePublication == nil)
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
        #expect(replaySync.lastPreview?.inputDigest == Self.emptyInputDigest)
        if case .online = replaySync.status {} else {
            Issue.record("One fresh publication should make the recomposed plan current")
        }
        let publications = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
        #expect(publications.count == 3)
        if publications.count == 3 {
            #expect(publications[0].body == publications[1].body)
            #expect(publications[2].body != publications[1].body)
        }
    }

    @Test("persistence failure restores the exact journal before any stale retry")
    func testStaleJournalClearRollsBackOnPersistenceFailure() async throws {
        let token = "canonical-publication-clear-failure-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-publication-clear-failure-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: PlannerEncryptionKey.random()
        )
        let planner = PlannerStore(
            persistence: persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"clear-failure","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    now: now,
                    replayed: false
                )
            )
        )
        let firstSync = Self.makeSync(planner: planner, token: token, now: now)
        await firstSync.sync()
        let pending = try #require(planner.pendingSchedulePublication)

        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 409,
                headers: ["Content-Type": "application/json"],
                body: Self.stalePublicationResponse,
                delay: 0.25
            )
        )
        let recovery = Self.makeSync(planner: planner, token: token, now: now)
        let run = Task { await recovery.sync() }
        for _ in 0..<100 {
            let publicationCount = URLProtocolStub.storage.requests(
                for: token,
                includingSchedulePublication: true
            ).count { $0.url.path.hasSuffix("/v1/schedule/publish") }
            if publicationCount >= 2 { break }
            try await Task.sleep(for: .milliseconds(5))
        }
        let external = try persistence.loadRevisioned()
        let externalSnapshot = try #require(external.snapshot)
        _ = try persistence.save(externalSnapshot, expectedRevision: external.revision)
        await run.value

        #expect(recovery.status.isFailure)
        #expect(planner.persistenceError == .concurrentModification)
        #expect(planner.pendingSchedulePublication == pending)
        #expect(try persistence.load()?.pendingSchedulePublication == pending)
        let publications = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
        #expect(publications.count == 2)
        #expect(URLProtocolStub.storage.requests(for: token).count == 2)
    }

    @Test("a cancelled post-send publication replays exact encrypted bytes after restart")
    func testPublicationJournalReplaysExactBytesAfterRestart() async throws {
        let token = "canonical-publication-restart-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-publication-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: PlannerEncryptionKey.random()
        )
        let planner = PlannerStore(
            persistence: persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"restart-cursor","has_more":false}"#.utf8)),
            .init(statusCode: 200, body: Data(Self.emptyPreviewObject(sourceRevisions: [:]).utf8))
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: Self.emptyInputDigest,
                    now: now,
                    replayed: false
                ),
                delay: 0.25
            )
        )
        let firstSync = Self.makeSync(planner: planner, token: token, now: now)
        let firstRun = Task { await firstSync.sync() }
        for _ in 0..<100 {
            if URLProtocolStub.storage.requests(
                for: token,
                includingSchedulePublication: true
            ).contains(where: { $0.url.path.hasSuffix("/v1/schedule/publish") }) {
                break
            }
            try await Task.sleep(for: .milliseconds(5))
        }
        firstSync.configurationDidChange()
        await firstRun.value
        #expect(firstSync.status.isFailure)
        #expect(firstSync.status.message.contains("recovery"))
        let pending = try #require(planner.pendingSchedulePublication)
        #expect(planner.persistenceError == nil)
        let pendingEncoder = JSONEncoder()
        pendingEncoder.dateEncodingStrategy = .millisecondsSince1970
        let pendingDecoder = JSONDecoder()
        pendingDecoder.dateDecodingStrategy = .millisecondsSince1970
        _ = try pendingDecoder.decode(
            PendingSchedulePublication.self,
            from: pendingEncoder.encode(pending)
        )
        let persistedBeforeRestart = try #require(try persistence.load())
        #expect(persistedBeforeRestart.pendingSchedulePublication == pending)
        let firstPublication = try #require(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).last(where: { $0.url.path.hasSuffix("/v1/schedule/publish") }))
        #expect(firstPublication.body == pending.preparedRequest.body)

        let replayNow = now.addingTimeInterval(2 * 86_400)
        let restored = PlannerStore(
            persistence: persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { replayNow }
        )
        #expect(restored.persistenceError == nil)
        #expect(restored.pendingSchedulePublication == pending)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"restart-fresh","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(Self.emptyPreviewObject(
                    sourceRevisions: [:],
                    asOf: replayNow
                ).utf8)
            )
        )
        URLProtocolStub.storage.enqueueSchedulePublication(
            key: token,
            .init(
                statusCode: 200,
                body: Self.publicationResponse(
                    inputDigest: Self.emptyInputDigest,
                    now: now,
                    replayed: true
                )
            )
        )
        let recoveredSync = Self.makeSync(planner: restored, token: token, now: replayNow)

        await recoveredSync.sync()

        let publications = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).filter { $0.url.path.hasSuffix("/v1/schedule/publish") }
        #expect(publications.count == 3)
        if publications.count == 3 {
            #expect(publications[0].body == publications[1].body)
            #expect(publications[2].body != publications[1].body)
        }
        #expect(restored.pendingSchedulePublication == nil)
        #expect(recoveredSync.lastPreview?.inputDigest == Self.emptyInputDigest)
        #expect(restored.canonicalPreviewFreshnessIssue == nil)
        if case .online = recoveredSync.status {} else {
            Issue.record("An exact replay should finish the local schedule commit")
        }
        let nonPublicationAfterRestart = URLProtocolStub.storage.requests(for: token)
            .filter { $0.url.path.hasSuffix("/v1/items/delta") }
        #expect(nonPublicationAfterRestart.count == 2)
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

    @Test("canonical cache cannot cross credentials at the same API origin")
    func testCanonicalCacheSameOriginCredentialBinding() async throws {
        let oldToken = "canonical-same-origin-old-token"
        let newToken = "canonical-same-origin-new-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let planner = PlannerStore(
            canonicalDeltaCursor: "old-credential-cursor",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: oldToken),
            restoreFromPersistence: false,
            now: { now }
        )
        URLProtocolStub.storage.reset(key: newToken)
        let sync = Self.makeSync(planner: planner, token: newToken, now: now)

        await sync.sync()

        #expect(sync.status.isFailure)
        #expect(planner.canonicalDeltaCursor == "old-credential-cursor")
        #expect(URLProtocolStub.storage.requests(for: newToken).isEmpty)
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
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.baseURLString),
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
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: Self.provenance(now: now, token: token),
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
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.baseURLString),
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
        let token = "canonical-prior-valid-binding-token"
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-29T08:00:00Z"))
        let itemID = UUID(uuidString: "27300000-2222-4333-8444-200000000000")!
        let item = try Self.decodeItem(Self.itemObject(id: itemID, revision: 1))
        let block = Self.block(itemID: itemID, revision: 1, start: now.addingTimeInterval(3_600))
        let planner = PlannerStore(
            blocks: [block], canonicalItems: [item], canonicalDeltaCursor: "actionable",
            canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
            schedulePreviewProvenance: Self.provenance(now: now, token: token),
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(planner.canMutate(block))
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: FixedAPIConfigurationStore(baseURL: Self.baseURLString),
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
                canonicalConfigurationIdentifier: Self.configurationIdentifier(token: token),
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
        isSensitive: Bool = false,
        splitPolicy: String = #"{"type":"indivisible"}"#
    ) -> String {
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        return """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":\(isSensitive),"kind":"task","status":"\(status)",
         "title":"Write launch plan","notes":"Private local notes","timezone_name":"Europe/Madrid",
         "duration_seconds":2700,"deadline_at":null,"earliest_start_at":null,"recurrence":null,
         "flexible_constraints":{"energy":"deep"},"split_policy":\(splitPolicy),
         "importance":50,"urgency":50,"parent_id":\(parent),"sibling_order":\(siblingOrder),"is_executable":true,
         "revision":\(revision),"created_at":"2026-08-29T08:00:00Z",
         "updated_at":"2026-08-29T08:00:00Z","completed_at":null,"deleted_at":null}
        """
    }

    private static func previewObject(
        itemID: UUID,
        blockID: UUID,
        itemIsSensitive: Bool = false
    ) -> String {
        let asOf = Date(timeIntervalSince1970: 1_787_990_400)
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: asOf)
        let horizonEnd = calendar.date(byAdding: .day, value: 7, to: horizonStart)
            ?? horizonStart.addingTimeInterval(7 * 86_400)
        let blockStart = asOf.addingTimeInterval(3_600)
        let blockEnd = blockStart.addingTimeInterval(2_700)
        return """
        {"input_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source_item_count":1,"accepted_item_count":1,
         "source_item_revisions":{"\(itemID.uuidString.lowercased())":1},
         "rejected_items":[],"ignored_previous_assignments":[],"plan":{
           "as_of":"\(wireTimestamp(asOf))","horizon_start":"\(wireTimestamp(horizonStart))",
           "horizon_end":"\(wireTimestamp(horizonEnd))","blocks":[{
             "id":"\(blockID.uuidString.lowercased())","is_sensitive":\(itemIsSensitive),"item_id":"\(itemID.uuidString.lowercased())",
             "occurrence_id":null,"external_block_id":null,"title":"Write launch plan",
             "start":"\(wireTimestamp(blockStart))","end":"\(wireTimestamp(blockEnd))",
             "session_index":0,"kind":"planned","explanations":[
               {"code":"earliest_available","message":"Placed in the earliest matching opening."}
             ]}],"unscheduled":[],"decisions":[],"violations":[],
           "score":{"scheduled_minutes":45,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},
           "occurrences":[]}}
        """
    }

    private static func emptyPreviewObject(
        sourceRevisions: [UUID: UInt64],
        asOf: Date = Date(timeIntervalSince1970: 1_787_990_400)
    ) -> String {
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: asOf)
        let horizonEnd = calendar.date(byAdding: .day, value: 7, to: horizonStart)
            ?? horizonStart.addingTimeInterval(7 * 86_400)
        let revisions = sourceRevisions
            .sorted { $0.key.uuidString < $1.key.uuidString }
            .map { "\"\($0.key.uuidString.lowercased())\":\($0.value)" }
            .joined(separator: ",")
        return """
        {"input_digest":"\(emptyInputDigest)","source_item_count":\(sourceRevisions.count),
         "accepted_item_count":\(sourceRevisions.count),"source_item_revisions":{\(revisions)},
         "rejected_items":[],"ignored_previous_assignments":[],"plan":{
           "as_of":"\(wireTimestamp(asOf))","horizon_start":"\(wireTimestamp(horizonStart))",
           "horizon_end":"\(wireTimestamp(horizonEnd))","blocks":[],"unscheduled":[],
           "decisions":[],"violations":[],"score":{"scheduled_minutes":0,
           "unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}
        """
    }

    private static let emptyInputDigest =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

    private static func publicationResponse(
        inputDigest: String,
        now: Date,
        replayed: Bool,
        publishedAt: Date? = nil
    ) -> Data {
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: now)
        let horizonEnd = calendar.date(byAdding: .day, value: 7, to: horizonStart)
            ?? horizonStart.addingTimeInterval(7 * 86_400)
        let timezoneName = TimeZone.autoupdatingCurrent.identifier == "GMT"
            ? "UTC"
            : TimeZone.autoupdatingCurrent.identifier
        let revisionID = "abababab-abab-4bab-8bab-abababababab"
        return Data("""
        {"revision":{"id":"\(revisionID)","revision":"1:\(revisionID)",
        "revision_number":1,"input_digest":"\(inputDigest)",
        "horizon_start":"\(wireTimestamp(horizonStart))",
        "horizon_end":"\(wireTimestamp(horizonEnd))","timezone_name":"\(timezoneName)",
        "published_at":"\(wireTimestamp(publishedAt ?? now))"},"replayed":\(replayed)}
        """.utf8)
    }

    private static let stalePublicationResponse = Data(
        #"{"error":{"code":"schedule_publication_stale","message":"Canonical items changed during publication; preview again"}}"#.utf8
    )

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
            configurationStore: FixedAPIConfigurationStore(baseURL: baseURLString),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            now: { now }
        )
    }

    private static let baseURLString = "https://api.example.com/gateway"

    private static func configurationIdentifier(token: String) -> String {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL(baseURLString),
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        ).configurationIdentifier
    }

    private static func provenance(now: Date, token: String) -> SchedulePreviewProvenance {
        let calendar = Calendar.autoupdatingCurrent
        let horizonStart = calendar.startOfDay(for: now)
        return .init(
            configurationIdentifier: configurationIdentifier(token: token),
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
