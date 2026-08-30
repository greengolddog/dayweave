import Foundation

enum GoogleAccountStatus: String, Codable, Equatable, Sendable {
    case active
    case paused
    case reauthorizationRequired = "reauthorization_required"
    case disconnecting
    case revocationFailed = "revocation_failed"
    case revoked
}

struct GoogleAccount: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let externalAccountID: String
    let displayLabel: String
    let status: GoogleAccountStatus
    let syncEnabled: Bool
    let isDefault: Bool
    let grantedScopes: [String]
    let tokenExpiresAt: Date?
    let revision: UInt64
    let createdAt: Date
    let updatedAt: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case id
        case externalAccountID = "external_account_id"
        case displayLabel = "display_label"
        case status
        case syncEnabled = "sync_enabled"
        case isDefault = "is_default"
        case grantedScopes = "granted_scopes"
        case tokenExpiresAt = "token_expires_at"
        case revision
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        externalAccountID = try container.decode(String.self, forKey: .externalAccountID)
        displayLabel = try container.decode(String.self, forKey: .displayLabel)
        status = try container.decode(GoogleAccountStatus.self, forKey: .status)
        syncEnabled = try container.decode(Bool.self, forKey: .syncEnabled)
        isDefault = try container.decode(Bool.self, forKey: .isDefault)
        grantedScopes = try container.decode([String].self, forKey: .grantedScopes)
        tokenExpiresAt = try container.decodeIfPresent(Date.self, forKey: .tokenExpiresAt)
        revision = try container.decode(UInt64.self, forKey: .revision)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        try validateGoogleAccount(self, codingPath: decoder.codingPath)
    }

    func encode(to encoder: any Encoder) throws {
        try validateGoogleAccount(self, codingPath: encoder.codingPath, encoding: true)
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(externalAccountID, forKey: .externalAccountID)
        try container.encode(displayLabel, forKey: .displayLabel)
        try container.encode(status, forKey: .status)
        try container.encode(syncEnabled, forKey: .syncEnabled)
        try container.encode(isDefault, forKey: .isDefault)
        try container.encode(grantedScopes, forKey: .grantedScopes)
        try encodeGoogleNullable(tokenExpiresAt, forKey: .tokenExpiresAt, into: &container)
        try container.encode(revision, forKey: .revision)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
    }
}

extension GoogleAccount: CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    var description: String { "Google account \(status.rawValue)" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

struct GoogleOAuthCleanupStatus: Codable, Equatable, Sendable {
    let held: UInt64
    let pending: UInt64
    let retrying: UInt64
    let exhausted: UInt64
    let volatileGuardians: UInt64
    let durabilityDegraded: Bool
    let revocationFenced: Bool
    let operatorRecoveryRequired: Bool
    let uncertainAuthorizations: UInt64
    let legacyRecoveryRequired: UInt64
    let nextAttemptAt: Date?
    let lastFailureAt: Date?

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case held
        case pending
        case retrying
        case exhausted
        case volatileGuardians = "volatile_guardians"
        case durabilityDegraded = "durability_degraded"
        case revocationFenced = "revocation_fenced"
        case operatorRecoveryRequired = "operator_recovery_required"
        case uncertainAuthorizations = "uncertain_authorizations"
        case legacyRecoveryRequired = "legacy_recovery_required"
        case nextAttemptAt = "next_attempt_at"
        case lastFailureAt = "last_failure_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        held = try container.decode(UInt64.self, forKey: .held)
        pending = try container.decode(UInt64.self, forKey: .pending)
        retrying = try container.decode(UInt64.self, forKey: .retrying)
        exhausted = try container.decode(UInt64.self, forKey: .exhausted)
        volatileGuardians = try container.decode(UInt64.self, forKey: .volatileGuardians)
        durabilityDegraded = try container.decode(Bool.self, forKey: .durabilityDegraded)
        revocationFenced = try container.decode(Bool.self, forKey: .revocationFenced)
        operatorRecoveryRequired = try container.decode(
            Bool.self,
            forKey: .operatorRecoveryRequired
        )
        uncertainAuthorizations = try container.decode(
            UInt64.self,
            forKey: .uncertainAuthorizations
        )
        legacyRecoveryRequired = try container.decode(
            UInt64.self,
            forKey: .legacyRecoveryRequired
        )
        nextAttemptAt = try container.decodeIfPresent(Date.self, forKey: .nextAttemptAt)
        lastFailureAt = try container.decodeIfPresent(Date.self, forKey: .lastFailureAt)
        try validateGoogleCounts(
            [
                held, pending, retrying, exhausted, volatileGuardians,
                uncertainAuthorizations, legacyRecoveryRequired,
            ],
            codingPath: decoder.codingPath,
            description: "Google OAuth cleanup count is outside the supported range"
        )
    }

