import Foundation

enum DayWeaveExecutionStatus: String, Codable, Equatable, Sendable {
    case active
    case paused
    case completed
    case skipped
    case deferred

    var isOpen: Bool { self == .active || self == .paused }

    var isCanonicalTerminal: Bool { self == .completed || self == .skipped }
}

/// One immutable identity and its server-authoritative timer state.
///
/// Execution is safety-sensitive: a newer server adding or omitting a field must
/// not be interpreted as an older shape and then used to start or finish work.
/// The decoder therefore requires the complete, exact wire contract.
struct DayWeaveExecutionSession: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let itemID: UUID
    let itemRevision: UInt64
    let occurrenceID: UUID?
    let sessionIndex: UInt16
    let plannedBlockID: UUID?
    let sourceDeviceID: UUID
    let status: DayWeaveExecutionStatus
    let revision: UInt64
    let accumulatedSeconds: UInt64
    let actualSeconds: UInt64?
    let startedAt: Date
    let runningSince: Date?
    let pausedAt: Date?
    let pauseUntil: Date?
    let pauseReason: String?
    let moveStart: Date?
    let moveEnd: Date?
    let endedAt: Date?
    let createdAt: Date
    let updatedAt: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case id
        case itemID = "item_id"
        case itemRevision = "item_revision"
        case occurrenceID = "occurrence_id"
        case sessionIndex = "session_index"
        case plannedBlockID = "planned_block_id"
        case sourceDeviceID = "source_device_id"
        case status
        case revision
        case accumulatedSeconds = "accumulated_seconds"
        case actualSeconds = "actual_seconds"
        case startedAt = "started_at"
        case runningSince = "running_since"
        case pausedAt = "paused_at"
        case pauseUntil = "pause_until"
        case pauseReason = "pause_reason"
        case moveStart = "move_start"
        case moveEnd = "move_end"
        case endedAt = "ended_at"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: any Decoder) throws {
        let optionalMoveKeys = Set([
            CodingKeys.moveStart.stringValue,
            CodingKeys.moveEnd.stringValue,
        ])
        try requireExecutionKeyShape(
            required: Set(CodingKeys.allCases.map(\.stringValue)).subtracting(optionalMoveKeys),
            optional: optionalMoveKeys,
            from: decoder
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        itemID = try container.decode(UUID.self, forKey: .itemID)
        itemRevision = try container.decode(UInt64.self, forKey: .itemRevision)
        occurrenceID = try container.decodeIfPresent(UUID.self, forKey: .occurrenceID)
        sessionIndex = try container.decode(UInt16.self, forKey: .sessionIndex)
        plannedBlockID = try container.decodeIfPresent(UUID.self, forKey: .plannedBlockID)
        sourceDeviceID = try container.decode(UUID.self, forKey: .sourceDeviceID)
        status = try container.decode(DayWeaveExecutionStatus.self, forKey: .status)
        revision = try container.decode(UInt64.self, forKey: .revision)
        accumulatedSeconds = try container.decode(UInt64.self, forKey: .accumulatedSeconds)
        actualSeconds = try container.decodeIfPresent(UInt64.self, forKey: .actualSeconds)
        startedAt = try container.decode(Date.self, forKey: .startedAt)
        runningSince = try container.decodeIfPresent(Date.self, forKey: .runningSince)
        pausedAt = try container.decodeIfPresent(Date.self, forKey: .pausedAt)
        pauseUntil = try container.decodeIfPresent(Date.self, forKey: .pauseUntil)
        pauseReason = try container.decodeIfPresent(String.self, forKey: .pauseReason)
        moveStart = try container.decodeIfPresent(Date.self, forKey: .moveStart)
        moveEnd = try container.decodeIfPresent(Date.self, forKey: .moveEnd)
        endedAt = try container.decodeIfPresent(Date.self, forKey: .endedAt)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        try validateExecutionSession(self, codingPath: decoder.codingPath)
    }

    func encode(to encoder: any Encoder) throws {
        try validateExecutionSession(self, codingPath: encoder.codingPath, encoding: true)
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(itemID, forKey: .itemID)
        try container.encode(itemRevision, forKey: .itemRevision)
        try encodeExecutionNullable(occurrenceID, forKey: .occurrenceID, into: &container)
        try container.encode(sessionIndex, forKey: .sessionIndex)
        try encodeExecutionNullable(plannedBlockID, forKey: .plannedBlockID, into: &container)
        try container.encode(sourceDeviceID, forKey: .sourceDeviceID)
        try container.encode(status, forKey: .status)
        try container.encode(revision, forKey: .revision)
        try container.encode(accumulatedSeconds, forKey: .accumulatedSeconds)
        try encodeExecutionNullable(actualSeconds, forKey: .actualSeconds, into: &container)
        try container.encode(startedAt, forKey: .startedAt)
        try encodeExecutionNullable(runningSince, forKey: .runningSince, into: &container)
        try encodeExecutionNullable(pausedAt, forKey: .pausedAt, into: &container)
        try encodeExecutionNullable(pauseUntil, forKey: .pauseUntil, into: &container)
        try encodeExecutionNullable(pauseReason, forKey: .pauseReason, into: &container)
        try container.encodeIfPresent(moveStart, forKey: .moveStart)
        try container.encodeIfPresent(moveEnd, forKey: .moveEnd)
        try encodeExecutionNullable(endedAt, forKey: .endedAt, into: &container)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
    }
}

extension DayWeaveExecutionSession {
    var hasValidShape: Bool {
        do {
            try validateExecutionSession(self, codingPath: [])
            return true
        } catch {
            return false
        }
    }
}

