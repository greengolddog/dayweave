import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Foreground service coordination")
@MainActor
struct DayWeaveServiceCoordinatorTests {
    @Test("occurrence recovery runs once before foreground composition and suspends at privacy boundaries")
    func routineOccurrenceLifecycle() async {
        let events = ServiceEventLog()
        let occurrences = RoutineOccurrenceServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: ProposalRecoveryDouble(hasPendingRecovery: false, resolvesRecovery: true,
                reportedResult: true, events: events),
            executionSync: ExecutionServiceDouble(events: events),
            canonicalSync: CanonicalServiceDouble(events: events), routineOccurrences: occurrences)

        coordinator.activate()
        await coordinator.waitForActivation()
        #expect(events.values == ["occurrence.activate", "occurrence.replay",
            "execution.refresh", "canonical.bootstrap", "canonical.poll", "execution.poll"])
        coordinator.deactivate()
        #expect(events.values.last == "occurrence.suspend")
        #expect(!coordinator.servicesAreActive)
    }

    @Test("held occurrence recovery cannot resume execution after privacy deactivation")
    func heldOccurrenceRecoveryStopsAtPrivacyBoundary() async {
        let events = ServiceEventLog()
        let occurrences = RoutineOccurrenceServiceDouble(events: events, holdReplay: true)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: ProposalRecoveryDouble(hasPendingRecovery: false, resolvesRecovery: true,
                reportedResult: true, events: events),
            executionSync: ExecutionServiceDouble(events: events),
            canonicalSync: CanonicalServiceDouble(events: events), routineOccurrences: occurrences)
        coordinator.activate()
        await occurrences.waitUntilHeld()
        coordinator.deactivate()
        let stoppedEvents = events.values
        occurrences.release()
        await Task.yield()
        #expect(!coordinator.servicesAreActive)
        #expect(events.values == stoppedEvents)
        #expect(!events.values.contains("execution.refresh"))
    }

    @Test("startup recovers proposal and Google outbound journals before reconciliation")
    func startupRecoversPendingJournalsInOrder() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: true,
            resolvesRecovery: true,
            reportedResult: true,
            events: events
        )
        let googleOutbound = GoogleOutboundRecoveryDouble(
            hasPendingRecovery: true,
            resolvesRecovery: true,
            reportedResult: true,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            googleOutbound: googleOutbound,
            executionSync: execution,
            canonicalSync: canonical
        )

        coordinator.activate()
        await coordinator.waitForActivation()

        #expect(coordinator.servicesAreActive)
        #expect(!proposals.hasPendingRecovery)
        #expect(!googleOutbound.hasPendingRecovery)
        #expect(events.values == [
            "proposal.recover",
            "google-outbound.recover",
            "execution.refresh",
            "canonical.bootstrap",
            "canonical.poll",
            "execution.poll",
        ])
    }

    @Test("failed Google outbound recovery preserves local foreground services")
    func failedGoogleRecoveryKeepsForegroundServicesAvailable() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: false,
            resolvesRecovery: false,
            reportedResult: false,
            events: events
        )
        let googleOutbound = GoogleOutboundRecoveryDouble(
            hasPendingRecovery: true,
            resolvesRecovery: false,
            reportedResult: false,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            googleOutbound: googleOutbound,
            executionSync: execution,
            canonicalSync: canonical
        )

        coordinator.activate()
        await coordinator.waitForActivation()

        #expect(coordinator.servicesAreActive)
        #expect(googleOutbound.hasPendingRecovery)
        #expect(events.values == [
            "google-outbound.recover",
            "execution.refresh",
            "canonical.bootstrap",
            "canonical.poll",
            "execution.poll",
        ])
    }

    @Test("manual proposal recovery resumes execution, canonical sync, then polling")
    func manualRecoveryResumesOrderedServices() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: false,
            resolvesRecovery: true,
            reportedResult: false,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            executionSync: execution,
            canonicalSync: canonical
        )

        coordinator.activate()
        await coordinator.waitForActivation()
        proposals.hasPendingRecovery = true
        events.values.removeAll()
        #expect(await coordinator.recoverPendingProposalAndResume())
        #expect(coordinator.servicesAreActive)
        #expect(events.values == [
            "proposal.recover",
            "execution.refresh",
            "canonical.bootstrap",
            "canonical.poll",
            "execution.poll",
        ])
    }

    @Test("an unresolved startup recovery releases activation for a later manual resume")
    func unresolvedActivationCanResumeLater() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: true,
            resolvesRecovery: false,
            reportedResult: false,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            executionSync: execution,
            canonicalSync: canonical
        )

        coordinator.activate()
        await coordinator.waitForActivation()
        #expect(!coordinator.servicesAreActive)
        #expect(events.values == ["proposal.recover"])

        proposals.resolvesRecovery = true
        #expect(await coordinator.recoverPendingProposalAndResume())
        #expect(coordinator.servicesAreActive)
        #expect(events.values == [
            "proposal.recover",
            "proposal.recover",
            "execution.refresh",
            "canonical.bootstrap",
            "canonical.poll",
            "execution.poll",
        ])
    }

    @Test("manual recovery cannot self-authorize before any foreground activation")
    func initiallyInactiveManualRecoveryDoesNothing() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(hasPendingRecovery: true, resolvesRecovery: true,
            reportedResult: true, events: events)
        let outbound = GoogleOutboundRecoveryDouble(hasPendingRecovery: true, resolvesRecovery: true,
            reportedResult: true, events: events)
        let coordinator = DayWeaveServiceCoordinator(proposalApplications: proposals,
            googleOutbound: outbound, executionSync: ExecutionServiceDouble(events: events),
            canonicalSync: CanonicalServiceDouble(events: events),
            itemProgress: ProgressServiceDouble(events: events))

        #expect(await coordinator.recoverPendingProposalAndResume() == false)
        #expect(!coordinator.servicesAreActive)
        #expect(proposals.hasPendingRecovery && outbound.hasPendingRecovery)
        #expect(events.values.isEmpty)
    }

    @Test("a delayed manual UI task cannot start recovery after deactivation")
    func manualTaskStartingAfterDeactivationDoesNothing() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(hasPendingRecovery: false, resolvesRecovery: true,
            reportedResult: true, events: events)
        let outbound = GoogleOutboundRecoveryDouble(hasPendingRecovery: false, resolvesRecovery: true,
            reportedResult: true, events: events)
        let coordinator = DayWeaveServiceCoordinator(proposalApplications: proposals,
            googleOutbound: outbound, executionSync: ExecutionServiceDouble(events: events),
            canonicalSync: CanonicalServiceDouble(events: events),
            itemProgress: ProgressServiceDouble(events: events))
        coordinator.activate()
        await coordinator.waitForActivation()
        proposals.hasPendingRecovery = true
        outbound.hasPendingRecovery = true
        events.values.removeAll()
        // This MainActor task cannot enter recovery until the current actor turn
        // yields. Deactivation therefore precedes even its initial authority check.
        let delayedRecovery = Task { @MainActor in await coordinator.recoverPendingProposalAndResume() }
        coordinator.deactivate()
        let stoppedEvents = events.values

        #expect(await delayedRecovery.value == false)
        #expect(await coordinator.recoverPendingProposalAndResume() == false)
        #expect(!coordinator.servicesAreActive)
        #expect(proposals.hasPendingRecovery && outbound.hasPendingRecovery)
        #expect(events.values == stoppedEvents)
    }

    @Test("held manual recovery cannot reactivate services after privacy deactivation")
    func heldManualRecoveryCannotUndoDeactivation() async {
        for kind in ["proposal", "google-outbound", "google-schedule"] {
            let events = ServiceEventLog()
            let held = HeldServiceRecovery(kind: kind, events: events)
            let coordinator = await heldRecoveryCoordinator(held, kind: kind, events: events)
            let recovery = Task { await coordinator.recoverPendingProposalAndResume() }
            await held.waitUntilHeld()
            coordinator.deactivate()
            let stoppedEvents = events.values
            held.release()

            #expect(await recovery.value == false, "\(kind)")
            #expect(!coordinator.servicesAreActive, "\(kind)")
            #expect(events.values == stoppedEvents, "\(kind)")
            #expect(!events.values.contains("progress.activate"), "\(kind)")
        }
    }

    @Test("old manual recovery cannot replace a newer foreground activation")
    func heldManualRecoveryCannotReplaceNewActivation() async {
        for kind in ["proposal", "google-outbound", "google-schedule"] {
            let events = ServiceEventLog()
            let held = HeldServiceRecovery(kind: kind, events: events)
            let coordinator = await heldRecoveryCoordinator(held, kind: kind, events: events)
            let recovery = Task { await coordinator.recoverPendingProposalAndResume() }
            await held.waitUntilHeld()
            coordinator.deactivate()
            coordinator.activate()
            await coordinator.waitForActivation()
            #expect(coordinator.servicesAreActive, "\(kind)")
            let newActivationEvents = events.values
            held.release()

            #expect(await recovery.value == false, "\(kind)")
            #expect(coordinator.servicesAreActive, "\(kind)")
            #expect(events.values == newActivationEvents, "\(kind)")
            #expect(events.values.filter { $0 == "progress.activate" }.count == 1, "\(kind)")
        }
    }

    @Test("cancelled manual recovery cannot start reconciliation after a late reply")
    func cancelledManualRecoveryDoesNotResume() async {
        let events = ServiceEventLog()
        let held = HeldServiceRecovery(kind: "proposal", events: events)
        let coordinator = await heldRecoveryCoordinator(held, kind: "proposal", events: events)
        let recovery = Task { await coordinator.recoverPendingProposalAndResume() }
        await held.waitUntilHeld()
        recovery.cancel()
        held.release()
        #expect(await recovery.value == false)
        #expect(coordinator.servicesAreActive) // Cancellation cannot stop the existing foreground lifecycle.
        #expect(events.values == ["proposal.recover"])
    }

    private func heldRecoveryCoordinator(_ held: HeldServiceRecovery, kind: String,
                                        events: ServiceEventLog) async -> DayWeaveServiceCoordinator {
        let proposals: any ProposalApplicationRecovering
        if kind == "proposal" { proposals = held }
        else {
            proposals = ProposalRecoveryDouble(hasPendingRecovery: false, resolvesRecovery: false,
                reportedResult: false, events: events)
        }
        let coordinator = DayWeaveServiceCoordinator(proposalApplications: proposals,
            googleOutbound: kind == "google-outbound" ? held : nil,
            googleSchedulePublication: kind == "google-schedule" ? held : nil,
            executionSync: ExecutionServiceDouble(events: events),
            canonicalSync: CanonicalServiceDouble(events: events),
            itemProgress: ProgressServiceDouble(events: events))
        held.hasPendingRecovery = false
        coordinator.activate()
        await coordinator.waitForActivation()
        held.hasPendingRecovery = true
        events.values.removeAll()
        return coordinator
    }

    @Test("privacy deactivation stops all execution foreground delivery")
    func deactivationStopsExecutionForegroundDelivery() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: false,
            resolvesRecovery: false,
            reportedResult: false,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            executionSync: execution,
            canonicalSync: canonical
        )

        coordinator.activate()
        await coordinator.waitForActivation()
        coordinator.deactivate()

        #expect(!coordinator.servicesAreActive)
        #expect(events.values == [
            "execution.refresh",
            "canonical.bootstrap",
            "canonical.poll",
            "execution.poll",
            "canonical.stop",
            "execution.stop",
        ])
    }

    @Test("failed initial canonical sync starts the binding-guarded recovery manager")
    func failedCanonicalSyncStartsGuardedItemDelivery() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: false,
            resolvesRecovery: false,
            reportedResult: false,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events, syncSucceeds: false)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            executionSync: execution,
            canonicalSync: canonical
        )

        coordinator.activate()
        await coordinator.waitForActivation()

        #expect(events.values == [
            "execution.refresh",
            "canonical.bootstrap",
            "canonical.poll",
            "execution.poll",
        ])
    }

    @Test("habit sync establishes its checkpoint before canonical bootstrap and is scrubbed at privacy")
    func habitLifecycleIsOrderedAndPrivate() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: false,
            resolvesRecovery: false,
            reportedResult: false,
            events: events
        )
        let execution = ExecutionServiceDouble(events: events)
        let canonical = CanonicalServiceDouble(events: events)
        let habits = HabitServiceDouble(events: events)
        let coordinator = DayWeaveServiceCoordinator(
            proposalApplications: proposals,
            executionSync: execution,
            canonicalSync: canonical,
            habitSync: habits
        )

        coordinator.activate()
        await coordinator.waitForActivation()
        coordinator.deactivate()

        #expect(events.values == [
            "execution.refresh",
            "habit.activate",
            "canonical.bootstrap",
            "canonical.poll",
            "habit.poll",
            "execution.poll",
            "canonical.stop",
            "execution.stop",
            "habit.stop",
            "habit.suspend",
        ])
    }
}

