import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Google integration recovery journals", .serialized)
@MainActor
struct GoogleIntegrationJournalsTests {
    private static let now = Date(timeIntervalSince1970: 1_788_000_000)
    private static let configurationIdentifier =
        "https://api.example.test|auth=device-v1:test-binding"

    @Test("disconnect retry persists until authoritative resolution")
    func disconnectRoundTrip() throws {
        let context = try Self.defaultsContext()
        defer { context.remove() }
        let journal = try GoogleDisconnectRetryJournal(
            accountID: UUID(uuidString: "10000000-0000-4000-8000-000000000001")!,
            expectedRevision: 7,
            idempotencyKey: "mac-google-disconnect-test-0001",
            configurationIdentifier: Self.configurationIdentifier,
            createdAt: Self.now
        )
        let store = UserDefaultsGoogleDisconnectRetryJournalStore(
            defaults: context.defaults
        )

        try store.save(journal, now: Self.now)

        #expect(try store.load(now: Self.now) == journal)
        #expect(
            try store.load(now: Self.now.addingTimeInterval(365 * 24 * 60 * 60))
                == journal
        )
        let persisted = try #require(context.defaults.data(
            forKey: UserDefaultsGoogleDisconnectRetryJournalStore.defaultKey
        ))
        let text = String(decoding: persisted, as: UTF8.self).lowercased()
        #expect(!text.contains("bearer"))
        #expect(!text.contains("refresh_token"))
        #expect(!text.contains("authorization_url"))
        #expect(!text.contains("expires_at"))
    }

    @Test("disconnect journal rejects an implausible future creation anchor")
    func disconnectRejectsFutureCreationAnchor() throws {
        let context = try Self.defaultsContext()
        defer { context.remove() }
        let store = UserDefaultsGoogleDisconnectRetryJournalStore(
            defaults: context.defaults
        )
        let journal = try GoogleDisconnectRetryJournal(
            accountID: UUID(uuidString: "20000000-0000-4000-8000-000000000002")!,
            expectedRevision: 1,
            idempotencyKey: "mac-google-disconnect-test-0002",
            configurationIdentifier: Self.configurationIdentifier,
            createdAt: Self.now.addingTimeInterval(5 * 60 + 1)
        )
        #expect(throws: GoogleIntegrationJournalStoreError.invalidJournal) {
            try store.save(journal, now: Self.now)
        }
    }

    @Test("disconnect journal fails closed for a wrong type or unknown field")
    func disconnectRejectsCorruption() throws {
        let context = try Self.defaultsContext()
        defer { context.remove() }
        let key = UserDefaultsGoogleDisconnectRetryJournalStore.defaultKey
        let store = UserDefaultsGoogleDisconnectRetryJournalStore(
            defaults: context.defaults
        )
        context.defaults.set("not journal data", forKey: key)
        #expect(throws: GoogleIntegrationJournalStoreError.invalidStoredJournal) {
            _ = try store.load(now: Self.now)
        }

        let journal = try GoogleDisconnectRetryJournal(
            accountID: UUID(uuidString: "30000000-0000-4000-8000-000000000003")!,
            expectedRevision: 2,
            idempotencyKey: "mac-google-disconnect-test-0003",
            configurationIdentifier: Self.configurationIdentifier,
            createdAt: Self.now
        )
        try store.save(journal, now: Self.now)
        let data = try #require(context.defaults.data(forKey: key))
        var object = try #require(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        object["unexpected"] = true
        context.defaults.set(try JSONSerialization.data(withJSONObject: object), forKey: key)
        #expect(throws: GoogleIntegrationJournalStoreError.invalidStoredJournal) {
            _ = try store.load(now: Self.now)
        }
    }

    @Test("OAuth journal is durable, strict, and redacted")
    func oauthJournalDurabilityStrictnessAndDiagnostics() throws {
        let context = try Self.defaultsContext()
        defer { context.remove() }
        let key = UserDefaultsGoogleOAuthStartJournalStore.defaultKey
        let idempotencyKey = "mac-google-oauth-private-marker"
        let configurationIdentifier = "private-oauth-configuration-marker"
        let journal = GoogleOAuthStartJournal(
            request: GoogleOAuthStartRequest(),
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            baselineAccountRevisions: [:],
            createdAt: Self.now,
            expiresAt: Self.now.addingTimeInterval(10 * 60)
        )
        let store = UserDefaultsGoogleOAuthStartJournalStore(
            defaults: context.defaults
        )

        try store.save(journal)

        let persisted = try #require(context.defaults.data(forKey: key))
        #expect(context.defaults.synchronize())
        #expect(context.defaults.data(forKey: key) == persisted)
        let relaunched = UserDefaultsGoogleOAuthStartJournalStore(
            defaults: context.defaults
        )
        #expect(try relaunched.load() == journal)
        let rendered = [
            String(describing: journal), String(reflecting: journal),
            String(describing: store), String(reflecting: store),
        ].joined()
        #expect(!rendered.contains(idempotencyKey))
        #expect(!rendered.contains(configurationIdentifier))

        context.defaults.set("not journal data", forKey: key)
        #expect(context.defaults.synchronize())
        #expect(throws: GoogleOAuthStartJournalStoreError.self) {
            _ = try relaunched.load()
        }

        try store.save(journal)
        let strictData = try #require(context.defaults.data(forKey: key))
        var object = try #require(
            JSONSerialization.jsonObject(with: strictData) as? [String: Any]
        )
        object["unexpected"] = true
        context.defaults.set(try JSONSerialization.data(withJSONObject: object), forKey: key)
        #expect(context.defaults.synchronize())
        #expect(throws: GoogleOAuthStartJournalStoreError.self) {
            _ = try relaunched.load()
        }
    }

    @Test("refresh ledger supports multiple accounts and exact response enrichment")
    func refreshRoundTripAndEnrichment() throws {
        let context = try Self.defaultsContext()
        defer { context.remove() }
        let store = UserDefaultsGooglePendingRefreshCompletionJournalStore(
            defaults: context.defaults
        )
        let first = try Self.refreshJournal(accountSuffix: 4, createdAt: Self.now)
        let second = try Self.refreshJournal(
            accountSuffix: 5,
            createdAt: Self.now.addingTimeInterval(1)
        )

        try store.save(first, now: Self.now)
        try store.save(second, now: Self.now.addingTimeInterval(1))

        #expect(try store.load(now: Self.now.addingTimeInterval(2)) == [first, second])
        let requestedAt = Self.now.addingTimeInterval(3)
        let enriched = try first.recording(
            serverRequestedAt: requestedAt,
            targetRefreshGeneration: 1
        )
        try store.save(enriched, now: requestedAt)
        #expect(
            try store.journal(
                accountID: first.accountID,
                configurationIdentifier: Self.configurationIdentifier,
                now: requestedAt
            ) == enriched
        )
        #expect(throws: GoogleIntegrationJournalStoreError.invalidJournal) {
            _ = try enriched.recording(
                serverRequestedAt: requestedAt.addingTimeInterval(1),
                targetRefreshGeneration: 2
            )
        }
    }

    @Test("refresh markers persist until verified composition")
    func refreshMarkersPersistIndefinitely() throws {
        let context = try Self.defaultsContext()
        defer { context.remove() }
        let store = UserDefaultsGooglePendingRefreshCompletionJournalStore(
            defaults: context.defaults
        )
        let first = try Self.refreshJournal(
            accountSuffix: 6,
            createdAt: Self.now
        )
        let second = try Self.refreshJournal(
            accountSuffix: 7,
            createdAt: Self.now.addingTimeInterval(1)
        )
        try store.save(first, now: Self.now)
        try store.save(second, now: Self.now.addingTimeInterval(1))

        let longAfterSevenDays = Self.now.addingTimeInterval(365 * 24 * 60 * 60)
        #expect(try store.load(now: longAfterSevenDays) == [first, second])

        let reloaded = UserDefaultsGooglePendingRefreshCompletionJournalStore(
            defaults: context.defaults
        )
        #expect(try reloaded.load(now: longAfterSevenDays) == [first, second])
        let persisted = try #require(context.defaults.data(
            forKey: UserDefaultsGooglePendingRefreshCompletionJournalStore.defaultKey
        ))
        #expect(!String(decoding: persisted, as: UTF8.self).contains("expires_at"))
    }

    @Test("journal diagnostics expose no request identity or configuration binding")
    func diagnosticsAreRedacted() throws {
        let journal = try GoogleDisconnectRetryJournal(
            accountID: UUID(uuidString: "80000000-0000-4000-8000-000000000008")!,
            expectedRevision: 9,
            idempotencyKey: "mac-google-disconnect-private-marker",
            configurationIdentifier: "private-configuration-marker",
            createdAt: Self.now
        )
        let rendered = [String(describing: journal), String(reflecting: journal)].joined()

        #expect(!rendered.contains(journal.accountID.uuidString))
        #expect(!rendered.contains(journal.idempotencyKey))
        #expect(!rendered.contains(journal.configurationIdentifier))
    }

    private static func refreshJournal(
        accountSuffix: Int,
        createdAt: Date
    ) throws -> GooglePendingRefreshCompletionJournal {
        let suffix = String(format: "%012d", accountSuffix)
        return try GooglePendingRefreshCompletionJournal(
            accountID: UUID(uuidString: "40000000-0000-4000-8000-\(suffix)")!,
            localRequestStartedAt: createdAt,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        )
    }

    private static func defaultsContext() throws -> DefaultsContext {
        let suiteName = "dayweave.google-journal-tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suiteName))
        defaults.removePersistentDomain(forName: suiteName)
        #expect(defaults.synchronize())
        return DefaultsContext(defaults: defaults, suiteName: suiteName)
    }
}

@MainActor
private struct DefaultsContext {
    let defaults: UserDefaults
    let suiteName: String

    func remove() {
        defaults.removePersistentDomain(forName: suiteName)
        _ = defaults.synchronize()
    }
}
#endif
