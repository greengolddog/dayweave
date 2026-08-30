import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Durable authoritative execution synchronization", .serialized)
@MainActor
struct ExecutionSyncStoreTests {
    nonisolated private static let itemID = UUID(
        uuidString: "20000000-0000-4000-8000-000000000002"
    )!
    nonisolated private static let blockID = UUID(
        uuidString: "30000000-0000-4000-8000-000000000003"
    )!
    nonisolated private static let deviceID = UUID(
        uuidString: "40000000-0000-4000-8000-000000000004"
    )!
    nonisolated private static let occurrenceID = UUID(
        uuidString: "50000000-0000-4000-8000-000000000005"
    )!
    nonisolated private static let binding = "execution-test-binding-a"
    nonisolated private static let canonicalConfiguration = "https://api.example.test"
    nonisolated private static let baseDate = Date(timeIntervalSince1970: 1_800_000_000)

    @Test("production execution connection preserves the auth-bound canonical identity")
    func productionConnectionUsesAuthBoundConfiguration() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let token = "synthetic-execution-configuration-token"
        let baseURL = try DayWeaveAPIBaseURL("https://api.example.com/gateway")
        let expectedConfiguration = DayWeaveAPIClient(
            baseURL: baseURL,
            bearerToken: token
        ).configurationIdentifier
        let planner = PlannerStore(
            canonicalConfigurationIdentifier: expectedConfiguration,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.baseDate }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":0,"active_session":null}}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"sessions":[],"next_offset":null}"#.utf8)
            ),
            .init(
                statusCode: 200,
                body: Data(#"{"execution":{"revision":0,"active_session":null}}"#.utf8)
            )
        )
        let sync = ExecutionSyncStore(
            planner: planner,
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            tokenStore: TestBearerTokenStore(
                token: token,
                origin: baseURL.credentialOriginIdentifier
            ),
            session: URLProtocolStub.makeSession(),
            now: { Self.baseDate }
        )

        #expect(await sync.refresh() == .success)
        #expect(planner.canonicalConfigurationIdentifier == expectedConfiguration)
        #expect(URLProtocolStub.storage.requests(for: token).count == 3)
    }

    @Test("fresh clients page through more than 100 rows and prove the complete revision sum")
    func fullHistoryBootstrapExceedsOnePage() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessions = try (0..<101).map { offset in
            try Self.terminalSession(
                sessionID: Self.uuid(offset + 1),
                sessionIndex: UInt16(offset),
                startedAt: Self.baseDate.addingTimeInterval(TimeInterval(1_000 - offset * 10))
            )
        }.sorted(by: Self.newestFirst)
        let snapshot = DayWeaveExecutionSnapshot(revision: 202, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [
                .init(sessions: Array(sessions.prefix(100)), nextOffset: 100),
                .init(sessions: Array(sessions.dropFirst(100)), nextOffset: nil),
            ]
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(planner: planner, transport: transport)

        #expect(await sync.refresh() == .success)
        #expect(planner.executionState.historyVerified)
        #expect(planner.executionState.revision == 202)
        #expect(planner.executionState.terminalOutcomes.count == 101)
        #expect(await transport.requestedOffsets() == [0, 100])
    }

    @Test("snapshot bracketing retries a generation that changes during pagination")
    func unstableGenerationIsRetried() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let older = try Self.terminalSession(
            sessionID: Self.uuid(1),
            sessionIndex: 0,
            startedAt: Self.baseDate
        )
        let newer = try Self.terminalSession(
            sessionID: Self.uuid(2),
            sessionIndex: 1,
            startedAt: Self.baseDate.addingTimeInterval(100)
        )
        let revision2 = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let revision4 = DayWeaveExecutionSnapshot(revision: 4, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [revision2, revision4, revision4],
            pages: [
                .init(sessions: [older], nextOffset: nil),
                .init(sessions: [newer, older], nextOffset: nil),
            ]
        )
        let planner = Self.planner(persistence: context.persistence)

        #expect(await Self.controller(planner: planner, transport: transport).refresh() == .success)
        #expect(planner.executionState.revision == 4)
        #expect(planner.executionState.terminalOutcomes.count == 2)
    }

    @Test("a process death replays the identical request bytes and idempotency key")
    func processDeathReplaysExactCommand() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let firstTransport = ExecutionTransportDouble(
            snapshots: [.init(revision: 0, activeSession: nil), .init(revision: 0, activeSession: nil)],
            pages: [.init(sessions: [], nextOffset: nil)],
            commandReplies: [.failure(.transport(.networkConnectionLost))]
        )
        let firstPlanner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()]
        )
        let first = Self.controller(planner: firstPlanner, transport: firstTransport)

        #expect(await first.start(Self.blockID) == .transientNetworkFailure)
        let staged = try #require(firstPlanner.executionState.pendingCommand)
        let firstRequest = try #require((await firstTransport.receivedCommands()).first)
        #expect(firstRequest.body == staged.encodedRequest)
        #expect(firstRequest.key == staged.idempotencyKey)

        let active = try Self.activeSession(sessionID: staged.identity.sessionID)
        let mutation = try Self.mutation(revision: 1, active: active, changed: active, replayed: true)
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let secondTransport = ExecutionTransportDouble(
            snapshots: [activeSnapshot, activeSnapshot],
            pages: [.init(sessions: [active], nextOffset: nil)],
            commandReplies: [.mutation(mutation)]
        )
        let restored = PlannerStore.live(persistence: context.persistence)
        let second = Self.controller(planner: restored, transport: secondTransport)

        #expect(await second.refresh() == .success)
        let replay = try #require((await secondTransport.receivedCommands()).first)
        #expect(replay.body == firstRequest.body)
        #expect(replay.key == firstRequest.key)
        #expect(restored.executionState.pendingCommand == nil)
        #expect(restored.executionState.activeSession == active)
        #expect(restored.executionState.historyVerified)

        restored.flushPersistence()
        let relaunched = PlannerStore.live(persistence: context.persistence)
        #expect(relaunched.persistenceError == nil)
        #expect(relaunched.executionState.activeSession == active)
        #expect(relaunched.executionState.historyVerified)
    }

    @Test("credential rotation cannot rebind a durable pending command")
    func credentialRotationLeavesPendingFenceIntact() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let request = DayWeaveExecutionCommandRequest(
            expectedRevision: 0,
            command: .start(
                sessionID: Self.uuid(90),
                itemID: Self.itemID,
                itemRevision: 1,
                occurrenceID: nil,
                sessionIndex: 0,
                plannedBlockID: Self.blockID,
                deviceID: Self.deviceID
            )
        )
        let command = DayWeavePendingExecutionCommand(
            idempotencyKey: "mac-execution-rotation-test",
            bindingIdentifier: Self.binding,
            expectedRevision: 0,
            identity: .init(
                sessionID: Self.uuid(90),
                itemID: Self.itemID,
                itemRevision: 1,
                occurrenceID: nil,
                sessionIndex: 0,
                plannedBlockID: Self.blockID,
                sourceDeviceID: Self.deviceID
            ),
            command: request.command,
            encodedRequest: try DayWeaveExecutionWireCodec.encode(request),
            priorSession: nil,
            focusedBlockID: Self.blockID,
            canonicalProjectionEligibleAtLeaseStart: true,
            stagedAt: Self.baseDate
        )
        var durable = Self.emptyBoundState
        durable.pendingCommand = command
        let planner = Self.planner(persistence: context.persistence, executionState: durable)
        planner.flushPersistence()
        let transport = ExecutionTransportDouble(snapshots: [], pages: [])
        let rotated = Self.controller(
            planner: planner,
            transport: transport,
            binding: "execution-test-binding-b"
        )

        #expect(await rotated.refresh() == .configurationChanged)
        #expect(planner.executionState.pendingCommand == command)
        #expect(planner.executionState.bindingIdentifier == Self.binding)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("422 clears a start fence only after complete history proves the session absent")
    func rejectedStartRequiresStableAbsenceProof() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let command = try Self.pendingStart()
        var durable = Self.emptyBoundState
        durable.pendingCommand = command
        let planner = Self.planner(persistence: context.persistence, executionState: durable)
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)],
            commandReplies: [.failure(.server(
                statusCode: 422,
                code: "invalid_execution",
                message: "expired absolute pause",
                requestID: nil
            ))]
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh()
            == .validationFailure)
        #expect(planner.executionState.pendingCommand == nil)
        #expect(planner.executionState.historyVerified)
    }

    @Test("a history-proven applied start preserves its original projection eligibility")
    func rejectedAppliedStartPreservesProjectionEligibility() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let command = try Self.pendingStart()
        let active = try Self.activeSession(sessionID: command.identity.sessionID)
        let snapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        var durable = Self.emptyBoundState
        durable.pendingCommand = command
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [active], nextOffset: nil)],
            commandReplies: [.failure(.server(
                statusCode: 409,
                code: "idempotency_expired",
                message: "the original start is already authoritative",
                requestID: nil
            ))]
        )

        #expect(await Self.controller(planner: planner, transport: transport).start(Self.blockID)
            == .success)
        #expect(planner.executionState.pendingCommand == nil)
        #expect(planner.executionState.activeSession == active)
        #expect(planner.executionState.leaseProjectionEligibility[active.id] == true)
    }

    @Test("a history revision gap fails closed and leaves starts unverified")
    func revisionGapFailsClosed() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(1),
            sessionIndex: 0,
            startedAt: Self.baseDate
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 4, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [terminal], nextOffset: nil)]
        )
        let planner = Self.planner(persistence: context.persistence)

        #expect(await Self.controller(planner: planner, transport: transport).refresh()
            == .protocolFailure)
        #expect(!planner.executionState.historyVerified)
        #expect(planner.executionState.revision == 0)
    }

    @Test("a forged mutation response cannot release the exact pending bytes")
    func forgedMutationRetainsFence() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let command = try Self.pendingStart()
        var durable = Self.emptyBoundState
        durable.pendingCommand = command
        let wrong = try Self.activeSession(sessionID: Self.uuid(999))
        let mutation = try Self.mutation(revision: 1, active: wrong, changed: wrong, replayed: false)
        let planner = Self.planner(persistence: context.persistence, executionState: durable)
        let transport = ExecutionTransportDouble(
            snapshots: [],
            pages: [],
            commandReplies: [.mutation(mutation)]
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh()
            == .protocolFailure)
        #expect(planner.executionState.pendingCommand == command)
    }

    @Test("a protocol-ahead public clock does not inflate or reject observed elapsed work")
    func protocolAheadMutationAcceptsServerObservedElapsedTime() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_001)
        let protocolStart = Self.baseDate.addingTimeInterval(3_600)
        let prior = try Self.session(
            id: sessionID,
            status: .active,
            revision: 1,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: protocolStart,
            updatedAt: protocolStart,
            accumulatedSeconds: 0,
            actualSeconds: nil,
            runningSince: protocolStart,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: nil
        )
        let changedAt = protocolStart.addingTimeInterval(1)
        let changed = try Self.session(
            id: sessionID,
            status: .completed,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: protocolStart,
            updatedAt: changedAt,
            accumulatedSeconds: 10,
            actualSeconds: 10,
            runningSince: nil,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: changedAt
        )
        let command = DayWeaveExecutionCommand.complete(
            sessionID: sessionID,
            actualSeconds: nil
        )
        let request = DayWeaveExecutionCommandRequest(
            expectedRevision: 1,
            command: command
        )
        let pending = DayWeavePendingExecutionCommand(
            idempotencyKey: "mac-execution-protocol-ahead-complete",
            bindingIdentifier: Self.binding,
            expectedRevision: 1,
            identity: .init(session: prior),
            command: command,
            encodedRequest: try DayWeaveExecutionWireCodec.encode(request),
            priorSession: prior,
            focusedBlockID: Self.blockID,
            canonicalProjectionEligibleAtLeaseStart: true,
            stagedAt: Self.baseDate
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = prior
        durable.historyWindow = [prior]
        durable.historyWindowRevision = 1
        durable.pendingCommand = pending
        let snapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [changed], nextOffset: nil)],
            commandReplies: [.mutation(try Self.mutation(
                revision: 2,
                active: nil,
                changed: changed,
                replayed: false
            ))]
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh() == .success)
        #expect(planner.executionState.pendingCommand == nil)
        #expect(planner.executionState.historyVerified)
        #expect(planner.executionState.terminalOutcomes[sessionID]?.session == changed)
        #expect(planner.pendingCanonicalMutations.first?.executionSessionID == sessionID)
    }

    @Test("a corrected terminal duration must match the exact command")
    func correctedTerminalDurationMismatchRetainsFence() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_002)
        let prior = try Self.activeSession(sessionID: sessionID)
        let changedAt = Self.baseDate.addingTimeInterval(1)
        let changed = try Self.session(
            id: sessionID,
            status: .completed,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: changedAt,
            accumulatedSeconds: 1,
            actualSeconds: 8,
            runningSince: nil,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: changedAt
        )
        let command = DayWeaveExecutionCommand.complete(
            sessionID: sessionID,
            actualSeconds: 7
        )
        let pending = DayWeavePendingExecutionCommand(
            idempotencyKey: "mac-execution-corrected-duration",
            bindingIdentifier: Self.binding,
            expectedRevision: 1,
            identity: .init(session: prior),
            command: command,
            encodedRequest: try DayWeaveExecutionWireCodec.encode(.init(
                expectedRevision: 1,
                command: command
            )),
            priorSession: prior,
            focusedBlockID: Self.blockID,
            canonicalProjectionEligibleAtLeaseStart: true,
            stagedAt: Self.baseDate
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = prior
        durable.historyWindow = [prior]
        durable.historyWindowRevision = 1
        durable.pendingCommand = pending
        let transport = ExecutionTransportDouble(
            snapshots: [],
            pages: [],
            commandReplies: [.mutation(try Self.mutation(
                revision: 2,
                active: nil,
                changed: changed,
                replayed: false
            ))]
        )
        let planner = Self.planner(
            persistence: context.persistence,
            executionState: durable
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh()
            == .protocolFailure)
        #expect(planner.executionState.pendingCommand == pending)
    }

    @Test("split-session completion changes only its exact block and never projects the parent")
    func splitSessionPresentationIsScoped() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let first = Self.block(id: Self.blockID, sessionIndex: 0)
        let second = Self.block(id: Self.uuid(301), sessionIndex: 1)
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(50),
            sessionIndex: 0,
            plannedBlockID: first.id,
            startedAt: Self.baseDate
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [terminal], nextOffset: nil)]
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [first, second],
            canonicalItems: [try Self.canonicalItem(splittable: true)]
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh() == .success)
        #expect(planner.blocks.first(where: { $0.id == first.id })?.status == .completed)
        #expect(planner.blocks.first(where: { $0.id == second.id })?.status == .scheduled)
        #expect(planner.pendingCanonicalMutations.isEmpty)
    }

    @Test("deferred history is retained without becoming completion or skip")
    func deferredHistoryIsTerminalButNonProjecting() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(55),
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [deferred], nextOffset: nil)]
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()]
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh() == .success)
        #expect(planner.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
        #expect(planner.pendingCanonicalMutations.isEmpty)
        #expect(planner.recurrenceSessionOutcomes.isEmpty)
        #expect(planner.executionState.terminalOutcomes[deferred.id]?.session == deferred)
        #expect(planner.executionState.terminalOutcomes[deferred.id]?.projection == .notRequired)

        planner.flushPersistence()
        let relaunched = PlannerStore.live(persistence: context.persistence)
        #expect(relaunched.persistenceError == nil)
        #expect(relaunched.executionState.terminalOutcomes[deferred.id]?.session == deferred)
        #expect(relaunched.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
    }

    @Test("a newer deferred session suppresses a projection but preserves its uncertainty journal")
    func newerDeferredSuppressesProjectionAndPreservesJournalAcrossRelaunch() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let older = try Self.terminalSession(
            sessionID: Self.uuid(56),
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate
        )
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(57),
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate.addingTimeInterval(100)
        )
        var state = Self.emptyBoundState
        state.revision = older.revision
        state.historyWindow = [older]
        state.historyWindowRevision = older.revision
        state.terminalOutcomes[older.id] = .init(
            session: older,
            recordedAt: older.updatedAt,
            projection: .pending
        )
        state.presentedBlockIDs = [Self.blockID]
        let linkedMutation = PendingCanonicalMutation(
            id: Self.uuid(58),
            itemID: Self.itemID,
            occurrenceID: nil,
            sessionIndex: 0,
            desiredStatus: .completed,
            baseRevision: older.itemRevision,
            createdAt: older.updatedAt,
            disposition: .pending,
            diagnostic: nil,
            executionSessionID: older.id
        )
        var completedBlock = Self.block()
        completedBlock.status = .completed
        completedBlock.actualMinutes = 1
        let seeded = Self.planner(
            persistence: context.persistence,
            blocks: [completedBlock],
            canonicalItems: [try Self.canonicalItem()],
            pendingCanonicalMutations: [linkedMutation],
            executionState: state
        )
        seeded.flushPersistence()

        let firstLaunch = PlannerStore.live(persistence: context.persistence)
        #expect(firstLaunch.pendingCanonicalMutations == [linkedMutation])
        let snapshot = DayWeaveExecutionSnapshot(revision: 4, activeSession: nil)
        let firstTransport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [deferred, older], nextOffset: nil)]
        )
        #expect(
            await Self.controller(planner: firstLaunch, transport: firstTransport).refresh()
                == .success
        )
        #expect(firstLaunch.pendingCanonicalMutations == [linkedMutation])
        #expect(firstLaunch.executionState.terminalOutcomes[older.id]?.projection == .notRequired)
        #expect(firstLaunch.executionState.terminalOutcomes[deferred.id]?.projection == .notRequired)
        #expect(firstLaunch.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
        firstLaunch.capturePendingCanonicalMutations()
        #expect(firstLaunch.pendingCanonicalMutations == [linkedMutation])
        let conflictDiagnostic = "The exact status write was rejected as stale."
        firstLaunch.markCanonicalMutationConflicted(
            itemID: Self.itemID,
            diagnostic: conflictDiagnostic
        )
        var conflictedMutation = linkedMutation
        conflictedMutation.disposition = .conflicted
        conflictedMutation.diagnostic = conflictDiagnostic
        #expect(firstLaunch.pendingCanonicalMutations == [conflictedMutation])
        #expect(firstLaunch.executionState.terminalOutcomes[older.id]?.projection == .notRequired)
        #expect(!firstLaunch.canRetryCanonicalMutation(conflictedMutation))
        #expect(!firstLaunch.canKeepLatestCanonicalItem(forExecutionSession: older.id))

        var recomposedBlock = Self.block()
        recomposedBlock.syncOrigin = .localComposition
        let compositionNow = Date()
        let provenance = LocalScheduleCompositionProvenance(
            configurationIdentifier: Self.canonicalConfiguration,
            localInputFingerprint: "local-sha256:\(String(repeating: "a", count: 64))",
            generatedAt: compositionNow,
            asOf: compositionNow,
            horizonStart: compositionNow.addingTimeInterval(-60),
            horizonEnd: compositionNow.addingTimeInterval(7 * 24 * 60 * 60),
            timezoneName: "Europe/Madrid",
            sourceItemRevisions: [Self.itemID: 1]
        )
        #expect(firstLaunch.beginCanonicalSync())
        try firstLaunch.commitLocalScheduleComposition(
            blocks: [recomposedBlock],
            message: "Recomposed after deferred history",
            provenance: provenance
        )
        firstLaunch.endCanonicalSync()
        #expect(firstLaunch.pendingCanonicalMutations == [conflictedMutation])
        #expect(firstLaunch.canonicalPreviewFreshnessIssue == nil)
        #expect(!firstLaunch.canRetryCanonicalMutation(conflictedMutation))
        #expect(firstLaunch.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)

        firstLaunch.flushPersistence()
        let secondLaunch = PlannerStore.live(persistence: context.persistence)
        #expect(secondLaunch.persistenceError == nil)
        #expect(secondLaunch.pendingCanonicalMutations == [conflictedMutation])
        #expect(secondLaunch.executionState.terminalOutcomes[older.id]?.projection == .notRequired)
        let secondTransport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [deferred, older], nextOffset: nil)]
        )
        #expect(
            await Self.controller(planner: secondLaunch, transport: secondTransport).refresh()
                == .success
        )
        #expect(secondLaunch.pendingCanonicalMutations == [conflictedMutation])
        #expect(!secondLaunch.canRetryCanonicalMutation(conflictedMutation))
        #expect(!secondLaunch.canKeepLatestCanonicalItem(forExecutionSession: older.id))
        #expect(secondLaunch.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
    }

    @Test("a newer deferred recurrence session removes stale outcome and completion anchor")
    func newerDeferredRemovesRecurrenceAnchorAcrossRelaunch() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let older = try Self.terminalSession(
            sessionID: Self.uuid(59),
            sessionIndex: 0,
            occurrenceID: Self.occurrenceID,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate
        )
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(60),
            sessionIndex: 0,
            occurrenceID: Self.occurrenceID,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate.addingTimeInterval(100)
        )
        var state = Self.emptyBoundState
        state.revision = older.revision
        state.historyWindow = [older]
        state.historyWindowRevision = older.revision
        state.terminalOutcomes[older.id] = .init(
            session: older,
            recordedAt: older.updatedAt,
            projection: .notRequired
        )
        state.presentedBlockIDs = [Self.blockID]
        let recurrenceOutcome = RecurrenceSessionOutcome(
            itemID: Self.itemID,
            occurrenceID: Self.occurrenceID,
            sessionIndex: 0,
            disposition: .completed,
            occurredAt: older.endedAt ?? older.updatedAt,
            occurrenceFullyScheduled: true
        )
        var completedBlock = Self.block(occurrenceID: Self.occurrenceID)
        completedBlock.status = .completed
        completedBlock.actualMinutes = 1
        let seeded = Self.planner(
            persistence: context.persistence,
            blocks: [completedBlock],
            canonicalItems: [try Self.canonicalItem()],
            completedOccurrenceIDs: [Self.occurrenceID],
            recurrenceSessionOutcomes: [recurrenceOutcome],
            executionState: state
        )
        seeded.flushPersistence()

        let firstLaunch = PlannerStore.live(persistence: context.persistence)
        #expect(firstLaunch.recurrenceCompletionAnchors()[Self.itemID] == recurrenceOutcome.occurredAt)
        let snapshot = DayWeaveExecutionSnapshot(revision: 4, activeSession: nil)
        let firstTransport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [deferred, older], nextOffset: nil)]
        )
        #expect(
            await Self.controller(planner: firstLaunch, transport: firstTransport).refresh()
                == .success
        )
        #expect(firstLaunch.recurrenceSessionOutcomes.isEmpty)
        #expect(firstLaunch.recurrenceCompletionAnchors().isEmpty)
        #expect(!firstLaunch.completedOccurrenceIDs.contains(Self.occurrenceID))
        #expect(firstLaunch.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)

        firstLaunch.flushPersistence()
        let secondLaunch = PlannerStore.live(persistence: context.persistence)
        #expect(secondLaunch.persistenceError == nil)
        #expect(secondLaunch.recurrenceSessionOutcomes.isEmpty)
        #expect(secondLaunch.recurrenceCompletionAnchors().isEmpty)
        let secondTransport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [deferred, older], nextOffset: nil)]
        )
        #expect(
            await Self.controller(planner: secondLaunch, transport: secondTransport).refresh()
                == .success
        )
        #expect(secondLaunch.recurrenceSessionOutcomes.isEmpty)
        #expect(secondLaunch.recurrenceCompletionAnchors().isEmpty)
        #expect(secondLaunch.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
    }

    @Test("eligible terminal outcomes enter the existing approval-safe canonical mutation path")
    func terminalOutcomeStagesLinkedProjection() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(60),
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate
        )
        var state = Self.emptyBoundState
        state.leaseProjectionEligibility[terminal.id] = true
        let snapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [terminal], nextOffset: nil)]
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()],
            executionState: state
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh() == .success)
        let mutation = try #require(planner.pendingCanonicalMutations.first)
        #expect(mutation.executionSessionID == terminal.id)
        #expect(mutation.baseRevision == terminal.itemRevision)
        #expect(mutation.desiredStatus == .completed)
        #expect(mutation.disposition == .pending)

        planner.flushPersistence()
        let relaunched = PlannerStore.live(persistence: context.persistence)
        #expect(relaunched.persistenceError == nil)
        #expect(relaunched.executionState.terminalOutcomes[terminal.id]?.session == terminal)
        #expect(relaunched.pendingCanonicalMutations.first?.executionSessionID == terminal.id)
    }

    @Test("the authoritative open lease overrides a later-dated terminal on the same target")
    func authoritativeActiveLeaseWinsDespiteOlderTimestamp() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(70),
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate
        )
        let active = try Self.activeSession(
            sessionID: Self.uuid(71),
            startedAt: Self.baseDate.addingTimeInterval(-100)
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: active)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [terminal, active], nextOffset: nil)]
        )
        var state = Self.emptyBoundState
        state.leaseProjectionEligibility[terminal.id] = true
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()],
            executionState: state
        )

        #expect(await Self.controller(planner: planner, transport: transport).refresh() == .success)
        #expect(planner.blocks.first(where: { $0.id == Self.blockID })?.status == .active)
        #expect(planner.executionState.terminalOutcomes[terminal.id]?.projection == .notRequired)
        #expect(planner.pendingCanonicalMutations.isEmpty)
    }

    @Test("an expired timed break remains paused until an explicit local choice")
    func expiredBreakRequiresChoice() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(80))
        var state = Self.emptyBoundState
        state.revision = 2
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = 2
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let transport = ExecutionTransportDouble(snapshots: [], pages: [])
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { paused.pauseUntil!.addingTimeInterval(1) }
        )

        #expect(sync.expiredBreakChoiceRequired)
        #expect(sync.keepPausedAfterExpiredBreak() == .success)
        #expect(!sync.expiredBreakChoiceRequired)
        #expect(planner.executionState.activeSession?.status == .paused)
    }

    @Test("execution refresh waits for the shared canonical mutation fence")
    func executionRefreshWaitsForSharedMutationFence() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(planner: planner, transport: transport)
        #expect(planner.beginCanonicalSync())

        let refresh = Task { @MainActor in
            await sync.refresh()
        }
        try await Task.sleep(for: .milliseconds(75))
        #expect(await transport.snapshotRequestCount() == 0)

        planner.endCanonicalSync()
        #expect(await refresh.value == .success)
        #expect(await transport.snapshotRequestCount() == 2)
        #expect(planner.executionState.historyVerified)
    }

    @Test("concurrent foreground operations are rejected before duplicate network I/O")
    func concurrentRefreshIsSerialized() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)],
            snapshotDelay: .milliseconds(100)
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(planner: planner, transport: transport)
        let first = Task { @MainActor in await sync.refresh() }
        try await Task.sleep(for: .milliseconds(20))

        #expect(await sync.refresh() == .invalidLocalState)
        #expect(await first.value == .success)
        #expect(await transport.snapshotRequestCount() == 2)
    }

    @Test("foreground polling is idempotent across repeated window activation")
    func foregroundPollingStartsOnlyOnce() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty, empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)],
            snapshotDelay: .milliseconds(80)
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(planner: planner, transport: transport)

        sync.startForegroundPolling(every: .seconds(60))
        try await Task.sleep(for: .milliseconds(20))
        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !planner.executionState.historyVerified, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        sync.stopForegroundPolling()

        #expect(await transport.snapshotRequestCount() == 2)
        #expect(planner.executionState.historyVerified)
    }

    private static var emptyBoundState: DayWeaveExecutionDurableState {
        var state = DayWeaveExecutionDurableState.empty
        state.deviceID = deviceID
        state.bindingIdentifier = binding
        state.historyWindowRevision = 0
        state.historyContinuityEstablished = true
        state.historyVerified = true
        return state
    }

    private static func pendingStart() throws -> DayWeavePendingExecutionCommand {
        let sessionID = uuid(90)
        let command = DayWeaveExecutionCommand.start(
            sessionID: sessionID,
            itemID: itemID,
            itemRevision: 1,
            occurrenceID: nil,
            sessionIndex: 0,
            plannedBlockID: blockID,
            deviceID: deviceID
        )
        let request = DayWeaveExecutionCommandRequest(expectedRevision: 0, command: command)
        return .init(
            idempotencyKey: "mac-execution-pending-start",
            bindingIdentifier: binding,
            expectedRevision: 0,
            identity: .init(
                sessionID: sessionID,
                itemID: itemID,
                itemRevision: 1,
                occurrenceID: nil,
                sessionIndex: 0,
                plannedBlockID: blockID,
                sourceDeviceID: deviceID
            ),
            command: command,
            encodedRequest: try DayWeaveExecutionWireCodec.encode(request),
            priorSession: nil,
            focusedBlockID: blockID,
            canonicalProjectionEligibleAtLeaseStart: true,
            stagedAt: baseDate
        )
    }

    private static func planner(
        persistence: EncryptedPlannerPersistence,
        blocks: [ScheduleBlock] = [],
        canonicalItems: [DayWeaveCanonicalItem] = [],
        completedOccurrenceIDs: Set<UUID> = [],
        pendingCanonicalMutations: [PendingCanonicalMutation] = [],
        recurrenceSessionOutcomes: [RecurrenceSessionOutcome] = [],
        executionState: DayWeaveExecutionDurableState = emptyBoundState
    ) -> PlannerStore {
        PlannerStore(
            blocks: blocks,
            canonicalItems: canonicalItems,
            completedOccurrenceIDs: completedOccurrenceIDs,
            pendingCanonicalMutations: pendingCanonicalMutations,
            recurrenceSessionOutcomes: recurrenceSessionOutcomes,
            canonicalConfigurationIdentifier: canonicalConfiguration,
            executionState: executionState,
            previewValidatedForCurrentLaunch: true,
            persistence: persistence,
            restoreFromPersistence: false,
            now: { baseDate }
        )
    }

    private static func controller(
        planner: PlannerStore,
        transport: ExecutionTransportDouble,
        binding: String = binding,
        now: @escaping @Sendable () -> Date = { baseDate }
    ) -> ExecutionSyncStore {
        let connection = DayWeaveExecutionConnection(
            canonicalConfigurationIdentifier: canonicalConfiguration,
            bindingIdentifier: binding,
            transport: transport
        )
        return ExecutionSyncStore(
            planner: planner,
            connectionProvider: { connection },
            now: now,
            makeUUID: { uuid(500) }
        )
    }

    private static func block(
        id: UUID = blockID,
        sessionIndex: UInt16 = 0,
        occurrenceID: UUID? = nil
    ) -> ScheduleBlock {
        ScheduleBlock(
            id: id,
            title: "Write plan",
            kind: .task,
            start: baseDate,
            end: baseDate.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: 1,
            occurrenceID: occurrenceID,
            sessionIndex: sessionIndex,
            syncOrigin: .canonicalPreview,
            placementReason: nil,
            previewKind: "planned",
            occurrenceFullyScheduled: true
        )
    }

    private static func canonicalItem(splittable: Bool = false) throws -> DayWeaveCanonicalItem {
        let split = splittable
            ? #"{"type":"splittable","minimum_chunk_seconds":300,"maximum_chunk_seconds":1800}"#
            : #"{"type":"indivisible"}"#
        let json = #"""
        {
          "id":"\#(itemID.uuidString.lowercased())","is_sensitive":false,"kind":"task","status":"scheduled",
          "title":"Write plan","notes":null,"timezone_name":"UTC","duration_seconds":1800,
          "deadline_at":null,"earliest_start_at":null,"recurrence":null,
          "flexible_constraints":{},"split_policy":\#(split),"importance":50,"urgency":50,
          "parent_id":null,"sibling_order":0,"is_executable":true,"revision":1,
          "created_at":"2027-01-15T08:00:00Z","updated_at":"2027-01-15T08:00:00Z",
          "completed_at":null,"deleted_at":null
        }
        """#
        return try decoder().decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }

    private static func activeSession(
        sessionID: UUID,
        startedAt: Date = baseDate
    ) throws -> DayWeaveExecutionSession {
        try session(
            id: sessionID,
            status: .active,
            revision: 1,
            sessionIndex: 0,
            plannedBlockID: blockID,
            startedAt: startedAt,
            updatedAt: startedAt,
            accumulatedSeconds: 0,
            actualSeconds: nil,
            runningSince: startedAt,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: nil
        )
    }

    private static func terminalSession(
        sessionID: UUID,
        sessionIndex: UInt16,
        occurrenceID: UUID? = nil,
        plannedBlockID: UUID? = nil,
        startedAt: Date
    ) throws -> DayWeaveExecutionSession {
        try session(
            id: sessionID,
            status: .completed,
            revision: 2,
            sessionIndex: sessionIndex,
            occurrenceID: occurrenceID,
            plannedBlockID: plannedBlockID,
            startedAt: startedAt,
            updatedAt: startedAt.addingTimeInterval(1),
            accumulatedSeconds: 1,
            actualSeconds: 1,
            runningSince: nil,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: startedAt.addingTimeInterval(1)
        )
    }

    private static func deferredSession(
        sessionID: UUID,
        sessionIndex: UInt16,
        occurrenceID: UUID? = nil,
        plannedBlockID: UUID?,
        startedAt: Date
    ) throws -> DayWeaveExecutionSession {
        let updatedAt = startedAt.addingTimeInterval(1)
        return try session(
            id: sessionID,
            status: .deferred,
            revision: 2,
            sessionIndex: sessionIndex,
            occurrenceID: occurrenceID,
            plannedBlockID: plannedBlockID,
            startedAt: startedAt,
            updatedAt: updatedAt,
            accumulatedSeconds: 1,
            actualSeconds: 1,
            runningSince: nil,
            pausedAt: nil,
            pauseUntil: nil,
            moveStart: updatedAt.addingTimeInterval(3_600),
            moveEnd: updatedAt.addingTimeInterval(5_400),
            endedAt: updatedAt
        )
    }

    private static func pausedSession(sessionID: UUID) throws -> DayWeaveExecutionSession {
        let updated = baseDate.addingTimeInterval(10)
        return try session(
            id: sessionID,
            status: .paused,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: blockID,
            startedAt: baseDate,
            updatedAt: updated,
            accumulatedSeconds: 10,
            actualSeconds: nil,
            runningSince: nil,
            pausedAt: updated,
            pauseUntil: updated.addingTimeInterval(60),
            endedAt: nil
        )
    }

    private static func session(
        id: UUID,
        status: DayWeaveExecutionStatus,
        revision: UInt64,
        sessionIndex: UInt16,
        occurrenceID: UUID? = nil,
        plannedBlockID: UUID?,
        startedAt: Date,
        updatedAt: Date,
        accumulatedSeconds: UInt64,
        actualSeconds: UInt64?,
        runningSince: Date?,
        pausedAt: Date?,
        pauseUntil: Date?,
        moveStart: Date? = nil,
        moveEnd: Date? = nil,
        endedAt: Date?
    ) throws -> DayWeaveExecutionSession {
        let object: [String: Any] = [
            "id": id.uuidString.lowercased(),
            "item_id": itemID.uuidString.lowercased(),
            "item_revision": 1,
            "occurrence_id": occurrenceID?.uuidString.lowercased() ?? NSNull(),
            "session_index": Int(sessionIndex),
            "planned_block_id": plannedBlockID?.uuidString.lowercased() ?? NSNull(),
            "source_device_id": deviceID.uuidString.lowercased(),
            "status": status.rawValue,
            "revision": revision,
            "accumulated_seconds": accumulatedSeconds,
            "actual_seconds": actualSeconds ?? NSNull(),
            "started_at": format(startedAt),
            "running_since": runningSince.map(format) ?? NSNull(),
            "paused_at": pausedAt.map(format) ?? NSNull(),
            "pause_until": pauseUntil.map(format) ?? NSNull(),
            "pause_reason": NSNull(),
            "move_start": moveStart.map(format) ?? NSNull(),
            "move_end": moveEnd.map(format) ?? NSNull(),
            "ended_at": endedAt.map(format) ?? NSNull(),
            "created_at": format(startedAt),
            "updated_at": format(updatedAt),
        ]
        return try decoder().decode(
            DayWeaveExecutionSession.self,
            from: JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        )
    }

    private static func mutation(
        revision: UInt64,
        active: DayWeaveExecutionSession?,
        changed: DayWeaveExecutionSession,
        replayed: Bool
    ) throws -> DayWeaveExecutionMutation {
        let object: [String: Any] = [
            "revision": revision,
            "active_session": active.map(sessionObject) ?? NSNull(),
            "changed_session": sessionObject(changed),
            "replayed": replayed,
        ]
        return try decoder().decode(
            DayWeaveExecutionMutation.self,
            from: JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        )
    }

    private static func sessionObject(_ session: DayWeaveExecutionSession) -> [String: Any] {
        [
            "id": session.id.uuidString.lowercased(),
            "item_id": session.itemID.uuidString.lowercased(),
            "item_revision": session.itemRevision,
            "occurrence_id": session.occurrenceID?.uuidString.lowercased() ?? NSNull(),
            "session_index": session.sessionIndex,
            "planned_block_id": session.plannedBlockID?.uuidString.lowercased() ?? NSNull(),
            "source_device_id": session.sourceDeviceID.uuidString.lowercased(),
            "status": session.status.rawValue,
            "revision": session.revision,
            "accumulated_seconds": session.accumulatedSeconds,
            "actual_seconds": session.actualSeconds ?? NSNull(),
            "started_at": format(session.startedAt),
            "running_since": session.runningSince.map(format) ?? NSNull(),
            "paused_at": session.pausedAt.map(format) ?? NSNull(),
            "pause_until": session.pauseUntil.map(format) ?? NSNull(),
            "pause_reason": session.pauseReason ?? NSNull(),
            "move_start": session.moveStart.map(format) ?? NSNull(),
            "move_end": session.moveEnd.map(format) ?? NSNull(),
            "ended_at": session.endedAt.map(format) ?? NSNull(),
            "created_at": format(session.createdAt),
            "updated_at": format(session.updatedAt),
        ]
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = parseDate(value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Invalid test date"
                )
            }
            return date
        }
        return decoder
    }

    nonisolated private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    nonisolated private static func parseDate(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) { return date }
        let whole = ISO8601DateFormatter()
        whole.formatOptions = [.withInternetDateTime]
        return whole.date(from: value)
    }

    private static func newestFirst(
        _ left: DayWeaveExecutionSession,
        _ right: DayWeaveExecutionSession
    ) -> Bool {
        if left.updatedAt != right.updatedAt { return left.updatedAt > right.updatedAt }
        return left.id.uuidString.lowercased() > right.id.uuidString.lowercased()
    }

    nonisolated private static func uuid(_ number: Int) -> UUID {
        UUID(uuidString: String(format: "10000000-0000-4000-8000-%012d", number))!
    }

    private static func persistenceContext() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeaveExecutionSyncTests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let key = try PlannerEncryptionKey(data: Data((0..<32).map(UInt8.init)))
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: key
            )
        )
    }
}

private actor ExecutionTransportDouble: DayWeaveExecutionTransport {
    enum Reply: Sendable {
        case mutation(DayWeaveExecutionMutation)
        case failure(DayWeaveAPIError)
    }

    struct ReceivedCommand: Equatable, Sendable {
        let body: Data
        let key: String
    }

    private var snapshots: [DayWeaveExecutionSnapshot]
    private var pages: [DayWeaveExecutionHistoryPage]
    private var commandReplies: [Reply]
    private var offsets: [Int] = []
    private var commands: [ReceivedCommand] = []
    private var snapshotCount = 0
    private let snapshotDelay: Duration?

    init(
        snapshots: [DayWeaveExecutionSnapshot],
        pages: [DayWeaveExecutionHistoryPage],
        commandReplies: [Reply] = [],
        snapshotDelay: Duration? = nil
    ) {
        self.snapshots = snapshots
        self.pages = pages
        self.commandReplies = commandReplies
        self.snapshotDelay = snapshotDelay
    }

    func executionSnapshot() async throws -> DayWeaveExecutionSnapshot {
        snapshotCount += 1
        if let snapshotDelay { try await Task.sleep(for: snapshotDelay) }
        guard !snapshots.isEmpty else { throw DayWeaveAPIError.responseDecodingFailed }
        return snapshots.removeFirst()
    }

    func executionHistoryPage(limit: Int, offset: Int) async throws
        -> DayWeaveExecutionHistoryPage
    {
        offsets.append(offset)
        guard limit == DayWeaveAPIClient.maximumExecutionHistoryLimit,
              !pages.isEmpty else { throw DayWeaveAPIError.responseDecodingFailed }
        return pages.removeFirst()
    }

    nonisolated func encodedExecutionCommand(
        _ request: DayWeaveExecutionCommandRequest
    ) throws -> Data {
        try DayWeaveExecutionWireCodec.encode(request)
    }

    func applyExecutionCommand(
        encodedRequest: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveExecutionMutation {
        commands.append(.init(body: encodedRequest, key: idempotencyKey))
        guard !commandReplies.isEmpty else { throw DayWeaveAPIError.responseDecodingFailed }
        switch commandReplies.removeFirst() {
        case let .mutation(mutation): return mutation
        case let .failure(error): throw error
        }
    }

    func requestedOffsets() -> [Int] { offsets }
    func receivedCommands() -> [ReceivedCommand] { commands }
    func snapshotRequestCount() -> Int { snapshotCount }
}
#endif
