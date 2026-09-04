import CryptoKit
import Darwin
import Foundation
import Security

enum PlannerPersistenceError: Error, Equatable, Sendable {
    case invalidKeyLength(actualBytes: Int)
    case keychainReadFailed(status: OSStatus)
    case keychainWriteFailed(status: OSStatus)
    case keychainReturnedInvalidData
    case storageLocationUnavailable
    case directoryPreparationFailed(cocoaCode: Int?)
    case fileReadFailed(cocoaCode: Int?)
    case fileWriteFailed(cocoaCode: Int?)
    case snapshotTooLarge(limitBytes: Int)
    case snapshotEncodingFailed
    case snapshotDecodingFailed
    case malformedEnvelope
    case unsupportedEnvelopeVersion(Int)
    case unsupportedCipher(String)
    case unsupportedSnapshotVersion(Int)
    case encryptionFailed
    case invalidCiphertext
    case authenticationFailed
    case lockUnavailable(errnoCode: Int32)
    case concurrentModification
}

extension PlannerPersistenceError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case let .invalidKeyLength(actualBytes):
            "The encryption key is \(actualBytes) bytes; DayWeave requires 32 bytes."
        case let .keychainReadFailed(status):
            "The device encryption key could not be read from Keychain (status \(status))."
        case let .keychainWriteFailed(status):
            "The device encryption key could not be saved to Keychain (status \(status))."
        case .keychainReturnedInvalidData:
            "Keychain returned an invalid device encryption key."
        case .storageLocationUnavailable:
            "The application support directory is unavailable."
        case let .directoryPreparationFailed(code):
            "The encrypted storage directory could not be prepared\(Self.codeSuffix(code))."
        case let .fileReadFailed(code):
            "The encrypted planner snapshot could not be read\(Self.codeSuffix(code))."
        case let .fileWriteFailed(code):
            "The encrypted planner snapshot could not be written\(Self.codeSuffix(code))."
        case let .snapshotTooLarge(limitBytes):
            "The encrypted planner snapshot exceeds the safe \(limitBytes / 1_048_576) MiB limit."
        case .snapshotEncodingFailed:
            "The planner snapshot could not be encoded."
        case .snapshotDecodingFailed:
            "The decrypted planner snapshot is invalid."
        case .malformedEnvelope:
            "The encrypted planner file has an invalid envelope."
        case let .unsupportedEnvelopeVersion(version):
            "Encrypted planner file version \(version) is not supported."
        case let .unsupportedCipher(cipher):
            "Encrypted planner cipher \"\(cipher)\" is not supported."
        case let .unsupportedSnapshotVersion(version):
            "Planner snapshot version \(version) is not supported."
        case .encryptionFailed:
            "The planner snapshot could not be encrypted."
        case .invalidCiphertext:
            "The encrypted planner payload is malformed."
        case .authenticationFailed:
            "The encrypted planner payload failed authentication."
        case let .lockUnavailable(errnoCode):
            "The encrypted planner snapshot lock is unavailable (errno \(errnoCode))."
        case .concurrentModification:
            "Another DayWeave process changed the encrypted planner snapshot. Reload before making more changes; this process will not overwrite it."
        }
    }

    private static func codeSuffix(_ code: Int?) -> String {
        code.map { " (Cocoa error \($0))" } ?? ""
    }
}

struct PlannerPersistenceRevision: Equatable, Sendable {
    static let missing = Self(digest: nil)
    fileprivate let digest: Data?
}

struct PlannerEncryptionKey: Equatable, Sendable {
    static let byteCount = 32

    fileprivate let data: Data

    init(data: Data) throws(PlannerPersistenceError) {
        guard data.count == Self.byteCount else {
            throw .invalidKeyLength(actualBytes: data.count)
        }
        self.data = data
    }

    static func random() -> Self {
        let key = SymmetricKey(size: .bits256)
        // CryptoKit's generated key is exactly 256 bits.
        return Self(validatedData: key.withUnsafeBytes { Data($0) })
    }

    private init(validatedData: Data) {
        data = validatedData
    }
}

protocol PlannerEncryptionKeyProviding: Sendable {
    func loadOrCreateKey() throws(PlannerPersistenceError) -> PlannerEncryptionKey
}

struct KeychainPlannerKeyProvider: PlannerEncryptionKeyProviding {
    let service: String
    let account: String

    init(
        service: String = "com.greengolddog.dayweave.planner-encryption",
        account: String = "device-key-v1"
    ) {
        self.service = service
        self.account = account
    }

    func loadOrCreateKey() throws(PlannerPersistenceError) -> PlannerEncryptionKey {
        if let existing = try readKey() {
            return existing
        }

        let generated = PlannerEncryptionKey.random()
        var query = identityQuery
        query[kSecValueData] = generated.data
        query[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

        let status = SecItemAdd(query as CFDictionary, nil)
        switch status {
        case errSecSuccess:
            return generated
        case errSecDuplicateItem:
            // Another app instance may have won the create race. Always use the
            // key already in Keychain so every process encrypts compatibly.
            if let existing = try readKey() {
                return existing
            }
            throw .keychainWriteFailed(status: status)
        default:
            throw .keychainWriteFailed(status: status)
        }
    }

    private var identityQuery: [CFString: Any] {
        [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecAttrSynchronizable: false,
            kSecUseDataProtectionKeychain: true,
        ]
    }

    private func readKey() throws(PlannerPersistenceError) -> PlannerEncryptionKey? {
        var query = identityQuery
        query[kSecReturnData] = true
        query[kSecMatchLimit] = kSecMatchLimitOne

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else {
                throw .keychainReturnedInvalidData
            }
            return try PlannerEncryptionKey(data: data)
        case errSecItemNotFound:
            return nil
        default:
            throw .keychainReadFailed(status: status)
        }
    }
}

private struct PlannerSnapshotSchemaProbe: Decodable {
    let schemaVersion: Int
}

enum PlannerProposalApplicationJournalValidator {
    static let maximumStoredReceipts = 100
    static let maximumAffectedItemIDsPerReceipt = 10_000

    private struct ApplyBody: Decodable {
        let expectedReviewHash: String

        private enum CodingKeys: String, CodingKey {
            case expectedReviewHash = "expected_review_hash"
        }
    }

    private struct UndoBody: Decodable {
        let expectedApplicationRevision: UInt64

        private enum CodingKeys: String, CodingKey {
            case expectedApplicationRevision = "expected_application_revision"
        }
    }

    static func isValid(_ mutation: DayWeavePendingProposalApplicationMutation) -> Bool {
        guard mutation.hasValidShape,
              mutation.expectedCommandIDs.count <= 100,
              let object = try? JSONSerialization.jsonObject(with: mutation.requestBody),
              let fields = object as? [String: Any] else {
            return false
        }
        switch mutation.operation {
        case .apply:
            guard Set(fields.keys) == ["expected_review_hash"],
                  let expectedReviewHash = mutation.expectedReviewHash,
                  expectedReviewHash.hasPrefix("sha256:"),
                  expectedReviewHash.count == 71,
                  expectedReviewHash.dropFirst(7).allSatisfy(\.isHexDigit),
                  let decoded = try? JSONDecoder().decode(
                      ApplyBody.self,
                      from: mutation.requestBody
                  ) else {
                return false
            }
            return decoded.expectedReviewHash == expectedReviewHash
        case .undo:
            guard Set(fields.keys) == ["expected_application_revision"],
                  let expectedRevision = mutation.expectedApplicationRevision,
                  let decoded = try? JSONDecoder().decode(
                      UndoBody.self,
                      from: mutation.requestBody
                  ) else {
                return false
            }
            return decoded.expectedApplicationRevision == expectedRevision
        }
    }

    static func isValid(_ receipt: DayWeaveStoredProposalApplicationReceipt) -> Bool {
        let application = receipt.application
        guard receipt.hasValidShape,
              (1...20).contains(application.proposals.count),
              application.proposals.allSatisfy({ $0.appliedRevision > 0 }),
              (1...100).contains(application.commandIDs.count),
              application.affectedItemIDs.count <= maximumAffectedItemIDsPerReceipt,
              application.appliedAt.timeIntervalSinceReferenceDate.isFinite,
              application.undoExpiresAt.timeIntervalSinceReferenceDate.isFinite,
              application.undoneAt?.timeIntervalSinceReferenceDate.isFinite != false else {
            return false
        }
        switch application.status {
        case .applied:
            return application.applicationRevision == 1
        case .undone:
            return application.applicationRevision == 2
        }
    }

