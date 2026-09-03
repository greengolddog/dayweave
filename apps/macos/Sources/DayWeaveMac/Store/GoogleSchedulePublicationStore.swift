import Foundation

enum GoogleSchedulePublicationRecoveryStage: String, Codable, Equatable, Sendable {
    case intent
    case previewed
    case approvalAttempted = "approval_attempted"
    case approved
    case accepted
}

/// The encrypted, exact recovery fence for one generated-schedule publication.
/// The one-time capability exists only in the approved stage and is removed as
/// soon as an acceptance is durably recorded. String conversion and reflection
/// expose stage metadata only.
struct GoogleSchedulePublicationRecoveryJournal: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumIntentLifetime: TimeInterval = 35 * 60
    static let maximumClockSkew: TimeInterval = 5 * 60

    let version: Int
    let recoveryID: UUID
    let operationGeneration: UInt64
    let configurationIdentifier: String
    let accountID: UUID
    let collectionID: UUID
    let expectedScheduleRevisionID: UUID
    let intentExpiresAt: Date
    let preview: GoogleSchedulePublicationPreview?
    let approvalAttempted: Bool
    let approvalCapability: String?
    let approvalExpiresAt: Date?
    let acceptance: GoogleSchedulePublicationAccepted?
    let deliveryStatus: GoogleSchedulePublicationStatus?
    let createdAt: Date

    init(
        recoveryID: UUID = UUID(),
        operationGeneration: UInt64,
        configurationIdentifier: String,
        accountID: UUID,
        collectionID: UUID,
        expectedScheduleRevisionID: UUID,
        intentExpiresAt: Date,
        preview: GoogleSchedulePublicationPreview? = nil,
        approvalAttempted: Bool = false,
        approvalCapability: String? = nil,
        approvalExpiresAt: Date? = nil,
        acceptance: GoogleSchedulePublicationAccepted? = nil,
        deliveryStatus: GoogleSchedulePublicationStatus? = nil,
        createdAt: Date
    ) throws {
        version = Self.currentVersion
        self.recoveryID = recoveryID
        self.operationGeneration = operationGeneration
        self.configurationIdentifier = configurationIdentifier
        self.accountID = accountID
        self.collectionID = collectionID
        self.expectedScheduleRevisionID = expectedScheduleRevisionID
        self.intentExpiresAt = intentExpiresAt
        self.preview = preview
        self.approvalAttempted = approvalAttempted
        self.approvalCapability = approvalCapability
        self.approvalExpiresAt = approvalExpiresAt
        self.acceptance = acceptance
        self.deliveryStatus = deliveryStatus
        self.createdAt = createdAt
        guard hasValidShape else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
        }
    }

    var stage: GoogleSchedulePublicationRecoveryStage {
        if acceptance != nil { return .accepted }
        if approvalCapability != nil { return .approved }
        if approvalAttempted { return .approvalAttempted }
        if preview != nil { return .previewed }
        return .intent
    }

    var isTerminal: Bool { deliveryStatus?.state.isTerminal == true }

    var previewRequest: GoogleSchedulePublicationPreviewRequest {
        GoogleSchedulePublicationPreviewRequest(
            collectionID: collectionID,
            expectedScheduleRevisionID: expectedScheduleRevisionID
        )
    }

    var enqueueRequest: GoogleSchedulePublicationEnqueueRequest? {
        guard let preview, let approvalCapability else { return nil }
        return GoogleSchedulePublicationEnqueueRequest(
            previewID: preview.id,
            collectionID: collectionID,
            expectedScheduleRevisionID: expectedScheduleRevisionID,
            approvalCapability: approvalCapability
        )
    }

    func recording(preview: GoogleSchedulePublicationPreview) throws -> Self {
        guard stage == .intent else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
        }
        return try replacing(preview: preview)
    }

    func recordingApprovalAttempt() throws -> Self {
        guard stage == .previewed else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
        }
        return try replacing(approvalAttempted: true)
    }

    func recording(approval: GoogleSchedulePublicationApproval) throws -> Self {
        guard stage == .approvalAttempted,
              approval.previewID == preview?.id else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
        }
        return try replacing(
            approvalCapability: approval.approvalCapability,
            approvalExpiresAt: approval.expiresAt
        )
    }

    /// Recording acceptance deliberately erases the bearer capability.
    func recording(acceptance: GoogleSchedulePublicationAccepted) throws -> Self {
        guard stage == .approved else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
        }
        return try Self(
            recoveryID: recoveryID,
            operationGeneration: operationGeneration,
            configurationIdentifier: configurationIdentifier,
            accountID: accountID,
            collectionID: collectionID,
            expectedScheduleRevisionID: expectedScheduleRevisionID,
            intentExpiresAt: intentExpiresAt,
            preview: preview,
            approvalAttempted: true,
            acceptance: acceptance,
            createdAt: createdAt
        )
    }

    func recording(status: GoogleSchedulePublicationStatus) throws -> Self {
        guard stage == .accepted else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
        }
        return try Self(
            recoveryID: recoveryID,
            operationGeneration: operationGeneration,
            configurationIdentifier: configurationIdentifier,
            accountID: accountID,
            collectionID: collectionID,
            expectedScheduleRevisionID: expectedScheduleRevisionID,
            intentExpiresAt: intentExpiresAt,
            preview: preview,
            approvalAttempted: true,
            acceptance: acceptance,
            deliveryStatus: status,
            createdAt: createdAt
        )
    }

    func canStartFresh(at date: Date) -> Bool {
        guard Self.isFinite(date), stage != .accepted, let authorityExpiresAt else {
            return false
        }
        return date >= authorityExpiresAt
    }

    func canDiscardExpired(at date: Date) -> Bool {
        guard Self.isFinite(date), stage != .accepted, let authorityExpiresAt else {
            return false
        }
        return date >= authorityExpiresAt.addingTimeInterval(Self.maximumClockSkew)
    }

    var safeDiscardAt: Date? {
        guard stage != .accepted else { return nil }
        return authorityExpiresAt?.addingTimeInterval(Self.maximumClockSkew)
    }

    func isValid(now: Date) -> Bool {
        hasValidShape
            && Self.isFinite(now)
            && createdAt <= now.addingTimeInterval(Self.maximumClockSkew)
    }

    var hasValidShape: Bool {
        guard version == Self.currentVersion,
              recoveryID != Self.zeroUUID,
              operationGeneration > 0,
              operationGeneration <= UInt64(Int64.max),
              GoogleDisconnectRetryJournal.isValidConfigurationIdentifier(
                  configurationIdentifier
              ),
              accountID != Self.zeroUUID,
              collectionID != Self.zeroUUID,
              expectedScheduleRevisionID != Self.zeroUUID,
              Self.isFinite(createdAt),
              Self.isFinite(intentExpiresAt),
              intentExpiresAt > createdAt,
              intentExpiresAt.timeIntervalSince(createdAt) <= Self.maximumIntentLifetime else {
            return false
        }

        if let preview {
            guard preview.hasValidShape,
                  preview.accountID == accountID,
                  preview.collectionID == collectionID,
                  preview.scheduleRevisionID == expectedScheduleRevisionID,
                  preview.expiresAt >= createdAt.addingTimeInterval(-Self.maximumClockSkew),
                  preview.expiresAt <= intentExpiresAt.addingTimeInterval(Self.maximumClockSkew)
            else { return false }
        } else if approvalAttempted
            || approvalCapability != nil
            || approvalExpiresAt != nil
            || acceptance != nil
            || deliveryStatus != nil {
            return false
        }

        if approvalCapability != nil, !approvalAttempted { return false }
        switch (approvalCapability, approvalExpiresAt, acceptance) {
        case (nil, nil, nil):
            if deliveryStatus != nil { return false }
        case let (capability?, expiresAt?, nil):
            guard let preview,
                  isValidGoogleScheduleApprovalCapability(capability),
                  Self.isFinite(expiresAt),
                  expiresAt >= createdAt.addingTimeInterval(-Self.maximumClockSkew),
                  expiresAt <= preview.expiresAt else {
                return false
            }
        case (nil, nil, let acceptance?):
            guard approvalAttempted,
                  acceptance.hasValidShape else { return false }
            if let deliveryStatus {
                guard deliveryStatus.hasValidShape,
                      deliveryStatus.publicationID == acceptance.publicationID,
                      deliveryStatus.accountID == accountID,
                      deliveryStatus.collectionID == collectionID,
                      deliveryStatus.scheduleRevisionID == expectedScheduleRevisionID,
                      deliveryStatus.createdAt >= createdAt.addingTimeInterval(
                          -Self.maximumClockSkew
                      ) else {
                    return false
                }
            }
        default:
            return false
        }
        return true
    }

    private var authorityExpiresAt: Date? {
        switch stage {
        case .intent: intentExpiresAt
        case .previewed, .approvalAttempted: preview?.expiresAt
        case .approved: approvalExpiresAt
        case .accepted: nil
        }
    }

    private func replacing(
        preview replacementPreview: GoogleSchedulePublicationPreview? = nil,
        approvalAttempted replacementAttempted: Bool? = nil,
        approvalCapability replacementCapability: String? = nil,
        approvalExpiresAt replacementApprovalExpiry: Date? = nil
    ) throws -> Self {
        try Self(
            recoveryID: recoveryID,
            operationGeneration: operationGeneration,
            configurationIdentifier: configurationIdentifier,
            accountID: accountID,
            collectionID: collectionID,
            expectedScheduleRevisionID: expectedScheduleRevisionID,
            intentExpiresAt: intentExpiresAt,
            preview: replacementPreview ?? preview,
            approvalAttempted: replacementAttempted ?? approvalAttempted,
            approvalCapability: replacementCapability ?? approvalCapability,
            approvalExpiresAt: replacementApprovalExpiry ?? approvalExpiresAt,
            acceptance: acceptance,
            deliveryStatus: deliveryStatus,
            createdAt: createdAt
        )
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case recoveryID = "recovery_id"
        case operationGeneration = "operation_generation"
        case configurationIdentifier = "configuration_identifier"
        case accountID = "account_id"
        case collectionID = "collection_id"
        case expectedScheduleRevisionID = "expected_schedule_revision_id"
        case intentExpiresAt = "intent_expires_at"
        case preview
        case approvalAttempted = "approval_attempted"
        case approvalCapability = "approval_capability"
        case approvalExpiresAt = "approval_expires_at"
        case acceptance
        case deliveryStatus = "delivery_status"
        case createdAt = "created_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactScheduleJournalKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        do {
            try self.init(
                recoveryID: container.decode(UUID.self, forKey: .recoveryID),
                operationGeneration: container.decode(UInt64.self, forKey: .operationGeneration),
                configurationIdentifier: container.decode(
                    String.self,
                    forKey: .configurationIdentifier
                ),
                accountID: container.decode(UUID.self, forKey: .accountID),
                collectionID: container.decode(UUID.self, forKey: .collectionID),
                expectedScheduleRevisionID: container.decode(
                    UUID.self,
                    forKey: .expectedScheduleRevisionID
                ),
                intentExpiresAt: container.decode(Date.self, forKey: .intentExpiresAt),
                preview: container.decodeIfPresent(
                    GoogleSchedulePublicationPreview.self,
                    forKey: .preview
                ),
                approvalAttempted: container.decode(Bool.self, forKey: .approvalAttempted),
                approvalCapability: container.decodeIfPresent(
                    String.self,
                    forKey: .approvalCapability
                ),
                approvalExpiresAt: container.decodeIfPresent(
                    Date.self,
                    forKey: .approvalExpiresAt
                ),
                acceptance: container.decodeIfPresent(
                    GoogleSchedulePublicationAccepted.self,
                    forKey: .acceptance
                ),
                deliveryStatus: container.decodeIfPresent(
                    GoogleSchedulePublicationStatus.self,
                    forKey: .deliveryStatus
                ),
                createdAt: container.decode(Date.self, forKey: .createdAt)
            )
            let decodedVersion = try container.decode(Int.self, forKey: .version)
            guard version == decodedVersion else {
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            }
        } catch let error as DecodingError {
            throw error
        } catch {
            throw scheduleJournalDecodingError(
                decoder.codingPath,
                "Invalid generated-schedule Google publication recovery journal"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw EncodingError.invalidValue(
                self,
                .init(codingPath: encoder.codingPath, debugDescription: "Invalid schedule journal")
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(recoveryID, forKey: .recoveryID)
        try container.encode(operationGeneration, forKey: .operationGeneration)
        try container.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(collectionID, forKey: .collectionID)
        try container.encode(expectedScheduleRevisionID, forKey: .expectedScheduleRevisionID)
        try container.encode(intentExpiresAt, forKey: .intentExpiresAt)
        try container.encode(preview, forKey: .preview)
        try container.encode(approvalAttempted, forKey: .approvalAttempted)
        try container.encode(approvalCapability, forKey: .approvalCapability)
        try container.encode(approvalExpiresAt, forKey: .approvalExpiresAt)
        try container.encode(acceptance, forKey: .acceptance)
        try container.encode(deliveryStatus, forKey: .deliveryStatus)
        try container.encode(createdAt, forKey: .createdAt)
    }

    private static func isFinite(_ date: Date) -> Bool {
        date.timeIntervalSinceReferenceDate.isFinite
    }

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}

extension GoogleSchedulePublicationRecoveryJournal: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String {
        "Generated-schedule Google publication recovery (\(stage.rawValue))"
    }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

private struct GoogleScheduleJournalDynamicCodingKey: CodingKey {
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

private func requireExactScheduleJournalKeys<Key: CodingKey & CaseIterable>(
    _ keyType: Key.Type,
    from decoder: any Decoder
) throws {
    let actual = Set(
        try decoder.container(keyedBy: GoogleScheduleJournalDynamicCodingKey.self)
            .allKeys.map(\.stringValue)
    )
    let expected = Set(Key.allCases.map(\.stringValue))
    guard actual == expected else {
        throw scheduleJournalDecodingError(
            decoder.codingPath,
            "Unsupported generated-schedule Google publication recovery fields"
        )
    }
}

private func scheduleJournalDecodingError(
    _ codingPath: [any CodingKey],
    _ description: String
) -> DecodingError {
    .dataCorrupted(.init(codingPath: codingPath, debugDescription: description))
}

@MainActor
protocol GoogleSchedulePublicationRecoveryStoring: AnyObject {
    func loadGoogleSchedulePublicationRecoveryJournal() throws
        -> GoogleSchedulePublicationRecoveryJournal?
    func saveGoogleSchedulePublicationRecoveryJournal(
        _ journal: GoogleSchedulePublicationRecoveryJournal
    ) throws
    func clearGoogleSchedulePublicationRecoveryJournal(
        _ expected: GoogleSchedulePublicationRecoveryJournal
    ) throws
}

struct GoogleSchedulePublicationApprovalConfirmation: Equatable, Sendable {
    fileprivate let recoveryID: UUID
    fileprivate let operationGeneration: UInt64
    fileprivate let configurationIdentifier: String
    fileprivate let accountID: UUID
    fileprivate let previewID: UUID
    fileprivate let previewHash: String
}

extension GoogleSchedulePublicationApprovalConfirmation: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Explicit generated-schedule Google preview confirmation" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

enum GoogleSchedulePublicationWorkflowStatus: Equatable, Sendable {
    case privacyProtected
    case idle
    case previewing
    case awaitingApproval(expiresAt: Date)
    case approving
    case enqueueing
    case refreshingStatus
    case active(GoogleSchedulePublicationStatus)
    case completed(GoogleSchedulePublicationStatus)
    case approvedReplayRequired
    case expirySafetyDelay(discardAfter: Date)
    case expired
    case recoveryRequired(String)
    case failed(String)

    var message: String {
        switch self {
        case .privacyProtected:
            "Schedule publication details are hidden while DayWeave is locked."
        case .idle:
            "Choose a writable Google Calendar and review the exact generated-schedule changes."
        case .previewing:
            "Preparing review-safe Google Calendar changes…"
        case let .awaitingApproval(expiresAt):
            "Review every change, then approve explicitly before \(expiresAt.formatted(date: .omitted, time: .shortened))."
        case .approving:
            "Creating a one-time approval for this exact preview…"
        case .enqueueing:
            "Durably queueing the approved schedule publication…"
        case .refreshingStatus:
            "Refreshing publication status…"
        case let .active(status):
            "Google Calendar delivery is \(status.state.displayName.lowercased())."
        case let .completed(status):
            status.state == .published
                ? "The generated schedule is published to Google Calendar."
                : "Google Calendar publication finished as \(status.state.displayName.lowercased())."
        case .approvedReplayRequired:
            "This exact preview was already approved. Review the replay warning before retrying its enqueue request."
        case let .expirySafetyDelay(discardAfter):
            "The saved authority expired locally. It can be discarded after \(discardAfter.formatted(date: .omitted, time: .shortened)) once clock-skew safety has elapsed."
        case .expired:
            "The saved publication authority expired. Discard it before preparing a fresh preview."
        case let .recoveryRequired(message), let .failed(message):
            message
        }
    }

    var isWorking: Bool {
        switch self {
        case .previewing, .approving, .enqueueing, .refreshingStatus: true
        case .privacyProtected, .idle, .awaitingApproval, .active, .completed,
             .approvedReplayRequired,
             .expirySafetyDelay, .expired, .recoveryRequired, .failed: false
        }
    }
}

extension GoogleSchedulePublicationState {
    var displayName: String {
        switch self {
        case .pending: "Pending"
        case .delivering: "Delivering"
        case .backoff: "Retrying"
        case .partiallyPublished: "Partially published"
        case .published: "Published"
        case .conflict: "Conflict"
        case .failed: "Failed"
        case .superseded: "Superseded"
        }
    }
}

extension GoogleSchedulePublicationWorkflowStatus: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { message }
    var debugDescription: String { message }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": message], displayStyle: .enum)
    }
}

