import SwiftUI

struct RootView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
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
        .task {
            guard canonicalSync.isConfigured else { return }
            await canonicalSync.sync()
        }
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    Task { await canonicalSync.sync() }
                } label: {
                    Label("Sync & compose", systemImage: "arrow.triangle.2.circlepath")
                }
                .help("Pull canonical items, publish safe local changes, and compose a preview")
                .disabled(!canonicalSync.isConfigured || canonicalSync.isSyncing || !store.canMutatePlan)

                Button {
                    store.recomposeSchedule()
                } label: {
                    Label("Recompose", systemImage: "wand.and.stars")
                }
                .help("Reorder local flexible blocks; sync validates server constraints")
                .disabled(!store.canRecomposeSchedule)

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
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

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
                        .fill(persistenceColor)
                        .frame(width: 8, height: 8)
                    Text(persistenceLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .help(persistenceHelp)
                Label("Google Calendar · not connected", systemImage: "circle.dashed")
                    .foregroundStyle(.secondary)
                Label {
                    Text(canonicalSync.status.message).lineLimit(2)
                } icon: {
                    Image(systemName: canonicalSync.isSyncing ? "arrow.triangle.2.circlepath" : "network")
                }
                .font(.caption)
                .foregroundStyle(canonicalSync.status.isFailure ? .red : .secondary)
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

    private var persistenceColor: Color {
        if store.persistenceError != nil { return .red }
        return store.hasEncryptedPersistence ? .green : .secondary
    }

    private var persistenceLabel: String {
        if store.persistenceError != nil { return "Local save needs attention" }
        return store.hasEncryptedPersistence ? "Encrypted local plan" : "Local persistence unavailable"
    }

    private var persistenceHelp: String {
        store.persistenceError?.localizedDescription
            ?? (store.hasEncryptedPersistence
                ? "Planner state is encrypted on this Mac"
                : "This store has no encrypted persistence backend")
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
                .disabled(!store.canMutate(block))
            }
        }
        .padding(12)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12))
    }
}

