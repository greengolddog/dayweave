import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("macOS app lock")
@MainActor
struct AppLockControllerTests {
    @Test("a new profile starts unlocked with app lock disabled")
    func newProfileDefaultsToDisabled() {
        let controller = makeController(store: TestAppLockPreferencesStore())

        #expect(controller.preferences == AppLockPreferences(
            isEnabled: false,
            timeout: .fiveMinutes
        ))
        #expect(controller.accessState == .disabled)
        #expect(controller.isContentAvailable)
    }

    @Test("an enabled profile fails closed on every cold launch")
    func enabledProfileStartsLocked() {
        let controller = makeController(store: TestAppLockPreferencesStore(
            stored: AppLockPreferences(isEnabled: true, timeout: .fifteenMinutes)
        ))

        #expect(controller.accessState == .locked)
        #expect(!controller.isContentAvailable)
    }

    @Test("corrupted existing preferences fail closed and remain recoverable")
    func corruptedPreferencesFailClosed() async {
        let store = TestAppLockPreferencesStore(loadError: .invalidStoredPreferences)
        let authenticator = TestAppLockAuthenticator(results: [.succeeded, .succeeded])
        let controller = makeController(store: store, authenticator: authenticator)

        #expect(controller.accessState == .locked)
        #expect(controller.preferences.isEnabled)
        #expect(controller.preferences.timeout == .immediately)
        #expect(await controller.unlock())
        #expect(controller.accessState == .unlocked)
        #expect(await controller.setEnabled(false))
        #expect(store.stored?.isEnabled == false)
        #expect(controller.accessState == .disabled)
    }

    @Test("unlock requires a successful device-owner authentication")
    func unlockRequiresAuthentication() async {
        let authenticator = TestAppLockAuthenticator(results: [.failed, .cancelled, .succeeded])
        let controller = makeController(
            store: TestAppLockPreferencesStore(
                stored: AppLockPreferences(isEnabled: true, timeout: .fiveMinutes)
            ),
            authenticator: authenticator
        )

        #expect(!(await controller.unlock()))
        #expect(controller.accessState == .locked)
        #expect(!(await controller.unlock()))
        #expect(controller.accessState == .locked)
        #expect(await controller.unlock())
        #expect(controller.accessState == .unlocked)
        #expect(authenticator.reasons.count == 3)
    }

    @Test("leaving the app locks at the configured deadline")
    func inactivityDeadlineIsEnforced() async {
        let start = Date(timeIntervalSince1970: 10_000)
        let controller = makeController(
            store: TestAppLockPreferencesStore(
                stored: AppLockPreferences(isEnabled: true, timeout: .fiveMinutes)
            ),
            authenticator: TestAppLockAuthenticator(results: [.succeeded]),
            now: { start }
        )
        #expect(await controller.unlock())

        controller.applicationBecameInactive(at: start)
        controller.applicationBecameActive(at: start.addingTimeInterval(299))
        #expect(controller.accessState == .unlocked)

        controller.applicationBecameInactive(at: start.addingTimeInterval(400))
        controller.applicationBecameActive(at: start.addingTimeInterval(700))
        #expect(controller.accessState == .locked)
        #expect(!controller.isContentAvailable)
    }

    @Test("the content is redacted when a background deadline fires")
    func backgroundDeadlineLocksWithoutReactivation() async {
        let start = Date(timeIntervalSince1970: 20_000)
        let sleepRecorder = TestAppLockSleepRecorder()
        let controller = makeController(
            store: TestAppLockPreferencesStore(
                stored: AppLockPreferences(isEnabled: true, timeout: .oneMinute)
            ),
            authenticator: TestAppLockAuthenticator(results: [.succeeded]),
            now: { start },
            sleep: { duration in await sleepRecorder.record(duration) }
        )
        #expect(await controller.unlock())

        controller.applicationBecameInactive()
        for _ in 0..<10 where controller.accessState != .locked {
            await Task.yield()
        }

        #expect(controller.accessState == .locked)
        #expect(await sleepRecorder.values() == [.seconds(60)])
    }

    @Test("an authentication result cannot unlock after the app became inactive")
    func staleAuthenticationCannotUnlock() async {
        let authenticator = SuspendedAppLockAuthenticator()
        let controller = makeController(
            store: TestAppLockPreferencesStore(
                stored: AppLockPreferences(isEnabled: true, timeout: .fiveMinutes)
            ),
            authenticator: authenticator
        )

        let unlockTask = Task { @MainActor in await controller.unlock() }
        for _ in 0..<10 where authenticator.reasons.isEmpty {
            await Task.yield()
        }
        #expect(authenticator.reasons.count == 1)

        controller.applicationBecameInactive()
        #expect(authenticator.cancelCount == 1)
        authenticator.resolve(.succeeded)

        #expect(!(await unlockTask.value))
        #expect(controller.accessState == .locked)
    }

    @Test("an inactive process cannot start an authentication ceremony")
    func inactiveProcessCannotAuthenticate() async {
        let authenticator = TestAppLockAuthenticator(results: [.succeeded])
        let controller = makeController(
            store: TestAppLockPreferencesStore(
                stored: AppLockPreferences(isEnabled: true, timeout: .fiveMinutes)
            ),
            authenticator: authenticator
        )
        controller.applicationBecameInactive()

        #expect(!(await controller.unlock()))
        #expect(controller.accessState == .locked)
        #expect(authenticator.reasons.isEmpty)
    }

    @Test("enabling and disabling are authenticated and durably ordered")
    func preferenceChangesRequireAuthentication() async {
        let store = TestAppLockPreferencesStore()
        let authenticator = TestAppLockAuthenticator(results: [.succeeded, .succeeded])
        let controller = makeController(store: store, authenticator: authenticator)

        #expect(await controller.setEnabled(true))
        #expect(controller.preferences.isEnabled)
        #expect(controller.accessState == .unlocked)
        #expect(store.savedValues.map(\.isEnabled) == [true])

        #expect(controller.setTimeout(.oneMinute))
        #expect(store.savedValues.map(\.timeout) == [.fiveMinutes, .oneMinute])

        #expect(await controller.setEnabled(false))
        #expect(controller.accessState == .disabled)
        #expect(store.savedValues.map(\.isEnabled) == [true, true, false])
        #expect(authenticator.reasons.count == 2)
    }

    @Test("a failed preference write never changes the live security state")
    func failedWriteDoesNotChangeState() async {
        let store = TestAppLockPreferencesStore(saveError: .writeFailed)
        let controller = makeController(
            store: store,
            authenticator: TestAppLockAuthenticator(results: [.succeeded])
        )

        #expect(!(await controller.setEnabled(true)))
        #expect(!controller.preferences.isEnabled)
        #expect(controller.accessState == .disabled)
        #expect(controller.statusMessage != nil)
    }

    @Test("the UserDefaults record is versioned and rejects malformed bytes")
    func userDefaultsStoreRoundTripsAndRejectsMalformedBytes() throws {
        let suiteName = "com.greengolddog.dayweave.tests.app-lock.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }
        let key = "test-preferences"
        let store = UserDefaultsAppLockPreferencesStore(defaults: defaults, key: key)
        let preferences = AppLockPreferences(isEnabled: true, timeout: .oneHour)

        try store.save(preferences)
        #expect(try store.load() == preferences)

        defaults.set(Data("not-json".utf8), forKey: key)
        #expect(throws: AppLockPreferencesStoreError.invalidStoredPreferences) {
            try store.load()
        }
    }

    private func makeController(
        store: TestAppLockPreferencesStore,
        authenticator: any AppLockAuthenticating = TestAppLockAuthenticator(),
        now: @escaping AppLockController.NowProvider = Date.init,
        sleep: @escaping AppLockController.SleepProvider = { duration in
            try await Task.sleep(for: duration)
        }
    ) -> AppLockController {
        AppLockController(
            preferencesStore: store,
            authenticator: authenticator,
            initiallyActive: true,
            now: now,
            sleep: sleep
        )
    }
}
#endif

