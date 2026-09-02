import Foundation
import SwiftUI

enum PlannerTimeZone {
    private static let utc = TimeZone(secondsFromGMT: 0)!

    static func resolve(_ timezoneName: String) -> TimeZone {
        DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: timezoneName) ?? utc
    }

    static func dateTimeLabel(_ date: Date, timezoneName: String) -> String {
        var style = Date.FormatStyle()
            .year()
            .month(.abbreviated)
            .day()
            .hour(.twoDigits(amPM: .omitted))
            .minute(.twoDigits)
            .timeZone(.iso8601(.long))
        style.timeZone = resolve(timezoneName)
        return date.formatted(style)
    }
}

enum PlannerItemKind: String, Codable, CaseIterable, Identifiable, Sendable {
    case event
    case task
    case habit
    case routine
    case goal
    case breakTime = "break"

    var id: Self { self }

    var title: String {
        switch self {
        case .event: "Event"
        case .task: "Task"
        case .habit: "Habit"
        case .routine: "Routine"
        case .goal: "Goal"
        case .breakTime: "Break"
        }
    }

    var symbol: String {
        switch self {
        case .event: "calendar"
        case .task: "checkmark.circle"
        case .habit: "repeat"
        case .routine: "list.number"
        case .goal: "scope"
        case .breakTime: "cup.and.saucer"
        }
    }

    var color: Color {
        switch self {
        case .event: .blue
        case .task: .indigo
        case .habit: .green
        case .routine: .orange
        case .goal: .purple
        case .breakTime: .mint
        }
    }
}

enum PlannerItemStatus: String, Codable, CaseIterable, Sendable {
    case notStarted
    case scheduled
    case active
    case paused
    case completed
    case skipped
    case canceled
    case blocked

    var title: String {
        switch self {
        case .notStarted: "Not started"
        case .scheduled: "Scheduled"
        case .active: "In progress"
        case .paused: "Paused"
        case .completed: "Completed"
        case .skipped: "Skipped"
        case .canceled: "Canceled"
        case .blocked: "Blocked"
        }
    }
}

enum EnergyLevel: String, Codable, CaseIterable, Identifiable, Sendable {
    case low
    case medium
    case deep

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

enum ScheduleBlockOrigin: String, Codable, Sendable {
    case local
    case canonicalPreview
    case externalPreview
    case localComposition
    case remoteExecutionLease
}

struct DayWeaveMoveDeadlineBoundary: Codable, Equatable, Hashable, Sendable {
    let date: Date
    let isHard: Bool
    let isCanonicalField: Bool

    var hasValidShape: Bool {
        dayWeavePostgresEpochMicroseconds(date) != nil
    }
}

enum DayWeaveMoveDeadlineAssessment: Equatable, Sendable {
    case valid(DayWeaveMoveDeadlineBoundary?)
    case invalid
}

extension DayWeaveCanonicalItem {
    /// Parses the scheduler's qualified latest-finish constraint without
    /// guessing through malformed or dual deadline metadata.
    var moveLaterDeadlineAssessment: DayWeaveMoveDeadlineAssessment {
        let canonical = deadlineAt.map {
            DayWeaveMoveDeadlineBoundary(date: $0, isHard: true, isCanonicalField: true)
        }
        guard case let .object(root) = flexibleConstraints else { return .invalid }
        guard let constraintsValue = root["constraints"], constraintsValue != .null else {
            return .valid(canonical)
        }
        guard case let .object(constraints) = constraintsValue else { return .invalid }
        guard let latestFinish = constraints["latest_finish"], latestFinish != .null else {
            return .valid(canonical)
        }
        guard canonical == nil,
              case let .object(qualified) = latestFinish,
              Set(qualified.keys) == ["value", "strength"],
              case let .string(rawDate)? = qualified["value"],
              rawDate.utf8.count <= 64,
              let date = RecurrenceMoveSource.parseRFC3339(rawDate),
              dayWeavePostgresEpochMicroseconds(date) != nil,
              case let .object(strength)? = qualified["strength"],
              case let .string(level)? = strength["level"] else { return .invalid }
        switch level {
        case "hard":
            guard Set(strength.keys) == ["level"] else { return .invalid }
            return .valid(.init(date: date, isHard: true, isCanonicalField: false))
        case "soft":
            guard Set(strength.keys) == ["level", "weight"],
                  case let .number(weight)? = strength["weight"],
                  let exactWeight = weight.exactUInt32,
                  exactWeight <= 1_000_000 else { return .invalid }
            return .valid(.init(date: date, isHard: false, isCanonicalField: false))
        default:
            return .invalid
        }
    }
}

struct ScheduleBlock: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    /// Effective sensitivity after canonical ancestor propagation.
    var isSensitive: Bool = false
    var title: String
    var kind: PlannerItemKind
    var start: Date
    var end: Date
    var status: PlannerItemStatus
    var project: String?
    var notes: String
    var energy: EnergyLevel
    var isFlexible: Bool
    var isHardConstraint: Bool
    var actualMinutes: Int?
    var sourceItemID: UUID? = nil
    var sourceItemRevision: UInt64? = nil
    var occurrenceID: UUID? = nil
    /// Exact source identity for a published non-item fixed constraint.
    var externalBlockID: UUID? = nil
    /// Root canonical item that owns this occurrence. A recurring hierarchy
    /// can emit executable descendant leaves with a different source item.
    var recurrenceSeriesItemID: UUID? = nil
    var sessionIndex: UInt16? = nil
    var syncOrigin: ScheduleBlockOrigin? = nil
    var placementReason: String? = nil
    /// The scheduler's wire kind (for example `planned`, `pinned`, or
    /// `calendar_event`). Keeping it avoids guessing stability eligibility.
    var previewKind: String? = nil
    /// False when the server reports remaining work for this occurrence.
    /// A partial preview must never advance a recurrence completion anchor.
    var occurrenceFullyScheduled: Bool = true
    /// Exact server-issued source envelope used to restore an occurrence after
    /// its nominal planning horizon has rolled away. Optional for backwards
    /// compatibility; occurrence moves fail closed when it is unavailable.
    var recurrenceMoveSource: RecurrenceMoveSource? = nil

