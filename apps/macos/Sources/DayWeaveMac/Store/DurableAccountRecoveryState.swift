import CryptoKit
import Darwin
import Foundation

struct DurableAccountRecoveryCodeMetadata: Codable, Equatable, Sendable {
    let id: UUID
    let createdAt: Date
    let revision: UInt64

    private enum CodingKeys: String, CodingKey {
        case id
        case createdAt = "created_at"
        case revision
    }
}

struct DurableAccountRecoverySnapshot: Equatable, Sendable {
    let recoveryCode: DurableAccountRecoveryCodeMetadata?
    let fetchedAt: Date
    let fence: DurableDeviceSessionInventoryFence
}

enum DurableAccountRecoveryCodeSource: String, Codable, Equatable, Sendable {
    case initial
    case rotation
    case recoveredSuccessor = "recovered_successor"
}

enum DurableAccountRecoveryJournalKind: String, Codable, Equatable, Sendable {
    case issue
    case consume

    var pathComponents: [String] {
        switch self {
        case .issue: ["v1", "auth", "recovery-codes"]
        case .consume: ["v1", "auth", "recovery-codes", "consume"]
        }
    }
}

/// An immutable HTTP request written to Keychain before a recovery authority
/// is sent. Protected issuance binds to the stable device identity so one
/// trusted access-token refresh can rebase it; public consumption binds to a
/// digest of the exact entered recovery credential.
struct DurableAccountRecoveryJournaledRequest: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumBodyBytes = 32 * 1_024
    static let maximumURLBytes = 8 * 1_024
    static let securityHeaders = [
        "Accept": "application/json",
        "Cache-Control": "no-store",
        "Content-Type": "application/json",
        "Pragma": "no-cache",
    ]

    let version: Int
    let kind: DurableAccountRecoveryJournalKind
    let configurationIdentifier: String
    let url: String
    let method: String
    let headers: [String: String]
    let body: Data
    let bodySHA256: String
    let authorizationBindingIdentifier: String

    private enum CodingKeys: String, CodingKey {
        case version, kind
        case configurationIdentifier = "configuration_identifier"
        case url, method, headers, body
        case bodySHA256 = "body_sha256"
        case authorizationBindingIdentifier = "authorization_binding_identifier"
    }

    static func make(
        kind: DurableAccountRecoveryJournalKind,
        baseURL: DayWeaveAPIBaseURL,
        body: Data,
        authorizationBindingIdentifier: String
    ) throws -> Self {
        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: kind.pathComponents)
        } catch {
            throw DurableAuthError.requestEncodingFailed
        }
        let request = Self(
            version: currentVersion,
            kind: kind,
            configurationIdentifier: baseURL.canonicalConfigurationIdentifier,
            url: endpoint.absoluteString,
            method: "POST",
            headers: securityHeaders,
            body: body,
            bodySHA256: sha256(body),
            authorizationBindingIdentifier: authorizationBindingIdentifier
        )
        guard request.isValid else { throw DurableAuthError.requestEncodingFailed }
        return request
    }

    var isValid: Bool {
        guard version == Self.currentVersion,
              method == "POST",
              headers == Self.securityHeaders,
              !body.isEmpty,
              body.count <= Self.maximumBodyBytes,
              url.utf8.count <= Self.maximumURLBytes,
              bodySHA256 == Self.sha256(body),
              Self.isSafeBinding(authorizationBindingIdentifier),
              let baseURL = try? DayWeaveAPIBaseURL(configurationIdentifier),
              baseURL.canonicalConfigurationIdentifier == configurationIdentifier,
              let expected = try? baseURL.endpoint(pathComponents: kind.pathComponents),
              expected.absoluteString == url,
              let components = URLComponents(string: url),
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil else { return false }
        return true
    }

    func isBound(
        to baseURL: DayWeaveAPIBaseURL,
        authorizationBindingIdentifier: String
    ) -> Bool {
        self.authorizationBindingIdentifier == authorizationBindingIdentifier
            && configurationIdentifier == baseURL.canonicalConfigurationIdentifier
            && isValid
    }

    func makeURLRequest(bearer: String) throws -> URLRequest {
        guard isValid, let target = URL(string: url) else {
            throw DurableAuthError.incompatibleState
        }
        var request = URLRequest(url: target)
        request.httpMethod = method
        request.httpBody = body
        request.timeoutInterval = 20
        request.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        for (name, value) in headers {
            request.setValue(value, forHTTPHeaderField: name)
        }
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        return request
    }

    static func credentialBinding(_ credential: String) -> String {
        "account-recovery-v1:\(sha256(Data(credential.utf8)))"
    }

    private static func isSafeBinding(_ value: String) -> Bool {
        guard value.count <= 256, !value.isEmpty else { return false }
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789-:_")
        return value.unicodeScalars.allSatisfy(allowed.contains)
    }

    private static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}

