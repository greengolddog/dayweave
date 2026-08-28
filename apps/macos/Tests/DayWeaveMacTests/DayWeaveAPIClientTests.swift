import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("DayWeave API client", .serialized)
@MainActor
struct DayWeaveAPIClientTests {
    init() {
        URLProtocolStub.storage.reset(key: Self.apiToken)
    }

    @Test("list authenticates and decodes the v1 contract")
    func testListUsesBearerAuthenticationAndDecodesTheContract() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.listEnvelope())
        )
        let client = makeClient(token: "test-secret-token")

        let suggestions = try await client.listSuggestions(status: .pending, limit: 25)

        let suggestion = try #require(suggestions.first)
        #expect(suggestions.count == 1)
        #expect(suggestion.id == Self.proposalID)
        #expect(suggestion.revision == 4)
        #expect(suggestion.source == .codex)
        #expect(suggestion.kind == .createItem)
        #expect(suggestion.status == .pending)
        #expect(suggestion.payload["duration_minutes"] == .number(45))

        let request = try #require(URLProtocolStub.storage.requests(for: Self.apiToken).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/suggestions")
        let query = try #require(URLComponents(url: request.url, resolvingAgainstBaseURL: false))
        #expect(Set(query.queryItems ?? []) == Set([
            URLQueryItem(name: "limit", value: "25"),
            URLQueryItem(name: "status", value: "pending"),
        ]))
        #expect(request.headers["Authorization"] == "Bearer test-secret-token")
        #expect(request.headers["Accept"] == "application/json")
    }

    @Test("decision and edit requests carry optimistic revisions")
    func testAcceptRejectAndEditSendRevisionGuardedRequests() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 200, body: Self.proposalEnvelope(status: "accepted", revision: 5)),
            .init(statusCode: 200, body: Self.proposalEnvelope(status: "rejected", revision: 5)),
            .init(
                statusCode: 200,
                body: Self.proposalEnvelope(status: "pending", revision: 5, title: "Edited title")
            )
        )
        let client = makeClient(token: "test-secret-token")

        let accepted = try await client.acceptSuggestion(
            id: Self.proposalID,
            expectedRevision: 4,
            note: "Reviewed on Mac"
        )
        let rejected = try await client.rejectSuggestion(
            id: Self.proposalID,
            expectedRevision: 4
        )
        let edited = try await client.editSuggestion(
            id: Self.proposalID,
            edit: .init(expectedRevision: 4, title: "Edited title", explanation: "More detail")
        )

        #expect(accepted.status == .accepted)
        #expect(rejected.status == .rejected)
        #expect(edited.title == "Edited title")

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        #expect(requests.map(\.method) == ["POST", "POST", "PATCH"])
        #expect(requests[0].url.path == "/gateway/v1/suggestions/\(Self.proposalID.uuidString.lowercased())/accept")
        #expect(requests[1].url.path == "/gateway/v1/suggestions/\(Self.proposalID.uuidString.lowercased())/reject")
        #expect(requests[2].url.path == "/gateway/v1/suggestions/\(Self.proposalID.uuidString.lowercased())")

        let acceptBody = try #require(requests[0].jsonBody)
        #expect((acceptBody["expected_revision"] as? NSNumber)?.uint64Value == 4)
        #expect(acceptBody["note"] as? String == "Reviewed on Mac")

        let rejectBody = try #require(requests[1].jsonBody)
        #expect((rejectBody["expected_revision"] as? NSNumber)?.uint64Value == 4)
        #expect(rejectBody["note"] == nil)

        let editBody = try #require(requests[2].jsonBody)
        #expect((editBody["expected_revision"] as? NSNumber)?.uint64Value == 4)
        #expect(editBody["title"] as? String == "Edited title")
        #expect(editBody["explanation"] as? String == "More detail")
    }

    @Test("structured server errors expose safe diagnostics")
    func testStructuredServerErrorPreservesSafeDiagnostics() async throws {
        URLProtocolStub.storage.enqueue(key: Self.apiToken, .init(
            statusCode: 409,
            headers: ["x-request-id": "request-123"],
            body: Data(#"{"error":{"code":"conflict","message":"revision changed"}}"#.utf8)
        ))
        let client = makeClient(token: "test-secret-token")

        do {
            _ = try await client.acceptSuggestion(id: Self.proposalID, expectedRevision: 1)
            Issue.record("Expected a conflict")
        } catch let error as DayWeaveAPIError {
            #expect(error == .server(
                    statusCode: 409,
                    code: "conflict",
                    message: "revision changed",
                    requestID: "request-123"
                ))
            #expect(!error.localizedDescription.contains("test-secret-token"))
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    @Test("missing credentials fail before transport")
    func testMissingTokenFailsBeforeMakingARequest() async throws {
        let client = makeClient(token: nil)

        do {
            _ = try await client.listSuggestions()
            Issue.record("Expected a missing credential error")
        } catch let error as DayWeaveAPIError {
            #expect(error == .credentialUnavailable)
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
        #expect(URLProtocolStub.storage.requests(for: Self.apiToken).isEmpty)
    }

    @Test("base URL enforces transport security")
    func testBaseURLRequiresHTTPSExceptForLoopbackDevelopment() throws {
        _ = try DayWeaveAPIBaseURL("https://api.example.com/root/")
        _ = try DayWeaveAPIBaseURL("http://127.0.0.1:8787")
        _ = try DayWeaveAPIBaseURL("http://localhost:8080")
        do {
            _ = try DayWeaveAPIBaseURL("http://api.example.com")
            Issue.record("Expected insecure remote HTTP to fail")
        } catch {
            #expect(error as? DayWeaveAPIBaseURLError == .insecureRemoteHTTP)
        }
        do {
            _ = try DayWeaveAPIBaseURL("https://token@example.com")
            Issue.record("Expected URL credentials to fail")
        } catch {
            #expect(error as? DayWeaveAPIBaseURLError == .credentialsNotAllowed)
        }
    }

    private func makeClient(token: String?) -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            tokenStore: TestBearerTokenStore(token: token)
        )
    }

    static let apiToken = "test-secret-token"
    static let proposalID = UUID(uuidString: "11111111-2222-4333-8444-555555555555")!

    static func listEnvelope() -> Data {
        Data("{\"suggestions\":[\(proposalObject(status: "pending", revision: 4))]}".utf8)
    }

    static func proposalEnvelope(status: String, revision: UInt64, title: String = "Prepare weekly review") -> Data {
        Data("{\"suggestion\":\(proposalObject(status: status, revision: revision, title: title))}".utf8)
    }

    static func proposalObject(status: String, revision: UInt64, title: String = "Prepare weekly review") -> String {
        """
        {
          "id":"\(proposalID.uuidString.lowercased())",
          "revision":\(revision),
          "submitted_by":"user-42",
          "source":"codex",
          "source_reference":"conversation-42",
          "kind":"create_item",
          "status":"\(status)",
          "title":"\(title)",
          "explanation":"A review keeps the plan realistic",
          "payload":{"duration_minutes":45,"preferences":{"morning":true}},
          "decision_note":null,
          "created_at":"2026-08-29T09:00:00Z",
          "updated_at":"2026-08-29T09:00:00.125Z",
          "expires_at":"2026-09-05T09:00:00Z",
          "decided_at":null
        }
        """
    }
}
#endif