enum GoogleSchedulePublicationWorkflowError: Error, Equatable, Sendable, LocalizedError {
    case privacyBoundary
    case operationInProgress
    case invalidIntent
    case pendingRecovery
    case invalidRecoveryJournal
    case invalidRecoveryTransition
    case recoveryChanged
    case configurationChanged
    case explicitApprovalRequired
    case invalidPreviewResponse
    case invalidApprovalResponse
    case invalidAcceptanceResponse
    case invalidStatusResponse
    case recoveryStillAuthorized
    case expiredRecoveryRequiresDiscard
    case publicationStillActive

    var errorDescription: String? {
        switch self {
        case .privacyBoundary:
            "Unlock DayWeave before reviewing or publishing a schedule."
        case .operationInProgress:
            "Wait for the current schedule publication operation to finish."
        case .invalidIntent:
            "The generated-schedule publication request is invalid. Nothing was sent."
        case .pendingRecovery:
            "Finish or dismiss the saved schedule publication before starting another."
        case .invalidRecoveryJournal:
            "The encrypted schedule publication recovery record is invalid. No request was sent."
        case .invalidRecoveryTransition:
            "The schedule publication recovery stage changed unexpectedly."
        case .recoveryChanged:
            "The encrypted schedule publication recovery record changed during the operation."
        case .configurationChanged:
            "The DayWeave API authentication changed. Restore the matching session to recover this publication."
        case .explicitApprovalRequired:
            "Approve the exact schedule preview currently displayed."
        case .invalidPreviewResponse:
            "The server returned a preview for different schedule data. Nothing was approved."
        case .invalidApprovalResponse:
            "The server returned an invalid approval. The exact recovery record remains saved."
        case .invalidAcceptanceResponse:
            "The server returned an invalid publication acceptance. Recovery remains saved."
        case .invalidStatusResponse:
            "The server returned status for a different schedule publication."
        case .recoveryStillAuthorized:
            "The saved authority has not expired and cannot be discarded safely."
        case .expiredRecoveryRequiresDiscard:
            "Discard the expired recovery before creating a fresh schedule preview."
        case .publicationStillActive:
            "The accepted publication is still active and cannot be dismissed."
        }
    }
}

