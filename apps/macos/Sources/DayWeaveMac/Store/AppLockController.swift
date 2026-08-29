import Combine
import Foundation
import LocalAuthentication

enum AppLockTimeout: Int, CaseIterable, Codable, Identifiable, Sendable {
    case immediately = 0
    case oneMinute = 60
    case fiveMinutes = 300
    case fifteenMinutes = 900
    case oneHour = 3_600

    var id: Int { rawValue }

    var title: String {
        switch self {
        case .immediately:
            "Immediately"
        case .oneMinute:
            "After 1 minute"
        case .fiveMinutes:
            "After 5 minutes"
        case .fifteenMinutes:
            "After 15 minutes"
        case .oneHour:
            "After 1 hour"
        }
    }
}

struct AppLockPreferences: Codable, Equatable, Sendable {
    static let currentVersion = 1

    let version: Int
    var isEnabled: Bool
    var timeout: AppLockTimeout

    init(isEnabled: Bool, timeout: AppLockTimeout) {
        version = Self.currentVersion
        self.isEnabled = isEnabled
        self.timeout = timeout
    }
}

enum AppLockPreferencesStoreError: Error, Equatable, Sendable {
    case invalidStoredPreferences
    case unsupportedVersion(Int)
    case writeFailed
}

@MainActor
protocol AppLockPreferencesStoring {
    func load() throws -> AppLockPreferences?
    func save(_ preferences: AppLockPreferences) throws
}

@MainActor
final class UserDefaultsAppLockPreferencesStore: AppLockPreferencesStoring {
    static let defaultKey = "dayweave.app-lock.preferences-v1"

    private let defaults: UserDefaults
    private let key: String

    init(
        defaults: UserDefaults = .standard,
        key: String = UserDefaultsAppLockPreferencesStore.defaultKey
    ) {
        self.defaults = defaults
        self.key = key
    }

    func load() throws -> AppLockPreferences? {
        guard let data = defaults.data(forKey: key) else { return nil }
        let preferences: AppLockPreferences
        do {
            preferences = try JSONDecoder().decode(AppLockPreferences.self, from: data)
        } catch {
            throw AppLockPreferencesStoreError.invalidStoredPreferences
        }
        guard preferences.version == AppLockPreferences.currentVersion else {
            throw AppLockPreferencesStoreError.unsupportedVersion(preferences.version)
        }
        return preferences
    }

    func save(_ preferences: AppLockPreferences) throws {
        guard preferences.version == AppLockPreferences.currentVersion else {
            throw AppLockPreferencesStoreError.unsupportedVersion(preferences.version)
        }
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            defaults.set(try encoder.encode(preferences), forKey: key)
        } catch {
            throw AppLockPreferencesStoreError.writeFailed
        }
    }
}

enum AppLockAuthenticationResult: Equatable, Sendable {
    case succeeded
    case cancelled
    case failed
    case unavailable
}

@MainActor
protocol AppLockAuthenticating: AnyObject {
    func authenticate(reason: String) async -> AppLockAuthenticationResult
    func cancel()
}

@MainActor
final class LocalDeviceOwnerAuthenticator: AppLockAuthenticating {
    private var context: LAContext?

    func authenticate(reason: String) async -> AppLockAuthenticationResult {
        cancel()

        let context = LAContext()
        context.localizedCancelTitle = "Cancel"
        var availabilityError: NSError?
        guard context.canEvaluatePolicy(
            .deviceOwnerAuthentication,
            error: &availabilityError
        ) else {
            return .unavailable
        }

        self.context = context
        defer {
            if self.context === context {
                self.context = nil
            }
        }

        do {
            return try await context.evaluatePolicy(
                .deviceOwnerAuthentication,
                localizedReason: reason
            ) ? .succeeded : .failed
        } catch let error as LAError {
            switch error.code {
            case .userCancel, .appCancel, .systemCancel:
                return .cancelled
            case .biometryNotAvailable, .passcodeNotSet:
                return .unavailable
            default:
                return .failed
            }
        } catch {
            return .failed
        }
    }

    func cancel() {
        context?.invalidate()
        context = nil
    }
}

