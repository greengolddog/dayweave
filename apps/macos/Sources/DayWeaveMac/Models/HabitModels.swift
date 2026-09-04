import Foundation

private struct DayWeaveAnyCodingKey: CodingKey, Hashable {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) {
        self.stringValue = stringValue
        intValue = nil
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}

private func requireExactHabitKeys(
    from decoder: any Decoder,
    required: Set<String>,
    optional: Set<String> = []
) throws {
    let container = try decoder.container(keyedBy: DayWeaveAnyCodingKey.self)
    let observed = Set(container.allKeys.map(\.stringValue))
    guard required.isSubset(of: observed),
          observed.isSubset(of: required.union(optional)) else {
        throw DecodingError.dataCorrupted(
            .init(
                codingPath: decoder.codingPath,
                debugDescription: "Habit response has missing or unsupported fields"
            )
        )
    }
}

struct DayWeaveLocalDate: Codable, Comparable, Hashable, Sendable {
    let rawValue: String

    init?(_ rawValue: String) {
        guard Self.isCanonical(rawValue) else { return nil }
        self.rawValue = rawValue
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        let value = try container.decode(String.self)
        guard Self.isCanonical(value) else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Expected a canonical calendar date"
            )
        }
        rawValue = value
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    static func < (left: Self, right: Self) -> Bool {
        left.rawValue < right.rawValue
    }

    static func containing(_ date: Date, timezoneName: String) -> Self? {
        guard let timezone = TimeZone(identifier: timezoneName) else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = timezone
        let parts = calendar.dateComponents([.year, .month, .day], from: date)
        guard let year = parts.year, let month = parts.month, let day = parts.day else {
            return nil
        }
        return Self(String(format: "%04d-%02d-%02d", year, month, day))
    }

    func date(in timezoneName: String) -> Date? {
        guard let timezone = TimeZone(identifier: timezoneName) else { return nil }
        let pieces = rawValue.split(separator: "-").compactMap { Int($0) }
        guard pieces.count == 3 else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = timezone
        return calendar.date(from: DateComponents(
            timeZone: timezone,
            year: pieces[0],
            month: pieces[1],
            day: pieces[2]
        ))
    }

    private static func isCanonical(_ value: String) -> Bool {
        let bytes = Array(value.utf8)
        guard bytes.count == 10,
              bytes[4] == 45,
              bytes[7] == 45,
              bytes.enumerated().allSatisfy({ index, byte in
                  index == 4 || index == 7 || (48...57).contains(byte)
              }) else { return false }
        let components = value.split(separator: "-").compactMap { Int($0) }
        guard components.count == 3,
              (1_900...2_200).contains(components[0]),
              (1...12).contains(components[1]),
              (1...31).contains(components[2]) else { return false }
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let candidate = DateComponents(
            calendar: calendar,
            timeZone: calendar.timeZone,
            year: components[0],
            month: components[1],
            day: components[2]
        )
        guard let date = calendar.date(from: candidate) else { return false }
        let roundTrip = calendar.dateComponents([.year, .month, .day], from: date)
        return roundTrip.year == components[0]
            && roundTrip.month == components[1]
            && roundTrip.day == components[2]
    }
}

enum DayWeaveHabitOutcomeStatus: String, Codable, CaseIterable, Sendable {
    case unresolved
    case partial
    case completed
    case skipped

    var title: String {
        switch self {
        case .unresolved: "Not logged"
        case .partial: "Partly done"
        case .completed: "Completed"
        case .skipped: "Skipped"
        }
    }

    var symbol: String {
        switch self {
        case .unresolved: "circle"
        case .partial: "circle.lefthalf.filled"
        case .completed: "checkmark.circle.fill"
        case .skipped: "forward.circle"
        }
    }
}

struct DayWeaveHabitOutcomeInput: Codable, Equatable, Sendable {
    static let maximumNoteCharacters = 10_000
    static let maximumUnitCharacters = 200
    static let maximumQuantity: Int64 = 1_000_000_000_000
    static let maximumActualSeconds: UInt64 = 366 * 24 * 60 * 60

    let status: DayWeaveHabitOutcomeStatus
    let progressBasisPoints: UInt16
    let quantity: Int64?
    let unit: String?
    let actualSeconds: UInt64?
    let note: String?
    let occurredAt: Date

    private enum CodingKeys: String, CodingKey {
        case status
        case progressBasisPoints = "progress_basis_points"
        case quantity
        case unit
        case actualSeconds = "actual_seconds"
        case note
        case occurredAt = "occurred_at"
    }