    static func isValidState(
        pending: DayWeavePendingProposalApplicationMutation?,
        receipts: [DayWeaveStoredProposalApplicationReceipt]
    ) -> Bool {
        guard receipts.count <= maximumStoredReceipts,
              pending.map(isValid) ?? true,
              receipts.allSatisfy(isValid),
              Set(receipts.map(\.application.applicationID)).count == receipts.count else {
            return false
        }

        var claimedProposalIDs = Set<UUID>()
        for receipt in receipts {
            guard receipt.application.proposals.allSatisfy({
                claimedProposalIDs.insert($0.proposalID).inserted
            }) else {
                return false
            }
        }

        let storedConfigurationIdentifiers = Set(receipts.map(\.configurationIdentifier))
        guard storedConfigurationIdentifiers.count <= 1 else { return false }
        guard let pending else { return true }
        guard storedConfigurationIdentifiers.isEmpty
                || storedConfigurationIdentifiers == [pending.configurationIdentifier] else {
            return false
        }

        switch pending.operation {
        case .apply:
            return claimedProposalIDs.isDisjoint(with: pending.proposalIDs)
        case .undo:
            guard let applicationID = pending.applicationID,
                  let expectedRevision = pending.expectedApplicationRevision,
                  let receipt = receipts.first(where: {
                      $0.application.applicationID == applicationID
                  }),
                  receipt.application.status == .applied,
                  receipt.application.applicationRevision == expectedRevision,
                  receipt.application.proposals.map(\.proposalID) == pending.proposalIDs,
                  receipt.application.proposals.map(\.appliedRevision)
                    == pending.proposalRevisions,
                  receipt.application.commandIDs == pending.expectedCommandIDs else {
                return false
            }
            return true
        }
    }
}

enum PlannerCanonicalAuthoringJournalValidator {
    static let maximumMutations = 500
    static let maximumTrashEntries = 500
    static let maximumDiagnosticBytes = 2 * 1_024
    static let maximumMutationBytes = 2 * 1_048_576
    static let maximumAggregateMutationBytes = 4 * 1_048_576

    static func isValidState(
        mutations: [DayWeavePendingCanonicalAuthoringMutation],
        trash: [DayWeaveCanonicalTrashEntry],
        canonicalItems: [DayWeaveCanonicalItem],
        tombstoneRevisions: [UUID: UInt64],
        configurationIdentifier: String?
    ) -> Bool {
        guard mutations.count <= maximumMutations,
              trash.count <= maximumTrashEntries,
              Set(mutations.map(\.id)).count == mutations.count,
              Set(mutations.map(\.itemID)).count == mutations.count,
              Set(trash.map(\.id)).count == trash.count,
              Set(canonicalItems.map(\.id)).count == canonicalItems.count,
              mutations.allSatisfy(isValid),
              trash.allSatisfy({ isValid($0, tombstoneRevisions: tombstoneRevisions) }) else {
            return false
        }

        var aggregateMutationBytes = 0
        for mutation in mutations {
            guard let mutationBytes = encodedBytes(of: mutation),
                  mutationBytes <= maximumMutationBytes,
                  aggregateMutationBytes
                    <= maximumAggregateMutationBytes - mutationBytes else {
                return false
            }
            aggregateMutationBytes += mutationBytes
        }

        let activeItems = Dictionary(
            canonicalItems.map { ($0.id, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        let activeIDs = Set(activeItems.keys)
        let trashIDs = Set(trash.map(\.id))
        guard activeIDs.isDisjoint(with: trashIDs),
              mutations.allSatisfy({ mutation in
                  guard mutation.operation == .restore else { return true }
                  if trashIDs.contains(mutation.itemID) { return true }
                  // Another device may have restored the item after this
                  // journal was written. A newer active revision is durable
                  // evidence that keeps the intent recoverable until sync can
                  // either reconcile exact content or expose a conflict.
                  guard let expectedRevision = mutation.expectedRevision,
                        let active = activeItems[mutation.itemID] else { return false }
                  return active.deletedAt == nil && active.revision > expectedRevision
              }) else { return false }

        let bindings = Set(mutations.compactMap(\.configurationIdentifier))
        guard bindings.count <= 1,
              bindings.isEmpty || configurationIdentifier.map(bindings.contains) == true else {
            return false
        }
        return true
    }

    static func isValid(_ mutation: DayWeavePendingCanonicalAuthoringMutation) -> Bool {
        guard mutation.isValid,
              mutation.createdAt.timeIntervalSinceReferenceDate.isFinite,
              encodedBytes(of: mutation).map({ $0 <= maximumMutationBytes }) == true else {
            return false
        }
        if mutation.hasBeenSubmitted, mutation.configurationIdentifier == nil { return false }
        switch mutation.disposition {
        case .pending:
            return mutation.diagnostic == nil
        case .conflicted:
            guard let diagnostic = mutation.diagnostic else { return false }
            return !diagnostic.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && diagnostic.utf8.count <= maximumDiagnosticBytes
        }
    }

    private static func encodedBytes(
        of mutation: DayWeavePendingCanonicalAuthoringMutation
    ) -> Int? {
        try? JSONEncoder().encode(mutation).count
    }

    static func isValid(
        _ entry: DayWeaveCanonicalTrashEntry,
        tombstoneRevisions: [UUID: UInt64]
    ) -> Bool {
        guard entry.revision > 0,
              entry.deletedAt.timeIntervalSinceReferenceDate.isFinite,
              entry.parentID != entry.id,
              tombstoneRevisions[entry.id].map({ $0 >= entry.revision }) == true else {
            return false
        }
        guard let item = entry.lastKnownItem else { return true }
        return item.id == entry.id
            && item.revision <= entry.revision
            && item.parentID != item.id
    }
}

struct PlannerSnapshot: Codable, Equatable, Sendable {
    /// Version 2 added canonical sync state, version 3 added persistent local
    /// capture quarantine diagnostics, version 4 added the encrypted execution
    /// replay fence and immutable terminal ledger, version 5 adds explicit
    /// sensitivity to canonical items and derived schedule blocks, version 6
    /// adds durable, revision-bound sensitivity edits, version 7 adds the
    /// submitted-request and follow-up fence, version 8 adds the exact
    /// schedule-publication replay journal, and version 9 adds exact pending
    /// proposal-application/undo requests plus bounded content-free receipts,
    /// and version 10 adds canonical authoring journals, deleted-item records,
    /// and a destination-aware canonical selection. Version 11 adds the
    /// encrypted Google outbound preview/approval/enqueue recovery fence, and
    /// version 12 adds provenance for signed on-device schedule composition,
    /// version 13 adds the encrypted, timezone-bound schedule profile, and
    /// version 14 adds the exact successful schedule-publication proof, and
    /// version 15 adds occurrence-scoped moves plus the causal execution ->
    /// publication recovery watermark, version 16 replaces local execution
    /// move approvals with exact server-issued defer assessment evidence, and
    /// version 17 retains the selected defer target until that target passes
    /// while expiring only the server-issued assessment evidence, and version
    /// 18 adds bounded, typed, approval-only Codex canonical-item drafts plus
    /// immutable accepted-item journal linkage. Version 19 adds the encrypted,
    /// content-free onboarding first-item identity and canonical revision, and
    /// version 20 durably upgrades legacy Google outbound recovery journals to
    /// entity-bound version 2 records, version 21 adds the encrypted,
    /// schedule-revision-bound Google Calendar publication recovery journal,
    /// and version 22 persists inferred or server-explicit typed duration,
    /// deadline, own-effort, and blocker metadata without discarding v21 caches.
    /// Legacy prose suggestions stay advisory and cannot acquire create authority during migration.
    /// Older binaries reject the newer schema instead of rewriting fields they
    /// do not understand.
    static let currentSchemaVersion = 22

    let schemaVersion: Int
    let savedAt: Date
    let destination: SidebarDestination?
    let selectedBlockID: UUID?
    let selectedCanonicalItemID: UUID?
    let blocks: [ScheduleBlock]
    let suggestions: [PlanningSuggestion]
    /// Highest authenticated wall-clock observation used for local Codex
    /// draft retention. It prevents a later clock rollback (including across
    /// relaunch) from silently granting another review lifetime.
    let localSuggestionDateHighWater: Date?
    let assistantMessages: [AssistantMessage]
    let lastScheduleMessage: String
    let protectedFreeMinutes: Int
    /// Optional only so schemas through 12 can be decoded before migration.
    /// Every schema-13-or-later snapshot must contain a valid profile whose
    /// protected duration agrees exactly with the retained compatibility field.
    let scheduleProfile: ScheduleProfile?
    let freezeHours: Int
    let showCompleted: Bool
    let canonicalItems: [DayWeaveCanonicalItem]?
    let canonicalDeltaCursor: String?
    let canonicalTombstoneRevisions: [UUID: UInt64]?
    let completedOccurrenceIDs: Set<UUID>?
    let pendingCanonicalMutations: [PendingCanonicalMutation]?
    let pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation]?
    let recurrenceSessionOutcomes: [RecurrenceSessionOutcome]?
    let recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove]?
    let pendingExecutionDeferIntent: DayWeavePendingExecutionDeferIntent?
    let deferredExecutionPublicationSessionIDs: Set<UUID>?
    let pendingPublicationDeferredSessionIDs: Set<UUID>?
    let canonicalConfigurationIdentifier: String?
    let schedulePreviewProvenance: SchedulePreviewProvenance?
    let publishedScheduleProof: DayWeavePublishedScheduleProof?
    let onboardingFirstItemAnchor: DayWeaveOnboardingFirstItemAnchor?
    let localScheduleCompositionProvenance: LocalScheduleCompositionProvenance?
    let pendingSchedulePublication: PendingSchedulePublication?
    let pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation?
    let proposalApplicationReceipts: [DayWeaveStoredProposalApplicationReceipt]?
    let pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation]?
    let canonicalTrash: [DayWeaveCanonicalTrashEntry]?
    let googleOutboundRecoveryJournal: GoogleOutboundRecoveryJournal?
    let googleSchedulePublicationRecoveryJournal: GoogleSchedulePublicationRecoveryJournal?
    let localCaptureDiagnostics: [UUID: String]?
    let executionState: DayWeaveExecutionDurableState?

