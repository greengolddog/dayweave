import Foundation

/// A schedule produced entirely on this device. It deliberately has no
/// `inputDigest`, so it cannot be passed to the server publication contract as
/// though the server had previewed it.
struct LocalScheduleComposition: Equatable, Sendable {
    typealias RejectedItem = DayWeaveSchedulePreview.RejectedItem
    typealias IgnoredAssignment = DayWeaveSchedulePreview.IgnoredAssignment

    let localInputFingerprint: String
    let sourceItemCount: Int
    let sourceItemRevisions: [UUID: UInt64]
    let acceptedItemCount: Int
    let rejectedItems: [RejectedItem]
    let ignoredPreviousAssignments: [IgnoredAssignment]
    let plan: DayWeaveSchedulePreview.Plan

    init(
        localInputFingerprint: String,
        sourceItemCount: Int,
        sourceItemRevisions: [UUID: UInt64],
        acceptedItemCount: Int,
        rejectedItems: [RejectedItem],
        ignoredPreviousAssignments: [IgnoredAssignment],
        plan: DayWeaveSchedulePreview.Plan
    ) {
        self.localInputFingerprint = localInputFingerprint
        self.sourceItemCount = sourceItemCount
        self.sourceItemRevisions = sourceItemRevisions
        self.acceptedItemCount = acceptedItemCount
        self.rejectedItems = rejectedItems
        self.ignoredPreviousAssignments = ignoredPreviousAssignments
        self.plan = plan
    }
}

extension LocalScheduleComposition: Decodable {
    private enum CodingKeys: String, CodingKey, CaseIterable {
        case plan
        case localInputFingerprint = "local_input_fingerprint"
        case sourceItemCount = "source_item_count"
        case sourceItemRevisions = "source_item_revisions"
        case acceptedItemCount = "accepted_item_count"
        case rejectedItems = "rejected_items"
        case ignoredPreviousAssignments = "ignored_previous_assignments"
    }

    init(from decoder: any Decoder) throws {
        let dynamic = try decoder.container(keyedBy: SchedulerHelperCodingKey.self)
        let expected = Set(CodingKeys.allCases.map(\.rawValue))
        guard Set(dynamic.allKeys.map(\.stringValue)) == expected else {
            throw DecodingError.dataCorrupted(
                .init(
                    codingPath: decoder.codingPath,
                    debugDescription: "Unexpected local composition shape"
                )
            )
        }

        let container = try decoder.container(keyedBy: CodingKeys.self)
        let fingerprint = try container.decode(String.self, forKey: .localInputFingerprint)
        let sourceCount = try container.decode(Int.self, forKey: .sourceItemCount)
        let acceptedCount = try container.decode(Int.self, forKey: .acceptedItemCount)
        let rejected = try container.decode([RejectedItem].self, forKey: .rejectedItems)
        let ignored = try container.decode(
            [IgnoredAssignment].self,
            forKey: .ignoredPreviousAssignments
        )
        let decodedPlan = try container.decode(
            DayWeaveSchedulePreview.Plan.self,
            forKey: .plan
        )
        let rawRevisions = try container.decode(
            [String: UInt64].self,
            forKey: .sourceItemRevisions
        )

        guard Self.isValidFingerprint(fingerprint),
              (0...10_000).contains(sourceCount),
              acceptedCount >= 0,
              acceptedCount <= sourceCount,
              rawRevisions.count == sourceCount,
              rejected.count == sourceCount - acceptedCount,
              ignored.count <= 10_000,
              rawRevisions.values.allSatisfy({ $0 > 0 }) else {
            throw DecodingError.dataCorruptedError(
                forKey: .sourceItemRevisions,
                in: container,
                debugDescription: "Invalid local composition evidence"
            )
        }

        var revisions: [UUID: UInt64] = [:]
        revisions.reserveCapacity(rawRevisions.count)
        for (rawID, revision) in rawRevisions {
            guard let id = UUID(uuidString: rawID), revisions[id] == nil else {
                throw DecodingError.dataCorruptedError(
                    forKey: .sourceItemRevisions,
                    in: container,
                    debugDescription: "Invalid local composition source revision"
                )
            }
            revisions[id] = revision
        }

        self.init(
            localInputFingerprint: fingerprint,
            sourceItemCount: sourceCount,
            sourceItemRevisions: revisions,
            acceptedItemCount: acceptedCount,
            rejectedItems: rejected,
            ignoredPreviousAssignments: ignored,
            plan: decodedPlan
        )
    }