    init(
        status: DayWeaveHabitOutcomeStatus,
        progressBasisPoints: UInt16,
        quantity: Int64? = nil,
        unit: String? = nil,
        actualSeconds: UInt64? = nil,
        note: String? = nil,
        occurredAt: Date
    ) {
        self.status = status
        self.progressBasisPoints = progressBasisPoints
        self.quantity = quantity
        self.unit = unit
        self.actualSeconds = actualSeconds
        self.note = note
        self.occurredAt = occurredAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: [
            "status", "progress_basis_points", "quantity", "unit", "actual_seconds", "note",
            "occurred_at",
        ])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        status = try container.decode(DayWeaveHabitOutcomeStatus.self, forKey: .status)
        progressBasisPoints = try container.decode(UInt16.self, forKey: .progressBasisPoints)
        quantity = try container.decodeIfPresent(Int64.self, forKey: .quantity)
        unit = try container.decodeIfPresent(String.self, forKey: .unit)
        actualSeconds = try container.decodeIfPresent(UInt64.self, forKey: .actualSeconds)
        note = try container.decodeIfPresent(String.self, forKey: .note)
        occurredAt = try container.decode(Date.self, forKey: .occurredAt)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit outcome input")
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(status, forKey: .status)
        try container.encode(progressBasisPoints, forKey: .progressBasisPoints)
        try container.encodeIfPresent(quantity, forKey: .quantity)
        if quantity == nil { try container.encodeNil(forKey: .quantity) }
        try container.encodeIfPresent(unit, forKey: .unit)
        if unit == nil { try container.encodeNil(forKey: .unit) }
        try container.encodeIfPresent(actualSeconds, forKey: .actualSeconds)
        if actualSeconds == nil { try container.encodeNil(forKey: .actualSeconds) }
        try container.encodeIfPresent(note, forKey: .note)
        if note == nil { try container.encodeNil(forKey: .note) }
        try container.encode(occurredAt, forKey: .occurredAt)
    }

    var hasValidShape: Bool {
        guard occurredAt.timeIntervalSinceReferenceDate.isFinite,
              quantity.map({ $0 >= -Self.maximumQuantity && $0 <= Self.maximumQuantity }) ?? true,
              actualSeconds.map({ $0 <= Self.maximumActualSeconds }) ?? true,
              (quantity == nil) == (unit == nil),
              unit.map({ Self.isValidText(
                  $0,
                  maximumCharacters: Self.maximumUnitCharacters,
                  permitsFormattingControls: false
              ) }) ?? true,
              note.map({ Self.isValidText(
                  $0,
                  maximumCharacters: Self.maximumNoteCharacters,
                  permitsFormattingControls: true
              ) }) ?? true else { return false }

        return switch status {
        case .unresolved:
            progressBasisPoints == 0
                && quantity == nil
                && actualSeconds == nil
                && note == nil
        case .partial:
            (1..<10_000).contains(progressBasisPoints)
        case .completed:
            progressBasisPoints == 10_000
        case .skipped:
            progressBasisPoints < 10_000
        }
    }

    static func completed(
        quantity: Int64? = nil,
        unit: String? = nil,
        actualSeconds: UInt64? = nil,
        note: String? = nil,
        occurredAt: Date = Date()
    ) -> Self {
        .init(
            status: .completed,
            progressBasisPoints: 10_000,
            quantity: quantity,
            unit: unit,
            actualSeconds: actualSeconds,
            note: note,
            occurredAt: occurredAt
        )
    }

    static func skipped(note: String? = nil, occurredAt: Date = Date()) -> Self {
        .init(
            status: .skipped,
            progressBasisPoints: 0,
            note: note,
            occurredAt: occurredAt
        )
    }

    private static func isValidText(
        _ value: String,
        maximumCharacters: Int,
        permitsFormattingControls: Bool
    ) -> Bool {
        guard !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              value.count <= maximumCharacters else { return false }
        return !value.unicodeScalars.contains { scalar in
            guard CharacterSet.controlCharacters.contains(scalar) else { return false }
            return !(permitsFormattingControls && [10, 13, 9].contains(scalar.value))
        }
    }
}

struct DayWeaveHabitOutcomeCommand: Codable, Equatable, Sendable {
    let operationID: UUID
    let expectedRevision: UInt64
    let outcome: DayWeaveHabitOutcomeInput

    private enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case expectedRevision = "expected_revision"
        case outcome
    }

    init(
        operationID: UUID,
        expectedRevision: UInt64,
        outcome: DayWeaveHabitOutcomeInput
    ) {
        self.operationID = operationID
        self.expectedRevision = expectedRevision
        self.outcome = outcome
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(
            from: decoder,
            required: ["operation_id", "expected_revision", "outcome"]
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        operationID = try container.decode(UUID.self, forKey: .operationID)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        outcome = try container.decode(DayWeaveHabitOutcomeInput.self, forKey: .outcome)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit command")
            )
        }
    }

    var hasValidShape: Bool {
        operationID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && outcome.hasValidShape
    }
}

struct DayWeaveHabitPauseStartCommand: Codable, Equatable, Sendable {
    let operationID: UUID
    let pauseID: UUID
    let expectedRevision: UInt64
    let startedAt: Date

    private enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case pauseID = "pause_id"
        case expectedRevision = "expected_revision"
        case startedAt = "started_at"
    }

    init(operationID: UUID, pauseID: UUID, expectedRevision: UInt64 = 0, startedAt: Date) {
        self.operationID = operationID
        self.pauseID = pauseID
        self.expectedRevision = expectedRevision
        self.startedAt = startedAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(
            from: decoder,
            required: ["operation_id", "pause_id", "expected_revision", "started_at"]
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        operationID = try container.decode(UUID.self, forKey: .operationID)
        pauseID = try container.decode(UUID.self, forKey: .pauseID)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        startedAt = try container.decode(Date.self, forKey: .startedAt)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid pause command")
            )
        }
    }

    var hasValidShape: Bool {
        operationID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && pauseID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && expectedRevision == 0
            && startedAt.timeIntervalSinceReferenceDate.isFinite
    }
}

struct DayWeaveHabitPauseResumeCommand: Codable, Equatable, Sendable {
    let operationID: UUID
    let expectedRevision: UInt64
    let endedAt: Date

