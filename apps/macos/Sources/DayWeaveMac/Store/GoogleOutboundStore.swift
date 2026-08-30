import Foundation

enum GoogleOutboundRecoveryStage: String, Codable, Equatable, Sendable {
    case intent
    case previewed
    case approvalAttempted = "approval_attempted"
    case approved
}

/// The complete local recovery fence for one Google publication. The approval
/// capability is a bearer secret, so production implementations of
/// `GoogleOutboundRecoveryStoring` must keep this value inside encrypted app
/// state. Reflection and string conversion deliberately reveal no fields.
struct GoogleOutboundRecoveryJournal: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumIntentLifetime: TimeInterval = 35 * 60
    static let maximumClockSkew: TimeInterval = 5 * 60

    let version: Int
    let recoveryID: UUID
    let operationGeneration: UInt64
    let configurationIdentifier: String
    let accountID: UUID
    let collectionID: UUID
    let itemID: UUID
    let expectedItemRevision: UInt64
    let operation: GoogleOutboundOperation
    let intentExpiresAt: Date
    let preview: GoogleOutboundPreview?
    let approvalAttempted: Bool
    let approvalCapability: String?
    let approvalExpiresAt: Date?
    let createdAt: Date

    init(
        recoveryID: UUID = UUID(),
        operationGeneration: UInt64,
        configurationIdentifier: String,
        accountID: UUID,
        collectionID: UUID,
        itemID: UUID,
        expectedItemRevision: UInt64,
        operation: GoogleOutboundOperation,
        intentExpiresAt: Date,
        preview: GoogleOutboundPreview? = nil,
        approvalAttempted: Bool = false,
        approvalCapability: String? = nil,
        approvalExpiresAt: Date? = nil,
        createdAt: Date
    ) throws {
        version = Self.currentVersion
        self.recoveryID = recoveryID
        self.operationGeneration = operationGeneration
        self.configurationIdentifier = configurationIdentifier
        self.accountID = accountID
        self.collectionID = collectionID
        self.itemID = itemID
        self.expectedItemRevision = expectedItemRevision
        self.operation = operation
        self.intentExpiresAt = intentExpiresAt
        self.preview = preview
        self.approvalAttempted = approvalAttempted
        self.approvalCapability = approvalCapability
        self.approvalExpiresAt = approvalExpiresAt
        self.createdAt = createdAt
        guard hasValidShape else {
            throw GoogleOutboundWorkflowError.invalidRecoveryJournal
        }
    }

    var stage: GoogleOutboundRecoveryStage {
        if approvalCapability != nil { return .approved }
        if approvalAttempted { return .approvalAttempted }
        if preview != nil { return .previewed }
        return .intent
    }

    var previewRequest: GoogleOutboundPreviewRequest {
        GoogleOutboundPreviewRequest(
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: expectedItemRevision,
            operation: operation
        )
    }

    var enqueueRequest: GoogleOutboundEnqueueRequest? {
        approvalCapability.map {
            GoogleOutboundEnqueueRequest(
                collectionID: collectionID,
                itemID: itemID,
                expectedItemRevision: expectedItemRevision,
                operation: operation,
                approvalCapability: $0
            )
        }
    }

    func recording(preview: GoogleOutboundPreview) throws -> Self {
        guard self.preview == nil,
              approvalCapability == nil,
              approvalExpiresAt == nil else {
            throw GoogleOutboundWorkflowError.invalidRecoveryTransition
        }
        return try Self(
            recoveryID: recoveryID,
            operationGeneration: operationGeneration,
            configurationIdentifier: configurationIdentifier,
            accountID: accountID,
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: expectedItemRevision,
            operation: operation,
            intentExpiresAt: intentExpiresAt,
            preview: preview,
            approvalAttempted: false,
            createdAt: createdAt
        )
    }

    func recordingApprovalAttempt() throws -> Self {
        guard preview != nil,
              !approvalAttempted,
              approvalCapability == nil,
              approvalExpiresAt == nil else {
            throw GoogleOutboundWorkflowError.invalidRecoveryTransition
        }
        return try Self(
            recoveryID: recoveryID,
            operationGeneration: operationGeneration,
            configurationIdentifier: configurationIdentifier,
            accountID: accountID,
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: expectedItemRevision,
            operation: operation,
            intentExpiresAt: intentExpiresAt,
            preview: preview,
            approvalAttempted: true,
            createdAt: createdAt
        )
    }

    func recording(approval: GoogleOutboundApproval) throws -> Self {
        guard let preview,
              approvalAttempted,
              approvalCapability == nil,
              approvalExpiresAt == nil,
              approval.previewID == preview.id else {
            throw GoogleOutboundWorkflowError.invalidRecoveryTransition
        }
        return try Self(
            recoveryID: recoveryID,
            operationGeneration: operationGeneration,
            configurationIdentifier: configurationIdentifier,
            accountID: accountID,
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: expectedItemRevision,
            operation: operation,
            intentExpiresAt: intentExpiresAt,
            preview: preview,
            approvalAttempted: true,
            approvalCapability: approval.approvalCapability,
            approvalExpiresAt: approval.expiresAt,
            createdAt: createdAt
        )
    }

    /// Expiration prevents this authority from creating new server work. An
    /// approved journal may still replay its exact tuple to learn whether the
    /// server consumed it before expiry; every earlier stage remains inert.
    /// The owner may explicitly clear this journal before starting fresh.
    func canStartFresh(at date: Date) -> Bool {
        guard Self.isFinite(date) else { return false }
        guard let expiration = authorityExpiresAt else { return false }
        return date >= expiration
    }

    /// Local expiry removes actionability, but destructive clearing waits one
    /// additional supported clock-skew window so a fast Mac cannot discard a
    /// capability that the server may still consider live.
    func canDiscardExpired(at date: Date) -> Bool {
        guard Self.isFinite(date), let safeDiscardAt else { return false }
        return date >= safeDiscardAt
    }

    var safeDiscardAt: Date? {
        authorityExpiresAt?.addingTimeInterval(Self.maximumClockSkew)
    }

    private var authorityExpiresAt: Date? {
        switch stage {
        case .intent:
            intentExpiresAt
        case .previewed:
            preview?.expiresAt
        case .approvalAttempted:
            preview?.expiresAt
        case .approved:
            approvalExpiresAt
        }
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
              itemID != Self.zeroUUID,
              expectedItemRevision > 0,
              expectedItemRevision <= UInt64(Int64.max),
              Self.isFinite(createdAt),
              Self.isFinite(intentExpiresAt),
              intentExpiresAt > createdAt,
              intentExpiresAt.timeIntervalSince(createdAt)
                <= Self.maximumIntentLifetime else {
            return false
        }

        if let preview {
            guard preview.id != Self.zeroUUID,
                  preview.accountID == accountID,
                  preview.collectionID == collectionID,
                  preview.itemID == itemID,
                  preview.itemRevision == expectedItemRevision,
                  preview.collectionRevision > 0,
                  preview.collectionRevision <= UInt64(Int64.max),
                  preview.entityKind == .calendarEvent,
                  preview.operation == operation,
                  Self.isValidPreviewHash(preview.previewHash),
                  Self.isFinite(preview.expiresAt),
                  preview.expiresAt >= createdAt.addingTimeInterval(
                      -Self.maximumClockSkew
                  ),
                  preview.expiresAt <= intentExpiresAt.addingTimeInterval(
                      Self.maximumClockSkew
                  ) else {
                return false
            }
        } else if approvalAttempted || approvalCapability != nil || approvalExpiresAt != nil {
            return false
        }

        if approvalCapability != nil, !approvalAttempted { return false }

        switch (approvalCapability, approvalExpiresAt) {
        case (nil, nil):
            return true
        case let (capability?, expiresAt?):
            guard let preview else { return false }
            return Self.isValidApprovalCapability(capability)
                && Self.isFinite(expiresAt)
                && expiresAt >= createdAt.addingTimeInterval(-Self.maximumClockSkew)
                && expiresAt <= preview.expiresAt
        default:
            return false
        }
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case recoveryID = "recovery_id"
        case operationGeneration = "operation_generation"
        case configurationIdentifier = "configuration_identifier"
        case accountID = "account_id"
        case collectionID = "collection_id"
        case itemID = "item_id"
        case expectedItemRevision = "expected_item_revision"
        case operation
        case intentExpiresAt = "intent_expires_at"
        case preview
        case approvalAttempted = "approval_attempted"
        case approvalCapability = "approval_capability"
        case approvalExpiresAt = "approval_expires_at"
        case createdAt = "created_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleJournalKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let decodedVersion = try container.decode(Int.self, forKey: .version)
        guard decodedVersion == Self.currentVersion else {
            throw googleJournalDecodingError(
                codingPath: decoder.codingPath,
                description: "The Google outbound recovery version is invalid"
            )
        }
        do {
            try self.init(
                recoveryID: container.decode(UUID.self, forKey: .recoveryID),
                operationGeneration: container.decode(
                    UInt64.self,
                    forKey: .operationGeneration
                ),
                configurationIdentifier: container.decode(
                    String.self,
                    forKey: .configurationIdentifier
                ),
                accountID: container.decode(UUID.self, forKey: .accountID),
                collectionID: container.decode(UUID.self, forKey: .collectionID),
                itemID: container.decode(UUID.self, forKey: .itemID),
                expectedItemRevision: container.decode(
                    UInt64.self,
                    forKey: .expectedItemRevision
                ),
                operation: container.decode(
                    GoogleOutboundOperation.self,
                    forKey: .operation
                ),
                intentExpiresAt: container.decode(Date.self, forKey: .intentExpiresAt),
                preview: container.decodeIfPresent(
                    GoogleOutboundPreview.self,
                    forKey: .preview
                ),
                approvalAttempted: container.decode(
                    Bool.self,
                    forKey: .approvalAttempted
                ),
                approvalCapability: container.decodeIfPresent(
                    String.self,
                    forKey: .approvalCapability
                ),
                approvalExpiresAt: container.decodeIfPresent(
                    Date.self,
                    forKey: .approvalExpiresAt
                ),
                createdAt: container.decode(Date.self, forKey: .createdAt)
            )
        } catch let error as DecodingError {
            throw error
        } catch {
            throw googleJournalDecodingError(
                codingPath: decoder.codingPath,
                description: "The Google outbound recovery journal is invalid"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard hasValidShape else {
            throw EncodingError.invalidValue(
                self,
                .init(
                    codingPath: encoder.codingPath,
                    debugDescription: "The Google outbound recovery journal is invalid"
                )
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(recoveryID, forKey: .recoveryID)
        try container.encode(operationGeneration, forKey: .operationGeneration)
        try container.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(collectionID, forKey: .collectionID)
        try container.encode(itemID, forKey: .itemID)
        try container.encode(expectedItemRevision, forKey: .expectedItemRevision)
        try container.encode(operation, forKey: .operation)
        try container.encode(intentExpiresAt, forKey: .intentExpiresAt)
        if let preview {
            try container.encode(preview, forKey: .preview)
        } else {
            try container.encodeNil(forKey: .preview)
        }
        try container.encode(approvalAttempted, forKey: .approvalAttempted)
        if let approvalCapability {
            try container.encode(approvalCapability, forKey: .approvalCapability)
        } else {
            try container.encodeNil(forKey: .approvalCapability)
        }
        if let approvalExpiresAt {
            try container.encode(approvalExpiresAt, forKey: .approvalExpiresAt)
        } else {
            try container.encodeNil(forKey: .approvalExpiresAt)
        }
        try container.encode(createdAt, forKey: .createdAt)
    }

    private static func isValidPreviewHash(_ value: String) -> Bool {
        value.utf8.count == 64
            && value.utf8.allSatisfy { byte in
                (48...57).contains(byte) || (97...102).contains(byte)
            }
    }

    private static func isValidApprovalCapability(_ value: String) -> Bool {
        let prefix = "dw_ga1_"
        guard value.hasPrefix(prefix) else { return false }
        let payload = String(value.dropFirst(prefix.count))
        guard payload.utf8.count == 43,
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

    private static func isFinite(_ date: Date) -> Bool {
        date.timeIntervalSinceReferenceDate.isFinite
    }

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}

extension GoogleOutboundRecoveryJournal: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Google outbound recovery journal (\(stage.rawValue))" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

@MainActor
protocol GoogleOutboundRecoveryStoring: AnyObject {
    func loadGoogleOutboundRecoveryJournal() throws -> GoogleOutboundRecoveryJournal?
    func saveGoogleOutboundRecoveryJournal(_ journal: GoogleOutboundRecoveryJournal) throws
    func clearGoogleOutboundRecoveryJournal(
        _ expected: GoogleOutboundRecoveryJournal
    ) throws
}

struct GoogleOutboundApprovalConfirmation: Equatable, Sendable {
    fileprivate let recoveryID: UUID
    fileprivate let operationGeneration: UInt64
    fileprivate let configurationIdentifier: String
    fileprivate let accountID: UUID
    fileprivate let previewID: UUID
    fileprivate let previewHash: String
}

struct GoogleOutboundRecoveryContext: Equatable, Sendable {
    let itemID: UUID
    let expectedItemRevision: UInt64
    let operation: GoogleOutboundOperation
    let stage: GoogleOutboundRecoveryStage
}

extension GoogleOutboundApprovalConfirmation: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Explicit Google outbound preview confirmation" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

enum GoogleOutboundWorkflowStatus: Equatable, Sendable {
    case privacyProtected
    case idle
    case previewing
    case awaitingApproval(expiresAt: Date)
    case approving
    case enqueueing
    case expirySafetyDelay(discardAfter: Date)
    case expired
    case recoveryRequired(String)
    case accepted(outboxID: UUID, replayed: Bool)
    case failed(String)

    var message: String {
        switch self {
        case .privacyProtected:
            "Google publication details are hidden while DayWeave is locked."
        case .idle:
            "Choose an exact calendar item and review its Google publication preview."
        case .previewing:
            "Preparing the exact Google Calendar change for review…"
        case let .awaitingApproval(expiresAt):
            "Review the provider change and approve it explicitly before \(expiresAt.formatted(date: .omitted, time: .shortened))."
        case .approving:
            "Creating one expiring approval for the reviewed Google Calendar change…"
        case .enqueueing:
            "Saving the approved Google Calendar change to the durable outbox…"
        case let .expirySafetyDelay(discardAfter):
            "This Mac's saved-authority expiry time passed. If the operation was already approved, its exact result can still be recovered safely. To tolerate device clock skew, this recovery can be discarded after \(discardAfter.formatted(date: .omitted, time: .shortened))."
        case .expired:
            "The saved Google publication authority expired. If it was already approved, check for an existing server acceptance. Otherwise discard it before creating a fresh preview."
        case let .recoveryRequired(message), let .failed(message):
            message
        case let .accepted(_, replayed):
            replayed
                ? "The previously accepted Google Calendar change was recovered."
                : "The Google Calendar change was accepted into the durable outbox."
        }
    }

    var isWorking: Bool {
        switch self {
        case .previewing, .approving, .enqueueing: true
        case .privacyProtected, .idle, .awaitingApproval, .expirySafetyDelay, .expired,
             .recoveryRequired, .accepted, .failed: false
        }
    }

    var isWaitingForSafeDiscard: Bool {
        if case .expirySafetyDelay = self { return true }
        return false
    }
}

extension GoogleOutboundWorkflowStatus: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { message }
    var debugDescription: String { message }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": message], displayStyle: .enum)
    }
}

enum GoogleOutboundWorkflowError: Error, Equatable, Sendable, LocalizedError {
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
    case expired
    case recoveryStillAuthorized
    case expiredRecoveryRequiresDiscard

    var errorDescription: String? {
        switch self {
        case .privacyBoundary:
            "Unlock DayWeave before reviewing or publishing to Google."
        case .operationInProgress:
            "Wait for the current Google publication operation to finish."
        case .invalidIntent:
            "The Google publication request is invalid. Nothing was sent."
        case .pendingRecovery:
            "Recover the saved Google publication operation before starting another one."
        case .invalidRecoveryJournal:
            "The encrypted Google publication recovery record is invalid. No request was sent."
        case .invalidRecoveryTransition:
            "The Google publication recovery stage changed unexpectedly."
        case .recoveryChanged:
            "The encrypted Google publication recovery record changed during the operation."
        case .configurationChanged:
            "The DayWeave API authentication changed. Restore the matching session to recover this publication."
        case .explicitApprovalRequired:
            "Approve the exact currently displayed Google publication preview."
        case .invalidPreviewResponse:
            "The server returned a Google preview for different publication data. Nothing was approved."
        case .invalidApprovalResponse:
            "The server returned an approval for a different or invalid preview. Recovery remains saved."
        case .invalidAcceptanceResponse:
            "The server returned an invalid Google outbox acceptance. Recovery remains saved."
        case .expired:
            "The saved Google publication authority expired. Generate a fresh preview."
        case .recoveryStillAuthorized:
            "The saved Google publication authority has not expired and cannot be discarded safely."
        case .expiredRecoveryRequiresDiscard:
            "Discard the exact expired Google publication recovery before creating a fresh preview."
        }
    }
}

extension GoogleOutboundWorkflowError: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { errorDescription ?? "Google outbound workflow error" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .enum)
    }
}

