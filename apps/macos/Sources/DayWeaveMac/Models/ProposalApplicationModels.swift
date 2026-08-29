import Foundation

let dayWeaveProposalChangeSetSchemaV1 = "dayweave.proposal-change-set/1"

struct DayWeaveProposalPreviewMember: Codable, Equatable, Sendable {
    let proposalID: UUID
    let expectedRevision: UInt64

    private enum CodingKeys: String, CodingKey {
        case proposalID = "proposal_id"
        case expectedRevision = "expected_revision"
    }
}

struct DayWeaveProposalPreviewRequest: Encodable, Equatable, Sendable {
    let proposals: [DayWeaveProposalPreviewMember]
}

struct DayWeaveProposalItemDiff: Codable, Equatable, Identifiable, Sendable {
    let commandID: UUID
    let operation: String
    let itemID: UUID
    let changedFields: [String]
    let before: DayWeaveCanonicalItem?
    let after: DayWeaveCanonicalItem?

    var id: UUID { commandID }

    private enum CodingKeys: String, CodingKey {
        case commandID = "command_id"
        case operation
        case itemID = "item_id"
        case changedFields = "changed_fields"
        case before
        case after
    }
}

struct DayWeaveProposalImplicitItemDiff: Codable, Equatable, Identifiable, Sendable {
    let itemID: UUID
    let reason: String
    let changedFields: [String]
    let before: DayWeaveCanonicalItem
    let after: DayWeaveCanonicalItem

    var id: UUID { itemID }

    private enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case reason
        case changedFields = "changed_fields"
        case before
        case after
    }
}

struct DayWeaveProposalRisk: Codable, Equatable, Identifiable, Sendable {
    let code: String
    let level: String
    let commandID: UUID?
    let itemID: UUID?
    let requiresExplicitApproval: Bool
    let summary: String

    var id: String {
        [code, commandID?.uuidString ?? "none", itemID?.uuidString ?? "none", summary]
            .joined(separator: ":")
    }

    private enum CodingKeys: String, CodingKey {
        case code
        case level
        case commandID = "command_id"
        case itemID = "item_id"
        case requiresExplicitApproval = "requires_explicit_approval"
        case summary
    }
}

struct DayWeaveProposalConflict: Codable, Equatable, Identifiable, Sendable {
    let code: String
    let commandID: UUID?
    let itemID: UUID?
    let expectedRevision: UInt64?
    let actualRevision: UInt64?
    let summary: String

    var id: String {
        [code, commandID?.uuidString ?? "none", itemID?.uuidString ?? "none", summary]
            .joined(separator: ":")
    }

    private enum CodingKeys: String, CodingKey {
        case code
        case commandID = "command_id"
        case itemID = "item_id"
        case expectedRevision = "expected_revision"
        case actualRevision = "actual_revision"
        case summary
    }
}

struct DayWeaveProposalApplicationPreview: Codable, Equatable, Sendable {
    let previewID: UUID
    let proposals: [DayWeaveProposalPreviewMember]
    let changeSetSchema: String
    let commandIDs: [UUID]
    let reviewHash: String
    let expiresAt: Date
    let canApply: Bool
    let maximumRisk: String
    let requiresExplicitApproval: Bool
    let diffs: [DayWeaveProposalItemDiff]
    let implicitDiffs: [DayWeaveProposalImplicitItemDiff]
    let risks: [DayWeaveProposalRisk]
    let conflicts: [DayWeaveProposalConflict]

    private enum CodingKeys: String, CodingKey {
        case previewID = "preview_id"
        case proposals
        case changeSetSchema = "change_set_schema"
        case commandIDs = "command_ids"
        case reviewHash = "review_hash"
        case expiresAt = "expires_at"
        case canApply = "can_apply"
        case maximumRisk = "maximum_risk"
        case requiresExplicitApproval = "requires_explicit_approval"
        case diffs
        case implicitDiffs = "implicit_diffs"
        case risks
        case conflicts
    }

    var hasSupportedContract: Bool {
        let commandIDSet = Set(commandIDs)
        let diffCommandIDs = diffs.map(\.commandID)
        let diffCommandIDSet = Set(diffCommandIDs)
        let proposalIDs = proposals.map(\.proposalID)
        let implicitItemIDs = implicitDiffs.map(\.itemID)
        return changeSetSchema == dayWeaveProposalChangeSetSchemaV1
            && (1...20).contains(proposals.count)
            && Set(proposalIDs).count == proposalIDs.count
            && proposals.allSatisfy { $0.expectedRevision > 0 }
            && (1...100).contains(commandIDs.count)
            && commandIDSet.count == commandIDs.count
            && isDayWeaveSHA256ReviewHash(reviewHash)
            && expiresAt.timeIntervalSinceReferenceDate.isFinite
            && ["low", "medium", "high", "critical"].contains(maximumRisk)
            && Set(implicitItemIDs).count == implicitItemIDs.count
            && canApply == conflicts.isEmpty
            && (!canApply || (diffs.count == commandIDs.count
                && diffCommandIDSet.count == diffCommandIDs.count
                && diffCommandIDSet == commandIDSet))
    }

