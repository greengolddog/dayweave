import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Transactional proposal application workflow", .serialized)
@MainActor
struct ProposalApplicationStoreTests {
    init() {
        URLProtocolStub.storage.reset(key: Self.testBearer)
    }

    @Test("review and explicit approval persist exact intent before atomic apply")
    func testReviewAndApply() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: true)),
            .init(statusCode: 200, body: ProposalApplicationAPIClientTests.applyResponseBody())
        )
        let (suggestions, journal, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)

        await applications.prepareReview(for: proposal)
        let review = try #require(applications.preview(for: proposal))
        #expect(review.canApply)

        let applied = await applications.apply(
            proposal,
            approval: applications.approval(for: proposal)
        )

        #expect(applied)
        #expect(journal.pendingProposalApplicationMutation == nil)
        let stored = try #require(journal.proposalApplicationReceipts.first)
        #expect(stored.application.status == .applied)
        #expect(suggestions.proposals.isEmpty)
        let persisted = try #require(journal.persistedMutations.first)
        let applyRequest = try #require(URLProtocolStub.storage.requests(for: Self.testBearer).last)
        #expect(applyRequest.body == persisted.requestBody)
        #expect(applyRequest.headers["Idempotency-Key"] == persisted.idempotencyKey)
        #expect(persisted.expectedReviewHash == review.reviewHash)
        #expect(persisted.expectedCommandIDs == review.commandIDs)
    }

    @Test("apply always requires an explicit local confirmation")
    func testApplyRequiresConfirmation() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: true))
        )
        let (suggestions, journal, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)

        #expect(!(await applications.apply(proposal, approval: nil)))
        #expect(journal.pendingProposalApplicationMutation == nil)
        #expect(journal.proposalApplicationReceipts.isEmpty)
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).count == 2)
    }

    @Test("sensitive before and after titles stay concealed until explicit reveal")
    func testSensitiveSnapshotTitlesRequireReveal() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(
                statusCode: 201,
                body: ProposalApplicationAPIClientTests.previewBody(canApply: true)
            )
        )
        let (suggestions, _, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)
        var item = try #require(applications.preview(for: proposal)?.diffs.first?.after)
        item.isSensitive = true
        item.title = "Confidential hierarchy goal"

        #expect(
            proposalItemSnapshotTitle(item, hidesSensitiveContent: true)
                == "Sensitive item"
        )
        #expect(
            proposalItemSnapshotTitle(item, hidesSensitiveContent: false)
                == "Confidential hierarchy goal"
        )
    }

    @Test("approval is bound to one exact preview hash across windows")
    func testApprovalCannotCrossFreshPreview() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: true)),
            .init(statusCode: 201, body: Self.alternatePreviewBody())
        )
        let (suggestions, journal, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)
        let oldApproval = try #require(applications.approval(for: proposal))

        await applications.prepareReview(for: proposal)
        let freshApproval = try #require(applications.approval(for: proposal))
        #expect(oldApproval != freshApproval)
        #expect(!(await applications.apply(proposal, approval: oldApproval)))
        #expect(journal.pendingProposalApplicationMutation == nil)
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).count == 3)
    }

    @Test("configuration changes immediately erase review content")
    func testConfigurationChangeErasesPreview() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: true))
        )
        let (suggestions, _, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)
        #expect(applications.preview(for: proposal) != nil)

        #expect(suggestions.applyConfiguration(
            baseURL: "https://api.example.com/gateway",
            newToken: ""
        ))
        #expect(applications.preview(for: proposal) == nil)
        #expect(applications.approval(for: proposal) == nil)
    }

    @Test("a blocked simulation remains reviewable but cannot reach apply transport")
    func testBlockedPreviewCannotApply() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: false))
        )
        let (suggestions, journal, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)

        let review = try #require(applications.preview(for: proposal))
        #expect(!review.canApply)
        #expect(review.conflicts.count == 1)
        #expect(!(await applications.apply(
            proposal,
            approval: applications.approval(for: proposal)
        )))
        #expect(journal.pendingProposalApplicationMutation == nil)
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).count == 2)
    }

    @Test("a malformed apply response recovers by proposal without duplicating the mutation")
    func testAmbiguousApplyRecoversByProposal() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: true)),
            .init(statusCode: 200, body: Data("{\"application\":".utf8)),
            .init(
                statusCode: 200,
                body: ProposalApplicationAPIClientTests.receiptBody(status: "applied", revision: 1)
            )
        )
        let (suggestions, journal, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)

        #expect(await applications.apply(
            proposal,
            approval: applications.approval(for: proposal)
        ))
        #expect(journal.pendingProposalApplicationMutation == nil)
        #expect(journal.proposalApplicationReceipts.first?.application.status == .applied)
        let requests = URLProtocolStub.storage.requests(for: Self.testBearer)
        #expect(requests.map(\.method) == ["GET", "POST", "POST", "GET"])
        #expect(requests.last?.url.path.hasSuffix(
            "/suggestions/\(Self.proposalID.uuidString.lowercased())/application"
        ) == true)
        #expect(journal.persistedMutations.count == 1)
    }

    @Test("undo persists a second exact intent and advances the retained receipt")
    func testUndo() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope()),
            .init(statusCode: 201, body: ProposalApplicationAPIClientTests.previewBody(canApply: true)),
            .init(statusCode: 200, body: ProposalApplicationAPIClientTests.applyResponseBody()),
            .init(statusCode: 200, body: ProposalApplicationAPIClientTests.undoResponseBody())
        )
        let (suggestions, journal, applications) = Self.makeWorkflow()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)
        await applications.prepareReview(for: proposal)
        #expect(await applications.apply(
            proposal,
            approval: applications.approval(for: proposal)
        ))
        let applied = try #require(journal.proposalApplicationReceipts.first)

        #expect(await applications.undo(applied))

        #expect(journal.pendingProposalApplicationMutation == nil)
        #expect(journal.proposalApplicationReceipts.count == 1)
        #expect(journal.proposalApplicationReceipts[0].application.status == .undone)
        #expect(journal.proposalApplicationReceipts[0].application.applicationRevision == 2)
        let undoMutation = try #require(journal.persistedMutations.last)
        #expect(undoMutation.operation == .undo)
        let undoRequest = try #require(URLProtocolStub.storage.requests(for: Self.testBearer).last)
        #expect(undoRequest.body == undoMutation.requestBody)
        #expect(undoRequest.headers["Idempotency-Key"] == undoMutation.idempotencyKey)
    }

    @Test("relaunch recovery replays the original apply body and retry key")
    func testRelaunchRecoveryUsesExactIntent() async throws {
        let client = Self.applicationClient()
        let requestBody = try client.prepareSuggestionApplicationApplyBody(
            expectedReviewHash: ProposalApplicationAPIClientTests.reviewHash
        )
        let mutation = DayWeavePendingProposalApplicationMutation.apply(
            configurationIdentifier: client.configurationIdentifier,
            proposalIDs: [Self.proposalID],
            proposalRevisions: [4],
            expectedCommandIDs: [ProposalApplicationAPIClientTests.commandID],
            previewID: ProposalApplicationAPIClientTests.previewID,
            expectedReviewHash: ProposalApplicationAPIClientTests.reviewHash,
            requestBody: requestBody,
            idempotencyKey: "macos-apply-relaunch-test",
            createdAt: Self.now
        )
        let journal = TestProposalApplicationJournal(pending: mutation)
        let suggestions = Self.makeSuggestions()
        let applications = ProposalApplicationStore(
            suggestions: suggestions,
            journal: journal,
            now: { Self.now }
        )
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(
                statusCode: 404,
                body: Data(#"{"error":{"code":"not_found","message":"not found"}}"#.utf8)
            ),
            .init(statusCode: 200, body: ProposalApplicationAPIClientTests.applyResponseBody())
        )

        #expect(await applications.recoverPendingMutation())

        #expect(journal.pendingProposalApplicationMutation == nil)
        let requests = URLProtocolStub.storage.requests(for: Self.testBearer)
        #expect(requests.count == 2)
        #expect(requests[1].body == requestBody)
        #expect(requests[1].headers["Idempotency-Key"] == mutation.idempotencyKey)
    }

    @Test("generic gateway errors never clear an ambiguous apply journal")
    func testGenericErrorsRetainApplyJournal() async throws {
        let mutation = try Self.pendingApplyMutation(idempotencyKey: "macos-generic-error-retain")
        let journal = TestProposalApplicationJournal(pending: mutation)
        let applications = ProposalApplicationStore(
            suggestions: Self.makeSuggestions(),
            journal: journal,
            now: { Self.now }
        )
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 404, body: Self.trustedNotFoundBody),
            .init(statusCode: 409, body: Self.trustedNoEffectBody)
        )

        #expect(!(await applications.recoverPendingMutation()))
        #expect(journal.pendingProposalApplicationMutation == mutation)
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).count == 2)
    }

    @Test("only strict endpoint-bound no-effect evidence clears an apply journal")
    func testTrustedNoEffectClearsApplyJournal() async throws {
        let mutation = try Self.pendingApplyMutation(idempotencyKey: "macos-trusted-error-clear")
        let journal = TestProposalApplicationJournal(pending: mutation)
        let applications = ProposalApplicationStore(
            suggestions: Self.makeSuggestions(),
            journal: journal,
            now: { Self.now }
        )
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(
                statusCode: 404,
                headers: Self.trustedErrorHeaders,
                body: Self.trustedNotFoundBody
            ),
            .init(
                statusCode: 409,
                headers: Self.trustedErrorHeaders,
                body: Self.trustedNoEffectBody
            )
        )

        #expect(!(await applications.recoverPendingMutation()))
        #expect(journal.pendingProposalApplicationMutation == nil)
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).count == 2)
    }

    @Test("protected typed schemas never fall back to legacy acceptance")
    func testProtectedSchemasNeverUseLegacyAccept() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 200, body: Self.typedListEnvelope())
        )
        let suggestions = Self.makeSuggestions()
        await suggestions.refresh()
        let proposal = try #require(suggestions.proposals.first)

        await suggestions.accept(proposal)

        #expect(suggestions.status.isFailure)
        #expect(suggestions.proposals.map(\.id) == [proposal.id])
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).count == 1)
    }

    static let testBearer = "proposal-application-store-test"
    static let proposalID = ProposalApplicationAPIClientTests.proposalID
    nonisolated static let now = Date(timeIntervalSince1970: 1_788_076_800)
    static let alternatePreviewID = UUID(
        uuidString: "22222222-2222-4222-8222-222222222223"
    )!
    static let alternateReviewHash = "sha256:" + String(repeating: "b", count: 64)
    static let trustedErrorHeaders = [
        "Content-Type": "application/json; charset=utf-8",
        "Cache-Control": "no-store, max-age=0",
        "Pragma": "no-cache",
    ]
    static let trustedNotFoundBody = Data(
        #"{"error":{"code":"not_found","message":"proposal application was not found"}}"#.utf8
    )
    static let trustedNoEffectBody = Data(
        #"{"error":{"code":"conflict","message":"Proposal application is stale or unsafe","details":{"conflict_code":"preview_expired"}}}"#.utf8
    )

    static func makeWorkflow() -> (
        SuggestionSyncStore,
        TestProposalApplicationJournal,
        ProposalApplicationStore
    ) {
        let suggestions = makeSuggestions()
        let journal = TestProposalApplicationJournal()
        let applications = ProposalApplicationStore(
            suggestions: suggestions,
            journal: journal,
            now: { now }
        )
        return (suggestions, journal, applications)
    }

    static func makeSuggestions() -> SuggestionSyncStore {
        SuggestionSyncStore(
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: "https://api.example.com/gateway"
            ),
            tokenStore: TestBearerTokenStore(token: testBearer),
            session: URLProtocolStub.makeSession(),
            now: { now }
        )
    }

    static func applicationClient() -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: testBearer
        )
    }

    static func alternatePreviewBody() -> Data {
        let original = String(
            decoding: ProposalApplicationAPIClientTests.previewBody(canApply: true),
            as: UTF8.self
        )
        return Data(original
            .replacingOccurrences(
                of: ProposalApplicationAPIClientTests.previewID.uuidString.lowercased(),
                with: alternatePreviewID.uuidString.lowercased()
            )
            .replacingOccurrences(
                of: ProposalApplicationAPIClientTests.reviewHash,
                with: alternateReviewHash
            )
            .utf8)
    }

    static func pendingApplyMutation(
        idempotencyKey: String
    ) throws -> DayWeavePendingProposalApplicationMutation {
        let client = applicationClient()
        return .apply(
            configurationIdentifier: client.configurationIdentifier,
            proposalIDs: [proposalID],
            proposalRevisions: [4],
            expectedCommandIDs: [ProposalApplicationAPIClientTests.commandID],
            previewID: ProposalApplicationAPIClientTests.previewID,
            expectedReviewHash: ProposalApplicationAPIClientTests.reviewHash,
            requestBody: try client.prepareSuggestionApplicationApplyBody(
                expectedReviewHash: ProposalApplicationAPIClientTests.reviewHash
            ),
            idempotencyKey: idempotencyKey,
            createdAt: now
        )
    }

    static func typedListEnvelope(
        schema: String = dayWeaveProposalChangeSetSchemaV1
    ) -> Data {
        Data("""
        {"suggestions":[{
          "id":"\(proposalID.uuidString.lowercased())",
          "revision":4,
          "submitted_by":"assistant-subject",
          "source":"codex",
          "source_reference":"conversation-fixture",
          "kind":"create_item",
          "status":"pending",
          "title":"Create a focused review task",
          "explanation":"The device must review this exact typed change.",
          "payload":{"schema":"\(schema)","commands":[]},
          "decision_note":null,
          "created_at":"2026-08-29T09:00:00Z",
          "updated_at":"2026-08-29T09:00:00Z",
          "expires_at":"2026-09-05T09:00:00Z",
          "decided_at":null
        }]}
        """.utf8)
    }
}

