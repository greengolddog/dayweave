import Foundation

enum CanonicalItemEditorRecurrence: String, CaseIterable, Identifiable, Sendable {
    case none
    case daily
    case weekly
    case monthly
    case everyInterval
    case afterCompletion
    case frequency
    case custom

    var id: Self { self }

    static let authorableCases = allCases.filter { $0 != .custom }

    var title: String {
        switch self {
        case .none: "Does not repeat"
        case .daily: "Daily"
        case .weekly: "Weekly"
        case .monthly: "Monthly"
        case .everyInterval: "Every interval"
        case .afterCompletion: "After completion"
        case .frequency: "Flexible frequency"
        case .custom: "Custom RRULE"
        }
    }
}

enum CanonicalItemEditorConstraintStrength: String, CaseIterable, Identifiable, Sendable {
    case hard
    case soft

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

enum CanonicalItemEditorRecurrencePeriod: String, CaseIterable, Identifiable, Sendable {
    case day
    case week
    case month

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

enum CanonicalItemEditorRecurrenceSemantics: String, CaseIterable, Identifiable, Sendable {
    case calendar
    case rolling

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

enum CanonicalItemEditorBreakCategory: String, CaseIterable, Identifiable, Sendable {
    case rest
    case meal
    case movement
    case pomodoro
    case decompression
    case other

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

struct CanonicalItemEditorDailyWindow: Identifiable, Equatable, Sendable {
    let id: UUID
    var weekdays: Set<CanonicalItemEditorWeekday>
    var startMinute: UInt16
    var endMinute: UInt16
    var strength: CanonicalItemEditorConstraintStrength
    var softWeight: UInt32

    init(
        id: UUID = UUID(),
        weekdays: Set<CanonicalItemEditorWeekday> = [],
        startMinute: UInt16 = 9 * 60,
        endMinute: UInt16 = 17 * 60,
        strength: CanonicalItemEditorConstraintStrength = .soft,
        softWeight: UInt32 = 100
    ) {
        self.id = id
        self.weekdays = weekdays
        self.startMinute = startMinute
        self.endMinute = endMinute
        self.strength = strength
        self.softWeight = softWeight
    }
}

struct CanonicalItemEditorAbsoluteWindow: Identifiable, Equatable, Sendable {
    let id: UUID
    var start: Date
    var end: Date
    var strength: CanonicalItemEditorConstraintStrength
    var softWeight: UInt32

    init(
        id: UUID = UUID(),
        start: Date,
        end: Date,
        strength: CanonicalItemEditorConstraintStrength = .soft,
        softWeight: UInt32 = 100
    ) {
        self.id = id
        self.start = start
        self.end = end
        self.strength = strength
        self.softWeight = softWeight
    }
}

struct CanonicalItemEditorQualifiedText: Identifiable, Equatable, Sendable {
    let id: UUID
    var value: String
    var strength: CanonicalItemEditorConstraintStrength
    var softWeight: UInt32

    init(
        id: UUID = UUID(),
        value: String = "",
        strength: CanonicalItemEditorConstraintStrength = .hard,
        softWeight: UInt32 = 100
    ) {
        self.id = id
        self.value = value
        self.strength = strength
        self.softWeight = softWeight
    }
}

struct CanonicalItemEditorTag: Identifiable, Equatable, Sendable {
    let id: UUID
    var value: String

    init(id: UUID = UUID(), value: String = "") {
        self.id = id
        self.value = value
    }
}

struct CanonicalItemEditorGoalMeasure: Identifiable, Equatable, Sendable {
    let id: UUID
    var name: String
    var target: Int64
    var current: Int64
    var unit: String

    init(
        id: UUID = UUID(),
        name: String = "",
        target: Int64 = 1,
        current: Int64 = 0,
        unit: String = "times"
    ) {
        self.id = id
        self.name = name
        self.target = target
        self.current = current
        self.unit = unit
    }
}

enum CanonicalItemEditorEnergy: String, CaseIterable, Identifiable, Sendable {
    case unspecified
    case low
    case medium
    case deep

    var id: Self { self }
    var title: String { self == .unspecified ? "No preference" : rawValue.capitalized }
}

enum CanonicalItemEditorWeekday: String, CaseIterable, Identifiable, Sendable {
    case monday
    case tuesday
    case wednesday
    case thursday
    case friday
    case saturday
    case sunday

    var id: Self { self }
    var shortTitle: String { String(rawValue.prefix(2)).capitalized }
    var title: String { rawValue.capitalized }
}

struct CanonicalItemEditorParentOption: Identifiable, Equatable, Sendable {
    let id: UUID
    let title: String
    let depth: Int
    let breadcrumb: [String]
    let isSensitive: Bool
}

struct CanonicalItemEditorEventMetadataPresentation: Equatable, Sendable {
    let summary: String
    let details: String
}

struct CanonicalItemEditorState: Equatable, Sendable {
    static let maximumRecurrenceCount: UInt32 = UInt32(UInt16.max)
    static let maximumSchedulingOffsetMinutes =
        DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes
    static let maximumIntervalMinutes = maximumSchedulingOffsetMinutes
    static let maximumSoftWeight: UInt32 = 1_000_000
    static let maximumParentBreadcrumbDepth = 8

    private struct OriginalEventState: Equatable, Sendable {
        enum MetadataKind: Equatable, Sendable {
            case calendarEvent
            case dayWeaveFirmBlock
        }

        let start: Date
        let end: Date
        let startWireValue: String
        let endWireValue: String
        let durationSeconds: UInt32?
        let canonicalEarliestStart: Date?
        let canonicalDeadline: Date?
        let metadataKind: MetadataKind
        let hadSourceCalendarIDField: Bool
    }

    private struct AllDayLocalDateSpan: Equatable, Sendable {
        let year: Int
        let month: Int
        let day: Int
        let dayCount: Int
    }

    let itemID: UUID
    var title: String
    var notes: String
    var kind: DayWeaveCanonicalItemKind
    var isSensitive: Bool
    var readyStatus: DayWeaveCanonicalItemStatus
    var timezoneName: String

    var hasDuration: Bool
    var durationSeconds: UInt32
    var hasEarliestStart: Bool
    var earliestStart: Date
    var earliestStartStrength: CanonicalItemEditorConstraintStrength
    var earliestStartSoftWeight: UInt32
    var hasDeadline: Bool
    var deadline: Date
    var deadlineStrength: CanonicalItemEditorConstraintStrength
    var deadlineSoftWeight: UInt32
    var importance: UInt8
    var urgency: UInt8

    var recurrence: CanonicalItemEditorRecurrence
    var recurrenceCount: UInt32
    var recurrenceIntervalMinutes: UInt32
    var weekdays: Set<CanonicalItemEditorWeekday>
    var recurrencePeriod: CanonicalItemEditorRecurrencePeriod
    var recurrenceSemantics: CanonicalItemEditorRecurrenceSemantics
    var recurrenceMinimumSpacingMinutes: UInt32
    var hasRecurrenceAnchor: Bool
    var recurrenceAnchor: Date
    var customRecurrenceRule: String
    var energy: CanonicalItemEditorEnergy
    var energyStrength: CanonicalItemEditorConstraintStrength
    var energySoftWeight: UInt32
    var tags: [CanonicalItemEditorTag]
    var hasPreferredStartMinute: Bool
    var preferredStartMinute: UInt16
    var hasOwnEffort: Bool

    var hasMinimumNotice: Bool
    var minimumNoticeMinutes: UInt32
    var minimumNoticeStrength: CanonicalItemEditorConstraintStrength
    var minimumNoticeSoftWeight: UInt32
    var hasAllowedWeekdays: Bool
    var allowedWeekdays: Set<CanonicalItemEditorWeekday>
    var allowedWeekdaysStrength: CanonicalItemEditorConstraintStrength
    var allowedWeekdaysSoftWeight: UInt32
    var preferredDailyWindows: [CanonicalItemEditorDailyWindow]
    var preferredAbsoluteWindows: [CanonicalItemEditorAbsoluteWindow]
    var forbiddenWindows: [CanonicalItemEditorAbsoluteWindow]
    var requiredContexts: [CanonicalItemEditorQualifiedText]
    var hasRequiredLocation: Bool
    var requiredLocation: String
    var requiredLocationStrength: CanonicalItemEditorConstraintStrength
    var requiredLocationSoftWeight: UInt32
    var hasBuffers: Bool
    var bufferBeforeMinutes: UInt32
    var bufferAfterMinutes: UInt32
    var bufferHasStrength: Bool
    var bufferStrength: CanonicalItemEditorConstraintStrength
    var bufferSoftWeight: UInt32
    var hasMaximumDailyWork: Bool
    var maximumDailyWorkMinutes: UInt32
    var maximumDailyWorkStrength: CanonicalItemEditorConstraintStrength
    var maximumDailyWorkSoftWeight: UInt32
    var hasMaximumWeeklyWork: Bool
    var maximumWeeklyWorkMinutes: UInt32
    var maximumWeeklyWorkStrength: CanonicalItemEditorConstraintStrength
    var maximumWeeklyWorkSoftWeight: UInt32

    var hasHabitTarget: Bool
    var habitTargetAmount: UInt32
    var habitTargetUnit: String
    var preservesStreakWhenPaused: Bool
    var routineOrdered: Bool
    var goalMeasures: [CanonicalItemEditorGoalMeasure]
    var hasGoalWeeklyAllocation: Bool
    var goalWeeklyMinimumMinutes: UInt32
    var hasGoalWeeklyMaximum: Bool
    var goalWeeklyMaximumMinutes: UInt32
    var breakCategory: CanonicalItemEditorBreakCategory
    var breakMandatory: Bool
    var breakPromptToResume: Bool

