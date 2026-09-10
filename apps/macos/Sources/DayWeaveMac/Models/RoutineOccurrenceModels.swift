import Foundation

enum RoutineOccurrenceError: Error, Equatable, Sendable {
    case invalidData, unavailable
    case definitive(String)
}

enum RoutineOccurrenceValidation {
    static let maximumMembers = 10_000
    static let maximumBytes = 8 * 1_024 * 1_024
    static let maximumRequestBytes = 1_024 * 1_024
    static let maximumRevision = UInt64(Int64.max)
    static let nilID = ItemCompletionValidation.nilID
    static func revision(_ value: UInt64) -> Bool { value > 0 && value <= maximumRevision }
    static func keys(_ decoder: any Decoder, _ names: Set<String>) throws {
        try ItemProgressValidation.keys(decoder, names)
    }
    static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        guard data.count <= maximumBytes,
              StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: data) else { throw RoutineOccurrenceError.invalidData }
        do { return try JSONDecoder().decode(type, from: data) }
        catch { throw RoutineOccurrenceError.invalidData }
    }
    static func require(_ valid: Bool) throws { if !valid { throw RoutineOccurrenceError.invalidData } }
    static func fitsSize<T: Encodable>(_ value: T) -> Bool {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
        return (try? encoder.encode(value).count).map { $0 <= maximumBytes } == true
    }
    static func title(_ value: String) -> Bool { ItemProgressValidation.text(value, limit: 500) }
    static func cursor(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 512 && value.utf8.allSatisfy { (33...126).contains($0) }
    }
    static func anchor(_ value: String) -> Bool {
        guard value.utf8.count <= 40,
              value.range(of: #"\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?(Z|[+-][0-9]{2}:[0-9]{2})\z"#, options: .regularExpression) != nil else { return false }
        if value.hasSuffix("Z") { return ItemCompletionValidation.timestamp(value) }
        let offset = String(value.suffix(6))
        guard let hours = Int(offset.dropFirst().prefix(2)), let minutes = Int(offset.suffix(2)), hours <= 23, minutes <= 59 else { return false }
        return ItemCompletionValidation.timestamp(String(value.dropLast(6)) + "Z")
    }
    static let definitiveStatuses: [String: Int] = [
        "routine_occurrence_invalid": 422, "routine_occurrence_too_large": 413,
        "routine_occurrence_definition_changed": 409, "routine_occurrence_source_ineligible": 409,
        "routine_occurrence_instance_stale": 409, "routine_occurrence_member_stale": 409,
        "routine_occurrence_evidence_stale": 409, "routine_occurrence_member_missing": 404,
        "routine_occurrence_missing": 404, "routine_occurrence_operation_reused": 409,
        "routine_occurrence_invalid_cursor": 409, "routine_occurrence_leaf_required": 422,
        "routine_occurrence_parent_required": 422, "routine_occurrence_evidence_required": 409,
        "routine_occurrence_execution_conflict": 409,
    ]
}

enum RoutineOccurrenceKind: String, Codable, Sendable { case task, project, goal, routine, habit, `break`, event }
enum RoutineOccurrenceStatus: String, Codable, Sendable {
    case inbox, planned, blocked, completed, skipped, cancelled
    var isOpen: Bool { self == .inbox || self == .planned || self == .blocked }
}
enum RoutineOccurrenceReason: String, Codable, Sendable {
    case unchanged, outcomeRecorded = "outcome_recorded", reopened, policyReviewed = "policy_reviewed"
    case occurrenceEvidenceRequired = "occurrence_evidence_required"
    case automaticallyCompleted = "automatically_completed", automaticallyReopened = "automatically_reopened"
    case manuallyCompleted = "manually_completed", manuallyKeptOpen = "manually_kept_open"
    case manualCompletionReleased = "manual_completion_released"
}

