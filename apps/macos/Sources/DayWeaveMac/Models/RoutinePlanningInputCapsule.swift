import Foundation

enum RoutinePlanningInputCapsuleError: Error, Equatable, LocalizedError, Sendable {
    case invalidData, tooLarge, persistenceRequired, configurationChanged, scopeChanged
    case pendingRecovery, incompleteSources, sourceChanged, checkpointChanged, inputChanged, clockChanged, superseded

    var errorDescription: String? {
        switch self {
        case .invalidData: "Saved recurring planning input is invalid."
        case .tooLarge: "The complete recurring planning input does not fit encrypted storage."
        case .persistenceRequired: "Healthy encrypted storage is required to retain recurring planning input."
        case .configurationChanged, .scopeChanged: "Saved recurring planning input belongs to another authenticated connection."
        case .pendingRecovery: "Finish exact pending recovery before preparing recurring planning input."
        case .incompleteSources: "Complete canonical, Habit, and occurrence checkpoints are required."
        case .sourceChanged, .checkpointChanged, .inputChanged: "Saved recurring planning input changed; prepare a new exact input while connected."
        case .clockChanged: "The saved planning clock is no longer eligible; prepare a new exact input while connected."
        case .superseded: "Planning input capture was superseded; the previous saved input was kept."
        }
    }
}