private struct TodayView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

    var body: some View {
        VStack(spacing: 0) {
            TodayHeader()
            CanonicalSyncBanner()
            PreviewDiagnosticsStrip()
            Divider()
            if store.visibleBlocks.isEmpty {
                ContentUnavailableView {
                    Label(
                        emptyTitle,
                        systemImage: store.canonicalItems.isEmpty ? "calendar.badge.plus" : "calendar.badge.exclamationmark"
                    )
                } description: {
                    Text(emptyDescription)
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

    private var emptyTitle: String {
        if store.canonicalItems.isEmpty { return "No plan yet" }
        return store.blocks.isEmpty ? "No blocks fit this preview" : "No blocks scheduled today"
    }

    private var emptyDescription: String {
        if store.canonicalItems.isEmpty {
            return "Start with Quick Add. Nothing is scheduled until you add it."
        }
        if store.blocks.isEmpty {
            return "\(store.canonicalItems.count) canonical items are safely cached. Review the preview diagnostics or adjust availability."
        }
        return "The canonical plan has work on later days. Open Calendar to review the full seven-day preview."
    }
}

private struct PreviewDiagnosticsStrip: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

    var body: some View {
        let messages = diagnosticMessages
        if !messages.isEmpty {
            DisclosureGroup {
                VStack(alignment: .leading, spacing: 5) {
                    ForEach(Array(messages.enumerated()), id: \.offset) { _, message in
                        Text(message)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    ForEach(conflictedMutations) { mutation in
                        Button("Retry \(title(for: mutation.itemID)) edit on current revision") {
                            store.retryConflictedCanonicalMutation(mutation.id)
                        }
                        .buttonStyle(.link)
                        .disabled(!store.canRetryCanonicalMutation(mutation))
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.top, 6)
            } label: {
                Label("\(messages.count) preview and sync diagnostic\(messages.count == 1 ? "" : "s")", systemImage: "exclamationmark.triangle")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.orange)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 7)
        }
    }

    private var diagnosticMessages: [String] {
        var result = canonicalSync.warnings
        if store.blocks.contains(where: {
            $0.syncOrigin == .canonicalPreview || $0.syncOrigin == .externalPreview
        }), let issue = store.canonicalPreviewFreshnessIssue {
            result.append("Canonical preview actions are locked: \(issue)")
        }
        result.append(contentsOf: store.pendingCanonicalMutations.map { mutation in
            let title = store.canonicalItem(id: mutation.itemID)?.title ?? mutation.itemID.uuidString
            let state = mutation.disposition == .conflicted ? "conflict" : "pending edit"
            return "\(title): \(state) → \(mutation.desiredStatus.title). \(mutation.diagnostic ?? "Retained in encrypted local storage.")"
        })
        result.append(contentsOf: store.recurrenceSessionOutcomes.map { outcome in
            let title = store.canonicalItem(id: outcome.itemID)?.title ?? outcome.itemID.uuidString
            return "\(title): session \(outcome.sessionIndex) \(outcome.disposition.rawValue) at \(outcome.occurredAt.formatted()). Start the retained block to correct this outcome."
        })
        result.append(contentsOf: store.localCaptureDiagnostics
            .sorted { $0.key.uuidString < $1.key.uuidString }
            .map { id, diagnostic in
                let title = store.blocks.first(where: { $0.id == id })?.title ?? "Local capture"
                return "\(title): \(diagnostic)"
            })
        guard let preview = canonicalSync.lastPreview else { return result }
        result.append(contentsOf: preview.plan.unscheduled.map {
            "\(title(for: $0.itemID)): \($0.remaining)m unscheduled (\($0.reason)). \($0.message)"
        })
        result.append(contentsOf: preview.rejectedItems.map {
            "\($0.title): rejected from preview (\($0.reason))."
        })
        result.append(contentsOf: preview.ignoredPreviousAssignments.map {
            "Previous assignment for \(title(for: $0.itemID)) was ignored: \($0.reason)."
        })
        result.append(contentsOf: preview.plan.decisions.map {
            "Decision: \($0.displayDescription)"
        })
        result.append(contentsOf: preview.plan.violations.map {
            "Violation: \($0.displayDescription)"
        })
        return result
    }

    private func title(for itemID: UUID) -> String {
        store.canonicalItem(id: itemID)?.title ?? "Item"
    }

    private var conflictedMutations: [PendingCanonicalMutation] {
        store.pendingCanonicalMutations.filter { $0.disposition == .conflicted }
    }
}

private struct CanonicalSyncBanner: View {
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

    var body: some View {
        HStack(spacing: 9) {
            if canonicalSync.isSyncing {
                ProgressView().controlSize(.small)
            } else {
                Circle()
                    .fill(statusColor)
                    .frame(width: 7, height: 7)
            }
            Text(canonicalSync.status.message)
                .font(.caption)
                .foregroundStyle(canonicalSync.status.isFailure ? .red : .secondary)
                .lineLimit(2)
            Spacer()
            if !canonicalSync.warnings.isEmpty {
                Label("\(canonicalSync.warnings.count) to review", systemImage: "exclamationmark.triangle")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.orange)
                    .help(canonicalSync.warnings.joined(separator: "\n"))
            }
            Button("Sync now") {
                Task { await canonicalSync.sync() }
            }
            .controlSize(.small)
            .disabled(!canonicalSync.isConfigured || canonicalSync.isSyncing)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 9)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.55))
    }

    private var statusColor: Color {
        switch canonicalSync.status {
        case .configurationRequired: .secondary
        case .ready: .orange
        case .syncing: .blue
        case .online: .green
        case .failed: .red
        }
    }
}