    func matches(_ proposal: DayWeaveProposal) -> Bool {
        proposals == [DayWeaveProposalPreviewMember(
            proposalID: proposal.id,
            expectedRevision: proposal.revision
        )]
    }
}

/// A local confirmation is valid only for the exact immutable simulation the
/// user reviewed. Keeping both identifiers prevents a second window or a fresh
/// preview from reusing an older checkbox state.
struct DayWeaveProposalReviewApproval: Equatable, Sendable {
    let proposalID: UUID
    let proposalRevision: UInt64
    let previewID: UUID
    let reviewHash: String
}

struct DayWeaveProposalApplyRequest: Codable, Equatable, Sendable {
    let expectedReviewHash: String

    private enum CodingKeys: String, CodingKey {
        case expectedReviewHash = "expected_review_hash"
    }
}

enum DayWeaveProposalApplicationStatus: String, Codable, Equatable, Sendable {
    case applied
    case undone
}

struct DayWeaveProposalAppliedMember: Codable, Equatable, Sendable {
    let proposalID: UUID
    let appliedRevision: UInt64

    private enum CodingKeys: String, CodingKey {
        case proposalID = "proposal_id"
        case appliedRevision = "applied_revision"
    }
}

struct DayWeaveProposalApplicationReceipt: Codable, Equatable, Identifiable, Sendable {
    let applicationID: UUID
    let proposals: [DayWeaveProposalAppliedMember]
    let applicationRevision: UInt64
    let status: DayWeaveProposalApplicationStatus
    let commandIDs: [UUID]
    let affectedItemIDs: [UUID]
    let appliedAt: Date
    let undoExpiresAt: Date
    let undoneAt: Date?

    var id: UUID { applicationID }

    private enum CodingKeys: String, CodingKey {
        case applicationID = "application_id"
        case proposals
        case applicationRevision = "application_revision"
        case status
        case commandIDs = "command_ids"
        case affectedItemIDs = "affected_item_ids"
        case appliedAt = "applied_at"
        case undoExpiresAt = "undo_expires_at"
        case undoneAt = "undone_at"
    }

    var hasValidShape: Bool {
        !proposals.isEmpty
            && Set(proposals.map(\.proposalID)).count == proposals.count
            && proposals.allSatisfy { $0.appliedRevision > 0 }
            && applicationRevision > 0
            && !commandIDs.isEmpty
            && Set(commandIDs).count == commandIDs.count
            && !affectedItemIDs.isEmpty
            && Set(affectedItemIDs).count == affectedItemIDs.count
            && appliedAt.timeIntervalSinceReferenceDate.isFinite
            && undoExpiresAt.timeIntervalSinceReferenceDate.isFinite
            && undoExpiresAt > appliedAt
            && ((status == .applied && undoneAt == nil && applicationRevision == 1)
                || (status == .undone
                    && applicationRevision == 2
                    && undoneAt.map {
                        $0.timeIntervalSinceReferenceDate.isFinite
                            && $0 >= appliedAt
                            && $0 <= undoExpiresAt
                    } == true))
    }

    func contains(proposalID: UUID) -> Bool {
        proposals.contains { $0.proposalID == proposalID }
    }
}

struct DayWeaveProposalApplyResponse: Codable, Equatable, Sendable {
    let application: DayWeaveProposalApplicationReceipt
    let replayed: Bool
}

struct DayWeaveProposalUndoRequest: Codable, Equatable, Sendable {
    let expectedApplicationRevision: UInt64

    private enum CodingKeys: String, CodingKey {
        case expectedApplicationRevision = "expected_application_revision"
    }
}

struct DayWeaveProposalUndoResponse: Codable, Equatable, Sendable {
    let application: DayWeaveProposalApplicationReceipt
    let replayed: Bool
}

enum DayWeavePendingProposalApplicationOperation: String, Codable, Equatable, Sendable {
    case apply
    case undo
}

struct DayWeavePendingProposalApplicationMutation: Codable, Equatable, Identifiable, Sendable {
    let mutationID: UUID
    let operation: DayWeavePendingProposalApplicationOperation
    let configurationIdentifier: String
    let proposalIDs: [UUID]
    let proposalRevisions: [UInt64]
    let expectedCommandIDs: [UUID]
    let previewID: UUID?
    let applicationID: UUID?
    let expectedReviewHash: String?
    let expectedApplicationRevision: UInt64?
    let requestBody: Data
    let idempotencyKey: String
    let createdAt: Date

