import AppKit
import SwiftUI

enum DayWeaveOnboardingEvidence {
    struct PrerequisiteFailure: Equatable, Sendable {
        let step: DayWeaveOnboardingStep
        let check: DayWeaveOnboardingCheck
    }

    static func apiConnection(
        durableAuthIsBusy: Bool,
        suggestionIsRefreshing: Bool,
        suggestionStatus: SuggestionSyncStatus,
        currentConfigurationIdentifier: String?,
        verifiedConfigurationIdentifier: String?
    ) -> DayWeaveOnboardingCheck {
        if durableAuthIsBusy || suggestionIsRefreshing {
            return .working("Finishing the exact DayWeave enrollment request…")
        }
        if suggestionStatus.isFailure {
            return .blocked(suggestionStatus.message)
        }
        guard let currentConfigurationIdentifier else {
            return .pending(
                "Open Settings, enter the private API address, and complete device enrollment."
            )
        }
        if verifiedConfigurationIdentifier == currentConfigurationIdentifier {
            return .ready("This Mac completed an authenticated DayWeave API request.")
        }
        return .pending(
            "The credential is stored but has not completed an authenticated API check."
        )
    }

    static func googleAccountIDsRequiringInitialImport(
        activeAccountIDs: Set<UUID>,
        configuredCollectionAccountIDs: [UUID]
    ) -> Set<UUID> {
        activeAccountIDs.intersection(configuredCollectionAccountIDs)
    }

    static func googleRefreshIsCurrent(
        refreshGeneration: UInt64,
        claimedRefreshGeneration: UInt64,
        completedRefreshGeneration: UInt64,
        requestedAt: Date?,
        completedAt: Date?
    ) -> Bool {
        refreshGeneration > 0
            && claimedRefreshGeneration == refreshGeneration
            && completedRefreshGeneration == refreshGeneration
            && requestedAt == nil
            && completedAt != nil
    }

    static func googleCollectionImportIsCurrent(
        configuredAt: Date?,
        lastImportAt: Date?,
        completedAt: Date?
    ) -> Bool {
        guard let configuredAt, let lastImportAt, let completedAt else { return false }
        return lastImportAt >= configuredAt && lastImportAt <= completedAt
    }

    static func firstPlanPrerequisiteFailure(
        apiConnection: DayWeaveOnboardingCheck,
        googleResources: DayWeaveOnboardingCheck,
        scheduleProfile: DayWeaveOnboardingCheck,
        notifications: DayWeaveOnboardingCheck,
        firstItem: DayWeaveOnboardingCheck
    ) -> PrerequisiteFailure? {
        let checks: [(DayWeaveOnboardingStep, DayWeaveOnboardingCheck)] = [
            (.apiConnection, apiConnection),
            (.googleResources, googleResources),
            (.scheduleProfile, scheduleProfile),
            (.notifications, notifications),
            (.firstItem, firstItem),
        ]
        return checks.first(where: { !$0.1.isReady }).map {
            PrerequisiteFailure(step: $0.0, check: $0.1)
        }
    }
}

/// Keeps onboarding below the app-lock boundary while deriving every mutable
/// milestone from its authoritative store. The onboarding record itself is
/// navigation-only and can never promote an item, account, or schedule.
@MainActor
struct DayWeaveOnboardingHost: View {
    @Environment(\.openSettings) private var openSettings
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var googleIntegration: GoogleIntegrationStore
    @EnvironmentObject private var durableAuth: DurableAuthSettingsModel
    @EnvironmentObject private var onboarding: DayWeaveOnboardingController

    @State private var firstItemEditor: FirstItemEditorRoute?
    @State private var compositionTask: Task<Void, Never>?
    @State private var compositionFailure: String?

    var body: some View {
        Group {
            if onboarding.progress.privacyAcknowledged {
                RootView()
            } else {
                DayWeaveOnboardingPrivacyBackdrop(
                    resume: onboarding.present
                )
            }
        }
            .sheet(isPresented: onboardingPresentation) {
                DayWeaveOnboardingView(
                    controller: onboarding,
                    readiness: readiness,
                    actions: actions
                )
                .sheet(item: $firstItemEditor) { route in
                    CanonicalItemEditorView(
                        mode: .createPrepared(itemID: route.id, draft: route.draft),
                        profileTimezoneName: store.scheduleProfile.timezoneName,
                        onSave: {
                            dayWeavePostAccessibilityAnnouncement(
                                "Your planned item was encrypted locally and is ready for the first plan."
                            )
                        }
                    )
                    .environmentObject(store)
                }
            }
            .onDisappear {
                compositionTask?.cancel()
                compositionTask = nil
                firstItemEditor = nil
            }
    }

