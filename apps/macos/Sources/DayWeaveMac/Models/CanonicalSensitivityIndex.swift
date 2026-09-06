import Foundation

/// A single-snapshot privacy projection for a complete canonical list. Resolve
/// every ancestry path once, rather than rebuilding and walking the graph for
/// every visible row. This does not grant editing or synchronization authority.
struct CanonicalSensitivityIndex: Sendable {
    private let presentations: [UUID: CanonicalSensitivityPresentation]

    subscript(itemID: UUID) -> CanonicalSensitivityPresentation {
        presentations[itemID] ?? .inherited
    }

    init(
        canonicalItems: [DayWeaveCanonicalItem],
        sensitivityMutations: [PendingCanonicalSensitivityMutation],
        authoringMutations: [DayWeavePendingCanonicalAuthoringMutation],
        trashEntries: [DayWeaveCanonicalTrashEntry]
    ) {
        var ownSensitivity: [UUID: Bool] = [:]
        var parents: [UUID: Set<UUID>] = [:]
        var ownPresentationIDs = Set<UUID>()

        func retain(itemID: UUID, isSensitive: Bool, parentID: UUID?) {
            ownSensitivity[itemID] = (ownSensitivity[itemID] ?? false) || isSensitive
            if isSensitive { ownPresentationIDs.insert(itemID) }
            if let parentID { parents[itemID, default: []].insert(parentID) }
        }

        for item in canonicalItems {
            retain(itemID: item.id, isSensitive: item.isSensitive, parentID: item.parentID)
        }
        for mutation in sensitivityMutations where mutation.requiresSensitivePresentation {
            retain(itemID: mutation.itemID, isSensitive: true, parentID: nil)
        }
        for mutation in authoringMutations {
            // Own marks remain visible even on an unsupported journal operation,
            // matching the existing single-item presentation boundary.
            if mutation.draft?.isSensitive == true || mutation.baseItem?.isSensitive == true {
                ownPresentationIDs.insert(mutation.itemID)
            }
            if let base = mutation.baseItem {
                retain(itemID: mutation.itemID, isSensitive: base.isSensitive, parentID: base.parentID)
            }
            guard mutation.operation == .create || mutation.operation == .replace,
                  let draft = mutation.draft else { continue }
            // Never discard old ancestry while a reparent/declassification is
            // pending. A node may have multiple protective parent paths.
            retain(itemID: mutation.itemID, isSensitive: draft.isSensitive, parentID: draft.parentID)
        }
        for entry in trashEntries where entry.isSensitive {
            ownPresentationIDs.insert(entry.id)
        }

        var unresolvedParentCounts: [UUID: Int] = [:]
        var children: [UUID: [UUID]] = [:]
        var ready: [UUID] = []
        for itemID in ownSensitivity.keys {
            let parentIDs = parents[itemID] ?? []
            unresolvedParentCounts[itemID] = parentIDs.count
            if parentIDs.isEmpty { ready.append(itemID) }
            for parentID in parentIDs {
                children[parentID, default: []].append(itemID)
            }
        }

        var effectiveSensitivity = ownSensitivity
        var cursor = 0
        while cursor < ready.count {
            let itemID = ready[cursor]
            cursor += 1
            for childID in children[itemID] ?? [] {
                if effectiveSensitivity[itemID] == true { effectiveSensitivity[childID] = true }
                let remaining = (unresolvedParentCounts[childID] ?? 0) - 1
                unresolvedParentCounts[childID] = remaining
                if remaining == 0 { ready.append(childID) }
            }
        }

        // Missing parents are never resolved; cycles and their descendants also
        // retain positive counts. All are sensitive, regardless of graph depth.
        var result: [UUID: CanonicalSensitivityPresentation] = [:]
        for itemID in ownSensitivity.keys {
            result[itemID] = effectiveSensitivity[itemID] == true
                || unresolvedParentCounts[itemID] != 0 ? .inherited : .standard
        }
        // Trash-only bodies do not become public roots or declassify missing
        // ancestry. Their retained own marks affect presentation only, as before.
        for itemID in ownPresentationIDs { result[itemID] = .own }
        presentations = result
    }
}
