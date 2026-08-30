import Foundation

enum CanonicalAuthoringDisposition: String, Codable, Equatable, Sendable {
    case pending
    case conflicted
}

enum CanonicalAuthoringOperation: String, Codable, Equatable, Sendable {
    case create
    case replace
    case trash
    case restore
}

/// A complete, typed item body retained in encrypted local storage before any
/// canonical authoring request can leave the Mac. It deliberately excludes
/// server-owned revision and timestamp fields.
struct DayWeaveCanonicalItemDraft: Codable, Equatable, Sendable {
    static let maximumTitleScalars = 500
    static let maximumNotesScalars = 100_000
    static let maximumDurationSeconds: UInt32 = 366 * 24 * 60 * 60
    static let maximumSiblingOrder: UInt32 = 1_000_000
    static let maximumRecurrenceBytes = 16 * 1_024
    static let maximumConstraintBytes = 32 * 1_024
    private static let chronoNonRegionTimeZoneIdentifiers: Set<String> = [
        "CET", "CST6CDT", "Cuba", "EET", "EST", "EST5EDT", "Egypt", "Eire",
        "GB", "GB-Eire", "GMT", "GMT+0", "GMT-0", "GMT0", "Greenwich", "HST",
        "Hongkong", "Iceland", "Iran", "Israel", "Jamaica", "Japan", "Kwajalein",
        "Libya", "MET", "MST", "MST7MDT", "NZ", "NZ-CHAT", "Navajo", "PRC",
        "PST8PDT", "Poland", "Portugal", "ROC", "ROK", "Singapore", "Turkey",
        "UCT", "UTC", "Universal", "W-SU", "WET", "Zulu",
    ]

    var isSensitive: Bool
    var kind: DayWeaveCanonicalItemKind
    var status: DayWeaveCanonicalItemStatus
    var title: String
    var notes: String?
    var timezoneName: String
    var durationSeconds: UInt32?
    var deadlineAt: Date?
    var earliestStartAt: Date?
    var recurrence: JSONValue?
    var flexibleConstraints: JSONValue
    var splitPolicy: DayWeaveSplitPolicy
    var importance: UInt8
    var urgency: UInt8
    var parentID: UUID?
    var siblingOrder: UInt32

    private enum CodingKeys: String, CodingKey {
        case kind, status, title, notes, recurrence, importance, urgency
        case isSensitive = "is_sensitive"
        case timezoneName = "timezone_name"
        case durationSeconds = "duration_seconds"
        case deadlineAt = "deadline_at"
        case earliestStartAt = "earliest_start_at"
        case flexibleConstraints = "flexible_constraints"
        case splitPolicy = "split_policy"
        case parentID = "parent_id"
        case siblingOrder = "sibling_order"
    }

    init(
        isSensitive: Bool = false,
        kind: DayWeaveCanonicalItemKind = .task,
        status: DayWeaveCanonicalItemStatus = .inbox,
        title: String,
        notes: String? = nil,
        timezoneName: String,
        durationSeconds: UInt32? = nil,
        deadlineAt: Date? = nil,
        earliestStartAt: Date? = nil,
        recurrence: JSONValue? = nil,
        flexibleConstraints: JSONValue = .object([:]),
        splitPolicy: DayWeaveSplitPolicy = .indivisible,
        importance: UInt8 = 50,
        urgency: UInt8 = 50,
        parentID: UUID? = nil,
        siblingOrder: UInt32 = 0
    ) {
        self.isSensitive = isSensitive
        self.kind = kind
        self.status = status
        self.title = title
        self.notes = notes
        self.timezoneName = timezoneName
        self.durationSeconds = durationSeconds
        self.deadlineAt = deadlineAt
        self.earliestStartAt = earliestStartAt
        self.recurrence = recurrence
        self.flexibleConstraints = flexibleConstraints
        self.splitPolicy = splitPolicy
        self.importance = importance
        self.urgency = urgency
        self.parentID = parentID
        self.siblingOrder = siblingOrder
    }

