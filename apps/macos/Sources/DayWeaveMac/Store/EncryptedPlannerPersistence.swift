import CryptoKit
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
    case snapshotEncodingFailed
    case snapshotDecodingFailed
    case malformedEnvelope
    case unsupportedEnvelopeVersion(Int)
    case unsupportedCipher(String)
    case unsupportedSnapshotVersion(Int)
    case encryptionFailed
    case invalidCiphertext
    case authenticationFailed
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
        }
    }

    private static func codeSuffix(_ code: Int?) -> String {
        code.map { " (Cocoa error \($0))" } ?? ""
    }
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

struct PlannerSnapshot: Codable, Equatable, Sendable {
    static let currentSchemaVersion = 1

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
        showCompleted: Bool
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
    }
}

struct EncryptedPlannerPersistence: Sendable {
    static let currentEnvelopeVersion = 1
    static let cipherName = "AES.GCM.256"

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
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            return nil
        }

        let envelopeData: Data
        do {
            envelopeData = try Data(contentsOf: fileURL, options: .mappedIfSafe)
        } catch {
            throw .fileReadFailed(cocoaCode: Self.cocoaCode(for: error))
        }

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

        let snapshot: PlannerSnapshot
        do {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .millisecondsSince1970
            snapshot = try decoder.decode(PlannerSnapshot.self, from: plaintext)
        } catch {
            throw .snapshotDecodingFailed
        }
        guard snapshot.schemaVersion == PlannerSnapshot.currentSchemaVersion else {
            throw .unsupportedSnapshotVersion(snapshot.schemaVersion)
        }
        return snapshot
    }

    func save(_ snapshot: PlannerSnapshot) throws(PlannerPersistenceError) {
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

        try prepareParentDirectory()
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
