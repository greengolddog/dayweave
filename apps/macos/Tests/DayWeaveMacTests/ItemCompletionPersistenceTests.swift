import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Completion encrypted schema and recovery fences", .serialized)
@MainActor
struct ItemCompletionPersistenceTests {
    private typealias F = ItemCompletionTestFixtures

    @Test("every predecessor rejects injected completion authority including explicit null")
    func predecessorInjection() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let original = try fixture.object()
        for schema in 1...26 {
            for injected in [NSNull() as Any, try #require(original["itemCompletionState"])] {
                var root = original; root["schemaVersion"] = schema
                root.removeValue(forKey: "routineOccurrenceState")
                if schema < 26 { root.removeValue(forKey: "itemProgressState") }
                root["itemCompletionState"] = injected
                let encrypted = try fixture.write(JSONSerialization.data(withJSONObject: root))
                #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
                #expect(try Data(contentsOf: fixture.fileURL) == encrypted)
            }
        }
    }

    @Test("schema26 preserves submitted canonical, progress and Google outbound intent exactly")
    func predecessorKeepsAllIntent() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let progressCommand = ItemProgressCommand(expectedItemRevision: 7, expectedProgressRevision: 0,
            components: [.init(name: "Synthetic retained progress", value: .percentage(basisPoints: 4_250))])
        let progressJournal = ItemProgressJournal(itemID: F.itemID, configurationIdentifier: Fixture.configuration,
            command: progressCommand, requestBody: Data(" \n".utf8) + (try progressCommand.bytes()), createdAt: F.date,
            wasSensitive: true, hasBeenSubmitted: true, noEffectCode: nil)
        try fixture.planner.commitItemProgressState(.init(configurationIdentifier: Fixture.configuration,
            observations: [], journals: [progressJournal]), replacing: .empty)
        let authored = try fixture.planner.enqueueCanonicalCreate(itemID: UUID(),
            draft: .init(title: "Synthetic retained canonical create", timezoneName: "UTC"))
        #expect(fixture.planner.beginCanonicalSync())
        try fixture.planner.prepareCanonicalSync(configurationIdentifier: Fixture.configuration)
        _ = try fixture.planner.bindCanonicalAuthoringMutation(authored.id, configurationIdentifier: Fixture.configuration)
        let submitted = try fixture.planner.markCanonicalAuthoringMutationSubmitted(authored.id)
        fixture.planner.endCanonicalSync()
        let outbound = try GoogleOutboundRecoveryJournal(operationGeneration: 1, configurationIdentifier: Fixture.configuration,
            accountID: UUID(), collectionID: UUID(), itemID: F.itemID, expectedItemRevision: 7,
            entityKind: .task, operation: .upsert, intentExpiresAt: F.date.addingTimeInterval(1_800), createdAt: F.date)
        try fixture.planner.saveGoogleOutboundRecoveryJournal(outbound)
        var prior = try fixture.object(); prior["schemaVersion"] = 26; prior.removeValue(forKey: "itemCompletionState")
        prior.removeValue(forKey: "routineOccurrenceState")
        _ = try fixture.write(JSONSerialization.data(withJSONObject: prior))
        let loaded = try #require(try fixture.persistence.load())
        let migrated = try loaded.migratedToCurrentSchema()
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion && migrated.itemCompletionState == .empty)
        #expect(migrated.itemProgressState?.journals == [progressJournal])
        #expect(migrated.pendingCanonicalAuthoringMutations == [submitted])
        #expect(migrated.googleOutboundRecoveryJournal == outbound)
        var result = try fixture.object(migrated); result["schemaVersion"] = 26; result.removeValue(forKey: "itemCompletionState")
        result.removeValue(forKey: "routineOccurrenceState")
        #expect(NSDictionary(dictionary: prior).isEqual(to: result))
    }

    @Test("current encrypted state requires every ledger, observation and journal field")
    func currentRequiredShape() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.installCompletion()
        let original = try fixture.object()
        let ledger = try #require(original["itemCompletionState"] as? [String: Any])
        var variants: [[String: Any]] = []
        var missing = original; missing.removeValue(forKey: "itemCompletionState"); variants.append(missing)
        var null = original; null["itemCompletionState"] = NSNull(); variants.append(null)
        for key in ledger.keys {
            var value = ledger; value.removeValue(forKey: key)
            var root = original; root["itemCompletionState"] = value; variants.append(root)
        }
        for key in ["observations", "journals"] {
            let rows = try #require(ledger[key] as? [[String: Any]])
            let row = try #require(rows.first)
            for field in row.keys {
                var incomplete = row; incomplete.removeValue(forKey: field)
                var value = ledger; value[key] = [incomplete]
                var root = original; root["itemCompletionState"] = value; variants.append(root)
            }
        }
        for root in variants {
            let encrypted = try fixture.write(JSONSerialization.data(withJSONObject: root))
            #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
            #expect(try Data(contentsOf: fixture.fileURL) == encrypted)
        }
    }

    @Test("raw duplicate authority and nested custody keys reject before decoding")
    func rawDuplicateAuthority() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.installCompletion()
        let source = String(decoding: try JSONSerialization.data(withJSONObject: fixture.object(), options: [.sortedKeys]), as: UTF8.self)
        for raw in ["{\"itemCompletionState\":null," + source.dropFirst(),
                    "{\"itemCompletionStat\\u0065\":null," + source.dropFirst(),
                    source.replacingOccurrences(of: "\"wasSensitive\":true", with: "\"wasSensitive\":false,\"wasSensitive\":true"),
                    source.replacingOccurrences(of: "\"needsCanonicalCatchUp\":false", with: "\"needsCanonicalCatchUp\":true,\"needsCanonicalCatchUp\":false")] {
            let encrypted = try fixture.write(Data(raw.utf8))
            #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
            #expect(try Data(contentsOf: fixture.fileURL) == encrypted)
        }
    }

    @Test("exact submitted bytes and historical observation remain encrypted after restart")
    func encryptedRestartAndCatchUpFence() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.installCompletion()
        let original = fixture.planner.itemCompletionState
        let restarted = fixture.restart()
        #expect(restarted.itemCompletionState == original)
        #expect(restarted.itemCompletionReadAdmissions.isEmpty)
        #expect(restarted.hasExecutionCredentialReplacementBlocker)
        let encrypted = try Data(contentsOf: fixture.fileURL)
        #expect(encrypted.range(of: try #require(original.journals.first?.requestBody)) == nil)
        var settled = original; settled.journals = []
        settled.observations[0].isReadProof = false; settled.needsCanonicalCatchUp = true
        try restarted.commitItemCompletionState(settled, replacing: original)
        let afterReceipt = fixture.restart()
        #expect(afterReceipt.itemCompletionState == settled)
        #expect(afterReceipt.hasExecutionCredentialReplacementBlocker)
        let canonical = afterReceipt.canonicalItems
        afterReceipt.resetCanonicalSyncState()
        #expect(afterReceipt.itemCompletionState == settled && afterReceipt.canonicalItems == canonical)
        #expect(afterReceipt.canonicalConfigurationIdentifier == Fixture.configuration)
    }

    @Test("failed exact encrypted save does not settle intent or install catch-up state")
    func failedSaveIsAtomic() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.installCompletion()
        let original = fixture.planner.itemCompletionState
        let competitor = fixture.restart(); competitor.flushPersistence()
        var settled = original; settled.journals = []; settled.needsCanonicalCatchUp = true
        #expect(throws: PlannerPersistenceError.self) {
            try fixture.planner.commitItemCompletionState(settled, replacing: original)
        }
        #expect(fixture.planner.itemCompletionState == original)
        #expect(try fixture.persistence.load()?.itemCompletionState == original)
    }

    @Test("pending completion pins deleted identity beyond retention and survives credential reset")
    func tombstoneAndCredentialCustody() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.installCompletion()
        let retained = fixture.planner.itemCompletionState
        fixture.planner.applyCanonicalDelta([.tombstone(.init(id: F.itemID, revision: 9, deletedAt: F.date, parentID: nil))],
            nextCursor: "synthetic-completion-deleted")
        fixture.planner.flushPersistence()
        let future = PlannerStore(persistence: fixture.persistence, now: { F.date.addingTimeInterval(40 * 24 * 60 * 60) })
        #expect(future.canonicalItem(id: F.itemID) == nil)
        #expect(future.canonicalTrashEntry(id: F.itemID)?.revision == 9)
        #expect(future.itemCompletionState == retained)
        future.resetCanonicalSyncState()
        #expect(future.itemCompletionState == retained && future.hasExecutionCredentialReplacementBlocker)
        future.flushPersistence()
        #expect(try fixture.persistence.load()?.canonicalTrash?.first(where: { $0.id == F.itemID })?.revision == 9)
    }

    @Test("bound completion data cannot cross the enclosing canonical credential")
    func enclosingBinding() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        var root = try fixture.object()
        var ledger = try #require(root["itemCompletionState"] as? [String: Any])
        ledger["configurationIdentifier"] = "different-synthetic-binding"
        root["itemCompletionState"] = ledger
        _ = try fixture.write(JSONSerialization.data(withJSONObject: root))
        #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
    }

    @MainActor
    private struct Fixture {
        static let configuration = (try! DayWeaveAPIBaseURL("https://api.example.test/"))
            .canonicalConfigurationIdentifier + "|auth=static-v1:" + String(repeating: "a", count: 64)
        let directory: URL
        let fileURL: URL
        let persistence: EncryptedPlannerPersistence
        let planner: PlannerStore
        private let keyData = Data(repeating: 47, count: 32)
        init() throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveCompletionPersistence-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            fileURL = directory.appendingPathComponent("synthetic.encrypted")
            persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: try PlannerEncryptionKey(data: keyData))
            let json = #"""
            {"id":"00000000-0000-0000-0000-000000000001","is_sensitive":true,
            "kind":"task","status":"planned","title":"Synthetic completion item","notes":null,
            "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,"earliest_start_at":null,
            "recurrence":null,"flexible_constraints":{},"split_policy":{"type":"indivisible"},
            "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,"is_executable":true,
            "revision":7,"created_at":"2026-09-09T10:00:00Z","updated_at":"2026-09-09T10:00:00Z",
            "completed_at":null,"deleted_at":null}
            """#
            let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
            let item = try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
            planner = PlannerStore(canonicalItems: [item], canonicalDeltaCursor: "synthetic-completion-cursor",
                canonicalConfigurationIdentifier: Self.configuration, persistence: persistence,
                restoreFromPersistence: false, now: { F.date })
            planner.flushPersistence()
        }
        func installCompletion() throws {
            let original = try F.journal()
            let journal = ItemCompletionJournal(itemID: original.itemID,
                configurationIdentifier: Self.configuration, command: original.command,
                requestBody: original.requestBody, createdAt: original.createdAt, wasSensitive: true,
                hasBeenSubmitted: original.hasBeenSubmitted, noEffectCode: original.noEffectCode)
            let value = ItemCompletionState(configurationIdentifier: Self.configuration,
                observations: [.init(snapshot: F.snapshot(itemRevision: 7, revision: 2), observedAt: F.date)], journals: [journal])
            try planner.commitItemCompletionState(value, replacing: planner.itemCompletionState)
        }
        func restart() -> PlannerStore { PlannerStore(persistence: persistence, now: { F.date }) }
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
            try envelope.write(to: fileURL)
            return envelope
        }
        func remove() { try? FileManager.default.removeItem(at: directory) }
    }
}
#endif