    init(item: DayWeaveCanonicalItem) {
        self.init(
            isSensitive: item.isSensitive,
            kind: item.kind,
            status: item.status,
            title: item.title,
            notes: item.notes,
            timezoneName: item.timezoneName,
            durationSeconds: item.durationSeconds,
            deadlineAt: item.deadlineAt,
            earliestStartAt: item.earliestStartAt,
            recurrence: item.recurrence,
            flexibleConstraints: item.flexibleConstraints,
            splitPolicy: item.splitPolicy,
            importance: item.importance,
            urgency: item.urgency,
            parentID: item.parentID,
            siblingOrder: item.siblingOrder
        )
    }

    var normalized: Self {
        var copy = self
        copy.title = copy.title.trimmingCharacters(in: .whitespacesAndNewlines)
        if copy.notes?.isEmpty == true { copy.notes = nil }
        copy.deadlineAt = copy.deadlineAt.map(Self.wireCanonicalDate)
        copy.earliestStartAt = copy.earliestStartAt.map(Self.wireCanonicalDate)
        return copy
    }

    private static func wireCanonicalDate(_ value: Date) -> Date {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: formatter.string(from: value)) ?? value
    }

    func validationIssue(itemID: UUID) -> String? {
        let value = normalized
        guard !value.title.isEmpty,
              value.title.unicodeScalars.count <= Self.maximumTitleScalars else {
            return "Title must contain 1–\(Self.maximumTitleScalars) Unicode characters."
        }
        guard value.notes?.unicodeScalars.count ?? 0 <= Self.maximumNotesScalars else {
            return "Notes exceed the 100,000-character limit."
        }
        guard let timeZone = Self.supportedTimeZone(identifier: value.timezoneName) else {
            return "Choose a valid IANA timezone."
        }
        if let duration = value.durationSeconds,
           duration == 0 || duration > Self.maximumDurationSeconds {
            return "Duration must be between one second and 366 days."
        }
        if let earliest = value.earliestStartAt,
           let deadline = value.deadlineAt,
           earliest >= deadline {
            return "Earliest start must be before the deadline."
        }
        if let recurrence = value.recurrence {
            guard recurrence.supportsCanonicalAuthoringRecurrence else {
                return "This recurrence form is not editable by this version of DayWeave."
            }
            guard ((try? JSONEncoder().encode(recurrence).count) ?? Int.max)
                    <= Self.maximumRecurrenceBytes else {
                return "Recurrence data exceeds the 16 KiB limit."
            }
        }
        guard value.flexibleConstraints.supportsCanonicalAuthoringConstraints else {
            return "These advanced constraints are read-only in this version of DayWeave."
        }
        if value.kind == .event,
           !value.flexibleConstraints.hasValidCanonicalAllDayBounds(in: timeZone) {
            return "All-day events must use local midnight bounds and an exclusive later end date."
        }
        guard ((try? JSONEncoder().encode(value.flexibleConstraints).count) ?? Int.max)
                <= Self.maximumConstraintBytes else {
            return "Constraint data exceeds the 32 KiB limit."
        }
        switch value.splitPolicy {
        case .indivisible:
            break
        case let .splittable(minimum, maximum):
            guard let duration = value.durationSeconds,
                  minimum > 0,
                  maximum >= minimum,
                  minimum <= duration,
                  maximum <= duration else {
                return "Split bounds must be positive, ordered, and no longer than the duration."
            }
        case .unknown:
            return "This split policy is read-only in this version of DayWeave."
        }
        guard value.importance <= 100, value.urgency <= 100 else {
            return "Importance and urgency must be between 0 and 100."
        }
        guard value.siblingOrder <= Self.maximumSiblingOrder else {
            return "Sibling order must be at most 1,000,000."
        }
        guard value.parentID != itemID else { return "An item cannot be its own parent." }
        switch value.kind {
        case .habit where value.recurrence == nil:
            return "Habits require a recurrence."
        case .event where value.recurrence != nil,
             .goal where value.recurrence != nil,
             .breakTime where value.recurrence != nil:
            return "Events, goals, and breaks cannot use task recurrence."
        case .unknown:
            return "This item type is read-only in this version of DayWeave."
        default:
            break
        }
        if value.kind == .event,
           !value.flexibleConstraints.hasCanonicalEventTiming {
            return "Events require calendar event timing metadata."
        }
        if value.kind != .event,
           value.flexibleConstraints.hasCanonicalEventTiming {
            return "Calendar event timing metadata is only valid for event items."
        }
        guard value.status == .inbox || value.status == .planned else {
            return "Authored items must be either Inbox or Planned."
        }
        return nil
    }

    /// Foundation accepts convenience aliases such as `PST` and `GMT+2` that
    /// the server's IANA parser rejects. Region identifiers are accepted only
    /// when Foundation can resolve them; the explicit non-region set mirrors
    /// chrono-tz 0.10.4, which is the server's parser.
    static func supportedTimeZone(identifier: String) -> TimeZone? {
        guard identifier.contains("/")
                || Self.chronoNonRegionTimeZoneIdentifiers.contains(identifier) else {
            return nil
        }
        return TimeZone(identifier: identifier)
    }

    var requestFields: DayWeaveCanonicalItemFields {
        let value = normalized
        return DayWeaveCanonicalItemFields(
            isSensitive: value.isSensitive,
            kind: value.kind,
            status: value.status,
            title: value.title,
            notes: value.notes,
            timezoneName: value.timezoneName,
            durationSeconds: value.durationSeconds,
            deadlineAt: value.deadlineAt,
            earliestStartAt: value.earliestStartAt,
            recurrence: value.recurrence,
            flexibleConstraints: value.flexibleConstraints,
            splitPolicy: value.splitPolicy,
            importance: value.importance,
            urgency: value.urgency,
            parentID: value.parentID,
            siblingOrder: value.siblingOrder
        )
    }

    func matches(_ item: DayWeaveCanonicalItem) -> Bool {
        let value = normalized
        return item.isSensitive == value.isSensitive
            && item.kind == value.kind
            && item.status == value.status
            && item.title == value.title
            && item.notes == value.notes
            && item.timezoneName == value.timezoneName
            && item.durationSeconds == value.durationSeconds
            && item.deadlineAt == value.deadlineAt
            && item.earliestStartAt == value.earliestStartAt
            && item.recurrence == value.recurrence
            && item.flexibleConstraints == value.flexibleConstraints
            && item.splitPolicy == value.splitPolicy
            && item.importance == value.importance
            && item.urgency == value.urgency
            && item.parentID == value.parentID
            && item.siblingOrder == value.siblingOrder
    }
}