    var eventStart: Date
    var eventEnd: Date
    var hasEventTiming: Bool
    var eventIsImmutable: Bool
    var eventIsAllDay: Bool
    var eventIsTentative: Bool
    var eventIsBusy: Bool

    var isSplittable: Bool
    var minimumChunkSeconds: UInt32
    var maximumChunkSeconds: UInt32
    var hasMaximumSessions: Bool
    var maximumSessions: UInt16
    var minimumGapMinutes: UInt32
    var hasMaximumSplitDays: Bool
    var maximumSplitDays: UInt16
    var parentID: UUID?
    var siblingOrder: UInt32

    private var retainedConstraints: [String: JSONValue]
    private var eventSourceCalendarID: String?
    private var originalEvent: OriginalEventState?
    private var allDayLocalDateSpan: AllDayLocalDateSpan?
    private var hadOwnEffortConstraint: Bool
    private var energyUsesQualifiedEncoding: Bool
    private var hadPreservesStreakConstraint: Bool
    private var hadRoutineOrderedConstraint: Bool
    private var hadBreakCategoryConstraint: Bool
    private var hadBreakMandatoryConstraint: Bool
    private var hadBreakPromptConstraint: Bool
    private var hadExplicitNullOccurrenceWindow: Bool
    private var retainedReadOnlyDraft: DayWeaveCanonicalItemDraft?
    private(set) var readOnlyDiagnostic: String?

    init(
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft? = nil,
        now: Date = Date(),
        timezoneName defaultTimezoneName: String = Self.defaultTimezoneName
    ) {
        let newItemTimezoneName = DayWeaveCanonicalItemDraft.supportedTimeZone(
            identifier: defaultTimezoneName
        ) == nil ? "UTC" : defaultTimezoneName
        let source = draft ?? DayWeaveCanonicalItemDraft(
            title: "",
            timezoneName: newItemTimezoneName
        )
        self.itemID = itemID
        title = source.title
        notes = source.notes ?? ""
        kind = source.kind
        isSensitive = source.isSensitive
        readyStatus = source.status
        timezoneName = source.timezoneName
        hasDuration = source.durationSeconds != nil
        durationSeconds = source.durationSeconds ?? 30 * 60
        hasEarliestStart = source.earliestStartAt != nil
        earliestStart = source.earliestStartAt ?? now
        earliestStartStrength = .hard
        earliestStartSoftWeight = 100
        hasDeadline = source.deadlineAt != nil
        deadline = source.deadlineAt ?? now.addingTimeInterval(24 * 60 * 60)
        deadlineStrength = .hard
        deadlineSoftWeight = 100
        importance = source.importance
        urgency = source.urgency
        recurrence = .none
        recurrenceCount = 1
        recurrenceIntervalMinutes = 24 * 60
        weekdays = []
        recurrencePeriod = .week
        recurrenceSemantics = .calendar
        recurrenceMinimumSpacingMinutes = 0
        hasRecurrenceAnchor = false
        recurrenceAnchor = now
        customRecurrenceRule = "FREQ=WEEKLY"
        energy = .unspecified
        energyStrength = .soft
        energySoftWeight = 100
        tags = []
        hasPreferredStartMinute = false
        preferredStartMinute = 9 * 60
        hasOwnEffort = false
        hasMinimumNotice = false
        minimumNoticeMinutes = 60
        minimumNoticeStrength = .hard
        minimumNoticeSoftWeight = 100
        hasAllowedWeekdays = false
        allowedWeekdays = []
        allowedWeekdaysStrength = .hard
        allowedWeekdaysSoftWeight = 100
        preferredDailyWindows = []
        preferredAbsoluteWindows = []
        forbiddenWindows = []
        requiredContexts = []
        hasRequiredLocation = false
        requiredLocation = ""
        requiredLocationStrength = .hard
        requiredLocationSoftWeight = 100
        hasBuffers = false
        bufferBeforeMinutes = 0
        bufferAfterMinutes = 0
        bufferHasStrength = true
        bufferStrength = .hard
        bufferSoftWeight = 100
        hasMaximumDailyWork = false
        maximumDailyWorkMinutes = 8 * 60
        maximumDailyWorkStrength = .hard
        maximumDailyWorkSoftWeight = 100
        hasMaximumWeeklyWork = false
        maximumWeeklyWorkMinutes = 40 * 60
        maximumWeeklyWorkStrength = .hard
        maximumWeeklyWorkSoftWeight = 100
        hasHabitTarget = false
        habitTargetAmount = 1
        habitTargetUnit = "times"
        preservesStreakWhenPaused = true
        routineOrdered = false
        goalMeasures = []
        hasGoalWeeklyAllocation = false
        goalWeeklyMinimumMinutes = 60
        hasGoalWeeklyMaximum = false
        goalWeeklyMaximumMinutes = 180
        breakCategory = .other
        breakMandatory = false
        breakPromptToResume = true
        eventStart = now
        eventEnd = now.addingTimeInterval(60 * 60)
        hasEventTiming = false
        eventIsImmutable = true
        eventIsAllDay = false
        eventIsTentative = false
        eventIsBusy = true
        isSplittable = false
        minimumChunkSeconds = 15 * 60
        maximumChunkSeconds = source.durationSeconds ?? 30 * 60
        hasMaximumSessions = false
        maximumSessions = 8
        minimumGapMinutes = 0
        hasMaximumSplitDays = false
        maximumSplitDays = 1
        parentID = source.parentID
        siblingOrder = source.siblingOrder
        retainedConstraints = [:]
        eventSourceCalendarID = nil
        originalEvent = nil
        allDayLocalDateSpan = nil
        hadOwnEffortConstraint = false
        energyUsesQualifiedEncoding = false
        hadPreservesStreakConstraint = false
        hadRoutineOrderedConstraint = false
        hadBreakCategoryConstraint = false
        hadBreakMandatoryConstraint = false
        hadBreakPromptConstraint = false
        hadExplicitNullOccurrenceWindow = false
        retainedReadOnlyDraft = nil
        readOnlyDiagnostic = nil

        parseReadyStatus(source.status)
        parseRecurrence(source.recurrence)
        parseConstraints(
            source.flexibleConstraints,
            originalDurationSeconds: draft == nil ? nil : source.durationSeconds,
            originalCanonicalEarliestStart: draft == nil ? nil : source.earliestStartAt,
            originalCanonicalDeadline: draft == nil ? nil : source.deadlineAt
        )
        parseSplitPolicy(source.splitPolicy)
        if eventIsAllDay { refreshAllDayLocalDateSpan() }
        if source.recurrence?.supportsCanonicalAuthoringRecurrence == false {
            markReadOnly("This recurrence form is preserved but cannot be safely edited.")
        }
        if !source.flexibleConstraints.supportsCanonicalAuthoringConstraints {
            markReadOnly("These scheduling constraints are preserved but cannot be safely edited.")
        }
        if case .unknown = source.kind {
            markReadOnly("This item type is not editable by this version of DayWeave.")
        }
        if source.parentID == itemID {
            markReadOnly("This item has an invalid self-referencing parent.")
        }
        if !source.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
           let issue = source.validationIssue(itemID: itemID) {
            markReadOnly(issue)
        }
        if readOnlyDiagnostic != nil { retainedReadOnlyDraft = source }
    }

    static let defaultTimezoneName = "UTC"

    var supportsRecurrence: Bool {
        switch kind {
        case .task, .habit, .routine: true
        default: false
        }
    }

    /// Incomplete Inbox events may validly carry general scheduling metadata,
    /// but a locally owned firm block must be the sole metadata key. Keep the
    /// current metadata visible and make the destructive transition explicit.
    var eventFlexibleMetadataPresentation: CanonicalItemEditorEventMetadataPresentation? {
        guard kind == .event, !genericConstraintsObject.isEmpty else { return nil }
        let keys = genericConstraintsObject.keys.sorted()
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let details = (try? encoder.encode(JSONValue.object(genericConstraintsObject)))
            .flatMap { String(data: $0, encoding: .utf8) }
            ?? "Retained scheduling metadata"
        let fieldLabel = keys.count == 1 ? "field" : "fields"
        return .init(
            summary: "Retained " + fieldLabel + ": " + keys.joined(separator: ", "),
            details: details
        )
    }

    var hasEventFlexibleMetadata: Bool {
        eventFlexibleMetadataPresentation != nil
    }

