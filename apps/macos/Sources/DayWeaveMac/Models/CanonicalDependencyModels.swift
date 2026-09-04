import Foundation

enum CanonicalDependencyRelation: String, CaseIterable, Identifiable, Sendable {
    case finishToStart = "finish_to_start"
    case startToStart = "start_to_start"
    case finishToFinish = "finish_to_finish"
    case startToFinish = "start_to_finish"

    var id: Self { self }

    var shortTitle: String {
        switch self {
        case .finishToStart: "Finish → start"
        case .startToStart: "Start → start"
        case .finishToFinish: "Finish → finish"
        case .startToFinish: "Start → finish"
        }
    }

    var guidance: String {
        switch self {
        case .finishToStart:
            "This item starts after the predecessor finishes."
        case .startToStart:
            "This item starts after the predecessor starts."
        case .finishToFinish:
            "This item finishes after the predecessor finishes."
        case .startToFinish:
            "This item finishes after the predecessor starts."
        }
    }

    var compactRequirement: String {
        switch self {
        case .finishToStart: "finish before this starts"
        case .startToStart: "start before this starts"
        case .finishToFinish: "finish before this finishes"
        case .startToFinish: "start before this finishes"
        }
    }
}

enum CanonicalDependencyStrength: Equatable, Sendable {
    case hard
    case soft(weight: UInt32)

    var isHard: Bool {
        if case .hard = self { return true }
        return false
    }

    var title: String {
        switch self {
        case .hard: "Required"
        case let .soft(weight): "Preferred · weight \(weight)"
        }
    }

    var jsonValue: JSONValue {
        switch self {
        case .hard:
            .object(["level": .string("hard")])
        case let .soft(weight):
            .object([
                "level": .string("soft"),
                "weight": .number(JSONNumber(UInt64(weight))),
            ])
        }
    }

    init?(jsonValue: JSONValue) {
        guard case let .object(object) = jsonValue,
              case let .string(level)? = object["level"] else { return nil }
        switch level {
        case "hard" where Set(object.keys) == ["level"]:
            self = .hard
        case "soft" where Set(object.keys) == ["level", "weight"]:
            guard case let .number(number)? = object["weight"],
                  let weight = number.exactUInt32,
                  weight <= 1_000_000 else { return nil }
            self = .soft(weight: weight)
        default:
            return nil
        }
    }
}

struct CanonicalDependencyEdge: Equatable, Sendable {
    static let maximumLagMinutes: UInt32 = 527_040

    let predecessorID: UUID
    let relation: CanonicalDependencyRelation
    let minimumLagMinutes: UInt32
    let strength: CanonicalDependencyStrength

    var jsonValue: JSONValue {
        .object([
            "item_id": .string(predecessorID.uuidString.lowercased()),
            "relation": .string(relation.rawValue),
            "minimum_lag": .number(JSONNumber(UInt64(minimumLagMinutes))),
            "strength": strength.jsonValue,
        ])
    }

    init(
        predecessorID: UUID,
        relation: CanonicalDependencyRelation,
        minimumLagMinutes: UInt32,
        strength: CanonicalDependencyStrength
    ) {
        self.predecessorID = predecessorID
        self.relation = relation
        self.minimumLagMinutes = minimumLagMinutes
        self.strength = strength
    }

    init?(jsonValue: JSONValue) {
        guard case let .object(object) = jsonValue,
              Set(object.keys) == ["item_id", "relation", "minimum_lag", "strength"],
              case let .string(rawID)? = object["item_id"],
              let predecessorID = UUID(uuidString: rawID),
              predecessorID != Self.nilID,
              case let .string(rawRelation)? = object["relation"],
              let relation = CanonicalDependencyRelation(rawValue: rawRelation),
              case let .number(number)? = object["minimum_lag"],
              let minimumLagMinutes = number.exactUInt32,
              minimumLagMinutes <= Self.maximumLagMinutes,
              let rawStrength = object["strength"],
              let strength = CanonicalDependencyStrength(jsonValue: rawStrength) else {
            return nil
        }
        self.init(
            predecessorID: predecessorID,
            relation: relation,
            minimumLagMinutes: minimumLagMinutes,
            strength: strength
        )
    }

