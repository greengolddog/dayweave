import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@MainActor
@Suite("Sensitive item authoring")
struct SensitiveAuthoringTests {
    @Test("own and inherited sensitivity stay distinct")
    func hierarchyPresentation() throws {
        let parentID = UUID(uuidString: "a1000000-0000-4000-8000-000000000001")!
        let childID = UUID(uuidString: "a1000000-0000-4000-8000-000000000002")!
        let parent = try Self.item(
            id: parentID,
            parentID: nil,
            isSensitive: true,
            title: "SYNTHETIC-SENSITIVE-PARENT"
        )
        let child = try Self.item(
            id: childID,
            parentID: parentID,
            isSensitive: false,
            title: "SYNTHETIC-INHERITED-CHILD"
        )
        let store = PlannerStore(
            canonicalItems: [parent, child],
            restoreFromPersistence: false
        )

        #expect(store.canonicalSensitivityPresentation(itemID: parentID) == .own)
        #expect(store.canonicalSensitivityPresentation(itemID: childID) == .inherited)
        #expect(store.setCanonicalItemSensitivity(childID, isSensitive: false))
        #expect(store.pendingCanonicalSensitivityMutations.isEmpty)
        #expect(store.canonicalSensitivityPresentation(itemID: childID) == .inherited)
    }