    var validationIssue: String? {
        if let readOnlyDiagnostic { return readOnlyDiagnostic }
        guard readyStatus == .inbox || readyStatus == .planned else {
            return "Captured items must be either Inbox or Planned."
        }
        if kind == .event, hasEventTiming, hasEventFlexibleMetadata {
            return "Clear the retained flexible metadata before setting exact event timing."
        }
        if kind == .event, hasEventTiming {
            let interval = eventEnd.timeIntervalSince(eventStart)
            guard interval.isFinite, interval >= 1 else {
                return "Event end must be after its start."
            }
        }
        if recurrence != .none {
            guard supportsRecurrence else {
                return "This item type cannot repeat."
            }
            switch recurrence {
            case .daily, .weekly, .monthly:
                guard (1...Self.maximumRecurrenceCount).contains(recurrenceCount) else {
                    return "Repeat count must be between 1 and \(Self.maximumRecurrenceCount)."
                }
            case .everyInterval, .afterCompletion:
                guard (1...Self.maximumIntervalMinutes).contains(recurrenceIntervalMinutes) else {
                    return "Repeat interval must be between 1 and \(Self.maximumIntervalMinutes) minutes."
                }
            case .frequency:
                guard (1...Self.maximumRecurrenceCount).contains(recurrenceCount) else {
                    return "Frequency target must be between 1 and \(Self.maximumRecurrenceCount)."
                }
                if recurrenceSemantics == .rolling {
                    if !weekdays.isEmpty {
                        return "Rolling frequency cannot select calendar weekdays."
                    }
                    if recurrencePeriod == .day, recurrenceCount > 1_440 {
                        return "Rolling daily frequency exceeds minute precision."
                    }
                    if recurrencePeriod == .week, recurrenceCount > 10_080 {
                        return "Rolling weekly frequency exceeds minute precision."
                    }
                    if hasRecurrenceAnchor,
                       recurrenceAnchor.timeIntervalSince1970.isFinite == false {
                        return "Choose a valid rolling-frequency anchor."
                    }
                } else if hasRecurrenceAnchor {
                    return "Calendar frequency cannot define a rolling anchor."
                }
                if recurrenceMinimumSpacingMinutes > Self.maximumSchedulingOffsetMinutes {
                    return "Frequency spacing must be at most \(Self.maximumSchedulingOffsetMinutes) minutes."
                }
            case .custom:
                return "Custom RRULE recurrence is retained read-only until expansion is supported."
            case .none:
                break
            }
        }
        if let issue = schedulingMetadataValidationIssue { return issue }
        return draft.validationIssue(itemID: itemID)
    }

    var draft: DayWeaveCanonicalItemDraft {
        if let retainedReadOnlyDraft { return retainedReadOnlyDraft }
        let emitsEventTiming = kind == .event
            && hasEventTiming
            && !hasEventFlexibleMetadata
        return DayWeaveCanonicalItemDraft(
            isSensitive: isSensitive,
            kind: kind,
            status: readyStatus,
            title: title,
            notes: notes,
            timezoneName: timezoneName,
            durationSeconds: kind == .event
                ? (emitsEventTiming ? eventDurationValue : nil)
                : (hasDuration ? durationSeconds : nil),
            deadlineAt: kind == .event
                ? (emitsEventTiming
                    ? (eventRangeIsUnchanged ? originalEvent?.canonicalDeadline : eventEnd)
                    : nil)
                : (hasDeadline && deadlineStrength == .hard ? deadline : nil),
            earliestStartAt: kind == .event
                ? (emitsEventTiming
                    ? (eventRangeIsUnchanged
                        ? originalEvent?.canonicalEarliestStart
                        : eventStart)
                    : nil)
                : (hasEarliestStart && earliestStartStrength == .hard
                    ? earliestStart
                    : nil),
            recurrence: recurrenceValue,
            flexibleConstraints: constraintsValue,
            splitPolicy: kind == .event || !isSplittable
                ? .indivisible
                : .splittable(
                    minimumChunkSeconds: minimumChunkSeconds,
                    maximumChunkSeconds: maximumChunkSeconds
                ),
            importance: importance,
            urgency: urgency,
            parentID: parentID,
            siblingOrder: siblingOrder
        ).normalized
    }

    mutating func normalizeForKindChange() {
        if !supportsRecurrence { recurrence = .none }
        if kind == .habit, recurrence == .none { recurrence = .daily }
        if kind == .goal || kind == .routine {
            hasOwnEffort = false
            hadOwnEffortConstraint = false
        } else {
            hasOwnEffort = false
            hadOwnEffortConstraint = false
        }
        if kind == .event {
            isSplittable = false
            hasPreferredStartMinute = false
            hasDuration = true
            _ = setEventTimingEnabled(true)
            if eventEnd <= eventStart {
                eventEnd = eventStart.addingTimeInterval(60 * 60)
            }
            if eventIsAllDay { normalizeAllDayEventBounds() }
        }
    }

    /// Returns false without changing state when general event metadata must
    /// first cross the explicit destructive-clear boundary.
    @discardableResult
    mutating func setEventTimingEnabled(_ value: Bool) -> Bool {
        if !value {
            hasEventTiming = false
            return true
        }
        guard kind == .event, !hasEventFlexibleMetadata else { return false }
        hasEventTiming = true
        return true
    }

    mutating func clearEventFlexibleMetadata() {
        guard kind == .event else { return }
        retainedConstraints = [:]
        hasEarliestStart = false
        hasDeadline = false
        energy = .unspecified
        energyStrength = .soft
        energySoftWeight = 100
        energyUsesQualifiedEncoding = false
        tags = []
        hasPreferredStartMinute = false
        hasOwnEffort = false
        hadOwnEffortConstraint = false
        hasMinimumNotice = false
        hasAllowedWeekdays = false
        allowedWeekdays = []
        preferredDailyWindows = []
        preferredAbsoluteWindows = []
        forbiddenWindows = []
        requiredContexts = []
        hasRequiredLocation = false
        requiredLocation = ""
        hasBuffers = false
        bufferBeforeMinutes = 0
        bufferAfterMinutes = 0
        bufferHasStrength = true
        hasMaximumDailyWork = false
        hasMaximumWeeklyWork = false
        hadExplicitNullOccurrenceWindow = false
        isSplittable = false
        hasMaximumSessions = false
        minimumGapMinutes = 0
        hasMaximumSplitDays = false
    }

    mutating func setTimezoneName(_ value: String) {
        if eventIsAllDay, allDayLocalDateSpan == nil {
            refreshAllDayLocalDateSpan()
        }
        timezoneName = value
        guard eventIsAllDay,
              let newCalendar = eventCalendar,
              let localDateSpan = allDayLocalDateSpan else { return }
        if let newStart = newCalendar.date(from: DateComponents(
               calendar: newCalendar,
               timeZone: newCalendar.timeZone,
               year: localDateSpan.year,
               month: localDateSpan.month,
               day: localDateSpan.day
           )) {
            eventStart = newCalendar.startOfDay(for: newStart)
            eventEnd = newCalendar.date(
                byAdding: .day,
                value: localDateSpan.dayCount,
                to: eventStart
            ) ?? eventStart.addingTimeInterval(
                TimeInterval(localDateSpan.dayCount * 86_400)
            )
        } else {
            normalizeAllDayEventBounds()
        }
    }

    mutating func setEventIsAllDay(_ value: Bool) {
        eventIsAllDay = value
        if value { normalizeAllDayEventBounds() }
    }

    mutating func setEventStart(_ value: Date) {
        guard eventIsAllDay, let calendar = eventCalendar else {
            eventStart = value
            return
        }
        let span = allDaySpan(using: calendar)
        eventStart = calendar.startOfDay(for: value)
        eventEnd = calendar.date(byAdding: .day, value: span, to: eventStart)
            ?? eventStart.addingTimeInterval(TimeInterval(span * 86_400))
        refreshAllDayLocalDateSpan(using: calendar)
    }

    mutating func setEventEnd(_ value: Date) {
        guard eventIsAllDay, let calendar = eventCalendar else {
            eventEnd = value
            return
        }
        let normalizedEnd = calendar.startOfDay(for: value)
        eventEnd = normalizedEnd > eventStart
            ? normalizedEnd
            : (calendar.date(byAdding: .day, value: 1, to: eventStart)
                ?? eventStart.addingTimeInterval(86_400))
        refreshAllDayLocalDateSpan(using: calendar)
    }

    static func parentOptions(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingMutations: [DayWeavePendingCanonicalAuthoringMutation],
        excluding itemID: UUID
    ) -> [CanonicalItemEditorParentOption] {
        struct Node {
            let id: UUID
            let title: String
            let parentID: UUID?
            let siblingOrder: UInt32
            let isSensitive: Bool
            let status: DayWeaveCanonicalItemStatus
        }

        var nodes = Dictionary(uniqueKeysWithValues: canonicalItems
            .filter { $0.deletedAt == nil }
            .map { item in
                (item.id, Node(
                    id: item.id,
                    title: item.title,
                    parentID: item.parentID,
                    siblingOrder: item.siblingOrder,
                    isSensitive: item.isSensitive,
                    status: item.status
                ))
            })
        for mutation in pendingMutations {
            if mutation.operation == .trash {
                nodes.removeValue(forKey: mutation.itemID)
            } else if let draft = mutation.draft {
                nodes[mutation.itemID] = Node(
                    id: mutation.itemID,
                    title: draft.title,
                    parentID: draft.parentID,
                    siblingOrder: draft.siblingOrder,
                    isSensitive: draft.isSensitive,
                    status: draft.status
                )
            }
        }

        func nodeOrder(_ leftID: UUID, _ rightID: UUID) -> Bool {
            guard let left = nodes[leftID], let right = nodes[rightID] else {
                return leftID.uuidString < rightID.uuidString
            }
            if left.siblingOrder != right.siblingOrder {
                return left.siblingOrder < right.siblingOrder
            }
            let titleOrder = left.title.localizedStandardCompare(right.title)
            if titleOrder != .orderedSame { return titleOrder == .orderedAscending }
            return left.id.uuidString < right.id.uuidString
        }

        var children: [UUID: [UUID]] = [:]
        var roots: [UUID] = []
        for node in nodes.values {
            if let parentID = node.parentID {
                if nodes[parentID] != nil { children[parentID, default: []].append(node.id) }
            } else {
                roots.append(node.id)
            }
        }
        roots.sort(by: nodeOrder)
        for parentID in children.keys { children[parentID]?.sort(by: nodeOrder) }

        var excluded: Set<UUID> = [itemID]
        var descendantQueue = [itemID]
        var descendantIndex = 0
        while descendantIndex < descendantQueue.count {
            let parentID = descendantQueue[descendantIndex]
            descendantIndex += 1
            for childID in children[parentID] ?? [] where excluded.insert(childID).inserted {
                descendantQueue.append(childID)
            }
        }

        var orderedIDs: [UUID] = []
        var depthByID: [UUID: Int] = [:]
        var breadcrumbByID: [UUID: [String]] = [:]
        var visited = Set<UUID>()
        var stack = roots.reversed().map { ($0, 0, [String]()) }
        while let (nodeID, depth, breadcrumb) = stack.popLast() {
            guard let node = nodes[nodeID], visited.insert(nodeID).inserted else { continue }
            orderedIDs.append(nodeID)
            depthByID[nodeID] = depth
            breadcrumbByID[nodeID] = breadcrumb
            var childBreadcrumb = breadcrumb
            childBreadcrumb.append(node.title)
            if childBreadcrumb.count > Self.maximumParentBreadcrumbDepth {
                childBreadcrumb.removeFirst(
                    childBreadcrumb.count - Self.maximumParentBreadcrumbDepth
                )
            }
            for childID in (children[nodeID] ?? []).reversed() {
                stack.append((childID, depth + 1, childBreadcrumb))
            }
        }

        return orderedIDs.compactMap { nodeID in
            guard !excluded.contains(nodeID),
                  let node = nodes[nodeID],
                  node.status == .inbox || node.status == .planned,
                  let depth = depthByID[nodeID],
                  let breadcrumb = breadcrumbByID[nodeID] else {
                return nil
            }
            return CanonicalItemEditorParentOption(
                id: node.id,
                title: node.title,
                depth: depth,
                breadcrumb: breadcrumb,
                isSensitive: node.isSensitive
            )
        }
    }