struct RoutineOccurrenceMemberDefinition: Equatable, Sendable {
    let itemID: UUID
    let parentID: UUID?
    let sourceRevision: UInt64
    let title: String
    let kind: RoutineOccurrenceKind
    let recurs: Bool
    let siblingOrder: UInt32
    let requiredForParent: Bool
    let initialOpen: ItemCompletionReopenState
    var isValid: Bool {
        itemID != RoutineOccurrenceValidation.nilID && parentID != itemID && parentID != RoutineOccurrenceValidation.nilID
            && RoutineOccurrenceValidation.revision(sourceRevision) && RoutineOccurrenceValidation.title(title)
            && siblingOrder <= 1_000_000 && (kind != .habit || recurs) && initialOpen.isValid(for: itemID)
    }
}
extension RoutineOccurrenceMemberDefinition: Codable {
    private enum CodingKeys: String, CodingKey {
        case itemID = "item_id", parentID = "parent_id", sourceRevision = "source_revision", title, kind, recurs
        case siblingOrder = "sibling_order", requiredForParent = "required_for_parent", initialOpen = "initial_open"
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["item_id", "parent_id", "source_revision", "title", "kind", "recurs", "sibling_order", "required_for_parent", "initial_open"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        itemID = try c.decode(UUID.self, forKey: .itemID); parentID = try c.decodeIfPresent(UUID.self, forKey: .parentID)
        sourceRevision = try c.decode(UInt64.self, forKey: .sourceRevision); title = try c.decode(String.self, forKey: .title)
        kind = try c.decode(RoutineOccurrenceKind.self, forKey: .kind); recurs = try c.decode(Bool.self, forKey: .recurs)
        siblingOrder = try c.decode(UInt32.self, forKey: .siblingOrder); requiredForParent = try c.decode(Bool.self, forKey: .requiredForParent)
        initialOpen = try c.decode(ItemCompletionReopenState.self, forKey: .initialOpen)
        try RoutineOccurrenceValidation.require(isValid)
    }
    func encode(to encoder: any Encoder) throws {
        try RoutineOccurrenceValidation.require(isValid)
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(itemID, forKey: .itemID); try c.encode(parentID, forKey: .parentID); try c.encode(sourceRevision, forKey: .sourceRevision)
        try c.encode(title, forKey: .title); try c.encode(kind, forKey: .kind); try c.encode(recurs, forKey: .recurs)
        try c.encode(siblingOrder, forKey: .siblingOrder); try c.encode(requiredForParent, forKey: .requiredForParent); try c.encode(initialOpen, forKey: .initialOpen)
    }
}

struct RoutineOccurrenceManifest: Equatable, Sendable {
    let schemaVersion: Int
    /// Ledger instance identity used in HTTP paths, distinct from the planner occurrence UUID.
    let id: UUID
    let seriesItemID: UUID
    let occurrenceID: UUID
    let identity: RecurrenceOccurrenceIdentity
    let nominalStart: String
    let nominalEnd: String
    let windowStart: String
    let windowEnd: String
    let timezoneName: String
    let definitionHash: String
    let members: [RoutineOccurrenceMemberDefinition]
    var isValid: Bool { (try? tree()) != nil && RoutineOccurrenceValidation.fitsSize(self) }

    /// Iterative complete topology, including canonical members omitted from the plan.
    fileprivate func tree() throws -> RoutineOccurrenceTree {
        try RoutineOccurrenceValidation.require(schemaVersion == 1 && id != RoutineOccurrenceValidation.nilID
            && seriesItemID != RoutineOccurrenceValidation.nilID && id != occurrenceID
            && dayWeaveIsRFC4122VersionFiveUUID(occurrenceID) && ItemCompletionValidation.hash(definitionHash)
            && !members.isEmpty && members.count <= RoutineOccurrenceValidation.maximumMembers
            && !timezoneName.isEmpty && timezoneName.utf8.count <= 100
            && [nominalStart, nominalEnd, windowStart, windowEnd].allSatisfy(ItemCompletionValidation.timestamp))
        guard let start = CanonicalRFC3339Instant(nominalStart), let end = CanonicalRFC3339Instant(nominalEnd),
              let windowStart = CanonicalRFC3339Instant(windowStart), let windowEnd = CanonicalRFC3339Instant(windowEnd),
              start < end, windowStart < windowEnd, let zone = DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: timezoneName),
              validIdentity(start: start, end: end, zone: zone) else { throw RoutineOccurrenceError.invalidData }
        var definitions: [UUID: RoutineOccurrenceMemberDefinition] = [:]
        for member in members {
            guard member.isValid, definitions.updateValue(member, forKey: member.itemID) == nil else { throw RoutineOccurrenceError.invalidData }
        }
        guard let root = definitions[seriesItemID], root.parentID == nil, root.recurs,
              root.kind == .task || root.kind == .routine else { throw RoutineOccurrenceError.invalidData }
        var children: [UUID: [UUID]] = [:]
        for member in members {
            if let parent = member.parentID {
                guard definitions[parent] != nil else { throw RoutineOccurrenceError.invalidData }
                children[parent, default: []].append(member.itemID)
            } else if member.itemID != seriesItemID { throw RoutineOccurrenceError.invalidData }
        }
        var remaining = definitions.mapValues { children[$0.itemID]?.count ?? 0 }
        var ready = remaining.compactMap { $0.value == 0 ? $0.key : nil }
        var postorder: [UUID] = []
        while let memberID = ready.popLast() {
            postorder.append(memberID)
            if let parent = definitions[memberID]?.parentID {
                remaining[parent, default: 0] -= 1
                if remaining[parent] == 0 { ready.append(parent) }
            }
        }
        guard postorder.count == members.count else { throw RoutineOccurrenceError.invalidData }
        return .init(definitions: definitions, children: children, postorder: postorder)
    }

