import Foundation

/// These failures are read/qualification diagnostics, never occurrence PUT
/// no-effect evidence and never permission to discard a submitted journal.
enum RoutinePlanningWitnessError: Error, Equatable, LocalizedError, Sendable {
    case invalidData, tooLarge, sourceChanged, cursorChanged, authentication, unavailable
    var errorDescription: String? {
        switch self {
        case .invalidData: "Routine planning evidence could not be verified."
        case .tooLarge: "Routine planning evidence exceeds the supported limit."
        case .sourceChanged: "Canonical planning sources changed. Sync and try again."
        case .cursorChanged: "Occurrence history changed. Sync and try again."
        case .authentication: "Routine planning authentication is unavailable."
        case .unavailable: "Routine planning authority is unavailable."
        }
    }
}

enum RoutinePlanningRemoteReason: String, Codable, CaseIterable, Sendable {
    case firstPublicationRequired = "first_publication_required"
    case executionEvidenceRequired = "execution_evidence_required"
    case retainedManualPlacementRequired = "retained_manual_placement_required"
    case sourceIneligible = "source_ineligible"
    case calendarProjectionIncomplete = "calendar_projection_incomplete"
    case compositionUnsupported = "composition_unsupported"

    var preparationMessage: String {
        switch self {
        case .firstPublicationRequired: "Sync to publish missing routine instances before preparing saved input."
        case .executionEvidenceRequired: "Execution evidence requires remote composition. No saved input was replaced."
        case .retainedManualPlacementRequired: "Retained manual placements require remote composition. No saved input was replaced."
        case .sourceIneligible: "The current source tree requires remote composition. No saved input was replaced."
        case .calendarProjectionIncomplete: "Calendar coverage is incomplete. Sync Calendar before preparing saved input."
        case .compositionUnsupported: "This composition requires the server. No saved input was replaced."
        }
    }
}

enum RoutinePlanningMemberStatus: String, Codable, Sendable {
    case notStarted = "not_started", scheduled, completed, skipped, canceled, blocked
}

enum RoutinePlanningWitnessValidation {
    static let maximumBytes = 16 * 1_024 * 1_024
    static let maximumMembers = 10_000
    static let maximumRevision = UInt64(Int64.max)
    static func require(_ condition: Bool) throws {
        guard condition else { throw RoutinePlanningWitnessError.invalidData }
    }
    static func keys(_ decoder: any Decoder, _ expected: Set<String>) throws {
        try require(Set(try decoder.container(keyedBy: SchedulerHelperCodingKey.self).allKeys.map(\.stringValue)) == expected)
    }
    static func uuid(_ value: String) throws -> UUID {
        guard let id = UUID(uuidString: value), id != RoutineOccurrenceValidation.nilID,
              id.uuidString.lowercased() == value.lowercased(), value.utf8.count == 36 else {
            throw RoutinePlanningWitnessError.invalidData
        }
        return id
    }
    static func revision(_ value: UInt64, zeroAllowed: Bool = false) -> Bool {
        (zeroAllowed || value > 0) && value <= maximumRevision
    }
    static func fingerprint(_ value: String, kind: String) -> Bool {
        let prefix = kind == "local" ? "local-sha256:" : "routine-witness-\(kind)-sha256:"
        guard value.hasPrefix(prefix) else { return false }
        let digest = value.dropFirst(prefix.count)
        return digest.count == 64 && digest.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func cursor(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 4_096 && value.utf8.allSatisfy { $0 < 128 }
    }
    static func sourceMap(_ raw: [String: UInt64]) throws -> [UUID: UInt64] {
        try require(raw.count <= maximumMembers)
        var result: [UUID: UInt64] = [:]
        for (key, value) in raw {
            let id = try uuid(key)
            try require(revision(value) && result.updateValue(value, forKey: id) == nil)
        }
        return result
    }
    static func decode<T: Decodable>(_ type: T.Type, from bytes: Data) throws -> T {
        guard bytes.count <= maximumBytes else { throw RoutinePlanningWitnessError.tooLarge }
        guard StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: bytes) else { throw RoutinePlanningWitnessError.invalidData }
        do { return try JSONDecoder().decode(type, from: bytes) }
        catch { throw RoutinePlanningWitnessError.invalidData }
    }
    static func encode<T: Encodable>(_ value: T) throws -> Data {
        do {
            let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
            let data = try encoder.encode(value)
            guard data.count <= maximumBytes else { throw RoutinePlanningWitnessError.tooLarge }
            try require(StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: data))
            return data
        } catch let error as RoutinePlanningWitnessError { throw error }
        catch { throw RoutinePlanningWitnessError.invalidData }
    }
}

