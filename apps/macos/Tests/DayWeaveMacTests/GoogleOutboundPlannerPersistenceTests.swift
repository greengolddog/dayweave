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
            collectionDisplayName: "Primary",
            itemID: Self.itemID,
            itemRevision: 4,
            entityKind: .calendarEvent,
            operation: .upsert,
            providerResourceID: nil,
            providerETag: nil,
            previewHash: String(repeating: "a", count: 64),
            providerPayload: ["summary": .string("Private event")],
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
