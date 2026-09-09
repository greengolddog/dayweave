import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

enum ItemCompletionTestFixtures {
    static let itemID = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!
    static let operationID = UUID(uuidString: "00000000-0000-0000-0000-000000000100")!
    static let hash = "sha256:" + String(repeating: "1", count: 64)
    static let otherHash = "sha256:" + String(repeating: "2", count: 64)
    static let date = Date(timeIntervalSince1970: 1_788_953_400)
    static func command() -> ItemCompletionCommand {
        .init(operationID: operationID, expectedItemRevision: 7, expectedCompletionRevision: 2,
              expectedEvidenceHash: hash, requiredForParent: true, mode: .keepOpen)
    }
    static func snapshot(itemRevision: UInt64 = 8, revision: UInt64 = 3, evidenceHash: String = hash,
                         counts: ItemCompletionCounts = .init(), mode: ItemCompletionMode = .keepOpen) -> ItemCompletionSnapshot {
        .init(itemID: itemID, itemRevision: itemRevision,
              state: .init(itemID: itemID, revision: revision, mode: mode,
                           updatedAt: "2026-09-09T10:00:00.123456Z"),
              evidenceHash: evidenceHash, counts: counts)
    }
    static func journal() throws -> ItemCompletionJournal {
        .init(itemID: itemID, configurationIdentifier: "synthetic-completion-binding", command: command(),
              requestBody: Data(" \n".utf8) + (try command().bytes()) + Data("\n ".utf8), createdAt: date,
              wasSensitive: false, hasBeenSubmitted: true, noEffectCode: nil)
    }
}

#if canImport(Testing)
@Suite("Completion exact wire and durable custody")
struct ItemCompletionModelTests {
    private typealias F = ItemCompletionTestFixtures

    @Test("all shared closed wire fixtures match producer semantics")
    func sharedWireContract() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let bytes = try Data(contentsOf: root.appendingPathComponent("fixtures/item-completion/wire-v1.json"))
        let fixture = try #require(JSONSerialization.jsonObject(with: bytes) as? [String: Any])
        #expect(fixture["schema_version"] as? Int == 1)
        for (key, expected) in [("valid", true), ("invalid", false)] {
            let cases = try #require(fixture[key] as? [[String: Any]])
            #expect(!cases.isEmpty)
            for entry in cases {
                let kind = try #require(entry["kind"] as? String)
                let name = try #require(entry["name"] as? String)
                let data = try JSONSerialization.data(withJSONObject: #require(entry["value"]), options: [.sortedKeys])
                let accepted: Bool
                switch kind {
                case "state": accepted = (try? ItemCompletionValidation.decode(ItemCompletionPolicy.self, from: data))?.isValid == true
                case "snapshot": accepted = (try? ItemCompletionValidation.decode(ItemCompletionSnapshot.self, from: data))?.isValid == true
                case "command": accepted = (try? ItemCompletionValidation.decode(ItemCompletionCommand.self, from: data))?.isValid(for: F.itemID) == true
                case "receipt": accepted = (try? ItemCompletionValidation.decode(ItemCompletionReceipt.self, from: data))?.isValid == true
                default: Issue.record("Unknown fixture kind: \(kind)"); continue
                }
                #expect(accepted == expected, "Fixture \(name)")
            }
        }
    }