struct RoutinePlanningLifecycleMember: Codable, Equatable, Sendable {
    let itemID: UUID
    let parentID: UUID?
    let sourceRevision: UInt64
    let status: RoutinePlanningMemberStatus
    private enum CodingKeys: String, CodingKey {
        case itemID = "item_id", parentID = "parent_id", sourceRevision = "source_revision", status
    }
    init(itemID: UUID, parentID: UUID?, sourceRevision: UInt64, status: RoutinePlanningMemberStatus) {
        self.itemID = itemID; self.parentID = parentID; self.sourceRevision = sourceRevision; self.status = status
    }
    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, ["item_id", "parent_id", "source_revision", "status"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        itemID = try RoutinePlanningWitnessValidation.uuid(c.decode(String.self, forKey: .itemID))
        parentID = try c.decodeIfPresent(String.self, forKey: .parentID).map(RoutinePlanningWitnessValidation.uuid)
        sourceRevision = try c.decode(UInt64.self, forKey: .sourceRevision)
        status = try c.decode(RoutinePlanningMemberStatus.self, forKey: .status)
        try RoutinePlanningWitnessValidation.require(parentID != itemID && RoutinePlanningWitnessValidation.revision(sourceRevision))
    }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(itemID.uuidString.lowercased(), forKey: .itemID)
        try c.encode(parentID?.uuidString.lowercased(), forKey: .parentID)
        try c.encode(sourceRevision, forKey: .sourceRevision); try c.encode(status, forKey: .status)
    }
}

struct RoutinePlanningLifecycleInstance: Codable, Equatable, Sendable {
    let rootItemID: UUID
    let occurrenceID: UUID
    let identity: RecurrenceOccurrenceIdentity
    let members: [RoutinePlanningLifecycleMember]
    private enum CodingKeys: String, CodingKey {
        case rootItemID = "root_item_id", occurrenceID = "occurrence_id", identity, members
    }
    init(rootItemID: UUID, occurrenceID: UUID, identity: RecurrenceOccurrenceIdentity, members: [RoutinePlanningLifecycleMember]) {
        self.rootItemID = rootItemID; self.occurrenceID = occurrenceID; self.identity = identity; self.members = members
    }
    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, ["root_item_id", "occurrence_id", "identity", "members"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        rootItemID = try RoutinePlanningWitnessValidation.uuid(c.decode(String.self, forKey: .rootItemID))
        occurrenceID = try RoutinePlanningWitnessValidation.uuid(c.decode(String.self, forKey: .occurrenceID))
        identity = try c.decode(RecurrenceOccurrenceIdentity.self, forKey: .identity)
        members = try c.decode([RoutinePlanningLifecycleMember].self, forKey: .members)
        try validate()
    }
    func encode(to encoder: any Encoder) throws {
        try validate()
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(rootItemID.uuidString.lowercased(), forKey: .rootItemID)
        try c.encode(occurrenceID.uuidString.lowercased(), forKey: .occurrenceID)
        try c.encode(identity, forKey: .identity); try c.encode(members, forKey: .members)
    }
    /// Indexed flat topology; logical depth never consumes the native stack.
    func validate() throws {
        try RoutinePlanningWitnessValidation.require(rootItemID != RoutineOccurrenceValidation.nilID
            && dayWeaveIsRFC4122VersionFiveUUID(occurrenceID) && identity != .custom && identity.hasValidShape
            && !members.isEmpty && members.count <= RoutinePlanningWitnessValidation.maximumMembers)
        var parents: [UUID: UUID] = [:], ids = Set<UUID>(), children: [UUID: Int] = [:]
        for member in members {
            try RoutinePlanningWitnessValidation.require(member.itemID != RoutineOccurrenceValidation.nilID
                && RoutinePlanningWitnessValidation.revision(member.sourceRevision) && ids.insert(member.itemID).inserted)
            children[member.itemID, default: 0] += 0
            if let parent = member.parentID { parents[member.itemID] = parent }
            else { try RoutinePlanningWitnessValidation.require(member.itemID == rootItemID) }
        }
        try RoutinePlanningWitnessValidation.require(ids.contains(rootItemID) && parents[rootItemID] == nil)
        for parent in parents.values {
            try RoutinePlanningWitnessValidation.require(ids.contains(parent))
            children[parent, default: 0] += 1
        }
        var ready = children.compactMap { $0.value == 0 ? $0.key : nil }, visited = 0
        while let id = ready.popLast() {
            visited += 1
            if let parent = parents[id] {
                children[parent, default: 0] -= 1
                if children[parent] == 0 { ready.append(parent) }
            }
        }
        try RoutinePlanningWitnessValidation.require(visited == members.count)
    }
}

