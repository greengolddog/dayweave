import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Protected live occurrence custody and convergence", .serialized)
@MainActor
struct RoutineOccurrenceStoreTests {
    private typealias F = RoutineOccurrenceTestFixtures
    private static let binding = "https://api.example.test/|auth=static-v1:" + String(repeating: "a", count: 64)

    @Test("an uncached selected planner identity is looked up; restored history grants no lease")
    func lookupAndRestart() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport()
        let store = c.store(client); defer { store.suspendForPrivacyBoundary() }
        let owner = UUID()
        store.showDetail(seriesItemID: F.rootID, occurrenceID: F.plannerID, owner: owner)
        #expect(store.selectedSnapshot == nil)
        #expect(await store.refreshSelected())
        #expect(await client.lookups() == [[F.rootID, F.plannerID]])
        #expect(store.selectedSnapshot?.aggregate.manifest.id == F.instanceID)
        #expect(store.reviewLease(memberID: F.rootID, owner: owner) != nil)
        let restored = PlannerStore(persistence: c.persistence)
        let resumed = c.store(client, planner: restored); defer { resumed.suspendForPrivacyBoundary() }
        resumed.showDetail(seriesItemID: F.rootID, occurrenceID: F.plannerID, owner: owner)
        #expect(resumed.selectedSnapshot == F.snapshot())
        #expect(resumed.reviewLease(memberID: F.rootID, owner: owner) == nil)
    }

    @Test("first send always reviews again and GET rejection never settles custody")
    func firstSendReadFailure() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport()
        let active = c.store(client); defer { active.suspendForPrivacyBoundary() }
        try await queue(active)
        let original = try #require(c.planner.routineOccurrenceState.journals.first)
        await client.setReadFailure(.definitive("routine_occurrence_missing"))
        await client.setPageFailure(.unavailable)
        #expect(!(await active.replayPending()))
        #expect(c.planner.routineOccurrenceState.journals == [original])
        #expect(await client.bodies().isEmpty)
        #expect(!original.hasBeenSubmitted && original.wasSensitive)
        #expect(try c.persistence.load()?.routineOccurrenceState == c.planner.routineOccurrenceState)
    }

    @Test("first-send evidence drift retains original unsubmitted bytes without rebasing")
    func freshDrift() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport()
        let active = c.store(client); defer { active.suspendForPrivacyBoundary() }
        try await queue(active)
        let original = try #require(c.planner.routineOccurrenceState.journals.first)
        await client.setSnapshot(F.snapshot(revision: 2, mode: .keepOpen))
        await client.setPageFailure(.unavailable)
        #expect(!(await active.replayPending()))
        #expect(c.planner.routineOccurrenceState.journals == [original])
        #expect(await client.bodies().isEmpty)
    }

    @Test("lost reply restarts exact submitted PUT without any source or selected GET")
    func lostReplyRestart() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport()
        let first = c.store(client)
        try await queue(first)
        await client.setPutFailure(.unavailable)
        await client.setPageFailure(.unavailable)
        #expect(!(await first.replayPending()))
        let submitted = try #require(c.planner.routineOccurrenceState.journals.first)
        #expect(submitted.hasBeenSubmitted && submitted.noEffectCode == nil)
        first.suspendForPrivacyBoundary()
        let restored = PlannerStore(persistence: c.persistence)
        let resumed = c.store(client, planner: restored); defer { resumed.suspendForPrivacyBoundary() }
        await client.setReadFailure(.definitive("routine_occurrence_missing"))
        await client.setPutFailure(nil)
        let reads = await client.readCount()
        #expect(!(await resumed.replayPending()))
        #expect(await client.readCount() == reads)
        #expect(await client.bodies() == [submitted.requestBody, submitted.requestBody])
        #expect(restored.routineOccurrenceState.journals.isEmpty)
        #expect(restored.routineOccurrenceState.minimumCatchUpRevisions == [F.instanceID: 2])
        #expect(restored.routineOccurrenceState.needsRemoteScheduleCatchUp)
        #expect(restored.canonicalItems.isEmpty && restored.executionState.activeSession == nil)
    }

    @Test("a historical receipt settles matching bytes without overwriting a newer GET")
    func historicalReceipt() async throws {
        let c = try Context(); defer { c.remove() }
        var state = RoutineOccurrenceState(configurationIdentifier: Self.binding)
        try state.observe(F.snapshot(), configurationIdentifier: Self.binding, at: Date())
        let command = F.command()
        let journal = RoutineOccurrenceJournal(instanceID: F.instanceID, memberID: F.rootID,
            configurationIdentifier: Self.binding, command: command,
            requestBody: Data(" \n\t".utf8) + (try command.bytes()), createdAt: Date(), wasSensitive: true)
        try state.enqueue(journal); try state.markSubmitted(journal)
        let newer = F.snapshot(revision: 3, mode: .keepOpen)
        try state.observe(newer, configurationIdentifier: Self.binding, at: Date())
        try c.planner.commitRoutineOccurrenceState(state, replacing: .empty)
        let client = LiveRoutineTransport()
        let active = c.store(client); defer { active.suspendForPrivacyBoundary() }
        await client.setPageFailure(.unavailable)
        #expect(!(await active.replayPending()))
        #expect(c.planner.routineOccurrenceState.observations.first?.snapshot == newer)
        #expect(c.planner.routineOccurrenceState.minimumCatchUpRevisions == [F.instanceID: 2])
        #expect(await client.bodies() == [journal.requestBody])
    }

    @Test("siblings share one instance CAS and submitted ambiguity cannot be discarded")
    func siblingAndDiscard() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport(), store = c.store(client)
        defer { store.suspendForPrivacyBoundary() }
        let owner = UUID(); store.showDetail(seriesItemID: F.rootID, occurrenceID: F.plannerID, owner: owner)
        #expect(await store.refreshSelected())
        let root = try #require(store.reviewLease(memberID: F.rootID, owner: owner))
        let child = try #require(store.reviewLease(memberID: F.childID, owner: owner))
        try store.queueReviewed(lease: root, baseline: F.snapshot(), action: .setPolicy(requiredForParent: true, mode: .keepOpen))
        #expect(throws: RoutineOccurrenceStateError.staleState) {
            try store.queueReviewed(lease: child, baseline: F.snapshot(), action: .setOutcome(status: .completed))
        }
        await client.setPutFailure(.unavailable); await client.setPageFailure(.unavailable)
        _ = await store.replayPending()
        let pending = try #require(c.planner.routineOccurrenceState.journals.first)
        #expect(throws: RoutineOccurrenceStateError.self) {
            try store.discardReviewedIntent(instanceID: F.instanceID, expectedOperationID: pending.id)
        }
        await client.setPutFailure(.definitive("routine_occurrence_instance_stale"))
        _ = await store.replayPending()
        #expect(c.planner.routineOccurrenceState.journals.first?.noEffectCode == "routine_occurrence_instance_stale")
        try store.discardReviewedIntent(instanceID: F.instanceID, expectedOperationID: pending.id)
        #expect(c.planner.routineOccurrenceState.journals.isEmpty)
    }

    @Test("privacy during a late GET or PUT prevents observation and receipt installation")
    func latePrivacy() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport(), store = c.store(client)
        let owner = UUID(); store.showDetail(seriesItemID: F.rootID, occurrenceID: F.plannerID, owner: owner)
        await client.setBeforeRead { store.suspendForPrivacyBoundary() }
        #expect(!(await store.refreshSelected()))
        #expect(c.planner.routineOccurrenceState.observations.isEmpty)
        #expect(store.selectedSnapshot == nil && store.recoveryEntries.isEmpty)
        await client.setBeforeRead(nil); store.activate()
        try await queue(store)
        await client.setBeforePut { store.suspendForPrivacyBoundary() }
        #expect(!(await store.replayPending()))
        #expect(c.planner.routineOccurrenceState.journals.first?.hasBeenSubmitted == true)
        #expect(c.planner.routineOccurrenceState.minimumCatchUpRevisions.isEmpty)
        #expect(store.selectedSnapshot == nil && store.recoveryEntries.isEmpty)
    }

    @Test("terminal revision target can recover from an empty delta by cold list")
    func terminalColdRetry() async throws {
        let c = try Context(); defer { c.remove() }
        let prior = RoutineOccurrenceState(configurationIdentifier: Self.binding,
            terminalDeltaCursor: "old-terminal", minimumCatchUpRevisions: [F.instanceID: 2], needsRemoteScheduleCatchUp: true)
        try c.planner.commitRoutineOccurrenceState(prior, replacing: .empty)
        let client = LiveRoutineTransport(), store = c.store(client)
        defer { store.suspendForPrivacyBoundary() }
        await client.setSnapshot(F.snapshot(revision: 2, mode: .keepOpen))
        await client.setEmptyDelta(true)
        #expect(await store.terminalCatchUp())
        #expect(await client.pageKinds() == ["delta", "list"])
        #expect(c.planner.routineOccurrenceState.terminalDeltaCursor == "terminal-2")
        #expect(c.planner.routineOccurrenceState.minimumCatchUpRevisions.isEmpty)
        #expect(c.planner.routineOccurrenceState.needsRemoteScheduleCatchUp)
    }

    @Test("late receipt custody invalidates a terminal chain without advancing its cursor")
    func lateTerminalCapture() async throws {
        let c = try Context(); defer { c.remove() }
        let prior = RoutineOccurrenceState(configurationIdentifier: Self.binding, terminalDeltaCursor: "old-terminal")
        try c.planner.commitRoutineOccurrenceState(prior, replacing: .empty)
        let client = LiveRoutineTransport(), store = c.store(client)
        defer { store.suspendForPrivacyBoundary() }
        await client.setBeforePage {
            let old = c.planner.routineOccurrenceState; var changed = old
            changed.minimumCatchUpRevisions[F.instanceID] = 2; changed.needsRemoteScheduleCatchUp = true
            try? c.planner.commitRoutineOccurrenceState(changed, replacing: old)
        }
        #expect(!(await store.terminalCatchUp()))
        #expect(c.planner.routineOccurrenceState.terminalDeltaCursor == "old-terminal")
        #expect(c.planner.routineOccurrenceState.minimumCatchUpRevisions == [F.instanceID: 2])
    }

    @Test("status-preserving remote policy changes require fresh remote composition without canonical delta")
    func policyOnlyRemoteCatchUp() async throws {
        let c = try Context(); defer { c.remove() }
        let prior = RoutineOccurrenceState(configurationIdentifier: Self.binding, terminalDeltaCursor: "old-terminal")
        try c.planner.commitRoutineOccurrenceState(prior, replacing: .empty)
        let client = LiveRoutineTransport()
        await client.setSnapshot(F.snapshot(revision: 2, mode: .keepOpen))
        var compositions = 0
        let store = c.store(client, catchUp: { compositions += 1; return true })
        defer { store.suspendForPrivacyBoundary() }
        let canonicalCursor = c.planner.canonicalDeltaCursor
        #expect(await store.replayPending())
        #expect(compositions == 1 && c.planner.canonicalDeltaCursor == canonicalCursor)
        #expect(!c.planner.routineOccurrenceState.needsRemoteScheduleCatchUp)
        #expect(c.planner.requiresRemoteRoutineOccurrenceComposition)
    }

    @Test("a privacy boundary during remote composition keeps the durable schedule latch")
    func lateComposition() async throws {
        let c = try Context(); defer { c.remove() }
        let client = LiveRoutineTransport()
        var store: RoutineOccurrenceStore?
        let active = c.store(client, catchUp: { store?.suspendForPrivacyBoundary(); return true })
        store = active
        #expect(!(await active.replayPending()))
        #expect(c.planner.routineOccurrenceState.needsRemoteScheduleCatchUp)
        #expect(try c.persistence.load()?.routineOccurrenceState?.needsRemoteScheduleCatchUp == true)
    }

    private func queue(_ store: RoutineOccurrenceStore) async throws {
        let owner = UUID()
        store.showDetail(seriesItemID: F.rootID, occurrenceID: F.plannerID, owner: owner)
        #expect(await store.refreshSelected())
        let lease = try #require(store.reviewLease(memberID: F.rootID, owner: owner))
        try store.queueReviewed(lease: lease, baseline: F.snapshot(), action: .setPolicy(requiredForParent: true, mode: .keepOpen))
    }
    @MainActor
    private struct Context {
        let directory: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        init() throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveRoutineLive-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            persistence = EncryptedPlannerPersistence(fileURL: directory.appendingPathComponent("synthetic.encrypted"),
                key: try PlannerEncryptionKey(data: Data(repeating: 81, count: 32)))
            planner = PlannerStore(blocks: [], canonicalItems: [], canonicalDeltaCursor: "synthetic-canonical-head",
                canonicalConfigurationIdentifier: RoutineOccurrenceStoreTests.binding,
                persistence: persistence, restoreFromPersistence: false)
        }
        func store(_ client: LiveRoutineTransport, planner supplied: PlannerStore? = nil,
                   catchUp: @escaping @MainActor () async -> Bool = { false }) -> RoutineOccurrenceStore {
            // The encrypted snapshot uses millisecondsSince1970. An arbitrary
            // wall-clock Double can round on that conversion; a stable clock
            // lets this fixture assert the complete exact custody preimage.
            let referenceDate = Date(timeIntervalSince1970: 1_788_000_000)
            let store = RoutineOccurrenceStore(planner: supplied ?? planner, connection: { client }, catchUp: catchUp,
                now: { referenceDate }, sleep: { _ in throw CancellationError() }, automaticOutbox: false)
            store.activate(); return store
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}

