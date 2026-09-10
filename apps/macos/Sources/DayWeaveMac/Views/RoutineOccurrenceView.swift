import SwiftUI

/// Calendar selection uses producer-issued identities. A descendant's source ID
/// and the displayed date cannot identify the occurrence's canonical root.
struct RoutineOccurrenceSelection: Equatable {
    let seriesItemID: UUID
    let occurrenceID: UUID

    init?(block: ScheduleBlock, currentSeriesKind: DayWeaveCanonicalItemKind? = nil) {
        guard let seriesItemID = block.recurrenceSeriesItemID,
              seriesItemID != RoutineOccurrenceValidation.nilID,
              let occurrenceID = block.occurrenceID,
              dayWeaveIsRFC4122VersionFiveUUID(occurrenceID) else { return nil }
        if let currentSeriesKind, currentSeriesKind != .task && currentSeriesKind != .routine { return nil }
        self.seriesItemID = seriesItemID
        self.occurrenceID = occurrenceID
    }

    func matches(_ snapshot: RoutineOccurrenceSnapshot) -> Bool {
        snapshot.aggregate.manifest.seriesItemID == seriesItemID
            && snapshot.aggregate.manifest.occurrenceID == occurrenceID
    }
}

struct RoutineOccurrenceMemberRow: Identifiable {
    let definition: RoutineOccurrenceMemberDefinition
    let state: RoutineOccurrenceMemberState
    let evaluation: RoutineOccurrenceMemberEvaluation
    let depth: Int
    let hasChildren: Bool
    var id: UUID { definition.itemID }
}

enum RoutineOccurrencePresentation {
    /// Flatten once, then render a lazy list. Deep manifests never create a
    /// recursive view tree or repeatedly scan the full member set for children.
    static func rows(_ snapshot: RoutineOccurrenceSnapshot) -> [RoutineOccurrenceMemberRow] {
        let manifest = snapshot.aggregate.manifest
        guard manifest.members.count <= RoutineOccurrenceValidation.maximumMembers else { return [] }
        var definitions: [UUID: RoutineOccurrenceMemberDefinition] = [:]
        var states: [UUID: RoutineOccurrenceMemberState] = [:]
        var evaluations: [UUID: RoutineOccurrenceMemberEvaluation] = [:]
        var children: [UUID: [UUID]] = [:]
        for definition in manifest.members {
            guard definitions.updateValue(definition, forKey: definition.itemID) == nil else { return [] }
            if let parent = definition.parentID { children[parent, default: []].append(definition.itemID) }
        }
        for state in snapshot.aggregate.members {
            guard states.updateValue(state, forKey: state.itemID) == nil else { return [] }
        }
        for evaluation in snapshot.members {
            guard evaluations.updateValue(evaluation, forKey: evaluation.itemID) == nil else { return [] }
        }
        guard definitions.count == states.count, definitions.count == evaluations.count,
              definitions[manifest.seriesItemID]?.parentID == nil else { return [] }
        for parent in Array(children.keys) {
            children[parent]?.sort {
                let left = definitions[$0]!, right = definitions[$1]!
                return left.siblingOrder == right.siblingOrder
                    ? left.itemID.uuidString < right.itemID.uuidString
                    : left.siblingOrder < right.siblingOrder
            }
        }
        var result: [RoutineOccurrenceMemberRow] = []
        var pending = [(manifest.seriesItemID, 0)]
        var seen = Set<UUID>()
        while let (id, depth) = pending.popLast() {
            guard seen.insert(id).inserted, let definition = definitions[id],
                  let state = states[id], let evaluation = evaluations[id] else { return [] }
            let descendants = children[id] ?? []
            result.append(.init(definition: definition, state: state, evaluation: evaluation,
                depth: depth, hasChildren: !descendants.isEmpty))
            pending.append(contentsOf: descendants.reversed().map { ($0, depth + 1) })
        }
        return result.count == definitions.count ? result : []
    }