struct RoutinePlanningLifecycleContext: Codable, Equatable, Sendable {
    let snapshotRevision: UInt64
    let instances: [RoutinePlanningLifecycleInstance]
    private enum CodingKeys: String, CodingKey { case snapshotRevision = "snapshot_revision", instances }
    init(snapshotRevision: UInt64, instances: [RoutinePlanningLifecycleInstance]) {
        self.snapshotRevision = snapshotRevision; self.instances = instances
    }
    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, ["snapshot_revision", "instances"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        snapshotRevision = try c.decode(UInt64.self, forKey: .snapshotRevision)
        instances = try c.decode([RoutinePlanningLifecycleInstance].self, forKey: .instances)
        try validate()
    }
    func validate() throws {
        try RoutinePlanningWitnessValidation.require(RoutinePlanningWitnessValidation.revision(snapshotRevision, zeroAllowed: true)
            && (snapshotRevision > 0 || instances.isEmpty) && instances.count <= RoutinePlanningWitnessValidation.maximumMembers)
        var remaining = RoutinePlanningWitnessValidation.maximumMembers, ids = Set<UUID>()
        for instance in instances {
            try RoutinePlanningWitnessValidation.require(instance.members.count <= remaining && ids.insert(instance.occurrenceID).inserted)
            remaining -= instance.members.count; try instance.validate()
        }
    }
}