    private enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case expectedRevision = "expected_revision"
        case endedAt = "ended_at"
    }

    init(operationID: UUID, expectedRevision: UInt64, endedAt: Date) {
        self.operationID = operationID
        self.expectedRevision = expectedRevision
        self.endedAt = endedAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(
            from: decoder,
            required: ["operation_id", "expected_revision", "ended_at"]
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        operationID = try container.decode(UUID.self, forKey: .operationID)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        endedAt = try container.decode(Date.self, forKey: .endedAt)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid resume command")
            )
        }
    }

    var hasValidShape: Bool {
        operationID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && expectedRevision > 0
            && endedAt.timeIntervalSinceReferenceDate.isFinite
    }
}

struct DayWeaveHabitOutcome: Codable, Equatable, Sendable {
    let revision: UInt64
    let status: DayWeaveHabitOutcomeStatus
    let progressBasisPoints: UInt16
    let quantity: Int64?
    let unit: String?
    let actualSeconds: UInt64?
    let note: String?
    let occurredAt: Date
    let updatedAt: Date

    private enum CodingKeys: String, CodingKey {
        case revision
        case status
        case progressBasisPoints = "progress_basis_points"
        case quantity
        case unit
        case actualSeconds = "actual_seconds"
        case note
        case occurredAt = "occurred_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: [
            "revision", "status", "progress_basis_points", "quantity", "unit",
            "actual_seconds", "note", "occurred_at", "updated_at",
        ])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        revision = try container.decode(UInt64.self, forKey: .revision)
        status = try container.decode(DayWeaveHabitOutcomeStatus.self, forKey: .status)
        progressBasisPoints = try container.decode(UInt16.self, forKey: .progressBasisPoints)
        quantity = try container.decodeIfPresent(Int64.self, forKey: .quantity)
        unit = try container.decodeIfPresent(String.self, forKey: .unit)
        actualSeconds = try container.decodeIfPresent(UInt64.self, forKey: .actualSeconds)
        note = try container.decodeIfPresent(String.self, forKey: .note)
        occurredAt = try container.decode(Date.self, forKey: .occurredAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit outcome")
            )
        }
    }

    init(
        revision: UInt64,
        status: DayWeaveHabitOutcomeStatus,
        progressBasisPoints: UInt16,
        quantity: Int64?,
        unit: String?,
        actualSeconds: UInt64?,
        note: String?,
        occurredAt: Date,
        updatedAt: Date
    ) {
        self.revision = revision
        self.status = status
        self.progressBasisPoints = progressBasisPoints
        self.quantity = quantity
        self.unit = unit
        self.actualSeconds = actualSeconds
        self.note = note
        self.occurredAt = occurredAt
        self.updatedAt = updatedAt
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(revision, forKey: .revision)
        try container.encode(status, forKey: .status)
        try container.encode(progressBasisPoints, forKey: .progressBasisPoints)
        try container.encodeIfPresent(quantity, forKey: .quantity)
        if quantity == nil { try container.encodeNil(forKey: .quantity) }
        try container.encodeIfPresent(unit, forKey: .unit)
        if unit == nil { try container.encodeNil(forKey: .unit) }
        try container.encodeIfPresent(actualSeconds, forKey: .actualSeconds)
        if actualSeconds == nil { try container.encodeNil(forKey: .actualSeconds) }
        try container.encodeIfPresent(note, forKey: .note)
        if note == nil { try container.encodeNil(forKey: .note) }
        try container.encode(occurredAt, forKey: .occurredAt)
        try container.encode(updatedAt, forKey: .updatedAt)
    }

    var input: DayWeaveHabitOutcomeInput {
        .init(
            status: status,
            progressBasisPoints: progressBasisPoints,
            quantity: quantity,
            unit: unit,
            actualSeconds: actualSeconds,
            note: note,
            occurredAt: occurredAt
        )
    }

    var hasValidShape: Bool {
        revision > 0
            && input.hasValidShape
            && updatedAt.timeIntervalSinceReferenceDate.isFinite
    }
}

struct DayWeaveHabitOccurrenceEvidence: Codable, Equatable, Sendable {
    let id: UUID
    let habitID: UUID
    let plannerOccurrenceID: UUID
    let sourceScheduleRevisionID: UUID
    let sourceItemRevision: UInt64
    let policyFingerprint: String
    let identity: JSONValue
    let nominalStart: Date
    let nominalEnd: Date
    let windowStart: Date
    let windowEnd: Date
    let localDate: DayWeaveLocalDate
    let timezoneName: String
    let expectedDurationSeconds: UInt64?
    let expectedQuantity: Int64?
    let expectedUnit: String?