    var durationMinutes: Int {
        max(1, Int(end.timeIntervalSince(start) / 60))
    }

    func timeRange(timezoneName: String) -> String {
        "\(startTimeLabel(timezoneName: timezoneName))–\(endTimeLabel(timezoneName: timezoneName))"
    }

    func startTimeLabel(timezoneName: String) -> String {
        Self.offsetTimeLabel(for: start, timezoneName: timezoneName)
    }

    func endTimeLabel(timezoneName: String) -> String {
        Self.offsetTimeLabel(for: end, timezoneName: timezoneName)
    }

    var isLocallyAuthored: Bool {
        syncOrigin == nil || syncOrigin == .local
    }

    /// External fixed inputs constrain composition, but they are not executable
    /// DayWeave work and must not contribute to project or execution rollups.
    var contributesToExecutionPresentation: Bool {
        previewKind != "external_fixed"
    }

    var isExternalFixedBlock: Bool {
        !contributesToExecutionPresentation
    }

    private static func offsetTimeLabel(
        for date: Date,
        timezoneName: String
    ) -> String {
        // The numeric offset disambiguates the repeated local hour during a
        // DST fall-back even when a zone happens to reuse an abbreviation.
        var style = Date.FormatStyle()
            .hour(.twoDigits(amPM: .omitted))
            .minute(.twoDigits)
            .timeZone(.iso8601(.long))
        style.timeZone = PlannerTimeZone.resolve(timezoneName)
        return date.formatted(style)
    }
}

/// The deterministic, rule-specific identity used by the scheduler to derive
/// an occurrence UUID. String-backed dates and anchors are retained exactly so
/// a later cross-horizon move can prove the same source occurrence without a
/// lossy Foundation round trip.
enum RecurrenceOccurrenceIdentity: Hashable, Codable, Sendable {
    case calendarDay(date: String, bucketOrdinal: UInt16)
    case calendarWeek(weekKey: Int32, bucketOrdinal: UInt16)
    case calendarMonth(year: Int32, month: UInt8, bucketOrdinal: UInt16)
    case rollingMinutes(index: Int64, anchor: String)
    case afterCompletion(anchor: String)
    case rollingMonth(cycle: Int64, index: UInt16, anchor: String)
    case custom

    private enum CodingKeys: String, CodingKey {
        case type, date, year, month, index, anchor, cycle
        case bucketOrdinal = "bucket_ordinal"
        case weekKey = "week_key"
    }

