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
    @StateObject private var canonicalSync: CanonicalSyncStore
    @StateObject private var executionSync: ExecutionSyncStore
    @State private var activationTask: Task<Void, Never>?
    @State private var servicesAreActive = false

    init() {
        let store = PlannerStore.live()
        _store = StateObject(wrappedValue: store)
        let codex = CodexAppServerClient()
        _codex = StateObject(wrappedValue: codex)
        _codexConversation = StateObject(wrappedValue: CodexConversationController(
            client: codex,
            contextProvider: store,
            suggestionRouter: CodexSuggestionInboxRouter(planner: store)
        ))
        _suggestionSync = StateObject(wrappedValue: SuggestionSyncStore())
        _canonicalSync = StateObject(wrappedValue: CanonicalSyncStore(planner: store))
        _executionSync = StateObject(wrappedValue: ExecutionSyncStore(planner: store))
        codex.startIfNeeded()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(store)
                .environmentObject(codex)
                .environmentObject(codexConversation)
                .environmentObject(suggestionSync)
                .environmentObject(canonicalSync)
                .environmentObject(executionSync)
                .frame(minWidth: 1_080, minHeight: 720)
                .onAppear {
                    if scenePhase == .active {
                        activateServices()
                    }
                }
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active {
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
        }
        .defaultSize(width: 1_420, height: 900)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Quick Add…") {
                    store.isQuickAddPresented = true
                }
                .keyboardShortcut("n", modifiers: [.command, .shift])
                .disabled(!store.canMutatePlan)

                Button("Recompose Schedule") {
                    store.recomposeSchedule()
                }
                .keyboardShortcut("r", modifiers: [.command, .option])
                .disabled(!store.canRecomposeSchedule)
            }
        }

        MenuBarExtra(
            "DayWeave",
            systemImage: executionSync.activeSession == nil && store.activeItem == nil
                ? "sparkles" : "timer"
        ) {
            MenuBarView()
                .environmentObject(store)
                .environmentObject(executionSync)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environmentObject(store)
                .environmentObject(codex)
                .environmentObject(suggestionSync)
                .environmentObject(canonicalSync)
                .environmentObject(executionSync)
                .frame(width: 660, height: 620)
        }
    }

    private func activateServices() {
        guard !servicesAreActive else { return }
        servicesAreActive = true
        activationTask = Task { @MainActor in
            let executionOutcome = await executionSync.refresh()
            guard !Task.isCancelled, servicesAreActive else { return }
            if executionOutcome == .success, canonicalSync.isConfigured {
                await canonicalSync.sync()
            }
            guard !Task.isCancelled, servicesAreActive else { return }
            executionSync.startForegroundPolling()
            activationTask = nil
        }
    }

    private func deactivateServices() {
        servicesAreActive = false
        activationTask?.cancel()
        activationTask = nil
        executionSync.stopForegroundPolling()
    }
}
