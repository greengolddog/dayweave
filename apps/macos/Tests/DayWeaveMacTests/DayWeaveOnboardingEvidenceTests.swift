import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("macOS onboarding evidence")
struct DayWeaveOnboardingEvidenceTests {
    @Test("stored API credentials remain pending until positively verified")
    func storedCredentialIsNotVerification() {
        let check = DayWeaveOnboardingEvidence.apiConnection(
            durableAuthIsBusy: false,
            suggestionIsRefreshing: false,
            suggestionStatus: .ready,
            currentConfigurationIdentifier: "current",
            verifiedConfigurationIdentifier: nil
        )

        #expect(check.state == .pending)
    }

    @Test("busy and failed states override matching API evidence")
    func busyAndFailureOverrideEvidence() {
        let working = DayWeaveOnboardingEvidence.apiConnection(
            durableAuthIsBusy: true,
            suggestionIsRefreshing: false,
            suggestionStatus: .online(updatedAt: .distantPast, message: "Loaded"),
            currentConfigurationIdentifier: "current",
            verifiedConfigurationIdentifier: "current"
        )
        let blocked = DayWeaveOnboardingEvidence.apiConnection(
            durableAuthIsBusy: false,
            suggestionIsRefreshing: false,
            suggestionStatus: .failed("Rejected"),
            currentConfigurationIdentifier: "current",
            verifiedConfigurationIdentifier: "current"
        )

        #expect(working.state == .working)
        #expect(blocked == .blocked("Rejected"))
    }

    @Test("only matching live API evidence is ready")
    func exactConfigurationMatching() {
        let live = DayWeaveOnboardingEvidence.apiConnection(
            durableAuthIsBusy: false,
            suggestionIsRefreshing: false,
            suggestionStatus: .online(updatedAt: .distantPast, message: "Loaded"),
            currentConfigurationIdentifier: "current",
            verifiedConfigurationIdentifier: "current"
        )
        let mismatch = DayWeaveOnboardingEvidence.apiConnection(
            durableAuthIsBusy: false,
            suggestionIsRefreshing: false,
            suggestionStatus: .ready,
            currentConfigurationIdentifier: "current",
            verifiedConfigurationIdentifier: "old"
        )

        #expect(live.isReady)
        #expect(mismatch.state == .pending)
    }

    @Test("Google initial import gates only accounts owning selected resources")
    func selectedGoogleAccountsOnly() {
        let selected = UUID()
        let unselected = UUID()
        let inactive = UUID()

        let result = DayWeaveOnboardingEvidence.googleAccountIDsRequiringInitialImport(
            activeAccountIDs: [selected, unselected],
            configuredCollectionAccountIDs: [selected, inactive]
        )

        #expect(result == [selected])
    }

    @Test("Google readiness requires the current refresh and every selected collection import")
    func currentGoogleImportEvidence() {
        #expect(DayWeaveOnboardingEvidence.googleRefreshIsCurrent(
            refreshGeneration: 3,
            claimedRefreshGeneration: 3,
            completedRefreshGeneration: 3,
            requestedAt: nil,
            completedAt: .distantPast
        ))
        #expect(!DayWeaveOnboardingEvidence.googleRefreshIsCurrent(
            refreshGeneration: 3,
            claimedRefreshGeneration: 3,
            completedRefreshGeneration: 2,
            requestedAt: nil,
            completedAt: .distantPast
        ))
        #expect(!DayWeaveOnboardingEvidence.googleRefreshIsCurrent(
            refreshGeneration: 3,
            claimedRefreshGeneration: 3,
            completedRefreshGeneration: 3,
            requestedAt: .distantPast,
            completedAt: .distantPast
        ))

        let configuredAt = Date(timeIntervalSince1970: 1_800_000_000)
        let completedAt = configuredAt.addingTimeInterval(60)
        #expect(DayWeaveOnboardingEvidence.googleCollectionImportIsCurrent(
            configuredAt: configuredAt,
            lastImportAt: configuredAt,
            completedAt: completedAt
        ))
        #expect(!DayWeaveOnboardingEvidence.googleCollectionImportIsCurrent(
            configuredAt: configuredAt,
            lastImportAt: configuredAt.addingTimeInterval(-1),
            completedAt: completedAt
        ))
        #expect(!DayWeaveOnboardingEvidence.googleCollectionImportIsCurrent(
            configuredAt: configuredAt,
            lastImportAt: completedAt.addingTimeInterval(1),
            completedAt: completedAt
        ))
        #expect(!DayWeaveOnboardingEvidence.googleCollectionImportIsCurrent(
            configuredAt: configuredAt,
            lastImportAt: nil,
            completedAt: completedAt
        ))
    }

    @Test("first-plan composition rechecks every prerequisite in setup order")
    func firstPlanPrerequisites() {
        #expect(DayWeaveOnboardingEvidence.firstPlanPrerequisiteFailure(
            apiConnection: .ready("API"),
            googleResources: .ready("Google"),
            scheduleProfile: .ready("Profile"),
            notifications: .ready("Notifications"),
            firstItem: .ready("Item")
        ) == nil)

        let googleFailure = DayWeaveOnboardingEvidence.firstPlanPrerequisiteFailure(
            apiConnection: .ready("API"),
            googleResources: .working("Importing"),
            scheduleProfile: .ready("Profile"),
            notifications: .ready("Notifications"),
            firstItem: .ready("Item")
        )
        #expect(googleFailure?.step == .googleResources)
        #expect(googleFailure?.check == .working("Importing"))

        let earliestFailure = DayWeaveOnboardingEvidence.firstPlanPrerequisiteFailure(
            apiConnection: .blocked("Expired"),
            googleResources: .blocked("Import failed"),
            scheduleProfile: .ready("Profile"),
            notifications: .ready("Notifications"),
            firstItem: .ready("Item")
        )
        #expect(earliestFailure?.step == .apiConnection)
    }
}
#endif