enum AppLockAccessState: Equatable, Sendable {
    case disabled
    case locked
    case unlocked
}

@MainActor
final class AppLockController: ObservableObject {
    typealias NowProvider = @MainActor () -> Date
    typealias SleepProvider = @Sendable (Duration) async throws -> Void

    @Published private(set) var preferences: AppLockPreferences
    @Published private(set) var accessState: AppLockAccessState
    @Published private(set) var isAuthenticating = false
    @Published private(set) var statusMessage: String?

    var isContentAvailable: Bool {
        accessState == .disabled || accessState == .unlocked
    }

    private let preferencesStore: any AppLockPreferencesStoring
    private let authenticator: any AppLockAuthenticating
    private let now: NowProvider
    private let sleep: SleepProvider
    private var isApplicationActive: Bool
    private var inactiveAt: Date?
    private var lifecycleGeneration = 0
    private var authenticationGeneration = 0
    private var backgroundLockTask: Task<Void, Never>?

    static func live() -> AppLockController {
        AppLockController(
            preferencesStore: UserDefaultsAppLockPreferencesStore(),
            authenticator: LocalDeviceOwnerAuthenticator(),
            initiallyActive: false
        )
    }

    init(
        preferencesStore: any AppLockPreferencesStoring,
        authenticator: any AppLockAuthenticating,
        initiallyActive: Bool = true,
        now: @escaping NowProvider = Date.init,
        sleep: @escaping SleepProvider = { duration in
            try await Task.sleep(for: duration)
        }
    ) {
        self.preferencesStore = preferencesStore
        self.authenticator = authenticator
        self.now = now
        self.sleep = sleep
        isApplicationActive = initiallyActive

        do {
            let stored = try preferencesStore.load()
            let preferences = stored ?? AppLockPreferences(
                isEnabled: false,
                timeout: .fiveMinutes
            )
            self.preferences = preferences
            accessState = preferences.isEnabled ? .locked : .disabled
        } catch {
            // Existing but unreadable settings might previously have enabled
            // the lock. Treat corruption as enabled and require device-owner
            // authentication before settings or planner content are visible.
            preferences = AppLockPreferences(isEnabled: true, timeout: .immediately)
            accessState = .locked
            statusMessage = "App-lock settings need recovery. Unlock, then save your preference again."
        }
    }

    deinit {
        backgroundLockTask?.cancel()
    }

    @discardableResult
    func unlock() async -> Bool {
        guard preferences.isEnabled else {
            accessState = .disabled
            return true
        }
        guard isApplicationActive else {
            statusMessage = "Bring DayWeave to the foreground before unlocking."
            return false
        }
        guard accessState == .locked, !isAuthenticating else { return false }

        let generation = beginAuthentication()
        let result = await authenticator.authenticate(
            reason: "Unlock your DayWeave schedule and account settings."
        )
        guard authenticationIsCurrent(generation) else { return false }
        finishAuthentication()

        switch result {
        case .succeeded:
            guard isApplicationActive else {
                accessState = .locked
                statusMessage = "DayWeave stayed locked because the app was no longer active."
                return false
            }
            accessState = .unlocked
            inactiveAt = nil
            statusMessage = nil
            return true
        case .cancelled:
            accessState = .locked
            statusMessage = "Authentication was canceled."
        case .failed:
            accessState = .locked
            statusMessage = "Authentication failed. Try Touch ID or your Mac login password again."
        case .unavailable:
            accessState = .locked
            statusMessage = "Device-owner authentication is unavailable. Configure Touch ID or a Mac login password, then try again."
        }
        return false
    }

    @discardableResult
    func setEnabled(_ enabled: Bool) async -> Bool {
        guard enabled != preferences.isEnabled else { return true }
        guard isApplicationActive else {
            statusMessage = "Bring DayWeave to the foreground before changing app-lock protection."
            return false
        }
        guard !isAuthenticating else { return false }

        let generation = beginAuthentication()
        let reason = enabled
            ? "Turn on DayWeave app lock."
            : "Turn off DayWeave app lock."
        let result = await authenticator.authenticate(reason: reason)
        guard authenticationIsCurrent(generation) else { return false }
        finishAuthentication()

        guard result == .succeeded else {
            statusMessage = authenticationFailureMessage(result)
            return false
        }

        var replacement = preferences
        replacement.isEnabled = enabled
        do {
            try preferencesStore.save(replacement)
        } catch {
            statusMessage = "The app-lock preference could not be saved. Nothing changed."
            return false
        }

        preferences = replacement
        accessState = enabled ? .unlocked : .disabled
        inactiveAt = nil
        backgroundLockTask?.cancel()
        backgroundLockTask = nil
        statusMessage = nil
        return true
    }

