import Foundation
import Security

protocol BearerTokenStoring: Sendable {
    func loadCredential() throws -> OriginBoundBearerCredential?
    func saveCredential(_ credential: OriginBoundBearerCredential) throws
    func deleteCredential() throws
}

struct OriginBoundBearerCredential: Codable, Equatable, Sendable {
    static let currentVersion = 1

    let version: Int
    let token: String
    let origin: String

    init(token: String, origin: String) {
        version = Self.currentVersion
        self.token = token
        self.origin = origin
    }
}

enum BearerTokenStoreError: Error, Equatable, Sendable {
    case emptyToken
    case invalidStoredToken
    case legacyUnboundToken
    case unsupportedCredentialVersion(Int)
    case credentialOriginMismatch
    case readFailed(status: OSStatus)
    case writeFailed(status: OSStatus)
    case deleteFailed(status: OSStatus)
}

extension BearerTokenStoreError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .emptyToken:
            "The bearer token is empty."
        case .invalidStoredToken:
            "The saved bearer credential is invalid. Replace it in Settings."
        case .legacyUnboundToken:
            "The saved bearer token predates origin binding. Re-enter it in Settings before connecting."
        case let .unsupportedCredentialVersion(version):
            "Bearer credential version \(version) is not supported. Re-enter it in Settings."
        case .credentialOriginMismatch:
            "The saved bearer credential belongs to a different API origin. Re-enter it in Settings before connecting."
        case let .readFailed(status):
            "The bearer token could not be read from Keychain (status \(status))."
        case let .writeFailed(status):
            "The bearer token could not be saved to Keychain (status \(status))."
        case let .deleteFailed(status):
            "The bearer token could not be removed from Keychain (status \(status))."
        }
    }
}

/// A narrow seam around Security.framework. Tests inject an in-memory
/// implementation without touching the user's login Keychain.
protocol KeychainSecretAccessing: Sendable {
    func read(service: String, account: String) throws -> Data?
    func save(_ data: Data, service: String, account: String) throws
    func delete(service: String, account: String) throws
}

struct SystemKeychainSecretAccess: KeychainSecretAccessing {
    func read(service: String, account: String) throws -> Data? {
        var query = identityQuery(service: service, account: account)
        query[kSecReturnData] = true
        query[kSecMatchLimit] = kSecMatchLimitOne

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else {
                throw BearerTokenStoreError.invalidStoredToken
            }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw BearerTokenStoreError.readFailed(status: status)
        }
    }

    func save(_ data: Data, service: String, account: String) throws {
        let query = identityQuery(service: service, account: account)
        let attributes: [CFString: Any] = [
            kSecValueData: data,
            kSecAttrAccessible: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]

        let updateStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        switch updateStatus {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            var addition = query
            addition.merge(attributes) { _, replacement in replacement }
            let addStatus = SecItemAdd(addition as CFDictionary, nil)
            if addStatus == errSecSuccess {
                return
            }
            if addStatus == errSecDuplicateItem {
                // A second app instance may have created the item after the
                // update missed it. Retry the non-destructive update once.
                let retryStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
                guard retryStatus == errSecSuccess else {
                    throw BearerTokenStoreError.writeFailed(status: retryStatus)
                }
                return
            }
            throw BearerTokenStoreError.writeFailed(status: addStatus)
        default:
            throw BearerTokenStoreError.writeFailed(status: updateStatus)
        }
    }

    func delete(service: String, account: String) throws {
        let status = SecItemDelete(identityQuery(service: service, account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw BearerTokenStoreError.deleteFailed(status: status)
        }
    }

    private func identityQuery(service: String, account: String) -> [CFString: Any] {
        [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecAttrSynchronizable: false,
            kSecUseDataProtectionKeychain: true,
        ]
    }
}

struct KeychainBearerTokenStore: BearerTokenStoring {
    static let defaultService = "com.greengolddog.dayweave.suggestions-api"
    static let defaultAccount = "bearer-credential-v2"
    static let defaultLegacyAccount = "bearer-token-v1"

    let service: String
    let account: String
    let legacyAccount: String?
    private let keychain: any KeychainSecretAccessing

    init(
        service: String = Self.defaultService,
        account: String = Self.defaultAccount,
        legacyAccount: String? = Self.defaultLegacyAccount,
        keychain: any KeychainSecretAccessing = SystemKeychainSecretAccess()
    ) {
        self.service = service
        self.account = account
        self.legacyAccount = legacyAccount
        self.keychain = keychain
    }

    func loadCredential() throws -> OriginBoundBearerCredential? {
        guard let data = try keychain.read(service: service, account: account) else {
            if let legacyAccount,
               try keychain.read(service: service, account: legacyAccount) != nil {
                throw BearerTokenStoreError.legacyUnboundToken
            }
            return nil
        }
        let credential: OriginBoundBearerCredential
        do {
            credential = try JSONDecoder().decode(OriginBoundBearerCredential.self, from: data)
        } catch {
            throw BearerTokenStoreError.invalidStoredToken
        }
        guard credential.version == OriginBoundBearerCredential.currentVersion else {
            throw BearerTokenStoreError.unsupportedCredentialVersion(credential.version)
        }
        try validate(credential)
        return credential
    }

    func saveCredential(_ credential: OriginBoundBearerCredential) throws {
        guard credential.version == OriginBoundBearerCredential.currentVersion else {
            throw BearerTokenStoreError.unsupportedCredentialVersion(credential.version)
        }
        try validate(credential)
        let encoded: Data
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            encoded = try encoder.encode(credential)
        } catch {
            throw BearerTokenStoreError.invalidStoredToken
        }
        // The version, normalized origin, and token are one Keychain value;
        // SecItemUpdate replaces that value atomically.
        try keychain.save(encoded, service: service, account: account)
        if let legacyAccount, legacyAccount != account,
           try keychain.read(service: service, account: legacyAccount) != nil {
            try keychain.delete(service: service, account: legacyAccount)
        }
    }

    func deleteCredential() throws {
        try keychain.delete(service: service, account: account)
        if let legacyAccount, legacyAccount != account {
            try keychain.delete(service: service, account: legacyAccount)
        }
    }

    private func validate(_ credential: OriginBoundBearerCredential) throws {
        let trimmedToken = credential.token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedToken.isEmpty, trimmedToken == credential.token else {
            throw BearerTokenStoreError.emptyToken
        }
        guard credential.token.utf8.count <= 64 * 1_024,
              credential.token.unicodeScalars.allSatisfy({
                  !CharacterSet.controlCharacters.contains($0)
              }) else {
            throw BearerTokenStoreError.invalidStoredToken
        }
        guard let baseURL = try? DayWeaveAPIBaseURL(credential.origin),
              baseURL.credentialOriginIdentifier == credential.origin else {
            throw BearerTokenStoreError.invalidStoredToken
        }
    }
}

extension BearerTokenStoring {
    func loadToken(boundTo baseURL: DayWeaveAPIBaseURL) throws -> String? {
        guard let credential = try loadCredential() else { return nil }
        guard credential.origin == baseURL.credentialOriginIdentifier else {
            throw BearerTokenStoreError.credentialOriginMismatch
        }
        return credential.token
    }
}
