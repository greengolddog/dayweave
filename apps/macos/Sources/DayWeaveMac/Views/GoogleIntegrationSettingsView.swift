import AppKit
import SwiftUI

struct GoogleIntegrationSettingsView: View {
    @EnvironmentObject private var store: GoogleIntegrationStore
    @EnvironmentObject private var outbound: GoogleOutboundStore
    @State private var disconnectCandidate: GoogleAccount?
    @State private var resetRecoveryConfirmationPresented = false
    @State private var abandonRecoveryConfirmationPresented = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            statusHeader

            if hasCleanupWarning {
                Label {
                    Text("Google account cleanup is still being completed securely. Some account actions may be temporarily unavailable; check again later.")
                } icon: {
                    Image(systemName: "exclamationmark.shield")
                }
                .font(.caption)
                .foregroundStyle(.orange)
                .accessibilityIdentifier("google.cleanup.warning")
            }

            if outbound.hasPendingRecovery {
                VStack(alignment: .leading, spacing: 7) {
                    Label(
                        "Recover the encrypted Google publication from its Inbox item before changing Google accounts or source policies.",
                        systemImage: "arrow.up.circle.fill"
                    )
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
                    if outbound.hasApprovedRecovery {
                        Button(outbound.status == .expired
                            ? "Check Google acceptance"
                            : "Recover approved Google change") {
                            Task { _ = await outbound.recoverPendingOperation() }
                        }
                        .controlSize(.small)
                        .disabled(outbound.status.isWorking)
                        .accessibilityIdentifier(
                            "google.outbound.settings-check-acceptance"
                        )
                    }
                    if outbound.status == .expired {
                        GoogleExpiredRecoveryDiscardButton(
                            title: "Discard expired Google recovery",
                            accessibilityIdentifier: "google.outbound.settings-discard-expired"
                        )
                        .controlSize(.small)
                    }
                }
                .accessibilityIdentifier("google.outbound.settings-recovery")
            }

            connectionActions

            if store.recoveryResetRequired {
                Button("Reset unreadable Google recovery…", role: .destructive) {
                    resetRecoveryConfirmationPresented = true
                }
                .controlSize(.small)
                .disabled(
                    store.isBusy || store.credentialTransitionInProgress
                        || outbound.hasPendingRecovery
                )
                .accessibilityIdentifier("google.recovery.reset")
            }

            if store.orphanedRecoveryRequiresConfirmation {
                Button("Abandon orphaned Google recovery…", role: .destructive) {
                    abandonRecoveryConfirmationPresented = true
                }
                .controlSize(.small)
                .disabled(
                    store.isBusy || store.credentialTransitionInProgress
                        || outbound.hasPendingRecovery
                )
                .accessibilityIdentifier("google.recovery.abandon")
            }

            if store.accounts.isEmpty {
                Text("Connect Google Calendar and Google Tasks to choose which sources DayWeave may import. Calendar and Tasks publishing are enabled separately, each with explicit Google approval.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("google.accounts.empty")
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(store.accounts) { account in
                        GoogleAccountSettingsCard(
                            account: account,
                            ordinal: accountOrdinal(account),
                            requestDisconnect: { disconnectCandidate = account }
                        )
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .confirmationDialog(
            disconnectConfirmationTitle,
            isPresented: disconnectConfirmationBinding,
            titleVisibility: .visible
        ) {
            Button(disconnectConfirmationActionTitle, role: .destructive) {
                guard let candidate = disconnectCandidate,
                      let current = store.accounts.first(where: { $0.id == candidate.id }) else {
                    disconnectCandidate = nil
                    return
                }
                disconnectCandidate = nil
                Task { await store.disconnectGoogleAccount(current) }
            }
            .accessibilityIdentifier("google.disconnect.confirm")

            Button("Cancel", role: .cancel) {
                disconnectCandidate = nil
            }
        } message: {
            Text(disconnectConfirmationMessage)
        }
        .confirmationDialog(
            "Reset unreadable Google recovery?",
            isPresented: $resetRecoveryConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Reset recovery", role: .destructive) {
                Task { await store.resetUnreadableRecovery() }
            }
            .accessibilityIdentifier("google.recovery.reset.confirm")

            Button("Cancel", role: .cancel) {}
        } message: {
            Text("DayWeave first verifies a fresh schedule composition, then removes only unreadable local, non-secret recovery data. An accepted import or exact disconnect may no longer be retryable from this Mac. Check authoritative server status before making another change.")
        }
        .confirmationDialog(
            "Abandon orphaned Google recovery?",
            isPresented: $abandonRecoveryConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Abandon recovery", role: .destructive) {
                Task { await store.abandonOrphanedRecovery() }
            }
            .accessibilityIdentifier("google.recovery.abandon.confirm")

            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The saved request belongs to a previous authenticated DayWeave API session, so this session cannot prove whether it completed. This removes only the orphaned local, non-secret marker; the remote operation may still exist. Restore the previous session instead if possible.")
        }
        .accessibilityIdentifier("google.integration.settings")
    }