    init(
        schemaVersion: Int = Self.currentSchemaVersion,
        savedAt: Date = Date(),
        destination: SidebarDestination?,
        selectedBlockID: UUID?,
        selectedCanonicalItemID: UUID? = nil,
        blocks: [ScheduleBlock],
        suggestions: [PlanningSuggestion],
        localSuggestionDateHighWater: Date? = nil,
        assistantMessages: [AssistantMessage],
        lastScheduleMessage: String,
        protectedFreeMinutes: Int,
        scheduleProfile: ScheduleProfile? = nil,
        freezeHours: Int,
        showCompleted: Bool,
        canonicalItems: [DayWeaveCanonicalItem]? = nil,
        canonicalDeltaCursor: String? = nil,
        canonicalTombstoneRevisions: [UUID: UInt64]? = nil,
        completedOccurrenceIDs: Set<UUID>? = nil,
        pendingCanonicalMutations: [PendingCanonicalMutation]? = nil,
        pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation]? = [],
        recurrenceSessionOutcomes: [RecurrenceSessionOutcome]? = nil,
        recurrenceOccurrenceMoves: [RecurrenceOccurrenceMove]? = [],
        pendingExecutionDeferIntent: DayWeavePendingExecutionDeferIntent? = nil,
        deferredExecutionPublicationSessionIDs: Set<UUID>? = [],
        pendingPublicationDeferredSessionIDs: Set<UUID>? = [],
        canonicalConfigurationIdentifier: String? = nil,
        schedulePreviewProvenance: SchedulePreviewProvenance? = nil,
        publishedScheduleProof: DayWeavePublishedScheduleProof? = nil,
        onboardingFirstItemAnchor: DayWeaveOnboardingFirstItemAnchor? = nil,
        localScheduleCompositionProvenance: LocalScheduleCompositionProvenance? = nil,
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation? = nil,
        proposalApplicationReceipts: [DayWeaveStoredProposalApplicationReceipt]? = [],
        pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation]? = [],
        canonicalTrash: [DayWeaveCanonicalTrashEntry]? = [],
        googleOutboundRecoveryJournal: GoogleOutboundRecoveryJournal? = nil,
        googleSchedulePublicationRecoveryJournal: GoogleSchedulePublicationRecoveryJournal? = nil,
        localCaptureDiagnostics: [UUID: String]? = nil,
        executionState: DayWeaveExecutionDurableState? = .empty
    ) {
        self.schemaVersion = schemaVersion
        self.savedAt = savedAt
        self.destination = destination
        self.selectedBlockID = selectedBlockID
        self.selectedCanonicalItemID = selectedCanonicalItemID
        self.blocks = blocks
        self.suggestions = suggestions
        self.localSuggestionDateHighWater = localSuggestionDateHighWater
        self.assistantMessages = assistantMessages
        self.lastScheduleMessage = lastScheduleMessage
        self.protectedFreeMinutes = protectedFreeMinutes
        if let scheduleProfile {
            self.scheduleProfile = scheduleProfile
        } else if schemaVersion == Self.currentSchemaVersion {
            self.scheduleProfile = Self.legacyScheduleProfile(
                protectedFreeMinutes: protectedFreeMinutes,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance
            )
        } else {
            self.scheduleProfile = nil
        }
        self.freezeHours = freezeHours
        self.showCompleted = showCompleted
        self.canonicalItems = canonicalItems
        self.canonicalDeltaCursor = canonicalDeltaCursor
        self.canonicalTombstoneRevisions = canonicalTombstoneRevisions
        self.completedOccurrenceIDs = completedOccurrenceIDs
        self.pendingCanonicalMutations = pendingCanonicalMutations
        self.pendingCanonicalSensitivityMutations = pendingCanonicalSensitivityMutations
        self.recurrenceSessionOutcomes = recurrenceSessionOutcomes
        self.recurrenceOccurrenceMoves = recurrenceOccurrenceMoves
        self.pendingExecutionDeferIntent = pendingExecutionDeferIntent
        self.deferredExecutionPublicationSessionIDs = deferredExecutionPublicationSessionIDs
        self.pendingPublicationDeferredSessionIDs = pendingPublicationDeferredSessionIDs
        self.canonicalConfigurationIdentifier = canonicalConfigurationIdentifier
        self.schedulePreviewProvenance = schedulePreviewProvenance
        self.publishedScheduleProof = publishedScheduleProof
        self.onboardingFirstItemAnchor = onboardingFirstItemAnchor
        self.localScheduleCompositionProvenance = localScheduleCompositionProvenance
        self.pendingSchedulePublication = pendingSchedulePublication
        self.pendingProposalApplicationMutation = pendingProposalApplicationMutation
        self.proposalApplicationReceipts = proposalApplicationReceipts
        self.pendingCanonicalAuthoringMutations = pendingCanonicalAuthoringMutations
        self.canonicalTrash = canonicalTrash
        self.googleOutboundRecoveryJournal = googleOutboundRecoveryJournal
        self.googleSchedulePublicationRecoveryJournal = googleSchedulePublicationRecoveryJournal
        self.localCaptureDiagnostics = localCaptureDiagnostics
        self.executionState = executionState
    }

    func migratedToCurrentSchema() throws(PlannerPersistenceError) -> PlannerSnapshot {
        switch schemaVersion {
        case Self.currentSchemaVersion:
            let pendingTypedSuggestions = suggestions.filter { suggestion in
                guard suggestion.state == .pending,
                      case .canonicalItemDraft = suggestion.payload else {
                    return false
                }
                return true
            }
            let localSuggestionHighWaterIsValid: Bool
            if pendingTypedSuggestions.isEmpty {
                localSuggestionHighWaterIsValid = localSuggestionDateHighWater == nil
            } else if let highWater = localSuggestionDateHighWater,
                      highWater.timeIntervalSinceReferenceDate.isFinite {
                localSuggestionHighWaterIsValid = pendingTypedSuggestions.allSatisfy {
                    $0.createdAt <= highWater && highWater < $0.expiresAt
                }
            } else {
                localSuggestionHighWaterIsValid = false
            }
            let deferredPublicationStateIsValid = executionState.map { state in
                guard let deferredExecutionPublicationSessionIDs else { return false }
                return deferredExecutionPublicationSessionIDs.count <= 10_000
                    && deferredExecutionPublicationSessionIDs.allSatisfy { sessionID in
                        state.terminalOutcomes[sessionID]?.session.status == .deferred
                    }
            } ?? false
            guard let scheduleProfile,
                  scheduleProfile.hasValidShape,
                  scheduleProfile.protectedFreeMinutes == protectedFreeMinutes,
                  schedulePreviewProvenance?.timezoneName == nil
                    || schedulePreviewProvenance?.timezoneName == scheduleProfile.timezoneName
                    || (publishedScheduleProof.map { proof in
                        proof.hasCurrentImmutablePlanSeal
                            && proof.configurationIdentifier
                                == canonicalConfigurationIdentifier
                            && schedulePreviewProvenance.map(proof.matches) == true
                            && proof.matchesPublishedPlan(blocks)
                    } == true),
                  localScheduleCompositionProvenance?.timezoneName == nil
                    || localScheduleCompositionProvenance?.timezoneName
                        == scheduleProfile.timezoneName,
                  let executionState,
                  pendingCanonicalSensitivityMutations != nil,
                  let recurrenceOccurrenceMoves,
                  let deferredExecutionPublicationSessionIDs,
                  let pendingPublicationDeferredSessionIDs,
                  RecurrenceOccurrenceMove.collectionIsValid(
                      recurrenceOccurrenceMoves,
                      canonicalItemIDs: Set((canonicalItems ?? []).map(\.id))
                  ),
                  pendingExecutionDeferIntent?.hasValidShape != false,
                  (pendingExecutionDeferIntent.map { intent in
                      executionState.activeSession.map(intent.identity.matches) == true
                          || executionState.terminalOutcomes[intent.identity.sessionID]
                            .map { intent.identity.matches($0.session) } == true
                          || executionState.pendingCommand
                            .map { $0.identity == intent.identity } == true
                  } ?? true),
                  deferredPublicationStateIsValid,
                  pendingPublicationDeferredSessionIDs.isSubset(
                      of: deferredExecutionPublicationSessionIDs
                  ),
                  pendingSchedulePublication != nil
                    || pendingPublicationDeferredSessionIDs.isEmpty,
                  let proposalApplicationReceipts,
                  let pendingCanonicalAuthoringMutations,
                  let canonicalTrash,
                  localSuggestionHighWaterIsValid,
                  PlanningSuggestion.collectionIsValid(suggestions),
                  PlannerProposalApplicationJournalValidator.isValidState(
                      pending: pendingProposalApplicationMutation,
                      receipts: proposalApplicationReceipts
                  ),
                  PlannerCanonicalAuthoringJournalValidator.isValidState(
                      mutations: pendingCanonicalAuthoringMutations,
                      trash: canonicalTrash,
                      canonicalItems: canonicalItems ?? [],
                      tombstoneRevisions: canonicalTombstoneRevisions ?? [:],
                      configurationIdentifier: canonicalConfigurationIdentifier
                  ),
                  googleOutboundRecoveryJournal?.hasValidShape != false,
                  googleSchedulePublicationRecoveryJournal?.hasValidShape != false,
                  localScheduleCompositionProvenance?.hasValidShape != false,
                  (onboardingFirstItemAnchor.map { anchor in
                      guard anchor.hasValidShape else { return false }
                      if let revision = anchor.canonicalRevision {
                          return (canonicalItems ?? []).contains {
                              $0.id == anchor.itemID
                                  && $0.revision == revision
                                  && $0.deletedAt == nil
                          }
                      }
                      return pendingCanonicalAuthoringMutations.contains { mutation in
                          mutation.itemID == anchor.itemID
                              && mutation.operation == .create
                              && mutation.draft.map {
                                  $0.createsPlanningDemand(itemID: anchor.itemID)
                              } == true
                      }
                  } ?? true),
                  schedulePreviewProvenance == nil
                    || localScheduleCompositionProvenance == nil,
                  (localScheduleCompositionProvenance.map {
                      $0.configurationIdentifier == canonicalConfigurationIdentifier
                          && !blocks.contains {
                              $0.syncOrigin == .canonicalPreview
                                  || $0.syncOrigin == .externalPreview
                          }
                  } ?? true),
                  schedulePreviewProvenance == nil
                    || !blocks.contains(where: { $0.syncOrigin == .localComposition }),
                  localScheduleCompositionProvenance != nil
                    || !blocks.contains(where: { $0.syncOrigin == .localComposition }),
                  (publishedScheduleProof.map { proof in
                      proof.hasValidShape
                          && proof.configurationIdentifier
                              == canonicalConfigurationIdentifier
                          && schedulePreviewProvenance.map(proof.matches) == true
                          && localScheduleCompositionProvenance == nil
                          && proof.matchesPublishedPlan(blocks)
                  } ?? true) else {
                throw .snapshotDecodingFailed
            }
            return self
        case 21:
            // Canonical structural metadata was previously nested or implicit,
            // while unknown-field retention could forward-capture the complete
            // server wire shape. The schema-aware item decoder either infers a
            // zero-key legacy row or preserves a complete captured shape (and
            // rejects partial shapes); rewriting makes that decision durable.
            return try PlannerSnapshot(
                savedAt: savedAt,
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                localSuggestionDateHighWater: localSuggestionDateHighWater,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: pendingExecutionDeferIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                onboardingFirstItemAnchor: onboardingFirstItemAnchor,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                googleSchedulePublicationRecoveryJournal:
                    googleSchedulePublicationRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 20:
            // Schema 20 predates generated-schedule Google Calendar authority.
            // Ignore any injected newer field so migration cannot invent a
            // reviewed preview or bearer capability.
            return try PlannerSnapshot(
                savedAt: savedAt,
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                localSuggestionDateHighWater: localSuggestionDateHighWater,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: pendingExecutionDeferIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                onboardingFirstItemAnchor: onboardingFirstItemAnchor,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                googleSchedulePublicationRecoveryJournal: nil,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 19:
            // Journal decoding upgrades legacy calendar-only version 1 records
            // in memory. Crossing the snapshot schema boundary makes that
            // upgrade durable by rewriting the entity-bound version 2 record.
            return try PlannerSnapshot(
                savedAt: savedAt,
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                localSuggestionDateHighWater: localSuggestionDateHighWater,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: pendingExecutionDeferIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                onboardingFirstItemAnchor: onboardingFirstItemAnchor,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 18:
            // Schema 18 predates the onboarding anchor. Ignore any injected
            // value so migration cannot designate an arbitrary canonical item
            // as the user's reviewed first item.
            return try PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                localSuggestionDateHighWater: localSuggestionDateHighWater,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: pendingExecutionDeferIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                onboardingFirstItemAnchor: nil,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 17:
            // Schema 17 suggestions were prose-only. Deliberately strip any
            // injected schema-18 payload/linkage fields while preserving their
            // display state so migration can never turn legacy text into a
            // canonical create request.
            return try PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions.map(\.migratedLegacyAdvisory),
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: pendingExecutionDeferIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 16:
            // Schema 16 capped the durable user's target at 24 hours even when
            // move_start was later. Preserve the exact target and any exact
            // server evidence/approval, but make the target itself the sole
            // lifetime boundary. Evidence freshness remains independently
            // fenced by assessment.expires_at.
            let migratedIntent: DayWeavePendingExecutionDeferIntent?
            if let legacy = pendingExecutionDeferIntent {
                guard legacy.version == 6, legacy.hasPersistableShape else {
                    throw .snapshotDecodingFailed
                }
                migratedIntent = DayWeavePendingExecutionDeferIntent(
                    identity: legacy.identity,
                    focusedBlockID: legacy.focusedBlockID,
                    sourceStart: legacy.sourceStart,
                    sourceEnd: legacy.sourceEnd,
                    moveStart: legacy.moveStart,
                    approvedMoveEnd: legacy.approvedMoveEnd,
                    approvedDeadlines: legacy.approvedDeadlines,
                    deadlineConflictApproved: legacy.deadlineConflictApproved,
                    approvedFixedConflicts: legacy.approvedFixedConflicts,
                    fixedConflictApproved: legacy.fixedConflictApproved,
                    sourceOverrideApproved: legacy.sourceOverrideApproved,
                    assessment: legacy.assessment,
                    approvedAssessmentDigest: legacy.approvedAssessmentDigest,
                    createdAt: legacy.createdAt,
                    expiresAt: legacy.moveStart
                )
            } else {
                migratedIntent = nil
            }
            return try PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: migratedIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 15:
            // Schema 15's execution move envelope contained only a locally
            // interpreted risk approval. Preserve its selected target, but
            // deliberately discard every approval while upgrading. A fresh
            // paused-revision assessment is required before a new Defer can be
            // staged. An already staged command remains independently fenced by
            // its byte-for-byte execution journal.
            let legacyIntent = pendingExecutionDeferIntent
            let sourceBlock = legacyIntent.flatMap { legacy in
                blocks.first { block in
                    let identityMatches = block.sourceItemID == legacy.identity.itemID
                        && block.sourceItemRevision == legacy.identity.itemRevision
                        && block.occurrenceID == legacy.identity.occurrenceID
                        && (block.sessionIndex ?? 0) == legacy.identity.sessionIndex
                    return block.id == legacy.focusedBlockID && identityMatches
                }
            }
            let migratedIntent: DayWeavePendingExecutionDeferIntent?
            if let legacy = legacyIntent,
               legacy.hasPersistableShape,
               DayWeaveExecutionDeferTiming.isAligned(legacy.moveStart),
               let source = sourceBlock {
                migratedIntent = DayWeavePendingExecutionDeferIntent(
                    identity: legacy.identity,
                    focusedBlockID: legacy.focusedBlockID,
                    sourceStart: source.start,
                    sourceEnd: source.end,
                    moveStart: legacy.moveStart,
                    approvedMoveEnd: legacy.moveStart,
                    approvedDeadlines: [],
                    deadlineConflictApproved: false,
                    approvedFixedConflicts: [],
                    fixedConflictApproved: false,
                    sourceOverrideApproved: false,
                    assessment: nil,
                    approvedAssessmentDigest: nil,
                    createdAt: legacy.createdAt,
                    expiresAt: legacy.moveStart
                )
            } else {
                migratedIntent = nil
            }
            return try PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: recurrenceOccurrenceMoves,
                pendingExecutionDeferIntent: migratedIntent,
                deferredExecutionPublicationSessionIDs:
                    deferredExecutionPublicationSessionIDs,
                pendingPublicationDeferredSessionIDs: pendingPublicationDeferredSessionIDs,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 14:
            // Schema 14 predates occurrence moves and the causal publication
            // watermark. Ignore injected newer fields, preserve its valid
            // publication receipt, then validate the complete migrated shape
            // through the current-schema branch.
            return try PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                recurrenceOccurrenceMoves: [],
                deferredExecutionPublicationSessionIDs: [],
                pendingPublicationDeferredSessionIDs: [],
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: publishedScheduleProof,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            ).migratedToCurrentSchema()
        case 13:
            // Schema 13 predates durable publication receipts. Ignore any
            // injected newer field so a legacy cache can never gain execution
            // authority merely by being migrated.
            guard let scheduleProfile,
                  scheduleProfile.hasValidShape,
                  scheduleProfile.protectedFreeMinutes == protectedFreeMinutes,
                  schedulePreviewProvenance?.timezoneName == nil
                    || schedulePreviewProvenance?.timezoneName == scheduleProfile.timezoneName,
                  localScheduleCompositionProvenance?.timezoneName == nil
                    || localScheduleCompositionProvenance?.timezoneName
                        == scheduleProfile.timezoneName,
                  executionState != nil,
                  pendingCanonicalSensitivityMutations != nil,
                  let proposalApplicationReceipts,
                  let pendingCanonicalAuthoringMutations,
                  let canonicalTrash,
                  PlannerProposalApplicationJournalValidator.isValidState(
                      pending: pendingProposalApplicationMutation,
                      receipts: proposalApplicationReceipts
                  ),
                  PlannerCanonicalAuthoringJournalValidator.isValidState(
                      mutations: pendingCanonicalAuthoringMutations,
                      trash: canonicalTrash,
                      canonicalItems: canonicalItems ?? [],
                      tombstoneRevisions: canonicalTombstoneRevisions ?? [:],
                      configurationIdentifier: canonicalConfigurationIdentifier
                  ),
                  googleOutboundRecoveryJournal?.hasValidShape != false,
                  localScheduleCompositionProvenance?.hasValidShape != false,
                  schedulePreviewProvenance == nil
                    || localScheduleCompositionProvenance == nil,
                  (localScheduleCompositionProvenance.map {
                      $0.configurationIdentifier == canonicalConfigurationIdentifier
                          && !blocks.contains {
                              $0.syncOrigin == .canonicalPreview
                                  || $0.syncOrigin == .externalPreview
                          }
                  } ?? true),
                  schedulePreviewProvenance == nil
                    || !blocks.contains(where: { $0.syncOrigin == .localComposition }),
                  localScheduleCompositionProvenance != nil
                    || !blocks.contains(where: { $0.syncOrigin == .localComposition }) else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: scheduleProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                publishedScheduleProof: nil,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 12:
            // Schema 12 has no trusted schedule profile. Ignore any injected
            // newer field and derive the exact legacy 06:00–23:00 shape from
            // the retained protected-minute setting and proven preview zone.
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil,
                  let proposalApplicationReceipts,
                  let pendingCanonicalAuthoringMutations,
                  let canonicalTrash,
                  PlannerProposalApplicationJournalValidator.isValidState(
                      pending: pendingProposalApplicationMutation,
                      receipts: proposalApplicationReceipts
                  ),
                  PlannerCanonicalAuthoringJournalValidator.isValidState(
                      mutations: pendingCanonicalAuthoringMutations,
                      trash: canonicalTrash,
                      canonicalItems: canonicalItems ?? [],
                      tombstoneRevisions: canonicalTombstoneRevisions ?? [:],
                      configurationIdentifier: canonicalConfigurationIdentifier
                  ),
                  googleOutboundRecoveryJournal?.hasValidShape != false,
                  localScheduleCompositionProvenance?.hasValidShape != false,
                  schedulePreviewProvenance == nil
                    || localScheduleCompositionProvenance == nil,
                  (localScheduleCompositionProvenance.map {
                      $0.configurationIdentifier == canonicalConfigurationIdentifier
                          && !blocks.contains {
                              $0.syncOrigin == .canonicalPreview
                                  || $0.syncOrigin == .externalPreview
                          }
                  } ?? true),
                  schedulePreviewProvenance == nil
                    || !blocks.contains(where: { $0.syncOrigin == .localComposition }),
                  localScheduleCompositionProvenance != nil
                    || !blocks.contains(where: { $0.syncOrigin == .localComposition }),
                  let migratedProfile = Self.legacyScheduleProfile(
                      protectedFreeMinutes: protectedFreeMinutes,
                      schedulePreviewProvenance: schedulePreviewProvenance,
                      localScheduleCompositionProvenance: localScheduleCompositionProvenance
                  ),
                  schedulePreviewProvenance?.timezoneName == nil
                    || schedulePreviewProvenance?.timezoneName
                        == migratedProfile.timezoneName,
                  localScheduleCompositionProvenance?.timezoneName == nil
                    || localScheduleCompositionProvenance?.timezoneName
                        == migratedProfile.timezoneName else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                scheduleProfile: migratedProfile,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localScheduleCompositionProvenance: localScheduleCompositionProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 11:
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil,
                  let proposalApplicationReceipts,
                  let pendingCanonicalAuthoringMutations,
                  let canonicalTrash,
                  PlannerProposalApplicationJournalValidator.isValidState(
                      pending: pendingProposalApplicationMutation,
                      receipts: proposalApplicationReceipts
                  ),
                  PlannerCanonicalAuthoringJournalValidator.isValidState(
                      mutations: pendingCanonicalAuthoringMutations,
                      trash: canonicalTrash,
                      canonicalItems: canonicalItems ?? [],
                      tombstoneRevisions: canonicalTombstoneRevisions ?? [:],
                      configurationIdentifier: canonicalConfigurationIdentifier
                  ),
                  googleOutboundRecoveryJournal?.hasValidShape != false else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localScheduleCompositionProvenance: nil,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 10:
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil,
                  let proposalApplicationReceipts,
                  let pendingCanonicalAuthoringMutations,
                  let canonicalTrash,
                  PlannerProposalApplicationJournalValidator.isValidState(
                      pending: pendingProposalApplicationMutation,
                      receipts: proposalApplicationReceipts
                  ),
                  PlannerCanonicalAuthoringJournalValidator.isValidState(
                      mutations: pendingCanonicalAuthoringMutations,
                      trash: canonicalTrash,
                      canonicalItems: canonicalItems ?? [],
                      tombstoneRevisions: canonicalTombstoneRevisions ?? [:],
                      configurationIdentifier: canonicalConfigurationIdentifier
                  ) else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: selectedCanonicalItemID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
                canonicalTrash: canonicalTrash,
                googleOutboundRecoveryJournal: nil,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 9:
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil,
                  let proposalApplicationReceipts,
                  PlannerProposalApplicationJournalValidator.isValidState(
                      pending: pendingProposalApplicationMutation,
                      receipts: proposalApplicationReceipts
                  ) else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                selectedCanonicalItemID: nil,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: pendingProposalApplicationMutation,
                proposalApplicationReceipts: proposalApplicationReceipts,
                pendingCanonicalAuthoringMutations: [],
                canonicalTrash: [],
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 8:
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                pendingSchedulePublication: pendingSchedulePublication,
                pendingProposalApplicationMutation: nil,
                proposalApplicationReceipts: [],
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 7:
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil else {
                throw .snapshotDecodingFailed
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                pendingSchedulePublication: nil,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 6:
            guard executionState != nil,
                  let pendingCanonicalSensitivityMutations else {
                throw .snapshotDecodingFailed
            }
            let conservativelySubmitted = pendingCanonicalSensitivityMutations.map {
                var mutation = $0
                mutation.hasBeenSubmitted = true
                mutation.followUpIsSensitive = nil
                return mutation
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: conservativelySubmitted,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 5:
            guard executionState != nil else { throw .snapshotDecodingFailed }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                pendingCanonicalSensitivityMutations: [],
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 4:
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: executionState
            )
        case 3:
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localCaptureDiagnostics: localCaptureDiagnostics,
                executionState: .empty
            )
        case 2:
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: blocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: canonicalTombstoneRevisions,
                completedOccurrenceIDs: completedOccurrenceIDs,
                pendingCanonicalMutations: pendingCanonicalMutations,
                recurrenceSessionOutcomes: recurrenceSessionOutcomes,
                canonicalConfigurationIdentifier: canonicalConfigurationIdentifier,
                schedulePreviewProvenance: schedulePreviewProvenance,
                localCaptureDiagnostics: [:]
            )
        case 1:
            let migratedBlocks = blocks.map { block in
                var migrated = block
                if migrated.occurrenceID != nil
                    && (migrated.status == .completed || migrated.status == .skipped) {
                    migrated.status = .scheduled
                    migrated.actualMinutes = nil
                }
                return migrated
            }
            return PlannerSnapshot(
                destination: destination,
                selectedBlockID: selectedBlockID,
                blocks: migratedBlocks,
                suggestions: suggestions,
                assistantMessages: assistantMessages,
                lastScheduleMessage: completedOccurrenceIDs?.isEmpty == false
                    ? "\(lastScheduleMessage) · recurrence outcomes will be revalidated after storage upgrade"
                    : lastScheduleMessage,
                protectedFreeMinutes: protectedFreeMinutes,
                freezeHours: freezeHours,
                showCompleted: showCompleted,
                canonicalItems: canonicalItems,
                canonicalDeltaCursor: canonicalDeltaCursor,
                canonicalTombstoneRevisions: [:],
                // Schema 1 marked skips and partial split sessions as completed
                // and stored no completion timestamp. Reusing those IDs could
                // suppress valid work or advance an after-completion rule.
                completedOccurrenceIDs: [],
                pendingCanonicalMutations: [],
                recurrenceSessionOutcomes: [],
                canonicalConfigurationIdentifier: nil,
                schedulePreviewProvenance: nil,
                localCaptureDiagnostics: [:]
            )
        default:
            throw .unsupportedSnapshotVersion(schemaVersion)
        }
    }

    private static func legacyScheduleProfile(
        protectedFreeMinutes: Int,
        schedulePreviewProvenance: SchedulePreviewProvenance?,
        localScheduleCompositionProvenance: LocalScheduleCompositionProvenance?
    ) -> ScheduleProfile? {
        let current = ScheduleProfile.normalizedTimezoneName(
            TimeZone.autoupdatingCurrent.identifier
        )
        let candidates = [
            schedulePreviewProvenance?.timezoneName,
            localScheduleCompositionProvenance?.timezoneName,
            current,
            "UTC",
        ]
        guard let timezoneName = candidates.compactMap({ $0 }).first(where: {
            ScheduleProfile.isKnownIANATimezone($0)
        }) else { return nil }
        return try? ScheduleProfile.legacyDefault(
            timezoneName: timezoneName,
            protectedFreeMinutes: protectedFreeMinutes
        )
    }
}