@MainActor
final class TestProposalApplicationJournal: ProposalApplicationJournaling {
    var pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation?
    var proposalApplicationReceipts: [DayWeaveStoredProposalApplicationReceipt]
    private(set) var persistedMutations: [DayWeavePendingProposalApplicationMutation] = []

    init(
        pending: DayWeavePendingProposalApplicationMutation? = nil,
        receipts: [DayWeaveStoredProposalApplicationReceipt] = []
    ) {
        pendingProposalApplicationMutation = pending
        proposalApplicationReceipts = receipts
    }

    func persistPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation
    ) throws {
        guard pendingProposalApplicationMutation == nil, mutation.hasValidShape else {
            throw TestProposalApplicationJournalError.invalidState
        }
        pendingProposalApplicationMutation = mutation
        persistedMutations.append(mutation)
    }

    func commitPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation,
        receipt: DayWeaveStoredProposalApplicationReceipt
    ) throws {
        guard pendingProposalApplicationMutation == mutation, receipt.hasValidShape else {
            throw TestProposalApplicationJournalError.invalidState
        }
        pendingProposalApplicationMutation = nil
        if let index = proposalApplicationReceipts.firstIndex(where: { $0.id == receipt.id }) {
            proposalApplicationReceipts[index] = receipt
        } else {
            proposalApplicationReceipts.insert(receipt, at: 0)
        }
    }

    func clearPendingProposalApplicationMutation(
        _ mutation: DayWeavePendingProposalApplicationMutation
    ) throws {
        guard pendingProposalApplicationMutation == mutation else {
            throw TestProposalApplicationJournalError.invalidState
        }
        pendingProposalApplicationMutation = nil
    }

    func recordProposalApplicationReceipt(
        _ receipt: DayWeaveStoredProposalApplicationReceipt
    ) throws {
        guard receipt.hasValidShape else {
            throw TestProposalApplicationJournalError.invalidState
        }
        if let index = proposalApplicationReceipts.firstIndex(where: { $0.id == receipt.id }) {
            proposalApplicationReceipts[index] = receipt
        } else {
            proposalApplicationReceipts.insert(receipt, at: 0)
        }
    }

    func proposalApplicationReceipt(
        applicationID: UUID,
        configurationIdentifier: String
    ) -> DayWeaveStoredProposalApplicationReceipt? {
        proposalApplicationReceipts.first {
            $0.configurationIdentifier == configurationIdentifier
                && $0.application.applicationID == applicationID
        }
    }
}

private enum TestProposalApplicationJournalError: Error {
    case invalidState
}
#endif
