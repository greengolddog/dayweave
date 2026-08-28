import SwiftUI

struct RootView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @State private var columnVisibility: NavigationSplitViewVisibility = .all

    var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 260)
        } content: {
            destinationContent
                .navigationSplitViewColumnWidth(min: 520, ideal: 760)
        } detail: {
            InspectorView()
                .navigationSplitViewColumnWidth(min: 300, ideal: 360, max: 430)
        }
        .sheet(isPresented: $store.isQuickAddPresented) {
            QuickAddView()
                .environmentObject(store)
        }
        .onAppear {
            codex.startIfNeeded()
        }
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    store.recomposeSchedule()
                } label: {
                    Label("Recompose", systemImage: "wand.and.stars")
                }
                .help("Recompose around current constraints")

                Button {
                    store.isQuickAddPresented = true
                } label: {
                    Label("Quick Add", systemImage: "plus")
                }
                .help("Quick Add (⇧⌘N)")
            }
        }
    }

    @ViewBuilder
    private var destinationContent: some View {
        switch store.destination ?? .today {
        case .today:
            TodayView()
        case .inbox:
            SuggestionsInboxView()
        default:
            PlaceholderDestinationView(destination: store.destination ?? .today)
        }
    }
}

private struct SidebarView: View {
    @EnvironmentObject private var store: PlannerStore

    var body: some View {
        List(selection: $store.destination) {
            Section {
                ForEach(SidebarDestination.allCases.prefix(3)) { destination in
                    Label(destination.title, systemImage: destination.symbol)
                        .tag(destination)
                }
            }

            Section("Plan") {
                ForEach(Array(SidebarDestination.allCases.dropFirst(3))) { destination in
                    Label(destination.title, systemImage: destination.symbol)
                        .tag(destination)
                }
            }

            Section("Status") {
                HStack(spacing: 8) {
                    Circle()
                        .fill(.green)
                        .frame(width: 8, height: 8)
                    Text("Synced just now")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Label("Google Calendar", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.secondary)
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) {
            if let active = store.activeItem {
                ActiveMiniPlayer(block: active)
                    .padding(10)
            }
        }
        .navigationTitle("DayWeave")
    }
}

private struct ActiveMiniPlayer: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("NOW")
                .font(.caption2.weight(.bold))
                .foregroundStyle(.secondary)
            Text(block.title)
                .font(.subheadline.weight(.semibold))
                .lineLimit(1)
            HStack {
                Label(block.timeRange, systemImage: "timer")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    store.pauseActive()
                } label: {
                    Image(systemName: "pause.fill")
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(12)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
    }
}

private struct TodayView: View {
    @EnvironmentObject private var store: PlannerStore

    var body: some View {
        VStack(spacing: 0) {
            TodayHeader()
            Divider()
            ScrollView {
                LazyVStack(spacing: 10) {
                    ForEach(store.visibleBlocks) { block in
                        ScheduleBlockView(block: block)
                            .onTapGesture { store.select(block) }
                    }
                }
                .padding(20)
            }
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .navigationTitle("Today")
    }
}

private struct TodayHeader: View {
    @EnvironmentObject private var store: PlannerStore

    var body: some View {
        HStack(alignment: .top, spacing: 24) {
            VStack(alignment: .leading, spacing: 5) {
                Text(Date.now.formatted(.dateTime.weekday(.wide).month(.wide).day()))
                    .font(.title2.weight(.semibold))
                Text(store.lastScheduleMessage)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            MetricChip(value: "\(store.completedCount)/\(store.blocks.count)", label: "done", symbol: "checkmark")
            MetricChip(value: "\(store.protectedFreeMinutes)m", label: "protected", symbol: "shield")
            MetricChip(value: "82", label: "day score", symbol: "sparkles")
        }
        .padding(20)
    }
}

private struct MetricChip: View {
    let value: String
    let label: String
    let symbol: String

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: symbol)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 0) {
                Text(value).font(.subheadline.weight(.semibold))
                Text(label).font(.caption2).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct ScheduleBlockView: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock

    private var isSelected: Bool { store.selectedBlockID == block.id }

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            VStack(alignment: .trailing, spacing: 2) {
                Text(block.start.formatted(date: .omitted, time: .shortened))
                    .font(.system(.caption, design: .monospaced).weight(.medium))
                Text("\(block.durationMinutes)m")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .frame(width: 58, alignment: .trailing)

            RoundedRectangle(cornerRadius: 3)
                .fill(block.kind.color)
                .frame(width: 5)

            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Image(systemName: block.kind.symbol)
                        .foregroundStyle(block.kind.color)
                    Text(block.title)
                        .font(.headline)
                        .strikethrough(block.status == .completed)
                    if block.isHardConstraint {
                        Image(systemName: "lock.fill")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .help("Hard constraint")
                    }
                    Spacer()
                    Text(block.status.title)
                        .font(.caption.weight(.medium))
                        .foregroundStyle(block.status == .active ? .green : .secondary)
                }

                HStack(spacing: 12) {
                    Text(block.project ?? block.kind.title)
                    Label(block.energy.title, systemImage: "bolt")
                    if block.isFlexible {
                        Label("Flexible", systemImage: "arrow.left.and.right")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)

                if block.status == .active || block.status == .paused {
                    HStack {
                        Button(block.status == .active ? "Pause" : "Resume") {
                            block.status == .active ? store.pauseActive() : store.start(block.id)
                        }
                        .buttonStyle(.borderedProminent)
                        Button("Complete") { store.complete(block.id) }
                        Button("Later") { store.doLater(block.id) }
                    }
                    .controlSize(.small)
                }
            }
            .opacity(block.status == .completed ? 0.55 : 1)
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 14)
                .fill(isSelected ? Color.accentColor.opacity(0.10) : Color(nsColor: .controlBackgroundColor))
        )
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(isSelected ? Color.accentColor.opacity(0.55) : .clear, lineWidth: 1)
        }
        .contentShape(RoundedRectangle(cornerRadius: 14))
        .contextMenu {
            Button("Start") { store.start(block.id) }
            Button("Mark Complete") { store.complete(block.id) }
            Divider()
            Button("Do Later") { store.doLater(block.id) }
            Button("Skip") { store.skip(block.id) }
        }
    }
}

