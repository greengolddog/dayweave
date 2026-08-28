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
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("DayWeavePersistenceTests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data((0..<32).map(UInt8.init)))
        return (
            directory,
            fileURL,
            EncryptedPlannerPersistence(fileURL: fileURL, key: key)
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

    func testPlannerStoreFlushesAndRestoresEncryptedState() throws {
        try EncryptedPlannerPersistenceScenarios.storeRestore()
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

    @Test("Planner store flushes and restores encrypted state")
    func storeRestore() throws {
        try EncryptedPlannerPersistenceScenarios.storeRestore()
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
        print("All encrypted planner persistence scenarios passed")
    }
}
#endif
