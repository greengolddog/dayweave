import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("macOS onboarding state machine")
@MainActor
struct DayWeaveOnboardingControllerTests {
    @Test("version-one progress migrates to the resumable version-two shape")
    func legacyProgressMigration() throws {
        let data = Data(
            #"{"version":1,"current_page":"schedule_profile","privacy_acknowledged":true,"completed":false}"#.utf8
        )

        let migrated = try JSONDecoder().decode(
            DayWeaveOnboardingProgress.self,
            from: data
        )

        #expect(migrated.version == DayWeaveOnboardingProgress.currentVersion)
        #expect(migrated.currentStep == .scheduleProfile)
        #expect(migrated.furthestReachedStep == .scheduleProfile)
        #expect(migrated.privacyAcknowledged)
        #expect(!migrated.completed)
        #expect(migrated.hasValidShape)
    }

    @Test("the exact presented step and furthest visit resume after relaunch")
    func resumeCurrentAndFurthestStep() throws {
        let store = TestDayWeaveOnboardingProgressStore()
        let first = DayWeaveOnboardingController(store: store)

        first.setPrivacyAcknowledged(true)
        #expect(first.advance(using: .pending))
        var readiness = DayWeaveOnboardingReadiness.pending
        readiness.apiConnection = .ready("Connected")
        #expect(first.advance(using: readiness))
        #expect(first.currentStep == .googleResources)

        first.navigate(to: .welcomePrivacy)
        #expect(first.currentStep == .welcomePrivacy)
        #expect(first.progress.furthestReachedStep == .googleResources)

        let resumed = DayWeaveOnboardingController(store: store)
        #expect(resumed.currentStep == .welcomePrivacy)
        #expect(resumed.progress.furthestReachedStep == .googleResources)
        #expect(resumed.progress.privacyAcknowledged)
        #expect(resumed.canNavigate(to: .apiConnection))
        #expect(resumed.canNavigate(to: .googleResources))
        #expect(!resumed.canNavigate(to: .scheduleProfile))

        resumed.setPrivacyAcknowledged(false)
        #expect(resumed.progress.privacyAcknowledged)
    }

    @Test("every page waits for live readiness and completion rechecks all milestones")
    func stepGating() {
        let store = TestDayWeaveOnboardingProgressStore()
        let controller = DayWeaveOnboardingController(store: store)
        var readiness = DayWeaveOnboardingReadiness.pending

        #expect(!controller.canAdvance(using: readiness))
        #expect(!controller.advance(using: readiness))
        #expect(controller.currentStep == .welcomePrivacy)

        controller.setPrivacyAcknowledged(true)
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .apiConnection)
        #expect(!controller.advance(using: readiness))

