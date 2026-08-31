import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Durable canonical delta application", .serialized)
@MainActor
struct PlannerCanonicalDeltaCommitTests {
    @Test("cursor-only own echo commits without invalidating an exact preview")
    func ownEchoPreservesPreview() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let item = try Self.item(id: UUID(), revision: 1)
        let configuration = "https://api.example.com"
        let planner = PlannerStore(
            canonicalItems: [item],
            canonicalDeltaCursor: "cursor-before",
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: .init(
                configurationIdentifier: configuration,
                generatedAt: now,
                asOf: now,
                horizonStart: now.addingTimeInterval(-3_600),
                horizonEnd: now.addingTimeInterval(86_400),
                timezoneName: "UTC"
            ),
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        planner.flushPersistence()
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
        #expect(planner.beginCanonicalSync())
        defer { planner.endCanonicalSync() }

        let result = try planner.applyCanonicalDeltaDurably(
            [.upsert(item)],
            nextCursor: "cursor-after"
        )

        #expect(!result.schedulingInputsChanged)
        #expect(result.cursorChanged)
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
        #expect(try context.persistence.load()?.canonicalDeltaCursor == "cursor-after")
    }

    @Test("recurrence pruning is detected and committed only after invalidation")
    func recurrencePruningIsSchedulingChange() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let missingItemID = UUID()
        let occurrenceID = UUID()
        let configuration = "https://api.example.com"
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configuration,
            generatedAt: now,
            asOf: now,
            horizonStart: now.addingTimeInterval(-3_600),
            horizonEnd: now.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let revisionID = UUID()
        let proof = DayWeavePublishedScheduleProof(
            configurationIdentifier: configuration,
            revisionID: revisionID,
            revision: "1:\(revisionID.uuidString.lowercased())",
            revisionNumber: 1,
            inputDigest: "sha256:\(String(repeating: "a", count: 64))",
            asOf: provenance.asOf,
            horizonStart: provenance.horizonStart,
            horizonEnd: provenance.horizonEnd,
            timezoneName: provenance.timezoneName,
            publishedAt: now,
            publishedBlocks: []
        )
        let block = ScheduleBlock(
            id: UUID(),
            title: "Orphaned recurring session",
            kind: .habit,
            start: now,
            end: now.addingTimeInterval(900),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .low,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: missingItemID,
            sourceItemRevision: 1,
            occurrenceID: occurrenceID,
            sessionIndex: 0,
            syncOrigin: .local,
            previewKind: "planned"
        )
        let planner = PlannerStore(
            blocks: [block],
            canonicalDeltaCursor: "cursor-before",
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: provenance,
            publishedScheduleProof: proof,
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        planner.complete(block.id)
        #expect(!planner.recurrenceSessionOutcomes.isEmpty)
        planner.flushPersistence()
        #expect(try context.persistence.load()?.publishedScheduleProof == proof)
        #expect(planner.beginCanonicalSync())
        defer { planner.endCanonicalSync() }

        let result = try planner.applyCanonicalDeltaDurably(
            [],
            nextCursor: "cursor-after"
        )

        #expect(result.schedulingInputsChanged)
        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.recurrenceSessionOutcomes.isEmpty)
        #expect(planner.completedOccurrenceIDs.isEmpty)
        let loaded = try context.persistence.load()
        let restored = try #require(loaded)
        #expect(restored.canonicalDeltaCursor == "cursor-after")
        #expect(restored.publishedScheduleProof == nil)
        #expect(restored.recurrenceSessionOutcomes?.isEmpty == true)
        #expect(restored.completedOccurrenceIDs?.isEmpty == true)
    }

    @Test("failed durable delta save restores the exact in-memory preimage")
    func deltaSaveFailureRollsBack() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let itemID = UUID()
        let original = try Self.item(id: itemID, revision: 1)
        let replacement = try Self.item(id: itemID, revision: 2)
        let configuration = "https://api.example.com"
        let evidence = Self.publicationEvidence(now: now, configuration: configuration)
        let planner = PlannerStore(
            canonicalItems: [original],
            canonicalDeltaCursor: "cursor-before",
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: evidence.provenance,
            publishedScheduleProof: evidence.proof,
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        planner.flushPersistence()
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
        #expect(planner.beginCanonicalSync())
        defer { planner.endCanonicalSync() }
        try FileManager.default.removeItem(at: context.directory)

        #expect(throws: (any Error).self) {
            try planner.applyCanonicalDeltaDurably(
                [.upsert(replacement)],
                nextCursor: "cursor-after"
            )
        }

        #expect(planner.canonicalItems == [original])
        #expect(planner.canonicalDeltaCursor == "cursor-before")
        #expect(planner.schedulePreviewProvenance == evidence.provenance)
        #expect(planner.publishedScheduleProof == evidence.proof)
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
        #expect(!planner.canPersistPlan)
        #expect(planner.persistenceError != nil)
    }

    @Test("failed durable cursor-scope rebuild restores selection and canonical state")
    func rebuildSaveFailureRollsBack() throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let original = try Self.item(id: UUID(), revision: 1)
        let replacement = try Self.item(id: UUID(), revision: 1)
        let configuration = "https://api.example.com"
        let evidence = Self.publicationEvidence(now: now, configuration: configuration)
        let planner = PlannerStore(
            canonicalItems: [original],
            canonicalDeltaCursor: "cursor-before",
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: evidence.provenance,
            publishedScheduleProof: evidence.proof,
            selectedCanonicalItemID: original.id,
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { now }
        )
        planner.flushPersistence()
        #expect(planner.beginCanonicalSync())
        defer { planner.endCanonicalSync() }
        try FileManager.default.removeItem(at: context.directory)

        #expect(throws: (any Error).self) {
            try planner.replaceCanonicalStateDurably(
                changes: [.upsert(replacement)],
                nextCursor: "cursor-rebuilt"
            )
        }

        #expect(planner.canonicalItems == [original])
        #expect(planner.canonicalDeltaCursor == "cursor-before")
        #expect(planner.selectedCanonicalItemID == original.id)
        #expect(planner.schedulePreviewProvenance == evidence.provenance)
        #expect(planner.publishedScheduleProof == evidence.proof)
        #expect(planner.canonicalPreviewFreshnessIssue == nil)
        #expect(!planner.canPersistPlan)
    }

    private static func persistenceContext() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveCanonicalDeltaCommitTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let key = try PlannerEncryptionKey(data: Data(repeating: 83, count: 32))
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: key
            )
        )
    }

    private static func item(id: UUID, revision: UInt64) throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {"id":"\#(id.uuidString.lowercased())","is_sensitive":false,
         "kind":"task","status":"scheduled","title":"Canonical work",
         "notes":null,"timezone_name":"UTC","duration_seconds":1800,
         "deadline_at":null,"earliest_start_at":null,"recurrence":null,
         "flexible_constraints":{},"split_policy":{"type":"indivisible"},
         "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
         "is_executable":true,"revision":\#(revision),
         "created_at":"2027-01-15T10:00:00Z","updated_at":"2027-01-15T10:00:00Z",
         "completed_at":null,"deleted_at":null}
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func publicationEvidence(
        now: Date,
        configuration: String
    ) -> (
        provenance: SchedulePreviewProvenance,
        proof: DayWeavePublishedScheduleProof
    ) {
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configuration,
            generatedAt: now,
            asOf: now,
            horizonStart: now.addingTimeInterval(-3_600),
            horizonEnd: now.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let revisionID = UUID()
        let proof = DayWeavePublishedScheduleProof(
            configurationIdentifier: configuration,
            revisionID: revisionID,
            revision: "1:\(revisionID.uuidString.lowercased())",
            revisionNumber: 1,
            inputDigest: "sha256:\(String(repeating: "b", count: 64))",
            asOf: provenance.asOf,
            horizonStart: provenance.horizonStart,
            horizonEnd: provenance.horizonEnd,
            timezoneName: provenance.timezoneName,
            publishedAt: now,
            publishedBlocks: []
        )
        return (provenance, proof)
    }
}
#endif