/// Durable comparisons, not execution or review authority. This records the
/// non-canonical inputs that must still be identical before a saved, fixed
/// request can be considered for a future display-only local composition.
struct RoutinePlanningInputEnvironment: Codable, Equatable, Sendable {
    let canonicalDeltaCursor: String
    let canonicalTombstoneRevisions: [UUID: UInt64]
    let completedOccurrenceIDs: Set<UUID>
    let recurrenceSessionOutcomes: [RecurrenceSessionOutcome]
    let recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove]
    let deferredExecutionPublicationSessionIDs: Set<UUID>
    let blocks: [ScheduleBlock]
    let publishedScheduleProof: DayWeavePublishedScheduleProof?
    let publishedScheduleLatestHintRevision: UInt64
    let scheduleProfile: ScheduleProfile
    let freezeHours: Int
    private(set) var executionState: DayWeaveExecutionDurableState
    let routineOccurrenceState: RoutineOccurrenceState
    let itemCompletionState: ItemCompletionState
    let itemProgressState: ItemProgressState
    let habitCheckpointFingerprint: String?

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case canonicalDeltaCursor, canonicalTombstoneRevisions, completedOccurrenceIDs
        case recurrenceSessionOutcomes, recurrenceOccurrenceMoves, deferredExecutionPublicationSessionIDs
        case blocks, publishedScheduleProof, publishedScheduleLatestHintRevision, scheduleProfile, freezeHours
        case executionState, routineOccurrenceState, itemCompletionState, itemProgressState, habitCheckpointFingerprint
    }

    /// An offline startup can withdraw execution freshness before a failed
    /// GET. Display-only reuse tolerates that withdrawal, never a new grant or
    /// any changed execution data. This local comparison copy is not persisted
    /// and cannot restore either flag on the planner or grant execution rights.
    func matchesForDisplay(current: Self) -> Bool {
        guard !current.executionState.historyVerified || executionState.historyVerified,
              !current.executionState.historyContinuityEstablished || executionState.historyContinuityEstablished else { return false }
        var comparison = current
        comparison.executionState.historyVerified = executionState.historyVerified
        comparison.executionState.historyContinuityEstablished = executionState.historyContinuityEstablished
        return comparison == self
    }

    @MainActor
    init(planner: PlannerStore, habitCheckpoint: HabitCompositionCheckpoint?) throws {
        guard let cursor = planner.canonicalDeltaCursor, !cursor.isEmpty else {
            throw RoutinePlanningInputCapsuleError.incompleteSources
        }
        canonicalDeltaCursor = cursor
        canonicalTombstoneRevisions = planner.canonicalTombstoneRevisions
        completedOccurrenceIDs = planner.completedOccurrenceIDs
        recurrenceSessionOutcomes = planner.recurrenceSessionOutcomes
        recurrenceOccurrenceMoves = planner.recurrenceOccurrenceMoves
        deferredExecutionPublicationSessionIDs = planner.deferredExecutionPublicationSessionIDs
        blocks = planner.blocks
        publishedScheduleProof = planner.publishedScheduleProof
        publishedScheduleLatestHintRevision = planner.publishedScheduleLatestHintRevision
        scheduleProfile = planner.scheduleProfile
        freezeHours = planner.freezeHours
        executionState = planner.executionState
        routineOccurrenceState = planner.routineOccurrenceState
        itemCompletionState = planner.itemCompletionState
        itemProgressState = planner.itemProgressState
        habitCheckpointFingerprint = habitCheckpoint?.fingerprint
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: SchedulerHelperCodingKey.self)
        let names = Set(dynamic.allKeys.map(\.stringValue))
        let allowed = Set(CodingKeys.allCases.map(\.rawValue))
        guard names.isSubset(of: allowed), allowed.subtracting(["publishedScheduleProof", "habitCheckpointFingerprint"]).isSubset(of: names) else {
            throw RoutinePlanningInputCapsuleError.invalidData
        }
        let c = try decoder.container(keyedBy: CodingKeys.self)
        canonicalDeltaCursor = try c.decode(String.self, forKey: .canonicalDeltaCursor)
        canonicalTombstoneRevisions = try c.decode([UUID: UInt64].self, forKey: .canonicalTombstoneRevisions)
        completedOccurrenceIDs = try c.decode(Set<UUID>.self, forKey: .completedOccurrenceIDs)
        recurrenceSessionOutcomes = try c.decode([RecurrenceSessionOutcome].self, forKey: .recurrenceSessionOutcomes)
        recurrenceOccurrenceMoves = try c.decode([RecurrenceOccurrenceMove].self, forKey: .recurrenceOccurrenceMoves)
        deferredExecutionPublicationSessionIDs = try c.decode(Set<UUID>.self, forKey: .deferredExecutionPublicationSessionIDs)
        blocks = try c.decode([ScheduleBlock].self, forKey: .blocks)
        publishedScheduleProof = try c.decodeIfPresent(DayWeavePublishedScheduleProof.self, forKey: .publishedScheduleProof)
        publishedScheduleLatestHintRevision = try c.decode(UInt64.self, forKey: .publishedScheduleLatestHintRevision)
        scheduleProfile = try c.decode(ScheduleProfile.self, forKey: .scheduleProfile)
        freezeHours = try c.decode(Int.self, forKey: .freezeHours)
        executionState = try c.decode(DayWeaveExecutionDurableState.self, forKey: .executionState)
        routineOccurrenceState = try c.decode(RoutineOccurrenceState.self, forKey: .routineOccurrenceState)
        itemCompletionState = try c.decode(ItemCompletionState.self, forKey: .itemCompletionState)
        itemProgressState = try c.decode(ItemProgressState.self, forKey: .itemProgressState)
        habitCheckpointFingerprint = try c.decodeIfPresent(String.self, forKey: .habitCheckpointFingerprint)
    }
}

/// One encrypted fixed-input artifact, never a reusable offline capability.
/// The caller may construct this only from an owned authenticated response
/// after its live configuration/source/cursor fences pass. The first response
/// attests workspace/user scope; later captures must retain that same-binding
/// pin. Hashes and a decoded object alone do not establish authenticity.
struct RoutinePlanningInputCapsule: Codable, Equatable, Sendable {
    static let maximumBytes = 16 * 1_024 * 1_024
    let schemaVersion: UInt16
    let origin: String
    /// Existing normalized origin plus stable opaque credential binding, not a bearer token.
    let configurationIdentifier: String
    let originalRequestBody: Data
    let request: RoutinePlanningWitnessRequest
    let witness: RoutinePlanningWitness
    let canonicalItems: [DayWeaveCanonicalItem]
    let environment: RoutinePlanningInputEnvironment
    let capturedAt: Date

