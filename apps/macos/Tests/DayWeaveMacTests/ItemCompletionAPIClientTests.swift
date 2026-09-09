import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Completion strict private transport", .serialized)
@MainActor
struct ItemCompletionAPIClientTests {
    private typealias F = ItemCompletionTestFixtures
    private static let token = "synthetic-completion-transport-test"
    private static let headers = ["Content-Type": "application/json", "Cache-Control": "no-store, max-age=0", "Pragma": "no-cache"]
    init() { URLProtocolStub.storage.reset(key: Self.token) }

    @Test("GET binds the selected item and returns explicit default state")
    func getBaseline() async throws {
        let baseline = ItemCompletionSnapshot(itemID: F.itemID, itemRevision: 7, state: .empty(itemID: F.itemID), evidenceHash: F.hash)
        try enqueue(baseline)
        let value = try await client().itemCompletion(F.itemID)
        #expect(value == baseline)
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/items/\(F.itemID.uuidString.lowercased())/completion")
        #expect(request.headers["Authorization"] == "Bearer \(Self.token)")
        #expect(request.body == nil)
    }

    @Test("PUT retains exact reviewed bytes and validates historical receipt independently")
    func exactPutReplay() async throws {
        let journal = try F.journal()
        try enqueue(receipt(), extra: ["Idempotency-Replayed": "true"])
        let result = try await client().putItemCompletion(F.itemID, requestBody: journal.requestBody)
        #expect(result.replayed && result.matches(itemID: F.itemID, command: journal.command))
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "PUT" && request.body == journal.requestBody)
        #expect(request.headers["Idempotency-Key"] == nil)
    }

    @Test("raw duplicate and fractional or exponent integer responses cannot become GET proof")
    func rawGetRejection() async throws {
        let original = try json(F.snapshot())
        for replacement in ["\"schema_version\":1,\"schema_version\":1",
                            "\"schema_version\":1,\"schema_versi\\u006fn\":1",
                            "\"schema_version\":1.0", "\"schema_version\":1e0"] {
            enqueueRaw(original.replacingOccurrences(of: "\"schema_version\":1", with: replacement))
            await #expect(throws: ItemCompletionError.invalidData) { try await client().itemCompletion(F.itemID) }
        }
    }

    @Test("missing nullable fields, nested unknown fields, and foreign identities fail closed")
    func closedShapes() async throws {
        let original = try json(F.snapshot())
        for raw in [original.replacingOccurrences(of: ",\"provenance\":null", with: ""),
                    original.replacingOccurrences(of: "\"mode\":\"keep_open\"", with: "\"mode\":\"keep_open\",\"future\":true"),
                    original.replacingOccurrences(of: F.itemID.uuidString, with: F.operationID.uuidString)] {
            enqueueRaw(raw)
            await #expect(throws: (any Error).self) { try await client().itemCompletion(F.itemID) }
        }
    }

    @Test("receipt item revision and policy revision each advance exactly one")
    func receiptRevisionBinding() async throws {
        for snapshot in [F.snapshot(itemRevision: 7), F.snapshot(itemRevision: 9),
                         F.snapshot(revision: 2), F.snapshot(revision: 4), F.snapshot(mode: .automatic)] {
            try enqueue(receipt(snapshot), extra: ["Idempotency-Replayed": "true"])
            await #expect(throws: ItemCompletionError.invalidData) {
                try await client().putItemCompletion(F.itemID, requestBody: F.command().bytes())
            }
        }
    }

    @Test("PUT requires exactly one agreeing replay header and GET forbids it")
    func replayHeaders() async throws {
        for extra in [[:], ["Idempotency-Replayed": "false"], ["Idempotency-Replayed": "true, true"],
                      ["Idempotency-Replayed": "false, true"]] {
            try enqueue(receipt(), extra: extra)
            await #expect(throws: ItemCompletionError.invalidData) {
                try await client().putItemCompletion(F.itemID, requestBody: F.command().bytes())
            }
        }
        try enqueue(F.snapshot(), extra: ["Idempotency-Replayed": "true"])
        await #expect(throws: ItemCompletionError.invalidData) { try await client().itemCompletion(F.itemID) }
        // HTTPURLResponse's dictionary initializer can nondeterministically fold
        // differently cased keys before URLProtocol delivers them. Exercise the
        // exact raw field consumer directly as well as coalesced transport values.
        #expect(throws: ItemCompletionError.invalidData) {
            try DayWeaveAPIClient.completionHeader("Idempotency-Replayed",
                fields: ["Idempotency-Replayed": "true", "idempotency-replayed": "false"])
        }
    }

    @Test("only exact named completion status codes can settle a failed PUT")
    func definitiveFailures() async throws {
        for (code, status) in ItemCompletionJournal.definitiveStatuses {
            enqueueRaw("{\"error\":{\"code\":\"\(code)\",\"message\":\"Synthetic conflict\",\"details\":null}}", status: status)
            await #expect(throws: ItemCompletionError.definitive(code)) {
                try await client().putItemCompletion(F.itemID, requestBody: F.command().bytes())
            }
        }
    }

    @Test("generic errors and mismatched status retain uncertainty without private diagnostics")
    func uncertainFailures() async throws {
        for (status, code) in [(409, "conflict"), (404, "not_found"), (409, "item_completion_item_missing"),
                               (400, "invalid_json"), (503, "service_unavailable"), (401, "unauthorized")] {
            enqueueRaw("{\"error\":{\"code\":\"\(code)\",\"message\":\"Synthetic private blocker detail\",\"details\":null}}", status: status)
            await #expect(throws: ItemCompletionError.unavailable) {
                try await client().putItemCompletion(F.itemID, requestBody: F.command().bytes())
            }
        }
        enqueueRaw(#"{"error":{"code":"item_completion_item_missing","message":"Synthetic private blocker detail","details":null}}"#, status: 404)
        await #expect(throws: ItemCompletionError.definitive("item_completion_item_missing")) {
            try await client().itemCompletion(F.itemID)
        }
    }

    @Test("raw duplicate error codes cannot establish no-effect authority")
    func errorIntegrity() async throws {
        for suffix in [#", "code":"item_completion_revision_stale""#,
                       #", "co\u0064e":"item_completion_revision_stale""#] {
            enqueueRaw("{\"error\":{\"code\":\"item_completion_revision_stale\"\(suffix),\"message\":\"Synthetic\"}}", status: 409)
            await #expect(throws: ItemCompletionError.invalidData) {
                try await client().putItemCompletion(F.itemID, requestBody: F.command().bytes())
            }
        }
    }

    @Test("private cache policy and bounded response bodies are required")
    func cacheAndSizeBounds() async throws {
        for headers in [["Content-Type": "application/json"],
                        ["Content-Type": "text/plain", "Cache-Control": "no-store, max-age=0", "Pragma": "no-cache"],
                        ["Content-Type": "application/json", "Cache-Control": "public, max-age=0", "Pragma": "no-cache"]] {
            URLProtocolStub.storage.enqueue(key: Self.token,
                .init(statusCode: 200, headers: headers, body: try JSONEncoder().encode(F.snapshot())))
            await #expect(throws: ItemCompletionError.invalidData) { try await client().itemCompletion(F.itemID) }
        }
        enqueueRaw(String(repeating: " ", count: 64 * 1_024) + (try json(F.snapshot())))
        await #expect(throws: ItemCompletionError.invalidData) { try await client().itemCompletion(F.itemID) }
    }

    @Test("malformed request and self-dependent reopening never reach transport")
    func invalidRequestNoNetwork() async throws {
        let original = try json(F.command())
        let duplicate = original.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1,\"schema_version\":1")
        await #expect(throws: ItemCompletionError.invalidData) {
            try await client().putItemCompletion(F.itemID, requestBody: Data(duplicate.utf8))
        }
        let selfBlocked = ItemCompletionCommand(expectedItemRevision: 7, expectedCompletionRevision: 2,
            expectedEvidenceHash: F.hash, requiredForParent: true, mode: .complete,
            reopening: .init(status: .blocked, blockedReasonKind: .dependency, blockedByItemID: F.itemID))
        await #expect(throws: ItemCompletionError.invalidData) {
            try await client().putItemCompletion(F.itemID, requestBody: selfBlocked.bytes())
        }
        #expect(URLProtocolStub.storage.requests(for: Self.token).isEmpty)
    }

    private func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(), bearerToken: Self.token)
    }
    private func receipt(_ snapshot: ItemCompletionSnapshot = F.snapshot()) -> ItemCompletionReceipt {
        .init(operationID: F.operationID, replayed: true, completion: snapshot)
    }
    private func json<T: Encodable>(_ value: T) throws -> String {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return String(decoding: try encoder.encode(value), as: UTF8.self)
    }
    private func enqueue<T: Encodable>(_ value: T, extra: [String: String] = [:]) throws {
        enqueueRaw(try json(value), extra: extra)
    }
    private func enqueueRaw(_ body: String, status: Int = 200, extra: [String: String] = [:]) {
        URLProtocolStub.storage.enqueue(key: Self.token, .init(statusCode: status,
            headers: Self.headers.merging(extra) { _, newer in newer }, body: Data(body.utf8)))
    }
}
#endif
