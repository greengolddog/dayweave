import AppKit
import Foundation

protocol GoogleIntegrationTransport: Sendable {
    var configurationIdentifier: String { get }

    func googleAccounts() async throws -> GoogleAccountsSnapshot
    func startGoogleOAuth(
        _ request: GoogleOAuthStartRequest,
        idempotencyKey: String
    ) async throws -> GoogleOAuthAuthorization
    func pauseGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount
    func resumeGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount
    func disconnectGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount
    func googleCollections(accountID: UUID) async throws -> [GoogleSyncCollection]
    func discoverGoogleCollections(accountID: UUID) async throws -> [GoogleSyncCollection]
    func configureGoogleCollection(
        accountID: UUID,
        collectionID: UUID,
        expectedRevision: UInt64,
        selected: Bool,
        visible: Bool,
        role: GoogleSyncRole,
        calendarPolicy: GoogleCalendarPolicy
    ) async throws -> GoogleSyncCollection
    func googleSyncStatus(accountID: UUID) async throws -> GoogleSyncStatus
    func requestGoogleSyncRefresh(
        accountID: UUID,
        requestID: UUID
    ) async throws -> GoogleSyncRefreshAccepted
}

extension DayWeaveAPIClient: GoogleIntegrationTransport {}

enum GoogleIntegrationStatus: Equatable, Sendable {
    case privacyProtected
    case configurationRequired(String)
    case ready
    case loading(String)
    case awaitingAuthorization(expiresAt: Date)
    case authorizationOutcomeUnknown(expiresAt: Date)
    case connected(updatedAt: Date, message: String)
    case refreshQueued(requestedAt: Date, message: String)
    case offline(String)
    case failed(String)

    var message: String {
        switch self {
        case .privacyProtected:
            "Google details are hidden while DayWeave is locked."
        case let .configurationRequired(message), let .loading(message),
             let .offline(message), let .failed(message):
            message
        case .ready:
            "Google is ready to connect."
        case let .awaitingAuthorization(expiresAt):
            "A private Google authorization page is ready until \(expiresAt.formatted(date: .omitted, time: .shortened))."
        case let .authorizationOutcomeUnknown(expiresAt):
            "The exact connection request may have reached the server. Retry that same request before \(expiresAt.formatted(date: .omitted, time: .shortened))."
        case let .connected(updatedAt, message):
            "\(message) · \(updatedAt.formatted(date: .omitted, time: .shortened))"
        case let .refreshQueued(requestedAt, message):
            "\(message) · queued \(requestedAt.formatted(date: .omitted, time: .shortened))"
        }
    }

    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

struct GoogleOAuthStartJournal: Codable, Equatable, Sendable {
    static let currentVersion = 1
    static let maximumLifetime: TimeInterval = 30 * 60
    static let maximumBaselineAccounts = 10_000

    let version: Int
    let request: GoogleOAuthStartRequest
    let idempotencyKey: String
    let configurationIdentifier: String
    var baselineAccountRevisions: [UUID: UInt64]
    let createdAt: Date
    var expiresAt: Date
    var browserOpenedAt: Date?

    init(
        request: GoogleOAuthStartRequest,
        idempotencyKey: String,
        configurationIdentifier: String,
        baselineAccountRevisions: [UUID: UInt64],
        createdAt: Date,
        expiresAt: Date,
        browserOpenedAt: Date? = nil
    ) {
        version = Self.currentVersion
        self.request = request
        self.idempotencyKey = idempotencyKey
        self.configurationIdentifier = configurationIdentifier
        self.baselineAccountRevisions = baselineAccountRevisions
        self.createdAt = createdAt
        self.expiresAt = expiresAt
        self.browserOpenedAt = browserOpenedAt
    }