    static func durationDescription(_ seconds: UInt32?) -> String {
        guard let seconds else { return "No estimate" }
        if seconds % 3_600 == 0 { return "\(seconds / 3_600)h" }
        if seconds % 60 == 0 { return "\(seconds / 60)m" }
        return "\(seconds)s"
    }

    static func minuteDescription(_ minutes: UInt32) -> String {
        guard minutes <= UInt32.max / 60 else { return "\(minutes)m" }
        return durationDescription(minutes * 60)
    }

    private var recurrenceValue: JSONValue? {
        guard supportsRecurrence else { return nil }
        switch recurrence {
        case .none:
            return nil
        case .daily:
            return .object([
                "type": .string("daily"),
                "times_per_day": .number(JSONNumber(UInt64(recurrenceCount))),
            ])
        case .weekly:
            return .object([
                "type": .string("weekly"),
                "times_per_week": .number(JSONNumber(UInt64(recurrenceCount))),
                "weekdays": .array(CanonicalItemEditorWeekday.allCases.compactMap { weekday in
                    weekdays.contains(weekday) ? .string(weekday.rawValue) : nil
                }),
            ])
        case .monthly:
            return .object([
                "type": .string("monthly"),
                "times_per_month": .number(JSONNumber(UInt64(recurrenceCount))),
            ])
        case .everyInterval:
            return .object([
                "type": .string("every_interval"),
                "interval": .number(JSONNumber(UInt64(recurrenceIntervalMinutes))),
            ])
        case .afterCompletion:
            return .object([
                "type": .string("after_completion"),
                "interval": .number(JSONNumber(UInt64(recurrenceIntervalMinutes))),
            ])
        case .frequency:
            var value: [String: JSONValue] = [
                "type": .string("frequency"),
                "target": .number(JSONNumber(UInt64(recurrenceCount))),
                "period": .string(recurrencePeriod.rawValue),
                "semantics": .string(recurrenceSemantics.rawValue),
            ]
            if !weekdays.isEmpty {
                value["weekdays"] = .array(orderedWeekdays(weekdays))
            }
            if recurrenceMinimumSpacingMinutes > 0 {
                value["minimum_spacing"] = .number(JSONNumber(
                    UInt64(recurrenceMinimumSpacingMinutes)
                ))
            }
            if hasRecurrenceAnchor {
                value["anchor"] = .string(Self.format(recurrenceAnchor))
            }
            return .object(value)
        case .custom:
            return .object([
                "type": .string("custom"),
                "rrule": .string(customRecurrenceRule),
            ])
        }
    }

    private var genericConstraintsObject: [String: JSONValue] {
        var value = retainedConstraints
        value.removeValue(forKey: "calendar_event")
        value.removeValue(forKey: "calendar_context")
        value.removeValue(forKey: "dayweave_firm_block")

        var schedulingConstraints: [String: JSONValue] = [:]
        if hasEarliestStart, earliestStartStrength == .soft {
            schedulingConstraints["earliest_start"] = qualified(
                .string(Self.format(earliestStart)),
                strength: earliestStartStrength,
                softWeight: earliestStartSoftWeight
            )
        }
        if hasDeadline, deadlineStrength == .soft {
            schedulingConstraints["latest_finish"] = qualified(
                .string(Self.format(deadline)),
                strength: deadlineStrength,
                softWeight: deadlineSoftWeight
            )
        }
        if hasMinimumNotice {
            schedulingConstraints["minimum_notice"] = qualified(
                number(minimumNoticeMinutes),
                strength: minimumNoticeStrength,
                softWeight: minimumNoticeSoftWeight
            )
        }
        if hasAllowedWeekdays {
            schedulingConstraints["allowed_weekdays"] = qualified(
                .array(orderedWeekdays(allowedWeekdays)),
                strength: allowedWeekdaysStrength,
                softWeight: allowedWeekdaysSoftWeight
            )
        }
        schedulingConstraints["preferred_daily_windows"] = preferredDailyWindows.isEmpty
            ? nil
            : .array(preferredDailyWindows.map { window in
                qualified(
                    .object([
                        "weekdays": .array(orderedWeekdays(window.weekdays)),
                        "start_minute": number(UInt32(window.startMinute)),
                        "end_minute": number(UInt32(window.endMinute)),
                    ]),
                    strength: window.strength,
                    softWeight: window.softWeight
                )
            })
        schedulingConstraints["preferred_absolute_windows"] = preferredAbsoluteWindows.isEmpty
            ? nil
            : .array(preferredAbsoluteWindows.map { window in
                qualified(
                    absoluteWindow(start: window.start, end: window.end),
                    strength: window.strength,
                    softWeight: window.softWeight
                )
            })
        schedulingConstraints["forbidden_windows"] = forbiddenWindows.isEmpty
            ? nil
            : .array(forbiddenWindows.map { window in
                qualified(
                    absoluteWindow(start: window.start, end: window.end),
                    strength: window.strength,
                    softWeight: window.softWeight
                )
            })
        schedulingConstraints["required_contexts"] = requiredContexts.isEmpty
            ? nil
            : .array(requiredContexts.map { context in
                qualified(
                    .string(context.value),
                    strength: context.strength,
                    softWeight: context.softWeight
                )
            })
        if hasRequiredLocation {
            schedulingConstraints["required_location"] = qualified(
                .string(requiredLocation),
                strength: requiredLocationStrength,
                softWeight: requiredLocationSoftWeight
            )
        }
        if hasBuffers {
            schedulingConstraints["buffers"] = .object([
                "before": number(bufferBeforeMinutes),
                "after": number(bufferAfterMinutes),
                "strength": bufferHasStrength
                    ? strengthValue(bufferStrength, softWeight: bufferSoftWeight)
                    : .null,
            ])
        }
        if hadExplicitNullOccurrenceWindow {
            schedulingConstraints["occurrence_window"] = .null
        }
        if hasMaximumDailyWork {
            schedulingConstraints["maximum_daily_work"] = qualified(
                number(maximumDailyWorkMinutes),
                strength: maximumDailyWorkStrength,
                softWeight: maximumDailyWorkSoftWeight
            )
        }
        if hasMaximumWeeklyWork {
            schedulingConstraints["maximum_weekly_work"] = qualified(
                number(maximumWeeklyWorkMinutes),
                strength: maximumWeeklyWorkStrength,
                softWeight: maximumWeeklyWorkSoftWeight
            )
        }
        if schedulingConstraints.isEmpty {
            value.removeValue(forKey: "constraints")
        } else {
            value["constraints"] = .object(schedulingConstraints)
        }

        if energy == .unspecified {
            value.removeValue(forKey: "energy")
        } else {
            if energyStrength == .soft,
               energySoftWeight == 100,
               !energyUsesQualifiedEncoding {
                value["energy"] = .string(energy.rawValue)
            } else {
                value["energy"] = qualified(
                    .string(energy.rawValue),
                    strength: energyStrength,
                    softWeight: energySoftWeight
                )
            }
        }
        value["tags"] = tags.isEmpty
            ? nil
            : .array(tags.map(\.value).sorted().map(JSONValue.string))
        value["preferred_start_minute"] = hasPreferredStartMinute
            ? number(UInt32(preferredStartMinute))
            : nil
        if ((kind == .goal || kind == .routine) && hasOwnEffort)
            || hadOwnEffortConstraint {
            value["has_own_effort"] = .bool(hasOwnEffort)
        } else {
            value.removeValue(forKey: "has_own_effort")
        }

        value["habit_target"] = kind == .habit && hasHabitTarget
            ? .object([
                "amount": number(habitTargetAmount),
                "unit": .string(habitTargetUnit),
            ])
            : nil
        value["preserves_streak_when_paused"] = kind == .habit
                && (!preservesStreakWhenPaused || hadPreservesStreakConstraint)
            ? .bool(preservesStreakWhenPaused)
            : nil
        value["routine_ordered"] = kind == .routine
                && (routineOrdered || hadRoutineOrderedConstraint)
            ? .bool(routineOrdered)
            : nil
        value["goal_measures"] = kind == .goal && !goalMeasures.isEmpty
            ? .array(goalMeasures.map { measure in
                .object([
                    "name": .string(measure.name),
                    "target": number(measure.target),
                    "current": number(measure.current),
                    "unit": .string(measure.unit),
                ])
            })
            : nil
        if kind == .goal, hasGoalWeeklyAllocation {
            value["goal_weekly_allocation"] = .object([
                "minimum": number(goalWeeklyMinimumMinutes),
                "maximum": hasGoalWeeklyMaximum
                    ? number(goalWeeklyMaximumMinutes)
                    : .null,
            ])
        } else {
            value.removeValue(forKey: "goal_weekly_allocation")
        }
        value["break_category"] = kind == .breakTime
                && (breakCategory != .other || hadBreakCategoryConstraint)
            ? .string(breakCategory.rawValue)
            : nil
        value["break_mandatory"] = kind == .breakTime
                && (breakMandatory || hadBreakMandatoryConstraint)
            ? .bool(breakMandatory)
            : nil
        value["break_prompt_to_resume"] = kind == .breakTime
                && (!breakPromptToResume || hadBreakPromptConstraint)
            ? .bool(breakPromptToResume)
            : nil

        if isSplittable {
            value["maximum_sessions"] = hasMaximumSessions ? number(UInt32(maximumSessions)) : nil
            value["minimum_gap_minutes"] = minimumGapMinutes > 0
                ? number(minimumGapMinutes)
                : nil
            value["maximum_split_days"] = hasMaximumSplitDays
                ? number(UInt32(maximumSplitDays))
                : nil
        } else {
            value.removeValue(forKey: "maximum_sessions")
            value.removeValue(forKey: "minimum_gap_minutes")
            value.removeValue(forKey: "maximum_split_days")
        }
        return value
    }

