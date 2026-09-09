import SwiftUI

extension ItemCompletionMode {
    var title: String {
        switch self {
        case .automatic: "Automatic"
        case .keepOpen: "Keep open"
        case .complete: "Complete manually"
        }
    }
    var explanation: String {
        switch self {
        case .automatic: "Complete when every required descendant is complete. New unfinished work can reopen this parent."
        case .keepOpen: "Keep this parent open even when its required work is complete. Its retained open state is restored when needed."
        case .complete: "Keep this parent completed without changing its descendants. Unfinished required descendants still count for higher ancestors."
        }
    }
}

struct ItemCompletionOutboxView: View {
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var completion: ItemCompletionStore
    @State private var message: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if planner.itemCompletionState.needsCanonicalCatchUp {
                Text("A confirmed completion needs its complete task update. Editing resumes after synchronization.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if planner.itemCompletionState.journals.isEmpty && !planner.itemCompletionState.needsCanonicalCatchUp {
                Text("No completion requests await recovery.").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(completion.recoveryEntries) { entry in
                VStack(alignment: .leading, spacing: 5) {
                    Text("Item \(entry.itemID.uuidString.prefix(8))").font(.subheadline.weight(.medium))
                    Text(entry.isRejected ? "Not applied · review or discard saved intent" : "Exact request retained for recovery")
                        .font(.caption)
                    if entry.canDiscard {
                        Button("Discard saved intent") {
                            do { try completion.discardReviewedIntent(entry.itemID, expectedOperationID: entry.id) }
                            catch { message = "The request remains retained; it could not be safely discarded." }
                        }.disabled(completion.isWorking)
                    }
                }
            }
            Button("Recover completion requests") { Task { await completion.replayPending() } }
                .disabled(completion.isWorking || !completion.hasPendingRecovery)
                .accessibilityIdentifier("item-completion.recover")
            if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
        }
    }
}

struct ItemCompletionReview: Identifiable {
    let id = UUID()
    let baseline: ItemCompletionSnapshot
    let lease: ItemCompletionReviewLease
    let owner: UUID
    let requiredForParent: Bool
    let mode: ItemCompletionMode
    let sensitive: Bool
}

struct ItemCompletionPanel: View {
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var completion: ItemCompletionStore
    let itemID: UUID
    let sensitive: Bool
    @State private var owner = UUID()
    @State private var review: ItemCompletionReview?
    @State private var message: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Completion").font(.headline)
            Text("Required descendants and your override determine parent completion—not effort or independent progress.")
                .font(.caption).foregroundStyle(.secondary)
            if let observation = completion.observation(for: itemID) {
                let snapshot = observation.snapshot
                ItemCompletionSummary(snapshot: snapshot)
                    .privacySensitive(sensitive || completion.isSensitive(itemID))
                Text(completion.canReview(itemID) ? "Current reviewed authority available"
                    : "Saved observation · refresh before making a new choice")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                Text("Fetch completion evidence to see the required work and current policy.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let journal = completion.journal(for: itemID) {
                Divider()
                Text(journal.noEffectCode != nil ? "Not applied · explicit review needed"
                    : journal.hasBeenSubmitted ? "Exact request awaiting recovery" : "Reviewed choices queued offline")
                    .font(.caption.weight(.medium))
                Text("\(journal.command.mode.title) · \(journal.command.requiredForParent ? "Required branch" : "Optional branch")")
                    .font(.caption).privacySensitive(sensitive || completion.isSensitive(itemID))
                if !journal.hasBeenSubmitted || journal.noEffectCode != nil {
                    Button("Discard saved intent") {
                        do { try completion.discardReviewedIntent(itemID, expectedOperationID: journal.id) }
                        catch { message = "The saved request could not be safely cleared." }
                    }.disabled(completion.isWorking)
                }
            }
            HStack {
                Button("Refresh") {
                    completion.showDetail(itemID, owner: owner)
                    completion.refreshVisibleDetail(owner: owner)
                }.disabled(completion.isWorking)
                Button("Review completion…") {
                    completion.showDetail(itemID, owner: owner)
                    guard let snapshot = completion.observation(for: itemID)?.snapshot,
                          let lease = completion.reviewLease(itemID: itemID, owner: owner) else { return }
                    let pending = completion.journal(for: itemID)?.command
                    review = .init(baseline: snapshot, lease: lease, owner: owner,
                        requiredForParent: pending?.requiredForParent ?? snapshot.state.requiredForParent,
                        mode: pending?.mode ?? snapshot.state.mode, sensitive: sensitive || completion.isSensitive(itemID))
                }.disabled(!completion.canReview(itemID))
                    .accessibilityIdentifier("item-completion.review")
            }
            Text(message ?? completion.message).font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityIdentifier("item-completion.panel")
        .onAppear { completion.showDetail(itemID, owner: owner) }
        .onChange(of: itemID) { _, value in review = nil; completion.showDetail(value, owner: owner) }
        .onChange(of: planner.canonicalConfigurationIdentifier) { _, _ in
            review = nil; completion.hideDetail(owner: owner); completion.showDetail(itemID, owner: owner)
        }
        .onDisappear { review = nil; completion.hideDetail(owner: owner) }
        .sheet(item: $review) { context in
            ItemCompletionReviewView(context: context)
                .privacySensitive(context.sensitive || completion.isSensitive(itemID))
        }
    }
}

