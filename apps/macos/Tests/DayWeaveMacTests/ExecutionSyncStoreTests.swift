import Foundation
import CryptoKit
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
            habitCompositionProvider: ExecutionHabitCheckpointStub(Self.habitCheckpoint()),
            configurationStore: TestSuggestionConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            tokenStore: TestBearerTokenStore(
                token: token,
                origin: baseURL.credentialOriginIdentifier
            ),
            session: URLProtocolStub.makeSession(),
            now: { Self.baseDate },
            breakNotificationCoordinator: DayWeaveNoopBreakNotificationCoordinator()
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

    @Test("Start requires the durable proof for the exact unchanged published block")
    func startRequiresExactPublishedBlockProof() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()]
        )
        let priorStart = planner.blocks[0].start
        planner.doLater(Self.blockID)
        #expect(planner.blocks.first?.start == priorStart)
        #expect(planner.publishedScheduleProof != nil)
        planner.invalidateCanonicalPreview()
        let transport = Self.emptyReadTransport()

        #expect(planner.blocks.first?.start == priorStart)
        #expect(await Self.controller(planner: planner, transport: transport).start(Self.blockID)
            == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)

        let mismatchContext = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: mismatchContext.directory) }
        let exactMismatch = Self.planner(
            persistence: mismatchContext.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()]
        )
        exactMismatch.blocks[0].start.addTimeInterval(300)
        exactMismatch.blocks[0].end.addTimeInterval(300)
        #expect(exactMismatch.publishedScheduleProof != nil)
        #expect(exactMismatch.canonicalScheduleBlockActionabilityIssue(
            exactMismatch.blocks[0]
        ) != nil)

        let unprovenContext = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: unprovenContext.directory) }
        let unproven = Self.planner(
            persistence: unprovenContext.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()],
            includePublicationProof: false
        )
        let unprovenTransport = Self.emptyReadTransport()
        #expect(await Self.controller(
            planner: unproven,
            transport: unprovenTransport
        ).start(Self.blockID) == .invalidLocalState)
        #expect(await unprovenTransport.receivedCommands().isEmpty)
    }

    @Test("Start rejects a missing server session index instead of synthesizing zero")
    func startRejectsMissingSessionIndex() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block(sessionIndex: nil)],
            canonicalItems: [try Self.canonicalItem()],
            includePublicationProof: false
        )
        let transport = Self.emptyReadTransport()

        #expect(await Self.controller(planner: planner, transport: transport).start(Self.blockID)
            == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("A locally composed helper block stays visible but cannot Start")
    func localHelperBlockIsNotActionable() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let item = try Self.canonicalItem()
        var helperBlock = Self.block()
        helperBlock.syncOrigin = .localComposition
        let provenance = LocalScheduleCompositionProvenance(
            configurationIdentifier: Self.canonicalConfiguration,
            localInputFingerprint: "local-sha256:\(String(repeating: "a", count: 64))",
            generatedAt: Self.baseDate,
            asOf: Self.baseDate,
            horizonStart: Self.baseDate.addingTimeInterval(-3_600),
            horizonEnd: Self.baseDate.addingTimeInterval(86_400),
            timezoneName: "UTC",
            sourceItemRevisions: [item.id: item.revision]
        )
        let planner = PlannerStore(
            blocks: [helperBlock],
            canonicalItems: [item],
            canonicalConfigurationIdentifier: Self.canonicalConfiguration,
            localScheduleCompositionProvenance: provenance,
            executionState: Self.emptyBoundState,
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.baseDate }
        )
        let transport = Self.emptyReadTransport()

        #expect(planner.blocks == [helperBlock])
        #expect(await Self.controller(planner: planner, transport: transport).start(Self.blockID)
            == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("pending or unreadable habit authority blocks only a new Start")
    func habitAuthorityFailsClosedAtExecutionStart() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()],
            canonicalItems: [try Self.canonicalItem()]
        )
        let provider = ExecutionHabitCheckpointStub(Self.habitCheckpoint(
            pendingMutationIDs: [Self.uuid(902)]
        ))
        let transport = Self.emptyReadTransport()
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            habitProvider: provider
        )

        #expect(sync.habitExecutionStartIsBlocked)
        #expect(await sync.start(Self.blockID) == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)
        #expect(planner.executionState.pendingCommand == nil)

        let priorGeneration = sync.habitExecutionReadinessGeneration
        provider.update(Self.habitCheckpoint())
        #expect(!sync.habitExecutionStartIsBlocked)
        #expect(sync.habitExecutionReadinessGeneration == priorGeneration + 1)

        provider.update(Self.habitCheckpoint(
            configurationIdentifier: nil,
            deltaCursor: nil,
            deltaCaughtUp: false
        ))
        #expect(sync.habitExecutionStartIsBlocked)
        planner.skip(Self.blockID)
        #expect(planner.blocks.first?.status == .skipped)
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

    @Test("Will do later pauses first and uses only the server-assessed exact placement")
    func deferUsesPausedAuthoritativeDurationAndFreshPublicationFence() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_120)
        let active = try Self.activeSession(sessionID: sessionID)
        let pausedAt = Self.baseDate.addingTimeInterval(300)
        let paused = try Self.session(
            id: sessionID,
            status: .paused,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: pausedAt,
            accumulatedSeconds: 300,
            actualSeconds: nil,
            runningSince: nil,
            pausedAt: pausedAt,
            pauseUntil: nil,
            endedAt: nil
        )
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let deferredAt = pausedAt.addingTimeInterval(1)
        let deferred = try Self.session(
            id: sessionID,
            status: .deferred,
            revision: 3,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: deferredAt,
            accumulatedSeconds: 300,
            actualSeconds: 300,
            runningSince: nil,
            pausedAt: pausedAt,
            pauseUntil: nil,
            moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500),
            endedAt: deferredAt
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let deferredSnapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [
                activeSnapshot, activeSnapshot,
                pausedSnapshot, pausedSnapshot,
                pausedSnapshot, pausedSnapshot,
                deferredSnapshot, deferredSnapshot,
            ],
            pages: [
                .init(sessions: [active], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [deferred], nextOffset: nil),
            ],
            commandReplies: [
                .mutation(try Self.mutation(
                    revision: 2,
                    active: paused,
                    changed: paused,
                    replayed: false
                )),
                .mutation(try Self.mutation(
                    revision: 3,
                    active: nil,
                    changed: deferred,
                    replayed: false
                )),
            ]
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.historyContinuityEstablished = true
        durable.historyVerified = true
        durable.leaseProjectionEligibility[sessionID] = true
        durable.presentedBlockIDs = [Self.blockID]
        var activeBlock = Self.block()
        activeBlock.status = .active
        var locallyFixed = Self.block(id: Self.uuid(1_119))
        locallyFixed.sourceItemID = nil
        locallyFixed.sourceItemRevision = nil
        locallyFixed.start = moveStart.addingTimeInterval(60)
        locallyFixed.end = moveStart.addingTimeInterval(300)
        locallyFixed.isFlexible = false
        locallyFixed.isHardConstraint = true
        locallyFixed.syncOrigin = .local
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [activeBlock, locallyFixed],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { pausedAt }
        )

        #expect(await sync.deferWork(
            Self.blockID,
            moveStart: moveStart,
            latestFinish: moveStart.addingTimeInterval(60)
        ) == .success)
        let receivedCommands = await transport.receivedCommands()
        let commands = try receivedCommands.map {
            try DayWeaveExecutionWireCodec.decode($0.body).command
        }
        #expect(commands.count == 2)
        if case let .pause(id, duration, until, reason) = commands[0] {
            #expect(id == sessionID)
            #expect(duration == nil)
            #expect(until == nil)
            #expect(reason == nil)
        } else {
            Issue.record("Expected an exact indefinite pause before defer")
        }
        #expect(commands[1] == .deferWork(
            sessionID: sessionID,
            moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500),
            actualSeconds: 300,
            assessmentDigest: "sha256:\(String(repeating: "d", count: 64))",
            approvedAssessmentDigest: nil
        ))
        let assessmentRequests = await transport.receivedAssessmentRequests()
        #expect(assessmentRequests == [
            .init(
                expectedRevision: 2,
                sessionID: sessionID,
                moveStart: moveStart,
                actualSeconds: 300
            ),
        ])
        #expect(planner.executionState.activeSession == nil)
        #expect(planner.executionState.terminalOutcomes[sessionID]?.session == deferred)
        #expect(planner.executionState.terminalOutcomes[sessionID]?.projection == .notRequired)
        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.pendingCanonicalMutations.isEmpty)
        #expect(planner.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
    }

    @Test("conflict approval is durable and the staged Defer replays unchanged after expiry")
    func deferConflictApprovalAndExpiredReplayAreExact() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_146))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let assessment = Self.deferAssessment(
            session: paused,
            executionRevision: paused.revision,
            moveStart: moveStart,
            digestByte: "e",
            approvalRequired: true,
            expiresAt: Self.baseDate.addingTimeInterval(300)
        )
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let assessmentTransport = ExecutionTransportDouble(
            snapshots: [],
            pages: [],
            assessmentReplies: [.assessment(assessment)]
        )

        #expect(await Self.controller(
            planner: planner,
            transport: assessmentTransport
        ).deferWork(Self.blockID, moveStart: moveStart) == .approvalRequired)
        #expect(await assessmentTransport.receivedCommands().isEmpty)
        #expect(planner.executionState.activeSession == paused)
        #expect(planner.pendingExecutionDeferIntent?.assessment == assessment)
        #expect(planner.pendingExecutionDeferIntent?.approvedAssessmentDigest == nil)

        let restored = PlannerStore(
            persistence: context.persistence,
            now: { Self.baseDate }
        )
        #expect(restored.persistenceError == nil)
        #expect(restored.pendingExecutionDeferIntent?.assessment == assessment)
        #expect(restored.pendingExecutionDeferIntent?.approvedAssessmentDigest == nil)
        let pausedSnapshot = DayWeaveExecutionSnapshot(
            revision: paused.revision,
            activeSession: paused
        )
        let commandTransport = ExecutionTransportDouble(
            snapshots: [pausedSnapshot, pausedSnapshot],
            pages: [.init(sessions: [paused], nextOffset: nil)],
            commandReplies: [.failure(.transport(.networkConnectionLost))]
        )

        #expect(await Self.controller(
            planner: restored,
            transport: commandTransport
        ).approveDeferredWork(
            Self.blockID,
            assessmentDigest: assessment.assessmentDigest
        ) == .transientNetworkFailure)
        let lostRequest = try #require((await commandTransport.receivedCommands()).first)
        let staged = try #require(restored.executionState.pendingCommand)
        let stagedIntent = try #require(restored.pendingExecutionDeferIntent)
        #expect(staged.encodedRequest == lostRequest.body)
        #expect(restored.pendingExecutionDeferIntent?.approvedAssessmentDigest
            == assessment.assessmentDigest)
        #expect(try DayWeaveExecutionWireCodec.decode(lostRequest.body).command == .deferWork(
            sessionID: paused.id,
            moveStart: assessment.moveStart,
            moveEnd: assessment.moveEnd,
            actualSeconds: assessment.actualSeconds,
            assessmentDigest: assessment.assessmentDigest,
            approvedAssessmentDigest: assessment.assessmentDigest
        ))
        #expect(Self.controller(
            planner: restored,
            transport: commandTransport
        ).cancelDeferredWork(stagedIntent) == .invalidLocalState)
        #expect(restored.executionState.pendingCommand == staged)
        #expect(restored.pendingExecutionDeferIntent == stagedIntent)

        let relaunched = PlannerStore(
            persistence: context.persistence,
            now: { assessment.expiresAt.addingTimeInterval(1) }
        )
        let deferredAt = paused.updatedAt.addingTimeInterval(1)
        let deferred = try Self.session(
            id: paused.id,
            status: .deferred,
            revision: paused.revision + 1,
            sessionIndex: paused.sessionIndex,
            plannedBlockID: Self.blockID,
            startedAt: paused.startedAt,
            updatedAt: deferredAt,
            accumulatedSeconds: paused.accumulatedSeconds,
            actualSeconds: assessment.actualSeconds,
            runningSince: nil,
            pausedAt: paused.pausedAt,
            pauseUntil: nil,
            moveStart: assessment.moveStart,
            moveEnd: assessment.moveEnd,
            endedAt: deferredAt
        )
        let deferredSnapshot = DayWeaveExecutionSnapshot(
            revision: paused.revision + 1,
            activeSession: nil
        )
        let replayTransport = ExecutionTransportDouble(
            snapshots: [deferredSnapshot, deferredSnapshot],
            pages: [.init(sessions: [deferred], nextOffset: nil)],
            commandReplies: [.mutation(try Self.mutation(
                revision: paused.revision + 1,
                active: nil,
                changed: deferred,
                replayed: true
            ))]
        )

        #expect(await Self.controller(
            planner: relaunched,
            transport: replayTransport,
            now: { assessment.expiresAt.addingTimeInterval(1) }
        ).refresh() == .success)
        let replayed = try #require((await replayTransport.receivedCommands()).first)
        #expect(replayed == lostRequest)
        #expect(relaunched.executionState.pendingCommand == nil)
        #expect(relaunched.pendingExecutionDeferIntent == nil)
        #expect(relaunched.executionState.terminalOutcomes[paused.id]?.session == deferred)
    }

    @Test("a lost Defer response replays exact bytes after relaunch and closes the saved move")
    func lostDeferResponseReplaysExactlyAfterRelaunch() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_121)
        let active = try Self.activeSession(sessionID: sessionID)
        let pausedAt = Self.baseDate.addingTimeInterval(300)
        let paused = try Self.session(
            id: sessionID, status: .paused, revision: 2, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: pausedAt,
            accumulatedSeconds: 300, actualSeconds: nil, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, endedAt: nil
        )
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let deferredAt = pausedAt.addingTimeInterval(1)
        let deferred = try Self.session(
            id: sessionID, status: .deferred, revision: 3, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: deferredAt,
            accumulatedSeconds: 300, actualSeconds: 300, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500), endedAt: deferredAt
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let firstTransport = ExecutionTransportDouble(
            snapshots: [
                activeSnapshot, activeSnapshot,
                pausedSnapshot, pausedSnapshot,
                pausedSnapshot, pausedSnapshot,
            ],
            pages: [
                .init(sessions: [active], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
            ],
            commandReplies: [
                .mutation(try Self.mutation(
                    revision: 2, active: paused, changed: paused, replayed: false
                )),
                .failure(.transport(.networkConnectionLost)),
            ]
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.presentedBlockIDs = [Self.blockID]
        var activeBlock = Self.block()
        activeBlock.status = .active
        let firstPlanner = Self.planner(
            persistence: context.persistence,
            blocks: [activeBlock],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let firstSync = Self.controller(
            planner: firstPlanner, transport: firstTransport, now: { pausedAt }
        )

        #expect(await firstSync.deferWork(Self.blockID, moveStart: moveStart)
            == .transientNetworkFailure)
        let firstCommands = await firstTransport.receivedCommands()
        let lostDefer = try #require(firstCommands.last)
        #expect(firstPlanner.executionState.pendingCommand?.encodedRequest == lostDefer.body)
        #expect(firstPlanner.pendingExecutionDeferIntent?.moveStart == moveStart)

        let restored = PlannerStore.live(persistence: context.persistence)
        #expect(restored.persistenceError == nil)
        #expect(restored.pendingExecutionDeferIntent?.moveStart == moveStart)
        let deferredSnapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: nil)
        let replayTransport = ExecutionTransportDouble(
            snapshots: [deferredSnapshot, deferredSnapshot],
            pages: [.init(sessions: [deferred], nextOffset: nil)],
            commandReplies: [.mutation(try Self.mutation(
                revision: 3, active: nil, changed: deferred, replayed: true
            ))]
        )
        let replaySync = Self.controller(
            planner: restored, transport: replayTransport, now: { pausedAt }
        )
        var publicationAttempts = 0
        replaySync.installDeferredPublicationCoordinator {
            publicationAttempts += 1
            return false
        }

        #expect(await replaySync.refreshAndCoordinateDeferredPublication() == .success)
        let replay = try #require((await replayTransport.receivedCommands()).first)
        #expect(replay == lostDefer)
        #expect(restored.executionState.pendingCommand == nil)
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.terminalOutcomes[sessionID]?.session == deferred)
        #expect(restored.deferredExecutionPublicationSessionIDs == [sessionID])
        #expect(publicationAttempts == 1)
    }

    @Test("approval-pending Defer can be durably canceled while keeping the lease paused")
    func approvalPendingDeferCanBeCanceled() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_148))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let assessment = Self.deferAssessment(
            session: paused,
            executionRevision: paused.revision,
            moveStart: moveStart,
            digestByte: "c",
            approvalRequired: true,
            expiresAt: Self.baseDate.addingTimeInterval(300)
        )
        let transport = ExecutionTransportDouble(
            snapshots: [],
            pages: [],
            assessmentReplies: [.assessment(assessment)]
        )
        let sync = Self.controller(planner: planner, transport: transport)

        #expect(await sync.deferWork(Self.blockID, moveStart: moveStart) == .approvalRequired)
        let intent = try #require(planner.pendingExecutionDeferIntent)
        #expect(sync.cancelDeferredWork(intent) == .success)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == paused)
        #expect(planner.executionState.pendingCommand == nil)
        let restored = PlannerStore(persistence: context.persistence, now: { Self.baseDate })
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.activeSession == paused)
    }

    @Test("assessment network failure leaves a cancelable durable paused intent")
    func assessmentNetworkFailureCanBeCanceled() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_149))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let transport = ExecutionTransportDouble(
            snapshots: [],
            pages: [],
            assessmentReplies: [.failure(.transport(.networkConnectionLost))]
        )
        let sync = Self.controller(planner: planner, transport: transport)

        #expect(await sync.deferWork(Self.blockID, moveStart: moveStart)
            == .transientNetworkFailure)
        let intent = try #require(planner.pendingExecutionDeferIntent)
        #expect(intent.assessment == nil)
        #expect(sync.cancelDeferredWork(intent) == .success)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == paused)
    }

    @Test("a lost Pause response retains and completes the selected move after relaunch")
    func lostPauseResponseResumesDurableMoveIntent() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_132)
        let active = try Self.activeSession(sessionID: sessionID)
        let pausedAt = Self.baseDate.addingTimeInterval(300)
        let paused = try Self.session(
            id: sessionID, status: .paused, revision: 2, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: pausedAt,
            accumulatedSeconds: 300, actualSeconds: nil, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, endedAt: nil
        )
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let deferredAt = pausedAt.addingTimeInterval(1)
        let deferred = try Self.session(
            id: sessionID, status: .deferred, revision: 3, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: deferredAt,
            accumulatedSeconds: 300, actualSeconds: 300, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500), endedAt: deferredAt
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.presentedBlockIDs = [Self.blockID]
        var activeBlock = Self.block()
        activeBlock.status = .active
        activeBlock.previewKind = "pinned"
        let firstPlanner = Self.planner(
            persistence: context.persistence,
            blocks: [activeBlock], canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let firstTransport = ExecutionTransportDouble(
            snapshots: [activeSnapshot, activeSnapshot],
            pages: [.init(sessions: [active], nextOffset: nil)],
            commandReplies: [.failure(.transport(.networkConnectionLost))]
        )

        #expect(await Self.controller(
            planner: firstPlanner, transport: firstTransport, now: { pausedAt }
        ).deferWork(
            Self.blockID,
            moveStart: moveStart,
            allowSourceOverride: true
        ) == .transientNetworkFailure)
        #expect(firstPlanner.pendingExecutionDeferIntent?.moveStart == moveStart)
        #expect(firstPlanner.pendingExecutionDeferIntent?.sourceOverrideApproved == false)
        guard case .pause = firstPlanner.executionState.pendingCommand?.command else {
            Issue.record("Expected the interrupted exact Pause journal")
            return
        }

        let restored = PlannerStore(
            persistence: context.persistence,
            now: { pausedAt }
        )
        #expect(restored.pendingExecutionDeferIntent?.sourceOverrideApproved == false)
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let deferredSnapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: nil)
        let replayTransport = ExecutionTransportDouble(
            snapshots: [
                pausedSnapshot, pausedSnapshot,
                pausedSnapshot, pausedSnapshot,
                deferredSnapshot, deferredSnapshot,
            ],
            pages: [
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [deferred], nextOffset: nil),
            ],
            commandReplies: [
                .mutation(try Self.mutation(
                    revision: 2, active: paused, changed: paused, replayed: true
                )),
                .mutation(try Self.mutation(
                    revision: 3, active: nil, changed: deferred, replayed: false
                )),
            ]
        )

        let recoveredOutcome = await Self.controller(
            planner: restored, transport: replayTransport, now: { pausedAt }
        ).refresh()
        #expect(recoveredOutcome == .success)
        let replayedCommands = try await replayTransport.receivedCommands().map {
            try DayWeaveExecutionWireCodec.decode($0.body).command
        }
        #expect(replayedCommands.count == 2)
        guard case .pause = replayedCommands.first else {
            Issue.record("Expected exact Pause replay before the saved Defer")
            return
        }
        #expect(replayedCommands.last == .deferWork(
            sessionID: sessionID, moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500), actualSeconds: 300,
            assessmentDigest: "sha256:\(String(repeating: "d", count: 64))",
            approvedAssessmentDigest: nil
        ))
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.terminalOutcomes[sessionID]?.session == deferred)
    }

    @Test("a v6 Defer target preserves microsecond source endpoints across relaunch")
    func deferIntentPreservesMicrosecondSourceEndpoints() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_145)
        let pausedAt = Self.baseDate.addingTimeInterval(300)
        let paused = try Self.session(
            id: sessionID, status: .paused, revision: 2, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: pausedAt,
            accumulatedSeconds: 300, actualSeconds: nil, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, endedAt: nil
        )
        let sourceStart = Date(timeIntervalSince1970: 1_800_000_000.123_456)
        let sourceEnd = sourceStart.addingTimeInterval(1_800)
        let sourceStartMicros = try #require(dayWeavePostgresEpochMicroseconds(sourceStart))
        let sourceEndMicros = try #require(dayWeavePostgresEpochMicroseconds(sourceEnd))
        #expect(sourceStartMicros % 1_000_000 != 0)
        #expect(sourceEndMicros - sourceStartMicros == 1_800_000_000)
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused), focusedBlockID: Self.blockID,
            sourceStart: sourceStart, sourceEnd: sourceEnd,
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: Self.baseDate,
            expiresAt: moveStart
        )
        #expect(intent.hasValidShape)
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        block.start = sourceStart
        block.end = sourceEnd
        let first = Self.planner(
            persistence: context.persistence,
            blocks: [block], canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent, executionState: durable
        )
        first.flushPersistence()

        let restored = PlannerStore(persistence: context.persistence, now: { Self.baseDate })
        let restoredIntent = try #require(restored.pendingExecutionDeferIntent)
        let restoredBlock = try #require(restored.blocks.first(where: { $0.id == Self.blockID }))
        let restoredProofBlock = try #require(
            restored.publishedScheduleProof?.publishedBlocks.first(where: {
                $0.id == Self.blockID
            })
        )
        #expect(restored.persistenceError == nil)
        #expect(restoredIntent.hasValidShape)
        #expect(dayWeavePostgresEpochMicroseconds(restoredIntent.sourceStart) == sourceStartMicros)
        #expect(dayWeavePostgresEpochMicroseconds(restoredIntent.sourceEnd) == sourceEndMicros)
        #expect(dayWeavePostgresEpochMicroseconds(restoredBlock.start) == sourceStartMicros)
        #expect(dayWeavePostgresEpochMicroseconds(restoredBlock.end) == sourceEndMicros)
        #expect(dayWeavePostgresEpochMicroseconds(restoredProofBlock.start) == sourceStartMicros)
        #expect(dayWeavePostgresEpochMicroseconds(restoredProofBlock.end) == sourceEndMicros)

        let deferredAt = pausedAt.addingTimeInterval(1)
        let deferred = try Self.session(
            id: sessionID, status: .deferred, revision: 3, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: deferredAt,
            accumulatedSeconds: 300, actualSeconds: 300, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500), endedAt: deferredAt
        )
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let deferredSnapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [
                pausedSnapshot, pausedSnapshot,
                pausedSnapshot, pausedSnapshot,
                deferredSnapshot, deferredSnapshot,
            ],
            pages: [
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [deferred], nextOffset: nil),
            ],
            commandReplies: [.mutation(try Self.mutation(
                revision: 3, active: nil, changed: deferred, replayed: false
            ))]
        )

        #expect(await Self.controller(
            planner: restored, transport: transport, now: { Self.baseDate }
        ).refresh() == .success)
        let command = try #require((await transport.receivedCommands()).first)
        #expect(try DayWeaveExecutionWireCodec.decode(command.body).command == .deferWork(
            sessionID: sessionID,
            moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500),
            actualSeconds: 300,
            assessmentDigest: "sha256:\(String(repeating: "d", count: 64))",
            approvedAssessmentDigest: nil
        ))
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.terminalOutcomes[sessionID]?.session == deferred)
    }

    @Test("encrypted schema 15 preserves legacy Defer bytes while dropping local approval")
    func schema15DeferApprovalMigratesFailClosed() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(
            sessionID: Self.uuid(1_141),
            accumulatedSeconds: 300
        )
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let legacy = DayWeavePendingExecutionDeferIntent(
            version: 5,
            identity: .init(session: paused),
            focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart.addingTimeInterval(1_790),
            approvedDeadlines: [],
            deadlineConflictApproved: true,
            approvedFixedConflicts: [],
            fixedConflictApproved: true,
            sourceOverrideApproved: true,
            createdAt: Self.baseDate,
            expiresAt: Self.baseDate.addingTimeInterval(1_800)
        )
        #expect(legacy.hasPersistableShape)
        #expect(!legacy.hasValidShape)
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        let legacyBytes = Data(
            """
            {"command":{"actual_seconds":300,"move_end":"\(Self.format(moveStart.addingTimeInterval(1_500)))","move_start":"\(Self.format(moveStart))","session_id":"\(paused.id.uuidString.lowercased())","type":"defer"},"expected_revision":2}
            """.utf8
        )
        let legacyCommand = try DayWeaveExecutionWireCodec.decode(legacyBytes).command
        let legacyKey = "mac-execution-schema15-defer"
        durable.pendingCommand = DayWeavePendingExecutionCommand(
            idempotencyKey: legacyKey,
            bindingIdentifier: Self.binding,
            expectedRevision: paused.revision,
            identity: .init(session: paused),
            command: legacyCommand,
            encodedRequest: legacyBytes,
            priorSession: paused,
            focusedBlockID: Self.blockID,
            canonicalProjectionEligibleAtLeaseStart: false,
            stagedAt: paused.updatedAt
        )
        var block = Self.block()
        block.status = .paused
        block.syncOrigin = .local
        let profile = Self.planner(
            persistence: context.persistence,
            blocks: [],
            executionState: Self.emptyBoundState
        ).scheduleProfile
        let snapshot = PlannerSnapshot(
            schemaVersion: 15,
            savedAt: Self.baseDate,
            destination: nil,
            selectedBlockID: Self.blockID,
            blocks: [block],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "Legacy approved move",
            protectedFreeMinutes: profile.protectedFreeMinutes,
            scheduleProfile: profile,
            freezeHours: 2,
            showCompleted: true,
            canonicalItems: [],
            canonicalTombstoneRevisions: [:],
            completedOccurrenceIDs: [],
            pendingCanonicalMutations: [],
            pendingCanonicalSensitivityMutations: [],
            recurrenceSessionOutcomes: [],
            recurrenceOccurrenceMoves: [],
            pendingExecutionDeferIntent: legacy,
            deferredExecutionPublicationSessionIDs: [],
            pendingPublicationDeferredSessionIDs: [],
            proposalApplicationReceipts: [],
            pendingCanonicalAuthoringMutations: [],
            canonicalTrash: [],
            executionState: durable
        )

        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        let plaintext = try encoder.encode(snapshot)
        let keyData = Data((0..<32).map(UInt8.init))
        let sealed = try AES.GCM.seal(
            plaintext,
            using: SymmetricKey(data: keyData),
            authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8)
        )
        let combined = try #require(sealed.combined)
        let envelope: [String: Any] = [
            "magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT",
            "formatVersion": 1,
            "cipher": "AES.GCM.256",
            "sealedSnapshot": combined.base64EncodedString(),
        ]
        try JSONSerialization.data(withJSONObject: envelope).write(
            to: context.persistence.fileURL,
            options: .atomic
        )

        let restored = PlannerStore(
            persistence: context.persistence,
            now: { moveStart.addingTimeInterval(1) }
        )
        let intent = try #require(restored.pendingExecutionDeferIntent)
        #expect(restored.persistenceError == nil)
        #expect(intent.version == DayWeavePendingExecutionDeferIntent.currentVersion)
        #expect(intent.moveStart == moveStart)
        #expect(intent.expiresAt == moveStart)
        #expect(intent.assessment == nil)
        #expect(intent.approvedAssessmentDigest == nil)
        #expect(intent.approvedMoveEnd == moveStart)
        #expect(intent.approvedDeadlines.isEmpty)
        #expect(!intent.deadlineConflictApproved)
        #expect(intent.approvedFixedConflicts.isEmpty)
        #expect(!intent.fixedConflictApproved)
        #expect(!intent.sourceOverrideApproved)
        #expect(intent.hasValidShape)
        let migratedPending = try #require(restored.executionState.pendingCommand)
        #expect(migratedPending.idempotencyKey == legacyKey)
        #expect(migratedPending.encodedRequest == legacyBytes)

        let deferredAt = paused.updatedAt.addingTimeInterval(1)
        let deferred = try Self.session(
            id: paused.id,
            status: .deferred,
            revision: paused.revision + 1,
            sessionIndex: paused.sessionIndex,
            plannedBlockID: Self.blockID,
            startedAt: paused.startedAt,
            updatedAt: deferredAt,
            accumulatedSeconds: paused.accumulatedSeconds,
            actualSeconds: 300,
            runningSince: nil,
            pausedAt: paused.pausedAt,
            pauseUntil: nil,
            moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500),
            endedAt: deferredAt
        )
        let deferredSnapshot = DayWeaveExecutionSnapshot(
            revision: deferred.revision,
            activeSession: nil
        )
        let transport = ExecutionTransportDouble(
            snapshots: [deferredSnapshot, deferredSnapshot],
            pages: [.init(sessions: [deferred], nextOffset: nil)],
            commandReplies: [.mutation(try Self.mutation(
                revision: deferred.revision,
                active: nil,
                changed: deferred,
                replayed: true
            ))]
        )

        #expect(await Self.controller(
            planner: restored,
            transport: transport,
            now: { moveStart.addingTimeInterval(1) }
        ).refresh() == .success)
        let replayed = try #require((await transport.receivedCommands()).first)
        #expect(replayed.body == legacyBytes)
        #expect(replayed.key == legacyKey)
        #expect(restored.executionState.pendingCommand == nil)
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.terminalOutcomes[paused.id]?.session == deferred)
    }

    @Test("a newer paused revision retains the target but cannot inherit an approved digest")
    func newerPausedRevisionReassessesWithoutInheritedApproval() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_142))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let priorAssessment = Self.deferAssessment(
            session: paused,
            executionRevision: paused.revision,
            moveStart: moveStart,
            digestByte: "a",
            approvalRequired: true,
            expiresAt: Self.baseDate.addingTimeInterval(300)
        )
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused),
            focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            assessment: priorAssessment,
            approvedAssessmentDigest: priorAssessment.assessmentDigest,
            createdAt: Self.baseDate,
            expiresAt: moveStart
        )
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent,
            executionState: durable
        )
        let updatedAt = Self.baseDate.addingTimeInterval(20)
        let newer = try Self.session(
            id: paused.id,
            status: .paused,
            revision: 4,
            sessionIndex: paused.sessionIndex,
            plannedBlockID: Self.blockID,
            startedAt: paused.startedAt,
            updatedAt: updatedAt,
            accumulatedSeconds: 20,
            actualSeconds: nil,
            runningSince: nil,
            pausedAt: updatedAt,
            pauseUntil: nil,
            endedAt: nil
        )
        let freshAssessment = Self.deferAssessment(
            session: newer,
            executionRevision: 4,
            moveStart: moveStart,
            digestByte: "b",
            approvalRequired: true,
            expiresAt: Self.baseDate.addingTimeInterval(300)
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 4, activeSession: newer)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [newer], nextOffset: nil)],
            assessmentReplies: [.assessment(freshAssessment)]
        )

        #expect(await Self.controller(
            planner: planner,
            transport: transport
        ).refresh() == .approvalRequired)
        let reassessed = try #require(planner.pendingExecutionDeferIntent)
        #expect(reassessed.moveStart == moveStart)
        #expect(reassessed.assessment == freshAssessment)
        #expect(reassessed.approvedAssessmentDigest == nil)
        #expect(planner.executionState.activeSession == newer)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("a same-ID source-window change cannot resume a saved Defer")
    func changedSourceWindowAfterRelaunchFailsClosed() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_144))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused),
            focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: Self.baseDate,
            expiresAt: moveStart
        )
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var shifted = Self.block()
        shifted.status = .paused
        shifted.start.addTimeInterval(300)
        shifted.end.addTimeInterval(300)
        let first = Self.planner(
            persistence: context.persistence,
            blocks: [shifted],
            canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent,
            executionState: durable
        )
        first.flushPersistence()
        let restored = PlannerStore(persistence: context.persistence, now: { Self.baseDate })
        let snapshot = DayWeaveExecutionSnapshot(
            revision: paused.revision,
            activeSession: paused
        )
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [paused], nextOffset: nil)]
        )

        #expect(await Self.controller(
            planner: restored,
            transport: transport
        ).refresh() == .conflict)
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.activeSession == paused)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("a superseding terminal state never counts as recovered Defer")
    func supersedingTerminalFailsRecoveredDeferClosed() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_122)
        let pausedAt = Self.baseDate.addingTimeInterval(300)
        let paused = try Self.session(
            id: sessionID, status: .paused, revision: 2, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: pausedAt,
            accumulatedSeconds: 300, actualSeconds: nil, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, endedAt: nil
        )
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let legacyBytes = Data(
            """
            {"command":{"actual_seconds":300,"move_end":"\(Self.format(moveStart.addingTimeInterval(1_500)))","move_start":"\(Self.format(moveStart))","session_id":"\(sessionID.uuidString.lowercased())","type":"defer"},"expected_revision":2}
            """.utf8
        )
        let command = try DayWeaveExecutionWireCodec.decode(legacyBytes).command
        let pending = DayWeavePendingExecutionCommand(
            idempotencyKey: "mac-execution-superseded-defer",
            bindingIdentifier: Self.binding,
            expectedRevision: 2,
            identity: .init(session: paused),
            command: command,
            encodedRequest: legacyBytes,
            priorSession: paused,
            focusedBlockID: Self.blockID,
            canonicalProjectionEligibleAtLeaseStart: false,
            stagedAt: pausedAt
        )
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused), focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: pausedAt,
            expiresAt: moveStart
        )
        let completedAt = pausedAt.addingTimeInterval(2)
        let completed = try Self.session(
            id: sessionID, status: .completed, revision: 3, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: completedAt,
            accumulatedSeconds: 300, actualSeconds: 300, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, endedAt: completedAt
        )
        var durable = Self.emptyBoundState
        durable.revision = 2
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = 2
        durable.pendingCommand = pending
        durable.presentedBlockIDs = [Self.blockID]
        var pausedBlock = Self.block()
        pausedBlock.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [pausedBlock],
            canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent,
            executionState: durable
        )
        planner.flushPersistence()
        let snapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot, snapshot, snapshot],
            pages: [
                .init(sessions: [completed], nextOffset: nil),
                .init(sessions: [completed], nextOffset: nil),
            ],
            commandReplies: [.failure(.server(
                statusCode: 409, code: "superseded",
                message: "the lease closed differently", requestID: nil
            ))]
        )

        #expect(await Self.controller(
            planner: planner, transport: transport, now: { pausedAt }
        ).refresh() == .conflict)
        #expect(planner.executionState.pendingCommand == nil)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.terminalOutcomes[sessionID]?.session == completed)
        #expect(planner.deferredExecutionPublicationSessionIDs.isEmpty)
    }

    @Test("an expired assessment after 24 hours preserves the target and reassesses")
    func expiredAssessmentPreservesLongLivedTarget() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_131)
        let pausedAt = Self.baseDate.addingTimeInterval(20)
        let paused = try Self.session(
            id: sessionID, status: .paused, revision: 2, sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate, updatedAt: pausedAt,
            accumulatedSeconds: 20, actualSeconds: nil, runningSince: nil,
            pausedAt: pausedAt, pauseUntil: nil, endedAt: nil
        )
        let moveStart = Self.baseDate.addingTimeInterval(48 * 60 * 60)
        let expiredAssessment = Self.deferAssessment(
            session: paused,
            executionRevision: paused.revision,
            moveStart: moveStart,
            digestByte: "a",
            approvalRequired: true,
            expiresAt: Self.baseDate.addingTimeInterval(5 * 60)
        )
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused), focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            assessment: expiredAssessment,
            approvedAssessmentDigest: expiredAssessment.assessmentDigest,
            createdAt: Self.baseDate,
            expiresAt: moveStart
        )
        #expect(intent.hasValidShape)
        var durable = Self.emptyBoundState
        durable.revision = 2
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = 2
        durable.presentedBlockIDs = [Self.blockID]
        var pausedBlock = Self.block()
        pausedBlock.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [pausedBlock], canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent, executionState: durable
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let observedAt = Self.baseDate.addingTimeInterval(25 * 60 * 60)
        let freshAssessment = Self.deferAssessment(
            session: paused,
            executionRevision: paused.revision,
            moveStart: moveStart,
            digestByte: "b",
            approvalRequired: true,
            expiresAt: observedAt.addingTimeInterval(5 * 60)
        )
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [paused], nextOffset: nil)],
            assessmentReplies: [.assessment(freshAssessment)]
        )

        #expect(await Self.controller(
            planner: planner,
            transport: transport,
            now: { observedAt }
        ).refresh() == .approvalRequired)
        #expect(planner.pendingExecutionDeferIntent?.moveStart == moveStart)
        #expect(planner.pendingExecutionDeferIntent?.expiresAt == moveStart)
        #expect(planner.pendingExecutionDeferIntent?.assessment == freshAssessment)
        #expect(planner.pendingExecutionDeferIntent?.approvedAssessmentDigest == nil)
        #expect(planner.executionState.activeSession == paused)
        #expect(planner.blocks.first?.status == .paused)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("an assessment-free restored target inside the safety margin clears before Pause")
    func restoredTargetInsideSafetyMarginClearsBeforePause() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_150))
        let moveStart = Self.baseDate.addingTimeInterval(10 * 60)
        let intent = Self.deferIntent(session: paused, moveStart: moveStart)
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent,
            executionState: durable
        )
        let transport = Self.emptyReadTransport()

        #expect(await Self.controller(
            planner: planner,
            transport: transport,
            now: { Self.baseDate.addingTimeInterval(60) }
        ).deferWork(Self.blockID, moveStart: moveStart) == .invalidLocalState)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == paused)
        #expect(await transport.receivedCommands().isEmpty)
        #expect(await transport.receivedAssessmentRequests().isEmpty)
    }

    @Test("fresh bypass evidence expiring before staging clears without reassessment")
    func freshAssessmentExpiryDuringDeferClearsTarget() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_151))
        let moveStart = Self.baseDate.addingTimeInterval(10 * 60)
        let assessment = Self.deferAssessment(
            session: paused,
            executionRevision: paused.revision,
            moveStart: moveStart,
            digestByte: "f",
            approvalRequired: false,
            expiresAt: Self.baseDate.addingTimeInterval(61)
        )
        let intent = Self.deferIntent(
            session: paused,
            moveStart: moveStart,
            assessment: assessment
        )
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: intent,
            executionState: durable
        )
        let transport = Self.emptyReadTransport()
        let clock = ExecutionSequenceClock([
            Self.baseDate.addingTimeInterval(60),
            Self.baseDate.addingTimeInterval(62),
        ])

        #expect(await Self.controller(
            planner: planner,
            transport: transport,
            now: { clock.now() }
        ).deferWork(Self.blockID, moveStart: moveStart) == .invalidLocalState)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == paused)
        #expect(await transport.receivedCommands().isEmpty)
        #expect(await transport.receivedAssessmentRequests().isEmpty)
    }

    @Test("Defer preflight rejects a non-whole-second source duration before pausing")
    func deferPreflightRejectsUnsupportedDuration() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let active = try Self.activeSession(sessionID: Self.uuid(1_123))
        var block = Self.block()
        block.status = .active
        block.end.addTimeInterval(0.000_001)
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.presentedBlockIDs = [Self.blockID]
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block], canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let transport = Self.emptyReadTransport()
        let moveStart = Self.baseDate.addingTimeInterval(3_600)

        #expect(await Self.controller(planner: planner, transport: transport).deferWork(
            Self.blockID,
            moveStart: moveStart
        ) == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(dayWeavePostgresEpochMicroseconds(
            Date(timeIntervalSince1970: Double.greatestFiniteMagnitude)
        ) == nil)
        #expect(dayWeavePostgresEpochMicroseconds(
            Date(timeIntervalSince1970: Double(Int64.max) / 1_000_000)
        ) == nil)
    }

    @Test("new Defer targets require the assessment TTL plus one aligned safety slot")
    func deferTargetSafetyBoundary() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let active = try Self.activeSession(sessionID: Self.uuid(1_147))
        var durable = Self.emptyBoundState
        durable.revision = active.revision
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = active.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .active
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(
            revision: active.revision,
            activeSession: active
        )
        let transport = ExecutionTransportDouble(
            snapshots: [activeSnapshot, activeSnapshot],
            pages: [.init(sessions: [active], nextOffset: nil)],
            commandReplies: [.failure(.transport(.networkConnectionLost))]
        )
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { Self.baseDate }
        )

        #expect(await sync.deferWork(
            Self.blockID,
            moveStart: Self.baseDate.addingTimeInterval(5 * 60)
        ) == .invalidLocalState)
        #expect(await sync.deferWork(
            Self.blockID,
            moveStart: Self.baseDate.addingTimeInterval(10 * 60 + 1)
        ) == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)

        let exactMinimum = Self.baseDate.addingTimeInterval(10 * 60)
        #expect(await sync.deferWork(
            Self.blockID,
            moveStart: exactMinimum
        ) == .transientNetworkFailure)
        #expect(planner.pendingExecutionDeferIntent?.moveStart == exactMinimum)
        #expect(planner.pendingExecutionDeferIntent?.expiresAt == exactMinimum)
        #expect((await transport.receivedCommands()).count == 1)
    }

    @Test("a boundary target losing its margin during Pause never requests assessment")
    func deferBoundaryClockAdvanceDuringPauseCancelsIntent() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_152)
        let active = try Self.activeSession(sessionID: sessionID)
        let pausedAt = Self.baseDate.addingTimeInterval(1)
        let paused = try Self.session(
            id: sessionID,
            status: .paused,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: pausedAt,
            accumulatedSeconds: 1,
            actualSeconds: nil,
            runningSince: nil,
            pausedAt: pausedAt,
            pauseUntil: nil,
            endedAt: nil
        )
        var durable = Self.emptyBoundState
        durable.revision = active.revision
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = active.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .active
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let clock = ExecutionSequenceClock([Self.baseDate])
        let transport = ExecutionTransportDouble(
            snapshots: [
                activeSnapshot, activeSnapshot,
                pausedSnapshot, pausedSnapshot,
            ],
            pages: [
                .init(sessions: [active], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
            ],
            commandReplies: [.mutation(try Self.mutation(
                revision: 2,
                active: paused,
                changed: paused,
                replayed: false
            ))],
            onCommandReceived: {
                clock.advance(to: Self.baseDate.addingTimeInterval(1))
            }
        )
        let moveStart = Self.baseDate.addingTimeInterval(10 * 60)

        #expect(await Self.controller(
            planner: planner,
            transport: transport,
            now: { clock.now() }
        ).deferWork(Self.blockID, moveStart: moveStart) == .invalidLocalState)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == paused)
        #expect(planner.executionState.pendingCommand == nil)
        #expect((await transport.receivedCommands()).count == 1)
        #expect(await transport.receivedAssessmentRequests().isEmpty)
    }

    @Test("authoritative active work defers despite stale pinned recurrence policy")
    func deferAuthoritativePinnedOccurrenceDespiteLocalPolicy() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_126)
        let active = try Self.session(
            id: sessionID,
            status: .active,
            revision: 1,
            sessionIndex: 0,
            occurrenceID: Self.occurrenceID,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: Self.baseDate,
            accumulatedSeconds: 0,
            actualSeconds: nil,
            runningSince: Self.baseDate,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: nil
        )
        var block = Self.block(occurrenceID: Self.occurrenceID)
        block.status = .active
        block.isFlexible = false
        block.isHardConstraint = true
        block.previewKind = "pinned"
        block.recurrenceMoveSource = RecurrenceMoveSource(
            itemRevision: 1,
            identity: .custom,
            nominalStart: "2027-01-15T08:00:00Z",
            nominalEnd: "2027-01-15T08:30:00Z",
            localDate: nil,
            ordinal: 0
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.presentedBlockIDs = [Self.blockID]
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let transport = ExecutionTransportDouble(
            snapshots: [activeSnapshot, activeSnapshot],
            pages: [.init(sessions: [active], nextOffset: nil)],
            commandReplies: [.failure(.transport(.networkConnectionLost))]
        )

        #expect(await Self.controller(planner: planner, transport: transport).deferWork(
            Self.blockID,
            moveStart: Self.baseDate.addingTimeInterval(3_600)
        ) == .transientNetworkFailure)
        #expect((await transport.receivedCommands()).count == 1)
        #expect(planner.pendingExecutionDeferIntent?.moveStart
            == Self.baseDate.addingTimeInterval(3_600))
        #expect(planner.executionState.activeSession == active)
    }

    @Test("foreground remote Defer requests canonical publication again after a transient failure")
    func remoteDeferPollingRetriesCanonicalPublication() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(1_125), sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: deferred.revision, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot, snapshot, snapshot],
            pages: [
                .init(sessions: [deferred], nextOffset: nil),
                .init(sessions: [deferred], nextOffset: nil),
            ]
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()], canonicalItems: [try Self.canonicalItem()]
        )
        let sync = Self.controller(planner: planner, transport: transport)
        var attempts = 0
        sync.installDeferredPublicationCoordinator {
            attempts += 1
            return attempts > 1
        }

        #expect(await sync.refreshAndCoordinateDeferredPublication() == .success)
        #expect(planner.deferredExecutionPublicationSessionIDs == [deferred.id])
        #expect(planner.publishedScheduleProof == nil)
        #expect(attempts == 1)
        #expect(await sync.refreshAndCoordinateDeferredPublication() == .success)
        #expect(attempts == 2)
    }

    @Test("matching source proof clears causally even when its publication clock is later")
    func deferredSourceProofClearsWithoutWallClockOrdering() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(1_126), sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()], canonicalItems: [try Self.canonicalItem()],
            proofPublishedAt: deferred.endedAt!.addingTimeInterval(3_600)
        )

        try planner.persistExecutionState(Self.terminalState(deferred))

        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.deferredExecutionPublicationSessionIDs == [deferred.id])
    }

    @Test("a pre-existing exact-window pinned sibling cannot discharge a later Defer")
    func preexistingExactWindowSiblingDoesNotDischargeDefer() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(1_127), sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate
        )
        let replacementID = Self.uuid(1_128)
        let moveStart = try #require(deferred.moveStart)
        let moveEnd = try #require(deferred.moveEnd)
        var replacement = Self.block(id: replacementID, sessionIndex: 1)
        replacement.start = moveStart
        replacement.end = moveEnd
        replacement.previewKind = "pinned"
        let replacementProof = try #require(
            DayWeavePublishedScheduleBlockProof(block: replacement)
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [replacement], canonicalItems: [try Self.canonicalItem(splittable: true)],
            proofPublishedAt: deferred.endedAt!.addingTimeInterval(-3_600),
            proofBlocks: [replacementProof]
        )

        try planner.persistExecutionState(Self.terminalState(deferred))

        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.deferredExecutionPublicationSessionIDs == [deferred.id])
    }

    @Test("only a publication journal that captured the Defer can discharge its fence")
    func capturedFreshPublicationDischargesDeferredFence() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(1_133), sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate
        )
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [Self.block()], canonicalItems: [try Self.canonicalItem(splittable: true)]
        )
        try planner.persistExecutionState(Self.terminalState(deferred))
        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.deferredExecutionPublicationSessionIDs == [deferred.id])

        let fixture = try Self.replacementPublication(after: deferred)
        try planner.persistPendingSchedulePublication(fixture.publication)
        try planner.commitPendingSchedulePublication(
            fixture.publication,
            blocks: [fixture.block],
            response: fixture.response
        )

        #expect(planner.pendingSchedulePublication == nil)
        #expect(planner.deferredExecutionPublicationSessionIDs.isEmpty)
        #expect(planner.publishedScheduleProof?.matches(fixture.block) == true)
        let restored = PlannerStore.live(persistence: context.persistence)
        #expect(restored.deferredExecutionPublicationSessionIDs.isEmpty)
        #expect(restored.publishedScheduleProof == planner.publishedScheduleProof)
    }

    @Test("a higher split sibling is not mistaken for the deferred replacement claim")
    func splitSiblingDoesNotClearDeferredPublicationFence() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let deferred = try Self.deferredSession(
            sessionID: Self.uuid(1_129), sessionIndex: 0,
            plannedBlockID: Self.blockID, startedAt: Self.baseDate
        )
        let siblingID = Self.uuid(1_130)
        var sibling = Self.block(id: siblingID, sessionIndex: 1)
        sibling.start = Self.baseDate.addingTimeInterval(1_800)
        sibling.end = Self.baseDate.addingTimeInterval(2_700)
        sibling.previewKind = "pinned"
        let source = Self.block()
        let sourceProof = try #require(DayWeavePublishedScheduleBlockProof(block: source))
        let siblingProof = try #require(DayWeavePublishedScheduleBlockProof(block: sibling))
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [source, sibling], canonicalItems: [try Self.canonicalItem(splittable: true)],
            proofBlocks: [sourceProof, siblingProof]
        )

        try planner.persistExecutionState(Self.terminalState(deferred))

        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.deferredExecutionPublicationSessionIDs == [deferred.id])
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
            timezoneName: firstLaunch.scheduleProfile.timezoneName,
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
        let notificationCoordinator = GatedBreakNotificationCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { paused.pauseUntil!.addingTimeInterval(1) },
            breakNotificationCoordinator: notificationCoordinator
        )

        #expect(sync.expiredBreakChoiceRequired)
        let completion = AsyncCompletionProbe()
        let keepPaused = Task { @MainActor in
            let outcome = await sync.keepPausedAfterExpiredBreak()
            await completion.markComplete()
            return outcome
        }
        await notificationCoordinator.waitUntilEntered()
        #expect(!(await completion.isComplete))
        // Notification removal is proved before the encrypted acknowledgment,
        // so a crash at this barrier still restores the explicit resolver.
        #expect(sync.expiredBreakChoiceRequired)
        #expect(planner.executionState.activeSession?.status == .paused)
        await notificationCoordinator.release()
        #expect(await keepPaused.value == .success)
    }

    @Test("a notification tap only presents its exact expired lease without mutating it")
    func breakNotificationTapIsExactAndPresentationOnly() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(81))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { paused.pauseUntil!.addingTimeInterval(1) }
        )
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let before = planner.executionState

        #expect(sync.routeBreakNotificationTap(identifier: identifier))
        #expect(sync.breakResolutionPresentation == .init(
            notificationIdentifier: identifier,
            observedSessionVersion: .init(
                sessionID: paused.id,
                revision: paused.revision
            ),
            observedBreakIdentifier: identifier
        ))
        #expect(sync.expiredBreakResolutionShouldBePresented)
        #expect(planner.executionState == before)

        #expect(await sync.keepPausedAfterExpiredBreak() == .success)
        #expect(sync.breakResolutionPresentation == nil)
        let acknowledged = planner.executionState
        #expect(!sync.routeBreakNotificationTap(identifier: identifier))
        #expect(!sync.expiredBreakResolutionShouldBePresented)
        #expect(planner.executionState == acknowledged)
    }

    @Test("a stale notification tap cannot present a newer expired break")
    func staleBreakNotificationDoesNotRetargetPresentation() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let stale = try Self.pausedSession(sessionID: Self.uuid(88))
        let current = try Self.pausedSession(sessionID: Self.uuid(89))
        var state = Self.emptyBoundState
        state.revision = current.revision
        state.activeSession = current
        state.historyWindow = [current]
        state.historyWindowRevision = current.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { current.pauseUntil!.addingTimeInterval(1) }
        )
        let staleIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: stale)?.identifier
        )
        let currentIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: current)?.identifier
        )

        // With no notification response, the ordinary in-app expiration path
        // still presents the current break.
        #expect(sync.expiredBreakResolutionShouldBePresented)
        let router = DayWeaveBreakNotificationTapRouter()
        #expect(router.route(identifier: staleIdentifier))
        #expect(!sync.shouldPresentExpiredBreakResolution(
            pendingNotificationIdentifier: router.pendingIdentifier
        ))
        // Closed-window and app-lock rendering both see the pending mailbox and
        // stay suppressed until content is available for exact routing.
        #expect(router.deliverPending(
            contentAvailable: false,
            route: sync.routeBreakNotificationTap(identifier:)
        ) == nil)
        #expect(router.pendingIdentifier == staleIdentifier)
        #expect(!sync.shouldPresentExpiredBreakResolution(
            pendingNotificationIdentifier: router.pendingIdentifier
        ))

        // Delivery installs the rejected exact token synchronously before the
        // router clears its mailbox, so B never sees an unguarded render.
        #expect(router.deliverPending(
            contentAvailable: true,
            route: sync.routeBreakNotificationTap(identifier:)
        ) == false)
        #expect(router.pendingIdentifier == nil)
        #expect(!sync.expiredBreakResolutionShouldBePresented)
        #expect(sync.breakResolutionPresentation == .init(
            notificationIdentifier: staleIdentifier,
            observedSessionVersion: .init(
                sessionID: current.id,
                revision: current.revision
            ),
            observedBreakIdentifier: currentIdentifier
        ))
        #expect(sync.breakNotificationTapIssue == .staleReminder)

        // Recovery requires a separate, explicit acknowledgement. The stale A
        // click itself never opens B, while the button restores the ordinary
        // current-break resolver without waiting for another notification.
        sync.acknowledgeStaleBreakNotificationTap()
        #expect(sync.breakNotificationTapIssue == nil)
        #expect(sync.breakResolutionPresentation == nil)
        #expect(sync.expiredBreakResolutionShouldBePresented)

        #expect(router.route(identifier: currentIdentifier))
        #expect(!sync.shouldPresentExpiredBreakResolution(
            pendingNotificationIdentifier: router.pendingIdentifier
        ))
        #expect(router.deliverPending(
            contentAvailable: true,
            route: sync.routeBreakNotificationTap(identifier:)
        ) == true)
        #expect(sync.expiredBreakResolutionShouldBePresented)
    }

    @Test("an offline deadline task publishes an exact local resolver wake")
    func localBreakDeadlineWakeDoesNotDependOnPolling() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(94))
        let clock = ExecutionSequenceClock([paused.pausedAt!])
        let sleeper = BreakDeadlineSleepGate()
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { clock.now() },
            breakDeadlineSleep: { duration in
                await sleeper.sleep(for: duration)
            }
        )

        let delay = await sleeper.waitUntilEntered()
        #expect(delay == .seconds(60))
        #expect(sync.breakResolutionWakeGeneration == 0)
        #expect(!sync.expiredBreakChoiceRequired)

        clock.advance(to: paused.pauseUntil!.addingTimeInterval(1))
        await sleeper.release()
        for _ in 0..<20 where sync.breakResolutionWakeGeneration == 0 {
            await Task.yield()
        }
        #expect(sync.breakResolutionWakeGeneration == 1)
        #expect(sync.expiredBreakChoiceRequired)
        #expect(sync.expiredBreakResolutionShouldBePresented)
    }

    @Test("a rejected tap cannot suppress a later clock-driven expiration")
    func staleTapSuppressionExpiresWithObservedDigest() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let stale = try Self.pausedSession(sessionID: Self.uuid(92))
        let current = try Self.pausedSession(sessionID: Self.uuid(93))
        let clock = ExecutionSequenceClock([current.pausedAt!])
        var state = Self.emptyBoundState
        state.revision = current.revision
        state.activeSession = current
        state.historyWindow = [current]
        state.historyWindowRevision = current.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { clock.now() }
        )
        let staleIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: stale)?.identifier
        )

        #expect(!sync.routeBreakNotificationTap(identifier: staleIdentifier))
        #expect(!sync.expiredBreakChoiceRequired)
        #expect(sync.breakResolutionPresentation?.observedBreakIdentifier == nil)

        clock.advance(to: current.pauseUntil!.addingTimeInterval(1))
        #expect(sync.expiredBreakChoiceRequired)
        #expect(sync.expiredBreakResolutionShouldBePresented)
    }

    @Test("an authorized add is durable before pause completion makes termination safe")
    func breakNotificationSchedulingIsAnAwaitedPauseBarrier() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(82)
        let active = try Self.activeSession(sessionID: sessionID)
        let pausedAt = Self.baseDate.addingTimeInterval(60)
        let paused = try Self.session(
            id: sessionID,
            status: .paused,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: pausedAt,
            accumulatedSeconds: 60,
            actualSeconds: nil,
            runningSince: nil,
            pausedAt: pausedAt,
            pauseUntil: pausedAt.addingTimeInterval(600),
            endedAt: nil
        )
        let activeSnapshot = DayWeaveExecutionSnapshot(revision: 1, activeSession: active)
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let transport = ExecutionTransportDouble(
            snapshots: [activeSnapshot, activeSnapshot, pausedSnapshot, pausedSnapshot],
            pages: [
                .init(sessions: [active], nextOffset: nil),
                .init(sessions: [paused], nextOffset: nil),
            ],
            commandReplies: [.mutation(try Self.mutation(
                revision: 2,
                active: paused,
                changed: paused,
                replayed: false
            ))]
        )
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.historyContinuityEstablished = true
        durable.historyVerified = true
        durable.leaseProjectionEligibility[sessionID] = true
        durable.presentedBlockIDs = [Self.blockID]
        var activeBlock = Self.block()
        activeBlock.status = .active
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [activeBlock],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let notificationCoordinator = GatedBreakNotificationCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { pausedAt },
            breakNotificationCoordinator: notificationCoordinator
        )

        let completion = AsyncCompletionProbe()
        let pause = Task { @MainActor in
            let outcome = await sync.pause(Self.blockID, durationSeconds: 600)
            await completion.markComplete()
            return outcome
        }
        await notificationCoordinator.waitUntilEntered()
        #expect(!(await completion.isComplete))
        #expect(planner.executionState.activeSession == paused)
        await notificationCoordinator.release()
        #expect(await pause.value == .success)
        #expect(await completion.isComplete)
    }

    @Test("resume awaits cancellation of the prior timed-break notification")
    func resumeAwaitsBreakNotificationCancellation() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(86)
        let paused = try Self.pausedSession(sessionID: sessionID)
        let resumedAt = paused.updatedAt.addingTimeInterval(30)
        let resumed = try Self.session(
            id: sessionID,
            status: .active,
            revision: 3,
            sessionIndex: 0,
            plannedBlockID: Self.blockID,
            startedAt: Self.baseDate,
            updatedAt: resumedAt,
            accumulatedSeconds: paused.accumulatedSeconds,
            actualSeconds: nil,
            runningSince: resumedAt,
            pausedAt: nil,
            pauseUntil: nil,
            endedAt: nil
        )
        let pausedSnapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: paused)
        let resumedSnapshot = DayWeaveExecutionSnapshot(revision: 3, activeSession: resumed)
        let transport = ExecutionTransportDouble(
            snapshots: [pausedSnapshot, pausedSnapshot, resumedSnapshot, resumedSnapshot],
            pages: [
                .init(sessions: [paused], nextOffset: nil),
                .init(sessions: [resumed], nextOffset: nil),
            ],
            commandReplies: [.mutation(try Self.mutation(
                revision: 3,
                active: resumed,
                changed: resumed,
                replayed: false
            ))]
        )
        var state = Self.emptyBoundState
        state.revision = 2
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = 2
        state.historyContinuityEstablished = true
        state.historyVerified = true
        state.leaseProjectionEligibility[sessionID] = true
        state.presentedBlockIDs = [Self.blockID]
        var pausedBlock = Self.block()
        pausedBlock.status = .paused
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [pausedBlock],
            canonicalItems: [try Self.canonicalItem()],
            executionState: state
        )
        let notifications = GatedBreakNotificationCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { resumedAt },
            breakNotificationCoordinator: notifications
        )
        let completion = AsyncCompletionProbe()

        let resume = Task { @MainActor in
            let outcome = await sync.resume(Self.blockID)
            await completion.markComplete()
            return outcome
        }
        await notifications.waitUntilEntered()
        #expect(planner.executionState.activeSession == resumed)
        #expect(!(await completion.isComplete))
        await notifications.release()
        #expect(await resume.value == .success)
    }

    @Test("an explicit permission prompt is separate from the authoritative pause lane")
    func explicitBreakNotificationPermissionDoesNotHoldExecutionUI() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(83))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let prompt = GatedBreakNotificationPermissionCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { paused.pausedAt! },
            breakNotificationCoordinator: prompt
        )

        let request = Task { @MainActor in
            await sync.requestBreakNotificationAuthorization()
        }
        await prompt.waitUntilAuthorizationRequestEntered()
        #expect(!sync.isSyncing)
        #expect(planner.executionState.activeSession == paused)
        #expect(sync.isRequestingBreakNotificationAuthorization)
        await prompt.releaseAuthorizationRequest()
        #expect(await request.value == .scheduled)
        #expect(!sync.isRequestingBreakNotificationAuthorization)
    }

    @Test("notification service failures remain visible and can be retried")
    func breakNotificationServiceFailureHasRetryState() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(90))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let notifications = RecoveringBreakNotificationCoordinator(mode: .schedulingUnavailable)
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { paused.pausedAt! },
            breakNotificationCoordinator: notifications
        )

        #expect(await sync.reconcileBreakNotification() == .unavailable)
        #expect(sync.breakNotificationAuthorizationState == .authorized)
        #expect(sync.breakNotificationIssue == .schedulingUnavailable)
        #expect(sync.breakNotificationIssue?.message.isEmpty == false)

        await notifications.setMode(.available)
        #expect(await sync.retryBreakNotification() == .scheduled)
        #expect(sync.breakNotificationIssue == nil)
    }

    @Test("authorization-service failure is visible until explicit retry succeeds")
    func breakNotificationAuthorizationFailureHasRetryState() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(91))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let notifications = RecoveringBreakNotificationCoordinator(
            mode: .authorizationUnavailable
        )
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { paused.pausedAt! },
            breakNotificationCoordinator: notifications
        )

        #expect(await sync.requestBreakNotificationAuthorization() == .unavailable)
        #expect(sync.breakNotificationAuthorizationState == .notDetermined)
        #expect(sync.breakNotificationIssue == .authorizationUnavailable)

        await notifications.setMode(.available)
        #expect(await sync.retryBreakNotification() == .scheduled)
        #expect(sync.breakNotificationAuthorizationState == .authorized)
        #expect(sync.breakNotificationIssue == nil)
    }

    @Test("removal convergence failure is not mislabeled as scheduling failure")
    func breakNotificationRemovalFailureHasExactRetryState() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(96))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let notifications = RecoveringBreakNotificationCoordinator(
            mode: .cancellationUnavailable
        )
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            now: { paused.pausedAt! },
            breakNotificationCoordinator: notifications
        )

        #expect(await sync.reconcileBreakNotification() == .cancellationUnavailable)
        #expect(sync.breakNotificationIssue == .cancellationUnavailable)
        #expect(sync.breakNotificationIssue?.message.contains("removal") == true)

        await notifications.setMode(.available)
        #expect(await sync.retryBreakNotification() == .scheduled)
        #expect(sync.breakNotificationIssue == nil)
    }

    @Test("denied alert permission cannot bypass a stale-removal retry")
    func cancellationRetryIgnoresDeniedAuthorization() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let planner = Self.planner(persistence: context.persistence)
        let notifications = DeniedCancellationRetryCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            breakNotificationCoordinator: notifications
        )

        #expect(await sync.reconcileBreakNotification() == .cancellationUnavailable)
        #expect(sync.breakNotificationAuthorizationState == .denied)
        #expect(sync.breakNotificationIssue == .cancellationUnavailable)

        #expect(await sync.retryBreakNotification() == .canceled)
        #expect(await notifications.reconcileAttempts == 2)
        #expect(sync.breakNotificationIssue == nil)
    }

    @Test("a cancellation issue stays visible without a future timed break")
    func cancellationIssueBannerDoesNotRequireFutureBreak() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let planner = Self.planner(persistence: context.persistence)
        let notifications = RecoveringBreakNotificationCoordinator(
            mode: .cancellationUnavailable
        )
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            breakNotificationCoordinator: notifications
        )

        #expect(!sync.hasFutureTimedBreakForNotificationPermission)
        #expect(!sync.breakNotificationBannerShouldBePresented)

        #expect(await sync.reconcileBreakNotification() == .cancellationUnavailable)
        #expect(sync.breakNotificationIssue == .cancellationUnavailable)
        #expect(sync.breakNotificationBannerShouldBePresented)
    }

    @Test("credential replacement awaits removal of the old notification generation")
    func credentialReplacementAwaitsBreakNotificationCancellation() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(84))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let notifications = GatedBreakNotificationCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            breakNotificationCoordinator: notifications
        )
        let completion = AsyncCompletionProbe()

        let replacement = Task { @MainActor in
            try await sync.prepareForCredentialReplacement()
            await completion.markComplete()
        }
        await notifications.waitUntilEntered()
        #expect(!(await completion.isComplete))
        // The encrypted lease remains intact until notification cancellation
        // completes, so process termination at this boundary is recoverable.
        #expect(planner.executionState.activeSession == paused)
        #expect(await notifications.lastSessionWasNil)
        await notifications.release()
        try await replacement.value
        #expect(await completion.isComplete)
        #expect(planner.executionState.activeSession == nil)
    }

    @Test("failed credential preparation restores the still-current reminder")
    func failedCredentialPreparationReconcilesCurrentBreak() async throws {
        let paused = try Self.pausedSession(sessionID: Self.uuid(87))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = PlannerStore(
            canonicalConfigurationIdentifier: Self.canonicalConfiguration,
            executionState: state,
            restoreFromPersistence: false,
            now: { Self.baseDate }
        )
        let notifications = RecordingBreakNotificationCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            breakNotificationCoordinator: notifications
        )

        do {
            try await sync.prepareForCredentialReplacement()
            Issue.record("Expected encrypted persistence to be required")
        } catch PlannerExecutionStateError.encryptedPersistenceRequired {
            // Expected: the first nil is the cancellation precondition and the
            // exact paused lease is then reconciled after preparation fails.
        }

        #expect(await notifications.observedSessionIDs == [nil, paused.id])
        #expect(planner.executionState.activeSession == paused)
        #expect(!sync.isSyncing)
    }

    @Test("unverified notification cancellation preserves encrypted credentials and lease")
    func failedNotificationCancellationBlocksCredentialReplacement() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(95))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let notifications = RecoveringBreakNotificationCoordinator(
            mode: .schedulingUnavailable
        )
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            breakNotificationCoordinator: notifications
        )

        do {
            try await sync.prepareForCredentialReplacement()
            Issue.record("Expected notification cancellation to block replacement")
        } catch PlannerExecutionStateError.breakNotificationCancellationUnavailable {
            // Expected: no encrypted state may be destroyed without observable
            // pending-and-delivered disappearance.
        }

        #expect(planner.executionState.activeSession == paused)
        #expect(planner.hasEncryptedPersistence)
        #expect(sync.breakNotificationIssue == .cancellationUnavailable)
        #expect(!sync.isSyncing)
    }

    @Test("canonical reset cancellation is an awaited barrier and leaves no presentation")
    func canonicalResetAwaitsBreakNotificationCancellation() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(85))
        var state = Self.emptyBoundState
        state.revision = paused.revision
        state.activeSession = paused
        state.historyWindow = [paused]
        state.historyWindowRevision = paused.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        let planner = Self.planner(persistence: context.persistence, executionState: state)
        let notifications = GatedBreakNotificationCoordinator()
        let sync = Self.controller(
            planner: planner,
            transport: ExecutionTransportDouble(snapshots: [], pages: []),
            breakNotificationCoordinator: notifications
        )
        let completion = AsyncCompletionProbe()

        let cancellation = Task { @MainActor in
            await sync.cancelBreakNotificationsForConfigurationReset()
            await completion.markComplete()
        }
        await notifications.waitUntilEntered()
        #expect(!(await completion.isComplete))
        #expect(await notifications.lastSessionWasNil)
        await notifications.release()
        await cancellation.value
        #expect(await completion.isComplete)
        #expect(sync.breakResolutionPresentation == nil)
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

    @Test("foreground stream coalesces high-water hints through authoritative refresh")
    func foregroundStreamCoalescesHintsWithoutPersistingThem() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(1_401),
            sessionIndex: 0,
            startedAt: Self.baseDate.addingTimeInterval(-60)
        )
        let advanced = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty, advanced, advanced],
            pages: [
                .init(sessions: [], nextOffset: nil),
                .init(sessions: [terminal], nextOffset: nil),
            ],
            snapshotDelay: .milliseconds(75)
        )
        let stream = ExecutionStreamDouble(
            initialDelay: .milliseconds(250),
            revisions: [1, 2],
            completion: .unsupported
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let emissionDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.emissionCount() < 2, ContinuousClock.now < emissionDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        // Delivery cannot promote the advertised revision. The delayed
        // snapshot read is still the only path that can change durable state.
        #expect(planner.executionState.revision == 0)

        let refreshDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while planner.executionState.revision < 2, ContinuousClock.now < refreshDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        sync.stopForegroundPolling()

        #expect(planner.executionState.revision == 2)
        #expect(planner.executionState.terminalOutcomes[terminal.id]?.session == terminal)
        #expect(await transport.snapshotRequestCount() == 4)
        #expect(await stream.requestedRevisions() == [0])
    }

    @Test("unreachable stream high-water causes at most one refresh per connection")
    func foregroundStreamBoundsUnreachableHintRefresh() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty, empty, empty],
            pages: [
                .init(sessions: [], nextOffset: nil),
                .init(sessions: [], nextOffset: nil),
            ],
            snapshotDelay: .milliseconds(30)
        )
        let stream = ExecutionStreamDouble(
            initialDelay: .milliseconds(150),
            interEventDelay: .milliseconds(250),
            revisions: [9, 10, 11],
            completion: .unsupported
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.emissionCount() < 3, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try await Task.sleep(for: .milliseconds(300))
        sync.stopForegroundPolling()

        #expect(planner.executionState.revision == 0)
        // Two stable snapshots for the immediate poll and exactly two for the
        // one coalesced hint refresh—never a 250 ms retry loop.
        #expect(await transport.snapshotRequestCount() == 4)
    }

    @Test("stream ignores revisions already represented by durable encrypted state")
    func foregroundStreamIgnoresOldAndEqualHints() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(1_402),
            sessionIndex: 0,
            startedAt: Self.baseDate.addingTimeInterval(-60)
        )
        let snapshot = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [terminal], nextOffset: nil)]
        )
        let stream = ExecutionStreamDouble(
            initialDelay: .milliseconds(100),
            revisions: [1, 2],
            completion: .unsupported
        )
        let planner = Self.planner(
            persistence: context.persistence,
            executionState: Self.terminalState(terminal)
        )
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.emissionCount() < 2, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try await Task.sleep(for: .milliseconds(100))
        sync.stopForegroundPolling()

        #expect(await transport.snapshotRequestCount() == 2)
        #expect(await stream.requestedRevisions() == [2])
        #expect(planner.executionState.revision == 2)
    }

    @Test("stopping foreground services cancels a blocked stream immediately")
    func stoppingForegroundServicesCancelsStream() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let stream = BlockingExecutionStreamDouble()
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let startDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !(await stream.hasStarted()), ContinuousClock.now < startDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        sync.stopForegroundPolling()
        let cancellationDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while !(await stream.wasCancelled()), ContinuousClock.now < cancellationDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }

        #expect(await stream.hasStarted())
        #expect(await stream.wasCancelled())
    }

    @Test("stream hint waits for the active execution lane instead of being dropped")
    func foregroundStreamRetriesBusyAdmission() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let terminal = try Self.terminalSession(
            sessionID: Self.uuid(1_403),
            sessionIndex: 0,
            startedAt: Self.baseDate.addingTimeInterval(-60)
        )
        let advanced = DayWeaveExecutionSnapshot(revision: 2, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty, empty, empty, advanced, advanced],
            pages: [
                .init(sessions: [], nextOffset: nil),
                .init(sessions: [], nextOffset: nil),
                .init(sessions: [terminal], nextOffset: nil),
            ],
            snapshotDelay: .milliseconds(100)
        )
        // Streaming starts only after the immediate foreground poll. Delay its
        // first hint long enough for an explicit refresh to acquire the shared
        // execution lane, then prove the hint waits and retries admission.
        let stream = ExecutionStreamDouble(
            initialDelay: .milliseconds(150),
            revisions: [2],
            completion: .unsupported
        )
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let streamDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.requestedRevisions().isEmpty,
              ContinuousClock.now < streamDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        let overlappingRefresh = Task { @MainActor in
            await sync.refresh()
        }
        #expect(await overlappingRefresh.value == .success)
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while planner.executionState.revision < 2, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        sync.stopForegroundPolling()

        #expect(planner.executionState.revision == 2)
        #expect(await transport.snapshotRequestCount() == 6)
    }

    @Test("transient stream failures back off from one to thirty seconds silently")
    func foregroundStreamReconnectsWithBoundedBackoff() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let failure = DayWeaveAPIError.transport(.networkConnectionLost)
        let stream = PlannedExecutionStreamDouble(plans: [
            .failure(failure), .failure(failure), .failure(failure),
            .failure(failure), .failure(failure), .failure(failure),
            .failure(failure), .completion(.unsupported),
        ])
        let sleeps = ExecutionStreamSleepRecorder()
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream,
            executionStreamSleep: { duration in
                await sleeps.record(duration)
            }
        )

        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.requestCount() < 8, ContinuousClock.now < deadline {
            await Task.yield()
        }
        while sync.status.phase != .connected, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        sync.stopForegroundPolling()

        #expect(await sleeps.values() == [
            .seconds(1), .seconds(2), .seconds(4), .seconds(8),
            .seconds(16), .seconds(30), .seconds(30),
        ])
        #expect(sync.status.phase == .connected)
    }

    @Test("repeated immediate stream EOFs use the same bounded transient backoff")
    func foregroundStreamBacksOffRepeatedImmediateEOF() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let stream = PlannedExecutionStreamDouble(plans: [
            .completion(.endOfStream), .completion(.endOfStream),
            .completion(.endOfStream), .completion(.endOfStream),
            .completion(.endOfStream), .completion(.endOfStream),
            .completion(.endOfStream), .completion(.unsupported),
        ])
        let sleeps = ExecutionStreamSleepRecorder()
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream,
            executionStreamSleep: { duration in
                await sleeps.record(duration)
            }
        )

        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.requestCount() < 8, ContinuousClock.now < deadline {
            await Task.yield()
        }
        sync.stopForegroundPolling()

        #expect(await sleeps.values() == [
            .seconds(1), .seconds(2), .seconds(4), .seconds(8),
            .seconds(16), .seconds(30), .seconds(30),
        ])
        #expect(sync.status.phase == .connected)
    }

    @Test("stream liveness resets early-EOF reconnect backoff")
    func foregroundStreamLivenessResetsEOFBackoff() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let stream = PlannedExecutionStreamDouble(plans: [
            .completion(.endOfStream), .completion(.endOfStream),
            .completion(.liveEndOfStream), .completion(.endOfStream),
            .completion(.unsupported),
        ])
        let sleeps = ExecutionStreamSleepRecorder()
        let planner = Self.planner(persistence: context.persistence)
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream,
            executionStreamSleep: { duration in
                await sleeps.record(duration)
            }
        )

        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.requestCount() < 5, ContinuousClock.now < deadline {
            await Task.yield()
        }
        sync.stopForegroundPolling()

        #expect(await sleeps.values() == [
            .seconds(1), .seconds(2), .seconds(1), .seconds(1),
        ])
    }

    @Test("stream captures Last-Event-ID only after a foreign binding is quarantined")
    func foregroundStreamStartsAfterFreshBindingPoll() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        var foreignState = Self.emptyBoundState
        foreignState.bindingIdentifier = "execution-v1:foreign-binding"
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let stream = ExecutionStreamDouble(
            revisions: [],
            completion: .unsupported
        )
        let planner = Self.planner(
            persistence: context.persistence,
            executionState: foreignState
        )
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.requestedRevisions().isEmpty,
              ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        sync.stopForegroundPolling()

        #expect(planner.executionState.bindingIdentifier == Self.binding)
        #expect(planner.executionState.historyVerified)
        #expect(await stream.requestedRevisions() == [0])
        #expect(await transport.snapshotRequestCount() == 2)
    }

    @Test("failed durable binding persistence never admits the foreground stream")
    func foregroundStreamRequiresDurableBindingPersistence() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        var foreignState = Self.emptyBoundState
        foreignState.bindingIdentifier = "execution-v1:foreign-binding"
        let planner = Self.planner(
            persistence: context.persistence,
            executionState: foreignState
        )
        planner.flushPersistence()
        #expect(planner.persistenceError == nil)

        // Advance the encrypted compare-and-swap generation behind this
        // process. prepareExecutionBinding will update memory first, but its
        // mandatory flush must fail closed before streaming can capture that
        // non-durable binding as Last-Event-ID.
        let newerWriter = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        newerWriter.lastScheduleMessage = "Newer encrypted writer"
        newerWriter.flushPersistence()
        #expect(newerWriter.persistenceError == nil)

        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let stream = ExecutionStreamDouble(
            revisions: [],
            completion: .unsupported
        )
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            streamTransport: stream
        )

        sync.startForegroundPolling(every: .seconds(60))
        let failureDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while planner.loadState != .persistenceFailed,
              ContinuousClock.now < failureDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try await Task.sleep(for: .milliseconds(50))
        sync.stopForegroundPolling()

        #expect(planner.loadState == .persistenceFailed)
        #expect(planner.executionState.bindingIdentifier == Self.binding)
        #expect(await stream.requestedRevisions().isEmpty)
        #expect(await transport.snapshotRequestCount() == 0)
    }

    @Test("stream retries readiness when the first foreground poll cannot bind")
    func foregroundStreamStartsAfterLaterSuccessfulBindingPoll() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)],
            repeatsLastResponses: true
        )
        let stream = ExecutionStreamDouble(
            revisions: [],
            completion: .unsupported
        )
        let connection = DayWeaveExecutionConnection(
            canonicalConfigurationIdentifier: Self.canonicalConfiguration,
            bindingIdentifier: Self.binding,
            transport: transport,
            streamTransport: stream
        )
        let connectionAvailability = ExecutionConnectionAvailability(connection: connection)
        let planner = Self.planner(
            persistence: context.persistence,
            executionState: .empty
        )
        let sync = ExecutionSyncStore(
            planner: planner,
            connectionProvider: {
                try connectionAvailability.provide()
            },
            now: { Self.baseDate },
            makeUUID: { Self.uuid(500) }
        )

        sync.startForegroundPolling(every: .milliseconds(75))
        let failureDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while sync.status.phase != .offline, ContinuousClock.now < failureDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(sync.status.phase == .offline)
        #expect(await stream.requestedRevisions().isEmpty)

        connectionAvailability.isAvailable = true
        let recoveryDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        var requestedRevisions: [UInt64] = []
        repeat {
            requestedRevisions = await stream.requestedRevisions()
            if requestedRevisions == [0],
               planner.executionState.historyVerified,
               sync.status.phase == .connected {
                break
            }
            try await Task.sleep(for: .milliseconds(10))
        } while ContinuousClock.now < recoveryDeadline
        sync.stopForegroundPolling()

        #expect(planner.executionState.bindingIdentifier == Self.binding)
        #expect(planner.executionState.historyVerified)
        #expect(requestedRevisions == [0])
        #expect(sync.status.phase == .connected)
    }

    @Test("late events from a replaced credential binding are ignored")
    func foregroundStreamIgnoresOldBindingHints() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        let transport = ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
        let stream = ExecutionStreamDouble(
            initialDelay: .milliseconds(200),
            revisions: [10],
            completion: .unsupported
        )
        let planner = Self.planner(persistence: context.persistence)
        var connection = DayWeaveExecutionConnection(
            canonicalConfigurationIdentifier: Self.canonicalConfiguration,
            bindingIdentifier: Self.binding,
            transport: transport,
            streamTransport: stream
        )
        let sync = ExecutionSyncStore(
            planner: planner,
            connectionProvider: { connection },
            now: { Self.baseDate },
            makeUUID: { Self.uuid(500) }
        )

        sync.startForegroundPolling(every: .seconds(60))
        let pollDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while sync.status.phase != .connected, ContinuousClock.now < pollDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        connection = DayWeaveExecutionConnection(
            canonicalConfigurationIdentifier: Self.canonicalConfiguration,
            bindingIdentifier: "execution-v1:replacement-binding",
            transport: transport
        )
        let emissionDeadline = ContinuousClock.now.advanced(by: .seconds(5))
        while await stream.emissionCount() < 1, ContinuousClock.now < emissionDeadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try await Task.sleep(for: .milliseconds(100))
        sync.stopForegroundPolling()

        #expect(planner.executionState.revision == 0)
        #expect(await transport.snapshotRequestCount() == 2)
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

    private static func terminalState(
        _ session: DayWeaveExecutionSession
    ) -> DayWeaveExecutionDurableState {
        var state = emptyBoundState
        state.revision = session.revision
        state.historyWindow = [session]
        state.historyWindowRevision = session.revision
        state.terminalOutcomes[session.id] = .init(
            session: session,
            recordedAt: session.updatedAt,
            projection: .notRequired
        )
        state.presentedBlockIDs = session.plannedBlockID.map { [$0] } ?? []
        return state
    }

    private static func replacementPublication(
        after deferred: DayWeaveExecutionSession
    ) throws -> (
        publication: PendingSchedulePublication,
        block: ScheduleBlock,
        response: DayWeaveSchedulePublishResponse
    ) {
        let moveStart = try #require(deferred.moveStart)
        let moveEnd = try #require(deferred.moveEnd)
        let replacementID = uuid(1_134)
        let horizonStart = baseDate.addingTimeInterval(-3_600)
        let horizonEnd = baseDate.addingTimeInterval(86_400)
        let digest = "sha256:\(String(repeating: "e", count: 64))"
        let schedule = DayWeaveSchedulePreviewRequest(
            asOf: baseDate,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            timezoneName: "UTC",
            availability: [],
            fixedBlocks: [],
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )
        let previewBlock = DayWeaveSchedulePreview.Plan.Block(
            id: replacementID,
            isSensitive: false,
            itemID: itemID,
            occurrenceID: deferred.occurrenceID,
            externalBlockID: nil,
            title: "Deferred replacement",
            start: moveStart,
            end: moveEnd,
            sessionIndex: deferred.sessionIndex + 1,
            kind: "pinned",
            explanations: []
        )
        let plan = DayWeaveSchedulePreview.Plan(
            asOf: baseDate,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            blocks: [previewBlock],
            unscheduled: [],
            decisions: [],
            violations: [],
            score: .init(
                scheduledMinutes: UInt32(
                    try #require(dayWeaveExactWholeSecondDelta(
                        from: moveStart,
                        to: moveEnd
                    )) / 60
                ),
                unscheduledMinutes: 0,
                softPenalty: 0,
                movedMinutes: 0
            ),
            occurrences: []
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let planObject = try JSONSerialization.jsonObject(with: encoder.encode(plan))
        let previewObject: [String: Any] = [
            "input_digest": digest,
            "source_item_count": 1,
            "accepted_item_count": 1,
            "source_item_revisions": [itemID.uuidString.lowercased(): deferred.itemRevision],
            "rejected_items": [],
            "ignored_previous_assignments": [],
            "plan": planObject,
        ]
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let preview = try decoder.decode(
            DayWeaveSchedulePreview.self,
            from: JSONSerialization.data(withJSONObject: previewObject)
        )
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: canonicalConfiguration,
            generatedAt: baseDate,
            asOf: baseDate,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            timezoneName: "UTC"
        )
        let publication = PendingSchedulePublication(
            configurationIdentifier: canonicalConfiguration,
            preparedRequest: .init(
                request: .init(
                    idempotencyKey: uuid(1_135),
                    expectedInputDigest: digest,
                    schedule: schedule
                ),
                body: Data("{}".utf8),
                bodySHA256: String(repeating: "f", count: 64)
            ),
            preview: preview,
            message: "Published deferred replacement",
            provenance: provenance,
            preparedAt: baseDate
        )
        var block = Self.block(id: replacementID, sessionIndex: deferred.sessionIndex + 1)
        block.title = previewBlock.title
        block.start = moveStart
        block.end = moveEnd
        block.previewKind = "pinned"
        let revisionID = uuid(1_136)
        let response = DayWeaveSchedulePublishResponse(
            revision: .init(
                id: revisionID,
                revision: "1:\(revisionID.uuidString.lowercased())",
                revisionNumber: 1,
                inputDigest: digest,
                horizonStart: horizonStart,
                horizonEnd: horizonEnd,
                timezoneName: "UTC",
                publishedAt: baseDate
            ),
            replayed: false
        )
        return (publication, block, response)
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
        pendingExecutionDeferIntent: DayWeavePendingExecutionDeferIntent? = nil,
        executionState: DayWeaveExecutionDurableState = emptyBoundState,
        includePublicationProof: Bool = true,
        proofPublishedAt: Date = baseDate,
        proofBlocks: [DayWeavePublishedScheduleBlockProof]? = nil
    ) -> PlannerStore {
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: canonicalConfiguration,
            generatedAt: baseDate,
            asOf: baseDate,
            horizonStart: baseDate.addingTimeInterval(-3_600),
            horizonEnd: baseDate.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let revisionID = UUID(uuidString: "10000000-0000-4000-8000-000000000001")!
        let publishedBlocks = blocks.compactMap { block -> DayWeavePublishedScheduleBlockProof? in
            guard block.syncOrigin == .canonicalPreview
                    || block.syncOrigin == .externalPreview else { return nil }
            return DayWeavePublishedScheduleBlockProof(block: block)
        }
        let proof = includePublicationProof ? DayWeavePublishedScheduleProof(
            configurationIdentifier: canonicalConfiguration,
            revisionID: revisionID,
            revision: "1:\(revisionID.uuidString.lowercased())",
            revisionNumber: 1,
            inputDigest: "sha256:\(String(repeating: "b", count: 64))",
            asOf: provenance.asOf,
            horizonStart: provenance.horizonStart,
            horizonEnd: provenance.horizonEnd,
            timezoneName: provenance.timezoneName,
            publishedAt: proofPublishedAt,
            publishedBlocks: proofBlocks ?? publishedBlocks
        ) : nil
        return PlannerStore(
            blocks: blocks,
            canonicalItems: canonicalItems,
            completedOccurrenceIDs: completedOccurrenceIDs,
            pendingCanonicalMutations: pendingCanonicalMutations,
            recurrenceSessionOutcomes: recurrenceSessionOutcomes,
            pendingExecutionDeferIntent: pendingExecutionDeferIntent,
            canonicalConfigurationIdentifier: canonicalConfiguration,
            schedulePreviewProvenance: provenance,
            publishedScheduleProof: proof,
            executionState: executionState,
            previewValidatedForCurrentLaunch: true,
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { baseDate }
        )
    }

    private static func controller(
        planner: PlannerStore,
        transport: ExecutionTransportDouble,
        habitProvider: (any HabitCompositionCheckpointProviding)? = nil,
        streamTransport: (any DayWeaveExecutionStreamTransport)? = nil,
        binding: String = binding,
        now: @escaping @Sendable () -> Date = { baseDate },
        breakNotificationCoordinator: any DayWeaveBreakNotificationCoordinating =
            DayWeaveNoopBreakNotificationCoordinator(),
        breakDeadlineSleep: @escaping @Sendable (Duration) async -> Void = { duration in
            try? await Task.sleep(for: duration)
        },
        executionStreamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        }
    ) -> ExecutionSyncStore {
        let connection = DayWeaveExecutionConnection(
            canonicalConfigurationIdentifier: canonicalConfiguration,
            bindingIdentifier: binding,
            transport: transport,
            streamTransport: streamTransport
        )
        return ExecutionSyncStore(
            planner: planner,
            habitCompositionProvider: habitProvider,
            connectionProvider: { connection },
            now: now,
            makeUUID: { uuid(500) },
            breakNotificationCoordinator: breakNotificationCoordinator,
            breakDeadlineSleep: breakDeadlineSleep,
            executionStreamSleep: executionStreamSleep
        )
    }

    private static func emptyReadTransport() -> ExecutionTransportDouble {
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        return ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
        )
    }

    private static func habitCheckpoint(
        configurationIdentifier: String? = canonicalConfiguration,
        deltaCursor: String? = "habit-cursor",
        deltaCaughtUp: Bool = true,
        pendingMutationIDs: [UUID] = [],
        hasActiveOperation: Bool = false
    ) -> HabitCompositionCheckpoint {
        .init(
            configurationIdentifier: configurationIdentifier,
            deltaCursor: deltaCursor,
            deltaCaughtUp: deltaCaughtUp,
            occurrences: [],
            pauses: [],
            pendingMutationIDs: pendingMutationIDs,
            hasActiveOperation: hasActiveOperation,
            operationGeneration: 1
        )
    }

    private static func block(
        id: UUID = blockID,
        sessionIndex: UInt16? = 0,
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

    private static func pausedSession(
        sessionID: UUID,
        accumulatedSeconds: UInt64 = 10
    ) throws -> DayWeaveExecutionSession {
        let updated = baseDate.addingTimeInterval(TimeInterval(accumulatedSeconds))
        return try session(
            id: sessionID,
            status: .paused,
            revision: 2,
            sessionIndex: 0,
            plannedBlockID: blockID,
            startedAt: baseDate,
            updatedAt: updated,
            accumulatedSeconds: accumulatedSeconds,
            actualSeconds: nil,
            runningSince: nil,
            pausedAt: updated,
            pauseUntil: updated.addingTimeInterval(60),
            endedAt: nil
        )
    }

    private static func deferAssessment(
        session: DayWeaveExecutionSession,
        executionRevision: UInt64,
        moveStart: Date,
        digestByte: Character,
        approvalRequired: Bool,
        expiresAt: Date
    ) -> DayWeaveDeferAssessment {
        let planned: UInt64 = 1_800
        let actual = session.accumulatedSeconds
        let roundedMinutes = actual / 60 + (actual % 60 == 0 ? 0 : 1)
        let credited = min(planned, roundedMinutes * 60)
        let remaining = planned - credited
        let conflicts: [DayWeaveDeferViolation]
        if approvalRequired {
            let conflictID = uuid(Int(executionRevision) + 8_000)
            let conflict = DayWeaveDeferConflict(
                blockID: conflictID,
                itemID: nil,
                occurrenceID: nil,
                externalBlockID: nil,
                kind: .calendarEvent,
                start: moveStart.addingTimeInterval(60),
                end: moveStart.addingTimeInterval(300)
            )
            conflicts = [DayWeaveDeferViolation(
                code: .immutableOverlap,
                itemIDs: [],
                occurrenceIDs: [],
                conflictingBlockIDs: [conflictID],
                conflictingBlocks: [conflict],
                start: moveStart,
                end: moveStart.addingTimeInterval(TimeInterval(remaining)),
                boundaryStart: nil,
                boundaryEnd: nil,
                message: "The exact placement overlaps immutable time"
            )]
        } else {
            conflicts = []
        }
        return DayWeaveDeferAssessment(
            sessionID: session.id,
            executionRevision: executionRevision,
            sessionRevision: session.revision,
            itemID: session.itemID,
            itemRevision: session.itemRevision,
            occurrenceID: session.occurrenceID,
            sourceSessionIndex: session.sessionIndex,
            replacementSessionIndex: session.sessionIndex + 1,
            sourceScheduleRevisionID: uuid(Int(executionRevision) + 9_000),
            sourceBlockID: session.plannedBlockID ?? blockID,
            actualSeconds: actual,
            creditedSourceSeconds: credited,
            plannedDurationSeconds: planned,
            remainingDurationSeconds: remaining,
            moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(TimeInterval(remaining)),
            environmentDigest: "sha256:\(String(repeating: "c", count: 64))",
            assessmentDigest: "sha256:\(String(repeating: digestByte, count: 64))",
            approvalRequired: approvalRequired,
            violations: conflicts,
            expiresAt: expiresAt
        )
    }

    private static func deferIntent(
        session: DayWeaveExecutionSession,
        moveStart: Date,
        assessment: DayWeaveDeferAssessment? = nil
    ) -> DayWeavePendingExecutionDeferIntent {
        DayWeavePendingExecutionDeferIntent(
            identity: .init(session: session),
            focusedBlockID: blockID,
            sourceStart: baseDate,
            sourceEnd: baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            assessment: assessment,
            approvedAssessmentDigest: nil,
            createdAt: baseDate,
            expiresAt: moveStart
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

@MainActor
private final class ExecutionHabitCheckpointStub: HabitCompositionCheckpointProviding {
    private(set) var habitCompositionCheckpoint: HabitCompositionCheckpoint
    private var observers: [@MainActor () -> Void] = []

    init(_ checkpoint: HabitCompositionCheckpoint) {
        habitCompositionCheckpoint = checkpoint
    }

    func observeHabitCompositionCheckpointChanges(
        _ observer: @escaping @MainActor () -> Void
    ) {
        observers.append(observer)
    }

    func update(_ checkpoint: HabitCompositionCheckpoint) {
        habitCompositionCheckpoint = checkpoint
        observers.forEach { $0() }
    }
}

private final class ExecutionSequenceClock: @unchecked Sendable {
    private let lock = NSLock()
    private var dates: [Date]

    init(_ dates: [Date]) {
        precondition(!dates.isEmpty)
        self.dates = dates
    }

    func now() -> Date {
        lock.lock()
        defer { lock.unlock() }
        guard dates.count > 1 else { return dates[0] }
        return dates.removeFirst()
    }

    func advance(to date: Date) {
        lock.lock()
        dates = [date]
        lock.unlock()
    }
}

private actor BreakDeadlineSleepGate {
    private var enteredDuration: Duration?
    private var released = false
    private var entryWaiters: [CheckedContinuation<Duration, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func sleep(for duration: Duration) async {
        enteredDuration = duration
        let waiters = entryWaiters
        entryWaiters.removeAll()
        waiters.forEach { $0.resume(returning: duration) }
        guard !released else { return }
        await withCheckedContinuation { continuation in
            releaseWaiters.append(continuation)
        }
    }

    func waitUntilEntered() async -> Duration {
        if let enteredDuration { return enteredDuration }
        return await withCheckedContinuation { continuation in
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

private actor GatedBreakNotificationCoordinator: DayWeaveBreakNotificationCoordinating {
    private var entered = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseContinuation: CheckedContinuation<Void, Never>?
    private var observedSession: DayWeaveExecutionSession?

    var lastSessionWasNil: Bool { entered && observedSession == nil }

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        .authorized
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .authorized
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        observedSession = session
        entered = true
        let waiters = entryWaiters
        entryWaiters.removeAll()
        waiters.forEach { $0.resume() }
        await withCheckedContinuation { continuation in
            releaseContinuation = continuation
        }
        guard let descriptor = DayWeaveBreakNotificationContract.descriptor(
            for: session
        ) else { return .canceled }
        return descriptor.version == acknowledged ? .canceled : .scheduled
    }

    func cancelExact(
        identifier: String,
        session: DayWeaveExecutionSession,
        acknowledged: DayWeaveExecutionSessionVersion
    ) async -> DayWeaveBreakNotificationReconcileResult {
        guard DayWeaveBreakNotificationContract.descriptor(for: session)?.identifier
                == identifier else {
            return .cancellationUnavailable
        }
        return await reconcile(session: session, acknowledged: acknowledged)
    }

    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { continuation in
            entryWaiters.append(continuation)
        }
    }

    func release() {
        releaseContinuation?.resume()
        releaseContinuation = nil
    }
}

private actor GatedBreakNotificationPermissionCoordinator:
    DayWeaveBreakNotificationCoordinating
{
    private var authorizationRequestEntered = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseContinuation: CheckedContinuation<Void, Never>?

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        authorizationRequestEntered ? .authorized : .notDetermined
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        authorizationRequestEntered = true
        let waiters = entryWaiters
        entryWaiters.removeAll()
        waiters.forEach { $0.resume() }
        await withCheckedContinuation { continuation in
            releaseContinuation = continuation
        }
        return .authorized
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        _ = session
        _ = acknowledged
        return .scheduled
    }

    func waitUntilAuthorizationRequestEntered() async {
        guard !authorizationRequestEntered else { return }
        await withCheckedContinuation { continuation in
            entryWaiters.append(continuation)
        }
    }

    func releaseAuthorizationRequest() {
        releaseContinuation?.resume()
        releaseContinuation = nil
    }
}

private actor RecordingBreakNotificationCoordinator: DayWeaveBreakNotificationCoordinating {
    private(set) var observedSessionIDs: [UUID?] = []

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        .authorized
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .authorized
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        _ = acknowledged
        observedSessionIDs.append(session?.id)
        return session == nil ? .canceled : .scheduled
    }
}

private actor RecoveringBreakNotificationCoordinator:
    DayWeaveBreakNotificationCoordinating
{
    enum Mode: Sendable {
        case authorizationUnavailable
        case schedulingUnavailable
        case cancellationUnavailable
        case available
    }

    private var mode: Mode

    init(mode: Mode) {
        self.mode = mode
    }

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        switch mode {
        case .authorizationUnavailable: .notDetermined
        case .schedulingUnavailable, .cancellationUnavailable, .available: .authorized
        }
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        switch mode {
        case .authorizationUnavailable: .unavailable
        case .schedulingUnavailable, .cancellationUnavailable, .available: .authorized
        }
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        _ = session
        _ = acknowledged
        return switch mode {
        case .authorizationUnavailable: .permissionRequired
        case .schedulingUnavailable: .unavailable
        case .cancellationUnavailable: .cancellationUnavailable
        case .available: .scheduled
        }
    }

    func setMode(_ mode: Mode) {
        self.mode = mode
    }
}

private actor DeniedCancellationRetryCoordinator:
    DayWeaveBreakNotificationCoordinating
{
    private(set) var reconcileAttempts = 0

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        .denied
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .denied
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        _ = session
        _ = acknowledged
        reconcileAttempts += 1
        return reconcileAttempts == 1 ? .cancellationUnavailable : .canceled
    }
}

private actor AsyncCompletionProbe {
    private var completed = false

    var isComplete: Bool { completed }

    func markComplete() { completed = true }
}

@MainActor
private final class ExecutionConnectionAvailability {
    let connection: DayWeaveExecutionConnection
    var isAvailable = false

    init(connection: DayWeaveExecutionConnection) {
        self.connection = connection
    }

    func provide() throws -> DayWeaveExecutionConnection {
        guard isAvailable else {
            throw DayWeaveAPIError.transport(.notConnectedToInternet)
        }
        return connection
    }
}

private actor ExecutionStreamDouble: DayWeaveExecutionStreamTransport {
    private let initialDelay: Duration?
    private let interEventDelay: Duration?
    private let revisions: [UInt64]
    private let completion: DayWeaveExecutionStreamCompletion
    private var requests: [UInt64] = []
    private var emissions = 0

    init(
        initialDelay: Duration? = nil,
        interEventDelay: Duration? = nil,
        revisions: [UInt64],
        completion: DayWeaveExecutionStreamCompletion
    ) {
        self.initialDelay = initialDelay
        self.interEventDelay = interEventDelay
        self.revisions = revisions
        self.completion = completion
    }

    func consumeExecutionInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamCompletion {
        requests.append(revision)
        if let initialDelay { try await Task.sleep(for: initialDelay) }
        for (index, revision) in revisions.enumerated() {
            if index > 0, let interEventDelay {
                try await Task.sleep(for: interEventDelay)
            }
            emissions += 1
            await receive(revision)
        }
        return completion
    }

    func requestedRevisions() -> [UInt64] { requests }
    func emissionCount() -> Int { emissions }
}

private actor BlockingExecutionStreamDouble: DayWeaveExecutionStreamTransport {
    private var started = false
    private var cancelled = false

    func consumeExecutionInvalidations(
        after _: UInt64,
        _: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamCompletion {
        started = true
        do {
            try await Task.sleep(for: .seconds(3_600))
            return .endOfStream
        } catch {
            cancelled = Task.isCancelled
            throw error
        }
    }

    func hasStarted() -> Bool { started }
    func wasCancelled() -> Bool { cancelled }
}

private actor PlannedExecutionStreamDouble: DayWeaveExecutionStreamTransport {
    enum Plan: Sendable {
        case failure(DayWeaveAPIError)
        case completion(DayWeaveExecutionStreamCompletion)
    }

    private var plans: [Plan]
    private var requests: [UInt64] = []

    init(plans: [Plan]) {
        self.plans = plans
    }

    func consumeExecutionInvalidations(
        after revision: UInt64,
        _: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamCompletion {
        requests.append(revision)
        guard !plans.isEmpty else { return .unsupported }
        switch plans.removeFirst() {
        case let .failure(error): throw error
        case let .completion(completion): return completion
        }
    }

    func requestCount() -> Int { requests.count }
}

private actor ExecutionStreamSleepRecorder {
    private var recorded: [Duration] = []

    func record(_ duration: Duration) {
        recorded.append(duration)
    }

    func values() -> [Duration] { recorded }
}

private actor ExecutionTransportDouble: DayWeaveExecutionTransport {
    enum Reply: Sendable {
        case mutation(DayWeaveExecutionMutation)
        case failure(DayWeaveAPIError)
    }

    enum AssessmentReply: Sendable {
        case assessment(DayWeaveDeferAssessment)
        case failure(DayWeaveAPIError)
    }

    struct ReceivedCommand: Equatable, Sendable {
        let body: Data
        let key: String
    }

    private var snapshots: [DayWeaveExecutionSnapshot]
    private var pages: [DayWeaveExecutionHistoryPage]
    private var commandReplies: [Reply]
    private var assessmentReplies: [AssessmentReply]
    private var offsets: [Int] = []
    private var commands: [ReceivedCommand] = []
    private var assessmentRequests: [DayWeaveDeferAssessmentRequest] = []
    private var snapshotCount = 0
    private var lastSnapshot: DayWeaveExecutionSnapshot?
    private let repeatsLastResponses: Bool
    private let snapshotDelay: Duration?
    private let onCommandReceived: (@Sendable () -> Void)?

    init(
        snapshots: [DayWeaveExecutionSnapshot],
        pages: [DayWeaveExecutionHistoryPage],
        commandReplies: [Reply] = [],
        assessmentReplies: [AssessmentReply] = [],
        repeatsLastResponses: Bool = false,
        snapshotDelay: Duration? = nil,
        onCommandReceived: (@Sendable () -> Void)? = nil
    ) {
        self.snapshots = snapshots
        self.pages = pages
        self.commandReplies = commandReplies
        self.assessmentReplies = assessmentReplies
        self.repeatsLastResponses = repeatsLastResponses
        self.snapshotDelay = snapshotDelay
        self.onCommandReceived = onCommandReceived
    }

    func executionSnapshot() async throws -> DayWeaveExecutionSnapshot {
        snapshotCount += 1
        if let snapshotDelay { try await Task.sleep(for: snapshotDelay) }
        guard let snapshot = snapshots.first else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        if !repeatsLastResponses || snapshots.count > 1 {
            snapshots.removeFirst()
        }
        lastSnapshot = snapshot
        return snapshot
    }

    func executionHistoryPage(limit: Int, offset: Int) async throws
        -> DayWeaveExecutionHistoryPage
    {
        offsets.append(offset)
        guard limit == DayWeaveAPIClient.maximumExecutionHistoryLimit,
              let page = pages.first else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        if !repeatsLastResponses || pages.count > 1 {
            pages.removeFirst()
        }
        return page
    }

    nonisolated func encodedExecutionCommand(
        _ request: DayWeaveExecutionCommandRequest
    ) throws -> Data {
        try DayWeaveExecutionWireCodec.encode(request)
    }

    func assessExecutionDefer(
        _ request: DayWeaveDeferAssessmentRequest
    ) async throws -> DayWeaveDeferAssessment {
        assessmentRequests.append(request)
        if !assessmentReplies.isEmpty {
            switch assessmentReplies.removeFirst() {
            case let .assessment(assessment): return assessment
            case let .failure(error): throw error
            }
        }
        guard let session = lastSnapshot?.activeSession,
              session.id == request.sessionID,
              session.status == .paused,
              let sourceBlockID = session.plannedBlockID,
              session.sessionIndex < UInt16.max else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        let planned: UInt64 = 1_800
        let actual = request.actualSeconds ?? session.accumulatedSeconds
        let roundedMinutes = actual / 60 + (actual % 60 == 0 ? 0 : 1)
        let credited = min(planned, roundedMinutes * 60)
        guard credited < planned else { throw DayWeaveAPIError.responseDecodingFailed }
        let remaining = planned - credited
        let scheduleRevisionID = UUID(
            uuidString: "10000000-0000-4000-8000-000000000001"
        )!
        return DayWeaveDeferAssessment(
            sessionID: session.id,
            executionRevision: request.expectedRevision,
            sessionRevision: session.revision,
            itemID: session.itemID,
            itemRevision: session.itemRevision,
            occurrenceID: session.occurrenceID,
            sourceSessionIndex: session.sessionIndex,
            replacementSessionIndex: session.sessionIndex + 1,
            sourceScheduleRevisionID: scheduleRevisionID,
            sourceBlockID: sourceBlockID,
            actualSeconds: actual,
            creditedSourceSeconds: credited,
            plannedDurationSeconds: planned,
            remainingDurationSeconds: remaining,
            moveStart: request.moveStart,
            moveEnd: request.moveStart.addingTimeInterval(TimeInterval(remaining)),
            environmentDigest: "sha256:\(String(repeating: "c", count: 64))",
            assessmentDigest: "sha256:\(String(repeating: "d", count: 64))",
            approvalRequired: false,
            violations: [],
            expiresAt: min(
                request.moveStart.addingTimeInterval(-1),
                session.updatedAt.addingTimeInterval(5 * 60)
            )
        )
    }

    func applyExecutionCommand(
        encodedRequest: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveExecutionMutation {
        commands.append(.init(body: encodedRequest, key: idempotencyKey))
        onCommandReceived?()
        guard !commandReplies.isEmpty else { throw DayWeaveAPIError.responseDecodingFailed }
        switch commandReplies.removeFirst() {
        case let .mutation(mutation): return mutation
        case let .failure(error): throw error
        }
    }

    func requestedOffsets() -> [Int] { offsets }
    func receivedCommands() -> [ReceivedCommand] { commands }
    func receivedAssessmentRequests() -> [DayWeaveDeferAssessmentRequest] {
        assessmentRequests
    }
    func snapshotRequestCount() -> Int { snapshotCount }
}
#endif
