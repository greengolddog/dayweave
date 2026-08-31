import Foundation

extension CodingUserInfoKey {
    /// Used only while decoding authenticated planner snapshots from schemas 1...4.
    static let dayWeaveAllowsMissingSensitivity = CodingUserInfoKey(
        rawValue: "com.greengolddog.dayweave.allows-missing-sensitivity"
    )!

    /// Lets nested durable models distinguish authenticated legacy snapshots
    /// from the current schema. This prevents fields injected into an older
    /// snapshot from acquiring authority during migration.
    static let dayWeavePlannerSnapshotSchemaVersion = CodingUserInfoKey(
        rawValue: "com.greengolddog.dayweave.planner-snapshot-schema-version"
    )!
}

enum DayWeaveCanonicalItemKind: Codable, Equatable, Hashable, Sendable {
    case event, task, habit, routine, goal, breakTime
    case unknown(String)

    var wireValue: String {
        switch self {
        case .event: "event"
        case .task: "task"
        case .habit: "habit"
        case .routine: "routine"
        case .goal: "goal"
        case .breakTime: "break"
        case let .unknown(value): value
        }
    }

    init(from decoder: any Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        self = switch value {
        case "event": .event
        case "task": .task
        case "habit": .habit
        case "routine": .routine
        case "goal": .goal
        case "break": .breakTime
        default: .unknown(value)
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wireValue)
    }
}

enum DayWeaveCanonicalItemStatus: Codable, Equatable, Hashable, Sendable {
    case inbox, planned, scheduled, inProgress, paused, completed, skipped, cancelled
    case unknown(String)

    var wireValue: String {
        switch self {
        case .inbox: "inbox"
        case .planned: "planned"
        case .scheduled: "scheduled"
        case .inProgress: "in_progress"
        case .paused: "paused"
        case .completed: "completed"
        case .skipped: "skipped"
        case .cancelled: "cancelled"
        case let .unknown(value): value
        }
    }

    init(from decoder: any Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        self = switch value {
        case "inbox": .inbox
        case "planned": .planned
        case "scheduled": .scheduled
        case "in_progress": .inProgress
        case "paused": .paused
        case "completed": .completed
        case "skipped": .skipped
        case "cancelled": .cancelled
        default: .unknown(value)
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wireValue)
    }
}

enum DayWeaveSplitPolicy: Codable, Equatable, Sendable {
    case indivisible
    case splittable(minimumChunkSeconds: UInt32, maximumChunkSeconds: UInt32)
    case unknown([String: JSONValue])

    private enum CodingKeys: String, CodingKey {
        case type
        case minimumChunkSeconds = "minimum_chunk_seconds"
        case maximumChunkSeconds = "maximum_chunk_seconds"
    }

    init(from decoder: any Decoder) throws {
        let raw = try [String: JSONValue](from: decoder)
        guard case let .string(type)? = raw["type"] else {
            self = .unknown(raw)
            return
        }
        switch type {
        case "indivisible":
            guard Set(raw.keys) == ["type"] else {
                self = .unknown(raw)
                return
            }
            self = .indivisible
        case "splittable":
            guard case let .number(minimum)? = raw["minimum_chunk_seconds"],
                  case let .number(maximum)? = raw["maximum_chunk_seconds"],
                  Set(raw.keys) == ["type", "minimum_chunk_seconds", "maximum_chunk_seconds"],
                  let minimumValue = minimum.exactUInt32,
                  let maximumValue = maximum.exactUInt32 else {
                self = .unknown(raw)
                return
            }
            self = .splittable(
                minimumChunkSeconds: minimumValue,
                maximumChunkSeconds: maximumValue
            )
        default:
            self = .unknown(raw)
        }
    }

    func encode(to encoder: any Encoder) throws {
        switch self {
        case .indivisible:
            try ["type": JSONValue.string("indivisible")].encode(to: encoder)
        case let .splittable(minimum, maximum):
            let raw: [String: JSONValue] = [
                "type": .string("splittable"),
                "minimum_chunk_seconds": .number(JSONNumber(UInt64(minimum))),
                "maximum_chunk_seconds": .number(JSONNumber(UInt64(maximum))),
            ]
            try raw.encode(to: encoder)
        case let .unknown(raw):
            try raw.encode(to: encoder)
        }
    }

    var isSupportedForWrite: Bool {
        if case .unknown = self { return false }
        return true
    }
}