/// One exact, idempotent canonical mutation. Submitted entries are immutable
/// until the same request is proven committed or rejected.
struct DayWeavePendingCanonicalAuthoringMutation: Codable, Equatable, Identifiable, Sendable {
    static let currentVersion = 1

    let version: Int
    let id: UUID
    let itemID: UUID
    let operation: CanonicalAuthoringOperation
    let draft: DayWeaveCanonicalItemDraft?
    let expectedRevision: UInt64?
    let baseItem: DayWeaveCanonicalItem?
    let idempotencyKey: String
    let createdAt: Date
    var configurationIdentifier: String?
    var hasBeenSubmitted: Bool
    var disposition: CanonicalAuthoringDisposition
    var diagnostic: String?

    init(
        id: UUID = UUID(),
        itemID: UUID,
        operation: CanonicalAuthoringOperation,
        draft: DayWeaveCanonicalItemDraft? = nil,
        expectedRevision: UInt64? = nil,
        baseItem: DayWeaveCanonicalItem? = nil,
        createdAt: Date = Date(),
        configurationIdentifier: String? = nil,
        hasBeenSubmitted: Bool = false,
        disposition: CanonicalAuthoringDisposition = .pending,
        diagnostic: String? = nil
    ) {
        version = Self.currentVersion
        self.id = id
        self.itemID = itemID
        self.operation = operation
        self.draft = draft?.normalized
        self.expectedRevision = expectedRevision
        self.baseItem = baseItem
        idempotencyKey = "mac-item-\(id.uuidString.lowercased())"
        self.createdAt = createdAt
        self.configurationIdentifier = configurationIdentifier
        self.hasBeenSubmitted = hasBeenSubmitted
        self.disposition = disposition
        self.diagnostic = diagnostic
    }

