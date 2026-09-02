import Combine
import Foundation

enum DayWeaveOnboardingStep: String, CaseIterable, Codable, Identifiable, Sendable {
    case welcomePrivacy = "welcome_privacy"
    case apiConnection = "api_connection"
    case googleResources = "google_resources"
    case scheduleProfile = "schedule_profile"
    case notifications
    case firstItem = "first_item"
    case firstPlan = "first_plan"
    case completion

    var id: String { rawValue }

    var ordinal: Int {
        Self.allCases.firstIndex(of: self) ?? 0
    }

    var next: Self? {
        let index = ordinal + 1
        guard Self.allCases.indices.contains(index) else { return nil }
        return Self.allCases[index]
    }

    var previous: Self? {
        let index = ordinal - 1
        guard Self.allCases.indices.contains(index) else { return nil }
        return Self.allCases[index]
    }

    var title: String {
        switch self {
        case .welcomePrivacy: "Welcome & privacy"
        case .apiConnection: "DayWeave API"
        case .googleResources: "Google resources"
        case .scheduleProfile: "Schedule profile"
        case .notifications: "Notifications"
        case .firstItem: "First item"
        case .firstPlan: "First plan"
        case .completion: "Ready"
        }
    }

    var symbol: String {
        switch self {
        case .welcomePrivacy: "hand.wave.fill"
        case .apiConnection: "network"
        case .googleResources: "calendar.badge.checkmark"
        case .scheduleProfile: "calendar.badge.clock"
        case .notifications: "bell.badge.fill"
        case .firstItem: "square.and.pencil"
        case .firstPlan: "sparkles"
        case .completion: "checkmark.seal.fill"
        }
    }
}

struct DayWeaveOnboardingCheck: Equatable, Sendable {
    enum State: String, Equatable, Sendable {
        case pending
        case working
        case ready
        case blocked
    }

    let state: State
    let detail: String

    var isReady: Bool { state == .ready }

    static func pending(_ detail: String) -> Self {
        .init(state: .pending, detail: detail)
    }

    static func working(_ detail: String) -> Self {
        .init(state: .working, detail: detail)
    }

    static func ready(_ detail: String) -> Self {
        .init(state: .ready, detail: detail)
    }

    static func blocked(_ detail: String) -> Self {
        .init(state: .blocked, detail: detail)
    }
}

/// Live, non-authoritative setup evidence supplied by the application shell.
/// Nothing in this value is persisted by the onboarding state machine.
struct DayWeaveOnboardingReadiness: Equatable, Sendable {
    var apiConnection: DayWeaveOnboardingCheck
    var googleResources: DayWeaveOnboardingCheck
    var scheduleProfile: DayWeaveOnboardingCheck
    var notifications: DayWeaveOnboardingCheck
    var firstItem: DayWeaveOnboardingCheck
    var firstPlan: DayWeaveOnboardingCheck

    static let pending = Self(
        apiConnection: .pending("Connect this Mac to the private DayWeave API."),
        googleResources: .pending("Choose the Calendar and Tasks sources DayWeave may use."),
        scheduleProfile: .pending("Review availability, sleep, and protected time."),
        notifications: .pending("Choose how DayWeave may remind you."),
        firstItem: .pending("Capture one item to plan."),
        firstPlan: .pending("Compose the first seven-day plan.")
    )

    static let ready = Self(
        apiConnection: .ready("This Mac has an active DayWeave session."),
        googleResources: .ready("Google resource choices are saved."),
        scheduleProfile: .ready("The schedule profile is ready."),
        notifications: .ready("A notification choice has been recorded."),
        firstItem: .ready("The first item is safely stored."),
        firstPlan: .ready("The first plan is available.")
    )

    func check(for step: DayWeaveOnboardingStep) -> DayWeaveOnboardingCheck? {
        switch step {
        case .welcomePrivacy, .completion: nil
        case .apiConnection: apiConnection
        case .googleResources: googleResources
        case .scheduleProfile: scheduleProfile
        case .notifications: notifications
        case .firstItem: firstItem
        case .firstPlan: firstPlan
        }
    }

    var firstIncompleteStep: DayWeaveOnboardingStep? {
        DayWeaveOnboardingStep.allCases.first { step in
            check(for: step).map { !$0.isReady } ?? false
        }
    }
}

