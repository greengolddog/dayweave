import AppKit
import Foundation
import SwiftUI
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Canonical hierarchy recorded roll-ups")
struct CanonicalHierarchyRollupTests {
    @Test("shared exact-second fixtures agree independent of input ordering")
    func sharedContract() throws {
        let fixture = try Self.fixture()
        #expect(fixture.schema == "dayweave.hierarchy-progress-fixtures/1")
        #expect(fixture.cases.count == 7)
        for input in fixture.cases {
            for nodes in [input.items.map(\.node), Array(input.items.reversed()).map(\.node)] {
                let result = CanonicalHierarchyRollup.build(nodes: nodes)
                #expect(result.unavailable == nil, "\(input.name)")
                for (id, expected) in input.expected {
                    #expect(result[try #require(UUID(uuidString: id))] == .available(expected.summary), "\(input.name)")
                }
            }
        }
        for input in fixture.invalidCases {
            #expect(CanonicalHierarchyRollup.build(nodes: input.items.map(\.node)).unavailable != nil,
                    "\(input.name): \(input.error)")
        }
    }

    @Test("canonical adapter preserves exact seconds rich bounds and unknown estimates")
    @MainActor
    func canonicalAdapter() throws {
        let goal = try Self.item(1, kind: .goal, status: .completed, seconds: 999, own: true)
        var range = try Self.item(4, parent: goal.id, seconds: 30)
        range.durationKind = .range
        range.durationMinimumSeconds = 1
        range.durationMaximumSeconds = 59
        let items = [goal, try Self.item(2, parent: goal.id, seconds: 1),
                     try Self.item(3, parent: goal.id, seconds: 1), range,
                     try Self.item(5, parent: goal.id, seconds: nil)]
        let store = Self.store(items)
        let result = CanonicalHierarchyRollupCache().presentation(for: store)
        let summary = try Self.summary(result[goal.id])
        #expect(summary.openLeafItems == 4)
        #expect(summary.completedLeafItems == 0) // Recorded parent lifecycle is independent.
        #expect(summary.minimumEstimateSeconds == 3)
        #expect(summary.expectedEstimateSeconds == 32)
        #expect(summary.maximumEstimateSeconds == 61)
        #expect(summary.unknownEstimates == 1)
        #expect(summary.estimateDescription.contains("3s min / 32s expected / 1m 1s max"))
        #expect(store.canonicalItems == items)
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(store.pendingCanonicalMutations.isEmpty)
    }

    @Test("unbound and not yet hydrated collections never publish zero or partial totals")
    @MainActor
    func authorityGates() throws {
        let goal = try Self.item(1, kind: .goal)
        let cache = CanonicalHierarchyRollupCache()
        for binding in [nil, "synthetic-binding"] {
            for cursor in [nil, "complete-cursor"] where binding == nil || cursor == nil {
                let store = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: cursor,
                    canonicalConfigurationIdentifier: binding, restoreFromPersistence: false)
                #expect(cache.presentation(for: store)[goal.id] == .unavailable(.notHydrated))
            }
        }
        for (binding, cursor) in [("", "cursor"), ("binding", "  \n"), (" \t", "cursor")] {
            let store = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: cursor,
                canonicalConfigurationIdentifier: binding, restoreFromPersistence: false)
            #expect(cache.presentation(for: store)[goal.id] == .unavailable(.notHydrated))
        }
        #expect(try Self.summary(cache.presentation(for: Self.store([goal]))[goal.id]).openLeafItems == 1)
    }

    @Test("queued create replace trash restore and status intent withhold without touching journals")
    @MainActor
    func pendingIntent() throws {
        let goal = try Self.item(1, kind: .goal)
        let child = try Self.item(2, parent: goal.id)
        for operation: CanonicalAuthoringOperation in [.create, .replace, .trash, .restore] {
            let mutation = DayWeavePendingCanonicalAuthoringMutation(
                itemID: child.id, operation: operation,
                draft: operation == .create || operation == .replace ? .init(item: child) : nil,
                expectedRevision: operation == .create ? nil : child.revision,
                baseItem: operation == .create || operation == .restore ? nil : child
            )
            let store = PlannerStore(canonicalItems: operation == .restore ? [goal] : [goal, child],
                canonicalDeltaCursor: "cursor", canonicalTombstoneRevisions: operation == .restore ? [child.id: 1] : [:],
                canonicalConfigurationIdentifier: "synthetic-binding",
                pendingCanonicalAuthoringMutations: [mutation],
                canonicalTrash: operation == .restore ? [.init(id: child.id, revision: 1,
                    deletedAt: Self.now, parentID: goal.id, lastKnownItem: child)] : [],
                restoreFromPersistence: false, now: { Self.now })
            let before = store.pendingCanonicalAuthoringMutations
            #expect(mutation.isValid, "\(operation)")
            #expect(store.canPersistPlan && store.persistenceError == nil, "\(operation)")
            #expect(CanonicalHierarchyRollupCache().presentation(for: store)[goal.id] == .unavailable(.pendingChanges))
            #expect(store.pendingCanonicalAuthoringMutations == before)
        }
        let status = PendingCanonicalMutation(id: UUID(), itemID: child.id, occurrenceID: nil,
            sessionIndex: nil, desiredStatus: .completed, baseRevision: child.revision,
            createdAt: Self.now, disposition: .conflicted, diagnostic: "Synthetic review")
        let store = PlannerStore(canonicalItems: [goal, child], canonicalDeltaCursor: "cursor",
            pendingCanonicalMutations: [status], canonicalConfigurationIdentifier: "synthetic-binding",
            restoreFromPersistence: false)
        #expect(store.canPersistPlan && store.persistenceError == nil)
        #expect(CanonicalHierarchyRollupCache().presentation(for: store)[goal.id] == .unavailable(.pendingChanges))
        #expect(store.pendingCanonicalMutations == [status])
    }

    @Test("sensitive descendants conceal public ancestor totals and privacy downgrades stay sticky")
    @MainActor
    func aggregatePrivacy() throws {
        let root = try Self.item(1, kind: .goal)
        let privateChild = try Self.item(2, parent: root.id, sensitive: true, seconds: 43)
        let other = try Self.item(3, kind: .goal)
        let cache = CanonicalHierarchyRollupCache()
        let store = Self.store([root, privateChild, other])
        let result = cache.presentation(for: store)
        #expect(result[root.id] == .concealed)
        #expect(result[privateChild.id] == .concealed)
        #expect(try Self.summary(result[other.id]).openLeafItems == 1)
        #expect(!result[root.id].accessibilityDescription.contains("43"))
        #expect(!result[root.id].accessibilityDescription.contains("1"))
        let downgrade = PendingCanonicalSensitivityMutation(id: UUID(), itemID: privateChild.id,
            desiredIsSensitive: false, baseRevision: 1, createdAt: Self.now, disposition: .pending, diagnostic: nil)
        let downgrading = PlannerStore(canonicalItems: [root, privateChild], canonicalDeltaCursor: "cursor",
            pendingCanonicalSensitivityMutations: [downgrade], canonicalConfigurationIdentifier: "synthetic-binding",
            restoreFromPersistence: false)
        #expect(cache.presentation(for: downgrading)[root.id] == .concealed)
        var publicChild = privateChild
        publicChild.isSensitive = false
        let promotion = PendingCanonicalSensitivityMutation(id: UUID(), itemID: publicChild.id,
            desiredIsSensitive: true, baseRevision: 1, createdAt: Self.now, disposition: .pending,
            diagnostic: nil, hasBeenSubmitted: true, followUpIsSensitive: false)
        let promoting = PlannerStore(canonicalItems: [root, publicChild], canonicalDeltaCursor: "cursor",
            pendingCanonicalSensitivityMutations: [promotion], canonicalConfigurationIdentifier: "synthetic-binding",
            restoreFromPersistence: false)
        #expect(cache.presentation(for: promoting)[root.id] == .concealed)
        #expect(promoting.pendingCanonicalSensitivityMutations == [promotion])
    }

    @Test("a pending move withholds both old and proposed ancestry")
    @MainActor
    func movedPrivacy() throws {
        let old = try Self.item(1, kind: .goal, sensitive: true)
        let new = try Self.item(2, kind: .goal)
        let child = try Self.item(3, parent: old.id, seconds: 31)
        var draft = DayWeaveCanonicalItemDraft(item: child)
        draft.parentID = new.id
        let mutation = DayWeavePendingCanonicalAuthoringMutation(itemID: child.id, operation: .replace,
            draft: draft, expectedRevision: 1, baseItem: child)
        let store = PlannerStore(canonicalItems: [old, new, child], canonicalDeltaCursor: "cursor",
            canonicalConfigurationIdentifier: "synthetic-binding", pendingCanonicalAuthoringMutations: [mutation],
            restoreFromPersistence: false)
        let result = CanonicalHierarchyRollupCache().presentation(for: store)
        #expect(result[old.id] == .unavailable(.pendingChanges))
        #expect(result[new.id] == .unavailable(.pendingChanges))
    }

    @Test("retained trash does not become a child or contribute old effort")
    @MainActor
    func retainedTrash() throws {
        let goal = try Self.item(1, kind: .goal)
        let old = try Self.item(2, parent: goal.id, status: .completed, sensitive: true, seconds: 9_999)
        let trash = DayWeaveCanonicalTrashEntry(id: old.id, revision: 2, deletedAt: Self.now,
            parentID: goal.id, lastKnownItem: old)
        let store = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: "cursor",
            canonicalTombstoneRevisions: [old.id: 2], canonicalConfigurationIdentifier: "synthetic-binding",
            canonicalTrash: [trash], restoreFromPersistence: false, now: { Self.now })
        #expect(store.canPersistPlan && store.persistenceError == nil)
        let summary = try Self.summary(CanonicalHierarchyRollupCache().presentation(for: store)[goal.id])
        #expect(summary.openLeafItems == 1)
        #expect(summary.completedLeafItems == 0 && summary.expectedEstimateSeconds == 0)
        #expect(store.canonicalTrash == [trash])
    }

    @Test("unresolved execution projections withhold even without a status journal")
    @MainActor
    func executionProjectionGate() throws {
        let goal = try Self.item(1, kind: .goal)
        let session = try Self.session(itemID: goal.id)
        for projection: DayWeaveTerminalProjectionState in [.pending, .retryAuthorized, .conflicted("Synthetic")] {
            var state = DayWeaveExecutionDurableState.empty
            state.deviceID = session.sourceDeviceID
            state.bindingIdentifier = "synthetic-binding"
            state.revision = session.revision
            state.terminalOutcomes[session.id] = .init(session: session, recordedAt: Self.now, projection: projection)
            let store = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: "cursor",
                canonicalConfigurationIdentifier: "synthetic-binding", executionState: state,
                restoreFromPersistence: false)
            #expect(store.canPersistPlan && store.persistenceError == nil)
            #expect(CanonicalHierarchyRollupCache().presentation(for: store)[goal.id] == .unavailable(.pendingChanges))
        }
    }

    @Test("failed storage has its own unavailable reason instead of exposing retained totals")
    @MainActor
    func failedStorage() throws {
        let goal = try Self.item(1, kind: .goal)
        let session = try Self.session(itemID: goal.id)
        var invalidState = DayWeaveExecutionDurableState.empty
        // Deliberately lacks the device/revision authority required by retained history.
        invalidState.terminalOutcomes[session.id] = .init(session: session, recordedAt: Self.now, projection: .pending)
        let store = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: "cursor",
            canonicalConfigurationIdentifier: "synthetic-binding", executionState: invalidState,
            restoreFromPersistence: false)
        #expect(!store.canPersistPlan && store.persistenceError != nil)
        #expect(CanonicalHierarchyRollupCache().presentation(for: store)[goal.id] == .unavailable(.storage))
    }

    @Test("execution receipts ahead of the cache wait for admitted item or tombstone acknowledgement")
    @MainActor
    func executionReceiptCatchup() throws {
        let goal = try Self.item(1, kind: .goal)
        let session = try Self.session(itemID: goal.id)
        var state = DayWeaveExecutionDurableState.empty
        state.deviceID = session.sourceDeviceID
        state.bindingIdentifier = "synthetic-binding"
        state.revision = session.revision
        state.terminalOutcomes[session.id] = .init(session: session, recordedAt: Self.now, projection: .applied(revision: 2))
        let store = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: "cursor",
            canonicalConfigurationIdentifier: "synthetic-binding", executionState: state,
            restoreFromPersistence: false)
        #expect(store.canPersistPlan && store.persistenceError == nil)
        let cache = CanonicalHierarchyRollupCache()
        #expect(cache.presentation(for: store)[goal.id] == .unavailable(.pendingChanges))
        let updated = try Self.item(1, kind: .goal, status: .completed, revision: 2)
        store.applyCanonicalDelta([.upsert(updated)], nextCursor: "cursor-next")
        #expect(try Self.summary(cache.presentation(for: store)[goal.id]).completedLeafItems == 1)
        #expect(cache.buildCount == 2)
        let tombstoned = PlannerStore(canonicalItems: [], canonicalDeltaCursor: "cursor",
            canonicalTombstoneRevisions: [goal.id: 2], canonicalConfigurationIdentifier: "synthetic-binding",
            executionState: state, restoreFromPersistence: false)
        #expect(cache.presentation(for: tombstoned).unavailable == nil)
        let prunedHistoricalItem = PlannerStore(canonicalItems: [], canonicalDeltaCursor: "complete-rebuild",
            canonicalConfigurationIdentifier: "synthetic-binding", executionState: state,
            restoreFromPersistence: false)
        #expect(cache.presentation(for: prunedHistoricalItem).unavailable == nil)
        let staleTombstone = PlannerStore(canonicalItems: [], canonicalDeltaCursor: "cursor",
            canonicalTombstoneRevisions: [goal.id: 1], canonicalConfigurationIdentifier: "synthetic-binding",
            executionState: state, restoreFromPersistence: false)
        #expect(cache.presentation(for: staleTombstone).unavailable == .pendingChanges)
        for resolution: DayWeaveTerminalProjectionState in [.keptLatest, .notRequired] {
            state.terminalOutcomes[session.id]?.projection = resolution
            let resolved = PlannerStore(canonicalItems: [goal], canonicalDeltaCursor: "cursor",
                canonicalConfigurationIdentifier: "synthetic-binding", executionState: state,
                restoreFromPersistence: false)
            #expect(try Self.summary(cache.presentation(for: resolved)[goal.id]).openLeafItems == 1)
        }
    }

    @Test("future and malformed canonical duration metadata are not silently normalized")
    @MainActor
    func unsupportedMetadata() throws {
        let valid = try Self.item(1, kind: .goal, seconds: 30, own: true)
        var examples: [DayWeaveCanonicalItem] = []
        var next = valid; next.kind = .unknown("new-kind"); examples.append(next)
        next = valid; next.status = .unknown("new-status"); examples.append(next)
        next = valid; next.durationKind = .unsupported("new-duration"); examples.append(next)
        next = valid; next.durationSource = .unsupported("new-source"); examples.append(next)
        next = valid; next.durationSource = nil; examples.append(next)
        next = valid; next.durationKind = .range; examples.append(next) // Equal range endpoints.
        next = valid; next.durationKind = .unknown; examples.append(next)
        for item in examples {
            #expect(CanonicalHierarchyRollupCache().presentation(for: Self.store([item]))[item.id]
                == .unavailable(.unsupportedMetadata))
        }
    }

    @Test("five thousand canonical levels aggregate once and redraw inputs reuse the cache")
    @MainActor
    func deepProductionCache() throws {
        let count = 5_000
        let items = try (1...count).map { index in
            try Self.item(index, parent: index == 1 ? nil : Self.id(index - 1),
                kind: index == 1 ? .goal : .task, seconds: index == count ? 1 : 999)
        }
        let store = Self.store(items)
        let cache = CanonicalHierarchyRollupCache()
        let start = ContinuousClock.now
        let first = cache.presentation(for: store)
        #expect(try Self.summary(first[Self.id(1)]).expectedEstimateSeconds == 1)
        #expect(try Self.summary(first[Self.id(count)]).openLeafItems == 1)
        for index in 1...30 {
            store.selectedCanonicalItemID = Self.id(index)
            #expect(cache.presentation(for: store) == first)
        }
        #expect(cache.buildCount == 1)
        let source = CanonicalHierarchySourceCache().presentation(for: store)
        for query in ["", "Synthetic 5000", "Synthetic 1"] {
            _ = CanonicalHierarchyPresentation.build(rows: source.hierarchyRows, scope: .goals,
                query: query, collapsedIDs: [Self.id(1)])
            #expect(cache.presentation(for: store) == first)
        }
        #expect(cache.buildCount == 1)
        print("Synthetic 5000-node roll-up adapter, cache and search checks: \(start.duration(to: .now))")
        let revised = try Self.item(count, parent: Self.id(count - 1), status: .completed, seconds: 2, revision: 2)
        store.applyCanonicalDelta([.upsert(revised)], nextCursor: "cursor-next")
        let updated = cache.presentation(for: store)
        #expect(cache.buildCount == 2)
        #expect(try Self.summary(updated[Self.id(1)]).expectedEstimateSeconds == 2)
        #expect(try Self.summary(updated[Self.id(1)]).completedLeafItems == 1)
        cache.clear()
        _ = cache.presentation(for: store)
        #expect(cache.buildCount == 3)
    }

    @Test("synthetic native hierarchy and inspector summary render without live services")
    @MainActor
    func syntheticRender() throws {
        let root = try Self.item(1, kind: .goal, title: "Build a personal observatory")
        let publicRoot = try Self.item(10, kind: .goal, title: "Learn the night sky")
        let items = [root,
            try Self.item(2, parent: root.id, status: .completed, seconds: 3_600, title: "Choose a telescope"),
            try Self.item(3, parent: root.id, seconds: 1_800, title: "Design the observing deck"),
            try Self.item(4, parent: root.id, status: .skipped, seconds: nil, title: "Visit the local shop"),
            publicRoot, try Self.item(11, parent: publicRoot.id, sensitive: true, title: "Protected synthetic item")]
        let store = Self.store(items)
        let source = CanonicalHierarchySourceCache().presentation(for: store)
        let rollups = CanonicalHierarchyRollupCache().presentation(for: store)
        let projection = CanonicalHierarchyPresentation.build(rows: source.hierarchyRows, scope: .goals,
            collapsedIDs: [root.id, publicRoot.id])
        let surface = HStack(spacing: 0) {
            CanonicalHierarchyBrowserContent(scope: .goals, presentation: projection,
                availability: .build(hasHydratedCache: true, status: .ready), query: .constant(""),
                selectedID: root.id, canMutate: false, timezoneName: "UTC", select: { _ in },
                toggle: { _ in }, review: { _ in }, rollups: rollups,
                parentIDs: Set(source.hierarchyRows.compactMap(\.parentID)))
                .frame(width: 790)
            Divider()
            VStack(alignment: .leading, spacing: 16) {
                Text("Recorded leaf summary").font(.headline)
                CanonicalHierarchyRollupView(presentation: rollups[root.id])
                Divider()
                CanonicalHierarchyRollupView(presentation: rollups[publicRoot.id])
                Spacer()
            }.padding(20).frame(width: 330)
        }
        .frame(width: 1_121, height: 680)
        .background(Color(nsColor: .windowBackgroundColor))
        .environment(\.colorScheme, .light)
        let host = NSHostingView(rootView: surface)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1_121, height: 680),
            styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: .aqua)
        window.contentView = host
        defer { window.close() }
        host.frame = NSRect(x: 0, y: 0, width: 1_121, height: 680)
        host.layoutSubtreeIfNeeded()
        let bitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        #expect(bitmap.pixelsWide >= 1_121 && bitmap.pixelsHigh >= 680)
        if let path = ProcessInfo.processInfo.environment["DAYWEAVE_HIERARCHY_RENDER_DIRECTORY"] {
            try #require(bitmap.representation(using: .png, properties: [:])).write(to:
                URL(fileURLWithPath: path).appendingPathComponent("macos-hierarchy-rollup-native-synthetic.png"))
        }
    }

    private struct Fixture: Decodable {
        let schema: String
        let cases: [Case]
        let invalidCases: [InvalidCase]
        enum CodingKeys: String, CodingKey { case schema, cases, invalidCases = "invalid_cases" }
    }
    private struct Case: Decodable { let name: String; let items: [Input]; let expected: [String: Expected] }
    private struct InvalidCase: Decodable { let name: String; let items: [Input]; let error: String }
    private struct Input: Decodable {
        let id: UUID
        let parentID: UUID?
        let kind: DayWeaveCanonicalItemKind
        let status: DayWeaveCanonicalItemStatus
        let hasOwnEffort: Bool
        let recurs: Bool
        let hasChildrenOutsidePlan: Bool
        let duration: Estimate?
        enum CodingKeys: String, CodingKey {
            case id, kind, status, recurs, duration, parentID = "parent_id"
            case hasOwnEffort = "has_own_effort", hasChildrenOutsidePlan = "has_children_outside_plan"
        }
        var node: CanonicalHierarchyRollup.Node {
            .init(id: id, parentID: parentID, kind: kind, status: status, hasOwnEffort: hasOwnEffort,
                recurs: recurs, hasChildrenOutsidePlan: hasChildrenOutsidePlan,
                estimate: duration.map { .init(minimumSeconds: $0.minimum_seconds,
                    expectedSeconds: $0.expected_seconds, maximumSeconds: $0.maximum_seconds) })
        }
    }
    private struct Estimate: Decodable {
        let minimum_seconds: UInt64; let expected_seconds: UInt64; let maximum_seconds: UInt64
    }
    private struct Expected: Decodable {
        let completed_leaf_items: UInt64; let skipped_leaf_items: UInt64; let cancelled_leaf_items: UInt64
        let open_leaf_items: UInt64; let recurring_leaf_items: UInt64; let fixed_events: UInt64
        let unknown_estimates: UInt64; let minimum_estimate_seconds: UInt64
        let expected_estimate_seconds: UInt64; let maximum_estimate_seconds: UInt64
        var summary: CanonicalHierarchyRollup.Summary {
            .init(completedLeafItems: completed_leaf_items, skippedLeafItems: skipped_leaf_items,
                cancelledLeafItems: cancelled_leaf_items, openLeafItems: open_leaf_items,
                recurringLeafItems: recurring_leaf_items, fixedEvents: fixed_events,
                unknownEstimates: unknown_estimates, minimumEstimateSeconds: minimum_estimate_seconds,
                expectedEstimateSeconds: expected_estimate_seconds, maximumEstimateSeconds: maximum_estimate_seconds)
        }
    }
    private static func fixture() throws -> Fixture {
        var repo = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repo.deleteLastPathComponent() }
        return try JSONDecoder().decode(Fixture.self, from: Data(contentsOf:
            repo.appendingPathComponent("fixtures/hierarchy-progress/projection-v1.json")))
    }
    private static let now = Date(timeIntervalSince1970: 1_788_860_000)
    private static func id(_ value: Int) -> UUID {
        UUID(uuidString: "00000000-0000-0000-0000-\(String(format: "%012d", value))")!
    }
    private static func summary(_ value: CanonicalHierarchyRollup.Presentation) throws -> CanonicalHierarchyRollup.Summary {
        guard case let .available(summary) = value else {
            throw NSError(domain: "SyntheticRollup", code: 1)
        }
        return summary
    }
    @MainActor
    private static func store(_ items: [DayWeaveCanonicalItem]) -> PlannerStore {
        PlannerStore(canonicalItems: items, canonicalDeltaCursor: "synthetic-cursor",
            canonicalConfigurationIdentifier: "synthetic-binding", restoreFromPersistence: false)
    }
    private static func item(_ number: Int, parent: UUID? = nil, kind: DayWeaveCanonicalItemKind = .task,
        status: DayWeaveCanonicalItemStatus = .planned, sensitive: Bool = false, seconds: UInt32? = nil,
        own: Bool = false, revision: UInt64 = 1, title: String? = nil) throws -> DayWeaveCanonicalItem {
        let object: [String: Any] = [
            "id": id(number).uuidString, "parent_id": parent?.uuidString as Any? ?? NSNull(),
            "sibling_order": 0, "title": title ?? "Synthetic \(number)", "kind": kind.wireValue,
            "status": status.wireValue, "is_sensitive": sensitive, "notes": NSNull(), "timezone_name": "UTC",
            "duration_kind": seconds == nil ? "unknown" : "exact", "duration_min_seconds": seconds as Any? ?? NSNull(),
            "duration_seconds": seconds as Any? ?? NSNull(), "duration_max_seconds": seconds as Any? ?? NSNull(),
            "duration_source": seconds == nil ? NSNull() : "user" as Any,
            "deadline_kind": "none", "deadline_date": NSNull(), "deadline_strength": NSNull(),
            "deadline_soft_weight": NSNull(), "has_own_effort": own,
            "blocked_reason_kind": NSNull(), "blocked_by_item_id": NSNull(), "blocked_reason": NSNull(),
            "deadline_at": NSNull(), "earliest_start_at": NSNull(), "recurrence": NSNull(),
            "flexible_constraints": [:] as [String: String], "split_policy": ["type": "indivisible"],
            "importance": 50, "urgency": 50, "is_executable": false, "revision": revision,
            "created_at": "2026-09-01T09:00:00Z", "updated_at": "2026-09-01T10:00:00Z",
            "completed_at": status == .completed ? "2026-09-01T10:00:00Z" as Any : NSNull(), "deleted_at": NSNull()
        ]
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: JSONSerialization.data(withJSONObject: object))
    }
    private static func session(itemID: UUID) throws -> DayWeaveExecutionSession {
        let raw: [String: Any] = [
            "id": id(99).uuidString, "item_id": itemID.uuidString, "item_revision": 1,
            "occurrence_id": NSNull(), "session_index": 0, "planned_block_id": NSNull(),
            "source_device_id": id(98).uuidString, "status": "completed", "revision": 2,
            "accumulated_seconds": 1, "actual_seconds": 1, "started_at": "2026-09-01T09:00:00Z",
            "running_since": NSNull(), "paused_at": NSNull(), "pause_until": NSNull(), "pause_reason": NSNull(),
            "ended_at": "2026-09-01T09:00:01Z", "created_at": "2026-09-01T09:00:00Z",
            "updated_at": "2026-09-01T09:00:01Z"
        ]
        let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveExecutionSession.self, from: JSONSerialization.data(withJSONObject: raw))
    }
}
#endif
