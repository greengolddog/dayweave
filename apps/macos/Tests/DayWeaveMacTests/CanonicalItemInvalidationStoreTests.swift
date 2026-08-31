import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Canonical item foreground invalidations", .serialized)
@MainActor
struct CanonicalItemInvalidationStoreTests {
    private static let baseURL = "https://api.example.com/gateway"

    @Test("own echo durably advances the cursor without preview or publication")
    func ownEchoDoesNotRecompose() async throws {
        let token = "canonical-item-own-echo-token"
        let context = try Self.context(token: token, cursor: "cursor-before")
        defer { try? FileManager.default.removeItem(at: context.directory) }
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after","has_more":false}"#.utf8
                )
            )
        )
        let stream = CanonicalItemStreamDouble(hints: ["cursor-after"])
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream
        )

        sync.startForegroundItemInvalidations(every: .seconds(3_600))
        try await Self.waitUntil { context.planner.canonicalDeltaCursor == "cursor-after" }
        sync.stopForegroundItemInvalidations()

        let requests = URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        )
        #expect(requests.map(\.url.path) == ["/gateway/v1/items/delta"])
        #expect(Self.queryValue("limit", in: requests[0].url) == "200")
        #expect(await stream.resumeCursors == ["cursor-before"])
        #expect(try context.persistence.load()?.canonicalDeltaCursor == "cursor-after")
    }

    @Test("404 stream fallback performs only a limit-one no-op probe")
    func unsupportedStreamKeepsLightweightPoll() async throws {
        let token = "canonical-item-unsupported-token"
        let context = try Self.context(token: token, cursor: "cursor-before")
        defer { try? FileManager.default.removeItem(at: context.directory) }
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-before","has_more":false}"#.utf8
                )
            )
        )
        let stream = CanonicalItemStreamDouble(hints: [])
        let sleep = CanonicalItemSleepGate()
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream,
            sleep: sleep
        )

        sync.startForegroundItemInvalidations(every: .seconds(30))
        try await Self.waitUntil { await sleep.waitingCount > 0 }
        await sleep.advance()
        try await Self.waitUntil {
            !URLProtocolStub.storage.requests(for: token).isEmpty
        }
        sync.stopForegroundItemInvalidations()

        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.count == 1)
        #expect(requests[0].url.path == "/gateway/v1/items/delta")
        #expect(Self.queryValue("limit", in: requests[0].url) == "1")
        #expect(await stream.resumeCursors == ["cursor-before"])
    }

    @Test("changed delta blocks the old preview and poll retries failed publication")
    func changedDeltaRetainsPublicationRepair() async throws {
        let token = "canonical-item-publication-repair-token"
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let context = try Self.context(
            token: token,
            cursor: "cursor-before",
            now: now,
            previewValidated: true
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        #expect(context.planner.canonicalPreviewFreshnessIssue == nil)
        let itemID = UUID()
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    "{\"changes\":[{\"type\":\"upsert\",\"item\":\(Self.itemObject(id: itemID))}],\"next_cursor\":\"cursor-after\",\"has_more\":false}".utf8
                )
            ),
            .init(statusCode: 503, body: Data(#"{"error":{"code":"offline","message":"retry"}}"#.utf8)),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after","has_more":false}"#.utf8
                )
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after","has_more":false}"#.utf8
                )
            ),
            .init(statusCode: 503, body: Data(#"{"error":{"code":"offline","message":"retry"}}"#.utf8))
        )
        let stream = CanonicalItemStreamDouble(hints: ["cursor-after"])
        let sleep = CanonicalItemSleepGate()
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream,
            sleep: sleep,
            now: now
        )

        sync.startForegroundItemInvalidations(every: .seconds(30))
        try await Self.waitUntil {
            context.planner.canonicalDeltaCursor == "cursor-after" && sync.status.isFailure
        }
        #expect(context.planner.canonicalPreviewFreshnessIssue != nil)
        try await Self.waitUntil { await sleep.waitingCount > 0 }
        await sleep.advance()
        try await Self.waitUntil {
            URLProtocolStub.storage.requests(for: token).count >= 5
        }
        sync.stopForegroundItemInvalidations()

        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.map(\.url.path) == [
            "/gateway/v1/items/delta",
            "/gateway/v1/schedule/preview",
            "/gateway/v1/items/delta",
            "/gateway/v1/items/delta",
            "/gateway/v1/schedule/preview",
        ])
        #expect(Self.queryValue("limit", in: requests[2].url) == "1")
        #expect(Self.queryValue("limit", in: requests[3].url) == "200")
    }

    @Test("an in-flight latest hint covered by the authoritative cursor is coalesced")
    func inFlightLatestHintIsProvenCovered() async throws {
        let token = "canonical-item-in-flight-coalescing-token"
        let context = try Self.context(token: token, cursor: "cursor-before")
        defer { try? FileManager.default.removeItem(at: context.directory) }
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after-two","has_more":false}"#.utf8
                ),
                delay: 0.2
            )
        )
        let stream = CanonicalInterleavedItemStreamDouble(token: token)
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream
        )

        sync.startForegroundItemInvalidations(every: .seconds(3_600))
        try await Self.waitUntil {
            context.planner.canonicalDeltaCursor == "cursor-after-two"
        }
        try await Task.sleep(for: .milliseconds(100))
        sync.stopForegroundItemInvalidations()

        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.count == 1)
        #expect(Self.queryValue("limit", in: requests[0].url) == "200")
        #expect(await stream.resumeCursors == ["cursor-before"])
    }

    @Test("a failed binding persistence cannot start SSE from its in-memory binding")
    func failedBindingPersistenceDoesNotStartStream() async throws {
        let token = "canonical-item-failed-binding-persistence-token"
        URLProtocolStub.storage.reset(key: token)
        let context = try Self.context(token: token, cursor: "cursor-before")
        let stream = CanonicalItemStreamDouble(hints: ["cursor-after"])
        let sleep = CanonicalItemSleepGate()
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream,
            sleep: sleep
        )
        try FileManager.default.removeItem(at: context.directory)
        #expect(context.planner.beginCanonicalSync())
        #expect(throws: (any Error).self) {
            try context.planner.prepareCanonicalSync(
                configurationIdentifier: Self.configurationIdentifier(token: token)
            )
        }
        context.planner.endCanonicalSync()
        #expect(!context.planner.canPersistPlan)

        sync.startForegroundItemInvalidations(every: .seconds(30))
        try await Self.waitUntil { await sleep.waitingCount > 0 }
        #expect(await stream.resumeCursors.isEmpty)
        #expect(URLProtocolStub.storage.requests(for: token).isEmpty)
        sync.stopForegroundItemInvalidations()
    }

    @Test("a durable cursor bound to another connection cannot start delivery")
    func staleBindingDoesNotStartStreamOrProbe() async throws {
        let token = "canonical-item-stale-binding-token"
        URLProtocolStub.storage.reset(key: token)
        let context = try Self.context(
            token: token,
            cursor: "cursor-before",
            boundConfigurationIdentifier: "https://old.example.com|auth=old-binding"
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let stream = CanonicalItemStreamDouble(hints: ["cursor-after"])
        let sleep = CanonicalItemSleepGate()
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream,
            sleep: sleep
        )

        sync.startForegroundItemInvalidations(every: .seconds(30))
        try await Self.waitUntil { await sleep.waitingCount > 0 }
        #expect(await stream.resumeCursors.isEmpty)
        #expect(URLProtocolStub.storage.requests(for: token).isEmpty)
        sync.stopForegroundItemInvalidations()
    }

    @Test("probe drains admit two immediate reconciliations and resume on the next probe")
    func probeDrainUsesBoundedImmediateAdmission() async throws {
        let token = "canonical-item-probe-drain-admission-token"
        let context = try Self.context(token: token, cursor: "cursor-before")
        defer { try? FileManager.default.removeItem(at: context.directory) }
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"probe-observed","has_more":false}"#.utf8
                )
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"drain-one","has_more":false}"#.utf8
                ),
                delay: 0.15
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"drain-two","has_more":false}"#.utf8
                ),
                delay: 0.15
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"stream-hint-two","has_more":false}"#.utf8
                )
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"stream-hint-two","has_more":false}"#.utf8
                )
            )
        )
        let stream = CanonicalProbeInterleavedItemStreamDouble(token: token)
        let sleep = CanonicalItemSleepGate()
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: stream,
            sleep: sleep
        )

        sync.startForegroundItemInvalidations(every: .seconds(30))
        try await Self.waitUntil { await sleep.waitingCount > 0 }
        await sleep.advance()
        try await Self.waitUntil {
            context.planner.canonicalDeltaCursor == "drain-two"
        }
        try await Task.sleep(for: .milliseconds(200))
        #expect(URLProtocolStub.storage.requests(for: token).count == 3)

        try await Self.waitUntil { await sleep.waitingCount > 0 }
        await sleep.advance()
        try await Self.waitUntil {
            context.planner.canonicalDeltaCursor == "stream-hint-two"
                && URLProtocolStub.storage.requests(for: token).count == 5
        }
        sync.stopForegroundItemInvalidations()

        let requests = URLProtocolStub.storage.requests(for: token)
        #expect(requests.compactMap { Self.queryValue("limit", in: $0.url) } == [
            "1", "200", "200", "1", "200",
        ])
        #expect(await stream.resumeCursors == ["cursor-before"])
    }

    private static func context(
        token: String,
        cursor: String,
        now: Date = Date(timeIntervalSince1970: 1_800_000_000),
        previewValidated: Bool = false,
        boundConfigurationIdentifier: String? = nil
    ) throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence,
        planner: PlannerStore
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveCanonicalItemStreamTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: try PlannerEncryptionKey(data: Data(repeating: 91, count: 32))
        )
        let configuration = boundConfigurationIdentifier
            ?? Self.configurationIdentifier(token: token)
        let planner = PlannerStore(
            canonicalDeltaCursor: cursor,
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: previewValidated
                ? .init(
                    configurationIdentifier: configuration,
                    generatedAt: now,
                    asOf: now,
                    horizonStart: now.addingTimeInterval(-3_600),
                    horizonEnd: now.addingTimeInterval(86_400),
                    timezoneName: "UTC"
                )
                : nil,
            previewValidatedForCurrentLaunch: previewValidated,
            persistence: persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        planner.flushPersistence()
        return (directory, persistence, planner)
    }

    private static func sync(
        planner: PlannerStore,
        token: String,
        stream: any DayWeaveItemStreamTransport,
        sleep: CanonicalItemSleepGate? = nil,
        now: Date = Date(timeIntervalSince1970: 1_800_000_000)
    ) -> CanonicalSyncStore {
        CanonicalSyncStore(
            planner: planner,
            configurationStore: CanonicalItemFixedConfigurationStore(baseURL: baseURL),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            itemStreamTransportProvider: { _ in stream },
            itemStreamSleep: { duration in
                if let sleep {
                    try await sleep.wait()
                } else {
                    try await Task.sleep(for: duration)
                }
            },
            now: { now }
        )
    }

    private static func configurationIdentifier(token: String) -> String {
        let client = DayWeaveAPIClient(
            baseURL: try! DayWeaveAPIBaseURL(baseURL),
            bearerToken: token
        )
        return client.configurationIdentifier
    }

    private static func queryValue(_ name: String, in url: URL) -> String? {
        URLComponents(url: url, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == name })?.value
    }

    private static func waitUntil(
        timeout: Duration = .seconds(3),
        _ condition: @escaping @MainActor () async -> Bool
    ) async throws {
        let deadline = ContinuousClock.now.advanced(by: timeout)
        while !(await condition()), ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await condition())
    }

    private static func itemObject(id: UUID) -> String {
        """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":false,
         "kind":"task","status":"scheduled","title":"Remote canonical work",
         "notes":null,"timezone_name":"UTC","duration_seconds":1800,
         "deadline_at":null,"earliest_start_at":null,"recurrence":null,
         "flexible_constraints":{},"split_policy":{"type":"indivisible"},
         "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
         "is_executable":true,"revision":1,"created_at":"2027-01-15T10:00:00Z",
         "updated_at":"2027-01-15T10:00:00Z","completed_at":null,"deleted_at":null}
        """
    }
}

