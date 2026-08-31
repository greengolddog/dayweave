import AppKit
import Combine
import CryptoKit
import Foundation
import UserNotifications

struct DayWeaveBreakNotificationRequest: Equatable, Sendable {
    let identifier: String
    let title: String
    let body: String
    let fireDate: Date
}

enum DayWeaveNotificationAuthorizationState: Equatable, Sendable {
    case notDetermined
    case authorized
    case denied
}

protocol DayWeaveBreakNotificationCenter: Sendable {
    func authorizationState() async -> DayWeaveNotificationAuthorizationState
    func requestAuthorization() async throws -> Bool
    func pendingRequestIdentifiers() async -> [String]
    func deliveredRequestIdentifiers() async -> [String]
    func add(_ request: DayWeaveBreakNotificationRequest) async throws
    func removePendingRequestIdentifiers(_ identifiers: [String]) async
    func removeDeliveredRequestIdentifiers(_ identifiers: [String]) async
}

struct DayWeaveUNBreakNotificationCenter: DayWeaveBreakNotificationCenter, @unchecked Sendable {
    private let center: UNUserNotificationCenter

    init(center: UNUserNotificationCenter = .current()) {
        self.center = center
    }

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        switch await center.notificationSettings().authorizationStatus {
        case .notDetermined:
            .notDetermined
        case .authorized, .provisional, .ephemeral:
            .authorized
        case .denied:
            .denied
        @unknown default:
            .denied
        }
    }

    func requestAuthorization() async throws -> Bool {
        try await center.requestAuthorization(options: [.alert, .sound])
    }

    func pendingRequestIdentifiers() async -> [String] {
        await center.pendingNotificationRequests().map(\.identifier)
    }

    func deliveredRequestIdentifiers() async -> [String] {
        await center.deliveredNotifications().map { $0.request.identifier }
    }

    func add(_ request: DayWeaveBreakNotificationRequest) async throws {
        try await center.add(Self.notificationRequest(for: request))
    }

    static func notificationRequest(
        for request: DayWeaveBreakNotificationRequest
    ) -> UNNotificationRequest {
        let content = UNMutableNotificationContent()
        content.title = request.title
        content.body = request.body
        content.sound = .default
        content.categoryIdentifier = DayWeaveBreakNotificationContract.categoryIdentifier

        var calendar = Calendar(identifier: .gregorian)
        let absoluteTimeZone = TimeZone(secondsFromGMT: 0)!
        calendar.timeZone = absoluteTimeZone
        var components = calendar.dateComponents(
            [.year, .month, .day, .hour, .minute, .second, .nanosecond],
            from: request.fireDate
        )
        // UNCalendarNotificationTrigger otherwise interprets these wall-clock
        // fields in the user's current zone. Carrying UTC in the components
        // keeps the server deadline an absolute instant across travel and DST.
        components.timeZone = absoluteTimeZone
        let trigger = UNCalendarNotificationTrigger(
            dateMatching: components,
            repeats: false
        )
        return UNNotificationRequest(
            identifier: request.identifier,
            content: content,
            trigger: trigger
        )
    }

    func removePendingRequestIdentifiers(_ identifiers: [String]) async {
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
    }

    func removeDeliveredRequestIdentifiers(_ identifiers: [String]) async {
        center.removeDeliveredNotifications(withIdentifiers: identifiers)
    }
}

enum DayWeaveBreakNotificationContract {
    static let identifierPrefix = "dayweave.break-ended.v1."
    static let categoryIdentifier = "dayweave.break-ended"
    static let title = "Break ended"
    static let body = "Open DayWeave to choose what happens next."

    struct Descriptor: Equatable, Sendable {
        let identifier: String
        let deadline: Date
        let version: DayWeaveExecutionSessionVersion
    }

    static func descriptor(
        for session: DayWeaveExecutionSession?
    ) -> Descriptor? {
        guard let session,
              session.hasValidShape,
              session.status == .paused,
              let deadline = session.pauseUntil,
              let deadlineMicros = dayWeavePostgresEpochMicroseconds(deadline) else {
            return nil
        }
        let material = [
            "DayWeave.BreakEnded.Notification.v1",
            session.id.uuidString.lowercased(),
            String(session.revision),
            String(deadlineMicros),
        ].joined(separator: "\u{0}")
        let digest = SHA256.hash(data: Data(material.utf8)).map {
            String(format: "%02x", $0)
        }.joined()
        return Descriptor(
            identifier: identifierPrefix + digest,
            deadline: deadline,
            version: .init(sessionID: session.id, revision: session.revision)
        )
    }

