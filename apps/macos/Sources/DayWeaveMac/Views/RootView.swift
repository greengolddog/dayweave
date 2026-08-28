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
                .disabled(!store.canMutatePlan)

                Button {
                    store.isQuickAddPresented = true
                } label: {
                    Label("Quick Add", systemImage: "plus")
                }
                .help("Quick Add (⇧⌘N)")
                .disabled(!store.canMutatePlan)
            }
        }
    }

    @ViewBuilder
    private var destinationContent: some View {
        switch store.destination ?? .today {
        case .today:
            TodayView()
        case .calendar:
            CalendarDestinationView()
        case .inbox:
            SuggestionsInboxView()
        case .habits:
            HabitsDestinationView()
        case .projects:
            ProjectsDestinationView()
        case .goals:
            GoalsDestinationView()
        case .statistics:
            StatisticsDestinationView()
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
                        .fill(store.persistenceError == nil ? .green : .red)
                        .frame(width: 8, height: 8)
                    Text(store.persistenceError == nil ? "Encrypted local plan" : "Local save needs attention")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .help(store.persistenceError?.localizedDescription ?? "Planner state is encrypted on this Mac")
                Label("Google Calendar · not connected", systemImage: "circle.dashed")
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
            if store.visibleBlocks.isEmpty {
                ContentUnavailableView {
                    Label("No plan yet", systemImage: "calendar.badge.plus")
                } description: {
                    Text("Start with Quick Add. Nothing is scheduled until you add it.")
                } actions: {
                    Button("Quick Add") { store.isQuickAddPresented = true }
                        .buttonStyle(.borderedProminent)
                        .disabled(!store.canMutatePlan)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
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
                    .disabled(!store.canMutatePlan)
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
                .disabled(!store.canMutatePlan)
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
                .disabled(
                    draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || !store.canMutatePlan
                )
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
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @State private var proposalBeingEdited: DayWeaveProposal?

    private var pendingLocalSuggestions: [PlanningSuggestion] {
        store.suggestions.filter { $0.state == .pending }
    }

    var body: some View {
        List {
            Section {
                Label {
                    Text("External tools can submit proposals, but they cannot change this schedule directly. Approval records a server-side decision only.")
                } icon: {
                    Image(systemName: "shield.checkered")
                        .foregroundStyle(.green)
                }
                .font(.subheadline)

                HStack(spacing: 8) {
                    Circle()
                        .fill(statusColor)
                        .frame(width: 8, height: 8)
                    Text(suggestionSync.status.message)
                        .font(.caption)
                        .foregroundStyle(suggestionSync.status.isFailure ? .red : .secondary)
                    Spacer()
                    if suggestionSync.isRefreshing {
                        ProgressView().controlSize(.small)
                    }
                    Button("Refresh") {
                        Task { await suggestionSync.refresh() }
                    }
                    .controlSize(.small)
                    .disabled(
                        !suggestionSync.isConfigured
                            || suggestionSync.isRefreshing
                            || !suggestionSync.activeProposalIDs.isEmpty
                    )
                }
            }

            if !pendingLocalSuggestions.isEmpty {
                Section("On this Mac") {
                    ForEach(pendingLocalSuggestions) { suggestion in
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

            Section("External proposals") {
                if suggestionSync.proposals.isEmpty {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(emptyExternalTitle)
                            .font(.headline)
                        Text(emptyExternalDetail)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        if !suggestionSync.isConfigured {
                            SettingsLink {
                                Label("Open API Settings", systemImage: "gearshape")
                            }
                        }
                    }
                    .padding(.vertical, 8)
                } else {
                    ForEach(suggestionSync.proposals) { proposal in
                        RemoteSuggestionRow(
                            proposal: proposal,
                            isWorking: suggestionSync.activeProposalIDs.contains(proposal.id),
                            edit: { proposalBeingEdited = proposal }
                        )
                    }
                }
            }
        }
        .navigationTitle("Inbox")
        .task {
            guard suggestionSync.isConfigured else { return }
            await suggestionSync.refresh()
        }
        .sheet(item: $proposalBeingEdited) { proposal in
            EditRemoteSuggestionView(proposal: proposal)
                .environmentObject(suggestionSync)
        }
    }

    private var statusColor: Color {
        switch suggestionSync.status {
        case .online: .green
        case .refreshing: .blue
        case .failed: .red
        case .ready: .orange
        case .configurationRequired: .secondary
        }
    }

    private var emptyExternalTitle: String {
        if suggestionSync.status.isFailure {
            "External proposals are unavailable"
        } else if suggestionSync.isConfigured {
            "No pending external proposals"
        } else {
            "External proposals are not configured"
        }
    }

    private var emptyExternalDetail: String {
        if suggestionSync.status.isFailure {
            "The last API request failed. Local suggestions and planning remain available."
        } else if suggestionSync.isConfigured {
            "Refresh to check the authenticated DayWeave API. Local planning remains available offline."
        } else {
            "Add an API URL and bearer token in Settings. Local suggestions continue to work without them."
        }
    }
}

private struct RemoteSuggestionRow: View {
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore

    let proposal: DayWeaveProposal
    let isWorking: Bool
    let edit: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Image(systemName: "network")
                    .foregroundStyle(.blue)
                Text(proposal.title)
                    .font(.headline)
                Spacer()
                Text(proposal.source.title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let explanation = proposal.explanation, !explanation.isEmpty {
                Text(explanation)
                    .foregroundStyle(.secondary)
            }

            HStack(spacing: 12) {
                Label(proposal.kind.title, systemImage: "doc.text")
                Label(
                    "Expires \(proposal.expiresAt.formatted(date: .abbreviated, time: .shortened))",
                    systemImage: "clock"
                )
                Text("Revision \(proposal.revision)")
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            HStack {
                Button("Approve proposal") {
                    Task { await suggestionSync.accept(proposal) }
                }
                .buttonStyle(.borderedProminent)
                Button("Reject") {
                    Task { await suggestionSync.reject(proposal) }
                }
                Button("Edit…", action: edit)
                if isWorking {
                    ProgressView().controlSize(.small)
                }
            }
            .controlSize(.small)
            .disabled(isWorking || suggestionSync.isRefreshing)
        }
        .padding(.vertical, 8)
    }
}

private struct EditRemoteSuggestionView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore

    let proposal: DayWeaveProposal
    @State private var title: String
    @State private var explanation: String

    init(proposal: DayWeaveProposal) {
        self.proposal = proposal
        _title = State(initialValue: proposal.title)
        _explanation = State(initialValue: proposal.explanation ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                VStack(alignment: .leading) {
                    Text("Edit external proposal")
                        .font(.title2.weight(.semibold))
                    Text("This edits the review proposal only; it does not change the schedule.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
            }

            TextField("Title", text: $title)
                .textFieldStyle(.roundedBorder)
            TextField("Explanation", text: $explanation, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(3...8)

            if proposal.explanation != nil, explanation.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text("The current API can replace an explanation but cannot clear it.")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }

            HStack {
                Spacer()
                Button("Save proposal") {
                    Task {
                        if await suggestionSync.edit(
                            proposal,
                            title: title,
                            explanation: explanation
                        ) {
                            dismiss()
                        }
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canSave || suggestionSync.activeProposalIDs.contains(proposal.id))
            }
        }
        .padding(24)
        .frame(width: 560)
    }

    private var canSave: Bool {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let explanation = explanation.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return false }
        if proposal.explanation != nil, explanation.isEmpty { return false }
        return title != proposal.title || explanation != (proposal.explanation ?? "")
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
                .disabled(
                    title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || !store.canMutatePlan
                )
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
                .disabled(!store.canMutatePlan)
            } else {
                ContentUnavailableView("Nothing active", systemImage: "checkmark.circle")
            }
            Divider()
            Button("Quick Add…") { store.isQuickAddPresented = true }
                .disabled(!store.canMutatePlan)
            Button("Recompose") { store.recomposeSchedule() }
                .disabled(!store.canMutatePlan)
        }
        .padding(14)
        .frame(width: 300)
    }
}

struct SettingsView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @State private var apiKey = ""
    @State private var dayWeaveAPIBaseURL = ""
    @State private var dayWeaveBearerToken = ""

    var body: some View {
        Form {
            Section("Scheduling") {
                Stepper("Freeze the next \(store.freezeHours) hours", value: $store.freezeHours, in: 0...24)
                Stepper("Protect \(store.protectedFreeMinutes) free minutes", value: $store.protectedFreeMinutes, in: 0...480, step: 15)
                Toggle("Show completed blocks", isOn: $store.showCompleted)
            }
            .disabled(!store.canMutatePlan)
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
            Section("DayWeave suggestions API") {
                TextField("https://dayweave.example.com", text: $dayWeaveAPIBaseURL)
                    .textContentType(.URL)
                SecureField(
                    suggestionSync.tokenConfigured ? "New bearer token (leave blank to keep saved token)" : "Bearer token",
                    text: $dayWeaveBearerToken
                )

                HStack {
                    Button("Save API settings") {
                        if suggestionSync.applyConfiguration(
                            baseURL: dayWeaveAPIBaseURL,
                            newToken: dayWeaveBearerToken
                        ) {
                            dayWeaveBearerToken = ""
                            dayWeaveAPIBaseURL = suggestionSync.baseURLString
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                        dayWeaveAPIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            || (!suggestionSync.tokenConfigured
                                && dayWeaveBearerToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    )

                    if suggestionSync.tokenConfigured {
                        Button("Remove saved token", role: .destructive) {
                            suggestionSync.clearBearerToken()
                            dayWeaveBearerToken = ""
                        }
                    }
                }

                LabeledContent("Credential", value: suggestionSync.tokenConfigured ? "Stored in Keychain" : "Not saved")
                Text(suggestionSync.status.message)
                    .font(.caption)
                    .foregroundStyle(suggestionSync.status.isFailure ? .red : .secondary)
                Text("Remote HTTP is rejected; plain HTTP is accepted only for localhost development. The token is never saved in the planner snapshot.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Local data") {
                LabeledContent("Planner storage", value: store.persistenceError == nil ? "Encrypted" : "Needs attention")
                if let error = store.persistenceError {
                    Text(error.localizedDescription)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                } else {
                    Text("Schedule content is sealed with AES-GCM. Its device key is stored in this Mac’s Keychain.")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .padding()
        .onAppear {
            dayWeaveAPIBaseURL = suggestionSync.baseURLString
        }
    }
}
