import Foundation
import Testing
@testable import DayWeaveMac

@Suite("Server-authoritative execution API", .serialized)
@MainActor
struct ExecutionAPIClientTests {
    private static let token = "execution-transport-test-token"
    private static let sessionID = UUID(uuidString: "10000000-0000-4000-8000-000000000001")!
    private static let itemID = UUID(uuidString: "20000000-0000-4000-8000-000000000002")!
    private static let blockID = UUID(uuidString: "30000000-0000-4000-8000-000000000003")!
    private static let deviceID = UUID(uuidString: "40000000-0000-4000-8000-000000000004")!

    init() {
        URLProtocolStub.storage.reset(key: Self.token)
    }

    @Test("snapshot and bounded history decode complete immutable sessions")
    func snapshotAndHistoryContracts() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":4,"active_session":\#(Self.session(status: "paused", revision: 2))}}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"sessions":[\#(Self.session(status: "completed", revision: 3))]}"#.utf8)
            )
        )
        let client = Self.client()

        let snapshot = try await client.executionSnapshot()
        let history = try await client.executionHistory(limit: 37)

        #expect(snapshot.revision == 4)
        #expect(snapshot.activeSession?.id == Self.sessionID)
        #expect(snapshot.activeSession?.status == .paused)
        #expect(snapshot.activeSession?.pauseReason == "Tea")
        #expect(history.count == 1)
        #expect(history[0].status == .completed)
        #expect(history[0].actualSeconds == 1_234)

        let requests = URLProtocolStub.storage.requests(for: Self.token)
        #expect(requests.map(\.method) == ["GET", "GET"])
        #expect(requests[0].url.path == "/gateway/v1/execution")
        #expect(requests[1].url.path == "/gateway/v1/execution/history")
        let query = try #require(URLComponents(
            url: requests[1].url,
            resolvingAgainstBaseURL: false
        ))
        #expect(query.queryItems == [URLQueryItem(name: "limit", value: "37")])
        #expect(requests.allSatisfy { $0.headers["Authorization"] == "Bearer \(Self.token)" })
    }

    @Test("commands preserve one deterministic body and idempotency key")
    func deterministicCommandBodiesAndReplay() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "active",
                    globalRevision: 1,
                    sessionRevision: 1,
                    replayed: false
                )
            ),
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "active",
                    globalRevision: 1,
                    sessionRevision: 1,
                    replayed: true
                )
            ),
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "paused",
                    globalRevision: 5,
                    sessionRevision: 2,
                    replayed: false
                )
            ),
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "active",
                    globalRevision: 6,
                    sessionRevision: 3,
                    replayed: false
                )
            )
        )
        let client = Self.client()
        let request = DayWeaveExecutionCommandRequest(
            expectedRevision: 0,
            command: .start(
                sessionID: Self.sessionID,
                itemID: Self.itemID,
                itemRevision: 9,
                occurrenceID: nil,
                sessionIndex: 0,
                plannedBlockID: Self.blockID,
                deviceID: Self.deviceID
            )
        )
        let durableBody = try client.encodedExecutionCommand(request)
        let repeatedBody = try client.encodedExecutionCommand(request)
        #expect(durableBody == repeatedBody)

        let first = try await client.applyExecutionCommand(
            encodedRequest: durableBody,
            idempotencyKey: "mac-execution-start-0001"
        )
        let replay = try await client.applyExecutionCommand(
            encodedRequest: durableBody,
            idempotencyKey: "mac-execution-start-0001"
        )
        _ = try await client.applyExecutionCommand(
            .init(
                expectedRevision: 4,
                command: .pause(
                    sessionID: Self.sessionID,
                    durationSeconds: 900,
                    pauseUntil: nil,
                    reason: "Tea"
                )
            ),
            idempotencyKey: "mac-execution-pause-0001"
        )
        let resumed = try await client.applyExecutionCommand(
            .init(
                expectedRevision: 5,
                command: .resume(sessionID: Self.sessionID)
            ),
            idempotencyKey: "mac-execution-resume-0001"
        )

        #expect(!first.replayed)
        #expect(replay.replayed)
        #expect(first.changedSession.id == Self.sessionID)
        #expect(resumed.changedSession.status == .active)
        #expect(resumed.changedSession.runningSince == resumed.changedSession.updatedAt)
        let requests = URLProtocolStub.storage.requests(for: Self.token)
        #expect(requests.count == 4)
        #expect(requests.allSatisfy { $0.url.path == "/gateway/v1/execution/commands" })
        #expect(requests[0].body == durableBody)
        #expect(requests[1].body == durableBody)
        #expect(requests[0].headers["Idempotency-Key"] == "mac-execution-start-0001")
        #expect(requests[1].headers["Idempotency-Key"] == "mac-execution-start-0001")

        let start = try #require(requests[0].jsonBody)
        #expect((start["expected_revision"] as? NSNumber)?.uint64Value == 0)
        let startCommand = try #require(start["command"] as? [String: Any])
        #expect(startCommand["type"] as? String == "start")
        #expect(startCommand["item_revision"] as? NSNumber == 9)
        #expect(startCommand["planned_block_id"] as? String == Self.blockID.uuidString)
        #expect(startCommand["occurrence_id"] == nil)

        let pause = try #require(requests[2].jsonBody?["command"] as? [String: Any])
        #expect(pause["type"] as? String == "pause")
        #expect((pause["duration_seconds"] as? NSNumber)?.uint32Value == 900)
        #expect(pause["reason"] as? String == "Tea")
        #expect(pause["pause_until"] == nil)
    }

    @Test("unknown or incomplete execution state fails closed")
    func strictResponseContract() async throws {
        let future = Self.session(status: "active", revision: 1)
            .dropLast()
            + #","future_lease":true}"#
        let futureBody = Data(
            #"{"execution":{"revision":1,"active_session":\#(future)}}"#.utf8
        )
        _ = try JSONSerialization.jsonObject(with: futureBody)
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: futureBody
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":1}}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":18446744073709551615,"active_session":null}}"#.utf8)
            )
        )
        let client = Self.client()

        await Self.expectDecodingFailure { try await client.executionSnapshot() }
        await Self.expectDecodingFailure { try await client.executionSnapshot() }
        await Self.expectDecodingFailure { try await client.executionSnapshot() }
    }

    @Test("invalid history limits fail before transport")
    func invalidHistoryLimit() async throws {
        let client = Self.client()
        do {
            _ = try await client.executionHistory(limit: 101)
            Issue.record("Expected the client-side history bound to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        #expect(URLProtocolStub.storage.requests(for: Self.token).isEmpty)
    }

    @Test("malformed commands and idempotency keys fail before transport")
    func invalidCommandsFailLocally() async throws {
        let client = Self.client()
        let invalid = DayWeaveExecutionCommandRequest(
            expectedRevision: 0,
            command: .start(
                sessionID: Self.sessionID,
                itemID: Self.itemID,
                itemRevision: 0,
                occurrenceID: nil,
                sessionIndex: 0,
                plannedBlockID: Self.blockID,
                deviceID: Self.deviceID
            )
        )
        do {
            _ = try client.encodedExecutionCommand(invalid)
            Issue.record("Expected an invalid item revision to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }

        let valid = DayWeaveExecutionCommandRequest(
            expectedRevision: 1,
            command: .resume(sessionID: Self.sessionID)
        )
        let body = try client.encodedExecutionCommand(valid)
        do {
            _ = try client.encodedExecutionCommand(
                .init(
                    expectedRevision: UInt64.max,
                    command: .resume(sessionID: Self.sessionID)
                )
            )
            Issue.record("Expected an overflowing execution revision to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        do {
            _ = try await client.applyExecutionCommand(
                encodedRequest: body,
                idempotencyKey: "invalid\nheader"
            )
            Issue.record("Expected an unsafe idempotency key to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .requestEncodingFailed)
        }
        #expect(URLProtocolStub.storage.requests(for: Self.token).isEmpty)
    }

    @Test("history rejects duplicate authoritative rows")
    func duplicateHistoryFailsClosed() async throws {
        let row = Self.session(status: "completed", revision: 3)
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: Data(#"{"sessions":[\#(row),\#(row)]}"#.utf8)
            )
        )
        do {
            _ = try await Self.client().executionHistory(limit: 2)
            Issue.record("Expected duplicate history to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
    }

    @Test("snapshots reject terminal leases and impossible session state")
    func impossibleSnapshotsFailClosed() async throws {
        let terminal = Self.session(status: "completed", revision: 3)
        let invalidInitial = Self.session(status: "active", revision: 1)
        let excessivePause = Self.session(status: "paused", revision: 2)
            .replacingOccurrences(
                of: "2026-08-29T05:35:34Z",
                with: "2026-08-31T05:20:34Z"
            )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":3,"active_session":\#(terminal)}}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":1,"active_session":\#(invalidInitial)}}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":2,"active_session":\#(excessivePause)}}"#.utf8)
            )
        )
        let client = Self.client()
        await Self.expectDecodingFailure { try await client.executionSnapshot() }
        await Self.expectDecodingFailure { try await client.executionSnapshot() }
        await Self.expectDecodingFailure { try await client.executionSnapshot() }
    }

    @Test("history rejects more than one workspace-wide open lease")
    func multipleOpenHistoryFailsClosed() async throws {
        let newerID = UUID(uuidString: "10000000-0000-4000-8000-000000000002")!
        let newer = Self.session(status: "paused", revision: 2, id: newerID)
        let older = Self.session(status: "active", revision: 2)
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: Data(#"{"sessions":[\#(newer),\#(older)]}"#.utf8)
            )
        )
        do {
            _ = try await Self.client().executionHistory(limit: 2)
            Issue.record("Expected multiple open execution leases to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseDecodingFailed)
        }
    }

    @Test("command responses are bound to identity, transition, and global revision")
    func commandResponsesAreBoundToRequest() async throws {
        let otherID = UUID(uuidString: "10000000-0000-4000-8000-000000000002")!
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "active",
                    globalRevision: 5,
                    sessionRevision: 2,
                    replayed: false,
                    id: otherID
                )
            ),
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "skipped",
                    globalRevision: 5,
                    sessionRevision: 3,
                    replayed: false
                )
            ),
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "active",
                    globalRevision: 6,
                    sessionRevision: 2,
                    replayed: false
                )
            ),
            .init(
                statusCode: 200,
                body: Data(
                    String(
                        decoding: Self.mutationEnvelope(
                            status: "paused",
                            globalRevision: 5,
                            sessionRevision: 2,
                            replayed: false
                        ),
                        as: UTF8.self
                    ).replacingOccurrences(
                        of: "2026-08-29T05:35:34Z",
                        with: "2026-08-29T05:25:34Z"
                    ).utf8
                )
            ),
            .init(
                statusCode: 200,
                body: Self.mutationEnvelope(
                    status: "active",
                    globalRevision: 1,
                    sessionRevision: 1,
                    replayed: false
                )
            ),
            .init(
                statusCode: 200,
                body: Data(
                    String(
                        decoding: Self.mutationEnvelope(
                            status: "active",
                            globalRevision: 5,
                            sessionRevision: 3,
                            replayed: false
                        ),
                        as: UTF8.self
                    ).replacingOccurrences(
                        of: #""running_since":"2026-08-29T05:20:34Z""#,
                        with: #""running_since":"2026-08-29T05:00:00Z""#
                    ).utf8
                )
            )
        )
        let client = Self.client()
        let commands: [(DayWeaveExecutionCommandRequest, String)] = [
            (.init(expectedRevision: 4, command: .resume(sessionID: Self.sessionID)),
             "mac-bind-wrong-id"),
            (.init(expectedRevision: 4, command: .complete(
                sessionID: Self.sessionID,
                actualSeconds: nil
            )), "mac-bind-wrong-status"),
            (.init(expectedRevision: 4, command: .resume(sessionID: Self.sessionID)),
             "mac-bind-wrong-revision"),
            (.init(expectedRevision: 4, command: .pause(
                sessionID: Self.sessionID,
                durationSeconds: 900,
                pauseUntil: nil,
                reason: "Tea"
            )), "mac-bind-wrong-duration"),
            (.init(expectedRevision: 0, command: .resume(sessionID: Self.sessionID)),
             "mac-bind-initial-row-as-resume"),
            (.init(expectedRevision: 4, command: .resume(sessionID: Self.sessionID)),
             "mac-bind-stale-running-clock"),
        ]
        for (request, key) in commands {
            do {
                _ = try await client.applyExecutionCommand(request, idempotencyKey: key)
                Issue.record("Expected an unrelated mutation to fail closed")
            } catch let error as DayWeaveAPIError {
                #expect(error == .responseDecodingFailed)
            }
        }
    }

    @Test("all command variants encode and absolute pauses are bounded before first send")
    func allCommandVariantsEncode() throws {
        let client = Self.client()
        let future = Date().addingTimeInterval(600)
        let commands: [(String, DayWeaveExecutionCommand)] = [
            ("pause", .pause(
                sessionID: Self.sessionID,
                durationSeconds: nil,
                pauseUntil: future,
                reason: nil
            )),
            ("resume", .resume(sessionID: Self.sessionID)),
            ("complete", .complete(sessionID: Self.sessionID, actualSeconds: 42)),
            ("skip", .skip(sessionID: Self.sessionID, actualSeconds: 7)),
        ]
        for (type, command) in commands {
            let body = try client.encodedExecutionCommand(
                .init(expectedRevision: 4, command: command)
            )
            let object = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
            let encoded = try #require(object["command"] as? [String: Any])
            #expect(encoded["type"] as? String == type)
        }

        for invalidUntil in [Date().addingTimeInterval(-1), Date().addingTimeInterval(86_401)] {
            do {
                _ = try client.encodedExecutionCommand(
                    .init(
                        expectedRevision: 4,
                        command: .pause(
                            sessionID: Self.sessionID,
                            durationSeconds: nil,
                            pauseUntil: invalidUntil,
                            reason: nil
                        )
                    )
                )
                Issue.record("Expected an invalid absolute pause to fail locally")
            } catch let error as DayWeaveAPIError {
                #expect(error == .requestEncodingFailed)
            }
        }
    }

    private static func expectDecodingFailure(
        _ operation: () async throws -> DayWeaveExecutionSnapshot
    ) async {
        do {
            _ = try await operation()
            Issue.record("Expected execution decoding to fail closed")
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

    private static func mutationEnvelope(
        status: String,
        globalRevision: UInt64,
        sessionRevision: UInt64,
        replayed: Bool,
        id: UUID = sessionID
    ) -> Data {
        let row = status == "active" && sessionRevision == 1
            ? initialActiveSession(id: id)
            : session(status: status, revision: sessionRevision, id: id)
        let active = status == "active" || status == "paused" ? row : "null"
        return Data(
            #"{"mutation":{"revision":\#(globalRevision),"active_session":\#(active),"changed_session":\#(row),"replayed":\#(replayed)}}"#.utf8
        )
    }

    private static func initialActiveSession(id: UUID = sessionID) -> String {
        """
        {
          "id":"\(id.uuidString.lowercased())",
          "item_id":"\(itemID.uuidString.lowercased())",
          "item_revision":9,
          "occurrence_id":null,
          "session_index":0,
          "planned_block_id":"\(blockID.uuidString.lowercased())",
          "source_device_id":"\(deviceID.uuidString.lowercased())",
          "status":"active",
          "revision":1,
          "accumulated_seconds":0,
          "actual_seconds":null,
          "started_at":"2026-08-29T05:00:00Z",
          "running_since":"2026-08-29T05:00:00Z",
          "paused_at":null,
          "pause_until":null,
          "pause_reason":null,
          "ended_at":null,
          "created_at":"2026-08-29T05:00:00Z",
          "updated_at":"2026-08-29T05:00:00Z"
        }
        """
    }

    private static func session(
        status: String,
        revision: UInt64,
        id: UUID = sessionID
    ) -> String {
        let isActive = status == "active"
        let isPaused = status == "paused"
        let isTerminal = status == "completed" || status == "skipped"
        return """
        {
          "id":"\(id.uuidString.lowercased())",
          "item_id":"\(itemID.uuidString.lowercased())",
          "item_revision":9,
          "occurrence_id":null,
          "session_index":0,
          "planned_block_id":"\(blockID.uuidString.lowercased())",
          "source_device_id":"\(deviceID.uuidString.lowercased())",
          "status":"\(status)",
          "revision":\(revision),
          "accumulated_seconds":1234,
          "actual_seconds":\(isTerminal ? "1234" : "null"),
          "started_at":"2026-08-29T05:00:00Z",
          "running_since":\(isActive ? "\"2026-08-29T05:20:34Z\"" : "null"),
          "paused_at":\(isPaused ? "\"2026-08-29T05:20:34Z\"" : "null"),
          "pause_until":\(isPaused ? "\"2026-08-29T05:35:34Z\"" : "null"),
          "pause_reason":\(isPaused ? "\"Tea\"" : "null"),
          "ended_at":\(isTerminal ? "\"2026-08-29T05:20:34Z\"" : "null"),
          "created_at":"2026-08-29T05:00:00Z",
          "updated_at":"2026-08-29T05:20:34Z"
        }
        """
    }
}
