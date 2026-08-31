import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Execution invalidation stream API", .serialized)
struct ExecutionInvalidationAPIClientTests {
    private static let token = "execution-stream-transport-test-token"

    init() {
        URLProtocolStub.storage.reset(key: Self.token)
    }

    @Test("stream request carries exact resume revision and emits validated hints")
    func requestAndResponseContract() async throws {
        let body = Data(
            ": heartbeat\n\nid: 6\nevent: execution-invalidation\ndata: {\"revision\":6}\n\n"
                .utf8
        )
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: [
                    "Content-Type": "text/event-stream; charset=utf-8",
                    "Content-Encoding": "IDENTITY",
                ],
                body: body
            )
        )
        let recorder = StreamRevisionRecorder()

        let completion = try await Self.client().consumeExecutionInvalidations(after: 5) {
            await recorder.append($0)
        }

        #expect(completion == .liveEndOfStream)
        #expect(await recorder.values == [6])
        let request = try #require(URLProtocolStub.storage.requests(for: Self.token).first)
        #expect(request.method == "GET")
        #expect(request.url.path == "/gateway/v1/execution/stream")
        #expect(request.headers["Accept"] == "text/event-stream")
        #expect(request.headers["Last-Event-ID"] == "5")
        #expect(request.headers["Cache-Control"] == "no-store")
        #expect(request.headers["Pragma"] == "no-cache")
        #expect(request.headers["Accept-Encoding"] == "identity")
        #expect(request.headers["Authorization"] == "Bearer \(Self.token)")
        #expect(request.body == nil)
    }

    @Test("404 disables streaming only for the activation")
    func unsupportedResponse() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 404, body: Data(repeating: 0x61, count: 32 * 1_024))
        )
        let completion = try await Self.client().consumeExecutionInvalidations(after: 0) { _ in
            Issue.record("An unsupported endpoint cannot emit an invalidation")
        }
        #expect(completion == .unsupported)
        #expect(URLProtocolStub.storage.requests(for: Self.token).count == 1)
    }

    @Test("successful streams require the exact event-stream media type")
    func strictContentType() async {
        for contentType in [
            "application/json",
            "text/event-stream; charset=iso-8859-1",
            "text/event-stream; charset=utf-8; future=true",
            "text/event-stream, text/event-stream",
        ] {
            URLProtocolStub.storage.enqueue(
                key: Self.token,
                .init(
                    statusCode: 200,
                    headers: ["Content-Type": contentType],
                    body: Data()
                )
            )
            do {
                _ = try await Self.client().consumeExecutionInvalidations(after: 0) { _ in }
                Issue.record("Expected media type \(contentType) to fail closed")
            } catch let error as DayWeaveExecutionStreamProtocolError {
                #expect(error == .invalidContentType)
            } catch {
                Issue.record("Unexpected error: \(error)")
            }
        }
    }

    @Test("successful streams reject compressed or duplicate content encodings")
    func strictContentEncoding() async {
        for contentEncoding in [
            "gzip",
            "identity, identity",
            "identity, gzip",
        ] {
            URLProtocolStub.storage.enqueue(
                key: Self.token,
                .init(
                    statusCode: 200,
                    headers: [
                        "Content-Type": "text/event-stream",
                        "Content-Encoding": contentEncoding,
                    ],
                    body: Data(": heartbeat\n\n".utf8)
                )
            )
            do {
                _ = try await Self.client().consumeExecutionInvalidations(after: 0) { _ in }
                Issue.record("Expected encoding \(contentEncoding) to fail closed")
            } catch let error as DayWeaveExecutionStreamProtocolError {
                #expect(error == .invalidContentEncoding)
            } catch {
                Issue.record("Unexpected error: \(error)")
            }
        }
    }

    @Test("non-success response bodies have a narrow independent bound")
    func boundsErrorBody() async {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(statusCode: 503, body: Data(repeating: 0x61, count: 8 * 1_024 + 1))
        )
        do {
            _ = try await Self.client().consumeExecutionInvalidations(after: 0) { _ in }
            Issue.record("Expected oversized error response to fail")
        } catch let error as DayWeaveAPIError {
            #expect(error == .responseTooLarge(limitBytes: 8 * 1_024))
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    @Test("task cancellation closes a response that has not arrived")
    func cancellationStopsRequest() async throws {
        URLProtocolStub.storage.enqueue(
            key: Self.token,
            .init(
                statusCode: 200,
                headers: ["Content-Type": "text/event-stream"],
                body: Data(),
                delay: 5
            )
        )
        let task = Task {
            try await Self.client().consumeExecutionInvalidations(after: 0) { _ in }
        }
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while URLProtocolStub.storage.requests(for: Self.token).isEmpty,
              ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        task.cancel()
        do {
            _ = try await task.value
            Issue.record("Expected canceled stream request to terminate")
        } catch let error as DayWeaveAPIError {
            #expect(error == .transport(.cancelled))
        } catch is CancellationError {
            // Foundation may surface structured cancellation before creating
            // the URLSession task; either route is a prompt closed request.
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
    }

    @Test("total lifetime watchdog cancels a stream despite heartbeat progress")
    func totalLifetimeWatchdogCancelsLiveStream() async throws {
        HangingExecutionStreamURLProtocol.reset()
        let lifetime = ExecutionStreamLifetimeGate()
        let client = DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: HangingExecutionStreamURLProtocol.makeSession(),
            bearerToken: Self.token,
            executionStreamLifetimeSleep: {
                try await lifetime.wait()
            }
        )
        let revisions = StreamRevisionRecorder()
        let task = Task {
            try await client.consumeExecutionInvalidations(after: 12) {
                await revisions.append($0)
            }
        }
        let progressDeadline = ContinuousClock.now.advanced(by: .seconds(2))
        while await revisions.values.count < 3,
              ContinuousClock.now < progressDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await revisions.values.count >= 3)
        #expect(HangingExecutionStreamURLProtocol.heartbeatCount >= 3)

        await lifetime.fire()
        do {
            _ = try await task.value
            Issue.record("Expected the absolute stream lifetime to expire")
        } catch let error as DayWeaveAPIError {
            #expect(error == .transport(.timedOut))
        } catch {
            Issue.record("Unexpected error: \(error)")
        }
        let stopDeadline = ContinuousClock.now.advanced(by: .seconds(2))
        while !HangingExecutionStreamURLProtocol.wasStopped,
              ContinuousClock.now < stopDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(HangingExecutionStreamURLProtocol.wasStopped)
    }

    private static func client() -> DayWeaveAPIClient {
        DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL("https://api.example.com/gateway"),
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        )
    }
}

