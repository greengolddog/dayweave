import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@MainActor
@Suite("Canonical sensitivity batch projection")
struct CanonicalSensitivityIndexTests {
    @Test("batch presentation agrees with the existing resolver for public and inherited marks")
    func ordinaryHierarchy() throws {
        let items = try [
            item(1), item(2, parent: 1), item(3, sensitive: true),
            item(4, parent: 3), item(5, parent: 4, sensitive: true)
        ]
        let store = makeStore(items: items)
        let index = assertParity(store, ids: (1...6).map(id))
        #expect(index[id(1)] == .standard)
        #expect(index[id(2)] == .standard)
        #expect(index[id(3)] == .own)
        #expect(index[id(4)] == .inherited)
        #expect(index[id(5)] == .own)
        #expect(index[id(6)] == .inherited)
    }

    @Test("old and proposed parent paths remain protective including a shared-ancestor diamond")
    func pendingParentPaths() throws {
        let items = try [item(1, sensitive: true), item(2), item(3, parent: 1),
                         item(4, parent: 2), item(5, parent: 3), item(6, parent: 4),
                         item(7), item(8, parent: 7), item(9, parent: 7), item(10, parent: 8)]
        let moves = [(3, 2), (4, 1), (10, 9)].map { child, parent in
            let base = items.first { $0.id == id(child) }!
            var draft = DayWeaveCanonicalItemDraft(item: base)
            draft.parentID = id(parent)
            return DayWeavePendingCanonicalAuthoringMutation(
                itemID: base.id, operation: .replace, draft: draft,
                expectedRevision: base.revision, baseItem: base
            )
        }
        let index = assertParity(makeStore(items: items, authoring: moves), ids: (1...10).map(id))
        for child in 3...6 { #expect(index[id(child)] == .inherited) }
        #expect(index[id(10)] == .standard)
    }

    @Test("queued marks and ambiguous follow-ups harden while queued removal never declassifies")
    func pendingOwnMarks() throws {
        let items = try [item(1, sensitive: true), item(2), item(3), item(4), item(5, parent: 4)]
        let marks = [(1, false, nil as Bool?), (2, true, false), (3, false, true)].map { number, desired, followUp in
            PendingCanonicalSensitivityMutation(
                id: id(100 + number), itemID: id(number), desiredIsSensitive: desired,
                baseRevision: 1, createdAt: Self.now, disposition: .pending,
                diagnostic: nil, hasBeenSubmitted: true, followUpIsSensitive: followUp
            )
        }
        var draft = DayWeaveCanonicalItemDraft(item: items[3])
        draft.isSensitive = true
        let authoring = DayWeavePendingCanonicalAuthoringMutation(
            itemID: id(4), operation: .replace, draft: draft,
            expectedRevision: 1, baseItem: items[3]
        )
        let index = assertParity(
            makeStore(items: items, marks: marks, authoring: [authoring]), ids: (1...5).map(id)
        )
        for number in 1...4 { #expect(index[id(number)] == .own) }
        #expect(index[id(5)] == .inherited)
    }

    @Test("missing parents self cycles longer cycles and their descendants fail closed")
    func malformedAncestry() throws {
        let items = try [item(1, parent: 99), item(2, parent: 1), item(3, parent: 3),
                         item(4, parent: 3), item(5, parent: 6), item(6, parent: 5),
                         item(7, parent: 6), item(8, parent: 7, sensitive: true), item(9)]
        let index = assertParity(makeStore(items: items), ids: (1...9).map(id))
        for number in 1...7 { #expect(index[id(number)] == .inherited) }
        #expect(index[id(8)] == .own)
        #expect(index[id(9)] == .standard)
    }

    @Test("retained trash and restore bodies do not make unknown ancestry public")
    func retainedBodies() throws {
        let privateBody = try item(1, sensitive: true)
        let publicBody = try item(2)
        let inheritedBody = try item(3, parent: 99)
        let trash = [DayWeaveCanonicalTrashEntry(item: privateBody), .init(item: publicBody),
                     .init(id: id(4), revision: 1, deletedAt: Self.now, parentID: nil, lastKnownItem: nil)]
        let authoring: [DayWeavePendingCanonicalAuthoringMutation] = [
            .init(itemID: id(1), operation: .restore, expectedRevision: 1, baseItem: privateBody),
            .init(itemID: id(3), operation: .restore, expectedRevision: 1, baseItem: inheritedBody),
            .init(itemID: id(5), operation: .restore, expectedRevision: 1)
        ]
        let index = assertParity(
            makeStore(items: [], authoring: authoring, trash: trash), ids: (1...5).map(id)
        )
        #expect(index[id(1)] == .own)
        #expect(index[id(2)] == .inherited)
        #expect(index[id(3)] == .inherited)
        #expect(index[id(4)] == .own)
        #expect(index[id(5)] == .inherited)
    }

    @Test("generated graph overlays match the independent per-item traversal")
    func generatedParity() throws {
        var seed: UInt64 = 17
        func next(_ limit: Int) -> Int {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1
            return Int((seed >> 32) % UInt64(limit))
        }
        for _ in 0..<32 {
            let items = try (1...32).map { number in
                let parent = next(36)
                return try item(number, parent: parent == 0 ? nil : parent, sensitive: next(9) == 0)
            }
            let mutations = (1...8).map { number in
                let base = items[number - 1]
                var draft = DayWeaveCanonicalItemDraft(item: base)
                let parent = next(36)
                draft.parentID = parent == 0 ? nil : id(parent)
                draft.isSensitive = next(7) == 0
                return DayWeavePendingCanonicalAuthoringMutation(
                    itemID: base.id, operation: .replace, draft: draft,
                    expectedRevision: 1, baseItem: base
                )
            }
            _ = assertParity(makeStore(items: items, authoring: mutations), ids: (1...36).map(id))
        }
    }

    @Test("ten thousand canonical levels resolve iteratively in one batch")
    func deepCanonicalGraph() throws {
        for sensitiveRoot in [false, true] {
            let items = try (1...10_000).map { number in
                try item(number, parent: number == 1 ? nil : number - 1,
                         sensitive: number == 1 && sensitiveRoot)
            }
            let index = CanonicalSensitivityIndex(
                canonicalItems: items, sensitivityMutations: [], authoringMutations: [], trashEntries: []
            )
            #expect(index[id(1)] == (sensitiveRoot ? .own : .standard))
            #expect(index[id(10_000)] == (sensitiveRoot ? .inherited : .standard))
        }
    }

    private static let now = Date(timeIntervalSince1970: 1_787_980_000)

    private func makeStore(
        items: [DayWeaveCanonicalItem], marks: [PendingCanonicalSensitivityMutation] = [],
        authoring: [DayWeavePendingCanonicalAuthoringMutation] = [], trash: [DayWeaveCanonicalTrashEntry] = []
    ) -> PlannerStore {
        let snapshotNow = Self.now
        return PlannerStore(canonicalItems: items, pendingCanonicalSensitivityMutations: marks,
                            pendingCanonicalAuthoringMutations: authoring, canonicalTrash: trash,
                            restoreFromPersistence: false, now: { snapshotNow })
    }

    @discardableResult
    private func assertParity(_ store: PlannerStore, ids: [UUID]) -> CanonicalSensitivityIndex {
        let index = store.canonicalSensitivityPresentationIndex()
        for itemID in ids {
            #expect(index[itemID] == store.canonicalSensitivityPresentation(itemID: itemID))
        }
        return index
    }

    private func id(_ number: Int) -> UUID {
        UUID(uuidString: "a3000000-0000-4000-8000-\(String(format: "%012d", number))")!
    }

    private func item(_ number: Int, parent: Int? = nil, sensitive: Bool = false) throws -> DayWeaveCanonicalItem {
        let encodedParent = parent.map { "\"\(id($0).uuidString)\"" } ?? "null"
        let json = """
        {"id":"\(id(number).uuidString)","is_sensitive":\(sensitive),
         "kind":"task","status":"planned","title":"Synthetic item \(number)","notes":null,
         "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,
         "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
         "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
         "parent_id":\(encodedParent),"sibling_order":0,"is_executable":true,
         "revision":1,"created_at":"2026-08-29T08:00:00Z",
         "updated_at":"2026-08-29T08:00:00Z","completed_at":null,"deleted_at":null}
        """
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(json.utf8))
    }
}
#endif