    func encode(to encoder: any Encoder) throws {
        try validateGoogleCounts(
            [
                held, pending, retrying, exhausted, volatileGuardians,
                uncertainAuthorizations, legacyRecoveryRequired,
            ],
            codingPath: encoder.codingPath,
            description: "Google OAuth cleanup count is outside the supported range",
            encoding: true
        )
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(held, forKey: .held)
        try container.encode(pending, forKey: .pending)
        try container.encode(retrying, forKey: .retrying)
        try container.encode(exhausted, forKey: .exhausted)
        try container.encode(volatileGuardians, forKey: .volatileGuardians)
        try container.encode(durabilityDegraded, forKey: .durabilityDegraded)
        try container.encode(revocationFenced, forKey: .revocationFenced)
        try container.encode(operatorRecoveryRequired, forKey: .operatorRecoveryRequired)
        try container.encode(uncertainAuthorizations, forKey: .uncertainAuthorizations)
        try container.encode(legacyRecoveryRequired, forKey: .legacyRecoveryRequired)
        try encodeGoogleNullable(nextAttemptAt, forKey: .nextAttemptAt, into: &container)
        try encodeGoogleNullable(lastFailureAt, forKey: .lastFailureAt, into: &container)
    }
}

struct GoogleAccountsSnapshot: Codable, Equatable, Sendable {
    let accounts: [GoogleAccount]
    let cleanup: GoogleOAuthCleanupStatus