@MainActor
private final class TestAppLockPreferencesStore: AppLockPreferencesStoring {
    var stored: AppLockPreferences?
    let loadError: AppLockPreferencesStoreError?
    let saveError: AppLockPreferencesStoreError?
    private(set) var savedValues: [AppLockPreferences] = []

    init(
        stored: AppLockPreferences? = nil,
        loadError: AppLockPreferencesStoreError? = nil,
        saveError: AppLockPreferencesStoreError? = nil
    ) {
        self.stored = stored
        self.loadError = loadError
        self.saveError = saveError
    }

    func load() throws -> AppLockPreferences? {
        if let loadError { throw loadError }
        return stored
    }

    func save(_ preferences: AppLockPreferences) throws {
        if let saveError { throw saveError }
        stored = preferences
        savedValues.append(preferences)
    }
}

@MainActor
private final class TestAppLockAuthenticator: AppLockAuthenticating {
    private var results: [AppLockAuthenticationResult]
    private(set) var reasons: [String] = []
    private(set) var cancelCount = 0

    init(results: [AppLockAuthenticationResult] = []) {
        self.results = results
    }

    func authenticate(reason: String) async -> AppLockAuthenticationResult {
        reasons.append(reason)
        return results.isEmpty ? .failed : results.removeFirst()
    }

    func cancel() {
        cancelCount += 1
    }
}

@MainActor
private final class SuspendedAppLockAuthenticator: AppLockAuthenticating {
    private var continuation: CheckedContinuation<AppLockAuthenticationResult, Never>?
    private(set) var reasons: [String] = []
    private(set) var cancelCount = 0

    func authenticate(reason: String) async -> AppLockAuthenticationResult {
        reasons.append(reason)
        return await withCheckedContinuation { continuation = $0 }
    }

    func cancel() {
        cancelCount += 1
    }

    func resolve(_ result: AppLockAuthenticationResult) {
        continuation?.resume(returning: result)
        continuation = nil
    }
}

private actor TestAppLockSleepRecorder {
    private var recorded: [Duration] = []

    func record(_ duration: Duration) {
        recorded.append(duration)
    }

    func values() -> [Duration] {
        recorded
    }
}
