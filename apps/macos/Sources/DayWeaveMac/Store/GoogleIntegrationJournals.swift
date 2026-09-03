import Foundation

/// Non-secret, exact request identity retained until an authoritative account
/// snapshot proves that disconnection completed.
struct GoogleDisconnectRetryJournal: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumConfigurationIdentifierBytes = 4_096

    let version: Int
    let accountID: UUID
    let expectedRevision: UInt64
    let idempotencyKey: String
    let configurationIdentifier: String
    let createdAt: Date

    init(
        accountID: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String,
        configurationIdentifier: String,
        createdAt: Date
    ) throws {
        guard Self.hasValidShape(
            version: Self.currentVersion,
            accountID: accountID,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        ) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        version = Self.currentVersion
        self.accountID = accountID
        self.expectedRevision = expectedRevision
        self.idempotencyKey = idempotencyKey
        self.configurationIdentifier = configurationIdentifier
        self.createdAt = createdAt
    }

    func isValid(now: Date) -> Bool {
        Self.isFinite(now)
            && Self.hasValidShape(
                version: version,
                accountID: accountID,
                expectedRevision: expectedRevision,
                idempotencyKey: idempotencyKey,
                configurationIdentifier: configurationIdentifier,
                createdAt: createdAt
            )
            && createdAt <= now.addingTimeInterval(5 * 60)
    }

    func rebinding(configurationIdentifier: String) throws -> Self {
        try Self(
            accountID: accountID,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        )
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case accountID = "account_id"
        case expectedRevision = "expected_revision"
        case idempotencyKey = "idempotency_key"
        case configurationIdentifier = "configuration_identifier"
        case createdAt = "created_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleJournalKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(Int.self, forKey: .version)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        expectedRevision = try container.decode(UInt64.self, forKey: .expectedRevision)
        idempotencyKey = try container.decode(String.self, forKey: .idempotencyKey)
        configurationIdentifier = try container.decode(
            String.self,
            forKey: .configurationIdentifier
        )
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        guard Self.hasValidShape(
            version: version,
            accountID: accountID,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        ) else {
            throw googleJournalDecodingError(
                codingPath: decoder.codingPath,
                description: "The Google disconnect recovery journal is invalid"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard Self.hasValidShape(
            version: version,
            accountID: accountID,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        ) else {
            throw EncodingError.invalidValue(
                self,
                .init(
                    codingPath: encoder.codingPath,
                    debugDescription: "The Google disconnect recovery journal is invalid"
                )
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(expectedRevision, forKey: .expectedRevision)
        try container.encode(idempotencyKey, forKey: .idempotencyKey)
        try container.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try container.encode(createdAt, forKey: .createdAt)
    }

    private static func hasValidShape(
        version: Int,
        accountID: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String,
        configurationIdentifier: String,
        createdAt: Date
    ) -> Bool {
        return version == currentVersion
            && isValidAccountID(accountID)
            && expectedRevision > 0
            && expectedRevision <= UInt64(Int64.max) - 2
            && isValidIdempotencyKey(idempotencyKey)
            && isValidConfigurationIdentifier(configurationIdentifier)
            && isFinite(createdAt)
    }

    static func isValidConfigurationIdentifier(_ value: String) -> Bool {
        (1...maximumConfigurationIdentifierBytes).contains(value.utf8.count)
            && value.utf8.allSatisfy { (33...126).contains($0) }
    }

    private static func isValidIdempotencyKey(_ value: String) -> Bool {
        (8...128).contains(value.utf8.count)
            && value.utf8.allSatisfy { byte in
                (65...90).contains(byte)
                    || (97...122).contains(byte)
                    || (48...57).contains(byte)
                    || [45, 46, 95].contains(byte)
            }
    }

    fileprivate static func isFinite(_ date: Date) -> Bool {
        date.timeIntervalSinceReferenceDate.isFinite
    }

    fileprivate static func isValidAccountID(_ accountID: UUID) -> Bool {
        accountID != zeroUUID
    }

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}

/// A completion fence for one locally requested Google import. The request ID
/// is persisted before the first send, so an uncertain response can replay the
/// exact request. Server timestamp and generation are recorded only together
/// after that exact request's 202 response is known.
struct GooglePendingRefreshCompletionJournal: Codable, Equatable, Sendable {
    static let currentVersion = 2
    static let maximumCreationDelay: TimeInterval = 5 * 60

    let version: Int
    let accountID: UUID
    let requestID: UUID
    let localRequestStartedAt: Date
    let serverRequestedAt: Date?
    let targetRefreshGeneration: UInt64?
    let configurationIdentifier: String
    let createdAt: Date

    init(
        accountID: UUID,
        requestID: UUID = UUID(),
        localRequestStartedAt: Date,
        serverRequestedAt: Date? = nil,
        targetRefreshGeneration: UInt64? = nil,
        configurationIdentifier: String,
        createdAt: Date
    ) throws {
        guard Self.hasValidShape(
            version: Self.currentVersion,
            accountID: accountID,
            requestID: requestID,
            localRequestStartedAt: localRequestStartedAt,
            serverRequestedAt: serverRequestedAt,
            targetRefreshGeneration: targetRefreshGeneration,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        ) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        version = Self.currentVersion
        self.accountID = accountID
        self.requestID = requestID
        self.localRequestStartedAt = localRequestStartedAt
        self.serverRequestedAt = serverRequestedAt
        self.targetRefreshGeneration = targetRefreshGeneration
        self.configurationIdentifier = configurationIdentifier
        self.createdAt = createdAt
    }

    func recording(
        serverRequestedAt: Date,
        targetRefreshGeneration: UInt64
    ) throws -> Self {
        guard (self.serverRequestedAt == nil && self.targetRefreshGeneration == nil)
                || (self.serverRequestedAt == serverRequestedAt
                    && self.targetRefreshGeneration == targetRefreshGeneration) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        return try Self(
            accountID: accountID,
            requestID: requestID,
            localRequestStartedAt: localRequestStartedAt,
            serverRequestedAt: serverRequestedAt,
            targetRefreshGeneration: targetRefreshGeneration,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        )
    }

    /// Replaces a terminal accepted run with a new persist-before-send request
    /// identity. Its future generation cannot be satisfied by the old run.
    func restarting(at localRequestStartedAt: Date) throws -> Self {
        try Self(
            accountID: accountID,
            requestID: UUID(),
            localRequestStartedAt: localRequestStartedAt,
            serverRequestedAt: nil,
            targetRefreshGeneration: nil,
            configurationIdentifier: configurationIdentifier,
            createdAt: localRequestStartedAt
        )
    }

    func rebinding(configurationIdentifier: String) throws -> Self {
        try Self(
            accountID: accountID,
            requestID: requestID,
            localRequestStartedAt: localRequestStartedAt,
            serverRequestedAt: serverRequestedAt,
            targetRefreshGeneration: targetRefreshGeneration,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        )
    }

    func isValid(now: Date) -> Bool {
        GoogleDisconnectRetryJournal.isFinite(now)
            && Self.hasValidShape(
                version: version,
                accountID: accountID,
                requestID: requestID,
                localRequestStartedAt: localRequestStartedAt,
                serverRequestedAt: serverRequestedAt,
                targetRefreshGeneration: targetRefreshGeneration,
                configurationIdentifier: configurationIdentifier,
                createdAt: createdAt
            )
            && createdAt <= now.addingTimeInterval(5 * 60)
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case accountID = "account_id"
        case requestID = "request_id"
        case localRequestStartedAt = "local_request_started_at"
        case serverRequestedAt = "server_requested_at"
        case targetRefreshGeneration = "target_refresh_generation"
        case configurationIdentifier = "configuration_identifier"
        case createdAt = "created_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleJournalKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(Int.self, forKey: .version)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        requestID = try container.decode(UUID.self, forKey: .requestID)
        localRequestStartedAt = try container.decode(Date.self, forKey: .localRequestStartedAt)
        serverRequestedAt = try container.decodeIfPresent(Date.self, forKey: .serverRequestedAt)
        targetRefreshGeneration = try container.decodeIfPresent(
            UInt64.self,
            forKey: .targetRefreshGeneration
        )
        configurationIdentifier = try container.decode(
            String.self,
            forKey: .configurationIdentifier
        )
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        guard Self.hasValidShape(
            version: version,
            accountID: accountID,
            requestID: requestID,
            localRequestStartedAt: localRequestStartedAt,
            serverRequestedAt: serverRequestedAt,
            targetRefreshGeneration: targetRefreshGeneration,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        ) else {
            throw googleJournalDecodingError(
                codingPath: decoder.codingPath,
                description: "The Google refresh completion journal is invalid"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard Self.hasValidShape(
            version: version,
            accountID: accountID,
            requestID: requestID,
            localRequestStartedAt: localRequestStartedAt,
            serverRequestedAt: serverRequestedAt,
            targetRefreshGeneration: targetRefreshGeneration,
            configurationIdentifier: configurationIdentifier,
            createdAt: createdAt
        ) else {
            throw EncodingError.invalidValue(
                self,
                .init(
                    codingPath: encoder.codingPath,
                    debugDescription: "The Google refresh completion journal is invalid"
                )
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(requestID, forKey: .requestID)
        try container.encode(localRequestStartedAt, forKey: .localRequestStartedAt)
        if let serverRequestedAt {
            try container.encode(serverRequestedAt, forKey: .serverRequestedAt)
        } else {
            try container.encodeNil(forKey: .serverRequestedAt)
        }
        if let targetRefreshGeneration {
            try container.encode(targetRefreshGeneration, forKey: .targetRefreshGeneration)
        } else {
            try container.encodeNil(forKey: .targetRefreshGeneration)
        }
        try container.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try container.encode(createdAt, forKey: .createdAt)
    }

    private static func hasValidShape(
        version: Int,
        accountID: UUID,
        requestID: UUID,
        localRequestStartedAt: Date,
        serverRequestedAt: Date?,
        targetRefreshGeneration: UInt64?,
        configurationIdentifier: String,
        createdAt: Date
    ) -> Bool {
        let creationDelay = createdAt.timeIntervalSince(localRequestStartedAt)
        return version == currentVersion
            && GoogleDisconnectRetryJournal.isValidAccountID(accountID)
            && GoogleDisconnectRetryJournal.isValidAccountID(requestID)
            && GoogleDisconnectRetryJournal.isValidConfigurationIdentifier(
                configurationIdentifier
            )
            && GoogleDisconnectRetryJournal.isFinite(localRequestStartedAt)
            && (serverRequestedAt.map(GoogleDisconnectRetryJournal.isFinite) ?? true)
            && ((serverRequestedAt == nil) == (targetRefreshGeneration == nil))
            && (targetRefreshGeneration.map {
                $0 > 0 && $0 <= UInt64(Int64.max)
            } ?? true)
            && GoogleDisconnectRetryJournal.isFinite(createdAt)
            && creationDelay >= 0
            && creationDelay <= maximumCreationDelay
    }

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}

@MainActor
protocol GoogleDisconnectRetryJournalStoring: AnyObject {
    func load(now: Date) throws -> GoogleDisconnectRetryJournal?
    func save(_ journal: GoogleDisconnectRetryJournal, now: Date) throws
    func delete() throws
}

@MainActor
extension GoogleDisconnectRetryJournalStoring {
    func load() throws -> GoogleDisconnectRetryJournal? { try load(now: Date()) }
    func save(_ journal: GoogleDisconnectRetryJournal) throws {
        try save(journal, now: Date())
    }
}

@MainActor
protocol GooglePendingRefreshCompletionJournalStoring: AnyObject {
    func load(now: Date) throws -> [GooglePendingRefreshCompletionJournal]
    func save(_ journal: GooglePendingRefreshCompletionJournal, now: Date) throws
    func delete(accountID: UUID, configurationIdentifier: String) throws
    func deleteAll() throws
}

@MainActor
extension GooglePendingRefreshCompletionJournalStoring {
    func load() throws -> [GooglePendingRefreshCompletionJournal] { try load(now: Date()) }
    func save(_ journal: GooglePendingRefreshCompletionJournal) throws {
        try save(journal, now: Date())
    }

    func journal(
        accountID: UUID,
        configurationIdentifier: String,
        now: Date = Date()
    ) throws -> GooglePendingRefreshCompletionJournal? {
        try load(now: now).first {
            $0.accountID == accountID
                && $0.configurationIdentifier == configurationIdentifier
        }
    }
}

enum GoogleIntegrationJournalStoreError: Error, Equatable, Sendable, LocalizedError {
    case invalidJournal
    case invalidStoredJournal
    case stateTooLarge
    case capacityExceeded
    case writeFailed

    var errorDescription: String? {
        switch self {
        case .invalidJournal:
            "DayWeave refused to save an invalid Google recovery journal. No request was sent."
        case .invalidStoredJournal:
            "The saved Google recovery journal is invalid. Google changes remain blocked."
        case .stateTooLarge:
            "The saved Google recovery journal exceeds its safe size limit."
        case .capacityExceeded:
            "Too many Google refresh recoveries are pending to save another safely."
        case .writeFailed:
            "DayWeave could not durably save the Google recovery journal. No request was sent."
        }
    }
}

@MainActor
final class UserDefaultsGoogleDisconnectRetryJournalStore:
    GoogleDisconnectRetryJournalStoring
{
    static let defaultKey = "dayweave.google.disconnect-retry-journal.v1"
    static let maximumEncodedBytes = 32 * 1_024

    private let defaults: UserDefaults
    private let key: String

    init(defaults: UserDefaults = .standard, key: String = defaultKey) {
        self.defaults = defaults
        self.key = key
    }

    func load(now: Date) throws -> GoogleDisconnectRetryJournal? {
        guard GoogleDisconnectRetryJournal.isFinite(now) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        guard let stored = defaults.object(forKey: key) else { return nil }
        guard let data = stored as? Data else {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        guard data.count <= Self.maximumEncodedBytes else {
            throw GoogleIntegrationJournalStoreError.stateTooLarge
        }
        let journal: GoogleDisconnectRetryJournal
        do {
            journal = try JSONDecoder().decode(GoogleDisconnectRetryJournal.self, from: data)
        } catch {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        guard journal.isValid(now: now) else {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        return journal
    }

    func save(_ journal: GoogleDisconnectRetryJournal, now: Date) throws {
        guard journal.isValid(now: now) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        try persist(try encode(journal))
    }

    func delete() throws {
        try removePersistedValue()
    }

    private func encode(_ journal: GoogleDisconnectRetryJournal) throws -> Data {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let data = try encoder.encode(journal)
            guard data.count <= Self.maximumEncodedBytes else {
                throw GoogleIntegrationJournalStoreError.stateTooLarge
            }
            return data
        } catch let error as GoogleIntegrationJournalStoreError {
            throw error
        } catch {
            throw GoogleIntegrationJournalStoreError.writeFailed
        }
    }

    private func persist(_ data: Data) throws {
        defaults.set(data, forKey: key)
        guard defaults.synchronize(), defaults.data(forKey: key) == data else {
            throw GoogleIntegrationJournalStoreError.writeFailed
        }
    }

    private func removePersistedValue() throws {
        defaults.removeObject(forKey: key)
        guard defaults.synchronize(), defaults.object(forKey: key) == nil else {
            throw GoogleIntegrationJournalStoreError.writeFailed
        }
    }
}

@MainActor
final class UserDefaultsGooglePendingRefreshCompletionJournalStore:
    GooglePendingRefreshCompletionJournalStoring
{
    static let defaultKey = "dayweave.google.pending-refresh-completions.v1"
    static let maximumEncodedBytes = 2 * 1_048_576
    static let maximumEntries = 10_000

    private let defaults: UserDefaults
    private let key: String

    init(defaults: UserDefaults = .standard, key: String = defaultKey) {
        self.defaults = defaults
        self.key = key
    }

    func load(now: Date) throws -> [GooglePendingRefreshCompletionJournal] {
        guard GoogleDisconnectRetryJournal.isFinite(now) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        guard let stored = defaults.object(forKey: key) else { return [] }
        guard let data = stored as? Data else {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        guard data.count <= Self.maximumEncodedBytes else {
            throw GoogleIntegrationJournalStoreError.stateTooLarge
        }
        let ledger: GooglePendingRefreshCompletionLedger
        do {
            ledger = try JSONDecoder().decode(
                GooglePendingRefreshCompletionLedger.self,
                from: data
            )
        } catch {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        guard ledger.entries.count <= Self.maximumEntries,
              Set(ledger.entries.map(\.accountID)).count == ledger.entries.count,
              ledger.entries.allSatisfy({ $0.isValid(now: now) }) else {
            throw GoogleIntegrationJournalStoreError.invalidStoredJournal
        }
        return Self.sorted(ledger.entries)
    }

    func save(_ journal: GooglePendingRefreshCompletionJournal, now: Date) throws {
        guard journal.isValid(now: now) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        var entries = try load(now: now)
        if let index = entries.firstIndex(where: { $0.accountID == journal.accountID }) {
            entries[index] = journal
        } else {
            guard entries.count < Self.maximumEntries else {
                throw GoogleIntegrationJournalStoreError.capacityExceeded
            }
            entries.append(journal)
        }
        try persist(try encode(Self.sorted(entries)))
    }

    func delete(accountID: UUID, configurationIdentifier: String) throws {
        guard GoogleDisconnectRetryJournal.isValidAccountID(accountID),
              GoogleDisconnectRetryJournal.isValidConfigurationIdentifier(
            configurationIdentifier
        ) else {
            throw GoogleIntegrationJournalStoreError.invalidJournal
        }
        let now = Date()
        let entries = try load(now: now)
        let retained = entries.filter {
            $0.accountID != accountID
                || $0.configurationIdentifier != configurationIdentifier
        }
        guard retained.count != entries.count else { return }
        if retained.isEmpty {
            try removePersistedValue()
        } else {
            try persist(try encode(retained))
        }
    }

    func deleteAll() throws {
        try removePersistedValue()
    }

    private func encode(_ entries: [GooglePendingRefreshCompletionJournal]) throws -> Data {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let data = try encoder.encode(
                GooglePendingRefreshCompletionLedger(entries: entries)
            )
            guard data.count <= Self.maximumEncodedBytes else {
                throw GoogleIntegrationJournalStoreError.stateTooLarge
            }
            return data
        } catch let error as GoogleIntegrationJournalStoreError {
            throw error
        } catch {
            throw GoogleIntegrationJournalStoreError.writeFailed
        }
    }

    private static func sorted(
        _ entries: [GooglePendingRefreshCompletionJournal]
    ) -> [GooglePendingRefreshCompletionJournal] {
        entries.sorted {
            if $0.createdAt != $1.createdAt { return $0.createdAt < $1.createdAt }
            return $0.accountID.uuidString < $1.accountID.uuidString
        }
    }

    private func persist(_ data: Data) throws {
        defaults.set(data, forKey: key)
        guard defaults.synchronize(), defaults.data(forKey: key) == data else {
            throw GoogleIntegrationJournalStoreError.writeFailed
        }
    }

    private func removePersistedValue() throws {
        defaults.removeObject(forKey: key)
        guard defaults.synchronize(), defaults.object(forKey: key) == nil else {
            throw GoogleIntegrationJournalStoreError.writeFailed
        }
    }
}

private struct GooglePendingRefreshCompletionLedger: Codable {
    static let currentVersion = 2

    let version: Int
    let entries: [GooglePendingRefreshCompletionJournal]

    init(entries: [GooglePendingRefreshCompletionJournal]) {
        version = Self.currentVersion
        self.entries = entries
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case entries
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleJournalKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(Int.self, forKey: .version)
        entries = try container.decode(
            [GooglePendingRefreshCompletionJournal].self,
            forKey: .entries
        )
        guard version == Self.currentVersion else {
            throw googleJournalDecodingError(
                codingPath: decoder.codingPath,
                description: "The Google refresh completion ledger version is invalid"
            )
        }
    }
}

private struct GoogleJournalDynamicCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int?

    init?(stringValue: String) {
        self.stringValue = stringValue
        intValue = nil
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}

func requireExactGoogleJournalKeys<Key: CodingKey & CaseIterable>(
    _ keyType: Key.Type,
    from decoder: any Decoder
) throws {
    let container = try decoder.container(keyedBy: GoogleJournalDynamicCodingKey.self)
    let actual = Set(container.allKeys.map(\.stringValue))
    let expected = Set(Key.allCases.map(\.stringValue))
    guard actual == expected else {
        throw googleJournalDecodingError(
            codingPath: decoder.codingPath,
            description: "The Google recovery journal has an unsupported field shape"
        )
    }
}

func googleJournalDecodingError(
    codingPath: [any CodingKey],
    description: String
) -> DecodingError {
    .dataCorrupted(.init(codingPath: codingPath, debugDescription: description))
}

extension GoogleDisconnectRetryJournal: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Google disconnect recovery journal" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

extension GooglePendingRefreshCompletionJournal: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    var description: String { "Google refresh completion journal" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

extension UserDefaultsGoogleDisconnectRetryJournalStore: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    nonisolated var description: String { "Google disconnect recovery journal store" }
    nonisolated var debugDescription: String { description }
    nonisolated var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .class)
    }
}

extension UserDefaultsGooglePendingRefreshCompletionJournalStore:
    CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable
{
    nonisolated var description: String { "Google refresh completion journal store" }
    nonisolated var debugDescription: String { description }
    nonisolated var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .class)
    }
}