enum DayWeaveOnboardingProgressError: Error, Equatable, Sendable {
    case malformed
    case unsupportedVersion(Int)
}

/// The complete durable onboarding record. Its closed shape deliberately has no
/// free-form text, URL, account identifier, token, item content, or credential
/// field, so it is safe to keep in UserDefaults.
struct DayWeaveOnboardingProgress: Codable, Equatable, Sendable {
    static let currentVersion = 2

    let version: Int
    let currentStep: DayWeaveOnboardingStep
    let furthestReachedStep: DayWeaveOnboardingStep
    let privacyAcknowledged: Bool
    let completed: Bool

    init(
        currentStep: DayWeaveOnboardingStep = .welcomePrivacy,
        furthestReachedStep: DayWeaveOnboardingStep = .welcomePrivacy,
        privacyAcknowledged: Bool = false,
        completed: Bool = false
    ) throws {
        version = Self.currentVersion
        self.currentStep = currentStep
        self.furthestReachedStep = furthestReachedStep
        self.privacyAcknowledged = privacyAcknowledged
        self.completed = completed
        guard hasValidShape else { throw DayWeaveOnboardingProgressError.malformed }
    }

    static var fresh: Self {
        // The default literals always satisfy the closed invariant.
        try! Self()
    }

    var hasValidShape: Bool {
        guard version == Self.currentVersion,
              currentStep.ordinal <= furthestReachedStep.ordinal else {
            return false
        }
        if furthestReachedStep != .welcomePrivacy, !privacyAcknowledged {
            return false
        }
        if completed {
            return currentStep == .completion
                && furthestReachedStep == .completion
                && privacyAcknowledged
        }
        return true
    }

    func replacing(
        currentStep: DayWeaveOnboardingStep? = nil,
        furthestReachedStep: DayWeaveOnboardingStep? = nil,
        privacyAcknowledged: Bool? = nil,
        completed: Bool? = nil
    ) throws -> Self {
        try Self(
            currentStep: currentStep ?? self.currentStep,
            furthestReachedStep: furthestReachedStep ?? self.furthestReachedStep,
            privacyAcknowledged: privacyAcknowledged ?? self.privacyAcknowledged,
            completed: completed ?? self.completed
        )
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case currentStep = "current_step"
        case furthestReachedStep = "furthest_reached_step"
        case privacyAcknowledged = "privacy_acknowledged"
        case completed
        case legacyCurrentPage = "current_page"
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: OnboardingDynamicCodingKey.self)
        let keys = Set(dynamic.allKeys.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let storedVersion = try container.decode(Int.self, forKey: .version)

        switch storedVersion {
        case 1:
            guard keys == [
                CodingKeys.version.rawValue,
                CodingKeys.legacyCurrentPage.rawValue,
                CodingKeys.privacyAcknowledged.rawValue,
                CodingKeys.completed.rawValue,
            ] else {
                throw DayWeaveOnboardingProgressError.malformed
            }
            let legacyStep = try container.decode(
                DayWeaveOnboardingStep.self,
                forKey: .legacyCurrentPage
            )
            let acknowledged = try container.decode(Bool.self, forKey: .privacyAcknowledged)
            let wasCompleted = try container.decode(Bool.self, forKey: .completed)
            let migratedStep: DayWeaveOnboardingStep = wasCompleted ? .completion : legacyStep
            try self.init(
                currentStep: migratedStep,
                furthestReachedStep: migratedStep,
                privacyAcknowledged: acknowledged,
                completed: wasCompleted
            )
        case Self.currentVersion:
            guard keys == [
                CodingKeys.version.rawValue,
                CodingKeys.currentStep.rawValue,
                CodingKeys.furthestReachedStep.rawValue,
                CodingKeys.privacyAcknowledged.rawValue,
                CodingKeys.completed.rawValue,
            ] else {
                throw DayWeaveOnboardingProgressError.malformed
            }
            try self.init(
                currentStep: container.decode(
                    DayWeaveOnboardingStep.self,
                    forKey: .currentStep
                ),
                furthestReachedStep: container.decode(
                    DayWeaveOnboardingStep.self,
                    forKey: .furthestReachedStep
                ),
                privacyAcknowledged: container.decode(
                    Bool.self,
                    forKey: .privacyAcknowledged
                ),
                completed: container.decode(Bool.self, forKey: .completed)
            )
        default:
            throw DayWeaveOnboardingProgressError.unsupportedVersion(storedVersion)
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else { throw DayWeaveOnboardingProgressError.malformed }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(Self.currentVersion, forKey: .version)
        try container.encode(currentStep, forKey: .currentStep)
        try container.encode(furthestReachedStep, forKey: .furthestReachedStep)
        try container.encode(privacyAcknowledged, forKey: .privacyAcknowledged)
        try container.encode(completed, forKey: .completed)
    }
}

private struct OnboardingDynamicCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) {
        self.stringValue = stringValue
        intValue = nil
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}