    private var onboardingPresentation: Binding<Bool> {
        Binding(
            get: { onboarding.isPresented },
            set: { presented in
                if presented {
                    onboarding.present()
                } else {
                    onboarding.dismiss()
                }
            }
        )
    }

    private var actions: DayWeaveOnboardingActions {
        DayWeaveOnboardingActions(
            openAPIConnection: handleAPIConnection,
            openGoogleResources: { openSettings() },
            openScheduleProfile: { openSettings() },
            configureNotifications: openNotificationSettings,
            captureFirstItem: reviewOrCreateFirstItem,
            composeFirstPlan: reviewOrComposeFirstPlan,
            dismiss: onboarding.dismiss,
            didComplete: {
                onboarding.dismiss()
                store.destination = .today
                dayWeavePostAccessibilityAnnouncement(
                    "DayWeave setup is complete. Your first plan is ready."
                )
            }
        )
    }

    private var readiness: DayWeaveOnboardingReadiness {
        DayWeaveOnboardingReadiness(
            apiConnection: apiConnectionCheck,
            googleResources: googleResourcesCheck,
            scheduleProfile: scheduleProfileCheck,
            notifications: notificationCheck,
            firstItem: firstItemCheck,
            firstPlan: firstPlanCheck
        )
    }

    private var apiConnectionCheck: DayWeaveOnboardingCheck {
        DayWeaveOnboardingEvidence.apiConnection(
            durableAuthIsBusy: durableAuth.isBusy,
            suggestionIsRefreshing: suggestionSync.isRefreshing,
            suggestionStatus: suggestionSync.status,
            currentConfigurationIdentifier:
                suggestionSync.currentApplicationConfigurationIdentifier,
            verifiedConfigurationIdentifier:
                suggestionSync.verifiedApplicationConfigurationIdentifier
        )
    }

    private var googleResourcesCheck: DayWeaveOnboardingCheck {
        if googleIntegration.isBusy || googleIntegration.credentialTransitionInProgress {
            return .working(googleIntegration.status.message)
        }
        if googleIntegration.hasPendingRecovery {
            return .blocked(
                "Finish the saved Google authorization, disconnect, or import recovery before continuing."
            )
        }
        let activeAccounts = googleIntegration.accounts.filter {
            $0.status == .active && $0.syncEnabled
        }
        guard !activeAccounts.isEmpty else {
            return .pending("Connect one Google account in Settings.")
        }
        let activeIDs = Set(activeAccounts.map(\.id))
        let configured = googleIntegration.collectionsByAccount
            .filter { activeIDs.contains($0.key) }
            .flatMap(\.value)
            .filter { $0.configuredAt != nil && $0.selected && !$0.providerDeleted }
        guard configured.contains(where: { $0.kind == .calendar }) else {
            return .pending("Discover, select, and save at least one Google Calendar.")
        }
        guard configured.contains(where: { $0.kind == .taskList }) else {
            return .pending("Discover, select, and save at least one Google Tasks list.")
        }
        let importAccountIDs = DayWeaveOnboardingEvidence
            .googleAccountIDsRequiringInitialImport(
                activeAccountIDs: activeIDs,
                configuredCollectionAccountIDs: configured.map(\.accountID)
            )
        let selectedAccounts = activeAccounts.filter { importAccountIDs.contains($0.id) }
        for account in selectedAccounts {
            guard let run = googleIntegration.syncStatusByAccount[account.id]?.run else {
                return .pending(
                    "Refresh Google imports once so the first plan includes current calendar constraints."
                )
            }
            if googleIntegration.pendingRefreshAccountIDs.contains(account.id)
                || run.state == .running {
                return .working("Importing the selected Google resources…")
            }
            switch run.state {
            case .backoff, .reauthorizationRequired, .failed:
                return .blocked(
                    "The initial Google import needs attention before composition."
                )
            case .idle:
                guard DayWeaveOnboardingEvidence.googleRefreshIsCurrent(
                    refreshGeneration: run.refreshGeneration,
                    claimedRefreshGeneration: run.claimedRefreshGeneration,
                    completedRefreshGeneration: run.completedRefreshGeneration,
                    requestedAt: run.requestedAt,
                    completedAt: run.completedAt
                ) else {
                    return .pending(
                        "Complete the latest Google import so every selected source is current."
                    )
                }
                let selectedTaskLists = configured.filter {
                    $0.accountID == account.id && $0.kind == .taskList
                }
                guard selectedTaskLists.allSatisfy({ collection in
                    DayWeaveOnboardingEvidence.googleCollectionImportIsCurrent(
                        configuredAt: collection.configuredAt,
                        lastImportAt: collection.lastImportAt,
                        completedAt: run.completedAt
                    )
                }) else {
                    return .pending(
                        "Complete the latest Google Tasks import before composing the first plan."
                    )
                }
            case .running:
                return .working("Importing the selected Google resources…")
            }
        }
        let configuredCalendars = configured.filter { $0.kind == .calendar }
        guard configuredCalendars.allSatisfy({ calendar in
            calendar.planningProjectionState == .complete
                && calendar.planningCollectionRevision == calendar.revision
        }) else {
            return .working(
                "Waiting for the selected calendars to reach the planning projection."
            )
        }
        guard case .connected = googleIntegration.status else {
            if googleIntegration.status.isFailure {
                return .blocked(googleIntegration.status.message)
            }
            return .working("Checking the selected Google resources…")
        }
        return .ready("Selected Calendar and Tasks sources are connected and saved.")
    }

