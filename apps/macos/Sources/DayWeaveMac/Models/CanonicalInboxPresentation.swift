import Foundation

struct CanonicalInboxPresentation: Equatable, Sendable {
    /// A hierarchy may be arbitrarily deep, but retaining every ancestor title
    /// on every flattened row would make a long chain consume quadratic memory.
    /// `depth` remains exact while the displayed breadcrumb keeps only the
    /// nearest ancestors.
    static let maximumBreadcrumbDepth = 32

    struct Row: Identifiable, Equatable, Sendable {
        enum Source: Equatable, Sendable {
            case canonical
            case localCreate
            case pendingReplace
            case pendingTrash
            case pendingRestore
            case activeRestore
            case recentTrash
        }

        enum SyncState: Equatable, Sendable {
            case synced
            case waiting
            case submitted
            case conflicted(String?)
        }

        let id: UUID
        let itemID: UUID
        let title: String
        let kind: DayWeaveCanonicalItemKind
        let status: DayWeaveCanonicalItemStatus
        let parentID: UUID?
        let siblingOrder: UInt32
        let depth: Int
        let breadcrumb: [String]
        let sensitivityPresentation: CanonicalSensitivityPresentation
        let durationKind: DayWeaveDurationKind
        let durationMinimumSeconds: UInt32?
        let durationSeconds: UInt32?
        let durationMaximumSeconds: UInt32?
        let durationSource: DayWeaveDurationSource?
        let deadlineKind: DayWeaveDeadlineKind
        let deadlineAt: Date?
        let deadlineDate: String?
        let deadlineStrength: DayWeaveDeadlineStrength?
        let deadlineSoftWeight: UInt32?
        let blockedReasonKind: DayWeaveBlockedReasonKind?
        let blockedByItemID: UUID?
        let blockedReason: String?
        let dependencyCauses: [CanonicalDependencyCause]
        let hasOpaqueDependencies: Bool
        let source: Source
        let syncState: SyncState
        let mutationID: UUID?
        let revision: UInt64?
        /// The latest active canonical version, when one exists separately
        /// from a retained local draft shown by this row.
        let activeCanonicalItem: DayWeaveCanonicalItem?
        let isReadOnly: Bool
        let hasMissingParent: Bool
        let hasHierarchyCycle: Bool

        var isSensitive: Bool { sensitivityPresentation != .standard }

        var durationDescription: String {
            switch durationKind {
            case .unknown:
                return "Unknown duration"
            case .exact:
                return Self.durationValue(durationSeconds)
            case .range:
                guard let minimum = durationMinimumSeconds,
                      let expected = durationSeconds,
                      let maximum = durationMaximumSeconds else {
                    return "Duration metadata unavailable"
                }
                return "\(Self.durationValue(minimum))–\(Self.durationValue(maximum))"
                    + " · expected \(Self.durationValue(expected))"
            case .unsupported:
                return "Duration requires a newer DayWeave version"
            }
        }

        var timingTitle: String? {
            if kind == .event, deadlineAt != nil { return "Ends" }
            return switch deadlineKind {
            case .none: nil
            case .date: "Due date"
            case .dateTime: "Deadline"
            case .unsupported: "Deadline"
            }
        }

        func timingDescription(timezoneName: String) -> String? {
            if kind == .event, let deadlineAt {
                return PlannerTimeZone.dateTimeLabel(deadlineAt, timezoneName: timezoneName)
            }
            let base: String?
            switch deadlineKind {
            case .none:
                return nil
            case .date:
                base = deadlineDate
            case .dateTime:
                base = deadlineAt.map {
                    PlannerTimeZone.dateTimeLabel($0, timezoneName: timezoneName)
                }
            case .unsupported:
                return "Requires a newer DayWeave version"
            }
            guard let base else { return "Unavailable" }
            let strength = switch deadlineStrength {
            case .hard?: "Hard"
            case .soft?: deadlineSoftWeight.map { "Soft · weight \($0)" } ?? "Soft"
            case .unsupported?: "Newer deadline policy"
            case nil: "Deadline policy unavailable"
            }
            return "\(base) · \(strength)"
        }

