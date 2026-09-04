import SwiftUI

enum CanonicalItemEditorMode: Equatable, Sendable {
    case create(itemID: UUID)
    case createPrepared(itemID: UUID, draft: DayWeaveCanonicalItemDraft)
    case createFromSuggestion(
        suggestionID: UUID,
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    )
    case replace(itemID: UUID, draft: DayWeaveCanonicalItemDraft)
    case updatePending(
        mutationID: UUID,
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    )

    var itemID: UUID {
        switch self {
        case let .create(itemID),
             let .createPrepared(itemID, _),
             let .createFromSuggestion(_, itemID, _),
             let .replace(itemID, _),
             let .updatePending(_, itemID, _):
            itemID
        }
    }

    var initialDraft: DayWeaveCanonicalItemDraft? {
        switch self {
        case .create: nil
        case let .createPrepared(_, draft),
             let .createFromSuggestion(_, _, draft),
             let .replace(_, draft),
             let .updatePending(_, _, draft): draft
        }
    }

    var title: String {
        switch self {
        case .create: "New item"
        case .createPrepared: "Your first planned item"
        case .createFromSuggestion: "Review Codex item draft"
        case .replace: "Edit item"
        case .updatePending: "Edit queued item"
        }
    }

    var actionTitle: String {
        switch self {
        case .create: "Add to Inbox"
        case .createPrepared: "Save planned item"
        case .createFromSuggestion: "Create item"
        case .replace: "Queue changes"
        case .updatePending: "Update queued change"
        }
    }

    var subtitle: String {
        switch self {
        case .createFromSuggestion:
            "Review every field. Codex cannot create this item until you approve it here."
        case .createPrepared:
            "Review every field. This is encrypted locally before it can join your first plan."
        case .create, .replace, .updatePending:
            "Saved locally first. Sync applies the exact queued change later."
        }
    }

    var allowsUnchangedDraft: Bool {
        switch self {
        case .create, .createPrepared, .createFromSuggestion: true
        case .replace, .updatePending: false
        }
    }

    /// A Codex draft is durably private while review is open. Letting the user
    /// edit the eventual sensitivity flag must not expose the process-local
    /// title or notes before the atomic approval transition finishes.
    var preservesSensitivePresentation: Bool {
        if case .createFromSuggestion = self { return true }
        return false
    }
}

