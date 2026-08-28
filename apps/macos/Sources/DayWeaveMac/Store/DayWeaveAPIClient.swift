import Foundation

enum JSONValue: Codable, Equatable, Sendable {
    case object([String: JSONValue])
    case array([JSONValue])
    case string(String)
    case number(Double)
    case bool(Bool)
    case null

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
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

    init(_ value: String) throws {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { throw DayWeaveAPIBaseURLError.empty }
        guard var components = URLComponents(string: value),
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
        if scheme == "http", !Self.isLoopback(host) {
            throw DayWeaveAPIBaseURLError.insecureRemoteHTTP
        }

        while components.path.count > 1, components.path.hasSuffix("/") {
            components.path.removeLast()
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

    private static func isLoopback(_ host: String) -> Bool {
        host == "localhost" || host == "127.0.0.1" || host == "::1"
    }
}

enum DayWeaveAPIError: Error, Equatable, Sendable {
    case credentialUnavailable
    case requestEncodingFailed
    case invalidEndpoint
    case transport(URLError.Code)
    case nonHTTPResponse
    case server(statusCode: Int, code: String?, message: String?, requestID: String?)
    case responseDecodingFailed
}

extension DayWeaveAPIError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .credentialUnavailable:
            return "The API bearer token is unavailable. Save it again in Settings."
        case .requestEncodingFailed:
            return "DayWeave could not encode the proposal request."
        case .invalidEndpoint:
            return "The configured API URL could not form a suggestions endpoint."
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
        case let .server(statusCode, code, message, requestID):
            var result: String
            if statusCode == 401 {
                result = "The DayWeave API rejected the bearer token. Replace it in Settings."
            } else if statusCode == 409 {
                result = "This proposal changed on the server. Refresh before trying again."
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
            return "The DayWeave API response did not match the supported suggestions contract."
        }
    }
}

struct DayWeaveAPIClient: Sendable {
    private struct SuggestionListEnvelope: Decodable {
        let suggestions: [DayWeaveProposal]
    }

    private struct SuggestionEnvelope: Decodable {
        let suggestion: DayWeaveProposal
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
    private let tokenStore: any BearerTokenStoring

    init(
        baseURL: DayWeaveAPIBaseURL,
        session: URLSession = .shared,
        tokenStore: any BearerTokenStoring
    ) {
        self.baseURL = baseURL
        self.session = session
        self.tokenStore = tokenStore
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
        body: Data? = nil
    ) async throws -> Response {
        let token: String
        do {
            guard let savedToken = try tokenStore.loadToken(), !savedToken.isEmpty else {
                throw DayWeaveAPIError.credentialUnavailable
            }
            token = savedToken
        } catch let error as DayWeaveAPIError {
            throw error
        } catch {
            throw DayWeaveAPIError.credentialUnavailable
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
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if body != nil {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(
                for: request,
                delegate: RejectRedirectDelegate.shared
            )
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
            throw DayWeaveAPIError.server(
                statusCode: httpResponse.statusCode,
                code: envelope?.error.code,
                message: envelope?.error.message.prefix(500).description,
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
