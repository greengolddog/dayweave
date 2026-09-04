import SwiftUI

enum PlannerPresentation {
    static func timeZone(timezoneName: String) -> TimeZone {
        PlannerTimeZone.resolve(timezoneName)
    }

    static func calendar(timezoneName: String) -> Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = timeZone(timezoneName: timezoneName)
        calendar.firstWeekday = 2
        calendar.minimumDaysInFirstWeek = 4
        return calendar
    }

    static func dayInterval(
        containing date: Date,
        timezoneName: String
    ) -> DateInterval? {
        let calendar = calendar(timezoneName: timezoneName)
        let start = calendar.startOfDay(for: date)
        guard let end = calendar.date(byAdding: .day, value: 1, to: start),
              end > start else { return nil }
        return DateInterval(start: start, end: end)
    }

    static func weekDays(
        containing date: Date,
        timezoneName: String
    ) -> [Date] {
        let calendar = calendar(timezoneName: timezoneName)
        let day = calendar.startOfDay(for: date)
        let daysSinceMonday = (calendar.component(.weekday, from: day) + 5) % 7
        guard let monday = calendar.date(
            byAdding: .day,
            value: -daysSinceMonday,
            to: day
        ) else { return [] }
        return (0..<7).compactMap {
            calendar.date(byAdding: .day, value: $0, to: monday)
        }
    }

    static func blocks(
        _ blocks: [ScheduleBlock],
        intersectingDayContaining date: Date,
        timezoneName: String
    ) -> [ScheduleBlock] {
        guard let interval = dayInterval(containing: date, timezoneName: timezoneName) else {
            return []
        }
        return blocks.filter { $0.end > interval.start && $0.start < interval.end }
    }

    static func weekdayLabel(_ date: Date, timezoneName: String) -> String {
        var style = Date.FormatStyle().weekday(.abbreviated)
        style.timeZone = timeZone(timezoneName: timezoneName)
        return date.formatted(style)
    }

    static func dayLabel(_ date: Date, timezoneName: String) -> String {
        var style = Date.FormatStyle().day()
        style.timeZone = timeZone(timezoneName: timezoneName)
        return date.formatted(style)
    }
}

enum HabitStatisticsPresentation {
    static func adherencePercent(_ analytics: [DayWeaveHabitAnalytics]) -> Int {
        let aggregate = analytics.reduce((weightedBasisPoints: UInt64(0), eligible: UInt64(0))) {
            result, value in
            (
                result.weightedBasisPoints
                    + value.totals.eligible * UInt64(value.totals.adherenceBasisPoints),
                result.eligible + value.totals.eligible
            )
        }
        guard aggregate.eligible > 0 else { return 0 }
        let denominator = aggregate.eligible * 100
        return Int(min(
            UInt64(100),
            (aggregate.weightedBasisPoints + denominator / 2) / denominator
        ))
    }

    static func streakLabel(_ days: UInt32) -> String {
        days == 1 ? "1 day" : "\(days) days"
    }

    static func durationLabel(_ seconds: UInt64) -> String {
        let minutes = seconds / 60
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        let remainder = minutes % 60
        return remainder == 0 ? "\(hours)h" : "\(hours)h \(remainder)m"
    }
}

