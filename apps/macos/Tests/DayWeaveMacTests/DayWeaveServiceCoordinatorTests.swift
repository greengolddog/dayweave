import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Foreground service coordination")
@MainActor
struct DayWeaveServiceCoordinatorTests {
    @Test("manual proposal recovery resumes execution, canonical sync, then polling")
    func manualRecoveryResumesOrderedServices() async {
        let events = ServiceEventLog()
        let proposals = ProposalRecoveryDouble(
            hasPendingRecovery: true,
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

        #expect(await coordinator.recoverPendingProposalAndResume())
        #expect(coordinator.servicesAreActive)
        #expect(events.values == [
            "proposal.recover",
            "execution.refresh",
            "canonical.sync",
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
            "canonical.sync",
            "execution.poll",
        ])
    }
}

@MainActor
private final class ServiceEventLog {
    var values: [String] = []
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

    init(events: ServiceEventLog) {
        self.events = events
    }

    func sync() async {
        events.values.append("canonical.sync")
    }
}
#endif
