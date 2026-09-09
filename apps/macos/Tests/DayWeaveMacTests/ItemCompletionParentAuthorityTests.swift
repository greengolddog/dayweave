import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Completion-qualified parent authoring", .serialized)
@MainActor
struct ItemCompletionParentAuthorityTests {
    private static let parentID = ItemCompletionTestFixtures.itemID
    private static let childID = UUID(uuidString: "00000000-0000-0000-0000-000000000002")!
    private static let now = ItemCompletionTestFixtures.date
    private static let headers = ["Content-Type": "application/json", "Cache-Control": "no-store, max-age=0", "Pragma": "no-cache"]

    @Test("Completed is not permission; current GET enables parent capture without content replacement")
    func pickerNeedsCurrentRead() throws {
        let f = try Fixture(); defer { f.remove() }
        let cache = CanonicalHierarchyAuthoringCache()
        #expect(!cache.eligibleParentIDs(for: f.planner).contains(Self.parentID))
        #expect(CanonicalHierarchyAuthoring.route(kind: .task, parentID: Self.parentID, store: f.planner) == nil)
        try f.admit()
        #expect(cache.eligibleParentIDs(for: f.planner).contains(Self.parentID))
        #expect(!f.planner.canonicalItems[0].supportsCanonicalAuthoringReplacement)
        #expect(CanonicalHierarchyAuthoring.route(kind: .task, parentID: Self.parentID, store: f.planner) != nil)
        f.planner.invalidateItemCompletionReadEvidence()
        #expect(!cache.eligibleParentIDs(for: f.planner).contains(Self.parentID))
        let restarted = f.restart()
        #expect(!restarted.itemCompletionQualifiesParent(Self.parentID))
    }