    @Test("nullable fields are emitted and exact reviewed bytes survive ledger restart")
    func exactJournalRoundTrip() throws {
        let journal = try F.journal()
        var ledger = ItemCompletionState(configurationIdentifier: journal.configurationIdentifier,
            observations: [.init(snapshot: F.snapshot(), observedAt: F.date, isReadProof: false)], journals: [journal])
        ledger.needsCanonicalCatchUp = true
        let encoded = try JSONEncoder().encode(ledger)
        let decoded = try JSONDecoder().decode(ItemCompletionState.self, from: encoded)
        #expect(decoded == ledger && decoded.journals[0].requestBody == journal.requestBody)
        #expect(decoded.needsCanonicalCatchUp && !decoded.observations[0].isReadProof)
        let body = try #require(JSONSerialization.jsonObject(with: journal.requestBody) as? [String: Any])
        #expect(body["reopening"] is NSNull)
        let zero = try JSONEncoder().encode(ItemCompletionPolicy.empty(itemID: F.itemID))
        let policy = try #require(JSONSerialization.jsonObject(with: zero) as? [String: Any])
        #expect(policy["provenance"] is NSNull && policy["updated_at"] is NSNull)
        var object = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        object.removeValue(forKey: "needsCanonicalCatchUp")
        #expect(throws: (any Error).self) {
            try JSONDecoder().decode(ItemCompletionState.self, from: JSONSerialization.data(withJSONObject: object))
        }
    }

    @Test("a receipt advances both revisions exactly and binds requested policy and operation")
    func receiptBinding() {
        let receipt = ItemCompletionReceipt(operationID: F.operationID, replayed: true, completion: F.snapshot())
        #expect(receipt.matches(itemID: F.itemID, command: F.command()))
        for snapshot in [F.snapshot(itemRevision: 7), F.snapshot(itemRevision: 9), F.snapshot(revision: 2),
                         F.snapshot(revision: 4), F.snapshot(mode: .automatic)] {
            #expect(!ItemCompletionReceipt(operationID: F.operationID, replayed: true, completion: snapshot)
                .matches(itemID: F.itemID, command: F.command()))
        }
        #expect(!receipt.matches(itemID: UUID(), command: F.command()))
        #expect(!ItemCompletionReceipt(operationID: UUID(), replayed: true, completion: F.snapshot())
            .matches(itemID: F.itemID, command: F.command()))
        let overflowing = ItemCompletionCommand(expectedItemRevision: UInt64(Int64.max),
            expectedCompletionRevision: UInt64(Int64.max), expectedEvidenceHash: F.hash, requiredForParent: true, mode: .automatic)
        #expect(overflowing.isValid && !receipt.matches(itemID: F.itemID, command: overflowing))
        let defaultReceipt = ItemCompletionReceipt(operationID: F.operationID, replayed: false,
            completion: .init(itemID: F.itemID, itemRevision: 1, state: .empty(itemID: F.itemID), evidenceHash: F.hash))
        #expect(!defaultReceipt.isValid)
    }

    @Test("historical receipts do not replace newer or same-revision GET evidence")
    func historicalObservation() throws {
        let fresh = F.snapshot(itemRevision: 10, revision: 4, evidenceHash: F.otherHash,
            counts: .init(requiredDescendants: 1, incomplete: 1))
        var state = ItemCompletionState(configurationIdentifier: "synthetic-completion-binding", observations: [], journals: [])
        try state.observe(fresh, at: F.date)
        try state.observe(F.snapshot(), at: F.date.addingTimeInterval(1), isReadProof: false)
        #expect(state.observations[0].snapshot == fresh && state.observations[0].isReadProof)
        let sameRevisionReceipt = F.snapshot(itemRevision: 10, revision: 4)
        try state.observe(sameRevisionReceipt, at: F.date.addingTimeInterval(2), isReadProof: false)
        #expect(state.observations[0].snapshot == fresh)
        var empty = ItemCompletionState(configurationIdentifier: "synthetic-completion-binding", observations: [], journals: [])
        try empty.observe(F.snapshot(), at: F.date, isReadProof: false)
        #expect(!empty.observations[0].isReadProof)
    }

    @Test("new GET evidence may change counts without rewriting same-revision policy")
    func refreshedWholeForestEvidence() throws {
        var state = ItemCompletionState(configurationIdentifier: "synthetic-completion-binding", observations: [], journals: [])
        try state.observe(F.snapshot(), at: F.date)
        let fresh = F.snapshot(evidenceHash: F.otherHash, counts: .init(requiredDescendants: 2, completed: 1, incomplete: 1))
        try state.observe(fresh, at: F.date.addingTimeInterval(1))
        #expect(state.observations[0].snapshot == fresh)
        let before = state
        #expect(throws: ItemCompletionError.invalidData) {
            try state.observe(F.snapshot(mode: .automatic), at: F.date.addingTimeInterval(2))
        }
        #expect(state == before)
        #expect(throws: ItemCompletionError.staleReview) {
            try state.observe(F.snapshot(itemRevision: 7, revision: 2), at: F.date.addingTimeInterval(3))
        }
        #expect(state == before)
    }

    @Test("journal custody permits sticky privacy strengthening only")
    func custodyAndBinding() throws {
        let original = try F.journal()
        var stronger = original; stronger.wasSensitive = true
        #expect(stronger.retainsCustody(of: original) && !original.retainsCustody(of: stronger))
        var rejected = stronger; rejected.noEffectCode = "item_completion_evidence_stale"
        #expect(rejected.isValid && !rejected.retainsCustody(of: stronger))
        var invalid = original; invalid.noEffectCode = "service_unavailable"
        #expect(!invalid.isValid)
        var unsent = rejected; unsent.hasBeenSubmitted = false
        #expect(unsent.isValid, "A local stale preflight can prove an unsent operation had no effect")
        #expect(!ItemCompletionState(configurationIdentifier: "another-binding", observations: [], journals: [original]).isValid)
        #expect(!ItemCompletionState(configurationIdentifier: nil, observations: [], journals: [], needsCanonicalCatchUp: true).isValid)
    }

    @Test("raw duplicate and noncanonical integer commands never become exact custody")
    func strictRawJournal() throws {
        let command = F.command()
        let original = String(decoding: try command.bytes(), as: UTF8.self)
        for raw in [original.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1,\"schema_version\":1"),
                    original.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1,\"schema_versi\\u006fn\":1"),
                    original.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1.0"),
                    original.replacingOccurrences(of: "\"schema_version\":1", with: "\"schema_version\":1e0")] {
            let journal = ItemCompletionJournal(itemID: F.itemID, configurationIdentifier: "synthetic", command: command,
                requestBody: Data(raw.utf8), createdAt: F.date, wasSensitive: false, hasBeenSubmitted: true, noEffectCode: nil)
            #expect(!journal.isValid)
        }
    }

    @Test("timestamp precision and count bounds are exact rather than rounded")
    func exactBounds() {
        for timestamp in ["2026-09-09T10:00:00Z", "2026-09-09T10:00:00.123456Z", "2026-09-09T10:00:00.123456+00:00"] {
            #expect(ItemCompletionValidation.timestamp(timestamp))
        }
        for timestamp in ["2026-09-09T10:00:00.123456000Z", "2026-09-09T10:00:00.123456789Z",
                          "2026-09-09T10:00:00-00:00", "2026-02-30T10:00:00Z", "2026-09-09T10:00:00Z\n"] {
            #expect(!ItemCompletionValidation.timestamp(timestamp))
        }
        #expect(ItemCompletionCounts(requiredDescendants: 20_000, completed: 20_000).isValid)
        #expect(!ItemCompletionCounts(requiredDescendants: 20_001, completed: 20_001).isValid)
        #expect(!ItemCompletionCounts(requiredDescendants: UInt64.max, completed: UInt64.max, incomplete: 1).isValid)
        #expect(!ItemCompletionCounts(requiredDescendants: 2, completed: 1).isValid)
    }
}
#endif
