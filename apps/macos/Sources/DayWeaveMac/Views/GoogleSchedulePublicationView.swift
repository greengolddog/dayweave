import SwiftUI

struct GoogleSchedulePublicationView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var google: GoogleIntegrationStore
    @EnvironmentObject private var publication: GoogleSchedulePublicationStore

    @State private var selectedAccountID: UUID?
    @State private var selectedCollectionID: UUID?
    @State private var explicitlyApproved = false
    @State private var isApprovedReplayConfirmationPresented = false
    @State private var isDiscardConfirmationPresented = false

    private var eligibleAccounts: [GoogleAccount] {
        google.accounts.filter {
            $0.status == .active && google.hasCalendarPublishingScope(for: $0)
        }
    }

    private var eligibleCalendars: [GoogleSyncCollection] {
        guard let selectedAccountID else { return [] }
        return (google.collectionsByAccount[selectedAccountID] ?? [])
            .filter { collection in
                guard collection.kind == .calendar,
                      collection.selected,
                      collection.syncRole == .writable,
                      !collection.providerDeleted,
                      let role = collection.providerAccessRole?.lowercased() else {
                    return false
                }
                return role == "owner" || role == "writer"
            }
            .sorted {
                $0.displayName.localizedCaseInsensitiveCompare($1.displayName)
                    == .orderedAscending
            }
    }

    private var selectedAccount: GoogleAccount? {
        eligibleAccounts.first { $0.id == selectedAccountID }
    }

    private var selectedCalendar: GoogleSyncCollection? {
        eligibleCalendars.first { $0.id == selectedCollectionID }
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if publication.hasSavedPublication {
                        savedPublicationContent
                    } else if let preview = publication.preview {
                        previewContent(preview)
                    } else {
                        destinationContent
                    }
                    statusCard
                }
                .padding(24)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Divider()
            footer
        }
        .frame(minWidth: 720, idealWidth: 820, minHeight: 620, idealHeight: 760)
        .onAppear(perform: selectDefaults)
        .onChange(of: selectedAccountID) { _, _ in
            if !eligibleCalendars.contains(where: { $0.id == selectedCollectionID }) {
                selectedCollectionID = eligibleCalendars.first?.id
            }
        }
        .onChange(of: publication.preview?.id) { _, _ in
            explicitlyApproved = false
        }
        .confirmationDialog(
            "Replay the previously approved publication?",
            isPresented: $isApprovedReplayConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Replay approved enqueue", role: .destructive) {
                Task { await publication.replayApprovedEnqueue() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "A prior enqueue response may have been lost. Verify Google Calendar or DayWeave server state before retrying. Continuing can resend that exact approved enqueue; DayWeave will not create a new approval or change the reviewed payload."
            )
        }
        .confirmationDialog(
            "Discard the expired publication recovery?",
            isPresented: $isDiscardConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Discard encrypted recovery", role: .destructive) {
                _ = publication.discardExpiredRecovery()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(discardConfirmationMessage)
        }
        .accessibilityIdentifier("google.schedule-publication.review")
    }

    private var header: some View {
        HStack(spacing: 14) {
            ZStack {
                RoundedRectangle(cornerRadius: 12)
                    .fill(Color.accentColor.opacity(0.14))
                Image(systemName: "calendar.badge.checkmark")
                    .font(.title2)
                    .foregroundStyle(Color.accentColor)
            }
            .frame(width: 46, height: 46)

            VStack(alignment: .leading, spacing: 3) {
                Text("Publish schedule to Google")
                    .font(.title2.weight(.semibold))
                Text("Preview first. Nothing is queued without your explicit approval.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.borderless)
            .accessibilityLabel("Close")
        }
        .padding(20)
    }

    @ViewBuilder
    private var destinationContent: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 16) {
                Label("Destination", systemImage: "calendar")
                    .font(.headline)

                if eligibleAccounts.isEmpty {
                    ContentUnavailableView(
                        "No publishing account",
                        systemImage: "person.crop.circle.badge.exclamationmark",
                        description: Text(
                            "Connect an active Google account, enable Calendar publishing access, and select a writable Publish calendar in Settings."
                        )
                    )
                    .frame(maxWidth: .infinity)
                } else {
                    Picker("Google account", selection: $selectedAccountID) {
                        ForEach(eligibleAccounts) { account in
                            Text(account.displayLabel).tag(Optional(account.id))
                        }
                    }
                    .pickerStyle(.menu)
                    .accessibilityIdentifier("google.schedule-publication.account")

                    Picker("Calendar", selection: $selectedCollectionID) {
                        if eligibleCalendars.isEmpty {
                            Text("No writable Publish calendar").tag(Optional<UUID>.none)
                        } else {
                            ForEach(eligibleCalendars) { calendar in
                                Text(calendar.displayName).tag(Optional(calendar.id))
                            }
                        }
                    }
                    .pickerStyle(.menu)
                    .disabled(eligibleCalendars.isEmpty)
                    .accessibilityIdentifier("google.schedule-publication.calendar")
                }
            }
            .padding(6)
        }

        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                Label("Current generated schedule", systemImage: "seal")
                    .font(.headline)
                if let proof = planner.publishedScheduleProof {
                    LabeledContent("Revision", value: "#\(proof.revisionNumber)")
                    LabeledContent(
                        "Published",
                        value: proof.publishedAt.formatted(date: .abbreviated, time: .shortened)
                    )
                    Text(
                        "The server will compare this immutable revision with the selected Calendar and return an exact, expiring review."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                } else {
                    Label(
                        "Sync and publish a generated schedule before sending it to Google Calendar.",
                        systemImage: "exclamationmark.triangle"
                    )
                    .foregroundStyle(.orange)
                }
            }
            .padding(6)
        }
    }

    @ViewBuilder
    private var savedPublicationContent: some View {
        if let preview = publication.preview {
            previewContent(preview)
        } else if let delivery = publication.deliveryStatus {
            deliveryContent(delivery)
        } else {
            GroupBox {
                VStack(alignment: .leading, spacing: 12) {
                    Label("Saved recovery", systemImage: "arrow.triangle.2.circlepath")
                        .font(.headline)
                    Text(
                        "DayWeave retained the exact encrypted publication state. Recovery never approves a preview automatically."
                    )
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    Button(recoveryActionTitle) {
                        requestRecovery()
                    }
                    .disabled(publication.status.isWorking)
                    .accessibilityIdentifier("google.schedule-publication.recover")
                }
                .padding(6)
            }
        }
    }

    private func previewContent(_ preview: GoogleSchedulePublicationPreview) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            GroupBox {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(preview.collectionDisplayName)
                                .font(.headline)
                            Text("Generated schedule revision #\(preview.scheduleRevisionNumber)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        Text("Expires \(preview.expiresAt.formatted(date: .omitted, time: .shortened))")
                            .font(.caption.weight(.medium))
                            .foregroundStyle(.secondary)
                    }

                    HStack(spacing: 8) {
                        countBadge("Create", count: preview.createCount, color: .green)
                        countBadge("Update", count: preview.updateCount, color: .blue)
                        countBadge("Remove", count: preview.deleteCount, color: .red)
                        countBadge("Unchanged", count: preview.noopCount, color: .secondary)
                    }

                    if preview.deleteCount > 0 {
                        Label(
                            "This publication removes \(preview.deleteCount) previously published Google Calendar event\(preview.deleteCount == 1 ? "" : "s").",
                            systemImage: "exclamationmark.triangle.fill"
                        )
                        .font(.callout.weight(.medium))
                        .foregroundStyle(.orange)
                    }
                }
                .padding(6)
            }

            GroupBox("Exact changes") {
                LazyVStack(spacing: 0) {
                    ForEach(preview.changes) { change in
                        GoogleSchedulePublicationChangeRow(change: change)
                        if change.id != preview.changes.last?.id {
                            Divider().padding(.leading, 34)
                        }
                    }
                }
                .padding(.horizontal, 6)
            }

            Toggle(isOn: $explicitlyApproved) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("I approve these exact Google Calendar changes")
                        .font(.headline)
                    Text(
                        "Approval is single-use and bound to this account, Calendar, schedule revision, and preview."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }
            .toggleStyle(.checkbox)
            .disabled(publication.status.isWorking)
            .accessibilityIdentifier("google.schedule-publication.explicit-approval")
        }
    }

    private func deliveryContent(_ delivery: GoogleSchedulePublicationStatus) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Label(delivery.state.displayName, systemImage: delivery.state.iconName)
                        .font(.headline)
                        .foregroundStyle(delivery.state.tint)
                    Spacer()
                    Text("\(delivery.publishedCount) of \(delivery.totalCount) published")
                        .font(.subheadline.monospacedDigit())
                }

                ProgressView(
                    value: Double(delivery.publishedCount),
                    total: max(1, Double(delivery.totalCount))
                )

                HStack(spacing: 14) {
                    deliveryMetric("Pending", delivery.pendingCount)
                    deliveryMetric("Delivering", delivery.deliveringCount)
                    deliveryMetric("Conflicts", delivery.conflictedCount)
                    deliveryMetric("Failed", delivery.failedCount)
                    deliveryMetric("Superseded", delivery.supersededCount)
                }

                if let code = delivery.lastErrorCode {
                    Label("Last delivery issue: \(code)", systemImage: "info.circle")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if let completedAt = delivery.completedAt {
                    Text("Completed \(completedAt.formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(6)
        }
    }

    private var statusCard: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: publication.status.isWorking
                ? "arrow.triangle.2.circlepath" : "lock.shield")
                .foregroundStyle(statusTint)
            Text(publication.status.message)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("google.schedule-publication.status")
    }

    private var footer: some View {
        HStack {
            if publication.hasSavedPublication {
                savedPublicationActions
            } else {
                Button("Cancel") { dismiss() }
                Spacer()
                Button("Prepare review") {
                    guard let accountID = selectedAccountID,
                          let collectionID = selectedCollectionID,
                          let revisionID = planner.publishedScheduleProof?.revisionID else { return }
                    Task {
                        _ = await publication.preparePreview(
                            accountID: accountID,
                            collectionID: collectionID,
                            scheduleRevisionID: revisionID
                        )
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(
                    selectedAccount == nil
                        || selectedCalendar == nil
                        || planner.publishedScheduleProof == nil
                        || publication.status.isWorking
                        || google.isBusy
                )
                .accessibilityIdentifier("google.schedule-publication.preview")
            }
        }
        .padding(16)
    }

    @ViewBuilder
    private var savedPublicationActions: some View {
        if let confirmation = publication.approvalConfirmation,
           publication.preview != nil {
            Button("Keep for later") { dismiss() }
            Spacer()
            Button("Approve and queue") {
                Task { await publication.approveAndEnqueue(confirmation) }
            }
            .buttonStyle(.borderedProminent)
            .disabled(!explicitlyApproved || publication.status.isWorking)
            .accessibilityIdentifier("google.schedule-publication.approve")
        } else if let delivery = publication.deliveryStatus {
            Button("Close") { dismiss() }
            Spacer()
            if delivery.state.isTerminal {
                Button("Done") {
                    if publication.dismissCompletedPublication() { dismiss() }
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("google.schedule-publication.done")
            } else {
                Button("Refresh status") {
                    Task { await publication.refreshStatus() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(publication.status.isWorking)
                .accessibilityIdentifier("google.schedule-publication.refresh")
            }
        } else {
            Button("Close") { dismiss() }
            Spacer()
            if case .expired = publication.status {
                Button("Discard expired recovery") {
                    isDiscardConfirmationPresented = true
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("google.schedule-publication.discard")
            } else {
                Button(recoveryActionTitle) {
                    requestRecovery()
                }
                .buttonStyle(.borderedProminent)
                .disabled(publication.status.isWorking)
                .accessibilityIdentifier("google.schedule-publication.recover-footer")
            }
        }
    }

    private func countBadge(_ label: String, count: UInt32, color: Color) -> some View {
        Text("\(count) \(label)")
            .font(.caption.weight(.semibold).monospacedDigit())
            .foregroundStyle(color)
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .background(color.opacity(0.1), in: Capsule())
    }

    private func deliveryMetric(_ label: String, _ count: UInt32) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("\(count)").font(.headline.monospacedDigit())
            Text(label).font(.caption2).foregroundStyle(.secondary)
        }
    }

    private var statusTint: Color {
        switch publication.status {
        case .failed, .expired: .red
        case .approvedReplayRequired, .expirySafetyDelay, .recoveryRequired: .orange
        case .completed: .green
        case .privacyProtected, .idle, .previewing, .awaitingApproval, .approving,
             .enqueueing, .refreshingStatus, .active: .accentColor
        }
    }

    private var recoveryActionTitle: String {
        publication.recoveryStage == .approved
            ? "Review approved replay…"
            : "Recover safely"
    }

    private var discardConfirmationMessage: String {
        if publication.recoveryStage == .approved {
            return "A prior enqueue response may have been lost. Verify Google Calendar or DayWeave server state before discarding this expired approved recovery. Discarding removes the encrypted local record and does not undo changes the server may already have accepted."
        }
        return "This removes the expired recovery record from this Mac. It does not undo any Google Calendar changes that may already have been accepted."
    }

    private func requestRecovery() {
        if publication.recoveryStage == .approved {
            isApprovedReplayConfirmationPresented = true
        } else {
            Task { await publication.recoverPendingPublication() }
        }
    }

    private func selectDefaults() {
        guard !publication.hasSavedPublication else { return }
        if !eligibleAccounts.contains(where: { $0.id == selectedAccountID }) {
            selectedAccountID = eligibleAccounts.first(where: \.isDefault)?.id
                ?? eligibleAccounts.first?.id
        }
        if !eligibleCalendars.contains(where: { $0.id == selectedCollectionID }) {
            selectedCollectionID = eligibleCalendars.first?.id
        }
    }
}

private struct GoogleSchedulePublicationChangeRow: View {
    let change: GoogleSchedulePublicationChange

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: change.operation.iconName)
                .foregroundStyle(change.operation.tint)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(change.summary)
                        .font(.body.weight(.medium))
                    Spacer()
                    Text(change.operation.displayName)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(change.operation.tint)
                }
                Text(change.startsAt.formatted(date: .abbreviated, time: .shortened)
                    + " – " + change.endsAt.formatted(date: .omitted, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 10)
    }
}

private extension GoogleSchedulePublicationOperation {
    var displayName: String {
        switch self {
        case .create: "Create"
        case .update: "Update"
        case .delete: "Remove"
        case .noop: "Unchanged"
        }
    }

    var iconName: String {
        switch self {
        case .create: "plus.circle.fill"
        case .update: "arrow.triangle.2.circlepath.circle.fill"
        case .delete: "minus.circle.fill"
        case .noop: "checkmark.circle"
        }
    }

    var tint: Color {
        switch self {
        case .create: .green
        case .update: .blue
        case .delete: .red
        case .noop: .secondary
        }
    }
}

private extension GoogleSchedulePublicationState {
    var iconName: String {
        switch self {
        case .pending: "clock"
        case .delivering: "arrow.up.circle"
        case .backoff: "arrow.clockwise.circle"
        case .partiallyPublished: "exclamationmark.circle"
        case .published: "checkmark.seal.fill"
        case .conflict: "arrow.triangle.branch"
        case .failed: "xmark.octagon"
        case .superseded: "arrowshape.turn.up.forward"
        }
    }

    var tint: Color {
        switch self {
        case .published: .green
        case .partiallyPublished, .conflict, .backoff: .orange
        case .failed: .red
        case .pending, .delivering, .superseded: .accentColor
        }
    }
}