@MainActor
private final class ServiceEventLog {
    var values: [String] = []
}

/// Holds only the first call, deliberately ignoring cancellation to model a
/// recovery response racing with deactivation or a newer activation.
@MainActor
private final class HeldServiceRecovery: ProposalApplicationRecovering, GoogleOutboundRecovering,
    GoogleSchedulePublicationRecovering {
    var hasPendingRecovery = true
    private let kind: String
    private let events: ServiceEventLog
    private var calls = 0
    private var held: CheckedContinuation<Void, Never>?
    private var entered: CheckedContinuation<Void, Never>?

    init(kind: String, events: ServiceEventLog) { self.kind = kind; self.events = events }
    func recoverPendingMutation() async -> Bool { await recover() }
    func recoverPendingOperation() async -> Bool { await recover() }
    func recoverPendingPublication() async -> Bool { await recover() }
    func waitUntilHeld() async {
        if held != nil { return }
        await withCheckedContinuation { entered = $0 }
    }
    func release() { let completion = held; held = nil; completion?.resume() }
    private func recover() async -> Bool {
        calls += 1
        events.values.append("\(kind).recover")
        if calls == 1 {
            await withCheckedContinuation { continuation in
                held = continuation
                let waiting = entered; entered = nil; waiting?.resume()
            }
        }
        hasPendingRecovery = false
        return true
    }
}

