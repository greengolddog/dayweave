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
        let policyFingerprint: String?
        let nominalStart: Date
        let windowStart: Date
        let windowEnd: Date
        let expectedDurationSeconds: UInt64?
        let outcome: Outcome?
        let missedResolution: DayWeaveHabitMissedResolution?
        let identity: JSONValue?
        let nominalEnd: Date?
        let localDate: DayWeaveLocalDate?

        init(
            id: UUID,
            habitID: UUID,
            plannerOccurrenceID: UUID,
            sourceItemRevision: UInt64,
            policyFingerprint: String? = nil,
            nominalStart: Date,
            windowStart: Date,
            windowEnd: Date,
            expectedDurationSeconds: UInt64?,
            outcome: Outcome?,
            missedResolution: DayWeaveHabitMissedResolution? = nil,
            identity: JSONValue? = nil,
            nominalEnd: Date? = nil,
            localDate: DayWeaveLocalDate? = nil
        ) {
            self.id = id
            self.habitID = habitID
            self.plannerOccurrenceID = plannerOccurrenceID
            self.sourceItemRevision = sourceItemRevision
            self.policyFingerprint = policyFingerprint
            self.nominalStart = nominalStart
            self.windowStart = windowStart
            self.windowEnd = windowEnd
            self.expectedDurationSeconds = expectedDurationSeconds
            self.outcome = outcome
            self.missedResolution = missedResolution
            self.identity = identity
            self.nominalEnd = nominalEnd
            self.localDate = localDate
        }
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

struct EffectiveHabitMissedProjection {
    let actionsByEvidenceID: [UUID: DayWeaveHabitMissedResolutionAction]
    let suppressedPlannerOccurrenceIDs: Set<UUID>
}

/// Resolves the server's forward reduction graph without allowing a suppressed source to cascade.
func effectiveHabitMissedProjection(
    occurrences: [HabitCompositionCheckpoint.Occurrence],
    sourceIsActive: (HabitCompositionCheckpoint.Occurrence) -> Bool,
    reductionTargetIsEligible: (
        HabitCompositionCheckpoint.Occurrence,
        HabitCompositionCheckpoint.Occurrence
    ) -> Bool
) -> EffectiveHabitMissedProjection {
    typealias Occurrence = HabitCompositionCheckpoint.Occurrence
    func ordinal(_ occurrence: Occurrence) -> UInt32? {
        guard let identity = occurrence.identity,
              let data = try? JSONEncoder().encode(identity),
              let decoded = try? JSONDecoder().decode(
                  RecurrenceOccurrenceIdentity.self,
                  from: data
              ) else { return nil }
        return decoded.stableOrdinal
    }
    func orderedBefore(_ left: Occurrence, _ right: Occurrence) -> Bool {
        if left.nominalStart != right.nominalStart {
            return left.nominalStart < right.nominalStart
        }
        let leftOrdinal = ordinal(left) ?? .max
        let rightOrdinal = ordinal(right) ?? .max
        if leftOrdinal != rightOrdinal { return leftOrdinal < rightOrdinal }
        return left.plannerOccurrenceID.uuidString < right.plannerOccurrenceID.uuidString
    }
    let plannerGroups = Dictionary(grouping: occurrences, by: \.plannerOccurrenceID)
    let candidates = occurrences.compactMap { occurrence -> (
        Occurrence,
        DayWeaveHabitMissedResolutionAction
    )? in
        guard ordinal(occurrence) != nil,
              sourceIsActive(occurrence),
              let action = occurrence.missedResolution?.action else { return nil }
        if case .cancelled = action { return nil }
        return (occurrence, action)
    }.sorted { orderedBefore($0.0, $1.0) }

    var actions: [UUID: DayWeaveHabitMissedResolutionAction] = [:]
    var suppressed: Set<UUID> = []
    for (source, action) in candidates {
        guard !suppressed.contains(source.plannerOccurrenceID) else { continue }
        actions[source.id] = action
        guard case let .reduceFrequency(ids) = action else { continue }
        for targetID in ids {
            guard let targets = plannerGroups[targetID], targets.count == 1,
                  let target = targets.first,
                  target.habitID == source.habitID,
                  orderedBefore(source, target),
                  reductionTargetIsEligible(source, target) else { continue }
            suppressed.insert(targetID)
        }
    }
    return .init(
        actionsByEvidenceID: actions,
        suppressedPlannerOccurrenceIDs: suppressed
    )
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
    case plannerAuthorityUnavailable

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
        case .plannerAuthorityUnavailable: "Encrypted schedule moves are unavailable. Habit authority was preserved without advancing sync."
        }
    }
}

