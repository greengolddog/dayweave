import SwiftUI

struct DeviceSessionSettingsView: View {
    @ObservedObject var durableAuth: DurableAuthSettingsModel

    let baseURLString: String
    let currentSessionRevocationDisabled: Bool
    let onRevokeCurrentSession: (DurableDeviceSessionInventorySnapshot) -> Void

    @State private var pendingRevocation: PendingRevocation?

    private struct PendingRevocation: Identifiable {
        let session: DurableDeviceSessionMetadata
        let inventory: DurableDeviceSessionInventorySnapshot

        var id: UUID { session.id }
        var isCurrent: Bool { inventory.currentSessionID == session.id }
    }

    var body: some View {
        Section("Active devices") {
            HStack(alignment: .firstTextBaseline) {
                Text("Review every device that can access this DayWeave account.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer(minLength: 12)
                Button {
                    refresh()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .disabled(baseURL == nil || !canLoadInventory || durableAuth.isManagingDeviceSessions)
                .accessibilityIdentifier("settings.active-devices.refresh")
            }

            if let inventory = durableAuth.deviceSessionInventory {
                HStack {
                    Text("\(inventory.sessions.count) active device\(inventory.sessions.count == 1 ? "" : "s")")
                        .font(.caption.weight(.semibold))
                    Spacer()
                    Text("Checked \(inventory.fetchedAt.formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                if let message = durableAuth.deviceSessionReadOnlyMessage {
                    Label(message, systemImage: "eye")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("settings.active-devices.read-only")
                }

                if inventory.sessions.isEmpty {
                    Label("No active device sessions were reported.", systemImage: "checkmark.shield")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(inventory.sessionsWithCurrentFirst, id: \.id) { session in
                        deviceRow(session, inventory: inventory)
                    }
                }
            } else if durableAuth.isRefreshingDeviceSessions {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Checking active devices…")
                        .foregroundStyle(.secondary)
                }
                .accessibilityIdentifier("settings.active-devices.loading")
            } else if canLoadInventory {
                Label(
                    "No device list is stored on this Mac. Connect to refresh the server-authoritative list.",
                    systemImage: "wifi"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            } else {
                Label(
                    "Finish DayWeave authentication to review active devices.",
                    systemImage: "lock"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }

            if durableAuth.deviceSessionInventoryIsStale,
               let fetchedAt = durableAuth.deviceSessionInventory?.fetchedAt {
                Label(
                    "Showing the last verified list from \(fetchedAt.formatted(date: .abbreviated, time: .shortened)); it may be stale.",
                    systemImage: "wifi.exclamationmark"
                )
                .font(.caption)
                .foregroundStyle(.orange)
                .accessibilityIdentifier("settings.active-devices.stale")
            }
            if let message = durableAuth.deviceSessionErrorMessage {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("settings.active-devices.error")
            } else if let notice = durableAuth.deviceSessionNotice {
                Label(notice, systemImage: "checkmark.circle.fill")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.active-devices.notice")
            }

            Text("The list is kept only in memory and is cleared when Settings closes, the app locks, or authentication changes. Remote revocation is never queued while offline.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .task(id: refreshTaskIdentity) {
            pendingRevocation = nil
            guard canLoadInventory, let baseURL else {
                durableAuth.clearDeviceSessionInventory()
                return
            }
            _ = await durableAuth.refreshDeviceSessions(baseURL: baseURL)
        }
        .onDisappear {
            pendingRevocation = nil
            durableAuth.clearDeviceSessionInventory()
        }
        .confirmationDialog(
            confirmationTitle,
            isPresented: confirmationIsPresented,
            titleVisibility: .visible
        ) {
            if let pendingRevocation {
                Button(
                    pendingRevocation.isCurrent ? "Revoke & Sign Out" : "Revoke Device",
                    role: .destructive
                ) {
                    confirm(pendingRevocation)
                }
                .disabled(
                    !durableAuth.canMutateDeviceSessionsFromInventory
                        || (pendingRevocation.isCurrent && currentSessionRevocationDisabled)
                )
            }
            Button("Cancel", role: .cancel) {
                pendingRevocation = nil
            }
        } message: {
            Text(confirmationMessage)
        }
    }

    @ViewBuilder
    private func deviceRow(
        _ session: DurableDeviceSessionMetadata,
        inventory: DurableDeviceSessionInventorySnapshot
    ) -> some View {
        let isCurrent = inventory.currentSessionID == session.id
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: session.clientKind == "macos" ? "desktopcomputer" : "smartphone")
                .font(.title3)
                .foregroundStyle(isCurrent ? Color.accentColor : .secondary)
                .frame(width: 24)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(session.deviceLabel)
                        .font(.body.weight(.semibold))
                        .lineLimit(2)
                    if isCurrent {
                        Text("This Mac")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(Color.accentColor)
                            .padding(.horizontal, 7)
                            .padding(.vertical, 2)
                            .background(Color.accentColor.opacity(0.12), in: Capsule())
                            .accessibilityIdentifier("settings.active-devices.current-badge")
                    }
                }
                Text("\(platformName(session.clientKind)) · DayWeave \(session.clientVersion)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Last used \(timestamp(session.lastSeenAt)) · Added \(timestamp(session.createdAt))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Refresh access until \(timestamp(session.refreshIdleExpiresAt)) · Ends \(timestamp(session.absoluteExpiresAt))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 12)

            if durableAuth.revokingDeviceSessionID == session.id {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Revoking \(session.deviceLabel)")
            } else {
                Button(isCurrent ? "Sign Out…" : "Revoke…", role: .destructive) {
                    pendingRevocation = .init(session: session, inventory: inventory)
                }
                .buttonStyle(.bordered)
                .disabled(
                    durableAuth.isManagingDeviceSessions
                        || durableAuth.isBusy
                        || durableAuth.deviceSessionInventoryIsStale
                        || !durableAuth.canMutateDeviceSessionsFromInventory
                        || (isCurrent && currentSessionRevocationDisabled)
                )
                .accessibilityLabel(
                    isCurrent
                        ? "Revoke this Mac and sign out"
                        : "Revoke \(session.deviceLabel)"
                )
                .accessibilityIdentifier(
                    "settings.active-devices.revoke.\(session.id.uuidString.lowercased())"
                )
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(
            "settings.active-devices.session.\(session.id.uuidString.lowercased())"
        )
    }

    private var baseURL: DayWeaveAPIBaseURL? {
        try? DayWeaveAPIBaseURL(baseURLString)
    }

    private var canLoadInventory: Bool {
        switch durableAuth.presentation.phase {
        case .legacy, .active, .refreshPending:
            true
        case .notConfigured, .enrollmentCreationPending, .enrollmentPending,
             .reauthenticationRequired, .incompatible:
            false
        }
    }

    private var refreshTaskIdentity: String {
        "\(baseURLString)|\(durableAuth.presentation.phase.rawValue)"
    }

    private var confirmationIsPresented: Binding<Bool> {
        Binding(
            get: { pendingRevocation != nil },
            set: { if !$0 { pendingRevocation = nil } }
        )
    }

    private var confirmationTitle: String {
        guard let pendingRevocation else { return "Revoke device access?" }
        return pendingRevocation.isCurrent
            ? "Revoke this Mac and sign out?"
            : "Revoke \(pendingRevocation.session.deviceLabel)?"
    }

    private var confirmationMessage: String {
        guard let pendingRevocation else { return "" }
        if pendingRevocation.isCurrent {
            return "DayWeave will revoke this Mac on the server, then remove its local Keychain credentials. Pending protected work must be reconciled first."
        }
        return "This device will lose DayWeave access and must be enrolled again. Nothing stored on this Mac will be removed."
    }

    private func confirm(_ pending: PendingRevocation) {
        pendingRevocation = nil
        guard let baseURL,
              durableAuth.canMutateDeviceSessionsFromInventory else { return }
        if pending.isCurrent {
            guard durableAuth.deviceSessionInventory == pending.inventory,
                  durableAuth.coordinator.isDeviceSessionInventoryCurrent(
                      pending.inventory,
                      boundTo: baseURL
                  ) else {
                refresh()
                return
            }
            onRevokeCurrentSession(pending.inventory)
        } else {
            Task { @MainActor in
                _ = await durableAuth.revokeRemoteDeviceSession(
                    pending.session.id,
                    approvedFrom: pending.inventory,
                    baseURL: baseURL
                )
            }
        }
    }

    private func refresh() {
        guard let baseURL, canLoadInventory else { return }
        Task { @MainActor in
            _ = await durableAuth.refreshDeviceSessions(baseURL: baseURL)
        }
    }

    private func platformName(_ kind: String) -> String {
        kind == "macos" ? "Mac" : "Android"
    }

    private func timestamp(_ date: Date) -> String {
        date.formatted(date: .abbreviated, time: .shortened)
    }
}
