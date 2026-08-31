import Foundation
import UserNotifications
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Privacy-safe timed-break notifications", .serialized)
struct BreakNotificationCoordinatorTests {
    private static let baseDate = Date(timeIntervalSince1970: 1_800_000_000)

    @Test("a valid timed break schedules one generic opaque notification")
    func schedulesGenericOpaqueNotification() async throws {
        let center = BreakNotificationCenterDouble(authorization: .authorized)
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )

        #expect(await coordinator.reconcile(
            session: paused,
            acknowledged: nil
        ) == .scheduled)
        let request = try #require((await center.snapshot()).added.first)
        let descriptor = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)
        )
        #expect(request.identifier == descriptor.identifier)
        #expect(DayWeaveBreakNotificationContract.owns(identifier: request.identifier))
        #expect(request.title == "Break ended")
        #expect(request.body == "Open DayWeave to choose what happens next.")
        #expect(request.fireDate == paused.pauseUntil)
        #expect(!request.title.contains("Private project"))
        #expect(!request.body.contains(paused.itemID.uuidString))
        #expect(!request.identifier.contains(paused.id.uuidString.lowercased()))
        #expect(!request.identifier.contains(paused.itemID.uuidString.lowercased()))

        let replacement = try Self.pausedSession(
            revision: 3,
            deadline: Self.baseDate.addingTimeInterval(900),
            updatedAt: Self.baseDate.addingTimeInterval(1)
        )
        #expect(DayWeaveBreakNotificationContract.descriptor(for: replacement)?.identifier
            != descriptor.identifier)
    }

    @Test("restart recovery schedules a missing request without duplicating an existing one")
    func restartRecoveryAndDuplicateSuppression() async throws {
        let center = BreakNotificationCenterDouble(authorization: .authorized)
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let first = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )
        #expect(await first.reconcile(session: paused, acknowledged: nil) == .scheduled)

        let afterFirst = await center.snapshot()
        #expect(afterFirst.added.count == 1)
        let relaunched = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )
        #expect(await relaunched.reconcile(session: paused, acknowledged: nil) == .unchanged)
        #expect((await center.snapshot()).added.count == 1)

        await center.replacePending([])
        let recovered = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )
        #expect(await recovered.reconcile(session: paused, acknowledged: nil) == .scheduled)
        #expect((await center.snapshot()).added.count == 2)
    }

    @Test("replacement and terminal state cancel stale pending and delivered requests")
    func replacementAndClosureCancelStaleRequests() async throws {
        let old = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let oldIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: old)?.identifier
        )
        let center = BreakNotificationCenterDouble(
            authorization: .authorized,
            pending: [oldIdentifier],
            delivered: [oldIdentifier]
        )
        let replacement = try Self.pausedSession(
            revision: 3,
            deadline: Self.baseDate.addingTimeInterval(900),
            updatedAt: Self.baseDate.addingTimeInterval(1)
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )

        #expect(await coordinator.reconcile(
            session: replacement,
            acknowledged: nil
        ) == .scheduled)
        var snapshot = await center.snapshot()
        let replacementIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: replacement)?.identifier
        )
        #expect(snapshot.removedPending.contains(oldIdentifier))
        #expect(snapshot.removedDelivered.contains(oldIdentifier))
        #expect(snapshot.pending == Set([replacementIdentifier]))

        let resumed = try Self.activeSession(
            revision: 4,
            updatedAt: Self.baseDate.addingTimeInterval(2)
        )
        #expect(await coordinator.reconcile(session: resumed, acknowledged: nil) == .canceled)
        snapshot = await center.snapshot()
        #expect(snapshot.removedPending.contains(replacementIdentifier))
        #expect(snapshot.pending.isEmpty)

        await center.replacePending([replacementIdentifier])
        await center.replaceDelivered([replacementIdentifier])
        #expect(await coordinator.reconcile(session: nil, acknowledged: nil) == .canceled)
        snapshot = await center.snapshot()
        #expect(snapshot.pending.isEmpty)
        #expect(snapshot.delivered.isEmpty)
    }

    @Test("automatic reconciliation never prompts and explicit permission remains nonfatal")
    func permissionIsExplicitAndNonfatal() async throws {
        let center = BreakNotificationCenterDouble(authorization: .denied)
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )

        #expect(await coordinator.reconcile(
            session: paused,
            acknowledged: nil
        ) == .permissionDenied)
        let snapshot = await center.snapshot()
        #expect(snapshot.added.isEmpty)
        #expect(snapshot.authorizationRequests == 0)

        let notDetermined = BreakNotificationCenterDouble(
            authorization: .notDetermined,
            authorizationResponse: .denied
        )
        let requested = DayWeaveBreakNotificationCoordinator(
            center: notDetermined,
            now: { Self.baseDate }
        )
        #expect(await requested.reconcile(
            session: paused,
            acknowledged: nil
        ) == .permissionRequired)
        #expect((await notDetermined.snapshot()).authorizationRequests == 0)
        #expect(await requested.requestAuthorization() == .denied)
        #expect((await notDetermined.snapshot()).authorizationRequests == 1)

        let grantCenter = BreakNotificationCenterDouble(
            authorization: .notDetermined,
            authorizationResponse: .granted
        )
        let granted = DayWeaveBreakNotificationCoordinator(
            center: grantCenter,
            now: { Self.baseDate }
        )
        #expect(await granted.requestAuthorization() == .authorized)
        #expect(await granted.reconcile(session: paused, acknowledged: nil) == .scheduled)
        #expect((await grantCenter.snapshot()).authorizationRequests == 1)

        let failing = BreakNotificationCenterDouble(
            authorization: .notDetermined,
            authorizationResponse: .failure
        )
        let unavailable = DayWeaveBreakNotificationCoordinator(
            center: failing,
            now: { Self.baseDate }
        )
        #expect(await unavailable.reconcile(
            session: paused,
            acknowledged: nil
        ) == .permissionRequired)
        #expect(await unavailable.requestAuthorization() == .unavailable)
        #expect((await failing.snapshot()).added.isEmpty)

        let inactive = BreakNotificationCenterDouble(authorization: .notDetermined)
        let noPrompt = DayWeaveBreakNotificationCoordinator(
            center: inactive,
            now: { Self.baseDate }
        )
        #expect(await noPrompt.reconcile(
            session: try Self.activeSession(revision: 3, updatedAt: Self.baseDate),
            acknowledged: nil
        ) == .unchanged)
        #expect((await inactive.snapshot()).authorizationRequests == 0)
    }

    @Test("an explicit authorization prompt that outlives the break never schedules a past request")
    func authorizationDelayRechecksDeadline() async throws {
        let deadline = Self.baseDate.addingTimeInterval(60)
        let paused = try Self.pausedSession(revision: 2, deadline: deadline)
        let clock = AdvancingBreakNotificationClock(Self.baseDate)
        let center = BreakNotificationCenterDouble(
            authorization: .notDetermined,
            onAuthorizationRequest: {
                clock.set(deadline.addingTimeInterval(1))
            }
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { clock.value }
        )

        #expect(await coordinator.reconcile(
            session: paused,
            acknowledged: nil
        ) == .permissionRequired)
        #expect(await coordinator.requestAuthorization() == .authorized)
        #expect(await coordinator.reconcile(
            session: paused,
            acknowledged: nil
        ) == .unchanged)
        #expect((await center.snapshot()).added.isEmpty)
    }

    @Test("production calendar request carries an absolute UTC timezone")
    func productionRequestUsesAbsoluteTimezone() throws {
        let fireDate = Date(timeIntervalSince1970: 1_800_000_123.456)
        let request = DayWeaveBreakNotificationRequest(
            identifier: DayWeaveBreakNotificationContract.identifierPrefix
                + String(repeating: "a", count: 64),
            title: DayWeaveBreakNotificationContract.title,
            body: DayWeaveBreakNotificationContract.body,
            fireDate: fireDate
        )

        let notification = DayWeaveUNBreakNotificationCenter.notificationRequest(for: request)
        let trigger = try #require(notification.trigger as? UNCalendarNotificationTrigger)
        let timezone = try #require(trigger.dateComponents.timeZone)
        #expect(timezone.secondsFromGMT(for: fireDate) == 0)
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timezone
        let reconstructed = try #require(calendar.date(from: trigger.dateComponents))
        #expect(abs(reconstructed.timeIntervalSince(fireDate)) < 0.001)
        #expect(notification.content.title == "Break ended")
        #expect(notification.content.body == "Open DayWeave to choose what happens next.")
        #expect(notification.content.userInfo.isEmpty)
    }

    @Test("a delayed stale add is removed before the replacement reconcile completes")
    func delayedAddConvergesToLatestGeneration() async throws {
        let gate = SequencedBreakNotificationGate()
        let center = BreakNotificationCenterDouble(
            authorization: .authorized,
            onAdd: { await gate.wait() }
        )
        let old = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let replacement = try Self.pausedSession(
            revision: 3,
            deadline: Self.baseDate.addingTimeInterval(900),
            updatedAt: Self.baseDate.addingTimeInterval(1)
        )
        let admission = BreakNotificationReconcileAdmissionProbe()
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate },
            onReconcileAccepted: admission.accept
        )

        let staleCompletion = BreakNotificationCompletionProbe()
        let stale = Task {
            let result = await coordinator.reconcile(session: old, acknowledged: nil)
            await staleCompletion.markComplete()
            return result
        }
        await gate.waitUntilEntered(call: 1)
        let latest = Task {
            await coordinator.reconcile(session: replacement, acknowledged: nil)
        }
        await admission.wait(for: 2)
        await gate.release(call: 1)
        await gate.waitUntilEntered(call: 2)

        // A superseded mutation/reset caller is still a durability waiter. It
        // cannot return while the replacement add remains uncommitted.
        #expect(!(await staleCompletion.isComplete))
        await gate.release(call: 2)

        #expect(await stale.value == .superseded)
        #expect(await staleCompletion.isComplete)
        #expect(await latest.value == .scheduled)
        let snapshot = await center.snapshot()
        let oldIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: old)?.identifier
        )
        let replacementIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: replacement)?.identifier
        )
        #expect(snapshot.pending == Set([replacementIdentifier]))
        #expect(snapshot.removedPending.contains(oldIdentifier))
    }

    @Test("a delayed stale removal is repaired for the latest desired generation")
    func delayedRemovalConvergesToLatestGeneration() async throws {
        let old = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let oldIdentifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: old)?.identifier
        )
        let gate = BreakNotificationGate()
        let center = BreakNotificationCenterDouble(
            authorization: .authorized,
            pending: [oldIdentifier],
            onRemovePending: { await gate.wait() }
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )

        let staleCancel = Task {
            await coordinator.reconcile(session: nil, acknowledged: nil)
        }
        await gate.waitUntilEntered()
        let latest = Task { await coordinator.reconcile(session: old, acknowledged: nil) }
        await gate.release()

        #expect(await staleCancel.value == .superseded)
        #expect(await latest.value == .scheduled)
        #expect((await center.snapshot()).pending == Set([oldIdentifier]))
    }

    @Test("cancellation removes a request that becomes delivered during pending removal")
    func pendingToDeliveredTransitionCannotEscapeCancellation() async throws {
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let center = BreakNotificationCenterDouble(
            authorization: .authorized,
            pending: [identifier],
            transitionPendingToDeliveredBeforePendingRemoval: true
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate }
        )

        #expect(await coordinator.reconcile(session: nil, acknowledged: nil) == .canceled)
        let snapshot = await center.snapshot()
        #expect(snapshot.pending.isEmpty)
        #expect(snapshot.delivered.isEmpty)
        #expect(snapshot.removedPending.contains(identifier))
        #expect(snapshot.removedDelivered.contains(identifier))
    }

    @Test("a known request cannot hide between the initial pending and delivered snapshots")
    func invisiblePendingToDeliveredTransitionUsesKnownIdentifier() async throws {
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let center = BreakNotificationCenterDouble(authorization: .authorized)
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate },
            removalVerificationDelay: .zero,
            sleep: { _ in }
        )
        #expect(await coordinator.reconcile(session: paused, acknowledged: nil) == .scheduled)
        await center.hidePendingRequestAcrossNextSnapshotPair()

        #expect(await coordinator.reconcile(session: nil, acknowledged: nil) == .canceled)
        let snapshot = await center.snapshot()
        #expect(snapshot.pending.isEmpty)
        #expect(snapshot.inFlight.isEmpty)
        #expect(snapshot.delivered.isEmpty)
        #expect(snapshot.removedPending.contains(identifier))
        #expect(snapshot.removedDelivered.contains(identifier))
    }

    @Test("cancellation waits until delayed daemon disappearance is observable")
    func cancellationRequeriesUntilRemovalConverges() async throws {
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let center = BreakNotificationCenterDouble(
            authorization: .authorized,
            pending: [identifier],
            pendingRemovalPassesBeforeDisappearance: 1
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate },
            removalVerificationAttempts: 3,
            removalVerificationDelay: .zero,
            sleep: { _ in }
        )

        #expect(await coordinator.reconcile(session: nil, acknowledged: nil) == .canceled)
        let snapshot = await center.snapshot()
        #expect(snapshot.pending.isEmpty)
        #expect(snapshot.delivered.isEmpty)
        #expect(snapshot.removedPending.filter { $0 == identifier }.count == 2)
        #expect(snapshot.removedDelivered.filter { $0 == identifier }.count == 2)
    }

    @Test("cancellation fails visibly when owned requests survive the bounded barrier")
    func cancellationTimeoutIsUnavailable() async throws {
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let center = BreakNotificationCenterDouble(
            authorization: .authorized,
            pending: [identifier],
            pendingRemovalPassesBeforeDisappearance: 10
        )
        let coordinator = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.baseDate },
            removalVerificationAttempts: 3,
            removalVerificationDelay: .zero,
            sleep: { _ in }
        )

        #expect(await coordinator.reconcile(
            session: nil,
            acknowledged: nil
        ) == .cancellationUnavailable)
        let snapshot = await center.snapshot()
        #expect(snapshot.pending == Set([identifier]))
        #expect(snapshot.removedPending.filter { $0 == identifier }.count == 3)
        #expect(snapshot.removedDelivered.filter { $0 == identifier }.count == 3)
    }

    @Test("only escaped owned requests are suppressed in the foreground")
    func staleForegroundDeliveryIsSuppressed() {
        let identifier = DayWeaveBreakNotificationContract.identifierPrefix
            + String(repeating: "b", count: 64)
        #expect(DayWeaveBreakNotificationContract.foregroundPresentationOptions(
            for: identifier
        ).isEmpty)
        #expect(DayWeaveBreakNotificationContract.foregroundPresentationOptions(
            for: "dayweave.some-future-notification"
        ) == [.banner, .sound])
    }

    @Test("tap routing accepts only the exact unresolved expired break")
    @MainActor
    func tapRoutingRevalidatesAuthoritativeState() async throws {
        let deadline = Self.baseDate.addingTimeInterval(600)
        let paused = try Self.pausedSession(revision: 2, deadline: deadline)
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let observedAt = deadline.addingTimeInterval(1)
        #expect(DayWeaveBreakNotificationContract.tapMatchesUnresolvedBreak(
            identifier: identifier,
            session: paused,
            acknowledged: nil,
            now: observedAt
        ))
        #expect(!DayWeaveBreakNotificationContract.tapMatchesUnresolvedBreak(
            identifier: identifier,
            session: paused,
            acknowledged: .init(sessionID: paused.id, revision: paused.revision),
            now: observedAt
        ))
        let replacement = try Self.pausedSession(
            revision: 3,
            deadline: deadline.addingTimeInterval(600),
            updatedAt: Self.baseDate.addingTimeInterval(1)
        )
        #expect(!DayWeaveBreakNotificationContract.tapMatchesUnresolvedBreak(
            identifier: identifier,
            session: replacement,
            acknowledged: nil,
            now: replacement.pauseUntil!.addingTimeInterval(1)
        ))

        let router = DayWeaveBreakNotificationTapRouter()
        #expect(!router.route(identifier: "dayweave.break-ended.v1.not-a-digest"))
        #expect(router.pendingIdentifier == nil)
        #expect(router.route(identifier: identifier))
        #expect(router.pendingIdentifier == identifier)
        var routedIdentifiers: [String] = []
        #expect(router.deliverPending(contentAvailable: false) { routedIdentifiers.append($0); return true }
            == nil)
        #expect(router.pendingIdentifier == identifier)
        #expect(routedIdentifiers.isEmpty)
        #expect(router.deliverPending(contentAvailable: true) {
            routedIdentifiers.append($0)
            return true
        } == true)
        #expect(router.pendingIdentifier == nil)
        #expect(routedIdentifiers == [identifier])
        #expect(router.deliverPending(contentAvailable: true) { _ in true } == nil)

        #expect(router.route(identifier: identifier))
        #expect(router.deliverPending(contentAvailable: true) { _ in false } == false)
        #expect(router.pendingIdentifier == nil)
    }

    @Test("delegate routing retains a process-lifetime main-window activation seam")
    @MainActor
    func tapActivatesWindowWithoutSceneContentObserver() async throws {
        let paused = try Self.pausedSession(
            revision: 2,
            deadline: Self.baseDate.addingTimeInterval(600)
        )
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: paused)?.identifier
        )
        let router = DayWeaveBreakNotificationTapRouter()
        var activations = 0

        // No Combine/SwiftUI content observer is installed. The retained scene
        // action still opens or focuses the singleton window when the delegate
        // receives a response later in the process lifetime.
        router.installMainWindowActivation { activations += 1 }
        #expect(router.route(identifier: identifier))
        #expect(activations == 1)
        #expect(router.pendingIdentifier == identifier)
        #expect(router.deliverPending(contentAvailable: false) { _ in true } == nil)
        #expect(router.pendingIdentifier == identifier)

        let beforeLateInstall = DayWeaveBreakNotificationTapRouter()
        #expect(beforeLateInstall.route(identifier: identifier))
        var lateActivations = 0
        beforeLateInstall.installMainWindowActivation { lateActivations += 1 }
        #expect(lateActivations == 1)
    }

    private static func pausedSession(
        revision: UInt64,
        deadline: Date,
        updatedAt: Date = baseDate
    ) throws -> DayWeaveExecutionSession {
        try session(
            status: "paused",
            revision: revision,
            accumulatedSeconds: 30,
            updatedAt: updatedAt,
            runningSince: nil,
            pausedAt: updatedAt,
            pauseUntil: deadline
        )
    }

    private static func activeSession(
        revision: UInt64,
        updatedAt: Date
    ) throws -> DayWeaveExecutionSession {
        try session(
            status: "active",
            revision: revision,
            accumulatedSeconds: 30,
            updatedAt: updatedAt,
            runningSince: updatedAt,
            pausedAt: nil,
            pauseUntil: nil
        )
    }

    private static func session(
        status: String,
        revision: UInt64,
        accumulatedSeconds: UInt64,
        updatedAt: Date,
        runningSince: Date?,
        pausedAt: Date?,
        pauseUntil: Date?
    ) throws -> DayWeaveExecutionSession {
        let sessionID = UUID(uuidString: "10000000-0000-4000-8000-000000000001")!
        let itemID = UUID(uuidString: "20000000-0000-4000-8000-000000000002")!
        let blockID = UUID(uuidString: "30000000-0000-4000-8000-000000000003")!
        let deviceID = UUID(uuidString: "40000000-0000-4000-8000-000000000004")!
        let pauseReason: Any = status == "paused" ? "Private project" : NSNull()
        let object: [String: Any] = [
            "id": sessionID.uuidString.lowercased(),
            "item_id": itemID.uuidString.lowercased(),
            "item_revision": 1,
            "occurrence_id": NSNull(),
            "session_index": 0,
            "planned_block_id": blockID.uuidString.lowercased(),
            "source_device_id": deviceID.uuidString.lowercased(),
            "status": status,
            "revision": revision,
            "accumulated_seconds": accumulatedSeconds,
            "actual_seconds": NSNull(),
            "started_at": format(baseDate),
            "running_since": runningSince.map(format) ?? NSNull(),
            "paused_at": pausedAt.map(format) ?? NSNull(),
            "pause_until": pauseUntil.map(format) ?? NSNull(),
            "pause_reason": pauseReason,
            "ended_at": NSNull(),
            "created_at": format(baseDate),
            "updated_at": format(updatedAt),
        ]
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(
            DayWeaveExecutionSession.self,
            from: JSONSerialization.data(withJSONObject: object)
        )
    }

    private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