    var id: UUID { mutationID }

    static func apply(
        configurationIdentifier: String,
        proposalIDs: [UUID],
        proposalRevisions: [UInt64],
        expectedCommandIDs: [UUID],
        previewID: UUID,
        expectedReviewHash: String,
        requestBody: Data,
        idempotencyKey: String,
        createdAt: Date
    ) -> Self {
        Self(
            mutationID: UUID(),
            operation: .apply,
            configurationIdentifier: configurationIdentifier,
            proposalIDs: proposalIDs,
            proposalRevisions: proposalRevisions,
            expectedCommandIDs: expectedCommandIDs,
            previewID: previewID,
            applicationID: nil,
            expectedReviewHash: expectedReviewHash,
            expectedApplicationRevision: nil,
            requestBody: requestBody,
            idempotencyKey: idempotencyKey,
            createdAt: createdAt
        )
    }

    static func undo(
        configurationIdentifier: String,
        proposalIDs: [UUID],
        proposalRevisions: [UInt64],
        expectedCommandIDs: [UUID],
        applicationID: UUID,
        expectedApplicationRevision: UInt64,
        requestBody: Data,
        idempotencyKey: String,
        createdAt: Date
    ) -> Self {
        Self(
            mutationID: UUID(),
            operation: .undo,
            configurationIdentifier: configurationIdentifier,
            proposalIDs: proposalIDs,
            proposalRevisions: proposalRevisions,
            expectedCommandIDs: expectedCommandIDs,
            previewID: nil,
            applicationID: applicationID,
            expectedReviewHash: nil,
            expectedApplicationRevision: expectedApplicationRevision,
            requestBody: requestBody,
            idempotencyKey: idempotencyKey,
            createdAt: createdAt
        )
    }

    var hasValidShape: Bool {
        guard !configurationIdentifier.isEmpty,
              configurationIdentifier.utf8.count <= 2_048,
              (1...20).contains(proposalIDs.count),
              Set(proposalIDs).count == proposalIDs.count,
              proposalRevisions.count == proposalIDs.count,
              proposalRevisions.allSatisfy({ $0 > 0 }),
              !expectedCommandIDs.isEmpty,
              Set(expectedCommandIDs).count == expectedCommandIDs.count,
              (1...4_096).contains(requestBody.count),
              (8...128).contains(idempotencyKey.utf8.count),
              idempotencyKey.utf8.allSatisfy({ byte in
                  (byte >= 65 && byte <= 90)
                      || (byte >= 97 && byte <= 122)
                      || (byte >= 48 && byte <= 57)
                      || [45, 46, 95, 126].contains(byte)
              }),
              createdAt.timeIntervalSinceReferenceDate.isFinite else {
            return false
        }
        switch operation {
        case .apply:
            return previewID != nil
                && applicationID == nil
                && expectedApplicationRevision == nil
                && expectedReviewHash.map(isDayWeaveSHA256ReviewHash) == true
        case .undo:
            return previewID == nil
                && applicationID != nil
                && expectedReviewHash == nil
                && expectedApplicationRevision.map { $0 > 0 } == true
        }
    }
}

struct DayWeaveStoredProposalApplicationReceipt: Codable, Equatable, Identifiable, Sendable {
    let configurationIdentifier: String
    let application: DayWeaveProposalApplicationReceipt

    var id: UUID { application.applicationID }

    var hasValidShape: Bool {
        !configurationIdentifier.isEmpty
            && configurationIdentifier.utf8.count <= 2_048
            && application.hasValidShape
    }
}

extension DayWeaveProposal {
    var advertisedChangeSetSchema: String? {
        guard case let .string(schema) = payload["schema"] else { return nil }
        return schema
    }

    var advertisesApplicationReadyChangeSet: Bool {
        advertisedChangeSetSchema == dayWeaveProposalChangeSetSchemaV1
    }

    var advertisesReservedChangeSetSchema: Bool {
        advertisedChangeSetSchema?.hasPrefix("dayweave.proposal-change-set/") == true
    }
}

private func isDayWeaveSHA256ReviewHash(_ value: String) -> Bool {
    value.hasPrefix("sha256:")
        && value.utf8.count == 71
        && value.utf8.dropFirst(7).allSatisfy { byte in
            (byte >= 48 && byte <= 57)
                || (byte >= 65 && byte <= 70)
                || (byte >= 97 && byte <= 102)
        }
}
