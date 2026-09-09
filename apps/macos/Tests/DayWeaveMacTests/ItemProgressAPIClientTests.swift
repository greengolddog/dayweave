import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Independent progress strict transport", .serialized)
@MainActor
struct ItemProgressAPIClientTests {
    private static let token = "synthetic-independent-progress-test"
    private static let itemID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    private static let operationID = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
    private static let headers = ["Content-Type": "application/json", "Cache-Control": "no-store, max-age=0", "Pragma": "no-cache"]
    init() { URLProtocolStub.storage.reset(key: Self.token) }

    @Test("GET uses item-scoped authority and explicit zero baseline")
    func getBaseline() async throws {
        enqueue(snapshot())
        let result = try await client().itemProgress(Self.itemID)
        #expect(result.revision == 0 && result.updatedAt == nil && result.components.isEmpty)
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/items/\(Self.itemID.uuidString.lowercased())/progress")
        #expect(request.headers["Authorization"] == "Bearer \(Self.token)")
    }

    @Test("PUT retains exact reviewed bytes and binds the historical receipt")
    func exactPutReplay() async throws {
        let command = ItemProgressCommand(operationID: Self.operationID, expectedItemRevision: 7,
            expectedProgressRevision: 0, components: [])
        let bytes = Data(" \n".utf8) + (try command.bytes()) + Data("\n ".utf8)
        enqueue(receipt(), extra: ["Idempotency-Replayed": "true"])
        let response = try await client().putItemProgress(Self.itemID, requestBody: bytes)
        #expect(response.replayed && response.matches(itemID: Self.itemID, command: command))
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "PUT" && request.body == bytes)
    }

    @Test("raw duplicate keys and alternate numeric spellings cannot become GET proof")
    func rawGetRejection() async {
        for raw in [snapshot().replacingOccurrences(of: "\"revision\":0", with: "\"revision\":0,\"revision\":0"),
            snapshot().replacingOccurrences(of: "\"revision\":0", with: "\"revision\":0,\"revi\\u0073ion\":0"),
            snapshot().replacingOccurrences(of: "\"revision\":0", with: "\"revision\":0.0"),
            snapshot().replacingOccurrences(of: "\"revision\":0", with: "\"revision\":0e0")] {
            enqueue(raw)
            await #expect(throws: ItemProgressError.invalidData) { try await client().itemProgress(Self.itemID) }
        }
    }

    @Test("missing nullable fields and unknown fields fail closed")
    func requiredFields() async {
        for raw in [snapshot().replacingOccurrences(of: ",\"updated_at\":null", with: ""),
            snapshot().replacingOccurrences(of: "\"revision\":0", with: "\"revision\":0,\"future\":true"),
            snapshot().replacingOccurrences(of: Self.itemID.uuidString.lowercased(), with: Self.operationID.uuidString.lowercased())] {
            enqueue(raw)
            await #expect(throws: (any Error).self) { try await client().itemProgress(Self.itemID) }
        }
    }

    @Test("PUT requires one agreeing replay header and GET forbids it")
    func replayHeaderProof() async throws {
        let command = ItemProgressCommand(operationID: Self.operationID, expectedItemRevision: 7,
            expectedProgressRevision: 0, components: [])
        for extra in [[:], ["Idempotency-Replayed": "false"], ["Idempotency-Replayed": "true, true"]] {
            enqueue(receipt(), extra: extra)
            await #expect(throws: ItemProgressError.invalidData) {
                try await client().putItemProgress(Self.itemID, requestBody: command.bytes())
            }
        }
        enqueue(snapshot(), extra: ["Idempotency-Replayed": "true"])
        await #expect(throws: ItemProgressError.invalidData) { try await client().itemProgress(Self.itemID) }
    }

    @Test("only the exact named status admits definitive rejection")
    func definitiveErrorProof() async throws {
        let command = ItemProgressCommand(operationID: Self.operationID, expectedItemRevision: 7,
            expectedProgressRevision: 0, components: [])
        enqueue(#"{"error":{"code":"item_progress_revision_stale","message":"Synthetic conflict","details":null}}"#, status: 409)
        await #expect(throws: ItemProgressError.definitive("item_progress_revision_stale")) {
            try await client().putItemProgress(Self.itemID, requestBody: command.bytes())
        }
        for code in ["conflict", "item_progress_item_missing"] {
            enqueue("{\"error\":{\"code\":\"\(code)\",\"message\":\"Synthetic\",\"details\":null}}", status: 409)
            do {
                _ = try await client().putItemProgress(Self.itemID, requestBody: command.bytes())
                Issue.record("A rejected HTTP response was accepted")
            } catch let error as ItemProgressError {
                if case .definitive = error { Issue.record("Generic status became discard authority") }
            } catch { /* Uncertain errors keep the caller's exact journal. */ }
        }
    }

    @Test("cache policy and duplicate error keys are required before rejection proof")
    func errorIntegrity() async throws {
        let command = ItemProgressCommand(operationID: Self.operationID, expectedItemRevision: 7,
            expectedProgressRevision: 0, components: [])
        enqueue(#"{"error":{"code":"item_progress_revision_stale","code":"item_progress_revision_stale","message":"Synthetic"}}"#, status: 409)
        await #expect(throws: ItemProgressError.invalidData) {
            try await client().putItemProgress(Self.itemID, requestBody: command.bytes())
        }
        URLProtocolStub.storage.enqueue(key: Self.token,
            .init(statusCode: 200, headers: ["Content-Type": "application/json"], body: Data(snapshot().utf8)))
        await #expect(throws: ItemProgressError.invalidData) { try await client().itemProgress(Self.itemID) }
    }

    private func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(), bearerToken: Self.token)
    }
    private func enqueue(_ body: String, status: Int = 200, extra: [String: String] = [:]) {
        URLProtocolStub.storage.enqueue(key: Self.token, .init(statusCode: status,
            headers: Self.headers.merging(extra) { _, newer in newer }, body: Data(body.utf8)))
    }
    private func snapshot(revision: Int = 0) -> String {
        "{\"schema_version\":1,\"item_id\":\"\(Self.itemID.uuidString.lowercased())\",\"item_revision\":7,\"revision\":\(revision),\"components\":[],\"updated_at\":\(revision == 0 ? "null" : "\"2026-09-08T10:01:00Z\"")}"
    }
    private func receipt() -> String {
        "{\"operation_id\":\"\(Self.operationID.uuidString.lowercased())\",\"replayed\":true,\"progress\":\(snapshot(revision: 1))}"
    }
}
#endif