    private enum CodingKeys: String, CodingKey, CaseIterable { case accounts, cleanup }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        accounts = try container.decode([GoogleAccount].self, forKey: .accounts)
        cleanup = try container.decode(GoogleOAuthCleanupStatus.self, forKey: .cleanup)
        guard accounts.count <= 10_000,
              Set(accounts.map(\.id)).count == accounts.count,
              Set(accounts.map(\.externalAccountID)).count == accounts.count,
              accounts.lazy.filter(\.isDefault).count <= 1 else {
            throw googleDecodingError(
                codingPath: decoder.codingPath,
                "Google account identities are duplicated or exceed the supported count"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard accounts.count <= 10_000,
              Set(accounts.map(\.id)).count == accounts.count,
              Set(accounts.map(\.externalAccountID)).count == accounts.count,
              accounts.lazy.filter(\.isDefault).count <= 1 else {
            throw googleEncodingError(
                codingPath: encoder.codingPath,
                "Google account identities are duplicated or exceed the supported count"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(accounts, forKey: .accounts)
        try container.encode(cleanup, forKey: .cleanup)
    }
}

enum GoogleService: String, Codable, Equatable, Sendable {
    case calendarReadOnly = "calendar_read_only"
    case calendar
    case tasksReadOnly = "tasks_read_only"
    case tasks
}

/// The macOS read-only connection deliberately sends an explicit empty service
/// array. The server contract interprets that exact value as Calendar and Tasks
/// read-only, while keeping writable Google scopes outside this client surface.
struct GoogleOAuthStartRequest: Codable, Equatable, Sendable {
    let services: [GoogleService]
    let forceConsent: Bool
    let loginHint: String?
    let accountID: UUID?
    let connectNew: Bool
    let makeDefault: Bool

    init(
        services: [GoogleService] = [],
        forceConsent: Bool = false,
        loginHint: String? = nil,
        accountID: UUID? = nil,
        connectNew: Bool = false,
        makeDefault: Bool = false
    ) {
        self.services = services
        self.forceConsent = forceConsent
        self.loginHint = loginHint
        self.accountID = accountID
        self.connectNew = connectNew
        self.makeDefault = makeDefault
    }

    var isValid: Bool {
        services.isEmpty
            && (!connectNew || accountID == nil)
            && (loginHint.map { hint in
                !hint.isEmpty
                    && hint.utf8.count <= 320
                    && !hint.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
            } ?? true)
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case services
        case forceConsent = "force_consent"
        case loginHint = "login_hint"
        case accountID = "account_id"
        case connectNew = "connect_new"
        case makeDefault = "make_default"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        services = try container.decode([GoogleService].self, forKey: .services)
        forceConsent = try container.decode(Bool.self, forKey: .forceConsent)
        loginHint = try container.decodeIfPresent(String.self, forKey: .loginHint)
        accountID = try container.decodeIfPresent(UUID.self, forKey: .accountID)
        connectNew = try container.decode(Bool.self, forKey: .connectNew)
        makeDefault = try container.decode(Bool.self, forKey: .makeDefault)
        guard isValid else {
            throw googleDecodingError(
                codingPath: decoder.codingPath,
                "Google OAuth start request is invalid"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard isValid else {
            throw googleEncodingError(
                codingPath: encoder.codingPath,
                "Google OAuth start request is invalid"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(services, forKey: .services)
        try container.encode(forceConsent, forKey: .forceConsent)
        try encodeGoogleNullable(loginHint, forKey: .loginHint, into: &container)
        try encodeGoogleNullable(accountID, forKey: .accountID, into: &container)
        try container.encode(connectNew, forKey: .connectNew)
        try container.encode(makeDefault, forKey: .makeDefault)
    }
}

struct GoogleOAuthAuthorization: Decodable, Equatable, Sendable {
    let authorizationURL: String
    let expiresAt: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case authorizationURL = "authorization_url"
        case expiresAt = "expires_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let rawURL = try container.decode(String.self, forKey: .authorizationURL)
        guard Self.isValidAuthorizationURL(rawURL) else {
            throw googleDecodingError(
                codingPath: decoder.codingPath,
                "Google authorization URL is not an approved provider endpoint"
            )
        }
        let decodedExpiry = try container.decode(Date.self, forKey: .expiresAt)
        let remaining = decodedExpiry.timeIntervalSinceNow
        guard remaining > 0, remaining <= 30 * 60 else {
            throw googleDecodingError(
                codingPath: decoder.codingPath,
                "Google authorization expiry is outside the supported window"
            )
        }
        authorizationURL = rawURL
        expiresAt = decodedExpiry
    }

    private static func isValidAuthorizationURL(_ value: String) -> Bool {
        guard value.utf8.count <= 8 * 1_024,
              let components = URLComponents(string: value),
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
              components.url?.absoluteString == value else { return false }
        return true
    }
}

extension GoogleOAuthAuthorization: CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    var description: String {
        "Google authorization pending until \(expiresAt.ISO8601Format())."
    }

    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .struct)
    }
}

enum GoogleCollectionKind: String, Codable, Equatable, Sendable {
    case calendar
    case taskList = "task_list"
}

enum GoogleSyncRole: String, Codable, Equatable, Sendable {
    case readOnly = "read_only"
    case blocking
    case writable
}

enum GoogleEventDisposition: String, Codable, Equatable, Sendable {
    case ignore
    case visibleNonblocking = "visible_nonblocking"
    case blocking
}

struct GoogleCalendarPolicy: Codable, Equatable, Sendable {
    let confirmedBusy: GoogleEventDisposition
    let tentative: GoogleEventDisposition
    let free: GoogleEventDisposition
    let allDay: GoogleEventDisposition
    let publishAllDay: Bool
    let publishTentative: Bool
    let publishFree: Bool

    init(
        confirmedBusy: GoogleEventDisposition = .blocking,
        tentative: GoogleEventDisposition = .visibleNonblocking,
        free: GoogleEventDisposition = .visibleNonblocking,
        allDay: GoogleEventDisposition = .visibleNonblocking,
        publishAllDay: Bool = false,
        publishTentative: Bool = false,
        publishFree: Bool = false
    ) {
        self.confirmedBusy = confirmedBusy
        self.tentative = tentative
        self.free = free
        self.allDay = allDay
        self.publishAllDay = publishAllDay
        self.publishTentative = publishTentative
        self.publishFree = publishFree
    }

    var isReadOnlySafe: Bool { !publishAllDay && !publishTentative && !publishFree }

    var withoutPublication: GoogleCalendarPolicy {
        GoogleCalendarPolicy(
            confirmedBusy: confirmedBusy,
            tentative: tentative,
            free: free,
            allDay: allDay
        )
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case confirmedBusy = "confirmed_busy"
        case tentative
        case free
        case allDay = "all_day"
        case publishAllDay = "publish_all_day"
        case publishTentative = "publish_tentative"
        case publishFree = "publish_free"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        confirmedBusy = try container.decode(GoogleEventDisposition.self, forKey: .confirmedBusy)
        tentative = try container.decode(GoogleEventDisposition.self, forKey: .tentative)
        free = try container.decode(GoogleEventDisposition.self, forKey: .free)
        allDay = try container.decode(GoogleEventDisposition.self, forKey: .allDay)
        publishAllDay = try container.decode(Bool.self, forKey: .publishAllDay)
        publishTentative = try container.decode(Bool.self, forKey: .publishTentative)
        publishFree = try container.decode(Bool.self, forKey: .publishFree)
    }
}

enum GoogleCalendarProjectionState: String, Codable, Equatable, Sendable {
    case uninitialized
    case complete
    case failed
}

struct GoogleSyncCollection: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let accountID: UUID
    let kind: GoogleCollectionKind
    let remoteCollectionID: String
    let displayName: String
    let providerAccessRole: String?
    let providerPrimary: Bool
    let providerSelected: Bool
    let providerHidden: Bool
    let providerDeleted: Bool
    let selected: Bool
    let visible: Bool
    let syncRole: GoogleSyncRole
    let calendarPolicy: GoogleCalendarPolicy
    let revision: UInt64
    let discoveredAt: Date
    let configuredAt: Date?
    let lastImportAt: Date?
    let planningProjectionState: GoogleCalendarProjectionState
    let planningGeneration: UInt64
    let planningCollectionRevision: UInt64?
    let planningWindowStart: Date?
    let planningWindowEnd: Date?
    let planningWindowRefreshedAt: Date?
    let createdAt: Date
    let updatedAt: Date

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case id
        case accountID = "account_id"
        case kind
        case remoteCollectionID = "remote_collection_id"
        case displayName = "display_name"
        case providerAccessRole = "provider_access_role"
        case providerPrimary = "provider_primary"
        case providerSelected = "provider_selected"
        case providerHidden = "provider_hidden"
        case providerDeleted = "provider_deleted"
        case selected
        case visible
        case syncRole = "sync_role"
        case calendarPolicy = "calendar_policy"
        case revision
        case discoveredAt = "discovered_at"
        case configuredAt = "configured_at"
        case lastImportAt = "last_import_at"
        case planningProjectionState = "planning_projection_state"
        case planningGeneration = "planning_generation"
        case planningCollectionRevision = "planning_collection_revision"
        case planningWindowStart = "planning_window_start"
        case planningWindowEnd = "planning_window_end"
        case planningWindowRefreshedAt = "planning_window_refreshed_at"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        kind = try container.decode(GoogleCollectionKind.self, forKey: .kind)
        remoteCollectionID = try container.decode(String.self, forKey: .remoteCollectionID)
        displayName = try container.decode(String.self, forKey: .displayName)
        providerAccessRole = try container.decodeIfPresent(String.self, forKey: .providerAccessRole)
        providerPrimary = try container.decode(Bool.self, forKey: .providerPrimary)
        providerSelected = try container.decode(Bool.self, forKey: .providerSelected)
        providerHidden = try container.decode(Bool.self, forKey: .providerHidden)
        providerDeleted = try container.decode(Bool.self, forKey: .providerDeleted)
        selected = try container.decode(Bool.self, forKey: .selected)
        visible = try container.decode(Bool.self, forKey: .visible)
        syncRole = try container.decode(GoogleSyncRole.self, forKey: .syncRole)
        calendarPolicy = try container.decode(GoogleCalendarPolicy.self, forKey: .calendarPolicy)
        revision = try container.decode(UInt64.self, forKey: .revision)
        discoveredAt = try container.decode(Date.self, forKey: .discoveredAt)
        configuredAt = try container.decodeIfPresent(Date.self, forKey: .configuredAt)
        lastImportAt = try container.decodeIfPresent(Date.self, forKey: .lastImportAt)
        planningProjectionState = try container.decode(
            GoogleCalendarProjectionState.self,
            forKey: .planningProjectionState
        )
        planningGeneration = try container.decode(UInt64.self, forKey: .planningGeneration)
        planningCollectionRevision = try container.decodeIfPresent(
            UInt64.self,
            forKey: .planningCollectionRevision
        )
        planningWindowStart = try container.decodeIfPresent(Date.self, forKey: .planningWindowStart)
        planningWindowEnd = try container.decodeIfPresent(Date.self, forKey: .planningWindowEnd)
        planningWindowRefreshedAt = try container.decodeIfPresent(
            Date.self,
            forKey: .planningWindowRefreshedAt
        )
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        try validateGoogleCollection(self, codingPath: decoder.codingPath)
    }

    func encode(to encoder: any Encoder) throws {
        try validateGoogleCollection(self, codingPath: encoder.codingPath, encoding: true)
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(kind, forKey: .kind)
        try container.encode(remoteCollectionID, forKey: .remoteCollectionID)
        try container.encode(displayName, forKey: .displayName)
        try encodeGoogleNullable(providerAccessRole, forKey: .providerAccessRole, into: &container)
        try container.encode(providerPrimary, forKey: .providerPrimary)
        try container.encode(providerSelected, forKey: .providerSelected)
        try container.encode(providerHidden, forKey: .providerHidden)
        try container.encode(providerDeleted, forKey: .providerDeleted)
        try container.encode(selected, forKey: .selected)
        try container.encode(visible, forKey: .visible)
        try container.encode(syncRole, forKey: .syncRole)
        try container.encode(calendarPolicy, forKey: .calendarPolicy)
        try container.encode(revision, forKey: .revision)
        try container.encode(discoveredAt, forKey: .discoveredAt)
        try encodeGoogleNullable(configuredAt, forKey: .configuredAt, into: &container)
        try encodeGoogleNullable(lastImportAt, forKey: .lastImportAt, into: &container)
        try container.encode(planningProjectionState, forKey: .planningProjectionState)
        try container.encode(planningGeneration, forKey: .planningGeneration)
        try encodeGoogleNullable(
            planningCollectionRevision,
            forKey: .planningCollectionRevision,
            into: &container
        )
        try encodeGoogleNullable(planningWindowStart, forKey: .planningWindowStart, into: &container)
        try encodeGoogleNullable(planningWindowEnd, forKey: .planningWindowEnd, into: &container)
        try encodeGoogleNullable(
            planningWindowRefreshedAt,
            forKey: .planningWindowRefreshedAt,
            into: &container
        )
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
    }
}

enum GoogleSyncRunState: String, Codable, Equatable, Sendable {
    case idle
    case running
    case backoff
    case reauthorizationRequired = "reauthorization_required"
    case failed
}

struct GoogleSyncRunStatus: Codable, Equatable, Sendable {
    let accountID: UUID
    let state: GoogleSyncRunState
    let requestedAt: Date?
    let startedAt: Date?
    let completedAt: Date?
    let nextAttemptAt: Date
    let consecutiveFailures: UInt32
    let lastErrorCode: String?
    let lastErrorAt: Date?
    let importedCount: UInt64
    let updatedCount: UInt64
    let deletedCount: UInt64
    let conflictCount: UInt64
    let rejectedCount: UInt64
    let refreshGeneration: UInt64
    let claimedRefreshGeneration: UInt64
    let completedRefreshGeneration: UInt64
    let revision: UInt64

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case accountID = "account_id"
        case state
        case requestedAt = "requested_at"
        case startedAt = "started_at"
        case completedAt = "completed_at"
        case nextAttemptAt = "next_attempt_at"
        case consecutiveFailures = "consecutive_failures"
        case lastErrorCode = "last_error_code"
        case lastErrorAt = "last_error_at"
        case importedCount = "imported_count"
        case updatedCount = "updated_count"
        case deletedCount = "deleted_count"
        case conflictCount = "conflict_count"
        case rejectedCount = "rejected_count"
        case refreshGeneration = "refresh_generation"
        case claimedRefreshGeneration = "claimed_refresh_generation"
        case completedRefreshGeneration = "completed_refresh_generation"
        case revision
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        state = try container.decode(GoogleSyncRunState.self, forKey: .state)
        requestedAt = try container.decodeIfPresent(Date.self, forKey: .requestedAt)
        startedAt = try container.decodeIfPresent(Date.self, forKey: .startedAt)
        completedAt = try container.decodeIfPresent(Date.self, forKey: .completedAt)
        nextAttemptAt = try container.decode(Date.self, forKey: .nextAttemptAt)
        consecutiveFailures = try container.decode(UInt32.self, forKey: .consecutiveFailures)
        lastErrorCode = try container.decodeIfPresent(String.self, forKey: .lastErrorCode)
        lastErrorAt = try container.decodeIfPresent(Date.self, forKey: .lastErrorAt)
        importedCount = try container.decode(UInt64.self, forKey: .importedCount)
        updatedCount = try container.decode(UInt64.self, forKey: .updatedCount)
        deletedCount = try container.decode(UInt64.self, forKey: .deletedCount)
        conflictCount = try container.decode(UInt64.self, forKey: .conflictCount)
        rejectedCount = try container.decode(UInt64.self, forKey: .rejectedCount)
        refreshGeneration = try container.decode(UInt64.self, forKey: .refreshGeneration)
        claimedRefreshGeneration = try container.decode(
            UInt64.self,
            forKey: .claimedRefreshGeneration
        )
        completedRefreshGeneration = try container.decode(
            UInt64.self,
            forKey: .completedRefreshGeneration
        )
        revision = try container.decode(UInt64.self, forKey: .revision)
        try validateGoogleSyncRun(self, codingPath: decoder.codingPath)
    }