private actor CanonicalItemStreamDouble: DayWeaveItemStreamTransport {
    private let hints: [String]
    private(set) var resumeCursors: [String] = []

    init(hints: [String]) {
        self.hints = hints
    }

    func consumeItemInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamCompletion {
        resumeCursors.append(cursor)
        for hint in hints { await receive(hint) }
        return .unsupported
    }
}

private actor CanonicalInterleavedItemStreamDouble: DayWeaveItemStreamTransport {
    private let token: String
    private(set) var resumeCursors: [String] = []

    init(token: String) {
        self.token = token
    }

    func consumeItemInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamCompletion {
        resumeCursors.append(cursor)
        await receive("cursor-after-one")
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while URLProtocolStub.storage.requests(for: token).isEmpty,
              ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(5))
        }
        await receive("cursor-after-two")
        return .unsupported
    }
}

private actor CanonicalProbeInterleavedItemStreamDouble: DayWeaveItemStreamTransport {
    private let token: String
    private(set) var resumeCursors: [String] = []

    init(token: String) {
        self.token = token
    }

    func consumeItemInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamCompletion {
        resumeCursors.append(cursor)
        try await waitForRequestCount(2)
        await receive("stream-hint-one")
        try await waitForRequestCount(3)
        await receive("stream-hint-two")
        return .unsupported
    }

    private func waitForRequestCount(_ count: Int) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while URLProtocolStub.storage.requests(for: token).count < count,
              ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}

private struct CanonicalItemFixedConfigurationStore: SuggestionAPIConfigurationStoring {
    let baseURL: String

    func loadBaseURL() -> String? { baseURL }
    func saveBaseURL(_: String) {}
}

private actor CanonicalItemSleepGate {
    private var waiters: [UUID: CheckedContinuation<Void, Error>] = [:]

    var waitingCount: Int { waiters.count }

    func wait() async throws {
        let id = UUID()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                waiters[id] = continuation
            }
        } onCancel: {
            Task { await self.cancel(id) }
        }
    }

    func advance() {
        guard let entry = waiters.first else { return }
        waiters.removeValue(forKey: entry.key)
        entry.value.resume()
    }

    private func cancel(_ id: UUID) {
        waiters.removeValue(forKey: id)?.resume(throwing: CancellationError())
    }
}
#endif