struct CalendarDestinationView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var googleSchedulePublication: GoogleSchedulePublicationStore
    @State private var selectedDate = Date()
    @State private var schedulePublicationIsPresented = false

    private var timezoneName: String { store.schedulePresentationTimezoneName }
    private var calendar: Calendar {
        PlannerPresentation.calendar(timezoneName: timezoneName)
    }

    private var weekDays: [Date] {
        PlannerPresentation.weekDays(
            containing: selectedDate,
            timezoneName: timezoneName
        )
    }

    private var selectedBlocks: [ScheduleBlock] {
        PlannerPresentation.blocks(
            store.blocks,
            intersectingDayContaining: selectedDate,
            timezoneName: timezoneName
        )
            .sorted { $0.start < $1.start }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Calendar").font(.title2.weight(.semibold))
                    Text("Firm DayWeave work can publish to Google. Sleep and protected-time blocks from your profile stay planning-only and never publish.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    schedulePublicationIsPresented = true
                } label: {
                    Label(
                        googleSchedulePublication.hasSavedPublication
                            ? "Publication status" : "Publish to Google",
                        systemImage: googleSchedulePublication.hasSavedPublication
                            ? "arrow.triangle.2.circlepath" : "calendar.badge.plus"
                    )
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("calendar.publish-google")
                Button {
                    selectedDate = calendar.date(byAdding: .weekOfYear, value: -1, to: selectedDate) ?? selectedDate
                } label: {
                    Image(systemName: "chevron.left")
                }
                Button("Today") {
                    selectedDate = calendar.startOfDay(for: Date())
                }
                Button {
                    selectedDate = calendar.date(byAdding: .weekOfYear, value: 1, to: selectedDate) ?? selectedDate
                } label: {
                    Image(systemName: "chevron.right")
                }
            }
            .padding(20)

            HStack(spacing: 8) {
                ForEach(weekDays, id: \.self) { date in
                    Button {
                        selectedDate = date
                    } label: {
                        VStack(spacing: 5) {
                            Text(PlannerPresentation.weekdayLabel(
                                date,
                                timezoneName: timezoneName
                            ))
                                .font(.caption)
                            Text(PlannerPresentation.dayLabel(
                                date,
                                timezoneName: timezoneName
                            ))
                                .font(.headline)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 9)
                        .background(
                            calendar.isDate(date, inSameDayAs: selectedDate)
                                ? Color.accentColor.opacity(0.16)
                                : Color.clear,
                            in: RoundedRectangle(cornerRadius: 10)
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 14)
            Divider()

            if selectedBlocks.isEmpty {
                ContentUnavailableView(
                    "No blocks on this day",
                    systemImage: "calendar.badge.plus",
                    description: Text("Capture work or navigate to a day that already has a plan.")
                )
            } else {
                ScrollView {
                    LazyVStack(spacing: 10) {
                        ForEach(selectedBlocks) { block in
                            CalendarAgendaRow(block: block, timezoneName: timezoneName)
                                .onTapGesture { store.select(block) }
                        }
                    }
                    .padding(20)
                }
            }
        }
        .navigationTitle("Calendar")
        .sheet(isPresented: $schedulePublicationIsPresented) {
            GoogleSchedulePublicationView()
        }
        .onChange(of: timezoneName) { _, _ in
            selectedDate = calendar.startOfDay(for: Date())
        }
    }
}

private struct CalendarAgendaRow: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock
    let timezoneName: String

    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .trailing, spacing: 3) {
                Text(block.startTimeLabel(timezoneName: timezoneName))
                    .font(.system(.subheadline, design: .monospaced).weight(.semibold))
                Text(block.endTimeLabel(timezoneName: timezoneName))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(width: 108)
            RoundedRectangle(cornerRadius: 3)
                .fill(block.kind.color)
                .frame(width: 5, height: 48)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(block.title).font(.headline)
                    if block.isHardConstraint {
                        Image(systemName: "lock.fill").font(.caption2).foregroundStyle(.secondary)
                    }
                }
                Text(block.project ?? block.kind.title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if block.isExternalFixedBlock {
                    Label("Planning-only fixed time", systemImage: "calendar.badge.exclamationmark")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            Text(block.status.title)
                .font(.caption.weight(.medium))
                .foregroundStyle(block.status == .active ? .green : .secondary)
        }
        .padding(14)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 13))
        .overlay {
            RoundedRectangle(cornerRadius: 13)
                .stroke(store.selectedBlockID == block.id ? Color.accentColor : .clear, lineWidth: 1)
        }
        .privacySensitive(block.isSensitive)
    }
}

private struct HabitScheduleOccurrenceRow: Identifiable {
    let block: ScheduleBlock
    let occurrence: DayWeaveHabitOccurrence
    var id: UUID { occurrence.id }
}

private struct HabitOutcomeEditorPresentation: Identifiable {
    let title: String
    let occurrence: DayWeaveHabitOccurrence
    let initialStatus: DayWeaveHabitOutcomeStatus
    let isCorrection: Bool
    var id: UUID { occurrence.id }
}