    private var constraintsValue: JSONValue {
        var generic = genericConstraintsObject
        guard kind == .event, hasEventTiming, generic.isEmpty else {
            return .object(generic)
        }
        let start = eventRangeIsUnchanged
            ? originalEvent?.startWireValue ?? Self.format(eventStart)
            : Self.format(eventStart)
        let end = eventRangeIsUnchanged
            ? originalEvent?.endWireValue ?? Self.format(eventEnd)
            : Self.format(eventEnd)
        if originalEvent?.metadataKind == .calendarEvent {
            var event: [String: JSONValue] = [
                "start": .string(start),
                "end": .string(end),
                "immutable": .bool(eventIsImmutable),
                "all_day": .bool(eventIsAllDay),
            ]
            if originalEvent?.hadSourceCalendarIDField != false {
                event["source_calendar_id"] = eventSourceCalendarID.map(JSONValue.string)
                    ?? .null
            }
            generic["calendar_event"] = .object(event)
            return .object(generic)
        }
        return .object([
            "dayweave_firm_block": .object([
                "owned": .bool(true),
                "starts_at": .string(start),
                "ends_at": .string(end),
                "all_day": .bool(eventIsAllDay),
                "tentative": .bool(eventIsTentative),
                "busy": .bool(eventIsBusy),
            ]),
        ])
    }

    private var eventDurationValue: UInt32? {
        if let originalEvent,
           originalEvent.start == eventStart,
           originalEvent.end == eventEnd {
            return originalEvent.durationSeconds
        }
        return Self.durationSeconds(from: eventStart, to: eventEnd)
    }

    private var eventRangeIsUnchanged: Bool {
        originalEvent.map { $0.start == eventStart && $0.end == eventEnd } ?? false
    }

    private var schedulingMetadataValidationIssue: String? {
        func invalidWeight(
            _ strength: CanonicalItemEditorConstraintStrength,
            _ weight: UInt32
        ) -> Bool {
            strength == .soft && weight > Self.maximumSoftWeight
        }
        let weightedValues: [(CanonicalItemEditorConstraintStrength, UInt32)] = [
            (earliestStartStrength, earliestStartSoftWeight),
            (deadlineStrength, deadlineSoftWeight),
            (minimumNoticeStrength, minimumNoticeSoftWeight),
            (allowedWeekdaysStrength, allowedWeekdaysSoftWeight),
            (requiredLocationStrength, requiredLocationSoftWeight),
            (maximumDailyWorkStrength, maximumDailyWorkSoftWeight),
            (maximumWeeklyWorkStrength, maximumWeeklyWorkSoftWeight),
            (energyStrength, energySoftWeight),
        ] + preferredDailyWindows.map { ($0.strength, $0.softWeight) }
            + preferredAbsoluteWindows.map { ($0.strength, $0.softWeight) }
            + forbiddenWindows.map { ($0.strength, $0.softWeight) }
            + requiredContexts.map { ($0.strength, $0.softWeight) }
            + (hasBuffers && bufferHasStrength ? [(bufferStrength, bufferSoftWeight)] : [])
        if weightedValues.contains(where: { invalidWeight($0.0, $0.1) }) {
            return "Soft constraint weights must be at most \(Self.maximumSoftWeight)."
        }
        if hasEarliestStart, hasDeadline, earliestStart >= deadline {
            return "Earliest start must be before the deadline."
        }
        if hasAllowedWeekdays, allowedWeekdays.isEmpty {
            return "Choose at least one allowed weekday."
        }
        for window in preferredDailyWindows {
            if window.startMinute >= 1_440
                || window.endMinute > 1_440
                || window.startMinute == window.endMinute {
                return "Daily windows must describe a non-empty range within a day."
            }
        }
        for window in preferredAbsoluteWindows + forbiddenWindows where window.start >= window.end {
            return "Absolute window end must be after its start."
        }
        if requiredContexts.contains(where: {
            $0.value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }) {
            return "Required contexts cannot be empty."
        }
        if hasRequiredLocation,
           requiredLocation.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return "Required location cannot be empty."
        }
        if hasBuffers, bufferHasStrength, bufferBeforeMinutes == 0, bufferAfterMinutes == 0 {
            return "A buffer needs preparation or decompression time."
        }
        if hasMinimumNotice, minimumNoticeMinutes > Self.maximumSchedulingOffsetMinutes {
            return "Minimum notice must be at most \(Self.maximumSchedulingOffsetMinutes) minutes."
        }
        if hasBuffers,
           (bufferBeforeMinutes > Self.maximumSchedulingOffsetMinutes
            || bufferAfterMinutes > Self.maximumSchedulingOffsetMinutes) {
            return "Buffers must be at most \(Self.maximumSchedulingOffsetMinutes) minutes."
        }
        let tagValues = tags.map(\.value)
        if tagValues.contains(where: {
            $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }) || Set(tagValues).count != tagValues.count {
            return "Tags must be non-empty and unique."
        }
        if hasPreferredStartMinute {
            guard kind != .event else {
                return "Fixed events cannot use a preferred start minute."
            }
            guard hasDuration else {
                return "Preferred start requires a duration estimate."
            }
            let durationMinutes = (UInt64(durationSeconds) + 59) / 60
            if UInt64(preferredStartMinute) + durationMinutes > 1_440 {
                return "Preferred start and duration must finish within the same day."
            }
        }
        if kind == .habit, hasHabitTarget {
            if habitTargetAmount == 0 { return "Habit target must be greater than zero." }
            if habitTargetUnit.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return "Habit target unit cannot be empty."
            }
        }
        if kind == .goal {
            if goalMeasures.contains(where: {
                $0.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    || $0.unit.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }) {
                return "Goal measures need a name and unit."
            }
            if hasGoalWeeklyAllocation {
                if hasGoalWeeklyMaximum,
                   goalWeeklyMaximumMinutes < goalWeeklyMinimumMinutes {
                    return "Weekly allocation maximum cannot be below its minimum."
                }
            }
        }
        if isSplittable {
            if hasMaximumSessions, maximumSessions == 0 {
                return "Maximum sessions must be at least one."
            }
            if hasMaximumSplitDays, maximumSplitDays == 0 {
                return "Maximum split days must be at least one."
            }
            if minimumGapMinutes > Self.maximumSchedulingOffsetMinutes {
                return "Minimum split gap must be at most \(Self.maximumSchedulingOffsetMinutes) minutes."
            }
        }
        return nil
    }

    private func number(_ value: UInt32) -> JSONValue {
        .number(JSONNumber(UInt64(value)))
    }

    private func number(_ value: Int64) -> JSONValue {
        .number(JSONNumber(integerLiteral: value))
    }

    private func orderedWeekdays(
        _ values: Set<CanonicalItemEditorWeekday>
    ) -> [JSONValue] {
        CanonicalItemEditorWeekday.allCases.compactMap { weekday in
            values.contains(weekday) ? .string(weekday.rawValue) : nil
        }
    }

    private func strengthValue(
        _ strength: CanonicalItemEditorConstraintStrength,
        softWeight: UInt32
    ) -> JSONValue {
        switch strength {
        case .hard:
            .object(["level": .string("hard")])
        case .soft:
            .object([
                "level": .string("soft"),
                "weight": number(softWeight),
            ])
        }
    }

    private func qualified(
        _ value: JSONValue,
        strength: CanonicalItemEditorConstraintStrength,
        softWeight: UInt32
    ) -> JSONValue {
        .object([
            "value": value,
            "strength": strengthValue(strength, softWeight: softWeight),
        ])
    }

    private func absoluteWindow(start: Date, end: Date) -> JSONValue {
        .object([
            "start": .string(Self.format(start)),
            "end": .string(Self.format(end)),
        ])
    }