    static let nilID = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))

    static func decodeArray(_ value: JSONValue) -> [Self]? {
        guard case let .array(values) = value else { return nil }
        var seen = Set<UUID>()
        var result: [Self] = []
        result.reserveCapacity(values.count)
        for value in values {
            guard let edge = Self(jsonValue: value),
                  seen.insert(edge.predecessorID).inserted else { return nil }
            result.append(edge)
        }
        return result
    }

    static func decode(fromFlexibleConstraints value: JSONValue) -> [Self]? {
        guard case let .object(root) = value else { return nil }
        guard let constraints = root["constraints"] else { return [] }
        guard case let .object(object) = constraints else { return nil }
        guard let dependencies = object["dependencies"] else { return [] }
        return decodeArray(dependencies)
    }

    /// Mirrors the server's authoritative graph projection: dependency edges
    /// are ordered by predecessor UUID, an empty dependency member is omitted,
    /// and an otherwise-empty `constraints` wrapper is removed.
    static func canonicalizedFlexibleConstraints(_ value: JSONValue) -> JSONValue? {
        guard case var .object(root) = value else { return nil }
        guard let rawConstraints = root["constraints"] else { return value }
        guard case var .object(constraints) = rawConstraints else { return nil }

        if let rawDependencies = constraints.removeValue(forKey: "dependencies") {
            guard let dependencies = decodeArray(rawDependencies) else { return nil }
            let ordered = dependencies.sorted {
                $0.predecessorID.uuidString.lowercased()
                    < $1.predecessorID.uuidString.lowercased()
            }
            if !ordered.isEmpty {
                constraints["dependencies"] = .array(ordered.map(\.jsonValue))
            }
        }

        if constraints.isEmpty {
            root.removeValue(forKey: "constraints")
        } else {
            root["constraints"] = .object(constraints)
        }
        return .object(root)
    }

    /// Splits the portable aggregate into the two independently reviewed
    /// proposal fields emitted by the server.
    static func proposalProjection(
        fromFlexibleConstraints value: JSONValue
    ) -> (metadata: JSONValue, dependencies: JSONValue)? {
        guard case var .object(root) = canonicalizedFlexibleConstraints(value) else {
            return nil
        }
        guard case var .object(constraints)? = root["constraints"] else {
            return (metadata: .object(root), dependencies: .array([]))
        }
        let dependencies = constraints.removeValue(forKey: "dependencies") ?? .array([])
        guard decodeArray(dependencies) != nil else { return nil }
        if constraints.isEmpty {
            root.removeValue(forKey: "constraints")
        } else {
            root["constraints"] = .object(constraints)
        }
        return (metadata: .object(root), dependencies: dependencies)
    }
}

struct CanonicalDependencyReference: Identifiable, Equatable, Sendable {
    let id: UUID
    let title: String
    let kind: DayWeaveCanonicalItemKind
    let status: DayWeaveCanonicalItemStatus
    let isSensitive: Bool
    let isAvailable: Bool
    let hasOpaqueDependencies: Bool

    var identifierHint: String { String(id.uuidString.lowercased().prefix(8)) }
    var isSelectableDependencyCandidate: Bool {
        isAvailable && !hasOpaqueDependencies
    }
}

struct CanonicalDependencyCause: Identifiable, Equatable, Sendable {
    let predecessorID: UUID
    let title: String
    let relation: CanonicalDependencyRelation?
    let minimumLagMinutes: UInt32
    let strength: CanonicalDependencyStrength
    let predecessorStatus: DayWeaveCanonicalItemStatus?
    let isSensitive: Bool
    let isTitleRedacted: Bool
    let isAvailable: Bool
    let isReportedBlocker: Bool

