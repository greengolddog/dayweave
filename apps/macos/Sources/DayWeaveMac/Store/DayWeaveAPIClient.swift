import CryptoKit
import Foundation

struct JSONNumber: Codable, Equatable, Sendable, ExpressibleByIntegerLiteral, ExpressibleByFloatLiteral {
    private enum Storage: Sendable {
        case signed(Int64, locallyExact: Bool)
        case unsigned(UInt64, locallyExact: Bool)
        case decimal(Decimal)
    }

    private let storage: Storage

    init(integerLiteral value: Int64) {
        storage = .signed(value, locallyExact: true)
    }

    init(floatLiteral value: Double) {
        storage = .decimal(Decimal(value))
    }

    init(_ value: UInt64) {
        storage = .unsigned(value, locallyExact: true)
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(Int64.self) {
            // Decoder APIs do not reveal whether the wire token was `1`,
            // `1.0`, or `1e0`. Preserve the exact numeric value for display,
            // but conservatively deny arbitrary-JSON replacement.
            storage = .signed(value, locallyExact: false)
        } else if let value = try? container.decode(UInt64.self) {
            storage = .unsigned(value, locallyExact: false)
        } else if let value = try? container.decode(Decimal.self) {
            // JSONDecoder does not expose a decimal's original token. Retain it
            // for display/cache purposes, but never claim it can be rewritten
            // byte-for-byte by a full-item replacement.
            storage = .decimal(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Unsupported JSON number"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch storage {
        case let .signed(value, _): try container.encode(value)
        case let .unsigned(value, _): try container.encode(value)
        case let .decimal(value): try container.encode(value)
        }
    }

    var exactUInt32: UInt32? {
        switch storage {
        case let .signed(value, _) where value >= 0 && value <= Int64(UInt32.max):
            UInt32(value)
        case let .unsigned(value, _) where value <= UInt64(UInt32.max):
            UInt32(value)
        default:
            nil
        }
    }

    var supportsLosslessRoundTrip: Bool {
        switch storage {
        case let .signed(_, locallyExact), let .unsigned(_, locallyExact): locallyExact
        case .decimal: false
        }
    }

    var displayDescription: String {
        switch storage {
        case let .signed(value, _): String(value)
        case let .unsigned(value, _): String(value)
        case let .decimal(value): NSDecimalNumber(decimal: value).stringValue
        }
    }

    static func == (left: Self, right: Self) -> Bool {
        switch (left.storage, right.storage) {
        case let (.signed(left, _), .signed(right, _)): left == right
        case let (.unsigned(left, _), .unsigned(right, _)): left == right
        case let (.signed(left, _), .unsigned(right, _)) where left >= 0:
            UInt64(left) == right
        case let (.unsigned(left, _), .signed(right, _)) where right >= 0:
            left == UInt64(right)
        case let (.decimal(left), .decimal(right)): left == right
        default: false
        }
    }
}

enum JSONValue: Codable, Equatable, Sendable {
    case object([String: JSONValue])
    case array([JSONValue])
    case string(String)
    case number(JSONNumber)
    case bool(Bool)
    case null

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(JSONNumber.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Unsupported JSON value"
            )
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case let .object(value): try container.encode(value)
        case let .array(value): try container.encode(value)
        case let .string(value): try container.encode(value)
        case let .number(value): try container.encode(value)
        case let .bool(value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }

    var supportsLosslessRoundTrip: Bool {
        switch self {
        case let .object(value): value.values.allSatisfy(\.supportsLosslessRoundTrip)
        case let .array(value): value.allSatisfy(\.supportsLosslessRoundTrip)
        case let .number(value): value.supportsLosslessRoundTrip
        case .string, .bool, .null: true
        }
    }

    var displayDescription: String {
        switch self {
        case let .object(value):
            let rendered = value.keys.sorted().map { key in
                let description = value[key]?.displayDescription ?? "null"
                return "\(key): \(description)"
            }
            return "{" + rendered.joined(separator: ", ") + "}"
        case let .array(value):
            return "[" + value.map(\.displayDescription).joined(separator: ", ") + "]"
        case let .string(value): return value
        case let .number(value): return value.displayDescription
        case let .bool(value): return value ? "true" : "false"
        case .null: return "null"
        }
    }
}

func makeDayWeaveEphemeralSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.urlCache = nil
    configuration.requestCachePolicy = .reloadIgnoringLocalAndRemoteCacheData
    configuration.httpCookieStorage = nil
    configuration.httpShouldSetCookies = false
    configuration.urlCredentialStorage = nil
    return URLSession(configuration: configuration)
}

enum DayWeaveProposalSource: Codable, Equatable, Hashable, Sendable {
    case appAssistant
    case chatGPT
    case codex
    case externalMCP
    case unknown(String)

    var wireValue: String {
        switch self {
        case .appAssistant: "app_assistant"
        case .chatGPT: "chat_gpt"
        case .codex: "codex"
        case .externalMCP: "external_mcp"
        case let .unknown(value): value
        }
    }

    var title: String {
        switch self {
        case .appAssistant: "DayWeave assistant"
        case .chatGPT: "ChatGPT"
        case .codex: "Codex"
        case .externalMCP: "External MCP"
        case let .unknown(value): value.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    init(from decoder: any Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        self = switch value {
        case "app_assistant": .appAssistant
        case "chat_gpt": .chatGPT
        case "codex": .codex
        case "external_mcp": .externalMCP
        default: .unknown(value)
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wireValue)
    }
}

enum DayWeaveProposalKind: Codable, Equatable, Hashable, Sendable {
    case createItem
    case updateItem
    case goalBreakdown
    case constraintChange
    case calendarEvent
    case schedulePlan
    case recommendation
    case unknown(String)

    var wireValue: String {
        switch self {
        case .createItem: "create_item"
        case .updateItem: "update_item"
        case .goalBreakdown: "goal_breakdown"
        case .constraintChange: "constraint_change"
        case .calendarEvent: "calendar_event"
        case .schedulePlan: "schedule_plan"
        case .recommendation: "recommendation"
        case let .unknown(value): value
        }
    }

    var title: String {
        wireValue.replacingOccurrences(of: "_", with: " ").capitalized
    }

    init(from decoder: any Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        self = switch value {
        case "create_item": .createItem
        case "update_item": .updateItem
        case "goal_breakdown": .goalBreakdown
        case "constraint_change": .constraintChange
        case "calendar_event": .calendarEvent
        case "schedule_plan": .schedulePlan
        case "recommendation": .recommendation
        default: .unknown(value)
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wireValue)
    }
}

enum DayWeaveProposalStatus: Codable, Equatable, Hashable, Sendable {
    case pending
    case accepted
    case rejected
    case expired
    case unknown(String)

    var wireValue: String {
        switch self {
        case .pending: "pending"
        case .accepted: "accepted"
        case .rejected: "rejected"
        case .expired: "expired"
        case let .unknown(value): value
        }
    }

    init(from decoder: any Decoder) throws {
        let value = try decoder.singleValueContainer().decode(String.self)
        self = switch value {
        case "pending": .pending
        case "accepted": .accepted
        case "rejected": .rejected
        case "expired": .expired
        default: .unknown(value)
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wireValue)
    }
}

struct DayWeaveProposal: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let revision: UInt64
    let submittedBy: String
    let source: DayWeaveProposalSource
    let sourceReference: String?
    let kind: DayWeaveProposalKind
    let status: DayWeaveProposalStatus
    let title: String
    let explanation: String?
    let payload: [String: JSONValue]
    let decisionNote: String?
    let createdAt: Date
    let updatedAt: Date
    let expiresAt: Date
    let decidedAt: Date?

    private enum CodingKeys: String, CodingKey {
        case id
        case revision
        case submittedBy = "submitted_by"
        case source
        case sourceReference = "source_reference"
        case kind
        case status
        case title
        case explanation
        case payload
        case decisionNote = "decision_note"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case expiresAt = "expires_at"
        case decidedAt = "decided_at"
    }
}

struct DayWeaveProposalEdit: Encodable, Equatable, Sendable {
    let expectedRevision: UInt64
    let title: String?
    let explanation: String?

    init(expectedRevision: UInt64, title: String? = nil, explanation: String? = nil) {
        self.expectedRevision = expectedRevision
        self.title = title
        self.explanation = explanation
    }

    private enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision"
        case title
        case explanation
    }
}

enum DayWeaveAPIBaseURLError: Error, Equatable, Sendable {
    case empty
    case notAbsoluteHTTPURL
    case credentialsNotAllowed
    case queryOrFragmentNotAllowed
    case insecureRemoteHTTP
}

extension DayWeaveAPIBaseURLError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .empty:
            "Enter the DayWeave API base URL."
        case .notAbsoluteHTTPURL:
            "The API base URL must be an absolute http or https URL with a host."
        case .credentialsNotAllowed:
            "Do not put credentials in the API URL. The bearer token is stored in Keychain."
        case .queryOrFragmentNotAllowed:
            "The API base URL cannot contain a query or fragment."
        case .insecureRemoteHTTP:
            "Plain HTTP is allowed only for localhost. Use HTTPS for a remote DayWeave API."
        }
    }
}

struct DayWeaveAPIBaseURL: Equatable, Sendable {
    let url: URL

    var credentialOriginIdentifier: String {
        canonicalComponents(includeBasePath: false)?.string ?? ""
    }

    var canonicalConfigurationIdentifier: String {
        canonicalComponents(includeBasePath: true)?.string ?? ""
    }

    init(_ value: String) throws {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { throw DayWeaveAPIBaseURLError.empty }
        guard let components = URLComponents(string: value),
              let scheme = components.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = components.host?.lowercased(),
              !host.isEmpty else {
            throw DayWeaveAPIBaseURLError.notAbsoluteHTTPURL
        }
        guard components.user == nil, components.password == nil else {
            throw DayWeaveAPIBaseURLError.credentialsNotAllowed
        }
        guard components.query == nil, components.fragment == nil else {
            throw DayWeaveAPIBaseURLError.queryOrFragmentNotAllowed
        }
        if scheme == "http", !Self.isLoopback(Self.unbracketExactlyOnce(host)) {
            throw DayWeaveAPIBaseURLError.insecureRemoteHTTP
        }

        guard let normalizedURL = components.url else {
            throw DayWeaveAPIBaseURLError.notAbsoluteHTTPURL
        }
        url = normalizedURL
    }

    func endpoint(pathComponents: [String], queryItems: [URLQueryItem] = []) throws -> URL {
        var endpoint = pathComponents.reduce(url) { partial, component in
            partial.appendingPathComponent(component)
        }
        if !queryItems.isEmpty {
            guard var components = URLComponents(url: endpoint, resolvingAgainstBaseURL: false) else {
                throw DayWeaveAPIBaseURLError.notAbsoluteHTTPURL
            }
            components.queryItems = queryItems
            guard let queriedURL = components.url else {
                throw DayWeaveAPIBaseURLError.notAbsoluteHTTPURL
            }
            endpoint = queriedURL
        }
        return endpoint
    }

    func hasSameOrigin(as other: Self) -> Bool {
        credentialOriginIdentifier == other.credentialOriginIdentifier
            && !credentialOriginIdentifier.isEmpty
    }

    private static func isLoopback(_ host: String) -> Bool {
        let address = host.split(separator: "%", maxSplits: 1, omittingEmptySubsequences: false)[0]
        return address == "localhost" || address == "127.0.0.1" || address == "::1"
    }

    private static func unbracketExactlyOnce(_ host: String) -> String {
        guard host.first == "[", host.last == "]", host.count >= 2 else { return host }
        return String(host.dropFirst().dropLast())
    }

    private static func effectivePort(for url: URL, scheme: String) -> Int? {
        if let port = url.port { return port }
        return defaultPort(for: scheme)
    }

    private static func defaultPort(for scheme: String) -> Int? {
        switch scheme {
        case "http": 80
        case "https": 443
        default: nil
        }
    }

    private func canonicalComponents(includeBasePath: Bool) -> URLComponents? {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let scheme = components.scheme?.lowercased(),
              let encodedHost = components.percentEncodedHost,
              !encodedHost.isEmpty else { return nil }
        components.scheme = scheme
        // Reassign only URLComponents' already validated encoded spelling.
        // Constructing `[\(url.host)]` can fatal for scoped IPv6 because the
        // decoded `%` is not legal in `percentEncodedHost`.
        components.percentEncodedHost = encodedHost.lowercased()
        if Self.effectivePort(for: url, scheme: scheme) == Self.defaultPort(for: scheme) {
            components.port = nil
        }
        components.user = nil
        components.password = nil
        components.query = nil
        components.fragment = nil
        if includeBasePath {
            var path = components.percentEncodedPath
            if path == "/" {
                path = ""
            } else if path.hasSuffix("/") {
                // Treat one conventional trailing separator as spelling, but
                // preserve additional empty path segments as real identity.
                path.removeLast()
            }
            components.percentEncodedPath = path
        } else {
            components.percentEncodedPath = ""
        }
        return components
    }
}

enum DayWeaveAPIError: Error, Equatable, Sendable {
    case credentialUnavailable
    case durableAuthentication(DurableAuthError)
    case requestEncodingFailed
    case invalidEndpoint
    case transport(URLError.Code)
    case nonHTTPResponse
    case responseTooLarge(limitBytes: Int)
    /// Emitted only for the exact, endpoint-bound stale-publication contract.
    /// Generic 409 errors never become a destructive local-state signal.
    case trustedSchedulePublicationStale
    /// Exact authenticated `not_found` evidence from the native current-
    /// schedule resource. Generic 404 responses never clear local state.
    case trustedCurrentScheduleAbsent
    /// Emitted only for the exact authenticated proposal-application endpoint,
    /// media type, cache policy, and typed `not_found` envelope. Generic 404s
    /// never become evidence that an ambiguous mutation had no effect.
    case trustedProposalApplicationAbsent
    /// Emitted only for an endpoint-specific typed conflict that the server
    /// guarantees was detected before any proposal mutation committed.
    case trustedProposalApplicationNoEffect(conflictCode: String)
    /// Exact authenticated item-mutation evidence that the matching
    /// idempotent request still owns the operation and must be retried.
    case trustedCanonicalMutationInProgress
    /// Exact authenticated item-mutation evidence that this request made no
    /// change. Callers may still reconcile independently observed semantics.
    case trustedCanonicalMutationNoEffect(conflictCode: String)
    /// Exact authenticated Google disconnect evidence that the optimistic
    /// revision check failed before this request could claim or revoke data.
    case trustedGoogleDisconnectNoEffect
    case server(statusCode: Int, code: String?, message: String?, requestID: String?)
    case responseDecodingFailed
}

extension DayWeaveAPIError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .credentialUnavailable:
            return "The API bearer token is unavailable. Save it again in Settings."
        case let .durableAuthentication(error):
            return error.localizedDescription
        case .requestEncodingFailed:
            return "DayWeave could not encode the API request."
        case .invalidEndpoint:
            return "The configured API URL could not form the requested endpoint."
        case let .transport(code):
            if code == .notConnectedToInternet {
                return "The Mac is offline. Local planning still works."
            } else if code == .timedOut {
                return "The DayWeave API request timed out."
            } else if code == .cancelled {
                return "The DayWeave API request was cancelled."
            } else {
                return "The DayWeave API could not be reached (network error \(code.rawValue))."
            }
        case .nonHTTPResponse:
            return "The DayWeave API returned an invalid response."
        case let .responseTooLarge(limitBytes):
            return "The DayWeave API response exceeded the safe \(limitBytes / 1_048_576) MiB limit."
        case .trustedSchedulePublicationStale:
            return "Canonical items changed before this schedule could be published."
        case .trustedCurrentScheduleAbsent:
            return "The authenticated server has no published schedule."
        case .trustedProposalApplicationAbsent:
            return "The exact proposal application resource is absent on the authenticated server."
        case .trustedProposalApplicationNoEffect:
            return "The authenticated server proved that the exact proposal request had no effect."
        case .trustedCanonicalMutationInProgress:
            return "The matching canonical item request is still in progress."
        case .trustedCanonicalMutationNoEffect:
            return "The authenticated server proved that the canonical item request had no effect."
        case .trustedGoogleDisconnectNoEffect:
            return "The authenticated server proved that the exact Google disconnect request had no effect."
        case let .server(statusCode, code, message, requestID):
            let safeCode = DayWeaveDiagnosticSanitizer.code(code, secrets: [])
            let safeMessage = DayWeaveDiagnosticSanitizer.text(
                message,
                secrets: [],
                maximumCharacters: 500
            )
            let safeRequestID = DayWeaveDiagnosticSanitizer.requestID(
                requestID,
                secrets: []
            )
            var result: String
            if statusCode == 401 {
                result = "The DayWeave API rejected the bearer token. Replace it in Settings."
            } else if statusCode == 409 {
                result = "This data changed on the server. Refresh before trying again."
            } else if let safeMessage {
                result = "DayWeave API error \(statusCode): \(safeMessage)"
            } else if let safeCode {
                result = "DayWeave API error \(statusCode) (\(safeCode))."
            } else {
                result = "The DayWeave API returned HTTP \(statusCode)."
            }
            if let safeRequestID {
                result += " Request ID: \(safeRequestID)."
            }
            return result
        case .responseDecodingFailed:
            return "The DayWeave API response did not match this app’s supported contract."
        }
    }
}

extension DayWeaveAPIError: CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    var description: String { errorDescription ?? "DayWeave API error" }
    var debugDescription: String { description }
    var customMirror: Mirror {
        Mirror(self, children: ["summary": description], displayStyle: .enum)
    }
}

enum DayWeaveDiagnosticSanitizer {
    static func text(
        _ value: String?,
        secrets: [String],
        maximumCharacters: Int
    ) -> String? {
        guard var value else { return nil }
        for secret in secrets.filter({ !$0.isEmpty }).sorted(by: { $0.count > $1.count }) {
            value = value.replacingOccurrences(of: secret, with: "[redacted]")
        }
        value = replacingPattern(
            #"(?i)\bBearer\s+[^\s,;]+"#,
            in: value,
            with: "Bearer [redacted]"
        )
        value = replacingPattern(
            #"\bdw_(?:en1|da1|dr1|mc1|ga1|gsa1)_[A-Za-z0-9_-]{20,}\b"#,
            in: value,
            with: "[redacted]"
        )
        guard !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
        else { return nil }
        value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        return String(value.prefix(maximumCharacters))
    }