struct DayWeaveCanonicalItem: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
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
    let isExecutable: Bool
    let revision: UInt64
    let createdAt: Date
    let updatedAt: Date
    let completedAt: Date?
    let deletedAt: Date?
    /// Persists the fact that an arbitrary server JSON number could not be
    /// round-tripped exactly. Snapshot JSON may normalize `1.0` to `1`; this
    /// marker prevents a restart from accidentally upgrading that item to
    /// writable.
    let hasNonRoundTrippableJSONNumber: Bool
    /// Future server fields are retained in the encrypted cache and make the
    /// item read-only until this client understands how to round-trip them.
    let unsupportedFields: [String: JSONValue]

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case id, kind, status, title, notes, recurrence, importance, urgency, revision
        case isSensitive = "is_sensitive"
        case timezoneName = "timezone_name"
        case durationSeconds = "duration_seconds"
        case deadlineAt = "deadline_at"
        case earliestStartAt = "earliest_start_at"
        case flexibleConstraints = "flexible_constraints"
        case splitPolicy = "split_policy"
        case parentID = "parent_id"
        case siblingOrder = "sibling_order"
        case isExecutable = "is_executable"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case completedAt = "completed_at"
        case deletedAt = "deleted_at"
        case hasNonRoundTrippableJSONNumber = "_dayweave_non_roundtrippable_json_number"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        if decoder.userInfo[.dayWeaveAllowsMissingSensitivity] as? Bool == true {
            isSensitive = try container.decodeIfPresent(Bool.self, forKey: .isSensitive) ?? false
        } else {
            isSensitive = try container.decode(Bool.self, forKey: .isSensitive)
        }
        kind = try container.decode(DayWeaveCanonicalItemKind.self, forKey: .kind)
        status = try container.decode(DayWeaveCanonicalItemStatus.self, forKey: .status)
        title = try container.decode(String.self, forKey: .title)
        notes = try container.decodeIfPresent(String.self, forKey: .notes)
        timezoneName = try container.decode(String.self, forKey: .timezoneName)
        durationSeconds = try container.decodeIfPresent(UInt32.self, forKey: .durationSeconds)
        deadlineAt = try container.decodeIfPresent(Date.self, forKey: .deadlineAt)
        earliestStartAt = try container.decodeIfPresent(Date.self, forKey: .earliestStartAt)
        recurrence = try container.decodeIfPresent(JSONValue.self, forKey: .recurrence)
        flexibleConstraints = try container.decode(JSONValue.self, forKey: .flexibleConstraints)
        splitPolicy = try container.decode(DayWeaveSplitPolicy.self, forKey: .splitPolicy)
        importance = try container.decode(UInt8.self, forKey: .importance)
        urgency = try container.decode(UInt8.self, forKey: .urgency)
        parentID = try container.decodeIfPresent(UUID.self, forKey: .parentID)
        siblingOrder = try container.decode(UInt32.self, forKey: .siblingOrder)
        isExecutable = try container.decode(Bool.self, forKey: .isExecutable)
        revision = try container.decode(UInt64.self, forKey: .revision)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        completedAt = try container.decodeIfPresent(Date.self, forKey: .completedAt)
        deletedAt = try container.decodeIfPresent(Date.self, forKey: .deletedAt)
        let persistedNumericMarker = try container.decodeIfPresent(
            Bool.self,
            forKey: .hasNonRoundTrippableJSONNumber
        ) ?? false
        let known = Set(CodingKeys.allCases.map(\.rawValue))
        let dynamic = try decoder.container(keyedBy: DynamicCodingKey.self)
        var future: [String: JSONValue] = [:]
        for key in dynamic.allKeys where !known.contains(key.stringValue) {
            future[key.stringValue] = try dynamic.decode(JSONValue.self, forKey: key)
        }
        unsupportedFields = future
        let splitPolicyNumbersAreRoundTrippable: Bool
        if case let .unknown(raw) = splitPolicy {
            splitPolicyNumbersAreRoundTrippable = raw.values.allSatisfy(
                \.supportsLosslessRoundTrip
            )
        } else {
            splitPolicyNumbersAreRoundTrippable = true
        }
        hasNonRoundTrippableJSONNumber = persistedNumericMarker
            || !(recurrence?.supportsLosslessRoundTrip ?? true)
            || !flexibleConstraints.supportsLosslessRoundTrip
            || !splitPolicyNumbersAreRoundTrippable
            || !future.values.allSatisfy(\.supportsLosslessRoundTrip)
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(isSensitive, forKey: .isSensitive)
        try container.encode(kind, forKey: .kind)
        try container.encode(status, forKey: .status)
        try container.encode(title, forKey: .title)
        try container.encodeIfPresent(notes, forKey: .notes)
        try container.encode(timezoneName, forKey: .timezoneName)
        try container.encodeIfPresent(durationSeconds, forKey: .durationSeconds)
        try container.encodeIfPresent(deadlineAt, forKey: .deadlineAt)
        try container.encodeIfPresent(earliestStartAt, forKey: .earliestStartAt)
        try container.encodeIfPresent(recurrence, forKey: .recurrence)
        try container.encode(flexibleConstraints, forKey: .flexibleConstraints)
        try container.encode(splitPolicy, forKey: .splitPolicy)
        try container.encode(importance, forKey: .importance)
        try container.encode(urgency, forKey: .urgency)
        try container.encodeIfPresent(parentID, forKey: .parentID)
        try container.encode(siblingOrder, forKey: .siblingOrder)
        try container.encode(isExecutable, forKey: .isExecutable)
        try container.encode(revision, forKey: .revision)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
        try container.encodeIfPresent(completedAt, forKey: .completedAt)
        try container.encodeIfPresent(deletedAt, forKey: .deletedAt)
        try container.encode(
            hasNonRoundTrippableJSONNumber,
            forKey: .hasNonRoundTrippableJSONNumber
        )
        var dynamic = encoder.container(keyedBy: DynamicCodingKey.self)
        for (key, value) in unsupportedFields {
            try dynamic.encode(value, forKey: .init(key))
        }
    }

    var supportsLosslessReplacement: Bool {
        guard splitPolicy.isSupportedForWrite, unsupportedFields.isEmpty else { return false }
        if case .unknown = kind { return false }
        if case .unknown = status { return false }
        // Status publication is a full-item replacement. We must never
        // normalize server timestamp strings or an arbitrary JSON number on
        // that path. Such items remain readable and cached, but read-only.
        guard deadlineAt == nil, earliestStartAt == nil,
              !hasNonRoundTrippableJSONNumber,
              recurrence?.supportsLosslessRoundTrip ?? true,
              flexibleConstraints.supportsLosslessRoundTrip else { return false }
        return true
    }
}

