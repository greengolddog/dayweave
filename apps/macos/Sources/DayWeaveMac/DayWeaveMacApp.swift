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
                .frame(minWidth: 1_080, minHeight: 720)
                .onChange(of: scenePhase) { _, phase in
                    if phase != .active {
                        store.flushPersistence()
                    }
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
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

        MenuBarExtra("DayWeave", systemImage: store.activeItem == nil ? "sparkles" : "timer") {
            MenuBarView()
                .environmentObject(store)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environmentObject(store)
                .environmentObject(codex)
                .environmentObject(suggestionSync)
                .environmentObject(canonicalSync)
                .frame(width: 660, height: 620)
        }
    }
}
