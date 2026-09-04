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
    static let maximumSchedulingOffsetMinutes: UInt32 = 366 * 24 * 60
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
        if let constraints = CanonicalDependencyEdge.canonicalizedFlexibleConstraints(
            copy.flexibleConstraints
        ) {
            copy.flexibleConstraints = constraints
        }
        return copy
    }

    private static func wireCanonicalDate(_ value: Date) -> Date {
        CanonicalRFC3339Instant(date: value)?.dateAtMicrosecondPrecision ?? value
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
        if [value.earliestStartAt, value.deadlineAt].contains(where: { date in
            date.map { CanonicalRFC3339Instant(date: $0) == nil } == true
        }) {
            return "Canonical timestamps must fit the supported RFC 3339 range."
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
            guard Self.encodedCanonicalByteCount(recurrence)
                    <= Self.maximumRecurrenceBytes else {
                return "Recurrence data exceeds the 16 KiB limit."
            }
        }
        guard value.flexibleConstraints.supportsCanonicalAuthoringConstraints else {
            return "These advanced constraints are read-only in this version of DayWeave."
        }
        if CanonicalDependencyEdge.decode(
            fromFlexibleConstraints: value.flexibleConstraints
        )?.contains(where: { $0.predecessorID == itemID }) == true {
            return "An item cannot depend on itself."
        }
        if value.flexibleConstraints.hasNestedEarliestStart,
           value.earliestStartAt != nil {
            return "Earliest start cannot be defined as both a canonical field and a constraint."
        }
        if value.flexibleConstraints.hasNestedLatestFinish,
           value.deadlineAt != nil {
            return "Deadline cannot be defined as both a canonical field and a constraint."
        }
        if let canonicalStart = value.earliestStartAt.flatMap({
            CanonicalRFC3339Instant(date: $0)
        }),
           let constrainedFinish = value.flexibleConstraints.canonicalNestedLatestFinish,
           canonicalStart >= constrainedFinish {
            return "Canonical earliest start must precede the constrained latest finish."
        }
        if let constrainedStart = value.flexibleConstraints.canonicalNestedEarliestStart,
           let canonicalFinish = value.deadlineAt.flatMap({
               CanonicalRFC3339Instant(date: $0)
           }),
           constrainedStart >= canonicalFinish {
            return "Constrained earliest start must precede the canonical deadline."
        }
        if let preferredStart = value.flexibleConstraints.canonicalPreferredStartMinute {
            guard value.kind != .event else {
                return "Fixed events cannot use a preferred start minute."
            }
            guard let duration = value.durationSeconds else {
                return "Preferred start requires a duration estimate."
            }
            let durationMinutes = (UInt64(duration) + 59) / 60
            guard UInt64(preferredStart) + durationMinutes <= 1_440 else {
                return "Preferred start and duration must finish within the same day."
            }
        }
        if value.kind == .event,
           !value.flexibleConstraints.hasValidCanonicalAllDayBounds(in: timeZone) {
            return "All-day events must use local midnight bounds and an exclusive later end date."
        }
        guard Self.encodedCanonicalByteCount(value.flexibleConstraints)
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
            if let sessionCap = value.flexibleConstraints.canonicalMaximumSessions,
               (UInt64(duration) + UInt64(maximum) - 1) / UInt64(maximum)
                    > UInt64(sessionCap) {
                return "Maximum sessions cannot contain the duration at the maximum session size."
            }
        case .unknown:
            return "This split policy is read-only in this version of DayWeave."
        }
        if value.kind == .event, value.splitPolicy != .indivisible {
            return "Events must be indivisible."
        }
        if value.splitPolicy == .indivisible,
           value.flexibleConstraints.hasCanonicalSplitMetadata {
            return "Session caps and gaps require a splittable item."
        }
        guard value.importance <= 100, value.urgency <= 100 else {
            return "Importance and urgency must be between 0 and 100."
        }
        guard value.siblingOrder <= Self.maximumSiblingOrder else {
            return "Sibling order must be at most 1,000,000."
        }
        guard value.parentID != itemID else { return "An item cannot be its own parent." }
        switch value.kind {
        case .habit where value.status == .planned && value.recurrence == nil:
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
        if let issue = value.flexibleConstraints.kindSpecificAuthoringIssue(for: value.kind) {
            return issue
        }
        if value.kind == .event,
           value.status == .planned,
           !value.flexibleConstraints.hasCanonicalEventTiming {
            return "Events require calendar event timing metadata."
        }
        if value.kind == .event,
           !value.flexibleConstraints.hasCanonicalEventTiming,
           value.durationSeconds != nil
                || value.earliestStartAt != nil
                || value.deadlineAt != nil {
            return "Incomplete Inbox events cannot define canonical timing fields without event timing metadata."
        }
        if value.kind != .event,
           value.flexibleConstraints.hasCanonicalEventTiming {
            return "Calendar event timing metadata is only valid for event items."
        }
        if value.kind == .event,
           let mismatch = value.flexibleConstraints.eventCanonicalFieldMismatch(
            earliestStart: value.earliestStartAt,
            deadline: value.deadlineAt,
            durationSeconds: value.durationSeconds
           ) {
            return mismatch
        }
        guard value.status == .inbox || value.status == .planned else {
            return "Authored items must be either Inbox or Planned."
        }
        return nil
    }

    private static func encodedCanonicalByteCount(_ value: some Encodable) -> Int {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        return (try? encoder.encode(value).count) ?? Int.max
    }

    /// A pure, fail-closed check for the minimum canonical shape that can add
    /// work to a composed schedule. Keeping this beside draft validation lets
    /// onboarding, persistence, and tests share one definition without giving
    /// a locally queued create any canonical or publication authority.
    func createsPlanningDemand(
        itemID: UUID,
        hasActiveChildren: Bool = false
    ) -> Bool {
        guard validationIssue(itemID: itemID) == nil,
              normalized.status == .planned else { return false }
        let value = normalized
        if value.kind == .event { return true }
        let hasOwnEffort: Bool
        if case let .object(constraints) = value.flexibleConstraints {
            hasOwnEffort = constraints["has_own_effort"] == .bool(true)
        } else {
            hasOwnEffort = false
        }
        guard !hasActiveChildren || hasOwnEffort else { return false }
        guard value.durationSeconds.map({ $0 > 0 }) == true else { return false }
        switch value.kind {
        case .goal, .project, .routine:
            return hasOwnEffort
        case .task, .habit, .breakTime:
            return true
        case .event:
            return true
        case .unknown:
            return false
        }
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

extension Collection where Element == DayWeavePendingCanonicalAuthoringMutation {
    func containsPendingCanonicalChild(of parentID: UUID) -> Bool {
        contains { mutation in
            mutation.itemID != parentID
                && (mutation.operation == .create || mutation.operation == .replace)
                && mutation.draft?.parentID == parentID
        }
    }
}

extension DayWeaveCanonicalItem {
    /// Mirrors the draft-side onboarding predicate after the server has
    /// assigned canonical execution state. Goal and routine containers count
    /// only when their visible `has_own_effort` flag is explicitly true.
    var createsPlanningDemand: Bool {
        createsPlanningDemand(canonicalItems: [self])
    }

    func createsPlanningDemand(
        canonicalItems: [DayWeaveCanonicalItem],
        hasPendingChildren: Bool = false
    ) -> Bool {
        guard deletedAt == nil,
              status == .planned || status == .scheduled else { return false }
        let hasCanonicalChildren = canonicalItems.contains { child in
            child.id != id && child.parentID == id && child.deletedAt == nil
        }
        // The API's execution flag is a leaf marker, not an own-effort marker.
        // Validate that authoritative relationship, then apply core occupancy
        // semantics below instead of suppressing eligible parents wholesale.
        guard isExecutable == !hasCanonicalChildren else { return false }
        if kind == .event { return true }
        let hasOwnEffort = self.hasOwnEffort
        let hasActiveChildren = hasPendingChildren || hasCanonicalChildren
        guard !hasActiveChildren || hasOwnEffort else { return false }
        guard durationSeconds.map({ $0 > 0 }) == true else { return false }
        switch kind {
        case .goal, .project, .routine:
            return hasOwnEffort
        case .task, .habit, .breakTime:
            return true
        case .event:
            return true
        case .unknown:
            return false
        }
    }

    /// Full authoring is allowed for the typed subset whose semantic values can
    /// be reconstructed safely. Legacy background status/privacy publication
    /// intentionally keeps using the stricter lossless-replacement predicate.
    var supportsCanonicalAuthoringReplacement: Bool {
        guard unsupportedFields.isEmpty,
              !hasExplicitStructuralMetadata,
              retainedUnrepresentableDeadlineAt == nil,
              retainedUnrepresentableEarliestStartAt == nil,
              splitPolicy.isSupportedForWrite else { return false }
        if kind == .project { return false }
        if case .unknown = kind { return false }
        if case .unknown = status { return false }
        guard recurrence?.supportsCanonicalAuthoringRecurrence ?? true,
              flexibleConstraints.supportsCanonicalAuthoringConstraints else { return false }
        guard flexibleConstraints.kindSpecificAuthoringIssue(for: kind) == nil else {
            return false
        }
        if kind == .event {
            guard !flexibleConstraints.hasImportedCalendarEvent else { return false }
            guard status == .inbox || flexibleConstraints.hasCanonicalEventTiming else {
                return false
            }
            guard flexibleConstraints.hasCanonicalEventTiming
                    || (durationSeconds == nil
                        && earliestStartAt == nil
                        && deadlineAt == nil) else {
                return false
            }
        } else if flexibleConstraints.hasCanonicalEventTiming {
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
            return Set(object.keys).isSubset(of: ["type", "times_per_day"])
                && (object["times_per_day"] == nil
                    || unsigned("times_per_day", maximum: UInt32(UInt16.max)))
        case "weekly":
            return Set(object.keys).isSubset(of: ["type", "times_per_week", "weekdays"])
                && (object["times_per_week"] == nil
                    || unsigned("times_per_week", maximum: UInt32(UInt16.max)))
                && (object["weekdays"] == nil || weekdays("weekdays"))
        case "monthly":
            return Set(object.keys).isSubset(of: ["type", "times_per_month"])
                && (object["times_per_month"] == nil
                    || unsigned("times_per_month", maximum: UInt32(UInt16.max)))
        case "every_interval", "after_completion":
            return Set(object.keys) == ["type", "interval"]
                && unsigned(
                    "interval",
                    maximum: DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes
                )
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
               !unsigned(
                "minimum_spacing",
                allowZero: true,
                maximum: DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes
               ) { return false }
            if let anchor = object["anchor"] {
                switch anchor {
                case let .string(value):
                    guard CanonicalRFC3339Instant(value)?.hasPostgresPrecision == true else {
                        return false
                    }
                case .null:
                    break
                default: return false
                }
            }
            if object["semantics"] == .string("rolling"),
               case let .array(days)? = object["weekdays"],
               !days.isEmpty { return false }
            if object["semantics"] == .string("calendar"),
               object["anchor"] != nil,
               object["anchor"] != .null { return false }
            if object["semantics"] == .string("rolling"),
               let target = object["target"]?.canonicalUnsigned {
                if object["period"] == .string("day"), target > 1_440 { return false }
                if object["period"] == .string("week"), target > 10_080 { return false }
            }
            return true
        case "custom":
            // The canonical core retains custom RRULE text but does not expand
            // it into scheduling occurrences. Preserve it losslessly and keep
            // replacement read-only until an expansion adapter exists.
            return false
        default:
            return false
        }
    }

    var supportsCanonicalAuthoringConstraints: Bool {
        guard case let .object(object) = self else { return false }
        let allowed: Set<String> = [
            "constraints", "energy", "tags", "goal_ids", "has_own_effort", "habit_target",
            "preserves_streak_when_paused", "routine_ordered", "goal_measures",
            "goal_weekly_allocation", "break_category", "break_mandatory",
            "break_prompt_to_resume", "maximum_sessions", "minimum_gap_minutes",
            "maximum_split_days", "preferred_start_minute", "calendar_event",
            "calendar_context", "dayweave_firm_block",
        ]
        guard Set(object.keys).isSubset(of: allowed) else { return false }
        if let firmBlock = object["dayweave_firm_block"], firmBlock != .null,
           Set(object.keys) != ["dayweave_firm_block"] { return false }
        for (key, value) in object {
            switch key {
            case "constraints":
                guard value.supportsCanonicalSchedulingConstraints else { return false }
            case "energy":
                if value == .null { continue }
                if case let .string(level) = value {
                    guard ["low", "medium", "deep"].contains(level) else { return false }
                } else {
                    guard value.supportsCanonicalQualified(where: { inner in
                        guard case let .string(level) = inner else { return false }
                        return ["low", "medium", "deep"].contains(level)
                    }) else { return false }
                }
            case "tags":
                guard value.supportsCanonicalStringSet(allowEmptyValues: false) else {
                    return false
                }
            case "goal_ids":
                // Graph membership needs cycle/privacy authority. An explicit
                // empty server default is safe to normalize away; any actual
                // relationship remains losslessly read-only.
                guard value == .array([]) else { return false }
            case "habit_target":
                if value == .null { continue }
                guard case let .object(target) = value,
                      Set(target.keys) == ["amount", "unit"],
                      target["amount"]?.canonicalUnsigned.map({ $0 > 0 }) == true,
                      case let .string(unit)? = target["unit"],
                      DayWeaveHabitOutcomeInput.isValidUnit(unit) else { return false }
            case "goal_measures":
                guard case let .array(measures) = value else { return false }
                for measure in measures {
                    guard case let .object(fields) = measure,
                          Set(fields.keys) == ["name", "target", "current", "unit"],
                          fields["name"]?.canonicalNonemptyString == true,
                          fields["unit"]?.canonicalNonemptyString == true,
                          fields["target"]?.canonicalSigned != nil,
                          fields["current"]?.canonicalSigned != nil else { return false }
                }
            case "goal_weekly_allocation":
                if value == .null { continue }
                guard case let .object(allocation) = value,
                      Set(allocation.keys).isSubset(of: ["minimum", "maximum"]),
                      allocation["minimum"]?.canonicalUnsigned != nil else {
                    return false
                }
                if let maximum = allocation["maximum"], maximum != .null {
                    guard let maximumValue = maximum.canonicalUnsigned,
                          let minimumValue = allocation["minimum"]?.canonicalUnsigned,
                          maximumValue >= minimumValue else { return false }
                }
            case "has_own_effort", "routine_ordered", "preserves_streak_when_paused",
                 "break_mandatory", "break_prompt_to_resume":
                guard case .bool = value else { return false }
            case "break_category":
                if value == .null { continue }
                guard case let .string(category) = value,
                      ["rest", "meal", "movement", "pomodoro", "decompression", "other"]
                        .contains(category) else { return false }
            case "calendar_event":
                if value == .null { continue }
                guard value.supportsCanonicalCalendarEvent else { return false }
            case "calendar_context":
                // Provider calendar context is system-owned. Explicit null is
                // the ordinary absent Option spelling; non-null stays read-only.
                guard value == .null else { return false }
            case "dayweave_firm_block":
                if value == .null { continue }
                guard value.supportsCanonicalFirmBlock else { return false }
            case "maximum_sessions":
                if value == .null { continue }
                guard let count = value.canonicalUnsigned,
                      count > 0, count <= UInt16.max else { return false }
            case "minimum_gap_minutes":
                guard value.canonicalUnsigned.map({
                    $0 <= DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes
                }) == true else { return false }
            case "maximum_split_days":
                if value == .null { continue }
                guard let count = value.canonicalUnsigned,
                      count > 0, count <= UInt16.max else { return false }
            case "preferred_start_minute":
                if value == .null { continue }
                guard let minute = value.canonicalUnsigned, minute < 1_440 else {
                    return false
                }
            default:
                return false
            }
        }
        return true
    }

    private var supportsCanonicalSchedulingConstraints: Bool {
        guard case let .object(object) = self else { return false }
        let allowed: Set<String> = [
            "earliest_start", "latest_finish", "minimum_notice", "allowed_weekdays",
            "preferred_daily_windows", "preferred_absolute_windows", "forbidden_windows",
            "required_contexts", "required_location", "dependencies", "maximum_daily_work",
            "maximum_weekly_work", "buffers", "occurrence_window",
        ]
        guard Set(object.keys).isSubset(of: allowed) else { return false }
        for (key, value) in object {
            switch key {
            case "earliest_start", "latest_finish":
                if value == .null { continue }
                guard value.supportsCanonicalQualified(where: { candidate in
                    guard case let .string(raw) = candidate else { return false }
                    return CanonicalRFC3339Instant(raw)?.hasPostgresPrecision == true
                }) else { return false }
            case "minimum_notice", "maximum_daily_work", "maximum_weekly_work":
                if value == .null { continue }
                guard value.supportsCanonicalQualified(where: { candidate in
                    guard let minutes = candidate.canonicalUnsigned else { return false }
                    if key == "minimum_notice" {
                        return minutes
                            <= DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes
                    }
                    return true
                }) else { return false }
            case "allowed_weekdays":
                if value == .null { continue }
                guard value.supportsCanonicalQualified(where: { candidate in
                    candidate.supportsCanonicalWeekdaySet(requireNonempty: true)
                }) else { return false }
            case "preferred_daily_windows":
                guard case let .array(windows) = value else { return false }
                for window in windows {
                    guard window.supportsCanonicalQualified(where: { candidate in
                        guard case let .object(fields) = candidate,
                              Set(fields.keys) == ["weekdays", "start_minute", "end_minute"],
                              fields["weekdays"]?.supportsCanonicalWeekdaySet(
                                requireNonempty: false
                              ) == true,
                              let start = fields["start_minute"]?.canonicalUnsigned,
                              let end = fields["end_minute"]?.canonicalUnsigned else {
                            return false
                        }
                        return start < 1_440 && end <= 1_440 && start != end
                    }) else { return false }
                }
            case "preferred_absolute_windows", "forbidden_windows":
                guard case let .array(windows) = value else { return false }
                for window in windows {
                    guard window.supportsCanonicalQualified(where: { candidate in
                        candidate.supportsCanonicalAbsoluteWindow
                    }) else { return false }
                }
            case "required_contexts":
                guard case let .array(contexts) = value else { return false }
                for context in contexts {
                    guard context.supportsCanonicalQualified(where: { candidate in
                        candidate.canonicalNonemptyString
                    }) else { return false }
                }
            case "required_location":
                if value == .null { continue }
                guard value.supportsCanonicalQualified(where: { candidate in
                    candidate.canonicalNonemptyString
                }) else { return false }
            case "dependencies":
                guard CanonicalDependencyEdge.decodeArray(value) != nil else { return false }
            case "buffers":
                guard case let .object(buffer) = value,
                      Set(buffer.keys) == ["before", "after", "strength"],
                      let before = buffer["before"]?.canonicalUnsigned,
                      let after = buffer["after"]?.canonicalUnsigned,
                      before <= DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes,
                      after <= DayWeaveCanonicalItemDraft.maximumSchedulingOffsetMinutes else {
                    return false
                }
                if buffer["strength"] != .null {
                    guard buffer["strength"]?.supportsCanonicalStrength == true,
                          before > 0 || after > 0 else { return false }
                }
            case "occurrence_window":
                guard value == .null else { return false }
            default:
                return false
            }
        }
        if let start = object["earliest_start"]?.canonicalQualifiedInstant,
           let end = object["latest_finish"]?.canonicalQualifiedInstant,
           start >= end { return false }
        return true
    }

    private func supportsCanonicalQualified(
        where validateValue: (JSONValue) -> Bool
    ) -> Bool {
        guard case let .object(object) = self,
              Set(object.keys) == ["value", "strength"],
              let value = object["value"],
              validateValue(value),
              object["strength"]?.supportsCanonicalStrength == true else { return false }
        return true
    }

    private var supportsCanonicalStrength: Bool {
        guard case let .object(object) = self,
              case let .string(level)? = object["level"] else { return false }
        switch level {
        case "hard":
            return Set(object.keys) == ["level"]
        case "soft":
            return Set(object.keys) == ["level", "weight"]
                && object["weight"]?.canonicalUnsigned.map({ $0 <= 1_000_000 }) == true
        default:
            return false
        }
    }

    private func supportsCanonicalStringSet(allowEmptyValues: Bool) -> Bool {
        guard case let .array(values) = self else { return false }
        var seen = Set<String>()
        for value in values {
            guard case let .string(text) = value,
                  (allowEmptyValues
                    || !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty),
                  seen.insert(text).inserted else {
                return false
            }
        }
        return true
    }

    private func supportsCanonicalWeekdaySet(requireNonempty: Bool) -> Bool {
        guard case let .array(values) = self,
              !requireNonempty || !values.isEmpty else { return false }
        let allowed = Set([
            "monday", "tuesday", "wednesday", "thursday",
            "friday", "saturday", "sunday",
        ])
        var seen = Set<String>()
        for value in values {
            guard case let .string(day) = value,
                  allowed.contains(day), seen.insert(day).inserted else { return false }
        }
        return true
    }

    private var supportsCanonicalAbsoluteWindow: Bool {
        guard case let .object(object) = self,
              Set(object.keys) == ["start", "end"],
              case let .string(startValue)? = object["start"],
              case let .string(endValue)? = object["end"],
              let start = CanonicalRFC3339Instant(startValue),
              let end = CanonicalRFC3339Instant(endValue) else { return false }
        return start.hasPostgresPrecision && end.hasPostgresPrecision && start < end
    }

    private var canonicalUnsigned: UInt32? {
        guard case let .number(number) = self else { return nil }
        return number.exactUInt32
    }

    private var canonicalSigned: Int64? {
        guard case let .number(number) = self else { return nil }
        return Int64(number.displayDescription)
    }

    private var canonicalQualifiedInstant: CanonicalRFC3339Instant? {
        guard case let .object(object) = self,
              case let .string(raw)? = object["value"] else { return nil }
        return CanonicalRFC3339Instant(raw)
    }

    private var canonicalNonemptyString: Bool {
        guard case let .string(value) = self else { return false }
        return !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    fileprivate var hasNestedEarliestStart: Bool {
        guard case let .object(root) = self,
              case let .object(constraints)? = root["constraints"] else { return false }
        return constraints["earliest_start"] != nil
            && constraints["earliest_start"] != .null
    }

    fileprivate var hasNestedLatestFinish: Bool {
        guard case let .object(root) = self,
              case let .object(constraints)? = root["constraints"] else { return false }
        return constraints["latest_finish"] != nil
            && constraints["latest_finish"] != .null
    }

    fileprivate var canonicalNestedEarliestStart: CanonicalRFC3339Instant? {
        guard case let .object(root) = self,
              case let .object(constraints)? = root["constraints"] else { return nil }
        return constraints["earliest_start"]?.canonicalQualifiedInstant
    }

    fileprivate var canonicalNestedLatestFinish: CanonicalRFC3339Instant? {
        guard case let .object(root) = self,
              case let .object(constraints)? = root["constraints"] else { return nil }
        return constraints["latest_finish"]?.canonicalQualifiedInstant
    }

    fileprivate var hasCanonicalSplitMetadata: Bool {
        guard case let .object(root) = self else { return false }
        return (root["maximum_sessions"] != nil && root["maximum_sessions"] != .null)
            || (root["minimum_gap_minutes"]?.canonicalUnsigned ?? 0) != 0
            || (root["maximum_split_days"] != nil && root["maximum_split_days"] != .null)
    }

    fileprivate var canonicalMaximumSessions: UInt16? {
        guard case let .object(root) = self,
              let value = root["maximum_sessions"]?.canonicalUnsigned,
              value <= UInt16.max else { return nil }
        return UInt16(value)
    }

    fileprivate var canonicalPreferredStartMinute: UInt16? {
        guard case let .object(root) = self,
              let value = root["preferred_start_minute"]?.canonicalUnsigned,
              value < 1_440 else { return nil }
        return UInt16(value)
    }

    fileprivate func kindSpecificAuthoringIssue(
        for kind: DayWeaveCanonicalItemKind
    ) -> String? {
        guard case let .object(root) = self else { return nil }
        func hasAny(_ keys: [String]) -> Bool {
            keys.contains { root[$0] != nil }
        }
        if kind != .event,
           hasAny(["calendar_event", "calendar_context", "dayweave_firm_block"]) {
            return "Calendar event metadata is only valid for event items."
        }
        if kind != .habit,
           hasAny(["habit_target", "preserves_streak_when_paused"]) {
            return "Habit metadata is only valid for habit items."
        }
        if kind != .routine, root["routine_ordered"] != nil {
            return "Routine ordering is only valid for routine items."
        }
        if kind != .goal,
           hasAny(["goal_measures", "goal_weekly_allocation"]) {
            return "Goal metadata is only valid for goal items."
        }
        if kind != .breakTime,
           hasAny(["break_category", "break_mandatory", "break_prompt_to_resume"]) {
            return "Break metadata is only valid for break items."
        }
        return nil
    }

    fileprivate func eventCanonicalFieldMismatch(
        earliestStart: Date?,
        deadline: Date?,
        durationSeconds: UInt32?
    ) -> String? {
        guard let bounds = canonicalEventBounds else { return nil }
        if let earliestStart,
           Self.wireInstant(earliestStart) != bounds.start {
            return "Event earliest start must equal its timing start."
        }
        if let deadline,
           Self.wireInstant(deadline) != bounds.end {
            return "Event deadline must equal its timing end."
        }
        if let durationSeconds,
           bounds.start.exactWholeSecondInterval(to: bounds.end) != Int64(durationSeconds) {
            return "Event duration must equal its timing interval."
        }
        return nil
    }

    private var canonicalEventBounds:
        (start: CanonicalRFC3339Instant, end: CanonicalRFC3339Instant)? {
        guard case let .object(root) = self else { return nil }
        let value: [String: JSONValue]
        let startKey: String
        let endKey: String
        if case let .object(event)? = root["calendar_event"] {
            value = event
            startKey = "start"
            endKey = "end"
        } else if case let .object(event)? = root["dayweave_firm_block"] {
            value = event
            startKey = "starts_at"
            endKey = "ends_at"
        } else {
            return nil
        }
        guard case let .string(startRaw)? = value[startKey],
              case let .string(endRaw)? = value[endKey],
              let start = CanonicalRFC3339Instant(startRaw),
              let end = CanonicalRFC3339Instant(endRaw) else { return nil }
        return (start, end)
    }

    private static func wireInstant(_ date: Date) -> CanonicalRFC3339Instant? {
        CanonicalRFC3339Instant(date: date)
    }

    fileprivate var hasCanonicalEventTiming: Bool {
        guard case let .object(object) = self else { return false }
        let hasCalendarEvent = object["calendar_event"] != nil
            && object["calendar_event"] != .null
        let hasFirmBlock = object["dayweave_firm_block"] != nil
            && object["dayweave_firm_block"] != .null
        return hasCalendarEvent != hasFirmBlock
    }

    fileprivate var hasImportedCalendarEvent: Bool {
        guard case let .object(object) = self else { return false }
        return object["calendar_event"] != nil && object["calendar_event"] != .null
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
              start.hasPostgresPrecision,
              end.hasPostgresPrecision,
              start < end,
              case .bool? = object["immutable"], case .bool? = object["all_day"] else {
            return false
        }
        if let source = object["source_calendar_id"] {
            switch source {
            case let .string(value) where !value.trimmingCharacters(
                in: .whitespacesAndNewlines
            ).isEmpty:
                break
            case .null:
                break
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
              start.hasPostgresPrecision,
              end.hasPostgresPrecision,
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
struct CanonicalRFC3339Instant: Comparable {
    private static let secondsFromYearOneToUnixEpoch: Int64 = 62_135_596_800

    private let wholeSeconds: Int64
    private let fractionalDigits: [UInt8]

    var isWholeSecond: Bool { fractionalDigits.allSatisfy { $0 == 0 } }
    var hasPostgresPrecision: Bool {
        fractionalDigits.dropFirst(6).allSatisfy { $0 == 0 }
    }

    /// Matches the spelling emitted by `time::format_description::well_known::Rfc3339`.
    /// The formatter omits a zero fraction, trims fractional trailing zeroes, and uses
    /// `Z` rather than a signed zero offset.
    static func hasCanonicalTimeRFC3339Spelling(_ value: String) -> Bool {
        guard Self(value) != nil,
              !value.hasSuffix("+00:00"),
              !value.hasSuffix("-00:00") else { return false }
        let bytes = Array(value.utf8)
        if bytes.count > 19, bytes[19] == Character.asciiPeriod {
            var cursor = 20
            while cursor < bytes.count, Self.isDigit(bytes[cursor]) {
                cursor += 1
            }
            guard cursor > 20, bytes[cursor - 1] != Character.asciiZero else { return false }
        }
        return true
    }

    var dateAtWholeSecond: Date {
        Date(timeIntervalSince1970: TimeInterval(
            wholeSeconds - Self.secondsFromYearOneToUnixEpoch
        ))
    }

    var dateAtMicrosecondPrecision: Date {
        Date(timeIntervalSince1970: TimeInterval(microsecondsSinceUnixEpoch) / 1_000_000)
    }

    var exactlyRepresentableDate: Date? {
        let date = dateAtMicrosecondPrecision
        guard CanonicalRFC3339Instant(date: date)?.microsecondsSinceUnixEpoch
                == microsecondsSinceUnixEpoch else { return nil }
        return date
    }

    var microsecondsSinceUnixEpoch: Int64 {
        (wholeSeconds - Self.secondsFromYearOneToUnixEpoch) * 1_000_000
            + microsecondFraction
    }

    /// A deterministic UTC representation with PostgreSQL's full six-digit
    /// precision. Unlike `ISO8601DateFormatter`, this never truncates a Date to
    /// milliseconds.
    var canonicalUTCString: String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let parts = calendar.dateComponents(
            [.year, .month, .day, .hour, .minute, .second],
            from: dateAtWholeSecond
        )
        guard let year = parts.year,
              let month = parts.month,
              let day = parts.day,
              let hour = parts.hour,
              let minute = parts.minute,
              let second = parts.second else {
            preconditionFailure("A validated RFC 3339 instant must have Gregorian components")
        }
        var fraction = String(format: "%06lld", microsecondFraction)
        while fraction.count > 3, fraction.last == "0" { fraction.removeLast() }
        return String(
            format: "%04d-%02d-%02dT%02d:%02d:%02d.%@Z",
            year, month, day, hour, minute, second, fraction
        )
    }

    private var microsecondFraction: Int64 {
        var result: Int64 = 0
        for index in 0..<6 {
            result = result * 10 + Int64(index < fractionalDigits.count ? fractionalDigits[index] : 0)
        }
        return result
    }

    init?(date: Date) {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let calendarParts = calendar.dateComponents([.era, .year], from: date)
        guard calendarParts.era == 1,
              calendarParts.year.map({ (1...9_999).contains($0) }) == true else {
            return nil
        }
        let seconds = date.timeIntervalSince1970
        let scaled = seconds * 1_000_000
        guard seconds.isFinite,
              scaled.isFinite,
              scaled > Double(Int64.min),
              scaled < Double(Int64.max) else { return nil }
        let totalMicroseconds = Int64(scaled.rounded())
        var unixWholeSeconds = totalMicroseconds / 1_000_000
        var fraction = totalMicroseconds % 1_000_000
        if fraction < 0 {
            unixWholeSeconds -= 1
            fraction += 1_000_000
        }
        let canonicalWholeSeconds = unixWholeSeconds + Self.secondsFromYearOneToUnixEpoch
        guard canonicalWholeSeconds >= 0 else { return nil }
        wholeSeconds = canonicalWholeSeconds
        fractionalDigits = String(format: "%06lld", fraction).utf8.map {
            $0 - Character.asciiZero
        }
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
            guard cursor > fractionStart, fraction.count <= 9 else { return nil }
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
                  (0...18).contains(offsetHour),
                  (0...59).contains(offsetMinute),
                  offsetHour < 18 || offsetMinute == 0 else { return nil }
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

    func exactWholeSecondInterval(to later: Self) -> Int64? {
        guard Self.compareFractions(later.fractionalDigits, fractionalDigits) == 0 else {
            return nil
        }
        return later.wholeSeconds - wholeSeconds
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