struct DayWeaveCanonicalItemFields: Encodable, Equatable, Sendable {
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
    private let permitsLosslessEncoding: Bool

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

    init(item: DayWeaveCanonicalItem, status: DayWeaveCanonicalItemStatus? = nil) {
        isSensitive = item.isSensitive
        kind = item.kind
        self.status = status ?? item.status
        title = item.title
        notes = item.notes
        timezoneName = item.timezoneName
        durationSeconds = item.durationSeconds
        deadlineAt = item.deadlineAt
        earliestStartAt = item.earliestStartAt
        recurrence = item.recurrence
        flexibleConstraints = item.flexibleConstraints
        splitPolicy = item.splitPolicy
        importance = item.importance
        urgency = item.urgency
        parentID = item.parentID
        siblingOrder = item.siblingOrder
        permitsLosslessEncoding = item.supportsLosslessReplacement
    }

    init(
        isSensitive: Bool = false,
        kind: DayWeaveCanonicalItemKind,
        status: DayWeaveCanonicalItemStatus,
        title: String,
        notes: String?,
        timezoneName: String,
        durationSeconds: UInt32?,
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
        let kindIsKnown: Bool
        if case .unknown = kind { kindIsKnown = false } else { kindIsKnown = true }
        let statusIsKnown: Bool
        if case .unknown = status { statusIsKnown = false } else { statusIsKnown = true }
        let recurrenceIsWritable = recurrence?.supportsLosslessRoundTrip ?? true
            || recurrence?.supportsCanonicalAuthoringRecurrence == true
        permitsLosslessEncoding = kindIsKnown
            && statusIsKnown
            && splitPolicy.isSupportedForWrite
            && recurrenceIsWritable
            && flexibleConstraints.supportsLosslessRoundTrip
    }

    func encode(to encoder: any Encoder) throws {
        guard permitsLosslessEncoding else {
            throw EncodingError.invalidValue(
                self,
                .init(
                    codingPath: encoder.codingPath,
                    debugDescription: "A full replacement would normalize unsupported JSON or server timestamps"
                )
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(isSensitive, forKey: .isSensitive)
        try container.encode(kind, forKey: .kind)
        try container.encode(status, forKey: .status)
        try container.encode(title, forKey: .title)
        try container.encodeIfPresent(notes, forKey: .notes)
        try container.encode(timezoneName, forKey: .timezoneName)
        try container.encodeIfPresent(durationSeconds, forKey: .durationSeconds)
        try container.encodeIfPresent(deadlineAt, forKey: .deadlineAt)
        try container.encodeIfPresent(earliestStartAt, forKey: .earliestStartAt)
        try container.encodeIfPresent(recurrence, forKey: .recurrence)
        try container.encode(flexibleConstraints, forKey: .flexibleConstraints)
        try container.encode(splitPolicy, forKey: .splitPolicy)
        try container.encode(importance, forKey: .importance)
        try container.encode(urgency, forKey: .urgency)
        try container.encodeIfPresent(parentID, forKey: .parentID)
        try container.encode(siblingOrder, forKey: .siblingOrder)
    }
}

struct DayWeaveNewCanonicalItem: Encodable, Equatable, Sendable {
    let id: UUID
    let fields: DayWeaveCanonicalItemFields

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        try container.encode(id, forKey: .init("id"))
        let data = try JSONEncoder.dayWeave.encode(fields)
        let object = try JSONDecoder.dayWeave.decode([String: JSONValue].self, from: data)
        for (key, value) in object {
            try container.encode(value, forKey: .init(key))
        }
    }
}

struct DayWeaveItemTombstone: Codable, Equatable, Sendable {
    let id: UUID
    let revision: UInt64
    let deletedAt: Date
    let parentID: UUID?