private struct ActiveGoogleOutboundOperation: Sendable {
    let id: UUID
    let privacyGeneration: UInt64
    let intentGeneration: UInt64
    let transport: any GoogleOutboundTransport
    let configurationIdentifier: String
}

@MainActor
final class GoogleOutboundStore: ObservableObject {
    typealias TransportProvider = () throws -> any GoogleOutboundTransport
    typealias ExpirySleeper = @Sendable (TimeInterval) async throws -> Void

    @Published private(set) var status: GoogleOutboundWorkflowStatus
    @Published private(set) var preview: GoogleOutboundPreview?
    @Published private(set) var accepted: GoogleOutboundAccepted?
    @Published private(set) var hasPendingRecovery: Bool
    @Published private(set) var recoveryContext: GoogleOutboundRecoveryContext?

    private let recoveryStore: any GoogleOutboundRecoveryStoring
    private let transportProvider: TransportProvider
    private let now: @Sendable () -> Date
    private let expirySleeper: ExpirySleeper
    private var privacyAvailable: Bool
    private var privacyGeneration: UInt64 = 1
    private var operationSequence: UInt64 = 0
    private var activeOperation: ActiveGoogleOutboundOperation?
    private var presentedJournal: GoogleOutboundRecoveryJournal?
    private var expiryTask: Task<Void, Never>?

