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
    case requestEncodingFailed
    case invalidEndpoint
    case transport(URLError.Code)
    case nonHTTPResponse
    case responseTooLarge(limitBytes: Int)
    case server(statusCode: Int, code: String?, message: String?, requestID: String?)
    case responseDecodingFailed
}

extension DayWeaveAPIError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .credentialUnavailable:
            return "The API bearer token is unavailable. Save it again in Settings."
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
        case let .server(statusCode, code, message, requestID):
            var result: String
            if statusCode == 401 {
                result = "The DayWeave API rejected the bearer token. Replace it in Settings."
            } else if statusCode == 409 {
                result = "This data changed on the server. Refresh before trying again."
            } else if let message, !message.isEmpty {
                result = "DayWeave API error \(statusCode): \(message)"
            } else if let code, !code.isEmpty {
                result = "DayWeave API error \(statusCode) (\(code))."
            } else {
                result = "The DayWeave API returned HTTP \(statusCode)."
            }
            if let requestID, !requestID.isEmpty {
                result += " Request ID: \(requestID)."
            }
            return result
        case .responseDecodingFailed:
            return "The DayWeave API response did not match this app’s supported contract."
        }
    }
}

struct DayWeaveAPIClient: Sendable {
    static let maximumResponseBytes = 16 * 1_048_576
    static let maximumRequestBytes = 16 * 1_048_576
    static let maximumExecutionHistoryLimit = 100

    private struct SuggestionListEnvelope: Decodable {
        let suggestions: [DayWeaveProposal]
    }

    private struct SuggestionEnvelope: Decodable {
        let suggestion: DayWeaveProposal
    }

    private struct ItemEnvelope: Decodable {
        let item: DayWeaveCanonicalItem
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

    private struct DecisionRequest: Encodable {
        let expectedRevision: UInt64
        let note: String?

        private enum CodingKeys: String, CodingKey {
            case expectedRevision = "expected_revision"
            case note
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

    var configurationIdentifier: String { baseURL.canonicalConfigurationIdentifier }

    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = makeDayWeaveEphemeralSession(),
        bearerToken: String?
    ) {
        self.baseURL = baseURL
        self.session = session
        self.bearerToken = bearerToken
    }

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
        guard (1...Self.maximumExecutionHistoryLimit).contains(limit) else {
            throw DayWeaveAPIError.requestEncodingFailed
        }
        let envelope: DayWeaveExecutionHistoryEnvelope = try await send(
            method: "GET",
            pathComponents: ["v1", "execution", "history"],
            queryItems: [URLQueryItem(name: "limit", value: String(limit))]
        )
        guard envelope.sessions.count <= limit,
              Set(envelope.sessions.map(\.id)).count == envelope.sessions.count,
              envelope.sessions.count(where: { $0.status.isOpen }) <= 1,
              zip(envelope.sessions, envelope.sessions.dropFirst()).allSatisfy({ newer, older in
                  newer.updatedAt > older.updatedAt
                      || (newer.updatedAt == older.updatedAt
                          && newer.id.uuidString.lowercased()
                              > older.id.uuidString.lowercased())
              }) else {
            throw DayWeaveAPIError.responseDecodingFailed
        }
        return envelope.sessions
    }

    /// Produces the deterministic body that the caller must durably retain
    /// together with its idempotency key before the first network attempt.
    func encodedExecutionCommand(_ request: DayWeaveExecutionCommandRequest) throws -> Data {
        try encode(request)
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
            persistedRequest = try makeDecoder().decode(
                DayWeaveExecutionCommandRequest.self,
                from: encodedRequest
            )
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

    func previewSchedule(
        _ request: DayWeaveSchedulePreviewRequest
    ) async throws -> DayWeaveSchedulePreview {
        try await send(
            method: "POST",
            pathComponents: ["v1", "schedule", "preview"],
            body: try encode(request)
        )
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
        body: Data? = nil
    ) async throws -> Response {
        guard let token = bearerToken, !token.isEmpty else {
            throw DayWeaveAPIError.credentialUnavailable
        }
        if let body, body.count > Self.maximumRequestBytes {
            throw DayWeaveAPIError.requestEncodingFailed
        }

        let endpoint: URL
        do {
            endpoint = try baseURL.endpoint(pathComponents: pathComponents, queryItems: queryItems)
        } catch {
            throw DayWeaveAPIError.invalidEndpoint
        }

        var request = URLRequest(url: endpoint)
        request.httpMethod = method
        request.httpBody = body
        request.timeoutInterval = 20
        request.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        request.setValue("no-cache", forHTTPHeaderField: "Pragma")
        for (name, value) in headers {
            request.setValue(value, forHTTPHeaderField: name)
        }
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let response: URLResponse
        do {
            let (bytes, receivedResponse) = try await session.bytes(
                for: request,
                delegate: RejectRedirectDelegate.shared
            )
            response = receivedResponse
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
            data = boundedData
        } catch let error as DayWeaveAPIError {
            throw error
        } catch let error as URLError {
            throw DayWeaveAPIError.transport(error.code)
        } catch {
            throw DayWeaveAPIError.transport(.unknown)
        }

        guard let httpResponse = response as? HTTPURLResponse else {
            throw DayWeaveAPIError.nonHTTPResponse
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let envelope = try? makeDecoder().decode(ErrorEnvelope.self, from: data)
            let safeMessage = envelope?.error.message
                .prefix(500)
                .description
                .replacingOccurrences(of: token, with: "[redacted]")
            throw DayWeaveAPIError.server(
                statusCode: httpResponse.statusCode,
                code: envelope?.error.code,
                message: safeMessage,
                requestID: httpResponse.value(forHTTPHeaderField: "x-request-id")
            )
        }

        do {
            return try makeDecoder().decode(Response.self, from: data)
        } catch {
            throw DayWeaveAPIError.responseDecodingFailed
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