    private enum CodingKeys: String, CodingKey {
        case id, revision
        case deletedAt = "deleted_at"
        case parentID = "parent_id"
    }
}

enum DayWeaveItemDeltaChange: Decodable, Equatable, Sendable {
    case upsert(DayWeaveCanonicalItem)
    case tombstone(DayWeaveItemTombstone)

    private enum CodingKeys: String, CodingKey { case type, item, tombstone }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .type) {
        case "upsert": self = .upsert(try container.decode(DayWeaveCanonicalItem.self, forKey: .item))
        case "tombstone": self = .tombstone(try container.decode(DayWeaveItemTombstone.self, forKey: .tombstone))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type,
                in: container,
                debugDescription: "Unsupported canonical item delta change"
            )
        }
    }

    var retainedByteEstimate: Int {
        switch self {
        case let .upsert(item): item.retainedByteEstimate
        case .tombstone: 96
        }
    }
}

private extension DayWeaveCanonicalItem {
    var retainedByteEstimate: Int {
        var total = 512
        total = total.saturatingAdding(kind.wireValue.utf8.count)
        total = total.saturatingAdding(status.wireValue.utf8.count)
        total = total.saturatingAdding(title.utf8.count)
        total = total.saturatingAdding(notes?.utf8.count ?? 0)
        total = total.saturatingAdding(timezoneName.utf8.count)
        total = total.saturatingAdding(recurrence?.retainedByteEstimate ?? 0)
        total = total.saturatingAdding(flexibleConstraints.retainedByteEstimate)
        if case let .unknown(raw) = splitPolicy {
            total = total.saturatingAdding(JSONValue.object(raw).retainedByteEstimate)
        }
        for (key, value) in unsupportedFields {
            total = total.saturatingAdding(key.utf8.count)
            total = total.saturatingAdding(value.retainedByteEstimate)
        }
        return total
    }
}

private extension JSONValue {
    var retainedByteEstimate: Int {
        switch self {
        case let .object(value):
            return value.reduce(64) { partial, entry in
                partial
                    .saturatingAdding(entry.key.utf8.count)
                    .saturatingAdding(entry.value.retainedByteEstimate)
            }
        case let .array(value):
            return value.reduce(32) {
                $0.saturatingAdding($1.retainedByteEstimate)
            }
        case let .string(value): return 16.saturatingAdding(value.utf8.count)
        case .number: return 32
        case .bool, .null: return 8
        }
    }
}

private extension Int {
    func saturatingAdding(_ other: Int) -> Int {
        let (value, overflow) = addingReportingOverflow(other)
        return overflow ? .max : value
    }
}

struct DayWeaveItemDeltaPage: Equatable, Sendable {
    let changes: [DayWeaveItemDeltaChange]
    let nextCursor: String
    let hasMore: Bool
}

struct DayWeaveSchedulePreviewRequest: Codable, Equatable, Sendable {
    struct Availability: Codable, Equatable, Sendable {
        let start: Date
        let end: Date
        let contexts: [String]
        let location: String?
        let energy: String
    }

    struct Configuration: Codable, Equatable, Sendable {
        let slotGranularityMinutes: UInt32
        let stabilityWeight: UInt32
        let defaultSoftWeight: UInt32

        private enum CodingKeys: String, CodingKey {
            case slotGranularityMinutes = "slot_granularity_minutes"
            case stabilityWeight = "stability_weight"
            case defaultSoftWeight = "default_soft_weight"
        }
    }

    struct FixedBlock: Codable, Equatable, Sendable {
        let id: UUID
        let isSensitive: Bool
        let title: String
        let start: Date
        let end: Date
        let source: String

        private enum CodingKeys: String, CodingKey {
            case id, title, start, end, source
            case isSensitive = "is_sensitive"
        }
    }

    struct PreviousAssignment: Codable, Equatable, Sendable {
        struct Block: Codable, Equatable, Sendable {
            let start: Date
            let end: Date
            let sessionIndex: UInt16
            private enum CodingKeys: String, CodingKey {
                case start, end
                case sessionIndex = "session_index"
            }
        }

        let itemID: UUID
        let itemRevision: UInt64
        let occurrenceID: UUID?
        let blocks: [Block]
        let pinned: Bool