struct RoutinePlanningWitnessRequest: Codable, Equatable, Sendable {
    let schemaVersion: UInt16
    let schedule: RoutinePlanningScheduleInput
    let expectedSourceItemRevisions: [UUID: UInt64]
    let terminalCursor: String
    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", schedule, expectedSourceItemRevisions = "expected_source_item_revisions", terminalCursor = "terminal_cursor"
    }
    init(schedule: RoutinePlanningScheduleInput, expectedSourceItemRevisions: [UUID: UInt64], terminalCursor: String) throws {
        schemaVersion = 1; self.schedule = schedule; self.expectedSourceItemRevisions = expectedSourceItemRevisions; self.terminalCursor = terminalCursor
        try validate()
    }
    init(schedule: DayWeaveSchedulePreviewRequest, expectedSourceItemRevisions: [UUID: UInt64], terminalCursor: String) throws {
        try self.init(schedule: RoutinePlanningScheduleInput(previewRequest: schedule), expectedSourceItemRevisions: expectedSourceItemRevisions, terminalCursor: terminalCursor)
    }
    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, ["schema_version", "schedule", "expected_source_item_revisions", "terminal_cursor"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(UInt16.self, forKey: .schemaVersion); schedule = try c.decode(RoutinePlanningScheduleInput.self, forKey: .schedule)
        expectedSourceItemRevisions = try RoutinePlanningWitnessValidation.sourceMap(c.decode([String: UInt64].self, forKey: .expectedSourceItemRevisions))
        terminalCursor = try c.decode(String.self, forKey: .terminalCursor); try validate()
    }
    func encode(to encoder: any Encoder) throws {
        try validate(); var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(schemaVersion, forKey: .schemaVersion); try c.encode(schedule, forKey: .schedule)
        try c.encode(Dictionary(uniqueKeysWithValues: expectedSourceItemRevisions.map { ($0.key.uuidString.lowercased(), $0.value) }), forKey: .expectedSourceItemRevisions)
        try c.encode(terminalCursor, forKey: .terminalCursor)
    }
    func validate() throws {
        try RoutinePlanningWitnessValidation.require(schemaVersion == 1 && RoutinePlanningWitnessValidation.cursor(terminalCursor)
            && expectedSourceItemRevisions.count <= RoutinePlanningWitnessValidation.maximumMembers
            && expectedSourceItemRevisions.allSatisfy { $0.key != RoutineOccurrenceValidation.nilID && RoutinePlanningWitnessValidation.revision($0.value) })
        try schedule.validate()
    }
}

