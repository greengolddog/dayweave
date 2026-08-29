import CryptoKit
import Foundation

protocol DayWeaveExecutionTransport: Sendable {
    func executionSnapshot() async throws -> DayWeaveExecutionSnapshot
    func executionHistoryPage(limit: Int, offset: Int) async throws -> DayWeaveExecutionHistoryPage
    func encodedExecutionCommand(_ request: DayWeaveExecutionCommandRequest) throws -> Data
    func applyExecutionCommand(
        encodedRequest: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveExecutionMutation
}

extension DayWeaveAPIClient: DayWeaveExecutionTransport {}

struct DayWeaveExecutionConnection: Sendable {
    let canonicalConfigurationIdentifier: String
    let bindingIdentifier: String
    let transport: any DayWeaveExecutionTransport
}

enum ExecutionSyncOutcome: Equatable, Sendable {
    case success
    case notConfigured
    case authenticationRequired
    case conflict
    case notFound
    case validationFailure
    case transientNetworkFailure
    case retryableServerFailure
    case protocolFailure
    case localStorageFailure
    case invalidLocalState
    case configurationChanged
    case unexpectedFailure
}

enum ExecutionSyncPhase: Equatable, Sendable {
    case notConfigured
    case ready
    case syncing
    case connected
    case offline
    case authenticationRequired
    case failed
}

struct ExecutionSyncStatus: Equatable, Sendable {
    let phase: ExecutionSyncPhase
    let message: String

    var isBusy: Bool { phase == .syncing }
}

private enum ExecutionSyncControllerError: Error {
    case notConfigured
    case invalidLocalState(String)
    case invalidProtocol
    case unstableRead
    case configurationChanged
}

private struct StableExecutionRead {
    let snapshot: DayWeaveExecutionSnapshot
    let history: [DayWeaveExecutionSession]
}

private struct ExecutionCommandSpec {
    let command: DayWeaveExecutionCommand
    let identity: DayWeaveExecutionIdentity
    let priorSession: DayWeaveExecutionSession?
    let focusedBlockID: UUID
    let projectionEligibleAtStart: Bool
}

/// Serializes every execution transition around an encrypted byte-for-byte
/// request fence. The server lease is the only authoritative active timer.
@MainActor
final class ExecutionSyncStore: ObservableObject {
    static let maximumHistoryPages = 1_000
    static let maximumStableReadAttempts = 2

    @Published private(set) var status: ExecutionSyncStatus
    @Published private(set) var isSyncing = false

    private let planner: PlannerStore
    private let connectionProvider: @MainActor @Sendable () throws -> DayWeaveExecutionConnection
    private let now: @Sendable () -> Date
    private let makeUUID: @Sendable () -> UUID
    private var operationID: UUID?
    private var configurationGeneration: UInt64 = 0
    private var foregroundPollingTask: Task<Void, Never>?

    init(
        planner: PlannerStore,
        configurationStore: any SuggestionAPIConfigurationStoring =
            UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        session: URLSession = makeDayWeaveEphemeralSession(),
        now: @escaping @Sendable () -> Date = Date.init,
        makeUUID: @escaping @Sendable () -> UUID = UUID.init
    ) {
        self.planner = planner
        self.now = now
        self.makeUUID = makeUUID
        connectionProvider = {
            guard let configuredURL = configurationStore.loadBaseURL() else {
                throw ExecutionSyncControllerError.notConfigured
            }
            let baseURL = try DayWeaveAPIBaseURL(configuredURL)
            guard let token = try tokenStore.loadToken(boundTo: baseURL), !token.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            let tokenDigest = SHA256.hash(data: Data(token.utf8))
                .map { String(format: "%02x", $0) }
                .joined()
            return DayWeaveExecutionConnection(
                canonicalConfigurationIdentifier: baseURL.canonicalConfigurationIdentifier,
                bindingIdentifier: "execution-v1:\(baseURL.canonicalConfigurationIdentifier):\(tokenDigest)",
                transport: DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: session,
                    bearerToken: token
                )
            )
        }
        status = .init(phase: .ready, message: "Ready to reconcile cross-device execution.")
    }

    init(
        planner: PlannerStore,
        connectionProvider: @escaping @MainActor @Sendable () throws
            -> DayWeaveExecutionConnection,
        now: @escaping @Sendable () -> Date = Date.init,
        makeUUID: @escaping @Sendable () -> UUID = UUID.init
    ) {
        self.planner = planner
        self.connectionProvider = connectionProvider
        self.now = now
        self.makeUUID = makeUUID
        status = .init(phase: .ready, message: "Ready to reconcile cross-device execution.")
    }

