import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Habit sync store", .serialized)
@MainActor
struct HabitSyncStoreTests {
    @Test("activation commits each delta page and cursor into the encrypted cache")
    func activationPersistsDelta() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )]
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .success)
        #expect(store.occurrences == [occurrence])
        let disk = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(disk.deltaCursor == "cursor-one")
        #expect(disk.occurrences == [occurrence])
        #expect(await transport.deltaCursors() == [nil])
    }

    @Test("an outcome is encrypted before transport and removed only after its response is durable")
    func mutationFence() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let observation = LockedFlag()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            beforeOutcome: {
                let pending = try? context.persistence.loadRevisioned().snapshot?.pendingMutations
                observation.set(pending?.count == 1)
            }
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        let result = await store.record(
            .completed(quantity: 20, unit: "pages", note: "felt steady", occurredAt: Self.now),
            for: occurrence
        )

        #expect(result == .success)
        #expect(observation.value)
        #expect(store.pendingMutations.isEmpty)
        #expect(store.occurrences.first?.outcome?.status == .completed)
        let disk = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(disk.pendingMutations.isEmpty)
        #expect(disk.occurrences.first?.outcome?.note == "felt steady")
    }

    @Test("ambiguous network loss survives process death and replays the same operation")
    func processDeathReplay() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            outcomeMode: .offline
        )
        let first = makeStore(context: context, transport: firstTransport)
        #expect(await first.activate() == .success)
        #expect(await first.record(.completed(occurredAt: Self.now), for: occurrence) == .offline)
        let queued = try #require(context.persistence.loadRevisioned().snapshot?.pendingMutations.first)

        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(changes: [], nextCursor: "cursor-two", hasMore: false)],
            outcomeMode: .replayed
        )
        let second = makeStore(context: context, transport: secondTransport)
        #expect(await second.activate() == .success)

        let request = try #require(await secondTransport.outcomeRequests().first)
        #expect(request.command.operationID == queued.id)
        #expect(request.idempotencyKey == queued.idempotencyKey)
        #expect(second.pendingMutations.isEmpty)
        #expect(second.occurrences.first?.outcome?.status == .completed)
    }

    @Test("a revision conflict keeps the encrypted edit until the user resolves it")
    func conflictRemainsDurable() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            outcomeMode: .conflict
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.record(.skipped(note: "not today", occurredAt: Self.now), for: occurrence) == .conflict)
        let pending = try #require(store.pendingMutations.first)
        #expect(pending.conflictDetected)
        #expect(try context.persistence.loadRevisioned().snapshot?.pendingMutations.first?.conflictDetected == true)
        #expect(store.status.phase == .attentionRequired)
    }

    @Test("a conflict discovered during process-death replay becomes a durable review item")
    func replayConflictRemainsDurable() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            outcomeMode: .offline
        )
        let first = makeStore(context: context, transport: firstTransport)
        #expect(await first.activate() == .success)
        #expect(await first.record(.completed(occurredAt: Self.now), for: occurrence) == .offline)

        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(changes: [], nextCursor: "cursor-two", hasMore: false)],
            outcomeMode: .conflict
        )
        let second = makeStore(context: context, transport: secondTransport)

        #expect(await second.activate() == .conflict)
        #expect(second.pendingMutations.first?.conflictDetected == true)
        #expect(try context.persistence.loadRevisioned().snapshot?
            .pendingMutations.first?.conflictDetected == true)
        #expect(await secondTransport.deltaCursors() == ["cursor-one"])
    }

    @Test("pending data bound to another connection is never exposed or transmitted")
    func originMismatchFailsClosed() async throws {
        let context = try Context()
        defer { context.remove() }
        _ = try context.persistence.save(
            Self.snapshotWithPending(binding: "origin-a|auth=device-a"),
            expectedRevision: .missing
        )
        let transport = HabitTransportStub(configurationIdentifier: "origin-b|auth=device-b")
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .configurationChanged)
        #expect(store.occurrences.isEmpty)
        #expect(store.pendingMutations.isEmpty)
        #expect(await transport.outcomeRequests().isEmpty)
        #expect(await transport.deltaCursors().isEmpty)
    }

    @Test("rotating the live API binding immediately scrubs the old private projection")
    func liveOriginRotationScrubsMemory() async throws {
        let context = try Context()
        defer { context.remove() }
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence(note: "private note"))],
                nextCursor: "cursor-one",
                hasMore: false
            )]
        )
        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-b|auth=device-b"
        )
        let selection = HabitConnectionSelection(firstTransport)
        let store = HabitSyncStore(
            persistence: context.persistence,
            connectionProvider: {
                .init(
                    configurationIdentifier: selection.transport.configurationIdentifier,
                    transport: selection.transport
                )
            },
            now: { Self.now }
        )
        #expect(await store.activate() == .success)
        #expect(store.occurrences.first?.outcome?.note == "private note")

        selection.transport = secondTransport
        #expect(await store.sync() == .configurationChanged)
        #expect(store.occurrences.isEmpty)
        #expect(store.pendingMutations.isEmpty)
        #expect(store.analytics.isEmpty)
        #expect(await secondTransport.deltaCursors().isEmpty)
    }

    @Test("privacy suspension clears notes from memory but preserves encrypted recovery")
    func privacyBoundary() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence(note: "private note")
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )]
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)
        #expect(store.occurrences.first?.outcome?.note == "private note")

        store.suspendForPrivacyBoundary()

        #expect(store.occurrences.isEmpty)
        #expect(store.analytics.isEmpty)
        #expect(store.status.phase == .locked)
        #expect(try context.persistence.loadRevisioned().snapshot?.occurrences.first?.outcome?.note == "private note")
    }

    @Test("a stream hint drains from the encrypted cursor and never installs its own token")
    func streamHintDrainsAuthoritativeDelta() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let authoritative = Self.occurrence(note: "authoritative delta")
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(
                    changes: [.occurrenceUpsert(occurrence)],
                    nextCursor: "encrypted_cursor",
                    hasMore: false
                ),
                .init(
                    changes: [.occurrenceUpsert(authoritative)],
                    nextCursor: "authoritative_head",
                    hasMore: false
                ),
            ]
        )
        let stream = HabitStreamTransportStub(
            events: ["untrusted_hint"],
            completion: .liveEndOfStream
        )
        let store = makeStore(context: context, transport: transport, stream: stream)
        #expect(await store.activate() == .success)

        store.startForegroundPolling(every: .seconds(60))
        defer { store.stopForegroundPolling() }
        try await eventually {
            await transport.deltaCursors().count == 2
        }

        #expect(await transport.deltaCursors() == [nil, "encrypted_cursor"])
        #expect(store.occurrences.first?.outcome?.note == "authoritative delta")
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor == "authoritative_head")
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor != "untrusted_hint")
        #expect(await stream.resumeCursors() == ["encrypted_cursor"])
    }

    @Test("a hint equal to the encrypted cursor causes no authoritative read")
    func alreadyCoveredStreamHintIsIgnored() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence())],
                nextCursor: "cursor_one",
                hasMore: false
            )]
        )
        let stream = HabitStreamTransportStub(
            events: ["cursor_one"],
            completion: .liveEndOfStream
        )
        let store = makeStore(context: context, transport: transport, stream: stream)
        #expect(await store.activate() == .success)

        store.startForegroundPolling(every: .seconds(60))
        defer { store.stopForegroundPolling() }
        try await eventually { await stream.resumeCursors().count == 1 }
        try await Task.sleep(for: .milliseconds(30))

        #expect(await transport.deltaCursors() == [nil])
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor == "cursor_one")
    }

    @Test("a delayed hint cannot cross an API origin or auth-binding rotation")
    func staleOriginStreamHintIsIgnored() async throws {
        let context = try Context()
        defer { context.remove() }
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence(note: "origin a"))],
                nextCursor: "cursor_a",
                hasMore: false
            )]
        )
        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-b|auth=device-b"
        )
        let gate = HabitStreamDeliveryGate()
        let stream = HabitStreamTransportStub(
            events: ["origin_a_hint"],
            completion: .liveEndOfStream,
            deliveryGate: gate
        )
        let selection = HabitStreamConnectionSelection(.init(
            configurationIdentifier: firstTransport.configurationIdentifier,
            transport: firstTransport,
            streamTransport: stream
        ))
        let store = HabitSyncStore(
            persistence: context.persistence,
            connectionProvider: { selection.connection },
            now: { Self.now }
        )
        #expect(await store.activate() == .success)
        store.startForegroundPolling(every: .seconds(60))
        defer { store.stopForegroundPolling() }
        try await eventually { await stream.resumeCursors().count == 1 }

        selection.connection = .init(
            configurationIdentifier: secondTransport.configurationIdentifier,
            transport: secondTransport
        )
        gate.release()
        try await Task.sleep(for: .milliseconds(40))

        #expect(await firstTransport.deltaCursors() == [nil])
        #expect(await secondTransport.deltaCursors().isEmpty)
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor == "cursor_a")
        #expect(store.occurrences.first?.outcome?.note == "origin a")
    }

    @Test("unsupported streaming leaves the independent poll catch-up active")
    func unsupportedStreamKeepsPolling() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(
                    changes: [.occurrenceUpsert(Self.occurrence())],
                    nextCursor: "cursor_one",
                    hasMore: false
                ),
                .init(changes: [], nextCursor: "cursor_two", hasMore: false),
            ]
        )
        let stream = HabitStreamTransportStub(events: [], completion: .unsupported)
        let store = makeStore(context: context, transport: transport, stream: stream)
        #expect(await store.activate() == .success)

        store.startForegroundPolling(every: .milliseconds(20))
        defer { store.stopForegroundPolling() }
        try await eventually { await transport.deltaCursors().count >= 2 }

        #expect(await stream.resumeCursors() == ["cursor_one"])
        #expect(Array((await transport.deltaCursors()).prefix(2)) == [nil, "cursor_one"])
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor == "cursor_two")
    }

    @Test("privacy suspension cancels an open habit stream and scrubs memory")
    func privacyBoundaryCancelsHabitStream() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence(note: "private note"))],
                nextCursor: "cursor_one",
                hasMore: false
            )]
        )
        let stream = HabitStreamTransportStub(holdsOpenUntilCancelled: true)
        let store = makeStore(context: context, transport: transport, stream: stream)
        #expect(await store.activate() == .success)
        store.startForegroundPolling(every: .seconds(60))
        try await eventually { await stream.resumeCursors().count == 1 }

        store.suspendForPrivacyBoundary()
        try await eventually { await stream.wasCancelled() }

        #expect(store.occurrences.isEmpty)
        #expect(store.status.phase == .locked)
        #expect(try context.persistence.loadRevisioned().snapshot?.occurrences.first?.outcome?.note == "private note")
    }

    @Test("analytics refresh caches only the requested deterministic projection")
    func analyticsRefresh() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let analytics = Self.analytics()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            analytics: analytics
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.refreshAnalytics(
            habitIDs: [Self.habitID, Self.habitID],
            startDate: DayWeaveLocalDate("2026-09-01")!,
            endDate: DayWeaveLocalDate("2026-09-30")!,
            bucket: .week
        ) == .success)

        #expect(store.analytics == [analytics])
        #expect(await transport.analyticsHabitIDs() == [Self.habitID])
        #expect(try context.persistence.loadRevisioned().snapshot?.analytics == [analytics])
    }

    @Test("server ledger identity, not planner occurrence identity, owns mutations")
    func ledgerIdentityOwnsMutation() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )]
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)
        #expect(await store.record(.completed(occurredAt: Self.now), for: occurrence) == .success)
        let request = try #require(await transport.outcomeRequests().first)
        #expect(request.occurrenceID == Self.ledgerOccurrenceID)
        #expect(request.occurrenceID != Self.plannerOccurrenceID)
    }

    @Test("an offline pause remains singular and blocks duplicate durable commands")
    func offlinePauseCannotDuplicate() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            pauseOffline: true
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.pause(habitID: Self.habitID) == .offline)
        #expect(store.pendingPauseMutation(forHabitID: Self.habitID) != nil)
        #expect(await store.pause(habitID: Self.habitID) == .conflict)
        #expect(store.pendingMutations.count == 1)
    }

    @Test("pause and resume advance the same immutable ledger pause")
    func pauseResumeLifecycle() async throws {
        let context = try Context()
        defer { context.remove() }
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence())],
                nextCursor: "cursor-one",
                hasMore: false
            )]
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)
        #expect(await store.pause(
            habitID: Self.habitID,
            at: Self.now.addingTimeInterval(-3_600)
        ) == .success)
        let pause = try #require(store.openPause(for: Self.habitID))

        #expect(await store.resume(pause, at: Self.now) == .success)
        #expect(store.openPause(for: Self.habitID) == nil)
        #expect(store.pauses.first?.revision == 2)
        #expect(store.pauses.first?.endedAt == Self.now)
        #expect(store.pendingMutations.isEmpty)
    }

    @Test("delta revisions cannot rewrite immutable occurrence evidence")
    func deltaRejectsChangedEvidence() async throws {
        let context = try Context()
        defer { context.remove() }
        let original = Self.occurrence()
        let changed = Self.occurrence(
            note: "new outcome",
            plannerID: UUID(uuidString: "99999999-9999-4999-8999-999999999999")!
        )
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(
                    changes: [.occurrenceUpsert(original)],
                    nextCursor: "cursor-one",
                    hasMore: true
                ),
                .init(
                    changes: [.occurrenceUpsert(changed)],
                    nextCursor: "cursor-two",
                    hasMore: false
                ),
            ]
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .protocolFailure)
        #expect(store.occurrences == [original])
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor == "cursor-one")
    }

    @Test("an editor holding stale occurrence evidence cannot overwrite refreshed progress")
    func staleOccurrenceIsRejectedBeforeOutbox() async throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(occurrence)],
                nextCursor: "cursor-one",
                hasMore: false
            )]
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.record(
            .completed(occurredAt: Self.now),
            for: Self.occurrence(note: "older editor projection")
        ) == .conflict)
        #expect(store.pendingMutations.isEmpty)
        #expect(await transport.outcomeRequests().isEmpty)
    }

    private func makeStore(
        context: Context,
        transport: HabitTransportStub,
        stream: (any DayWeaveHabitStreamTransport)? = nil
    ) -> HabitSyncStore {
        HabitSyncStore(
            persistence: context.persistence,
            connectionProvider: {
                .init(
                    configurationIdentifier: transport.configurationIdentifier,
                    transport: transport,
                    streamTransport: stream
                )
            },
            now: { Self.now },
            makeUUID: UUID.init
        )
    }

    private func eventually(
        _ predicate: @escaping @MainActor () async -> Bool
    ) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(2))
        while !(await predicate()), ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await predicate())
    }

    nonisolated fileprivate static let now = date("2026-09-04T12:30:00.123456Z")
    nonisolated fileprivate static let habitID = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")!
    nonisolated fileprivate static let ledgerOccurrenceID = UUID(uuidString: "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb")!
    nonisolated fileprivate static let plannerOccurrenceID = UUID(uuidString: "cccccccc-3333-4333-8333-cccccccccccc")!

    nonisolated fileprivate static func occurrence(
        note: String? = nil,
        plannerID: UUID = plannerOccurrenceID
    ) -> DayWeaveHabitOccurrence {
        .init(
            evidence: .init(
                id: ledgerOccurrenceID,
                habitID: habitID,
                plannerOccurrenceID: plannerID,
                sourceScheduleRevisionID: UUID(uuidString: "dddddddd-4444-4444-8444-dddddddddddd")!,
                sourceItemRevision: 3,
                policyFingerprint: "sha256:\(String(repeating: "a", count: 64))",
                identity: .object([:]),
                nominalStart: date("2026-09-04T12:00:00.000000Z"),
                nominalEnd: date("2026-09-04T13:00:00.000000Z"),
                windowStart: date("2026-09-04T11:00:00.000000Z"),
                windowEnd: date("2026-09-04T14:00:00.000000Z"),
                localDate: DayWeaveLocalDate("2026-09-04")!,
                timezoneName: "Europe/Paris",
                expectedDurationSeconds: 3_600,
                expectedQuantity: 20,
                expectedUnit: "pages"
            ),
            outcome: note.map {
                .init(
                    revision: 1,
                    status: .partial,
                    progressBasisPoints: 2_500,
                    quantity: 5,
                    unit: "pages",
                    actualSeconds: 600,
                    note: $0,
                    occurredAt: now,
                    updatedAt: now
                )
            }
        )
    }

    private static func analytics() -> DayWeaveHabitAnalytics {
        let totals = DayWeaveHabitAnalyticsTotals(
            expected: 5,
            eligible: 4,
            completed: 3,
            partial: 1,
            skipped: 0,
            missed: 0,
            excused: 1,
            unresolved: 0,
            adherenceBasisPoints: 8_125,
            actualSecondsTotal: 7_200,
            quantityTotals: [.init(unit: "pages", amount: 60)]
        )
        return .init(
            habitID: habitID,
            startDate: DayWeaveLocalDate("2026-09-01")!,
            endDate: DayWeaveLocalDate("2026-09-30")!,
            bucket: .week,
            totals: totals,
            currentStreak: 2,
            longestStreak: 5,
            trends: [.init(
                startDate: DayWeaveLocalDate("2026-09-01")!,
                endDate: DayWeaveLocalDate("2026-09-07")!,
                totals: totals
            )],
            supportiveFactCodes: [.activeStreak, .strongAdherence]
        )
    }

    private static func snapshotWithPending(binding: String) -> DayWeaveHabitClientSnapshot {
        let occurrence = occurrence()
        let operationID = UUID(uuidString: "eeeeeeee-5555-4555-8555-eeeeeeeeeeee")!
        return .init(
            savedAt: now,
            configurationIdentifier: binding,
            deltaCursor: "cursor-one",
            occurrences: [occurrence],
            pauses: [],
            analytics: [],
            pendingMutations: [.outcome(.init(
                habitID: habitID,
                occurrenceID: ledgerOccurrenceID,
                idempotencyKey: "habit-occurrence:\(operationID.uuidString.lowercased())",
                command: .init(
                    operationID: operationID,
                    expectedRevision: 0,
                    outcome: .completed(occurredAt: now)
                ),
                createdAt: now,
                conflictDetected: false
            ))]
        )
    }

    nonisolated private static func date(_ text: String) -> Date {
        CanonicalRFC3339Instant(text)!.exactlyRepresentableDate!
    }

    private struct Context {
        let root: URL
        let persistence: EncryptedHabitPersistence

        init() throws {
            root = FileManager.default.temporaryDirectory
                .appendingPathComponent("DayWeaveHabitStoreTests-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            let key = try PlannerEncryptionKey(data: Data(repeating: 4, count: 32))
            persistence = .init(
                fileURL: root.appendingPathComponent("habits.snapshot.encrypted"),
                key: key
            )
        }

        func remove() { try? FileManager.default.removeItem(at: root) }
    }
}

