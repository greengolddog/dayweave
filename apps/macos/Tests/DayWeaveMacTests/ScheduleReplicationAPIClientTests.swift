import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Published schedule native replication API", .serialized)
struct ScheduleReplicationAPIClientTests {
    private static let token = "schedule-replica-api-token"

    init() {
        URLProtocolStub.storage.reset(key: Self.token)
    }

    @Test("current publication requires the exact no-store JSON envelope")
    func currentScheduleContract() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(Self.currentScheduleJSON.utf8)
            )
        )

        let current = try #require(try await Self.client().currentPublishedSchedule())

        #expect(current.revision.revisionNumber == 7)
        #expect(current.schedule.manualPlacementAssessments.isEmpty)
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/schedule/current")
        #expect(request.headers["Accept"] == "application/json")
        #expect(request.headers["Cache-Control"] == "no-store")
        #expect(request.headers["Pragma"] == "no-cache")
        #expect(request.headers["Authorization"] == "Bearer \(Self.token)")
    }

    @Test("only the exact typed non-cacheable absence becomes nil")
    func exactAbsenceContract() async throws {
        let body = Data(
            #"{"error":{"code":"not_found","message":"Published schedule was not found"}}"#.utf8
        )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 404, headers: Self.cacheJSONHeaders, body: body),
            .init(
                statusCode: 404,
                headers: ["Content-Type": "application/json"],
                body: body
            )
        )

        #expect(try await Self.client().currentPublishedSchedule() == nil)
        do {
            _ = try await Self.client().currentPublishedSchedule()
            Issue.record("A cacheable 404 must not clear an encrypted projection")
        } catch let error as DayWeaveAPIError {
            guard case let .server(statusCode, _, _, _) = error else {
                Issue.record("Unexpected API error: \(error)")
                return
            }
            #expect(statusCode == 404)
        }
    }

    @Test("duplicate or widened current JSON fails closed")
    func strictCurrentShape() async {
        let widened = Self.currentScheduleJSON.replacingOccurrences(
            of: #""schedule":{"#,
            with: #""future":true,"schedule":{"#
        )
        let duplicate = Self.currentScheduleJSON.replacingOccurrences(
            of: #""revision_number":7"#,
            with: #""revision_number":7,"revision_number":7"#
        )
        let noncanonicalEmptyAssessments = Self.currentScheduleJSON.replacingOccurrences(
            of: #""ignored_previous_assignments":[]"#,
            with: #""ignored_previous_assignments":[],"manual_placement_assessments":[]"#
        )
        for body in [widened, duplicate, noncanonicalEmptyAssessments] {
            URLProtocolStub.storage.enqueue(
                key: Self.token,
                .init(statusCode: 200, headers: Self.currentJSONHeaders, body: Data(body.utf8))
            )
            do {
                _ = try await Self.client().currentPublishedSchedule()
                Issue.record("Expanded or ambiguous current JSON must fail closed")
            } catch let error as DayWeaveAPIError {
                #expect(error == .responseDecodingFailed)
            } catch {
                Issue.record("Unexpected error: \(error)")
            }
        }
    }

    @Test("current publication recursively validates decisions, violations, and assessments")
    func strictNestedScheduleEvidence() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(Self.currentScheduleWithEvidenceJSON.utf8)
            )
        )
        let current = try #require(try await Self.client().currentPublishedSchedule())
        #expect(current.schedule.plan.decisions.count == 1)
        #expect(current.schedule.plan.violations.count == 1)
        #expect(current.schedule.manualPlacementAssessments.count == 1)

        let exactExternalConflict = Self.currentScheduleWithEvidenceJSON.replacingOccurrences(
            of: #""block_id":"40000000-0000-0000-0000-000000000001","item_id":"20000000-0000-0000-0000-000000000001","occurrence_id":"30000000-0000-0000-0000-000000000001","external_block_id":null,"kind":"pinned""#,
            with: #""block_id":"40000000-0000-0000-0000-000000000001","item_id":null,"occurrence_id":null,"external_block_id":"40000000-0000-0000-0000-000000000001","kind":"external_fixed""#
        )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(exactExternalConflict.utf8)
            )
        )
        #expect(try await Self.client().currentPublishedSchedule() != nil)

        let approvedEvidence = Self.currentScheduleWithEvidenceJSON.replacingOccurrences(
            of: #""approval_required":true"#,
            with: #""approval_required":false"#
        )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(approvedEvidence.utf8)
            )
        )
        #expect(try await Self.client().currentPublishedSchedule() != nil)

        let largePenalty = Self.currentScheduleWithEvidenceJSON
            .replacingOccurrences(of: #""penalty":5"#, with: #""penalty":9223372036854775808"#)
            .replacingOccurrences(
                of: #""soft_penalty":5"#,
                with: #""soft_penalty":9223372036854775808"#
            )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(largePenalty.utf8)
            )
        )
        #expect(try await Self.client().currentPublishedSchedule() != nil)

        let hostileBodies = [
            Self.replacingEvidence(
                #""kind":"scheduled","message":"Placed""#,
                with: #""kind":"invented","message":"Placed""#
            ),
            Self.replacingEvidence(
                #""kind":"scheduled","message":"Placed""#,
                with: #""kind":"scheduled","future":true,"message":"Placed""#
            ),
            Self.replacingEvidence(
                #""item_id":"20000000-0000-0000-0000-000000000001","occurrence_id":"30000000-0000-0000-0000-000000000001","kind":"scheduled""#,
                with: #""item_id":"20000000-0000-0000-0000-000000000099","occurrence_id":"30000000-0000-0000-0000-000000000001","kind":"scheduled""#
            ),
            Self.replacingEvidence(
                #""severity":"warning""#,
                with: #""severity":"notice""#
            ),
            Self.replacingEvidence(
                #""severity":"warning","item_ids""#,
                with: #""severity":"warning","future":true,"item_ids""#
            ),
            Self.replacingEvidence(
                #""penalty":5"#,
                with: #""penalty":false"#
            ),
            Self.replacingEvidence(
                #""penalty":5"#,
                with: #""penalty":18446744073709551616"#
            ),
            Self.replacingEvidence(
                #""penalty":5"#,
                with: #""penalty":1.5"#
            ),
            Self.replacingEvidence(
                #""start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z","penalty":5"#,
                with: #""start":"not-a-timestamp","end":"2027-01-15T09:30:00Z","penalty":5"#
            ),
            Self.replacingEvidence(
                #""start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z","penalty":5"#,
                with: #""start":"2027-01-15T09:00:00Z","end":null,"penalty":5"#
            ),
            Self.replacingEvidence(
                #""approval_digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc""#,
                with: #""approval_digest":"sha256:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC""#
            ),
            Self.replacingEvidence(
                #""placement_id":"50000000-0000-0000-0000-000000000001""#,
                with: #""placement_id":"not-a-uuid""#
            ),
            Self.replacingEvidence(
                #""approval_required":true,"violations""#,
                with: #""approval_required":true,"future":true,"violations""#
            ),
            Self.replacingEvidence(
                #""code":"immutable_overlap""#,
                with: #""code":"future_conflict""#
            ),
            Self.replacingEvidence(
                #""conflicting_block_ids":["40000000-0000-0000-0000-000000000001"]"#,
                with: #""conflicting_block_ids":["40000000-0000-0000-0000-000000000099"]"#
            ),
            Self.replacingEvidence(
                #""kind":"pinned","start":"2027-01-15T09:00:00Z""#,
                with: #""kind":"pinned","future":true,"start":"2027-01-15T09:00:00Z""#
            ),
            Self.replacingEvidence(
                #""block_id":"40000000-0000-0000-0000-000000000001","item_id":"20000000-0000-0000-0000-000000000001","occurrence_id":"30000000-0000-0000-0000-000000000001","external_block_id":null,"kind":"pinned""#,
                with: #""block_id":"40000000-0000-0000-0000-000000000001","item_id":null,"occurrence_id":null,"external_block_id":"40000000-0000-0000-0000-000000000002","kind":"external_fixed""#
            ),
            Self.replacingEvidence(
                #""approval_required":true"#,
                with: #""approval_required":1"#
            ),
            Self.replacingEvidence(
                #""boundary_start":null"#,
                with: #""boundary_start":42"#
            ),
            try Self.currentEvidenceWithEmptyAssessmentViolations(),
        ]
        for body in hostileBodies {
            await Self.expectCurrentScheduleRejected(body)
        }
    }

    @Test("current publication validates explanations and non-plan item evidence")
    func strictAncillaryScheduleEvidence() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(Self.currentScheduleWithAncillaryEvidenceJSON.utf8)
            )
        )
        let current = try #require(try await Self.client().currentPublishedSchedule())
        #expect(current.schedule.rejectedItems.count == 1)
        #expect(current.schedule.ignoredPreviousAssignments.count == 1)
        #expect(current.schedule.plan.unscheduled.count == 1)
        #expect(current.schedule.plan.blocks.first?.explanations.count == 1)

        let largeRemaining = Self.currentScheduleWithAncillaryEvidenceJSON
            .replacingOccurrences(
                of: #""remaining":15,"reason":"no_capacity""#,
                with: #""remaining":200000,"reason":"no_capacity""#
            )
            .replacingOccurrences(
                of: #""unscheduled_minutes":15"#,
                with: #""unscheduled_minutes":200000"#
            )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.currentJSONHeaders,
                body: Data(largeRemaining.utf8)
            )
        )
        #expect(try await Self.client().currentPublishedSchedule() != nil)

        let repeatedExplanations = "[" + Array(
            repeating: #"{"code":"priority","message":"Priority fit"}"#,
            count: 65
        ).joined(separator: ",") + "]"
        let tooLongTitle = String(repeating: "t", count: 501)
        let tooLongMessage = String(repeating: "m", count: 4_001)
        let unscheduled = #"{"item_id":"20000000-0000-0000-0000-000000000002","occurrence_id":null,"remaining":15,"reason":"no_capacity","message":"Needs capacity"}"#
        let hostileBodies = [
            Self.replacingAncillary(
                #""code":"priority","message":"Priority fit""#,
                with: #""code":"future_reason","message":"Priority fit""#
            ),
            Self.replacingAncillary(
                #""message":"Priority fit""#,
                with: #""message":"Priority\u0000fit""#
            ),
            Self.replacingAncillary(
                #""message":"Priority fit""#,
                with: #""message":"\#(tooLongMessage)""#
            ),
            Self.replacingAncillary(
                #"[{"code":"priority","message":"Priority fit"}]"#,
                with: repeatedExplanations
            ),
            Self.replacingAncillary(
                #""item_id":"20000000-0000-0000-0000-000000000003","is_sensitive":false"#,
                with: #""item_id":"20000000-0000-0000-0000-000000000099","is_sensitive":false"#
            ),
            Self.replacingAncillary(
                #""is_sensitive":false,"title":"Rejected item""#,
                with: #""is_sensitive":0,"title":"Rejected item""#
            ),
            Self.replacingAncillary(
                #""title":"Rejected item""#,
                with: #""title":"Rejected\u000aitem""#
            ),
            Self.replacingAncillary(
                #""title":"Rejected item""#,
                with: #""title":"\#(tooLongTitle)""#
            ),
            Self.replacingAncillary(
                #""reason":"Invalid constraints""#,
                with: #""reason":"\#(tooLongMessage)""#
            ),
            Self.replacingAncillary(
                #"[{"item_id":"20000000-0000-0000-0000-000000000003","is_sensitive":false,"title":"Rejected item","reason":"Invalid constraints"}]"#,
                with: #"[{"item_id":"20000000-0000-0000-0000-000000000003","is_sensitive":false,"title":"Rejected item","reason":"Invalid constraints"},{"item_id":"20000000-0000-0000-0000-000000000003","is_sensitive":false,"title":"Rejected item","reason":"Invalid constraints"}]"#
            ),
            Self.replacingAncillary(
                #""item_id":"20000000-0000-0000-0000-000000000002","requested_revision":3"#,
                with: #""item_id":"20000000-0000-0000-0000-000000000099","requested_revision":3"#
            ),
            Self.replacingAncillary(
                #""requested_revision":3"#,
                with: #""requested_revision":0"#
            ),
            Self.replacingAncillary(
                #""current_revision":null,"reason":"Assignment changed""#,
                with: #""current_revision":0,"reason":"Assignment changed""#
            ),
            Self.replacingAncillary(
                #""reason":"Assignment changed""#,
                with: #""reason":"Assignment\u0009changed""#
            ),
            Self.replacingAncillary(
                #""remaining":15,"reason":"no_capacity""#,
                with: #""remaining":4294967296,"reason":"no_capacity""#
            ),
            Self.replacingAncillary(
                #""item_id":"20000000-0000-0000-0000-000000000002","occurrence_id":null,"remaining":15"#,
                with: #""item_id":"20000000-0000-0000-0000-000000000099","occurrence_id":null,"remaining":15"#
            ),
            Self.replacingAncillary(
                #""reason":"no_capacity","message":"Needs capacity""#,
                with: #""reason":"future_reason","message":"Needs capacity""#
            ),
            Self.replacingAncillary(
                #""message":"Needs capacity""#,
                with: #""message":"Needs\u0000capacity""#
            ),
            Self.replacingAncillary(
                #""message":"Needs capacity""#,
                with: #""message":"\#(tooLongMessage)""#
            ),
            Self.replacingAncillary(
                "[\(unscheduled)]",
                with: "[\(unscheduled),\(unscheduled)]"
            ),
        ]
        for body in hostileBodies {
            await Self.expectCurrentScheduleRejected(body)
        }
    }

    @Test("current publication accepts only a known IANA timezone identifier")
    func strictReplicaTimezone() async {
        let hostile = Self.currentScheduleJSON.replacingOccurrences(
            of: #""timezone_name":"UTC""#,
            with: #""timezone_name":"GMT+01:00""#
        )
        await Self.expectCurrentScheduleRejected(hostile)
    }

    @Test("schedule stream carries the durable revision and validates hints")
    func streamContract() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: [
                    "Content-Type": "text/event-stream; charset=utf-8",
                    "Cache-Control": "no-store, no-cache",
                    "Pragma": "no-cache",
                    "X-Accel-Buffering": "no",
                    "Content-Encoding": "identity",
                ],
                body: Data(
                    ": heartbeat\n\nid: 8\nevent: schedule-invalidation\ndata: {\"revision\":8}\n\n".utf8
                )
            )
        )
        let recorder = ScheduleRevisionRecorder()

        let completion = try await Self.client().consumeScheduleInvalidations(after: 7) {
            await recorder.append($0)
        }

        #expect(completion == .liveEndOfStream)
        #expect(await recorder.values == [8])
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.url.path == "/gateway/v1/schedule/stream")
        #expect(request.headers["Accept"] == "text/event-stream")
        #expect(request.headers["Last-Event-ID"] == "7")
    }

    @Test("cursor-ahead recovery trusts only the exact conflict contract")
    func cursorAheadContract() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 409,
                headers: Self.cacheJSONHeaders,
                body: Data(
                    #"{"error":{"code":"conflict","message":"schedule stream cursor is ahead of authoritative state","details":{"cursor_revision":9,"head_revision":4}}}"#.utf8
                )
            )
        )

        let completion = try await Self.client().consumeScheduleInvalidations(after: 9) { _ in
            Issue.record("A conflict response cannot emit a hint")
        }
        #expect(completion == .cursorAhead(headRevision: 4))
    }

    @Test("schedule parser rejects execution events and nonmonotonic revisions")
    func scheduleParserIsNarrow() throws {
        var parser = DayWeaveExecutionSSEParser(
            after: 4,
            expectedEventName: "schedule-invalidation"
        )
        let wrong = Data(
            "id: 5\nevent: execution-invalidation\ndata: {\"revision\":5}\n\n".utf8
        )
        #expect(throws: DayWeaveExecutionStreamProtocolError.invalidEvent) {
            for byte in wrong { _ = try parser.consume(byte) }
        }
    }

    private static let cacheJSONHeaders = [
        "Content-Type": "application/json; charset=utf-8",
        "Cache-Control": "no-store, max-age=0",
        "Pragma": "no-cache",
    ]
    private static let currentJSONHeaders = [
        "Content-Type": "application/json; charset=utf-8",
        "Cache-Control": "no-store, max-age=0",
        "Pragma": "no-cache",
        "ETag": "\"7:10000000-0000-0000-0000-000000000007\"",
    ]

    private static let revisionID = "10000000-0000-0000-0000-000000000007"
    private static let digest = "sha256:" + String(repeating: "b", count: 64)
    private static let currentScheduleJSON = """
    {"revision":{"id":"\(revisionID)","revision":"7:\(revisionID)","revision_number":7,"input_digest":"\(digest)","horizon_start":"2027-01-15T00:00:00Z","horizon_end":"2027-01-16T00:00:00Z","timezone_name":"UTC","published_at":"2027-01-15T08:00:00Z"},"schedule":{"input_digest":"\(digest)","source_item_count":0,"accepted_item_count":0,"source_item_revisions":{},"rejected_items":[],"ignored_previous_assignments":[],"plan":{"as_of":"2027-01-15T08:00:00Z","horizon_start":"2027-01-15T00:00:00Z","horizon_end":"2027-01-16T00:00:00Z","blocks":[],"unscheduled":[],"decisions":[],"violations":[],"score":{"scheduled_minutes":0,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}}
    """

    private static let currentScheduleWithEvidenceJSON = """
    {"revision":{"id":"\(revisionID)","revision":"7:\(revisionID)","revision_number":7,"input_digest":"\(digest)","horizon_start":"2027-01-15T00:00:00Z","horizon_end":"2027-01-16T00:00:00Z","timezone_name":"UTC","published_at":"2027-01-15T08:00:00Z"},"schedule":{"input_digest":"\(digest)","source_item_count":1,"accepted_item_count":1,"source_item_revisions":{"20000000-0000-0000-0000-000000000001":1},"rejected_items":[],"ignored_previous_assignments":[],"manual_placement_assessments":[{"placement_id":"50000000-0000-0000-0000-000000000001","environment_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","approval_digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","approval_required":true,"violations":[{"code":"immutable_overlap","item_ids":["20000000-0000-0000-0000-000000000001"],"occurrence_ids":["30000000-0000-0000-0000-000000000001"],"conflicting_block_ids":["40000000-0000-0000-0000-000000000001"],"conflicting_blocks":[{"block_id":"40000000-0000-0000-0000-000000000001","item_id":"20000000-0000-0000-0000-000000000001","occurrence_id":"30000000-0000-0000-0000-000000000001","external_block_id":null,"kind":"pinned","start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z"}],"start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z","boundary_start":null,"boundary_end":null,"message":"Pinned work overlaps"}]}],"plan":{"as_of":"2027-01-15T08:00:00Z","horizon_start":"2027-01-15T00:00:00Z","horizon_end":"2027-01-16T00:00:00Z","blocks":[],"unscheduled":[],"decisions":[{"item_id":"20000000-0000-0000-0000-000000000001","occurrence_id":"30000000-0000-0000-0000-000000000001","kind":"scheduled","message":"Placed"}],"violations":[{"kind":"soft_constraint","severity":"warning","item_ids":["20000000-0000-0000-0000-000000000001"],"occurrence_ids":["30000000-0000-0000-0000-000000000001"],"start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z","penalty":5,"message":"Outside preference"}],"score":{"scheduled_minutes":0,"unscheduled_minutes":0,"soft_penalty":5,"moved_minutes":0},"occurrences":[{"id":"30000000-0000-0000-0000-000000000001","series_item_id":"20000000-0000-0000-0000-000000000001","identity":{"type":"calendar_day","date":"2027-01-15","bucket_ordinal":0},"nominal_start":"2027-01-15T09:00:00Z","nominal_end":"2027-01-15T09:30:00Z","window_start":"2027-01-15T09:00:00Z","window_end":"2027-01-15T09:30:00Z","local_date":"2027-01-15","ordinal":0,"state":"generated"}]}}}
    """

    private static let currentScheduleWithAncillaryEvidenceJSON =
        currentScheduleWithEvidenceJSON
        .replacingOccurrences(
            of: #""source_item_count":1,"accepted_item_count":1,"source_item_revisions":{"20000000-0000-0000-0000-000000000001":1}"#,
            with: #""source_item_count":3,"accepted_item_count":2,"source_item_revisions":{"20000000-0000-0000-0000-000000000001":1,"20000000-0000-0000-0000-000000000002":2,"20000000-0000-0000-0000-000000000003":3}"#
        )
        .replacingOccurrences(
            of: #""rejected_items":[]"#,
            with: #""rejected_items":[{"item_id":"20000000-0000-0000-0000-000000000003","is_sensitive":false,"title":"Rejected item","reason":"Invalid constraints"}]"#
        )
        .replacingOccurrences(
            of: #""ignored_previous_assignments":[]"#,
            with: #""ignored_previous_assignments":[{"item_id":"20000000-0000-0000-0000-000000000002","requested_revision":3,"current_revision":null,"reason":"Assignment changed"}]"#
        )
        .replacingOccurrences(
            of: #""blocks":[],"unscheduled":[]"#,
            with: #""blocks":[{"id":"60000000-0000-0000-0000-000000000001","is_sensitive":false,"item_id":"20000000-0000-0000-0000-000000000001","occurrence_id":"30000000-0000-0000-0000-000000000001","external_block_id":null,"title":"Scheduled item","start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z","session_index":0,"kind":"planned","explanations":[{"code":"priority","message":"Priority fit"}]}],"unscheduled":[{"item_id":"20000000-0000-0000-0000-000000000002","occurrence_id":null,"remaining":15,"reason":"no_capacity","message":"Needs capacity"}]"#
        )
        .replacingOccurrences(
            of: #""score":{"scheduled_minutes":0,"unscheduled_minutes":0,"soft_penalty":5,"moved_minutes":0}"#,
            with: #""score":{"scheduled_minutes":30,"unscheduled_minutes":15,"soft_penalty":5,"moved_minutes":0}"#
        )

    private static func replacingEvidence(_ target: String, with replacement: String) -> String {
        let result = currentScheduleWithEvidenceJSON.replacingOccurrences(
            of: target,
            with: replacement
        )
        precondition(result != currentScheduleWithEvidenceJSON, "Test mutation did not match")
        return result
    }

    private static func replacingAncillary(_ target: String, with replacement: String) -> String {
        let result = currentScheduleWithAncillaryEvidenceJSON.replacingOccurrences(
            of: target,
            with: replacement
        )
        precondition(
            result != currentScheduleWithAncillaryEvidenceJSON,
            "Ancillary test mutation did not match"
        )
        return result
    }

    private static func currentEvidenceWithEmptyAssessmentViolations() throws -> String {
        var outer = try #require(
            JSONSerialization.jsonObject(
                with: Data(currentScheduleWithEvidenceJSON.utf8)
            ) as? [String: Any]
        )
        var schedule = try #require(outer["schedule"] as? [String: Any])
        var assessments = try #require(
            schedule["manual_placement_assessments"] as? [[String: Any]]
        )
        assessments[0]["violations"] = []
        schedule["manual_placement_assessments"] = assessments
        outer["schedule"] = schedule
        let data = try JSONSerialization.data(withJSONObject: outer, options: [.sortedKeys])
        return try #require(String(data: data, encoding: .utf8))
    }

    private static func expectCurrentScheduleRejected(_ body: String) async {
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(statusCode: 200, headers: currentJSONHeaders, body: Data(body.utf8))
        )
        do {
            _ = try await client().currentPublishedSchedule()
            Issue.record("Malformed nested current-schedule evidence must fail closed")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    private static func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        )
    }
}

private actor ScheduleRevisionRecorder {
    private(set) var values: [UInt64] = []
    func append(_ revision: UInt64) { values.append(revision) }
}
#endif