struct DayWeaveExecutionSnapshot: Codable, Equatable, Sendable {
    let revision: UInt64
    let activeSession: DayWeaveExecutionSession?

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case revision
        case activeSession = "active_session"
    }

    init(revision: UInt64, activeSession: DayWeaveExecutionSession?) {
        self.revision = revision
        self.activeSession = activeSession
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        revision = try container.decode(UInt64.self, forKey: .revision)
        activeSession = try container.decodeIfPresent(
            DayWeaveExecutionSession.self,
            forKey: .activeSession
        )
        let revisionIsSupported = revision <= UInt64(Int64.max)
        let activeSessionIsCoherent = activeSession.map { session in
            revision > 0
                && session.revision <= revision
                && (session.status == .active || session.status == .paused)
        } ?? true
        guard revisionIsSupported, activeSessionIsCoherent else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Execution snapshot revision is outside the supported state"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard revision <= UInt64(Int64.max),
              activeSession.map({
                  revision > 0
                      && $0.revision <= revision
                      && ($0.status == .active || $0.status == .paused)
              }) ?? true else {
            throw executionEncodingError(
                codingPath: encoder.codingPath,
                "Execution snapshot revision is outside the supported state"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(revision, forKey: .revision)
        try encodeExecutionNullable(activeSession, forKey: .activeSession, into: &container)
    }
}

struct DayWeaveExecutionMutation: Codable, Equatable, Sendable {
    let revision: UInt64
    let activeSession: DayWeaveExecutionSession?
    let changedSession: DayWeaveExecutionSession
    let replayed: Bool

    var snapshot: DayWeaveExecutionSnapshot {
        .init(revision: revision, activeSession: activeSession)
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case revision
        case activeSession = "active_session"
        case changedSession = "changed_session"
        case replayed
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        revision = try container.decode(UInt64.self, forKey: .revision)
        activeSession = try container.decodeIfPresent(
            DayWeaveExecutionSession.self,
            forKey: .activeSession
        )
        changedSession = try container.decode(
            DayWeaveExecutionSession.self,
            forKey: .changedSession
        )
        replayed = try container.decode(Bool.self, forKey: .replayed)
        let activeMutationIsCoherent = if changedSession.status == .active
            || changedSession.status == .paused
        {
            activeSession == changedSession
        } else {
            activeSession == nil
        }
        guard revision > 0,
              revision <= UInt64(Int64.max),
              changedSession.revision <= revision,
              activeMutationIsCoherent else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Execution mutation state is internally inconsistent"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        let activeMutationIsCoherent = if changedSession.status == .active
            || changedSession.status == .paused
        {
            activeSession == changedSession
        } else {
            activeSession == nil
        }
        guard revision > 0,
              revision <= UInt64(Int64.max),
              changedSession.revision <= revision,
              activeMutationIsCoherent else {
            throw executionEncodingError(
                codingPath: encoder.codingPath,
                "Execution mutation state is internally inconsistent"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(revision, forKey: .revision)
        try encodeExecutionNullable(activeSession, forKey: .activeSession, into: &container)
        try container.encode(changedSession, forKey: .changedSession)
        try container.encode(replayed, forKey: .replayed)
    }
}

struct DayWeaveDeferAssessmentRequest: Codable, Equatable, Sendable {
    let expectedRevision: UInt64
    let sessionID: UUID
    let moveStart: Date
    let actualSeconds: UInt64?

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case expectedRevision = "expected_revision"
        case sessionID = "session_id"
        case moveStart = "move_start"
        case actualSeconds = "actual_seconds"
    }

    init(
        expectedRevision: UInt64,
        sessionID: UUID,
        moveStart: Date,
        actualSeconds: UInt64?
    ) {
        self.expectedRevision = expectedRevision
        self.sessionID = sessionID
        self.moveStart = moveStart
        self.actualSeconds = actualSeconds
    }

    init(from decoder: any Decoder) throws {
        try requireExecutionKeyShape(
            required: ["expected_revision", "session_id", "move_start"],
            optional: ["actual_seconds"],
            from: decoder
        )
        let container = try decoder.container(keyedBy: CodingKeys.self)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        sessionID = try container.decode(UUID.self, forKey: .sessionID)
        moveStart = try container.decode(Date.self, forKey: .moveStart)
        actualSeconds = try container.decodeIfPresent(UInt64.self, forKey: .actualSeconds)
        guard hasValidShape else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Defer assessment request violates the supported contract"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw executionEncodingError(
                codingPath: encoder.codingPath,
                "Defer assessment request violates the supported contract"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(expectedRevision, forKey: .expectedRevision)
        try container.encode(sessionID, forKey: .sessionID)
        try container.encode(moveStart, forKey: .moveStart)
        try container.encodeIfPresent(actualSeconds, forKey: .actualSeconds)
    }

    var hasValidShape: Bool {
        expectedRevision > 0
            && expectedRevision <= UInt64(Int64.max)
            && !sessionID.isDayWeaveNil
            && dayWeavePostgresEpochMicroseconds(moveStart).map {
                $0 % 300_000_000 == 0
            } == true
            && actualSeconds.map({ $0 <= UInt64(Int64.max) }) ?? true
    }
}

enum DayWeaveManualPlacementViolationCode: String, Codable, CaseIterable, Sendable {
    case outsideAvailability = "outside_availability"
    case earliestStart = "earliest_start"
    case latestFinish = "latest_finish"
    case minimumNotice = "minimum_notice"
    case allowedWeekday = "allowed_weekday"
    case preferredDailyWindow = "preferred_daily_window"
    case preferredAbsoluteWindow = "preferred_absolute_window"
    case forbiddenWindow = "forbidden_window"
    case requiredContext = "required_context"
    case requiredLocation = "required_location"
    case requiredCapabilities = "required_capabilities"
    case energy
    case dependency
    case maximumDailyWork = "maximum_daily_work"
    case maximumWeeklyWork = "maximum_weekly_work"
    case bufferCompressed = "buffer_compressed"
    case immutableOverlap = "immutable_overlap"

    var title: String {
        switch self {
        case .outsideAvailability: "Outside availability"
        case .earliestStart: "Before earliest start"
        case .latestFinish: "After latest finish"
        case .minimumNotice: "Minimum notice"
        case .allowedWeekday: "Weekday restriction"
        case .preferredDailyWindow: "Preferred daily window"
        case .preferredAbsoluteWindow: "Preferred date window"
        case .forbiddenWindow: "Forbidden time"
        case .requiredContext: "Required context"
        case .requiredLocation: "Required location"
        case .requiredCapabilities: "Required capability"
        case .energy: "Energy mismatch"
        case .dependency: "Dependency ordering"
        case .maximumDailyWork: "Daily capacity"
        case .maximumWeeklyWork: "Weekly capacity"
        case .bufferCompressed: "Protected buffer"
        case .immutableOverlap: "Fixed-time overlap"
        }
    }
}

enum DayWeaveAssessedBlockKind: String, Codable, Sendable {
    case planned
    case pinned
    case calendarEvent = "calendar_event"
    case externalFixed = "external_fixed"
}

struct DayWeaveDeferConflict: Codable, Equatable, Sendable {
    let blockID: UUID
    let itemID: UUID?
    let occurrenceID: UUID?
    let externalBlockID: UUID?
    let kind: DayWeaveAssessedBlockKind
    let start: Date
    let end: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case blockID = "block_id"
        case itemID = "item_id"
        case occurrenceID = "occurrence_id"
        case externalBlockID = "external_block_id"
        case kind
        case start
        case end
    }

    init(
        blockID: UUID,
        itemID: UUID?,
        occurrenceID: UUID?,
        externalBlockID: UUID?,
        kind: DayWeaveAssessedBlockKind,
        start: Date,
        end: Date
    ) {
        self.blockID = blockID
        self.itemID = itemID
        self.occurrenceID = occurrenceID
        self.externalBlockID = externalBlockID
        self.kind = kind
        self.start = start
        self.end = end
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        blockID = try container.decode(UUID.self, forKey: .blockID)
        itemID = try container.decodeIfPresent(UUID.self, forKey: .itemID)
        occurrenceID = try container.decodeIfPresent(UUID.self, forKey: .occurrenceID)
        externalBlockID = try container.decodeIfPresent(UUID.self, forKey: .externalBlockID)
        kind = try container.decode(DayWeaveAssessedBlockKind.self, forKey: .kind)
        start = try container.decode(Date.self, forKey: .start)
        end = try container.decode(Date.self, forKey: .end)
        guard hasValidShape else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Defer conflict violates the supported contract"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw executionEncodingError(
                codingPath: encoder.codingPath,
                "Defer conflict violates the supported contract"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(blockID, forKey: .blockID)
        try encodeExecutionNullable(itemID, forKey: .itemID, into: &container)
        try encodeExecutionNullable(occurrenceID, forKey: .occurrenceID, into: &container)
        try encodeExecutionNullable(externalBlockID, forKey: .externalBlockID, into: &container)
        try container.encode(kind, forKey: .kind)
        try container.encode(start, forKey: .start)
        try container.encode(end, forKey: .end)
    }

    var hasValidShape: Bool {
        !blockID.isDayWeaveNil
            && !(itemID?.isDayWeaveNil ?? false)
            && !(occurrenceID?.isDayWeaveNil ?? false)
            && !(externalBlockID?.isDayWeaveNil ?? false)
            && executionHasPostgresTimestampPrecision(start)
            && executionHasPostgresTimestampPrecision(end)
            && start < end
    }
}

struct DayWeaveDeferViolation: Codable, Equatable, Sendable, Identifiable {
    let code: DayWeaveManualPlacementViolationCode
    let itemIDs: [UUID]
    let occurrenceIDs: [UUID]
    let conflictingBlockIDs: [UUID]
    let conflictingBlocks: [DayWeaveDeferConflict]
    let start: Date
    let end: Date
    let boundaryStart: Date?
    let boundaryEnd: Date?
    let message: String

    var id: String {
        let blockIDs = conflictingBlockIDs.map(\.uuidString).joined(separator: ",")
        return "\(code.rawValue)|\(start.timeIntervalSince1970)|\(end.timeIntervalSince1970)|\(blockIDs)"
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case code
        case itemIDs = "item_ids"
        case occurrenceIDs = "occurrence_ids"
        case conflictingBlockIDs = "conflicting_block_ids"
        case conflictingBlocks = "conflicting_blocks"
        case start
        case end
        case boundaryStart = "boundary_start"
        case boundaryEnd = "boundary_end"
        case message
    }

    init(
        code: DayWeaveManualPlacementViolationCode,
        itemIDs: [UUID],
        occurrenceIDs: [UUID],
        conflictingBlockIDs: [UUID],
        conflictingBlocks: [DayWeaveDeferConflict],
        start: Date,
        end: Date,
        boundaryStart: Date?,
        boundaryEnd: Date?,
        message: String
    ) {
        self.code = code
        self.itemIDs = itemIDs
        self.occurrenceIDs = occurrenceIDs
        self.conflictingBlockIDs = conflictingBlockIDs
        self.conflictingBlocks = conflictingBlocks
        self.start = start
        self.end = end
        self.boundaryStart = boundaryStart
        self.boundaryEnd = boundaryEnd
        self.message = message
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        code = try container.decode(DayWeaveManualPlacementViolationCode.self, forKey: .code)
        itemIDs = try container.decode([UUID].self, forKey: .itemIDs)
        occurrenceIDs = try container.decode([UUID].self, forKey: .occurrenceIDs)
        conflictingBlockIDs = try container.decode([UUID].self, forKey: .conflictingBlockIDs)
        conflictingBlocks = try container.decode([DayWeaveDeferConflict].self, forKey: .conflictingBlocks)
        start = try container.decode(Date.self, forKey: .start)
        end = try container.decode(Date.self, forKey: .end)
        boundaryStart = try container.decodeIfPresent(Date.self, forKey: .boundaryStart)
        boundaryEnd = try container.decodeIfPresent(Date.self, forKey: .boundaryEnd)
        message = try container.decode(String.self, forKey: .message)
        guard hasValidShape else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Defer violation violates the supported contract"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw executionEncodingError(
                codingPath: encoder.codingPath,
                "Defer violation violates the supported contract"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(code, forKey: .code)
        try container.encode(itemIDs, forKey: .itemIDs)
        try container.encode(occurrenceIDs, forKey: .occurrenceIDs)
        try container.encode(conflictingBlockIDs, forKey: .conflictingBlockIDs)
        try container.encode(conflictingBlocks, forKey: .conflictingBlocks)
        try container.encode(start, forKey: .start)
        try container.encode(end, forKey: .end)
        try encodeExecutionNullable(boundaryStart, forKey: .boundaryStart, into: &container)
        try encodeExecutionNullable(boundaryEnd, forKey: .boundaryEnd, into: &container)
        try container.encode(message, forKey: .message)
    }

    var hasValidShape: Bool {
        itemIDs.count <= 10_000
            && Set(itemIDs).count == itemIDs.count
            && itemIDs.allSatisfy { !$0.isDayWeaveNil }
            && occurrenceIDs.count <= 10_000
            && Set(occurrenceIDs).count == occurrenceIDs.count
            && occurrenceIDs.allSatisfy { !$0.isDayWeaveNil }
            && conflictingBlockIDs.count <= 10_000
            && Set(conflictingBlockIDs).count == conflictingBlockIDs.count
            && conflictingBlockIDs.allSatisfy { !$0.isDayWeaveNil }
            && conflictingBlocks.count <= 10_000
            && conflictingBlocks.allSatisfy(\.hasValidShape)
            && Set(conflictingBlocks.map(\.blockID)).count == conflictingBlocks.count
            && Set(conflictingBlocks.map(\.blockID)) == Set(conflictingBlockIDs)
            && executionHasPostgresTimestampPrecision(start)
            && executionHasPostgresTimestampPrecision(end)
            && start < end
            && boundaryStart.map(executionHasPostgresTimestampPrecision) ?? true
            && boundaryEnd.map(executionHasPostgresTimestampPrecision) ?? true
            && !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && message.unicodeScalars.count <= 1_000
    }
}

struct DayWeaveDeferAssessment: Codable, Equatable, Sendable {
    static let maximumViolationCount = 10_000

    let sessionID: UUID
    let executionRevision: UInt64
    let sessionRevision: UInt64
    let itemID: UUID
    let itemRevision: UInt64
    let occurrenceID: UUID?
    let sourceSessionIndex: UInt16
    let replacementSessionIndex: UInt16
    let sourceScheduleRevisionID: UUID
    let sourceBlockID: UUID
    let actualSeconds: UInt64
    let creditedSourceSeconds: UInt64
    let plannedDurationSeconds: UInt64
    let remainingDurationSeconds: UInt64
    let moveStart: Date
    let moveEnd: Date
    let environmentDigest: String
    let assessmentDigest: String
    let approvalRequired: Bool
    let violations: [DayWeaveDeferViolation]
    let expiresAt: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case sessionID = "session_id"
        case executionRevision = "execution_revision"
        case sessionRevision = "session_revision"
        case itemID = "item_id"
        case itemRevision = "item_revision"
        case occurrenceID = "occurrence_id"
        case sourceSessionIndex = "source_session_index"
        case replacementSessionIndex = "replacement_session_index"
        case sourceScheduleRevisionID = "source_schedule_revision_id"
        case sourceBlockID = "source_block_id"
        case actualSeconds = "actual_seconds"
        case creditedSourceSeconds = "credited_source_seconds"
        case plannedDurationSeconds = "planned_duration_seconds"
        case remainingDurationSeconds = "remaining_duration_seconds"
        case moveStart = "move_start"
        case moveEnd = "move_end"
        case environmentDigest = "environment_digest"
        case assessmentDigest = "assessment_digest"
        case approvalRequired = "approval_required"
        case violations
        case expiresAt = "expires_at"
    }

    init(
        sessionID: UUID,
        executionRevision: UInt64,
        sessionRevision: UInt64,
        itemID: UUID,
        itemRevision: UInt64,
        occurrenceID: UUID?,
        sourceSessionIndex: UInt16,
        replacementSessionIndex: UInt16,
        sourceScheduleRevisionID: UUID,
        sourceBlockID: UUID,
        actualSeconds: UInt64,
        creditedSourceSeconds: UInt64,
        plannedDurationSeconds: UInt64,
        remainingDurationSeconds: UInt64,
        moveStart: Date,
        moveEnd: Date,
        environmentDigest: String,
        assessmentDigest: String,
        approvalRequired: Bool,
        violations: [DayWeaveDeferViolation],
        expiresAt: Date
    ) {
        self.sessionID = sessionID
        self.executionRevision = executionRevision
        self.sessionRevision = sessionRevision
        self.itemID = itemID
        self.itemRevision = itemRevision
        self.occurrenceID = occurrenceID
        self.sourceSessionIndex = sourceSessionIndex
        self.replacementSessionIndex = replacementSessionIndex
        self.sourceScheduleRevisionID = sourceScheduleRevisionID
        self.sourceBlockID = sourceBlockID
        self.actualSeconds = actualSeconds
        self.creditedSourceSeconds = creditedSourceSeconds
        self.plannedDurationSeconds = plannedDurationSeconds
        self.remainingDurationSeconds = remainingDurationSeconds
        self.moveStart = moveStart
        self.moveEnd = moveEnd
        self.environmentDigest = environmentDigest
        self.assessmentDigest = assessmentDigest
        self.approvalRequired = approvalRequired
        self.violations = violations
        self.expiresAt = expiresAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessionID = try container.decode(UUID.self, forKey: .sessionID)
        executionRevision = try container.decode(UInt64.self, forKey: .executionRevision)
        sessionRevision = try container.decode(UInt64.self, forKey: .sessionRevision)
        itemID = try container.decode(UUID.self, forKey: .itemID)
        itemRevision = try container.decode(UInt64.self, forKey: .itemRevision)
        occurrenceID = try container.decodeIfPresent(UUID.self, forKey: .occurrenceID)
        sourceSessionIndex = try container.decode(UInt16.self, forKey: .sourceSessionIndex)
        replacementSessionIndex = try container.decode(UInt16.self, forKey: .replacementSessionIndex)
        sourceScheduleRevisionID = try container.decode(UUID.self, forKey: .sourceScheduleRevisionID)
        sourceBlockID = try container.decode(UUID.self, forKey: .sourceBlockID)
        actualSeconds = try container.decode(UInt64.self, forKey: .actualSeconds)
        creditedSourceSeconds = try container.decode(UInt64.self, forKey: .creditedSourceSeconds)
        plannedDurationSeconds = try container.decode(UInt64.self, forKey: .plannedDurationSeconds)
        remainingDurationSeconds = try container.decode(UInt64.self, forKey: .remainingDurationSeconds)
        moveStart = try container.decode(Date.self, forKey: .moveStart)
        moveEnd = try container.decode(Date.self, forKey: .moveEnd)
        environmentDigest = try container.decode(String.self, forKey: .environmentDigest)
        assessmentDigest = try container.decode(String.self, forKey: .assessmentDigest)
        approvalRequired = try container.decode(Bool.self, forKey: .approvalRequired)
        violations = try container.decode([DayWeaveDeferViolation].self, forKey: .violations)
        expiresAt = try container.decode(Date.self, forKey: .expiresAt)
        guard hasValidShape else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Defer assessment violates the supported contract"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw executionEncodingError(
                codingPath: encoder.codingPath,
                "Defer assessment violates the supported contract"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessionID, forKey: .sessionID)
        try container.encode(executionRevision, forKey: .executionRevision)
        try container.encode(sessionRevision, forKey: .sessionRevision)
        try container.encode(itemID, forKey: .itemID)
        try container.encode(itemRevision, forKey: .itemRevision)
        try encodeExecutionNullable(occurrenceID, forKey: .occurrenceID, into: &container)
        try container.encode(sourceSessionIndex, forKey: .sourceSessionIndex)
        try container.encode(replacementSessionIndex, forKey: .replacementSessionIndex)
        try container.encode(sourceScheduleRevisionID, forKey: .sourceScheduleRevisionID)
        try container.encode(sourceBlockID, forKey: .sourceBlockID)
        try container.encode(actualSeconds, forKey: .actualSeconds)
        try container.encode(creditedSourceSeconds, forKey: .creditedSourceSeconds)
        try container.encode(plannedDurationSeconds, forKey: .plannedDurationSeconds)
        try container.encode(remainingDurationSeconds, forKey: .remainingDurationSeconds)
        try container.encode(moveStart, forKey: .moveStart)
        try container.encode(moveEnd, forKey: .moveEnd)
        try container.encode(environmentDigest, forKey: .environmentDigest)
        try container.encode(assessmentDigest, forKey: .assessmentDigest)
        try container.encode(approvalRequired, forKey: .approvalRequired)
        try container.encode(violations, forKey: .violations)
        try container.encode(expiresAt, forKey: .expiresAt)
    }

    var hasValidShape: Bool {
        guard !sessionID.isDayWeaveNil,
              executionRevision > 0,
              executionRevision <= UInt64(Int64.max),
              sessionRevision > 0,
              sessionRevision <= executionRevision,
              !itemID.isDayWeaveNil,
              itemRevision > 0,
              itemRevision <= UInt64(Int64.max),
              !(occurrenceID?.isDayWeaveNil ?? false),
              replacementSessionIndex > sourceSessionIndex,
              !sourceScheduleRevisionID.isDayWeaveNil,
              !sourceBlockID.isDayWeaveNil,
              actualSeconds <= UInt64(Int64.max),
              plannedDurationSeconds > 0,
              plannedDurationSeconds <= 86_400,
              creditedSourceSeconds <= plannedDurationSeconds,
              remainingDurationSeconds > 0,
              remainingDurationSeconds == plannedDurationSeconds - creditedSourceSeconds,
              dayWeaveExactWholeSecondDelta(from: moveStart, to: moveEnd)
                == remainingDurationSeconds,
              dayWeavePostgresEpochMicroseconds(moveStart).map({
                  $0 % 300_000_000 == 0
              }) == true,
              executionHasPostgresTimestampPrecision(moveEnd),
              executionHasPostgresTimestampPrecision(expiresAt),
              expiresAt < moveStart,
              isCanonicalExecutionDigest(environmentDigest),
              isCanonicalExecutionDigest(assessmentDigest),
              violations.count <= Self.maximumViolationCount,
              violations.allSatisfy(\.hasValidShape),
              approvalRequired == !violations.isEmpty else { return false }
        return true
    }
}