    func isValid(now: Date) -> Bool {
        now.timeIntervalSinceReferenceDate.isFinite
            && Self.hasValidShape(
                version: version,
                request: request,
                idempotencyKey: idempotencyKey,
                configurationIdentifier: configurationIdentifier,
                baselineAccountRevisions: baselineAccountRevisions,
                createdAt: createdAt,
                expiresAt: expiresAt,
                browserOpenedAt: browserOpenedAt
            )
            && createdAt <= now
            && expiresAt > now
            && expiresAt.timeIntervalSince(now) <= Self.maximumLifetime
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case version
        case request
        case idempotencyKey = "idempotency_key"
        case configurationIdentifier = "configuration_identifier"
        case baselineAccountRevisions = "baseline_account_revisions"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
        case browserOpenedAt = "browser_opened_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleJournalKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(Int.self, forKey: .version)
        request = try container.decode(GoogleOAuthStartRequest.self, forKey: .request)
        idempotencyKey = try container.decode(String.self, forKey: .idempotencyKey)
        configurationIdentifier = try container.decode(
            String.self,
            forKey: .configurationIdentifier
        )
        baselineAccountRevisions = try container.decode(
            [UUID: UInt64].self,
            forKey: .baselineAccountRevisions
        )
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        expiresAt = try container.decode(Date.self, forKey: .expiresAt)
        browserOpenedAt = try container.decodeIfPresent(Date.self, forKey: .browserOpenedAt)
        guard Self.hasValidShape(
            version: version,
            request: request,
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            baselineAccountRevisions: baselineAccountRevisions,
            createdAt: createdAt,
            expiresAt: expiresAt,
            browserOpenedAt: browserOpenedAt
        ) else {
            throw googleJournalDecodingError(
                codingPath: decoder.codingPath,
                description: "The Google authorization recovery journal is invalid"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard Self.hasValidShape(
            version: version,
            request: request,
            idempotencyKey: idempotencyKey,
            configurationIdentifier: configurationIdentifier,
            baselineAccountRevisions: baselineAccountRevisions,
            createdAt: createdAt,
            expiresAt: expiresAt,
            browserOpenedAt: browserOpenedAt
        ) else {
            throw EncodingError.invalidValue(
                self,
                .init(
                    codingPath: encoder.codingPath,
                    debugDescription: "The Google authorization recovery journal is invalid"
                )
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(request, forKey: .request)
        try container.encode(idempotencyKey, forKey: .idempotencyKey)
        try container.encode(configurationIdentifier, forKey: .configurationIdentifier)
        try container.encode(baselineAccountRevisions, forKey: .baselineAccountRevisions)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(expiresAt, forKey: .expiresAt)
        if let browserOpenedAt {
            try container.encode(browserOpenedAt, forKey: .browserOpenedAt)
        } else {
            try container.encodeNil(forKey: .browserOpenedAt)
        }
    }

    private static func hasValidShape(
        version: Int,
        request: GoogleOAuthStartRequest,
        idempotencyKey: String,
        configurationIdentifier: String,
        baselineAccountRevisions: [UUID: UInt64],
        createdAt: Date,
        expiresAt: Date,
        browserOpenedAt: Date?
    ) -> Bool {
        version == currentVersion
            && request.isValid
            && request.loginHint == nil
            && GoogleDisconnectRetryJournal.isValidConfigurationIdentifier(
                configurationIdentifier
            )
            && (8...128).contains(idempotencyKey.utf8.count)
            && idempotencyKey.utf8.allSatisfy(Self.isSafeIdempotencyByte)
            && baselineAccountRevisions.count <= maximumBaselineAccounts
            && baselineAccountRevisions.allSatisfy {
                $0.key != zeroUUID && $0.value > 0 && $0.value <= UInt64(Int64.max) - 2
            }
            && createdAt.timeIntervalSinceReferenceDate.isFinite
            && expiresAt.timeIntervalSinceReferenceDate.isFinite
            && expiresAt > createdAt
            && (browserOpenedAt.map {
                $0.timeIntervalSinceReferenceDate.isFinite
                    && $0 >= createdAt
                    && $0 <= expiresAt
            } ?? true)
    }

    private static func isSafeIdempotencyByte(_ byte: UInt8) -> Bool {
        (byte >= 65 && byte <= 90)
            || (byte >= 97 && byte <= 122)
            || (byte >= 48 && byte <= 57)
            || [45, 46, 95].contains(byte)
    }

    private static let zeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )
}

@MainActor
protocol GoogleOAuthStartJournalStoring: AnyObject {
    func load() throws -> GoogleOAuthStartJournal?
    func save(_ journal: GoogleOAuthStartJournal) throws
    func delete() throws
}

enum GoogleOAuthStartJournalStoreError: LocalizedError {
    case invalidStoredJournal
    case writeFailed

    var errorDescription: String? {
        switch self {
        case .invalidStoredJournal:
            "The saved Google connection recovery request is invalid. Remove it after its expiry before starting another connection."
        case .writeFailed:
            "DayWeave could not save the non-secret Google connection recovery request. No authorization request was sent."
        }
    }
}

@MainActor
final class UserDefaultsGoogleOAuthStartJournalStore: GoogleOAuthStartJournalStoring {
    static let defaultKey = "dayweave.google.oauth-start-journal.v1"
    static let maximumEncodedBytes = 2 * 1_048_576

    private let defaults: UserDefaults
    private let key: String

    init(defaults: UserDefaults = .standard, key: String = defaultKey) {
        self.defaults = defaults
        self.key = key
    }

    func load() throws -> GoogleOAuthStartJournal? {
        guard let stored = defaults.object(forKey: key) else { return nil }
        guard let data = stored as? Data,
              data.count <= Self.maximumEncodedBytes else {
            throw GoogleOAuthStartJournalStoreError.invalidStoredJournal
        }
        do {
            return try JSONDecoder().decode(GoogleOAuthStartJournal.self, from: data)
        } catch {
            throw GoogleOAuthStartJournalStoreError.invalidStoredJournal
        }
    }

    func save(_ journal: GoogleOAuthStartJournal) throws {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let data = try encoder.encode(journal)
            guard data.count <= Self.maximumEncodedBytes else {
                throw GoogleOAuthStartJournalStoreError.writeFailed
            }
            defaults.set(data, forKey: key)
            guard defaults.synchronize(), defaults.data(forKey: key) == data else {
                throw GoogleOAuthStartJournalStoreError.writeFailed
            }
        } catch let error as GoogleOAuthStartJournalStoreError {
            throw error
        } catch {
            throw GoogleOAuthStartJournalStoreError.writeFailed
        }
    }

    func delete() throws {
        defaults.removeObject(forKey: key)
        guard defaults.synchronize(), defaults.object(forKey: key) == nil else {
            throw GoogleOAuthStartJournalStoreError.writeFailed
        }
    }
}

private struct GoogleIntegrationOperation: Sendable {
    let id: UUID
    let generation: UInt64
    let transport: any GoogleIntegrationTransport
    let configurationIdentifier: String
}

private enum GoogleIntegrationLocalError: LocalizedError {
    case inactive
    case operationInProgress
    case invalidAuthorizationURL
    case authorizationExpired
    case recoveryBelongsToAnotherConfiguration
    case invalidCollectionRole
    case publicationPolicyForbidden
    case invalidMutationResponse
    case refreshCompletionPending

    var errorDescription: String? {
        switch self {
        case .inactive:
            "Unlock DayWeave to use Google."
        case .operationInProgress:
            "Wait for the current Google operation to finish."
        case .invalidAuthorizationURL:
            "The DayWeave API returned an unsafe Google authorization page. Nothing was opened."
        case .authorizationExpired:
            "The Google authorization page expired. Retry the exact connection request."
        case .recoveryBelongsToAnotherConfiguration:
            "A Google connection request belongs to another DayWeave API session. Restore that session or wait for the request to expire."
        case .invalidCollectionRole:
            "Publishing requires an active Google Calendar write grant and an owner or writer calendar. Google Tasks remain read-only."
        case .publicationPolicyForbidden:
            "Publication options are available only on a writable Google Calendar."
        case .invalidMutationResponse:
            "The Google integration returned a result for the wrong account, source, or revision. Refresh before trying again."
        case .refreshCompletionPending:
            "DayWeave is already tracking an accepted import for this Google account. Check status before requesting another."
        }
    }
}

@MainActor
final class GoogleIntegrationStore: ObservableObject {
    static let googleCalendarWriteScope = "https://www.googleapis.com/auth/calendar"
    typealias TransportProvider = () throws -> any GoogleIntegrationTransport
    typealias AuthorizationOpener = (URL) -> Bool
    typealias Sleep = @Sendable (Duration) async throws -> Void

    @Published private(set) var accounts: [GoogleAccount] = []
    @Published private(set) var collectionsByAccount: [UUID: [GoogleSyncCollection]] = [:]
    @Published private(set) var syncStatusByAccount: [UUID: GoogleSyncStatus] = [:]
    @Published private(set) var cleanupStatus: GoogleOAuthCleanupStatus?
    @Published private(set) var status: GoogleIntegrationStatus = .privacyProtected
    @Published private(set) var isBusy = false
    @Published private(set) var canOpenAuthorization = false
    @Published private(set) var canRetryAuthorization = false
    @Published private(set) var canCheckAuthorization = false
    @Published private(set) var mutationRecoveryRequired = false
    @Published private(set) var authorizationRecoveryRequiresAttention = false
    @Published private(set) var authorizationRecoveryResetRequired = false
    @Published private(set) var disconnectRecoveryRequiresAttention = false
    @Published private(set) var disconnectRecoveryResetRequired = false
    @Published private(set) var refreshCompletionRecoveryRequiresAttention = false
    @Published private(set) var refreshCompletionRecoveryResetRequired = false
    @Published private(set) var orphanedRecoveryRequiresConfirmation = false
    @Published private(set) var pendingDisconnectAccountID: UUID?
    @Published private(set) var pendingRefreshAccountIDs: Set<UUID> = []
    @Published private(set) var retryableRefreshAccountIDs: Set<UUID> = []
    @Published private(set) var credentialTransitionInProgress = false

    private let configurationStore: (any SuggestionAPIConfigurationStoring)?
    private let authCoordinator: DurableAuthCoordinator?
    private let session: URLSession
    private let transportProvider: TransportProvider?
    private let journalStore: any GoogleOAuthStartJournalStoring
    private let disconnectJournalStore: any GoogleDisconnectRetryJournalStoring
    private let refreshCompletionJournalStore:
        any GooglePendingRefreshCompletionJournalStoring
    private let authorizationOpener: AuthorizationOpener
    private let now: @Sendable () -> Date
    private let sleep: Sleep
    private let authorizationPollLimit: Int

    private var isActive = false
    private var generation: UInt64 = 0
    private var operationID: UUID?
    private var activeTask: Task<Void, Never>?
    private var pendingAuthorization: GoogleOAuthAuthorization?
    private var pendingAuthorizationConfigurationIdentifier: String?
    private var trustedConfigurationIdentifier: String?
    private var mutationRecoveryConfigurationIdentifier: String?
    private var importCompletionHandler: (() async -> Bool)?
    private var pendingRefreshPresentationDate: Date?
    private var disconnectCompletionRequiresComposition = false

    init(
        configurationStore: any SuggestionAPIConfigurationStoring =
            UserDefaultsSuggestionAPIConfigurationStore(),
        authCoordinator: DurableAuthCoordinator,
        session: URLSession = makeDayWeaveEphemeralSession(),
        journalStore: any GoogleOAuthStartJournalStoring =
            UserDefaultsGoogleOAuthStartJournalStore(),
        disconnectJournalStore: any GoogleDisconnectRetryJournalStoring =
            UserDefaultsGoogleDisconnectRetryJournalStore(),
        refreshCompletionJournalStore:
            any GooglePendingRefreshCompletionJournalStoring =
            UserDefaultsGooglePendingRefreshCompletionJournalStore(),
        authorizationOpener: @escaping AuthorizationOpener = { NSWorkspace.shared.open($0) },
        authorizationPollLimit: Int = 12,
        now: @escaping @Sendable () -> Date = Date.init,
        sleep: @escaping Sleep = { try await Task.sleep(for: $0) }
    ) {
        self.configurationStore = configurationStore
        self.authCoordinator = authCoordinator
        self.session = session
        transportProvider = nil
        self.journalStore = journalStore
        self.disconnectJournalStore = disconnectJournalStore
        self.refreshCompletionJournalStore = refreshCompletionJournalStore
        self.authorizationOpener = authorizationOpener
        self.authorizationPollLimit = max(1, authorizationPollLimit)
        self.now = now
        self.sleep = sleep
    }

    init(
        transportProvider: @escaping TransportProvider,
        journalStore: any GoogleOAuthStartJournalStoring,
        disconnectJournalStore: any GoogleDisconnectRetryJournalStoring,
        refreshCompletionJournalStore:
            any GooglePendingRefreshCompletionJournalStoring,
        authorizationOpener: @escaping AuthorizationOpener = { _ in true },
        authorizationPollLimit: Int = 3,
        now: @escaping @Sendable () -> Date = Date.init,
        sleep: @escaping Sleep = { _ in }
    ) {
        configurationStore = nil
        authCoordinator = nil
        session = makeDayWeaveEphemeralSession()
        self.transportProvider = transportProvider
        self.journalStore = journalStore
        self.disconnectJournalStore = disconnectJournalStore
        self.refreshCompletionJournalStore = refreshCompletionJournalStore
        self.authorizationOpener = authorizationOpener
        self.authorizationPollLimit = max(1, authorizationPollLimit)
        self.now = now
        self.sleep = sleep
    }

    var sidebarMessage: String {
        if authorizationRecoveryResetRequired
            || disconnectRecoveryResetRequired
            || refreshCompletionRecoveryResetRequired
            || mutationRecoveryRequired
            || orphanedRecoveryRequiresConfirmation {
            return "Google · needs attention"
        }
        switch status {
        case .privacyProtected:
            return "Google · locked"
        case .configurationRequired:
            return "Google · setup required"
        case .awaitingAuthorization, .authorizationOutcomeUnknown:
            return "Google · connecting"
        case .offline:
            return "Google · offline"
        case .failed:
            return "Google · needs attention"
        case .loading where credentialTransitionInProgress:
            return "Google · authentication changing"
        case .ready, .loading, .connected, .refreshQueued:
            break
        }
        if accounts.isEmpty {
            return switch status {
            case .loading:
                "Google · checking"
            default:
                "Google · not connected"
            }
        }
        if syncStatusByAccount.values.contains(where: { $0.run?.state == .running }) {
            return "Google · importing"
        }
        if accounts.contains(where: { $0.status == .reauthorizationRequired }) {
            return "Google · reconnect required"
        }
        if accounts.contains(where: { $0.status == .revocationFailed }) {
            return "Google · needs attention"
        }
        if accounts.allSatisfy({ $0.status == .disconnecting || $0.status == .revoked }) {
            return "Google · disconnecting"
        }
        let activeCount = accounts.filter { $0.status == .active }.count
        if activeCount > 0 {
            return "Google · \(activeCount) connected"
        }
        return "Google · paused"
    }

    var sidebarSymbol: String {
        if authorizationRecoveryResetRequired
            || disconnectRecoveryResetRequired
            || refreshCompletionRecoveryResetRequired
            || mutationRecoveryRequired
            || orphanedRecoveryRequiresConfirmation
            || status.isFailure {
            return "exclamationmark.triangle"
        }
        if case .offline = status { return "wifi.slash" }
        if isBusy || credentialTransitionInProgress { return "arrow.triangle.2.circlepath" }
        if accounts.contains(where: { $0.status == .reauthorizationRequired }) {
            return "person.crop.circle.badge.exclamationmark"
        }
        if accounts.contains(where: { $0.status == .revocationFailed }) {
            return "exclamationmark.triangle"
        }
        if accounts.contains(where: { $0.status == .active }) { return "checkmark.circle" }
        if accounts.contains(where: { $0.status == .paused }) { return "pause.circle" }
        return accounts.isEmpty ? "circle.dashed" : "clock"
    }

    var hasPendingAuthorizationRecovery: Bool {
        if canOpenAuthorization || canRetryAuthorization || canCheckAuthorization {
            return true
        }
        if authorizationRecoveryRequiresAttention { return true }
        do {
            guard let journal = try journalStore.load() else { return false }
            return journal.isValid(now: now())
        } catch {
            return true
        }
    }

    var authorizationStartIsFenced: Bool {
        cleanupStatus?.revocationFenced == true
            || cleanupStatus?.operatorRecoveryRequired == true
    }

    var hasPendingRecovery: Bool {
        if mutationRecoveryRequired || hasPendingAuthorizationRecovery { return true }
        if disconnectRecoveryRequiresAttention
            || disconnectRecoveryResetRequired
            || refreshCompletionRecoveryRequiresAttention
            || refreshCompletionRecoveryResetRequired {
            return true
        }
        do {
            if try disconnectJournalStore.load(now: now()) != nil { return true }
        } catch {
            return true
        }
        do {
            return try !refreshCompletionJournalStore.load(now: now()).isEmpty
        } catch {
            return true
        }
    }

    var recoveryResetRequired: Bool {
        authorizationRecoveryResetRequired
            || disconnectRecoveryResetRequired
            || refreshCompletionRecoveryResetRequired
    }

    func hasPendingDisconnectRecovery(for account: GoogleAccount) -> Bool {
        pendingDisconnectAccountID == account.id
    }

    func hasPendingRefreshCompletion(for account: GoogleAccount) -> Bool {
        pendingRefreshAccountIDs.contains(account.id)
    }

    func canRetryPendingRefresh(for account: GoogleAccount) -> Bool {
        retryableRefreshAccountIDs.contains(account.id)
    }

    func beginCredentialTransition() -> Bool {
        guard !credentialTransitionInProgress,
              operationID == nil,
              !hasPendingRecovery else {
            status = .failed(
                "Finish or reset the pending Google recovery before changing DayWeave authentication."
            )
            return false
        }
        credentialTransitionInProgress = true
        return true
    }

    func canRepairAuthentication(boundTo baseURL: DayWeaveAPIBaseURL) -> Bool {
        guard hasPendingRecovery,
              !authorizationRecoveryResetRequired,
              !disconnectRecoveryResetRequired,
              !refreshCompletionRecoveryResetRequired else { return false }
        do {
            var configurationIdentifiers: [String] = []
            if let journal = try journalStore.load(), journal.isValid(now: now()) {
                configurationIdentifiers.append(journal.configurationIdentifier)
            }
            if let journal = try disconnectJournalStore.load(now: now()) {
                configurationIdentifiers.append(journal.configurationIdentifier)
            }
            configurationIdentifiers.append(contentsOf:
                try refreshCompletionJournalStore.load(now: now())
                    .map(\.configurationIdentifier)
            )
            if mutationRecoveryRequired {
                guard let mutationRecoveryConfigurationIdentifier else { return false }
                configurationIdentifiers.append(mutationRecoveryConfigurationIdentifier)
            }
            guard !configurationIdentifiers.isEmpty else { return false }
            let prefix = baseURL.canonicalConfigurationIdentifier + "|auth="
            return configurationIdentifiers.allSatisfy { $0.hasPrefix(prefix) }
        } catch {
            return false
        }
    }

    func beginCredentialRepairTransition(boundTo baseURL: DayWeaveAPIBaseURL) -> Bool {
        guard !credentialTransitionInProgress,
              operationID == nil,
              canRepairAuthentication(boundTo: baseURL) else {
            status = .failed(
                "Authentication repair must use the same DayWeave API base as every pending Google recovery."
            )
            return false
        }
        credentialTransitionInProgress = true
        return true
    }

    func endCredentialTransition() {
        credentialTransitionInProgress = false
    }

    func installImportCompletionVerifier(_ handler: @escaping () async -> Bool) {
        importCompletionHandler = handler
    }

    func activate(automaticallyReload: Bool = true) {
        guard !isActive else { return }
        isActive = true
        generation &+= 1
        status = .ready
        refreshRecoveryPresentation()
        guard automaticallyReload else { return }
        Task { @MainActor [weak self] in
            await self?.reload()
        }
    }

    func suspendForPrivacyBoundary() {
        guard isActive || !accounts.isEmpty || pendingAuthorization != nil else {
            status = .privacyProtected
            return
        }
        isActive = false
        generation &+= 1
        cancelActiveOperation()
        clearPrivatePresentation()
        trustedConfigurationIdentifier = nil
        orphanedRecoveryRequiresConfirmation = false
        disconnectCompletionRequiresComposition = false
        status = .privacyProtected
    }

    func configurationDidChange() {
        generation &+= 1
        cancelActiveOperation()
        clearPrivatePresentation()
        trustedConfigurationIdentifier = nil
        orphanedRecoveryRequiresConfirmation = false
        disconnectCompletionRequiresComposition = false
        guard isActive else {
            status = .privacyProtected
            return
        }
        status = .ready
        refreshRecoveryPresentation()
        Task { @MainActor [weak self] in
            await self?.reload()
        }
    }

    func waitForCurrentOperation() async {
        await activeTask?.value
    }

    func reload() async {
        guard let operation = beginOperation(message: "Checking connected Google accounts…") else {
            return
        }
        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performReload(operation)
        }
        activeTask = task
        await task.value
    }

    private func performReload(_ operation: GoogleIntegrationOperation) async {
        defer { finishOperation(operation.id) }
        do {
            let snapshot = try await operation.transport.googleAccounts()
            try requireCurrent(operation)
            var loadedCollections: [UUID: [GoogleSyncCollection]] = [:]
            var loadedStatuses: [UUID: GoogleSyncStatus] = [:]
            for account in snapshot.accounts where account.status == .active {
                loadedCollections[account.id] = try await operation.transport.googleCollections(
                    accountID: account.id
                )
                try requireCurrent(operation)
                loadedStatuses[account.id] = try await operation.transport.googleSyncStatus(
                    accountID: account.id
                )
                try requireCurrent(operation)
            }
            await commitTrustedSnapshot(
                snapshot,
                collections: loadedCollections,
                statuses: loadedStatuses,
                operation: operation
            )
        } catch {
            handleReadError(error, operation: operation)
        }
    }

    private func commitTrustedSnapshot(
        _ snapshot: GoogleAccountsSnapshot,
        collections: [UUID: [GoogleSyncCollection]],
        statuses: [UUID: GoogleSyncStatus],
        operation: GoogleIntegrationOperation
    ) async {
        guard operationIsCurrent(operation) else { return }
        accounts = snapshot.accounts.sorted(by: Self.accountSort)
        collectionsByAccount = collections
        syncStatusByAccount = statuses
        cleanupStatus = snapshot.cleanup
        trustedConfigurationIdentifier = operation.configurationIdentifier
        mutationRecoveryRequired = false
        mutationRecoveryConfigurationIdentifier = nil
        orphanedRecoveryRequiresConfirmation = false
        disconnectCompletionRequiresComposition = false
        reconcileAuthorizationJournal(with: snapshot.accounts, operation: operation)
        await reconcileDisconnectJournal(with: snapshot.accounts, operation: operation)
        await reconcileRefreshCompletionJournals(
            with: snapshot.accounts,
            statuses: statuses,
            operation: operation
        )
        guard operationIsCurrent(operation) else { return }
        if authorizationRecoveryResetRequired
            || disconnectRecoveryResetRequired
            || refreshCompletionRecoveryResetRequired {
            status = .failed(
                "A saved Google recovery record is unreadable. Reset it explicitly before continuing."
            )
            return
        }
        if canOpenAuthorization, let pendingAuthorization {
            status = .awaitingAuthorization(expiresAt: pendingAuthorization.expiresAt)
            return
        }
        if canCheckAuthorization || canRetryAuthorization,
           let journal = try? journalStore.load() {
            status = .authorizationOutcomeUnknown(expiresAt: journal.expiresAt)
            return
        }
        if disconnectRecoveryRequiresAttention {
            if disconnectCompletionRequiresComposition {
                status = .failed(
                    "Google is disconnected, but DayWeave must finish a verified schedule refresh before clearing recovery."
                )
            } else if orphanedRecoveryRequiresConfirmation,
                      pendingDisconnectAccountID == nil {
                status = .failed(
                    "A saved disconnect belongs to a previous authenticated API session and cannot be proven here. Restore that session or explicitly abandon the orphaned recovery."
                )
            } else if let accountID = pendingDisconnectAccountID,
               let account = accounts.first(where: { $0.id == accountID }) {
                switch account.status {
                case .disconnecting:
                    status = .loading(
                        "Google account disconnection is pending. Retry the exact saved disconnect request."
                    )
                case .revocationFailed:
                    status = .failed(
                        "Google revocation failed. Retry the exact saved disconnect request."
                    )
                case .active, .paused, .reauthorizationRequired:
                    status = .failed(
                        "The prior disconnect did not finish. Retry the exact saved request."
                    )
                case .revoked:
                    break
                }
            } else {
                status = .failed(
                    "A saved disconnect recovery belongs to another DayWeave API session. Restore that session before continuing."
                )
            }
            return
        }
        if orphanedRecoveryRequiresConfirmation {
            status = .failed(
                "A saved Google import belongs to a previous authenticated API session and cannot be proven here. Restore that session or explicitly abandon the orphaned recovery."
            )
            return
        }
        if refreshCompletionRecoveryRequiresAttention,
           let requestedAt = pendingRefreshPresentationDate {
            status = .refreshQueued(
                requestedAt: requestedAt,
                message: "Waiting for a queued Google import to finish"
            )
            return
        }
        let activeCount = accounts.filter { $0.status == .active }.count
        if accounts.contains(where: { $0.status == .revocationFailed }) {
            status = .failed(
                "Google account revocation needs attention. Retry the exact disconnect request if it is available."
            )
        } else if accounts.contains(where: { $0.status == .reauthorizationRequired }) {
            status = .failed("Google authorization must be renewed before imports can continue.")
        } else if !accounts.isEmpty,
                  accounts.allSatisfy({ $0.status == .disconnecting || $0.status == .revoked }) {
            status = .loading("Google account disconnection is still in progress…")
        } else {
            status = .connected(
                updatedAt: now(),
                message: activeCount == 0
                    ? (accounts.isEmpty
                        ? "No Google account is connected"
                        : "Google sync is paused")
                    : "Loaded \(activeCount) Google account\(activeCount == 1 ? "" : "s")"
            )
        }
    }

    func connectGoogleAccount() async {
        guard mutationRecoveryIsClear() else { return }
        guard !authorizationStartIsFenced else {
            status = .failed(
                "Google cleanup must finish before starting a new authorization request."
            )
            return
        }
        guard !accounts.contains(where: {
            $0.status == .disconnecting || $0.status == .revocationFailed
        }) else {
            status = .failed(
                "Finish the pending Google account revocation before connecting another account."
            )
            return
        }
        let request = GoogleOAuthStartRequest(
            services: [],
            forceConsent: false,
            loginHint: nil,
            accountID: nil,
            connectNew: !accounts.isEmpty,
            makeDefault: accounts.isEmpty
        )
        await startAuthorization(request: request, existingJournal: nil)
    }

    func reauthorizeGoogleAccount(_ account: GoogleAccount) async {
        guard mutationRecoveryIsClear() else { return }
        guard !authorizationStartIsFenced else {
            status = .failed(
                "Google cleanup must finish before starting a new authorization request."
            )
            return
        }
        guard account.status == .active
                || account.status == .paused
                || account.status == .reauthorizationRequired else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        guard !hasPendingRefreshCompletion(for: account)
                || requiresReauthorization(for: account) else {
            status = .failed(
                "Finish the accepted import and canonical recomposition before reauthorizing this account."
            )
            return
        }
        guard accounts.contains(where: { $0.id == account.id && $0.revision == account.revision })
        else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        let request = GoogleOAuthStartRequest(
            services: [],
            forceConsent: true,
            loginHint: nil,
            accountID: account.id,
            connectNew: false,
            makeDefault: account.isDefault
        )
        await startAuthorization(request: request, existingJournal: nil)
    }

    /// Requests only the additional Calendar write scope for an existing
    /// account. Google Tasks stay read-only in this client slice, and the
    /// returned provider grant still has to be observed authoritatively before
    /// any calendar may be configured as writable.
    func enableCalendarPublishing(for account: GoogleAccount) async {
        guard mutationRecoveryIsClear() else { return }
        guard !authorizationStartIsFenced else {
            status = .failed(
                "Google cleanup must finish before expanding Calendar access."
            )
            return
        }
        guard accountIsCurrent(account),
              account.status == .active || account.status == .paused else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        guard !hasPendingRefreshCompletion(for: account) else {
            status = .failed(
                "Finish the accepted import and canonical recomposition before expanding Calendar access."
            )
            return
        }
        guard !hasCalendarPublishingScope(for: account) else {
            status = .connected(
                updatedAt: now(),
                message: "Calendar publishing access is already enabled"
            )
            return
        }
        let request = GoogleOAuthStartRequest(
            services: [.calendar],
            forceConsent: true,
            loginHint: nil,
            accountID: account.id,
            connectNew: false,
            makeDefault: account.isDefault
        )
        await startAuthorization(request: request, existingJournal: nil)
    }

    func hasCalendarPublishingScope(for account: GoogleAccount) -> Bool {
        account.grantedScopes.contains(Self.googleCalendarWriteScope)
    }

    func canEnableCalendarPublishing(for account: GoogleAccount) -> Bool {
        accountIsCurrent(account)
            && (account.status == .active || account.status == .paused)
            && !hasCalendarPublishingScope(for: account)
            && !authorizationStartIsFenced
            && !hasPendingRefreshCompletion(for: account)
    }

    func requiresReauthorization(for account: GoogleAccount) -> Bool {
        account.status == .reauthorizationRequired
            || syncStatusByAccount[account.id]?.run?.state == .reauthorizationRequired
    }

    func retryExactAuthorizationRequest() async {
        do {
            guard let journal = try journalStore.load(), journal.isValid(now: now()) else {
                try? journalStore.delete()
                authorizationRecoveryRequiresAttention = false
                canRetryAuthorization = false
                canCheckAuthorization = false
                status = .failed(GoogleIntegrationLocalError.authorizationExpired.localizedDescription)
                return
            }
            guard journal.browserOpenedAt == nil else {
                canRetryAuthorization = false
                canCheckAuthorization = true
                await reload()
                return
            }
            await startAuthorization(request: journal.request, existingJournal: journal)
        } catch {
            authorizationRecoveryRequiresAttention = true
            authorizationRecoveryResetRequired = error is GoogleOAuthStartJournalStoreError
            canRetryAuthorization = false
            canCheckAuthorization = false
            status = .failed(safeErrorMessage(
                error,
                fallback: "The saved Google connection request could not be loaded safely."
            ))
        }
    }

    private func startAuthorization(
        request: GoogleOAuthStartRequest,
        existingJournal: GoogleOAuthStartJournal?
    ) async {
        guard request.isValid else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        let authorizationPurpose = request.services == [.calendar]
            ? "Preparing Google Calendar publishing access…"
            : "Preparing a Google connection…"
        guard let operation = beginOperation(message: authorizationPurpose)
        else { return }

        let journal: GoogleOAuthStartJournal
        do {
            if let existingJournal {
                guard existingJournal.isValid(now: now()),
                      existingJournal.request == request,
                      existingJournal.configurationIdentifier
                        == operation.configurationIdentifier else {
                    throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
                }
                journal = existingJournal
            } else {
                if let stored = try journalStore.load() {
                    if stored.isValid(now: now()) {
                        throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
                    }
                    try journalStore.delete()
                }
                let createdAt = now()
                journal = GoogleOAuthStartJournal(
                    request: request,
                    idempotencyKey: "mac-google-oauth-\(UUID().uuidString.lowercased())",
                    configurationIdentifier: operation.configurationIdentifier,
                    baselineAccountRevisions: Dictionary(
                        uniqueKeysWithValues: accounts.map { ($0.id, $0.revision) }
                    ),
                    createdAt: createdAt,
                    expiresAt: createdAt.addingTimeInterval(
                        GoogleOAuthStartJournal.maximumLifetime
                    )
                )
                try journalStore.save(journal)
                authorizationRecoveryRequiresAttention = true
                authorizationRecoveryResetRequired = false
            }
        } catch {
            finishOperation(operation.id)
            if error is GoogleOAuthStartJournalStoreError {
                authorizationRecoveryRequiresAttention = true
                authorizationRecoveryResetRequired = true
            }
            status = .failed(safeErrorMessage(
                error,
                fallback: "The Google connection recovery request could not be saved safely."
            ))
            return
        }

        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performAuthorizationStart(operation, journal: journal)
        }
        activeTask = task
        await task.value
    }

