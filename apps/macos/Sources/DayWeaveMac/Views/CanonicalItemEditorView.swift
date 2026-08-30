import SwiftUI

enum CanonicalItemEditorMode: Equatable, Sendable {
    case create(itemID: UUID)
    case replace(itemID: UUID, draft: DayWeaveCanonicalItemDraft)
    case updatePending(
        mutationID: UUID,
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    )

    var itemID: UUID {
        switch self {
        case let .create(itemID), let .replace(itemID, _), let .updatePending(_, itemID, _):
            itemID
        }
    }

    var initialDraft: DayWeaveCanonicalItemDraft? {
        switch self {
        case .create: nil
        case let .replace(_, draft), let .updatePending(_, _, draft): draft
        }
    }

    var title: String {
        switch self {
        case .create: "New item"
        case .replace: "Edit item"
        case .updatePending: "Edit queued item"
        }
    }

    var actionTitle: String {
        switch self {
        case .create: "Add to Inbox"
        case .replace: "Queue changes"
        case .updatePending: "Update queued change"
        }
    }
}

struct CanonicalItemEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var store: PlannerStore
    @FocusState private var titleIsFocused: Bool

    let mode: CanonicalItemEditorMode
    let externalReadOnlyDiagnostic: String?
    @State private var state: CanonicalItemEditorState
    @State private var saveError: String?

    init(
        mode: CanonicalItemEditorMode,
        readOnlyDiagnostic: String? = nil,
        now: Date = Date()
    ) {
        self.mode = mode
        externalReadOnlyDiagnostic = readOnlyDiagnostic
        _state = State(initialValue: CanonicalItemEditorState(
            itemID: mode.itemID,
            draft: mode.initialDraft,
            now: now
        ))
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let readOnlyDiagnostic {
                        diagnosticBanner(
                            readOnlyDiagnostic,
                            symbol: "lock.shield",
                            color: .orange
                        )
                    } else if let issue = localValidationIssue {
                        diagnosticBanner(issue, symbol: "exclamationmark.triangle", color: .orange)
                    }
                    if let saveError {
                        diagnosticBanner(saveError, symbol: "xmark.octagon", color: .red)
                    }

                    identitySection
                    planningSection
                    if state.kind == .event { eventSection }
                    if state.supportsRecurrence { recurrenceSection }
                    constraintsSection
                    hierarchySection
                }
                .padding(20)
                .disabled(readOnlyDiagnostic != nil)
            }
            Divider()
            footer
        }
        .frame(minWidth: 680, idealWidth: 720, minHeight: 680, idealHeight: 780)
        .onAppear {
            if case .create = mode { titleIsFocused = true }
        }
        .onChange(of: state.kind) { _, _ in
            state.normalizeForKindChange()
        }
        .accessibilityIdentifier("canonical-editor")
    }

    private var header: some View {
        HStack(spacing: 14) {
            Image(systemName: "square.and.pencil")
                .font(.title2)
                .foregroundStyle(.tint)
                .frame(width: 42, height: 42)
                .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 11))
            VStack(alignment: .leading, spacing: 3) {
                Text(mode.title).font(.title2.weight(.semibold))
                Text("Saved locally first. Sync applies the exact queued change later.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Cancel") { dismiss() }
                .keyboardShortcut(.cancelAction)
                .accessibilityIdentifier("canonical-editor.cancel")
        }
        .padding(20)
    }

    private var identitySection: some View {
        CanonicalEditorSection(title: "Identity", symbol: "text.cursor") {
            TextField("Title", text: $state.title)
                .textFieldStyle(.roundedBorder)
                .font(.title3)
                .focused($titleIsFocused)
                .privacySensitive(requiresSensitivePresentation)
                .accessibilityIdentifier("canonical-editor.title")
            HStack {
                Text("\(state.title.unicodeScalars.count)/\(DayWeaveCanonicalItemDraft.maximumTitleScalars) characters")
                Spacer()
                Text(state.isSensitive ? "Private title" : "Standard privacy")
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            Text("Notes")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            TextEditor(text: $state.notes)
                .font(.body)
                .frame(minHeight: 80)
                .padding(6)
                .background(
                    Color(nsColor: .textBackgroundColor),
                    in: RoundedRectangle(cornerRadius: 8)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(.separator, lineWidth: 1)
                }
                .privacySensitive(requiresSensitivePresentation)
                .accessibilityIdentifier("canonical-editor.notes")

            HStack(spacing: 16) {
                Picker("Type", selection: $state.kind) {
                    Label("Task", systemImage: "checkmark.circle").tag(DayWeaveCanonicalItemKind.task)
                    Label("Habit", systemImage: "repeat").tag(DayWeaveCanonicalItemKind.habit)
                    Label("Routine", systemImage: "list.number").tag(DayWeaveCanonicalItemKind.routine)
                    Label("Goal", systemImage: "target").tag(DayWeaveCanonicalItemKind.goal)
                    Label("Event", systemImage: "calendar").tag(DayWeaveCanonicalItemKind.event)
                    Label("Break", systemImage: "cup.and.saucer").tag(DayWeaveCanonicalItemKind.breakTime)
                }
                .accessibilityIdentifier("canonical-editor.kind")

                Picker("Ready state", selection: $state.readyStatus) {
                    Text("Inbox — decide later").tag(DayWeaveCanonicalItemStatus.inbox)
                    Text("Planned — ready to compose").tag(DayWeaveCanonicalItemStatus.planned)
                }
                .accessibilityIdentifier("canonical-editor.status")
            }

            Toggle(isOn: $state.isSensitive) {
                VStack(alignment: .leading, spacing: 2) {
                    Label("Sensitive", systemImage: "checkmark.shield")
                    Text("Excluded from assistant context except for anonymous occupied time.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .accessibilityIdentifier("canonical-editor.sensitive")
        }
    }

    private var planningSection: some View {
        CanonicalEditorSection(title: "Planning", symbol: "calendar.badge.clock") {
            if state.kind != .event {
                Toggle("Estimate a duration", isOn: $state.hasDuration)
                    .accessibilityIdentifier("canonical-editor.duration.enabled")
                if state.hasDuration {
                    Stepper(value: durationMinutes, in: 1...527_040, step: 5) {
                        LabeledContent(
                            "Duration",
                            value: CanonicalItemEditorState.durationDescription(
                                state.durationSeconds
                            )
                        )
                    }
                    .accessibilityIdentifier("canonical-editor.duration")
                }
                if state.kind == .goal || state.kind == .routine {
                    Toggle("Schedule this container's own effort", isOn: $state.hasOwnEffort)
                        .accessibilityIdentifier("canonical-editor.own-effort")
                    Text(state.hasOwnEffort
                        ? "Its duration is calendar demand in addition to any subtasks."
                        : "Only its schedulable subtasks contribute calendar demand.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Toggle("Earliest start", isOn: $state.hasEarliestStart)
            if state.hasEarliestStart {
                DatePicker(
                    "May start after",
                    selection: $state.earliestStart,
                    displayedComponents: [.date, .hourAndMinute]
                )
                .accessibilityIdentifier("canonical-editor.earliest")
            }

            Toggle("Deadline", isOn: $state.hasDeadline)
            if state.hasDeadline {
                DatePicker(
                    "Finish by",
                    selection: $state.deadline,
                    displayedComponents: [.date, .hourAndMinute]
                )
                .accessibilityIdentifier("canonical-editor.deadline")
            }

            TextField("IANA timezone", text: timezoneName)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("canonical-editor.timezone")

            scoreEditor(title: "Importance", value: $state.importance)
            scoreEditor(title: "Urgency", value: $state.urgency)
        }
    }

    private var eventSection: some View {
        CanonicalEditorSection(title: "Event time", symbol: "calendar") {
            Toggle("All-day event", isOn: eventIsAllDay)
            DatePicker(
                "Starts",
                selection: eventStart,
                displayedComponents: state.eventIsAllDay ? [.date] : [.date, .hourAndMinute]
            )
            .environment(\.timeZone, editorTimeZone)
            .accessibilityIdentifier("canonical-editor.event.start")
            DatePicker(
                "Ends",
                selection: eventEnd,
                displayedComponents: state.eventIsAllDay ? [.date] : [.date, .hourAndMinute]
            )
            .environment(\.timeZone, editorTimeZone)
            .accessibilityIdentifier("canonical-editor.event.end")
            Label("This owned event reserves a fixed time range.", systemImage: "pin.fill")
                .font(.caption)
                .foregroundStyle(.secondary)
            Toggle("Publish as tentative", isOn: $state.eventIsTentative)
                .accessibilityIdentifier("canonical-editor.event.tentative")
            Toggle("Show as busy", isOn: $state.eventIsBusy)
                .accessibilityIdentifier("canonical-editor.event.busy")
            Text("Calendar-linked events are read-only here and must be changed in their source calendar.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var recurrenceSection: some View {
        CanonicalEditorSection(title: "Recurrence", symbol: "repeat") {
            Picker("Repeats", selection: $state.recurrence) {
                ForEach(CanonicalItemEditorRecurrence.allCases) { option in
                    Text(option.title).tag(option)
                }
            }
            .accessibilityIdentifier("canonical-editor.recurrence")

            switch state.recurrence {
            case .none:
                Text(state.kind == .habit
                    ? "Habits need a repeat rule before they can be saved."
                    : "This item is scheduled once.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            case .daily:
                Stepper(value: recurrenceCount, in: 1...64) {
                    Text("\(state.recurrenceCount) time\(state.recurrenceCount == 1 ? "" : "s") per day")
                }
            case .weekly:
                Stepper(value: recurrenceCount, in: 1...64) {
                    Text("\(state.recurrenceCount) time\(state.recurrenceCount == 1 ? "" : "s") per week")
                }
                weekdayPicker
            case .monthly:
                Stepper(value: recurrenceCount, in: 1...64) {
                    Text("\(state.recurrenceCount) time\(state.recurrenceCount == 1 ? "" : "s") per month")
                }
            case .everyInterval, .afterCompletion:
                Stepper(value: intervalMinutes, in: 1...5_256_000, step: 15) {
                    LabeledContent(
                        "Interval",
                        value: CanonicalItemEditorState.durationDescription(
                            state.recurrenceIntervalMinutes * 60
                        )
                    )
                }
                Text(state.recurrence == .afterCompletion
                    ? "The next occurrence is anchored to completion."
                    : "Occurrences use a rolling interval in minutes.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var weekdayPicker: some View {
        HStack(spacing: 6) {
            ForEach(CanonicalItemEditorWeekday.allCases) { weekday in
                Button(weekday.shortTitle) {
                    if state.weekdays.contains(weekday) {
                        state.weekdays.remove(weekday)
                    } else {
                        state.weekdays.insert(weekday)
                    }
                }
                .buttonStyle(.bordered)
                .tint(state.weekdays.contains(weekday) ? .accentColor : .secondary)
                .help(weekday.title)
                .accessibilityLabel(weekday.title)
                .accessibilityValue(state.weekdays.contains(weekday) ? "Selected" : "Not selected")
            }
        }
        .accessibilityIdentifier("canonical-editor.weekdays")
    }

    private var constraintsSection: some View {
        CanonicalEditorSection(title: "Flexible constraints", symbol: "slider.horizontal.3") {
            if state.kind != .event {
                Picker("Energy", selection: $state.energy) {
                    ForEach(CanonicalItemEditorEnergy.allCases) { energy in
                        Text(energy.title).tag(energy)
                    }
                }
                .accessibilityIdentifier("canonical-editor.energy")

                Toggle("Allow splitting across sessions", isOn: $state.isSplittable)
                    .accessibilityIdentifier("canonical-editor.split.enabled")
                if state.isSplittable {
                    Stepper(value: minimumChunkMinutes, in: 1...527_040, step: 5) {
                        LabeledContent(
                            "Minimum session",
                            value: CanonicalItemEditorState.durationDescription(
                                state.minimumChunkSeconds
                            )
                        )
                    }
                    Stepper(value: maximumChunkMinutes, in: 1...527_040, step: 5) {
                        LabeledContent(
                            "Maximum session",
                            value: CanonicalItemEditorState.durationDescription(
                                state.maximumChunkSeconds
                            )
                        )
                    }
                }
            }
        }
    }

    private var hierarchySection: some View {
        CanonicalEditorSection(title: "Hierarchy", symbol: "point.3.connected.trianglepath.dotted") {
            Picker("Parent", selection: $state.parentID) {
                Text("No parent").tag(nil as UUID?)
                ForEach(parentOptions) { option in
                    Text(parentOptionTitle(option)).tag(option.id as UUID?)
                }
            }
            .accessibilityIdentifier("canonical-editor.parent")
            Text("The picker excludes this item and every descendant, at any depth.")
                .font(.caption)
                .foregroundStyle(.secondary)

            Stepper(value: siblingOrder, in: 0...1_000_000) {
                LabeledContent("Sibling order", value: String(state.siblingOrder))
            }
            .accessibilityIdentifier("canonical-editor.sibling-order")
        }
    }

    private var footer: some View {
        HStack {
            if !store.canMutatePlan {
                Label("Planner changes are temporarily locked", systemImage: "lock")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button(mode.actionTitle, action: save)
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canSave)
                .accessibilityIdentifier("canonical-editor.save")
        }
        .padding(20)
    }

    private var readOnlyDiagnostic: String? {
        externalReadOnlyDiagnostic ?? state.readOnlyDiagnostic
    }

    private var parentOptions: [CanonicalItemEditorParentOption] {
        CanonicalItemEditorState.parentOptions(
            canonicalItems: store.canonicalItems,
            pendingMutations: store.pendingCanonicalAuthoringMutations,
            excluding: mode.itemID
        )
    }

    private var parentSelectionIsValid: Bool {
        guard let parentID = state.parentID else { return true }
        return parentOptions.contains { $0.id == parentID }
    }

    /// Protect edited text immediately when either the draft, its selected
    /// ancestry, or the durable pre-edit item is sensitive. Removing a marker
    /// never weakens presentation before the queued server change is proven.
    private var requiresSensitivePresentation: Bool {
        if state.isSensitive { return true }
        if let parentID = state.parentID,
           store.canonicalItemRequiresSensitivePresentation(itemID: parentID) {
            return true
        }
        switch mode {
        case .create:
            return false
        case .replace, .updatePending:
            return store.canonicalItemRequiresSensitivePresentation(itemID: mode.itemID)
        }
    }

    private var localValidationIssue: String? {
        if !parentSelectionIsValid {
            return "Choose an available Inbox or Planned parent, or remove the parent."
        }
        return state.validationIssue
    }

    private var canSave: Bool {
        guard store.canMutatePlan,
              readOnlyDiagnostic == nil,
              localValidationIssue == nil else { return false }
        return mode.initialDraft?.normalized != state.draft || mode.initialDraft == nil
    }

    private func parentOptionTitle(_ option: CanonicalItemEditorParentOption) -> String {
        let visibleDepth = min(option.depth, 5)
        let prefix = String(repeating: "› ", count: visibleDepth)
        let overflow = option.depth > visibleDepth ? "(+\(option.depth - visibleDepth)) " : ""
        return "\(prefix)\(overflow)\(option.title)"
    }

    private func save() {
        guard canSave else { return }
        do {
            switch mode {
            case let .create(itemID):
                try store.enqueueCanonicalCreate(itemID: itemID, draft: state.draft)
            case let .replace(itemID, _):
                try store.enqueueCanonicalReplace(itemID: itemID, draft: state.draft)
            case let .updatePending(mutationID, _, _):
                try store.updateCanonicalAuthoringDraft(mutationID, draft: state.draft)
            }
            store.selectCanonicalItem(mode.itemID)
            dismiss()
        } catch {
            saveError = error.localizedDescription
        }
    }

    @ViewBuilder
    private func diagnosticBanner(_ text: String, symbol: String, color: Color) -> some View {
        Label {
            Text(text).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol)
        }
        .font(.subheadline)
        .foregroundStyle(color)
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(color.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("canonical-editor.diagnostic")
    }

    @ViewBuilder
    private func scoreEditor(title: String, value: Binding<UInt8>) -> some View {
        HStack {
            Text(title)
            Slider(
                value: Binding(
                    get: { Double(value.wrappedValue) },
                    set: { value.wrappedValue = UInt8(max(0, min(100, Int($0.rounded())))) }
                ),
                in: 0...100,
                step: 1
            )
            Text(String(value.wrappedValue))
                .monospacedDigit()
                .frame(width: 28, alignment: .trailing)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("canonical-editor.\(title.lowercased())")
    }

    private var durationMinutes: Binding<Int> {
        secondsBinding(\CanonicalItemEditorState.durationSeconds)
    }

    private var minimumChunkMinutes: Binding<Int> {
        secondsBinding(\CanonicalItemEditorState.minimumChunkSeconds)
    }

    private var maximumChunkMinutes: Binding<Int> {
        secondsBinding(\CanonicalItemEditorState.maximumChunkSeconds)
    }

    private func secondsBinding(
        _ keyPath: WritableKeyPath<CanonicalItemEditorState, UInt32>
    ) -> Binding<Int> {
        Binding(
            get: { max(1, Int(state[keyPath: keyPath]) / 60) },
            set: { value in
                let seconds = min(
                    UInt64(DayWeaveCanonicalItemDraft.maximumDurationSeconds),
                    UInt64(max(1, value)) * 60
                )
                state[keyPath: keyPath] = UInt32(seconds)
            }
        )
    }

    private var recurrenceCount: Binding<Int> {
        Binding(
            get: { Int(state.recurrenceCount) },
            set: { state.recurrenceCount = UInt32(max(1, min(64, $0))) }
        )
    }

    private var intervalMinutes: Binding<Int> {
        Binding(
            get: { Int(state.recurrenceIntervalMinutes) },
            set: {
                state.recurrenceIntervalMinutes = UInt32(max(
                    1,
                    min(Int(CanonicalItemEditorState.maximumIntervalMinutes), $0)
                ))
            }
        )
    }

    private var siblingOrder: Binding<Int> {
        Binding(
            get: { Int(state.siblingOrder) },
            set: { state.siblingOrder = UInt32(max(0, min(1_000_000, $0))) }
        )
    }

    private var timezoneName: Binding<String> {
        Binding(
            get: { state.timezoneName },
            set: { state.setTimezoneName($0) }
        )
    }

    private var eventIsAllDay: Binding<Bool> {
        Binding(
            get: { state.eventIsAllDay },
            set: { state.setEventIsAllDay($0) }
        )
    }

    private var eventStart: Binding<Date> {
        Binding(
            get: { state.eventStart },
            set: { state.setEventStart($0) }
        )
    }

    private var eventEnd: Binding<Date> {
        Binding(
            get: { state.eventEnd },
            set: { state.setEventEnd($0) }
        )
    }

    private var editorTimeZone: TimeZone {
        DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: state.timezoneName)
            ?? TimeZone.autoupdatingCurrent
    }
}

private struct CanonicalEditorSection<Content: View>: View {
    let title: String
    let symbol: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(title, systemImage: symbol)
                .font(.headline)
                .foregroundStyle(.primary)
            content
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            Color(nsColor: .controlBackgroundColor),
            in: RoundedRectangle(cornerRadius: 14)
        )
    }
}
