import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Encrypted execution-locked routine display custody", .serialized)
@MainActor
struct RoutinePlanningDisplayPlanTests {
    private typealias F = RoutinePlanningInputCapsuleTests.Fixture
    private typealias O = RoutineOccurrenceTestFixtures
    private static let date = SchedulerHelperRFC3339.date(from: "2026-09-10T09:00:01Z")!

    @Test("display comparison permits only one-way withdrawal of two execution freshness flags")
    func freshnessComparisonIsDirectionalAndExact() throws {
        let f = try F(); defer { f.remove() }
        try installExecutionFreshness(on: f, verified: true)
        let saved = try RoutinePlanningInputEnvironment(planner: f.planner, habitCheckpoint: nil)
        let withdrawn = try changingEnvironment(saved) { root in
            var execution = root["executionState"] as! [String: Any]
            execution["historyVerified"] = false; execution["historyContinuityEstablished"] = false
            root["executionState"] = execution
        }
        #expect(saved != withdrawn)
        #expect(saved.matchesForDisplay(current: withdrawn))
        #expect(!withdrawn.matchesForDisplay(current: saved))
        #expect(withdrawn.matchesForDisplay(current: withdrawn))
        for (field, value) in [("revision", 1 as Any), ("historyWindowRevision", 1 as Any),
                               ("bindingIdentifier", "another-binding" as Any), ("deviceID", UUID().uuidString as Any)] {
            let changed = try changingEnvironment(withdrawn) { root in
                var execution = root["executionState"] as! [String: Any]
                execution[field] = value; root["executionState"] = execution
            }
            #expect(!saved.matchesForDisplay(current: changed))
        }
        let changedSource = try changingEnvironment(withdrawn) { $0["canonicalDeltaCursor"] = "other-source-checkpoint" }
        #expect(!saved.matchesForDisplay(current: changedSource))
    }