struct EncryptedPlannerPersistence: Sendable {
    static let currentEnvelopeVersion = 1
    static let cipherName = "AES.GCM.256"

    /// Before generated-schedule publication journals, a complete planner
    /// snapshot could occupy 16 MiB. Keep that entire prior allowance while a
    /// review is recoverable; otherwise a transport-valid preview could make
    /// an already-valid planner impossible to save at the authority boundary.
    static let legacyMaximumPlaintextBytes = 16 * 1_048_576

    /// A decoded 16 MiB JSON response can require up to twice its wire size
    /// when Foundation re-encodes JSON string contents (notably `/` on older
    /// encoders and a literal U+2028/U+2029 as a six-byte escape). JSON syntax,
    /// numbers, UUIDs, and ISO dates do not exceed that factor for these strict
    /// DTOs. This remains tied to, and does not weaken, the transport cap.
    static let maximumSchedulePublicationPreviewTransportBytes =
        DayWeaveAPIClient.maximumResponseBytes
    static let maximumJSONReencodingExpansionFactor = 2
    static let maximumReencodedSchedulePublicationPreviewBytes = checkedMultiply(
        maximumSchedulePublicationPreviewTransportBytes,
        maximumJSONReencodingExpansionFactor
    )

    /// Outside the preview, the journal has one configuration identifier plus
    /// fixed keys, UUIDs, bounded dates/counts, a 51-byte capability, and at
    /// most a 100-character error code. The identifier can itself double when
    /// JSON-escaped; the remaining 8 KiB reserve is more than twice the bound
    /// of every other current field and the enclosing snapshot property.
    static let maximumSchedulePublicationJournalMetadataBytes = checkedAdd(
        checkedMultiply(
            GoogleDisconnectRetryJournal.maximumConfigurationIdentifierBytes,
            maximumJSONReencodingExpansionFactor
        ),
        8 * 1_024
    )