    private enum CodingKeys: String, CodingKey {
        case id
        case habitID = "habit_id"
        case plannerOccurrenceID = "planner_occurrence_id"
        case sourceScheduleRevisionID = "source_schedule_revision_id"
        case sourceItemRevision = "source_item_revision"
        case policyFingerprint = "policy_fingerprint"
        case identity
        case nominalStart = "nominal_start"
        case nominalEnd = "nominal_end"
        case windowStart = "window_start"
        case windowEnd = "window_end"
        case localDate = "local_date"
        case timezoneName = "timezone_name"
        case expectedDurationSeconds = "expected_duration_seconds"
        case expectedQuantity = "expected_quantity"
        case expectedUnit = "expected_unit"
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: [
            "id", "habit_id", "planner_occurrence_id", "source_schedule_revision_id",
            "source_item_revision", "policy_fingerprint", "identity", "nominal_start",
            "nominal_end", "window_start", "window_end", "local_date", "timezone_name",
            "expected_duration_seconds", "expected_quantity", "expected_unit",
        ])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        habitID = try container.decode(UUID.self, forKey: .habitID)
        plannerOccurrenceID = try container.decode(UUID.self, forKey: .plannerOccurrenceID)
        sourceScheduleRevisionID = try container.decode(UUID.self, forKey: .sourceScheduleRevisionID)
        sourceItemRevision = try container.decode(UInt64.self, forKey: .sourceItemRevision)
        policyFingerprint = try container.decode(String.self, forKey: .policyFingerprint)
        identity = try container.decode(JSONValue.self, forKey: .identity)
        nominalStart = try container.decode(Date.self, forKey: .nominalStart)
        nominalEnd = try container.decode(Date.self, forKey: .nominalEnd)
        windowStart = try container.decode(Date.self, forKey: .windowStart)
        windowEnd = try container.decode(Date.self, forKey: .windowEnd)
        localDate = try container.decode(DayWeaveLocalDate.self, forKey: .localDate)
        timezoneName = try container.decode(String.self, forKey: .timezoneName)
        expectedDurationSeconds = try container.decodeIfPresent(
            UInt64.self,
            forKey: .expectedDurationSeconds
        )
        expectedQuantity = try container.decodeIfPresent(Int64.self, forKey: .expectedQuantity)
        expectedUnit = try container.decodeIfPresent(String.self, forKey: .expectedUnit)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(
                    codingPath: decoder.codingPath,
                    debugDescription: "Invalid habit occurrence evidence"
                )
            )
        }
    }

    init(
        id: UUID,
        habitID: UUID,
        plannerOccurrenceID: UUID,
        sourceScheduleRevisionID: UUID,
        sourceItemRevision: UInt64,
        policyFingerprint: String,
        identity: JSONValue,
        nominalStart: Date,
        nominalEnd: Date,
        windowStart: Date,
        windowEnd: Date,
        localDate: DayWeaveLocalDate,
        timezoneName: String,
        expectedDurationSeconds: UInt64?,
        expectedQuantity: Int64?,
        expectedUnit: String?
    ) {
        self.id = id
        self.habitID = habitID
        self.plannerOccurrenceID = plannerOccurrenceID
        self.sourceScheduleRevisionID = sourceScheduleRevisionID
        self.sourceItemRevision = sourceItemRevision
        self.policyFingerprint = policyFingerprint
        self.identity = identity
        self.nominalStart = nominalStart
        self.nominalEnd = nominalEnd
        self.windowStart = windowStart
        self.windowEnd = windowEnd
        self.localDate = localDate
        self.timezoneName = timezoneName
        self.expectedDurationSeconds = expectedDurationSeconds
        self.expectedQuantity = expectedQuantity
        self.expectedUnit = expectedUnit
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(habitID, forKey: .habitID)
        try container.encode(plannerOccurrenceID, forKey: .plannerOccurrenceID)
        try container.encode(sourceScheduleRevisionID, forKey: .sourceScheduleRevisionID)
        try container.encode(sourceItemRevision, forKey: .sourceItemRevision)
        try container.encode(policyFingerprint, forKey: .policyFingerprint)
        try container.encode(identity, forKey: .identity)
        try container.encode(nominalStart, forKey: .nominalStart)
        try container.encode(nominalEnd, forKey: .nominalEnd)
        try container.encode(windowStart, forKey: .windowStart)
        try container.encode(windowEnd, forKey: .windowEnd)
        try container.encode(localDate, forKey: .localDate)
        try container.encode(timezoneName, forKey: .timezoneName)
        try container.encodeIfPresent(expectedDurationSeconds, forKey: .expectedDurationSeconds)
        if expectedDurationSeconds == nil { try container.encodeNil(forKey: .expectedDurationSeconds) }
        try container.encodeIfPresent(expectedQuantity, forKey: .expectedQuantity)
        if expectedQuantity == nil { try container.encodeNil(forKey: .expectedQuantity) }
        try container.encodeIfPresent(expectedUnit, forKey: .expectedUnit)
        if expectedUnit == nil { try container.encodeNil(forKey: .expectedUnit) }
    }

    var hasValidShape: Bool {
        guard id != Self.nilID,
              habitID != Self.nilID,
              plannerOccurrenceID != Self.nilID,
              sourceScheduleRevisionID != Self.nilID,
              sourceItemRevision > 0,
              Self.isSHA256(policyFingerprint),
              case .object = identity,
              (try? JSONEncoder().encode(identity).count).map({ $0 <= 64 * 1_024 }) == true,
              nominalStart.timeIntervalSinceReferenceDate.isFinite,
              nominalEnd.timeIntervalSinceReferenceDate.isFinite,
              windowStart.timeIntervalSinceReferenceDate.isFinite,
              windowEnd.timeIntervalSinceReferenceDate.isFinite,
              windowStart < windowEnd,
              nominalStart < nominalEnd,
              nominalStart >= windowStart,
              nominalEnd <= windowEnd,
              TimeZone(identifier: timezoneName) != nil,
              expectedDurationSeconds.map({ $0 > 0 }) ?? true,
              (expectedQuantity == nil) == (expectedUnit == nil),
              expectedQuantity.map({ $0 > 0 && $0 <= DayWeaveHabitOutcomeInput.maximumQuantity })
                ?? true,
              expectedUnit.map({ value in
                  !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                      && value.count <= DayWeaveHabitOutcomeInput.maximumUnitCharacters
                      && !value.unicodeScalars.contains(
                          where: CharacterSet.controlCharacters.contains
                      )
              }) ?? true else { return false }
        return true
    }

    private static let nilID = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))

    private static func isSHA256(_ value: String) -> Bool {
        guard value.utf8.count == 71, value.hasPrefix("sha256:") else { return false }
        return value.utf8.dropFirst(7).allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        }
    }
}

