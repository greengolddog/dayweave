import SwiftUI

@main
@MainActor
struct DayWeaveMacApp: App {
    @StateObject private var store = PlannerStore.preview()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(store)
                .frame(minWidth: 1_080, minHeight: 720)
        }
        .defaultSize(width: 1_420, height: 900)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Quick Add…") {
                    store.isQuickAddPresented = true
                }
                .keyboardShortcut("n", modifiers: [.command, .shift])

                Button("Recompose Schedule") {
                    store.recomposeSchedule()
                }
                .keyboardShortcut("r", modifiers: [.command, .option])
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
                .frame(width: 620, height: 480)
        }
    }
}