@MainActor
private final class ProgressServiceDouble: ItemProgressServiceSynchronizing {
    private let events: ServiceEventLog
    init(events: ServiceEventLog) { self.events = events }
    func activate() { events.values.append("progress.activate") }
    func suspendForPrivacyBoundary() { events.values.append("progress.suspend") }
    func replayPending() async -> Bool { events.values.append("progress.replay"); return true }
}

@MainActor
private final class RoutineOccurrenceServiceDouble: RoutineOccurrenceServiceSynchronizing {
    private let events: ServiceEventLog
    private let holdReplay: Bool
    private var held: CheckedContinuation<Void, Never>?
    private var entered: CheckedContinuation<Void, Never>?

    init(events: ServiceEventLog, holdReplay: Bool = false) { self.events = events; self.holdReplay = holdReplay }
    func activate() { events.values.append("occurrence.activate") }
    func suspendForPrivacyBoundary() { events.values.append("occurrence.suspend") }
    func replayPending() async -> Bool {
        events.values.append("occurrence.replay")
        if holdReplay {
            await withCheckedContinuation { continuation in
                held = continuation
                let waiting = entered; entered = nil; waiting?.resume()
            }
        }
        return true
    }
    func waitUntilHeld() async {
        if held != nil { return }
        await withCheckedContinuation { entered = $0 }
    }
    func release() { let continuation = held; held = nil; continuation?.resume() }
}

