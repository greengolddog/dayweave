import Foundation

private let googleScheduleZeroUUID = UUID(
    uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
)

private struct GoogleScheduleDynamicCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) {
        self.stringValue = stringValue
        intValue = nil
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}

private func requireExactScheduleGoogleKeys<Key>(
    _ keyType: Key.Type,
    from decoder: any Decoder
) throws where Key: CodingKey & CaseIterable, Key.AllCases: Collection {
    let container = try decoder.container(keyedBy: GoogleScheduleDynamicCodingKey.self)
    let expected = Set(Key.allCases.map(\.stringValue))
    let actual = Set(container.allKeys.map(\.stringValue))
    guard actual == expected else {
        throw DecodingError.dataCorrupted(
            .init(
                codingPath: decoder.codingPath,
                debugDescription: "Unexpected generated-schedule Google publication fields"
            )
        )
    }
}

private func scheduleGoogleDecodingError(
    _ decoder: any Decoder,
    _ description: String
) -> DecodingError {
    .dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: description))
}

private func scheduleGoogleEncodingError(
    _ encoder: any Encoder,
    _ description: String
) -> EncodingError {
    .invalidValue(description, .init(codingPath: encoder.codingPath, debugDescription: description))
}

private func isFiniteScheduleGoogleDate(_ value: Date) -> Bool {
    value.timeIntervalSinceReferenceDate.isFinite
}

private func isValidScheduleGoogleText(_ value: String, maximumUTF8Bytes: Int) -> Bool {
    !value.isEmpty
        && value.utf8.count <= maximumUTF8Bytes
        && value == value.trimmingCharacters(in: .whitespacesAndNewlines)
        && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
}

func isValidGoogleSchedulePreviewHash(_ value: String) -> Bool {
    value.utf8.count == 64
        && value.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (97...102).contains(byte)
        }
}

func isValidGoogleScheduleApprovalCapability(_ value: String) -> Bool {
    let prefix = "dw_gsa1_"
    guard value.hasPrefix(prefix) else { return false }
    let payload = String(value.dropFirst(prefix.count))
    guard payload.utf8.count == 43,
          payload.last.map({ "AEIMQUYcgkosw048".contains($0) }) == true,
          payload.utf8.allSatisfy({ byte in
              (65...90).contains(byte)
                  || (97...122).contains(byte)
                  || (48...57).contains(byte)
                  || byte == 45
                  || byte == 95
          }) else {
        return false
    }
    let standard = payload
        .replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/") + "="
    guard let decoded = Data(base64Encoded: standard), decoded.count == 32 else {
        return false
    }
    return decoded.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "") == payload
}

struct GoogleSchedulePublicationPreviewRequest: Codable, Equatable, Sendable {
    let collectionID: UUID
    let expectedScheduleRevisionID: UUID

    var isValid: Bool {
        collectionID != googleScheduleZeroUUID
            && expectedScheduleRevisionID != googleScheduleZeroUUID
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case collectionID = "collection_id"
        case expectedScheduleRevisionID = "expected_schedule_revision_id"
    }

    init(collectionID: UUID, expectedScheduleRevisionID: UUID) {
        self.collectionID = collectionID
        self.expectedScheduleRevisionID = expectedScheduleRevisionID
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        collectionID = try container.decode(UUID.self, forKey: .collectionID)
        expectedScheduleRevisionID = try container.decode(
            UUID.self,
            forKey: .expectedScheduleRevisionID
        )
        guard isValid else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule preview request")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard isValid else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule preview request")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(collectionID, forKey: .collectionID)
        try container.encode(expectedScheduleRevisionID, forKey: .expectedScheduleRevisionID)
    }
}

struct GoogleSchedulePublicationApprovalRequest: Codable, Equatable, Sendable {
    let expectedPreviewHash: String