    var activeSession: DayWeaveExecutionSession? { planner.executionState.activeSession }

    var expiredBreakChoiceRequired: Bool {
        guard let active = planner.executionState.activeSession,
              active.status == .paused,
              let pauseUntil = active.pauseUntil,
              pauseUntil <= now() else { return false }
        return planner.executionState.acknowledgedExpiredPause
            != .init(sessionID: active.id, revision: active.revision)
    }

    var credentialReplacementIsBlocked: Bool {
        planner.hasExecutionCredentialReplacementBlocker
    }

    func configurationDidChange() {
        configurationGeneration &+= 1
        stopForegroundPolling()
        status = .init(
            phase: .ready,
            message: planner.executionState.pendingCommand == nil
                ? "API settings changed; execution must be reconciled again."
                : "API settings changed; the exact pending command remains fenced."
        )
    }

    func prepareForCredentialReplacement() throws {
        guard operationID == nil else {
            throw PlannerExecutionStateError.credentialReplacementBlocked
        }
        try planner.prepareForExecutionCredentialReplacement()
        configurationGeneration &+= 1
    }

    func refresh() async -> ExecutionSyncOutcome {
        await runExclusive { [self] operationID, generation in
            let connection = try configuredConnection()
            try prepareLocalState(for: connection)
            setBusy("Reconciling cross-device execution…")
            try markHistoryUnverified(binding: connection.bindingIdentifier)
            if planner.executionState.pendingCommand != nil {
                let pendingOutcome = try await reconcilePending(
                    connection: connection,
                    operationID: operationID,
                    generation: generation
                )
                if pendingOutcome != .success { return pendingOutcome }
            }
            let stable = try await readStableHistory(
                connection: connection,
                initialSnapshot: nil,
                operationID: operationID,
                generation: generation
            )
            try persist(stable: stable, binding: connection.bindingIdentifier, pending: nil)
            setConnected("Execution is synchronized across devices")
            return .success
        }
    }

