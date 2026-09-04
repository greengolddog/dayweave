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

private func scheduleTimeZone(_ identifier: String) -> TimeZone {
    PlannerTimeZone.resolve(identifier)
}

private func scheduleTimeLabel(_ date: Date, timezoneName: String) -> String {
    var style = Date.FormatStyle()
        .hour(.twoDigits(amPM: .omitted))
        .minute(.twoDigits)
        .timeZone(.iso8601(.long))
    style.timeZone = scheduleTimeZone(timezoneName)
    return date.formatted(style)
}

private func scheduleTimeRange(_ block: ScheduleBlock, timezoneName: String) -> String {
    "\(scheduleTimeLabel(block.start, timezoneName: timezoneName))–\(scheduleTimeLabel(block.end, timezoneName: timezoneName))"
}

private func scheduleDateTimeLabel(_ date: Date, timezoneName: String) -> String {
    PlannerTimeZone.dateTimeLabel(date, timezoneName: timezoneName)
}

private func canonicalDeadlineDisplayValue(
    _ base: String,
    item: DayWeaveCanonicalItem
) -> String {
    let policy = switch item.deadlineStrength {
    case .hard?: "Hard"
    case .soft?: item.deadlineSoftWeight.map { "Soft · weight \($0)" } ?? "Soft"
    case .unsupported?: "Newer deadline policy"
    case nil: "Deadline policy unavailable"
    }
    return "\(base) · \(policy)"
}

private func canonicalBlockedDisplayValue(
    _ item: DayWeaveCanonicalItem,
    dependencyCauses: [CanonicalDependencyCause] = []
) -> String? {
    guard item.status == .blocked else { return nil }
    switch item.blockedReasonKind {
    case .dependency?:
        let blockers = dependencyCauses.filter(\.isBlocking)
        if blockers.count == 1, let blocker = blockers.first {
            return "Waiting for \(blocker.title)"
        }
        if blockers.count > 1 { return "Waiting on \(blockers.count) prerequisites" }
        let dependency = item.blockedByItemID.map {
            "Waiting for item \($0.uuidString.lowercased().prefix(8))"
        } ?? "Waiting for a dependency"
        // Legacy dependency reasons may embed a private predecessor title.
        // Without a resolved cause, only show the opaque relationship.
        return dependency
    case .manual?:
        return item.blockedReason.map { "Manually blocked · \($0)" } ?? "Manually blocked"
    case .external?:
        return item.blockedReason.map { "External blocker · \($0)" } ?? "External blocker"
    case .unsupported?:
        return "Blocked for a reason that requires a newer DayWeave version"
    case nil:
        return "Blocked reason unavailable"
    }
}

private func scheduleDayLabel(_ date: Date, timezoneName: String) -> String {
    var style = Date.FormatStyle()
        .weekday(.wide)
        .month(.wide)
        .day()
    style.timeZone = scheduleTimeZone(timezoneName)
    return date.formatted(style)
}

struct WillDoLaterMoveWindow: Equatable, Sendable {
    let start: Date
    let end: Date
    let movedBlockIDs: Set<UUID>
}

enum WillDoLaterTiming {
    static let executionSlotSeconds = DayWeaveExecutionDeferTiming.slotSeconds

    static func roundedUpToMinute(_ date: Date) -> Date {
        Date(timeIntervalSince1970: (date.timeIntervalSince1970 / 60).rounded(.up) * 60)
    }

    static func roundedUpToExecutionSlot(_ date: Date) -> Date {
        DayWeaveExecutionDeferTiming.roundedUpToSlot(date)
    }

    static func minimumExecutionMoveStart(after referenceDate: Date) -> Date {
        DayWeaveExecutionDeferTiming.minimumMoveStart(after: referenceDate)
    }

    static func tomorrowMorning(
        after referenceDate: Date,
        minimum: Date,
        timezoneName: String
    ) -> Date {
        var calendar = PlannerPresentation.calendar(timezoneName: timezoneName)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: referenceDate)
            ?? referenceDate
        let morning = calendar.date(
            bySettingHour: 9,
            minute: 0,
            second: 0,
            of: tomorrow
        ) ?? tomorrow
        return max(morning, roundedUpToMinute(minimum))
    }

    static func proposedWindow(
        for block: ScheduleBlock,
        moveStart: Date,
        allBlocks: [ScheduleBlock],
        accumulatedSeconds: UInt64?
    ) -> WillDoLaterMoveWindow? {
        guard moveStart.timeIntervalSinceReferenceDate.isFinite else { return nil }

        if block.status == .active || block.status == .paused {
            let planned = block.end.timeIntervalSince(block.start)
            guard planned > 0, planned.isFinite else { return nil }
            let remaining = max(0, planned - TimeInterval(accumulatedSeconds ?? 0))
            guard remaining > 0 else { return nil }
            return .init(
                start: moveStart,
                end: moveStart.addingTimeInterval(remaining),
                movedBlockIDs: [block.id]
            )
        }

        if let occurrenceID = block.occurrenceID,
           let focusedItemID = block.sourceItemID,
           block.sourceItemRevision != nil,
           let source = block.recurrenceMoveSource {
            let seriesItemID = block.recurrenceSeriesItemID ?? focusedItemID
            let siblings = allBlocks.filter { $0.occurrenceID == occurrenceID }
            guard !siblings.isEmpty,
                  siblings.allSatisfy({ sibling in
                      sibling.sourceItemID != nil
                          && sibling.sourceItemRevision != nil
                          && (sibling.recurrenceSeriesItemID ?? sibling.sourceItemID)
                            == seriesItemID
                          && sibling.recurrenceMoveSource == source
                          && sibling.status == .scheduled
                          && sibling.isFlexible
                          && !sibling.isHardConstraint
                          && sibling.previewKind != "pinned"
                          && sibling.previewKind != "external_fixed"
                          && sibling.occurrenceFullyScheduled
                  }),
                  let earliest = siblings.map(\.start).min(),
                  let latest = siblings.map(\.end).max() else { return nil }
            let shift = moveStart.timeIntervalSince(block.start)
            let shiftedStart = earliest.addingTimeInterval(shift)
            let shiftedEnd = latest.addingTimeInterval(shift)
            guard shiftedEnd > shiftedStart else { return nil }
            return .init(
                start: shiftedStart,
                end: shiftedEnd,
                movedBlockIDs: Set(siblings.map(\.id))
            )
        }

        let duration = block.end.timeIntervalSince(block.start)
        guard duration > 0, duration.isFinite else { return nil }
        return .init(
            start: moveStart,
            end: moveStart.addingTimeInterval(duration),
            movedBlockIDs: [block.id]
        )
    }

    static func fixedConflicts(
        with window: WillDoLaterMoveWindow,
        in blocks: [ScheduleBlock]
    ) -> [ScheduleBlock] {
        blocks.filter { candidate in
            !window.movedBlockIDs.contains(candidate.id)
                && candidate.status != .completed
                && candidate.status != .skipped
                && candidate.status != .canceled
                && DayWeaveMoveConflictIdentity(block: candidate).hasValidShape
                && window.start < candidate.end
                && window.end > candidate.start
        }
        .sorted { $0.start < $1.start }
    }

    static func finishesAfterLatestFinish(
        _ window: WillDoLaterMoveWindow,
        latestFinish: Date
    ) -> Bool {
        window.end > latestFinish
    }

    static func crossedDeadlines(
        _ deadlines: Set<DayWeaveMoveDeadlineIdentity>,
        window: WillDoLaterMoveWindow
    ) -> Set<DayWeaveMoveDeadlineIdentity> {
        // A recurrence exception provides an outer window rather than exact
        // leaf placements. Its end is therefore a conservative "may be as late
        // as" bound for every moved leaf, while exact/local work uses its real
        // replacement end.
        return deadlines.filter { window.end > $0.boundary.date }
    }

    static func usesExactPlacement(for block: ScheduleBlock) -> Bool {
        block.sourceItemID == nil
            || block.status == .active
            || block.status == .paused
    }

    static func conflictLabel(
        _ block: ScheduleBlock,
        timezoneName: String
    ) -> String {
        let title = block.isSensitive ? "Sensitive busy time" : block.title
        return "\(title) (\(scheduleTimeRange(block, timezoneName: timezoneName)))"
    }
}

@MainActor
final class WillDoLaterPresenter: ObservableObject {
    struct Request: Identifiable, Equatable {
        let id: UUID
        let blockID: UUID
        let initialMoveStart: Date
    }

    @Published var request: Request?

    func present(blockID: UUID, initialMoveStart: Date) {
        request = .init(
            id: UUID(),
            blockID: blockID,
            initialMoveStart: initialMoveStart
        )
    }

    func dismiss(_ requestID: UUID) {
        guard request?.id == requestID else { return }
        request = nil
    }
}