    var isValid: Bool {
        guard version == Self.currentVersion,
              idempotencyKey == "mac-item-\(id.uuidString.lowercased())",
              configurationIdentifier.map({ !$0.isEmpty && $0.utf8.count <= 4_096 }) ?? true,
              disposition == .pending || diagnostic?.isEmpty == false else { return false }
        switch operation {
        case .create:
            return expectedRevision == nil
                && baseItem == nil
                && draft?.validationIssue(itemID: itemID) == nil
        case .replace:
            return expectedRevision != nil
                && expectedRevision == baseItem?.revision
                && baseItem?.id == itemID
                && baseItem?.deletedAt == nil
                && draft?.validationIssue(itemID: itemID) == nil
        case .trash:
            return draft == nil
                && expectedRevision != nil
                && (baseItem == nil || (expectedRevision == baseItem?.revision
                    && baseItem?.id == itemID
                    && baseItem?.deletedAt == nil))
        case .restore:
            return draft == nil
                && expectedRevision != nil
                && (baseItem == nil || (baseItem?.id == itemID
                    && baseItem?.revision == expectedRevision
                    && baseItem?.deletedAt != nil))
        }
    }

    var displayTitle: String {
        draft?.title ?? baseItem?.title ?? "Item \(itemID.uuidString.prefix(8))"
    }
}

struct DayWeaveCanonicalTrashEntry: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let revision: UInt64
    let deletedAt: Date
    let parentID: UUID?
    let lastKnownItem: DayWeaveCanonicalItem?

    init(item: DayWeaveCanonicalItem) {
        id = item.id
        revision = item.revision
        deletedAt = item.deletedAt ?? item.updatedAt
        parentID = item.parentID
        lastKnownItem = item
    }

    init(tombstone: DayWeaveItemTombstone, lastKnownItem: DayWeaveCanonicalItem?) {
        id = tombstone.id
        revision = tombstone.revision
        deletedAt = tombstone.deletedAt
        parentID = tombstone.parentID
        self.lastKnownItem = lastKnownItem
    }

    init(
        id: UUID,
        revision: UInt64,
        deletedAt: Date,
        parentID: UUID?,
        lastKnownItem: DayWeaveCanonicalItem?
    ) {
        self.id = id
        self.revision = revision
        self.deletedAt = deletedAt
        self.parentID = parentID
        self.lastKnownItem = lastKnownItem
    }

    var withoutRetainedItemBody: Self {
        Self(
            id: id,
            revision: revision,
            deletedAt: deletedAt,
            parentID: parentID,
            lastKnownItem: nil
        )
    }

    /// Remote clocks are not a retention authority. Persist the first local
    /// observation as the latest possible deletion timestamp so a future-dated
    /// tombstone cannot extend the thirty-day privacy window indefinitely.
    func clampingDeletedAt(to localObservation: Date) -> Self {
        guard deletedAt > localObservation else { return self }
        return Self(
            id: id,
            revision: revision,
            deletedAt: localObservation,
            parentID: parentID,
            lastKnownItem: lastKnownItem
        )
    }

    var title: String { lastKnownItem?.title ?? "Deleted item \(id.uuidString.prefix(8))" }
    var isSensitive: Bool { lastKnownItem?.isSensitive ?? true }
}

extension DayWeaveCanonicalItem {
    /// Full authoring is allowed for the typed subset whose semantic values can
    /// be reconstructed safely. Legacy background status/privacy publication
    /// intentionally keeps using the stricter lossless-replacement predicate.
    var supportsCanonicalAuthoringReplacement: Bool {
        guard unsupportedFields.isEmpty, splitPolicy.isSupportedForWrite else { return false }
        if case .unknown = kind { return false }
        if case .unknown = status { return false }
        guard recurrence?.supportsCanonicalAuthoringRecurrence ?? true,
              flexibleConstraints.supportsCanonicalAuthoringConstraints else { return false }
        guard (kind == .event) == flexibleConstraints.hasCanonicalEventTiming else {
            return false
        }
        return true
    }
}