struct HabitsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var habitSync: HabitSyncStore
    @State private var editor: HabitOutcomeEditorPresentation?

    private var habits: [ScheduleBlock] {
        PlannerPresentation.blocks(
            store.blocks,
            intersectingDayContaining: Date(),
            timezoneName: store.schedulePresentationTimezoneName
        )
            .filter { $0.kind == .habit }
            .sorted { $0.start < $1.start }
    }

    private var canonicalRows: [HabitScheduleOccurrenceRow] {
        var seen = Set<UUID>()
        return habits.compactMap { block in
            guard let occurrence = habitSync.canonicalOccurrence(for: block),
                  seen.insert(occurrence.id).inserted else { return nil }
            return .init(block: block, occurrence: occurrence)
        }
    }

    private var localFallback: [ScheduleBlock] {
        habits.filter { habitSync.canonicalOccurrence(for: $0) == nil }
    }

    private var completedCount: Int {
        canonicalRows.count(where: { $0.occurrence.outcome?.status == .completed })
            + localFallback.count(where: { $0.status == .completed })
    }

    var body: some View {
        DestinationScroll(
            title: "Habits",
            subtitle: "Log each scheduled occurrence, keep partial effort, and pause a habit without losing its history."
        ) {
            SummaryStrip(metrics: [
                ("\(canonicalRows.count + localFallback.count)", "today", "repeat"),
                ("\(completedCount)", "completed", "checkmark.circle"),
                ("\(canonicalRows.count(where: { habitSync.openPause(for: $0.occurrence.evidence.habitID) != nil }))", "paused", "pause.circle"),
            ])

            HabitSyncStatusBanner(status: habitSync.status) {
                Task { await habitSync.sync() }
            }

            if !habitSync.pendingMutations.filter(\.conflictDetected).isEmpty {
                VStack(alignment: .leading, spacing: 10) {
                    Label("Saved edits need review", systemImage: "exclamationmark.triangle.fill")
                        .font(.headline)
                        .foregroundStyle(.orange)
                    Text("The server has newer habit progress. Use its current version, then log a new correction if needed.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    ForEach(habitSync.pendingMutations.filter(\.conflictDetected)) { pending in
                        HStack {
                            Text(conflictLabel(pending))
                                .font(.subheadline.weight(.medium))
                            Spacer()
                            Button("Use server version") {
                                Task { await habitSync.discardPendingMutation(pending.id) }
                            }
                            .accessibilityIdentifier(
                                "habit.conflict.use-server.\(pending.id.uuidString.lowercased())"
                            )
                        }
                    }
                }
                .padding(16)
                .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("habits.conflicts")
            }

            if habits.isEmpty {
                DestinationEmpty(title: "No habit occurrences", symbol: "repeat.circle", action: "Add habit") {
                    store.isQuickAddPresented = true
                }
            } else {
                ForEach(canonicalRows) { row in
                    CanonicalHabitOccurrenceCard(
                        row: row,
                        timezoneName: store.schedulePresentationTimezoneName,
                        edit: { status, correction in
                            editor = .init(
                                title: row.block.title,
                                occurrence: row.occurrence,
                                initialStatus: status,
                                isCorrection: correction
                            )
                        }
                    )
                }

                ForEach(localFallback) { habit in
                    PlanningCard(
                        block: habit,
                        detail: habit.notes,
                        timezoneName: store.schedulePresentationTimezoneName
                    ) {
                        if habit.sourceItemID != nil {
                            AuthoritativeExecutionControls(
                                block: habit,
                                accessibilityScope: "habits-card"
                            )
                            if canResolveScheduledCanonicalOccurrence(habit) {
                                Button("Complete") { store.complete(habit.id) }
                                    .disabled(!store.canMutate(habit))
                                    .accessibilityIdentifier(
                                        "habit.complete.scheduled.\(habit.id.uuidString.lowercased())"
                                    )
                                Button("Skipped") { store.skip(habit.id) }
                                    .disabled(!store.canMutate(habit))
                                    .accessibilityIdentifier(
                                        "habit.skip.scheduled.\(habit.id.uuidString.lowercased())"
                                    )
                            }
                        } else {
                            Button("Complete") { store.complete(habit.id) }
                                .disabled(!store.canMutate(habit))
                            Button("Skipped") { store.skip(habit.id) }
                                .disabled(!store.canMutate(habit))
                            WillDoLaterButton(
                                block: habit,
                                title: "Will do later",
                                accessibilityScope: "habits-card-local"
                            )
                        }
                    }
                }
            }
        }
        .navigationTitle("Habits")
        .sheet(item: $editor) { presentation in
            HabitOutcomeEditor(presentation: presentation)
                .environmentObject(habitSync)
        }
    }

    private func canResolveScheduledCanonicalOccurrence(_ habit: ScheduleBlock) -> Bool {
        guard habit.status == .scheduled,
              let itemID = habit.sourceItemID,
              habit.occurrenceID != nil,
              let item = store.canonicalItem(id: itemID) else { return false }
        return item.recurrence != nil
            && item.revision == habit.sourceItemRevision
            && store.canonicalAuthoringMutation(itemID: itemID) == nil
            && store.executionState.pendingCommand == nil
    }

    private func conflictLabel(_ pending: DayWeavePendingHabitMutation) -> String {
        switch pending {
        case .outcome:
            "Occurrence correction"
        case .pauseStart:
            "Pause request"
        case .pauseResume:
            "Resume request"
        }
    }
}

private struct HabitSyncStatusBanner: View {
    let status: HabitSyncStatus
    let retry: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: symbol)
                .foregroundStyle(color)
            Text(status.message)
                .font(.subheadline)
                .foregroundStyle(.secondary)
            Spacer()
            if !status.isBusy && status.phase != .online && status.phase != .locked {
                Button("Retry", action: retry)
                    .controlSize(.small)
                    .accessibilityIdentifier("habits.sync.retry")
            }
            if status.isBusy { ProgressView().controlSize(.small) }
        }
        .padding(12)
        .background(color.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityIdentifier("habits.sync.status")
    }

    private var symbol: String {
        switch status.phase {
        case .online: "checkmark.icloud"
        case .syncing: "arrow.triangle.2.circlepath"
        case .offline: "icloud.slash"
        case .attentionRequired: "exclamationmark.triangle"
        case .authenticationRequired: "person.crop.circle.badge.exclamationmark"
        case .failed: "xmark.shield"
        case .locked: "lock.fill"
        case .ready: "icloud"
        }
    }

    private var color: Color {
        switch status.phase {
        case .online: .green
        case .attentionRequired, .authenticationRequired: .orange
        case .failed: .red
        default: .accentColor
        }
    }
}

private struct CanonicalHabitOccurrenceCard: View {
    @EnvironmentObject private var habitSync: HabitSyncStore
    let row: HabitScheduleOccurrenceRow
    let timezoneName: String
    let edit: (DayWeaveHabitOutcomeStatus, Bool) -> Void

