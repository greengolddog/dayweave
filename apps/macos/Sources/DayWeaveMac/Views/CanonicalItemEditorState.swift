import Foundation

enum CanonicalItemEditorRecurrence: String, CaseIterable, Identifiable, Sendable {
    case none
    case daily
    case weekly
    case monthly
    case everyInterval
    case afterCompletion

    var id: Self { self }

    var title: String {
        switch self {
        case .none: "Does not repeat"
        case .daily: "Daily"
        case .weekly: "Weekly"
        case .monthly: "Monthly"
        case .everyInterval: "Every interval"
        case .afterCompletion: "After completion"
        }
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

struct CanonicalItemEditorState: Equatable, Sendable {
    static let maximumRecurrenceCount: UInt32 = 64
    static let maximumIntervalMinutes: UInt32 = 10 * 365 * 24 * 60
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
    var hasDeadline: Bool
    var deadline: Date
    var importance: UInt8
    var urgency: UInt8

    var recurrence: CanonicalItemEditorRecurrence
    var recurrenceCount: UInt32
    var recurrenceIntervalMinutes: UInt32
    var weekdays: Set<CanonicalItemEditorWeekday>
    var energy: CanonicalItemEditorEnergy
    var hasOwnEffort: Bool

    var eventStart: Date
    var eventEnd: Date
    var eventIsImmutable: Bool
    var eventIsAllDay: Bool
    var eventIsTentative: Bool
    var eventIsBusy: Bool

    var isSplittable: Bool
    var minimumChunkSeconds: UInt32
    var maximumChunkSeconds: UInt32
    var parentID: UUID?
    var siblingOrder: UInt32

    private var retainedConstraints: [String: JSONValue]
    private var eventSourceCalendarID: String?
    private var originalEvent: OriginalEventState?
    private var allDayLocalDateSpan: AllDayLocalDateSpan?
    private var hadOwnEffortConstraint: Bool
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
        hasDeadline = source.deadlineAt != nil
        deadline = source.deadlineAt ?? now.addingTimeInterval(24 * 60 * 60)
        importance = source.importance
        urgency = source.urgency
        recurrence = .none
        recurrenceCount = 1
        recurrenceIntervalMinutes = 24 * 60
        weekdays = []
        energy = .unspecified
        hasOwnEffort = false
        eventStart = now
        eventEnd = now.addingTimeInterval(60 * 60)
        eventIsImmutable = true
        eventIsAllDay = false
        eventIsTentative = false
        eventIsBusy = true
        isSplittable = false
        minimumChunkSeconds = 15 * 60
        maximumChunkSeconds = source.durationSeconds ?? 30 * 60
        parentID = source.parentID
        siblingOrder = source.siblingOrder
        retainedConstraints = [:]
        eventSourceCalendarID = nil
        originalEvent = nil
        allDayLocalDateSpan = nil
        hadOwnEffortConstraint = false
        readOnlyDiagnostic = nil

        parseReadyStatus(source.status)
        parseRecurrence(source.recurrence)
        parseConstraints(
            source.flexibleConstraints,
            originalDurationSeconds: draft == nil ? nil : source.durationSeconds
        )
        parseSplitPolicy(source.splitPolicy)
        if eventIsAllDay { refreshAllDayLocalDateSpan() }
        if case .unknown = source.kind {
            markReadOnly("This item type is not editable by this version of DayWeave.")
        }
        if source.parentID == itemID {
            markReadOnly("This item has an invalid self-referencing parent.")
        }
    }

    static let defaultTimezoneName = "UTC"

    var supportsRecurrence: Bool {
        switch kind {
        case .task, .habit, .routine: true
        default: false
        }
    }

    var validationIssue: String? {
        if let readOnlyDiagnostic { return readOnlyDiagnostic }
        guard readyStatus == .inbox || readyStatus == .planned else {
            return "Captured items must be either Inbox or Planned."
        }
        if kind == .event {
            let interval = eventEnd.timeIntervalSince(eventStart)
            guard interval.isFinite, interval >= 1,
                  interval <= TimeInterval(DayWeaveCanonicalItemDraft.maximumDurationSeconds) else {
                return "Event end must be after its start and no more than 366 days later."
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
            case .none:
                break
            }
        }
        return draft.validationIssue(itemID: itemID)
    }

    var draft: DayWeaveCanonicalItemDraft {
        return DayWeaveCanonicalItemDraft(
            isSensitive: isSensitive,
            kind: kind,
            status: readyStatus,
            title: title,
            notes: notes,
            timezoneName: timezoneName,
            durationSeconds: kind == .event
                ? eventDurationValue
                : (hasDuration ? durationSeconds : nil),
            deadlineAt: hasDeadline ? deadline : nil,
            earliestStartAt: hasEarliestStart ? earliestStart : nil,
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
            hasOwnEffort = true
            hadOwnEffortConstraint = true
        } else {
            hasOwnEffort = false
            hadOwnEffortConstraint = false
        }
        if kind == .event {
            isSplittable = false
            hasDuration = true
            if eventEnd <= eventStart {
                eventEnd = eventStart.addingTimeInterval(60 * 60)
            }
            if eventIsAllDay { normalizeAllDayEventBounds() }
        }
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
        }
    }