        private enum CodingKeys: String, CodingKey {
            case blocks, pinned
            case itemID = "item_id"
            case itemRevision = "item_revision"
            case occurrenceID = "occurrence_id"
        }
    }

    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
    let availability: [Availability]
    let fixedBlocks: [FixedBlock]
    let previousAssignments: [PreviousAssignment]
    let config: Configuration
    let recurrenceContext: [String: JSONValue]

    private enum CodingKeys: String, CodingKey {
        case availability, config
        case asOf = "as_of"
        case horizonStart = "horizon_start"
        case horizonEnd = "horizon_end"
        case timezoneName = "timezone_name"
        case fixedBlocks = "fixed_blocks"
        case previousAssignments = "previous_assignments"
        case recurrenceContext = "recurrence_context"
    }
}

struct DayWeaveSchedulePreview: Codable, Equatable, Sendable {
    struct RejectedItem: Codable, Equatable, Sendable {
        let itemID: UUID
        let isSensitive: Bool
        let title: String
        let reason: String
        private enum CodingKeys: String, CodingKey {
            case title, reason
            case itemID = "item_id"
            case isSensitive = "is_sensitive"
        }
    }

    struct IgnoredAssignment: Codable, Equatable, Sendable {
        let itemID: UUID
        let requestedRevision: UInt64
        let currentRevision: UInt64?
        let reason: String
        private enum CodingKeys: String, CodingKey {
            case reason
            case itemID = "item_id"
            case requestedRevision = "requested_revision"
            case currentRevision = "current_revision"
        }
    }

    struct Plan: Codable, Equatable, Sendable {
        /// Exact occurrence metadata returned by the scheduler. The timestamp
        /// strings deliberately remain strings instead of being round-tripped
        /// through `Date`: a later cross-horizon move must send the exact
        /// server-issued nominal envelope back to the scheduler.
        struct Occurrence: Codable, Equatable, Sendable {
            let id: UUID
            let seriesItemID: UUID
            let identity: RecurrenceOccurrenceIdentity
            let nominalStart: String
            let nominalEnd: String
            let windowStart: String
            let windowEnd: String
            let localDate: String?
            let ordinal: UInt32
            let state: String

            private enum CodingKeys: String, CodingKey {
                case id, identity, ordinal, state
                case seriesItemID = "series_item_id"
                case nominalStart = "nominal_start"
                case nominalEnd = "nominal_end"
                case windowStart = "window_start"
                case windowEnd = "window_end"
                case localDate = "local_date"
            }
        }

        struct Block: Codable, Equatable, Identifiable, Sendable {
            struct Explanation: Codable, Equatable, Sendable {
                let code: String
                let message: String
            }
            let id: UUID
            let isSensitive: Bool
            let itemID: UUID?
            let occurrenceID: UUID?
            let externalBlockID: UUID?
            let title: String
            let start: Date
            let end: Date
            let sessionIndex: UInt16
            let kind: String
            let explanations: [Explanation]
            private enum CodingKeys: String, CodingKey {
                case id, title, start, end, kind, explanations
                case isSensitive = "is_sensitive"
                case itemID = "item_id"
                case occurrenceID = "occurrence_id"
                case externalBlockID = "external_block_id"
                case sessionIndex = "session_index"
            }
        }

        struct Unscheduled: Codable, Equatable, Sendable {
            let itemID: UUID
            let occurrenceID: UUID?
            let remaining: UInt32
            let reason: String
            let message: String
            private enum CodingKeys: String, CodingKey {
                case remaining, reason, message
                case itemID = "item_id"
                case occurrenceID = "occurrence_id"
            }
        }

        struct Score: Codable, Equatable, Sendable {
            let scheduledMinutes: UInt32
            let unscheduledMinutes: UInt32
            let softPenalty: UInt64
            let movedMinutes: UInt32
            private enum CodingKeys: String, CodingKey {
                case scheduledMinutes = "scheduled_minutes"
                case unscheduledMinutes = "unscheduled_minutes"
                case softPenalty = "soft_penalty"
                case movedMinutes = "moved_minutes"
            }
        }

        let asOf: Date
        let horizonStart: Date
        let horizonEnd: Date
        let blocks: [Block]
        let unscheduled: [Unscheduled]
        let decisions: [JSONValue]
        let violations: [JSONValue]
        let score: Score
        let occurrences: [Occurrence]

        private enum CodingKeys: String, CodingKey {
            case blocks, unscheduled, decisions, violations, score, occurrences
            case asOf = "as_of"
            case horizonStart = "horizon_start"
            case horizonEnd = "horizon_end"
        }
    }

    let inputDigest: String
    let sourceItemCount: Int
    let acceptedItemCount: Int
    let sourceItemRevisions: [UUID: UInt64]
    let rejectedItems: [RejectedItem]
    let ignoredPreviousAssignments: [IgnoredAssignment]
    let plan: Plan