    private enum Kind: String {
        case calendarDay = "calendar_day"
        case calendarWeek = "calendar_week"
        case calendarMonth = "calendar_month"
        case rollingMinutes = "rolling_minutes"
        case afterCompletion = "after_completion"
        case rollingMonth = "rolling_month"
        case custom
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: RecurrenceIdentityCodingKey.self)
        let typeKey = RecurrenceIdentityCodingKey(stringValue: CodingKeys.type.rawValue)!
        guard dynamic.contains(typeKey) else {
            throw DecodingError.keyNotFound(
                CodingKeys.type,
                .init(
                    codingPath: decoder.codingPath,
                    debugDescription: "A recurrence identity requires a type."
                )
            )
        }
        let rawType = try dynamic.decode(String.self, forKey: typeKey)
        guard let kind = Kind(rawValue: rawType) else {
            throw DecodingError.dataCorrupted(
                .init(
                    codingPath: decoder.codingPath + [CodingKeys.type],
                    debugDescription: "Unknown recurrence occurrence identity type."
                )
            )
        }
        let keys = Set(dynamic.allKeys.map(\.stringValue))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let decoded: Self
        switch kind {
        case .calendarDay:
            try Self.requireExactKeys(
                keys,
                ["type", "date", "bucket_ordinal"],
                decoder: decoder
            )
            decoded = .calendarDay(
                date: try container.decode(String.self, forKey: .date),
                bucketOrdinal: try container.decode(UInt16.self, forKey: .bucketOrdinal)
            )
        case .calendarWeek:
            try Self.requireExactKeys(
                keys,
                ["type", "week_key", "bucket_ordinal"],
                decoder: decoder
            )
            decoded = .calendarWeek(
                weekKey: try container.decode(Int32.self, forKey: .weekKey),
                bucketOrdinal: try container.decode(UInt16.self, forKey: .bucketOrdinal)
            )
        case .calendarMonth:
            try Self.requireExactKeys(
                keys,
                ["type", "year", "month", "bucket_ordinal"],
                decoder: decoder
            )
            decoded = .calendarMonth(
                year: try container.decode(Int32.self, forKey: .year),
                month: try container.decode(UInt8.self, forKey: .month),
                bucketOrdinal: try container.decode(UInt16.self, forKey: .bucketOrdinal)
            )
        case .rollingMinutes:
            try Self.requireExactKeys(
                keys,
                ["type", "index", "anchor"],
                decoder: decoder
            )
            decoded = .rollingMinutes(
                index: try container.decode(Int64.self, forKey: .index),
                anchor: try container.decode(String.self, forKey: .anchor)
            )
        case .afterCompletion:
            try Self.requireExactKeys(keys, ["type", "anchor"], decoder: decoder)
            decoded = .afterCompletion(
                anchor: try container.decode(String.self, forKey: .anchor)
            )
        case .rollingMonth:
            try Self.requireExactKeys(
                keys,
                ["type", "cycle", "index", "anchor"],
                decoder: decoder
            )
            decoded = .rollingMonth(
                cycle: try container.decode(Int64.self, forKey: .cycle),
                index: try container.decode(UInt16.self, forKey: .index),
                anchor: try container.decode(String.self, forKey: .anchor)
            )
        case .custom:
            try Self.requireExactKeys(keys, ["type"], decoder: decoder)
            decoded = .custom
        }
        guard decoded.hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(
                    codingPath: decoder.codingPath,
                    debugDescription: "Malformed recurrence occurrence identity."
                )
            )
        }
        self = decoded
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .calendarDay(date, bucketOrdinal):
            try container.encode(Kind.calendarDay.rawValue, forKey: .type)
            try container.encode(date, forKey: .date)
            try container.encode(bucketOrdinal, forKey: .bucketOrdinal)
        case let .calendarWeek(weekKey, bucketOrdinal):
            try container.encode(Kind.calendarWeek.rawValue, forKey: .type)
            try container.encode(weekKey, forKey: .weekKey)
            try container.encode(bucketOrdinal, forKey: .bucketOrdinal)
        case let .calendarMonth(year, month, bucketOrdinal):
            try container.encode(Kind.calendarMonth.rawValue, forKey: .type)
            try container.encode(year, forKey: .year)
            try container.encode(month, forKey: .month)
            try container.encode(bucketOrdinal, forKey: .bucketOrdinal)
        case let .rollingMinutes(index, anchor):
            try container.encode(Kind.rollingMinutes.rawValue, forKey: .type)
            try container.encode(index, forKey: .index)
            try container.encode(anchor, forKey: .anchor)
        case let .afterCompletion(anchor):
            try container.encode(Kind.afterCompletion.rawValue, forKey: .type)
            try container.encode(anchor, forKey: .anchor)
        case let .rollingMonth(cycle, index, anchor):
            try container.encode(Kind.rollingMonth.rawValue, forKey: .type)
            try container.encode(cycle, forKey: .cycle)
            try container.encode(index, forKey: .index)
            try container.encode(anchor, forKey: .anchor)
        case .custom:
            try container.encode(Kind.custom.rawValue, forKey: .type)
        }
    }

    var hasValidShape: Bool {
        switch self {
        case let .calendarDay(date, _):
            RecurrenceMoveSource.isValidLocalDate(date)
        case .calendarWeek:
            true
        case let .calendarMonth(year, month, _):
            (1...9_999).contains(year) && (1...12).contains(month)
        case let .rollingMinutes(index, anchor):
            index >= 0
                && UInt32(exactly: index) != nil
                && anchor.utf8.count <= 64
                && RecurrenceMoveSource.parseRFC3339(anchor) != nil
        case let .afterCompletion(anchor):
            anchor.utf8.count <= 64 && RecurrenceMoveSource.parseRFC3339(anchor) != nil
        case let .rollingMonth(cycle, _, anchor):
            cycle >= 0
                && cycle <= Int64(Int32.max)
                && anchor.utf8.count <= 64
                && RecurrenceMoveSource.parseRFC3339(anchor) != nil
        case .custom:
            true
        }
    }

    var stableOrdinal: UInt32? {
        switch self {
        case let .calendarDay(_, bucketOrdinal),
             let .calendarWeek(_, bucketOrdinal),
             let .calendarMonth(_, _, bucketOrdinal):
            UInt32(bucketOrdinal)
        case let .rollingMinutes(index, _):
            UInt32(exactly: index)
        case .afterCompletion, .custom:
            0
        case let .rollingMonth(_, index, _):
            UInt32(index)
        }
    }

    var expectsLocalDate: Bool {
        switch self {
        case .calendarDay, .calendarWeek, .calendarMonth:
            true
        case .rollingMinutes, .afterCompletion, .rollingMonth, .custom:
            false
        }
    }

    var jsonValue: JSONValue {
        switch self {
        case let .calendarDay(date, bucketOrdinal):
            .object([
                "type": .string(Kind.calendarDay.rawValue),
                "date": .string(date),
                "bucket_ordinal": .number(.init(UInt64(bucketOrdinal))),
            ])
        case let .calendarWeek(weekKey, bucketOrdinal):
            .object([
                "type": .string(Kind.calendarWeek.rawValue),
                "week_key": .number(.init(integerLiteral: Int64(weekKey))),
                "bucket_ordinal": .number(.init(UInt64(bucketOrdinal))),
            ])
        case let .calendarMonth(year, month, bucketOrdinal):
            .object([
                "type": .string(Kind.calendarMonth.rawValue),
                "year": .number(.init(integerLiteral: Int64(year))),
                "month": .number(.init(UInt64(month))),
                "bucket_ordinal": .number(.init(UInt64(bucketOrdinal))),
            ])
        case let .rollingMinutes(index, anchor):
            .object([
                "type": .string(Kind.rollingMinutes.rawValue),
                "index": .number(.init(integerLiteral: index)),
                "anchor": .string(anchor),
            ])
        case let .afterCompletion(anchor):
            .object([
                "type": .string(Kind.afterCompletion.rawValue),
                "anchor": .string(anchor),
            ])
        case let .rollingMonth(cycle, index, anchor):
            .object([
                "type": .string(Kind.rollingMonth.rawValue),
                "cycle": .number(.init(integerLiteral: cycle)),
                "index": .number(.init(UInt64(index))),
                "anchor": .string(anchor),
            ])
        case .custom:
            .object(["type": .string(Kind.custom.rawValue)])
        }
    }

    private static func requireExactKeys(
        _ actual: Set<String>,
        _ expected: Set<String>,
        decoder: any Decoder
    ) throws {
        guard actual == expected else {
            throw DecodingError.dataCorrupted(
                .init(
                    codingPath: decoder.codingPath,
                    debugDescription: "A recurrence identity has missing or unknown fields."
                )
            )
        }
    }
}