    func encode(to encoder: any Encoder) throws {
        try validateGoogleSyncRun(self, codingPath: encoder.codingPath, encoding: true)
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(state, forKey: .state)
        try encodeGoogleNullable(requestedAt, forKey: .requestedAt, into: &container)
        try encodeGoogleNullable(startedAt, forKey: .startedAt, into: &container)
        try encodeGoogleNullable(completedAt, forKey: .completedAt, into: &container)
        try container.encode(nextAttemptAt, forKey: .nextAttemptAt)
        try container.encode(consecutiveFailures, forKey: .consecutiveFailures)
        try encodeGoogleNullable(lastErrorCode, forKey: .lastErrorCode, into: &container)
        try encodeGoogleNullable(lastErrorAt, forKey: .lastErrorAt, into: &container)
        try container.encode(importedCount, forKey: .importedCount)
        try container.encode(updatedCount, forKey: .updatedCount)
        try container.encode(deletedCount, forKey: .deletedCount)
        try container.encode(conflictCount, forKey: .conflictCount)
        try container.encode(rejectedCount, forKey: .rejectedCount)
        try container.encode(refreshGeneration, forKey: .refreshGeneration)
        try container.encode(claimedRefreshGeneration, forKey: .claimedRefreshGeneration)
        try container.encode(completedRefreshGeneration, forKey: .completedRefreshGeneration)
        try container.encode(revision, forKey: .revision)
    }
}