    private enum CodingKeys: String, CodingKey {
        case plan
        case inputDigest = "input_digest"
        case sourceItemCount = "source_item_count"
        case acceptedItemCount = "accepted_item_count"
        case sourceItemRevisions = "source_item_revisions"
        case rejectedItems = "rejected_items"
        case ignoredPreviousAssignments = "ignored_previous_assignments"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        inputDigest = try container.decode(String.self, forKey: .inputDigest)
        sourceItemCount = try container.decode(Int.self, forKey: .sourceItemCount)
        acceptedItemCount = try container.decode(Int.self, forKey: .acceptedItemCount)
        rejectedItems = try container.decode([RejectedItem].self, forKey: .rejectedItems)
        ignoredPreviousAssignments = try container.decode(
            [IgnoredAssignment].self,
            forKey: .ignoredPreviousAssignments
        )
        plan = try container.decode(Plan.self, forKey: .plan)

        let raw = try container.decode([String: UInt64].self, forKey: .sourceItemRevisions)
        guard sourceItemCount >= 0,
              acceptedItemCount >= 0,
              raw.count == sourceItemCount,
              acceptedItemCount <= sourceItemCount else {
            throw DecodingError.dataCorruptedError(
                forKey: .sourceItemRevisions,
                in: container,
                debugDescription: "source_item_revisions is not a complete source map"
            )
        }
        var revisions: [UUID: UInt64] = [:]
        for (key, revision) in raw {
            guard let id = UUID(uuidString: key), revisions[id] == nil else {
                throw DecodingError.dataCorruptedError(
                    forKey: .sourceItemRevisions,
                    in: container,
                    debugDescription: "source_item_revisions contains an invalid or duplicate UUID"
                )
            }
            revisions[id] = revision
        }
        sourceItemRevisions = revisions
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(inputDigest, forKey: .inputDigest)
        try container.encode(sourceItemCount, forKey: .sourceItemCount)
        try container.encode(acceptedItemCount, forKey: .acceptedItemCount)
        try container.encode(rejectedItems, forKey: .rejectedItems)
        try container.encode(ignoredPreviousAssignments, forKey: .ignoredPreviousAssignments)
        try container.encode(plan, forKey: .plan)
        let revisions = Dictionary(uniqueKeysWithValues: sourceItemRevisions.map {
            ($0.key.uuidString.lowercased(), $0.value)
        })
        try container.encode(revisions, forKey: .sourceItemRevisions)
    }
}

struct DayWeaveSchedulePublishRequest: Codable, Equatable, Sendable {
    let idempotencyKey: UUID
    let expectedInputDigest: String
    let schedule: DayWeaveSchedulePreviewRequest

    private enum CodingKeys: String, CodingKey {
        case schedule
        case idempotencyKey = "idempotency_key"
        case expectedInputDigest = "expected_input_digest"
    }
}

struct DayWeavePreparedSchedulePublication: Codable, Equatable, Sendable {
    let request: DayWeaveSchedulePublishRequest
    let body: Data
    let bodySHA256: String

    private enum CodingKeys: String, CodingKey {
        case request, body
        case bodySHA256 = "body_sha256"
    }
}

struct DayWeavePublishedScheduleRevision: Decodable, Equatable, Sendable {
    let id: UUID
    let revision: String
    let revisionNumber: UInt64
    let inputDigest: String
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
    let publishedAt: Date

    private enum CodingKeys: String, CodingKey {
        case id, revision
        case revisionNumber = "revision_number"
        case inputDigest = "input_digest"
        case horizonStart = "horizon_start"
        case horizonEnd = "horizon_end"
        case timezoneName = "timezone_name"
        case publishedAt = "published_at"
    }
}

struct DayWeaveSchedulePublishResponse: Decodable, Equatable, Sendable {
    let revision: DayWeavePublishedScheduleRevision
    let replayed: Bool
}

struct DayWeavePublishedScheduleBlockProof: Codable, Equatable, Sendable {
    let id: UUID
    let itemID: UUID
    let itemRevision: UInt64
    let occurrenceID: UUID?
    let sessionIndex: UInt16
    let start: Date
    let end: Date
    let kind: String

    init?(
        block: DayWeaveSchedulePreview.Plan.Block,
        sourceItemRevisions: [UUID: UInt64]
    ) {
        guard let itemID = block.itemID,
              let itemRevision = sourceItemRevisions[itemID] else { return nil }
        self.init(
            id: block.id,
            itemID: itemID,
            itemRevision: itemRevision,
            occurrenceID: block.occurrenceID,
            sessionIndex: block.sessionIndex,
            start: block.start,
            end: block.end,
            kind: block.kind
        )
    }