struct RoutinePlanningWitness: Codable, Equatable, Sendable {
    let workspaceID: UUID
    let userID: UUID
    let requestFingerprint: String
    let witnessFingerprint: String
    let localInputFingerprint: String
    let calendarProjectionFingerprint: String
    let sourceItemRevisions: [UUID: UInt64]
    let terminalCursor: String
    let schedule: RoutinePlanningScheduleInput
    let occurrenceLifecycle: RoutinePlanningLifecycleContext
    let executionSnapshotRevision: UInt64
    let habitChangeHead: UInt64
    let publishedScheduleRevisionID: UUID?
    private enum CodingKeys: String, CodingKey {
        case workspaceID = "workspace_id", userID = "user_id", requestFingerprint = "request_fingerprint", witnessFingerprint = "witness_fingerprint"
        case localInputFingerprint = "local_input_fingerprint", calendarProjectionFingerprint = "calendar_projection_fingerprint"
        case sourceItemRevisions = "source_item_revisions", terminalCursor = "terminal_cursor", schedule
        case occurrenceLifecycle = "occurrence_lifecycle", executionSnapshotRevision = "execution_snapshot_revision", habitChangeHead = "habit_change_head"
        case publishedScheduleRevisionID = "published_schedule_revision_id"
    }
    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, ["workspace_id", "user_id", "request_fingerprint", "witness_fingerprint", "local_input_fingerprint", "calendar_projection_fingerprint", "source_item_revisions", "terminal_cursor", "schedule", "occurrence_lifecycle", "execution_snapshot_revision", "habit_change_head", "published_schedule_revision_id"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        workspaceID = try RoutinePlanningWitnessValidation.uuid(c.decode(String.self, forKey: .workspaceID))
        userID = try RoutinePlanningWitnessValidation.uuid(c.decode(String.self, forKey: .userID))
        requestFingerprint = try c.decode(String.self, forKey: .requestFingerprint); witnessFingerprint = try c.decode(String.self, forKey: .witnessFingerprint)
        localInputFingerprint = try c.decode(String.self, forKey: .localInputFingerprint); calendarProjectionFingerprint = try c.decode(String.self, forKey: .calendarProjectionFingerprint)
        sourceItemRevisions = try RoutinePlanningWitnessValidation.sourceMap(c.decode([String: UInt64].self, forKey: .sourceItemRevisions))
        terminalCursor = try c.decode(String.self, forKey: .terminalCursor); schedule = try c.decode(RoutinePlanningScheduleInput.self, forKey: .schedule)
        occurrenceLifecycle = try c.decode(RoutinePlanningLifecycleContext.self, forKey: .occurrenceLifecycle)
        executionSnapshotRevision = try c.decode(UInt64.self, forKey: .executionSnapshotRevision); habitChangeHead = try c.decode(UInt64.self, forKey: .habitChangeHead)
        publishedScheduleRevisionID = try c.decodeIfPresent(String.self, forKey: .publishedScheduleRevisionID).map(RoutinePlanningWitnessValidation.uuid)
        try validateShape()
    }
    func encode(to encoder: any Encoder) throws {
        try validateShape(); var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(workspaceID.uuidString.lowercased(), forKey: .workspaceID); try c.encode(userID.uuidString.lowercased(), forKey: .userID)
        try c.encode(requestFingerprint, forKey: .requestFingerprint); try c.encode(witnessFingerprint, forKey: .witnessFingerprint)
        try c.encode(localInputFingerprint, forKey: .localInputFingerprint); try c.encode(calendarProjectionFingerprint, forKey: .calendarProjectionFingerprint)
        try c.encode(Dictionary(uniqueKeysWithValues: sourceItemRevisions.map { ($0.key.uuidString.lowercased(), $0.value) }), forKey: .sourceItemRevisions)
        try c.encode(terminalCursor, forKey: .terminalCursor); try c.encode(schedule, forKey: .schedule); try c.encode(occurrenceLifecycle, forKey: .occurrenceLifecycle)
        try c.encode(executionSnapshotRevision, forKey: .executionSnapshotRevision); try c.encode(habitChangeHead, forKey: .habitChangeHead)
        try c.encode(publishedScheduleRevisionID?.uuidString.lowercased(), forKey: .publishedScheduleRevisionID)
    }
    func validateShape() throws {
        try RoutinePlanningWitnessValidation.require(workspaceID != RoutineOccurrenceValidation.nilID && userID != RoutineOccurrenceValidation.nilID
            && RoutinePlanningWitnessValidation.fingerprint(requestFingerprint, kind: "request")
            && RoutinePlanningWitnessValidation.fingerprint(witnessFingerprint, kind: "capture")
            && RoutinePlanningWitnessValidation.fingerprint(localInputFingerprint, kind: "local")
            && RoutinePlanningWitnessValidation.fingerprint(calendarProjectionFingerprint, kind: "calendar")
            && RoutinePlanningWitnessValidation.cursor(terminalCursor)
            && RoutinePlanningWitnessValidation.revision(executionSnapshotRevision, zeroAllowed: true)
            && RoutinePlanningWitnessValidation.revision(habitChangeHead, zeroAllowed: true)
            && sourceItemRevisions.count <= RoutinePlanningWitnessValidation.maximumMembers)
        try schedule.validate(); try occurrenceLifecycle.validate()
        try RoutinePlanningWitnessValidation.require(occurrenceLifecycle.instances.flatMap(\.members).allSatisfy {
            sourceItemRevisions[$0.itemID] == $0.sourceRevision
        })
    }
    /// HTTP operation ownership binds the original request. Rust fingerprints
    /// are opaque domain-separated values, not hashes of native JSON encoding.
    func requireMatches(_ request: RoutinePlanningWitnessRequest) throws {
        try validateShape(); try request.validate()
        try RoutinePlanningWitnessValidation.require(sourceItemRevisions == request.expectedSourceItemRevisions && terminalCursor == request.terminalCursor)
        try schedule.requireNormalization(of: request.schedule)
    }
    func requireMatches(_ request: RoutinePlanningWitnessRequest, expectedWorkspaceID: UUID, expectedUserID: UUID) throws {
        try RoutinePlanningWitnessValidation.require(workspaceID == expectedWorkspaceID && userID == expectedUserID)
        try requireMatches(request)
    }
    func validate(canonicalItems: [DayWeaveCanonicalItem]) throws {
        try validateShape()
        try RoutinePlanningWitnessValidation.require(canonicalItems.count <= RoutinePlanningWitnessValidation.maximumMembers)
        var sources: [UUID: DayWeaveCanonicalItem] = [:], children: [UUID: Int] = [:]
        for item in canonicalItems {
            try RoutinePlanningWitnessValidation.require(item.deletedAt == nil && sources.updateValue(item, forKey: item.id) == nil)
            children[item.id, default: 0] += 0
            if let parent = item.parentID { children[parent, default: 0] += 1 }
        }
        try RoutinePlanningWitnessValidation.require(sources.count == sourceItemRevisions.count
            && sources.allSatisfy { sourceItemRevisions[$0.key] == $0.value.revision })
        var remaining = children, ready = sources.keys.filter { remaining[$0] == 0 }, visited = 0
        while let id = ready.popLast() {
            visited += 1
            if let parent = sources[id]?.parentID {
                try RoutinePlanningWitnessValidation.require(sources[parent] != nil)
                remaining[parent, default: 0] -= 1
                if remaining[parent] == 0 { ready.append(parent) }
            }
        }
        try RoutinePlanningWitnessValidation.require(visited == sources.count)
        for instance in occurrenceLifecycle.instances {
            guard let root = sources[instance.rootItemID] else { throw RoutinePlanningWitnessError.invalidData }
            try RoutinePlanningWitnessValidation.require((root.kind == .task || root.kind == .routine)
                && instance.identity.isCompatible(with: root.recurrence))
            var memberChildren: [UUID: Int] = [:]
            for member in instance.members { if let parent = member.parentID { memberChildren[parent, default: 0] += 1 } }
            for member in instance.members {
                guard let source = sources[member.itemID] else { throw RoutinePlanningWitnessError.invalidData }
                try RoutinePlanningWitnessValidation.require(source.revision == member.sourceRevision
                    && (source.id == instance.rootItemID || source.recurrence == nil)
                    && member.parentID == (member.itemID == instance.rootItemID ? nil : source.parentID)
                    && children[member.itemID, default: 0] == memberChildren[member.itemID, default: 0])
            }
        }
    }
}

