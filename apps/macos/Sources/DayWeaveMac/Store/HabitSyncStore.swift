import CryptoKit
import Foundation

/// An immutable, read-only scheduling projection of the private habit cache.
/// Mutation commands and private notes never cross this boundary.
struct HabitCompositionCheckpoint: Equatable, Sendable {
    let configurationIdentifier: String?
    let deltaCursor: String?
    let deltaCaughtUp: Bool
    let occurrences: [Occurrence]
    let pauses: [Pause]
    let pendingMutationIDs: [UUID]
    let hasActiveOperation: Bool
    /// Monotonic in-memory fence. It is intentionally excluded from the
    /// durable fingerprint but detects a sync that starts and finishes while
    /// a helper composition is in flight.
    let operationGeneration: UInt64

    struct Occurrence: Codable, Equatable, Sendable {
        let id: UUID
        let habitID: UUID
        let plannerOccurrenceID: UUID
        let sourceItemRevision: UInt64
        let nominalStart: Date
        let windowStart: Date
        let windowEnd: Date
        let expectedDurationSeconds: UInt64?
        let outcome: Outcome?
    }

    struct Outcome: Codable, Equatable, Sendable {
        let revision: UInt64
        let status: DayWeaveHabitOutcomeStatus
        let progressBasisPoints: UInt16
        let occurredAt: Date
    }

    struct Pause: Codable, Equatable, Sendable {
        let id: UUID
        let habitID: UUID
        let revision: UInt64
        let startedAt: Date
        let endedAt: Date?
    }

    /// Content-free proof stored beside a local schedule. Its payload contains
    /// only fields consumed by composition and is deterministically ordered.
    var fingerprint: String? {
        let payload = FingerprintPayload(
            configurationIdentifier: configurationIdentifier,
            deltaCursor: deltaCursor,
            deltaCaughtUp: deltaCaughtUp,
            occurrences: occurrences.sorted {
                $0.id.uuidString < $1.id.uuidString
            },
            pauses: pauses.sorted { $0.id.uuidString < $1.id.uuidString },
            pendingMutationIDs: pendingMutationIDs.sorted { $0.uuidString < $1.uuidString }
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let bytes = try? encoder.encode(payload) else { return nil }
        return "habit-sha256:" + SHA256.hash(data: bytes).map {
            String(format: "%02x", $0)
        }.joined()
    }

    /// Shared fail-closed authority predicate for composition and execution.
    /// Historical rows for deleted habits remain harmless, while evidence for
    /// a currently active habit may never claim a revision the canonical cache
    /// has not observed.
    func isAuthoritative(
        for configurationIdentifier: String,
        activeHabitRevisions: [UUID: UInt64]
    ) -> Bool {
        guard self.configurationIdentifier == configurationIdentifier,
              deltaCaughtUp,
              let deltaCursor,
              DayWeaveHabitCursorContract.isValidTransportToken(deltaCursor),
              pendingMutationIDs.isEmpty,
              !hasActiveOperation,
              fingerprint != nil else { return false }
        return occurrences.allSatisfy { occurrence in
            guard let canonicalRevision = activeHabitRevisions[occurrence.habitID] else {
                return true
            }
            return occurrence.sourceItemRevision <= canonicalRevision
        }
    }

    private struct FingerprintPayload: Encodable {
        let configurationIdentifier: String?
        let deltaCursor: String?
        let deltaCaughtUp: Bool
        let occurrences: [Occurrence]
        let pauses: [Pause]
        let pendingMutationIDs: [UUID]
    }
}

@MainActor
protocol HabitCompositionCheckpointProviding: AnyObject {
    var habitCompositionCheckpoint: HabitCompositionCheckpoint { get }
    func observeHabitCompositionCheckpointChanges(
        _ observer: @escaping @MainActor () -> Void
    )
}

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
final class HabitSyncStore: ObservableObject, HabitCompositionCheckpointProviding {
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
    /// Set synchronously when SSE reports a different opaque cursor. The
    /// durable snapshot is also marked incomplete, but this in-memory fence
    /// closes the interval before that write and survives a write failure.
    private var foregroundStreamInvalidationPending = false
    private var compositionCheckpointObservers: [@MainActor () -> Void] = []
    private var habitCompositionOperationGeneration: UInt64 = 0

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