    var isValid: Bool { isValidGoogleSchedulePreviewHash(expectedPreviewHash) }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case expectedPreviewHash = "expected_preview_hash"
    }

    init(expectedPreviewHash: String) {
        self.expectedPreviewHash = expectedPreviewHash
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        expectedPreviewHash = try decoder.container(keyedBy: CodingKeys.self)
            .decode(String.self, forKey: .expectedPreviewHash)
        guard isValid else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule approval request")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard isValid else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule approval request")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(expectedPreviewHash, forKey: .expectedPreviewHash)
    }
}

struct GoogleSchedulePublicationEnqueueRequest: Codable, Equatable, Sendable {
    let previewID: UUID
    let collectionID: UUID
    let expectedScheduleRevisionID: UUID
    let approvalCapability: String

    var isValid: Bool {
        previewID != googleScheduleZeroUUID
            && collectionID != googleScheduleZeroUUID
            && expectedScheduleRevisionID != googleScheduleZeroUUID
            && isValidGoogleScheduleApprovalCapability(approvalCapability)
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case previewID = "preview_id"
        case collectionID = "collection_id"
        case expectedScheduleRevisionID = "expected_schedule_revision_id"
        case approvalCapability = "approval_capability"
    }

    init(
        previewID: UUID,
        collectionID: UUID,
        expectedScheduleRevisionID: UUID,
        approvalCapability: String
    ) {
        self.previewID = previewID
        self.collectionID = collectionID
        self.expectedScheduleRevisionID = expectedScheduleRevisionID
        self.approvalCapability = approvalCapability
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        previewID = try container.decode(UUID.self, forKey: .previewID)
        collectionID = try container.decode(UUID.self, forKey: .collectionID)
        expectedScheduleRevisionID = try container.decode(
            UUID.self,
            forKey: .expectedScheduleRevisionID
        )
        approvalCapability = try container.decode(String.self, forKey: .approvalCapability)
        guard isValid else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule enqueue request")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard isValid else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule enqueue request")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(previewID, forKey: .previewID)
        try container.encode(collectionID, forKey: .collectionID)
        try container.encode(expectedScheduleRevisionID, forKey: .expectedScheduleRevisionID)
        try container.encode(approvalCapability, forKey: .approvalCapability)
    }
}

extension GoogleSchedulePublicationEnqueueRequest: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Approved generated-schedule Google publication request" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

enum GoogleSchedulePublicationOperation: String, Codable, Equatable, Sendable {
    case create
    case update
    case delete
    case noop
}

struct GoogleSchedulePublicationChange: Codable, Equatable, Identifiable, Sendable {
    let ordinal: UInt32
    let slotID: UUID
    let sourceBlockID: UUID?
    let operation: GoogleSchedulePublicationOperation
    let providerResourceID: String?
    let providerETag: String?
    let summary: String
    let startsAt: Date
    let endsAt: Date

    var id: UUID { slotID }

    var hasValidShape: Bool {
        guard ordinal < 10_000
            && slotID != googleScheduleZeroUUID
            && sourceBlockID != googleScheduleZeroUUID
            && providerResourceID.map({
                isValidScheduleGoogleText($0, maximumUTF8Bytes: 4_096)
            }) != false
            && providerETag.map({
                isValidScheduleGoogleText($0, maximumUTF8Bytes: 4_096)
            }) != false
            && isValidScheduleGoogleText(summary, maximumUTF8Bytes: 2_048)
            && isFiniteScheduleGoogleDate(startsAt)
            && isFiniteScheduleGoogleDate(endsAt)
            && startsAt < endsAt else {
            return false
        }
        switch operation {
        case .create:
            return sourceBlockID != nil
                && providerResourceID == nil
                && providerETag == nil
        case .update, .noop:
            return sourceBlockID != nil
                && providerResourceID != nil
                && providerETag != nil
        case .delete:
            return sourceBlockID == nil
                && providerResourceID != nil
                && providerETag != nil
        }
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case ordinal
        case slotID = "slot_id"
        case sourceBlockID = "source_block_id"
        case operation
        case providerResourceID = "provider_resource_id"
        case providerETag = "provider_etag"
        case summary
        case startsAt = "starts_at"
        case endsAt = "ends_at"
    }