    private var eventCalendar: Calendar? {
        guard let timeZone = DayWeaveCanonicalItemDraft.supportedTimeZone(
            identifier: timezoneName
        ) else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        return calendar
    }

    private mutating func normalizeAllDayEventBounds() {
        guard let calendar = eventCalendar else { return }
        let normalizedStart = calendar.startOfDay(for: eventStart)
        let normalizedEnd = calendar.startOfDay(for: eventEnd)
        eventStart = normalizedStart
        eventEnd = normalizedEnd > normalizedStart
            ? normalizedEnd
            : (calendar.date(byAdding: .day, value: 1, to: normalizedStart)
                ?? normalizedStart.addingTimeInterval(86_400))
        refreshAllDayLocalDateSpan(using: calendar)
    }

    private mutating func refreshAllDayLocalDateSpan(using calendar: Calendar? = nil) {
        guard eventIsAllDay, let calendar = calendar ?? eventCalendar else { return }
        let components = calendar.dateComponents([.year, .month, .day], from: eventStart)
        guard let year = components.year,
              let month = components.month,
              let day = components.day else { return }
        allDayLocalDateSpan = .init(
            year: year,
            month: month,
            day: day,
            dayCount: allDaySpan(using: calendar)
        )
    }

    private func allDaySpan(using calendar: Calendar) -> Int {
        let start = calendar.startOfDay(for: eventStart)
        let end = calendar.startOfDay(for: eventEnd)
        guard end > start else { return 1 }
        return max(1, calendar.dateComponents([.day], from: start, to: end).day ?? 1)
    }

    private mutating func parseReadyStatus(_ status: DayWeaveCanonicalItemStatus) {
        guard status == .inbox || status == .planned else {
            markReadOnly("Only Inbox and Planned items can be edited from this view.")
            return
        }
    }

    private mutating func parseRecurrence(_ value: JSONValue?) {
        guard let value else { return }
        guard case let .object(object) = value,
              case let .string(type)? = object["type"] else {
            markReadOnly("This recurrence cannot be represented by the typed editor.")
            return
        }
        switch type {
        case "daily":
            recurrence = .daily
            guard let count = object["times_per_day"].map(Self.unsigned) ?? 1 else {
                markReadOnly("This daily recurrence has no editable repeat count.")
                return
            }
            recurrenceCount = count
        case "weekly":
            recurrence = .weekly
            let parsed = object["weekdays"].flatMap(Self.parseWeekdays) ?? []
            weekdays = parsed
            guard let count = object["times_per_week"].map(Self.unsigned)
                    ?? UInt32(max(1, parsed.count)) else {
                markReadOnly("This weekly recurrence has no editable repeat count.")
                return
            }
            recurrenceCount = count
        case "monthly":
            recurrence = .monthly
            guard let count = object["times_per_month"].map(Self.unsigned) ?? 1 else {
                markReadOnly("This monthly recurrence has no editable repeat count.")
                return
            }
            recurrenceCount = count
        case "every_interval":
            recurrence = .everyInterval
            guard let interval = Self.unsigned(object["interval"]) else {
                markReadOnly("This rolling recurrence has no editable interval.")
                return
            }
            recurrenceIntervalMinutes = interval
        case "after_completion":
            recurrence = .afterCompletion
            guard let interval = Self.unsigned(object["interval"]) else {
                markReadOnly("This completion-relative recurrence has no editable interval.")
                return
            }
            recurrenceIntervalMinutes = interval
        case "frequency":
            recurrence = .frequency
            guard let target = Self.unsigned(object["target"]),
                  case let .string(periodRaw)? = object["period"],
                  let period = CanonicalItemEditorRecurrencePeriod(rawValue: periodRaw),
                  case let .string(semanticsRaw)? = object["semantics"],
                  let semantics = CanonicalItemEditorRecurrenceSemantics(
                    rawValue: semanticsRaw
                  ) else {
                markReadOnly("This frequency recurrence has unsupported required fields.")
                return
            }
            recurrenceCount = target
            recurrencePeriod = period
            recurrenceSemantics = semantics
            recurrenceMinimumSpacingMinutes = Self.unsigned(
                object["minimum_spacing"]
            ) ?? 0
            if object["minimum_spacing"] != nil,
               Self.unsigned(object["minimum_spacing"]) == nil {
                markReadOnly("This frequency recurrence has invalid spacing.")
                return
            }
            if let weekdayValue = object["weekdays"] {
                guard let parsed = Self.parseWeekdays(weekdayValue) else {
                    markReadOnly("This frequency recurrence contains an unknown weekday.")
                    return
                }
                weekdays = parsed
            }
            if let anchorValue = object["anchor"], anchorValue != .null {
                guard case let .string(raw)? = object["anchor"],
                      let parsed = Self.parse(raw) else {
                    markReadOnly("This frequency recurrence has an invalid anchor.")
                    return
                }
                hasRecurrenceAnchor = true
                recurrenceAnchor = parsed
            }
        case "custom":
            recurrence = .custom
            guard Set(object.keys) == ["type", "rrule"],
                  case let .string(rule)? = object["rrule"],
                  !rule.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                markReadOnly("This custom recurrence rule cannot be represented.")
                return
            }
            customRecurrenceRule = rule
        default:
            markReadOnly("This recurrence form is preserved but is not editable in the typed editor.")
        }
    }