    @Test("offline freshness withdrawal allows only display custody and keeps full runtime fences exact")
    func withdrawnFreshnessIsDisplayOnly() throws {
        let f = try F(); defer { f.remove() }
        try installExecutionFreshness(on: f, verified: true)
        let capsule = try f.save(), display = try artifact(capsule)
        let beforeRead = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        var withdrawn = f.planner.executionState
        withdrawn.historyVerified = false; withdrawn.historyContinuityEstablished = false
        try f.planner.persistExecutionState(withdrawn)
        #expect(f.planner.routinePlanningInputCapsuleIssue(origin: f.origin, configurationIdentifier: f.binding,
            habitCheckpoint: nil, at: Self.date) == .inputChanged)
        #expect(f.planner.routinePlanningDisplayCapsuleIssue(origin: f.origin, configurationIdentifier: f.binding,
            habitCheckpoint: nil, at: Self.date) == nil)
        #expect(throws: RoutinePlanningInputCapsuleError.superseded) {
            try f.planner.requireRoutinePlanningInputFence(beforeRead, habitCheckpoint: nil)
        }
        let displayFence = try f.planner.currentRoutinePlanningInputFence(habitCheckpoint: nil)
        _ = try f.planner.commitRoutinePlanningDisplayPlan(display, expected: displayFence, habitCheckpoint: nil, isCurrent: { true })
        #expect(f.planner.executionState == withdrawn)
        #expect(try f.persistence.load()?.executionState == withdrawn)
        #expect(f.planner.routinePlanningInputCapsule?.environment.executionState.historyVerified == true)
        #expect(f.planner.routinePlanningDisplayPlan == display)
        #expect(f.planner.localScheduleCompositionProvenance == nil)
        #expect(f.planner.requiresRemoteRoutineOccurrenceComposition)
        var newer = withdrawn; newer.revision = 1
        try f.planner.persistExecutionState(newer)
        #expect(f.planner.routinePlanningDisplayCapsuleIssue(origin: f.origin, configurationIdentifier: f.binding,
            habitCheckpoint: nil, at: Self.date) == .inputChanged)
    }

    @Test("a first freshness grant is not silently folded into a capsule captured without it")
    func freshnessGrantRemainsChangedInput() throws {
        let f = try F(); defer { f.remove() }
        try installExecutionFreshness(on: f, verified: false)
        let capsule = try f.save()
        var granted = f.planner.executionState
        granted.historyVerified = true; granted.historyContinuityEstablished = true
        try f.planner.persistExecutionState(granted)
        #expect(f.planner.routinePlanningDisplayCapsuleIssue(origin: f.origin, configurationIdentifier: f.binding,
            habitCheckpoint: nil, at: Self.date) == .inputChanged)
        #expect(f.planner.routinePlanningInputCapsule == capsule)
        #expect(f.planner.routinePlanningDisplayPlan == nil)
    }

    @Test("raw helper bytes and complete capsule binding survive model round-trip without Date rewriting")
    func exactRawModel() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(), result = try composition(capsule)
        let display = try RoutinePlanningDisplayPlan(capsule: capsule, composition: result, generatedAt: Self.date)
        let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .millisecondsSince1970
        let restored = try decoder.decode(RoutinePlanningDisplayPlan.self, from: encoder.encode(display))
        #expect(restored == display)
        #expect(restored.rawOutput == result.rawOutput)
        #expect(restored.rawOutput.starts(with: Data(" \n\t".utf8)))
        #expect(restored.rawOutput.range(of: Data("09:00:00.123456Z".utf8)) != nil)
        #expect(try restored.validatedComposition(capsule: capsule) == result)
        #expect(restored.capsuleBinding.canonicalItems == capsule.canonicalItems)
    }

    @Test("typed projections, raw output and exact capsule may not be substituted independently")
    func exactPairing() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(), result = try composition(capsule)
        let display = try RoutinePlanningDisplayPlan(capsule: capsule, composition: result, generatedAt: Self.date)
        let forged = RoutineOccurrenceLocalComposition(composition: result.composition,
            occurrenceSnapshotRevision: result.occurrenceSnapshotRevision + 1, rawOutput: result.rawOutput)
        #expect(throws: RoutinePlanningDisplayPlanError.invalidData) {
            try RoutinePlanningDisplayPlan(capsule: capsule, composition: forged, generatedAt: Self.date)
        }
        let fence = try f.planner.currentRoutinePlanningInputFence(habitCheckpoint: nil)
        let changedCapsule = try f.capsule(fence, requestPadding: 7)
        #expect(throws: RoutinePlanningDisplayPlanError.capsuleChanged) {
            try display.validatedComposition(capsule: changedCapsule)
        }
        #expect(throws: RoutinePlanningDisplayPlanError.invalidData) {
            try RoutinePlanningDisplayPlan(capsule: capsule, composition: result,
                generatedAt: Self.date.addingTimeInterval(-1))
        }
    }

    @Test("save changes only the independent artifact; encrypted restart never restores its runtime fence")
    func isolatedSaveAndRestart() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save()
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let before = try #require(try f.persistence.load())
        let display = try artifact(capsule)
        let postSave = try f.planner.commitRoutinePlanningDisplayPlan(display, expected: fence,
            habitCheckpoint: nil, isCurrent: { true })
        let bytes = try Data(contentsOf: f.file)
        try f.planner.requireRoutinePlanningInputFence(postSave, habitCheckpoint: nil)
        _ = try f.planner.currentRoutinePlanningInputFence(habitCheckpoint: nil)
        #expect(try Data(contentsOf: f.file) == bytes) // Presentation checks do not save.
        let after = try #require(try f.persistence.load())
        let beforeFields = try withoutDisplay(before, fixture: f)
        let afterFields = try withoutDisplay(after, fixture: f)
        #expect(beforeFields == afterFields)
        #expect(f.planner.blocks == before.blocks)
        #expect(f.planner.publishedScheduleProof == before.publishedScheduleProof)
        #expect(f.planner.routineOccurrenceState == before.routineOccurrenceState)
        #expect(f.planner.localScheduleCompositionProvenance == nil)
        #expect(f.planner.requiresRemoteRoutineOccurrenceComposition)
        #expect(bytes.range(of: display.rawOutput) == nil)
        let restarted = f.restart()
        #expect(restarted.routinePlanningDisplayPlan == display)
        #expect(restarted.routinePlanningInputCapsule == capsule)
        #expect(throws: RoutinePlanningInputCapsuleError.superseded) {
            try restarted.requireRoutinePlanningInputFence(postSave, habitCheckpoint: nil)
        }
        #expect(restarted.localScheduleCompositionProvenance == nil)
        #expect(restarted.requiresRemoteRoutineOccurrenceComposition)
    }

    @Test("a late private fence after bounded preflight restores the prior artifact before disk IO")
    func latePrivateFenceRollback() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(), prior = try saveDisplay(capsule, fixture: f)
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let replacement = try artifact(capsule, suffix: " \n")
        let bytes = try Data(contentsOf: f.file)
        var calls = 0
        #expect(throws: RoutinePlanningInputCapsuleError.superseded) {
            try f.planner.commitRoutinePlanningDisplayPlan(replacement, expected: fence, habitCheckpoint: nil) {
                calls += 1
                return calls < 3
            }
        }
        #expect(calls == 3)
        #expect(f.planner.routinePlanningDisplayPlan == prior)
        #expect(f.planner.routinePlanningInputCapsule == capsule)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("generation changes, recovery latches and reset never consume old display custody")
    func changedEvidence() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(), prior = try saveDisplay(capsule, fixture: f)
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        f.planner.invalidateRoutineOccurrencePlanningEvidence()
        #expect(throws: RoutinePlanningInputCapsuleError.superseded) {
            try f.planner.commitRoutinePlanningDisplayPlan(prior, expected: fence, habitCheckpoint: nil, isCurrent: { true })
        }
        let ledger = f.planner.routineOccurrenceState
        var pending = ledger; pending.needsRemoteScheduleCatchUp = true
        try f.planner.commitRoutineOccurrenceState(pending, replacing: ledger)
        let bytes = try Data(contentsOf: f.file)
        #expect(throws: RoutinePlanningInputCapsuleError.pendingRecovery) {
            try f.planner.currentRoutinePlanningInputFence(habitCheckpoint: nil)
        }
        f.planner.resetCanonicalSyncState()
        #expect(f.planner.routinePlanningDisplayPlan == prior)
        #expect(f.planner.routineOccurrenceState == pending)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("replacing fixed input retains old display history but exact capsule pairing makes it ineligible")
    func replacedCapsuleRetainsHistory() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(), prior = try saveDisplay(capsule, fixture: f)
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let replacement = try f.capsule(fence, requestPadding: 7)
        try f.planner.commitRoutinePlanningInputCapsule(replacement, expected: fence, habitCheckpoint: nil)
        #expect(f.planner.routinePlanningDisplayPlan == prior)
        #expect(try f.persistence.load()?.routinePlanningDisplayPlan == prior)
        #expect(throws: RoutinePlanningDisplayPlanError.capsuleChanged) {
            try prior.validatedComposition(capsule: replacement)
        }
        f.planner.resetCanonicalSyncState()
        #expect(f.planner.routinePlanningDisplayPlan == nil)
        #expect(f.planner.routinePlanningInputCapsule == nil)
    }

    @Test("full capsule duplication cannot borrow publication headroom or evict the previous display")
    func combinedCapacityRollback() throws {
        let f = try F(messagePadding: 6 * 1_024 * 1_024); defer { f.remove() }
        let capsule = try f.save(), prior = try saveDisplay(capsule, fixture: f)
        let inputFence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let large = try f.capsule(inputFence, requestPadding: 4 * 1_024 * 1_024)
        try f.planner.commitRoutinePlanningInputCapsule(large, expected: inputFence, habitCheckpoint: nil)
        let display = try artifact(large)
        try display.validate() // Intrinsic artifact fits; duplicate full custody plus planner does not.
        let displayFence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let bytes = try Data(contentsOf: f.file)
        #expect(throws: PlannerPersistenceError.self) {
            try f.planner.commitRoutinePlanningDisplayPlan(display, expected: displayFence, habitCheckpoint: nil, isCurrent: { true })
        }
        #expect(f.planner.routinePlanningDisplayPlan == prior)
        #expect(f.planner.routinePlanningInputCapsule == large)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("competing encrypted writers cannot replace display history after a stale disk CAS")
    func competingWriter() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(), prior = try saveDisplay(capsule, fixture: f)
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        let replacement = try artifact(capsule, suffix: " \n")
        let competitor = f.restart(); competitor.flushPersistence()
        let bytes = try Data(contentsOf: f.file)
        #expect(throws: PlannerPersistenceError.self) {
            try f.planner.commitRoutinePlanningDisplayPlan(replacement, expected: fence, habitCheckpoint: nil, isCurrent: { true })
        }
        #expect(f.planner.routinePlanningDisplayPlan == prior)
        #expect(try Data(contentsOf: f.file) == bytes)
    }

    @Test("schema29 migration preserves exact capsule, submitted requests and terminal catch-up targets")
    func schema29Migration() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save()
        let authoring = try f.planner.enqueueCanonicalCreate(itemID: UUID(),
            draft: .init(title: "Synthetic display migration intent", timezoneName: "UTC"))
        #expect(f.planner.beginCanonicalSync())
        _ = try f.planner.bindCanonicalAuthoringMutation(authoring.id, configurationIdentifier: f.binding)
        _ = try f.planner.markCanonicalAuthoringMutationSubmitted(authoring.id)
        f.planner.endCanonicalSync()
        let old = f.planner.routineOccurrenceState, command = O.command()
        var ledger = old
        try ledger.observe(O.snapshot(), configurationIdentifier: f.binding, at: Self.date)
        let journal = RoutineOccurrenceJournal(instanceID: O.instanceID, memberID: O.rootID,
            configurationIdentifier: f.binding, command: command,
            requestBody: Data(" \n\t".utf8) + (try command.bytes()), createdAt: Self.date, wasSensitive: true)
        try ledger.enqueue(journal); try ledger.markSubmitted(journal)
        ledger.needsRemoteScheduleCatchUp = true
        try f.planner.commitRoutineOccurrenceState(ledger, replacing: old)
        var original = try f.object(); original["schemaVersion"] = 29
        original.removeValue(forKey: "routinePlanningDisplayPlan")
        _ = try f.write(JSONSerialization.data(withJSONObject: original, options: [.sortedKeys]))
        let migrated = try #require(try f.persistence.load())
        #expect(migrated.schemaVersion == 30)
        #expect(migrated.routinePlanningInputCapsule == capsule)
        #expect(migrated.routinePlanningDisplayPlan == nil)
        #expect(migrated.routineOccurrenceState == ledger)
        #expect(migrated.routineOccurrenceState?.journals.first?.requestBody == journal.requestBody)
        #expect(migrated.pendingCanonicalAuthoringMutations == f.planner.pendingCanonicalAuthoringMutations)
        var restored = try f.object(migrated); restored["schemaVersion"] = 29
        restored.removeValue(forKey: "routinePlanningDisplayPlan")
        #expect(NSDictionary(dictionary: original).isEqual(to: restored))
    }

    @Test("predecessor injection and malformed current display preserve encrypted bytes")
    func closedPersistenceBoundary() throws {
        let f = try F(); defer { f.remove() }
        let capsule = try f.save(); _ = try saveDisplay(capsule, fixture: f)
        let original = try f.object()
        for key in ["routinePlanningDisplayPlan", "routine_planning_display_plan", "routinePlanningPresentationLease"] {
            var changed = original; changed["schemaVersion"] = 29
            changed.removeValue(forKey: "routinePlanningDisplayPlan"); changed[key] = NSNull()
            let bytes = try f.write(JSONSerialization.data(withJSONObject: changed, options: [.sortedKeys]))
            #expect(throws: PlannerPersistenceError.self) { try f.persistence.load() }
            #expect(try Data(contentsOf: f.file) == bytes)
        }
        for (key, value) in [("restoredAdmission", true as Any), ("schemaVersion", 2 as Any),
                             ("rawOutput", Data("PRIVATE INVALID OUTPUT".utf8).base64EncodedString() as Any)] {
            var changed = original
            var artifact = try #require(original["routinePlanningDisplayPlan"] as? [String: Any])
            artifact[key] = value; changed["routinePlanningDisplayPlan"] = artifact
            let bytes = try f.write(JSONSerialization.data(withJSONObject: changed, options: [.sortedKeys]))
            #expect(throws: PlannerPersistenceError.self) { try f.persistence.load() }
            #expect(try Data(contentsOf: f.file) == bytes)
        }
    }

    private func artifact(_ capsule: RoutinePlanningInputCapsule, suffix: String = "\n") throws -> RoutinePlanningDisplayPlan {
        try .init(capsule: capsule, composition: composition(capsule, suffix: suffix), generatedAt: Self.date)
    }

    private func saveDisplay(_ capsule: RoutinePlanningInputCapsule, fixture f: F) throws -> RoutinePlanningDisplayPlan {
        let display = try artifact(capsule)
        let fence = try f.planner.captureRoutinePlanningInputFence(habitCheckpoint: nil)
        _ = try f.planner.commitRoutinePlanningDisplayPlan(display, expected: fence, habitCheckpoint: nil, isCurrent: { true })
        return display
    }

    /// Deliberately synthetic decoder/store fixture, not proof of scheduler output.
    /// The root-owned real-helper gate independently verifies scheduler parity.
    private func composition(_ capsule: RoutinePlanningInputCapsule, suffix: String = "\n") throws -> RoutineOccurrenceLocalComposition {
        let witness = capsule.witness
        let schedule = try #require(try JSONSerialization.jsonObject(with: RoutinePlanningWitnessValidation.encode(witness.schedule)) as? [String: Any])
        let plan: [String: Any] = ["as_of": try #require(schedule["as_of"]),
            "horizon_start": try #require(schedule["horizon_start"]), "horizon_end": try #require(schedule["horizon_end"]),
            "blocks": [], "unscheduled": [], "decisions": [], "violations": [], "occurrences": [],
            "score": ["scheduled_minutes": 0, "unscheduled_minutes": 0, "soft_penalty": 0, "moved_minutes": 0]]
        let body: [String: Any] = ["protocol": "dayweave.scheduler.helper", "version": 2,
            "result": ["type": "composition", "composition": [
                "local_input_fingerprint": witness.localInputFingerprint,
                "source_item_revisions": Dictionary(uniqueKeysWithValues: witness.sourceItemRevisions.map { ($0.key.uuidString.lowercased(), $0.value) }),
                "source_item_count": witness.sourceItemRevisions.count, "accepted_item_count": witness.sourceItemRevisions.count,
                "rejected_items": [], "ignored_previous_assignments": [],
                "occurrence_snapshot_revision": witness.occurrenceLifecycle.snapshotRevision, "plan": plan]]]
        let raw = Data(" \n\t".utf8) + (try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])) + Data(suffix.utf8)
        return try SchedulerHelperClient.decodeOccurrenceOutput(
            .init(standardOutput: raw, standardError: Data(), termination: .exited(0)), witness: witness)
    }

    private func withoutDisplay(_ snapshot: PlannerSnapshot, fixture: F) throws -> NSDictionary {
        var object = try fixture.object(snapshot)
        object.removeValue(forKey: "routinePlanningDisplayPlan"); object.removeValue(forKey: "savedAt")
        return NSDictionary(dictionary: object)
    }

    private func installExecutionFreshness(on fixture: F, verified: Bool) throws {
        var execution = fixture.planner.executionState
        execution.deviceID = UUID(uuidString: "10000000-0000-4000-8000-000000000099")!
        execution.bindingIdentifier = "static-v1:" + String(repeating: "a", count: 64)
        execution.historyWindowRevision = execution.revision
        execution.historyVerified = verified; execution.historyContinuityEstablished = verified
        try fixture.planner.persistExecutionState(execution)
    }

    private func changingEnvironment(_ environment: RoutinePlanningInputEnvironment,
                                     _ change: (inout [String: Any]) -> Void) throws -> RoutinePlanningInputEnvironment {
        let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
        var root = try #require(try JSONSerialization.jsonObject(with: encoder.encode(environment)) as? [String: Any])
        change(&root)
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .millisecondsSince1970
        return try decoder.decode(RoutinePlanningInputEnvironment.self,
            from: JSONSerialization.data(withJSONObject: root, options: [.sortedKeys]))
    }
}
#endif