    init(
        ordinal: UInt32,
        slotID: UUID,
        sourceBlockID: UUID?,
        operation: GoogleSchedulePublicationOperation,
        providerResourceID: String?,
        providerETag: String?,
        summary: String,
        startsAt: Date,
        endsAt: Date
    ) {
        self.ordinal = ordinal
        self.slotID = slotID
        self.sourceBlockID = sourceBlockID
        self.operation = operation
        self.providerResourceID = providerResourceID
        self.providerETag = providerETag
        self.summary = summary
        self.startsAt = startsAt
        self.endsAt = endsAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        ordinal = try container.decode(UInt32.self, forKey: .ordinal)
        slotID = try container.decode(UUID.self, forKey: .slotID)
        sourceBlockID = try container.decodeIfPresent(UUID.self, forKey: .sourceBlockID)
        operation = try container.decode(GoogleSchedulePublicationOperation.self, forKey: .operation)
        providerResourceID = try container.decodeIfPresent(
            String.self,
            forKey: .providerResourceID
        )
        providerETag = try container.decodeIfPresent(String.self, forKey: .providerETag)
        summary = try container.decode(String.self, forKey: .summary)
        startsAt = try container.decode(Date.self, forKey: .startsAt)
        endsAt = try container.decode(Date.self, forKey: .endsAt)
        guard hasValidShape else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule publication change")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule publication change")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(ordinal, forKey: .ordinal)
        try container.encode(slotID, forKey: .slotID)
        try container.encode(sourceBlockID, forKey: .sourceBlockID)
        try container.encode(operation, forKey: .operation)
        try container.encode(providerResourceID, forKey: .providerResourceID)
        try container.encode(providerETag, forKey: .providerETag)
        try container.encode(summary, forKey: .summary)
        try container.encode(startsAt, forKey: .startsAt)
        try container.encode(endsAt, forKey: .endsAt)
    }
}

struct GoogleSchedulePublicationPreview: Codable, Equatable, Sendable {
    static let maximumChanges = 10_000

    let id: UUID
    let accountID: UUID
    let collectionID: UUID
    let collectionRevision: UInt64
    let collectionDisplayName: String
    let scheduleRevisionID: UUID
    let scheduleRevisionNumber: UInt64
    let previewHash: String
    let createCount: UInt32
    let updateCount: UInt32
    let deleteCount: UInt32
    let noopCount: UInt32
    let changes: [GoogleSchedulePublicationChange]
    let expiresAt: Date

    var actionableCount: UInt32 { createCount + updateCount + deleteCount }

    var hasValidShape: Bool {
        guard id != googleScheduleZeroUUID,
              accountID != googleScheduleZeroUUID,
              collectionID != googleScheduleZeroUUID,
              collectionRevision > 0,
              collectionRevision <= UInt64(Int64.max),
              collectionDisplayName.unicodeScalars.count <= 500,
              isValidScheduleGoogleText(collectionDisplayName, maximumUTF8Bytes: 2_048),
              scheduleRevisionID != googleScheduleZeroUUID,
              scheduleRevisionNumber > 0,
              scheduleRevisionNumber <= UInt64(Int64.max),
              isValidGoogleSchedulePreviewHash(previewHash),
              changes.count <= Self.maximumChanges,
              changes.allSatisfy(\.hasValidShape),
              isFiniteScheduleGoogleDate(expiresAt) else {
            return false
        }
        let actualCreateCount = changes.count { $0.operation == .create }
        let actualUpdateCount = changes.count { $0.operation == .update }
        let actualDeleteCount = changes.count { $0.operation == .delete }
        let actualNoopCount = changes.count { $0.operation == .noop }
        guard Int(createCount) == actualCreateCount,
              Int(updateCount) == actualUpdateCount,
              Int(deleteCount) == actualDeleteCount,
              Int(noopCount) == actualNoopCount,
              Set(changes.map(\.slotID)).count == changes.count else {
            return false
        }
        return changes.enumerated().allSatisfy { index, change in
            change.ordinal == UInt32(index)
        }
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case id
        case accountID = "account_id"
        case collectionID = "collection_id"
        case collectionRevision = "collection_revision"
        case collectionDisplayName = "collection_display_name"
        case scheduleRevisionID = "schedule_revision_id"
        case scheduleRevisionNumber = "schedule_revision_number"
        case previewHash = "preview_hash"
        case createCount = "create_count"
        case updateCount = "update_count"
        case deleteCount = "delete_count"
        case noopCount = "noop_count"
        case changes
        case expiresAt = "expires_at"
    }