struct DayWeaveHabitOccurrence: Codable, Equatable, Identifiable, Sendable {
    let evidence: DayWeaveHabitOccurrenceEvidence
    let outcome: DayWeaveHabitOutcome?

    var id: UUID { evidence.id }

    private enum CodingKeys: String, CodingKey {
        case evidence
        case outcome
    }

    init(evidence: DayWeaveHabitOccurrenceEvidence, outcome: DayWeaveHabitOutcome?) {
        self.evidence = evidence
        self.outcome = outcome
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: ["evidence", "outcome"])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        evidence = try container.decode(DayWeaveHabitOccurrenceEvidence.self, forKey: .evidence)
        outcome = try container.decodeIfPresent(DayWeaveHabitOutcome.self, forKey: .outcome)
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(evidence, forKey: .evidence)
        try container.encodeIfPresent(outcome, forKey: .outcome)
        if outcome == nil { try container.encodeNil(forKey: .outcome) }
    }
}

struct DayWeaveHabitPause: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let habitID: UUID
    let revision: UInt64
    let startedAt: Date
    let endedAt: Date?
    let preservesStreak: Bool
    let createdAt: Date
    let updatedAt: Date

    private enum CodingKeys: String, CodingKey {
        case id
        case habitID = "habit_id"
        case revision
        case startedAt = "started_at"
        case endedAt = "ended_at"
        case preservesStreak = "preserves_streak"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: [
            "id", "habit_id", "revision", "started_at", "ended_at",
            "preserves_streak", "created_at", "updated_at",
        ])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        habitID = try container.decode(UUID.self, forKey: .habitID)
        revision = try container.decode(UInt64.self, forKey: .revision)
        startedAt = try container.decode(Date.self, forKey: .startedAt)
        endedAt = try container.decodeIfPresent(Date.self, forKey: .endedAt)
        preservesStreak = try container.decode(Bool.self, forKey: .preservesStreak)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit pause")
            )
        }
    }

    init(
        id: UUID,
        habitID: UUID,
        revision: UInt64,
        startedAt: Date,
        endedAt: Date?,
        preservesStreak: Bool,
        createdAt: Date,
        updatedAt: Date
    ) {
        self.id = id
        self.habitID = habitID
        self.revision = revision
        self.startedAt = startedAt
        self.endedAt = endedAt
        self.preservesStreak = preservesStreak
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(habitID, forKey: .habitID)
        try container.encode(revision, forKey: .revision)
        try container.encode(startedAt, forKey: .startedAt)
        try container.encodeIfPresent(endedAt, forKey: .endedAt)
        if endedAt == nil { try container.encodeNil(forKey: .endedAt) }
        try container.encode(preservesStreak, forKey: .preservesStreak)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
    }

    var hasValidShape: Bool {
        id != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && habitID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
            && revision > 0
            && startedAt.timeIntervalSinceReferenceDate.isFinite
            && endedAt.map({ $0.timeIntervalSinceReferenceDate.isFinite && $0 > startedAt }) ?? true
            && createdAt.timeIntervalSinceReferenceDate.isFinite
            && updatedAt.timeIntervalSinceReferenceDate.isFinite
            && updatedAt >= createdAt
    }
}

enum DayWeaveHabitDeltaChange: Codable, Equatable, Sendable {
    case occurrenceUpsert(DayWeaveHabitOccurrence)
    case pauseUpsert(DayWeaveHabitPause)

    private enum CodingKeys: String, CodingKey {
        case type
        case occurrence
        case pause
    }

    private enum Kind: String, Codable {
        case occurrenceUpsert = "occurrence_upsert"
        case pauseUpsert = "pause_upsert"
    }

    init(from decoder: any Decoder) throws {
        let probe = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try probe.decode(Kind.self, forKey: .type)
        switch kind {
        case .occurrenceUpsert:
            try requireExactHabitKeys(from: decoder, required: ["type", "occurrence"])
            self = .occurrenceUpsert(
                try probe.decode(DayWeaveHabitOccurrence.self, forKey: .occurrence)
            )
        case .pauseUpsert:
            try requireExactHabitKeys(from: decoder, required: ["type", "pause"])
            self = .pauseUpsert(try probe.decode(DayWeaveHabitPause.self, forKey: .pause))
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .occurrenceUpsert(occurrence):
            try container.encode(Kind.occurrenceUpsert, forKey: .type)
            try container.encode(occurrence, forKey: .occurrence)
        case let .pauseUpsert(pause):
            try container.encode(Kind.pauseUpsert, forKey: .type)
            try container.encode(pause, forKey: .pause)
        }
    }
}

struct DayWeaveHabitOccurrencePage: Codable, Equatable, Sendable {
    let occurrences: [DayWeaveHabitOccurrence]
    let nextCursor: String?
    let hasMore: Bool

    private enum CodingKeys: String, CodingKey {
        case occurrences
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
    }

