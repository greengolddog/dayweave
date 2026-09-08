import AppKit
import Foundation
import SwiftUI
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical hierarchy browser")
struct CanonicalHierarchyPresentationTests {
    @Test("both platforms consume the same hierarchy projection cases")
    func sharedProjectionContract() throws {
        let fixture = try Self.fixture()
        let items = try fixture.nodes.map { try Self.item($0) }
        for input in [items, Array(items.reversed())] {
            let rows = CanonicalInboxPresentation.build(
                activeItems: input, pendingMutations: [], trashEntries: []
            ).hierarchyRows
            for testCase in fixture.cases {
                let result = CanonicalHierarchyPresentation.build(
                    rows: Array(rows.reversed()), scope: testCase.scope,
                    query: testCase.query, collapsedIDs: Set(testCase.collapsed.map(Self.id))
                )
                #expect(result.entries.map(\.id) == testCase.visible.map(Self.id), "\(testCase.name)")
                #expect(result.entries.map(\.depth) == testCase.depths, "\(testCase.name)")
                #expect(result.entries.filter(\.isContext).map(\.id) == testCase.scopeContext.map(Self.id))
                #expect(result.entries.filter(\.isSearchMatch).map(\.id) == testCase.searchMatches.map(Self.id))
            }
        }
    }