    static func request(
        for session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?,
        now: Date
    ) -> DayWeaveBreakNotificationRequest? {
        guard let descriptor = descriptor(for: session),
              acknowledged != descriptor.version,
              descriptor.deadline > now else { return nil }
        return .init(
            identifier: descriptor.identifier,
            title: title,
            body: body,
            fireDate: descriptor.deadline
        )
    }

    static func tapMatchesUnresolvedBreak(
        identifier: String,
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?,
        now: Date
    ) -> Bool {
        guard let descriptor = descriptor(for: session) else { return false }
        return identifier == descriptor.identifier
            && descriptor.deadline <= now
            && acknowledged != descriptor.version
    }

    static func owns(identifier: String) -> Bool {
        guard identifier.hasPrefix(identifierPrefix) else { return false }
        let suffix = identifier.dropFirst(identifierPrefix.count).utf8
        return suffix.count == 64 && suffix.allSatisfy { byte in
            (48...57).contains(byte) || (97...102).contains(byte)
        }
    }

    static func foregroundPresentationOptions(
        for identifier: String
    ) -> UNNotificationPresentationOptions {
        // An opaque identifier alone cannot prove that the encrypted lease is
        // still current. Suppress even owned requests in the foreground; the
        // exact in-app resolution UI is driven from revalidated local state.
        guard owns(identifier: identifier) else { return [.banner, .sound] }
        return []
    }
}

enum DayWeaveBreakNotificationReconcileResult: Equatable, Sendable {
    case scheduled
    case canceled
    case unchanged
    case permissionRequired
    case permissionDenied
    case unavailable
    case cancellationUnavailable
    case superseded

    var isVerifiedCancellation: Bool {
        switch self {
        case .canceled, .unchanged:
            true
        case .scheduled, .permissionRequired, .permissionDenied, .unavailable,
             .cancellationUnavailable, .superseded:
            false
        }
    }
}

enum DayWeaveNotificationAuthorizationRequestResult: Equatable, Sendable {
    case authorized
    case denied
    case unavailable
}

protocol DayWeaveBreakNotificationCoordinating: Sendable {
    func authorizationState() async -> DayWeaveNotificationAuthorizationState
    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult
    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult
}