    init(
        id: UUID,
        itemID: UUID,
        itemRevision: UInt64,
        occurrenceID: UUID?,
        sessionIndex: UInt16,
        start: Date,
        end: Date,
        kind: String
    ) {
        self.id = id
        self.itemID = itemID
        self.itemRevision = itemRevision
        self.occurrenceID = occurrenceID
        self.sessionIndex = sessionIndex
        self.start = start
        self.end = end
        self.kind = kind
    }

    var hasValidShape: Bool {
        !id.isDayWeavePublicationNil
            && !itemID.isDayWeavePublicationNil
            && itemRevision > 0
            && start.timeIntervalSinceReferenceDate.isFinite
            && end.timeIntervalSinceReferenceDate.isFinite
            && start < end
            && !kind.isEmpty
            && kind.utf8.count <= 128
            && !kind.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }

    func matches(_ block: ScheduleBlock) -> Bool {
        hasValidShape
            && block.id == id
            && block.sourceItemID == itemID
            && block.sourceItemRevision == itemRevision
            && block.occurrenceID == occurrenceID
            && block.sessionIndex == sessionIndex
            && block.syncOrigin == .canonicalPreview
            && block.previewKind == kind
            && sameInstant(block.start, start)
            && sameInstant(block.end, end)
    }

    private func sameInstant(_ left: Date, _ right: Date) -> Bool {
        abs(left.timeIntervalSince(right)) <= 0.002
    }
}

/// Durable positive evidence for the exact schedule installed after a
/// validated, non-replayed publication response. The server receipt does not
/// carry the compose `as_of` value or local API binding, so both are retained
/// beside it instead of being inferred again after a restart.
struct DayWeavePublishedScheduleProof: Codable, Equatable, Sendable {
    static let currentVersion = 1

    let version: Int
    let configurationIdentifier: String
    let revisionID: UUID
    let revision: String
    let revisionNumber: UInt64
    let inputDigest: String
    let asOf: Date
    let horizonStart: Date
    let horizonEnd: Date
    let timezoneName: String
    let publishedAt: Date
    let publishedBlocks: [DayWeavePublishedScheduleBlockProof]

    init?(
        publication: PendingSchedulePublication,
        revision: DayWeavePublishedScheduleRevision
    ) {
        let request = publication.preparedRequest.request
        let publishedBlocks = publication.preview.plan.blocks.compactMap {
            DayWeavePublishedScheduleBlockProof(
                block: $0,
                sourceItemRevisions: publication.preview.sourceItemRevisions
            )
        }.sorted { $0.id.uuidString < $1.id.uuidString }
        self.init(
            configurationIdentifier: publication.configurationIdentifier,
            revisionID: revision.id,
            revision: revision.revision,
            revisionNumber: revision.revisionNumber,
            inputDigest: revision.inputDigest,
            asOf: request.schedule.asOf,
            horizonStart: revision.horizonStart,
            horizonEnd: revision.horizonEnd,
            timezoneName: revision.timezoneName,
            publishedAt: revision.publishedAt,
            publishedBlocks: publishedBlocks
        )
        let canonicalPreviewBlockCount = publication.preview.plan.blocks.count {
            $0.itemID != nil
        }
        guard publishedBlocks.count == canonicalPreviewBlockCount,
              inputDigest == request.expectedInputDigest,
              inputDigest == publication.preview.inputDigest,
              sameInstant(horizonStart, request.schedule.horizonStart),
              sameInstant(horizonEnd, request.schedule.horizonEnd),
              timezoneName == request.schedule.timezoneName,
              matches(publication.provenance) else { return nil }
    }

    init(
        version: Int = Self.currentVersion,
        configurationIdentifier: String,
        revisionID: UUID,
        revision: String,
        revisionNumber: UInt64,
        inputDigest: String,
        asOf: Date,
        horizonStart: Date,
        horizonEnd: Date,
        timezoneName: String,
        publishedAt: Date,
        publishedBlocks: [DayWeavePublishedScheduleBlockProof]
    ) {
        self.version = version
        self.configurationIdentifier = configurationIdentifier
        self.revisionID = revisionID
        self.revision = revision
        self.revisionNumber = revisionNumber
        self.inputDigest = inputDigest
        self.asOf = asOf
        self.horizonStart = horizonStart
        self.horizonEnd = horizonEnd
        self.timezoneName = timezoneName
        self.publishedAt = publishedAt
        self.publishedBlocks = publishedBlocks
    }