private struct InspectorView: View {
    @EnvironmentObject private var store: PlannerStore
    @State private var tab = 0

    var body: some View {
        VStack(spacing: 0) {
            Picker("Inspector", selection: $tab) {
                Text("Details").tag(0)
                Text("Assistant").tag(1)
            }
            .pickerStyle(.segmented)
            .padding(16)

            Divider()

            if tab == 0 {
                if let block = store.selectedBlock {
                    BlockInspector(block: block)
                } else {
                    ContentUnavailableView("Select an item", systemImage: "sidebar.right")
                }
            } else {
                AssistantView()
            }
        }
        .navigationTitle(tab == 0 ? "Inspector" : "Assistant")
    }
}

private struct BlockInspector: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                HStack(spacing: 12) {
                    Image(systemName: block.kind.symbol)
                        .font(.title2)
                        .foregroundStyle(block.kind.color)
                        .frame(width: 42, height: 42)
                        .background(block.kind.color.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
                    VStack(alignment: .leading) {
                        Text(block.title).font(.title3.weight(.semibold))
                        Text(block.kind.title).foregroundStyle(.secondary)
                    }
                }

                InspectorSection(title: "Schedule") {
                    LabeledContent("Time", value: block.timeRange)
                    LabeledContent("Duration", value: "\(block.durationMinutes) minutes")
                    LabeledContent("Energy", value: block.energy.title)
                    LabeledContent("Constraint", value: block.isHardConstraint ? "Hard" : "Soft")
                }

                InspectorSection(title: "Why here?") {
                    Text(reason(for: block))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                if !block.notes.isEmpty {
                    InspectorSection(title: "Notes") {
                        Text(block.notes).font(.subheadline)
                    }
                }

                HStack {
                    Button("Start") { store.start(block.id) }
                        .buttonStyle(.borderedProminent)
                    Button("Complete") { store.complete(block.id) }
                    Menu("More") {
                        Button("Do Later") { store.doLater(block.id) }
                        Button("Skip") { store.skip(block.id) }
                    }
                }
            }
            .padding(18)
        }
    }

    private func reason(for block: ScheduleBlock) -> String {
        if block.isHardConstraint { return "This block is fixed because it protects a hard commitment." }
        if block.energy == .deep { return "Placed in a high-focus window with transition space before the next hard commitment." }
        return "Placed in the earliest opening that matches its energy and context preferences."
    }
}

private struct InspectorSection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title.uppercased())
                .font(.caption2.weight(.bold))
                .foregroundStyle(.secondary)
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct AssistantView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @State private var draft = ""

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 7) {
                Circle()
                    .fill(codex.state.isConnected ? Color.green : Color.orange)
                    .frame(width: 7, height: 7)
                Text(codex.state.title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                if !codex.state.isConnected {
                    Button("Connect") { codex.signInWithBrowser() }
                        .buttonStyle(.link)
                        .font(.caption)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            Divider()

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(store.assistantMessages) { message in
                            AssistantBubble(message: message)
                                .id(message.id)
                        }
                    }
                    .padding(16)
                }
                .onChange(of: store.assistantMessages.count) {
                    if let last = store.assistantMessages.last {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }

            Divider()
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Ask about your day…", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...5)
                    .onSubmit(send)
                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .buttonStyle(.plain)
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(12)
        }
    }

    private func send() {
        store.sendAssistantMessage(draft)
        draft = ""
    }
}

