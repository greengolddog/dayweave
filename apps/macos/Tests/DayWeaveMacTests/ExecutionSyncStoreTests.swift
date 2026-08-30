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

    @Test("Will do later pauses first, fences exact remaining seconds, and invalidates old proof")
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
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [activeBlock],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let sync = Self.controller(
            planner: planner,
            transport: transport,
            now: { pausedAt }
        )

        #expect(await sync.deferWork(Self.blockID, moveStart: moveStart) == .success)
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
            actualSeconds: 300
        ))
        #expect(planner.executionState.activeSession == nil)
        #expect(planner.executionState.terminalOutcomes[sessionID]?.session == deferred)
        #expect(planner.executionState.terminalOutcomes[sessionID]?.projection == .notRequired)
        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.pendingCanonicalMutations.isEmpty)
        #expect(planner.blocks.first(where: { $0.id == Self.blockID })?.status == .scheduled)
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
        #expect(firstPlanner.pendingExecutionDeferIntent?.sourceOverrideApproved == true)
        guard case .pause = firstPlanner.executionState.pendingCommand?.command else {
            Issue.record("Expected the interrupted exact Pause journal")
            return
        }

        let restored = PlannerStore(
            persistence: context.persistence,
            now: { pausedAt }
        )
        #expect(restored.pendingExecutionDeferIntent?.sourceOverrideApproved == true)
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
            moveEnd: moveStart.addingTimeInterval(1_500), actualSeconds: 300
        ))
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.terminalOutcomes[sessionID]?.session == deferred)
    }

    @Test("a v5 Defer intent preserves microsecond source endpoints across relaunch")
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
            approvedMoveEnd: moveStart.addingTimeInterval(1_500),
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: Self.baseDate,
            expiresAt: Self.baseDate.addingTimeInterval(1_800)
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
            actualSeconds: 300
        ))
        #expect(restored.pendingExecutionDeferIntent == nil)
        #expect(restored.executionState.terminalOutcomes[sessionID]?.session == deferred)
    }

    @Test("active pinned work requires an explicit source-placement override before Pause")
    func pinnedDeferRequiresSourceOverride() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_140)
        let active = try Self.activeSession(sessionID: sessionID)
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .active
        block.previewKind = "pinned"
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let transport = Self.emptyReadTransport()

        #expect(await Self.controller(
            planner: planner,
            transport: transport
        ).deferWork(
            Self.blockID,
            moveStart: Self.baseDate.addingTimeInterval(3_600)
        ) == .invalidLocalState)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == active)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("legacy saved Defer intent relaunches paused and requires fresh approval")
    func legacyDeferIntentFailsClosedAfterRelaunch() async throws {
        struct LegacyIntent: Encodable {
            let version: Int
            let identity: DayWeaveExecutionIdentity
            let focusedBlockID: UUID
            let moveStart: Date
            let createdAt: Date
            let expiresAt: Date
        }
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_141))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let encoded = try JSONEncoder().encode(LegacyIntent(
            version: 1,
            identity: .init(session: paused),
            focusedBlockID: Self.blockID,
            moveStart: moveStart,
            createdAt: Self.baseDate,
            expiresAt: Self.baseDate.addingTimeInterval(1_800)
        ))
        let legacy = try JSONDecoder().decode(
            DayWeavePendingExecutionDeferIntent.self,
            from: encoded
        )
        #expect(legacy.hasPersistableShape)
        #expect(!legacy.hasValidShape)
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        let first = Self.planner(
            persistence: context.persistence,
            blocks: [block],
            canonicalItems: [try Self.canonicalItem()],
            pendingExecutionDeferIntent: legacy,
            executionState: durable
        )
        first.flushPersistence()

        let restored = PlannerStore(persistence: context.persistence, now: { Self.baseDate })
        #expect(restored.persistenceError == nil)
        #expect(restored.pendingExecutionDeferIntent?.version == 1)
        #expect(!restored.canMutatePlan)
        #expect(!restored.beginCanonicalSync())
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

    @Test("a new fixed conflict after approval clears recovery intent and leaves Pause intact")
    func changedRiskAfterRelaunchRequiresFreshApproval() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let paused = try Self.pausedSession(sessionID: Self.uuid(1_142))
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused),
            focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart.addingTimeInterval(1_790),
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: Self.baseDate,
            expiresAt: Self.baseDate.addingTimeInterval(1_800)
        )
        var durable = Self.emptyBoundState
        durable.revision = paused.revision
        durable.activeSession = paused
        durable.historyWindow = [paused]
        durable.historyWindowRevision = paused.revision
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .paused
        var newlyFixed = Self.block(id: Self.uuid(1_143))
        newlyFixed.sourceItemID = nil
        newlyFixed.sourceItemRevision = nil
        newlyFixed.start = moveStart.addingTimeInterval(60)
        newlyFixed.end = moveStart.addingTimeInterval(300)
        newlyFixed.isFlexible = false
        newlyFixed.isHardConstraint = true
        newlyFixed.syncOrigin = .local
        let first = Self.planner(
            persistence: context.persistence,
            blocks: [block, newlyFixed],
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
            approvedMoveEnd: moveStart.addingTimeInterval(1_790),
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: Self.baseDate,
            expiresAt: Self.baseDate.addingTimeInterval(1_800)
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
        let command = DayWeaveExecutionCommand.deferWork(
            sessionID: sessionID,
            moveStart: moveStart,
            moveEnd: moveStart.addingTimeInterval(1_500),
            actualSeconds: 300
        )
        let pending = DayWeavePendingExecutionCommand(
            idempotencyKey: "mac-execution-superseded-defer",
            bindingIdentifier: Self.binding,
            expectedRevision: 2,
            identity: .init(session: paused),
            command: command,
            encodedRequest: try DayWeaveExecutionWireCodec.encode(.init(
                expectedRevision: 2, command: command
            )),
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
            approvedMoveEnd: moveStart.addingTimeInterval(1_500),
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: pausedAt,
            expiresAt: min(moveStart, pausedAt.addingTimeInterval(86_400))
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

    @Test("an expired saved move intent clears without resuming or moving the paused lease")
    func expiredDeferIntentLeavesExactLeasePaused() async throws {
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
        let moveStart = Self.baseDate.addingTimeInterval(3_600)
        let intent = DayWeavePendingExecutionDeferIntent(
            identity: .init(session: paused), focusedBlockID: Self.blockID,
            sourceStart: Self.baseDate,
            sourceEnd: Self.baseDate.addingTimeInterval(1_800),
            moveStart: moveStart,
            approvedMoveEnd: moveStart.addingTimeInterval(1_780),
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            createdAt: Self.baseDate,
            expiresAt: Self.baseDate.addingTimeInterval(100)
        )
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
        let transport = ExecutionTransportDouble(
            snapshots: [snapshot, snapshot],
            pages: [.init(sessions: [paused], nextOffset: nil)]
        )

        #expect(await Self.controller(
            planner: planner,
            transport: transport,
            now: { Self.baseDate.addingTimeInterval(200) }
        ).refresh() == .success)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == paused)
        #expect(planner.blocks.first?.status == .paused)
        #expect(await transport.receivedCommands().isEmpty)
    }

    @Test("Defer preflight rejects microsecond duration and finish-after-deadline before pausing")
    func deferPreflightRejectsUnsupportedDurationAndEndDeadline() async throws {
        for deadlineOnly in [false, true] {
            let context = try Self.persistenceContext()
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let active = try Self.activeSession(sessionID: Self.uuid(deadlineOnly ? 1_124 : 1_123))
            var block = Self.block()
            block.status = .active
            if !deadlineOnly { block.end.addTimeInterval(0.000_001) }
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
            let outcome = await Self.controller(planner: planner, transport: transport).deferWork(
                Self.blockID,
                moveStart: moveStart,
                latestFinish: deadlineOnly ? moveStart.addingTimeInterval(900) : nil
            )

            #expect(outcome == .invalidLocalState)
            #expect(await transport.receivedCommands().isEmpty)
            #expect(planner.pendingExecutionDeferIntent == nil)
        }
        #expect(dayWeavePostgresEpochMicroseconds(
            Date(timeIntervalSince1970: Double.greatestFiniteMagnitude)
        ) == nil)
        #expect(dayWeavePostgresEpochMicroseconds(
            Date(timeIntervalSince1970: Double(Int64.max) / 1_000_000)
        ) == nil)
    }

    @Test("an exact Defer cannot target another day without a target-day schedule review")
    func deferRejectsCrossDayTargetBeforePause() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sessionID = Self.uuid(1_127)
        let active = try Self.activeSession(sessionID: sessionID)
        var durable = Self.emptyBoundState
        durable.revision = 1
        durable.activeSession = active
        durable.historyWindow = [active]
        durable.historyWindowRevision = 1
        durable.presentedBlockIDs = [Self.blockID]
        var block = Self.block()
        block.status = .active
        let planner = Self.planner(
            persistence: context.persistence,
            blocks: [block], canonicalItems: [try Self.canonicalItem()],
            executionState: durable
        )
        let transport = Self.emptyReadTransport()
        // This remains inside the fixture's 24-hour publication horizon, but
        // lands on the next UTC calendar day. Exact overlap approval is not
        // valid across that day boundary.
        let nextDay = Self.baseDate.addingTimeInterval(18 * 3_600)

        #expect(await Self.controller(planner: planner, transport: transport).deferWork(
            Self.blockID,
            moveStart: nextDay
        ) == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)
        #expect(planner.pendingExecutionDeferIntent == nil)
        #expect(planner.executionState.activeSession == active)
    }

    @Test("Defer rejects a custom recurrence before saving intent or pausing")
    func deferRejectsCustomOccurrenceIdentityBeforeMutation() async throws {
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
        let transport = Self.emptyReadTransport()

        #expect(await Self.controller(planner: planner, transport: transport).deferWork(
            Self.blockID,
            moveStart: Self.baseDate.addingTimeInterval(3_600)
        ) == .invalidLocalState)
        #expect(await transport.receivedCommands().isEmpty)
        #expect(planner.pendingExecutionDeferIntent == nil)
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
        let replacementProof = DayWeavePublishedScheduleBlockProof(
            id: replacementID, itemID: Self.itemID, itemRevision: 1,
            occurrenceID: nil, sessionIndex: 1, start: moveStart, end: moveEnd,
            kind: "pinned"
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
        let sourceProof = DayWeavePublishedScheduleBlockProof(
            id: source.id, itemID: Self.itemID, itemRevision: 1,
            occurrenceID: nil, sessionIndex: 0, start: source.start, end: source.end,
            kind: "planned"
        )
        let siblingProof = DayWeavePublishedScheduleBlockProof(
            id: sibling.id, itemID: Self.itemID, itemRevision: 1,
            occurrenceID: nil, sessionIndex: 1, start: sibling.start, end: sibling.end,
            kind: "pinned"
        )
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
            guard block.syncOrigin == .canonicalPreview,
                  let itemID = block.sourceItemID,
                  let itemRevision = block.sourceItemRevision else { return nil }
            return .init(
                id: block.id,
                itemID: itemID,
                itemRevision: itemRevision,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex ?? 0,
                start: block.start,
                end: block.end,
                kind: block.previewKind ?? "planned"
            )
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

    private static func emptyReadTransport() -> ExecutionTransportDouble {
        let empty = DayWeaveExecutionSnapshot(revision: 0, activeSession: nil)
        return ExecutionTransportDouble(
            snapshots: [empty, empty],
            pages: [.init(sessions: [], nextOffset: nil)]
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