private actor BreakNotificationCenterDouble: DayWeaveBreakNotificationCenter {
    enum AuthorizationResponse: Sendable {
        case granted
        case denied
        case failure
    }

    enum TestError: Error { case unavailable }

    struct Snapshot: Sendable {
        let pending: Set<String>
        let inFlight: Set<String>
        let delivered: Set<String>
        let added: [DayWeaveBreakNotificationRequest]
        let removedPending: [String]
        let removedDelivered: [String]
        let authorizationRequests: Int
    }

    private var authorization: DayWeaveNotificationAuthorizationState
    private let authorizationResponse: AuthorizationResponse
    private let onAuthorizationRequest: (@Sendable () async -> Void)?
    private let onAdd: (@Sendable () async -> Void)?
    private let onRemovePending: (@Sendable () async -> Void)?
    private let transitionPendingToDeliveredBeforePendingRemoval: Bool
    private var pendingRemovalPassesBeforeDisappearance: Int
    private var deliveredRemovalPassesBeforeDisappearance: Int
    private var pending: Set<String>
    private var inFlight: Set<String> = []
    private var delivered: Set<String>
    private var added: [DayWeaveBreakNotificationRequest] = []
    private var removedPending: [String] = []
    private var removedDelivered: [String] = []
    private var authorizationRequests = 0
    private var hideNextSnapshotPair = false
    private var snapshotPairIsHidingTransition = false

    init(
        authorization: DayWeaveNotificationAuthorizationState,
        pending: Set<String> = [],
        delivered: Set<String> = [],
        authorizationResponse: AuthorizationResponse = .granted,
        onAuthorizationRequest: (@Sendable () async -> Void)? = nil,
        onAdd: (@Sendable () async -> Void)? = nil,
        onRemovePending: (@Sendable () async -> Void)? = nil,
        transitionPendingToDeliveredBeforePendingRemoval: Bool = false,
        pendingRemovalPassesBeforeDisappearance: Int = 0,
        deliveredRemovalPassesBeforeDisappearance: Int = 0
    ) {
        self.authorization = authorization
        self.pending = pending
        self.delivered = delivered
        self.authorizationResponse = authorizationResponse
        self.onAuthorizationRequest = onAuthorizationRequest
        self.onAdd = onAdd
        self.onRemovePending = onRemovePending
        self.transitionPendingToDeliveredBeforePendingRemoval =
            transitionPendingToDeliveredBeforePendingRemoval
        self.pendingRemovalPassesBeforeDisappearance =
            pendingRemovalPassesBeforeDisappearance
        self.deliveredRemovalPassesBeforeDisappearance =
            deliveredRemovalPassesBeforeDisappearance
    }

    func authorizationState() -> DayWeaveNotificationAuthorizationState {
        authorization
    }

    func requestAuthorization() async throws -> Bool {
        authorizationRequests += 1
        await onAuthorizationRequest?()
        switch authorizationResponse {
        case .granted:
            authorization = .authorized
            return true
        case .denied:
            authorization = .denied
            return false
        case .failure: throw TestError.unavailable
        }
    }

    func pendingRequestIdentifiers() -> [String] {
        if hideNextSnapshotPair {
            hideNextSnapshotPair = false
            snapshotPairIsHidingTransition = true
            inFlight.formUnion(pending)
            pending.removeAll()
            return []
        }
        return Array(pending)
    }

    func deliveredRequestIdentifiers() -> [String] {
        if snapshotPairIsHidingTransition {
            snapshotPairIsHidingTransition = false
            return []
        }
        return Array(delivered)
    }

    func add(_ request: DayWeaveBreakNotificationRequest) async throws {
        await onAdd?()
        added.append(request)
        pending.insert(request.identifier)
    }

    func removePendingRequestIdentifiers(_ identifiers: [String]) async {
        if transitionPendingToDeliveredBeforePendingRemoval {
            let transitioning = pending.intersection(Set(identifiers))
            pending.subtract(transitioning)
            delivered.formUnion(transitioning)
        }
        await onRemovePending?()
        removedPending.append(contentsOf: identifiers)
        if pendingRemovalPassesBeforeDisappearance > 0 {
            pendingRemovalPassesBeforeDisappearance -= 1
        } else {
            pending.subtract(identifiers)
        }
    }

    func removeDeliveredRequestIdentifiers(_ identifiers: [String]) {
        delivered.formUnion(inFlight)
        inFlight.removeAll()
        removedDelivered.append(contentsOf: identifiers)
        if deliveredRemovalPassesBeforeDisappearance > 0 {
            deliveredRemovalPassesBeforeDisappearance -= 1
        } else {
            delivered.subtract(identifiers)
        }
    }

    func replacePending(_ identifiers: Set<String>) { pending = identifiers }
    func replaceDelivered(_ identifiers: Set<String>) { delivered = identifiers }
    func hidePendingRequestAcrossNextSnapshotPair() { hideNextSnapshotPair = true }

    func snapshot() -> Snapshot {
        Snapshot(
            pending: pending,
            inFlight: inFlight,
            delivered: delivered,
            added: added,
            removedPending: removedPending,
            removedDelivered: removedDelivered,
            authorizationRequests: authorizationRequests
        )
    }
}