private struct AssistantBubble: View {
    let message: AssistantMessage

    var body: some View {
        Text(message.text)
            .font(.subheadline)
            .padding(11)
            .background(
                message.role == .assistant ? Color(nsColor: .controlBackgroundColor) : Color.accentColor.opacity(0.16),
                in: RoundedRectangle(cornerRadius: 12)
            )
            .frame(maxWidth: .infinity, alignment: message.role == .assistant ? .leading : .trailing)
    }
}

private struct SuggestionsInboxView: View {
    @EnvironmentObject private var store: PlannerStore

    var body: some View {
        List {
            Section("Suggestions") {
                ForEach(store.suggestions.filter { $0.state == .pending }) { suggestion in
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            Image(systemName: "sparkles").foregroundStyle(.purple)
                            Text(suggestion.title).font(.headline)
                            Spacer()
                            Text(suggestion.source).font(.caption).foregroundStyle(.secondary)
                        }
                        Text(suggestion.summary).foregroundStyle(.secondary)
                        HStack {
                            Button("Review & accept") { store.acceptSuggestion(suggestion.id) }
                                .buttonStyle(.borderedProminent)
                            Button("Reject") { store.rejectSuggestion(suggestion.id) }
                        }
                        .controlSize(.small)
                    }
                    .padding(.vertical, 8)
                }
            }
        }
        .navigationTitle("Inbox")
    }
}

private struct PlaceholderDestinationView: View {
    let destination: SidebarDestination

    var body: some View {
        ContentUnavailableView(
            destination.title,
            systemImage: destination.symbol,
            description: Text("This native surface is connected to the shared DayWeave model as its implementation lands.")
        )
        .navigationTitle(destination.title)
    }
}

private struct QuickAddView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var store: PlannerStore
    @State private var title = ""
    @State private var kind: PlannerItemKind = .task
    @State private var minutes = 30

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                VStack(alignment: .leading) {
                    Text("Quick Add").font(.title2.weight(.semibold))
                    Text("DayWeave will place it in the next safe opening.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
            }

            TextField("What needs to happen?", text: $title)
                .textFieldStyle(.roundedBorder)
                .font(.title3)

            Picker("Type", selection: $kind) {
                ForEach(PlannerItemKind.allCases) { itemKind in
                    Label(itemKind.title, systemImage: itemKind.symbol).tag(itemKind)
                }
            }

            Stepper("Estimated duration: \(minutes) minutes", value: $minutes, in: 5...480, step: 5)

            HStack {
                Spacer()
                Button("Add & schedule") {
                    store.quickAdd(title: title, kind: kind, minutes: minutes)
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(24)
        .frame(width: 520)
    }
}

struct MenuBarView: View {
    @EnvironmentObject private var store: PlannerStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let active = store.activeItem {
                Text("In progress").font(.caption).foregroundStyle(.secondary)
                Text(active.title).font(.headline)
                Text(active.timeRange).font(.caption).foregroundStyle(.secondary)
                HStack {
                    Button("Pause") { store.pauseActive() }
                    Button("Complete") { store.complete(active.id) }
                }
            } else {
                ContentUnavailableView("Nothing active", systemImage: "checkmark.circle")
            }
            Divider()
            Button("Quick Add…") { store.isQuickAddPresented = true }
            Button("Recompose") { store.recomposeSchedule() }
        }
        .padding(14)
        .frame(width: 300)
    }
}

struct SettingsView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @State private var apiKey = ""

    var body: some View {
        Form {
            Section("Scheduling") {
                Stepper("Freeze the next \(store.freezeHours) hours", value: $store.freezeHours, in: 0...24)
                Stepper("Protect \(store.protectedFreeMinutes) free minutes", value: $store.protectedFreeMinutes, in: 0...480, step: 15)
                Toggle("Show completed blocks", isOn: $store.showCompleted)
            }
            Section("Accounts") {
                LabeledContent("Google", value: "Ready to connect")
                LabeledContent("Codex", value: codex.state.title)

                if case let .signedIn(email, _) = codex.state {
                    if let email { Text(email).foregroundStyle(.secondary) }
                    Button("Sign out of Codex", role: .destructive) { codex.signOut() }
                } else {
                    HStack {
                        Button("Sign in with ChatGPT") { codex.signInWithBrowser() }
                            .buttonStyle(.borderedProminent)
                        Button("Use device code") { codex.signInWithDeviceCode() }
                    }

                    if let code = codex.deviceCode {
                        LabeledContent("Device code", value: code)
                            .textSelection(.enabled)
                    }

                    DisclosureGroup("API key fallback") {
                        SecureField("OpenAI API key", text: $apiKey)
                        Button("Connect with API key") {
                            codex.signInWithAPIKey(apiKey)
                            apiKey = ""
                        }
                        .disabled(apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .padding()
    }
}