    static func code(_ value: String?, secrets: [String]) -> String? {
        guard let value = text(value, secrets: secrets, maximumCharacters: 100) else { return nil }
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789_")
        guard value.unicodeScalars.allSatisfy(allowed.contains) else { return nil }
        return value
    }

    static func requestID(_ value: String?, secrets: [String]) -> String? {
        guard let value = text(value, secrets: secrets, maximumCharacters: 200) else { return nil }
        let allowed = CharacterSet(
            charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._:-"
        )
        guard value.unicodeScalars.allSatisfy(allowed.contains) else { return nil }
        return value
    }

    private static func replacingPattern(
        _ pattern: String,
        in value: String,
        with replacement: String
    ) -> String {
        guard let expression = try? NSRegularExpression(pattern: pattern) else { return value }
        let range = NSRange(value.startIndex..<value.endIndex, in: value)
        return expression.stringByReplacingMatches(
            in: value,
            range: range,
            withTemplate: replacement
        )
    }
}

protocol GoogleOutboundTransport: Sendable {
    var configurationIdentifier: String { get }

    func previewGoogleOutbound(
        accountID: UUID,
        request: GoogleOutboundPreviewRequest
    ) async throws -> GoogleOutboundPreview

    func approveGoogleOutbound(
        accountID: UUID,
        previewID: UUID,
        expectedPreviewHash: String
    ) async throws -> GoogleOutboundApproval

    func enqueueGoogleOutbound(
        accountID: UUID,
        request: GoogleOutboundEnqueueRequest
    ) async throws -> GoogleOutboundAccepted
}

protocol GoogleSchedulePublicationTransport: Sendable {
    var configurationIdentifier: String { get }

    func previewGoogleSchedulePublication(
        accountID: UUID,
        request: GoogleSchedulePublicationPreviewRequest
    ) async throws -> GoogleSchedulePublicationPreview

    func approveGoogleSchedulePublication(
        accountID: UUID,
        previewID: UUID,
        expectedPreviewHash: String
    ) async throws -> GoogleSchedulePublicationApproval

    func enqueueGoogleSchedulePublication(
        accountID: UUID,
        request: GoogleSchedulePublicationEnqueueRequest
    ) async throws -> GoogleSchedulePublicationAccepted

    func googleSchedulePublicationStatus(
        accountID: UUID,
        publicationID: UUID
    ) async throws -> GoogleSchedulePublicationStatus
}

protocol DayWeaveHabitTransport: Sendable {
    var configurationIdentifier: String { get }

    func habitOccurrences(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        cursor: String?,
        limit: Int
    ) async throws -> DayWeaveHabitOccurrencePage

    func putHabitOutcome(
        habitID: UUID,
        occurrenceID: UUID,
        command: DayWeaveHabitOutcomeCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitOccurrenceMutationResponse

    func startHabitPause(
        habitID: UUID,
        command: DayWeaveHabitPauseStartCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitPauseMutationResponse

    func resumeHabitPause(
        habitID: UUID,
        pauseID: UUID,
        command: DayWeaveHabitPauseResumeCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitPauseMutationResponse

    func habitDelta(cursor: String?, limit: Int) async throws -> DayWeaveHabitDeltaPage

    func habitAnalytics(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        bucket: DayWeaveHabitAnalyticsBucket
    ) async throws -> DayWeaveHabitAnalytics
}

struct DayWeaveAPIClient: Sendable {
    static let maximumResponseBytes = 16 * 1_048_576
    static let maximumRequestBytes = 16 * 1_048_576
    static let maximumExecutionHistoryLimit = 100
    static let maximumCanonicalItemListLimit = 200
    static let maximumExecutionStreamLifetime: Duration = .seconds(330)

    private static let defaultCanonicalItemListLimit = 100

    private struct SuggestionListEnvelope: Decodable {
        let suggestions: [DayWeaveProposal]
    }

    private struct SuggestionEnvelope: Decodable {
        let suggestion: DayWeaveProposal
    }

    private struct ItemEnvelope: Decodable {
        let item: DayWeaveCanonicalItem
    }

    private struct ItemListEnvelope: Decodable {
        let items: [DayWeaveCanonicalItem]
    }

    private struct ItemDeltaEnvelope: Decodable {
        let changes: [DayWeaveItemDeltaChange]
        let nextCursor: String
        let hasMore: Bool

        private enum CodingKeys: String, CodingKey {
            case changes
            case nextCursor = "next_cursor"
            case hasMore = "has_more"
        }
    }

    private struct ReplaceItemRequest: Encodable {
        let expectedRevision: UInt64
        let item: DayWeaveCanonicalItemFields

        private enum CodingKeys: String, CodingKey {
            case expectedRevision = "expected_revision"
            case item
        }
    }

    private struct RevisionRequest: Encodable {
        let expectedRevision: UInt64

        private enum CodingKeys: String, CodingKey {
            case expectedRevision = "expected_revision"
        }
    }

    private struct ConfigureGoogleCollectionRequest: Encodable {
        let expectedRevision: UInt64
        let selected: Bool
        let visible: Bool
        let syncRole: GoogleSyncRole
        let calendarPolicy: GoogleCalendarPolicy

        private enum CodingKeys: String, CodingKey {
            case expectedRevision = "expected_revision"
            case selected
            case visible
            case syncRole = "sync_role"
            case calendarPolicy = "calendar_policy"
        }
    }

    private struct DecisionRequest: Encodable {
        let expectedRevision: UInt64
        let note: String?

        private enum CodingKeys: String, CodingKey {
            case expectedRevision = "expected_revision"
            case note
        }
    }

    private struct GoogleSyncRefreshRequest: Encodable {
        let requestID: UUID

        private enum CodingKeys: String, CodingKey {
            case requestID = "request_id"
        }
    }

    private struct ErrorEnvelope: Decodable {
        let error: ErrorBody
    }

    private struct ErrorBody: Decodable {
        let code: String
        let message: String
    }

    private let baseURL: DayWeaveAPIBaseURL
    private let session: URLSession
    private let bearerToken: String?
    private let authCoordinator: DurableAuthCoordinator?
    private let expectedBindingIdentifier: String
    private let now: @Sendable () -> Date
    private let executionStreamLifetimeSleep: @Sendable () async throws -> Void

    let configurationIdentifier: String

    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = makeDayWeaveEphemeralSession(),
        bearerToken: String?,
        now: @escaping @Sendable () -> Date = Date.init,
        executionStreamLifetimeSleep: @escaping @Sendable () async throws -> Void = {
            try await Task.sleep(for: DayWeaveAPIClient.maximumExecutionStreamLifetime)
        }
    ) {
        self.baseURL = baseURL
        self.session = session
        self.bearerToken = bearerToken
        authCoordinator = nil
        self.now = now
        self.executionStreamLifetimeSleep = executionStreamLifetimeSleep
        let binding = Self.staticBindingIdentifier(token: bearerToken)
        expectedBindingIdentifier = binding
        configurationIdentifier = Self.configurationIdentifier(baseURL: baseURL, binding: binding)
    }

    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = makeDayWeaveEphemeralSession(),
        authCoordinator: DurableAuthCoordinator,
        now: @escaping @Sendable () -> Date = Date.init,
        executionStreamLifetimeSleep: @escaping @Sendable () async throws -> Void = {
            try await Task.sleep(for: DayWeaveAPIClient.maximumExecutionStreamLifetime)
        }
    ) {
        self.baseURL = baseURL
        self.session = session
        bearerToken = nil
        self.authCoordinator = authCoordinator
        self.now = now
        self.executionStreamLifetimeSleep = executionStreamLifetimeSleep
        let binding = (try? authCoordinator.bindingIdentifier(boundTo: baseURL))
            ?? "device-v1-unavailable:\(baseURL.canonicalConfigurationIdentifier)"
        expectedBindingIdentifier = binding
        configurationIdentifier = Self.configurationIdentifier(baseURL: baseURL, binding: binding)
    }

    /// Outbound authority must prove a durable device binding before its caller
    /// persists intent. Unlike the general client initializer, this never
    /// manufactures an unavailable fallback identifier.
    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = makeDayWeaveEphemeralSession(),
        durableAuthCoordinator authCoordinator: DurableAuthCoordinator,
        now: @escaping @Sendable () -> Date = Date.init,
        executionStreamLifetimeSleep: @escaping @Sendable () async throws -> Void = {
            try await Task.sleep(for: DayWeaveAPIClient.maximumExecutionStreamLifetime)
        }
    ) throws {
        self.baseURL = baseURL
        self.session = session
        bearerToken = nil
        self.authCoordinator = authCoordinator
        self.now = now
        self.executionStreamLifetimeSleep = executionStreamLifetimeSleep
        let binding = try authCoordinator.durableBindingIdentifier(boundTo: baseURL)
        expectedBindingIdentifier = binding
        configurationIdentifier = Self.configurationIdentifier(baseURL: baseURL, binding: binding)
    }

