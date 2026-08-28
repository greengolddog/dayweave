import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Suggestion sync safety", .serialized)
@MainActor
struct SuggestionSyncStoreTests {
    init() {
        URLProtocolStub.storage.reset(key: Self.syncToken)
    }

    @Test("remote approval cannot mutate the planner schedule")
    func testRemoteApprovalCannotMutateThePlannerSchedule() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.syncToken,
            .init(statusCode: 200, body: DayWeaveAPIClientTests.listEnvelope()),
            .init(
                statusCode: 200,
                body: DayWeaveAPIClientTests.proposalEnvelope(status: "accepted", revision: 5)
            )
        )
        let planner = PlannerStore.preview(now: Date(timeIntervalSince1970: 1_700_000_000))
        let scheduleBeforeApproval = planner.blocks
        let sync = SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: "https://api.example.com/gateway"
            ),
            tokenStore: TestBearerTokenStore(token: Self.syncToken),
            session: URLProtocolStub.makeSession(),
            now: { Date(timeIntervalSince1970: 1_777_777_777) }
        )

        await sync.refresh()
        let proposal = try #require(sync.proposals.first)
        await sync.accept(proposal)

        #expect(planner.blocks == scheduleBeforeApproval)
        #expect(sync.proposals.isEmpty)
        guard case let .online(updatedAt, message) = sync.status else {
            Issue.record("Expected an online status")
            return
        }
        #expect(updatedAt == Date(timeIntervalSince1970: 1_777_777_777))
        #expect(message.contains("no schedule changes"))
    }

    @Test("remote failure leaves local suggestion handling available")
    func testRefreshFailureLeavesLocalSuggestionsAvailable() async throws {
        URLProtocolStub.storage.enqueue(key: Self.syncToken, .init(
            statusCode: 503,
            body: Data(#"{"error":{"code":"service_unavailable","message":"try later"}}"#.utf8)
        ))
        let planner = PlannerStore.preview(now: Date(timeIntervalSince1970: 1_700_000_000))
        let localSuggestion = try #require(planner.suggestions.first)
        let sync = SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: "https://api.example.com"
            ),
            tokenStore: TestBearerTokenStore(token: Self.syncToken),
            session: URLProtocolStub.makeSession()
        )

        await sync.refresh()
        planner.acceptSuggestion(localSuggestion.id)

        #expect(sync.status.isFailure)
        #expect(planner.suggestions.first?.state == .accepted)
    }

    @Test("configuration separates URL persistence from credential storage")
    func testApplyingConfigurationSavesOnlyURLInDefaultsStoreAndTokenInCredentialStore() {
        let configuration = TestSuggestionConfigurationStore(baseURL: nil)
        let tokenStore = TestBearerTokenStore(token: nil)
        let sync = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )

        sync.applyConfiguration(
            baseURL: "https://api.example.com/",
            newToken: "new-secret-token"
        )

        #expect(configuration.loadBaseURL() == "https://api.example.com/")
        #expect(tokenStore.loadToken() == "new-secret-token")
        #expect(!(configuration.loadBaseURL()?.contains("new-secret-token") ?? true))
        #expect(sync.status == .ready)
    }

    @Test("bearer token never enters the Codable planner snapshot")
    func testBearerTokenIsAbsentFromPlannerSnapshot() throws {
        let credentialCanary = "BEARER-CANARY-THAT-MUST-STAY-IN-KEYCHAIN"
        let configuration = TestSuggestionConfigurationStore(baseURL: nil)
        let tokenStore = TestBearerTokenStore(token: nil)
        let sync = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        #expect(sync.applyConfiguration(
            baseURL: "https://api.example.com",
            newToken: credentialCanary
        ))

        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeaveCredentialSnapshotTests-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: try PlannerEncryptionKey(data: Data(repeating: 3, count: 32))
        )
        let planner = PlannerStore(
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )
        planner.quickAdd(title: "Safe local task", kind: .task, minutes: 20)
        planner.flushPersistence()

        let loadedSnapshot = try persistence.load()
        let snapshot = try #require(loadedSnapshot)
        let encodedSnapshot = try JSONEncoder().encode(snapshot)
        #expect(encodedSnapshot.range(of: Data(credentialCanary.utf8)) == nil)
        #expect(tokenStore.loadToken() == credentialCanary)
    }

    @Test("production-style startup is empty when no snapshot exists")
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

        #expect(store.blocks.isEmpty)
        #expect(store.suggestions.isEmpty)
        #expect(store.assistantMessages.isEmpty)
        #expect(store.selectedBlockID == nil)
        #expect(store.lastScheduleMessage == "No schedule yet — add an item when you’re ready")
    }

    @Test("failed restore gates mutations and preserves the unreadable snapshot")
    func testFailedRestoreGatesMutations() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeaveRestoreGateTests-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let unreadableSnapshot = Data("not-an-encrypted-envelope".utf8)
        try unreadableSnapshot.write(to: fileURL)
        let key = try PlannerEncryptionKey(data: Data(repeating: 9, count: 32))
        let persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: key)
        let store = PlannerStore.live(persistence: persistence)

        store.quickAdd(title: "Must not overwrite recovery data", kind: .task, minutes: 30)
        store.flushPersistence()

        #expect(store.loadState == .persistenceFailed)
        #expect(!store.canMutatePlan)
        #expect(store.blocks.isEmpty)
        #expect(try Data(contentsOf: fileURL) == unreadableSnapshot)
    }

    private static let syncToken = "sync-secret-token"
}
#endif

final class TestSuggestionConfigurationStore: SuggestionAPIConfigurationStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var baseURL: String?

    init(baseURL: String?) {
        self.baseURL = baseURL
    }

    func loadBaseURL() -> String? {
        lock.withLock { baseURL }
    }

    func saveBaseURL(_ value: String) {
        lock.withLock { baseURL = value }
    }
}
