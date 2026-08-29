import AppKit
import SwiftUI

@main
@MainActor
struct DayWeaveMacApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var store: PlannerStore
    @StateObject private var codex: CodexAppServerClient
    @StateObject private var codexConversation: CodexConversationController
    @StateObject private var suggestionSync: SuggestionSyncStore
    @StateObject private var proposalApplications: ProposalApplicationStore
    @StateObject private var canonicalSync: CanonicalSyncStore
    @StateObject private var executionSync: ExecutionSyncStore
    @StateObject private var serviceCoordinator: DayWeaveServiceCoordinator
    @StateObject private var durableAuth: DurableAuthSettingsModel
    @StateObject private var appLock: AppLockController
    @StateObject private var appearance: AppearanceController
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
        let canonicalSync = CanonicalSyncStore(
            planner: store,
            authCoordinator: authCoordinator
        )
        _canonicalSync = StateObject(wrappedValue: canonicalSync)
        let executionSync = ExecutionSyncStore(
            planner: store,
            authCoordinator: authCoordinator
        )
        _executionSync = StateObject(wrappedValue: executionSync)
        _serviceCoordinator = StateObject(wrappedValue: DayWeaveServiceCoordinator(
            proposalApplications: proposalApplications,
            executionSync: executionSync,
            canonicalSync: canonicalSync
        ))
        _appLock = StateObject(wrappedValue: AppLockController.live())
        _appearance = StateObject(wrappedValue: AppearanceController.live())
    }

    var body: some Scene {
        WindowGroup {
            Group {
                if appLock.isContentAvailable {
                    RootView()
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
                .environmentObject(serviceCoordinator)
                .environmentObject(durableAuth)
                .environmentObject(appLock)
                .environmentObject(appearance)
                .preferredColorScheme(appearance.preferredColorScheme)
                .tint(appearance.accentColor)
                .frame(minWidth: 1_080, minHeight: 720)
                .onAppear {
                    updateAppLock(for: scenePhase)
                    if scenePhase == .active {
                        activateServices()
                    }
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
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
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
        }
        .defaultSize(width: 1_420, height: 900)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Quick Add…") {
                    store.isQuickAddPresented = true
                }
                .keyboardShortcut("n", modifiers: [.command, .shift])
                .disabled(!appLock.isContentAvailable || !store.canMutatePlan)

                Button("Recompose Schedule") {
                    store.recomposeSchedule()
                }
                .keyboardShortcut("r", modifiers: [.command, .option])
                .disabled(!appLock.isContentAvailable || !store.canRecomposeSchedule)
            }
        }

        MenuBarExtra(
            "DayWeave",
            systemImage: appLock.isContentAvailable
                ? (executionSync.activeSession == nil && store.activeItem == nil
                    ? "sparkles" : "timer")
                : "lock.fill"
        ) {
            Group {
                if appLock.isContentAvailable {
                    MenuBarView()
                        .environmentObject(store)
                        .environmentObject(executionSync)
                        .environmentObject(durableAuth)
                } else {
                    AppLockMenuBarView()
                }
            }
            .environmentObject(appLock)
            .environmentObject(appearance)
            .preferredColorScheme(appearance.preferredColorScheme)
            .tint(appearance.accentColor)
        }
        .menuBarExtraStyle(.window)

        Settings {
            Group {
                if appLock.isContentAvailable {
                    SettingsView()
                        .environmentObject(store)
                        .environmentObject(codex)
                        .environmentObject(suggestionSync)
                        .environmentObject(proposalApplications)
                        .environmentObject(canonicalSync)
                        .environmentObject(executionSync)
                        .environmentObject(durableAuth)
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
        guard appLock.isContentAvailable else { return }
        codex.startIfNeeded()
        serviceCoordinator.activate()
    }

    private func deactivateServices() {
        serviceCoordinator.deactivate()
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
}