    private func validIdentity(start: CanonicalRFC3339Instant, end: CanonicalRFC3339Instant, zone: TimeZone) -> Bool {
        let first = Self.localDay(start.microsecondsSinceUnixEpoch, zone: zone)
        let last = Self.localDay(end.microsecondsSinceUnixEpoch - 1, zone: zone)
        let sameDay = first.day == last.day
        let matchesDate: (String) -> Bool = { sameDay && $0 == first.date }
        switch identity {
        case let .calendarDay(date, ordinal): return ordinal < UInt16.max && matchesDate(date)
        case let .calendarWeek(week, ordinal): return sameDay && ordinal < UInt16.max && Int64(week) <= first.day + 2_440_588 && first.day + 2_440_588 <= Int64(week) + 6
        case let .calendarMonth(year, month, ordinal): return sameDay && ordinal < UInt16.max && Int(year) == first.year && Int(month) == first.month
        case let .rollingMinutes(index, anchor): return UInt32(exactly: index) != nil && RoutineOccurrenceValidation.anchor(anchor)
        case let .afterCompletion(anchor): return RoutineOccurrenceValidation.anchor(anchor)
        case let .rollingMonth(cycle, index, anchor): return (0...Int64(Int32.max)).contains(cycle) && index < UInt16.max && RoutineOccurrenceValidation.anchor(anchor)
        case let .customRule(ruleID, sequence, date): return UUID(uuidString: ruleID).map(dayWeaveIsRFC4122VersionFiveUUID) == true && sequence < 10_000 && matchesDate(date)
        case .custom: return false
        }
    }

    /// Gregorian integer arithmetic avoids Date's fractional precision and
    /// historical calendar cutovers. Timezone offsets are queried at whole seconds.
    private static func localDay(_ micros: Int64, zone: TimeZone) -> (day: Int64, date: String, year: Int, month: Int) {
        var seconds = micros / 1_000_000
        if micros % 1_000_000 < 0 { seconds -= 1 }
        let local = seconds + Int64(zone.secondsFromGMT(for: Date(timeIntervalSince1970: Double(seconds))))
        var day = local / 86_400
        if local % 86_400 < 0 { day -= 1 }
        let z = day + 719_468
        let era = (z >= 0 ? z : z - 146_096) / 146_097
        let doe = z - era * 146_097
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365
        var year = yoe + era * 400
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        let mp = (5 * doy + 2) / 153
        let d = doy - (153 * mp + 2) / 5 + 1
        let month = mp + (mp < 10 ? 3 : -9)
        year += month <= 2 ? 1 : 0
        return (day, String(format: "%04lld-%02lld-%02lld", year, month, d), Int(year), Int(month))
    }
}
extension RoutineOccurrenceManifest: Codable {
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", id, seriesItemID = "series_item_id", occurrenceID = "occurrence_id", identity
        case nominalStart = "nominal_start", nominalEnd = "nominal_end", windowStart = "window_start", windowEnd = "window_end"
        case timezoneName = "timezone_name", definitionHash = "definition_hash", members
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["schema_version", "id", "series_item_id", "occurrence_id", "identity", "nominal_start", "nominal_end", "window_start", "window_end", "timezone_name", "definition_hash", "members"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); id = try c.decode(UUID.self, forKey: .id)
        seriesItemID = try c.decode(UUID.self, forKey: .seriesItemID); occurrenceID = try c.decode(UUID.self, forKey: .occurrenceID)
        identity = try c.decode(RoutineOccurrenceIdentityWire.self, forKey: .identity).value
        nominalStart = try c.decode(String.self, forKey: .nominalStart); nominalEnd = try c.decode(String.self, forKey: .nominalEnd)
        windowStart = try c.decode(String.self, forKey: .windowStart); windowEnd = try c.decode(String.self, forKey: .windowEnd)
        timezoneName = try c.decode(String.self, forKey: .timezoneName); definitionHash = try c.decode(String.self, forKey: .definitionHash)
        members = try c.decode([RoutineOccurrenceMemberDefinition].self, forKey: .members)
        try RoutineOccurrenceValidation.require(isValid)
    }
}