    init(
        id: UUID,
        accountID: UUID,
        collectionID: UUID,
        collectionRevision: UInt64,
        collectionDisplayName: String,
        scheduleRevisionID: UUID,
        scheduleRevisionNumber: UInt64,
        previewHash: String,
        createCount: UInt32,
        updateCount: UInt32,
        deleteCount: UInt32,
        noopCount: UInt32,
        changes: [GoogleSchedulePublicationChange],
        expiresAt: Date
    ) {
        self.id = id
        self.accountID = accountID
        self.collectionID = collectionID
        self.collectionRevision = collectionRevision
        self.collectionDisplayName = collectionDisplayName
        self.scheduleRevisionID = scheduleRevisionID
        self.scheduleRevisionNumber = scheduleRevisionNumber
        self.previewHash = previewHash
        self.createCount = createCount
        self.updateCount = updateCount
        self.deleteCount = deleteCount
        self.noopCount = noopCount
        self.changes = changes
        self.expiresAt = expiresAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        collectionID = try container.decode(UUID.self, forKey: .collectionID)
        collectionRevision = try container.decode(UInt64.self, forKey: .collectionRevision)
        collectionDisplayName = try container.decode(String.self, forKey: .collectionDisplayName)
        scheduleRevisionID = try container.decode(UUID.self, forKey: .scheduleRevisionID)
        scheduleRevisionNumber = try container.decode(UInt64.self, forKey: .scheduleRevisionNumber)
        previewHash = try container.decode(String.self, forKey: .previewHash)
        createCount = try container.decode(UInt32.self, forKey: .createCount)
        updateCount = try container.decode(UInt32.self, forKey: .updateCount)
        deleteCount = try container.decode(UInt32.self, forKey: .deleteCount)
        noopCount = try container.decode(UInt32.self, forKey: .noopCount)
        changes = try container.decode([GoogleSchedulePublicationChange].self, forKey: .changes)
        expiresAt = try container.decode(Date.self, forKey: .expiresAt)
        guard hasValidShape else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule publication preview")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule publication preview")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(collectionID, forKey: .collectionID)
        try container.encode(collectionRevision, forKey: .collectionRevision)
        try container.encode(collectionDisplayName, forKey: .collectionDisplayName)
        try container.encode(scheduleRevisionID, forKey: .scheduleRevisionID)
        try container.encode(scheduleRevisionNumber, forKey: .scheduleRevisionNumber)
        try container.encode(previewHash, forKey: .previewHash)
        try container.encode(createCount, forKey: .createCount)
        try container.encode(updateCount, forKey: .updateCount)
        try container.encode(deleteCount, forKey: .deleteCount)
        try container.encode(noopCount, forKey: .noopCount)
        try container.encode(changes, forKey: .changes)
        try container.encode(expiresAt, forKey: .expiresAt)
    }
}

struct GoogleSchedulePublicationApproval: Codable, Equatable, Sendable {
    let previewID: UUID
    let approvalCapability: String
    let expiresAt: Date

    var hasValidShape: Bool {
        previewID != googleScheduleZeroUUID
            && isValidGoogleScheduleApprovalCapability(approvalCapability)
            && isFiniteScheduleGoogleDate(expiresAt)
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case previewID = "preview_id"
        case approvalCapability = "approval_capability"
        case expiresAt = "expires_at"
    }

