import Foundation

/// Read-only recorded facts from a complete forest. Not scheduling permission,
/// elapsed/remaining work, weighted progress, or parent completion authority.
struct CanonicalHierarchyRollup: Equatable, Sendable {
    struct Estimate: Equatable, Sendable {
        let minimumSeconds: UInt64
        let expectedSeconds: UInt64
        let maximumSeconds: UInt64
    }

    struct Node: Equatable, Sendable {
        let id: UUID
        let parentID: UUID?
        let kind: DayWeaveCanonicalItemKind
        let status: DayWeaveCanonicalItemStatus
        let hasOwnEffort: Bool
        let recurs: Bool
        var hasChildrenOutsidePlan = false
        let estimate: Estimate?
        var isSensitive = false
        var hasUnsupportedMetadata = false
    }

    struct Summary: Equatable, Sendable {
        var completedLeafItems: UInt64 = 0
        var skippedLeafItems: UInt64 = 0
        var cancelledLeafItems: UInt64 = 0
        var openLeafItems: UInt64 = 0
        var recurringLeafItems: UInt64 = 0
        var fixedEvents: UInt64 = 0
        var unknownEstimates: UInt64 = 0
        var minimumEstimateSeconds: UInt64 = 0
        var expectedEstimateSeconds: UInt64 = 0
        var maximumEstimateSeconds: UInt64 = 0

        mutating func include(_ other: Self) -> Bool {
            let fields: [WritableKeyPath<Self, UInt64>] = [
                \.completedLeafItems, \.skippedLeafItems, \.cancelledLeafItems,
                \.openLeafItems, \.recurringLeafItems, \.fixedEvents, \.unknownEstimates,
                \.minimumEstimateSeconds, \.expectedEstimateSeconds, \.maximumEstimateSeconds
            ]
            for field in fields {
                let (sum, overflow) = self[keyPath: field].addingReportingOverflow(other[keyPath: field])
                guard !overflow, sum <= UInt64(Int64.max) else { return false }
                self[keyPath: field] = sum
            }
            return true
        }

        var leafDescription: String {
            "Leaf items · \(completedLeafItems) completed · \(openLeafItems) open · \(skippedLeafItems) skipped · \(cancelledLeafItems) cancelled"
        }

        var separateDescription: String {
            "\(recurringLeafItems) recurring leaf items · \(fixedEvents) one-off fixed events"
        }

        var estimateDescription: String {
            if expectedEstimateSeconds == 0 {
                return unknownEstimates > 0
                    ? "Recorded leaf effort · \(unknownEstimates) unknown estimates · no known estimates"
                    : "No recorded flexible leaf effort estimates."
            }
            if minimumEstimateSeconds == expectedEstimateSeconds && expectedEstimateSeconds == maximumEstimateSeconds {
                return "Known recorded leaf effort · \(Self.time(expectedEstimateSeconds)) exact · \(unknownEstimates) unknown estimates"
            }
            return "Known recorded leaf effort · \(Self.time(minimumEstimateSeconds)) min / \(Self.time(expectedEstimateSeconds)) expected / \(Self.time(maximumEstimateSeconds)) max · \(unknownEstimates) unknown estimates"
        }

        /// Format only after exact aggregation; never round each leaf upward.
        private static func time(_ seconds: UInt64) -> String {
            let hours = seconds / 3_600
            let minutes = (seconds % 3_600) / 60
            let remainder = seconds % 60
            var parts: [String] = []
            if hours > 0 { parts.append("\(hours)h") }
            if minutes > 0 { parts.append("\(minutes)m") }
            if remainder > 0 || parts.isEmpty { parts.append("\(remainder)s") }
            return parts.joined(separator: " ")
        }
    }

    enum Unavailability: Equatable, Sendable {
        case notHydrated, pendingChanges, storage, invalidForest, unsupportedMetadata, overflow

        var message: String {
            switch self {
            case .notHydrated: "Leaf summary unavailable until the complete item collection is synced."
            case .pendingChanges: "Leaf summary withheld while item changes await sync or review."
            case .storage: "Leaf summary unavailable while saved item storage needs attention."
            case .invalidForest: "Leaf summary unavailable because hierarchy data is incomplete or inconsistent."
            case .unsupportedMetadata: "Leaf summary unavailable because some recorded fields need a newer app."
            case .overflow: "Leaf summary unavailable because recorded totals exceed the supported range."
            }
        }
    }

    enum Presentation: Equatable, Sendable {
        case available(Summary), concealed, unavailable(Unavailability)

        var accessibilityDescription: String {
            switch self {
            case let .available(summary):
                "Saved-cache summary. \(summary.leafDescription). \(summary.separateDescription). \(summary.estimateDescription). Not remaining time or weighted progress."
            case .concealed: "Leaf summary hidden because this subtree contains protected items."
            case let .unavailable(reason): reason.message
            }
        }
    }

