import Foundation

enum RoutinePlanningDisplayPlanError: Error, Equatable, LocalizedError, Sendable {
    case invalidData, tooLarge, capsuleChanged

    var errorDescription: String? {
        switch self {
        case .invalidData: "The saved routine display plan could not be verified."
        case .tooLarge: "The complete routine display plan does not fit encrypted storage."
        case .capsuleChanged: "The saved routine display plan belongs to another fixed input. Recompute it explicitly."
        }
    }
}

/// Separate encrypted display-only custody. No field grants execution,
/// publication, occurrence editing or a restored private presentation lease.
/// The exact capsule value deliberately costs additional bounded space: no
/// native reserialization hash substitutes for its full input equality.
struct RoutinePlanningDisplayPlan: Codable, Equatable, Sendable {
    static let maximumBytes = 16 * 1_024 * 1_024
    let schemaVersion: UInt16
    let capsuleBinding: RoutinePlanningInputCapsule
    let rawOutput: Data
    let generatedAt: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case schemaVersion, capsuleBinding, rawOutput, generatedAt
    }

    init(capsule: RoutinePlanningInputCapsule,
         composition: RoutineOccurrenceLocalComposition, generatedAt: Date) throws {
        schemaVersion = 1
        capsuleBinding = capsule
        rawOutput = composition.rawOutput
        self.generatedAt = generatedAt
        guard try validatedRetainedComposition() == composition else { throw RoutinePlanningDisplayPlanError.invalidData }
    }

    init(from decoder: any Decoder) throws {
        try RoutinePlanningWitnessValidation.keys(decoder, Set(CodingKeys.allCases.map(\.rawValue)))
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try container.decode(UInt16.self, forKey: .schemaVersion)
        capsuleBinding = try container.decode(RoutinePlanningInputCapsule.self, forKey: .capsuleBinding)
        rawOutput = try container.decode(Data.self, forKey: .rawOutput)
        generatedAt = try container.decode(Date.self, forKey: .generatedAt)
        _ = try validatedRetainedComposition()
    }

    func validate() throws {
        _ = try validatedRetainedComposition()
    }

    private func validatedRetainedComposition() throws -> RoutineOccurrenceLocalComposition {
        let decoded = try retainedComposition()
        let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
        guard try encoder.encode(self).count <= Self.maximumBytes else { throw RoutinePlanningDisplayPlanError.tooLarge }
        return decoded
    }

    /// Only the returned typed plan is a lossy display projection. Persist and
    /// compare rawOutput, never an encoding of its Foundation Date values.
    func validatedComposition(capsule: RoutinePlanningInputCapsule) throws -> RoutineOccurrenceLocalComposition {
        guard capsule == capsuleBinding else { throw RoutinePlanningDisplayPlanError.capsuleChanged }
        return try validatedRetainedComposition()
    }

    private func retainedComposition() throws -> RoutineOccurrenceLocalComposition {
        guard schemaVersion == 1, !rawOutput.isEmpty,
              rawOutput.count <= SchedulerHelperClient.maximumStandardOutputBytes,
              generatedAt.timeIntervalSinceReferenceDate.isFinite,
              generatedAt >= capsuleBinding.capturedAt else { throw RoutinePlanningDisplayPlanError.invalidData }
        do {
            try capsuleBinding.validate()
            return try SchedulerHelperClient.decodeOccurrenceOutput(
                .init(standardOutput: rawOutput, standardError: Data(), termination: .exited(0)),
                witness: capsuleBinding.witness)
        } catch {
            // Retained private stdout and decoder cause chains never escape.
            throw RoutinePlanningDisplayPlanError.invalidData
        }
    }
}
