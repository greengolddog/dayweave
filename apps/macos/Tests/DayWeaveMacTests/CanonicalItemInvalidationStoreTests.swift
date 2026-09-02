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

    @Test("schedule publication winning the item hint race drains items and installs in one pass")
    func scheduleHintDrainsItemRace() async throws {
        let token = "schedule-item-race-token"
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let context = try Self.context(
            token: token,
            cursor: "cursor-before",
            now: now,
            scheduleProfile: try .legacyDefault(
                timezoneName: "Europe/Paris",
                protectedFreeMinutes: 90
            )
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let blockID = UUID()
        let revisionID = UUID()
        let current = Self.currentScheduleObject(
            itemID: itemID,
            blockID: blockID,
            revisionID: revisionID
        )
        let currentHeaders = [
            "Content-Type": "application/json",
            "Cache-Control": "no-store, max-age=0",
            "Pragma": "no-cache",
            "ETag": "\"1:\(revisionID.uuidString.lowercased())\"",
        ]
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            // Activation's lightweight item probe observes no hint yet.
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-before","has_more":false}"#.utf8
                )
            ),
            // No publication existed at the activation probe boundary.
            .init(
                statusCode: 404,
                headers: [
                    "Content-Type": "application/json",
                    "Cache-Control": "no-store, max-age=0",
                    "Pragma": "no-cache",
                ],
                body: Data(
                    #"{"error":{"code":"not_found","message":"Published schedule was not found"}}"#.utf8
                )
            ),
            // The schedule invalidation wins; this head references a newer item.
            .init(
                statusCode: 200,
                headers: currentHeaders,
                body: Data(current.utf8)
            ),
            // The schedule drain catches up items without waiting for an item hint.
            .init(
                statusCode: 200,
                body: Data(
                    "{\"changes\":[{\"type\":\"upsert\",\"item\":\(Self.itemObject(id: itemID))}],\"next_cursor\":\"cursor-after\",\"has_more\":false}".utf8
                )
            ),
            // Refetch after the item boundary proves the same immutable head.
            .init(
                statusCode: 200,
                headers: currentHeaders,
                body: Data(current.utf8)
            )
        )
        let scheduleStream = CanonicalScheduleStreamDouble(hints: [1])
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: CanonicalItemStreamDouble(hints: []),
            scheduleStream: scheduleStream,
            now: now
        )

        sync.startForegroundItemInvalidations(every: .seconds(3_600))
        try await Self.waitUntil {
            context.planner.publishedScheduleProof?.revisionNumber == 1
                && context.planner.canonicalDeltaCursor == "cursor-after"
        }
        sync.stopForegroundItemInvalidations()

        #expect(sync.warnings.isEmpty)
        #expect(context.planner.blocks.map(\.id) == [blockID])
        #expect(context.planner.blocks.first?.title == "Remote canonical work")
        #expect(context.planner.scheduleProfile.timezoneName == "Europe/Paris")
        #expect(context.planner.schedulePreviewProvenance?.timezoneName == "UTC")
        #expect(context.planner.canonicalPreviewFreshnessIssue == nil)
        let persisted = try #require(try context.persistence.load())
        #expect(persisted.scheduleProfile?.timezoneName == "Europe/Paris")
        #expect(persisted.schedulePreviewProvenance?.timezoneName == "UTC")
        #expect(persisted.publishedScheduleProof?.revisionNumber == 1)
        let restored = PlannerStore(persistence: context.persistence, now: { now })
        #expect(restored.scheduleProfile.timezoneName == "Europe/Paris")
        #expect(restored.schedulePreviewProvenance?.timezoneName == "UTC")
        #expect(restored.publishedScheduleProof?.revisionNumber == 1)
        #expect(restored.blocks.map(\.id) == [blockID])
        let paths = URLProtocolStub.storage.requests(for: token).map(\.url.path)
        #expect(paths == [
            "/gateway/v1/items/delta",
            "/gateway/v1/schedule/current",
            "/gateway/v1/schedule/current",
            "/gateway/v1/items/delta",
            "/gateway/v1/schedule/current",
        ])
        #expect(await scheduleStream.resumeRevisions == [0])
    }

    @Test("cursor-ahead recovery resets a stale schedule hint only after authoritative GET")
    func scheduleCursorAheadResetsHintHighWater() async throws {
        let token = "schedule-cursor-ahead-reset-token"
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let context = try Self.context(token: token, cursor: "cursor-before", now: now)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let blockID = UUID()
        let firstRevisionID = UUID()
        let recoveredRevisionID = UUID()
        let scheduleStream = CanonicalCursorAheadScheduleStreamDouble(staleHint: 9)
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: CanonicalItemStreamDouble(hints: []),
            scheduleStream: scheduleStream,
            now: now
        )

        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    "{\"changes\":[{\"type\":\"upsert\",\"item\":\(Self.itemObject(id: itemID))}],\"next_cursor\":\"cursor-after\",\"has_more\":false}".utf8
                )
            ),
            .init(
                statusCode: 200,
                headers: Self.currentHeaders(revisionID: firstRevisionID),
                body: Data(Self.currentScheduleObject(
                    itemID: itemID,
                    blockID: blockID,
                    revisionID: firstRevisionID
                ).utf8)
            )
        )
        #expect(await sync.bootstrapForegroundActivation())
        #expect(context.planner.publishedScheduleProof?.revisionNumber == 1)

        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after","has_more":false}"#.utf8
                )
            ),
            // The startup poll cannot lower a cursor after a generic failure.
            .init(
                statusCode: 503,
                body: Data(#"{"error":{"code":"offline","message":"retry"}}"#.utf8)
            ),
            // Only this authoritative GET may replace the stale hint high-water.
            .init(
                statusCode: 200,
                headers: Self.currentHeaders(
                    revisionID: recoveredRevisionID,
                    revisionNumber: 4
                ),
                body: Data(Self.currentScheduleObject(
                    itemID: itemID,
                    blockID: blockID,
                    revisionID: recoveredRevisionID,
                    revisionNumber: 4
                ).utf8)
            )
        )

        sync.startForegroundItemInvalidations(every: .seconds(3_600))
        try await Self.waitUntil {
            await scheduleStream.resumeRevisions.count >= 2
                && context.planner.publishedScheduleProof?.revisionNumber == 4
        }
        sync.stopForegroundItemInvalidations()

        #expect(await scheduleStream.resumeRevisions.prefix(2) == [1, 4])
        #expect(context.planner.publishedScheduleProof?.revisionID == recoveredRevisionID)
    }

    @Test("activation catches up items before installing current and performs no write")
    func activationReadFirstInstallsWithoutPublishing() async throws {
        let token = "schedule-activation-read-first-token"
        let now = Date(timeIntervalSince1970: 1_800_007_200)
        let context = try Self.context(token: token, cursor: "cursor-before", now: now)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let blockID = UUID()
        let revisionID = UUID()
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    "{\"changes\":[{\"type\":\"upsert\",\"item\":\(Self.itemObject(id: itemID))}],\"next_cursor\":\"cursor-after\",\"has_more\":false}".utf8
                )
            ),
            .init(
                statusCode: 200,
                headers: Self.currentHeaders(revisionID: revisionID),
                body: Data(Self.currentScheduleObject(
                    itemID: itemID,
                    blockID: blockID,
                    revisionID: revisionID
                ).utf8)
            )
        )
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: CanonicalItemStreamDouble(hints: []),
            scheduleStream: CanonicalScheduleStreamDouble(hints: []),
            now: now
        )

        #expect(await sync.bootstrapForegroundActivation())
        #expect(context.planner.canonicalDeltaCursor == "cursor-after")
        #expect(context.planner.blocks.map(\.id) == [blockID])
        #expect(context.planner.publishedScheduleProof?.hasCurrentImmutablePlanSeal == true)
        #expect(
            context.planner.schedulePreviewProvenance?.generatedAt
                == Date(timeIntervalSince1970: 1_800_000_000)
        )
        #expect(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).map(\.url.path) == [
            "/gateway/v1/items/delta",
            "/gateway/v1/schedule/current",
        ])
    }

    @Test("activation transport failure preserves the exact prior publication and writes nothing")
    func activationFailurePreservesReplica() async throws {
        let token = "schedule-activation-preserve-token"
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let context = try Self.context(token: token, cursor: "cursor-before", now: now)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let blockID = UUID()
        let revisionID = UUID()
        let current = Self.currentScheduleObject(
            itemID: itemID,
            blockID: blockID,
            revisionID: revisionID
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    "{\"changes\":[{\"type\":\"upsert\",\"item\":\(Self.itemObject(id: itemID))}],\"next_cursor\":\"cursor-after\",\"has_more\":false}".utf8
                )
            ),
            .init(
                statusCode: 200,
                headers: Self.currentHeaders(revisionID: revisionID),
                body: Data(current.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after","has_more":false}"#.utf8
                )
            ),
            .init(
                statusCode: 503,
                body: Data(#"{"error":{"code":"offline","message":"retry"}}"#.utf8)
            )
        )
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: CanonicalItemStreamDouble(hints: []),
            scheduleStream: CanonicalScheduleStreamDouble(hints: []),
            now: now
        )
        #expect(await sync.bootstrapForegroundActivation())
        let priorBlocks = context.planner.blocks
        let priorProof = context.planner.publishedScheduleProof

        #expect(!(await sync.bootstrapForegroundActivation()))
        #expect(context.planner.blocks == priorBlocks)
        #expect(context.planner.publishedScheduleProof == priorProof)
        #expect(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).map(\.url.path) == [
            "/gateway/v1/items/delta", "/gateway/v1/schedule/current",
            "/gateway/v1/items/delta", "/gateway/v1/schedule/current",
        ])
    }

    @Test("activation exact 404 atomically clears and never composes or publishes")
    func activationExactAbsenceClearsReadOnly() async throws {
        let token = "schedule-activation-absence-token"
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let context = try Self.context(token: token, cursor: "cursor-before", now: now)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let blockID = UUID()
        let revisionID = UUID()
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    "{\"changes\":[{\"type\":\"upsert\",\"item\":\(Self.itemObject(id: itemID))}],\"next_cursor\":\"cursor-after\",\"has_more\":false}".utf8
                )
            ),
            .init(
                statusCode: 200,
                headers: Self.currentHeaders(revisionID: revisionID),
                body: Data(Self.currentScheduleObject(
                    itemID: itemID,
                    blockID: blockID,
                    revisionID: revisionID
                ).utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"cursor-after","has_more":false}"#.utf8
                )
            ),
            .init(
                statusCode: 404,
                headers: [
                    "Content-Type": "application/json",
                    "Cache-Control": "no-store, max-age=0",
                    "Pragma": "no-cache",
                ],
                body: Data(
                    #"{"error":{"code":"not_found","message":"Published schedule was not found"}}"#.utf8
                )
            )
        )
        let sync = Self.sync(
            planner: context.planner,
            token: token,
            stream: CanonicalItemStreamDouble(hints: []),
            scheduleStream: CanonicalScheduleStreamDouble(hints: []),
            now: now
        )
        #expect(await sync.bootstrapForegroundActivation())

        #expect(await sync.bootstrapForegroundActivation())
        #expect(context.planner.blocks.isEmpty)
        #expect(context.planner.schedulePreviewProvenance == nil)
        #expect(context.planner.publishedScheduleProof == nil)
        #expect(try #require(context.persistence.load()).publishedScheduleProof == nil)
        #expect(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).map(\.url.path) == [
            "/gateway/v1/items/delta", "/gateway/v1/schedule/current",
            "/gateway/v1/items/delta", "/gateway/v1/schedule/current",
        ])
    }

    private static func context(
        token: String,
        cursor: String,
        now: Date = Date(timeIntervalSince1970: 1_800_000_000),
        previewValidated: Bool = false,
        scheduleProfile: ScheduleProfile? = nil,
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
            scheduleProfile: scheduleProfile,
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
        scheduleStream: (any DayWeaveScheduleStreamTransport)? = nil,
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
            scheduleStreamTransportProvider: { _ in scheduleStream },
            scheduleReplicaRequiresDurableBinding: scheduleStream.map { _ in false } ?? true,
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

    private static func currentScheduleObject(
        itemID: UUID,
        blockID: UUID,
        revisionID: UUID,
        revisionNumber: UInt64 = 1
    ) -> String {
        let digest = "sha256:" + String(repeating: "a", count: 64)
        return """
        {"revision":{"id":"\(revisionID.uuidString.lowercased())","revision":"\(revisionNumber):\(revisionID.uuidString.lowercased())","revision_number":\(revisionNumber),"input_digest":"\(digest)","horizon_start":"2027-01-15T00:00:00Z","horizon_end":"2027-01-16T00:00:00Z","timezone_name":"UTC","published_at":"2027-01-15T08:00:00Z"},"schedule":{"input_digest":"\(digest)","source_item_count":1,"accepted_item_count":1,"source_item_revisions":{"\(itemID.uuidString.lowercased())":1},"rejected_items":[],"ignored_previous_assignments":[],"plan":{"as_of":"2027-01-15T08:00:00Z","horizon_start":"2027-01-15T00:00:00Z","horizon_end":"2027-01-16T00:00:00Z","blocks":[{"id":"\(blockID.uuidString.lowercased())","is_sensitive":false,"item_id":"\(itemID.uuidString.lowercased())","occurrence_id":null,"external_block_id":null,"title":"Remote canonical work","start":"2027-01-15T09:00:00Z","end":"2027-01-15T09:30:00Z","session_index":0,"kind":"planned","explanations":[]}],"unscheduled":[],"decisions":[],"violations":[],"score":{"scheduled_minutes":30,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},"occurrences":[]}}}
        """
    }

    private static func currentHeaders(
        revisionID: UUID,
        revisionNumber: UInt64 = 1
    ) -> [String: String] {
        [
            "Content-Type": "application/json",
            "Cache-Control": "no-store, max-age=0",
            "Pragma": "no-cache",
            "ETag": "\"\(revisionNumber):\(revisionID.uuidString.lowercased())\"",
        ]
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

private actor CanonicalScheduleStreamDouble: DayWeaveScheduleStreamTransport {
    private let hints: [UInt64]
    private(set) var resumeRevisions: [UInt64] = []

    init(hints: [UInt64]) {
        self.hints = hints
    }

    func consumeScheduleInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveScheduleStreamCompletion {
        resumeRevisions.append(revision)
        for hint in hints { await receive(hint) }
        return .liveEndOfStream
    }
}

private actor CanonicalCursorAheadScheduleStreamDouble:
    DayWeaveScheduleStreamTransport
{
    private let staleHint: UInt64
    private(set) var resumeRevisions: [UInt64] = []

    init(staleHint: UInt64) {
        self.staleHint = staleHint
    }

    func consumeScheduleInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveScheduleStreamCompletion {
        resumeRevisions.append(revision)
        if resumeRevisions.count == 1 {
            await receive(staleHint)
            return .cursorAhead(headRevision: 0)
        }
        try await Task.sleep(for: .seconds(3_600))
        return .liveEndOfStream
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