        var blockedDescription: String? {
            guard status == .blocked else { return nil }
            switch blockedReasonKind {
            case .dependency?:
                let blockers = blockingDependencyCauses
                if blockers.count == 1, let blocker = blockers.first {
                    return "Waiting for \(blocker.title)"
                }
                if blockers.count > 1 {
                    return "Waiting on \(blockers.count) prerequisites"
                }
                let dependency = blockedByItemID.map {
                    "Waiting for item \($0.uuidString.lowercased().prefix(8))"
                } ?? "Waiting for a dependency"
                // Legacy dependency reasons may embed a private predecessor title.
                // Without a resolved cause, only show the opaque relationship.
                return dependency
            case .manual?:
                return blockedReason.map { "Manually blocked · \($0)" } ?? "Manually blocked"
            case .external?:
                return blockedReason.map { "External blocker · \($0)" } ?? "External blocker"
            case .unsupported?:
                return "Blocked for a reason that requires a newer DayWeave version"
            case nil:
                return "Blocked reason unavailable"
            }
        }

        var blockingDependencyCauses: [CanonicalDependencyCause] {
            guard status == .blocked, blockedReasonKind == .dependency else { return [] }
            return dependencyCauses.filter(\.isBlocking)
        }

        var accessibilitySummary: String {
            let kindName = kind.wireValue.replacingOccurrences(of: "_", with: " ")
            let state: String = switch syncState {
            case .synced: "synced"
            case .waiting: "waiting to sync"
            case .submitted: "submitted, awaiting recovery"
            case .conflicted: "needs conflict review"
            }
            let privacy = isSensitive ? "sensitive" : "standard privacy"
            return "\(title), \(kindName), level \(depth + 1), \(privacy), \(state)"
        }

        private static func durationValue(_ seconds: UInt32?) -> String {
            guard let seconds else { return "Unknown" }
            if seconds.isMultiple(of: 60) {
                return "\(seconds / 60) min"
            }
            return "\(seconds) sec"
        }
    }

    let inbox: [Row]
    let planned: [Row]
    let active: [Row]
    let completed: [Row]
    let conflicts: [Row]
    let trash: [Row]