struct CanonicalItemEditorView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var store: PlannerStore
    @FocusState private var titleIsFocused: Bool

    let mode: CanonicalItemEditorMode
    let externalReadOnlyDiagnostic: String?
    let onSave: () -> Void
    @State private var state: CanonicalItemEditorState
    @State private var saveError: String?

    init(
        mode: CanonicalItemEditorMode,
        readOnlyDiagnostic: String? = nil,
        profileTimezoneName: String,
        now: Date = Date(),
        onSave: @escaping () -> Void = {}
    ) {
        self.mode = mode
        externalReadOnlyDiagnostic = readOnlyDiagnostic
        self.onSave = onSave
        _state = State(initialValue: CanonicalItemEditorState(
            itemID: mode.itemID,
            draft: mode.initialDraft,
            now: now,
            timezoneName: profileTimezoneName
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
                    if state.kind != .event || state.hasEventFlexibleMetadata {
                        constraintsSection
                    }
                    if state.kind != .event { kindMetadataSection }
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
            switch mode {
            case .create, .createPrepared:
                titleIsFocused = true
            case .createFromSuggestion, .replace, .updatePending:
                break
            }
        }
        .onChange(of: state.kind) { _, _ in
            state.normalizeForKindChange()
        }
        .onChange(of: saveError) { _, error in
            guard let error else { return }
            dayWeavePostAccessibilityAnnouncement(
                "The item could not be saved. \(error)",
                priority: .high
            )
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
                Text(mode.subtitle)
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
                    Text("Planned — prepare for scheduling").tag(DayWeaveCanonicalItemStatus.planned)
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

            if state.kind != .event {
                Toggle("Earliest start", isOn: $state.hasEarliestStart)
                if state.hasEarliestStart {
                    DatePicker(
                        "May start after",
                        selection: $state.earliestStart,
                        displayedComponents: [.date, .hourAndMinute]
                    )
                    .environment(\.timeZone, editorTimeZone)
                    .accessibilityIdentifier("canonical-editor.earliest")
                    strengthEditor(
                        strength: $state.earliestStartStrength,
                        softWeight: $state.earliestStartSoftWeight
                    )
                }

                Toggle("Deadline", isOn: $state.hasDeadline)
                if state.hasDeadline {
                    DatePicker(
                        "Finish by",
                        selection: $state.deadline,
                        displayedComponents: [.date, .hourAndMinute]
                    )
                    .environment(\.timeZone, editorTimeZone)
                    .accessibilityIdentifier("canonical-editor.deadline")
                    strengthEditor(
                        strength: $state.deadlineStrength,
                        softWeight: $state.deadlineSoftWeight
                    )
                }
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
            if let metadata = state.eventFlexibleMetadataPresentation {
                VStack(alignment: .leading, spacing: 8) {
                    Label("Flexible metadata is retained", systemImage: "archivebox")
                        .font(.subheadline.weight(.semibold))
                    Text(metadata.summary)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    DisclosureGroup("Retained metadata details") {
                        Text(metadata.details)
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 4)
                    }
                    Button(role: .destructive) {
                        state.clearEventFlexibleMetadata()
                    } label: {
                        Label("Clear flexible metadata", systemImage: "trash")
                    }
                    .accessibilityIdentifier("canonical-editor.event.clear-flexible-metadata")
                    Text(
                        "Owned timing must be this event's only scheduling metadata. "
                            + "Clear these details explicitly before adding fixed bounds."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("canonical-editor.event.retained-flexible-metadata")
            }
            Toggle("Set an exact event time", isOn: eventTimingEnabled)
                .disabled(state.hasEventFlexibleMetadata)
                .accessibilityIdentifier("canonical-editor.event.timing-enabled")
            if state.hasEventTiming {
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
                Toggle("Publish as busy to connected calendars", isOn: $state.eventIsBusy)
                    .accessibilityIdentifier("canonical-editor.event.busy")
                Text("DayWeave always reserves this owned interval; Busy only controls calendar publication.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Text("Inbox events can stay incomplete. Planned events require exact timing.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Text("Calendar-linked events are read-only here and must be changed in their source calendar.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var recurrenceSection: some View {
        CanonicalEditorSection(title: "Recurrence", symbol: "repeat") {
            if state.recurrence == .custom {
                LabeledContent("Repeats", value: "Custom RRULE (read-only)")
            } else {
                Picker("Repeats", selection: $state.recurrence) {
                    ForEach(CanonicalItemEditorRecurrence.authorableCases) { option in
                        Text(option.title).tag(option)
                    }
                }
                .accessibilityIdentifier("canonical-editor.recurrence")
            }

            switch state.recurrence {
            case .none:
                Text(state.kind == .habit
                    ? (state.readyStatus == .planned
                        ? "Planned habits need a repeat rule before they can be saved."
                        : "Inbox habits can stay without a repeat rule until you are ready to plan them.")
                    : "This item is scheduled once.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            case .daily:
                Stepper(value: recurrenceCount, in: 1...Int(UInt16.max)) {
                    Text("\(state.recurrenceCount) time\(state.recurrenceCount == 1 ? "" : "s") per day")
                }
            case .weekly:
                Stepper(value: recurrenceCount, in: 1...Int(UInt16.max)) {
                    Text("\(state.recurrenceCount) time\(state.recurrenceCount == 1 ? "" : "s") per week")
                }
                weekdayPicker
            case .monthly:
                Stepper(value: recurrenceCount, in: 1...Int(UInt16.max)) {
                    Text("\(state.recurrenceCount) time\(state.recurrenceCount == 1 ? "" : "s") per month")
                }
            case .everyInterval, .afterCompletion:
                Stepper(
                    value: intervalMinutes,
                    in: 1...Int(CanonicalItemEditorState.maximumSchedulingOffsetMinutes),
                    step: 15
                ) {
                    LabeledContent(
                        "Interval",
                        value: CanonicalItemEditorState.minuteDescription(
                            state.recurrenceIntervalMinutes
                        )
                    )
                }
                Text(state.recurrence == .afterCompletion
                    ? "The next occurrence is anchored to completion."
                    : "Occurrences use a rolling interval in minutes.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            case .frequency:
                Stepper(value: recurrenceCount, in: 1...Int(UInt16.max)) {
                    LabeledContent("Target", value: String(state.recurrenceCount))
                }
                HStack {
                    Picker("Period", selection: $state.recurrencePeriod) {
                        ForEach(CanonicalItemEditorRecurrencePeriod.allCases) { period in
                            Text(period.title).tag(period)
                        }
                    }
                    Picker("Semantics", selection: $state.recurrenceSemantics) {
                        ForEach(CanonicalItemEditorRecurrenceSemantics.allCases) { semantics in
                            Text(semantics.title).tag(semantics)
                        }
                    }
                    .onChange(of: state.recurrenceSemantics) { _, semantics in
                        if semantics == .calendar {
                            state.hasRecurrenceAnchor = false
                        } else {
                            state.weekdays = []
                        }
                    }
                }
                if state.recurrenceSemantics == .calendar { weekdayPicker }
                Stepper(
                    value: frequencySpacingMinutes,
                    in: 0...Int(CanonicalItemEditorState.maximumSchedulingOffsetMinutes),
                    step: 15
                ) {
                    LabeledContent(
                        "Minimum spacing",
                        value: state.recurrenceMinimumSpacingMinutes == 0
                            ? "No minimum"
                            : CanonicalItemEditorState.minuteDescription(
                                state.recurrenceMinimumSpacingMinutes
                            )
                    )
                }
                if state.recurrenceSemantics == .rolling {
                    Toggle("Use a rolling anchor", isOn: $state.hasRecurrenceAnchor)
                    if state.hasRecurrenceAnchor {
                        DatePicker(
                            "Anchor",
                            selection: $state.recurrenceAnchor,
                            displayedComponents: [.date, .hourAndMinute]
                        )
                        .environment(\.timeZone, editorTimeZone)
                    }
                }
            case .custom:
                Text(state.customRecurrenceRule)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("canonical-editor.recurrence.rrule")
                Text("This rule is preserved exactly but cannot be edited until DayWeave can expand it.")
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
            if state.kind == .event {
                Text(
                    "These retained Inbox details remain part of the event until you clear "
                        + "them explicitly in Event time."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Picker("Energy", selection: $state.energy) {
                ForEach(CanonicalItemEditorEnergy.allCases) { energy in
                    Text(energy.title).tag(energy)
                }
            }
            .accessibilityIdentifier("canonical-editor.energy")
            if state.energy != .unspecified {
                strengthEditor(
                    strength: $state.energyStrength,
                    softWeight: $state.energySoftWeight
                )
            }

            editorListHeader("Tags") {
                state.tags.append(.init())
            }
            .accessibilityIdentifier("canonical-editor.tags.add")
            ForEach($state.tags) { $tag in
                HStack {
                    TextField("Tag", text: $tag.value)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier("canonical-editor.tags.value")
                    Button(role: .destructive) {
                        state.tags.removeAll { $0.id == tag.id }
                    } label: {
                        Image(systemName: "minus.circle")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Remove tag")
                }
            }

            DisclosureGroup("Timing and capability rules") {
                VStack(alignment: .leading, spacing: 12) {
                    Toggle(
                        "Prefer a start time",
                        isOn: $state.hasPreferredStartMinute
                    )
                    if state.hasPreferredStartMinute {
                        Stepper(
                            value: preferredStartMinute,
                            in: 0...1_439,
                            step: 15
                        ) {
                            LabeledContent(
                                "Preferred start",
                                value: minuteOfDayLabel(state.preferredStartMinute)
                            )
                        }
                        .accessibilityIdentifier("canonical-editor.preferred-start-minute")
                        Text("The complete item must still fit within that local day.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    Toggle("Minimum notice", isOn: $state.hasMinimumNotice)
                    if state.hasMinimumNotice {
                        Stepper(
                            value: minimumNoticeMinutes,
                            in: 0...Int(CanonicalItemEditorState.maximumSchedulingOffsetMinutes)
                        ) {
                            LabeledContent(
                                "Notice",
                                value: CanonicalItemEditorState.minuteDescription(
                                    state.minimumNoticeMinutes
                                )
                            )
                        }
                        strengthEditor(
                            strength: $state.minimumNoticeStrength,
                            softWeight: $state.minimumNoticeSoftWeight
                        )
                    }

                    Toggle("Restrict weekdays", isOn: $state.hasAllowedWeekdays)
                    if state.hasAllowedWeekdays {
                        weekdaySetPicker(selection: $state.allowedWeekdays)
                        strengthEditor(
                            strength: $state.allowedWeekdaysStrength,
                            softWeight: $state.allowedWeekdaysSoftWeight
                        )
                    }

                    editorListHeader("Preferred daily windows") {
                        state.preferredDailyWindows.append(.init())
                    }
                    ForEach($state.preferredDailyWindows) { $window in
                        dailyWindowEditor(window: $window) {
                            state.preferredDailyWindows.removeAll { $0.id == window.id }
                        }
                    }

                    editorListHeader("Preferred absolute windows") {
                        state.preferredAbsoluteWindows.append(.init(
                            start: Date(),
                            end: Date().addingTimeInterval(3_600)
                        ))
                    }
                    ForEach($state.preferredAbsoluteWindows) { $window in
                        absoluteWindowEditor(window: $window) {
                            state.preferredAbsoluteWindows.removeAll { $0.id == window.id }
                        }
                    }

                    editorListHeader("Forbidden windows") {
                        state.forbiddenWindows.append(.init(
                            start: Date(),
                            end: Date().addingTimeInterval(3_600),
                            strength: .hard
                        ))
                    }
                    ForEach($state.forbiddenWindows) { $window in
                        absoluteWindowEditor(window: $window) {
                            state.forbiddenWindows.removeAll { $0.id == window.id }
                        }
                    }

                    editorListHeader("Required contexts") {
                        state.requiredContexts.append(.init())
                    }
                    ForEach($state.requiredContexts) { $context in
                        HStack {
                            TextField("Context", text: $context.value)
                                .textFieldStyle(.roundedBorder)
                            Button(role: .destructive) {
                                state.requiredContexts.removeAll { $0.id == context.id }
                            } label: {
                                Image(systemName: "minus.circle")
                            }
                            .buttonStyle(.plain)
                        }
                        strengthEditor(
                            strength: $context.strength,
                            softWeight: $context.softWeight
                        )
                    }

                    Toggle("Require a location", isOn: $state.hasRequiredLocation)
                    if state.hasRequiredLocation {
                        TextField("Location", text: $state.requiredLocation)
                            .textFieldStyle(.roundedBorder)
                        strengthEditor(
                            strength: $state.requiredLocationStrength,
                            softWeight: $state.requiredLocationSoftWeight
                        )
                    }

                    Toggle("Configure buffers", isOn: $state.hasBuffers)
                    if state.hasBuffers {
                        HStack {
                            Stepper(
                                "Before: \(state.bufferBeforeMinutes)m",
                                value: bufferBeforeMinutes,
                                in: 0...Int(CanonicalItemEditorState.maximumSchedulingOffsetMinutes),
                                step: 5
                            )
                            Stepper(
                                "After: \(state.bufferAfterMinutes)m",
                                value: bufferAfterMinutes,
                                in: 0...Int(CanonicalItemEditorState.maximumSchedulingOffsetMinutes),
                                step: 5
                            )
                        }
                        Toggle("Enforce buffer policy", isOn: $state.bufferHasStrength)
                        if state.bufferHasStrength {
                            strengthEditor(
                                strength: $state.bufferStrength,
                                softWeight: $state.bufferSoftWeight
                            )
                        } else {
                            Text("Inactive server policy retained; these values do not reserve buffer time.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }

                    qualifiedMinuteCap(
                        title: "Daily work cap",
                        enabled: $state.hasMaximumDailyWork,
                        minutes: maximumDailyWorkMinutes,
                        strength: $state.maximumDailyWorkStrength,
                        softWeight: $state.maximumDailyWorkSoftWeight
                    )
                    qualifiedMinuteCap(
                        title: "Weekly work cap",
                        enabled: $state.hasMaximumWeeklyWork,
                        minutes: maximumWeeklyWorkMinutes,
                        strength: $state.maximumWeeklyWorkStrength,
                        softWeight: $state.maximumWeeklyWorkSoftWeight
                    )
                }
                .padding(.top, 8)
            }

            if state.kind != .event {
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
                    Toggle("Limit session count", isOn: $state.hasMaximumSessions)
                    if state.hasMaximumSessions {
                        Stepper(value: maximumSessions, in: 1...Int(UInt16.max)) {
                            LabeledContent(
                                "Maximum sessions",
                                value: String(state.maximumSessions)
                            )
                        }
                    }
                    Stepper(
                        value: minimumGapMinutes,
                        in: 0...Int(CanonicalItemEditorState.maximumSchedulingOffsetMinutes),
                        step: 5
                    ) {
                        LabeledContent("Minimum gap", value: "\(state.minimumGapMinutes)m")
                    }
                    Toggle("Limit split days", isOn: $state.hasMaximumSplitDays)
                    if state.hasMaximumSplitDays {
                        Stepper(value: maximumSplitDays, in: 1...Int(UInt16.max)) {
                            LabeledContent("Maximum days", value: String(state.maximumSplitDays))
                        }
                    }
                }
            }
        }
    }

    private var kindMetadataSection: some View {
        CanonicalEditorSection(title: "Type details", symbol: "list.bullet.clipboard") {
            switch state.kind {
            case .habit:
                Toggle("Track a quantity target", isOn: $state.hasHabitTarget)
                if state.hasHabitTarget {
                    Stepper(value: habitTargetAmount, in: 1...Int(UInt32.max)) {
                        LabeledContent("Target", value: String(state.habitTargetAmount))
                    }
                    TextField("Target unit", text: $state.habitTargetUnit)
                        .textFieldStyle(.roundedBorder)
                }
                Toggle("Preserve streak while paused", isOn: $state.preservesStreakWhenPaused)
            case .routine:
                Toggle("Run subtasks in sibling order", isOn: $state.routineOrdered)
                Text("Ordered routines complete each child before the next sibling starts.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            case .goal:
                editorListHeader("Measures") {
                    state.goalMeasures.append(.init())
                }
                ForEach($state.goalMeasures) { $measure in
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            TextField("Measure name", text: $measure.name)
                            TextField("Unit", text: $measure.unit)
                            Button(role: .destructive) {
                                state.goalMeasures.removeAll { $0.id == measure.id }
                            } label: { Image(systemName: "minus.circle") }
                                .buttonStyle(.plain)
                        }
                        HStack {
                            TextField("Current", value: $measure.current, format: .number)
                            TextField("Target", value: $measure.target, format: .number)
                        }
                    }
                }
                Toggle("Set weekly allocation", isOn: $state.hasGoalWeeklyAllocation)
                if state.hasGoalWeeklyAllocation {
                    Stepper(value: goalWeeklyMinimumMinutes, in: 0...Int(UInt32.max), step: 15) {
                        LabeledContent("Minimum", value: "\(state.goalWeeklyMinimumMinutes)m")
                    }
                    Toggle("Set a maximum", isOn: $state.hasGoalWeeklyMaximum)
                    if state.hasGoalWeeklyMaximum {
                        Stepper(value: goalWeeklyMaximumMinutes, in: 0...Int(UInt32.max), step: 15) {
                            LabeledContent("Maximum", value: "\(state.goalWeeklyMaximumMinutes)m")
                        }
                    }
                }
            case .breakTime:
                Picker("Category", selection: $state.breakCategory) {
                    ForEach(CanonicalItemEditorBreakCategory.allCases) { category in
                        Text(category.title).tag(category)
                    }
                }
                Toggle("Mandatory break", isOn: $state.breakMandatory)
                Toggle("Prompt me to resume", isOn: $state.breakPromptToResume)
            case .task, .project, .event, .unknown:
                Text("No additional type-specific settings.")
                    .foregroundStyle(.secondary)
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
        if state.isSensitive || mode.preservesSensitivePresentation { return true }
        if let parentID = state.parentID,
           store.canonicalItemRequiresSensitivePresentation(itemID: parentID) {
            return true
        }
        switch mode {
        case .create, .createPrepared:
            return false
        case .createFromSuggestion:
            return true
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
        return mode.allowsUnchangedDraft
            || mode.initialDraft?.normalized != state.draft
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
            case let .createPrepared(itemID, _):
                try store.enqueueOnboardingFirstItemCreate(
                    itemID: itemID,
                    draft: state.draft
                )
            case let .createFromSuggestion(suggestionID, itemID, _):
                try store.acceptCanonicalItemSuggestion(
                    suggestionID,
                    itemID: itemID,
                    draft: state.draft
                )
            case let .replace(itemID, _):
                try store.enqueueCanonicalReplace(itemID: itemID, draft: state.draft)
            case let .updatePending(mutationID, _, _):
                try store.updateCanonicalAuthoringDraft(mutationID, draft: state.draft)
            }
            store.selectCanonicalItem(mode.itemID)
            onSave()
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
        .accessibilityElement(children: .combine)
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

    @ViewBuilder
    private func strengthEditor(
        strength: Binding<CanonicalItemEditorConstraintStrength>,
        softWeight: Binding<UInt32>
    ) -> some View {
        HStack {
            Picker("Strength", selection: strength) {
                ForEach(CanonicalItemEditorConstraintStrength.allCases) { value in
                    Text(value.title).tag(value)
                }
            }
            .pickerStyle(.segmented)
            if strength.wrappedValue == .soft {
                Stepper(
                    "Weight \(softWeight.wrappedValue)",
                    value: intBinding(softWeight),
                    in: 0...Int(CanonicalItemEditorState.maximumSoftWeight)
                )
            }
        }
    }

    @ViewBuilder
    private func editorListHeader(_ title: String, add: @escaping () -> Void) -> some View {
        HStack {
            Text(title).font(.subheadline.weight(.semibold))
            Spacer()
            Button(action: add) {
                Label("Add", systemImage: "plus.circle")
            }
            .buttonStyle(.plain)
        }
    }

    @ViewBuilder
    private func weekdaySetPicker(
        selection: Binding<Set<CanonicalItemEditorWeekday>>
    ) -> some View {
        HStack(spacing: 6) {
            ForEach(CanonicalItemEditorWeekday.allCases) { weekday in
                Button(weekday.shortTitle) {
                    if selection.wrappedValue.contains(weekday) {
                        selection.wrappedValue.remove(weekday)
                    } else {
                        selection.wrappedValue.insert(weekday)
                    }
                }
                .buttonStyle(.bordered)
                .tint(selection.wrappedValue.contains(weekday) ? .accentColor : .secondary)
                .help(weekday.title)
            }
        }
    }

    @ViewBuilder
    private func dailyWindowEditor(
        window: Binding<CanonicalItemEditorDailyWindow>,
        remove: @escaping () -> Void
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Stepper(
                    "Start \(minuteOfDayLabel(window.wrappedValue.startMinute))",
                    value: uint16Binding(window.startMinute),
                    in: 0...1_439,
                    step: 15
                )
                Stepper(
                    "End \(minuteOfDayLabel(window.wrappedValue.endMinute))",
                    value: uint16Binding(window.endMinute),
                    in: 0...1_440,
                    step: 15
                )
                Button(role: .destructive, action: remove) {
                    Image(systemName: "minus.circle")
                }
                .buttonStyle(.plain)
            }
            weekdaySetPicker(selection: window.weekdays)
            Text("No selected weekdays means every day.")
                .font(.caption)
                .foregroundStyle(.secondary)
            strengthEditor(strength: window.strength, softWeight: window.softWeight)
        }
        .padding(10)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private func absoluteWindowEditor(
        window: Binding<CanonicalItemEditorAbsoluteWindow>,
        remove: @escaping () -> Void
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                DatePicker(
                    "Starts",
                    selection: window.start,
                    displayedComponents: [.date, .hourAndMinute]
                )
                DatePicker(
                    "Ends",
                    selection: window.end,
                    displayedComponents: [.date, .hourAndMinute]
                )
                Button(role: .destructive, action: remove) {
                    Image(systemName: "minus.circle")
                }
                .buttonStyle(.plain)
            }
            .environment(\.timeZone, editorTimeZone)
            strengthEditor(strength: window.strength, softWeight: window.softWeight)
        }
        .padding(10)
        .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 8))
    }

    @ViewBuilder
    private func qualifiedMinuteCap(
        title: String,
        enabled: Binding<Bool>,
        minutes: Binding<Int>,
        strength: Binding<CanonicalItemEditorConstraintStrength>,
        softWeight: Binding<UInt32>
    ) -> some View {
        Toggle(title, isOn: enabled)
        if enabled.wrappedValue {
            Stepper(value: minutes, in: 0...Int(UInt32.max), step: 15) {
                LabeledContent(title, value: "\(minutes.wrappedValue)m")
            }
            strengthEditor(strength: strength, softWeight: softWeight)
        }
    }

    private func intBinding(_ value: Binding<UInt32>) -> Binding<Int> {
        Binding(
            get: { Int(value.wrappedValue) },
            set: { value.wrappedValue = UInt32(max(0, min(Int(UInt32.max), $0))) }
        )
    }

    private func uint16Binding(_ value: Binding<UInt16>) -> Binding<Int> {
        Binding(
            get: { Int(value.wrappedValue) },
            set: { value.wrappedValue = UInt16(max(0, min(Int(UInt16.max), $0))) }
        )
    }

    private func minuteOfDayLabel(_ minute: UInt16) -> String {
        if minute == 1_440 { return "24:00" }
        return String(format: "%02d:%02d", minute / 60, minute % 60)
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
            set: {
                state.recurrenceCount = UInt32(max(1, min(Int(UInt16.max), $0)))
            }
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

    private var frequencySpacingMinutes: Binding<Int> {
        intBinding($state.recurrenceMinimumSpacingMinutes)
    }

    private var minimumNoticeMinutes: Binding<Int> {
        intBinding($state.minimumNoticeMinutes)
    }

    private var preferredStartMinute: Binding<Int> {
        Binding(
            get: { Int(state.preferredStartMinute) },
            set: { state.preferredStartMinute = UInt16(max(0, min(1_439, $0))) }
        )
    }

    private var bufferBeforeMinutes: Binding<Int> {
        intBinding($state.bufferBeforeMinutes)
    }

    private var bufferAfterMinutes: Binding<Int> {
        intBinding($state.bufferAfterMinutes)
    }

    private var maximumDailyWorkMinutes: Binding<Int> {
        intBinding($state.maximumDailyWorkMinutes)
    }

    private var maximumWeeklyWorkMinutes: Binding<Int> {
        intBinding($state.maximumWeeklyWorkMinutes)
    }

    private var minimumGapMinutes: Binding<Int> {
        intBinding($state.minimumGapMinutes)
    }

    private var goalWeeklyMinimumMinutes: Binding<Int> {
        intBinding($state.goalWeeklyMinimumMinutes)
    }

    private var goalWeeklyMaximumMinutes: Binding<Int> {
        intBinding($state.goalWeeklyMaximumMinutes)
    }

    private var habitTargetAmount: Binding<Int> {
        intBinding($state.habitTargetAmount)
    }

    private var maximumSessions: Binding<Int> {
        Binding(
            get: { Int(state.maximumSessions) },
            set: { state.maximumSessions = UInt16(max(1, min(Int(UInt16.max), $0))) }
        )
    }

    private var maximumSplitDays: Binding<Int> {
        Binding(
            get: { Int(state.maximumSplitDays) },
            set: { state.maximumSplitDays = UInt16(max(1, min(Int(UInt16.max), $0))) }
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

    private var eventTimingEnabled: Binding<Bool> {
        Binding(
            get: { state.hasEventTiming },
            set: { state.setEventTimingEnabled($0) }
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
        PlannerTimeZone.resolve(state.timezoneName)
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