    init(previewID: UUID, approvalCapability: String, expiresAt: Date) {
        self.previewID = previewID
        self.approvalCapability = approvalCapability
        self.expiresAt = expiresAt
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        previewID = try container.decode(UUID.self, forKey: .previewID)
        approvalCapability = try container.decode(String.self, forKey: .approvalCapability)
        expiresAt = try container.decode(Date.self, forKey: .expiresAt)
        guard hasValidShape else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule approval response")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule approval response")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(previewID, forKey: .previewID)
        try container.encode(approvalCapability, forKey: .approvalCapability)
        try container.encode(expiresAt, forKey: .expiresAt)
    }
}

extension GoogleSchedulePublicationApproval: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Generated-schedule Google publication approval" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

struct GoogleSchedulePublicationAccepted: Codable, Equatable, Sendable {
    let publicationID: UUID
    let replayed: Bool

    var hasValidShape: Bool { publicationID != googleScheduleZeroUUID }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case publicationID = "publication_id"
        case replayed
    }

    init(publicationID: UUID, replayed: Bool) {
        self.publicationID = publicationID
        self.replayed = replayed
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        publicationID = try container.decode(UUID.self, forKey: .publicationID)
        replayed = try container.decode(Bool.self, forKey: .replayed)
        guard hasValidShape else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule acceptance response")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule acceptance response")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(publicationID, forKey: .publicationID)
        try container.encode(replayed, forKey: .replayed)
    }
}

enum GoogleSchedulePublicationState: String, Codable, Equatable, Sendable {
    case pending
    case delivering
    case backoff
    case partiallyPublished = "partially_published"
    case published
    case conflict
    case failed
    case superseded

    var isTerminal: Bool {
        switch self {
        case .partiallyPublished, .published, .conflict, .failed, .superseded: true
        case .pending, .delivering, .backoff: false
        }
    }
}

struct GoogleSchedulePublicationStatus: Codable, Equatable, Sendable {
    let publicationID: UUID
    let accountID: UUID
    let collectionID: UUID
    let scheduleRevisionID: UUID
    let state: GoogleSchedulePublicationState
    let totalCount: UInt32
    let pendingCount: UInt32
    let deliveringCount: UInt32
    let publishedCount: UInt32
    let conflictedCount: UInt32
    let failedCount: UInt32
    let supersededCount: UInt32
    let createdAt: Date
    let completedAt: Date?
    let lastErrorCode: String?

    var hasValidShape: Bool {
        guard publicationID != googleScheduleZeroUUID,
              accountID != googleScheduleZeroUUID,
              collectionID != googleScheduleZeroUUID,
              scheduleRevisionID != googleScheduleZeroUUID,
              totalCount <= GoogleSchedulePublicationPreview.maximumChanges,
              isFiniteScheduleGoogleDate(createdAt),
              completedAt.map(isFiniteScheduleGoogleDate) != false,
              completedAt.map({ $0 >= createdAt }) != false,
              lastErrorCode.map({ code in
                  !code.isEmpty
                      && code.utf8.count <= 100
                      && code.unicodeScalars.allSatisfy(
                          CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789_").contains
                      )
              }) != false else {
            return false
        }
        let reported = [
            pendingCount,
            deliveringCount,
            publishedCount,
            conflictedCount,
            failedCount,
            supersededCount,
        ].map(UInt64.init).reduce(0, +)
        guard reported == UInt64(totalCount) else { return false }
        let stateMatchesCounts = switch state {
        case .delivering:
            deliveringCount > 0
        case .pending, .backoff:
            deliveringCount == 0 && pendingCount > 0
        case .published:
            publishedCount == totalCount
        case .partiallyPublished:
            pendingCount == 0
                && deliveringCount == 0
                && publishedCount > 0
                && publishedCount < totalCount
        case .conflict:
            pendingCount == 0
                && deliveringCount == 0
                && publishedCount == 0
                && conflictedCount > 0
        case .failed:
            pendingCount == 0
                && deliveringCount == 0
                && publishedCount == 0
                && conflictedCount == 0
                && failedCount > 0
        case .superseded:
            totalCount > 0
                && pendingCount == 0
                && deliveringCount == 0
                && publishedCount == 0
                && conflictedCount == 0
                && failedCount == 0
                && supersededCount == totalCount
        }
        return stateMatchesCounts
            && (state.isTerminal ? completedAt != nil : completedAt == nil)
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case publicationID = "publication_id"
        case accountID = "account_id"
        case collectionID = "collection_id"
        case scheduleRevisionID = "schedule_revision_id"
        case state
        case totalCount = "total_count"
        case pendingCount = "pending_count"
        case deliveringCount = "delivering_count"
        case publishedCount = "published_count"
        case conflictedCount = "conflicted_count"
        case failedCount = "failed_count"
        case supersededCount = "superseded_count"
        case createdAt = "created_at"
        case completedAt = "completed_at"
        case lastErrorCode = "last_error_code"
    }