    static let maximumPlaintextBytes = checkedAdd(
        checkedAdd(
            legacyMaximumPlaintextBytes,
            maximumReencodedSchedulePublicationPreviewBytes
        ),
        maximumSchedulePublicationJournalMetadataBytes
    )

    /// CryptoKit's combined AES-GCM representation is the 12-byte nonce,
    /// ciphertext, and 16-byte tag. The envelope encoder explicitly leaves `/`
    /// unescaped, so its Data value is exactly standard base64 (4 * ceil(n/3))
    /// plus the deterministically derived fixed JSON framing below.
    static let aesGCMCombinedOverheadBytes = 12 + 16
    static let envelopeJSONFramingBytes = checkedAdd(
        checkedAdd(
            #"{"cipher":"","formatVersion":,"magic":"","sealedSnapshot":""}"#.utf8.count,
            cipherName.utf8.count
        ),
        checkedAdd(
            String(currentEnvelopeVersion).utf8.count,
            EncryptedEnvelope.magic.utf8.count
        )
    )
    static let maximumEnvelopeBytes = checkedAdd(
        base64EncodedByteCount(
            for: checkedAdd(maximumPlaintextBytes, aesGCMCombinedOverheadBytes)
        ),
        envelopeJSONFramingBytes
    )

    let fileURL: URL
    private let keyProvider: any PlannerEncryptionKeyProviding

