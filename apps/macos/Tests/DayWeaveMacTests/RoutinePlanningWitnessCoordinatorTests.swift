import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Protected saved routine input preparation", .serialized)
@MainActor
struct RoutinePlanningWitnessCoordinatorTests {
    @Test("owned private POST and v2 parity save encrypted exact input without installing or publishing")
    func prepareOnly() async throws {
        let c = try Context(); defer { c.cleanUp() }
        c.planner.flushPersistence()
        let before = try Data(contentsOf: c.file)
        #expect(c.sync.canPrepareRoutinePlanningInput)
        #expect(try Data(contentsOf: c.file) == before) // UI admission is read-only.
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let capsule = try #require(c.planner.routinePlanningInputCapsule)
        #expect(c.helper.calls == 1)
        let requests = URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true)
        #expect(requests.count == 1 && requests[0].method == "POST")
        #expect(requests[0].url.path == "/gateway/v1/routine-occurrences/planning-witness")
        #expect(capsule.originalRequestBody == requests[0].body)
        #expect(capsule.canonicalItems == c.planner.canonicalItems)
        #expect(capsule.witness.occurrenceLifecycle.snapshotRevision == 7)
        #expect(c.planner.blocks.isEmpty && c.planner.pendingSchedulePublication == nil)
        #expect(c.planner.localScheduleCompositionProvenance == nil && c.sync.lastLocalComposition == nil)
        #expect(!c.sync.canRecomposeLocally && c.planner.requiresRemoteRoutineOccurrenceComposition)
        #expect(c.planner.routineOccurrenceState.terminalDeltaCursor == RoutinePlanningWitnessTestFixtures.cursor)
        #expect(!c.planner.routineOccurrenceState.needsRemoteScheduleCatchUp)
        #expect(try c.persistence.load()?.routinePlanningInputCapsule == capsule)
        let clock = c.clock
        let restarted = PlannerStore(persistence: c.persistence, now: { clock.read() })
        #expect(restarted.routinePlanningInputCapsule == capsule)
        #expect(restarted.requiresRemoteRoutineOccurrenceComposition)
        #expect(restarted.schedulePreviewProvenance == nil && restarted.publishedScheduleProof == nil)
        #expect(restarted.localScheduleCompositionProvenance == nil)
        #expect(!String(decoding: try Data(contentsOf: c.file), as: UTF8.self).contains("Synthetic private routine"))
    }

    @Test("all remote classifications preserve an earlier complete capsule and never invoke helper")
    func remoteRequired() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let prior = c.planner.routinePlanningInputCapsule
        for reason in RoutinePlanningRemoteReason.allCases {
            c.transport.reason = reason
            #expect(!(await c.sync.prepareRoutineOccurrencePlanningInput()))
            #expect(c.planner.routinePlanningInputCapsule == prior && c.helper.calls == 1)
            #expect(c.sync.routinePlanningInputMessage == reason.preparationMessage)
        }
    }

    enum Boundary: CaseIterable, Sendable { case occurrenceRead, canonicalRead, configuration, background, clockRollback, nextDay }
    @Test("late POST cannot cross read, binding, privacy or original-clock boundaries", arguments: Boundary.allCases)
    func lateResponse(_ boundary: Boundary) async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let prior = c.planner.routinePlanningInputCapsule
        c.transport.onResponse = {
            switch boundary {
            case .occurrenceRead: c.planner.invalidateRoutineOccurrencePlanningEvidence()
            case .canonicalRead: c.planner.invalidateItemCompletionReadEvidence()
            case .configuration: c.sync.configurationDidChange()
            case .background: c.sync.stopForegroundItemInvalidations()
            case .clockRollback: c.clock.set(c.clock.read().addingTimeInterval(-1))
            case .nextDay: c.clock.set(c.clock.read().addingTimeInterval(86_400))
            }
        }
        #expect(!(await c.sync.prepareRoutineOccurrencePlanningInput()))
        #expect(c.helper.calls == 1 && c.planner.routinePlanningInputCapsule == prior)
        #expect(!c.planner.isCanonicalSyncLocked && !c.sync.isPreparingRoutinePlanningInput)
    }

    @Test("late helper result cannot commit after occurrence authority changes")
    func lateHelper() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let prior = c.planner.routinePlanningInputCapsule
        c.helper.onComposition = { c.planner.invalidateRoutineOccurrencePlanningEvidence() }
        #expect(!(await c.sync.prepareRoutineOccurrencePlanningInput()))
        #expect(c.planner.routinePlanningInputCapsule == prior)
        #expect(c.planner.pendingSchedulePublication == nil && c.planner.blocks.isEmpty)
    }

    @Test("clock cannot move backwards between POST completion and helper completion")
    func lateHelperClockRollback() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let prior = c.planner.routinePlanningInputCapsule, originalClock = c.clock.read()
        c.transport.onResponse = { c.clock.set(originalClock.addingTimeInterval(120)) }
        c.helper.onComposition = { c.clock.set(originalClock.addingTimeInterval(60)) }
        #expect(!(await c.sync.prepareRoutineOccurrencePlanningInput()))
        #expect(c.planner.routinePlanningInputCapsule == prior)
    }

    @Test("caller cancellation rejects a late successful POST without invoking helper")
    func cancellation() async throws {
        let c = try Context(); defer { c.cleanUp() }
        var operation: Task<Bool, Never>?
        c.transport.onResponse = { operation?.cancel() }
        operation = Task { await c.sync.prepareRoutineOccurrencePlanningInput() }
        #expect(!(await operation!.value))
        #expect(c.helper.calls == 0 && c.planner.routinePlanningInputCapsule == nil)
        #expect(!c.planner.isCanonicalSyncLocked)
    }

    @Test("a queued Settings task cannot start after foreground permission is withdrawn")
    func queuedStartAfterWithdrawal() async throws {
        let c = try Context(); defer { c.cleanUp() }
        let queued = Task { await c.sync.prepareRoutineOccurrencePlanningInput() }
        c.sync.stopForegroundItemInvalidations()
        #expect(!(await queued.value))
        #expect(!c.sync.canPrepareRoutinePlanningInput)
        #expect(URLProtocolStub.storage.requests(for: c.token).isEmpty && c.helper.calls == 0)
        #expect(c.planner.routinePlanningInputCapsule == nil)
    }

    @Test("same-binding learned scope cannot be replaced by another qualified reply")
    func scopePin() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let prior = c.planner.routinePlanningInputCapsule
        c.transport.foreignScope = true
        #expect(!(await c.sync.prepareRoutineOccurrencePlanningInput()))
        #expect(c.helper.calls == 1 && c.planner.routinePlanningInputCapsule == prior)
    }

    @Test("pending occurrence catch-up is retained and stops preparation before network or helper")
    func pendingCatchUp() async throws {
        let c = try Context(); defer { c.cleanUp() }
        var pending = c.planner.routineOccurrenceState; pending.needsRemoteScheduleCatchUp = true
        try c.planner.commitRoutineOccurrenceState(pending, replacing: c.planner.routineOccurrenceState)
        #expect(!c.sync.canPrepareRoutinePlanningInput)
        #expect(!(await c.sync.prepareRoutineOccurrencePlanningInput()))
        #expect(URLProtocolStub.storage.requests(for: c.token).isEmpty && c.helper.calls == 0)
        #expect(c.planner.routineOccurrenceState == pending && c.planner.routinePlanningInputCapsule == nil)
    }

    @Test("offline restart recomputes only the exact saved v2 input into separate encrypted display custody")
    func savedDisplayOfflineRestart() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let capsule = try #require(c.planner.routinePlanningInputCapsule)
        let requests = URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true)
        c.clock.set(c.clock.read().addingTimeInterval(120))
        let (restarted, helper, sync) = c.restart()
        #expect(!sync.canRecomputeSavedRoutinePlanningDisplay)
        #expect(sync.routinePlanningDisplayPresentation == nil)
        #expect(!(await sync.recomputeSavedRoutinePlanningDisplay()))
        sync.activateRoutinePlanningInputCapture()
        let before = try #require(try c.persistence.load())
        let bytes = try Data(contentsOf: c.file)
        #expect(sync.canRecomputeSavedRoutinePlanningDisplay)
        #expect(try Data(contentsOf: c.file) == bytes)
        #expect(await sync.recomputeSavedRoutinePlanningDisplay())
        let artifact = try #require(restarted.routinePlanningDisplayPlan)
        let presentation = try #require(sync.routinePlanningDisplayPresentation)
        #expect(helper.calls == 1 && helper.lastWitness == capsule.witness)
        #expect(helper.lastItems == capsule.canonicalItems && helper.lastRawOutput == artifact.rawOutput)
        let originalClock = try RoutinePlanningShape.string(capsule.witness.schedule.fields["as_of"])
        #expect(presentation.asOf == originalClock)
        #expect(artifact.generatedAt == c.clock.read())
        #expect(artifact.capsuleBinding == capsule && restarted.routinePlanningInputCapsule == capsule)
        #expect(restarted.blocks == before.blocks && restarted.routineOccurrenceState == before.routineOccurrenceState)
        #expect(restarted.publishedScheduleProof == before.publishedScheduleProof)
        #expect(restarted.pendingSchedulePublication == before.pendingSchedulePublication)
        #expect(restarted.localScheduleCompositionProvenance == nil && sync.lastLocalComposition == nil)
        #expect(restarted.requiresRemoteRoutineOccurrenceComposition && !sync.canRecomposeLocally)
        #expect(try c.persistence.load()?.routinePlanningDisplayPlan == artifact)
        #expect(URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count == requests.count)
        let (twiceRestarted, secondHelper, secondSync) = c.restart()
        #expect(twiceRestarted.routinePlanningDisplayPlan == artifact)
        secondSync.activateRoutinePlanningInputCapture()
        #expect(secondSync.routinePlanningDisplayPresentation == nil)
        #expect(await secondSync.recomputeSavedRoutinePlanningDisplay())
        #expect(secondHelper.calls == 1 && secondSync.routinePlanningDisplayPresentation != nil)
        #expect(URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count == requests.count)
    }

    @Test("late saved-input helper result cannot cross read, binding, private foreground or clock fences", arguments: Boundary.allCases)
    func savedDisplayLateHelper(_ boundary: Boundary) async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        let prior = try #require(c.planner.routinePlanningDisplayPlan)
        let capsule = c.planner.routinePlanningInputCapsule
        c.helper.onComposition = {
            switch boundary {
            case .occurrenceRead: c.planner.invalidateRoutineOccurrencePlanningEvidence()
            case .canonicalRead: c.planner.invalidateItemCompletionReadEvidence()
            case .configuration: c.sync.configurationDidChange()
            case .background: c.sync.stopForegroundItemInvalidations()
            case .clockRollback: c.clock.set(c.clock.read().addingTimeInterval(-1))
            case .nextDay: c.clock.set(c.clock.read().addingTimeInterval(86_400))
            }
        }
        #expect(!(await c.sync.recomputeSavedRoutinePlanningDisplay()))
        #expect(c.planner.routinePlanningDisplayPlan == prior && c.planner.routinePlanningInputCapsule == capsule)
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        #expect(!c.planner.isCanonicalSyncLocked && !c.sync.isRecomputingRoutinePlanningDisplay)
        #expect(URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count == 1)
        #expect(c.planner.blocks.isEmpty && c.planner.pendingSchedulePublication == nil)
    }

    @Test("saved display permission expires on private withdrawal and cannot be restored by activation alone")
    func savedDisplayPrivateLease() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        let prior = c.planner.routinePlanningDisplayPlan
        #expect(c.sync.routinePlanningDisplayPresentation != nil)
        c.sync.stopForegroundItemInvalidations()
        #expect(c.sync.routinePlanningDisplayPresentation == nil && !c.sync.canRecomputeSavedRoutinePlanningDisplay)
        c.sync.activateRoutinePlanningInputCapture()
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        #expect(c.planner.routinePlanningDisplayPlan == prior)
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        c.planner.invalidateRoutineOccurrencePlanningEvidence()
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        #expect(c.planner.routinePlanningDisplayPlan != nil)
    }

    @Test("quiet display ticks enforce a sticky observed-clock high-water and original-day expiry")
    func savedDisplayClockObservation() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        let original = c.clock.read(), artifact = c.planner.routinePlanningDisplayPlan
        let bytes = try Data(contentsOf: c.file)
        c.clock.set(original.addingTimeInterval(600)); c.sync.refreshRoutinePlanningDisplayAdmission()
        #expect(c.sync.routinePlanningDisplayPresentation != nil)
        c.clock.set(original.addingTimeInterval(300)); c.sync.refreshRoutinePlanningDisplayAdmission()
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        c.clock.set(original.addingTimeInterval(900)); c.sync.refreshRoutinePlanningDisplayAdmission()
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        #expect(c.planner.routinePlanningDisplayPlan == artifact)
        #expect(try Data(contentsOf: c.file) == bytes) // Clock observations never rewrite custody.
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        #expect(c.sync.routinePlanningDisplayPresentation != nil)
        c.clock.set(original.addingTimeInterval(86_400)); c.sync.refreshRoutinePlanningDisplayAdmission()
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        c.clock.set(original.addingTimeInterval(1_000)); c.sync.refreshRoutinePlanningDisplayAdmission()
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        #expect(c.planner.routinePlanningDisplayPlan != nil)
    }

    @Test("cancellation and a queued recompute after stop never install or expose a late plan")
    func savedDisplayCancellation() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        let prior = c.planner.routinePlanningDisplayPlan
        var operation: Task<Bool, Never>?
        c.helper.onComposition = { operation?.cancel() }
        operation = Task { await c.sync.recomputeSavedRoutinePlanningDisplay() }
        #expect(!(await operation!.value))
        #expect(c.planner.routinePlanningDisplayPlan == prior && c.sync.routinePlanningDisplayPresentation == nil)
        let count = c.helper.calls
        let queued = Task { await c.sync.recomputeSavedRoutinePlanningDisplay() }
        c.sync.stopForegroundItemInvalidations()
        #expect(!(await queued.value))
        #expect(c.helper.calls == count && !c.planner.isCanonicalSyncLocked)
    }

    @Test("new pending catch-up preserves the prior display and blocks offline recomputation before helper")
    func savedDisplayPendingRecovery() async throws {
        let c = try Context(); defer { c.cleanUp() }
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        #expect(await c.sync.recomputeSavedRoutinePlanningDisplay())
        let prior = c.planner.routinePlanningDisplayPlan
        let old = c.planner.routineOccurrenceState
        var pending = old; pending.needsRemoteScheduleCatchUp = true
        try c.planner.commitRoutineOccurrenceState(pending, replacing: old)
        #expect(c.sync.routinePlanningDisplayPresentation == nil)
        #expect(!c.sync.canRecomputeSavedRoutinePlanningDisplay)
        #expect(!(await c.sync.recomputeSavedRoutinePlanningDisplay()))
        #expect(c.helper.calls == 2 && c.planner.routinePlanningDisplayPlan == prior)
        #expect(c.planner.routineOccurrenceState == pending)
        #expect(URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count == 1)
    }

    @Test("real failed execution refresh withdraws freshness without preventing exact offline display or restoring execution authority")
    func savedDisplayAfterRealExecutionReadFailure() async throws {
        let c = try Context(withExistingExecutionBinding: true); defer { c.cleanUp() }
        URLProtocolStub.storage.enqueue(key: c.token,
            .init(statusCode: 200, body: Data(#"{"execution":{"revision":0,"active_session":null}}"#.utf8)),
            .init(statusCode: 200, body: Data(#"{"sessions":[],"next_offset":null}"#.utf8)),
            .init(statusCode: 200, body: Data(#"{"execution":{"revision":0,"active_session":null}}"#.utf8)))
        let execution = try c.executionStore(planner: c.planner)
        #expect(await execution.refresh() == .success)
        #expect(c.planner.executionState.historyVerified && c.planner.executionState.historyContinuityEstablished)
        #expect(c.planner.canonicalItems.count == 3 && c.sync.canPrepareRoutinePlanningInput)
        #expect(await c.sync.prepareRoutineOccurrencePlanningInput())
        let capsule = try #require(c.planner.routinePlanningInputCapsule)
        let (restarted, helper, sync) = c.restart()
        let offlineExecution = try c.executionStore(planner: restarted)
        // The real refresh path sends its GET through an empty, fail-closed
        // URLProtocol queue. It cannot reach a service or receive fresh proof.
        #expect(await offlineExecution.refresh() != .success)
        #expect(!restarted.executionState.historyVerified && !restarted.executionState.historyContinuityEstablished)
        let withdrawn = restarted.executionState
        let requestCount = URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count
        sync.activateRoutinePlanningInputCapture()
        #expect(sync.canRecomputeSavedRoutinePlanningDisplay)
        #expect(await sync.recomputeSavedRoutinePlanningDisplay())
        #expect(helper.calls == 1 && helper.lastWitness == capsule.witness)
        #expect(sync.routinePlanningDisplayPresentation != nil)
        #expect(restarted.executionState == withdrawn)
        #expect(restarted.routinePlanningInputCapsule?.environment.executionState.historyVerified == true)
        #expect(restarted.publishedScheduleProof == nil && restarted.localScheduleCompositionProvenance == nil)
        #expect(URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count == requestCount)
        let prior = restarted.routinePlanningDisplayPlan
        var changed = withdrawn; changed.revision += 1
        try restarted.persistExecutionState(changed)
        #expect(sync.routinePlanningDisplayPresentation == nil && !sync.canRecomputeSavedRoutinePlanningDisplay)
        #expect(!(await sync.recomputeSavedRoutinePlanningDisplay()))
        #expect(restarted.routinePlanningDisplayPlan == prior && restarted.executionState == changed)
        #expect(helper.calls == 1)
    }

    @Test("real cold foreground activation keeps terminal Habit custody and permits only an explicit offline display recomputation")
    func savedDisplayAfterRealColdActivation() async throws {
        let c = try Context(withExistingExecutionBinding: true); defer { c.cleanUp() }
        let clock = c.clock
        let client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(Configuration.url),
            session: URLProtocolStub.makeSession(), bearerToken: c.token)
        let habitPersistence = EncryptedHabitPersistence(fileURL: c.directory.appendingPathComponent("habits.encrypted"), key: .random())
        let terminal = DayWeaveHabitClientSnapshot(savedAt: clock.read(), configurationIdentifier: client.configurationIdentifier,
            deltaCursor: "synthetic-habit-terminal", deltaCaughtUp: true, occurrences: [], pauses: [], analytics: [], pendingMutations: [])
        _ = try habitPersistence.save(terminal, expectedRevision: .missing)
        let connection = DayWeaveHabitConnection(configurationIdentifier: client.configurationIdentifier, transport: client)
        func habits() -> HabitSyncStore {
            HabitSyncStore(persistence: habitPersistence, connectionProvider: { connection }, now: { clock.read() })
        }
        let preparedHabits = habits()
        // Loading a terminal encrypted cache may fail its new remote read, but
        // must not manufacture a new missed-reconcile journal while offline.
        #expect(await preparedHabits.activate() != .success)
        #expect(preparedHabits.pendingMutations.isEmpty)
        let habitFingerprint = preparedHabits.habitCompositionCheckpoint.fingerprint
        URLProtocolStub.storage.enqueue(key: c.token,
            .init(statusCode: 200, body: Data(#"{"execution":{"revision":0,"active_session":null}}"#.utf8)),
            .init(statusCode: 200, body: Data(#"{"sessions":[],"next_offset":null}"#.utf8)),
            .init(statusCode: 200, body: Data(#"{"execution":{"revision":0,"active_session":null}}"#.utf8)))
        let preparingExecution = try c.executionStore(planner: c.planner)
        #expect(await preparingExecution.refresh() == .success)
        let preparing = c.syncStore(planner: c.planner, helper: c.helper, habits: preparedHabits)
        preparing.activateRoutinePlanningInputCapture()
        #expect(c.planner.canonicalItems.count == 3 && preparing.canPrepareRoutinePlanningInput)
        #expect(await preparing.prepareRoutineOccurrencePlanningInput())
        let capsule = try #require(c.planner.routinePlanningInputCapsule)
        #expect(capsule.environment.habitCheckpointFingerprint == habitFingerprint)
        let restarted = PlannerStore(persistence: c.persistence, now: { clock.read() })
        let restartedHabits = habits(), helper = Composer()
        let sync = c.syncStore(planner: restarted, helper: helper, habits: restartedHabits)
        let execution = try c.executionStore(planner: restarted)
        let lifecycle = DayWeaveServiceCoordinator(proposalApplications: NoPendingProposal(), executionSync: execution,
            canonicalSync: sync, habitSync: restartedHabits)
        defer { lifecycle.deactivate() }
        lifecycle.activate()
        await lifecycle.waitForActivation()
        // Quiesce polling only; this does not withdraw the independently
        // granted private foreground ownership or grant any fresh evidence.
        execution.stopForegroundPolling(); restartedHabits.stopForegroundPolling()
        // Execution polling starts with an immediate refresh. Cancellation is
        // cooperative: its real URLSession/notification unwind must release
        // the shared canonical lock before a new local operation is admitted.
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while (execution.isSyncing || restartedHabits.habitCompositionCheckpoint.hasActiveOperation),
              ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(!execution.isSyncing && !restarted.isCanonicalSyncLocked)
        #expect(!restartedHabits.habitCompositionCheckpoint.hasActiveOperation)
        #expect(!restarted.executionState.historyVerified && !restarted.executionState.historyContinuityEstablished)
        #expect(restartedHabits.pendingMutations.isEmpty)
        #expect(restartedHabits.habitCompositionCheckpoint.fingerprint == habitFingerprint)
        #expect(try habitPersistence.loadRevisioned().snapshot == terminal)
        #expect(sync.routinePlanningDisplayPresentation == nil)
        let checkpoint = restartedHabits.habitCompositionCheckpoint
        let inputIssue = restarted.routinePlanningDisplayCapsuleIssue(origin: capsule.origin,
            configurationIdentifier: capsule.configurationIdentifier, habitCheckpoint: checkpoint, at: clock.read())
        #expect(inputIssue == nil)
        let currentEnvironment = try RoutinePlanningInputEnvironment(planner: restarted, habitCheckpoint: checkpoint)
        #expect(capsule.environment.matchesForDisplay(current: currentEnvironment))
        #expect(sync.canRecomputeSavedRoutinePlanningDisplay)
        let requestCount = URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count
        #expect(await sync.recomputeSavedRoutinePlanningDisplay())
        #expect(helper.calls == 1 && helper.lastWitness == capsule.witness)
        #expect(sync.routinePlanningDisplayPresentation != nil && restarted.routinePlanningDisplayPlan != nil)
        #expect(!restarted.executionState.historyVerified && !restarted.executionState.historyContinuityEstablished)
        #expect(restartedHabits.pendingMutations.isEmpty)
        #expect(restarted.blocks.isEmpty && restarted.pendingSchedulePublication == nil)
        #expect(URLProtocolStub.storage.requests(for: c.token, includingSchedulePublication: true).count == requestCount)
        lifecycle.deactivate()
        #expect(sync.routinePlanningDisplayPresentation == nil && !sync.canRecomputeSavedRoutinePlanningDisplay)
    }

    @MainActor
    private final class Context {
        let token = "synthetic-routine-prepare-" + UUID().uuidString
        let directory: URL, file: URL, persistence: EncryptedPlannerPersistence
        let clock = Clock()
        let planner: PlannerStore, transport: EchoTransport, helper: Composer, sync: CanonicalSyncStore
        init(withExistingExecutionBinding: Bool = false) throws {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("RoutinePlanningCoordinator-" + UUID().uuidString)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            self.directory = directory; file = directory.appendingPathComponent("planner.encrypted")
            persistence = EncryptedPlannerPersistence(fileURL: file, key: .random())
            let client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(Configuration.url),
                session: URLProtocolStub.makeSession(), bearerToken: token)
            let clock = self.clock
            // This cache was captured after connection setup. Installing a
            // different first execution binding after seeding remote canonical
            // data would correctly quarantine that data as credential change.
            var executionState = DayWeaveExecutionDurableState.empty
            if withExistingExecutionBinding {
                executionState.deviceID = UUID()
                executionState.bindingIdentifier = "synthetic-execution-binding"
            }
            planner = PlannerStore(canonicalItems: try RoutinePlanningWitnessTestFixtures.canonicalItems(),
                canonicalDeltaCursor: "synthetic-canonical-terminal", canonicalConfigurationIdentifier: client.configurationIdentifier,
                executionState: executionState,
                routineOccurrenceState: .init(configurationIdentifier: client.configurationIdentifier,
                    terminalDeltaCursor: RoutinePlanningWitnessTestFixtures.cursor),
                scheduleProfile: try .legacyDefault(timezoneName: "UTC", protectedFreeMinutes: 60),
                persistence: persistence, restoreFromPersistence: false, autosaveDelay: .seconds(60), now: { clock.read() })
            transport = EchoTransport(client: client, token: token); helper = Composer()
            let transport = self.transport
            sync = CanonicalSyncStore(planner: planner, configurationStore: Configuration(),
                tokenStore: TestBearerTokenStore(token: token), session: URLProtocolStub.makeSession(),
                occurrenceComposer: helper, routinePlanningWitnessTransportProvider: { _ in transport }, now: { clock.read() })
            // Grant the same process-only foreground signal used by service activation,
            // without starting unrelated item/schedule polling in this unit fixture.
            sync.activateRoutinePlanningInputCapture()
        }
        func restart() -> (PlannerStore, Composer, CanonicalSyncStore) {
            let clock = self.clock, transport = self.transport
            let restarted = PlannerStore(persistence: persistence, now: { clock.read() })
            let helper = Composer()
            let sync = CanonicalSyncStore(planner: restarted, configurationStore: Configuration(),
                tokenStore: TestBearerTokenStore(token: token), session: URLProtocolStub.makeSession(),
                occurrenceComposer: helper, routinePlanningWitnessTransportProvider: { _ in transport }, now: { clock.read() })
            return (restarted, helper, sync)
        }
        func executionStore(planner: PlannerStore) throws -> ExecutionSyncStore {
            let client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(Configuration.url),
                session: URLProtocolStub.makeSession(), bearerToken: token)
            let connection = DayWeaveExecutionConnection(canonicalConfigurationIdentifier: client.configurationIdentifier,
                bindingIdentifier: "synthetic-execution-binding", transport: client)
            let clock = self.clock
            return ExecutionSyncStore(planner: planner, connectionProvider: { connection }, now: { clock.read() })
        }
        func syncStore(planner: PlannerStore, helper: Composer, habits: HabitSyncStore) -> CanonicalSyncStore {
            let clock = self.clock, transport = self.transport
            return CanonicalSyncStore(planner: planner, configurationStore: Configuration(),
                tokenStore: TestBearerTokenStore(token: token), session: URLProtocolStub.makeSession(),
                occurrenceComposer: helper, routinePlanningWitnessTransportProvider: { _ in transport },
                habitCompositionProvider: habits, now: { clock.read() })
        }
        func cleanUp() { try? FileManager.default.removeItem(at: directory) }
    }

    private struct Configuration: SuggestionAPIConfigurationStoring {
        static let url = "https://api.example.com/gateway"
        func loadBaseURL() -> String? { Self.url }
        func saveBaseURL(_ value: String) {}
    }
    private final class Clock: @unchecked Sendable {
        private let lock = NSLock()
        private var value = ISO8601DateFormatter().date(from: "2026-09-10T09:00:00Z")!
        func read() -> Date { lock.withLock { value } }
        func set(_ value: Date) { lock.withLock { self.value = value } }
    }
    @MainActor
    private final class NoPendingProposal: ProposalApplicationRecovering {
        let hasPendingRecovery = false
        func recoverPendingMutation() async -> Bool { true }
    }
    @MainActor
    private final class EchoTransport: RoutinePlanningWitnessTransport {
        nonisolated let configurationIdentifier: String
        let client: DayWeaveAPIClient, token: String
        var reason: RoutinePlanningRemoteReason?, foreignScope = false
        var onResponse: (@MainActor () -> Void)?
        init(client: DayWeaveAPIClient, token: String) {
            self.client = client; self.token = token; configurationIdentifier = client.configurationIdentifier
            URLProtocolStub.storage.reset(key: token)
        }
        func routinePlanningWitness(_ request: RoutinePlanningWitnessRequest) async throws -> RoutinePlanningWitnessResponse {
            let response: RoutinePlanningWitnessResponse
            if let reason { response = .init(result: .remoteRequired(reason)) }
            else {
                var object = RoutinePlanningWitnessTestFixtures.witnessObject()
                object["schedule"] = try JSONSerialization.jsonObject(with: RoutinePlanningWitnessValidation.encode(request.schedule))
                object["source_item_revisions"] = Dictionary(uniqueKeysWithValues: request.expectedSourceItemRevisions.map { ($0.key.uuidString.lowercased(), $0.value) })
                object["terminal_cursor"] = request.terminalCursor; object["execution_snapshot_revision"] = 0
                if foreignScope { object["workspace_id"] = "00000000-0000-4000-8000-000000000099" }
                response = .init(result: .qualified(try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self,
                    from: RoutinePlanningWitnessTestFixtures.data(object))))
            }
            URLProtocolStub.storage.enqueue(key: token, .init(statusCode: 200,
                headers: ["Content-Type": "application/json", "Cache-Control": "no-store, max-age=0"],
                body: try RoutinePlanningWitnessValidation.encode(response)))
            let received = try await client.routinePlanningWitness(request)
            onResponse?()
            return received
        }
    }
    @MainActor
    private final class Composer: RoutineOccurrenceScheduleComposing {
        var calls = 0
        var lastWitness: RoutinePlanningWitness?
        var lastItems: [DayWeaveCanonicalItem]?
        var lastRawOutput: Data?
        var onComposition: (@MainActor () -> Void)?
        func composeOccurrences(canonicalItems: [DayWeaveCanonicalItem], witness: RoutinePlanningWitness) async throws -> RoutineOccurrenceLocalComposition {
            calls += 1
            let composition = try RoutinePlanningDisplaySyntheticComposition.make(witness: witness)
            lastWitness = witness; lastItems = canonicalItems; lastRawOutput = composition.rawOutput
            onComposition?()
            return composition
        }
    }
}
#endif