    @discardableResult
    func setTimeout(_ timeout: AppLockTimeout) -> Bool {
        guard timeout != preferences.timeout else { return true }
        var replacement = preferences
        replacement.timeout = timeout
        do {
            try preferencesStore.save(replacement)
        } catch {
            statusMessage = "The automatic-lock timing could not be saved."
            return false
        }
        preferences = replacement
        statusMessage = nil

        if preferences.isEnabled, !isApplicationActive, accessState == .unlocked {
            scheduleBackgroundLock()
        }
        return true
    }

    func lockNow() {
        guard preferences.isEnabled else { return }
        cancelAuthentication()
        backgroundLockTask?.cancel()
        backgroundLockTask = nil
        inactiveAt = nil
        accessState = .locked
        statusMessage = nil
    }

    func applicationBecameInactive(at date: Date? = nil) {
        guard isApplicationActive || inactiveAt == nil else { return }
        isApplicationActive = false
        let transitionDate = date ?? now()
        inactiveAt = inactiveAt ?? transitionDate
        cancelAuthentication()
        guard preferences.isEnabled, accessState == .unlocked else { return }
        scheduleBackgroundLock()
    }

    func applicationBecameActive(at date: Date? = nil) {
        isApplicationActive = true
        backgroundLockTask?.cancel()
        backgroundLockTask = nil

        guard preferences.isEnabled, accessState == .unlocked,
              let inactiveAt else {
            self.inactiveAt = nil
            return
        }

        let elapsed = max(0, (date ?? now()).timeIntervalSince(inactiveAt))
        self.inactiveAt = nil
        if elapsed >= TimeInterval(preferences.timeout.rawValue) {
            accessState = .locked
            statusMessage = nil
        }
    }

    private func scheduleBackgroundLock() {
        lifecycleGeneration += 1
        let generation = lifecycleGeneration
        backgroundLockTask?.cancel()

        let timeout = preferences.timeout
        if timeout == .immediately {
            accessState = .locked
            statusMessage = nil
            return
        }

        let elapsed = inactiveAt.map { max(0, now().timeIntervalSince($0)) } ?? 0
        let remaining = max(0, TimeInterval(timeout.rawValue) - elapsed)
        if remaining == 0 {
            accessState = .locked
            statusMessage = nil
            return
        }

        let sleep = self.sleep
        backgroundLockTask = Task { @MainActor [weak self] in
            do {
                try await sleep(.seconds(remaining))
            } catch {
                return
            }
            guard let self,
                  generation == lifecycleGeneration,
                  !isApplicationActive,
                  preferences.isEnabled,
                  accessState == .unlocked else { return }
            accessState = .locked
            statusMessage = nil
            backgroundLockTask = nil
        }
    }

    private func beginAuthentication() -> Int {
        authenticationGeneration += 1
        isAuthenticating = true
        statusMessage = nil
        return authenticationGeneration
    }

    private func authenticationIsCurrent(_ generation: Int) -> Bool {
        generation == authenticationGeneration && isAuthenticating
    }

    private func finishAuthentication() {
        isAuthenticating = false
    }

    private func cancelAuthentication() {
        guard isAuthenticating else { return }
        authenticationGeneration += 1
        isAuthenticating = false
        authenticator.cancel()
    }

    private func authenticationFailureMessage(_ result: AppLockAuthenticationResult) -> String {
        switch result {
        case .cancelled:
            "Authentication was canceled."
        case .failed:
            "Authentication failed. The app-lock preference was not changed."
        case .unavailable:
            "Device-owner authentication is unavailable, so the app-lock preference was not changed."
        case .succeeded:
            "Authentication did not complete."
        }
    }
}