struct RoutinePlanningWitnessResponse: Codable, Equatable, Sendable {
    enum Result: Codable, Equatable, Sendable {
        case qualified(RoutinePlanningWitness), remoteRequired(RoutinePlanningRemoteReason)
        private enum CodingKeys: String, CodingKey { case status, witness, reason }
        init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            switch try c.decode(String.self, forKey: .status) {
            case "qualified":
                try RoutinePlanningWitnessValidation.keys(decoder, ["status", "witness"])
                self = .qualified(try c.decode(RoutinePlanningWitness.self, forKey: .witness))
            case "remote_required":
                try RoutinePlanningWitnessValidation.keys(decoder, ["status", "reason"])
                self = .remoteRequired(try c.decode(RoutinePlanningRemoteReason.self, forKey: .reason))
            default: throw RoutinePlanningWitnessError.invalidData
            }
        }
        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .qualified(witness): try c.encode("qualified", forKey: .status); try c.encode(witness, forKey: .witness)
            case let .remoteRequired(reason): try c.encode("remote_required", forKey: .status); try c.encode(reason, forKey: .reason)
            }
        }
    }
    let schemaVersion: UInt16
    let result: Result
    private enum CodingKeys: String, CodingKey { case schemaVersion = "schema_version", result }
    init(result: Result) { schemaVersion = 1; self.result = result }
    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, ["schema_version", "result"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(UInt16.self, forKey: .schemaVersion); result = try c.decode(Result.self, forKey: .result)
        try RoutinePlanningWitnessValidation.require(schemaVersion == 1)
    }
}
