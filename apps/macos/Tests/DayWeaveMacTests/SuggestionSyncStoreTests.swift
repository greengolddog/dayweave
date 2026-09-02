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

    @Test("stored credentials are not verified until an authenticated request succeeds")
    func testVerifiedConfigurationRequiresSuccessfulRefresh() async throws {
        let token = "suggestion-verification-token"
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: DayWeaveAPIClientTests.listEnvelope())
        )
        let sync = SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: "https://api.example.com/gateway"
            ),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession()
        )
        let configured = try #require(sync.currentApplicationConfigurationIdentifier)

        #expect(sync.verifiedApplicationConfigurationIdentifier == nil)
        await sync.refresh()
        #expect(sync.verifiedApplicationConfigurationIdentifier == configured)

        #expect(sync.applyConfiguration(
            baseURL: "https://other.example.com/gateway",
            newToken: "replacement-verification-token"
        ))
        #expect(sync.verifiedApplicationConfigurationIdentifier == nil)
    }

    @Test("malformed decision responses never remove local review intent")
    func testDecisionResponseIdentityRevisionAndStatusAreValidated() async throws {
        let token = "suggestion-mutation-validation-token"
        let wrongID = UUID(uuidString: "99999999-2222-4333-8444-555555555555")!
        let validID = DayWeaveAPIClientTests.proposalID.uuidString.lowercased()
        let wrongIdentity = DayWeaveAPIClientTests
            .proposalObject(status: "accepted", revision: 5)
            .replacingOccurrences(of: validID, with: wrongID.uuidString.lowercased())
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, body: DayWeaveAPIClientTests.listEnvelope()),
            .init(statusCode: 200, body: Data("{\"suggestion\":\(wrongIdentity)}".utf8)),
            .init(
                statusCode: 200,
                body: DayWeaveAPIClientTests.proposalEnvelope(status: "accepted", revision: 4)
            ),
            .init(
                statusCode: 200,
                body: DayWeaveAPIClientTests.proposalEnvelope(status: "pending", revision: 5)
            )
        )
        let sync = SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: "https://api.example.com/gateway"
            ),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession()
        )
        await sync.refresh()
        let proposal = try #require(sync.proposals.first)

        for _ in 0..<3 {
            await sync.accept(proposal)
            #expect(sync.proposals.map(\.id) == [proposal.id])
            #expect(sync.status.isFailure)
        }
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

    @Test("saved credentials are never reused across API origins")
    func testOriginChangeRequiresExplicitToken() throws {
        let configuration = TestSuggestionConfigurationStore(baseURL: "https://old.example.com/api")
        let tokenStore = TestBearerTokenStore(
            token: "old-origin-secret",
            origin: "https://old.example.com"
        )
        let sync = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )

        #expect(!sync.applyConfiguration(
            baseURL: "https://new.example.com/api",
            newToken: ""
        ))
        #expect(configuration.loadBaseURL() == "https://old.example.com/api")
        #expect(tokenStore.loadToken() == "old-origin-secret")
        #expect(sync.status.isFailure)

        #expect(sync.applyConfiguration(
            baseURL: "https://old.example.com/other-path",
            newToken: ""
        ))
        #expect(configuration.loadBaseURL() == "https://old.example.com/other-path")
        #expect(tokenStore.loadToken() == "old-origin-secret")

        #expect(sync.applyConfiguration(
            baseURL: "https://new.example.com/api",
            newToken: "new-origin-secret"
        ))
        #expect(tokenStore.loadToken() == "new-origin-secret")
        let savedCredential = try #require(try tokenStore.loadCredential())
        #expect(savedCredential.origin == "https://new.example.com")
        let relaunched = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        #expect(relaunched.isConfigured)
    }

    @Test("a proposal retained by an edit sheet cannot cross API configurations")
    func testStaleProposalCannotMutateNewConfiguration() async throws {
        let oldToken = "stale-proposal-old-origin"
        let newToken = "stale-proposal-new-origin"
        let configuration = TestSuggestionConfigurationStore(
            baseURL: "https://old.example.com/api"
        )
        let tokenStore = TestBearerTokenStore(
            token: oldToken,
            origin: "https://old.example.com"
        )
        URLProtocolStub.storage.reset(key: oldToken)
        URLProtocolStub.storage.reset(key: newToken)
        URLProtocolStub.storage.enqueue(
            key: oldToken,
            .init(statusCode: 200, body: DayWeaveAPIClientTests.listEnvelope())
        )
        let sync = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        await sync.refresh()
        let staleProposal = try #require(sync.proposals.first)
        #expect(sync.applyConfiguration(
            baseURL: "https://new.example.com/api",
            newToken: newToken
        ))
        #expect(sync.proposals.isEmpty)

        let edited = await sync.edit(
            staleProposal,
            title: "Must not cross origins",
            explanation: staleProposal.explanation ?? "Still must not cross origins"
        )

        #expect(!edited)
        #expect(sync.status.isFailure)
        #expect(URLProtocolStub.storage.requests(for: oldToken).count == 1)
        #expect(URLProtocolStub.storage.requests(for: newToken).isEmpty)
        #expect(sync.activeProposalIDs.isEmpty)
    }

    @Test("an interrupted cross-origin update is fail-closed after relaunch for both clients")
    func testInterruptedOriginChangeCannotMixURLAndCredential() async throws {
        let oldToken = "interrupted-origin-old"
        let newToken = "interrupted-origin-new"
        let configuration = TestSuggestionConfigurationStore(
            baseURL: "https://old.example.com/api"
        )
        let tokenStore = TestBearerTokenStore(
            token: oldToken,
            origin: "https://old.example.com"
        )
        URLProtocolStub.storage.reset(key: oldToken)
        URLProtocolStub.storage.reset(key: newToken)

        // This is the only possible torn state: the atomic Keychain record was
        // replaced, then the process terminated before UserDefaults changed.
        tokenStore.saveCredential(.init(
            token: newToken,
            origin: "https://new.example.com"
        ))

        let relaunchedSuggestions = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        #expect(!relaunchedSuggestions.isConfigured)
        #expect(!relaunchedSuggestions.tokenConfigured)
        await relaunchedSuggestions.refresh()

        let planner = PlannerStore(restoreFromPersistence: false)
        let relaunchedCanonical = CanonicalSyncStore(
            planner: planner,
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        #expect(!relaunchedCanonical.isConfigured)
        await relaunchedCanonical.sync()

        #expect(URLProtocolStub.storage.requests(for: oldToken).isEmpty)
        #expect(URLProtocolStub.storage.requests(for: newToken).isEmpty)
        #expect(!planner.isCanonicalSyncLocked)
    }

    @Test("legacy raw credentials require re-entry and never reach either API client")
    func testLegacyCredentialFailsClosedUntilReentry() async throws {
        let legacyToken = "legacy-originless-secret"
        let configuration = TestSuggestionConfigurationStore(
            baseURL: "https://api.example.com/api"
        )
        let tokenStore = TestBearerTokenStore(legacyToken: legacyToken)
        URLProtocolStub.storage.reset(key: legacyToken)
        let suggestions = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )

        #expect(!suggestions.isConfigured)
        #expect(!suggestions.applyConfiguration(
            baseURL: "https://api.example.com/api",
            newToken: ""
        ))
        await suggestions.refresh()
        let planner = PlannerStore(restoreFromPersistence: false)
        let canonical = CanonicalSyncStore(
            planner: planner,
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        await canonical.sync()

        #expect(URLProtocolStub.storage.requests(for: legacyToken).isEmpty)
        #expect(suggestions.applyConfiguration(
            baseURL: "https://api.example.com/api",
            newToken: "replacement-origin-bound-secret"
        ))
        #expect(suggestions.isConfigured)
    }

    @Test("clearing credentials deletes the complete bound record")
    func testClearCredentialDeletesRecordAndSurvivesRelaunch() throws {
        let configuration = TestSuggestionConfigurationStore(
            baseURL: "https://api.example.com/api"
        )
        let tokenStore = TestBearerTokenStore(token: "clear-me")
        let suggestions = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )

        suggestions.clearBearerToken()

        #expect(try tokenStore.loadCredential() == nil)
        #expect(!suggestions.tokenConfigured)
        let relaunched = SuggestionSyncStore(
            configurationStore: configuration,
            tokenStore: tokenStore,
            session: URLProtocolStub.makeSession()
        )
        #expect(!relaunched.isConfigured)
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
