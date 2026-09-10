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

    @MainActor
    private final class Context {
        let token = "synthetic-routine-prepare-" + UUID().uuidString
        let directory: URL, file: URL, persistence: EncryptedPlannerPersistence
        let clock = Clock()
        let planner: PlannerStore, transport: EchoTransport, helper: Composer, sync: CanonicalSyncStore
        init() throws {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("RoutinePlanningCoordinator-" + UUID().uuidString)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            self.directory = directory; file = directory.appendingPathComponent("planner.encrypted")
            persistence = EncryptedPlannerPersistence(fileURL: file, key: .random())
            let client = DayWeaveAPIClient(baseURL: try DayWeaveAPIBaseURL(Configuration.url),
                session: URLProtocolStub.makeSession(), bearerToken: token)
            let clock = self.clock
            planner = PlannerStore(canonicalItems: try RoutinePlanningWitnessTestFixtures.canonicalItems(),
                canonicalDeltaCursor: "synthetic-canonical-terminal", canonicalConfigurationIdentifier: client.configurationIdentifier,
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
        var onComposition: (@MainActor () -> Void)?
        func composeOccurrences(canonicalItems: [DayWeaveCanonicalItem], witness: RoutinePlanningWitness) async throws -> RoutineOccurrenceLocalComposition {
            calls += 1
            func time(_ field: String) throws -> Date {
                guard case let .string(value)? = witness.schedule.fields[field],
                      let date = SchedulerHelperRFC3339.date(from: value) else { throw RoutinePlanningWitnessError.invalidData }
                return date
            }
            let plan = try DayWeaveSchedulePreview.Plan(asOf: time("as_of"), horizonStart: time("horizon_start"),
                horizonEnd: time("horizon_end"), blocks: [], unscheduled: [], decisions: [], violations: [],
                score: .init(scheduledMinutes: 0, unscheduledMinutes: 0, softPenalty: 0, movedMinutes: 0), occurrences: [])
            onComposition?()
            return .init(composition: .init(localInputFingerprint: witness.localInputFingerprint,
                sourceItemCount: canonicalItems.count, sourceItemRevisions: witness.sourceItemRevisions,
                acceptedItemCount: canonicalItems.count, rejectedItems: [], ignoredPreviousAssignments: [], plan: plan),
                occurrenceSnapshotRevision: witness.occurrenceLifecycle.snapshotRevision)
        }
    }
}
#endif