    @Test("five thousand levels retain exact depth without recursive traversal")
    func deepHierarchy() throws {
        let count = 5_000
        let pending = (1...count).map { index in
            DayWeavePendingCanonicalAuthoringMutation(
                itemID: Self.id(index), operation: .create,
                draft: DayWeaveCanonicalItemDraft(
                    kind: index == 1 ? .goal : .task, title: "Level \(index)", timezoneName: "UTC",
                    parentID: index == 1 ? nil : Self.id(index - 1)
                )
            )
        }
        let rows = CanonicalInboxPresentation.build(
            activeItems: [], pendingMutations: Array(pending.reversed()), trashEntries: []
        ).hierarchyRows
        let expanded = CanonicalHierarchyPresentation.build(rows: rows, scope: .goals)
        #expect(expanded.entries.count == count)
        #expect(expanded.entries.last?.depth == count - 1)
        #expect(expanded.entries.last?.breadcrumb.count == CanonicalInboxPresentation.maximumBreadcrumbDepth)
        let collapsed = Set([Self.id(1)])
        #expect(CanonicalHierarchyPresentation.build(
            rows: rows, scope: .goals, collapsedIDs: collapsed
        ).entries.count == 1)
        let searched = CanonicalHierarchyPresentation.build(
            rows: rows, scope: .goals, query: "Level 5000", collapsedIDs: collapsed
        )
        #expect(searched.entries.count == count)
        #expect(searched.entries.filter(\.isSearchMatch).map(\.id) == [Self.id(count)])
        #expect(!searched.entries.contains { $0.isCollapsed })
        #expect(CanonicalHierarchyPresentation.build(
            rows: rows, scope: .goals, query: " ", collapsedIDs: collapsed
        ).entries.count == 1)
    }

    @Test("titles never replace manual sibling order and UUID tie breaking")
    func titleIndependentOrdering() throws {
        let nodes: [Node] = [
            .init(id: Self.id(1), title: "Goal", kind: .goal),
            .init(id: Self.id(2), parentID: Self.id(1), title: "Zulu"),
            .init(id: Self.id(3), parentID: Self.id(1), title: "Alpha"),
            .init(id: Self.id(4), parentID: Self.id(1), siblingOrder: 2, title: "First alphabetically")
        ]
        let rows = CanonicalInboxPresentation.build(
            activeItems: try nodes.reversed().map { try Self.item($0) },
            pendingMutations: [], trashEntries: []
        ).hierarchyRows
        #expect(rows.map(\.itemID) == [1, 2, 3, 4].map(Self.id))
        #expect(CanonicalHierarchyPresentation.build(rows: rows, scope: .goals).entries.map(\.id)
            == [1, 2, 3, 4].map(Self.id))
    }

    @Test("all lifecycle statuses remain present and inspectable")
    @MainActor
    func allStatusesAndReviewFences() throws {
        let statuses = ["inbox", "planned", "scheduled", "in_progress", "paused", "blocked",
                        "completed", "skipped", "cancelled", "future_state"]
        let items = try statuses.enumerated().map { index, status in
            try Self.item(.init(id: Self.id(index + 1), parentID: nil, siblingOrder: 0,
                                title: status, kind: .goal, status: status))
        } + [Self.item(.init(id: Self.id(20), parentID: nil, siblingOrder: 0,
                            title: "Project", kind: .project, status: "inbox"))]
        let store = PlannerStore(canonicalItems: items, restoreFromPersistence: false)
        let source = Self.source(store)
        #expect(source.hierarchyRows.count == statuses.count + 1)
        #expect(source.hierarchyRows.filter { $0.status == .skipped }.count == 1)
        for row in source.hierarchyRows {
            store.selectCanonicalItem(row.itemID)
            #expect(store.selectedCanonicalItemID == row.itemID)
            let route = try #require(CanonicalInboxEditorRoute.review(row: row, store: store))
            if row.status != .inbox && row.status != .planned {
                #expect(row.isReadOnly)
                #expect(route.readOnlyDiagnostic != nil)
            } else {
                #expect(!row.isReadOnly)
                #expect(route.readOnlyDiagnostic == nil)
            }
        }
    }

    @Test("shared dependency lookup preserves redaction opaque metadata and reported blockers")
    func indexedDependencyParity() throws {
        let reference = CanonicalDependencyReference(
            id: Self.id(1), title: "Private predecessor", kind: .task, status: .planned,
            isSensitive: true, isAvailable: true, hasOpaqueDependencies: false
        )
        let edge = CanonicalDependencyEdge(predecessorID: reference.id, relation: .finishToStart,
                                           minimumLagMinutes: 15, strength: .hard)
        let metadata: [JSONValue] = [
            .object([:]),
            .object(["constraints": .object(["dependencies": .array([edge.jsonValue])])]),
            .object(["constraints": .object(["dependencies": .string("newer dependency format")])])
        ]
        for constraints in metadata {
            let draft = DayWeaveCanonicalItemDraft(title: "Dependent work", timezoneName: "UTC",
                                                  flexibleConstraints: constraints)
            for ownerIsSensitive in [false, true] {
                for reported in [nil, reference.id, Self.id(99)] {
                    let array = CanonicalDependencyCatalog.causes(
                        for: draft, ownerIsSensitive: ownerIsSensitive,
                        references: [reference], reportedBlockerID: reported
                    )
                    let indexed = CanonicalDependencyCatalog.causes(
                        for: draft, ownerIsSensitive: ownerIsSensitive,
                        referencesByID: [reference.id: reference], reportedBlockerID: reported
                    )
                    #expect(indexed == array)
                    if !ownerIsSensitive {
                        #expect(indexed.allSatisfy { $0.title != reference.title })
                    }
                }
            }
        }
    }

    @Test("self cycles, rootless cycles and missing ancestry are protected and discoverable")
    func malformedHierarchy() throws {
        let nodes: [Node] = [
            .init(id: Self.id(1), parentID: Self.id(1), title: "Self goal", kind: .goal),
            .init(id: Self.id(2), parentID: Self.id(1), title: "Self descendant"),
            .init(id: Self.id(3), parentID: Self.id(4), title: "Cycle goal", kind: .goal),
            .init(id: Self.id(4), parentID: Self.id(3), title: "Cycle member"),
            .init(id: Self.id(5), parentID: Self.id(4), title: "Cycle descendant"),
            .init(id: Self.id(6), parentID: Self.id(99), title: "Orphan goal", kind: .goal),
            .init(id: Self.id(7), parentID: Self.id(6), title: "Orphan descendant")
        ]
        let source = CanonicalInboxPresentation.build(
            activeItems: try nodes.map { try Self.item($0) }, pendingMutations: [], trashEntries: []
        )
        #expect(source.hierarchyRows.first { $0.itemID == Self.id(1) }?.hasHierarchyCycle == true)
        #expect(source.hierarchyRows.filter(\.hasHierarchyCycle).count == 3)
        let projection = CanonicalHierarchyPresentation.build(rows: source.hierarchyRows, scope: .goals)
        #expect(Set(projection.entries.map(\.id)) == Set(nodes.map(\.id)))
        #expect(projection.entries.allSatisfy { $0.hasUnsafeAncestry })
        #expect(projection.entries.allSatisfy { $0.row.isReadOnly })
        #expect(projection.entries.allSatisfy { $0.row.isSensitive })
        #expect(projection.entries.allSatisfy { $0.breadcrumb.isEmpty })
        #expect(CanonicalHierarchyPresentation.build(
            rows: Array(source.hierarchyRows.reversed()), scope: .goals
        ) == projection)
    }

    @Test("queued create reparent detach and trash overlay canonical topology without losing custody")
    @MainActor
    func queuedOverlayAndPrivacy() throws {
        let privateGoal = try Self.item(.init(id: Self.id(1), title: "Private goal", kind: .goal), sensitive: true)
        let publicGoal = try Self.item(.init(id: Self.id(2), title: "Public goal", kind: .goal))
        let child = try Self.item(.init(id: Self.id(3), parentID: Self.id(1), title: "Moved work"))
        let trashed = try Self.item(.init(id: Self.id(4), parentID: Self.id(2), title: "Trashed leaf"))
        var move = DayWeaveCanonicalItemDraft(item: child)
        move.parentID = publicGoal.id
        let mutations: [DayWeavePendingCanonicalAuthoringMutation] = [
            .init(itemID: child.id, operation: .replace, draft: move,
                  expectedRevision: child.revision, baseItem: child),
            .init(itemID: Self.id(5), operation: .create,
                  draft: .init(title: "Queued child", timezoneName: "UTC", parentID: child.id)),
            .init(itemID: trashed.id, operation: .trash,
                  expectedRevision: trashed.revision, baseItem: trashed)
        ]
        let store = PlannerStore(
            canonicalItems: [privateGoal, publicGoal, child, trashed],
            pendingCanonicalAuthoringMutations: mutations, restoreFromPersistence: false
        )
        let source = Self.source(store)
        let projected = CanonicalHierarchyPresentation.build(rows: source.hierarchyRows, scope: .goals)
        #expect(projected.entries.map(\.id) == [1, 2, 3, 5].map(Self.id))
        #expect(projected.entries.last?.depth == 2)
        #expect(source.trash.map(\.itemID) == [trashed.id])
        #expect(projected.entries.filter { [child.id, Self.id(5)].contains($0.id) }.allSatisfy { $0.row.isSensitive })
        let retained = try #require(source.hierarchyRows.first { $0.itemID == child.id })
        #expect(retained.source == .pendingReplace)
        #expect(retained.mutationID == mutations[0].id)
        #expect(retained.activeCanonicalItem?.revision == child.revision)
        #expect(retained.parentID == publicGoal.id)

        move.parentID = nil
        let detached = DayWeavePendingCanonicalAuthoringMutation(
            itemID: child.id, operation: .replace, draft: move,
            expectedRevision: child.revision, baseItem: child
        )
        let detachedSource = CanonicalInboxPresentation.build(
            activeItems: [privateGoal, publicGoal, child], pendingMutations: [detached], trashEntries: []
        )
        #expect(!CanonicalHierarchyPresentation.build(
            rows: detachedSource.hierarchyRows, scope: .goals
        ).entries.contains { $0.id == child.id })
        #expect(store.pendingCanonicalAuthoringMutations == mutations)
    }

    @Test("authoritative delta refresh rebuilds the tree and review fences preserve submitted drafts")
    @MainActor
    func refreshAndSubmittedReview() throws {
        let root = try Self.item(.init(id: Self.id(1), title: "Goal", kind: .goal))
        let child = try Self.item(.init(id: Self.id(2), parentID: root.id, title: "First"))
        let store = PlannerStore(canonicalItems: [root, child], restoreFromPersistence: false)
        let before = Self.source(store)
        let updated = try Self.item(.init(id: child.id, title: "Detached"), revision: 2)
        store.applyCanonicalDelta([.upsert(updated)], nextCursor: "synthetic-drained-cursor")
        #expect(CanonicalHierarchyPresentation.build(rows: before.hierarchyRows, scope: .goals).entries.count == 2)
        #expect(CanonicalHierarchyPresentation.build(rows: Self.source(store).hierarchyRows, scope: .goals).entries.count == 1)
        #expect(store.canonicalDeltaCursor == "synthetic-drained-cursor")
        for submitted in [false, true] {
            let mutation = DayWeavePendingCanonicalAuthoringMutation(
                itemID: child.id, operation: .replace, draft: DayWeaveCanonicalItemDraft(item: child),
                expectedRevision: child.revision, baseItem: child,
                configurationIdentifier: "https://example.invalid", hasBeenSubmitted: submitted
            )
            let bound = PlannerStore(canonicalItems: [root, child],
                                     pendingCanonicalAuthoringMutations: [mutation], restoreFromPersistence: false)
            let row = try #require(Self.source(bound).hierarchyRows.first { $0.itemID == child.id })
            let route = try #require(CanonicalInboxEditorRoute.review(row: row, store: bound))
            #expect(route.readOnlyDiagnostic?.contains("bound for synchronization") == true)
            #expect(route.mode.itemID == child.id)
        }
    }

    @Test("missing hydration, offline state and failed storage never claim an empty workspace")
    func hydrationStates() {
        let statuses: [CanonicalSyncStatus] = [
            .ready, .configurationRequired("Missing"), .syncing("Loading"),
            .failed("Offline"), .online(updatedAt: Date(timeIntervalSince1970: 0), message: "Synced")
        ]
        for status in statuses {
            #expect(!CanonicalHierarchyAvailability.build(hasHydratedCache: false, status: status).canConfirmEmpty)
            #expect(!CanonicalHierarchyAvailability.build(
                hasHydratedCache: true, status: status, hasPersistenceError: true
            ).canConfirmEmpty)
        }
        #expect(CanonicalHierarchyAvailability.build(hasHydratedCache: true, status: .ready).canConfirmEmpty)
        #expect(!CanonicalHierarchyAvailability.build(hasHydratedCache: true, status: .failed("offline")).canConfirmEmpty)
    }

    @Test("production adapter caches deep canonical graphs and invalidates privacy immediately")
    @MainActor
    func deepProductionAdapterCache() throws {
        let nodes = (1...5_000).map { index in
            Node(id: Self.id(index), parentID: index == 1 ? nil : Self.id(index - 1),
                 title: "Canonical level \(index)", kind: index == 1 ? .goal : .task)
        }
        let store = PlannerStore(
            canonicalItems: try nodes.map { try Self.item($0) },
            canonicalDeltaCursor: "synthetic-complete-cursor",
            canonicalConfigurationIdentifier: "https://example.invalid", restoreFromPersistence: false
        )
        let cache = CanonicalHierarchySourceCache()
        let first = cache.presentation(for: store)
        #expect(first.hierarchyRows.count == 5_000)
        #expect(first.hierarchyRows.last?.depth == 4_999)
        #expect(first.hierarchyRows.last?.isSensitive == false)
        #expect(cache.buildCount == 1)
        #expect(cache.presentation(for: store) == first)
        _ = CanonicalHierarchyPresentation.build(rows: cache.presentation(for: store).hierarchyRows,
                                                scope: .goals, query: "5000", collapsedIDs: [Self.id(1)])
        #expect(cache.buildCount == 1)
        let privateRoot = try Self.item(nodes[0], sensitive: true, revision: 2)
        store.applyCanonicalDelta([.upsert(privateRoot)], nextCursor: "synthetic-private-refresh")
        #expect(cache.presentation(for: store).hierarchyRows.last?.isSensitive == true)
        #expect(cache.buildCount == 2)
        cache.clear()
        _ = cache.presentation(for: store)
        #expect(cache.buildCount == 3)
    }

    @Test("browser and inspector admit only owner-bound cache or never-submitted local creates")
    @MainActor
    func admissionAndInspectorRouting() throws {
        let goal = try Self.item(.init(id: Self.id(1), title: "Retained goal", kind: .goal))
        let terminal = try Self.item(.init(id: Self.id(2), parentID: goal.id, title: "Finished", status: "completed"))
        let local = DayWeavePendingCanonicalAuthoringMutation(
            itemID: Self.id(3), operation: .create,
            draft: .init(kind: .goal, title: "Local goal", timezoneName: "UTC")
        )
        let submitted = DayWeavePendingCanonicalAuthoringMutation(
            itemID: Self.id(4), operation: .create,
            draft: .init(kind: .goal, title: "Bound submitted goal", timezoneName: "UTC"),
            configurationIdentifier: "https://example.invalid", hasBeenSubmitted: true
        )
        let privateLocalChild = DayWeavePendingCanonicalAuthoringMutation(
            itemID: Self.id(5), operation: .create,
            draft: .init(kind: .goal, title: "Unadmitted parent", timezoneName: "UTC", parentID: goal.id)
        )
        let dependent = DayWeavePendingCanonicalAuthoringMutation(
            itemID: Self.id(6), operation: .create,
            draft: .init(kind: .goal, title: "Public dependent", timezoneName: "UTC",
                         flexibleConstraints: .object(["constraints": .object([
                            "dependencies": .array([CanonicalDependencyEdge(
                                predecessorID: privateLocalChild.itemID, relation: .finishToStart,
                                minimumLagMinutes: 0, strength: .hard
                            ).jsonValue])
                         ])]))
        )
        let unbound = PlannerStore(
            canonicalItems: [goal, terminal],
            pendingCanonicalAuthoringMutations: [local, submitted, privateLocalChild, dependent],
            selectedCanonicalItemID: goal.id, restoreFromPersistence: false
        )
        let cache = CanonicalHierarchySourceCache()
        let rows = cache.presentation(for: unbound).hierarchyRows
        #expect(Set(rows.map(\.itemID)) == [local.itemID, privateLocalChild.itemID, dependent.itemID])
        #expect(rows.first { $0.itemID == privateLocalChild.itemID }?.isSensitive == true)
        let dependentRow = try #require(rows.first { $0.itemID == dependent.itemID })
        #expect(!dependentRow.isSensitive)
        #expect(dependentRow.dependencyCauses.first?.isTitleRedacted == true)
        #expect(dependentRow.dependencyCauses.first?.title != privateLocalChild.draft?.title)
        #expect(cache.selectedRow(itemID: unbound.selectedCanonicalItemID, scope: .goals, store: unbound) == nil)
        #expect(cache.selectedRow(itemID: local.itemID, scope: .goals, store: unbound)?.title == "Local goal")

        // A bound cache may hold known items before initial delta completion;
        // it is readable but cannot claim an empty/complete workspace.
        let bound = PlannerStore(
            canonicalItems: [goal, terminal], canonicalConfigurationIdentifier: "https://example.invalid",
            restoreFromPersistence: false
        )
        #expect(cache.presentation(for: bound).hierarchyRows.count == 2)
        #expect(cache.selectedRow(itemID: terminal.id, scope: .goals, store: bound)?.isReadOnly == true)
        #expect(cache.selectedRow(itemID: goal.id, scope: .projects, store: bound) == nil)
        #expect(SidebarDestination.goals.usesCanonicalItemInspector)
        #expect(SidebarDestination.projects.usesCanonicalItemInspector)
        #expect(SidebarDestination.inbox.usesCanonicalItemInspector)
        #expect(!SidebarDestination.calendar.usesCanonicalItemInspector)
        #expect(!SidebarDestination.today.usesCanonicalItemInspector)
    }

    @Test("synthetic browser rows and native surface render without live app services")
    @MainActor
    func syntheticRender() throws {
        let fixture = try Self.fixture()
        let rows = CanonicalInboxPresentation.build(
            activeItems: try fixture.nodes.map { try Self.item($0) }, pendingMutations: [], trashEntries: []
        ).hierarchyRows
        let projection = CanonicalHierarchyPresentation.build(rows: rows, scope: .goals, collapsedIDs: [Self.id(4)])
        let surface = CanonicalHierarchyBrowserContent(
            scope: .goals, presentation: projection,
            availability: .build(hasHydratedCache: true, status: .ready), query: .constant(""),
            selectedID: Self.id(2), canMutate: true, timezoneName: "UTC",
            select: { _ in }, toggle: { _ in }, review: { _ in },
            eligibleParentIDs: Set(rows.filter { $0.status == .inbox || $0.status == .planned || $0.status == .blocked }.map(\.itemID))
        )
        .frame(width: 900, height: 850)
        // ImageRenderer cannot draw AppKit-backed TextField/ScrollView content.
        // Verify actual production rows separately, then capture the native
        // surface in a non-presented synthetic NSHostingView window.
        let rowSurface = VStack(spacing: 8) {
            ForEach(projection.entries.prefix(6)) { entry in
                CanonicalHierarchyBrowserRow(
                    entry: entry, isSelected: entry.id == Self.id(2), canMutate: true,
                    isSearching: false, timezoneName: "UTC", select: {}, toggle: {}, review: {}
                )
            }
        }
        .padding(20)
        .frame(width: 900)
        .background(Color(nsColor: .windowBackgroundColor))
        let renderer = ImageRenderer(content: rowSurface)
        renderer.scale = 1
        let image = try #require(renderer.cgImage)
        #expect(image.width == 900)
        #expect(image.height > 300)
        let host = NSHostingView(rootView: surface)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 850),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        defer { window.close() }
        host.frame = NSRect(x: 0, y: 0, width: 900, height: 850)
        host.layoutSubtreeIfNeeded()
        let nativeBitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: nativeBitmap)
        #expect(nativeBitmap.pixelsWide >= 900)
        #expect(nativeBitmap.pixelsHigh >= 850)
        if let path = ProcessInfo.processInfo.environment["DAYWEAVE_HIERARCHY_RENDER_DIRECTORY"] {
            let directory = URL(fileURLWithPath: path, isDirectory: true)
            let bitmap = NSBitmapImageRep(cgImage: image)
            let data = try #require(bitmap.representation(using: .png, properties: [:]))
            try data.write(to: directory.appendingPathComponent("macos-goals-rows-synthetic.png"), options: .atomic)
            let nativeData = try #require(nativeBitmap.representation(using: .png, properties: [:]))
            try nativeData.write(to: directory.appendingPathComponent("macos-goals-native-synthetic.png"), options: .atomic)
        }
    }

    @MainActor
    private static func source(_ store: PlannerStore) -> CanonicalInboxPresentation {
        CanonicalInboxPresentation.build(
            activeItems: store.canonicalItems, pendingMutations: store.pendingCanonicalAuthoringMutations,
            trashEntries: store.canonicalTrash,
            sensitivityPresentation: { store.canonicalSensitivityPresentation(itemID: $0) }
        )
    }

    private struct Node: Decodable {
        var id: UUID
        var parentID: UUID?
        var siblingOrder: UInt32 = 0
        var title: String
        var kind: DayWeaveCanonicalItemKind = .task
        var status: String = "inbox"
        enum CodingKeys: String, CodingKey {
            case id, parentID = "parent_id", siblingOrder = "sibling_order", title, kind, status
        }
    }

    private struct Fixture: Decodable {
        let nodes: [Node]
        let cases: [ProjectionCase]
    }

    private struct ProjectionCase: Decodable {
        let name: String
        let scope: CanonicalHierarchyPresentation.Scope
        let query: String
        let collapsed: [Int]
        let visible: [Int]
        let depths: [Int]
        let scopeContext: [Int]
        let searchMatches: [Int]
        enum CodingKeys: String, CodingKey {
            case name, scope, query, collapsed, visible, depths
            case scopeContext = "scope_context", searchMatches = "search_matches"
        }
    }

    private static func fixture() throws -> Fixture {
        let repo = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        return try JSONDecoder().decode(Fixture.self, from: Data(contentsOf:
            repo.appendingPathComponent("fixtures/hierarchy-browser/projection-v1.json")))
    }

    private static func id(_ number: Int) -> UUID {
        UUID(uuidString: "00000000-0000-0000-0000-\(String(format: "%012d", number))")!
    }

    private static func item(_ node: Node, sensitive: Bool = false, revision: UInt64 = 1) throws -> DayWeaveCanonicalItem {
        let object: [String: Any] = [
            "id": node.id.uuidString, "parent_id": node.parentID?.uuidString as Any? ?? NSNull(),
            "sibling_order": node.siblingOrder, "title": node.title, "kind": node.kind.wireValue,
            "status": node.status, "is_sensitive": sensitive, "notes": NSNull(), "timezone_name": "UTC",
            "duration_kind": "unknown", "duration_min_seconds": NSNull(),
            "duration_max_seconds": NSNull(), "duration_source": NSNull(),
            "deadline_kind": "none", "deadline_date": NSNull(), "deadline_strength": NSNull(),
            "deadline_soft_weight": NSNull(), "has_own_effort": false,
            "blocked_reason_kind": node.status == "blocked" ? "manual" as Any : NSNull(),
            "blocked_by_item_id": NSNull(),
            "blocked_reason": node.status == "blocked" ? "Synthetic prerequisite" as Any : NSNull(),
            "duration_seconds": NSNull(), "deadline_at": NSNull(), "earliest_start_at": NSNull(),
            "recurrence": NSNull(), "flexible_constraints": [:] as [String: String],
            "split_policy": ["type": "indivisible"], "importance": 50, "urgency": 50,
            "is_executable": false, "revision": revision,
            "created_at": "2026-09-01T09:00:00Z", "updated_at": "2026-09-01T10:00:00Z",
            "completed_at": node.status == "completed" ? "2026-09-01T10:00:00Z" as Any : NSNull(),
            "deleted_at": NSNull()
        ]
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: JSONSerialization.data(withJSONObject: object))
    }
}

#endif