private actor StreamRevisionRecorder {
    private(set) var values: [UInt64] = []

    func append(_ revision: UInt64) {
        values.append(revision)
    }
}

private actor ExecutionStreamLifetimeGate {
    private var continuation: CheckedContinuation<Void, Never>?
    private var fired = false

    func wait() async throws {
        try Task.checkCancellation()
        if !fired {
            await withCheckedContinuation { continuation in
                self.continuation = continuation
            }
        }
        try Task.checkCancellation()
    }

    func fire() {
        fired = true
        continuation?.resume()
        continuation = nil
    }
}

private final class HangingExecutionStreamURLProtocol: URLProtocol, @unchecked Sendable {
    private final class State: @unchecked Sendable {
        private let lock = NSLock()
        private var heartbeatCount = 0
        private var wasStopped = false

        var observedHeartbeatCount: Int { lock.withLock { heartbeatCount } }
        var observedStop: Bool { lock.withLock { wasStopped } }

        func reset() {
            lock.withLock {
                heartbeatCount = 0
                wasStopped = false
            }
        }

        func recordHeartbeat() {
            lock.withLock { heartbeatCount += 1 }
        }

        func recordStop() {
            lock.withLock { wasStopped = true }
        }
    }

    private static let state = State()
    private let stateLock = NSLock()
    private var stopped = false

    static var heartbeatCount: Int { state.observedHeartbeatCount }
    static var wasStopped: Bool { state.observedStop }

    static func reset() {
        state.reset()
    }

    static func makeSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [HangingExecutionStreamURLProtocol.self]
        return URLSession(configuration: configuration)
    }

    override class func canInit(with _: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url,
              let response = HTTPURLResponse(
                  url: url,
                  statusCode: 200,
                  httpVersion: "HTTP/1.1",
                  headerFields: ["Content-Type": "text/event-stream"]
              ) else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        deliverHeartbeat(1)
    }

    override func stopLoading() {
        stateLock.withLock { stopped = true }
        Self.state.recordStop()
    }

    private func deliverHeartbeat(_ sequence: Int) {
        guard !stateLock.withLock({ stopped }) else { return }
        client?.urlProtocol(
            self,
            didLoad: Data(
                ": heartbeat\n\nid: \(12 + sequence)\n"
                    .appending("event: execution-invalidation\n")
                    .appending("data: {\"revision\":\(12 + sequence)}\n\n")
                    .utf8
            )
        )
        Self.state.recordHeartbeat()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.02) { [weak self] in
            self?.deliverHeartbeat(sequence + 1)
        }
    }
}
#endif
