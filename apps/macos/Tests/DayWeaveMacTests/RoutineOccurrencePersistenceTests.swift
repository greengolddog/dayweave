import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Bounded occurrence encrypted recovery", .serialized)
@MainActor
struct RoutineOccurrencePersistenceTests {
    private typealias F = RoutineOccurrenceTestFixtures
    private static let date = Date(timeIntervalSince1970: 1_788_953_400)
    private static let configuration = (try! DayWeaveAPIBaseURL("https://api.example.test/"))
        .canonicalConfigurationIdentifier + "|auth=static-v1:" + String(repeating: "a", count: 64)

    private func journal(snapshot: RoutineOccurrenceSnapshot = F.snapshot(), operation: UUID = F.operationID,
        padding: Int = 3, submitted: Bool = false) throws -> RoutineOccurrenceJournal {
        let command = RoutineOccurrenceCommand(schemaVersion: 1, operationID: operation,
            expectedInstanceRevision: snapshot.aggregate.revision,
            expectedMemberRevision: snapshot.aggregate.members[0].revision, expectedEvidenceHash: snapshot.evidenceHash,
            action: .setPolicy(requiredForParent: true, mode: .keepOpen))
        return .init(instanceID: snapshot.aggregate.manifest.id, memberID: F.rootID,
            configurationIdentifier: Self.configuration, command: command,
            requestBody: Data(repeating: 32, count: padding) + (try command.bytes()), createdAt: Self.date,
            wasSensitive: true, hasBeenSubmitted: submitted)
    }
    private func pending() throws -> RoutineOccurrenceState {
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        try state.observe(F.snapshot(), configurationIdentifier: Self.configuration, at: Self.date)
        let intent = try journal(); try state.enqueue(intent); try state.markSubmitted(intent)
        return state
    }
    private func identified(_ base: RoutineOccurrenceSnapshot, _ number: Int, title: String? = nil) -> RoutineOccurrenceSnapshot {
        let m = base.aggregate.manifest
        let definitions = title.map { title in m.members.map { member in
            RoutineOccurrenceMemberDefinition(itemID: member.itemID, parentID: member.parentID, sourceRevision: member.sourceRevision,
                title: title, kind: member.kind, recurs: member.recurs, siblingOrder: member.siblingOrder,
                requiredForParent: member.requiredForParent, initialOpen: member.initialOpen)
        } } ?? m.members
        let manifest = RoutineOccurrenceManifest(schemaVersion: 1, id: F.id(30_000 + number), seriesItemID: m.seriesItemID,
            occurrenceID: UUID(uuidString: String(format: "00000000-0000-5000-8000-%012d", number))!, identity: m.identity,
            nominalStart: m.nominalStart, nominalEnd: m.nominalEnd, windowStart: m.windowStart, windowEnd: m.windowEnd,
            timezoneName: m.timezoneName, definitionHash: m.definitionHash, members: definitions)
        return .init(schemaVersion: 1, aggregate: .init(manifest: manifest, revision: base.aggregate.revision,
            members: base.aggregate.members), evidenceHash: base.evidenceHash, freshEditEligible: base.freshEditEligible,
            members: base.members)
    }
    private func page(_ snapshots: [RoutineOccurrenceSnapshot], cursor: String = "opaque-terminal", hasMore: Bool = false,
        firstSequence: UInt64 = 1) -> RoutineOccurrencePage {
        .init(schemaVersion: 1, changes: snapshots.enumerated().map {
            .init(sequence: firstSequence + UInt64($0.offset), occurrence: $0.element)
        }, cursor: cursor, hasMore: hasMore)
    }