struct DayWeaveDeferAssessmentEnvelope: Decodable, Sendable {
    let assessment: DayWeaveDeferAssessment

    private enum CodingKeys: String, CodingKey, CaseIterable { case assessment }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        assessment = try decoder.container(keyedBy: CodingKeys.self)
            .decode(DayWeaveDeferAssessment.self, forKey: .assessment)
    }
}

/// The server's assessment evidence lives for five minutes. A newly selected
/// target must leave that entire TTL plus one complete placement slot so a
/// slow response never collapses the user's replacement window into expiry.
enum DayWeaveExecutionDeferTiming {
    static let slotSeconds: TimeInterval = 5 * 60
    static let assessmentTTLSeconds: TimeInterval = 5 * 60
    static let minimumLeadSeconds = assessmentTTLSeconds + slotSeconds

    static func roundedUpToSlot(_ date: Date) -> Date {
        Date(
            timeIntervalSince1970: (
                date.timeIntervalSince1970 / slotSeconds
            ).rounded(.up) * slotSeconds
        )
    }

    static func minimumMoveStart(after referenceDate: Date) -> Date {
        roundedUpToSlot(referenceDate.addingTimeInterval(minimumLeadSeconds))
    }

    static func isAligned(_ date: Date) -> Bool {
        dayWeavePostgresEpochMicroseconds(date).map {
            $0 % 300_000_000 == 0
        } == true
    }