struct GoogleSyncStatus: Codable, Equatable, Sendable {
    let run: GoogleSyncRunStatus?
    let importConflicts: UInt64
    let pendingOutbound: UInt64
    let conflictedOutbound: UInt64
    let failedOutbound: UInt64
    let lastOutboundErrorCode: String?
    let lastOutboundErrorAt: Date?
    let nextOutboundAttemptAt: Date?

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case run
        case importConflicts = "import_conflicts"
        case pendingOutbound = "pending_outbound"
        case conflictedOutbound = "conflicted_outbound"
        case failedOutbound = "failed_outbound"
        case lastOutboundErrorCode = "last_outbound_error_code"
        case lastOutboundErrorAt = "last_outbound_error_at"
        case nextOutboundAttemptAt = "next_outbound_attempt_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        run = try container.decodeIfPresent(GoogleSyncRunStatus.self, forKey: .run)
        importConflicts = try container.decode(UInt64.self, forKey: .importConflicts)
        pendingOutbound = try container.decode(UInt64.self, forKey: .pendingOutbound)
        conflictedOutbound = try container.decode(UInt64.self, forKey: .conflictedOutbound)
        failedOutbound = try container.decode(UInt64.self, forKey: .failedOutbound)
        lastOutboundErrorCode = try container.decodeIfPresent(
            String.self,
            forKey: .lastOutboundErrorCode
        )
        lastOutboundErrorAt = try container.decodeIfPresent(
            Date.self,
            forKey: .lastOutboundErrorAt
        )
        nextOutboundAttemptAt = try container.decodeIfPresent(
            Date.self,
            forKey: .nextOutboundAttemptAt
        )
        try validateGoogleCounts(
            [importConflicts, pendingOutbound, conflictedOutbound, failedOutbound],
            codingPath: decoder.codingPath,
            description: "Google sync count is outside the supported range"
        )
    }

    func encode(to encoder: any Encoder) throws {
        try validateGoogleCounts(
            [importConflicts, pendingOutbound, conflictedOutbound, failedOutbound],
            codingPath: encoder.codingPath,
            description: "Google sync count is outside the supported range",
            encoding: true
        )
        var container = encoder.container(keyedBy: CodingKeys.self)
        try encodeGoogleNullable(run, forKey: .run, into: &container)
        try container.encode(importConflicts, forKey: .importConflicts)
        try container.encode(pendingOutbound, forKey: .pendingOutbound)
        try container.encode(conflictedOutbound, forKey: .conflictedOutbound)
        try container.encode(failedOutbound, forKey: .failedOutbound)
        try encodeGoogleNullable(
            lastOutboundErrorCode,
            forKey: .lastOutboundErrorCode,
            into: &container
        )
        try encodeGoogleNullable(
            lastOutboundErrorAt,
            forKey: .lastOutboundErrorAt,
            into: &container
        )
        try encodeGoogleNullable(
            nextOutboundAttemptAt,
            forKey: .nextOutboundAttemptAt,
            into: &container
        )
    }
}