    init(fileURL: URL, keyProvider: any PlannerEncryptionKeyProviding) {
        self.fileURL = fileURL
        self.keyProvider = keyProvider
    }

    init(fileURL: URL, key: PlannerEncryptionKey) {
        self.init(fileURL: fileURL, keyProvider: FixedKeyProvider(key: key))
    }

    static func applicationDefault() throws(PlannerPersistenceError) -> Self {
        guard let applicationSupport = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else {
            throw .storageLocationUnavailable
        }
        let fileURL = applicationSupport
            .appendingPathComponent("DayWeave", isDirectory: true)
            .appendingPathComponent("planner.snapshot.encrypted", isDirectory: false)
        return Self(fileURL: fileURL, keyProvider: KeychainPlannerKeyProvider())
    }

    func load() throws(PlannerPersistenceError) -> PlannerSnapshot? {
        try loadRevisioned().snapshot
    }

    func loadRevisioned() throws(PlannerPersistenceError) -> (
        snapshot: PlannerSnapshot?,
        revision: PlannerPersistenceRevision
    ) {
        try prepareParentDirectory()
        return try withExclusiveLock { () throws(PlannerPersistenceError) -> (
            PlannerSnapshot?, PlannerPersistenceRevision
        ) in
            try removeOrphanedTemporaryFiles()
            guard let envelopeData = try readEnvelopeDataIfPresent() else {
                return (nil, .missing)
            }
            let snapshot = try decodeSnapshot(from: envelopeData)
            let migrated = try snapshot.migratedToCurrentSchema()
            if snapshot.schemaVersion != PlannerSnapshot.currentSchemaVersion {
                // Migration and replacement happen under the same sibling-file
                // lock so a second process cannot be silently overwritten.
                let migratedData = try encodeEnvelope(for: migrated)
                try writeEnvelopeData(migratedData)
                return (migrated, Self.revision(for: migratedData))
            }
            return (migrated, Self.revision(for: envelopeData))
        }
    }

