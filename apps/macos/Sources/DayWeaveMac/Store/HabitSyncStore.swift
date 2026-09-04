import Foundation

struct DayWeaveHabitConnection: Sendable {
    let configurationIdentifier: String
    let transport: any DayWeaveHabitTransport
    let streamTransport: (any DayWeaveHabitStreamTransport)?

    init(
        configurationIdentifier: String,
        transport: any DayWeaveHabitTransport,
        streamTransport: (any DayWeaveHabitStreamTransport)? = nil
    ) {
        self.configurationIdentifier = configurationIdentifier
        self.transport = transport
        self.streamTransport = streamTransport
            ?? (transport as? any DayWeaveHabitStreamTransport)
    }
}

enum HabitSyncPhase: Equatable, Sendable {
    case locked
    case ready
    case syncing
    case online
    case offline
    case authenticationRequired
    case attentionRequired
    case failed
}

struct HabitSyncStatus: Equatable, Sendable {
    let phase: HabitSyncPhase
    let message: String

    var isBusy: Bool { phase == .syncing }
}

enum HabitSyncOutcome: Equatable, Sendable {
    case success
    case notConfigured
    case authenticationRequired
    case conflict
    case offline
    case protocolFailure
    case localStorageFailure
    case configurationChanged
    case unexpectedFailure
}

private enum HabitSyncControllerError: Error, LocalizedError {
    case notConfigured
    case configurationChanged
    case protocolFailure
    case operationInProgress
    case authoritativeOccurrenceUnavailable
    case authoritativeOccurrenceChanged
    case pendingMutationExists
    case pauseUnavailable

    var errorDescription: String? {
        switch self {
        case .notConfigured: "Connect DayWeave in Settings to sync habits."
        case .configurationChanged: "The API connection changed. Private habit data stayed bound to its original connection."
        case .protocolFailure: "The habit service returned data that could not be trusted."
        case .operationInProgress: "Wait for the current habit operation to finish."
        case .authoritativeOccurrenceUnavailable: "This occurrence is not yet in the published server schedule. Local habit controls remain available."
        case .authoritativeOccurrenceChanged: "This occurrence changed while it was open. Review its current progress before saving."
        case .pendingMutationExists: "This habit already has a saved update waiting to sync."
        case .pauseUnavailable: "The habit pause changed. Refresh before trying again."
        }
    }
}

/// A privacy-bound, process-death-safe projection of the canonical habit ledger.
/// Private notes are released into memory only after `activate` and removed at
/// every app-lock/background privacy boundary.
@MainActor
final class HabitSyncStore: ObservableObject {
    static let maximumDeltaPagesPerSync = 1_000
    static let maximumImmediateStreamDrains = 2

    @Published private(set) var occurrences: [DayWeaveHabitOccurrence] = []
    @Published private(set) var pauses: [DayWeaveHabitPause] = []
    @Published private(set) var analytics: [DayWeaveHabitAnalytics] = []
    @Published private(set) var pendingMutations: [DayWeavePendingHabitMutation] = []
    @Published private(set) var status = HabitSyncStatus(
        phase: .locked,
        message: "Unlock DayWeave to load private habit progress."
    )
    @Published private(set) var lastSyncedAt: Date?

    private let persistence: EncryptedHabitPersistence?
    private let connectionProvider: @MainActor @Sendable () throws -> DayWeaveHabitConnection
    private let now: @Sendable () -> Date
    private let makeUUID: @Sendable () -> UUID
    private let streamSleep: @Sendable (Duration) async throws -> Void
    private var snapshot: DayWeaveHabitClientSnapshot?
    private var persistenceRevision = HabitPersistenceRevision.missing
    private var operationID: UUID?
    private var generation: UInt64 = 0
    private var foregroundPollingTask: Task<Void, Never>?
    private var foregroundStreamTask: Task<Void, Never>?
    private var foregroundStreamDrainTask: Task<Void, Never>?
    private var foregroundStreamGeneration: UInt64 = 0
    private var foregroundStreamObservationGeneration: UInt64 = 0
    private var foregroundStreamReconciledGeneration: UInt64 = 0
    private var foregroundStreamLatestHintCursor: String?
    private var foregroundStreamUnavailableForActivation = false
    private var foregroundStreamImmediateAttempts = 0