    init(
        publicationID: UUID,
        accountID: UUID,
        collectionID: UUID,
        scheduleRevisionID: UUID,
        state: GoogleSchedulePublicationState,
        totalCount: UInt32,
        pendingCount: UInt32,
        deliveringCount: UInt32,
        publishedCount: UInt32,
        conflictedCount: UInt32,
        failedCount: UInt32,
        supersededCount: UInt32,
        createdAt: Date,
        completedAt: Date?,
        lastErrorCode: String?
    ) {
        self.publicationID = publicationID
        self.accountID = accountID
        self.collectionID = collectionID
        self.scheduleRevisionID = scheduleRevisionID
        self.state = state
        self.totalCount = totalCount
        self.pendingCount = pendingCount
        self.deliveringCount = deliveringCount
        self.publishedCount = publishedCount
        self.conflictedCount = conflictedCount
        self.failedCount = failedCount
        self.supersededCount = supersededCount
        self.createdAt = createdAt
        self.completedAt = completedAt
        self.lastErrorCode = lastErrorCode
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        publicationID = try container.decode(UUID.self, forKey: .publicationID)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        collectionID = try container.decode(UUID.self, forKey: .collectionID)
        scheduleRevisionID = try container.decode(UUID.self, forKey: .scheduleRevisionID)
        state = try container.decode(GoogleSchedulePublicationState.self, forKey: .state)
        totalCount = try container.decode(UInt32.self, forKey: .totalCount)
        pendingCount = try container.decode(UInt32.self, forKey: .pendingCount)
        deliveringCount = try container.decode(UInt32.self, forKey: .deliveringCount)
        publishedCount = try container.decode(UInt32.self, forKey: .publishedCount)
        conflictedCount = try container.decode(UInt32.self, forKey: .conflictedCount)
        failedCount = try container.decode(UInt32.self, forKey: .failedCount)
        supersededCount = try container.decode(UInt32.self, forKey: .supersededCount)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        completedAt = try container.decodeIfPresent(Date.self, forKey: .completedAt)
        lastErrorCode = try container.decodeIfPresent(String.self, forKey: .lastErrorCode)
        guard hasValidShape else {
            throw scheduleGoogleDecodingError(decoder, "Invalid schedule publication status")
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw scheduleGoogleEncodingError(encoder, "Invalid schedule publication status")
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(publicationID, forKey: .publicationID)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(collectionID, forKey: .collectionID)
        try container.encode(scheduleRevisionID, forKey: .scheduleRevisionID)
        try container.encode(state, forKey: .state)
        try container.encode(totalCount, forKey: .totalCount)
        try container.encode(pendingCount, forKey: .pendingCount)
        try container.encode(deliveringCount, forKey: .deliveringCount)
        try container.encode(publishedCount, forKey: .publishedCount)
        try container.encode(conflictedCount, forKey: .conflictedCount)
        try container.encode(failedCount, forKey: .failedCount)
        try container.encode(supersededCount, forKey: .supersededCount)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(completedAt, forKey: .completedAt)
        try container.encode(lastErrorCode, forKey: .lastErrorCode)
    }
}