extension JSONValue {
    var supportsCanonicalAuthoringRecurrence: Bool {
        guard case let .object(object) = self,
              case let .string(type)? = object["type"] else { return false }
        func unsigned(
            _ key: String,
            allowZero: Bool = false,
            maximum: UInt32 = .max
        ) -> Bool {
            guard case let .number(number)? = object[key],
                  let value = number.exactUInt32,
                  value <= maximum else { return false }
            return allowZero || value > 0
        }
        func weekdays(_ key: String) -> Bool {
            guard case let .array(values)? = object[key] else { return false }
            let allowed = Set([
                "monday", "tuesday", "wednesday", "thursday",
                "friday", "saturday", "sunday",
            ])
            let parsed = values.compactMap { value -> String? in
                if case let .string(day) = value { day } else { nil }
            }
            return parsed.count == values.count
                && parsed.count <= allowed.count
                && Set(parsed).count == parsed.count
                && Set(parsed).isSubset(of: allowed)
        }
        func string(_ key: String, isOneOf allowed: Set<String>) -> Bool {
            guard case let .string(value)? = object[key] else { return false }
            return allowed.contains(value)
        }
        switch type {
        case "daily":
            return Set(object.keys) == ["type", "times_per_day"]
                && unsigned("times_per_day", maximum: UInt32(UInt16.max))
        case "weekly":
            return Set(object.keys) == ["type", "times_per_week", "weekdays"]
                && unsigned("times_per_week", maximum: UInt32(UInt16.max))
                && weekdays("weekdays")
        case "monthly":
            return Set(object.keys) == ["type", "times_per_month"]
                && unsigned("times_per_month", maximum: UInt32(UInt16.max))
        case "every_interval", "after_completion":
            return Set(object.keys) == ["type", "interval"] && unsigned("interval")
        case "frequency":
            let allowed: Set<String> = [
                "type", "target", "period", "semantics", "weekdays",
                "minimum_spacing", "anchor",
            ]
            guard Set(object.keys).isSubset(of: allowed),
                  Set(["type", "target", "period", "semantics"]).isSubset(of: object.keys),
                  unsigned("target", maximum: UInt32(UInt16.max)),
                  string("period", isOneOf: ["day", "week", "month"]),
                  string("semantics", isOneOf: ["calendar", "rolling"]) else { return false }
            if object["weekdays"] != nil, !weekdays("weekdays") { return false }
            if object["minimum_spacing"] != nil,
               !unsigned("minimum_spacing", allowZero: true) { return false }
            if let anchor = object["anchor"] {
                switch anchor {
                case let .string(value):
                    let fractional = ISO8601DateFormatter()
                    fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
                    let ordinary = ISO8601DateFormatter()
                    ordinary.formatOptions = [.withInternetDateTime]
                    guard fractional.date(from: value) != nil
                            || ordinary.date(from: value) != nil else { return false }
                case .null:
                    break
                default: return false
                }
            }
            return true
        case "custom":
            // The typed editor cannot validate the full RFC 5545 grammar yet.
            // Preserve custom rules read-only instead of saving a recurrence
            // that canonical storage accepts but composition later rejects.
            return false
        default:
            return false
        }
    }

    var supportsCanonicalAuthoringConstraints: Bool {
        guard case let .object(object) = self else { return false }
        let allowed: Set<String> = [
            "energy", "has_own_effort", "routine_ordered",
            "preserves_streak_when_paused", "break_category",
            "break_mandatory", "break_prompt_to_resume", "calendar_event",
            "dayweave_firm_block",
        ]
        guard Set(object.keys).isSubset(of: allowed) else { return false }
        if object["dayweave_firm_block"] != nil,
           Set(object.keys) != ["dayweave_firm_block"] { return false }
        for (key, value) in object {
            switch key {
            case "energy":
                guard case let .string(level) = value,
                      ["low", "medium", "deep"].contains(level) else { return false }
            case "has_own_effort", "routine_ordered", "preserves_streak_when_paused",
                 "break_mandatory", "break_prompt_to_resume":
                guard case .bool = value else { return false }
            case "break_category":
                guard case let .string(category) = value,
                      ["rest", "meal", "movement", "pomodoro", "decompression", "other"]
                        .contains(category) else { return false }
            case "calendar_event":
                guard value.supportsCanonicalCalendarEvent else { return false }
            case "dayweave_firm_block":
                guard value.supportsCanonicalFirmBlock else { return false }
            default:
                return false
            }
        }
        return true
    }

    fileprivate var hasCanonicalEventTiming: Bool {
        guard case let .object(object) = self else { return false }
        return (object["calendar_event"] != nil) != (object["dayweave_firm_block"] != nil)
    }

