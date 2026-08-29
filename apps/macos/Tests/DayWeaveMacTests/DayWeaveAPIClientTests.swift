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
        #expect(request.headers["Cache-Control"] == "no-store")
        #expect(request.headers["Pragma"] == "no-cache")
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

    @Test("declared oversized responses are rejected before buffering")
    func testOversizedResponseIsRejected() async throws {
        URLProtocolStub.storage.enqueue(key: Self.apiToken, .init(
            statusCode: 200,
            headers: ["Content-Length": String(DayWeaveAPIClient.maximumResponseBytes + 1)],
            body: Data(#"{"suggestions":[]}"#.utf8)
        ))
        let client = makeClient(token: "test-secret-token")

        do {
            _ = try await client.listSuggestions()
            Issue.record("Expected the response size gate to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseTooLarge(limitBytes: DayWeaveAPIClient.maximumResponseBytes))
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    @Test("base URL enforces transport security")
    func testBaseURLRequiresHTTPSExceptForLoopbackDevelopment() throws {
        let normalized = try DayWeaveAPIBaseURL("HTTPS://API.EXAMPLE.COM:443/root/")
        #expect(normalized.credentialOriginIdentifier == "https://api.example.com")
        #expect(normalized.canonicalConfigurationIdentifier == "https://api.example.com/root")
        _ = try DayWeaveAPIBaseURL("http://127.0.0.1:8787")
        _ = try DayWeaveAPIBaseURL("http://localhost:8080")
        let ipv6 = try DayWeaveAPIBaseURL("http://[::1]:8787/root")
        #expect(ipv6.credentialOriginIdentifier == "http://[::1]:8787")
        let scopedIPv6 = try DayWeaveAPIBaseURL("http://[::1%25lo0]:8787/root")
        #expect(scopedIPv6.credentialOriginIdentifier == "http://[::1%25lo0]:8787")
        #expect(scopedIPv6.canonicalConfigurationIdentifier == "http://[::1%25lo0]:8787/root")
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

    @Test("full configuration identifiers normalize only safe spelling variants")
    func testCanonicalFullConfigurationIdentifier() throws {
        let rootA = try DayWeaveAPIBaseURL("https://API.EXAMPLE.COM:443/")
        let rootB = try DayWeaveAPIBaseURL("https://api.example.com")
        #expect(rootA.canonicalConfigurationIdentifier == rootB.canonicalConfigurationIdentifier)

        let pathA = try DayWeaveAPIBaseURL("https://API.EXAMPLE.COM:443/gateway/")
        let pathB = try DayWeaveAPIBaseURL("https://api.example.com/gateway")
        #expect(pathA.canonicalConfigurationIdentifier == pathB.canonicalConfigurationIdentifier)
        #expect(
            try DayWeaveAPIBaseURL("https://api.example.com/a%2Fb").canonicalConfigurationIdentifier
                != DayWeaveAPIBaseURL("https://api.example.com/a/b").canonicalConfigurationIdentifier
        )
        #expect(
            try DayWeaveAPIBaseURL("https://api.example.com/a/../b").canonicalConfigurationIdentifier
                != DayWeaveAPIBaseURL("https://api.example.com/b").canonicalConfigurationIdentifier
        )
        #expect(
            try DayWeaveAPIBaseURL("https://api.example.com/gateway//").canonicalConfigurationIdentifier
                != pathA.canonicalConfigurationIdentifier
        )
        #expect(
            try DayWeaveAPIBaseURL("https://other.example.com/gateway").canonicalConfigurationIdentifier
                != pathA.canonicalConfigurationIdentifier
        )
    }

    @Test("default API transport is ephemeral and credential-free")
    func testEphemeralTransportConfiguration() {
        let session = makeDayWeaveEphemeralSession()
        #expect(session.configuration.urlCache == nil)
        #expect(session.configuration.httpCookieStorage == nil)
        #expect(!session.configuration.httpShouldSetCookies)
        #expect(session.configuration.urlCredentialStorage == nil)
        #expect(session.configuration.requestCachePolicy == .reloadIgnoringLocalAndRemoteCacheData)
    }

    @Test("fixed preview inputs encode an explicit sensitivity classification")
    func testFixedPreviewInputEncodesSensitivity() throws {
        let canaryID = UUID(uuidString: "44444444-4444-4444-8444-444444444444")!
        let instant = Date(timeIntervalSince1970: 1_787_993_600)
        let request = DayWeaveSchedulePreviewRequest(
            asOf: instant,
            horizonStart: instant,
            horizonEnd: instant.addingTimeInterval(3_600),
            timezoneName: "UTC",
            availability: [],
            fixedBlocks: [
                .init(
                    id: canaryID,
                    isSensitive: true,
                    title: "SYNTHETIC-SENSITIVE-FIXED-MACOS",
                    start: instant,
                    end: instant.addingTimeInterval(1_800),
                    source: "google_calendar"
                )
            ],
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let object = try #require(
            JSONSerialization.jsonObject(with: encoder.encode(request)) as? [String: Any]
        )
        let fixed = try #require((object["fixed_blocks"] as? [[String: Any]])?.first)

        #expect(fixed["id"] as? String == canaryID.uuidString.uppercased())
        #expect(fixed["is_sensitive"] as? Bool == true)
        #expect(fixed["title"] as? String == "SYNTHETIC-SENSITIVE-FIXED-MACOS")
    }

    @Test("canonical delta preserves scheduling fields and cursor semantics")
    func testCanonicalDeltaPreservesSchedulingFields() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(
                statusCode: 200,
                body: Data("""
                {"changes":[{"type":"upsert","item":\(Self.canonicalItemObject(revision: 7, includeFutureField: true))}],
                 "next_cursor":"opaque-cursor-7","has_more":false}
                """.utf8)
            )
        )
        let client = makeClient(token: Self.apiToken)

        let page = try await client.itemDelta(cursor: "opaque-cursor-6", limit: 37)

        #expect(page.nextCursor == "opaque-cursor-7")
        #expect(!page.hasMore)
        let item: DayWeaveCanonicalItem
        switch try #require(page.changes.first) {
        case let .upsert(value): item = value
        case .tombstone:
            Issue.record("Expected an item upsert")
            return
        }
        #expect(item.id == Self.itemID)
        #expect(!item.isSensitive)
        #expect(item.revision == 7)
        #expect(item.deadlineAt != nil)
        #expect(item.parentID == Self.parentID)
        #expect(item.recurrence == .object([
            "type": .string("weekly"),
            "times_per_week": .number(2),
            "weekdays": .array([.string("monday"), .string("thursday")]),
        ]))
        #expect(item.splitPolicy == .splittable(minimumChunkSeconds: 900, maximumChunkSeconds: 2_700))
        #expect(item.flexibleConstraints == .object([
            "energy": .string("deep"),
            "tags": .array([.string("client")]),
        ]))
        #expect(item.unsupportedFields == [
            "future_scheduling_rule": .object(["mode": .string("server_defined")]),
        ])
        #expect(!item.supportsLosslessReplacement)

        let request = try #require(URLProtocolStub.storage.requests(for: Self.apiToken).first)
        #expect(request.url.path == "/gateway/v1/items/delta")
        let query = try #require(URLComponents(url: request.url, resolvingAgainstBaseURL: false))
        #expect(Set(query.queryItems ?? []) == Set([
            URLQueryItem(name: "cursor", value: "opaque-cursor-6"),
            URLQueryItem(name: "limit", value: "37"),
        ]))
    }

    @Test("canonical create and replacement are idempotent and revision guarded")
    func testCanonicalMutationsCarryExactContracts() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.apiToken,
            .init(statusCode: 201, body: Data("{\"item\":\(Self.canonicalItemObject(revision: 1, deadlineAt: "null", recurrence: "null"))}".utf8)),
            .init(statusCode: 200, body: Data("{\"item\":\(Self.canonicalItemObject(revision: 2, status: "paused", deadlineAt: "null", recurrence: "null"))}".utf8))
        )
        let client = makeClient(token: Self.apiToken)
        let fields = DayWeaveCanonicalItemFields(
            kind: .task,
            status: .scheduled,
            title: "Canonical deep work",
            notes: "Keep exact constraints",
            timezoneName: "Europe/Madrid",
            durationSeconds: 3_600,
            deadlineAt: Self.date("2026-09-01T17:00:00Z"),
            recurrence: .object([
                "type": .string("weekly"),
                "times_per_week": .number(2),
                "weekdays": .array([.string("monday"), .string("thursday")]),
            ]),
            flexibleConstraints: .object([
                "energy": .string("deep"),
                "tags": .array([.string("client")]),
            ]),
            splitPolicy: .splittable(minimumChunkSeconds: 900, maximumChunkSeconds: 2_700),
            importance: 90,
            urgency: 70,
            parentID: Self.parentID,
            siblingOrder: 3
        )

        let created = try await client.createCanonicalItem(
            .init(id: Self.itemID, fields: fields),
            idempotencyKey: "mac-create-stable"
        )
        let replacement = DayWeaveCanonicalItemFields(item: created, status: .paused)
        _ = try await client.replaceCanonicalItem(
            created.id,
            expectedRevision: created.revision,
            item: replacement,
            idempotencyKey: "mac-replace-stable"
        )

        let requests = URLProtocolStub.storage.requests(for: Self.apiToken)
        #expect(requests.map(\.method) == ["POST", "PUT"])
        #expect(requests[0].headers["Idempotency-Key"] == "mac-create-stable")
        #expect(requests[1].headers["Idempotency-Key"] == "mac-replace-stable")
        let create = try #require(requests[0].jsonBody)
        #expect((create["id"] as? String)?.lowercased() == Self.itemID.uuidString.lowercased())
        #expect(create["is_sensitive"] as? Bool == false)
        #expect(create["fields"] == nil)
        #expect((create["duration_seconds"] as? NSNumber)?.uint32Value == 3_600)
        #expect((create["recurrence"] as? [String: Any])?["type"] as? String == "weekly")
        #expect((create["split_policy"] as? [String: Any])?["type"] as? String == "splittable")
        let replace = try #require(requests[1].jsonBody)
        #expect((replace["expected_revision"] as? NSNumber)?.uint64Value == 1)
        #expect((replace["item"] as? [String: Any])?["status"] as? String == "paused")
        #expect((replace["item"] as? [String: Any])?["is_sensitive"] as? Bool == false)
    }

    @Test("canonical sensitivity is required on current wire payloads")
    func testCanonicalSensitivityCannotBeOmitted() throws {
        var object = try #require(
            JSONSerialization.jsonObject(
                with: Data(Self.canonicalItemObject(revision: 1).utf8)
            ) as? [String: Any]
        )
        object.removeValue(forKey: "is_sensitive")
        let missing = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        #expect(throws: DecodingError.self) {
            try decoder.decode(DayWeaveCanonicalItem.self, from: missing)
        }
    }

    @Test("an operation keeps its immutable credential snapshot")
    func testCredentialSnapshotDoesNotRotateMidOperation() async throws {
        let originalToken = "credential-before-rotation"
        let tokenStore = TestBearerTokenStore(token: originalToken)
        URLProtocolStub.storage.reset(key: originalToken)
        URLProtocolStub.storage.enqueue(
            key: originalToken,
            .init(statusCode: 200, body: Data(#"{"changes":[],"next_cursor":"stable","has_more":false}"#.utf8))
        )
        let client = DayWeaveAPIClient(
            baseURL: try DayWeaveAPIBaseURL("https://old-origin.example/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: tokenStore.loadToken()
        )
        tokenStore.saveToken("credential-after-rotation")

        _ = try await client.itemDelta(cursor: nil)

        let request = try #require(URLProtocolStub.storage.requests(for: originalToken).first)
        #expect(request.url.host == "old-origin.example")
        #expect(request.headers["Authorization"] == "Bearer \(originalToken)")
        #expect(URLProtocolStub.storage.requests(for: "credential-after-rotation").isEmpty)
    }

    @Test("future split fields fail closed and unsigned JSON integers round-trip exactly")
    func testLosslessJSONAndFutureSplitPolicy() throws {
        let split = try JSONDecoder().decode(
            DayWeaveSplitPolicy.self,
            from: Data(#"{"type":"splittable","minimum_chunk_seconds":900,"maximum_chunk_seconds":2700,"future_mode":{"limit":18446744073709551615}}"#.utf8)
        )
        if case let .unknown(raw) = split {
            #expect(raw["future_mode"] == .object([
                "limit": .number(JSONNumber(UInt64.max)),
            ]))
        } else {
            Issue.record("A known split policy with future nested fields must be read-only")
        }
        #expect(!split.isSupportedForWrite)

        let value = try JSONDecoder().decode(
            JSONValue.self,
            from: Data("18446744073709551615".utf8)
        )
        let encoded = try JSONEncoder().encode(value)
        #expect(String(decoding: encoded, as: UTF8.self) == "18446744073709551615")
        #expect(!value.supportsLosslessRoundTrip)

        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let value = try decoder.singleValueContainer().decode(String.self)
            let fractional = ISO8601DateFormatter()
            fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            let whole = ISO8601DateFormatter()
            whole.formatOptions = [.withInternetDateTime]
            guard let date = fractional.date(from: value) ?? whole.date(from: value) else {
                throw DecodingError.dataCorrupted(
                    .init(codingPath: decoder.codingPath, debugDescription: "Invalid test timestamp")
                )
            }
            return date
        }
        let item = try decoder.decode(
            DayWeaveCanonicalItem.self,
            from: Data(Self.canonicalItemObject(revision: 9).utf8)
        )
        #expect(!item.supportsLosslessReplacement)
        do {
            _ = try JSONEncoder().encode(DayWeaveCanonicalItemFields(item: item, status: .paused))
            Issue.record("A server timestamp must not be normalized by a full replacement")
        } catch {
            #expect(error is EncodingError)
        }
    }

    private func makeClient(token: String?) -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        )
    }

    static let apiToken = "test-secret-token"
    static let proposalID = UUID(uuidString: "11111111-2222-4333-8444-555555555555")!
    static let itemID = UUID(uuidString: "aaaaaaaa-2222-4333-8444-bbbbbbbbbbbb")!
    static let parentID = UUID(uuidString: "cccccccc-2222-4333-8444-dddddddddddd")!

    static func date(_ value: String) -> Date {
        ISO8601DateFormatter().date(from: value)!
    }

    static func canonicalItemObject(
        revision: UInt64,
        status: String = "scheduled",
        deadlineAt: String = "\"2026-09-01T17:00:00Z\"",
        recurrence: String = #"{"type":"weekly","times_per_week":2,"weekdays":["monday","thursday"]}"#,
        includeFutureField: Bool = false
    ) -> String {
        let futureField = includeFutureField
            ? ",\"future_scheduling_rule\":{\"mode\":\"server_defined\"}"
            : ""
        return """
        {
          "id":"\(itemID.uuidString.lowercased())",
          "is_sensitive":false,
          "kind":"task",
          "status":"\(status)",
          "title":"Canonical deep work",
          "notes":"Keep exact constraints",
          "timezone_name":"Europe/Madrid",
          "duration_seconds":3600,
          "deadline_at":\(deadlineAt),
          "earliest_start_at":null,
          "recurrence":\(recurrence),
          "flexible_constraints":{"energy":"deep","tags":["client"]},
          "split_policy":{"type":"splittable","minimum_chunk_seconds":900,"maximum_chunk_seconds":2700},
          "importance":90,
          "urgency":70,
          "parent_id":"\(parentID.uuidString.lowercased())",
          "sibling_order":3,
          "is_executable":true,
          "revision":\(revision),
          "created_at":"2026-08-29T09:00:00Z",
          "updated_at":"2026-08-29T09:00:00.125Z",
          "completed_at":null,
          "deleted_at":null\(futureField)
        }
        """
    }

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
    private var credential: OriginBoundBearerCredential?
    private var legacyToken: String?

    init(token: String?, origin: String = "https://api.example.com") {
        credential = token.map {
            OriginBoundBearerCredential(
                token: $0,
                origin: Self.normalizedOrigin(origin)
            )
        }
    }

    init(legacyToken: String) {
        credential = nil
        self.legacyToken = legacyToken
    }

    func loadCredential() throws -> OriginBoundBearerCredential? {
        try lock.withLock {
            if legacyToken != nil { throw BearerTokenStoreError.legacyUnboundToken }
            return credential
        }
    }

    func saveCredential(_ credential: OriginBoundBearerCredential) {
        lock.withLock {
            self.credential = credential
            legacyToken = nil
        }
    }

    func deleteCredential() {
        lock.withLock {
            credential = nil
            legacyToken = nil
        }
    }

    func loadToken() -> String? {
        lock.withLock { credential?.token }
    }

    func saveToken(_ token: String, origin: String = "https://api.example.com") {
        saveCredential(.init(token: token, origin: Self.normalizedOrigin(origin)))
    }

    private static func normalizedOrigin(_ value: String) -> String {
        (try? DayWeaveAPIBaseURL(value).credentialOriginIdentifier) ?? value
    }
}

final class URLProtocolStub: URLProtocol, @unchecked Sendable {
    struct Response: Sendable {
        let statusCode: Int
        let headers: [String: String]
        let body: Data
        let delay: TimeInterval

        init(
            statusCode: Int,
            headers: [String: String] = [:],
            body: Data,
            delay: TimeInterval = 0
        ) {
            self.statusCode = statusCode
            self.headers = headers
            self.body = body
            self.delay = delay
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
    private let stateLock = NSLock()
    private var isStopped = false

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
        let deliver: @Sendable () -> Void = { [weak self] in
            guard let self, !self.stateLock.withLock({ self.isStopped }) else { return }
            self.client?.urlProtocol(self, didReceive: httpResponse, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: response.body)
            self.client?.urlProtocolDidFinishLoading(self)
        }
        if response.delay > 0 {
            DispatchQueue.global().asyncAfter(deadline: .now() + response.delay, execute: deliver)
        } else {
            deliver()
        }
    }

    override func stopLoading() {
        stateLock.withLock { isStopped = true }
    }
}