private struct ActiveGoogleSchedulePublicationOperation: Sendable {
    let id: UUID
    let privacyGeneration: UInt64
    let intentGeneration: UInt64
    let transport: any GoogleSchedulePublicationTransport
    let configurationIdentifier: String
}

@MainActor
final class GoogleSchedulePublicationStore: ObservableObject {
    typealias TransportProvider = () throws -> any GoogleSchedulePublicationTransport

    @Published private(set) var status: GoogleSchedulePublicationWorkflowStatus
    @Published private(set) var preview: GoogleSchedulePublicationPreview?
    @Published private(set) var deliveryStatus: GoogleSchedulePublicationStatus?
    @Published private(set) var recoveryStage: GoogleSchedulePublicationRecoveryStage?
    @Published private(set) var hasPendingRecovery = false
    @Published private(set) var hasSavedPublication = false

    private let recoveryStore: any GoogleSchedulePublicationRecoveryStoring
    private let transportProvider: TransportProvider
    private let now: @Sendable () -> Date
    private var privacyAvailable: Bool
    private var privacyGeneration: UInt64 = 1
    private var operationSequence: UInt64 = 0
    private var activeOperation: ActiveGoogleSchedulePublicationOperation?
    private var presentedJournal: GoogleSchedulePublicationRecoveryJournal?