    private mutating func parseConstraints(
        _ value: JSONValue,
        originalDurationSeconds: UInt32?,
        originalCanonicalEarliestStart: Date?,
        originalCanonicalDeadline: Date?
    ) {
        guard case var .object(object) = value else {
            markReadOnly("This item has constraints that the typed editor cannot preserve.")
            return
        }
        if let firmBlock = object["dayweave_firm_block"], firmBlock != .null,
           object.count != 1 {
            markReadOnly("DayWeave-owned events require firm timing as their sole constraint.")
        }
        if let energyValue = object.removeValue(forKey: "energy"), energyValue != .null {
            let raw: String
            if case let .string(simple) = energyValue {
                raw = simple
                energyStrength = .soft
                energySoftWeight = 100
                energyUsesQualifiedEncoding = false
            } else if let qualified = Self.parseQualified(energyValue),
                      case let .string(qualifiedRaw) = qualified.value {
                raw = qualifiedRaw
                energyStrength = qualified.strength
                energySoftWeight = qualified.softWeight
                energyUsesQualifiedEncoding = true
            } else {
                markReadOnly("This item uses an energy rule that the typed editor cannot preserve.")
                return
            }
            guard let parsed = CanonicalItemEditorEnergy(rawValue: raw),
                  parsed != .unspecified else {
                markReadOnly("This item uses an energy rule that the typed editor cannot preserve.")
                return
            }
            energy = parsed
        }
        if let ownEffortValue = object.removeValue(forKey: "has_own_effort") {
            guard case let .bool(parsed) = ownEffortValue else {
                markReadOnly("This item uses an own-effort rule that cannot be edited.")
                return
            }
            hasOwnEffort = parsed
            hadOwnEffortConstraint = true
        }
        if let constraintsValue = object.removeValue(forKey: "constraints") {
            parseSchedulingConstraints(constraintsValue)
        }
        if let goalIDs = object.removeValue(forKey: "goal_ids"), goalIDs != .array([]) {
            markReadOnly("Goal relationships are preserved until graph editing is available.")
            return
        }
        if let tagsValue = object.removeValue(forKey: "tags") {
            guard case let .array(values) = tagsValue else {
                markReadOnly("This item uses tags the typed editor cannot preserve.")
                return
            }
            var parsed: [CanonicalItemEditorTag] = []
            for value in values {
                guard case let .string(tag) = value else {
                    markReadOnly("This item uses tags the typed editor cannot preserve.")
                    return
                }
                parsed.append(.init(value: tag))
            }
            tags = parsed
        }
        if let preferredValue = object.removeValue(forKey: "preferred_start_minute"),
           preferredValue != .null {
            guard let parsed = Self.unsigned(preferredValue), parsed < 1_440 else {
                markReadOnly("This preferred start minute cannot be represented.")
                return
            }
            hasPreferredStartMinute = true
            preferredStartMinute = UInt16(parsed)
        }
        if let targetValue = object.removeValue(forKey: "habit_target"), targetValue != .null {
            guard case let .object(target) = targetValue,
                  Set(target.keys) == ["amount", "unit"],
                  let amount = Self.unsigned(target["amount"]),
                  case let .string(unit)? = target["unit"] else {
                markReadOnly("This habit target cannot be represented by the typed editor.")
                return
            }
            hasHabitTarget = true
            habitTargetAmount = amount
            habitTargetUnit = unit
        }
        if let pauseValue = object.removeValue(forKey: "preserves_streak_when_paused") {
            guard case let .bool(parsed) = pauseValue else {
                markReadOnly("This habit pause behavior cannot be represented.")
                return
            }
            preservesStreakWhenPaused = parsed
            hadPreservesStreakConstraint = true
        }
        if let orderedValue = object.removeValue(forKey: "routine_ordered") {
            guard case let .bool(parsed) = orderedValue else {
                markReadOnly("This routine order cannot be represented.")
                return
            }
            routineOrdered = parsed
            hadRoutineOrderedConstraint = true
        }
        if let measuresValue = object.removeValue(forKey: "goal_measures") {
            guard case let .array(values) = measuresValue else {
                markReadOnly("These goal measures cannot be represented.")
                return
            }
            var parsed: [CanonicalItemEditorGoalMeasure] = []
            for value in values {
                guard case let .object(measure) = value,
                      Set(measure.keys) == ["name", "target", "current", "unit"],
                      case let .string(name)? = measure["name"],
                      let target = Self.signed(measure["target"]),
                      let current = Self.signed(measure["current"]),
                      case let .string(unit)? = measure["unit"] else {
                    markReadOnly("These goal measures cannot be represented.")
                    return
                }
                parsed.append(.init(name: name, target: target, current: current, unit: unit))
            }
            goalMeasures = parsed
        }
        if let allocationValue = object.removeValue(forKey: "goal_weekly_allocation"),
           allocationValue != .null {
            guard case let .object(allocation) = allocationValue,
                  Set(allocation.keys).isSubset(of: ["minimum", "maximum"]),
                  allocation.keys.contains("minimum"),
                  let minimum = Self.unsigned(allocation["minimum"]) else {
                markReadOnly("This weekly goal allocation cannot be represented.")
                return
            }
            hasGoalWeeklyAllocation = true
            goalWeeklyMinimumMinutes = minimum
            if let maximumValue = allocation["maximum"], maximumValue != .null {
                guard let maximum = Self.unsigned(maximumValue) else {
                    markReadOnly("This weekly goal allocation cannot be represented.")
                    return
                }
                hasGoalWeeklyMaximum = true
                goalWeeklyMaximumMinutes = maximum
            }
        }
        if let categoryValue = object.removeValue(forKey: "break_category"),
           categoryValue != .null {
            guard case let .string(raw) = categoryValue,
                  let parsed = CanonicalItemEditorBreakCategory(rawValue: raw) else {
                markReadOnly("This break category cannot be represented.")
                return
            }
            breakCategory = parsed
            hadBreakCategoryConstraint = true
        }
        if let mandatoryValue = object.removeValue(forKey: "break_mandatory") {
            guard case let .bool(parsed) = mandatoryValue else {
                markReadOnly("This break behavior cannot be represented.")
                return
            }
            breakMandatory = parsed
            hadBreakMandatoryConstraint = true
        }
        if let promptValue = object.removeValue(forKey: "break_prompt_to_resume") {
            guard case let .bool(parsed) = promptValue else {
                markReadOnly("This break resume behavior cannot be represented.")
                return
            }
            breakPromptToResume = parsed
            hadBreakPromptConstraint = true
        }
        if let maximumValue = object.removeValue(forKey: "maximum_sessions"),
           maximumValue != .null {
            guard let parsed = Self.unsigned(maximumValue), parsed <= UInt16.max else {
                markReadOnly("This split session cap cannot be represented.")
                return
            }
            hasMaximumSessions = true
            maximumSessions = UInt16(parsed)
        }
        if let gapValue = object.removeValue(forKey: "minimum_gap_minutes") {
            guard let parsed = Self.unsigned(gapValue) else {
                markReadOnly("This split-session gap cannot be represented.")
                return
            }
            minimumGapMinutes = parsed
        }
        if let daysValue = object.removeValue(forKey: "maximum_split_days"),
           daysValue != .null {
            guard let parsed = Self.unsigned(daysValue), parsed <= UInt16.max else {
                markReadOnly("This split-day cap cannot be represented.")
                return
            }
            hasMaximumSplitDays = true
            maximumSplitDays = UInt16(parsed)
        }
        let calendarEventValue = object.removeValue(forKey: "calendar_event")
        let calendarContextValue = object.removeValue(forKey: "calendar_context")
        let firmBlockValue = object.removeValue(forKey: "dayweave_firm_block")
        if let calendarContextValue, calendarContextValue != .null {
            markReadOnly("Calendar context timing is system-owned and cannot be edited here.")
        }
        let eventValue = calendarEventValue == .null ? nil : calendarEventValue
        let firmValue = firmBlockValue == .null ? nil : firmBlockValue
        if eventValue != nil, firmValue != nil {
            markReadOnly("An event cannot combine imported and DayWeave-owned timing metadata.")
        } else if let eventValue {
            guard kind == .event,
                  case let .object(event) = eventValue,
                  case let .string(start)? = event["start"],
                  case let .string(end)? = event["end"],
                  case let .bool(immutable)? = event["immutable"],
                  case let .bool(allDay)? = event["all_day"],
                  let parsedStart = Self.parse(start),
                  let parsedEnd = Self.parse(end) else {
                markReadOnly("This calendar event has metadata that the typed editor cannot preserve.")
                return
            }
            eventStart = parsedStart
            eventEnd = parsedEnd
            hasEventTiming = true
            eventIsImmutable = immutable
            eventIsAllDay = allDay
            originalEvent = .init(
                start: parsedStart,
                end: parsedEnd,
                startWireValue: start,
                endWireValue: end,
                durationSeconds: originalDurationSeconds,
                canonicalEarliestStart: originalCanonicalEarliestStart,
                canonicalDeadline: originalCanonicalDeadline,
                metadataKind: .calendarEvent,
                hadSourceCalendarIDField: event.keys.contains("source_calendar_id")
            )
            switch event["source_calendar_id"] {
            case let .string(source)?:
                eventSourceCalendarID = source
                markReadOnly("Calendar-linked events must be edited in their source calendar.")
            case .null?, nil:
                eventSourceCalendarID = nil
            default:
                markReadOnly("This calendar event has an unsupported source binding.")
            }
            markReadOnly("Imported or legacy calendar events must be edited in their source calendar.")
        } else if let firmValue {
            guard kind == .event,
                  case let .object(firm) = firmValue,
                  firm["owned"] == .bool(true),
                  case let .string(start)? = firm["starts_at"],
                  case let .string(end)? = firm["ends_at"],
                  let parsedStart = Self.parse(start),
                  let parsedEnd = Self.parse(end) else {
                markReadOnly("This DayWeave-owned event has timing that cannot be preserved.")
                retainedConstraints = object
                return
            }
            eventStart = parsedStart
            eventEnd = parsedEnd
            hasEventTiming = true
            eventIsImmutable = true
            eventIsAllDay = Self.boolean(firm["all_day"], default: false) ?? false
            eventIsTentative = Self.boolean(firm["tentative"], default: false) ?? false
            eventIsBusy = Self.boolean(firm["busy"], default: true) ?? true
            if Self.boolean(firm["all_day"], default: false) == nil
                || Self.boolean(firm["tentative"], default: false) == nil
                || Self.boolean(firm["busy"], default: true) == nil {
                markReadOnly("This DayWeave-owned event has unsupported publication flags.")
            }
            originalEvent = .init(
                start: parsedStart,
                end: parsedEnd,
                startWireValue: start,
                endWireValue: end,
                durationSeconds: originalDurationSeconds,
                canonicalEarliestStart: originalCanonicalEarliestStart,
                canonicalDeadline: originalCanonicalDeadline,
                metadataKind: .dayWeaveFirmBlock,
                hadSourceCalendarIDField: false
            )
        }
        retainedConstraints = object
    }