private actor LiveRoutineTransport: RoutineOccurrenceTransport {
    private typealias F = RoutineOccurrenceTestFixtures
    nonisolated let configurationIdentifier = "https://api.example.test/|auth=static-v1:" + String(repeating: "a", count: 64)
    private var snapshot = F.snapshot()
    private var readFailure: RoutineOccurrenceError?
    private var putFailure: RoutineOccurrenceError?
    private var pageFailure: RoutineOccurrenceError?
    private var emptyDelta = false
    private var sent: [Data] = []
    private var lookupIDs: [[UUID]] = []
    private var reads = 0
    private var kinds: [String] = []
    private var beforeRead: (@MainActor @Sendable () -> Void)?
    private var beforePut: (@MainActor @Sendable () -> Void)?
    private var beforePage: (@MainActor @Sendable () -> Void)?
    func lookupRoutineOccurrence(seriesItemID: UUID, occurrenceID: UUID) async throws -> RoutineOccurrenceSnapshot {
        lookupIDs.append([seriesItemID, occurrenceID]); return try await routineOccurrence(instanceID: F.instanceID)
    }
    func routineOccurrence(instanceID: UUID) async throws -> RoutineOccurrenceSnapshot {
        reads += 1
        if let beforeRead { await beforeRead() }
        if let readFailure { throw readFailure }
        return snapshot
    }
    func routineOccurrences(cursor: String?, limit: Int) async throws -> RoutineOccurrencePage {
        kinds.append("list"); return try await page(cursor: cursor, delta: false)
    }
    func routineOccurrenceDelta(cursor: String?, limit: Int) async throws -> RoutineOccurrencePage {
        kinds.append("delta"); return try await page(cursor: cursor, delta: true)
    }
    private func page(cursor: String?, delta: Bool) async throws -> RoutineOccurrencePage {
        if let beforePage { self.beforePage = nil; await beforePage() }
        if let pageFailure { throw pageFailure }
        if delta && emptyDelta { return .init(schemaVersion: 1, changes: [], cursor: cursor ?? "old-terminal", hasMore: false) }
        return .init(schemaVersion: 1, changes: [.init(sequence: snapshot.aggregate.revision, occurrence: snapshot)],
            cursor: "terminal-\(snapshot.aggregate.revision)", hasMore: false)
    }
    func putRoutineOccurrenceMember(instanceID: UUID, memberID: UUID, requestBody: Data) async throws -> RoutineOccurrenceMutation {
        sent.append(requestBody)
        if let beforePut { await beforePut() }
        if let putFailure { throw putFailure }
        let command = try RoutineOccurrenceValidation.decode(RoutineOccurrenceCommand.self, from: requestBody)
        return .init(operationID: command.operationID, replayed: sent.count > 1,
            occurrence: F.snapshot(revision: 2, mode: .keepOpen))
    }
    func setSnapshot(_ value: RoutineOccurrenceSnapshot) { snapshot = value }
    func setReadFailure(_ value: RoutineOccurrenceError?) { readFailure = value }
    func setPutFailure(_ value: RoutineOccurrenceError?) { putFailure = value }
    func setPageFailure(_ value: RoutineOccurrenceError?) { pageFailure = value }
    func setBeforeRead(_ value: (@MainActor @Sendable () -> Void)?) { beforeRead = value }
    func setBeforePut(_ value: (@MainActor @Sendable () -> Void)?) { beforePut = value }
    func setBeforePage(_ value: (@MainActor @Sendable () -> Void)?) { beforePage = value }
    func setEmptyDelta(_ value: Bool) { emptyDelta = value }
    func bodies() -> [Data] { sent }
    func lookups() -> [[UUID]] { lookupIDs }
    func readCount() -> Int { reads }
    func pageKinds() -> [String] { kinds }
}
#endif