    private func performAuthorizationStart(
        _ operation: GoogleIntegrationOperation,
        journal: GoogleOAuthStartJournal
    ) async {
        defer { finishOperation(operation.id) }
        do {
            let authorization = try await operation.transport.startGoogleOAuth(
                journal.request,
                idempotencyKey: journal.idempotencyKey
            )
            let url = try validatedAuthorizationURL(authorization)
            try requireCurrent(operation)
            let baseline = try await operation.transport.googleAccounts()
            try requireCurrent(operation)
            if let accountID = journal.request.accountID,
               !baseline.accounts.contains(where: { $0.id == accountID }) {
                throw GoogleIntegrationLocalError.invalidMutationResponse
            }
            var updatedJournal = journal
            updatedJournal.expiresAt = authorization.expiresAt
            updatedJournal.baselineAccountRevisions = Dictionary(
                uniqueKeysWithValues: baseline.accounts.map { ($0.id, $0.revision) }
            )
            try journalStore.save(updatedJournal)
            try requireCurrent(operation)
            authorizationRecoveryRequiresAttention = true
            authorizationRecoveryResetRequired = false
            pendingAuthorization = authorization
            pendingAuthorizationConfigurationIdentifier = operation.configurationIdentifier
            canOpenAuthorization = true
            canRetryAuthorization = false
            canCheckAuthorization = false
            // Force complete validation before publishing the availability bit.
            _ = url
            status = .awaitingAuthorization(expiresAt: authorization.expiresAt)
        } catch {
            guard operationIsCurrent(operation) else { return }
            pendingAuthorization = nil
            pendingAuthorizationConfigurationIdentifier = nil
            canOpenAuthorization = false
            canCheckAuthorization = false
            if isAuthenticationError(error) {
                clearPrivatePresentation()
                trustedConfigurationIdentifier = nil
                authorizationRecoveryRequiresAttention = true
                status = .configurationRequired(
                    "Authenticate this Mac again, then retry the exact saved Google connection request."
                )
            } else if isAmbiguousMutationError(error) {
                canRetryAuthorization = true
                status = .authorizationOutcomeUnknown(expiresAt: journal.expiresAt)
            } else {
                canRetryAuthorization = true
                status = .failed(safeErrorMessage(
                    error,
                    fallback: "The Google connection request could not be prepared safely."
                ))
            }
        }
    }