struct GoogleSyncRefreshAccepted: Codable, Equatable, Sendable {
    let accountID: UUID
    let requestID: UUID
    let refreshGeneration: UInt64
    let requestedAt: Date

    init(
        accountID: UUID,
        requestID: UUID,
        refreshGeneration: UInt64,
        requestedAt: Date
    ) {
        self.accountID = accountID
        self.requestID = requestID
        self.refreshGeneration = refreshGeneration
        self.requestedAt = requestedAt
    }

    private enum CodingKeys: String, CodingKey, CaseIterable {
        case accountID = "account_id"
        case requestID = "request_id"
        case refreshGeneration = "refresh_generation"
        case requestedAt = "requested_at"
    }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        accountID = try container.decode(UUID.self, forKey: .accountID)
        requestID = try container.decode(UUID.self, forKey: .requestID)
        refreshGeneration = try container.decode(UInt64.self, forKey: .refreshGeneration)
        requestedAt = try container.decode(Date.self, forKey: .requestedAt)
        guard accountID != .googleZero,
              requestID != .googleZero,
              refreshGeneration > 0,
              refreshGeneration <= UInt64(Int64.max),
              requestedAt.timeIntervalSinceReferenceDate.isFinite else {
            throw googleDecodingError(
                codingPath: decoder.codingPath,
                "Google sync refresh account identity is invalid"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        guard accountID != .googleZero,
              requestID != .googleZero,
              refreshGeneration > 0,
              refreshGeneration <= UInt64(Int64.max),
              requestedAt.timeIntervalSinceReferenceDate.isFinite else {
            throw googleEncodingError(
                codingPath: encoder.codingPath,
                "Google sync refresh account identity is invalid"
            )
        }
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(accountID, forKey: .accountID)
        try container.encode(requestID, forKey: .requestID)
        try container.encode(refreshGeneration, forKey: .refreshGeneration)
        try container.encode(requestedAt, forKey: .requestedAt)
    }
}

// Exact top-level server envelopes remain internal transport details.
struct GoogleCollectionsSnapshot: Decodable, Equatable, Sendable {
    let collections: [GoogleSyncCollection]

