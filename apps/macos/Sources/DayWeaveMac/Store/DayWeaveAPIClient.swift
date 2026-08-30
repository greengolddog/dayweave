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
            #"\bdw_(?:en1|da1|dr1|mc1)_[A-Za-z0-9_-]{20,}\b"#,
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

struct DayWeaveAPIClient: Sendable {
    static let maximumResponseBytes = 16 * 1_048_576
    static let maximumRequestBytes = 16 * 1_048_576
    static let maximumExecutionHistoryLimit = 100
    static let maximumCanonicalItemListLimit = 200

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

    let configurationIdentifier: String

    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = makeDayWeaveEphemeralSession(),
        bearerToken: String?
    ) {
        self.baseURL = baseURL
        self.session = session
        self.bearerToken = bearerToken
        authCoordinator = nil
        let binding = Self.staticBindingIdentifier(token: bearerToken)
        expectedBindingIdentifier = binding
        configurationIdentifier = Self.configurationIdentifier(baseURL: baseURL, binding: binding)
    }

    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = makeDayWeaveEphemeralSession(),
        authCoordinator: DurableAuthCoordinator
    ) {
        self.baseURL = baseURL
        self.session = session
        bearerToken = nil
        self.authCoordinator = authCoordinator
        let binding = (try? authCoordinator.bindingIdentifier(boundTo: baseURL))
            ?? "device-v1-unavailable:\(baseURL.canonicalConfigurationIdentifier)"
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
        guard expectedRevision > 0,
              expectedRevision < UInt64(Int64.max),
              role != .writable,
              calendarPolicy.isReadOnlySafe else {
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
        guard collection.accountID == accountID,
              collection.id == collectionID,
              collection.revision == expectedRevision + 1,
              collection.selected == selected,
              collection.visible == visible,
              collection.syncRole == role,
              collection.calendarPolicy == calendarPolicy,
              role != .blocking || collection.kind == .calendar else {
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

    func previewSchedule(
        _ request: DayWeaveSchedulePreviewRequest
    ) async throws -> DayWeaveSchedulePreview {
        try await send(
            method: "POST",
            pathComponents: ["v1", "schedule", "preview"],
            body: try encode(request)
        )
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
        requiresDurableAuthorization: Bool = false
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

        var tokensToRedact = [initialAuthorization.bearerToken]
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
            return try makeDecoder().decode(Response.self, from: data)
        } catch {
            throw DayWeaveAPIError.responseDecodingFailed
        }
    }

    private enum ProposalApplicationEndpoint: Equatable {
        case lookup
        case apply
        case undo
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
                "an executing or terminal item cannot become a parent": "invalid_parent_state",
            ]
        case .replace:
            [
                "item cannot be its own parent": "self_parent",
                "item hierarchy would contain a cycle": "hierarchy_cycle",
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
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) {
            return date
        }
        let wholeSeconds = ISO8601DateFormatter()
        wholeSeconds.formatOptions = [.withInternetDateTime]
        return wholeSeconds.date(from: value)
    }

    private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
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

/// Foundation's JSON object decoders collapse duplicate member names before a
/// keyed container can inspect them. Destructive trust promotion therefore
/// performs a small duplicate-aware grammar pass over the exact wire bytes
/// before using `JSONSerialization` for typed envelope checks.
private struct StrictJSONObjectKeyScanner {
    private static let maximumDepth = 64

    private let bytes: [UInt8]
    private var index = 0

    private init(_ data: Data) {
        bytes = Array(data)
    }

    static func hasUniqueKeys(in data: Data) -> Bool {
        var scanner = Self(data)
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
        _ = consume(0x2D)
        guard index < bytes.count else { return false }
        if consume(0x30) {
            if index < bytes.count, Self.isDigit(bytes[index]) { return false }
        } else {
            guard index < bytes.count, (0x31...0x39).contains(bytes[index]) else { return false }
            repeat { index += 1 } while index < bytes.count && Self.isDigit(bytes[index])
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
