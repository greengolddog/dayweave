import CryptoKit
import Foundation
import Testing
@testable import DayWeaveMac

@Suite("Encrypted Google outbound recovery", .serialized)
@MainActor
struct GoogleOutboundPlannerPersistenceTests {
    @Test("approval capability round-trips only inside encrypted planner state")
    func approvedRecoveryIsEncryptedAndRoundTrips() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { Self.now }
        )
        let intent = try makeIntent()
        let previewed = try intent.recording(preview: makePreview())
        let approvalAttempted = try previewed.recordingApprovalAttempt()
        let capability = makeCapability()
        let approved = try approvalAttempted.recording(approval: GoogleOutboundApproval(
            previewID: try #require(previewed.preview?.id),
            approvalCapability: capability,
            expiresAt: Self.now.addingTimeInterval(8 * 60)
        ))

        try store.saveGoogleOutboundRecoveryJournal(intent)
        try store.saveGoogleOutboundRecoveryJournal(previewed)
        try store.saveGoogleOutboundRecoveryJournal(approvalAttempted)
        try store.saveGoogleOutboundRecoveryJournal(approved)

        let envelope = try Data(contentsOf: context.fileURL)
        #expect(envelope.range(of: Data(capability.utf8)) == nil)

        let restored = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60),
            now: { Self.now }
        )
        #expect(try restored.loadGoogleOutboundRecoveryJournal() == approved)
        #expect(!restored.beginCanonicalSync())

        try restored.clearGoogleOutboundRecoveryJournal(approved)
        let cleared = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60),
            now: { Self.now }
        )
        #expect(try cleared.loadGoogleOutboundRecoveryJournal() == nil)
        #expect(cleared.beginCanonicalSync())
        cleared.endCanonicalSync()
    }

    @Test("only exact monotonic recovery transitions may replace the journal")
    func recoveryTransitionsFailClosed() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { Self.now }
        )
        let intent = try makeIntent()
        try store.saveGoogleOutboundRecoveryJournal(intent)

        let unrelated = try GoogleOutboundRecoveryJournal(
            operationGeneration: intent.operationGeneration + 1,
            configurationIdentifier: intent.configurationIdentifier,
            accountID: intent.accountID,
            collectionID: intent.collectionID,
            itemID: UUID(),
            expectedItemRevision: intent.expectedItemRevision,
            entityKind: intent.entityKind,
            operation: intent.operation,
            intentExpiresAt: intent.intentExpiresAt,
            createdAt: intent.createdAt
        )
        #expect(throws: PlannerGoogleOutboundRecoveryError.journalConflict) {
            try store.saveGoogleOutboundRecoveryJournal(unrelated)
        }
        #expect(throws: PlannerGoogleOutboundRecoveryError.journalConflict) {
            try store.clearGoogleOutboundRecoveryJournal(unrelated)
        }

        let changedEntityPreview = GoogleOutboundPreview(
            id: Self.previewID,
            accountID: intent.accountID,
            collectionID: intent.collectionID,
            collectionRevision: 2,
            collectionDisplayName: "Calendar",
            itemID: intent.itemID,
            itemRevision: intent.expectedItemRevision,
            entityKind: .calendarEvent,
            operation: intent.operation,
            providerResourceID: nil,
            providerETag: nil,
            previewHash: String(repeating: "b", count: 64),
            providerPayload: ["summary": .string("Private event")],
            expiresAt: Self.now.addingTimeInterval(10 * 60)
        )
        let changedEntity = try GoogleOutboundRecoveryJournal(
            recoveryID: intent.recoveryID,
            operationGeneration: intent.operationGeneration,
            configurationIdentifier: intent.configurationIdentifier,
            accountID: intent.accountID,
            collectionID: intent.collectionID,
            itemID: intent.itemID,
            expectedItemRevision: intent.expectedItemRevision,
            entityKind: .calendarEvent,
            operation: intent.operation,
            intentExpiresAt: intent.intentExpiresAt,
            preview: changedEntityPreview,
            createdAt: intent.createdAt
        )
        #expect(throws: PlannerGoogleOutboundRecoveryError.journalConflict) {
            try store.saveGoogleOutboundRecoveryJournal(changedEntity)
        }
        #expect(try store.loadGoogleOutboundRecoveryJournal() == intent)
    }

    @Test("schema ten migration invents no Google publication authority")
    func schemaTenMigrationAddsNoRecovery() throws {
        let legacy = PlannerSnapshot(
            schemaVersion: 10,
            savedAt: Self.now,
            destination: .today,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "legacy",
            protectedFreeMinutes: 90,
            freezeHours: 2,
            showCompleted: true
        )

        let migrated = try legacy.migratedToCurrentSchema()
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion)
        #expect(migrated.googleOutboundRecoveryJournal == nil)
    }

    @Test("schema 19 rewrites an exact legacy calendar journal as entity-bound v2")
    func schemaNineteenLegacyCalendarJournalIsDurablyRewritten() throws {
        let context = try makeContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let keyData = Data((0..<32).map(UInt8.init))
        let intent = try GoogleOutboundRecoveryJournal(
            recoveryID: UUID(uuidString: "75000000-0000-4000-8000-000000000005")!,
            operationGeneration: 7,
            configurationIdentifier: "google-outbound-schema-19-test",
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 4,
            entityKind: .calendarEvent,
            operation: .upsert,
            intentExpiresAt: Self.now.addingTimeInterval(30 * 60),
            createdAt: Self.now
        )
        let previewed = try intent.recording(preview: GoogleOutboundPreview(
            id: Self.previewID,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            collectionRevision: 2,
            collectionDisplayName: "Primary",
            itemID: Self.itemID,
            itemRevision: 4,
            entityKind: .calendarEvent,
            operation: .upsert,
            providerResourceID: nil,
            providerETag: nil,
            previewHash: String(repeating: "c", count: 64),
            providerPayload: ["summary": .string("Private event")],
            expiresAt: Self.now.addingTimeInterval(10 * 60)
        ))
        let approvalAttempted = try previewed.recordingApprovalAttempt()
        let journal = try approvalAttempted.recording(approval: GoogleOutboundApproval(
            previewID: Self.previewID,
            approvalCapability: makeCapability(),
            expiresAt: Self.now.addingTimeInterval(8 * 60)
        ))
        let expected = PlannerSnapshot(
            savedAt: Self.now,
            destination: .today,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "schema 19 legacy Google recovery",
            protectedFreeMinutes: 90,
            scheduleProfile: try ScheduleProfile.legacyDefault(
                timezoneName: "UTC",
                protectedFreeMinutes: 90
            ),
            freezeHours: 2,
            showCompleted: true,
            googleOutboundRecoveryJournal: journal
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        let expectedPlaintext = try encoder.encode(expected)
        var legacyObject = try #require(
            JSONSerialization.jsonObject(with: expectedPlaintext) as? [String: Any]
        )
        legacyObject["schemaVersion"] = 19
        var legacyJournal = try #require(
            legacyObject["googleOutboundRecoveryJournal"] as? [String: Any]
        )
        legacyJournal["version"] = 1
        legacyJournal.removeValue(forKey: "entity_kind")
        #expect(Set(legacyJournal.keys) == Set([
            "version",
            "recovery_id",
            "operation_generation",
            "configuration_identifier",
            "account_id",
            "collection_id",
            "item_id",
            "expected_item_revision",
            "operation",
            "intent_expires_at",
            "preview",
            "approval_attempted",
            "approval_capability",
            "approval_expires_at",
            "created_at",
        ]))
        legacyObject["googleOutboundRecoveryJournal"] = legacyJournal
        let legacyPlaintext = try JSONSerialization.data(
            withJSONObject: legacyObject,
            options: [.sortedKeys]
        )
        let legacySealed = try AES.GCM.seal(
            legacyPlaintext,
            using: SymmetricKey(data: keyData),
            authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8)
        )
        let legacyCombined = try #require(legacySealed.combined)
        let legacyEnvelope: [String: Any] = [
            "magic": "DAYWEAVE-ENCRYPTED-SNAPSHOT",
            "formatVersion": 1,
            "cipher": "AES.GCM.256",
            "sealedSnapshot": legacyCombined.base64EncodedString(),
        ]
        let legacyEnvelopeData = try JSONSerialization.data(
            withJSONObject: legacyEnvelope,
            options: [.sortedKeys]
        )
        try legacyEnvelopeData.write(to: context.fileURL, options: .atomic)

        let loaded = try context.persistence.load()
        let migrated = try #require(loaded)
        #expect(migrated == expected)
        let rewrittenEnvelopeData = try Data(contentsOf: context.fileURL)
        #expect(rewrittenEnvelopeData != legacyEnvelopeData)
        let rewrittenEnvelope = try #require(
            JSONSerialization.jsonObject(with: rewrittenEnvelopeData) as? [String: Any]
        )
        let rewrittenCiphertext = try #require(
            rewrittenEnvelope["sealedSnapshot"] as? String
        )
        let rewrittenCombined = try #require(Data(base64Encoded: rewrittenCiphertext))
        let rewrittenPlaintext = try AES.GCM.open(
            AES.GCM.SealedBox(combined: rewrittenCombined),
            using: SymmetricKey(data: keyData),
            authenticating: Data("DayWeave.PlannerSnapshot|1|AES.GCM.256".utf8)
        )
        #expect(rewrittenPlaintext == expectedPlaintext)
        let rewrittenObject = try #require(
            JSONSerialization.jsonObject(with: rewrittenPlaintext) as? [String: Any]
        )
        #expect(rewrittenObject["schemaVersion"] as? Int == 22)
        let rewrittenJournal = try #require(
            rewrittenObject["googleOutboundRecoveryJournal"] as? [String: Any]
        )
        #expect(rewrittenJournal["version"] as? Int == 2)
        #expect(rewrittenJournal["entity_kind"] as? String == "calendar_event")
    }

    nonisolated private static let now = Date(timeIntervalSince1970: 1_788_067_200)
    nonisolated private static let accountID = UUID(
        uuidString: "71000000-0000-4000-8000-000000000001"
    )!
    nonisolated private static let collectionID = UUID(
        uuidString: "72000000-0000-4000-8000-000000000002"
    )!
    nonisolated private static let itemID = UUID(
        uuidString: "73000000-0000-4000-8000-000000000003"
    )!
    nonisolated private static let previewID = UUID(
        uuidString: "74000000-0000-4000-8000-000000000004"
    )!

    private func makeContext() throws -> (
        directory: URL,
        fileURL: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveGoogleOutboundPersistence-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data((0..<32).map(UInt8.init)))
        return (directory, fileURL, EncryptedPlannerPersistence(fileURL: fileURL, key: key))
    }

    private func makeIntent() throws -> GoogleOutboundRecoveryJournal {
        try GoogleOutboundRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: "google-outbound-persistence-test",
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 4,
            entityKind: .task,
            operation: .upsert,
            intentExpiresAt: Self.now.addingTimeInterval(30 * 60),
            createdAt: Self.now
        )
    }

    private func makePreview() -> GoogleOutboundPreview {
        GoogleOutboundPreview(
            id: Self.previewID,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            collectionRevision: 2,
            collectionDisplayName: "Personal tasks",
            itemID: Self.itemID,
            itemRevision: 4,
            entityKind: .task,
            operation: .upsert,
            providerResourceID: nil,
            providerETag: nil,
            previewHash: String(repeating: "a", count: 64),
            providerPayload: [
                "id": .string(""),
                "etag": .null,
                "title": .string("Private task"),
                "notes": .string("Private notes"),
                "status": .string("needsAction"),
                "due": .null,
                "completed": .null,
                "updated": .null,
                "parent": .null,
                "position": .null,
                "links": .null,
                "deleted": .bool(false),
                "hidden": .bool(false),
            ],
            expiresAt: Self.now.addingTimeInterval(10 * 60)
        )
    }

    private func makeCapability() -> String {
        let payload = Data(repeating: 7, count: 32).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return "dw_ga1_\(payload)"
    }
}