    var id: UUID { predecessorID }
    var isSatisfied: Bool { predecessorStatus == .completed }
    var isBlocking: Bool { isReportedBlocker || (strength.isHard && !isSatisfied) }

    var requirementDescription: String {
        guard let relation else { return "Reported as the blocking dependency." }
        let lag = minimumLagMinutes == 0
            ? ""
            : " plus \(Self.minuteDescription(minimumLagMinutes)) lag"
        return "Must \(relation.compactRequirement)\(lag)."
    }

    var statusDescription: String {
        guard isAvailable else { return "Predecessor is unavailable" }
        if isSatisfied { return "Satisfied" }
        guard let predecessorStatus else { return "Status unavailable" }
        switch predecessorStatus {
        case .inProgress: return "In progress"
        case .completed: return "Satisfied"
        case .blocked: return "Predecessor is blocked"
        case .skipped: return "Predecessor was skipped"
        case .cancelled: return "Predecessor was cancelled"
        case .inbox: return "Predecessor is in Inbox"
        case .planned: return "Predecessor is planned"
        case .scheduled: return "Predecessor is scheduled"
        case .paused: return "Predecessor is paused"
        case .unknown: return "Predecessor status requires a newer DayWeave version"
        }
    }

    private static func minuteDescription(_ minutes: UInt32) -> String {
        if minutes.isMultiple(of: 60) { return "\(minutes / 60)h" }
        return "\(minutes)m"
    }
}

enum CanonicalDependencyCatalog {
    private struct ProjectedItem {
        let id: UUID
        let draft: DayWeaveCanonicalItemDraft
        let deleted: Bool
        let ownSensitivity: Bool

        var dependencies: [CanonicalDependencyEdge]? {
            CanonicalDependencyEdge.decode(
                fromFlexibleConstraints: draft.flexibleConstraints
            )
        }

        var hasOpaqueDependencies: Bool { dependencies == nil }
    }

    private struct DependencyGraphProjection {
        let explicitDependenciesByItem: [UUID: Set<UUID>]
        let knownDependenciesByItem: [UUID: Set<UUID>]
        let opaqueItemIDs: Set<UUID>
        let recurringOwnersByItem: [UUID: RecurringOwner]

        func dependencyPath(from start: UUID, to target: UUID) -> DependencyPath {
            var pending = [start]
            var visited = Set<UUID>()
            var reachedOpaqueItem = false
            while let itemID = pending.popLast() {
                if itemID == target { return .present }
                guard visited.insert(itemID).inserted else { continue }
                if opaqueItemIDs.contains(itemID) { reachedOpaqueItem = true }
                pending.append(contentsOf: knownDependenciesByItem[itemID] ?? [])
            }
            return reachedOpaqueItem ? .unknown : .absent
        }

        var recurringBoundarySafety: RecurringBoundarySafety {
            var hasUnprovenEdge = false
            for (successorID, predecessorIDs) in explicitDependenciesByItem {
                for predecessorID in predecessorIDs {
                    switch recurringBoundarySafety(
                        successorID: successorID,
                        predecessorID: predecessorID
                    ) {
                    case .crossBoundary:
                        return .crossBoundary
                    case .unproven:
                        hasUnprovenEdge = true
                    case .safe:
                        break
                    }
                }
            }
            return hasUnprovenEdge ? .unproven : .safe
        }

        func recurringBoundarySafety(
            successorID: UUID,
            predecessorID: UUID
        ) -> RecurringBoundarySafety {
            switch recurringOwnersByItem[predecessorID] ?? .unknown {
            case .none:
                return .safe
            case .unknown:
                return .unproven
            case let .known(predecessorOwner):
                switch recurringOwnersByItem[successorID] ?? .unknown {
                case .none:
                    return .crossBoundary
                case .unknown:
                    return .unproven
                case let .known(successorOwner):
                    return successorOwner == predecessorOwner ? .safe : .crossBoundary
                }
            }
        }
    }

    private enum DependencyPath: Equatable {
        case absent
        case present
        case unknown
    }

