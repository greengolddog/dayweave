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
        let attributes = try FileManager.default.attributesOfItem(
            atPath: context.fileURL.path
        )
        let permissions = try requireValue(
            attributes[.posixPermissions] as? NSNumber,
            "Encrypted snapshot had no POSIX permissions"
        ).intValue & 0o777
        try require(
            permissions == 0o600,
            "Encrypted snapshot was committed without exact owner-only permissions"
        )
        let siblingNames = try FileManager.default.contentsOfDirectory(
            atPath: context.directory.path
        )
        try require(
            !siblingNames.contains(where: { $0.hasSuffix(".tmp") }),
            "Encrypted snapshot commit left a temporary envelope sibling"
        )
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

    static func crashOrphanedTemporaryEnvelopeIsCleanedSafely() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        try context.persistence.save(makeSnapshot())

        let baseName = context.fileURL.lastPathComponent
        let orphan = context.directory.appendingPathComponent(
            ".\(baseName).\(UUID().uuidString).tmp"
        )
        let nearMatch = context.directory.appendingPathComponent(
            ".\(baseName).NOT-A-UUID.tmp"
        )
        let symlink = context.directory.appendingPathComponent(
            ".\(baseName).\(UUID().uuidString).tmp"
        )
        try FileManager.default.copyItem(at: context.fileURL, to: orphan)
        try FileManager.default.copyItem(at: context.fileURL, to: nearMatch)
        try FileManager.default.createSymbolicLink(
            at: symlink,
            withDestinationURL: context.fileURL
        )

        _ = try context.persistence.loadRevisioned()

        try require(
            !FileManager.default.fileExists(atPath: orphan.path),
            "A crash-orphaned encrypted temporary envelope survived load"
        )
        try require(
            FileManager.default.fileExists(atPath: nearMatch.path),
            "Cleanup removed a sibling outside the exact app-owned pattern"
        )
        let symlinkDestination = try FileManager.default.destinationOfSymbolicLink(
            atPath: symlink.path
        )
        try require(
            !symlinkDestination.isEmpty,
            "Cleanup followed or removed a matching symbolic link"
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
                count: EncryptedPlannerPersistence.legacyMaximumPlaintextBytes + 1
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
                limitBytes: EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
            ),
            "Ordinary planner plaintext consumed publication-only headroom"
        )
    }

    static func oversizedOrdinaryPlaintextIsRejectedOnLoad() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let oversized = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: [AssistantMessage(
                id: UUID(),
                role: .user,
                text: String(
                    repeating: "x",
                    count: EncryptedPlannerPersistence.legacyMaximumPlaintextBytes + 1
                ),
                createdAt: base.savedAt
            )],
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        let plaintext = try encoder.encode(oversized)
        try require(
            plaintext.count > EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
                && plaintext.count <= EncryptedPlannerPersistence.maximumPlaintextBytes,
            "Ordinary load fixture did not exercise the contextual plaintext gate"
        )
        let sealed = try AES.GCM.seal(
            plaintext,
            using: SymmetricKey(data: context.keyData),
            authenticating: Data(
                "DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8
            )
        )
        let combined = try requireValue(
            sealed.combined,
            "AES-GCM did not produce a combined load fixture"
        )
        let envelope = try JSONSerialization.data(
            withJSONObject: [
                "magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT",
                "formatVersion": 1,
                "cipher": "AES.GCM.256",
                "sealedSnapshot": combined.base64EncodedString(),
            ],
            options: [.sortedKeys, .withoutEscapingSlashes]
        )
        try envelope.write(to: context.fileURL)

        var observedError: PlannerPersistenceError?
        do {
            _ = try context.persistence.load()
        } catch {
            observedError = error
        }
        try require(
            observedError == .snapshotTooLarge(
                limitBytes: EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
            ),
            "Decoded ordinary planner plaintext consumed publication-only headroom"
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
            autosaveDelay: .seconds(60),
            now: { snapshot.savedAt }
        )
        source.destination = .goals
        try source.updateScheduleProfile(
            ScheduleProfile.legacyDefault(
                timezoneName: source.scheduleProfile.timezoneName,
                protectedFreeMinutes: 135
            ),
            expectedCurrentProfile: source.scheduleProfile
        )
        source.freezeHours = 5
        source.showCompleted = false
        source.flushPersistence()
        try require(source.persistenceError == nil, "Source store failed to persist")

        let restored = PlannerStore(
            blocks: [],
            persistence: context.persistence,
            autosaveDelay: .seconds(60),
            now: { snapshot.savedAt }
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
        let sensitivityMutation = PendingCanonicalSensitivityMutation(
            id: UUID(uuidString: "61000000-0000-4000-8000-000000000006")!,
            itemID: item.id,
            desiredIsSensitive: false,
            baseRevision: item.revision,
            createdAt: base.savedAt,
            disposition: .conflicted,
            diagnostic: "privacy revision changed",
            hasBeenSubmitted: true,
            followUpIsSensitive: true
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
            pendingCanonicalSensitivityMutations: [sensitivityMutation],
            recurrenceSessionOutcomes: [outcome],
            canonicalConfigurationIdentifier: configurationIdentifier,
            schedulePreviewProvenance: provenance,
            localCaptureDiagnostics: [base.blocks[0].id: "Persistent recovery warning"]
        )

        try context.persistence.save(snapshot)
        let restored = try requireValue(context.persistence.load(), "Canonical snapshot was not restored")
        try require(restored.canonicalItems == [item], "Canonical item fields were not restored exactly")
        try require(restored.canonicalItems?.first?.isSensitive == true, "Sensitivity was not restored")
        try require(restored.canonicalDeltaCursor == "opaque-encrypted-cursor", "Delta cursor was not restored")
        try require(restored.completedOccurrenceIDs == snapshot.completedOccurrenceIDs, "Occurrence state was not restored")
        try require(restored.canonicalTombstoneRevisions == snapshot.canonicalTombstoneRevisions, "Tombstone revisions were not restored")
        try require(restored.pendingCanonicalMutations == [mutation], "Pending mutation was not restored")
        try require(
            restored.pendingCanonicalSensitivityMutations == [sensitivityMutation],
            "Pending privacy mutation was not restored"
        )
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

        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Legacy snapshot did not migrate to the current schema"
        )
        try require(
            migrated.executionState == .empty,
            "Legacy snapshot invented execution recovery state"
        )
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
        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Encrypted legacy snapshot did not migrate"
        )
        try require(
            migrated.executionState == .empty,
            "Encrypted legacy migration invented execution recovery state"
        )
        try require(
            migrated.blocks.first?.occurrenceFullyScheduled == true,
            "A genuine pre-field block did not receive its safe legacy default"
        )
        let secondLoad = try requireValue(context.persistence.load(), "Migrated snapshot was not rewritten")
        try require(
            secondLoad.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Migrated file was not durable"
        )
    }

    static func schemaFourAddsExplicitSensitivityDefaults() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 4,
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
            canonicalItems: [try canonicalItem()],
            executionState: .empty
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var object = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema-4 snapshot was not an object"
        )
        var blocks = try requireValue(object["blocks"] as? [[String: Any]], "Missing blocks")
        for index in blocks.indices { blocks[index].removeValue(forKey: "isSensitive") }
        object["blocks"] = blocks
        var items = try requireValue(
            object["canonicalItems"] as? [[String: Any]],
            "Missing canonical items"
        )
        for index in items.indices { items[index].removeValue(forKey: "is_sensitive") }
        object["canonicalItems"] = items
        let payload = try JSONSerialization.data(withJSONObject: object)

        let strict = JSONDecoder()
        strict.dateDecodingStrategy = .millisecondsSince1970
        do {
            _ = try strict.decode(PlannerSnapshot.self, from: payload)
            throw PersistenceScenarioFailure(description: "Current decoding accepted missing sensitivity")
        } catch is DecodingError {
            // Only the authenticated legacy snapshot path may supply the migration default.
        }

        let legacyDecoder = JSONDecoder()
        legacyDecoder.dateDecodingStrategy = .millisecondsSince1970
        legacyDecoder.userInfo[.dayWeaveAllowsMissingSensitivity] = true
        let decoded = try legacyDecoder.decode(PlannerSnapshot.self, from: payload)
        let migrated = try decoded.migratedToCurrentSchema()
        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema 4 did not migrate to the current schema"
        )
        try require(migrated.blocks.allSatisfy { !$0.isSensitive }, "Legacy block invented sensitivity")
        try require(
            migrated.canonicalItems?.allSatisfy { !$0.isSensitive } == true,
            "Legacy canonical item invented sensitivity"
        )
    }

    static func schemaFiveRemainsSensitivityStrictAndAddsNoPrivacyIntent() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 5,
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
            canonicalItems: [try canonicalItem()],
            executionState: .empty
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var object = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema-5 snapshot was not an object"
        )
        object.removeValue(forKey: "pendingCanonicalSensitivityMutations")
        let payload = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        let decoded = try decoder.decode(PlannerSnapshot.self, from: payload)
        let migrated = try decoded.migratedToCurrentSchema()
        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema 5 did not migrate to the current schema"
        )
        try require(
            migrated.pendingCanonicalSensitivityMutations == [],
            "Schema 5 migration invented a privacy edit"
        )

        var missingSensitivity = object
        var items = try requireValue(
            missingSensitivity["canonicalItems"] as? [[String: Any]],
            "Schema-5 canonical items were missing"
        )
        items[0].removeValue(forKey: "is_sensitive")
        missingSensitivity["canonicalItems"] = items
        do {
            _ = try decoder.decode(
                PlannerSnapshot.self,
                from: JSONSerialization.data(withJSONObject: missingSensitivity)
            )
            throw PersistenceScenarioFailure(
                description: "Schema 5 accepted a missing canonical sensitivity field"
            )
        } catch is DecodingError {
            // Schema 5 is never eligible for the pre-sensitivity migration default.
        }
    }

    static func schemaSixPrivacyIntentMigratesAsAmbiguouslySubmitted() throws {
        let base = makeSnapshot()
        let item = try canonicalItem()
        let mutation = PendingCanonicalSensitivityMutation(
            id: UUID(uuidString: "62000000-0000-4000-8000-000000000006")!,
            itemID: item.id,
            desiredIsSensitive: false,
            baseRevision: item.revision,
            createdAt: base.savedAt,
            disposition: .pending,
            diagnostic: nil
        )
        let legacy = PlannerSnapshot(
            schemaVersion: 6,
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
            pendingCanonicalSensitivityMutations: [mutation],
            executionState: .empty
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var object = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema-6 snapshot was not an object"
        )
        var mutations = try requireValue(
            object["pendingCanonicalSensitivityMutations"] as? [[String: Any]],
            "Schema-6 privacy intent was missing"
        )
        mutations[0].removeValue(forKey: "hasBeenSubmitted")
        mutations[0].removeValue(forKey: "followUpIsSensitive")
        object["pendingCanonicalSensitivityMutations"] = mutations
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        let decoded = try decoder.decode(
            PlannerSnapshot.self,
            from: JSONSerialization.data(withJSONObject: object)
        )
        let migrated = try decoded.migratedToCurrentSchema()
        let restored = try requireValue(
            migrated.pendingCanonicalSensitivityMutations?.first,
            "Schema-6 privacy intent was discarded"
        )
        try require(restored.hasBeenSubmitted, "Schema-6 ambiguity became cancelable")
        try require(restored.followUpIsSensitive == nil, "Migration invented a follow-up edit")
    }

    static func schemaSevenAddsNoPublicationIntent() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 7,
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
            canonicalItems: base.canonicalItems,
            canonicalDeltaCursor: base.canonicalDeltaCursor,
            canonicalTombstoneRevisions: base.canonicalTombstoneRevisions,
            completedOccurrenceIDs: base.completedOccurrenceIDs,
            pendingCanonicalMutations: base.pendingCanonicalMutations,
            pendingCanonicalSensitivityMutations: [],
            recurrenceSessionOutcomes: base.recurrenceSessionOutcomes,
            canonicalConfigurationIdentifier: base.canonicalConfigurationIdentifier,
            schedulePreviewProvenance: base.schedulePreviewProvenance,
            localCaptureDiagnostics: base.localCaptureDiagnostics,
            executionState: .empty
        )

        let migrated = try legacy.migratedToCurrentSchema()

        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema-7 snapshot did not migrate to the current schema"
        )
        try require(
            migrated.pendingSchedulePublication == nil,
            "Schema-7 migration invented a schedule publication"
        )
    }

    static func proposalApplicationJournalRoundTrip() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let receipt = proposalApplicationReceipt()
        let mutation = DayWeavePendingProposalApplicationMutation.undo(
            configurationIdentifier: receipt.configurationIdentifier,
            proposalIDs: receipt.application.proposals.map(\.proposalID),
            proposalRevisions: receipt.application.proposals.map(\.appliedRevision),
            expectedCommandIDs: receipt.application.commandIDs,
            applicationID: receipt.application.applicationID,
            expectedApplicationRevision: receipt.application.applicationRevision,
            requestBody: Data(#"{"expected_application_revision":1}"#.utf8),
            idempotencyKey: "proposal-undo-retry-key-0001",
            createdAt: base.savedAt.addingTimeInterval(60)
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
            canonicalConfigurationIdentifier: receipt.configurationIdentifier,
            pendingProposalApplicationMutation: mutation,
            proposalApplicationReceipts: [receipt]
        )

        try context.persistence.save(snapshot)
        let restored = try requireValue(
            context.persistence.load(),
            "Proposal application journal was not restored"
        )
        try require(
            restored.pendingProposalApplicationMutation == mutation,
            "Exact pending undo bytes or evidence changed during encrypted round-trip"
        )
        try require(
            restored.proposalApplicationReceipts == [receipt],
            "Content-free application receipt changed during encrypted round-trip"
        )
        let encrypted = try Data(contentsOf: context.fileURL)
        try require(
            encrypted.range(of: Data(mutation.idempotencyKey.utf8)) == nil,
            "Proposal retry key leaked outside encrypted snapshot content"
        )
    }

    static func schemaEightAddsNoProposalApplicationIntent() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 8,
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
            pendingCanonicalSensitivityMutations: [],
            executionState: .empty
        )

        let migrated = try legacy.migratedToCurrentSchema()

        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema-8 snapshot did not migrate to the current schema"
        )
        try require(
            migrated.pendingProposalApplicationMutation == nil,
            "Schema-8 migration invented a pending proposal mutation"
        )
        try require(
            migrated.proposalApplicationReceipts == [],
            "Schema-8 migration invented proposal application receipts"
        )
    }

    static func canonicalAuthoringJournalRoundTrip() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let itemID = UUID(uuidString: "a1000000-0000-4000-8000-000000000001")!
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "a2000000-0000-4000-8000-000000000002")!,
            itemID: itemID,
            operation: .create,
            draft: .init(
                isSensitive: true,
                title: "ENCRYPTED-AUTHORING-CANARY",
                notes: "Offline private draft",
                timezoneName: "Europe/Madrid",
                durationSeconds: nil
            ),
            createdAt: base.savedAt
        )
        let snapshot = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: .inbox,
            selectedBlockID: nil,
            selectedCanonicalItemID: itemID,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "Offline draft",
            protectedFreeMinutes: 90,
            freezeHours: 2,
            showCompleted: true,
            pendingCanonicalAuthoringMutations: [mutation],
            canonicalTrash: []
        )

        try context.persistence.save(snapshot)
        let restored = try requireValue(
            context.persistence.load(),
            "Canonical authoring journal was not restored"
        )
        try require(
            restored.pendingCanonicalAuthoringMutations == [mutation],
            "Canonical authoring mutation changed during encrypted round-trip"
        )
        try require(
            restored.selectedCanonicalItemID == itemID,
            "Destination-aware canonical selection was not restored"
        )
        let encrypted = try Data(contentsOf: context.fileURL)
        try require(
            encrypted.range(of: Data("ENCRYPTED-AUTHORING-CANARY".utf8)) == nil,
            "Canonical authoring content leaked outside the encrypted snapshot"
        )
    }

    static func schemaTwentyTwoAuthoringDraftMigrationAddsRollbackFence() throws {
        let base = makeSnapshot()
        let itemID = UUID(uuidString: "a3000000-0000-4000-8000-000000000003")!
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "a4000000-0000-4000-8000-000000000004")!,
            itemID: itemID,
            operation: .create,
            draft: .init(
                title: "Legacy scalar duration",
                timezoneName: "UTC",
                durationSeconds: 2_700
            ),
            createdAt: base.savedAt
        )
        let legacy = PlannerSnapshot(
            schemaVersion: 22,
            savedAt: base.savedAt,
            destination: .inbox,
            selectedBlockID: nil,
            selectedCanonicalItemID: itemID,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "Legacy draft",
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: 2,
            showCompleted: true,
            pendingCanonicalAuthoringMutations: [mutation],
            canonicalTrash: []
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var root = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema 22 draft fixture was not an object"
        )
        var mutations = try requireValue(
            root["pendingCanonicalAuthoringMutations"] as? [[String: Any]],
            "Schema 22 draft fixture had no authoring mutation"
        )
        var draft = try requireValue(
            mutations[0]["draft"] as? [String: Any],
            "Schema 22 mutation had no draft"
        )
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 22
        var rejectedInjectedShape = false
        do {
            _ = try decoder.decode(
                PlannerSnapshot.self,
                from: JSONSerialization.data(withJSONObject: root)
            )
        } catch is DecodingError {
            rejectedInjectedShape = true
        }
        try require(
            rejectedInjectedShape,
            "Schema 22 draft accepted injected rich-duration authority"
        )
        for key in [
            "duration_kind", "duration_min_seconds", "duration_max_seconds",
            "duration_source",
        ] {
            draft.removeValue(forKey: key)
        }
        mutations[0].removeValue(forKey: "durationWireShape")
        mutations[0]["draft"] = draft
        root["pendingCanonicalAuthoringMutations"] = mutations

        let migrated = try decoder.decode(
            PlannerSnapshot.self,
            from: JSONSerialization.data(withJSONObject: root)
        ).migratedToCurrentSchema()
        let migratedDraft = try requireValue(
            migrated.pendingCanonicalAuthoringMutations?.first?.draft,
            "Schema 22 migration discarded its authoring draft"
        )
        try require(
            migrated.pendingCanonicalAuthoringMutations?.first?.durationWireShape == .richV2,
            "An unsubmitted schema 22 draft did not upgrade to the current request shape"
        )
        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion
                && PlannerSnapshot.currentSchemaVersion == 24,
            "Rich authoring drafts are not protected by the current rollback fence"
        )
        try require(
            migratedDraft.durationKind == .exact
                && migratedDraft.durationMinimumSeconds == 2_700
                && migratedDraft.durationSeconds == 2_700
                && migratedDraft.durationMaximumSeconds == 2_700
                && migratedDraft.durationSource == .user,
            "Schema 22 scalar duration did not upgrade to the exact rich shape"
        )
        let rewritten = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(migrated)) as? [String: Any],
            "Migrated current-schema fixture was not an object"
        )
        let rewrittenDraft = try requireValue(
            (rewritten["pendingCanonicalAuthoringMutations"] as? [[String: Any]])?
                .first?["draft"] as? [String: Any],
            "Migrated current-schema fixture had no authoring draft"
        )
        try require(
            rewrittenDraft["duration_kind"] as? String == "exact"
                && rewrittenDraft["duration_min_seconds"] as? Int == 2_700
                && rewrittenDraft["duration_max_seconds"] as? Int == 2_700
                && rewrittenDraft["duration_source"] as? String == "user",
            "Current-schema rewrite did not durably persist rich duration metadata"
        )

        var partialCurrentRoot = rewritten
        var partialCurrentMutations = try requireValue(
            partialCurrentRoot["pendingCanonicalAuthoringMutations"] as? [[String: Any]],
            "Schema 23 corruption fixture had no authoring mutation"
        )
        var partialCurrentDraft = try requireValue(
            partialCurrentMutations[0]["draft"] as? [String: Any],
            "Schema 23 corruption fixture had no draft"
        )
        partialCurrentDraft.removeValue(forKey: "duration_kind")
        partialCurrentMutations[0]["draft"] = partialCurrentDraft
        partialCurrentRoot["pendingCanonicalAuthoringMutations"] = partialCurrentMutations
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 23
        var rejectedPartialCurrentShape = false
        do {
            _ = try decoder.decode(
                PlannerSnapshot.self,
                from: JSONSerialization.data(withJSONObject: partialCurrentRoot)
            )
        } catch is DecodingError {
            rejectedPartialCurrentShape = true
        }
        try require(
            rejectedPartialCurrentShape,
            "Schema 23 draft silently normalized partial rich-duration metadata"
        )
    }

    static func schemaTwentyThreeCannotAcquireOccurrenceMembershipAuthority() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let configurationIdentifier = "publication-membership-fixture"
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configurationIdentifier,
            generatedAt: now,
            asOf: now,
            horizonStart: now.addingTimeInterval(-3_600),
            horizonEnd: now.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let revisionID = UUID(uuidString: "a5000000-0000-4000-8000-000000000005")!
        func proof(version: Int) -> DayWeavePublishedScheduleProof {
            DayWeavePublishedScheduleProof(
                version: version,
                configurationIdentifier: configurationIdentifier,
                revisionID: revisionID,
                revision: "1:\(revisionID.uuidString.lowercased())",
                revisionNumber: 1,
                inputDigest: "sha256:\(String(repeating: "c", count: 64))",
                asOf: now,
                horizonStart: provenance.horizonStart,
                horizonEnd: provenance.horizonEnd,
                timezoneName: provenance.timezoneName,
                publishedAt: now,
                publishedBlocks: [],
                publishedOccurrences: version == DayWeavePublishedScheduleProof.currentVersion
                    ? [.init(
                        plannerOccurrenceID: UUID(
                            uuidString: "a6000000-0000-5000-8000-000000000006"
                        )!,
                        seriesItemID: UUID(
                            uuidString: "a7000000-0000-4000-8000-000000000007"
                        )!,
                        state: "generated"
                    )]
                    : nil
            )
        }
        func snapshot(
            proof: DayWeavePublishedScheduleProof,
            injectedHintRevision: UInt64? = nil
        ) throws -> PlannerSnapshot {
            PlannerSnapshot(
                schemaVersion: 23,
                savedAt: now,
                destination: .today,
                selectedBlockID: nil,
                blocks: [],
                suggestions: [],
                assistantMessages: [],
                lastScheduleMessage: "Published",
                protectedFreeMinutes: 90,
                scheduleProfile: try .legacyDefault(
                    timezoneName: "UTC",
                    protectedFreeMinutes: 90
                ),
                freezeHours: 2,
                showCompleted: true,
                canonicalItems: [],
                canonicalConfigurationIdentifier: configurationIdentifier,
                schedulePreviewProvenance: provenance,
                publishedScheduleProof: proof,
                publishedScheduleLatestHintRevision: injectedHintRevision
            )
        }

        let legacyV2 = proof(version: 2)
        let migratedV2 = try snapshot(proof: legacyV2).migratedToCurrentSchema()
        try require(
            migratedV2.publishedScheduleProof == legacyV2
                && migratedV2.publishedScheduleProof?.currentOccurrenceAuthority == nil
                && migratedV2.publishedScheduleLatestHintRevision
                    == legacyV2.revisionNumber,
            "Schema 23 did not preserve legacy block proof without granting occurrence authority"
        )

        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        let injectedPlaintext = try encoder.encode(
            try snapshot(
                proof: proof(version: DayWeavePublishedScheduleProof.currentVersion),
                injectedHintRevision: 99
            )
        )
        let sealed = try AES.GCM.seal(
            injectedPlaintext,
            using: SymmetricKey(data: context.keyData),
            authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8)
        )
        let combined = try requireValue(
            sealed.combined,
            "Injected predecessor ciphertext was unavailable"
        )
        let envelope: [String: Any] = [
            "magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT",
            "formatVersion": 1,
            "cipher": "AES.GCM.256",
            "sealedSnapshot": combined.base64EncodedString(),
        ]
        try JSONSerialization.data(withJSONObject: envelope).write(to: context.fileURL)

        let migrated = try requireValue(
            context.persistence.load(),
            "Authenticated schema 23 fixture did not migrate"
        )
        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion
                && migrated.publishedScheduleProof == nil
                && migrated.publishedScheduleLatestHintRevision == 0,
            "Schema 23 acquired injected v3 occurrence-membership authority"
        )
        let rewritten = try requireValue(
            context.persistence.load(),
            "Migrated occurrence-membership snapshot was not rewritten"
        )
        try require(
            rewritten.publishedScheduleProof == nil
                && rewritten.publishedScheduleLatestHintRevision == 0,
            "Injected occurrence-membership authority survived encrypted rewrite"
        )
    }

    static func schemaTwentyTwoSubmittedAuthoringKeepsLegacyRequestShape() throws {
        let base = makeSnapshot()
        let configuration =
            "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "b", count: 64))"
        let createItemID = UUID(uuidString: "a5000000-0000-4000-8000-000000000005")!
        let replaceItemID = UUID(uuidString: "a6000000-0000-4000-8000-000000000006")!
        let createDraft = DayWeaveCanonicalItemDraft(
            title: "Lost create response",
            timezoneName: "UTC",
            durationSeconds: 1_800
        )
        let baseItemData = Data(#"""
        {
          "id":"\#(replaceItemID.uuidString.lowercased())","is_sensitive":false,
          "kind":"task","status":"inbox","title":"Lost replace response",
          "notes":null,"timezone_name":"UTC","duration_seconds":2700,
          "deadline_at":null,"earliest_start_at":null,"recurrence":null,
          "flexible_constraints":{},"split_policy":{"type":"indivisible"},
          "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
          "is_executable":true,"revision":4,
          "created_at":"2026-09-01T10:00:00Z","updated_at":"2026-09-01T11:00:00Z",
          "completed_at":null,"deleted_at":null
        }
        """#.utf8)
        let itemDecoder = JSONDecoder()
        itemDecoder.dateDecodingStrategy = .iso8601
        let replaceBase = try itemDecoder.decode(
            DayWeaveCanonicalItem.self,
            from: baseItemData
        )
        let create = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "a7000000-0000-4000-8000-000000000007")!,
            itemID: createItemID,
            operation: .create,
            draft: createDraft,
            createdAt: base.savedAt,
            configurationIdentifier: configuration,
            hasBeenSubmitted: true
        )
        let replace = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "a8000000-0000-4000-8000-000000000008")!,
            itemID: replaceItemID,
            operation: .replace,
            draft: DayWeaveCanonicalItemDraft(item: replaceBase),
            expectedRevision: replaceBase.revision,
            baseItem: replaceBase,
            createdAt: base.savedAt,
            configurationIdentifier: configuration,
            hasBeenSubmitted: true
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        for submitted in [create, replace] {
            let isReplace = submitted.operation == .replace
            let legacy = PlannerSnapshot(
                schemaVersion: 22,
                savedAt: base.savedAt,
                destination: .inbox,
                selectedBlockID: nil,
                selectedCanonicalItemID: submitted.itemID,
                blocks: [],
                suggestions: [],
                assistantMessages: [],
                lastScheduleMessage: "Response-loss replay",
                protectedFreeMinutes: base.protectedFreeMinutes,
                scheduleProfile: base.scheduleProfile,
                freezeHours: 2,
                showCompleted: true,
                canonicalItems: isReplace ? [replaceBase] : [],
                canonicalConfigurationIdentifier: configuration,
                pendingCanonicalAuthoringMutations: [submitted],
                canonicalTrash: []
            )
            var root = try requireValue(
                JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
                "Schema 22 response-loss fixture was not an object"
            )
            var mutations = try requireValue(
                root["pendingCanonicalAuthoringMutations"] as? [[String: Any]],
                "Schema 22 response-loss fixture had no journal"
            )
            var draft = try requireValue(
                mutations[0]["draft"] as? [String: Any],
                "Schema 22 response-loss journal had no draft"
            )
            for key in [
                "duration_kind", "duration_min_seconds", "duration_max_seconds",
                "duration_source",
            ] {
                draft.removeValue(forKey: key)
            }
            mutations[0].removeValue(forKey: "durationWireShape")
            mutations[0]["draft"] = draft
            root["pendingCanonicalAuthoringMutations"] = mutations
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .millisecondsSince1970
            decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 22
            let migrated = try decoder.decode(
                PlannerSnapshot.self,
                from: JSONSerialization.data(withJSONObject: root)
            ).migratedToCurrentSchema()
            let journals = try requireValue(
                migrated.pendingCanonicalAuthoringMutations,
                "Submitted response-loss journal was discarded"
            )
            try require(journals.count == 1, "Response-loss migration changed journal count")
            let journal = try requireValue(
                journals.first,
                "Submitted response-loss journal was discarded"
            )
            try require(
                journal.durationWireShape == .legacyV1,
                "Submitted schema 22 request did not retain its legacy wire shape"
            )
            let migratedDraft = try requireValue(
                journal.draft,
                "Migrated replay lost its item body"
            )
            let fieldsData = try JSONEncoder().encode(
                migratedDraft.requestFields(durationWireShape: journal.durationWireShape)
            )
            let fields = try requireValue(
                JSONSerialization.jsonObject(with: fieldsData) as? [String: Any],
                "Migrated replay fields were not an object"
            )
            try require(
                fields["duration_seconds"] != nil
                    && fields["duration_kind"] == nil
                    && fields["duration_min_seconds"] == nil
                    && fields["duration_max_seconds"] == nil
                    && fields["duration_source"] == nil,
                "Migrated response-loss replay changed its frozen duration body"
            )
        }

        let trash = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "a9000000-0000-4000-8000-000000000009")!,
            itemID: replaceItemID,
            operation: .trash,
            expectedRevision: replaceBase.revision,
            baseItem: replaceBase,
            createdAt: base.savedAt,
            configurationIdentifier: configuration,
            hasBeenSubmitted: true
        )
        let restore = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "aa000000-0000-4000-8000-00000000000a")!,
            itemID: replaceItemID,
            operation: .restore,
            expectedRevision: replaceBase.revision,
            createdAt: base.savedAt,
            configurationIdentifier: configuration,
            hasBeenSubmitted: true
        )
        let deletedEntry = DayWeaveCanonicalTrashEntry(
            id: replaceItemID,
            revision: replaceBase.revision,
            deletedAt: base.savedAt,
            parentID: nil,
            lastKnownItem: nil
        )
        for submitted in [trash, restore] {
            let isRestore = submitted.operation == .restore
            let legacy = PlannerSnapshot(
                schemaVersion: 22,
                savedAt: base.savedAt,
                destination: .inbox,
                selectedBlockID: nil,
                selectedCanonicalItemID: submitted.itemID,
                blocks: [],
                suggestions: [],
                assistantMessages: [],
                lastScheduleMessage: "Identity-only response-loss replay",
                protectedFreeMinutes: base.protectedFreeMinutes,
                scheduleProfile: base.scheduleProfile,
                freezeHours: 2,
                showCompleted: true,
                canonicalItems: isRestore ? [] : [replaceBase],
                canonicalTombstoneRevisions: isRestore
                    ? [replaceItemID: replaceBase.revision]
                    : [:],
                canonicalConfigurationIdentifier: configuration,
                pendingCanonicalAuthoringMutations: [submitted],
                canonicalTrash: isRestore ? [deletedEntry] : []
            )
            var root = try requireValue(
                JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
                "Schema 22 identity-only fixture was not an object"
            )
            var mutations = try requireValue(
                root["pendingCanonicalAuthoringMutations"] as? [[String: Any]],
                "Schema 22 identity-only fixture had no journal"
            )
            mutations[0].removeValue(forKey: "durationWireShape")
            root["pendingCanonicalAuthoringMutations"] = mutations
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .millisecondsSince1970
            decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 22
            let migrated = try decoder.decode(
                PlannerSnapshot.self,
                from: JSONSerialization.data(withJSONObject: root)
            ).migratedToCurrentSchema()
            let journal = try requireValue(
                migrated.pendingCanonicalAuthoringMutations?.first,
                "Submitted identity-only response-loss journal was discarded"
            )
            try require(
                journal.operation == submitted.operation
                    && journal.durationWireShape == .richV2
                    && journal.isValid,
                "Schema 22 identity-only replay acquired a body-only legacy wire shape"
            )
        }
    }

    static func schemaNineAddsNoCanonicalAuthoringIntent() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 9,
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
            pendingCanonicalSensitivityMutations: [],
            proposalApplicationReceipts: [],
            executionState: .empty
        )

        let migrated = try legacy.migratedToCurrentSchema()

        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema-9 snapshot did not migrate to schema 10"
        )
        try require(
            migrated.pendingCanonicalAuthoringMutations == [],
            "Schema-9 migration invented canonical authoring intent"
        )
        try require(
            migrated.canonicalTrash == [],
            "Schema-9 migration invented deleted-item history"
        )
        try require(
            migrated.selectedCanonicalItemID == nil,
            "Schema-9 migration invented a canonical selection"
        )
    }

    static func malformedCanonicalAuthoringStateIsRejected() throws {
        let base = makeSnapshot()
        let mutation = DayWeavePendingCanonicalAuthoringMutation(
            itemID: UUID(),
            operation: .create,
            draft: .init(title: "Submitted without binding", timezoneName: "UTC"),
            createdAt: base.savedAt,
            hasBeenSubmitted: true
        )
        let malformed = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: .inbox,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "Malformed",
            protectedFreeMinutes: 90,
            freezeHours: 2,
            showCompleted: true,
            pendingCanonicalAuthoringMutations: [mutation],
            canonicalTrash: []
        )

        var observedMigrationError: PlannerPersistenceError?
        do {
            _ = try malformed.migratedToCurrentSchema()
        } catch {
            observedMigrationError = error
        }
        try require(
            observedMigrationError == .snapshotDecodingFailed,
            "Submitted unbound authoring state did not fail strict decoding"
        )

        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        var observedSaveError: PlannerPersistenceError?
        do {
            try context.persistence.save(malformed)
        } catch {
            observedSaveError = error
        }
        try require(
            observedSaveError == .snapshotEncodingFailed,
            "Malformed authoring state was written to encrypted persistence"
        )
    }

    static func typedCodexSuggestionRoundTripsEncrypted() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let suggestion = typedCodexSuggestion(createdAt: base.savedAt)
        let snapshot = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: [suggestion],
            localSuggestionDateHighWater: base.savedAt,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted
        )

        try context.persistence.save(snapshot)
        let restored = try requireValue(
            context.persistence.load(),
            "Typed Codex suggestion was not restored"
        )
        try require(
            restored.suggestions == [suggestion],
            "Typed Codex suggestion changed during encrypted round-trip"
        )
        try require(
            restored.localSuggestionDateHighWater == base.savedAt,
            "Typed Codex retention high-water changed during encrypted round-trip"
        )
        let encrypted = try Data(contentsOf: context.fileURL)
        try require(
            encrypted.range(of: Data("AI-DRAFT-ENCRYPTION-CANARY".utf8)) == nil,
            "Typed Codex draft leaked outside encrypted snapshot content"
        )
    }

    static func schemaTwentyOneCanonicalStructureMigratesWithoutLoss() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 21,
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted,
            canonicalItems: [try canonicalItem()]
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var root = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema 21 fixture was not an object"
        )
        var items = try requireValue(
            root["canonicalItems"] as? [[String: Any]],
            "Schema 21 fixture had no canonical item"
        )
        let newerItemKeys = [
            "duration_kind", "duration_min_seconds", "duration_max_seconds",
            "duration_source", "deadline_kind", "deadline_date", "deadline_strength",
            "deadline_soft_weight", "has_own_effort", "blocked_reason_kind",
            "blocked_by_item_id", "blocked_reason", "_dayweave_exact_deadline_at",
            "_dayweave_explicit_structural_metadata",
        ]
        for key in newerItemKeys { items[0].removeValue(forKey: key) }
        root["canonicalItems"] = items
        let legacyData = try JSONSerialization.data(withJSONObject: root)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 21

        let migrated = try decoder.decode(PlannerSnapshot.self, from: legacyData)
            .migratedToCurrentSchema()
        let item = try requireValue(
            migrated.canonicalItems?.first,
            "Schema 21 migration discarded its canonical item"
        )
        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema 21 did not migrate to the current schema"
        )
        try require(
            item.durationKind == .exact
                && item.durationMinimumSeconds == 3_600
                && item.durationMaximumSeconds == 3_600
                && item.durationSource == .user,
            "Schema 21 duration was not inferred losslessly"
        )
        try require(
            item.deadlineKind == .dateTime
                && item.deadlineStrength == .hard
                && item.retainedCanonicalDeadlineAt != nil,
            "Schema 21 deadline was not retained through migration"
        )
        try require(
            item.unsupportedFields["future_rule"] != nil,
            "Schema 21 migration discarded a future canonical field"
        )

        let rewrittenRoot = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(migrated)) as? [String: Any],
            "Migrated schema 22 fixture was not an object"
        )
        let rewrittenItem = try requireValue(
            (rewrittenRoot["canonicalItems"] as? [[String: Any]])?.first,
            "Migrated schema 22 fixture had no canonical item"
        )
        try require(
            Set(newerItemKeys).isSubset(of: Set(rewrittenItem.keys)),
            "Migrated schema 22 item did not persist its complete structural shape"
        )
    }

    static func schemaTwentyOneForwardCapturedStructureMigratesWithoutLoss() throws {
        let base = makeSnapshot()
        let capturedItem = try canonicalItem()
        let pendingMutation = DayWeavePendingCanonicalAuthoringMutation(
            id: UUID(uuidString: "52000000-0000-4000-8000-000000000052")!,
            itemID: capturedItem.id,
            operation: .trash,
            expectedRevision: capturedItem.revision,
            baseItem: capturedItem,
            createdAt: base.savedAt
        )
        let legacy = PlannerSnapshot(
            schemaVersion: 21,
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted,
            canonicalItems: [capturedItem],
            pendingCanonicalAuthoringMutations: [pendingMutation]
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var root = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema 21 forward-capture fixture was not an object"
        )
        let originalItem = try requireValue(
            (root["canonicalItems"] as? [[String: Any]])?.first,
            "Schema 21 forward-capture fixture had no canonical item"
        )

        func withoutCurrentMarkers(_ value: [String: Any]) -> [String: Any] {
            var result = value
            result.removeValue(forKey: "_dayweave_exact_deadline_at")
            result.removeValue(forKey: "_dayweave_explicit_structural_metadata")
            return result
        }

        var richProject = withoutCurrentMarkers(originalItem)
        richProject.removeValue(forKey: "future_rule")
        richProject["kind"] = "project"
        richProject["title"] = "Forward-captured rich project"
        richProject["duration_kind"] = "range"
        richProject["duration_min_seconds"] = 1_800
        richProject["duration_seconds"] = 3_600
        richProject["duration_max_seconds"] = 7_200
        richProject["duration_source"] = "assistant"
        richProject["deadline_kind"] = "date"
        richProject["deadline_at"] = NSNull()
        richProject["deadline_date"] = "2026-09-30"
        richProject["deadline_strength"] = "soft"
        richProject["deadline_soft_weight"] = 75
        richProject["has_own_effort"] = true
        var constraints = try requireValue(
            richProject["flexible_constraints"] as? [String: Any],
            "Schema 21 rich item had no flexible constraints"
        )
        constraints["has_own_effort"] = true
        richProject["flexible_constraints"] = constraints

        var blocked = withoutCurrentMarkers(originalItem)
        blocked.removeValue(forKey: "future_rule")
        let blockedID = UUID(uuidString: "53000000-0000-4000-8000-000000000053")!
        blocked["id"] = blockedID.uuidString.lowercased()
        blocked["status"] = "blocked"
        blocked["title"] = "Forward-captured blocked task"
        blocked["blocked_reason_kind"] = "manual"
        blocked["blocked_by_item_id"] = NSNull()
        blocked["blocked_reason"] = "Waiting for review"
        root["canonicalItems"] = [richProject, blocked]

        var mutations = try requireValue(
            root["pendingCanonicalAuthoringMutations"] as? [[String: Any]],
            "Schema 21 forward-capture fixture had no pending authoring mutation"
        )
        mutations[0].removeValue(forKey: "durationWireShape")
        mutations[0]["baseItem"] = richProject
        root["pendingCanonicalAuthoringMutations"] = mutations

        let legacyData = try JSONSerialization.data(withJSONObject: root)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 21
        let migrated = try decoder.decode(PlannerSnapshot.self, from: legacyData)
            .migratedToCurrentSchema()

        let items = try requireValue(
            migrated.canonicalItems,
            "Schema 21 forward capture discarded canonical items"
        )
        let project = try requireValue(
            items.first(where: { $0.id == capturedItem.id }),
            "Schema 21 forward capture discarded its rich project"
        )
        try require(
            project.kind == .project
                && project.status == .planned
                && project.durationKind == .range
                && project.durationMinimumSeconds == 1_800
                && project.durationSeconds == 3_600
                && project.durationMaximumSeconds == 7_200
                && project.durationSource == .assistant
                && project.deadlineKind == .date
                && project.deadlineAt == nil
                && project.deadlineDate == "2026-09-30"
                && project.deadlineStrength == .soft
                && project.deadlineSoftWeight == 75
                && project.hasOwnEffort,
            "Schema 21 migration normalized a complete rich structural shape"
        )
        try require(
            project.hasExplicitStructuralMetadata && !project.supportsLosslessReplacement,
            "Schema 21 rich project was not fenced from legacy full-item writes"
        )
        let blockedItem = try requireValue(
            items.first(where: { $0.id == blockedID }),
            "Schema 21 forward capture discarded its Blocked item"
        )
        try require(
            blockedItem.status == .blocked
                && blockedItem.blockedReasonKind == .manual
                && blockedItem.blockedByItemID == nil
                && blockedItem.blockedReason == "Waiting for review",
            "Schema 21 migration normalized the Blocked reason tuple"
        )
        try require(
            blockedItem.hasExplicitStructuralMetadata
                && !blockedItem.supportsLosslessReplacement
                && blockedItem.retainedCanonicalDeadlineAt
                    == "2026-09-01T17:00:00.000Z",
            "Schema 21 Blocked item was not retained exactly and fenced"
        )

        let pendingBase = try requireValue(
            migrated.pendingCanonicalAuthoringMutations?.first?.baseItem,
            "Schema 21 migration discarded a nested pending base item"
        )
        try require(
            pendingBase.durationKind == .range
                && pendingBase.deadlineKind == .date
                && pendingBase.deadlineDate == "2026-09-30"
                && pendingBase.hasExplicitStructuralMetadata
                && !pendingBase.supportsLosslessReplacement,
            "Schema 21 migration normalized or unfenced a nested pending base item"
        )

        let rewrittenRoot = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(migrated)) as? [String: Any],
            "Migrated forward-capture fixture was not an object"
        )
        let rewrittenItems = try requireValue(
            rewrittenRoot["canonicalItems"] as? [[String: Any]],
            "Migrated forward-capture fixture had no canonical items"
        )
        let rewrittenProject = try requireValue(
            rewrittenItems.first(where: {
                ($0["id"] as? String)?.lowercased() == capturedItem.id.uuidString.lowercased()
            }),
            "Migrated forward-capture fixture had no rich project"
        )
        let rewrittenBlocked = try requireValue(
            rewrittenItems.first(where: {
                ($0["id"] as? String)?.lowercased() == blockedID.uuidString.lowercased()
            }),
            "Migrated forward-capture fixture had no Blocked item"
        )
        try require(
            rewrittenProject["duration_kind"] as? String == "range"
                && rewrittenProject["deadline_date"] as? String == "2026-09-30"
                && rewrittenProject["_dayweave_explicit_structural_metadata"] as? Bool == true
                && rewrittenBlocked["blocked_reason_kind"] as? String == "manual"
                && rewrittenBlocked["blocked_reason"] as? String == "Waiting for review"
                && rewrittenBlocked["_dayweave_exact_deadline_at"] as? String
                    == "2026-09-01T17:00:00.000Z"
                && rewrittenBlocked["_dayweave_explicit_structural_metadata"] as? Bool == true,
            "Schema 22 rewrite did not durably preserve forward-captured structural values"
        )
    }

    static func schemaTwentyOnePartialCanonicalStructureFailsClosed() throws {
        let base = makeSnapshot()
        let legacy = PlannerSnapshot(
            schemaVersion: 21,
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: base.suggestions,
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted,
            canonicalItems: [try canonicalItem()]
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var root = try requireValue(
            JSONSerialization.jsonObject(with: encoder.encode(legacy)) as? [String: Any],
            "Schema 21 partial-shape fixture was not an object"
        )
        var items = try requireValue(
            root["canonicalItems"] as? [[String: Any]],
            "Schema 21 partial-shape fixture had no canonical item"
        )
        items[0].removeValue(forKey: "blocked_reason")
        items[0].removeValue(forKey: "_dayweave_exact_deadline_at")
        items[0].removeValue(forKey: "_dayweave_explicit_structural_metadata")
        root["canonicalItems"] = items

        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 21
        var rejected = false
        do {
            _ = try decoder.decode(
                PlannerSnapshot.self,
                from: JSONSerialization.data(withJSONObject: root)
            )
        } catch is DecodingError {
            rejected = true
        }
        try require(
            rejected,
            "Schema 21 partial structural shape was silently normalized"
        )
    }

    static func schemaSeventeenCannotAcquireCodexDraftAuthority() throws {
        let base = makeSnapshot()
        let injected = typedCodexSuggestion(createdAt: base.savedAt)
        let legacy = PlannerSnapshot(
            schemaVersion: 17,
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: [injected],
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        let encoded = try encoder.encode(legacy)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = 17
        let decoded = try decoder.decode(PlannerSnapshot.self, from: encoded)
        let migrated = try decoded.migratedToCurrentSchema()

        try require(
            migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion,
            "Schema 17 did not migrate to the current planner schema"
        )
        try require(migrated.suggestions.count == 1, "Legacy suggestion was unexpectedly dropped")
        guard case .advisory = migrated.suggestions[0].payload else {
            throw PersistenceScenarioFailure(
                description: "Schema 17 injected an actionable Codex item draft"
            )
        }
        try require(
            migrated.suggestions[0].resultingItemID == nil
                && migrated.suggestions[0].resultingMutationID == nil,
            "Schema 17 injected accepted-item linkage"
        )
        try require(
            migrated.localSuggestionDateHighWater == nil,
            "Schema 17 migration invented a typed-draft clock authority"
        )
    }

    static func malformedCodexHighWaterIsRejected() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let pending = typedCodexSuggestion(createdAt: base.savedAt)

        func snapshot(
            suggestions: [PlanningSuggestion] = [pending],
            highWater: Date?
        ) -> PlannerSnapshot {
            PlannerSnapshot(
                savedAt: base.savedAt,
                destination: base.destination,
                selectedBlockID: base.selectedBlockID,
                blocks: base.blocks,
                suggestions: suggestions,
                localSuggestionDateHighWater: highWater,
                assistantMessages: base.assistantMessages,
                lastScheduleMessage: base.lastScheduleMessage,
                protectedFreeMinutes: base.protectedFreeMinutes,
                scheduleProfile: base.scheduleProfile,
                freezeHours: base.freezeHours,
                showCompleted: base.showCompleted
            )
        }

        let malformed = [
            snapshot(highWater: nil),
            snapshot(highWater: pending.createdAt.addingTimeInterval(-1)),
            snapshot(highWater: pending.expiresAt),
            snapshot(highWater: Date(timeIntervalSinceReferenceDate: .infinity)),
            snapshot(suggestions: base.suggestions, highWater: base.savedAt),
        ]
        for value in malformed {
            var observedError: PlannerPersistenceError?
            do {
                try context.persistence.preflightSave(value)
            } catch {
                observedError = error
            }
            try require(
                observedError == .snapshotEncodingFailed,
                "Malformed typed-draft retention high-water was persistable"
            )
        }
    }

    static func malformedCodexSuggestionStateIsRejected() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = makeSnapshot()
        let pending = typedCodexSuggestion(createdAt: base.savedAt)
        guard case let .canonicalItemDraft(itemDraft) = pending.payload else {
            throw PersistenceScenarioFailure(description: "Typed fixture was not actionable")
        }
        let malformed = PlanningSuggestion(
            id: pending.id,
            title: pending.title,
            summary: pending.summary,
            source: pending.source,
            createdAt: pending.createdAt,
            expiresAt: pending.expiresAt,
            state: .accepted,
            payload: .canonicalItemReference(itemID: itemDraft.itemID),
            resultingItemID: itemDraft.itemID,
            resultingMutationID: nil
        )
        let snapshot = PlannerSnapshot(
            savedAt: base.savedAt,
            destination: base.destination,
            selectedBlockID: base.selectedBlockID,
            blocks: base.blocks,
            suggestions: [malformed],
            assistantMessages: base.assistantMessages,
            lastScheduleMessage: base.lastScheduleMessage,
            protectedFreeMinutes: base.protectedFreeMinutes,
            scheduleProfile: base.scheduleProfile,
            freezeHours: base.freezeHours,
            showCompleted: base.showCompleted
        )
        var observedError: PlannerPersistenceError?
        do {
            try context.persistence.preflightSave(snapshot)
        } catch {
            observedError = error
        }
        try require(
            observedError == .snapshotEncodingFailed,
            "Malformed accepted Codex suggestion was persistable"
        )
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

    private static func typedCodexSuggestion(createdAt: Date) -> PlanningSuggestion {
        let itemID = UUID(uuidString: "21000000-0000-4000-8000-000000000002")!
        let draft = DayWeaveCanonicalItemDraft(
            isSensitive: true,
            kind: .task,
            status: .inbox,
            title: "Review AI-created item",
            notes: "AI-DRAFT-ENCRYPTION-CANARY",
            timezoneName: "Europe/Madrid",
            durationSeconds: 2_700,
            flexibleConstraints: .object([:]),
            splitPolicy: .splittable(
                minimumChunkSeconds: 900,
                maximumChunkSeconds: 2_700
            ),
            importance: 70,
            urgency: 55
        )
        return PlanningSuggestion(
            id: UUID(uuidString: "22000000-0000-4000-8000-000000000002")!,
            title: draft.title,
            summary: "Review every field before creating this Inbox item.",
            source: PlanningSuggestion.codexSource,
            createdAt: createdAt,
            expiresAt: createdAt.addingTimeInterval(7 * 24 * 60 * 60),
            state: .pending,
            payload: .canonicalItemDraft(
                PlanningSuggestionItemDraft(itemID: itemID, draft: draft)
            )
        )
    }

    private static func canonicalItem() throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {
          "id":"50000000-0000-4000-8000-000000000005","is_sensitive":true,"kind":"task","status":"planned",
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
          "id":"51000000-0000-4000-8000-000000000005","is_sensitive":false,"kind":"task","status":"planned",
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

    private static func proposalApplicationReceipt() -> DayWeaveStoredProposalApplicationReceipt {
        let appliedAt = Date(timeIntervalSince1970: 1_700_000_000)
        return DayWeaveStoredProposalApplicationReceipt(
            configurationIdentifier: "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))",
            application: DayWeaveProposalApplicationReceipt(
                applicationID: UUID(uuidString: "91000000-0000-4000-8000-000000000001")!,
                proposals: [DayWeaveProposalAppliedMember(
                    proposalID: UUID(uuidString: "92000000-0000-4000-8000-000000000002")!,
                    appliedRevision: 5
                )],
                applicationRevision: 1,
                status: .applied,
                commandIDs: [UUID(uuidString: "93000000-0000-4000-8000-000000000003")!],
                affectedItemIDs: [UUID(uuidString: "94000000-0000-4000-8000-000000000004")!],
                appliedAt: appliedAt,
                undoExpiresAt: appliedAt.addingTimeInterval(86_400),
                undoneAt: nil
            )
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

    func testOversizedEnvelopeIsRejected() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedEnvelopeIsRejected()
    }

    func testOversizedPlaintextIsRejectedOnSave() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedPlaintextIsRejectedOnSave()
    }

    func testOversizedOrdinaryPlaintextIsRejectedOnLoad() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedOrdinaryPlaintextIsRejectedOnLoad()
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

    func testSchemaFourAddsExplicitSensitivityDefaults() throws {
        try EncryptedPlannerPersistenceScenarios.schemaFourAddsExplicitSensitivityDefaults()
    }

    func testSchemaFivePrivacyMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaFiveRemainsSensitivityStrictAndAddsNoPrivacyIntent()
    }

    func testSchemaSixPrivacyAttemptMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaSixPrivacyIntentMigratesAsAmbiguouslySubmitted()
    }

    func testSchemaSevenPublicationMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaSevenAddsNoPublicationIntent()
    }

    func testProposalApplicationJournalRoundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.proposalApplicationJournalRoundTrip()
    }

    func testSchemaEightProposalApplicationMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaEightAddsNoProposalApplicationIntent()
    }

    func testCanonicalAuthoringJournalRoundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.canonicalAuthoringJournalRoundTrip()
    }

    func testSchemaTwentyTwoAuthoringDraftMigrationAddsRollbackFence() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyTwoAuthoringDraftMigrationAddsRollbackFence()
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyTwoSubmittedAuthoringKeepsLegacyRequestShape()
    }

    func testSchemaTwentyThreeCannotAcquireOccurrenceMembershipAuthority() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyThreeCannotAcquireOccurrenceMembershipAuthority()
    }

    func testSchemaNineCanonicalAuthoringMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaNineAddsNoCanonicalAuthoringIntent()
    }

    func testMalformedCanonicalAuthoringStateIsRejected() throws {
        try EncryptedPlannerPersistenceScenarios.malformedCanonicalAuthoringStateIsRejected()
    }

    func testTypedCodexSuggestionRoundTripsEncrypted() throws {
        try EncryptedPlannerPersistenceScenarios.typedCodexSuggestionRoundTripsEncrypted()
    }

    func testSchemaSeventeenCannotAcquireCodexDraftAuthority() throws {
        try EncryptedPlannerPersistenceScenarios.schemaSeventeenCannotAcquireCodexDraftAuthority()
    }

    func testSchemaTwentyOneCanonicalStructureMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaTwentyOneCanonicalStructureMigratesWithoutLoss()
    }

    func testSchemaTwentyOneForwardCapturedCanonicalStructureMigration() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyOneForwardCapturedStructureMigratesWithoutLoss()
    }

    func testSchemaTwentyOnePartialCanonicalStructureFailsClosed() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyOnePartialCanonicalStructureFailsClosed()
    }

    func testMalformedCodexSuggestionStateIsRejected() throws {
        try EncryptedPlannerPersistenceScenarios.malformedCodexSuggestionStateIsRejected()
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

    @Test("Crash-orphaned encrypted temp files are cleaned without following symlinks")
    func crashOrphanCleanup() throws {
        try EncryptedPlannerPersistenceScenarios
            .crashOrphanedTemporaryEnvelopeIsCleanedSafely()
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

    @Test("Decoded ordinary plaintext cannot consume publication-only headroom")
    func oversizedOrdinaryPlaintextLoad() throws {
        try EncryptedPlannerPersistenceScenarios.oversizedOrdinaryPlaintextIsRejectedOnLoad()
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

    @Test("Schema 4 receives explicit non-sensitive migration defaults")
    func schemaFourSensitivityMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaFourAddsExplicitSensitivityDefaults()
    }

    @Test("Schema 5 stays sensitivity-strict and migrates with no invented privacy edit")
    func schemaFivePrivacyMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaFiveRemainsSensitivityStrictAndAddsNoPrivacyIntent()
    }

    @Test("Schema 6 privacy intent migrates as ambiguously submitted")
    func schemaSixPrivacyAttemptMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaSixPrivacyIntentMigratesAsAmbiguouslySubmitted()
    }

    @Test("Schema 7 migrates without inventing a schedule publication")
    func schemaSevenPublicationMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaSevenAddsNoPublicationIntent()
    }

    @Test("Proposal application recovery state is encrypted and round-trips exactly")
    func proposalApplicationJournalRoundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.proposalApplicationJournalRoundTrip()
    }

    @Test("Schema 8 migrates without inventing proposal application state")
    func schemaEightProposalApplicationMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaEightAddsNoProposalApplicationIntent()
    }

    @Test("Canonical authoring recovery state is encrypted and round-trips exactly")
    func canonicalAuthoringJournalRoundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.canonicalAuthoringJournalRoundTrip()
    }

    @Test("Schema 22 authoring drafts migrate behind the rich-duration rollback fence")
    func schemaTwentyTwoAuthoringDraftMigration() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyTwoAuthoringDraftMigrationAddsRollbackFence()
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyTwoSubmittedAuthoringKeepsLegacyRequestShape()
    }

    @Test("Schema 23 cannot acquire v3 occurrence-membership authority")
    func schemaTwentyThreeOccurrenceMembershipMigration() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyThreeCannotAcquireOccurrenceMembershipAuthority()
    }

    @Test("Schema 9 migrates without inventing canonical authoring intent")
    func schemaNineCanonicalAuthoringMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaNineAddsNoCanonicalAuthoringIntent()
    }

    @Test("Malformed canonical authoring state fails closed")
    func malformedCanonicalAuthoringState() throws {
        try EncryptedPlannerPersistenceScenarios.malformedCanonicalAuthoringStateIsRejected()
    }

    @Test("Typed Codex item drafts are encrypted and round-trip exactly")
    func typedCodexSuggestionRoundTrip() throws {
        try EncryptedPlannerPersistenceScenarios.typedCodexSuggestionRoundTripsEncrypted()
    }

    @Test("Schema 17 prose cannot gain typed Codex draft authority")
    func schemaSeventeenCodexDraftMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaSeventeenCannotAcquireCodexDraftAuthority()
    }

    @Test("Schema 21 canonical items gain lossless typed structural metadata")
    func schemaTwentyOneCanonicalStructureMigration() throws {
        try EncryptedPlannerPersistenceScenarios.schemaTwentyOneCanonicalStructureMigratesWithoutLoss()
    }

    @Test("Schema 21 complete forward-captured structural rows remain exact and read-only")
    func schemaTwentyOneForwardCapturedCanonicalStructureMigration() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyOneForwardCapturedStructureMigratesWithoutLoss()
    }

    @Test("Schema 21 partial forward-captured structural rows fail closed")
    func schemaTwentyOnePartialCanonicalStructure() throws {
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyOnePartialCanonicalStructureFailsClosed()
    }

    @Test("Malformed Codex decision linkage fails closed")
    func malformedCodexSuggestionState() throws {
        try EncryptedPlannerPersistenceScenarios.malformedCodexSuggestionStateIsRejected()
    }

    @Test("Malformed Codex retention high-water fails closed")
    func malformedCodexHighWater() throws {
        try EncryptedPlannerPersistenceScenarios.malformedCodexHighWaterIsRejected()
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
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyTwoAuthoringDraftMigrationAddsRollbackFence()
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyTwoSubmittedAuthoringKeepsLegacyRequestShape()
        try EncryptedPlannerPersistenceScenarios.schemaTwentyOneCanonicalStructureMigratesWithoutLoss()
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyOneForwardCapturedStructureMigratesWithoutLoss()
        try EncryptedPlannerPersistenceScenarios
            .schemaTwentyOnePartialCanonicalStructureFailsClosed()
        print("All encrypted planner persistence scenarios passed")
    }
}
#endif