private actor BreakNotificationGate {
    private var entered = false
    private var released = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        entered = true
        let waiters = entryWaiters
        entryWaiters.removeAll()
        waiters.forEach { $0.resume() }
        guard !released else { return }
        await withCheckedContinuation { continuation in
            releaseWaiters.append(continuation)
        }
    }

    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { continuation in
            entryWaiters.append(continuation)
        }
    }

    func release() {
        released = true
        let waiters = releaseWaiters
        releaseWaiters.removeAll()
        waiters.forEach { $0.resume() }
    }
}

private actor SequencedBreakNotificationGate {
    private var callCount = 0
    private var entered: Set<Int> = []
    private var released: Set<Int> = []
    private var entryWaiters: [Int: [CheckedContinuation<Void, Never>]] = [:]
    private var releaseWaiters: [Int: [CheckedContinuation<Void, Never>]] = [:]

    func wait() async {
        callCount += 1
        let call = callCount
        entered.insert(call)
        let waiters = entryWaiters.removeValue(forKey: call) ?? []
        waiters.forEach { $0.resume() }
        guard !released.contains(call) else { return }
        await withCheckedContinuation { continuation in
            releaseWaiters[call, default: []].append(continuation)
        }
    }

    func waitUntilEntered(call: Int) async {
        guard !entered.contains(call) else { return }
        await withCheckedContinuation { continuation in
            entryWaiters[call, default: []].append(continuation)
        }
    }

    func release(call: Int) {
        released.insert(call)
        let waiters = releaseWaiters.removeValue(forKey: call) ?? []
        waiters.forEach { $0.resume() }
    }
}