/// A privacy-bound, process-death-safe projection of the canonical habit ledger.
/// Private notes are released into memory only after `activate` and removed at
/// every app-lock/background privacy boundary.
@MainActor
final class HabitSyncStore: ObservableObject, HabitCompositionCheckpointProviding {
    static let maximumDeltaPagesPerSync = 1_000
    static let maximumMissedReconcilePagesPerSync = 1_000
    static let maximumImmediateStreamDrains = 2
    /// Empty automatic reconcile responses have a bounded server replay
    /// lease. Rotate an unresolved client journal well before that lease can
    /// expire; changed responses remain permanently replayable, and delta is
    /// kept non-authoritative until the replacement scan completes.
    static let missedReconcileJournalLease: TimeInterval = 12 * 60 * 60

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
    private let protectedPlannerOccurrenceIDs: @MainActor @Sendable () -> Set<UUID>?
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
        protectedPlannerOccurrenceIDs: @escaping @MainActor @Sendable () -> Set<UUID>? = { [] },
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
        self.protectedPlannerOccurrenceIDs = protectedPlannerOccurrenceIDs
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
        protectedPlannerOccurrenceIDs: @escaping @MainActor @Sendable () -> Set<UUID>? = { [] },
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
        self.protectedPlannerOccurrenceIDs = protectedPlannerOccurrenceIDs
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
                    policyFingerprint: occurrence.evidence.policyFingerprint,
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
                    },
                    missedResolution: occurrence.missedResolution,
                    identity: occurrence.evidence.identity,
                    nominalEnd: occurrence.evidence.nominalEnd,
                    localDate: occurrence.evidence.localDate
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
            switch mutation {
            case let .outcome(value): return value.occurrenceID == occurrenceID
            case let .missedResolution(value): return value.occurrenceID == occurrenceID
            default: return false
            }
        }
    }

    func pendingPauseMutation(forHabitID habitID: UUID) -> DayWeavePendingHabitMutation? {
        pendingMutations.first { mutation in
            switch mutation {
            case .outcome, .missedReconcile, .missedResolution:
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
               restored.pendingMutations.contains(where: mutationRequiresOriginalBinding) {
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

            try fenceDeltaWhilePendingMutationsExist()
            try await replayPendingMutations(using: connection, operation: operation)
            try await reconcileMissedOccurrences(using: connection, operation: operation)
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
            try fenceDeltaWhilePendingMutationsExist()
            try await replayPendingMutations(using: connection, operation: operation)
            try await reconcileMissedOccurrences(using: connection, operation: operation)
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

    func resolveMissed(
        _ occurrence: DayWeaveHabitOccurrence,
        action: DayWeaveHabitMissedExplicitAction
    ) async -> HabitSyncOutcome {
        guard operationID == nil else { return .unexpectedFailure }
        guard pendingMutation(forOccurrenceID: occurrence.id) == nil else {
            status = .init(
                phase: .attentionRequired,
                message: HabitSyncControllerError.pendingMutationExists.localizedDescription
            )
            return .conflict
        }
        guard occurrences.first(where: { $0.id == occurrence.id }) == occurrence,
              let resolution = occurrence.missedResolution,
              resolution.configuredPolicy == .ask,
              resolution.action.isDecisionRequired,
              occurrence.hasActiveMissedResolutionLifecycle(pauses: pauses) else {
            status = .init(
                phase: .attentionRequired,
                message: "This missed-habit choice changed. Review the current server version."
            )
            return .conflict
        }
        let operation = makeUUID()
        let pending = DayWeavePendingHabitMutation.missedResolution(.init(
            habitID: occurrence.evidence.habitID,
            occurrenceID: occurrence.id,
            idempotencyKey: "habit-missed-resolution:\(operation.uuidString.lowercased())",
            command: .init(
                operationID: operation,
                expectedRevision: resolution.revision,
                action: action
            ),
            createdAt: now(),
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
                deltaCaughtUp: false,
                analytics: candidate.analytics.filter { $0.habitID != pending.habitID },
                pendingMutations: candidate.pendingMutations + [pending]
            )
            try persist(candidate)
            try await execute(pending, using: connection, operation: operation)
            try await reconcileDelta(using: connection, operation: operation)
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
        try rotateExpiredMissedReconcileJournal()
        let queued = snapshot?.pendingMutations ?? []
        for pending in queued where !pending.conflictDetected {
            do {
                try await execute(pending, using: connection, operation: operation)
            } catch {
                guard isConflict(error), pending.canRequireUserConflictReview else { throw error }
                // A conflict discovered while replaying after process death
                // needs the same durable review marker as an immediate write.
                // Keep processing the delta so the user can compare against
                // the authoritative current occurrence or pause.
                try markPendingConflict(pending.id)
            }
        }
    }

    private func rotateExpiredMissedReconcileJournal() throws {
        guard let current = snapshot else { throw HabitSyncControllerError.protocolFailure }
        let cutoff = now().addingTimeInterval(-Self.missedReconcileJournalLease)
        let retained = current.pendingMutations.filter { mutation in
            guard case let .missedReconcile(value) = mutation else { return true }
            return value.createdAt > cutoff
        }
        guard retained.count != current.pendingMutations.count else { return }
        try persist(replacing(
            current,
            deltaCaughtUp: false,
            pendingMutations: retained
        ))
    }

    private func mutationRequiresOriginalBinding(
        _ mutation: DayWeavePendingHabitMutation
    ) -> Bool {
        guard case let .missedReconcile(value) = mutation else { return true }
        let cutoff = now().addingTimeInterval(-Self.missedReconcileJournalLease)
        return value.createdAt > cutoff
    }

    private func reconcileMissedOccurrences(
        using connection: DayWeaveHabitConnection,
        operation: UUID
    ) async throws {
        var pages = 0
        while true {
            guard pages < Self.maximumMissedReconcilePagesPerSync,
                  let current = snapshot else {
                throw HabitSyncControllerError.protocolFailure
            }
            let requestID = makeUUID()
            let pending = DayWeavePendingHabitMutation.missedReconcile(.init(
                idempotencyKey: "habit-missed-reconcile:\(requestID.uuidString.lowercased())",
                command: .init(operationID: requestID),
                limit: 200,
                createdAt: now(),
                conflictDetected: false
            ))
            try persist(replacing(
                current,
                deltaCaughtUp: false,
                pendingMutations: current.pendingMutations + [pending]
            ))
            let hasMore = try await execute(pending, using: connection, operation: operation)
            pages += 1
            if !hasMore { return }
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

    private func fenceDeltaWhilePendingMutationsExist() throws {
        guard let current = snapshot,
              current.deltaCaughtUp,
              !current.pendingMutations.isEmpty else { return }
        try persist(replacing(current, deltaCaughtUp: false))
    }

    @discardableResult
    private func execute(
        _ pending: DayWeavePendingHabitMutation,
        using connection: DayWeaveHabitConnection,
        operation: UUID
    ) async throws -> Bool {
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
            // Outcome and missed-resolution revisions are independent server
            // coordinates. Another device may advance the latter while this
            // request is in flight; validate and merge that coordinate rather
            // than requiring the response to echo our cached projection.
            let merged = try Self.mergedOccurrence(prior, incoming: response.occurrence)
            try commitMutation(pending.id, occurrence: merged, pause: nil)
            return false
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
            return false
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
            return false
        case let .missedReconcile(value):
            let response = try await connection.transport.reconcileMissedHabitOccurrences(
                command: value.command,
                limit: value.limit,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            guard response.resolutions.count <= value.limit else {
                throw HabitSyncControllerError.protocolFailure
            }
            try commitMutation(
                pending.id,
                occurrence: nil,
                pause: nil,
                missedResolutions: response.resolutions
            )
            return response.hasMore
        case let .missedResolution(value):
            guard let prior = snapshot?.occurrences.first(where: {
                $0.id == value.occurrenceID
            }),
                prior.evidence.habitID == value.habitID,
                let priorResolution = prior.missedResolution,
                priorResolution.action.isDecisionRequired,
                priorResolution.revision == value.command.expectedRevision else {
                throw HabitSyncControllerError.protocolFailure
            }
            let response = try await connection.transport.resolveMissedHabitOccurrence(
                habitID: value.habitID,
                occurrenceID: value.occurrenceID,
                command: value.command,
                idempotencyKey: value.idempotencyKey
            )
            try assertCurrent(operation: operation, connection: connection)
            let resolution = response.resolution
            let nextRevision = value.command.expectedRevision.addingReportingOverflow(1)
            guard !nextRevision.overflow,
                  resolution.belongs(to: prior.evidence),
                  resolution.revision == nextRevision.partialValue,
                  resolution.configuredPolicy == .ask,
                  Self.sameMissedResolutionIdentity(priorResolution, resolution),
                  priorResolution.canTransition(to: resolution),
                  Self.missedResolutionAction(
                      resolution.action,
                      satisfies: value.command.action
                  ) else {
                throw HabitSyncControllerError.protocolFailure
            }
            try commitMutation(
                pending.id,
                occurrence: nil,
                pause: nil,
                missedResolutions: [resolution]
            )
            return false
        }
    }

    private func commitMutation(
        _ operationID: UUID,
        occurrence: DayWeaveHabitOccurrence?,
        pause: DayWeaveHabitPause?,
        missedResolutions: [DayWeaveHabitMissedResolution] = []
    ) throws {
        guard var candidate = snapshot,
              candidate.pendingMutations.contains(where: { $0.id == operationID }) else {
            throw HabitSyncControllerError.protocolFailure
        }
        var occurrenceIndex = Dictionary(uniqueKeysWithValues: candidate.occurrences.map { ($0.id, $0) })
        if let occurrence { occurrenceIndex[occurrence.id] = occurrence }
        for resolution in missedResolutions {
            guard let prior = occurrenceIndex[resolution.occurrenceEvidenceID] else { continue }
            guard resolution.belongs(to: prior.evidence) else {
                throw HabitSyncControllerError.protocolFailure
            }
            occurrenceIndex[prior.id] = .init(
                evidence: prior.evidence,
                outcome: prior.outcome,
                missedResolution: try Self.mergedMissedResolution(
                    prior.missedResolution,
                    incoming: resolution
                )
            )
        }
        var pauseIndex = Dictionary(uniqueKeysWithValues: candidate.pauses.map { ($0.id, $0) })
        if let pause { pauseIndex[pause.id] = pause }
        let remainingMutations = candidate.pendingMutations.filter { $0.id != operationID }
        let retentionDate = now()
        guard let protectedPlannerOccurrenceIDs = protectedPlannerOccurrenceIDs() else {
            throw HabitSyncControllerError.plannerAuthorityUnavailable
        }
        let retainedOccurrences = try Self.retainedOccurrences(
            Array(occurrenceIndex.values),
            pendingMutations: remainingMutations,
            protectedPlannerOccurrenceIDs: protectedPlannerOccurrenceIDs,
            referenceDate: retentionDate
        )
        candidate = replacing(
            candidate,
            occurrences: retainedOccurrences,
            pauses: try Self.retainedPauses(
                Array(pauseIndex.values),
                pendingMutations: remainingMutations,
                protectedOccurrences: retainedOccurrences
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
                        occurrenceIndex[value.id] = try Self.mergedOccurrence(
                            prior,
                            incoming: value
                        )
                    } else {
                        occurrenceIndex[value.id] = value
                    }
                case let .pauseUpsert(value):
                    if let prior = pauseIndex[value.id] {
                        guard value.habitID == prior.habitID,
                              value.startedAt == prior.startedAt,
                              value.createdAt == prior.createdAt,
                              value.preservesStreak == prior.preservesStreak else {
                            throw HabitSyncControllerError.protocolFailure
                        }
                        if value.revision < prior.revision { continue }
                        if value.revision == prior.revision {
                            guard value == prior else {
                                throw HabitSyncControllerError.protocolFailure
                            }
                            continue
                        }
                        guard prior.endedAt == nil || value.endedAt == prior.endedAt else {
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
                guard let protectedPlannerOccurrenceIDs = protectedPlannerOccurrenceIDs() else {
                    throw HabitSyncControllerError.plannerAuthorityUnavailable
                }
                let retentionDate = now()
                let retainedOccurrences = try Self.retainedOccurrences(
                    Array(occurrenceIndex.values),
                    pendingMutations: current.pendingMutations,
                    protectedPlannerOccurrenceIDs: protectedPlannerOccurrenceIDs,
                    referenceDate: retentionDate
                )
                candidate = replacing(
                    current,
                    deltaCursor: page.nextCursor,
                    deltaCaughtUp: terminalCaughtUp,
                    occurrences: retainedOccurrences,
                    pauses: try Self.retainedPauses(
                        Array(pauseIndex.values),
                        pendingMutations: current.pendingMutations,
                        protectedOccurrences: retainedOccurrences
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
        protectedPlannerOccurrenceIDs: Set<UUID> = [],
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
        guard Set(ordered.map(\.id)).count == ordered.count,
              Set(ordered.map(\.evidence.plannerOccurrenceID)).count == ordered.count else {
            throw HabitSyncControllerError.protocolFailure
        }
        guard ordered.count > limit else {
            return ordered
        }
        let journalIDs = Set(pendingMutations.compactMap { mutation -> UUID? in
            switch mutation {
            case let .outcome(value): return value.occurrenceID
            case let .missedResolution(value): return value.occurrenceID
            default: return nil
            }
        })
        let plannerMoveProtectedIDs = Set(ordered.compactMap { occurrence in
            protectedPlannerOccurrenceIDs.contains(occurrence.evidence.plannerOccurrenceID)
                ? occurrence.id : nil
        })
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
        let occurrenceByPlannerID = Dictionary(
            uniqueKeysWithValues: ordered.map { ($0.evidence.plannerOccurrenceID, $0) }
        )
        var activeMissedIDs = Set<UUID>()
        var reductionSourcesByTarget: [UUID: [DayWeaveHabitOccurrence]] = [:]
        for occurrence in ordered {
            guard let resolution = occurrence.missedResolution else { continue }
            if case let .reduceFrequency(targetPlannerIDs) = resolution.action {
                // A terminal target can later be corrected to unresolved
                // without replaying its upstream source. Preserve every
                // physical reduction edge, including split-page sources, so
                // pruning cannot change graph precedence after restart.
                activeMissedIDs.insert(occurrence.id)
                for targetPlannerID in targetPlannerIDs {
                    reductionSourcesByTarget[targetPlannerID, default: []].append(occurrence)
                    if let target = occurrenceByPlannerID[targetPlannerID],
                       target.evidence.habitID == occurrence.evidence.habitID {
                        activeMissedIDs.insert(target.id)
                    }
                }
                continue
            }
            guard occurrence.outcome?.status.endsMissedResolutionLifecycle != true else { continue }
            switch resolution.action {
            case .decisionRequired, .reductionPending:
                activeMissedIDs.insert(occurrence.id)
            case let .carry(windowStart, windowEnd)
                where windowStart < compositionEnd && windowEnd > compositionStart:
                activeMissedIDs.insert(occurrence.id)
            case .cancelled, .skip, .carry, .reduceFrequency:
                break
            }
        }
        let completedRows = ordered.filter { $0.outcome?.status == .completed }
        var mandatoryIDs = journalIDs
            .union(activeMissedIDs)
            .union(plannerMoveProtectedIDs)
            .union(compositionRows.map(\.id))
            .union(completedRows.map(\.id))
        let occurrenceByID = Dictionary(uniqueKeysWithValues: ordered.map { ($0.id, $0) })
        var plannerIDsToVisit = mandatoryIDs.compactMap {
            occurrenceByID[$0]?.evidence.plannerOccurrenceID
        }
        var visitedPlannerIDs = Set<UUID>()
        while let targetPlannerID = plannerIDsToVisit.popLast() {
            guard visitedPlannerIDs.insert(targetPlannerID).inserted else { continue }
            for source in reductionSourcesByTarget[targetPlannerID, default: []]
                where source.evidence.habitID
                    == occurrenceByPlannerID[targetPlannerID]?.evidence.habitID {
                if mandatoryIDs.insert(source.id).inserted {
                    plannerIDsToVisit.append(source.evidence.plannerOccurrenceID)
                }
            }
        }
        var retained: [UUID: DayWeaveHabitOccurrence] = [:]
        for value in ordered.reversed() where mandatoryIDs.contains(value.id) {
            retained[value.id] = value
        }
        guard retained.count <= limit else {
            throw HabitSyncControllerError.protocolFailure
        }
        // Every completed occurrence remains an authoritative fallback anchor.
        // The transitive closure above also preserves any unresolved reducer
        // whose target is required. Otherwise evicting A from A -> B -> C can
        // make B spuriously effective after restart and suppress C.
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
        protectedOccurrences: [DayWeaveHabitOccurrence] = [],
        limit: Int = DayWeaveHabitClientSnapshot.maximumPauses
    ) throws -> [DayWeaveHabitPause] {
        guard limit >= 0 else { throw HabitSyncControllerError.protocolFailure }
        let ordered = values.sorted {
            if $0.startedAt == $1.startedAt { return $0.id.uuidString < $1.id.uuidString }
            return $0.startedAt < $1.startedAt
        }
        guard Set(ordered.map(\.id)).count == ordered.count else {
            throw HabitSyncControllerError.protocolFailure
        }
        for habitPauses in Dictionary(grouping: ordered, by: \.habitID).values {
            guard habitPauses.filter({ $0.endedAt == nil }).count <= 1 else {
                throw HabitSyncControllerError.protocolFailure
            }
            for (previous, next) in zip(habitPauses, habitPauses.dropFirst()) {
                guard let previousEnd = previous.endedAt,
                      previousEnd <= next.startedAt else {
                    throw HabitSyncControllerError.protocolFailure
                }
            }
        }
        guard ordered.count > limit else { return ordered }
        let occurrenceGroups = Dictionary(
            grouping: protectedOccurrences,
            by: \.evidence.plannerOccurrenceID
        )
        var protectedWindows: [(habitID: UUID, start: Date, end: Date)] = []
        for occurrence in protectedOccurrences {
            protectedWindows.append((
                habitID: occurrence.evidence.habitID,
                start: occurrence.evidence.windowStart,
                end: occurrence.evidence.windowEnd
            ))
            guard let action = occurrence.missedResolution?.action else { continue }
            if case let .carry(windowStart, windowEnd) = action {
                protectedWindows.append((
                    habitID: occurrence.evidence.habitID,
                    start: windowStart,
                    end: windowEnd
                ))
            }
            guard case let .reduceFrequency(targetPlannerIDs) = action else { continue }
            for targetPlannerID in targetPlannerIDs {
                guard let targets = occurrenceGroups[targetPlannerID], targets.count == 1,
                      let target = targets.first,
                      target.evidence.habitID == occurrence.evidence.habitID else { continue }
                protectedWindows.append((
                    habitID: target.evidence.habitID,
                    start: target.evidence.windowStart,
                    end: target.evidence.windowEnd
                ))
            }
        }
        let lifecyclePauseIDs = ordered.compactMap { pause -> UUID? in
            protectedWindows.contains { window in
                pause.habitID == window.habitID
                    && pause.startedAt < window.end
                    && (pause.endedAt.map { $0 > window.start } ?? true)
            } ? pause.id : nil
        }
        let mandatoryIDs = Set(pendingMutations.compactMap { mutation -> UUID? in
            guard case let .pauseResume(value) = mutation else { return nil }
            return value.pauseID
        })
            .union(ordered.compactMap { $0.endedAt == nil ? $0.id : nil })
            .union(lifecyclePauseIDs)
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

    private static func mergedOccurrence(
        _ prior: DayWeaveHabitOccurrence,
        incoming: DayWeaveHabitOccurrence
    ) throws -> DayWeaveHabitOccurrence {
        guard prior.evidence == incoming.evidence else {
            throw HabitSyncControllerError.protocolFailure
        }
        return .init(
            evidence: prior.evidence,
            outcome: try mergedOutcome(prior.outcome, incoming: incoming.outcome),
            missedResolution: try mergedMissedResolution(
                prior.missedResolution,
                incoming: incoming.missedResolution
            )
        )
    }

    private static func mergedOutcome(
        _ prior: DayWeaveHabitOutcome?,
        incoming: DayWeaveHabitOutcome?
    ) throws -> DayWeaveHabitOutcome? {
        switch (prior, incoming) {
        case (nil, let incoming):
            return incoming
        case (let prior, nil):
            return prior
        case let (.some(prior), .some(incoming)):
            if incoming.revision == prior.revision {
                guard incoming == prior else { throw HabitSyncControllerError.protocolFailure }
                return prior
            }
            return incoming.revision > prior.revision ? incoming : prior
        }
    }

    private static func mergedMissedResolution(
        _ prior: DayWeaveHabitMissedResolution?,
        incoming: DayWeaveHabitMissedResolution?
    ) throws -> DayWeaveHabitMissedResolution? {
        switch (prior, incoming) {
        case (nil, let incoming):
            return incoming
        case (let prior, nil):
            return prior
        case let (.some(prior), .some(incoming)):
            guard sameMissedResolutionIdentity(prior, incoming) else {
                throw HabitSyncControllerError.protocolFailure
            }
            if incoming.revision == prior.revision {
                guard incoming == prior else { throw HabitSyncControllerError.protocolFailure }
                return prior
            }
            guard incoming.revision > prior.revision else {
                guard incoming.updatedAt <= prior.updatedAt else {
                    throw HabitSyncControllerError.protocolFailure
                }
                return prior
            }
            guard incoming.updatedAt >= prior.updatedAt,
                  isReachableMissedResolutionAction(from: prior, to: incoming) else {
                throw HabitSyncControllerError.protocolFailure
            }
            return incoming
        }
    }

    private enum MissedResolutionState: Hashable {
        case decisionRequired
        case reductionPending
        case cancelled(DayWeaveHabitMissedResumeAction)
        case skip
        case carry
        case reduceFrequency

        init(_ action: DayWeaveHabitMissedResolutionAction) {
            switch action {
            case .decisionRequired: self = .decisionRequired
            case .reductionPending: self = .reductionPending
            case let .cancelled(_, resumeAction): self = .cancelled(resumeAction)
            case .skip: self = .skip
            case .carry: self = .carry
            case .reduceFrequency: self = .reduceFrequency
            }
        }
    }

    private static func isReachableMissedResolutionAction(
        from prior: DayWeaveHabitMissedResolution,
        to incoming: DayWeaveHabitMissedResolution
    ) -> Bool {
        guard incoming.revision > prior.revision else { return false }
        let revisionDistance = incoming.revision - prior.revision
        if revisionDistance == 1 {
            return prior.canTransition(to: incoming)
        }
        let target = MissedResolutionState(incoming.action)
        var frontier: Set<MissedResolutionState> = [MissedResolutionState(prior.action)]
        var step: UInt64 = 0
        var seen: [Set<MissedResolutionState>: UInt64] = [:]
        while step < revisionDistance {
            if let cycleStart = seen[frontier] {
                let cycleLength = step - cycleStart
                let remaining = revisionDistance - step
                let completeCycles = remaining / cycleLength
                if completeCycles > 0 {
                    step += completeCycles * cycleLength
                    continue
                }
            } else {
                seen[frontier] = step
            }
            frontier = Set(frontier.flatMap {
                nextMissedResolutionStates(after: $0, policy: prior.configuredPolicy)
            })
            step += 1
            if frontier.isEmpty { return false }
        }
        return frontier.contains(target)
    }

    private static func nextMissedResolutionStates(
        after state: MissedResolutionState,
        policy: DayWeaveHabitMissedPolicy
    ) -> Set<MissedResolutionState> {
        switch state {
        case .decisionRequired:
            guard policy == .ask else { return [] }
            return [
                .skip, .carry, .reductionPending, .reduceFrequency,
                .cancelled(.decisionRequired), .cancelled(.skip),
                .cancelled(.carry), .cancelled(.reduceFrequency),
            ]
        case .reductionPending, .reduceFrequency:
            return [.reductionPending, .reduceFrequency, .cancelled(.reduceFrequency)]
        case .skip:
            return [.cancelled(.skip)]
        case .carry:
            if policy == .ask { return [.decisionRequired, .cancelled(.carry)] }
            return policy == .carry ? [.carry, .cancelled(.carry)] : []
        case let .cancelled(resumeAction):
            return switch resumeAction {
            case .decisionRequired: [.decisionRequired]
            case .skip: [.skip]
            case .carry: [.carry]
            case .reduceFrequency: [.reductionPending, .reduceFrequency]
            }
        }
    }

    private static func sameMissedResolutionIdentity(
        _ left: DayWeaveHabitMissedResolution,
        _ right: DayWeaveHabitMissedResolution
    ) -> Bool {
        left.occurrenceEvidenceID == right.occurrenceEvidenceID
            && left.habitID == right.habitID
            && left.sourcePlannerOccurrenceID == right.sourcePlannerOccurrenceID
            && left.configuredPolicy == right.configuredPolicy
            && dayWeavePostgresEpochMicroseconds(left.createdAt)
                == dayWeavePostgresEpochMicroseconds(right.createdAt)
    }

    private static func missedResolutionAction(
        _ received: DayWeaveHabitMissedResolutionAction,
        satisfies requested: DayWeaveHabitMissedExplicitAction
    ) -> Bool {
        switch (requested, received) {
        case (.skip, .skip), (.carry, .carry):
            return true
        case (.reduceFrequency, .reduceFrequency), (.reduceFrequency, .reductionPending):
            return true
        case let (.skip, .cancelled(_, resumeAction)):
            return resumeAction == .skip
        case let (.carry, .cancelled(_, resumeAction)):
            return resumeAction == .carry
        case let (.reduceFrequency, .cancelled(_, resumeAction)):
            return resumeAction == .reduceFrequency
        default:
            return false
        }
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
        case HabitSyncControllerError.plannerAuthorityUnavailable:
            outcome = .localStorageFailure
            phase = .failed
            message = HabitSyncControllerError.plannerAuthorityUnavailable.localizedDescription
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
