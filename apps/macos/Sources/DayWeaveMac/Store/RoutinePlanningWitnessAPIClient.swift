import Foundation

protocol RoutinePlanningWitnessTransport: Sendable {
    var configurationIdentifier: String { get }
    func routinePlanningWitness(_ request: RoutinePlanningWitnessRequest) async throws -> RoutinePlanningWitnessResponse
}

extension DayWeaveAPIClient: RoutinePlanningWitnessTransport {
    func routinePlanningWitness(_ request: RoutinePlanningWitnessRequest) async throws -> RoutinePlanningWitnessResponse {
        do {
            try Task.checkCancellation(); try request.validate()
            let body = try RoutinePlanningWitnessValidation.encode(request)
            let response = try await sendRoutinePlanningWitness(requestBody: body)
            try Task.checkCancellation()
            if case let .qualified(witness) = response.result { try witness.requireMatches(request) }
            return response
        } catch is CancellationError { throw CancellationError() }
        catch let error as RoutinePlanningWitnessError { throw error }
        catch {
            // Do not retain decoding snippets, server messages, URLSession
            // causes or credentials in private planning diagnostics.
            throw RoutinePlanningWitnessError.unavailable
        }
    }
}

/// POST is a private read, not an occurrence PUT. It has its own byte budget
/// and must never acquire replay-header or definitive-mutation semantics.
enum RoutinePlanningWitnessHTTP {
    static func decode<T: Decodable>(_ type: T.Type, response: HTTPURLResponse, data: Data) throws -> T {
        do {
            let fields = response.allHeaderFields
            guard let media = try RoutineOccurrenceHTTP.header("content-type", fields: fields)?.lowercased(),
                  media == "application/json" || media == "application/json; charset=utf-8",
                  try RoutineOccurrenceHTTP.header("cache-control", fields: fields)?.lowercased() == "no-store, max-age=0",
                  try RoutineOccurrenceHTTP.header("idempotency-replayed", fields: fields) == nil else {
                throw RoutinePlanningWitnessError.invalidData
            }
            guard data.count <= RoutinePlanningWitnessValidation.maximumBytes else { throw RoutinePlanningWitnessError.tooLarge }
            guard StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: data) else { throw RoutinePlanningWitnessError.invalidData }
            if response.statusCode == 200 {
                return try RoutinePlanningWitnessValidation.decode(type, from: data)
            }
            let envelope = try RoutinePlanningWitnessValidation.decode(ErrorEnvelope.self, from: data)
            switch (response.statusCode, envelope.error.code) {
            case (422, "routine_planning_invalid"), (400, "invalid_json"): throw RoutinePlanningWitnessError.invalidData
            case (413, "routine_planning_too_large"): throw RoutinePlanningWitnessError.tooLarge
            case (409, "routine_planning_source_changed"): throw RoutinePlanningWitnessError.sourceChanged
            case (409, "routine_planning_cursor_changed"): throw RoutinePlanningWitnessError.cursorChanged
            case (401, _), (403, _): throw RoutinePlanningWitnessError.authentication
            default: throw RoutinePlanningWitnessError.unavailable
            }
        } catch let error as RoutinePlanningWitnessError { throw error }
        catch { throw RoutinePlanningWitnessError.invalidData }
    }
    private struct ErrorEnvelope: Decodable {
        let error: ErrorBody
        private enum CodingKeys: String, CodingKey { case error }
        init(from decoder: any Decoder) throws {
            try RoutinePlanningWitnessValidation.keys(decoder, ["error"])
            error = try decoder.container(keyedBy: CodingKeys.self).decode(ErrorBody.self, forKey: .error)
        }
    }
    private struct ErrorBody: Decodable {
        let code: String
        private enum CodingKeys: String, CodingKey { case code, message }
        init(from decoder: any Decoder) throws {
            try RoutinePlanningWitnessValidation.keys(decoder, ["code", "message"])
            let c = try decoder.container(keyedBy: CodingKeys.self)
            code = try c.decode(String.self, forKey: .code)
            _ = try c.decode(String.self, forKey: .message)
        }
    }
}
