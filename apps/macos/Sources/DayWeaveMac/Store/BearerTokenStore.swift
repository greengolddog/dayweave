import Foundation
import Security

protocol BearerTokenStoring: Sendable {
    func loadToken() throws -> String?
    func saveToken(_ token: String) throws
    func deleteToken() throws
}

enum BearerTokenStoreError: Error, Equatable, Sendable {
    case emptyToken
    case invalidStoredToken
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
            "The saved bearer token is invalid. Replace it in Settings."
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
    static let defaultAccount = "bearer-token-v1"

    let service: String
    let account: String
    private let keychain: any KeychainSecretAccessing

    init(
        service: String = Self.defaultService,
        account: String = Self.defaultAccount,
        keychain: any KeychainSecretAccessing = SystemKeychainSecretAccess()
    ) {
        self.service = service
        self.account = account
        self.keychain = keychain
    }

    func loadToken() throws -> String? {
        guard let data = try keychain.read(service: service, account: account) else {
            return nil
        }
        guard let token = String(data: data, encoding: .utf8), !token.isEmpty else {
            throw BearerTokenStoreError.invalidStoredToken
        }
        return token
    }

    func saveToken(_ token: String) throws {
        let token = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else {
            throw BearerTokenStoreError.emptyToken
        }
        try keychain.save(Data(token.utf8), service: service, account: account)
    }

    func deleteToken() throws {
        try keychain.delete(service: service, account: account)
    }
}
