import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Habit API client", .serialized)
@MainActor
struct HabitAPIClientTests {
    private static let token = "habit-api-test-token"
    private static let habitID = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")!
    private static let occurrenceID = UUID(uuidString: "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb")!
    private static let plannerOccurrenceID = UUID(uuidString: "cccccccc-3333-4333-8333-cccccccccccc")!
    private static let scheduleID = UUID(uuidString: "dddddddd-4444-4444-8444-dddddddddddd")!

    init() {
        URLProtocolStub.storage.reset(key: Self.token)
    }

    @Test("occurrence list uses the exact bounded route and joins server evidence")
    func listOccurrences() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            Self.response(Self.listEnvelope())
        )
        let client = makeClient()
        let page = try await client.habitOccurrences(
            habitID: Self.habitID,
            startDate: DayWeaveLocalDate("2026-09-01")!,
            endDate: DayWeaveLocalDate("2026-09-07")!,
            cursor: nil,
            limit: 200
        )

        #expect(page.occurrences.count == 1)
        #expect(page.occurrences[0].id == Self.occurrenceID)
        #expect(page.occurrences[0].evidence.plannerOccurrenceID == Self.plannerOccurrenceID)
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/habits/\(Self.habitID.uuidString.lowercased())/occurrences")
        let query = try #require(URLComponents(url: request.url, resolvingAgainstBaseURL: false))
        #expect(Set(query.queryItems ?? []) == Set([
            .init(name: "start_date", value: "2026-09-01"),
            .init(name: "end_date", value: "2026-09-07"),
            .init(name: "limit", value: "200"),
        ]))
        #expect(request.headers["Cache-Control"] == "no-store")
        #expect(request.headers["Authorization"] == "Bearer \(Self.token)")
    }

    @Test("skipped outcomes preserve partial evidence and send one idempotency key")
    func skippedEvidenceMutation() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            Self.response(
                Data("{\"occurrence\":\(Self.occurrence(outcome: Self.skippedOutcome)),\"replayed\":false}".utf8),
                replayed: false
            )
        )
        let occurredAt = Self.date("2026-09-04T12:30:00.000000Z")
        let command = DayWeaveHabitOutcomeCommand(
            operationID: UUID(uuidString: "eeeeeeee-5555-4555-8555-eeeeeeeeeeee")!,
            expectedRevision: 0,
            outcome: .init(
                status: .skipped,
                progressBasisPoints: 2_500,
                quantity: 5,
                unit: "pages",
                actualSeconds: 600,
                note: "Stopped early",
                occurredAt: occurredAt
            )
        )

        let result = try await makeClient().putHabitOutcome(
            habitID: Self.habitID,
            occurrenceID: Self.occurrenceID,
            command: command,
            idempotencyKey: "habit-operation:test-001"
        )

        #expect(result.occurrence.outcome?.status == .skipped)
        #expect(result.occurrence.outcome?.quantity == 5)
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "PUT")
        #expect(request.headers["Idempotency-Key"] == "habit-operation:test-001")
        let body = try #require(request.jsonBody)
        #expect((body["expected_revision"] as? NSNumber)?.uint64Value == 0)
        let outcome = try #require(body["outcome"] as? [String: Any])
        #expect(outcome["status"] as? String == "skipped")
        #expect((outcome["progress_basis_points"] as? NSNumber)?.intValue == 2_500)
        #expect(outcome["note"] as? String == "Stopped early")
    }

    @Test("delta decodes only the frozen internally tagged change set")
    func deltaContract() async throws {
        let body = Data(
            "{\"changes\":[{\"type\":\"occurrence_upsert\",\"occurrence\":\(Self.occurrence(outcome: "null"))}],\"next_cursor\":\"aGVhZDox\",\"has_more\":false}".utf8
        )
        URLProtocolStub.storage.enqueue(key: Self.token, Self.response(body))

        let page = try await makeClient().habitDelta(cursor: nil, limit: 100)

        #expect(page.nextCursor == "aGVhZDox")
        guard case let .occurrenceUpsert(value) = try #require(page.changes.first) else {
            Issue.record("Expected an occurrence upsert")
            return
        }
        #expect(value.id == Self.occurrenceID)
    }

    @Test("response echo accepts the exact transmitted microsecond for a finer in-memory Date")
    func submicrosecondOutcomeEcho() async throws {
        let occurredAt = Date(timeIntervalSince1970: 1_788_527_800.123_456_7)
        let wireDate = try #require(CanonicalRFC3339Instant(date: occurredAt)?.canonicalUTCString)
        let outcome = """
        {"revision":1,"status":"completed","progress_basis_points":10000,"quantity":null,"unit":null,"actual_seconds":null,"note":null,"occurred_at":"\(wireDate)","updated_at":"\(wireDate)"}
        """
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            Self.response(
                Data("{\"occurrence\":\(Self.occurrence(outcome: outcome)),\"replayed\":false}".utf8),
                replayed: false
            )
        )

        let response = try await makeClient().putHabitOutcome(
            habitID: Self.habitID,
            occurrenceID: Self.occurrenceID,
            command: .init(
                operationID: UUID(uuidString: "abababab-7777-4777-8777-abababababab")!,
                expectedRevision: 0,
                outcome: .completed(occurredAt: occurredAt)
            ),
            idempotencyKey: "habit-operation:date-echo"
        )

        #expect(response.occurrence.outcome?.status == .completed)
    }

    @Test("pause and resume use ledger routes, revisions, and replay headers")
    func pauseLifecycle() async throws {
        let pauseID = UUID(uuidString: "cdcdcdcd-8888-4888-8888-cdcdcdcdcdcd")!
        let operationID = UUID(uuidString: "dededede-9999-4999-8999-dededededede")!
        let resumeOperationID = UUID(uuidString: "efefefef-aaaa-4aaa-8aaa-efefefefefef")!
        let startedAt = Date(timeIntervalSince1970: 1_788_527_800.123_456_7)
        let endedAt = Date(timeIntervalSince1970: 1_788_531_400.765_432_1)
        let startedWire = try #require(CanonicalRFC3339Instant(date: startedAt)?.canonicalUTCString)
        let endedWire = try #require(CanonicalRFC3339Instant(date: endedAt)?.canonicalUTCString)
        let startBody = Data("""
        {"pause":{"id":"\(pauseID.uuidString.lowercased())","habit_id":"\(Self.habitID.uuidString.lowercased())","revision":1,"started_at":"\(startedWire)","ended_at":null,"preserves_streak":true,"created_at":"\(startedWire)","updated_at":"\(startedWire)"},"replayed":false}
        """.utf8)
        let resumeBody = Data("""
        {"pause":{"id":"\(pauseID.uuidString.lowercased())","habit_id":"\(Self.habitID.uuidString.lowercased())","revision":2,"started_at":"\(startedWire)","ended_at":"\(endedWire)","preserves_streak":true,"created_at":"\(startedWire)","updated_at":"\(endedWire)"},"replayed":false}
        """.utf8)
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            Self.response(startBody, replayed: false),
            Self.response(resumeBody, replayed: false)
        )
        let client = makeClient()

        let started = try await client.startHabitPause(
            habitID: Self.habitID,
            command: .init(
                operationID: operationID,
                pauseID: pauseID,
                startedAt: startedAt
            ),
            idempotencyKey: "habit-pause:start-test"
        )
        let resumed = try await client.resumeHabitPause(
            habitID: Self.habitID,
            pauseID: pauseID,
            command: .init(
                operationID: resumeOperationID,
                expectedRevision: started.pause.revision,
                endedAt: endedAt
            ),
            idempotencyKey: "habit-pause:resume-test"
        )

        #expect(resumed.pause.endedAt != nil)
        let requests = URLProtocolStub.storage.requests(for: Self.token)
        #expect(requests.map(\.url.path) == [
            "/gateway/v1/habits/\(Self.habitID.uuidString.lowercased())/pauses",
            "/gateway/v1/habits/\(Self.habitID.uuidString.lowercased())/pauses/\(pauseID.uuidString.lowercased())/resume",
        ])
    }

    @Test("analytics unwraps deterministic totals and supportive fact codes")
    func analyticsContract() async throws {
        URLProtocolStub.storage.enqueue(key: Self.token, Self.response(Self.analyticsEnvelope()))

        let analytics = try await makeClient().habitAnalytics(
            habitID: Self.habitID,
            startDate: DayWeaveLocalDate("2026-09-01")!,
            endDate: DayWeaveLocalDate("2026-09-30")!,
            bucket: .week
        )

        #expect(analytics.totals.adherenceBasisPoints == 8_125)
        #expect(analytics.currentStreak == 2)
        #expect(analytics.supportiveFactCodes == [.activeStreak, .strongAdherence])
    }

    @Test("habit success without exact privacy headers fails closed")
    func missingPrivacyHeaders() async {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 200, headers: ["Content-Type": "application/json"], body: Self.listEnvelope())
        )
        await #expect(throws: DayWeaveAPIError.responseDecodingFailed) {
            try await makeClient().habitOccurrences(
                habitID: Self.habitID,
                startDate: DayWeaveLocalDate("2026-09-01")!,
                endDate: DayWeaveLocalDate("2026-09-07")!,
                cursor: nil,
                limit: 100
            )
        }
    }

    @Test("inclusive occurrence and analytics ranges are bounded to 366 days locally")
    func boundedDateRanges() async {
        let client = makeClient()
        await #expect(throws: DayWeaveAPIError.requestEncodingFailed) {
            try await client.habitOccurrences(
                habitID: Self.habitID,
                startDate: DayWeaveLocalDate("2025-09-04")!,
                endDate: DayWeaveLocalDate("2026-09-05")!,
                cursor: nil,
                limit: 100
            )
        }
        await #expect(throws: DayWeaveAPIError.requestEncodingFailed) {
            try await client.habitAnalytics(
                habitID: Self.habitID,
                startDate: DayWeaveLocalDate("2025-09-04")!,
                endDate: DayWeaveLocalDate("2026-09-05")!,
                bucket: .month
            )
        }
        #expect(URLProtocolStub.storage.requests(for: Self.token).isEmpty)
    }

    @Test("habit errors require the same private, strict JSON response contract")
    func strictErrorResponse() async {
        let body = Data("{\"error\":{\"code\":\"conflict\",\"message\":\"stale\"}}".utf8)
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 409, headers: ["Content-Type": "application/json"], body: body)
        )
        await #expect(throws: DayWeaveAPIError.responseDecodingFailed) {
            try await makeClient().putHabitOutcome(
                habitID: Self.habitID,
                occurrenceID: Self.occurrenceID,
                command: .init(
                    operationID: UUID(uuidString: "ffffffff-6666-4666-8666-ffffffffffff")!,
                    expectedRevision: 0,
                    outcome: .completed(occurredAt: Self.date("2026-09-04T12:30:00.000000Z"))
                ),
                idempotencyKey: "habit-operation:test-error"
            )
        }
    }

    @Test("unknown or duplicate habit response fields are rejected")
    func strictResponseShapes() async {
        let unknown = Data(
            String(decoding: Self.listEnvelope(), as: UTF8.self)
                .replacingOccurrences(of: "\"has_more\":false", with: "\"has_more\":false,\"future\":true")
                .utf8
        )
        let duplicate = Data(
            String(decoding: Self.listEnvelope(), as: UTF8.self)
                .replacingOccurrences(of: "\"has_more\":false", with: "\"has_more\":false,\"has_more\":false")
                .utf8
        )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            Self.response(unknown),
            Self.response(duplicate)
        )
        for _ in 0..<2 {
            await #expect(throws: DayWeaveAPIError.responseDecodingFailed) {
                try await makeClient().habitOccurrences(
                    habitID: Self.habitID,
                    startDate: DayWeaveLocalDate("2026-09-01")!,
                    endDate: DayWeaveLocalDate("2026-09-07")!,
                    cursor: nil,
                    limit: 100
                )
            }
        }
    }

    private func makeClient() -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: Self.token
        )
    }

    private static func response(_ body: Data, replayed: Bool? = nil) -> URLProtocolStub.Response {
        var headers = [
            "Content-Type": "application/json",
            "Cache-Control": "no-store, max-age=0",
            "Pragma": "no-cache",
        ]
        if let replayed { headers["idempotency-replayed"] = replayed ? "true" : "false" }
        return .init(statusCode: 200, headers: headers, body: body)
    }

    private static func listEnvelope() -> Data {
        Data("{\"occurrences\":[\(occurrence(outcome: "null"))],\"next_cursor\":null,\"has_more\":false}".utf8)
    }

    private static func occurrence(outcome: String) -> String {
        "{\"evidence\":\(evidence),\"outcome\":\(outcome)}"
    }

    private static let evidence = """
    {"id":"\(occurrenceID.uuidString.lowercased())","habit_id":"\(habitID.uuidString.lowercased())","planner_occurrence_id":"\(plannerOccurrenceID.uuidString.lowercased())","source_schedule_revision_id":"\(scheduleID.uuidString.lowercased())","source_item_revision":3,"policy_fingerprint":"sha256:\(String(repeating: "a", count: 64))","identity":{},"nominal_start":"2026-09-04T12:00:00.000000Z","nominal_end":"2026-09-04T13:00:00.000000Z","window_start":"2026-09-04T11:00:00.000000Z","window_end":"2026-09-04T14:00:00.000000Z","local_date":"2026-09-04","timezone_name":"Europe/Paris","expected_duration_seconds":3600,"expected_quantity":20,"expected_unit":"pages"}
    """

    private static let skippedOutcome = """
    {"revision":1,"status":"skipped","progress_basis_points":2500,"quantity":5,"unit":"pages","actual_seconds":600,"note":"Stopped early","occurred_at":"2026-09-04T12:30:00.000000Z","updated_at":"2026-09-04T12:31:00.000000Z"}
    """

    private static func analyticsEnvelope() -> Data {
        Data("""
        {"analytics":{"habit_id":"\(habitID.uuidString.lowercased())","start_date":"2026-09-01","end_date":"2026-09-30","bucket":"week","expected":5,"eligible":4,"completed":3,"partial":1,"skipped":0,"missed":0,"excused":1,"unresolved":0,"adherence_basis_points":8125,"actual_seconds_total":7200,"quantity_totals":[{"unit":"pages","amount":60}],"current_streak":2,"longest_streak":5,"trends":[{"start_date":"2026-09-01","end_date":"2026-09-07","expected":5,"eligible":4,"completed":3,"partial":1,"skipped":0,"missed":0,"excused":1,"unresolved":0,"adherence_basis_points":8125,"actual_seconds_total":7200,"quantity_totals":[{"unit":"pages","amount":60}]}],"supportive_fact_codes":["active_streak","strong_adherence"]}}
        """.utf8)
    }

    private static func date(_ text: String) -> Date {
        CanonicalRFC3339Instant(text)!.exactlyRepresentableDate!
    }
}
#endif