    init(
        occurrences: [DayWeaveHabitOccurrence],
        nextCursor: String?,
        hasMore: Bool
    ) {
        self.occurrences = occurrences
        self.nextCursor = nextCursor
        self.hasMore = hasMore
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(
            from: decoder,
            required: ["occurrences", "next_cursor", "has_more"]
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        occurrences = try container.decode([DayWeaveHabitOccurrence].self, forKey: .occurrences)
        nextCursor = try container.decodeIfPresent(String.self, forKey: .nextCursor)
        hasMore = try container.decode(Bool.self, forKey: .hasMore)
        guard occurrences.count <= 200,
              Set(occurrences.map(\.id)).count == occurrences.count,
              nextCursor.map(Self.isValidCursor) ?? !hasMore,
              hasMore == (nextCursor != nil) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid occurrence page")
            )
        }
    }

    private static func isValidCursor(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 256 && value.utf8.allSatisfy {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0)
                || $0 == 45 || $0 == 95
        }
    }
}

struct DayWeaveHabitDeltaPage: Codable, Equatable, Sendable {
    let changes: [DayWeaveHabitDeltaChange]
    let nextCursor: String
    let hasMore: Bool

    private enum CodingKeys: String, CodingKey {
        case changes
        case nextCursor = "next_cursor"
        case hasMore = "has_more"
    }

    init(changes: [DayWeaveHabitDeltaChange], nextCursor: String, hasMore: Bool) {
        self.changes = changes
        self.nextCursor = nextCursor
        self.hasMore = hasMore
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: ["changes", "next_cursor", "has_more"])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        changes = try container.decode([DayWeaveHabitDeltaChange].self, forKey: .changes)
        nextCursor = try container.decode(String.self, forKey: .nextCursor)
        hasMore = try container.decode(Bool.self, forKey: .hasMore)
        guard changes.count <= 200,
              !nextCursor.isEmpty,
              nextCursor.utf8.count <= 256,
              nextCursor.utf8.allSatisfy({
                  (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0)
                      || $0 == 45 || $0 == 95
              }) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit delta")
            )
        }
    }
}

struct DayWeaveHabitOccurrenceMutationResponse: Codable, Equatable, Sendable {
    let occurrence: DayWeaveHabitOccurrence
    let replayed: Bool

    private enum CodingKeys: String, CodingKey {
        case occurrence
        case replayed
    }

    init(occurrence: DayWeaveHabitOccurrence, replayed: Bool) {
        self.occurrence = occurrence
        self.replayed = replayed
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: ["occurrence", "replayed"])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        occurrence = try container.decode(DayWeaveHabitOccurrence.self, forKey: .occurrence)
        replayed = try container.decode(Bool.self, forKey: .replayed)
    }
}

struct DayWeaveHabitPauseMutationResponse: Codable, Equatable, Sendable {
    let pause: DayWeaveHabitPause
    let replayed: Bool

    private enum CodingKeys: String, CodingKey {
        case pause
        case replayed
    }

    init(pause: DayWeaveHabitPause, replayed: Bool) {
        self.pause = pause
        self.replayed = replayed
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: ["pause", "replayed"])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        pause = try container.decode(DayWeaveHabitPause.self, forKey: .pause)
        replayed = try container.decode(Bool.self, forKey: .replayed)
    }
}

enum DayWeaveHabitAnalyticsBucket: String, Codable, CaseIterable, Sendable {
    case day
    case week
    case month
}

enum DayWeaveHabitSupportiveFactCode: String, Codable, Hashable, Sendable {
    case noData = "no_data"
    case activeStreak = "active_streak"
    case strongAdherence = "strong_adherence"
    case freshStartAvailable = "fresh_start_available"

    var message: String {
        switch self {
        case .noData: "Your pattern will appear as you log a few occurrences."
        case .activeStreak: "You have a steady rhythm going—keep choosing what supports you today."
        case .strongAdherence: "Your recent follow-through is strong. Leave room for rest when you need it."
        case .freshStartAvailable: "A missed day is information, not a verdict. The next occurrence is a fresh start."
        }
    }
}

struct DayWeaveHabitQuantityTotal: Codable, Equatable, Sendable {
    static let maximumAbsoluteAmount: Int64 = 50_000_000_000_000_000

    let unit: String
    let amount: Int64

    private enum CodingKeys: String, CodingKey {
        case unit
        case amount
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: ["unit", "amount"])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        unit = try container.decode(String.self, forKey: .unit)
        amount = try container.decode(Int64.self, forKey: .amount)
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid quantity total")
            )
        }
    }

    init(unit: String, amount: Int64) {
        self.unit = unit
        self.amount = amount
    }

    var hasValidShape: Bool {
        amount >= -Self.maximumAbsoluteAmount
            && amount <= Self.maximumAbsoluteAmount
            && !unit.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && unit.count <= DayWeaveHabitOutcomeInput.maximumUnitCharacters
            && !unit.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }
}

struct DayWeaveHabitAnalyticsTotals: Codable, Equatable, Sendable {
    /// The server deliberately refuses to aggregate more ledger rows than
    /// this for one analytics projection. Keeping the same bound client-side
    /// makes later presentation arithmetic safe for untrusted responses.
    static let maximumExpectedOccurrences: UInt64 = 50_000

    let expected: UInt64
    let eligible: UInt64
    let completed: UInt64
    let partial: UInt64
    let skipped: UInt64
    let missed: UInt64
    let excused: UInt64
    let unresolved: UInt64
    let adherenceBasisPoints: UInt16
    let actualSecondsTotal: UInt64
    let quantityTotals: [DayWeaveHabitQuantityTotal]

