import AppKit
import SwiftUI

@MainActor
private struct DayWeaveCommands: Commands {
    @Environment(\.openWindow) private var openWindow
    @ObservedObject var store: PlannerStore
    @ObservedObject var canonicalSync: CanonicalSyncStore
    @ObservedObject var appLock: AppLockController
    @ObservedObject var onboarding: DayWeaveOnboardingController

    var body: some Commands {
        CommandGroup(after: .newItem) {
            Button("Quick Capture…") {
                openWindow(id: "quick-capture")
            }
            .keyboardShortcut("n", modifiers: [.command, .shift])
            .disabled(
                !appLock.isContentAvailable
                    || !onboarding.progress.privacyAcknowledged
                    || !store.canMutatePlan
            )

            Button("Compose on This Mac") {
                Task { await canonicalSync.recomposeLocally() }
            }
            .keyboardShortcut("r", modifiers: [.command, .option])
            .disabled(
                !appLock.isContentAvailable
                    || !onboarding.progress.privacyAcknowledged
                    || !store.canMutatePlan
                    || canonicalSync.isSyncing
                    || canonicalSync.isLocallyComposing
                    || !canonicalSync.canRecomposeLocally
            )
            .accessibilityIdentifier("schedule.compose-local.command")
        }
    }
}

@main
@MainActor
struct DayWeaveMacApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.openWindow) private var openWindow
    @NSApplicationDelegateAdaptor(DayWeaveMacAppDelegate.self) private var appDelegate
    @StateObject private var store: PlannerStore
    @StateObject private var codex: CodexAppServerClient
    @StateObject private var codexConversation: CodexConversationController
    @StateObject private var suggestionSync: SuggestionSyncStore
    @StateObject private var proposalApplications: ProposalApplicationStore
    @StateObject private var canonicalSync: CanonicalSyncStore
    @StateObject private var executionSync: ExecutionSyncStore
    @StateObject private var habitSync: HabitSyncStore
    @StateObject private var googleIntegration: GoogleIntegrationStore
    @StateObject private var googleOutbound: GoogleOutboundStore
    @StateObject private var googleSchedulePublication: GoogleSchedulePublicationStore
    @StateObject private var serviceCoordinator: DayWeaveServiceCoordinator
    @StateObject private var durableAuth: DurableAuthSettingsModel
    @StateObject private var appLock: AppLockController
    @StateObject private var appearance: AppearanceController
    @StateObject private var onboarding: DayWeaveOnboardingController
    private let breakNotificationTapRouter = DayWeaveBreakNotificationTapRouter.shared
    init() {
        let store = PlannerStore.live()
        _store = StateObject(wrappedValue: store)
        let codex = CodexAppServerClient()
        _codex = StateObject(wrappedValue: codex)
        let authCoordinator = DurableAuthCoordinator()
        _durableAuth = StateObject(wrappedValue: DurableAuthSettingsModel(
            coordinator: authCoordinator
        ))
        _codexConversation = StateObject(wrappedValue: CodexConversationController(
            client: codex,
            contextProvider: store,
            suggestionRouter: CodexSuggestionInboxRouter(planner: store)
        ))
        let suggestionSync = SuggestionSyncStore(
            authCoordinator: authCoordinator
        )
        _suggestionSync = StateObject(wrappedValue: suggestionSync)
        let proposalApplications = ProposalApplicationStore(
            suggestions: suggestionSync,
            journal: store
        )
        _proposalApplications = StateObject(wrappedValue: proposalApplications)
        let habitSync = HabitSyncStore(
            authCoordinator: authCoordinator,
            protectedPlannerOccurrenceIDs: {
                store.habitRetentionProtectedPlannerOccurrenceIDs
            }
        )
        _habitSync = StateObject(wrappedValue: habitSync)
        let canonicalSync = CanonicalSyncStore(
            planner: store,
            authCoordinator: authCoordinator,
            habitCompositionProvider: habitSync
        )
        _canonicalSync = StateObject(wrappedValue: canonicalSync)
        let executionSync = ExecutionSyncStore(
            planner: store,
            habitCompositionProvider: habitSync,
            authCoordinator: authCoordinator
        )
        executionSync.installDeferredPublicationCoordinator {
            await canonicalSync.syncThroughFreshComposition()
        }
        _executionSync = StateObject(wrappedValue: executionSync)
        let googleIntegration = GoogleIntegrationStore(
            authCoordinator: authCoordinator
        )
        googleIntegration.installImportCompletionVerifier {
            await canonicalSync.syncThroughFreshComposition()
        }
        _googleIntegration = StateObject(wrappedValue: googleIntegration)
        let outboundConfiguration = UserDefaultsSuggestionAPIConfigurationStore()
        let outboundSession = makeDayWeaveEphemeralSession()
        let googleOutbound = GoogleOutboundStore(
            recoveryStore: store,
            transportProvider: {
                guard let value = outboundConfiguration.loadBaseURL(),
                      !value.isEmpty else {
                    throw DayWeaveAPIError.credentialUnavailable
                }
                let baseURL = try DayWeaveAPIBaseURL(value)
                return try DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: outboundSession,
                    durableAuthCoordinator: authCoordinator
                )
            }
        )
        _googleOutbound = StateObject(wrappedValue: googleOutbound)
        let googleSchedulePublication = GoogleSchedulePublicationStore(
            recoveryStore: store,
            transportProvider: {
                guard let value = outboundConfiguration.loadBaseURL(),
                      !value.isEmpty else {
                    throw DayWeaveAPIError.credentialUnavailable
                }
                let baseURL = try DayWeaveAPIBaseURL(value)
                return try DayWeaveAPIClient(
                    baseURL: baseURL,
                    session: outboundSession,
                    durableAuthCoordinator: authCoordinator
                )
            }
        )
        _googleSchedulePublication = StateObject(wrappedValue: googleSchedulePublication)
        _serviceCoordinator = StateObject(wrappedValue: DayWeaveServiceCoordinator(
            proposalApplications: proposalApplications,
            googleOutbound: googleOutbound,
            googleSchedulePublication: googleSchedulePublication,
            executionSync: executionSync,
            canonicalSync: canonicalSync,
            habitSync: habitSync
        ))
        _appLock = StateObject(wrappedValue: AppLockController.live())
        _appearance = StateObject(wrappedValue: AppearanceController.live())
        _onboarding = StateObject(wrappedValue: DayWeaveOnboardingController.live())
    }

    var body: some Scene {
        Window("DayWeave", id: "main") {
            Group {
                if appLock.isContentAvailable {
                    DayWeaveOnboardingHost()
                } else {
                    AppLockedView()
                }
            }
                .environmentObject(store)
                .environmentObject(codex)
                .environmentObject(codexConversation)
                .environmentObject(suggestionSync)
                .environmentObject(proposalApplications)
                .environmentObject(canonicalSync)
                .environmentObject(executionSync)
                .environmentObject(habitSync)
                .environmentObject(googleIntegration)
                .environmentObject(googleOutbound)
                .environmentObject(googleSchedulePublication)
                .environmentObject(serviceCoordinator)
                .environmentObject(durableAuth)
                .environmentObject(appLock)
                .environmentObject(appearance)
                .environmentObject(onboarding)
                .preferredColorScheme(appearance.preferredColorScheme)
                .tint(appearance.accentColor)
                .frame(minWidth: 1_080, minHeight: 720)
                .onAppear {
                    installBreakNotificationWindowActivation()
                    updateAppLock(for: scenePhase)
                    if scenePhase == .active {
                        activateServices()
                    }
                    routePendingBreakNotificationTap()
                }
                .onChange(of: scenePhase) { _, phase in
                    updateAppLock(for: phase)
                    if phase == .active {
                        activateServices()
                    } else {
                        deactivateServices()
                        store.flushPersistence()
                    }
                }
                .onChange(of: appLock.isContentAvailable) { _, isAvailable in
                    if isAvailable, scenePhase == .active {
                        activateServices()
                        routePendingBreakNotificationTap()
                    } else {
                        deactivateServices()
                        store.flushPersistence()
                    }
                }
                .onChange(of: onboarding.progress.privacyAcknowledged) { _, acknowledged in
                    if acknowledged, appLock.isContentAvailable, scenePhase == .active {
                        activateServices()
                    } else {
                        deactivateServices()
                        store.flushPersistence()
                    }
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
                    deactivateServices()
                    store.flushPersistence()
                    codexConversation.shutDown()
                    codex.shutDown()
                }
                .onReceive(breakNotificationTapRouter.$pendingIdentifier) { _ in
                    // @Published emits during willSet. Defer one MainActor turn
                    // so deliverPending observes the newly stored identifier;
                    // this matters when the singleton window is already open
                    // and therefore has no fresh onAppear callback.
                    Task { @MainActor in routePendingBreakNotificationTap() }
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                    for: NSWorkspace.sessionDidResignActiveNotification
                )) { _ in
                    appLock.applicationBecameInactive()
                    deactivateServices()
                    store.flushPersistence()
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                    for: NSWorkspace.willSleepNotification
                )) { _ in
                    appLock.applicationBecameInactive()
                    deactivateServices()
                    store.flushPersistence()
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                    for: NSWorkspace.sessionDidBecomeActiveNotification
                )) { _ in
                    resumeAfterSystemBoundaryIfFrontmost()
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                    for: NSWorkspace.didWakeNotification
                )) { _ in
                    resumeAfterSystemBoundaryIfFrontmost()
                }
        }
        .defaultSize(width: 1_420, height: 900)
        .commands {
            DayWeaveCommands(
                store: store,
                canonicalSync: canonicalSync,
                appLock: appLock,
                onboarding: onboarding
            )
        }

        Window("Quick Capture", id: "quick-capture") {
            Group {
                if appLock.isContentAvailable,
                   onboarding.progress.privacyAcknowledged {
                    QuickCaptureView(
                        profileTimezoneName: store.scheduleProfile.timezoneName
                    )
                        .environmentObject(store)
                } else if appLock.isContentAvailable {
                    DayWeaveOnboardingPrivacyBackdrop(
                        resume: resumeOnboardingInMainWindow
                    )
                } else {
                    AppLockedView()
                }
            }
            .environmentObject(appLock)
            .environmentObject(appearance)
            .preferredColorScheme(appearance.preferredColorScheme)
            .tint(appearance.accentColor)
            .onAppear {
                updateAppLock(for: scenePhase)
                if scenePhase == .active { activateServices() }
            }
            .onChange(of: scenePhase) { _, phase in
                updateAppLock(for: phase)
                if phase == .active {
                    activateServices()
                } else {
                    deactivateServices()
                    store.flushPersistence()
                }
            }
            .onChange(of: appLock.isContentAvailable) { _, isAvailable in
                if isAvailable, scenePhase == .active {
                    activateServices()
                } else {
                    deactivateServices()
                    store.flushPersistence()
                }
            }
            .onReceive(NotificationCenter.default.publisher(
                for: NSApplication.willTerminateNotification
            )) { _ in
                deactivateServices()
                store.flushPersistence()
                codexConversation.shutDown()
                codex.shutDown()
            }
            .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                for: NSWorkspace.sessionDidResignActiveNotification
            )) { _ in
                appLock.applicationBecameInactive()
                deactivateServices()
                store.flushPersistence()
            }
            .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                for: NSWorkspace.willSleepNotification
            )) { _ in
                appLock.applicationBecameInactive()
                deactivateServices()
                store.flushPersistence()
            }
            .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                for: NSWorkspace.sessionDidBecomeActiveNotification
            )) { _ in
                resumeAfterSystemBoundaryIfFrontmost()
            }
            .onReceive(NSWorkspace.shared.notificationCenter.publisher(
                for: NSWorkspace.didWakeNotification
            )) { _ in
                resumeAfterSystemBoundaryIfFrontmost()
            }
            .onReceive(breakNotificationTapRouter.$pendingIdentifier) { _ in
                Task { @MainActor in routePendingBreakNotificationTap() }
            }
        }
        .defaultPosition(.center)
        .windowResizability(.contentSize)

        MenuBarExtra(
            "DayWeave",
            systemImage: appLock.isContentAvailable
                && onboarding.progress.privacyAcknowledged
                ? (executionSync.activeSession == nil && store.activeItem == nil
                    ? "sparkles" : "timer")
                : "lock.fill"
        ) {
            Group {
                if appLock.isContentAvailable,
                   onboarding.progress.privacyAcknowledged {
                    MenuBarView()
                        .environmentObject(store)
                        .environmentObject(canonicalSync)
                        .environmentObject(executionSync)
                        .environmentObject(habitSync)
                        .environmentObject(durableAuth)
                } else if appLock.isContentAvailable {
                    DayWeaveOnboardingPrivacyMenuView(
                        resume: resumeOnboardingInMainWindow
                    )
                } else {
                    AppLockMenuBarView()
                }
            }
            .environmentObject(appLock)
            .environmentObject(appearance)
            .onReceive(breakNotificationTapRouter.$pendingIdentifier) { _ in
                Task { @MainActor in routePendingBreakNotificationTap() }
            }
            .preferredColorScheme(appearance.preferredColorScheme)
            .tint(appearance.accentColor)
        }
        .menuBarExtraStyle(.window)

        Settings {
            Group {
                if appLock.isContentAvailable,
                   onboarding.progress.privacyAcknowledged {
                    SettingsView()
                        .environmentObject(store)
                        .environmentObject(codex)
                        .environmentObject(suggestionSync)
                        .environmentObject(proposalApplications)
                        .environmentObject(canonicalSync)
                        .environmentObject(executionSync)
                        .environmentObject(googleIntegration)
                        .environmentObject(googleOutbound)
                        .environmentObject(googleSchedulePublication)
                        .environmentObject(durableAuth)
                        .environmentObject(onboarding)
                } else if appLock.isContentAvailable {
                    DayWeaveOnboardingPrivacyBackdrop(
                        resume: resumeOnboardingInMainWindow
                    )
                } else {
                    AppLockedView()
                }
            }
            .environmentObject(appLock)
            .environmentObject(appearance)
            .preferredColorScheme(appearance.preferredColorScheme)
            .tint(appearance.accentColor)
            .frame(width: 660, height: 620)
        }
    }

    private func activateServices() {
        guard appLock.isContentAvailable,
              onboarding.progress.privacyAcknowledged else { return }
        googleOutbound.setPrivacyAvailable(true)
        googleSchedulePublication.setPrivacyAvailable(true)
        codex.startIfNeeded()
        googleIntegration.activate()
        serviceCoordinator.activate()
        Task { await executionSync.reconcileBreakNotification() }
        routePendingBreakNotificationTap()
    }

    private func deactivateServices() {
        googleOutbound.setPrivacyAvailable(false)
        googleSchedulePublication.setPrivacyAvailable(false)
        serviceCoordinator.deactivate()
        googleIntegration.suspendForPrivacyBoundary()
        codexConversation.suspendForPrivacyBoundary()
        proposalApplications.suspendForPrivacyBoundary()
    }

    private func updateAppLock(for phase: ScenePhase) {
        if phase == .active {
            appLock.applicationBecameActive()
        } else {
            appLock.applicationBecameInactive()
        }
    }

    private func resumeAfterSystemBoundaryIfFrontmost() {
        guard NSApp.isActive else { return }
        appLock.applicationBecameActive()
        activateServices()
    }

    private func routePendingBreakNotificationTap() {
        guard onboarding.progress.privacyAcknowledged else { return }
        guard breakNotificationTapRouter.deliverPending(
            contentAvailable: appLock.isContentAvailable,
            route: executionSync.routeBreakNotificationTap(identifier:)
        ) == true else { return }
        NSApp.activate(ignoringOtherApps: true)
        openWindow(id: "main")
    }

    private func installBreakNotificationWindowActivation() {
        breakNotificationTapRouter.installMainWindowActivation {
            NSApp.activate(ignoringOtherApps: true)
            openWindow(id: "main")
        }
    }

    private func resumeOnboardingInMainWindow() {
        onboarding.present()
        NSApp.activate(ignoringOtherApps: true)
        openWindow(id: "main")
    }
}

private struct DayWeaveOnboardingPrivacyMenuView: View {
    let resume: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Privacy review required", systemImage: "lock.shield.fill")
                .font(.headline)
            Text("Workspace content and network services are paused.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button("Resume guided setup", action: resume)
                .buttonStyle(.borderedProminent)
        }
        .padding(14)
        .frame(width: 270)
        .accessibilityIdentifier("onboarding.privacy-menu")
    }
}