    var hasValidShape: Bool {
        let digestPrefix = "sha256:"
        let digest = inputDigest.dropFirst(digestPrefix.count)
        let expectedRevision = "\(revisionNumber):\(revisionID.uuidString.lowercased())"
        return version == Self.currentVersion
            && !configurationIdentifier.isEmpty
            && configurationIdentifier.utf8.count <= 4_096
            && !configurationIdentifier.unicodeScalars.contains(
                where: CharacterSet.controlCharacters.contains
            )
            && !revisionID.isDayWeavePublicationNil
            && revisionNumber > 0
            && revision == expectedRevision
            && inputDigest.hasPrefix(digestPrefix)
            && digest.count == 64
            && digest.utf8.allSatisfy {
                (48...57).contains($0) || (97...102).contains($0)
            }
            && asOf.timeIntervalSinceReferenceDate.isFinite
            && horizonStart.timeIntervalSinceReferenceDate.isFinite
            && horizonEnd.timeIntervalSinceReferenceDate.isFinite
            && publishedAt.timeIntervalSinceReferenceDate.isFinite
            && horizonStart <= asOf
            && asOf < horizonEnd
            && TimeZone(identifier: timezoneName) != nil
            && publishedBlocks.count <= 10_000
            && publishedBlocks.allSatisfy {
                $0.hasValidShape
                    && $0.start < horizonEnd
                    && horizonStart < $0.end
            }
            && Set(publishedBlocks.map(\.id)).count == publishedBlocks.count
    }

    func matches(_ provenance: SchedulePreviewProvenance) -> Bool {
        hasValidShape
            && configurationIdentifier == provenance.configurationIdentifier
            && sameInstant(asOf, provenance.asOf)
            && sameInstant(horizonStart, provenance.horizonStart)
            && sameInstant(horizonEnd, provenance.horizonEnd)
            && timezoneName == provenance.timezoneName
    }

    func matchesPublishedPlan(_ blocks: [ScheduleBlock]) -> Bool {
        guard hasValidShape else { return false }
        let canonicalBlocks = blocks.filter { $0.syncOrigin == .canonicalPreview }
        guard canonicalBlocks.count == publishedBlocks.count else { return false }
        var blockByID: [UUID: ScheduleBlock] = [:]
        for block in canonicalBlocks {
            guard blockByID.updateValue(block, forKey: block.id) == nil else {
                return false
            }
        }
        return publishedBlocks.allSatisfy { proof in
            blockByID[proof.id].map(proof.matches) == true
        }
    }

    func matches(_ block: ScheduleBlock) -> Bool {
        guard hasValidShape,
              let proof = publishedBlocks.first(where: { $0.id == block.id }) else {
            return false
        }
        return proof.matches(block)
    }

    func rebindingConfigurationIdentifier(
        _ configurationIdentifier: String
    ) -> DayWeavePublishedScheduleProof {
        .init(
            version: version,
            configurationIdentifier: configurationIdentifier,
            revisionID: revisionID,
            revision: revision,
            revisionNumber: revisionNumber,
            inputDigest: inputDigest,
            asOf: asOf,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            timezoneName: timezoneName,
            publishedAt: publishedAt,
            publishedBlocks: publishedBlocks
        )
    }

    private func sameInstant(_ left: Date, _ right: Date) -> Bool {
        abs(left.timeIntervalSince(right)) <= 0.002
    }
}

private extension UUID {
    var isDayWeavePublicationNil: Bool {
        self == UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
    }
}

/// Encrypted with the planner snapshot before the first publication request is
/// sent. Retaining both the exact compose input and its accepted preview lets a
/// restart replay one idempotency tuple and finish the local commit without
/// inventing a new schedule.
struct PendingSchedulePublication: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumEncodedBytes = 8 * 1_048_576
    static let maximumMessageBytes = 8 * 1_024

    let version: Int
    let configurationIdentifier: String
    let preparedRequest: DayWeavePreparedSchedulePublication
    let preview: DayWeaveSchedulePreview
    let message: String
    let provenance: SchedulePreviewProvenance
    let preparedAt: Date

    init(
        version: Int = Self.currentVersion,
        configurationIdentifier: String,
        preparedRequest: DayWeavePreparedSchedulePublication,
        preview: DayWeaveSchedulePreview,
        message: String,
        provenance: SchedulePreviewProvenance,
        preparedAt: Date
    ) {
        self.version = version
        self.configurationIdentifier = configurationIdentifier
        self.preparedRequest = preparedRequest
        self.preview = preview
        self.message = message
        self.provenance = provenance
        self.preparedAt = preparedAt
    }

    var isWithinEncodedSizeLimit: Bool {
        guard message.utf8.count <= Self.maximumMessageBytes else { return false }
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        return (try? encoder.encode(self).count).map { $0 <= Self.maximumEncodedBytes } ?? false
    }
}

private struct DynamicCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil
    init(_ value: String) { stringValue = value }
    init?(stringValue: String) { self.init(stringValue) }
    init?(intValue: Int) { return nil }
}

extension JSONEncoder {
    fileprivate static var dayWeave: JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            try container.encode(formatter.string(from: date))
        }
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }
}

extension JSONDecoder {
    fileprivate static var dayWeave: JSONDecoder { JSONDecoder() }
}