    private func decodeSnapshot(from envelopeData: Data) throws(PlannerPersistenceError) -> PlannerSnapshot {
        let envelope: EncryptedEnvelope
        do {
            envelope = try JSONDecoder().decode(EncryptedEnvelope.self, from: envelopeData)
        } catch {
            throw .malformedEnvelope
        }

        guard envelope.magic == EncryptedEnvelope.magic else {
            throw .malformedEnvelope
        }
        guard envelope.formatVersion == Self.currentEnvelopeVersion else {
            throw .unsupportedEnvelopeVersion(envelope.formatVersion)
        }
        guard envelope.cipher == Self.cipherName else {
            throw .unsupportedCipher(envelope.cipher)
        }

        let sealedBox: AES.GCM.SealedBox
        do {
            sealedBox = try AES.GCM.SealedBox(combined: envelope.sealedSnapshot)
        } catch {
            throw .invalidCiphertext
        }

        let key = try keyProvider.loadOrCreateKey()
        let plaintext: Data
        do {
            plaintext = try AES.GCM.open(
                sealedBox,
                using: SymmetricKey(data: key.data),
                authenticating: Self.authenticatedHeader(for: envelope.formatVersion)
            )
        } catch {
            throw .authenticationFailed
        }
        // The global maximum is needed before decoding untrusted ciphertext.
        // A second, authority-scoped check below prevents ordinary planner
        // snapshots from consuming publication-only headroom.
        guard plaintext.count <= Self.maximumPlaintextBytes else {
            throw .snapshotTooLarge(limitBytes: Self.maximumPlaintextBytes)
        }

        let snapshot: PlannerSnapshot
        do {
            let probe = try JSONDecoder().decode(PlannerSnapshotSchemaProbe.self, from: plaintext)
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .millisecondsSince1970
            // Only schemas that predate the sensitivity field may default it.
            // Schema 5 and every newer schema remain sensitivity-strict.
            if (1..<5).contains(probe.schemaVersion) {
                decoder.userInfo[.dayWeaveAllowsMissingSensitivity] = true
            }
            decoder.userInfo[.dayWeavePlannerSnapshotSchemaVersion] = probe.schemaVersion
            snapshot = try decoder.decode(PlannerSnapshot.self, from: plaintext)
        } catch {
            throw .snapshotDecodingFailed
        }
        let migrated = try snapshot.migratedToCurrentSchema()
        let contextualLimit = Self.maximumPlaintextBytes(for: migrated)
        guard plaintext.count <= contextualLimit else {
            throw .snapshotTooLarge(limitBytes: contextualLimit)
        }
        return snapshot
    }

    func save(_ snapshot: PlannerSnapshot) throws(PlannerPersistenceError) {
        _ = try save(snapshot, expectedRevision: .missing)
    }

    /// Runs the exact schema and plaintext-size checks used by `save` without
    /// loading a key, encrypting, locking, or touching durable error state.
    func preflightSave(_ snapshot: PlannerSnapshot) throws(PlannerPersistenceError) {
        _ = try Self.encodePlaintext(for: snapshot)
    }

    @discardableResult
    func save(
        _ snapshot: PlannerSnapshot,
        expectedRevision: PlannerPersistenceRevision
    ) throws(PlannerPersistenceError) -> PlannerPersistenceRevision {
        let data = try encodeEnvelope(for: snapshot)
        try prepareParentDirectory()
        return try withExclusiveLock { () throws(PlannerPersistenceError) -> PlannerPersistenceRevision in
            try removeOrphanedTemporaryFiles()
            let currentData = try readEnvelopeDataIfPresent()
            guard Self.revision(for: currentData) == expectedRevision else {
                throw PlannerPersistenceError.concurrentModification
            }
            try writeEnvelopeData(data)
            return Self.revision(for: data)
        }
    }

    private func encodeEnvelope(for snapshot: PlannerSnapshot) throws(PlannerPersistenceError) -> Data {
        let plaintext = try Self.encodePlaintext(for: snapshot)

        let key = try keyProvider.loadOrCreateKey()
        let sealedBox: AES.GCM.SealedBox
        do {
            sealedBox = try AES.GCM.seal(
                plaintext,
                using: SymmetricKey(data: key.data),
                authenticating: Self.authenticatedHeader(for: Self.currentEnvelopeVersion)
            )
        } catch {
            throw .encryptionFailed
        }
        guard let combined = sealedBox.combined else {
            throw .encryptionFailed
        }

        let envelope = EncryptedEnvelope(
            magic: EncryptedEnvelope.magic,
            formatVersion: Self.currentEnvelopeVersion,
            cipher: Self.cipherName,
            sealedSnapshot: combined
        )
        let data: Data
        do {
            let encoder = JSONEncoder()
            // Base64 includes `/`. Keeping it literal makes the envelope-size
            // formula above deterministic instead of probabilistic.
            encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
            data = try encoder.encode(envelope)
        } catch {
            throw .snapshotEncodingFailed
        }
        guard data.count <= Self.maximumEnvelopeBytes else {
            throw .snapshotTooLarge(limitBytes: Self.maximumEnvelopeBytes)
        }

        return data
    }

    private static func encodePlaintext(
        for snapshot: PlannerSnapshot
    ) throws(PlannerPersistenceError) -> Data {
        guard snapshot.schemaVersion == PlannerSnapshot.currentSchemaVersion else {
            throw .unsupportedSnapshotVersion(snapshot.schemaVersion)
        }
        guard (try? snapshot.migratedToCurrentSchema()) == snapshot else {
            throw .snapshotEncodingFailed
        }

        let plaintext: Data
        do {
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .millisecondsSince1970
            encoder.outputFormatting = [.sortedKeys]
            plaintext = try encoder.encode(snapshot)
        } catch {
            throw .snapshotEncodingFailed
        }
        let contextualLimit = maximumPlaintextBytes(for: snapshot)
        guard plaintext.count <= contextualLimit else {
            throw .snapshotTooLarge(limitBytes: contextualLimit)
        }
        return plaintext
    }

    /// Publication headroom is capability-scoped. An ordinary snapshot keeps
    /// the historical 16 MiB limit. The small intent journal receives only its
    /// bounded metadata reserve, and the 2x transport allowance is available
    /// only after a valid journal contains the decoded preview it must recover.
    static func maximumPlaintextBytes(for snapshot: PlannerSnapshot) -> Int {
        guard let journal = snapshot.googleSchedulePublicationRecoveryJournal else {
            return legacyMaximumPlaintextBytes
        }
        guard journal.preview != nil else {
            return checkedAdd(
                legacyMaximumPlaintextBytes,
                maximumSchedulePublicationJournalMetadataBytes
            )
        }
        return maximumPlaintextBytes
    }

    private static func checkedAdd(_ left: Int, _ right: Int) -> Int {
        let (value, overflow) = left.addingReportingOverflow(right)
        precondition(!overflow, "Planner persistence byte budget overflow")
        return value
    }

    private static func checkedMultiply(_ left: Int, _ right: Int) -> Int {
        let (value, overflow) = left.multipliedReportingOverflow(by: right)
        precondition(!overflow, "Planner persistence byte budget overflow")
        return value
    }

    private static func base64EncodedByteCount(for rawByteCount: Int) -> Int {
        precondition(rawByteCount >= 0, "Base64 byte budget must be nonnegative")
        return checkedMultiply(checkedAdd(rawByteCount, 2) / 3, 4)
    }

