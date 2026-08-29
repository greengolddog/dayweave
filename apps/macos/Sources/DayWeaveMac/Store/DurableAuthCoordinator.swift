import CryptoKit
import Darwin
import Foundation
import Security

enum DayWeaveAuthScope: String, Codable, CaseIterable, Sendable {
    case suggestionsRead = "suggestions_read"
    case suggestionsWrite = "suggestions_write"
    case scheduleRead = "schedule_read"
    case scheduleSimulate = "schedule_simulate"
    case itemsRead = "items_read"
    case itemsWrite = "items_write"
    case executionRead = "execution_read"
    case executionWrite = "execution_write"
    case googleRead = "google_read"
    case googleWrite = "google_write"
    case authSessionsRead = "auth_sessions_read"
    case authSessionsWrite = "auth_sessions_write"
    case authMCPClientsRead = "auth_mcp_clients_read"
    case authMCPClientsWrite = "auth_mcp_clients_write"

    static let deviceDefaults: [Self] = [
        .suggestionsRead,
        .suggestionsWrite,
        .scheduleRead,
        .scheduleSimulate,
        .itemsRead,
        .itemsWrite,
        .executionRead,
        .executionWrite,
        .googleRead,
        .googleWrite,
        .authSessionsRead,
        .authSessionsWrite,
        .authMCPClientsRead,
        .authMCPClientsWrite,
    ]
}

struct DurableAuthClientDescriptor: Codable, Equatable, Sendable {
    static let contractVersion = 1
    static let capabilities = [
        "durable_auth_v1",
        "exact_retry_v1",
        "stable_session_binding_v1",
    ]

    let deviceLabel: String
    let clientVersion: String
    let scopes: [DayWeaveAuthScope]
    let clientCapabilities: [String]

    init(
        deviceLabel: String,
        clientVersion: String,
        scopes: [DayWeaveAuthScope] = DayWeaveAuthScope.deviceDefaults,
        clientCapabilities: [String] = Self.capabilities
    ) {
        self.deviceLabel = deviceLabel
        self.clientVersion = clientVersion
        self.scopes = scopes
        self.clientCapabilities = clientCapabilities
    }

    static var live: Self {
        let label = Host.current().localizedName
            .flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty }
            ?? "This Mac"
        let version = (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String)
            .flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty }
            ?? "development"
        return Self(deviceLabel: label, clientVersion: version)
    }

    var isValid: Bool {
        Self.validLabel(deviceLabel, maximumCharacters: 200)
            && Self.validLabel(clientVersion, maximumCharacters: 100)
            && !scopes.isEmpty
            && Set(scopes).count == scopes.count
            && scopes == DayWeaveAuthScope.deviceDefaults
            && clientCapabilities.count <= 100
            && Set(clientCapabilities).count == clientCapabilities.count
            && clientCapabilities.allSatisfy {
                Self.validLabel($0, maximumCharacters: 100)
            }
    }

    private static func validLabel(_ value: String, maximumCharacters: Int) -> Bool {
        !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && value.count <= maximumCharacters
            && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }
}

enum DurableAuthRequestKind: String, Codable, Equatable, Sendable {
    case createEnrollment = "create_enrollment"
    case consumeEnrollment = "consume_enrollment"
    case refreshSession = "refresh_session"

    var pathComponents: [String] {
        switch self {
        case .createEnrollment:
            ["v1", "auth", "device-enrollments"]
        case .consumeEnrollment:
            ["v1", "auth", "device-enrollments", "consume"]
        case .refreshSession:
            ["v1", "auth", "sessions", "refresh"]
        }
    }
}

