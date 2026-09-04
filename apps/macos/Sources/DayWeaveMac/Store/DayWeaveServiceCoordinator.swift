import Combine
import Foundation

@MainActor
protocol ProposalApplicationRecovering: AnyObject {
    var hasPendingRecovery: Bool { get }

    @discardableResult
    func recoverPendingMutation() async -> Bool
}

extension ProposalApplicationStore: ProposalApplicationRecovering {}

@MainActor
protocol GoogleOutboundRecovering: AnyObject {
    var hasPendingRecovery: Bool { get }

    @discardableResult
    func recoverPendingOperation() async -> Bool
}

extension GoogleOutboundStore: GoogleOutboundRecovering {}

@MainActor
protocol GoogleSchedulePublicationRecovering: AnyObject {
    var hasPendingRecovery: Bool { get }

    @discardableResult
    func recoverPendingPublication() async -> Bool
}

extension GoogleSchedulePublicationStore: GoogleSchedulePublicationRecovering {}

@MainActor
protocol CanonicalServiceSynchronizing: AnyObject {
    var isConfigured: Bool { get }

    @discardableResult
    func bootstrapForegroundActivation() async -> Bool
    @discardableResult
    func syncThroughFreshComposition() async -> Bool
    func startForegroundItemInvalidations(every interval: Duration)
    func stopForegroundItemInvalidations()
}

extension CanonicalSyncStore: CanonicalServiceSynchronizing {}

@MainActor
protocol ExecutionServiceSynchronizing: AnyObject {
    func refresh() async -> ExecutionSyncOutcome
    func startForegroundPolling(every interval: Duration)
    func stopForegroundPolling()
}

extension ExecutionSyncStore: ExecutionServiceSynchronizing {}

@MainActor
protocol HabitServiceSynchronizing: AnyObject {
    @discardableResult
    func activate() async -> HabitSyncOutcome
    func startForegroundPolling(every interval: Duration)
    func stopForegroundPolling()
    func suspendForPrivacyBoundary()
}

extension HabitSyncStore: HabitServiceSynchronizing {}

/// Owns the foreground-service lifecycle so automatic and user-requested
/// proposal recovery resume the same execution -> habit -> canonical -> polling order.
/// A failed startup recovery releases the activation state instead of leaving
/// the app permanently marked active with its synchronizers stopped.
@MainActor
final class DayWeaveServiceCoordinator: ObservableObject {
    @Published private(set) var servicesAreActive = false

    private let proposalApplications: any ProposalApplicationRecovering
    private let googleOutbound: (any GoogleOutboundRecovering)?
    private let googleSchedulePublication: (any GoogleSchedulePublicationRecovering)?
    private let executionSync: any ExecutionServiceSynchronizing
    private let canonicalSync: any CanonicalServiceSynchronizing
    private let habitSync: (any HabitServiceSynchronizing)?
    private var activationTask: Task<Void, Never>?
    private var lifecycleGeneration: UInt64 = 0

    init(
        proposalApplications: any ProposalApplicationRecovering,
        googleOutbound: (any GoogleOutboundRecovering)? = nil,
        googleSchedulePublication: (any GoogleSchedulePublicationRecovering)? = nil,
        executionSync: any ExecutionServiceSynchronizing,
        canonicalSync: any CanonicalServiceSynchronizing,
        habitSync: (any HabitServiceSynchronizing)? = nil
    ) {
        self.proposalApplications = proposalApplications
        self.googleOutbound = googleOutbound
        self.googleSchedulePublication = googleSchedulePublication
        self.executionSync = executionSync
        self.canonicalSync = canonicalSync
        self.habitSync = habitSync
    }

    func activate() {
        guard !servicesAreActive, activationTask == nil else { return }
        lifecycleGeneration &+= 1
        let generation = lifecycleGeneration
        servicesAreActive = true
        activationTask = Task { @MainActor [weak self] in
            await self?.performActivation(generation: generation)
        }
    }