    private var scheduleProfileCheck: DayWeaveOnboardingCheck {
        guard store.hasEncryptedPersistence, store.canPersistPlan else {
            return .blocked(
                store.persistenceError?.localizedDescription
                    ?? "Encrypted planner storage must be healthy before saving your profile."
            )
        }
        guard store.scheduleProfile.hasValidShape else {
            return .blocked("Review the schedule profile and repair its time boundaries.")
        }
        let profile = store.scheduleProfile
        let activeDays = profile.availability.count { !$0.windows.isEmpty }
        return .ready(
            "\(profile.timezoneName); sleep \(clock(profile.sleep.start))–\(clock(profile.sleep.end)); availability on \(activeDays) days; \(profile.protectedFreeMinutes) protected minutes before sleep. Continuing confirms these values."
        )
    }

    private var notificationCheck: DayWeaveOnboardingCheck {
        switch executionSync.breakNotificationAuthorizationState {
        case .authorized:
            .ready("Timed-break reminders are allowed by macOS. Continuing keeps this choice.")
        case .denied:
            .ready("System reminders are off; continuing keeps in-app break resolution only.")
        case .notDetermined:
            .ready(
                "Permission is deferred. Continuing keeps it deferred until you explicitly create a future timed break."
            )
        }
    }

    private var firstItemCheck: DayWeaveOnboardingCheck {
        if let pending = firstPlanningMutation {
            return .ready(
                pending.hasBeenSubmitted
                    ? "A planned item is durably queued and awaiting exact sync reconciliation."
                    : "A planned item is encrypted locally and ready to sync."
            )
        }
        if firstPlanningCanonicalItem != nil {
            return .ready("The reviewed canonical item is ready for composition.")
        }
        if store.onboardingFirstItemAnchor != nil {
            return .blocked(
                "The encrypted first-item checkpoint no longer matches its exact queued or canonical item. Recover canonical sync before continuing."
            )
        }
        return .pending(
            "Create a Planned leaf item with a duration, or a fully timed event."
        )
    }

    private var firstPlanCheck: DayWeaveOnboardingCheck {
        if store.pendingSchedulePublication == nil,
              store.canonicalPreviewFreshnessIssue == nil,
              store.hasExactOnboardingFirstPlanProof,
              let anchor = store.onboardingFirstItemAnchor,
              let anchoredRevision = anchor.canonicalRevision,
              let firstItem = firstPlanningCanonicalItem,
              firstItem.revision == anchoredRevision,
              let provenance = store.schedulePreviewProvenance,
              let proof = store.currentPublishedScheduleProofAuthority,
              proof.matches(provenance),
              proof.matchesPublishedPlan(store.blocks),
              proof.configurationIdentifier
                == suggestionSync.currentApplicationConfigurationIdentifier {
            return .ready(
                "Published schedule revision \(proof.revisionNumber) includes the reviewed item at revision \(anchoredRevision)."
            )
        }
        if let failure = firstPlanPrerequisiteFailure {
            return .init(
                state: failure.check.state,
                detail: "\(failure.step.title): \(failure.check.detail)"
            )
        }
        if canonicalSync.isSyncing {
            return .working(canonicalSync.status.message)
        }
        if let compositionFailure {
            return .blocked(compositionFailure)
        }
        return .pending(
            "Sync the reviewed item and publish a schedule containing its exact revision."
        )
    }

    private var firstPlanPrerequisiteFailure:
        DayWeaveOnboardingEvidence.PrerequisiteFailure? {
        DayWeaveOnboardingEvidence.firstPlanPrerequisiteFailure(
            apiConnection: apiConnectionCheck,
            googleResources: googleResourcesCheck,
            scheduleProfile: scheduleProfileCheck,
            notifications: notificationCheck,
            firstItem: firstItemCheck
        )
    }