/// This route permits time-crate anchors through ±23:59. Do not broaden the
/// existing canonical authoring identity decoder's narrower historical contract.
private struct RoutineOccurrenceIdentityWire: Decodable {
    let value: RecurrenceOccurrenceIdentity
    private enum CodingKeys: String, CodingKey {
        case type, date, year, month, index, anchor, cycle, sequence
        case bucketOrdinal = "bucket_ordinal", weekKey = "week_key", ruleID = "rule_id"
    }
    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "calendar_day":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "date", "bucket_ordinal"])
            value = .calendarDay(date: try c.decode(String.self, forKey: .date), bucketOrdinal: try c.decode(UInt16.self, forKey: .bucketOrdinal))
        case "calendar_week":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "week_key", "bucket_ordinal"])
            value = .calendarWeek(weekKey: try c.decode(Int32.self, forKey: .weekKey), bucketOrdinal: try c.decode(UInt16.self, forKey: .bucketOrdinal))
        case "calendar_month":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "year", "month", "bucket_ordinal"])
            value = .calendarMonth(year: try c.decode(Int32.self, forKey: .year), month: try c.decode(UInt8.self, forKey: .month), bucketOrdinal: try c.decode(UInt16.self, forKey: .bucketOrdinal))
        case "rolling_minutes":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "index", "anchor"])
            value = .rollingMinutes(index: try c.decode(Int64.self, forKey: .index), anchor: try c.decode(String.self, forKey: .anchor))
        case "after_completion":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "anchor"])
            value = .afterCompletion(anchor: try c.decode(String.self, forKey: .anchor))
        case "rolling_month":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "cycle", "index", "anchor"])
            value = .rollingMonth(cycle: try c.decode(Int64.self, forKey: .cycle), index: try c.decode(UInt16.self, forKey: .index), anchor: try c.decode(String.self, forKey: .anchor))
        case "custom_rule":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "rule_id", "sequence", "date"])
            value = .customRule(ruleID: try c.decode(String.self, forKey: .ruleID), sequence: try c.decode(UInt32.self, forKey: .sequence), date: try c.decode(String.self, forKey: .date))
        default: throw RoutineOccurrenceError.invalidData
        }
    }
}

fileprivate struct RoutineOccurrenceTree {
    let definitions: [UUID: RoutineOccurrenceMemberDefinition]
    let children: [UUID: [UUID]]
    let postorder: [UUID]
}