    static func build(
        activeItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry],
        sensitivityPresentation: ((UUID) -> CanonicalSensitivityPresentation)? = nil
    ) -> Self {
        let pendingByItem = Dictionary(
            pendingMutations.map { ($0.itemID, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        var nodes: [UUID: Node] = Dictionary(
            uniqueKeysWithValues: activeItems.map { item in
                (item.id, Node(item: item, mutation: pendingByItem[item.id]))
            }
        )
        for mutation in pendingMutations
            where mutation.operation == .create || mutation.operation == .replace {
            guard let draft = mutation.draft, nodes[mutation.itemID] == nil else { continue }
            nodes[mutation.itemID] = Node(itemID: mutation.itemID, draft: draft, mutation: mutation)
        }

        let hierarchy = Hierarchy(nodes: nodes)
        let dependencyReferences = CanonicalDependencyCatalog.references(
            canonicalItems: activeItems,
            pendingMutations: pendingMutations,
            trashEntries: trashEntries,
            sensitivity: { id in
                if let sensitivityPresentation {
                    return sensitivityPresentation(id) != .standard
                }
                return hierarchy.sensitivityPresentationByID[id] != .standard
            }
        )
        var inbox: [Row] = []
        var planned: [Row] = []
        var active: [Row] = []
        var completed: [Row] = []
        var conflicts: [Row] = []
        var trash: [Row] = []

        for id in hierarchy.orderedIDs {
            guard let node = nodes[id], node.mutation?.operation != .trash else { continue }
            let row = node.row(
                depth: hierarchy.depthByID[id] ?? 0,
                breadcrumb: hierarchy.breadcrumbByID[id] ?? [],
                sensitivityPresentation: sensitivityPresentation?(id)
                    ?? hierarchy.sensitivityPresentationByID[id]
                    ?? .inherited,
                dependencyReferences: dependencyReferences,
                hasMissingParent: hierarchy.missingParentIDs.contains(id),
                hasHierarchyCycle: hierarchy.cyclicIDs.contains(id)
            )
            if case .conflicted = row.syncState { conflicts.append(row) }
            switch row.status {
            case .inbox: inbox.append(row)
            case .planned: planned.append(row)
            case .scheduled, .inProgress, .paused, .blocked: active.append(row)
            case .completed: completed.append(row)
            case .skipped, .cancelled, .unknown: break
            }
        }

        let activeByID = Dictionary(uniqueKeysWithValues: activeItems.map { ($0.id, $0) })
        for mutation in pendingMutations where mutation.operation == .trash {
            let item = mutation.baseItem ?? activeByID[mutation.itemID]
            let row = Row(
                id: mutation.itemID,
                itemID: mutation.itemID,
                title: item?.title ?? mutation.displayTitle,
                kind: item?.kind ?? .task,
                status: item?.status ?? .inbox,
                parentID: item?.parentID,
                siblingOrder: item?.siblingOrder ?? 0,
                depth: 0,
                breadcrumb: [],
                sensitivityPresentation: sensitivityPresentation?(mutation.itemID)
                    ?? ((item?.isSensitive ?? true) ? .own : .standard),
                durationKind: item?.durationKind
                    ?? (item?.durationSeconds == nil ? .unknown : .exact),
                durationMinimumSeconds: item?.durationMinimumSeconds ?? item?.durationSeconds,
                durationSeconds: item?.durationSeconds,
                durationMaximumSeconds: item?.durationMaximumSeconds ?? item?.durationSeconds,
                durationSource: item?.durationSource
                    ?? (item?.durationSeconds == nil ? nil : .user),
                deadlineKind: item?.deadlineKind
                    ?? (item?.kind == .event || item?.deadlineAt == nil ? .none : .dateTime),
                deadlineAt: item?.deadlineAt,
                deadlineDate: item?.deadlineDate,
                deadlineStrength: item?.deadlineStrength
                    ?? (item?.kind == .event || item?.deadlineAt == nil ? nil : .hard),
                deadlineSoftWeight: item?.deadlineSoftWeight,
                blockedReasonKind: item?.blockedReasonKind,
                blockedByItemID: item?.blockedByItemID,
                blockedReason: item?.blockedReason,
                dependencyCauses: [],
                hasOpaqueDependencies: item.map {
                    CanonicalDependencyEdge.decode(
                        fromFlexibleConstraints: $0.flexibleConstraints
                    ) == nil
                } ?? false,
                source: .pendingTrash,
                syncState: syncState(mutation),
                mutationID: mutation.id,
                revision: item?.revision,
                activeCanonicalItem: activeByID[mutation.itemID],
                isReadOnly: true,
                hasMissingParent: false,
                hasHierarchyCycle: false
            )
            trash.append(row)
            if case .conflicted = row.syncState { conflicts.append(row) }
        }
        for entry in trashEntries.sorted(by: trashOrder) {
            let mutation = pendingByItem[entry.id]
            // The retained draft row is the only safe conflict-recovery UI for
            // create/replace collisions. A second generic trash row would
            // misleadingly offer Restore while another operation owns the ID.
            if let mutation, mutation.operation != .restore { continue }
            let item = entry.lastKnownItem
            let row = Row(
                id: entry.id,
                itemID: entry.id,
                title: entry.title,
                kind: item?.kind ?? .task,
                status: item?.status ?? .inbox,
                parentID: entry.parentID,
                siblingOrder: item?.siblingOrder ?? 0,
                depth: 0,
                breadcrumb: [],
                sensitivityPresentation: sensitivityPresentation?(entry.id)
                    ?? (entry.isSensitive ? .own : .standard),
                durationKind: item?.durationKind
                    ?? (item?.durationSeconds == nil ? .unknown : .exact),
                durationMinimumSeconds: item?.durationMinimumSeconds ?? item?.durationSeconds,
                durationSeconds: item?.durationSeconds,
                durationMaximumSeconds: item?.durationMaximumSeconds ?? item?.durationSeconds,
                durationSource: item?.durationSource
                    ?? (item?.durationSeconds == nil ? nil : .user),
                deadlineKind: item?.deadlineKind
                    ?? (item?.kind == .event || item?.deadlineAt == nil ? .none : .dateTime),
                deadlineAt: item?.deadlineAt,
                deadlineDate: item?.deadlineDate,
                deadlineStrength: item?.deadlineStrength
                    ?? (item?.kind == .event || item?.deadlineAt == nil ? nil : .hard),
                deadlineSoftWeight: item?.deadlineSoftWeight,
                blockedReasonKind: item?.blockedReasonKind,
                blockedByItemID: item?.blockedByItemID,
                blockedReason: item?.blockedReason,
                dependencyCauses: [],
                hasOpaqueDependencies: item.map {
                    CanonicalDependencyEdge.decode(
                        fromFlexibleConstraints: $0.flexibleConstraints
                    ) == nil
                } ?? false,
                source: mutation?.operation == .restore ? .pendingRestore : .recentTrash,
                syncState: mutation.map(syncState) ?? .synced,
                mutationID: mutation?.id,
                revision: entry.revision,
                activeCanonicalItem: nil,
                isReadOnly: true,
                hasMissingParent: false,
                hasHierarchyCycle: false
            )
            trash.append(row)
            if case .conflicted = row.syncState { conflicts.append(row) }
        }

        return Self(
            inbox: inbox,
            planned: planned,
            active: active,
            completed: completed,
            conflicts: deduplicated(conflicts),
            trash: deduplicated(trash)
        )
    }

    private static func syncState(
        _ mutation: DayWeavePendingCanonicalAuthoringMutation
    ) -> Row.SyncState {
        if mutation.disposition == .conflicted { return .conflicted(mutation.diagnostic) }
        return mutation.hasBeenSubmitted ? .submitted : .waiting
    }

    private static func trashOrder(
        _ left: DayWeaveCanonicalTrashEntry,
        _ right: DayWeaveCanonicalTrashEntry
    ) -> Bool {
        if left.deletedAt != right.deletedAt { return left.deletedAt > right.deletedAt }
        return left.id.uuidString < right.id.uuidString
    }

    private static func deduplicated(_ rows: [Row]) -> [Row] {
        var seen = Set<UUID>()
        return rows.filter { seen.insert($0.itemID).inserted }
    }
}

private extension CanonicalInboxPresentation {
    struct Node: Sendable {
        let itemID: UUID
        let draft: DayWeaveCanonicalItemDraft
        let mutation: DayWeavePendingCanonicalAuthoringMutation?
        let revision: UInt64?
        let activeCanonicalItem: DayWeaveCanonicalItem?
        let readOnly: Bool

        init(item: DayWeaveCanonicalItem, mutation: DayWeavePendingCanonicalAuthoringMutation?) {
            itemID = item.id
            if mutation?.operation == .create || mutation?.operation == .replace,
               let pendingDraft = mutation?.draft {
                draft = pendingDraft
            } else {
                draft = DayWeaveCanonicalItemDraft(item: item)
            }
            self.mutation = mutation
            revision = item.revision
            activeCanonicalItem = item
            readOnly = !item.supportsCanonicalAuthoringReplacement
                || (item.status != .inbox && item.status != .planned)
                || mutation?.hasBeenSubmitted == true
                || mutation?.disposition == .conflicted
                || mutation?.operation == .restore
        }

        init(
            itemID: UUID,
            draft: DayWeaveCanonicalItemDraft,
            mutation: DayWeavePendingCanonicalAuthoringMutation
        ) {
            self.itemID = itemID
            self.draft = draft
            self.mutation = mutation
            revision = nil
            activeCanonicalItem = nil
            readOnly = mutation.hasBeenSubmitted || mutation.disposition == .conflicted
        }

        func row(
            depth: Int,
            breadcrumb: [String],
            sensitivityPresentation: CanonicalSensitivityPresentation,
            dependencyReferences: [CanonicalDependencyReference],
            hasMissingParent: Bool,
            hasHierarchyCycle: Bool
        ) -> Row {
            let source: Row.Source
            if mutation?.operation == .create {
                source = .localCreate
            } else if mutation?.operation == .replace {
                source = .pendingReplace
            } else if mutation?.operation == .restore {
                source = .activeRestore
            } else {
                source = .canonical
            }
            let syncState = mutation.map(CanonicalInboxPresentation.syncState) ?? .synced
            let retainsCanonicalStructure = source == .canonical || source == .activeRestore
            let structuralItem = retainsCanonicalStructure ? activeCanonicalItem : nil
            let inferredDeadlineKind: DayWeaveDeadlineKind = draft.kind == .event
                || draft.deadlineAt == nil ? .none : .dateTime
            let hasOpaqueDependencies = CanonicalDependencyEdge.decode(
                fromFlexibleConstraints: draft.flexibleConstraints
            ) == nil
            return Row(
                id: itemID,
                itemID: itemID,
                title: draft.title,
                kind: draft.kind,
                status: draft.status,
                parentID: draft.parentID,
                siblingOrder: draft.siblingOrder,
                depth: depth,
                breadcrumb: breadcrumb,
                sensitivityPresentation: sensitivityPresentation,
                durationKind: structuralItem?.durationKind
                    ?? (draft.durationSeconds == nil ? .unknown : .exact),
                durationMinimumSeconds: structuralItem?.durationMinimumSeconds
                    ?? draft.durationSeconds,
                durationSeconds: draft.durationSeconds,
                durationMaximumSeconds: structuralItem?.durationMaximumSeconds
                    ?? draft.durationSeconds,
                durationSource: structuralItem?.durationSource
                    ?? (draft.durationSeconds == nil ? nil : .user),
                deadlineKind: structuralItem?.deadlineKind ?? inferredDeadlineKind,
                deadlineAt: draft.deadlineAt,
                deadlineDate: structuralItem?.deadlineDate,
                deadlineStrength: structuralItem?.deadlineStrength
                    ?? (inferredDeadlineKind == .none ? nil : .hard),
                deadlineSoftWeight: structuralItem?.deadlineSoftWeight,
                blockedReasonKind: structuralItem?.blockedReasonKind,
                blockedByItemID: structuralItem?.blockedByItemID,
                blockedReason: structuralItem?.blockedReason,
                dependencyCauses: CanonicalDependencyCatalog.causes(
                    for: draft,
                    ownerIsSensitive: sensitivityPresentation != .standard,
                    references: dependencyReferences,
                    reportedBlockerID: structuralItem?.blockedReasonKind == .dependency
                        ? structuralItem?.blockedByItemID
                        : nil
                ),
                hasOpaqueDependencies: hasOpaqueDependencies,
                source: source,
                syncState: syncState,
                mutationID: mutation?.id,
                revision: revision,
                activeCanonicalItem: activeCanonicalItem,
                isReadOnly: readOnly || hasOpaqueDependencies
                    || hasMissingParent || hasHierarchyCycle,
                hasMissingParent: hasMissingParent,
                hasHierarchyCycle: hasHierarchyCycle
            )
        }
    }

    struct Hierarchy {
        let orderedIDs: [UUID]
        let depthByID: [UUID: Int]
        let breadcrumbByID: [UUID: [String]]
        let sensitivityPresentationByID: [UUID: CanonicalSensitivityPresentation]
        let missingParentIDs: Set<UUID>
        let cyclicIDs: Set<UUID>

        init(nodes: [UUID: Node]) {
            func nodeOrder(_ left: Node, _ right: Node) -> Bool {
                if left.draft.siblingOrder != right.draft.siblingOrder {
                    return left.draft.siblingOrder < right.draft.siblingOrder
                }
                let titleOrder = left.draft.title.localizedStandardCompare(right.draft.title)
                if titleOrder != .orderedSame { return titleOrder == .orderedAscending }
                return left.itemID.uuidString < right.itemID.uuidString
            }

            var children: [UUID: [Node]] = [:]
            var roots: [Node] = []
            var missing = Set<UUID>()
            for node in nodes.values {
                guard let parentID = node.draft.parentID else {
                    roots.append(node)
                    continue
                }
                if parentID == node.itemID || nodes[parentID] == nil {
                    roots.append(node)
                    if nodes[parentID] == nil { missing.insert(node.itemID) }
                } else {
                    children[parentID, default: []].append(node)
                }
            }
            roots.sort(by: nodeOrder)
            for key in children.keys { children[key]?.sort(by: nodeOrder) }

            var ordered: [UUID] = []
            var depths: [UUID: Int] = [:]
            var breadcrumbs: [UUID: [String]] = [:]
            var sensitivities: [UUID: CanonicalSensitivityPresentation] = [:]
            var visited = Set<UUID>()
            var stack = roots.reversed().map {
                ($0, 0, [String](), missing.contains($0.itemID))
            }
            while let (node, depth, ancestors, ancestorIsSensitive) = stack.popLast() {
                guard visited.insert(node.itemID).inserted else { continue }
                ordered.append(node.itemID)
                depths[node.itemID] = depth
                breadcrumbs[node.itemID] = ancestors
                let presentation: CanonicalSensitivityPresentation = if node.draft.isSensitive {
                    .own
                } else if ancestorIsSensitive {
                    .inherited
                } else {
                    .standard
                }
                sensitivities[node.itemID] = presentation
                var nextAncestors = ancestors
                nextAncestors.append(node.draft.title)
                if nextAncestors.count > CanonicalInboxPresentation.maximumBreadcrumbDepth {
                    nextAncestors.removeFirst(
                        nextAncestors.count - CanonicalInboxPresentation.maximumBreadcrumbDepth
                    )
                }
                for child in (children[node.itemID] ?? []).reversed() {
                    stack.append((
                        child,
                        depth + 1,
                        nextAncestors,
                        presentation != .standard
                    ))
                }
            }

            var cycles = Set<UUID>()
            for start in nodes.values.sorted(by: nodeOrder) where !visited.contains(start.itemID) {
                var chain: [Node] = []
                var chainIndex: [UUID: Int] = [:]
                var current: Node? = start
                while let node = current, !visited.contains(node.itemID) {
                    if let cycleStart = chainIndex[node.itemID] {
                        cycles.formUnion(chain[cycleStart...].map(\.itemID))
                        break
                    }
                    chainIndex[node.itemID] = chain.count
                    chain.append(node)
                    current = node.draft.parentID.flatMap { nodes[$0] }
                }
                for node in chain {
                    guard visited.insert(node.itemID).inserted else { continue }
                    ordered.append(node.itemID)
                    depths[node.itemID] = 0
                    breadcrumbs[node.itemID] = []
                    sensitivities[node.itemID] = node.draft.isSensitive ? .own : .inherited
                }
            }

            orderedIDs = ordered
            depthByID = depths
            breadcrumbByID = breadcrumbs
            sensitivityPresentationByID = sensitivities
            missingParentIDs = missing
            cyclicIDs = cycles
        }
    }
}
