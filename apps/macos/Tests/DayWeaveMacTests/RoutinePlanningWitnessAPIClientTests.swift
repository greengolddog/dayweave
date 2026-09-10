import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Routine planning witness private POST", .serialized)
@MainActor
struct RoutinePlanningWitnessAPIClientTests {
    private typealias F = RoutinePlanningWitnessTestFixtures
    private static let token = "synthetic-private-planning-witness"
    private static let headers = ["Content-Type": "application/json", "Cache-Control": "no-store, max-age=0"]
    init() { URLProtocolStub.storage.reset(key: Self.token) }

    @Test("POST is a bound private read, never an idempotent PUT")
    func post() async throws {
        enqueue(try F.data(F.response()))
        let requested = try F.request(), response = try await client().routinePlanningWitness(requested)
        #expect(response.result == .qualified(try F.witness()))
        let captured = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(captured.method == "POST" && captured.url.path == "/gateway/v1/routine-occurrences/planning-witness")
        #expect(captured.url.query == nil && captured.headers["Idempotency-Key"] == nil)
        #expect(captured.headers["Authorization"] == "Bearer \(Self.token)")
        #expect(try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessRequest.self, from: #require(captured.body)) == requested)
    }

    @Test("qualified replies must match the exact request source and terminal capture")
    func requestPairing() async throws {
        for key in ["terminal_cursor", "source_item_revisions"] {
            var witness = F.witnessObject()
            if key == "terminal_cursor" { witness[key] = "different-terminal" }
            else { var revisions = F.revisions(); revisions[F.rootID.uuidString.lowercased()] = 99; witness[key] = revisions }
            enqueue(try F.data(["schema_version": 1, "result": ["status": "qualified", "witness": witness]]))
            await #expect(throws: RoutinePlanningWitnessError.invalidData) { try await client().routinePlanningWitness(F.request()) }
        }
    }

    @Test("success and error must reject any replay header")
    func replayHeaders() async throws {
        for header in ["true", "false", "true, false"] {
            enqueue(try F.data(F.response()), extra: ["Idempotency-Replayed": header])
            await #expect(throws: RoutinePlanningWitnessError.invalidData) { try await client().routinePlanningWitness(F.request()) }
            enqueue(try F.data(["error": ["code": "routine_planning_source_changed", "message": "Synthetic"]]), status: 409, extra: ["Idempotency-Replayed": header])
            await #expect(throws: RoutinePlanningWitnessError.invalidData) { try await client().routinePlanningWitness(F.request()) }
        }
    }

    @Test("all remote reasons and errors remain qualification-only outcomes")
    func outcomes() async throws {
        for reason in RoutinePlanningRemoteReason.allCases {
            enqueue(try RoutinePlanningWitnessValidation.encode(RoutinePlanningWitnessResponse(result: .remoteRequired(reason))))
            #expect(try await client().routinePlanningWitness(F.request()).result == .remoteRequired(reason))
        }
        for (code, status, expected) in [
            ("routine_planning_invalid", 422, RoutinePlanningWitnessError.invalidData),
            ("routine_planning_too_large", 413, .tooLarge),
            ("routine_planning_source_changed", 409, .sourceChanged),
            ("routine_planning_cursor_changed", 409, .cursorChanged),
            ("routine_occurrence_member_stale", 409, .unavailable),
            ("unauthorized", 401, .authentication),
        ] {
            enqueue(try F.data(["error": ["code": code, "message": "PRIVATE DIAGNOSTIC MUST NOT ESCAPE"]]), status: status)
            await #expect(throws: expected) { try await client().routinePlanningWitness(F.request()) }
            #expect(!expected.localizedDescription.contains("PRIVATE DIAGNOSTIC"))
        }
    }

    @Test("witness uses sixteen MiB without widening occurrence transport")
    func independentBounds() async throws {
        let body = try RoutinePlanningWitnessValidation.encode(RoutinePlanningWitnessResponse(result: .remoteRequired(.sourceIneligible)))
        var padded = body; padded.append(Data(repeating: 32, count: RoutineOccurrenceValidation.maximumBytes + 1 - body.count))
        enqueue(padded)
        #expect(try await client().routinePlanningWitness(F.request()).result == .remoteRequired(.sourceIneligible))
        let response = try #require(HTTPURLResponse(url: URL(string: "https://api.example.com/v1/routine-occurrences/planning-witness")!, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: Self.headers))
        #expect(throws: RoutinePlanningWitnessError.tooLarge) {
            try RoutinePlanningWitnessHTTP.decode(RoutinePlanningWitnessResponse.self, response: response,
                data: Data(repeating: 32, count: RoutinePlanningWitnessValidation.maximumBytes + 1))
        }
        #expect(RoutineOccurrenceValidation.maximumBytes == 8 * 1_024 * 1_024)
    }

    @Test("privacy headers and duplicate numeric tokens cannot provide evidence")
    func privacyAndShape() async throws {
        enqueue(try F.data(F.response()), extra: ["Cache-Control": "public, max-age=60"])
        await #expect(throws: RoutinePlanningWitnessError.invalidData) { try await client().routinePlanningWitness(F.request()) }
        let raw = String(decoding: try F.data(F.response()), as: UTF8.self).replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1e0")
        enqueue(Data(raw.utf8))
        await #expect(throws: RoutinePlanningWitnessError.invalidData) { try await client().routinePlanningWitness(F.request()) }
    }

    private func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"), session: URLProtocolStub.makeSession(), bearerToken: Self.token)
    }
    private func enqueue(_ body: Data, status: Int = 200, extra: [String: String] = [:]) {
        URLProtocolStub.storage.enqueue(key: Self.token, .init(statusCode: status,
            headers: Self.headers.merging(extra) { _, value in value }, body: body))
    }
}
#endif