struct RoutineOccurrenceMemberState: Equatable, Sendable {
    let itemID: UUID
    let revision: UInt64
    let status: RoutineOccurrenceStatus
    let requiredForParent: Bool
    let mode: ItemCompletionMode
    let open: ItemCompletionReopenState
    let provenance: ItemCompletionProvenance?
    let completedAt: String?
    let updatedAt: String
    var isValid: Bool {
        guard itemID != RoutineOccurrenceValidation.nilID && RoutineOccurrenceValidation.revision(revision)
            && open.isValid(for: itemID) && ItemCompletionValidation.timestamp(updatedAt)
            && (completedAt.map(ItemCompletionValidation.timestamp) ?? true)
            && ((status == .completed) == (completedAt != nil))
            && (!status.isOpen || status.rawValue == open.status.rawValue) else { return false }
        if let provenance {
            return status == .completed && provenance.reopen == open
                && ((mode == .automatic && provenance.kind == .automatic) || (mode == .complete && provenance.kind == .manual))
        }
        return mode != .complete
    }
}
extension RoutineOccurrenceMemberState: Codable {
    private enum CodingKeys: String, CodingKey {
        case itemID = "item_id", revision, status, requiredForParent = "required_for_parent", mode, open, provenance
        case completedAt = "completed_at", updatedAt = "updated_at"
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["item_id", "revision", "status", "required_for_parent", "mode", "open", "provenance", "completed_at", "updated_at"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        itemID = try c.decode(UUID.self, forKey: .itemID); revision = try c.decode(UInt64.self, forKey: .revision)
        status = try c.decode(RoutineOccurrenceStatus.self, forKey: .status); requiredForParent = try c.decode(Bool.self, forKey: .requiredForParent)
        mode = try c.decode(ItemCompletionMode.self, forKey: .mode); open = try c.decode(ItemCompletionReopenState.self, forKey: .open)
        provenance = try c.decodeIfPresent(ItemCompletionProvenance.self, forKey: .provenance)
        completedAt = try c.decodeIfPresent(String.self, forKey: .completedAt); updatedAt = try c.decode(String.self, forKey: .updatedAt)
        try RoutineOccurrenceValidation.require(isValid)
    }
    func encode(to encoder: any Encoder) throws {
        try RoutineOccurrenceValidation.require(isValid)
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(itemID, forKey: .itemID); try c.encode(revision, forKey: .revision); try c.encode(status, forKey: .status)
        try c.encode(requiredForParent, forKey: .requiredForParent); try c.encode(mode, forKey: .mode); try c.encode(open, forKey: .open)
        try c.encode(provenance, forKey: .provenance); try c.encode(completedAt, forKey: .completedAt); try c.encode(updatedAt, forKey: .updatedAt)
    }
}

struct RoutineOccurrenceAggregate: Equatable, Sendable {
    let manifest: RoutineOccurrenceManifest
    let revision: UInt64
    let members: [RoutineOccurrenceMemberState]
    var isValid: Bool { (try? evaluation()) != nil && RoutineOccurrenceValidation.fitsSize(self) }