    var habitCompositionCheckpoint: HabitCompositionCheckpoint {
        HabitCompositionCheckpoint(
            configurationIdentifier: snapshot?.configurationIdentifier,
            deltaCursor: snapshot?.deltaCursor,
            deltaCaughtUp: (snapshot?.deltaCaughtUp ?? false)
                && !foregroundStreamInvalidationPending,
            occurrences: (snapshot?.occurrences ?? []).map { occurrence in
                .init(
                    id: occurrence.id,
                    habitID: occurrence.evidence.habitID,
                    plannerOccurrenceID: occurrence.evidence.plannerOccurrenceID,
                    sourceItemRevision: occurrence.evidence.sourceItemRevision,
                    nominalStart: occurrence.evidence.nominalStart,
                    windowStart: occurrence.evidence.windowStart,
                    windowEnd: occurrence.evidence.windowEnd,
                    expectedDurationSeconds: occurrence.evidence.expectedDurationSeconds,
                    outcome: occurrence.outcome.map {
                        .init(
                            revision: $0.revision,
                            status: $0.status,
                            progressBasisPoints: $0.progressBasisPoints,
                            occurredAt: $0.occurredAt
                        )
                    }
                )
            },
            pauses: (snapshot?.pauses ?? []).map {
                .init(
                    id: $0.id,
                    habitID: $0.habitID,
                    revision: $0.revision,
                    startedAt: $0.startedAt,
                    endedAt: $0.endedAt
                )
            },
            pendingMutationIDs: (snapshot?.pendingMutations ?? []).map(\.id),
            hasActiveOperation: operationID != nil,
            operationGeneration: habitCompositionOperationGeneration
        )
    }

    func observeHabitCompositionCheckpointChanges(
        _ observer: @escaping @MainActor () -> Void
    ) {
        compositionCheckpointObservers.append(observer)
    }