    private enum CodingKeys: String, CodingKey, CaseIterable { case collections }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        collections = try container.decode([GoogleSyncCollection].self, forKey: .collections)
        guard collections.count <= 10_000,
              Set(collections.map(\.id)).count == collections.count,
              Set(collections.map { "\($0.kind.rawValue)\u{0}\($0.remoteCollectionID)" }).count
                  == collections.count else {
            throw googleDecodingError(
                codingPath: decoder.codingPath,
                "Google collection identities are duplicated or exceed the supported count"
            )
        }
    }
}

struct GoogleCollectionSnapshot: Decodable, Equatable, Sendable {
    let collection: GoogleSyncCollection

    private enum CodingKeys: String, CodingKey, CaseIterable { case collection }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        collection = try decoder.container(keyedBy: CodingKeys.self)
            .decode(GoogleSyncCollection.self, forKey: .collection)
    }
}

struct GoogleSyncStatusSnapshot: Decodable, Equatable, Sendable {
    let sync: GoogleSyncStatus

    private enum CodingKeys: String, CodingKey, CaseIterable { case sync }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        sync = try decoder.container(keyedBy: CodingKeys.self)
            .decode(GoogleSyncStatus.self, forKey: .sync)
    }
}

struct GoogleSyncRefreshSnapshot: Decodable, Equatable, Sendable {
    let refresh: GoogleSyncRefreshAccepted

    private enum CodingKeys: String, CodingKey, CaseIterable { case refresh }

    init(from decoder: any Decoder) throws {
        try requireExactGoogleKeys(CodingKeys.self, from: decoder)
        refresh = try decoder.container(keyedBy: CodingKeys.self)
            .decode(GoogleSyncRefreshAccepted.self, forKey: .refresh)
    }
}

