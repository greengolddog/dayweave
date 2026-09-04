import CryptoKit
import Foundation

protocol DayWeaveExecutionTransport: Sendable {
    func executionSnapshot() async throws -> DayWeaveExecutionSnapshot
    func executionHistoryPage(limit: Int, offset: Int) async throws -> DayWeaveExecutionHistoryPage
    func assessExecutionDefer(
        _ request: DayWeaveDeferAssessmentRequest
    ) async throws -> DayWeaveDeferAssessment
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
    let streamTransport: (any DayWeaveExecutionStreamTransport)?

    init(
        canonicalConfigurationIdentifier: String,
        bindingIdentifier: String,
        transport: any DayWeaveExecutionTransport,
        streamTransport: (any DayWeaveExecutionStreamTransport)? = nil
    ) {
        self.canonicalConfigurationIdentifier = canonicalConfigurationIdentifier
        self.bindingIdentifier = bindingIdentifier
        self.transport = transport
        self.streamTransport = streamTransport
    }
}

enum ExecutionSyncOutcome: Equatable, Sendable {
    case success
    case approvalRequired
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

struct DayWeaveBreakResolutionPresentation: Equatable, Sendable {
    let notificationIdentifier: String
    let observedSessionVersion: DayWeaveExecutionSessionVersion?
    let observedBreakIdentifier: String?
}

enum DayWeaveBreakNotificationIssue: Equatable, Sendable {
    case authorizationUnavailable
    case schedulingUnavailable
    case cancellationUnavailable

    var message: String {
        switch self {
        case .authorizationUnavailable:
            "Break reminder permission could not be checked. Retry when Notification Center is available."
        case .schedulingUnavailable:
            "The break reminder could not be scheduled. Retry before the break ends."
        case .cancellationUnavailable:
            "DayWeave could not verify removal of its break reminder. Encrypted execution data and credentials were preserved."
        }
    }

    var retryTitle: String {
        switch self {
        case .authorizationUnavailable: "Retry permission"
        case .schedulingUnavailable, .cancellationUnavailable: "Retry reminder check"
        }
    }
}

enum DayWeaveBreakNotificationTapIssue: Equatable, Sendable {
    case staleReminder

    var message: String {
        "That reminder no longer matches the current break. Review the current break separately."
    }
}

/// Serializes every execution transition around an encrypted byte-for-byte
/// request fence. The server lease is the only authoritative active timer.
@MainActor
final class ExecutionSyncStore: ObservableObject {
    static let maximumHistoryPages = 1_000
    static let maximumStableReadAttempts = 2

    @Published private(set) var status: ExecutionSyncStatus
    @Published private(set) var isSyncing = false
    @Published private(set) var breakResolutionPresentation:
        DayWeaveBreakResolutionPresentation? = nil
    @Published private(set) var breakNotificationAuthorizationState:
        DayWeaveNotificationAuthorizationState = .notDetermined
    @Published private(set) var breakNotificationIssue:
        DayWeaveBreakNotificationIssue? = nil
    @Published private(set) var breakNotificationTapIssue:
        DayWeaveBreakNotificationTapIssue? = nil
    @Published private(set) var isRequestingBreakNotificationAuthorization = false
    @Published private(set) var breakResolutionWakeGeneration: UInt64 = 0
    /// Observation-only token so Start controls recompute when the private
    /// habit authority fence changes without exposing any habit payload.
    @Published private(set) var habitExecutionReadinessGeneration: UInt64 = 0
    @Published private var breakAlternativeHandoffSource:
        BreakAlternativeHandoffSource? = nil
    @Published private var selectedBreakAlternativeBlockID: UUID? = nil

    private let planner: PlannerStore
    private let habitCompositionProvider: (any HabitCompositionCheckpointProviding)?
    private let connectionProvider: @MainActor @Sendable () throws -> DayWeaveExecutionConnection
    private let now: @Sendable () -> Date
    private let makeUUID: @Sendable () -> UUID
    private let breakNotificationCoordinator: any DayWeaveBreakNotificationCoordinating
    private let breakDeadlineSleep: @Sendable (Duration) async -> Void
    private let executionStreamSleep: @Sendable (Duration) async throws -> Void
    private var operationID: UUID?
    private var configurationGeneration: UInt64 = 0
    private var foregroundPollingTask: Task<Void, Never>?
    private var foregroundStreamTask: Task<Void, Never>?
    private var foregroundStreamDrainTask: Task<Void, Never>?
    private var foregroundStreamGeneration: UInt64 = 0
    private var foregroundStreamConnectionGeneration: UInt64 = 0
    private var foregroundStreamRefreshAttemptedConnection: UInt64?
    private var foregroundStreamHighWaterRevision: UInt64?
    private var foregroundStreamUnavailableForActivation = false
    private var breakDeadlineWakeTask: Task<Void, Never>?
    private var scheduledBreakDeadlineIdentifier: String?
    private var breakNotificationReconciliationIsSuppressed = false
    private var deferredPublicationCoordinator: (@MainActor @Sendable () async -> Bool)?