    func canonicalOccurrence(for block: ScheduleBlock) -> DayWeaveHabitOccurrence? {
        guard block.kind == .habit,
              let habitID = block.sourceItemID,
              let itemRevision = block.sourceItemRevision,
              let plannerOccurrenceID = block.occurrenceID else { return nil }
        return occurrences.first { occurrence in
            occurrence.evidence.habitID == habitID
                && occurrence.evidence.plannerOccurrenceID == plannerOccurrenceID
                && occurrence.evidence.sourceItemRevision <= itemRevision
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
        beginOperation(operation)
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
                install(nil)
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
                install(.init(
                    savedAt: now(),
                    configurationIdentifier: connection.configurationIdentifier,
                    deltaCursor: nil,
                    deltaCaughtUp: false,
                    occurrences: [],
                    pauses: [],
                    analytics: [],
                    pendingMutations: []
                ))
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
        beginOperation(operation)
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
            // A reviewed conflict may have been restored from a snapshot
            // written by an older client. Removing the final journal is not
            // itself proof that the server ledger is terminally caught up.
            deltaCaughtUp: false,
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
        beginOperation(operation)
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
        let priorCheckpoint = habitCompositionCheckpoint
        stopForegroundPolling()
        generation &+= 1
        habitCompositionOperationGeneration &+= 1
        operationID = nil
        foregroundStreamInvalidationPending = false
        clearInMemoryPrivateData()
        install(nil)
        persistenceRevision = .missing
        lastSyncedAt = nil
        status = .init(
            phase: .locked,
            message: "Unlock DayWeave to load private habit progress."
        )
        notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
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
              current.deltaCaughtUp,
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
                  current.deltaCaughtUp,
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
        let priorCheckpoint = habitCompositionCheckpoint
        foregroundStreamObservationGeneration &+= 1
        foregroundStreamLatestHintCursor = cursor
        foregroundStreamInvalidationPending = true
        if let current = snapshot, current.deltaCaughtUp {
            // The hint is never installed as a cursor. It only revokes the
            // terminal verdict until an authoritative delta reaches a terminal
            // page. Persisting that revocation makes process death fail closed.
            do {
                try persist(replacing(current, deltaCaughtUp: false))
            } catch {
                // The process-local flag above still prevents composition. The
                // normal drain reports/retries the underlying storage failure.
            }
        }
        notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
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
                reconcileForegroundInvalidations(
                    through: foregroundStreamObservationGeneration
                )
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
                reconcileForegroundInvalidations(
                    through: foregroundStreamObservationGeneration
                )
            } else {
                // A fully drained authoritative delta covers the observation
                // captured before this request. A newer in-flight observation
                // receives one separately bounded immediate drain.
                reconcileForegroundInvalidations(through: targetGeneration)
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
        let priorCheckpoint = habitCompositionCheckpoint
        if operationID == operation { operationID = nil }
        notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
        guard foregroundStreamObservationGeneration > foregroundStreamReconciledGeneration else {
            return
        }
        enqueueForegroundStreamDrain(generation: foregroundStreamGeneration)
    }

    private func beginOperation(_ operation: UUID) {
        let priorCheckpoint = habitCompositionCheckpoint
        habitCompositionOperationGeneration &+= 1
        operationID = operation
        notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
    }

    private func enqueueAndExecute(
        _ pending: DayWeavePendingHabitMutation
    ) async -> HabitSyncOutcome {
        guard operationID == nil, pending.hasValidShape, var candidate = snapshot else {
            return .unexpectedFailure
        }
        let operation = pending.id
        beginOperation(operation)
        status = .init(phase: .syncing, message: "Saving this habit update securely…")
        defer { releaseOperation(operation) }
        do {
            let connection = try connectionProvider()
            guard candidate.configurationIdentifier == connection.configurationIdentifier else {
                throw HabitSyncControllerError.configurationChanged
            }
            candidate = replacing(
                candidate,
                analytics: candidate.analytics.filter { $0.habitID != pending.habitID },
                pendingMutations: candidate.pendingMutations + [pending]
            )
            try persist(candidate)
            try await execute(pending, using: connection, operation: operation)
            status = .init(phase: .online, message: "Habit progress saved and synced.")
            return .success
        } catch {
            if isConflict(error), let current = snapshot,
               let index = current.pendingMutations.firstIndex(where: { $0.id == pending.id }) {
                do { try markPendingConflict(current.pendingMutations[index].id) }
                catch { return handle(error) }
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
        try persist(replacing(
            current,
            deltaCaughtUp: false,
            pendingMutations: blocked
        ))
    }

    private func execute(
        _ pending: DayWeavePendingHabitMutation,
        using connection: DayWeaveHabitConnection,
        operation: UUID
    ) async throws {
        switch pending {
        case let .outcome(value):
            guard let prior = snapshot?.occurrences.first(where: {
                $0.id == value.occurrenceID
            }),
                prior.evidence.habitID == value.habitID,
                (prior.outcome?.revision ?? 0) == value.command.expectedRevision else {
                throw HabitSyncControllerError.protocolFailure
            }
            let response = try await connection.transport.putHabitOutcome(
                habitID: value.habitID,
                occurrenceID: value.occurrenceID,
                command: value.command,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            let nextRevision = value.command.expectedRevision.addingReportingOverflow(1)
            guard !nextRevision.overflow,
                  prior.evidence == response.occurrence.evidence,
                  let received = response.occurrence.outcome,
                  received.revision == nextRevision.partialValue,
                  received.input == value.command.outcome else {
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
            let nextRevision = value.command.expectedRevision.addingReportingOverflow(1)
            guard response.pause.id == value.command.pauseID,
                  response.pause.habitID == value.habitID,
                  !nextRevision.overflow,
                  response.pause.revision == nextRevision.partialValue,
                  response.pause.startedAt == value.command.startedAt,
                  response.pause.endedAt == nil else {
                throw HabitSyncControllerError.protocolFailure
            }
            try commitMutation(pending.id, occurrence: nil, pause: response.pause)
        case let .pauseResume(value):
            guard let prior = snapshot?.pauses.first(where: { $0.id == value.pauseID }),
                  prior.habitID == value.habitID,
                  prior.endedAt == nil,
                  prior.revision == value.command.expectedRevision else {
                throw HabitSyncControllerError.protocolFailure
            }
            let response = try await connection.transport.resumeHabitPause(
                habitID: value.habitID,
                pauseID: value.pauseID,
                command: value.command,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            let nextRevision = value.command.expectedRevision.addingReportingOverflow(1)
            guard !nextRevision.overflow,
                  response.pause.id == value.pauseID,
                  response.pause.habitID == value.habitID,
                  response.pause.revision == nextRevision.partialValue,
                  response.pause.endedAt == value.command.endedAt,
                  response.pause.startedAt == prior.startedAt,
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
        let remainingMutations = candidate.pendingMutations.filter { $0.id != operationID }
        let retentionDate = now()
        candidate = replacing(
            candidate,
            occurrences: try Self.retainedOccurrences(
                Array(occurrenceIndex.values),
                pendingMutations: remainingMutations,
                referenceDate: retentionDate
            ),
            pauses: try Self.retainedPauses(
                Array(pauseIndex.values),
                pendingMutations: remainingMutations
            ),
            pendingMutations: remainingMutations
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
            let observedInvalidationGeneration = foregroundStreamObservationGeneration
            let page = try await connection.transport.habitDelta(
                cursor: current.deltaCursor,
                limit: 200
            )
            try assertCurrent(operation: operation, connection: connection)
            // A successful authoritative response proves that the previously
            // terminal cache may be stale. Revoke process-local authority
            // before validating or persisting the page; only a committed
            // terminal candidate below may restore it. This also covers a CAS
            // or storage failure from `persist(candidate)` itself.
            let checkpointBeforePage = habitCompositionCheckpoint
            foregroundStreamInvalidationPending = true
            notifyCompositionCheckpointObservers(ifChangedFrom: checkpointBeforePage)
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
            let observationsAtCommit = foregroundStreamObservationGeneration
            let receivedObservationInFlight =
                observationsAtCommit > observedInvalidationGeneration
            // Opaque stream cursors have no ordering relationship. A terminal
            // page can cover observations that predated its request, but an
            // observation received while the request was in flight is covered
            // only by exact equality with that observation's latest cursor.
            let coversInFlightObservations = !receivedObservationInFlight
                || page.nextCursor == foregroundStreamLatestHintCursor
            let terminalCaughtUp = !page.hasMore && coversInFlightObservations
            let candidate: DayWeaveHabitClientSnapshot
            do {
                candidate = replacing(
                    current,
                    deltaCursor: page.nextCursor,
                    deltaCaughtUp: terminalCaughtUp,
                    occurrences: try Self.retainedOccurrences(
                        Array(occurrenceIndex.values),
                        pendingMutations: current.pendingMutations,
                        referenceDate: now()
                    ),
                    pauses: try Self.retainedPauses(
                        Array(pauseIndex.values),
                        pendingMutations: current.pendingMutations
                    )
                )
            } catch {
                // Once a new authoritative page has been observed, an older
                // terminal snapshot cannot remain composition-authoritative if
                // the page cannot be retained safely.
                if current.deltaCaughtUp {
                    let priorCheckpoint = habitCompositionCheckpoint
                    foregroundStreamInvalidationPending = true
                    try? persist(replacing(current, deltaCaughtUp: false))
                    notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
                }
                throw error
            }
            // The complete page and its opaque cursor share one encrypted CAS
            // commit. A crash can replay the page, but can never skip it.
            try persist(candidate)
            pages += 1
            if !page.hasMore {
                reconcileForegroundInvalidations(
                    through: coversInFlightObservations
                        ? observationsAtCommit : observedInvalidationGeneration
                )
                if terminalCaughtUp { return }
                // The terminal page raced a newer opaque observation and did
                // not prove it covered that observation. Its cursor is durable
                // but incomplete; fetch from it before reporting success.
                continue
            }
        }
    }

    private func persist(_ candidate: DayWeaveHabitClientSnapshot) throws {
        guard let persistence else { throw HabitPersistenceError.storageUnavailable }
        let saved = replacing(candidate, savedAt: now())
        let revision = try persistence.save(saved, expectedRevision: persistenceRevision)
        persistenceRevision = revision
        install(saved)
    }

    private func install(_ value: DayWeaveHabitClientSnapshot?) {
        let priorCheckpoint = habitCompositionCheckpoint
        snapshot = value
        occurrences = value?.occurrences ?? []
        pauses = value?.pauses ?? []
        analytics = value?.analytics ?? []
        pendingMutations = value?.pendingMutations ?? []
        notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
    }

    private func notifyCompositionCheckpointObservers(
        ifChangedFrom priorCheckpoint: HabitCompositionCheckpoint
    ) {
        guard priorCheckpoint != habitCompositionCheckpoint else { return }
        compositionCheckpointObservers.forEach { $0() }
    }

    private func reconcileForegroundInvalidations(through generation: UInt64) {
        let priorCheckpoint = habitCompositionCheckpoint
        foregroundStreamReconciledGeneration = max(
            foregroundStreamReconciledGeneration,
            generation
        )
        if foregroundStreamReconciledGeneration >= foregroundStreamObservationGeneration {
            foregroundStreamLatestHintCursor = nil
            foregroundStreamInvalidationPending = false
        }
        notifyCompositionCheckpointObservers(ifChangedFrom: priorCheckpoint)
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
        deltaCaughtUp: Bool? = nil,
        occurrences: [DayWeaveHabitOccurrence]? = nil,
        pauses: [DayWeaveHabitPause]? = nil,
        analytics: [DayWeaveHabitAnalytics]? = nil,
        pendingMutations: [DayWeavePendingHabitMutation]? = nil
    ) -> DayWeaveHabitClientSnapshot {
        .init(
            savedAt: savedAt ?? value.savedAt,
            configurationIdentifier: configurationIdentifier ?? value.configurationIdentifier,
            deltaCursor: deltaCursor ?? value.deltaCursor,
            deltaCaughtUp: deltaCaughtUp ?? value.deltaCaughtUp,
            occurrences: occurrences ?? value.occurrences,
            pauses: pauses ?? value.pauses,
            analytics: analytics ?? value.analytics,
            pendingMutations: pendingMutations ?? value.pendingMutations
        )
    }

    static func retainedOccurrences(
        _ values: [DayWeaveHabitOccurrence],
        pendingMutations: [DayWeavePendingHabitMutation],
        referenceDate: Date = Date(),
        limit: Int = DayWeaveHabitClientSnapshot.maximumOccurrences
    ) throws -> [DayWeaveHabitOccurrence] {
        guard limit >= 0 else { throw HabitSyncControllerError.protocolFailure }
        let ordered = values.sorted {
            if $0.evidence.nominalStart == $1.evidence.nominalStart {
                return $0.id.uuidString < $1.id.uuidString
            }
            return $0.evidence.nominalStart < $1.evidence.nominalStart
        }
        guard ordered.count > limit else {
            return ordered
        }
        let mandatoryIDs = Set(pendingMutations.compactMap { mutation -> UUID? in
            guard case let .outcome(value) = mutation else { return nil }
            return value.occurrenceID
        })
        var retained: [UUID: DayWeaveHabitOccurrence] = [:]
        for value in ordered.reversed() where mandatoryIDs.contains(value.id) {
            retained[value.id] = value
        }
        guard retained.count <= limit else {
            throw HabitSyncControllerError.protocolFailure
        }
        // Protect the complete scheduling neighborhood before allocating any
        // capacity to historical completion anchors. Without this reservation,
        // one old completion from many deleted habits can crowd every current
        // occurrence out of an otherwise terminal cache.
        let compositionStart = referenceDate.addingTimeInterval(-24 * 60 * 60)
        let compositionEnd = referenceDate.addingTimeInterval(8 * 24 * 60 * 60)
        let compositionRows = ordered.filter {
            $0.evidence.windowEnd > compositionStart
                && $0.evidence.windowStart < compositionEnd
        }
        // Every completed occurrence remains an authoritative fallback anchor.
        // A later completion can be corrected to partial/skipped, at which
        // point an older completion becomes the streak anchor again. Until the
        // wire contract provides a compact correction-safe anchor, no completed
        // row may be evicted silently.
        let completedRows = ordered.filter { $0.outcome?.status == .completed }
        let requiredIDs = mandatoryIDs
            .union(compositionRows.map(\.id))
            .union(completedRows.map(\.id))
        guard requiredIDs.count <= limit else {
            throw HabitSyncControllerError.protocolFailure
        }
        for value in compositionRows.reversed() {
            retained[value.id] = value
        }
        for value in completedRows.sorted(by: {
            ($0.outcome?.occurredAt ?? .distantPast) > ($1.outcome?.occurredAt ?? .distantPast)
        }) {
            retained[value.id] = value
        }
        for value in ordered.reversed()
            where retained.count < limit {
            retained[value.id] = value
        }
        guard mandatoryIDs.allSatisfy({ id in
            !ordered.contains(where: { $0.id == id }) || retained[id] != nil
        }) else { throw HabitSyncControllerError.protocolFailure }
        return retained.values.sorted {
            if $0.evidence.nominalStart == $1.evidence.nominalStart {
                return $0.id.uuidString < $1.id.uuidString
            }
            return $0.evidence.nominalStart < $1.evidence.nominalStart
        }
    }

    static func retainedPauses(
        _ values: [DayWeaveHabitPause],
        pendingMutations: [DayWeavePendingHabitMutation],
        limit: Int = DayWeaveHabitClientSnapshot.maximumPauses
    ) throws -> [DayWeaveHabitPause] {
        guard limit >= 0 else { throw HabitSyncControllerError.protocolFailure }
        let ordered = values.sorted {
            if $0.startedAt == $1.startedAt { return $0.id.uuidString < $1.id.uuidString }
            return $0.startedAt < $1.startedAt
        }
        guard ordered.count > limit else { return ordered }
        let mandatoryIDs = Set(pendingMutations.compactMap { mutation -> UUID? in
            guard case let .pauseResume(value) = mutation else { return nil }
            return value.pauseID
        }).union(ordered.compactMap { $0.endedAt == nil ? $0.id : nil })
        guard mandatoryIDs.count <= limit else {
            throw HabitSyncControllerError.protocolFailure
        }
        var retained: [UUID: DayWeaveHabitPause] = [:]
        for value in ordered.reversed() where mandatoryIDs.contains(value.id) {
            retained[value.id] = value
        }
        for value in ordered.reversed()
            where retained.count < limit {
            retained[value.id] = value
        }
        guard mandatoryIDs.allSatisfy({ id in
            !ordered.contains(where: { $0.id == id }) || retained[id] != nil
        }) else { throw HabitSyncControllerError.protocolFailure }
        return retained.values.sorted {
            if $0.startedAt == $1.startedAt { return $0.id.uuidString < $1.id.uuidString }
            return $0.startedAt < $1.startedAt
        }
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
            install(nil)
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