    private var statusHeader: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 8)
                    .fill(statusColor.opacity(0.13))
                    .frame(width: 34, height: 34)
                if store.isBusy || store.credentialTransitionInProgress {
                    ProgressView()
                        .controlSize(.small)
                } else {
                    Image(systemName: statusSymbol)
                        .foregroundStyle(statusColor)
                }
            }

            VStack(alignment: .leading, spacing: 3) {
                Text("Google Calendar & Tasks")
                    .font(.subheadline.weight(.semibold))
                Text(store.status.message)
                    .font(.caption)
                    .foregroundStyle(statusColor)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("google.status.message")
            }

            Spacer(minLength: 8)

            Button {
                Task { await store.reload() }
            } label: {
                Label("Check status", systemImage: "arrow.clockwise")
            }
            .controlSize(.small)
            .disabled(
                store.isBusy || store.credentialTransitionInProgress
                    || outbound.hasPendingRecovery
                    || statusIsPrivacyProtected
            )
            .help("Reload connected accounts, source policies, and import status")
            .accessibilityIdentifier("google.status.reload")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("google.status.header")
    }

    private var connectionActions: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 8) {
                if store.canRetryAuthorization {
                    Button {
                        Task { await store.retryExactAuthorizationRequest() }
                    } label: {
                        Label("Retry exact request", systemImage: "arrow.counterclockwise")
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("google.authorization.retry-exact")
                } else if store.canOpenAuthorization {
                    Button {
                        _ = store.openAuthorizationPage()
                    } label: {
                        Label("Open Google", systemImage: "safari")
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("google.authorization.open")
                } else if store.canCheckAuthorization {
                    Button {
                        Task { await store.checkAuthorization() }
                    } label: {
                        Label("Check connection", systemImage: "person.crop.circle.badge.checkmark")
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("google.authorization.check")
                } else {
                    Button {
                        Task { await store.connectGoogleAccount() }
                    } label: {
                        Label(
                            store.accounts.isEmpty
                                ? "Connect Calendar & Tasks"
                                : "Connect another account",
                            systemImage: "person.crop.circle.badge.plus"
                        )
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!canStartConnection)
                    .help("Requests Google Calendar and Google Tasks read access; publishing for each service is a separate approval")
                    .accessibilityIdentifier("google.authorization.connect")
                }

                if store.isBusy {
                    Text("Working securely…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .controlSize(.small)
            .disabled(
                store.isBusy || store.credentialTransitionInProgress
                    || outbound.hasPendingRecovery
                    || store.mutationRecoveryRequired
                    || store.disconnectRecoveryRequiresAttention
                    || store.refreshCompletionRecoveryResetRequired
            )

            if store.canOpenAuthorization {
                Text("DayWeave will open Google in your browser. The private authorization address is never shown or stored with planner content.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if store.canRetryAuthorization {
                Text("The prior result is uncertain. Retrying reuses the exact saved request; DayWeave will not create a second authorization attempt.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if store.canCheckAuthorization {
                Text("Finish the browser flow, then check for updated accounts. Until the saved request expires, DayWeave keeps recovery open because the accounts endpoint cannot prove which browser attempt completed.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityIdentifier("google.authorization.actions")
    }

    private var disconnectConfirmationBinding: Binding<Bool> {
        Binding(
            get: { disconnectCandidate != nil },
            set: { isPresented in
                if !isPresented { disconnectCandidate = nil }
            }
        )
    }

    private var disconnectConfirmationIsRetry: Bool {
        guard let candidate = disconnectCandidate else { return false }
        return store.hasPendingDisconnectRecovery(for: candidate)
    }

    private var disconnectConfirmationTitle: String {
        disconnectConfirmationIsRetry
            ? "Retry exact Google disconnect?"
            : "Disconnect Google account?"
    }

    private var disconnectConfirmationActionTitle: String {
        disconnectConfirmationIsRetry ? "Retry exact request" : "Disconnect"
    }

    private var disconnectConfirmationMessage: String {
        disconnectConfirmationIsRetry
            ? "DayWeave will reuse the exact saved disconnect request identity and revision. It will not create a second disconnect operation."
            : "This stops future imports and asks DayWeave to revoke the server-held Google connection. You will need to connect again to resume syncing."
    }

    private var hasCleanupWarning: Bool {
        guard let cleanup = store.cleanupStatus else { return false }
        return cleanup.held > 0
            || cleanup.pending > 0
            || cleanup.retrying > 0
            || cleanup.exhausted > 0
            || cleanup.volatileGuardians > 0
            || cleanup.durabilityDegraded
            || cleanup.revocationFenced
            || cleanup.operatorRecoveryRequired
            || cleanup.uncertainAuthorizations > 0
            || cleanup.legacyRecoveryRequired > 0
    }

    private var canStartConnection: Bool {
        guard !store.isBusy,
              !store.credentialTransitionInProgress,
              !outbound.hasPendingRecovery,
              !store.hasPendingRecovery,
              !store.authorizationStartIsFenced,
              !store.canOpenAuthorization,
              !store.canRetryAuthorization,
              !store.canCheckAuthorization,
              !store.accounts.contains(where: {
                $0.status == .disconnecting || $0.status == .revocationFailed
              }) else { return false }
        switch store.status {
        case .privacyProtected, .configurationRequired:
            return false
        case .ready, .loading, .awaitingAuthorization, .authorizationOutcomeUnknown,
             .connected, .refreshQueued, .offline, .failed:
            return true
        }
    }

    private var statusIsPrivacyProtected: Bool {
        if case .privacyProtected = store.status { return true }
        return false
    }

    private var statusSymbol: String {
        switch store.status {
        case .privacyProtected: "lock.fill"
        case .configurationRequired: "gearshape.fill"
        case .ready: "link.badge.plus"
        case .loading: "arrow.triangle.2.circlepath"
        case .awaitingAuthorization, .authorizationOutcomeUnknown: "person.badge.key.fill"
        case .connected: "checkmark.circle.fill"
        case .refreshQueued: "clock.arrow.circlepath"
        case .offline: "wifi.slash"
        case .failed: "exclamationmark.triangle.fill"
        }
    }

    private var statusColor: Color {
        switch store.status {
        case .privacyProtected, .configurationRequired, .ready:
            .secondary
        case .loading:
            .blue
        case .awaitingAuthorization, .authorizationOutcomeUnknown:
            .purple
        case .connected:
            .green
        case .refreshQueued:
            .orange
        case .offline, .failed:
            .red
        }
    }

    private func accountOrdinal(_ account: GoogleAccount) -> Int {
        (store.accounts.firstIndex(where: { $0.id == account.id }) ?? 0) + 1
    }
}

private struct GoogleAccountSettingsCard: View {
    @EnvironmentObject private var store: GoogleIntegrationStore
    @EnvironmentObject private var outbound: GoogleOutboundStore
    @State private var sourcesAreExpanded = false

    let account: GoogleAccount
    let ordinal: Int
    let requestDisconnect: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            accountHeader

            Text(accountDetail)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            accountActions

            if account.status == .active {
                activeAccountControls

                if let sync = store.syncStatusByAccount[account.id] {
                    GoogleSyncSummaryView(sync: sync, accountOrdinal: ordinal)
                } else {
                    Text("Import status has not been loaded yet.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("google.account.\(ordinal).sync.empty")
                }

                DisclosureGroup(isExpanded: $sourcesAreExpanded) {
                    sourceList
                        .padding(.top, 7)
                } label: {
                    Label(
                        "Sources (\(collections.count))",
                        systemImage: "rectangle.stack"
                    )
                    .font(.caption.weight(.semibold))
                }
                .accessibilityIdentifier("google.account.\(ordinal).sources")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            Color(nsColor: .controlBackgroundColor).opacity(0.72),
            in: RoundedRectangle(cornerRadius: 12)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .stroke(accountBorderColor.opacity(0.28), lineWidth: 1)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("google.account.\(ordinal)")
    }

    private var accountHeader: some View {
        HStack(alignment: .center, spacing: 9) {
            Image(systemName: "person.crop.circle.fill")
                .font(.title3)
                .foregroundStyle(accountBorderColor)

            Text(account.displayLabel)
                .font(.subheadline.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.middle)

            Spacer(minLength: 6)

            HStack(spacing: 5) {
                if account.isDefault {
                    GoogleStatusBadge(title: "Default", color: .blue)
                }
                GoogleStatusBadge(title: accountStatusTitle, color: accountBorderColor)
            }
        }
    }

    private var accountActions: some View {
        HStack(spacing: 7) {
            if store.hasPendingDisconnectRecovery(for: account) {
                Spacer(minLength: 4)
                Button("Retry exact disconnect", role: .destructive) {
                    requestDisconnect()
                }
                .accessibilityIdentifier("google.account.\(ordinal).disconnect-retry")
            } else {
                if store.canEnableCalendarPublishing(for: account) {
                    Button("Enable Calendar publishing") {
                        Task { await store.enableCalendarPublishing(for: account) }
                    }
                    .accessibilityIdentifier(
                        "google.account.\(ordinal).enable-calendar-publishing"
                    )
                }

                if store.canEnableTasksPublishing(for: account) {
                    Button(tasksPublishingActionTitle) {
                        Task { await store.enableTasksPublishing(for: account) }
                    }
                    .accessibilityIdentifier(
                        "google.account.\(ordinal).enable-tasks-publishing"
                    )
                }

                if canReauthorize
                    && (!pendingRefreshRecovery
                        || store.requiresReauthorization(for: account)) {
                    Button("Reauthorize") {
                        Task { await store.reauthorizeGoogleAccount(account) }
                    }
                    .accessibilityIdentifier("google.account.\(ordinal).reauthorize")
                }

                if account.status == .active, !pendingRefreshRecovery {
                    Button("Pause") {
                        Task { await store.setAccountPaused(account, paused: true) }
                    }
                    .accessibilityIdentifier("google.account.\(ordinal).pause")
                } else if account.status == .paused {
                    Button("Resume") {
                        Task { await store.setAccountPaused(account, paused: false) }
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("google.account.\(ordinal).resume")
                }

                Spacer(minLength: 4)

                if canDisconnect, !pendingRefreshRecovery {
                    Button("Disconnect", role: .destructive, action: requestDisconnect)
                        .accessibilityIdentifier("google.account.\(ordinal).disconnect")
                }
            }
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .disabled(
            store.isBusy || store.credentialTransitionInProgress
                || outbound.hasPendingRecovery
                || store.mutationRecoveryRequired || store.hasPendingAuthorizationRecovery
                || store.disconnectRecoveryResetRequired
                || store.refreshCompletionRecoveryResetRequired
                || (pendingRefreshRecovery
                    && account.status != .paused
                    && !store.requiresReauthorization(for: account))
                || (store.disconnectRecoveryRequiresAttention
                    && !store.hasPendingDisconnectRecovery(for: account))
        )
    }

    private var activeAccountControls: some View {
        HStack(spacing: 7) {
            Button {
                Task { await store.discoverSources(for: account) }
            } label: {
                Label("Discover sources", systemImage: "sparkle.magnifyingglass")
            }
            .accessibilityIdentifier("google.account.\(ordinal).discover")

            Button {
                Task { await store.refreshImports(for: account) }
            } label: {
                Label(
                    store.canRetryPendingRefresh(for: account)
                        ? "Retry import request"
                        : (pendingRefreshRecovery ? "Import pending" : "Refresh import"),
                    systemImage: store.canRetryPendingRefresh(for: account)
                        ? "arrow.counterclockwise.circle"
                        : "arrow.down.circle"
                )
            }
            .buttonStyle(.borderedProminent)
            .disabled(
                pendingRefreshRecovery && !store.canRetryPendingRefresh(for: account)
            )
            .accessibilityIdentifier("google.account.\(ordinal).refresh-import")

            Spacer(minLength: 0)
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .disabled(
            store.isBusy || store.credentialTransitionInProgress
                || outbound.hasPendingRecovery
                || store.mutationRecoveryRequired || store.hasPendingAuthorizationRecovery
                || store.disconnectRecoveryRequiresAttention
                || store.refreshCompletionRecoveryResetRequired
        )
    }

    @ViewBuilder
    private var sourceList: some View {
        if collections.isEmpty {
            Text("No Calendar or Tasks sources are available yet. Discover sources to load the server’s current inventory.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("google.account.\(ordinal).sources.empty")
        } else {
            VStack(alignment: .leading, spacing: 7) {
                ForEach(collections) { collection in
                    GoogleSourceSettingsRow(
                        collection: collection,
                        accountOrdinal: ordinal,
                        sourceOrdinal: sourceOrdinal(collection)
                    )
                }
            }
        }
    }

    private var collections: [GoogleSyncCollection] {
        store.collectionsByAccount[account.id] ?? []
    }

    private var canReauthorize: Bool {
        guard !store.authorizationStartIsFenced else { return false }
        return switch account.status {
        case .active, .paused, .reauthorizationRequired:
            true
        case .disconnecting, .revocationFailed, .revoked:
            false
        }
    }

    private var pendingRefreshRecovery: Bool {
        store.hasPendingRefreshCompletion(for: account)
    }

    private var canDisconnect: Bool {
        store.hasPendingDisconnectRecovery(for: account)
            || account.status == .active
            || account.status == .paused
            || account.status == .reauthorizationRequired
    }

    private var accountStatusTitle: String {
        switch account.status {
        case .active: "Connected"
        case .paused: "Paused"
        case .reauthorizationRequired: "Reconnect"
        case .disconnecting: "Disconnecting"
        case .revocationFailed: "Needs attention"
        case .revoked: "Disconnected"
        }
    }

    private var accountDetail: String {
        switch account.status {
        case .active:
            switch (
                store.hasCalendarPublishingScope(for: account),
                store.hasTasksPublishingScope(for: account)
            ) {
            case (true, true):
                "Calendar and Tasks publishing are authorized. Only sources explicitly marked Publish can receive separately reviewed DayWeave items."
            case (true, false):
                "Calendar publishing is authorized. Tasks remain import-only until their separate Google approval is completed."
            case (false, true):
                "Tasks publishing is authorized. Calendars remain import-only until their separate Google approval is completed."
            case (false, false):
                "Calendar and Tasks imports are enabled for selected sources. Publishing for each service needs a separate approval."
            }
        case .paused:
            "Imports are paused. Existing DayWeave data remains available."
        case .reauthorizationRequired:
            "Google access must be renewed before imports can continue."
        case .disconnecting:
            "The server is securely finishing this disconnection."
        case .revocationFailed:
            "The server could not finish revocation safely. Check status before retrying."
        case .revoked:
            "This Google connection is no longer active."
        }
    }

    private var tasksPublishingActionTitle: String {
        store.hasTasksPublishingScope(for: account)
            && store.requiresReauthorization(for: account)
            ? "Reauthorize Tasks publishing"
            : "Enable Tasks publishing"
    }

    private var accountBorderColor: Color {
        switch account.status {
        case .active: .green
        case .paused: .orange
        case .reauthorizationRequired, .revocationFailed: .red
        case .disconnecting, .revoked: .secondary
        }
    }

    private func sourceOrdinal(_ collection: GoogleSyncCollection) -> Int {
        (collections.firstIndex(where: { $0.id == collection.id }) ?? 0) + 1
    }
}

private struct GoogleSourceSettingsRow: View {
    @EnvironmentObject private var store: GoogleIntegrationStore
    @EnvironmentObject private var outbound: GoogleOutboundStore

    let collection: GoogleSyncCollection
    let accountOrdinal: Int
    let sourceOrdinal: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 7) {
                Image(systemName: kindSymbol)
                    .foregroundStyle(kindColor)
                Text(collection.displayName)
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 5)
                GoogleStatusBadge(title: kindTitle, color: kindColor)
                if collection.providerPrimary {
                    GoogleStatusBadge(title: "Primary", color: .blue)
                }
                if collection.providerDeleted {
                    GoogleStatusBadge(title: "Deleted", color: .red)
                }
            }

            if requiresReadOnlyDowngrade {
                Label(
                    "This source has a publishing policy that is no longer supported by its current Google grant or provider role.",
                    systemImage: "exclamationmark.shield"
                )
                .font(.caption2)
                .foregroundStyle(.orange)
                .fixedSize(horizontal: false, vertical: true)

                Button("Switch to read-only import") {
                    Task {
                        await store.configureSource(
                            collection,
                            selected: collection.selected,
                            visible: collection.visible,
                            role: .readOnly
                        )
                    }
                }
                .controlSize(.small)
                .accessibilityIdentifier(sourceIdentifier("downgrade-read-only"))
            } else {
                HStack(spacing: 14) {
                    Toggle("Import", isOn: selectedBinding)
                        .accessibilityIdentifier(sourceIdentifier("selected"))
                    Toggle("Visible", isOn: visibleBinding)
                        .accessibilityIdentifier(sourceIdentifier("visible"))
                    Spacer(minLength: 0)
                }
                .toggleStyle(.switch)
                .controlSize(.small)

                if collection.kind == .calendar {
                    Picker("Scheduling", selection: roleBinding) {
                        Text("Reference").tag(GoogleSyncRole.readOnly)
                        Text("Blocks time").tag(GoogleSyncRole.blocking)
                        if canPublishToCollection {
                            Text("Publish").tag(GoogleSyncRole.writable)
                        }
                    }
                    .pickerStyle(.segmented)
                    .controlSize(.small)
                    .accessibilityIdentifier(sourceIdentifier("role"))
                    .help("Reference sources are context, blocking calendars reserve time, and Publish calendars may receive explicitly reviewed DayWeave events")

                    if collection.syncRole == .writable {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Optional event types")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.secondary)
                            HStack(spacing: 12) {
                                Toggle("All-day", isOn: publishAllDayBinding)
                                Toggle("Tentative", isOn: publishTentativeBinding)
                                Toggle("Free", isOn: publishFreeBinding)
                            }
                            .toggleStyle(.checkbox)
                            .controlSize(.small)
                            Text("Confirmed busy timed events are allowed by default. Every external change still requires a separate preview and approval.")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .accessibilityIdentifier(sourceIdentifier("publication-policy"))
                    }
                } else if canPublishToCollection {
                    Picker("Tasks", selection: roleBinding) {
                        Text("Reference").tag(GoogleSyncRole.readOnly)
                        Text("Publish").tag(GoogleSyncRole.writable)
                    }
                    .pickerStyle(.segmented)
                    .controlSize(.small)
                    .accessibilityIdentifier(sourceIdentifier("role"))
                    .help("Reference Task lists import context, while Publish Task lists may receive separately reviewed DayWeave tasks; Task lists never block calendar time")
                } else {
                    VStack(alignment: .leading, spacing: 3) {
                        LabeledContent("Tasks", value: "Reference only")
                        if !accountHasTasksPublishingScope {
                            Text("Enable Tasks publishing for this account before marking a Task list Publish.")
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(sourceIdentifier("role-read-only"))
                }
            }

            if collection.providerDeleted {
                Text("Google reports that this source was deleted. Its import policy is shown for reference and cannot be changed.")
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let lastImportAt = collection.lastImportAt {
                Text("Last imported \(lastImportAt.formatted(date: .abbreviated, time: .shortened))")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 9))
        .disabled(
            store.isBusy || store.credentialTransitionInProgress
                || outbound.hasPendingRecovery
                || store.mutationRecoveryRequired
                || store.hasPendingAuthorizationRecovery
                || store.disconnectRecoveryRequiresAttention
                || store.refreshCompletionRecoveryResetRequired
                || collection.providerDeleted
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(sourceIdentifier("row"))
    }

    private var selectedBinding: Binding<Bool> {
        Binding(
            get: { collection.selected },
            set: { selected in
                Task {
                    await store.configureSource(
                        collection,
                        selected: selected,
                        visible: collection.visible,
                        role: selected ? collection.syncRole : .readOnly
                    )
                }
            }
        )
    }

    private var visibleBinding: Binding<Bool> {
        Binding(
            get: { collection.visible },
            set: { visible in
                Task {
                    await store.configureSource(
                        collection,
                        selected: collection.selected,
                        visible: visible,
                        role: collection.syncRole
                    )
                }
            }
        )
    }

    private var roleBinding: Binding<GoogleSyncRole> {
        Binding(
            get: { collection.syncRole },
            set: { role in
                Task {
                    await store.configureSource(
                        collection,
                        selected: role == .writable ? true : collection.selected,
                        visible: collection.visible,
                        role: role
                    )
                }
            }
        )
    }

    private var requiresReadOnlyDowngrade: Bool {
        (collection.syncRole == .writable && !canPublishToCollection)
            || (collection.syncRole != .writable && !collection.calendarPolicy.isReadOnlySafe)
            || (collection.kind == .taskList && collection.syncRole == .blocking)
            || (collection.kind == .taskList && !collection.calendarPolicy.isReadOnlySafe)
    }

    private var canPublishToCollection: Bool {
        guard !collection.providerDeleted,
              let account = store.accounts.first(where: { $0.id == collection.accountID }) else {
            return false
        }
        switch collection.kind {
        case .calendar:
            guard store.hasCalendarPublishingScope(for: account),
                  let access = collection.providerAccessRole?.lowercased() else {
                return false
            }
            return access == "owner" || access == "writer"
        case .taskList:
            return store.hasTasksPublishingScope(for: account)
        }
    }

    private var accountHasTasksPublishingScope: Bool {
        store.accounts.first(where: { $0.id == collection.accountID }).map {
            store.hasTasksPublishingScope(for: $0)
        } == true
    }

    private var publishAllDayBinding: Binding<Bool> {
        publicationBinding(\.publishAllDay)
    }

    private var publishTentativeBinding: Binding<Bool> {
        publicationBinding(\.publishTentative)
    }

    private var publishFreeBinding: Binding<Bool> {
        publicationBinding(\.publishFree)
    }

    private func publicationBinding(
        _ keyPath: KeyPath<GoogleCalendarPolicy, Bool>
    ) -> Binding<Bool> {
        Binding(
            get: { collection.calendarPolicy[keyPath: keyPath] },
            set: { value in
                let current = collection.calendarPolicy
                let policy = GoogleCalendarPolicy(
                    confirmedBusy: current.confirmedBusy,
                    tentative: current.tentative,
                    free: current.free,
                    allDay: current.allDay,
                    publishAllDay: keyPath == \.publishAllDay
                        ? value : current.publishAllDay,
                    publishTentative: keyPath == \.publishTentative
                        ? value : current.publishTentative,
                    publishFree: keyPath == \.publishFree
                        ? value : current.publishFree
                )
                Task {
                    await store.configureSource(
                        collection,
                        selected: true,
                        visible: collection.visible,
                        role: .writable,
                        calendarPolicy: policy
                    )
                }
            }
        )
    }

    private var kindTitle: String {
        switch collection.kind {
        case .calendar: "Calendar"
        case .taskList: "Tasks"
        }
    }

    private var kindSymbol: String {
        switch collection.kind {
        case .calendar: "calendar"
        case .taskList: "checklist"
        }
    }

    private var kindColor: Color {
        switch collection.kind {
        case .calendar: .blue
        case .taskList: .purple
        }
    }

    private func sourceIdentifier(_ suffix: String) -> String {
        "google.account.\(accountOrdinal).source.\(sourceOrdinal).\(suffix)"
    }
}

private struct GoogleSyncSummaryView: View {
    let sync: GoogleSyncStatus
    let accountOrdinal: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let run = sync.run {
                HStack(spacing: 7) {
                    Image(systemName: stateSymbol(run.state))
                        .foregroundStyle(stateColor(run.state))
                    Text(stateTitle(run.state))
                        .font(.caption.weight(.semibold))
                    Spacer(minLength: 6)
                    if let completedAt = run.completedAt {
                        Text("Completed \(completedAt.formatted(date: .abbreviated, time: .shortened))")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 72), spacing: 6)],
                    alignment: .leading,
                    spacing: 6
                ) {
                    GoogleSyncMetric(title: "Imported", value: run.importedCount)
                    GoogleSyncMetric(title: "Updated", value: run.updatedCount)
                    GoogleSyncMetric(title: "Removed", value: run.deletedCount)
                    GoogleSyncMetric(title: "Conflicts", value: run.conflictCount)
                    GoogleSyncMetric(title: "Rejected", value: run.rejectedCount)
                    GoogleSyncMetric(title: "Publishing", value: sync.pendingOutbound)
                    GoogleSyncMetric(
                        title: "Publish issues",
                        value: outboundIssueCount
                    )
                }

                if run.state == .backoff {
                    Label(
                        "Safe retry after \(run.nextAttemptAt.formatted(date: .abbreviated, time: .shortened))",
                        systemImage: "clock.badge.exclamationmark"
                    )
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("google.account.\(accountOrdinal).sync.backoff")
                }
            } else {
                Label("No import run has been recorded yet.", systemImage: "clock")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if sync.importConflicts > 0 {
                Label(
                    "\(sync.importConflicts) imported item\(sync.importConflicts == 1 ? "" : "s") need conflict review.",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.caption2)
                .foregroundStyle(.orange)
                .accessibilityIdentifier("google.account.\(accountOrdinal).sync.conflicts")
            }

            if sync.pendingOutbound > 0 {
                Label(
                    "\(sync.pendingOutbound) reviewed Google change\(sync.pendingOutbound == 1 ? " is" : "s are") queued for delivery.",
                    systemImage: "arrow.up.circle"
                )
                .font(.caption2)
                .foregroundStyle(.blue)
                .accessibilityIdentifier(
                    "google.account.\(accountOrdinal).sync.outbound-pending"
                )
            }

            if outboundIssueCount > 0 {
                Label(
                    "\(outboundIssueCount) Google publication\(outboundIssueCount == 1 ? " needs" : "s need") review. Provider details remain hidden.",
                    systemImage: "exclamationmark.arrow.triangle.2.circlepath"
                )
                .font(.caption2)
                .foregroundStyle(.orange)
                .accessibilityIdentifier(
                    "google.account.\(accountOrdinal).sync.outbound-issues"
                )
            } else if let nextAttempt = sync.nextOutboundAttemptAt,
                      sync.pendingOutbound > 0 {
                Label(
                    "Next safe Google delivery attempt \(nextAttempt.formatted(date: .abbreviated, time: .shortened)).",
                    systemImage: "clock.arrow.circlepath"
                )
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
        }
        .padding(9)
        .background(.quaternary.opacity(0.32), in: RoundedRectangle(cornerRadius: 9))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("google.account.\(accountOrdinal).sync")
    }

    private var outboundIssueCount: UInt64 {
        let (sum, overflow) = sync.conflictedOutbound.addingReportingOverflow(
            sync.failedOutbound
        )
        return overflow ? .max : sum
    }

    private func stateTitle(_ state: GoogleSyncRunState) -> String {
        switch state {
        case .idle: "Import ready"
        case .running: "Importing"
        case .backoff: "Waiting to retry"
        case .reauthorizationRequired: "Authorization required"
        case .failed: "Import needs attention"
        }
    }

    private func stateSymbol(_ state: GoogleSyncRunState) -> String {
        switch state {
        case .idle: "checkmark.circle"
        case .running: "arrow.triangle.2.circlepath"
        case .backoff: "clock.arrow.circlepath"
        case .reauthorizationRequired: "person.crop.circle.badge.exclamationmark"
        case .failed: "exclamationmark.triangle"
        }
    }

    private func stateColor(_ state: GoogleSyncRunState) -> Color {
        switch state {
        case .idle: .green
        case .running: .blue
        case .backoff: .orange
        case .reauthorizationRequired, .failed: .red
        }
    }
}

private struct GoogleSyncMetric: View {
    let title: String
    let value: UInt64

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(value, format: .number)
                .font(.caption.weight(.semibold))
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 7)
        .padding(.vertical, 5)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 7))
    }
}

private struct GoogleStatusBadge: View {
    let title: String
    let color: Color

    var body: some View {
        Text(title)
            .font(.caption2.weight(.medium))
            .foregroundStyle(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(color.opacity(0.12), in: Capsule())
    }
}