    var workspaceID: UUID { witness.workspaceID }
    var userID: UUID { witness.userID }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion, origin, configurationIdentifier, originalRequestBody, request, witness, canonicalItems, environment, capturedAt
    }

    init(origin: String, configurationIdentifier: String, originalRequestBody: Data,
         request: RoutinePlanningWitnessRequest, witness: RoutinePlanningWitness,
         canonicalItems: [DayWeaveCanonicalItem], environment: RoutinePlanningInputEnvironment,
         capturedAt: Date) throws {
        schemaVersion = 1; self.origin = origin; self.configurationIdentifier = configurationIdentifier
        self.originalRequestBody = originalRequestBody; self.request = request; self.witness = witness
        self.canonicalItems = canonicalItems; self.environment = environment; self.capturedAt = capturedAt
        try validate()
    }

    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, Set(CodingKeys.allCases.map(\.rawValue)))
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(UInt16.self, forKey: .schemaVersion)
        origin = try c.decode(String.self, forKey: .origin)
        configurationIdentifier = try c.decode(String.self, forKey: .configurationIdentifier)
        originalRequestBody = try c.decode(Data.self, forKey: .originalRequestBody)
        request = try c.decode(RoutinePlanningWitnessRequest.self, forKey: .request)
        witness = try c.decode(RoutinePlanningWitness.self, forKey: .witness)
        canonicalItems = try c.decode([DayWeaveCanonicalItem].self, forKey: .canonicalItems)
        environment = try c.decode(RoutinePlanningInputEnvironment.self, forKey: .environment)
        capturedAt = try c.decode(Date.self, forKey: .capturedAt)
        try validateShape()
    }

    func validate() throws {
        try validateShape()
        let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
        guard try encoder.encode(self).count <= Self.maximumBytes else { throw RoutinePlanningInputCapsuleError.tooLarge }
    }

    private func validateShape() throws {
        guard schemaVersion == 1,
              let base = try? DayWeaveAPIBaseURL(origin), base.canonicalConfigurationIdentifier == origin,
              configurationIdentifier.hasPrefix(origin + "|auth="),
              configurationIdentifier.utf8.count > (origin + "|auth=").utf8.count,
              configurationIdentifier.utf8.count <= 4_096,
              !configurationIdentifier.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
              !originalRequestBody.isEmpty, originalRequestBody.count <= RoutinePlanningWitnessValidation.maximumBytes,
              canonicalItems.count <= 10_000, capturedAt.timeIntervalSinceReferenceDate.isFinite,
              environment.scheduleProfile.hasValidShape,
              environment.routineOccurrenceState.isValid,
              environment.itemCompletionState.isValid, environment.itemProgressState.isValid,
              environment.routineOccurrenceState.configurationIdentifier == configurationIdentifier,
              environment.routineOccurrenceState.terminalDeltaCursor == request.terminalCursor,
              !environment.routineOccurrenceState.hasUnresolvedCustody,
              environment.itemCompletionState.journals.isEmpty, !environment.itemCompletionState.needsCanonicalCatchUp,
              environment.itemProgressState.journals.isEmpty,
              environment.executionState.activeSession == nil, environment.executionState.pendingCommand == nil,
              !environment.executionState.hasCredentialReplacementBlocker,
              witness.executionSnapshotRevision == environment.executionState.revision else {
            throw RoutinePlanningInputCapsuleError.invalidData
        }
        do {
            guard try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessRequest.self, from: originalRequestBody) == request else {
                throw RoutinePlanningInputCapsuleError.invalidData
            }
            try witness.requireMatches(request)
            try witness.validate(canonicalItems: canonicalItems)
            for item in canonicalItems { _ = try SchedulerHelperCanonicalItemWire(validating: item) }
        } catch {
            throw RoutinePlanningInputCapsuleError.invalidData
        }
    }
}

/// In-process capture CAS only. Never Codable and never reconstructed on load.
struct RoutinePlanningInputCaptureFence: Equatable, Sendable {
    let environment: RoutinePlanningInputEnvironment
    let canonicalItems: [DayWeaveCanonicalItem]
    let priorCapsule: RoutinePlanningInputCapsule?
    let snapshot: PlannerSnapshot
    let ownerID: UUID
    let occurrenceGeneration: UInt64
    let canonicalGeneration: UInt64
    let habitCheckpoint: HabitCompositionCheckpoint?
}
