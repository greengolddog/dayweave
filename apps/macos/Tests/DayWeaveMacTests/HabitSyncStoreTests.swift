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
        #expect(disk.deltaCaughtUp)
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

    @Test("an outcome acknowledgement merges an independently advanced missed decision")
    func outcomeResponseMergesAdvancedMissedCoordinate() async throws {
        let context = try Context()
        defer { context.remove() }
        let decision = Self.occurrence(missedResolution: Self.missedResolution())
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(decision)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            outcomeMode: .advancedMissed
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        let partial = DayWeaveHabitOutcomeInput(
            status: .partial,
            progressBasisPoints: 5_000,
            occurredAt: Self.now
        )
        #expect(await store.record(partial, for: decision) == .success)
        #expect(store.pendingMutations.isEmpty)
        #expect(store.occurrences.first?.outcome?.progressBasisPoints == 5_000)
        guard case .carry = store.occurrences.first?.missedResolution?.action else {
            Issue.record("Expected the independently advanced missed coordinate")
            return
        }
        #expect(store.occurrences.first?.missedResolution?.revision == 2)
        #expect(try context.persistence.loadRevisioned().snapshot?
            .occurrences.first?.missedResolution?.revision == 2)
    }

    @Test("an invalid missed coordinate in an outcome acknowledgement keeps the journal")
    func outcomeResponseRejectsDivergentMissedCoordinate() async throws {
        for mode in [HabitTransportStub.OutcomeMode.divergentMissed,
                     HabitTransportStub.OutcomeMode.unreachableMissed] {
            let context = try Context()
            defer { context.remove() }
            let decision = Self.occurrence(missedResolution: Self.missedResolution())
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(
                    changes: [.occurrenceUpsert(decision)],
                    nextCursor: "cursor-one",
                    hasMore: false
                )],
                outcomeMode: mode
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)

            let partial = DayWeaveHabitOutcomeInput(
                status: .partial,
                progressBasisPoints: 5_000,
                occurredAt: Self.now
            )
            #expect(await store.record(partial, for: decision) == .protocolFailure)
            #expect(store.pendingMutations.count == 1)
            #expect(store.occurrences.first?.outcome == nil)
            #expect(store.occurrences.first?.missedResolution == decision.missedResolution)
        }
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
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCaughtUp == false)
        #expect(store.status.phase == .attentionRequired)
    }

    @Test("missed reconciliation is encrypted before transport and replays after process death")
    func missedReconcileProcessDeathReplay() async throws {
        let context = try Context()
        defer { context.remove() }
        let persistedBeforeTransport = LockedFlag()
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            missedReconcileMode: .offline,
            beforeMissedReconcile: {
                let pending = try? context.persistence.loadRevisioned().snapshot?.pendingMutations
                persistedBeforeTransport.set(pending?.contains(where: {
                    if case .missedReconcile = $0 { return true }
                    return false
                }) == true)
            }
        )
        let first = makeStore(context: context, transport: firstTransport)

        #expect(await first.activate() == .offline)
        #expect(persistedBeforeTransport.value)
        let queued = try #require(context.persistence.loadRevisioned().snapshot?
            .pendingMutations.first)
        guard case let .missedReconcile(saved) = queued else {
            Issue.record("Expected a durable missed-reconcile request")
            return
        }

        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(changes: [], nextCursor: "cursor-reconciled", hasMore: false)],
            missedReconcileMode: .replayed
        )
        let second = makeStore(context: context, transport: secondTransport)
        #expect(await second.activate() == .success)
        let replay = try #require(await secondTransport.missedReconcileRequests().first)
        #expect(replay.command == saved.command)
        #expect(replay.limit == saved.limit)
        #expect(replay.idempotencyKey == saved.idempotencyKey)
        #expect(second.pendingMutations.isEmpty)
    }

    @Test("an expired no-op reconcile journal rotates while delta authority stays revoked")
    func expiredMissedReconcileJournalRotates() async throws {
        let context = try Context()
        defer { context.remove() }
        let expiredID = UUID(uuidString: "eeeeeeee-5555-4555-8555-eeeeeeeeeeee")!
        let expired = DayWeavePendingHabitMutation.missedReconcile(.init(
            idempotencyKey: "habit-missed-reconcile:expired",
            command: .init(operationID: expiredID),
            limit: 200,
            createdAt: Self.now.addingTimeInterval(
                -HabitSyncStore.missedReconcileJournalLease - 1
            ),
            conflictDetected: false
        ))
        _ = try context.persistence.save(.init(
            savedAt: Self.now,
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: "cursor-one",
            deltaCaughtUp: true,
            occurrences: [],
            pauses: [],
            analytics: [],
            pendingMutations: [expired]
        ), expectedRevision: .missing)
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            missedReconcileMode: .offline
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .offline)
        let requests = await transport.missedReconcileRequests()
        #expect(requests.count == 1)
        #expect(requests.first?.command.operationID != expiredID)
        let disk = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(!disk.deltaCaughtUp)
        #expect(disk.pendingMutations.count == 1)
        guard case let .missedReconcile(replacement) =
                try #require(disk.pendingMutations.first) else {
            Issue.record("Expected a fresh reconcile lease")
            return
        }
        #expect(replacement.command.operationID != expiredID)
        #expect(replacement.createdAt == Self.now)
    }

    @Test("an expired automatic reconcile journal cannot pin an obsolete API binding")
    func expiredMissedReconcileJournalDoesNotPinBinding() async throws {
        let context = try Context()
        defer { context.remove() }
        let expiredID = UUID(uuidString: "eeeeeeee-5555-4555-8555-eeeeeeeeeeee")!
        let expired = DayWeavePendingHabitMutation.missedReconcile(.init(
            idempotencyKey: "habit-missed-reconcile:expired-binding",
            command: .init(operationID: expiredID),
            limit: 200,
            createdAt: Self.now.addingTimeInterval(
                -HabitSyncStore.missedReconcileJournalLease - 1
            ),
            conflictDetected: false
        ))
        _ = try context.persistence.save(.init(
            savedAt: Self.now,
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: "cursor-private",
            deltaCaughtUp: true,
            occurrences: [Self.occurrence(note: "old connection")],
            pauses: [],
            analytics: [],
            pendingMutations: [expired]
        ), expectedRevision: .missing)
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-b|auth=device-b",
            missedReconcileMode: .offline
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .offline)
        #expect(store.occurrences.isEmpty)
        let request = try #require(await transport.missedReconcileRequests().first)
        #expect(request.command.operationID != expiredID)
        let disk = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(disk.configurationIdentifier == "origin-b|auth=device-b")
        #expect(disk.deltaCursor == nil)
        #expect(!disk.deltaCaughtUp)
        guard case let .missedReconcile(replacement) =
                try #require(disk.pendingMutations.first) else {
            Issue.record("Expected a fresh reconcile lease for the new binding")
            return
        }
        #expect(replacement.command.operationID == request.command.operationID)
    }

    @Test("a missed choice is durable before transport and installs only the derived response")
    func missedChoiceMutationFence() async throws {
        let context = try Context()
        defer { context.remove() }
        let decision = Self.occurrence(missedResolution: Self.missedResolution())
        let persistedBeforeTransport = LockedFlag()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(decision)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            beforeMissedResolution: {
                let pending = try? context.persistence.loadRevisioned().snapshot?.pendingMutations
                persistedBeforeTransport.set(pending?.contains(where: {
                    if case .missedResolution = $0 { return true }
                    return false
                }) == true)
            }
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.resolveMissed(decision, action: .carry) == .success)
        #expect(persistedBeforeTransport.value)
        guard case let .carry(windowStart, windowEnd) = try #require(
            store.occurrences.first?.missedResolution?.action
        ) else {
            Issue.record("Expected the server-derived carry window")
            return
        }
        #expect(windowStart == Self.now)
        #expect(windowEnd == Self.now.addingTimeInterval(86_400))
        #expect(store.pendingMutations.isEmpty)
    }

    @Test("a successful missed choice stays non-authoritative until terminal delta catch-up")
    func missedChoiceRequiresDeltaCatchUp() async throws {
        let context = try Context()
        defer { context.remove() }
        let decision = Self.occurrence(missedResolution: Self.missedResolution())
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(decision)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            deltaFailureAtCall: 1
        )
        let first = makeStore(context: context, transport: firstTransport)
        #expect(await first.activate() == .success)

        #expect(await first.resolveMissed(decision, action: .carry) == .offline)
        #expect(first.pendingMutations.isEmpty)
        guard case .carry = first.occurrences.first?.missedResolution?.action else {
            Issue.record("Expected the durable server response before delta failure")
            return
        }
        let incomplete = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(!incomplete.deltaCaughtUp)
        #expect(!first.habitCompositionCheckpoint.deltaCaughtUp)

        let carried = Self.occurrence(missedResolution: Self.missedResolution(
            action: .carry(
                windowStart: Self.now,
                windowEnd: Self.now.addingTimeInterval(86_400)
            ),
            revision: 2,
            updatedAt: Self.now
        ))
        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(carried)],
                nextCursor: "cursor-two",
                hasMore: false
            )]
        )
        let second = makeStore(context: context, transport: secondTransport)
        #expect(await second.activate() == .success)
        #expect(second.habitCompositionCheckpoint.deltaCaughtUp)
        #expect(second.occurrences.first == carried)
    }

    @Test("an ambiguous missed choice replays the exact operation after process death")
    func missedChoiceProcessDeathReplay() async throws {
        let context = try Context()
        defer { context.remove() }
        let decision = Self.occurrence(missedResolution: Self.missedResolution())
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(decision)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            missedResolutionMode: .offline
        )
        let first = makeStore(context: context, transport: firstTransport)
        #expect(await first.activate() == .success)
        #expect(await first.resolveMissed(decision, action: .skip) == .offline)
        let queued = try #require(context.persistence.loadRevisioned().snapshot?
            .pendingMutations.first)

        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(changes: [], nextCursor: "cursor-two", hasMore: false)],
            missedResolutionMode: .replayed
        )
        let second = makeStore(context: context, transport: secondTransport)
        #expect(await second.activate() == .success)
        let replay = try #require(await secondTransport.missedResolutionRequests().first)
        #expect(replay.command.operationID == queued.id)
        #expect(replay.idempotencyKey == queued.idempotencyKey)
        #expect(second.occurrences.first?.missedResolution?.action == .skip)
        #expect(second.pendingMutations.isEmpty)
    }

    @Test("a missed-choice conflict remains encrypted for explicit review")
    func missedChoiceConflictRemainsDurable() async throws {
        let context = try Context()
        defer { context.remove() }
        let decision = Self.occurrence(missedResolution: Self.missedResolution())
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(decision)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            missedResolutionMode: .conflict
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.resolveMissed(decision, action: .reduceFrequency) == .conflict)
        #expect(store.pendingMutations.first?.conflictDetected == true)
        #expect(try context.persistence.loadRevisioned().snapshot?
            .pendingMutations.first?.conflictDetected == true)
        #expect(store.occurrences.first?.missedResolution?.action.isDecisionRequired == true)
    }

    @Test("a direct missed choice accepts a matching server race cancellation")
    func missedChoiceRaceCancellation() async throws {
        let context = try Context()
        defer { context.remove() }
        let decision = Self.occurrence(missedResolution: Self.missedResolution())
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(decision)],
                nextCursor: "cursor-one",
                hasMore: false
            )],
            missedResolutionMode: .cancelled
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.resolveMissed(decision, action: .carry) == .success)
        guard case let .cancelled(reason, resumeAction) = try #require(
            store.occurrences.first?.missedResolution?.action
        ) else {
            Issue.record("Expected a durable race cancellation")
            return
        }
        #expect(reason == .sourceCompleted)
        #expect(resumeAction == .carry)
    }

    @Test("terminal outcome or overlapping pause blocks a stale missed decision locally")
    func staleMissedChoiceIsNotJournaled() async throws {
        do {
            let context = try Context()
            defer { context.remove() }
            let decision = Self.occurrence(
                completed: true,
                missedResolution: Self.missedResolution()
            )
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(
                    changes: [.occurrenceUpsert(decision)],
                    nextCursor: "cursor-one",
                    hasMore: false
                )]
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)
            #expect(await store.resolveMissed(decision, action: .skip) == .conflict)
            #expect(store.pendingMutations.isEmpty)
            #expect(await transport.missedResolutionRequests().isEmpty)
        }

        do {
            let context = try Context()
            defer { context.remove() }
            let decision = Self.occurrence(missedResolution: Self.missedResolution())
            let pause = Self.pause(
                id: UUID(),
                startedAt: decision.evidence.windowStart,
                endedAt: decision.evidence.windowEnd
            )
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(
                    changes: [.occurrenceUpsert(decision), .pauseUpsert(pause)],
                    nextCursor: "cursor-one",
                    hasMore: false
                )]
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)
            #expect(await store.resolveMissed(decision, action: .carry) == .conflict)
            #expect(store.pendingMutations.isEmpty)
            #expect(await transport.missedResolutionRequests().isEmpty)
        }
    }

    @Test("discarding a reviewed conflict stays incomplete until a terminal delta commits")
    func discardedConflictRemainsFailClosedWhenDeltaFails() async throws {
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
            outcomeMode: .conflict,
            deltaFailureAtCall: 1
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)
        #expect(await store.record(.skipped(occurredAt: Self.now), for: occurrence) == .conflict)
        let pendingID = try #require(store.pendingMutations.first?.id)

        #expect(await store.discardPendingMutation(pendingID) == .offline)
        let disk = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(disk.pendingMutations.isEmpty)
        #expect(!disk.deltaCaughtUp)
        #expect(!store.habitCompositionCheckpoint.deltaCaughtUp)
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

    @Test("a new stream hint revokes durable authority before its queued delta begins")
    func streamHintImmediatelyRevokesCompositionAuthority() async throws {
        let context = try Context()
        defer { context.remove() }
        let observedFailClosedBeforeDelta = LockedFlag()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence())],
                nextCursor: "cursor_one",
                hasMore: false
            )],
            deltaFailureAtCall: 1,
            beforeDelta: { cursor in
                guard cursor == "cursor_one" else { return }
                let caughtUp = try? context.persistence.loadRevisioned().snapshot?.deltaCaughtUp
                observedFailClosedBeforeDelta.set(caughtUp == false)
            }
        )
        let stream = HabitStreamTransportStub(
            events: ["opaque_new_hint"],
            completion: .liveEndOfStream
        )
        let store = makeStore(context: context, transport: transport, stream: stream)
        #expect(await store.activate() == .success)

        store.startForegroundPolling(every: .seconds(60))
        defer { store.stopForegroundPolling() }
        try await eventually { observedFailClosedBeforeDelta.value }

        #expect(!store.habitCompositionCheckpoint.deltaCaughtUp)
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCaughtUp == false)
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCursor == "cursor_one")
    }

    @Test("a terminal delta racing a newer opaque hint stays durably incomplete")
    func inFlightHintRequiresAnotherAuthoritativeDelta() async throws {
        let context = try Context()
        defer { context.remove() }
        let secondPageGate = HabitDeltaResponseGate()
        let thirdPageGate = HabitDeltaResponseGate()
        let streamDeliveryGate = HabitStreamDeliveryGate()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(
                    changes: [.occurrenceUpsert(Self.occurrence())],
                    nextCursor: "cursor_one",
                    hasMore: false
                ),
                .init(changes: [], nextCursor: "cursor_two", hasMore: false),
                .init(changes: [], nextCursor: "cursor_three", hasMore: false),
            ],
            beforeDelta: { cursor in
                if cursor == "cursor_one" { await secondPageGate.wait() }
                if cursor == "cursor_two" { await thirdPageGate.wait() }
            }
        )
        let stream = HabitStreamTransportStub(
            events: ["cursor_three"],
            completion: .liveEndOfStream,
            deliveryGate: streamDeliveryGate
        )
        let store = makeStore(context: context, transport: transport, stream: stream)
        #expect(await store.activate() == .success)
        store.startForegroundPolling(every: .seconds(60))
        defer { store.stopForegroundPolling() }
        try await eventually { await stream.resumeCursors() == ["cursor_one"] }

        let syncTask = Task { @MainActor in await store.sync() }
        await secondPageGate.waitUntilEntered()
        streamDeliveryGate.release()
        try await eventually {
            (try? context.persistence.loadRevisioned().snapshot?.deltaCaughtUp) == false
        }
        await secondPageGate.release()
        await thirdPageGate.waitUntilEntered()

        let intermediate = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(intermediate.deltaCursor == "cursor_two")
        #expect(!intermediate.deltaCaughtUp)
        #expect(!store.habitCompositionCheckpoint.deltaCaughtUp)

        await thirdPageGate.release()
        #expect(await syncTask.value == .success)
        let terminal = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(terminal.deltaCursor == "cursor_three")
        #expect(terminal.deltaCaughtUp)
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

    @Test("staging outcome and pause writes invalidates analytics only for the affected habit")
    func stagedMutationsInvalidateAffectedAnalyticsOnly() async throws {
        let otherHabit = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaab")!
        let otherAnalytics = Self.analytics(habitID: otherHabit)

        do {
            let context = try Context()
            defer { context.remove() }
            let occurrence = Self.occurrence()
            _ = try context.persistence.save(.init(
                savedAt: Self.now,
                configurationIdentifier: "origin-a|auth=device-a",
                deltaCursor: "cursor-one",
                deltaCaughtUp: true,
                occurrences: [occurrence],
                pauses: [],
                analytics: [Self.analytics(), otherAnalytics],
                pendingMutations: []
            ), expectedRevision: .missing)
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(changes: [], nextCursor: "cursor-two", hasMore: false)],
                outcomeMode: .offline
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)

            #expect(await store.record(.completed(occurredAt: Self.now), for: occurrence) == .offline)
            #expect(store.analytics == [otherAnalytics])
            #expect(try context.persistence.loadRevisioned().snapshot?.analytics == [otherAnalytics])
        }

        do {
            let context = try Context()
            defer { context.remove() }
            _ = try context.persistence.save(.init(
                savedAt: Self.now,
                configurationIdentifier: "origin-a|auth=device-a",
                deltaCursor: "cursor-one",
                deltaCaughtUp: true,
                occurrences: [],
                pauses: [],
                analytics: [Self.analytics(), otherAnalytics],
                pendingMutations: []
            ), expectedRevision: .missing)
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(changes: [], nextCursor: "cursor-two", hasMore: false)],
                pauseOffline: true
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)

            #expect(await store.pause(habitID: Self.habitID) == .offline)
            #expect(store.analytics == [otherAnalytics])
            #expect(try context.persistence.loadRevisioned().snapshot?.analytics == [otherAnalytics])
        }
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
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCaughtUp == false)
    }

    @Test("a genesis delta replay validates identity before ignoring an older open pause")
    func deltaValidatesStalePauseIdentityBeforeIgnoring() async throws {
        let context = try Context()
        defer { context.remove() }
        let pauseID = UUID(uuidString: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!
        let startedAt = Self.now.addingTimeInterval(-3_600)
        let closed = Self.pause(
            id: pauseID,
            startedAt: startedAt,
            endedAt: Self.now,
            revision: 2
        )
        let staleOpen = Self.pause(id: pauseID, startedAt: startedAt, revision: 1)
        let changedIdentity = Self.pause(
            id: pauseID,
            habitID: UUID(uuidString: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")!,
            startedAt: startedAt,
            revision: 1
        )
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(changes: [.pauseUpsert(closed)], nextCursor: "cursor-one", hasMore: false),
                .init(
                    changes: [.pauseUpsert(staleOpen), .pauseUpsert(closed)],
                    nextCursor: "cursor-two",
                    hasMore: false
                ),
                .init(
                    changes: [.pauseUpsert(changedIdentity)],
                    nextCursor: "cursor-three",
                    hasMore: false
                ),
            ]
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .success)
        #expect(await store.sync() == .success)
        #expect(store.pauses == [closed])
        #expect(store.habitCompositionCheckpoint.deltaCursor == "cursor-two")
        #expect(store.habitCompositionCheckpoint.deltaCaughtUp)

        #expect(await store.sync() == .protocolFailure)
        #expect(store.pauses == [closed])
        let durable = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(durable.deltaCursor == "cursor-two")
        #expect(!durable.deltaCaughtUp)
    }

    @Test("a higher pause revision cannot reopen or move a closed pause")
    func deltaRejectsChangedClosedPauseEnd() async throws {
        let pauseID = UUID(uuidString: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!
        let startedAt = Self.now.addingTimeInterval(-3_600)
        let closed = Self.pause(
            id: pauseID,
            startedAt: startedAt,
            endedAt: Self.now,
            revision: 2
        )
        let invalidRevisions = [
            Self.pause(id: pauseID, startedAt: startedAt, revision: 3),
            Self.pause(
                id: pauseID,
                startedAt: startedAt,
                endedAt: Self.now.addingTimeInterval(60),
                revision: 3
            ),
        ]

        for invalid in invalidRevisions {
            let context = try Context()
            defer { context.remove() }
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [
                    .init(
                        changes: [.pauseUpsert(closed)],
                        nextCursor: "cursor-one",
                        hasMore: false
                    ),
                    .init(
                        changes: [.pauseUpsert(invalid)],
                        nextCursor: "cursor-two",
                        hasMore: false
                    ),
                ]
            )
            let store = makeStore(context: context, transport: transport)

            #expect(await store.activate() == .success)
            #expect(await store.sync() == .protocolFailure)
            #expect(store.pauses == [closed])
            let durable = try #require(context.persistence.loadRevisioned().snapshot)
            #expect(durable.deltaCursor == "cursor-one")
            #expect(!durable.deltaCaughtUp)
        }
    }

    @Test("an intermediate delta cursor stays incomplete across failure and process death")
    func intermediateDeltaCursorResumesFailClosed() async throws {
        let context = try Context()
        defer { context.remove() }
        let firstTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(
                changes: [.occurrenceUpsert(Self.occurrence())],
                nextCursor: "cursor-one",
                hasMore: true
            )],
            deltaFailureAtCall: 1
        )
        let first = makeStore(context: context, transport: firstTransport)

        #expect(await first.activate() == .offline)
        let intermediate = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(intermediate.deltaCursor == "cursor-one")
        #expect(!intermediate.deltaCaughtUp)

        let secondTransport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [.init(changes: [], nextCursor: "cursor-two", hasMore: false)]
        )
        let relaunched = makeStore(context: context, transport: secondTransport)
        #expect(await relaunched.activate() == .success)
        #expect(await secondTransport.deltaCursors() == ["cursor-one"])
        let complete = try #require(context.persistence.loadRevisioned().snapshot)
        #expect(complete.deltaCursor == "cursor-two")
        #expect(complete.deltaCaughtUp)
    }

    @Test("a delta candidate CAS failure cannot reauthorize the prior terminal cache in memory")
    func deltaPersistenceFailureRevokesProcessAuthority() async throws {
        let context = try Context()
        defer { context.remove() }
        let forcedCASConflict = LockedFlag()
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(
                    changes: [.occurrenceUpsert(Self.occurrence())],
                    nextCursor: "cursor-one",
                    hasMore: false
                ),
                .init(changes: [], nextCursor: "cursor-two", hasMore: false),
            ],
            beforeDelta: { cursor in
                guard cursor == "cursor-one", !forcedCASConflict.value,
                      let loaded = try? context.persistence.loadRevisioned(),
                      let snapshot = loaded.snapshot else { return }
                forcedCASConflict.set(true)
                _ = try? context.persistence.save(snapshot, expectedRevision: loaded.revision)
            }
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.sync() == .localStorageFailure)
        #expect(forcedCASConflict.value)
        #expect(!store.habitCompositionCheckpoint.deltaCaughtUp)
        #expect(try context.persistence.loadRevisioned().snapshot?.deltaCaughtUp == false)
    }

    @Test("a mismatched outcome acknowledgement cannot clear its encrypted journal")
    func mismatchedOutcomeResponseKeepsJournal() async throws {
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
            outcomeMode: .mismatched
        )
        let store = makeStore(context: context, transport: transport)
        #expect(await store.activate() == .success)

        #expect(await store.record(.completed(occurredAt: Self.now), for: occurrence) == .protocolFailure)
        #expect(store.pendingMutations.count == 1)
        #expect(store.occurrences == [occurrence])
        #expect(try context.persistence.loadRevisioned().snapshot?.pendingMutations.count == 1)
    }

    @Test("mismatched pause acknowledgements preserve start and resume journals")
    func mismatchedPauseResponsesKeepJournals() async throws {
        do {
            let context = try Context()
            defer { context.remove() }
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(changes: [], nextCursor: "cursor-one", hasMore: false)],
                pauseResponseMode: .mismatchedStart
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)
            #expect(await store.pause(habitID: Self.habitID) == .protocolFailure)
            #expect(store.pendingMutations.count == 1)
            #expect(store.pauses.isEmpty)
        }

        do {
            let context = try Context()
            defer { context.remove() }
            let openPause = Self.pause(
                id: UUID(uuidString: "f1000000-0000-4000-8000-000000000001")!,
                startedAt: Self.now.addingTimeInterval(-3_600)
            )
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [.init(
                    changes: [.pauseUpsert(openPause)],
                    nextCursor: "cursor-one",
                    hasMore: false
                )],
                pauseResponseMode: .mismatchedResume
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)
            #expect(await store.resume(openPause, at: Self.now) == .protocolFailure)
            #expect(store.pendingMutations.count == 1)
            #expect(store.openPause(for: Self.habitID) == openPause)
        }
    }

    @Test("stable occurrence evidence survives harmless item revision bumps")
    func stableOccurrenceEvidenceAllowsNewerItemRevision() async throws {
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
        let block = ScheduleBlock(
            id: UUID(),
            title: "Stable habit",
            kind: .habit,
            start: Self.now,
            end: Self.now.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: Self.habitID,
            sourceItemRevision: occurrence.evidence.sourceItemRevision + 1,
            occurrenceID: Self.plannerOccurrenceID,
            syncOrigin: .localComposition
        )

        #expect(store.canonicalOccurrence(for: block) == occurrence)
        var olderCanonicalBlock = block
        olderCanonicalBlock.sourceItemRevision = occurrence.evidence.sourceItemRevision - 1
        #expect(store.canonicalOccurrence(for: olderCanonicalBlock) == nil)
    }

    @Test("delta accepts a valid missed-resolution revision jump from a compact projection")
    func missedResolutionRevisionJump() async throws {
        let context = try Context()
        defer { context.remove() }
        let first = Self.occurrence(missedResolution: Self.missedResolution())
        let jumped = Self.occurrence(missedResolution: Self.missedResolution(
            action: .skip,
            revision: 4,
            updatedAt: Self.now
        ))
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(changes: [.occurrenceUpsert(first)], nextCursor: "cursor-one", hasMore: false),
                .init(changes: [.occurrenceUpsert(jumped)], nextCursor: "cursor-two", hasMore: false),
            ]
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .success)
        #expect(await store.sync() == .success)
        #expect(store.occurrences.first?.missedResolution?.revision == 4)
    }

    @Test("compacted missed revisions require an action reachable in the exact revision gap")
    func missedResolutionRevisionJumpUsesExactDistance() async throws {
        func run(
            first: DayWeaveHabitOccurrence,
            jumped: DayWeaveHabitOccurrence
        ) async throws -> HabitSyncOutcome {
            let context = try Context()
            defer { context.remove() }
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [
                    .init(
                        changes: [.occurrenceUpsert(first)],
                        nextCursor: "cursor-one",
                        hasMore: false
                    ),
                    .init(
                        changes: [.occurrenceUpsert(jumped)],
                        nextCursor: "cursor-two",
                        hasMore: false
                    ),
                ]
            )
            let store = makeStore(context: context, transport: transport)
            #expect(await store.activate() == .success)
            return await store.sync()
        }

        let skipped = Self.occurrence(missedResolution: Self.missedResolution(
            action: .skip,
            revision: 2
        ))
        let evenSkip = Self.occurrence(missedResolution: Self.missedResolution(
            action: .skip,
            revision: 4,
            updatedAt: Self.now
        ))
        #expect(try await run(first: skipped, jumped: evenSkip) == .success)

        let oddSkip = Self.occurrence(missedResolution: Self.missedResolution(
            action: .skip,
            revision: 5,
            updatedAt: Self.now
        ))
        #expect(try await run(first: skipped, jumped: oddSkip) == .protocolFailure)

        let carryAction = DayWeaveHabitMissedResolutionAction.carry(
            windowStart: Self.now,
            windowEnd: Self.now.addingTimeInterval(86_400)
        )
        let carried = Self.occurrence(missedResolution: Self.missedResolution(
            action: carryAction,
            revision: 2,
            updatedAt: Self.now
        ))
        let cycledCarry = Self.occurrence(missedResolution: Self.missedResolution(
            action: carryAction,
            revision: 4,
            updatedAt: Self.now
        ))
        #expect(try await run(first: carried, jumped: cycledCarry) == .success)
    }

    @Test("delta rejects a revision jump to an unreachable missed-resolution family")
    func missedResolutionRevisionJumpRequiresReachableAction() async throws {
        let context = try Context()
        defer { context.remove() }
        let skipped = Self.occurrence(missedResolution: Self.missedResolution(
            action: .skip,
            revision: 2
        ))
        let impossibleReprompt = Self.occurrence(missedResolution: Self.missedResolution(
            action: .decisionRequired,
            revision: 4,
            updatedAt: Self.now
        ))
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(changes: [.occurrenceUpsert(skipped)], nextCursor: "cursor-one", hasMore: false),
                .init(
                    changes: [.occurrenceUpsert(impossibleReprompt)],
                    nextCursor: "cursor-two",
                    hasMore: false
                ),
            ]
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .success)
        #expect(await store.sync() == .protocolFailure)
        #expect(store.occurrences.first == skipped)
        #expect(!store.habitCompositionCheckpoint.deltaCaughtUp)
    }

    @Test("delta merges crossed outcome and missed-resolution coordinates independently")
    func crossedHabitCoordinatesMergeIndependently() async throws {
        let context = try Context()
        defer { context.remove() }
        let prior = Self.occurrence(
            note: "private partial",
            missedResolution: Self.missedResolution(
                action: .skip,
                revision: 4,
                updatedAt: Self.now
            )
        )
        let advancedOutcome = DayWeaveHabitOutcome(
            revision: 2,
            status: .partial,
            progressBasisPoints: 5_000,
            quantity: 10,
            unit: "pages",
            actualSeconds: 1_200,
            note: "private correction",
            occurredAt: Self.now,
            updatedAt: Self.now
        )
        let crossed = DayWeaveHabitOccurrence(
            evidence: prior.evidence,
            outcome: advancedOutcome,
            missedResolution: Self.missedResolution(
                action: .skip,
                revision: 3,
                updatedAt: Self.now.addingTimeInterval(-1)
            )
        )
        let transport = HabitTransportStub(
            configurationIdentifier: "origin-a|auth=device-a",
            deltaPages: [
                .init(changes: [.occurrenceUpsert(prior)], nextCursor: "cursor-one", hasMore: false),
                .init(changes: [.occurrenceUpsert(crossed)], nextCursor: "cursor-two", hasMore: false),
            ]
        )
        let store = makeStore(context: context, transport: transport)

        #expect(await store.activate() == .success)
        #expect(await store.sync() == .success)
        let merged = try #require(store.occurrences.first)
        #expect(merged.outcome == advancedOutcome)
        #expect(merged.missedResolution == prior.missedResolution)
        #expect(store.habitCompositionCheckpoint.deltaCaughtUp)
    }

    @Test("a stale missed coordinate must retain its immutable identity and timestamp ordering")
    func staleMissedCoordinateStillValidatesAuthority() async throws {
        let priorResolution = Self.missedResolution(
            action: .skip,
            revision: 4,
            updatedAt: Self.now
        )
        let prior = Self.occurrence(
            note: "private partial",
            missedResolution: priorResolution
        )
        let advancedOutcome = DayWeaveHabitOutcome(
            revision: 2,
            status: .partial,
            progressBasisPoints: 5_000,
            quantity: 10,
            unit: "pages",
            actualSeconds: 1_200,
            note: "private correction",
            occurredAt: Self.now,
            updatedAt: Self.now
        )
        let invalidStaleCoordinates = [
            DayWeaveHabitMissedResolution(
                occurrenceEvidenceID: priorResolution.occurrenceEvidenceID,
                habitID: priorResolution.habitID,
                sourcePlannerOccurrenceID: priorResolution.sourcePlannerOccurrenceID,
                revision: 3,
                configuredPolicy: .skip,
                action: .skip,
                createdAt: priorResolution.createdAt,
                updatedAt: priorResolution.updatedAt.addingTimeInterval(-1)
            ),
            DayWeaveHabitMissedResolution(
                occurrenceEvidenceID: priorResolution.occurrenceEvidenceID,
                habitID: priorResolution.habitID,
                sourcePlannerOccurrenceID: priorResolution.sourcePlannerOccurrenceID,
                revision: 3,
                configuredPolicy: priorResolution.configuredPolicy,
                action: .skip,
                createdAt: priorResolution.createdAt,
                updatedAt: priorResolution.updatedAt.addingTimeInterval(1)
            ),
        ]

        for invalidResolution in invalidStaleCoordinates {
            let context = try Context()
            defer { context.remove() }
            let incoming = DayWeaveHabitOccurrence(
                evidence: prior.evidence,
                outcome: advancedOutcome,
                missedResolution: invalidResolution
            )
            let transport = HabitTransportStub(
                configurationIdentifier: "origin-a|auth=device-a",
                deltaPages: [
                    .init(
                        changes: [.occurrenceUpsert(prior)],
                        nextCursor: "cursor-one",
                        hasMore: false
                    ),
                    .init(
                        changes: [.occurrenceUpsert(incoming)],
                        nextCursor: "cursor-two",
                        hasMore: false
                    ),
                ]
            )
            let store = makeStore(context: context, transport: transport)

            #expect(await store.activate() == .success)
            #expect(await store.sync() == .protocolFailure)
            #expect(store.occurrences == [prior])
            let durable = try #require(context.persistence.loadRevisioned().snapshot)
            #expect(durable.deltaCursor == "cursor-one")
            #expect(!durable.deltaCaughtUp)
        }
    }

    @Test("retention never evicts rows referenced by durable habit journals")
    func retentionProtectsOutboxTargets() throws {
        let oldest = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-20 * 86_400)
        )
        let middle = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-10 * 86_400)
        )
        let newest = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-9 * 86_400)
        )
        let operationID = UUID()
        let pending = DayWeavePendingHabitMutation.outcome(.init(
            habitID: Self.habitID,
            occurrenceID: oldest.id,
            idempotencyKey: "habit-occurrence:\(operationID.uuidString.lowercased())",
            command: .init(
                operationID: operationID,
                expectedRevision: 0,
                outcome: .completed(occurredAt: Self.now)
            ),
            createdAt: Self.now,
            conflictDetected: false
        ))

        let retained = try HabitSyncStore.retainedOccurrences(
            [oldest, middle, newest],
            pendingMutations: [pending],
            referenceDate: Self.now,
            limit: 2
        )
        #expect(retained.contains(where: { $0.id == oldest.id }))
        #expect(retained.contains(where: { $0.id == newest.id }))

        let referencedPause = Self.pause(
            id: UUID(),
            startedAt: Self.now.addingTimeInterval(-10_800),
            endedAt: Self.now.addingTimeInterval(-7_200)
        )
        let newerPause = Self.pause(
            id: UUID(),
            startedAt: Self.now.addingTimeInterval(-3_600),
            endedAt: Self.now.addingTimeInterval(-1_800)
        )
        let resumeOperationID = UUID()
        let reviewedResume = DayWeavePendingHabitMutation.pauseResume(.init(
            habitID: Self.habitID,
            pauseID: referencedPause.id,
            idempotencyKey: "habit-resume:\(resumeOperationID.uuidString.lowercased())",
            command: .init(
                operationID: resumeOperationID,
                expectedRevision: referencedPause.revision,
                endedAt: Self.now
            ),
            createdAt: Self.now,
            conflictDetected: true
        ))
        let retainedPauses = try HabitSyncStore.retainedPauses(
            [referencedPause, newerPause],
            pendingMutations: [reviewedResume],
            limit: 1
        )
        #expect(retainedPauses == [referencedPause])
    }

    @Test("closed pauses protecting retained schedule authority survive pause retention")
    func retentionProtectsClosedMissedLifecyclePauses() throws {
        let occurrence = Self.occurrence()
        let protectedPause = Self.pause(
            id: UUID(),
            startedAt: Self.now.addingTimeInterval(-3_600),
            endedAt: Self.now.addingTimeInterval(1_800)
        )
        let firstNewer = Self.pause(
            id: UUID(),
            habitID: UUID(),
            startedAt: Self.now.addingTimeInterval(10 * 86_400),
            endedAt: Self.now.addingTimeInterval(10 * 86_400 + 3_600)
        )
        let secondNewer = Self.pause(
            id: UUID(),
            habitID: UUID(),
            startedAt: Self.now.addingTimeInterval(20 * 86_400),
            endedAt: Self.now.addingTimeInterval(20 * 86_400 + 3_600)
        )

        let retained = try HabitSyncStore.retainedPauses(
            [protectedPause, firstNewer, secondNewer],
            pendingMutations: [],
            protectedOccurrences: [occurrence],
            limit: 2
        )

        #expect(retained.contains(where: { $0.id == protectedPause.id }))
        #expect(retained.contains(where: { $0.id == secondNewer.id }))
    }

    @Test("overlapping pauses fail before retention can hide the conflict")
    func retentionRejectsOverlappingPausesBeforePruning() {
        let first = Self.pause(
            id: UUID(),
            startedAt: Self.now.addingTimeInterval(-7_200),
            endedAt: Self.now.addingTimeInterval(-1_800)
        )
        let overlapping = Self.pause(
            id: UUID(),
            startedAt: Self.now.addingTimeInterval(-3_600),
            endedAt: Self.now
        )
        let unrelated = Self.pause(
            id: UUID(),
            habitID: UUID(),
            startedAt: Self.now.addingTimeInterval(3_600),
            endedAt: Self.now.addingTimeInterval(7_200)
        )

        #expect(throws: (any Error).self) {
            try HabitSyncStore.retainedPauses(
                [first, overlapping, unrelated],
                pendingMutations: [],
                limit: 2
            )
        }
    }

    @Test("retention evicts terminal missed history while preserving active missed effects")
    func retentionBoundsMissedHistory() throws {
        func resolving(
            _ occurrence: DayWeaveHabitOccurrence,
            policy: DayWeaveHabitMissedPolicy,
            revision: UInt64,
            action: DayWeaveHabitMissedResolutionAction
        ) -> DayWeaveHabitOccurrence {
            .init(
                evidence: occurrence.evidence,
                outcome: occurrence.outcome,
                missedResolution: .init(
                    occurrenceEvidenceID: occurrence.id,
                    habitID: occurrence.evidence.habitID,
                    sourcePlannerOccurrenceID: occurrence.evidence.plannerOccurrenceID,
                    revision: revision,
                    configuredPolicy: policy,
                    action: action,
                    createdAt: occurrence.evidence.windowEnd,
                    updatedAt: max(occurrence.evidence.windowEnd, Self.now)
                )
            )
        }

        let terminalHistory = (10...19).map { age in
            let occurrence = Self.occurrence(
                plannerID: UUID(),
                ledgerID: UUID(),
                nominalStart: Self.now.addingTimeInterval(-Double(age) * 86_400)
            )
            return resolving(occurrence, policy: .skip, revision: 1, action: .skip)
        }
        let carriedBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-9 * 86_400)
        )
        let carried = resolving(
            carriedBase,
            policy: .carry,
            revision: 1,
            action: .carry(
                windowStart: Self.now.addingTimeInterval(3_600),
                windowEnd: Self.now.addingTimeInterval(7_200)
            )
        )
        let promptBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-8 * 86_400)
        )
        let prompt = resolving(
            promptBase,
            policy: .ask,
            revision: 1,
            action: .decisionRequired
        )
        let reductionTarget = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(86_400)
        )
        let reductionBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-7 * 86_400)
        )
        let reduction = resolving(
            reductionBase,
            policy: .reduceFrequency,
            revision: 2,
            action: .reduceFrequency(
                suppressedPlannerOccurrenceIDs: [reductionTarget.evidence.plannerOccurrenceID]
            )
        )

        let retained = try HabitSyncStore.retainedOccurrences(
            terminalHistory + [carried, prompt, reduction, reductionTarget],
            pendingMutations: [],
            referenceDate: Self.now,
            limit: 4
        )
        #expect(Set(retained.map(\.id)) == [
            carried.id,
            prompt.id,
            reduction.id,
            reductionTarget.id,
        ])

        let movedSkip = terminalHistory.last!
        let moveProtected = try HabitSyncStore.retainedOccurrences(
            terminalHistory + [carried, prompt, reduction, reductionTarget],
            pendingMutations: [],
            protectedPlannerOccurrenceIDs: [movedSkip.evidence.plannerOccurrenceID],
            referenceDate: Self.now,
            limit: 5
        )
        #expect(moveProtected.contains(where: { $0.id == movedSkip.id }))
        #expect(Set(moveProtected.map(\.id)).isSuperset(of: [
            carried.id,
            prompt.id,
            reduction.id,
            reductionTarget.id,
        ]))
        #expect(throws: (any Error).self) {
            try HabitSyncStore.retainedOccurrences(
                [terminalHistory[0], terminalHistory[1]],
                pendingMutations: [],
                protectedPlannerOccurrenceIDs: [
                    terminalHistory[0].evidence.plannerOccurrenceID,
                    terminalHistory[1].evidence.plannerOccurrenceID,
                ],
                referenceDate: Self.now,
                limit: 1
            )
        }
    }

    @Test("retention preserves the full upstream reduction dependency chain")
    func retentionPreservesTransitiveReductionDependencies() throws {
        func reducing(
            _ occurrence: DayWeaveHabitOccurrence,
            target: DayWeaveHabitOccurrence
        ) -> DayWeaveHabitOccurrence {
            .init(
                evidence: occurrence.evidence,
                outcome: occurrence.outcome,
                missedResolution: .init(
                    occurrenceEvidenceID: occurrence.id,
                    habitID: occurrence.evidence.habitID,
                    sourcePlannerOccurrenceID: occurrence.evidence.plannerOccurrenceID,
                    revision: 1,
                    configuredPolicy: .reduceFrequency,
                    action: .reduceFrequency(
                        suppressedPlannerOccurrenceIDs: [
                            target.evidence.plannerOccurrenceID,
                        ]
                    ),
                    createdAt: occurrence.evidence.windowEnd,
                    updatedAt: occurrence.evidence.windowEnd
                )
            )
        }

        let target = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(86_400)
        )
        let middleBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-10 * 86_400)
        )
        let middle = reducing(middleBase, target: target)
        let upstreamBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-20 * 86_400)
        )
        let upstream = reducing(upstreamBase, target: middle)
        let newerHistory = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-2 * 86_400)
        )

        let retained = try HabitSyncStore.retainedOccurrences(
            [upstream, middle, newerHistory, target],
            pendingMutations: [],
            referenceDate: Self.now,
            limit: 3
        )

        #expect(Set(retained.map(\.id)) == [upstream.id, middle.id, target.id])
        #expect(throws: (any Error).self) {
            try HabitSyncStore.retainedOccurrences(
                [upstream, middle, newerHistory, target],
                pendingMutations: [],
                referenceDate: Self.now,
                limit: 2
            )
        }
    }

    @Test("retention keeps recent carry destinations and durable reduction sources")
    func retentionKeepsMissedSchedulingBridges() throws {
        let carryBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-20 * 86_400)
        )
        let carryStart = Self.now.addingTimeInterval(-12 * 3_600)
        let carried = DayWeaveHabitOccurrence(
            evidence: carryBase.evidence,
            outcome: nil,
            missedResolution: .init(
                occurrenceEvidenceID: carryBase.id,
                habitID: carryBase.evidence.habitID,
                sourcePlannerOccurrenceID: carryBase.evidence.plannerOccurrenceID,
                revision: 1,
                configuredPolicy: .carry,
                action: .carry(
                    windowStart: carryStart,
                    windowEnd: Self.now.addingTimeInterval(-3_600)
                ),
                createdAt: carryBase.evidence.windowEnd,
                updatedAt: carryStart
            )
        )
        let missingTargetBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-30 * 86_400)
        )
        let missingTargetSource = DayWeaveHabitOccurrence(
            evidence: missingTargetBase.evidence,
            outcome: nil,
            missedResolution: .init(
                occurrenceEvidenceID: missingTargetBase.id,
                habitID: missingTargetBase.evidence.habitID,
                sourcePlannerOccurrenceID: missingTargetBase.evidence.plannerOccurrenceID,
                revision: 1,
                configuredPolicy: .reduceFrequency,
                action: .reduceFrequency(
                    suppressedPlannerOccurrenceIDs: [Self.versionFiveUUID(UUID())]
                ),
                createdAt: missingTargetBase.evidence.windowEnd,
                updatedAt: Self.now.addingTimeInterval(-3_600)
            )
        )
        let ordinary = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-2 * 86_400)
        )

        let retained = try HabitSyncStore.retainedOccurrences(
            [missingTargetSource, carried, ordinary],
            pendingMutations: [],
            referenceDate: Self.now,
            limit: 2
        )

        #expect(Set(retained.map(\.id)) == [missingTargetSource.id, carried.id])

        let expiredBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-500 * 86_400)
        )
        let expiredBridge = DayWeaveHabitOccurrence(
            evidence: expiredBase.evidence,
            outcome: nil,
            missedResolution: .init(
                occurrenceEvidenceID: expiredBase.id,
                habitID: expiredBase.evidence.habitID,
                sourcePlannerOccurrenceID: expiredBase.evidence.plannerOccurrenceID,
                revision: 1,
                configuredPolicy: .reduceFrequency,
                action: .reduceFrequency(
                    suppressedPlannerOccurrenceIDs: [Self.versionFiveUUID(UUID())]
                ),
                createdAt: expiredBase.evidence.windowEnd,
                updatedAt: Self.now.addingTimeInterval(-367 * 86_400)
            )
        )
        let recentHistory = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-3 * 86_400)
        )
        let historicalResult = try HabitSyncStore.retainedOccurrences(
            [expiredBridge, recentHistory, ordinary],
            pendingMutations: [],
            referenceDate: Self.now,
            limit: 2
        )
        #expect(historicalResult.contains(where: { $0.id == expiredBridge.id }))
    }

    @Test("retention rejects duplicate planner identities without trapping")
    func retentionRejectsDuplicatePlannerIdentity() {
        let plannerID = UUID()
        let first = Self.occurrence(plannerID: plannerID, ledgerID: UUID())
        let second = Self.occurrence(plannerID: plannerID, ledgerID: UUID())

        #expect(throws: (any Error).self) {
            try HabitSyncStore.retainedOccurrences(
                [first, second],
                pendingMutations: [],
                referenceDate: Self.now,
                limit: 1
            )
        }
    }

    @Test("the 20k cache ceiling cannot evict a moved occurrence's missed skip")
    func retentionProtectsMovedMissedSkipAtProductionLimit() throws {
        let skippedBase = Self.occurrence(
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-30_000 * 86_400)
        )
        let skipped = DayWeaveHabitOccurrence(
            evidence: skippedBase.evidence,
            outcome: nil,
            missedResolution: .init(
                occurrenceEvidenceID: skippedBase.id,
                habitID: skippedBase.evidence.habitID,
                sourcePlannerOccurrenceID: skippedBase.evidence.plannerOccurrenceID,
                revision: 1,
                configuredPolicy: .skip,
                action: .skip,
                createdAt: skippedBase.evidence.windowEnd,
                updatedAt: Self.now
            )
        )
        let history = (0..<DayWeaveHabitClientSnapshot.maximumOccurrences).map { offset in
            Self.occurrence(
                plannerID: UUID(),
                ledgerID: UUID(),
                nominalStart: Self.now.addingTimeInterval(
                    -Double(DayWeaveHabitClientSnapshot.maximumOccurrences - offset + 10) * 86_400
                )
            )
        }

        let retained = try HabitSyncStore.retainedOccurrences(
            [skipped] + history,
            pendingMutations: [],
            protectedPlannerOccurrenceIDs: [skipped.evidence.plannerOccurrenceID],
            referenceDate: Self.now
        )

        #expect(retained.count == DayWeaveHabitClientSnapshot.maximumOccurrences)
        #expect(retained.contains(where: { $0.id == skipped.id }))
    }

    @Test("retention reserves current rows and every correction-safe completion anchor")
    func retentionCannotSilentlyCrowdAuthoritativeRows() throws {
        let firstHabit = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaa1")!
        let thirdHabit = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaa3")!
        let firstCompletion = Self.occurrence(
            habitID: firstHabit,
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-30 * 86_400),
            completed: true
        )
        let secondCompletion = Self.occurrence(
            habitID: firstHabit,
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-20 * 86_400),
            completed: true
        )
        let newerOrdinaryHistory = Self.occurrence(
            note: "partial history",
            habitID: firstHabit,
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(-10 * 86_400)
        )
        let current = Self.occurrence(
            habitID: thirdHabit,
            plannerID: UUID(),
            ledgerID: UUID(),
            nominalStart: Self.now.addingTimeInterval(3_600)
        )

        let retained = try HabitSyncStore.retainedOccurrences(
            [firstCompletion, secondCompletion, current],
            pendingMutations: [],
            referenceDate: Self.now,
            limit: 3
        )
        #expect(Set(retained.map(\.id)) == [firstCompletion.id, secondCompletion.id, current.id])
        let correctionSafe = try HabitSyncStore.retainedOccurrences(
            [firstCompletion, secondCompletion, newerOrdinaryHistory],
            pendingMutations: [],
            referenceDate: Self.now,
            limit: 2
        )
        #expect(Set(correctionSafe.map(\.id)) == [firstCompletion.id, secondCompletion.id])
        #expect(throws: (any Error).self) {
            try HabitSyncStore.retainedOccurrences(
                [firstCompletion, secondCompletion, current],
                pendingMutations: [],
                referenceDate: Self.now,
                limit: 2
            )
        }
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
    nonisolated fileprivate static let plannerOccurrenceID = UUID(uuidString: "cccccccc-3333-5333-8333-cccccccccccc")!

    nonisolated fileprivate static func occurrence(
        note: String? = nil,
        habitID occurrenceHabitID: UUID = habitID,
        plannerID: UUID = plannerOccurrenceID,
        ledgerID: UUID = ledgerOccurrenceID,
        nominalStart: Date = date("2026-09-04T12:00:00.000000Z"),
        sourceItemRevision: UInt64 = 3,
        completed: Bool = false,
        missedResolution: DayWeaveHabitMissedResolution? = nil
    ) -> DayWeaveHabitOccurrence {
        let outcome: DayWeaveHabitOutcome?
        if completed {
            outcome = .init(
                revision: 1,
                status: .completed,
                progressBasisPoints: 10_000,
                quantity: 20,
                unit: "pages",
                actualSeconds: 3_600,
                note: note,
                occurredAt: nominalStart,
                updatedAt: nominalStart
            )
        } else {
            outcome = note.map {
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
        }
        return .init(
            evidence: .init(
                id: ledgerID,
                habitID: occurrenceHabitID,
                plannerOccurrenceID: versionFiveUUID(plannerID),
                sourceScheduleRevisionID: UUID(uuidString: "dddddddd-4444-4444-8444-dddddddddddd")!,
                sourceItemRevision: sourceItemRevision,
                policyFingerprint: "sha256:\(String(repeating: "a", count: 64))",
                identity: .object([
                    "type": .string("rolling_minutes"),
                    "index": .number(JSONNumber(UInt64(0))),
                    "anchor": .string("2026-09-04T12:00:00Z"),
                ]),
                nominalStart: nominalStart,
                nominalEnd: nominalStart.addingTimeInterval(3_600),
                windowStart: nominalStart.addingTimeInterval(-3_600),
                windowEnd: nominalStart.addingTimeInterval(7_200),
                localDate: DayWeaveLocalDate.containing(
                    nominalStart,
                    timezoneName: "Europe/Paris"
                )!,
                timezoneName: "Europe/Paris",
                expectedDurationSeconds: 3_600,
                expectedQuantity: 20,
                expectedUnit: "pages"
            ),
            outcome: outcome,
            missedResolution: missedResolution
        )
    }

    nonisolated fileprivate static func missedResolution(
        action: DayWeaveHabitMissedResolutionAction = .decisionRequired,
        revision: UInt64 = 1,
        updatedAt: Date? = nil,
        policy: DayWeaveHabitMissedPolicy = .ask
    ) -> DayWeaveHabitMissedResolution {
        let createdAt = now.addingTimeInterval(-3_600)
        return .init(
            occurrenceEvidenceID: ledgerOccurrenceID,
            habitID: habitID,
            sourcePlannerOccurrenceID: plannerOccurrenceID,
            revision: revision,
            configuredPolicy: policy,
            action: action,
            createdAt: createdAt,
            updatedAt: updatedAt ?? createdAt
        )
    }

    nonisolated private static func versionFiveUUID(_ value: UUID) -> UUID {
        var bytes = value.uuid
        bytes.6 = (bytes.6 & 0x0f) | 0x50
        bytes.8 = (bytes.8 & 0x3f) | 0x80
        return UUID(uuid: bytes)
    }

    nonisolated fileprivate static func pause(
        id: UUID,
        habitID: UUID = habitID,
        startedAt: Date,
        endedAt: Date? = nil,
        revision: UInt64 = 1
    ) -> DayWeaveHabitPause {
        .init(
            id: id,
            habitID: habitID,
            revision: revision,
            startedAt: startedAt,
            endedAt: endedAt,
            preservesStreak: true,
            createdAt: startedAt,
            updatedAt: endedAt ?? startedAt
        )
    }

    private static func analytics(habitID: UUID = habitID) -> DayWeaveHabitAnalytics {
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
            deltaCaughtUp: true,
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
    enum OutcomeMode: Equatable, Sendable {
        case success
        case replayed
        case offline
        case conflict
        case mismatched
        case advancedMissed
        case divergentMissed
        case unreachableMissed
    }
    enum MissedReconcileMode: Equatable, Sendable { case success, replayed, offline }
    enum MissedResolutionMode: Equatable, Sendable {
        case success
        case replayed
        case offline
        case conflict
        case cancelled
    }
    enum PauseResponseMode: Equatable, Sendable {
        case success
        case mismatchedStart
        case mismatchedResume
    }

    struct OutcomeRequest: Sendable {
        let occurrenceID: UUID
        let command: DayWeaveHabitOutcomeCommand
        let idempotencyKey: String
    }

    struct MissedReconcileRequest: Sendable {
        let command: DayWeaveHabitMissedReconcileCommand
        let limit: Int
        let idempotencyKey: String
    }

    struct MissedResolutionRequest: Sendable {
        let occurrenceID: UUID
        let command: DayWeaveHabitMissedResolveCommand
        let idempotencyKey: String
    }

    final class State: @unchecked Sendable {
        private let lock = NSLock()
        var pages: [DayWeaveHabitDeltaPage]
        var requests: [OutcomeRequest] = []
        var missedReconcileRequests: [MissedReconcileRequest] = []
        var missedResolutionRequests: [MissedResolutionRequest] = []
        var cursors: [String?] = []
        var analyticIDs: [UUID] = []

        init(pages: [DayWeaveHabitDeltaPage]) { self.pages = pages }

        func nextDelta(
            cursor: String?,
            failureAtCall: Int?
        ) throws -> DayWeaveHabitDeltaPage {
            try lock.withLock {
                cursors.append(cursor)
                if failureAtCall == cursors.count - 1 {
                    throw DayWeaveAPIError.transport(.networkConnectionLost)
                }
                if !pages.isEmpty { return pages.removeFirst() }
                return .init(changes: [], nextCursor: cursor ?? "cursor-empty", hasMore: false)
            }
        }

        func add(_ request: OutcomeRequest) { lock.withLock { requests.append(request) } }
        func add(_ request: MissedReconcileRequest) {
            lock.withLock { missedReconcileRequests.append(request) }
        }
        func add(_ request: MissedResolutionRequest) {
            lock.withLock { missedResolutionRequests.append(request) }
        }
        func addAnalytics(_ id: UUID) { lock.withLock { analyticIDs.append(id) } }
        func outcomeRequests() -> [OutcomeRequest] { lock.withLock { requests } }
        func reconcileRequests() -> [MissedReconcileRequest] {
            lock.withLock { missedReconcileRequests }
        }
        func resolutionRequests() -> [MissedResolutionRequest] {
            lock.withLock { missedResolutionRequests }
        }
        func deltaCursors() -> [String?] { lock.withLock { cursors } }
        func analyticsIDs() -> [UUID] { lock.withLock { analyticIDs } }
    }

    nonisolated let configurationIdentifier: String
    private let state: State
    private let outcomeMode: OutcomeMode
    private let missedReconcileMode: MissedReconcileMode
    private let missedResolutionMode: MissedResolutionMode
    private let beforeOutcome: @Sendable () -> Void
    private let beforeMissedReconcile: @Sendable () -> Void
    private let beforeMissedResolution: @Sendable () -> Void
    private let analyticsValue: DayWeaveHabitAnalytics?
    private let pauseOffline: Bool
    private let pauseResponseMode: PauseResponseMode
    private let deltaFailureAtCall: Int?
    private let beforeDelta: @Sendable (String?) async -> Void

    init(
        configurationIdentifier: String,
        deltaPages: [DayWeaveHabitDeltaPage] = [],
        outcomeMode: OutcomeMode = .success,
        missedReconcileMode: MissedReconcileMode = .success,
        missedResolutionMode: MissedResolutionMode = .success,
        analytics: DayWeaveHabitAnalytics? = nil,
        pauseOffline: Bool = false,
        pauseResponseMode: PauseResponseMode = .success,
        deltaFailureAtCall: Int? = nil,
        beforeDelta: @escaping @Sendable (String?) async -> Void = { _ in },
        beforeOutcome: @escaping @Sendable () -> Void = {},
        beforeMissedReconcile: @escaping @Sendable () -> Void = {},
        beforeMissedResolution: @escaping @Sendable () -> Void = {}
    ) {
        self.configurationIdentifier = configurationIdentifier
        state = .init(pages: deltaPages)
        self.outcomeMode = outcomeMode
        self.missedReconcileMode = missedReconcileMode
        self.missedResolutionMode = missedResolutionMode
        analyticsValue = analytics
        self.pauseOffline = pauseOffline
        self.pauseResponseMode = pauseResponseMode
        self.deltaFailureAtCall = deltaFailureAtCall
        self.beforeDelta = beforeDelta
        self.beforeOutcome = beforeOutcome
        self.beforeMissedReconcile = beforeMissedReconcile
        self.beforeMissedResolution = beforeMissedResolution
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
        case .success, .replayed, .mismatched, .advancedMissed, .divergentMissed,
             .unreachableMissed:
            let base = HabitSyncStoreTests.occurrence()
            let receivedInput = outcomeMode == .mismatched
                ? DayWeaveHabitOutcomeInput.skipped(occurredAt: command.outcome.occurredAt)
                : command.outcome
            let outcome = DayWeaveHabitOutcome(
                revision: command.expectedRevision + 1,
                status: receivedInput.status,
                progressBasisPoints: receivedInput.progressBasisPoints,
                quantity: receivedInput.quantity,
                unit: receivedInput.unit,
                actualSeconds: receivedInput.actualSeconds,
                note: receivedInput.note,
                occurredAt: receivedInput.occurredAt,
                updatedAt: HabitSyncStoreTests.now
            )
            let missedResolution: DayWeaveHabitMissedResolution? = switch outcomeMode {
            case .advancedMissed:
                HabitSyncStoreTests.missedResolution(
                    action: .carry(
                        windowStart: HabitSyncStoreTests.now,
                        windowEnd: HabitSyncStoreTests.now.addingTimeInterval(86_400)
                    ),
                    revision: 2,
                    updatedAt: HabitSyncStoreTests.now
                )
            case .divergentMissed:
                HabitSyncStoreTests.missedResolution(action: .skip)
            case .unreachableMissed:
                HabitSyncStoreTests.missedResolution(
                    action: .decisionRequired,
                    revision: 2,
                    updatedAt: HabitSyncStoreTests.now
                )
            default:
                nil
            }
            return .init(
                occurrence: .init(
                    evidence: base.evidence,
                    outcome: outcome,
                    missedResolution: missedResolution
                ),
                replayed: outcomeMode == .replayed
            )
        }
    }

    func reconcileMissedHabitOccurrences(
        command: DayWeaveHabitMissedReconcileCommand,
        limit: Int,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitMissedReconcileResponse {
        state.add(.init(command: command, limit: limit, idempotencyKey: idempotencyKey))
        beforeMissedReconcile()
        if missedReconcileMode == .offline {
            throw DayWeaveAPIError.transport(.networkConnectionLost)
        }
        return .init(
            resolutions: [],
            hasMore: false,
            replayed: missedReconcileMode == .replayed
        )
    }

    func resolveMissedHabitOccurrence(
        habitID: UUID,
        occurrenceID: UUID,
        command: DayWeaveHabitMissedResolveCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitMissedResolutionMutationResponse {
        state.add(.init(
            occurrenceID: occurrenceID,
            command: command,
            idempotencyKey: idempotencyKey
        ))
        beforeMissedResolution()
        switch missedResolutionMode {
        case .offline:
            throw DayWeaveAPIError.transport(.networkConnectionLost)
        case .conflict:
            throw DayWeaveAPIError.server(
                statusCode: 409,
                code: "conflict",
                message: "missed resolution changed",
                requestID: nil
            )
        case .success, .replayed, .cancelled:
            let action: DayWeaveHabitMissedResolutionAction
            if missedResolutionMode == .cancelled {
                action = .cancelled(
                    reason: .sourceCompleted,
                    resumeAction: Self.resumeAction(for: command.action)
                )
            } else {
                switch command.action {
                case .skip:
                    action = .skip
                case .carry:
                    action = .carry(
                        windowStart: HabitSyncStoreTests.now,
                        windowEnd: HabitSyncStoreTests.now.addingTimeInterval(86_400)
                    )
                case .reduceFrequency:
                    action = .reductionPending
                }
            }
            return .init(
                resolution: HabitSyncStoreTests.missedResolution(
                    action: action,
                    revision: command.expectedRevision + 1,
                    updatedAt: HabitSyncStoreTests.now
                ),
                replayed: missedResolutionMode == .replayed
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
                revision: pauseResponseMode == .mismatchedStart ? 2 : 1,
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
        let endedAt = pauseResponseMode == .mismatchedResume
            ? command.endedAt.addingTimeInterval(60)
            : command.endedAt
        return .init(
            pause: .init(
                id: pauseID,
                habitID: habitID,
                revision: command.expectedRevision + 1,
                startedAt: HabitSyncStoreTests.now.addingTimeInterval(-3_600),
                endedAt: endedAt,
                preservesStreak: true,
                createdAt: HabitSyncStoreTests.now.addingTimeInterval(-3_600),
                updatedAt: endedAt
            ),
            replayed: false
        )
    }

    func habitDelta(cursor: String?, limit: Int) async throws -> DayWeaveHabitDeltaPage {
        await beforeDelta(cursor)
        return try state.nextDelta(cursor: cursor, failureAtCall: deltaFailureAtCall)
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
    func missedReconcileRequests() async -> [MissedReconcileRequest] {
        state.reconcileRequests()
    }
    func missedResolutionRequests() async -> [MissedResolutionRequest] {
        state.resolutionRequests()
    }
    func deltaCursors() async -> [String?] { state.deltaCursors() }
    func analyticsHabitIDs() async -> [UUID] { state.analyticsIDs() }

    private static func resumeAction(
        for action: DayWeaveHabitMissedExplicitAction
    ) -> DayWeaveHabitMissedResumeAction {
        switch action {
        case .skip: .skip
        case .carry: .carry
        case .reduceFrequency: .reduceFrequency
        }
    }
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

private actor HabitDeltaResponseGate {
    private var entered = false
    private var released = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        entered = true
        let waiters = entryWaiters
        entryWaiters.removeAll()
        waiters.forEach { $0.resume() }
        guard !released else { return }
        await withCheckedContinuation { continuation in
            releaseWaiters.append(continuation)
        }
    }

    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { continuation in
            entryWaiters.append(continuation)
        }
    }

    func release() {
        released = true
        let waiters = releaseWaiters
        releaseWaiters.removeAll()
        waiters.forEach { $0.resume() }
    }
}
#endif