    private func writeEnvelopeData(_ data: Data) throws(PlannerPersistenceError) {
        let directoryURL = fileURL.deletingLastPathComponent()
        let directoryDescriptor = Darwin.open(
            directoryURL.path,
            O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW
        )
        guard directoryDescriptor >= 0 else {
            throw .fileWriteFailed(cocoaCode: nil)
        }
        defer { _ = Darwin.close(directoryDescriptor) }

        let temporaryURL = directoryURL.appendingPathComponent(
            ".\(fileURL.lastPathComponent).\(UUID().uuidString).tmp",
            isDirectory: false
        )
        let flags = O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW
        var descriptor = Darwin.open(
            temporaryURL.path,
            flags,
            mode_t(S_IRUSR | S_IWUSR)
        )
        guard descriptor >= 0 else {
            throw .fileWriteFailed(cocoaCode: nil)
        }
        var temporaryFileExists = true
        defer {
            if descriptor >= 0 { _ = Darwin.close(descriptor) }
            if temporaryFileExists { _ = Darwin.unlink(temporaryURL.path) }
        }

        do {
            guard Darwin.fchmod(descriptor, mode_t(S_IRUSR | S_IWUSR)) == 0 else {
                throw PlannerPersistenceError.fileWriteFailed(cocoaCode: nil)
            }
            try data.withUnsafeBytes { bytes in
                guard let baseAddress = bytes.baseAddress else { return }
                var written = 0
                while written < bytes.count {
                    let result = Darwin.write(
                        descriptor,
                        baseAddress.advanced(by: written),
                        bytes.count - written
                    )
                    if result < 0, errno == EINTR { continue }
                    guard result > 0 else {
                        throw PlannerPersistenceError.fileWriteFailed(cocoaCode: nil)
                    }
                    written += result
                }
            }
            guard Darwin.fsync(descriptor) == 0 else {
                throw PlannerPersistenceError.fileWriteFailed(cocoaCode: nil)
            }
            guard Darwin.close(descriptor) == 0 else {
                descriptor = -1
                throw PlannerPersistenceError.fileWriteFailed(cocoaCode: nil)
            }
            descriptor = -1

            // The temporary file already has its final private permissions.
            // rename(2) atomically changes the name; the directory fsync below
            // is the durability barrier before that commit may be reported.
            guard Darwin.rename(temporaryURL.path, fileURL.path) == 0 else {
                throw PlannerPersistenceError.fileWriteFailed(cocoaCode: nil)
            }
            temporaryFileExists = false
            guard Darwin.fsync(directoryDescriptor) == 0 else {
                // The rename may already be visible, so callers treat this as
                // an ambiguous persistence failure and lock until reload.
                throw PlannerPersistenceError.fileWriteFailed(cocoaCode: nil)
            }
        } catch let error as PlannerPersistenceError {
            throw error
        } catch {
            throw .fileWriteFailed(cocoaCode: Self.cocoaCode(for: error))
        }
    }

    /// Removes only regular files matching the exact random sibling name used
    /// by `writeEnvelopeData`. Cleanup runs while holding the same lock as
    /// load/save, and `lstat` ensures a matching symlink is never followed or
    /// removed. This bounds encrypted copies left by a crash before rename.
    private func removeOrphanedTemporaryFiles() throws(PlannerPersistenceError) {
        let directory = fileURL.deletingLastPathComponent()
        let prefix = ".\(fileURL.lastPathComponent)."
        let suffix = ".tmp"
        let names: [String]
        do {
            names = try FileManager.default.contentsOfDirectory(atPath: directory.path)
        } catch {
            throw .fileWriteFailed(cocoaCode: Self.cocoaCode(for: error))
        }

        for name in names where name.hasPrefix(prefix) && name.hasSuffix(suffix) {
            let identifierStart = name.index(name.startIndex, offsetBy: prefix.count)
            let identifierEnd = name.index(name.endIndex, offsetBy: -suffix.count)
            let identifierText = String(name[identifierStart..<identifierEnd])
            guard let identifier = UUID(uuidString: identifierText),
                  identifier.uuidString == identifierText else {
                continue
            }
            let orphanURL = directory.appendingPathComponent(name, isDirectory: false)
            var metadata = stat()
            let status = orphanURL.withUnsafeFileSystemRepresentation { path -> Int32 in
                guard let path else { return -1 }
                return Darwin.lstat(path, &metadata)
            }
            if status != 0 {
                if errno == ENOENT { continue }
                throw .fileWriteFailed(cocoaCode: nil)
            }
            guard metadata.st_mode & S_IFMT == S_IFREG,
                  metadata.st_uid == geteuid() else {
                continue
            }
            let unlinkStatus = orphanURL.withUnsafeFileSystemRepresentation { path -> Int32 in
                guard let path else { return -1 }
                return Darwin.unlink(path)
            }
            if unlinkStatus != 0, errno != ENOENT {
                throw .fileWriteFailed(cocoaCode: nil)
            }
        }
    }

    private func readEnvelopeDataIfPresent() throws(PlannerPersistenceError) -> Data? {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return nil }
        do {
            let attributes = try FileManager.default.attributesOfItem(atPath: fileURL.path)
            if let size = (attributes[.size] as? NSNumber)?.uint64Value,
               size > UInt64(Self.maximumEnvelopeBytes) {
                throw PlannerPersistenceError.snapshotTooLarge(
                    limitBytes: Self.maximumEnvelopeBytes
                )
            }
            let handle = try FileHandle(forReadingFrom: fileURL)
            defer { try? handle.close() }
            var bounded = Data()
            while bounded.count <= Self.maximumEnvelopeBytes {
                let remaining = Self.maximumEnvelopeBytes + 1 - bounded.count
                guard let chunk = try handle.read(upToCount: min(64 * 1_024, remaining)),
                      !chunk.isEmpty else { break }
                bounded.append(chunk)
            }
            guard bounded.count <= Self.maximumEnvelopeBytes else {
                throw PlannerPersistenceError.snapshotTooLarge(
                    limitBytes: Self.maximumEnvelopeBytes
                )
            }
            return bounded
        } catch let error as PlannerPersistenceError {
            throw error
        } catch {
            throw .fileReadFailed(cocoaCode: Self.cocoaCode(for: error))
        }
    }

    private func withExclusiveLock<T>(
        _ body: () throws(PlannerPersistenceError) -> T
    ) throws(PlannerPersistenceError) -> T {
        let lockURL = fileURL.appendingPathExtension("lock")
        let descriptor = lockURL.withUnsafeFileSystemRepresentation { path -> Int32 in
            guard let path else { return -1 }
            return Darwin.open(path, O_CREAT | O_RDWR, mode_t(S_IRUSR | S_IWUSR))
        }
        guard descriptor >= 0 else { throw .lockUnavailable(errnoCode: errno) }
        defer { Darwin.close(descriptor) }
        guard flock(descriptor, LOCK_EX) == 0 else {
            throw .lockUnavailable(errnoCode: errno)
        }
        defer { _ = flock(descriptor, LOCK_UN) }
        return try body()
    }

    private static func revision(for data: Data?) -> PlannerPersistenceRevision {
        guard let data else { return .missing }
        return PlannerPersistenceRevision(digest: Data(SHA256.hash(data: data)))
    }

    private func prepareParentDirectory() throws(PlannerPersistenceError) {
        let directory = fileURL.deletingLastPathComponent()
        var isDirectory: ObjCBool = false
        let exists = FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory)
        if exists {
            guard isDirectory.boolValue else {
                throw .directoryPreparationFailed(cocoaCode: nil)
            }
            return
        }

        do {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: NSNumber(value: Int16(0o700))]
            )
        } catch {
            throw .directoryPreparationFailed(cocoaCode: Self.cocoaCode(for: error))
        }
    }

    private static func authenticatedHeader(for version: Int) -> Data {
        Data("DayWeave.PlannerSnapshot|\(version)|\(cipherName)".utf8)
    }

    private static func cocoaCode(for error: any Error) -> Int? {
        (error as? CocoaError)?.code.rawValue
    }
}

private struct FixedKeyProvider: PlannerEncryptionKeyProviding {
    let key: PlannerEncryptionKey

    func loadOrCreateKey() throws(PlannerPersistenceError) -> PlannerEncryptionKey {
        key
    }
}

private struct EncryptedEnvelope: Codable {
    static let magic = "DAYWEAVE-ENCRYPTED-SNAPSHOT"

    let magic: String
    let formatVersion: Int
    let cipher: String
    let sealedSnapshot: Data
}