private final class LockedFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var stored = false
    var value: Bool { lock.withLock { stored } }
    func set(_ value: Bool) { lock.withLock { stored = value } }
}

@MainActor
private final class HabitConnectionSelection {
    var transport: HabitTransportStub
    init(_ transport: HabitTransportStub) { self.transport = transport }
}

@MainActor
private final class HabitStreamConnectionSelection {
    var connection: DayWeaveHabitConnection
    init(_ connection: DayWeaveHabitConnection) { self.connection = connection }
}

private final class HabitTransportStub: DayWeaveHabitTransport, @unchecked Sendable {
    enum OutcomeMode: Equatable, Sendable { case success, replayed, offline, conflict }

    struct OutcomeRequest: Sendable {
        let occurrenceID: UUID
        let command: DayWeaveHabitOutcomeCommand
        let idempotencyKey: String
    }

    final class State: @unchecked Sendable {
        private let lock = NSLock()
        var pages: [DayWeaveHabitDeltaPage]
        var requests: [OutcomeRequest] = []
        var cursors: [String?] = []
        var analyticIDs: [UUID] = []

        init(pages: [DayWeaveHabitDeltaPage]) { self.pages = pages }

        func nextDelta(cursor: String?) -> DayWeaveHabitDeltaPage {
            lock.withLock {
                cursors.append(cursor)
                if !pages.isEmpty { return pages.removeFirst() }
                return .init(changes: [], nextCursor: cursor ?? "cursor-empty", hasMore: false)
            }
        }