    static func isValidNewMoveStart(_ moveStart: Date, now: Date) -> Bool {
        isAligned(moveStart)
            && moveStart >= now.addingTimeInterval(minimumLeadSeconds)
    }
}

enum DayWeaveExecutionCommand: Codable, Equatable, Sendable {
    case start(
        sessionID: UUID,
        itemID: UUID,
        itemRevision: UInt64,
        occurrenceID: UUID?,
        sessionIndex: UInt16,
        plannedBlockID: UUID?,
        deviceID: UUID
    )
    case pause(sessionID: UUID, durationSeconds: UInt32?, pauseUntil: Date?, reason: String?)
    case resume(sessionID: UUID)
    case complete(sessionID: UUID, actualSeconds: UInt64?)
    case skip(sessionID: UUID, actualSeconds: UInt64?)
    case deferWork(
        sessionID: UUID,
        moveStart: Date,
        moveEnd: Date,
        actualSeconds: UInt64?,
        assessmentDigest: String? = nil,
        approvedAssessmentDigest: String? = nil
    )

    private enum CodingKeys: String, CodingKey {
        case type
        case sessionID = "session_id"
        case itemID = "item_id"
        case itemRevision = "item_revision"
        case occurrenceID = "occurrence_id"
        case sessionIndex = "session_index"
        case plannedBlockID = "planned_block_id"
        case deviceID = "device_id"
        case durationSeconds = "duration_seconds"
        case pauseUntil = "pause_until"
        case reason
        case actualSeconds = "actual_seconds"
        case moveStart = "move_start"
        case moveEnd = "move_end"
        case assessmentDigest = "assessment_digest"
        case approvedAssessmentDigest = "approved_assessment_digest"
    }

    var sessionID: UUID {
        switch self {
        case let .start(sessionID, _, _, _, _, _, _),
             let .pause(sessionID, _, _, _),
             let .resume(sessionID),
             let .complete(sessionID, _),
             let .skip(sessionID, _),
             let .deferWork(sessionID, _, _, _, _, _):
            sessionID
        }
    }