    init(
        recoveryStore: any GoogleOutboundRecoveryStoring,
        transportProvider: @escaping TransportProvider,
        privacyAvailable: Bool = false,
        now: @escaping @Sendable () -> Date = Date.init,
        expirySleeper: @escaping ExpirySleeper = { interval in
            try await Task.sleep(for: .seconds(interval))
        }
    ) {
        self.recoveryStore = recoveryStore
        self.transportProvider = transportProvider
        self.privacyAvailable = privacyAvailable
        self.now = now
        self.expirySleeper = expirySleeper
        status = privacyAvailable ? .idle : .privacyProtected
        preview = nil
        accepted = nil
        hasPendingRecovery = false
        recoveryContext = nil
        expiryTask = nil
        refreshRecoveryPresentation()
    }

    var approvalConfirmation: GoogleOutboundApprovalConfirmation? {
        guard privacyAvailable,
              let journal = presentedJournal,
              journal.stage == .previewed,
              let preview = journal.preview,
              preview.expiresAt > now(),
              journal.isValid(now: now()) else {
            return nil
        }
        return GoogleOutboundApprovalConfirmation(
            recoveryID: journal.recoveryID,
            operationGeneration: journal.operationGeneration,
            configurationIdentifier: journal.configurationIdentifier,
            accountID: journal.accountID,
            previewID: preview.id,
            previewHash: preview.previewHash
        )
    }

