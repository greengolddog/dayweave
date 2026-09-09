import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Independent progress encrypted schema", .serialized)
@MainActor
struct ItemProgressPersistenceTests {
    @Test("every predecessor rejects injected progress including explicit null before migration")
    func predecessorInjection() throws {
        let fixture = try Fixture()
        defer { fixture.remove() }
        for schema in 1...25 {
            for injected in [NSNull() as Any, try #require(fixture.root["itemProgressState"])] {
                var root = fixture.root
                root["schemaVersion"] = schema
                root.removeValue(forKey: "itemCompletionState")
                root["itemProgressState"] = injected
                let encrypted = try fixture.write(JSONSerialization.data(withJSONObject: root))
                #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
                #expect(try Data(contentsOf: fixture.fileURL) == encrypted)
            }
        }
    }

    @Test("schema25 migrates to empty progress without changing old snapshot fields")
    func predecessorMigration() throws {
        let fixture = try Fixture()
        defer { fixture.remove() }
        var root = fixture.root
        root["schemaVersion"] = 25
        root.removeValue(forKey: "itemProgressState")
        root.removeValue(forKey: "itemCompletionState")
        _ = try fixture.write(JSONSerialization.data(withJSONObject: root))
        let loaded = try fixture.persistence.load()
        let prior = try #require(loaded)
        let migrated = try prior.migratedToCurrentSchema()
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion)
        #expect(migrated.itemProgressState == .empty)
        var encoded = try fixture.object(migrated)
        encoded["schemaVersion"] = 25
        encoded.removeValue(forKey: "itemProgressState")
        encoded.removeValue(forKey: "itemCompletionState")
        #expect(NSDictionary(dictionary: root).isEqual(to: encoded))
    }

    @Test("current snapshot requires every explicit progress-state field")
    func currentFieldsRequired() throws {
        let fixture = try Fixture()
        defer { fixture.remove() }
        let state = try #require(fixture.root["itemProgressState"] as? [String: Any])
        var variants: [[String: Any]] = []
        var missing = fixture.root; missing.removeValue(forKey: "itemProgressState"); variants.append(missing)
        var null = fixture.root; null["itemProgressState"] = NSNull(); variants.append(null)
        for key in state.keys {
            var incomplete = state; incomplete.removeValue(forKey: key)
            var root = fixture.root; root["itemProgressState"] = incomplete; variants.append(root)
        }
        for root in variants {
            _ = try fixture.write(JSONSerialization.data(withJSONObject: root))
            #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
        }
    }

    @Test("duplicate and escaped-equivalent authority keys reject in original encrypted bytes")
    func rawDuplicateKeys() throws {
        let fixture = try Fixture()
        defer { fixture.remove() }
        let source = try #require(String(data: JSONSerialization.data(withJSONObject: fixture.root), encoding: .utf8))
        for key in ["itemProgressState", "itemProgressStat\\u0065"] {
            let raw = "{\"\(key)\":null," + source.dropFirst()
            _ = try fixture.write(Data(raw.utf8))
            #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
        }
    }

    @Test("even empty bound progress cannot cross the enclosing credential binding")
    func bindingMismatch() throws {
        let fixture = try Fixture()
        defer { fixture.remove() }
        var state = try #require(fixture.root["itemProgressState"] as? [String: Any])
        state["configurationIdentifier"] = "different-synthetic-binding"
        var root = fixture.root; root["itemProgressState"] = state
        _ = try fixture.write(JSONSerialization.data(withJSONObject: root))
        #expect(throws: PlannerPersistenceError.self) { try fixture.persistence.load() }
    }

    @MainActor
    private struct Fixture {
        let directory: URL
        let fileURL: URL
        let persistence: EncryptedPlannerPersistence
        let root: [String: Any]
        private let keyData = Data(repeating: 44, count: 32)
        init() throws {
            directory = FileManager.default.temporaryDirectory.appendingPathComponent("DayWeaveProgressMigration-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            fileURL = directory.appendingPathComponent("synthetic.encrypted")
            persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: try PlannerEncryptionKey(data: keyData))
            let planner = PlannerStore(persistence: persistence, restoreFromPersistence: false)
            planner.flushPersistence()
            let loaded = try persistence.load()
            let snapshot = try #require(loaded)
            let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
            root = try #require(JSONSerialization.jsonObject(with: encoder.encode(snapshot)) as? [String: Any])
        }
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
