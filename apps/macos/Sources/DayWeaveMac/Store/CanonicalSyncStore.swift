import Foundation

enum CanonicalSyncStatus: Equatable, Sendable {
    case configurationRequired(String)
    case ready
    case syncing(String)
    case online(updatedAt: Date, message: String)
    case failed(String)

    var message: String {
        switch self {
        case let .configurationRequired(message), let .syncing(message), let .failed(message): message
        case .ready: "Canonical sync is configured."
        case let .online(updatedAt, message):
            "\(message) · \(updatedAt.formatted(date: .omitted, time: .shortened))"
        }
    }

    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

enum LocalScheduleCompositionStatus: Equatable, Sendable {
    case ready
    case composing(String)
    case composed(generatedAt: Date, message: String)
    case failed(String)

    var message: String {
        switch self {
        case .ready:
            "On-device composition is ready."
        case let .composing(message), let .failed(message):
            message
        case let .composed(generatedAt, message):
            "\(message) · \(generatedAt.formatted(date: .omitted, time: .shortened))"
        }
    }
}

@MainActor
final class CanonicalSyncStore: ObservableObject {
    static let maximumDeltaChanges = 20_000
    static let maximumRetainedDeltaBytes = 32 * 1_048_576
    static let maximumDeltaCursorBytes = 4_096
    static let maximumCreatePushesPerSync = 100
    static let maximumAuthoringPushesPerSync = 100
    static let maximumStatusPushesPerSync = 100
    static let maximumPreviousAssignments = 10_000
    static let maximumPreviousAssignmentBlocks = 50_000
    @Published private(set) var status: CanonicalSyncStatus
    @Published private(set) var isSyncing = false
    @Published private(set) var lastPreview: DayWeaveSchedulePreview?
    @Published private(set) var warnings: [String] = []
    @Published private(set) var localCompositionStatus: LocalScheduleCompositionStatus = .ready
    @Published private(set) var isLocallyComposing = false
    @Published private(set) var lastLocalComposition: LocalScheduleComposition?
    @Published private(set) var lastLocalCompositionScore: DayWeaveSchedulePreview.Plan.Score?
    @Published private(set) var localCompositionWarnings: [String] = []

    private let planner: PlannerStore
    private let configurationStore: any SuggestionAPIConfigurationStoring
    private let tokenStore: any BearerTokenStoring
    private let authCoordinator: DurableAuthCoordinator?
    private let session: URLSession
    private let localComposer: any LocalScheduleComposing
    private let habitCompositionProvider: (any HabitCompositionCheckpointProviding)?
    private let now: @Sendable () -> Date
    private let createPushLimit: Int
    private let authoringPushLimit: Int
    private let statusPushLimit: Int
    private let previousAssignmentLimit: Int
    private let previousAssignmentBlockLimit: Int
    private let itemStreamTransportProvider:
        @MainActor @Sendable (DayWeaveAPIClient) -> (any DayWeaveItemStreamTransport)?
    private let itemStreamSleep: @Sendable (Duration) async throws -> Void
    private let scheduleStreamTransportProvider:
        @MainActor @Sendable (DayWeaveAPIClient) -> (any DayWeaveScheduleStreamTransport)?
    private let scheduleStreamSleep: @Sendable (Duration) async throws -> Void
    private let scheduleReplicaRequiresDurableBinding: Bool
    private var configurationGeneration: UInt64 = 0
    private var activeSyncID: UUID?
    private var activeSyncTask: Task<Void, Never>?
    private var activeSyncScheduleProfile: ScheduleProfile?
    private var lastSuccessfulSyncID: UUID?
    private var lastFreshCompositionSyncID: UUID?
    private var activeLocalCompositionID: UUID?
    private var activeLocalCompositionTask: Task<LocalScheduleComposition, Error>?
    private var activeLocalCompositionScheduleProfile: ScheduleProfile?
    private var foregroundItemPollTask: Task<Void, Never>?
    private var foregroundItemStreamTask: Task<Void, Never>?
    private var foregroundItemDrainTask: Task<Void, Never>?
    private var foregroundItemOperationID: UUID?
    private var lastSuccessfulForegroundItemOperationID: UUID?
    private var foregroundItemGeneration: UInt64 = 0
    private var foregroundItemObservationGeneration: UInt64 = 0
    private var foregroundItemReconciledGeneration: UInt64 = 0
    private var foregroundItemLatestHintCursor: String?
    private var foregroundItemStreamUnavailableForActivation = false
    private var foregroundItemImmediateAttempts = 0
    private var foregroundPublicationRepairRequired = false
    private var foregroundScheduleStreamTask: Task<Void, Never>?
    private var foregroundScheduleDrainTask: Task<Void, Never>?
    private var foregroundScheduleLatestHintRevision: UInt64 = 0
    private var foregroundScheduleReconciledRevision: UInt64 = 0
    private var foregroundScheduleStreamUnavailableForActivation = false
    private var foregroundScheduleImmediateAttempts = 0
    private var foregroundScheduleRefreshInProgress = false
    /// Created only after the server rejects the exact durable SSE cursor as
    /// ahead. It remains pending across an overlapping drain or transient GET
    /// and is consumed only by a durable current-schedule installation for the
    /// same binding while that rejected revision is still the local high-water.
    private var foregroundScheduleEpochResetFence:
        PlannerScheduleRevisionEpochResetFence?