    fileprivate func hasValidCanonicalAllDayBounds(in timeZone: TimeZone) -> Bool {
        guard case let .object(constraints) = self else { return true }
        let event: [String: JSONValue]
        let startKey: String
        let endKey: String
        if case let .object(calendarEvent)? = constraints["calendar_event"] {
            event = calendarEvent
            startKey = "start"
            endKey = "end"
        } else if case let .object(firmBlock)? = constraints["dayweave_firm_block"] {
            event = firmBlock
            startKey = "starts_at"
            endKey = "ends_at"
        } else {
            return true
        }
        guard case let .bool(allDay)? = event["all_day"] else { return true }
        guard allDay else { return true }
        guard case let .string(startValue)? = event[startKey],
              case let .string(endValue)? = event[endKey],
              let start = CanonicalRFC3339Instant(startValue),
              let end = CanonicalRFC3339Instant(endValue),
              start.isWholeSecond,
              end.isWholeSecond else { return false }

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let startDate = start.dateAtWholeSecond
        let endDate = end.dateAtWholeSecond
        guard calendar.startOfDay(for: startDate) == startDate,
              calendar.startOfDay(for: endDate) == endDate else { return false }
        let startParts = calendar.dateComponents([.year, .month, .day], from: startDate)
        let endParts = calendar.dateComponents([.year, .month, .day], from: endDate)
        guard let startYear = startParts.year,
              let startMonth = startParts.month,
              let startDay = startParts.day,
              let endYear = endParts.year,
              let endMonth = endParts.month,
              let endDay = endParts.day else { return false }
        if startYear != endYear { return startYear < endYear }
        if startMonth != endMonth { return startMonth < endMonth }
        return startDay < endDay
    }

    private var supportsCanonicalCalendarEvent: Bool {
        guard case let .object(object) = self else { return false }
        let required: Set<String> = ["start", "end", "immutable", "all_day"]
        let allowed = required.union(["source_calendar_id"])
        guard required.isSubset(of: object.keys), Set(object.keys).isSubset(of: allowed),
              case let .string(startValue)? = object["start"],
              case let .string(endValue)? = object["end"],
              let start = CanonicalRFC3339Instant(startValue),
              let end = CanonicalRFC3339Instant(endValue),
              start < end,
              case .bool? = object["immutable"], case .bool? = object["all_day"] else {
            return false
        }
        if let source = object["source_calendar_id"] {
            switch source {
            case .string, .null: break
            default: return false
            }
        }
        return true
    }

    private var supportsCanonicalFirmBlock: Bool {
        guard case let .object(object) = self else { return false }
        let required: Set<String> = ["owned", "starts_at", "ends_at"]
        let allowed = required.union(["all_day", "tentative", "busy"])
        guard required.isSubset(of: object.keys), Set(object.keys).isSubset(of: allowed),
              object["owned"] == .bool(true),
              case let .string(startValue)? = object["starts_at"],
              case let .string(endValue)? = object["ends_at"],
              let start = CanonicalRFC3339Instant(startValue),
              let end = CanonicalRFC3339Instant(endValue),
              start < end else { return false }
        for key in ["all_day", "tentative", "busy"] where object[key] != nil {
            guard case .bool? = object[key] else { return false }
        }
        return true
    }
}

/// A deliberately small RFC 3339 parser for persisted scheduling evidence.
/// `ISO8601DateFormatter` is unsuitable for validation because it accepts
/// impossible calendar dates and offsets without the required colon. Keeping
/// the fractional digits also lets the ordering check remain exact below the
/// precision represented by `Date`.
private struct CanonicalRFC3339Instant: Comparable {
    private static let secondsFromYearOneToUnixEpoch: Int64 = 62_135_596_800

    private let wholeSeconds: Int64
    private let fractionalDigits: [UInt8]

    var isWholeSecond: Bool { fractionalDigits.allSatisfy { $0 == 0 } }

    var dateAtWholeSecond: Date {
        Date(timeIntervalSince1970: TimeInterval(
            wholeSeconds - Self.secondsFromYearOneToUnixEpoch
        ))
    }