    private enum RecurringOwner: Equatable {
        case none
        case known(UUID)
        case unknown
    }

    private enum RecurringBoundarySafety {
        case safe
        case crossBoundary
        case unproven
    }

    static func references(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry] = [],
        excluding itemID: UUID? = nil,
        sensitivity: ((UUID) -> Bool)? = nil
    ) -> [CanonicalDependencyReference] {
        projectedItems(
            canonicalItems: canonicalItems,
            pendingMutations: pendingMutations,
            trashEntries: trashEntries
        ).values.compactMap { item in
            guard item.id != itemID else { return nil }
            return CanonicalDependencyReference(
                id: item.id,
                title: item.draft.title,
                kind: item.draft.kind,
                status: item.draft.status,
                isSensitive: sensitivity?(item.id) ?? item.ownSensitivity,
                isAvailable: !item.deleted,
                hasOpaqueDependencies: item.hasOpaqueDependencies
            )
        }.sorted { left, right in
            if left.isAvailable != right.isAvailable { return left.isAvailable }
            let titleOrder = left.title.localizedStandardCompare(right.title)
            if titleOrder != .orderedSame { return titleOrder == .orderedAscending }
            return left.id.uuidString < right.id.uuidString
        }
    }

    static func causes(
        for draft: DayWeaveCanonicalItemDraft,
        ownerIsSensitive: Bool,
        references: [CanonicalDependencyReference],
        reportedBlockerID: UUID? = nil
    ) -> [CanonicalDependencyCause] {
        let dependencies = CanonicalDependencyEdge.decode(
            fromFlexibleConstraints: draft.flexibleConstraints
        ) ?? []
        let byID = Dictionary(uniqueKeysWithValues: references.map { ($0.id, $0) })
        var causes = dependencies.map { dependency in
            let reference = byID[dependency.predecessorID]
            let redact = reference?.isSensitive == true && !ownerIsSensitive
            let title: String
            if redact {
                let hint = reference?.identifierHint
                    ?? String(dependency.predecessorID.uuidString.lowercased().prefix(8))
                title = "Sensitive item \(hint)"
            } else if let reference {
                title = reference.title
            } else {
                title = "Unavailable item \(dependency.predecessorID.uuidString.lowercased().prefix(8))"
            }
            return CanonicalDependencyCause(
                predecessorID: dependency.predecessorID,
                title: title,
                relation: dependency.relation,
                minimumLagMinutes: dependency.minimumLagMinutes,
                strength: dependency.strength,
                predecessorStatus: reference?.status,
                isSensitive: reference?.isSensitive ?? true,
                isTitleRedacted: redact || reference == nil,
                isAvailable: reference?.isAvailable ?? false,
                isReportedBlocker: dependency.predecessorID == reportedBlockerID
            )
        }
        if let reportedBlockerID,
           !causes.contains(where: { $0.predecessorID == reportedBlockerID }) {
            let reference = byID[reportedBlockerID]
            let redact = reference?.isSensitive == true && !ownerIsSensitive
            let title: String
            if redact {
                let hint = reference?.identifierHint
                    ?? String(reportedBlockerID.uuidString.lowercased().prefix(8))
                title = "Sensitive item \(hint)"
            } else if let reference {
                title = reference.title
            } else {
                title = "Unavailable item \(reportedBlockerID.uuidString.lowercased().prefix(8))"
            }
            causes.append(CanonicalDependencyCause(
                predecessorID: reportedBlockerID,
                title: title,
                relation: nil,
                minimumLagMinutes: 0,
                strength: .hard,
                predecessorStatus: reference?.status,
                isSensitive: reference?.isSensitive ?? true,
                isTitleRedacted: redact || reference == nil,
                isAvailable: reference?.isAvailable ?? false,
                isReportedBlocker: true
            ))
        }
        return causes
    }

    static func cycleWarning(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry] = [],
        replacing itemID: UUID,
        with draft: DayWeaveCanonicalItemDraft
    ) -> String? {
        let currentItems = projectedItems(
            canonicalItems: canonicalItems,
            pendingMutations: pendingMutations,
            trashEntries: trashEntries
        )
        var items = currentItems
        items[itemID] = .init(
            id: itemID,
            draft: draft,
            deleted: false,
            ownSensitivity: draft.isSensitive
        )

        let previous = dependencyGraphProjection(currentItems)
        let proposed = dependencyGraphProjection(items)
        switch proposed.recurringBoundarySafety {
        case .crossBoundary:
            return "A recurring predecessor can only be linked from within the same recurring subtree."
        case .unproven:
            return "Dependency recurrence safety cannot be verified because related hierarchy metadata is unavailable."
        case .safe:
            break
        }
        if proposed.knownDependenciesByItem.hasCycle {
            return "These dependencies would create a cycle. Remove or change a predecessor before saving."
        }
        for (successor, predecessors) in proposed.knownDependenciesByItem {
            let previousPredecessors = previous.knownDependenciesByItem[successor] ?? []
            for predecessor in predecessors.subtracting(previousPredecessors)
                where proposed.dependencyPath(from: predecessor, to: successor) == .unknown {
                return "Dependency safety cannot be verified because a related item uses newer metadata."
            }
        }
        return nil
    }

    static func recurringBoundaryCandidateWarning(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry] = [],
        replacing itemID: UUID,
        with draft: DayWeaveCanonicalItemDraft,
        predecessorID: UUID
    ) -> String? {
        recurringBoundaryCandidateWarnings(
            canonicalItems: canonicalItems,
            pendingMutations: pendingMutations,
            trashEntries: trashEntries,
            replacing: itemID,
            with: draft,
            predecessorIDs: [predecessorID]
        )[predecessorID]
    }

    static func recurringBoundaryCandidateWarnings(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry] = [],
        replacing itemID: UUID,
        with draft: DayWeaveCanonicalItemDraft,
        predecessorIDs: [UUID]
    ) -> [UUID: String] {
        var items = projectedItems(
            canonicalItems: canonicalItems,
            pendingMutations: pendingMutations,
            trashEntries: trashEntries
        )
        items[itemID] = .init(
            id: itemID,
            draft: draft,
            deleted: false,
            ownSensitivity: draft.isSensitive
        )
        let graph = dependencyGraphProjection(items)
        var warnings: [UUID: String] = [:]
        for predecessorID in predecessorIDs {
            switch graph.recurringBoundarySafety(
                successorID: itemID,
                predecessorID: predecessorID
            ) {
            case .safe:
                break
            case .crossBoundary:
                warnings[predecessorID] =
                    "A recurring predecessor can only be linked from within the same recurring subtree."
            case .unproven:
                warnings[predecessorID] =
                    "Dependency recurrence safety cannot be verified because related hierarchy metadata is unavailable."
            }
        }
        return warnings
    }

    private static func dependencyGraphProjection(
        _ items: [UUID: ProjectedItem]
    ) -> DependencyGraphProjection {
        var explicitDependenciesByItem = Dictionary(
            uniqueKeysWithValues: items.keys.map { ($0, Set<UUID>()) }
        )
        var opaqueItemIDs = Set<UUID>()
        for (successorID, item) in items {
            guard let dependencies = item.dependencies else {
                opaqueItemIDs.insert(successorID)
                continue
            }
            explicitDependenciesByItem[successorID] = Set(
                dependencies.map(\.predecessorID)
            )
        }
        var dependenciesByItem = explicitDependenciesByItem
        for (successorID, predecessorIDs) in explicitDependenciesByItem {
            dependenciesByItem[successorID] = Set(predecessorIDs.filter { items[$0] != nil })
        }

        for routine in items.values where !routine.deleted
            && routine.draft.kind == .routine
            && routine.draft.flexibleConstraints.canonicalRoutineIsOrdered {
            let children = items.values.filter {
                !$0.deleted && $0.draft.parentID == routine.id
            }.sorted {
                if $0.draft.siblingOrder != $1.draft.siblingOrder {
                    return $0.draft.siblingOrder < $1.draft.siblingOrder
                }
                return $0.id.uuidString < $1.id.uuidString
            }
            for pair in zip(children, children.dropFirst()) {
                dependenciesByItem[pair.1.id, default: []].insert(pair.0.id)
            }
        }
        return .init(
            explicitDependenciesByItem: explicitDependenciesByItem,
            knownDependenciesByItem: dependenciesByItem,
            opaqueItemIDs: opaqueItemIDs,
            recurringOwnersByItem: recurringOwners(items)
        )
    }

    private static func recurringOwners(
        _ items: [UUID: ProjectedItem]
    ) -> [UUID: RecurringOwner] {
        var resolved: [UUID: RecurringOwner] = [:]
        for start in items.keys where resolved[start] == nil {
            var path: [UUID] = []
            var visiting = Set<UUID>()
            var current: UUID? = start
            var owner: RecurringOwner?
            while owner == nil {
                guard let itemID = current else {
                    owner = RecurringOwner.none
                    break
                }
                if let cachedOwner = resolved[itemID] {
                    owner = cachedOwner
                    break
                }
                guard visiting.insert(itemID).inserted,
                      let item = items[itemID] else {
                    owner = .unknown
                    break
                }
                path.append(itemID)
                current = item.draft.parentID
            }
            var inheritedOwner = owner ?? .unknown
            for itemID in path.reversed() {
                if inheritedOwner == .none, items[itemID]?.draft.recurrence != nil {
                    inheritedOwner = .known(itemID)
                }
                resolved[itemID] = inheritedOwner
            }
        }
        return resolved
    }

    private static func projectedItems(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry]
    ) -> [UUID: ProjectedItem] {
        var items: [UUID: ProjectedItem] = [:]
        for item in canonicalItems {
            items[item.id] = .init(
                id: item.id,
                draft: .init(item: item),
                deleted: item.deletedAt != nil,
                ownSensitivity: item.isSensitive
            )
        }
        for entry in trashEntries where items[entry.id] == nil {
            guard let item = entry.lastKnownItem else { continue }
            items[item.id] = .init(
                id: item.id,
                draft: .init(item: item),
                deleted: true,
                ownSensitivity: item.isSensitive
            )
        }
        for mutation in pendingMutations {
            switch mutation.operation {
            case .create, .replace:
                guard let draft = mutation.draft else { continue }
                items[mutation.itemID] = .init(
                    id: mutation.itemID,
                    draft: draft,
                    deleted: false,
                    ownSensitivity: draft.isSensitive
                )
            case .trash:
                guard let existing = items[mutation.itemID] else { continue }
                items[mutation.itemID] = .init(
                    id: existing.id,
                    draft: existing.draft,
                    deleted: true,
                    ownSensitivity: existing.ownSensitivity
                )
            case .restore:
                guard let existing = items[mutation.itemID] else { continue }
                items[mutation.itemID] = .init(
                    id: existing.id,
                    draft: existing.draft,
                    deleted: false,
                    ownSensitivity: existing.ownSensitivity
                )
            }
        }
        return items
    }
}

private extension Dictionary where Key == UUID, Value == Set<UUID> {
    var hasCycle: Bool {
        var visiting = Set<UUID>()
        var visited = Set<UUID>()
        func visit(_ itemID: UUID) -> Bool {
            if visiting.contains(itemID) { return true }
            guard visited.insert(itemID).inserted else { return false }
            visiting.insert(itemID)
            if (self[itemID] ?? []).contains(where: visit) { return true }
            visiting.remove(itemID)
            return false
        }
        return keys.contains(where: visit)
    }
}

private extension JSONValue {
    var canonicalRoutineIsOrdered: Bool {
        guard case let .object(root) = self,
              case let .bool(value)? = root["routine_ordered"] else { return false }
        return value
    }
}