    init(
        planner: PlannerStore,
        configurationStore: any SuggestionAPIConfigurationStoring = UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        authCoordinator: DurableAuthCoordinator? = nil,
        session: URLSession = makeDayWeaveEphemeralSession(),
        localComposer: any LocalScheduleComposing = SchedulerHelperClient(),
        habitCompositionProvider: (any HabitCompositionCheckpointProviding)? = nil,
        createPushLimit: Int = CanonicalSyncStore.maximumCreatePushesPerSync,
        authoringPushLimit: Int = CanonicalSyncStore.maximumAuthoringPushesPerSync,
        statusPushLimit: Int = CanonicalSyncStore.maximumStatusPushesPerSync,
        previousAssignmentLimit: Int = CanonicalSyncStore.maximumPreviousAssignments,
        previousAssignmentBlockLimit: Int = CanonicalSyncStore.maximumPreviousAssignmentBlocks,
        itemStreamTransportProvider: @escaping @MainActor @Sendable
            (DayWeaveAPIClient) -> (any DayWeaveItemStreamTransport)? = { $0 },
        itemStreamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        },
        scheduleStreamTransportProvider: @escaping @MainActor @Sendable
            (DayWeaveAPIClient) -> (any DayWeaveScheduleStreamTransport)? = { $0 },
        scheduleStreamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        },
        scheduleReplicaRequiresDurableBinding: Bool = true,
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.planner = planner
        self.configurationStore = configurationStore
        self.tokenStore = tokenStore
        self.authCoordinator = authCoordinator
        self.session = session
        self.localComposer = localComposer
        self.habitCompositionProvider = habitCompositionProvider
        self.createPushLimit = max(0, createPushLimit)
        self.authoringPushLimit = max(0, authoringPushLimit)
        self.statusPushLimit = max(0, statusPushLimit)
        self.previousAssignmentLimit = max(0, previousAssignmentLimit)
        self.previousAssignmentBlockLimit = max(0, previousAssignmentBlockLimit)
        self.itemStreamTransportProvider = itemStreamTransportProvider
        self.itemStreamSleep = itemStreamSleep
        self.scheduleStreamTransportProvider = scheduleStreamTransportProvider
        self.scheduleStreamSleep = scheduleStreamSleep
        self.scheduleReplicaRequiresDurableBinding = scheduleReplicaRequiresDurableBinding
        self.now = now
        status = .ready
        reloadConfigurationStatus()
        planner.observeCommittedScheduleProfileChanges { [weak self] in
            self?.scheduleProfileDidCommit()
        }
        habitCompositionProvider?.observeHabitCompositionCheckpointChanges { [weak self] in
            self?.habitCompositionCheckpointDidChange()
        }
    }

    var isConfigured: Bool {
        makeClient(reportFailure: false) != nil
    }

    /// Side-effect-free eligibility for UI commands. The operation repeats
    /// this fail-closed preflight immediately before acquiring the mutation
    /// fence, so this value is only an enablement hint rather than authority.
    var canRecomposeLocally: Bool {
        do {
            _ = try requireLocalCompositionPreflight()
            return true
        } catch {
            return false
        }
    }

    func configurationDidChange() {
        stopForegroundItemInvalidations()
        configurationGeneration &+= 1
        activeSyncTask?.cancel()
        activeLocalCompositionTask?.cancel()
        planner.invalidateCanonicalPreview()
        lastPreview = nil
        warnings = []
        clearTransientLocalComposition()
        reloadConfigurationStatus()
        if planner.pendingSchedulePublication != nil {
            status = .failed(
                "A schedule publication is awaiting exact recovery. Restore its original API configuration and authentication, then sync before replacing or resetting this connection."
            )
        }
    }

    /// Starts content-free foreground item delivery beside a lightweight
    /// delta probe. The coordinator calls this only after the activation
    /// bootstrap has attempted to establish the durable URL/auth binding and
    /// item cursor; each delivery path independently rechecks that binding.
    func startForegroundItemInvalidations(every interval: Duration = .seconds(30)) {
        guard foregroundItemPollTask == nil else { return }
        foregroundItemStreamUnavailableForActivation = false
        foregroundItemGeneration &+= 1
        let generation = foregroundItemGeneration
        foregroundItemPollTask = Task { @MainActor [weak self] in
            guard let self else { return }
            if let configurationIdentifier = self.planner.canonicalConfigurationIdentifier,
               self.configurationSupportsScheduleReplica(configurationIdentifier) {
                await self.probeForegroundItemDelta(generation: generation)
            }
            await self.probeForegroundSchedule(generation: generation)
            self.startForegroundItemStreamIfReady()
            self.startForegroundScheduleStreamIfReady()
            while self.foregroundItemIsCurrent(generation) {
                do {
                    try await self.itemStreamSleep(interval)
                } catch {
                    return
                }
                guard self.foregroundItemIsCurrent(generation) else { return }
                await self.probeForegroundItemDelta(generation: generation)
                await self.probeForegroundSchedule(generation: generation)
                self.startForegroundItemStreamIfReady()
                self.startForegroundScheduleStreamIfReady()
            }
        }
    }

    func stopForegroundItemInvalidations() {
        foregroundItemPollTask?.cancel()
        foregroundItemPollTask = nil
        foregroundItemGeneration &+= 1
        foregroundItemStreamTask?.cancel()
        foregroundItemStreamTask = nil
        foregroundItemDrainTask?.cancel()
        foregroundItemDrainTask = nil
        if let foregroundItemOperationID, activeSyncID == foregroundItemOperationID {
            activeSyncTask?.cancel()
        }
        foregroundItemOperationID = nil
        foregroundItemObservationGeneration = 0
        foregroundItemReconciledGeneration = 0
        foregroundItemLatestHintCursor = nil
        foregroundItemStreamUnavailableForActivation = false
        foregroundItemImmediateAttempts = 0
        foregroundPublicationRepairRequired = false
        foregroundScheduleStreamTask?.cancel()
        foregroundScheduleStreamTask = nil
        foregroundScheduleDrainTask?.cancel()
        foregroundScheduleDrainTask = nil
        foregroundScheduleLatestHintRevision = 0
        foregroundScheduleReconciledRevision = 0
        foregroundScheduleStreamUnavailableForActivation = false
        foregroundScheduleImmediateAttempts = 0
        foregroundScheduleRefreshInProgress = false
        foregroundScheduleEpochResetFence = nil
    }

    private func startForegroundItemStreamIfReady() {
        guard foregroundItemPollTask != nil,
              foregroundItemStreamTask == nil,
              !foregroundItemStreamUnavailableForActivation,
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              let durableCursor = planner.canonicalDeltaCursor,
              DayWeaveItemCursorContract.isValidTransportToken(durableCursor),
              let client = makeClient(reportFailure: false),
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
              let transport = itemStreamTransportProvider(client),
              canonicalClientIsCurrent(client) else { return }
        let generation = foregroundItemGeneration
        foregroundItemStreamTask = Task { @MainActor [weak self] in
            await self?.runForegroundItemStream(
                initialTransport: transport,
                generation: generation
            )
        }
    }

    private func runForegroundItemStream(
        initialTransport: any DayWeaveItemStreamTransport,
        generation: UInt64
    ) async {
        var retrySeconds = 1
        var nextTransport: (any DayWeaveItemStreamTransport)? = initialTransport
        defer {
            if generation == foregroundItemGeneration {
                foregroundItemStreamTask = nil
            }
        }
        while foregroundItemIsCurrent(generation) {
            let client: DayWeaveAPIClient
            if let current = makeClient(reportFailure: false) {
                client = current
            } else {
                return
            }
            guard planner.hasEncryptedPersistence,
                  planner.canPersistPlan,
                  planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
                  canonicalClientIsCurrent(client),
                  let durableCursor = planner.canonicalDeltaCursor,
                  DayWeaveItemCursorContract.isValidTransportToken(durableCursor),
                  let transport = nextTransport ?? itemStreamTransportProvider(client) else {
                return
            }
            nextTransport = nil
            foregroundItemImmediateAttempts = 0
            var reconnectDelaySeconds = 1
            do {
                let completion = try await transport.consumeItemInvalidations(
                    after: durableCursor
                ) { [weak self] cursor in
                    await self?.acceptForegroundItemHint(
                        cursor,
                        configurationIdentifier: client.configurationIdentifier,
                        generation: generation
                    )
                }
                guard foregroundItemIsCurrent(generation) else { return }
                switch completion {
                case .unsupported:
                    foregroundItemStreamUnavailableForActivation = true
                    return
                case .endOfStream:
                    reconnectDelaySeconds = retrySeconds
                    retrySeconds = min(retrySeconds * 2, 30)
                case .liveEndOfStream:
                    retrySeconds = 1
                    reconnectDelaySeconds = 1
                }
            } catch {
                guard foregroundItemIsCurrent(generation) else { return }
                guard itemStreamFailureIsTransient(error) else {
                    foregroundItemStreamUnavailableForActivation = true
                    return
                }
                reconnectDelaySeconds = retrySeconds
                retrySeconds = min(retrySeconds * 2, 30)
            }
            do {
                try await itemStreamSleep(.seconds(reconnectDelaySeconds))
            } catch {
                return
            }
        }
    }

    private func acceptForegroundItemHint(
        _ cursor: String,
        configurationIdentifier: String,
        generation: UInt64
    ) {
        guard foregroundItemIsCurrent(generation),
              DayWeaveItemCursorContract.isValidTransportToken(cursor),
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              planner.canonicalConfigurationIdentifier == configurationIdentifier,
              makeClient(reportFailure: false)?.configurationIdentifier
                == configurationIdentifier else { return }
        if cursor == planner.canonicalDeltaCursor {
            return
        }
        foregroundItemObservationGeneration &+= 1
        foregroundItemLatestHintCursor = cursor
        enqueueForegroundItemDrain(generation: generation)
    }

    private func enqueueForegroundItemDrain(generation: UInt64) {
        guard foregroundItemIsCurrent(generation), foregroundItemDrainTask == nil else { return }
        foregroundItemDrainTask = Task { @MainActor [weak self] in
            await self?.drainForegroundItemObservations(generation: generation)
        }
    }

    private func drainForegroundItemObservations(generation: UInt64) async {
        var drainImmediateAttempts = 0
        defer {
            if generation == foregroundItemGeneration {
                foregroundItemDrainTask = nil
            }
        }
        while foregroundItemIsCurrent(generation) {
            let targetGeneration = foregroundItemObservationGeneration
            guard targetGeneration > foregroundItemReconciledGeneration
                    || foregroundPublicationRepairRequired else { return }
            guard planner.hasEncryptedPersistence, planner.canPersistPlan else { return }

            if !foregroundPublicationRepairRequired,
               let hintedCursor = foregroundItemLatestHintCursor,
               hintedCursor == planner.canonicalDeltaCursor {
                foregroundItemReconciledGeneration = targetGeneration
                foregroundItemLatestHintCursor = nil
                continue
            }
            // Keep a per-drain ceiling as well as the activation admission
            // counter. A stream reconnect may replenish later admission while
            // this task is suspended in URLSession, but cannot extend this
            // drain beyond its initial attempt plus one immediate follow-up.
            guard drainImmediateAttempts < 2,
                  foregroundItemImmediateAttempts < 2 else { return }
            drainImmediateAttempts += 1
            foregroundItemImmediateAttempts += 1
            let succeeded = await reconcileForegroundItemChanges(
                generation: generation
            )
            guard succeeded, foregroundItemIsCurrent(generation) else { return }
            if foregroundItemLatestHintCursor == planner.canonicalDeltaCursor {
                // Equality with the authoritative delta cursor proves that
                // every observation through the latest opaque hint was
                // covered, including hints received while this drain was in
                // flight. Opaque cursors are never compared or ordered.
                foregroundItemReconciledGeneration = foregroundItemObservationGeneration
                foregroundItemLatestHintCursor = nil
            } else {
                foregroundItemReconciledGeneration = targetGeneration
            }
            if foregroundItemObservationGeneration == targetGeneration,
               !foregroundPublicationRepairRequired {
                return
            }
        }
    }

    private func probeForegroundItemDelta(generation: UInt64) async {
        guard foregroundItemIsCurrent(generation),
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              activeSyncID == nil,
              let durableCursor = planner.canonicalDeltaCursor,
              DayWeaveItemCursorContract.isValidTransportToken(durableCursor),
              let client = makeClient(reportFailure: false),
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
              canonicalClientIsCurrent(client) else { return }
        do {
            let page = try await client.itemDelta(cursor: durableCursor, limit: 1)
            guard foregroundItemIsCurrent(generation),
                  canonicalClientIsCurrent(client),
                  planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
                  planner.canonicalDeltaCursor == durableCursor,
                  DayWeaveItemCursorContract.isValidTransportToken(page.nextCursor) else { return }
            let changed = !page.changes.isEmpty
                || page.hasMore
                || page.nextCursor != durableCursor
            if changed {
                foregroundItemObservationGeneration &+= 1
                foregroundItemLatestHintCursor = page.changes.isEmpty && !page.hasMore
                    ? page.nextCursor
                    : nil
            }
        } catch let error as DayWeaveAPIError {
            guard foregroundItemIsCurrent(generation),
                  canonicalClientIsCurrent(client),
                  planner.canonicalDeltaCursor == durableCursor else { return }
            if case let .server(statusCode, _, _, _) = error, statusCode == 422 {
                foregroundItemObservationGeneration &+= 1
                foregroundItemLatestHintCursor = nil
            }
        } catch {
            return
        }
        foregroundItemImmediateAttempts = 0
        if foregroundItemObservationGeneration > foregroundItemReconciledGeneration
            || foregroundPublicationRepairRequired {
            enqueueForegroundItemDrain(generation: generation)
        }
    }

    private func foregroundItemIsCurrent(_ generation: UInt64) -> Bool {
        !Task.isCancelled
            && foregroundItemPollTask != nil
            && generation == foregroundItemGeneration
    }

    private func canonicalClientIsCurrent(_ client: DayWeaveAPIClient) -> Bool {
        makeClient(reportFailure: false)?.configurationIdentifier
            == client.configurationIdentifier
            && planner.canonicalConfigurationIdentifier == client.configurationIdentifier
    }

    private func itemStreamFailureIsTransient(_ error: Error) -> Bool {
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

    private func startForegroundScheduleStreamIfReady() {
        guard foregroundItemPollTask != nil,
              foregroundScheduleStreamTask == nil,
              !foregroundScheduleStreamUnavailableForActivation,
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              planner.pendingSchedulePublication == nil,
              let client = makeClient(reportFailure: false),
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
              configurationSupportsScheduleReplica(client.configurationIdentifier),
              let transport = scheduleStreamTransportProvider(client),
              canonicalClientIsCurrent(client) else { return }
        let generation = foregroundItemGeneration
        foregroundScheduleStreamTask = Task { @MainActor [weak self] in
            await self?.runForegroundScheduleStream(
                initialTransport: transport,
                generation: generation
            )
        }
    }

    private func runForegroundScheduleStream(
        initialTransport: any DayWeaveScheduleStreamTransport,
        generation: UInt64
    ) async {
        var retrySeconds = 1
        var nextTransport: (any DayWeaveScheduleStreamTransport)? = initialTransport
        defer {
            if generation == foregroundItemGeneration {
                foregroundScheduleStreamTask = nil
            }
        }
        while foregroundItemIsCurrent(generation) {
            guard let client = makeClient(reportFailure: false),
                  planner.hasEncryptedPersistence,
                  planner.canPersistPlan,
                  planner.pendingSchedulePublication == nil,
                  planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
                  configurationSupportsScheduleReplica(client.configurationIdentifier),
                  canonicalClientIsCurrent(client),
                  let transport = nextTransport ?? scheduleStreamTransportProvider(client) else {
                return
            }
            nextTransport = nil
            foregroundScheduleImmediateAttempts = 0
            let durableRevision = durablePublishedScheduleRevision(
                configurationIdentifier: client.configurationIdentifier
            )
            var reconnectDelaySeconds = 1
            do {
                let completion = try await transport.consumeScheduleInvalidations(
                    after: durableRevision
                ) { [weak self] revision in
                    await self?.acceptForegroundScheduleHint(
                        revision,
                        configurationIdentifier: client.configurationIdentifier,
                        generation: generation
                    )
                }
                guard foregroundItemIsCurrent(generation) else { return }
                switch completion {
                case .endOfStream:
                    reconnectDelaySeconds = retrySeconds
                    retrySeconds = min(retrySeconds * 2, 30)
                case .liveEndOfStream:
                    retrySeconds = 1
                    reconnectDelaySeconds = 1
                case .cursorAhead:
                    // The numeric server head is diagnostic only. The exact
                    // rejected request cursor is bound into a one-use fence;
                    // only a subsequent current GET may lower durable state.
                    foregroundScheduleEpochResetFence = .init(
                        configurationIdentifier: client.configurationIdentifier,
                        rejectedRevision: durableRevision
                    )
                    _ = await refreshForegroundSchedule(generation: generation)
                    retrySeconds = 1
                    reconnectDelaySeconds = 1
                }
            } catch {
                guard foregroundItemIsCurrent(generation) else { return }
                guard itemStreamFailureIsTransient(error) else {
                    foregroundScheduleStreamUnavailableForActivation = true
                    return
                }
                reconnectDelaySeconds = retrySeconds
                retrySeconds = min(retrySeconds * 2, 30)
            }
            do {
                try await scheduleStreamSleep(.seconds(reconnectDelaySeconds))
            } catch {
                return
            }
        }
    }

    private func acceptForegroundScheduleHint(
        _ revision: UInt64,
        configurationIdentifier: String,
        generation: UInt64
    ) {
        guard foregroundItemIsCurrent(generation),
              revision > 0,
              revision <= UInt64(Int64.max),
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              planner.canonicalConfigurationIdentifier == configurationIdentifier,
              makeClient(reportFailure: false)?.configurationIdentifier
                == configurationIdentifier else { return }
        let durable = durablePublishedScheduleRevision(
            configurationIdentifier: configurationIdentifier
        )
        guard revision > durable else { return }
        do {
            try planner.persistPublishedScheduleRevisionHint(revision)
        } catch {
            // The hint is not accepted until its high-water is encrypted. The
            // planner already failed closed on a save error, and a later
            // activation/reload can safely retry from durable state.
            return
        }
        foregroundScheduleLatestHintRevision = max(
            foregroundScheduleLatestHintRevision,
            revision
        )
        enqueueForegroundScheduleDrain(generation: generation)
    }

    private func enqueueForegroundScheduleDrain(generation: UInt64) {
        guard foregroundItemIsCurrent(generation),
              foregroundScheduleDrainTask == nil else { return }
        foregroundScheduleDrainTask = Task { @MainActor [weak self] in
            await self?.drainForegroundScheduleObservations(generation: generation)
        }
    }

    private func drainForegroundScheduleObservations(generation: UInt64) async {
        var attempts = 0
        defer {
            if generation == foregroundItemGeneration {
                foregroundScheduleDrainTask = nil
            }
        }
        while foregroundItemIsCurrent(generation), attempts < 2 {
            guard let configurationIdentifier = planner.canonicalConfigurationIdentifier else {
                return
            }
            let target = foregroundScheduleLatestHintRevision
            let durable = installedPublishedScheduleRevision(
                configurationIdentifier: configurationIdentifier
            )
            if durable >= target {
                foregroundScheduleReconciledRevision = durable
                return
            }
            guard foregroundScheduleImmediateAttempts < 2 else { return }
            attempts += 1
            foregroundScheduleImmediateAttempts += 1
            guard await refreshForegroundSchedule(generation: generation) else { return }
        }
    }

    private func probeForegroundSchedule(generation: UInt64) async {
        guard foregroundItemIsCurrent(generation),
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              planner.pendingSchedulePublication == nil,
              activeSyncID == nil,
              let configurationIdentifier = planner.canonicalConfigurationIdentifier,
              configurationSupportsScheduleReplica(configurationIdentifier) else { return }
        foregroundScheduleImmediateAttempts = 0
        _ = await refreshForegroundSchedule(generation: generation)
    }

    private func refreshForegroundSchedule(generation: UInt64) async -> Bool {
        guard foregroundItemIsCurrent(generation),
              !foregroundScheduleRefreshInProgress,
              !hasPendingScheduleReplicaWrites,
              await waitForCanonicalMutationFence(reportFailure: false) else { return false }
        foregroundScheduleRefreshInProgress = true
        defer {
            foregroundScheduleRefreshInProgress = false
            planner.endCanonicalSync()
        }
        guard foregroundItemIsCurrent(generation),
              !hasPendingScheduleReplicaWrites,
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              let client = makeClient(reportFailure: false),
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
              configurationSupportsScheduleReplica(client.configurationIdentifier),
              canonicalClientIsCurrent(client) else { return false }
        let epochResetFence = foregroundScheduleEpochResetFence.flatMap { fence in
            fence.configurationIdentifier == client.configurationIdentifier
                ? fence
                : nil
        }
        do {
            var current = try await client.currentPublishedSchedule()
            guard foregroundItemIsCurrent(generation),
                  canonicalClientIsCurrent(client),
                  planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
                  !hasPendingScheduleReplicaWrites else { return false }
            guard let initialCurrent = current else {
                try planner.clearCurrentPublishedSchedule(
                    configurationIdentifier: client.configurationIdentifier,
                    revisionEpochResetFence: epochResetFence
                )
                lastPreview = nil
                foregroundScheduleReconciledRevision = 0
                if epochResetFence != nil {
                    foregroundScheduleLatestHintRevision = 0
                    foregroundScheduleEpochResetFence = nil
                }
                return true
            }
            do {
                try validateReplicatedSchedule(initialCurrent)
            } catch CanonicalSyncError.sourceRevisionMismatch {
                // Publication and item invalidations are independent hints.
                // If the publication wins that race, drain the authoritative
                // item delta under this same mutation fence and refetch the
                // current head instead of waiting for the next 30-second poll.
                try await pullCanonicalItemsForScheduleReplica(
                    client: client,
                    generation: generation
                )
                current = try await client.currentPublishedSchedule()
                guard foregroundItemIsCurrent(generation),
                      canonicalClientIsCurrent(client),
                      planner.canonicalConfigurationIdentifier
                        == client.configurationIdentifier,
                      !hasPendingScheduleReplicaWrites else { return false }
                guard let current else {
                    try planner.clearCurrentPublishedSchedule(
                        configurationIdentifier: client.configurationIdentifier,
                        revisionEpochResetFence: epochResetFence
                    )
                    lastPreview = nil
                    foregroundScheduleReconciledRevision = 0
                    if epochResetFence != nil {
                        foregroundScheduleLatestHintRevision = 0
                        foregroundScheduleEpochResetFence = nil
                    }
                    return true
                }
                try validateReplicatedSchedule(current)
            }
            guard let current else { return false }
            warnings = []
            let rendered = render(current.schedule)
            let message = "Recovered published schedule revision \(current.revision.revisionNumber) · \(Self.composedBlockSummary(current.schedule.plan.blocks))"
            try planner.installCurrentPublishedSchedule(
                current,
                blocks: rendered,
                configurationIdentifier: client.configurationIdentifier,
                message: message,
                revisionEpochResetFence: epochResetFence
            )
            clearTransientLocalComposition()
            lastPreview = current.schedule
            foregroundScheduleReconciledRevision = current.revision.revisionNumber
            if epochResetFence != nil {
                foregroundScheduleLatestHintRevision = current.revision.revisionNumber
                foregroundScheduleEpochResetFence = nil
            }
            return true
        } catch {
            return false
        }
    }

    private func pullCanonicalItemsForScheduleReplica(
        client: DayWeaveAPIClient,
        generation: UInt64
    ) async throws {
        func ensureCurrent() throws {
            guard foregroundItemIsCurrent(generation),
                  canonicalClientIsCurrent(client),
                  planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
                  !hasPendingScheduleReplicaWrites else {
                throw CanonicalSyncError.operationSuperseded
            }
        }
        do {
            let result = try await loadDelta(
                client: client,
                from: planner.canonicalDeltaCursor,
                enforceItemCursorContract: true
            )
            try ensureCurrent()
            _ = try planner.applyCanonicalDeltaDurably(
                result.changes,
                nextCursor: result.cursor
            )
        } catch let error as DayWeaveAPIError {
            guard case let .server(statusCode, _, _, _) = error,
                  statusCode == 422,
                  planner.canonicalDeltaCursor != nil else { throw error }
            try ensureCurrent()
            let result = try await loadDelta(
                client: client,
                from: nil,
                enforceItemCursorContract: true
            )
            try ensureCurrent()
            _ = try planner.replaceCanonicalStateDurably(
                changes: result.changes,
                nextCursor: result.cursor
            )
        }
        if foregroundItemLatestHintCursor == planner.canonicalDeltaCursor {
            foregroundItemReconciledGeneration = foregroundItemObservationGeneration
            foregroundItemLatestHintCursor = nil
        }
    }

    private var hasPendingScheduleReplicaWrites: Bool {
        planner.pendingSchedulePublication != nil
            || !planner.pendingCanonicalMutations.isEmpty
            || !planner.pendingCanonicalSensitivityMutations.isEmpty
            || !planner.pendingCanonicalAuthoringMutations.isEmpty
            || planner.hasDeferredExecutionPublicationWork
    }

    private func durablePublishedScheduleRevision(
        configurationIdentifier: String
    ) -> UInt64 {
        guard planner.canonicalConfigurationIdentifier == configurationIdentifier else {
            return 0
        }
        return max(
            installedPublishedScheduleRevision(
                configurationIdentifier: configurationIdentifier
            ),
            planner.publishedScheduleLatestHintRevision
        )
    }

    private func installedPublishedScheduleRevision(
        configurationIdentifier: String
    ) -> UInt64 {
        guard let proof = planner.publishedScheduleProof,
              proof.hasCurrentImmutablePlanSeal,
              proof.configurationIdentifier == configurationIdentifier else { return 0 }
        return proof.revisionNumber
    }

    private func configurationSupportsScheduleReplica(_ identifier: String) -> Bool {
        !scheduleReplicaRequiresDurableBinding || identifier.contains("|auth=device-v1:")
    }

    private func validateReplicatedSchedule(
        _ publication: DayWeaveCurrentPublishedSchedule
    ) throws {
        let schedule = publication.schedule
        let revision = publication.revision
        let localRevisions = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
        )
        guard schedule.sourceItemCount == planner.canonicalItems.count,
              schedule.sourceItemRevisions == localRevisions else {
            throw CanonicalSyncError.sourceRevisionMismatch
        }
        guard schedule.acceptedItemCount + schedule.rejectedItems.count
                == schedule.sourceItemCount,
              schedule.ignoredPreviousAssignments.count <= 10_000,
              schedule.manualPlacementAssessments.count <= 10_000,
              revision.inputDigest == schedule.inputDigest,
              sameInstant(revision.horizonStart, schedule.plan.horizonStart),
              sameInstant(revision.horizonEnd, schedule.plan.horizonEnd),
              revision.publishedAt.timeIntervalSinceReferenceDate.isFinite,
              revision.publishedAt <= now().addingTimeInterval(5 * 60),
              DayWeaveCanonicalItemDraft.supportedTimeZone(
                  identifier: revision.timezoneName
              ) != nil,
              revision.revisionNumber > 0,
              revision.revision
                == "\(revision.revisionNumber):\(revision.id.uuidString.lowercased())" else {
            throw CanonicalSyncError.invalidSchedulePublication
        }
        let fixedBlocks = schedule.plan.blocks.compactMap { block
            -> DayWeaveSchedulePreviewRequest.FixedBlock? in
            guard block.kind == "external_fixed", let sourceID = block.externalBlockID else {
                return nil
            }
            return .init(
                id: sourceID,
                isSensitive: block.isSensitive,
                title: block.title,
                start: block.start,
                end: block.end,
                source: "published_schedule_replica"
            )
        }
        let request = DayWeaveSchedulePreviewRequest(
            asOf: schedule.plan.asOf,
            horizonStart: schedule.plan.horizonStart,
            horizonEnd: schedule.plan.horizonEnd,
            timezoneName: revision.timezoneName,
            availability: [],
            fixedBlocks: fixedBlocks,
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 0,
                defaultSoftWeight: 0
            ),
            recurrenceContext: [:]
        )
        try validate(preview: schedule, against: request)
    }

    func resetCanonicalSyncState() {
        guard activeSyncID == nil, activeLocalCompositionID == nil else { return }
        guard planner.pendingSchedulePublication == nil else {
            status = .failed(
                "An exact schedule publication may already be committed remotely. Restore its original API configuration and authentication, then sync to recover it before resetting local state."
            )
            return
        }
        planner.resetCanonicalSyncState()
        lastPreview = nil
        warnings = []
        clearTransientLocalComposition()
        reloadConfigurationStatus()
    }

    /// Composes from the complete encrypted canonical cache without making a
    /// network request or creating a server publication journal.
    @discardableResult
    func recomposeLocally() async -> Bool {
        let habitCheckpoint: HabitCompositionCheckpoint?
        do {
            habitCheckpoint = try requireLocalCompositionPreflight()
        } catch {
            reportLocalCompositionFailure(error)
            return false
        }
        guard planner.beginCanonicalSync() else {
            reportLocalCompositionFailure(LocalCompositionCoordinatorError.busy)
            return false
        }

        let operationID = UUID()
        let generation = configurationGeneration
        activeLocalCompositionID = operationID
        activeLocalCompositionScheduleProfile = planner.scheduleProfile
        isLocallyComposing = true
        localCompositionWarnings = []
        localCompositionStatus = .composing("Composing seven days on this Mac…")

        let succeeded: Bool
        do {
            planner.flushPersistence()
            if let persistenceError = planner.persistenceError { throw persistenceError }
            try ensureLocalCompositionCurrent(
                operationID: operationID,
                generation: generation
            )

            let priorWarningCount = warnings.count
            let request = try makePreviewRequest(habitCheckpoint: habitCheckpoint)
            let requestWarnings = Array(warnings.dropFirst(priorWarningCount))
            if warnings.count > priorWarningCount {
                warnings.removeLast(warnings.count - priorWarningCount)
            }
            let canonicalItems = planner.canonicalItems
            let capturedRevisions = Dictionary(
                uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
            )
            let fence = LocalCompositionMutationFence(
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: planner.canonicalDeltaCursor,
                canonicalConfigurationIdentifier: planner.canonicalConfigurationIdentifier,
                completedOccurrenceIDs: planner.completedOccurrenceIDs,
                recurrenceSessionOutcomes: planner.recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: planner.recurrenceOccurrenceMoves,
                deferredExecutionPublicationSessionIDs:
                    planner.deferredExecutionPublicationSessionIDs,
                blocks: planner.blocks,
                publishedScheduleProof: planner.publishedScheduleProof,
                publishedScheduleLatestHintRevision:
                    planner.publishedScheduleLatestHintRevision,
                scheduleProfile: planner.scheduleProfile,
                freezeHours: planner.freezeHours,
                timezoneName: planningTimezone,
                habitCompositionCheckpoint: habitCheckpoint
            )
            let composer = localComposer
            let task = Task.detached(priority: .userInitiated) {
                try await composer.compose(
                    canonicalItems: canonicalItems,
                    schedule: request
                )
            }
            activeLocalCompositionTask = task
            let composition = try await withTaskCancellationHandler {
                try await task.value
            } onCancel: {
                task.cancel()
            }

            try ensureLocalCompositionCurrent(
                operationID: operationID,
                generation: generation
            )
            guard fence.matches(
                planner: planner,
                timezoneName: planningTimezone,
                currentHabitCheckpoint: habitCompositionProvider?.habitCompositionCheckpoint
            ) else {
                throw LocalCompositionCoordinatorError.canonicalStateChanged
            }
            guard composition.sourceItemCount == canonicalItems.count,
                  composition.sourceItemRevisions == capturedRevisions,
                  composition.sourceItemRevisions == Dictionary(
                      uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
                  ),
                  composition.acceptedItemCount >= 0,
                  composition.acceptedItemCount <= composition.sourceItemCount,
                  composition.rejectedItems.count
                    == composition.sourceItemCount - composition.acceptedItemCount,
                  composition.ignoredPreviousAssignments.count <= 10_000 else {
                throw LocalCompositionCoordinatorError.invalidHelperResponse
            }
            try validate(
                plan: composition.plan,
                sourceItemRevisions: composition.sourceItemRevisions,
                rejectedItems: composition.rejectedItems,
                against: request
            )

            let generatedAt = now()
            var calendar = Calendar(identifier: .gregorian)
            guard let timezone = TimeZone(identifier: request.timezoneName) else {
                throw LocalCompositionCoordinatorError.invalidHelperResponse
            }
            calendar.timeZone = timezone
            guard planningTimezone == request.timezoneName,
                  calendar.isDate(request.asOf, inSameDayAs: generatedAt),
                  generatedAt >= request.horizonStart,
                  generatedAt < request.horizonEnd else {
                throw LocalCompositionCoordinatorError.canonicalStateChanged
            }
            let provenance = LocalScheduleCompositionProvenance(
                configurationIdentifier: fence.canonicalConfigurationIdentifier ?? "",
                localInputFingerprint: composition.localInputFingerprint,
                generatedAt: generatedAt,
                asOf: composition.plan.asOf,
                horizonStart: composition.plan.horizonStart,
                horizonEnd: composition.plan.horizonEnd,
                timezoneName: request.timezoneName,
                sourceItemRevisions: composition.sourceItemRevisions,
                habitCheckpointFingerprint: habitCheckpoint?.fingerprint
            )
            guard provenance.hasValidShape else {
                throw LocalCompositionCoordinatorError.invalidHelperResponse
            }
            let renderWarningCount = warnings.count
            let rendered = render(
                plan: composition.plan,
                sourceItemRevisions: composition.sourceItemRevisions,
                origin: .localComposition
            )
            let renderWarnings = Array(warnings.dropFirst(renderWarningCount))
            if warnings.count > renderWarningCount {
                warnings.removeLast(warnings.count - renderWarningCount)
            }
            var localWarnings = requestWarnings + renderWarnings
            localWarnings.append(contentsOf: composition.rejectedItems.map {
                "“\($0.title)” was excluded on this device: \($0.reason)"
            })
            localWarnings.append(contentsOf: composition.ignoredPreviousAssignments.map {
                "A previous assignment for \($0.itemID.uuidString.lowercased()) was ignored on this device: \($0.reason)"
            })
            let message = "On-device schedule · composed \(Self.composedBlockSummary(composition.plan.blocks)) · \(composition.plan.score.unscheduledMinutes)m unscheduled · not published"
            try planner.commitLocalScheduleComposition(
                blocks: rendered,
                message: message,
                provenance: provenance
            )
            // The durable local install is now authoritative. Do not expose
            // evidence from an older server candidate beside it.
            lastPreview = nil
            warnings = []
            lastLocalComposition = composition
            lastLocalCompositionScore = composition.plan.score
            localCompositionWarnings = localWarnings
            localCompositionStatus = .composed(
                generatedAt: generatedAt,
                message: message
            )
            succeeded = true
        } catch {
            if activeLocalCompositionID == operationID,
               generation == configurationGeneration {
                reportLocalCompositionFailure(error)
            }
            succeeded = false
        }

        if activeLocalCompositionID == operationID {
            activeLocalCompositionTask = nil
            activeLocalCompositionID = nil
            activeLocalCompositionScheduleProfile = nil
            isLocallyComposing = false
            planner.endCanonicalSync()
        }
        return succeeded
    }

    func sync() async {
        _ = await syncReportingSuccess()
    }

    /// Persists the user's move as a canonical constraint replacement before
    /// any network write, then publishes a fresh schedule from that revision.
    /// This is the authoritative path for work that has not started yet; an
    /// open execution lease uses ExecutionSyncStore.defer instead.
    @discardableResult
    func moveCanonicalWorkLater(
        _ blockID: UUID,
        earliestStart: Date,
        reviewedRisk: DayWeaveMoveRiskEnvelope? = nil,
        allowDeadlineConflict: Bool = false,
        allowFixedConflicts: Bool = false
    ) async -> Bool {
        do {
            guard let block = planner.blocks.first(where: { $0.id == blockID }),
                  let currentRisk = moveLaterRisk(
                    for: block,
                    earliestStart: earliestStart
                  ) else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            if let reviewedRisk {
                guard reviewedRisk.hasValidShape,
                      reviewedRisk.moveStart == currentRisk.moveStart,
                      reviewedRisk.moveEnd == currentRisk.moveEnd,
                      reviewedRisk.deadlines == currentRisk.deadlines,
                      currentRisk.fixedConflicts.isSubset(
                        of: reviewedRisk.fixedConflicts
                      ) else {
                    throw PlannerCanonicalAuthoringError.invalidDraft
                }
            } else {
                guard currentRisk.fixedConflicts.isEmpty else {
                    throw PlannerCanonicalAuthoringError.invalidDraft
                }
            }
            guard let currentWindow = WillDoLaterTiming.proposedWindow(
                for: block,
                moveStart: earliestStart,
                allBlocks: planner.blocks,
                accumulatedSeconds: nil
            ) else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            let crossedDeadlines = WillDoLaterTiming.crossedDeadlines(
                currentRisk.deadlines,
                window: currentWindow
            )
            let crossesDeadline = !crossedDeadlines.isEmpty
            guard allowDeadlineConflict == crossesDeadline,
                  allowFixedConflicts == !currentRisk.fixedConflicts.isEmpty else {
                throw PlannerCanonicalAuthoringError.invalidDraft
            }
            let crossedHardDeadlines = crossedDeadlines.filter(\.boundary.isHard)
            if !crossedHardDeadlines.isEmpty,
               !(block.occurrenceID == nil
                    && crossedHardDeadlines.allSatisfy(\.boundary.isCanonicalField)) {
                throw PlannerCanonicalAuthoringError.unsupportedReplacement
            }
            if block.occurrenceID != nil {
                _ = try planner.enqueueCanonicalOccurrenceMove(
                    blockID: blockID,
                    moveStart: earliestStart
                )
            } else {
                _ = try planner.enqueueCanonicalMoveLater(
                    blockID: blockID,
                    earliestStart: earliestStart,
                    relaxCanonicalDeadlineTo: crossedDeadlines.contains(where: {
                        $0.boundary.isCanonicalField
                    })
                        ? currentRisk.moveEnd
                        : nil
                )
            }
        } catch {
            status = .failed(error.localizedDescription)
            return false
        }
        return await syncThroughFreshComposition()
    }

    private func moveLaterRisk(
        for block: ScheduleBlock,
        earliestStart: Date
    ) -> DayWeaveMoveRiskEnvelope? {
        guard let window = WillDoLaterTiming.proposedWindow(
            for: block,
            moveStart: earliestStart,
            allBlocks: planner.blocks,
            accumulatedSeconds: nil
        ) else { return nil }
        guard let deadlines = DayWeaveMoveDeadlinePolicy.identities(
            for: block,
            movingWholeOccurrence: block.occurrenceID != nil,
            allBlocks: planner.blocks,
            canonicalItems: planner.canonicalItems
        ) else { return nil }
        let risk = DayWeaveMoveRiskEnvelope(
            moveStart: window.start,
            moveEnd: window.end,
            deadlines: deadlines,
            // Scheduled work is recomposed by the authoritative preview. Its
            // current outer window is not an exact placement and therefore
            // cannot support a truthful fixed-overlap approval.
            fixedConflicts: [],
            sourceRequiresOverride: false
        )
        return risk.hasValidShape ? risk : nil
    }

    private func syncReportingSuccess() async -> Bool {
        guard await waitForCanonicalMutationFence() else { return false }
        // The normal sync path now owns the exclusive mutation fence and is
        // about to invalidate the durable local composition. Clear its
        // transient score/status at the same boundary, including when client
        // construction or the later network attempt fails.
        clearTransientLocalComposition()
        // Invalidate before reading configuration or Keychain state. An
        // out-of-process change must not leave an old preview actionable just
        // because client construction fails.
        planner.invalidateCanonicalPreview()
        lastPreview = nil
        warnings = []
        guard let client = makeClient(reportFailure: true) else {
            planner.endCanonicalSync()
            return false
        }
        if let pending = planner.pendingSchedulePublication,
           pending.configurationIdentifier != client.configurationIdentifier {
            planner.endCanonicalSync()
            status = .failed(
                "The exact schedule publication belongs to another API URL or credential session. Restore that original configuration and authentication, then sync to recover it; it was not discarded."
            )
            return false
        }
        do {
            try planner.prepareCanonicalSync(
                configurationIdentifier: client.configurationIdentifier
            )
        } catch {
            planner.endCanonicalSync()
            status = .failed(error.localizedDescription)
            return false
        }

        let operationID = UUID()
        let generation = configurationGeneration
        activeSyncID = operationID
        activeSyncScheduleProfile = planner.scheduleProfile
        isSyncing = true
        status = .syncing("Pulling canonical item revisions…")
        let task = Task<Void, Never> { @MainActor [weak self] in
            guard let self else { return }
            await self.performSync(
                client: client,
                operationID: operationID,
                generation: generation
            )
        }
        activeSyncTask = task
        await task.value
        return lastSuccessfulSyncID == operationID
    }

    /// Foreground startup is read-first when no durable local write needs
    /// recovery. This prevents a resumed Mac from superseding another device's
    /// authoritative publication before it has read that immutable head.
    /// Explicit user/import/proposal workflows continue to use
    /// `syncThroughFreshComposition()` and therefore retain their requirement
    /// to publish a newly composed schedule.
    @discardableResult
    func bootstrapForegroundActivation() async -> Bool {
        guard await waitForCanonicalMutationFence() else { return false }
        warnings = []
        guard let client = makeClient(reportFailure: true) else {
            planner.endCanonicalSync()
            return false
        }
        if let pending = planner.pendingSchedulePublication,
           pending.configurationIdentifier != client.configurationIdentifier {
            planner.endCanonicalSync()
            status = .failed(
                "The exact schedule publication belongs to another API URL or credential session. Restore that original configuration and authentication, then sync to recover it; it was not discarded."
            )
            return false
        }
        do {
            try planner.prepareCanonicalReplicaRead(
                configurationIdentifier: client.configurationIdentifier
            )
        } catch {
            planner.endCanonicalSync()
            status = .failed(error.localizedDescription)
            return false
        }

        let requiresWriteRecovery = hasPendingForegroundCanonicalWrites
            || !configurationSupportsScheduleReplica(client.configurationIdentifier)
        if requiresWriteRecovery {
            clearTransientLocalComposition()
            planner.invalidateCanonicalPreview()
            lastPreview = nil
        }

        let operationID = UUID()
        let generation = configurationGeneration
        activeSyncID = operationID
        activeSyncScheduleProfile = planner.scheduleProfile
        isSyncing = true
        let task = Task<Void, Never> { @MainActor [weak self] in
            guard let self else { return }
            if requiresWriteRecovery {
                self.status = .syncing("Recovering pending canonical writes…")
                await self.performSync(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
            } else {
                await self.performReadFirstActivationBootstrap(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
            }
        }
        activeSyncTask = task
        await task.value
        return lastSuccessfulSyncID == operationID
    }

    /// Import completion must survive an unrelated pending publication
    /// recovery. A recovered old receipt is successful work, but it does not
    /// prove that newly imported canonical items were pulled and recomposed.
    @discardableResult
    func syncThroughFreshComposition() async -> Bool {
        for _ in 0..<2 {
            guard await syncReportingSuccess(),
                  let successfulID = lastSuccessfulSyncID else { return false }
            if lastFreshCompositionSyncID == successfulID { return true }
        }
        return false
    }

    /// A proposal completion must not lose its required pull/recomposition just
    /// because execution reconciliation owns the shared canonical mutation
    /// fence for a moment. Existing canonical work is also awaited and followed
    /// by this request, so every caller is either cancelled explicitly or gets a
    /// synchronization attempt after the work that caused contention.
    private func waitForCanonicalMutationFence(reportFailure: Bool = true) async -> Bool {
        while !Task.isCancelled {
            guard planner.pendingProposalApplicationMutation == nil else {
                if reportFailure {
                    status = .failed(
                        "Recover the exact pending proposal application or undo before synchronizing canonical items."
                    )
                }
                return false
            }
            guard planner.canPersistPlan else { return false }
            if let activeSyncTask {
                await activeSyncTask.value
                continue
            }
            if planner.beginCanonicalSync() {
                return true
            }
            guard planner.isCanonicalSyncLocked else { return false }
            do {
                try await Task.sleep(for: .milliseconds(25))
            } catch {
                return false
            }
        }
        return false
    }

    private func reconcileForegroundItemChanges(generation: UInt64) async -> Bool {
        guard foregroundItemIsCurrent(generation),
              await waitForCanonicalMutationFence(reportFailure: false) else { return false }
        guard foregroundItemIsCurrent(generation),
              planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              let client = makeClient(reportFailure: false),
              planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
              canonicalClientIsCurrent(client),
              let durableCursor = planner.canonicalDeltaCursor,
              DayWeaveItemCursorContract.isValidTransportToken(durableCursor) else {
            planner.endCanonicalSync()
            return false
        }

        let requiresFullSync = hasPendingForegroundCanonicalWrites
        if requiresFullSync {
            clearTransientLocalComposition()
            planner.invalidateCanonicalPreview()
            lastPreview = nil
            warnings = []
            do {
                try planner.prepareCanonicalSync(
                    configurationIdentifier: client.configurationIdentifier
                )
            } catch {
                planner.endCanonicalSync()
                return false
            }
        }

        let operationID = UUID()
        let operationGeneration = configurationGeneration
        activeSyncID = operationID
        activeSyncScheduleProfile = planner.scheduleProfile
        foregroundItemOperationID = operationID
        lastSuccessfulForegroundItemOperationID = nil
        isSyncing = true
        let task = Task<Void, Never> { @MainActor [weak self] in
            guard let self else { return }
            if requiresFullSync {
                self.status = .syncing("Pulling canonical item revisions…")
                await self.performSync(
                    client: client,
                    operationID: operationID,
                    generation: operationGeneration
                )
                if self.lastFreshCompositionSyncID == operationID {
                    self.lastSuccessfulForegroundItemOperationID = operationID
                }
            } else {
                await self.performForegroundItemReconciliation(
                    client: client,
                    operationID: operationID,
                    generation: operationGeneration
                )
            }
        }
        activeSyncTask = task
        await task.value
        if foregroundItemOperationID == operationID {
            foregroundItemOperationID = nil
        }
        return lastSuccessfulForegroundItemOperationID == operationID
    }

    private var hasPendingForegroundCanonicalWrites: Bool {
        planner.pendingSchedulePublication != nil
            || !planner.pendingCanonicalMutations.isEmpty
            || !planner.pendingCanonicalSensitivityMutations.isEmpty
            || !planner.pendingCanonicalAuthoringMutations.isEmpty
            || planner.hasDeferredExecutionPublicationWork
            || planner.blocks.contains {
                $0.isLocallyAuthored && $0.sourceItemID == nil
            }
    }

    private func performForegroundItemReconciliation(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async {
        defer {
            if activeSyncID == operationID {
                activeSyncTask = nil
                activeSyncID = nil
                activeSyncScheduleProfile = nil
                isSyncing = false
                planner.endCanonicalSync()
            }
        }

        do {
            let commit = try await pullCanonicalItemsDurably(
                client: client,
                operationID: operationID,
                generation: generation
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            if commit.schedulingInputsChanged {
                foregroundPublicationRepairRequired = true
                lastPreview = nil
                clearTransientLocalComposition()
            }
            guard commit.schedulingInputsChanged || foregroundPublicationRepairRequired else {
                lastSuccessfulForegroundItemOperationID = operationID
                return
            }

            status = .syncing("Canonical items changed; composing a fresh schedule…")
            let installed = try await composeAndPublishFreshSchedule(
                client: client,
                operationID: operationID,
                generation: generation,
                created: 0,
                privacyUpdated: 0,
                updated: 0,
                retryBudget: 1
            )
            lastPreview = installed.preview
            foregroundPublicationRepairRequired = false
            lastSuccessfulSyncID = operationID
            lastFreshCompositionSyncID = operationID
            lastSuccessfulForegroundItemOperationID = operationID
            status = .online(
                updatedAt: now(),
                message: "Synced \(planner.canonicalItems.count) items; composed \(installed.blockSummary)"
            )
        } catch {
            planner.flushPersistence()
            guard activeSyncID == operationID,
                  generation == configurationGeneration,
                  !Task.isCancelled else { return }
            if foregroundPublicationRepairRequired || planner.persistenceError != nil {
                status = .failed((planner.persistenceError ?? error).localizedDescription)
            }
        }
    }

    private func pullCanonicalItemsDurably(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> CanonicalDeltaCommitResult {
        do {
            let result = try await loadDelta(
                client: client,
                from: planner.canonicalDeltaCursor,
                enforceItemCursorContract: true
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard DayWeaveItemCursorContract.isValidTransportToken(result.cursor) else {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            return try planner.applyCanonicalDeltaDurably(
                result.changes,
                nextCursor: result.cursor
            )
        } catch let error as DayWeaveAPIError {
            guard case let .server(statusCode, _, _, _) = error,
                  statusCode == 422,
                  planner.canonicalDeltaCursor != nil else {
                throw error
            }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            let result = try await loadDelta(
                client: client,
                from: nil,
                enforceItemCursorContract: true
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard DayWeaveItemCursorContract.isValidTransportToken(result.cursor) else {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            let commit = try planner.replaceCanonicalStateDurably(
                changes: result.changes,
                nextCursor: result.cursor
            )
            warnings.append("The server item stream changed; the encrypted cache was rebuilt safely.")
            return commit
        }
    }

    private func performReadFirstActivationBootstrap(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async {
        defer {
            if activeSyncID == operationID {
                activeSyncTask = nil
                activeSyncID = nil
                activeSyncScheduleProfile = nil
                isSyncing = false
                planner.endCanonicalSync()
                if generation != configurationGeneration {
                    reloadConfigurationStatus()
                }
            }
        }

        do {
            status = .syncing("Catching up canonical item revisions…")
            _ = try await pullCanonicalItemsDurably(
                client: client,
                operationID: operationID,
                generation: generation
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)

            status = .syncing("Checking the current published schedule…")
            var current = try await client.currentPublishedSchedule()
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard planner.canonicalConfigurationIdentifier == client.configurationIdentifier,
                  canonicalClientIsCurrent(client),
                  !hasPendingScheduleReplicaWrites else {
                throw CanonicalSyncError.operationSuperseded
            }

            if let initialCurrent = current {
                do {
                    try validateReplicatedSchedule(initialCurrent)
                } catch CanonicalSyncError.sourceRevisionMismatch {
                    // A publication may advance after the initial item boundary.
                    // Drain once more and prove the immutable head against that
                    // exact durable item generation before installing it.
                    _ = try await pullCanonicalItemsDurably(
                        client: client,
                        operationID: operationID,
                        generation: generation
                    )
                    try ensureOperationCurrent(
                        operationID: operationID,
                        generation: generation
                    )
                    current = try await client.currentPublishedSchedule()
                    try ensureOperationCurrent(
                        operationID: operationID,
                        generation: generation
                    )
                    guard planner.canonicalConfigurationIdentifier
                            == client.configurationIdentifier,
                          canonicalClientIsCurrent(client),
                          !hasPendingScheduleReplicaWrites else {
                        throw CanonicalSyncError.operationSuperseded
                    }
                    if let current {
                        try validateReplicatedSchedule(current)
                    }
                }
            }

            if let current {
                let rendered = render(current.schedule)
                let message = "Recovered published schedule revision \(current.revision.revisionNumber) · \(Self.composedBlockSummary(current.schedule.plan.blocks))"
                try planner.installCurrentPublishedSchedule(
                    current,
                    blocks: rendered,
                    configurationIdentifier: client.configurationIdentifier,
                    message: message
                )
                clearTransientLocalComposition()
                lastPreview = current.schedule
                foregroundScheduleLatestHintRevision = current.revision.revisionNumber
                foregroundScheduleReconciledRevision = current.revision.revisionNumber
                foregroundPublicationRepairRequired = false
                lastSuccessfulSyncID = operationID
                status = .online(updatedAt: now(), message: message)
                return
            }

            // Only the transport's exact authenticated, non-cacheable typed 404
            // reaches this branch. It may clear obsolete authority only when
            // no nonzero durable SSE high-water exists; lowering an old epoch
            // requires the separately scoped cursor-ahead fence. It does not
            // authorize an activation-time write: without an expected-head
            // publish precondition, another device could publish between this
            // read and our POST. Onboarding and explicit sync remain the paths
            // that deliberately compose and publish.
            try planner.clearCurrentPublishedSchedule(
                configurationIdentifier: client.configurationIdentifier
            )
            lastPreview = nil
            foregroundScheduleLatestHintRevision = 0
            foregroundScheduleReconciledRevision = 0
            foregroundPublicationRepairRequired = false
            lastSuccessfulSyncID = operationID
            status = .online(
                updatedAt: now(),
                message: "No schedule is currently published on this workspace"
            )
        } catch {
            planner.flushPersistence()
            guard activeSyncID == operationID,
                  generation == configurationGeneration,
                  !Task.isCancelled else { return }
            status = .failed((planner.persistenceError ?? error).localizedDescription)
        }
    }

    private func performSync(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async {
        defer {
            if activeSyncID == operationID {
                activeSyncTask = nil
                activeSyncID = nil
                activeSyncScheduleProfile = nil
                isSyncing = false
                planner.endCanonicalSync()
                if generation != configurationGeneration {
                    reloadConfigurationStatus()
                    if planner.pendingSchedulePublication != nil {
                        status = .failed(
                            "A schedule publication is awaiting exact recovery. Restore its original API configuration and authentication, then sync before replacing or resetting this connection."
                        )
                    }
                }
            }
        }

        do {
            var freshPublicationRetryBudget = 1
            if let pending = planner.pendingSchedulePublication {
                status = .syncing("Recovering an exact schedule publication…")
                let recovery = try await recoverPendingSchedulePublication(
                    pending,
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                switch recovery {
                case let .installed(revisionNumber, blockSummary):
                    lastPreview = pending.preview
                    lastSuccessfulSyncID = operationID
                    status = .online(
                        updatedAt: now(),
                        message: "Recovered published revision \(revisionNumber); installed \(blockSummary)"
                    )
                    return
                case .requiresFreshComposition:
                    // The recovered receipt may name a superseded revision, or
                    // the server proved that the old composition was never
                    // published. Permit exactly one newly composed attempt in
                    // this sync; it must not recursively retry again.
                    freshPublicationRetryBudget = 0
                }
            }
            planner.capturePendingCanonicalMutations()
            planner.flushPersistence()
            if let persistenceError = planner.persistenceError { throw persistenceError }
            try await pullCanonicalItems(
                client: client,
                operationID: operationID,
                generation: generation
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            status = .syncing("Publishing encrypted Inbox edits…")
            let authored = try await publishCanonicalAuthoringMutations(
                client: client,
                operationID: operationID,
                generation: generation
            )
            if authored > 0 {
                status = .syncing("Refreshing authored item revisions…")
                try await pullCanonicalItems(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
            }
            planner.capturePendingCanonicalMutations()
            planner.flushPersistence()
            if let persistenceError = planner.persistenceError { throw persistenceError }
            warnings.append(contentsOf: planner.canonicalItems
                .filter { !$0.supportsLosslessReplacement }
                .map {
                    "“\($0.title)” cannot be full-replaced without normalizing a server timestamp, number, or unsupported field, so it remains read-only."
                })
            status = .syncing("Publishing local captures…")
            let created = try await publishLocalCaptures(
                client: client,
                operationID: operationID,
                generation: generation
            )
            status = .syncing("Publishing privacy changes…")
            let privacyUpdated = try await publishSensitivityChanges(
                client: client,
                operationID: operationID,
                generation: generation
            )
            status = .syncing("Reconciling status changes…")
            let updated = try await publishSafeStatusChanges(
                client: client,
                operationID: operationID,
                generation: generation
            )
            let installed = try await composeAndPublishFreshSchedule(
                client: client,
                operationID: operationID,
                generation: generation,
                created: created,
                privacyUpdated: privacyUpdated,
                updated: updated,
                retryBudget: freshPublicationRetryBudget
            )
            lastPreview = installed.preview
            lastSuccessfulSyncID = operationID
            lastFreshCompositionSyncID = operationID
            status = .online(
                updatedAt: now(),
                message: "Synced \(planner.canonicalItems.count) items"
                    + (authored > 0 ? "; applied \(authored) Inbox edit\(authored == 1 ? "" : "s")" : "")
                    + "; composed \(installed.blockSummary)"
            )
        } catch {
            planner.flushPersistence()
            guard activeSyncID == operationID,
                  generation == configurationGeneration,
                  !Task.isCancelled else { return }
            status = .failed((planner.persistenceError ?? error).localizedDescription)
        }
    }

    private func pullCanonicalItems(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws {
        do {
            let result = try await loadDelta(client: client, from: planner.canonicalDeltaCursor)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            planner.applyCanonicalDelta(result.changes, nextCursor: result.cursor)
        } catch let error as DayWeaveAPIError {
            guard case let .server(statusCode, _, _, _) = error, statusCode == 422,
                  planner.canonicalDeltaCursor != nil else {
                throw error
            }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            // A server restart can rotate the cursor scope. Build a complete replacement
            // in memory first so an interrupted recovery never erases the offline cache.
            let result = try await loadDelta(client: client, from: nil)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            planner.replaceCanonicalState(changes: result.changes, nextCursor: result.cursor)
            warnings.append("The server item stream changed; the encrypted cache was rebuilt safely.")
        }
    }

    private func loadDelta(
        client: DayWeaveAPIClient,
        from initialCursor: String?,
        enforceItemCursorContract: Bool = false
    ) async throws -> (changes: [DayWeaveItemDeltaChange], cursor: String) {
        var cursor = initialCursor
        var changes: [DayWeaveItemDeltaChange] = []
        var retainedBytes = 0
        var seen = Set<String>()
        for _ in 0..<100 {
            if let cursor,
               cursor.utf8.count > Self.maximumDeltaCursorBytes {
                throw CanonicalSyncError.deltaResourceLimit
            }
            let page = try await client.itemDelta(cursor: cursor, limit: 200)
            if enforceItemCursorContract,
               !DayWeaveItemCursorContract.isValidTransportToken(page.nextCursor) {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            guard page.changes.count <= Self.maximumDeltaChanges - changes.count else {
                throw CanonicalSyncError.deltaResourceLimit
            }
            for change in page.changes {
                let (nextRetainedBytes, overflow) = retainedBytes.addingReportingOverflow(
                    change.retainedByteEstimate
                )
                guard !overflow, nextRetainedBytes <= Self.maximumRetainedDeltaBytes else {
                    throw CanonicalSyncError.deltaResourceLimit
                }
                retainedBytes = nextRetainedBytes
            }
            let cursorBytes = page.nextCursor.utf8.count
            let (withCursorBytes, cursorOverflow) = retainedBytes.addingReportingOverflow(cursorBytes)
            guard cursorBytes <= Self.maximumDeltaCursorBytes,
                  !cursorOverflow,
                  withCursorBytes <= Self.maximumRetainedDeltaBytes else {
                throw CanonicalSyncError.deltaResourceLimit
            }
            retainedBytes = withCursorBytes
            changes.append(contentsOf: page.changes)
            guard !page.nextCursor.isEmpty else {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            if !page.hasMore { return (changes, page.nextCursor) }
            guard page.nextCursor != cursor,
                  seen.insert(page.nextCursor).inserted else {
                throw CanonicalSyncError.invalidDeltaSequence
            }
            cursor = page.nextCursor
        }
        throw CanonicalSyncError.tooManyDeltaPages
    }

    private func loadConsistentPreview(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> (preview: DayWeaveSchedulePreview, request: DayWeaveSchedulePreviewRequest) {
        for attempt in 1...3 {
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            let request = try makePreviewRequest()
            let preview = try await client.previewSchedule(request)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            let localRevisions = Dictionary(
                uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
            )
            if preview.sourceItemRevisions == localRevisions {
                try validate(preview: preview, against: request)
                return (preview, request)
            }
            guard attempt < 3 else { break }
            warnings.append(
                "The preview used different item revisions; refreshed the delta stream before retry \(attempt + 1)."
            )
            try await pullCanonicalItems(
                client: client,
                operationID: operationID,
                generation: generation
            )
        }
        throw CanonicalSyncError.sourceRevisionMismatch
    }

    private func recoverPendingSchedulePublication(
        _ publication: PendingSchedulePublication,
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> PendingSchedulePublicationRecovery {
        try ensureOperationCurrent(operationID: operationID, generation: generation)
        try validatePublicationJournal(publication, client: client)
        let rendered = render(publication.preview)
        let published: DayWeaveSchedulePublishResponse
        do {
            published = try await client.publishSchedule(publication.preparedRequest)
        } catch {
            guard Self.isStaleSchedulePublication(error) else { throw error }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            try planner.clearPendingSchedulePublicationWithoutApplying(publication)
            warnings.append(
                "The server proved that the retained schedule was stale before publication; it was cleared safely and will be composed once from current items."
            )
            return .requiresFreshComposition
        }
        try ensureOperationCurrent(operationID: operationID, generation: generation)
        try validatePublicationResponse(published, for: publication)
        if published.replayed {
            // An exact receipt can identify a revision that another device has
            // already superseded. Clear the now-acknowledged write boundary but
            // leave the prior local plan invalid until one fresh composition.
            try planner.clearPendingSchedulePublicationWithoutApplying(publication)
            warnings.append(
                "Recovered the exact publication receipt; recomposing once before making any schedule actionable because that receipt may be superseded."
            )
            return .requiresFreshComposition
        }
        try planner.commitPendingSchedulePublication(
            publication,
            blocks: rendered,
            response: published
        )
        clearTransientLocalComposition()
        return .installed(
            revisionNumber: published.revision.revisionNumber,
            blockSummary: Self.composedBlockSummary(publication.preview.plan.blocks)
        )
    }

    private func composeAndPublishFreshSchedule(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64,
        created: Int,
        privacyUpdated: Int,
        updated: Int,
        retryBudget: Int
    ) async throws -> InstalledSchedulePublication {
        var retriesRemaining = max(0, retryBudget)
        while true {
            status = .syncing("Composing a read-only schedule preview…")
            let consistentPreview = try await loadConsistentPreview(
                client: client,
                operationID: operationID,
                generation: generation
            )
            let preview = consistentPreview.preview
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            warnings.append(contentsOf: preview.rejectedItems.map {
                "“\($0.title)” was excluded from the preview: \($0.reason)"
            })
            let rendered = render(preview)
            let preparedAt = now()
            let provenance = SchedulePreviewProvenance(
                configurationIdentifier: client.configurationIdentifier,
                generatedAt: preparedAt,
                asOf: preview.plan.asOf,
                horizonStart: preview.plan.horizonStart,
                horizonEnd: preview.plan.horizonEnd,
                timezoneName: consistentPreview.request.timezoneName
            )
            let publicationRequest = DayWeaveSchedulePublishRequest(
                idempotencyKey: UUID(),
                expectedInputDigest: preview.inputDigest,
                schedule: consistentPreview.request
            )
            let publication = PendingSchedulePublication(
                configurationIdentifier: client.configurationIdentifier,
                preparedRequest: try client.prepareSchedulePublication(publicationRequest),
                preview: preview,
                message: previewMessage(
                    preview,
                    created: created,
                    privacyUpdated: privacyUpdated,
                    updated: updated
                ),
                provenance: provenance,
                preparedAt: preparedAt
            )
            try validatePublicationJournal(publication, client: client)
            try planner.persistPendingSchedulePublication(publication)
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            status = .syncing("Publishing the validated schedule…")

            let published: DayWeaveSchedulePublishResponse
            do {
                published = try await client.publishSchedule(publication.preparedRequest)
            } catch {
                guard Self.isStaleSchedulePublication(error) else { throw error }
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                try planner.clearPendingSchedulePublicationWithoutApplying(publication)
                guard retriesRemaining > 0 else {
                    throw CanonicalSyncError.schedulePublicationStayedStale
                }
                retriesRemaining -= 1
                warnings.append(
                    "Canonical items changed during publication; discarded that non-published candidate and recomposed once."
                )
                status = .syncing("Refreshing canonical items after a publication race…")
                try await pullCanonicalItems(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                continue
            }

            try ensureOperationCurrent(operationID: operationID, generation: generation)
            try validatePublicationResponse(published, for: publication)
            if published.replayed {
                try planner.clearPendingSchedulePublicationWithoutApplying(publication)
                guard retriesRemaining > 0 else {
                    throw CanonicalSyncError.schedulePublicationReplayNeedsFreshComposition
                }
                retriesRemaining -= 1
                warnings.append(
                    "The publication returned an existing exact receipt; recomposed once before making a schedule actionable."
                )
                status = .syncing("Refreshing canonical items after an exact receipt…")
                try await pullCanonicalItems(
                    client: client,
                    operationID: operationID,
                    generation: generation
                )
                continue
            }

            try planner.commitPendingSchedulePublication(
                publication,
                blocks: rendered,
                response: published
            )
            clearTransientLocalComposition()
            return .init(
                preview: preview,
                blockSummary: Self.composedBlockSummary(preview.plan.blocks)
            )
        }
    }

    private static func isStaleSchedulePublication(_ error: any Error) -> Bool {
        (error as? DayWeaveAPIError) == .trustedSchedulePublicationStale
    }

    private func validatePublicationJournal(
        _ publication: PendingSchedulePublication,
        client: DayWeaveAPIClient
    ) throws {
        let request = publication.preparedRequest.request
        guard publication.version == PendingSchedulePublication.currentVersion,
              publication.isWithinEncodedSizeLimit,
              publication.configurationIdentifier == client.configurationIdentifier,
              publication.configurationIdentifier == planner.canonicalConfigurationIdentifier,
              publication.provenance.configurationIdentifier == publication.configurationIdentifier,
              Self.isValidInputDigest(request.expectedInputDigest),
              request.expectedInputDigest == publication.preview.inputDigest,
              sameInstant(publication.provenance.asOf, request.schedule.asOf),
              sameInstant(publication.provenance.horizonStart, request.schedule.horizonStart),
              sameInstant(publication.provenance.horizonEnd, request.schedule.horizonEnd),
              publication.provenance.timezoneName == request.schedule.timezoneName,
              publication.preparedAt.timeIntervalSinceReferenceDate.isFinite,
              publication.provenance.generatedAt.timeIntervalSinceReferenceDate.isFinite,
              publication.provenance.generatedAt >= publication.preparedAt.addingTimeInterval(-0.002),
              publication.provenance.generatedAt <= publication.preparedAt.addingTimeInterval(0.002),
              publication.message.utf8.count <= PendingSchedulePublication.maximumMessageBytes,
              !publication.message.unicodeScalars.contains(
                  where: CharacterSet.controlCharacters.contains
              ) else {
            throw CanonicalSyncError.invalidSchedulePublication
        }
        try client.validatePreparedSchedulePublication(publication.preparedRequest)
        let localRevisions = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
        )
        guard publication.preview.sourceItemRevisions == localRevisions else {
            throw CanonicalSyncError.sourceRevisionMismatch
        }
        try validate(preview: publication.preview, against: request.schedule)
    }

    private func validatePublicationResponse(
        _ response: DayWeaveSchedulePublishResponse,
        for publication: PendingSchedulePublication
    ) throws {
        let revision = response.revision
        let request = publication.preparedRequest.request
        let components = revision.revision.split(
            separator: ":",
            maxSplits: 1,
            omittingEmptySubsequences: false
        )
        let currentTime = now()
        guard components.count == 2,
              revision.revisionNumber > 0,
              components[0] == Substring(String(revision.revisionNumber)),
              components[1] == Substring(revision.id.uuidString.lowercased()),
              revision.inputDigest == request.expectedInputDigest,
              sameInstant(revision.horizonStart, request.schedule.horizonStart),
              sameInstant(revision.horizonEnd, request.schedule.horizonEnd),
              revision.timezoneName == request.schedule.timezoneName,
              revision.publishedAt.timeIntervalSinceReferenceDate.isFinite,
              revision.publishedAt <= currentTime.addingTimeInterval(5 * 60) else {
            throw CanonicalSyncError.invalidSchedulePublication
        }
    }

    private func sameInstant(_ left: Date, _ right: Date) -> Bool {
        abs(left.timeIntervalSince(right)) <= 0.002
    }

    private static func isValidInputDigest(_ value: String) -> Bool {
        let prefix = "sha256:"
        guard value.hasPrefix(prefix), value.utf8.count == prefix.utf8.count + 64 else {
            return false
        }
        return value.utf8.dropFirst(prefix.utf8.count).allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        }
    }

    private func validate(
        preview: DayWeaveSchedulePreview,
        against request: DayWeaveSchedulePreviewRequest
    ) throws {
        guard !preview.inputDigest.isEmpty else {
            throw CanonicalSyncError.invalidPreview(
                "The response has no server input digest."
            )
        }
        try validateOccurrenceEvidenceOwnership(preview)
        try validate(
            plan: preview.plan,
            sourceItemRevisions: preview.sourceItemRevisions,
            rejectedItems: preview.rejectedItems,
            against: request
        )
    }

    /// The public transport can prove only that occurrence and item references
    /// exist. Recurring hierarchies deliberately attach a leaf decision/block
    /// to an occurrence owned by its recurring ancestor, so exact ownership is
    /// checked here against the durable canonical parent graph.
    private func validateOccurrenceEvidenceOwnership(
        _ preview: DayWeaveSchedulePreview
    ) throws {
        let itemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        let seriesByOccurrence = Dictionary(
            uniqueKeysWithValues: preview.plan.occurrences.map {
                ($0.id, $0.seriesItemID)
            }
        )
        func belongs(_ itemID: UUID, to occurrenceID: UUID) -> Bool {
            guard let seriesItemID = seriesByOccurrence[occurrenceID] else { return false }
            return Self.item(itemID, belongsToSeries: seriesItemID, itemByID: itemByID)
        }
        func uuid(_ value: JSONValue?) -> UUID? {
            guard case let .string(raw)? = value,
                  let id = UUID(uuidString: raw),
                  id.uuidString.lowercased() == raw else { return nil }
            return id
        }
        func optionalUUID(_ value: JSONValue?) -> (UUID?, Bool) {
            if value == .null { return (nil, true) }
            guard let id = uuid(value) else { return (nil, false) }
            return (id, true)
        }
        func uuidArray(_ value: JSONValue?) -> [UUID]? {
            guard case let .array(values)? = value else { return nil }
            let result = values.compactMap { uuid($0) }
            return result.count == values.count ? result : nil
        }
        func validateEvidence(
            itemIDs: [UUID],
            occurrenceIDs: [UUID]
        ) -> Bool {
            occurrenceIDs.allSatisfy { occurrenceID in
                itemIDs.contains { belongs($0, to: occurrenceID) }
            }
        }

        for work in preview.plan.unscheduled {
            if let occurrenceID = work.occurrenceID,
               !belongs(work.itemID, to: occurrenceID) {
                throw CanonicalSyncError.invalidPreview(
                    "Unscheduled recurrence evidence does not belong to its source series."
                )
            }
        }
        for rawDecision in preview.plan.decisions {
            guard case let .object(decision) = rawDecision,
                  let itemID = uuid(decision["item_id"]) else {
                throw CanonicalSyncError.invalidPreview("A plan decision has invalid identity evidence.")
            }
            let occurrence = optionalUUID(decision["occurrence_id"])
            guard occurrence.1,
                  occurrence.0.map({ belongs(itemID, to: $0) }) ?? true else {
                throw CanonicalSyncError.invalidPreview(
                    "A plan decision does not belong to its source recurrence series."
                )
            }
        }
        for rawViolation in preview.plan.violations {
            guard case let .object(violation) = rawViolation,
                  let itemIDs = uuidArray(violation["item_ids"]),
                  let occurrenceIDs = uuidArray(violation["occurrence_ids"]),
                  validateEvidence(itemIDs: itemIDs, occurrenceIDs: occurrenceIDs) else {
                throw CanonicalSyncError.invalidPreview(
                    "A plan violation does not belong to its source recurrence series."
                )
            }
        }
        for rawAssessment in preview.manualPlacementAssessments {
            guard case let .object(assessment) = rawAssessment,
                  case let .array(violations)? = assessment["violations"] else {
                throw CanonicalSyncError.invalidPreview(
                    "A manual placement assessment has invalid recurrence evidence."
                )
            }
            for rawViolation in violations {
                guard case let .object(violation) = rawViolation,
                      let itemIDs = uuidArray(violation["item_ids"]),
                      let occurrenceIDs = uuidArray(violation["occurrence_ids"]),
                      validateEvidence(itemIDs: itemIDs, occurrenceIDs: occurrenceIDs),
                      case let .array(conflicts)? = violation["conflicting_blocks"] else {
                    throw CanonicalSyncError.invalidPreview(
                        "Manual placement evidence does not belong to its source recurrence series."
                    )
                }
                for rawConflict in conflicts {
                    guard case let .object(conflict) = rawConflict else {
                        throw CanonicalSyncError.invalidPreview(
                            "A conflicting block has invalid recurrence evidence."
                        )
                    }
                    let occurrence = optionalUUID(conflict["occurrence_id"])
                    guard occurrence.1 else {
                        throw CanonicalSyncError.invalidPreview(
                            "A conflicting block has invalid recurrence evidence."
                        )
                    }
                    if let occurrenceID = occurrence.0 {
                        guard let itemID = uuid(conflict["item_id"]),
                              belongs(itemID, to: occurrenceID) else {
                            throw CanonicalSyncError.invalidPreview(
                                "A conflicting block does not belong to its source recurrence series."
                            )
                        }
                    }
                }
            }
        }
    }

    private func validate(
        plan: DayWeaveSchedulePreview.Plan,
        sourceItemRevisions: [UUID: UInt64],
        rejectedItems: [DayWeaveSchedulePreview.RejectedItem],
        against request: DayWeaveSchedulePreviewRequest
    ) throws {
        let itemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        func sameInstant(_ left: Date, _ right: Date) -> Bool {
            abs(left.timeIntervalSince(right)) <= 0.002
        }
        func effectiveSensitivity(for itemID: UUID) -> Bool {
            var currentID: UUID? = itemID
            var visited = Set<UUID>()
            while let identifier = currentID {
                guard visited.insert(identifier).inserted,
                      let item = itemByID[identifier] else { return true }
                if item.isSensitive { return true }
                currentID = item.parentID
            }
            return false
        }
        let fixedByID = try Self.validatedFixedBlocksByID(request.fixedBlocks)
        guard sameInstant(plan.asOf, request.asOf),
              sameInstant(plan.horizonStart, request.horizonStart),
              sameInstant(plan.horizonEnd, request.horizonEnd),
              plan.horizonStart < plan.horizonEnd else {
            throw CanonicalSyncError.invalidPreview(
                "The response clock or horizon does not match the preview request."
            )
        }
        for rejected in rejectedItems {
            guard let item = itemByID[rejected.itemID],
                  item.title == rejected.title,
                  rejected.isSensitive == effectiveSensitivity(for: rejected.itemID) else {
                throw CanonicalSyncError.invalidPreview(
                    "A rejected item has inconsistent canonical sensitivity or identity."
                )
            }
        }

        var blockIDs = Set<UUID>()
        var sessionIdentities = Set<PreviewSessionIdentity>()
        var externalBlockIDs = Set<UUID>()
        var scheduledMinutes: UInt32 = 0
        var occurrenceByID: [UUID: DayWeaveSchedulePreview.Plan.Occurrence] = [:]
        for occurrence in plan.occurrences {
            let source = RecurrenceMoveSource(
                itemRevision: sourceItemRevisions[occurrence.seriesItemID] ?? 0,
                identity: occurrence.identity,
                nominalStart: occurrence.nominalStart,
                nominalEnd: occurrence.nominalEnd,
                localDate: occurrence.localDate,
                ordinal: occurrence.ordinal
            )
            let seriesRecurrence = itemByID[occurrence.seriesItemID]?.recurrence
            guard occurrenceByID.updateValue(occurrence, forKey: occurrence.id) == nil,
                  dayWeaveIsRFC4122VersionFiveUUID(occurrence.id),
                  occurrence.identity.isCompatible(with: seriesRecurrence),
                  source.hasValidShape,
                  Self.validOccurrenceWindow(occurrence),
                  ["generated", "completed", "paused", "skipped"].contains(occurrence.state) else {
                throw CanonicalSyncError.invalidPreview(
                    "The response contains invalid or duplicate recurrence occurrence metadata."
                )
            }
        }
        let orderedBlocks = plan.blocks.sorted {
            if $0.start != $1.start { return $0.start < $1.start }
            if $0.end != $1.end { return $0.end < $1.end }
            return $0.id.uuidString < $1.id.uuidString
        }
        var latestAnyEnd: Date?
        var latestPlannedEnd: Date?
        for block in orderedBlocks {
            guard blockIDs.insert(block.id).inserted else {
                throw CanonicalSyncError.invalidPreview("The response contains a duplicate block identifier.")
            }
            guard block.start < block.end,
                  block.end > plan.horizonStart,
                  block.start < plan.horizonEnd else {
                throw CanonicalSyncError.invalidPreview(
                    "A response block has an empty interval or does not intersect the response horizon."
                )
            }
            if let itemID = block.itemID {
                guard itemByID[itemID] != nil,
                      itemByID[itemID]?.title == block.title,
                      block.isSensitive == effectiveSensitivity(for: itemID) else {
                    throw CanonicalSyncError.invalidPreview(
                        "A canonical block has inconsistent identity, title, or effective sensitivity."
                    )
                }
                if let occurrenceID = block.occurrenceID {
                    guard let seriesItemID = occurrenceByID[occurrenceID]?.seriesItemID,
                          Self.item(
                            itemID,
                            belongsToSeries: seriesItemID,
                            itemByID: itemByID
                          ) else {
                        throw CanonicalSyncError.invalidPreview(
                            "A recurring block has no exact source occurrence envelope."
                        )
                    }
                }
            }
            switch block.kind {
            case "planned", "pinned":
                let overlappingEarlierEnd = block.kind == "planned"
                    ? latestAnyEnd
                    : latestPlannedEnd
                if let overlappingEarlierEnd, overlappingEarlierEnd > block.start {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned response block overlaps another block."
                    )
                }
                guard block.start >= plan.horizonStart,
                      block.end <= plan.horizonEnd else {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned response block lies outside the response horizon."
                    )
                }
                guard let itemID = block.itemID,
                      sourceItemRevisions[itemID] != nil,
                      block.externalBlockID == nil,
                      itemByID[itemID]?.isExecutable == true else {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned block does not reference an executable canonical leaf item."
                    )
                }
                let identity = PreviewSessionIdentity(
                    itemID: itemID,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex
                )
                guard sessionIdentities.insert(identity).inserted else {
                    throw CanonicalSyncError.invalidPreview(
                        "The response contains a duplicate item/occurrence/session identity."
                    )
                }
                let seconds = block.end.timeIntervalSince(block.start)
                let rawMinutes = seconds / 60
                guard seconds.isFinite,
                      rawMinutes.isFinite,
                      abs(rawMinutes.rounded() - rawMinutes) < 0.000_001,
                      rawMinutes >= 0,
                      rawMinutes <= Double(UInt32.max),
                      let minutes = UInt32(exactly: rawMinutes.rounded()) else {
                    throw CanonicalSyncError.invalidPreview(
                        "A planned response block has a non-finite or non-minute duration."
                    )
                }
                scheduledMinutes = scheduledMinutes.addingReportingOverflow(minutes).overflow
                    ? UInt32.max
                    : scheduledMinutes &+ minutes
            case "calendar_event":
                if let latestPlannedEnd, latestPlannedEnd > block.start {
                    throw CanonicalSyncError.invalidPreview(
                        "A calendar block overlaps planned work."
                    )
                }
                guard let itemID = block.itemID,
                      sourceItemRevisions[itemID] != nil,
                      block.externalBlockID == nil,
                      itemByID[itemID]?.kind == .event else {
                    throw CanonicalSyncError.invalidPreview(
                        "A calendar block does not reference a canonical event item."
                    )
                }
                let identity = PreviewSessionIdentity(
                    itemID: itemID,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex
                )
                guard sessionIdentities.insert(identity).inserted else {
                    throw CanonicalSyncError.invalidPreview(
                        "The response contains a duplicate item/occurrence/session identity."
                    )
                }
            case "external_fixed":
                if let latestPlannedEnd, latestPlannedEnd > block.start {
                    throw CanonicalSyncError.invalidPreview(
                        "An external fixed block overlaps planned work."
                    )
                }
                guard block.itemID == nil,
                      block.occurrenceID == nil,
                      let externalBlockID = block.externalBlockID,
                      block.id == externalBlockID,
                      let fixed = fixedByID[externalBlockID],
                      fixed.title == block.title,
                      fixed.isSensitive == block.isSensitive,
                      sameInstant(fixed.start, block.start),
                      sameInstant(fixed.end, block.end),
                      externalBlockIDs.insert(externalBlockID).inserted else {
                    throw CanonicalSyncError.invalidPreview(
                        "An external fixed block has an invalid or duplicate source identity."
                    )
                }
            default:
                throw CanonicalSyncError.invalidPreview(
                    "The response contains an unsupported schedule block kind."
                )
            }
            if latestAnyEnd == nil || block.end > latestAnyEnd! {
                latestAnyEnd = block.end
            }
            if block.kind == "planned",
               latestPlannedEnd == nil || block.end > latestPlannedEnd! {
                latestPlannedEnd = block.end
            }
        }
        try Self.validateFixedBlockCoverage(
            returnedExternalBlockIDs: externalBlockIDs,
            request: request
        )
        var unscheduledMinutes: UInt32 = 0
        var unscheduledIdentities = Set<PreviewOccurrenceIdentity>()
        for unscheduled in plan.unscheduled {
            guard sourceItemRevisions[unscheduled.itemID] != nil,
                  unscheduledIdentities.insert(.init(
                      itemID: unscheduled.itemID,
                      occurrenceID: unscheduled.occurrenceID
                  )).inserted else {
                throw CanonicalSyncError.invalidPreview(
                    "The response reports duplicate unscheduled work or an unknown source item."
                )
            }
            unscheduledMinutes = unscheduledMinutes.addingReportingOverflow(unscheduled.remaining).overflow
                ? UInt32.max
                : unscheduledMinutes &+ unscheduled.remaining
        }
        guard plan.score.scheduledMinutes == scheduledMinutes,
              plan.score.unscheduledMinutes == unscheduledMinutes else {
            throw CanonicalSyncError.invalidPreview(
                "The response score does not match its scheduled and unscheduled work."
            )
        }
    }

    private static func validOccurrenceWindow(
        _ occurrence: DayWeaveSchedulePreview.Plan.Occurrence
    ) -> Bool {
        guard occurrence.windowStart.utf8.count <= 64,
              occurrence.windowEnd.utf8.count <= 64,
              let start = RecurrenceMoveSource.parseRFC3339(occurrence.windowStart),
              let end = RecurrenceMoveSource.parseRFC3339(occurrence.windowEnd) else {
            return false
        }
        return start < end
    }

    static func validateFixedBlockCoverage(
        returnedExternalBlockIDs: Set<UUID>,
        request: DayWeaveSchedulePreviewRequest
    ) throws {
        let fixedByID = try validatedFixedBlocksByID(request.fixedBlocks)
        let expectedExternalBlockIDs = Set(fixedByID.values.compactMap { fixed in
            fixed.end > request.horizonStart && fixed.start < request.horizonEnd ? fixed.id : nil
        })
        guard returnedExternalBlockIDs == expectedExternalBlockIDs else {
            throw CanonicalSyncError.invalidPreview(
                "The response omitted or invented an intersecting external fixed block."
            )
        }
    }

    private static func validatedFixedBlocksByID(
        _ fixedBlocks: [DayWeaveSchedulePreviewRequest.FixedBlock]
    ) throws -> [UUID: DayWeaveSchedulePreviewRequest.FixedBlock] {
        var fixedByID: [UUID: DayWeaveSchedulePreviewRequest.FixedBlock] = [:]
        for fixed in fixedBlocks {
            guard fixedByID[fixed.id] == nil else {
                throw CanonicalSyncError.invalidPreview(
                    "The preview request contains a duplicate external fixed-block identifier."
                )
            }
            fixedByID[fixed.id] = fixed
        }
        return fixedByID
    }

    private func publishCanonicalAuthoringMutations(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        let ordered = orderedCanonicalAuthoringMutations(
            planner.sortedPendingCanonicalAuthoringMutations.filter {
                $0.disposition == .pending
            }
        )
        var appliedCount = 0
        var attemptedCount = 0

        for (offset, original) in ordered.enumerated() {
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard attemptedCount < authoringPushLimit else {
                let deferred = ordered.count - offset
                warnings.append(
                    "Deferred \(deferred) encrypted Inbox edit\(deferred == 1 ? "" : "s") after reaching the \(authoringPushLimit)-request safety cap."
                )
                break
            }
            guard var mutation = planner.canonicalAuthoringMutation(id: original.id),
                  mutation.disposition == .pending else { continue }

            if mutation.hasBeenSubmitted,
               let observed = try reconcileCanonicalAuthoringFromCache(mutation) {
                if observed { appliedCount += 1 }
                continue
            }
            guard try canonicalAuthoringPreflightIsCurrent(mutation) else { continue }
            if !mutation.hasBeenSubmitted {
                mutation = try planner.bindCanonicalAuthoringMutation(
                    mutation.id,
                    configurationIdentifier: client.configurationIdentifier
                )
                mutation = try planner.markCanonicalAuthoringMutationSubmitted(mutation.id)
            }
            guard mutation.configurationIdentifier == client.configurationIdentifier else {
                throw PlannerCanonicalAuthoringError.invalidConfiguration
            }

            attemptedCount += 1
            do {
                let response = try await sendCanonicalAuthoringMutation(
                    mutation,
                    client: client
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                try planner.applyCanonicalAuthoringResponse(mutation.id, item: response)
                appliedCount += 1
            } catch let error as DayWeaveAPIError {
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                if let reconciled = try await reconcileCanonicalAuthoringAfterServerError(
                    mutation,
                    error: error,
                    client: client,
                    operationID: operationID,
                    generation: generation
                ) {
                    if reconciled { appliedCount += 1 }
                    continue
                }
                throw error
            }
        }
        return appliedCount
    }

    /// Every mutation of a child may refresh both its old and new canonical
    /// parents. Publish any queued parent operation first so that the child's
    /// hierarchy side effect cannot make the parent's exact base revision stale
    /// inside the same offline batch. Submitted work otherwise retains priority
    /// among unrelated nodes so uncertain requests are reconciled first.
    private func orderedCanonicalAuthoringMutations(
        _ mutations: [DayWeavePendingCanonicalAuthoringMutation]
    ) -> [DayWeavePendingCanonicalAuthoringMutation] {
        let stable = mutations.sorted {
            if $0.hasBeenSubmitted != $1.hasBeenSubmitted {
                return $0.hasBeenSubmitted && !$1.hasBeenSubmitted
            }
            if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
            return $0.id.uuidString < $1.id.uuidString
        }
        let byItemID = Dictionary(
            stable.map { ($0.itemID, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        let stableRank = Dictionary(uniqueKeysWithValues: stable.enumerated().map {
            ($0.element.id, $0.offset)
        })
        var childrenByParentMutationID: [UUID: Set<UUID>] = [:]
        var dependencyCount = Dictionary(uniqueKeysWithValues: stable.map { ($0.id, 0) })
        for child in stable {
            for parentItemID in canonicalAuthoringAffectedParentIDs(child) {
                guard let parent = byItemID[parentItemID], parent.id != child.id else { continue }
                if childrenByParentMutationID[parent.id, default: []].insert(child.id).inserted {
                    dependencyCount[child.id, default: 0] += 1
                }
            }
        }

        func stableOrder(_ left: DayWeavePendingCanonicalAuthoringMutation,
                         _ right: DayWeavePendingCanonicalAuthoringMutation) -> Bool {
            (stableRank[left.id] ?? .max) < (stableRank[right.id] ?? .max)
        }

        var ready = stable.filter { dependencyCount[$0.id] == 0 }.sorted(by: stableOrder)
        var emitted = Set<UUID>()
        var result: [DayWeavePendingCanonicalAuthoringMutation] = []
        result.reserveCapacity(stable.count)

        while !ready.isEmpty {
            let mutation = ready.removeFirst()
            guard emitted.insert(mutation.id).inserted else { continue }
            result.append(mutation)
            for childID in childrenByParentMutationID[mutation.id] ?? [] {
                let remaining = max(0, (dependencyCount[childID] ?? 0) - 1)
                dependencyCount[childID] = remaining
                if remaining == 0, let child = stable.first(where: { $0.id == childID }) {
                    ready.append(child)
                    ready.sort(by: stableOrder)
                }
            }
        }
        // A cycle across old and new ancestry cannot be serialized with exact
        // per-item revisions. Keep deterministic order; normal preflight and
        // conflict recovery retain every request rather than dropping it.
        result.append(contentsOf: stable.filter { !emitted.contains($0.id) })
        return result
    }

    private func canonicalAuthoringAffectedParentIDs(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) -> Set<UUID> {
        var parentIDs = Set<UUID>()
        if let parentID = mutation.draft?.parentID { parentIDs.insert(parentID) }
        if let parentID = mutation.baseItem?.parentID { parentIDs.insert(parentID) }
        if mutation.operation == .restore,
           let parentID = planner.canonicalTrashEntry(id: mutation.itemID)?.parentID {
            parentIDs.insert(parentID)
        }
        parentIDs.remove(mutation.itemID)
        return parentIDs
    }

    private func canonicalAuthoringPreflightIsCurrent(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool {
        let diagnostic: String?
        switch mutation.operation {
        case .create:
            diagnostic = if planner.canonicalItem(id: mutation.itemID) != nil
                || planner.canonicalTombstoneRevisions[mutation.itemID] != nil {
                "This identifier already belongs to canonical history. Keep the latest item or create a new draft."
            } else if let draft = mutation.draft,
                      !planner.canonicalAuthoringDraftHierarchyIsCurrent(
                          draft,
                          itemID: mutation.itemID,
                          requiresCommittedParent: true
                      ) {
                "The selected parent is no longer available for this item. Choose an active Inbox or Planned parent before retrying."
            } else {
                nil
            }
        case .replace, .trash:
            if let expected = mutation.expectedRevision,
               planner.canonicalItem(id: mutation.itemID)?.revision == expected {
                if mutation.operation == .replace,
                   let draft = mutation.draft,
                   !planner.canonicalAuthoringDraftHierarchyIsCurrent(
                       draft,
                       itemID: mutation.itemID,
                       requiresCommittedParent: true
                   ) {
                    diagnostic = "The selected parent is no longer available for this item. Review the latest hierarchy before retrying."
                } else if mutation.operation == .trash,
                          planner.canonicalItems.contains(where: {
                              $0.parentID == mutation.itemID && $0.deletedAt == nil
                          }) {
                    diagnostic = "This item now has active children and cannot be deleted until they are moved or deleted."
                } else {
                    diagnostic = nil
                }
            } else if mutation.operation == .trash,
                      mutation.hasBeenSubmitted,
                      let expected = mutation.expectedRevision,
                      let entry = planner.canonicalTrashEntry(id: mutation.itemID),
                      entry.revision > expected {
                // A pulled tombstone proves the item left the active set, but
                // delta deliberately retains only the pre-delete body. Replay
                // the immutable request to recover the full deleted response;
                // do not misclassify response loss as a revision conflict.
                diagnostic = nil
            } else {
                diagnostic = "The item changed after this edit was saved. Review the latest revision before retrying."
            }
        case .restore:
            if let expected = mutation.expectedRevision,
               let entry = planner.canonicalTrashEntry(id: mutation.itemID),
               entry.revision == expected {
                if let parentID = entry.parentID,
                   planner.canonicalItem(id: parentID) == nil {
                    diagnostic = "The deleted item's parent is no longer active, so it cannot be restored in place."
                } else {
                    diagnostic = nil
                }
            } else {
                diagnostic = "The deleted item changed after restore was requested. Review the latest revision before retrying."
            }
        }
        guard let diagnostic else { return true }
        _ = try planner.markCanonicalAuthoringMutationConflicted(
            mutation.id,
            diagnostic: diagnostic
        )
        warnings.append("An Inbox edit needs conflict review before it can be published.")
        return false
    }

    private func sendCanonicalAuthoringMutation(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation,
        client: DayWeaveAPIClient
    ) async throws -> DayWeaveCanonicalItem {
        switch mutation.operation {
        case .create:
            guard let draft = mutation.draft else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.createCanonicalItem(
                DayWeaveNewCanonicalItem(
                    id: mutation.itemID,
                    fields: draft.requestFields(
                        durationWireShape: mutation.durationWireShape
                    )
                ),
                idempotencyKey: mutation.idempotencyKey
            )
        case .replace:
            guard let draft = mutation.draft,
                  let expectedRevision = mutation.expectedRevision else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.replaceCanonicalItem(
                mutation.itemID,
                expectedRevision: expectedRevision,
                item: draft.requestFields(
                    durationWireShape: mutation.durationWireShape
                ),
                idempotencyKey: mutation.idempotencyKey
            )
        case .trash:
            guard let expectedRevision = mutation.expectedRevision else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.trashCanonicalItem(
                mutation.itemID,
                expectedRevision: expectedRevision,
                idempotencyKey: mutation.idempotencyKey
            )
        case .restore:
            guard let expectedRevision = mutation.expectedRevision else {
                throw PlannerCanonicalAuthoringError.invalidMutation
            }
            return try await client.restoreCanonicalItem(
                mutation.itemID,
                expectedRevision: expectedRevision,
                idempotencyKey: mutation.idempotencyKey
            )
        }
    }

    /// Returns `true` when the exact journal was committed, `false` when it was
    /// moved to explicit conflict review, and `nil` when no conclusive local
    /// observation exists and the exact request still needs replay.
    private func reconcileCanonicalAuthoringFromCache(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool? {
        let candidate: DayWeaveCanonicalItem?
        switch mutation.operation {
        case .create:
            candidate = planner.canonicalItem(id: mutation.itemID)
        case .replace, .restore:
            guard let expected = mutation.expectedRevision,
                  let observed = planner.canonicalItem(id: mutation.itemID),
                  observed.revision > expected else { return nil }
            candidate = observed
        case .trash:
            guard let expected = mutation.expectedRevision,
                  let entry = planner.canonicalTrashEntry(id: mutation.itemID),
                  entry.revision > expected,
                  entry.lastKnownItem?.deletedAt != nil else { return nil }
            candidate = entry.lastKnownItem
        }
        guard let candidate else { return nil }
        return try reconcileCanonicalAuthoringCandidate(candidate, mutation: mutation)
    }

    private func reconcileCanonicalAuthoringCandidate(
        _ candidate: DayWeaveCanonicalItem,
        mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool {
        do {
            try planner.applyCanonicalAuthoringResponse(mutation.id, item: candidate)
            return true
        } catch PlannerCanonicalAuthoringError.invalidRemoteResponse {
            _ = try planner.markCanonicalAuthoringMutationConflicted(
                mutation.id,
                diagnostic: "The canonical item now has different content or revision state. Review both versions before deciding which to keep."
            )
            warnings.append("An Inbox edit resolved to different canonical content and needs review.")
            return false
        }
    }

    private func reconcileCanonicalAuthoringAfterServerError(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation,
        error: DayWeaveAPIError,
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Bool? {
        let statusCode: Int
        let trustedNoEffect: Bool
        switch error {
        case .trustedCanonicalMutationInProgress:
            return nil
        case .trustedCanonicalMutationNoEffect:
            statusCode = 409
            trustedNoEffect = true
        case let .server(code, _, _, _):
            statusCode = code
            trustedNoEffect = false
        default:
            return nil
        }
        if statusCode == 400 || statusCode == 422 {
            _ = try planner.markCanonicalAuthoringMutationConflicted(
                mutation.id,
                diagnostic: "The server rejected this saved item contract. Edit the retained draft before retrying."
            )
            warnings.append("An Inbox edit was rejected by the canonical contract and remains encrypted for review.")
            return false
        }
        guard statusCode == 404 || statusCode == 409 else { return nil }

        let observed: [DayWeaveCanonicalItem]
        do {
            observed = try await client.listCanonicalItems(
                includeDeleted: true,
                limit: DayWeaveAPIClient.maximumCanonicalItemListLimit
            )
        } catch {
            if trustedNoEffect {
                return try markCanonicalAuthoringNoEffectConflict(mutation)
            }
            // The original exact mutation remains submitted. Failure to obtain
            // independent canonical evidence must never turn ambiguity into a
            // destructive conflict decision.
            return nil
        }
        try ensureOperationCurrent(operationID: operationID, generation: generation)
        if let candidate = observed.first(where: { $0.id == mutation.itemID }) {
            if mutation.operation != .create,
               let expected = mutation.expectedRevision,
               candidate.revision <= expected {
                // A matching idempotent request can return 409 while it still
                // owns the mutation. During that window the list endpoint
                // legitimately exposes the unchanged base item (or deleted
                // restore base). It is not evidence of a conflicting result.
                return trustedNoEffect
                    ? try markCanonicalAuthoringNoEffectConflict(mutation)
                    : nil
            }
            return try reconcileCanonicalAuthoringCandidate(candidate, mutation: mutation)
        }
        if trustedNoEffect {
            return try markCanonicalAuthoringNoEffectConflict(mutation)
        }
        if statusCode == 404,
           observed.count < DayWeaveAPIClient.maximumCanonicalItemListLimit {
            _ = try planner.markCanonicalAuthoringMutationConflicted(
                mutation.id,
                diagnostic: "The authenticated server confirmed that this item is absent. Keep the saved draft or discard this operation."
            )
            warnings.append("An Inbox edit references an item that is no longer available.")
            return false
        }
        // A 409 with no observed item can mean the matching idempotent request
        // is still in progress. A full 200-item list can also be truncated.
        // Preserve the exact submitted journal and retry later.
        return nil
    }

    private func markCanonicalAuthoringNoEffectConflict(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) throws -> Bool {
        _ = try planner.markCanonicalAuthoringMutationConflicted(
            mutation.id,
            diagnostic: "The authenticated server proved this exact request made no change. Review the latest canonical hierarchy and revision, then keep or copy the retained draft."
        )
        warnings.append("An Inbox edit made no canonical change and needs conflict review.")
        return false
    }

    private func publishLocalCaptures(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        let captures = planner.blocks
            .filter { $0.isLocallyAuthored && $0.sourceItemID == nil }
            .sorted {
                if $0.start != $1.start { return $0.start < $1.start }
                return $0.id.uuidString < $1.id.uuidString
            }
        var publishable: [ScheduleBlock] = []
        for var block in captures {
            guard let normalizedTitle = PlannerStore.normalizedCanonicalTitle(block.title) else {
                let diagnostic = "Not published: the title must contain 1–\(PlannerStore.maximumCanonicalTitleScalars) Unicode characters. Edit or delete this local capture in the inspector."
                planner.quarantineLocalCapture(block.id, diagnostic: diagnostic)
                warnings.append("A legacy local capture was skipped safely because its title is outside the server contract.")
                continue
            }
            if normalizedTitle != block.title {
                planner.normalizeLocalCaptureForSync(block.id, title: normalizedTitle)
                block.title = normalizedTitle
            }
            guard canonicalStatus(block.status) != nil else {
                let diagnostic = "Not published: this legacy local status is not representable by the canonical API. Delete the capture or change it through a supported planner action."
                planner.quarantineLocalCapture(block.id, diagnostic: diagnostic)
                warnings.append("“\(block.title)” was skipped safely because its local status is unsupported.")
                continue
            }
            if let existing = planner.canonicalItem(id: block.id) {
                guard let intended = makeNewItem(from: block),
                      DayWeaveCanonicalItemFields(item: existing) == intended.fields else {
                    let diagnostic = "Not published: this identifier already belongs to a server item with different canonical fields or privacy. Edit or delete the local capture."
                    planner.quarantineLocalCapture(block.id, diagnostic: diagnostic)
                    warnings.append("“\(block.title)” was not published because its identifier already exists remotely.")
                    continue
                }
                planner.bindLocalBlock(block.id, to: existing)
                continue
            }
            planner.clearLocalCaptureDiagnostic(block.id)
            publishable.append(block)
        }
        // Persist normalization/quarantine before the first remote side effect.
        planner.flushPersistence()
        if let persistenceError = planner.persistenceError { throw persistenceError }

        var createdCount = 0
        var networkPushes = 0
        for (offset, block) in publishable.enumerated() {
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard let newItem = makeNewItem(from: block) else {
                planner.quarantineLocalCapture(
                    block.id,
                    diagnostic: "Not published because this legacy capture cannot be represented losslessly. Edit or delete it in the inspector."
                )
                warnings.append("“\(block.title)” could not be represented by the canonical API and was retained locally.")
                continue
            }
            guard networkPushes < createPushLimit else {
                warnings.append(
                    "Deferred \(publishable.count - offset) local capture push\(publishable.count - offset == 1 ? "" : "es") to a later sync after reaching the \(createPushLimit)-request safety cap."
                )
                break
            }
            networkPushes += 1
            let created = try await client.createCanonicalItem(
                newItem,
                idempotencyKey: "mac-create-\(block.id.uuidString.lowercased())"
            )
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard created.id == block.id,
                  created.revision > (planner.canonicalTombstoneRevisions[block.id] ?? 0),
                  created.isSensitive == newItem.fields.isSensitive,
                  created.status == newItem.fields.status,
                  created.deletedAt == nil else {
                throw CanonicalSyncError.invalidMutationResponse
            }
            planner.bindLocalBlock(block.id, to: created)
            createdCount += 1
        }
        return createdCount
    }

    private func publishSensitivityChanges(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        var mutations = planner.pendingCanonicalSensitivityMutations.sorted {
            if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
            return $0.id.uuidString < $1.id.uuidString
        }
        var updatedCount = 0
        var networkPushes = 0

        while !mutations.isEmpty {
            let mutation = mutations.removeFirst()
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard mutation.disposition == .pending else {
                warnings.append("A conflicted privacy edit remains encrypted for review.")
                continue
            }
            guard let item = planner.canonicalItem(id: mutation.itemID) else {
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: mutation.itemID,
                    diagnostic: "The source item is no longer present in the canonical cache."
                )
                warnings.append("A local privacy edit targets an item that was removed remotely.")
                continue
            }
            if item.isSensitive == mutation.desiredIsSensitive {
                if let next = planner.reconcileCanonicalSensitivityObservation(item) {
                    mutations.append(next)
                }
                continue
            }
            guard mutation.baseRevision == item.revision else {
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: item.id,
                    diagnostic: "Remote revision \(item.revision) differs from local privacy-edit base revision \(mutation.baseRevision)."
                )
                warnings.append("“\(item.title)” changed remotely; its privacy edit was retained as a conflict.")
                continue
            }
            guard item.supportsLosslessReplacement else {
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: item.id,
                    diagnostic: "A full replacement cannot losslessly preserve this item's fields."
                )
                warnings.append("“\(item.title)” has fields this app will not overwrite; its privacy edit remains recoverable.")
                continue
            }
            guard planner.executionState.activeSession?.itemID != item.id,
                  planner.executionState.pendingCommand == nil else {
                warnings.append("“\(item.title)” has active execution state; its privacy edit was deferred safely.")
                continue
            }
            guard networkPushes < statusPushLimit else {
                warnings.append(
                    "Deferred remaining privacy pushes to a later sync after reaching the \(statusPushLimit)-request safety cap."
                )
                break
            }
            let (expectedRevision, revisionOverflow) = item.revision.addingReportingOverflow(1)
            guard !revisionOverflow else {
                throw CanonicalSyncError.invalidMutationResponse
            }
            var replacement = DayWeaveCanonicalItemFields(item: item)
            replacement.isSensitive = mutation.desiredIsSensitive

            do {
                guard planner.markCanonicalSensitivityMutationSubmitted(mutation.id) else {
                    throw CanonicalSyncError.localPersistenceUnavailable
                }
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                networkPushes += 1
                let updated = try await client.replaceCanonicalItem(
                    item.id,
                    expectedRevision: item.revision,
                    item: replacement,
                    idempotencyKey: "mac-sensitive-\(item.id.uuidString.lowercased())-r\(item.revision)-\(mutation.desiredIsSensitive ? "private" : "standard")"
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard updated.id == item.id,
                      updated.revision == expectedRevision,
                      updated.deletedAt == nil,
                      updated.supportsLosslessReplacement,
                      DayWeaveCanonicalItemFields(item: updated) == replacement else {
                    throw CanonicalSyncError.invalidMutationResponse
                }
                if let next = planner.applyCanonicalSensitivityMutationResponse(
                    updated,
                    replacingBaseRevision: item.revision
                ) {
                    mutations.append(next)
                }
                updatedCount += 1
            } catch let error as DayWeaveAPIError {
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard case let .server(statusCode, _, _, _) = error, statusCode == 409 else {
                    throw error
                }
                planner.markCanonicalSensitivityMutationConflicted(
                    itemID: item.id,
                    diagnostic: "The server rejected privacy-edit base revision \(item.revision) as stale."
                )
                warnings.append("“\(item.title)” conflicted remotely; its privacy edit was retained.")
            }
        }
        return updatedCount
    }

    private func publishSafeStatusChanges(
        client: DayWeaveAPIClient,
        operationID: UUID,
        generation: UInt64
    ) async throws -> Int {
        let grouped = Dictionary(grouping: planner.pendingCanonicalMutations, by: \.itemID)
        var updatedCount = 0
        var networkPushes = 0

        for itemID in grouped.keys.sorted(by: { $0.uuidString < $1.uuidString }) {
            guard let mutations = grouped[itemID] else { continue }
            try ensureOperationCurrent(operationID: operationID, generation: generation)
            guard let item = planner.canonicalItem(id: itemID) else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "The source item is no longer present in the canonical cache."
                )
                warnings.append("A local status edit targets an item that was removed remotely.")
                continue
            }
            guard planner.canonicalSensitivityMutation(itemID: itemID) == nil else {
                // Privacy replacements own this item's revision until their
                // complete submitted/follow-up chain is reconciled. Advancing
                // the item here would strand the final privacy choice on a
                // stale base revision when the privacy push cap is reached.
                warnings.append(
                    "“\(item.title)” has a privacy edit in progress; its status edit was deferred safely."
                )
                continue
            }
            guard mutations.count == 1,
                  let mutation = mutations.first,
                  mutation.occurrenceID == nil,
                  mutation.disposition == .pending else {
                warnings.append("“\(item.title)” has split-session or conflicted status intent retained for review.")
                continue
            }
            guard planner.canPublishCanonicalMutation(mutation) else {
                warnings.append(
                    "“\(item.title)” has a superseded execution status journal retained without another write."
                )
                continue
            }
            guard mutation.baseRevision == item.revision else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "Remote revision \(item.revision) differs from local base revision \(mutation.baseRevision)."
                )
                warnings.append("“\(item.title)” changed remotely; its local status edit was retained as a conflict.")
                continue
            }
            let itemBlocks = planner.blocks.filter {
                $0.sourceItemID == itemID && $0.occurrenceID == nil
            }
            guard item.recurrence == nil,
                  case .indivisible = item.splitPolicy,
                  itemBlocks.count == 1,
                  itemBlocks[0].occurrenceFullyScheduled else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "A session-level edit cannot safely replace a recurring, split, partial, or multi-block item."
                )
                warnings.append(
                    "“\(item.title)” has a session-level status edit retained locally; the item was not full-replaced."
                )
                continue
            }
            guard item.supportsLosslessReplacement,
                  let desiredStatus = canonicalStatus(mutation.desiredStatus) else {
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "A full replacement cannot losslessly preserve this item's fields or status."
                )
                warnings.append("“\(item.title)” has fields or a status this app will not overwrite; the edit remains recoverable.")
                continue
            }
            let (expectedRevision, revisionOverflow) = item.revision.addingReportingOverflow(1)
            guard !revisionOverflow else {
                throw CanonicalSyncError.invalidMutationResponse
            }
            let replacement = DayWeaveCanonicalItemFields(
                item: item,
                status: desiredStatus
            )

            do {
                guard networkPushes < statusPushLimit else {
                    warnings.append(
                        "Deferred remaining status pushes to a later sync after reaching the \(statusPushLimit)-request safety cap."
                    )
                    break
                }
                networkPushes += 1
                let updated = try await client.replaceCanonicalItem(
                    item.id,
                    expectedRevision: item.revision,
                    item: replacement,
                    idempotencyKey: "mac-status-\(item.id.uuidString.lowercased())-r\(item.revision)-\(desiredStatus.wireValue)"
                )
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard updated.id == item.id,
                      updated.revision == expectedRevision,
                      updated.supportsLosslessReplacement,
                      DayWeaveCanonicalItemFields(item: updated) == replacement,
                      updated.deletedAt == nil else {
                    throw CanonicalSyncError.invalidMutationResponse
                }
                planner.upsertCanonicalItem(updated)
                planner.clearCanonicalMutations(itemID: item.id)
                updatedCount += 1
            } catch let error as DayWeaveAPIError {
                try ensureOperationCurrent(operationID: operationID, generation: generation)
                guard case let .server(statusCode, _, _, _) = error, statusCode == 409 else {
                    throw error
                }
                planner.markCanonicalMutationConflicted(
                    itemID: itemID,
                    diagnostic: "The server rejected base revision \(item.revision) as stale."
                )
                warnings.append("“\(item.title)” conflicted remotely; its local status edit was retained.")
            }
        }
        return updatedCount
    }

    private func makeNewItem(from block: ScheduleBlock) -> DayWeaveNewCanonicalItem? {
        guard let status = canonicalStatus(block.status) else { return nil }
        let kind = canonicalKind(block.kind)
        var constraints: [String: JSONValue] = ["energy": .string(block.energy.rawValue)]
        var recurrence: JSONValue?
        if block.kind == .event {
            constraints["calendar_event"] = .object([
                "start": .string(format(block.start)),
                "end": .string(format(block.end)),
                "immutable": .bool(true),
                "all_day": .bool(false),
            ])
        } else if block.kind == .habit {
            recurrence = .object(["type": .string("daily"), "times_per_day": .number(1)])
        } else if block.kind == .goal {
            constraints["has_own_effort"] = .bool(true)
        }

        return DayWeaveNewCanonicalItem(
            id: block.id,
            fields: DayWeaveCanonicalItemFields(
                isSensitive: block.isSensitive,
                kind: kind,
                status: status,
                title: block.title,
                notes: block.notes.isEmpty ? nil : block.notes,
                timezoneName: planningTimezone,
                durationSeconds: UInt32(clamping: block.durationMinutes * 60),
                recurrence: recurrence,
                flexibleConstraints: .object(constraints),
                splitPolicy: .indivisible
            )
        )
    }

    private func makePreviewRequest(
        habitCheckpoint: HabitCompositionCheckpoint? = nil
    ) throws -> DayWeaveSchedulePreviewRequest {
        struct ExceptionRecord {
            let occurredAt: Date
            let occurrenceID: UUID
            let value: JSONValue
        }
        struct PauseRecord {
            let habitID: UUID
            let start: Date
            let end: Date
        }
        let expandedProfile = try planner.scheduleProfile.expanded(asOf: now())
        let asOf = expandedProfile.asOf
        let start = expandedProfile.horizonStart
        let end = expandedProfile.horizonEnd
        let activeItems = planner.canonicalItems.filter { $0.deletedAt == nil }
        let activeItemIDs = Set(activeItems.map(\.id))
        let activeParentIDs = Set(activeItems.compactMap(\.parentID))
        let activeHabitIDs = Set(activeItems.filter { $0.kind == .habit }.map(\.id))
        let authoritativeHabitIDs = habitCheckpoint == nil ? Set<UUID>() : activeHabitIDs
        let currentItemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        let currentRevisionByItem = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
        )
        let storedMoves = planner.recurrenceOccurrenceMoves
        guard storedMoves.allSatisfy({ move in
            move.hasValidShape
                && currentItemByID[move.itemID].map {
                    move.source?.canAuthorizeOccurrenceMove(for: $0) == true
                } == true
        }) else {
            throw CanonicalSyncError.staleOccurrenceMove
        }
        let movedOccurrenceIDsInHorizon = Set(storedMoves.compactMap { move in
            move.startAt >= start && move.endAt <= end ? move.occurrenceID : nil
        })
        let activeOutcomes = planner.recurrenceSessionOutcomes.filter {
            activeItemIDs.contains($0.itemID) && !authoritativeHabitIDs.contains($0.itemID)
        }
        let ordinaryCompletions = Dictionary(
            grouping: activeOutcomes.filter {
                $0.disposition == .completed
                    && $0.occurrenceFullyScheduled
                    && planner.completedOccurrenceIDs.contains($0.occurrenceID)
            },
            by: \.occurrenceID
        )
            .compactMap { occurrenceID, outcomes in
                outcomes.map(\.occurredAt).max().map { (occurrenceID, $0) }
            }
        let checkpointOccurrences = habitCheckpoint?.occurrences ?? []
        let checkpointPauses = habitCheckpoint?.pauses ?? []
        func hasBaseMissedResolutionLifecycle(
            _ occurrence: HabitCompositionCheckpoint.Occurrence
        ) -> Bool {
            guard let resolution = occurrence.missedResolution,
                  let item = currentItemByID[occurrence.habitID],
                  item.kind == .habit,
                  item.isExecutable,
                  !activeParentIDs.contains(item.id),
                  item.recurrence != nil,
                  item.status.allowsMissedHabitScheduling,
                  occurrence.sourceItemRevision <= item.revision,
                  item.habitPolicyFingerprint == occurrence.policyFingerprint,
                  occurrence.outcome?.status.endsMissedResolutionLifecycle != true else {
                return false
            }
            let window = resolution.action.sourceLifecycleWindow(
                fallbackStart: occurrence.windowStart,
                fallbackEnd: occurrence.windowEnd
            )
            return !checkpointPauses.contains { pause in
                pause.habitID == occurrence.habitID
                    && pause.startedAt < window.end
                    && (pause.endedAt ?? .distantFuture) > window.start
            }
        }
        let occurrenceByPlannerID = Dictionary(
            uniqueKeysWithValues: checkpointOccurrences.map {
                ($0.plannerOccurrenceID, $0)
            }
        )
        let publishedOccurrenceAuthority: DayWeavePublishedScheduleOccurrenceAuthority?
        if let proof = planner.publishedScheduleProof,
           proof.configurationIdentifier == planner.canonicalConfigurationIdentifier,
           proof.revisionNumber == planner.publishedScheduleLatestHintRevision {
            publishedOccurrenceAuthority = proof.currentOccurrenceAuthority
        } else {
            publishedOccurrenceAuthority = nil
        }
        func isEligibleMissedReductionTarget(
            _ targetID: UUID,
            for source: HabitCompositionCheckpoint.Occurrence
        ) -> Bool {
            guard let target = occurrenceByPlannerID[targetID],
                  target.habitID == source.habitID,
                  let item = currentItemByID[target.habitID],
                  target.sourceItemRevision <= item.revision,
                  item.habitPolicyFingerprint == target.policyFingerprint,
                  target.outcome.map({ $0.status == .unresolved }) ?? true,
                  let publishedOccurrenceAuthority,
                  publishedOccurrenceAuthority.authorizesMissedReductionTarget(
                      plannerOccurrenceID: target.plannerOccurrenceID,
                      seriesItemID: target.habitID,
                      windowStart: target.windowStart,
                      windowEnd: target.windowEnd
                  ) else {
                return false
            }
            return !checkpointPauses.contains { pause in
                pause.habitID == target.habitID
                    && pause.startedAt < target.windowEnd
                    && (pause.endedAt ?? .distantFuture) > target.windowStart
            }
        }
        let effectiveMissed = effectiveHabitMissedProjection(
            occurrences: checkpointOccurrences,
            sourceIsActive: hasBaseMissedResolutionLifecycle,
            reductionTargetIsEligible: { source, target in
                isEligibleMissedReductionTarget(target.plannerOccurrenceID, for: source)
            }
        )
        func activeMissedAction(
            _ occurrence: HabitCompositionCheckpoint.Occurrence
        ) -> DayWeaveHabitMissedResolutionAction? {
            effectiveMissed.actionsByEvidenceID[occurrence.id]
        }
        let authoritativeOccurrences = checkpointOccurrences.filter { occurrence in
            guard authoritativeHabitIDs.contains(occurrence.habitID),
                  let revision = currentRevisionByItem[occurrence.habitID],
                  occurrence.sourceItemRevision <= revision else { return false }
            let sourceIsRelevant = occurrence.windowStart < end && occurrence.windowEnd > start
            let missedActionIsRelevant: Bool
            switch activeMissedAction(occurrence) {
            case let .carry(windowStart, windowEnd):
                missedActionIsRelevant = windowStart < end && windowEnd > start
            case let .reduceFrequency(ids):
                missedActionIsRelevant = ids.contains(
                    where: effectiveMissed.suppressedPlannerOccurrenceIDs.contains
                )
            default:
                missedActionIsRelevant = false
            }
            return sourceIsRelevant
                || missedActionIsRelevant
                || movedOccurrenceIDsInHorizon.contains(occurrence.plannerOccurrenceID)
        }
        let authoritativeCompletions: [(UUID, Date)] = authoritativeOccurrences.compactMap {
            occurrence in
            guard let outcome = occurrence.outcome, outcome.status == .completed else { return nil }
            return (occurrence.plannerOccurrenceID, outcome.occurredAt)
        }
        var completionByOccurrence: [UUID: Date] = [:]
        for entry in ordinaryCompletions {
            completionByOccurrence[entry.0] = max(
                completionByOccurrence[entry.0] ?? .distantPast,
                entry.1
            )
        }
        for entry in authoritativeCompletions {
            completionByOccurrence[entry.0] = max(
                completionByOccurrence[entry.0] ?? .distantPast,
                entry.1
            )
        }
        let completedOccurrenceIDs: [JSONValue] = completionByOccurrence
            .sorted {
                if $0.value != $1.value { return $0.value > $1.value }
                return $0.key.uuidString < $1.key.uuidString
            }
            .map { JSONValue.string($0.key.uuidString.lowercased()) }

        var completionAnchorDates = planner.recurrenceCompletionAnchors().filter {
            activeItemIDs.contains($0.key) && !authoritativeHabitIDs.contains($0.key)
        }
        for occurrence in habitCheckpoint?.occurrences ?? [] {
            guard authoritativeHabitIDs.contains(occurrence.habitID),
                  let revision = currentRevisionByItem[occurrence.habitID],
                  occurrence.sourceItemRevision <= revision,
                  let outcome = occurrence.outcome,
                  outcome.status == .completed else { continue }
            completionAnchorDates[occurrence.habitID] = max(
                completionAnchorDates[occurrence.habitID] ?? .distantPast,
                outcome.occurredAt
            )
        }
        let completionAnchors = completionAnchorDates.reduce(into: [String: JSONValue]()) {
            result, entry in
            result[entry.key.uuidString.lowercased()] = .string(format(entry.value))
        }
        func canProjectMissedCarry(
            _ occurrence: HabitCompositionCheckpoint.Occurrence
        ) -> Bool {
            guard case let .carry(windowStart, windowEnd)? = activeMissedAction(occurrence),
                  start <= windowStart,
                  windowEnd <= end,
                  let identity = occurrence.identity,
                  occurrence.nominalEnd != nil,
                  occurrence.localDate != nil,
                  let identityData = try? JSONEncoder().encode(identity),
                  let recurrenceIdentity = try? JSONDecoder().decode(
                      RecurrenceOccurrenceIdentity.self,
                      from: identityData
                  ),
                  recurrenceIdentity.stableOrdinal != nil,
                  let item = currentItemByID[occurrence.habitID],
                  recurrenceIdentity.isCompatible(with: item.recurrence) else { return false }
            return true
        }
        func missedResolutionSkipsSource(
            _ occurrence: HabitCompositionCheckpoint.Occurrence
        ) -> Bool {
            guard let action = activeMissedAction(occurrence) else { return false }
            switch action {
            case .skip:
                return true
            case .carry:
                return occurrence.windowStart < end
                    && occurrence.windowEnd > start
                    && !canProjectMissedCarry(occurrence)
            default:
                return false
            }
        }
        let authoritativeSkippedOccurrenceIDs = authoritativeOccurrences.reduce(
            into: Set<UUID>()
        ) { skipped, occurrence in
            if occurrence.outcome?.status == .skipped
                || missedResolutionSkipsSource(occurrence) {
                skipped.insert(occurrence.plannerOccurrenceID)
            }
            if case let .reduceFrequency(ids)? = activeMissedAction(occurrence) {
                skipped.formUnion(ids.filter {
                    effectiveMissed.suppressedPlannerOccurrenceIDs.contains($0)
                })
            }
        }
        let authoritativeCarryOccurrenceIDs = Set(authoritativeOccurrences.compactMap {
            occurrence -> UUID? in
            guard case .carry? = activeMissedAction(occurrence) else { return nil }
            return occurrence.plannerOccurrenceID
        })
        // A server-owned skip, reduction, or carry is the sole recurrence
        // exception authority for its occurrence. A locally stored move may
        // predate or postdate that lifecycle row, but must never resurrect or
        // replace it; this mirrors the server composition merge.
        let authoritativeExceptionOwnedOccurrenceIDs =
            authoritativeSkippedOccurrenceIDs.union(authoritativeCarryOccurrenceIDs)
        let partialProgress = try authoritativeOccurrences.reduce(
            into: [String: JSONValue]()
        ) { result, occurrence in
            guard let outcome = occurrence.outcome,
                  outcome.status == .partial,
                  !authoritativeSkippedOccurrenceIDs.contains(occurrence.plannerOccurrenceID),
                  let expectedSeconds = occurrence.expectedDurationSeconds else { return }
            let rounded = expectedSeconds.addingReportingOverflow(59)
            guard !rounded.overflow, rounded.partialValue / 60 > 0 else {
                throw LocalCompositionCoordinatorError.incompleteHabitLedger
            }
            result[occurrence.plannerOccurrenceID.uuidString.lowercased()] = .object([
                "progress_basis_points": .number(.init(UInt64(outcome.progressBasisPoints))),
                "expected_duration_minutes": .number(.init(rounded.partialValue / 60)),
            ])
        }
        let ordinarySkipExceptions = activeOutcomes
            .filter { $0.disposition == .skipped }
            .reduce(into: [UUID: RecurrenceSessionOutcome]()) { latest, outcome in
                if latest[outcome.occurrenceID]?.occurredAt ?? .distantPast < outcome.occurredAt {
                    latest[outcome.occurrenceID] = outcome
                }
            }
            .values
            .sorted {
                if $0.occurredAt != $1.occurredAt { return $0.occurredAt > $1.occurredAt }
                return $0.occurrenceID.uuidString < $1.occurrenceID.uuidString
            }
            .map { outcome in
                ExceptionRecord(
                    occurredAt: outcome.occurredAt,
                    occurrenceID: outcome.occurrenceID,
                    value: .object([
                        "item_id": .string(outcome.itemID.uuidString.lowercased()),
                        "selector": .object([
                            "type": .string("occurrence"),
                            "id": .string(outcome.occurrenceID.uuidString.lowercased()),
                        ]),
                        "action": .object(["type": .string("skip")]),
                    ])
                )
            }
        let authoritativeSkipExceptions: [ExceptionRecord] = authoritativeOccurrences.compactMap {
            occurrence in
            let outcomeSkipped = occurrence.outcome?.status == .skipped
            // If a carry cannot be represented faithfully in this compose
            // request, suppress the source while the server retains the
            // durable carry decision. This prevents the old slot returning.
            let missedSkipped = missedResolutionSkipsSource(occurrence)
            guard outcomeSkipped || missedSkipped else { return nil }
            return ExceptionRecord(
                occurredAt: max(
                    outcomeSkipped ? occurrence.outcome?.occurredAt ?? .distantPast : .distantPast,
                    missedSkipped
                        ? occurrence.missedResolution?.updatedAt ?? .distantPast : .distantPast
                ),
                occurrenceID: occurrence.plannerOccurrenceID,
                value: .object([
                    "item_id": .string(occurrence.habitID.uuidString.lowercased()),
                    "selector": .object([
                        "type": .string("occurrence"),
                        "id": .string(occurrence.plannerOccurrenceID.uuidString.lowercased()),
                    ]),
                    "action": .object(["type": .string("skip")]),
                ])
            )
        }
        let missedReductionExceptions: [ExceptionRecord] = authoritativeOccurrences.flatMap {
            occurrence -> [ExceptionRecord] in
            guard let resolution = occurrence.missedResolution,
                  case let .reduceFrequency(ids)? = activeMissedAction(occurrence) else { return [] }
            return ids.compactMap { occurrenceID in
                guard effectiveMissed.suppressedPlannerOccurrenceIDs.contains(occurrenceID) else {
                    return nil
                }
                return ExceptionRecord(
                    occurredAt: resolution.updatedAt,
                    occurrenceID: occurrenceID,
                    value: .object([
                        "item_id": .string(occurrence.habitID.uuidString.lowercased()),
                        "selector": .object([
                            "type": .string("occurrence"),
                            "id": .string(occurrenceID.uuidString.lowercased()),
                        ]),
                        "action": .object(["type": .string("skip")]),
                    ])
                )
            }
        }
        let skipExceptions = (ordinarySkipExceptions
            + authoritativeSkipExceptions
            + missedReductionExceptions)
            .sorted {
                if $0.occurredAt != $1.occurredAt { return $0.occurredAt > $1.occurredAt }
                return $0.occurrenceID.uuidString < $1.occurrenceID.uuidString
            }
            .reduce(into: [UUID: ExceptionRecord]()) { values, exception in
                if values[exception.occurrenceID] == nil {
                    values[exception.occurrenceID] = exception
                }
            }
            .values
            .map { $0 }
        let pauseRecords: [PauseRecord] = checkpointPauses
            .compactMap { pause in
                guard authoritativeHabitIDs.contains(pause.habitID) else { return nil }
                let clippedStart = max(pause.startedAt, start)
                let clippedEnd = min(pause.endedAt ?? end, end)
                guard clippedStart < clippedEnd else { return nil }
                return PauseRecord(habitID: pause.habitID, start: clippedStart, end: clippedEnd)
            }
            .sorted {
                if $0.habitID != $1.habitID {
                    return $0.habitID.uuidString < $1.habitID.uuidString
                }
                if $0.start != $1.start { return $0.start < $1.start }
                return $0.end < $1.end
            }
        let recurrencePauses: [JSONValue] = pauseRecords.map { pause in
                .object([
                    "item_id": .string(pause.habitID.uuidString.lowercased()),
                    "start": .string(format(pause.start)),
                    "end": .string(format(pause.end)),
                ])
        }
        let moveExceptions = storedMoves
            .filter { !authoritativeExceptionOwnedOccurrenceIDs.contains($0.occurrenceID) }
            .map { move in
                let source = move.source!
                return ExceptionRecord(
                    occurredAt: move.movedAt,
                    occurrenceID: move.occurrenceID,
                    value: .object([
                        "item_id": .string(move.itemID.uuidString.lowercased()),
                        "selector": .object([
                            "type": .string("occurrence"),
                            "id": .string(move.occurrenceID.uuidString.lowercased()),
                        ]),
                        "action": .object([
                            "type": .string("move"),
                            "start": .string(format(move.startAt)),
                            "end": .string(format(move.endAt)),
                            "source": .object([
                                "item_revision": .number(.init(source.itemRevision)),
                                "identity": source.identity.jsonValue,
                                "nominal_start": .string(source.nominalStart),
                                "nominal_end": .string(source.nominalEnd),
                                "local_date": source.localDate.map(JSONValue.string) ?? .null,
                                "ordinal": .number(.init(UInt64(source.ordinal))),
                            ]),
                        ]),
                    ])
                )
            }
        let missedMoveExceptions: [ExceptionRecord] = authoritativeOccurrences.compactMap {
            occurrence in
            guard let resolution = occurrence.missedResolution,
                  case let .carry(windowStart, windowEnd)? = activeMissedAction(occurrence),
                  start <= windowStart,
                  windowEnd <= end,
                  let identity = occurrence.identity,
                  let nominalEnd = occurrence.nominalEnd,
                  let localDate = occurrence.localDate,
                  let identityData = try? JSONEncoder().encode(identity),
                  let recurrenceIdentity = try? JSONDecoder().decode(
                      RecurrenceOccurrenceIdentity.self,
                      from: identityData
                  ),
                  let ordinal = recurrenceIdentity.stableOrdinal,
                  let item = currentItemByID[occurrence.habitID],
                  item.habitPolicyFingerprint == occurrence.policyFingerprint,
                  recurrenceIdentity.isCompatible(with: item.recurrence) else { return nil }
            return ExceptionRecord(
                occurredAt: resolution.updatedAt,
                occurrenceID: occurrence.plannerOccurrenceID,
                value: .object([
                    "item_id": .string(occurrence.habitID.uuidString.lowercased()),
                    "selector": .object([
                        "type": .string("occurrence"),
                        "id": .string(occurrence.plannerOccurrenceID.uuidString.lowercased()),
                    ]),
                    "action": .object([
                        "type": .string("move"),
                        "start": .string(format(windowStart)),
                        "end": .string(format(windowEnd)),
                        "source": .object([
                            "item_revision": .number(.init(item.revision)),
                            "identity": identity,
                            "nominal_start": .string(format(occurrence.nominalStart)),
                            "nominal_end": .string(format(nominalEnd)),
                            "local_date": .string(localDate.rawValue),
                            "ordinal": .number(.init(UInt64(ordinal))),
                        ]),
                    ]),
                ])
            )
        }
        guard completedOccurrenceIDs.count + completionAnchors.count + partialProgress.count
            + recurrencePauses.count + skipExceptions.count + moveExceptions.count
            + missedMoveExceptions.count <= 9_000 else {
            throw CanonicalSyncError.recurrenceContextCapacity
        }
        let recurrenceExceptions = (skipExceptions + moveExceptions + missedMoveExceptions)
            .sorted {
                if $0.occurredAt != $1.occurredAt {
                    return $0.occurredAt > $1.occurredAt
                }
                return $0.occurrenceID.uuidString < $1.occurrenceID.uuidString
            }
            .map(\.value)
        return .init(
            asOf: asOf,
            horizonStart: start,
            horizonEnd: end,
            timezoneName: expandedProfile.timezoneName,
            availability: expandedProfile.availability,
            fixedBlocks: expandedProfile.fixedBlocks,
            previousAssignments: previousAssignments(stabilityStart: asOf, horizonEnd: end),
            config: .init(slotGranularityMinutes: 5, stabilityWeight: 4, defaultSoftWeight: 100),
            recurrenceContext: [
                "completed_occurrence_ids": .array(completedOccurrenceIDs),
                "completion_anchors": .object(completionAnchors),
                "partial_progress": .object(partialProgress),
                "pauses": .array(recurrencePauses),
                "exceptions": .array(recurrenceExceptions),
            ]
        )
    }

    private func previousAssignments(
        stabilityStart: Date,
        horizonEnd: Date
    ) -> [DayWeaveSchedulePreviewRequest.PreviousAssignment] {
        struct AssignmentKey: Hashable {
            let itemID: UUID
            let itemRevision: UInt64
            let occurrenceID: UUID?
        }
        let frozenUntil = now().addingTimeInterval(TimeInterval(planner.freezeHours * 3_600))
        let currentRevisionByItem = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0.revision) }
        )
        let candidates = planner.blocks.compactMap { block -> (AssignmentKey, ScheduleBlock)? in
            guard block.syncOrigin == .canonicalPreview
                    || block.syncOrigin == .localComposition,
                  (block.previewKind == "planned" || block.previewKind == "pinned"),
                  block.status != .completed,
                  block.status != .skipped,
                  block.status != .canceled,
                  let itemID = block.sourceItemID,
                  let revision = block.sourceItemRevision,
                  currentRevisionByItem[itemID] == revision,
                  block.end > stabilityStart,
                  block.start < horizonEnd else { return nil }
            return (.init(itemID: itemID, itemRevision: revision, occurrenceID: block.occurrenceID), block)
        }
        let assignments: [DayWeaveSchedulePreviewRequest.PreviousAssignment] = Dictionary(
            grouping: candidates,
            by: \.0
        )
            .compactMap { key, entries -> DayWeaveSchedulePreviewRequest.PreviousAssignment? in
                let blocks = entries.map(\.1)
                let sessionIndices = blocks.map { $0.sessionIndex ?? 0 }
                guard Set(sessionIndices).count == sessionIndices.count else {
                    warnings.append(
                        "A previous assignment for \(key.itemID.uuidString) had duplicate session indices and was omitted."
                    )
                    return nil
                }
                let allInsideFreeze = blocks.allSatisfy {
                    $0.start >= stabilityStart && $0.end <= frozenUntil
                }
                return .init(
                    itemID: key.itemID,
                    itemRevision: key.itemRevision,
                    occurrenceID: key.occurrenceID,
                    blocks: entries.map(\.1).sorted {
                        if $0.start != $1.start { return $0.start < $1.start }
                        if $0.end != $1.end { return $0.end < $1.end }
                        return ($0.sessionIndex ?? 0) < ($1.sessionIndex ?? 0)
                    }.map {
                        .init(
                            start: $0.start,
                            end: $0.end,
                            sessionIndex: $0.sessionIndex ?? 0
                        )
                    },
                    pinned: allInsideFreeze
                )
            }
            .sorted {
                if $0.itemID != $1.itemID { return $0.itemID.uuidString < $1.itemID.uuidString }
                if $0.occurrenceID != $1.occurrenceID {
                    return ($0.occurrenceID?.uuidString ?? "") < ($1.occurrenceID?.uuidString ?? "")
                }
                return $0.itemRevision < $1.itemRevision
            }
        var retained: [DayWeaveSchedulePreviewRequest.PreviousAssignment] = []
        retained.reserveCapacity(min(assignments.count, previousAssignmentLimit))
        var retainedBlockCount = 0
        var omittedAssignmentCount = 0
        var omittedBlockCount = 0
        for assignment in assignments {
            let blockCount = assignment.blocks.count
            let (nextBlockCount, overflow) = retainedBlockCount.addingReportingOverflow(blockCount)
            guard retained.count < previousAssignmentLimit,
                  !overflow,
                  nextBlockCount <= previousAssignmentBlockLimit else {
                omittedAssignmentCount += 1
                omittedBlockCount += blockCount
                continue
            }
            retained.append(assignment)
            retainedBlockCount = nextBlockCount
        }
        if omittedAssignmentCount > 0 {
            warnings.append(
                "Omitted \(omittedAssignmentCount) previous assignment\(omittedAssignmentCount == 1 ? "" : "s") containing \(omittedBlockCount) block\(omittedBlockCount == 1 ? "" : "s") to stay within the API's \(previousAssignmentLimit)-assignment/\(previousAssignmentBlockLimit)-block limits."
            )
        }
        return retained
    }

    private func render(_ preview: DayWeaveSchedulePreview) -> [ScheduleBlock] {
        render(
            plan: preview.plan,
            sourceItemRevisions: preview.sourceItemRevisions,
            origin: nil
        )
    }

    private func render(
        plan: DayWeaveSchedulePreview.Plan,
        sourceItemRevisions: [UUID: UInt64],
        origin: ScheduleBlockOrigin?
    ) -> [ScheduleBlock] {
        let itemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        var previousBySession: [PreviewSessionIdentity: ScheduleBlock] = [:]
        var duplicatePreviousSessions = Set<PreviewSessionIdentity>()
        for previous in planner.blocks {
            guard let itemID = previous.sourceItemID else { continue }
            let identity = PreviewSessionIdentity(
                itemID: itemID,
                occurrenceID: previous.occurrenceID,
                sessionIndex: previous.sessionIndex ?? 0
            )
            if previousBySession.updateValue(previous, forKey: identity) != nil {
                duplicatePreviousSessions.insert(identity)
            }
        }
        for identity in duplicatePreviousSessions {
            previousBySession.removeValue(forKey: identity)
        }
        if !duplicatePreviousSessions.isEmpty {
            warnings.append(
                "\(duplicatePreviousSessions.count) duplicate local session identities were not used to restore status."
            )
        }
        let unscheduledOccurrences = Set(plan.unscheduled.map {
            PreviewOccurrenceIdentity(itemID: $0.itemID, occurrenceID: $0.occurrenceID)
        })
        let occurrenceByID = Dictionary(
            plan.occurrences.map { ($0.id, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        return plan.blocks.map { block in
            let item = block.itemID.flatMap { itemByID[$0] }
            let previous = block.itemID.flatMap { itemID -> ScheduleBlock? in
                let candidate = previousBySession[.init(
                    itemID: itemID,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: block.sessionIndex
                )]
                return candidate?.sourceItemRevision == item?.revision ? candidate : nil
            }
            let explanation = block.explanations.map(\.message).joined(separator: " ")
            let occurrenceFullyScheduled = block.itemID.map {
                !unscheduledOccurrences.contains(.init(
                    itemID: $0,
                    occurrenceID: block.occurrenceID
                ))
            } ?? true
            let isPlannable = block.kind == "planned" || block.kind == "pinned"
            let recurrenceMoveSource = block.occurrenceID
                .flatMap { occurrenceByID[$0] }
                .flatMap { occurrence -> RecurrenceMoveSource? in
                    guard let revision = sourceItemRevisions[occurrence.seriesItemID],
                          let blockItemID = block.itemID,
                          Self.item(
                            blockItemID,
                            belongsToSeries: occurrence.seriesItemID,
                            itemByID: itemByID
                          ),
                          occurrence.identity.isCompatible(
                            with: itemByID[occurrence.seriesItemID]?.recurrence
                          ) else { return nil }
                    let source = RecurrenceMoveSource(
                        itemRevision: revision,
                        identity: occurrence.identity,
                        nominalStart: occurrence.nominalStart,
                        nominalEnd: occurrence.nominalEnd,
                        localDate: occurrence.localDate,
                        ordinal: occurrence.ordinal
                    )
                    return source.hasValidShape ? source : nil
                }
            return ScheduleBlock(
                id: block.id,
                isSensitive: block.isSensitive,
                title: block.title,
                kind: item.map { plannerKind($0.kind) } ?? (block.kind == "calendar_event" ? .event : .breakTime),
                start: block.start,
                end: block.end,
                status: previous?.status ?? item.map { plannerStatus($0.status) } ?? .scheduled,
                project: item?.parentID.flatMap { itemByID[$0]?.title },
                notes: item?.notes ?? "",
                energy: item.map(energyLevel) ?? .medium,
                isFlexible: isPlannable && item?.kind != .event,
                isHardConstraint: !isPlannable,
                actualMinutes: previous?.actualMinutes,
                sourceItemID: block.itemID,
                sourceItemRevision: item?.revision,
                occurrenceID: block.occurrenceID,
                externalBlockID: block.externalBlockID,
                recurrenceSeriesItemID: block.occurrenceID
                    .flatMap { occurrenceByID[$0]?.seriesItemID },
                sessionIndex: block.sessionIndex,
                syncOrigin: origin ?? (item == nil ? .externalPreview : .canonicalPreview),
                placementReason: explanation.isEmpty ? nil : explanation,
                previewKind: block.kind,
                occurrenceFullyScheduled: occurrenceFullyScheduled,
                recurrenceMoveSource: recurrenceMoveSource
            )
        }
    }

    private func previewMessage(
        _ preview: DayWeaveSchedulePreview,
        created: Int,
        privacyUpdated: Int,
        updated: Int
    ) -> String {
        var parts = [
            "Composed \(Self.composedBlockSummary(preview.plan.blocks))",
            "\(preview.plan.score.unscheduledMinutes)m unscheduled",
        ]
        if created > 0 { parts.append("published \(created) new") }
        if privacyUpdated > 0 { parts.append("updated \(privacyUpdated) privacy") }
        if updated > 0 { parts.append("updated \(updated)") }
        if !preview.rejectedItems.isEmpty { parts.append("\(preview.rejectedItems.count) need review") }
        if !warnings.isEmpty { parts.append("\(warnings.count) sync warning\(warnings.count == 1 ? "" : "s")") }
        return parts.joined(separator: " · ")
    }

    /// Recurrence ownership follows the canonical hierarchy: a recurring
    /// routine owns every executable descendant leaf emitted for its
    /// occurrence. Cycles and missing ancestors fail closed.
    private static func item(
        _ itemID: UUID,
        belongsToSeries seriesItemID: UUID,
        itemByID: [UUID: DayWeaveCanonicalItem]
    ) -> Bool {
        var currentID: UUID? = itemID
        var visited = Set<UUID>()
        while let identifier = currentID {
            guard visited.insert(identifier).inserted,
                  let item = itemByID[identifier] else { return false }
            if identifier == seriesItemID { return true }
            currentID = item.parentID
        }
        return false
    }

    private static func composedBlockSummary(
        _ blocks: [DayWeaveSchedulePreview.Plan.Block]
    ) -> String {
        let fixedCount = blocks.count(where: { $0.kind == "external_fixed" })
        let plannedCount = blocks.count - fixedCount
        let planned = "\(plannedCount) work/calendar block\(plannedCount == 1 ? "" : "s")"
        guard fixedCount > 0 else { return planned }
        let fixed = "\(fixedCount) fixed-time block\(fixedCount == 1 ? "" : "s")"
        return plannedCount > 0 ? "\(planned) + \(fixed)" : fixed
    }

    private var planningTimezone: String {
        planner.scheduleProfile.timezoneName
    }

    private func energyLevel(_ item: DayWeaveCanonicalItem) -> EnergyLevel {
        guard case let .object(constraints) = item.flexibleConstraints,
              let energy = constraints["energy"] else { return .medium }
        let value: String?
        switch energy {
        case let .string(raw): value = raw
        case let .object(object):
            if case let .string(raw)? = object["value"] { value = raw } else { value = nil }
        default: value = nil
        }
        return value.flatMap(EnergyLevel.init(rawValue:)) ?? .medium
    }

    private func reloadConfigurationStatus() {
        status = makeClient(reportFailure: false) == nil
            ? .configurationRequired("Add the DayWeave API URL and bearer token in Settings.")
            : .ready
    }

    private func ensureOperationCurrent(
        operationID: UUID,
        generation: UInt64
    ) throws {
        guard !Task.isCancelled,
              activeSyncID == operationID,
              configurationGeneration == generation,
              activeSyncScheduleProfile == planner.scheduleProfile else {
            throw CanonicalSyncError.operationSuperseded
        }
        guard planner.canPersistPlan else {
            if let persistenceError = planner.persistenceError { throw persistenceError }
            throw CanonicalSyncError.localPersistenceUnavailable
        }
    }

    private func requireLocalCompositionPreflight() throws -> HabitCompositionCheckpoint? {
        guard !Task.isCancelled,
              activeSyncID == nil,
              activeSyncTask == nil,
              activeLocalCompositionID == nil,
              activeLocalCompositionTask == nil,
              !isSyncing,
              !planner.isCanonicalSyncLocked else {
            throw LocalCompositionCoordinatorError.busy
        }
        guard planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              planner.persistenceError == nil else {
            throw LocalCompositionCoordinatorError.persistenceUnavailable
        }
        guard let cursor = planner.canonicalDeltaCursor,
              !cursor.isEmpty,
              cursor.utf8.count <= Self.maximumDeltaCursorBytes,
              let configurationIdentifier = planner.canonicalConfigurationIdentifier,
              !configurationIdentifier.isEmpty else {
            throw LocalCompositionCoordinatorError.incompleteCanonicalCache
        }
        guard planner.pendingSchedulePublication == nil else {
            throw LocalCompositionCoordinatorError.pendingSchedulePublication
        }
        guard planner.pendingProposalApplicationMutation == nil else {
            throw LocalCompositionCoordinatorError.pendingProposalApplication
        }
        guard planner.googleOutboundRecoveryJournal == nil,
              !planner.hasGoogleSchedulePublicationAuthorityFence else {
            throw LocalCompositionCoordinatorError.pendingGoogleRecovery
        }
        guard planner.pendingCanonicalMutations.isEmpty else {
            throw LocalCompositionCoordinatorError.pendingStatusMutation
        }
        guard planner.pendingCanonicalSensitivityMutations.isEmpty else {
            throw LocalCompositionCoordinatorError.pendingSensitivityMutation
        }
        guard planner.pendingCanonicalAuthoringMutations.isEmpty else {
            throw LocalCompositionCoordinatorError.pendingAuthoringMutation
        }
        guard planner.executionState.activeSession == nil,
              planner.executionState.pendingCommand == nil,
              !planner.executionState.hasCredentialReplacementBlocker,
              !planner.blocks.contains(where: { $0.syncOrigin == .remoteExecutionLease }) else {
            throw LocalCompositionCoordinatorError.executionLeaseActive
        }
        guard planner.canonicalItems.count <= 10_000 else {
            throw LocalCompositionCoordinatorError.canonicalResourceLimit
        }
        let checkpoint = habitCompositionProvider?.habitCompositionCheckpoint
        if let checkpoint,
           !checkpoint.pendingMutationIDs.isEmpty || checkpoint.hasActiveOperation {
            throw LocalCompositionCoordinatorError.incompleteHabitLedger
        }
        let activeHabitRevisions = Dictionary(uniqueKeysWithValues:
            planner.canonicalItems.compactMap { item in
                item.kind == .habit && item.deletedAt == nil
                    ? (item.id, item.revision) : nil
            }
        )
        guard !activeHabitRevisions.isEmpty else { return checkpoint }
        guard let checkpoint,
              checkpoint.isAuthoritative(
                  for: configurationIdentifier,
                  activeHabitRevisions: activeHabitRevisions
              ) else {
            throw LocalCompositionCoordinatorError.incompleteHabitLedger
        }
        return checkpoint
    }

    private func ensureLocalCompositionCurrent(
        operationID: UUID,
        generation: UInt64
    ) throws {
        guard !Task.isCancelled,
              activeLocalCompositionID == operationID,
              configurationGeneration == generation,
              activeLocalCompositionScheduleProfile == planner.scheduleProfile,
              activeSyncID == nil,
              planner.isCanonicalSyncLocked else {
            throw LocalCompositionCoordinatorError.operationSuperseded
        }
        guard planner.hasEncryptedPersistence,
              planner.canPersistPlan,
              planner.persistenceError == nil else {
            throw LocalCompositionCoordinatorError.persistenceUnavailable
        }
        guard planner.pendingSchedulePublication == nil,
              planner.pendingProposalApplicationMutation == nil,
              planner.googleOutboundRecoveryJournal == nil,
              !planner.hasGoogleSchedulePublicationAuthorityFence,
              planner.pendingCanonicalMutations.isEmpty,
              planner.pendingCanonicalSensitivityMutations.isEmpty,
              planner.pendingCanonicalAuthoringMutations.isEmpty,
              planner.executionState.activeSession == nil,
              planner.executionState.pendingCommand == nil,
              !planner.executionState.hasCredentialReplacementBlocker,
              !planner.blocks.contains(where: { $0.syncOrigin == .remoteExecutionLease }) else {
            throw LocalCompositionCoordinatorError.canonicalStateChanged
        }
    }

    private func reportLocalCompositionFailure(_ error: any Error) {
        let diagnostic = (error as? LocalizedError)?.errorDescription
            ?? error.localizedDescription
        localCompositionStatus = .failed(
            "\(diagnostic) No network request or publication was made. Use normal Sync as the explicit fallback."
        )
    }

    private func clearTransientLocalComposition() {
        lastLocalComposition = nil
        lastLocalCompositionScore = nil
        localCompositionWarnings = []
        localCompositionStatus = .ready
    }

    private func scheduleProfileDidCommit() {
        // PlannerStore emits this boundary only after the encrypted CAS has
        // committed. A failed candidate/rollback therefore preserves all
        // transient evidence associated with the still-installed schedule.
        lastPreview = nil
        warnings = []
        clearTransientLocalComposition()
        reloadConfigurationStatus()
    }

    private func habitCompositionCheckpointDidChange() {
        planner.invalidateCanonicalPreview()
        lastPreview = nil
        clearTransientLocalComposition()
    }

    private func makeClient(reportFailure: Bool) -> DayWeaveAPIClient? {
        do {
            guard let configuredURL = configurationStore.loadBaseURL() else {
                if reportFailure {
                    status = .configurationRequired("Add the DayWeave API URL and bearer token in Settings.")
                }
                return nil
            }
            let baseURL = try DayWeaveAPIBaseURL(configuredURL)
            if let authCoordinator {
                guard authCoordinator.hasUsableCredential(boundTo: baseURL) else {
                    if reportFailure {
                        status = .configurationRequired("Authenticate this Mac in Settings before syncing.")
                    }
                    return nil
                }
                return DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: session,
                    authCoordinator: authCoordinator
                )
            }
            guard let token = try tokenStore.loadToken(boundTo: baseURL),
                  !token.isEmpty else {
                if reportFailure {
                    status = .configurationRequired("Add the DayWeave API URL and bearer token in Settings.")
                }
                return nil
            }
            return DayWeaveAPIClient(
                baseURL: baseURL,
                session: session,
                bearerToken: token
            )
        } catch {
            if reportFailure { status = .configurationRequired(error.localizedDescription) }
            return nil
        }
    }

    private func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

private enum PendingSchedulePublicationRecovery {
    case installed(revisionNumber: UInt64, blockSummary: String)
    case requiresFreshComposition
}

private struct InstalledSchedulePublication {
    let preview: DayWeaveSchedulePreview
    let blockSummary: String
}

private struct LocalCompositionMutationFence: Sendable {
    let canonicalItems: [DayWeaveCanonicalItem]
    let canonicalDeltaCursor: String?
    let canonicalConfigurationIdentifier: String?
    let completedOccurrenceIDs: Set<UUID>
    let recurrenceSessionOutcomes: [RecurrenceSessionOutcome]
    let recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove]
    let deferredExecutionPublicationSessionIDs: Set<UUID>
    let blocks: [ScheduleBlock]
    let publishedScheduleProof: DayWeavePublishedScheduleProof?
    let publishedScheduleLatestHintRevision: UInt64
    let scheduleProfile: ScheduleProfile
    let freezeHours: Int
    let timezoneName: String
    let habitCompositionCheckpoint: HabitCompositionCheckpoint?

    @MainActor
    func matches(
        planner: PlannerStore,
        timezoneName currentTimezoneName: String,
        currentHabitCheckpoint: HabitCompositionCheckpoint?
    ) -> Bool {
        canonicalItems == planner.canonicalItems
            && canonicalDeltaCursor == planner.canonicalDeltaCursor
            && canonicalConfigurationIdentifier == planner.canonicalConfigurationIdentifier
            && completedOccurrenceIDs == planner.completedOccurrenceIDs
            && recurrenceSessionOutcomes == planner.recurrenceSessionOutcomes
            && recurrenceOccurrenceMoves == planner.recurrenceOccurrenceMoves
            && deferredExecutionPublicationSessionIDs
                == planner.deferredExecutionPublicationSessionIDs
            && blocks == planner.blocks
            && publishedScheduleProof == planner.publishedScheduleProof
            && publishedScheduleLatestHintRevision
                == planner.publishedScheduleLatestHintRevision
            && scheduleProfile == planner.scheduleProfile
            && freezeHours == planner.freezeHours
            && timezoneName == currentTimezoneName
            && habitCompositionCheckpoint == currentHabitCheckpoint
    }
}

private enum LocalCompositionCoordinatorError: LocalizedError {
    case busy
    case persistenceUnavailable
    case incompleteCanonicalCache
    case pendingSchedulePublication
    case pendingProposalApplication
    case pendingGoogleRecovery
    case pendingStatusMutation
    case pendingSensitivityMutation
    case pendingAuthoringMutation
    case executionLeaseActive
    case canonicalResourceLimit
    case incompleteHabitLedger
    case canonicalStateChanged
    case operationSuperseded
    case invalidHelperResponse

    var errorDescription: String? {
        switch self {
        case .busy:
            "Wait for the active canonical, execution, or on-device composition operation to finish."
        case .persistenceUnavailable:
            "Healthy encrypted planner persistence is required for on-device composition."
        case .incompleteCanonicalCache:
            "The encrypted canonical cache is not complete and bound to an API configuration."
        case .pendingSchedulePublication:
            "Recover the exact pending schedule publication before composing on this device."
        case .pendingProposalApplication:
            "Recover the exact pending proposal application or undo before composing on this device."
        case .pendingGoogleRecovery:
            "Recover the pending Google publication before composing on this device."
        case .pendingStatusMutation:
            "Resolve the pending canonical status journal before composing on this device."
        case .pendingSensitivityMutation:
            "Resolve the pending canonical privacy journal before composing on this device."
        case .pendingAuthoringMutation:
            "Resolve the pending canonical authoring journal before composing on this device."
        case .executionLeaseActive:
            "Reconcile the remote execution lease and actionability state before composing on this device."
        case .canonicalResourceLimit:
            "The canonical cache exceeds the on-device scheduler's 10,000-item safety limit."
        case .incompleteHabitLedger:
            "Synchronize the complete habit history before composing on this device."
        case .canonicalStateChanged:
            "Canonical schedule inputs changed while the helper was running; its result was discarded."
        case .operationSuperseded:
            "On-device composition was cancelled because its operation or configuration changed."
        case .invalidHelperResponse:
            "The signed helper response did not match the captured canonical schedule request."
        }
    }
}

private struct PreviewSessionIdentity: Hashable {
    let itemID: UUID
    let occurrenceID: UUID?
    let sessionIndex: UInt16
}

private struct PreviewOccurrenceIdentity: Hashable {
    let itemID: UUID
    let occurrenceID: UUID?
}

private enum CanonicalSyncError: LocalizedError {
    case invalidDeltaSequence
    case tooManyDeltaPages
    case localPersistenceUnavailable
    case operationSuperseded
    case sourceRevisionMismatch
    case invalidPreview(String)
    case invalidSchedulePublication
    case schedulePublicationStayedStale
    case schedulePublicationReplayNeedsFreshComposition
    case deltaResourceLimit
    case invalidMutationResponse
    case staleOccurrenceMove
    case recurrenceContextCapacity

    var errorDescription: String? {
        switch self {
        case .invalidDeltaSequence: "The canonical item stream returned a non-progressing cursor."
        case .tooManyDeltaPages: "Canonical sync exceeded the safe page limit."
        case .localPersistenceUnavailable: "Canonical sync stopped because encrypted local storage is unavailable."
        case .operationSuperseded: "Canonical sync was cancelled because its configuration changed."
        case .sourceRevisionMismatch: "The scheduler preview never matched the local canonical revision map after three bounded retries."
        case let .invalidPreview(diagnostic): "The scheduler preview was rejected safely: \(diagnostic)"
        case .invalidSchedulePublication: "The schedule publication journal or server acknowledgment was rejected safely. The exact request remains available for recovery."
        case .schedulePublicationStayedStale: "Canonical items changed during both bounded publication attempts. No candidate was applied or left pending; sync again to retry."
        case .schedulePublicationReplayNeedsFreshComposition: "The bounded fresh publication also returned an older exact receipt. No candidate was applied or left pending; sync again to recompose."
        case .deltaResourceLimit: "Canonical sync exceeded the safe 20,000-change or 32 MiB retained-delta limit."
        case .invalidMutationResponse: "The canonical API returned a mutation result with the wrong identity, status, or revision. Local state was not changed."
        case .staleOccurrenceMove: "A saved occurrence move no longer matches the canonical item revision. Review the refreshed occurrence before moving it again."
        case .recurrenceContextCapacity: "The recurrence completion, skip, and move ledger exceeds the scheduler's safe request capacity. No recurrence exception was omitted."
        }
    }
}

private func canonicalKind(_ kind: PlannerItemKind) -> DayWeaveCanonicalItemKind {
    switch kind {
    case .event: .event
    case .task: .task
    case .habit: .habit
    case .routine: .routine
    case .goal: .goal
    case .project: .project
    case .breakTime: .breakTime
    }
}

private func plannerKind(_ kind: DayWeaveCanonicalItemKind) -> PlannerItemKind {
    switch kind {
    case .event: .event
    case .task: .task
    case .habit: .habit
    case .routine: .routine
    case .goal: .goal
    case .project: .project
    case .breakTime: .breakTime
    case .unknown: .task
    }
}

private func canonicalStatus(_ status: PlannerItemStatus) -> DayWeaveCanonicalItemStatus? {
    switch status {
    case .notStarted: .planned
    case .scheduled: .scheduled
    case .active: .inProgress
    case .paused: .paused
    case .completed: .completed
    case .skipped: .skipped
    case .canceled: .cancelled
    case .blocked: nil
    }
}

private func plannerStatus(_ status: DayWeaveCanonicalItemStatus) -> PlannerItemStatus {
    switch status {
    case .inbox, .planned: .notStarted
    case .scheduled: .scheduled
    case .inProgress: .active
    case .paused: .paused
    case .blocked: .blocked
    case .completed: .completed
    case .skipped: .skipped
    case .cancelled: .canceled
    case .unknown: .blocked
    }
}