actor DayWeaveBreakNotificationCoordinator: DayWeaveBreakNotificationCoordinating {
    private struct Input: Sendable {
        let session: DayWeaveExecutionSession?
        let acknowledged: DayWeaveExecutionSessionVersion?

        var desiredIdentifier: String? {
            guard let descriptor = DayWeaveBreakNotificationContract.descriptor(for: session),
                  descriptor.version != acknowledged else { return nil }
            return descriptor.identifier
        }
    }

    private let center: any DayWeaveBreakNotificationCenter
    private let now: @Sendable () -> Date
    private let removalVerificationAttempts: Int
    private let removalVerificationDelay: Duration
    private let sleep: @Sendable (Duration) async -> Void
    private let onReconcileAccepted: (@Sendable (UInt64) -> Void)?
    private var generation: UInt64 = 0
    private var latestInput = Input(session: nil, acknowledged: nil)
    private var knownOwnedIdentifiers: Set<String> = []
    private var isDraining = false
    private var waiters: [
        UInt64: [CheckedContinuation<DayWeaveBreakNotificationReconcileResult, Never>]
    ] = [:]

    init(
        center: any DayWeaveBreakNotificationCenter = DayWeaveUNBreakNotificationCenter(),
        now: @escaping @Sendable () -> Date = Date.init,
        removalVerificationAttempts: Int = 4,
        removalVerificationDelay: Duration = .milliseconds(75),
        sleep: @escaping @Sendable (Duration) async -> Void = { duration in
            try? await Task.sleep(for: duration)
        },
        onReconcileAccepted: (@Sendable (UInt64) -> Void)? = nil
    ) {
        precondition(removalVerificationAttempts > 0)
        self.center = center
        self.now = now
        self.removalVerificationAttempts = removalVerificationAttempts
        self.removalVerificationDelay = removalVerificationDelay
        self.sleep = sleep
        self.onReconcileAccepted = onReconcileAccepted
    }

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        await center.authorizationState()
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        switch await center.authorizationState() {
        case .authorized:
            return .authorized
        case .denied:
            return .denied
        case .notDetermined:
            do {
                return try await center.requestAuthorization() ? .authorized : .denied
            } catch {
                return .unavailable
            }
        }
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        generation &+= 1
        let operation = generation
        latestInput = Input(session: session, acknowledged: acknowledged)
        return await withCheckedContinuation { continuation in
            waiters[operation, default: []].append(continuation)
            onReconcileAccepted?(operation)
            guard !isDraining else { return }
            isDraining = true
            Task { await self.drain() }
        }
    }

    private func drain() async {
        while true {
            let operation = generation
            let input = latestInput
            let result = await performReconciliation(input, operation: operation)

            guard operation == generation else {
                // Every caller is part of the durability barrier. Keep older
                // waiters suspended until the newest desired generation has
                // actually converged; returning here would let a successful
                // execution mutation terminate before its replacement
                // reconciliation removed or installed the system request.
                continue
            }
            resumeWaiters(before: operation, with: .superseded)
            resumeWaiters(at: operation, with: result)
            isDraining = false
            return
        }
    }

    private func performReconciliation(
        _ input: Input,
        operation: UInt64
    ) async -> DayWeaveBreakNotificationReconcileResult {
        let observedAt = now()
        let descriptor = DayWeaveBreakNotificationContract.descriptor(for: input.session)
        let desired = descriptor.flatMap { value in
            input.acknowledged == value.version ? nil : value
        }
        let pending = Set(await center.pendingRequestIdentifiers()).filter(
            DayWeaveBreakNotificationContract.owns
        )
        knownOwnedIdentifiers.formUnion(pending)
        guard operation == generation else { return .superseded }
        let delivered = Set(await center.deliveredRequestIdentifiers()).filter(
            DayWeaveBreakNotificationContract.owns
        )
        knownOwnedIdentifiers.formUnion(delivered)
        guard operation == generation else { return .superseded }

        let retainedIdentifier = desired?.identifier
        let stalePending = pending.filter { $0 != retainedIdentifier }
        let staleDelivered = delivered.filter { $0 != retainedIdentifier }
        // Retain the last exact desired digest independently of Notification
        // Center snapshots. During a pending-to-delivered handoff both queries
        // may transiently miss the request; the known digest still gives the
        // cancellation barrier an exact, privacy-safe target.
        let knownStale = knownOwnedIdentifiers.filter { $0 != retainedIdentifier }
        let staleIdentifiers = stalePending.union(staleDelivered).union(knownStale)
        if !staleIdentifiers.isEmpty {
            guard await removeEverywhere(staleIdentifiers) else {
                return operation == generation ? .cancellationUnavailable : .superseded
            }
            knownOwnedIdentifiers.subtract(staleIdentifiers)
        }
        guard operation == generation else { return .superseded }

        guard let desired else {
            return staleIdentifiers.isEmpty ? .unchanged : .canceled
        }
        if desired.deadline <= observedAt {
            if pending.contains(desired.identifier)
                || delivered.contains(desired.identifier)
                || knownOwnedIdentifiers.contains(desired.identifier) {
                if await removeEverywhere([desired.identifier]) {
                    knownOwnedIdentifiers.remove(desired.identifier)
                    return .canceled
                }
                return .cancellationUnavailable
            }
            return staleIdentifiers.isEmpty ? .unchanged : .canceled
        }
        if pending.contains(desired.identifier) || delivered.contains(desired.identifier) {
            return staleIdentifiers.isEmpty ? .unchanged : .canceled
        }

        let authorization = await center.authorizationState()
        guard operation == generation else { return .superseded }
        switch authorization {
        case .authorized:
            break
        case .denied:
            return .permissionDenied
        case .notDetermined:
            return .permissionRequired
        }
        guard let request = DayWeaveBreakNotificationContract.request(
            for: input.session,
            acknowledged: input.acknowledged,
            now: now()
        ) else { return .superseded }
        do {
            try await center.add(request)
            knownOwnedIdentifiers.insert(request.identifier)
            guard operation == generation else {
                // Actors are reentrant across center.add. If a newer desired
                // generation arrived while this add was in flight, remove only
                // the request that is no longer wanted. The single drain then
                // converges the center to the newest state before its waiter
                // completes.
                if latestInput.desiredIdentifier != request.identifier {
                    _ = await removeEverywhere([request.identifier])
                }
                return .superseded
            }
            return .scheduled
        } catch {
            if operation != generation,
               latestInput.desiredIdentifier != request.identifier {
                _ = await removeEverywhere([request.identifier])
            }
            return operation == generation ? .unavailable : .superseded
        }
    }

    private func removeEverywhere(_ identifiers: Set<String>) async -> Bool {
        await removeEverywhere(identifiers.sorted())
    }

    private func removeEverywhere(_ identifiers: [String]) async -> Bool {
        guard !identifiers.isEmpty else { return true }
        let targets = Set(identifiers)
        for attempt in 0..<removalVerificationAttempts {
            // UserNotifications removal methods return void before daemon-side
            // disappearance is observable. Remove from both collections, then
            // requery both; repeating also catches a request that transitions
            // from pending to delivered during cancellation.
            await center.removePendingRequestIdentifiers(identifiers)
            await center.removeDeliveredRequestIdentifiers(identifiers)
            let pending = Set(await center.pendingRequestIdentifiers())
                .intersection(targets)
            let delivered = Set(await center.deliveredRequestIdentifiers())
                .intersection(targets)
            if pending.isEmpty, delivered.isEmpty { return true }
            if attempt + 1 < removalVerificationAttempts {
                await sleep(removalVerificationDelay)
            }
        }
        return false
    }

    private func resumeWaiters(
        before operation: UInt64,
        with result: DayWeaveBreakNotificationReconcileResult
    ) {
        guard operation > 0 else { return }
        let generations = waiters.keys.filter { $0 < operation }
        for generation in generations {
            resumeWaiters(at: generation, with: result)
        }
    }

    private func resumeWaiters(
        at operation: UInt64,
        with result: DayWeaveBreakNotificationReconcileResult
    ) {
        let continuations = waiters.removeValue(forKey: operation) ?? []
        continuations.forEach { $0.resume(returning: result) }
    }
}

