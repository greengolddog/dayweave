import Foundation

/// A private, inert view value. Deliberately not ScheduleBlock: it carries no
/// mutable origin, execution session, publication proof or move authorization.
struct RoutinePlanningDisplayPresentation: Equatable, Sendable {
    struct Block: Identifiable, Equatable, Sendable {
        let id: UUID
        let title: String
        let start: Date
        let end: Date
        let originalStart: String
        let originalEnd: String
        let kind: String
        let explanation: String
    }
    struct Unscheduled: Identifiable, Equatable, Sendable {
        let id: Int
        let title: String
        let remainingMinutes: UInt32
        let message: String
    }
    struct Member: Identifiable, Equatable, Sendable {
        struct ID: Hashable, Sendable { let occurrenceID: UUID; let itemID: UUID }
        let id: ID
        let title: String
        let status: RoutinePlanningMemberStatus
    }
    let asOf: String
    let horizonStart: String
    let horizonEnd: String
    let timezoneName: String
    let generatedAt: Date
    let occurrenceSnapshotRevision: UInt64
    let instanceCount: Int
    let scheduledMinutes: UInt32
    let unscheduledMinutes: UInt32
    let blocks: [Block]
    let unscheduled: [Unscheduled]
    let notices: [String]
    let members: [Member]

    init(artifact: RoutinePlanningDisplayPlan, capsule: RoutinePlanningInputCapsule) throws {
        let composition = try artifact.validatedComposition(capsule: capsule).composition
        let plan = composition.plan
        let source = Dictionary(uniqueKeysWithValues: capsule.canonicalItems.map { ($0.id, $0) })
        let lifecycle = Dictionary(uniqueKeysWithValues: capsule.witness.occurrenceLifecycle.instances.map { ($0.occurrenceID, $0) })
        var occurrenceRoots: [UUID: UUID] = [:]
        for occurrence in plan.occurrences {
            try RoutinePlanningWitnessValidation.require(source[occurrence.seriesItemID] != nil
                && occurrenceRoots.updateValue(occurrence.seriesItemID, forKey: occurrence.id) == nil)
            if let instance = lifecycle[occurrence.id] {
                try RoutinePlanningWitnessValidation.require(instance.rootItemID == occurrence.seriesItemID
                    && instance.identity == occurrence.identity)
            }
        }
        var memberIDs: [UUID: Set<UUID>] = [:]
        for instance in lifecycle.values { memberIDs[instance.occurrenceID] = Set(instance.members.map(\.itemID)) }
        func requireReference(itemID: UUID, occurrenceID: UUID?) throws {
            guard source[itemID] != nil else { throw RoutinePlanningWitnessError.invalidData }
            if let occurrenceID {
                guard let rootID = occurrenceRoots[occurrenceID], let root = source[rootID] else {
                    throw RoutinePlanningWitnessError.invalidData
                }
                if root.kind == .task || root.kind == .routine {
                    try RoutinePlanningWitnessValidation.require(memberIDs[occurrenceID]?.contains(itemID) == true)
                } else {
                    try RoutinePlanningWitnessValidation.require(root.kind == .habit && itemID == rootID)
                }
            }
        }
        // Retain original helper timestamp strings; Date is only a rendering aid.
        let raw = try RoutinePlanningWitnessValidation.decode(JSONValue.self, from: artifact.rawOutput)
        let envelope = try RoutinePlanningShape.map(raw)
        let result = try RoutinePlanningShape.map(envelope["result"])
        let composed = try RoutinePlanningShape.map(result["composition"])
        let rawPlan = try RoutinePlanningShape.map(composed["plan"])
        let rawBlocks = try RoutinePlanningShape.array(rawPlan["blocks"], maximum: 50_000)
        try RoutinePlanningWitnessValidation.require(rawBlocks.count == plan.blocks.count)
        var seen = Set<UUID>(), projected: [Block] = []
        for (block, rawBlock) in zip(plan.blocks, rawBlocks) {
            try RoutinePlanningWitnessValidation.require(seen.insert(block.id).inserted)
            if let itemID = block.itemID { try requireReference(itemID: itemID, occurrenceID: block.occurrenceID) }
            else { try RoutinePlanningWitnessValidation.require(block.occurrenceID == nil) }
            let fields = try RoutinePlanningShape.map(rawBlock)
            projected.append(.init(id: block.id, title: block.title, start: block.start, end: block.end,
                originalStart: try RoutinePlanningShape.string(fields["start"]),
                originalEnd: try RoutinePlanningShape.string(fields["end"]), kind: block.kind,
                explanation: block.explanations.map(\.message).joined(separator: " ")))
        }
        blocks = projected.sorted { $0.start == $1.start ? $0.id.uuidString < $1.id.uuidString : $0.start < $1.start }
        unscheduled = try plan.unscheduled.enumerated().map { index, row in
            try requireReference(itemID: row.itemID, occurrenceID: row.occurrenceID)
            return .init(id: index, title: source[row.itemID]!.title, remainingMinutes: row.remaining, message: row.message)
        }
        // Helper validation has already closed these objects. Display only the
        // plain diagnostic, never manufacture interactive actions from JSON.
        notices = (plan.decisions + plan.violations).compactMap { value in
            guard case let .object(row) = value, case let .string(message)? = row["message"] else { return nil }
            return message
        }
        members = capsule.witness.occurrenceLifecycle.instances.flatMap { instance in
            instance.members.map { member in
                Member(id: .init(occurrenceID: instance.occurrenceID, itemID: member.itemID),
                    title: source[member.itemID]!.title, status: member.status)
            }
        }
        asOf = try RoutinePlanningShape.string(capsule.witness.schedule.fields["as_of"])
        horizonStart = try RoutinePlanningShape.string(capsule.witness.schedule.fields["horizon_start"])
        horizonEnd = try RoutinePlanningShape.string(capsule.witness.schedule.fields["horizon_end"])
        timezoneName = try RoutinePlanningShape.string(capsule.witness.schedule.fields["timezone_name"])
        generatedAt = artifact.generatedAt
        occurrenceSnapshotRevision = capsule.witness.occurrenceLifecycle.snapshotRevision
        instanceCount = capsule.witness.occurrenceLifecycle.instances.count
        scheduledMinutes = plan.score.scheduledMinutes
        unscheduledMinutes = plan.score.unscheduledMinutes
    }
}
