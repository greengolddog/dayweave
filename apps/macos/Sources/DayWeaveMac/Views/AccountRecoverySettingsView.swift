import AppKit
import SwiftUI

struct AccountRecoverySettingsView: View {
    @ObservedObject var durableAuth: DurableAuthSettingsModel

    let baseURLString: String
    let credentialReplacementDisabled: Bool
    let onRecover: (String) async -> Bool
    let onResumeCredentialReplacement: () async -> Bool

    @State private var enteredRecoveryCode = ""
    @State private var approvedIssueSnapshot: DurableAccountRecoverySnapshot?
    @State private var recoveryConfirmationPresented = false
    @State private var discardPendingConfirmationPresented = false
    @State private var discardIssueConfirmationPresented = false
    @State private var discardIncompatibleConfirmationPresented = false
    @State private var acknowledgedSavedCopy = false
    @State private var clipboardChangeCount: Int?

    var body: some View {
        Section("Account Recovery") {
            Text("Keep one offline recovery code for this DayWeave account. The server stores only a verifier; each code works once and recovery creates its successor.")
                .font(.caption)
                .foregroundStyle(.secondary)

            recoveryJournalContent

            if durableAuth.accountRecoveryPresentation.phase == .idle {
                currentCodeContent
                recoveryInputContent
            }

            if let message = durableAuth.accountRecoveryErrorMessage {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("settings.account-recovery.error")
            } else if let notice = durableAuth.accountRecoveryNotice {
                Label(notice, systemImage: "checkmark.circle.fill")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.account-recovery.notice")
            }

            Text("Generated and entered secrets are kept in the device-only Keychain journal, never in preferences or planner data. Visible text and owned clipboard contents are cleared when Settings closes or DayWeave locks.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .task(id: refreshIdentity) {
            guard durableAuth.accountRecoveryPresentation.phase == .idle,
                  canReadCurrentCode,
                  let baseURL else { return }
            _ = await durableAuth.refreshAccountRecoveryCode(baseURL: baseURL)
        }
        .onChange(of: durableAuth.recoveryPrivacyGeneration) { _, _ in
            clearTransientSecrets()
        }
        .onDisappear {
            clearTransientSecrets()
            durableAuth.clearAccountRecoveryMemory()
        }
        .confirmationDialog(
            issueConfirmationTitle,
            isPresented: issueConfirmationIsPresented,
            titleVisibility: .visible
        ) {
            Button(
                approvedIssueSnapshot?.recoveryCode == nil
                    ? "Generate Recovery Code"
                    : "Replace Recovery Code",
                role: approvedIssueSnapshot?.recoveryCode == nil ? nil : .destructive
            ) {
                issueApprovedCode()
            }
            Button("Cancel", role: .cancel) { approvedIssueSnapshot = nil }
        } message: {
            Text(issueConfirmationMessage)
        }
        .confirmationDialog(
            "Recover this Mac?",
            isPresented: $recoveryConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Recover This Mac", role: .destructive) {
                consumeEnteredCode()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Recovery revokes the account's existing rotating device sessions, pending device enrollments, and local MCP clients, then installs a new full-owner session and one-use successor code. Planner data and Google account data are preserved. Pending protected work must be reconciled first.")
        }
        .confirmationDialog(
            "Abandon the saved recovery attempt?",
            isPresented: $discardPendingConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Abandon Exact Attempt", role: .destructive) {
                Task { @MainActor in
                    _ = await durableAuth.discardPendingAccountRecoveryConsumption()
                }
            }
            Button("Keep Saved Attempt", role: .cancel) {}
        } message: {
            Text("Only do this if you are certain recovery did not commit or you intentionally accept losing this exact generated tuple. A consumed code cannot be reused with a different tuple.")
        }
        .confirmationDialog(
            "Delete the quarantined recovery journal?",
            isPresented: $discardIncompatibleConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Delete Recovery Journal", role: .destructive) {
                Task { @MainActor in
                    _ = await durableAuth.discardIncompatibleAccountRecoveryJournal()
                }
            }
            Button("Keep Quarantined Journal", role: .cancel) {}
        } message: {
            Text("This removes only the unreadable recovery Keychain item. It may contain the only copy of unfinished recovery authority. Authentication, account data, and local planner data are not deleted.")
        }
        .confirmationDialog(
            "Abandon the saved code update?",
            isPresented: $discardIssueConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Abandon Code Update", role: .destructive) {
                Task { @MainActor in
                    _ = await durableAuth.discardPendingAccountRecoveryIssue()
                }
            }
            Button("Keep Exact Update", role: .cancel) {}
        } message: {
            Text("The server may already have activated the generated code even though this Mac did not receive a valid response. Abandoning the journal destroys this Mac's only copy; refresh and replace recovery protection from an authorized device afterward.")
        }
    }

    @ViewBuilder
    private var recoveryJournalContent: some View {
        switch durableAuth.accountRecoveryPresentation.phase {
        case .idle:
            EmptyView()
        case .issuePending:
            journalStatus(systemImage: "arrow.triangle.2.circlepath")
            Button("Resume Exact Code Update") { resumeNonReplacingWork() }
                .disabled(isBusy)
                .accessibilityIdentifier("settings.account-recovery.resume-issue")
            Button("Abandon Saved Code Update…", role: .destructive) {
                discardIssueConfirmationPresented = true
            }
            .disabled(isBusy)
            .accessibilityIdentifier("settings.account-recovery.discard-issue")
        case .consumePending:
            journalStatus(systemImage: "key.horizontal")
            Button("Finish Account Recovery") { resumeReplacingWork() }
                .buttonStyle(.borderedProminent)
                .disabled(isBusy || credentialReplacementDisabled)
                .accessibilityIdentifier("settings.account-recovery.resume-consume")
            Button("Abandon Saved Attempt…", role: .destructive) {
                discardPendingConfirmationPresented = true
            }
            .disabled(isBusy)
            .accessibilityIdentifier("settings.account-recovery.discard-pending")
        case .committedAwaitingInstallation, .installedAwaitingHandoff:
            journalStatus(systemImage: "key.horizontal")
            Button("Finish Account Recovery") { resumeReplacingWork() }
                .buttonStyle(.borderedProminent)
                .disabled(isBusy || credentialReplacementDisabled)
                .accessibilityIdentifier("settings.account-recovery.resume-committed")
        case .awaitingAcknowledgement:
            awaitingAcknowledgementContent
        case .incompatible:
            journalStatus(systemImage: "exclamationmark.shield")
            Button("Delete Quarantined Recovery Journal…", role: .destructive) {
                discardIncompatibleConfirmationPresented = true
            }
            .disabled(isBusy)
            .accessibilityIdentifier("settings.account-recovery.discard-incompatible")
        }
    }

    private func journalStatus(systemImage: String) -> some View {
        Label {
            VStack(alignment: .leading, spacing: 3) {
                Text(durableAuth.accountRecoveryPresentation.title)
                    .font(.body.weight(.semibold))
                Text(durableAuth.accountRecoveryPresentation.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        } icon: {
            Image(systemName: systemImage).foregroundStyle(Color.accentColor)
        }
        .accessibilityIdentifier("settings.account-recovery.journal-status")
    }

    @ViewBuilder
    private var awaitingAcknowledgementContent: some View {
        journalStatus(systemImage: "key.viewfinder")
        if let metadata = durableAuth.accountRecoveryPresentation.awaitingMetadata {
            LabeledContent(
                "Created",
                value: metadata.createdAt.formatted(date: .abbreviated, time: .shortened)
            )
        }

        GroupBox("One-use recovery code") {
            VStack(alignment: .leading, spacing: 10) {
                if let code = durableAuth.revealedAccountRecoveryCode {
                    Text(code)
                        .font(.system(.body, design: .monospaced))
                        .privacySensitive()
                        .accessibilityLabel("Revealed account recovery code")
                        .accessibilityIdentifier("settings.account-recovery.revealed-code")
                    HStack {
                        Button("Copy") { copy(code) }
                            .accessibilityIdentifier("settings.account-recovery.copy")
                        Button("Hide") {
                            clearClipboardIfOwned()
                            durableAuth.clearAccountRecoveryMemory()
                        }
                        .accessibilityIdentifier("settings.account-recovery.hide")
                    }
                } else {
                    Text("•••• •••• •••• ••••")
                        .font(.system(.body, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .accessibilityLabel("Recovery code hidden")
                    Button("Reveal Recovery Code") { reveal() }
                        .accessibilityIdentifier("settings.account-recovery.reveal")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }

        Toggle("I saved this code somewhere private", isOn: $acknowledgedSavedCopy)
            .accessibilityIdentifier("settings.account-recovery.saved-confirmation")
        Button("Finish and Hide Code") { acknowledge() }
            .buttonStyle(.borderedProminent)
            .disabled(!acknowledgedSavedCopy || isBusy)
            .accessibilityIdentifier("settings.account-recovery.acknowledge")
    }

    @ViewBuilder
    private var currentCodeContent: some View {
        HStack(alignment: .firstTextBaseline) {
            if let snapshot = durableAuth.accountRecoverySnapshot {
                if let current = snapshot.recoveryCode {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Recovery code is active")
                            .font(.body.weight(.semibold))
                        Text("Created \(current.createdAt.formatted(date: .abbreviated, time: .shortened))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } else {
                    Label("No recovery code is active.", systemImage: "shield.slash")
                        .foregroundStyle(.secondary)
                }
            } else if durableAuth.isRefreshingAccountRecovery {
                ProgressView().controlSize(.small)
                Text("Checking recovery protection…").foregroundStyle(.secondary)
            } else if canReadCurrentCode {
                Text("Connect to check the server's current recovery state.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Text("A full-owner rotating session can generate recovery protection.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 12)
            Button {
                refresh()
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(!canReadCurrentCode || isBusy)
            .accessibilityIdentifier("settings.account-recovery.refresh")
        }

        if durableAuth.accountRecoverySnapshotIsStale {
            Label(
                "The recovery status may be stale. Refresh before generating or replacing a code.",
                systemImage: "wifi.exclamationmark"
            )
            .font(.caption)
            .foregroundStyle(.orange)
            .accessibilityIdentifier("settings.account-recovery.stale")
        }

        if let snapshot = durableAuth.accountRecoverySnapshot {
            if snapshot.recoveryCode == nil {
                Button("Generate Recovery Code…") { approvedIssueSnapshot = snapshot }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                        isBusy
                            || durableAuth.accountRecoverySnapshotIsStale
                            || durableAuth.presentation.phase != .active
                    )
                    .accessibilityIdentifier("settings.account-recovery.issue")
            } else {
                Button("Replace Recovery Code…") { approvedIssueSnapshot = snapshot }
                    .buttonStyle(.bordered)
                    .disabled(
                        isBusy
                            || durableAuth.accountRecoverySnapshotIsStale
                            || durableAuth.presentation.phase != .active
                    )
                    .accessibilityIdentifier("settings.account-recovery.issue")
            }
        }
    }

    private var recoveryInputContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            Divider()
            Text("Recover with a saved code")
                .font(.body.weight(.semibold))
            SecureField("Account recovery code (dw_rc1_…)", text: $enteredRecoveryCode)
                .textContentType(.password)
                .privacySensitive()
                .disabled(isBusy || credentialReplacementDisabled)
                .accessibilityIdentifier("settings.account-recovery.input")
            Button("Recover This Mac…") {
                recoveryConfirmationPresented = true
            }
            .disabled(
                enteredRecoveryCode.isEmpty
                    || isBusy
                    || credentialReplacementDisabled
            )
            .accessibilityIdentifier("settings.account-recovery.consume")
        }
    }

    private var baseURL: DayWeaveAPIBaseURL? {
        try? DayWeaveAPIBaseURL(baseURLString)
    }

    private var canReadCurrentCode: Bool {
        switch durableAuth.presentation.phase {
        case .active, .refreshPending:
            true
        case .notConfigured, .legacy, .enrollmentCreationPending, .enrollmentPending,
             .reauthenticationRequired, .incompatible:
            false
        }
    }

    private var isBusy: Bool {
        durableAuth.isBusy
            || durableAuth.isManagingDeviceSessions
            || durableAuth.isManagingAccountRecovery
    }

    private var refreshIdentity: String {
        "\(baseURLString)|\(durableAuth.presentation.phase.rawValue)|\(durableAuth.accountRecoveryPresentation.phase.rawValue)"
    }

    private var issueConfirmationIsPresented: Binding<Bool> {
        Binding(
            get: { approvedIssueSnapshot != nil },
            set: { if !$0 { approvedIssueSnapshot = nil } }
        )
    }

    private var issueConfirmationTitle: String {
        approvedIssueSnapshot?.recoveryCode == nil
            ? "Generate an account recovery code?"
            : "Replace the current recovery code?"
    }

    private var issueConfirmationMessage: String {
        approvedIssueSnapshot?.recoveryCode == nil
            ? "DayWeave will generate the secret on this Mac and save the exact request before sending it. You must store the displayed code somewhere private."
            : "The existing code becomes unusable as soon as replacement commits. DayWeave will not hide the new code until you confirm that you saved it."
    }

    private func refresh() {
        guard let baseURL else { return }
        Task { @MainActor in
            _ = await durableAuth.refreshAccountRecoveryCode(baseURL: baseURL)
        }
    }

    private func issueApprovedCode() {
        guard let snapshot = approvedIssueSnapshot, let baseURL else { return }
        approvedIssueSnapshot = nil
        Task { @MainActor in
            guard durableAuth.accountRecoverySnapshot == snapshot,
                  durableAuth.coordinator.isAccountRecoverySnapshotCurrent(
                      snapshot,
                      boundTo: baseURL
                  ) else {
                refresh()
                return
            }
            _ = await durableAuth.issueAccountRecoveryCode(
                baseURL: baseURL,
                approvedFrom: snapshot
            )
        }
    }

    private func consumeEnteredCode() {
        let code = enteredRecoveryCode
        Task { @MainActor in
            if await onRecover(code) {
                enteredRecoveryCode = ""
            }
        }
    }

    private func resumeNonReplacingWork() {
        guard let baseURL else { return }
        Task { @MainActor in
            _ = await durableAuth.resumeAccountRecovery(baseURL: baseURL)
        }
    }

    private func resumeReplacingWork() {
        Task { @MainActor in _ = await onResumeCredentialReplacement() }
    }

    private func reveal() {
        guard let baseURL else { return }
        Task { @MainActor in
            await durableAuth.revealAccountRecoveryCode(baseURL: baseURL)
        }
    }

    private func acknowledge() {
        guard let baseURL else { return }
        clearClipboardIfOwned()
        Task { @MainActor in
            if await durableAuth.acknowledgeAccountRecoveryCode(baseURL: baseURL) {
                acknowledgedSavedCopy = false
                refresh()
            }
        }
    }

    private func copy(_ code: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.setString(code, forType: .string) else { return }
        clipboardChangeCount = pasteboard.changeCount
        let expectedChangeCount = pasteboard.changeCount
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(60))
            guard clipboardChangeCount == expectedChangeCount else { return }
            clearClipboardIfOwned()
        }
    }

    private func clearTransientSecrets() {
        enteredRecoveryCode = ""
        approvedIssueSnapshot = nil
        recoveryConfirmationPresented = false
        discardPendingConfirmationPresented = false
        discardIssueConfirmationPresented = false
        discardIncompatibleConfirmationPresented = false
        acknowledgedSavedCopy = false
        clearClipboardIfOwned()
    }

    private func clearClipboardIfOwned() {
        guard let clipboardChangeCount else { return }
        let pasteboard = NSPasteboard.general
        if pasteboard.changeCount == clipboardChangeCount {
            pasteboard.clearContents()
        }
        self.clipboardChangeCount = nil
    }
}