        func add(_ request: OutcomeRequest) { lock.withLock { requests.append(request) } }
        func addAnalytics(_ id: UUID) { lock.withLock { analyticIDs.append(id) } }
        func outcomeRequests() -> [OutcomeRequest] { lock.withLock { requests } }
        func deltaCursors() -> [String?] { lock.withLock { cursors } }
        func analyticsIDs() -> [UUID] { lock.withLock { analyticIDs } }
    }

    nonisolated let configurationIdentifier: String
    private let state: State
    private let outcomeMode: OutcomeMode
    private let beforeOutcome: @Sendable () -> Void
    private let analyticsValue: DayWeaveHabitAnalytics?
    private let pauseOffline: Bool

    init(
        configurationIdentifier: String,
        deltaPages: [DayWeaveHabitDeltaPage] = [],
        outcomeMode: OutcomeMode = .success,
        analytics: DayWeaveHabitAnalytics? = nil,
        pauseOffline: Bool = false,
        beforeOutcome: @escaping @Sendable () -> Void = {}
    ) {
        self.configurationIdentifier = configurationIdentifier
        state = .init(pages: deltaPages)
        self.outcomeMode = outcomeMode
        analyticsValue = analytics
        self.pauseOffline = pauseOffline
        self.beforeOutcome = beforeOutcome
    }