    init?(_ value: String) {
        let bytes = Array(value.utf8)
        guard bytes.count >= 20,
              bytes[4] == Character.asciiHyphen,
              bytes[7] == Character.asciiHyphen,
              bytes[10] == Character.asciiUppercaseT,
              bytes[13] == Character.asciiColon,
              bytes[16] == Character.asciiColon,
              let year = Self.decimal(bytes, 0..<4),
              let month = Self.decimal(bytes, 5..<7),
              let day = Self.decimal(bytes, 8..<10),
              let hour = Self.decimal(bytes, 11..<13),
              let minute = Self.decimal(bytes, 14..<16),
              let second = Self.decimal(bytes, 17..<19),
              (1...9_999).contains(year),
              (1...12).contains(month),
              (1...Self.daysInMonth(month, year: year)).contains(day),
              (0...23).contains(hour),
              (0...59).contains(minute),
              (0...59).contains(second) else { return nil }

        var cursor = 19
        var fraction: [UInt8] = []
        if cursor < bytes.count, bytes[cursor] == Character.asciiPeriod {
            cursor += 1
            let fractionStart = cursor
            while cursor < bytes.count, Self.isDigit(bytes[cursor]) {
                fraction.append(bytes[cursor] - Character.asciiZero)
                cursor += 1
            }
            guard cursor > fractionStart else { return nil }
        }

        let offsetSeconds: Int
        if cursor + 1 == bytes.count, bytes[cursor] == Character.asciiUppercaseZ {
            offsetSeconds = 0
        } else {
            guard cursor + 6 == bytes.count,
                  bytes[cursor] == Character.asciiPlus
                    || bytes[cursor] == Character.asciiHyphen,
                  bytes[cursor + 3] == Character.asciiColon,
                  let offsetHour = Self.decimal(bytes, (cursor + 1)..<(cursor + 3)),
                  let offsetMinute = Self.decimal(bytes, (cursor + 4)..<(cursor + 6)),
                  (0...23).contains(offsetHour),
                  (0...59).contains(offsetMinute) else { return nil }
            let magnitude = offsetHour * 3_600 + offsetMinute * 60
            offsetSeconds = bytes[cursor] == Character.asciiHyphen ? -magnitude : magnitude
        }

        let priorYear = Int64(year - 1)
        var elapsedDays = priorYear * 365
            + priorYear / 4
            - priorYear / 100
            + priorYear / 400
        let daysBeforeMonth = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
        elapsedDays += Int64(daysBeforeMonth[month - 1])
        if month > 2, Self.isLeapYear(year) { elapsedDays += 1 }
        elapsedDays += Int64(day - 1)

        wholeSeconds = elapsedDays * 86_400
            + Int64(hour * 3_600 + minute * 60 + second - offsetSeconds)
        fractionalDigits = fraction
    }

    static func < (lhs: Self, rhs: Self) -> Bool {
        if lhs.wholeSeconds != rhs.wholeSeconds {
            return lhs.wholeSeconds < rhs.wholeSeconds
        }
        return compareFractions(lhs.fractionalDigits, rhs.fractionalDigits) < 0
    }

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.wholeSeconds == rhs.wholeSeconds
            && compareFractions(lhs.fractionalDigits, rhs.fractionalDigits) == 0
    }

    private static func compareFractions(_ lhs: [UInt8], _ rhs: [UInt8]) -> Int {
        let digitCount = max(lhs.count, rhs.count)
        for index in 0..<digitCount {
            let left = index < lhs.count ? lhs[index] : 0
            let right = index < rhs.count ? rhs[index] : 0
            if left != right { return left < right ? -1 : 1 }
        }
        return 0
    }

    private static func decimal(_ bytes: [UInt8], _ range: Range<Int>) -> Int? {
        var result = 0
        for index in range {
            guard index < bytes.count, isDigit(bytes[index]) else { return nil }
            result = result * 10 + Int(bytes[index] - Character.asciiZero)
        }
        return result
    }

    private static func daysInMonth(_ month: Int, year: Int) -> Int {
        switch month {
        case 2: isLeapYear(year) ? 29 : 28
        case 4, 6, 9, 11: 30
        default: 31
        }
    }

    private static func isLeapYear(_ year: Int) -> Bool {
        year.isMultiple(of: 400) || (year.isMultiple(of: 4) && !year.isMultiple(of: 100))
    }

    private static func isDigit(_ byte: UInt8) -> Bool {
        (Character.asciiZero...Character.asciiNine).contains(byte)
    }
}

private extension Character {
    static let asciiZero: UInt8 = 0x30
    static let asciiNine: UInt8 = 0x39
    static let asciiPlus: UInt8 = 0x2B
    static let asciiHyphen: UInt8 = 0x2D
    static let asciiPeriod: UInt8 = 0x2E
    static let asciiColon: UInt8 = 0x3A
    static let asciiUppercaseT: UInt8 = 0x54
    static let asciiUppercaseZ: UInt8 = 0x5A
}