        readiness.apiConnection = .working("Checking")
        #expect(!controller.advance(using: readiness))
        readiness.apiConnection = .ready("Connected")
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .googleResources)

        readiness.googleResources = .ready("Selected")
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .scheduleProfile)

        readiness.scheduleProfile = .blocked("Fix the profile")
        #expect(!controller.advance(using: readiness))
        readiness.scheduleProfile = .ready("Profile saved")
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .notifications)

        readiness.notifications = .ready("Choice recorded")
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .firstItem)

        readiness.firstItem = .ready("Item stored")
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .firstPlan)

        readiness.firstPlan = .ready("Plan composed")
        #expect(controller.advance(using: readiness))
        #expect(controller.currentStep == .completion)

        readiness.apiConnection = .blocked("Session expired")
        #expect(!controller.finish(using: readiness))
        #expect(!controller.isComplete)
        #expect(controller.blockingReason(using: readiness)?.contains("DayWeave API") == true)

        readiness.apiConnection = .ready("Connected")
        #expect(controller.finish(using: readiness))
        #expect(controller.isComplete)
        #expect(store.stored?.completed == true)
    }

    @Test("failed persistence never presents an uncommitted step")
    func persistenceFailureDoesNotAdvance() {
        let store = TestDayWeaveOnboardingProgressStore(saveError: .writeFailed)
        let controller = DayWeaveOnboardingController(store: store)

        controller.setPrivacyAcknowledged(true)

        #expect(!controller.progress.privacyAcknowledged)
        #expect(controller.currentStep == .welcomePrivacy)
        #expect(controller.persistenceMessage != nil)
        #expect(store.saved.isEmpty)
    }

    @Test("set up later is process-local and never creates false completion")
    func dismissalAndResume() throws {
        let store = TestDayWeaveOnboardingProgressStore()
        let controller = DayWeaveOnboardingController(store: store)

        #expect(controller.isPresented)
        controller.dismiss()
        #expect(!controller.isPresented)
        #expect(!controller.isComplete)
        #expect(store.saved.isEmpty)

        controller.present()
        #expect(controller.isPresented)

        let relaunched = DayWeaveOnboardingController(store: store)
        #expect(relaunched.isPresented)
        #expect(!relaunched.isComplete)

        let completed = try DayWeaveOnboardingProgress(
            currentStep: .completion,
            furthestReachedStep: .completion,
            privacyAcknowledged: true,
            completed: true
        )
        let completedController = DayWeaveOnboardingController(
            store: TestDayWeaveOnboardingProgressStore(stored: completed)
        )
        #expect(!completedController.isPresented)
        completedController.present()
        #expect(!completedController.isPresented)
    }

    @Test("UserDefaults retains only the closed milestone record, never injected details")
    func userDefaultsRecordContainsNoInjectedData() throws {
        let suiteName = "com.greengolddog.dayweave.tests.onboarding.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let key = "test-onboarding"
        let store = UserDefaultsDayWeaveOnboardingProgressStore(
            defaults: defaults,
            key: key
        )
        let controller = DayWeaveOnboardingController(store: store)
        let canary = "dw_secret_canary_https://private.example.invalid"

        controller.setPrivacyAcknowledged(true)
        #expect(controller.advance(using: .pending))
        var readiness = DayWeaveOnboardingReadiness.pending
        readiness.apiConnection = .ready(canary)
        #expect(controller.advance(using: readiness))

        let data = try #require(defaults.data(forKey: key))
        let encoded = try #require(String(data: data, encoding: .utf8))
        let object = try #require(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )

        #expect(!encoded.contains(canary))
        #expect(Set(object.keys) == [
            "completed",
            "current_step",
            "furthest_reached_step",
            "privacy_acknowledged",
            "version",
        ])
        #expect(try store.load() == controller.progress)
    }

    @Test("the live store migrates the legacy key before removing it")
    func userDefaultsLegacyKeyMigration() throws {
        let suiteName = "com.greengolddog.dayweave.tests.onboarding-migration.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let currentKey = "test-onboarding-v2"
        let legacyKey = "test-onboarding-v1"
        defaults.set(
            Data(
                #"{"version":1,"current_page":"google_resources","privacy_acknowledged":true,"completed":false}"#.utf8
            ),
            forKey: legacyKey
        )
        let store = UserDefaultsDayWeaveOnboardingProgressStore(
            defaults: defaults,
            key: currentKey,
            legacyKey: legacyKey
        )

        let migrated = try #require(try store.load())

        #expect(migrated.version == DayWeaveOnboardingProgress.currentVersion)
        #expect(migrated.currentStep == .googleResources)
        #expect(defaults.data(forKey: currentKey) != nil)
        #expect(defaults.data(forKey: legacyKey) == nil)
    }

    @Test("unreadable progress is not overwritten without explicit recovery")
    func corruptProgressRequiresExplicitReset() {
        let store = TestDayWeaveOnboardingProgressStore(
            loadError: .unsupportedVersion(99)
        )
        let controller = DayWeaveOnboardingController(store: store)

        #expect(controller.persistenceRecoveryRequired)
        #expect(controller.persistenceMessage != nil)
        controller.setPrivacyAcknowledged(true)
        #expect(!controller.progress.privacyAcknowledged)
        #expect(store.saved.isEmpty)

        controller.resetProgressAfterWarning()
        #expect(!controller.persistenceRecoveryRequired)
        #expect(controller.progress == .fresh)
        #expect(store.saved == [.fresh])
        controller.setPrivacyAcknowledged(true)
        #expect(controller.progress.privacyAcknowledged)
    }

    @Test("unknown or expanded durable shapes fail closed")
    func unsupportedAndExpandedRecordsFailClosed() throws {
        let future = Data(
            #"{"version":99,"current_step":"welcome_privacy","furthest_reached_step":"welcome_privacy","privacy_acknowledged":false,"completed":false}"#.utf8
        )
        #expect(throws: DayWeaveOnboardingProgressError.unsupportedVersion(99)) {
            try JSONDecoder().decode(DayWeaveOnboardingProgress.self, from: future)
        }

        let expanded = Data(
            #"{"version":2,"current_step":"welcome_privacy","furthest_reached_step":"welcome_privacy","privacy_acknowledged":false,"completed":false,"token":"must-not-exist"}"#.utf8
        )
        #expect(throws: DayWeaveOnboardingProgressError.malformed) {
            try JSONDecoder().decode(DayWeaveOnboardingProgress.self, from: expanded)
        }
    }
}
#endif

@MainActor
private final class TestDayWeaveOnboardingProgressStore:
    DayWeaveOnboardingProgressStoring
{
    var stored: DayWeaveOnboardingProgress?
    let loadError: DayWeaveOnboardingProgressStoreError?
    let saveError: DayWeaveOnboardingProgressStoreError?
    private(set) var saved: [DayWeaveOnboardingProgress] = []

    init(
        stored: DayWeaveOnboardingProgress? = nil,
        loadError: DayWeaveOnboardingProgressStoreError? = nil,
        saveError: DayWeaveOnboardingProgressStoreError? = nil
    ) {
        self.stored = stored
        self.loadError = loadError
        self.saveError = saveError
    }

    func load() throws -> DayWeaveOnboardingProgress? {
        if let loadError { throw loadError }
        return stored
    }

    func save(_ progress: DayWeaveOnboardingProgress) throws {
        if let saveError { throw saveError }
        stored = progress
        saved.append(progress)
    }
}
