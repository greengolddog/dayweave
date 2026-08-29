import CryptoKit
import Foundation
#if canImport(XCTest)
import XCTest
#elseif canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

private struct PersistenceScenarioFailure: Error, CustomStringConvertible {
    let description: String
}

@MainActor
private enum EncryptedPlannerPersistenceScenarios {
    static let canary = "TOP-SECRET-DAYWEAVE-CANARY-7E4D"

    static func roundTrip() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let snapshot = makeSnapshot()

        try context.persistence.save(snapshot)
        let restored = try requireValue(context.persistence.load(), "Saved snapshot was not restored")

        try require(restored == snapshot, "Restored snapshot differs from the saved snapshot")
    }

    static func encryptedAtRest() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }

        try context.persistence.save(makeSnapshot())

        let bytes = try Data(contentsOf: context.fileURL)
        try require(
            bytes.range(of: Data(canary.utf8)) == nil,
            "Encrypted file contains the schedule title canary"
        )
        try require(
            bytes.range(of: Data("private notes and attendee details".utf8)) == nil,
            "Encrypted file contains plaintext notes"
        )
    }

    static func corruptionFailure() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        try context.persistence.save(makeSnapshot())

        let original = try Data(contentsOf: context.fileURL)
        var object = try requireValue(
            JSONSerialization.jsonObject(with: original) as? [String: Any],
            "Envelope is not a JSON object"
        )
        let encodedCiphertext = try requireValue(
            object["sealedSnapshot"] as? String,
            "Envelope has no ciphertext"
        )
        var ciphertext = try requireValue(
            Data(base64Encoded: encodedCiphertext),
            "Envelope ciphertext is not base64"
        )
        let corruptionIndex = ciphertext.index(
            ciphertext.startIndex,
            offsetBy: ciphertext.count / 2
        )
        ciphertext[corruptionIndex] ^= 0x01
        object["sealedSnapshot"] = ciphertext.base64EncodedString()
        let corrupted = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        try corrupted.write(to: context.fileURL, options: .atomic)

        var observedError: PlannerPersistenceError?
        do {
            _ = try context.persistence.load()
        } catch {
            observedError = error
        }
        try require(
            observedError == .authenticationFailed,
            "Corrupted ciphertext did not produce an authentication failure: \(String(describing: observedError))"
        )
    }

    static func oversizedEnvelopeIsRejected() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        try FileManager.default.createDirectory(
            at: context.directory,
            withIntermediateDirectories: true
        )
        _ = FileManager.default.createFile(atPath: context.fileURL.path, contents: Data())
        let handle = try FileHandle(forWritingTo: context.fileURL)
        try handle.truncate(
            atOffset: UInt64(EncryptedPlannerPersistence.maximumEnvelopeBytes + 1)
        )
        try handle.close()

        var observedError: PlannerPersistenceError?
        do {
            _ = try context.persistence.load()
        } catch {
            observedError = error
        }
        try require(
            observedError == .snapshotTooLarge(
                limitBytes: EncryptedPlannerPersistence.maximumEnvelopeBytes
            ),
            "Oversized encrypted file did not fail at the envelope resource gate"
        )
    }

    static func oversizedPlaintextIsRejectedOnSave() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let oversizedMessage = AssistantMessage(
            id: UUID(),
            role: .user,
            text: String(
                repeating: "x",
                count: EncryptedPlannerPersistence.maximumPlaintextBytes + 1
            ),
            createdAt: base.savedAt
        )
        let oversized = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: [oversizedMessage],
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted
        )
        var observedError: PlannerPersistenceError?
        do {
            try context.persistence.save(oversized)
        } catch {
            observedError = error
        }
        try require(
            observedError == .snapshotTooLarge(
                limitBytes: EncryptedPlannerPersistence.maximumPlaintextBytes
            ),
            "Oversized plaintext did not fail at the save resource gate"
        )
    }

    static func storeRestore() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let snapshot = makeSnapshot()
        let source = PlannerStore(
            blocks: snapshot.blocks,
            suggestions: snapshot.suggestions,
            assistantMessages: snapshot.assistantMessages,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60)
        )
        source.destination = .goals
        source.protectedFreeMinutes = 135
        source.freezeHours = 5
        source.showCompleted = false
        source.flushPersistence()
        try require(source.persistenceError == nil, "Source store failed to persist")

        let restored = PlannerStore(
            blocks: [],
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )

        try require(restored.blocks == snapshot.blocks, "Store did not restore schedule blocks")
        try require(restored.suggestions == snapshot.suggestions, "Store did not restore suggestions")
        try require(
            restored.assistantMessages == snapshot.assistantMessages,
            "Store did not restore assistant messages"
        )
        try require(restored.destination == .goals, "Store did not restore its destination")
        try require(
            restored.selectedBlockID == snapshot.blocks.first?.id,
            "Store did not restore its selection"
        )
        try require(restored.protectedFreeMinutes == 135, "Store did not restore protected time")
        try require(restored.freezeHours == 5, "Store did not restore the freeze horizon")
        try require(!restored.showCompleted, "Store did not restore completed-item visibility")
        try require(restored.persistenceError == nil, "Restored store reported a persistence error")
    }

    static func canonicalCacheRoundTrip() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let item = try canonicalItem()
        let occurrenceID = UUID(uuidString: "40000000-0000-4000-8000-000000000004")!
        let mutation = PendingCanonicalMutation(
            id: UUID(uuidString: "60000000-0000-4000-8000-000000000006")!,
            itemID: item.id,
            occurrenceID: nil,
            sessionIndex: 0,
            desiredStatus: .paused,
            baseRevision: item.revision,
            createdAt: base.savedAt,
            disposition: .conflicted,
            diagnostic: "revision changed"
        )
        let outcome = RecurrenceSessionOutcome(
            itemID: item.id,
            occurrenceID: occurrenceID,
            sessionIndex: 0,
            disposition: .completed,
            occurredAt: base.savedAt,
            occurrenceFullyScheduled: true
        )
        let configurationIdentifier = "https://api.example.com/gateway"
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configurationIdentifier,
            generatedAt: base.savedAt,
            asOf: base.savedAt,
            horizonStart: base.savedAt,
            horizonEnd: base.savedAt.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let snapshot = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted,
            canonicalItems: [item],
            canonicalDeltaCursor: "opaque-encrypted-cursor",
            canonicalTombstoneRevisions: [UUID(uuidString: "70000000-0000-4000-8000-000000000007")!: 12],
            completedOccurrenceIDs: [occurrenceID],
            pendingCanonicalMutations: [mutation],
            recurrenceSessionOutcomes: [outcome],
            canonicalConfigurationIdentifier: configurationIdentifier,
            schedulePreviewProvenance: provenance,
            localCaptureDiagnostics: [base.blocks[0].id: "Persistent recovery warning"]
        )

        try context.persistence.save(snapshot)
        let restored = try requireValue(context.persistence.load(), "Canonical snapshot was not restored")
        try require(restored.canonicalItems == [item], "Canonical item fields were not restored exactly")
        try require(restored.canonicalDeltaCursor == "opaque-encrypted-cursor", "Delta cursor was not restored")
        try require(restored.completedOccurrenceIDs == snapshot.completedOccurrenceIDs, "Occurrence state was not restored")
        try require(restored.canonicalTombstoneRevisions == snapshot.canonicalTombstoneRevisions, "Tombstone revisions were not restored")
        try require(restored.pendingCanonicalMutations == [mutation], "Pending mutation was not restored")
        try require(restored.recurrenceSessionOutcomes == [outcome], "Recurrence outcome was not restored")
        try require(
            restored.canonicalConfigurationIdentifier == configurationIdentifier,
            "Canonical API binding was not restored"
        )
        try require(restored.schedulePreviewProvenance == provenance, "Preview provenance was not restored")
        try require(
            restored.localCaptureDiagnostics == snapshot.localCaptureDiagnostics,
            "Local capture recovery diagnostics were not restored"
        )
        let bytes = try Data(contentsOf: context.fileURL)
        try require(bytes.range(of: Data("CANONICAL-PRIVATE-NOTES".utf8)) == nil, "Canonical notes leaked in plaintext")
    }

    static func nonRoundTrippableNumberRemainsReadOnlyAfterRestart() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let item = try decimalCanonicalItem()
        try require(!item.supportsLosslessReplacement, "Server decimal unexpectedly started writable")
        let snapshot = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted,
            canonicalItems: [item]
        )

        try context.persistence.save(snapshot)
        let restored = try requireValue(context.persistence.load(), "Decimal snapshot was not restored")
        let restoredItem = try requireValue(restored.canonicalItems?.first, "Decimal item was lost")

        try require(
            restoredItem.hasNonRoundTrippableJSONNumber,
            "Snapshot normalization erased the durable numeric safety marker"
        )
        try require(
            !restoredItem.supportsLosslessReplacement,
            "Snapshot normalization incorrectly upgraded a decimal item to writable"
        )
    }

    static func concurrentStoreWriterFailsClosed() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        try context.persistence.save(makeSnapshot())
        let first = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        let second = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )

        try require(
            first.quickAdd(title: "First process edit", kind: .task, minutes: 20),
            "First process could not create its local edit"
        )
        first.flushPersistence()
        try require(first.persistenceError == nil, "First process failed to persist")
        let firstBytes = try Data(contentsOf: context.fileURL)

        try require(
            second.quickAdd(title: "Stale second edit", kind: .task, minutes: 20),
            "Second process could not stage its local edit"
        )
        second.flushPersistence()

        try require(
            second.persistenceError == .concurrentModification,
            "Stale process did not fail at the compare-and-swap gate"
        )
        try require(second.loadState == .persistenceFailed, "Stale writer was not mutation-gated")
        let afterStaleFlush = try Data(contentsOf: context.fileURL)
        try require(
            afterStaleFlush == firstBytes,
            "Stale process overwrote the newer encrypted snapshot"
        )
        let restored = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60)
        )
        try require(
            restored.blocks.contains { $0.title == "First process edit" },
            "The winning process edit was not retained"
        )
        try require(
            !restored.blocks.contains { $0.title == "Stale second edit" },
            "The stale process edit reached disk"
        )
    }

    static func schemaOneMigratesWithoutInventingNewState() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 1,
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted,
            completedOccurrenceIDs: [UUID(uuidString: "80000000-0000-4000-8000-000000000008")!]
        )

        let migrated = try legacy.migratedToCurrentSchema()

        try require(migrated.schemaVersion == 3, "Legacy snapshot did not migrate to schema 3")
        try require(migrated.pendingCanonicalMutations == [], "Migration invented pending mutations")
        try require(migrated.recurrenceSessionOutcomes == [], "Migration invented recurrence outcomes")
        try require(migrated.canonicalTombstoneRevisions == [:], "Migration invented tombstones")
        try require(migrated.completedOccurrenceIDs == [], "Migration reused unsafe schema-1 occurrence IDs")
        try require(
            migrated.lastScheduleMessage.contains("revalidated"),
            "Migration did not explain recurrence revalidation"
        )
    }

    static func encryptedSchemaOneIsAtomicallyRewritten() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 1,
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        let currentShapePlaintext = try encoder.encode(legacy)
        var legacyObject = try requireValue(
            JSONSerialization.jsonObject(with: currentShapePlaintext) as? [String: Any],
            "Legacy plaintext was not an object"
        )
        var legacyBlocks = try requireValue(
            legacyObject["blocks"] as? [[String: Any]],
            "Legacy plaintext had no blocks"
        )
        for index in legacyBlocks.indices {
            legacyBlocks[index].removeValue(forKey: "previewKind")
            legacyBlocks[index].removeValue(forKey: "occurrenceFullyScheduled")
        }
        legacyObject["blocks"] = legacyBlocks
        let plaintext = try JSONSerialization.data(withJSONObject: legacyObject)
        let sealed = try AES.GCM.seal(
            plaintext,
            using: SymmetricKey(data: context.keyData),
            authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8)
        )
        let combined = try requireValue(sealed.combined, "Legacy ciphertext was unavailable")
        let envelope: [String: Any] = [
            "magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT",
            "formatVersion": 1,
            "cipher": "AES.GCM.256",
            "sealedSnapshot": combined.base64EncodedString(),
        ]
        try JSONSerialization.data(withJSONObject: envelope).write(to: context.fileURL)

        let migrated = try requireValue(context.persistence.load(), "Legacy snapshot did not load")
        try require(migrated.schemaVersion == 3, "Encrypted legacy snapshot did not migrate")
        try require(
            migrated.blocks.first?.occurrenceFullyScheduled == true,
            "A genuine pre-field block did not receive its safe legacy default"
        )
        let secondLoad = try requireValue(context.persistence.load(), "Migrated snapshot was not rewritten")
        try require(secondLoad.schemaVersion == 3, "Migrated file was not durable")
    }

    private static func require(
        _ condition: @autoclosure () -> Bool,
        _ message: String
    ) throws {
        guard condition() else {
            throw PersistenceScenarioFailure(description: message)
        }
    }

    private static func requireValue<T>(_ value: T?, _ message: String) throws -> T {
        guard let value else {
            throw PersistenceScenarioFailure(description: message)
        }
        return value
    }

    private static func makeContext() throws -> (
        directory: URL,
        fileURL: URL,
        persistence: EncryptedPlannerPersistence,
        keyData: Data
    ) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeavePersistenceTests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let keyData = Data((0..<32).map(UInt8.init))
        let key = try PlannerEncryptionKey(data: keyData)
        return (
            directory,
            fileURL,
            EncryptedPlannerPersistence(fileURL: fileURL, key: key),
            keyData
        )
    }

    private static func makeSnapshot() -> PlannerSnapshot {
        let start = Date(timeIntervalSince1970: 1_700_000_000)
        let block = ScheduleBlock(
            id: UUID(uuidString: "10000000-0000-0000-0000-000000000001")!,
            title: canary,
            kind: .event,
            start: start,
            end: start.addingTimeInterval(45 * 60),
            status: .scheduled,
            project: "Confidential project",
            notes: "private notes and attendee details",
            energy: .deep,
            isFlexible: false,
            isHardConstraint: true,
            actualMinutes: nil
        )
        let suggestion = PlanningSuggestion(
            id: UUID(uuidString: "20000000-0000-0000-0000-000000000002")!,
            title: "Protect the afternoon",
            summary: "Move the confidential meeting",
            source: "DayWeave assistant",
            createdAt: start,
            expiresAt: start.addingTimeInterval(86_400),
            state: .pending
        )
        let message = AssistantMessage(
            id: UUID(uuidString: "30000000-0000-0000-0000-000000000003")!,
            role: .assistant,
            text: "The private schedule still fits.",
            createdAt: start
        )
        return PlannerSnapshot(
            savedAt: start,
            destination: .today,
            selectedBlockID: block.id,
            blocks: [block],
            suggestions: [suggestion],
            assistantMessages: [message],
            lastScheduleMessage: "Schedule is private and balanced",
            protectedFreeMinutes: 120,
            freezeHours: 3,
            showCompleted: true
        )
    }

    private static func canonicalItem() throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {
          "id":"50000000-0000-4000-8000-000000000005","kind":"task","status":"planned",
          "title":"Encrypted canonical item","notes":"CANONICAL-PRIVATE-NOTES","timezone_name":"Europe/Madrid",
          "duration_seconds":3600,"deadline_at":"2026-09-01T17:00:00Z","earliest_start_at":null,
          "recurrence":{"type":"weekly","times_per_week":2},"flexible_constraints":{"energy":"deep"},
          "split_policy":{"type":"splittable","minimum_chunk_seconds":900,"maximum_chunk_seconds":2700},
          "importance":80,"urgency":70,"parent_id":null,"sibling_order":0,"is_executable":true,
          "revision":3,"created_at":"2026-08-29T09:00:00Z","updated_at":"2026-08-29T09:00:00Z",
          "completed_at":null,"deleted_at":null,"future_rule":{"mode":"read_only"}
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func decimalCanonicalItem() throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {
          "id":"51000000-0000-4000-8000-000000000005","kind":"task","status":"planned",
          "title":"Read-only decimal item","notes":null,"timezone_name":"Europe/Madrid",
          "duration_seconds":3600,"deadline_at":null,"earliest_start_at":null,
          "recurrence":null,"flexible_constraints":{"future_ratio":1.0},
          "split_policy":{"type":"indivisible"},"importance":80,"urgency":70,
          "parent_id":null,"sibling_order":0,"is_executable":true,"revision":3,
          "created_at":"2026-08-29T09:00:00Z","updated_at":"2026-08-29T09:00:00Z",
          "completed_at":null,"deleted_at":null
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }
}