    private enum CodingKeys: String, CodingKey {
        case expected
        case eligible
        case completed
        case partial
        case skipped
        case missed
        case excused
        case unresolved
        case adherenceBasisPoints = "adherence_basis_points"
        case actualSecondsTotal = "actual_seconds_total"
        case quantityTotals = "quantity_totals"
    }

    var hasValidShape: Bool {
        let maximumActual = expected.multipliedReportingOverflow(
            by: DayWeaveHabitOutcomeInput.maximumActualSeconds
        )
        guard expected <= Self.maximumExpectedOccurrences,
              expected == eligible.addingReportingOverflow(excused).partialValue,
              !eligible.addingReportingOverflow(excused).overflow,
              completed <= eligible,
              partial <= eligible,
              skipped <= eligible,
              missed <= eligible,
              unresolved <= eligible,
              !maximumActual.overflow,
              actualSecondsTotal <= maximumActual.partialValue,
              quantityTotals.count <= expected,
              quantityTotals.allSatisfy(\.hasValidShape) else { return false }
        let partitions = [completed, partial, skipped, missed, unresolved]
            .reduce((value: UInt64(0), overflow: false)) { result, value in
                let sum = result.value.addingReportingOverflow(value)
                return (sum.partialValue, result.overflow || sum.overflow)
            }
        guard !partitions.overflow,
              partitions.value == eligible,
              Set(quantityTotals.map(\.unit)).count == quantityTotals.count else { return false }
        return adherenceBasisPoints <= 10_000
            && (eligible > 0 || adherenceBasisPoints == 0)
    }
}

struct DayWeaveHabitTrendBucket: Codable, Equatable, Sendable {
    let startDate: DayWeaveLocalDate
    let endDate: DayWeaveLocalDate
    let totals: DayWeaveHabitAnalyticsTotals

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case startDate = "start_date"
        case endDate = "end_date"
        case expected
        case eligible
        case completed
        case partial
        case skipped
        case missed
        case excused
        case unresolved
        case adherenceBasisPoints = "adherence_basis_points"
        case actualSecondsTotal = "actual_seconds_total"
        case quantityTotals = "quantity_totals"
    }

    init(
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        totals: DayWeaveHabitAnalyticsTotals
    ) {
        self.startDate = startDate
        self.endDate = endDate
        self.totals = totals
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(
            from: decoder,
            required: Set(CodingKeys.allCases.map(\.rawValue))
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        startDate = try container.decode(DayWeaveLocalDate.self, forKey: .startDate)
        endDate = try container.decode(DayWeaveLocalDate.self, forKey: .endDate)
        totals = .init(
            expected: try container.decode(UInt64.self, forKey: .expected),
            eligible: try container.decode(UInt64.self, forKey: .eligible),
            completed: try container.decode(UInt64.self, forKey: .completed),
            partial: try container.decode(UInt64.self, forKey: .partial),
            skipped: try container.decode(UInt64.self, forKey: .skipped),
            missed: try container.decode(UInt64.self, forKey: .missed),
            excused: try container.decode(UInt64.self, forKey: .excused),
            unresolved: try container.decode(UInt64.self, forKey: .unresolved),
            adherenceBasisPoints: try container.decode(
                UInt16.self,
                forKey: .adherenceBasisPoints
            ),
            actualSecondsTotal: try container.decode(UInt64.self, forKey: .actualSecondsTotal),
            quantityTotals: try container.decode(
                [DayWeaveHabitQuantityTotal].self,
                forKey: .quantityTotals
            )
        )
        guard startDate <= endDate, totals.hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit trend")
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(startDate, forKey: .startDate)
        try container.encode(endDate, forKey: .endDate)
        try container.encode(totals.expected, forKey: .expected)
        try container.encode(totals.eligible, forKey: .eligible)
        try container.encode(totals.completed, forKey: .completed)
        try container.encode(totals.partial, forKey: .partial)
        try container.encode(totals.skipped, forKey: .skipped)
        try container.encode(totals.missed, forKey: .missed)
        try container.encode(totals.excused, forKey: .excused)
        try container.encode(totals.unresolved, forKey: .unresolved)
        try container.encode(totals.adherenceBasisPoints, forKey: .adherenceBasisPoints)
        try container.encode(totals.actualSecondsTotal, forKey: .actualSecondsTotal)
        try container.encode(totals.quantityTotals, forKey: .quantityTotals)
    }
}

struct DayWeaveHabitAnalytics: Codable, Equatable, Identifiable, Sendable {
    let habitID: UUID
    let startDate: DayWeaveLocalDate
    let endDate: DayWeaveLocalDate
    let bucket: DayWeaveHabitAnalyticsBucket
    let totals: DayWeaveHabitAnalyticsTotals
    let currentStreak: UInt32
    let longestStreak: UInt32
    let trends: [DayWeaveHabitTrendBucket]
    let supportiveFactCodes: [DayWeaveHabitSupportiveFactCode]