    func start(_ blockID: UUID) async -> ExecutionSyncOutcome {
        await command(blockID: blockID) { snapshot, block, deviceID in
            guard snapshot.activeSession == nil else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "Finish or skip the current cross-device session first."
                )
            }
            guard block.status == .scheduled,
                  let itemID = block.sourceItemID,
                  let itemRevision = block.sourceItemRevision else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "Only a current scheduled canonical block can be started."
                )
            }
            guard !self.executionStartIsBlocked(for: block) else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "This session already ended or its canonical outcome still needs review."
                )
            }
            let sessionID = self.makeUUID()
            let identity = DayWeaveExecutionIdentity(
                sessionID: sessionID,
                itemID: itemID,
                itemRevision: itemRevision,
                occurrenceID: block.occurrenceID,
                sessionIndex: block.sessionIndex ?? 0,
                plannedBlockID: block.id,
                sourceDeviceID: deviceID
            )
            return .init(
                command: .start(
                    sessionID: sessionID,
                    itemID: itemID,
                    itemRevision: itemRevision,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex ?? 0,
                    plannedBlockID: block.id,
                    deviceID: deviceID
                ),
                identity: identity,
                priorSession: nil,
                focusedBlockID: block.id,
                projectionEligibleAtStart:
                    self.planner.canonicalProjectionEligibleAtExecutionStart(block)
            )
        }
    }

    func pause(
        _ blockID: UUID,
        durationSeconds: UInt32? = nil,
        pauseUntil: Date? = nil,
        reason: String? = nil
    ) async -> ExecutionSyncOutcome {
        await command(blockID: blockID) { snapshot, block, _ in
            let currentTime = self.now()
            guard durationSeconds.map({ (1...86_400).contains($0) }) ?? true,
                  durationSeconds == nil || pauseUntil == nil,
                  pauseUntil.map({ $0 > currentTime && $0 <= currentTime.addingTimeInterval(86_400) })
                    ?? true,
                  reason.map({
                      !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                          && $0.unicodeScalars.count <= 500
                  }) ?? true else {
                throw ExecutionSyncControllerError.invalidLocalState("The requested break is invalid.")
            }
            let active = try self.requireActiveSession(snapshot, matching: block)
            return .init(
                command: .pause(
                    sessionID: active.id,
                    durationSeconds: durationSeconds,
                    pauseUntil: pauseUntil,
                    reason: reason
                ),
                identity: .init(session: active),
                priorSession: active,
                focusedBlockID: block.id,
                projectionEligibleAtStart:
                    self.planner.executionState.leaseProjectionEligibility[active.id] ?? false
            )
        }
    }

    func resume(_ blockID: UUID) async -> ExecutionSyncOutcome {
        await command(blockID: blockID) { snapshot, block, _ in
            let active = try self.requireActiveSession(snapshot, matching: block)
            guard active.status == .paused else {
                throw ExecutionSyncControllerError.invalidLocalState("The execution lease is not paused.")
            }
            return .init(
                command: .resume(sessionID: active.id),
                identity: .init(session: active),
                priorSession: active,
                focusedBlockID: block.id,
                projectionEligibleAtStart:
                    self.planner.executionState.leaseProjectionEligibility[active.id] ?? false
            )
        }
    }

    func complete(_ blockID: UUID, actualSeconds: UInt64? = nil) async -> ExecutionSyncOutcome {
        await finish(blockID, status: .completed, actualSeconds: actualSeconds)
    }

    func skip(_ blockID: UUID, actualSeconds: UInt64? = nil) async -> ExecutionSyncOutcome {
        await finish(blockID, status: .skipped, actualSeconds: actualSeconds)
    }

    func keepPausedAfterExpiredBreak() -> ExecutionSyncOutcome {
        guard expiredBreakChoiceRequired,
              let active = planner.executionState.activeSession else { return .invalidLocalState }
        var next = planner.executionState
        next.acknowledgedExpiredPause = .init(sessionID: active.id, revision: active.revision)
        do {
            try planner.persistExecutionState(
                next,
                message: "Break ended; the session remains paused until you resume or finish it"
            )
            return .success
        } catch {
            return report(error)
        }
    }

    func startForegroundPolling(every interval: Duration = .seconds(30)) {
        guard foregroundPollingTask == nil else { return }
        foregroundPollingTask = Task { @MainActor [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                _ = await self.refresh()
                do {
                    try await Task.sleep(for: interval)
                } catch {
                    return
                }
            }
        }
    }

    func stopForegroundPolling() {
        foregroundPollingTask?.cancel()
        foregroundPollingTask = nil
    }

    private func finish(
        _ blockID: UUID,
        status: DayWeaveExecutionStatus,
        actualSeconds: UInt64?
    ) async -> ExecutionSyncOutcome {
        await command(blockID: blockID) { snapshot, block, _ in
            guard actualSeconds.map({ $0 <= UInt64(Int64.max) }) ?? true else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "The corrected duration is outside the supported range."
                )
            }
            let active = try self.requireActiveSession(snapshot, matching: block)
            let command: DayWeaveExecutionCommand = status == .completed
                ? .complete(sessionID: active.id, actualSeconds: actualSeconds)
                : .skip(sessionID: active.id, actualSeconds: actualSeconds)
            return .init(
                command: command,
                identity: .init(session: active),
                priorSession: active,
                focusedBlockID: block.id,
                projectionEligibleAtStart:
                    self.planner.executionState.leaseProjectionEligibility[active.id] ?? false
            )
        }
    }

    private func command(
        blockID: UUID,
        makeSpec: @escaping (
            DayWeaveExecutionSnapshot,
            ScheduleBlock,
            UUID
        ) throws -> ExecutionCommandSpec
    ) async -> ExecutionSyncOutcome {
        await runExclusive { [self] operationID, generation in
            let connection = try configuredConnection()
            try prepareLocalState(for: connection)
            setBusy("Checking the cross-device execution lease…")
            try markHistoryUnverified(binding: connection.bindingIdentifier)
            if planner.executionState.pendingCommand != nil {
                let outcome = try await reconcilePending(
                    connection: connection,
                    operationID: operationID,
                    generation: generation
                )
                if outcome == .success {
                    setConnected("Previous execution command reconciled; review state before retrying")
                }
                return outcome
            }
            let stable = try await readStableHistory(
                connection: connection,
                initialSnapshot: nil,
                operationID: operationID,
                generation: generation
            )
            try persist(stable: stable, binding: connection.bindingIdentifier, pending: nil)
            guard let block = planner.blocks.first(where: { $0.id == blockID }),
                  block.sourceItemID != nil,
                  let deviceID = planner.executionState.deviceID else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "The canonical scheduled block is unavailable."
                )
            }
            let spec = try makeSpec(stable.snapshot, block, deviceID)
            let request = DayWeaveExecutionCommandRequest(
                expectedRevision: stable.snapshot.revision,
                command: spec.command
            )
            let bytes = try connection.transport.encodedExecutionCommand(request)
            let pending = DayWeavePendingExecutionCommand(
                idempotencyKey: "mac-execution-\(makeUUID().uuidString.lowercased())",
                bindingIdentifier: connection.bindingIdentifier,
                expectedRevision: stable.snapshot.revision,
                identity: spec.identity,
                command: spec.command,
                encodedRequest: bytes,
                priorSession: spec.priorSession,
                focusedBlockID: spec.focusedBlockID,
                canonicalProjectionEligibleAtLeaseStart: spec.projectionEligibleAtStart,
                stagedAt: now()
            )
            var staged = planner.executionState
            guard staged.pendingCommand == nil, staged.historyVerified else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "Execution history is not start-safe."
                )
            }
            staged.pendingCommand = pending
            try planner.persistExecutionState(
                staged,
                message: "Saving exact execution command before network I/O"
            )
            let outcome = try await applyPending(
                pending,
                connection: connection,
                operationID: operationID,
                generation: generation
            )
            if outcome != .success { return outcome }
            let reconciled = try await readStableHistory(
                connection: connection,
                initialSnapshot: nil,
                operationID: operationID,
                generation: generation
            )
            try persist(stable: reconciled, binding: connection.bindingIdentifier, pending: nil)
            setConnected("Execution updated across devices")
            return .success
        }
    }

    private func reconcilePending(
        connection: DayWeaveExecutionConnection,
        operationID: UUID,
        generation: UInt64
    ) async throws -> ExecutionSyncOutcome {
        guard let pending = planner.executionState.pendingCommand else { return .success }
        guard pending.bindingIdentifier == connection.bindingIdentifier else {
            throw ExecutionSyncControllerError.configurationChanged
        }
        return try await applyPending(
            pending,
            connection: connection,
            operationID: operationID,
            generation: generation
        )
    }

    private func applyPending(
        _ pending: DayWeavePendingExecutionCommand,
        connection: DayWeaveExecutionConnection,
        operationID: UUID,
        generation: UInt64
    ) async throws -> ExecutionSyncOutcome {
        do {
            let mutation = try await connection.transport.applyExecutionCommand(
                encodedRequest: pending.encodedRequest,
                idempotencyKey: pending.idempotencyKey
            )
            try ensureCurrent(
                connection,
                operationID: operationID,
                generation: generation
            )
            guard validate(mutation: mutation, pending: pending) else {
                throw ExecutionSyncControllerError.invalidProtocol
            }
            var next = planner.executionState
            next.revision = mutation.revision
            next.activeSession = mutation.activeSession
            next.pendingCommand = nil
            next.historyWindow = []
            next.historyWindowRevision = nil
            next.historyContinuityEstablished = false
            next.historyVerified = false
            next.acknowledgedExpiredPause = nil
            if mutation.changedSession.status.isOpen {
                next.leaseProjectionEligibility[mutation.changedSession.id] =
                    pending.canonicalProjectionEligibleAtLeaseStart
            } else {
                let projection: DayWeaveTerminalProjectionState =
                    pending.canonicalProjectionEligibleAtLeaseStart ? .pending : .notRequired
                next.terminalOutcomes[mutation.changedSession.id] = .init(
                    session: mutation.changedSession,
                    recordedAt: now(),
                    projection: projection
                )
                next.leaseProjectionEligibility.removeValue(forKey: mutation.changedSession.id)
            }
            try planner.persistExecutionState(
                next,
                message: mutation.replayed
                    ? "Recovered the exact execution command after an interrupted response"
                    : "Execution command confirmed by the server",
                reconcilePresentation: true
            )
            return .success
        } catch let error as DayWeaveAPIError {
            guard case let .server(statusCode, _, _, _) = error,
                  [400, 404, 409, 422].contains(statusCode) else { throw error }
            return try await reconcileRejectedPending(
                pending,
                statusCode: statusCode,
                connection: connection,
                operationID: operationID,
                generation: generation
            )
        }
    }

    private func reconcileRejectedPending(
        _ pending: DayWeavePendingExecutionCommand,
        statusCode: Int,
        connection: DayWeaveExecutionConnection,
        operationID: UUID,
        generation: UInt64
    ) async throws -> ExecutionSyncOutcome {
        let stable = try await readStableHistory(
            connection: connection,
            initialSnapshot: nil,
            operationID: operationID,
            generation: generation
        )
        let matching = stable.history.filter { pending.identity.matches($0) }
        guard matching.count <= 1 else { throw ExecutionSyncControllerError.invalidProtocol }
        let appliedOrSuperseded: Bool
        if let current = matching.first {
            if case .start = pending.command {
                appliedOrSuperseded = true
            } else if let prior = pending.priorSession {
                guard current.revision >= prior.revision else {
                    throw ExecutionSyncControllerError.invalidProtocol
                }
                appliedOrSuperseded = current.revision > prior.revision
                if current.revision == prior.revision, current != prior {
                    throw ExecutionSyncControllerError.invalidProtocol
                }
            } else {
                throw ExecutionSyncControllerError.invalidProtocol
            }
        } else {
            guard case .start = pending.command else {
                throw ExecutionSyncControllerError.invalidProtocol
            }
            appliedOrSuperseded = false
        }
        try persist(stable: stable, binding: connection.bindingIdentifier, pending: nil)
        if appliedOrSuperseded { return .success }
        return switch statusCode {
        case 404: .notFound
        case 409: .conflict
        case 400, 422: .validationFailure
        default: .unexpectedFailure
        }
    }

    private func readStableHistory(
        connection: DayWeaveExecutionConnection,
        initialSnapshot: DayWeaveExecutionSnapshot?,
        operationID: UUID,
        generation: UInt64
    ) async throws -> StableExecutionRead {
        var before: DayWeaveExecutionSnapshot
        if let initialSnapshot {
            before = initialSnapshot
        } else {
            before = try await connection.transport.executionSnapshot()
        }
        try ensureCurrent(connection, operationID: operationID, generation: generation)
        for _ in 0..<Self.maximumStableReadAttempts {
            var history: [DayWeaveExecutionSession] = []
            var offset = 0
            var pages = 0
            while true {
                guard pages < Self.maximumHistoryPages else {
                    throw ExecutionSyncControllerError.invalidProtocol
                }
                let page = try await connection.transport.executionHistoryPage(
                    limit: DayWeaveAPIClient.maximumExecutionHistoryLimit,
                    offset: offset
                )
                try ensureCurrent(connection, operationID: operationID, generation: generation)
                let expected = offset.addingReportingOverflow(page.sessions.count)
                guard !expected.overflow,
                      page.sessions.count <= DayWeaveAPIClient.maximumExecutionHistoryLimit,
                      page.nextOffset.map({
                          page.sessions.count == DayWeaveAPIClient.maximumExecutionHistoryLimit
                              && $0 == expected.partialValue && $0 > offset
                      }) ?? true else {
                    throw ExecutionSyncControllerError.invalidProtocol
                }
                history.append(contentsOf: page.sessions)
                pages += 1
                guard let nextOffset = page.nextOffset else { break }
                offset = nextOffset
            }
            let after = try await connection.transport.executionSnapshot()
            try ensureCurrent(connection, operationID: operationID, generation: generation)
            if before == after {
                guard validateStable(snapshot: after, history: history) else {
                    throw ExecutionSyncControllerError.invalidProtocol
                }
                return .init(snapshot: after, history: history)
            }
            before = after
        }
        throw ExecutionSyncControllerError.unstableRead
    }

    private func persist(
        stable: StableExecutionRead,
        binding: String,
        pending: DayWeavePendingExecutionCommand?
    ) throws {
        var next = planner.executionState
        let reconciledFence = next.pendingCommand
        guard next.bindingIdentifier == binding,
              stable.snapshot.revision >= next.revision else {
            throw ExecutionSyncControllerError.configurationChanged
        }
        let remoteByID = Dictionary(uniqueKeysWithValues: stable.history.map { ($0.id, $0) })
        if stable.snapshot.revision == next.revision, next.pendingCommand == nil,
           stable.snapshot.activeSession != next.activeSession {
            throw ExecutionSyncControllerError.invalidProtocol
        }
        if let cachedActive = next.activeSession {
            guard let remote = remoteByID[cachedActive.id],
                  DayWeaveExecutionIdentity(session: remote)
                    == DayWeaveExecutionIdentity(session: cachedActive),
                  remote.startedAt == cachedActive.startedAt,
                  remote.createdAt == cachedActive.createdAt,
                  remote.revision >= cachedActive.revision,
                  remote.accumulatedSeconds >= cachedActive.accumulatedSeconds,
                  remote.revision != cachedActive.revision || remote == cachedActive else {
                throw ExecutionSyncControllerError.invalidProtocol
            }
        }
        for (sessionID, old) in next.terminalOutcomes {
            guard remoteByID[sessionID] == old.session else {
                throw ExecutionSyncControllerError.invalidProtocol
            }
        }
        var outcomes: [UUID: DayWeaveTerminalExecutionOutcome] = [:]
        for session in stable.history where !session.status.isOpen {
            if let existing = next.terminalOutcomes[session.id] {
                outcomes[session.id] = existing
            } else {
                let eligible = next.leaseProjectionEligibility[session.id]
                    ?? next.pendingCommand.flatMap {
                        $0.identity.sessionID == session.id
                            ? $0.canonicalProjectionEligibleAtLeaseStart : nil
                    }
                    ?? false
                outcomes[session.id] = .init(
                    session: session,
                    recordedAt: now(),
                    projection: eligible ? .pending : .notRequired
                )
            }
        }
        next.revision = stable.snapshot.revision
        next.activeSession = stable.snapshot.activeSession
        next.historyWindow = Array(stable.history.prefix(
            DayWeaveAPIClient.maximumExecutionHistoryLimit
        ))
        next.historyWindowRevision = stable.snapshot.revision
        next.historyContinuityEstablished = true
        next.historyVerified = true
        next.pendingCommand = pending
        next.terminalOutcomes = outcomes
        if let active = stable.snapshot.activeSession,
           let fenced = pending ?? reconciledFence,
           fenced.identity.sessionID == active.id {
            next.leaseProjectionEligibility[active.id] =
                fenced.canonicalProjectionEligibleAtLeaseStart
        }
        next.leaseProjectionEligibility = next.leaseProjectionEligibility.filter {
            stable.snapshot.activeSession?.id == $0.key
        }
        if let active = stable.snapshot.activeSession,
           next.acknowledgedExpiredPause
            != .init(sessionID: active.id, revision: active.revision) {
            next.acknowledgedExpiredPause = nil
        } else if stable.snapshot.activeSession == nil {
            next.acknowledgedExpiredPause = nil
        }
        try planner.persistExecutionState(
            next,
            message: "Execution is synchronized across devices",
            reconcilePresentation: true
        )
    }

    private func validateStable(
        snapshot: DayWeaveExecutionSnapshot,
        history: [DayWeaveExecutionSession]
    ) -> Bool {
        guard Set(history.map(\.id)).count == history.count,
              zip(history, history.dropFirst()).allSatisfy({ newer, older in
                  newer.updatedAt > older.updatedAt
                      || (newer.updatedAt == older.updatedAt
                          && newer.id.uuidString.lowercased()
                              > older.id.uuidString.lowercased())
              }),
              history.allSatisfy({ $0.revision <= snapshot.revision }) else { return false }
        var revisionSum: UInt64 = 0
        for session in history {
            let addition = revisionSum.addingReportingOverflow(session.revision)
            guard !addition.overflow else { return false }
            revisionSum = addition.partialValue
        }
        guard revisionSum == snapshot.revision,
              (snapshot.revision == 0) == history.isEmpty else { return false }
        let open = history.filter { $0.status.isOpen }
        if let active = snapshot.activeSession {
            guard open.count == 1, open[0] == active else { return false }
        } else if !open.isEmpty {
            return false
        }
        return true
    }

    private func validate(
        mutation: DayWeaveExecutionMutation,
        pending: DayWeavePendingExecutionCommand
    ) -> Bool {
        let global = pending.expectedRevision.addingReportingOverflow(1)
        guard !global.overflow,
              mutation.revision == global.partialValue,
              pending.identity.matches(mutation.changedSession) else { return false }
        let changed = mutation.changedSession
        let expectedStatus: DayWeaveExecutionStatus = switch pending.command {
        case .start, .resume: .active
        case .pause: .paused
        case .complete: .completed
        case .skip: .skipped
        }
        guard changed.status == expectedStatus,
              expectedStatus.isOpen ? mutation.activeSession == changed : mutation.activeSession == nil
        else { return false }
        if case .start = pending.command {
            return pending.priorSession == nil
                && changed.revision == 1
                && changed.accumulatedSeconds == 0
        }
        guard let prior = pending.priorSession,
              prior.status.isOpen,
              DayWeaveExecutionIdentity(session: prior) == pending.identity,
              changed.revision == prior.revision + 1,
              changed.startedAt == prior.startedAt,
              changed.createdAt == prior.createdAt else { return false }
        let elapsed = executionElapsedSeconds(prior, at: changed.updatedAt)
        switch pending.command {
        case let .pause(_, duration, absolute, reason):
            let expectedUntil = duration.map {
                changed.updatedAt.addingTimeInterval(TimeInterval($0))
            } ?? absolute
            return changed.accumulatedSeconds == elapsed
                && changed.pausedAt == (prior.pausedAt ?? changed.updatedAt)
                && changed.pauseUntil == expectedUntil
                && changed.pauseReason == (reason ?? prior.pauseReason)
        case .resume:
            return prior.status == .paused
                && changed.accumulatedSeconds == prior.accumulatedSeconds
        case let .complete(_, corrected), let .skip(_, corrected):
            return changed.accumulatedSeconds == elapsed
                && changed.actualSeconds == (corrected ?? elapsed)
                && changed.pausedAt == (prior.pausedAt
                    ?? (prior.status == .paused ? changed.updatedAt : nil))
        case .start:
            return false
        }
    }

    private func executionElapsedSeconds(
        _ session: DayWeaveExecutionSession,
        at date: Date
    ) -> UInt64 {
        let running: UInt64
        if let since = session.runningSince {
            running = UInt64(max(0, floor(date.timeIntervalSince(since))))
        } else {
            running = 0
        }
        let total = session.accumulatedSeconds.addingReportingOverflow(running)
        return total.overflow ? UInt64(Int64.max) : min(total.partialValue, UInt64(Int64.max))
    }

    private func requireActiveSession(
        _ snapshot: DayWeaveExecutionSnapshot,
        matching block: ScheduleBlock
    ) throws -> DayWeaveExecutionSession {
        guard let active = snapshot.activeSession else {
            throw ExecutionSyncControllerError.invalidLocalState(
                "No canonical execution lease is open."
            )
        }
        guard active.itemID == block.sourceItemID,
              active.itemRevision == block.sourceItemRevision,
              active.occurrenceID == block.occurrenceID,
              active.sessionIndex == (block.sessionIndex ?? 0) else {
            throw ExecutionSyncControllerError.invalidLocalState(
                "Another device owns a different execution session."
            )
        }
        return active
    }

    private func executionStartIsBlocked(for block: ScheduleBlock) -> Bool {
        guard planner.executionState.historyVerified else { return true }
        if !planner.pendingCanonicalMutations.isEmpty { return true }
        return planner.executionState.terminalOutcomes.values.contains { outcome in
            let session = outcome.session
            return session.itemID == block.sourceItemID
                && session.itemRevision == block.sourceItemRevision
                && session.occurrenceID == block.occurrenceID
                && session.sessionIndex == (block.sessionIndex ?? 0)
        }
    }

    private func prepareLocalState(for connection: DayWeaveExecutionConnection) throws {
        try planner.prepareExecutionBinding(
            connection.bindingIdentifier,
            canonicalConfigurationIdentifier: connection.canonicalConfigurationIdentifier
        )
    }

    private func markHistoryUnverified(binding: String) throws {
        var next = planner.executionState
        guard next.bindingIdentifier == binding else {
            throw ExecutionSyncControllerError.configurationChanged
        }
        next.historyVerified = false
        next.historyContinuityEstablished = false
        try planner.persistExecutionState(
            next,
            message: "Verifying complete execution history before enabling starts"
        )
    }

    private func configuredConnection() throws -> DayWeaveExecutionConnection {
        try connectionProvider()
    }

    private func ensureCurrent(
        _ connection: DayWeaveExecutionConnection,
        operationID: UUID,
        generation: UInt64
    ) throws {
        guard !Task.isCancelled,
              self.operationID == operationID,
              configurationGeneration == generation else {
            throw ExecutionSyncControllerError.configurationChanged
        }
        let current = try configuredConnection()
        guard current.bindingIdentifier == connection.bindingIdentifier,
              current.canonicalConfigurationIdentifier
                == connection.canonicalConfigurationIdentifier else {
            throw ExecutionSyncControllerError.configurationChanged
        }
    }

    private func runExclusive(
        _ operation: @escaping (UUID, UInt64) async throws -> ExecutionSyncOutcome
    ) async -> ExecutionSyncOutcome {
        guard operationID == nil else { return .invalidLocalState }
        guard planner.canPersistPlan, planner.hasEncryptedPersistence else {
            return report(PlannerExecutionStateError.encryptedPersistenceRequired)
        }
        guard planner.beginCanonicalSync() else { return .invalidLocalState }
        let id = UUID()
        let generation = configurationGeneration
        operationID = id
        isSyncing = true
        defer {
            if operationID == id {
                operationID = nil
                isSyncing = false
            }
            planner.endCanonicalSync()
        }
        do {
            return try await operation(id, generation)
        } catch {
            return report(error)
        }
    }

    @discardableResult
    private func report(_ error: Error) -> ExecutionSyncOutcome {
        let phase: ExecutionSyncPhase
        let message: String
        let outcome: ExecutionSyncOutcome
        switch error {
        case ExecutionSyncControllerError.notConfigured:
            phase = .notConfigured
            message = "Configure the DayWeave API before canonical execution."
            outcome = .notConfigured
        case DayWeaveAPIError.credentialUnavailable:
            phase = .authenticationRequired
            message = planner.executionState.pendingCommand == nil
                ? "Enter the bearer token to reconcile execution."
                : "Authentication failed; the exact pending command remains fenced."
            outcome = .authenticationRequired
        case let DayWeaveAPIError.server(statusCode, _, _, _):
            if statusCode == 401 {
                phase = .authenticationRequired
                message = "Authentication failed; any exact pending command was retained."
                outcome = .authenticationRequired
            } else if statusCode == 408 || statusCode == 429 || statusCode >= 500 {
                phase = .offline
                message = "The execution API is temporarily unavailable; exact pending bytes were retained."
                outcome = .retryableServerFailure
            } else {
                phase = .failed
                message = "The execution API rejected the request; no local success was invented."
                outcome = .unexpectedFailure
            }
        case DayWeaveAPIError.transport:
            phase = .offline
            message = planner.executionState.pendingCommand == nil
                ? "Offline; canonical execution was not changed locally."
                : "Offline; the exact execution command remains fenced for replay."
            outcome = .transientNetworkFailure
        case DayWeaveAPIError.responseDecodingFailed,
             DayWeaveAPIError.nonHTTPResponse,
             DayWeaveAPIError.responseTooLarge,
             ExecutionSyncControllerError.invalidProtocol:
            phase = .failed
            message = "The server execution response is incompatible; no local success was invented."
            outcome = .protocolFailure
        case ExecutionSyncControllerError.unstableRead:
            phase = .failed
            message = "Execution changed during reconciliation; retry after it settles."
            outcome = .conflict
        case ExecutionSyncControllerError.configurationChanged,
             PlannerExecutionStateError.configurationMismatch,
             PlannerExecutionStateError.credentialReplacementBlocked:
            phase = .failed
            message = "API credentials changed while execution state was fenced."
            outcome = .configurationChanged
        case let ExecutionSyncControllerError.invalidLocalState(reason):
            phase = .failed
            message = reason
            outcome = .invalidLocalState
        case is PlannerPersistenceError,
             PlannerExecutionStateError.encryptedPersistenceRequired:
            phase = .failed
            message = "Encrypted planner storage is unavailable; execution is disabled."
            outcome = .localStorageFailure
        case DayWeaveAPIError.requestEncodingFailed,
             DayWeaveAPIError.invalidEndpoint,
             PlannerExecutionStateError.invalidDurableState:
            phase = .failed
            message = "The local execution request is invalid; no network action was attempted."
            outcome = .invalidLocalState
        default:
            phase = .failed
            message = "Canonical execution could not be reconciled safely."
            outcome = .unexpectedFailure
        }
        status = .init(phase: phase, message: message)
        return outcome
    }

    private func setBusy(_ message: String) {
        status = .init(phase: .syncing, message: message)
    }

    private func setConnected(_ message: String) {
        status = .init(phase: .connected, message: message)
    }
}
