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

    private static func item(
        id: UUID,
        parentID: UUID?,
        isSensitive: Bool,
        title: String
    ) throws -> DayWeaveCanonicalItem {
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        let json = """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":\(isSensitive),
         "kind":"task","status":"planned","title":"\(title)","notes":null,
         "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,
         "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
         "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
         "parent_id":\(parent),"sibling_order":0,"is_executable":true,
         "revision":1,"created_at":"2026-08-29T08:00:00Z",
         "updated_at":"2026-08-29T08:00:00Z","completed_at":null,"deleted_at":null}
        """
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }
}
#endif