@MainActor
private final class ProposalRecoveryDouble: ProposalApplicationRecovering {
    var hasPendingRecovery: Bool
    var resolvesRecovery: Bool
    private let reportedResult: Bool
    private let events: ServiceEventLog

    init(
        hasPendingRecovery: Bool,
        resolvesRecovery: Bool,
        reportedResult: Bool,
        events: ServiceEventLog
    ) {
        self.hasPendingRecovery = hasPendingRecovery
        self.resolvesRecovery = resolvesRecovery
        self.reportedResult = reportedResult
        self.events = events
    }

    func recoverPendingMutation() async -> Bool {
        events.values.append("proposal.recover")
        if resolvesRecovery {
            hasPendingRecovery = false
        }
        return reportedResult
    }
}

@MainActor
private final class GoogleOutboundRecoveryDouble: GoogleOutboundRecovering {
    var hasPendingRecovery: Bool
    private let resolvesRecovery: Bool
    private let reportedResult: Bool
    private let events: ServiceEventLog

    init(
        hasPendingRecovery: Bool,
        resolvesRecovery: Bool,
        reportedResult: Bool,
        events: ServiceEventLog
    ) {
        self.hasPendingRecovery = hasPendingRecovery
        self.resolvesRecovery = resolvesRecovery
        self.reportedResult = reportedResult
        self.events = events
    }