    var id: UUID { habitID }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case habitID = "habit_id"
        case startDate = "start_date"
        case endDate = "end_date"
        case bucket
        case expected
        case eligible
        case completed
        case partial
        case skipped
        case missed
        case excused
        case unresolved
        case adherenceBasisPoints = "adherence_basis_points"
        case actualSecondsTotal = "actual_seconds_total"
        case quantityTotals = "quantity_totals"
        case currentStreak = "current_streak"
        case longestStreak = "longest_streak"
        case trends
        case supportiveFactCodes = "supportive_fact_codes"
    }

    init(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        bucket: DayWeaveHabitAnalyticsBucket,
        totals: DayWeaveHabitAnalyticsTotals,
        currentStreak: UInt32,
        longestStreak: UInt32,
        trends: [DayWeaveHabitTrendBucket],
        supportiveFactCodes: [DayWeaveHabitSupportiveFactCode]
    ) {
        self.habitID = habitID
        self.startDate = startDate
        self.endDate = endDate
        self.bucket = bucket
        self.totals = totals
        self.currentStreak = currentStreak
        self.longestStreak = longestStreak
        self.trends = trends
        self.supportiveFactCodes = supportiveFactCodes
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(
            from: decoder,
            required: Set(CodingKeys.allCases.map(\.rawValue))
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        habitID = try container.decode(UUID.self, forKey: .habitID)
        startDate = try container.decode(DayWeaveLocalDate.self, forKey: .startDate)
        endDate = try container.decode(DayWeaveLocalDate.self, forKey: .endDate)
        bucket = try container.decode(DayWeaveHabitAnalyticsBucket.self, forKey: .bucket)
        totals = .init(
            expected: try container.decode(UInt64.self, forKey: .expected),
            eligible: try container.decode(UInt64.self, forKey: .eligible),
            completed: try container.decode(UInt64.self, forKey: .completed),
            partial: try container.decode(UInt64.self, forKey: .partial),
            skipped: try container.decode(UInt64.self, forKey: .skipped),
            missed: try container.decode(UInt64.self, forKey: .missed),
            excused: try container.decode(UInt64.self, forKey: .excused),
            unresolved: try container.decode(UInt64.self, forKey: .unresolved),
            adherenceBasisPoints: try container.decode(
                UInt16.self,
                forKey: .adherenceBasisPoints
            ),
            actualSecondsTotal: try container.decode(UInt64.self, forKey: .actualSecondsTotal),
            quantityTotals: try container.decode(
                [DayWeaveHabitQuantityTotal].self,
                forKey: .quantityTotals
            )
        )
        currentStreak = try container.decode(UInt32.self, forKey: .currentStreak)
        longestStreak = try container.decode(UInt32.self, forKey: .longestStreak)
        trends = try container.decode([DayWeaveHabitTrendBucket].self, forKey: .trends)
        supportiveFactCodes = try container.decode(
            [DayWeaveHabitSupportiveFactCode].self,
            forKey: .supportiveFactCodes
        )
        guard hasValidShape else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "Invalid habit analytics")
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(habitID, forKey: .habitID)
        try container.encode(startDate, forKey: .startDate)
        try container.encode(endDate, forKey: .endDate)
        try container.encode(bucket, forKey: .bucket)
        try container.encode(totals.expected, forKey: .expected)
        try container.encode(totals.eligible, forKey: .eligible)
        try container.encode(totals.completed, forKey: .completed)
        try container.encode(totals.partial, forKey: .partial)
        try container.encode(totals.skipped, forKey: .skipped)
        try container.encode(totals.missed, forKey: .missed)
        try container.encode(totals.excused, forKey: .excused)
        try container.encode(totals.unresolved, forKey: .unresolved)
        try container.encode(totals.adherenceBasisPoints, forKey: .adherenceBasisPoints)
        try container.encode(totals.actualSecondsTotal, forKey: .actualSecondsTotal)
        try container.encode(totals.quantityTotals, forKey: .quantityTotals)
        try container.encode(currentStreak, forKey: .currentStreak)
        try container.encode(longestStreak, forKey: .longestStreak)
        try container.encode(trends, forKey: .trends)
        try container.encode(supportiveFactCodes, forKey: .supportiveFactCodes)
    }

    var hasValidShape: Bool {
        var expectedFacts = Set<DayWeaveHabitSupportiveFactCode>()
        if totals.expected == 0 { expectedFacts.insert(.noData) }
        if currentStreak > 0 { expectedFacts.insert(.activeStreak) }
        if totals.eligible > 0, totals.adherenceBasisPoints >= 8_000 {
            expectedFacts.insert(.strongAdherence)
        }
        if totals.missed > 0 { expectedFacts.insert(.freshStartAvailable) }
        guard habitID != UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
              startDate <= endDate,
              totals.hasValidShape,
              currentStreak <= longestStreak,
              longestStreak <= 366,
              Set(supportiveFactCodes) == expectedFacts,
              supportiveFactCodes.count == expectedFacts.count,
              trends.count <= 366,
              trends.allSatisfy({
                  $0.startDate >= startDate && $0.endDate <= endDate && $0.totals.hasValidShape
              }),
              zip(trends, trends.dropFirst()).allSatisfy({ pair in
                  pair.0.endDate < pair.1.startDate
              }) else {
            return false
        }
        return true
    }
}

struct DayWeaveHabitAnalyticsEnvelope: Codable, Equatable, Sendable {
    let analytics: DayWeaveHabitAnalytics

    private enum CodingKeys: String, CodingKey {
        case analytics
    }

    init(from decoder: any Decoder) throws {
        try requireExactHabitKeys(from: decoder, required: ["analytics"])
        let container = try decoder.container(keyedBy: CodingKeys.self)
        analytics = try container.decode(DayWeaveHabitAnalytics.self, forKey: .analytics)
    }
}