private struct RecurrenceIdentityCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil

    init?(stringValue: String) {
        self.stringValue = stringValue
    }

    init?(intValue: Int) {
        return nil
    }
}

/// Server-issued identity metadata for one generated recurrence occurrence.
/// RFC 3339 timestamps are retained byte-for-byte so emitting a later move
/// cannot silently reduce their precision through Foundation `Date`.
struct RecurrenceMoveSource: Hashable, Codable, Sendable {
    let itemRevision: UInt64
    let identity: RecurrenceOccurrenceIdentity
    let nominalStart: String
    let nominalEnd: String
    let localDate: String?
    let ordinal: UInt32

    var hasValidShape: Bool {
        guard itemRevision > 0,
              identity.hasValidShape,
              identity.stableOrdinal == ordinal,
              nominalStart.utf8.count <= 64,
              nominalEnd.utf8.count <= 64,
              let start = Self.parseRFC3339(nominalStart),
              let end = Self.parseRFC3339(nominalEnd),
              start < end else { return false }
        guard identity.expectsLocalDate else { return localDate == nil }
        guard let localDate else { return false }
        if case let .calendarDay(date, _) = identity, localDate != date {
            return false
        }
        return Self.isValidLocalDate(localDate) && nominalStart.hasPrefix(localDate + "T")
    }

    /// Custom RFC 5545 occurrences currently share a placeholder identity, so
    /// it is safe to display them but not to authorize a per-instance move.
    var canAuthorizeOccurrenceMove: Bool {
        guard hasValidShape else { return false }
        if case .custom = identity { return false }
        return true
    }

    static func parseRFC3339(_ value: String) -> Date? {
        let bytes = Array(value.utf8)
        guard bytes.count >= 20,
              bytes[4] == 45,
              bytes[7] == 45,
              bytes[10] == 84,
              bytes[13] == 58,
              bytes[16] == 58,
              let year = decimal(bytes, 0..<4),
              let month = decimal(bytes, 5..<7),
              let day = decimal(bytes, 8..<10),
              let hour = decimal(bytes, 11..<13),
              let minute = decimal(bytes, 14..<16),
              let second = decimal(bytes, 17..<19),
              year > 0,
              isValidLocalDate(String(format: "%04d-%02d-%02d", year, month, day)),
              (0...23).contains(hour),
              (0...59).contains(minute),
              (0...59).contains(second) else { return nil }
        var cursor = 19
        if cursor < bytes.count, bytes[cursor] == 46 {
            cursor += 1
            let fractionStart = cursor
            while cursor < bytes.count, (48...57).contains(bytes[cursor]) {
                cursor += 1
            }
            guard (1...9).contains(cursor - fractionStart) else { return nil }
        }
        if cursor + 1 == bytes.count, bytes[cursor] == 90 {
            // UTC (`Z`).
        } else {
            guard cursor + 6 == bytes.count,
                  bytes[cursor] == 43 || bytes[cursor] == 45,
                  bytes[cursor + 3] == 58,
                  let offsetHour = decimal(bytes, (cursor + 1)..<(cursor + 3)),
                  let offsetMinute = decimal(bytes, (cursor + 4)..<(cursor + 6)),
                  (0...23).contains(offsetHour),
                  (0...59).contains(offsetMinute) else { return nil }
        }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) { return date }
        let whole = ISO8601DateFormatter()
        whole.formatOptions = [.withInternetDateTime]
        return whole.date(from: value)
    }

    private static func decimal(_ bytes: [UInt8], _ range: Range<Int>) -> Int? {
        guard range.lowerBound >= 0, range.upperBound <= bytes.count else { return nil }
        var value = 0
        for byte in bytes[range] {
            guard (48...57).contains(byte) else { return nil }
            value = value * 10 + Int(byte - 48)
        }
        return value
    }

    static func isValidLocalDate(_ value: String) -> Bool {
        let bytes = Array(value.utf8)
        guard bytes.count == 10,
              bytes[4] == 45,
              bytes[7] == 45,
              let year = Int(String(decoding: bytes[0..<4], as: UTF8.self)),
              let month = Int(String(decoding: bytes[5..<7], as: UTF8.self)),
              let day = Int(String(decoding: bytes[8..<10], as: UTF8.self)) else {
            return false
        }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        guard let date = calendar.date(from: DateComponents(
            calendar: calendar,
            timeZone: calendar.timeZone,
            year: year,
            month: month,
            day: day
        )) else { return false }
        let components = calendar.dateComponents([.year, .month, .day], from: date)
        return components.year == year && components.month == month && components.day == day
    }
}