    private mutating func parseSchedulingConstraints(_ value: JSONValue) {
        guard case var .object(object) = value else {
            markReadOnly("These scheduling constraints cannot be represented.")
            return
        }
        if let boundary = object.removeValue(forKey: "earliest_start"), boundary != .null {
            guard let parsed = Self.parseQualifiedDate(boundary) else {
                markReadOnly("This earliest-start constraint cannot be represented.")
                return
            }
            if hasEarliestStart {
                markReadOnly("Earliest start is defined in two conflicting places.")
                return
            }
            hasEarliestStart = true
            earliestStart = parsed.value
            earliestStartStrength = parsed.strength
            earliestStartSoftWeight = parsed.softWeight
        }
        if let boundary = object.removeValue(forKey: "latest_finish"), boundary != .null {
            guard let parsed = Self.parseQualifiedDate(boundary) else {
                markReadOnly("This deadline constraint cannot be represented.")
                return
            }
            if hasDeadline {
                markReadOnly("Deadline is defined in two conflicting places.")
                return
            }
            hasDeadline = true
            deadline = parsed.value
            deadlineStrength = parsed.strength
            deadlineSoftWeight = parsed.softWeight
        }
        if let notice = object.removeValue(forKey: "minimum_notice"), notice != .null {
            guard let parsed = Self.parseQualifiedUnsigned(notice) else {
                markReadOnly("This minimum-notice constraint cannot be represented.")
                return
            }
            hasMinimumNotice = true
            minimumNoticeMinutes = parsed.value
            minimumNoticeStrength = parsed.strength
            minimumNoticeSoftWeight = parsed.softWeight
        }
        if let allowed = object.removeValue(forKey: "allowed_weekdays"), allowed != .null {
            guard let parsed = Self.parseQualified(allowed),
                  let parsedDays = Self.parseWeekdays(parsed.value) else {
                markReadOnly("This weekday constraint cannot be represented.")
                return
            }
            hasAllowedWeekdays = true
            allowedWeekdays = parsedDays
            allowedWeekdaysStrength = parsed.strength
            allowedWeekdaysSoftWeight = parsed.softWeight
        }
        if let windows = object.removeValue(forKey: "preferred_daily_windows") {
            guard case let .array(values) = windows else {
                markReadOnly("These daily windows cannot be represented.")
                return
            }
            var parsed: [CanonicalItemEditorDailyWindow] = []
            for value in values {
                guard let qualified = Self.parseQualified(value),
                      case let .object(window) = qualified.value,
                      Set(window.keys) == ["weekdays", "start_minute", "end_minute"],
                      let days = Self.parseWeekdays(window["weekdays"]),
                      let start = Self.unsigned(window["start_minute"]),
                      let end = Self.unsigned(window["end_minute"]),
                      start <= UInt16.max, end <= UInt16.max else {
                    markReadOnly("These daily windows cannot be represented.")
                    return
                }
                parsed.append(.init(
                    weekdays: days,
                    startMinute: UInt16(start),
                    endMinute: UInt16(end),
                    strength: qualified.strength,
                    softWeight: qualified.softWeight
                ))
            }
            preferredDailyWindows = parsed
        }
        if let windows = object.removeValue(forKey: "preferred_absolute_windows") {
            guard let parsed = Self.parseAbsoluteWindows(windows) else {
                markReadOnly("These preferred windows cannot be represented.")
                return
            }
            preferredAbsoluteWindows = parsed
        }
        if let windows = object.removeValue(forKey: "forbidden_windows") {
            guard let parsed = Self.parseAbsoluteWindows(windows) else {
                markReadOnly("These forbidden windows cannot be represented.")
                return
            }
            forbiddenWindows = parsed
        }
        if let contexts = object.removeValue(forKey: "required_contexts") {
            guard case let .array(values) = contexts else {
                markReadOnly("These context constraints cannot be represented.")
                return
            }
            var parsed: [CanonicalItemEditorQualifiedText] = []
            for value in values {
                guard let qualified = Self.parseQualified(value),
                      case let .string(text) = qualified.value else {
                    markReadOnly("These context constraints cannot be represented.")
                    return
                }
                parsed.append(.init(
                    value: text,
                    strength: qualified.strength,
                    softWeight: qualified.softWeight
                ))
            }
            requiredContexts = parsed
        }
        if let location = object.removeValue(forKey: "required_location"), location != .null {
            guard let parsed = Self.parseQualified(location),
                  case let .string(text) = parsed.value else {
                markReadOnly("This location constraint cannot be represented.")
                return
            }
            hasRequiredLocation = true
            requiredLocation = text
            requiredLocationStrength = parsed.strength
            requiredLocationSoftWeight = parsed.softWeight
        }
        if let buffers = object.removeValue(forKey: "buffers") {
            guard case let .object(buffer) = buffers,
                  Set(buffer.keys) == ["before", "after", "strength"],
                  let before = Self.unsigned(buffer["before"]),
                  let after = Self.unsigned(buffer["after"]) else {
                markReadOnly("This buffer constraint cannot be represented.")
                return
            }
            hasBuffers = true
            bufferBeforeMinutes = before
            bufferAfterMinutes = after
            if buffer["strength"] == .null {
                bufferHasStrength = false
            } else if let parsedStrength = Self.parseStrength(buffer["strength"]) {
                bufferHasStrength = true
                bufferStrength = parsedStrength.strength
                bufferSoftWeight = parsedStrength.softWeight
            } else {
                markReadOnly("This buffer constraint cannot be represented.")
                return
            }
        }
        if let cap = object.removeValue(forKey: "maximum_daily_work"), cap != .null {
            guard let parsed = Self.parseQualifiedUnsigned(cap) else {
                markReadOnly("This daily work cap cannot be represented.")
                return
            }
            hasMaximumDailyWork = true
            maximumDailyWorkMinutes = parsed.value
            maximumDailyWorkStrength = parsed.strength
            maximumDailyWorkSoftWeight = parsed.softWeight
        }
        if let cap = object.removeValue(forKey: "maximum_weekly_work"), cap != .null {
            guard let parsed = Self.parseQualifiedUnsigned(cap) else {
                markReadOnly("This weekly work cap cannot be represented.")
                return
            }
            hasMaximumWeeklyWork = true
            maximumWeeklyWorkMinutes = parsed.value
            maximumWeeklyWorkStrength = parsed.strength
            maximumWeeklyWorkSoftWeight = parsed.softWeight
        }
        if let occurrenceWindow = object.removeValue(forKey: "occurrence_window") {
            if occurrenceWindow == .null {
                hadExplicitNullOccurrenceWindow = true
            } else {
                markReadOnly("A materialized occurrence window is scheduler-owned and read-only.")
            }
        }
        if let dependencies = object.removeValue(forKey: "dependencies"),
           dependencies != .array([]) {
            markReadOnly("Dependencies are preserved until graph editing is available.")
        }
        if !object.isEmpty {
            markReadOnly("These scheduling constraints include fields this editor cannot author.")
        }
    }

    private mutating func parseSplitPolicy(_ value: DayWeaveSplitPolicy) {
        switch value {
        case .indivisible:
            break
        case let .splittable(minimum, maximum):
            isSplittable = true
            minimumChunkSeconds = minimum
            maximumChunkSeconds = maximum
        case .unknown:
            markReadOnly("This split policy is preserved but cannot be edited.")
        }
    }

    private mutating func markReadOnly(_ diagnostic: String) {
        if readOnlyDiagnostic == nil { readOnlyDiagnostic = diagnostic }
    }

    private static func unsigned(_ value: JSONValue?) -> UInt32? {
        guard case let .number(number)? = value else { return nil }
        return number.exactUInt32
    }

    private static func signed(_ value: JSONValue?) -> Int64? {
        guard case let .number(number)? = value else { return nil }
        return Int64(number.displayDescription)
    }

    private struct ParsedQualified: Sendable {
        let value: JSONValue
        let strength: CanonicalItemEditorConstraintStrength
        let softWeight: UInt32
    }

    private struct ParsedQualifiedDate: Sendable {
        let value: Date
        let strength: CanonicalItemEditorConstraintStrength
        let softWeight: UInt32
    }

    private struct ParsedQualifiedUnsigned: Sendable {
        let value: UInt32
        let strength: CanonicalItemEditorConstraintStrength
        let softWeight: UInt32
    }

    private static func parseStrength(
        _ value: JSONValue?
    ) -> (strength: CanonicalItemEditorConstraintStrength, softWeight: UInt32)? {
        guard case let .object(object)? = value,
              case let .string(level)? = object["level"] else { return nil }
        switch level {
        case "hard" where Set(object.keys) == ["level"]:
            return (.hard, 100)
        case "soft" where Set(object.keys) == ["level", "weight"]:
            guard let weight = unsigned(object["weight"]),
                  weight <= maximumSoftWeight else { return nil }
            return (.soft, weight)
        default:
            return nil
        }
    }

    private static func parseQualified(_ value: JSONValue) -> ParsedQualified? {
        guard case let .object(object) = value,
              Set(object.keys) == ["value", "strength"],
              let qualifiedValue = object["value"],
              let strength = parseStrength(object["strength"]) else { return nil }
        return .init(
            value: qualifiedValue,
            strength: strength.strength,
            softWeight: strength.softWeight
        )
    }

    private static func parseQualifiedDate(_ value: JSONValue) -> ParsedQualifiedDate? {
        guard let parsed = parseQualified(value),
              case let .string(raw) = parsed.value,
              let date = parse(raw) else { return nil }
        return .init(
            value: date,
            strength: parsed.strength,
            softWeight: parsed.softWeight
        )
    }

    private static func parseQualifiedUnsigned(
        _ value: JSONValue
    ) -> ParsedQualifiedUnsigned? {
        guard let parsed = parseQualified(value),
              let unsignedValue = unsigned(parsed.value) else { return nil }
        return .init(
            value: unsignedValue,
            strength: parsed.strength,
            softWeight: parsed.softWeight
        )
    }

    private static func parseWeekdays(
        _ value: JSONValue?
    ) -> Set<CanonicalItemEditorWeekday>? {
        guard case let .array(values)? = value else { return nil }
        var result = Set<CanonicalItemEditorWeekday>()
        for value in values {
            guard case let .string(raw) = value,
                  let weekday = CanonicalItemEditorWeekday(rawValue: raw),
                  result.insert(weekday).inserted else { return nil }
        }
        return result
    }

    private static func parseAbsoluteWindows(
        _ value: JSONValue
    ) -> [CanonicalItemEditorAbsoluteWindow]? {
        guard case let .array(values) = value else { return nil }
        var result: [CanonicalItemEditorAbsoluteWindow] = []
        for value in values {
            guard let qualified = parseQualified(value),
                  case let .object(window) = qualified.value,
                  Set(window.keys) == ["start", "end"],
                  case let .string(startRaw)? = window["start"],
                  case let .string(endRaw)? = window["end"],
                  let start = parse(startRaw),
                  let end = parse(endRaw) else { return nil }
            result.append(.init(
                start: start,
                end: end,
                strength: qualified.strength,
                softWeight: qualified.softWeight
            ))
        }
        return result
    }

    private static func boolean(_ value: JSONValue?, default defaultValue: Bool) -> Bool? {
        guard let value else { return defaultValue }
        guard case let .bool(result) = value else { return nil }
        return result
    }

    private static func durationSeconds(from start: Date, to end: Date) -> UInt32? {
        guard let start = CanonicalRFC3339Instant(date: start),
              let end = CanonicalRFC3339Instant(date: end) else { return nil }
        let durationMicroseconds = end.microsecondsSinceUnixEpoch
            - start.microsecondsSinceUnixEpoch
        guard durationMicroseconds.isMultiple(of: 1_000_000) else { return nil }
        let durationSeconds = durationMicroseconds / 1_000_000
        guard durationSeconds > 0,
              durationSeconds <= Int64(DayWeaveCanonicalItemDraft.maximumDurationSeconds) else {
            return nil
        }
        return UInt32(durationSeconds)
    }

    private static func parse(_ value: String) -> Date? {
        guard let instant = CanonicalRFC3339Instant(value),
              instant.hasPostgresPrecision else { return nil }
        return instant.exactlyRepresentableDate
    }

    private static func format(_ date: Date) -> String {
        CanonicalRFC3339Instant(date: date)?.canonicalUTCString ?? ""
    }
}