    static func reason(_ reason: RoutineOccurrenceReason) -> String {
        switch reason {
        case .unchanged: "Current occurrence state"
        case .outcomeRecorded: "Outcome recorded for this occurrence"
        case .reopened: "Reopened to the reviewed open state"
        case .policyReviewed: "Occurrence policy reviewed"
        case .occurrenceEvidenceRequired: "This recurring branch needs its own occurrence evidence"
        case .automaticallyCompleted: "Completed automatically from required descendants"
        case .automaticallyReopened: "Reopened automatically because required work is incomplete"
        case .manuallyCompleted: "Completed by the parent override"
        case .manuallyKeptOpen: "Kept open by the parent policy"
        case .manualCompletionReleased: "Manual completion released"
        }
    }

    static func rejection(_ code: String) -> String {
        switch code {
        case "routine_occurrence_definition_changed": "The routine definition changed. Refresh and review the occurrence again."
        case "routine_occurrence_source_ineligible": "The source no longer permits a new occurrence choice."
        case "routine_occurrence_instance_stale", "routine_occurrence_member_stale", "routine_occurrence_evidence_stale":
            "The occurrence changed since this review. Refresh to review the current evidence."
        case "routine_occurrence_member_missing", "routine_occurrence_missing": "This occurrence or member is no longer available."
        case "routine_occurrence_evidence_required": "This recurring branch needs its own occurrence evidence."
        case "routine_occurrence_execution_conflict": "Execution activity conflicts with this choice. Refresh after resolving the active session."
        case "routine_occurrence_leaf_required": "This outcome is only available for a leaf member."
        case "routine_occurrence_parent_required": "This policy is only available for a parent member."
        case "routine_occurrence_operation_reused": "The server could not accept this operation identity. The saved intent remains available for review."
        case "routine_occurrence_too_large": "The occurrence exceeds the supported size."
        case "routine_occurrence_invalid_cursor": "Occurrence synchronization must restart from a current checkpoint."
        default: "The server did not apply this request. Refresh and review before choosing again."
        }
    }

    static func actionTitle(_ action: RoutineOccurrenceAction) -> String {
        switch action {
        case let .setOutcome(status): status == .completed ? "Complete this occurrence member" : "Skip this occurrence member"
        case let .reopen(open): "Reopen as \(open.status.rawValue)"
        case let .setPolicy(required, mode): "\(mode.title) · \(required ? "Required branch" : "Optional branch")"
        }
    }

    static func canApply(_ action: RoutineOccurrenceAction, to row: RoutineOccurrenceMemberRow) -> Bool {
        guard !row.evaluation.occurrenceEvidenceRequired, action.isValid(for: row.id) else { return false }
        switch action {
        case .setOutcome: return !row.hasChildren
        case let .reopen(open): return !row.hasChildren && open == row.state.open
        case let .setPolicy(_, mode): return row.hasChildren || mode == .automatic
        }
    }
}

struct RoutineOccurrenceOutboxView: View {
    @EnvironmentObject private var occurrences: RoutineOccurrenceStore
    @State private var message: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !occurrences.hasPendingRecovery {
                Text("No routine occurrence requests await recovery.").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(occurrences.recoveryEntries) { entry in
                VStack(alignment: .leading, spacing: 5) {
                    Text("Occurrence \(entry.instanceID.uuidString.prefix(8)) · member \(entry.memberID.uuidString.prefix(8))")
                        .font(.subheadline.weight(.medium))
                    Text(entry.isRejected ? "Not applied · review the selected calendar occurrence or discard the saved intent"
                        : "Exact request retained for recovery")
                        .font(.caption)
                    if entry.canDiscard {
                        Button("Discard saved intent") {
                            do {
                                try occurrences.discardReviewedIntent(instanceID: entry.instanceID, expectedOperationID: entry.id)
                            } catch { message = "The saved request changed or could not be safely discarded." }
                        }.disabled(occurrences.isWorking)
                    }
                }
            }
            Button("Recover occurrence requests") { Task { _ = await occurrences.replayPending() } }
                .disabled(occurrences.isWorking || !occurrences.hasPendingRecovery || !occurrences.hasAdmittedConnection)
                .accessibilityIdentifier("routine-occurrence.recover")
            Text(message ?? occurrences.message).font(.caption).foregroundStyle(.secondary)
        }
        .privacySensitive(true)
        .accessibilityIdentifier("routine-occurrence.outbox")
    }
}