    func matchesChangedSession(_ session: DayWeaveExecutionSession) -> Bool {
        guard session.id == sessionID else { return false }
        switch self {
        case let .start(
            _, itemID, itemRevision, occurrenceID, sessionIndex, plannedBlockID, deviceID
        ):
            return session.status == .active
                && session.itemID == itemID
                && session.itemRevision == itemRevision
                && session.occurrenceID == occurrenceID
                && session.sessionIndex == sessionIndex
                && session.plannedBlockID == plannedBlockID
                && session.sourceDeviceID == deviceID
                && session.revision == 1
                && session.accumulatedSeconds == 0
        case let .pause(_, durationSeconds, pauseUntil, reason):
            let pauseShapeMatches = if let durationSeconds {
                session.pauseUntil
                    == session.updatedAt.addingTimeInterval(TimeInterval(durationSeconds))
            } else if let pauseUntil {
                session.pauseUntil == pauseUntil
            } else {
                session.pauseUntil == nil
            }
            return session.status == .paused
                && pauseShapeMatches
                && reason.map { session.pauseReason == $0 } ?? true
        case .resume:
            return session.status == .active && session.revision >= 3
        case let .complete(_, actualSeconds):
            return session.status == .completed
                && actualSeconds.map { session.actualSeconds == $0 } ?? true
        case let .skip(_, actualSeconds):
            return session.status == .skipped
                && actualSeconds.map { session.actualSeconds == $0 } ?? true
        case let .deferWork(_, moveStart, moveEnd, actualSeconds, _, _):
            return session.status == .deferred
                && session.moveStart == moveStart
                && session.moveEnd == moveEnd
                && (actualSeconds.map { session.actualSeconds == $0 }
                    ?? (session.actualSeconds == session.accumulatedSeconds))
        }
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: ExecutionDynamicCodingKey.self)
        guard let typeKey = ExecutionDynamicCodingKey(stringValue: "type"),
              let type = try? dynamic.decode(String.self, forKey: typeKey) else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Execution command type is missing or invalid"
            )
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch type {
        case "start":
            try requireExecutionKeyShape(
                required: ["type", "session_id", "item_id", "item_revision", "session_index", "device_id"],
                optional: ["occurrence_id", "planned_block_id"],
                from: decoder
            )
            self = .start(
                sessionID: try container.decode(UUID.self, forKey: .sessionID),
                itemID: try container.decode(UUID.self, forKey: .itemID),
                itemRevision: try container.decode(UInt64.self, forKey: .itemRevision),
                occurrenceID: try container.decodeIfPresent(UUID.self, forKey: .occurrenceID),
                sessionIndex: try container.decode(UInt16.self, forKey: .sessionIndex),
                plannedBlockID: try container.decodeIfPresent(UUID.self, forKey: .plannedBlockID),
                deviceID: try container.decode(UUID.self, forKey: .deviceID)
            )
        case "pause":
            try requireExecutionKeyShape(
                required: ["type", "session_id"],
                optional: ["duration_seconds", "pause_until", "reason"],
                from: decoder
            )
            self = .pause(
                sessionID: try container.decode(UUID.self, forKey: .sessionID),
                durationSeconds: try container.decodeIfPresent(UInt32.self, forKey: .durationSeconds),
                pauseUntil: try container.decodeIfPresent(Date.self, forKey: .pauseUntil),
                reason: try container.decodeIfPresent(String.self, forKey: .reason)
            )
        case "resume":
            try requireExecutionKeyShape(
                required: ["type", "session_id"],
                optional: [],
                from: decoder
            )
            self = .resume(sessionID: try container.decode(UUID.self, forKey: .sessionID))
        case "complete", "skip":
            try requireExecutionKeyShape(
                required: ["type", "session_id"],
                optional: ["actual_seconds"],
                from: decoder
            )
            let sessionID = try container.decode(UUID.self, forKey: .sessionID)
            let actualSeconds = try container.decodeIfPresent(UInt64.self, forKey: .actualSeconds)
            self = type == "complete"
                ? .complete(sessionID: sessionID, actualSeconds: actualSeconds)
                : .skip(sessionID: sessionID, actualSeconds: actualSeconds)
        case "defer":
            try requireExecutionKeyShape(
                required: ["type", "session_id", "move_start", "move_end"],
                optional: [
                    "actual_seconds",
                    "assessment_digest",
                    "approved_assessment_digest",
                ],
                from: decoder
            )
            let rawMoveStart = try? container.decode(String.self, forKey: .moveStart)
            let rawMoveEnd = try? container.decode(String.self, forKey: .moveEnd)
            let rawTimestampsAreValid = switch (rawMoveStart, rawMoveEnd) {
            case let (.some(start), .some(end)):
                executionWireTimestampHasPostgresPrecision(start)
                    && executionWireTimestampHasPostgresPrecision(end)
            case (.none, .none):
                // Encrypted snapshots use the snapshot decoder's numeric Date
                // strategy. The exact wire bytes are independently decoded and
                // compared by PlannerStore's pending-command invariant.
                true
            case (.some, .none), (.none, .some):
                false
            }
            guard rawTimestampsAreValid else {
                throw executionDecodingError(
                    codingPath: decoder.codingPath,
                    "Defer timestamps exceed PostgreSQL microsecond precision"
                )
            }
            self = .deferWork(
                sessionID: try container.decode(UUID.self, forKey: .sessionID),
                moveStart: try container.decode(Date.self, forKey: .moveStart),
                moveEnd: try container.decode(Date.self, forKey: .moveEnd),
                actualSeconds: try container.decodeIfPresent(UInt64.self, forKey: .actualSeconds),
                assessmentDigest: try container.decodeIfPresent(
                    String.self,
                    forKey: .assessmentDigest
                ),
                approvedAssessmentDigest: try container.decodeIfPresent(
                    String.self,
                    forKey: .approvedAssessmentDigest
                )
            )
        default:
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Execution command type is unsupported"
            )
        }
        try validate(codingPath: decoder.codingPath, replayingPersistedBytes: true)
    }

    func encode(to encoder: any Encoder) throws {
        try encode(to: encoder, replayingPersistedBytes: false)
    }

    /// Snapshot journals may contain a command accepted under an older wire
    /// contract. Its immutable request bytes remain the replay authority, so
    /// snapshot re-encryption must not reinterpret that historical command as
    /// a newly authored request.
    fileprivate func encodePersisted(to encoder: any Encoder) throws {
        try encode(to: encoder, replayingPersistedBytes: true)
    }

    private func encode(
        to encoder: any Encoder,
        replayingPersistedBytes: Bool
    ) throws {
        try validate(
            codingPath: encoder.codingPath,
            replayingPersistedBytes: replayingPersistedBytes
        )
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .start(
            sessionID,
            itemID,
            itemRevision,
            occurrenceID,
            sessionIndex,
            plannedBlockID,
            deviceID
        ):
            try container.encode("start", forKey: .type)
            try container.encode(sessionID, forKey: .sessionID)
            try container.encode(itemID, forKey: .itemID)
            try container.encode(itemRevision, forKey: .itemRevision)
            try container.encodeIfPresent(occurrenceID, forKey: .occurrenceID)
            try container.encode(sessionIndex, forKey: .sessionIndex)
            try container.encodeIfPresent(plannedBlockID, forKey: .plannedBlockID)
            try container.encode(deviceID, forKey: .deviceID)
        case let .pause(sessionID, durationSeconds, pauseUntil, reason):
            try container.encode("pause", forKey: .type)
            try container.encode(sessionID, forKey: .sessionID)
            try container.encodeIfPresent(durationSeconds, forKey: .durationSeconds)
            try container.encodeIfPresent(pauseUntil, forKey: .pauseUntil)
            try container.encodeIfPresent(reason, forKey: .reason)
        case let .resume(sessionID):
            try container.encode("resume", forKey: .type)
            try container.encode(sessionID, forKey: .sessionID)
        case let .complete(sessionID, actualSeconds):
            try container.encode("complete", forKey: .type)
            try container.encode(sessionID, forKey: .sessionID)
            try container.encodeIfPresent(actualSeconds, forKey: .actualSeconds)
        case let .skip(sessionID, actualSeconds):
            try container.encode("skip", forKey: .type)
            try container.encode(sessionID, forKey: .sessionID)
            try container.encodeIfPresent(actualSeconds, forKey: .actualSeconds)
        case let .deferWork(
            sessionID,
            moveStart,
            moveEnd,
            actualSeconds,
            assessmentDigest,
            approvedAssessmentDigest
        ):
            try container.encode("defer", forKey: .type)
            try container.encode(sessionID, forKey: .sessionID)
            try container.encode(moveStart, forKey: .moveStart)
            try container.encode(moveEnd, forKey: .moveEnd)
            try container.encodeIfPresent(actualSeconds, forKey: .actualSeconds)
            try container.encodeIfPresent(assessmentDigest, forKey: .assessmentDigest)
            try container.encodeIfPresent(
                approvedAssessmentDigest,
                forKey: .approvedAssessmentDigest
            )
        }
    }

    private func validate(
        codingPath: [any CodingKey],
        replayingPersistedBytes: Bool
    ) throws {
        let valid: Bool
        switch self {
        case let .start(
            sessionID,
            itemID,
            itemRevision,
            occurrenceID,
            _,
            plannedBlockID,
            deviceID
        ):
            valid = !sessionID.isDayWeaveNil
                && !itemID.isDayWeaveNil
                && itemRevision > 0
                && itemRevision <= UInt64(Int64.max)
                && !(occurrenceID?.isDayWeaveNil ?? false)
                && !(plannedBlockID?.isDayWeaveNil ?? false)
                && !deviceID.isDayWeaveNil
        case let .pause(sessionID, durationSeconds, pauseUntil, reason):
            let now = Date()
            let pauseShapeIsValid = switch (durationSeconds, pauseUntil) {
            case let (.some(seconds), .none): (1...86_400).contains(seconds)
            case let (.none, .some(until)):
                replayingPersistedBytes
                    || (until > now && until <= now.addingTimeInterval(86_400))
            case (.none, .none): true
            case (.some, .some): false
            }
            let reasonIsValid = reason.map {
                !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    && $0.unicodeScalars.count <= 500
            } ?? true
            valid = !sessionID.isDayWeaveNil && pauseShapeIsValid && reasonIsValid
        case let .resume(sessionID):
            valid = !sessionID.isDayWeaveNil
        case let .complete(sessionID, actualSeconds), let .skip(sessionID, actualSeconds):
            valid = !sessionID.isDayWeaveNil
                && actualSeconds.map { $0 <= UInt64(Int64.max) } ?? true
        case let .deferWork(
            sessionID,
            moveStart,
            moveEnd,
            actualSeconds,
            assessmentDigest,
            approvedAssessmentDigest
        ):
            let exactDuration = dayWeaveExactWholeSecondDelta(from: moveStart, to: moveEnd)
            let actualIsValid = actualSeconds.map({ $0 <= UInt64(Int64.max) })
                ?? replayingPersistedBytes
            let assessmentIsValid = assessmentDigest.map(isCanonicalExecutionDigest)
                ?? replayingPersistedBytes
            let approvalIsValid = approvedAssessmentDigest.map(isCanonicalExecutionDigest) ?? true
            let approvalMatches = approvedAssessmentDigest == nil
                || approvedAssessmentDigest == assessmentDigest
            valid = !sessionID.isDayWeaveNil
                && moveStart.timeIntervalSinceReferenceDate.isFinite
                && moveEnd.timeIntervalSinceReferenceDate.isFinite
                && executionHasPostgresTimestampPrecision(moveStart)
                && executionHasPostgresTimestampPrecision(moveEnd)
                && (replayingPersistedBytes || moveStart > Date())
                && exactDuration.map { $0 <= 86_400 } == true
                && actualIsValid
                && assessmentIsValid
                && approvalIsValid
                && approvalMatches
        }
        guard valid else {
            throw EncodingError.invalidValue(
                self,
                .init(
                    codingPath: codingPath,
                    debugDescription: "Execution command violates the supported API contract"
                )
            )
        }
    }
}