    init(
        configurationStore: any SuggestionAPIConfigurationStoring =
            UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        authCoordinator: DurableAuthCoordinator? = nil,
        session: URLSession = makeDayWeaveEphemeralSession(),
        persistence: EncryptedHabitPersistence? = try? .applicationDefault(),
        now: @escaping @Sendable () -> Date = Date.init,
        makeUUID: @escaping @Sendable () -> UUID = UUID.init,
        streamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        }
    ) {
        self.persistence = persistence
        self.now = now
        self.makeUUID = makeUUID
        self.streamSleep = streamSleep
        connectionProvider = {
            guard let configuredURL = configurationStore.loadBaseURL() else {
                throw HabitSyncControllerError.notConfigured
            }
            let baseURL = try DayWeaveAPIBaseURL(configuredURL)
            if let authCoordinator {
                let client = DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: session,
                    authCoordinator: authCoordinator
                )
                return DayWeaveHabitConnection(
                    configurationIdentifier: client.configurationIdentifier,
                    transport: client,
                    streamTransport: client
                )
            }
            guard let token = try tokenStore.loadToken(boundTo: baseURL), !token.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            let client = DayWeaveAPIClient(
                baseURL: baseURL,
                session: session,
                bearerToken: token
            )
            return DayWeaveHabitConnection(
                configurationIdentifier: client.configurationIdentifier,
                transport: client,
                streamTransport: client
            )
        }
    }

    init(
        persistence: EncryptedHabitPersistence?,
        connectionProvider: @escaping @MainActor @Sendable () throws -> DayWeaveHabitConnection,
        now: @escaping @Sendable () -> Date = Date.init,
        makeUUID: @escaping @Sendable () -> UUID = UUID.init,
        streamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        }
    ) {
        self.persistence = persistence
        self.connectionProvider = connectionProvider
        self.now = now
        self.makeUUID = makeUUID
        self.streamSleep = streamSleep
    }

    var hasPendingConflict: Bool { pendingMutations.contains(where: \.conflictDetected) }

    func canonicalOccurrence(for block: ScheduleBlock) -> DayWeaveHabitOccurrence? {
        guard block.kind == .habit,
              let habitID = block.sourceItemID,
              let itemRevision = block.sourceItemRevision,
              let plannerOccurrenceID = block.occurrenceID else { return nil }
        return occurrences.first { occurrence in
            occurrence.evidence.habitID == habitID
                && occurrence.evidence.plannerOccurrenceID == plannerOccurrenceID
                && occurrence.evidence.sourceItemRevision == itemRevision
        }
    }

    func openPause(for habitID: UUID) -> DayWeaveHabitPause? {
        pauses
            .filter { $0.habitID == habitID && $0.endedAt == nil }
            .max { $0.revision < $1.revision }
    }

    func analytics(for habitID: UUID) -> DayWeaveHabitAnalytics? {
        analytics.first { $0.habitID == habitID }
    }

    func pendingMutation(forOccurrenceID occurrenceID: UUID) -> DayWeavePendingHabitMutation? {
        pendingMutations.first { mutation in
            guard case let .outcome(value) = mutation else { return false }
            return value.occurrenceID == occurrenceID
        }
    }

    func pendingPauseMutation(forHabitID habitID: UUID) -> DayWeavePendingHabitMutation? {
        pendingMutations.first { mutation in
            switch mutation {
            case .outcome:
                return false
            case let .pauseStart(value):
                return value.habitID == habitID
            case let .pauseResume(value):
                return value.habitID == habitID
            }
        }
    }

    func activate() async -> HabitSyncOutcome {
        guard operationID == nil else { return .unexpectedFailure }
        let operation = makeUUID()
        operationID = operation
        let expectedGeneration = generation
        status = .init(phase: .syncing, message: "Restoring encrypted habit progress…")
        defer { releaseOperation(operation) }

        do {
            guard let persistence else { throw HabitPersistenceError.storageUnavailable }
            let connection = try connectionProvider()
            let loaded = try persistence.loadRevisioned()
            guard generation == expectedGeneration else { throw HabitSyncControllerError.configurationChanged }
            persistenceRevision = loaded.revision
            let restored = loaded.snapshot ?? .empty(at: now())

            if let storedBinding = restored.configurationIdentifier,
               storedBinding != connection.configurationIdentifier,
               !restored.pendingMutations.isEmpty {
                clearInMemoryPrivateData()
                snapshot = nil
                lastSyncedAt = nil
                status = .init(
                    phase: .attentionRequired,
                    message: "Saved habit updates belong to another API connection. Restore that connection before syncing."
                )
                return .configurationChanged
            }

            if restored.configurationIdentifier == connection.configurationIdentifier {
                install(restored)
            } else {
                clearInMemoryPrivateData()
                snapshot = .init(
                    savedAt: now(),
                    configurationIdentifier: connection.configurationIdentifier,
                    deltaCursor: nil,
                    occurrences: [],
                    pauses: [],
                    analytics: [],
                    pendingMutations: []
                )
            }

            try await replayPendingMutations(using: connection, operation: operation)
            try await reconcileDelta(using: connection, operation: operation)
            lastSyncedAt = now()
            status = .init(
                phase: hasPendingConflict ? .attentionRequired : .online,
                message: hasPendingConflict
                    ? "Habit progress refreshed. One saved edit needs your review."
                    : "Habit progress is synced across devices."
            )
            return hasPendingConflict ? .conflict : .success
        } catch {
            return handle(error)
        }
    }

    func sync() async -> HabitSyncOutcome {
        guard snapshot != nil else { return await activate() }
        guard operationID == nil else { return .unexpectedFailure }
        let operation = makeUUID()
        operationID = operation
        status = .init(phase: .syncing, message: "Syncing habit progress…")
        defer { releaseOperation(operation) }
        do {
            let connection = try connectionProvider()
            guard snapshot?.configurationIdentifier == connection.configurationIdentifier else {
                throw HabitSyncControllerError.configurationChanged
            }
            try await replayPendingMutations(using: connection, operation: operation)
            try await reconcileDelta(using: connection, operation: operation)
            lastSyncedAt = now()
            status = .init(
                phase: hasPendingConflict ? .attentionRequired : .online,
                message: hasPendingConflict
                    ? "Synced. Review the saved edit that conflicts with newer progress."
                    : "Habit progress is up to date."
            )
            return hasPendingConflict ? .conflict : .success
        } catch {
            return handle(error)
        }
    }

    func record(
        _ input: DayWeaveHabitOutcomeInput,
        for occurrence: DayWeaveHabitOccurrence
    ) async -> HabitSyncOutcome {
        guard operationID == nil else { return .unexpectedFailure }
        let createdAt = now()
        guard input.hasValidShape,
              let occurredAt = canonicalMutationDate(input.occurredAt, relativeTo: createdAt) else {
            status = .init(
                phase: .attentionRequired,
                message: "Check this Mac’s date and time, progress, quantity, and note before saving."
            )
            return .unexpectedFailure
        }
        guard pendingMutation(forOccurrenceID: occurrence.id) == nil else {
            status = .init(
                phase: .attentionRequired,
                message: HabitSyncControllerError.pendingMutationExists.localizedDescription
            )
            return .conflict
        }
        guard occurrences.first(where: { $0.id == occurrence.id }) == occurrence else {
            status = .init(
                phase: .attentionRequired,
                message: HabitSyncControllerError.authoritativeOccurrenceChanged.localizedDescription
            )
            return .conflict
        }
        let operation = makeUUID()
        let normalizedInput = DayWeaveHabitOutcomeInput(
            status: input.status,
            progressBasisPoints: input.progressBasisPoints,
            quantity: input.quantity,
            unit: input.unit,
            actualSeconds: input.actualSeconds,
            note: input.note,
            occurredAt: occurredAt
        )
        let command = DayWeaveHabitOutcomeCommand(
            operationID: operation,
            expectedRevision: occurrence.outcome?.revision ?? 0,
            outcome: normalizedInput
        )
        let pending = DayWeavePendingHabitMutation.outcome(.init(
            habitID: occurrence.evidence.habitID,
            occurrenceID: occurrence.id,
            idempotencyKey: "habit-occurrence:\(operation.uuidString.lowercased())",
            command: command,
            createdAt: createdAt,
            conflictDetected: false
        ))
        return await enqueueAndExecute(pending)
    }

    func pause(habitID: UUID, at date: Date? = nil) async -> HabitSyncOutcome {
        guard operationID == nil else { return .unexpectedFailure }
        guard pendingPauseMutation(forHabitID: habitID) == nil else {
            status = .init(
                phase: .attentionRequired,
                message: HabitSyncControllerError.pendingMutationExists.localizedDescription
            )
            return .conflict
        }
        guard openPause(for: habitID) == nil else {
            status = .init(phase: .attentionRequired, message: "This habit is already paused.")
            return .conflict
        }
        let createdAt = now()
        guard let startedAt = canonicalMutationDate(date ?? createdAt, relativeTo: createdAt) else {
            status = .init(phase: .attentionRequired, message: "Check this Mac’s date and time before pausing the habit.")
            return .unexpectedFailure
        }
        let operation = makeUUID()
        let pauseID = makeUUID()
        let pending = DayWeavePendingHabitMutation.pauseStart(.init(
            habitID: habitID,
            idempotencyKey: "habit-pause:\(operation.uuidString.lowercased())",
            command: .init(
                operationID: operation,
                pauseID: pauseID,
                startedAt: startedAt
            ),
            createdAt: createdAt,
            conflictDetected: false
        ))
        return await enqueueAndExecute(pending)
    }

    func resume(_ pause: DayWeaveHabitPause, at date: Date? = nil) async -> HabitSyncOutcome {
        guard operationID == nil else { return .unexpectedFailure }
        guard pendingPauseMutation(forHabitID: pause.habitID) == nil else {
            status = .init(
                phase: .attentionRequired,
                message: HabitSyncControllerError.pendingMutationExists.localizedDescription
            )
            return .conflict
        }
        guard pause.endedAt == nil, openPause(for: pause.habitID) == pause else {
            status = .init(
                phase: .attentionRequired,
                message: HabitSyncControllerError.pauseUnavailable.localizedDescription
            )
            return .conflict
        }
        let createdAt = now()
        guard let endedAt = canonicalMutationDate(date ?? createdAt, relativeTo: createdAt),
              endedAt > pause.startedAt else {
            status = .init(
                phase: .attentionRequired,
                message: "Resume time must be after the pause began and close to this Mac’s current time."
            )
            return .unexpectedFailure
        }
        let operation = makeUUID()
        let pending = DayWeavePendingHabitMutation.pauseResume(.init(
            habitID: pause.habitID,
            pauseID: pause.id,
            idempotencyKey: "habit-resume:\(operation.uuidString.lowercased())",
            command: .init(
                operationID: operation,
                expectedRevision: pause.revision,
                endedAt: endedAt
            ),
            createdAt: createdAt,
            conflictDetected: false
        ))
        return await enqueueAndExecute(pending)
    }

    func discardPendingMutation(_ id: UUID) async -> HabitSyncOutcome {
        guard operationID == nil, var candidate = snapshot,
              candidate.pendingMutations.contains(where: { $0.id == id }) else {
            return .unexpectedFailure
        }
        candidate = replacing(
            candidate,
            pendingMutations: candidate.pendingMutations.filter { $0.id != id }
        )
        do {
            try persist(candidate)
            status = .init(phase: .ready, message: "The saved edit was discarded. Server progress is unchanged.")
            return await sync()
        } catch {
            return handle(error)
        }
    }

    func refreshAnalytics(
        habitIDs: [UUID],
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        bucket: DayWeaveHabitAnalyticsBucket
    ) async -> HabitSyncOutcome {
        guard operationID == nil, !habitIDs.isEmpty else { return .unexpectedFailure }
        let uniqueIDs = Array(Set(habitIDs)).sorted { $0.uuidString < $1.uuidString }
        let operation = makeUUID()
        operationID = operation
        status = .init(phase: .syncing, message: "Calculating private habit trends…")
        defer { releaseOperation(operation) }
        do {
            let connection = try connectionProvider()
            guard snapshot?.configurationIdentifier == connection.configurationIdentifier else {
                throw HabitSyncControllerError.configurationChanged
            }
            var fetched: [DayWeaveHabitAnalytics] = []
            for habitID in uniqueIDs {
                fetched.append(try await connection.transport.habitAnalytics(
                    habitID: habitID,
                    startDate: startDate,
                    endDate: endDate,
                    bucket: bucket
                ))
                try assertCurrent(operation: operation, connection: connection)
            }
            guard var candidate = snapshot else { throw HabitSyncControllerError.protocolFailure }
            var indexed = Dictionary(uniqueKeysWithValues: candidate.analytics.map { ($0.habitID, $0) })
            for value in fetched { indexed[value.habitID] = value }
            candidate = replacing(
                candidate,
                analytics: indexed.values.sorted { $0.habitID.uuidString < $1.habitID.uuidString }
            )
            try persist(candidate)
            lastSyncedAt = now()
            status = .init(phase: .online, message: "Habit trends are up to date.")
            return .success
        } catch {
            return handle(error)
        }
    }

    func suspendForPrivacyBoundary() {
        stopForegroundPolling()
        generation &+= 1
        operationID = nil
        clearInMemoryPrivateData()
        snapshot = nil
        persistenceRevision = .missing
        lastSyncedAt = nil
        status = .init(
            phase: .locked,
            message: "Unlock DayWeave to load private habit progress."
        )
    }

    func startForegroundPolling(every interval: Duration) {
        guard foregroundPollingTask == nil else { return }
        foregroundStreamUnavailableForActivation = false
        foregroundStreamGeneration &+= 1
        let pollingGeneration = foregroundStreamGeneration
        foregroundPollingTask = Task { @MainActor [weak self] in
            guard let self else { return }
            self.startForegroundStreamIfReady(generation: pollingGeneration)
            while self.foregroundIsCurrent(pollingGeneration) {
                do {
                    // Polling is intentionally independent of SSE health and
                    // remains the bounded catch-up path when streaming is
                    // unsupported, malformed, or transiently unavailable.
                    try await Task.sleep(for: interval)
                } catch {
                    return
                }
                guard self.foregroundIsCurrent(pollingGeneration) else { return }
                let outcome = await self.sync()
                guard self.foregroundIsCurrent(pollingGeneration) else { return }
                if outcome == .success || outcome == .conflict {
                    self.foregroundStreamImmediateAttempts = 0
                    self.startForegroundStreamIfReady(generation: pollingGeneration)
                }
            }
        }
    }

    func stopForegroundPolling() {
        foregroundPollingTask?.cancel()
        foregroundPollingTask = nil
        foregroundStreamGeneration &+= 1
        foregroundStreamTask?.cancel()
        foregroundStreamTask = nil
        foregroundStreamDrainTask?.cancel()
        foregroundStreamDrainTask = nil
        foregroundStreamObservationGeneration = 0
        foregroundStreamReconciledGeneration = 0
        foregroundStreamLatestHintCursor = nil
        foregroundStreamUnavailableForActivation = false
        foregroundStreamImmediateAttempts = 0
    }

    private func startForegroundStreamIfReady(generation: UInt64) {
        guard foregroundIsCurrent(generation),
              foregroundStreamTask == nil,
              !foregroundStreamUnavailableForActivation,
              let current = snapshot,
              let durableCursor = current.deltaCursor,
              DayWeaveHabitCursorContract.isValidTransportToken(durableCursor),
              let connection = try? connectionProvider(),
              connection.configurationIdentifier == current.configurationIdentifier,
              connectionIsCurrent(connection),
              connection.streamTransport != nil else { return }
        foregroundStreamTask = Task { @MainActor [weak self] in
            await self?.runForegroundStream(generation: generation)
        }
    }

    private func runForegroundStream(generation: UInt64) async {
        var retrySeconds = 1
        defer {
            if generation == foregroundStreamGeneration {
                foregroundStreamTask = nil
            }
        }
        while foregroundIsCurrent(generation) {
            let connection: DayWeaveHabitConnection
            do {
                connection = try connectionProvider()
            } catch {
                return
            }
            guard let current = snapshot,
                  current.configurationIdentifier == connection.configurationIdentifier,
                  connectionIsCurrent(connection),
                  let durableCursor = current.deltaCursor,
                  DayWeaveHabitCursorContract.isValidTransportToken(durableCursor),
                  let streamTransport = connection.streamTransport else { return }
            foregroundStreamImmediateAttempts = 0
            var reconnectDelaySeconds = 1
            do {
                let completion = try await streamTransport.consumeHabitInvalidations(
                    after: durableCursor
                ) { [weak self] cursor in
                    await self?.acceptForegroundStreamHint(
                        cursor,
                        configurationIdentifier: connection.configurationIdentifier,
                        generation: generation
                    )
                }
                guard foregroundIsCurrent(generation) else { return }
                switch completion {
                case .unsupported:
                    foregroundStreamUnavailableForActivation = true
                    return
                case .endOfStream:
                    reconnectDelaySeconds = retrySeconds
                    retrySeconds = min(retrySeconds * 2, 30)
                case .liveEndOfStream:
                    retrySeconds = 1
                    reconnectDelaySeconds = 1
                }
            } catch {
                guard foregroundIsCurrent(generation) else { return }
                guard streamFailureIsTransient(error) else {
                    foregroundStreamUnavailableForActivation = true
                    return
                }
                reconnectDelaySeconds = retrySeconds
                retrySeconds = min(retrySeconds * 2, 30)
            }
            do {
                try await streamSleep(.seconds(reconnectDelaySeconds))
            } catch {
                return
            }
        }
    }

    private func acceptForegroundStreamHint(
        _ cursor: String,
        configurationIdentifier: String,
        generation: UInt64
    ) {
        guard foregroundIsCurrent(generation),
              DayWeaveHabitCursorContract.isValidTransportToken(cursor),
              snapshot?.configurationIdentifier == configurationIdentifier,
              (try? connectionProvider().configurationIdentifier) == configurationIdentifier else {
            return
        }
        if cursor == snapshot?.deltaCursor { return }
        foregroundStreamObservationGeneration &+= 1
        foregroundStreamLatestHintCursor = cursor
        enqueueForegroundStreamDrain(generation: generation)
    }

    private func enqueueForegroundStreamDrain(generation: UInt64) {
        guard foregroundIsCurrent(generation),
              foregroundStreamDrainTask == nil,
              operationID == nil else { return }
        foregroundStreamDrainTask = Task { @MainActor [weak self] in
            await self?.drainForegroundStreamObservations(generation: generation)
        }
    }

    private func drainForegroundStreamObservations(generation: UInt64) async {
        var drainAttempts = 0
        defer {
            if generation == foregroundStreamGeneration {
                foregroundStreamDrainTask = nil
            }
        }
        while foregroundIsCurrent(generation) {
            let targetGeneration = foregroundStreamObservationGeneration
            guard targetGeneration > foregroundStreamReconciledGeneration else { return }
            if foregroundStreamLatestHintCursor == snapshot?.deltaCursor {
                foregroundStreamReconciledGeneration = foregroundStreamObservationGeneration
                foregroundStreamLatestHintCursor = nil
                continue
            }
            guard operationID == nil,
                  drainAttempts < Self.maximumImmediateStreamDrains,
                  foregroundStreamImmediateAttempts < Self.maximumImmediateStreamDrains else {
                return
            }
            drainAttempts += 1
            foregroundStreamImmediateAttempts += 1
            let outcome = await sync()
            guard foregroundIsCurrent(generation),
                  outcome == .success || outcome == .conflict else { return }
            if foregroundStreamLatestHintCursor == snapshot?.deltaCursor {
                // Exact equality is the only relationship inferred between
                // opaque tokens. It also covers hints received in flight.
                foregroundStreamReconciledGeneration = foregroundStreamObservationGeneration
                foregroundStreamLatestHintCursor = nil
            } else {
                // A fully drained authoritative delta covers the observation
                // captured before this request. A newer in-flight observation
                // receives one separately bounded immediate drain.
                foregroundStreamReconciledGeneration = targetGeneration
            }
        }
    }

    private func foregroundIsCurrent(_ generation: UInt64) -> Bool {
        !Task.isCancelled
            && foregroundPollingTask != nil
            && generation == foregroundStreamGeneration
    }

    private func connectionIsCurrent(_ connection: DayWeaveHabitConnection) -> Bool {
        (try? connectionProvider().configurationIdentifier) == connection.configurationIdentifier
            && snapshot?.configurationIdentifier == connection.configurationIdentifier
    }

    private func streamFailureIsTransient(_ error: any Error) -> Bool {
        switch error {
        case let DayWeaveAPIError.transport(code):
            return code != .cancelled
        case let DayWeaveAPIError.server(statusCode, _, _, _):
            return statusCode == 408 || statusCode == 429 || statusCode >= 500
        case let DayWeaveAPIError.durableAuthentication(error):
            switch error {
            case .transport, .retryableServer: return true
            default: return false
            }
        default:
            return false
        }
    }

    private func releaseOperation(_ operation: UUID) {
        if operationID == operation { operationID = nil }
        guard foregroundStreamObservationGeneration > foregroundStreamReconciledGeneration else {
            return
        }
        enqueueForegroundStreamDrain(generation: foregroundStreamGeneration)
    }

    private func enqueueAndExecute(
        _ pending: DayWeavePendingHabitMutation
    ) async -> HabitSyncOutcome {
        guard operationID == nil, pending.hasValidShape, var candidate = snapshot else {
            return .unexpectedFailure
        }
        let operation = pending.id
        operationID = operation
        status = .init(phase: .syncing, message: "Saving this habit update securely…")
        defer { releaseOperation(operation) }
        do {
            let connection = try connectionProvider()
            guard candidate.configurationIdentifier == connection.configurationIdentifier else {
                throw HabitSyncControllerError.configurationChanged
            }
            candidate = replacing(
                candidate,
                pendingMutations: candidate.pendingMutations + [pending]
            )
            try persist(candidate)
            try await execute(pending, using: connection, operation: operation)
            status = .init(phase: .online, message: "Habit progress saved and synced.")
            return .success
        } catch {
            if isConflict(error), let current = snapshot,
               let index = current.pendingMutations.firstIndex(where: { $0.id == pending.id }) {
                var blocked = current.pendingMutations
                blocked[index] = blocked[index].markingConflict()
                do { try persist(replacing(current, pendingMutations: blocked)) } catch { return handle(error) }
                status = .init(
                    phase: .attentionRequired,
                    message: "This habit changed on another device. Review the current progress before replacing it."
                )
                return .conflict
            }
            return handle(error)
        }
    }

    private func replayPendingMutations(
        using connection: DayWeaveHabitConnection,
        operation: UUID
    ) async throws {
        let queued = snapshot?.pendingMutations ?? []
        for pending in queued where !pending.conflictDetected {
            do {
                try await execute(pending, using: connection, operation: operation)
            } catch {
                guard isConflict(error) else { throw error }
                // A conflict discovered while replaying after process death
                // needs the same durable review marker as an immediate write.
                // Keep processing the delta so the user can compare against
                // the authoritative current occurrence or pause.
                try markPendingConflict(pending.id)
            }
        }
    }

    private func markPendingConflict(_ id: UUID) throws {
        guard let current = snapshot,
              let index = current.pendingMutations.firstIndex(where: { $0.id == id }) else {
            throw HabitSyncControllerError.protocolFailure
        }
        var blocked = current.pendingMutations
        blocked[index] = blocked[index].markingConflict()
        try persist(replacing(current, pendingMutations: blocked))
    }

    private func execute(
        _ pending: DayWeavePendingHabitMutation,
        using connection: DayWeaveHabitConnection,
        operation: UUID
    ) async throws {
        switch pending {
        case let .outcome(value):
            let response = try await connection.transport.putHabitOutcome(
                habitID: value.habitID,
                occurrenceID: value.occurrenceID,
                command: value.command,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            guard snapshot?.occurrences.first(where: { $0.id == value.occurrenceID })?
                .evidence == response.occurrence.evidence else {
                throw HabitSyncControllerError.protocolFailure
            }
            try commitMutation(pending.id, occurrence: response.occurrence, pause: nil)
        case let .pauseStart(value):
            let response = try await connection.transport.startHabitPause(
                habitID: value.habitID,
                command: value.command,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            try commitMutation(pending.id, occurrence: nil, pause: response.pause)
        case let .pauseResume(value):
            guard let prior = snapshot?.pauses.first(where: { $0.id == value.pauseID }),
                  prior.endedAt == nil else {
                throw HabitSyncControllerError.protocolFailure
            }
            let response = try await connection.transport.resumeHabitPause(
                habitID: value.habitID,
                pauseID: value.pauseID,
                command: value.command,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            guard response.pause.startedAt == prior.startedAt,
                  response.pause.createdAt == prior.createdAt,
                  response.pause.preservesStreak == prior.preservesStreak else {
                throw HabitSyncControllerError.protocolFailure
            }
            try commitMutation(pending.id, occurrence: nil, pause: response.pause)
        }
    }

    private func commitMutation(
        _ operationID: UUID,
        occurrence: DayWeaveHabitOccurrence?,
        pause: DayWeaveHabitPause?
    ) throws {
        guard var candidate = snapshot,
              candidate.pendingMutations.contains(where: { $0.id == operationID }) else {
            throw HabitSyncControllerError.protocolFailure
        }
        var occurrenceIndex = Dictionary(uniqueKeysWithValues: candidate.occurrences.map { ($0.id, $0) })
        if let occurrence { occurrenceIndex[occurrence.id] = occurrence }
        var pauseIndex = Dictionary(uniqueKeysWithValues: candidate.pauses.map { ($0.id, $0) })
        if let pause { pauseIndex[pause.id] = pause }
        candidate = replacing(
            candidate,
            occurrences: boundedOccurrences(Array(occurrenceIndex.values)),
            pauses: boundedPauses(Array(pauseIndex.values)),
            pendingMutations: candidate.pendingMutations.filter { $0.id != operationID }
        )
        try persist(candidate)
    }

    private func reconcileDelta(
        using connection: DayWeaveHabitConnection,
        operation: UUID
    ) async throws {
        var pages = 0
        while true {
            guard pages < Self.maximumDeltaPagesPerSync,
                  let current = snapshot else { throw HabitSyncControllerError.protocolFailure }
            let page = try await connection.transport.habitDelta(
                cursor: current.deltaCursor,
                limit: 200
            )
            try assertCurrent(operation: operation, connection: connection)
            guard !page.hasMore || page.nextCursor != current.deltaCursor else {
                throw HabitSyncControllerError.protocolFailure
            }
            var occurrenceIndex = Dictionary(uniqueKeysWithValues: current.occurrences.map { ($0.id, $0) })
            var pauseIndex = Dictionary(uniqueKeysWithValues: current.pauses.map { ($0.id, $0) })
            for change in page.changes {
                switch change {
                case let .occurrenceUpsert(value):
                    if let prior = occurrenceIndex[value.id] {
                        let priorRevision = prior.outcome?.revision ?? 0
                        let nextRevision = value.outcome?.revision ?? 0
                        guard value.evidence == prior.evidence,
                              nextRevision > priorRevision || value == prior else {
                            throw HabitSyncControllerError.protocolFailure
                        }
                    }
                    occurrenceIndex[value.id] = value
                case let .pauseUpsert(value):
                    if let prior = pauseIndex[value.id] {
                        guard value.habitID == prior.habitID,
                              value.startedAt == prior.startedAt,
                              value.createdAt == prior.createdAt,
                              value.preservesStreak == prior.preservesStreak,
                              value.revision > prior.revision || value == prior else {
                            throw HabitSyncControllerError.protocolFailure
                        }
                    }
                    pauseIndex[value.id] = value
                }
            }
            let candidate = replacing(
                current,
                deltaCursor: page.nextCursor,
                occurrences: boundedOccurrences(Array(occurrenceIndex.values)),
                pauses: boundedPauses(Array(pauseIndex.values))
            )
            // The complete page and its opaque cursor share one encrypted CAS
            // commit. A crash can replay the page, but can never skip it.
            try persist(candidate)
            pages += 1
            if !page.hasMore { return }
        }
    }

    private func persist(_ candidate: DayWeaveHabitClientSnapshot) throws {
        guard let persistence else { throw HabitPersistenceError.storageUnavailable }
        let saved = replacing(candidate, savedAt: now())
        let revision = try persistence.save(saved, expectedRevision: persistenceRevision)
        persistenceRevision = revision
        install(saved)
    }

    private func install(_ value: DayWeaveHabitClientSnapshot) {
        snapshot = value
        occurrences = value.occurrences
        pauses = value.pauses
        analytics = value.analytics
        pendingMutations = value.pendingMutations
    }

    private func clearInMemoryPrivateData() {
        occurrences = []
        pauses = []
        analytics = []
        pendingMutations = []
    }

    private func assertCurrent(
        operation: UUID,
        connection: DayWeaveHabitConnection
    ) throws {
        guard operationID == operation else { throw CancellationError() }
        let current = try connectionProvider()
        guard current.configurationIdentifier == connection.configurationIdentifier,
              snapshot?.configurationIdentifier == connection.configurationIdentifier else {
            throw HabitSyncControllerError.configurationChanged
        }
    }

    private func replacing(
        _ value: DayWeaveHabitClientSnapshot,
        savedAt: Date? = nil,
        configurationIdentifier: String? = nil,
        deltaCursor: String? = nil,
        occurrences: [DayWeaveHabitOccurrence]? = nil,
        pauses: [DayWeaveHabitPause]? = nil,
        analytics: [DayWeaveHabitAnalytics]? = nil,
        pendingMutations: [DayWeavePendingHabitMutation]? = nil
    ) -> DayWeaveHabitClientSnapshot {
        .init(
            savedAt: savedAt ?? value.savedAt,
            configurationIdentifier: configurationIdentifier ?? value.configurationIdentifier,
            deltaCursor: deltaCursor ?? value.deltaCursor,
            occurrences: occurrences ?? value.occurrences,
            pauses: pauses ?? value.pauses,
            analytics: analytics ?? value.analytics,
            pendingMutations: pendingMutations ?? value.pendingMutations
        )
    }

    private func boundedOccurrences(
        _ values: [DayWeaveHabitOccurrence]
    ) -> [DayWeaveHabitOccurrence] {
        Array(values.sorted {
            if $0.evidence.nominalStart == $1.evidence.nominalStart {
                return $0.id.uuidString < $1.id.uuidString
            }
            return $0.evidence.nominalStart < $1.evidence.nominalStart
        }.suffix(DayWeaveHabitClientSnapshot.maximumOccurrences))
    }

    private func boundedPauses(_ values: [DayWeaveHabitPause]) -> [DayWeaveHabitPause] {
        Array(values.sorted {
            if $0.startedAt == $1.startedAt { return $0.id.uuidString < $1.id.uuidString }
            return $0.startedAt < $1.startedAt
        }.suffix(DayWeaveHabitClientSnapshot.maximumPauses))
    }

    private func canonicalMutationDate(_ value: Date, relativeTo anchor: Date) -> Date? {
        guard let instant = CanonicalRFC3339Instant(date: value) else { return nil }
        let date = instant.dateAtMicrosecondPrecision
        let oldest = anchor.addingTimeInterval(-TimeInterval(366 * 20 * 24 * 60 * 60))
        let newest = anchor.addingTimeInterval(5 * 60)
        guard date >= oldest, date <= newest else { return nil }
        return date
    }

    private func isConflict(_ error: any Error) -> Bool {
        if case let DayWeaveAPIError.server(statusCode, code, _, _) = error {
            return statusCode == 409 && code == "conflict"
        }
        return false
    }

    private func handle(_ error: any Error) -> HabitSyncOutcome {
        let outcome: HabitSyncOutcome
        let phase: HabitSyncPhase
        let message: String
        switch error {
        case HabitSyncControllerError.notConfigured:
            outcome = .notConfigured
            phase = .ready
            message = HabitSyncControllerError.notConfigured.localizedDescription
        case HabitSyncControllerError.configurationChanged:
            // Never retain notes or outcome details in the live projection
            // after the API origin or credential binding changes. The old
            // encrypted snapshot remains untouched for an exact restoration.
            clearInMemoryPrivateData()
            snapshot = nil
            lastSyncedAt = nil
            outcome = .configurationChanged
            phase = .attentionRequired
            message = HabitSyncControllerError.configurationChanged.localizedDescription
        case HabitSyncControllerError.protocolFailure,
             DayWeaveAPIError.responseDecodingFailed,
             DayWeaveAPIError.nonHTTPResponse,
             DayWeaveAPIError.responseTooLarge:
            outcome = .protocolFailure
            phase = .failed
            message = "Habit sync stopped because the server response could not be trusted."
        case is HabitPersistenceError:
            outcome = .localStorageFailure
            phase = .failed
            message = "Encrypted habit storage is unavailable. No update was discarded."
        case DayWeaveAPIError.credentialUnavailable,
             DayWeaveAPIError.durableAuthentication:
            outcome = .authenticationRequired
            phase = .authenticationRequired
            message = "Sign in again to sync saved habit progress."
        case let DayWeaveAPIError.server(statusCode, _, _, _) where statusCode == 401 || statusCode == 403:
            outcome = .authenticationRequired
            phase = .authenticationRequired
            message = "This connection is not authorized to sync habit progress."
        case let DayWeaveAPIError.server(statusCode, _, _, _) where statusCode == 409:
            outcome = .conflict
            phase = .attentionRequired
            message = "Habit progress changed elsewhere. Refresh and review before correcting it."
        case DayWeaveAPIError.transport:
            outcome = .offline
            phase = .offline
            message = "Offline. Saved habit updates will retry without being duplicated."
        default:
            outcome = .unexpectedFailure
            phase = .failed
            message = "Habit sync could not finish. Saved progress was kept for retry."
        }
        status = .init(phase: phase, message: message)
        return outcome
    }
}