private struct RoutineOccurrenceReview: Identifiable {
    let id = UUID()
    let baseline: RoutineOccurrenceSnapshot
    let lease: RoutineOccurrenceReviewLease
    let memberID: UUID
    let owner: UUID
    let action: RoutineOccurrenceAction
}

struct RoutineOccurrencePanel: View {
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var occurrences: RoutineOccurrenceStore
    let selection: RoutineOccurrenceSelection
    @State private var owner = UUID()
    @State private var review: RoutineOccurrenceReview?
    @State private var message: String?

    private var snapshot: RoutineOccurrenceSnapshot? {
        guard let snapshot = occurrences.selectedSnapshot, selection.matches(snapshot) else { return nil }
        return snapshot
    }
    private var journal: RoutineOccurrenceJournal? {
        guard let journal = occurrences.selectedJournal,
              journal.instanceID == snapshot?.aggregate.manifest.id else { return nil }
        return journal
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Routine occurrence").font(.headline)
            Text("Review the complete member tree and choices for this selected occurrence.")
                .font(.caption).foregroundStyle(.secondary)
            if let snapshot {
                if !snapshot.freshEditEligible {
                    Text("Saved occurrence · new choices are unavailable until current evidence permits editing.")
                        .font(.caption).foregroundStyle(.orange)
                }
                LazyVStack(alignment: .leading, spacing: 14) {
                    ForEach(RoutineOccurrencePresentation.rows(snapshot)) { row in
                        VStack(alignment: .leading, spacing: 6) {
                            RoutineOccurrenceMemberSummary(row: row)
                            if row.evaluation.occurrenceEvidenceRequired {
                                Text("Open this branch's own calendar occurrence to review its work.")
                                    .font(.caption).foregroundStyle(.secondary)
                            } else {
                                memberActions(row, snapshot: snapshot)
                            }
                        }
                        .padding(.leading, CGFloat(min(row.depth, 8)) * 10)
                        .accessibilityIdentifier("routine-occurrence.member.\(row.id.uuidString.lowercased())")
                    }
                }
            } else {
                Text("Refresh to retrieve this occurrence's full member tree and current evidence.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let journal {
                Divider()
                Text(journal.noEffectCode != nil ? "Not applied · review needed"
                    : journal.hasBeenSubmitted ? "Exact request awaiting recovery" : "Reviewed choice saved for delivery")
                    .font(.caption.weight(.medium))
                Text(RoutineOccurrencePresentation.actionTitle(journal.command.action)).font(.caption)
                if let code = journal.noEffectCode {
                    Text(RoutineOccurrencePresentation.rejection(code)).font(.caption).foregroundStyle(.orange)
                }
                HStack {
                    if journal.noEffectCode == nil {
                        Button("Retry saved request") { Task { _ = await occurrences.replayPending() } }
                            .disabled(occurrences.isWorking || !occurrences.hasAdmittedConnection)
                    }
                    if !journal.hasBeenSubmitted || journal.noEffectCode != nil {
                        Button("Discard saved intent") {
                            do {
                                try occurrences.discardReviewedIntent(instanceID: journal.instanceID, expectedOperationID: journal.id)
                            } catch { message = "The saved request changed or could not be safely discarded." }
                        }.disabled(occurrences.isWorking)
                    }
                }
            }
            Button("Refresh occurrence") {
                showDetail()
                Task { _ = await occurrences.refreshSelected() }
            }
            .disabled(occurrences.isWorking || !occurrences.hasAdmittedConnection)
            .accessibilityIdentifier("routine-occurrence.refresh")
            Text(message ?? occurrences.message).font(.caption).foregroundStyle(.secondary)
        }
        .privacySensitive(true)
        .accessibilityIdentifier("routine-occurrence.panel")
        .onAppear { showDetail() }
        .onChange(of: selection) { _, _ in review = nil; message = nil; showDetail() }
        .onChange(of: planner.canonicalConfigurationIdentifier) { _, _ in
            review = nil; message = nil
            occurrences.hideDetail(owner: owner)
            showDetail()
        }
        .onChange(of: occurrences.hasAdmittedConnection) { _, admitted in
            if !admitted { review = nil; message = nil }
        }
        .onChange(of: occurrences.selectionRevision) { _, _ in review = nil; message = nil }
        .onDisappear { review = nil; occurrences.hideDetail(owner: owner) }
        .sheet(item: $review) { context in
            RoutineOccurrenceReviewView(context: context).privacySensitive(true)
        }
    }