    func googleAccounts() async throws -> GoogleAccountsSnapshot {
        try await send(
            method: "GET",
            pathComponents: ["v1", "integrations", "google", "accounts"],
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
    }

    func startGoogleOAuth(
        _ request: GoogleOAuthStartRequest,
        idempotencyKey: String
    ) async throws -> GoogleOAuthAuthorization {
        guard request.isValid, Self.isValidGoogleIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return try await send(
            method: "POST",
            pathComponents: ["v1", "integrations", "google", "oauth", "start"],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(request),
            requiredStatusCode: 201,
            requiresDurableAuthorization: true
        )
    }

    func pauseGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount {
        try validateGoogleAccountMutationRequest(
            id: id,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey
        )
        let account: GoogleAccount = try await send(
            method: "POST",
            pathComponents: [
                "v1", "integrations", "google", "accounts", id.uuidString.lowercased(), "pause",
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(RevisionRequest(expectedRevision: expectedRevision)),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        return try validateGoogleAccountMutationResponse(
            account,
            id: id,
            expectedRevision: expectedRevision,
            expectedStatus: .paused,
            expectedSyncEnabled: false
        )
    }

    func resumeGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount {
        try validateGoogleAccountMutationRequest(
            id: id,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey
        )
        let account: GoogleAccount = try await send(
            method: "POST",
            pathComponents: [
                "v1", "integrations", "google", "accounts", id.uuidString.lowercased(), "resume",
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(RevisionRequest(expectedRevision: expectedRevision)),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        return try validateGoogleAccountMutationResponse(
            account,
            id: id,
            expectedRevision: expectedRevision,
            expectedStatus: .active,
            expectedSyncEnabled: true
        )
    }

    func disconnectGoogleAccount(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> GoogleAccount {
        try validateGoogleAccountMutationRequest(
            id: id,
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey
        )
        guard expectedRevision <= UInt64(Int64.max) - 2 else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let account: GoogleAccount = try await send(
            method: "DELETE",
            pathComponents: [
                "v1", "integrations", "google", "accounts", id.uuidString.lowercased(),
            ],
            queryItems: [
                URLQueryItem(name: "expected_revision", value: String(expectedRevision)),
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        return try validateGoogleAccountMutationResponse(
            account,
            id: id,
            expectedRevision: expectedRevision,
            expectedStatus: .revoked,
            expectedSyncEnabled: false,
            minimumRevisionIncrement: 2,
            requiresExactNextRevision: false
        )
    }

    func googleCollections(accountID: UUID) async throws -> [GoogleSyncCollection] {
        try validateGoogleIdentity(accountID)
        let snapshot: GoogleCollectionsSnapshot = try await send(
            method: "GET",
            pathComponents: googleAccountPath(accountID) + ["collections"],
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        return try validateGoogleCollections(snapshot.collections, accountID: accountID)
    }

    func discoverGoogleCollections(accountID: UUID) async throws -> [GoogleSyncCollection] {
        try validateGoogleIdentity(accountID)
        let snapshot: GoogleCollectionsSnapshot = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + ["collections", "discover"],
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        return try validateGoogleCollections(snapshot.collections, accountID: accountID)
    }

    func configureGoogleCollection(
        accountID: UUID,
        collectionID: UUID,
        expectedRevision: UInt64,
        selected: Bool,
        visible: Bool,
        role: GoogleSyncRole,
        calendarPolicy: GoogleCalendarPolicy
    ) async throws -> GoogleSyncCollection {
        try validateGoogleIdentity(accountID)
        try validateGoogleIdentity(collectionID)
        let publicationPolicyIsValid = switch role {
        case .readOnly, .blocking:
            calendarPolicy.isReadOnlySafe
        case .writable:
            true
        }
        guard expectedRevision > 0,
              expectedRevision < UInt64(Int64.max),
              publicationPolicyIsValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let snapshot: GoogleCollectionSnapshot = try await send(
            method: "PUT",
            pathComponents: googleAccountPath(accountID) + [
                "collections", collectionID.uuidString.lowercased(),
            ],
            body: try encode(ConfigureGoogleCollectionRequest(
                expectedRevision: expectedRevision,
                selected: selected,
                visible: visible,
                syncRole: role,
                calendarPolicy: calendarPolicy
            )),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        let collection = snapshot.collection
        let collectionRoleIsValid = switch (collection.kind, role) {
        case (.calendar, .readOnly), (.calendar, .blocking), (.calendar, .writable):
            true
        case (.taskList, .readOnly), (.taskList, .writable):
            calendarPolicy.isReadOnlySafe
        case (.taskList, .blocking):
            false
        }
        guard collection.accountID == accountID,
              collection.id == collectionID,
              collection.revision == expectedRevision + 1,
              collection.selected == selected,
              collection.visible == visible,
              collection.syncRole == role,
              collection.calendarPolicy == calendarPolicy,
              collectionRoleIsValid else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return collection
    }

    func googleSyncStatus(accountID: UUID) async throws -> GoogleSyncStatus {
        try validateGoogleIdentity(accountID)
        let snapshot: GoogleSyncStatusSnapshot = try await send(
            method: "GET",
            pathComponents: googleAccountPath(accountID) + ["sync"],
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        guard snapshot.sync.run.map({ $0.accountID == accountID }) ?? true else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return snapshot.sync
    }

    func requestGoogleSyncRefresh(
        accountID: UUID,
        requestID: UUID
    ) async throws -> GoogleSyncRefreshAccepted {
        try validateGoogleIdentity(accountID)
        try validateGoogleIdentity(requestID)
        let snapshot: GoogleSyncRefreshSnapshot = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + ["sync", "refresh"],
            body: try encode(GoogleSyncRefreshRequest(requestID: requestID)),
            requiredStatusCode: 202,
            requiresDurableAuthorization: true
        )
        guard snapshot.refresh.accountID == accountID,
              snapshot.refresh.requestID == requestID else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return snapshot.refresh
    }

    func previewGoogleOutbound(
        accountID: UUID,
        request: GoogleOutboundPreviewRequest
    ) async throws -> GoogleOutboundPreview {
        try validateGoogleIdentity(accountID)
        guard request.isValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let snapshot: GoogleOutboundPreviewSnapshot = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + ["outbound", "previews"],
            body: try encode(request),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        let preview = snapshot.preview
        let remaining = preview.expiresAt.timeIntervalSince(now())
        guard preview.accountID == accountID,
              preview.collectionID == request.collectionID,
              preview.itemID == request.itemID,
              preview.itemRevision == request.expectedItemRevision,
              preview.operation == request.operation,
              remaining >= -GoogleOutboundRecoveryJournal.maximumClockSkew,
              remaining <= GoogleOutboundRecoveryJournal.maximumIntentLifetime else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return preview
    }

    func approveGoogleOutbound(
        accountID: UUID,
        previewID: UUID,
        expectedPreviewHash: String
    ) async throws -> GoogleOutboundApproval {
        try validateGoogleIdentity(accountID)
        try validateGoogleIdentity(previewID)
        let request = GoogleOutboundApprovalRequest(
            expectedPreviewHash: expectedPreviewHash
        )
        guard request.isValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let snapshot: GoogleOutboundApprovalSnapshot = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + [
                "outbound", "previews", previewID.uuidString.lowercased(), "approve",
            ],
            body: try encode(request),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        let approval = snapshot.approval
        let remaining = approval.expiresAt.timeIntervalSince(now())
        guard approval.previewID == previewID,
              remaining >= -GoogleOutboundRecoveryJournal.maximumClockSkew,
              remaining <= GoogleOutboundRecoveryJournal.maximumIntentLifetime else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return approval
    }

    func enqueueGoogleOutbound(
        accountID: UUID,
        request: GoogleOutboundEnqueueRequest
    ) async throws -> GoogleOutboundAccepted {
        try validateGoogleIdentity(accountID)
        guard request.isValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let snapshot: GoogleOutboundAcceptedSnapshot = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + ["outbound"],
            body: try encode(request),
            requiredStatusCode: 202,
            requiresDurableAuthorization: true,
            additionalSecretsToRedact: [request.approvalCapability]
        )
        return snapshot.outbound
    }

    func previewGoogleSchedulePublication(
        accountID: UUID,
        request: GoogleSchedulePublicationPreviewRequest
    ) async throws -> GoogleSchedulePublicationPreview {
        try validateGoogleIdentity(accountID)
        guard request.isValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let preview: GoogleSchedulePublicationPreview = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + [
                "schedule-publications", "previews",
            ],
            body: try encode(request),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        let remaining = preview.expiresAt.timeIntervalSince(now())
        guard preview.accountID == accountID,
              preview.collectionID == request.collectionID,
              preview.scheduleRevisionID == request.expectedScheduleRevisionID,
              remaining >= -GoogleSchedulePublicationRecoveryJournal.maximumClockSkew,
              remaining <= GoogleSchedulePublicationRecoveryJournal.maximumIntentLifetime else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return preview
    }

    func approveGoogleSchedulePublication(
        accountID: UUID,
        previewID: UUID,
        expectedPreviewHash: String
    ) async throws -> GoogleSchedulePublicationApproval {
        try validateGoogleIdentity(accountID)
        try validateGoogleIdentity(previewID)
        let request = GoogleSchedulePublicationApprovalRequest(
            expectedPreviewHash: expectedPreviewHash
        )
        guard request.isValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let approval: GoogleSchedulePublicationApproval = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + [
                "schedule-publications", "previews",
                previewID.uuidString.lowercased(), "approve",
            ],
            body: try encode(request),
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        let remaining = approval.expiresAt.timeIntervalSince(now())
        guard approval.previewID == previewID,
              remaining >= -GoogleSchedulePublicationRecoveryJournal.maximumClockSkew,
              remaining <= GoogleSchedulePublicationRecoveryJournal.maximumIntentLifetime else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return approval
    }

    func enqueueGoogleSchedulePublication(
        accountID: UUID,
        request: GoogleSchedulePublicationEnqueueRequest
    ) async throws -> GoogleSchedulePublicationAccepted {
        try validateGoogleIdentity(accountID)
        guard request.isValid else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let accepted: GoogleSchedulePublicationAccepted = try await send(
            method: "POST",
            pathComponents: googleAccountPath(accountID) + ["schedule-publications"],
            body: try encode(request),
            requiredStatusCode: 202,
            requiresDurableAuthorization: true,
            additionalSecretsToRedact: [request.approvalCapability]
        )
        return accepted
    }

    func googleSchedulePublicationStatus(
        accountID: UUID,
        publicationID: UUID
    ) async throws -> GoogleSchedulePublicationStatus {
        try validateGoogleIdentity(accountID)
        try validateGoogleIdentity(publicationID)
        let status: GoogleSchedulePublicationStatus = try await send(
            method: "GET",
            pathComponents: googleAccountPath(accountID) + [
                "schedule-publications", publicationID.uuidString.lowercased(),
            ],
            requiredStatusCode: 200,
            requiresDurableAuthorization: true
        )
        guard status.accountID == accountID,
              status.publicationID == publicationID else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return status
    }

    private func googleAccountPath(_ accountID: UUID) -> [String] {
        [
            "v1", "integrations", "google", "accounts", accountID.uuidString.lowercased(),
        ]
    }

    private func validateGoogleIdentity(_ id: UUID) throws {
        guard id != Self.googleZeroUUID else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private func validateGoogleAccountMutationRequest(
        id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) throws {
        try validateGoogleIdentity(id)
        guard expectedRevision > 0,
              expectedRevision < UInt64(Int64.max),
              Self.isValidGoogleIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private func validateGoogleAccountMutationResponse(
        _ account: GoogleAccount,
        id: UUID,
        expectedRevision: UInt64,
        expectedStatus: GoogleAccountStatus,
        expectedSyncEnabled: Bool,
        minimumRevisionIncrement: UInt64 = 1,
        requiresExactNextRevision: Bool = true
    ) throws -> GoogleAccount {
        guard account.id == id,
              account.revision >= expectedRevision + minimumRevisionIncrement,
              !requiresExactNextRevision || account.revision == expectedRevision + 1,
              account.status == expectedStatus,
              account.syncEnabled == expectedSyncEnabled else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return account
    }

    private func validateGoogleCollections(
        _ collections: [GoogleSyncCollection],
        accountID: UUID
    ) throws -> [GoogleSyncCollection] {
        guard collections.allSatisfy({ $0.accountID == accountID }) else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return collections
    }

    private static func isValidGoogleIdempotencyKey(_ value: String) -> Bool {
        (8...128).contains(value.utf8.count)
            && value.utf8.allSatisfy { byte in
                (byte >= 65 && byte <= 90)
                    || (byte >= 97 && byte <= 122)
                    || (byte >= 48 && byte <= 57)
                    || [45, 46, 95].contains(byte)
            }
    }

    private static let googleZeroUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )

    func listSuggestions(
        status: DayWeaveProposalStatus? = .pending,
        limit: Int = 200
    ) async throws -> [DayWeaveProposal] {
        var queryItems = [URLQueryItem(name: "limit", value: String(limit))]
        if let status {
            queryItems.append(URLQueryItem(name: "status", value: status.wireValue))
        }
        let envelope: SuggestionListEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "suggestions"],
            queryItems: queryItems
        )
        return envelope.suggestions
    }

    func acceptSuggestion(
        id: UUID,
        expectedRevision: UInt64,
        note: String? = nil
    ) async throws -> DayWeaveProposal {
        let body = try encode(DecisionRequest(expectedRevision: expectedRevision, note: note))
        let envelope: SuggestionEnvelope = try await send(
            method: "POST",
            pathComponents: ["v1", "suggestions", id.uuidString.lowercased(), "accept"],
            body: body
        )
        return envelope.suggestion
    }

    func rejectSuggestion(
        id: UUID,
        expectedRevision: UInt64,
        note: String? = nil
    ) async throws -> DayWeaveProposal {
        let body = try encode(DecisionRequest(expectedRevision: expectedRevision, note: note))
        let envelope: SuggestionEnvelope = try await send(
            method: "POST",
            pathComponents: ["v1", "suggestions", id.uuidString.lowercased(), "reject"],
            body: body
        )
        return envelope.suggestion
    }

    func editSuggestion(id: UUID, edit: DayWeaveProposalEdit) async throws -> DayWeaveProposal {
        let body = try encode(edit)
        let envelope: SuggestionEnvelope = try await send(
            method: "PATCH",
            pathComponents: ["v1", "suggestions", id.uuidString.lowercased()],
            body: body
        )
        return envelope.suggestion
    }

    func previewSuggestionApplication(
        _ request: DayWeaveProposalPreviewRequest
    ) async throws -> DayWeaveProposalApplicationPreview {
        guard (1...20).contains(request.proposals.count),
              Set(request.proposals.map(\.proposalID)).count == request.proposals.count,
              request.proposals.allSatisfy({ $0.expectedRevision > 0 }) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return try await send(
            method: "POST",
            pathComponents: ["v1", "suggestions", "application-previews"],
            body: try encode(request),
            requiredStatusCode: 201
        )
    }

    func applySuggestionApplication(
        previewID: UUID,
        expectedReviewHash: String,
        idempotencyKey: String
    ) async throws -> DayWeaveProposalApplyResponse {
        let body = try prepareSuggestionApplicationApplyBody(
            expectedReviewHash: expectedReviewHash
        )
        return try await applySuggestionApplication(
            previewID: previewID,
            expectedReviewHash: expectedReviewHash,
            requestBody: body,
            idempotencyKey: idempotencyKey
        )
    }

    func prepareSuggestionApplicationApplyBody(
        expectedReviewHash: String
    ) throws -> Data {
        guard Self.isValidProposalReviewHash(expectedReviewHash) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return try encode(DayWeaveProposalApplyRequest(
            expectedReviewHash: expectedReviewHash
        ))
    }

    func applySuggestionApplication(
        previewID: UUID,
        expectedReviewHash: String,
        requestBody: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveProposalApplyResponse {
        guard Self.isValidProposalIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        try validateSuggestionApplicationApplyBody(
            requestBody,
            expectedReviewHash: expectedReviewHash
        )
        return try await send(
            method: "POST",
            pathComponents: [
                "v1", "suggestions", "application-previews",
                previewID.uuidString.lowercased(), "apply",
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            body: requestBody,
            requiredStatusCode: 200
        )
    }

    func suggestionApplication(
        applicationID: UUID
    ) async throws -> DayWeaveProposalApplicationReceipt {
        try await send(
            method: "GET",
            pathComponents: [
                "v1", "suggestions", "applications", applicationID.uuidString.lowercased(),
            ],
            requiredStatusCode: 200
        )
    }

    func suggestionApplication(
        forProposalID proposalID: UUID
    ) async throws -> DayWeaveProposalApplicationReceipt {
        try await send(
            method: "GET",
            pathComponents: [
                "v1", "suggestions", proposalID.uuidString.lowercased(), "application",
            ],
            requiredStatusCode: 200
        )
    }

    func undoSuggestionApplication(
        applicationID: UUID,
        expectedApplicationRevision: UInt64,
        idempotencyKey: String
    ) async throws -> DayWeaveProposalUndoResponse {
        let body = try prepareSuggestionApplicationUndoBody(
            expectedApplicationRevision: expectedApplicationRevision
        )
        return try await undoSuggestionApplication(
            applicationID: applicationID,
            expectedApplicationRevision: expectedApplicationRevision,
            requestBody: body,
            idempotencyKey: idempotencyKey
        )
    }

    func prepareSuggestionApplicationUndoBody(
        expectedApplicationRevision: UInt64
    ) throws -> Data {
        guard expectedApplicationRevision > 0 else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return try encode(DayWeaveProposalUndoRequest(
            expectedApplicationRevision: expectedApplicationRevision
        ))
    }

    func undoSuggestionApplication(
        applicationID: UUID,
        expectedApplicationRevision: UInt64,
        requestBody: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveProposalUndoResponse {
        guard Self.isValidProposalIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        try validateSuggestionApplicationUndoBody(
            requestBody,
            expectedApplicationRevision: expectedApplicationRevision
        )
        return try await send(
            method: "POST",
            pathComponents: [
                "v1", "suggestions", "applications",
                applicationID.uuidString.lowercased(), "undo",
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            body: requestBody,
            requiredStatusCode: 200
        )
    }

    private func validateSuggestionApplicationApplyBody(
        _ body: Data,
        expectedReviewHash: String
    ) throws {
        let expected = DayWeaveProposalApplyRequest(expectedReviewHash: expectedReviewHash)
        guard body.count <= Self.maximumRequestBytes,
              (try? makeDecoder().decode(DayWeaveProposalApplyRequest.self, from: body)) == expected,
              (try? encode(expected)) == body else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private func validateSuggestionApplicationUndoBody(
        _ body: Data,
        expectedApplicationRevision: UInt64
    ) throws {
        let expected = DayWeaveProposalUndoRequest(
            expectedApplicationRevision: expectedApplicationRevision
        )
        guard body.count <= Self.maximumRequestBytes,
              (try? makeDecoder().decode(DayWeaveProposalUndoRequest.self, from: body)) == expected,
              (try? encode(expected)) == body else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private static func isValidProposalReviewHash(_ value: String) -> Bool {
        value.hasPrefix("sha256:")
            && value.utf8.count == 71
            && value.utf8.dropFirst(7).allSatisfy { byte in
                (byte >= 48 && byte <= 57)
                    || (byte >= 65 && byte <= 70)
                    || (byte >= 97 && byte <= 102)
            }
    }

    private static func isValidProposalIdempotencyKey(_ value: String) -> Bool {
        (8...128).contains(value.utf8.count)
            && value.utf8.allSatisfy { byte in
                (byte >= 65 && byte <= 90)
                    || (byte >= 97 && byte <= 122)
                    || (byte >= 48 && byte <= 57)
                    || [45, 46, 95, 126].contains(byte)
            }
    }

    func executionSnapshot() async throws -> DayWeaveExecutionSnapshot {
        let envelope: DayWeaveExecutionSnapshotEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "execution"]
        )
        return envelope.execution
    }

    /// Opens the privacy-safe foreground invalidation stream. Revisions from
    /// this method are hints only; callers must fetch and validate the ordinary
    /// execution snapshot before persisting any state.
    func consumeExecutionInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamCompletion {
        try await withThrowingTaskGroup(
            of: DayWeaveExecutionStreamCompletion.self
        ) { group in
            group.addTask {
                try await consumeExecutionInvalidationsWithinLifetime(
                    after: revision,
                    receive
                )
            }
            group.addTask {
                try await executionStreamLifetimeSleep()
                try Task.checkCancellation()
                throw DayWeaveAPIError.transport(.timedOut)
            }
            do {
                guard let result = try await group.next() else {
                    throw DayWeaveAPIError.transport(.unknown)
                }
                group.cancelAll()
                return result
            } catch {
                group.cancelAll()
                throw error
            }
        }
    }

    /// Opens the content-free published-schedule invalidation stream. Every
    /// revision remains an untrusted wake-up hint until the ordinary current
    /// resource has been fetched and validated.
    func consumeScheduleInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveScheduleStreamCompletion {
        try await withThrowingTaskGroup(
            of: DayWeaveScheduleStreamCompletion.self
        ) { group in
            group.addTask {
                try await consumeScheduleInvalidationsWithinLifetime(
                    after: revision,
                    receive
                )
            }
            group.addTask {
                try await executionStreamLifetimeSleep()
                try Task.checkCancellation()
                throw DayWeaveAPIError.transport(.timedOut)
            }
            do {
                guard let result = try await group.next() else {
                    throw DayWeaveAPIError.transport(.unknown)
                }
                group.cancelAll()
                return result
            } catch {
                group.cancelAll()
                throw error
            }
        }
    }

    private func consumeScheduleInvalidationsWithinLifetime(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveScheduleStreamCompletion {
        guard revision <= UInt64(Int64.max) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: ["v1", "schedule", "stream"])
        } catch {
            throw DayWeaveAPIError.invalidEndpoint
        }

        var pristineRequest = URLRequest(url: endpoint)
        pristineRequest.httpMethod = "GET"
        pristineRequest.timeoutInterval = 330
        pristineRequest.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        pristineRequest.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        pristineRequest.setValue(String(revision), forHTTPHeaderField: "Last-Event-ID")
        pristineRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        pristineRequest.setValue("no-cache", forHTTPHeaderField: "Pragma")
        pristineRequest.setValue("identity", forHTTPHeaderField: "Accept-Encoding")

        let initialAuthorization: DurableAuthorization
        if let authCoordinator {
            do {
                initialAuthorization = try await authCoordinator.authorization(boundTo: baseURL)
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
        } else {
            guard let bearerToken, !bearerToken.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            initialAuthorization = .init(
                bearerToken: bearerToken,
                bindingIdentifier: expectedBindingIdentifier,
                isDurable: false
            )
        }
        guard initialAuthorization.bindingIdentifier == expectedBindingIdentifier else {
            throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
        }

        var authorization = initialAuthorization
        var tokensToRedact = [authorization.bearerToken]
        for attemptIndex in 0...1 {
            let result = try await performExecutionInvalidationStreamRequest(
                pristineRequest,
                bearer: authorization.bearerToken,
                bindingIdentifier: initialAuthorization.bindingIdentifier,
                initialRevision: revision,
                expectedEventName: "schedule-invalidation",
                requiresScheduleHeaders: true,
                receive
            )
            switch result {
            case let .endOfStream(wasLive):
                return wasLive ? .liveEndOfStream : .endOfStream
            case let .http(response, body):
                let normalizedHeaders = Self.normalizedHeaders(response)
                if attemptIndex == 0,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    let recovered: DurableAuthorization
                    do {
                        recovered = try await authCoordinator.recoverFromUnauthorized(
                            rejectedBearer: authorization.bearerToken,
                            boundTo: baseURL
                        )
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                    guard recovered.bindingIdentifier == initialAuthorization.bindingIdentifier,
                          recovered.bindingIdentifier == expectedBindingIdentifier else {
                        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
                    }
                    authorization = recovered
                    tokensToRedact.append(recovered.bearerToken)
                    continue
                }
                if attemptIndex == 1,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    do {
                        try await authCoordinator.retireDefinitivelyRejectedAuthorization(
                            authorization,
                            boundTo: baseURL
                        )
                        throw DayWeaveAPIError.durableAuthentication(.reauthenticationRequired)
                    } catch let error as DayWeaveAPIError {
                        throw error
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                }
                try validatePostResponseBinding(initialAuthorization.bindingIdentifier)
                if let head = Self.trustedScheduleCursorAhead(
                    requestedRevision: revision,
                    statusCode: response.statusCode,
                    contentType: response.value(forHTTPHeaderField: "content-type"),
                    cacheControl: response.value(forHTTPHeaderField: "cache-control"),
                    pragma: response.value(forHTTPHeaderField: "pragma"),
                    body: body
                ) {
                    return .cursorAhead(headRevision: head)
                }
                let envelope = try? makeDecoder().decode(ErrorEnvelope.self, from: body)
                throw DayWeaveAPIError.server(
                    statusCode: response.statusCode,
                    code: DayWeaveDiagnosticSanitizer.code(
                        envelope?.error.code,
                        secrets: tokensToRedact
                    ),
                    message: DayWeaveDiagnosticSanitizer.text(
                        envelope?.error.message,
                        secrets: tokensToRedact,
                        maximumCharacters: 500
                    ),
                    requestID: DayWeaveDiagnosticSanitizer.requestID(
                        response.value(forHTTPHeaderField: "x-request-id"),
                        secrets: tokensToRedact
                    )
                )
            }
        }
        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
    }

    private func consumeExecutionInvalidationsWithinLifetime(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamCompletion {
        guard revision <= UInt64(Int64.max) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: ["v1", "execution", "stream"])
        } catch {
            throw DayWeaveAPIError.invalidEndpoint
        }

        var pristineRequest = URLRequest(url: endpoint)
        pristineRequest.httpMethod = "GET"
        // The server deliberately ends healthy connections at five minutes.
        // Leave a small transport margin without permitting an unbounded read.
        pristineRequest.timeoutInterval = 330
        pristineRequest.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        pristineRequest.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        pristineRequest.setValue(String(revision), forHTTPHeaderField: "Last-Event-ID")
        pristineRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        pristineRequest.setValue("no-cache", forHTTPHeaderField: "Pragma")
        pristineRequest.setValue("identity", forHTTPHeaderField: "Accept-Encoding")

        let initialAuthorization: DurableAuthorization
        if let authCoordinator {
            do {
                initialAuthorization = try await authCoordinator.authorization(boundTo: baseURL)
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
        } else {
            guard let bearerToken, !bearerToken.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            initialAuthorization = .init(
                bearerToken: bearerToken,
                bindingIdentifier: expectedBindingIdentifier,
                isDurable: false
            )
        }
        guard initialAuthorization.bindingIdentifier == expectedBindingIdentifier else {
            throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
        }

        var authorization = initialAuthorization
        var tokensToRedact = [authorization.bearerToken]
        for attemptIndex in 0...1 {
            let result = try await performExecutionInvalidationStreamRequest(
                pristineRequest,
                bearer: authorization.bearerToken,
                bindingIdentifier: initialAuthorization.bindingIdentifier,
                initialRevision: revision,
                receive
            )
            switch result {
            case let .endOfStream(wasLive):
                return wasLive ? .liveEndOfStream : .endOfStream
            case let .http(response, body):
                let normalizedHeaders = Self.normalizedHeaders(response)
                if attemptIndex == 0,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    let recovered: DurableAuthorization
                    do {
                        recovered = try await authCoordinator.recoverFromUnauthorized(
                            rejectedBearer: authorization.bearerToken,
                            boundTo: baseURL
                        )
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                    guard recovered.bindingIdentifier == initialAuthorization.bindingIdentifier,
                          recovered.bindingIdentifier == expectedBindingIdentifier else {
                        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
                    }
                    authorization = recovered
                    tokensToRedact.append(recovered.bearerToken)
                    continue
                }
                if attemptIndex == 1,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    do {
                        try await authCoordinator.retireDefinitivelyRejectedAuthorization(
                            authorization,
                            boundTo: baseURL
                        )
                        throw DayWeaveAPIError.durableAuthentication(.reauthenticationRequired)
                    } catch let error as DayWeaveAPIError {
                        throw error
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                }
                try validatePostResponseBinding(initialAuthorization.bindingIdentifier)
                if response.statusCode == 404 {
                    return .unsupported
                }
                let envelope = try? makeDecoder().decode(ErrorEnvelope.self, from: body)
                throw DayWeaveAPIError.server(
                    statusCode: response.statusCode,
                    code: DayWeaveDiagnosticSanitizer.code(
                        envelope?.error.code,
                        secrets: tokensToRedact
                    ),
                    message: DayWeaveDiagnosticSanitizer.text(
                        envelope?.error.message,
                        secrets: tokensToRedact,
                        maximumCharacters: 500
                    ),
                    requestID: DayWeaveDiagnosticSanitizer.requestID(
                        response.value(forHTTPHeaderField: "x-request-id"),
                        secrets: tokensToRedact
                    )
                )
            }
        }
        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
    }

    func executionHistory(limit: Int = Self.maximumExecutionHistoryLimit) async throws
        -> [DayWeaveExecutionSession]
    {
        (try await executionHistoryPage(limit: limit, offset: 0)).sessions
    }

    func executionHistoryPage(
        limit: Int = Self.maximumExecutionHistoryLimit,
        offset: Int
    ) async throws -> DayWeaveExecutionHistoryPage {
        guard (1...Self.maximumExecutionHistoryLimit).contains(limit) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        guard offset >= 0 else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        var queryItems = [URLQueryItem(name: "limit", value: String(limit))]
        if offset > 0 {
            queryItems.append(URLQueryItem(name: "offset", value: String(offset)))
        }
        let envelope: DayWeaveExecutionHistoryEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "execution", "history"],
            queryItems: queryItems
        )
        let expectedNext = offset.addingReportingOverflow(envelope.sessions.count)
        guard envelope.sessions.count <= limit,
              Set(envelope.sessions.map(\.id)).count == envelope.sessions.count,
              envelope.sessions.count(where: { $0.status.isOpen }) <= 1,
              zip(envelope.sessions, envelope.sessions.dropFirst()).allSatisfy({ newer, older in
                  newer.updatedAt > older.updatedAt
                      || (newer.updatedAt == older.updatedAt
                          && newer.id.uuidString.lowercased()
                              > older.id.uuidString.lowercased())
              }),
              !expectedNext.overflow,
              (envelope.nextOffset.map {
                  envelope.sessions.count == limit
                      && $0 == expectedNext.partialValue
                      && $0 > offset
              } ?? true) else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return .init(sessions: envelope.sessions, nextOffset: envelope.nextOffset)
    }

    func assessExecutionDefer(
        _ request: DayWeaveDeferAssessmentRequest
    ) async throws -> DayWeaveDeferAssessment {
        guard request.hasValidShape,
              request.moveStart > now(),
              let actualSeconds = request.actualSeconds else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let envelope: DayWeaveDeferAssessmentEnvelope = try await send(
            method: "POST",
            pathComponents: ["v1", "execution", "defer-assessments"],
            body: try encode(request)
        )
        let assessment = envelope.assessment
        let receivedAt = now()
        guard assessment.hasValidShape,
              assessment.sessionID == request.sessionID,
              assessment.executionRevision == request.expectedRevision,
              assessment.moveStart == request.moveStart,
              assessment.actualSeconds == actualSeconds,
              assessment.expiresAt > receivedAt,
              assessment.expiresAt < assessment.moveStart else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return assessment
    }

    /// Produces the deterministic body that the caller must durably retain
    /// together with its idempotency key before the first network attempt.
    func encodedExecutionCommand(_ request: DayWeaveExecutionCommandRequest) throws -> Data {
        do {
            return try DayWeaveExecutionWireCodec.encode(request)
        } catch {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    func applyExecutionCommand(
        _ request: DayWeaveExecutionCommandRequest,
        idempotencyKey: String
    ) async throws -> DayWeaveExecutionMutation {
        try await applyExecutionCommand(
            encodedRequest: encodedExecutionCommand(request),
            idempotencyKey: idempotencyKey
        )
    }

    /// Replays a previously persisted byte-for-byte command body.
    func applyExecutionCommand(
        encodedRequest: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveExecutionMutation {
        let persistedRequest: DayWeaveExecutionCommandRequest
        do {
            guard encodedRequest.count <= Self.maximumRequestBytes else {
                throw DayWeaveAPIError.requestEncodingFailed
            }
            persistedRequest = try DayWeaveExecutionWireCodec.decode(encodedRequest)
        } catch let error as DayWeaveAPIError {
            throw error
        } catch {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let idempotencyBytes = idempotencyKey.utf8
        guard (8...128).contains(idempotencyBytes.count),
              idempotencyBytes.allSatisfy({ byte in
                  (48...57).contains(byte)
                      || (65...90).contains(byte)
                      || (97...122).contains(byte)
                      || [46, 95, 58, 45].contains(byte)
              }) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let envelope: DayWeaveExecutionMutationEnvelope = try await send(
            method: "POST",
            pathComponents: ["v1", "execution", "commands"],
            headers: ["Idempotency-Key": idempotencyKey],
            body: encodedRequest
        )
        let expectedMutationRevision = persistedRequest.expectedRevision + 1
        guard envelope.mutation.revision == expectedMutationRevision,
              persistedRequest.command.matchesChangedSession(
                  envelope.mutation.changedSession
              ) else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return envelope.mutation
    }

    func itemDelta(cursor: String?, limit: Int = 200) async throws -> DayWeaveItemDeltaPage {
        var queryItems = [URLQueryItem(name: "limit", value: String(limit))]
        if let cursor, !cursor.isEmpty {
            queryItems.append(URLQueryItem(name: "cursor", value: cursor))
        }
        let envelope: ItemDeltaEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "items", "delta"],
            queryItems: queryItems
        )
        return DayWeaveItemDeltaPage(
            changes: envelope.changes,
            nextCursor: envelope.nextCursor,
            hasMore: envelope.hasMore
        )
    }

    /// Opens the content-free canonical item invalidation stream. A received
    /// cursor is an untrusted hint only; callers must drain `/items/delta`
    /// from their encrypted cursor before changing durable canonical state.
    func consumeItemInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamCompletion {
        guard DayWeaveItemCursorContract.isValidTransportToken(cursor) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return try await withThrowingTaskGroup(
            of: DayWeaveItemStreamCompletion.self
        ) { group in
            group.addTask {
                try await consumeItemInvalidationsWithinLifetime(
                    after: cursor,
                    receive
                )
            }
            group.addTask {
                try await executionStreamLifetimeSleep()
                try Task.checkCancellation()
                throw DayWeaveAPIError.transport(.timedOut)
            }
            do {
                guard let result = try await group.next() else {
                    throw DayWeaveAPIError.transport(.unknown)
                }
                group.cancelAll()
                return result
            } catch {
                group.cancelAll()
                throw error
            }
        }
    }

    private func consumeItemInvalidationsWithinLifetime(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamCompletion {
        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: ["v1", "items", "stream"])
        } catch {
            throw DayWeaveAPIError.invalidEndpoint
        }

        var pristineRequest = URLRequest(url: endpoint)
        pristineRequest.httpMethod = "GET"
        pristineRequest.timeoutInterval = 330
        pristineRequest.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        pristineRequest.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        pristineRequest.setValue(cursor, forHTTPHeaderField: "Last-Event-ID")
        pristineRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        pristineRequest.setValue("no-cache", forHTTPHeaderField: "Pragma")
        pristineRequest.setValue("identity", forHTTPHeaderField: "Accept-Encoding")

        let initialAuthorization: DurableAuthorization
        if let authCoordinator {
            do {
                initialAuthorization = try await authCoordinator.authorization(boundTo: baseURL)
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
        } else {
            guard let bearerToken, !bearerToken.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            initialAuthorization = .init(
                bearerToken: bearerToken,
                bindingIdentifier: expectedBindingIdentifier,
                isDurable: false
            )
        }
        guard initialAuthorization.bindingIdentifier == expectedBindingIdentifier else {
            throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
        }

        var authorization = initialAuthorization
        var tokensToRedact = [authorization.bearerToken]
        for attemptIndex in 0...1 {
            let result = try await performItemInvalidationStreamRequest(
                pristineRequest,
                bearer: authorization.bearerToken,
                bindingIdentifier: initialAuthorization.bindingIdentifier,
                receive
            )
            switch result {
            case let .endOfStream(wasLive):
                return wasLive ? .liveEndOfStream : .endOfStream
            case let .http(response, body):
                let normalizedHeaders = Self.normalizedHeaders(response)
                if attemptIndex == 0,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    let recovered: DurableAuthorization
                    do {
                        recovered = try await authCoordinator.recoverFromUnauthorized(
                            rejectedBearer: authorization.bearerToken,
                            boundTo: baseURL
                        )
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                    guard recovered.bindingIdentifier == initialAuthorization.bindingIdentifier,
                          recovered.bindingIdentifier == expectedBindingIdentifier else {
                        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
                    }
                    authorization = recovered
                    tokensToRedact.append(recovered.bearerToken)
                    continue
                }
                if attemptIndex == 1,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    do {
                        try await authCoordinator.retireDefinitivelyRejectedAuthorization(
                            authorization,
                            boundTo: baseURL
                        )
                        throw DayWeaveAPIError.durableAuthentication(.reauthenticationRequired)
                    } catch let error as DayWeaveAPIError {
                        throw error
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                }
                try validatePostResponseBinding(initialAuthorization.bindingIdentifier)
                if response.statusCode == 404 { return .unsupported }
                let envelope = try? makeDecoder().decode(ErrorEnvelope.self, from: body)
                throw DayWeaveAPIError.server(
                    statusCode: response.statusCode,
                    code: DayWeaveDiagnosticSanitizer.code(
                        envelope?.error.code,
                        secrets: tokensToRedact
                    ),
                    message: DayWeaveDiagnosticSanitizer.text(
                        envelope?.error.message,
                        secrets: tokensToRedact,
                        maximumCharacters: 500
                    ),
                    requestID: DayWeaveDiagnosticSanitizer.requestID(
                        response.value(forHTTPHeaderField: "x-request-id"),
                        secrets: tokensToRedact
                    )
                )
            }
        }
        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
    }

    func listCanonicalItems(
        includeDeleted: Bool = false,
        parentID: UUID? = nil,
        limit: Int? = nil
    ) async throws -> [DayWeaveCanonicalItem] {
        guard limit.map({ (1...Self.maximumCanonicalItemListLimit).contains($0) }) ?? true else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        var queryItems = [
            URLQueryItem(name: "include_deleted", value: includeDeleted ? "true" : "false"),
        ]
        if let parentID {
            queryItems.append(URLQueryItem(
                name: "parent_id",
                value: parentID.uuidString.lowercased()
            ))
        }
        if let limit {
            queryItems.append(URLQueryItem(name: "limit", value: String(limit)))
        }
        let envelope: ItemListEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "items"],
            queryItems: queryItems,
            requiredStatusCode: 200
        )
        let effectiveLimit = limit ?? Self.defaultCanonicalItemListLimit
        guard envelope.items.count <= effectiveLimit,
              Set(envelope.items.map(\.id)).count == envelope.items.count,
              envelope.items.allSatisfy({ item in
                  (includeDeleted || item.deletedAt == nil)
                      && (parentID.map({ item.parentID == $0 }) ?? true)
              }) else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return envelope.items
    }

    func canonicalItem(_ id: UUID) async throws -> DayWeaveCanonicalItem {
        let envelope: ItemEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "items", id.uuidString.lowercased()],
            requiredStatusCode: 200
        )
        guard envelope.item.id == id, envelope.item.deletedAt == nil else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return envelope.item
    }

    func createCanonicalItem(
        _ item: DayWeaveNewCanonicalItem,
        idempotencyKey: String
    ) async throws -> DayWeaveCanonicalItem {
        let envelope: ItemEnvelope = try await send(
            method: "POST",
            pathComponents: ["v1", "items"],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(item)
        )
        return envelope.item
    }

    func replaceCanonicalItem(
        _ id: UUID,
        expectedRevision: UInt64,
        item: DayWeaveCanonicalItemFields,
        idempotencyKey: String
    ) async throws -> DayWeaveCanonicalItem {
        let envelope: ItemEnvelope = try await send(
            method: "PUT",
            pathComponents: ["v1", "items", id.uuidString.lowercased()],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(ReplaceItemRequest(expectedRevision: expectedRevision, item: item))
        )
        return envelope.item
    }

    func trashCanonicalItem(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> DayWeaveCanonicalItem {
        try validateCanonicalMutationRequest(
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey
        )
        let envelope: ItemEnvelope = try await send(
            method: "DELETE",
            pathComponents: ["v1", "items", id.uuidString.lowercased()],
            queryItems: [
                URLQueryItem(name: "expected_revision", value: String(expectedRevision)),
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            requiredStatusCode: 200
        )
        return try validateCanonicalMutationResponse(
            envelope.item,
            id: id,
            expectedRevision: expectedRevision,
            isDeleted: true
        )
    }

    func restoreCanonicalItem(
        _ id: UUID,
        expectedRevision: UInt64,
        idempotencyKey: String
    ) async throws -> DayWeaveCanonicalItem {
        try validateCanonicalMutationRequest(
            expectedRevision: expectedRevision,
            idempotencyKey: idempotencyKey
        )
        let envelope: ItemEnvelope = try await send(
            method: "POST",
            pathComponents: ["v1", "items", id.uuidString.lowercased(), "restore"],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(RevisionRequest(expectedRevision: expectedRevision)),
            requiredStatusCode: 200
        )
        return try validateCanonicalMutationResponse(
            envelope.item,
            id: id,
            expectedRevision: expectedRevision,
            isDeleted: false
        )
    }

    private func validateCanonicalMutationRequest(
        expectedRevision: UInt64,
        idempotencyKey: String
    ) throws {
        guard expectedRevision > 0,
              expectedRevision < UInt64.max,
              Self.isValidCanonicalItemIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private func validateCanonicalMutationResponse(
        _ item: DayWeaveCanonicalItem,
        id: UUID,
        expectedRevision: UInt64,
        isDeleted: Bool
    ) throws -> DayWeaveCanonicalItem {
        guard item.id == id,
              item.revision == expectedRevision + 1,
              (item.deletedAt != nil) == isDeleted else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return item
    }

    private static func isValidCanonicalItemIdempotencyKey(_ value: String) -> Bool {
        (8...128).contains(value.utf8.count)
            && value.utf8.allSatisfy { byte in
                (byte >= 65 && byte <= 90)
                    || (byte >= 97 && byte <= 122)
                    || (byte >= 48 && byte <= 57)
                    || [45, 46, 58, 95].contains(byte)
            }
    }

    /// Opens the content-free habit invalidation stream. A cursor received
    /// here is never installed as durable state; callers must drain the
    /// authenticated habit delta from their encrypted cursor.
    func consumeHabitInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveHabitStreamCompletion {
        guard DayWeaveHabitCursorContract.isValidTransportToken(cursor) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return try await withThrowingTaskGroup(
            of: DayWeaveHabitStreamCompletion.self
        ) { group in
            group.addTask {
                try await consumeHabitInvalidationsWithinLifetime(
                    after: cursor,
                    receive
                )
            }
            group.addTask {
                try await executionStreamLifetimeSleep()
                try Task.checkCancellation()
                throw DayWeaveAPIError.transport(.timedOut)
            }
            do {
                guard let result = try await group.next() else {
                    throw DayWeaveAPIError.transport(.unknown)
                }
                group.cancelAll()
                return result
            } catch {
                group.cancelAll()
                throw error
            }
        }
    }

    private func consumeHabitInvalidationsWithinLifetime(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveHabitStreamCompletion {
        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: ["v1", "habits", "stream"])
        } catch {
            throw DayWeaveAPIError.invalidEndpoint
        }

        var pristineRequest = URLRequest(url: endpoint)
        pristineRequest.httpMethod = "GET"
        pristineRequest.timeoutInterval = 330
        pristineRequest.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        pristineRequest.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        pristineRequest.setValue(cursor, forHTTPHeaderField: "Last-Event-ID")
        pristineRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        pristineRequest.setValue("no-cache", forHTTPHeaderField: "Pragma")
        pristineRequest.setValue("identity", forHTTPHeaderField: "Accept-Encoding")

        let initialAuthorization: DurableAuthorization
        if let authCoordinator {
            do {
                initialAuthorization = try await authCoordinator.authorization(boundTo: baseURL)
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
        } else {
            guard let bearerToken, !bearerToken.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            initialAuthorization = .init(
                bearerToken: bearerToken,
                bindingIdentifier: expectedBindingIdentifier,
                isDurable: false
            )
        }
        guard initialAuthorization.bindingIdentifier == expectedBindingIdentifier else {
            throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
        }

        var authorization = initialAuthorization
        var tokensToRedact = [authorization.bearerToken]
        for attemptIndex in 0...1 {
            let result = try await performHabitInvalidationStreamRequest(
                pristineRequest,
                bearer: authorization.bearerToken,
                bindingIdentifier: initialAuthorization.bindingIdentifier,
                receive
            )
            switch result {
            case let .endOfStream(wasLive):
                return wasLive ? .liveEndOfStream : .endOfStream
            case let .http(response, body):
                let normalizedHeaders = Self.normalizedHeaders(response)
                if attemptIndex == 0,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    let recovered: DurableAuthorization
                    do {
                        recovered = try await authCoordinator.recoverFromUnauthorized(
                            rejectedBearer: authorization.bearerToken,
                            boundTo: baseURL
                        )
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                    guard recovered.bindingIdentifier == initialAuthorization.bindingIdentifier,
                          recovered.bindingIdentifier == expectedBindingIdentifier else {
                        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
                    }
                    authorization = recovered
                    tokensToRedact.append(recovered.bearerToken)
                    continue
                }
                if attemptIndex == 1,
                   response.statusCode == 401,
                   let authCoordinator,
                   DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
                       statusCode: response.statusCode,
                       headers: normalizedHeaders,
                       body: body
                   ) {
                    do {
                        try await authCoordinator.retireDefinitivelyRejectedAuthorization(
                            authorization,
                            boundTo: baseURL
                        )
                        throw DayWeaveAPIError.durableAuthentication(.reauthenticationRequired)
                    } catch let error as DayWeaveAPIError {
                        throw error
                    } catch let error as DurableAuthError {
                        throw DayWeaveAPIError.durableAuthentication(error)
                    } catch {
                        throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
                    }
                }
                try validatePostResponseBinding(initialAuthorization.bindingIdentifier)
                if response.statusCode == 404 { return .unsupported }
                let envelope = try? makeDecoder().decode(ErrorEnvelope.self, from: body)
                throw DayWeaveAPIError.server(
                    statusCode: response.statusCode,
                    code: DayWeaveDiagnosticSanitizer.code(
                        envelope?.error.code,
                        secrets: tokensToRedact
                    ),
                    message: DayWeaveDiagnosticSanitizer.text(
                        envelope?.error.message,
                        secrets: tokensToRedact,
                        maximumCharacters: 500
                    ),
                    requestID: DayWeaveDiagnosticSanitizer.requestID(
                        response.value(forHTTPHeaderField: "x-request-id"),
                        secrets: tokensToRedact
                    )
                )
            }
        }
        throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
    }

    func habitOccurrences(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        cursor: String? = nil,
        limit: Int = 100
    ) async throws -> DayWeaveHabitOccurrencePage {
        guard habitID != Self.nilUUID,
              Self.isValidHabitDateRange(startDate, endDate),
              (1...200).contains(limit),
              cursor.map(Self.isValidHabitCursor) ?? true else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        var queryItems = [
            URLQueryItem(name: "start_date", value: startDate.rawValue),
            URLQueryItem(name: "end_date", value: endDate.rawValue),
            URLQueryItem(name: "limit", value: String(limit)),
        ]
        if let cursor {
            queryItems.append(URLQueryItem(name: "cursor", value: cursor))
        }
        let page: DayWeaveHabitOccurrencePage = try await send(
            method: "GET",
            pathComponents: ["v1", "habits", habitID.uuidString.lowercased(), "occurrences"],
            queryItems: queryItems,
            requiredStatusCode: 200
        )
        guard page.occurrences.allSatisfy({ occurrence in
            occurrence.evidence.habitID == habitID
                && occurrence.evidence.localDate >= startDate
                && occurrence.evidence.localDate <= endDate
        }) else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return page
    }

    func putHabitOutcome(
        habitID: UUID,
        occurrenceID: UUID,
        command: DayWeaveHabitOutcomeCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitOccurrenceMutationResponse {
        guard habitID != Self.nilUUID,
              occurrenceID != Self.nilUUID,
              command.expectedRevision < UInt64.max,
              command.hasValidShape,
              Self.isValidHabitIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let response: DayWeaveHabitOccurrenceMutationResponse = try await send(
            method: "PUT",
            pathComponents: [
                "v1", "habits", habitID.uuidString.lowercased(), "occurrences",
                occurrenceID.uuidString.lowercased(),
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(command),
            requiredStatusCode: 200
        )
        guard response.occurrence.evidence.habitID == habitID,
              response.occurrence.id == occurrenceID,
              response.occurrence.outcome?.revision == command.expectedRevision + 1,
              response.occurrence.outcome.map({
                  Self.hasSameHabitOutcomeWireValue($0.input, command.outcome)
              }) == true else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return response
    }

    func startHabitPause(
        habitID: UUID,
        command: DayWeaveHabitPauseStartCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitPauseMutationResponse {
        guard habitID != Self.nilUUID,
              command.hasValidShape,
              Self.isValidHabitIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let response: DayWeaveHabitPauseMutationResponse = try await send(
            method: "POST",
            pathComponents: ["v1", "habits", habitID.uuidString.lowercased(), "pauses"],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(command),
            requiredStatusCode: 200
        )
        guard response.pause.habitID == habitID,
              response.pause.id == command.pauseID,
              response.pause.revision == 1,
              Self.hasSameHabitInstant(response.pause.startedAt, command.startedAt),
              response.pause.endedAt == nil else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return response
    }

    func resumeHabitPause(
        habitID: UUID,
        pauseID: UUID,
        command: DayWeaveHabitPauseResumeCommand,
        idempotencyKey: String
    ) async throws -> DayWeaveHabitPauseMutationResponse {
        guard habitID != Self.nilUUID,
              pauseID != Self.nilUUID,
              command.expectedRevision < UInt64.max,
              command.hasValidShape,
              Self.isValidHabitIdempotencyKey(idempotencyKey) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let response: DayWeaveHabitPauseMutationResponse = try await send(
            method: "POST",
            pathComponents: [
                "v1", "habits", habitID.uuidString.lowercased(), "pauses",
                pauseID.uuidString.lowercased(), "resume",
            ],
            headers: ["Idempotency-Key": idempotencyKey],
            body: try encode(command),
            requiredStatusCode: 200
        )
        guard response.pause.habitID == habitID,
              response.pause.id == pauseID,
              response.pause.revision == command.expectedRevision + 1,
              response.pause.endedAt.map({
                  Self.hasSameHabitInstant($0, command.endedAt)
              }) == true else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return response
    }

    func habitDelta(cursor: String? = nil, limit: Int = 100) async throws
        -> DayWeaveHabitDeltaPage
    {
        guard (1...200).contains(limit), cursor.map(Self.isValidHabitCursor) ?? true else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        var queryItems = [URLQueryItem(name: "limit", value: String(limit))]
        if let cursor {
            queryItems.append(URLQueryItem(name: "cursor", value: cursor))
        }
        return try await send(
            method: "GET",
            pathComponents: ["v1", "habits", "occurrences", "delta"],
            queryItems: queryItems,
            requiredStatusCode: 200
        )
    }

    func habitAnalytics(
        habitID: UUID,
        startDate: DayWeaveLocalDate,
        endDate: DayWeaveLocalDate,
        bucket: DayWeaveHabitAnalyticsBucket
    ) async throws -> DayWeaveHabitAnalytics {
        guard habitID != Self.nilUUID,
              Self.isValidHabitDateRange(startDate, endDate) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let envelope: DayWeaveHabitAnalyticsEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "habits", habitID.uuidString.lowercased(), "analytics"],
            queryItems: [
                URLQueryItem(name: "start_date", value: startDate.rawValue),
                URLQueryItem(name: "end_date", value: endDate.rawValue),
                URLQueryItem(name: "bucket", value: bucket.rawValue),
            ],
            requiredStatusCode: 200
        )
        guard envelope.analytics.habitID == habitID,
              envelope.analytics.startDate == startDate,
              envelope.analytics.endDate == endDate,
              envelope.analytics.bucket == bucket else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return envelope.analytics
    }

    private static let nilUUID = UUID(
        uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    )

    private static func isValidHabitIdempotencyKey(_ value: String) -> Bool {
        (8...128).contains(value.utf8.count) && value.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
                || [45, 46, 58, 95].contains(byte)
        }
    }

    private static func isValidHabitCursor(_ value: String) -> Bool {
        DayWeaveHabitCursorContract.isValidTransportToken(value)
    }

    private static func isValidHabitDateRange(
        _ startDate: DayWeaveLocalDate,
        _ endDate: DayWeaveLocalDate
    ) -> Bool {
        guard startDate <= endDate,
              let start = startDate.date(in: "UTC"),
              let end = endDate.date(in: "UTC") else { return false }
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        guard let elapsedDays = calendar.dateComponents(
            [.day],
            from: start,
            to: end
        ).day else { return false }
        // Both endpoints are inclusive, so a difference of 365 is the
        // service's maximum 366-day projection.
        return (0..<366).contains(elapsedDays)
    }

    /// Requests are serialized at PostgreSQL's six-digit precision. `Date`
    /// can retain finer binary fractions in memory, so response-echo checks
    /// compare the exact transmitted instant instead of the pre-encoding bit
    /// pattern.
    private static func hasSameHabitInstant(_ left: Date, _ right: Date) -> Bool {
        guard let left = CanonicalRFC3339Instant(date: left),
              let right = CanonicalRFC3339Instant(date: right) else { return false }
        return left.microsecondsSinceUnixEpoch == right.microsecondsSinceUnixEpoch
    }

    private static func hasSameHabitOutcomeWireValue(
        _ left: DayWeaveHabitOutcomeInput,
        _ right: DayWeaveHabitOutcomeInput
    ) -> Bool {
        left.status == right.status
            && left.progressBasisPoints == right.progressBasisPoints
            && left.quantity == right.quantity
            && left.unit == right.unit
            && left.actualSeconds == right.actualSeconds
            && left.note == right.note
            && hasSameHabitInstant(left.occurredAt, right.occurredAt)
    }

    func previewSchedule(
        _ request: DayWeaveSchedulePreviewRequest
    ) async throws -> DayWeaveSchedulePreview {
        try await send(
            method: "POST",
            pathComponents: ["v1", "schedule", "preview"],
            body: try encode(request)
        )
    }

    /// Fetches the one authoritative immutable publication. A nil result is
    /// returned only for the endpoint's exact typed 404 contract; malformed
    /// errors, cacheable responses, duplicate keys, and widened JSON shapes
    /// fail closed.
    func currentPublishedSchedule() async throws -> DayWeaveCurrentPublishedSchedule? {
        do {
            let current: DayWeaveCurrentPublishedSchedule = try await send(
                method: "GET",
                pathComponents: ["v1", "schedule", "current"],
                requiredStatusCode: 200
            )
            return current
        } catch DayWeaveAPIError.trustedCurrentScheduleAbsent {
            return nil
        }
    }

    func prepareSchedulePublication(
        _ request: DayWeaveSchedulePublishRequest
    ) throws -> DayWeavePreparedSchedulePublication {
        let body = try encode(request)
        guard body.count <= Self.maximumRequestBytes,
              let canonicalRequest = try? makeDecoder().decode(
                  DayWeaveSchedulePublishRequest.self,
                  from: body
              ) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        return .init(
            // RFC 3339 carries millisecond precision on the wire. Keep the
            // semantic copy decoded from the exact bytes so ordinary Date()
            // sub-milliseconds cannot make a valid journal fail equality after
            // persistence or restart.
            request: canonicalRequest,
            body: body,
            bodySHA256: Self.sha256(body)
        )
    }

    func publishSchedule(
        _ prepared: DayWeavePreparedSchedulePublication
    ) async throws -> DayWeaveSchedulePublishResponse {
        try validatePreparedSchedulePublication(prepared)
        return try await send(
            method: "POST",
            pathComponents: ["v1", "schedule", "publish"],
            body: prepared.body,
            requiredStatusCode: 200
        )
    }

    func validatePreparedSchedulePublication(
        _ prepared: DayWeavePreparedSchedulePublication
    ) throws {
        guard prepared.body.count <= Self.maximumRequestBytes,
              prepared.bodySHA256 == Self.sha256(prepared.body),
              (try? makeDecoder().decode(
                  DayWeaveSchedulePublishRequest.self,
                  from: prepared.body
              )) == prepared.request else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private func encode(_ value: some Encodable) throws -> Data {
        do {
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .custom { date, encoder in
                var container = encoder.singleValueContainer()
                try container.encode(Self.format(date))
            }
            encoder.outputFormatting = [.sortedKeys]
            return try encoder.encode(value)
        } catch {
            throw DayWeaveAPIError.requestEncodingFailed
        }
    }

    private func send<Response: Decodable>(
        method: String,
        pathComponents: [String],
        queryItems: [URLQueryItem] = [],
        headers: [String: String] = [:],
        body: Data? = nil,
        requiredStatusCode: Int? = nil,
        requiresDurableAuthorization: Bool = false,
        additionalSecretsToRedact: [String] = []
    ) async throws -> Response {
        if let body, body.count > Self.maximumRequestBytes {
            throw DayWeaveAPIError.requestEncodingFailed
        }

        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: pathComponents, queryItems: queryItems)
        } catch {
            throw DayWeaveAPIError.invalidEndpoint
        }

        var pristineRequest = URLRequest(url: endpoint)
        pristineRequest.httpMethod = method
        pristineRequest.httpBody = body
        pristineRequest.timeoutInterval = 20
        pristineRequest.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        pristineRequest.setValue("application/json", forHTTPHeaderField: "Accept")
        pristineRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        pristineRequest.setValue("no-cache", forHTTPHeaderField: "Pragma")
        for (name, value) in headers {
            pristineRequest.setValue(value, forHTTPHeaderField: name)
        }
        if body != nil {
            pristineRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let initialAuthorization: DurableAuthorization
        if let authCoordinator {
            do {
                initialAuthorization = try await authCoordinator.authorization(boundTo: baseURL)
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
        } else {
            guard let token = bearerToken, !token.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            initialAuthorization = .init(
                bearerToken: token,
                bindingIdentifier: expectedBindingIdentifier,
                isDurable: false
            )
        }
        guard initialAuthorization.bindingIdentifier == expectedBindingIdentifier else {
            throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
        }
        guard !requiresDurableAuthorization || initialAuthorization.isDurable else {
            throw DayWeaveAPIError.durableAuthentication(.enrollmentRequired)
        }

        var tokensToRedact = additionalSecretsToRedact + [initialAuthorization.bearerToken]
        var replayedAuthorization: DurableAuthorization?
        var result = try await perform(
            pristineRequest,
            bearer: initialAuthorization.bearerToken
        )
        if result.response.statusCode == 401, let authCoordinator,
           DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
               statusCode: result.response.statusCode,
               headers: Self.normalizedHeaders(result.response),
               body: result.data
           ) {
            let recovered: DurableAuthorization
            do {
                recovered = try await authCoordinator.recoverFromUnauthorized(
                    rejectedBearer: initialAuthorization.bearerToken,
                    boundTo: baseURL
                )
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
            guard recovered.bindingIdentifier == initialAuthorization.bindingIdentifier,
                  recovered.bindingIdentifier == expectedBindingIdentifier else {
                throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
            }
            guard !requiresDurableAuthorization || recovered.isDurable else {
                throw DayWeaveAPIError.durableAuthentication(.enrollmentRequired)
            }
            tokensToRedact.append(recovered.bearerToken)
            replayedAuthorization = recovered
            // `pristineRequest` is retained untouched. The replay changes only
            // Authorization; method, URL, headers, and body bytes are identical.
            result = try await perform(pristineRequest, bearer: recovered.bearerToken)
        }
        if result.response.statusCode == 401,
           let replayedAuthorization,
           let authCoordinator,
           DayWeaveAuthResponseContract.isDefinitiveUnauthorized(
               statusCode: result.response.statusCode,
               headers: Self.normalizedHeaders(result.response),
               body: result.data
           ) {
            do {
                try await authCoordinator.retireDefinitivelyRejectedAuthorization(
                    replayedAuthorization,
                    boundTo: baseURL
                )
                throw DayWeaveAPIError.durableAuthentication(.reauthenticationRequired)
            } catch let error as DayWeaveAPIError {
                throw error
            } catch let error as DurableAuthError {
                throw DayWeaveAPIError.durableAuthentication(error)
            } catch {
                throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
            }
        }
        try validatePostResponseBinding(initialAuthorization.bindingIdentifier)

        let data = result.data
        let httpResponse = result.response
        let hasAcceptedStatus = requiredStatusCode.map {
            httpResponse.statusCode == $0
        } ?? (200..<300).contains(httpResponse.statusCode)
        guard hasAcceptedStatus else {
            if Self.isHabitEndpoint(pathComponents),
               !Self.isValidHabitJSONErrorResponse(
                   contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                   cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                   pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                   body: data
               ) {
                throw DayWeaveAPIError.responseDecodingFailed
            }
            if pathComponents == ["v1", "schedule", "current"],
               Self.isTrustedCurrentScheduleAbsent(
                   statusCode: httpResponse.statusCode,
                   contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                   cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                   pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                   body: data
               ) {
                throw DayWeaveAPIError.trustedCurrentScheduleAbsent
            }
            if pathComponents == ["v1", "schedule", "publish"],
               Self.isTrustedSchedulePublicationStale(
                   statusCode: httpResponse.statusCode,
                   contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                   body: data
            ) {
                throw DayWeaveAPIError.trustedSchedulePublicationStale
            }
            if let trusted = Self.trustedProposalApplicationError(
                pathComponents: pathComponents,
                statusCode: httpResponse.statusCode,
                contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                body: data
            ) {
                throw trusted
            }
            if let trusted = Self.trustedCanonicalMutationError(
                method: method,
                pathComponents: pathComponents,
                statusCode: httpResponse.statusCode,
                contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                body: data
            ) {
                throw trusted
            }
            if let trusted = Self.trustedGoogleDisconnectError(
                method: method,
                pathComponents: pathComponents,
                queryItems: queryItems,
                statusCode: httpResponse.statusCode,
                contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                body: data
            ) {
                throw trusted
            }
            let envelope = try? makeDecoder().decode(ErrorEnvelope.self, from: data)
            throw DayWeaveAPIError.server(
                statusCode: httpResponse.statusCode,
                code: DayWeaveDiagnosticSanitizer.code(
                    envelope?.error.code,
                    secrets: tokensToRedact
                ),
                message: DayWeaveDiagnosticSanitizer.text(
                    envelope?.error.message,
                    secrets: tokensToRedact,
                    maximumCharacters: 500
                ),
                requestID: DayWeaveDiagnosticSanitizer.requestID(
                    httpResponse.value(forHTTPHeaderField: "x-request-id"),
                    secrets: tokensToRedact
                )
            )
        }

        do {
            if Self.isHabitEndpoint(pathComponents),
               !Self.isValidHabitJSONResponse(
                   method: method,
                   pathComponents: pathComponents,
                   contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                   cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                   pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                   replayHeader: httpResponse.value(forHTTPHeaderField: "idempotency-replayed"),
                   body: data
               ) {
                throw DayWeaveAPIError.responseDecodingFailed
            }
            if pathComponents == ["v1", "schedule", "current"],
               !Self.isValidCurrentScheduleResponse(
                   contentType: httpResponse.value(forHTTPHeaderField: "content-type"),
                   cacheControl: httpResponse.value(forHTTPHeaderField: "cache-control"),
                   pragma: httpResponse.value(forHTTPHeaderField: "pragma"),
                   etag: httpResponse.value(forHTTPHeaderField: "etag"),
                   body: data
               ) {
                throw DayWeaveAPIError.responseDecodingFailed
            }
            if (pathComponents.contains("outbound")
                || pathComponents.contains("schedule-publications")),
               !StrictJSONObjectKeyScanner.hasUniqueKeys(in: data) {
                throw DayWeaveAPIError.responseDecodingFailed
            }
            return try makeDecoder().decode(Response.self, from: data)
        } catch let error as DayWeaveAPIError {
            throw error
        } catch {
            throw DayWeaveAPIError.responseDecodingFailed
        }
    }

    private enum ProposalApplicationEndpoint: Equatable {
        case lookup
        case apply
        case undo
    }

    private static func isHabitEndpoint(_ pathComponents: [String]) -> Bool {
        pathComponents.count >= 2
            && pathComponents[0] == "v1"
            && pathComponents[1] == "habits"
    }

    private static func isValidHabitJSONResponse(
        method: String,
        pathComponents: [String],
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        replayHeader: String?,
        body: Data
    ) -> Bool {
        guard !body.isEmpty,
              body.count <= Self.maximumResponseBytes,
              isStrictJSONMediaType(contentType),
              cacheControl?.lowercased() == "no-store, max-age=0",
              pragma?.lowercased() == "no-cache",
              StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: body) else {
            return false
        }

        let isMutation = method == "PUT"
            || (method == "POST" && pathComponents.contains("pauses"))
        guard isMutation else { return replayHeader == nil }
        guard let replayHeader = replayHeader?.lowercased(),
              replayHeader == "true" || replayHeader == "false",
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let replayed = object["replayed"] as? Bool else { return false }
        return replayed == (replayHeader == "true")
    }

    private static func isValidHabitJSONErrorResponse(
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        body: Data
    ) -> Bool {
        guard !body.isEmpty,
              body.count <= Self.maximumResponseBytes,
              isStrictJSONMediaType(contentType),
              cacheControl?.lowercased() == "no-store, max-age=0",
              pragma?.lowercased() == "no-cache",
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: body),
              let root = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(root.keys) == ["error"],
              let error = root["error"] as? [String: Any],
              Set(["code", "message"]).isSubset(of: Set(error.keys)),
              Set(error.keys).isSubset(of: ["code", "message", "details"]),
              let code = error["code"] as? String,
              !code.isEmpty,
              code.utf8.count <= 128,
              code.utf8.allSatisfy({
                  (97...122).contains($0) || (48...57).contains($0) || $0 == 95
              }),
              let message = error["message"] as? String,
              !message.isEmpty,
              message.utf8.count <= 16_384 else { return false }
        return true
    }

    private enum CanonicalMutationEndpoint: Equatable {
        case create
        case replace
        case trash
        case restore
    }

    private static func trustedCanonicalMutationError(
        method: String,
        pathComponents: [String],
        statusCode: Int,
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        body: Data
    ) -> DayWeaveAPIError? {
        guard statusCode == 409,
              body.count <= 8 * 1_024,
              isStrictJSONMediaType(contentType),
              cacheControl?.lowercased() == "no-store, max-age=0",
              pragma?.lowercased() == "no-cache",
              let endpoint = canonicalMutationEndpoint(
                  method: method,
                  pathComponents: pathComponents
              ),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              error["code"] as? String == "conflict",
              let message = error["message"] as? String else { return nil }

        if Set(error.keys) == ["code", "message"],
           message == "matching idempotent request is still in progress" {
            return .trustedCanonicalMutationInProgress
        }

        if Set(error.keys) == ["code", "message", "details"],
           endpoint != .create,
           message == "item was changed by another request",
           let details = error["details"] as? [String: Any],
           Set(details.keys) == ["expected_revision", "actual_revision"],
           isStrictPositiveJSONInteger(details["expected_revision"]),
           isStrictPositiveJSONInteger(details["actual_revision"]) {
            return .trustedCanonicalMutationNoEffect(conflictCode: "revision_conflict")
        }

        guard Set(error.keys) == ["code", "message"] else { return nil }
        let common = [
            "Idempotency-Key was already used for different content": "idempotency_conflict",
        ]
        let endpointSpecific: [String: String] = switch endpoint {
        case .create:
            [
                "item already exists": "item_already_exists",
                "item cannot be its own parent": "self_parent",
                "item hierarchy would contain a cycle": "hierarchy_cycle",
                "item dependency graph would contain a cycle": "dependency_cycle",
                "an executing or terminal item cannot become a parent": "invalid_parent_state",
            ]
        case .replace:
            [
                "item cannot be its own parent": "self_parent",
                "item hierarchy would contain a cycle": "hierarchy_cycle",
                "item dependency graph would contain a cycle": "dependency_cycle",
                "an executing or terminal item cannot become a parent": "invalid_parent_state",
                "only leaf items can enter an executable state": "non_leaf_executable",
            ]
        case .trash:
            ["an item with active children cannot be deleted": "has_children"]
        case .restore:
            [
                "deleted item's parent must be restored first": "deleted_parent",
                "only leaf items can enter an executable state": "non_leaf_executable",
            ]
        }
        guard let conflictCode = endpointSpecific[message] ?? common[message] else {
            return nil
        }
        return .trustedCanonicalMutationNoEffect(conflictCode: conflictCode)
    }

    private static func trustedGoogleDisconnectError(
        method: String,
        pathComponents: [String],
        queryItems: [URLQueryItem],
        statusCode: Int,
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        body: Data
    ) -> DayWeaveAPIError? {
        guard method == "DELETE",
              pathComponents.count == 5,
              pathComponents[0...3] == ["v1", "integrations", "google", "accounts"],
              UUID(uuidString: pathComponents[4]) != nil,
              queryItems.count == 1,
              queryItems[0].name == "expected_revision",
              let requestedRevision = UInt64(queryItems[0].value ?? ""),
              requestedRevision > 0,
              statusCode == 409,
              body.count <= 8 * 1_024,
              isStrictJSONMediaType(contentType),
              cacheControl?.lowercased() == "no-store, max-age=0",
              pragma?.lowercased() == "no-cache",
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: body),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              Set(error.keys) == ["code", "message", "details"],
              error["code"] as? String == "conflict",
              error["message"] as? String == "Google account changed on another device",
              let details = error["details"] as? [String: Any],
              Set(details.keys) == ["expected_revision", "actual_revision"],
              isStrictPositiveJSONInteger(details["expected_revision"]),
              isStrictPositiveJSONInteger(details["actual_revision"]),
              (details["expected_revision"] as? NSNumber)?.uint64Value
                == requestedRevision,
              let actualRevision = (details["actual_revision"] as? NSNumber)?.uint64Value,
              actualRevision != requestedRevision else { return nil }
        return .trustedGoogleDisconnectNoEffect
    }

    private static func isStrictPositiveJSONInteger(_ value: Any?) -> Bool {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              let parsed = UInt64(number.stringValue),
              parsed > 0,
              number.stringValue == String(parsed) else { return false }
        return true
    }

    private static func canonicalMutationEndpoint(
        method: String,
        pathComponents: [String]
    ) -> CanonicalMutationEndpoint? {
        if method == "POST", pathComponents == ["v1", "items"] {
            return .create
        }
        if pathComponents.count == 3,
           pathComponents[0] == "v1", pathComponents[1] == "items",
           UUID(uuidString: pathComponents[2]) != nil {
            return switch method {
            case "PUT": .replace
            case "DELETE": .trash
            default: nil
            }
        }
        if method == "POST", pathComponents.count == 4,
           pathComponents[0] == "v1", pathComponents[1] == "items",
           UUID(uuidString: pathComponents[2]) != nil,
           pathComponents[3] == "restore" {
            return .restore
        }
        return nil
    }

    private static func trustedProposalApplicationError(
        pathComponents: [String],
        statusCode: Int,
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        body: Data
    ) -> DayWeaveAPIError? {
        guard body.count <= 8 * 1_024,
              isStrictJSONMediaType(contentType),
              cacheControl?.lowercased() == "no-store, max-age=0",
              pragma?.lowercased() == "no-cache",
              let endpoint = proposalApplicationEndpoint(pathComponents),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              let code = error["code"] as? String,
              let message = error["message"] as? String else {
            return nil
        }

        if statusCode == 404,
           Set(error.keys) == ["code", "message"],
           code == "not_found",
           message == "proposal application was not found" {
            return .trustedProposalApplicationAbsent
        }

        guard statusCode == 409,
              endpoint != .lookup,
              Set(error.keys) == ["code", "message", "details"],
              code == "conflict",
              message == "Proposal application is stale or unsafe",
              let details = error["details"] as? [String: Any],
              Set(details.keys) == ["conflict_code"],
              let conflictCode = details["conflict_code"] as? String else {
            return nil
        }
        let applyNoEffect = Set([
            "proposal_not_pending", "proposal_expired", "proposal_revision_mismatch",
            "item_already_exists", "item_not_found", "item_revision_mismatch",
            "parent_not_found", "hierarchy_cycle", "invalid_parent_state",
            "dependency_not_found", "dependency_cycle",
            "non_leaf_executable", "has_children", "deleted_parent", "invalid_item",
            "provider_managed_item", "preview_expired", "preview_mismatch",
            "preview_not_applicable",
        ])
        let undoNoEffect = Set([
            "provider_managed_item", "undo_expired", "undo_diverged",
        ])
        let isNoEffect = switch endpoint {
        case .lookup:
            false
        case .apply:
            applyNoEffect.contains(conflictCode)
        case .undo:
            undoNoEffect.contains(conflictCode)
        }
        return isNoEffect
            ? .trustedProposalApplicationNoEffect(conflictCode: conflictCode)
            : nil
    }

    private static func proposalApplicationEndpoint(
        _ components: [String]
    ) -> ProposalApplicationEndpoint? {
        if components.count == 4,
           components[0] == "v1", components[1] == "suggestions",
           ((components[2] == "applications" && UUID(uuidString: components[3]) != nil)
               || (UUID(uuidString: components[2]) != nil && components[3] == "application")) {
            return .lookup
        }
        if components.count == 5,
           components[0] == "v1", components[1] == "suggestions",
           components[2] == "application-previews",
           UUID(uuidString: components[3]) != nil,
           components[4] == "apply" {
            return .apply
        }
        if components.count == 5,
           components[0] == "v1", components[1] == "suggestions",
           components[2] == "applications",
           UUID(uuidString: components[3]) != nil,
           components[4] == "undo" {
            return .undo
        }
        return nil
    }

    private static func isTrustedSchedulePublicationStale(
        statusCode: Int,
        contentType: String?,
        body: Data
    ) -> Bool {
        guard statusCode == 409,
              body.count <= 8 * 1_024,
              isStrictJSONMediaType(contentType),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              Set(error.keys) == ["code", "message"],
              let code = error["code"] as? String,
              let message = error["message"] as? String,
              code == "schedule_publication_stale",
              !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              message.utf16.count <= 500,
              !message.unicodeScalars.contains(
                  where: CharacterSet.controlCharacters.contains
              ) else {
            return false
        }
        return true
    }

    private static func isTrustedCurrentScheduleAbsent(
        statusCode: Int,
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        body: Data
    ) -> Bool {
        guard statusCode == 404,
              body.count <= 8 * 1_024,
              isStrictJSONMediaType(contentType),
              isNoStoreZeroAge(cacheControl),
              pragma?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                == "no-cache",
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: body),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              Set(error.keys) == ["code", "message"],
              error["code"] as? String == "not_found",
              error["message"] as? String == "Published schedule was not found" else {
            return false
        }
        return true
    }

    private static func trustedScheduleCursorAhead(
        requestedRevision: UInt64,
        statusCode: Int,
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        body: Data
    ) -> UInt64? {
        guard statusCode == 409,
              body.count <= 8 * 1_024,
              isStrictJSONMediaType(contentType),
              isNoStoreZeroAge(cacheControl),
              pragma?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                == "no-cache",
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: body),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              Set(outer.keys) == ["error"],
              let error = outer["error"] as? [String: Any],
              Set(error.keys) == ["code", "message", "details"],
              error["code"] as? String == "conflict",
              error["message"] as? String
                == "schedule stream cursor is ahead of authoritative state",
              let details = error["details"] as? [String: Any],
              Set(details.keys) == ["cursor_revision", "head_revision"],
              let cursor = strictUnsignedJSONInteger(details["cursor_revision"]),
              let head = strictUnsignedJSONInteger(details["head_revision"]),
              cursor == requestedRevision,
              head < cursor,
              head <= UInt64(Int64.max) else { return nil }
        return head
    }

    private static func strictUnsignedJSONInteger(_ value: Any?) -> UInt64? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              !CFNumberIsFloatType(number),
              let parsed = UInt64(number.stringValue),
              parsed <= UInt64(Int64.max),
              number.stringValue == String(parsed) else { return nil }
        return parsed
    }

    private static func isValidCurrentScheduleResponse(
        contentType: String?,
        cacheControl: String?,
        pragma: String?,
        etag: String?,
        body: Data
    ) -> Bool {
        guard body.count <= Self.maximumResponseBytes,
              isStrictJSONMediaType(contentType),
              isNoStoreZeroAge(cacheControl),
              pragma?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                == "no-cache",
              StrictJSONObjectKeyScanner.hasUniqueKeys(in: body),
              let outer = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              exactKeys(outer, required: ["revision", "schedule"]),
              let revision = outer["revision"] as? [String: Any],
              exactKeys(revision, required: [
                  "id", "revision", "revision_number", "input_digest",
                  "horizon_start", "horizon_end", "timezone_name", "published_at",
              ]),
              let revisionLabel = revision["revision"] as? String,
              let timezoneName = revision["timezone_name"] as? String,
              DayWeaveCanonicalItemDraft.supportedTimeZone(identifier: timezoneName) != nil,
              etag == "\"\(revisionLabel)\"",
              let schedule = outer["schedule"] as? [String: Any],
              exactKeys(
                  schedule,
                  required: [
                      "input_digest", "source_item_count", "accepted_item_count",
                      "source_item_revisions", "rejected_items",
                      "ignored_previous_assignments", "plan",
                  ],
                  optional: ["manual_placement_assessments"]
              ),
              schedule["source_item_revisions"] is [String: Any],
              exactArrayObjects(
                  schedule["rejected_items"],
                  required: ["item_id", "is_sensitive", "title", "reason"]
              ),
              exactArrayObjects(
                  schedule["ignored_previous_assignments"],
                  required: ["item_id", "requested_revision", "current_revision", "reason"]
              ),
              let plan = schedule["plan"] as? [String: Any],
              exactKeys(plan, required: [
                  "as_of", "horizon_start", "horizon_end", "blocks", "unscheduled",
                  "decisions", "violations", "score", "occurrences",
              ]),
              exactArrayObjects(
                  plan["blocks"],
                  required: [
                      "id", "is_sensitive", "item_id", "occurrence_id",
                      "external_block_id", "title", "start", "end", "session_index",
                      "kind", "explanations",
                  ]
              ),
              exactArrayObjects(
                  plan["unscheduled"],
                  required: ["item_id", "occurrence_id", "remaining", "reason", "message"]
              ),
              exactArrayObjects(
                  plan["occurrences"],
                  required: [
                      "id", "series_item_id", "identity", "nominal_start", "nominal_end",
                      "window_start", "window_end", "local_date", "ordinal", "state",
                  ]
              ),
              let score = plan["score"] as? [String: Any],
              exactKeys(score, required: [
                  "scheduled_minutes", "unscheduled_minutes", "soft_penalty", "moved_minutes",
              ]),
              let references = strictCurrentScheduleReferences(schedule: schedule, plan: plan),
              strictRejectedItems(schedule["rejected_items"], references: references),
              strictIgnoredPreviousAssignments(
                  schedule["ignored_previous_assignments"],
                  references: references
              ),
              strictUnscheduledWork(plan["unscheduled"], references: references),
              strictPlanDecisions(plan["decisions"], references: references),
              strictPlanViolations(plan["violations"], references: references),
              strictManualPlacementAssessments(
                  schedule["manual_placement_assessments"],
                  references: references
              ),
              strictBlockExplanations(plan["blocks"]) else { return false }
        return true
    }

    private static func isNoStoreZeroAge(_ value: String?) -> Bool {
        guard let value else { return false }
        return value.split(separator: ",", omittingEmptySubsequences: false).map {
            $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        } == ["no-store", "max-age=0"]
    }

    private static func isNoStoreNoCache(_ value: String?) -> Bool {
        guard let value else { return false }
        return value.split(separator: ",", omittingEmptySubsequences: false).map {
            $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        } == ["no-store", "no-cache"]
    }

    private static func exactKeys(
        _ object: [String: Any],
        required: Set<String>,
        optional: Set<String> = []
    ) -> Bool {
        let keys = Set(object.keys)
        return required.isSubset(of: keys) && keys.isSubset(of: required.union(optional))
    }

    private static func exactArrayObjects(
        _ value: Any?,
        required: Set<String>,
        optional: Set<String> = []
    ) -> Bool {
        guard let values = value as? [Any] else { return false }
        return values.allSatisfy {
            guard let object = $0 as? [String: Any] else { return false }
            return exactKeys(object, required: required, optional: optional)
        }
    }

    private static func strictBlockExplanations(_ value: Any?) -> Bool {
        let codes: Set<String> = [
            "fixed_event", "pinned", "hard_deadline", "goal_progress", "habit_or_routine",
            "priority", "preferred_window", "context_match", "energy_match", "dependency",
            "stable_time", "earliest_available", "split_session",
        ]
        guard let blocks = value as? [Any] else { return false }
        return blocks.allSatisfy { rawBlock in
            guard let block = rawBlock as? [String: Any],
                  let explanations = block["explanations"] as? [Any],
                  explanations.count <= 64 else { return false }
            return explanations.allSatisfy { rawExplanation in
                guard let explanation = rawExplanation as? [String: Any],
                      exactKeys(explanation, required: ["code", "message"]),
                      let code = explanation["code"] as? String,
                      codes.contains(code) else { return false }
                return strictNonemptyText(explanation["message"], maximumScalars: 4_000)
            }
        }
    }

    private struct CurrentScheduleReferences {
        let itemIDs: Set<UUID>
        let occurrenceItems: [UUID: UUID]
    }

    private static func strictCurrentScheduleReferences(
        schedule: [String: Any],
        plan: [String: Any]
    ) -> CurrentScheduleReferences? {
        guard let rawRevisions = schedule["source_item_revisions"] as? [String: Any],
              let rawOccurrences = plan["occurrences"] as? [Any] else { return nil }
        var itemIDs = Set<UUID>()
        for (rawID, rawRevision) in rawRevisions {
            guard let id = strictCanonicalUUID(rawID),
                  let revision = strictUnsignedJSONInteger(rawRevision),
                  revision > 0,
                  itemIDs.insert(id).inserted else { return nil }
        }
        var occurrenceItems: [UUID: UUID] = [:]
        for rawOccurrence in rawOccurrences {
            guard let occurrence = rawOccurrence as? [String: Any],
                  let rawID = occurrence["id"] as? String,
                  let id = strictCanonicalUUID(rawID),
                  let rawSeriesID = occurrence["series_item_id"] as? String,
                  let seriesID = strictCanonicalUUID(rawSeriesID),
                  itemIDs.contains(seriesID),
                  occurrenceItems.updateValue(seriesID, forKey: id) == nil else { return nil }
        }
        return .init(itemIDs: itemIDs, occurrenceItems: occurrenceItems)
    }

    private static func strictRejectedItems(
        _ value: Any?,
        references: CurrentScheduleReferences
    ) -> Bool {
        guard let values = value as? [Any] else { return false }
        var itemIDs = Set<UUID>()
        return values.allSatisfy { rawItem in
            guard let item = rawItem as? [String: Any],
                  let rawItemID = item["item_id"] as? String,
                  let itemID = strictCanonicalUUID(rawItemID),
                  references.itemIDs.contains(itemID),
                  itemIDs.insert(itemID).inserted,
                  strictJSONBoolean(item["is_sensitive"]) != nil,
                  strictNonemptyText(item["title"], maximumScalars: 500),
                  strictNonemptyText(item["reason"], maximumScalars: 4_000)
            else { return false }
            return true
        }
    }

    private static func strictIgnoredPreviousAssignments(
        _ value: Any?,
        references: CurrentScheduleReferences
    ) -> Bool {
        guard let values = value as? [Any] else { return false }
        // The wire form intentionally omits occurrence_id. Repeated item IDs
        // can therefore represent distinct ignored recurrence assignments.
        return values.allSatisfy { rawAssignment in
            guard let assignment = rawAssignment as? [String: Any],
                  let rawItemID = assignment["item_id"] as? String,
                  let itemID = strictCanonicalUUID(rawItemID),
                  references.itemIDs.contains(itemID),
                  let requested = strictUnsignedJSONInteger(assignment["requested_revision"]),
                  requested > 0,
                  let current = strictOptionalUnsignedJSONInteger(
                      assignment["current_revision"]
                  ),
                  current.value.map({ $0 > 0 }) ?? true,
                  strictNonemptyText(assignment["reason"], maximumScalars: 4_000)
            else { return false }
            return true
        }
    }

    private static func strictUnscheduledWork(
        _ value: Any?,
        references: CurrentScheduleReferences
    ) -> Bool {
        let reasons: Set<String> = [
            "missing_duration", "no_capacity", "hard_constraint", "blocked",
            "dependency_unavailable", "dependency_cycle", "session_limit",
        ]
        guard let values = value as? [Any] else { return false }
        var identities = Set<String>()
        return values.allSatisfy { rawWork in
            guard let work = rawWork as? [String: Any],
                  let rawItemID = work["item_id"] as? String,
                  let itemID = strictCanonicalUUID(rawItemID),
                  references.itemIDs.contains(itemID),
                  let occurrence = strictOptionalCanonicalUUID(work["occurrence_id"]),
                  occurrence.value.map({ references.occurrenceItems[$0] != nil }) ?? true,
                  strictFullUnsignedJSONInteger(work["remaining"]) != nil,
                  let reason = work["reason"] as? String,
                  reasons.contains(reason),
                  strictNonemptyText(work["message"], maximumScalars: 4_000)
            else { return false }
            let identity = "\(rawItemID):\(occurrence.value?.uuidString.lowercased() ?? "-")"
            return identities.insert(identity).inserted
        }
    }

    private static func strictPlanDecisions(
        _ value: Any?,
        references: CurrentScheduleReferences
    ) -> Bool {
        let kinds: Set<String> = [
            "container_rolled_up", "terminal_item_ignored", "fixed_event_retained",
            "scheduled", "partially_scheduled", "kept_pinned",
        ]
        guard let decisions = value as? [Any] else { return false }
        return decisions.allSatisfy { rawDecision in
            guard let decision = rawDecision as? [String: Any],
                  exactKeys(
                      decision,
                      required: ["item_id", "occurrence_id", "kind", "message"]
                  ),
                  let rawItemID = decision["item_id"] as? String,
                  let itemID = strictCanonicalUUID(rawItemID),
                  references.itemIDs.contains(itemID),
                  let kind = decision["kind"] as? String,
                  kinds.contains(kind),
                  strictNonemptyText(decision["message"], maximumScalars: 4_000),
                  let occurrence = strictOptionalCanonicalUUID(decision["occurrence_id"])
            else { return false }
            return occurrence.value.map { references.occurrenceItems[$0] != nil } ?? true
        }
    }

    private static func strictPlanViolations(
        _ value: Any?,
        references: CurrentScheduleReferences
    ) -> Bool {
        let kinds: Set<String> = [
            "soft_constraint", "fixed_overlap", "pinned_conflict", "deadline_risk",
            "dependency", "buffer_compressed", "capacity",
        ]
        let severities: Set<String> = ["warning", "error"]
        guard let violations = value as? [Any] else { return false }
        return violations.allSatisfy { rawViolation in
            guard let violation = rawViolation as? [String: Any],
                  exactKeys(violation, required: [
                      "kind", "severity", "item_ids", "occurrence_ids", "start", "end",
                      "penalty", "message",
                  ]),
                  let kind = violation["kind"] as? String,
                  kinds.contains(kind),
                  let severity = violation["severity"] as? String,
                  severities.contains(severity),
                  let itemIDs = strictCanonicalUUIDArray(violation["item_ids"]),
                  itemIDs.allSatisfy(references.itemIDs.contains),
                  let occurrenceIDs = strictCanonicalUUIDArray(violation["occurrence_ids"]),
                  occurrenceIDs.allSatisfy({ references.occurrenceItems[$0] != nil }),
                  strictFullUnsignedJSONInteger(violation["penalty"]) != nil,
                  strictNonemptyText(violation["message"], maximumScalars: 2_000),
                  let start = strictOptionalTimestamp(violation["start"]),
                  let end = strictOptionalTimestamp(violation["end"])
            else { return false }
            switch (start.value, end.value) {
            case (nil, nil):
                return true
            case let (.some(start), .some(end)):
                return start < end
            default:
                return false
            }
        }
    }

    private static func strictManualPlacementAssessments(
        _ value: Any?,
        references: CurrentScheduleReferences
    ) -> Bool {
        guard let value else { return true }
        guard let assessments = value as? [Any],
              !assessments.isEmpty,
              assessments.count <= 64 else { return false }
        var placementIDs = Set<UUID>()
        var totalViolations = 0
        var totalConflictFacts = 0
        return assessments.allSatisfy { rawAssessment in
            guard let assessment = rawAssessment as? [String: Any],
                  exactKeys(assessment, required: [
                      "placement_id", "environment_digest", "approval_digest",
                      "approval_required", "violations",
                  ]),
                  let rawPlacementID = assessment["placement_id"] as? String,
                  let placementID = strictCanonicalUUID(rawPlacementID),
                  placementIDs.insert(placementID).inserted,
                  strictSHA256Digest(assessment["environment_digest"]),
                  strictSHA256Digest(assessment["approval_digest"]),
                  let approvalRequired = strictJSONBoolean(assessment["approval_required"]),
                  let violations = assessment["violations"] as? [Any],
                  !approvalRequired || !violations.isEmpty,
                  totalViolations <= 4_096 - violations.count else { return false }
            totalViolations += violations.count
            return violations.allSatisfy { rawViolation in
                guard let violation = rawViolation as? [String: Any],
                      exactKeys(violation, required: [
                          "code", "item_ids", "occurrence_ids", "conflicting_block_ids",
                          "conflicting_blocks", "start", "end", "boundary_start",
                          "boundary_end", "message",
                      ]),
                      let conflicts = violation["conflicting_blocks"] as? [Any],
                      totalConflictFacts <= 4_096 - conflicts.count,
                      strictManualPlacementViolation(violation, references: references)
                else { return false }
                totalConflictFacts += conflicts.count
                return true
            }
        }
    }

    private static func strictManualPlacementViolation(
        _ violation: [String: Any],
        references: CurrentScheduleReferences
    ) -> Bool {
        let codes: Set<String> = [
            "outside_availability", "earliest_start", "latest_finish", "minimum_notice",
            "allowed_weekday", "preferred_daily_window", "preferred_absolute_window",
            "forbidden_window", "required_context", "required_location",
            "required_capabilities", "energy", "dependency", "maximum_daily_work",
            "maximum_weekly_work", "buffer_compressed", "immutable_overlap",
        ]
        guard let code = violation["code"] as? String,
              codes.contains(code),
              let itemIDs = strictCanonicalUUIDArray(violation["item_ids"]),
              itemIDs.allSatisfy(references.itemIDs.contains),
              let occurrenceIDs = strictCanonicalUUIDArray(violation["occurrence_ids"]),
              occurrenceIDs.allSatisfy({ references.occurrenceItems[$0] != nil }),
              let conflictingBlockIDs = strictCanonicalUUIDArray(
                  violation["conflicting_block_ids"]
              ),
              let start = strictTimestamp(violation["start"]),
              let end = strictTimestamp(violation["end"]),
              start < end,
              strictOptionalTimestamp(violation["boundary_start"]) != nil,
              strictOptionalTimestamp(violation["boundary_end"]) != nil,
              strictNonemptyText(violation["message"], maximumScalars: 4_000),
              let rawConflicts = violation["conflicting_blocks"] as? [Any] else { return false }
        var conflictIDs = Set<UUID>()
        for rawConflict in rawConflicts {
            guard let conflict = rawConflict as? [String: Any],
                  exactKeys(conflict, required: [
                      "block_id", "item_id", "occurrence_id", "external_block_id", "kind",
                      "start", "end",
                  ]),
                  let rawBlockID = conflict["block_id"] as? String,
                  let blockID = strictCanonicalUUID(rawBlockID),
                  conflictIDs.insert(blockID).inserted,
                  let item = strictOptionalCanonicalUUID(conflict["item_id"]),
                  let occurrence = strictOptionalCanonicalUUID(conflict["occurrence_id"]),
                  let external = strictOptionalCanonicalUUID(conflict["external_block_id"]),
                  let kind = conflict["kind"] as? String,
                  ["planned", "pinned", "calendar_event", "external_fixed"].contains(kind),
                  let conflictStart = strictTimestamp(conflict["start"]),
                  let conflictEnd = strictTimestamp(conflict["end"]),
                  conflictStart < conflictEnd else { return false }
            if kind == "external_fixed" {
                guard item.value == nil,
                      occurrence.value == nil,
                      external.value == blockID else {
                    return false
                }
            } else {
                guard external.value == nil,
                      let itemID = item.value,
                      references.itemIDs.contains(itemID),
                      occurrence.value.map({ references.occurrenceItems[$0] != nil }) ?? true
                else { return false }
            }
        }
        return conflictIDs == Set(conflictingBlockIDs)
    }

    private static func strictCanonicalUUIDArray(_ value: Any?) -> [UUID]? {
        guard let values = value as? [Any] else { return nil }
        var result: [UUID] = []
        var unique = Set<UUID>()
        result.reserveCapacity(values.count)
        for value in values {
            guard let raw = value as? String,
                  let id = strictCanonicalUUID(raw),
                  unique.insert(id).inserted else { return nil }
            result.append(id)
        }
        return result
    }

    private static func strictCanonicalUUID(_ value: String) -> UUID? {
        guard value != "00000000-0000-0000-0000-000000000000",
              let id = UUID(uuidString: value),
              id.uuidString.lowercased() == value else { return nil }
        return id
    }

    private static func strictOptionalCanonicalUUID(
        _ value: Any?
    ) -> (value: UUID?, valid: Bool)? {
        guard let value else { return nil }
        if value is NSNull { return (nil, true) }
        guard let raw = value as? String,
              let id = strictCanonicalUUID(raw) else { return nil }
        return (id, true)
    }

    private static func strictTimestamp(_ value: Any?) -> Date? {
        guard let raw = value as? String else { return nil }
        return parseDate(raw)
    }

    private static func strictOptionalTimestamp(
        _ value: Any?
    ) -> (value: Date?, valid: Bool)? {
        guard let value else { return nil }
        if value is NSNull { return (nil, true) }
        guard let date = strictTimestamp(value) else { return nil }
        return (date, true)
    }

    private static func strictOptionalUnsignedJSONInteger(
        _ value: Any?
    ) -> (value: UInt64?, valid: Bool)? {
        guard let value else { return nil }
        if value is NSNull { return (nil, true) }
        guard let parsed = strictUnsignedJSONInteger(value) else { return nil }
        return (parsed, true)
    }

    /// Plan evidence is modeled as Rust `u64` and is not constrained by the
    /// signed PostgreSQL revision domain used by cursors and entity revisions.
    private static func strictFullUnsignedJSONInteger(_ value: Any?) -> UInt64? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              let parsed = UInt64(number.stringValue),
              number.stringValue == String(parsed) else { return nil }
        return parsed
    }

    private static func strictSHA256Digest(_ value: Any?) -> Bool {
        guard let value = value as? String,
              value.utf8.count == 71,
              value.hasPrefix("sha256:") else { return false }
        return value.utf8.dropFirst(7).allSatisfy {
            ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102)
        }
    }

    private static func strictJSONBoolean(_ value: Any?) -> Bool? {
        guard let value = value as? NSNumber,
              CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
        return value.boolValue
    }

    private static func strictNonemptyText(
        _ value: Any?,
        maximumScalars: Int = .max
    ) -> Bool {
        guard let value = value as? String,
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              value.unicodeScalars.count <= maximumScalars else { return false }
        return !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
    }

    private static func isStrictJSONMediaType(_ value: String?) -> Bool {
        guard let value else { return false }
        let components = value.split(separator: ";", omittingEmptySubsequences: false)
        guard let mediaType = components.first?.trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased(), mediaType == "application/json" else {
            return false
        }
        switch components.count {
        case 1:
            return true
        case 2:
            let parameter = components[1].split(
                separator: "=",
                maxSplits: 1,
                omittingEmptySubsequences: false
            )
            return parameter.count == 2
                && parameter[0].trimmingCharacters(in: .whitespacesAndNewlines)
                    .lowercased() == "charset"
                && parameter[1].trimmingCharacters(in: .whitespacesAndNewlines)
                    .lowercased() == "utf-8"
        default:
            return false
        }
    }

    private func validatePostResponseBinding(_ initialBinding: String) throws {
        guard let authCoordinator else { return }
        do {
            let current = try authCoordinator.bindingIdentifier(boundTo: baseURL)
            guard current == initialBinding, current == expectedBindingIdentifier else {
                throw DayWeaveAPIError.durableAuthentication(.concurrentStateChange)
            }
        } catch let error as DayWeaveAPIError {
            throw error
        } catch let error as DurableAuthError {
            throw DayWeaveAPIError.durableAuthentication(error)
        } catch {
            throw DayWeaveAPIError.durableAuthentication(.localStateUnavailable)
        }
    }

    private func perform(
        _ pristineRequest: URLRequest,
        bearer: String
    ) async throws -> (data: Data, response: HTTPURLResponse) {
        var request = pristineRequest
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        do {
            let (bytes, receivedResponse) = try await session.bytes(
                for: request,
                delegate: RejectRedirectDelegate.shared
            )
            guard let httpResponse = receivedResponse as? HTTPURLResponse else {
                throw DayWeaveAPIError.nonHTTPResponse
            }
            if receivedResponse.expectedContentLength > Int64(Self.maximumResponseBytes) {
                bytes.task.cancel()
                throw DayWeaveAPIError.responseTooLarge(limitBytes: Self.maximumResponseBytes)
            }
            var boundedData = Data()
            if receivedResponse.expectedContentLength > 0 {
                boundedData.reserveCapacity(Int(receivedResponse.expectedContentLength))
            }
            for try await byte in bytes {
                guard boundedData.count < Self.maximumResponseBytes else {
                    bytes.task.cancel()
                    throw DayWeaveAPIError.responseTooLarge(limitBytes: Self.maximumResponseBytes)
                }
                boundedData.append(byte)
            }
            return (boundedData, httpResponse)
        } catch let error as DayWeaveAPIError {
            throw error
        } catch let error as URLError {
            throw DayWeaveAPIError.transport(error.code)
        } catch {
            throw DayWeaveAPIError.transport(.unknown)
        }
    }

    private func performExecutionInvalidationStreamRequest(
        _ pristineRequest: URLRequest,
        bearer: String,
        bindingIdentifier: String,
        initialRevision: UInt64,
        expectedEventName: String = "execution-invalidation",
        requiresScheduleHeaders: Bool = false,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamAttemptResult {
        var request = pristineRequest
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        let cancellation = DayWeaveURLSessionTaskCancellationBox()
        return try await withTaskCancellationHandler {
            do {
                let (bytes, receivedResponse) = try await session.bytes(
                    for: request,
                    delegate: RejectRedirectDelegate.shared
                )
                cancellation.install(bytes.task)
                defer {
                    bytes.task.cancel()
                    cancellation.clear(bytes.task)
                }
                guard let response = receivedResponse as? HTTPURLResponse else {
                    throw DayWeaveAPIError.nonHTTPResponse
                }
                guard response.statusCode == 200 else {
                    if response.statusCode == 404 {
                        // Absence is the complete activation-scoped signal; no
                        // response body is trusted or needed to classify it.
                        return .http(response, Data())
                    }
                    let maximumErrorBytes = 8 * 1_024
                    if receivedResponse.expectedContentLength > Int64(maximumErrorBytes) {
                        throw DayWeaveAPIError.responseTooLarge(limitBytes: maximumErrorBytes)
                    }
                    var body = Data()
                    if receivedResponse.expectedContentLength > 0 {
                        body.reserveCapacity(Int(receivedResponse.expectedContentLength))
                    }
                    for try await byte in bytes {
                        guard body.count < maximumErrorBytes else {
                            throw DayWeaveAPIError.responseTooLarge(limitBytes: maximumErrorBytes)
                        }
                        body.append(byte)
                    }
                    return .http(response, body)
                }
                guard Self.isStrictEventStreamMediaType(
                    response.value(forHTTPHeaderField: "content-type")
                ) else {
                    throw DayWeaveExecutionStreamProtocolError.invalidContentType
                }
                guard Self.isStrictIdentityContentEncoding(
                    response.value(forHTTPHeaderField: "content-encoding")
                ) else {
                    throw DayWeaveExecutionStreamProtocolError.invalidContentEncoding
                }
                if requiresScheduleHeaders {
                    guard Self.isNoStoreNoCache(
                        response.value(forHTTPHeaderField: "cache-control")
                    ) else {
                        throw DayWeaveExecutionStreamProtocolError.invalidCacheControl
                    }
                    guard response.value(forHTTPHeaderField: "pragma")?
                        .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                        == "no-cache" else {
                        throw DayWeaveExecutionStreamProtocolError.invalidPragma
                    }
                    guard response.value(forHTTPHeaderField: "x-accel-buffering")?
                        .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                        == "no" else {
                        throw DayWeaveExecutionStreamProtocolError.invalidBufferingPolicy
                    }
                }
                try validatePostResponseBinding(bindingIdentifier)

                var parser = DayWeaveExecutionSSEParser(
                    after: initialRevision,
                    expectedEventName: expectedEventName
                )
                for try await byte in bytes {
                    try Task.checkCancellation()
                    if let revision = try parser.consume(byte) {
                        // The auth binding may be replaced while a long-lived
                        // response is open. Revalidate before allowing its hint
                        // to cross the transport boundary.
                        try validatePostResponseBinding(bindingIdentifier)
                        await receive(revision)
                    }
                }
                try parser.finish()
                try validatePostResponseBinding(bindingIdentifier)
                return .endOfStream(wasLive: parser.hasObservedLiveness)
            } catch let error as DayWeaveAPIError {
                throw error
            } catch let error as DayWeaveExecutionStreamProtocolError {
                throw error
            } catch is CancellationError {
                throw DayWeaveAPIError.transport(.cancelled)
            } catch let error as URLError {
                throw DayWeaveAPIError.transport(error.code)
            } catch {
                throw DayWeaveAPIError.transport(Task.isCancelled ? .cancelled : .unknown)
            }
        } onCancel: {
            cancellation.cancel()
        }
    }

    private func performItemInvalidationStreamRequest(
        _ pristineRequest: URLRequest,
        bearer: String,
        bindingIdentifier: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamAttemptResult {
        var request = pristineRequest
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        let cancellation = DayWeaveURLSessionTaskCancellationBox()
        return try await withTaskCancellationHandler {
            do {
                let (bytes, receivedResponse) = try await session.bytes(
                    for: request,
                    delegate: RejectRedirectDelegate.shared
                )
                cancellation.install(bytes.task)
                defer {
                    bytes.task.cancel()
                    cancellation.clear(bytes.task)
                }
                guard let response = receivedResponse as? HTTPURLResponse else {
                    throw DayWeaveAPIError.nonHTTPResponse
                }
                guard response.statusCode == 200 else {
                    if response.statusCode == 404 {
                        return .http(response, Data())
                    }
                    let maximumErrorBytes = 8 * 1_024
                    if receivedResponse.expectedContentLength > Int64(maximumErrorBytes) {
                        throw DayWeaveAPIError.responseTooLarge(limitBytes: maximumErrorBytes)
                    }
                    var body = Data()
                    if receivedResponse.expectedContentLength > 0 {
                        body.reserveCapacity(Int(receivedResponse.expectedContentLength))
                    }
                    for try await byte in bytes {
                        guard body.count < maximumErrorBytes else {
                            throw DayWeaveAPIError.responseTooLarge(limitBytes: maximumErrorBytes)
                        }
                        body.append(byte)
                    }
                    return .http(response, body)
                }
                guard Self.isStrictEventStreamMediaType(
                    response.value(forHTTPHeaderField: "content-type")
                ) else {
                    throw DayWeaveItemStreamProtocolError.invalidContentType
                }
                guard Self.isStrictIdentityContentEncoding(
                    response.value(forHTTPHeaderField: "content-encoding")
                ) else {
                    throw DayWeaveItemStreamProtocolError.invalidContentEncoding
                }
                try validatePostResponseBinding(bindingIdentifier)

                var parser = DayWeaveItemSSEParser()
                for try await byte in bytes {
                    try Task.checkCancellation()
                    if let cursor = try parser.consume(byte) {
                        try validatePostResponseBinding(bindingIdentifier)
                        await receive(cursor)
                    }
                }
                try parser.finish()
                try validatePostResponseBinding(bindingIdentifier)
                return .endOfStream(wasLive: parser.hasObservedLiveness)
            } catch let error as DayWeaveAPIError {
                throw error
            } catch let error as DayWeaveItemStreamProtocolError {
                throw error
            } catch is CancellationError {
                throw DayWeaveAPIError.transport(.cancelled)
            } catch let error as URLError {
                throw DayWeaveAPIError.transport(error.code)
            } catch {
                throw DayWeaveAPIError.transport(Task.isCancelled ? .cancelled : .unknown)
            }
        } onCancel: {
            cancellation.cancel()
        }
    }

    private func performHabitInvalidationStreamRequest(
        _ pristineRequest: URLRequest,
        bearer: String,
        bindingIdentifier: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveHabitStreamAttemptResult {
        var request = pristineRequest
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        let cancellation = DayWeaveURLSessionTaskCancellationBox()
        return try await withTaskCancellationHandler {
            do {
                let (bytes, receivedResponse) = try await session.bytes(
                    for: request,
                    delegate: RejectRedirectDelegate.shared
                )
                cancellation.install(bytes.task)
                defer {
                    bytes.task.cancel()
                    cancellation.clear(bytes.task)
                }
                guard let response = receivedResponse as? HTTPURLResponse else {
                    throw DayWeaveAPIError.nonHTTPResponse
                }
                guard response.statusCode == 200 else {
                    if response.statusCode == 404 {
                        return .http(response, Data())
                    }
                    let maximumErrorBytes = 8 * 1_024
                    if receivedResponse.expectedContentLength > Int64(maximumErrorBytes) {
                        throw DayWeaveAPIError.responseTooLarge(limitBytes: maximumErrorBytes)
                    }
                    var body = Data()
                    if receivedResponse.expectedContentLength > 0 {
                        body.reserveCapacity(Int(receivedResponse.expectedContentLength))
                    }
                    for try await byte in bytes {
                        guard body.count < maximumErrorBytes else {
                            throw DayWeaveAPIError.responseTooLarge(limitBytes: maximumErrorBytes)
                        }
                        body.append(byte)
                    }
                    return .http(response, body)
                }
                guard Self.isStrictEventStreamMediaType(
                    response.value(forHTTPHeaderField: "content-type")
                ) else {
                    throw DayWeaveHabitStreamProtocolError.invalidContentType
                }
                guard Self.isStrictIdentityContentEncoding(
                    response.value(forHTTPHeaderField: "content-encoding")
                ) else {
                    throw DayWeaveHabitStreamProtocolError.invalidContentEncoding
                }
                guard Self.isNoStoreNoCache(
                    response.value(forHTTPHeaderField: "cache-control")
                ) else {
                    throw DayWeaveHabitStreamProtocolError.invalidCacheControl
                }
                guard response.value(forHTTPHeaderField: "pragma")?
                    .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                    == "no-cache" else {
                    throw DayWeaveHabitStreamProtocolError.invalidPragma
                }
                guard response.value(forHTTPHeaderField: "x-accel-buffering")?
                    .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                    == "no" else {
                    throw DayWeaveHabitStreamProtocolError.invalidBufferingPolicy
                }
                try validatePostResponseBinding(bindingIdentifier)

                var parser = DayWeaveHabitSSEParser()
                for try await byte in bytes {
                    try Task.checkCancellation()
                    if let cursor = try parser.consume(byte) {
                        // A durable auth binding may rotate while the response
                        // is open; no hint crosses that boundary unchecked.
                        try validatePostResponseBinding(bindingIdentifier)
                        await receive(cursor)
                    }
                }
                try parser.finish()
                try validatePostResponseBinding(bindingIdentifier)
                return .endOfStream(wasLive: parser.hasObservedLiveness)
            } catch let error as DayWeaveAPIError {
                throw error
            } catch let error as DayWeaveHabitStreamProtocolError {
                throw error
            } catch is CancellationError {
                throw DayWeaveAPIError.transport(.cancelled)
            } catch let error as URLError {
                throw DayWeaveAPIError.transport(error.code)
            } catch {
                throw DayWeaveAPIError.transport(Task.isCancelled ? .cancelled : .unknown)
            }
        } onCancel: {
            cancellation.cancel()
        }
    }

    private static func isStrictEventStreamMediaType(_ value: String?) -> Bool {
        guard let value else { return false }
        let components = value.split(separator: ";", omittingEmptySubsequences: false)
        guard let mediaType = components.first?.trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased(), mediaType == "text/event-stream" else {
            return false
        }
        switch components.count {
        case 1:
            return true
        case 2:
            let parameter = components[1].split(
                separator: "=",
                maxSplits: 1,
                omittingEmptySubsequences: false
            )
            return parameter.count == 2
                && parameter[0].trimmingCharacters(in: .whitespacesAndNewlines)
                    .lowercased() == "charset"
                && parameter[1].trimmingCharacters(in: .whitespacesAndNewlines)
                    .lowercased() == "utf-8"
        default:
            return false
        }
    }

    private static func isStrictIdentityContentEncoding(_ value: String?) -> Bool {
        guard let value else { return true }
        let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return !normalized.contains(",")
            && normalized.caseInsensitiveCompare("identity") == .orderedSame
    }

    private func makeDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = Self.parseDate(value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Expected an RFC 3339 timestamp"
                )
            }
            return date
        }
        return decoder
    }

    private static func parseDate(_ value: String) -> Date? {
        guard let instant = CanonicalRFC3339Instant(value),
              instant.hasPostgresPrecision else { return nil }
        return instant.exactlyRepresentableDate
    }

    private static func format(_ date: Date) throws -> String {
        guard let instant = CanonicalRFC3339Instant(date: date) else {
            throw EncodingError.invalidValue(
                date,
                .init(
                    codingPath: [],
                    debugDescription: "Date is outside the canonical RFC 3339 range"
                )
            )
        }
        return instant.canonicalUTCString
    }

    private static func normalizedHeaders(_ response: HTTPURLResponse) -> [String: String] {
        var result: [String: String] = [:]
        for (key, value) in response.allHeaderFields {
            guard let key = key as? String else { continue }
            result[key.lowercased()] = String(describing: value)
        }
        return result
    }

    private static func staticBindingIdentifier(token: String?) -> String {
        let digest = SHA256.hash(data: Data((token ?? "").utf8))
            .map { String(format: "%02x", $0) }
            .joined()
        return "static-v1:\(digest)"
    }

    private static func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private static func configurationIdentifier(
        baseURL: DayWeaveAPIBaseURL,
        binding: String
    ) -> String {
        "\(baseURL.canonicalConfigurationIdentifier)|auth=\(binding)"
    }
}

extension DayWeaveAPIClient:
    GoogleOutboundTransport,
    GoogleSchedulePublicationTransport,
    DayWeaveExecutionStreamTransport,
    DayWeaveItemStreamTransport,
    DayWeaveScheduleStreamTransport,
    DayWeaveHabitTransport,
    DayWeaveHabitStreamTransport {}

private enum DayWeaveExecutionStreamAttemptResult: Sendable {
    case endOfStream(wasLive: Bool)
    case http(HTTPURLResponse, Data)
}

private enum DayWeaveItemStreamAttemptResult: Sendable {
    case endOfStream(wasLive: Bool)
    case http(HTTPURLResponse, Data)
}

private enum DayWeaveHabitStreamAttemptResult: Sendable {
    case endOfStream(wasLive: Bool)
    case http(HTTPURLResponse, Data)
}

/// Bridges structured-task cancellation to the URLSession task even when the
/// task is canceled while `bytes(for:)` is still establishing the response.
private final class DayWeaveURLSessionTaskCancellationBox: @unchecked Sendable {
    private let lock = NSLock()
    private var task: URLSessionTask?
    private var cancellationRequested = false

    func install(_ task: URLSessionTask) {
        let shouldCancel = lock.withLock {
            self.task = task
            return cancellationRequested
        }
        if shouldCancel { task.cancel() }
    }

    func clear(_ task: URLSessionTask) {
        lock.withLock {
            if self.task === task {
                self.task = nil
            }
        }
    }

    func cancel() {
        let task = lock.withLock {
            cancellationRequested = true
            return self.task
        }
        task?.cancel()
    }
}

/// Foundation's JSON object decoders collapse duplicate member names before a
/// keyed container can inspect them. Destructive trust promotion therefore
/// performs a small duplicate-aware grammar pass over the exact wire bytes
/// before using `JSONSerialization` for typed envelope checks.
struct StrictJSONObjectKeyScanner {
    private static let maximumDepth = 64

    private let bytes: [UInt8]
    private let requiresCanonicalIntegers: Bool
    private var index = 0

    private init(_ data: Data, requiresCanonicalIntegers: Bool) {
        bytes = Array(data)
        self.requiresCanonicalIntegers = requiresCanonicalIntegers
    }

    static func hasUniqueKeys(in data: Data) -> Bool {
        scan(data, requiresCanonicalIntegers: false)
    }

    static func hasUniqueKeysAndCanonicalIntegers(in data: Data) -> Bool {
        scan(data, requiresCanonicalIntegers: true)
    }

    private static func scan(_ data: Data, requiresCanonicalIntegers: Bool) -> Bool {
        var scanner = Self(data, requiresCanonicalIntegers: requiresCanonicalIntegers)
        scanner.skipWhitespace()
        guard scanner.parseValue(depth: 0) else { return false }
        scanner.skipWhitespace()
        return scanner.index == scanner.bytes.count
    }

    private mutating func parseValue(depth: Int) -> Bool {
        guard depth <= Self.maximumDepth, index < bytes.count else { return false }
        switch bytes[index] {
        case 0x7B:
            return parseObject(depth: depth)
        case 0x5B:
            return parseArray(depth: depth)
        case 0x22:
            return parseString() != nil
        case 0x74:
            return consumeLiteral([0x74, 0x72, 0x75, 0x65])
        case 0x66:
            return consumeLiteral([0x66, 0x61, 0x6C, 0x73, 0x65])
        case 0x6E:
            return consumeLiteral([0x6E, 0x75, 0x6C, 0x6C])
        case 0x2D, 0x30...0x39:
            return parseNumber()
        default:
            return false
        }
    }

    private mutating func parseObject(depth: Int) -> Bool {
        index += 1
        skipWhitespace()
        if consume(0x7D) { return true }
        var keys = Set<String>()
        while true {
            guard index < bytes.count, bytes[index] == 0x22,
                  let key = parseString(), keys.insert(key).inserted else { return false }
            skipWhitespace()
            guard consume(0x3A) else { return false }
            skipWhitespace()
            guard parseValue(depth: depth + 1) else { return false }
            skipWhitespace()
            if consume(0x7D) { return true }
            guard consume(0x2C) else { return false }
            skipWhitespace()
        }
    }

    private mutating func parseArray(depth: Int) -> Bool {
        index += 1
        skipWhitespace()
        if consume(0x5D) { return true }
        while true {
            guard parseValue(depth: depth + 1) else { return false }
            skipWhitespace()
            if consume(0x5D) { return true }
            guard consume(0x2C) else { return false }
            skipWhitespace()
        }
    }

    private mutating func parseString() -> String? {
        guard consume(0x22) else { return nil }
        let start = index - 1
        while index < bytes.count {
            let byte = bytes[index]
            if byte == 0x22 {
                index += 1
                return try? JSONDecoder().decode(
                    String.self,
                    from: Data(bytes[start..<index])
                )
            }
            if byte < 0x20 { return nil }
            if byte == 0x5C {
                index += 1
                guard index < bytes.count else { return nil }
                let escape = bytes[index]
                if escape == 0x75 {
                    guard index + 4 < bytes.count,
                          bytes[(index + 1)...(index + 4)].allSatisfy(Self.isHexDigit) else {
                        return nil
                    }
                    index += 5
                    continue
                }
                guard [0x22, 0x5C, 0x2F, 0x62, 0x66, 0x6E, 0x72, 0x74].contains(escape) else {
                    return nil
                }
            }
            index += 1
        }
        return nil
    }

    private mutating func parseNumber() -> Bool {
        let isNegative = consume(0x2D)
        guard index < bytes.count else { return false }
        let isZero: Bool
        if consume(0x30) {
            isZero = true
            if index < bytes.count, Self.isDigit(bytes[index]) { return false }
        } else {
            isZero = false
            guard index < bytes.count, (0x31...0x39).contains(bytes[index]) else { return false }
            repeat { index += 1 } while index < bytes.count && Self.isDigit(bytes[index])
        }
        if requiresCanonicalIntegers {
            guard !(isNegative && isZero),
                  index >= bytes.count
                    || (bytes[index] != 0x2E && bytes[index] != 0x65 && bytes[index] != 0x45)
            else { return false }
            return true
        }
        if consume(0x2E) {
            guard index < bytes.count, Self.isDigit(bytes[index]) else { return false }
            repeat { index += 1 } while index < bytes.count && Self.isDigit(bytes[index])
        }
        if index < bytes.count, bytes[index] == 0x65 || bytes[index] == 0x45 {
            index += 1
            if index < bytes.count, bytes[index] == 0x2B || bytes[index] == 0x2D {
                index += 1
            }
            guard index < bytes.count, Self.isDigit(bytes[index]) else { return false }
            repeat { index += 1 } while index < bytes.count && Self.isDigit(bytes[index])
        }
        return true
    }

    private mutating func consumeLiteral(_ literal: [UInt8]) -> Bool {
        guard index + literal.count <= bytes.count,
              Array(bytes[index..<(index + literal.count)]) == literal else { return false }
        index += literal.count
        return true
    }

    private mutating func consume(_ byte: UInt8) -> Bool {
        guard index < bytes.count, bytes[index] == byte else { return false }
        index += 1
        return true
    }

    private mutating func skipWhitespace() {
        while index < bytes.count, [0x20, 0x09, 0x0A, 0x0D].contains(bytes[index]) {
            index += 1
        }
    }

    private static func isDigit(_ byte: UInt8) -> Bool {
        (0x30...0x39).contains(byte)
    }

    private static func isHexDigit(_ byte: UInt8) -> Bool {
        isDigit(byte) || (0x41...0x46).contains(byte) || (0x61...0x66).contains(byte)
    }
}

private final class RejectRedirectDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    static let shared = RejectRedirectDelegate()

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
