import AppKit
import SwiftUI

@main
@MainActor
struct DayWeaveMacApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var store: PlannerStore
    @StateObject private var codex: CodexAppServerClient
    @StateObject private var suggestionSync: SuggestionSyncStore

    init() {
        _store = StateObject(wrappedValue: PlannerStore.live())
        let codex = CodexAppServerClient()
        _codex = StateObject(wrappedValue: codex)
        _suggestionSync = StateObject(wrappedValue: SuggestionSyncStore())
        codex.startIfNeeded()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(store)
                .environmentObject(codex)
                .environmentObject(suggestionSync)
                .frame(minWidth: 1_080, minHeight: 720)
                .onChange(of: scenePhase) { _, phase in
                    if phase != .active {
                        store.flushPersistence()
                    }
                }
                .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
                    store.flushPersistence()
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
                .disabled(!store.canMutatePlan)
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
                .frame(width: 660, height: 620)
        }
    }
}