extension ScheduleBlock {
    private enum CodingKeys: String, CodingKey {
        case id, title, kind, start, end, status, project, notes, energy
        case isSensitive
        case isFlexible, isHardConstraint, actualMinutes
        case sourceItemID, sourceItemRevision, occurrenceID, externalBlockID
        case recurrenceSeriesItemID, sessionIndex
        case syncOrigin, placementReason, previewKind, occurrenceFullyScheduled
        case recurrenceMoveSource
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        if decoder.userInfo[.dayWeaveAllowsMissingSensitivity] as? Bool == true {
            isSensitive = try container.decodeIfPresent(Bool.self, forKey: .isSensitive) ?? false
        } else {
            isSensitive = try container.decode(Bool.self, forKey: .isSensitive)
        }
        title = try container.decode(String.self, forKey: .title)
        kind = try container.decode(PlannerItemKind.self, forKey: .kind)
        start = try container.decode(Date.self, forKey: .start)
        end = try container.decode(Date.self, forKey: .end)
        status = try container.decode(PlannerItemStatus.self, forKey: .status)
        project = try container.decodeIfPresent(String.self, forKey: .project)
        notes = try container.decode(String.self, forKey: .notes)
        energy = try container.decode(EnergyLevel.self, forKey: .energy)
        isFlexible = try container.decode(Bool.self, forKey: .isFlexible)
        isHardConstraint = try container.decode(Bool.self, forKey: .isHardConstraint)
        actualMinutes = try container.decodeIfPresent(Int.self, forKey: .actualMinutes)
        sourceItemID = try container.decodeIfPresent(UUID.self, forKey: .sourceItemID)
        sourceItemRevision = try container.decodeIfPresent(UInt64.self, forKey: .sourceItemRevision)
        occurrenceID = try container.decodeIfPresent(UUID.self, forKey: .occurrenceID)
        externalBlockID = try container.decodeIfPresent(UUID.self, forKey: .externalBlockID)
        recurrenceSeriesItemID = try container.decodeIfPresent(
            UUID.self,
            forKey: .recurrenceSeriesItemID
        )
        sessionIndex = try container.decodeIfPresent(UInt16.self, forKey: .sessionIndex)
        syncOrigin = try container.decodeIfPresent(ScheduleBlockOrigin.self, forKey: .syncOrigin)
        placementReason = try container.decodeIfPresent(String.self, forKey: .placementReason)
        previewKind = try container.decodeIfPresent(String.self, forKey: .previewKind)
        // Schema 1 predates this recurrence safety marker. Defaulting to true
        // preserves the old block shape so migration can reset terminal
        // recurrence state before it is exposed.
        occurrenceFullyScheduled = try container.decodeIfPresent(
            Bool.self,
            forKey: .occurrenceFullyScheduled
        ) ?? true
        recurrenceMoveSource = try container.decodeIfPresent(
            RecurrenceMoveSource.self,
            forKey: .recurrenceMoveSource
        )
    }
}

enum CanonicalMutationDisposition: String, Codable, Hashable, Sendable {
    case pending
    case conflicted
}

struct PendingCanonicalMutation: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    let itemID: UUID
    let occurrenceID: UUID?
    let sessionIndex: UInt16?
    var desiredStatus: PlannerItemStatus
    var baseRevision: UInt64
    let createdAt: Date
    var disposition: CanonicalMutationDisposition
    var diagnostic: String?
    /// Links an approval-gated canonical status projection to the immutable
    /// execution outcome that requested it. Older snapshots decode this as nil.
    var executionSessionID: UUID? = nil
}

/// Durable, revision-bound intent to change only an item's own privacy marker.
/// Inherited sensitivity is derived from the canonical hierarchy and is never
/// overridden by a child edit.
struct PendingCanonicalSensitivityMutation: Identifiable, Hashable, Codable, Sendable {
    let id: UUID
    let itemID: UUID
    var desiredIsSensitive: Bool
    var baseRevision: UInt64
    let createdAt: Date
    var disposition: CanonicalMutationDisposition
    var diagnostic: String?
    /// Set and durably flushed before the first request byte is sent. Once
    /// submitted, the current replacement cannot be canceled or inverted
    /// until its exact outcome is reconciled.
    var hasBeenSubmitted: Bool = false
    /// A user-requested final classification that must run only after the
    /// submitted replacement above is observed or replayed exactly.
    var followUpIsSensitive: Bool? = nil

    var requestedIsSensitive: Bool {
        followUpIsSensitive ?? desiredIsSensitive
    }

    /// Privacy is a one-way presentation fence across an ambiguous chain. If
    /// either the submitted replacement or its queued final replacement marks
    /// the item sensitive, content must remain redacted until both are
    /// authoritatively reconciled.
    var requiresSensitivePresentation: Bool {
        desiredIsSensitive || followUpIsSensitive == true
    }
}

extension PendingCanonicalSensitivityMutation {
    private enum CodingKeys: String, CodingKey {
        case id, itemID, desiredIsSensitive, baseRevision, createdAt
        case disposition, diagnostic, hasBeenSubmitted, followUpIsSensitive
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        itemID = try container.decode(UUID.self, forKey: .itemID)
        desiredIsSensitive = try container.decode(Bool.self, forKey: .desiredIsSensitive)
        baseRevision = try container.decode(UInt64.self, forKey: .baseRevision)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        disposition = try container.decode(CanonicalMutationDisposition.self, forKey: .disposition)
        diagnostic = try container.decodeIfPresent(String.self, forKey: .diagnostic)
        // A pre-fence schema cannot prove that a retained request was never
        // sent. Missing attempt state is therefore ambiguous, never cancelable.
        hasBeenSubmitted = try container.decodeIfPresent(
            Bool.self,
            forKey: .hasBeenSubmitted
        ) ?? true
        followUpIsSensitive = try container.decodeIfPresent(
            Bool.self,
            forKey: .followUpIsSensitive
        )
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(itemID, forKey: .itemID)
        try container.encode(desiredIsSensitive, forKey: .desiredIsSensitive)
        try container.encode(baseRevision, forKey: .baseRevision)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(disposition, forKey: .disposition)
        try container.encodeIfPresent(diagnostic, forKey: .diagnostic)
        try container.encode(hasBeenSubmitted, forKey: .hasBeenSubmitted)
        try container.encodeIfPresent(followUpIsSensitive, forKey: .followUpIsSensitive)
    }
}

enum RecurrenceSessionDisposition: String, Codable, Hashable, Sendable {
    case completed
    case skipped
}