    private var constraintsValue: JSONValue {
        var value = retainedConstraints
        if kind == .event {
            let rangeIsUnchanged = originalEvent.map {
                $0.start == eventStart && $0.end == eventEnd
            } ?? false
            let start = rangeIsUnchanged
                    ? originalEvent?.startWireValue ?? Self.format(eventStart)
                    : Self.format(eventStart)
            let end = rangeIsUnchanged
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
                value.removeValue(forKey: "dayweave_firm_block")
                value["calendar_event"] = .object(event)
                return .object(value)
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
        value.removeValue(forKey: "calendar_event")
        value.removeValue(forKey: "dayweave_firm_block")
        if energy == .unspecified {
            value.removeValue(forKey: "energy")
        } else {
            value["energy"] = .string(energy.rawValue)
        }
        if kind == .goal || kind == .routine || hadOwnEffortConstraint {
            value["has_own_effort"] = .bool(hasOwnEffort)
        } else {
            value.removeValue(forKey: "has_own_effort")
        }
        return .object(value)
    }

    private var eventDurationValue: UInt32? {
        if let originalEvent,
           originalEvent.start == eventStart,
           originalEvent.end == eventEnd {
            return originalEvent.durationSeconds
        }
        return Self.durationSeconds(from: eventStart, to: eventEnd)
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
            guard let count = Self.unsigned(object["times_per_day"]) else {
                markReadOnly("This daily recurrence has no editable repeat count.")
                return
            }
            recurrenceCount = count
        case "weekly":
            recurrence = .weekly
            guard let count = Self.unsigned(object["times_per_week"]) else {
                markReadOnly("This weekly recurrence has no editable repeat count.")
                return
            }
            recurrenceCount = count
            guard case let .array(values)? = object["weekdays"] else {
                markReadOnly("This weekly recurrence has no editable weekday list.")
                return
            }
            var parsed = Set<CanonicalItemEditorWeekday>()
            for value in values {
                guard case let .string(raw) = value,
                      let weekday = CanonicalItemEditorWeekday(rawValue: raw) else {
                    markReadOnly("This weekly recurrence contains an unknown weekday.")
                    return
                }
                parsed.insert(weekday)
            }
            weekdays = parsed
        case "monthly":
            recurrence = .monthly
            guard let count = Self.unsigned(object["times_per_month"]) else {
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
        default:
            markReadOnly("This recurrence form is preserved but is not editable in the typed editor.")
        }
    }

    private mutating func parseConstraints(
        _ value: JSONValue,
        originalDurationSeconds: UInt32?
    ) {
        guard case var .object(object) = value else {
            markReadOnly("This item has constraints that the typed editor cannot preserve.")
            return
        }
        if object["dayweave_firm_block"] != nil, object.count != 1 {
            markReadOnly("DayWeave-owned events require firm timing as their sole constraint.")
        }
        if let energyValue = object.removeValue(forKey: "energy") {
            guard case let .string(raw) = energyValue,
                  let parsed = CanonicalItemEditorEnergy(rawValue: raw),
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
        let calendarEventValue = object.removeValue(forKey: "calendar_event")
        let firmBlockValue = object.removeValue(forKey: "dayweave_firm_block")
        if calendarEventValue != nil, firmBlockValue != nil {
            markReadOnly("An event cannot combine imported and DayWeave-owned timing metadata.")
        } else if let eventValue = calendarEventValue {
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
            eventIsImmutable = immutable
            eventIsAllDay = allDay
            originalEvent = .init(
                start: parsedStart,
                end: parsedEnd,
                startWireValue: start,
                endWireValue: end,
                durationSeconds: originalDurationSeconds,
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
        } else if let firmValue = firmBlockValue {
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
                metadataKind: .dayWeaveFirmBlock,
                hadSourceCalendarIDField: false
            )
        }
        retainedConstraints = object
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

    private static func boolean(_ value: JSONValue?, default defaultValue: Bool) -> Bool? {
        guard let value else { return defaultValue }
        guard case let .bool(result) = value else { return nil }
        return result
    }

    private static func durationSeconds(from start: Date, to end: Date) -> UInt32? {
        let duration = end.timeIntervalSince(start)
        guard duration.isFinite, duration >= 1,
              duration <= TimeInterval(DayWeaveCanonicalItemDraft.maximumDurationSeconds) else {
            return nil
        }
        return UInt32(duration.rounded())
    }

    private static func parse(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) { return date }
        let ordinary = ISO8601DateFormatter()
        ordinary.formatOptions = [.withInternetDateTime]
        return ordinary.date(from: value)
    }

    private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.string(from: date)
    }
}