    func habitOccurrences(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        cursor: String?,
        limit: Int
    ) async throws -> DayWeaveHabitOccurrencePage {
        .init(occurrences: [], nextCursor: nil, hasMore: false)
    }

    func putHabitOutcome(
        habitID: UUID,
        occurrenceID: UUID,
        command: DayWeaveHabitOutcomeCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitOccurrenceMutationResponse {
        state.add(.init(
            occurrenceID: occurrenceID,
            command: command,
            idempotencyKey: idempotencyKey
        ))
        beforeOutcome()
        switch outcomeMode {
        case .offline:
            throw DayWeaveAPIError.transport(.networkConnectionLost)
        case .conflict:
            throw DayWeaveAPIError.server(
                statusCode: 409,
                code: "conflict",
                message: "habit revision conflict",
                requestID: nil
            )
        case .success, .replayed:
            let base = HabitSyncStoreTests.occurrence()
            let outcome = DayWeaveHabitOutcome(
                revision: command.expectedRevision + 1,
                status: command.outcome.status,
                progressBasisPoints: command.outcome.progressBasisPoints,
                quantity: command.outcome.quantity,
                unit: command.outcome.unit,
                actualSeconds: command.outcome.actualSeconds,
                note: command.outcome.note,
                occurredAt: command.outcome.occurredAt,
                updatedAt: HabitSyncStoreTests.now
            )
            return .init(
                occurrence: .init(evidence: base.evidence, outcome: outcome),
                replayed: outcomeMode == .replayed
            )
        }
    }

    func startHabitPause(
        habitID: UUID,
        command: DayWeaveHabitPauseStartCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitPauseMutationResponse {
        if pauseOffline { throw DayWeaveAPIError.transport(.networkConnectionLost) }
        return .init(
            pause: .init(
                id: command.pauseID,
                habitID: habitID,
                revision: 1,
                startedAt: command.startedAt,
                endedAt: nil,
                preservesStreak: true,
                createdAt: command.startedAt,
                updatedAt: command.startedAt
            ),
            replayed: false
        )
    }

    func resumeHabitPause(
        habitID: UUID,
        pauseID: UUID,
        command: DayWeaveHabitPauseResumeCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitPauseMutationResponse {
        .init(
            pause: .init(
                id: pauseID,
                habitID: habitID,
                revision: command.expectedRevision + 1,
                startedAt: HabitSyncStoreTests.now.addingTimeInterval(-3_600),
                endedAt: command.endedAt,
                preservesStreak: true,
                createdAt: HabitSyncStoreTests.now.addingTimeInterval(-3_600),
                updatedAt: command.endedAt
            ),
            replayed: false
        )
    }

    func habitDelta(cursor: String?, limit: Int) async throws -> DayWeaveHabitDeltaPage {
        state.nextDelta(cursor: cursor)
    }

    func habitAnalytics(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        bucket: DayWeaveHabitAnalyticsBucket
    ) async throws -> DayWeaveHabitAnalytics {
        state.addAnalytics(habitID)
        guard let analyticsValue else { throw DayWeaveAPIError.responseDecodingFailed }
        return analyticsValue
    }

    func outcomeRequests() async -> [OutcomeRequest] { state.outcomeRequests() }
    func deltaCursors() async -> [String?] { state.deltaCursors() }
    func analyticsHabitIDs() async -> [UUID] { state.analyticsIDs() }
}

private final class HabitStreamTransportStub: DayWeaveHabitStreamTransport, @unchecked Sendable {
    private final class State: @unchecked Sendable {
        private let lock = NSLock()
        private var cursors: [String] = []
        private var cancelled = false

        func append(_ cursor: String) { lock.withLock { cursors.append(cursor) } }
        func markCancelled() { lock.withLock { cancelled = true } }
        func resumeCursors() -> [String] { lock.withLock { cursors } }
        func wasCancelled() -> Bool { lock.withLock { cancelled } }
    }

    private let state = State()
    private let events: [String]
    private let completion: DayWeaveHabitStreamCompletion
    private let holdsOpenUntilCancelled: Bool
    private let deliveryGate: HabitStreamDeliveryGate?

    init(
        events: [String] = [],
        completion: DayWeaveHabitStreamCompletion = .endOfStream,
        holdsOpenUntilCancelled: Bool = false,
        deliveryGate: HabitStreamDeliveryGate? = nil
    ) {
        self.events = events
        self.completion = completion
        self.holdsOpenUntilCancelled = holdsOpenUntilCancelled
        self.deliveryGate = deliveryGate
    }

    func consumeHabitInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveHabitStreamCompletion {
        state.append(cursor)
        if let deliveryGate { await deliveryGate.wait() }
        for event in events { await receive(event) }
        guard holdsOpenUntilCancelled else { return completion }
        return try await withTaskCancellationHandler {
            try await Task.sleep(for: .seconds(3_600))
            return completion
        } onCancel: {
            state.markCancelled()
        }
    }

    func resumeCursors() async -> [String] { state.resumeCursors() }
    func wasCancelled() async -> Bool { state.wasCancelled() }
}

private final class HabitStreamDeliveryGate: @unchecked Sendable {
    private let lock = NSLock()
    private var isReleased = false
    private var continuation: CheckedContinuation<Void, Never>?

    func wait() async {
        await withCheckedContinuation { continuation in
            let resumeNow = lock.withLock {
                if isReleased { return true }
                self.continuation = continuation
                return false
            }
            if resumeNow { continuation.resume() }
        }
    }

    func release() {
        let pending = lock.withLock {
            isReleased = true
            defer { continuation = nil }
            return continuation
        }
        pending?.resume()
    }
}
#endif