enum DayWeaveMoveDeadlinePolicy {
    /// Returns every canonical deadline fact that governs the move. Scheduled
    /// recurrence moves cover every executable leaf in the occurrence (plus
    /// its series root when distinct); an execution Defer covers only the
    /// focused executable leaf.
    static func identities(
        for focused: ScheduleBlock,
        movingWholeOccurrence: Bool,
        allBlocks: [ScheduleBlock],
        canonicalItems: [DayWeaveCanonicalItem]
    ) -> Set<DayWeaveMoveDeadlineIdentity>? {
        let itemByID = Dictionary(
            canonicalItems.map { ($0.id, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        var expectedRevisions: [UUID: UInt64] = [:]
        func record(_ itemID: UUID?, revision: UInt64?) -> Bool {
            guard let itemID, let revision,
                  expectedRevisions[itemID].map({ $0 == revision }) ?? true
            else { return false }
            expectedRevisions[itemID] = revision
            return true
        }

        if movingWholeOccurrence, let occurrenceID = focused.occurrenceID {
            let siblings = allBlocks.filter { $0.occurrenceID == occurrenceID }
            guard !siblings.isEmpty,
                  siblings.allSatisfy({
                      record($0.sourceItemID, revision: $0.sourceItemRevision)
                  }) else { return nil }
            if let seriesItemID = focused.recurrenceSeriesItemID,
               !record(seriesItemID, revision: focused.recurrenceMoveSource?.itemRevision) {
                return nil
            }
        } else if !record(
            focused.sourceItemID,
            revision: focused.sourceItemRevision
        ) {
            return nil
        }

        var result = Set<DayWeaveMoveDeadlineIdentity>()
        for (itemID, revision) in expectedRevisions {
            guard let item = itemByID[itemID], item.revision == revision else { return nil }
            switch item.moveLaterDeadlineAssessment {
            case .invalid:
                return nil
            case .valid(nil):
                continue
            case let .valid(.some(boundary)):
                let identity = DayWeaveMoveDeadlineIdentity(
                    itemID: itemID,
                    itemRevision: revision,
                    boundary: boundary
                )
                guard identity.hasValidShape else { return nil }
                result.insert(identity)
            }
        }
        return result
    }
}

struct RecurrenceSessionOutcome: Hashable, Codable, Sendable {
    let itemID: UUID
    let occurrenceID: UUID
    let sessionIndex: UInt16
    var disposition: RecurrenceSessionDisposition
    var occurredAt: Date
    var occurrenceFullyScheduled: Bool
}

/// Encrypted, occurrence-scoped scheduling intent. The scheduler's `move`
/// exception consumes the shifted outer window while retaining the occurrence
/// identity. The scheduler recomposes descendant/split sessions inside it.
struct RecurrenceOccurrenceMove: Hashable, Codable, Sendable {
    static let maximumStoredCount = 3_000

    let itemID: UUID
    let occurrenceID: UUID
    let startAt: Date
    let endAt: Date
    let movedAt: Date
    let source: RecurrenceMoveSource?

    var hasValidShape: Bool {
        let nilID = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
        return itemID != nilID
            && occurrenceID != nilID
            && (occurrenceID.uuid.6 >> 4) == 5
            && startAt.timeIntervalSinceReferenceDate.isFinite
            && endAt.timeIntervalSinceReferenceDate.isFinite
            && movedAt.timeIntervalSinceReferenceDate.isFinite
            && startAt < endAt
            && source?.canAuthorizeOccurrenceMove == true
    }

    static func collectionIsValid(
        _ moves: [Self],
        canonicalItemIDs: Set<UUID>
    ) -> Bool {
        moves.count <= maximumStoredCount
            && Set(moves.map(\.occurrenceID)).count == moves.count
            && moves.allSatisfy {
                $0.hasValidShape && canonicalItemIDs.contains($0.itemID)
            }
    }
}

struct SchedulePreviewProvenance: Equatable, Codable, Sendable {
    let configurationIdentifier: String
    let generatedAt: Date
    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
}

/// Encrypted, content-free identity for the item explicitly created during
/// onboarding. A nil revision means only the exact local create is retained;
/// it cannot prove that a canonical item or a published first plan exists.
struct DayWeaveOnboardingFirstItemAnchor: Equatable, Codable, Sendable {
    let itemID: UUID
    let canonicalRevision: UInt64?

    var hasValidShape: Bool {
        itemID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && canonicalRevision.map { $0 > 0 } != false
    }

    /// Core exact-item proof used in addition to the caller's full publication
    /// provenance/freshness checks. Any pending authoring operation, including
    /// a submitted create, fails closed.
    func hasExactPublishedPlanProof(
        canonicalItems: [DayWeaveCanonicalItem],
        pendingAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation],
        publishedScheduleProof: DayWeavePublishedScheduleProof?
    ) -> Bool {
        guard hasValidShape,
              let canonicalRevision,
              !pendingAuthoringMutations.contains(where: { $0.itemID == itemID }),
              canonicalItems.contains(where: {
                  $0.id == itemID
                      && $0.revision == canonicalRevision
                      && $0.deletedAt == nil
                      && $0.isExecutable
              }),
              let publishedScheduleProof,
              publishedScheduleProof.hasCurrentImmutablePlanSeal else { return false }
        return publishedScheduleProof.publishedBlocks.contains {
            $0.itemID == itemID && $0.itemRevision == canonicalRevision
        }
    }
}

/// Evidence for a schedule composed by the signed helper on this Mac. This is
/// intentionally disjoint from `SchedulePreviewProvenance`: a local
/// fingerprint is not a server `input_digest` and cannot authorize schedule
/// publication.
struct LocalScheduleCompositionProvenance: Equatable, Codable, Sendable {
    let configurationIdentifier: String
    let localInputFingerprint: String
    let generatedAt: Date
    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
    let sourceItemRevisions: [UUID: UInt64]

    var hasValidShape: Bool {
        let prefix = "local-sha256:"
        let digest = localInputFingerprint.dropFirst(prefix.count)
        return localInputFingerprint.hasPrefix(prefix)
            && digest.count == 64
            && digest.utf8.allSatisfy {
                (48...57).contains($0) || (97...102).contains($0)
            }
            && !configurationIdentifier.isEmpty
            && configurationIdentifier.utf8.count <= 4_096
            && !configurationIdentifier.unicodeScalars.contains(
                where: CharacterSet.controlCharacters.contains
            )
            && generatedAt.timeIntervalSinceReferenceDate.isFinite
            && asOf.timeIntervalSinceReferenceDate.isFinite
            && horizonStart.timeIntervalSinceReferenceDate.isFinite
            && horizonEnd.timeIntervalSinceReferenceDate.isFinite
            && horizonStart < horizonEnd
            && TimeZone(identifier: timezoneName) != nil
            && sourceItemRevisions.count <= 10_000
            && sourceItemRevisions.values.allSatisfy { $0 > 0 }
    }
}

enum SidebarDestination: String, Codable, CaseIterable, Identifiable, Sendable {
    case today
    case calendar
    case inbox
    case habits
    case projects
    case goals
    case statistics

    var id: Self { self }

    var title: String {
        switch self {
        case .today: "Today"
        case .calendar: "Calendar"
        case .inbox: "Inbox"
        case .habits: "Habits"
        case .projects: "Projects"
        case .goals: "Goals"
        case .statistics: "Statistics"
        }
    }

    var symbol: String {
        switch self {
        case .today: "sun.max"
        case .calendar: "calendar"
        case .inbox: "tray"
        case .habits: "repeat.circle"
        case .projects: "folder"
        case .goals: "scope"
        case .statistics: "chart.bar"
        }
    }
}

struct AssistantMessage: Identifiable, Hashable, Codable, Sendable {
    enum Role: String, Codable, Sendable { case user, assistant }

    let id: UUID
    let role: Role
    let text: String
    let createdAt: Date
}

struct PlanningSuggestionItemDraft: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumEncodedBytes = 64 * 1_024

    let version: Int
    let itemID: UUID
    let draft: DayWeaveCanonicalItemDraft

    init(
        version: Int = Self.currentVersion,
        itemID: UUID,
        draft: DayWeaveCanonicalItemDraft
    ) {
        self.version = version
        self.itemID = itemID
        self.draft = draft.normalized
    }

    var hasValidShape: Bool {
        guard version == Self.currentVersion,
              draft == draft.normalized,
              draft.isSensitive,
              draft.status == .inbox,
              draft.parentID == nil,
              draft.siblingOrder == 0,
              draft.validationIssue(itemID: itemID) == nil,
              draft.deadlineAt?.timeIntervalSinceReferenceDate.isFinite != false,
              draft.earliestStartAt?.timeIntervalSinceReferenceDate.isFinite != false,
              !PlanningSuggestion.hasUnsafeText(draft.title, allowingLayoutControls: false),
              !PlanningSuggestion.hasUnsafeText(draft.notes ?? "", allowingLayoutControls: true),
              let encoded = try? JSONEncoder().encode(self),
              encoded.count <= Self.maximumEncodedBytes else {
            return false
        }
        return true
    }
}

enum PlanningSuggestionPayload: Codable, Equatable, Sendable {
    case advisory
    case canonicalItemDraft(PlanningSuggestionItemDraft)
    case canonicalItemReference(itemID: UUID)