private struct TodayHeader: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

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
            MetricChip(
                value: "\(store.todaysBlocks.count(where: { $0.status == .completed }))/\(store.todaysBlocks.count)",
                label: "done",
                symbol: "checkmark"
            )
            MetricChip(value: "\(store.protectedFreeMinutes)m", label: "protected", symbol: "shield")
            MetricChip(value: previewCoverage, label: "preview coverage", symbol: "chart.pie")
        }
        .padding(20)
    }

    private var previewCoverage: String {
        guard let score = canonicalSync.lastPreview?.plan.score else { return "—" }
        let total = score.scheduledMinutes + score.unscheduledMinutes
        guard total > 0 else { return "100%" }
        return "\(Int((Double(score.scheduledMinutes) / Double(total) * 100).rounded()))%"
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
                Text(block.startTimeLabel)
                    .font(.system(.caption, design: .monospaced).weight(.medium))
                Text("\(block.durationMinutes)m")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .frame(width: 102, alignment: .trailing)

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
                            .help("Fixed by the scheduler preview")
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
                            .disabled(!block.isFlexible || block.isHardConstraint)
                    }
                    .controlSize(.small)
                    .disabled(!store.canMutate(block))
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
            Button("Start") { store.start(block.id) }.disabled(!store.canMutate(block))
            Button("Mark Complete") { store.complete(block.id) }.disabled(!store.canMutate(block))
            Divider()
            Button("Do Later") { store.doLater(block.id) }
                .disabled(!store.canMutate(block) || !block.isFlexible || block.isHardConstraint)
            Button("Skip") { store.skip(block.id) }.disabled(!store.canMutate(block))
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
                        .id(block.id)
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
    @State private var recoveryTitle: String

    init(block: ScheduleBlock) {
        self.block = block
        _recoveryTitle = State(initialValue: block.title)
    }

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
                    LabeledContent("Placement", value: block.isHardConstraint ? "Fixed in preview" : "Flexible in preview")
                    if let itemID = block.sourceItemID,
                       let item = store.canonicalItem(id: itemID) {
                        LabeledContent("Revision", value: String(item.revision))
                        if let deadline = item.deadlineAt {
                            LabeledContent(
                                "Deadline",
                                value: deadline.formatted(date: .abbreviated, time: .shortened)
                            )
                        }
                        LabeledContent("Split", value: splitDescription(item.splitPolicy))
                        if item.parentID != nil {
                            LabeledContent("Hierarchy", value: block.project ?? "Nested item")
                        }
                        if item.recurrence != nil {
                            LabeledContent("Recurrence", value: "Canonical rule cached; outcome context applied on preview")
                        }
                    }
                }

                InspectorSection(title: "Why here?") {
                    Text(block.placementReason ?? "The scheduler did not provide a placement explanation.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                if !block.notes.isEmpty {
                    InspectorSection(title: "Notes") {
                        Text(block.notes).font(.subheadline)
                    }
                }

                if let mutation = store.canonicalMutation(for: block),
                   mutation.disposition == .conflicted {
                    InspectorSection(title: "Conflict recovery") {
                        Text(mutation.diagnostic ?? "The server revision changed while this local edit was pending.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        Button("Retry edit on current revision") {
                            store.retryConflictedCanonicalMutation(mutation.id)
                        }
                        .disabled(!store.canRetryCanonicalMutation(mutation))
                        Text("This keeps the requested status, rebases it onto the currently cached revision, and leaves it pending until the next sync.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if block.isLocallyAuthored, block.sourceItemID == nil {
                    InspectorSection(title: "Local capture recovery") {
                        TextField("Title", text: $recoveryTitle)
                            .textFieldStyle(.roundedBorder)
                        Text("\(recoveryTitle.unicodeScalars.count)/\(PlannerStore.maximumCanonicalTitleScalars) Unicode characters")
                            .font(.caption)
                            .foregroundStyle(localCaptureTitleIsValid ? Color.secondary : Color.red)
                        if let diagnostic = store.localCaptureDiagnostics[block.id] {
                            Text(diagnostic)
                                .font(.caption)
                                .foregroundStyle(.orange)
                        }
                        HStack {
                            Button("Save title") {
                                _ = store.updateLocalCapture(block.id, title: recoveryTitle)
                            }
                            .disabled(!localCaptureTitleIsValid || !store.canMutatePlan)
                            Button("Delete local capture", role: .destructive) {
                                store.deleteLocalCapture(block.id)
                            }
                            .disabled(!store.canMutatePlan)
                        }
                        Text("Editing or deleting this unpublished capture changes encrypted local state only.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                HStack {
                    Button("Start") { store.start(block.id) }
                        .buttonStyle(.borderedProminent)
                    Button("Complete") { store.complete(block.id) }
                    Menu("More") {
                        Button("Do Later") { store.doLater(block.id) }
                            .disabled(!block.isFlexible || block.isHardConstraint)
                        Button("Skip") { store.skip(block.id) }
                    }
                }
                .disabled(!store.canMutate(block))
            }
            .padding(18)
        }
    }

    private var localCaptureTitleIsValid: Bool {
        PlannerStore.normalizedCanonicalTitle(recoveryTitle) != nil
    }

    private func splitDescription(_ policy: DayWeaveSplitPolicy) -> String {
        switch policy {
        case .indivisible: "Indivisible"
        case let .splittable(minimum, maximum): "\(minimum / 60)–\(maximum / 60) minute sessions"
        case .unknown: "Unsupported — read only"
        }
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
                TextField("Save a local planner note…", text: $draft, axis: .vertical)
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
                    Text("Captured locally first; sync validates and composes its placement.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
            }

            TextField("What needs to happen?", text: $title)
                .textFieldStyle(.roundedBorder)
                .font(.title3)

            Text("\(title.unicodeScalars.count)/\(PlannerStore.maximumCanonicalTitleScalars) Unicode characters")
                .font(.caption)
                .foregroundStyle(titleIsValid ? Color.secondary : Color.red)

            Picker("Type", selection: $kind) {
                ForEach(PlannerItemKind.allCases) { itemKind in
                    Label(itemKind.title, systemImage: itemKind.symbol).tag(itemKind)
                }
            }

            Stepper("Estimated duration: \(minutes) minutes", value: $minutes, in: 5...480, step: 5)

            HStack {
                Spacer()
                Button("Add locally") {
                    if store.quickAdd(title: title, kind: kind, minutes: minutes) {
                        dismiss()
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(
                    !titleIsValid || !store.canMutatePlan
                )
            }
        }
        .padding(24)
        .frame(width: 520)
    }

    private var titleIsValid: Bool {
        PlannerStore.normalizedCanonicalTitle(title) != nil
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
                .disabled(!store.canMutate(active))
            } else {
                ContentUnavailableView("Nothing active", systemImage: "checkmark.circle")
            }
            Divider()
            Button("Quick Add…") { store.isQuickAddPresented = true }
                .disabled(!store.canMutatePlan)
            Button("Recompose") { store.recomposeSchedule() }
                .disabled(!store.canRecomposeSchedule)
        }
        .padding(14)
        .frame(width: 300)
    }
}

