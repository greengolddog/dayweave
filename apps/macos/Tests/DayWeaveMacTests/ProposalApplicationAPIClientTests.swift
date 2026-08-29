import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Transactional proposal application API", .serialized)
@MainActor
struct ProposalApplicationAPIClientTests {
    init() {
        URLProtocolStub.storage.reset(key: Self.testBearer)
    }

    @Test("preview, exact apply, receipt lookup, and exact undo use the protected routes")
    func testCompleteTransportContract() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.testBearer,
            .init(statusCode: 201, body: Self.previewBody(canApply: true)),
            .init(statusCode: 200, body: Self.applyResponseBody()),
            .init(statusCode: 200, body: Self.receiptBody(status: "applied", revision: 1)),
            .init(statusCode: 200, body: Self.undoResponseBody())
        )
        let client = Self.client()
        let preview = try await client.previewSuggestionApplication(.init(proposals: [
            .init(proposalID: Self.proposalID, expectedRevision: 4),
        ]))
        #expect(preview.hasSupportedContract)
        #expect(preview.canApply)

        let applyBody = try client.prepareSuggestionApplicationApplyBody(
            expectedReviewHash: Self.reviewHash
        )
        let applied = try await client.applySuggestionApplication(
            previewID: Self.previewID,
            expectedReviewHash: Self.reviewHash,
            requestBody: applyBody,
            idempotencyKey: "macos-apply-transport-test"
        )
        #expect(applied.application.status == .applied)

        let fetched = try await client.suggestionApplication(applicationID: Self.applicationID)
        #expect(fetched == applied.application)

        let undoBody = try client.prepareSuggestionApplicationUndoBody(
            expectedApplicationRevision: fetched.applicationRevision
        )
        let undone = try await client.undoSuggestionApplication(
            applicationID: Self.applicationID,
            expectedApplicationRevision: 1,
            requestBody: undoBody,
            idempotencyKey: "macos-undo-transport-test"
        )
        #expect(undone.application.status == .undone)
        #expect(undone.application.applicationRevision == 2)

        let requests = URLProtocolStub.storage.requests(for: Self.testBearer)
        #expect(requests.map(\.method) == ["POST", "POST", "GET", "POST"])
        #expect(requests.map(\.url.path) == [
            "/gateway/v1/suggestions/application-previews",
            "/gateway/v1/suggestions/application-previews/\(Self.previewID.uuidString.lowercased())/apply",
            "/gateway/v1/suggestions/applications/\(Self.applicationID.uuidString.lowercased())",
            "/gateway/v1/suggestions/applications/\(Self.applicationID.uuidString.lowercased())/undo",
        ])
        #expect(requests[0].jsonBody?["proposals"] is [Any])
        #expect(requests[1].body == applyBody)
        #expect(requests[1].headers["Idempotency-Key"] == "macos-apply-transport-test")
        #expect(requests[1].jsonBody?["expected_review_hash"] as? String == Self.reviewHash)
        #expect(requests[3].body == undoBody)
        #expect(requests[3].headers["Idempotency-Key"] == "macos-undo-transport-test")
        #expect((requests[3].jsonBody?["expected_application_revision"] as? NSNumber)?.uint64Value == 1)
    }

    @Test("malformed exact request material fails before transport")
    func testMalformedExactRequestsFailClosed() async throws {
        let client = Self.client()
        let validBody = try client.prepareSuggestionApplicationApplyBody(
            expectedReviewHash: Self.reviewHash
        )
        let mutatedBody = validBody + Data(" ".utf8)

        await #expect(throws: DayWeaveAPIError.requestEncodingFailed) {
            _ = try await client.applySuggestionApplication(
                previewID: Self.previewID,
                expectedReviewHash: Self.reviewHash,
                requestBody: mutatedBody,
                idempotencyKey: "macos-apply-invalid-body"
            )
        }
        await #expect(throws: DayWeaveAPIError.requestEncodingFailed) {
            _ = try await client.applySuggestionApplication(
                previewID: Self.previewID,
                expectedReviewHash: Self.reviewHash,
                requestBody: validBody,
                idempotencyKey: "bad key with spaces"
            )
        }
        await #expect(throws: DayWeaveAPIError.requestEncodingFailed) {
            _ = try await client.previewSuggestionApplication(.init(proposals: []))
        }
        #expect(URLProtocolStub.storage.requests(for: Self.testBearer).isEmpty)
    }

    static let testBearer = "proposal-application-api-test"
    static let proposalID = DayWeaveAPIClientTests.proposalID
    static let itemID = DayWeaveAPIClientTests.itemID
    static let previewID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    static let commandID = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
    static let applicationID = UUID(uuidString: "44444444-4444-4444-8444-444444444444")!
    static let reviewHash = "sha256:" + String(repeating: "a", count: 64)

    static func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: testBearer
        )
    }

    static func previewBody(canApply: Bool) -> Data {
        let conflicts = canApply ? "[]" : """
        [{"code":"item_not_found","command_id":"\(commandID.uuidString.lowercased())",
          "item_id":"\(itemID.uuidString.lowercased())","expected_revision":null,
          "actual_revision":null,"summary":"The item changed before review."}]
        """
        let diffs = canApply ? """
        [{"command_id":"\(commandID.uuidString.lowercased())","operation":"create_item",
          "item_id":"\(itemID.uuidString.lowercased())","changed_fields":[
            "is_sensitive","kind","status","title","notes","timezone_name",
            "duration_seconds","deadline_at","earliest_start_at","recurrence",
            "flexible_constraints","split_policy","importance","urgency","parent_id",
            "sibling_order","is_executable","revision","completed_at","deleted_at"
          ],
          "before":null,"after":\(DayWeaveAPIClientTests.canonicalItemObject(revision: 1))}]
        """ : "[]"
        return Data("""
        {
          "preview_id":"\(previewID.uuidString.lowercased())",
          "proposals":[{"proposal_id":"\(proposalID.uuidString.lowercased())","expected_revision":4}],
          "change_set_schema":"dayweave.proposal-change-set/1",
          "command_ids":["\(commandID.uuidString.lowercased())"],
          "review_hash":"\(reviewHash)",
          "expires_at":"2026-09-01T09:00:00Z",
          "can_apply":\(canApply),
          "maximum_risk":"low",
          "requires_explicit_approval":false,
          "diffs":\(diffs),
          "implicit_diffs":[],
          "risks":[{"code":"creates_item","level":"low",
            "command_id":"\(commandID.uuidString.lowercased())",
            "item_id":"\(itemID.uuidString.lowercased())",
            "requires_explicit_approval":false,"summary":"Creates a reversible local item."}],
          "conflicts":\(conflicts)
        }
        """.utf8)
    }

    static func applyResponseBody() -> Data {
        Data("{\"application\":\(String(decoding: receiptBody(status: "applied", revision: 1), as: UTF8.self)),\"replayed\":false}".utf8)
    }

    static func undoResponseBody() -> Data {
        Data("{\"application\":\(String(decoding: receiptBody(status: "undone", revision: 2), as: UTF8.self)),\"replayed\":false}".utf8)
    }

    static func receiptBody(status: String, revision: UInt64) -> Data {
        let undoneAt = status == "undone" ? "\"2026-08-30T10:00:00Z\"" : "null"
        return Data("""
        {
          "application_id":"\(applicationID.uuidString.lowercased())",
          "proposals":[{"proposal_id":"\(proposalID.uuidString.lowercased())","applied_revision":5}],
          "application_revision":\(revision),
          "status":"\(status)",
          "command_ids":["\(commandID.uuidString.lowercased())"],
          "affected_item_ids":["\(itemID.uuidString.lowercased())"],
          "applied_at":"2026-08-30T09:00:00Z",
          "undo_expires_at":"2026-08-31T09:00:00Z",
          "undone_at":\(undoneAt)
        }
        """.utf8)
    }
}
#endif
