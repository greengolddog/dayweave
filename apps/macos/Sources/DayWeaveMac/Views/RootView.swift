import AppKit
import SwiftUI

private extension Notification.Name {
    static let dayWeaveShowSuggestionsInbox = Notification.Name(
        "com.greengolddog.dayweave.show-suggestions-inbox"
    )
}

private func executionSession(
    _ session: DayWeaveExecutionSession,
    matches block: ScheduleBlock
) -> Bool {
    block.sourceItemID == session.itemID
        && block.sourceItemRevision == session.itemRevision
        && block.occurrenceID == session.occurrenceID
        && (block.sessionIndex ?? 0) == session.sessionIndex
}

private func executionBlock(
    matching session: DayWeaveExecutionSession,
    in blocks: [ScheduleBlock]
) -> ScheduleBlock? {
    if let plannedBlockID = session.plannedBlockID,
       let block = blocks.first(where: {
           $0.id == plannedBlockID && executionSession(session, matches: $0)
       }) {
        return block
    }
    let matches = blocks.filter { executionSession(session, matches: $0) }
    if matches.count == 1 { return matches[0] }
    return matches.first(where: {
        $0.syncOrigin == .remoteExecutionLease && $0.id == session.id
    })
}

struct RootView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @State private var columnVisibility: NavigationSplitViewVisibility = .all
    @State private var isResolvingExpiredBreak = false

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
            QuickCaptureView()
                .environmentObject(store)
        }
        .onAppear {
            codex.startIfNeeded()
        }
        .alert("Your break has ended", isPresented: expiredBreakAlertBinding) {
            Button("Resume") {
                if let blockID = activeExecutionBlockID {
                    isResolvingExpiredBreak = true
                    Task {
                        _ = await executionSync.resume(blockID)
                        isResolvingExpiredBreak = false
                    }
                }
            }
            .accessibilityIdentifier("execution.expired-break.resume")
            .disabled(executionSync.isSyncing
                || store.executionState.pendingCommand != nil
                || !store.canMutatePlan)
            Button("Keep paused") {
                _ = executionSync.keepPausedAfterExpiredBreak()
            }
            .accessibilityIdentifier("execution.expired-break.keep-paused")
            .disabled(executionSync.isSyncing
                || store.executionState.pendingCommand != nil
                || !store.canMutatePlan)
        } message: {
            Text("Choose whether to resume the authoritative session or keep it paused. DayWeave will not resume it automatically.")
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
                    Task { await executionSync.refresh() }
                } label: {
                    Label("Refresh execution", systemImage: "timer")
                }
                .help("Reconcile the authoritative execution lease and complete history")
                .disabled(executionSync.isSyncing || !store.canMutatePlan)
                .accessibilityIdentifier("execution.refresh")

                Button {
                    store.recomposeSchedule()
                } label: {
                    Label("Recompose", systemImage: "wand.and.stars")
                }
                .help("Reorder local flexible blocks; sync validates server constraints")
                .disabled(!store.canRecomposeSchedule)

                if store.destination != .inbox {
                    Button {
                        store.isQuickAddPresented = true
                    } label: {
                        Label("Quick Capture", systemImage: "plus")
                    }
                    .help("Quick Capture (⇧⌘N)")
                    .disabled(!store.canMutatePlan)
                    .accessibilityIdentifier("planner.quick-capture")
                }
            }
        }
    }

    private var expiredBreakAlertBinding: Binding<Bool> {
        Binding(
            get: {
                executionSync.expiredBreakChoiceRequired && !isResolvingExpiredBreak
            },
            set: { _ in }
        )
    }

    private var activeExecutionBlockID: UUID? {
        guard let active = executionSync.activeSession else { return nil }
        return executionBlock(matching: active, in: store.blocks)?.id
    }

    @ViewBuilder
    private var destinationContent: some View {
        switch store.destination ?? .today {
        case .today:
            TodayView()
        case .calendar:
            CalendarDestinationView()
        case .inbox:
            UnifiedInboxView()
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
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var googleIntegration: GoogleIntegrationStore

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
                Label {
                    Text(googleIntegration.sidebarMessage).lineLimit(2)
                } icon: {
                    Image(systemName: googleIntegration.sidebarSymbol)
                }
                .font(.caption)
                .foregroundStyle(googleIntegration.status.isFailure ? .red : .secondary)
                Label {
                    Text(canonicalSync.status.message).lineLimit(2)
                } icon: {
                    Image(systemName: canonicalSync.isSyncing ? "arrow.triangle.2.circlepath" : "network")
                }
                .font(.caption)
                .foregroundStyle(canonicalSync.status.isFailure ? .red : .secondary)
                Label {
                    Text(executionSync.status.message).lineLimit(2)
                } icon: {
                    Image(systemName: executionSync.isSyncing ? "timer.circle" : "timer")
                }
                .font(.caption)
                .foregroundStyle(executionStatusIsFailure ? .red : .secondary)
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) {
            if let active = focusedExecutionBlock {
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

    private var executionStatusIsFailure: Bool {
        switch executionSync.status.phase {
        case .failed, .authenticationRequired, .notConfigured, .offline:
            true
        case .ready, .syncing, .connected:
            false
        }
    }

    private var focusedExecutionBlock: ScheduleBlock? {
        if let session = executionSync.activeSession,
           let block = executionBlock(matching: session, in: store.blocks) {
            return block
        }
        return store.activeItem
    }
}

private struct ActiveMiniPlayer: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(block.status == .paused ? "PAUSED" : "NOW")
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
                if block.sourceItemID != nil {
                    AuthoritativeExecutionControls(block: block, includesCustomPause: false)
                        .controlSize(.mini)
                } else {
                    Button {
                        store.pauseActive()
                    } label: {
                        Image(systemName: "pause.fill")
                    }
                    .buttonStyle(.borderless)
                    .disabled(!store.canMutate(block))
                    .accessibilityLabel("Pause current item")
                    .help("Pause current item")
                }
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
            ExecutionSyncBanner()
            PreviewDiagnosticsStrip()
            Divider()
            if store.visibleBlocks.isEmpty {
                ContentUnavailableView {
                    Label(
                        emptyTitle,
                        systemImage: inboxItemCount == 0
                            ? "calendar.badge.plus"
                            : "tray.full"
                    )
                } description: {
                    Text(emptyDescription)
                } actions: {
                    HStack {
                        if inboxItemCount > 0 {
                            Button("Open Inbox") { store.destination = .inbox }
                                .buttonStyle(.borderedProminent)
                                .accessibilityIdentifier("today.open-inbox")
                            quickCaptureButton.buttonStyle(.bordered)
                        } else {
                            quickCaptureButton.buttonStyle(.borderedProminent)
                        }
                    }
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
        if inboxItemCount == 0, store.canonicalItems.isEmpty { return "No plan yet" }
        if inboxItemCount > 0 { return "Items are waiting in Inbox" }
        return store.blocks.isEmpty ? "No blocks fit this preview" : "No blocks scheduled today"
    }

    private var emptyDescription: String {
        if inboxItemCount == 0, store.canonicalItems.isEmpty {
            return "Start with Quick Capture. It stays in Inbox until you decide it is ready for planning."
        }
        if inboxItemCount > 0 {
            return "\(inboxItemCount) item\(inboxItemCount == 1 ? " is" : "s are") safely retained for triage. Planned items become eligible after sync and composition."
        }
        if store.blocks.isEmpty {
            return "\(store.canonicalItems.count) canonical items are safely cached. Review the preview diagnostics or adjust availability."
        }
        return "The canonical plan has work on later days. Open Calendar to review the full seven-day preview."
    }

    private var inboxItemCount: Int {
        var ids = Set(store.canonicalItems.compactMap { item in
            item.status == .inbox || item.status == .planned ? item.id : nil
        })
        ids.formUnion(store.pendingCanonicalAuthoringMutations.map(\.itemID))
        return ids.count
    }

    private var quickCaptureButton: some View {
        Button("Quick Capture") { store.isQuickAddPresented = true }
            .disabled(!store.canMutatePlan)
            .accessibilityIdentifier("today.quick-capture")
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
                        CanonicalConflictRecoveryControls(
                            mutation: mutation,
                            itemTitle: title(for: mutation.itemID)
                        )
                    }
                    ForEach(conflictedSensitivityMutations) { mutation in
                        CanonicalSensitivityConflictRecoveryControls(
                            mutation: mutation
                        )
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
        result.append(contentsOf: store.pendingCanonicalSensitivityMutations.map { mutation in
            let title = store.canonicalItem(id: mutation.itemID)?.title ?? mutation.itemID.uuidString
            let state = mutation.disposition == .conflicted ? "privacy conflict" : "pending privacy edit"
            let change = mutation.requestedIsSensitive ? "mark sensitive" : "remove own sensitive marker"
            return "\(title): \(state) → \(change). \(mutation.diagnostic ?? "Retained in encrypted local storage.")"
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

    private var conflictedSensitivityMutations: [PendingCanonicalSensitivityMutation] {
        store.pendingCanonicalSensitivityMutations.filter { $0.disposition == .conflicted }
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

private struct ExecutionSyncBanner: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore

    var body: some View {
        HStack(spacing: 9) {
            if executionSync.isSyncing {
                ProgressView().controlSize(.small)
            } else {
                Circle()
                    .fill(statusColor)
                    .frame(width: 7, height: 7)
            }
            Text(executionSync.status.message)
                .font(.caption)
                .foregroundStyle(statusIsFailure ? .red : .secondary)
                .lineLimit(2)
            Spacer()
            if store.executionState.pendingCommand != nil {
                Label("Replay protected", systemImage: "lock.shield")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.orange)
                    .help("The exact command bytes and idempotency key are retained in encrypted storage until reconciliation succeeds.")
            }
            Button("Refresh execution") {
                Task { await executionSync.refresh() }
            }
            .controlSize(.small)
            .disabled(executionSync.isSyncing || !store.canMutatePlan)
            .accessibilityIdentifier("execution.refresh.banner")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 9)
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.4))
    }

    private var statusColor: Color {
        switch executionSync.status.phase {
        case .notConfigured: .secondary
        case .ready: .orange
        case .syncing: .blue
        case .connected: .green
        case .offline, .authenticationRequired, .failed: .red
        }
    }

    private var statusIsFailure: Bool {
        switch executionSync.status.phase {
        case .offline, .authenticationRequired, .failed:
            true
        case .notConfigured, .ready, .syncing, .connected:
            false
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
                    if block.isSensitive {
                        Image(systemName: "checkmark.shield.fill")
                            .font(.caption)
                            .foregroundStyle(.purple)
                            .help(sensitivityHelp)
                            .accessibilityLabel(sensitivityHelp)
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

                if block.sourceItemID != nil {
                    AuthoritativeExecutionControls(block: block)
                        .controlSize(.small)
                } else if block.status == .active || block.status == .paused {
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
            if block.sourceItemID != nil {
                AuthoritativeExecutionContextMenu(block: block)
            } else {
                Button("Start") { store.start(block.id) }.disabled(!store.canMutate(block))
                Button("Mark Complete") { store.complete(block.id) }.disabled(!store.canMutate(block))
                Divider()
                Button("Do Later") { store.doLater(block.id) }
                    .disabled(!store.canMutate(block) || !block.isFlexible || block.isHardConstraint)
                Button("Skip") { store.skip(block.id) }.disabled(!store.canMutate(block))
            }
        }
    }

    private var sensitivityHelp: String {
        guard let itemID = block.sourceItemID else {
            return "Sensitive local capture"
        }
        if let mutation = store.canonicalSensitivityMutation(itemID: itemID),
           mutation.requiresSensitivePresentation {
            return mutation.requestedIsSensitive
                ? "Sensitive privacy mark pending sync"
                : "Sensitive until submitted privacy changes are reconciled"
        }
        return switch store.canonicalSensitivityPresentation(itemID: itemID) {
        case .standard: "Sensitive schedule details"
        case .own: "Sensitive on this item"
        case .inherited: "Sensitive through its hierarchy"
        }
    }
}

private enum ExecutionPauseEditorMode: String, CaseIterable, Identifiable {
    case duration
    case until

    var id: Self { self }
    var title: String { self == .duration ? "Duration" : "Until" }
}

private struct AuthoritativeExecutionControls: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    let block: ScheduleBlock
    var includesCustomPause = true

    @State private var isPauseEditorPresented = false
    @State private var pauseMode = ExecutionPauseEditorMode.duration
    @State private var pauseMinutes = 15
    @State private var pauseUntil = Date.now.addingTimeInterval(15 * 60)
    @State private var pauseReason = ""

    var body: some View {
        HStack(spacing: 7) {
            switch block.status {
            case .scheduled:
                Button("Start") {
                    Task { await executionSync.start(block.id) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canStart)
                .accessibilityIdentifier("execution.start.\(block.id.uuidString.lowercased())")
                Menu("More") {
                    Button("Do later") { store.doLater(block.id) }
                        .disabled(!canEditScheduledBlock
                            || !block.isFlexible || block.isHardConstraint)
                    Button("Skip without starting") { store.skip(block.id) }
                        .disabled(!canEditScheduledBlock)
                }
            case .active:
                pauseMenu
                    .buttonStyle(.borderedProminent)
                    .disabled(!canControlOpenLease)
                    .accessibilityIdentifier("execution.pause.\(block.id.uuidString.lowercased())")
                terminalMenu
                    .disabled(!canControlOpenLease)
            case .paused:
                Button("Resume") {
                    Task { await executionSync.resume(block.id) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canControlOpenLease)
                .accessibilityIdentifier("execution.resume.\(block.id.uuidString.lowercased())")
                terminalMenu
                    .disabled(!canControlOpenLease)
            default:
                EmptyView()
            }
        }
        .sheet(isPresented: $isPauseEditorPresented) {
            ExecutionPauseEditor(
                mode: $pauseMode,
                minutes: $pauseMinutes,
                pauseUntil: $pauseUntil,
                reason: $pauseReason,
                onCancel: { isPauseEditorPresented = false },
                onPause: applyCustomPause
            )
        }
    }

    private var pauseMenu: some View {
        Menu("Pause") {
            Button("Pause indefinitely") {
                pause(durationSeconds: nil)
            }
            Divider()
            ForEach([5, 15, 30, 60], id: \.self) { minutes in
                Button("Pause for \(minutes) minutes") {
                    pause(durationSeconds: UInt32(minutes * 60))
                }
            }
            if includesCustomPause {
                Divider()
                Button("Custom break…") {
                    pauseMode = .duration
                    pauseMinutes = 15
                    pauseUntil = Date.now.addingTimeInterval(15 * 60)
                    pauseReason = ""
                    isPauseEditorPresented = true
                }
            }
        }
        .help("Pause for a stated duration, until a time, or indefinitely")
    }

    private var terminalMenu: some View {
        Menu("Finish") {
            Button("Complete") {
                Task { await executionSync.complete(block.id) }
            }
            .accessibilityIdentifier("execution.complete.\(block.id.uuidString.lowercased())")
            Button("Skip") {
                Task { await executionSync.skip(block.id) }
            }
            .accessibilityIdentifier("execution.skip.\(block.id.uuidString.lowercased())")
        }
    }

    private var canStart: Bool {
        operationIsUnlocked
            && block.status == .scheduled
            && store.canMutate(block)
            && executionSync.activeSession == nil
            && store.executionState.historyVerified
            && store.executionState.pendingCommand == nil
            && store.pendingCanonicalMutations.isEmpty
            && block.sourceItemRevision == block.sourceItemID.flatMap {
                store.canonicalItem(id: $0)?.revision
            }
            && block.sourceItemID.flatMap { store.canonicalItem(id: $0) }?.isExecutable == true
            && !store.executionState.terminalOutcomes.values.contains { outcome in
                outcome.session.itemID == block.sourceItemID
                    && outcome.session.itemRevision == block.sourceItemRevision
                    && outcome.session.occurrenceID == block.occurrenceID
                    && outcome.session.sessionIndex == (block.sessionIndex ?? 0)
            }
    }

    private var canControlOpenLease: Bool {
        guard operationIsUnlocked, let active = executionSync.activeSession else { return false }
        return executionSession(active, matches: block)
    }

    private var operationIsUnlocked: Bool {
        store.canMutatePlan
            && !executionSync.isSyncing
            && store.executionState.pendingCommand == nil
    }

    private var canEditScheduledBlock: Bool {
        operationIsUnlocked && store.canMutate(block)
    }

    private func pause(durationSeconds: UInt32?) {
        Task {
            await executionSync.pause(block.id, durationSeconds: durationSeconds)
        }
    }

    private func applyCustomPause() {
        let reason = normalizedPauseReason
        let duration = pauseMode == .duration ? UInt32(pauseMinutes * 60) : nil
        let until = pauseMode == .until ? pauseUntil : nil
        isPauseEditorPresented = false
        Task {
            await executionSync.pause(
                block.id,
                durationSeconds: duration,
                pauseUntil: until,
                reason: reason
            )
        }
    }

    private var normalizedPauseReason: String? {
        let trimmed = pauseReason.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

private struct ExecutionPauseEditor: View {
    @Binding var mode: ExecutionPauseEditorMode
    @Binding var minutes: Int
    @Binding var pauseUntil: Date
    @Binding var reason: String
    let onCancel: () -> Void
    let onPause: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Pause session").font(.title2.weight(.semibold))
            Picker("Break type", selection: $mode) {
                ForEach(ExecutionPauseEditorMode.allCases) { mode in
                    Text(mode.title).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            if mode == .duration {
                Stepper("\(minutes) minutes", value: $minutes, in: 1...1_440)
            } else {
                DatePicker("Resume no earlier than", selection: $pauseUntil)
            }
            TextField("Reason (optional)", text: $reason)
                .textFieldStyle(.roundedBorder)
            Text("\(reason.unicodeScalars.count)/500 characters")
                .font(.caption)
                .foregroundStyle(reasonIsValid ? Color.secondary : Color.red)
            HStack {
                Spacer()
                Button("Cancel", action: onCancel)
                    .keyboardShortcut(.cancelAction)
                Button("Pause", action: onPause)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!submissionIsValid)
            }
        }
        .padding(24)
        .frame(width: 430)
    }

    private var reasonIsValid: Bool {
        let trimmed = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        return reason.isEmpty || (!trimmed.isEmpty && reason.unicodeScalars.count <= 500)
    }

    private var submissionIsValid: Bool {
        guard reasonIsValid else { return false }
        if mode == .duration { return (1...1_440).contains(minutes) }
        let interval = pauseUntil.timeIntervalSinceNow
        return interval > 0 && interval <= 86_400
    }
}

private struct AuthoritativeExecutionContextMenu: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    let block: ScheduleBlock

    var body: some View {
        Group {
            if block.status == .scheduled {
                Button("Start") { Task { await executionSync.start(block.id) } }
                    .disabled(!canStart)
                Button("Do later") { store.doLater(block.id) }
                    .disabled(!contextIsUnlocked || !store.canMutate(block)
                        || !block.isFlexible || block.isHardConstraint)
                Button("Skip without starting") { store.skip(block.id) }
                    .disabled(!contextIsUnlocked || !store.canMutate(block))
            } else if block.status == .active {
                Menu("Pause") {
                    Button("Indefinitely") {
                        Task { await executionSync.pause(block.id) }
                    }
                    ForEach([5, 15, 30, 60], id: \.self) { minutes in
                        Button("For \(minutes) minutes") {
                            Task {
                                await executionSync.pause(
                                    block.id,
                                    durationSeconds: UInt32(minutes * 60)
                                )
                            }
                        }
                    }
                }
                .disabled(!canControlOpenLease)
            } else if block.status == .paused {
                Button("Resume") { Task { await executionSync.resume(block.id) } }
                    .disabled(!canControlOpenLease)
            }
            if block.status == .active || block.status == .paused {
                Divider()
                Button("Complete") { Task { await executionSync.complete(block.id) } }
                    .disabled(!canControlOpenLease)
                Button("Skip") { Task { await executionSync.skip(block.id) } }
                    .disabled(!canControlOpenLease)
            }
        }
    }

    private var canStart: Bool {
        store.canMutatePlan
            && !executionSync.isSyncing
            && store.canMutate(block)
            && executionSync.activeSession == nil
            && store.executionState.historyVerified
            && store.executionState.pendingCommand == nil
            && store.pendingCanonicalMutations.isEmpty
            && block.sourceItemRevision == block.sourceItemID.flatMap {
                store.canonicalItem(id: $0)?.revision
            }
            && block.sourceItemID.flatMap { store.canonicalItem(id: $0) }?.isExecutable == true
            && !store.executionState.terminalOutcomes.values.contains { outcome in
                outcome.session.itemID == block.sourceItemID
                    && outcome.session.itemRevision == block.sourceItemRevision
                    && outcome.session.occurrenceID == block.occurrenceID
                    && outcome.session.sessionIndex == (block.sessionIndex ?? 0)
            }
    }

    private var canControlOpenLease: Bool {
        guard contextIsUnlocked,
              let active = executionSync.activeSession else { return false }
        return executionSession(active, matches: block)
    }

    private var contextIsUnlocked: Bool {
        store.canMutatePlan
            && !executionSync.isSyncing
            && store.executionState.pendingCommand == nil
    }
}

private struct CanonicalConflictRecoveryControls: View {
    @EnvironmentObject private var store: PlannerStore
    let mutation: PendingCanonicalMutation
    let itemTitle: String
    @State private var errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Button("Retry \(itemTitle) edit on current revision") {
                    errorMessage = nil
                    store.retryConflictedCanonicalMutation(mutation.id)
                }
                .buttonStyle(.link)
                .disabled(!store.canRetryCanonicalMutation(mutation))
                if let sessionID = mutation.executionSessionID {
                    Button("Keep latest canonical item") {
                        do {
                            try store.keepLatestCanonicalItem(forExecutionSession: sessionID)
                            errorMessage = nil
                        } catch {
                            errorMessage = error.localizedDescription
                        }
                    }
                    .buttonStyle(.link)
                    .disabled(!store.canMutatePlan)
                }
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }
        }
    }
}

private struct CanonicalSensitivityConflictRecoveryControls: View {
    @EnvironmentObject private var store: PlannerStore
    let mutation: PendingCanonicalSensitivityMutation

    var body: some View {
        HStack {
            Button("Retry privacy edit on current revision") {
                store.retryConflictedCanonicalSensitivityMutation(mutation.id)
            }
            .buttonStyle(.link)
            .disabled(!store.canEditCanonicalSensitivity(itemID: mutation.itemID))
            Button("Keep latest privacy setting") {
                store.keepLatestCanonicalSensitivity(mutation.id)
            }
            .buttonStyle(.link)
            .disabled(!store.canMutatePlan)
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
                if destinationIsInbox {
                    CanonicalInboxInspector()
                } else if let block = store.selectedBlock {
                    BlockInspector(block: block)
                        .id(block.id)
                } else {
                    ContentUnavailableView("Select an item", systemImage: "sidebar.right")
                }
            } else {
                AssistantView()
            }
        }
        .navigationTitle(inspectorTitle)
    }

    private var destinationIsInbox: Bool {
        (store.destination ?? .today) == .inbox
    }

    private var inspectorTitle: String {
        if tab == 1 { return "Assistant" }
        return destinationIsInbox ? "Inbox Details" : "Inspector"
    }
}

private struct CanonicalInboxInspector: View {
    @EnvironmentObject private var store: PlannerStore

    private var selectedRow: CanonicalInboxPresentation.Row? {
        guard let selectedID = store.selectedCanonicalItemID else { return nil }
        let presentation = CanonicalInboxPresentation.build(
            activeItems: store.canonicalItems,
            pendingMutations: store.pendingCanonicalAuthoringMutations,
            trashEntries: store.canonicalTrash,
            sensitivityPresentation: {
                store.canonicalSensitivityPresentation(itemID: $0)
            }
        )
        return (presentation.conflicts
            + presentation.inbox
            + presentation.planned
            + presentation.trash)
            .first { $0.itemID == selectedID }
    }

    var body: some View {
        if let row = selectedRow {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: kindSymbol(row.kind))
                            .font(.title2)
                            .foregroundStyle(.tint)
                            .frame(width: 42, height: 42)
                            .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row.title)
                                .font(.headline)
                                .textSelection(.enabled)
                                .privacySensitive(row.isSensitive)
                            Text(kindTitle(row.kind))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 0)
                    }

                    InspectorSection(title: "Inbox state") {
                        LabeledContent("Status", value: statusTitle(row.status))
                        LabeledContent("Sync", value: syncTitle(row.syncState))
                        LabeledContent("Source", value: sourceTitle(row.source))
                        LabeledContent(
                            "Privacy",
                            value: privacyTitle(row)
                        )
                    }

                    InspectorSection(title: "Planning") {
                        LabeledContent(
                            "Duration",
                            value: CanonicalItemEditorState.durationDescription(row.durationSeconds)
                        )
                        LabeledContent(
                            "Deadline",
                            value: row.deadlineAt?.formatted(date: .abbreviated, time: .shortened)
                                ?? "None"
                        )
                        LabeledContent("Hierarchy level", value: String(row.depth + 1))
                        if !row.breadcrumb.isEmpty {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Parents")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                Text(row.breadcrumb.joined(separator: " › "))
                                    .font(.subheadline)
                                    .lineLimit(4)
                                    .textSelection(.enabled)
                                    .privacySensitive(row.isSensitive)
                            }
                        }
                    }

                    if row.hasHierarchyCycle || row.hasMissingParent {
                        Label(
                            row.hasHierarchyCycle
                                ? "This hierarchy contains a cycle and is read-only."
                                : "This item's parent is unavailable and it is read-only.",
                            systemImage: "exclamationmark.triangle.fill"
                        )
                        .font(.caption)
                        .foregroundStyle(.orange)
                    }

                    if case let .conflicted(message) = row.syncState {
                        InspectorSection(title: "Conflict review") {
                            Label(
                                message ?? "This saved change differs from canonical state.",
                                systemImage: "arrow.triangle.branch"
                            )
                            .font(.caption)
                            .foregroundStyle(.orange)
                            .textSelection(.enabled)
                            Text(row.source == .activeRestore
                                ? "The main details show the active version. Use Keep Active to discard local restore intent."
                                : "The title above and Notes section show the retained local draft. Use the Inbox row actions to copy it or keep canonical state.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            if row.source == .localCreate || row.source == .pendingReplace {
                                Divider()
                                Text("Latest canonical version")
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(.secondary)
                                if let active = row.activeCanonicalItem {
                                    Text(active.title)
                                        .font(.subheadline.weight(.medium))
                                        .textSelection(.enabled)
                                        .privacySensitive(row.isSensitive || active.isSensitive)
                                    LabeledContent(
                                        "Revision",
                                        value: String(active.revision)
                                    )
                                    if let notes = active.notes, !notes.isEmpty {
                                        Text(String(notes.prefix(2_000)))
                                            .font(.caption)
                                            .textSelection(.enabled)
                                            .privacySensitive(row.isSensitive || active.isSensitive)
                                    }
                                } else {
                                    Label(
                                        "No active canonical item remains; the server state is deleted or unavailable.",
                                        systemImage: "trash"
                                    )
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                }
                            }
                            if let retained = retainedRestoreItem(for: row) {
                                Divider()
                                Text("Retained deleted version")
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(.secondary)
                                Text(retained.title)
                                    .font(.subheadline.weight(.medium))
                                    .textSelection(.enabled)
                                    .privacySensitive(row.isSensitive)
                                if let notes = retained.notes, !notes.isEmpty {
                                    Text(String(notes.prefix(2_000)))
                                        .font(.caption)
                                        .textSelection(.enabled)
                                        .privacySensitive(row.isSensitive)
                                }
                            }
                        }
                    }

                    if let notes = notes(for: row), !notes.isEmpty {
                        InspectorSection(title: "Notes") {
                            Text(String(notes.prefix(2_000)))
                                .font(.subheadline)
                                .textSelection(.enabled)
                                .privacySensitive(row.isSensitive)
                            if notes.count > 2_000 {
                                Text("Open the item editor to read the remaining notes.")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }

                    Label(
                        usesSensitivePresentation(row)
                            ? "Sensitive content is shown only while DayWeave is unlocked."
                            : "Canonical details are stored in the encrypted local plan.",
                        systemImage: usesSensitivePresentation(row) ? "lock.shield.fill" : "lock"
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
                .padding(20)
            }
            .id(row.id)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(row.accessibilitySummary)
            .accessibilityIdentifier("canonical-inbox.inspector")
        } else {
            ContentUnavailableView {
                Label("Select an Inbox item", systemImage: "tray.full")
            } description: {
                Text("Choose a captured, planned, conflicted, or recently deleted item to inspect it here.")
            }
            .accessibilityIdentifier("canonical-inbox.inspector.empty")
        }
    }

    private func notes(for row: CanonicalInboxPresentation.Row) -> String? {
        let mutation = row.mutationID.flatMap { store.canonicalAuthoringMutation(id: $0) }
        if row.source == .activeRestore {
            return store.canonicalItems.first(where: { $0.id == row.itemID })?.notes
        }
        return mutation?.draft?.notes
            ?? mutation?.baseItem?.notes
            ?? store.canonicalItems.first(where: { $0.id == row.itemID })?.notes
            ?? store.canonicalTrash.first(where: { $0.id == row.itemID })?.lastKnownItem?.notes
    }

    private func retainedRestoreItem(
        for row: CanonicalInboxPresentation.Row
    ) -> DayWeaveCanonicalItem? {
        guard row.source == .activeRestore, let mutationID = row.mutationID else { return nil }
        return store.canonicalAuthoringMutation(id: mutationID)?.baseItem
    }

    private func kindTitle(_ kind: DayWeaveCanonicalItemKind) -> String {
        switch kind {
        case .breakTime: "Break"
        default: kind.wireValue.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    private func kindSymbol(_ kind: DayWeaveCanonicalItemKind) -> String {
        switch kind {
        case .event: "calendar"
        case .task: "checkmark.circle"
        case .habit: "repeat"
        case .routine: "list.number"
        case .goal: "target"
        case .breakTime: "cup.and.saucer"
        case .unknown: "questionmark.diamond"
        }
    }

    private func statusTitle(_ status: DayWeaveCanonicalItemStatus) -> String {
        status.wireValue.replacingOccurrences(of: "_", with: " ").capitalized
    }

    private func syncTitle(_ state: CanonicalInboxPresentation.Row.SyncState) -> String {
        switch state {
        case .synced: "Synced"
        case .waiting: "Queued locally"
        case .submitted: "Submitted; recovering"
        case .conflicted: "Needs review"
        }
    }

    private func sourceTitle(_ source: CanonicalInboxPresentation.Row.Source) -> String {
        switch source {
        case .canonical: "Canonical"
        case .localCreate: "Local capture"
        case .pendingReplace: "Queued edit"
        case .pendingTrash: "Queued deletion"
        case .pendingRestore: "Queued restore"
        case .activeRestore: "Restore conflict"
        case .recentTrash: "Recently deleted"
        }
    }

    private func privacyTitle(_ row: CanonicalInboxPresentation.Row) -> String {
        switch row.sensitivityPresentation {
        case .standard: return "Standard marker"
        case .own: return "Sensitive marker"
        case .inherited: return "Inherited sensitive"
        }
    }

    private func usesSensitivePresentation(_ row: CanonicalInboxPresentation.Row) -> Bool {
        row.isSensitive
    }
}

private struct BlockInspector: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock
    @State private var recoveryTitle: String
    @State private var recoveryIsSensitive: Bool

    init(block: ScheduleBlock) {
        self.block = block
        _recoveryTitle = State(initialValue: block.title)
        _recoveryIsSensitive = State(initialValue: block.isSensitive)
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
                        LabeledContent(
                            "Privacy",
                            value: sensitivityDescription(itemID: itemID)
                        )
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
                        CanonicalConflictRecoveryControls(
                            mutation: mutation,
                            itemTitle: block.title
                        )
                        Text("This keeps the requested status, rebases it onto the currently cached revision, and leaves it pending until the next sync.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if let itemID = block.sourceItemID,
                   let item = store.canonicalItem(id: itemID) {
                    InspectorSection(title: "Privacy") {
                        CanonicalSensitivityEditor(item: item)
                    }
                }

                if block.isLocallyAuthored, block.sourceItemID == nil {
                    InspectorSection(title: "Local capture recovery") {
                        TextField("Title", text: $recoveryTitle)
                            .textFieldStyle(.roundedBorder)
                        Text("\(recoveryTitle.unicodeScalars.count)/\(PlannerStore.maximumCanonicalTitleScalars) Unicode characters")
                            .font(.caption)
                            .foregroundStyle(localCaptureTitleIsValid ? Color.secondary : Color.red)
                        Toggle(isOn: $recoveryIsSensitive) {
                            Label("Sensitive", systemImage: "checkmark.shield")
                        }
                        Text("Sensitive captures are published with their privacy marker and omitted from Codex context except for an anonymous busy span.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if let diagnostic = store.localCaptureDiagnostics[block.id] {
                            Text(diagnostic)
                                .font(.caption)
                                .foregroundStyle(.orange)
                        }
                        HStack {
                            Button("Save title") {
                                _ = store.updateLocalCapture(
                                    block.id,
                                    title: recoveryTitle,
                                    isSensitive: recoveryIsSensitive
                                )
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

                if block.sourceItemID != nil {
                    AuthoritativeExecutionControls(block: block)
                } else {
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
            }
            .padding(18)
        }
    }

    private var localCaptureTitleIsValid: Bool {
        PlannerStore.normalizedCanonicalTitle(recoveryTitle) != nil
    }

    private func sensitivityDescription(itemID: UUID) -> String {
        let current = switch store.canonicalSensitivityPresentation(itemID: itemID) {
        case .standard: "Standard"
        case .own: "Sensitive on this item"
        case .inherited: "Sensitive from an ancestor"
        }
        guard let mutation = store.canonicalSensitivityMutation(itemID: itemID) else {
            return current
        }
        let requested = mutation.requestedIsSensitive ? "mark pending" : "removal pending"
        return "\(current) · \(requested)"
    }

    private func splitDescription(_ policy: DayWeaveSplitPolicy) -> String {
        switch policy {
        case .indivisible: "Indivisible"
        case let .splittable(minimum, maximum): "\(minimum / 60)–\(maximum / 60) minute sessions"
        case .unknown: "Unsupported — read only"
        }
    }
}

private struct CanonicalSensitivityEditor: View {
    @EnvironmentObject private var store: PlannerStore
    let item: DayWeaveCanonicalItem
    @State private var desiredIsSensitive: Bool

    init(item: DayWeaveCanonicalItem) {
        self.item = item
        _desiredIsSensitive = State(initialValue: item.isSensitive)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(currentLabel, systemImage: currentSymbol)
                .foregroundStyle(currentColor)

            Toggle("Mark this item sensitive", isOn: $desiredIsSensitive)
                .disabled(!store.canEditCanonicalSensitivity(itemID: item.id))

            Text(helpText)
                .font(.caption)
                .foregroundStyle(.secondary)

            if let mutation {
                Label(
                    mutationDescription(mutation),
                    systemImage: mutation.disposition == .conflicted
                        ? "exclamationmark.triangle.fill"
                        : "arrow.triangle.2.circlepath"
                )
                .font(.caption)
                .foregroundStyle(mutation.disposition == .conflicted ? Color.orange : Color.secondary)

                if mutation.disposition == .conflicted {
                    if let diagnostic = mutation.diagnostic {
                        Text(diagnostic)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    HStack {
                        Button("Retry on latest revision") {
                            store.retryConflictedCanonicalSensitivityMutation(mutation.id)
                        }
                        .disabled(!store.canEditCanonicalSensitivity(itemID: item.id))
                        Button("Keep latest") {
                            store.keepLatestCanonicalSensitivity(mutation.id)
                        }
                    }
                    .controlSize(.small)
                }
            }

            Button(mutation == nil ? "Save privacy change" : "Update privacy change") {
                _ = store.setCanonicalItemSensitivity(
                    item.id,
                    isSensitive: desiredIsSensitive
                )
            }
            .disabled(!canSave)
        }
        .onAppear { refreshDraft() }
        .onChange(of: item.revision) { _, _ in refreshDraft() }
        .onChange(of: mutation) { _, _ in refreshDraft() }
    }

    private var mutation: PendingCanonicalSensitivityMutation? {
        store.canonicalSensitivityMutation(itemID: item.id)
    }

    private var currentLabel: String {
        switch store.canonicalSensitivityPresentation(itemID: item.id) {
        case .standard: "Standard privacy"
        case .own: "Sensitive on this item"
        case .inherited: "Sensitive through an ancestor"
        }
    }

    private var currentSymbol: String {
        switch store.canonicalSensitivityPresentation(itemID: item.id) {
        case .standard: "shield"
        case .own: "checkmark.shield.fill"
        case .inherited: "arrow.triangle.branch"
        }
    }

    private var currentColor: Color {
        store.canonicalSensitivityPresentation(itemID: item.id) == .standard
            ? .secondary
            : .purple
    }

    private var helpText: String {
        if store.canonicalSensitivityPresentation(itemID: item.id) == .inherited {
            return "This item stays effectively sensitive while any ancestor is sensitive. Turning off its own marker cannot override that hierarchy."
        }
        if !item.supportsLosslessReplacement {
            return "This item contains fields that this build cannot round-trip losslessly, so its privacy marker is read-only."
        }
        return "The change is encrypted locally, then sent as a revision-guarded full replacement. Sensitive content is excluded from Codex context."
    }

    private var canSave: Bool {
        store.canEditCanonicalSensitivity(itemID: item.id)
            && desiredIsSensitive != (mutation?.requestedIsSensitive ?? item.isSensitive)
    }

    private func refreshDraft() {
        desiredIsSensitive = mutation?.requestedIsSensitive ?? item.isSensitive
    }

    private func mutationDescription(
        _ mutation: PendingCanonicalSensitivityMutation
    ) -> String {
        if let followUp = mutation.followUpIsSensitive {
            return followUp
                ? "A submitted privacy change will be reconciled, then the item will be marked sensitive."
                : "A submitted privacy change will be reconciled before the final privacy removal."
        }
        return mutation.desiredIsSensitive
            ? "A privacy mark is waiting for sync."
            : "A privacy removal is waiting for sync; content remains redacted until confirmed."
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
    @EnvironmentObject private var conversation: CodexConversationController
    @State private var draft = ""

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 7) {
                Circle()
                    .fill(codexStatusColor)
                    .frame(width: 7, height: 7)
                Text(codex.state.title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                switch codex.state {
                case .signedOut:
                    Button("Sign in") { codex.signInWithDeviceCode() }
                        .buttonStyle(.link)
                        .font(.caption)
                case .signingIn where codex.verificationURL != nil:
                    Button("Open sign-in") { codex.openVerificationPage() }
                        .buttonStyle(.link)
                        .font(.caption)
                case .unavailable:
                    Button("Retry") { codex.retry() }
                        .buttonStyle(.link)
                        .font(.caption)
                default:
                    EmptyView()
                }
                if case .signedIn = codex.state, conversation.isTurnActive {
                    Button("Stop") { conversation.stopResponse() }
                        .buttonStyle(.link)
                        .font(.caption)
                        .disabled(conversation.activity == .stopping)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            Divider()

            if conversation.messages.isEmpty {
                assistantEmptyState
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(conversation.messages) { message in
                                AssistantBubble(message: message)
                                    .id(message.id)
                            }
                        }
                        .padding(16)
                    }
                    .onChange(of: conversation.messages) {
                        if let last = conversation.messages.last {
                            withAnimation(.easeOut(duration: 0.16)) {
                                proxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                }
            }

            if let progress = conversation.progressText, !progress.isEmpty {
                HStack(spacing: 7) {
                    ProgressView().controlSize(.mini)
                    Text(progress)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                    Spacer()
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .background(Color(nsColor: .controlBackgroundColor))
            }

            if case let .failed(message) = conversation.activity {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 7)
                    .background(Color.red.opacity(0.08))
            } else if conversation.lastProposalCount > 0 {
                Button {
                    store.destination = .inbox
                    store.selectCanonicalItem(nil)
                    NotificationCenter.default.post(name: .dayWeaveShowSuggestionsInbox, object: nil)
                    Task { @MainActor in
                        await Task.yield()
                        NotificationCenter.default.post(
                            name: .dayWeaveShowSuggestionsInbox,
                            object: nil
                        )
                    }
                } label: {
                    Label(
                        "\(conversation.lastProposalCount) proposal\(conversation.lastProposalCount == 1 ? "" : "s") sent to Inbox for review",
                        systemImage: "tray.and.arrow.down.fill"
                    )
                    .font(.caption)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.purple)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .background(Color.purple.opacity(0.08))
            }

            Divider()
            HStack(alignment: .bottom, spacing: 8) {
                TextField(composerPlaceholder, text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...5)
                    .onSubmit(send)
                    .disabled(!isSignedIn || conversation.activity.isBusy)
                Button(action: primaryComposerAction) {
                    Image(systemName: conversation.isTurnActive ? "stop.circle.fill" : "arrow.up.circle.fill")
                        .font(.title2)
                }
                .buttonStyle(.plain)
                .disabled(conversation.isTurnActive ? conversation.activity == .stopping : !canSend)
                .help(conversation.isTurnActive ? "Stop response" : "Send to Codex")
            }
            .padding(12)
        }
    }

    private func send() {
        guard canSend else { return }
        conversation.send(draft)
        draft = ""
    }

    private func primaryComposerAction() {
        if conversation.isTurnActive {
            conversation.stopResponse()
        } else {
            send()
        }
    }

    @ViewBuilder
    private var assistantEmptyState: some View {
        switch codex.state {
        case .signedIn:
            ContentUnavailableView {
                Label("Plan with Codex", systemImage: "sparkles")
            } description: {
                Text("Ask about today, tradeoffs, habits, or deadlines. Codex receives a redacted, read-only planner snapshot and can only propose changes for your approval.")
            }
        case .signedOut:
            ContentUnavailableView {
                Label("Connect ChatGPT", systemImage: "person.crop.circle.badge.plus")
            } description: {
                Text("Use device-code sign-in to start a private in-app planning conversation.")
            } actions: {
                Button("Sign in") { codex.signInWithDeviceCode() }
                    .buttonStyle(.borderedProminent)
            }
        case .signingIn, .cancellingSignIn:
            ContentUnavailableView {
                Label("Finish ChatGPT sign-in", systemImage: "hourglass")
            } description: {
                Text("Return here after completing the device-code ceremony.")
            }
        case let .unavailable(message):
            ContentUnavailableView {
                Label("Codex is offline", systemImage: "bolt.slash")
            } description: {
                Text(message)
            } actions: {
                Button("Retry") { codex.retry() }
            }
        case .starting:
            ContentUnavailableView {
                Label("Starting Codex", systemImage: "ellipsis")
            } description: {
                Text("Verifying the contained runtime and checking ChatGPT sign-in.")
            }
        case .stopped:
            ContentUnavailableView("Codex is stopped", systemImage: "stop.circle")
        }
    }

    private var canSend: Bool {
        isSignedIn
            && !conversation.activity.isBusy
            && !conversation.isTurnActive
            && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && draft.utf8.count <= CodexPlannerContextSerializer.maximumUserMessageBytes
    }

    private var isSignedIn: Bool {
        if case .signedIn = codex.state { return true }
        return false
    }

    private var composerPlaceholder: String {
        if !isSignedIn { return "Sign in to chat with Codex…" }
        if conversation.activity.isBusy { return "Codex is responding…" }
        return "Ask about your plan…"
    }

    private var codexStatusColor: Color {
        switch codex.state {
        case .signedIn: .green
        case .starting, .signingIn, .cancellingSignIn: .orange
        case .unavailable: .red
        case .stopped, .signedOut: .secondary
        }
    }
}

private struct AssistantBubble: View {
    let message: CodexConversationMessage

    var body: some View {
        VStack(alignment: message.role == .assistant ? .leading : .trailing, spacing: 5) {
            if message.text.isEmpty && message.delivery == .streaming {
                ProgressView().controlSize(.small)
                    .padding(.horizontal, 5)
            } else {
                Text(message.text)
                    .font(.subheadline)
                    .textSelection(.enabled)
            }
            if message.delivery == .interrupted {
                Label("Stopped", systemImage: "stop.fill")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else if message.delivery == .failed {
                Label("Not delivered", systemImage: "exclamationmark.circle")
                    .font(.caption2)
                    .foregroundStyle(.red)
            }
        }
        .padding(11)
        .background(
            message.role == .assistant
                ? Color(nsColor: .controlBackgroundColor)
                : Color.accentColor.opacity(0.16),
            in: RoundedRectangle(cornerRadius: 12)
        )
        .frame(maxWidth: .infinity, alignment: message.role == .assistant ? .leading : .trailing)
    }
}

private struct UnifiedInboxView: View {
    private enum Section: String, CaseIterable, Identifiable {
        case items
        case suggestions

        var id: Self { self }

        var title: String {
            switch self {
            case .items: "Items"
            case .suggestions: "Suggestions"
            }
        }
    }

    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @State private var section: Section = .items

    private var canonicalPresentation: CanonicalInboxPresentation {
        CanonicalInboxPresentation.build(
            activeItems: store.canonicalItems,
            pendingMutations: store.pendingCanonicalAuthoringMutations,
            trashEntries: store.canonicalTrash,
            sensitivityPresentation: {
                store.canonicalSensitivityPresentation(itemID: $0)
            }
        )
    }

    private var capturedItemCount: Int {
        let presentation = canonicalPresentation
        return Set(
            (presentation.inbox + presentation.planned + presentation.trash).map(\.itemID)
        ).count
    }

    private var suggestionCount: Int {
        store.suggestions.count(where: { $0.state == .pending })
            + suggestionSync.proposals.count
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .center, spacing: 18) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Inbox")
                        .font(.title2.weight(.semibold))
                    Text(sectionDescription)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }

                Spacer(minLength: 16)

                Picker("Inbox section", selection: $section) {
                    Text("Items · \(capturedItemCount)").tag(Section.items)
                    Text("Suggestions · \(suggestionCount)").tag(Section.suggestions)
                }
                .pickerStyle(.segmented)
                .frame(width: 260)
                .accessibilityIdentifier("inbox.section-picker")

                if section == .suggestions {
                    Button {
                        section = .items
                        store.isQuickAddPresented = true
                    } label: {
                        Label("Quick Capture", systemImage: "plus")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!store.canMutatePlan)
                    .help("Capture an encrypted canonical Inbox item")
                    .accessibilityIdentifier("inbox.quick-capture")
                }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            .background(.bar)

            Divider()

            switch section {
            case .items:
                CanonicalCapturedInboxView()
            case .suggestions:
                SuggestionsInboxView()
            }
        }
        .navigationTitle("Inbox")
        .onChange(of: section) { _, value in
            if value == .suggestions {
                store.selectCanonicalItem(nil)
            }
        }
        .onReceive(NotificationCenter.default.publisher(
            for: .dayWeaveShowSuggestionsInbox
        )) { _ in
            section = .suggestions
            store.selectCanonicalItem(nil)
        }
        .accessibilityIdentifier("unified-inbox")
    }

    private var sectionDescription: String {
        switch section {
        case .items:
            "Capture and prepare your own work before it becomes schedulable."
        case .suggestions:
            "Review local and external proposals before any canonical change is applied."
        }
    }
}

private struct SuggestionsInboxView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @EnvironmentObject private var proposalApplications: ProposalApplicationStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var serviceCoordinator: DayWeaveServiceCoordinator
    @State private var proposalBeingEdited: DayWeaveProposal?
    @State private var proposalBeingReviewed: DayWeaveProposal?

    private var pendingLocalSuggestions: [PlanningSuggestion] {
        store.suggestions.filter { $0.state == .pending }
    }

    var body: some View {
        List {
            Section {
                Label {
                    Text("External tools can only submit proposals. Executable changes are simulated first, require your explicit approval, and apply atomically from this device.")
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
                            || proposalApplications.hasPendingRecovery
                    )
                }

                HStack(spacing: 8) {
                    Image(systemName: proposalApplications.hasPendingRecovery
                        ? "arrow.clockwise.circle.fill"
                        : "checkmark.shield")
                        .foregroundStyle(proposalApplications.status.isFailure ? .red : .secondary)
                    Text(proposalApplications.status.message)
                        .font(.caption)
                        .foregroundStyle(proposalApplications.status.isFailure ? .red : .secondary)
                    Spacer()
                    if proposalApplications.status.isWorking {
                        ProgressView().controlSize(.small)
                    }
                    if proposalApplications.hasPendingRecovery {
                        Button("Recover safely") {
                            Task {
                                await serviceCoordinator.recoverPendingProposalAndResume()
                            }
                        }
                        .controlSize(.small)
                        .disabled(proposalApplications.status.isWorking)
                    }
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
                            isWorking: suggestionSync.activeProposalIDs.contains(proposal.id)
                                || proposalApplications.activeProposalID == proposal.id
                                || proposalApplications.hasPendingRecovery,
                            edit: { proposalBeingEdited = proposal },
                            review: { proposalBeingReviewed = proposal }
                        )
                    }
                }
            }

            if !proposalApplications.recentReceipts.isEmpty {
                Section("Recent applications") {
                    ForEach(Array(proposalApplications.recentReceipts.prefix(5))) { receipt in
                        ProposalApplicationReceiptRow(storedReceipt: receipt)
                    }
                }
            }
        }
        .navigationTitle("Suggestions")
        .task {
            guard suggestionSync.isConfigured,
                  !proposalApplications.hasPendingRecovery else { return }
            await suggestionSync.refresh()
        }
        .sheet(item: $proposalBeingEdited) { proposal in
            EditRemoteSuggestionView(proposal: proposal)
                .environmentObject(suggestionSync)
                .environmentObject(proposalApplications)
        }
        .sheet(item: $proposalBeingReviewed) { proposal in
            ProposalApplicationReviewView(proposal: proposal)
                .environmentObject(proposalApplications)
                .environmentObject(canonicalSync)
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
    let review: () -> Void

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

            if proposal.advertisesApplicationReadyChangeSet {
                Label("Executable typed changes · review and device approval required", systemImage: "checkmark.shield")
                    .font(.caption)
                    .foregroundStyle(.blue)
            } else if proposal.advertisesReservedChangeSetSchema {
                Label("Protected change-set version requires a newer DayWeave build", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
            } else {
                Label("Advisory proposal · accepting records a decision without changing items", systemImage: "text.bubble")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            HStack {
                if proposal.advertisesApplicationReadyChangeSet {
                    Button("Review changes…", action: review)
                        .buttonStyle(.borderedProminent)
                } else if !proposal.advertisesReservedChangeSetSchema {
                    Button("Accept advisory") {
                        Task { await suggestionSync.accept(proposal) }
                    }
                    .buttonStyle(.borderedProminent)
                }
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
    @EnvironmentObject private var proposalApplications: ProposalApplicationStore

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
                            proposalApplications.discardPreview(for: proposal.id)
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

private struct ProposalApplicationReviewView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var proposalApplications: ProposalApplicationStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

    let proposal: DayWeaveProposal
    @State private var approvedReview: DayWeaveProposalReviewApproval?
    @State private var isRefreshingCanonicalState = false

    private var review: DayWeaveProposalApplicationPreview? {
        proposalApplications.preview(for: proposal)
    }

    private var storedReceipt: DayWeaveStoredProposalApplicationReceipt? {
        proposalApplications.recentReceipts.first {
            $0.application.contains(proposalID: proposal.id)
        }
    }

    private var currentApproval: DayWeaveProposalReviewApproval? {
        proposalApplications.approval(for: proposal)
    }

    private var approvalBinding: Binding<Bool> {
        Binding(
            get: { approvedReview != nil && approvedReview == currentApproval },
            set: { isApproved in
                approvedReview = isApproved ? currentApproval : nil
            }
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .top, spacing: 16) {
                Image(systemName: "checkmark.shield.fill")
                    .font(.title2)
                    .foregroundStyle(.blue)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Review proposed changes")
                        .font(.title2.weight(.semibold))
                    Text(proposal.title)
                        .font(.headline)
                    Text("The simulation is read-only. Apply uses the exact review hash and either commits every change or none.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { dismiss() }
            }
            .padding(24)

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if let storedReceipt {
                        ProposalApplicationReceiptRow(storedReceipt: storedReceipt)
                            .padding(.vertical, 4)
                    } else if let review {
                        reviewContent(review)
                    } else {
                        HStack(spacing: 12) {
                            if proposalApplications.status.isWorking {
                                ProgressView()
                            } else {
                                Image(systemName: proposalApplications.status.isFailure
                                    ? "exclamationmark.triangle.fill"
                                    : "shield")
                                    .foregroundStyle(proposalApplications.status.isFailure ? .red : .secondary)
                            }
                            Text(proposalApplications.status.message)
                                .foregroundStyle(proposalApplications.status.isFailure ? .red : .secondary)
                            Spacer()
                            if !proposalApplications.status.isWorking {
                                Button(proposalApplications.status.isFailure
                                    ? "Try review again" : "Generate review") {
                                    Task { await proposalApplications.prepareReview(for: proposal) }
                                }
                                .disabled(proposalApplications.status.isWorking)
                            }
                        }
                        .padding(18)
                        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
                    }
                }
                .padding(24)
            }
        }
        .frame(minWidth: 720, idealWidth: 820, minHeight: 620, idealHeight: 760)
        .task(id: proposal.revision) {
            guard storedReceipt == nil, review == nil else { return }
            await proposalApplications.prepareReview(for: proposal)
        }
    }

    @ViewBuilder
    private func reviewContent(_ review: DayWeaveProposalApplicationPreview) -> some View {
        HStack(spacing: 18) {
            Label(review.maximumRisk.capitalized + " risk", systemImage: "gauge.with.dots.needle.50percent")
                .foregroundStyle(proposalRiskColor(review.maximumRisk))
            Label(
                "Expires \(review.expiresAt.formatted(date: .omitted, time: .shortened))",
                systemImage: "clock"
            )
            Label(
                "\(review.commandIDs.count) atomic change\(review.commandIDs.count == 1 ? "" : "s")",
                systemImage: "square.stack.3d.up"
            )
            Spacer()
            Button("Refresh simulation") {
                approvedReview = nil
                Task { await proposalApplications.prepareReview(for: proposal) }
            }
            .disabled(proposalApplications.status.isWorking)
        }
        .font(.subheadline)

        if !review.conflicts.isEmpty {
            GroupBox {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(review.conflicts) { conflict in
                        Label {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(proposalFieldLabel(conflict.code)).fontWeight(.medium)
                                Text(conflict.summary).foregroundStyle(.secondary)
                            }
                        } icon: {
                            Image(systemName: "exclamationmark.octagon.fill")
                                .foregroundStyle(.red)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } label: {
                Text("Blocking conflicts")
                    .font(.headline)
            }
        }

        GroupBox {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(review.diffs) { diff in
                    ProposalItemDiffCard(diff: diff)
                    if diff.id != review.diffs.last?.id { Divider() }
                }
                if review.diffs.isEmpty {
                    Text("No direct item diff could be produced because the simulation is blocked.")
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Text("Direct changes")
                .font(.headline)
        }

        if !review.implicitDiffs.isEmpty {
            GroupBox {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(review.implicitDiffs) { diff in
                        ProposalImplicitItemDiffCard(diff: diff)
                        if diff.id != review.implicitDiffs.last?.id { Divider() }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } label: {
                Text("Implicit hierarchy changes")
                    .font(.headline)
            }
        }

        GroupBox {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(review.risks) { risk in
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: risk.requiresExplicitApproval
                            ? "exclamationmark.shield.fill"
                            : "info.circle.fill")
                            .foregroundStyle(proposalRiskColor(risk.level))
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(proposalFieldLabel(risk.code)) · \(risk.level.capitalized)")
                                .fontWeight(.medium)
                            Text(risk.summary)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } label: {
            Text("Risks and reversibility")
                .font(.headline)
        }

        VStack(alignment: .leading, spacing: 12) {
            Toggle(isOn: approvalBinding) {
                Text("I reviewed the direct changes, hierarchy side effects, risks, and conflicts.")
            }
            .toggleStyle(.checkbox)

            HStack {
                Text(proposalApplications.status.message)
                    .font(.caption)
                    .foregroundStyle(proposalApplications.status.isFailure ? .red : .secondary)
                Spacer()
                if proposalApplications.status.isWorking || isRefreshingCanonicalState {
                    ProgressView().controlSize(.small)
                }
                Button("Apply exact changes") {
                    Task {
                        let applied = await proposalApplications.apply(
                            proposal,
                            approval: approvedReview
                        )
                        if applied { approvedReview = nil }
                        if applied, canonicalSync.isConfigured {
                            isRefreshingCanonicalState = true
                            await canonicalSync.sync()
                            isRefreshingCanonicalState = false
                        }
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(
                    approvedReview == nil
                        || approvedReview != currentApproval
                        || !review.canApply
                        || proposalApplications.status.isWorking
                        || isRefreshingCanonicalState
                )
            }
        }
        .padding(16)
        .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
    }
}

private struct ProposalItemDiffCard: View {
    let diff: DayWeaveProposalItemDiff
    @State private var revealsSensitiveValues = false

    private var containsSensitiveValues: Bool {
        diff.before?.isSensitive == true || diff.after?.isSensitive == true
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label(proposalFieldLabel(diff.operation), systemImage: operationSymbol)
                    .font(.headline)
                Spacer()
                Text("Item \(diff.itemID.uuidString.prefix(8))")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }

            HStack(alignment: .top, spacing: 12) {
                ProposalItemSnapshotView(
                    label: "Before",
                    item: diff.before,
                    hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
                )
                Image(systemName: "arrow.right")
                    .foregroundStyle(.secondary)
                    .padding(.top, 22)
                ProposalItemSnapshotView(
                    label: "After",
                    item: diff.after,
                    hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
                )
            }

            if containsSensitiveValues {
                Toggle("Reveal sensitive before/after values", isOn: $revealsSensitiveValues)
                    .toggleStyle(.checkbox)
                    .font(.caption)
            }

            ProposalChangedValuesView(
                fields: diff.changedFields,
                before: diff.before,
                after: diff.after,
                hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
            )
        }
    }

    private var operationSymbol: String {
        switch diff.operation {
        case "create_item": "plus.circle.fill"
        case "replace_item": "pencil.circle.fill"
        case "trash_item": "trash.circle.fill"
        case "restore_item": "arrow.uturn.backward.circle.fill"
        default: "questionmark.circle"
        }
    }
}

private struct ProposalImplicitItemDiffCard: View {
    let diff: DayWeaveProposalImplicitItemDiff
    @State private var revealsSensitiveValues = false

    private var containsSensitiveValues: Bool {
        diff.before.isSensitive || diff.after.isSensitive
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Hierarchy side effect", systemImage: "arrow.triangle.branch")
                    .font(.headline)
                Spacer()
                Text("Item \(diff.itemID.uuidString.prefix(8))")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            HStack(alignment: .top, spacing: 12) {
                ProposalItemSnapshotView(
                    label: "Before",
                    item: diff.before,
                    hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
                )
                Image(systemName: "arrow.right")
                    .foregroundStyle(.secondary)
                    .padding(.top, 22)
                ProposalItemSnapshotView(
                    label: "After",
                    item: diff.after,
                    hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
                )
            }
            if containsSensitiveValues {
                Toggle("Reveal sensitive before/after values", isOn: $revealsSensitiveValues)
                    .toggleStyle(.checkbox)
                    .font(.caption)
            }
            ProposalChangedValuesView(
                fields: diff.changedFields,
                before: diff.before,
                after: diff.after,
                hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
            )
        }
    }
}

private struct ProposalItemSnapshotView: View {
    let label: String
    let item: DayWeaveCanonicalItem?
    let hidesSensitiveContent: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            if let item {
                HStack(spacing: 5) {
                    if item.isSensitive {
                        Image(systemName: "lock.fill").foregroundStyle(.orange)
                    }
                    Text(proposalItemSnapshotTitle(
                        item,
                        hidesSensitiveContent: hidesSensitiveContent
                    ))
                        .fontWeight(.medium)
                }
                Text("\(proposalFieldLabel(item.kind.wireValue)) · \(proposalFieldLabel(item.status.wireValue)) · revision \(item.revision)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let duration = item.durationSeconds {
                    Text("\(duration / 60) min · importance \(item.importance) · urgency \(item.urgency)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else {
                Text("None")
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
    }
}

func proposalItemSnapshotTitle(
    _ item: DayWeaveCanonicalItem,
    hidesSensitiveContent: Bool
) -> String {
    hidesSensitiveContent ? "Sensitive item" : item.title
}

private struct ProposalChangedValuesView: View {
    let fields: [String]
    let before: DayWeaveCanonicalItem?
    let after: DayWeaveCanonicalItem?
    let hidesSensitiveContent: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text("Exact changed values")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            ForEach(fields, id: \.self) { field in
                VStack(alignment: .leading, spacing: 4) {
                    Text(proposalFieldLabel(field))
                        .font(.caption.weight(.semibold))
                    HStack(alignment: .top, spacing: 8) {
                        proposalValueCell(
                            label: "Before",
                            value: proposalItemFieldValue(
                                field,
                                item: before,
                                hidesSensitiveContent: hidesSensitiveContent
                            )
                        )
                        Image(systemName: "arrow.right")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.top, 13)
                        proposalValueCell(
                            label: "After",
                            value: proposalItemFieldValue(
                                field,
                                item: after,
                                hidesSensitiveContent: hidesSensitiveContent
                            )
                        )
                    }
                }
            }
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
    }

    private func proposalValueCell(label: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased())
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private func proposalItemFieldValue(
    _ field: String,
    item: DayWeaveCanonicalItem?,
    hidesSensitiveContent: Bool
) -> String {
    guard let item else { return "Item absent" }
    if hidesSensitiveContent,
       ["title", "notes", "recurrence", "flexible_constraints"].contains(field) {
        return "Hidden — use Reveal sensitive before/after values"
    }
    return switch field {
    case "is_sensitive": item.isSensitive ? "Yes" : "No"
    case "kind": item.kind.wireValue
    case "status": item.status.wireValue
    case "title": item.title
    case "notes": item.notes ?? "None"
    case "timezone_name": item.timezoneName
    case "duration_seconds": item.durationSeconds.map { "\($0) seconds" } ?? "None"
    case "deadline_at": proposalDateValue(item.deadlineAt)
    case "earliest_start_at": proposalDateValue(item.earliestStartAt)
    case "recurrence": item.recurrence.map(proposalJSONValue) ?? "None"
    case "flexible_constraints": proposalJSONValue(item.flexibleConstraints)
    case "split_policy": proposalSplitPolicyValue(item.splitPolicy)
    case "importance": String(item.importance)
    case "urgency": String(item.urgency)
    case "parent_id": item.parentID?.uuidString.lowercased() ?? "None"
    case "sibling_order": String(item.siblingOrder)
    case "is_executable": item.isExecutable ? "Yes" : "No"
    case "revision": String(item.revision)
    case "completed_at": proposalDateValue(item.completedAt)
    case "deleted_at": proposalDateValue(item.deletedAt)
    default: "Unsupported field"
    }
}

private func proposalDateValue(_ date: Date?) -> String {
    guard let date else { return "None" }
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return formatter.string(from: date)
}

private func proposalJSONValue(_ value: JSONValue) -> String {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(value),
          let string = String(data: data, encoding: .utf8) else {
        return "Unsupported JSON value"
    }
    return string
}

private func proposalSplitPolicyValue(_ policy: DayWeaveSplitPolicy) -> String {
    return switch policy {
    case .indivisible:
        "Indivisible"
    case let .splittable(minimum, maximum):
        "Splittable · minimum \(minimum) seconds · maximum \(maximum) seconds"
    case let .unknown(raw):
        proposalJSONValue(.object(raw))
    }
}

private struct ProposalApplicationReceiptRow: View {
    @EnvironmentObject private var proposalApplications: ProposalApplicationStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

    let storedReceipt: DayWeaveStoredProposalApplicationReceipt
    @State private var isRefreshingCanonicalState = false

    private var receipt: DayWeaveProposalApplicationReceipt {
        storedReceipt.application
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: receipt.status == .applied
                    ? "checkmark.seal.fill"
                    : "arrow.uturn.backward.circle.fill")
                    .foregroundStyle(receipt.status == .applied ? .green : .blue)
                Text(receipt.status == .applied ? "Changes applied" : "Application undone")
                    .font(.headline)
                Spacer()
                Text(receipt.appliedAt.formatted(date: .abbreviated, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            HStack(spacing: 14) {
                Label("\(receipt.proposals.count) proposal\(receipt.proposals.count == 1 ? "" : "s")", systemImage: "sparkles")
                Label("\(receipt.commandIDs.count) command\(receipt.commandIDs.count == 1 ? "" : "s")", systemImage: "square.stack.3d.up")
                Label("\(receipt.affectedItemIDs.count) affected item\(receipt.affectedItemIDs.count == 1 ? "" : "s")", systemImage: "list.bullet.rectangle")
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            if receipt.status == .applied {
                HStack {
                    Text("Undo available until \(receipt.undoExpiresAt.formatted(date: .abbreviated, time: .shortened)); later item changes can make undo unsafe.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    if proposalApplications.status.isWorking || isRefreshingCanonicalState {
                        ProgressView().controlSize(.small)
                    }
                    Button("Undo application") {
                        Task {
                            let undone = await proposalApplications.undo(storedReceipt)
                            if undone, canonicalSync.isConfigured {
                                isRefreshingCanonicalState = true
                                await canonicalSync.sync()
                                isRefreshingCanonicalState = false
                            }
                        }
                    }
                    .disabled(
                        Date() >= receipt.undoExpiresAt
                            || proposalApplications.status.isWorking
                            || isRefreshingCanonicalState
                    )
                }
            } else if let undoneAt = receipt.undoneAt {
                Text("Undone \(undoneAt.formatted(date: .abbreviated, time: .shortened)). Proposal history remains accepted and auditable.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 8)
    }
}

private func proposalFieldLabel(_ value: String) -> String {
    value.replacingOccurrences(of: "_", with: " ").capitalized
}

private func proposalRiskColor(_ level: String) -> Color {
    switch level {
    case "low": .green
    case "medium": .orange
    case "high": .red
    case "critical": .purple
    default: .secondary
    }
}

struct AppLockedView: View {
    @EnvironmentObject private var appLock: AppLockController

    var body: some View {
        ZStack {
            LinearGradient(
                colors: [
                    Color(nsColor: .windowBackgroundColor),
                    Color.accentColor.opacity(0.08),
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            .ignoresSafeArea()

            VStack(spacing: 18) {
                Image(systemName: "lock.shield.fill")
                    .font(.system(size: 52, weight: .semibold))
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                VStack(spacing: 7) {
                    Text("DayWeave is locked")
                        .font(.title2.weight(.semibold))
                    Text("Your schedule, accounts, and assistant stay hidden until you authenticate.")
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: 390)
                }

                Button {
                    Task { await appLock.unlock() }
                } label: {
                    if appLock.isAuthenticating {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("Authenticating…")
                        }
                    } else {
                        Label("Unlock DayWeave", systemImage: "touchid")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(appLock.isAuthenticating)
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier("app-lock.unlock")

                Text("Use Touch ID or your Mac login password.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let message = appLock.statusMessage {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 420)
                        .accessibilityIdentifier("app-lock.status")
                }
            }
            .padding(40)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("app-lock.screen")
    }
}

struct AppLockMenuBarView: View {
    @EnvironmentObject private var appLock: AppLockController

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("DayWeave is locked", systemImage: "lock.fill")
                .font(.headline)
            Text("Schedule details are hidden.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button(appLock.isAuthenticating ? "Authenticating…" : "Unlock DayWeave") {
                Task { await appLock.unlock() }
            }
            .buttonStyle(.borderedProminent)
            .disabled(appLock.isAuthenticating)
            Divider()
            Button("Quit DayWeave") { NSApp.terminate(nil) }
        }
        .padding(14)
        .frame(width: 260)
    }
}

struct MenuBarView: View {
    @Environment(\.openWindow) private var openWindow
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let active = focusedExecutionBlock {
                Text(active.status == .paused ? "Paused" : "In progress")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(active.title).font(.headline)
                Text(active.timeRange).font(.caption).foregroundStyle(.secondary)
                if active.sourceItemID != nil {
                    AuthoritativeExecutionControls(
                        block: active,
                        includesCustomPause: false
                    )
                    if executionSync.expiredBreakChoiceRequired {
                        Button("Keep paused") {
                            _ = executionSync.keepPausedAfterExpiredBreak()
                        }
                        .disabled(executionSync.isSyncing
                            || store.executionState.pendingCommand != nil
                            || !store.canMutatePlan)
                    }
                } else {
                    HStack {
                        Button("Pause") { store.pauseActive() }
                        Button("Complete") { store.complete(active.id) }
                    }
                    .disabled(!store.canMutate(active))
                }
            } else {
                ContentUnavailableView("Nothing active", systemImage: "checkmark.circle")
            }
            Divider()
            Button("Quick Capture…") { openWindow(id: "quick-capture") }
                .disabled(!store.canMutatePlan)
                .accessibilityIdentifier("menu-bar.quick-capture")
            Button("Recompose") { store.recomposeSchedule() }
                .disabled(!store.canRecomposeSchedule)
            Divider()
            Text(executionSync.status.message)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
            Button("Refresh execution") {
                Task { await executionSync.refresh() }
            }
            .disabled(executionSync.isSyncing || !store.canMutatePlan)
        }
        .padding(14)
        .frame(width: 300)
    }

    private var focusedExecutionBlock: ScheduleBlock? {
        if let session = executionSync.activeSession,
           let block = executionBlock(matching: session, in: store.blocks) {
            return block
        }
        return store.activeItem
    }
}

struct SettingsView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var googleIntegration: GoogleIntegrationStore
    @EnvironmentObject private var durableAuth: DurableAuthSettingsModel
    @EnvironmentObject private var appLock: AppLockController
    @EnvironmentObject private var appearance: AppearanceController
    @State private var dayWeaveAPIBaseURL = ""
    @State private var dayWeaveBearerToken = ""
    @State private var dayWeaveEnrollmentCode = ""
    @State private var isCanonicalResetConfirmationPresented = false
    @State private var isLocalOnlyForgetConfirmationPresented = false
    @State private var apiSettingsError: String?

    var body: some View {
        Form {
            Section("Scheduling") {
                Stepper("Freeze the next \(store.freezeHours) hours", value: $store.freezeHours, in: 0...24)
                Stepper("Protect \(store.protectedFreeMinutes) free minutes", value: $store.protectedFreeMinutes, in: 0...480, step: 15)
                Toggle("Show completed blocks", isOn: $store.showCompleted)
            }
            .disabled(!store.canMutatePlan)
            Section("Accounts") {
                GoogleIntegrationSettingsView()
                codexAccountControls
            }
            Section("Appearance") {
                Picker("Theme", selection: appearanceModeBinding) {
                    ForEach(DayWeaveAppearanceMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .pickerStyle(.segmented)

                LabeledContent("Accent") {
                    HStack(spacing: 12) {
                        ForEach(DayWeaveAccent.allCases) { accent in
                            Button {
                                _ = appearance.setAccent(accent)
                            } label: {
                                Circle()
                                    .fill(accent.color)
                                    .frame(width: 22, height: 22)
                                    .overlay {
                                        if appearance.preferences.accent == accent {
                                            Image(systemName: "checkmark")
                                                .font(.caption2.bold())
                                                .foregroundStyle(.white)
                                        }
                                    }
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("\(accent.title) accent")
                            .accessibilityValue(
                                appearance.preferences.accent == accent ? "Selected" : ""
                            )
                        }
                    }
                }

                if let message = appearance.statusMessage {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
            Section("Privacy & app lock") {
                Toggle("Require authentication to open DayWeave", isOn: appLockEnabledBinding)
                    .disabled(appLock.isAuthenticating)

                Picker("Lock when away", selection: appLockTimeoutBinding) {
                    ForEach(AppLockTimeout.allCases) { timeout in
                        Text(timeout.title).tag(timeout)
                    }
                }
                .disabled(!appLock.preferences.isEnabled || appLock.isAuthenticating)

                if appLock.preferences.isEnabled {
                    Button("Lock now") { appLock.lockNow() }
                        .disabled(appLock.isAuthenticating)
                }

                Text("Cold launches always start locked. When you leave the app, DayWeave hides every window and its menu-bar details after the selected delay. Enabling or disabling the lock requires Touch ID or your Mac login password.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if appLock.isAuthenticating {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Waiting for device-owner authentication…")
                            .foregroundStyle(.secondary)
                    }
                } else if let message = appLock.statusMessage {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.red)
                }
            }
            Section("DayWeave API") {
                TextField("https://dayweave.example.com", text: $dayWeaveAPIBaseURL)
                    .textContentType(.URL)
                    .disabled(durableAuth.isBusy)
                SecureField(
                    durableAuth.presentation.phase == .active
                        ? "Revoke current session before replacement"
                        : (durableAuth.presentation.canReenroll
                            ? "Bootstrap bearer for explicit re-enrollment"
                            : (suggestionSync.tokenConfigured
                                ? "New bootstrap bearer (blank for the same API origin)"
                                : "Bootstrap bearer")),
                    text: $dayWeaveBearerToken
                )
                .disabled(durableAuth.isBusy || !authReplacementControlsEnabled)
                SecureField(
                    "One-time enrollment code (dw_en1_…)",
                    text: $dayWeaveEnrollmentCode
                )
                .disabled(durableAuth.isBusy || !authReplacementControlsEnabled)
                HStack {
                    Button("Use one-time enrollment code") {
                        consumeOneTimeEnrollmentCode()
                    }
                    .disabled(
                        dayWeaveEnrollmentCode.isEmpty
                            || dayWeaveAPIBaseURL
                                .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            || !durableAuth.presentation.canConsumeEnrollmentCode
                            || durableAuth.isBusy
                            || suggestionSync.isRefreshing
                            || !suggestionSync.activeProposalIDs.isEmpty
                            || executionSync.isSyncing
                            || canonicalSync.isSyncing
                            || !store.canMutatePlan
                            || executionSync.credentialReplacementIsBlocked
                            || googleAuthenticationUpdateIsBlocked
                    )
                    Text("Directly consumes an already-minted code; it is never sent as a legacy bearer.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                HStack {
                    Button("Save API settings") {
                        saveAPISettings()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                        dayWeaveAPIBaseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            || (!suggestionSync.tokenConfigured
                                && dayWeaveBearerToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            || suggestionSync.isRefreshing
                            || !suggestionSync.activeProposalIDs.isEmpty
                            || executionSync.isSyncing
                            || canonicalSync.isSyncing
                            || durableAuth.isBusy
                            || (!authReplacementControlsEnabled
                                && !dayWeaveBearerToken.isEmpty)
                            || !store.canMutatePlan
                            || (apiCredentialReplacementRequired
                                && (executionSync.credentialReplacementIsBlocked
                                    || googleAuthenticationUpdateIsBlocked))
                    )

                    if durableAuth.presentation.canForget || suggestionSync.tokenConfigured {
                        if durableAuth.presentation.canRevokeRemotely {
                            Button("Revoke this Mac & sign out", role: .destructive) {
                                revokeAndRemoveAuthentication()
                            }
                            .disabled(
                                suggestionSync.isRefreshing
                                    || !suggestionSync.activeProposalIDs.isEmpty
                                    || executionSync.isSyncing
                                    || canonicalSync.isSyncing
                                    || durableAuth.isBusy
                                    || !store.canMutatePlan
                                    || executionSync.credentialReplacementIsBlocked
                                    || googleCredentialTransitionIsBlocked
                            )
                        }
                        Button("Forget only on this Mac…", role: .destructive) {
                            isLocalOnlyForgetConfirmationPresented = true
                        }
                        .disabled(
                            suggestionSync.isRefreshing
                                || !suggestionSync.activeProposalIDs.isEmpty
                                || executionSync.isSyncing
                                || canonicalSync.isSyncing
                                || durableAuth.isBusy
                                || !store.canMutatePlan
                                || executionSync.credentialReplacementIsBlocked
                                || googleCredentialTransitionIsBlocked
                        )
                    }
                }

                LabeledContent("Authentication", value: durableAuth.presentation.title)
                Text(durableAuth.presentation.detail)
                    .font(.caption)
                    .foregroundStyle(
                        durableAuth.presentation.phase == .incompatible
                            || durableAuth.presentation.phase == .reauthenticationRequired
                            ? .orange : .secondary
                    )
                if let expiresAt = durableAuth.presentation.accessExpiresAt {
                    LabeledContent(
                        "Current access expires",
                        value: expiresAt.formatted(date: .abbreviated, time: .shortened)
                    )
                }
                if durableAuth.presentation.canUpgrade {
                    Button(
                        durableAuth.presentation.phase == .enrollmentPending
                            || durableAuth.presentation.phase == .enrollmentCreationPending
                            ? "Resume exact enrollment"
                            : "Upgrade to rotating session"
                    ) {
                        upgradeDurableAuthentication()
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                        durableAuth.isBusy
                            || suggestionSync.isRefreshing
                            || !suggestionSync.activeProposalIDs.isEmpty
                            || executionSync.isSyncing
                            || canonicalSync.isSyncing
                            || !store.canMutatePlan
                            || executionSync.credentialReplacementIsBlocked
                            || googleAuthenticationUpdateIsBlocked
                    )
                }
                if durableAuth.isBusy {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Updating the atomic Keychain session…")
                            .foregroundStyle(.secondary)
                    }
                } else if let message = durableAuth.errorMessage {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
                Text(suggestionSync.status.message)
                    .font(.caption)
                    .foregroundStyle(suggestionSync.status.isFailure ? .red : .secondary)
                if let apiSettingsError {
                    Text(apiSettingsError)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
                if executionSync.credentialReplacementIsBlocked {
                    Text(store.pendingSchedulePublication == nil
                        ? "Reconcile the exact execution command or resolve pending canonical outcome choices before replacing this credential."
                        : "An exact schedule publication may already exist remotely. Restore the original API configuration and authentication, then run Planner sync to recover it before replacing credentials or resetting the cache.")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                Text("Remote HTTP is rejected; plain HTTP is accepted only for localhost development. Rotating access and refresh credentials stay in one atomic, device-only Keychain envelope and are never saved in the planner snapshot. DayWeave never falls back to a legacy bearer after durable activation.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                LabeledContent("Planner sync", value: canonicalSync.status.message)
                    .foregroundStyle(canonicalSync.status.isFailure ? .red : .secondary)
                LabeledContent("Execution sync", value: executionSync.status.message)
                    .foregroundStyle(executionStatusIsFailure ? .red : .secondary)
                Button("Reset local canonical cache…", role: .destructive) {
                    isCanonicalResetConfirmationPresented = true
                }
                .disabled(!store.canMutatePlan || executionSync.credentialReplacementIsBlocked)
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
        .confirmationDialog(
            "Forget authentication only on this Mac?",
            isPresented: $isLocalOnlyForgetConfirmationPresented,
            titleVisibility: .visible
        ) {
            Button("Forget locally without revoking", role: .destructive) {
                forgetAuthenticationLocally()
            }
        } message: {
            Text("This destroys the local Keychain credentials without contacting the server. A server-side device session, one-time enrollment, or legacy bootstrap bearer may remain active. Use this only when remote revocation is impossible.")
        }
        .formStyle(.grouped)
        .padding()
        .onAppear {
            dayWeaveAPIBaseURL = suggestionSync.baseURLString
            durableAuth.reload()
        }
        .onChange(of: dayWeaveAPIBaseURL) { _, value in
            durableAuth.reload(boundTo: try? DayWeaveAPIBaseURL(value))
        }
    }

    private var authReplacementControlsEnabled: Bool {
        switch durableAuth.presentation.phase {
        case .notConfigured, .legacy, .reauthenticationRequired:
            true
        case .enrollmentCreationPending, .enrollmentPending, .active,
             .refreshPending, .incompatible:
            false
        }
    }

    private var apiCredentialReplacementRequired: Bool {
        let tokenIsBeingReplaced = !dayWeaveBearerToken
            .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        guard let requested = try? DayWeaveAPIBaseURL(dayWeaveAPIBaseURL),
              let current = try? DayWeaveAPIBaseURL(suggestionSync.baseURLString) else {
            return tokenIsBeingReplaced || dayWeaveAPIBaseURL != suggestionSync.baseURLString
        }
        return tokenIsBeingReplaced
            || requested.canonicalConfigurationIdentifier
                != current.canonicalConfigurationIdentifier
    }

    private var googleCredentialTransitionIsBlocked: Bool {
        googleIntegration.isBusy
            || googleIntegration.credentialTransitionInProgress
            || googleIntegration.hasPendingRecovery
    }

    private var googleAuthenticationUpdateIsBlocked: Bool {
        guard googleCredentialTransitionIsBlocked else { return false }
        guard !googleIntegration.isBusy,
              !googleIntegration.credentialTransitionInProgress,
              durableAuthenticationNeedsRepair,
              let baseURL = try? DayWeaveAPIBaseURL(dayWeaveAPIBaseURL) else {
            return true
        }
        return !googleIntegration.canRepairAuthentication(boundTo: baseURL)
    }

    private var durableAuthenticationNeedsRepair: Bool {
        switch durableAuth.presentation.phase {
        case .active:
            false
        case .notConfigured, .legacy, .enrollmentCreationPending, .enrollmentPending,
             .refreshPending, .reauthenticationRequired, .incompatible:
            true
        }
    }

    private func allowGoogleCredentialTransition(allowSameAPIBaseRepair: Bool = false) -> Bool {
        if allowSameAPIBaseRepair,
           durableAuthenticationNeedsRepair,
           let baseURL = try? DayWeaveAPIBaseURL(dayWeaveAPIBaseURL),
           googleIntegration.canRepairAuthentication(boundTo: baseURL) {
            guard googleIntegration.beginCredentialRepairTransition(boundTo: baseURL) else {
                apiSettingsError = "DayWeave could not reserve Google recovery while repairing authentication."
                return false
            }
            return true
        }
        guard !googleCredentialTransitionIsBlocked else {
            apiSettingsError = "Finish or authoritatively reconcile the pending Google operation before replacing DayWeave authentication."
            return false
        }
        guard googleIntegration.beginCredentialTransition() else {
            apiSettingsError = "DayWeave could not reserve the Google connection while authentication changes."
            return false
        }
        return true
    }

    private var appLockEnabledBinding: Binding<Bool> {
        Binding(
            get: { appLock.preferences.isEnabled },
            set: { enabled in
                Task { await appLock.setEnabled(enabled) }
            }
        )
    }

    private var appearanceModeBinding: Binding<DayWeaveAppearanceMode> {
        Binding(
            get: { appearance.preferences.mode },
            set: { mode in
                _ = appearance.setMode(mode)
            }
        )
    }

    private var appLockTimeoutBinding: Binding<AppLockTimeout> {
        Binding(
            get: { appLock.preferences.timeout },
            set: { timeout in
                _ = appLock.setTimeout(timeout)
            }
        )
    }

    private var executionStatusIsFailure: Bool {
        switch executionSync.status.phase {
        case .offline, .authenticationRequired, .failed:
            true
        case .notConfigured, .ready, .syncing, .connected:
            false
        }
    }

    private func saveAPISettings() {
        apiSettingsError = nil
        let reservedGoogleTransition = apiCredentialReplacementRequired
        if reservedGoogleTransition,
           !allowGoogleCredentialTransition(allowSameAPIBaseRepair: true) { return }
        do {
            let baseURL = try DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
            let capturedBaseURL = baseURL.url.absoluteString
            let replacementRequired = apiCredentialReplacementRequired
            if replacementRequired {
                try executionSync.prepareForCredentialReplacement()
            }
            let bootstrap = dayWeaveBearerToken
                .trimmingCharacters(in: .whitespacesAndNewlines)
            Task { @MainActor in
                defer {
                    if reservedGoogleTransition {
                        googleIntegration.endCredentialTransition()
                    }
                }
                if !bootstrap.isEmpty {
                    let saved: Bool
                    if durableAuth.presentation.canReenroll {
                        saved = await durableAuth.enroll(
                            baseURL: baseURL,
                            bootstrapToken: bootstrap
                        )
                    } else {
                        saved = await durableAuth.installLegacy(
                            baseURL: baseURL,
                            token: bootstrap
                        )
                    }
                    guard saved else {
                        apiSettingsError = durableAuth.errorMessage
                        return
                    }
                }
                guard suggestionSync.applyConfiguration(
                    baseURL: capturedBaseURL,
                    newToken: ""
                ) else {
                    apiSettingsError = suggestionSync.status.message
                    return
                }
                dayWeaveBearerToken = ""
                dayWeaveAPIBaseURL = suggestionSync.baseURLString
                suggestionSync.durableAuthenticationDidChange()
                guard replacementRequired || !bootstrap.isEmpty else { return }
                googleIntegration.configurationDidChange()
                canonicalSync.configurationDidChange()
                executionSync.configurationDidChange()
                executionSync.startForegroundPolling()
            }
        } catch {
            if reservedGoogleTransition {
                googleIntegration.endCredentialTransition()
            }
            apiSettingsError = error.localizedDescription
        }
    }

    private func revokeAndRemoveAuthentication() {
        apiSettingsError = nil
        guard allowGoogleCredentialTransition() else { return }
        do {
            try executionSync.prepareForCredentialReplacement()
            let baseURL = try DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
            Task { @MainActor in
                defer { googleIntegration.endCredentialTransition() }
                guard await durableAuth.revokeAndForget(baseURL: baseURL) else {
                    apiSettingsError = durableAuth.errorMessage
                    return
                }
                suggestionSync.durableAuthenticationDidChange()
                googleIntegration.configurationDidChange()
                canonicalSync.configurationDidChange()
                executionSync.configurationDidChange()
                dayWeaveBearerToken = ""
            }
        } catch {
            googleIntegration.endCredentialTransition()
            apiSettingsError = error.localizedDescription
        }
    }

    private func forgetAuthenticationLocally() {
        apiSettingsError = nil
        guard allowGoogleCredentialTransition() else { return }
        do {
            try executionSync.prepareForCredentialReplacement()
            let baseURL = try? DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
            Task { @MainActor in
                defer { googleIntegration.endCredentialTransition() }
                guard await durableAuth.forgetLocally(baseURL: baseURL) else {
                    apiSettingsError = durableAuth.errorMessage
                    return
                }
                suggestionSync.durableAuthenticationDidChange()
                googleIntegration.configurationDidChange()
                canonicalSync.configurationDidChange()
                executionSync.configurationDidChange()
                dayWeaveBearerToken = ""
                dayWeaveEnrollmentCode = ""
            }
        } catch {
            googleIntegration.endCredentialTransition()
            apiSettingsError = error.localizedDescription
        }
    }

    private func consumeOneTimeEnrollmentCode() {
        apiSettingsError = nil
        guard allowGoogleCredentialTransition(allowSameAPIBaseRepair: true) else { return }
        do {
            let baseURL = try DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
            let capturedBaseURL = baseURL.url.absoluteString
            try executionSync.prepareForCredentialReplacement()
            let code = dayWeaveEnrollmentCode
            Task { @MainActor in
                defer { googleIntegration.endCredentialTransition() }
                guard await durableAuth.consumeEnrollmentCode(baseURL: baseURL, code: code) else {
                    apiSettingsError = durableAuth.errorMessage
                    return
                }
                dayWeaveEnrollmentCode = ""
                guard suggestionSync.applyConfiguration(
                    baseURL: capturedBaseURL,
                    newToken: ""
                ) else {
                    apiSettingsError = suggestionSync.status.message
                    return
                }
                dayWeaveAPIBaseURL = suggestionSync.baseURLString
                suggestionSync.durableAuthenticationDidChange()
                googleIntegration.configurationDidChange()
                canonicalSync.configurationDidChange()
                executionSync.configurationDidChange()
                executionSync.startForegroundPolling()
            }
        } catch {
            googleIntegration.endCredentialTransition()
            apiSettingsError = error.localizedDescription
        }
    }

    private func upgradeDurableAuthentication() {
        apiSettingsError = nil
        guard allowGoogleCredentialTransition(allowSameAPIBaseRepair: true) else { return }
        do {
            let baseURL = try DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
            let capturedBaseURL = baseURL.url.absoluteString
            try executionSync.prepareForCredentialReplacement()
            Task { @MainActor in
                defer { googleIntegration.endCredentialTransition() }
                guard await durableAuth.enroll(baseURL: baseURL) else {
                    apiSettingsError = durableAuth.errorMessage
                    return
                }
                guard suggestionSync.applyConfiguration(
                    baseURL: capturedBaseURL,
                    newToken: ""
                ) else {
                    apiSettingsError = suggestionSync.status.message
                    return
                }
                dayWeaveAPIBaseURL = suggestionSync.baseURLString
                suggestionSync.durableAuthenticationDidChange()
                googleIntegration.configurationDidChange()
                canonicalSync.configurationDidChange()
                executionSync.configurationDidChange()
                executionSync.startForegroundPolling()
            }
        } catch {
            googleIntegration.endCredentialTransition()
            apiSettingsError = error.localizedDescription
        }
    }

    @ViewBuilder
    private var codexAccountControls: some View {
        LabeledContent {
            HStack(spacing: 7) {
                Circle()
                    .fill(codexStatusColor)
                    .frame(width: 8, height: 8)
                Text(codex.state.title)
            }
        } label: {
            Text("Codex")
        }

        switch codex.state {
        case .stopped:
            Text("A verified, contained Codex runtime is bundled with DayWeave.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button("Start Codex") { codex.retry() }

        case .starting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Starting the contained Codex runtime…")
                    .foregroundStyle(.secondary)
            }

        case .signedOut:
            Text("Connect your ChatGPT account with a one-time device code. Only managed ChatGPT sign-in is enabled.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button("Sign in with ChatGPT") { codex.signInWithDeviceCode() }
                .buttonStyle(.borderedProminent)

        case .signingIn:
            if let code = codex.deviceCode {
                VStack(alignment: .leading, spacing: 10) {
                    Text("Enter this one-time code on the OpenAI sign-in page")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(code)
                        .font(.system(.title3, design: .monospaced).weight(.semibold))
                        .textSelection(.enabled)
                        .accessibilityLabel("Device code \(code)")
                    HStack {
                        Button("Open OpenAI sign-in") { codex.openVerificationPage() }
                            .buttonStyle(.borderedProminent)
                        Button("Copy code") { copyCodexDeviceCode(code) }
                        Button("Cancel", role: .cancel) { codex.cancelSignIn() }
                    }
                }
                .padding(.vertical, 4)
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Preparing a one-time device code…")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Cancel", role: .cancel) { codex.cancelSignIn() }
                }
            }

        case .cancellingSignIn:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Canceling the sign-in ceremony…")
                    .foregroundStyle(.secondary)
            }

        case let .signedIn(email, _):
            if let email {
                LabeledContent("ChatGPT account", value: email)
                    .textSelection(.enabled)
            }
            Text("Credentials are managed by Codex inside DayWeave’s isolated local storage.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button("Sign out of Codex", role: .destructive) { codex.signOut() }

        case let .unavailable(message):
            Text(message)
                .font(.caption)
                .foregroundStyle(.red)
                .textSelection(.enabled)
            Button("Retry Codex") { codex.retry() }
        }
    }

    private var codexStatusColor: Color {
        switch codex.state {
        case .signedIn: .green
        case .starting, .signingIn, .cancellingSignIn: .orange
        case .unavailable: .red
        case .stopped, .signedOut: .secondary
        }
    }

    private func copyCodexDeviceCode(_ code: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(code, forType: .string)
    }
}