private struct ItemCompletionSummary: View {
    let snapshot: ItemCompletionSnapshot
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(snapshot.state.mode.title).font(.subheadline.weight(.semibold))
            Text("\(snapshot.counts.completed) of \(snapshot.counts.requiredDescendants) required descendants complete")
                .font(.caption)
            if snapshot.counts.incomplete > 0 {
                Text("\(snapshot.counts.incomplete) still incomplete").font(.caption).foregroundStyle(.secondary)
            }
            if snapshot.counts.occurrenceEvidenceRequired > 0 || snapshot.occurrenceEvidenceRequired {
                Text("Recurring work needs occurrence-specific evidence; template status cannot complete this parent.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Text(snapshot.state.requiredForParent ? "Required by ancestors" : "Optional branch for ancestors")
                .font(.caption).foregroundStyle(.secondary)
            if let provenance = snapshot.state.provenance {
                Text("Retained reopening state: \(provenance.reopen.status.rawValue)")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

struct ItemCompletionReviewView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var completion: ItemCompletionStore
    let context: ItemCompletionReview
    @State private var baseline: ItemCompletionSnapshot
    @State private var lease: ItemCompletionReviewLease
    @State private var requiredForParent: Bool
    @State private var mode: ItemCompletionMode
    @State private var sensitive: Bool
    @State private var message: String?

    init(context: ItemCompletionReview) {
        self.context = context
        _baseline = State(initialValue: context.baseline); _lease = State(initialValue: context.lease)
        _requiredForParent = State(initialValue: context.requiredForParent); _mode = State(initialValue: context.mode)
        _sensitive = State(initialValue: context.sensitive)
    }
    private var maySave: Bool {
        completion.reviewIsCurrent(lease, baseline: baseline)
            && (mode == baseline.state.mode || completion.canOverride(baseline.itemID))
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Review completion").font(.title2.weight(.semibold))
            ItemCompletionSummary(snapshot: baseline)
            Divider()
            Toggle("Required for the parent", isOn: $requiredForParent)
                .accessibilityIdentifier("item-completion.required")
            Text("Making a branch optional excludes it from ancestor completion checks. Its tasks and schedule remain intact.")
                .font(.caption).foregroundStyle(.secondary)
            Picker("Parent policy", selection: $mode) {
                ForEach(ItemCompletionMode.allCases, id: \.self) { value in Text(value.title).tag(value) }
            }
            .disabled(!completion.canOverride(baseline.itemID))
            .accessibilityIdentifier("item-completion.mode")
            Text(mode.explanation).font(.callout).foregroundStyle(.secondary)
            if !maySave {
                Text("This review is no longer current or the requested policy is unavailable. Your choices are retained; refresh and review the new evidence.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
            HStack {
                Button("Cancel", role: .cancel) { dismiss() }
                Button("Refresh review") {
                    Task {
                        guard await completion.refresh(baseline.itemID),
                              let snapshot = completion.observation(for: baseline.itemID)?.snapshot,
                              let refreshed = completion.reviewLease(itemID: baseline.itemID, owner: context.owner) else {
                            message = "A current review is unavailable. Your entered choices are unchanged."; return
                        }
                        sensitive = sensitive || completion.isSensitive(baseline.itemID)
                        baseline = snapshot; lease = refreshed
                        message = "Evidence refreshed. Check the counts and policy before approving your retained choices."
                    }
                }.disabled(completion.isWorking)
                Spacer()
                Button("Save reviewed choice") {
                    do {
                        try completion.queueReviewed(lease: lease, baseline: baseline,
                            requiredForParent: requiredForParent, mode: mode)
                        dismiss()
                    } catch { message = "The review changed or could not be saved. Refresh to review again; your choices are retained." }
                }.buttonStyle(.borderedProminent).disabled(!maySave)
                    .accessibilityIdentifier("item-completion.approve")
            }
        }
        .padding(24).frame(width: 510)
        .privacySensitive(sensitive || completion.isSensitive(baseline.itemID)
            || completion.observation(for: baseline.itemID)?.snapshot != baseline)
        .accessibilityIdentifier("item-completion.sheet")
    }
}