    @Test("local captures preserve their own privacy marker through edits")
    func localCapturePrivacy() throws {
        let store = PlannerStore(restoreFromPersistence: false)
        #expect(store.quickAdd(
            title: "SYNTHETIC-LOCAL-PRIVATE-CAPTURE",
            kind: .task,
            minutes: 25,
            isSensitive: true
        ))
        let capture = try #require(store.blocks.first)
        #expect(capture.isSensitive)
        #expect(store.updateLocalCapture(
            capture.id,
            title: "SYNTHETIC-LOCAL-STANDARD-CAPTURE",
            isSensitive: false
        ))
        #expect(store.blocks.first?.isSensitive == false)
    }

    @Test("a queued privacy mark is durable intent and hardens presentation")
    func queuedMarkHardensBlock() throws {
        let itemID = UUID(uuidString: "a1000000-0000-4000-8000-000000000003")!
        let item = try Self.item(
            id: itemID,
            parentID: nil,
            isSensitive: false,
            title: "SYNTHETIC-PENDING-SENSITIVE-ITEM"
        )
        let block = ScheduleBlock(
            id: UUID(uuidString: "a1000000-0000-4000-8000-000000000004")!,
            isSensitive: false,
            title: "SYNTHETIC-PENDING-SENSITIVE-ITEM",
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_981_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: 1,
            syncOrigin: .canonicalPreview,
            previewKind: "planned"
        )
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [item],
            restoreFromPersistence: false
        )

        #expect(store.setCanonicalItemSensitivity(itemID, isSensitive: true))
        let mutation = try #require(store.pendingCanonicalSensitivityMutations.first)
        #expect(mutation.itemID == itemID)
        #expect(mutation.baseRevision == 1)
        #expect(mutation.desiredIsSensitive)
        #expect(mutation.disposition == .pending)
        #expect(store.blocks.first?.isSensitive == true)

        #expect(store.setCanonicalItemSensitivity(itemID, isSensitive: false))
        #expect(store.pendingCanonicalSensitivityMutations.isEmpty)
        // A canceled local mark does not eagerly declassify a rendered block;
        // only a validated preview may lower an existing privacy boundary.
        #expect(store.blocks.first?.isSensitive == true)
    }

    @Test("queued full-item privacy and reparenting harden blocks and Codex context immediately")
    func queuedAuthoringHardensPrivacyBoundary() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let sensitiveParentID = UUID(uuidString: "a1000000-0000-4000-8000-000000000010")!
        let ownMarkID = UUID(uuidString: "a1000000-0000-4000-8000-000000000011")!
        let movedChildID = UUID(uuidString: "a1000000-0000-4000-8000-000000000012")!
        let publicID = UUID(uuidString: "a1000000-0000-4000-8000-000000000013")!
        let parent = try Self.item(
            id: sensitiveParentID,
            parentID: nil,
            isSensitive: true,
            title: "SYNTHETIC-PRIVATE-PARENT"
        )
        let ownMark = try Self.item(
            id: ownMarkID,
            parentID: nil,
            isSensitive: false,
            title: "SYNTHETIC-QUEUED-OWN-PRIVATE"
        )
        let movedChild = try Self.item(
            id: movedChildID,
            parentID: nil,
            isSensitive: false,
            title: "SYNTHETIC-QUEUED-INHERITED-PRIVATE"
        )
        let publicItem = try Self.item(
            id: publicID,
            parentID: nil,
            isSensitive: false,
            title: "SYNTHETIC-STILL-PUBLIC"
        )
        let start = Date(timeIntervalSince1970: 1_787_980_000)
        let blocks = [
            Self.block(item: ownMark, start: start),
            Self.block(item: movedChild, start: start.addingTimeInterval(2_000)),
        ]
        let store = PlannerStore(
            blocks: blocks,
            canonicalItems: [parent, ownMark, movedChild, publicItem],
            persistence: context.persistence,
            restoreFromPersistence: false
        )

        var ownDraft = DayWeaveCanonicalItemDraft(item: ownMark)
        ownDraft.isSensitive = true
        _ = try store.enqueueCanonicalReplace(itemID: ownMarkID, draft: ownDraft)
        var movedDraft = DayWeaveCanonicalItemDraft(item: movedChild)
        movedDraft.parentID = sensitiveParentID
        _ = try store.enqueueCanonicalReplace(itemID: movedChildID, draft: movedDraft)

        #expect(store.blocks.allSatisfy { $0.isSensitive })
        #expect(store.canonicalSensitivityPresentation(itemID: ownMarkID) == .own)
        #expect(store.canonicalSensitivityPresentation(itemID: movedChildID) == .inherited)
        let snapshot = store.codexPlannerContextSnapshot()
        #expect(snapshot.scheduledBlocks.isEmpty)
        #expect(snapshot.privateBusySpans.count == 2)
        #expect(snapshot.plannerItems.map(\.title) == ["SYNTHETIC-STILL-PUBLIC"])
    }

    @Test("a sensitive deleted restore body protects a different public active revision")
    func activeRestoreConflictKeepsRetainedPrivacyBoundary() throws {
        let context = try Self.makePersistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let itemID = UUID()
        let deleted = try Self.item(
            id: itemID,
            parentID: nil,
            isSensitive: true,
            title: "SYNTHETIC-PRIVATE-DELETED-VERSION",
            notes: "SYNTHETIC-PRIVATE-RESTORE-NOTES",
            revision: 2,
            deleted: true
        )
        let active = try Self.item(
            id: itemID,
            parentID: nil,
            isSensitive: false,
            title: "SYNTHETIC-PUBLIC-ACTIVE-VERSION",
            revision: 3
        )
        let store = PlannerStore(
            canonicalTombstoneRevisions: [itemID: 2],
            canonicalTrash: [.init(item: deleted)],
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Date(timeIntervalSince1970: 1_787_980_000) }
        )
        let restore = try store.enqueueCanonicalRestore(itemID: itemID)

        store.applyCanonicalDelta([.upsert(active)], nextCursor: "restore-conflict")

        #expect(store.canonicalAuthoringMutation(id: restore.id)?.disposition == .conflicted)
        #expect(store.canonicalSensitivityPresentation(itemID: itemID) == .own)
        #expect(store.canonicalItemRequiresSensitivePresentation(itemID: itemID))
        #expect(!store.codexPlannerContextSnapshot().plannerItems.contains {
            $0.title == active.title
        })
    }

    private static func block(
        item: DayWeaveCanonicalItem,
        start: Date
    ) -> ScheduleBlock {
        ScheduleBlock(
            id: UUID(),
            isSensitive: false,
            title: item.title,
            kind: .task,
            start: start,
            end: start.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: item.id,
            sourceItemRevision: item.revision,
            syncOrigin: .canonicalPreview,
            previewKind: "planned"
        )
    }

    private static func makePersistence() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveSensitiveAuthoringTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let key = try PlannerEncryptionKey(data: Data(repeating: 29, count: 32))
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: key
            )
        )
    }

    private static func item(
        id: UUID,
        parentID: UUID?,
        isSensitive: Bool,
        title: String,
        notes: String? = nil,
        revision: UInt64 = 1,
        deleted: Bool = false
    ) throws -> DayWeaveCanonicalItem {
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        let encodedNotes = notes.map { "\"\($0)\"" } ?? "null"
        let deletedAt = deleted ? "\"2026-08-29T08:00:00Z\"" : "null"
        let json = """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":\(isSensitive),
         "kind":"task","status":"planned","title":"\(title)","notes":\(encodedNotes),
         "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,
         "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
         "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
         "parent_id":\(parent),"sibling_order":0,"is_executable":true,
         "revision":\(revision),"created_at":"2026-08-29T08:00:00Z",
         "updated_at":"2026-08-29T08:00:00Z","completed_at":null,"deleted_at":\(deletedAt)}
        """
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }
}
#endif