private struct GoogleDynamicCodingKey: CodingKey {
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

private extension UUID {
    static let googleZero = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
}

private func requireExactGoogleKeys<Key: CodingKey & CaseIterable>(
    _ keyType: Key.Type,
    from decoder: any Decoder
) throws {
    let container = try decoder.container(keyedBy: GoogleDynamicCodingKey.self)
    let actual = Set(container.allKeys.map(\.stringValue))
    let expected = Set(Key.allCases.map(\.stringValue))
    guard actual == expected else {
        throw googleDecodingError(
            codingPath: decoder.codingPath,
            "Google integration response has an unsupported field shape"
        )
    }
}

private func encodeGoogleNullable<Value: Encodable, Key: CodingKey>(
    _ value: Value?,
    forKey key: Key,
    into container: inout KeyedEncodingContainer<Key>
) throws {
    if let value {
        try container.encode(value, forKey: key)
    } else {
        try container.encodeNil(forKey: key)
    }
}

private func validateGoogleAccount(
    _ account: GoogleAccount,
    codingPath: [any CodingKey],
    encoding: Bool = false
) throws {
    let credentialProjectionIsValid = if account.status == .revoked {
        account.grantedScopes.isEmpty
            && account.tokenExpiresAt == nil
            && !account.isDefault
    } else {
        !account.grantedScopes.isEmpty
    }
    let valid = account.id != .googleZero
        && account.revision > 0
        && account.revision <= UInt64(Int64.max)
        && validGoogleText(account.externalAccountID, maximumUTF8Bytes: 2_048)
        && validGoogleText(account.displayLabel, maximumUTF8Bytes: 1_024)
        && Set(account.grantedScopes).count == account.grantedScopes.count
        && account.grantedScopes.allSatisfy {
            validGoogleText($0, maximumUTF8Bytes: 2_048)
        }
        && account.createdAt <= account.updatedAt
        && account.syncEnabled == (account.status == .active)
        && credentialProjectionIsValid
    guard valid else {
        if encoding {
            throw googleEncodingError(codingPath: codingPath, "Google account state is invalid")
        }
        throw googleDecodingError(codingPath: codingPath, "Google account state is invalid")
    }
}

private func validateGoogleCollection(
    _ collection: GoogleSyncCollection,
    codingPath: [any CodingKey],
    encoding: Bool = false
) throws {
    let roleIsValid = switch (collection.kind, collection.syncRole) {
    case (.calendar, .readOnly), (.calendar, .blocking), (.taskList, .readOnly),
         (.calendar, .writable), (.taskList, .writable):
        true
    case (.taskList, .blocking):
        false
    }
    let writableCalendarAccessIsValid = collection.kind != .calendar
        || collection.syncRole != .writable
        || collection.providerAccessRole == "owner"
        || collection.providerAccessRole == "writer"
    let projectionRevisionIsValid = collection.planningCollectionRevision.map {
        $0 > 0 && $0 <= UInt64(Int64.max)
    } ?? true
    let windowIsValid = switch (collection.planningWindowStart, collection.planningWindowEnd) {
    case (nil, nil): true
    case let (start?, end?): start < end
    default: false
    }
    let valid = collection.id != .googleZero
        && collection.accountID != .googleZero
        && collection.revision > 0
        && collection.revision <= UInt64(Int64.max)
        && collection.planningGeneration <= UInt64(Int64.max)
        && projectionRevisionIsValid
        && validGoogleText(collection.remoteCollectionID, maximumUTF8Bytes: 2_048)
        && validGoogleText(collection.displayName, maximumUTF8Bytes: 4_096)
        && collection.providerAccessRole.map {
            validGoogleText($0, maximumUTF8Bytes: 64)
        } ?? true
        && collection.createdAt <= collection.updatedAt
        && roleIsValid
        && writableCalendarAccessIsValid
        && windowIsValid
    guard valid else {
        if encoding {
            throw googleEncodingError(codingPath: codingPath, "Google collection state is invalid")
        }
        throw googleDecodingError(codingPath: codingPath, "Google collection state is invalid")
    }
}

private func validateGoogleSyncRun(
    _ run: GoogleSyncRunStatus,
    codingPath: [any CodingKey],
    encoding: Bool = false
) throws {
    let counts = [
        run.importedCount, run.updatedCount, run.deletedCount, run.conflictCount, run.rejectedCount,
    ]
    let valid = run.accountID != .googleZero
        && run.revision > 0
        && run.revision <= UInt64(Int64.max)
        && run.refreshGeneration <= UInt64(Int64.max)
        && run.claimedRefreshGeneration <= run.refreshGeneration
        && run.completedRefreshGeneration <= run.claimedRefreshGeneration
        && counts.allSatisfy { $0 <= UInt64(Int64.max) }
        && run.lastErrorCode.map { validGoogleText($0, maximumUTF8Bytes: 256) } ?? true
    guard valid else {
        if encoding {
            throw googleEncodingError(codingPath: codingPath, "Google sync run state is invalid")
        }
        throw googleDecodingError(codingPath: codingPath, "Google sync run state is invalid")
    }
}

private func validateGoogleCounts(
    _ counts: [UInt64],
    codingPath: [any CodingKey],
    description: String,
    encoding: Bool = false
) throws {
    guard counts.allSatisfy({ $0 <= UInt64(Int64.max) }) else {
        if encoding {
            throw googleEncodingError(codingPath: codingPath, description)
        }
        throw googleDecodingError(codingPath: codingPath, description)
    }
}

private func validGoogleText(_ value: String, maximumUTF8Bytes: Int) -> Bool {
    !value.isEmpty
        && value.utf8.count <= maximumUTF8Bytes
        && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
}

private func googleDecodingError(
    codingPath: [any CodingKey],
    _ description: String
) -> DecodingError {
    .dataCorrupted(.init(codingPath: codingPath, debugDescription: description))
}

private func googleEncodingError(
    codingPath: [any CodingKey],
    _ description: String
) -> EncodingError {
    .invalidValue(description, .init(codingPath: codingPath, debugDescription: description))
}
