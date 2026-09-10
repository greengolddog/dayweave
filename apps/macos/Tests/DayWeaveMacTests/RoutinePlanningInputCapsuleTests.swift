import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Encrypted fixed routine planning input custody", .serialized)
@MainActor
struct RoutinePlanningInputCapsuleTests {
    private typealias W = RoutinePlanningWitnessTestFixtures
    private typealias O = RoutineOccurrenceTestFixtures
    private static let origin = try! DayWeaveAPIBaseURL("https://api.example.test/").canonicalConfigurationIdentifier
    private static let binding = origin + "|auth=static-v1:" + String(repeating: "a", count: 64)
    private static let date = SchedulerHelperRFC3339.date(from: "2026-09-10T09:00:01Z")!

    @Test("exact original bytes and complete fixed input survive encrypted restart without a process admission")
    func encryptedRestart() throws {
        let f = try Fixture(); defer { f.remove() }
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let capsule = try f.capsule(fence)
        let prior = try f.persistence.load()
        try f.planner.commitRoutinePlanningInputCapsule(capsule, expected: fence, habitCheckpoint: nil)
        let encrypted = try Data(contentsOf: f.file)
        #expect(encrypted.range(of: Data("Synthetic fixed input".utf8)) == nil)
        #expect(encrypted.range(of: Data("routinePlanningInputCapsule".utf8)) == nil)
        let restarted = f.restart()
        #expect(restarted.routinePlanningInputCapsule == capsule)
        #expect(restarted.routinePlanningInputCapsule?.originalRequestBody == capsule.originalRequestBody)
        #expect(restarted.routineOccurrenceState == prior?.routineOccurrenceState)
        #expect(restarted.pendingSchedulePublication == prior?.pendingSchedulePublication)
        #expect(restarted.localScheduleCompositionProvenance == nil)
        #expect(restarted.requiresRemoteRoutineOccurrenceComposition)
        #expect(restarted.routinePlanningInputCapsuleIssue(origin: Self.origin,
            configurationIdentifier: Self.binding, habitCheckpoint: nil, at: Self.date) == nil)
        #expect(throws: RoutinePlanningInputCapsuleError.superseded) {
            try restarted.requireRoutinePlanningInputFence(fence, habitCheckpoint: nil)
        }
    }

    @Test("capsules retain and require the complete canonical API base path")
    func basePathBinding() throws {
        let f = try Fixture(basePath: "/dayweave"); defer { f.remove() }
        let capsule = try f.save()
        #expect(capsule.origin == Self.origin + "/dayweave")
        #expect(capsule.configurationIdentifier == f.binding)
        #expect(capsule.configurationIdentifier.hasPrefix(Self.origin + "/dayweave|auth="))
        let restarted = f.restart()
        #expect(restarted.routinePlanningInputCapsule == capsule)
        #expect(restarted.routinePlanningInputCapsuleIssue(origin: f.origin,
            configurationIdentifier: f.binding, habitCheckpoint: nil, at: Self.date) == nil)
        for changedOrigin in [Self.origin, Self.origin + "/other", "https://other.example.test/dayweave"] {
            #expect(throws: RoutinePlanningInputCapsuleError.invalidData) {
                try RoutinePlanningInputCapsule(origin: changedOrigin,
                    configurationIdentifier: capsule.configurationIdentifier, originalRequestBody: capsule.originalRequestBody,
                    request: capsule.request, witness: capsule.witness, canonicalItems: capsule.canonicalItems,
                    environment: capsule.environment, capturedAt: capsule.capturedAt)
            }
        }
    }

    @Test("UI readiness is read-only and an owned canonical lock does not mutate the input fence")
    func readinessAndOwnedLock() throws {
        let f = try Fixture(); defer { f.remove() }
        let bytes = try Data(contentsOf: f.file)
        #expect(f.planner.routinePlanningInputPreparationIssue(habitCheckpoint: nil) == nil)
        #expect(try Data(contentsOf: f.file) == bytes)
        #expect(f.planner.beginCanonicalSync())
        defer { f.planner.endCanonicalSync() }
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        try f.planner.requireRoutinePlanningInputFence(fence, habitCheckpoint: nil)
        try f.planner.commitRoutinePlanningInputCapsule(f.capsule(fence), expected: fence, habitCheckpoint: nil)
    }

    @Test("read and privacy generation invalidation keeps the prior artifact and rejects late capture")
    func supersededCapture() throws {
        let f = try Fixture(); defer { f.remove() }
        let original = try f.save()
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let candidate = try f.capsule(fence)
        let bytes = try Data(contentsOf: f.file)
        f.planner.invalidateRoutineOccurrencePlanningEvidence()
        #expect(throws: RoutinePlanningInputCapsuleError.superseded) {
            try f.planner.commitRoutinePlanningInputCapsule(candidate, expected: fence, habitCheckpoint: nil)
        }
        #expect(f.planner.routinePlanningInputCapsule == original)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("same-binding later responses cannot change the first authenticated workspace or user pin")
    func scopePin() throws {
        let f = try Fixture(); defer { f.remove() }
        let original = try f.save()
        for field in ["workspace_id", "user_id"] {
            let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
            var wire = W.witnessObject(); wire["execution_snapshot_revision"] = 0
            wire[field] = UUID().uuidString.lowercased()
            let witness = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: W.data(wire))
            let changed = try f.capsule(fence, witness: witness)
            #expect(throws: RoutinePlanningInputCapsuleError.scopeChanged) {
                try f.planner.commitRoutinePlanningInputCapsule(changed, expected: fence, habitCheckpoint: nil)
            }
            #expect(f.planner.routinePlanningInputCapsule == original)
        }
    }

    @Test("competing encrypted writers cannot replace capsule custody or any journal preimage")
    func competingWriter() throws {
        let f = try Fixture(); defer { f.remove() }
        let original = try f.save()
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let candidate = try f.capsule(fence)
        let competitor = f.restart(); competitor.flushPersistence()
        let committed = try Data(contentsOf: f.file)
        #expect(throws: PlannerPersistenceError.self) {
            try f.planner.commitRoutinePlanningInputCapsule(candidate, expected: fence, habitCheckpoint: nil)
        }
        #expect(f.planner.routinePlanningInputCapsule == original)
        #expect(try Data(contentsOf: f.file) == committed)
        #expect(try f.persistence.load()?.routinePlanningInputCapsule == original)
    }

    @Test("combined snapshot overflow fails admission without claiming publication-only reserve")
    func combinedBudget() throws {
        let f = try Fixture(messagePadding: 2_500_000); defer { f.remove() }
        let original = try f.save()
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let oversized = try f.capsule(fence, requestPadding: 11 * 1_024 * 1_024)
        try oversized.validate() // Intrinsically valid; the complete planner does not fit.
        let bytes = try Data(contentsOf: f.file)
        #expect(EncryptedPlannerPersistence.maximumPlaintextBytes(for: try #require(try f.persistence.load())) == 16 * 1_024 * 1_024)
        #expect(throws: PlannerPersistenceError.self) {
            try f.planner.commitRoutinePlanningInputCapsule(oversized, expected: fence, habitCheckpoint: nil)
        }
        #expect(f.planner.routinePlanningInputCapsule == original)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("new recovery latches and outboxes block preparation without discarding saved input")
    func pendingRecovery() throws {
        let f = try Fixture(); defer { f.remove() }
        let original = try f.save(), prior = f.planner.routineOccurrenceState
        var changed = prior; changed.needsRemoteScheduleCatchUp = true
        try f.planner.commitRoutineOccurrenceState(changed, replacing: prior)
        let bytes = try Data(contentsOf: f.file)
        #expect(f.planner.routinePlanningInputPreparationIssue(habitCheckpoint: nil) == .pendingRecovery)
        #expect(throws: RoutinePlanningInputCapsuleError.pendingRecovery) {
            try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        }
        f.planner.resetCanonicalSyncState()
        #expect(f.planner.routinePlanningInputCapsule == original)
        #expect(f.planner.routineOccurrenceState == changed)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("stale clocks and changed profiles preserve history; explicit safe reset clears scoped capsule")
    func staleEligibilityAndReset() throws {
        let f = try Fixture(); defer { f.remove() }
        let original = try f.save()
        #expect(f.planner.routinePlanningInputCapsuleIssue(origin: Self.origin, configurationIdentifier: Self.binding,
            habitCheckpoint: nil, at: Self.date.addingTimeInterval(-1)) == .clockChanged)
        #expect(f.planner.routinePlanningInputCapsuleIssue(origin: Self.origin, configurationIdentifier: Self.binding,
            habitCheckpoint: nil, at: Self.date.addingTimeInterval(86_400)) == .clockChanged)
        try f.planner.updateScheduleProfile(ScheduleProfile.legacyDefault(timezoneName: "UTC", protectedFreeMinutes: 60))
        #expect(f.planner.routinePlanningInputCapsuleIssue(origin: Self.origin, configurationIdentifier: Self.binding,
            habitCheckpoint: nil, at: Self.date) == .inputChanged)
        #expect(f.planner.routinePlanningInputCapsule == original)
        f.planner.resetCanonicalSyncState()
        #expect(f.planner.routinePlanningInputCapsule == nil)
        #expect(try f.persistence.load()?.routinePlanningInputCapsule == nil)
    }

    @Test("schema28 migration retains exact submitted occurrence and canonical authoring recovery")
    func schema28Migration() throws {
        let f = try Fixture(); defer { f.remove() }
        let authoring = try f.planner.enqueueCanonicalCreate(itemID: UUID(),
            draft: .init(title: "Synthetic migration intent", timezoneName: "UTC"))
        #expect(f.planner.beginCanonicalSync())
        _ = try f.planner.bindCanonicalAuthoringMutation(authoring.id, configurationIdentifier: Self.binding)
        _ = try f.planner.markCanonicalAuthoringMutationSubmitted(authoring.id)
        f.planner.endCanonicalSync()
        let command = O.command()
        let journal = RoutineOccurrenceJournal(instanceID: O.instanceID, memberID: O.rootID,
            configurationIdentifier: Self.binding, command: command,
            requestBody: Data(" \n\t".utf8) + (try command.bytes()), createdAt: Self.date,
            wasSensitive: true)
        let prior = f.planner.routineOccurrenceState
        var ledger = prior
        try ledger.observe(O.snapshot(), configurationIdentifier: Self.binding, at: Self.date)
        try ledger.enqueue(journal)
        try ledger.markSubmitted(journal)
        try f.planner.commitRoutineOccurrenceState(ledger, replacing: prior)
        var source = try f.object(); source["schemaVersion"] = 28; source.removeValue(forKey: "routinePlanningInputCapsule")
        _ = try f.write(W.data(source))
        let migrated = try #require(try f.persistence.load())
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion && migrated.routinePlanningInputCapsule == nil)
        #expect(migrated.routineOccurrenceState?.journals == ledger.journals)
        #expect(migrated.routineOccurrenceState?.journals.first?.hasBeenSubmitted == true)
        #expect(migrated.routineOccurrenceState?.journals.first?.requestBody == journal.requestBody)
        #expect(migrated.pendingCanonicalAuthoringMutations == f.planner.pendingCanonicalAuthoringMutations)
        var restored = try f.object(migrated); restored["schemaVersion"] = 28; restored.removeValue(forKey: "routinePlanningInputCapsule")
        #expect(NSDictionary(dictionary: source).isEqual(to: restored))
    }

    @Test("predecessors reject capsule injection and malformed current capsule preserves encrypted bytes")
    func closedMigrationBoundary() throws {
        let f = try Fixture(); defer { f.remove() }
        _ = try f.save()
        let original = try f.object()
        for field in ["routinePlanningInputCapsule", "routine_planning_input_capsule", "planningWitnessLease"] {
            var predecessor = original; predecessor["schemaVersion"] = 28
            predecessor.removeValue(forKey: "routinePlanningInputCapsule"); predecessor[field] = NSNull()
            let encrypted = try f.write(W.data(predecessor))
            #expect(throws: PlannerPersistenceError.self) { try f.persistence.load() }
            #expect(try Data(contentsOf: f.file) == encrypted)
        }
        var malformed = original, capsule = try #require(original["routinePlanningInputCapsule"] as? [String: Any])
        capsule["restoredReviewLease"] = true; malformed["routinePlanningInputCapsule"] = capsule
        let encrypted = try f.write(W.data(malformed))
        #expect(throws: PlannerPersistenceError.self) { try f.persistence.load() }
        #expect(try Data(contentsOf: f.file) == encrypted)
    }

    @MainActor
    struct Fixture {
        let directory: URL, file: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        let origin: String
        let binding: String
        private let keyData = Data(repeating: 67, count: 32)
        init(messagePadding: Int = 0, basePath: String = "") throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveInputCapsule-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            file = directory.appendingPathComponent("synthetic.encrypted")
            persistence = EncryptedPlannerPersistence(fileURL: file, key: try PlannerEncryptionKey(data: keyData))
            let referenceDate = RoutinePlanningInputCapsuleTests.date
            origin = try DayWeaveAPIBaseURL(RoutinePlanningInputCapsuleTests.origin + basePath).canonicalConfigurationIdentifier
            binding = origin + "|auth=static-v1:" + String(repeating: "a", count: 64)
            planner = PlannerStore(canonicalItems: try W.canonicalItems(), canonicalDeltaCursor: "canonical-complete",
                canonicalConfigurationIdentifier: binding,
                routineOccurrenceState: .init(configurationIdentifier: binding,
                    terminalDeltaCursor: W.cursor),
                scheduleProfile: try ScheduleProfile.legacyDefault(timezoneName: "UTC", protectedFreeMinutes: 30),
                lastScheduleMessage: "Synthetic fixed input" + String(repeating: "x", count: messagePadding),
                persistence: persistence, restoreFromPersistence: false,
                now: { referenceDate })
            planner.flushPersistence()
            if let error = planner.persistenceError { throw error }
        }
        func capsule(_ fence: RoutinePlanningInputCaptureFence, witness: RoutinePlanningWitness? = nil,
                     requestPadding: Int = 3) throws -> RoutinePlanningInputCapsule {
            var wire = W.witnessObject(); wire["execution_snapshot_revision"] = 0
            let request = try W.request()
            return try .init(origin: origin,
                configurationIdentifier: binding,
                originalRequestBody: Data(repeating: 32, count: requestPadding) + W.data(W.requestObject()), request: request,
                witness: witness ?? RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: W.data(wire)),
                canonicalItems: fence.canonicalItems, environment: fence.environment, capturedAt: RoutinePlanningInputCapsuleTests.date)
        }
        func save() throws -> RoutinePlanningInputCapsule {
            let fence = try planner.captureRoutinePlanningInputFence(habitCheckpoint: nil), value = try capsule(fence)
            try planner.commitRoutinePlanningInputCapsule(value, expected: fence, habitCheckpoint: nil)
            return value
        }
        func restart() -> PlannerStore {
            let referenceDate = RoutinePlanningInputCapsuleTests.date
            return PlannerStore(persistence: persistence, now: { referenceDate })
        }
        func object() throws -> [String: Any] { try object(#require(try persistence.load())) }
        func object(_ snapshot: PlannerSnapshot) throws -> [String: Any] {
            let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
            return try #require(JSONSerialization.jsonObject(with: encoder.encode(snapshot)) as? [String: Any])
        }
        func write(_ plaintext: Data) throws -> Data {
            let box = try AES.GCM.seal(plaintext, using: SymmetricKey(data: keyData),
                authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8))
            let envelope = try W.data(["magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT", "formatVersion": 1,
                "cipher": "AES.GCM.256", "sealedSnapshot": try #require(box.combined).base64EncodedString()])
            try envelope.write(to: file); return envelope
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}
#endif