    private var occurrence: DayWeaveHabitOccurrence { row.occurrence }
    private var outcome: DayWeaveHabitOutcome? { occurrence.outcome }
    private var pending: DayWeavePendingHabitMutation? {
        habitSync.pendingMutation(forOccurrenceID: occurrence.id)
    }
    private var pause: DayWeaveHabitPause? {
        habitSync.openPause(for: occurrence.evidence.habitID)
    }
    private var pendingPause: DayWeavePendingHabitMutation? {
        habitSync.pendingPauseMutation(forHabitID: occurrence.evidence.habitID)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 10) {
                Image(systemName: outcome?.status.symbol ?? "circle")
                    .font(.title3)
                    .foregroundStyle(statusColor)
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.block.title).font(.headline)
                    Text(row.block.timeRange(timezoneName: timezoneName))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text(outcome?.status.title ?? "Not logged")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(statusColor)
            }

            if let outcome {
                HabitOutcomeEvidenceLine(outcome: outcome)
                if let note = outcome.note {
                    Text(note)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .privacySensitive()
                        .accessibilityIdentifier("habit.outcome.note.\(occurrence.id.uuidString.lowercased())")
                }
            } else {
                Text("Any effort counts. Log what happened when this window is over—or whenever it feels useful.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            if let pending {
                HStack {
                    Label(
                        pending.conflictDetected
                            ? "Newer server progress needs review"
                            : "Saved securely; waiting to sync",
                        systemImage: pending.conflictDetected
                            ? "exclamationmark.triangle.fill" : "lock.icloud"
                    )
                    .font(.caption)
                    .foregroundStyle(pending.conflictDetected ? .orange : .secondary)
                    Spacer()
                    if pending.conflictDetected {
                        Button("Use server version") {
                            Task { await habitSync.discardPendingMutation(pending.id) }
                        }
                        .controlSize(.small)
                        .accessibilityIdentifier("habit.conflict.use-server.\(occurrence.id.uuidString.lowercased())")
                    }
                }
            }

            HStack(spacing: 8) {
                Button {
                    Task {
                        _ = await habitSync.record(
                            .completed(
                                quantity: occurrence.evidence.expectedQuantity,
                                unit: occurrence.evidence.expectedUnit
                            ),
                            for: occurrence
                        )
                    }
                } label: {
                    Label("Complete", systemImage: "checkmark")
                }
                .buttonStyle(.borderedProminent)
                .disabled(pending != nil || habitSync.status.isBusy)
                .accessibilityIdentifier("habit.complete.\(occurrence.id.uuidString.lowercased())")

                Button("Partial") { edit(.partial, false) }
                    .disabled(pending != nil || habitSync.status.isBusy)
                    .accessibilityIdentifier("habit.partial.\(occurrence.id.uuidString.lowercased())")
                Button("Skip") { edit(.skipped, false) }
                    .disabled(pending != nil || habitSync.status.isBusy)
                    .accessibilityIdentifier("habit.skip.\(occurrence.id.uuidString.lowercased())")
                if let outcome {
                    Button("Correct") { edit(outcome.status, true) }
                        .disabled(pending != nil || habitSync.status.isBusy)
                        .accessibilityIdentifier("habit.correct.\(occurrence.id.uuidString.lowercased())")
                }
                Spacer()
                if let pause {
                    Button {
                        Task { await habitSync.resume(pause) }
                    } label: {
                        Label("Resume habit", systemImage: "play.fill")
                    }
                    .disabled(pendingPause != nil || habitSync.status.isBusy)
                    .accessibilityIdentifier("habit.resume.\(occurrence.evidence.habitID.uuidString.lowercased())")
                } else {
                    Button {
                        Task { await habitSync.pause(habitID: occurrence.evidence.habitID) }
                    } label: {
                        Label("Pause habit", systemImage: "pause.fill")
                    }
                    .disabled(pendingPause != nil || habitSync.status.isBusy)
                    .accessibilityIdentifier("habit.pause.\(occurrence.evidence.habitID.uuidString.lowercased())")
                }
            }
            .controlSize(.small)
        }
        .padding(16)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
        .overlay(alignment: .leading) {
            RoundedRectangle(cornerRadius: 3)
                .fill(statusColor)
                .frame(width: 4)
                .padding(.vertical, 8)
        }
        .privacySensitive(row.block.isSensitive)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("habit.card.\(occurrence.id.uuidString.lowercased())")
    }

    private var statusColor: Color {
        switch outcome?.status {
        case .completed: .green
        case .partial: .blue
        case .skipped: .orange
        case .unresolved, nil: .secondary
        }
    }
}

private struct HabitOutcomeEvidenceLine: View {
    let outcome: DayWeaveHabitOutcome