final class TestBearerTokenStore: BearerTokenStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var token: String?

    init(token: String?) {
        self.token = token
    }

    func loadToken() -> String? {
        lock.withLock { token }
    }

    func saveToken(_ token: String) {
        lock.withLock { self.token = token }
    }

    func deleteToken() {
        lock.withLock { token = nil }
    }
}

final class URLProtocolStub: URLProtocol, @unchecked Sendable {
    struct Response: Sendable {
        let statusCode: Int
        let headers: [String: String]
        let body: Data

        init(statusCode: Int, headers: [String: String] = [:], body: Data) {
            self.statusCode = statusCode
            self.headers = headers
            self.body = body
        }
    }

    struct RecordedRequest: Sendable {
        let url: URL
        let method: String
        let headers: [String: String]
        let body: Data?

        var jsonBody: [String: Any]? {
            guard let body,
                  let object = try? JSONSerialization.jsonObject(with: body) else { return nil }
            return object as? [String: Any]
        }
    }

    final class Storage: @unchecked Sendable {
        private let lock = NSLock()
        private var queuedResponses: [String: [Response]] = [:]
        private var recordedRequests: [String: [RecordedRequest]] = [:]

        func requests(for key: String) -> [RecordedRequest] {
            lock.withLock { recordedRequests[key] ?? [] }
        }

        func reset(key: String) {
            lock.withLock {
                queuedResponses[key] = []
                recordedRequests[key] = []
            }
        }

        func enqueue(key: String, _ responses: Response...) {
            lock.withLock { queuedResponses[key, default: []].append(contentsOf: responses) }
        }

        func takeResponse(for request: URLRequest, key: String) -> Response? {
            lock.withLock {
                guard let url = request.url else { return nil }
                recordedRequests[key, default: []].append(.init(
                    url: url,
                    method: request.httpMethod ?? "GET",
                    headers: request.allHTTPHeaderFields ?? [:],
                    body: Self.readBody(from: request)
                ))
                guard var responses = queuedResponses[key], !responses.isEmpty else { return nil }
                let response = responses.removeFirst()
                queuedResponses[key] = responses
                return response
            }
        }

        private static func readBody(from request: URLRequest) -> Data? {
            if let body = request.httpBody {
                return body
            }
            guard let stream = request.httpBodyStream else { return nil }
            stream.open()
            defer { stream.close() }

            var result = Data()
            var buffer = [UInt8](repeating: 0, count: 4_096)
            while true {
                let count = buffer.withUnsafeMutableBufferPointer { pointer in
                    guard let baseAddress = pointer.baseAddress else { return 0 }
                    return stream.read(baseAddress, maxLength: pointer.count)
                }
                guard count > 0 else { break }
                result.append(buffer, count: count)
            }
            return result
        }
    }

    static let storage = Storage()

    static func makeSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [URLProtocolStub.self]
        return URLSession(configuration: configuration)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let authorization = request.value(forHTTPHeaderField: "Authorization") ?? ""
        let key = authorization.hasPrefix("Bearer ") ? String(authorization.dropFirst(7)) : ""
        guard let response = Self.storage.takeResponse(for: request, key: key),
              let url = request.url,
              let httpResponse = HTTPURLResponse(
                url: url,
                statusCode: response.statusCode,
                httpVersion: "HTTP/1.1",
                headerFields: response.headers
              ) else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        client?.urlProtocol(self, didReceive: httpResponse, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: response.body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