enum DayWeaveOnboardingProgressStoreError: Error, Equatable, Sendable {
    case invalidStoredProgress
    case unsupportedVersion(Int)
    case writeFailed
}

@MainActor
protocol DayWeaveOnboardingProgressStoring {
    func load() throws -> DayWeaveOnboardingProgress?
    func save(_ progress: DayWeaveOnboardingProgress) throws
}

@MainActor
final class UserDefaultsDayWeaveOnboardingProgressStore: DayWeaveOnboardingProgressStoring {
    static let defaultKey = "dayweave.onboarding.progress-v2"
    static let legacyKey = "dayweave.onboarding.progress-v1"
    static let maximumEncodedBytes = 2_048

    private let defaults: UserDefaults
    private let key: String
    private let migrationKey: String?

    init(
        defaults: UserDefaults = .standard,
        key: String = UserDefaultsDayWeaveOnboardingProgressStore.defaultKey,
        legacyKey: String? = nil
    ) {
        self.defaults = defaults
        self.key = key
        migrationKey = legacyKey ?? (key == Self.defaultKey ? Self.legacyKey : nil)
    }

    func load() throws -> DayWeaveOnboardingProgress? {
        if let data = defaults.data(forKey: key) {
            return try decode(data)
        }
        guard let migrationKey,
              let data = defaults.data(forKey: migrationKey) else { return nil }
        let migrated = try decode(data)
        try save(migrated)
        defaults.removeObject(forKey: migrationKey)
        return migrated
    }

    private func decode(_ data: Data) throws -> DayWeaveOnboardingProgress {
        guard data.count <= Self.maximumEncodedBytes else {
            throw DayWeaveOnboardingProgressStoreError.invalidStoredProgress
        }
        do {
            return try JSONDecoder().decode(DayWeaveOnboardingProgress.self, from: data)
        } catch let error as DayWeaveOnboardingProgressError {
            switch error {
            case let .unsupportedVersion(version):
                throw DayWeaveOnboardingProgressStoreError.unsupportedVersion(version)
            case .malformed:
                throw DayWeaveOnboardingProgressStoreError.invalidStoredProgress
            }
        } catch {
            throw DayWeaveOnboardingProgressStoreError.invalidStoredProgress
        }
    }

    func save(_ progress: DayWeaveOnboardingProgress) throws {
        guard progress.version == DayWeaveOnboardingProgress.currentVersion,
              progress.hasValidShape else {
            throw DayWeaveOnboardingProgressStoreError.invalidStoredProgress
        }
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let data = try encoder.encode(progress)
            guard data.count <= Self.maximumEncodedBytes else {
                throw DayWeaveOnboardingProgressStoreError.writeFailed
            }
            defaults.set(data, forKey: key)
        } catch let error as DayWeaveOnboardingProgressStoreError {
            throw error
        } catch {
            throw DayWeaveOnboardingProgressStoreError.writeFailed
        }
    }
}

@MainActor
final class DayWeaveOnboardingController: ObservableObject {
    @Published private(set) var progress: DayWeaveOnboardingProgress
    @Published private(set) var persistenceMessage: String?
    @Published private(set) var persistenceRecoveryRequired = false
    /// Process-local presentation state. Dismissing setup never weakens or
    /// completes its durable milestone record, and an unfinished relaunch
    /// offers the flow again.
    @Published private(set) var isPresented = true

    private let store: any DayWeaveOnboardingProgressStoring

    static func live() -> DayWeaveOnboardingController {
        DayWeaveOnboardingController(
            store: UserDefaultsDayWeaveOnboardingProgressStore()
        )
    }