    private let summaries: [UUID: Summary]
    private let sensitiveSubtrees: Set<UUID>
    let unavailable: Unavailability?

    subscript(id: UUID) -> Presentation {
        if let unavailable { return .unavailable(unavailable) }
        if sensitiveSubtrees.contains(id) { return .concealed }
        guard let summary = summaries[id] else { return .unavailable(.notHydrated) }
        return .available(summary)
    }

    static func withheld(_ reason: Unavailability) -> Self {
        Self(summaries: [:], sensitiveSubtrees: [], unavailable: reason)
    }

    static func build(nodes: [Node]) -> Self {
        var byID: [UUID: Node] = [:]
        var children: [UUID: [UUID]] = [:]
        var roots: [UUID] = []
        for node in nodes {
            guard node.id != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
                  byID.updateValue(node, forKey: node.id) == nil,
                  !node.hasChildrenOutsidePlan else { return .withheld(.invalidForest) }
            if case .unknown = node.kind { return .withheld(.unsupportedMetadata) }
            if case .unknown = node.status { return .withheld(.unsupportedMetadata) }
            guard !node.hasUnsupportedMetadata else { return .withheld(.unsupportedMetadata) }
            if let estimate = node.estimate {
                guard estimate.minimumSeconds > 0,
                      estimate.minimumSeconds <= estimate.expectedSeconds,
                      estimate.expectedSeconds <= estimate.maximumSeconds,
                      estimate.maximumSeconds <= UInt64(Int64.max) else { return .withheld(.invalidForest) }
            }
        }
        for node in nodes {
            if let parent = node.parentID {
                guard byID[parent] != nil else { return .withheld(.invalidForest) }
                children[parent, default: []].append(node.id)
            } else { roots.append(node.id) }
        }
        // Top-down inheritance and reverse topological reduction are linear,
        // without a stack-depth limit or a per-node ancestor walk.
        var ordered = roots
        var recurring = Set<UUID>()
        var sensitive = Set<UUID>()
        var cursor = 0
        while cursor < ordered.count {
            let id = ordered[cursor]
            cursor += 1
            guard let node = byID[id] else { return .withheld(.invalidForest) }
            if node.recurs || node.parentID.map({ recurring.contains($0) }) == true {
                recurring.insert(id)
            }
            if node.isSensitive || node.parentID.map({ sensitive.contains($0) }) == true {
                sensitive.insert(id)
            }
            ordered.append(contentsOf: children[id] ?? [])
        }
        guard ordered.count == nodes.count else { return .withheld(.invalidForest) }
        var summaries: [UUID: Summary] = [:]
        for id in ordered.reversed() {
            guard let node = byID[id] else { return .withheld(.invalidForest) }
            let descendants = children[id] ?? []
            var summary = Summary()
            if !recurring.contains(id) && node.kind == .event { summary.fixedEvents = 1 }
            if descendants.isEmpty {
                if recurring.contains(id) { summary.recurringLeafItems = 1 }
                else {
                    switch node.status {
                    case .completed: summary.completedLeafItems = 1
                    case .skipped: summary.skippedLeafItems = 1
                    case .cancelled: summary.cancelledLeafItems = 1
                    default: summary.openLeafItems = 1
                    }
                    let contributesEffort: Bool = switch node.kind {
                    case .task, .habit, .breakTime: true
                    case .project, .goal, .routine: node.hasOwnEffort
                    case .event, .unknown: false
                    }
                    if contributesEffort {
                        if let estimate = node.estimate {
                            summary.minimumEstimateSeconds = estimate.minimumSeconds
                            summary.expectedEstimateSeconds = estimate.expectedSeconds
                            summary.maximumEstimateSeconds = estimate.maximumSeconds
                        } else { summary.unknownEstimates = 1 }
                    }
                }
            }
            for child in descendants {
                guard let contribution = summaries[child], summary.include(contribution) else {
                    return .withheld(.overflow)
                }
                if sensitive.contains(child) { sensitive.insert(id) }
            }
            summaries[id] = summary
        }
        return Self(summaries: summaries, sensitiveSubtrees: sensitive, unavailable: nil)
    }
}

/// View-owned, ephemeral cache. No journal, schedule block, clock, query, or
/// disclosure state is used as evidence of completion or estimated work.
@MainActor
final class CanonicalHierarchyRollupCache {
    private struct Key: Equatable {
        let items: [DayWeaveCanonicalItem]
        let authoring: [DayWeavePendingCanonicalAuthoringMutation]
        let statuses: [PendingCanonicalMutation]
        let sensitivity: [PendingCanonicalSensitivityMutation]
        let trash: [DayWeaveCanonicalTrashEntry]
        let tombstoneRevisions: [UUID: UInt64]
        let terminalOutcomes: [UUID: DayWeaveTerminalExecutionOutcome]
        let pendingExecutionCommand: DayWeavePendingExecutionCommand?
        let pendingProposal: DayWeavePendingProposalApplicationMutation?
        let binding: String?
        let cursor: String?
        let storageIsTrusted: Bool
    }
    private var key: Key?
    private var cached: CanonicalHierarchyRollup?
    private(set) var buildCount = 0