    var body: some View {
        HStack(spacing: 14) {
            if outcome.status == .partial || (outcome.status == .skipped && outcome.progressBasisPoints > 0) {
                Label("\(Int(outcome.progressBasisPoints) / 100)%", systemImage: "chart.bar.fill")
            }
            if let quantity = outcome.quantity, let unit = outcome.unit {
                Label("\(quantity) \(unit)", systemImage: "number")
            }
            if let seconds = outcome.actualSeconds {
                Label(HabitStatisticsPresentation.durationLabel(seconds), systemImage: "timer")
            }
            Text("Updated \(outcome.updatedAt.formatted(date: .abbreviated, time: .shortened))")
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}

private struct HabitOutcomeEditor: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var habitSync: HabitSyncStore
    let presentation: HabitOutcomeEditorPresentation

    @State private var status: DayWeaveHabitOutcomeStatus
    @State private var progressPercent: Int
    @State private var quantity: String
    @State private var unit: String
    @State private var actualMinutes: String
    @State private var note: String
    @State private var isSaving = false
    @State private var validationMessage: String?

    init(presentation: HabitOutcomeEditorPresentation) {
        self.presentation = presentation
        let outcome = presentation.isCorrection ? presentation.occurrence.outcome : nil
        _status = State(initialValue: presentation.initialStatus)
        _progressPercent = State(initialValue: max(
            presentation.initialStatus == .partial ? 50 : 0,
            Int(outcome?.progressBasisPoints ?? 0) / 100
        ))
        _quantity = State(initialValue: outcome?.quantity.map(String.init) ?? "")
        _unit = State(initialValue: outcome?.unit ?? presentation.occurrence.evidence.expectedUnit ?? "")
        _actualMinutes = State(initialValue: outcome?.actualSeconds.map { String($0 / 60) } ?? "")
        _note = State(initialValue: outcome?.note ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 4) {
                Text(presentation.isCorrection ? "Correct habit entry" : "Log habit progress")
                    .font(.title2.weight(.semibold))
                Text(presentation.title)
                    .foregroundStyle(.secondary)
            }

            Picker("Outcome", selection: $status) {
                Text("Completed").tag(DayWeaveHabitOutcomeStatus.completed)
                Text("Partly done").tag(DayWeaveHabitOutcomeStatus.partial)
                Text("Skipped").tag(DayWeaveHabitOutcomeStatus.skipped)
                if presentation.isCorrection {
                    Text("Clear entry").tag(DayWeaveHabitOutcomeStatus.unresolved)
                }
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier("habit.editor.status")

            if status == .partial || status == .skipped {
                Stepper(
                    "Progress kept: \(progressPercent)%",
                    value: $progressPercent,
                    in: status == .partial ? 1...99 : 0...99
                )
                .accessibilityIdentifier("habit.editor.progress")
            }

            if status != .unresolved {
                HStack {
                    TextField("Quantity (optional)", text: $quantity)
                        .accessibilityIdentifier("habit.editor.quantity")
                    TextField("Unit", text: $unit)
                        .accessibilityIdentifier("habit.editor.unit")
                    TextField("Actual minutes", text: $actualMinutes)
                        .accessibilityIdentifier("habit.editor.actual-minutes")
                }
                Text("Skipped entries can keep partial quantity and time. Leaving quantity blank also clears its unit.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Text("Note (optional)").font(.caption.weight(.semibold))
                TextEditor(text: $note)
                    .font(.body)
                    .frame(height: 84)
                    .padding(6)
                    .background(.background, in: RoundedRectangle(cornerRadius: 8))
                    .overlay { RoundedRectangle(cornerRadius: 8).stroke(.quaternary) }
                    .accessibilityIdentifier("habit.editor.note")
            }

            if let validationMessage {
                Label(validationMessage, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            HStack {
                Text("Corrections preserve the occurrence history and advance its server revision.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(presentation.isCorrection ? "Save correction" : "Save") {
                    save()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(isSaving)
                .accessibilityIdentifier("habit.editor.save")
            }
        }
        .padding(24)
        .frame(width: 580)
        .privacySensitive()
        .onChange(of: status) { _, newValue in
            if newValue == .partial && progressPercent == 0 { progressPercent = 50 }
            validationMessage = nil
        }
    }

    private func save() {
        guard let input = makeInput() else { return }
        isSaving = true
        validationMessage = nil
        Task {
            let outcome = await habitSync.record(input, for: presentation.occurrence)
            isSaving = false
            switch outcome {
            case .success, .offline:
                dismiss()
            case .conflict:
                validationMessage = "Newer progress exists. Close this sheet and review the conflict."
            case .localStorageFailure:
                validationMessage = "Encrypted storage is unavailable; nothing was sent."
            default:
                validationMessage = habitSync.status.message
            }
        }
    }

    private func makeInput() -> DayWeaveHabitOutcomeInput? {
        if status == .unresolved {
            return .init(
                status: .unresolved,
                progressBasisPoints: 0,
                occurredAt: Date()
            )
        }
        let trimmedQuantity = quantity.trimmingCharacters(in: .whitespacesAndNewlines)
        let parsedQuantity: Int64?
        if trimmedQuantity.isEmpty {
            parsedQuantity = nil
        } else if let value = Int64(trimmedQuantity),
                  value >= -DayWeaveHabitOutcomeInput.maximumQuantity,
                  value <= DayWeaveHabitOutcomeInput.maximumQuantity {
            parsedQuantity = value
        } else {
            validationMessage = "Quantity must be a whole number from −1,000,000,000,000 through 1,000,000,000,000."
            return nil
        }
        let trimmedUnit = unit.trimmingCharacters(in: .whitespacesAndNewlines)
        if parsedQuantity != nil && trimmedUnit.isEmpty {
            validationMessage = "Add a unit for the quantity."
            return nil
        }

        let trimmedMinutes = actualMinutes.trimmingCharacters(in: .whitespacesAndNewlines)
        let seconds: UInt64?
        if trimmedMinutes.isEmpty {
            seconds = nil
        } else if let minutes = UInt64(trimmedMinutes),
                  minutes <= DayWeaveHabitOutcomeInput.maximumActualSeconds / 60 {
            seconds = minutes * 60
        } else {
            validationMessage = "Actual minutes must be a nonnegative whole number within one year."
            return nil
        }

        let trimmedNote = note.trimmingCharacters(in: .whitespacesAndNewlines)
        let basisPoints: UInt16 = switch status {
        case .completed: 10_000
        case .partial, .skipped: UInt16(progressPercent * 100)
        case .unresolved: 0
        }
        let input = DayWeaveHabitOutcomeInput(
            status: status,
            progressBasisPoints: basisPoints,
            quantity: parsedQuantity,
            unit: parsedQuantity == nil ? nil : trimmedUnit,
            actualSeconds: seconds,
            note: trimmedNote.isEmpty ? nil : trimmedNote,
            occurredAt: Date()
        )
        guard input.hasValidShape else {
            validationMessage = "Check the progress, quantity, time, and note before saving."
            return nil
        }
        return input
    }
}

struct ProjectsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore

    private var groups: [(String, [ScheduleBlock])] {
        Dictionary(grouping: store.blocks.filter(\.contributesToExecutionPresentation)) {
            $0.project ?? "Personal"
        }
            .map { ($0.key, $0.value.sorted { $0.start < $1.start }) }
            .sorted { $0.0.localizedCaseInsensitiveCompare($1.0) == .orderedAscending }
    }

    var body: some View {
        DestinationScroll(title: "Projects", subtitle: "Project progress rolls up from executable leaf work.") {
            ForEach(groups, id: \.0) { name, blocks in
                let completed = blocks.count(where: { $0.status == .completed })
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Image(systemName: "folder.fill").foregroundStyle(.blue)
                        Text(name).font(.title3.weight(.semibold))
                        Spacer()
                        Text("\(completed)/\(blocks.count) done")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    ProgressView(value: Double(completed), total: Double(max(blocks.count, 1)))
                    ForEach(blocks.prefix(4)) { block in
                        Button {
                            store.select(block)
                        } label: {
                            HStack {
                                Image(systemName: block.status == .completed ? "checkmark.circle.fill" : block.kind.symbol)
                                    .foregroundStyle(block.status == .completed ? .green : block.kind.color)
                                Text(block.title).lineLimit(1)
                                Spacer()
                                Text(block.timeRange(
                                    timezoneName: store.schedulePresentationTimezoneName
                                ))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            }
                        }
                        .buttonStyle(.plain)
                        .privacySensitive(block.isSensitive)
                    }
                }
                .padding(16)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
                .privacySensitive(blocks.contains(where: \ScheduleBlock.isSensitive))
            }
        }
        .navigationTitle("Projects")
    }
}

struct GoalsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore

    private var goals: [ScheduleBlock] {
        store.blocks.filter { $0.kind == .goal }.sorted { $0.start < $1.start }
    }

    var body: some View {
        DestinationScroll(title: "Goals", subtitle: "Outcomes can remain unscheduled; only their executable leaf actions reserve time.") {
            if goals.isEmpty {
                DestinationEmpty(title: "No goals yet", symbol: "scope", action: "Capture a goal") {
                    store.isQuickAddPresented = true
                }
            } else {
                ForEach(goals) { goal in
                    PlanningCard(
                        block: goal,
                        detail: goal.notes,
                        timezoneName: store.schedulePresentationTimezoneName
                    ) {
                        Button("Open") { store.select(goal) }
                        Button("Complete") { store.complete(goal.id) }
                            .disabled(!store.canMutate(goal))
                    }
                }
            }

            VStack(alignment: .leading, spacing: 10) {
                Label("Goal scheduling rule", systemImage: "info.circle")
                    .font(.headline)
                Text("A goal without a duration is an outcome. Add leaf tasks, milestones, routines, or habits to turn it into calendar demand.")
                    .foregroundStyle(.secondary)
            }
            .padding(16)
            .background(Color.purple.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
        }
        .navigationTitle("Goals")
    }
}

private enum HabitStatisticsRange: Int, CaseIterable, Identifiable {
    case month = 30
    case quarter = 90
    case year = 365

    var id: Int { rawValue }
    var title: String {
        switch self {
        case .month: "30 days"
        case .quarter: "90 days"
        case .year: "1 year"
        }
    }

    var bucket: DayWeaveHabitAnalyticsBucket {
        switch self {
        case .month: .day
        case .quarter: .week
        case .year: .month
        }
    }
}

struct StatisticsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var habitSync: HabitSyncStore
    @State private var range: HabitStatisticsRange = .quarter

    private var todayExecutionBlocks: [ScheduleBlock] {
        PlannerPresentation.blocks(
            store.blocks,
            intersectingDayContaining: Date(),
            timezoneName: store.schedulePresentationTimezoneName
        )
        .filter(\.contributesToExecutionPresentation)
    }

    private var habitItems: [DayWeaveCanonicalItem] {
        store.canonicalItems
            .filter { $0.kind == .habit && $0.deletedAt == nil }
            .sorted { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedAscending }
    }

    private var displayedAnalytics: [DayWeaveHabitAnalytics] {
        let ids = Set(habitItems.map(\.id))
        return habitSync.analytics.filter { ids.contains($0.habitID) }
    }

    private var analyticsRange: (DayWeaveLocalDate, DayWeaveLocalDate)? {
        let timezoneName = store.schedulePresentationTimezoneName
        let calendar = PlannerPresentation.calendar(timezoneName: timezoneName)
        let end = calendar.startOfDay(for: Date())
        guard let start = calendar.date(byAdding: .day, value: -(range.rawValue - 1), to: end),
              let startDate = DayWeaveLocalDate.containing(start, timezoneName: timezoneName),
              let endDate = DayWeaveLocalDate.containing(end, timezoneName: timezoneName) else {
            return nil
        }
        return (startDate, endDate)
    }

    var body: some View {
        let blocks = todayExecutionBlocks
        let plannedMinutes = blocks.reduce(0) { $0 + $1.durationMinutes }
        let completedBlocks = blocks.filter { $0.status == .completed }
        let actualMinutes = completedBlocks.reduce(0) {
            $0 + ($1.actualMinutes ?? $1.durationMinutes)
        }
        let canonicalAnalytics = displayedAnalytics
        let adherence = HabitStatisticsPresentation.adherencePercent(canonicalAnalytics)
        let completedOccurrences = canonicalAnalytics.reduce(UInt64(0)) {
            $0 + $1.totals.completed
        }
        let eligibleOccurrences = canonicalAnalytics.reduce(UInt64(0)) {
            $0 + $1.totals.eligible
        }
        let currentStreak = canonicalAnalytics.map(\.currentStreak).max() ?? 0

        DestinationScroll(
            title: "Statistics",
            subtitle: "Private, deterministic habit trends alongside today’s execution summary. Rest days preserved by habit pauses are excused."
        ) {
            HStack {
                Picker("Habit range", selection: $range) {
                    ForEach(HabitStatisticsRange.allCases) { value in
                        Text(value.title).tag(value)
                    }
                }
                .pickerStyle(.segmented)
                .frame(maxWidth: 320)
                .accessibilityIdentifier("statistics.habits.range")
                Spacer()
                Button {
                    refreshHabitAnalytics()
                } label: {
                    Label("Refresh trends", systemImage: "arrow.clockwise")
                }
                .disabled(habitSync.status.isBusy || habitItems.isEmpty)
                .accessibilityIdentifier("statistics.habits.refresh")
            }

            SummaryStrip(metrics: [
                ("\(adherence)%", "habit adherence", "chart.line.uptrend.xyaxis"),
                ("\(completedOccurrences)/\(eligibleOccurrences)", "occurrences", "checkmark.circle"),
                (HabitStatisticsPresentation.streakLabel(currentStreak), "best active streak", "flame"),
            ])

            HabitSyncStatusBanner(status: habitSync.status) {
                refreshHabitAnalytics()
            }

            if habitItems.isEmpty {
                StatisticsSection(title: "Habit trends") {
                    Text("Create and schedule a habit to begin an adherence history.")
                        .foregroundStyle(.secondary)
                }
            } else if canonicalAnalytics.isEmpty {
                StatisticsSection(title: "Habit trends") {
                    HStack(spacing: 12) {
                        ProgressView().controlSize(.small)
                        Text("Canonical trends will appear after the encrypted habit ledger finishes its first sync.")
                            .foregroundStyle(.secondary)
                    }
                }
            } else {
                ForEach(habitItems) { item in
                    if let value = canonicalAnalytics.first(where: { $0.habitID == item.id }) {
                        HabitAnalyticsCard(item: item, analytics: value)
                    }
                }
            }

            SummaryStrip(metrics: [
                ("\(plannedMinutes)m", "planned today", "calendar"),
                ("\(actualMinutes)m", "completed today", "timer"),
                ("\(completedBlocks.count)", "items done", "checkmark.circle"),
            ])

            StatisticsSection(title: "Today’s status") {
                ForEach(PlannerItemStatus.allCases, id: \.self) { status in
                    let count = blocks.count(where: { $0.status == status })
                    if count > 0 {
                        DistributionRow(
                            label: status.title,
                            value: count,
                            total: max(blocks.count, 1),
                            color: status == .completed ? .green : .accentColor
                        )
                    }
                }
            }

            StatisticsSection(title: "Today’s energy demand") {
                ForEach(EnergyLevel.allCases) { energy in
                    let minutes = blocks
                        .filter { $0.energy == energy }
                        .reduce(0) { $0 + $1.durationMinutes }
                    DistributionRow(
                        label: energy.title,
                        value: minutes,
                        total: max(plannedMinutes, 1),
                        valueSuffix: "m",
                        color: energy == .deep ? .indigo : energy == .medium ? .orange : .mint
                    )
                }
            }
        }
        .navigationTitle("Statistics")
        .task(id: range) {
            refreshHabitAnalytics()
        }
    }

    private func refreshHabitAnalytics() {
        guard let dates = analyticsRange, !habitItems.isEmpty else { return }
        let ids = habitItems.map(\.id)
        Task {
            _ = await habitSync.refreshAnalytics(
                habitIDs: ids,
                startDate: dates.0,
                endDate: dates.1,
                bucket: range.bucket
            )
        }
    }
}

private struct HabitAnalyticsCard: View {
    let item: DayWeaveCanonicalItem
    let analytics: DayWeaveHabitAnalytics

    private var adherence: Int {
        Int((UInt32(analytics.totals.adherenceBasisPoints) + 50) / 100)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Image(systemName: "repeat.circle.fill")
                    .font(.title3)
                    .foregroundStyle(.green)
                VStack(alignment: .leading, spacing: 2) {
                    Text(item.title).font(.headline)
                    Text("\(analytics.startDate.rawValue) – \(analytics.endDate.rawValue)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 2) {
                    Text("\(adherence)%").font(.title3.weight(.semibold))
                    Text("adherence").font(.caption).foregroundStyle(.secondary)
                }
            }

            HabitTrendBars(trends: analytics.trends)
                .frame(height: 70)

            HStack(spacing: 18) {
                Label(
                    "Current \(HabitStatisticsPresentation.streakLabel(analytics.currentStreak))",
                    systemImage: "flame.fill"
                )
                Label(
                    "Longest \(HabitStatisticsPresentation.streakLabel(analytics.longestStreak))",
                    systemImage: "trophy.fill"
                )
                if analytics.totals.actualSecondsTotal > 0 {
                    Label(
                        HabitStatisticsPresentation.durationLabel(analytics.totals.actualSecondsTotal),
                        systemImage: "timer"
                    )
                }
                Spacer()
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            if !analytics.totals.quantityTotals.isEmpty {
                Text(analytics.totals.quantityTotals.map { "\($0.amount) \($0.unit)" }.joined(separator: " · "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            ForEach(analytics.supportiveFactCodes, id: \.self) { fact in
                Label(fact.message, systemImage: "heart.text.square")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(16)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
        .privacySensitive(item.isSensitive)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("statistics.habit.\(item.id.uuidString.lowercased())")
    }
}

private struct HabitTrendBars: View {
    let trends: [DayWeaveHabitTrendBucket]

    var body: some View {
        GeometryReader { geometry in
            HStack(alignment: .bottom, spacing: max(2, min(8, geometry.size.width / 80))) {
                ForEach(Array(trends.enumerated()), id: \.offset) { _, trend in
                    let fraction = trend.totals.eligible == 0
                        ? 0
                        : Double(trend.totals.adherenceBasisPoints) / 10_000
                    RoundedRectangle(cornerRadius: 3)
                        .fill(fraction >= 0.8 ? Color.green : fraction > 0 ? Color.accentColor : Color.secondary.opacity(0.25))
                        .frame(
                            maxWidth: .infinity,
                            minHeight: 4,
                            maxHeight: max(4, geometry.size.height * fraction)
                        )
                        .accessibilityLabel("\(trend.startDate.rawValue) through \(trend.endDate.rawValue), \(Int(fraction * 100)) percent")
                }
            }
            .frame(maxHeight: .infinity, alignment: .bottom)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("statistics.habit.trend")
    }
}

private struct DestinationScroll<Content: View>: View {
    let title: String
    let subtitle: String
    @ViewBuilder let content: Content

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.title2.weight(.semibold))
                    Text(subtitle).font(.subheadline).foregroundStyle(.secondary)
                }
                .padding(.bottom, 4)
                content
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(20)
        }
    }
}

private struct SummaryStrip: View {
    let metrics: [(String, String, String)]

    var body: some View {
        HStack(spacing: 10) {
            ForEach(Array(metrics.enumerated()), id: \.offset) { _, metric in
                HStack(spacing: 9) {
                    Image(systemName: metric.2).foregroundStyle(.tint)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(metric.0).font(.headline)
                        Text(metric.1).font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                }
                .padding(13)
                .frame(maxWidth: .infinity)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
            }
        }
    }
}

private struct PlanningCard<Actions: View>: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock
    let detail: String
    let timezoneName: String
    @ViewBuilder let actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: block.kind.symbol).foregroundStyle(block.kind.color)
                Text(block.title).font(.headline)
                Spacer()
                Text(block.status.title).font(.caption).foregroundStyle(.secondary)
            }
            HStack(spacing: 12) {
                Label(
                    block.timeRange(timezoneName: timezoneName),
                    systemImage: "clock"
                )
                Label("\(block.durationMinutes)m", systemImage: "timer")
                Label(block.energy.title, systemImage: "bolt")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            if !detail.isEmpty {
                Text(detail).font(.subheadline).foregroundStyle(.secondary)
            }
            HStack { actions }
                .controlSize(.small)
        }
        .padding(16)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
        .onTapGesture { store.select(block) }
        .privacySensitive(block.isSensitive)
    }
}

private struct DestinationEmpty: View {
    let title: String
    let symbol: String
    let action: String
    let perform: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: symbol).font(.largeTitle).foregroundStyle(.secondary)
            Text(title).font(.headline)
            Button(action, action: perform).buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity)
        .padding(36)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
    }
}

private struct StatisticsSection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.headline)
            content
        }
        .padding(16)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
    }
}

private struct DistributionRow: View {
    let label: String
    let value: Int
    let total: Int
    var valueSuffix = ""
    let color: Color

    var body: some View {
        HStack(spacing: 12) {
            Text(label).frame(width: 100, alignment: .leading)
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(.quaternary)
                    Capsule()
                        .fill(color)
                        .frame(width: geometry.size.width * min(1, Double(value) / Double(max(total, 1))))
                }
            }
            .frame(height: 8)
            Text("\(value)\(valueSuffix)")
                .font(.system(.caption, design: .monospaced))
                .frame(width: 54, alignment: .trailing)
        }
    }
}
