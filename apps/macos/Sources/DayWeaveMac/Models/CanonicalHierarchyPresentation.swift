import Foundation

/// Local browsing only: no mutation, scheduling, network, or persisted search.
struct CanonicalHierarchyPresentation: Equatable, Sendable {
    enum Scope: String, CaseIterable, Codable, Sendable {
        case projects, goals

        var kind: DayWeaveCanonicalItemKind { self == .projects ? .project : .goal }
        var title: String { self == .projects ? "Projects" : "Goals" }
        var symbol: String { self == .projects ? "folder" : "scope" }
    }

    struct Entry: Identifiable, Equatable, Sendable {
        let row: CanonicalInboxPresentation.Row
        let depth: Int
        let breadcrumb: [String]
        let hasChildren: Bool
        let isCollapsed: Bool
        let isContext: Bool
        let isSearchMatch: Bool
        let hasUnsafeAncestry: Bool
        var id: UUID { row.itemID }
    }

    let entries: [Entry]
    let scopedItemCount: Int
    let matchingItemCount: Int
    let isSearching: Bool

    static func build(
        rows: [CanonicalInboxPresentation.Row],
        scope: Scope,
        query: String = "",
        collapsedIDs: Set<UUID> = []
    ) -> Self {
        let byID = Dictionary(rows.map { ($0.itemID, $0) }, uniquingKeysWith: { first, _ in first })
        func ordered(_ ids: [UUID]) -> [UUID] {
            ids.sorted {
                let left = byID[$0]?.siblingOrder ?? 0
                let right = byID[$1]?.siblingOrder ?? 0
                return left == right ? $0.uuidString < $1.uuidString : left < right
            }
        }
        var children: [UUID: [UUID]] = [:]
        for row in byID.values {
            if let parent = row.parentID, parent != row.itemID, byID[parent] != nil {
                children[parent, default: []].append(row.itemID)
            }
        }
        for key in children.keys { children[key] = ordered(children[key] ?? []) }

        var unsafe = Set<UUID>()
        var unsafePending = rows.filter {
            $0.hasUnsafeAncestry || $0.hasHierarchyCycle || $0.hasMissingParent
                || ($0.parentID != nil && byID[$0.parentID!] == nil)
        }.map(\.itemID)
        while let id = unsafePending.popLast() {
            guard unsafe.insert(id).inserted else { continue }
            unsafePending.append(contentsOf: children[id] ?? [])
        }

        // A global visited set bounds both overlapping root walks and cycles.
        var scoped = Set<UUID>()
        var pending = rows.filter { $0.kind == scope.kind }.map(\.itemID)
        while let id = pending.popLast() {
            guard scoped.insert(id).inserted else { continue }
            pending.append(contentsOf: children[id] ?? [])
        }
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let searching = !trimmed.isEmpty
        let matches = searching ? Set(scoped.filter { id in
            byID[id]?.title.range(
                of: trimmed, options: [.caseInsensitive], locale: Locale(identifier: "en_US_POSIX")
            ) != nil
        }) : scoped
        var included = matches
        pending = Array(matches)
        while let id = pending.popLast() {
            guard let parent = byID[id]?.parentID, byID[parent] != nil,
                  included.insert(parent).inserted else { continue }
            pending.append(parent)
        }

        let orderedIDs = ordered(Array(included))
        let roots = orderedIDs.filter { id in
            guard let parent = byID[id]?.parentID else { return true }
            return parent == id || !included.contains(parent)
        }
        var visited = Set<UUID>()
        var entries: [Entry] = []
        // Both passes are iterative. The second makes rootless cycles and
        // malformed retained data discoverable without inventing parent edges.
        for candidates in [roots, orderedIDs] {
            for root in candidates where !visited.contains(root) {
                var stack: [(UUID, Int, [String], Bool)] = [(root, 0, [], false)]
                while let (id, depth, breadcrumb, hidden) = stack.popLast() {
                    guard visited.insert(id).inserted, let row = byID[id] else { continue }
                    let descendants = (children[id] ?? []).filter { included.contains($0) }
                    let collapsed = !searching && collapsedIDs.contains(id)
                    if !hidden {
                        entries.append(Entry(
                            row: row,
                            depth: depth,
                            breadcrumb: unsafe.contains(id) ? [] : breadcrumb,
                            hasChildren: !descendants.isEmpty,
                            isCollapsed: collapsed,
                            isContext: !scoped.contains(id),
                            isSearchMatch: searching && matches.contains(id),
                            hasUnsafeAncestry: unsafe.contains(id)
                        ))
                    }
                    var nextBreadcrumb = breadcrumb
                    nextBreadcrumb.append(row.title)
                    if nextBreadcrumb.count > CanonicalInboxPresentation.maximumBreadcrumbDepth {
                        nextBreadcrumb.removeFirst(
                            nextBreadcrumb.count - CanonicalInboxPresentation.maximumBreadcrumbDepth
                        )
                    }
                    for child in descendants.reversed() {
                        stack.append((child, depth + 1, nextBreadcrumb, hidden || collapsed))
                    }
                }
            }
        }
        return Self(
            entries: entries, scopedItemCount: scoped.count,
            matchingItemCount: matches.count, isSearching: searching
        )
    }
}

