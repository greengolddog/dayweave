import Foundation
import Security
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Bearer token Keychain storage")
struct BearerTokenStoreTests {
    @Test("injected Keychain access handles the credential lifecycle")
    func testKeychainStoreUsesInjectedSecretAccessForLifecycle() throws {
        let access = TestKeychainSecretAccess()
        let store = KeychainBearerTokenStore(
            service: "test.service",
            account: "test.account",
            legacyAccount: nil,
            keychain: access
        )

        #expect(try store.loadCredential() == nil)
        let credential = OriginBoundBearerCredential(
            token: "secret-token",
            origin: "https://api.example.com"
        )
        try store.saveCredential(credential)

        #expect(try store.loadCredential() == credential)
        #expect(access.lastService == "test.service")
        #expect(access.lastAccount == "test.account")

        try store.deleteCredential()
        #expect(try store.loadCredential() == nil)
    }

    @Test("invalid Keychain bytes cannot become authorization text")
    func testInvalidKeychainBytesNeverBecomeAuthorizationText() throws {
        let access = TestKeychainSecretAccess(data: Data([0xFF, 0xFE]))
        let store = KeychainBearerTokenStore(keychain: access)

        do {
            _ = try store.loadCredential()
            Issue.record("Expected invalid stored token data")
        } catch {
            #expect(error as? BearerTokenStoreError == .invalidStoredToken)
        }
    }

    @Test("Keychain failures do not include secret material")
    func testInjectedKeychainFailureIsPreservedWithoutSecretMaterial() throws {
        let access = TestKeychainSecretAccess(writeError: .writeFailed(status: errSecNotAvailable))
        let store = KeychainBearerTokenStore(keychain: access)

        do {
            try store.saveCredential(.init(
                token: "never-echo-this-token",
                origin: "https://api.example.com"
            ))
            Issue.record("Expected an injected Keychain failure")
        } catch {
            #expect(error as? BearerTokenStoreError == .writeFailed(status: errSecNotAvailable))
            #expect(!error.localizedDescription.contains("never-echo-this-token"))
        }
    }

    @Test("legacy raw tokens remain unusable until explicitly replaced")
    func testLegacyRawTokenFailsClosedUntilReplacement() throws {
        let access = TestKeychainSecretAccess()
        access.seed(
            Data("legacy-secret".utf8),
            service: KeychainBearerTokenStore.defaultService,
            account: KeychainBearerTokenStore.defaultLegacyAccount
        )
        let store = KeychainBearerTokenStore(keychain: access)

        do {
            _ = try store.loadCredential()
            Issue.record("Expected an unbound legacy credential failure")
        } catch {
            #expect(error as? BearerTokenStoreError == .legacyUnboundToken)
        }

        let replacement = OriginBoundBearerCredential(
            token: "replacement-secret",
            origin: "https://api.example.com"
        )
        try store.saveCredential(replacement)
        #expect(try store.loadCredential() == replacement)
    }
}
#endif

private final class TestKeychainSecretAccess: KeychainSecretAccessing, @unchecked Sendable {
    private let lock = NSLock()
    private var dataByIdentity: [String: Data]
    private let writeError: BearerTokenStoreError?
    private(set) var lastService: String?
    private(set) var lastAccount: String?

    init(data: Data? = nil, writeError: BearerTokenStoreError? = nil) {
        if let data {
            dataByIdentity = [Self.identity(
                service: KeychainBearerTokenStore.defaultService,
                account: KeychainBearerTokenStore.defaultAccount
            ): data]
        } else {
            dataByIdentity = [:]
        }
        self.writeError = writeError
    }

    func seed(_ data: Data, service: String, account: String) {
        lock.withLock {
            dataByIdentity[Self.identity(service: service, account: account)] = data
        }
    }

    func read(service: String, account: String) -> Data? {
        lock.withLock {
            lastService = service
            lastAccount = account
            return dataByIdentity[Self.identity(service: service, account: account)]
        }
    }

    func save(_ data: Data, service: String, account: String) throws {
        try lock.withLock {
            if let writeError { throw writeError }
            lastService = service
            lastAccount = account
            dataByIdentity[Self.identity(service: service, account: account)] = data
        }
    }

    func delete(service: String, account: String) {
        lock.withLock {
            lastService = service
            lastAccount = account
            dataByIdentity.removeValue(forKey: Self.identity(service: service, account: account))
        }
    }

    private static func identity(service: String, account: String) -> String {
        service + "\u{0}" + account
    }
}