struct DayWeaveExecutionCommandRequest: Codable, Equatable, Sendable {
    let expectedRevision: UInt64
    let command: DayWeaveExecutionCommand

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case expectedRevision = "expected_revision"
        case command
    }

    init(expectedRevision: UInt64, command: DayWeaveExecutionCommand) {
        self.expectedRevision = expectedRevision
        self.command = command
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        command = try container.decode(DayWeaveExecutionCommand.self, forKey: .command)
        guard expectedRevision < UInt64(Int64.max) else {
            throw executionDecodingError(
                codingPath: decoder.codingPath,
                "Execution revision exceeds the server persistence range"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard expectedRevision < UInt64(Int64.max) else {
            throw EncodingError.invalidValue(
                expectedRevision,
                .init(
                    codingPath: encoder.codingPath,
                    debugDescription: "Execution revision exceeds the server persistence range"
                )
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(expectedRevision, forKey: .expectedRevision)
        try container.encode(command, forKey: .command)
    }
}

struct DayWeaveExecutionIdentity: Codable, Equatable, Hashable, Sendable {
    let sessionID: UUID
    let itemID: UUID
    let itemRevision: UInt64
    let occurrenceID: UUID?
    let sessionIndex: UInt16
    let plannedBlockID: UUID?
    let sourceDeviceID: UUID

    init(session: DayWeaveExecutionSession) {
        sessionID = session.id
        itemID = session.itemID
        itemRevision = session.itemRevision
        occurrenceID = session.occurrenceID
        sessionIndex = session.sessionIndex
        plannedBlockID = session.plannedBlockID
        sourceDeviceID = session.sourceDeviceID
    }

    init(
        sessionID: UUID,
        itemID: UUID,
        itemRevision: UInt64,
        occurrenceID: UUID?,
        sessionIndex: UInt16,
        plannedBlockID: UUID?,
        sourceDeviceID: UUID
    ) {
        self.sessionID = sessionID
        self.itemID = itemID
        self.itemRevision = itemRevision
        self.occurrenceID = occurrenceID
        self.sessionIndex = sessionIndex
        self.plannedBlockID = plannedBlockID
        self.sourceDeviceID = sourceDeviceID
    }

    func matches(_ session: DayWeaveExecutionSession) -> Bool {
        self == Self(session: session)
    }

    var hasValidShape: Bool {
        !sessionID.isDayWeaveNil
            && !itemID.isDayWeaveNil
            && itemRevision > 0
            && itemRevision <= UInt64(Int64.max)
            && !(occurrenceID?.isDayWeaveNil ?? false)
            && !(plannedBlockID?.isDayWeaveNil ?? false)
            && !sourceDeviceID.isDayWeaveNil
    }
}

struct DayWeavePendingExecutionCommand: Codable, Equatable, Sendable {
    let idempotencyKey: String
    let bindingIdentifier: String
    let expectedRevision: UInt64
    let identity: DayWeaveExecutionIdentity
    let command: DayWeaveExecutionCommand
    let encodedRequest: Data
    let priorSession: DayWeaveExecutionSession?
    let focusedBlockID: UUID
    let canonicalProjectionEligibleAtLeaseStart: Bool
    let stagedAt: Date

    private enum CodingKeys: String, CodingKey {
        case idempotencyKey
        case bindingIdentifier
        case expectedRevision
        case identity
        case command
        case encodedRequest
        case priorSession
        case focusedBlockID
        case canonicalProjectionEligibleAtLeaseStart
        case stagedAt
    }

    init(
        idempotencyKey: String,
        bindingIdentifier: String,
        expectedRevision: UInt64,
        identity: DayWeaveExecutionIdentity,
        command: DayWeaveExecutionCommand,
        encodedRequest: Data,
        priorSession: DayWeaveExecutionSession?,
        focusedBlockID: UUID,
        canonicalProjectionEligibleAtLeaseStart: Bool,
        stagedAt: Date
    ) {
        self.idempotencyKey = idempotencyKey
        self.bindingIdentifier = bindingIdentifier
        self.expectedRevision = expectedRevision
        self.identity = identity
        self.command = command
        self.encodedRequest = encodedRequest
        self.priorSession = priorSession
        self.focusedBlockID = focusedBlockID
        self.canonicalProjectionEligibleAtLeaseStart = canonicalProjectionEligibleAtLeaseStart
        self.stagedAt = stagedAt
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        idempotencyKey = try container.decode(String.self, forKey: .idempotencyKey)
        bindingIdentifier = try container.decode(String.self, forKey: .bindingIdentifier)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        identity = try container.decode(DayWeaveExecutionIdentity.self, forKey: .identity)
        command = try container.decode(DayWeaveExecutionCommand.self, forKey: .command)
        encodedRequest = try container.decode(Data.self, forKey: .encodedRequest)
        priorSession = try container.decodeIfPresent(
            DayWeaveExecutionSession.self,
            forKey: .priorSession
        )
        focusedBlockID = try container.decode(UUID.self, forKey: .focusedBlockID)
        canonicalProjectionEligibleAtLeaseStart = try container.decode(
            Bool.self,
            forKey: .canonicalProjectionEligibleAtLeaseStart
        )
        stagedAt = try container.decode(Date.self, forKey: .stagedAt)
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(idempotencyKey, forKey: .idempotencyKey)
        try container.encode(bindingIdentifier, forKey: .bindingIdentifier)
        try container.encode(expectedRevision, forKey: .expectedRevision)
        try container.encode(identity, forKey: .identity)
        try command.encodePersisted(to: container.superEncoder(forKey: .command))
        try container.encode(encodedRequest, forKey: .encodedRequest)
        try container.encodeIfPresent(priorSession, forKey: .priorSession)
        try container.encode(focusedBlockID, forKey: .focusedBlockID)
        try container.encode(
            canonicalProjectionEligibleAtLeaseStart,
            forKey: .canonicalProjectionEligibleAtLeaseStart
        )
        try container.encode(stagedAt, forKey: .stagedAt)
    }
}

/// High-level crash-recovery intent spanning the Pause -> Defer command pair.
/// Each individual server request still owns its exact idempotency journal;
/// this record preserves the user's selected placement between those commands.
struct DayWeaveMoveConflictIdentity: Codable, Equatable, Hashable, Sendable {
    let id: UUID
    let sourceItemRevision: UInt64?
    let start: Date
    let end: Date
    let kind: PlannerItemKind
    let previewKind: String?
    let isFlexible: Bool
    let isHardConstraint: Bool

    init(block: ScheduleBlock) {
        id = block.id
        sourceItemRevision = block.sourceItemRevision
        start = block.start
        end = block.end
        kind = block.kind
        previewKind = block.previewKind
        isFlexible = block.isFlexible
        isHardConstraint = block.isHardConstraint
    }

    var hasValidShape: Bool {
        !id.isDayWeaveNil
            && sourceItemRevision.map({ $0 > 0 && $0 <= UInt64(Int64.max) }) ?? true
            && dayWeavePostgresEpochMicroseconds(start) != nil
            && dayWeavePostgresEpochMicroseconds(end) != nil
            && start < end
            && previewKind.map({ !$0.isEmpty && $0.utf8.count <= 64 }) ?? true
            && (isHardConstraint
                || !isFlexible
                || kind == .event
                || ["pinned", "calendar_event", "external_fixed"]
                    .contains(previewKind ?? ""))
    }
}

struct DayWeaveMoveDeadlineIdentity: Codable, Equatable, Hashable, Sendable {
    let itemID: UUID
    let itemRevision: UInt64
    let boundary: DayWeaveMoveDeadlineBoundary

    var hasValidShape: Bool {
        !itemID.isDayWeaveNil
            && itemRevision > 0
            && itemRevision <= UInt64(Int64.max)
            && boundary.hasValidShape
    }
}

/// Exact risk snapshot shown by the move sheet. Approval is valid only while
/// the target window, parsed deadline, and protected-block identities still
/// match this value.
struct DayWeaveMoveRiskEnvelope: Equatable, Sendable {
    let moveStart: Date
    let moveEnd: Date
    let deadlines: Set<DayWeaveMoveDeadlineIdentity>
    let fixedConflicts: Set<DayWeaveMoveConflictIdentity>
    let sourceRequiresOverride: Bool

    var hasValidShape: Bool {
        guard let startMicros = dayWeavePostgresEpochMicroseconds(moveStart),
              let endMicros = dayWeavePostgresEpochMicroseconds(moveEnd),
              startMicros < endMicros,
              deadlines.count <= DayWeavePendingExecutionDeferIntent.maximumDeadlineCount,
              deadlines.allSatisfy(\.hasValidShape),
              Set(deadlines.map(\.itemID)).count == deadlines.count,
              fixedConflicts.count <= DayWeavePendingExecutionDeferIntent.maximumConflictCount,
              fixedConflicts.allSatisfy(\.hasValidShape),
              Set(fixedConflicts.map(\.id)).count == fixedConflicts.count else { return false }
        return true
    }
}

struct DayWeavePendingExecutionDeferIntent: Codable, Equatable, Sendable {
    static let currentVersion = 7
    static let maximumConflictCount = 10_000
    static let maximumDeadlineCount = 10_000

    let version: Int
    let identity: DayWeaveExecutionIdentity
    let focusedBlockID: UUID
    let sourceStart: Date
    let sourceEnd: Date
    let moveStart: Date
    /// The longest target window the user reviewed before a running session
    /// was paused. The exact server-authoritative remainder may be shorter.
    let approvedMoveEnd: Date
    let approvedDeadlines: Set<DayWeaveMoveDeadlineIdentity>
    let deadlineConflictApproved: Bool
    let approvedFixedConflicts: Set<DayWeaveMoveConflictIdentity>
    let fixedConflictApproved: Bool
    let sourceOverrideApproved: Bool
    /// Server-issued, content-free evidence for this exact paused revision and
    /// target. It is intentionally separate from the immutable command journal:
    /// once a command is staged, replay uses its retained bytes instead.
    let assessment: DayWeaveDeferAssessment?
    /// Exact explicit approval for `assessment`. A replacement assessment is
    /// always stored with this field cleared before any command can be staged.
    let approvedAssessmentDigest: String?
    let createdAt: Date
    let expiresAt: Date

    private enum CodingKeys: String, CodingKey {
        case version
        case identity
        case focusedBlockID
        case sourceStart
        case sourceEnd
        case moveStart
        case approvedMoveEnd
        case approvedDeadlines
        case deadlineConflictApproved
        case approvedFixedConflicts
        case fixedConflictApproved
        case sourceOverrideApproved
        case assessment
        case approvedAssessmentDigest
        case createdAt
        case expiresAt
    }

    init(
        version: Int = Self.currentVersion,
        identity: DayWeaveExecutionIdentity,
        focusedBlockID: UUID,
        sourceStart: Date,
        sourceEnd: Date,
        moveStart: Date,
        approvedMoveEnd: Date,
        approvedDeadlines: Set<DayWeaveMoveDeadlineIdentity>,
        deadlineConflictApproved: Bool,
        approvedFixedConflicts: Set<DayWeaveMoveConflictIdentity>,
        fixedConflictApproved: Bool,
        sourceOverrideApproved: Bool,
        assessment: DayWeaveDeferAssessment? = nil,
        approvedAssessmentDigest: String? = nil,
        createdAt: Date,
        expiresAt: Date
    ) {
        self.version = version
        self.identity = identity
        self.focusedBlockID = focusedBlockID
        self.sourceStart = sourceStart
        self.sourceEnd = sourceEnd
        self.moveStart = moveStart
        self.approvedMoveEnd = approvedMoveEnd
        self.approvedDeadlines = approvedDeadlines
        self.deadlineConflictApproved = deadlineConflictApproved
        self.approvedFixedConflicts = approvedFixedConflicts
        self.fixedConflictApproved = fixedConflictApproved
        self.sourceOverrideApproved = sourceOverrideApproved
        self.assessment = assessment
        self.approvedAssessmentDigest = approvedAssessmentDigest
        self.createdAt = createdAt
        self.expiresAt = expiresAt
    }

    /// Earlier versions predated the complete durable approval envelope. They
    /// remain decodable so
    /// an encrypted planner snapshot cannot be quarantined during an upgrade,
    /// but its deliberately empty envelope can never authorize a Defer.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(Int.self, forKey: .version)
        identity = try container.decode(DayWeaveExecutionIdentity.self, forKey: .identity)
        focusedBlockID = try container.decode(UUID.self, forKey: .focusedBlockID)
        moveStart = try container.decode(Date.self, forKey: .moveStart)
        sourceStart = try container.decodeIfPresent(Date.self, forKey: .sourceStart) ?? moveStart
        sourceEnd = try container.decodeIfPresent(Date.self, forKey: .sourceEnd) ?? moveStart
        approvedMoveEnd = try container.decodeIfPresent(
            Date.self,
            forKey: .approvedMoveEnd
        ) ?? moveStart
        approvedDeadlines = try container.decodeIfPresent(
            Set<DayWeaveMoveDeadlineIdentity>.self,
            forKey: .approvedDeadlines
        ) ?? []
        deadlineConflictApproved = try container.decodeIfPresent(
            Bool.self,
            forKey: .deadlineConflictApproved
        ) ?? false
        approvedFixedConflicts = try container.decodeIfPresent(
            Set<DayWeaveMoveConflictIdentity>.self,
            forKey: .approvedFixedConflicts
        ) ?? []
        fixedConflictApproved = try container.decodeIfPresent(
            Bool.self,
            forKey: .fixedConflictApproved
        ) ?? false
        sourceOverrideApproved = try container.decodeIfPresent(
            Bool.self,
            forKey: .sourceOverrideApproved
        ) ?? false
        assessment = try container.decodeIfPresent(
            DayWeaveDeferAssessment.self,
            forKey: .assessment
        )
        approvedAssessmentDigest = try container.decodeIfPresent(
            String.self,
            forKey: .approvedAssessmentDigest
        )
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        expiresAt = try container.decode(Date.self, forKey: .expiresAt)
    }

    var hasPersistableShape: Bool {
        guard identity.hasValidShape,
              identity.plannedBlockID == focusedBlockID,
              !focusedBlockID.isDayWeaveNil,
              let moveMicros = dayWeavePostgresEpochMicroseconds(moveStart),
              createdAt.timeIntervalSinceReferenceDate.isFinite,
              expiresAt.timeIntervalSinceReferenceDate.isFinite,
              createdAt < expiresAt else { return false }
        if version < Self.currentVersion {
            return (1...6).contains(version)
                && (version <= 5 || moveMicros % 300_000_000 == 0)
                && expiresAt <= moveStart
                && expiresAt <= createdAt.addingTimeInterval(86_400)
        }
        return hasValidShape
    }

    var hasValidShape: Bool {
        guard version == Self.currentVersion,
              identity.hasValidShape,
              identity.plannedBlockID == focusedBlockID,
              dayWeavePostgresEpochMicroseconds(sourceStart) != nil,
              dayWeavePostgresEpochMicroseconds(sourceEnd) != nil,
              dayWeaveExactWholeSecondDelta(from: sourceStart, to: sourceEnd)
                .map({ $0 <= 86_400 }) == true,
              let moveMicros = dayWeavePostgresEpochMicroseconds(moveStart),
              moveMicros % 300_000_000 == 0,
              approvedMoveEnd == moveStart,
              approvedDeadlines.isEmpty,
              !deadlineConflictApproved,
              approvedFixedConflicts.isEmpty,
              !fixedConflictApproved,
              !sourceOverrideApproved,
              createdAt.timeIntervalSinceReferenceDate.isFinite,
              expiresAt.timeIntervalSinceReferenceDate.isFinite,
              createdAt < expiresAt,
              expiresAt == moveStart else { return false }
        guard let assessment else { return approvedAssessmentDigest == nil }
        return assessment.hasValidShape
            && assessment.sessionID == identity.sessionID
            && assessment.itemID == identity.itemID
            && assessment.itemRevision == identity.itemRevision
            && assessment.occurrenceID == identity.occurrenceID
            && assessment.sourceSessionIndex == identity.sessionIndex
            && assessment.sourceBlockID == focusedBlockID
            && assessment.moveStart == moveStart
            && (approvedAssessmentDigest == nil
                || (assessment.approvalRequired
                    && approvedAssessmentDigest == assessment.assessmentDigest))
            && (!assessment.approvalRequired || !assessment.violations.isEmpty)
            && (assessment.approvalRequired || approvedAssessmentDigest == nil)
    }

    var approvalIsRequired: Bool {
        assessment?.approvalRequired == true && approvedAssessmentDigest == nil
    }

    func isSameRequest(as other: Self) -> Bool {
        version == Self.currentVersion
            && other.version == Self.currentVersion
            && identity == other.identity
            && focusedBlockID == other.focusedBlockID
            && sourceStart == other.sourceStart
            && sourceEnd == other.sourceEnd
            && moveStart == other.moveStart
            && createdAt == other.createdAt
            && expiresAt == other.expiresAt
    }

    func replacingAssessment(_ replacement: DayWeaveDeferAssessment?) -> Self {
        Self(
            identity: identity,
            focusedBlockID: focusedBlockID,
            sourceStart: sourceStart,
            sourceEnd: sourceEnd,
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            assessment: replacement,
            approvedAssessmentDigest: nil,
            createdAt: createdAt,
            expiresAt: expiresAt
        )
    }

    func approvingAssessment(digest: String) -> Self? {
        guard let assessment,
              assessment.approvalRequired,
              assessment.assessmentDigest == digest,
              isCanonicalExecutionDigest(digest) else { return nil }
        return Self(
            identity: identity,
            focusedBlockID: focusedBlockID,
            sourceStart: sourceStart,
            sourceEnd: sourceEnd,
            moveStart: moveStart,
            approvedMoveEnd: moveStart,
            approvedDeadlines: [],
            deadlineConflictApproved: false,
            approvedFixedConflicts: [],
            fixedConflictApproved: false,
            sourceOverrideApproved: false,
            assessment: assessment,
            approvedAssessmentDigest: digest,
            createdAt: createdAt,
            expiresAt: expiresAt
        )
    }
}

enum DayWeaveTerminalProjectionState: Codable, Equatable, Sendable {
    case notRequired
    case pending
    case conflicted(String)
    case retryAuthorized
    case applied(revision: UInt64)
    case keptLatest

    var blocksCredentialReplacement: Bool {
        switch self {
        case .pending, .retryAuthorized: true
        case .notRequired, .conflicted, .applied, .keptLatest: false
        }
    }
}

struct DayWeaveTerminalExecutionOutcome: Codable, Equatable, Sendable {
    let session: DayWeaveExecutionSession
    let recordedAt: Date
    var projection: DayWeaveTerminalProjectionState
}

struct DayWeaveExecutionSessionVersion: Codable, Equatable, Sendable {
    let sessionID: UUID
    let revision: UInt64
}

/// Encrypted crash-recovery state for the one server-authoritative execution
/// lease. Terminal rows are a lifetime ledger for this credential binding and
/// intentionally have no age/count eviction policy.
struct DayWeaveExecutionDurableState: Codable, Equatable, Sendable {
    var deviceID: UUID?
    var bindingIdentifier: String?
    var revision: UInt64
    var activeSession: DayWeaveExecutionSession?
    var historyWindow: [DayWeaveExecutionSession]
    var historyWindowRevision: UInt64?
    var historyContinuityEstablished: Bool
    var historyVerified: Bool
    var pendingCommand: DayWeavePendingExecutionCommand?
    var terminalOutcomes: [UUID: DayWeaveTerminalExecutionOutcome]
    var leaseProjectionEligibility: [UUID: Bool]
    var presentedBlockIDs: Set<UUID>
    var acknowledgedExpiredPause: DayWeaveExecutionSessionVersion?

    static let empty = Self(
        deviceID: nil,
        bindingIdentifier: nil,
        revision: 0,
        activeSession: nil,
        historyWindow: [],
        historyWindowRevision: nil,
        historyContinuityEstablished: false,
        historyVerified: false,
        pendingCommand: nil,
        terminalOutcomes: [:],
        leaseProjectionEligibility: [:],
        presentedBlockIDs: [],
        acknowledgedExpiredPause: nil
    )

    var hasCredentialBoundState: Bool {
        bindingIdentifier != nil
            || revision != 0
            || activeSession != nil
            || !historyWindow.isEmpty
            || historyWindowRevision != nil
            || historyContinuityEstablished
            || historyVerified
            || pendingCommand != nil
            || !terminalOutcomes.isEmpty
            || !leaseProjectionEligibility.isEmpty
            || !presentedBlockIDs.isEmpty
            || acknowledgedExpiredPause != nil
    }

    var hasCredentialReplacementBlocker: Bool {
        pendingCommand != nil
            || terminalOutcomes.values.contains(where: { $0.projection.blocksCredentialReplacement })
    }
}

struct DayWeaveExecutionHistoryPage: Equatable, Sendable {
    let sessions: [DayWeaveExecutionSession]
    let nextOffset: Int?
}

enum DayWeaveExecutionWireCodec {
    static func encode(_ request: DayWeaveExecutionCommandRequest) throws -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(format(date))
        }
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(request)
    }

    static func decode(_ data: Data) throws -> DayWeaveExecutionCommandRequest {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = parse(value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Expected an RFC 3339 timestamp"
                )
            }
            return date
        }
        return try decoder.decode(DayWeaveExecutionCommandRequest.self, from: data)
    }

    private static func parse(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) { return date }
        let whole = ISO8601DateFormatter()
        whole.formatOptions = [.withInternetDateTime]
        return whole.date(from: value)
    }

    private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