    init(
        planner: PlannerStore,
        habitCompositionProvider: any HabitCompositionCheckpointProviding,
        configurationStore: any SuggestionAPIConfigurationStoring =
            UserDefaultsSuggestionAPIConfigurationStore(),
        tokenStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        authCoordinator: DurableAuthCoordinator? = nil,
        session: URLSession = makeDayWeaveEphemeralSession(),
        now: @escaping @Sendable () -> Date = Date.init,
        makeUUID: @escaping @Sendable () -> UUID = UUID.init,
        breakNotificationCoordinator: any DayWeaveBreakNotificationCoordinating =
            DayWeaveBreakNotificationCoordinator(),
        breakDeadlineSleep: @escaping @Sendable (Duration) async -> Void = { duration in
            try? await Task.sleep(for: duration)
        },
        executionStreamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        }
    ) {
        self.planner = planner
        self.habitCompositionProvider = habitCompositionProvider
        self.now = now
        self.makeUUID = makeUUID
        self.breakNotificationCoordinator = breakNotificationCoordinator
        self.breakDeadlineSleep = breakDeadlineSleep
        self.executionStreamSleep = executionStreamSleep
        connectionProvider = {
            guard let configuredURL = configurationStore.loadBaseURL() else {
                throw ExecutionSyncControllerError.notConfigured
            }
            let baseURL = try DayWeaveAPIBaseURL(configuredURL)
            if let authCoordinator {
                let bindingIdentifier = try authCoordinator.bindingIdentifier(boundTo: baseURL)
                let client = DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: session,
                    authCoordinator: authCoordinator
                )
                return DayWeaveExecutionConnection(
                    canonicalConfigurationIdentifier: client.configurationIdentifier,
                    bindingIdentifier: bindingIdentifier,
                    transport: client,
                    streamTransport: client
                )
            }
            guard let token = try tokenStore.loadToken(boundTo: baseURL), !token.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            let tokenDigest = SHA256.hash(data: Data(token.utf8))
                .map { String(format: "%02x", $0) }
                .joined()
            let client = DayWeaveAPIClient(
                baseURL: baseURL,
                session: session,
                bearerToken: token
            )
            return DayWeaveExecutionConnection(
                canonicalConfigurationIdentifier: client.configurationIdentifier,
                bindingIdentifier: "execution-v1:\(baseURL.canonicalConfigurationIdentifier):\(tokenDigest)",
                transport: client,
                streamTransport: client
            )
        }
        status = .init(phase: .ready, message: "Ready to reconcile cross-device execution.")
        observeHabitCompositionReadiness()
        scheduleBreakDeadlineWakeIfNeeded()
    }

    init(
        planner: PlannerStore,
        habitCompositionProvider: (any HabitCompositionCheckpointProviding)? = nil,
        connectionProvider: @escaping @MainActor @Sendable () throws
            -> DayWeaveExecutionConnection,
        now: @escaping @Sendable () -> Date = Date.init,
        makeUUID: @escaping @Sendable () -> UUID = UUID.init,
        breakNotificationCoordinator: any DayWeaveBreakNotificationCoordinating =
            DayWeaveNoopBreakNotificationCoordinator(),
        breakDeadlineSleep: @escaping @Sendable (Duration) async -> Void = { duration in
            try? await Task.sleep(for: duration)
        },
        executionStreamSleep: @escaping @Sendable (Duration) async throws -> Void = { duration in
            try await Task.sleep(for: duration)
        }
    ) {
        self.planner = planner
        self.habitCompositionProvider = habitCompositionProvider
        self.connectionProvider = connectionProvider
        self.now = now
        self.makeUUID = makeUUID
        self.breakNotificationCoordinator = breakNotificationCoordinator
        self.breakDeadlineSleep = breakDeadlineSleep
        self.executionStreamSleep = executionStreamSleep
        status = .init(phase: .ready, message: "Ready to reconcile cross-device execution.")
        observeHabitCompositionReadiness()
        scheduleBreakDeadlineWakeIfNeeded()
    }

    var activeSession: DayWeaveExecutionSession? { planner.executionState.activeSession }

    /// Production construction always injects the encrypted habit authority.
    /// A nil provider exists only as an explicit unit-test seam for legacy
    /// execution tests; an injected unreadable/incomplete provider fails closed.
    var habitExecutionStartIsBlocked: Bool {
        guard let habitCompositionProvider else { return false }
        guard let configurationIdentifier = planner.canonicalConfigurationIdentifier else {
            return true
        }
        let activeHabitRevisions = Dictionary(uniqueKeysWithValues:
            planner.canonicalItems.compactMap { item in
                item.kind == .habit && item.deletedAt == nil
                    ? (item.id, item.revision) : nil
            }
        )
        return !habitCompositionProvider.habitCompositionCheckpoint.isAuthoritative(
            for: configurationIdentifier,
            activeHabitRevisions: activeHabitRevisions
        )
    }

    private func observeHabitCompositionReadiness() {
        habitCompositionProvider?.observeHabitCompositionCheckpointChanges { [weak self] in
            self?.habitExecutionReadinessGeneration &+= 1
        }
    }

    var breakAlternativePresentation: BreakAlternativePresentation? {
        guard let source = breakAlternativeHandoffSource else { return nil }
        return BreakAlternativePolicy.presentation(
            source: source,
            selectedCandidateID: selectedBreakAlternativeBlockID,
            planner: planner
        )
    }

    var expiredBreakChoiceRequired: Bool {
        guard let active = planner.executionState.activeSession,
              active.status == .paused,
              let pauseUntil = active.pauseUntil,
              pauseUntil <= now() else { return false }
        return planner.executionState.acknowledgedExpiredPause
            != .init(sessionID: active.id, revision: active.revision)
    }

    var expiredBreakResolutionShouldBePresented: Bool {
        shouldPresentExpiredBreakResolution(pendingNotificationIdentifier: nil)
    }

    func shouldPresentExpiredBreakResolution(
        pendingNotificationIdentifier: String?
    ) -> Bool {
        if let pendingNotificationIdentifier,
           DayWeaveBreakNotificationContract.owns(
               identifier: pendingNotificationIdentifier
           ) {
            // The process-lifetime router installs the exact store token before
            // clearing its mailbox. Suppress this initial render so a closed or
            // newly unlocked window cannot briefly present current break B while
            // notification response A is still waiting to be revalidated.
            return false
        }
        guard expiredBreakChoiceRequired,
              let active = planner.executionState.activeSession,
              let descriptor = DayWeaveBreakNotificationContract.descriptor(for: active) else {
            return false
        }
        guard let presentation = breakResolutionPresentation else {
            // No notification response is driving this render, so the ordinary
            // in-app clock/state path presents the current expired break.
            return true
        }
        guard presentation.observedBreakIdentifier == descriptor.identifier else {
            // A tap observed no expired break or an older exact digest. It
            // cannot suppress a later deadline or independently expired lease,
            // even if a malformed peer reused the same numeric revision.
            return true
        }
        return presentation.notificationIdentifier == descriptor.identifier
    }

    var credentialReplacementIsBlocked: Bool {
        planner.hasExecutionCredentialReplacementBlocker
    }

    var hasFutureTimedBreakForNotificationPermission: Bool {
        DayWeaveBreakNotificationContract.request(
            for: planner.executionState.activeSession,
            acknowledged: planner.executionState.acknowledgedExpiredPause,
            now: now()
        ) != nil
    }

    var breakNotificationBannerShouldBePresented: Bool {
        breakNotificationIssue != nil
            || (hasFutureTimedBreakForNotificationPermission
                && breakNotificationAuthorizationState != .authorized)
    }

    @discardableResult
    func reconcileBreakNotification() async -> DayWeaveBreakNotificationReconcileResult {
        scheduleBreakDeadlineWakeIfNeeded()
        let session = breakNotificationReconciliationIsSuppressed
            ? nil : planner.executionState.activeSession
        let acknowledged = breakNotificationReconciliationIsSuppressed
            ? nil : planner.executionState.acknowledgedExpiredPause
        let result = await breakNotificationCoordinator.reconcile(
            session: session,
            acknowledged: acknowledged
        )
        breakNotificationAuthorizationState =
            await breakNotificationCoordinator.authorizationState()
        applyBreakNotificationResult(result)
        clearBreakResolutionPresentationIfStale()
        scheduleBreakDeadlineWakeIfNeeded()
        return result
    }

    /// The system prompt is reachable only from an explicit user action. An
    /// automatic launch, refresh, or remote pause reconciliation never calls
    /// this method and therefore cannot hold execution UI behind a modal OS
    /// permission sheet.
    @discardableResult
    func requestBreakNotificationAuthorization()
        async -> DayWeaveBreakNotificationReconcileResult
    {
        guard hasFutureTimedBreakForNotificationPermission,
              !isRequestingBreakNotificationAuthorization else { return .superseded }
        isRequestingBreakNotificationAuthorization = true
        defer { isRequestingBreakNotificationAuthorization = false }
        let result = await breakNotificationCoordinator.requestAuthorization()
        switch result {
        case .authorized:
            breakNotificationAuthorizationState = .authorized
            return await reconcileBreakNotification()
        case .denied:
            breakNotificationAuthorizationState = .denied
            breakNotificationIssue = nil
            return .permissionDenied
        case .unavailable:
            breakNotificationAuthorizationState =
                await breakNotificationCoordinator.authorizationState()
            breakNotificationIssue = .authorizationUnavailable
            return .unavailable
        }
    }

    @discardableResult
    func retryBreakNotification() async -> DayWeaveBreakNotificationReconcileResult {
        if breakNotificationIssue == .cancellationUnavailable {
            // Removing an owned request does not require alert authorization.
            // A denied state must not erase the visible convergence failure
            // without actually retrying the pending-and-delivered barrier.
            return await reconcileBreakNotification()
        }
        switch breakNotificationAuthorizationState {
        case .notDetermined:
            return await requestBreakNotificationAuthorization()
        case .authorized:
            return await reconcileBreakNotification()
        case .denied:
            breakNotificationIssue = nil
            return .permissionDenied
        }
    }

    @discardableResult
    func routeBreakNotificationTap(identifier: String) -> Bool {
        guard DayWeaveBreakNotificationContract.owns(identifier: identifier) else {
            return false
        }
        let observedSession = expiredBreakChoiceRequired
            ? planner.executionState.activeSession : nil
        let observedVersion = observedSession.map {
            DayWeaveExecutionSessionVersion(sessionID: $0.id, revision: $0.revision)
        }
        let observedBreakIdentifier = observedSession.flatMap {
            DayWeaveBreakNotificationContract.descriptor(for: $0)?.identifier
        }
        // Retain the clicked opaque digest even when it is stale. The alert
        // binding can then distinguish a rejected A response from the current B
        // generation instead of retargeting the click to B. With no tap state,
        // ordinary clock-driven expiration remains unchanged.
        breakResolutionPresentation = .init(
            notificationIdentifier: identifier,
            observedSessionVersion: observedVersion,
            observedBreakIdentifier: observedBreakIdentifier
        )
        let accepted = DayWeaveBreakNotificationContract.tapMatchesUnresolvedBreak(
            identifier: identifier,
            session: planner.executionState.activeSession,
            acknowledged: planner.executionState.acknowledgedExpiredPause,
            now: now()
        )
        breakNotificationTapIssue = accepted ? nil : .staleReminder
        return accepted
    }

    /// A stale notification response is never silently retargeted. This
    /// explicit acknowledgement clears its exact suppression token; only then
    /// may the ordinary current-break resolver present independently.
    func acknowledgeStaleBreakNotificationTap() {
        breakResolutionPresentation = nil
        breakNotificationTapIssue = nil
        breakResolutionWakeGeneration &+= 1
        scheduleBreakDeadlineWakeIfNeeded()
    }

    /// Recomputes the presentation from current proof rather than retaining a
    /// schedule snapshot. A disappeared candidate also clears the ordinary
    /// Today selection when that selection came from this handoff.
    func reconcileBreakAlternativeSelection() {
        guard let source = breakAlternativeHandoffSource else {
            selectedBreakAlternativeBlockID = nil
            return
        }
        guard let presentation = BreakAlternativePolicy.presentation(
            source: source,
            selectedCandidateID: selectedBreakAlternativeBlockID,
            planner: planner
        ) else {
            let priorSelection = selectedBreakAlternativeBlockID
            breakAlternativeHandoffSource = nil
            selectedBreakAlternativeBlockID = nil
            if planner.selectedBlockID == priorSelection {
                planner.selectedBlockID = nil
            }
            return
        }
        guard selectedBreakAlternativeBlockID != presentation.selectedCandidateID else {
            return
        }
        let priorSelection = selectedBreakAlternativeBlockID
        selectedBreakAlternativeBlockID = presentation.selectedCandidateID
        if planner.selectedBlockID == priorSelection {
            planner.selectedBlockID = nil
        }
    }

    /// Presentation-only selection. This never enters the execution command
    /// path and therefore cannot start, resume, finish, skip, defer, or publish
    /// anything.
    func selectBreakAlternative(_ blockID: UUID) {
        guard let presentation = breakAlternativePresentation,
              presentation.candidates.contains(where: { $0.id == blockID }),
              let block = planner.blocks.first(where: { $0.id == blockID }) else {
            reconcileBreakAlternativeSelection()
            return
        }
        selectedBreakAlternativeBlockID = blockID
        planner.destination = .today
        planner.select(block)
    }

    /// Called from the privacy-safe process-local foreground-delivery pulse.
    /// The pulse contains no request identifier or planner content.
    func breakNotificationForegroundDeliveryDidOccur() {
        clearBreakResolutionPresentationIfStale()
        breakResolutionWakeGeneration &+= 1
        scheduleBreakDeadlineWakeIfNeeded()
    }

    func installDeferredPublicationCoordinator(
        _ coordinator: @escaping @MainActor @Sendable () async -> Bool
    ) {
        deferredPublicationCoordinator = coordinator
    }

    func configurationDidChange() async {
        configurationGeneration &+= 1
        stopForegroundPolling()
        status = .init(
            phase: .ready,
            message: planner.executionState.pendingCommand == nil
                ? "API settings changed; execution must be reconciled again."
                : "API settings changed; the exact pending command remains fenced."
        )
        await cancelBreakNotificationsForConfigurationReset()
    }

    func prepareForCredentialReplacement() async throws {
        guard operationID == nil else {
            throw PlannerExecutionStateError.credentialReplacementBlocked
        }
        let transitionID = UUID()
        operationID = transitionID
        isSyncing = true
        stopForegroundPolling()
        breakNotificationReconciliationIsSuppressed = true
        var cancellationConverged = false
        do {
            // Cancellation is the awaited precondition to destroying the only
            // encrypted state capable of identifying this notification. This
            // closes the crash window where a surviving request could outlive
            // its authoritative lease cache.
            let cancellation = await cancelBreakNotificationsForConfigurationReset()
            guard cancellation.isVerifiedCancellation else {
                throw PlannerExecutionStateError.breakNotificationCancellationUnavailable
            }
            cancellationConverged = true
            try planner.prepareForExecutionCredentialReplacement()
            configurationGeneration &+= 1
            breakNotificationReconciliationIsSuppressed = false
            operationID = nil
            isSyncing = false
        } catch {
            breakNotificationReconciliationIsSuppressed = false
            // If local quarantine was refused, restore notification state from
            // whichever authoritative lease remains current.
            if cancellationConverged {
                _ = await reconcileBreakNotification()
            }
            if operationID == transitionID {
                operationID = nil
                isSyncing = false
            }
            throw error
        }
    }

    /// Called before an explicit local canonical-cache reset. The caller must
    /// await this barrier before erasing the encrypted authoritative session so
    /// an old pending or delivered request cannot survive the reset.
    @discardableResult
    func cancelBreakNotificationsForConfigurationReset()
        async -> DayWeaveBreakNotificationReconcileResult
    {
        let wasSuppressed = breakNotificationReconciliationIsSuppressed
        breakNotificationReconciliationIsSuppressed = true
        let result = await breakNotificationCoordinator.reconcile(
            session: nil,
            acknowledged: nil
        )
        if result.isVerifiedCancellation {
            breakResolutionPresentation = nil
            breakNotificationTapIssue = nil
            breakDeadlineWakeTask?.cancel()
            breakDeadlineWakeTask = nil
            scheduledBreakDeadlineIdentifier = nil
        }
        breakNotificationAuthorizationState =
            await breakNotificationCoordinator.authorizationState()
        applyBreakNotificationResult(result)
        if !result.isVerifiedCancellation {
            breakNotificationIssue = .cancellationUnavailable
        }
        breakNotificationReconciliationIsSuppressed = wasSuppressed
        if wasSuppressed == false {
            scheduleBreakDeadlineWakeIfNeeded()
        }
        return result
    }

    func refresh() async -> ExecutionSyncOutcome {
        let recoveredPending = planner.executionState.pendingCommand
        let outcome = await refreshExecutionOnly()
        guard outcome == .success else { return outcome }
        if let recoveredPending, case .deferWork = recoveredPending.command {
            let recovered = planner.executionState.terminalOutcomes[
                recoveredPending.identity.sessionID
            ]?.session
            guard recovered.map({
                recoveredDeferIsExact(recoveredPending, session: $0)
            }) == true else {
                _ = await resumePendingDeferIntentIfNeeded()
                return .conflict
            }
        }
        return await resumePendingDeferIntentIfNeeded()
    }

    private func refreshExecutionOnly() async -> ExecutionSyncOutcome {
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
            guard let sessionIndex = block.sessionIndex else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "This block has no server-issued session index. Sync and publish the schedule again."
                )
            }
            if let issue = self.planner.canonicalScheduleBlockActionabilityIssue(block) {
                throw ExecutionSyncControllerError.invalidLocalState(issue)
            }
            guard self.planner.canonicalAuthoringMutation(itemID: itemID) == nil else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "Sync or resolve this item's queued edit before starting it."
                )
            }
            // This closure runs on MainActor immediately before the durable
            // execution journal is built, with no suspension point between the
            // authority check and persistence. Later/Skip command paths do not
            // use this fence and remain independently available.
            guard !self.habitExecutionStartIsBlocked else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "Habit progress is not fully synchronized. Wait for habit recovery before starting a new session."
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
                sessionIndex: sessionIndex,
                plannedBlockID: block.id,
                sourceDeviceID: deviceID
            )
            return .init(
                command: .start(
                    sessionID: sessionID,
                    itemID: itemID,
                    itemRevision: itemRevision,
                    occurrenceID: block.occurrenceID,
                    sessionIndex: sessionIndex,
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

    /// Moves the unfinished part of an authoritative lease. Running work is
    /// paused first so the server gives us one exact accumulated-second value;
    /// that same value is then fenced into both the corrected actual and the
    /// replacement-window duration.
    func deferWork(
        _ blockID: UUID,
        moveStart requestedMoveStart: Date,
        approvedMoveEnd requestedApprovedMoveEnd: Date? = nil,
        latestFinish: Date? = nil,
        deadlineBoundary: DayWeaveMoveDeadlineBoundary? = nil,
        deadlineIdentities requestedDeadlineIdentities: Set<DayWeaveMoveDeadlineIdentity>? = nil,
        allowDeadlineConflict: Bool = false,
        approvedFixedConflicts: Set<DayWeaveMoveConflictIdentity>? = nil,
        allowFixedConflicts: Bool = false,
        allowSourceOverride: Bool = false
    ) async -> ExecutionSyncOutcome {
        // The compatibility arguments above remain source-compatible for one
        // release, but they authorize only non-execution schedule moves. An
        // open execution lease is governed exclusively by the server assessment
        // below; local deadline/overlap interpretation is never sent as proof.
        _ = requestedApprovedMoveEnd
        _ = latestFinish
        _ = deadlineBoundary
        _ = requestedDeadlineIdentities
        _ = allowDeadlineConflict
        _ = approvedFixedConflicts
        _ = allowFixedConflicts
        _ = allowSourceOverride

        if let recoveredPending = planner.executionState.pendingCommand {
            let recovery = await refreshExecutionOnly()
            guard recovery == .success else { return recovery }
            if case let .deferWork(
                _, recoveredMoveStart, _, _, _, _
            ) = recoveredPending.command {
                guard recoveredPending.focusedBlockID == blockID,
                      recoveredMoveStart == requestedMoveStart,
                      let recovered = planner.executionState.terminalOutcomes[
                        recoveredPending.identity.sessionID
                      ]?.session,
                      recoveredDeferIsExact(
                        recoveredPending,
                        session: recovered
                      ) else {
                    return .conflict
                }
                if let intent = planner.pendingExecutionDeferIntent,
                   intent.identity == recoveredPending.identity {
                    guard exactDeferredClosure(intent: intent, session: recovered) else {
                        return clearInvalidDeferIntent(intent)
                    }
                    do {
                        try planner.clearExecutionDeferIntent(
                            intent,
                            message: "Recovered the exact move; publishing its replacement placement"
                        )
                    } catch {
                        return report(error)
                    }
                }
                return .success
            }
        }
        guard let block = planner.blocks.first(where: { $0.id == blockID }),
              let open = planner.executionState.activeSession,
              executionSession(open, matches: block) else { return .invalidLocalState }
        let selectedAt = now()
        let existingIntent = planner.pendingExecutionDeferIntent
        let existingRequestMatches = existingIntent.map {
            deferRequest(
                $0,
                matches: open,
                block: block,
                moveStart: requestedMoveStart,
                at: selectedAt
            )
        } == true
        let existingAssessmentIsFresh = existingIntent.flatMap(\.assessment).map {
            $0.expiresAt > selectedAt
                && assessmentMatchesCurrentPausedState(
                    $0,
                    session: open,
                    block: block
                )
        } == true
        let newTargetHasSafetyMargin = DayWeaveExecutionDeferTiming.isValidNewMoveStart(
            requestedMoveStart,
            now: selectedAt
        )
        guard dayWeaveExactWholeSecondDelta(from: block.start, to: block.end)
                .map({ $0 <= 86_400 }) == true,
              requestedMoveStart > selectedAt else {
            return .invalidLocalState
        }
        guard newTargetHasSafetyMargin
                || (existingRequestMatches && existingAssessmentIsFresh) else {
            if existingRequestMatches, let existingIntent {
                do {
                    try planner.cancelExecutionDeferIntent(
                        existingIntent,
                        message: "The saved move no longer leaves enough time for a fresh assessment; execution remains paused"
                    )
                } catch {
                    return report(error)
                }
            }
            return .invalidLocalState
        }
        if let existing = planner.pendingExecutionDeferIntent,
           !deferRequest(
               existing,
               matches: open,
               block: block,
               moveStart: requestedMoveStart,
               at: selectedAt
           ) {
            do {
                try planner.clearExecutionDeferIntent(
                    existing,
                    message: "The selected execution move changed; prior assessment evidence was cleared"
                )
            } catch {
                return report(error)
            }
        }
        if planner.pendingExecutionDeferIntent == nil {
            let createdAt = now()
            let staged = DayWeavePendingExecutionDeferIntent(
                identity: .init(session: open),
                focusedBlockID: blockID,
                sourceStart: block.start,
                sourceEnd: block.end,
                moveStart: requestedMoveStart,
                approvedMoveEnd: requestedMoveStart,
                approvedDeadlines: [],
                deadlineConflictApproved: false,
                approvedFixedConflicts: [],
                fixedConflictApproved: false,
                sourceOverrideApproved: false,
                assessment: nil,
                approvedAssessmentDigest: nil,
                createdAt: createdAt,
                expiresAt: requestedMoveStart
            )
            do {
                try planner.persistExecutionDeferIntent(staged)
            } catch {
                return report(error)
            }
        }

        if open.status == .active {
            for attempt in 0..<2 {
                let pauseOutcome = await pause(blockID)
                guard pauseOutcome == .success else { return pauseOutcome }
                if planner.executionState.activeSession?.status == .paused { break }
                guard attempt == 0,
                      let stillActive = planner.executionState.activeSession,
                      stillActive.status == .active,
                      let currentBlock = planner.blocks.first(where: { $0.id == blockID }),
                      executionSession(stillActive, matches: currentBlock) else {
                    return .conflict
                }
            }
        }
        guard let paused = planner.executionState.activeSession,
              paused.status == .paused,
              let currentBlock = planner.blocks.first(where: { $0.id == blockID }),
              executionSession(paused, matches: currentBlock) else { return .conflict }
        guard let saved = planner.pendingExecutionDeferIntent,
              deferRequest(
                  saved,
                  matches: paused,
                  block: currentBlock,
                  moveStart: requestedMoveStart
              ) else { return .conflict }

        for attempt in 0..<2 {
            let assessmentObservedAt = now()
            if let assessment = planner.pendingExecutionDeferIntent?.assessment,
               !assessmentMatchesCurrentPausedState(
                   assessment,
                   session: paused,
                   block: currentBlock
               ) || assessment.expiresAt <= assessmentObservedAt {
                guard clearDeferAssessmentEvidence() else { return .localStorageFailure }
            }
            if planner.pendingExecutionDeferIntent?.assessment == nil {
                let assessmentRequestAt = now()
                if !DayWeaveExecutionDeferTiming.isValidNewMoveStart(
                       requestedMoveStart,
                       now: assessmentRequestAt
                   ),
                   let staleIntent = planner.pendingExecutionDeferIntent {
                    do {
                        try planner.cancelExecutionDeferIntent(
                            staleIntent,
                            message: "Not enough assessment time remains before the selected target; choose a later time while execution stays paused"
                        )
                    } catch {
                        return report(error)
                    }
                    return .invalidLocalState
                }
                let assessmentOutcome = await assessDeferredWork(blockID)
                guard assessmentOutcome == .success else { return assessmentOutcome }
            }
            guard let assessedIntent = planner.pendingExecutionDeferIntent,
                  let assessment = assessedIntent.assessment else {
                return .protocolFailure
            }
            if assessment.approvalRequired,
               assessedIntent.approvedAssessmentDigest == nil {
                setConnected("Move assessed · review the content-free scheduling conflicts")
                return .approvalRequired
            }

            var expectedCommand: DayWeaveExecutionCommand?
            let outcome = await command(blockID: blockID) { snapshot, block, _ in
                let source = try self.requireActiveSession(snapshot, matching: block)
                guard source.status == .paused,
                      let currentIntent = self.planner.pendingExecutionDeferIntent,
                      currentIntent.isSameRequest(as: assessedIntent),
                      let currentAssessment = currentIntent.assessment,
                      currentAssessment == assessment,
                      self.assessmentMatches(
                          currentAssessment,
                          snapshot: snapshot,
                          session: source,
                          block: block
                      ),
                      currentAssessment.expiresAt > self.now(),
                      currentIntent.approvedAssessmentDigest
                        == (currentAssessment.approvalRequired
                            ? currentAssessment.assessmentDigest : nil) else {
                    throw ExecutionSyncControllerError.invalidLocalState(
                        "The saved defer assessment is stale; the session remains paused."
                    )
                }
                let command = DayWeaveExecutionCommand.deferWork(
                    sessionID: source.id,
                    moveStart: currentAssessment.moveStart,
                    moveEnd: currentAssessment.moveEnd,
                    actualSeconds: currentAssessment.actualSeconds,
                    assessmentDigest: currentAssessment.assessmentDigest,
                    approvedAssessmentDigest: currentIntent.approvedAssessmentDigest
                )
                expectedCommand = command
                return .init(
                    command: command,
                    identity: .init(session: source),
                    priorSession: source,
                    focusedBlockID: block.id,
                    projectionEligibleAtStart: false
                )
            }
            if outcome == .success,
               let expectedCommand,
               let deferred = planner.executionState.terminalOutcomes[paused.id]?.session,
               expectedCommand.matchesChangedSession(deferred) {
                do {
                    if let retained = planner.pendingExecutionDeferIntent {
                        try planner.clearExecutionDeferIntent(
                            retained,
                            message: "Move saved · publishing the exact assessed replacement placement"
                        )
                    }
                } catch {
                    return report(error)
                }
                return .success
            }
            guard attempt == 0,
                  planner.executionState.pendingCommand == nil,
                  [
                    .success,
                    .conflict,
                    .validationFailure,
                    .invalidLocalState,
                  ].contains(outcome),
                  planner.executionState.activeSession?.status == .paused,
                  clearDeferAssessmentEvidence() else {
                return outcome == .success ? .protocolFailure : outcome
            }
        }
        return .conflict
    }

    func approveDeferredWork(
        _ blockID: UUID,
        assessmentDigest: String
    ) async -> ExecutionSyncOutcome {
        guard let intent = planner.pendingExecutionDeferIntent else {
            return .invalidLocalState
        }
        guard planner.executionState.pendingCommand == nil,
              intent.focusedBlockID == blockID,
              let assessment = intent.assessment,
              assessment.expiresAt > now(),
              let approved = intent.approvingAssessment(digest: assessmentDigest),
              let active = planner.executionState.activeSession,
              active.status == .paused,
              let block = planner.blocks.first(where: { $0.id == blockID }),
              deferRequest(approved, matches: active, block: block, moveStart: intent.moveStart),
              assessmentMatchesCurrentPausedState(assessment, session: active, block: block) else {
            if planner.pendingExecutionDeferIntent?.assessment != nil {
                guard clearDeferAssessmentEvidence() else { return .localStorageFailure }
            }
            return await deferWork(blockID, moveStart: intent.moveStart)
        }
        do {
            // The approval is durably committed before the exact command body is
            // constructed. A crash at either boundary restores this digest or
            // the immutable command journal, never an in-memory approval.
            try planner.persistExecutionDeferIntent(approved)
        } catch {
            return report(error)
        }
        return await deferWork(blockID, moveStart: approved.moveStart)
    }

    /// Cancels only the exact, still-unsent Pause -> Defer intent selected by
    /// the caller. Once command bytes are journaled they remain replayable and
    /// cannot be discarded through this UI path.
    func cancelDeferredWork(
        _ expectedIntent: DayWeavePendingExecutionDeferIntent
    ) -> ExecutionSyncOutcome {
        guard planner.executionState.pendingCommand == nil,
              planner.pendingExecutionDeferIntent == expectedIntent else {
            return .invalidLocalState
        }
        do {
            try planner.cancelExecutionDeferIntent(
                expectedIntent,
                message: "Move canceled; the exact execution session remains paused"
            )
            setConnected("Move canceled · execution remains paused")
            return .success
        } catch {
            return report(error)
        }
    }

    var pendingDeferApproval: DayWeaveDeferAssessment? {
        guard let intent = planner.pendingExecutionDeferIntent,
              intent.approvalIsRequired,
              let assessment = intent.assessment,
              assessment.expiresAt > now() else { return nil }
        return assessment
    }

    private func assessDeferredWork(_ blockID: UUID) async -> ExecutionSyncOutcome {
        await runExclusive { [self] operationID, generation in
            let connection = try configuredConnection()
            try prepareLocalState(for: connection)
            setBusy("Assessing the exact paused work placement…")

            for attempt in 0..<2 {
                guard planner.executionState.pendingCommand == nil,
                      planner.executionState.historyVerified,
                      let intent = planner.pendingExecutionDeferIntent,
                      let block = planner.blocks.first(where: { $0.id == blockID }),
                      let paused = planner.executionState.activeSession,
                      paused.status == .paused,
                      deferRequest(
                          intent,
                          matches: paused,
                          block: block,
                          moveStart: intent.moveStart
                      ) else {
                    throw ExecutionSyncControllerError.invalidLocalState(
                        "The exact paused execution is unavailable for assessment."
                    )
                }
                let request = DayWeaveDeferAssessmentRequest(
                    expectedRevision: planner.executionState.revision,
                    sessionID: paused.id,
                    moveStart: intent.moveStart,
                    actualSeconds: paused.accumulatedSeconds
                )
                do {
                    let assessment = try await connection.transport.assessExecutionDefer(request)
                    try ensureCurrent(
                        connection,
                        operationID: operationID,
                        generation: generation
                    )
                    guard assessmentMatches(
                        assessment,
                        snapshot: .init(
                            revision: planner.executionState.revision,
                            activeSession: planner.executionState.activeSession
                        ),
                        session: paused,
                        block: block
                    ),
                    planner.pendingExecutionDeferIntent?.isSameRequest(as: intent) == true else {
                        throw ExecutionSyncControllerError.invalidProtocol
                    }
                    // Replacing the evidence clears approval by construction, so
                    // a newly issued digest can never inherit the old consent.
                    try planner.persistExecutionDeferIntent(
                        intent.replacingAssessment(assessment)
                    )
                    setConnected(
                        assessment.approvalRequired
                            ? "Move assessed · explicit approval is required"
                            : "Move assessed · no override is required"
                    )
                    return .success
                } catch let error as DayWeaveAPIError {
                    guard attempt == 0, deferAssessmentCanBeRetried(after: error) else {
                        if deferAssessmentCanBeRetried(after: error) {
                            _ = clearDeferAssessmentEvidence()
                            setConnected("Assessment changed; the exact session remains paused")
                            return .conflict
                        }
                        throw error
                    }
                    _ = clearDeferAssessmentEvidence()
                    try markHistoryUnverified(binding: connection.bindingIdentifier)
                    let stable = try await readStableHistory(
                        connection: connection,
                        initialSnapshot: nil,
                        operationID: operationID,
                        generation: generation
                    )
                    try persist(
                        stable: stable,
                        binding: connection.bindingIdentifier,
                        pending: nil
                    )
                }
            }
            return .conflict
        }
    }

    private func deferAssessmentCanBeRetried(after error: DayWeaveAPIError) -> Bool {
        guard case let .server(statusCode, code, _, _) = error else { return false }
        return statusCode == 409
            || [
                "execution_defer_assessment_stale",
                "execution_schedule_stale",
                "execution_defer_requires_pause",
            ].contains(code ?? "")
    }

    private func deferRequest(
        _ intent: DayWeavePendingExecutionDeferIntent,
        matches session: DayWeaveExecutionSession,
        block: ScheduleBlock,
        moveStart: Date,
        at referenceDate: Date? = nil
    ) -> Bool {
        let referenceDate = referenceDate ?? now()
        return intent.hasValidShape
            && intent.moveStart > referenceDate
            && intent.identity.matches(session)
            && intent.focusedBlockID == block.id
            && intent.moveStart == moveStart
            && sourceBlockMatchesIntent(block, intent: intent)
    }

    private func assessmentMatchesCurrentPausedState(
        _ assessment: DayWeaveDeferAssessment,
        session: DayWeaveExecutionSession,
        block: ScheduleBlock
    ) -> Bool {
        assessmentMatches(
            assessment,
            snapshot: .init(
                revision: planner.executionState.revision,
                activeSession: planner.executionState.activeSession
            ),
            session: session,
            block: block
        )
    }

    private func assessmentMatches(
        _ assessment: DayWeaveDeferAssessment,
        snapshot: DayWeaveExecutionSnapshot,
        session: DayWeaveExecutionSession,
        block: ScheduleBlock
    ) -> Bool {
        assessment.hasValidShape
            && session.status == .paused
            && snapshot.revision == assessment.executionRevision
            && snapshot.activeSession == session
            && session.id == assessment.sessionID
            && session.revision == assessment.sessionRevision
            && session.itemID == assessment.itemID
            && session.itemRevision == assessment.itemRevision
            && session.occurrenceID == assessment.occurrenceID
            && session.sessionIndex == assessment.sourceSessionIndex
            && session.accumulatedSeconds == assessment.actualSeconds
            && session.plannedBlockID == assessment.sourceBlockID
            && block.id == assessment.sourceBlockID
            && block.sourceItemID == assessment.itemID
            && block.sourceItemRevision == assessment.itemRevision
            && block.occurrenceID == assessment.occurrenceID
            && (block.sessionIndex ?? 0) == assessment.sourceSessionIndex
            && dayWeaveExactWholeSecondDelta(from: block.start, to: block.end)
                == assessment.plannedDurationSeconds
    }

    @discardableResult
    private func clearDeferAssessmentEvidence() -> Bool {
        guard let intent = planner.pendingExecutionDeferIntent else { return true }
        let cleared = intent.replacingAssessment(nil)
        if cleared == intent { return true }
        do {
            try planner.persistExecutionDeferIntent(cleared)
            return true
        } catch {
            _ = report(error)
            return false
        }
    }

    func keepPausedAfterExpiredBreak() async -> ExecutionSyncOutcome {
        await acknowledgeExpiredBreakKeepingPaused(presentAlternatives: false)
    }

    func chooseAnotherAfterExpiredBreak() async -> ExecutionSyncOutcome {
        let outcome = await acknowledgeExpiredBreakKeepingPaused(
            presentAlternatives: true
        )
        if outcome == .success { planner.destination = .today }
        return outcome
    }

    func startForegroundPolling(every interval: Duration = .seconds(30)) {
        guard foregroundPollingTask == nil else { return }
        foregroundStreamUnavailableForActivation = false
        foregroundPollingTask = Task { @MainActor [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                let outcome = await self.refreshAndCoordinateDeferredPublication()
                if outcome == .success,
                   !Task.isCancelled,
                   self.foregroundPollingTask != nil {
                    // A successful poll proves the binding installation or
                    // quarantine reached encrypted persistence before
                    // Last-Event-ID is captured. A later successful poll can
                    // retry if an earlier attempt failed before that boundary.
                    self.startForegroundExecutionStreamIfReady()
                }
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
        foregroundStreamGeneration &+= 1
        foregroundStreamTask?.cancel()
        foregroundStreamTask = nil
        foregroundStreamDrainTask?.cancel()
        foregroundStreamDrainTask = nil
        foregroundStreamRefreshAttemptedConnection = nil
        foregroundStreamHighWaterRevision = nil
        foregroundStreamUnavailableForActivation = false
    }

    private func startForegroundExecutionStreamIfReady() {
        guard foregroundStreamTask == nil,
              !foregroundStreamUnavailableForActivation,
              planner.canPersistPlan,
              planner.hasEncryptedPersistence,
              let connection = try? configuredConnection(),
              connection.streamTransport != nil,
              planner.executionState.bindingIdentifier == connection.bindingIdentifier,
              connectionIsCurrent(connection) else { return }
        foregroundStreamGeneration &+= 1
        let generation = foregroundStreamGeneration
        foregroundStreamTask = Task { @MainActor [weak self] in
            await self?.runForegroundExecutionStream(generation: generation)
        }
    }

    private func runForegroundExecutionStream(generation: UInt64) async {
        var retrySeconds = 1
        defer {
            if generation == foregroundStreamGeneration {
                foregroundStreamTask = nil
            }
        }
        while foregroundStreamIsCurrent(generation) {
            var reconnectDelaySeconds = 1
            let connection: DayWeaveExecutionConnection
            do {
                connection = try configuredConnection()
            } catch {
                // Polling remains responsible for user-visible configuration
                // and authentication status. Stream health is intentionally
                // silent and activation-scoped.
                return
            }
            guard let streamTransport = connection.streamTransport,
                  planner.executionState.bindingIdentifier == connection.bindingIdentifier,
                  connectionIsCurrent(connection) else { return }
            let durableRevision = planner.executionState.revision
            foregroundStreamConnectionGeneration &+= 1
            let connectionGeneration = foregroundStreamConnectionGeneration

            do {
                let completion = try await streamTransport.consumeExecutionInvalidations(
                    after: durableRevision
                ) { [weak self] revision in
                    await self?.acceptExecutionStreamHint(
                        revision,
                        connection: connection,
                        generation: generation,
                        connectionGeneration: connectionGeneration
                    )
                }
                guard foregroundStreamIsCurrent(generation) else { return }
                switch completion {
                case .unsupported:
                    // Do not probe again until a later foreground activation.
                    foregroundStreamUnavailableForActivation = true
                    return
                case .endOfStream:
                    // An immediate clean EOF is still a transient failure; a
                    // broken proxy must not create one request per second for
                    // the entire foreground activation.
                    reconnectDelaySeconds = retrySeconds
                    retrySeconds = min(retrySeconds * 2, 30)
                case .liveEndOfStream:
                    retrySeconds = 1
                    reconnectDelaySeconds = 1
                }
            } catch {
                guard foregroundStreamIsCurrent(generation) else { return }
                guard executionStreamFailureIsTransient(error) else {
                    foregroundStreamUnavailableForActivation = true
                    return
                }
                reconnectDelaySeconds = retrySeconds
                retrySeconds = min(retrySeconds * 2, 30)
            }

            do {
                try await executionStreamSleep(.seconds(reconnectDelaySeconds))
            } catch {
                return
            }
        }
    }

    private func acceptExecutionStreamHint(
        _ revision: UInt64,
        connection: DayWeaveExecutionConnection,
        generation: UInt64,
        connectionGeneration: UInt64
    ) {
        guard foregroundStreamIsCurrent(generation),
              connectionIsCurrent(connection),
              planner.executionState.bindingIdentifier == connection.bindingIdentifier,
              revision > planner.executionState.revision else { return }
        foregroundStreamHighWaterRevision = max(
            foregroundStreamHighWaterRevision ?? revision,
            revision
        )
        guard foregroundStreamDrainTask == nil,
              foregroundStreamRefreshAttemptedConnection != connectionGeneration else { return }
        foregroundStreamDrainTask = Task { @MainActor [weak self] in
            await self?.drainExecutionStreamHighWater(
                connection: connection,
                generation: generation,
                connectionGeneration: connectionGeneration
            )
        }
    }

    private func drainExecutionStreamHighWater(
        connection: DayWeaveExecutionConnection,
        generation: UInt64,
        connectionGeneration: UInt64
    ) async {
        defer {
            if generation == foregroundStreamGeneration {
                foregroundStreamDrainTask = nil
            }
        }
        while foregroundStreamIsCurrent(generation) {
            guard connectionIsCurrent(connection),
                  planner.executionState.bindingIdentifier == connection.bindingIdentifier else {
                return
            }
            guard let highWater = foregroundStreamHighWaterRevision else { return }
            if highWater <= planner.executionState.revision {
                foregroundStreamHighWaterRevision = nil
                continue
            }

            // A stream hint arriving during a user command or canonical-sync
            // lane must wait for admission instead of being discarded as a
            // duplicate operation. The normal poll remains independent.
            if operationID != nil || planner.isCanonicalSyncLocked {
                do {
                    try await executionStreamSleep(.milliseconds(25))
                } catch {
                    return
                }
                continue
            }

            foregroundStreamRefreshAttemptedConnection = connectionGeneration
            _ = await refreshAndCoordinateDeferredPublication()
            guard foregroundStreamIsCurrent(generation),
                  connectionIsCurrent(connection),
                  planner.executionState.bindingIdentifier == connection.bindingIdentifier else {
                return
            }
            if let currentHighWater = foregroundStreamHighWaterRevision,
               currentHighWater <= planner.executionState.revision {
                foregroundStreamHighWaterRevision = nil
                return
            }
            // An invalidation is an untrusted hint, not proof that this API
            // origin can ever produce the advertised revision. One coalesced
            // snapshot attempt per stream connection prevents a malicious high
            // water value from creating a tight request loop. Keep the target
            // in memory and yield durable catch-up to the unchanged poll.
            return
        }
    }

    private func foregroundStreamIsCurrent(_ generation: UInt64) -> Bool {
        !Task.isCancelled
            && foregroundPollingTask != nil
            && generation == foregroundStreamGeneration
    }

    private func connectionIsCurrent(_ connection: DayWeaveExecutionConnection) -> Bool {
        guard let current = try? configuredConnection() else { return false }
        return current.bindingIdentifier == connection.bindingIdentifier
            && current.canonicalConfigurationIdentifier
                == connection.canonicalConfigurationIdentifier
    }

    private func executionStreamFailureIsTransient(_ error: Error) -> Bool {
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

    @discardableResult
    func refreshAndCoordinateDeferredPublication() async -> ExecutionSyncOutcome {
        let outcome = await refresh()
        if let highWater = foregroundStreamHighWaterRevision,
           highWater <= planner.executionState.revision {
            foregroundStreamHighWaterRevision = nil
        }
        guard outcome == .success,
              planner.hasDeferredExecutionPublicationWork,
              let deferredPublicationCoordinator else { return outcome }
        _ = await deferredPublicationCoordinator()
        return outcome
    }

    private func resumePendingDeferIntentIfNeeded() async -> ExecutionSyncOutcome {
        guard let intent = planner.pendingExecutionDeferIntent else { return .success }
        guard intent.hasValidShape else {
            return clearInvalidDeferIntent(intent)
        }
        if intent.moveStart <= now() {
            do {
                try planner.clearExecutionDeferIntent(
                    intent,
                    message: "The saved move time expired; the exact session remains paused for review"
                )
                return .success
            } catch {
                return report(error)
            }
        }
        if let closed = planner.executionState.terminalOutcomes[intent.identity.sessionID]?.session {
            let exact = exactDeferredClosure(intent: intent, session: closed)
            do {
                try planner.clearExecutionDeferIntent(
                    intent,
                    message: exact
                        ? "Recovered the exact move; publishing its replacement placement"
                        : "Execution changed before the saved move could finish; review its terminal state"
                )
            } catch {
                return report(error)
            }
            return exact ? .success : .conflict
        }
        guard planner.executionState.activeSession.map(intent.identity.matches) == true else {
            return .conflict
        }
        guard let block = planner.blocks.first(where: { $0.id == intent.focusedBlockID }),
              let active = planner.executionState.activeSession,
              executionSession(active, matches: block),
              sourceBlockMatchesIntent(block, intent: intent) else {
            return clearInvalidDeferIntent(intent)
        }
        return await deferWork(intent.focusedBlockID, moveStart: intent.moveStart)
    }

    private func clearInvalidDeferIntent(
        _ intent: DayWeavePendingExecutionDeferIntent
    ) -> ExecutionSyncOutcome {
        do {
            try planner.clearExecutionDeferIntent(
                intent,
                message: "The saved execution move no longer matches current state; the exact session remains paused for a fresh assessment"
            )
            return .conflict
        } catch {
            return report(error)
        }
    }

    private func exactDeferredClosure(
        intent: DayWeavePendingExecutionDeferIntent,
        session: DayWeaveExecutionSession
    ) -> Bool {
        guard session.status == .deferred,
              intent.identity.matches(session),
              session.moveStart == intent.moveStart,
              let block = planner.blocks.first(where: { $0.id == intent.focusedBlockID }),
              sourceBlockMatchesIntent(block, intent: intent),
              let actual = session.actualSeconds,
              actual == session.accumulatedSeconds else { return false }
        if let assessment = intent.assessment {
            return actual == assessment.actualSeconds
                && session.moveEnd == assessment.moveEnd
                && assessment.moveStart == intent.moveStart
        }
        // A schema-15 intent can survive only beside an already journaled
        // legacy command. It gains no authority, but its exact historical
        // result may still be recognized after byte-for-byte replay.
        guard let planned = try? exactPlannedSeconds(for: block), actual < planned else {
            return false
        }
        return session.moveEnd == intent.moveStart.addingTimeInterval(
            TimeInterval(planned - actual)
        )
    }

    private func sourceBlockMatchesIntent(
        _ block: ScheduleBlock,
        intent: DayWeavePendingExecutionDeferIntent
    ) -> Bool {
        block.id == intent.focusedBlockID
            && block.sourceItemID == intent.identity.itemID
            && block.sourceItemRevision == intent.identity.itemRevision
            && block.occurrenceID == intent.identity.occurrenceID
            && (block.sessionIndex ?? 0) == intent.identity.sessionIndex
            && dayWeavePostgresEpochMicroseconds(block.start)
                == dayWeavePostgresEpochMicroseconds(intent.sourceStart)
            && dayWeavePostgresEpochMicroseconds(block.end)
                == dayWeavePostgresEpochMicroseconds(intent.sourceEnd)
    }

    private func recoveredDeferIsExact(
        _ pending: DayWeavePendingExecutionCommand,
        session: DayWeaveExecutionSession
    ) -> Bool {
        guard case let .deferWork(
            _, moveStart, moveEnd, correctedActual, assessmentDigest, approvedDigest
        ) = pending.command,
              let prior = pending.priorSession,
              prior.status == .paused,
              pending.identity.matches(prior),
              pending.identity.matches(session),
              pending.command.matchesChangedSession(session),
              correctedActual == prior.accumulatedSeconds,
              session.accumulatedSeconds == prior.accumulatedSeconds,
              let block = planner.blocks.first(where: { $0.id == pending.focusedBlockID }),
              executionSession(prior, matches: block) else { return false }
        if let assessmentDigest {
            return isCanonicalExecutionDigest(assessmentDigest)
                && (approvedDigest == nil || approvedDigest == assessmentDigest)
                && dayWeaveExactWholeSecondDelta(from: moveStart, to: moveEnd) != nil
        }
        guard approvedDigest == nil,
              let planned = try? exactPlannedSeconds(for: block),
              prior.accumulatedSeconds < planned else { return false }
        return dayWeaveExactWholeSecondDelta(from: moveStart, to: moveEnd)
            == planned - prior.accumulatedSeconds
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
                    mutation.changedSession.status.isCanonicalTerminal
                        && pending.canonicalProjectionEligibleAtLeaseStart
                        ? .pending : .notRequired
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
                let eligible = session.status.isCanonicalTerminal
                    && (next.leaseProjectionEligibility[session.id]
                        ?? next.pendingCommand.flatMap {
                            $0.identity.sessionID == session.id
                                ? $0.canonicalProjectionEligibleAtLeaseStart : nil
                        }
                        ?? false)
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
        case .deferWork: .deferred
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
              changed.createdAt == prior.createdAt,
              changed.updatedAt >= prior.updatedAt,
              accumulatedTransitionIsValid(prior: prior, changed: changed) else { return false }
        switch pending.command {
        case let .pause(_, duration, absolute, reason):
            let expectedUntil = duration.map {
                changed.updatedAt.addingTimeInterval(TimeInterval($0))
            } ?? absolute
            return changed.pausedAt == (prior.pausedAt ?? changed.updatedAt)
                && changed.pauseUntil == expectedUntil
                && changed.pauseReason == (reason ?? prior.pauseReason)
        case .resume:
            return prior.status == .paused
                && changed.accumulatedSeconds == prior.accumulatedSeconds
        case let .complete(_, corrected), let .skip(_, corrected):
            let actualIsValid = corrected.map { changed.actualSeconds == $0 }
                ?? (changed.actualSeconds == changed.accumulatedSeconds)
            return actualIsValid
                && changed.pausedAt == (prior.pausedAt
                    ?? (prior.status == .paused ? changed.updatedAt : nil))
        case let .deferWork(_, moveStart, moveEnd, corrected, assessmentDigest, approvedDigest):
            let actualIsValid = corrected.map { changed.actualSeconds == $0 }
                ?? (changed.actualSeconds == changed.accumulatedSeconds)
            return prior.status == .paused
                && changed.accumulatedSeconds == prior.accumulatedSeconds
                && actualIsValid
                && changed.moveStart == moveStart
                && changed.moveEnd == moveEnd
                && changed.pausedAt == (prior.pausedAt ?? changed.updatedAt)
                && (assessmentDigest.map(isCanonicalExecutionDigest) ?? true)
                && (approvedDigest == nil || approvedDigest == assessmentDigest)
        case .start:
            return false
        }
    }

    private func exactPlannedSeconds(for block: ScheduleBlock) throws -> UInt64 {
        guard let exact = dayWeaveExactWholeSecondDelta(from: block.start, to: block.end),
              exact <= 86_400 else {
            throw ExecutionSyncControllerError.invalidLocalState(
                "The published block does not have an exact supported duration."
            )
        }
        return exact
    }

    private func accumulatedTransitionIsValid(
        prior: DayWeaveExecutionSession,
        changed: DayWeaveExecutionSession
    ) -> Bool {
        guard changed.accumulatedSeconds >= prior.accumulatedSeconds else { return false }
        // A paused session has no private running anchor, so its accumulated
        // value is exactly stable. For an active session, public running_since
        // is the causal protocol clock and may be ahead of the server's private
        // observed timer anchor after wall-clock rollback; only monotonicity is
        // externally provable in that case.
        return prior.runningSince != nil
            || changed.accumulatedSeconds == prior.accumulatedSeconds
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

    private func executionSession(
        _ session: DayWeaveExecutionSession,
        matches block: ScheduleBlock
    ) -> Bool {
        session.itemID == block.sourceItemID
            && session.itemRevision == block.sourceItemRevision
            && session.occurrenceID == block.occurrenceID
            && session.sessionIndex == (block.sessionIndex ?? 0)
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
        reconcilesBreakNotificationAfterward: Bool = true,
        _ operation: @escaping (UUID, UInt64) async throws -> ExecutionSyncOutcome
    ) async -> ExecutionSyncOutcome {
        guard operationID == nil else { return .invalidLocalState }
        guard planner.pendingProposalApplicationMutation == nil else {
            return report(ExecutionSyncControllerError.invalidLocalState(
                "Recover the exact pending proposal application or undo before changing cross-device execution."
            ))
        }
        guard planner.canPersistPlan, planner.hasEncryptedPersistence else {
            return report(PlannerExecutionStateError.encryptedPersistenceRequired)
        }
        while !planner.beginExecutionSync() {
            guard !Task.isCancelled else { return .configurationChanged }
            guard operationID == nil else { return .invalidLocalState }
            guard planner.pendingProposalApplicationMutation == nil else {
                return report(ExecutionSyncControllerError.invalidLocalState(
                    "Recover the exact pending proposal application or undo before changing cross-device execution."
                ))
            }
            guard planner.canPersistPlan, planner.hasEncryptedPersistence else {
                return report(PlannerExecutionStateError.encryptedPersistenceRequired)
            }
            guard planner.isCanonicalSyncLocked else { return .invalidLocalState }
            do {
                try await Task.sleep(for: .milliseconds(25))
            } catch {
                return .configurationChanged
            }
        }
        let id = UUID()
        let generation = configurationGeneration
        operationID = id
        isSyncing = true
        let outcome: ExecutionSyncOutcome
        do {
            outcome = try await operation(id, generation)
        } catch {
            outcome = report(error)
        }
        clearBreakResolutionPresentationIfStale()
        // OS scheduling/removal is an awaited durability boundary for an
        // already-authorized notification center. A not-determined state is
        // returned without prompting and therefore cannot delay the server
        // command behind a system permission sheet.
        if reconcilesBreakNotificationAfterward {
            _ = await reconcileBreakNotification()
        }
        if operationID == id {
            operationID = nil
            isSyncing = false
        }
        planner.endCanonicalSync()
        reconcileBreakAlternativeSelection()
        return outcome
    }

    private func acknowledgeExpiredBreakKeepingPaused(
        presentAlternatives: Bool
    ) async -> ExecutionSyncOutcome {
        await runExclusive(reconcilesBreakNotificationAfterward: false) {
            [self] _, _ in
            guard expiredBreakChoiceRequired,
                  let capturedSession = planner.executionState.activeSession,
                  let capturedDescriptor = DayWeaveBreakNotificationContract.descriptor(
                      for: capturedSession
                  ),
                  capturedDescriptor.deadline <= now() else {
                throw ExecutionSyncControllerError.invalidLocalState(
                    "The expired break changed before it could be acknowledged."
                )
            }
            let source = BreakAlternativeHandoffSource(session: capturedSession)
            setBusy("Verifying removal of the exact break reminder…")
            breakNotificationReconciliationIsSuppressed = true
            let cancellation = await breakNotificationCoordinator.cancelExact(
                identifier: capturedDescriptor.identifier,
                session: capturedSession,
                acknowledged: source.version
            )
            breakNotificationAuthorizationState =
                await breakNotificationCoordinator.authorizationState()
            applyBreakNotificationResult(cancellation)

            guard cancellation.isVerifiedCancellation else {
                breakNotificationReconciliationIsSuppressed = false
                _ = await reconcileBreakNotification()
                throw PlannerExecutionStateError.breakNotificationCancellationUnavailable
            }

            guard expiredBreakChoiceRequired,
                  let current = planner.executionState.activeSession,
                  source.matches(current),
                  DayWeaveBreakNotificationContract.descriptor(for: current)?.identifier
                    == capturedDescriptor.identifier else {
                breakNotificationReconciliationIsSuppressed = false
                _ = await reconcileBreakNotification()
                throw ExecutionSyncControllerError.unstableRead
            }

            do {
                try planner.persistExpiredPauseAcknowledgment(
                    source.version,
                    message:
                        "Break ended; the session remains paused until you move, resume, or finish it"
                )
            } catch {
                breakNotificationReconciliationIsSuppressed = false
                _ = await reconcileBreakNotification()
                throw error
            }

            breakResolutionPresentation = nil
            breakNotificationTapIssue = nil
            breakNotificationReconciliationIsSuppressed = false
            scheduleBreakDeadlineWakeIfNeeded()
            if presentAlternatives {
                breakAlternativeHandoffSource = source
                selectedBreakAlternativeBlockID = nil
            }
            setConnected("Break acknowledged; the authoritative session remains paused")
            return .success
        }
    }

    private func clearBreakResolutionPresentationIfStale() {
        guard let presented = breakResolutionPresentation else { return }
        let currentIdentifier = planner.executionState.activeSession.flatMap {
            DayWeaveBreakNotificationContract.descriptor(for: $0)?.identifier
        }
        if currentIdentifier != presented.observedBreakIdentifier
            || !expiredBreakChoiceRequired {
            breakResolutionPresentation = nil
            breakNotificationTapIssue = nil
        }
    }

    private func scheduleBreakDeadlineWakeIfNeeded() {
        let descriptor = DayWeaveBreakNotificationContract.descriptor(
            for: planner.executionState.activeSession
        )
        let acknowledged = planner.executionState.acknowledgedExpiredPause
        let observedAt = now()
        guard !breakNotificationReconciliationIsSuppressed,
              let descriptor,
              descriptor.version != acknowledged,
              descriptor.deadline > observedAt else {
            breakDeadlineWakeTask?.cancel()
            breakDeadlineWakeTask = nil
            scheduledBreakDeadlineIdentifier = nil
            return
        }
        guard scheduledBreakDeadlineIdentifier != descriptor.identifier
                || breakDeadlineWakeTask == nil else { return }

        breakDeadlineWakeTask?.cancel()
        scheduledBreakDeadlineIdentifier = descriptor.identifier
        let delay = Duration.seconds(
            descriptor.deadline.timeIntervalSince(observedAt)
        )
        let sleep = breakDeadlineSleep
        breakDeadlineWakeTask = Task { @MainActor [weak self] in
            await sleep(delay)
            guard !Task.isCancelled,
                  let self,
                  self.scheduledBreakDeadlineIdentifier == descriptor.identifier else {
                return
            }
            self.breakDeadlineWakeTask = nil
            self.scheduledBreakDeadlineIdentifier = nil
            self.clearBreakResolutionPresentationIfStale()
            self.breakResolutionWakeGeneration &+= 1
        }
    }

    private func applyBreakNotificationResult(
        _ result: DayWeaveBreakNotificationReconcileResult
    ) {
        switch result {
        case .unavailable:
            breakNotificationIssue = .schedulingUnavailable
        case .cancellationUnavailable:
            breakNotificationIssue = .cancellationUnavailable
        case .scheduled, .canceled, .unchanged, .permissionDenied:
            breakNotificationIssue = nil
        case .permissionRequired:
            // A prior add failure is no longer relevant, but a failed explicit
            // permission request remains visible until the user retries it.
            if breakNotificationIssue == .schedulingUnavailable {
                breakNotificationIssue = nil
            }
        case .superseded:
            // This caller waited for a newer generation to converge. The owner
            // of that newest input publishes its result; an older waiter must
            // not overwrite it after resumption.
            break
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
        case let DayWeaveAPIError.durableAuthentication(authError):
            switch authError {
            case .notConfigured, .originMismatch, .invalidBootstrapCredential,
                 .invalidEnrollmentCode,
                 .durableSessionRequiresExplicitReenrollment, .remoteRevocationUnavailable,
                 .activeSessionMustBeRevoked,
                 .enrollmentRequired,
                 .reauthenticationRequired, .rejected:
                phase = .authenticationRequired
                message = planner.executionState.pendingCommand == nil
                    ? authError.localizedDescription
                    : "Authentication needs attention; the exact pending command remains fenced."
                outcome = .authenticationRequired
            case .transport:
                phase = .offline
                message = "Offline; the exact authentication and execution recovery state was retained."
                outcome = .transientNetworkFailure
            case .retryableServer:
                phase = .offline
                message = "Authentication is temporarily unavailable; exact recovery state was retained."
                outcome = .retryableServerFailure
            case .localStateUnavailable, .concurrentStateChange:
                phase = .failed
                message = "Atomic Keychain authentication state is unavailable; execution remains fenced."
                outcome = .localStorageFailure
            case .incompatibleState, .invalidResponse, .responseTooLarge:
                phase = .failed
                message = "The durable authentication contract is incompatible; execution remains fenced."
                outcome = .protocolFailure
            case .randomnessUnavailable, .requestEncodingFailed:
                phase = .failed
                message = "A safe authentication request could not be prepared; no network action was attempted."
                outcome = .invalidLocalState
            }
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
        case PlannerExecutionStateError.breakNotificationCancellationUnavailable:
            phase = .failed
            message = PlannerExecutionStateError
                .breakNotificationCancellationUnavailable.localizedDescription
            outcome = .unexpectedFailure
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
