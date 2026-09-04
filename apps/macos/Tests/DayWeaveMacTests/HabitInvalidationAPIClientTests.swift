import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Habit invalidation stream API", .serialized)
struct HabitInvalidationAPIClientTests {
    private static let token = "habit-stream-transport-test-token"

    init() {
        URLProtocolStub.storage.reset(key: Self.token)
    }

    @Test("request copies the encrypted cursor exactly and emits only validated hints")
    func requestAndResponseContract() async throws {
        let cursor = "DWH1_before-token"
        let next = "DWH1_after-token"
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.successHeaders,
                body: Data(
                    ": heartbeat\n\nid: \(next)\nevent: habit-invalidation\n"
                        .appending("data: {\"cursor\":\"\(next)\"}\n\n")
                        .utf8
                )
            )
        )
        let recorder = HabitStreamCursorRecorder()

        let completion = try await Self.client().consumeHabitInvalidations(after: cursor) {
            await recorder.append($0)
        }

        #expect(completion == .liveEndOfStream)
        #expect(await recorder.values == [next])
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/habits/stream")
        #expect(request.headers["Accept"] == "text/event-stream")
        #expect(request.headers["Last-Event-ID"] == cursor)
        #expect(request.headers["Cache-Control"] == "no-store")
        #expect(request.headers["Pragma"] == "no-cache")
        #expect(request.headers["Accept-Encoding"] == "identity")
        #expect(request.headers["Authorization"] == "Bearer \(Self.token)")
        #expect(request.body == nil)
    }

    @Test("404 disables only the optional stream")
    func unsupportedResponse() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 404, body: Data(repeating: 0x61, count: 32 * 1_024))
        )
        let completion = try await Self.client().consumeHabitInvalidations(after: "cursor") { _ in
            Issue.record("An unsupported endpoint cannot emit a hint")
        }
        #expect(completion == .unsupported)
    }

    @Test("invalid local cursors never reach URLSession")
    func rejectsUnsafeResumeCursor() async {
        for cursor in ["", "unsafe cursor", "opaque.dot"] {
            do {
                _ = try await Self.client().consumeHabitInvalidations(after: cursor) { _ in }
                Issue.record("Expected unsafe Last-Event-ID to fail locally")
            } catch let error as DayWeaveAPIError {
                #expect(error == .requestEncodingFailed)
            } catch {
                Issue.record("Unexpected error: \(error)")
            }
        }
        #expect(URLProtocolStub.storage.requests(for: Self.token).isEmpty)
    }

    @Test("successful streams require strict content-free privacy headers")
    func strictResponseHeaders() async {
        let invalidHeaders: [[String: String]] = [
            [
                "Content-Type": "application/json",
                "Cache-Control": "no-store, no-cache",
                "Pragma": "no-cache",
                "X-Accel-Buffering": "no",
            ],
            [
                "Content-Type": "text/event-stream",
                "Content-Encoding": "gzip",
                "Cache-Control": "no-store, no-cache",
                "Pragma": "no-cache",
                "X-Accel-Buffering": "no",
            ],
            [
                "Content-Type": "text/event-stream",
                "Cache-Control": "no-store",
                "Pragma": "no-cache",
                "X-Accel-Buffering": "no",
            ],
            [
                "Content-Type": "text/event-stream",
                "Cache-Control": "no-store, no-cache",
                "X-Accel-Buffering": "no",
            ],
            [
                "Content-Type": "text/event-stream",
                "Cache-Control": "no-store, no-cache",
                "Pragma": "no-cache",
                "X-Accel-Buffering": "yes",
            ],
        ]
        for headers in invalidHeaders {
            URLProtocolStub.storage.enqueue(
                key: Self.token,
                .init(statusCode: 200, headers: headers, body: Data(": heartbeat\n\n".utf8))
            )
            do {
                _ = try await Self.client().consumeHabitInvalidations(after: "cursor") { _ in }
                Issue.record("Expected response headers to fail closed")
            } catch is DayWeaveHabitStreamProtocolError {
                // Expected.
            } catch {
                Issue.record("Unexpected error: \(error)")
            }
        }
    }

    @Test("non-success response bodies are independently bounded")
    func boundsErrorBody() async {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 503, body: Data(repeating: 0x61, count: 8 * 1_024 + 1))
        )
        do {
            _ = try await Self.client().consumeHabitInvalidations(after: "cursor") { _ in }
            Issue.record("Expected oversized error response to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseTooLarge(limitBytes: 8 * 1_024))
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    @Test("task cancellation closes a habit response that has not arrived")
    func cancellationStopsRequest() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: Self.successHeaders,
                body: Data(),
                holdsOpenUntilCancelled: true
            )
        )
        let task = Task {
            try await Self.client().consumeHabitInvalidations(after: "cursor") { _ in }
        }
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while URLProtocolStub.storage.requests(for: Self.token).isEmpty,
              ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        task.cancel()
        do {
            _ = try await task.value
            Issue.record("Expected canceled habit stream request to terminate")
        } catch let error as DayWeaveAPIError {
            #expect(error == .transport(.cancelled))
        } catch is CancellationError {
            // Either structured cancellation path must close promptly.
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    private static let successHeaders = [
        "Content-Type": "text/event-stream; charset=utf-8",
        "Content-Encoding": "IDENTITY",
        "Cache-Control": "no-store, no-cache",
        "Pragma": "no-cache",
        "X-Accel-Buffering": "no",
    ]

    private static func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        )
    }
}

private actor HabitStreamCursorRecorder {
    private(set) var values: [String] = []

    func append(_ cursor: String) {
        values.append(cursor)
    }
}
#endif