    /// Validate the server's already-reconciled fixed point. This never writes
    /// lifecycle, invents progress, or supplies scheduling authority.
    fileprivate func evaluation() throws -> [UUID: RoutineOccurrenceMemberEvaluation] {
        try RoutineOccurrenceValidation.require(RoutineOccurrenceValidation.revision(revision)
            && members.count <= RoutineOccurrenceValidation.maximumMembers && members.count == manifest.members.count)
        let tree = try manifest.tree()
        var states: [UUID: RoutineOccurrenceMemberState] = [:]
        for member in members {
            guard member.isValid, member.revision <= revision, tree.definitions[member.itemID] != nil,
                  states.updateValue(member, forKey: member.itemID) == nil else { throw RoutineOccurrenceError.invalidData }
            let parent = tree.children[member.itemID] != nil
            try RoutineOccurrenceValidation.require(parent ? (member.status.isOpen || member.provenance != nil) : (member.mode == .automatic && member.provenance == nil))
        }
        var unqualified: [UUID: Bool] = [:]
        for id in tree.postorder.reversed() {
            let definition = tree.definitions[id]!
            unqualified[id] = (id != manifest.seriesItemID && definition.recurs)
                || definition.parentID.map { unqualified[$0] == true } == true
        }
        var counts: [UUID: ItemCompletionCounts] = [:]
        var result: [UUID: RoutineOccurrenceMemberEvaluation] = [:]
        for id in tree.postorder {
            let member = states[id]!
            let count = counts[id] ?? .init()
            let missing = unqualified[id] == true
            if !missing {
                switch member.mode {
                case .complete: try RoutineOccurrenceValidation.require(member.status == .completed && member.provenance?.kind == .manual)
                case .keepOpen: try RoutineOccurrenceValidation.require(member.status.isOpen && member.provenance == nil)
                case .automatic:
                    let ready = count.requiredDescendants > 0 && count.incomplete == 0 && count.occurrenceEvidenceRequired == 0
                    if ready { try RoutineOccurrenceValidation.require(member.status == .completed && member.provenance?.kind == .automatic) }
                    else { try RoutineOccurrenceValidation.require(member.provenance == nil) }
                }
            }
            result[id] = .init(itemID: id, counts: count, occurrenceEvidenceRequired: missing, reason: missing ? .occurrenceEvidenceRequired : .unchanged)
            if member.requiredForParent, let parent = tree.definitions[id]?.parentID {
                let existing = counts[parent] ?? .init()
                let next = ItemCompletionCounts(requiredDescendants: existing.requiredDescendants + count.requiredDescendants + 1,
                    completed: existing.completed + count.completed + ((!missing && member.status == .completed) ? 1 : 0),
                    incomplete: existing.incomplete + count.incomplete + ((!missing && member.status != .completed) ? 1 : 0),
                    occurrenceEvidenceRequired: existing.occurrenceEvidenceRequired + count.occurrenceEvidenceRequired + (missing ? 1 : 0))
                try RoutineOccurrenceValidation.require(next.isValid && next.requiredDescendants < UInt64(members.count))
                counts[parent] = next
            }
        }
        return result
    }
}
extension RoutineOccurrenceAggregate: Codable {
    private enum CodingKeys: String, CodingKey { case manifest, revision, members }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["manifest", "revision", "members"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        manifest = try c.decode(RoutineOccurrenceManifest.self, forKey: .manifest); revision = try c.decode(UInt64.self, forKey: .revision)
        members = try c.decode([RoutineOccurrenceMemberState].self, forKey: .members)
        try RoutineOccurrenceValidation.require(isValid)
    }
}

struct RoutineOccurrenceMemberEvaluation: Equatable, Sendable {
    let itemID: UUID
    let counts: ItemCompletionCounts
    let occurrenceEvidenceRequired: Bool
    let reason: RoutineOccurrenceReason
}
extension RoutineOccurrenceMemberEvaluation: Codable {
    private enum CodingKeys: String, CodingKey { case itemID = "item_id", counts, occurrenceEvidenceRequired = "occurrence_evidence_required", reason }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["item_id", "counts", "occurrence_evidence_required", "reason"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        itemID = try c.decode(UUID.self, forKey: .itemID); counts = try c.decode(ItemCompletionCounts.self, forKey: .counts)
        occurrenceEvidenceRequired = try c.decode(Bool.self, forKey: .occurrenceEvidenceRequired); reason = try c.decode(RoutineOccurrenceReason.self, forKey: .reason)
        try RoutineOccurrenceValidation.require(itemID != RoutineOccurrenceValidation.nilID && counts.requiredDescendants < UInt64(RoutineOccurrenceValidation.maximumMembers))
    }
}

struct RoutineOccurrenceSnapshot: Equatable, Sendable {
    let schemaVersion: Int
    let aggregate: RoutineOccurrenceAggregate
    let evidenceHash: String
    let freshEditEligible: Bool
    let members: [RoutineOccurrenceMemberEvaluation]
    var isValid: Bool {
        guard schemaVersion == 1, ItemCompletionValidation.hash(evidenceHash), members.count == aggregate.members.count,
              let expected = try? aggregate.evaluation() else { return false }
        var seen = Set<UUID>()
        return members.allSatisfy { member in
            seen.insert(member.itemID).inserted && member.counts.isValid
                && expected[member.itemID]?.counts == member.counts
                && expected[member.itemID]?.occurrenceEvidenceRequired == member.occurrenceEvidenceRequired
        } && RoutineOccurrenceValidation.fitsSize(self)
    }
}
extension RoutineOccurrenceSnapshot: Codable {
    private enum CodingKeys: String, CodingKey { case schemaVersion = "schema_version", aggregate, evidenceHash = "evidence_hash", freshEditEligible = "fresh_edit_eligible", members }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["schema_version", "aggregate", "evidence_hash", "fresh_edit_eligible", "members"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); aggregate = try c.decode(RoutineOccurrenceAggregate.self, forKey: .aggregate)
        evidenceHash = try c.decode(String.self, forKey: .evidenceHash); freshEditEligible = try c.decode(Bool.self, forKey: .freshEditEligible)
        members = try c.decode([RoutineOccurrenceMemberEvaluation].self, forKey: .members)
        try RoutineOccurrenceValidation.require(isValid)
    }
}