#if canImport(XCTest)
@MainActor
final class EncryptedPlannerPersistenceTests: XCTestCase {
    func testEncryptedSnapshotRoundTrips() throws {
        try EncryptedPlannerPersistenceScenarios.roundTrip()
    }

    func testSnapshotDoesNotContainPlaintextScheduleContent() throws {
        try EncryptedPlannerPersistenceScenarios.encryptedAtRest()
    }

    func testCiphertextCorruptionFailsAuthentication() throws {
        try EncryptedPlannerPersistenceScenarios.corruptionFailure()
    }

    func testOversizedEnvelopeIsRejected() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedEnvelopeIsRejected()
    }

    func testOversizedPlaintextIsRejectedOnSave() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedPlaintextIsRejectedOnSave()
    }

    func testPlannerStoreFlushesAndRestoresEncryptedState() throws {
        try EncryptedPlannerPersistenceScenarios.storeRestore()
    }

    func testCanonicalSyncCacheIsEncryptedAndRoundTrips() throws {
        try EncryptedPlannerPersistenceScenarios.canonicalCacheRoundTrip()
    }

    func testNonRoundTrippableNumberRemainsReadOnlyAfterRestart() throws {
        try EncryptedPlannerPersistenceScenarios.nonRoundTrippableNumberRemainsReadOnlyAfterRestart()
    }

    func testConcurrentStoreWriterFailsClosed() throws {
        try EncryptedPlannerPersistenceScenarios.concurrentStoreWriterFailsClosed()
    }

    func testSchemaOneMigratesToSchemaTwo() throws {
        try EncryptedPlannerPersistenceScenarios.schemaOneMigratesWithoutInventingNewState()
    }

    func testEncryptedSchemaOneIsAtomicallyRewritten() throws {
        try EncryptedPlannerPersistenceScenarios.encryptedSchemaOneIsAtomicallyRewritten()
    }
}
#elseif canImport(Testing)
@Suite("Encrypted planner persistence")
@MainActor
struct EncryptedPlannerPersistenceTests {
    @Test("Encrypted snapshot round-trips")
    func roundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.roundTrip()
    }

    @Test("Schedule content is encrypted at rest")
    func encryptedAtRest() throws {
        try EncryptedPlannerPersistenceScenarios.encryptedAtRest()
    }

    @Test("Ciphertext corruption fails authentication")
    func corruptionFailure() throws {
        try EncryptedPlannerPersistenceScenarios.corruptionFailure()
    }

    @Test("Oversized encrypted files are rejected before decode")
    func oversizedEnvelope() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedEnvelopeIsRejected()
    }

    @Test("Oversized planner plaintext is rejected before encryption")
    func oversizedPlaintext() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedPlaintextIsRejectedOnSave()
    }

    @Test("Planner store flushes and restores encrypted state")
    func storeRestore() throws {
        try EncryptedPlannerPersistenceScenarios.storeRestore()
    }

    @Test("Canonical sync cache is encrypted and round-trips")
    func canonicalCacheRoundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.canonicalCacheRoundTrip()
    }

    @Test("Non-round-trippable JSON numbers stay read-only after restart")
    func decimalSafetyMarker() throws {
        try EncryptedPlannerPersistenceScenarios.nonRoundTrippableNumberRemainsReadOnlyAfterRestart()
    }

    @Test("A stale process cannot overwrite a newer encrypted snapshot")
    func concurrentWriter() throws {
        try EncryptedPlannerPersistenceScenarios.concurrentStoreWriterFailsClosed()
    }

    @Test("Schema 1 migrates without inventing state")
    func schemaOneMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaOneMigratesWithoutInventingNewState()
    }

    @Test("Encrypted schema 1 is atomically rewritten")
    func encryptedSchemaOneMigration() throws {
        try EncryptedPlannerPersistenceScenarios.encryptedSchemaOneIsAtomicallyRewritten()
    }
}
#endif

#if PERSISTENCE_MANUAL_TEST
@main
private struct PersistenceManualTestRunner {
    @MainActor
    static func main() throws {
        try EncryptedPlannerPersistenceScenarios.roundTrip()
        try EncryptedPlannerPersistenceScenarios.encryptedAtRest()
        try EncryptedPlannerPersistenceScenarios.corruptionFailure()
        try EncryptedPlannerPersistenceScenarios.storeRestore()
        try EncryptedPlannerPersistenceScenarios.canonicalCacheRoundTrip()
        print("All encrypted planner persistence scenarios passed")
    }
}
#endif