    init(
        recoveryStore: any GoogleSchedulePublicationRecoveryStoring,
        transportProvider: @escaping TransportProvider,
        privacyAvailable: Bool = false,
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.recoveryStore = recoveryStore
        self.transportProvider = transportProvider
        self.privacyAvailable = privacyAvailable
        self.now = now
        status = privacyAvailable ? .idle : .privacyProtected
        refreshRecoveryPresentation()
    }

    var approvalConfirmation: GoogleSchedulePublicationApprovalConfirmation? {
        guard privacyAvailable,
              let journal = presentedJournal,
              journal.stage == .previewed,
              let preview = journal.preview,
              preview.expiresAt > now(),
              journal.isValid(now: now()) else {
            return nil
        }
        return confirmation(for: journal, preview: preview)
    }

    func setPrivacyAvailable(_ available: Bool) {
        guard privacyAvailable != available else { return }
        privacyAvailable = available
        advancePrivacyGeneration()
        activeOperation = nil
        preview = nil
        deliveryStatus = nil
        recoveryStage = nil
        presentedJournal = nil
        if available {
            refreshRecoveryPresentation()
        } else {
            hasPendingRecovery = false
            hasSavedPublication = false
            status = .privacyProtected
        }
    }

    func configurationDidChange() {
        advancePrivacyGeneration()
        activeOperation = nil
        preview = nil
        deliveryStatus = nil
        recoveryStage = nil
        presentedJournal = nil
        refreshRecoveryPresentation()
    }