    private enum Kind: String, Codable {
        case advisory
        case canonicalItemDraft = "canonical_item_draft"
        case canonicalItemReference = "canonical_item_reference"
    }

    private enum CodingKeys: String, CodingKey {
        case type
        case itemDraft = "item_draft"
        case itemID = "item_id"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .type) {
        case .advisory:
            guard container.allKeys == [.type] else {
                throw DecodingError.dataCorruptedError(
                    forKey: .type,
                    in: container,
                    debugDescription: "Advisory suggestion payload has unexpected fields."
                )
            }
            self = .advisory
        case .canonicalItemDraft:
            guard Set(container.allKeys) == [.type, .itemDraft] else {
                throw DecodingError.dataCorruptedError(
                    forKey: .type,
                    in: container,
                    debugDescription: "Canonical draft suggestion payload has unexpected fields."
                )
            }
            self = .canonicalItemDraft(
                try container.decode(PlanningSuggestionItemDraft.self, forKey: .itemDraft)
            )
        case .canonicalItemReference:
            guard Set(container.allKeys) == [.type, .itemID] else {
                throw DecodingError.dataCorruptedError(
                    forKey: .type,
                    in: container,
                    debugDescription: "Canonical reference suggestion payload has unexpected fields."
                )
            }
            self = .canonicalItemReference(
                itemID: try container.decode(UUID.self, forKey: .itemID)
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .advisory:
            try container.encode(Kind.advisory, forKey: .type)
        case let .canonicalItemDraft(itemDraft):
            try container.encode(Kind.canonicalItemDraft, forKey: .type)
            try container.encode(itemDraft, forKey: .itemDraft)
        case let .canonicalItemReference(itemID):
            try container.encode(Kind.canonicalItemReference, forKey: .type)
            try container.encode(itemID, forKey: .itemID)
        }
    }
}

struct PlanningSuggestion: Identifiable, Equatable, Codable, Sendable {
    static let codexSource = "Codex · requires approval"
    static let maximumStoredCount = 500
    static let maximumSummaryUTF8Bytes = 16 * 1_024
    static let maximumAggregateEncodedBytes = 4 * 1_048_576
    static let scrubbedCanonicalTitle = "Codex item draft"

    enum State: String, Codable, Sendable {
        case pending, accepted, rejected, expired
    }

    let id: UUID
    var title: String
    var summary: String
    var source: String
    var createdAt: Date
    var expiresAt: Date
    var state: State
    var payload: PlanningSuggestionPayload
    var resultingItemID: UUID?
    var resultingMutationID: UUID?