    private func showDetail() {
        occurrences.showDetail(seriesItemID: selection.seriesItemID, occurrenceID: selection.occurrenceID, owner: owner)
    }

    private func beginReview(_ row: RoutineOccurrenceMemberRow, snapshot: RoutineOccurrenceSnapshot,
                             action: RoutineOccurrenceAction) {
        guard let lease = occurrences.reviewLease(memberID: row.id, owner: owner) else { return }
        review = .init(baseline: snapshot, lease: lease, memberID: row.id, owner: owner, action: action)
    }

    @ViewBuilder
    private func memberActions(_ row: RoutineOccurrenceMemberRow, snapshot: RoutineOccurrenceSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            if !row.hasChildren {
                ViewThatFits(in: .horizontal) {
                    HStack { leafActions(row, snapshot: snapshot) }
                    VStack(alignment: .leading) { leafActions(row, snapshot: snapshot) }
                }
            }
            Button(row.hasChildren ? "Review parent policy…" : "Review required branch…") {
                beginReview(row, snapshot: snapshot,
                    action: .setPolicy(requiredForParent: row.state.requiredForParent, mode: row.state.mode))
            }
        }
        .controlSize(.small)
        .disabled(occurrences.isWorking || occurrences.reviewLease(memberID: row.id, owner: owner) == nil)
    }

    @ViewBuilder
    private func leafActions(_ row: RoutineOccurrenceMemberRow, snapshot: RoutineOccurrenceSnapshot) -> some View {
        Button("Complete…") { beginReview(row, snapshot: snapshot, action: .setOutcome(status: .completed)) }
        Button("Skip…") { beginReview(row, snapshot: snapshot, action: .setOutcome(status: .skipped)) }
        Button("Reopen…") { beginReview(row, snapshot: snapshot, action: .reopen(open: row.state.open)) }
    }
}

private struct RoutineOccurrenceMemberSummary: View {
    let row: RoutineOccurrenceMemberRow
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(row.definition.title).font(.subheadline.weight(.semibold))
            Text("\(row.state.status.rawValue.capitalized) · \(row.state.requiredForParent ? "Required" : "Optional")\(row.hasChildren ? " · " + row.state.mode.title : "")")
                .font(.caption)
            if row.depth > 8 { Text("Level \(row.depth + 1)").font(.caption2).foregroundStyle(.secondary) }
            if row.hasChildren {
                Text("\(row.evaluation.counts.completed) of \(row.evaluation.counts.requiredDescendants) required descendants complete")
                    .font(.caption)
                Text("\(row.evaluation.counts.incomplete) incomplete · \(row.evaluation.counts.occurrenceEvidenceRequired) need occurrence evidence")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Text(RoutineOccurrencePresentation.reason(row.evaluation.reason)).font(.caption).foregroundStyle(.secondary)
        }
    }
}

