import Foundation

/// Lossless normalized helper input. In particular, timestamps remain strings
/// instead of taking a potentially lossy round-trip through Foundation Date.
struct RoutinePlanningScheduleInput: Codable, Equatable, Sendable {
    let fields: [String: JSONValue]
    private static let required: Set<String> = ["as_of", "horizon_start", "horizon_end", "timezone_name"]
    private static let optional: Set<String> = ["availability", "fixed_blocks", "previous_assignments", "manual_placements", "manual_placement_releases", "config", "recurrence_context"]
    init(previewRequest: DayWeaveSchedulePreviewRequest) throws {
        let bytes = try SchedulerHelperClient.encoder.encode(previewRequest)
        self = try RoutinePlanningWitnessValidation.decode(Self.self, from: bytes)
    }
    init(from decoder: any Decoder) throws {
        let value = try JSONValue(from: decoder)
        fields = try RoutinePlanningShape.object(value, required: Self.required, optional: Self.optional)
        try validate()
    }
    private static func effectiveFields(_ input: [String: JSONValue]) throws -> [String: JSONValue] {
        var fields = try RoutinePlanningShape.object(.object(input), required: Self.required, optional: Self.optional)
        for name in ["availability", "fixed_blocks", "previous_assignments", "manual_placements", "manual_placement_releases"] {
            if fields[name] == nil { fields[name] = .array([]) }
        }
        if fields["config"] == nil { fields["config"] = .object(["slot_granularity_minutes": .number(5), "stability_weight": .number(4), "default_soft_weight": .number(100)]) }
        fields["recurrence_context"] = .object(try Self.contextDefaults(fields["recurrence_context"] ?? .object([:])))
        return fields
    }
    func encode(to encoder: any Encoder) throws { try validate(); try fields.encode(to: encoder) }

    func validate() throws {
        let object = try Self.effectiveFields(fields)
        let start = try RoutinePlanningShape.instant(object["horizon_start"]), end = try RoutinePlanningShape.instant(object["horizon_end"])
        let asOf = try RoutinePlanningShape.instant(object["as_of"])
        try RoutinePlanningWitnessValidation.require(start < end && end - start <= 90 * 86_400 * 1_000_000 && asOf <= end)
        let zone = try RoutinePlanningShape.string(object["timezone_name"], maximum: 100)
        try RoutinePlanningWitnessValidation.require(TimeZone(identifier: zone) != nil)
        let config = try RoutinePlanningShape.object(object["config"], required: ["slot_granularity_minutes", "stability_weight", "default_soft_weight"])
        _ = try RoutinePlanningShape.unsigned(config["slot_granularity_minutes"], maximum: UInt64(UInt32.max), minimum: 1)
        for key in ["stability_weight", "default_soft_weight"] { _ = try RoutinePlanningShape.unsigned(config[key], maximum: 1_000_000) }
        for raw in try RoutinePlanningShape.array(object["availability"]) {
            let row = try RoutinePlanningShape.object(raw, required: ["start", "end", "contexts", "energy"], optional: ["location"])
            try RoutinePlanningShape.window(row)
            let contexts = try RoutinePlanningShape.array(row["contexts"])
            let names = try contexts.map { try RoutinePlanningShape.string($0, maximum: 500) }
            try RoutinePlanningWitnessValidation.require(Set(names).count == names.count)
            if row["location"] != nil && row["location"] != .null { _ = try RoutinePlanningShape.string(row["location"], maximum: 500) }
            try RoutinePlanningWitnessValidation.require(["low", "medium", "deep"].contains(try RoutinePlanningShape.string(row["energy"])))
        }
        var fixedIDs = Set<UUID>()
        for raw in try RoutinePlanningShape.array(object["fixed_blocks"]) {
            let row = try RoutinePlanningShape.object(raw, required: ["id", "is_sensitive", "title", "start", "end", "source"])
            let id = try RoutinePlanningShape.uuid(row["id"])
            try RoutinePlanningWitnessValidation.require(fixedIDs.insert(id).inserted)
            try RoutinePlanningShape.window(row); try RoutinePlanningShape.bool(row["is_sensitive"])
            _ = try RoutinePlanningShape.string(row["title"], maximum: 500)
            try RoutinePlanningWitnessValidation.require(["google_calendar", "sleep", "protected_time", "travel", "manual"].contains(try RoutinePlanningShape.string(row["source"])))
        }
        var blockCount = 0
        for raw in try RoutinePlanningShape.array(object["previous_assignments"]) {
            let row = try RoutinePlanningShape.object(raw, required: ["item_id", "item_revision", "blocks", "pinned"], optional: ["occurrence_id"])
            try Self.assignment(row, blockCount: &blockCount); try RoutinePlanningShape.bool(row["pinned"])
        }
        var manualCount = 0
        for raw in try RoutinePlanningShape.array(object["manual_placements"], maximum: 64) {
            let row = try RoutinePlanningShape.object(raw, required: ["id", "assignments"], optional: ["source_schedule_revision_id"])
            _ = try RoutinePlanningShape.uuid(row["id"])
            try RoutinePlanningShape.optionalUUID(row["source_schedule_revision_id"])
            for rawAssignment in try RoutinePlanningShape.array(row["assignments"], maximum: 128) {
                manualCount += 1
                try RoutinePlanningWitnessValidation.require(manualCount <= 128)
                let assignment = try RoutinePlanningShape.object(rawAssignment, required: ["item_id", "item_revision", "blocks"], optional: ["occurrence_id"])
                try Self.assignment(assignment, blockCount: &blockCount)
            }
        }
        for raw in try RoutinePlanningShape.array(object["manual_placement_releases"], maximum: 64) {
            let row = try RoutinePlanningShape.object(raw, required: ["id", "placement_id", "source_schedule_revision_id"])
            for key in row.keys { _ = try RoutinePlanningShape.uuid(row[key]) }
        }
        try Self.validateContext(try Self.contextDefaults(object["recurrence_context"]))
    }