    init(store: any DayWeaveOnboardingProgressStoring) {
        self.store = store
        do {
            progress = try store.load() ?? .fresh
        } catch {
            progress = .fresh
            persistenceRecoveryRequired = true
            persistenceMessage = "Saved onboarding progress could not be read. It was left untouched; explicitly reset only setup checkpoints to continue."
        }
        isPresented = !progress.completed
    }

    var currentStep: DayWeaveOnboardingStep { progress.currentStep }
    var isComplete: Bool { progress.completed }
    var canGoBack: Bool { !progress.completed && currentStep.previous != nil }

    func present() {
        guard !progress.completed else { return }
        isPresented = true
    }

    func dismiss() {
        isPresented = false
    }

    func resetProgressAfterWarning() {
        guard persistenceRecoveryRequired else { return }
        do {
            try store.save(.fresh)
            progress = .fresh
            persistenceRecoveryRequired = false
            persistenceMessage = nil
            isPresented = true
        } catch {
            persistenceMessage = "Onboarding progress could not be reset. Planner data and credentials were not changed."
        }
    }

    func canNavigate(to step: DayWeaveOnboardingStep) -> Bool {
        !progress.completed && step.ordinal <= progress.furthestReachedStep.ordinal
    }

    func setPrivacyAcknowledged(_ acknowledged: Bool) {
        guard !progress.completed else { return }
        guard acknowledged || progress.furthestReachedStep == .welcomePrivacy else { return }
        guard progress.privacyAcknowledged != acknowledged else { return }
        do {
            try commit(progress.replacing(privacyAcknowledged: acknowledged))
        } catch {
            reportPersistenceFailure()
        }
    }

    func blockingReason(using readiness: DayWeaveOnboardingReadiness) -> String? {
        switch currentStep {
        case .welcomePrivacy:
            return progress.privacyAcknowledged
                ? nil
                : "Acknowledge the privacy summary before continuing."
        case .completion:
            guard progress.privacyAcknowledged else {
                return "Review and acknowledge the privacy summary."
            }
            if let incomplete = readiness.firstIncompleteStep {
                return "\(incomplete.title) is not ready. Finish it before completing setup."
            }
            return nil
        default:
            guard let check = readiness.check(for: currentStep), !check.isReady else {
                return nil
            }
            return check.detail
        }
    }

    func canAdvance(using readiness: DayWeaveOnboardingReadiness) -> Bool {
        !progress.completed && blockingReason(using: readiness) == nil
    }

    @discardableResult
    func advance(using readiness: DayWeaveOnboardingReadiness) -> Bool {
        guard canAdvance(using: readiness) else { return false }
        if currentStep == .completion {
            return finish(using: readiness)
        }
        guard let next = currentStep.next else { return false }
        let furthest = next.ordinal > progress.furthestReachedStep.ordinal
            ? next
            : progress.furthestReachedStep
        do {
            try commit(progress.replacing(
                currentStep: next,
                furthestReachedStep: furthest
            ))
            return true
        } catch {
            reportPersistenceFailure()
            return false
        }
    }

    func goBack() {
        guard canGoBack, let previous = currentStep.previous else { return }
        navigate(to: previous)
    }

    func navigate(to step: DayWeaveOnboardingStep) {
        guard canNavigate(to: step), step != currentStep else { return }
        do {
            try commit(progress.replacing(currentStep: step))
        } catch {
            reportPersistenceFailure()
        }
    }

    @discardableResult
    func finish(using readiness: DayWeaveOnboardingReadiness) -> Bool {
        guard currentStep == .completion,
              canAdvance(using: readiness) else { return false }
        do {
            try commit(progress.replacing(completed: true))
            return true
        } catch {
            reportPersistenceFailure()
            return false
        }
    }

    private func commit(_ replacement: DayWeaveOnboardingProgress) throws {
        guard !persistenceRecoveryRequired else {
            throw DayWeaveOnboardingProgressStoreError.writeFailed
        }
        try store.save(replacement)
        progress = replacement
        if replacement.completed { isPresented = false }
        persistenceMessage = nil
    }

    private func reportPersistenceFailure() {
        if persistenceRecoveryRequired { return }
        persistenceMessage = "Onboarding progress could not be saved. Nothing advanced; retry after local storage is available."
    }
}