    @discardableResult
    func openAuthorizationPage() -> Bool {
        guard isActive,
              !isBusy,
              !credentialTransitionInProgress,
              let authorization = pendingAuthorization,
              let expectedConfiguration = pendingAuthorizationConfigurationIdentifier else {
            status = .failed(GoogleIntegrationLocalError.inactive.localizedDescription)
            return false
        }
        var reservedOperation: GoogleIntegrationOperation?
        do {
            let transport = try makeTransport()
            guard transport.configurationIdentifier == expectedConfiguration,
                  let journal = try journalStore.load(),
                  journal.configurationIdentifier == expectedConfiguration,
                  journal.isValid(now: now()) else {
                throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
            }
            let url = try validatedAuthorizationURL(authorization)
            guard let operation = beginOperation(
                message: "Opening the private Google authorization page…"
            ) else { return false }
            reservedOperation = operation
            guard operation.configurationIdentifier == expectedConfiguration else {
                throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
            }
            // Consume the in-memory bearer capability before handing it to the
            // workspace. A concurrent privacy/configuration boundary runs on
            // this same actor and cannot republish it after this point.
            pendingAuthorization = nil
            pendingAuthorizationConfigurationIdentifier = nil
            canOpenAuthorization = false
            guard authorizationOpener(url) else {
                finishOperation(operation.id)
                reservedOperation = nil
                canRetryAuthorization = true
                canCheckAuthorization = false
                status = .failed(
                    "macOS could not open Google. Retry the exact saved connection request."
                )
                return false
            }
            var openedJournal = journal
            openedJournal.browserOpenedAt = now()
            try journalStore.save(openedJournal)
            canRetryAuthorization = false
            canCheckAuthorization = true
            status = .loading("Waiting for Google to finish the connection…")
            let task = Task<Void, Never> { @MainActor [weak self] in
                await self?.performAuthorizationPolling(operation, journal: openedJournal)
            }
            activeTask = task
            return true
        } catch {
            if let reservedOperation {
                finishOperation(reservedOperation.id)
            }
            pendingAuthorization = nil
            pendingAuthorizationConfigurationIdentifier = nil
            canOpenAuthorization = false
            if error is GoogleOAuthStartJournalStoreError {
                refreshAuthorizationJournalPresentation()
                if authorizationRecoveryResetRequired {
                    status = .failed(
                        "The saved Google connection recovery record is unreadable. Reset it explicitly before continuing."
                    )
                } else {
                    status = .failed(
                        "Google may already be open. DayWeave retained the exact request; check or retry it according to the saved recovery state."
                    )
                }
            } else if isAuthenticationError(error) {
                clearPrivatePresentation()
                trustedConfigurationIdentifier = nil
                authorizationRecoveryRequiresAttention = true
                status = .configurationRequired(
                    "Authenticate this Mac again before reopening the saved Google connection."
                )
            } else {
                canRetryAuthorization = true
                status = .failed(safeErrorMessage(
                    error,
                    fallback: "The private Google authorization page could not be opened safely."
                ))
            }
            return false
        }
    }

    func checkAuthorization() async {
        await reload()
    }

    func resetUnreadableAuthorizationRecovery() async {
        guard authorizationRecoveryResetRequired, !isBusy else { return }
        do {
            try journalStore.delete()
            authorizationRecoveryRequiresAttention = false
            authorizationRecoveryResetRequired = false
            canOpenAuthorization = false
            canRetryAuthorization = false
            canCheckAuthorization = false
            await reload()
        } catch {
            authorizationRecoveryRequiresAttention = true
            authorizationRecoveryResetRequired = true
            status = .failed(
                "The unreadable Google recovery record could not be reset. Try again after checking local storage."
            )
        }
    }

    func resetUnreadableRecovery() async {
        guard recoveryResetRequired,
              !isBusy,
              operationID == nil,
              !credentialTransitionInProgress else { return }
        let resetOperationID = UUID()
        operationID = resetOperationID
        isBusy = true
        status = .loading("Verifying the schedule before resetting Google recovery…")
        defer { finishOperation(resetOperationID) }
        if disconnectRecoveryResetRequired || refreshCompletionRecoveryResetRequired {
            let resetGeneration = generation
            guard let importCompletionHandler else {
                status = .failed(
                    "Sync and compose the current DayWeave schedule before resetting this recovery."
                )
                return
            }
            let completionVerified = await importCompletionHandler()
            guard operationID == resetOperationID,
                  generation == resetGeneration,
                  !credentialTransitionInProgress else { return }
            guard completionVerified else {
                status = .failed(
                    "DayWeave could not verify a fresh schedule composition, so the recovery record was retained."
                )
                return
            }
        }
        do {
            if authorizationRecoveryResetRequired {
                try journalStore.delete()
            }
            if disconnectRecoveryResetRequired {
                try disconnectJournalStore.delete()
            }
            if refreshCompletionRecoveryResetRequired {
                try refreshCompletionJournalStore.deleteAll()
            }
            authorizationRecoveryRequiresAttention = false
            authorizationRecoveryResetRequired = false
            disconnectRecoveryRequiresAttention = false
            disconnectRecoveryResetRequired = false
            refreshCompletionRecoveryRequiresAttention = false
            refreshCompletionRecoveryResetRequired = false
            orphanedRecoveryRequiresConfirmation = false
            disconnectCompletionRequiresComposition = false
            pendingDisconnectAccountID = nil
            pendingRefreshAccountIDs = []
            retryableRefreshAccountIDs = []
            pendingRefreshPresentationDate = nil
            finishOperation(resetOperationID)
            await reload()
        } catch {
            refreshRecoveryPresentation()
            status = .failed(
                "The unreadable Google recovery could not be reset. Try again after checking local storage."
            )
        }
    }

    func abandonOrphanedRecovery() async {
        guard orphanedRecoveryRequiresConfirmation,
              !isBusy,
              !credentialTransitionInProgress,
              let currentConfigurationIdentifier = trustedConfigurationIdentifier else {
            return
        }
        do {
            let visibleAccountIDs = Set(accounts.compactMap { account in
                account.status == .revoked ? nil : account.id
            })
            if let journal = try disconnectJournalStore.load(now: now()),
               journal.configurationIdentifier != currentConfigurationIdentifier,
               !visibleAccountIDs.contains(journal.accountID) {
                try disconnectJournalStore.delete()
            }
            let refreshJournals = try refreshCompletionJournalStore.load(now: now())
            for journal in refreshJournals
            where journal.configurationIdentifier != currentConfigurationIdentifier
                && !visibleAccountIDs.contains(journal.accountID) {
                try refreshCompletionJournalStore.delete(
                    accountID: journal.accountID,
                    configurationIdentifier: journal.configurationIdentifier
                )
            }
            orphanedRecoveryRequiresConfirmation = false
            disconnectCompletionRequiresComposition = false
            refreshDisconnectJournalPresentation()
            refreshCompletionPresentationAfterLocalChange()
            await reload()
        } catch {
            orphanedRecoveryRequiresConfirmation = true
            status = .failed(
                "The orphaned Google recovery could not be removed safely. Its local marker was retained."
            )
        }
    }

    private func performAuthorizationPolling(
        _ operation: GoogleIntegrationOperation,
        journal: GoogleOAuthStartJournal
    ) async {
        defer { finishOperation(operation.id) }
        do {
            try requireCurrent(operation)
            guard journal.configurationIdentifier == operation.configurationIdentifier,
                  let persisted = try journalStore.load(),
                  persisted == journal,
                  persisted.isValid(now: now()) else {
                throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
            }
        } catch {
            guard operationIsCurrent(operation) else { return }
            if error is GoogleOAuthStartJournalStoreError {
                authorizationRecoveryRequiresAttention = true
                authorizationRecoveryResetRequired = true
                canCheckAuthorization = false
                status = .failed(
                    "The saved Google connection recovery record is unreadable. Reset it explicitly before continuing."
                )
            } else {
                status = .failed(safeErrorMessage(
                    error,
                    fallback: "The saved Google connection no longer belongs to this API session."
                ))
            }
            return
        }
        for attempt in 0..<authorizationPollLimit {
            do {
                let snapshot = try await operation.transport.googleAccounts()
                try requireCurrent(operation)
                if authorizationJournalHasAccountChange(journal, accounts: snapshot.accounts) {
                    var loadedCollections: [UUID: [GoogleSyncCollection]] = [:]
                    var loadedStatuses: [UUID: GoogleSyncStatus] = [:]
                    for account in snapshot.accounts where account.status == .active {
                        loadedCollections[account.id] = try await operation.transport
                            .googleCollections(accountID: account.id)
                        try requireCurrent(operation)
                        loadedStatuses[account.id] = try await operation.transport
                            .googleSyncStatus(accountID: account.id)
                        try requireCurrent(operation)
                    }
                    pendingAuthorization = nil
                    pendingAuthorizationConfigurationIdentifier = nil
                    canOpenAuthorization = false
                    canRetryAuthorization = false
                    canCheckAuthorization = true
                    await commitTrustedSnapshot(
                        snapshot,
                        collections: loadedCollections,
                        statuses: loadedStatuses,
                        operation: operation
                    )
                    return
                }
                if attempt + 1 < authorizationPollLimit {
                    try await sleep(.seconds(2))
                    try requireCurrent(operation)
                }
            } catch {
                guard operationIsCurrent(operation) else { return }
                if isAuthenticationError(error) {
                    clearPrivatePresentation()
                    trustedConfigurationIdentifier = nil
                    authorizationRecoveryRequiresAttention = true
                    status = .configurationRequired(
                        "Authenticate this Mac again, then check the saved Google connection."
                    )
                    return
                }
                if isOfflineError(error) {
                    canCheckAuthorization = true
                    status = .offline("The Mac is offline. Google connection checking will resume when you retry.")
                    return
                }
                if attempt + 1 >= authorizationPollLimit {
                    canCheckAuthorization = true
                    status = .authorizationOutcomeUnknown(expiresAt: journal.expiresAt)
                    return
                }
                try? await sleep(.seconds(2))
            }
        }
        guard operationIsCurrent(operation) else { return }
        canCheckAuthorization = true
        status = .authorizationOutcomeUnknown(expiresAt: journal.expiresAt)
    }

    func discoverSources(for account: GoogleAccount) async {
        guard accountIsCurrent(account) else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        guard let operation = beginMutationOperation(
            message: "Discovering Google Calendar and Tasks sources…"
        )
        else { return }
        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performDiscovery(operation, account: account)
        }
        activeTask = task
        await task.value
    }

    private func performDiscovery(
        _ operation: GoogleIntegrationOperation,
        account: GoogleAccount
    ) async {
        defer { finishOperation(operation.id) }
        do {
            let collections: [GoogleSyncCollection]
            let discoveryOutcomeIsUncertain: Bool
            do {
                collections = try await operation.transport.discoverGoogleCollections(
                    accountID: account.id
                )
                discoveryOutcomeIsUncertain = false
            } catch {
                guard isAmbiguousMutationError(error), operationIsCurrent(operation) else {
                    throw error
                }
                // Discovery may have committed before the response was lost.
                // A read is the only safe next step; the POST is never replayed
                // automatically with a new request identity.
                collections = try await operation.transport.googleCollections(accountID: account.id)
                discoveryOutcomeIsUncertain = true
            }
            try requireCurrent(operation)
            guard collections.allSatisfy({ $0.accountID == account.id }) else {
                throw GoogleIntegrationLocalError.invalidMutationResponse
            }
            collectionsByAccount[account.id] = Self.sortCollections(collections)
            trustedConfigurationIdentifier = operation.configurationIdentifier
            if discoveryOutcomeIsUncertain {
                status = .failed(
                    "Google source discovery may be incomplete. The current saved inventory was refreshed; resolve any Google connection issue and retry discovery."
                )
            } else {
                status = .connected(
                    updatedAt: now(),
                    message: "Loaded \(collections.count) Google source\(collections.count == 1 ? "" : "s")"
                )
            }
        } catch {
            handleMutationError(error, operation: operation)
        }
    }