    /// Only the server-owned recurrence fields and retained assignments may
    /// differ. Immutable constraints are compared without JSON key-order or
    /// equivalent timestamp-spelling assumptions.
    func requireNormalization(of original: Self) throws {
        try validate(); try original.validate()
        let currentFields = try Self.effectiveFields(fields), originalFields = try Self.effectiveFields(original.fields)
        for key in ["manual_placements", "manual_placement_releases"] {
            try RoutinePlanningWitnessValidation.require(currentFields[key] == .array([]) && originalFields[key] == .array([]))
        }
        for key in Self.required.union(Self.optional).subtracting(["previous_assignments", "recurrence_context"]) {
            try RoutinePlanningWitnessValidation.require(try Self.comparison(key, currentFields[key]) == Self.comparison(key, originalFields[key]))
        }
        let current = try Self.contextDefaults(currentFields["recurrence_context"]), prior = try Self.contextDefaults(originalFields["recurrence_context"])
        for key in ["calendar", "rolling_anchors", "minimum_spacing"] {
            try RoutinePlanningWitnessValidation.require(try Self.comparison(key, current[key]) == Self.comparison(key, prior[key]))
        }
    }

    private static func assignment(_ row: [String: JSONValue], blockCount: inout Int) throws {
        _ = try RoutinePlanningShape.uuid(row["item_id"])
        _ = try RoutinePlanningShape.unsigned(row["item_revision"], minimum: 1)
        try RoutinePlanningShape.optionalUUID(row["occurrence_id"])
        for raw in try RoutinePlanningShape.array(row["blocks"], maximum: 50_000) {
            blockCount += 1; try RoutinePlanningWitnessValidation.require(blockCount <= 50_000)
            let block = try RoutinePlanningShape.object(raw, required: ["start", "end", "session_index"])
            try RoutinePlanningShape.window(block)
            _ = try RoutinePlanningShape.unsigned(block["session_index"], maximum: UInt64(UInt16.max))
        }
    }
    private static func contextDefaults(_ value: JSONValue?) throws -> [String: JSONValue] {
        let keys: Set<String> = ["calendar", "completion_anchors", "rolling_anchors", "minimum_spacing", "completed_occurrence_ids", "partial_progress", "pauses", "exceptions"]
        var result = try RoutinePlanningShape.object(value, required: [], optional: keys)
        for key in ["completion_anchors", "rolling_anchors", "minimum_spacing", "partial_progress"] {
            if result[key] == nil { result[key] = .object([:]) }
        }
        for key in ["completed_occurrence_ids", "pauses", "exceptions"] { if result[key] == nil { result[key] = .array([]) } }
        if result["calendar"] == nil { result["calendar"] = .object(["time_zone_id": .null, "week_starts_on": .string("monday"), "days": .array([])]) }
        return result
    }
    private static func validateContext(_ context: [String: JSONValue]) throws {
        let calendar = try RoutinePlanningShape.object(context["calendar"], required: ["time_zone_id", "week_starts_on", "days"])
        if calendar["time_zone_id"] != .null { _ = try RoutinePlanningShape.string(calendar["time_zone_id"], maximum: 100) }
        try RoutinePlanningWitnessValidation.require(["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"].contains(try RoutinePlanningShape.string(calendar["week_starts_on"])))
        for raw in try RoutinePlanningShape.array(calendar["days"], maximum: 92) {
            let day = try RoutinePlanningShape.object(raw, required: ["local_date", "start", "end"])
            try RoutinePlanningShape.window(day); try RoutinePlanningShape.dayTuple(day["local_date"])
        }
        for key in ["completion_anchors", "rolling_anchors", "minimum_spacing", "partial_progress"] {
            let map = try RoutinePlanningShape.map(context[key]); var ids = Set<UUID>()
            for (id, value) in map {
                try RoutinePlanningWitnessValidation.require(ids.insert(try RoutinePlanningWitnessValidation.uuid(id)).inserted)
                if key == "minimum_spacing" { _ = try RoutinePlanningShape.unsigned(value, maximum: UInt64(UInt32.max)) }
                else if key == "partial_progress" {
                    let progress = try RoutinePlanningShape.object(value, required: ["progress_basis_points", "expected_duration_minutes"], optional: ["remaining_duration_minutes"])
                    _ = try RoutinePlanningShape.unsigned(progress["progress_basis_points"], maximum: 9_999, minimum: 1)
                    _ = try RoutinePlanningShape.unsigned(progress["expected_duration_minutes"], maximum: UInt64(UInt32.max), minimum: 1)
                    if progress["remaining_duration_minutes"] != nil { _ = try RoutinePlanningShape.unsigned(progress["remaining_duration_minutes"], maximum: UInt64(UInt32.max), minimum: 1) }
                } else { _ = try RoutinePlanningShape.instant(value) }
            }
        }
        var completed = Set<UUID>()
        for raw in try RoutinePlanningShape.array(context["completed_occurrence_ids"]) {
            try RoutinePlanningWitnessValidation.require(completed.insert(try RoutinePlanningShape.uuid(raw)).inserted)
        }
        for raw in try RoutinePlanningShape.array(context["pauses"]) {
            let pause = try RoutinePlanningShape.object(raw, required: ["item_id", "start", "end"])
            _ = try RoutinePlanningShape.uuid(pause["item_id"]); try RoutinePlanningShape.window(pause)
        }
        for raw in try RoutinePlanningShape.array(context["exceptions"]) {
            let row = try RoutinePlanningShape.object(raw, required: ["item_id", "selector", "action"])
            _ = try RoutinePlanningShape.uuid(row["item_id"])
            let selector = try RoutinePlanningShape.map(row["selector"])
            switch try RoutinePlanningShape.string(selector["type"]) {
            case "occurrence": _ = try RoutinePlanningShape.object(row["selector"], required: ["type", "id"]); _ = try RoutinePlanningShape.uuid(selector["id"])
            case "local_date": _ = try RoutinePlanningShape.object(row["selector"], required: ["type", "date"]); try RoutinePlanningShape.dayTuple(selector["date"])
            case "nominal_start": _ = try RoutinePlanningShape.object(row["selector"], required: ["type", "at"]); _ = try RoutinePlanningShape.instant(selector["at"])
            default: throw RoutinePlanningWitnessError.invalidData
            }
            let action = try RoutinePlanningShape.map(row["action"])
            switch try RoutinePlanningShape.string(action["type"]) {
            case "skip": _ = try RoutinePlanningShape.object(row["action"], required: ["type"])
            case "move":
                _ = try RoutinePlanningShape.object(row["action"], required: ["type", "start", "end", "source"])
                try RoutinePlanningShape.window(action)
                let source = try RoutinePlanningShape.object(action["source"], required: ["item_revision", "identity", "nominal_start", "nominal_end", "local_date", "ordinal"])
                _ = try RoutinePlanningShape.unsigned(source["item_revision"], minimum: 1)
                _ = try RoutinePlanningShape.unsigned(source["ordinal"], maximum: UInt64(UInt32.max))
                try RoutinePlanningWitnessValidation.require(try RoutinePlanningShape.instant(source["nominal_start"]) < RoutinePlanningShape.instant(source["nominal_end"]))
                if source["local_date"] != .null { try RoutinePlanningShape.isoDate(source["local_date"]) }
                _ = try RoutinePlanningWitnessValidation.decode(RecurrenceOccurrenceIdentity.self, from: RoutinePlanningWitnessValidation.encode(source["identity"]))
            default: throw RoutinePlanningWitnessError.invalidData
            }
        }
    }
    private static func comparison(_ key: String, _ value: JSONValue?) throws -> JSONValue {
        if ["as_of", "horizon_start", "horizon_end", "start", "end"].contains(key) {
            return .number(.init(integerLiteral: try RoutinePlanningShape.instant(value)))
        }
        if key == "rolling_anchors" {
            return .object(try RoutinePlanningShape.map(value).mapValues { .number(.init(integerLiteral: try RoutinePlanningShape.instant($0))) })
        }
        if case let .array(rows)? = value, ["availability", "fixed_blocks", "days"].contains(key) {
            return .array(try rows.map { raw in
                var row = try RoutinePlanningShape.map(raw)
                for time in ["start", "end"] { row[time] = try comparison(time, row[time]) }
                if key == "availability" {
                    if row["location"] == nil { row["location"] = .null }
                    row["contexts"] = .array(try RoutinePlanningShape.array(row["contexts"]).map { try RoutinePlanningShape.string($0) }.sorted().map(JSONValue.string))
                }
                return .object(row)
            })
        }
        if key == "calendar" {
            var calendar = try RoutinePlanningShape.map(value)
            calendar["days"] = try comparison("days", calendar["days"])
            return .object(calendar)
        }
        guard let value else { throw RoutinePlanningWitnessError.invalidData }
        return value
    }
}