struct RootView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @ObservedObject private var breakNotificationTapRouter =
        DayWeaveBreakNotificationTapRouter.shared
    @ObservedObject private var breakNotificationDeliveryPulse =
        DayWeaveBreakNotificationDeliveryPulse.shared
    @StateObject private var willDoLaterPresenter = WillDoLaterPresenter()
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
        .environmentObject(willDoLaterPresenter)
        .safeAreaInset(edge: .top, spacing: 0) {
            VStack(spacing: 0) {
                staleBreakNotificationTapBanner
                breakNotificationPermissionBanner
            }
        }
        .sheet(isPresented: $store.isQuickAddPresented) {
            QuickCaptureView(profileTimezoneName: store.scheduleProfile.timezoneName)
                .environmentObject(store)
        }
        .sheet(item: $willDoLaterPresenter.request) { request in
            WillDoLaterSheet(request: request)
                .environmentObject(store)
                .environmentObject(canonicalSync)
                .environmentObject(executionSync)
                .environmentObject(willDoLaterPresenter)
        }
        .onAppear {
            codex.startIfNeeded()
        }
        .onChange(of: breakNotificationDeliveryPulse.generation) { _, _ in
            executionSync.breakNotificationForegroundDeliveryDidOccur()
        }
        .onChange(of: breakAlternativeCandidateIDs) { _, _ in
            executionSync.reconcileBreakAlternativeSelection()
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
            Button("Extend 10 minutes") {
                if let blockID = activeExecutionBlockID {
                    isResolvingExpiredBreak = true
                    Task {
                        _ = await executionSync.pause(
                            blockID,
                            durationSeconds: 10 * 60
                        )
                        isResolvingExpiredBreak = false
                    }
                }
            }
            .accessibilityIdentifier("execution.expired-break.extend-10-minutes")
            .disabled(executionSync.isSyncing
                || store.executionState.pendingCommand != nil
                || !store.canMutatePlan)
            Button("Choose another item") {
                isResolvingExpiredBreak = true
                Task {
                    _ = await executionSync.chooseAnotherAfterExpiredBreak()
                    isResolvingExpiredBreak = false
                }
            }
            .accessibilityIdentifier("execution.expired-break.choose-another")
            .disabled(executionSync.isSyncing
                || store.executionState.pendingCommand != nil
                || !store.canMutatePlan)
            Button("Keep paused") {
                isResolvingExpiredBreak = true
                Task {
                    _ = await executionSync.keepPausedAfterExpiredBreak()
                    isResolvingExpiredBreak = false
                }
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
                .disabled(
                    !canonicalSync.isConfigured
                        || canonicalSync.isSyncing
                        || canonicalSync.isLocallyComposing
                        || !store.canMutatePlan
                )

                Button {
                    Task { await executionSync.refresh() }
                } label: {
                    Label("Refresh execution", systemImage: "timer")
                }
                .help("Reconcile the authoritative execution lease and complete history")
                .disabled(executionSync.isSyncing || !store.canMutatePlan)
                .accessibilityIdentifier("execution.refresh")

                Button {
                    Task { await canonicalSync.recomposeLocally() }
                } label: {
                    Label("Compose on Mac", systemImage: "wand.and.stars")
                }
                .help("Compose seven days from the encrypted cache on this Mac without publishing to Google Calendar")
                .disabled(
                    !store.canMutatePlan
                        || canonicalSync.isSyncing
                        || canonicalSync.isLocallyComposing
                        || !canonicalSync.canRecomposeLocally
                )
                .accessibilityIdentifier("schedule.compose-local.toolbar")

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
                executionSync.shouldPresentExpiredBreakResolution(
                    pendingNotificationIdentifier:
                        breakNotificationTapRouter.pendingIdentifier
                )
                    && !isResolvingExpiredBreak
            },
            set: { _ in }
        )
    }

    private var activeExecutionBlockID: UUID? {
        guard let active = executionSync.activeSession else { return nil }
        return executionBlock(matching: active, in: store.blocks)?.id
    }

    private var breakAlternativeCandidateIDs: [UUID]? {
        executionSync.breakAlternativePresentation?.candidates.map(\.id)
    }

    @ViewBuilder
    private var staleBreakNotificationTapBanner: some View {
        if let issue = executionSync.breakNotificationTapIssue {
            HStack(spacing: 12) {
                Label(issue.message, systemImage: "bell.slash")
                    .font(.subheadline.weight(.medium))
                    .accessibilityIdentifier("execution.break-notification.stale-tap")
                Spacer()
                Button(executionSync.expiredBreakChoiceRequired
                    ? "Review current break" : "Dismiss") {
                    executionSync.acknowledgeStaleBreakNotificationTap()
                }
                .accessibilityIdentifier(
                    "execution.break-notification.stale-tap-acknowledge"
                )
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
            .background(.bar)
            .overlay(alignment: .bottom) { Divider() }
        }
    }

    @ViewBuilder
    private var breakNotificationPermissionBanner: some View {
        if executionSync.breakNotificationBannerShouldBePresented {
            HStack(spacing: 12) {
                Label(
                    executionSync.breakNotificationIssue?.message
                        ?? (executionSync.breakNotificationAuthorizationState == .denied
                            ? "Break reminders are disabled"
                            : "Get a reminder when this break ends"),
                    systemImage: "bell.badge"
                )
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("execution.break-notification.status")
                Spacer()
                if let issue = executionSync.breakNotificationIssue {
                    Button(issue.retryTitle) {
                        Task { _ = await executionSync.retryBreakNotification() }
                    }
                    .disabled(executionSync.isRequestingBreakNotificationAuthorization)
                    .accessibilityIdentifier("execution.break-notification.retry")
                } else if executionSync.breakNotificationAuthorizationState == .notDetermined {
                    Button("Enable reminders") {
                        Task {
                            _ = await executionSync.requestBreakNotificationAuthorization()
                        }
                    }
                    .disabled(executionSync.isRequestingBreakNotificationAuthorization)
                    .accessibilityIdentifier("execution.break-notification.enable")
                } else {
                    Button("Open Notification Settings") {
                        if let url = URL(string:
                            "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
                        ) {
                            NSWorkspace.shared.open(url)
                        }
                    }
                    .accessibilityIdentifier("execution.break-notification.settings")
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
            .background(.bar)
            .overlay(alignment: .bottom) { Divider() }
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
    @EnvironmentObject private var googleOutbound: GoogleOutboundStore
    @EnvironmentObject private var googleSchedulePublication: GoogleSchedulePublicationStore
    @State private var googleReviewIsPresented = false

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
                    Text(googleOutbound.status.message).lineLimit(2)
                } icon: {
                    Image(systemName: googleOutbound.hasPendingRecovery
                        ? "arrow.up.circle.fill" : "arrow.up.circle")
                }
                .font(.caption)
                .foregroundStyle(googleOutboundStatusColor)
                if googleOutbound.hasPendingRecovery {
                    if googleOutbound.preview != nil {
                        Button("Review \(googleRecoveryServiceName) change") {
                            googleReviewIsPresented = true
                        }
                        .accessibilityIdentifier("google.outbound.sidebar-review")
                    } else if googleOutbound.hasApprovedRecovery {
                        Button(googleOutbound.status == .expired
                            ? "Check \(googleRecoveryServiceName) acceptance"
                            : "Recover approved \(googleRecoveryServiceName) change") {
                            Task {
                                _ = await googleOutbound.recoverPendingOperation()
                            }
                        }
                        .disabled(googleOutbound.status.isWorking)
                        .accessibilityIdentifier("google.outbound.sidebar-check-acceptance")
                        if googleOutbound.status == .expired {
                            GoogleExpiredRecoveryDiscardButton(
                                title: "Discard expired \(googleRecoveryServiceName) recovery",
                                accessibilityIdentifier: "google.outbound.sidebar-discard"
                            )
                        }
                    } else if googleOutbound.status == .expired {
                        GoogleExpiredRecoveryDiscardButton(
                            title: "Discard expired \(googleRecoveryServiceName) recovery",
                            accessibilityIdentifier: "google.outbound.sidebar-discard"
                        )
                    } else if !googleOutbound.status.isWaitingForSafeDiscard {
                        Button("Recover \(googleRecoveryServiceName) change") {
                            Task {
                                _ = await googleOutbound.recoverPendingOperation()
                                if googleOutbound.preview != nil {
                                    googleReviewIsPresented = true
                                }
                            }
                        }
                        .disabled(googleOutbound.status.isWorking)
                        .accessibilityIdentifier("google.outbound.sidebar-recover")
                    }
                }
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
        .sheet(isPresented: $googleReviewIsPresented) {
            GoogleOutboundReviewSheet(fallbackTitle: googleRecoveryFallbackTitle)
                .environmentObject(googleOutbound)
        }
    }

    private var persistenceColor: Color {
        if store.persistenceError != nil { return .red }
        return store.hasEncryptedPersistence ? .green : .secondary
    }

    private var googleRecoveryFallbackTitle: String {
        guard let itemID = googleOutbound.recoveryContext?.itemID else {
            return googleRecoveryEntityKind == .task
                ? "Saved DayWeave task" : "Saved DayWeave event"
        }
        return store.canonicalItems.first(where: { $0.id == itemID })?.title
            ?? store.canonicalTrash.first(where: { $0.id == itemID })?.title
            ?? (googleRecoveryEntityKind == .task
                ? "Saved DayWeave task" : "Saved DayWeave event")
    }

    private var googleRecoveryEntityKind: GoogleOutboundEntityKind {
        googleOutbound.preview?.entityKind
            ?? googleOutbound.recoveryContext?.entityKind
            ?? .calendarEvent
    }

    private var googleRecoveryServiceName: String {
        googleRecoveryEntityKind == .task ? "Google Tasks" : "Google Calendar"
    }

    private var googleOutboundStatusColor: Color {
        switch googleOutbound.status {
        case .failed, .recoveryRequired, .expirySafetyDelay, .expired:
            .orange
        case .accepted:
            .green
        case .privacyProtected, .idle, .previewing, .awaitingApproval,
             .approving, .enqueueing:
            .secondary
        }
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
                Label(
                    scheduleTimeRange(
                        block,
                        timezoneName: store.schedulePresentationTimezoneName
                    ),
                    systemImage: "timer"
                )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                if block.sourceItemID != nil {
                    AuthoritativeExecutionControls(
                        block: block,
                        includesCustomPause: false,
                        accessibilityScope: "active-strip"
                    )
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
        .privacySensitive(block.isSensitive)
    }
}

private struct TodayView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var onboarding: DayWeaveOnboardingController

    var body: some View {
        VStack(spacing: 0) {
            TodayHeader()
            if !onboarding.isComplete, !onboarding.isPresented {
                OnboardingResumeBanner()
            }
            CanonicalSyncBanner()
            LocalCompositionBanner()
            ExecutionSyncBanner()
            PreviewDiagnosticsStrip()
            if let presentation = executionSync.breakAlternativePresentation {
                BreakAlternativeHandoffView(presentation: presentation)
            }
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

private struct OnboardingResumeBanner: View {
    @EnvironmentObject private var onboarding: DayWeaveOnboardingController

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "checklist")
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("Finish setting up DayWeave")
                    .font(.subheadline.weight(.semibold))
                Text("Resume at \(onboarding.currentStep.title.lowercased()).")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Resume setup") {
                onboarding.present()
            }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier("onboarding.resume.today")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
        .background(Color.accentColor.opacity(0.07))
        .overlay(alignment: .bottom) { Divider() }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("onboarding.resume-banner")
    }
}

private struct BreakAlternativeHandoffView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    let presentation: BreakAlternativePresentation

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Label("Choose another item", systemImage: "arrow.triangle.branch")
                    .font(.headline)
                Spacer()
                Text("Current session paused")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.orange)
            }

            Text(BreakAlternativePresentation.selectionGuidance)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if presentation.candidates.isEmpty {
                Label(
                    BreakAlternativePresentation.emptyGuidance,
                    systemImage: "pause.circle"
                )
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("execution.break-alternatives.empty")
            } else {
                ScrollView(.horizontal) {
                    LazyHStack(alignment: .top, spacing: 10) {
                        ForEach(presentation.candidates) { candidate in
                            candidateButton(candidate)
                        }
                    }
                    .padding(.vertical, 2)
                }
                .scrollIndicators(.hidden)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .background(Color.accentColor.opacity(0.07))
        .overlay(alignment: .bottom) { Divider() }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("execution.break-alternatives")
    }

    private func candidateButton(_ candidate: BreakAlternativeCandidate) -> some View {
        let isSelected = presentation.selectedCandidateID == candidate.id
        return Button {
            executionSync.selectBreakAlternative(candidate.id)
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    if candidate.isNextInPlan {
                        Text("Next in plan")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.tint)
                    }
                    if isSelected {
                        Label("Selected", systemImage: "checkmark.circle.fill")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.tint)
                    }
                }
                Text(candidate.block.title)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Text(scheduleTimeRange(
                    candidate.block,
                    timezoneName: store.schedulePresentationTimezoneName
                ))
                .font(.caption)
                .foregroundStyle(.secondary)
                if let reason = candidate.placementReason {
                    Text(reason)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            .frame(width: 220, alignment: .leading)
            .padding(12)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(isSelected
                        ? Color.accentColor.opacity(0.12)
                        : Color(nsColor: .controlBackgroundColor))
            )
            .overlay {
                RoundedRectangle(cornerRadius: 10)
                    .stroke(
                        isSelected ? Color.accentColor : Color(nsColor: .separatorColor),
                        lineWidth: 1
                    )
            }
            .contentShape(RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        .privacySensitive(candidate.block.isSensitive)
        .accessibilityIdentifier(
            "execution.break-alternative.\(candidate.id.uuidString.lowercased())"
        )
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
        appendUnique(canonicalSync.localCompositionWarnings, to: &result)
        if store.blocks.contains(where: {
            $0.syncOrigin == .canonicalPreview
                || $0.syncOrigin == .externalPreview
                || $0.syncOrigin == .localComposition
        }), let issue = store.canonicalPreviewFreshnessIssue {
            result.append("Schedule actions are locked: \(issue)")
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
        if let composition = canonicalSync.lastLocalComposition {
            appendUnique(composition.plan.unscheduled.map {
                "\(title(for: $0.itemID)): \($0.remaining)m unscheduled on this Mac (\($0.reason)). \($0.message)"
            }, to: &result)
            appendUnique(composition.rejectedItems.map {
                "“\($0.title)” was excluded on this device: \($0.reason)"
            }, to: &result)
            appendUnique(composition.ignoredPreviousAssignments.map {
                "A previous assignment for \($0.itemID.uuidString.lowercased()) was ignored on this device: \($0.reason)"
            }, to: &result)
            appendUnique(composition.plan.decisions.map {
                "On-device decision: \($0.displayDescription)"
            }, to: &result)
            appendUnique(composition.plan.violations.map {
                "On-device violation: \($0.displayDescription)"
            }, to: &result)
            return result
        }
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

    private func appendUnique(_ messages: [String], to result: inout [String]) {
        for message in messages where !result.contains(message) {
            result.append(message)
        }
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
    @EnvironmentObject private var store: PlannerStore
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
            .disabled(
                !canonicalSync.isConfigured
                    || canonicalSync.isSyncing
                    || canonicalSync.isLocallyComposing
                    || !store.canMutatePlan
            )
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

private struct LocalCompositionBanner: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore

    var body: some View {
        HStack(spacing: 9) {
            if canonicalSync.isLocallyComposing {
                ProgressView().controlSize(.small)
            } else {
                Circle()
                    .fill(statusColor)
                    .frame(width: 7, height: 7)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(statusMessage)
                    .font(.caption)
                    .foregroundStyle(statusIsFailure ? .red : .secondary)
                    .lineLimit(2)
                Text(provenanceSummary ?? "Uses the encrypted cache; stays on this Mac and is not published to Google Calendar.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .help(provenanceSummary ?? "Uses the encrypted cache; stays on this Mac and is not published to Google Calendar.")
                if provenanceSummary != nil {
                    Text("This schedule stays on this Mac and is not published to Google Calendar.")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
            Spacer()
            if !canonicalSync.localCompositionWarnings.isEmpty {
                Label(
                    "\(canonicalSync.localCompositionWarnings.count) to review",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.caption.weight(.medium))
                .foregroundStyle(.orange)
                .help(canonicalSync.localCompositionWarnings.joined(separator: "\n"))
            }
            Button("Compose on this Mac") {
                Task { await canonicalSync.recomposeLocally() }
            }
            .controlSize(.small)
            .disabled(
                !store.canMutatePlan
                    || canonicalSync.isSyncing
                    || canonicalSync.isLocallyComposing
                    || !canonicalSync.canRecomposeLocally
            )
            .accessibilityIdentifier("schedule.compose-local.banner")
            Button("Sync instead") {
                Task { await canonicalSync.sync() }
            }
            .controlSize(.small)
            .disabled(
                !canonicalSync.isConfigured
                    || canonicalSync.isSyncing
                    || canonicalSync.isLocallyComposing
                    || !store.canMutatePlan
            )
            .help("Pull canonical changes, publish safe edits, and compose through the server")
            .accessibilityIdentifier("schedule.sync-fallback.banner")
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 9)
        .background(Color.accentColor.opacity(0.055))
    }

    private var statusMessage: String {
        if case .ready = canonicalSync.localCompositionStatus,
           store.localScheduleCompositionProvenance != nil {
            return "An on-device schedule is installed from the encrypted cache."
        }
        if case .ready = canonicalSync.localCompositionStatus,
           !canonicalSync.canRecomposeLocally {
            return "Sync once and resolve pending planner changes before composing on this Mac."
        }
        return canonicalSync.localCompositionStatus.message
    }

    private var provenanceSummary: String? {
        guard let provenance = store.localScheduleCompositionProvenance else { return nil }
        let sourceCount = provenance.sourceItemRevisions.count
        return "Composed \(scheduleDateTimeLabel(provenance.generatedAt, timezoneName: provenance.timezoneName)) · through \(scheduleDateTimeLabel(provenance.horizonEnd, timezoneName: provenance.timezoneName)) · \(sourceCount) source revision\(sourceCount == 1 ? "" : "s") · \(provenance.timezoneName)"
    }

    private var statusColor: Color {
        switch canonicalSync.localCompositionStatus {
        case .ready:
            store.localScheduleCompositionProvenance == nil ? .secondary : .green
        case .composing: .blue
        case .composed: .green
        case .failed: .red
        }
    }

    private var statusIsFailure: Bool {
        if case .failed = canonicalSync.localCompositionStatus { return true }
        return false
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
                Text(scheduleDayLabel(
                    Date.now,
                    timezoneName: store.schedulePresentationTimezoneName
                ))
                    .font(.title2.weight(.semibold))
                Text(store.lastScheduleMessage)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            MetricChip(
                value: "\(actionableTodayBlocks.count(where: { $0.status == .completed }))/\(actionableTodayBlocks.count)",
                label: "done",
                symbol: "checkmark"
            )
            MetricChip(
                value: "\(protectedMinutesToday)m",
                label: "protected today",
                symbol: "shield"
            )
            MetricChip(value: scheduleCoverage, label: "schedule coverage", symbol: "chart.pie")
        }
        .padding(20)
    }

    private var actionableTodayBlocks: [ScheduleBlock] {
        store.todaysBlocks.filter { block in
            guard !block.isHardConstraint,
                  block.kind != .event,
                  block.kind != .breakTime else { return false }
            if let itemID = block.sourceItemID {
                return store.canonicalItem(id: itemID)?.isExecutable == true
            }
            return block.isLocallyAuthored
        }
    }

    private var scheduleCoverage: String {
        guard let score = canonicalSync.lastLocalCompositionScore
                ?? canonicalSync.lastPreview?.plan.score else { return "—" }
        let total = score.scheduledMinutes + score.unscheduledMinutes
        guard total > 0 else { return "100%" }
        return "\(Int((Double(score.scheduledMinutes) / Double(total) * 100).rounded()))%"
    }

    private var protectedMinutesToday: Int {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = scheduleTimeZone(store.scheduleProfile.timezoneName)
        let foundationWeekday = calendar.component(.weekday, from: Date.now)
        let isoWeekday = ((foundationWeekday + 5) % 7) + 1
        guard let weekday = ScheduleWeekday(rawValue: isoWeekday),
              let day = store.scheduleProfile.protectedTime.first(where: {
                  $0.weekday == weekday
              }), day.isEnabled else { return 0 }
        return day.windows.reduce(0) { $0 + $1.durationMinutes }
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
                Text(scheduleTimeLabel(
                    block.start,
                    timezoneName: store.schedulePresentationTimezoneName
                ))
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
                    if !isExternalFixed {
                        Label(block.energy.title, systemImage: "bolt")
                    }
                    if block.isFlexible {
                        Label("Flexible", systemImage: "arrow.left.and.right")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)

                if !isExternalFixed {
                    if block.sourceItemID != nil {
                        AuthoritativeExecutionControls(
                            block: block,
                            accessibilityScope: "timeline-inline"
                        )
                            .controlSize(.small)
                    } else if block.status == .active || block.status == .paused {
                        HStack {
                            Button(block.status == .active ? "Pause" : "Resume") {
                                block.status == .active ? store.pauseActive() : store.start(block.id)
                            }
                            .buttonStyle(.borderedProminent)
                            Button("Complete") { store.complete(block.id) }
                            WillDoLaterButton(
                                block: block,
                                title: "Will do later",
                                accessibilityScope: "timeline-inline-local"
                            )
                        }
                        .controlSize(.small)
                        .disabled(!store.canMutate(block))
                    }
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
            if !isExternalFixed {
                if block.sourceItemID != nil {
                    AuthoritativeExecutionContextMenu(block: block)
                } else {
                    Button("Start") { store.start(block.id) }.disabled(!store.canMutate(block))
                    Button("Mark Complete") { store.complete(block.id) }.disabled(!store.canMutate(block))
                    Divider()
                    WillDoLaterButton(
                        block: block,
                        title: "Will do later",
                        accessibilityScope: "timeline-context-local"
                    )
                    Button("Skip") { store.skip(block.id) }.disabled(!store.canMutate(block))
                }
            }
        }
        .privacySensitive(block.isSensitive)
    }

    private var isExternalFixed: Bool {
        block.previewKind == "external_fixed"
    }

    private var sensitivityHelp: String {
        if isExternalFixed {
            return "Sensitive fixed or busy time; details stay hidden from assistant context."
        }
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

struct AuthoritativeExecutionControls: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    let block: ScheduleBlock
    var includesCustomPause = true
    let accessibilityScope: String

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
                    WillDoLaterButton(
                        block: block,
                        title: "Will do later",
                        accessibilityScope: "\(accessibilityScope)-scheduled-more"
                    )
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
                WillDoLaterButton(
                    block: block,
                    title: "Will do later",
                    accessibilityScope: "\(accessibilityScope)-active"
                )
            case .paused:
                Button("Resume") {
                    Task { await executionSync.resume(block.id) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canControlOpenLease)
                .accessibilityIdentifier("execution.resume.\(block.id.uuidString.lowercased())")
                terminalMenu
                    .disabled(!canControlOpenLease)
                WillDoLaterButton(
                    block: block,
                    title: "Will do later",
                    accessibilityScope: "\(accessibilityScope)-paused"
                )
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
            && !executionSync.habitExecutionStartIsBlocked
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

struct WillDoLaterButton: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var presenter: WillDoLaterPresenter
    let block: ScheduleBlock
    let title: String
    let accessibilityScope: String

    var body: some View {
        Button(title) {
            presenter.present(blockID: block.id, initialMoveStart: initialMoveStart)
        }
        .disabled(!isEligible)
        .accessibilityIdentifier(
            "execution.defer.\(accessibilityScope).\(block.id.uuidString.lowercased())"
        )
    }

    private var isEligible: Bool {
        guard !executionSync.isSyncing,
              !canonicalSync.isSyncing,
              store.executionState.pendingCommand == nil else { return false }
        if block.sourceItemID != nil,
           block.status == .active || block.status == .paused {
            guard canonicalSync.isConfigured,
                  let active = executionSync.activeSession,
                  executionSession(active, matches: block) else { return false }
            let matchingRestoredIntent = store.pendingExecutionDeferIntent.map {
                $0.hasValidShape
                    && $0.focusedBlockID == block.id
                    && $0.identity.matches(active)
                    && $0.moveStart > Date.now
            } == true
            return store.canMutatePlan
                || (matchingRestoredIntent
                    && store.canPersistPlan
                    && !store.isCanonicalSyncLocked)
        }
        guard store.canMutatePlan,
              block.isFlexible,
              !block.isHardConstraint,
              block.occurrenceID == nil
                || block.recurrenceMoveSource?.canAuthorizeOccurrenceMove == true,
              block.previewKind != "external_fixed" else { return false }
        if block.sourceItemID == nil {
            return [.scheduled, .active, .paused].contains(block.status)
                && store.canMutate(block)
        }
        switch block.status {
        case .scheduled:
            guard executionSync.activeSession == nil,
                  canonicalSync.isConfigured,
                  block.previewKind != "pinned",
                  store.canMutate(block),
                  let itemID = block.sourceItemID,
                  let item = store.canonicalItem(id: itemID) else { return false }
            let seriesItemID = block.recurrenceSeriesItemID ?? itemID
            let seriesItem = store.canonicalItem(id: seriesItemID)
            return item.revision == block.sourceItemRevision
                && (block.occurrenceID == nil
                    ? item.supportsCanonicalAuthoringReplacement
                    : seriesItem.map {
                        block.recurrenceMoveSource?.canAuthorizeOccurrenceMove(for: $0) == true
                    } == true
                        && WillDoLaterTiming.proposedWindow(
                            for: block,
                            moveStart: preset(hours: 1),
                            allBlocks: store.blocks,
                            accumulatedSeconds: nil
                        ) != nil)
                && store.canonicalAuthoringMutation(itemID: itemID) == nil
                && store.canonicalAuthoringMutation(itemID: seriesItemID) == nil
        case .active, .paused:
            return false
        default:
            return false
        }
    }

    private var referenceDate: Date {
        block.status == .scheduled ? max(Date.now, block.start) : Date.now
    }

    private var initialMoveStart: Date {
        if block.status == .active || block.status == .paused,
           let intent = store.pendingExecutionDeferIntent,
           intent.hasValidShape,
           intent.focusedBlockID == block.id,
           intent.moveStart > Date.now {
            return intent.moveStart
        }
        return preset(hours: 1)
    }

    private var minimumMoveStart: Date {
        block.status == .active || block.status == .paused
            ? WillDoLaterTiming.minimumExecutionMoveStart(after: referenceDate)
            : referenceDate.addingTimeInterval(60)
    }

    private func preset(hours: Int) -> Date {
        let proposed = referenceDate.addingTimeInterval(TimeInterval(hours * 3_600))
        return block.status == .active || block.status == .paused
            ? WillDoLaterTiming.roundedUpToExecutionSlot(proposed)
            : WillDoLaterTiming.roundedUpToMinute(proposed)
    }
}

private struct WillDoLaterSheet: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var presenter: WillDoLaterPresenter
    let request: WillDoLaterPresenter.Request

    @State private var moveStart: Date
    @State private var approvedOverlapRisk: DayWeaveMoveRiskEnvelope?
    @State private var approvedDeadlineRisk: DayWeaveMoveRiskEnvelope?
    @State private var approvedSourceRisk: DayWeaveMoveRiskEnvelope?
    @State private var isSubmitting = false
    @State private var submissionError: String?

    init(request: WillDoLaterPresenter.Request) {
        self.request = request
        _moveStart = State(initialValue: request.initialMoveStart)
    }

    var body: some View {
        Group {
            if let block {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Will do later").font(.title2.weight(.semibold))
                        Text(explanation(for: block))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    HStack(spacing: 8) {
                        Button("In 1 hour") { moveStart = preset(hours: 1, for: block) }
                        Button("In 3 hours") { moveStart = preset(hours: 3, for: block) }
                        Button("Tomorrow morning") {
                            moveStart = tomorrowMorning(for: block)
                        }
                    }
                    .buttonStyle(.bordered)
                    DatePicker(
                        block.sourceItemID == nil || block.status != .scheduled
                            ? "Move remaining work to" : "Allow scheduling from",
                        selection: $moveStart,
                        in: minimumMoveStart(for: block)...,
                        displayedComponents: [.date, .hourAndMinute]
                    )
                    .environment(\.timeZone, profileTimeZone)

                    if isExecutionMove(block) {
                        serverDeferAssessmentNotice(for: block)
                    } else {
                        deadlineNotice(for: block)
                        if let coverageIssue = exactMoveCoverageIssue(for: block) {
                            Label(
                                coverageIssue,
                                systemImage: "calendar.badge.exclamationmark"
                            )
                            .font(.caption)
                            .foregroundStyle(.red)
                        }
                        if !fixedConflicts(for: block).isEmpty {
                            overlapNotice(for: block)
                        }
                        if sourceRequiresOverride(block) {
                            sourceOverrideNotice(for: block)
                        }
                    }
                    if let submissionError {
                        Label(submissionError, systemImage: "exclamationmark.triangle.fill")
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                    HStack {
                        Spacer()
                        Button("Cancel") { cancel() }
                            .keyboardShortcut(.cancelAction)
                        Button(submitTitle(for: block)) { submit(block) }
                            .buttonStyle(.borderedProminent)
                            .keyboardShortcut(.defaultAction)
                            .disabled(!isValid(for: block) || isSubmitting)
                    }
                }
                .padding(24)
                .frame(width: 520)
                .privacySensitive(block.isSensitive)
                .onChange(of: moveStart) {
                    approvedOverlapRisk = nil
                    approvedDeadlineRisk = nil
                    approvedSourceRisk = nil
                    submissionError = nil
                }
            } else {
                VStack(spacing: 14) {
                    Text("This schedule block is no longer available.")
                    Button("Close") { cancel() }
                        .keyboardShortcut(.cancelAction)
                }
                .padding(24)
            }
        }
        .interactiveDismissDisabled(matchingPendingIntent != nil)
    }

    private var block: ScheduleBlock? {
        store.blocks.first(where: { $0.id == request.blockID })
    }

    private var profileTimeZone: TimeZone {
        scheduleTimeZone(store.schedulePresentationTimezoneName)
    }

    private func referenceDate(for block: ScheduleBlock) -> Date {
        block.status == .scheduled ? max(Date.now, block.start) : Date.now
    }

    private func minimumMoveStart(for block: ScheduleBlock) -> Date {
        let reference = referenceDate(for: block)
        guard isExecutionMove(block) else {
            return reference.addingTimeInterval(60)
        }
        let freshAssessmentMinimum = WillDoLaterTiming.minimumExecutionMoveStart(
            after: reference
        )
        if let intent = matchingPendingIntent,
           intent.moveStart == moveStart,
           intent.moveStart > Date.now {
            return min(intent.moveStart, freshAssessmentMinimum)
        }
        return freshAssessmentMinimum
    }

    private func preset(hours: Int, for block: ScheduleBlock) -> Date {
        let proposed = referenceDate(for: block)
            .addingTimeInterval(TimeInterval(hours * 3_600))
        return isExecutionMove(block)
            ? WillDoLaterTiming.roundedUpToExecutionSlot(proposed)
            : WillDoLaterTiming.roundedUpToMinute(proposed)
    }

    private func tomorrowMorning(for block: ScheduleBlock) -> Date {
        WillDoLaterTiming.tomorrowMorning(
            after: referenceDate(for: block),
            minimum: minimumMoveStart(for: block),
            timezoneName: store.schedulePresentationTimezoneName
        )
    }

    private func explanation(for block: ScheduleBlock) -> String {
        if block.sourceItemID == nil {
            return "Choose the new start for this local block."
        }
        if block.status == .scheduled {
            if block.occurrenceID != nil {
                return "DayWeave will move only this recurring occurrence, recompose it within the shifted window, then publish a fresh schedule."
            }
            return "DayWeave will save this as the canonical earliest start, then compose and publish a fresh schedule."
        }
        return "DayWeave will pause the authoritative timer, preserve its exact unfinished seconds, and publish the replacement before it can be started again."
    }

    private func proposedWindow(for block: ScheduleBlock) -> WillDoLaterMoveWindow? {
        let accumulated: UInt64?
        if let active = executionSync.activeSession,
           executionSession(active, matches: block) {
            accumulated = active.accumulatedSeconds
        } else {
            accumulated = nil
        }
        return WillDoLaterTiming.proposedWindow(
            for: block,
            moveStart: moveStart,
            allBlocks: store.blocks,
            accumulatedSeconds: accumulated
        )
    }

    private func reviewedDeadlines(
        for block: ScheduleBlock
    ) -> Set<DayWeaveMoveDeadlineIdentity>? {
        guard block.sourceItemID != nil else { return [] }
        return DayWeaveMoveDeadlinePolicy.identities(
            for: block,
            movingWholeOccurrence: block.status == .scheduled
                && block.occurrenceID != nil,
            allBlocks: store.blocks,
            canonicalItems: store.canonicalItems
        )
    }

    private func hasDeadlineConflict(
        _ deadline: DayWeaveMoveDeadlineIdentity,
        block: ScheduleBlock
    ) -> Bool {
        guard let window = proposedWindow(for: block) else { return true }
        return WillDoLaterTiming.crossedDeadlines(
            [deadline],
            window: window
        ).contains(deadline)
    }

    @ViewBuilder
    private func deadlineNotice(for block: ScheduleBlock) -> some View {
        if let deadlines = reviewedDeadlines(for: block) {
            let ordered = deadlines.sorted {
                if $0.boundary.date != $1.boundary.date {
                    return $0.boundary.date < $1.boundary.date
                }
                return $0.itemID.uuidString < $1.itemID.uuidString
            }
            let crossed = ordered.filter { hasDeadlineConflict($0, block: block) }
            if let deadline = crossed.first ?? ordered.first {
                let label = scheduleDateTimeLabel(
                    deadline.boundary.date,
                    timezoneName: store.schedulePresentationTimezoneName
                )
                let conflict = !crossed.isEmpty
                let suffix = (conflict ? crossed.count : ordered.count) > 1
                    ? " and \((conflict ? crossed.count : ordered.count) - 1) other constraint(s)"
                    : ""
                if conflict {
                Label(
                        canOverrideDeadlines(Set(crossed), for: block)
                            ? deadlineConflictText(
                                for: block,
                                label: label,
                                suffix: suffix,
                                cannotOverride: false
                              )
                            : deadlineConflictText(
                                for: block,
                                label: label,
                                suffix: suffix,
                                cannotOverride: true
                              ),
                        systemImage: "calendar.badge.exclamationmark"
                    )
                    .font(.caption)
                    .foregroundStyle(.red)
                    if canOverrideDeadlines(Set(crossed), for: block) {
                        Toggle(
                            block.occurrenceID == nil
                                && crossed.contains(where: { $0.boundary.isCanonicalField })
                                ? "Move anyway and extend the latest finish"
                                : block.occurrenceID == nil
                                    ? "Move anyway despite the latest finish"
                                    : "Move anyway despite the possible latest-finish violation",
                            isOn: deadlineApprovalBinding(for: block)
                        )
                        .font(.caption)
                    }
                } else {
                    Label(
                        "Latest finish: \(label)\(suffix)",
                        systemImage: "calendar.badge.clock"
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }
        } else {
            Label(
                "This item's latest-finish constraint is malformed or ambiguous. Fix it before moving the work.",
                systemImage: "calendar.badge.exclamationmark"
            )
            .font(.caption)
            .foregroundStyle(.red)
        }
    }

    private func fixedConflicts(for block: ScheduleBlock) -> [ScheduleBlock] {
        guard moveIsExact(for: block), let window = proposedWindow(for: block) else {
            return []
        }
        return WillDoLaterTiming.fixedConflicts(with: window, in: store.blocks)
    }

    private func moveIsExact(for block: ScheduleBlock) -> Bool {
        WillDoLaterTiming.usesExactPlacement(for: block)
    }

    private func exactMoveCoverageIssue(for block: ScheduleBlock) -> String? {
        guard moveIsExact(for: block), let window = proposedWindow(for: block) else {
            return nil
        }
        return store.exactMoveWindowCoverageIssue(
            for: block,
            start: window.start,
            end: window.end
        )
    }

    @ViewBuilder
    private func overlapNotice(for block: ScheduleBlock) -> some View {
        let conflicts = fixedConflicts(for: block)
        let names = conflicts.prefix(3).map { conflict in
            WillDoLaterTiming.conflictLabel(
                conflict,
                timezoneName: store.schedulePresentationTimezoneName
            )
        }.joined(separator: ", ")
        Label(
            block.occurrenceID == nil
                ? "This exact move overlaps fixed or protected time: \(names)."
                : "The shifted occurrence window intersects fixed or protected time: \(names). The occurrence will be recomposed within that window.",
            systemImage: "exclamationmark.triangle.fill"
        )
        .font(.caption)
        .foregroundStyle(.orange)
        .privacySensitive(conflicts.contains(where: \.isSensitive))
        Toggle("Move anyway despite the overlap", isOn: overlapApprovalBinding(for: block))
            .font(.caption)
    }

    @ViewBuilder
    private func sourceOverrideNotice(for block: ScheduleBlock) -> some View {
        Label(
            "This session came from an explicitly pinned placement. Moving its unfinished work will replace that placement.",
            systemImage: "pin.slash"
        )
        .font(.caption)
        .foregroundStyle(.orange)
        Toggle(
            "Move anyway despite the pinned placement",
            isOn: sourceApprovalBinding(for: block)
        )
        .font(.caption)
    }

    private func isValid(for block: ScheduleBlock) -> Bool {
        guard moveStart >= minimumMoveStart(for: block),
              moveStart.timeIntervalSinceReferenceDate.isFinite else {
            return false
        }
        if isExecutionMove(block) {
            return DayWeaveExecutionDeferTiming.isAligned(moveStart)
        }
        guard block.occurrenceID == nil
                || block.recurrenceMoveSource?.canAuthorizeOccurrenceMove == true else {
            return false
        }
        guard proposedWindow(for: block) != nil,
              exactMoveCoverageIssue(for: block) == nil else { return false }
        guard let risk = currentRisk(for: block) else { return false }
        guard let window = proposedWindow(for: block) else { return false }
        let crossedDeadlines = WillDoLaterTiming.crossedDeadlines(
            risk.deadlines,
            window: window
        )
        if !crossedDeadlines.isEmpty {
            guard canOverrideDeadlines(Set(crossedDeadlines), for: block),
                  approvedDeadlineRisk == risk else { return false }
        }
        if !risk.fixedConflicts.isEmpty && approvedOverlapRisk != risk { return false }
        if risk.sourceRequiresOverride && approvedSourceRisk != risk { return false }
        return true
    }

    private func submit(_ block: ScheduleBlock) {
        guard isValid(for: block) else { return }
        let selectedStart = moveStart
        submissionError = nil
        if block.sourceItemID == nil {
            store.doLater(block.id, moveStart: selectedStart)
            presenter.dismiss(request.id)
            return
        }
        isSubmitting = true
        Task {
            if block.status == .active || block.status == .paused {
                let outcome: ExecutionSyncOutcome
                if let assessment = serverAssessment(for: block) {
                    outcome = await executionSync.approveDeferredWork(
                        block.id,
                        assessmentDigest: assessment.assessmentDigest
                    )
                } else {
                    outcome = await executionSync.deferWork(
                        block.id,
                        moveStart: selectedStart
                    )
                }
                if outcome == .success {
                    presenter.dismiss(request.id)
                    _ = await canonicalSync.syncThroughFreshComposition()
                } else if outcome == .approvalRequired {
                    submissionError = nil
                } else {
                    submissionError = "The exact move was not accepted (\(String(describing: outcome))). Review the current session and try again."
                }
            } else {
                guard let reviewedRisk = currentRisk(for: block) else {
                    submissionError = "The current scheduling constraints are unavailable."
                    isSubmitting = false
                    return
                }
                let approvedDeadlineConflict = approvedDeadlineRisk == reviewedRisk
                let approvedFixedConflicts = approvedOverlapRisk == reviewedRisk
                let succeeded = await canonicalSync.moveCanonicalWorkLater(
                    block.id,
                    earliestStart: selectedStart,
                    reviewedRisk: reviewedRisk,
                    allowDeadlineConflict: approvedDeadlineConflict,
                    allowFixedConflicts: approvedFixedConflicts
                )
                if succeeded {
                    presenter.dismiss(request.id)
                } else {
                    submissionError = "The canonical move was not published. The previous schedule remains authoritative."
                }
            }
            isSubmitting = false
        }
    }

    private func isExecutionMove(_ block: ScheduleBlock) -> Bool {
        block.sourceItemID != nil && (block.status == .active || block.status == .paused)
    }

    private var matchingPendingIntent: DayWeavePendingExecutionDeferIntent? {
        guard let intent = store.pendingExecutionDeferIntent,
              intent.hasValidShape,
              intent.focusedBlockID == request.blockID else { return nil }
        return intent
    }

    private func cancel() {
        guard let intent = matchingPendingIntent else {
            presenter.dismiss(request.id)
            return
        }
        let outcome = executionSync.cancelDeferredWork(intent)
        if outcome == .success {
            presenter.dismiss(request.id)
        } else {
            submissionError = "The move is already being sent and must be reconciled before it can be closed."
        }
    }

    private func serverAssessment(for block: ScheduleBlock) -> DayWeaveDeferAssessment? {
        guard isExecutionMove(block),
              let intent = store.pendingExecutionDeferIntent,
              intent.focusedBlockID == block.id,
              intent.moveStart == moveStart,
              intent.approvalIsRequired else { return nil }
        return executionSync.pendingDeferApproval
    }

    private func submitTitle(for block: ScheduleBlock) -> String {
        if serverAssessment(for: block) != nil {
            return "Approve assessed conflicts and move"
        }
        return isExecutionMove(block) ? "Assess and move" : "Move work"
    }

    @ViewBuilder
    private func serverDeferAssessmentNotice(for block: ScheduleBlock) -> some View {
        if let assessment = serverAssessment(for: block) {
            VStack(alignment: .leading, spacing: 10) {
                Label(
                    "The server found \(assessment.violations.count) scheduling conflict(s). Approval applies only to this exact assessment and expires shortly.",
                    systemImage: "checkmark.shield.fill"
                )
                .font(.caption.weight(.semibold))
                .foregroundStyle(.orange)

                ForEach(
                    Array(assessment.violations.enumerated()),
                    id: \.offset
                ) { _, violation in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(violation.code.title)
                            .font(.caption.weight(.semibold))
                        Text(violation.message)
                            .font(.caption)
                        Text(
                            "\(scheduleDateTimeLabel(violation.start, timezoneName: store.schedulePresentationTimezoneName)) – \(scheduleDateTimeLabel(violation.end, timezoneName: store.schedulePresentationTimezoneName))"
                        )
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                    }
                }
            }
            .privacySensitive(true)
            .accessibilityIdentifier("execution.defer.assessment.\(block.id.uuidString.lowercased())")
        } else if let intent = store.pendingExecutionDeferIntent,
                  intent.focusedBlockID == block.id,
                  intent.moveStart == moveStart,
                  intent.assessment != nil {
            Label(
                "The prior assessment expired or changed. DayWeave will request fresh evidence; prior approval cannot carry forward.",
                systemImage: "arrow.clockwise.circle"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        } else {
            Label(
                "DayWeave will pause first, then ask the server to assess the exact remaining work against the current private schedule.",
                systemImage: "shield.lefthalf.filled"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }

    private func currentRisk(for block: ScheduleBlock) -> DayWeaveMoveRiskEnvelope? {
        guard let window = proposedWindow(for: block),
              let deadlines = reviewedDeadlines(for: block) else { return nil }
        let envelope = DayWeaveMoveRiskEnvelope(
            moveStart: window.start,
            moveEnd: window.end,
            deadlines: deadlines,
            fixedConflicts: Set(
                fixedConflicts(for: block).map(DayWeaveMoveConflictIdentity.init)
            ),
            sourceRequiresOverride: sourceRequiresOverride(block)
        )
        return envelope.hasValidShape ? envelope : nil
    }

    private func canOverrideDeadlines(
        _ deadlines: Set<DayWeaveMoveDeadlineIdentity>,
        for block: ScheduleBlock
    ) -> Bool {
        let hard = deadlines.filter(\.boundary.isHard)
        if hard.isEmpty { return true }
        return block.status == .scheduled && block.occurrenceID == nil
            && hard.allSatisfy(\.boundary.isCanonicalField)
    }

    private func deadlineConflictText(
        for block: ScheduleBlock,
        label: String,
        suffix: String,
        cannotOverride: Bool
    ) -> String {
        let subject = block.occurrenceID == nil
            ? "The moved work would finish"
            : "Fresh composition could place moved work"
        let tail = cannotOverride ? "; this move cannot safely override it." : "."
        return "\(subject) after a\(cannotOverride ? " hard" : "") latest finish (\(label))\(suffix)\(tail)"
    }

    private func deadlineApprovalBinding(for block: ScheduleBlock) -> Binding<Bool> {
        Binding(
            get: {
                guard let risk = currentRisk(for: block) else { return false }
                return approvedDeadlineRisk == risk
            },
            set: { approved in
                approvedDeadlineRisk = approved ? currentRisk(for: block) : nil
            }
        )
    }

    private func overlapApprovalBinding(for block: ScheduleBlock) -> Binding<Bool> {
        Binding(
            get: {
                guard let risk = currentRisk(for: block) else { return false }
                return approvedOverlapRisk == risk
            },
            set: { approved in
                approvedOverlapRisk = approved ? currentRisk(for: block) : nil
            }
        )
    }

    private func sourceRequiresOverride(_ block: ScheduleBlock) -> Bool {
        (block.status == .active || block.status == .paused)
            && block.previewKind == "pinned"
    }

    private func sourceApprovalBinding(for block: ScheduleBlock) -> Binding<Bool> {
        Binding(
            get: {
                guard let risk = currentRisk(for: block) else { return false }
                return approvedSourceRisk == risk
            },
            set: { approved in
                approvedSourceRisk = approved ? currentRisk(for: block) : nil
            }
        )
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
                WillDoLaterButton(
                    block: block,
                    title: "Will do later",
                    accessibilityScope: "execution-context-scheduled"
                )
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
                WillDoLaterButton(
                    block: block,
                    title: "Will do later",
                    accessibilityScope: "execution-context-open"
                )
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
            && !executionSync.habitExecutionStartIsBlocked
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
                    .disabled(
                        !store.canKeepLatestCanonicalItem(forExecutionSession: sessionID)
                    )
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

private struct GooglePublicationTarget: Identifiable {
    let accountID: UUID
    let collectionID: UUID
    let displayName: String

    var id: String { "\(accountID.uuidString):\(collectionID.uuidString)" }
}

private struct GooglePublicationCandidate {
    let itemID: UUID
    let revision: UInt64
    let entityKind: GoogleOutboundEntityKind
    let operation: GoogleOutboundOperation
    let isAllDay: Bool
    let isTentative: Bool
    let isBusy: Bool

    var serviceTitle: String {
        entityKind == .calendarEvent ? "Google Calendar" : "Google Tasks"
    }

    var itemTitle: String {
        entityKind == .calendarEvent ? "event" : "task"
    }
}

private extension JSONValue {
    /// Mirrors the server's narrow Calendar-write eligibility boundary. The
    /// general canonical validator checks the timestamps and supported fields;
    /// this additional check proves the event is app-authored and owned.
    var isOwnedGooglePublishableFirmBlock: Bool {
        guard supportsCanonicalAuthoringConstraints,
              case let .object(constraints) = self,
              Set(constraints.keys) == ["dayweave_firm_block"],
              case let .object(firmBlock)? = constraints["dayweave_firm_block"],
              case .bool(true)? = firmBlock["owned"],
              case .string? = firmBlock["starts_at"],
              case .string? = firmBlock["ends_at"] else {
            return false
        }
        return true
    }

    var googleFirmBlockPublicationTraits: (
        isAllDay: Bool,
        isTentative: Bool,
        isBusy: Bool
    )? {
        guard isOwnedGooglePublishableFirmBlock,
              case let .object(constraints) = self,
              case let .object(firmBlock)? = constraints["dayweave_firm_block"] else {
            return nil
        }
        func flag(_ key: String, default fallback: Bool) -> Bool {
            guard case let .bool(value)? = firmBlock[key] else { return fallback }
            return value
        }
        return (
            flag("all_day", default: false),
            flag("tentative", default: false),
            flag("busy", default: true)
        )
    }
}

private struct CanonicalInboxInspector: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var googleIntegration: GoogleIntegrationStore
    @EnvironmentObject private var googleOutbound: GoogleOutboundStore
    @State private var googleReviewIsPresented = false

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
            + presentation.active
            + presentation.completed
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
                            value: row.durationDescription
                        )
                        if let timingTitle = row.timingTitle,
                           let timing = row.timingDescription(
                               timezoneName: store.scheduleProfile.timezoneName
                           ) {
                            LabeledContent(timingTitle, value: timing)
                        } else {
                            LabeledContent("Deadline", value: "None")
                        }
                        if let blocker = row.blockedDescription {
                            LabeledContent("Blocked", value: blocker)
                                .privacySensitive(row.isSensitive)
                        }
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

                    if !row.dependencyCauses.isEmpty {
                        InspectorSection(title: row.blockingDependencyCauses.isEmpty
                            ? "Dependencies"
                            : "Dependency blockers") {
                            ForEach(row.dependencyCauses) { cause in
                                CanonicalDependencyCauseRow(
                                    cause: cause,
                                    open: cause.isAvailable
                                        ? { store.selectCanonicalItem(cause.predecessorID) }
                                        : nil
                                )
                            }
                        }
                    }

                    if row.hasOpaqueDependencies {
                        Label(
                            "Dependency details require a newer DayWeave version and remain read-only.",
                            systemImage: "lock.shield"
                        )
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .accessibilityIdentifier("canonical-dependency-opaque")
                    }

                    if row.kind == .event || row.kind == .task {
                        InspectorSection(
                            title: row.kind == .event ? "Google Calendar" : "Google Tasks"
                        ) {
                            googlePublicationControls(for: row)
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
            .privacySensitive(usesSensitivePresentation(row))
            .id(row.id)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(row.accessibilitySummary)
            .accessibilityIdentifier("canonical-inbox.inspector")
            .sheet(isPresented: $googleReviewIsPresented) {
                GoogleOutboundReviewSheet(
                    fallbackTitle: row.title
                )
                .environmentObject(googleOutbound)
            }
        } else {
            ContentUnavailableView {
                Label("Select an Inbox item", systemImage: "tray.full")
            } description: {
                Text("Choose a captured, planned, conflicted, or recently deleted item to inspect it here.")
            }
            .accessibilityIdentifier("canonical-inbox.inspector.empty")
        }
    }

    @ViewBuilder
    private func googlePublicationControls(
        for row: CanonicalInboxPresentation.Row
    ) -> some View {
        if let candidate = googlePublicationCandidate(for: row) {
            let eligibleTargets = writableGoogleTargets(for: candidate)
            if let preview = googleOutbound.preview,
               preview.itemID == candidate.itemID,
               preview.itemRevision == candidate.revision,
               preview.entityKind == candidate.entityKind,
               preview.operation == candidate.operation {
                Button("Review prepared \(candidate.serviceTitle) change") {
                    googleReviewIsPresented = true
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("google.outbound.review-open")
            } else if googleOutbound.hasPendingRecovery {
                Label(
                    "A saved Google operation must be recovered from the sidebar before another item can be published.",
                    systemImage: "arrow.up.circle.fill"
                )
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            } else if eligibleTargets.isEmpty {
                Label(
                    candidate.entityKind == .calendarEvent
                        ? "No selected Publish calendar permits this event type. Check Calendar ownership and the all-day, tentative, or free publication switches in Settings."
                        : "No selected Publish task list is available. Enable Google Tasks publishing, then choose a writable list in Settings.",
                    systemImage: candidate.entityKind == .calendarEvent
                        ? "calendar.badge.exclamationmark" : "checklist"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            } else {
                Menu(candidate.operation == .delete
                    ? "Preview removal from Google"
                    : "Preview in \(candidate.serviceTitle)") {
                    ForEach(eligibleTargets) { target in
                        Button(target.displayName) {
                            prepareGooglePreview(candidate, target: target)
                        }
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(googleOutbound.status.isWorking || googleIntegration.isBusy)
                .accessibilityIdentifier("google.outbound.preview")
            }

            Text(candidate.operation == .delete
                ? "The server permits removal only when this exact trashed DayWeave \(candidate.itemTitle) still has a DayWeave-owned mapping and retained version in the selected destination. The provider deletion is reviewed first."
                : candidate.entityKind == .calendarEvent
                    ? "Only this app-authored fixed event is eligible. DayWeave shows the exact private Google payload and asks again before queueing it."
                    : "Only a synced, app-authored, non-recurring task is eligible. Title, notes, completion state, and due date may be sent; DayWeave-only planning metadata stays local.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Text(googleOutbound.status.message)
                .font(.caption2)
                .foregroundStyle(googleOutboundStatusColor)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("google.outbound.status")
        } else {
            Label(
                row.kind == .task
                    ? "Sync an app-authored, non-recurring task before publishing it. Imported, skipped, cancelled, locally queued, or unsupported tasks cannot be sent."
                    : "Sync this app-authored fixed event before publishing it. Imported, flexible, locally queued, or unrestorable events cannot be sent to Google.",
                systemImage: "lock.shield"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func writableGoogleTargets(
        for candidate: GooglePublicationCandidate
    ) -> [GooglePublicationTarget] {
        googleIntegration.accounts
            .filter {
                $0.status == .active
                    && (candidate.entityKind == .calendarEvent
                        ? googleIntegration.hasCalendarPublishingScope(for: $0)
                        : googleIntegration.hasTasksPublishingScope(for: $0))
            }
            .flatMap { account in
                (googleIntegration.collectionsByAccount[account.id] ?? []).compactMap {
                    collection in
                    guard collection.kind == (candidate.entityKind == .calendarEvent
                            ? .calendar : .taskList),
                          collection.selected,
                          collection.syncRole == .writable,
                          !collection.providerDeleted else {
                        return nil
                    }
                    if candidate.entityKind == .calendarEvent {
                        guard candidate.operation == .delete
                                || ((!candidate.isAllDay
                                        || collection.calendarPolicy.publishAllDay)
                                    && (!candidate.isTentative
                                        || collection.calendarPolicy.publishTentative)
                                    && (candidate.isBusy
                                        || collection.calendarPolicy.publishFree)),
                              let access = collection.providerAccessRole?.lowercased(),
                              access == "owner" || access == "writer" else {
                            return nil
                        }
                    }
                    return GooglePublicationTarget(
                        accountID: account.id,
                        collectionID: collection.id,
                        displayName: "\(account.displayLabel) · \(collection.displayName)"
                    )
                }
            }
            .sorted { $0.displayName.localizedCaseInsensitiveCompare($1.displayName)
                == .orderedAscending }
    }

    private func googlePublicationCandidate(
        for row: CanonicalInboxPresentation.Row
    ) -> GooglePublicationCandidate? {
        if row.source == .canonical,
           row.syncState == .synced,
           let item = store.canonicalItems.first(where: { $0.id == row.itemID }),
           item.deletedAt == nil,
           item.kind == .event,
           let traits = item.flexibleConstraints.googleFirmBlockPublicationTraits {
            return GooglePublicationCandidate(
                itemID: item.id,
                revision: item.revision,
                entityKind: .calendarEvent,
                operation: .upsert,
                isAllDay: traits.isAllDay,
                isTentative: traits.isTentative,
                isBusy: traits.isBusy
            )
        }
        if row.source == .recentTrash,
           row.syncState == .synced,
           let entry = store.canonicalTrash.first(where: { $0.id == row.itemID }),
           let item = entry.lastKnownItem,
           item.kind == .event,
           let traits = item.flexibleConstraints.googleFirmBlockPublicationTraits {
            return GooglePublicationCandidate(
                itemID: entry.id,
                revision: entry.revision,
                entityKind: .calendarEvent,
                operation: .delete,
                isAllDay: traits.isAllDay,
                isTentative: traits.isTentative,
                isBusy: traits.isBusy
            )
        }
        if row.source == .canonical,
           row.syncState == .synced,
           let item = store.canonicalItems.first(where: { $0.id == row.itemID }),
           item.isEligibleForGoogleTaskPublication(deleted: false) {
            return GooglePublicationCandidate(
                itemID: item.id,
                revision: item.revision,
                entityKind: .task,
                operation: .upsert,
                isAllDay: false,
                isTentative: false,
                isBusy: false
            )
        }
        if row.source == .recentTrash,
           row.syncState == .synced,
           let entry = store.canonicalTrash.first(where: { $0.id == row.itemID }),
           let item = entry.lastKnownItem,
           item.isEligibleForGoogleTaskPublication(deleted: true) {
            return GooglePublicationCandidate(
                itemID: entry.id,
                revision: entry.revision,
                entityKind: .task,
                operation: .delete,
                isAllDay: false,
                isTentative: false,
                isBusy: false
            )
        }
        return nil
    }

    private func prepareGooglePreview(
        _ candidate: GooglePublicationCandidate,
        target: GooglePublicationTarget
    ) {
        Task {
            let prepared = await googleOutbound.preparePreview(
                accountID: target.accountID,
                collectionID: target.collectionID,
                itemID: candidate.itemID,
                expectedItemRevision: candidate.revision,
                entityKind: candidate.entityKind,
                operation: candidate.operation
            )
            if prepared, googleOutbound.preview != nil {
                googleReviewIsPresented = true
            }
        }
    }

    private var googleOutboundStatusColor: Color {
        switch googleOutbound.status {
        case .failed, .recoveryRequired, .expirySafetyDelay, .expired: .orange
        case .accepted: .green
        case .privacyProtected, .idle, .previewing, .awaitingApproval,
             .approving, .enqueueing: .secondary
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
        case .project: "folder"
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

private struct GoogleOutboundReviewSheet: View {
    @EnvironmentObject private var googleOutbound: GoogleOutboundStore
    @Environment(\.dismiss) private var dismiss

    let fallbackTitle: String

    var body: some View {
        NavigationStack {
            Group {
                if let preview = googleOutbound.preview {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 18) {
                            Label(
                                reviewHeading(preview),
                                systemImage: reviewSymbol(preview)
                            )
                            .font(.title3.weight(.semibold))

                            Text("The preview request reached your DayWeave server, but no Google provider change has been queued or sent. Review the exact change below; Approve & Queue creates one short-lived approval and saves the operation to the durable server outbox.")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)

                            InspectorSection(title: "Destination") {
                                LabeledContent(
                                    preview.entityKind == .calendarEvent ? "Calendar" : "Task list",
                                    value: preview.collectionDisplayName
                                )
                                LabeledContent(
                                    "Change",
                                    value: preview.operation == .delete ? "Delete" : "Create or update"
                                )
                                LabeledContent(
                                    "Provider item",
                                    value: preview.providerResourceID == nil
                                        ? (preview.entityKind == .calendarEvent
                                            ? "New private event" : "New task")
                                        : (preview.entityKind == .calendarEvent
                                            ? "Existing DayWeave-owned event"
                                            : "Existing DayWeave-owned task")
                                )
                                LabeledContent(
                                    "Approval expires",
                                    value: preview.expiresAt.formatted(
                                        date: .abbreviated,
                                        time: .shortened
                                    )
                                )
                            }

                            InspectorSection(
                                title: preview.entityKind == .calendarEvent
                                    ? "Visible event details" : "Visible task details"
                            ) {
                                LabeledContent(
                                    "Title",
                                    value: payloadString(
                                        preview.entityKind == .calendarEvent ? "summary" : "title"
                                    ) ?? fallbackTitle
                                )
                                    .privacySensitive(true)
                                if let details = payloadString(
                                    preview.entityKind == .calendarEvent ? "description" : "notes"
                                ), !details.isEmpty {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(preview.entityKind == .calendarEvent
                                            ? "Description" : "Notes")
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                        Text(details)
                                            .textSelection(.enabled)
                                            .privacySensitive(true)
                                    }
                                }
                                if preview.entityKind == .calendarEvent {
                                    if let start = providerBound("start") {
                                        LabeledContent("Start", value: start)
                                    }
                                    if let end = providerBound("end") {
                                        LabeledContent("End", value: end)
                                    }
                                } else {
                                    if let due = payloadString("due") {
                                        LabeledContent("Due", value: due)
                                    }
                                    if let completed = payloadString("completed") {
                                        LabeledContent("Completed", value: completed)
                                    }
                                }
                                if let status = payloadString("status") {
                                    LabeledContent(
                                        "Status",
                                        value: status == "needsAction"
                                            ? "Needs action" : status.capitalized
                                    )
                                }
                                if preview.entityKind == .calendarEvent,
                                   let transparency = payloadString("transparency") {
                                    LabeledContent("Availability", value: transparency.capitalized)
                                }
                                if preview.operation == .delete {
                                    Text(preview.entityKind == .calendarEvent
                                        ? "Google will delete only the mapped event whose private ownership proof and retained ETag still match this reviewed DayWeave item."
                                        : "Google will delete only the mapped task whose retained identity and ETag still match this reviewed DayWeave item.")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }

                            DisclosureGroup("Reviewed technical payload") {
                                Text(prettyProviderPayload(preview.providerPayload))
                                    .font(.system(.caption, design: .monospaced))
                                    .textSelection(.enabled)
                                    .privacySensitive(true)
                                    .padding(.top, 8)
                            }

                            Text(preview.entityKind == .calendarEvent
                                ? "Server-managed private ownership proof values are redacted from this reviewed payload."
                                : "Task identifiers, versions, hierarchy, ordering, and DayWeave-only planning metadata are not included in this reviewed write payload.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)

                            Label(
                                preview.entityKind == .task && preview.operation == .upsert
                                    && preview.providerResourceID == nil
                                    ? "DayWeave keeps an encrypted local recovery copy. A new Google Task is attempted only once; an ambiguous provider result is never repeated blindly and requires reconciliation. Provider credentials remain server-only."
                                    : "DayWeave keeps an encrypted local recovery copy. The server stores the preview and only a hash of the short-lived approval capability; provider credentials remain server-only.",
                                systemImage: "lock.shield.fill"
                            )
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)

                            Text(googleOutbound.status.message)
                                .font(.caption)
                                .foregroundStyle(reviewStatusColor)
                                .fixedSize(horizontal: false, vertical: true)
                                .accessibilityIdentifier("google.outbound.review.status")
                        }
                        .padding(22)
                        .privacySensitive(true)
                    }
                } else {
                    ContentUnavailableView {
                        Label(
                            "Preview unavailable",
                            systemImage: presentedEntityKind == .calendarEvent
                                ? "calendar.badge.exclamationmark" : "checklist"
                        )
                    } description: {
                        Text(googleOutbound.status.message)
                    } actions: {
                        if googleOutbound.hasApprovedRecovery {
                            Button(googleOutbound.status == .expired
                                ? "Check server acceptance"
                                : "Recover approved operation") {
                                Task { _ = await googleOutbound.recoverPendingOperation() }
                            }
                            .disabled(googleOutbound.status.isWorking)
                            .accessibilityIdentifier(
                                "google.outbound.review-empty-check-acceptance"
                            )
                        }
                        if googleOutbound.status == .expired {
                            GoogleExpiredRecoveryDiscardButton(
                                title: "Discard expired recovery",
                                accessibilityIdentifier: "google.outbound.review-empty-discard",
                                onDiscard: { dismiss() }
                            )
                        } else if googleOutbound.hasPendingRecovery,
                                  !googleOutbound.hasApprovedRecovery,
                                  !googleOutbound.status.isWaitingForSafeDiscard {
                            Button("Recover saved operation") {
                                Task { _ = await googleOutbound.recoverPendingOperation() }
                            }
                            .disabled(googleOutbound.status.isWorking)
                        }
                    }
                }
            }
            .navigationTitle(
                presentedEntityKind == .calendarEvent
                    ? "Review Google Calendar change" : "Review Google Tasks change"
            )
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if let confirmation = googleOutbound.approvalConfirmation {
                        Button("Approve & Queue") {
                            Task {
                                if await googleOutbound.approveAndEnqueue(confirmation) {
                                    dismiss()
                                }
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(googleOutbound.status.isWorking)
                        .accessibilityIdentifier("google.outbound.review.approve")
                    } else if googleOutbound.status == .expired {
                        GoogleExpiredRecoveryDiscardButton(
                            title: "Discard Expired",
                            accessibilityIdentifier: "google.outbound.review.discard-expired",
                            onDiscard: { dismiss() }
                        )
                    }
                }
            }
        }
        .frame(minWidth: 620, minHeight: 620)
        .accessibilityIdentifier("google.outbound.review")
    }

    private var presentedEntityKind: GoogleOutboundEntityKind {
        googleOutbound.preview?.entityKind
            ?? googleOutbound.recoveryContext?.entityKind
            ?? .calendarEvent
    }

    private func reviewHeading(_ preview: GoogleOutboundPreview) -> String {
        switch (preview.entityKind, preview.operation) {
        case (.calendarEvent, .upsert): "Publish one private event to Google Calendar"
        case (.calendarEvent, .delete): "Remove one DayWeave event from Google Calendar"
        case (.task, .upsert): "Publish one task to Google Tasks"
        case (.task, .delete): "Remove one DayWeave task from Google Tasks"
        }
    }

    private func reviewSymbol(_ preview: GoogleOutboundPreview) -> String {
        switch (preview.entityKind, preview.operation) {
        case (.calendarEvent, .upsert): "calendar.badge.plus"
        case (.calendarEvent, .delete): "calendar.badge.minus"
        case (.task, .upsert): "checklist"
        case (.task, .delete): "checkmark.circle.badge.xmark"
        }
    }

    private func payloadString(_ key: String) -> String? {
        guard let preview = googleOutbound.preview,
              case let .string(value)? = preview.providerPayload[key] else { return nil }
        return value
    }

    private func providerBound(_ key: String) -> String? {
        guard let preview = googleOutbound.preview,
              case let .object(bound)? = preview.providerPayload[key] else { return nil }
        for candidate in ["dateTime", "date_time", "date"] {
            if case let .string(value)? = bound[candidate] {
                if let timezone = bound["timeZone"].flatMap(jsonString)
                    ?? bound["time_zone"].flatMap(jsonString) {
                    return "\(value) (\(timezone))"
                }
                return value
            }
        }
        return nil
    }

    private func jsonString(_ value: JSONValue) -> String? {
        if case let .string(string) = value { return string }
        return nil
    }

    private func prettyProviderPayload(_ payload: [String: JSONValue]) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(payload),
              let text = String(data: data, encoding: .utf8) else {
            return "The exact payload could not be displayed. Close this review without approving."
        }
        return text
    }

    private var reviewStatusColor: Color {
        switch googleOutbound.status {
        case .failed, .recoveryRequired, .expirySafetyDelay, .expired: .orange
        case .accepted: .green
        case .privacyProtected, .idle, .previewing, .awaitingApproval,
             .approving, .enqueueing: .secondary
        }
    }
}

struct GoogleExpiredRecoveryDiscardButton: View {
    @EnvironmentObject private var googleOutbound: GoogleOutboundStore
    @State private var confirmationIsPresented = false

    let title: String
    let accessibilityIdentifier: String
    let onDiscard: () -> Void

    init(
        title: String,
        accessibilityIdentifier: String,
        onDiscard: @escaping () -> Void = {}
    ) {
        self.title = title
        self.accessibilityIdentifier = accessibilityIdentifier
        self.onDiscard = onDiscard
    }

    var body: some View {
        Button(title, role: .destructive) {
            confirmationIsPresented = true
        }
        .accessibilityIdentifier(accessibilityIdentifier)
        .confirmationDialog(
            "Discard expired \(serviceName) recovery?",
            isPresented: $confirmationIsPresented,
            titleVisibility: .visible
        ) {
            Button("Discard local recovery", role: .destructive) {
                if googleOutbound.discardExpiredRecovery() {
                    onDiscard()
                }
            }
            Button("Keep recovery", role: .cancel) {}
        } message: {
            Text(discardMessage)
        }
    }

    private var discardMessage: String {
        if googleOutbound.recoveryContext?.stage == .approved {
            return "The approval is expired and cannot authorize a new enqueue. However, a prior enqueue response may have been lost, so the server could already be delivering this exact \(serviceName) change. Discarding removes only this Mac's local recovery; verify \(serviceName) or server status before trying the change again."
        }
        if googleOutbound.recoveryContext?.stage == .approvalAttempted {
            return "The one-shot approval response may have been lost, so DayWeave did not request another capability or queue this change. Approval alone does not modify \(serviceName). Discarding removes only this Mac's expired local recovery."
        }
        return "No \(serviceName) provider change was approved by this recovery. Discarding removes only the exact expired local preview record and allows planner sync to continue."
    }

    private var serviceName: String {
        googleOutbound.recoveryContext?.entityKind == .task
            ? "Google Tasks" : "Google Calendar"
    }
}

private struct CanonicalDependencyCauseRow: View {
    let cause: CanonicalDependencyCause
    let open: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: statusSymbol)
                .foregroundStyle(statusColor)
                .frame(width: 18)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 3) {
                Text(cause.title)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Text("\(cause.statusDescription) · \(cause.strength.title)")
                    .font(.caption)
                    .foregroundStyle(statusColor)
                Text(cause.requirementDescription)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if let open {
                Button("Open", action: open)
                    .buttonStyle(.bordered)
                    .controlSize(.small)
                    .accessibilityLabel("Open predecessor \(cause.title)")
            }
        }
        .padding(.vertical, 3)
        .privacySensitive(cause.isSensitive && !cause.isTitleRedacted)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(
            "canonical-dependency-cause.\(cause.predecessorID.uuidString.lowercased())"
        )
    }

    private var statusSymbol: String {
        if cause.isSatisfied { return "checkmark.circle.fill" }
        if cause.isBlocking { return "pause.circle.fill" }
        return "arrow.triangle.branch"
    }

    private var statusColor: Color {
        if cause.isSatisfied { return .green }
        if cause.isBlocking { return .orange }
        return .blue
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
                    LabeledContent(
                        "Time",
                        value: scheduleTimeRange(
                            block,
                            timezoneName: store.schedulePresentationTimezoneName
                        )
                    )
                    LabeledContent("Duration", value: "\(block.durationMinutes) minutes")
                    if !isExternalFixed {
                        LabeledContent("Energy", value: block.energy.title)
                    }
                    LabeledContent("Placement", value: placementDescription)
                    if let itemID = block.sourceItemID,
                       let item = store.canonicalItem(id: itemID) {
                        LabeledContent("Revision", value: String(item.revision))
                        if item.kind == .event, let end = item.deadlineAt {
                            LabeledContent(
                                "Ends",
                                value: scheduleDateTimeLabel(
                                    end,
                                    timezoneName: store.schedulePresentationTimezoneName
                                )
                            )
                        } else if item.deadlineKind == .date, let date = item.deadlineDate {
                            LabeledContent(
                                "Due date",
                                value: canonicalDeadlineDisplayValue(date, item: item)
                            )
                        } else if item.deadlineKind == .dateTime,
                                  let deadline = item.deadlineAt {
                            LabeledContent(
                                "Deadline",
                                value: canonicalDeadlineDisplayValue(
                                    scheduleDateTimeLabel(
                                        deadline,
                                        timezoneName: store.schedulePresentationTimezoneName
                                    ),
                                    item: item
                                )
                            )
                        }
                        if let blocker = canonicalBlockedDisplayValue(
                            item,
                            dependencyCauses: dependencyCauses
                        ) {
                            LabeledContent("Blocked", value: blocker)
                                .privacySensitive(item.isSensitive)
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


                if !dependencyCauses.isEmpty {
                    InspectorSection(title: dependencyBlockersAreActive
                        ? "Dependency blockers"
                        : "Dependencies") {
                        ForEach(dependencyCauses) { cause in
                            CanonicalDependencyCauseRow(
                                cause: cause,
                                open: cause.isAvailable
                                    ? { store.selectCanonicalItem(cause.predecessorID) }
                                    : nil
                            )
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

                if !isExternalFixed {
                    if block.sourceItemID != nil {
                        AuthoritativeExecutionControls(
                            block: block,
                            accessibilityScope: "inspector"
                        )
                    } else {
                        HStack {
                            Button("Start") { store.start(block.id) }
                                .buttonStyle(.borderedProminent)
                            Button("Complete") { store.complete(block.id) }
                            Menu("More") {
                                WillDoLaterButton(
                                    block: block,
                                    title: "Will do later",
                                    accessibilityScope: "inspector-more-local"
                                )
                                Button("Skip") { store.skip(block.id) }
                            }
                        }
                        .disabled(!store.canMutate(block))
                    }
                }
            }
            .padding(18)
        }
        .privacySensitive(block.isSensitive)
    }

    private var dependencyCauses: [CanonicalDependencyCause] {
        guard let itemID = block.sourceItemID,
              let item = store.canonicalItem(id: itemID) else { return [] }
        let references = CanonicalDependencyCatalog.references(
            canonicalItems: store.canonicalItems,
            pendingMutations: store.pendingCanonicalAuthoringMutations,
            trashEntries: store.canonicalTrash,
            sensitivity: { store.canonicalItemRequiresSensitivePresentation(itemID: $0) }
        )
        return CanonicalDependencyCatalog.causes(
            for: .init(item: item),
            ownerIsSensitive: store.canonicalItemRequiresSensitivePresentation(itemID: itemID),
            references: references,
            reportedBlockerID: item.blockedReasonKind == .dependency
                ? item.blockedByItemID
                : nil
        )
    }

    private var dependencyBlockersAreActive: Bool {
        guard let itemID = block.sourceItemID,
              let item = store.canonicalItem(id: itemID),
              item.status == .blocked,
              item.blockedReasonKind == .dependency else { return false }
        return dependencyCauses.contains(where: \.isBlocking)
    }

    private var isExternalFixed: Bool {
        block.previewKind == "external_fixed"
    }

    private var placementDescription: String {
        guard isExternalFixed else {
            return block.isHardConstraint ? "Fixed in preview" : "Flexible in preview"
        }
        // The wire contract deliberately redacts the fixed-block source, so do
        // not infer profile ownership from the presentation kind or title.
        return "Profile or external fixed time"
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
                .accessibilityAddTraits(.updatesFrequently)
                .accessibilityIdentifier("assistant.proposals-ready")
            }

            Divider()
            HStack(alignment: .bottom, spacing: 8) {
                TextField(composerPlaceholder, text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...5)
                    .onSubmit(send)
                    .disabled(!isSignedIn || conversation.activity.isBusy)
                    .accessibilityLabel("Message to Codex")
                    .accessibilityIdentifier("assistant.composer")
                Button(action: primaryComposerAction) {
                    Image(systemName: conversation.isTurnActive ? "stop.circle.fill" : "arrow.up.circle.fill")
                        .font(.title2)
                }
                .buttonStyle(.plain)
                .disabled(conversation.isTurnActive ? conversation.activity == .stopping : !canSend)
                .help(conversation.isTurnActive ? "Stop response" : "Send to Codex")
                .accessibilityLabel(conversation.isTurnActive ? "Stop response" : "Send to Codex")
                .accessibilityIdentifier("assistant.composer.primary-action")
            }
            .padding(12)
        }
        .onChange(of: conversation.activity) { _, activity in
            guard case let .failed(message) = activity else { return }
            dayWeavePostAccessibilityAnnouncement(
                "Codex response failed. \(message)",
                priority: .high
            )
        }
        .onChange(of: conversation.lastProposalCount) { _, count in
            guard count > 0 else { return }
            dayWeavePostAccessibilityAnnouncement(
                "\(count) Codex proposal\(count == 1 ? "" : "s") ready in Inbox for review."
            )
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

struct LocalCanonicalItemSuggestionReviewRoute: Identifiable, Equatable {
    let suggestionID: UUID
    let itemID: UUID
    let draft: DayWeaveCanonicalItemDraft

    var id: UUID { suggestionID }
}

enum LocalCanonicalItemSuggestionReviewAvailability: Equatable {
    case pending
    case accepted
    case unavailable
}

func localCanonicalItemSuggestionReviewAvailability(
    route: LocalCanonicalItemSuggestionReviewRoute,
    suggestions: [PlanningSuggestion]
) -> LocalCanonicalItemSuggestionReviewAvailability {
    guard let suggestion = suggestions.first(where: { $0.id == route.suggestionID }) else {
        return .unavailable
    }
    if suggestion.state == .accepted,
       case let .canonicalItemReference(itemID) = suggestion.payload,
       itemID == route.itemID {
        return .accepted
    }
    if suggestion.state == .pending,
       case let .canonicalItemDraft(itemDraft) = suggestion.payload,
       itemDraft.itemID == route.itemID {
        return .pending
    }
    return .unavailable
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
    @State private var localSuggestionReviewRoute:
        LocalCanonicalItemSuggestionReviewRoute?
    @State private var localSuggestionReviewNotice: String?

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
        let completed = store.showCompleted ? presentation.completed : []
        return Set(
            (presentation.inbox
                + presentation.planned
                + presentation.active
                + completed
                + presentation.trash).map(\.itemID)
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

            if let localSuggestionReviewNotice {
                HStack(spacing: 10) {
                    Label(localSuggestionReviewNotice, systemImage: "clock.badge.exclamationmark")
                        .font(.subheadline)
                    Spacer()
                    Button("Dismiss") {
                        self.localSuggestionReviewNotice = nil
                    }
                    .buttonStyle(.borderless)
                }
                .foregroundStyle(.orange)
                .padding(.horizontal, 20)
                .padding(.vertical, 10)
                .background(Color.orange.opacity(0.09))
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("inbox.suggestion-review-notice")

                Divider()
            }

            switch section {
            case .items:
                CanonicalCapturedInboxView()
            case .suggestions:
                SuggestionsInboxView(
                    reviewCanonicalItemDraft: reviewCanonicalItemSuggestion
                )
            }
        }
        .navigationTitle("Inbox")
        .sheet(item: $localSuggestionReviewRoute) { route in
            CanonicalItemEditorView(
                mode: .createFromSuggestion(
                    suggestionID: route.suggestionID,
                    itemID: route.itemID,
                    draft: route.draft
                ),
                profileTimezoneName: store.scheduleProfile.timezoneName,
                onSave: {
                    section = .items
                }
            )
            .environmentObject(store)
        }
        .onChange(of: section) { _, value in
            if value == .suggestions {
                store.selectCanonicalItem(nil)
            }
        }
        .onChange(of: store.suggestions) { _, currentSuggestions in
            guard let route = localSuggestionReviewRoute else { return }
            switch localCanonicalItemSuggestionReviewAvailability(
                route: route,
                suggestions: currentSuggestions
            ) {
            case .pending:
                break
            case .accepted:
                // The editor's successful save owns navigation and user
                // feedback. Release the redundant process-local route only.
                localSuggestionReviewRoute = nil
            case .unavailable:
                // Expiration/rejection scrubs the durable body; release the
                // sheet's process-local copy at the same boundary.
                localSuggestionReviewRoute = nil
                let notice = "The Codex draft expired or became unavailable, so its review was closed."
                localSuggestionReviewNotice = notice
                dayWeavePostAccessibilityAnnouncement(notice, priority: .high)
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

    private func reviewCanonicalItemSuggestion(_ suggestionID: UUID) {
        store.expireLocalSuggestions()
        guard let suggestion = store.suggestions.first(where: {
            $0.id == suggestionID && $0.state == .pending
        }),
              case let .canonicalItemDraft(itemDraft) = suggestion.payload else {
            return
        }
        localSuggestionReviewNotice = nil
        localSuggestionReviewRoute = .init(
            suggestionID: suggestion.id,
            itemID: itemDraft.itemID,
            draft: itemDraft.draft
        )
    }

    private var sectionDescription: String {
        switch section {
        case .items:
            "Capture, plan, and review your canonical work across its lifecycle."
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
    let reviewCanonicalItemDraft: (UUID) -> Void

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
                        LocalPlanningSuggestionRow(
                            suggestion: suggestion,
                            canMutate: store.canMutatePlan,
                            reviewCanonicalItemDraft: {
                                reviewCanonicalItemDraft(suggestion.id)
                            },
                            markAdvisoryReviewed: {
                                store.acceptSuggestion(suggestion.id)
                            },
                            reject: {
                                store.rejectSuggestion(suggestion.id)
                            }
                        )
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
            store.expireLocalSuggestions()
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

private struct LocalPlanningSuggestionRow: View {
    let suggestion: PlanningSuggestion
    let canMutate: Bool
    let reviewCanonicalItemDraft: () -> Void
    let markAdvisoryReviewed: () -> Void
    let reject: () -> Void

    private var canonicalItemDraft: PlanningSuggestionItemDraft? {
        guard case let .canonicalItemDraft(itemDraft) = suggestion.payload else {
            return nil
        }
        return itemDraft
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "sparkles")
                    .foregroundStyle(.purple)
                    .accessibilityHidden(true)
                Text(suggestion.title)
                    .font(.headline)
                    .privacySensitive(canonicalItemDraft?.draft.isSensitive == true)
                    .accessibilityIdentifier(localIdentifier("title"))
                Spacer()
                Text(suggestion.source)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Text(suggestion.summary)
                .foregroundStyle(.secondary)
                .privacySensitive(canonicalItemDraft?.draft.isSensitive == true)
                .accessibilityIdentifier(localIdentifier("summary"))

            if let itemDraft = canonicalItemDraft {
                typedDraftMetadata(itemDraft.draft)
                Label(
                    "Review every field before creating this item.",
                    systemImage: "checkmark.shield"
                )
                .font(.caption)
                .foregroundStyle(.blue)

                HStack {
                    Button("Review item draft…", action: reviewCanonicalItemDraft)
                        .buttonStyle(.borderedProminent)
                        .disabled(!canMutate || suggestion.expiresAt <= Date())
                        .help("Open this typed draft in the canonical item editor")
                        .accessibilityLabel("Review \(itemDraft.draft.title) item draft")
                        .accessibilityIdentifier(localIdentifier("review-item-draft"))
                    Button("Reject", action: reject)
                        .disabled(!canMutate)
                        .accessibilityIdentifier(localIdentifier("reject"))
                }
                .controlSize(.small)
            } else {
                Label(
                    "Advisory only · marking reviewed never creates or changes an item",
                    systemImage: "text.bubble"
                )
                .font(.caption)
                .foregroundStyle(.secondary)

                HStack {
                    Button("Mark reviewed", action: markAdvisoryReviewed)
                        .buttonStyle(.borderedProminent)
                        .disabled(!canMutate || suggestion.expiresAt <= Date())
                        .accessibilityIdentifier(localIdentifier("mark-reviewed"))
                    Button("Reject", action: reject)
                        .disabled(!canMutate)
                        .accessibilityIdentifier(localIdentifier("reject"))
                }
                .controlSize(.small)
            }
        }
        .padding(.vertical, 8)
        .privacySensitive(canonicalItemDraft?.draft.isSensitive == true)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(localIdentifier("row"))
    }

    @ViewBuilder
    private func typedDraftMetadata(_ draft: DayWeaveCanonicalItemDraft) -> some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) { typedDraftMetadataContent(draft) }
            VStack(alignment: .leading, spacing: 5) { typedDraftMetadataContent(draft) }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .accessibilityIdentifier(localIdentifier("metadata"))
    }

    @ViewBuilder
    private func typedDraftMetadataContent(_ draft: DayWeaveCanonicalItemDraft) -> some View {
        Label(
            draft.kind.wireValue.replacingOccurrences(of: "_", with: " ").capitalized,
            systemImage: canonicalItemKindSymbol(draft.kind)
        )
        Label(
            draft.status.wireValue.replacingOccurrences(of: "_", with: " ").capitalized,
            systemImage: draft.status == .planned ? "checkmark.circle" : "tray"
        )
        if let durationSeconds = draft.durationSeconds {
            Label(
                CanonicalItemEditorState.durationDescription(durationSeconds),
                systemImage: "timer"
            )
        }
        Label(
            "Expires \(suggestion.expiresAt.formatted(date: .abbreviated, time: .shortened))",
            systemImage: "clock"
        )
        .accessibilityIdentifier(localIdentifier("expiry"))
    }

    private func localIdentifier(_ suffix: String) -> String {
        "local-suggestion.\(suggestion.id.uuidString.lowercased()).\(suffix)"
    }

    private func canonicalItemKindSymbol(_ kind: DayWeaveCanonicalItemKind) -> String {
        switch kind {
        case .event: "calendar"
        case .task: "checkmark.circle"
        case .habit: "repeat"
        case .routine: "list.number"
        case .goal: "target"
        case .project: "folder"
        case .breakTime: "cup.and.saucer"
        case .unknown: "questionmark.diamond"
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
                Text(proposalItemIdentityLabel(
                    diff.itemID,
                    hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
                ))
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
                Text(proposalItemIdentityLabel(
                    diff.itemID,
                    hidesSensitiveContent: containsSensitiveValues && !revealsSensitiveValues
                ))
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
                Text(proposalItemSnapshotMetadata(
                    item,
                    hidesSensitiveContent: hidesSensitiveContent
                ))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let metrics = proposalItemSnapshotMetrics(
                    item,
                    hidesSensitiveContent: hidesSensitiveContent
                ) {
                    Text(metrics)
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

func proposalItemIdentityLabel(
    _ itemID: UUID,
    hidesSensitiveContent: Bool
) -> String {
    hidesSensitiveContent ? "Sensitive item" : "Item \(itemID.uuidString.prefix(8))"
}

func proposalItemSnapshotMetadata(
    _ item: DayWeaveCanonicalItem,
    hidesSensitiveContent: Bool
) -> String {
    if hidesSensitiveContent { return "Details hidden until reveal" }
    return "\(proposalFieldLabel(item.kind.wireValue)) · "
        + "\(proposalFieldLabel(item.status.wireValue)) · revision \(item.revision)"
}

func proposalItemSnapshotMetrics(
    _ item: DayWeaveCanonicalItem,
    hidesSensitiveContent: Bool
) -> String? {
    guard !hidesSensitiveContent, let duration = item.durationSeconds else { return nil }
    return "\(duration / 60) min · importance \(item.importance) · urgency \(item.urgency)"
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

func proposalItemFieldValue(
    _ field: String,
    item: DayWeaveCanonicalItem?,
    hidesSensitiveContent: Bool
) -> String {
    guard let item else { return "Item absent" }
    if hidesSensitiveContent, field != "is_sensitive" {
        return "Hidden — use Reveal sensitive before/after values"
    }
    return switch field {
    case "is_sensitive": item.isSensitive ? "Yes" : "No"
    case "kind": item.kind.wireValue
    case "status": item.status.wireValue
    case "title": item.title
    case "notes": item.notes ?? "None"
    case "timezone_name": item.timezoneName
    case "duration_kind": item.durationKind.wireValue
    case "duration_min_seconds": proposalDurationValue(item.durationMinimumSeconds)
    case "duration_seconds": item.durationSeconds.map { "\($0) seconds" } ?? "None"
    case "duration_max_seconds": proposalDurationValue(item.durationMaximumSeconds)
    case "duration_source": item.durationSource?.wireValue ?? "None"
    case "deadline_kind": item.deadlineKind.wireValue
    case "deadline_at": item.retainedCanonicalDeadlineAt
        ?? item.retainedUnrepresentableDeadlineAt
        ?? proposalDateValue(item.deadlineAt)
    case "deadline_date": item.deadlineDate ?? "None"
    case "deadline_strength": item.deadlineStrength?.wireValue ?? "None"
    case "deadline_soft_weight": item.deadlineSoftWeight.map(String.init) ?? "None"
    case "earliest_start_at": proposalDateValue(item.earliestStartAt)
    case "recurrence": item.recurrence.map(proposalJSONValue) ?? "None"
    case "flexible_constraints": CanonicalDependencyEdge.proposalProjection(
        fromFlexibleConstraints: item.flexibleConstraints
    ).map { proposalJSONValue($0.metadata) } ?? "Unsupported value"
    case "dependencies": CanonicalDependencyEdge.proposalProjection(
        fromFlexibleConstraints: item.flexibleConstraints
    ).map { proposalJSONValue($0.dependencies) } ?? "Unsupported value"
    case "split_policy": proposalSplitPolicyValue(item.splitPolicy)
    case "importance": String(item.importance)
    case "urgency": String(item.urgency)
    case "parent_id": item.parentID?.uuidString.lowercased() ?? "None"
    case "sibling_order": String(item.siblingOrder)
    case "has_own_effort": item.hasOwnEffort ? "Yes" : "No"
    case "blocked_reason_kind": item.blockedReasonKind?.wireValue ?? "None"
    case "blocked_by_item_id": item.blockedByItemID?.uuidString.lowercased() ?? "None"
    case "blocked_reason": item.blockedReason ?? "None"
    case "is_executable": item.isExecutable ? "Yes" : "No"
    case "revision": String(item.revision)
    case "completed_at": proposalDateValue(item.completedAt)
    case "deleted_at": proposalDateValue(item.deletedAt)
    default: "Unsupported field"
    }
}

private func proposalDurationValue(_ seconds: UInt32?) -> String {
    seconds.map { "\($0) seconds" } ?? "None"
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
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @StateObject private var willDoLaterPresenter = WillDoLaterPresenter()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let active = focusedExecutionBlock {
                VStack(alignment: .leading, spacing: 8) {
                    Text(active.status == .paused ? "Paused" : "In progress")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(active.title).font(.headline)
                    Text(scheduleTimeRange(
                        active,
                        timezoneName: store.schedulePresentationTimezoneName
                    ))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    if active.sourceItemID != nil {
                        AuthoritativeExecutionControls(
                            block: active,
                            includesCustomPause: false,
                            accessibilityScope: "menu-bar-active"
                        )
                        if executionSync.expiredBreakChoiceRequired {
                            Button("Extend break 10 minutes") {
                                Task {
                                    _ = await executionSync.pause(
                                        active.id,
                                        durationSeconds: 10 * 60
                                    )
                                }
                            }
                            .disabled(executionSync.isSyncing
                                || store.executionState.pendingCommand != nil
                                || !store.canMutatePlan)
                            Button("Choose another item") {
                                Task {
                                    let outcome = await executionSync
                                        .chooseAnotherAfterExpiredBreak()
                                    if outcome == .success {
                                        openWindow(id: "main")
                                    }
                                }
                            }
                            .disabled(executionSync.isSyncing
                                || store.executionState.pendingCommand != nil
                                || !store.canMutatePlan)
                            .accessibilityIdentifier(
                                "menu-bar.execution.expired-break.choose-another"
                            )
                            Button("Keep paused") {
                                Task {
                                    _ = await executionSync.keepPausedAfterExpiredBreak()
                                }
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
                }
                .privacySensitive(active.isSensitive)
            } else {
                ContentUnavailableView("Nothing active", systemImage: "checkmark.circle")
            }
            Divider()
            Button("Quick Capture…") { openWindow(id: "quick-capture") }
                .disabled(!store.canMutatePlan)
                .accessibilityIdentifier("menu-bar.quick-capture")
            Button("Compose on this Mac") {
                Task { await canonicalSync.recomposeLocally() }
            }
            .disabled(
                !store.canMutatePlan
                    || canonicalSync.isSyncing
                    || canonicalSync.isLocallyComposing
                    || !canonicalSync.canRecomposeLocally
            )
            .accessibilityIdentifier("menu-bar.compose-local")
            Button("Sync & compose") {
                Task { await canonicalSync.sync() }
            }
            .disabled(
                !canonicalSync.isConfigured
                    || canonicalSync.isSyncing
                    || canonicalSync.isLocallyComposing
                    || !store.canMutatePlan
            )
            .accessibilityIdentifier("menu-bar.sync-compose")
            Divider()
            Text(localCompositionStatusMessage)
                .font(.caption)
                .foregroundStyle(localCompositionStatusIsFailure ? .red : .secondary)
                .lineLimit(2)
            Text(executionSync.status.message)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
            Button("Refresh execution") {
                Task { await executionSync.refresh() }
            }
            .disabled(executionSync.isSyncing || !store.canMutatePlan)
        }
        .environmentObject(willDoLaterPresenter)
        .sheet(item: $willDoLaterPresenter.request) { request in
            WillDoLaterSheet(request: request)
                .environmentObject(store)
                .environmentObject(canonicalSync)
                .environmentObject(executionSync)
                .environmentObject(willDoLaterPresenter)
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

    private var localCompositionStatusIsFailure: Bool {
        if case .failed = canonicalSync.localCompositionStatus { return true }
        return false
    }

    private var localCompositionStatusMessage: String {
        if case .ready = canonicalSync.localCompositionStatus,
           store.localScheduleCompositionProvenance != nil {
            return "On-device schedule installed · not published to Google Calendar"
        }
        return canonicalSync.localCompositionStatus.message
    }
}

private extension ScheduleWeekday {
    var scheduleSettingsTitle: String {
        switch self {
        case .monday: "Monday"
        case .tuesday: "Tuesday"
        case .wednesday: "Wednesday"
        case .thursday: "Thursday"
        case .friday: "Friday"
        case .saturday: "Saturday"
        case .sunday: "Sunday"
        }
    }
}

private enum ScheduleProfileWindowKind: Equatable {
    case availability
    case protectedTime

    var lowercaseTitle: String {
        switch self {
        case .availability: "availability"
        case .protectedTime: "protected-time"
        }
    }
}

private struct ScheduleProfileWindowDraft: Identifiable, Equatable {
    let id: UUID
    var startMinutes: Int
    var endMinutes: Int

    init(
        id: UUID = UUID(),
        startMinutes: Int,
        endMinutes: Int
    ) {
        self.id = id
        self.startMinutes = startMinutes
        self.endMinutes = endMinutes
    }

    init(_ window: ScheduleLocalTimeWindow) {
        self.init(
            startMinutes: Int(window.start.minutesSinceMidnight),
            endMinutes: Int(window.end.minutesSinceMidnight)
        )
    }

    static func == (left: Self, right: Self) -> Bool {
        left.startMinutes == right.startMinutes
            && left.endMinutes == right.endMinutes
    }
}

private struct ScheduleProfileDayDraft: Identifiable, Equatable {
    let weekday: ScheduleWeekday
    var isEnabled: Bool
    var windows: [ScheduleProfileWindowDraft]

    var id: Int { weekday.rawValue }

    init(_ day: ScheduleAvailabilityDay) {
        weekday = day.weekday
        isEnabled = day.isEnabled
        windows = day.windows.map(ScheduleProfileWindowDraft.init)
    }

    init(_ day: ScheduleProtectedDay) {
        weekday = day.weekday
        isEnabled = day.isEnabled
        windows = day.windows.map(ScheduleProfileWindowDraft.init)
    }
}

private struct ScheduleProfileDraft: Equatable {
    var timezoneName: String
    var availability: [ScheduleProfileDayDraft]
    var sleepStartMinutes: Int
    var sleepEndMinutes: Int
    var protectedTime: [ScheduleProfileDayDraft]
    var defaultEnergy: EnergyLevel
    var contextsInput: String
    var locationInput: String

    init(_ profile: ScheduleProfile) {
        timezoneName = profile.timezoneName
        availability = profile.availability.map(ScheduleProfileDayDraft.init)
        sleepStartMinutes = Int(profile.sleep.start.minutesSinceMidnight)
        sleepEndMinutes = Int(profile.sleep.end.minutesSinceMidnight)
        protectedTime = profile.protectedTime.map(ScheduleProfileDayDraft.init)
        defaultEnergy = profile.defaultEnergy
        contextsInput = profile.contexts.joined(separator: ", ")
        locationInput = profile.location ?? ""
    }

    var normalizedContexts: [String] {
        let values = contextsInput.split(whereSeparator: { character in
            character == "," || character == "\n"
        })
        return Array(Set(values.compactMap { value -> String? in
            let normalized = ScheduleProfile.normalizeContext(String(value))
            return normalized.isEmpty ? nil : normalized
        })).sorted()
    }

    var sleepDurationMinutes: Int? {
        guard sleepStartMinutes > sleepEndMinutes else { return nil }
        return ScheduleLocalTime.minutesPerDay - sleepStartMinutes + sleepEndMinutes
    }

    func makeProfile() throws -> ScheduleProfile {
        let sleep = try ScheduleSleepInterval(
            start: ScheduleLocalTime(minutesSinceMidnight: sleepStartMinutes),
            end: ScheduleLocalTime(minutesSinceMidnight: sleepEndMinutes)
        )
        let validatedAvailability = try availability.map { day in
            let windows: [ScheduleLocalTimeWindow]
            if day.isEnabled {
                windows = try day.windows.map(Self.validatedWindow)
            } else {
                windows = []
            }
            return try ScheduleAvailabilityDay(
                weekday: day.weekday,
                isEnabled: day.isEnabled,
                windows: windows
            )
        }
        let validatedProtectedTime = try protectedTime.map { day in
            let windows: [ScheduleLocalTimeWindow]
            if day.isEnabled {
                windows = try day.windows.map(Self.validatedWindow)
            } else {
                windows = []
            }
            return try ScheduleProtectedDay(
                weekday: day.weekday,
                isEnabled: day.isEnabled,
                windows: windows
            )
        }
        return try ScheduleProfile(
            timezoneName: ScheduleProfile.normalizedTimezoneName(timezoneName),
            availability: validatedAvailability,
            sleep: sleep,
            protectedTime: validatedProtectedTime,
            defaultEnergy: defaultEnergy,
            contexts: normalizedContexts,
            location: ScheduleProfile.normalizeLocation(locationInput)
        )
    }

    func validationMessage(
        for kind: ScheduleProfileWindowKind,
        weekday: ScheduleWeekday
    ) -> String? {
        guard let targetDay = day(for: kind, weekday: weekday),
              targetDay.isEnabled else {
            return nil
        }
        guard !targetDay.windows.isEmpty else {
            return "Add at least one \(kind.lowercaseTitle) window."
        }
        guard sleepStartMinutes > sleepEndMinutes else {
            return "Set an overnight sleep interval before editing daily windows."
        }
        guard targetDay.windows.allSatisfy({
            $0.startMinutes < $0.endMinutes
                && $0.startMinutes >= sleepEndMinutes
                && $0.endMinutes <= sleepStartMinutes
        }) else {
            return "Keep every window non-empty and between wake and sleep."
        }
        let ordered = targetDay.windows.sorted {
            if $0.startMinutes != $1.startMinutes {
                return $0.startMinutes < $1.startMinutes
            }
            return $0.endMinutes < $1.endMinutes
        }
        guard zip(ordered, ordered.dropFirst()).allSatisfy({
            $0.endMinutes <= $1.startMinutes
        }) else {
            return "Windows on the same day cannot overlap."
        }
        if kind == .protectedTime {
            let total = ordered.reduce(0) {
                $0 + ($1.endMinutes - $1.startMinutes)
            }
            guard total <= ScheduleProfile.maximumProtectedFreeMinutes else {
                return "Protect at most eight hours on one day."
            }
        }
        let otherWindows = day(for: opposite(of: kind), weekday: weekday)
            .flatMap { $0.isEnabled ? $0.windows : [] } ?? []
        guard ordered.allSatisfy({ window in
            otherWindows.allSatisfy { other in
                window.endMinutes <= other.startMinutes
                    || other.endMinutes <= window.startMinutes
            }
        }) else {
            return kind == .availability
                ? "Availability cannot overlap protected time."
                : "Protected time cannot overlap availability."
        }
        return nil
    }

    func suggestedWindow(
        for kind: ScheduleProfileWindowKind,
        weekday: ScheduleWeekday
    ) -> ScheduleProfileWindowDraft? {
        guard sleepStartMinutes > sleepEndMinutes,
              let target = day(for: kind, weekday: weekday),
              target.windows.count < ScheduleAvailabilityDay.maximumWindows else {
            return nil
        }
        let other = day(for: opposite(of: kind), weekday: weekday)
        let occupied = (target.windows + ((other?.isEnabled == true) ? (other?.windows ?? []) : []))
            .filter {
                $0.startMinutes < $0.endMinutes
                    && $0.endMinutes > sleepEndMinutes
                    && $0.startMinutes < sleepStartMinutes
            }
            .map {
                ScheduleProfileWindowDraft(
                    startMinutes: max(sleepEndMinutes, $0.startMinutes),
                    endMinutes: min(sleepStartMinutes, $0.endMinutes)
                )
            }
            .sorted { $0.startMinutes < $1.startMinutes }

        var gaps: [(start: Int, end: Int)] = []
        var cursor = sleepEndMinutes
        for window in occupied {
            if cursor < window.startMinutes {
                gaps.append((cursor, window.startMinutes))
            }
            cursor = max(cursor, window.endMinutes)
        }
        if cursor < sleepStartMinutes { gaps.append((cursor, sleepStartMinutes)) }

        let preferredStart = kind == .availability ? 9 * 60 : 18 * 60
        for gap in gaps {
            let preferred = max(gap.start, preferredStart)
            let start = preferred + 30 <= gap.end ? preferred : gap.start
            let duration = min(60, gap.end - start)
            if duration >= 30 {
                return ScheduleProfileWindowDraft(
                    startMinutes: start,
                    endMinutes: start + duration
                )
            }
        }
        return nil
    }

    private static func validatedWindow(
        _ draft: ScheduleProfileWindowDraft
    ) throws -> ScheduleLocalTimeWindow {
        try ScheduleLocalTimeWindow(
            start: ScheduleLocalTime(minutesSinceMidnight: draft.startMinutes),
            end: ScheduleLocalTime(minutesSinceMidnight: draft.endMinutes)
        )
    }

    private func day(
        for kind: ScheduleProfileWindowKind,
        weekday: ScheduleWeekday
    ) -> ScheduleProfileDayDraft? {
        let days = kind == .availability ? availability : protectedTime
        return days.first(where: { $0.weekday == weekday })
    }

    private func opposite(of kind: ScheduleProfileWindowKind) -> ScheduleProfileWindowKind {
        kind == .availability ? .protectedTime : .availability
    }
}

private struct ScheduleProfileTimeField: View {
    let label: String
    let accessibilityContext: String
    @Binding var minutes: Int

    init(
        label: String,
        accessibilityContext: String = "",
        minutes: Binding<Int>
    ) {
        self.label = label
        self.accessibilityContext = accessibilityContext
        _minutes = minutes
    }

    var body: some View {
        HStack(spacing: 4) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Picker("\(accessibilityPrefix) hour", selection: hourBinding) {
                ForEach(0..<24, id: \.self) { hour in
                    Text(String(format: "%02d", hour)).tag(hour)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(width: 58)
            .accessibilityLabel("\(accessibilityPrefix) hour")
            .accessibilityValue(String(format: "%02d", minutes / 60))

            Text(":")
                .font(.system(.body, design: .monospaced).weight(.semibold))

            Picker("\(accessibilityPrefix) minute", selection: minuteBinding) {
                ForEach(0..<60, id: \.self) { minute in
                    Text(String(format: "%02d", minute)).tag(minute)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .frame(width: 58)
            .accessibilityLabel("\(accessibilityPrefix) minute")
            .accessibilityValue(String(format: "%02d", minutes % 60))
        }
        .accessibilityElement(children: .contain)
    }

    private var hourBinding: Binding<Int> {
        Binding(
            get: { minutes / 60 },
            set: { minutes = $0 * 60 + minutes % 60 }
        )
    }

    private var minuteBinding: Binding<Int> {
        Binding(
            get: { minutes % 60 },
            set: { minutes = (minutes / 60) * 60 + $0 }
        )
    }

    private var accessibilityPrefix: String {
        accessibilityContext.isEmpty ? label : "\(accessibilityContext) \(label)"
    }
}

private struct ScheduleProfileDayEditor: View {
    let kind: ScheduleProfileWindowKind
    @Binding var day: ScheduleProfileDayDraft
    let suggestedWindow: ScheduleProfileWindowDraft?
    let validationMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Toggle(day.weekday.scheduleSettingsTitle, isOn: enabledBinding)
                    .toggleStyle(.switch)
                    .frame(width: 150, alignment: .leading)
                    .accessibilityIdentifier(
                        "schedule-profile.\(kind.lowercaseTitle).\(day.weekday.rawValue).enabled"
                    )
                Spacer()
                Text(day.isEnabled ? windowCountLabel : "Unavailable")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button {
                    guard let suggestedWindow else { return }
                    day.windows.append(suggestedWindow)
                } label: {
                    Label("Add window", systemImage: "plus")
                }
                .buttonStyle(.borderless)
                .disabled(!day.isEnabled || suggestedWindow == nil)
                .help(addWindowHelp)
                .accessibilityLabel(
                    "Add \(kind.lowercaseTitle) window on \(day.weekday.scheduleSettingsTitle)"
                )
            }

            if day.isEnabled {
                ForEach($day.windows) { $window in
                    HStack(spacing: 10) {
                        ScheduleProfileTimeField(
                            label: "Start",
                            accessibilityContext:
                                "\(day.weekday.scheduleSettingsTitle) \(kind.lowercaseTitle)",
                            minutes: $window.startMinutes
                        )
                        Image(systemName: "arrow.right")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                        ScheduleProfileTimeField(
                            label: "End",
                            accessibilityContext:
                                "\(day.weekday.scheduleSettingsTitle) \(kind.lowercaseTitle)",
                            minutes: $window.endMinutes
                        )
                        Spacer()
                        Button(role: .destructive) {
                            day.windows.removeAll(where: { $0.id == window.id })
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .help("Remove this window")
                        .accessibilityLabel(
                            "Remove \(kind.lowercaseTitle) window on \(day.weekday.scheduleSettingsTitle)"
                        )
                    }
                    .padding(.leading, 30)
                }
            }

            if let validationMessage {
                Label(validationMessage, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .padding(.leading, 30)
                    .accessibilityLabel(
                        "\(day.weekday.scheduleSettingsTitle): \(validationMessage)"
                    )
            }
        }
        .padding(.vertical, 5)
    }

    private var enabledBinding: Binding<Bool> {
        Binding(
            get: { day.isEnabled },
            set: { isEnabled in
                day.isEnabled = isEnabled
                if isEnabled, day.windows.isEmpty, let suggestedWindow {
                    day.windows = [suggestedWindow]
                }
            }
        )
    }

    private var windowCountLabel: String {
        "\(day.windows.count) window\(day.windows.count == 1 ? "" : "s")"
    }

    private var addWindowHelp: String {
        if day.windows.count >= ScheduleAvailabilityDay.maximumWindows {
            return "This day already has the maximum of eight windows."
        }
        if suggestedWindow == nil {
            return "Make at least 30 free minutes between wake and sleep first."
        }
        return "Add another \(kind.lowercaseTitle) window."
    }
}

private struct ScheduleProfileSettingsEditor: View {
    @Binding var draft: ScheduleProfileDraft
    let isDirty: Bool
    let canCommit: Bool
    let validationMessage: String?
    let blockedMessage: String?
    let errorMessage: String?
    let statusMessage: String?
    let onSave: () -> Void
    let onRevert: () -> Void

    @State private var availabilityIsExpanded = true
    @State private var protectedTimeIsExpanded = false

    private static let timezoneChoices = Array(Set(
        TimeZone.knownTimeZoneIdentifiers + ["Europe/Madrid", "UTC"]
    ))
    .filter { $0 != "GMT" && ScheduleProfile.isKnownIANATimezone($0) }
    .sorted()

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Label("Schedule profile", systemImage: "calendar.badge.clock")
                    .font(.headline)
                Text("DayWeave composes seven days from these local-time boundaries. Weeks are Monday-first and every time uses the 24-hour clock.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            GroupBox {
                VStack(alignment: .leading, spacing: 12) {
                    LabeledContent("Time zone") {
                        Picker("Time zone", selection: $draft.timezoneName) {
                            ForEach(Self.timezoneChoices, id: \.self) { timezone in
                                Text(timezone).tag(timezone)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                        .frame(minWidth: 220, alignment: .trailing)
                        .accessibilityLabel("Schedule profile time zone")
                        .accessibilityIdentifier("schedule-profile.timezone")
                    }
                    Text("When you travel, this saved IANA zone remains authoritative until you explicitly change it.")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    Divider()

                    HStack(alignment: .center, spacing: 14) {
                        ScheduleProfileTimeField(
                            label: "Sleep",
                            accessibilityContext: "Overnight sleep",
                            minutes: $draft.sleepStartMinutes
                        )
                        Image(systemName: "arrow.right")
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                        ScheduleProfileTimeField(
                            label: "Wake",
                            accessibilityContext: "Overnight sleep",
                            minutes: $draft.sleepEndMinutes
                        )
                        Spacer()
                        Text(sleepSummary)
                            .font(.caption)
                            .foregroundStyle(draft.sleepDurationMinutes == nil ? .orange : .secondary)
                    }
                    Text("Sleep is a hard overnight block. Its start must be later on the clock than its next-day wake time.")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    Divider()

                    Picker("Available energy", selection: $draft.defaultEnergy) {
                        ForEach(EnergyLevel.allCases) { energy in
                            Text(energy.title).tag(energy)
                        }
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("schedule-profile.default-energy")
                    Text("Availability windows inherit this capacity; items that require more energy are placed elsewhere.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
            } label: {
                Label("Time, sleep & energy", systemImage: "moon.stars")
            }

            GroupBox {
                DisclosureGroup(isExpanded: $availabilityIsExpanded) {
                    VStack(spacing: 0) {
                        ForEach($draft.availability) { $day in
                            ScheduleProfileDayEditor(
                                kind: .availability,
                                day: $day,
                                suggestedWindow: draft.suggestedWindow(
                                    for: .availability,
                                    weekday: day.weekday
                                ),
                                validationMessage: draft.validationMessage(
                                    for: .availability,
                                    weekday: day.weekday
                                )
                            )
                            if day.weekday != .sunday { Divider() }
                        }
                    }
                    .padding(.top, 8)
                } label: {
                    HStack {
                        Label("Availability", systemImage: "calendar.badge.clock")
                        Spacer()
                        Text(availabilitySummary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            GroupBox {
                DisclosureGroup(isExpanded: $protectedTimeIsExpanded) {
                    VStack(alignment: .leading, spacing: 0) {
                        Text("Protected windows stay visible as fixed time and cannot overlap availability. Protect up to eight hours per day.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 8)
                        ForEach($draft.protectedTime) { $day in
                            ScheduleProfileDayEditor(
                                kind: .protectedTime,
                                day: $day,
                                suggestedWindow: draft.suggestedWindow(
                                    for: .protectedTime,
                                    weekday: day.weekday
                                ),
                                validationMessage: draft.validationMessage(
                                    for: .protectedTime,
                                    weekday: day.weekday
                                )
                            )
                            if day.weekday != .sunday { Divider() }
                        }
                    }
                } label: {
                    HStack {
                        Label("Protected time", systemImage: "shield")
                        Spacer()
                        Text(protectedTimeSummary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            GroupBox {
                VStack(alignment: .leading, spacing: 10) {
                    TextField(
                        "home, desk, errands",
                        text: $draft.contextsInput,
                        axis: .vertical
                    )
                    .lineLimit(1...3)
                    .accessibilityLabel("Default scheduling contexts, separated by commas")
                    .accessibilityIdentifier("schedule-profile.contexts")
                    if !draft.normalizedContexts.isEmpty {
                        ScrollView(.horizontal) {
                            HStack(spacing: 6) {
                                ForEach(draft.normalizedContexts, id: \.self) { context in
                                    Text("#\(context)")
                                        .font(.caption)
                                        .padding(.horizontal, 8)
                                        .padding(.vertical, 4)
                                        .background(.quaternary, in: Capsule())
                                        .accessibilityLabel("Normalized context \(context)")
                                }
                            }
                        }
                        .scrollIndicators(.hidden)
                    }
                    Text("Contexts are trimmed, lowercased, deduplicated, and sorted when saved (up to 16; 64 UTF-8 bytes each).")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    Divider()

                    TextField("Optional default location", text: $draft.locationInput)
                        .accessibilityLabel("Optional default scheduling location")
                        .accessibilityIdentifier("schedule-profile.location")
                    Text("Whitespace is normalized when saved. The location may use up to 256 UTF-8 bytes.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
            } label: {
                Label("Context & location", systemImage: "mappin.and.ellipse")
            }

            VStack(alignment: .leading, spacing: 8) {
                if let validationMessage {
                    Label(validationMessage, systemImage: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .accessibilityIdentifier("schedule-profile.validation-error")
                }
                if let blockedMessage {
                    Label(blockedMessage, systemImage: "lock")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if let errorMessage {
                    Label(errorMessage, systemImage: "xmark.octagon.fill")
                        .font(.caption)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("schedule-profile.save-error")
                } else if let statusMessage {
                    Label(statusMessage, systemImage: "checkmark.circle.fill")
                        .font(.caption)
                        .foregroundStyle(.green)
                        .accessibilityIdentifier("schedule-profile.save-status")
                }
                Text("Saving is atomic and encrypted. A proven published schedule stays in its publication timezone; this profile is used the next time you compose. Unpublished local schedule blocks are cleared.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            HStack {
                Spacer()
                Button("Revert", role: .cancel, action: onRevert)
                    .disabled(!isDirty)
                    .accessibilityIdentifier("schedule-profile.revert")
                Button("Save profile", action: onSave)
                    .buttonStyle(.borderedProminent)
                    .disabled(!isDirty || validationMessage != nil || !canCommit)
                    .keyboardShortcut("s", modifiers: [.command, .option])
                    .accessibilityIdentifier("schedule-profile.save")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .privacySensitive()
    }

    private var sleepSummary: String {
        guard let duration = draft.sleepDurationMinutes else {
            return "Must cross midnight"
        }
        let hours = duration / 60
        let minutes = duration % 60
        return minutes == 0 ? "\(hours)h overnight" : "\(hours)h \(minutes)m overnight"
    }

    private var availabilitySummary: String {
        let enabledDays = draft.availability.count(where: \.isEnabled)
        let windows = draft.availability.filter(\.isEnabled).reduce(0) {
            $0 + $1.windows.count
        }
        return "\(enabledDays) day\(enabledDays == 1 ? "" : "s") · \(windows) window\(windows == 1 ? "" : "s")"
    }

    private var protectedTimeSummary: String {
        let minutes = draft.protectedTime.filter(\.isEnabled).reduce(0) { total, day in
            total + day.windows.reduce(0) {
                $0 + max(0, $1.endMinutes - $1.startMinutes)
            }
        }
        let hours = minutes / 60
        let remainder = minutes % 60
        if minutes == 0 { return "None" }
        return remainder == 0 ? "\(hours)h / week" : "\(hours)h \(remainder)m / week"
    }
}

struct SettingsView: View {
    @Environment(\.openWindow) private var openWindow
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var codex: CodexAppServerClient
    @EnvironmentObject private var suggestionSync: SuggestionSyncStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var executionSync: ExecutionSyncStore
    @EnvironmentObject private var googleIntegration: GoogleIntegrationStore
    @EnvironmentObject private var googleOutbound: GoogleOutboundStore
    @EnvironmentObject private var googleSchedulePublication: GoogleSchedulePublicationStore
    @EnvironmentObject private var durableAuth: DurableAuthSettingsModel
    @EnvironmentObject private var appLock: AppLockController
    @EnvironmentObject private var appearance: AppearanceController
    @EnvironmentObject private var onboarding: DayWeaveOnboardingController
    @State private var dayWeaveAPIBaseURL = ""
    @State private var dayWeaveBearerToken = ""
    @State private var dayWeaveEnrollmentCode = ""
    @State private var isCanonicalResetConfirmationPresented = false
    @State private var isLocalOnlyForgetConfirmationPresented = false
    @State private var apiSettingsError: String?
    @State private var scheduleProfileBaseline: ScheduleProfile?
    @State private var scheduleProfileDraft: ScheduleProfileDraft?
    @State private var scheduleProfileError: String?
    @State private var scheduleProfileStatus: String?

    var body: some View {
        Form {
            if !onboarding.isComplete {
                Section("Getting started") {
                    LabeledContent(
                        "Guided setup",
                        value: onboarding.currentStep.title
                    )
                    Text("Setup is resumable and does not mark itself complete until a planned item and an exact first schedule are safely stored.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Button("Resume guided setup") {
                        onboarding.present()
                        NSApp.activate(ignoringOtherApps: true)
                        openWindow(id: "main")
                    }
                    .accessibilityIdentifier("onboarding.resume.settings")
                }
            }
            Section("Scheduling") {
                Stepper(
                    "Freeze the next \(store.freezeHours) hours",
                    value: $store.freezeHours,
                    in: 0...24
                )
                .disabled(!store.canMutatePlan)
                Toggle("Show completed blocks", isOn: $store.showCompleted)
                    .disabled(!store.canMutatePlan)
                Divider()
                if scheduleProfileDraft != nil {
                    ScheduleProfileSettingsEditor(
                        draft: scheduleProfileDraftBinding,
                        isDirty: scheduleProfileIsDirty,
                        canCommit: scheduleProfileCanCommit,
                        validationMessage: scheduleProfileValidationMessage,
                        blockedMessage: scheduleProfileBlockedMessage,
                        errorMessage: scheduleProfileError,
                        statusMessage: scheduleProfileStatus,
                        onSave: saveScheduleProfile,
                        onRevert: revertScheduleProfile
                    )
                } else {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Loading the encrypted schedule profile…")
                            .foregroundStyle(.secondary)
                    }
                }
            }
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
                if googleSchedulePublication.hasSavedPublication {
                    Text("A generated-schedule Google Calendar preview, approval, or delivery status is preserved in encrypted local recovery. Open Calendar → Publication status before replacing authentication or resetting canonical state.")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                if googleOutbound.hasPendingRecovery {
                    Text(googleOutbound.recoveryContext?.entityKind == .task
                        ? "A Google Tasks preview or approved publication is preserved in encrypted local recovery. Recover it from the Items inspector before replacing authentication, changing the API origin, or resetting canonical state."
                        : "A Google Calendar preview or approved publication is preserved in encrypted local recovery. Recover it from the Items inspector before replacing authentication, changing the API origin, or resetting canonical state.")
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
                if let notificationIssue = executionSync.breakNotificationIssue {
                    HStack {
                        Text(notificationIssue.message)
                            .font(.caption)
                            .foregroundStyle(.orange)
                        Spacer()
                        Button(notificationIssue.retryTitle) {
                            Task { _ = await executionSync.retryBreakNotification() }
                        }
                        .disabled(executionSync.isRequestingBreakNotificationAuthorization)
                    }
                    .accessibilityIdentifier("settings.execution.break-notification.issue")
                }
                Button("Reset local canonical cache…", role: .destructive) {
                    isCanonicalResetConfirmationPresented = true
                }
                .disabled(
                    !store.canMutatePlan
                        || executionSync.credentialReplacementIsBlocked
                        || googleOutbound.hasPendingRecovery
                        || googleSchedulePublication.hasSavedPublication
                )
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
                Task { @MainActor in
                    let cancellation = await executionSync
                        .cancelBreakNotificationsForConfigurationReset()
                    guard cancellation.isVerifiedCancellation else { return }
                    canonicalSync.resetCanonicalSyncState()
                    // If the reset lost a preflight race and was refused, this
                    // restores the still-authoritative reminder. A successful
                    // reset simply confirms that the center remains empty.
                    _ = await executionSync.reconcileBreakNotification()
                }
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
            loadScheduleProfileIfNeeded()
        }
        .onChange(of: dayWeaveAPIBaseURL) { _, value in
            durableAuth.reload(boundTo: try? DayWeaveAPIBaseURL(value))
        }
        .onChange(of: store.scheduleProfile) { _, profile in
            persistedScheduleProfileDidChange(profile)
        }
    }

    private var scheduleProfileDraftBinding: Binding<ScheduleProfileDraft> {
        Binding(
            get: {
                scheduleProfileDraft ?? ScheduleProfileDraft(store.scheduleProfile)
            },
            set: { draft in
                scheduleProfileDraft = draft
                if scheduleProfileBaseline == store.scheduleProfile {
                    scheduleProfileError = nil
                }
                scheduleProfileStatus = nil
            }
        )
    }

    private var scheduleProfileIsDirty: Bool {
        guard let scheduleProfileDraft, let scheduleProfileBaseline else {
            return false
        }
        guard let candidate = try? scheduleProfileDraft.makeProfile() else {
            return true
        }
        return candidate != scheduleProfileBaseline
    }

    private var scheduleProfileValidationMessage: String? {
        guard let scheduleProfileDraft else { return nil }
        do {
            _ = try scheduleProfileDraft.makeProfile()
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    private var scheduleProfileCanCommit: Bool {
        store.hasEncryptedPersistence && store.canMutatePlan
    }

    private var scheduleProfileBlockedMessage: String? {
        if !store.hasEncryptedPersistence {
            return "Configure healthy encrypted planner storage before saving this profile."
        }
        if !store.canPersistPlan {
            return "Repair local encrypted persistence before saving this profile."
        }
        if !store.canMutatePlan {
            return "Wait for canonical sync or on-device composition to finish before saving."
        }
        return nil
    }

    private func loadScheduleProfileIfNeeded() {
        guard scheduleProfileBaseline == nil || scheduleProfileDraft == nil else {
            return
        }
        installScheduleProfileDraft(store.scheduleProfile)
    }

    private func installScheduleProfileDraft(_ profile: ScheduleProfile) {
        scheduleProfileBaseline = profile
        scheduleProfileDraft = ScheduleProfileDraft(profile)
        scheduleProfileError = nil
    }

    private func saveScheduleProfile() {
        guard let scheduleProfileDraft, let scheduleProfileBaseline else { return }
        scheduleProfileError = nil
        scheduleProfileStatus = nil
        do {
            let candidate = try scheduleProfileDraft.makeProfile()
            try store.updateScheduleProfile(
                candidate,
                expectedCurrentProfile: scheduleProfileBaseline
            )
            installScheduleProfileDraft(candidate)
            scheduleProfileStatus = "Schedule profile saved. Compose again when you are ready."
        } catch {
            scheduleProfileError = error.localizedDescription
        }
    }

    private func revertScheduleProfile() {
        installScheduleProfileDraft(store.scheduleProfile)
        scheduleProfileStatus = "Unsaved profile edits reverted."
    }

    private func persistedScheduleProfileDidChange(_ profile: ScheduleProfile) {
        guard let scheduleProfileBaseline, let scheduleProfileDraft else {
            installScheduleProfileDraft(profile)
            return
        }
        guard profile != scheduleProfileBaseline else { return }
        if (try? scheduleProfileDraft.makeProfile()) == scheduleProfileBaseline {
            installScheduleProfileDraft(profile)
            scheduleProfileStatus = "Reloaded a newer saved schedule profile."
        } else {
            scheduleProfileError = "The saved schedule profile changed while you were editing. Revert to reload it before saving."
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
            || googleOutbound.hasPendingRecovery
            || googleSchedulePublication.hasSavedPublication
    }

    private var googleAuthenticationUpdateIsBlocked: Bool {
        if googleOutbound.hasPendingRecovery
            || googleSchedulePublication.hasSavedPublication { return true }
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
        guard !googleOutbound.hasPendingRecovery,
              !googleSchedulePublication.hasSavedPublication else {
            apiSettingsError = "Recover or finish the saved Google publication before changing DayWeave authentication."
            return false
        }
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
            let bootstrap = dayWeaveBearerToken
                .trimmingCharacters(in: .whitespacesAndNewlines)
            Task { @MainActor in
                defer {
                    if reservedGoogleTransition {
                        googleIntegration.endCredentialTransition()
                    }
                }
                if replacementRequired {
                    do {
                        try await executionSync.prepareForCredentialReplacement()
                    } catch {
                        apiSettingsError = error.localizedDescription
                        return
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
                googleOutbound.configurationDidChange()
                googleSchedulePublication.configurationDidChange()
                canonicalSync.configurationDidChange()
                await executionSync.configurationDidChange()
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
        let baseURL: DayWeaveAPIBaseURL
        do {
            baseURL = try DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
        } catch {
            googleIntegration.endCredentialTransition()
            apiSettingsError = error.localizedDescription
            return
        }
        Task { @MainActor in
            defer { googleIntegration.endCredentialTransition() }
            do {
                try await executionSync.prepareForCredentialReplacement()
                guard await durableAuth.revokeAndForget(baseURL: baseURL) else {
                    apiSettingsError = durableAuth.errorMessage
                    return
                }
                suggestionSync.durableAuthenticationDidChange()
                googleIntegration.configurationDidChange()
                googleOutbound.configurationDidChange()
                googleSchedulePublication.configurationDidChange()
                canonicalSync.configurationDidChange()
                await executionSync.configurationDidChange()
                dayWeaveBearerToken = ""
            } catch {
                apiSettingsError = error.localizedDescription
            }
        }
    }

    private func forgetAuthenticationLocally() {
        apiSettingsError = nil
        guard allowGoogleCredentialTransition() else { return }
        let baseURL = try? DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
        Task { @MainActor in
            defer { googleIntegration.endCredentialTransition() }
            do {
                try await executionSync.prepareForCredentialReplacement()
                guard await durableAuth.forgetLocally(baseURL: baseURL) else {
                    apiSettingsError = durableAuth.errorMessage
                    return
                }
                suggestionSync.durableAuthenticationDidChange()
                googleIntegration.configurationDidChange()
                googleOutbound.configurationDidChange()
                googleSchedulePublication.configurationDidChange()
                canonicalSync.configurationDidChange()
                await executionSync.configurationDidChange()
                dayWeaveBearerToken = ""
                dayWeaveEnrollmentCode = ""
            } catch {
                apiSettingsError = error.localizedDescription
            }
        }
    }

    private func consumeOneTimeEnrollmentCode() {
        apiSettingsError = nil
        guard allowGoogleCredentialTransition(allowSameAPIBaseRepair: true) else { return }
        do {
            let baseURL = try DayWeaveAPIBaseURL(dayWeaveAPIBaseURL)
            let capturedBaseURL = baseURL.url.absoluteString
            let code = dayWeaveEnrollmentCode
            Task { @MainActor in
                defer { googleIntegration.endCredentialTransition() }
                do {
                    try await executionSync.prepareForCredentialReplacement()
                } catch {
                    apiSettingsError = error.localizedDescription
                    return
                }
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
                googleOutbound.configurationDidChange()
                googleSchedulePublication.configurationDidChange()
                canonicalSync.configurationDidChange()
                await executionSync.configurationDidChange()
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
            Task { @MainActor in
                defer { googleIntegration.endCredentialTransition() }
                do {
                    try await executionSync.prepareForCredentialReplacement()
                } catch {
                    apiSettingsError = error.localizedDescription
                    return
                }
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
                googleOutbound.configurationDidChange()
                googleSchedulePublication.configurationDidChange()
                canonicalSync.configurationDidChange()
                await executionSync.configurationDidChange()
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