    func configureSource(
        _ collection: GoogleSyncCollection,
        selected: Bool,
        visible: Bool,
        role: GoogleSyncRole,
        calendarPolicy: GoogleCalendarPolicy? = nil
    ) async {
        guard sourceIsCurrent(collection) else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        guard Self.roleIsSupported(role, for: collection.kind),
              role != .writable || selected,
              role != .writable || collection.providerAccessRole.map({
                  $0.caseInsensitiveCompare("owner") == .orderedSame
                      || $0.caseInsensitiveCompare("writer") == .orderedSame
              }) == true,
              role != .writable || accounts.first(where: {
                  $0.id == collection.accountID
              }).map({ hasCalendarPublishingScope(for: $0) }) == true else {
            status = .failed(GoogleIntegrationLocalError.invalidCollectionRole.localizedDescription)
            return
        }
        let targetPolicy: GoogleCalendarPolicy
        if role == .writable {
            targetPolicy = calendarPolicy ?? collection.calendarPolicy
        } else {
            guard calendarPolicy == nil || calendarPolicy?.isReadOnlySafe == true else {
                status = .failed(
                    GoogleIntegrationLocalError.publicationPolicyForbidden.localizedDescription
                )
                return
            }
            targetPolicy = (calendarPolicy ?? collection.calendarPolicy).withoutPublication
        }
        guard let operation = beginMutationOperation(message: "Saving the Google source policy…") else {
            return
        }
        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performSourceConfiguration(
                operation,
                collection: collection,
                selected: selected,
                visible: visible,
                role: role,
                targetPolicy: targetPolicy
            )
        }
        activeTask = task
        await task.value
    }

    private func performSourceConfiguration(
        _ operation: GoogleIntegrationOperation,
        collection: GoogleSyncCollection,
        selected: Bool,
        visible: Bool,
        role: GoogleSyncRole,
        targetPolicy: GoogleCalendarPolicy
    ) async {
        defer { finishOperation(operation.id) }
        do {
            let updated: GoogleSyncCollection
            do {
                updated = try await operation.transport.configureGoogleCollection(
                    accountID: collection.accountID,
                    collectionID: collection.id,
                    expectedRevision: collection.revision,
                    selected: selected,
                    visible: visible,
                    role: role,
                    calendarPolicy: targetPolicy
                )
            } catch {
                guard (isAmbiguousMutationError(error) || isConflictError(error)),
                      operationIsCurrent(operation) else { throw error }
                let authoritative = try await operation.transport.googleCollections(
                    accountID: collection.accountID
                )
                try requireCurrent(operation)
                guard let candidate = authoritative.first(where: { $0.id == collection.id }),
                      candidate.revision > collection.revision,
                      candidate.selected == selected,
                      candidate.visible == visible,
                      candidate.syncRole == role,
                      candidate.calendarPolicy == targetPolicy else {
                    collectionsByAccount[collection.accountID] = Self.sortCollections(authoritative)
                    throw error
                }
                updated = candidate
            }
            try requireCurrent(operation)
            guard updated.id == collection.id,
                  updated.accountID == collection.accountID,
                  updated.revision > collection.revision,
                  updated.selected == selected,
                  updated.visible == visible,
                  updated.syncRole == role,
                  updated.calendarPolicy == targetPolicy else {
                throw GoogleIntegrationLocalError.invalidMutationResponse
            }
            replaceCollection(updated)
            // Configuring a selected source increments the server's account
            // refresh generation. The previously cached run can no longer
            // prove that this collection revision was imported, even when
            // server timestamps happen to compare equal.
            syncStatusByAccount.removeValue(forKey: collection.accountID)
            trustedConfigurationIdentifier = operation.configurationIdentifier
            status = .connected(
                updatedAt: now(),
                message: selected
                    ? "Updated \(updated.displayName) import policy"
                    : "Stopped importing \(updated.displayName)"
            )
        } catch {
            handleMutationError(error, operation: operation)
        }
    }

    func refreshImports(for account: GoogleAccount) async {
        guard accountIsCurrent(account), account.status == .active else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        let existingJournal: GooglePendingRefreshCompletionJournal?
        do {
            existingJournal = try refreshCompletionJournalStore.load(now: now()).first {
                $0.accountID == account.id
            }
            if let existingJournal {
                refreshCompletionRecoveryRequiresAttention = true
                refreshCompletionRecoveryResetRequired = false
                pendingRefreshAccountIDs.insert(account.id)
                guard existingJournal.serverRequestedAt == nil
                        || Self.refreshRunPermitsRetry(
                            syncStatusByAccount[account.id]?.run,
                            journal: existingJournal
                        ) else {
                    status = .failed(
                        GoogleIntegrationLocalError.refreshCompletionPending.localizedDescription
                    )
                    return
                }
                retryableRefreshAccountIDs.insert(account.id)
            }
        } catch {
            refreshCompletionRecoveryRequiresAttention = true
            refreshCompletionRecoveryResetRequired = true
            status = .failed(
                "The saved import completion recovery is unreadable. Reset it explicitly before continuing."
            )
            return
        }
        guard let operation = beginMutationOperation(message: "Requesting a Google import…") else {
            return
        }
        let journal: GooglePendingRefreshCompletionJournal
        if let existingJournal {
            guard existingJournal.configurationIdentifier == operation.configurationIdentifier else {
                finishOperation(operation.id)
                status = .failed(
                    GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
                        .localizedDescription
                )
                return
            }
            if existingJournal.serverRequestedAt == nil {
                journal = existingJournal
            } else {
                guard Self.refreshRunPermitsRetry(
                    syncStatusByAccount[account.id]?.run,
                    journal: existingJournal
                ) else {
                    finishOperation(operation.id)
                    status = .failed(
                        "The accepted Google import is still pending and cannot be requested again yet."
                    )
                    return
                }
                let retryStartedAt = now()
                do {
                    journal = try existingJournal.restarting(at: retryStartedAt)
                    try refreshCompletionJournalStore.save(journal, now: retryStartedAt)
                    refreshCompletionRecoveryRequiresAttention = true
                    refreshCompletionRecoveryResetRequired = false
                    pendingRefreshAccountIDs.insert(account.id)
                    retryableRefreshAccountIDs.insert(account.id)
                    pendingRefreshPresentationDate = retryStartedAt
                } catch {
                    finishOperation(operation.id)
                    refreshCompletionPresentationAfterLocalChange()
                    status = .failed(
                        "The terminal Google import remains saved because its retry boundary could not be persisted safely."
                    )
                    return
                }
            }
        } else {
            let startedAt = now()
            do {
                journal = try GooglePendingRefreshCompletionJournal(
                    accountID: account.id,
                    localRequestStartedAt: startedAt,
                    configurationIdentifier: operation.configurationIdentifier,
                    createdAt: startedAt
                )
                try refreshCompletionJournalStore.save(journal, now: startedAt)
                refreshCompletionRecoveryRequiresAttention = true
                refreshCompletionRecoveryResetRequired = false
                pendingRefreshAccountIDs.insert(account.id)
                retryableRefreshAccountIDs.insert(account.id)
                pendingRefreshPresentationDate = startedAt
            } catch {
                finishOperation(operation.id)
                do {
                    try refreshCompletionJournalStore.delete(
                        accountID: account.id,
                        configurationIdentifier: operation.configurationIdentifier
                    )
                    refreshCompletionPresentationAfterLocalChange()
                } catch {
                    refreshCompletionRecoveryRequiresAttention = true
                    refreshCompletionRecoveryResetRequired = true
                }
                status = .failed(safeErrorMessage(
                    error,
                    fallback: "The Google import recovery record could not be saved safely. No request was sent."
                ))
                return
            }
        }
        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performImportRefresh(
                operation,
                account: account,
                journal: journal,
                isRecoveryRetry: existingJournal != nil
            )
        }
        activeTask = task
        await task.value
    }

    private func performImportRefresh(
        _ operation: GoogleIntegrationOperation,
        account: GoogleAccount,
        journal: GooglePendingRefreshCompletionJournal,
        isRecoveryRetry: Bool
    ) async {
        defer { finishOperation(operation.id) }
        var requestMayHaveBeenDispatched = false
        do {
            guard try refreshCompletionJournalStore.journal(
                accountID: account.id,
                configurationIdentifier: operation.configurationIdentifier,
                now: now()
            ) == journal else {
                throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
            }
            var requestedAt: Date
            var acceptedJournal = journal
            do {
                requestMayHaveBeenDispatched = true
                let accepted = try await operation.transport.requestGoogleSyncRefresh(
                    accountID: account.id,
                    requestID: journal.requestID
                )
                guard accepted.accountID == account.id,
                      accepted.requestID == journal.requestID,
                      accepted.refreshGeneration > 0,
                      accepted.refreshGeneration <= UInt64(Int64.max),
                      accepted.requestedAt.timeIntervalSinceReferenceDate.isFinite else {
                    throw GoogleIntegrationLocalError.invalidMutationResponse
                }
                requestedAt = accepted.requestedAt
                acceptedJournal = try journal.recording(
                    serverRequestedAt: requestedAt,
                    targetRefreshGeneration: accepted.refreshGeneration
                )
                try refreshCompletionJournalStore.save(acceptedJournal, now: now())
            } catch {
                if error is GoogleIntegrationJournalStoreError {
                    throw error
                }
                if isAmbiguousMutationError(error) {
                    guard operationIsCurrent(operation) else { return }
                    status = .failed(
                        "The import response was interrupted. DayWeave retained its exact request and can safely replay it without queuing a duplicate."
                    )
                    return
                }
                // A privacy/configuration boundary may cancel the task after
                // the request reached the server. A stale operation must never
                // delete its persist-before-send identity, even if the final
                // local error appears definitive.
                guard operationIsCurrent(operation) else { return }
                if !isRecoveryRetry {
                    try refreshCompletionJournalStore.delete(
                        accountID: journal.accountID,
                        configurationIdentifier: journal.configurationIdentifier
                    )
                }
                refreshCompletionPresentationAfterLocalChange()
                throw error
            }
            try requireCurrent(operation)
            refreshCompletionRecoveryRequiresAttention = true
            refreshCompletionRecoveryResetRequired = false
            pendingRefreshAccountIDs.insert(account.id)
            retryableRefreshAccountIDs.remove(account.id)
            pendingRefreshPresentationDate = requestedAt
            status = .refreshQueued(
                requestedAt: requestedAt,
                message: "Google accepted the import request"
            )
            await pollForImportCompletion(
                operation,
                accountID: account.id,
                journal: acceptedJournal
            )
        } catch {
            guard operationIsCurrent(operation) else { return }
            if error is GoogleIntegrationJournalStoreError {
                refreshCompletionPresentationAfterLocalChange()
                if refreshCompletionRecoveryResetRequired {
                    status = .failed(
                        "The saved Google import recovery record is unreadable and needs an explicit reset before continuing."
                    )
                } else {
                    if requestMayHaveBeenDispatched
                        && !refreshCompletionRecoveryRequiresAttention {
                        mutationRecoveryRequired = true
                        mutationRecoveryConfigurationIdentifier =
                            operation.configurationIdentifier
                    }
                    status = .failed(
                        requestMayHaveBeenDispatched
                            ? "The import request may have been accepted. Its readable recovery marker was retained for authoritative reconciliation."
                            : "The Google import recovery record could not be accessed safely. No request was sent."
                    )
                }
            } else {
                handleMutationError(error, operation: operation)
            }
        }
    }

    private func pollForImportCompletion(
        _ operation: GoogleIntegrationOperation,
        accountID: UUID,
        journal: GooglePendingRefreshCompletionJournal
    ) async {
        guard let requestedAt = journal.serverRequestedAt,
              let targetRefreshGeneration = journal.targetRefreshGeneration else {
            status = .failed(
                "The Google import is pending authoritative reconciliation before it can be completed."
            )
            return
        }
        for attempt in 0..<authorizationPollLimit {
            do {
                if attempt > 0 {
                    try await sleep(.seconds(2))
                    try requireCurrent(operation)
                }
                let sync = try await operation.transport.googleSyncStatus(accountID: accountID)
                try requireCurrent(operation)
                guard sync.run?.accountID == accountID || sync.run == nil else {
                    throw GoogleIntegrationLocalError.invalidMutationResponse
                }
                syncStatusByAccount[accountID] = sync
                guard let run = sync.run else { continue }
                if run.state == .idle,
                   run.completedRefreshGeneration >= targetRefreshGeneration {
                    let refreshedCollections = try await operation.transport.googleCollections(
                        accountID: accountID
                    )
                    try requireCurrent(operation)
                    guard refreshedCollections.allSatisfy({ $0.accountID == accountID }) else {
                        throw GoogleIntegrationLocalError.invalidMutationResponse
                    }
                    collectionsByAccount[accountID] = Self.sortCollections(refreshedCollections)
                    let changeCounts = [
                        run.importedCount, run.updatedCount, run.deletedCount,
                    ]
                    var changed: UInt64 = 0
                    var countOverflowed = false
                    for count in changeCounts {
                        let addition = changed.addingReportingOverflow(count)
                        changed = addition.partialValue
                        countOverflowed = countOverflowed || addition.overflow
                    }
                    status = .connected(
                        updatedAt: now(),
                        message: countOverflowed
                            ? "Google import finished with a very large change set"
                            : "Google import finished with \(changed) change\(changed == 1 ? "" : "s")"
                    )
                    if let importCompletionHandler {
                        let compositionCompleted = await importCompletionHandler()
                        try requireCurrent(operation)
                        if compositionCompleted {
                            try refreshCompletionJournalStore.delete(
                                accountID: journal.accountID,
                                configurationIdentifier: journal.configurationIdentifier
                            )
                            refreshCompletionPresentationAfterLocalChange()
                        } else {
                            status = .failed(
                                "Google import finished, but canonical recomposition still needs attention. Its completion remains saved for retry."
                            )
                        }
                    } else {
                        status = .refreshQueued(
                            requestedAt: requestedAt,
                            message: "Google import finished; canonical recomposition is pending"
                        )
                    }
                    return
                }
                switch run.state {
                case .backoff:
                    status = .refreshQueued(
                        requestedAt: requestedAt,
                        message: "Google import is waiting for its safe retry window"
                    )
                    return
                case .reauthorizationRequired:
                    retryableRefreshAccountIDs.insert(accountID)
                    status = .failed("Google authorization must be renewed before import can continue.")
                    return
                case .failed:
                    retryableRefreshAccountIDs.insert(accountID)
                    status = .failed("Google import needs attention before it can continue.")
                    return
                case .idle, .running:
                    break
                }
            } catch {
                guard operationIsCurrent(operation) else { return }
                if error is GoogleIntegrationJournalStoreError {
                    refreshCompletionPresentationAfterLocalChange()
                    if refreshCompletionRecoveryResetRequired {
                        status = .failed(
                            "The completed Google import recovery is unreadable and needs explicit attention."
                        )
                    } else if refreshCompletionRecoveryRequiresAttention {
                        status = .failed(
                            "Canonical recomposition finished, but its completion marker remains saved and may safely run again."
                        )
                    }
                    return
                }
                if isOfflineError(error) {
                    status = .offline("The import remains queued, but this Mac is offline.")
                    return
                }
                if attempt + 1 >= authorizationPollLimit {
                    status = .refreshQueued(
                        requestedAt: requestedAt,
                        message: "Google import is still queued; use Check status later"
                    )
                    return
                }
            }
        }
        guard operationIsCurrent(operation) else { return }
        status = .refreshQueued(
            requestedAt: requestedAt,
            message: "Google import is still queued; use Check status later"
        )
    }

    func setAccountPaused(_ account: GoogleAccount, paused: Bool) async {
        guard accountIsCurrent(account), account.status == (paused ? .active : .paused) else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        guard !hasPendingRefreshCompletion(for: account) || !paused else {
            status = .failed(
                "Finish the accepted import and canonical recomposition before changing this account's lifecycle."
            )
            return
        }
        guard let operation = beginMutationOperation(
            message: paused ? "Pausing Google import…" : "Resuming Google import…"
        ) else { return }
        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performAccountPauseMutation(operation, account: account, paused: paused)
        }
        activeTask = task
        await task.value
    }

    private func performAccountPauseMutation(
        _ operation: GoogleIntegrationOperation,
        account: GoogleAccount,
        paused: Bool
    ) async {
        defer { finishOperation(operation.id) }
        let expectedStatus: GoogleAccountStatus = paused ? .paused : .active
        do {
            let idempotencyKey = "mac-google-account-\(UUID().uuidString.lowercased())"
            do {
                let updated = if paused {
                    try await operation.transport.pauseGoogleAccount(
                        account.id,
                        expectedRevision: account.revision,
                        idempotencyKey: idempotencyKey
                    )
                } else {
                    try await operation.transport.resumeGoogleAccount(
                        account.id,
                        expectedRevision: account.revision,
                        idempotencyKey: idempotencyKey
                    )
                }
                guard updated.id == account.id,
                      updated.revision > account.revision,
                      updated.status == expectedStatus else {
                    throw GoogleIntegrationLocalError.invalidMutationResponse
                }
            } catch {
                guard (isAmbiguousMutationError(error) || isConflictError(error)),
                      operationIsCurrent(operation) else { throw error }
                let snapshot = try await operation.transport.googleAccounts()
                try requireCurrent(operation)
                guard let updated = snapshot.accounts.first(where: { $0.id == account.id }),
                      updated.revision > account.revision,
                      updated.status == expectedStatus else { throw error }
            }
            try await reloadSnapshotAfterMutation(operation)
        } catch {
            handleMutationError(error, operation: operation)
        }
    }

    func disconnectGoogleAccount(_ account: GoogleAccount) async {
        guard accountIsCurrent(account), account.status != .revoked else {
            status = .failed(GoogleIntegrationLocalError.invalidMutationResponse.localizedDescription)
            return
        }
        guard !hasPendingRefreshCompletion(for: account) else {
            status = .failed(
                "Finish the accepted import and canonical recomposition before disconnecting this account."
            )
            return
        }
        let journal: GoogleDisconnectRetryJournal
        let operation: GoogleIntegrationOperation
        var reservedOperationID: UUID?
        do {
            if let existing = try disconnectJournalStore.load(now: now()) {
                guard existing.accountID == account.id else {
                    status = .failed(
                        "Finish the saved disconnect recovery before disconnecting another Google account."
                    )
                    return
                }
                guard disconnectRetryLaneIsClear(),
                      let reserved = beginOperation(
                        message: "Retrying the exact Google disconnect request…"
                      ) else { return }
                guard existing.configurationIdentifier == reserved.configurationIdentifier else {
                    finishOperation(reserved.id)
                    status = .failed(
                        GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
                            .localizedDescription
                    )
                    return
                }
                operation = reserved
                reservedOperationID = reserved.id
                journal = existing
            } else {
                guard account.status != .disconnecting,
                      account.status != .revocationFailed else {
                    status = .failed(
                        "This account is already fenced for disconnection, but this Mac has no exact retry identity. Check authoritative status or recover it from the Mac that started the disconnect."
                    )
                    return
                }
                guard mutationRecoveryIsClear(),
                      let reserved = beginOperation(
                        message: "Disconnecting the Google account…"
                      ) else { return }
                operation = reserved
                reservedOperationID = reserved.id
                let createdAt = now()
                journal = try GoogleDisconnectRetryJournal(
                    accountID: account.id,
                    expectedRevision: account.revision,
                    idempotencyKey: "mac-google-disconnect-\(UUID().uuidString.lowercased())",
                    configurationIdentifier: operation.configurationIdentifier,
                    createdAt: createdAt
                )
                try disconnectJournalStore.save(journal, now: createdAt)
                disconnectRecoveryRequiresAttention = true
                disconnectRecoveryResetRequired = false
                pendingDisconnectAccountID = account.id
            }
        } catch {
            if let reservedOperationID {
                finishOperation(reservedOperationID)
            }
            disconnectRecoveryRequiresAttention = true
            disconnectRecoveryResetRequired = error is GoogleIntegrationJournalStoreError
            pendingDisconnectAccountID = nil
            status = .failed(safeErrorMessage(
                error,
                fallback: "The exact Google disconnect recovery could not be loaded or saved safely."
            ))
            return
        }
        let task = Task<Void, Never> { @MainActor [weak self] in
            await self?.performAccountDisconnect(operation, journal: journal)
        }
        activeTask = task
        await task.value
    }

    private func performAccountDisconnect(
        _ operation: GoogleIntegrationOperation,
        journal: GoogleDisconnectRetryJournal
    ) async {
        defer { finishOperation(operation.id) }
        var disconnectProvenComplete = false
        do {
            guard try disconnectJournalStore.load(now: now()) == journal else {
                throw GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
            }
            do {
                let updated = try await operation.transport.disconnectGoogleAccount(
                    journal.accountID,
                    expectedRevision: journal.expectedRevision,
                    idempotencyKey: journal.idempotencyKey
                )
                guard updated.id == journal.accountID,
                      updated.revision > journal.expectedRevision,
                      updated.status == .revoked else {
                    throw GoogleIntegrationLocalError.invalidMutationResponse
                }
            } catch {
                if case DayWeaveAPIError.trustedGoogleDisconnectNoEffect = error {
                    let snapshot = try await operation.transport.googleAccounts()
                    try requireCurrent(operation)
                    let authoritative = snapshot.accounts.first {
                        $0.id == journal.accountID
                    }
                    if let authoritative,
                       authoritative.status == .active
                        || authoritative.status == .paused
                        || authoritative.status == .reauthorizationRequired
                        || authoritative.status == .revocationFailed {
                        // The exact stale request provably changed nothing and
                        // no revocation is currently progressing. Retire it
                        // only after the authoritative read; this permits a
                        // fresh disconnect or revocation-failure retry.
                        try clearDisconnectRecovery()
                    } else if authoritative == nil || authoritative?.status == .revoked {
                        // Another device may have completed disconnection
                        // between the conflict and this read. Keep the durable
                        // marker through verified canonical recomposition.
                        disconnectProvenComplete = true
                        removeAccountPresentation(journal.accountID)
                    }
                    try await commitFetchedSnapshot(snapshot, operation: operation)
                    return
                }
                guard (isAmbiguousMutationError(error) || isConflictError(error)),
                      operationIsCurrent(operation) else { throw error }
                let snapshot = try await operation.transport.googleAccounts()
                try requireCurrent(operation)
                if let updated = snapshot.accounts.first(where: { $0.id == journal.accountID }),
                   updated.status != .revoked {
                    try await commitFetchedSnapshot(snapshot, operation: operation)
                    disconnectRecoveryRequiresAttention = true
                    disconnectRecoveryResetRequired = false
                    pendingDisconnectAccountID = updated.id
                    return
                }
                disconnectProvenComplete = true
                removeAccountPresentation(journal.accountID)
                try await commitFetchedSnapshot(snapshot, operation: operation)
                return
            }
            disconnectProvenComplete = true
            removeAccountPresentation(journal.accountID)
            try await reloadSnapshotAfterMutation(operation)
        } catch {
            if disconnectProvenComplete {
                handleReadError(error, operation: operation)
            } else {
                handleDisconnectError(error, operation: operation, journal: journal)
            }
        }
    }

    private func reloadSnapshotAfterMutation(
        _ operation: GoogleIntegrationOperation
    ) async throws {
        let snapshot = try await operation.transport.googleAccounts()
        try requireCurrent(operation)
        try await commitFetchedSnapshot(snapshot, operation: operation)
    }

    private func commitFetchedSnapshot(
        _ snapshot: GoogleAccountsSnapshot,
        operation: GoogleIntegrationOperation
    ) async throws {
        try requireCurrent(operation)
        var loadedCollections: [UUID: [GoogleSyncCollection]] = [:]
        var loadedStatuses: [UUID: GoogleSyncStatus] = [:]
        for active in snapshot.accounts where active.status == .active {
            loadedCollections[active.id] = try await operation.transport.googleCollections(
                accountID: active.id
            )
            try requireCurrent(operation)
            loadedStatuses[active.id] = try await operation.transport.googleSyncStatus(
                accountID: active.id
            )
            try requireCurrent(operation)
        }
        await commitTrustedSnapshot(
            snapshot,
            collections: loadedCollections,
            statuses: loadedStatuses,
            operation: operation
        )
    }

    private func beginOperation(message: String) -> GoogleIntegrationOperation? {
        guard isActive else {
            status = .privacyProtected
            return nil
        }
        guard !credentialTransitionInProgress else {
            status = .loading("Waiting for the DayWeave authentication change to finish…")
            return nil
        }
        guard operationID == nil else {
            status = .failed(GoogleIntegrationLocalError.operationInProgress.localizedDescription)
            return nil
        }
        do {
            let transport = try makeTransport()
            let id = UUID()
            operationID = id
            isBusy = true
            status = .loading(message)
            return GoogleIntegrationOperation(
                id: id,
                generation: generation,
                transport: transport,
                configurationIdentifier: transport.configurationIdentifier
            )
        } catch {
            handleConfigurationError(error)
            return nil
        }
    }

    private func beginMutationOperation(message: String) -> GoogleIntegrationOperation? {
        guard mutationRecoveryIsClear() else { return nil }
        return beginOperation(message: message)
    }

    private func mutationRecoveryIsClear() -> Bool {
        guard !credentialTransitionInProgress else {
            status = .failed(
                "Wait for the DayWeave authentication change to finish before changing Google."
            )
            return false
        }
        guard !mutationRecoveryRequired else {
            status = .failed(
                "Check Google status before another account or source change; the prior request outcome is not yet authoritative."
            )
            return false
        }
        guard !hasPendingAuthorizationRecovery else {
            status = .failed(
                "Finish or reset the pending Google connection recovery before changing Google."
            )
            return false
        }
        do {
            if let journal = try disconnectJournalStore.load(now: now()) {
                disconnectRecoveryRequiresAttention = true
                disconnectRecoveryResetRequired = false
                pendingDisconnectAccountID = journal.accountID
                status = .failed(
                    "Retry the exact saved disconnect request before another Google change."
                )
                return false
            }
        } catch {
            refreshDisconnectJournalPresentation()
            status = .failed(
                "The saved disconnect recovery is unreadable. Reset it explicitly before continuing."
            )
            return false
        }
        guard !refreshCompletionRecoveryResetRequired else {
            status = .failed(
                "The saved import completion recovery is unreadable. Reset it explicitly before continuing."
            )
            return false
        }
        return true
    }

    private func disconnectRetryLaneIsClear() -> Bool {
        guard !credentialTransitionInProgress,
              !mutationRecoveryRequired,
              !hasPendingAuthorizationRecovery,
              !disconnectRecoveryResetRequired,
              !refreshCompletionRecoveryResetRequired else {
            status = .failed(
                "Finish or reset the other pending Google recovery before retrying disconnect."
            )
            return false
        }
        return true
    }

    private func clearDisconnectRecovery() throws {
        try disconnectJournalStore.delete()
        disconnectRecoveryRequiresAttention = false
        disconnectRecoveryResetRequired = false
        disconnectCompletionRequiresComposition = false
        pendingDisconnectAccountID = nil
    }

    private func refreshCompletionPresentationAfterLocalChange() {
        do {
            let journals = try refreshCompletionJournalStore.load(now: now())
            refreshCompletionRecoveryRequiresAttention = !journals.isEmpty
            refreshCompletionRecoveryResetRequired = false
            let currentConfiguration = try? makeTransport().configurationIdentifier
            let current = journals.filter {
                $0.configurationIdentifier == currentConfiguration
            }
            pendingRefreshAccountIDs = Set(current.map(\.accountID))
            retryableRefreshAccountIDs = Set(
                current.filter { journal in
                    journal.serverRequestedAt == nil
                        || Self.refreshRunPermitsRetry(
                            syncStatusByAccount[journal.accountID]?.run,
                            journal: journal
                        )
                }.map(\.accountID)
            )
            pendingRefreshPresentationDate = current
                .map { $0.serverRequestedAt ?? $0.localRequestStartedAt }
                .min()
        } catch {
            refreshCompletionRecoveryRequiresAttention = true
            refreshCompletionRecoveryResetRequired = true
            pendingRefreshAccountIDs = []
            retryableRefreshAccountIDs = []
            pendingRefreshPresentationDate = nil
        }
    }

    private func finishOperation(_ id: UUID) {
        guard operationID == id else { return }
        operationID = nil
        isBusy = false
        activeTask = nil
    }

    private func cancelActiveOperation() {
        operationID = nil
        isBusy = false
        activeTask?.cancel()
        activeTask = nil
    }

    private func makeTransport() throws -> any GoogleIntegrationTransport {
        if let transportProvider {
            return try transportProvider()
        }
        guard let configurationStore,
              let authCoordinator,
              let value = configurationStore.loadBaseURL(),
              !value.isEmpty else {
            throw DayWeaveAPIError.credentialUnavailable
        }
        let baseURL = try DayWeaveAPIBaseURL(value)
        guard authCoordinator.hasUsableCredential(boundTo: baseURL) else {
            throw DayWeaveAPIError.credentialUnavailable
        }
        return DayWeaveAPIClient(
            baseURL: baseURL,
            session: session,
            authCoordinator: authCoordinator
        )
    }

    private func requireCurrent(_ operation: GoogleIntegrationOperation) throws {
        guard operationIsCurrent(operation) else { throw CancellationError() }
        let current = try makeTransport()
        guard current.configurationIdentifier == operation.configurationIdentifier else {
            throw CancellationError()
        }
    }

    private func operationIsCurrent(_ operation: GoogleIntegrationOperation) -> Bool {
        isActive
            && !Task.isCancelled
            && operationID == operation.id
            && generation == operation.generation
    }

    private func handleConfigurationError(_ error: Error) {
        clearPrivatePresentation()
        trustedConfigurationIdentifier = nil
        if case DayWeaveAPIError.credentialUnavailable = error {
            status = .configurationRequired(
                "Finish DayWeave API authentication before connecting Google."
            )
        } else {
            status = .configurationRequired(safeErrorMessage(
                error,
                fallback: "Finish durable DayWeave authentication before connecting Google."
            ))
        }
    }

    private func handleReadError(
        _ error: Error,
        operation: GoogleIntegrationOperation
    ) {
        guard operationIsCurrent(operation) else { return }
        if error is CancellationError { return }
        if isAuthenticationError(error) {
            clearPrivatePresentation()
            trustedConfigurationIdentifier = nil
            status = .configurationRequired("Authenticate this Mac again to load Google details.")
        } else if isOfflineError(error) {
            if trustedConfigurationIdentifier != operation.configurationIdentifier {
                clearPrivatePresentation()
            }
            status = .offline("The Mac is offline. Cached Google details remain read-only.")
        } else {
            status = .failed(safeErrorMessage(
                error,
                fallback: "Google details could not be loaded safely."
            ))
        }
    }

    private func handleMutationError(
        _ error: Error,
        operation: GoogleIntegrationOperation
    ) {
        guard operationIsCurrent(operation) else { return }
        if error is CancellationError { return }
        let outcomeIsAmbiguous = isAmbiguousMutationError(error)
        if outcomeIsAmbiguous {
            mutationRecoveryRequired = true
            mutationRecoveryConfigurationIdentifier = operation.configurationIdentifier
        }
        if isAuthenticationError(error) {
            clearPrivatePresentation()
            trustedConfigurationIdentifier = nil
            status = .configurationRequired("Authenticate this Mac again before changing Google.")
        } else if isOfflineError(error) {
            status = .offline("The Mac is offline. No Google operation will be retried automatically.")
        } else {
            status = .failed(safeErrorMessage(
                error,
                fallback: outcomeIsAmbiguous
                    ? "The Google request outcome is unknown. Check authoritative status before trying again."
                    : "The Google request could not be completed safely."
            ))
        }
    }

    private func handleDisconnectError(
        _ error: Error,
        operation: GoogleIntegrationOperation,
        journal: GoogleDisconnectRetryJournal
    ) {
        guard operationIsCurrent(operation) else { return }
        if error is CancellationError { return }
        disconnectRecoveryRequiresAttention = true
        disconnectRecoveryResetRequired = error is GoogleIntegrationJournalStoreError
        pendingDisconnectAccountID = disconnectRecoveryResetRequired
            ? nil
            : journal.accountID
        if isAuthenticationError(error) {
            clearPrivatePresentation()
            trustedConfigurationIdentifier = nil
            status = .configurationRequired(
                "Authenticate this Mac again, then retry the exact saved Google disconnect request."
            )
        } else if isOfflineError(error) {
            status = .offline(
                "The Mac is offline. The exact Google disconnect request remains saved for retry."
            )
        } else if disconnectRecoveryResetRequired {
            status = .failed(
                "The exact Google disconnect recovery is unreadable. Reset it explicitly before continuing."
            )
        } else {
            status = .failed(
                "The Google disconnect did not finish. Retry the exact saved request; DayWeave will not create a second request identity."
            )
        }
    }

    private func clearPrivatePresentation() {
        accounts = []
        collectionsByAccount = [:]
        syncStatusByAccount = [:]
        cleanupStatus = nil
        pendingAuthorization = nil
        pendingAuthorizationConfigurationIdentifier = nil
        canOpenAuthorization = false
        canRetryAuthorization = false
        canCheckAuthorization = false
    }

    private func refreshRecoveryPresentation() {
        refreshAuthorizationJournalPresentation()
        refreshDisconnectJournalPresentation()
        refreshCompletionPresentationAfterLocalChange()
        if authorizationRecoveryResetRequired
            || disconnectRecoveryResetRequired
            || refreshCompletionRecoveryResetRequired {
            status = .failed(
                "A saved Google recovery record is unreadable. Reset it explicitly before continuing."
            )
        } else if authorizationRecoveryRequiresAttention {
            // Authorization presentation was selected by its journal state.
        } else if disconnectRecoveryRequiresAttention {
            status = .failed(
                pendingDisconnectAccountID == nil
                    ? "A saved disconnect recovery belongs to another DayWeave API session. Restore that session before continuing."
                    : "Retry the exact saved Google disconnect request before another Google change."
            )
        } else if refreshCompletionRecoveryRequiresAttention,
                  let requestedAt = pendingRefreshPresentationDate {
            status = .refreshQueued(
                requestedAt: requestedAt,
                message: "Waiting for a queued Google import to finish"
            )
        }
    }

    private func refreshAuthorizationJournalPresentation() {
        do {
            guard let loadedJournal = try journalStore.load() else {
                authorizationRecoveryRequiresAttention = false
                authorizationRecoveryResetRequired = false
                return
            }
            guard loadedJournal.isValid(now: now()) else {
                try journalStore.delete()
                authorizationRecoveryRequiresAttention = false
                authorizationRecoveryResetRequired = false
                return
            }
            let journal = loadedJournal
            authorizationRecoveryRequiresAttention = true
            authorizationRecoveryResetRequired = false
            let current: any GoogleIntegrationTransport
            do {
                current = try makeTransport()
            } catch {
                canRetryAuthorization = false
                canCheckAuthorization = false
                status = .configurationRequired(
                    "Restore the DayWeave API session that owns the saved Google connection request."
                )
                return
            }
            guard journal.configurationIdentifier == current.configurationIdentifier else {
                status = .configurationRequired(
                    GoogleIntegrationLocalError.recoveryBelongsToAnotherConfiguration
                        .localizedDescription
                )
                return
            }
            canRetryAuthorization = journal.browserOpenedAt == nil
            canCheckAuthorization = journal.browserOpenedAt != nil
            status = .authorizationOutcomeUnknown(expiresAt: journal.expiresAt)
        } catch {
            authorizationRecoveryRequiresAttention = true
            authorizationRecoveryResetRequired = error is GoogleOAuthStartJournalStoreError
            canRetryAuthorization = false
            canCheckAuthorization = false
            status = .failed(
                "The saved Google connection recovery record is unreadable. Reset it explicitly before continuing."
            )
        }
    }

    private func refreshDisconnectJournalPresentation() {
        disconnectCompletionRequiresComposition = false
        do {
            guard let loadedJournal = try disconnectJournalStore.load(now: now()) else {
                disconnectRecoveryRequiresAttention = false
                disconnectRecoveryResetRequired = false
                pendingDisconnectAccountID = nil
                return
            }
            let journal = loadedJournal
            disconnectRecoveryRequiresAttention = true
            disconnectRecoveryResetRequired = false
            guard let current = try? makeTransport(),
                  current.configurationIdentifier == journal.configurationIdentifier else {
                pendingDisconnectAccountID = nil
                return
            }
            pendingDisconnectAccountID = journal.accountID
        } catch {
            disconnectRecoveryRequiresAttention = true
            disconnectRecoveryResetRequired = true
            pendingDisconnectAccountID = nil
        }
    }

    private func reconcileAuthorizationJournal(
        with fetchedAccounts: [GoogleAccount],
        operation: GoogleIntegrationOperation
    ) {
        do {
            guard let loadedJournal = try journalStore.load() else {
                authorizationRecoveryRequiresAttention = false
                authorizationRecoveryResetRequired = false
                canRetryAuthorization = false
                canCheckAuthorization = false
                return
            }
            guard loadedJournal.isValid(now: now()) else {
                try journalStore.delete()
                authorizationRecoveryRequiresAttention = false
                authorizationRecoveryResetRequired = false
                canRetryAuthorization = false
                canCheckAuthorization = false
                return
            }
            var journal = loadedJournal
            authorizationRecoveryRequiresAttention = true
            authorizationRecoveryResetRequired = false
            if journal.configurationIdentifier != operation.configurationIdentifier {
                let scopeIsVisible = if let accountID = journal.request.accountID {
                    fetchedAccounts.contains { $0.id == accountID }
                } else {
                    fetchedAccounts.contains {
                        journal.baselineAccountRevisions[$0.id] != nil
                    }
                }
                guard scopeIsVisible else {
                    canRetryAuthorization = false
                    canCheckAuthorization = false
                    return
                }
                journal = GoogleOAuthStartJournal(
                    request: journal.request,
                    idempotencyKey: journal.idempotencyKey,
                    configurationIdentifier: operation.configurationIdentifier,
                    baselineAccountRevisions: journal.baselineAccountRevisions,
                    createdAt: journal.createdAt,
                    expiresAt: journal.expiresAt,
                    browserOpenedAt: journal.browserOpenedAt
                )
                try journalStore.save(journal)
            }
            _ = authorizationJournalHasAccountChange(journal, accounts: fetchedAccounts)
            canRetryAuthorization = journal.browserOpenedAt == nil
            canCheckAuthorization = journal.browserOpenedAt != nil
        } catch {
            refreshAuthorizationJournalPresentation()
        }
    }

    private func reconcileDisconnectJournal(
        with fetchedAccounts: [GoogleAccount],
        operation: GoogleIntegrationOperation
    ) async {
        do {
            guard let loadedJournal = try disconnectJournalStore.load(now: now()) else {
                disconnectRecoveryRequiresAttention = false
                disconnectRecoveryResetRequired = false
                pendingDisconnectAccountID = nil
                return
            }
            var journal = loadedJournal
            disconnectRecoveryRequiresAttention = true
            disconnectRecoveryResetRequired = false
            if journal.configurationIdentifier != operation.configurationIdentifier {
                guard fetchedAccounts.contains(where: {
                    $0.id == journal.accountID && $0.status != .revoked
                }) else {
                    pendingDisconnectAccountID = nil
                    orphanedRecoveryRequiresConfirmation = true
                    return
                }
                journal = try journal.rebinding(
                    configurationIdentifier: operation.configurationIdentifier
                )
                try disconnectJournalStore.save(journal, now: now())
            }
            guard let account = fetchedAccounts.first(where: { $0.id == journal.accountID }),
                  account.status != .revoked else {
                disconnectCompletionRequiresComposition = true
                pendingDisconnectAccountID = journal.accountID
                guard let importCompletionHandler else { return }
                let completionVerified = await importCompletionHandler()
                guard operationIsCurrent(operation) else { return }
                guard completionVerified else { return }
                try clearDisconnectRecovery()
                return
            }
            pendingDisconnectAccountID = journal.accountID
        } catch {
            refreshDisconnectJournalPresentation()
        }
    }

    private func reconcileRefreshCompletionJournals(
        with fetchedAccounts: [GoogleAccount],
        statuses: [UUID: GoogleSyncStatus],
        operation: GoogleIntegrationOperation
    ) async {
        do {
            let journals = try refreshCompletionJournalStore.load(now: now())
            var retained: [GooglePendingRefreshCompletionJournal] = []
            for original in journals {
                guard operationIsCurrent(operation) else { return }
                var journal = original
                if journal.configurationIdentifier != operation.configurationIdentifier {
                    guard fetchedAccounts.contains(where: {
                        $0.id == journal.accountID && $0.status != .revoked
                    }) else {
                        retained.append(journal)
                        orphanedRecoveryRequiresConfirmation = true
                        continue
                    }
                    journal = try journal.rebinding(
                        configurationIdentifier: operation.configurationIdentifier
                    )
                    try refreshCompletionJournalStore.save(journal, now: now())
                }
                guard let account = fetchedAccounts.first(where: {
                    $0.id == journal.accountID && $0.status != .revoked
                }) else {
                    guard let importCompletionHandler else {
                        retained.append(journal)
                        continue
                    }
                    let completionVerified = await importCompletionHandler()
                    guard operationIsCurrent(operation) else { return }
                    guard completionVerified else {
                        retained.append(journal)
                        continue
                    }
                    try refreshCompletionJournalStore.delete(
                        accountID: journal.accountID,
                        configurationIdentifier: journal.configurationIdentifier
                    )
                    continue
                }
                guard journal.serverRequestedAt != nil,
                      let targetRefreshGeneration = journal.targetRefreshGeneration,
                      let run = statuses[account.id]?.run,
                      run.accountID == account.id,
                      run.state == .idle,
                      run.completedRefreshGeneration >= targetRefreshGeneration,
                      let importCompletionHandler else {
                    retained.append(journal)
                    continue
                }
                let completionVerified = await importCompletionHandler()
                guard operationIsCurrent(operation) else { return }
                guard completionVerified else {
                    retained.append(journal)
                    continue
                }
                try refreshCompletionJournalStore.delete(
                    accountID: journal.accountID,
                    configurationIdentifier: journal.configurationIdentifier
                )
                guard operationIsCurrent(operation) else { return }
            }
            guard operationIsCurrent(operation) else { return }
            refreshCompletionRecoveryRequiresAttention = !retained.isEmpty
            refreshCompletionRecoveryResetRequired = false
            let current = retained.filter {
                $0.configurationIdentifier == operation.configurationIdentifier
            }
            pendingRefreshAccountIDs = Set(current.map(\.accountID))
            retryableRefreshAccountIDs = Set(
                current.filter { journal in
                    journal.serverRequestedAt == nil
                        || Self.refreshRunPermitsRetry(
                            statuses[journal.accountID]?.run,
                            journal: journal
                        )
                }.map(\.accountID)
            )
            pendingRefreshPresentationDate = current
                .map { $0.serverRequestedAt ?? $0.localRequestStartedAt }
                .min()
        } catch {
            guard operationIsCurrent(operation) else { return }
            refreshCompletionPresentationAfterLocalChange()
        }
    }

    private func authorizationJournalHasAccountChange(
        _ journal: GoogleOAuthStartJournal,
        accounts fetchedAccounts: [GoogleAccount]
    ) -> Bool {
        if let accountID = journal.request.accountID {
            guard let account = fetchedAccounts.first(where: { $0.id == accountID }),
                  account.status == .active else { return false }
            return account.revision > (journal.baselineAccountRevisions[accountID] ?? 0)
        }
        if journal.request.connectNew {
            return fetchedAccounts.contains { account in
                account.status == .active
                    && journal.baselineAccountRevisions[account.id] == nil
            }
        }
        if journal.baselineAccountRevisions.isEmpty {
            return fetchedAccounts.contains { $0.status == .active }
        }
        guard let account = fetchedAccounts.first(where: \.isDefault) else { return false }
        return account.status == .active
            && account.revision > (journal.baselineAccountRevisions[account.id] ?? 0)
    }

    private static func refreshRunPermitsRetry(
        _ run: GoogleSyncRunStatus?,
        journal: GooglePendingRefreshCompletionJournal
    ) -> Bool {
        guard journal.serverRequestedAt != nil,
              let targetRefreshGeneration = journal.targetRefreshGeneration,
              let run,
              run.accountID == journal.accountID,
              run.refreshGeneration >= targetRefreshGeneration else { return false }
        return run.state == .failed || run.state == .reauthorizationRequired
    }

    private func validatedAuthorizationURL(
        _ authorization: GoogleOAuthAuthorization
    ) throws -> URL {
        let currentTime = now()
        guard authorization.authorizationURL.utf8.count <= 8 * 1_024,
              authorization.expiresAt > currentTime,
              authorization.expiresAt.timeIntervalSince(currentTime)
                <= GoogleOAuthStartJournal.maximumLifetime,
              let components = URLComponents(string: authorization.authorizationURL),
              components.scheme == "https",
              components.percentEncodedHost == "accounts.google.com",
              components.port == nil || components.port == 443,
              components.user == nil,
              components.password == nil,
              components.percentEncodedPath == "/o/oauth2/v2/auth",
              let query = components.percentEncodedQuery,
              !query.isEmpty,
              query.utf8.count <= 8 * 1_024,
              components.fragment == nil,
              let url = components.url else {
            throw GoogleIntegrationLocalError.invalidAuthorizationURL
        }
        return url
    }

    private func accountIsCurrent(_ account: GoogleAccount) -> Bool {
        accounts.contains { $0.id == account.id && $0.revision == account.revision }
    }

    private func sourceIsCurrent(_ collection: GoogleSyncCollection) -> Bool {
        collectionsByAccount[collection.accountID]?.contains {
            $0.id == collection.id && $0.revision == collection.revision
        } == true
    }

    private func replaceCollection(_ collection: GoogleSyncCollection) {
        var collections = collectionsByAccount[collection.accountID] ?? []
        guard let index = collections.firstIndex(where: { $0.id == collection.id }) else {
            return
        }
        collections[index] = collection
        collectionsByAccount[collection.accountID] = Self.sortCollections(collections)
    }

    private func removeAccountPresentation(_ accountID: UUID) {
        accounts.removeAll { $0.id == accountID }
        collectionsByAccount.removeValue(forKey: accountID)
        syncStatusByAccount.removeValue(forKey: accountID)
    }

    private static func accountSort(_ lhs: GoogleAccount, _ rhs: GoogleAccount) -> Bool {
        if lhs.isDefault != rhs.isDefault { return lhs.isDefault }
        let order = lhs.displayLabel.localizedCaseInsensitiveCompare(rhs.displayLabel)
        if order != .orderedSame { return order == .orderedAscending }
        return lhs.id.uuidString < rhs.id.uuidString
    }

    private static func sortCollections(
        _ collections: [GoogleSyncCollection]
    ) -> [GoogleSyncCollection] {
        collections.sorted { lhs, rhs in
            if lhs.kind != rhs.kind { return lhs.kind == .calendar }
            if lhs.providerPrimary != rhs.providerPrimary { return lhs.providerPrimary }
            let order = lhs.displayName.localizedCaseInsensitiveCompare(rhs.displayName)
            if order != .orderedSame { return order == .orderedAscending }
            return lhs.id.uuidString < rhs.id.uuidString
        }
    }

    private static func roleIsSupported(
        _ role: GoogleSyncRole,
        for kind: GoogleCollectionKind
    ) -> Bool {
        switch (kind, role) {
        case (.calendar, .readOnly), (.calendar, .blocking), (.calendar, .writable),
             (.taskList, .readOnly): true
        case (.taskList, .writable), (.taskList, .blocking): false
        }
    }

    private func safeErrorMessage(_ error: Error, fallback: String) -> String {
        if let local = error as? GoogleIntegrationLocalError {
            return local.localizedDescription
        }
        if let journal = error as? GoogleOAuthStartJournalStoreError {
            return journal.localizedDescription
        }
        if let journal = error as? GoogleIntegrationJournalStoreError {
            return journal.localizedDescription
        }
        guard let api = error as? DayWeaveAPIError else { return fallback }
        return switch api {
        case .credentialUnavailable:
            "Durable DayWeave authentication is required before using Google."
        case .durableAuthentication:
            "DayWeave authentication changed or needs attention. Authenticate again before using Google."
        case .requestEncodingFailed, .invalidEndpoint:
            "The Google request could not be prepared safely."
        case let .transport(code):
            code == .notConnectedToInternet
                ? "The Mac is offline."
                : "The DayWeave API could not be reached safely."
        case .nonHTTPResponse, .responseTooLarge, .responseDecodingFailed:
            "The DayWeave API returned an unsupported Google response."
        case let .server(statusCode, _, _, _):
            if statusCode == 401 || statusCode == 403 {
                "Authenticate this Mac again before using Google."
            } else if statusCode == 409 {
                "Google state changed or needs renewed authorization. Check status before retrying."
            } else if statusCode >= 500 {
                "Google or the DayWeave API is temporarily unavailable."
            } else {
                "The DayWeave API could not complete the Google request."
            }
        case .trustedSchedulePublicationStale,
             .trustedCurrentScheduleAbsent,
             .trustedProposalApplicationAbsent,
             .trustedProposalApplicationNoEffect,
             .trustedCanonicalMutationInProgress,
             .trustedCanonicalMutationNoEffect,
             .trustedGoogleDisconnectNoEffect:
            fallback
        }
    }

    private func isAuthenticationError(_ error: Error) -> Bool {
        switch error {
        case DayWeaveAPIError.credentialUnavailable,
             DayWeaveAPIError.durableAuthentication:
            true
        case let DayWeaveAPIError.server(statusCode, _, _, _):
            statusCode == 401 || statusCode == 403
        default:
            false
        }
    }

    private func isOfflineError(_ error: Error) -> Bool {
        if case let DayWeaveAPIError.transport(code) = error {
            return code == .notConnectedToInternet || code == .networkConnectionLost
        }
        return false
    }

    private func isConflictError(_ error: Error) -> Bool {
        if case let DayWeaveAPIError.server(statusCode, _, _, _) = error {
            return statusCode == 409
        }
        return false
    }

    private func isAmbiguousMutationError(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        if case GoogleIntegrationLocalError.invalidMutationResponse = error {
            return true
        }
        return switch error {
        case DayWeaveAPIError.transport:
            true
        case DayWeaveAPIError.nonHTTPResponse,
             DayWeaveAPIError.responseTooLarge,
             DayWeaveAPIError.responseDecodingFailed:
            true
        case let DayWeaveAPIError.server(statusCode, _, _, _):
            statusCode == 409 || statusCode >= 500
        case DayWeaveAPIError.durableAuthentication:
            true
        default:
            false
        }
    }
}

extension GoogleIntegrationStore: CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    nonisolated var description: String { "Google integration" }
    nonisolated var debugDescription: String { description }
    nonisolated var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .class)
    }
}

extension GoogleOAuthStartJournal: CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    var description: String { "Google authorization recovery journal" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

extension UserDefaultsGoogleOAuthStartJournalStore: CustomStringConvertible,
    CustomDebugStringConvertible, CustomReflectable
{
    nonisolated var description: String { "Google authorization recovery journal store" }
    nonisolated var debugDescription: String { description }
    nonisolated var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .class)
    }
}