    var hasApprovedRecovery: Bool {
        hasPendingRecovery && recoveryContext?.stage == .approved
    }

    func setPrivacyAvailable(_ available: Bool) {
        guard privacyAvailable != available else { return }
        cancelExpiryObservation()
        privacyAvailable = available
        advancePrivacyGeneration()
        activeOperation = nil
        preview = nil
        accepted = nil
        presentedJournal = nil
        recoveryContext = nil
        if available {
            refreshRecoveryPresentation()
        } else {
            status = .privacyProtected
        }
    }

    /// Call when authentication or the API base URL changes. In-flight results
    /// are fenced, and a journal is shown again only under its exact binding.
    func configurationDidChange() {
        cancelExpiryObservation()
        advancePrivacyGeneration()
        activeOperation = nil
        preview = nil
        accepted = nil
        presentedJournal = nil
        recoveryContext = nil
        refreshRecoveryPresentation()
    }

    /// Clears only authority that is already unusable at the server. This is
    /// the explicit escape hatch for an expired preview/capability whose old
    /// authentication binding can no longer be restored.
    @discardableResult
    func discardExpiredRecovery() -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        guard activeOperation == nil else {
            status = .failed(GoogleOutboundWorkflowError.operationInProgress.localizedDescription)
            return false
        }
        do {
            let currentDate = now()
            guard let journal = try recoveryStore.loadGoogleOutboundRecoveryJournal(),
                  journal.isValid(now: currentDate) else {
                throw GoogleOutboundWorkflowError.invalidRecoveryJournal
            }
            guard journal.canStartFresh(at: currentDate) else {
                throw GoogleOutboundWorkflowError.recoveryStillAuthorized
            }
            guard journal.canDiscardExpired(at: currentDate) else {
                presentExpiredRecovery(journal)
                return false
            }
            try recoveryStore.clearGoogleOutboundRecoveryJournal(journal)
            cancelExpiryObservation()
            hasPendingRecovery = false
            presentedJournal = nil
            recoveryContext = nil
            preview = nil
            accepted = nil
            status = .idle
            return true
        } catch {
            handleFailure(error, operation: nil)
            return false
        }
    }

    @discardableResult
    func preparePreview(
        accountID: UUID,
        collectionID: UUID,
        itemID: UUID,
        expectedItemRevision: UInt64,
        operation: GoogleOutboundOperation
    ) async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        let currentDate = now()
        let request = GoogleOutboundPreviewRequest(
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: expectedItemRevision,
            operation: operation
        )
        guard request.isValid,
              accountID != Self.zeroUUID,
              currentDate.timeIntervalSinceReferenceDate.isFinite else {
            status = .failed(GoogleOutboundWorkflowError.invalidIntent.localizedDescription)
            return false
        }

        var operationContext: ActiveGoogleOutboundOperation?
        defer {
            if let operationContext { finishOperation(operationContext.id) }
        }
        do {
            let existing = try recoveryStore.loadGoogleOutboundRecoveryJournal()
            if let existing {
                guard existing.isValid(now: currentDate) else {
                    throw GoogleOutboundWorkflowError.invalidRecoveryJournal
                }
                if existing.canStartFresh(at: currentDate) {
                    throw GoogleOutboundWorkflowError.expiredRecoveryRequiresDiscard
                }
                throw GoogleOutboundWorkflowError.pendingRecovery
            }
            let generation = try nextOperationGeneration(after: existing?.operationGeneration)
            let context = try beginOperation(intentGeneration: generation)
            operationContext = context
            status = .previewing

            let intentExpiry = currentDate.addingTimeInterval(
                GoogleOutboundRecoveryJournal.maximumIntentLifetime
            )
            let journal = try GoogleOutboundRecoveryJournal(
                operationGeneration: generation,
                configurationIdentifier: context.configurationIdentifier,
                accountID: accountID,
                collectionID: collectionID,
                itemID: itemID,
                expectedItemRevision: expectedItemRevision,
                operation: operation,
                intentExpiresAt: intentExpiry,
                createdAt: currentDate
            )
            try recoveryStore.saveGoogleOutboundRecoveryJournal(journal)
            scheduleExpiryObservation(for: journal)
            hasPendingRecovery = true
            presentedJournal = nil
            recoveryContext = Self.context(for: journal)
            preview = nil
            accepted = nil
            return try await performPreview(journal, using: context)
        } catch {
            handleFailure(error, operation: operationContext)
            return false
        }
    }

    @discardableResult
    func approveAndEnqueue(_ confirmation: GoogleOutboundApprovalConfirmation) async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        var operationContext: ActiveGoogleOutboundOperation?
        defer {
            if let operationContext { finishOperation(operationContext.id) }
        }
        do {
            let currentDate = now()
            guard let journal = try recoveryStore.loadGoogleOutboundRecoveryJournal(),
                  journal.isValid(now: currentDate) else {
                throw GoogleOutboundWorkflowError.invalidRecoveryJournal
            }
            guard journal.stage == .previewed,
                  let preview = journal.preview else {
                throw GoogleOutboundWorkflowError.explicitApprovalRequired
            }
            guard !journal.canStartFresh(at: currentDate) else {
                presentExpiredRecovery(journal)
                return false
            }
            guard confirmation == confirmationFor(journal: journal, preview: preview) else {
                throw GoogleOutboundWorkflowError.explicitApprovalRequired
            }

            let context = try beginOperation(intentGeneration: journal.operationGeneration)
            operationContext = context
            status = .approving
            try requireConfiguration(journal, operation: context)

            // Approval capability issuance is one-shot and the server retains
            // only its hash. Persist the attempted ceremony before POST so a
            // lost response can never invite a second misleading approval.
            let attempted = try journal.recordingApprovalAttempt()
            try persistTransition(from: journal, to: attempted)
            scheduleExpiryObservation(for: attempted)
            presentedJournal = attempted
            recoveryContext = Self.context(for: attempted)
            self.preview = nil
            hasPendingRecovery = true

            let approval = try await context.transport.approveGoogleOutbound(
                accountID: attempted.accountID,
                previewID: preview.id,
                expectedPreviewHash: preview.previewHash
            )
            try requireCurrent(context)
            guard approval.previewID == preview.id,
                  approval.expiresAt.timeIntervalSinceReferenceDate.isFinite,
                  approval.expiresAt >= attempted.createdAt.addingTimeInterval(
                      -GoogleOutboundRecoveryJournal.maximumClockSkew
                  ),
                  approval.expiresAt <= preview.expiresAt else {
                throw GoogleOutboundWorkflowError.invalidApprovalResponse
            }
            let approved = try attempted.recording(approval: approval)
            try persistTransition(from: attempted, to: approved)
            scheduleExpiryObservation(for: approved)
            presentedJournal = approved
            recoveryContext = Self.context(for: approved)
            self.preview = nil
            hasPendingRecovery = true
            return try await performEnqueue(approved, using: context)
        } catch {
            handleFailure(error, operation: operationContext)
            return false
        }
    }

    /// Replays only requests that were already authorized locally. A recovered
    /// preview is never approved automatically.
    @discardableResult
    func recoverPendingOperation() async -> Bool {
        guard privacyAvailable else {
            status = .privacyProtected
            return false
        }
        var operationContext: ActiveGoogleOutboundOperation?
        defer {
            if let operationContext { finishOperation(operationContext.id) }
        }
        do {
            let currentDate = now()
            guard let journal = try recoveryStore.loadGoogleOutboundRecoveryJournal() else {
                cancelExpiryObservation()
                hasPendingRecovery = false
                presentedJournal = nil
                recoveryContext = nil
                preview = nil
                status = privacyAvailable ? .idle : .privacyProtected
                return true
            }
            guard journal.isValid(now: currentDate) else {
                throw GoogleOutboundWorkflowError.invalidRecoveryJournal
            }
            let authorityExpired = journal.canStartFresh(at: currentDate)
            guard !authorityExpired || journal.stage == .approved else {
                presentExpiredRecovery(journal)
                return false
            }
            if authorityExpired {
                cancelExpiryObservation()
            } else {
                scheduleExpiryObservation(for: journal)
            }

            if journal.stage == .approvalAttempted {
                try requireCurrentRecoveryJournal(journal)
                presentedJournal = journal
                preview = nil
                hasPendingRecovery = true
                status = .recoveryRequired(Self.uncertainApprovalMessage)
                return true
            }

            let context = try beginOperation(intentGeneration: journal.operationGeneration)
            operationContext = context
            try requireConfiguration(journal, operation: context)
            switch journal.stage {
            case .intent:
                status = .previewing
                return try await performPreview(journal, using: context)
            case .previewed:
                try requireCurrent(context)
                presentedJournal = journal
                preview = journal.preview
                hasPendingRecovery = true
                if let expiry = journal.preview?.expiresAt {
                    status = .awaitingApproval(expiresAt: expiry)
                    return true
                }
                throw GoogleOutboundWorkflowError.invalidRecoveryJournal
            case .approvalAttempted:
                throw GoogleOutboundWorkflowError.invalidRecoveryTransition
            case .approved:
                status = .enqueueing
                return try await performEnqueue(journal, using: context)
            }
        } catch {
            handleFailure(error, operation: operationContext)
            return false
        }
    }

    private func performPreview(
        _ journal: GoogleOutboundRecoveryJournal,
        using operation: ActiveGoogleOutboundOperation
    ) async throws -> Bool {
        let response = try await operation.transport.previewGoogleOutbound(
            accountID: journal.accountID,
            request: journal.previewRequest
        )
        try requireCurrent(operation)
        guard response.accountID == journal.accountID,
              response.collectionID == journal.collectionID,
              response.itemID == journal.itemID,
              response.itemRevision == journal.expectedItemRevision,
              response.collectionRevision > 0,
              response.collectionRevision <= UInt64(Int64.max),
              response.entityKind == .calendarEvent,
              response.operation == journal.operation,
              response.expiresAt.timeIntervalSinceReferenceDate.isFinite,
              response.expiresAt >= journal.createdAt.addingTimeInterval(
                  -GoogleOutboundRecoveryJournal.maximumClockSkew
              ),
              response.expiresAt <= journal.intentExpiresAt.addingTimeInterval(
                  GoogleOutboundRecoveryJournal.maximumClockSkew
              ) else {
            throw GoogleOutboundWorkflowError.invalidPreviewResponse
        }
        let previewed = try journal.recording(preview: response)
        try persistTransition(from: journal, to: previewed)
        scheduleExpiryObservation(for: previewed)
        presentedJournal = previewed
        recoveryContext = Self.context(for: previewed)
        preview = response
        accepted = nil
        hasPendingRecovery = true
        if previewed.canStartFresh(at: now()) {
            presentExpiredRecovery(previewed)
            return false
        }
        status = .awaitingApproval(expiresAt: response.expiresAt)
        return true
    }

    private func performEnqueue(
        _ journal: GoogleOutboundRecoveryJournal,
        using operation: ActiveGoogleOutboundOperation
    ) async throws -> Bool {
        guard journal.stage == .approved,
              let request = journal.enqueueRequest,
              request.isValid else {
            throw GoogleOutboundWorkflowError.invalidRecoveryJournal
        }
        status = .enqueueing
        let response = try await operation.transport.enqueueGoogleOutbound(
            accountID: journal.accountID,
            request: request
        )
        try requireCurrent(operation)
        guard response.outboxID != Self.zeroUUID else {
            throw GoogleOutboundWorkflowError.invalidAcceptanceResponse
        }
        guard try recoveryStore.loadGoogleOutboundRecoveryJournal() == journal else {
            throw GoogleOutboundWorkflowError.recoveryChanged
        }
        // Acceptance is the only non-destructive path that clears recovery.
        // At authoritative server expiry, the exact request can only recover
        // an outbox already created; an unconsumed capability remains closed.
        try recoveryStore.clearGoogleOutboundRecoveryJournal(journal)
        cancelExpiryObservation()
        hasPendingRecovery = false
        presentedJournal = nil
        recoveryContext = nil
        preview = nil
        accepted = response
        status = .accepted(outboxID: response.outboxID, replayed: response.replayed)
        return true
    }

    private func persistTransition(
        from existing: GoogleOutboundRecoveryJournal,
        to replacement: GoogleOutboundRecoveryJournal
    ) throws {
        guard replacement.isValid(now: now()),
              try recoveryStore.loadGoogleOutboundRecoveryJournal() == existing else {
            throw GoogleOutboundWorkflowError.recoveryChanged
        }
        try recoveryStore.saveGoogleOutboundRecoveryJournal(replacement)
    }

    private func confirmationFor(
        journal: GoogleOutboundRecoveryJournal,
        preview: GoogleOutboundPreview
    ) -> GoogleOutboundApprovalConfirmation {
        GoogleOutboundApprovalConfirmation(
            recoveryID: journal.recoveryID,
            operationGeneration: journal.operationGeneration,
            configurationIdentifier: journal.configurationIdentifier,
            accountID: journal.accountID,
            previewID: preview.id,
            previewHash: preview.previewHash
        )
    }

    private func beginOperation(
        intentGeneration: UInt64
    ) throws -> ActiveGoogleOutboundOperation {
        guard privacyAvailable else { throw GoogleOutboundWorkflowError.privacyBoundary }
        guard activeOperation == nil else {
            throw GoogleOutboundWorkflowError.operationInProgress
        }
        let transport = try transportProvider()
        guard GoogleDisconnectRetryJournal.isValidConfigurationIdentifier(
            transport.configurationIdentifier
        ) else {
            throw GoogleOutboundWorkflowError.configurationChanged
        }
        let operation = ActiveGoogleOutboundOperation(
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

    private func requireCurrent(_ operation: ActiveGoogleOutboundOperation) throws {
        guard privacyAvailable,
              !Task.isCancelled,
              activeOperation?.id == operation.id,
              activeOperation?.intentGeneration == operation.intentGeneration,
              operation.privacyGeneration == privacyGeneration else {
            throw CancellationError()
        }
        let current = try transportProvider()
        guard current.configurationIdentifier == operation.configurationIdentifier else {
            throw GoogleOutboundWorkflowError.configurationChanged
        }
    }

    private func requireConfiguration(
        _ journal: GoogleOutboundRecoveryJournal,
        operation: ActiveGoogleOutboundOperation
    ) throws {
        guard journal.operationGeneration == operation.intentGeneration,
              journal.configurationIdentifier == operation.configurationIdentifier else {
            throw GoogleOutboundWorkflowError.configurationChanged
        }
    }

    private func nextOperationGeneration(after previous: UInt64?) throws -> UInt64 {
        let floor = max(operationSequence, previous ?? 0)
        guard floor < UInt64(Int64.max) else {
            throw GoogleOutboundWorkflowError.invalidRecoveryJournal
        }
        operationSequence = floor + 1
        return operationSequence
    }

    private func advancePrivacyGeneration() {
        privacyGeneration = privacyGeneration == UInt64(Int64.max)
            ? 1
            : privacyGeneration + 1
    }

    private func refreshRecoveryPresentation() {
        guard privacyAvailable else {
            status = .privacyProtected
            return
        }
        do {
            guard let journal = try recoveryStore.loadGoogleOutboundRecoveryJournal() else {
                cancelExpiryObservation()
                status = .idle
                hasPendingRecovery = false
                recoveryContext = nil
                return
            }
            hasPendingRecovery = true
            guard journal.isValid(now: now()) else {
                throw GoogleOutboundWorkflowError.invalidRecoveryJournal
            }
            recoveryContext = Self.context(for: journal)
            operationSequence = max(operationSequence, journal.operationGeneration)
            if journal.canStartFresh(at: now()) {
                presentExpiredRecovery(journal)
                return
            }
            scheduleExpiryObservation(for: journal)
            presentedJournal = journal
            if journal.stage == .approvalAttempted {
                preview = nil
                status = .recoveryRequired(Self.uncertainApprovalMessage)
                return
            }
            let current = try transportProvider()
            guard current.configurationIdentifier == journal.configurationIdentifier else {
                throw GoogleOutboundWorkflowError.configurationChanged
            }
            if journal.stage == .previewed, let recoveredPreview = journal.preview {
                preview = recoveredPreview
                status = .awaitingApproval(expiresAt: recoveredPreview.expiresAt)
            } else {
                preview = nil
                let recoveryMessage = switch journal.stage {
                case .intent:
                    "Retry the exact saved Google publication preview request."
                case .approvalAttempted:
                    Self.uncertainApprovalMessage
                case .approved:
                    "Retry the exact saved Google outbox enqueue request."
                case .previewed:
                    "Review the exact saved Google publication preview."
                }
                status = .recoveryRequired(recoveryMessage)
            }
        } catch {
            preview = nil
            presentedJournal = nil
            if !hasPendingRecovery { recoveryContext = nil }
            status = .failed(safeErrorMessage(
                error,
                secrets: [],
                fallback: "The saved Google publication recovery could not be loaded safely."
            ))
        }
    }

    private static func context(
        for journal: GoogleOutboundRecoveryJournal
    ) -> GoogleOutboundRecoveryContext {
        GoogleOutboundRecoveryContext(
            itemID: journal.itemID,
            expectedItemRevision: journal.expectedItemRevision,
            operation: journal.operation,
            stage: journal.stage
        )
    }

    private func requireCurrentRecoveryJournal(
        _ expected: GoogleOutboundRecoveryJournal
    ) throws {
        guard try recoveryStore.loadGoogleOutboundRecoveryJournal() == expected else {
            throw GoogleOutboundWorkflowError.recoveryChanged
        }
    }

    private func handleFailure(
        _ error: Error,
        operation: ActiveGoogleOutboundOperation?
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
        let journal: GoogleOutboundRecoveryJournal?
        do {
            journal = try recoveryStore.loadGoogleOutboundRecoveryJournal()
        } catch {
            hasPendingRecovery = true
            preview = nil
            presentedJournal = nil
            status = .failed(
                "The encrypted Google publication recovery could not be read. No new request will be sent."
            )
            return
        }
        hasPendingRecovery = journal != nil
        presentedJournal = journal
        recoveryContext = journal.map(Self.context(for:))
        if let journal, !journal.canStartFresh(at: now()) {
            scheduleExpiryObservation(for: journal)
        } else {
            cancelExpiryObservation()
        }
        if let journal, journal.stage == .previewed,
           !journal.canStartFresh(at: now()) {
            preview = journal.preview
        } else {
            preview = nil
        }
        let secrets = journal?.approvalCapability.map { [$0] } ?? []
        let safe = safeErrorMessage(
            error,
            secrets: secrets,
            fallback: "The Google publication did not finish. Its exact recovery state remains saved."
        )
        if let journal, journal.canStartFresh(at: now()) {
            presentExpiredRecovery(journal)
        } else if journal != nil {
            status = .recoveryRequired(safe)
        } else {
            status = .failed(safe)
        }
    }

    private func scheduleExpiryObservation(for journal: GoogleOutboundRecoveryJournal) {
        let currentDate = now()
        let expiration = journal.canStartFresh(at: currentDate)
            ? journal.safeDiscardAt
            : journal.safeDiscardAt?.addingTimeInterval(
                -GoogleOutboundRecoveryJournal.maximumClockSkew
            )
        guard let expiration else {
            cancelExpiryObservation()
            return
        }
        cancelExpiryObservation()
        let delay = max(0, expiration.timeIntervalSince(now()))
        let recoveryID = journal.recoveryID
        let generation = journal.operationGeneration
        let sleeper = expirySleeper
        expiryTask = Task { @MainActor [weak self] in
            do {
                try await sleeper(delay)
            } catch {
                return
            }
            guard !Task.isCancelled, let self else { return }
            self.expiryTask = nil
            self.refreshExpiredRecovery(
                recoveryID: recoveryID,
                operationGeneration: generation
            )
        }
    }

    private func refreshExpiredRecovery(
        recoveryID: UUID,
        operationGeneration: UInt64
    ) {
        guard privacyAvailable else { return }
        do {
            guard let journal = try recoveryStore.loadGoogleOutboundRecoveryJournal(),
                  journal.recoveryID == recoveryID,
                  journal.operationGeneration == operationGeneration,
                  journal.isValid(now: now()) else {
                return
            }
            guard journal.canStartFresh(at: now()) else {
                scheduleExpiryObservation(for: journal)
                return
            }
            presentExpiredRecovery(journal)
        } catch {
            hasPendingRecovery = true
            preview = nil
            presentedJournal = nil
            status = .failed(
                "The encrypted Google publication recovery could not be refreshed at expiry."
            )
        }
    }

    private func presentExpiredRecovery(_ journal: GoogleOutboundRecoveryJournal) {
        hasPendingRecovery = true
        presentedJournal = journal
        recoveryContext = Self.context(for: journal)
        preview = nil
        if journal.canDiscardExpired(at: now()) {
            cancelExpiryObservation()
            status = .expired
        } else if let safeDiscardAt = journal.safeDiscardAt {
            status = .expirySafetyDelay(discardAfter: safeDiscardAt)
            scheduleExpiryObservation(for: journal)
        } else {
            cancelExpiryObservation()
            status = .failed(
                "The Google publication expiry could not be validated safely."
            )
        }
    }

    private func cancelExpiryObservation() {
        expiryTask?.cancel()
        expiryTask = nil
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

    private static let uncertainApprovalMessage =
        "The approval response may have been lost. DayWeave will not request another capability or queue this change; keep the recovery until its reviewed preview expires."

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}