struct DurableAccountRecoveryAuthFence: Codable, Equatable, Sendable {
    let configurationIdentifier: String
    let originIdentifier: String
    let envelopeRevision: UInt64?
    let envelopeSHA256: String?
    let clientInstanceID: UUID?
    let sessionID: UUID?

    var isValid: Bool {
        guard let baseURL = try? DayWeaveAPIBaseURL(configurationIdentifier),
              baseURL.canonicalConfigurationIdentifier == configurationIdentifier,
              baseURL.credentialOriginIdentifier == originIdentifier,
              (envelopeRevision == nil) == (envelopeSHA256 == nil),
              (clientInstanceID == nil || envelopeRevision != nil),
              (sessionID == nil || clientInstanceID != nil) else { return false }
        guard let envelopeSHA256 else { return true }
        let allowed = CharacterSet(charactersIn: "0123456789abcdef")
        return envelopeSHA256.count == 64
            && envelopeSHA256.unicodeScalars.allSatisfy(allowed.contains)
    }
}

struct DurableAccountRecoveryIssuePending: Codable, Equatable, Sendable {
    let proposedID: UUID
    let recoveryCode: String
    let replaces: DurableAccountRecoveryCodeMetadata?
    let preparedAt: Date
    let authorizationFence: DurableDeviceSessionInventoryFence
    let request: DurableAccountRecoveryJournaledRequest

    private enum CodingKeys: String, CodingKey {
        case proposedID = "proposed_id"
        case recoveryCode = "recovery_code"
        case replaces
        case preparedAt = "prepared_at"
        case authorizationFence = "authorization_fence"
        case request
    }
}

struct DurableAccountRecoveryAwaitingAcknowledgement: Codable, Equatable, Sendable {
    let metadata: DurableAccountRecoveryCodeMetadata
    let recoveryCode: String
    let source: DurableAccountRecoveryCodeSource
    let configurationIdentifier: String
    let originIdentifier: String
}

struct DurableAccountRecoveryConsumePending: Codable, Equatable, Sendable {
    let recoveryCode: String
    let proposedSessionID: UUID
    let proposedCredentials: DurableAuthCredentialPair
    let proposedClientInstanceID: UUID
    let descriptor: DurableAuthClientDescriptor
    let successorRecoveryCodeID: UUID
    let successorRecoveryCode: String
    let preparedAt: Date
    let installationFence: DurableAccountRecoveryAuthFence
    let request: DurableAccountRecoveryJournaledRequest

    private enum CodingKeys: String, CodingKey {
        case recoveryCode = "recovery_code"
        case proposedSessionID = "proposed_session_id"
        case proposedCredentials = "proposed_credentials"
        case proposedClientInstanceID = "proposed_client_instance_id"
        case descriptor
        case successorRecoveryCodeID = "successor_recovery_code_id"
        case successorRecoveryCode = "successor_recovery_code"
        case preparedAt = "prepared_at"
        case installationFence = "installation_fence"
        case request
    }
}

struct DurableAccountRecoveryConsumeCommitted: Codable, Equatable, Sendable {
    let pending: DurableAccountRecoveryConsumePending
    let session: DurableDeviceSessionMetadata
    let successor: DurableAccountRecoveryCodeMetadata
    let replayed: Bool
}

enum DurableAccountRecoveryState: Codable, Equatable, Sendable {
    case issuePending(DurableAccountRecoveryIssuePending)
    case awaitingAcknowledgement(DurableAccountRecoveryAwaitingAcknowledgement)
    case consumePending(DurableAccountRecoveryConsumePending)
    case consumeCommittedAwaitingInstallation(DurableAccountRecoveryConsumeCommitted)
    case consumeInstalledAwaitingHandoff(DurableAccountRecoveryConsumeCommitted)
    case incompatible(reasonCode: String, storedStateSHA256: String)