struct DayWeaveNoopBreakNotificationCoordinator: DayWeaveBreakNotificationCoordinating {
    func authorizationState() async -> DayWeaveNotificationAuthorizationState { .denied }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .denied
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        .unchanged
    }
}

@MainActor
final class DayWeaveBreakNotificationTapRouter: ObservableObject {
    static let shared = DayWeaveBreakNotificationTapRouter()

    @Published private(set) var pendingIdentifier: String?
    private var activateMainWindow: (() -> Void)?

    /// SwiftUI scene observers disappear when every window is closed. Retain
    /// one process-lifetime scene action so the notification delegate can
    /// reactivate the singleton planner window before any content observer is
    /// present. State is still revalidated only after the app lock opens.
    func installMainWindowActivation(_ activation: @escaping () -> Void) {
        activateMainWindow = activation
        if pendingIdentifier != nil { activation() }
    }

    func route(identifier: String) -> Bool {
        guard DayWeaveBreakNotificationContract.owns(identifier: identifier) else {
            return false
        }
        pendingIdentifier = identifier
        activateMainWindow?()
        return true
    }

    func consume(identifier: String) {
        guard pendingIdentifier == identifier else { return }
        pendingIdentifier = nil
    }

    /// Lock state is checked before routing so a notification response cannot
    /// pierce the local privacy boundary. Once content is available, the exact
    /// opaque response is consumed regardless of whether authoritative state
    /// still accepts it; a stale response must never be retargeted.
    func deliverPending(
        contentAvailable: Bool,
        route: (String) -> Bool
    ) -> Bool? {
        guard contentAvailable, let identifier = pendingIdentifier else { return nil }
        let accepted = route(identifier)
        consume(identifier: identifier)
        return accepted
    }
}

@MainActor
final class DayWeaveBreakNotificationDeliveryPulse: ObservableObject {
    static let shared = DayWeaveBreakNotificationDeliveryPulse()

    /// This process-local counter deliberately carries no notification ID or
    /// decrypted planner data. It only invalidates the in-app resolver when an
    /// owned foreground delivery is suppressed by Notification Center.
    @Published private(set) var generation: UInt64 = 0

    func emitOwnedDelivery() {
        generation &+= 1
    }
}

final class DayWeaveMacAppDelegate: NSObject, NSApplicationDelegate,
    UNUserNotificationCenterDelegate
{
    func applicationDidFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        // Notification Center can hand us a request that escaped cancellation
        // just before an execution replacement. The app delegate deliberately
        // has no access to decrypted planner state, so foreground delivery is
        // suppressed instead of guessing that an opaque owned identifier is
        // still authoritative. The in-app break resolution remains exact.
        let identifier = notification.request.identifier
        let options = DayWeaveBreakNotificationContract.foregroundPresentationOptions(
            for: identifier
        )
        if DayWeaveBreakNotificationContract.owns(identifier: identifier) {
            await MainActor.run {
                DayWeaveBreakNotificationDeliveryPulse.shared.emitOwnedDelivery()
            }
        }
        return options
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let identifier = response.notification.request.identifier
        guard DayWeaveBreakNotificationContract.owns(identifier: identifier) else { return }
        await MainActor.run {
            _ = DayWeaveBreakNotificationTapRouter.shared.route(identifier: identifier)
        }
    }
}