private actor BreakNotificationCompletionProbe {
    private var completed = false

    var isComplete: Bool { completed }

    func markComplete() { completed = true }
}

private final class BreakNotificationReconcileAdmissionProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var generation: UInt64 = 0
    private var waiters: [(
        generation: UInt64,
        continuation: CheckedContinuation<Void, Never>
    )] = []

    func accept(_ generation: UInt64) {
        let ready: [CheckedContinuation<Void, Never>] = lock.withLock {
            self.generation = max(self.generation, generation)
            let ready = waiters.filter { $0.generation <= self.generation }
            waiters.removeAll { $0.generation <= self.generation }
            return ready.map(\.continuation)
        }
        ready.forEach { $0.resume() }
    }

    func wait(for generation: UInt64) async {
        if lock.withLock({ self.generation >= generation }) { return }
        await withCheckedContinuation { continuation in
            let resumeImmediately = lock.withLock {
                if self.generation >= generation { return true }
                waiters.append((generation, continuation))
                return false
            }
            if resumeImmediately { continuation.resume() }
        }
    }
}

private final class AdvancingBreakNotificationClock: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Date

    init(_ value: Date) {
        stored = value
    }

    var value: Date {
        lock.withLock { stored }
    }

    func set(_ value: Date) {
        lock.withLock { stored = value }
    }
}
#endif