struct DayWeaveExecutionSnapshotEnvelope: Decodable, Sendable {
    let execution: DayWeaveExecutionSnapshot

    private enum CodingKeys: String, CodingKey, CaseIterable { case execution }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        execution = try decoder.container(keyedBy: CodingKeys.self)
            .decode(DayWeaveExecutionSnapshot.self, forKey: .execution)
    }
}

struct DayWeaveExecutionMutationEnvelope: Decodable, Sendable {
    let mutation: DayWeaveExecutionMutation

    private enum CodingKeys: String, CodingKey, CaseIterable { case mutation }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        mutation = try decoder.container(keyedBy: CodingKeys.self)
            .decode(DayWeaveExecutionMutation.self, forKey: .mutation)
    }
}

struct DayWeaveExecutionHistoryEnvelope: Decodable, Sendable {
    let sessions: [DayWeaveExecutionSession]
    let nextOffset: Int?

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case sessions
        case nextOffset = "next_offset"
    }

    init(from decoder: any Decoder) throws {
        try requireExactExecutionKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        sessions = try container.decode([DayWeaveExecutionSession].self, forKey: .sessions)
        nextOffset = try container.decodeIfPresent(Int.self, forKey: .nextOffset)
    }
}

private struct ExecutionDynamicCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil

    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}

