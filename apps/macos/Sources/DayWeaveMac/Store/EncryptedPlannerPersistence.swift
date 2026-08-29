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

struct PlannerSnapshot: Codable, Equatable, Sendable {
    /// Version 2 added canonical sync state, version 3 added persistent local
    /// capture quarantine diagnostics, version 4 added the encrypted execution
    /// replay fence and immutable terminal ledger, version 5 adds explicit
    /// sensitivity to canonical items and derived schedule blocks, version 6
    /// adds durable, revision-bound sensitivity edits, version 7 adds the
    /// submitted-request and follow-up fence, and version 8 adds the exact
    /// schedule-publication replay journal. Older binaries reject the newer
    /// schema instead of rewriting fields they do not understand.
    static let currentSchemaVersion = 8

    let schemaVersion: Int
    let savedAt: Date
    let destination: SidebarDestination?
    let selectedBlockID: UUID?
    let blocks: [ScheduleBlock]
    let suggestions: [PlanningSuggestion]
    let assistantMessages: [AssistantMessage]
    let lastScheduleMessage: String
    let protectedFreeMinutes: Int
    let freezeHours: Int
    let showCompleted: Bool
    let canonicalItems: [DayWeaveCanonicalItem]?
    let canonicalDeltaCursor: String?
    let canonicalTombstoneRevisions: [UUID: UInt64]?
    let completedOccurrenceIDs: Set<UUID>?
    let pendingCanonicalMutations: [PendingCanonicalMutation]?
    let pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation]?
    let recurrenceSessionOutcomes: [RecurrenceSessionOutcome]?
    let canonicalConfigurationIdentifier: String?
    let schedulePreviewProvenance: SchedulePreviewProvenance?
    let pendingSchedulePublication: PendingSchedulePublication?
    let localCaptureDiagnostics: [UUID: String]?
    let executionState: DayWeaveExecutionDurableState?

    init(
        schemaVersion: Int = Self.currentSchemaVersion,
        savedAt: Date = Date(),
        destination: SidebarDestination?,
        selectedBlockID: UUID?,
        blocks: [ScheduleBlock],
        suggestions: [PlanningSuggestion],
        assistantMessages: [AssistantMessage],
        lastScheduleMessage: String,
        protectedFreeMinutes: Int,
        freezeHours: Int,
        showCompleted: Bool,
        canonicalItems: [DayWeaveCanonicalItem]? = nil,
        canonicalDeltaCursor: String? = nil,
        canonicalTombstoneRevisions: [UUID: UInt64]? = nil,
        completedOccurrenceIDs: Set<UUID>? = nil,
        pendingCanonicalMutations: [PendingCanonicalMutation]? = nil,
        pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation]? = [],
        recurrenceSessionOutcomes: [RecurrenceSessionOutcome]? = nil,
        canonicalConfigurationIdentifier: String? = nil,
        schedulePreviewProvenance: SchedulePreviewProvenance? = nil,
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        localCaptureDiagnostics: [UUID: String]? = nil,
        executionState: DayWeaveExecutionDurableState? = .empty
    ) {
        self.schemaVersion = schemaVersion
        self.savedAt = savedAt
        self.destination = destination
        self.selectedBlockID = selectedBlockID
        self.blocks = blocks
        self.suggestions = suggestions
        self.assistantMessages = assistantMessages
        self.lastScheduleMessage = lastScheduleMessage
        self.protectedFreeMinutes = protectedFreeMinutes
        self.freezeHours = freezeHours
        self.showCompleted = showCompleted
        self.canonicalItems = canonicalItems
        self.canonicalDeltaCursor = canonicalDeltaCursor
        self.canonicalTombstoneRevisions = canonicalTombstoneRevisions
        self.completedOccurrenceIDs = completedOccurrenceIDs
        self.pendingCanonicalMutations = pendingCanonicalMutations
        self.pendingCanonicalSensitivityMutations = pendingCanonicalSensitivityMutations
        self.recurrenceSessionOutcomes = recurrenceSessionOutcomes
        self.canonicalConfigurationIdentifier = canonicalConfigurationIdentifier
        self.schedulePreviewProvenance = schedulePreviewProvenance
        self.pendingSchedulePublication = pendingSchedulePublication
        self.localCaptureDiagnostics = localCaptureDiagnostics
        self.executionState = executionState
    }

    func migratedToCurrentSchema() throws(PlannerPersistenceError) -> PlannerSnapshot {
        switch schemaVersion {
        case Self.currentSchemaVersion:
            guard executionState != nil,
                  pendingCanonicalSensitivityMutations != nil else {
                throw .snapshotDecodingFailed
            }
            return self
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
}

struct EncryptedPlannerPersistence: Sendable {
    static let currentEnvelopeVersion = 1
    static let cipherName = "AES.GCM.256"
    static let maximumPlaintextBytes = 16 * 1_048_576
    static let maximumEnvelopeBytes = 24 * 1_048_576

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
            snapshot = try decoder.decode(PlannerSnapshot.self, from: plaintext)
        } catch {
            throw .snapshotDecodingFailed
        }
        return snapshot
    }

    func save(_ snapshot: PlannerSnapshot) throws(PlannerPersistenceError) {
        _ = try save(snapshot, expectedRevision: .missing)
    }

    @discardableResult
    func save(
        _ snapshot: PlannerSnapshot,
        expectedRevision: PlannerPersistenceRevision
    ) throws(PlannerPersistenceError) -> PlannerPersistenceRevision {
        let data = try encodeEnvelope(for: snapshot)
        try prepareParentDirectory()
        return try withExclusiveLock { () throws(PlannerPersistenceError) -> PlannerPersistenceRevision in
            let currentData = try readEnvelopeDataIfPresent()
            guard Self.revision(for: currentData) == expectedRevision else {
                throw PlannerPersistenceError.concurrentModification
            }
            try writeEnvelopeData(data)
            return Self.revision(for: data)
        }
    }

    private func encodeEnvelope(for snapshot: PlannerSnapshot) throws(PlannerPersistenceError) -> Data {
        guard snapshot.schemaVersion == PlannerSnapshot.currentSchemaVersion else {
            throw .unsupportedSnapshotVersion(snapshot.schemaVersion)
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
        guard plaintext.count <= Self.maximumPlaintextBytes else {
            throw .snapshotTooLarge(limitBytes: Self.maximumPlaintextBytes)
        }

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
            encoder.outputFormatting = [.sortedKeys]
            data = try encoder.encode(envelope)
        } catch {
            throw .snapshotEncodingFailed
        }
        guard data.count <= Self.maximumEnvelopeBytes else {
            throw .snapshotTooLarge(limitBytes: Self.maximumEnvelopeBytes)
        }

        return data
    }

    private func writeEnvelopeData(_ data: Data) throws(PlannerPersistenceError) {
        do {
            // Data's atomic option writes a sibling temporary file and renames it,
            // preventing a partial snapshot from replacing the last good one.
            try data.write(to: fileURL, options: .atomic)
            try FileManager.default.setAttributes(
                [.posixPermissions: NSNumber(value: Int16(0o600))],
                ofItemAtPath: fileURL.path
            )
        } catch {
            throw .fileWriteFailed(cocoaCode: Self.cocoaCode(for: error))
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