    private var firstPlanningMutation: DayWeavePendingCanonicalAuthoringMutation? {
        guard let anchor = store.onboardingFirstItemAnchor,
              anchor.canonicalRevision == nil else { return nil }
        return anchoredAuthoringMutation.flatMap { mutation in
            guard mutation.operation == .create,
                  mutation.disposition == .pending,
                  let draft = mutation.draft,
                  draft.createsPlanningDemand(
                      itemID: mutation.itemID,
                      hasActiveChildren: store.pendingCanonicalAuthoringMutations
                          .containsPendingCanonicalChild(of: mutation.itemID)
                  ) else {
                return nil
            }
            return mutation
        }
    }

    private var anchoredAuthoringMutation: DayWeavePendingCanonicalAuthoringMutation? {
        guard let itemID = store.onboardingFirstItemAnchor?.itemID else { return nil }
        return store.canonicalAuthoringMutation(itemID: itemID)
    }

    private var anchoredCanonicalItem: DayWeaveCanonicalItem? {
        guard let itemID = store.onboardingFirstItemAnchor?.itemID else { return nil }
        return store.canonicalItems.first { $0.id == itemID }
    }

    private var firstPlanningCanonicalItem: DayWeaveCanonicalItem? {
        guard let anchor = store.onboardingFirstItemAnchor,
              let revision = anchor.canonicalRevision,
              let item = anchoredCanonicalItem,
              item.revision == revision,
              canonicalItemCreatesPlanningDemand(item) else { return nil }
        return item
    }

    private func canonicalItemCreatesPlanningDemand(_ item: DayWeaveCanonicalItem) -> Bool {
        item.createsPlanningDemand(
            canonicalItems: store.canonicalItems,
            hasPendingChildren: store.pendingCanonicalAuthoringMutations
                .containsPendingCanonicalChild(of: item.id)
        )
    }

    private func reviewOrCreateFirstItem() {
        if let mutation = anchoredAuthoringMutation {
            store.selectCanonicalItem(mutation.itemID)
            store.destination = .inbox
            onboarding.dismiss()
            return
        }
        if let item = anchoredCanonicalItem {
            store.selectCanonicalItem(item.id)
            store.destination = .inbox
            onboarding.dismiss()
            return
        }
        guard store.onboardingFirstItemAnchor == nil else { return }
        compositionFailure = nil
        let itemID = UUID()
        firstItemEditor = FirstItemEditorRoute(
            id: itemID,
            draft: DayWeaveCanonicalItemDraft(
                kind: .task,
                status: .planned,
                title: "",
                timezoneName: store.scheduleProfile.timezoneName,
                durationSeconds: 30 * 60
            )
        )
    }

    private func composeFirstPlan() {
        guard compositionTask == nil,
              firstPlanPrerequisiteFailure == nil else {
            return
        }
        compositionFailure = nil
        compositionTask = Task { @MainActor in
            defer { compositionTask = nil }
            let succeeded = await canonicalSync.syncThroughFreshComposition()
            guard !Task.isCancelled, onboarding.isPresented else { return }
            if succeeded {
                dayWeavePostAccessibilityAnnouncement(
                    "The first deterministic plan was published and validated."
                )
            } else {
                compositionFailure = canonicalSync.status.message
                dayWeavePostAccessibilityAnnouncement(
                    "The first plan is not ready. \(canonicalSync.status.message)",
                    priority: .high
                )
            }
        }
    }

    private func reviewOrComposeFirstPlan() {
        if firstPlanCheck.isReady {
            store.destination = .today
            onboarding.dismiss()
            return
        }
        composeFirstPlan()
    }

    private func openNotificationSettings() {
        guard let url = URL(
            string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
        ) else { return }
        NSWorkspace.shared.open(url)
    }

    private func clock(_ value: ScheduleLocalTime) -> String {
        String(format: "%02d:%02d", value.hour, value.minute)
    }

    private func handleAPIConnection() {
        guard suggestionSync.currentApplicationConfigurationIdentifier != nil else {
            openSettings()
            return
        }
        switch suggestionSync.status {
        case .ready:
            Task { await suggestionSync.refresh() }
        case .configurationRequired, .failed, .online:
            openSettings()
        case .refreshing:
            break
        }
    }
}

private struct FirstItemEditorRoute: Identifiable {
    let id: UUID
    let draft: DayWeaveCanonicalItemDraft
}

struct DayWeaveOnboardingPrivacyBackdrop: View {
    let resume: () -> Void

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "lock.shield.fill")
                .font(.system(size: 44, weight: .semibold))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text("Privacy review required")
                .font(.title2.weight(.semibold))
            Text("DayWeave keeps workspace content hidden and network services paused until you acknowledge the privacy and approval boundaries.")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 460)
            Button("Resume guided setup", action: resume)
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .accessibilityIdentifier("onboarding.privacy.resume")
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .windowBackgroundColor))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("onboarding.privacy-backdrop")
    }
}