    @Test("all predecessors reject injected occurrence state and proof, even explicit null")
    func predecessorInjection() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let original = try fixture.object()
        for version in 1...27 {
            for field in ["routineOccurrenceState", "routineOccurrenceReadAdmissions", "routine_occurrence_evidence_epoch",
                          "occurrenceLifecycleProof", "minimumCatchUpRevisions", "needsRemoteScheduleCatchUp"] {
                for value in [NSNull() as Any, ["synthetic": true]] {
                    var root = original; root["schemaVersion"] = version; root.removeValue(forKey: "routineOccurrenceState")
                    if version < 27 { root.removeValue(forKey: "itemCompletionState") }
                    if version < 26 { root.removeValue(forKey: "itemProgressState") }
                    root[field] = value
                    let encrypted = try fixture.write(JSONSerialization.data(withJSONObject: root))
                    #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
                    #expect(try Data(contentsOf: fixture.fileURL) == encrypted)
                }
            }
        }
    }

    @Test("schema27 preserves completion/progress bytes and publication/authoring journals")
    func schema27Migration() throws {
        let fixture = try Fixture(withAuthoring: true); defer { fixture.remove() }
        let completionCommand = ItemCompletionTestFixtures.command()
        let completion = ItemCompletionJournal(itemID: F.rootID, configurationIdentifier: Self.configuration,
            command: completionCommand, requestBody: Data(" \n\t".utf8) + (try completionCommand.bytes()),
            createdAt: Self.date, wasSensitive: true, hasBeenSubmitted: true, noEffectCode: nil)
        try fixture.planner.commitItemCompletionState(.init(configurationIdentifier: Self.configuration,
            observations: [], journals: [completion], needsCanonicalCatchUp: true), replacing: .empty)
        let progressCommand = ItemProgressCommand(expectedItemRevision: 7, expectedProgressRevision: 0,
            components: [.init(name: "Synthetic progress", value: .percentage(basisPoints: 4_250))])
        let progress = ItemProgressJournal(itemID: F.rootID, configurationIdentifier: Self.configuration,
            command: progressCommand, requestBody: Data("\n ".utf8) + (try progressCommand.bytes()), createdAt: Self.date,
            wasSensitive: true, hasBeenSubmitted: true, noEffectCode: nil)
        try fixture.planner.commitItemProgressState(.init(configurationIdentifier: Self.configuration,
            observations: [], journals: [progress]), replacing: .empty)
        var source = try fixture.object(); source["schemaVersion"] = 27; source.removeValue(forKey: "routineOccurrenceState")
        let publication = try GoogleSchedulePublicationRecoveryJournal(operationGeneration: 1,
            configurationIdentifier: Self.configuration, accountID: F.id(500), collectionID: F.id(501),
            expectedScheduleRevisionID: F.id(502), intentExpiresAt: Self.date.addingTimeInterval(1_800), createdAt: Self.date)
        let publicationEncoder = JSONEncoder(); publicationEncoder.dateEncodingStrategy = .millisecondsSince1970
        source["googleSchedulePublicationRecoveryJournal"] = try JSONSerialization.jsonObject(with: publicationEncoder.encode(publication))
        _ = try fixture.write(JSONSerialization.data(withJSONObject: source))
        let migrated = try #require(try fixture.persistence.load()).migratedToCurrentSchema()
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion && migrated.routineOccurrenceState == .empty)
        #expect(migrated.itemCompletionState?.journals == [completion])
        #expect(migrated.itemCompletionState?.needsCanonicalCatchUp == true)
        #expect(migrated.itemProgressState?.journals == [progress])
        #expect(migrated.googleSchedulePublicationRecoveryJournal == publication)
        #expect(migrated.pendingCanonicalAuthoringMutations?.first?.hasBeenSubmitted == true)
        var result = try fixture.object(migrated); result["schemaVersion"] = 27; result.removeValue(forKey: "routineOccurrenceState")
        #expect(NSDictionary(dictionary: source).isEqual(to: result))
        let schema27 = PlannerSnapshot(schemaVersion: 27, destination: nil, selectedBlockID: nil, blocks: [], suggestions: [],
            assistantMessages: [], lastScheduleMessage: "Synthetic", protectedFreeMinutes: 30, freezeHours: 1, showCompleted: true)
        #expect(schema27.itemCompletionState == .empty)
        #expect(try schema27.migratedToCurrentSchema().routineOccurrenceState == .empty)
    }

    @Test("schema28 requires all occurrence fields and rejects persisted runtime review proofs")
    func requiredClosedShape() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.planner.commitRoutineOccurrenceState(pending(), replacing: .empty)
        let original = try fixture.object(), ledger = try #require(original["routineOccurrenceState"] as? [String: Any])
        var variants: [[String: Any]] = []
        var missing = original; missing.removeValue(forKey: "routineOccurrenceState"); variants.append(missing)
        var null = original; null["routineOccurrenceState"] = NSNull(); variants.append(null)
        for key in ledger.keys {
            var incomplete = ledger; incomplete.removeValue(forKey: key)
            var root = original; root["routineOccurrenceState"] = incomplete; variants.append(root)
        }
        for key in ["freshReadEpoch", "isReadProof", "getLease"] {
            var injected = ledger; injected[key] = true
            var root = original; root["routineOccurrenceState"] = injected; variants.append(root)
        }
        for field in ["observations", "journals"] {
            let row = try #require((ledger[field] as? [[String: Any]])?.first)
            for key in row.keys {
                var incomplete = row; incomplete.removeValue(forKey: key)
                var value = ledger; value[field] = [incomplete]
                var root = original; root["routineOccurrenceState"] = value; variants.append(root)
            }
        }
        for root in variants {
            let encrypted = try fixture.write(JSONSerialization.data(withJSONObject: root))
            #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
            #expect(try Data(contentsOf: fixture.fileURL) == encrypted)
        }
    }

    @Test("encrypted restart retains exact request, instance target and historical receipt catch-up atomically")
    func exactRetryHistoricalReceipt() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        var state = try pending()
        let intent = try #require(state.journals.first)
        let newer = F.snapshot(revision: 3, mode: .keepOpen)
        try state.observe(newer, configurationIdentifier: Self.configuration, at: Self.date)
        try fixture.planner.commitRoutineOccurrenceState(state, replacing: .empty)
        let restarted = fixture.restart()
        #expect(restarted.routineOccurrenceState == state)
        #expect(state.journals.first?.requestBody == intent.requestBody)
        #expect(state.journals.first?.instanceID == F.instanceID && F.instanceID != F.plannerID)
        #expect(try Data(contentsOf: fixture.fileURL).range(of: intent.requestBody) == nil)
        var settled = restarted.routineOccurrenceState
        try settled.settleReceipt(F.mutation(), for: intent, at: Self.date)
        try restarted.commitRoutineOccurrenceState(settled, replacing: state)
        #expect(settled.observations.first?.snapshot == newer)
        #expect(settled.minimumCatchUpRevisions == [F.instanceID: 2] && settled.journals.isEmpty)
        #expect(settled.needsRemoteScheduleCatchUp)
        #expect(fixture.restart().routineOccurrenceState == settled)
        #expect(fixture.restart().canonicalItems == fixture.planner.canonicalItems)
    }

    @Test("equal aggregate revisions may refresh ephemeral evidence but cannot change immutable content")
    func sameRevisionEvidence() throws {
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        let original = F.snapshot()
        try state.observe(original, configurationIdentifier: Self.configuration, at: Self.date)
        let ephemeral = RoutineOccurrenceSnapshot(schemaVersion: 1, aggregate: original.aggregate,
            evidenceHash: "sha256:" + String(repeating: "2", count: 64), freshEditEligible: false,
            members: original.members.map { .init(itemID: $0.itemID, counts: $0.counts,
                occurrenceEvidenceRequired: $0.occurrenceEvidenceRequired, reason: .policyReviewed) })
        try state.observe(ephemeral, configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state.observations.first?.snapshot == ephemeral)
        let retained = state
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.observe(F.snapshot(mode: .keepOpen), configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == retained)
    }

    @Test("sibling operations conflict per instance and submitted or mismatched receipts retain custody")
    func siblingAndReceiptCustody() throws {
        var state = try pending(); let prior = state, intent = try #require(state.journals.first)
        let siblingCommand = RoutineOccurrenceCommand(schemaVersion: 1, operationID: F.id(201), expectedInstanceRevision: 1,
            expectedMemberRevision: 1, expectedEvidenceHash: F.hash, action: .setOutcome(status: .completed))
        let sibling = RoutineOccurrenceJournal(instanceID: F.instanceID, memberID: F.childID,
            configurationIdentifier: Self.configuration, command: siblingCommand, requestBody: try siblingCommand.bytes(),
            createdAt: Self.date, wasSensitive: true)
        #expect(throws: RoutineOccurrenceStateError.self) { try state.enqueue(sibling) }
        #expect(throws: RoutineOccurrenceStateError.self) { try state.discardUnsubmittedOrRejected(intent) }
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.settleReceipt(.init(operationID: F.id(999), replayed: true, occurrence: F.mutation().occurrence),
                for: intent, at: Self.date)
        }
        #expect(state == prior)
        try state.markNoEffect("routine_occurrence_instance_stale", for: intent)
        try state.discardUnsubmittedOrRejected(#require(state.journals.first))
        #expect(state.journals.isEmpty && !state.needsRemoteScheduleCatchUp)
    }

    @Test("only terminal chain reads discharge receipt revisions, and captured state rejects late convergence")
    func terminalOnlyCatchUp() throws {
        var state = try pending(), started = state
        let intent = try #require(state.journals.first)
        try state.settleReceipt(F.mutation(), for: intent, at: Self.date)
        let receiptState = state
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges([page([F.mutation().occurrence])], replacing: started,
                configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges([page([F.mutation().occurrence], hasMore: true)], replacing: receiptState,
                configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == receiptState)
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges([page([])], replacing: receiptState, configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == receiptState && state.terminalDeltaCursor == nil)
        #expect(state.minimumCatchUpRevisions == [F.instanceID: 2])
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.acknowledgeRemoteScheduleCatchUp(replacing: state, configurationIdentifier: Self.configuration)
        }
        started = state
        try state.installTerminalChanges([page([F.mutation().occurrence])], replacing: started,
            configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state.minimumCatchUpRevisions.isEmpty && state.needsRemoteScheduleCatchUp)
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.acknowledgeRemoteScheduleCatchUp(replacing: started, configurationIdentifier: Self.configuration)
        }
        let scheduleStarted = state
        try state.acknowledgeRemoteScheduleCatchUp(replacing: scheduleStarted, configurationIdentifier: Self.configuration)
        #expect(!state.hasUnresolvedCustody)
    }

    @Test("terminal cursor chains reject repeats, cycles, wrong mode and unchanged nonempty replies")
    func terminalCursorChains() throws {
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration, terminalDeltaCursor: "start")
        let prior = state
        let first = page([F.snapshot()], cursor: "middle", hasMore: true)
        let next = F.snapshot(revision: 2, mode: .keepOpen)
        let invalidChains = [
            [page([F.snapshot()], cursor: "start")],
            [page([F.snapshot()], cursor: "start", hasMore: true), page([next], cursor: "end", firstSequence: 2)],
            [first, page([next], cursor: "middle", firstSequence: 2)],
            [first, page([next], cursor: "start", firstSequence: 2)],
        ]
        for pages in invalidChains {
            #expect(throws: RoutineOccurrenceStateError.self) {
                try state.installTerminalChanges(pages, replacing: prior, configurationIdentifier: Self.configuration, at: Self.date)
            }
            #expect(state == prior)
        }
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges([page([])], replacing: prior, configurationIdentifier: Self.configuration,
                at: Self.date, isCurrentState: true)
        }
        try state.installTerminalChanges([page([], cursor: "start")], replacing: prior,
            configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state == prior)
        try state.installTerminalChanges([first, page([next], cursor: "end", firstSequence: 2)], replacing: prior,
            configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state.terminalDeltaCursor == "end" && state.observations.first?.snapshot == next)

        var cold = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        let empty = cold
        #expect(throws: RoutineOccurrenceStateError.self) {
            try cold.installTerminalChanges([page([])], replacing: empty, configurationIdentifier: Self.configuration,
                at: Self.date, isCurrentState: false)
        }
        #expect(throws: RoutineOccurrenceStateError.self) {
            try cold.installTerminalChanges([first, page([next], cursor: "end", firstSequence: 2)], replacing: empty,
                configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(cold == empty)
    }

    @Test("changed terminal checkpoints latch remote catch-up even with unchanged members or empty changes")
    func changedTerminalCheckpointCatchUp() throws {
        var clean = RoutineOccurrenceState(configurationIdentifier: Self.configuration, terminalDeltaCursor: "start")
        try clean.observe(F.snapshot(), configurationIdentifier: Self.configuration, at: Self.date)
        #expect(!clean.needsRemoteScheduleCatchUp)
        for snapshots in [[], [F.snapshot()]] {
            var changed = clean
            try changed.installTerminalChanges([page(snapshots, cursor: "changed")], replacing: clean,
                configurationIdentifier: Self.configuration, at: Self.date)
            #expect(changed.needsRemoteScheduleCatchUp && changed.terminalDeltaCursor == "changed")
            #expect(changed.observations == clean.observations)
        }
        var unchanged = clean
        try unchanged.installTerminalChanges([page([], cursor: "start")], replacing: clean,
            configurationIdentifier: Self.configuration, at: Self.date)
        #expect(unchanged == clean && !unchanged.needsRemoteScheduleCatchUp)
        var waiting = clean; waiting.needsRemoteScheduleCatchUp = true
        for cursor in ["start", "changed"] {
            var retained = waiting
            try retained.installTerminalChanges([page([], cursor: cursor)], replacing: waiting,
                configurationIdentifier: Self.configuration, at: Self.date)
            #expect(retained.needsRemoteScheduleCatchUp)
        }
    }

    @Test("every receipt target needs this chain's read revision before cursor or cache installation")
    func completeReceiptCoverage() throws {
        var state = try pending()
        try state.settleReceipt(F.mutation(), for: #require(state.journals.first), at: Self.date)
        try state.observe(F.snapshot(revision: 3, mode: .keepOpen), configurationIdentifier: Self.configuration, at: Self.date)
        let second = identified(F.mutation().occurrence, 2)
        state.minimumCatchUpRevisions[second.aggregate.manifest.id] = 2
        state.terminalDeltaCursor = "coverage-start"
        let prior = state
        for snapshots in [[F.mutation().occurrence], [F.snapshot(), second]] {
            #expect(throws: RoutineOccurrenceStateError.self) {
                try state.installTerminalChanges([page(snapshots)], replacing: prior,
                    configurationIdentifier: Self.configuration, at: Self.date)
            }
            #expect(state == prior)
        }
        try state.installTerminalChanges([page([F.mutation().occurrence, second])], replacing: prior,
            configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state.minimumCatchUpRevisions.isEmpty && state.needsRemoteScheduleCatchUp)
        #expect(state.observations.first(where: { $0.instanceID == F.instanceID })?.snapshot.aggregate.revision == 3)
        #expect(state.terminalDeltaCursor == "opaque-terminal")
    }

    @Test("terminal chain page and member visit budgets apply before cache folding")
    func terminalCountBudgets() throws {
        let small = F.snapshot(count: 1)
        func pages(_ count: Int) -> [RoutineOccurrencePage] {
            (1...count).map { index in page([identified(small, index)], cursor: "terminal-count-\(index)",
                hasMore: index < count, firstSequence: UInt64(index)) }
        }
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        let empty = state
        try state.installTerminalChanges(pages(128), replacing: empty, configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state.observations.count == 128 && state.terminalDeltaCursor == "terminal-count-128")
        state = empty
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges(pages(129), replacing: empty, configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == empty)
        let deep = F.snapshot(count: 5_000)
        var tooManyVisits = (1...8).map { index in page([identified(deep, index)], cursor: "member-count-\(index)",
            hasMore: true, firstSequence: UInt64(index)) }
        tooManyVisits.append(page([identified(small, 9)], cursor: "member-count-end", firstSequence: 9))
        #expect(tooManyVisits.flatMap(\.changes).reduce(0) { $0 + $1.occurrence.aggregate.members.count } == 40_001)
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges(tooManyVisits, replacing: empty, configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == empty)
    }

    @Test("terminal chain sums actual page bytes even when every page and member count fits")
    func terminalByteBudget() throws {
        let wide = identified(F.snapshot(count: 4_000), 1, title: String(repeating: "x", count: 500))
        let pageBytes = try F.bytes(page([wide], cursor: "wide-01", hasMore: true)).count
        #expect(pageBytes < RoutineOccurrenceValidation.maximumBytes)
        let overLimitCount = RoutineOccurrenceState.maximumTerminalBytes / pageBytes + 1
        #expect(overLimitCount * 4_000 <= RoutineOccurrenceState.maximumTerminalMemberVisits)
        func pages(_ count: Int) -> [RoutineOccurrencePage] {
            (1...count).map { index in page([identified(wide, index)], cursor: String(format: "wide-%02d", index),
                hasMore: index < count, firstSequence: UInt64(index)) }
        }
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        let empty = state
        let fitting = pages(overLimitCount - 1)
        #expect(try fitting.reduce(0) { try $0 + F.bytes($1).count } <= RoutineOccurrenceState.maximumTerminalBytes)
        try state.installTerminalChanges(fitting, replacing: empty, configurationIdentifier: Self.configuration, at: Self.date)
        #expect(state.terminalDeltaCursor == fitting.last?.cursor)
        state = empty
        let oversized = pages(overLimitCount)
        #expect(try oversized.reduce(0) { try $0 + F.bytes($1).count } > RoutineOccurrenceState.maximumTerminalBytes)
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.installTerminalChanges(oversized, replacing: empty, configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == empty)
    }

    @Test("deep trees and aggregate bytes are bounded, with pinned observations and journals never evicted")
    func cacheByteBudgets() throws {
        let deep = F.snapshot(count: 5_000)
        var deepState = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        try deepState.observe(deep, configurationIdentifier: Self.configuration, at: Self.date)
        #expect(deepState.isValid)
        let large = F.snapshot(count: 2_500)
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration)
        try state.observe(large, configurationIdentifier: Self.configuration, at: Self.date)
        try state.enqueue(journal(snapshot: large))
        let retainedIntent = state.journals
        for index in 1...6 {
            try state.observe(identified(large, index), configurationIdentifier: Self.configuration,
                at: Self.date.addingTimeInterval(Double(index)))
        }
        #expect(state.journals == retainedIntent)
        #expect(state.observations.contains { $0.instanceID == F.instanceID })
        #expect(state.observations.count < 7)
        #expect(try F.bytes(state).count <= RoutineOccurrenceState.maximumSerializedBytes)
        for observation in state.observations {
            state.minimumCatchUpRevisions[observation.instanceID] = observation.snapshot.aggregate.revision
        }
        state.needsRemoteScheduleCatchUp = true
        let pinned = state
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.observe(identified(large, 20), configurationIdentifier: Self.configuration, at: Self.date)
        }
        #expect(state == pinned && state.journals == retainedIntent)
        let crowded = RoutineOccurrenceState(configurationIdentifier: Self.configuration,
            observations: (1...9).map { .init(snapshot: identified(large, $0), observedAt: Self.date) })
        #expect(!crowded.isValid)
        #expect(throws: (any Error).self) { try F.bytes(crowded) }
    }

    @Test("intent count and total original request bytes reject additions without dropping prior intent")
    func journalBudgets() throws {
        let base = F.snapshot()
        var state = RoutineOccurrenceState(configurationIdentifier: Self.configuration,
            observations: (1...65).map { .init(snapshot: identified(base, $0), observedAt: Self.date) },
            journals: try (1...64).map { try journal(snapshot: identified(base, $0), operation: F.id(40_000 + $0)) })
        #expect(state.isValid)
        let full = state
        #expect(throws: RoutineOccurrenceStateError.self) {
            try state.enqueue(journal(snapshot: identified(base, 65), operation: F.id(40_065)))
        }
        #expect(state == full)
        var byteState = RoutineOccurrenceState(configurationIdentifier: Self.configuration,
            observations: (1...2).map { .init(snapshot: identified(base, $0), observedAt: Self.date) })
        try byteState.enqueue(journal(snapshot: identified(base, 1), operation: F.id(41_001), padding: 600_000))
        let byteFull = byteState
        #expect(throws: RoutineOccurrenceStateError.self) {
            try byteState.enqueue(journal(snapshot: identified(base, 2), operation: F.id(41_002), padding: 600_000))
        }
        #expect(byteState == byteFull)
    }

    @Test("encrypted CAS failure preserves receipt journal and catch-up preimage")
    func encryptedCASFailure() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let original = try pending()
        try fixture.planner.commitRoutineOccurrenceState(original, replacing: .empty)
        let competitor = fixture.restart(); competitor.flushPersistence()
        var replacement = original
        try replacement.settleReceipt(F.mutation(), for: #require(original.journals.first), at: Self.date)
        #expect(throws: PlannerPersistenceError.self) {
            try fixture.planner.commitRoutineOccurrenceState(replacement, replacing: original)
        }
        #expect(fixture.planner.routineOccurrenceState == original)
        #expect(try fixture.persistence.load()?.routineOccurrenceState == original)
    }

    @Test("duplicate receipt targets and oversized metadata fail closed before durable state admission")
    func malformedCatchUpTargetsAndMetadata() throws {
        let state = RoutineOccurrenceState(configurationIdentifier: Self.configuration,
            minimumCatchUpRevisions: [F.instanceID: 2], needsRemoteScheduleCatchUp: true)
        var root = try F.object(state)
        root["minimumCatchUpRevisions"] = [F.instanceID.uuidString, 2, F.instanceID.uuidString.lowercased(), 3]
        #expect(throws: (any Error).self) {
            try JSONDecoder().decode(RoutineOccurrenceState.self, from: F.data(root))
        }
        var oversized = state; oversized.configurationIdentifier = String(repeating: "x", count: 1_000_000)
        #expect(!oversized.isValid)
        #expect(throws: (any Error).self) { try F.bytes(oversized) }
        oversized = state; oversized.terminalDeltaCursor = String(repeating: "x", count: 1_000_000)
        #expect(!oversized.isValid)
        #expect(try JSONDecoder().decode(RoutineOccurrenceState.self, from: F.bytes(state)) == state)
    }

    @Test("origin replacement, local reset and quarantine cannot abandon occurrence custody")
    func originAndReset() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        var original = try pending()
        try fixture.planner.commitRoutineOccurrenceState(original, replacing: .empty)
        #expect(fixture.planner.hasExecutionCredentialReplacementBlocker)
        fixture.planner.resetCanonicalSyncState()
        #expect(fixture.planner.routineOccurrenceState == original)
        #expect(throws: (any Error).self) { try fixture.planner.prepareForExecutionCredentialReplacement() }
        #expect(throws: (any Error).self) {
            try fixture.planner.prepareExecutionBinding("different-device-binding",
                canonicalConfigurationIdentifier: "https://different.example.test/|auth=static-v1:" + String(repeating: "b", count: 64))
        }
        #expect(fixture.planner.routineOccurrenceState == original)
        #expect(throws: RoutineOccurrenceStateError.self) {
            try original.observe(F.snapshot(), configurationIdentifier: "https://different.example.test", at: Self.date)
        }
        var root = try fixture.object(), state = try #require(root["routineOccurrenceState"] as? [String: Any])
        state["configurationIdentifier"] = "different-origin"; root["routineOccurrenceState"] = state
        _ = try fixture.write(JSONSerialization.data(withJSONObject: root))
        #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
    }

    @MainActor
    private struct Fixture {
        let directory: URL
        let fileURL: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        private let keyData = Data(repeating: 53, count: 32)
        init(withAuthoring: Bool = false) throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveOccurrencePersistence-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            fileURL = directory.appendingPathComponent("synthetic.encrypted")
            persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: try PlannerEncryptionKey(data: keyData))
            let referenceDate = RoutineOccurrencePersistenceTests.date
            planner = PlannerStore(canonicalConfigurationIdentifier: RoutineOccurrencePersistenceTests.configuration,
                persistence: persistence, restoreFromPersistence: false, now: { referenceDate })
            if withAuthoring {
                let authoring = try planner.enqueueCanonicalCreate(itemID: UUID(),
                    draft: .init(title: "Synthetic retained authoring", timezoneName: "UTC"))
                guard planner.beginCanonicalSync() else { throw RoutineOccurrenceStateError.busy }
                try planner.prepareCanonicalSync(configurationIdentifier: RoutineOccurrencePersistenceTests.configuration)
                _ = try planner.bindCanonicalAuthoringMutation(authoring.id, configurationIdentifier: RoutineOccurrencePersistenceTests.configuration)
                _ = try planner.markCanonicalAuthoringMutationSubmitted(authoring.id)
                planner.endCanonicalSync()
            }
            planner.flushPersistence()
        }
        func restart() -> PlannerStore {
            let referenceDate = RoutineOccurrencePersistenceTests.date
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
            let envelope = try JSONSerialization.data(withJSONObject: ["magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT",
                "formatVersion": 1, "cipher": "AES.GCM.256", "sealedSnapshot": try #require(box.combined).base64EncodedString()])
            try envelope.write(to: fileURL); return envelope
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}
#endif
