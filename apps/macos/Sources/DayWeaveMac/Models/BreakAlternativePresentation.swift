import Foundation

struct BreakAlternativeHandoffSource: Equatable, Sendable {
    let sessionID: UUID
    let sessionRevision: UInt64
    let itemID: UUID
    let itemRevision: UInt64
    let occurrenceID: UUID?

    init(session: DayWeaveExecutionSession) {
        sessionID = session.id
        sessionRevision = session.revision
        itemID = session.itemID
        itemRevision = session.itemRevision
        occurrenceID = session.occurrenceID
    }

    var version: DayWeaveExecutionSessionVersion {
        .init(sessionID: sessionID, revision: sessionRevision)
    }

    func matches(_ session: DayWeaveExecutionSession) -> Bool {
        session.id == sessionID
            && session.revision == sessionRevision
            && session.itemID == itemID
            && session.itemRevision == itemRevision
            && session.occurrenceID == occurrenceID
            && session.status == .paused
    }
}

struct BreakAlternativeCandidate: Identifiable, Equatable, Sendable {
    let block: ScheduleBlock
    let placementReason: String?
    let isNextInPlan: Bool

    var id: UUID { block.id }
}

struct BreakAlternativePresentation: Equatable, Sendable {
    static let emptyGuidance =
        "Your current item remains paused. Move it later, complete it, or skip it before another item can start."
    static let selectionGuidance =
        "Selecting an item only highlights it. Your current paused session must be moved later, completed, or skipped before Start becomes available."

    let source: BreakAlternativeHandoffSource
    let candidates: [BreakAlternativeCandidate]
    let selectedCandidateID: UUID?
}

/// Builds a presentation-only handoff from exact publication and execution
/// evidence. It deliberately ignores the one open paused lease only when
/// assessing otherwise-startable alternatives; that lease remains the
/// canonical Start blocker until the owner resolves it separately.
@MainActor
enum BreakAlternativePolicy {
    static func presentation(
        source: BreakAlternativeHandoffSource,
        selectedCandidateID: UUID?,
        planner: PlannerStore
    ) -> BreakAlternativePresentation? {
        guard let active = planner.executionState.activeSession,
              source.matches(active),
              planner.executionState.acknowledgedExpiredPause == source.version else {
            return nil
        }

        let candidates = eligibleBlocks(source: source, planner: planner)
            .enumerated()
            .map { index, block in
                BreakAlternativeCandidate(
                    block: block,
                    placementReason: safePlacementReason(block.placementReason),
                    isNextInPlan: index == 0
                )
            }
        let candidateIDs = Set(candidates.map(\.id))
        return .init(
            source: source,
            candidates: candidates,
            selectedCandidateID: selectedCandidateID.flatMap {
                candidateIDs.contains($0) ? $0 : nil
            }
        )
    }

    private static func eligibleBlocks(
        source: BreakAlternativeHandoffSource,
        planner: PlannerStore
    ) -> [ScheduleBlock] {
        guard planner.canMutatePlan,
              planner.executionState.historyVerified,
              planner.executionState.pendingCommand == nil,
              planner.pendingCanonicalMutations.isEmpty,
              planner.pendingProposalApplicationMutation == nil,
              planner.googleOutboundRecoveryJournal == nil,
              !planner.hasGoogleSchedulePublicationAuthorityFence,
              planner.pendingSchedulePublication == nil,
              planner.deferredExecutionPublicationSessionIDs.isEmpty,
              planner.canonicalPreviewFreshnessIssue == nil,
              let provenance = planner.schedulePreviewProvenance,
              let proof = planner.publishedScheduleProof,
              proof.configurationIdentifier == planner.canonicalConfigurationIdentifier,
              proof.matches(provenance),
              proof.matchesPublishedPlan(planner.blocks),
              Set(planner.canonicalItems.map(\.id)).count
                == planner.canonicalItems.count else {
            return []
        }

        let itemByID = Dictionary(
            uniqueKeysWithValues: planner.canonicalItems.map { ($0.id, $0) }
        )
        let parentIDsWithKnownChildren = Set(
            planner.canonicalItems.compactMap(\.parentID)
        )
        let hierarchyIDsWithPendingEvidence = planner.pendingCanonicalAuthoringMutations
            .reduce(into: Set<UUID>()) { result, mutation in
                result.insert(mutation.itemID)
                if let parentID = mutation.draft?.parentID { result.insert(parentID) }
                if let parentID = mutation.baseItem?.parentID { result.insert(parentID) }
            }
        let sensitivityPendingItemIDs = Set(
            planner.pendingCanonicalSensitivityMutations.map(\.itemID)
        )

        return planner.todaysBlocks.filter { block in
            guard block.status == .scheduled,
                  block.syncOrigin == .canonicalPreview,
                  block.isFlexible,
                  !block.isHardConstraint,
                  block.previewKind == "planned",
                  block.kind != .event,
                  block.kind != .breakTime,
                  let itemID = block.sourceItemID,
                  itemID != source.itemID,
                  source.occurrenceID.map({ block.occurrenceID != $0 }) ?? true,
                  let itemRevision = block.sourceItemRevision,
                  let sessionIndex = block.sessionIndex,
                  let item = itemByID[itemID],
                  item.revision == itemRevision,
                  item.status == .scheduled,
                  item.deletedAt == nil,
                  item.isExecutable,
                  canonicalKind(item.kind, matches: block.kind),
                  !parentIDsWithKnownChildren.contains(itemID),
                  !hierarchyIDsWithPendingEvidence.contains(itemID),
                  !sensitivityPendingItemIDs.contains(itemID),
                  hierarchyIsCompleteAndAcyclic(for: item, itemByID: itemByID),
                  proof.matches(block),
                  planner.canMutate(block),
                  planner.canonicalScheduleBlockActionabilityIssue(block) == nil else {
                return false
            }
            return !planner.executionState.terminalOutcomes.values.contains { outcome in
                let session = outcome.session
                return session.itemID == itemID
                    && session.itemRevision == itemRevision
                    && session.occurrenceID == block.occurrenceID
                    && session.sessionIndex == sessionIndex
            }
        }
        .sorted { left, right in
            if left.start != right.start { return left.start < right.start }
            if left.end != right.end { return left.end < right.end }
            return left.id.uuidString < right.id.uuidString
        }
    }

    private static func hierarchyIsCompleteAndAcyclic(
        for item: DayWeaveCanonicalItem,
        itemByID: [UUID: DayWeaveCanonicalItem]
    ) -> Bool {
        var current = item.parentID
        var visited: Set<UUID> = [item.id]
        while let itemID = current {
            guard visited.insert(itemID).inserted,
                  let parent = itemByID[itemID] else { return false }
            current = parent.parentID
        }
        return true
    }

    private static func canonicalKind(
        _ canonical: DayWeaveCanonicalItemKind,
        matches planner: PlannerItemKind
    ) -> Bool {
        switch (canonical, planner) {
        case (.task, .task), (.habit, .habit), (.routine, .routine), (.goal, .goal):
            true
        case (.event, _), (.breakTime, _), (.unknown, _),
             (_, .event), (_, .breakTime):
            false
        default:
            false
        }
    }

    private static func safePlacementReason(_ value: String?) -> String? {
        guard let value else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              trimmed.unicodeScalars.count <= 500,
              !trimmed.unicodeScalars.contains(
                  where: CharacterSet.controlCharacters.contains
              ) else { return nil }
        return trimmed
    }
}