struct SettingsView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @State private var apiKey = ""
    @State private var dayWeaveAPIBaseURL = ""
    @State private var dayWeaveBearerToken = ""
    @State private var isCanonicalResetConfirmationPresented = false

    var body: some View {
        Form {
            Section("Scheduling") {
                Stepper("Freeze the next \(store.freezeHours) hours", value: $store.freezeHours, in: 0...24)
                Stepper("Protect \(store.protectedFreeMinutes) free minutes", value: $store.protectedFreeMinutes, in: 0...480, step: 15)
                Toggle("Show completed blocks", isOn: $store.showCompleted)
            }
            .disabled(!store.canMutatePlan)
            Section("Accounts") {
                LabeledContent("Google", value: "Not connected")
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
            Section("DayWeave API") {
                TextField("https://dayweave.example.com", text: $dayWeaveAPIBaseURL)
                    .textContentType(.URL)
                SecureField(
                    suggestionSync.tokenConfigured ? "New bearer token (blank only for the same API origin)" : "Bearer token",
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
                            canonicalSync.configurationDidChange()
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                        dayWeaveAPIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            || (!suggestionSync.tokenConfigured
                                && dayWeaveBearerToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            || suggestionSync.isRefreshing
                            || !suggestionSync.activeProposalIDs.isEmpty
                    )

                    if suggestionSync.tokenConfigured {
                        Button("Remove saved token", role: .destructive) {
                            suggestionSync.clearBearerToken()
                            canonicalSync.configurationDidChange()
                            dayWeaveBearerToken = ""
                        }
                        .disabled(
                            suggestionSync.isRefreshing
                                || !suggestionSync.activeProposalIDs.isEmpty
                        )
                    }
                }

                LabeledContent("Credential", value: suggestionSync.tokenConfigured ? "Stored in Keychain" : "Not saved")
                Text(suggestionSync.status.message)
                    .font(.caption)
                    .foregroundStyle(suggestionSync.status.isFailure ? .red : .secondary)
                Text("Remote HTTP is rejected; plain HTTP is accepted only for localhost development. Changing the API origin requires re-entering the token, which is never saved in the planner snapshot.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                LabeledContent("Planner sync", value: canonicalSync.status.message)
                    .foregroundStyle(canonicalSync.status.isFailure ? .red : .secondary)
                Button("Reset local canonical cache…", role: .destructive) {
                    isCanonicalResetConfirmationPresented = true
                }
                .disabled(!store.canMutatePlan)
            }
            Section("Local data") {
                LabeledContent(
                    "Planner storage",
                    value: store.persistenceError != nil
                        ? "Needs attention"
                        : (store.hasEncryptedPersistence ? "Encrypted" : "Not configured")
                )
                if let error = store.persistenceError {
                    Text(error.localizedDescription)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                } else if store.hasEncryptedPersistence {
                    Text("Schedule content is sealed with AES-GCM. Its device key is stored in this Mac’s Keychain.")
                        .foregroundStyle(.secondary)
                } else {
                    Text("No encrypted persistence backend is attached; changes are memory-only.")
                        .foregroundStyle(.orange)
                }
            }
        }
        .confirmationDialog(
            "Reset the local canonical cache?",
            isPresented: $isCanonicalResetConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Reset local cache", role: .destructive) {
                canonicalSync.resetCanonicalSyncState()
            }
        } message: {
            Text("This removes cached canonical items, preview blocks, recurrence history, and pending/conflicted canonical edits from this Mac. It does not change the server or locally authored captures.")
        }
        .formStyle(.grouped)
        .padding()
        .onAppear {
            dayWeaveAPIBaseURL = suggestionSync.baseURLString
        }
    }
}