    func deactivate() {
        lifecycleGeneration &+= 1
        servicesAreActive = false
        activationTask?.cancel()
        activationTask = nil
        canonicalSync.stopForegroundItemInvalidations()
        executionSync.stopForegroundPolling()
        habitSync?.stopForegroundPolling()
        habitSync?.suspendForPrivacyBoundary()
    }

    /// Resolves a user-visible pending journal and resumes the full foreground
    /// service sequence. Recovery can legitimately return `false` after the
    /// server proves the exact request had no effect, so journal presence—not
    /// the Boolean result—is the authoritative completion signal.
    @discardableResult
    func recoverPendingProposalAndResume() async -> Bool {
        guard activationTask == nil else { return false }
        if proposalApplications.hasPendingRecovery {
            _ = await proposalApplications.recoverPendingMutation()
        }
        guard !proposalApplications.hasPendingRecovery, !Task.isCancelled else {
            return false
        }
        if googleOutbound?.hasPendingRecovery == true {
            _ = await googleOutbound?.recoverPendingOperation()
        }
        guard !Task.isCancelled else { return false }
        if googleSchedulePublication?.hasPendingRecovery == true {
            _ = await googleSchedulePublication?.recoverPendingPublication()
        }
        guard !Task.isCancelled else { return false }

        lifecycleGeneration &+= 1
        let generation = lifecycleGeneration
        servicesAreActive = true
        let resumed = await reconcileAndStartPolling(generation: generation)
        if !resumed, generation == lifecycleGeneration {
            servicesAreActive = false
        }
        return resumed
    }

    /// Used by focused lifecycle tests to observe the fire-and-forget activation
    /// entry point without exposing or replacing its task.
    func waitForActivation() async {
        await activationTask?.value
    }

    private func performActivation(generation: UInt64) async {
        defer {
            if generation == lifecycleGeneration {
                activationTask = nil
            }
        }

        if proposalApplications.hasPendingRecovery {
            _ = await proposalApplications.recoverPendingMutation()
            guard !proposalApplications.hasPendingRecovery else {
                if generation == lifecycleGeneration {
                    servicesAreActive = false
                }
                return
            }
        }

        if googleOutbound?.hasPendingRecovery == true {
            _ = await googleOutbound?.recoverPendingOperation()
            guard operationIsCurrent(generation) else { return }
        }

        if googleSchedulePublication?.hasPendingRecovery == true {
            _ = await googleSchedulePublication?.recoverPendingPublication()
            guard operationIsCurrent(generation) else { return }
        }

        guard await reconcileAndStartPolling(generation: generation) else {
            if generation == lifecycleGeneration {
                servicesAreActive = false
            }
            return
        }
    }

    private func reconcileAndStartPolling(generation: UInt64) async -> Bool {
        guard operationIsCurrent(generation) else { return false }
        let executionOutcome = await executionSync.refresh()
        guard operationIsCurrent(generation) else { return false }
        if let habitSync {
            _ = await habitSync.activate()
            guard operationIsCurrent(generation) else { return false }
        }
        if executionOutcome == .success, canonicalSync.isConfigured {
            _ = await canonicalSync.bootstrapForegroundActivation()
            guard operationIsCurrent(generation) else { return false }
            // Start the guarded delivery manager even when the read-first bootstrap
            // failed transiently. CanonicalSyncStore admits neither the stream
            // nor its fallback probe until the encrypted durable binding and
            // cursor match the current connection, so a failed persistence or
            // configuration transition remains fail-closed while an existing
            // exact binding can recover during this foreground activation.
            canonicalSync.startForegroundItemInvalidations(every: .seconds(30))
        }
        guard operationIsCurrent(generation) else { return false }
        if let habitSync {
            habitSync.startForegroundPolling(every: .seconds(30))
        }
        guard operationIsCurrent(generation) else { return false }
        executionSync.startForegroundPolling(every: .seconds(30))
        return true
    }

    private func operationIsCurrent(_ generation: UInt64) -> Bool {
        !Task.isCancelled
            && servicesAreActive
            && generation == lifecycleGeneration
    }
}