    private enum CodingKeys: String, CodingKey { case kind, payload }
    private enum Kind: String, Codable {
        case issuePending = "issue_pending"
        case awaitingAcknowledgement = "awaiting_acknowledgement"
        case consumePending = "consume_pending"
        case consumeCommittedAwaitingInstallation = "consume_committed_awaiting_installation"
        case consumeInstalledAwaitingHandoff = "consume_installed_awaiting_handoff"
        case incompatible
    }
    private struct IncompatiblePayload: Codable, Equatable {
        let reasonCode: String
        let storedStateSHA256: String
        private enum CodingKeys: String, CodingKey {
            case reasonCode = "reason_code"
            case storedStateSHA256 = "stored_state_sha256"
        }
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .issuePending:
            self = .issuePending(try container.decode(
                DurableAccountRecoveryIssuePending.self,
                forKey: .payload
            ))
        case .awaitingAcknowledgement:
            self = .awaitingAcknowledgement(try container.decode(
                DurableAccountRecoveryAwaitingAcknowledgement.self,
                forKey: .payload
            ))
        case .consumePending:
            self = .consumePending(try container.decode(
                DurableAccountRecoveryConsumePending.self,
                forKey: .payload
            ))
        case .consumeCommittedAwaitingInstallation:
            self = .consumeCommittedAwaitingInstallation(try container.decode(
                DurableAccountRecoveryConsumeCommitted.self,
                forKey: .payload
            ))
        case .consumeInstalledAwaitingHandoff:
            self = .consumeInstalledAwaitingHandoff(try container.decode(
                DurableAccountRecoveryConsumeCommitted.self,
                forKey: .payload
            ))
        case .incompatible:
            let value = try container.decode(IncompatiblePayload.self, forKey: .payload)
            self = .incompatible(
                reasonCode: value.reasonCode,
                storedStateSHA256: value.storedStateSHA256
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .issuePending(value):
            try container.encode(Kind.issuePending, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .awaitingAcknowledgement(value):
            try container.encode(Kind.awaitingAcknowledgement, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .consumePending(value):
            try container.encode(Kind.consumePending, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .consumeCommittedAwaitingInstallation(value):
            try container.encode(Kind.consumeCommittedAwaitingInstallation, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .consumeInstalledAwaitingHandoff(value):
            try container.encode(Kind.consumeInstalledAwaitingHandoff, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .incompatible(reasonCode, storedStateSHA256):
            try container.encode(Kind.incompatible, forKey: .kind)
            try container.encode(IncompatiblePayload(
                reasonCode: reasonCode,
                storedStateSHA256: storedStateSHA256
            ), forKey: .payload)
        }
    }
}

struct DurableAccountRecoveryEnvelope: Codable, Equatable, Sendable {
    static let currentSchemaVersion = 1
    let schemaVersion: Int
    let revision: UInt64
    let state: DurableAccountRecoveryState

    init(
        revision: UInt64,
        state: DurableAccountRecoveryState,
        schemaVersion: Int = Self.currentSchemaVersion
    ) {
        self.schemaVersion = schemaVersion
        self.revision = revision
        self.state = state
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case revision, state
    }

    func replacingState(_ state: DurableAccountRecoveryState) throws -> Self {
        guard revision < UInt64.max else { throw DurableAuthStateStoreError.revisionOverflow }
        return .init(revision: revision + 1, state: state)
    }
}

extension DurableAccountRecoveryJournaledRequest: RedactedAuthDescribing {}
extension DurableAccountRecoveryIssuePending: RedactedAuthDescribing {}
extension DurableAccountRecoveryAwaitingAcknowledgement: RedactedAuthDescribing {}
extension DurableAccountRecoveryConsumePending: RedactedAuthDescribing {}
extension DurableAccountRecoveryConsumeCommitted: RedactedAuthDescribing {}
extension DurableAccountRecoveryState: RedactedAuthDescribing {}
extension DurableAccountRecoveryEnvelope: RedactedAuthDescribing {}
extension AccountRecoveryIssueRequest: RedactedAuthDescribing {}
extension AccountRecoveryConsumeRequest: RedactedAuthDescribing {}

protocol DurableAccountRecoveryStateStoring: Sendable {
    func loadEnvelope() throws -> DurableAccountRecoveryEnvelope?
    func compareAndSwap(
        expected: DurableAccountRecoveryEnvelope?,
        replacement: DurableAccountRecoveryEnvelope?
    ) throws -> Bool
    func discardIncompatibleEnvelope(
        expected: DurableAccountRecoveryEnvelope
    ) throws -> Bool
}

/// One local transaction boundary shared by the ordinary authentication and
/// recovery Keychain journals. Store-specific locks still protect each item;
/// this outer gate makes the cross-store policy check plus one CAS a single
/// decision in every DayWeave process without ever spanning a network await.
protocol DurableAuthRecoveryTransactionGating: Sendable {
    func withTransaction(_ operation: () throws -> Void) throws
}

final class FileDurableAuthRecoveryTransactionGate:
    DurableAuthRecoveryTransactionGating, @unchecked Sendable
{
    static let shared = FileDurableAuthRecoveryTransactionGate()

    private static let processLock = NSLock()
    private let interprocessLockURL: URL?

    init(
        interprocessLockURL: URL? = FileDurableAuthRecoveryTransactionGate
            .defaultInterprocessLockURL
    ) {
        self.interprocessLockURL = interprocessLockURL
    }

    func withTransaction(_ operation: () throws -> Void) throws {
        try Self.processLock.withLock {
            guard let interprocessLockURL else {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            do {
                try FileManager.default.createDirectory(
                    at: interprocessLockURL.deletingLastPathComponent(),
                    withIntermediateDirectories: true,
                    attributes: [.posixPermissions: 0o700]
                )
            } catch {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            let descriptor = Darwin.open(
                interprocessLockURL.path,
                O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW,
                S_IRUSR | S_IWUSR
            )
            guard descriptor >= 0 else {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            defer { Darwin.close(descriptor) }
            guard flock(descriptor, LOCK_EX) == 0 else {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            defer { _ = flock(descriptor, LOCK_UN) }
            try operation()
        }
    }

    private static var defaultInterprocessLockURL: URL? {
        guard let root = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else { return nil }
        let identity = Data(
            "\(KeychainDurableAuthStateStore.defaultService)\u{0}auth-recovery-transaction-v1"
                .utf8
        )
        let digest = SHA256.hash(data: identity).prefix(16)
            .map { String(format: "%02x", $0) }.joined()
        return root.appendingPathComponent("DayWeave", isDirectory: true)
            .appendingPathComponent("AuthLocks", isDirectory: true)
            .appendingPathComponent("\(digest).lock")
    }
}

extension DurableAccountRecoveryStateStoring {
    func discardIncompatibleEnvelope(
        expected: DurableAccountRecoveryEnvelope
    ) throws -> Bool {
        guard case .incompatible = expected.state else { return false }
        return try compareAndSwap(expected: expected, replacement: nil)
    }
}

final class KeychainDurableAccountRecoveryStateStore:
    DurableAccountRecoveryStateStoring, @unchecked Sendable
{
    static let defaultService = KeychainDurableAuthStateStore.defaultService
    static let defaultAccount = "durable-account-recovery-journal-v1"
    static let maximumEnvelopeBytes = 128 * 1_024

    private static let mutationLock = NSLock()
    private let service: String
    private let account: String
    private let keychain: any KeychainSecretAccessing
    private let interprocessLockURL: URL?

    init(
        service: String = KeychainDurableAccountRecoveryStateStore.defaultService,
        account: String = KeychainDurableAccountRecoveryStateStore.defaultAccount,
        keychain: any KeychainSecretAccessing = SystemKeychainSecretAccess(),
        interprocessLockURL: URL? = KeychainDurableAccountRecoveryStateStore.defaultInterprocessLockURL
    ) {
        self.service = service
        self.account = account
        self.keychain = keychain
        self.interprocessLockURL = interprocessLockURL
    }

    func loadEnvelope() throws -> DurableAccountRecoveryEnvelope? {
        try withMutationLock { try loadEnvelopeLocked() }
    }

    func compareAndSwap(
        expected: DurableAccountRecoveryEnvelope?,
        replacement: DurableAccountRecoveryEnvelope?
    ) throws -> Bool {
        try withMutationLock {
            let current = try loadEnvelopeLocked()
            guard current == expected else { return false }
            if case .incompatible = current?.state {
                throw DurableAuthStateStoreError.invalidStoredState
            }
            if let replacement {
                let expectedRevision = expected.map { $0.revision + 1 } ?? 0
                guard replacement.revision == expectedRevision,
                      Self.isStructurallyValid(replacement) else {
                    throw DurableAuthStateStoreError.invalidStoredState
                }
                let data = try Self.encode(replacement)
                try keychain.save(data, service: service, account: account)
                guard try keychain.read(service: service, account: account) == data else {
                    throw DurableAuthStateStoreError.writeVerificationFailed
                }
            } else {
                try keychain.delete(service: service, account: account)
                guard try keychain.read(service: service, account: account) == nil else {
                    throw DurableAuthStateStoreError.writeVerificationFailed
                }
            }
            return true
        }
    }

    func discardIncompatibleEnvelope(
        expected: DurableAccountRecoveryEnvelope
    ) throws -> Bool {
        try withMutationLock {
            let current = try loadEnvelopeLocked()
            guard current == expected,
                  case .incompatible = expected.state else { return false }
            try keychain.delete(service: service, account: account)
            guard try keychain.read(service: service, account: account) == nil else {
                throw DurableAuthStateStoreError.writeVerificationFailed
            }
            return true
        }
    }

    private func loadEnvelopeLocked() throws -> DurableAccountRecoveryEnvelope? {
        guard let data = try keychain.read(service: service, account: account) else { return nil }
        guard data.count <= Self.maximumEnvelopeBytes,
              let envelope = try? JSONDecoder().decode(
                  DurableAccountRecoveryEnvelope.self,
                  from: data
              ),
              envelope.schemaVersion == DurableAccountRecoveryEnvelope.currentSchemaVersion,
              (try? Self.encode(envelope)) == data,
              Self.isStructurallyValid(envelope) else {
            return .init(
                revision: 0,
                state: .incompatible(
                    reasonCode: "stored_recovery_state_invalid",
                    storedStateSHA256: Self.sha256(data)
                )
            )
        }
        return envelope
    }

    private func withMutationLock<T>(_ operation: () throws -> T) throws -> T {
        try Self.mutationLock.withLock {
            guard let interprocessLockURL else { return try operation() }
            do {
                try FileManager.default.createDirectory(
                    at: interprocessLockURL.deletingLastPathComponent(),
                    withIntermediateDirectories: true,
                    attributes: [.posixPermissions: 0o700]
                )
            } catch {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            let descriptor = Darwin.open(
                interprocessLockURL.path,
                O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW,
                S_IRUSR | S_IWUSR
            )
            guard descriptor >= 0 else {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            defer { Darwin.close(descriptor) }
            guard flock(descriptor, LOCK_EX) == 0 else {
                throw DurableAuthStateStoreError.interprocessLockUnavailable
            }
            defer { _ = flock(descriptor, LOCK_UN) }
            return try operation()
        }
    }

    private static var defaultInterprocessLockURL: URL? {
        guard let root = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else { return nil }
        let identity = Data("\(defaultService)\u{0}\(defaultAccount)".utf8)
        let digest = SHA256.hash(data: identity).prefix(16)
            .map { String(format: "%02x", $0) }.joined()
        return root.appendingPathComponent("DayWeave", isDirectory: true)
            .appendingPathComponent("AuthLocks", isDirectory: true)
            .appendingPathComponent("\(digest).lock")
    }

    private static func encode(_ envelope: DurableAccountRecoveryEnvelope) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(envelope)
        guard data.count <= maximumEnvelopeBytes else {
            throw DurableAuthStateStoreError.stateTooLarge
        }
        return data
    }

    private static func isStructurallyValid(_ envelope: DurableAccountRecoveryEnvelope) -> Bool {
        guard envelope.schemaVersion == DurableAccountRecoveryEnvelope.currentSchemaVersion else {
            return false
        }
        switch envelope.state {
        case let .issuePending(value):
            return !isNilUUID(value.proposedID)
                && value.proposedID != value.replaces?.id
                && validCode(value.recoveryCode)
                && finite(value.preparedAt)
                && validMetadata(value.replaces)
                && validIssueFence(value.authorizationFence, request: value.request)
                && value.request.kind == .issue
                && value.request.isValid
                && value.request.authorizationBindingIdentifier
                    == value.authorizationFence.authorizationBindingIdentifier
                && canonicalBody(AccountRecoveryIssueRequest(
                    id: value.proposedID,
                    recoveryCode: value.recoveryCode,
                    replacesRecoveryCodeID: value.replaces?.id,
                    replacesRecoveryCodeRevision: value.replaces?.revision
                )) == value.request.body
        case let .awaitingAcknowledgement(value):
            return validCode(value.recoveryCode)
                && validMetadata(value.metadata)
                && validConfiguration(
                    value.configurationIdentifier,
                    origin: value.originIdentifier
                )
        case let .consumePending(value):
            return validConsumePending(value)
        case let .consumeCommittedAwaitingInstallation(value):
            return validConsumePending(value.pending)
                && validMetadata(value.successor)
                && value.successor.id == value.pending.successorRecoveryCodeID
                && validSession(value.session, pending: value.pending)
        case let .consumeInstalledAwaitingHandoff(value):
            return validConsumePending(value.pending)
                && validMetadata(value.successor)
                && value.successor.id == value.pending.successorRecoveryCodeID
                && validSession(value.session, pending: value.pending)
        case let .incompatible(reasonCode, digest):
            let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789_")
            let hex = CharacterSet(charactersIn: "0123456789abcdef")
            return !reasonCode.isEmpty && reasonCode.count <= 100
                && reasonCode.unicodeScalars.allSatisfy(allowed.contains)
                && digest.count == 64 && digest.unicodeScalars.allSatisfy(hex.contains)
        }
    }

    private static func validConsumePending(_ value: DurableAccountRecoveryConsumePending) -> Bool {
        let materials = [
            value.recoveryCode,
            value.proposedCredentials.accessToken,
            value.proposedCredentials.refreshToken,
            value.successorRecoveryCode,
        ].compactMap(DurableAuthCoordinator.credentialMaterial)
        return !isNilUUID(value.proposedSessionID)
            && !isNilUUID(value.proposedClientInstanceID)
            && !isNilUUID(value.successorRecoveryCodeID)
            && Set([
                value.proposedSessionID,
                value.proposedClientInstanceID,
                value.successorRecoveryCodeID,
            ]).count == 3
            && validCode(value.recoveryCode)
            && validCode(value.successorRecoveryCode)
            && DurableAuthCoordinator.isCredential(
                value.proposedCredentials.accessToken,
                prefix: "dw_da1_"
            )
            && DurableAuthCoordinator.isCredential(
                value.proposedCredentials.refreshToken,
                prefix: "dw_dr1_"
            )
            && materials.count == 4 && Set(materials).count == 4
            && value.descriptor.isValid
            && finite(value.preparedAt)
            && value.installationFence.isValid
            && value.request.kind == .consume
            && value.request.isValid
            && value.request.authorizationBindingIdentifier
                == DurableAccountRecoveryJournaledRequest.credentialBinding(value.recoveryCode)
            && value.request.configurationIdentifier
                == value.installationFence.configurationIdentifier
            && canonicalBody(AccountRecoveryConsumeRequest(
                sessionID: value.proposedSessionID,
                accessToken: value.proposedCredentials.accessToken,
                refreshToken: value.proposedCredentials.refreshToken,
                clientInstanceID: value.proposedClientInstanceID,
                clientKind: "macos",
                deviceLabel: value.descriptor.deviceLabel,
                clientContractVersion: DurableAuthClientDescriptor.contractVersion,
                clientVersion: value.descriptor.clientVersion,
                clientCapabilities: value.descriptor.clientCapabilities,
                successorRecoveryCodeID: value.successorRecoveryCodeID,
                successorRecoveryCode: value.successorRecoveryCode
            )) == value.request.body
    }

    private static func validSession(
        _ session: DurableDeviceSessionMetadata,
        pending: DurableAccountRecoveryConsumePending
    ) -> Bool {
        DurableAuthCoordinator.isStoredSessionValid(session)
            && session.id == pending.proposedSessionID
            && session.clientInstanceID == pending.proposedClientInstanceID
            && session.clientKind == "macos"
            && session.deviceLabel == pending.descriptor.deviceLabel
            && session.scopes == pending.descriptor.scopes
            && session.clientContractVersion == DurableAuthClientDescriptor.contractVersion
            && session.clientVersion == pending.descriptor.clientVersion
            && session.clientCapabilities == pending.descriptor.clientCapabilities
            && session.revision == 1
            && session.createdAt
                >= pending.preparedAt.addingTimeInterval(
                    -DurableAuthCoordinator.clockSkewAllowance
                )
    }

    private static func validMetadata(_ value: DurableAccountRecoveryCodeMetadata?) -> Bool {
        guard let value else { return true }
        return !isNilUUID(value.id) && value.revision == 1 && finite(value.createdAt)
    }

    private static func validIssueFence(
        _ fence: DurableDeviceSessionInventoryFence,
        request: DurableAccountRecoveryJournaledRequest
    ) -> Bool {
        guard let baseURL = try? DayWeaveAPIBaseURL(fence.configurationIdentifier),
              baseURL.canonicalConfigurationIdentifier == fence.configurationIdentifier,
              baseURL.credentialOriginIdentifier == fence.originIdentifier,
              fence.configurationIdentifier == request.configurationIdentifier,
              let sessionID = fence.currentSessionID,
              let clientInstanceID = fence.clientInstanceID else { return false }
        return fence.authorizationBindingIdentifier
            == "device-v1:\(clientInstanceID.uuidString.lowercased()):\(sessionID.uuidString.lowercased())"
    }

    private static func isNilUUID(_ value: UUID) -> Bool {
        value == UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
    }

    private static func validConfiguration(_ value: String, origin: String) -> Bool {
        guard let baseURL = try? DayWeaveAPIBaseURL(value) else { return false }
        return baseURL.canonicalConfigurationIdentifier == value
            && baseURL.credentialOriginIdentifier == origin
    }

    private static func validCode(_ value: String) -> Bool {
        DurableAuthCoordinator.isCredential(value, prefix: "dw_rc1_")
    }

    private static func finite(_ value: Date) -> Bool {
        value.timeIntervalSinceReferenceDate.isFinite
    }

    private static func canonicalBody(_ value: some Encodable) -> Data? {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try? encoder.encode(value)
    }

    private static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}

struct AccountRecoveryIssueRequest: Encodable, Equatable, Sendable {
    let id: UUID
    let recoveryCode: String
    let replacesRecoveryCodeID: UUID?
    let replacesRecoveryCodeRevision: UInt64?

    private enum CodingKeys: String, CodingKey {
        case id
        case recoveryCode = "recovery_code"
        case replacesRecoveryCodeID = "replaces_recovery_code_id"
        case replacesRecoveryCodeRevision = "replaces_recovery_code_revision"
    }
}

struct AccountRecoveryConsumeRequest: Encodable, Equatable, Sendable {
    let sessionID: UUID
    let accessToken: String
    let refreshToken: String
    let clientInstanceID: UUID
    let clientKind: String
    let deviceLabel: String
    let clientContractVersion: Int
    let clientVersion: String
    let clientCapabilities: [String]
    let successorRecoveryCodeID: UUID
    let successorRecoveryCode: String

    private enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case clientInstanceID = "client_instance_id"
        case clientKind = "client_kind"
        case deviceLabel = "device_label"
        case clientContractVersion = "client_contract_version"
        case clientVersion = "client_version"
        case clientCapabilities = "client_capabilities"
        case successorRecoveryCodeID = "successor_recovery_code_id"
        case successorRecoveryCode = "successor_recovery_code"
    }
}

enum DurableAccountRecoveryPhase: String, Equatable, Sendable {
    case idle
    case issuePending
    case consumePending
    case committedAwaitingInstallation
    case installedAwaitingHandoff
    case awaitingAcknowledgement
    case incompatible
}

struct DurableAccountRecoveryPresentation: Equatable, Sendable {
    let phase: DurableAccountRecoveryPhase
    let title: String
    let detail: String
    let awaitingMetadata: DurableAccountRecoveryCodeMetadata?
    let source: DurableAccountRecoveryCodeSource?

    static let idle = Self(
        phase: .idle,
        title: "Recovery protection",
        detail: "Generate a one-use recovery code or securely recover this Mac with one you saved.",
        awaitingMetadata: nil,
        source: nil
    )
}