    func recoverPendingOperation() async -> Bool {
        events.values.append("google-outbound.recover")
        if resolvesRecovery {
            hasPendingRecovery = false
        }
        return reportedResult
    }
}

@MainActor
private final class ExecutionServiceDouble: ExecutionServiceSynchronizing {
    private let events: ServiceEventLog

    init(events: ServiceEventLog) {
        self.events = events
    }

    func refresh() async -> ExecutionSyncOutcome {
        events.values.append("execution.refresh")
        return .success
    }

    func startForegroundPolling(every _: Duration) {
        events.values.append("execution.poll")
    }

    func stopForegroundPolling() {
        events.values.append("execution.stop")
    }
}

@MainActor
private final class CanonicalServiceDouble: CanonicalServiceSynchronizing {
    let isConfigured = true
    private let events: ServiceEventLog
    private let syncSucceeds: Bool

    init(events: ServiceEventLog, syncSucceeds: Bool = true) {
        self.events = events
        self.syncSucceeds = syncSucceeds
    }

    func bootstrapForegroundActivation() async -> Bool {
        events.values.append("canonical.bootstrap")
        return syncSucceeds
    }

    func syncThroughFreshComposition() async -> Bool {
        events.values.append("canonical.sync")
        return syncSucceeds
    }

    func startForegroundItemInvalidations(every _: Duration) {
        events.values.append("canonical.poll")
    }

    func stopForegroundItemInvalidations() {
        events.values.append("canonical.stop")
    }
}

@MainActor
private final class HabitServiceDouble: HabitServiceSynchronizing {
    private let events: ServiceEventLog

    init(events: ServiceEventLog) {
        self.events = events
    }

    func activate() async -> HabitSyncOutcome {
        events.values.append("habit.activate")
        return .success
    }

    func startForegroundPolling(every _: Duration) {
        events.values.append("habit.poll")
    }

    func stopForegroundPolling() {
        events.values.append("habit.stop")
    }

    func suspendForPrivacyBoundary() {
        events.values.append("habit.suspend")
    }
}
#endif