    func presentation(for store: PlannerStore) -> CanonicalHierarchyRollup {
        let next = Key(
            items: store.canonicalItems, authoring: store.pendingCanonicalAuthoringMutations,
            statuses: store.pendingCanonicalMutations, sensitivity: store.pendingCanonicalSensitivityMutations,
            trash: store.canonicalTrash, tombstoneRevisions: store.canonicalTombstoneRevisions,
            terminalOutcomes: store.executionState.terminalOutcomes,
            pendingExecutionCommand: store.executionState.pendingCommand,
            pendingProposal: store.pendingProposalApplicationMutation,
            binding: store.canonicalConfigurationIdentifier,
            cursor: store.canonicalDeltaCursor,
            storageIsTrusted: store.canPersistPlan && store.persistenceError == nil
        )
        if next == key, let cached { return cached }
        let result: CanonicalHierarchyRollup
        if !next.storageIsTrusted { result = .withheld(.storage) }
        else if next.binding?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false
            || next.cursor?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false {
            result = .withheld(.notHydrated)
        }
        else if !next.authoring.isEmpty || !next.statuses.isEmpty
            || next.pendingExecutionCommand != nil || next.pendingProposal != nil
            || Self.hasUnacknowledgedExecutionReceipt(next)
            || next.terminalOutcomes.values.contains(where: {
                switch $0.projection {
                case .pending, .retryAuthorized, .conflicted: true
                case .notRequired, .applied, .keptLatest: false
                }
            }) { result = .withheld(.pendingChanges) }
        else {
            let sensitivity = store.canonicalSensitivityPresentationIndex()
            result = .build(nodes: next.items.filter { $0.deletedAt == nil }.map { item in
                let estimate: CanonicalHierarchyRollup.Estimate?
                var unsupported = !item.unsupportedFields.isEmpty
                if case .unsupported = item.durationSource { unsupported = true }
                switch item.durationKind {
                case .unknown:
                    estimate = nil
                    unsupported = unsupported || item.durationMinimumSeconds != nil
                        || item.durationSeconds != nil || item.durationMaximumSeconds != nil
                        || item.durationSource != nil
                case .exact:
                    if let seconds = item.durationSeconds,
                       item.durationMinimumSeconds == seconds, item.durationMaximumSeconds == seconds,
                       item.durationSource != nil {
                        estimate = .init(minimumSeconds: UInt64(seconds), expectedSeconds: UInt64(seconds),
                                         maximumSeconds: UInt64(seconds))
                    } else { estimate = nil; unsupported = true }
                case .range:
                    if let minimum = item.durationMinimumSeconds, let expected = item.durationSeconds,
                       let maximum = item.durationMaximumSeconds, minimum < maximum,
                       item.durationSource != nil {
                        estimate = .init(minimumSeconds: UInt64(minimum), expectedSeconds: UInt64(expected),
                                         maximumSeconds: UInt64(maximum))
                    } else { estimate = nil; unsupported = true }
                case .unsupported: estimate = nil; unsupported = true
                }
                return .init(
                    id: item.id, parentID: item.parentID, kind: item.kind, status: item.status,
                    hasOwnEffort: item.hasOwnEffort, recurs: item.recurrence != nil, estimate: estimate,
                    isSensitive: sensitivity[item.id] != .standard, hasUnsupportedMetadata: unsupported
                )
            })
        }
        key = next
        cached = result
        buildCount += 1
        return result
    }

    func clear() { key = nil; cached = nil }

    private static func hasUnacknowledgedExecutionReceipt(_ key: Key) -> Bool {
        let receipts = key.terminalOutcomes.values.compactMap { outcome -> (UUID, UInt64)? in
            guard case let .applied(revision) = outcome.projection else { return nil }
            return (outcome.session.itemID, revision)
        }
        guard !receipts.isEmpty else { return false }
        var revisions = key.tombstoneRevisions
        for item in key.items { revisions[item.id] = max(revisions[item.id] ?? 0, item.revision) }
        for item in key.trash { revisions[item.id] = max(revisions[item.id] ?? 0, item.revision) }
        // A complete collection rebuild can legitimately prune an old deleted
        // identity's tombstone while the lifetime execution receipt survives.
        // Only contradictory retained evidence is a known cache gap; absence
        // is not a promise that this saved-cache projection is latest server state.
        return receipts.contains { receipt in
            revisions[receipt.0].map { $0 < receipt.1 } ?? false
        }
    }
}
