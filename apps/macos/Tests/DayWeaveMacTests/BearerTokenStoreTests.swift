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
            keychain: access
        )

        #expect(try store.loadToken() == nil)
        try store.saveToken("  secret-token  ")

        #expect(try store.loadToken() == "secret-token")
        #expect(access.lastService == "test.service")
        #expect(access.lastAccount == "test.account")

        try store.deleteToken()
        #expect(try store.loadToken() == nil)
    }

    @Test("invalid Keychain bytes cannot become authorization text")
    func testInvalidKeychainBytesNeverBecomeAuthorizationText() throws {
        let access = TestKeychainSecretAccess(data: Data([0xFF, 0xFE]))
        let store = KeychainBearerTokenStore(keychain: access)

        do {
            _ = try store.loadToken()
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
            try store.saveToken("never-echo-this-token")
            Issue.record("Expected an injected Keychain failure")
        } catch {
            #expect(error as? BearerTokenStoreError == .writeFailed(status: errSecNotAvailable))
            #expect(!error.localizedDescription.contains("never-echo-this-token"))
        }
    }
}
#endif

private final class TestKeychainSecretAccess: KeychainSecretAccessing, @unchecked Sendable {
    private let lock = NSLock()
    private var data: Data?
    private let writeError: BearerTokenStoreError?
    private(set) var lastService: String?
    private(set) var lastAccount: String?

    init(data: Data? = nil, writeError: BearerTokenStoreError? = nil) {
        self.data = data
        self.writeError = writeError
    }

    func read(service: String, account: String) -> Data? {
        lock.withLock {
            lastService = service
            lastAccount = account
            return data
        }
    }

    func save(_ data: Data, service: String, account: String) throws {
        try lock.withLock {
            if let writeError { throw writeError }
            lastService = service
            lastAccount = account
            self.data = data
        }
    }

    func delete(service: String, account: String) {
        lock.withLock {
            lastService = service
            lastAccount = account
            data = nil
        }
    }
}