/// A complete immutable HTTP request fence persisted before credential
/// authority is first sent. Authorization plaintext remains in its dedicated
/// state field; its exact header digest binds that secret to this request.
struct DurableAuthJournaledRequest: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumURLBytes = 8 * 1_024
    static let maximumBodyBytes = 256 * 1_024
    static let securityHeaders = [
        "Accept": "application/json",
        "Cache-Control": "no-store",
        "Content-Type": "application/json",
        "Pragma": "no-cache",
    ]

    let version: Int
    let kind: DurableAuthRequestKind
    let configurationIdentifier: String
    let url: String
    let method: String
    let headers: [String: String]
    let body: Data
    let bodySHA256: String
    let authorizationHeaderSHA256: String

    private enum CodingKeys: String, CodingKey {
        case version
        case kind
        case configurationIdentifier = "configuration_identifier"
        case url
        case method
        case headers
        case body
        case bodySHA256 = "body_sha256"
        case authorizationHeaderSHA256 = "authorization_header_sha256"
    }

    static func make(
        kind: DurableAuthRequestKind,
        baseURL: DayWeaveAPIBaseURL,
        bearer: String,
        body: Data
    ) throws -> Self {
        let configurationIdentifier = baseURL.canonicalConfigurationIdentifier
        guard let canonicalBaseURL = try? DayWeaveAPIBaseURL(configurationIdentifier) else {
            throw DurableAuthError.requestEncodingFailed
        }
        let endpoint: URL
        do {
            endpoint = try canonicalBaseURL.endpoint(pathComponents: kind.pathComponents)
        } catch {
            throw DurableAuthError.requestEncodingFailed
        }
        let value = Self(
            version: currentVersion,
            kind: kind,
            configurationIdentifier: configurationIdentifier,
            url: endpoint.absoluteString,
            method: "POST",
            headers: securityHeaders,
            body: body,
            bodySHA256: sha256(body),
            authorizationHeaderSHA256: sha256(Data("Bearer \(bearer)".utf8))
        )
        guard value.isValid(bearer: bearer) else {
            throw DurableAuthError.requestEncodingFailed
        }
        return value
    }

    func isValid(bearer: String) -> Bool {
        guard version == Self.currentVersion,
              method == "POST",
              headers == Self.securityHeaders,
              !body.isEmpty,
              body.count <= Self.maximumBodyBytes,
              url.utf8.count <= Self.maximumURLBytes,
              bodySHA256 == Self.sha256(body),
              authorizationHeaderSHA256
                == Self.sha256(Data("Bearer \(bearer)".utf8)),
              let baseURL = try? DayWeaveAPIBaseURL(configurationIdentifier),
              baseURL.canonicalConfigurationIdentifier == configurationIdentifier,
              let expected = try? baseURL.endpoint(pathComponents: kind.pathComponents),
              expected.absoluteString == url,
              let components = URLComponents(string: url),
              components.user == nil,
              components.password == nil,
              components.query == nil,
              components.fragment == nil else { return false }
        // `DayWeaveAPIBaseURL` already enforces HTTPS remotely and permits
        // plain HTTP only for an exact loopback host.
        return true
    }

    func isBound(to baseURL: DayWeaveAPIBaseURL, bearer: String) -> Bool {
        configurationIdentifier == baseURL.canonicalConfigurationIdentifier
            && isValid(bearer: bearer)
    }

    func boundBaseURL() throws -> DayWeaveAPIBaseURL {
        guard let baseURL = try? DayWeaveAPIBaseURL(configurationIdentifier),
              baseURL.canonicalConfigurationIdentifier == configurationIdentifier else {
            throw DurableAuthError.incompatibleState
        }
        return baseURL
    }

    func makeURLRequest(bearer: String) throws -> URLRequest {
        guard isValid(bearer: bearer), let target = URL(string: url) else {
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

    private static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

struct DurableDeviceSessionMetadata: Codable, Equatable, Sendable {
    let id: UUID
    let clientInstanceID: UUID
    let clientKind: String
    let deviceLabel: String
    let scopes: [DayWeaveAuthScope]
    let clientContractVersion: Int
    let clientVersion: String
    let clientCapabilities: [String]
    let createdAt: Date
    let lastSeenAt: Date
    let credentialIssuedAt: Date
    let accessExpiresAt: Date
    let refreshIdleExpiresAt: Date
    let absoluteExpiresAt: Date
    let revision: UInt64

    private enum CodingKeys: String, CodingKey {
        case id
        case clientInstanceID = "client_instance_id"
        case clientKind = "client_kind"
        case deviceLabel = "device_label"
        case scopes
        case clientContractVersion = "client_contract_version"
        case clientVersion = "client_version"
        case clientCapabilities = "client_capabilities"
        case createdAt = "created_at"
        case lastSeenAt = "last_seen_at"
        case credentialIssuedAt = "credential_issued_at"
        case accessExpiresAt = "access_expires_at"
        case refreshIdleExpiresAt = "refresh_idle_expires_at"
        case absoluteExpiresAt = "absolute_expires_at"
        case revision
    }
}

struct DurableAuthCredentialPair: Codable, Equatable, Sendable {
    let accessToken: String
    let refreshToken: String

    private enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
    }
}

struct LegacyAuthState: Codable, Equatable, Sendable {
    let bearerToken: String

    private enum CodingKeys: String, CodingKey {
        case bearerToken = "bearer_token"
    }
}

struct EnrollmentCreationPendingAuthState: Codable, Equatable, Sendable {
    let bootstrapToken: String
    let proposedEnrollmentID: UUID
    let proposedEnrollmentToken: String
    let proposedSessionID: UUID
    let proposedCredentials: DurableAuthCredentialPair
    let descriptor: DurableAuthClientDescriptor
    let preparedAt: Date
    let creationRequest: DurableAuthJournaledRequest
    let durableWasPreviouslyActivated: Bool

    private enum CodingKeys: String, CodingKey {
        case bootstrapToken = "bootstrap_token"
        case proposedEnrollmentID = "proposed_enrollment_id"
        case proposedEnrollmentToken = "proposed_enrollment_token"
        case proposedSessionID = "proposed_session_id"
        case proposedCredentials = "proposed_credentials"
        case descriptor
        case preparedAt = "prepared_at"
        case creationRequest = "creation_request"
        case durableWasPreviouslyActivated = "durable_was_previously_activated"
    }
}

struct EnrollmentPendingAuthState: Codable, Equatable, Sendable {
    let enrollmentID: UUID?
    let enrollmentToken: String
    let enrollmentExpiresAt: Date?
    let proposedSessionID: UUID
    let proposedCredentials: DurableAuthCredentialPair
    let descriptor: DurableAuthClientDescriptor
    let preparedAt: Date
    let consumeRequest: DurableAuthJournaledRequest
    let durableWasPreviouslyActivated: Bool

    private enum CodingKeys: String, CodingKey {
        case enrollmentID = "enrollment_id"
        case enrollmentToken = "enrollment_token"
        case enrollmentExpiresAt = "enrollment_expires_at"
        case proposedSessionID = "proposed_session_id"
        case proposedCredentials = "proposed_credentials"
        case descriptor
        case preparedAt = "prepared_at"
        case consumeRequest = "consume_request"
        case durableWasPreviouslyActivated = "durable_was_previously_activated"
    }
}

struct ActiveDurableAuthState: Codable, Equatable, Sendable {
    let session: DurableDeviceSessionMetadata
    let credentials: DurableAuthCredentialPair
}

struct RefreshPendingAuthState: Codable, Equatable, Sendable {
    let previous: ActiveDurableAuthState
    let nextCredentials: DurableAuthCredentialPair
    let preparedAt: Date
    let refreshRequest: DurableAuthJournaledRequest

    private enum CodingKeys: String, CodingKey {
        case previous
        case nextCredentials = "next_credentials"
        case preparedAt = "prepared_at"
        case refreshRequest = "refresh_request"
    }
}

enum DurableReauthenticationReason: String, Codable, Equatable, Sendable {
    case rejected
    case expired
    case explicitlyDisconnected = "explicitly_disconnected"
}

struct ReauthenticationAuthState: Codable, Equatable, Sendable {
    let clientInstanceID: UUID?
    let previousSessionID: UUID?
    let reason: DurableReauthenticationReason
    let detectedAt: Date

    private enum CodingKeys: String, CodingKey {
        case clientInstanceID = "client_instance_id"
        case previousSessionID = "previous_session_id"
        case reason
        case detectedAt = "detected_at"
    }
}

enum IncompatibleAuthRecovery: Codable, Equatable, Sendable {
    case enrollmentCreation(EnrollmentCreationPendingAuthState)
    case enrollment(EnrollmentPendingAuthState)
    case refresh(RefreshPendingAuthState)
    case active(ActiveDurableAuthState)

    private enum CodingKeys: String, CodingKey { case kind, payload }
    private enum Kind: String, Codable {
        case enrollmentCreation = "enrollment_creation"
        case enrollment
        case refresh
        case active
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .enrollmentCreation:
            self = .enrollmentCreation(try container.decode(
                EnrollmentCreationPendingAuthState.self,
                forKey: .payload
            ))
        case .enrollment:
            self = .enrollment(try container.decode(
                EnrollmentPendingAuthState.self,
                forKey: .payload
            ))
        case .refresh:
            self = .refresh(try container.decode(RefreshPendingAuthState.self, forKey: .payload))
        case .active:
            self = .active(try container.decode(ActiveDurableAuthState.self, forKey: .payload))
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .enrollmentCreation(value):
            try container.encode(Kind.enrollmentCreation, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .enrollment(value):
            try container.encode(Kind.enrollment, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .refresh(value):
            try container.encode(Kind.refresh, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .active(value):
            try container.encode(Kind.active, forKey: .kind)
            try container.encode(value, forKey: .payload)
        }
    }
}

struct IncompatibleAuthState: Codable, Equatable, Sendable {
    let reasonCode: String
    let storedSchemaVersion: Int?
    let storedStateSHA256: String?
    let detectedAt: Date
    let recovery: IncompatibleAuthRecovery?

    init(
        reasonCode: String,
        storedSchemaVersion: Int?,
        storedStateSHA256: String? = nil,
        detectedAt: Date,
        recovery: IncompatibleAuthRecovery?
    ) {
        self.reasonCode = reasonCode
        self.storedSchemaVersion = storedSchemaVersion
        self.storedStateSHA256 = storedStateSHA256
        self.detectedAt = detectedAt
        self.recovery = recovery
    }

    private enum CodingKeys: String, CodingKey {
        case reasonCode = "reason_code"
        case storedSchemaVersion = "stored_schema_version"
        case storedStateSHA256 = "stored_state_sha256"
        case detectedAt = "detected_at"
        case recovery
    }
}

enum DurableAuthState: Codable, Equatable, Sendable {
    case legacy(LegacyAuthState)
    case enrollmentCreationPending(EnrollmentCreationPendingAuthState)
    case enrollmentPending(EnrollmentPendingAuthState)
    case active(ActiveDurableAuthState)
    case refreshPending(RefreshPendingAuthState)
    case reauthenticationRequired(ReauthenticationAuthState)
    case incompatible(IncompatibleAuthState)

    private enum CodingKeys: String, CodingKey { case kind, payload }
    private enum Kind: String, Codable {
        case legacy
        case enrollmentCreationPending = "enrollment_creation_pending"
        case enrollmentPending = "enrollment_pending"
        case active
        case refreshPending = "refresh_pending"
        case reauthenticationRequired = "reauthentication_required"
        case incompatible
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .legacy:
            self = .legacy(try container.decode(LegacyAuthState.self, forKey: .payload))
        case .enrollmentCreationPending:
            self = .enrollmentCreationPending(try container.decode(
                EnrollmentCreationPendingAuthState.self,
                forKey: .payload
            ))
        case .enrollmentPending:
            self = .enrollmentPending(try container.decode(
                EnrollmentPendingAuthState.self,
                forKey: .payload
            ))
        case .active:
            self = .active(try container.decode(ActiveDurableAuthState.self, forKey: .payload))
        case .refreshPending:
            self = .refreshPending(try container.decode(
                RefreshPendingAuthState.self,
                forKey: .payload
            ))
        case .reauthenticationRequired:
            self = .reauthenticationRequired(try container.decode(
                ReauthenticationAuthState.self,
                forKey: .payload
            ))
        case .incompatible:
            self = .incompatible(try container.decode(IncompatibleAuthState.self, forKey: .payload))
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .legacy(value):
            try container.encode(Kind.legacy, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .enrollmentCreationPending(value):
            try container.encode(Kind.enrollmentCreationPending, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .enrollmentPending(value):
            try container.encode(Kind.enrollmentPending, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .active(value):
            try container.encode(Kind.active, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .refreshPending(value):
            try container.encode(Kind.refreshPending, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .reauthenticationRequired(value):
            try container.encode(Kind.reauthenticationRequired, forKey: .kind)
            try container.encode(value, forKey: .payload)
        case let .incompatible(value):
            try container.encode(Kind.incompatible, forKey: .kind)
            try container.encode(value, forKey: .payload)
        }
    }
}

struct DurableAuthEnvelope: Codable, Equatable, Sendable {
    static let currentSchemaVersion = 1

    let schemaVersion: Int
    let revision: UInt64
    let origin: String?
    let clientInstanceID: UUID?
    let state: DurableAuthState

    init(
        revision: UInt64,
        origin: String?,
        clientInstanceID: UUID?,
        state: DurableAuthState,
        schemaVersion: Int = Self.currentSchemaVersion
    ) {
        self.schemaVersion = schemaVersion
        self.revision = revision
        self.origin = origin
        self.clientInstanceID = clientInstanceID
        self.state = state
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case revision
        case origin
        case clientInstanceID = "client_instance_id"
        case state
    }

    func replacingState(_ state: DurableAuthState) throws -> Self {
        guard revision < UInt64.max else {
            throw DurableAuthStateStoreError.revisionOverflow
        }
        return Self(
            revision: revision + 1,
            origin: origin,
            clientInstanceID: clientInstanceID,
            state: state
        )
    }
}

/// Prevents accidental disclosure through `String(describing:)`,
/// `String(reflecting:)`, debugger summaries, or generic mirror-based logs.
protocol RedactedAuthDescribing: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable {}

extension RedactedAuthDescribing {
    var description: String { "<redacted durable authentication state>" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

extension DurableAuthCredentialPair: RedactedAuthDescribing {}
extension DurableAuthJournaledRequest: RedactedAuthDescribing {}
extension LegacyAuthState: RedactedAuthDescribing {}
extension EnrollmentCreationPendingAuthState: RedactedAuthDescribing {}
extension EnrollmentPendingAuthState: RedactedAuthDescribing {}
extension ActiveDurableAuthState: RedactedAuthDescribing {}
extension RefreshPendingAuthState: RedactedAuthDescribing {}
extension IncompatibleAuthRecovery: RedactedAuthDescribing {}
extension IncompatibleAuthState: RedactedAuthDescribing {}
extension DurableAuthState: RedactedAuthDescribing {}
extension DurableAuthEnvelope: RedactedAuthDescribing {}

enum DurableAuthStateStoreError: Error, Equatable, Sendable {
    case invalidStoredState
    case stateTooLarge
    case revisionOverflow
    case interprocessLockUnavailable
    case writeVerificationFailed
}

protocol DurableAuthStateStoring: Sendable {
    func loadEnvelope() throws -> DurableAuthEnvelope?
    func compareAndSwap(
        expected: DurableAuthEnvelope?,
        replacement: DurableAuthEnvelope?
    ) throws -> Bool
}

final class KeychainDurableAuthStateStore: DurableAuthStateStoring, @unchecked Sendable {
    static let defaultService = KeychainBearerTokenStore.defaultService
    static let defaultAccount = "durable-auth-envelope-v1"
    static let maximumEnvelopeBytes = 256 * 1_024

    private static let mutationLock = NSLock()
    private let service: String
    private let account: String
    private let keychain: any KeychainSecretAccessing
    private let now: @Sendable () -> Date
    private let interprocessLockURL: URL?

    init(
        service: String = KeychainDurableAuthStateStore.defaultService,
        account: String = KeychainDurableAuthStateStore.defaultAccount,
        keychain: any KeychainSecretAccessing = SystemKeychainSecretAccess(),
        now: @escaping @Sendable () -> Date = Date.init,
        interprocessLockURL: URL? = KeychainDurableAuthStateStore.defaultInterprocessLockURL
    ) {
        self.service = service
        self.account = account
        self.keychain = keychain
        self.now = now
        self.interprocessLockURL = interprocessLockURL
    }

    func loadEnvelope() throws -> DurableAuthEnvelope? {
        try withMutationLock { try loadEnvelopeLocked() }
    }

    func compareAndSwap(
        expected: DurableAuthEnvelope?,
        replacement: DurableAuthEnvelope?
    ) throws -> Bool {
        try withMutationLock {
            guard try loadEnvelopeLocked() == expected else { return false }
            if let replacement {
                if expected?.revision == UInt64.max {
                    throw DurableAuthStateStoreError.revisionOverflow
                }
                let expectedRevision = expected.map { $0.revision + 1 } ?? 0
                guard replacement.revision == expectedRevision else {
                    throw DurableAuthStateStoreError.revisionOverflow
                }
                guard Self.isStructurallyValid(replacement) else {
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

    private static var defaultInterprocessLockURL: URL? {
        guard let root = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first else { return nil }
        let identity = Data("\(defaultService)\u{0}\(defaultAccount)".utf8)
        let digest = SHA256.hash(data: identity)
            .prefix(16)
            .map { String(format: "%02x", $0) }
            .joined()
        return root
            .appendingPathComponent("DayWeave", isDirectory: true)
            .appendingPathComponent("AuthLocks", isDirectory: true)
            .appendingPathComponent("\(digest).lock", isDirectory: false)
    }

    private func withMutationLock<T>(_ operation: () throws -> T) throws -> T {
        try Self.mutationLock.withLock {
            guard let interprocessLockURL else { return try operation() }
            let directory = interprocessLockURL.deletingLastPathComponent()
            do {
                try FileManager.default.createDirectory(
                    at: directory,
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

    private func loadEnvelopeLocked() throws -> DurableAuthEnvelope? {
        guard let data = try keychain.read(service: service, account: account) else { return nil }
        guard data.count <= Self.maximumEnvelopeBytes else {
            return incompatibleEnvelope(data: data, reason: "stored_state_too_large")
        }
        let decoder = JSONDecoder()
        guard let envelope = try? decoder.decode(DurableAuthEnvelope.self, from: data) else {
            return incompatibleEnvelope(data: data, reason: "stored_state_invalid")
        }
        guard envelope.schemaVersion == DurableAuthEnvelope.currentSchemaVersion else {
            return DurableAuthEnvelope(
                revision: envelope.revision,
                origin: Self.canonicalOrigin(envelope.origin),
                clientInstanceID: envelope.clientInstanceID,
                state: .incompatible(.init(
                    reasonCode: "unsupported_schema_version",
                    storedSchemaVersion: envelope.schemaVersion,
                    storedStateSHA256: Self.sha256Identity(data),
                    detectedAt: Date(timeIntervalSince1970: 0),
                    recovery: nil
                )),
                schemaVersion: envelope.schemaVersion
            )
        }
        // Schema v1 has one canonical byte representation. Unknown keys,
        // whitespace, alternate key order, and other JSON aliases are not
        // allowed to acquire authority just because JSONDecoder accepts them.
        guard (try? Self.encode(envelope)) == data else {
            return incompatibleEnvelope(data: data, reason: "stored_state_noncanonical")
        }
        guard Self.isStructurallyValid(envelope) else {
            return incompatibleEnvelope(data: data, reason: "stored_state_invalid")
        }
        return envelope
    }

    private func incompatibleEnvelope(data: Data, reason: String) -> DurableAuthEnvelope {
        struct Header: Decodable {
            let schemaVersion: Int?
            let revision: UInt64?
            let origin: String?
            let clientInstanceID: UUID?

            private enum CodingKeys: String, CodingKey {
                case schemaVersion = "schema_version"
                case revision
                case origin
                case clientInstanceID = "client_instance_id"
            }
        }
        let header = try? JSONDecoder().decode(Header.self, from: data)
        return DurableAuthEnvelope(
            revision: header?.revision ?? 0,
            origin: Self.canonicalOrigin(header?.origin),
            clientInstanceID: header?.clientInstanceID,
            state: .incompatible(.init(
                reasonCode: reason,
                storedSchemaVersion: header?.schemaVersion,
                storedStateSHA256: Self.sha256Identity(data),
                detectedAt: Date(timeIntervalSince1970: 0),
                recovery: nil
            ))
        )
    }

    private static func encode(_ envelope: DurableAuthEnvelope) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(envelope)
        guard data.count <= maximumEnvelopeBytes else {
            throw DurableAuthStateStoreError.stateTooLarge
        }
        return data
    }

    private static func isStructurallyValid(_ envelope: DurableAuthEnvelope) -> Bool {
        guard envelope.schemaVersion == DurableAuthEnvelope.currentSchemaVersion else { return false }
        switch envelope.state {
        case let .legacy(value):
            return canonicalOrigin(envelope.origin) != nil
                && envelope.clientInstanceID != nil
                && DurableAuthCoordinator.isValidLegacyToken(value.bearerToken)
        case let .enrollmentCreationPending(value):
            return canonicalOrigin(envelope.origin) != nil
                && isValidEnrollmentCreation(
                    value,
                    clientInstanceID: envelope.clientInstanceID,
                    origin: envelope.origin
                )
        case let .enrollmentPending(value):
            return canonicalOrigin(envelope.origin) != nil
                && isValidEnrollment(
                    value,
                    clientInstanceID: envelope.clientInstanceID,
                    origin: envelope.origin
                )
        case let .active(value):
            return canonicalOrigin(envelope.origin) != nil
                && isValidActive(value, clientInstanceID: envelope.clientInstanceID)
        case let .refreshPending(value):
            return canonicalOrigin(envelope.origin) != nil
                && isValidRefresh(
                    value,
                    clientInstanceID: envelope.clientInstanceID,
                    origin: envelope.origin
                )
        case let .reauthenticationRequired(value):
            return canonicalOrigin(envelope.origin) != nil
                && value.clientInstanceID == envelope.clientInstanceID
                && isFinite(value.detectedAt)
        case let .incompatible(value):
            return isValidIncompatible(value, envelope: envelope)
        }
    }

    private static func isValidEnrollmentCreation(
        _ value: EnrollmentCreationPendingAuthState,
        clientInstanceID: UUID?,
        origin: String?
    ) -> Bool {
        guard let clientInstanceID,
              DurableAuthCoordinator.isValidLegacyToken(value.bootstrapToken),
              value.descriptor.isValid,
              isFinite(value.preparedAt),
              requestMatchesOrigin(value.creationRequest, origin: origin),
              value.creationRequest.kind == .createEnrollment,
              value.creationRequest.isValid(bearer: value.bootstrapToken),
              let expectedBody = canonicalRequestBody(CreateEnrollmentRequest(
                  id: value.proposedEnrollmentID,
                  enrollmentToken: value.proposedEnrollmentToken,
                  clientInstanceID: clientInstanceID,
                  clientKind: "macos",
                  deviceLabel: value.descriptor.deviceLabel,
                  scopes: value.descriptor.scopes,
                  clientContractVersion: DurableAuthClientDescriptor.contractVersion,
                  clientVersion: value.descriptor.clientVersion,
                  clientCapabilities: value.descriptor.clientCapabilities
              )),
              expectedBody == value.creationRequest.body else { return false }
        return hasDistinctMaterials([
            (value.proposedEnrollmentToken, "dw_en1_"),
            (value.proposedCredentials.accessToken, "dw_da1_"),
            (value.proposedCredentials.refreshToken, "dw_dr1_"),
        ])
    }

    private static func isValidEnrollment(
        _ value: EnrollmentPendingAuthState,
        clientInstanceID: UUID?,
        origin: String?
    ) -> Bool {
        let identityIsConsistent = (value.enrollmentID == nil) == (value.enrollmentExpiresAt == nil)
        let clientIdentityIsConsistent = value.enrollmentID == nil || clientInstanceID != nil
        let expiryIsValid = value.enrollmentExpiresAt.map {
            isFinite($0) && $0 >= value.preparedAt.addingTimeInterval(
                -DurableAuthCoordinator.clockSkewAllowance
            )
        } ?? true
        return value.descriptor.isValid
            && isFinite(value.preparedAt)
            && identityIsConsistent
            && clientIdentityIsConsistent
            && expiryIsValid
            && requestMatchesOrigin(value.consumeRequest, origin: origin)
            && value.consumeRequest.kind == .consumeEnrollment
            && value.consumeRequest.isValid(bearer: value.enrollmentToken)
            && canonicalRequestBody(ConsumeEnrollmentRequest(
                sessionID: value.proposedSessionID,
                accessToken: value.proposedCredentials.accessToken,
                refreshToken: value.proposedCredentials.refreshToken
            )) == value.consumeRequest.body
            && hasDistinctMaterials([
                (value.enrollmentToken, "dw_en1_"),
                (value.proposedCredentials.accessToken, "dw_da1_"),
                (value.proposedCredentials.refreshToken, "dw_dr1_"),
            ])
    }

    private static func isValidActive(
        _ value: ActiveDurableAuthState,
        clientInstanceID: UUID?
    ) -> Bool {
        value.session.clientInstanceID == clientInstanceID
            && DurableAuthCoordinator.isStoredSessionValid(value.session)
            && hasDistinctMaterials([
                (value.credentials.accessToken, "dw_da1_"),
                (value.credentials.refreshToken, "dw_dr1_"),
            ])
    }

    private static func isValidRefresh(
        _ value: RefreshPendingAuthState,
        clientInstanceID: UUID?,
        origin: String?
    ) -> Bool {
        isFinite(value.preparedAt)
            && isValidActive(value.previous, clientInstanceID: clientInstanceID)
            && requestMatchesOrigin(value.refreshRequest, origin: origin)
            && value.refreshRequest.kind == .refreshSession
            && value.refreshRequest.isValid(bearer: value.previous.credentials.refreshToken)
            && canonicalRequestBody(RefreshRequest(
                nextAccessToken: value.nextCredentials.accessToken,
                nextRefreshToken: value.nextCredentials.refreshToken
            )) == value.refreshRequest.body
            && hasDistinctMaterials([
                (value.previous.credentials.accessToken, "dw_da1_"),
                (value.previous.credentials.refreshToken, "dw_dr1_"),
                (value.nextCredentials.accessToken, "dw_da1_"),
                (value.nextCredentials.refreshToken, "dw_dr1_"),
            ])
    }

    private static func isValidIncompatible(
        _ value: IncompatibleAuthState,
        envelope: DurableAuthEnvelope
    ) -> Bool {
        guard isSafeReasonCode(value.reasonCode), isFinite(value.detectedAt),
              value.storedSchemaVersion.map({ $0 >= 0 }) ?? true,
              value.storedStateSHA256.map(isSHA256Identity) ?? true else { return false }
        if value.storedStateSHA256 != nil {
            return value.recovery == nil
        }
        guard canonicalOrigin(envelope.origin) != nil else { return false }
        switch value.recovery {
        case let .enrollmentCreation(pending):
            return isValidEnrollmentCreation(
                pending,
                clientInstanceID: envelope.clientInstanceID,
                origin: envelope.origin
            )
        case let .enrollment(pending):
            return isValidEnrollment(
                pending,
                clientInstanceID: envelope.clientInstanceID,
                origin: envelope.origin
            )
        case let .refresh(pending):
            return isValidRefresh(
                pending,
                clientInstanceID: envelope.clientInstanceID,
                origin: envelope.origin
            )
        case let .active(active):
            return isValidActive(active, clientInstanceID: envelope.clientInstanceID)
        case nil: return true
        }
    }

    private static func hasDistinctMaterials(_ credentials: [(String, String)]) -> Bool {
        let materials = credentials.compactMap { credential, prefix -> Data? in
            guard DurableAuthCoordinator.isCredential(credential, prefix: prefix) else { return nil }
            return DurableAuthCoordinator.credentialMaterial(credential)
        }
        return materials.count == credentials.count && Set(materials).count == credentials.count
    }

    private static func requestMatchesOrigin(
        _ request: DurableAuthJournaledRequest,
        origin: String?
    ) -> Bool {
        guard let baseURL = try? request.boundBaseURL() else { return false }
        return baseURL.credentialOriginIdentifier == origin
    }

    private static func canonicalRequestBody(_ value: some Encodable) -> Data? {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try? encoder.encode(value)
    }

    private static func canonicalOrigin(_ origin: String?) -> String? {
        guard let origin, let baseURL = try? DayWeaveAPIBaseURL(origin),
              baseURL.credentialOriginIdentifier == origin else { return nil }
        return origin
    }

    private static func isFinite(_ date: Date) -> Bool {
        date.timeIntervalSinceReferenceDate.isFinite
    }

    private static func isSafeReasonCode(_ value: String) -> Bool {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789_")
        return !value.isEmpty && value.count <= 100
            && value.unicodeScalars.allSatisfy(allowed.contains)
    }

    private static func sha256Identity(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private static func isSHA256Identity(_ value: String) -> Bool {
        let allowed = CharacterSet(charactersIn: "0123456789abcdef")
        return value.count == 64 && value.unicodeScalars.allSatisfy(allowed.contains)
    }
}

protocol DurableCredentialGenerating: Sendable {
    func makeCredential(prefix: String) throws -> String
    func makeUUID() throws -> UUID
}

struct SystemDurableCredentialGenerator: DurableCredentialGenerating {
    func makeCredential(prefix: String) throws -> String {
        var random = Data(count: 32)
        let status = random.withUnsafeMutableBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { return errSecAllocate }
            return SecRandomCopyBytes(kSecRandomDefault, buffer.count, baseAddress)
        }
        guard status == errSecSuccess else {
            random.resetBytes(in: random.startIndex..<random.endIndex)
            throw DurableAuthError.randomnessUnavailable
        }
        let payload = random.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        random.resetBytes(in: random.startIndex..<random.endIndex)
        let credential = prefix + payload
        guard DurableAuthCoordinator.isCredential(credential, prefix: prefix) else {
            throw DurableAuthError.randomnessUnavailable
        }
        return credential
    }

    func makeUUID() throws -> UUID {
        let bytes = try makeCredential(prefix: "")
        guard let data = DurableAuthCoordinator.decodeCredentialPayload(bytes), data.count >= 16 else {
            throw DurableAuthError.randomnessUnavailable
        }
        let uuidBytes = [UInt8](data.prefix(16))
        return UUID(uuid: (
            uuidBytes[0], uuidBytes[1], uuidBytes[2], uuidBytes[3],
            uuidBytes[4], uuidBytes[5], uuidBytes[6], uuidBytes[7],
            uuidBytes[8], uuidBytes[9], uuidBytes[10], uuidBytes[11],
            uuidBytes[12], uuidBytes[13], uuidBytes[14], uuidBytes[15]
        ))
    }
}

struct DurableAuthHTTPResponse: Sendable {
    let statusCode: Int
    let headers: [String: String]
    let body: Data

    func header(_ name: String) -> String? { headers[name.lowercased()] }
}

/// Contract checks shared by the credential endpoints and ordinary API 401
/// recovery. Only a fully authenticated error shape is allowed to rotate or
/// destroy local credential state.
enum DayWeaveAuthResponseContract {
    static let maximumErrorBytes = 64 * 1_024
    static let bearerChallenge = "Bearer realm=\"dayweave\""

    static func validateNoStore(
        headers: [String: String],
        requiresJSON: Bool
    ) throws {
        let cacheDirectives = (headers["cache-control"] ?? "")
            .split(separator: ",", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }
        guard cacheDirectives.count == 2,
              Set(cacheDirectives) == Set(["no-store", "max-age=0"]),
              headers["pragma"]?.trimmingCharacters(in: .whitespacesAndNewlines)
                .lowercased() == "no-cache",
              !requiresJSON || isJSONMediaType(headers["content-type"]) else {
            throw DurableAuthError.invalidResponse
        }
    }

    static func isDefinitiveUnauthorized(
        statusCode: Int,
        headers: [String: String],
        body: Data
    ) -> Bool {
        guard statusCode == 401,
              headers["www-authenticate"] == bearerChallenge else { return false }
        return (try? validateError(
            statusCode: statusCode,
            headers: headers,
            body: body
        )) != nil
    }

    @discardableResult
    static func validateDeterministicError(
        statusCode: Int,
        headers: [String: String],
        body: Data
    ) throws -> String {
        try validateError(statusCode: statusCode, headers: headers, body: body)
    }

    private static func validateError(
        statusCode: Int,
        headers: [String: String],
        body: Data
    ) throws -> String {
        let expectedCode: String = switch statusCode {
        case 400: "invalid_json"
        case 401: "unauthorized"
        case 403: "forbidden"
        case 404: "not_found"
        case 409: "conflict"
        case 422: "validation_failed"
        default: throw DurableAuthError.invalidResponse
        }
        guard body.count <= maximumErrorBytes else { throw DurableAuthError.invalidResponse }
        try validateNoStore(headers: headers, requiresJSON: true)
        if statusCode == 401, headers["www-authenticate"] != bearerChallenge {
            throw DurableAuthError.invalidResponse
        }
        if let requestID = headers["x-request-id"], !isSafeText(requestID, maximum: 200) {
            throw DurableAuthError.invalidResponse
        }
        guard let outer = try JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              Set(error.keys) == ["code", "message"],
              let code = error["code"] as? String,
              let message = error["message"] as? String,
              code == expectedCode,
              isSafeCode(code),
              isSafeText(message, maximum: 500) else {
            throw DurableAuthError.invalidResponse
        }
        return code
    }

    private static func isJSONMediaType(_ value: String?) -> Bool {
        guard let value else { return false }
        let components = value.split(separator: ";", omittingEmptySubsequences: false)
        guard let mediaType = components.first?.trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased(), mediaType == "application/json" else { return false }
        // Credential mutation trusts only the media forms emitted by the
        // DayWeave server. Broad MIME parameter parsing would let arbitrary or
        // malformed parameters become an authentication trust signal.
        switch components.count {
        case 1:
            return true
        case 2:
            let pair = components[1].split(
                separator: "=",
                maxSplits: 1,
                omittingEmptySubsequences: false
            )
            return pair.count == 2
                && pair[0].trimmingCharacters(in: .whitespacesAndNewlines)
                    .lowercased() == "charset"
                && pair[1].trimmingCharacters(in: .whitespacesAndNewlines)
                    .lowercased() == "utf-8"
        default:
            return false
        }
    }

    private static func isSafeCode(_ value: String) -> Bool {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789_")
        return !value.isEmpty && value.count <= 100
            && value.unicodeScalars.allSatisfy(allowed.contains)
    }

    private static func isSafeText(_ value: String, maximum: Int) -> Bool {
        !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && value.count <= maximum
            && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }
}

protocol DurableAuthHTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> DurableAuthHTTPResponse
}

struct URLSessionDurableAuthHTTPTransport: DurableAuthHTTPTransport {
    static let maximumResponseBytes = 1 * 1_048_576
    private let session: URLSession

    init(session: URLSession = makeDayWeaveEphemeralSession()) {
        self.session = session
    }

    func send(_ request: URLRequest) async throws -> DurableAuthHTTPResponse {
        do {
            let (bytes, response) = try await session.bytes(
                for: request,
                delegate: DurableAuthRejectRedirectDelegate.shared
            )
            guard let http = response as? HTTPURLResponse else {
                throw DurableAuthError.invalidResponse
            }
            if http.expectedContentLength > Int64(Self.maximumResponseBytes) {
                bytes.task.cancel()
                throw DurableAuthError.responseTooLarge
            }
            var body = Data()
            for try await byte in bytes {
                guard body.count < Self.maximumResponseBytes else {
                    bytes.task.cancel()
                    throw DurableAuthError.responseTooLarge
                }
                body.append(byte)
            }
            var headers: [String: String] = [:]
            for (key, value) in http.allHeaderFields {
                guard let key = key as? String else { continue }
                headers[key.lowercased()] = String(describing: value)
            }
            return .init(statusCode: http.statusCode, headers: headers, body: body)
        } catch let error as DurableAuthError {
            throw error
        } catch let error as URLError {
            throw DurableAuthError.transport(error.code)
        } catch {
            throw DurableAuthError.transport(.unknown)
        }
    }
}

private final class DurableAuthRejectRedirectDelegate: NSObject, URLSessionTaskDelegate,
    @unchecked Sendable
{
    static let shared = DurableAuthRejectRedirectDelegate()

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

enum DurableAuthError: Error, Equatable, Sendable {
    case notConfigured
    case originMismatch
    case invalidBootstrapCredential
    case invalidEnrollmentCode
    case durableSessionRequiresExplicitReenrollment
    case remoteRevocationUnavailable
    case activeSessionMustBeRevoked
    case enrollmentRequired
    case reauthenticationRequired
    case incompatibleState
    case randomnessUnavailable
    case localStateUnavailable
    case concurrentStateChange
    case requestEncodingFailed
    case invalidResponse
    case responseTooLarge
    case rejected
    case retryableServer(statusCode: Int)
    case transport(URLError.Code)
}

extension DurableAuthError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .notConfigured:
            "Add the DayWeave API address and a bootstrap credential in Settings."
        case .originMismatch:
            "The saved authentication state belongs to a different API origin."
        case .invalidBootstrapCredential:
            "The bootstrap credential is invalid. Re-enter it without leading or trailing spaces."
        case .invalidEnrollmentCode:
            "The one-time enrollment code is invalid. It must be an exact unmodified DayWeave enrollment code."
        case .durableSessionRequiresExplicitReenrollment:
            "A rotating device session is already active. Use Re-enroll explicitly to replace it."
        case .remoteRevocationUnavailable:
            "This authentication state has no usable device session to revoke remotely. Confirm local-only removal if you understand that server or bootstrap authority may remain active."
        case .activeSessionMustBeRevoked:
            "Revoke the current device session before enrolling a replacement. DayWeave will not orphan a live server session."
        case .enrollmentRequired:
            "This Mac still needs to be enrolled for rotating credentials."
        case .reauthenticationRequired:
            "This device session expired or was revoked. Re-enroll this Mac in Settings."
        case .incompatibleState:
            "Authentication state is incompatible with this app version. Update DayWeave or reset the saved session."
        case .randomnessUnavailable:
            "Secure credential generation is unavailable. No authentication request was sent."
        case .localStateUnavailable:
            "The atomic authentication state could not be read or saved in Keychain."
        case .concurrentStateChange:
            "Authentication changed in another operation. Reload Settings before trying again."
        case .requestEncodingFailed:
            "The authentication request could not be encoded safely."
        case .invalidResponse:
            "The authentication server returned a response outside the supported contract."
        case .responseTooLarge:
            "The authentication response exceeded the safe size limit."
        case .rejected:
            "The authentication server rejected the credential. Re-enroll this Mac."
        case let .retryableServer(statusCode):
            "The authentication server is temporarily unavailable (HTTP \(statusCode)). The exact request is saved for retry."
        case let .transport(code):
            code == .notConnectedToInternet
                ? "The Mac is offline. The exact authentication request is saved for retry."
                : "The authentication request could not be completed (network error \(code.rawValue))."
        }
    }
}

struct DurableAuthorization: Equatable, Sendable {
    let bearerToken: String
    let bindingIdentifier: String
    let isDurable: Bool
}

extension DurableAuthorization: RedactedAuthDescribing {}

enum DurableAuthPhase: String, Equatable, Sendable {
    case notConfigured
    case legacy
    case enrollmentCreationPending
    case enrollmentPending
    case active
    case refreshPending
    case reauthenticationRequired
    case incompatible
}

struct DurableAuthPresentation: Equatable, Sendable {
    let phase: DurableAuthPhase
    let title: String
    let detail: String
    let accessExpiresAt: Date?
    let canUpgrade: Bool
    let canReenroll: Bool
    let canRevokeRemotely: Bool
    let canForget: Bool

    static let notConfigured = Self(
        phase: .notConfigured,
        title: "Not connected",
        detail: "Save a bootstrap bearer, then upgrade this Mac to rotating credentials.",
        accessExpiresAt: nil,
        canUpgrade: false,
        canReenroll: false,
        canRevokeRemotely: false,
        canForget: false
    )

    var canConsumeEnrollmentCode: Bool {
        switch phase {
        case .notConfigured, .legacy, .reauthenticationRequired:
            true
        case .enrollmentCreationPending, .enrollmentPending, .active,
             .refreshPending, .incompatible:
            false
        }
    }
}

actor DurableAuthCoordinator {
    static let proactiveRefreshLeadTime: TimeInterval = 2 * 60
    static let clockSkewAllowance: TimeInterval = 5 * 60
    static let accessLifetime: TimeInterval = 15 * 60
    static let refreshIdleLifetime: TimeInterval = 30 * 24 * 60 * 60
    static let absoluteLifetime: TimeInterval = 180 * 24 * 60 * 60

    nonisolated let stateStore: any DurableAuthStateStoring
    nonisolated let legacyStore: any BearerTokenStoring
    private let transport: any DurableAuthHTTPTransport
    private let generator: any DurableCredentialGenerating
    private let now: @Sendable () -> Date
    private var enrollmentCreationInFlight: (
        envelope: DurableAuthEnvelope,
        configurationIdentifier: String,
        task: Task<ActiveDurableAuthState, Error>
    )?
    private var enrollmentInFlight: (
        envelope: DurableAuthEnvelope,
        configurationIdentifier: String,
        task: Task<ActiveDurableAuthState, Error>
    )?
    private var refreshInFlight: (
        envelope: DurableAuthEnvelope,
        configurationIdentifier: String,
        task: Task<ActiveDurableAuthState, Error>
    )?

    init(
        stateStore: any DurableAuthStateStoring = KeychainDurableAuthStateStore(),
        legacyStore: any BearerTokenStoring = KeychainBearerTokenStore(),
        transport: any DurableAuthHTTPTransport = URLSessionDurableAuthHTTPTransport(),
        generator: any DurableCredentialGenerating = SystemDurableCredentialGenerator(),
        now: @escaping @Sendable () -> Date = Date.init
    ) {
        self.stateStore = stateStore
        self.legacyStore = legacyStore
        self.transport = transport
        self.generator = generator
        self.now = now
    }

    nonisolated func hasUsableCredential(boundTo baseURL: DayWeaveAPIBaseURL) -> Bool {
        do {
            if let envelope = try stateStore.loadEnvelope() {
                guard envelope.origin == baseURL.credentialOriginIdentifier else { return false }
                switch envelope.state {
                case .legacy, .active:
                    return true
                case let .refreshPending(pending):
                    return pending.refreshRequest.isBound(
                        to: baseURL,
                        bearer: pending.previous.credentials.refreshToken
                    )
                case .enrollmentCreationPending, .enrollmentPending,
                     .reauthenticationRequired, .incompatible:
                    return false
                }
            }
            return try legacyStore.loadToken(boundTo: baseURL)?.isEmpty == false
        } catch {
            // A Keychain read failure is not evidence that the durable envelope
            // is absent. Never consult or advertise a static fallback then.
            return false
        }
    }

    nonisolated func bindingIdentifier(boundTo baseURL: DayWeaveAPIBaseURL) throws -> String {
        if let envelope = try stateStore.loadEnvelope() {
            guard envelope.origin == baseURL.credentialOriginIdentifier else {
                throw DurableAuthError.originMismatch
            }
            switch envelope.state {
            case let .legacy(value):
                return Self.legacyBinding(token: value.bearerToken)
            case .enrollmentCreationPending:
                throw DurableAuthError.enrollmentRequired
            case let .active(value):
                return Self.deviceBinding(session: value.session)
            case let .refreshPending(value):
                guard value.refreshRequest.isBound(
                    to: baseURL,
                    bearer: value.previous.credentials.refreshToken
                ) else {
                    throw DurableAuthError.originMismatch
                }
                return Self.deviceBinding(session: value.previous.session)
            case .enrollmentPending:
                throw DurableAuthError.enrollmentRequired
            case .reauthenticationRequired:
                throw DurableAuthError.reauthenticationRequired
            case .incompatible:
                throw DurableAuthError.incompatibleState
            }
        }
        guard let token = try legacyStore.loadToken(boundTo: baseURL), !token.isEmpty else {
            throw DurableAuthError.notConfigured
        }
        return Self.legacyBinding(token: token)
    }

    nonisolated func presentation(boundTo baseURL: DayWeaveAPIBaseURL?) -> DurableAuthPresentation {
        let envelope: DurableAuthEnvelope?
        do {
            envelope = try stateStore.loadEnvelope()
        } catch {
            return Self.localStateUnavailablePresentation
        }
        guard let envelope else {
            guard let baseURL else { return .notConfigured }
            do {
                guard try legacyStore.loadToken(boundTo: baseURL)?.isEmpty == false else {
                    return .notConfigured
                }
            } catch {
                return Self.localStateUnavailablePresentation
            }
            return Self.presentation(for: .legacy(.init(bearerToken: "")))
        }
        if let baseURL, envelope.origin != baseURL.credentialOriginIdentifier {
            if Self.isExplicitLocalOnlyTombstone(envelope.state) {
                return .init(
                    phase: .reauthenticationRequired,
                    title: "Ready for a new connection",
                    detail: "Local-only removal was explicitly confirmed. A new origin may now be enrolled.",
                    accessExpiresAt: nil,
                    canUpgrade: false,
                    canReenroll: true,
                    canRevokeRemotely: false,
                    canForget: true
                )
            }
            return .init(
                phase: .incompatible,
                title: "Connection mismatch",
                detail: "Saved authentication may still authorize the prior origin. Restore that address to revoke it, or explicitly confirm local-only removal before replacing it.",
                accessExpiresAt: nil,
                canUpgrade: false,
                canReenroll: false,
                canRevokeRemotely: false,
                canForget: true
            )
        }
        if let baseURL,
           let pendingConfiguration = Self.pendingConfigurationIdentifier(in: envelope.state),
           pendingConfiguration != baseURL.canonicalConfigurationIdentifier {
            return .init(
                phase: .incompatible,
                title: "Connection path mismatch",
                detail: "An exact authentication request is saved for a different server base path. Restore that address to resume it unchanged.",
                accessExpiresAt: nil,
                canUpgrade: false,
                canReenroll: false,
                canRevokeRemotely: false,
                canForget: true
            )
        }
        return Self.presentation(for: envelope.state)
    }

    func installLegacyCredential(
        _ token: String,
        boundTo baseURL: DayWeaveAPIBaseURL
    ) throws {
        guard Self.isValidLegacyToken(token) else {
            throw DurableAuthError.invalidBootstrapCredential
        }
        let current = try loadState()
        if let current {
            if current.origin != baseURL.credentialOriginIdentifier {
                throw Self.wasDurable(current.state)
                    ? DurableAuthError.activeSessionMustBeRevoked
                    : DurableAuthError.originMismatch
            }
            switch current.state {
            case .active, .refreshPending, .enrollmentCreationPending, .enrollmentPending:
                throw DurableAuthError.activeSessionMustBeRevoked
            case .legacy:
                break
            case .reauthenticationRequired:
                throw DurableAuthError.durableSessionRequiresExplicitReenrollment
            case .incompatible:
                throw DurableAuthError.incompatibleState
            }
        }
        let sameOrigin = current?.origin == baseURL.credentialOriginIdentifier
        let clientID = sameOrigin ? current?.clientInstanceID : nil
        let resolvedClientID = try clientID ?? generator.makeUUID()
        let replacement = DurableAuthEnvelope(
            revision: try Self.nextEnvelopeRevision(after: current),
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: resolvedClientID,
            state: .legacy(.init(bearerToken: token))
        )
        guard try stateStore.compareAndSwap(expected: current, replacement: replacement) else {
            throw DurableAuthError.concurrentStateChange
        }
        try removeLegacyDuplicate()
    }

    @discardableResult
    func enroll(
        boundTo baseURL: DayWeaveAPIBaseURL,
        descriptor: DurableAuthClientDescriptor,
        bootstrapToken: String? = nil
    ) async throws -> DurableDeviceSessionMetadata {
        guard descriptor.isValid else { throw DurableAuthError.requestEncodingFailed }
        var current = try loadState()
        if current == nil {
            try adoptLegacyCredentialIfPresent(boundTo: baseURL)
            current = try loadState()
        }
        if let current, current.origin != baseURL.credentialOriginIdentifier {
            guard Self.isExplicitLocalOnlyTombstone(current.state) else {
                throw Self.wasDurable(current.state)
                    ? DurableAuthError.activeSessionMustBeRevoked
                    : DurableAuthError.originMismatch
            }
            guard let bootstrapToken, Self.isValidLegacyToken(bootstrapToken) else {
                throw DurableAuthError.originMismatch
            }
            return try await createAndConsumeEnrollment(
                expected: current,
                baseURL: baseURL,
                bootstrapToken: bootstrapToken,
                clientInstanceID: generator.makeUUID(),
                descriptor: descriptor,
                durableWasPreviouslyActivated: Self.wasDurable(current.state)
            )
        }
        if let current {
            switch current.state {
            case let .enrollmentCreationPending(pending):
                guard pending.descriptor == descriptor,
                      bootstrapToken == nil || bootstrapToken == pending.bootstrapToken else {
                    throw DurableAuthError.concurrentStateChange
                }
                return try await finishEnrollmentCreation(
                    envelope: current,
                    pending: pending,
                    baseURL: baseURL
                ).session
            case let .enrollmentPending(pending):
                guard pending.descriptor == descriptor else {
                    throw DurableAuthError.concurrentStateChange
                }
                return try await consumeEnrollment(
                    envelope: current,
                    pending: pending,
                    baseURL: baseURL
                ).session
            case .refreshPending:
                throw DurableAuthError.activeSessionMustBeRevoked
            case let .legacy(legacy):
                return try await createAndConsumeEnrollment(
                    expected: current,
                    baseURL: baseURL,
                    bootstrapToken: bootstrapToken ?? legacy.bearerToken,
                    clientInstanceID: try requireClientID(current),
                    descriptor: descriptor,
                    durableWasPreviouslyActivated: false
                )
            case .active:
                throw DurableAuthError.activeSessionMustBeRevoked
            case .reauthenticationRequired:
                guard let bootstrapToken, Self.isValidLegacyToken(bootstrapToken) else {
                    throw DurableAuthError.invalidBootstrapCredential
                }
                return try await createAndConsumeEnrollment(
                    expected: current,
                    baseURL: baseURL,
                    bootstrapToken: bootstrapToken,
                    clientInstanceID: try requireClientID(current),
                    descriptor: descriptor,
                    durableWasPreviouslyActivated: true
                )
            case .incompatible:
                throw DurableAuthError.incompatibleState
            }
        }
        guard let bootstrapToken, Self.isValidLegacyToken(bootstrapToken) else {
            throw DurableAuthError.notConfigured
        }
        return try await createAndConsumeEnrollment(
            expected: nil,
            baseURL: baseURL,
            bootstrapToken: bootstrapToken,
            clientInstanceID: generator.makeUUID(),
            descriptor: descriptor,
            durableWasPreviouslyActivated: false
        )
    }

    /// Consumes an enrollment credential minted by an administrator or another
    /// already-authorized DayWeave client. This path never treats the code as a
    /// static bearer and never calls the enrollment-creation endpoint.
    @discardableResult
    func consumeOneTimeEnrollmentCode(
        _ enrollmentCode: String,
        boundTo baseURL: DayWeaveAPIBaseURL,
        descriptor: DurableAuthClientDescriptor
    ) async throws -> DurableDeviceSessionMetadata {
        guard descriptor.isValid,
              Self.isCredential(enrollmentCode, prefix: "dw_en1_") else {
            throw DurableAuthError.invalidEnrollmentCode
        }
        let current = try loadState()
        if let current, current.origin != baseURL.credentialOriginIdentifier {
            guard Self.isExplicitLocalOnlyTombstone(current.state) else {
                throw Self.wasDurable(current.state)
                    ? DurableAuthError.activeSessionMustBeRevoked
                    : DurableAuthError.originMismatch
            }
        }
        if let current, case let .enrollmentPending(pending) = current.state {
            guard current.origin == baseURL.credentialOriginIdentifier,
                  pending.enrollmentToken == enrollmentCode,
                  pending.descriptor == descriptor else {
                throw DurableAuthError.concurrentStateChange
            }
            return try await consumeEnrollment(
                envelope: current,
                pending: pending,
                baseURL: baseURL
            ).session
        }
        if let current, case .enrollmentCreationPending = current.state {
            throw DurableAuthError.activeSessionMustBeRevoked
        }
        if let current, case .refreshPending = current.state {
            throw DurableAuthError.activeSessionMustBeRevoked
        }
        if let current, case .active = current.state {
            throw DurableAuthError.activeSessionMustBeRevoked
        }
        if let current, case .incompatible = current.state {
            throw DurableAuthError.incompatibleState
        }
        let pair = DurableAuthCredentialPair(
            accessToken: try generator.makeCredential(prefix: "dw_da1_"),
            refreshToken: try generator.makeCredential(prefix: "dw_dr1_")
        )
        let materials = [
            Self.credentialMaterial(enrollmentCode),
            Self.credentialMaterial(pair.accessToken),
            Self.credentialMaterial(pair.refreshToken),
        ].compactMap { $0 }
        guard materials.count == 3, Set(materials).count == 3 else {
            throw DurableAuthError.randomnessUnavailable
        }
        let sameOrigin = current?.origin == baseURL.credentialOriginIdentifier
        let proposedSessionID = try generator.makeUUID()
        let consumeBody = try encode(ConsumeEnrollmentRequest(
            sessionID: proposedSessionID,
            accessToken: pair.accessToken,
            refreshToken: pair.refreshToken
        ))
        let consumeRequest = try DurableAuthJournaledRequest.make(
            kind: .consumeEnrollment,
            baseURL: baseURL,
            bearer: enrollmentCode,
            body: consumeBody
        )
        let pending = EnrollmentPendingAuthState(
            enrollmentID: nil,
            enrollmentToken: enrollmentCode,
            enrollmentExpiresAt: nil,
            proposedSessionID: proposedSessionID,
            proposedCredentials: pair,
            descriptor: descriptor,
            preparedAt: now(),
            consumeRequest: consumeRequest,
            durableWasPreviouslyActivated: current.map { Self.wasDurable($0.state) } ?? false
        )
        let replacement = DurableAuthEnvelope(
            revision: try Self.nextEnvelopeRevision(after: current),
            origin: baseURL.credentialOriginIdentifier,
            // A directly supplied code is already bound server-side. A clean
            // Mac adopts that stable client identity from the strict response.
            clientInstanceID: sameOrigin ? current?.clientInstanceID : nil,
            state: .enrollmentPending(pending)
        )
        guard try stateStore.compareAndSwap(expected: current, replacement: replacement) else {
            throw DurableAuthError.concurrentStateChange
        }
        try removeLegacyDuplicate()
        return try await consumeEnrollment(
            envelope: replacement,
            pending: pending,
            baseURL: baseURL
        ).session
    }

    func resumePendingWork(boundTo baseURL: DayWeaveAPIBaseURL) async throws {
        guard let envelope = try loadState() else { return }
        guard envelope.origin == baseURL.credentialOriginIdentifier else {
            throw DurableAuthError.originMismatch
        }
        switch envelope.state {
        case let .enrollmentCreationPending(pending):
            _ = try await finishEnrollmentCreation(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
        case let .enrollmentPending(pending):
            _ = try await consumeEnrollment(envelope: envelope, pending: pending, baseURL: baseURL)
        case .refreshPending:
            _ = try await finishRefresh(envelope: envelope, baseURL: baseURL)
        case .legacy, .active, .reauthenticationRequired, .incompatible:
            return
        }
    }

    func authorization(boundTo baseURL: DayWeaveAPIBaseURL) async throws -> DurableAuthorization {
        var envelope = try loadState()
        if envelope == nil {
            try adoptLegacyCredentialIfPresent(boundTo: baseURL)
            envelope = try loadState()
        }
        guard let envelope else { throw DurableAuthError.notConfigured }
        guard envelope.origin == baseURL.credentialOriginIdentifier else {
            throw DurableAuthError.originMismatch
        }
        switch envelope.state {
        case let .legacy(value):
            return .init(
                bearerToken: value.bearerToken,
                bindingIdentifier: Self.legacyBinding(token: value.bearerToken),
                isDurable: false
            )
        case let .enrollmentCreationPending(pending):
            _ = try await finishEnrollmentCreation(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
            return try await authorization(boundTo: baseURL)
        case let .enrollmentPending(pending):
            _ = try await consumeEnrollment(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
            return try await authorization(boundTo: baseURL)
        case let .active(active):
            if now() >= active.session.absoluteExpiresAt
                || now() >= active.session.refreshIdleExpiresAt
            {
                try transitionToReauthentication(
                    expected: envelope,
                    session: active.session,
                    reason: .expired
                )
                throw DurableAuthError.reauthenticationRequired
            }
            if now().addingTimeInterval(Self.proactiveRefreshLeadTime)
                >= active.session.accessExpiresAt
            {
                let refreshed = try await beginRefresh(
                    envelope: envelope,
                    active: active,
                    baseURL: baseURL
                )
                return Self.authorization(refreshed)
            }
            return Self.authorization(active)
        case .refreshPending:
            _ = try await finishRefresh(envelope: envelope, baseURL: baseURL)
            return try await authorization(boundTo: baseURL)
        case .reauthenticationRequired:
            throw DurableAuthError.reauthenticationRequired
        case .incompatible:
            throw DurableAuthError.incompatibleState
        }
    }

    func recoverFromUnauthorized(
        rejectedBearer: String,
        boundTo baseURL: DayWeaveAPIBaseURL
    ) async throws -> DurableAuthorization {
        guard let envelope = try loadState() else { throw DurableAuthError.notConfigured }
        guard envelope.origin == baseURL.credentialOriginIdentifier else {
            throw DurableAuthError.originMismatch
        }
        switch envelope.state {
        case .legacy:
            throw DurableAuthError.rejected
        case let .enrollmentCreationPending(pending):
            _ = try await finishEnrollmentCreation(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
            return try await authorization(boundTo: baseURL)
        case let .active(active):
            if active.credentials.accessToken != rejectedBearer {
                return Self.authorization(active)
            }
            let refreshed = try await beginRefresh(
                envelope: envelope,
                active: active,
                baseURL: baseURL
            )
            return Self.authorization(refreshed)
        case .refreshPending:
            _ = try await finishRefresh(envelope: envelope, baseURL: baseURL)
            return try await authorization(boundTo: baseURL)
        case let .enrollmentPending(pending):
            _ = try await consumeEnrollment(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
            return try await authorization(boundTo: baseURL)
        case .reauthenticationRequired:
            throw DurableAuthError.reauthenticationRequired
        case .incompatible:
            throw DurableAuthError.incompatibleState
        }
    }

    /// Retires only the exact durable access lease that was definitively
    /// rejected after a refresh-and-replay. A newer active state or an
    /// in-progress rotation is never overwritten.
    func retireDefinitivelyRejectedAuthorization(
        _ rejected: DurableAuthorization,
        boundTo baseURL: DayWeaveAPIBaseURL
    ) throws {
        guard rejected.isDurable else { throw DurableAuthError.rejected }
        guard let envelope = try loadState() else { throw DurableAuthError.notConfigured }
        guard envelope.origin == baseURL.credentialOriginIdentifier else {
            throw DurableAuthError.originMismatch
        }
        guard case let .active(active) = envelope.state,
              active.credentials.accessToken == rejected.bearerToken,
              Self.deviceBinding(session: active.session)
                == rejected.bindingIdentifier else {
            if case .reauthenticationRequired = envelope.state {
                throw DurableAuthError.reauthenticationRequired
            }
            throw DurableAuthError.concurrentStateChange
        }
        try transitionToReauthentication(
            expected: envelope,
            session: active.session,
            reason: .rejected
        )
    }

    func revokeAndForget(boundTo baseURL: DayWeaveAPIBaseURL) async throws {
        var authorization = try await authorization(boundTo: baseURL)
        guard authorization.isDurable else {
            throw DurableAuthError.remoteRevocationUnavailable
        }
        var expected = try loadState()
        guard let firstExpected = expected,
              let sessionID = Self.sessionID(in: firstExpected.state) else {
            throw DurableAuthError.remoteRevocationUnavailable
        }
        var response = try await send(
            method: "DELETE",
            path: ["v1", "auth", "sessions", sessionID.uuidString.lowercased()],
            baseURL: baseURL,
            bearer: authorization.bearerToken,
            body: nil
        )
        if response.statusCode == 401 {
            guard Self.isDefinitiveUnauthorized(response) else {
                throw DurableAuthError.invalidResponse
            }
            authorization = try await recoverFromUnauthorized(
                rejectedBearer: authorization.bearerToken,
                boundTo: baseURL
            )
            guard authorization.isDurable else {
                throw DurableAuthError.remoteRevocationUnavailable
            }
            expected = try loadState()
            guard let latest = expected, Self.sessionID(in: latest.state) == sessionID else {
                throw DurableAuthError.concurrentStateChange
            }
            response = try await send(
                method: "DELETE",
                path: ["v1", "auth", "sessions", sessionID.uuidString.lowercased()],
                baseURL: baseURL,
                bearer: authorization.bearerToken,
                body: nil
            )
            if response.statusCode == 401 {
                guard Self.isDefinitiveUnauthorized(response) else {
                    throw DurableAuthError.invalidResponse
                }
                try retireDefinitivelyRejectedAuthorization(
                    authorization,
                    boundTo: baseURL
                )
                throw DurableAuthError.reauthenticationRequired
            }
        }
        guard response.statusCode == 204 else { throw try mapFailure(response) }
        try validateNoStore(response, requiresJSON: false)
        guard response.body.isEmpty, let expected else {
            throw DurableAuthError.invalidResponse
        }
        // Clear any obsolete pre-envelope copy first. If this fails, retain
        // the authoritative envelope even though the server has revoked it.
        try removeLegacyDuplicate()
        guard try stateStore.compareAndSwap(expected: expected, replacement: nil) else {
            throw DurableAuthError.concurrentStateChange
        }
    }

    /// Explicit recovery-only path. The caller must present a warning that
    /// this does not revoke server-side session or bootstrap authority.
    func confirmLocalOnlyForget() throws {
        let current = try loadState()
        // Remove any pre-envelope duplicate before changing the authoritative
        // state. If Keychain refuses, leave the envelope untouched and fail.
        try removeLegacyDuplicate()
        let replacement: DurableAuthEnvelope?
        if let current, Self.wasDurable(current.state), current.origin != nil {
            replacement = try current.replacingState(.reauthenticationRequired(.init(
                clientInstanceID: current.clientInstanceID,
                previousSessionID: Self.sessionID(in: current.state),
                reason: .explicitlyDisconnected,
                detectedAt: now()
            )))
        } else {
            replacement = nil
        }
        guard try stateStore.compareAndSwap(expected: current, replacement: replacement) else {
            throw DurableAuthError.concurrentStateChange
        }
    }

    private func createAndConsumeEnrollment(
        expected: DurableAuthEnvelope?,
        baseURL: DayWeaveAPIBaseURL,
        bootstrapToken: String,
        clientInstanceID: UUID,
        descriptor: DurableAuthClientDescriptor,
        durableWasPreviouslyActivated: Bool
    ) async throws -> DurableDeviceSessionMetadata {
        guard Self.isValidLegacyToken(bootstrapToken) else {
            throw DurableAuthError.invalidBootstrapCredential
        }
        let proposedEnrollmentID = try generator.makeUUID()
        let proposedEnrollmentToken = try generator.makeCredential(prefix: "dw_en1_")
        let proposedSessionID = try generator.makeUUID()
        let proposedCredentials = DurableAuthCredentialPair(
            accessToken: try generator.makeCredential(prefix: "dw_da1_"),
            refreshToken: try generator.makeCredential(prefix: "dw_dr1_")
        )
        let materials = [
            Self.credentialMaterial(proposedEnrollmentToken),
            Self.credentialMaterial(proposedCredentials.accessToken),
            Self.credentialMaterial(proposedCredentials.refreshToken),
        ].compactMap { $0 }
        guard materials.count == 3, Set(materials).count == 3 else {
            throw DurableAuthError.randomnessUnavailable
        }
        let requestBody = CreateEnrollmentRequest(
            id: proposedEnrollmentID,
            enrollmentToken: proposedEnrollmentToken,
            clientInstanceID: clientInstanceID,
            clientKind: "macos",
            deviceLabel: descriptor.deviceLabel,
            scopes: descriptor.scopes,
            clientContractVersion: DurableAuthClientDescriptor.contractVersion,
            clientVersion: descriptor.clientVersion,
            clientCapabilities: descriptor.clientCapabilities
        )
        let encodedBody = try encode(requestBody)
        let creationRequest = try DurableAuthJournaledRequest.make(
            kind: .createEnrollment,
            baseURL: baseURL,
            bearer: bootstrapToken,
            body: encodedBody
        )
        let pending = EnrollmentCreationPendingAuthState(
            bootstrapToken: bootstrapToken,
            proposedEnrollmentID: proposedEnrollmentID,
            proposedEnrollmentToken: proposedEnrollmentToken,
            proposedSessionID: proposedSessionID,
            proposedCredentials: proposedCredentials,
            descriptor: descriptor,
            preparedAt: now(),
            creationRequest: creationRequest,
            durableWasPreviouslyActivated: durableWasPreviouslyActivated
        )
        let replacement = DurableAuthEnvelope(
            revision: try Self.nextEnvelopeRevision(after: expected),
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientInstanceID,
            state: .enrollmentCreationPending(pending)
        )
        guard try stateStore.compareAndSwap(expected: expected, replacement: replacement) else {
            throw DurableAuthError.concurrentStateChange
        }
        try removeLegacyDuplicate()
        return try await finishEnrollmentCreation(
            envelope: replacement,
            pending: pending,
            baseURL: baseURL
        ).session
    }

    private func finishEnrollmentCreation(
        envelope: DurableAuthEnvelope,
        pending: EnrollmentCreationPendingAuthState,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard pending.creationRequest.isBound(to: baseURL, bearer: pending.bootstrapToken) else {
            throw DurableAuthError.originMismatch
        }
        let configurationIdentifier = pending.creationRequest.configurationIdentifier
        if let inFlight = enrollmentCreationInFlight,
           inFlight.envelope == envelope,
           inFlight.configurationIdentifier == configurationIdentifier {
            return try await inFlight.task.value
        }
        if let latest = try loadState(), latest != envelope,
           let resolved = try await resolveEnrollmentCreationProgress(
               latest,
               pending: pending,
               expectedClientInstanceID: envelope.clientInstanceID,
               baseURL: baseURL
           ) {
            return resolved
        }
        let task = Task {
            try await self.performEnrollmentCreation(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
        }
        enrollmentCreationInFlight = (envelope, configurationIdentifier, task)
        do {
            let result = try await task.value
            if enrollmentCreationInFlight?.envelope == envelope,
               enrollmentCreationInFlight?.configurationIdentifier == configurationIdentifier {
                enrollmentCreationInFlight = nil
            }
            return result
        } catch {
            if enrollmentCreationInFlight?.envelope == envelope,
               enrollmentCreationInFlight?.configurationIdentifier == configurationIdentifier {
                enrollmentCreationInFlight = nil
            }
            throw error
        }
    }

    private func performEnrollmentCreation(
        envelope: DurableAuthEnvelope,
        pending: EnrollmentCreationPendingAuthState,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard envelope.revision < UInt64.max else {
            throw DurableAuthStateStoreError.revisionOverflow
        }
        guard envelope.origin == baseURL.credentialOriginIdentifier,
              pending.creationRequest.isBound(to: baseURL, bearer: pending.bootstrapToken) else {
            throw DurableAuthError.originMismatch
        }
        let response = try await send(
            pending.creationRequest,
            bearer: pending.bootstrapToken
        )
        if response.statusCode == 401 {
            guard Self.isDefinitiveUnauthorized(response) else {
                throw DurableAuthError.invalidResponse
            }
            try transitionToIncompatible(
                expected: envelope,
                reason: "enrollment_creation_rejected",
                recovery: .enrollmentCreation(pending)
            )
            throw DurableAuthError.rejected
        }
        guard response.statusCode == 200 || response.statusCode == 201 else {
            if Self.isDeterministicClientError(response.statusCode) {
                _ = try mapFailure(response)
                try transitionToIncompatible(
                    expected: envelope,
                    reason: "enrollment_creation_rejected",
                    recovery: .enrollmentCreation(pending)
                )
            }
            throw try mapFailure(response)
        }

        do {
            try validateNoStore(response)
            try validateObjectKeys(
                response.body,
                exactly: [
                    "id", "enrollment_token", "expires_at",
                    "client_contract_version", "replayed",
                ]
            )
            let issued = try decode(DeviceEnrollmentResponse.self, from: response.body)
            let receivedAt = now()
            guard (response.statusCode == 200) == issued.replayed,
                  issued.id == pending.proposedEnrollmentID,
                  issued.enrollmentToken == pending.proposedEnrollmentToken,
                  issued.clientContractVersion == DurableAuthClientDescriptor.contractVersion,
                  issued.expiresAt > receivedAt.addingTimeInterval(-Self.clockSkewAllowance),
                  issued.expiresAt
                    <= receivedAt.addingTimeInterval(
                        10 * 60 + Self.clockSkewAllowance
                    ) else {
                throw DurableAuthError.invalidResponse
            }
            let originalBaseURL = try pending.creationRequest.boundBaseURL()
            let consumeBody = try encode(ConsumeEnrollmentRequest(
                sessionID: pending.proposedSessionID,
                accessToken: pending.proposedCredentials.accessToken,
                refreshToken: pending.proposedCredentials.refreshToken
            ))
            let consumeRequest = try DurableAuthJournaledRequest.make(
                kind: .consumeEnrollment,
                baseURL: originalBaseURL,
                bearer: pending.proposedEnrollmentToken,
                body: consumeBody
            )
            let enrollmentPending = EnrollmentPendingAuthState(
                enrollmentID: pending.proposedEnrollmentID,
                enrollmentToken: pending.proposedEnrollmentToken,
                enrollmentExpiresAt: issued.expiresAt,
                proposedSessionID: pending.proposedSessionID,
                proposedCredentials: pending.proposedCredentials,
                descriptor: pending.descriptor,
                preparedAt: pending.preparedAt,
                consumeRequest: consumeRequest,
                durableWasPreviouslyActivated: pending.durableWasPreviouslyActivated
            )
            let replacement = try envelope.replacingState(.enrollmentPending(enrollmentPending))
            guard try stateStore.compareAndSwap(expected: envelope, replacement: replacement) else {
                guard let latest = try loadState(),
                      let resolved = try await resolveEnrollmentCreationProgress(
                          latest,
                          pending: pending,
                          expectedClientInstanceID: envelope.clientInstanceID,
                          baseURL: baseURL
                      ) else {
                    throw DurableAuthError.concurrentStateChange
                }
                return resolved
            }
            return try await consumeEnrollment(
                envelope: replacement,
                pending: enrollmentPending,
                baseURL: originalBaseURL
            )
        } catch let error as DurableAuthError {
            if error == .invalidResponse {
                // Once the creation response has been accepted, this method
                // advances to an enrollment-consumption journal. Validation
                // failures from that nested operation quarantine that newer
                // journal themselves and must not be overwritten by a stale
                // creation-state CAS.
                if try loadState() == envelope {
                    try transitionToIncompatible(
                        expected: envelope,
                        reason: "enrollment_creation_response_mismatch",
                        recovery: .enrollmentCreation(pending)
                    )
                }
            }
            throw error
        } catch {
            if try loadState() == envelope {
                try transitionToIncompatible(
                    expected: envelope,
                    reason: "enrollment_creation_response_mismatch",
                    recovery: .enrollmentCreation(pending)
                )
            }
            throw DurableAuthError.invalidResponse
        }
    }

    private func resolveEnrollmentCreationProgress(
        _ latest: DurableAuthEnvelope,
        pending: EnrollmentCreationPendingAuthState,
        expectedClientInstanceID: UUID?,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState? {
        guard latest.origin == baseURL.credentialOriginIdentifier,
              latest.clientInstanceID == expectedClientInstanceID,
              expectedClientInstanceID != nil else { return nil }
        switch latest.state {
        case let .enrollmentPending(enrollment)
            where enrollment.enrollmentID == pending.proposedEnrollmentID
                && enrollment.enrollmentToken == pending.proposedEnrollmentToken
                && enrollment.proposedSessionID == pending.proposedSessionID
                && enrollment.proposedCredentials == pending.proposedCredentials:
            return try await consumeEnrollment(
                envelope: latest,
                pending: enrollment,
                baseURL: baseURL
            )
        case let .active(active)
            where active.session.id == pending.proposedSessionID
                && active.credentials == pending.proposedCredentials:
            return active
        default:
            return nil
        }
    }

    private func consumeEnrollment(
        envelope: DurableAuthEnvelope,
        pending: EnrollmentPendingAuthState,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard pending.consumeRequest.isBound(to: baseURL, bearer: pending.enrollmentToken) else {
            throw DurableAuthError.originMismatch
        }
        let configurationIdentifier = pending.consumeRequest.configurationIdentifier
        if let inFlight = enrollmentInFlight,
           inFlight.envelope == envelope,
           inFlight.configurationIdentifier == configurationIdentifier {
            return try await inFlight.task.value
        }
        if let latest = try loadState(), latest != envelope,
           latest.origin == envelope.origin,
           case let .active(active) = latest.state,
           active.session.id == pending.proposedSessionID,
           active.credentials == pending.proposedCredentials {
            return active
        }
        let task = Task {
            try await self.performEnrollmentConsumption(
                envelope: envelope,
                pending: pending,
                baseURL: baseURL
            )
        }
        enrollmentInFlight = (envelope, configurationIdentifier, task)
        do {
            let result = try await task.value
            if enrollmentInFlight?.envelope == envelope,
               enrollmentInFlight?.configurationIdentifier == configurationIdentifier {
                enrollmentInFlight = nil
            }
            return result
        } catch {
            if enrollmentInFlight?.envelope == envelope,
               enrollmentInFlight?.configurationIdentifier == configurationIdentifier {
                enrollmentInFlight = nil
            }
            throw error
        }
    }

    private func performEnrollmentConsumption(
        envelope: DurableAuthEnvelope,
        pending: EnrollmentPendingAuthState,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard envelope.revision < UInt64.max else {
            throw DurableAuthStateStoreError.revisionOverflow
        }
        guard envelope.origin == baseURL.credentialOriginIdentifier else {
            throw DurableAuthError.originMismatch
        }
        let expectedBody = try encode(ConsumeEnrollmentRequest(
            sessionID: pending.proposedSessionID,
            accessToken: pending.proposedCredentials.accessToken,
            refreshToken: pending.proposedCredentials.refreshToken
        ))
        guard pending.consumeRequest.kind == .consumeEnrollment,
              pending.consumeRequest.isBound(to: baseURL, bearer: pending.enrollmentToken),
              pending.consumeRequest.body == expectedBody else {
            throw DurableAuthError.incompatibleState
        }
        let response = try await send(
            pending.consumeRequest,
            bearer: pending.enrollmentToken
        )
        if response.statusCode == 401 {
            guard Self.isDefinitiveUnauthorized(response) else {
                throw DurableAuthError.invalidResponse
            }
            try transitionToReauthentication(
                expected: envelope,
                sessionID: pending.proposedSessionID,
                reason: .rejected
            )
            throw DurableAuthError.reauthenticationRequired
        }
        guard response.statusCode == 200 || response.statusCode == 201 else {
            if Self.isDeterministicClientError(response.statusCode) {
                _ = try mapFailure(response)
                try transitionToIncompatible(
                    expected: envelope,
                    reason: "enrollment_contract_rejected",
                    recovery: .enrollment(pending)
                )
            }
            throw try mapFailure(response)
        }
        do {
            let mutation = try decodeMutation(response)
            guard (response.statusCode == 200) == mutation.replayed,
                  validateEnrollmentSession(
                    mutation.session,
                    pending: pending,
                    clientInstanceID: envelope.clientInstanceID,
                    receivedAt: now(),
                    replayed: mutation.replayed
                  ) else {
                throw DurableAuthError.invalidResponse
            }
            let active = ActiveDurableAuthState(
                session: mutation.session,
                credentials: pending.proposedCredentials
            )
            let replacement = DurableAuthEnvelope(
                revision: envelope.revision + 1,
                origin: envelope.origin,
                clientInstanceID: mutation.session.clientInstanceID,
                state: .active(active)
            )
            guard try stateStore.compareAndSwap(expected: envelope, replacement: replacement) else {
                return try resolveCommittedActive(
                    sessionID: mutation.session.id,
                    credentials: pending.proposedCredentials
                )
            }
            return active
        } catch let error as DurableAuthError {
            if error == .invalidResponse {
                try transitionToIncompatible(
                    expected: envelope,
                    reason: "enrollment_response_mismatch",
                    recovery: .enrollment(pending)
                )
            }
            throw error
        } catch {
            try transitionToIncompatible(
                expected: envelope,
                reason: "enrollment_response_mismatch",
                recovery: .enrollment(pending)
            )
            throw DurableAuthError.invalidResponse
        }
    }

    private func beginRefresh(
        envelope: DurableAuthEnvelope,
        active: ActiveDurableAuthState,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard envelope.revision < UInt64.max,
              active.session.revision < UInt64.max else {
            throw DurableAuthStateStoreError.revisionOverflow
        }
        let next = DurableAuthCredentialPair(
            accessToken: try generator.makeCredential(prefix: "dw_da1_"),
            refreshToken: try generator.makeCredential(prefix: "dw_dr1_")
        )
        let materials = [
            Self.credentialMaterial(active.credentials.accessToken),
            Self.credentialMaterial(active.credentials.refreshToken),
            Self.credentialMaterial(next.accessToken),
            Self.credentialMaterial(next.refreshToken),
        ]
        guard materials.compactMap({ $0 }).count == 4,
              Set(materials.compactMap { $0 }).count == 4 else {
            throw DurableAuthError.randomnessUnavailable
        }
        let refreshBody = try encode(RefreshRequest(
            nextAccessToken: next.accessToken,
            nextRefreshToken: next.refreshToken
        ))
        let refreshRequest = try DurableAuthJournaledRequest.make(
            kind: .refreshSession,
            baseURL: baseURL,
            bearer: active.credentials.refreshToken,
            body: refreshBody
        )
        let pending = RefreshPendingAuthState(
            previous: active,
            nextCredentials: next,
            preparedAt: now(),
            refreshRequest: refreshRequest
        )
        let replacement = try envelope.replacingState(.refreshPending(pending))
        guard try stateStore.compareAndSwap(expected: envelope, replacement: replacement) else {
            let latest = try loadState()
            if let latest, latest.origin == envelope.origin,
               case let .refreshPending(latestPending) = latest.state,
               latestPending.previous == active {
                return try await finishRefresh(envelope: latest, baseURL: baseURL)
            }
            if let latest, case let .active(current) = latest.state,
               current.session.id == active.session.id,
               current.session.revision > active.session.revision {
                return current
            }
            throw DurableAuthError.concurrentStateChange
        }
        return try await finishRefresh(envelope: replacement, baseURL: baseURL)
    }

    private func finishRefresh(
        envelope: DurableAuthEnvelope,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard case let .refreshPending(pending) = envelope.state,
              pending.refreshRequest.isBound(
                  to: baseURL,
                  bearer: pending.previous.credentials.refreshToken
              ) else {
            throw DurableAuthError.originMismatch
        }
        let configurationIdentifier = pending.refreshRequest.configurationIdentifier
        if let inFlight = refreshInFlight,
           inFlight.envelope == envelope,
           inFlight.configurationIdentifier == configurationIdentifier {
            return try await inFlight.task.value
        }
        if let latest = try loadState(), latest != envelope,
           latest.origin == envelope.origin,
           latest.clientInstanceID == envelope.clientInstanceID,
           case let .refreshPending(pending) = envelope.state,
           case let .active(active) = latest.state,
           active.session.id == pending.previous.session.id,
           active.credentials == pending.nextCredentials {
            return active
        }
        let task = Task {
            try await self.performRefresh(envelope: envelope, baseURL: baseURL)
        }
        refreshInFlight = (envelope, configurationIdentifier, task)
        do {
            let result = try await task.value
            if refreshInFlight?.envelope == envelope,
               refreshInFlight?.configurationIdentifier == configurationIdentifier {
                refreshInFlight = nil
            }
            return result
        } catch {
            if refreshInFlight?.envelope == envelope,
               refreshInFlight?.configurationIdentifier == configurationIdentifier {
                refreshInFlight = nil
            }
            throw error
        }
    }

    private func performRefresh(
        envelope: DurableAuthEnvelope,
        baseURL: DayWeaveAPIBaseURL
    ) async throws -> ActiveDurableAuthState {
        guard case let .refreshPending(pending) = envelope.state else {
            throw DurableAuthError.concurrentStateChange
        }
        guard envelope.origin == baseURL.credentialOriginIdentifier else {
            throw DurableAuthError.originMismatch
        }
        guard envelope.revision < UInt64.max,
              pending.previous.session.revision < UInt64.max else {
            throw DurableAuthStateStoreError.revisionOverflow
        }
        let expectedBody = try encode(RefreshRequest(
            nextAccessToken: pending.nextCredentials.accessToken,
            nextRefreshToken: pending.nextCredentials.refreshToken
        ))
        guard pending.refreshRequest.kind == .refreshSession,
              pending.refreshRequest.isBound(
                  to: baseURL,
                  bearer: pending.previous.credentials.refreshToken
              ),
              pending.refreshRequest.body == expectedBody else {
            throw DurableAuthError.incompatibleState
        }
        let response = try await send(
            pending.refreshRequest,
            bearer: pending.previous.credentials.refreshToken
        )
        if response.statusCode == 401 {
            guard Self.isDefinitiveUnauthorized(response) else {
                throw DurableAuthError.invalidResponse
            }
            try transitionToReauthentication(
                expected: envelope,
                session: pending.previous.session,
                reason: .rejected
            )
            throw DurableAuthError.reauthenticationRequired
        }
        guard response.statusCode == 200 else {
            if Self.isDeterministicClientError(response.statusCode) {
                _ = try mapFailure(response)
                try transitionToIncompatible(
                    expected: envelope,
                    reason: "refresh_contract_rejected",
                    recovery: .refresh(pending)
                )
            }
            throw try mapFailure(response)
        }
        do {
            let mutation = try decodeMutation(response)
            guard validateRefreshSession(
                mutation.session,
                previous: pending.previous.session,
                preparedAt: pending.preparedAt,
                receivedAt: now(),
                replayed: mutation.replayed
            ) else {
                throw DurableAuthError.invalidResponse
            }
            let active = ActiveDurableAuthState(
                session: mutation.session,
                credentials: pending.nextCredentials
            )
            let replacement = try envelope.replacingState(.active(active))
            guard try stateStore.compareAndSwap(expected: envelope, replacement: replacement) else {
                return try resolveCommittedActive(
                    sessionID: mutation.session.id,
                    credentials: pending.nextCredentials
                )
            }
            return active
        } catch let error as DurableAuthError {
            if error == .invalidResponse {
                try transitionToIncompatible(
                    expected: envelope,
                    reason: "refresh_response_mismatch",
                    recovery: .refresh(pending)
                )
            }
            throw error
        } catch {
            try transitionToIncompatible(
                expected: envelope,
                reason: "refresh_response_mismatch",
                recovery: .refresh(pending)
            )
            throw DurableAuthError.invalidResponse
        }
    }

    private func resolveCommittedActive(
        sessionID: UUID,
        credentials: DurableAuthCredentialPair
    ) throws -> ActiveDurableAuthState {
        guard let latest = try loadState(), case let .active(active) = latest.state,
              active.session.id == sessionID,
              active.credentials == credentials else {
            throw DurableAuthError.concurrentStateChange
        }
        return active
    }

    private func transitionToReauthentication(
        expected: DurableAuthEnvelope,
        session: DurableDeviceSessionMetadata,
        reason: DurableReauthenticationReason
    ) throws {
        try transitionToReauthentication(
            expected: expected,
            sessionID: session.id,
            reason: reason
        )
    }

    private func transitionToReauthentication(
        expected: DurableAuthEnvelope,
        sessionID: UUID?,
        reason: DurableReauthenticationReason
    ) throws {
        let replacement = try expected.replacingState(.reauthenticationRequired(.init(
            clientInstanceID: expected.clientInstanceID,
            previousSessionID: sessionID,
            reason: reason,
            detectedAt: now()
        )))
        guard try stateStore.compareAndSwap(expected: expected, replacement: replacement) else {
            guard try loadState() == replacement else {
                throw DurableAuthError.concurrentStateChange
            }
            return
        }
    }

    private func transitionToIncompatible(
        expected: DurableAuthEnvelope,
        reason: String,
        recovery: IncompatibleAuthRecovery
    ) throws {
        let replacement = try expected.replacingState(.incompatible(.init(
            reasonCode: reason,
            storedSchemaVersion: DurableAuthEnvelope.currentSchemaVersion,
            detectedAt: now(),
            recovery: recovery
        )))
        guard try stateStore.compareAndSwap(expected: expected, replacement: replacement) else {
            guard try loadState() == replacement else {
                throw DurableAuthError.concurrentStateChange
            }
            return
        }
    }

    private func adoptLegacyCredentialIfPresent(boundTo baseURL: DayWeaveAPIBaseURL) throws {
        guard try stateStore.loadEnvelope() == nil,
              let token = try legacyStore.loadToken(boundTo: baseURL),
              Self.isValidLegacyToken(token) else { return }
        let clientInstanceID = try generator.makeUUID()
        let envelope = DurableAuthEnvelope(
            revision: 0,
            origin: baseURL.credentialOriginIdentifier,
            clientInstanceID: clientInstanceID,
            state: .legacy(.init(bearerToken: token))
        )
        guard try stateStore.compareAndSwap(expected: nil, replacement: envelope) else { return }
        try removeLegacyDuplicate()
    }

    private func loadState() throws -> DurableAuthEnvelope? {
        do {
            let envelope = try stateStore.loadEnvelope()
            if envelope != nil { try removeLegacyDuplicate() }
            return envelope
        } catch let error as DurableAuthError {
            throw error
        } catch {
            throw DurableAuthError.localStateUnavailable
        }
    }

    private func removeLegacyDuplicate() throws {
        do {
            try legacyStore.deleteCredential()
        } catch {
            throw DurableAuthError.localStateUnavailable
        }
    }

    private func requireClientID(_ envelope: DurableAuthEnvelope) throws -> UUID {
        guard let id = envelope.clientInstanceID else { throw DurableAuthError.incompatibleState }
        return id
    }

    private func send(
        _ journal: DurableAuthJournaledRequest,
        bearer: String
    ) async throws -> DurableAuthHTTPResponse {
        try await transport.send(journal.makeURLRequest(bearer: bearer))
    }

    private func send(
        method: String,
        path: [String],
        baseURL: DayWeaveAPIBaseURL,
        bearer: String,
        body: Data?
    ) async throws -> DurableAuthHTTPResponse {
        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: path)
        } catch {
            throw DurableAuthError.requestEncodingFailed
        }
        var request = URLRequest(url: endpoint)
        request.httpMethod = method
        request.httpBody = body
        request.timeoutInterval = 20
        request.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        request.setValue("no-cache", forHTTPHeaderField: "Pragma")
        return try await transport.send(request)
    }

    private func mapFailure(_ response: DurableAuthHTTPResponse) throws -> DurableAuthError {
        if Self.isRetryableStatus(response.statusCode) {
            return .retryableServer(statusCode: response.statusCode)
        }
        if response.statusCode == 401 {
            guard Self.isDefinitiveUnauthorized(response) else {
                throw DurableAuthError.invalidResponse
            }
            return .rejected
        }
        if Self.isDeterministicClientError(response.statusCode) {
            do {
                _ = try DayWeaveAuthResponseContract.validateDeterministicError(
                    statusCode: response.statusCode,
                    headers: response.headers,
                    body: response.body
                )
            } catch {
                throw DurableAuthError.invalidResponse
            }
        }
        return .invalidResponse
    }

    private static func isRetryableStatus(_ statusCode: Int) -> Bool {
        statusCode == 408 || statusCode == 425 || statusCode == 429
            || (500...599).contains(statusCode)
    }

    private static func isDeterministicClientError(_ statusCode: Int) -> Bool {
        statusCode == 400 || statusCode == 403 || statusCode == 404
            || statusCode == 409 || statusCode == 422
    }

    private static func isDefinitiveUnauthorized(_ response: DurableAuthHTTPResponse) -> Bool {
        DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
            statusCode: response.statusCode,
            headers: response.headers,
            body: response.body
        )
    }

    private func decodeMutation(_ response: DurableAuthHTTPResponse) throws
        -> DeviceSessionMutationResponse
    {
        try validateNoStore(response)
        try validateObjectKeys(response.body, exactly: ["session", "replayed"])
        try validateNestedObjectKeys(
            response.body,
            key: "session",
            exactly: [
                "id", "client_instance_id", "client_kind", "device_label", "scopes",
                "client_contract_version", "client_version", "client_capabilities",
                "created_at", "last_seen_at", "credential_issued_at", "access_expires_at",
                "refresh_idle_expires_at", "absolute_expires_at", "revision",
            ]
        )
        return try decode(DeviceSessionMutationResponse.self, from: response.body)
    }

    private func validateNoStore(
        _ response: DurableAuthHTTPResponse,
        requiresJSON: Bool = true
    ) throws {
        try DayWeaveAuthResponseContract.validateNoStore(
            headers: response.headers,
            requiresJSON: requiresJSON
        )
    }

    private func validateObjectKeys(_ data: Data, exactly keys: Set<String>) throws {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(object.keys) == keys else { throw DurableAuthError.invalidResponse }
    }

    private func validateNestedObjectKeys(
        _ data: Data,
        key: String,
        exactly keys: Set<String>
    ) throws {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let nested = object[key] as? [String: Any], Set(nested.keys) == keys else {
            throw DurableAuthError.invalidResponse
        }
    }

    private func validateEnrollmentSession(
        _ session: DurableDeviceSessionMetadata,
        pending: EnrollmentPendingAuthState,
        clientInstanceID: UUID?,
        receivedAt: Date,
        replayed: Bool
    ) -> Bool {
        session.id == pending.proposedSessionID
            && (clientInstanceID == nil || session.clientInstanceID == clientInstanceID)
            && session.clientKind == "macos"
            && session.deviceLabel == pending.descriptor.deviceLabel
            && session.scopes == pending.descriptor.scopes
            && session.clientContractVersion == DurableAuthClientDescriptor.contractVersion
            && session.clientVersion == pending.descriptor.clientVersion
            && session.clientCapabilities == pending.descriptor.clientCapabilities
            && session.revision == 1
            && validateSessionTimestamps(
                session,
                preparedAt: pending.preparedAt,
                receivedAt: receivedAt,
                replayed: replayed
            )
    }

    private func validateRefreshSession(
        _ session: DurableDeviceSessionMetadata,
        previous: DurableDeviceSessionMetadata,
        preparedAt: Date,
        receivedAt: Date,
        replayed: Bool
    ) -> Bool {
        session.id == previous.id
            && session.clientInstanceID == previous.clientInstanceID
            && session.clientKind == previous.clientKind
            && session.deviceLabel == previous.deviceLabel
            && session.scopes == previous.scopes
            && session.clientContractVersion == previous.clientContractVersion
            && session.clientVersion == previous.clientVersion
            && session.clientCapabilities == previous.clientCapabilities
            && session.createdAt == previous.createdAt
            && session.absoluteExpiresAt == previous.absoluteExpiresAt
            && previous.revision < UInt64.max
            && session.revision == previous.revision + 1
            && session.lastSeenAt >= previous.lastSeenAt
            && session.credentialIssuedAt >= previous.credentialIssuedAt
            && validateSessionTimestamps(
                session,
                preparedAt: preparedAt,
                receivedAt: receivedAt,
                replayed: replayed
            )
    }

    private func validateSessionTimestamps(
        _ session: DurableDeviceSessionMetadata,
        preparedAt: Date,
        receivedAt: Date,
        replayed: Bool
    ) -> Bool {
        let skew = Self.clockSkewAllowance
        let finiteDates = [
            session.createdAt, session.lastSeenAt, session.credentialIssuedAt,
            session.accessExpiresAt, session.refreshIdleExpiresAt,
            session.absoluteExpiresAt, preparedAt, receivedAt,
        ].allSatisfy { $0.timeIntervalSinceReferenceDate.isFinite }
        return finiteDates
            && session.createdAt <= session.credentialIssuedAt
            && session.credentialIssuedAt <= session.lastSeenAt
            && session.credentialIssuedAt < session.accessExpiresAt
            && session.accessExpiresAt.timeIntervalSince(session.credentialIssuedAt)
                <= Self.accessLifetime + 1
            && session.credentialIssuedAt < session.refreshIdleExpiresAt
            && session.refreshIdleExpiresAt.timeIntervalSince(session.credentialIssuedAt)
                <= Self.refreshIdleLifetime + 1
            && session.credentialIssuedAt < session.absoluteExpiresAt
            && session.absoluteExpiresAt.timeIntervalSince(session.createdAt)
                <= Self.absoluteLifetime + 1
            && session.accessExpiresAt <= session.absoluteExpiresAt
            && session.refreshIdleExpiresAt <= session.absoluteExpiresAt
            && session.refreshIdleExpiresAt > receivedAt
            && session.absoluteExpiresAt > receivedAt
            && session.credentialIssuedAt
                >= preparedAt.addingTimeInterval(-skew)
            && session.credentialIssuedAt <= receivedAt.addingTimeInterval(skew)
            && session.lastSeenAt <= receivedAt.addingTimeInterval(skew)
            && (replayed
                || (session.credentialIssuedAt >= receivedAt.addingTimeInterval(-skew)
                    && session.accessExpiresAt > receivedAt))
            && receivedAt.timeIntervalSince(session.createdAt)
                <= Self.absoluteLifetime + Self.clockSkewAllowance
    }

    private func encode(_ value: some Encodable) throws -> Data {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            return try encoder.encode(value)
        } catch {
            throw DurableAuthError.requestEncodingFailed
        }
    }

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            let fractional = ISO8601DateFormatter()
            fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            if let date = fractional.date(from: value) { return date }
            let whole = ISO8601DateFormatter()
            whole.formatOptions = [.withInternetDateTime]
            guard let date = whole.date(from: value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Expected RFC 3339"
                )
            }
            return date
        }
        return try decoder.decode(type, from: data)
    }

    private static func authorization(_ active: ActiveDurableAuthState) -> DurableAuthorization {
        .init(
            bearerToken: active.credentials.accessToken,
            bindingIdentifier: deviceBinding(session: active.session),
            isDurable: true
        )
    }

    private static func legacyBinding(token: String) -> String {
        let digest = SHA256.hash(data: Data(token.utf8)).map { String(format: "%02x", $0) }.joined()
        return "legacy-v1:\(digest)"
    }

    private static func deviceBinding(session: DurableDeviceSessionMetadata) -> String {
        "device-v1:\(session.clientInstanceID.uuidString.lowercased()):\(session.id.uuidString.lowercased())"
    }

    private static func nextEnvelopeRevision(
        after envelope: DurableAuthEnvelope?
    ) throws -> UInt64 {
        guard let envelope else { return 0 }
        guard envelope.revision < UInt64.max else {
            throw DurableAuthStateStoreError.revisionOverflow
        }
        return envelope.revision + 1
    }

    private static func presentation(for state: DurableAuthState) -> DurableAuthPresentation {
        switch state {
        case .legacy:
            .init(
                phase: .legacy,
                title: "Legacy bearer connected",
                detail: "Upgrade once to a rotating, revocable device session.",
                accessExpiresAt: nil,
                canUpgrade: true,
                canReenroll: false,
                canRevokeRemotely: false,
                canForget: true
            )
        case .enrollmentCreationPending:
            .init(
                phase: .enrollmentCreationPending,
                title: "Finishing secure bootstrap",
                detail: "The exact enrollment-creation request is saved in Keychain and will be retried unchanged.",
                accessExpiresAt: nil,
                canUpgrade: true,
                canReenroll: false,
                canRevokeRemotely: true,
                canForget: true
            )
        case .enrollmentPending:
            .init(
                phase: .enrollmentPending,
                title: "Finishing secure enrollment",
                detail: "The exact proposed session is saved in Keychain and will be retried unchanged.",
                accessExpiresAt: nil,
                canUpgrade: true,
                canReenroll: false,
                canRevokeRemotely: true,
                canForget: true
            )
        case let .active(active):
            .init(
                phase: .active,
                title: "Rotating device session",
                detail: "Access rotates automatically; the refresh credential never leaves Keychain except for its refresh request.",
                accessExpiresAt: active.session.accessExpiresAt,
                canUpgrade: false,
                canReenroll: false,
                canRevokeRemotely: true,
                canForget: true
            )
        case let .refreshPending(pending):
            .init(
                phase: .refreshPending,
                title: "Finishing credential rotation",
                detail: "The exact next pair is saved and will be retried unchanged.",
                accessExpiresAt: pending.previous.session.accessExpiresAt,
                canUpgrade: false,
                canReenroll: false,
                canRevokeRemotely: true,
                canForget: true
            )
        case .reauthenticationRequired:
            .init(
                phase: .reauthenticationRequired,
                title: "Re-enrollment required",
                detail: "The durable session expired or was revoked. DayWeave will not fall back to the old bearer.",
                accessExpiresAt: nil,
                canUpgrade: false,
                canReenroll: true,
                canRevokeRemotely: false,
                canForget: true
            )
        case .incompatible:
            .init(
                phase: .incompatible,
                title: "Authentication update required",
                detail: "Saved recovery material is quarantined because the contract did not match this app version.",
                accessExpiresAt: nil,
                canUpgrade: false,
                canReenroll: false,
                canRevokeRemotely: false,
                canForget: true
            )
        }
    }

    private static let localStateUnavailablePresentation = DurableAuthPresentation(
        phase: .incompatible,
        title: "Keychain unavailable",
        detail: "Authentication state could not be read safely. No legacy credential will be used until Keychain access is restored.",
        accessExpiresAt: nil,
        canUpgrade: false,
        canReenroll: false,
        canRevokeRemotely: false,
        canForget: false
    )

    static func isValidLegacyToken(_ token: String) -> Bool {
        let bytes = token.utf8
        return !token.isEmpty
            && token == token.trimmingCharacters(in: .whitespacesAndNewlines)
            && bytes.count <= 64 * 1_024
            && !token.hasPrefix("dw_")
            && !token.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }

    static func isCredential(_ value: String, prefix: String) -> Bool {
        guard value.hasPrefix(prefix),
              let decoded = decodeCredentialPayload(String(value.dropFirst(prefix.count))) else {
            return false
        }
        return decoded.count == 32
            && value.utf8.count == prefix.utf8.count + 43
            && prefix + encodeCredentialPayload(decoded) == value
    }

    static func credentialMaterial(_ value: String) -> Data? {
        guard let separator = value.firstIndex(of: "_") else { return nil }
        let afterFirst = value.index(after: separator)
        guard let second = value[afterFirst...].firstIndex(of: "_") else { return nil }
        return decodeCredentialPayload(String(value[value.index(after: second)...]))
    }

    static func decodeCredentialPayload(_ value: String) -> Data? {
        var base64 = value.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = base64.count % 4
        if remainder != 0 { base64 += String(repeating: "=", count: 4 - remainder) }
        return Data(base64Encoded: base64)
    }

    private static func encodeCredentialPayload(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private static func wasDurable(_ state: DurableAuthState) -> Bool {
        switch state {
        case .legacy: false
        case .enrollmentCreationPending, .enrollmentPending, .active,
             .refreshPending, .reauthenticationRequired, .incompatible:
            true
        }
    }

    private static func pendingConfigurationIdentifier(
        in state: DurableAuthState
    ) -> String? {
        switch state {
        case let .enrollmentCreationPending(pending):
            pending.creationRequest.configurationIdentifier
        case let .enrollmentPending(pending):
            pending.consumeRequest.configurationIdentifier
        case let .refreshPending(pending):
            pending.refreshRequest.configurationIdentifier
        case .legacy, .active, .reauthenticationRequired, .incompatible:
            nil
        }
    }

    private static func isExplicitLocalOnlyTombstone(_ state: DurableAuthState) -> Bool {
        guard case let .reauthenticationRequired(value) = state else { return false }
        return value.reason == .explicitlyDisconnected
    }

    static func isStoredSessionValid(_ session: DurableDeviceSessionMetadata) -> Bool {
        let descriptor = DurableAuthClientDescriptor(
            deviceLabel: session.deviceLabel,
            clientVersion: session.clientVersion,
            scopes: session.scopes,
            clientCapabilities: session.clientCapabilities
        )
        let finiteDates = [
            session.createdAt, session.lastSeenAt, session.credentialIssuedAt,
            session.accessExpiresAt, session.refreshIdleExpiresAt, session.absoluteExpiresAt,
        ].allSatisfy { $0.timeIntervalSinceReferenceDate.isFinite }
        return finiteDates
            && session.clientKind == "macos"
            && session.clientContractVersion == DurableAuthClientDescriptor.contractVersion
            && descriptor.isValid
            && session.revision >= 1
            && session.createdAt <= session.credentialIssuedAt
            && session.credentialIssuedAt <= session.lastSeenAt
            && session.credentialIssuedAt < session.accessExpiresAt
            && session.accessExpiresAt.timeIntervalSince(session.credentialIssuedAt)
                <= accessLifetime + 1
            && session.credentialIssuedAt < session.refreshIdleExpiresAt
            && session.refreshIdleExpiresAt.timeIntervalSince(session.credentialIssuedAt)
                <= refreshIdleLifetime + 1
            && session.credentialIssuedAt < session.absoluteExpiresAt
            && session.absoluteExpiresAt.timeIntervalSince(session.createdAt)
                <= absoluteLifetime + 1
            && session.accessExpiresAt <= session.absoluteExpiresAt
            && session.refreshIdleExpiresAt <= session.absoluteExpiresAt
    }

    private static func sessionID(in state: DurableAuthState) -> UUID? {
        switch state {
        case let .enrollmentCreationPending(value): value.proposedSessionID
        case let .enrollmentPending(value): value.proposedSessionID
        case let .active(value): value.session.id
        case let .refreshPending(value): value.previous.session.id
        case let .reauthenticationRequired(value): value.previousSessionID
        case let .incompatible(value):
            switch value.recovery {
            case let .enrollmentCreation(pending): pending.proposedSessionID
            case let .enrollment(pending): pending.proposedSessionID
            case let .refresh(pending): pending.previous.session.id
            case let .active(active): active.session.id
            case nil: nil
            }
        case .legacy: nil
        }
    }
}

struct CreateEnrollmentRequest: Encodable {
    let id: UUID
    let enrollmentToken: String
    let clientInstanceID: UUID
    let clientKind: String
    let deviceLabel: String
    let scopes: [DayWeaveAuthScope]
    let clientContractVersion: Int
    let clientVersion: String
    let clientCapabilities: [String]

    private enum CodingKeys: String, CodingKey {
        case id
        case enrollmentToken = "enrollment_token"
        case clientInstanceID = "client_instance_id"
        case clientKind = "client_kind"
        case deviceLabel = "device_label"
        case scopes
        case clientContractVersion = "client_contract_version"
        case clientVersion = "client_version"
        case clientCapabilities = "client_capabilities"
    }
}

struct DeviceEnrollmentResponse: Decodable {
    let id: UUID
    let enrollmentToken: String
    let expiresAt: Date
    let clientContractVersion: Int
    let replayed: Bool

    private enum CodingKeys: String, CodingKey {
        case id
        case enrollmentToken = "enrollment_token"
        case expiresAt = "expires_at"
        case clientContractVersion = "client_contract_version"
        case replayed
    }
}

struct ConsumeEnrollmentRequest: Encodable {
    let sessionID: UUID
    let accessToken: String
    let refreshToken: String

    private enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
    }
}

struct RefreshRequest: Encodable {
    let nextAccessToken: String
    let nextRefreshToken: String

    private enum CodingKeys: String, CodingKey {
        case nextAccessToken = "next_access_token"
        case nextRefreshToken = "next_refresh_token"
    }
}

private struct DeviceSessionMutationResponse: Decodable {
    let session: DurableDeviceSessionMetadata
    let replayed: Bool
}

extension DeviceEnrollmentResponse: RedactedAuthDescribing {}
extension CreateEnrollmentRequest: RedactedAuthDescribing {}
extension ConsumeEnrollmentRequest: RedactedAuthDescribing {}
extension RefreshRequest: RedactedAuthDescribing {}

@MainActor
final class DurableAuthSettingsModel: ObservableObject {
    @Published private(set) var presentation: DurableAuthPresentation
    @Published private(set) var isBusy = false
    @Published private(set) var errorMessage: String?

    let coordinator: DurableAuthCoordinator
    private let configurationStore: any SuggestionAPIConfigurationStoring
    private let descriptor: DurableAuthClientDescriptor

    init(
        coordinator: DurableAuthCoordinator,
        configurationStore: any SuggestionAPIConfigurationStoring =
            UserDefaultsSuggestionAPIConfigurationStore(),
        descriptor: DurableAuthClientDescriptor = .live
    ) {
        self.coordinator = coordinator
        self.configurationStore = configurationStore
        self.descriptor = descriptor
        let baseURL = configurationStore.loadBaseURL().flatMap { try? DayWeaveAPIBaseURL($0) }
        presentation = coordinator.presentation(boundTo: baseURL)
    }

    func reload() {
        let baseURL = configurationStore.loadBaseURL().flatMap { try? DayWeaveAPIBaseURL($0) }
        reload(boundTo: baseURL)
    }

    func reload(boundTo baseURL: DayWeaveAPIBaseURL?) {
        guard !isBusy else { return }
        presentation = coordinator.presentation(boundTo: baseURL)
    }

    @discardableResult
    func installLegacy(baseURL: DayWeaveAPIBaseURL, token: String) async -> Bool {
        await perform(baseURL: baseURL) {
            try await self.coordinator.installLegacyCredential(token, boundTo: baseURL)
        }
    }

    @discardableResult
    func enroll(baseURL: DayWeaveAPIBaseURL, bootstrapToken: String? = nil) async -> Bool {
        await perform(baseURL: baseURL) {
            _ = try await self.coordinator.enroll(
                boundTo: baseURL,
                descriptor: self.descriptor,
                bootstrapToken: bootstrapToken
            )
        }
    }

    @discardableResult
    func consumeEnrollmentCode(baseURL: DayWeaveAPIBaseURL, code: String) async -> Bool {
        await perform(baseURL: baseURL) {
            _ = try await self.coordinator.consumeOneTimeEnrollmentCode(
                code,
                boundTo: baseURL,
                descriptor: self.descriptor
            )
        }
    }

    @discardableResult
    func resume(baseURL: DayWeaveAPIBaseURL) async -> Bool {
        await perform(baseURL: baseURL) {
            try await self.coordinator.resumePendingWork(boundTo: baseURL)
        }
    }

    @discardableResult
    func revokeAndForget(baseURL: DayWeaveAPIBaseURL) async -> Bool {
        await perform(baseURL: baseURL) {
            try await self.coordinator.revokeAndForget(boundTo: baseURL)
        }
    }

    @discardableResult
    func forgetLocally(baseURL: DayWeaveAPIBaseURL?) async -> Bool {
        await perform(baseURL: baseURL) {
            try await self.coordinator.confirmLocalOnlyForget()
        }
    }

    private func perform(
        baseURL: DayWeaveAPIBaseURL?,
        operation: () async throws -> Void
    ) async -> Bool {
        guard !isBusy else { return false }
        isBusy = true
        errorMessage = nil
        defer {
            isBusy = false
            presentation = coordinator.presentation(boundTo: baseURL)
        }
        do {
            try await operation()
            return true
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
    }
}