private struct RoutineOccurrenceReviewView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var occurrences: RoutineOccurrenceStore
    let context: RoutineOccurrenceReview
    @State private var baseline: RoutineOccurrenceSnapshot
    @State private var lease: RoutineOccurrenceReviewLease
    @State private var action: RoutineOccurrenceAction
    @State private var message: String?

    init(context: RoutineOccurrenceReview) {
        self.context = context
        _baseline = State(initialValue: context.baseline)
        _lease = State(initialValue: context.lease)
        _action = State(initialValue: context.action)
    }

    private var row: RoutineOccurrenceMemberRow? {
        RoutineOccurrencePresentation.rows(baseline).first { $0.id == context.memberID }
    }
    private var maySave: Bool {
        !occurrences.isWorking && occurrences.reviewIsCurrent(lease, baseline: baseline)
            && row.map { RoutineOccurrencePresentation.canApply(action, to: $0) } == true
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Review occurrence choice").font(.title2.weight(.semibold))
            if let row { RoutineOccurrenceMemberSummary(row: row) }
            Divider()
            Text(RoutineOccurrencePresentation.actionTitle(action)).font(.headline)
            switch action {
            case let .setOutcome(status):
                Text(status == .completed
                    ? "Record completion for this member of the selected occurrence."
                    : "Record a skipped outcome for this member. Skipped required work remains incomplete for ancestor completion.")
                    .font(.callout).foregroundStyle(.secondary)
            case let .reopen(open):
                Text("Restore the exact retained open state shown below.").font(.callout).foregroundStyle(.secondary)
                LabeledContent("Open status", value: open.status.rawValue.capitalized)
                if let kind = open.blockedReasonKind { LabeledContent("Blocked kind", value: kind.rawValue.capitalized) }
                if let blocker = open.blockedByItemID { LabeledContent("Blocking item", value: blocker.uuidString) }
                if let reason = open.blockedReason { LabeledContent("Blocked reason", value: reason) }
                if let row, open != row.state.open {
                    Text("The retained reopening state changed. Review the current state before saving.")
                        .font(.caption).foregroundStyle(.orange)
                    Button("Use current reopening state") { action = .reopen(open: row.state.open) }
                }
            case let .setPolicy(required, mode):
                Toggle("Required for the parent", isOn: Binding(get: { required }, set: {
                    action = .setPolicy(requiredForParent: $0, mode: mode)
                }))
                .accessibilityIdentifier("routine-occurrence.required")
                if row?.hasChildren == true {
                    Picker("Parent policy", selection: Binding(get: { mode }, set: {
                        action = .setPolicy(requiredForParent: required, mode: $0)
                    })) {
                        ForEach(ItemCompletionMode.allCases, id: \.self) { value in Text(value.title).tag(value) }
                    }
                    .accessibilityIdentifier("routine-occurrence.mode")
                    Text(mode.explanation).font(.callout).foregroundStyle(.secondary)
                }
                Text("An optional branch is excluded from ancestor completion checks for this occurrence.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if !maySave {
                Text("Current evidence is required before saving. Refresh and review your retained choice.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
            HStack {
                Button("Cancel", role: .cancel) { dismiss() }
                Button("Refresh review") {
                    Task {
                        guard await occurrences.refreshSelected(),
                              let snapshot = occurrences.selectedSnapshot,
                              snapshot.aggregate.manifest.id == baseline.aggregate.manifest.id,
                              let refreshed = occurrences.reviewLease(memberID: context.memberID, owner: context.owner) else {
                            message = "A current review is unavailable. Your choice remains unchanged."; return
                        }
                        baseline = snapshot; lease = refreshed
                        message = "Evidence refreshed. Check the current state and counts before approving your retained choice."
                    }
                }.disabled(occurrences.isWorking || !occurrences.hasAdmittedConnection)
                Spacer()
                Button("Save reviewed choice") {
                    do {
                        try occurrences.queueReviewed(lease: lease, baseline: baseline, action: action)
                        dismiss()
                    } catch {
                        message = "The review changed or could not be saved. Refresh and review again; your choice remains unchanged."
                    }
                }
                .buttonStyle(.borderedProminent).disabled(!maySave)
                .accessibilityIdentifier("routine-occurrence.approve")
            }
        }
        .padding(24).frame(width: 560)
        .privacySensitive(true)
        .accessibilityIdentifier("routine-occurrence.sheet")
    }
}