private func requireExactExecutionKeys<Keys: CodingKey & CaseIterable>(
    _ type: Keys.Type,
    from decoder: any Decoder
) throws where Keys.AllCases: Sequence {
    let expected = Set(type.allCases.map(\.stringValue))
    try requireExecutionKeyShape(required: expected, optional: [], from: decoder)
}

private func requireExecutionKeyShape(
    required: Set<String>,
    optional: Set<String>,
    from decoder: any Decoder
) throws {
    let dynamic = try decoder.container(keyedBy: ExecutionDynamicCodingKey.self)
    let actual = Set(dynamic.allKeys.map(\.stringValue))
    guard required.isSubset(of: actual), actual.isSubset(of: required.union(optional)) else {
        throw DecodingError.dataCorrupted(
            .init(
                codingPath: decoder.codingPath,
                debugDescription: "Execution object fields do not match the supported contract"
            )
        )
    }
}

private func encodeExecutionNullable<Value: Encodable, Key: CodingKey>(
    _ value: Value?,
    forKey key: Key,
    into container: inout KeyedEncodingContainer<Key>
) throws {
    if let value {
        try container.encode(value, forKey: key)
    } else {
        try container.encodeNil(forKey: key)
    }
}

private func validateExecutionSession(
    _ session: DayWeaveExecutionSession,
    codingPath: [any CodingKey],
    encoding: Bool = false
) throws {
    let identifiersAreValid = !session.id.isDayWeaveNil
        && !session.itemID.isDayWeaveNil
        && !session.sourceDeviceID.isDayWeaveNil
        && !(session.occurrenceID?.isDayWeaveNil ?? false)
        && !(session.plannedBlockID?.isDayWeaveNil ?? false)
    let commonStateIsValid = identifiersAreValid
        && session.itemRevision > 0
        && session.itemRevision <= UInt64(Int64.max)
        && session.revision > 0
        && session.revision <= UInt64(Int64.max)
        && session.accumulatedSeconds <= UInt64(Int64.max)
        && session.actualSeconds.map { $0 <= UInt64(Int64.max) } ?? true
        && session.createdAt == session.startedAt
        && session.updatedAt >= session.createdAt
    let initialRevisionIsValid = session.revision != 1
        || (session.status == .active
            && session.accumulatedSeconds == 0
            && session.runningSince == session.startedAt
            && session.updatedAt == session.startedAt)
    let deferredMoveIsValid = if let start = session.moveStart, let end = session.moveEnd {
        start > session.updatedAt
            && executionHasPostgresTimestampPrecision(start)
            && executionHasPostgresTimestampPrecision(end)
            && dayWeaveExactWholeSecondDelta(from: start, to: end)
                .map { $0 <= 86_400 } == true
    } else {
        false
    }
    let statusStateIsValid: Bool
    switch session.status {
    case .active:
        statusStateIsValid = session.runningSince == session.updatedAt
            && session.pausedAt == nil
            && session.pauseUntil == nil
            && session.pauseReason == nil
            && session.moveStart == nil
            && session.moveEnd == nil
            && session.actualSeconds == nil
            && session.endedAt == nil
    case .paused:
        statusStateIsValid = session.runningSince == nil
            && session.pausedAt.map { $0 >= session.startedAt && $0 <= session.updatedAt } == true
            && session.pauseUntil.map { $0 > session.updatedAt } ?? true
            && session.pauseUntil.map {
                $0 <= session.updatedAt.addingTimeInterval(86_400)
            } ?? true
            && session.moveStart == nil
            && session.moveEnd == nil
            && session.actualSeconds == nil
            && session.endedAt == nil
    case .completed, .skipped:
        statusStateIsValid = session.runningSince == nil
            && session.pauseUntil == nil
            && session.pauseReason == nil
            && session.moveStart == nil
            && session.moveEnd == nil
            && session.actualSeconds != nil
            && session.endedAt == session.updatedAt
            && (session.pausedAt.map {
                $0 >= session.startedAt && $0 <= session.updatedAt
            } ?? true)
    case .deferred:
        statusStateIsValid = session.runningSince == nil
            && session.pauseUntil == nil
            && session.pauseReason == nil
            && session.actualSeconds != nil
            && session.endedAt == session.updatedAt
            && deferredMoveIsValid
            && (session.pausedAt.map {
                $0 >= session.startedAt && $0 <= session.updatedAt
            } ?? true)
    }
    let reasonIsValid = session.pauseReason.map {
        !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && $0.unicodeScalars.count <= 500
    } ?? true
    guard commonStateIsValid, initialRevisionIsValid, statusStateIsValid, reasonIsValid else {
        let description = "Execution session violates the supported state invariants"
        if encoding {
            throw executionEncodingError(codingPath: codingPath, description)
        }
        throw executionDecodingError(codingPath: codingPath, description)
    }
}

private func executionHasPostgresTimestampPrecision(_ date: Date) -> Bool {
    dayWeavePostgresEpochMicroseconds(date) != nil
}

func isCanonicalExecutionDigest(_ value: String) -> Bool {
    let bytes = value.utf8
    return bytes.count == 71
        && bytes.starts(with: "sha256:".utf8)
        && bytes.dropFirst(7).allSatisfy { byte in
            (48...57).contains(byte) || (97...102).contains(byte)
        }
}

/// JSONDecoder necessarily rounds RFC 3339 strings into `Date`. Inspect the
/// original token as well so a persisted seven-plus-digit instant cannot look
/// microsecond-exact only because Foundation discarded its final digits.
private func executionWireTimestampHasPostgresPrecision(_ value: String) -> Bool {
    let bytes = Array(value.utf8)
    guard bytes.count >= 20 else { return false }
    var cursor = 19
    guard cursor < bytes.count else { return false }
    if bytes[cursor] == 46 {
        cursor += 1
        let fractionStart = cursor
        while cursor < bytes.count, (48...57).contains(bytes[cursor]) {
            cursor += 1
        }
        guard cursor > fractionStart else { return false }
        let fraction = bytes[fractionStart..<cursor]
        if fraction.count > 6,
           fraction.dropFirst(6).contains(where: { $0 != 48 }) {
            return false
        }
    }
    // Full RFC 3339 syntax/calendar validation remains owned by the configured
    // Date decoder; this check binds only the raw precision it cannot retain.
    return cursor < bytes.count
}

/// Normalizes one Foundation instant to PostgreSQL's stored microsecond epoch
/// representation without letting Double duration tolerances hide a real
/// microsecond remainder.
func dayWeavePostgresEpochMicroseconds(_ date: Date) -> Int64? {
    let seconds = date.timeIntervalSince1970
    guard seconds.isFinite else { return nil }
    let scaled = seconds * 1_000_000
    guard scaled.isFinite else { return nil }
    let rounded = scaled.rounded()
    let normalized = rounded / 1_000_000
    guard abs(seconds - normalized) <= max(seconds.ulp, normalized.ulp) else {
        return nil
    }
    return Int64(exactly: rounded)
}

func dayWeaveExactWholeSecondDelta(from start: Date, to end: Date) -> UInt64? {
    guard let startMicros = dayWeavePostgresEpochMicroseconds(start),
          let endMicros = dayWeavePostgresEpochMicroseconds(end) else { return nil }
    let delta = endMicros.subtractingReportingOverflow(startMicros)
    guard !delta.overflow,
          delta.partialValue > 0,
          delta.partialValue % 1_000_000 == 0 else { return nil }
    return UInt64(delta.partialValue / 1_000_000)
}

private func executionEncodingError(
    codingPath: [any CodingKey],
    _ description: String
) -> EncodingError {
    .invalidValue(
        description,
        .init(codingPath: codingPath, debugDescription: description)
    )
}

private func executionDecodingError(
    codingPath: [any CodingKey],
    _ description: String
) -> DecodingError {
    .dataCorrupted(.init(codingPath: codingPath, debugDescription: description))
}

private extension UUID {
    var isDayWeaveNil: Bool {
        self == UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
    }
}