enum RoutineOccurrenceAction: Equatable, Sendable {
    case setOutcome(status: RoutineOccurrenceStatus)
    case reopen(open: ItemCompletionReopenState)
    case setPolicy(requiredForParent: Bool, mode: ItemCompletionMode)
    func isValid(for memberID: UUID) -> Bool {
        guard memberID != RoutineOccurrenceValidation.nilID else { return false }
        switch self {
        case let .setOutcome(status): return status == .completed || status == .skipped
        case let .reopen(open): return open.isValid(for: memberID)
        case .setPolicy: return true
        }
    }
}
extension RoutineOccurrenceAction: Codable {
    private enum CodingKeys: String, CodingKey { case type, status, open, requiredForParent = "required_for_parent", mode }
    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "set_outcome":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "status"])
            let status = try c.decode(RoutineOccurrenceStatus.self, forKey: .status)
            try RoutineOccurrenceValidation.require(status == .completed || status == .skipped)
            self = .setOutcome(status: status)
        case "reopen":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "open"])
            self = .reopen(open: try c.decode(ItemCompletionReopenState.self, forKey: .open))
        case "set_policy":
            try RoutineOccurrenceValidation.keys(decoder, ["type", "required_for_parent", "mode"])
            self = .setPolicy(requiredForParent: try c.decode(Bool.self, forKey: .requiredForParent), mode: try c.decode(ItemCompletionMode.self, forKey: .mode))
        default: throw RoutineOccurrenceError.invalidData
        }
    }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .setOutcome(status):
            try RoutineOccurrenceValidation.require(status == .completed || status == .skipped)
            try c.encode("set_outcome", forKey: .type); try c.encode(status, forKey: .status)
        case let .reopen(open):
            try RoutineOccurrenceValidation.require(open.isValid)
            try c.encode("reopen", forKey: .type); try c.encode(open, forKey: .open)
        case let .setPolicy(required, mode):
            try c.encode("set_policy", forKey: .type); try c.encode(required, forKey: .requiredForParent); try c.encode(mode, forKey: .mode)
        }
    }
}

struct RoutineOccurrenceCommand: Equatable, Sendable {
    let schemaVersion: Int
    let operationID: UUID
    let expectedInstanceRevision: UInt64
    let expectedMemberRevision: UInt64
    let expectedEvidenceHash: String
    let action: RoutineOccurrenceAction
    var isValid: Bool {
        schemaVersion == 1 && operationID != RoutineOccurrenceValidation.nilID
            && RoutineOccurrenceValidation.revision(expectedInstanceRevision)
            && RoutineOccurrenceValidation.revision(expectedMemberRevision)
            && ItemCompletionValidation.hash(expectedEvidenceHash)
            && { if case let .setOutcome(status) = action { return status == .completed || status == .skipped }
                 if case let .reopen(open) = action { return open.isValid }; return true }()
    }
    func isValid(for memberID: UUID) -> Bool { isValid && action.isValid(for: memberID) }
    func bytes() throws -> Data {
        try RoutineOccurrenceValidation.require(isValid)
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(self)
        try RoutineOccurrenceValidation.require(data.count <= RoutineOccurrenceValidation.maximumRequestBytes)
        return data
    }
}
extension RoutineOccurrenceCommand: Codable {
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", operationID = "operation_id", expectedInstanceRevision = "expected_instance_revision"
        case expectedMemberRevision = "expected_member_revision", expectedEvidenceHash = "expected_evidence_hash", action
    }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["schema_version", "operation_id", "expected_instance_revision", "expected_member_revision", "expected_evidence_hash", "action"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); operationID = try c.decode(UUID.self, forKey: .operationID)
        expectedInstanceRevision = try c.decode(UInt64.self, forKey: .expectedInstanceRevision); expectedMemberRevision = try c.decode(UInt64.self, forKey: .expectedMemberRevision)
        expectedEvidenceHash = try c.decode(String.self, forKey: .expectedEvidenceHash); action = try c.decode(RoutineOccurrenceAction.self, forKey: .action)
        try RoutineOccurrenceValidation.require(isValid)
    }
}