    init(
        id: UUID,
        title: String,
        summary: String,
        source: String,
        createdAt: Date,
        expiresAt: Date,
        state: State,
        payload: PlanningSuggestionPayload = .advisory,
        resultingItemID: UUID? = nil,
        resultingMutationID: UUID? = nil
    ) {
        self.id = id
        self.title = title
        self.summary = summary
        self.source = source
        self.createdAt = createdAt
        self.expiresAt = expiresAt
        self.state = state
        self.payload = payload
        self.resultingItemID = resultingItemID
        self.resultingMutationID = resultingMutationID
    }

    private enum CodingKeys: String, CodingKey {
        case id, title, summary, source, createdAt, expiresAt, state, payload
        case resultingItemID, resultingMutationID
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        title = try container.decode(String.self, forKey: .title)
        summary = try container.decode(String.self, forKey: .summary)
        source = try container.decode(String.self, forKey: .source)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        expiresAt = try container.decode(Date.self, forKey: .expiresAt)
        state = try container.decode(State.self, forKey: .state)

        let schemaVersion = decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] as? Int
        if let schemaVersion, schemaVersion < 18 {
            // These fields did not exist before schema 18. Ignore even valid-
            // looking injected values so migration cannot manufacture an
            // actionable AI draft or accepted-item linkage.
            payload = .advisory
            resultingItemID = nil
            resultingMutationID = nil
        } else if let schemaVersion, schemaVersion >= 18 {
            payload = try container.decode(PlanningSuggestionPayload.self, forKey: .payload)
            resultingItemID = try container.decodeIfPresent(UUID.self, forKey: .resultingItemID)
            resultingMutationID = try container.decodeIfPresent(UUID.self, forKey: .resultingMutationID)
        } else {
            // Standalone decoders remain source-compatible with legacy fixtures;
            // durable planner decoding always supplies the authenticated schema.
            payload = try container.decodeIfPresent(
                PlanningSuggestionPayload.self,
                forKey: .payload
            ) ?? .advisory
            resultingItemID = try container.decodeIfPresent(UUID.self, forKey: .resultingItemID)
            resultingMutationID = try container.decodeIfPresent(UUID.self, forKey: .resultingMutationID)
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(title, forKey: .title)
        try container.encode(summary, forKey: .summary)
        try container.encode(source, forKey: .source)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(expiresAt, forKey: .expiresAt)
        try container.encode(state, forKey: .state)
        try container.encode(payload, forKey: .payload)
        try container.encodeIfPresent(resultingItemID, forKey: .resultingItemID)
        try container.encodeIfPresent(resultingMutationID, forKey: .resultingMutationID)
    }

    var hasValidShape: Bool {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty,
              title == self.title,
              title.unicodeScalars.count <= DayWeaveCanonicalItemDraft.maximumTitleScalars,
              summary.utf8.count <= Self.maximumSummaryUTF8Bytes,
              !source.isEmpty,
              source.utf8.count <= 1_024,
              !Self.hasUnsafeText(title, allowingLayoutControls: false),
              !Self.hasUnsafeText(summary, allowingLayoutControls: true),
              !Self.hasUnsafeText(source, allowingLayoutControls: false),
              createdAt.timeIntervalSinceReferenceDate.isFinite,
              expiresAt.timeIntervalSinceReferenceDate.isFinite,
              createdAt < expiresAt,
              expiresAt.timeIntervalSince(createdAt) <= 31 * 24 * 60 * 60 else {
            return false
        }

        switch payload {
        case .advisory:
            return resultingItemID == nil && resultingMutationID == nil
        case let .canonicalItemDraft(itemDraft):
            return source == Self.codexSource
                && state == .pending
                && resultingItemID == nil
                && resultingMutationID == nil
                && title == itemDraft.draft.title
                && itemDraft.hasValidShape
        case let .canonicalItemReference(itemID):
            guard source == Self.codexSource, state != .pending else { return false }
            guard title == Self.scrubbedCanonicalTitle,
                  summary == Self.scrubbedCanonicalSummary(for: state) else {
                return false
            }
            if state == .accepted {
                return resultingItemID == itemID && resultingMutationID != nil
            }
            return resultingItemID == nil && resultingMutationID == nil
        }
    }

    static func collectionIsValid(_ suggestions: [Self]) -> Bool {
        guard suggestions.count <= maximumStoredCount,
              Set(suggestions.map(\.id)).count == suggestions.count,
              suggestions.allSatisfy(\.hasValidShape) else {
            return false
        }
        var aggregateBytes = 0
        let encoder = JSONEncoder()
        for suggestion in suggestions {
            guard let data = try? encoder.encode(suggestion),
                  aggregateBytes <= maximumAggregateEncodedBytes - data.count else {
                return false
            }
            aggregateBytes += data.count
        }
        return true
    }

    var migratedLegacyAdvisory: Self {
        Self(
            id: id,
            title: title,
            summary: summary,
            source: source,
            createdAt: createdAt,
            expiresAt: expiresAt,
            state: state,
            payload: .advisory
        )
    }

    static func scrubbedCanonicalSummary(for state: State) -> String? {
        switch state {
        case .accepted: "Approved after local review."
        case .rejected: "Rejected; retained draft content was removed."
        case .expired: "Expired; retained draft content was removed."
        case .pending: nil
        }
    }

    fileprivate static func hasUnsafeText(
        _ value: String,
        allowingLayoutControls: Bool
    ) -> Bool {
        for scalar in value.unicodeScalars {
            if (0x202A...0x202E).contains(scalar.value)
                || (0x2066...0x2069).contains(scalar.value)
                || scalar.value == 0x061C
                || scalar.value == 0x200E
                || scalar.value == 0x200F {
                return true
            }
            if CharacterSet.controlCharacters.contains(scalar),
               !(allowingLayoutControls && (scalar.value == 0x09 || scalar.value == 0x0A)) {
                return true
            }
        }
        return false
    }
}
