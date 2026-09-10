import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Routine occurrence private native transport", .serialized)
@MainActor
struct RoutineOccurrenceAPIClientTests {
    private typealias F = RoutineOccurrenceTestFixtures
    private static let token = "synthetic-routine-occurrence-transport"
    private static let headers = ["Content-Type": "application/json", "Cache-Control": "no-store, max-age=0"]
    init() { URLProtocolStub.storage.reset(key: Self.token) }

    @Test("GET uses ledger instance identity and requires no legacy Pragma header")
    func get() async throws {
        try enqueue(F.snapshot())
        #expect(try await client().routineOccurrence(instanceID: F.instanceID) == F.snapshot())
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/routine-occurrences/\(F.instanceID.uuidString.lowercased())")
        #expect(request.body == nil && request.headers["Authorization"] == "Bearer \(Self.token)")
        try enqueue(F.snapshot())
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrence(instanceID: F.plannerID) }
    }

    @Test("list and delta forward opaque cursors without guessing checkpoint internals")
    func pages() async throws {
        try enqueue(F.page())
        #expect(try await client().routineOccurrences(cursor: "opaque+/:checkpoint", limit: 12) == F.page())
        try enqueue(F.page())
        #expect(try await client().routineOccurrenceDelta(cursor: "terminal-checkpoint", limit: 100) == F.page())
        let requests = URLProtocolStub.storage.requests(for: Self.token)
        #expect(requests.map(\.url.path) == ["/gateway/v1/routine-occurrences", "/gateway/v1/routine-occurrences/delta"])
        let query = try #require(URLComponents(url: requests[0].url, resolvingAgainstBaseURL: false)?.queryItems)
        #expect(query.first(where: { $0.name == "cursor" })?.value == "opaque+/:checkpoint")
        #expect(query.first(where: { $0.name == "limit" })?.value == "12")
    }

    @Test("selected uncached occurrence lookup binds explicit series and planner identity")
    func lookup() async throws {
        try enqueue(F.snapshot())
        #expect(try await client().lookupRoutineOccurrence(seriesItemID: F.rootID, occurrenceID: F.plannerID) == F.snapshot())
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.url.path == "/gateway/v1/routine-occurrences/lookup")
        let query = try #require(URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.queryItems)
        #expect(query.first(where: { $0.name == "series_item_id" })?.value == F.rootID.uuidString.lowercased())
        #expect(query.first(where: { $0.name == "occurrence_id" })?.value == F.plannerID.uuidString.lowercased())
        #expect(request.body == nil && request.headers["Authorization"] == "Bearer \(Self.token)")
        try enqueue(F.snapshot())
        await #expect(throws: RoutineOccurrenceError.invalidData) {
            try await client().lookupRoutineOccurrence(seriesItemID: F.childID, occurrenceID: F.plannerID)
        }
        try enqueue(F.snapshot(), extra: ["Idempotency-Replayed": "false"])
        await #expect(throws: RoutineOccurrenceError.invalidData) {
            try await client().lookupRoutineOccurrence(seriesItemID: F.rootID, occurrenceID: F.plannerID)
        }
        await #expect(throws: RoutineOccurrenceError.invalidData) {
            try await client().lookupRoutineOccurrence(seriesItemID: F.rootID, occurrenceID: F.instanceID)
        }
    }

    @Test("PUT sends unchanged reviewed bytes and validates exact historical replay")
    func exactReplay() async throws {
        let body = Data(" \n".utf8) + (try F.command().bytes()) + Data("\n ".utf8)
        try enqueue(F.mutation(), extra: ["Idempotency-Replayed": "true"])
        let result = try await client().putRoutineOccurrenceMember(instanceID: F.instanceID, memberID: F.rootID, requestBody: body)
        #expect(result.replayed && result.matches(instanceID: F.instanceID, memberID: F.rootID, command: F.command()))
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "PUT" && request.body == body)
        #expect(request.url.path == "/gateway/v1/routine-occurrences/\(F.instanceID.uuidString.lowercased())/members/\(F.rootID.uuidString.lowercased())")
        #expect(request.headers["Idempotency-Key"] == nil)
    }

    @Test("replay header must be single and agree, and never appear on GET or error")
    func replayHeaders() async throws {
        for headers in [[:], ["Idempotency-Replayed": "false"], ["Idempotency-Replayed": "true, true"], ["Idempotency-Replayed": "true, false"]] {
            try enqueue(F.mutation(), extra: headers)
            await #expect(throws: RoutineOccurrenceError.invalidData) { try await put() }
        }
        try enqueue(F.snapshot(), extra: ["Idempotency-Replayed": "true"])
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrence(instanceID: F.instanceID) }
        #expect(throws: RoutineOccurrenceError.invalidData) {
            try RoutineOccurrenceHTTP.header("idempotency-replayed", fields: ["Idempotency-Replayed": "true", "idempotency-replayed": "false"])
        }
        enqueueRaw(#"{"error":{"code":"routine_occurrence_member_stale","message":"Synthetic"}}"#, status: 409, extra: ["Idempotency-Replayed": "true"])
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await put() }
    }

    @Test("only exact named code and status pairs provide typed no-effect evidence")
    func errors() async throws {
        for (code, status) in RoutineOccurrenceValidation.definitiveStatuses {
            enqueueRaw("{\"error\":{\"code\":\"\(code)\",\"message\":\"Synthetic private blocker\"}}", status: status)
            await #expect(throws: RoutineOccurrenceError.definitive(code)) { try await put() }
        }
        for (code, status) in [("routine_occurrence_missing", 409), ("not_found", 404), ("conflict", 409), ("invalid_json", 400), ("service_unavailable", 503), ("unauthorized", 401)] {
            enqueueRaw("{\"error\":{\"code\":\"\(code)\",\"message\":\"Synthetic private blocker\"}}", status: status)
            await #expect(throws: RoutineOccurrenceError.unavailable) { try await put() }
        }
        enqueueRaw(#"{"error":{"code":"routine_occurrence_missing","message":"Synthetic"}}"#, status: 404)
        await #expect(throws: RoutineOccurrenceError.definitive("routine_occurrence_missing")) { try await client().routineOccurrence(instanceID: F.instanceID) }
    }

    @Test("duplicate keys and noncanonical integer tokens cannot become read or failure proof")
    func rawIntegrity() async throws {
        let raw = String(decoding: try F.bytes(F.snapshot()), as: UTF8.self)
        for replacement in ["\"schema_version\":1.0", "\"schema_version\":1e0", "\"schema_version\":1,\"schema_versi\\u006fn\":1"] {
            enqueueRaw(raw.replacingOccurrences(of: "\"schema_version\":1", with: replacement))
            await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrence(instanceID: F.instanceID) }
        }
        enqueueRaw(#"{"error":{"code":"routine_occurrence_member_stale","co\u0064e":"routine_occurrence_member_stale","message":"Synthetic"}}"#, status: 409)
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await put() }
        enqueueRaw(#"{"error":{"code":"routine_occurrence_member_stale","message":"Synthetic","unknown":true}}"#, status: 409)
        await #expect(throws: RoutineOccurrenceError.unavailable) { try await put() }
    }

    @Test("malformed error envelopes never establish definitive no-effect authority")
    func closedErrorEnvelopes() async throws {
        for raw in [
            #"{"error":{"code":"routine_occurrence_member_stale","message":"Synthetic"},"future":true}"#,
            #"{"error":{"code":"routine_occurrence_member_stale","message":"Synthetic","future":true}}"#,
            #"{"error":{"code":"routine_occurrence_member_stale","message":"Synthetic","details":null}}"#,
            #"{"error":{"code":"routine_occurrence_member_stale"}}"#,
            #"{"error":{"code":"routine_occurrence_member_stale","message":null}}"#,
            #"{"error":{"code":"routine_occurrence_member_stale","message":42}}"#,
            #"{"error":{"code":"routine_occurrence_member_stale","message":{"private":"Synthetic"}}}"#,
            #"{"error":{"message":"Synthetic"}}"#,
            #"{"error":{"code":409,"message":"Synthetic"}}"#,
            #"{"error":null}"#,
        ] {
            enqueueRaw(raw, status: 409)
            await #expect(throws: RoutineOccurrenceError.unavailable) { try await put() }
            enqueueRaw(raw, status: 409)
            await #expect(throws: RoutineOccurrenceError.unavailable) {
                try await client().routineOccurrence(instanceID: F.instanceID)
            }
        }
    }

    @Test("wrong receipt instance, revisions and requested mode reject without canonical mutation")
    func receiptBinding() async throws {
        for mode in 0..<4 {
            var object = try F.object(F.mutation())
            var occurrence = object["occurrence"] as! [String: Any]
            var aggregate = occurrence["aggregate"] as! [String: Any]
            switch mode {
            case 0: object["operation_id"] = F.id(201).uuidString
            case 1: aggregate["revision"] = 3
            case 2:
                var manifest = aggregate["manifest"] as! [String: Any]
                manifest["id"] = F.id(999).uuidString; aggregate["manifest"] = manifest
            default:
                var members = aggregate["members"] as! [[String: Any]]
                members[0]["mode"] = "automatic"; aggregate["members"] = members
            }
            occurrence["aggregate"] = aggregate; object["occurrence"] = occurrence
            enqueueRaw(String(decoding: try F.data(object), as: UTF8.self), extra: ["Idempotency-Replayed": "true"])
            await #expect(throws: RoutineOccurrenceError.invalidData) { try await put() }
        }
    }

    @Test("privacy headers, whole-instance pages and continuation progress are enforced")
    func privacyAndPageBounds() async throws {
        for headers in [["Content-Type": "application/json"], ["Content-Type": "text/plain", "Cache-Control": "no-store, max-age=0"],
                        ["Content-Type": "application/json", "Cache-Control": "public, max-age=0"]] {
            URLProtocolStub.storage.enqueue(key: Self.token, .init(statusCode: 200, headers: headers, body: try F.bytes(F.snapshot())))
            await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrence(instanceID: F.instanceID) }
        }
        let two = RoutineOccurrencePage(schemaVersion: 1, changes: [.init(sequence: 1, occurrence: F.snapshot()),
            .init(sequence: 2, occurrence: F.snapshot(revision: 2, mode: .keepOpen))], cursor: "next", hasMore: true)
        try enqueue(two)
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrences() }
        try enqueue(two)
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrenceDelta(limit: 1) }
        try enqueue(two)
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrenceDelta(cursor: "next") }
    }

    @Test("invalid commands and out-of-bound query inputs never reach transport")
    func requestAdmission() async throws {
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrences(limit: 101) }
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().routineOccurrenceDelta(cursor: String(repeating: "x", count: 513)) }
        let raw = String(decoding: try F.command().bytes(), as: UTF8.self).replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1,\"schema_version\":1")
        await #expect(throws: RoutineOccurrenceError.invalidData) { try await client().putRoutineOccurrenceMember(instanceID: F.instanceID, memberID: F.rootID, requestBody: Data(raw.utf8)) }
        #expect(URLProtocolStub.storage.requests(for: Self.token).isEmpty)
    }

    @Test("streaming body admission keeps this private endpoint at eight MiB")
    func byteBound() async throws {
        URLProtocolStub.storage.enqueue(key: Self.token, .init(statusCode: 200, headers: Self.headers,
            body: Data(repeating: 32, count: RoutineOccurrenceValidation.maximumBytes + 1)))
        await #expect(throws: DayWeaveAPIError.responseTooLarge(limitBytes: RoutineOccurrenceValidation.maximumBytes)) {
            try await client().routineOccurrence(instanceID: F.instanceID)
        }
    }

    private func put() async throws -> RoutineOccurrenceMutation {
        try await client().putRoutineOccurrenceMember(instanceID: F.instanceID, memberID: F.rootID, requestBody: F.command().bytes())
    }
    private func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"), session: URLProtocolStub.makeSession(), bearerToken: Self.token)
    }
    private func enqueue<T: Encodable>(_ value: T, extra: [String: String] = [:]) throws {
        enqueueRaw(String(decoding: try F.bytes(value), as: UTF8.self), extra: extra)
    }
    private func enqueueRaw(_ body: String, status: Int = 200, extra: [String: String] = [:]) {
        URLProtocolStub.storage.enqueue(key: Self.token, .init(statusCode: status,
            headers: Self.headers.merging(extra) { _, value in value }, body: Data(body.utf8)))
    }
}
#endif