enum RoutinePlanningShape {
    static func map(_ value: JSONValue?) throws -> [String: JSONValue] {
        guard case let .object(value)? = value, value.count <= 10_000 else { throw RoutinePlanningWitnessError.invalidData }
        return value
    }
    static func object(_ value: JSONValue?, required: Set<String>, optional: Set<String> = []) throws -> [String: JSONValue] {
        let result = try map(value), keys = Set(result.keys)
        try RoutinePlanningWitnessValidation.require(required.isSubset(of: keys) && keys.isSubset(of: required.union(optional)))
        return result
    }
    static func array(_ value: JSONValue?, maximum: Int = 10_000) throws -> [JSONValue] {
        guard case let .array(value)? = value, value.count <= maximum else { throw RoutinePlanningWitnessError.invalidData }
        return value
    }
    static func string(_ value: JSONValue?, maximum: Int = 4_096) throws -> String {
        guard case let .string(value)? = value, !value.isEmpty, value.utf8.count <= maximum else { throw RoutinePlanningWitnessError.invalidData }
        return value
    }
    static func bool(_ value: JSONValue?) throws {
        guard case .bool? = value else { throw RoutinePlanningWitnessError.invalidData }
    }
    static func unsigned(_ value: JSONValue?, maximum: UInt64 = UInt64(Int64.max), minimum: UInt64 = 0) throws -> UInt64 {
        guard case let .number(number)? = value, let integer = UInt64(number.displayDescription), integer >= minimum && integer <= maximum else { throw RoutinePlanningWitnessError.invalidData }
        return integer
    }
    static func uuid(_ value: JSONValue?) throws -> UUID { try RoutinePlanningWitnessValidation.uuid(string(value)) }
    static func optionalUUID(_ value: JSONValue?) throws { if let value, value != .null { _ = try uuid(value) } }
    static func instant(_ value: JSONValue?) throws -> Int64 {
        let raw = try string(value, maximum: 40)
        guard let parsed = CanonicalRFC3339Instant(raw), parsed.hasPostgresPrecision else { throw RoutinePlanningWitnessError.invalidData }
        return parsed.microsecondsSinceUnixEpoch
    }
    static func window(_ row: [String: JSONValue]) throws {
        try RoutinePlanningWitnessValidation.require(try instant(row["start"]) < instant(row["end"]))
    }
    static func isoDate(_ value: JSONValue?) throws {
        try RoutinePlanningWitnessValidation.require(RecurrenceMoveSource.isValidLocalDate(try string(value, maximum: 10)))
    }
    static func dayTuple(_ value: JSONValue?) throws {
        let date = try array(value, maximum: 2)
        try RoutinePlanningWitnessValidation.require(date.count == 2)
        let year = try unsigned(date[0], maximum: 9_999, minimum: 1)
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
        _ = try unsigned(date[1], maximum: leap ? 366 : 365, minimum: 1)
    }
}