    private static func isValidFingerprint(_ value: String) -> Bool {
        let prefix = "local-sha256:"
        guard value.hasPrefix(prefix) else { return false }
        let digest = value.dropFirst(prefix.count)
        return digest.count == 64 && digest.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (97...102).contains(byte)
        }
    }
}

/// The only canonical-item representation allowed to cross the local helper
/// boundary. Unlike the API replacement model, this projection always removes
/// notes before encoding.
struct SchedulerHelperCanonicalItemWire: Encodable, Equatable, Sendable {
    let id: UUID
    let isSensitive: Bool
    let kind: DayWeaveCanonicalItemKind
    let status: DayWeaveCanonicalItemStatus
    let title: String
    let timezoneName: String
    let durationSeconds: UInt32?
    let deadlineAt: Date?
    let earliestStartAt: Date?
    let recurrence: JSONValue?
    let flexibleConstraints: JSONValue
    let splitPolicy: DayWeaveSplitPolicy
    let importance: UInt8
    let urgency: UInt8
    let parentID: UUID?
    let siblingOrder: UInt32
    let isExecutable: Bool
    let revision: UInt64
    let createdAt: Date
    let updatedAt: Date
    let completedAt: Date?
    let deletedAt: Date?

    private enum CodingKeys: String, CodingKey {
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
    }

    init(validating item: DayWeaveCanonicalItem) throws {
        // Helper composition is read-only and fingerprints its normalized
        // typed input. Unlike a server replacement, it does not claim to
        // reproduce arbitrary JSON tokens byte-for-byte, so decoded numeric
        // values in known recurrence/constraint fields remain usable.
        guard item.unsupportedFields.isEmpty,
              !item.hasExplicitStructuralMetadata,
              item.revision > 0 else {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }
        if case .unknown = item.kind {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }
        if case .unknown = item.status {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }
        if case .unknown = item.splitPolicy {
            throw SchedulerHelperClientError.unsupportedCanonicalItem
        }

        id = item.id
        isSensitive = item.isSensitive
        kind = item.kind
        status = item.status
        title = item.title
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
        isExecutable = item.isExecutable
        revision = item.revision
        createdAt = item.createdAt
        updatedAt = item.updatedAt
        completedAt = item.completedAt
        deletedAt = item.deletedAt
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(isSensitive, forKey: .isSensitive)
        try container.encode(kind, forKey: .kind)
        try container.encode(status, forKey: .status)
        try container.encode(title, forKey: .title)
        try container.encodeNil(forKey: .notes)
        try container.encode(timezoneName, forKey: .timezoneName)
        try container.encodeIfPresent(durationSeconds, forKey: .durationSeconds)
        if durationSeconds == nil { try container.encodeNil(forKey: .durationSeconds) }
        try container.encodeIfPresent(deadlineAt, forKey: .deadlineAt)
        if deadlineAt == nil { try container.encodeNil(forKey: .deadlineAt) }
        try container.encodeIfPresent(earliestStartAt, forKey: .earliestStartAt)
        if earliestStartAt == nil { try container.encodeNil(forKey: .earliestStartAt) }
        try container.encodeIfPresent(recurrence, forKey: .recurrence)
        if recurrence == nil { try container.encodeNil(forKey: .recurrence) }
        try container.encode(flexibleConstraints, forKey: .flexibleConstraints)
        try container.encode(splitPolicy, forKey: .splitPolicy)
        try container.encode(importance, forKey: .importance)
        try container.encode(urgency, forKey: .urgency)
        try container.encodeIfPresent(parentID, forKey: .parentID)
        if parentID == nil { try container.encodeNil(forKey: .parentID) }
        try container.encode(siblingOrder, forKey: .siblingOrder)
        try container.encode(isExecutable, forKey: .isExecutable)
        try container.encode(revision, forKey: .revision)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
        try container.encodeIfPresent(completedAt, forKey: .completedAt)
        if completedAt == nil { try container.encodeNil(forKey: .completedAt) }
        try container.encodeIfPresent(deletedAt, forKey: .deletedAt)
        if deletedAt == nil { try container.encodeNil(forKey: .deletedAt) }
    }
}

struct SchedulerHelperCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil

    init(_ value: String) { stringValue = value }
    init?(stringValue: String) { self.init(stringValue) }
    init?(intValue: Int) { return nil }
}
