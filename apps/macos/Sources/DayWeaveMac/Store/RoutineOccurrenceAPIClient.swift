import Foundation

protocol RoutineOccurrenceTransport: Sendable {
    var configurationIdentifier: String { get }
    func routineOccurrence(instanceID: UUID) async throws -> RoutineOccurrenceSnapshot
    func routineOccurrences(cursor: String?, limit: Int) async throws -> RoutineOccurrencePage
    func routineOccurrenceDelta(cursor: String?, limit: Int) async throws -> RoutineOccurrencePage
    func putRoutineOccurrenceMember(instanceID: UUID, memberID: UUID, requestBody: Data) async throws -> RoutineOccurrenceMutation
}

extension DayWeaveAPIClient: RoutineOccurrenceTransport {
    func routineOccurrence(instanceID: UUID) async throws -> RoutineOccurrenceSnapshot {
        try RoutineOccurrenceValidation.require(instanceID != RoutineOccurrenceValidation.nilID)
        let result: RoutineOccurrenceSnapshot = try await sendRoutineOccurrence(method: "GET",
            pathComponents: ["v1", "routine-occurrences", instanceID.uuidString.lowercased()])
        try RoutineOccurrenceValidation.require(result.aggregate.manifest.id == instanceID && result.isValid)
        return result
    }

    func routineOccurrences(cursor: String? = nil, limit: Int = 50) async throws -> RoutineOccurrencePage {
        let page: RoutineOccurrencePage = try await sendRoutineOccurrence(method: "GET",
            pathComponents: ["v1", "routine-occurrences"], queryItems: try occurrenceQuery(cursor: cursor, limit: limit))
        try RoutineOccurrenceValidation.require(page.isCurrentStatePage && page.changes.count <= limit
            && ((page.changes.isEmpty && !page.hasMore) || page.cursor != cursor))
        return page
    }

    func routineOccurrenceDelta(cursor: String? = nil, limit: Int = 50) async throws -> RoutineOccurrencePage {
        let page: RoutineOccurrencePage = try await sendRoutineOccurrence(method: "GET",
            pathComponents: ["v1", "routine-occurrences", "delta"], queryItems: try occurrenceQuery(cursor: cursor, limit: limit))
        try RoutineOccurrenceValidation.require(page.isValid && page.changes.count <= limit
            && ((page.changes.isEmpty && !page.hasMore) || page.cursor != cursor))
        return page
    }

    func putRoutineOccurrenceMember(instanceID: UUID, memberID: UUID, requestBody: Data) async throws -> RoutineOccurrenceMutation {
        try RoutineOccurrenceValidation.require(instanceID != RoutineOccurrenceValidation.nilID
            && requestBody.count <= RoutineOccurrenceValidation.maximumRequestBytes)
        let command = try RoutineOccurrenceValidation.decode(RoutineOccurrenceCommand.self, from: requestBody)
        try RoutineOccurrenceValidation.require(command.isValid(for: memberID))
        let result: RoutineOccurrenceMutation = try await sendRoutineOccurrence(method: "PUT",
            pathComponents: ["v1", "routine-occurrences", instanceID.uuidString.lowercased(), "members", memberID.uuidString.lowercased()], body: requestBody)
        try RoutineOccurrenceValidation.require(result.matches(instanceID: instanceID, memberID: memberID, command: command))
        return result
    }

    private func occurrenceQuery(cursor: String?, limit: Int) throws -> [URLQueryItem] {
        try RoutineOccurrenceValidation.require((1...100).contains(limit) && (cursor.map(RoutineOccurrenceValidation.cursor) ?? true))
        var query = [URLQueryItem(name: "limit", value: String(limit))]
        if let cursor { query.append(.init(name: "cursor", value: cursor)) }
        return query
    }
}

/// Closed private response admission. Shared API execution still owns bound
/// durable-auth refresh, exact request replay, redirects and cancellation.
enum RoutineOccurrenceHTTP {
    static func header(_ name: String, fields: [AnyHashable: Any]) throws -> String? {
        let matches = fields.filter { ($0.key as? String)?.caseInsensitiveCompare(name) == .orderedSame }.map(\.value)
        guard matches.count <= 1 else { throw RoutineOccurrenceError.invalidData }
        guard let value = matches.first else { return nil }
        guard let value = value as? String else { throw RoutineOccurrenceError.invalidData }
        return value
    }

    static func decode<T: Decodable>(_ type: T.Type, method: String, response: HTTPURLResponse, data: Data) throws -> T {
        let fields = response.allHeaderFields
        guard let media = try header("content-type", fields: fields)?.lowercased(),
              media == "application/json" || media == "application/json; charset=utf-8",
              try header("cache-control", fields: fields)?.lowercased() == "no-store, max-age=0",
              data.count <= RoutineOccurrenceValidation.maximumBytes,
              StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: data) else { throw RoutineOccurrenceError.invalidData }
        // Pragma is deliberately not required: this route's no-store contract
        // is sufficient and older intermediaries may omit the legacy header.
        let replay = try header("idempotency-replayed", fields: fields)
        if response.statusCode == 200 {
            if method == "GET" { try RoutineOccurrenceValidation.require(replay == nil) }
            else {
                guard replay == "true" || replay == "false" else { throw RoutineOccurrenceError.invalidData }
                let mutation = try RoutineOccurrenceValidation.decode(RoutineOccurrenceMutation.self, from: data)
                try RoutineOccurrenceValidation.require(mutation.replayed == (replay == "true"))
            }
            return try RoutineOccurrenceValidation.decode(type, from: data)
        }
        try RoutineOccurrenceValidation.require(replay == nil)
        if let envelope = try? JSONDecoder().decode(PrivateErrorEnvelope.self, from: data),
           RoutineOccurrenceValidation.definitiveStatuses[envelope.error.code] == response.statusCode {
            throw RoutineOccurrenceError.definitive(envelope.error.code)
        }
        throw RoutineOccurrenceError.unavailable
    }

    private struct PrivateErrorEnvelope: Decodable {
        let error: PrivateErrorBody
        private enum CodingKeys: String, CodingKey { case error }
        init(from decoder: any Decoder) throws {
            try RoutineOccurrenceValidation.keys(decoder, ["error"])
            error = try decoder.container(keyedBy: CodingKeys.self).decode(PrivateErrorBody.self, forKey: .error)
        }
    }
    private struct PrivateErrorBody: Decodable {
        let code: String
        private enum CodingKeys: String, CodingKey { case code, message }
        init(from decoder: any Decoder) throws {
            try RoutineOccurrenceValidation.keys(decoder, ["code", "message"])
            let c = try decoder.container(keyedBy: CodingKeys.self)
            code = try c.decode(String.self, forKey: .code)
            _ = try c.decode(String.self, forKey: .message)
        }
    }
}