    @discardableResult
    func preparePreview(
        accountID: UUID,
        collectionID: UUID,
        scheduleRevisionID: UUID
    ) async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        let request = GoogleSchedulePublicationPreviewRequest(
            collectionID: collectionID,
            expectedScheduleRevisionID: scheduleRevisionID
        )
        let currentDate = now()
        guard accountID != Self.zeroUUID,
              request.isValid,
              currentDate.timeIntervalSinceReferenceDate.isFinite else {
            status = .failed(GoogleSchedulePublicationWorkflowError.invalidIntent.localizedDescription)
            return false
        }

        var operation: ActiveGoogleSchedulePublicationOperation?
        defer { if let operation { finishOperation(operation.id) } }
        do {
            if let existing = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal() {
                guard existing.isValid(now: currentDate) else {
                    throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
                }
                if existing.canStartFresh(at: currentDate) {
                    throw GoogleSchedulePublicationWorkflowError.expiredRecoveryRequiresDiscard
                }
                throw GoogleSchedulePublicationWorkflowError.pendingRecovery
            }
            let generation = try nextOperationGeneration()
            let context = try beginOperation(intentGeneration: generation)
            operation = context
            status = .previewing
            let journal = try GoogleSchedulePublicationRecoveryJournal(
                operationGeneration: generation,
                configurationIdentifier: context.configurationIdentifier,
                accountID: accountID,
                collectionID: collectionID,
                expectedScheduleRevisionID: scheduleRevisionID,
                intentExpiresAt: currentDate.addingTimeInterval(
                    GoogleSchedulePublicationRecoveryJournal.maximumIntentLifetime
                ),
                createdAt: currentDate
            )
            try recoveryStore.saveGoogleSchedulePublicationRecoveryJournal(journal)
            updatePresentation(journal)
            do {
                return try await performPreview(journal, using: context)
            } catch {
                try clearDefinitivelyRejectedPreview(error, journal: journal, using: context)
                throw error
            }
        } catch {
            handleFailure(error, operation: operation)
            return false
        }
    }

    @discardableResult
    func approveAndEnqueue(
        _ confirmation: GoogleSchedulePublicationApprovalConfirmation
    ) async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        var operation: ActiveGoogleSchedulePublicationOperation?
        defer { if let operation { finishOperation(operation.id) } }
        do {
            let currentDate = now()
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal(),
                  journal.isValid(now: currentDate),
                  journal.stage == .previewed,
                  let preview = journal.preview,
                  confirmation == self.confirmation(for: journal, preview: preview) else {
                throw GoogleSchedulePublicationWorkflowError.explicitApprovalRequired
            }
            guard !journal.canStartFresh(at: currentDate) else {
                presentExpired(journal)
                return false
            }
            let context = try beginOperation(intentGeneration: journal.operationGeneration)
            operation = context
            try requireConfiguration(journal, using: context)
            status = .approving

            // Approval is one-shot. Persist that the ceremony started before
            // sending so a lost response never triggers a second capability.
            let attempted = try journal.recordingApprovalAttempt()
            try persistTransition(from: journal, to: attempted)
            updatePresentation(attempted)
            previewForDisplay(nil)

            let approval = try await context.transport.approveGoogleSchedulePublication(
                accountID: attempted.accountID,
                previewID: preview.id,
                expectedPreviewHash: preview.previewHash
            )
            try requireCurrent(context)
            guard approval.hasValidShape,
                  approval.previewID == preview.id,
                  approval.expiresAt >= attempted.createdAt.addingTimeInterval(
                      -GoogleSchedulePublicationRecoveryJournal.maximumClockSkew
                  ),
                  approval.expiresAt <= preview.expiresAt else {
                throw GoogleSchedulePublicationWorkflowError.invalidApprovalResponse
            }
            let approved = try attempted.recording(approval: approval)
            try persistTransition(from: attempted, to: approved)
            updatePresentation(approved)
            return try await performEnqueue(approved, using: context)
        } catch {
            handleFailure(error, operation: operation)
            return false
        }
    }

    @discardableResult
    func refreshStatus() async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        var operation: ActiveGoogleSchedulePublicationOperation?
        defer { if let operation { finishOperation(operation.id) } }
        do {
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal(),
                  journal.isValid(now: now()),
                  journal.stage == .accepted else {
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            }
            let context = try beginOperation(intentGeneration: journal.operationGeneration)
            operation = context
            try requireConfiguration(journal, using: context)
            return try await performStatusRefresh(journal, using: context)
        } catch {
            handleFailure(error, operation: operation)
            return false
        }
    }

    /// Replays only operations already authorized by encrypted state. A
    /// recovered preview is always returned to the user for explicit review.
    @discardableResult
    func recoverPendingPublication() async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        var operation: ActiveGoogleSchedulePublicationOperation?
        defer { if let operation { finishOperation(operation.id) } }
        do {
            let currentDate = now()
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal()
            else {
                clearPresentation()
                status = .idle
                return true
            }
            guard journal.isValid(now: currentDate) else {
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            }
            if journal.stage != .accepted, journal.canStartFresh(at: currentDate),
               journal.stage != .approved {
                presentExpired(journal)
                return false
            }
            if journal.stage == .approvalAttempted {
                updatePresentation(journal)
                previewForDisplay(nil)
                status = .recoveryRequired(Self.uncertainApprovalMessage)
                return true
            }
            if journal.stage == .approved {
                updatePresentation(journal)
                previewForDisplay(nil)
                status = .approvedReplayRequired
                return true
            }

            let context = try beginOperation(intentGeneration: journal.operationGeneration)
            operation = context
            try requireConfiguration(journal, using: context)
            switch journal.stage {
            case .intent:
                status = .previewing
                return try await performPreview(journal, using: context)
            case .previewed:
                try requireCurrent(context)
                updatePresentation(journal)
                previewForDisplay(journal.preview)
                if let expiresAt = journal.preview?.expiresAt {
                    status = .awaitingApproval(expiresAt: expiresAt)
                    return true
                }
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            case .approvalAttempted:
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
            case .approved:
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryTransition
            case .accepted:
                if journal.isTerminal {
                    updatePresentation(journal)
                    status = .completed(journal.deliveryStatus!)
                    return true
                }
                return try await performStatusRefresh(journal, using: context)
            }
        } catch {
            handleFailure(error, operation: operation)
            return false
        }
    }

    /// Replays only the exact enqueue already authorized by the encrypted
    /// approved journal. Callers must put this method behind an explicit user
    /// confirmation because the earlier response may have been lost before
    /// the server durably accepted the publication.
    @discardableResult
    func replayApprovedEnqueue() async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        var operation: ActiveGoogleSchedulePublicationOperation?
        defer { if let operation { finishOperation(operation.id) } }
        do {
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal(),
                  journal.isValid(now: now()),
                  journal.stage == .approved else {
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            }
            let context = try beginOperation(intentGeneration: journal.operationGeneration)
            operation = context
            try requireConfiguration(journal, using: context)
            return try await performEnqueue(journal, using: context)
        } catch {
            handleFailure(error, operation: operation)
            return false
        }
    }

    @discardableResult
    func discardExpiredRecovery() -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        do {
            guard activeOperation == nil else {
                throw GoogleSchedulePublicationWorkflowError.operationInProgress
            }
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal(),
                  journal.isValid(now: now()) else {
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            }
            guard journal.canStartFresh(at: now()) else {
                throw GoogleSchedulePublicationWorkflowError.recoveryStillAuthorized
            }
            guard journal.canDiscardExpired(at: now()) else {
                presentExpired(journal)
                return false
            }
            try recoveryStore.clearGoogleSchedulePublicationRecoveryJournal(journal)
            clearPresentation()
            status = .idle
            return true
        } catch {
            handleFailure(error, operation: nil)
            return false
        }
    }

    @discardableResult
    func dismissCompletedPublication() -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        do {
            guard activeOperation == nil else {
                throw GoogleSchedulePublicationWorkflowError.operationInProgress
            }
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal(),
                  journal.isValid(now: now()),
                  journal.stage == .accepted,
                  journal.isTerminal else {
                throw GoogleSchedulePublicationWorkflowError.publicationStillActive
            }
            try recoveryStore.clearGoogleSchedulePublicationRecoveryJournal(journal)
            clearPresentation()
            status = .idle
            return true
        } catch {
            handleFailure(error, operation: nil)
            return false
        }
    }

    private func performPreview(
        _ journal: GoogleSchedulePublicationRecoveryJournal,
        using operation: ActiveGoogleSchedulePublicationOperation
    ) async throws -> Bool {
        let response = try await operation.transport.previewGoogleSchedulePublication(
            accountID: journal.accountID,
            request: journal.previewRequest
        )
        try requireCurrent(operation)
        guard response.hasValidShape,
              response.accountID == journal.accountID,
              response.collectionID == journal.collectionID,
              response.scheduleRevisionID == journal.expectedScheduleRevisionID,
              response.expiresAt >= journal.createdAt.addingTimeInterval(
                  -GoogleSchedulePublicationRecoveryJournal.maximumClockSkew
              ),
              response.expiresAt <= journal.intentExpiresAt.addingTimeInterval(
                  GoogleSchedulePublicationRecoveryJournal.maximumClockSkew
              ) else {
            throw GoogleSchedulePublicationWorkflowError.invalidPreviewResponse
        }
        let previewed = try journal.recording(preview: response)
        try persistTransition(from: journal, to: previewed)
        updatePresentation(previewed)
        previewForDisplay(response)
        if previewed.canStartFresh(at: now()) {
            presentExpired(previewed)
            return false
        }
        status = .awaitingApproval(expiresAt: response.expiresAt)
        return true
    }

    private func performEnqueue(
        _ journal: GoogleSchedulePublicationRecoveryJournal,
        using operation: ActiveGoogleSchedulePublicationOperation
    ) async throws -> Bool {
        guard journal.stage == .approved,
              let request = journal.enqueueRequest,
              request.isValid else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
        }
        status = .enqueueing
        let response = try await operation.transport.enqueueGoogleSchedulePublication(
            accountID: journal.accountID,
            request: request
        )
        try requireCurrent(operation)
        guard response.hasValidShape else {
            throw GoogleSchedulePublicationWorkflowError.invalidAcceptanceResponse
        }
        let accepted = try journal.recording(acceptance: response)
        try persistTransition(from: journal, to: accepted)
        updatePresentation(accepted)
        previewForDisplay(nil)
        return try await performStatusRefresh(accepted, using: operation)
    }

    private func performStatusRefresh(
        _ journal: GoogleSchedulePublicationRecoveryJournal,
        using operation: ActiveGoogleSchedulePublicationOperation
    ) async throws -> Bool {
        guard journal.stage == .accepted,
              let acceptance = journal.acceptance else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
        }
        status = .refreshingStatus
        let response = try await operation.transport.googleSchedulePublicationStatus(
            accountID: journal.accountID,
            publicationID: acceptance.publicationID
        )
        try requireCurrent(operation)
        guard response.hasValidShape,
              response.publicationID == acceptance.publicationID,
              response.accountID == journal.accountID,
              response.collectionID == journal.collectionID,
              response.scheduleRevisionID == journal.expectedScheduleRevisionID else {
            throw GoogleSchedulePublicationWorkflowError.invalidStatusResponse
        }
        let updated = try journal.recording(status: response)
        try persistTransition(from: journal, to: updated)
        updatePresentation(updated)
        status = response.state.isTerminal ? .completed(response) : .active(response)
        return true
    }

    private func persistTransition(
        from existing: GoogleSchedulePublicationRecoveryJournal,
        to replacement: GoogleSchedulePublicationRecoveryJournal
    ) throws {
        guard replacement.isValid(now: now()),
              try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal() == existing else {
            throw GoogleSchedulePublicationWorkflowError.recoveryChanged
        }
        try recoveryStore.saveGoogleSchedulePublicationRecoveryJournal(replacement)
    }

    private func clearDefinitivelyRejectedPreview(
        _ error: Error,
        journal: GoogleSchedulePublicationRecoveryJournal,
        using operation: ActiveGoogleSchedulePublicationOperation
    ) throws {
        guard let apiError = error as? DayWeaveAPIError,
              case let .server(statusCode, _, _, _) = apiError,
              (400..<500).contains(statusCode) else {
            return
        }
        try requireCurrent(operation)
        guard try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal() == journal else {
            throw GoogleSchedulePublicationWorkflowError.recoveryChanged
        }
        try recoveryStore.clearGoogleSchedulePublicationRecoveryJournal(journal)
        clearPresentation()
    }

    private func beginOperation(
        intentGeneration: UInt64
    ) throws -> ActiveGoogleSchedulePublicationOperation {
        guard privacyAvailable else {
            throw GoogleSchedulePublicationWorkflowError.privacyBoundary
        }
        guard activeOperation == nil else {
            throw GoogleSchedulePublicationWorkflowError.operationInProgress
        }
        let transport = try transportProvider()
        guard GoogleDisconnectRetryJournal.isValidConfigurationIdentifier(
            transport.configurationIdentifier
        ) else {
            throw GoogleSchedulePublicationWorkflowError.configurationChanged
        }
        let operation = ActiveGoogleSchedulePublicationOperation(
            id: UUID(),
            privacyGeneration: privacyGeneration,
            intentGeneration: intentGeneration,
            transport: transport,
            configurationIdentifier: transport.configurationIdentifier
        )
        activeOperation = operation
        return operation
    }

    private func finishOperation(_ id: UUID) {
        guard activeOperation?.id == id else { return }
        activeOperation = nil
    }

    private func requireCurrent(_ operation: ActiveGoogleSchedulePublicationOperation) throws {
        guard privacyAvailable,
              !Task.isCancelled,
              activeOperation?.id == operation.id,
              operation.privacyGeneration == privacyGeneration else {
            throw CancellationError()
        }
        let current = try transportProvider()
        guard current.configurationIdentifier == operation.configurationIdentifier else {
            throw GoogleSchedulePublicationWorkflowError.configurationChanged
        }
    }

    private func requireConfiguration(
        _ journal: GoogleSchedulePublicationRecoveryJournal,
        using operation: ActiveGoogleSchedulePublicationOperation
    ) throws {
        guard journal.operationGeneration == operation.intentGeneration,
              journal.configurationIdentifier == operation.configurationIdentifier else {
            throw GoogleSchedulePublicationWorkflowError.configurationChanged
        }
    }

    private func nextOperationGeneration() throws -> UInt64 {
        guard operationSequence < UInt64(Int64.max) else {
            throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
        }
        operationSequence += 1
        return operationSequence
    }

    private func confirmation(
        for journal: GoogleSchedulePublicationRecoveryJournal,
        preview: GoogleSchedulePublicationPreview
    ) -> GoogleSchedulePublicationApprovalConfirmation {
        GoogleSchedulePublicationApprovalConfirmation(
            recoveryID: journal.recoveryID,
            operationGeneration: journal.operationGeneration,
            configurationIdentifier: journal.configurationIdentifier,
            accountID: journal.accountID,
            previewID: preview.id,
            previewHash: preview.previewHash
        )
    }

    private func refreshRecoveryPresentation() {
        guard privacyAvailable else {
            status = .privacyProtected
            return
        }
        do {
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal()
            else {
                clearPresentation()
                status = .idle
                return
            }
            guard journal.isValid(now: now()) else {
                throw GoogleSchedulePublicationWorkflowError.invalidRecoveryJournal
            }
            operationSequence = max(operationSequence, journal.operationGeneration)
            updatePresentation(journal)
            if journal.stage != .accepted, journal.canStartFresh(at: now()) {
                presentExpired(journal)
                return
            }
            let current = try transportProvider()
            guard current.configurationIdentifier == journal.configurationIdentifier else {
                throw GoogleSchedulePublicationWorkflowError.configurationChanged
            }
            switch journal.stage {
            case .previewed:
                previewForDisplay(journal.preview)
                status = .awaitingApproval(expiresAt: journal.preview!.expiresAt)
            case .approvalAttempted:
                previewForDisplay(nil)
                status = .recoveryRequired(Self.uncertainApprovalMessage)
            case .accepted:
                previewForDisplay(nil)
                if let deliveryStatus = journal.deliveryStatus {
                    status = deliveryStatus.state.isTerminal
                        ? .completed(deliveryStatus)
                        : .active(deliveryStatus)
                } else {
                    status = .recoveryRequired(
                        "Refresh the accepted Google Calendar publication status."
                    )
                }
            case .intent:
                status = .recoveryRequired("Retry the exact saved schedule preview request.")
            case .approved:
                previewForDisplay(nil)
                status = .approvedReplayRequired
            }
        } catch {
            preview = nil
            deliveryStatus = nil
            presentedJournal = nil
            hasPendingRecovery = true
            hasSavedPublication = true
            status = .failed(safeErrorMessage(
                error,
                secrets: [],
                fallback: "The encrypted schedule publication recovery could not be loaded safely."
            ))
        }
    }

    private func updatePresentation(_ journal: GoogleSchedulePublicationRecoveryJournal) {
        presentedJournal = journal
        deliveryStatus = journal.deliveryStatus
        recoveryStage = journal.stage
        hasSavedPublication = true
        hasPendingRecovery = !journal.isTerminal
    }

    private func previewForDisplay(_ preview: GoogleSchedulePublicationPreview?) {
        self.preview = preview
    }

    private func clearPresentation() {
        preview = nil
        deliveryStatus = nil
        recoveryStage = nil
        presentedJournal = nil
        hasPendingRecovery = false
        hasSavedPublication = false
    }

    private func presentExpired(_ journal: GoogleSchedulePublicationRecoveryJournal) {
        updatePresentation(journal)
        previewForDisplay(nil)
        if journal.canDiscardExpired(at: now()) {
            status = .expired
        } else if let safeDiscardAt = journal.safeDiscardAt {
            status = .expirySafetyDelay(discardAfter: safeDiscardAt)
        } else {
            status = .failed("The schedule publication expiry could not be validated safely.")
        }
    }

    private func handleFailure(
        _ error: Error,
        operation: ActiveGoogleSchedulePublicationOperation?
    ) {
        if let operation,
           activeOperation?.id != operation.id
            || operation.privacyGeneration != privacyGeneration {
            return
        }
        guard privacyAvailable else {
            status = .privacyProtected
            return
        }
        do {
            guard let journal = try recoveryStore.loadGoogleSchedulePublicationRecoveryJournal()
            else {
                clearPresentation()
                status = .failed(safeErrorMessage(
                    error,
                    secrets: [],
                    fallback: "The schedule publication request failed safely."
                ))
                return
            }
            updatePresentation(journal)
            previewForDisplay(
                journal.stage == .previewed && !journal.canStartFresh(at: now())
                    ? journal.preview
                    : nil
            )
            let secrets = journal.approvalCapability.map { [$0] } ?? []
            let safe = safeErrorMessage(
                error,
                secrets: secrets,
                fallback: "The schedule publication did not finish. Its exact recovery remains saved."
            )
            if journal.stage != .accepted, journal.canStartFresh(at: now()) {
                presentExpired(journal)
            } else if journal.stage == .accepted, let delivery = journal.deliveryStatus {
                status = delivery.state.isTerminal ? .completed(delivery) : .recoveryRequired(safe)
            } else {
                status = .recoveryRequired(safe)
            }
        } catch {
            preview = nil
            deliveryStatus = nil
            recoveryStage = nil
            hasPendingRecovery = true
            hasSavedPublication = true
            presentedJournal = nil
            status = .failed(
                "The encrypted schedule publication recovery could not be read. No new request will be sent."
            )
        }
    }

    private func safeErrorMessage(
        _ error: Error,
        secrets: [String],
        fallback: String
    ) -> String {
        DayWeaveDiagnosticSanitizer.text(
            error.localizedDescription,
            secrets: secrets,
            maximumCharacters: 400
        ) ?? fallback
    }

    private func advancePrivacyGeneration() {
        privacyGeneration = privacyGeneration == UInt64(Int64.max)
            ? 1
            : privacyGeneration + 1
    }

    private static let uncertainApprovalMessage =
        "The one-time approval response may have been lost. DayWeave will not request another capability or queue the schedule; keep this recovery until its reviewed preview expires."

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}
