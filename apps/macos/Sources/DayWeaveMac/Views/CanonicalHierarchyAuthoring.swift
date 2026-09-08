import Foundation

@MainActor
final class CanonicalHierarchyAuthoringCache {
    private struct Key: Equatable {
        let items: [DayWeaveCanonicalItem]
        let mutations: [DayWeavePendingCanonicalAuthoringMutation]
        let statusMutations: [PendingCanonicalMutation]
        let sensitivityMutations: [PendingCanonicalSensitivityMutation]
        let trash: [DayWeaveCanonicalTrashEntry]
        let binding: String?
        let activeItemID: UUID?
        let hasPendingExecutionCommand: Bool
    }
    private var key: Key?
    private var cached: Set<UUID> = []
    private(set) var buildCount = 0

    func eligibleParentIDs(for store: PlannerStore) -> Set<UUID> {
        let next = Key(items: store.canonicalItems, mutations: store.pendingCanonicalAuthoringMutations,
            statusMutations: store.pendingCanonicalMutations, sensitivityMutations: store.pendingCanonicalSensitivityMutations,
            trash: store.canonicalTrash, binding: store.canonicalConfigurationIdentifier,
            activeItemID: store.executionState.activeSession?.itemID,
            hasPendingExecutionCommand: store.executionState.pendingCommand != nil)
        if key == next { return cached }
        cached = store.canonicalAuthoringEligibleParentIDs()
        key = next
        buildCount += 1
        return cached
    }

    func clear() { key = nil; cached = [] }
}

@MainActor
enum CanonicalHierarchyAuthoring {
    static func route(kind: DayWeaveCanonicalItemKind, parentID: UUID? = nil,
                      store: PlannerStore, itemID: UUID = UUID()) -> CanonicalInboxEditorRoute? {
        guard store.canMutatePlan else { return nil }
        if let parentID, !store.canonicalAuthoringEligibleParentIDs().contains(parentID) { return nil }
        let draft = DayWeaveCanonicalItemDraft(kind: kind, status: .inbox, title: "",
            timezoneName: store.scheduleProfile.timezoneName, parentID: parentID)
        return CanonicalInboxEditorRoute(mode: .createHierarchy(itemID: itemID, draft: draft,
            sensitiveContext: parentID.map { store.canonicalItemRequiresSensitivePresentation(itemID: $0) } ?? false))
    }
}

extension PlannerStore {
    /// Batch candidate projection for the picker and hierarchy buttons. Invalid
    /// ancestry propagates once, without imposing a depth cap or filtering the
    /// lifecycle of otherwise coherent ancestors.
    func canonicalAuthoringEligibleParentIDs() -> Set<UUID> {
        guard executionState.pendingCommand == nil else { return [] }
        var nodes = Dictionary(uniqueKeysWithValues: (canonicalConfigurationIdentifier == nil ? [] : canonicalItems)
            .filter { $0.deletedAt == nil }.map { ($0.id, DayWeaveCanonicalItemDraft(item: $0)) })
        let canonicalByID = Dictionary(uniqueKeysWithValues: canonicalItems.map { ($0.id, $0) })
        for mutation in pendingCanonicalAuthoringMutations {
            let coherent: Bool
            switch mutation.operation {
            case .create: coherent = canonicalByID[mutation.itemID] == nil
            case .replace: coherent = canonicalConfigurationIdentifier != nil
                && canonicalByID[mutation.itemID] == mutation.baseItem
                && canonicalByID[mutation.itemID]?.revision == mutation.expectedRevision
            case .trash, .restore: coherent = false
            }
            if coherent, mutation.disposition == .pending, !mutation.hasBeenSubmitted,
               mutation.configurationIdentifier == nil, let draft = mutation.draft,
               draft.validationIssue(itemID: mutation.itemID) == nil {
                nodes[mutation.itemID] = draft
            } else {
                nodes.removeValue(forKey: mutation.itemID)
            }
        }
        for id in canonicalTrash.map(\.id) + pendingCanonicalMutations.map(\.itemID)
            + pendingCanonicalSensitivityMutations.map(\.itemID)
            + [executionState.activeSession?.itemID].compactMap({ $0 }) {
            nodes.removeValue(forKey: id)
        }
        var children: [UUID: [UUID]] = [:]
        var queue: [UUID] = []
        for (id, node) in nodes {
            if let parentID = node.parentID { children[parentID, default: []].append(id) }
            else { queue.append(id) }
        }
        var admitted: Set<UUID> = []
        var index = 0
        while index < queue.count {
            let id = queue[index]
            index += 1
            guard admitted.insert(id).inserted else { continue }
            queue.append(contentsOf: children[id] ?? [])
        }
        return admitted.filter { id in
            guard let status = nodes[id]?.status else { return false }
            return status == .inbox || status == .planned || status == .blocked
        }
    }
}