struct RoutineOccurrenceMutation: Equatable, Sendable {
    let operationID: UUID
    let replayed: Bool
    let occurrence: RoutineOccurrenceSnapshot
    var isValid: Bool {
        operationID != RoutineOccurrenceValidation.nilID && occurrence.isValid
            && occurrence.aggregate.revision >= 2 && occurrence.freshEditEligible && RoutineOccurrenceValidation.fitsSize(self)
    }
    func matches(instanceID: UUID, memberID: UUID, command: RoutineOccurrenceCommand) -> Bool {
        guard isValid, command.isValid(for: memberID), operationID == command.operationID,
              occurrence.aggregate.manifest.id == instanceID,
              command.expectedInstanceRevision < RoutineOccurrenceValidation.maximumRevision,
              command.expectedMemberRevision < RoutineOccurrenceValidation.maximumRevision,
              occurrence.aggregate.revision == command.expectedInstanceRevision + 1,
              let member = occurrence.aggregate.members.first(where: { $0.itemID == memberID }),
              member.revision == command.expectedMemberRevision + 1,
              let evaluation = occurrence.members.first(where: { $0.itemID == memberID }),
              !evaluation.occurrenceEvidenceRequired else { return false }
        let parent = occurrence.aggregate.manifest.members.contains { $0.parentID == memberID }
        switch command.action {
        case let .setOutcome(status): return !parent && member.status == status && evaluation.reason == .outcomeRecorded
        case let .reopen(open): return !parent && member.open == open && member.status.rawValue == open.status.rawValue && evaluation.reason == .reopened
        case let .setPolicy(required, mode): return member.requiredForParent == required && member.mode == mode && (parent || mode == .automatic)
        }
    }
}
extension RoutineOccurrenceMutation: Codable {
    private enum CodingKeys: String, CodingKey { case operationID = "operation_id", replayed, occurrence }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["operation_id", "replayed", "occurrence"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        operationID = try c.decode(UUID.self, forKey: .operationID); replayed = try c.decode(Bool.self, forKey: .replayed)
        occurrence = try c.decode(RoutineOccurrenceSnapshot.self, forKey: .occurrence)
        try RoutineOccurrenceValidation.require(isValid)
    }
}

struct RoutineOccurrenceChange: Equatable, Sendable {
    let sequence: UInt64
    let occurrence: RoutineOccurrenceSnapshot
}
extension RoutineOccurrenceChange: Codable {
    private enum CodingKeys: String, CodingKey { case sequence, occurrence }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["sequence", "occurrence"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sequence = try c.decode(UInt64.self, forKey: .sequence); occurrence = try c.decode(RoutineOccurrenceSnapshot.self, forKey: .occurrence)
        try RoutineOccurrenceValidation.require(RoutineOccurrenceValidation.revision(sequence))
    }
}

struct RoutineOccurrencePage: Equatable, Sendable {
    let schemaVersion: Int
    let changes: [RoutineOccurrenceChange]
    let cursor: String
    let hasMore: Bool
    var isValid: Bool {
        guard schemaVersion == 1, changes.count <= 100, RoutineOccurrenceValidation.cursor(cursor),
              !hasMore || !changes.isEmpty else { return false }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.withoutEscapingSlashes]
        let envelope = Self(schemaVersion: schemaVersion, changes: [], cursor: cursor, hasMore: hasMore)
        guard let envelopeBytes = try? encoder.encode(envelope).count,
              envelopeBytes <= RoutineOccurrenceValidation.maximumBytes else { return false }
        var remainingBytes = RoutineOccurrenceValidation.maximumBytes - envelopeBytes
        var previousSequence: UInt64 = 0
        var previousByInstance: [UUID: RoutineOccurrenceSnapshot] = [:]
        for (index, change) in changes.enumerated() {
            guard RoutineOccurrenceValidation.revision(change.sequence), change.sequence > previousSequence, change.occurrence.isValid else { return false }
            // Encode at most one already-bounded instance, never a potentially
            // 100 × 8 MiB page. The empty envelope already includes both brackets.
            if index > 0 {
                guard remainingBytes > 0 else { return false }
                remainingBytes -= 1 // Array comma.
            }
            guard let changeBytes = try? encoder.encode(change).count,
                  changeBytes <= remainingBytes else { return false }
            remainingBytes -= changeBytes
            let id = change.occurrence.aggregate.manifest.id
            if let old = previousByInstance[id] {
                guard change.occurrence.aggregate.revision > old.aggregate.revision,
                      change.occurrence.aggregate.manifest == old.aggregate.manifest else { return false }
            }
            previousSequence = change.sequence; previousByInstance[id] = change.occurrence
        }
        return true
    }
    var isCurrentStatePage: Bool { isValid && Set(changes.map { $0.occurrence.aggregate.manifest.id }).count == changes.count }
}
extension RoutineOccurrencePage: Codable {
    private enum CodingKeys: String, CodingKey { case schemaVersion = "schema_version", changes, cursor, hasMore = "has_more" }
    init(from decoder: any Decoder) throws {
        try RoutineOccurrenceValidation.keys(decoder, ["schema_version", "changes", "cursor", "has_more"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion); changes = try c.decode([RoutineOccurrenceChange].self, forKey: .changes)
        cursor = try c.decode(String.self, forKey: .cursor); hasMore = try c.decode(Bool.self, forKey: .hasMore)
        try RoutineOccurrenceValidation.require(isValid)
    }
}