    @Test("one scoped proof tolerates only its exact queued child and expires with evidence")
    func queuedChildScopedProof() throws {
        let f = try Fixture(); defer { f.remove() }
        try f.admit()
        let queued = try f.queue()
        #expect(!f.planner.itemCompletionQualifiesParent(Self.parentID))
        #expect(f.planner.beginCanonicalSync())
        defer { f.planner.endCanonicalSync() }
        let bound = try f.planner.bindCanonicalAuthoringMutation(queued.id, configurationIdentifier: f.binding)
        let admission = f.proof(bound)
        #expect(f.planner.itemCompletionQualifiesParent(Self.parentID, admission: admission))
        #expect(f.planner.canonicalAuthoringDraftHierarchyIsCurrent(try #require(bound.draft), itemID: Self.childID,
            requiresCommittedParent: true, completionParentAdmission: admission))
        #expect(!f.planner.canonicalAuthoringDraftHierarchyIsCurrent(try #require(bound.draft), itemID: UUID(),
            requiresCommittedParent: true, completionParentAdmission: admission))
        #expect(!f.planner.canonicalAuthoringDraftHierarchyIsCurrent(try #require(bound.draft), itemID: Self.childID,
            requiresCommittedParent: false, completionParentAdmission: admission))
        let foreign = ItemCompletionParentAdmission(mutation: bound, snapshot: Self.snapshot(),
            configurationIdentifier: "foreign-synthetic-binding", evidenceGeneration: f.planner.itemCompletionEvidenceGeneration)
        #expect(!f.planner.itemCompletionQualifiesParent(Self.parentID, admission: foreign))
        #expect(!f.planner.itemCompletionQualifiesParent(Self.parentID))
        f.planner.invalidateItemCompletionReadEvidence()
        #expect(!f.planner.itemCompletionQualifiesParent(Self.parentID, admission: admission))
    }

    @Test("scoped child exception does not waive another pending authoring intent")
    func unrelatedIntentStillBlocks() throws {
        let f = try Fixture(); defer { f.remove() }
        try f.admit()
        let queued = try f.queue()
        _ = try f.planner.enqueueCanonicalCreate(draft: .init(title: "Separate synthetic intent", timezoneName: "UTC"))
        #expect(f.planner.beginCanonicalSync())
        defer { f.planner.endCanonicalSync() }
        let bound = try f.planner.bindCanonicalAuthoringMutation(queued.id, configurationIdentifier: f.binding)
        #expect(!f.planner.itemCompletionQualifiesParent(Self.parentID, admission: f.proof(bound)))
    }

    @Test("provenance cannot waive unavailable ancestry")
    func missingAncestorRemainsUnavailable() throws {
        let f = try Fixture(parentParentID: UUID()); defer { f.remove() }
        try f.admit()
        #expect(!f.planner.canonicalAuthoringEligibleParentIDs().contains(Self.parentID))
        #expect(!f.planner.canonicalAuthoringDraftHierarchyIsCurrent(Fixture.draft,
            itemID: Self.childID, requiresCommittedParent: false))
    }

    @Test("first send refreshes parent after binding; restart replays exact child without another GET")
    func actualSendAndSubmittedReplay() async throws {
        let f = try Fixture(); defer { f.remove() }
        try f.admit()
        let queued = try f.queue()
        f.enqueueDelta()
        try f.enqueueCompletion(Self.snapshot())
        f.enqueueFailure()
        await f.sync(f.planner).sync()
        let submitted = try #require(f.planner.canonicalAuthoringMutation(id: queued.id))
        #expect(submitted.hasBeenSubmitted && submitted.disposition == .pending)
        #expect(!f.planner.itemCompletionQualifiesParent(Self.parentID))
        #expect(f.planner.itemCompletionState.observations.first?.isReadProof == false)
        let requests = URLProtocolStub.storage.requests(for: f.token)
        #expect(requests.map(\.method) == ["GET", "GET", "POST"])
        #expect(requests[1].url.path.hasSuffix("/completion"))
        #expect(requests[2].jsonBody?["parent_id"] as? String == Self.parentID.uuidString)
        #expect(requests[2].headers["Idempotency-Key"] == queued.idempotencyKey)
        let restarted = f.restart()
        #expect(restarted.pendingCanonicalAuthoringMutations == [submitted])
        f.enqueueDelta()
        f.enqueueFailure()
        await f.sync(restarted).sync()
        let replayed = URLProtocolStub.storage.requests(for: f.token)
        #expect(replayed.map(\.method) == ["GET", "GET", "POST", "GET", "POST"])
        #expect(replayed[4].body == requests[2].body)
        #expect(replayed[4].headers["Idempotency-Key"] == requests[2].headers["Idempotency-Key"])
        #expect(restarted.pendingCanonicalAuthoringMutations == [submitted])
    }

    @Test("transient GET failure retains never-submitted child without a conflict")
    func parentReadFailureRetainsIntent() async throws {
        let f = try Fixture(); defer { f.remove() }
        try f.admit()
        let queued = try f.queue()
        f.enqueueDelta()
        f.enqueueFailure()
        await f.sync(f.planner).sync()
        let retained = try #require(f.planner.canonicalAuthoringMutation(id: queued.id))
        #expect(retained.disposition == .pending && !retained.hasBeenSubmitted)
        #expect(retained.idempotencyKey == queued.idempotencyKey && retained.draft == queued.draft)
        #expect(URLProtocolStub.storage.requests(for: f.token).map(\.method) == ["GET", "GET"])
        #expect(f.restart().pendingCanonicalAuthoringMutations == [retained])
    }

    @Test("unproven recurrence, missing provenance and revision drift never permit the child POST")
    func rejectedParentEvidence() async throws {
        for response in [Self.snapshot(itemRevision: 8), Self.snapshot(occurrenceRequired: true),
                         Self.snapshot(hasProvenance: false)] {
            let f = try Fixture(); defer { f.remove() }
            try f.admit()
            let queued = try f.queue()
            f.enqueueDelta()
            try f.enqueueCompletion(response)
            await f.sync(f.planner).sync()
            let retained = try #require(f.planner.canonicalAuthoringMutation(id: queued.id))
            #expect(retained.disposition == .pending && !retained.hasBeenSubmitted)
            #expect(URLProtocolStub.storage.requests(for: f.token).map(\.method) == ["GET", "GET"])
        }
    }

    private static func snapshot(itemRevision: UInt64 = 7, occurrenceRequired: Bool = false,
                                 hasProvenance: Bool = true) -> ItemCompletionSnapshot {
        .init(itemID: parentID, itemRevision: itemRevision,
            state: .init(itemID: parentID, revision: 2, mode: .automatic,
                provenance: hasProvenance ? .init(kind: .automatic, reopen: .init(status: .planned)) : nil,
                updatedAt: "2026-09-09T10:00:00Z"), evidenceHash: ItemCompletionTestFixtures.hash,
            occurrenceEvidenceRequired: occurrenceRequired)
    }

    @MainActor
    private struct Fixture {
        static let draft = DayWeaveCanonicalItemDraft(title: "Synthetic child", timezoneName: "UTC", parentID: parentID)
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        let token: String
        let binding: String
        init(parentParentID: UUID? = nil) throws {
            token = "synthetic-parent-\(UUID())"
            URLProtocolStub.storage.reset(key: token)
            let client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL("https://api.example.com/gateway"),
                session: URLProtocolStub.makeSession(), bearerToken: token)
            binding = client.configurationIdentifier
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveCompletionParent-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            persistence = EncryptedPlannerPersistence(fileURL: directory.appendingPathComponent("synthetic.encrypted"),
                key: try PlannerEncryptionKey(data: Data(repeating: 53, count: 32)))
            let parent = parentParentID.map { "\"\($0.uuidString)\"" } ?? "null"
            let json = """
            {"id":"\(parentID.uuidString)","is_sensitive":true,"kind":"task","status":"completed",
            "title":"Synthetic managed parent","notes":null,"timezone_name":"UTC","duration_seconds":1800,
            "deadline_at":null,"earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
            "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,"parent_id":\(parent),
            "sibling_order":0,"is_executable":true,"revision":7,"created_at":"2026-09-09T10:00:00Z",
            "updated_at":"2026-09-09T10:00:00Z","completed_at":"2026-09-09T10:00:00Z","deleted_at":null}
            """
            let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
            let item = try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
            planner = PlannerStore(canonicalItems: [item], canonicalDeltaCursor: "synthetic-parent-cursor",
                canonicalConfigurationIdentifier: binding, persistence: persistence, restoreFromPersistence: false,
                now: { ItemCompletionTestFixtures.date })
            planner.flushPersistence()
            #expect(planner.canPersistPlan)
        }
        func admit() throws {
            var next = planner.itemCompletionState
            next.configurationIdentifier = binding
            try next.observe(snapshot(), at: now)
            try planner.commitItemCompletionState(next, replacing: planner.itemCompletionState)
            try planner.admitItemCompletionRead(snapshot(), generation: planner.itemCompletionEvidenceGeneration)
        }
        func queue() throws -> DayWeavePendingCanonicalAuthoringMutation {
            try planner.enqueueCanonicalCreate(itemID: childID, draft: Self.draft)
        }
        func proof(_ mutation: DayWeavePendingCanonicalAuthoringMutation) -> ItemCompletionParentAdmission {
            .init(mutation: mutation, snapshot: snapshot(), configurationIdentifier: binding,
                evidenceGeneration: planner.itemCompletionEvidenceGeneration)
        }
        func restart() -> PlannerStore { PlannerStore(persistence: persistence, now: { ItemCompletionTestFixtures.date }) }
        func sync(_ store: PlannerStore) -> CanonicalSyncStore {
            CanonicalSyncStore(planner: store,
                configurationStore: CompletionParentAPIConfiguration(),
                tokenStore: TestBearerTokenStore(token: token), session: URLProtocolStub.makeSession(),
                now: { ItemCompletionTestFixtures.date })
        }
        func enqueueDelta() {
            URLProtocolStub.storage.enqueue(key: token, .init(statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"synthetic-parent-cursor","has_more":false}"#.utf8)))
        }
        func enqueueCompletion(_ value: ItemCompletionSnapshot) throws {
            URLProtocolStub.storage.enqueue(key: token,
                .init(statusCode: 200, headers: headers, body: try JSONEncoder().encode(value)))
        }
        func enqueueFailure() {
            URLProtocolStub.storage.enqueue(key: token, .init(statusCode: 503, headers: headers,
                body: Data(#"{"error":{"code":"service_unavailable","message":"Synthetic unavailable"}}"#.utf8)))
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}

private struct CompletionParentAPIConfiguration: SuggestionAPIConfigurationStoring {
    func loadBaseURL() -> String? { "https://api.example.com/gateway" }
    func saveBaseURL(_ value: String) {}
}
#endif