/// Synchronous, graph-keyed memoization keeps privacy updates immediate while
/// query, disclosure, selection and clock redraws reuse the admitted base rows.
/// The cache is view-owned and is cleared when that protected view disappears.
@MainActor
final class CanonicalHierarchySourceCache {
    private struct Key: Equatable {
        let items: [DayWeaveCanonicalItem]
        let mutations: [DayWeavePendingCanonicalAuthoringMutation]
        let sensitivity: [PendingCanonicalSensitivityMutation]
        let trash: [DayWeaveCanonicalTrashEntry]
        let binding: String?
    }

    private var key: Key?
    private var cached: CanonicalInboxPresentation?
    private var cachedParentIDs: Set<UUID> = []
    private(set) var buildCount = 0

    func presentation(for store: PlannerStore) -> CanonicalInboxPresentation {
        let next = Key(
            items: store.canonicalItems,
            mutations: store.pendingCanonicalAuthoringMutations,
            sensitivity: store.pendingCanonicalSensitivityMutations,
            trash: store.canonicalTrash,
            binding: store.canonicalConfigurationIdentifier
        )
        if key == next, let cached { return cached }
        let sensitivity = store.canonicalSensitivityPresentationIndex()
        // An unbound legacy cache has no admitted owner. Only truly local,
        // never-submitted creates can enter this browser before binding.
        let admittedItems = next.binding == nil ? [] : next.items
        let admittedMutations = next.binding == nil ? next.mutations.filter {
            $0.operation == .create && $0.configurationIdentifier == nil && !$0.hasBeenSubmitted
        } : next.mutations
        let result = CanonicalInboxPresentation.build(
            activeItems: admittedItems, pendingMutations: admittedMutations,
            trashEntries: next.binding == nil ? [] : next.trash,
            sensitivityPresentation: { sensitivity[$0] }
        )
        key = next
        cached = result
        cachedParentIDs = Set(result.hierarchyRows.compactMap(\.parentID))
        buildCount += 1
        return result
    }

    func clear() {
        key = nil
        cached = nil
        cachedParentIDs = []
    }

    func parentIDs(for store: PlannerStore) -> Set<UUID> {
        _ = presentation(for: store)
        return cachedParentIDs
    }

    func selectedRow(itemID: UUID?, scope: CanonicalHierarchyPresentation.Scope,
                     store: PlannerStore) -> CanonicalInboxPresentation.Row? {
        guard let itemID else { return nil }
        return CanonicalHierarchyPresentation.build(rows: presentation(for: store).hierarchyRows, scope: scope)
            .entries.first { $0.id == itemID }?.row
    }
}

extension SidebarDestination {
    var usesCanonicalItemInspector: Bool {
        switch self {
        case .inbox, .projects, .goals: true
        default: false
        }
    }

    var hierarchyScope: CanonicalHierarchyPresentation.Scope? {
        switch self {
        case .projects: .projects
        case .goals: .goals
        default: nil
        }
    }
}

/// A retained cursor is installed only after the complete bounded delta drain.
/// Neither schedule blocks nor a configured account prove item hydration.
struct CanonicalHierarchyAvailability: Equatable, Sendable {
    let message: String
    let canConfirmEmpty: Bool

    static func build(
        hasHydratedCache: Bool, status: CanonicalSyncStatus, hasPersistenceError: Bool = false
    ) -> Self {
        if hasPersistenceError {
            return Self(
                message: "Local storage needs attention · the saved item collection may be incomplete.",
                canConfirmEmpty: false
            )
        }
        return switch status {
        case .configurationRequired:
            Self(message: hasHydratedCache
                ? "Offline view · showing the last synced items and local changes. Configure sync to refresh."
                : "Not synced yet · only local changes are available. Configure sync to load your items.",
                 canConfirmEmpty: false)
        case .failed:
            Self(message: hasHydratedCache
                ? "Sync unavailable · showing the last synced items and local changes."
                : "Items could not be loaded · local changes remain available.", canConfirmEmpty: false)
        case .syncing:
            Self(message: hasHydratedCache
                ? "Refreshing · showing the last synced items and local changes."
                : "Loading items · local changes remain available.", canConfirmEmpty: false)
        case .ready:
            Self(message: hasHydratedCache
                ? "Saved item cache · local changes are included. Sync to check for updates."
                : "Not synced yet · sync to load your items. Local changes remain available.",
                 canConfirmEmpty: hasHydratedCache)
        case .online:
            Self(message: hasHydratedCache
                ? "Synced item cache · local changes are included."
                : "Items have not been loaded yet. Local changes remain available.",
                 canConfirmEmpty: hasHydratedCache)
        }
    }
}
