import Foundation

/// Item invalidations are deliberately separate from the ordinary canonical
/// API. The opaque cursor is only a wake-up hint; `/v1/items/delta` remains the
/// source of truth and is the only path allowed to advance durable state.
protocol DayWeaveItemStreamTransport: Sendable {
    func consumeItemInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveItemStreamCompletion
}

enum DayWeaveItemStreamCompletion: Equatable, Sendable {
    case endOfStream
    case liveEndOfStream
    case unsupported
}

enum DayWeaveItemStreamProtocolError: Error, Equatable, Sendable {
    case lineTooLarge
    case frameTooLarge
    case tooManyLines
    case tooManyFrames
    case tooManyEvents
    case invalidUTF8
    case invalidControlCharacter
    case invalidLineEnding
    case invalidField
    case duplicateField
    case incompleteFrame
    case invalidEvent
    case invalidCursor
    case invalidContentType
    case invalidContentEncoding
}

enum DayWeaveItemCursorContract {
    static let maximumBytes = 256

    /// Cursor structure is opaque. This checks only the transport contract
    /// needed to copy one server-issued token unchanged between JSON, SSE and
    /// `Last-Event-ID`; it deliberately does not decode or order the token.
    static func isValidTransportToken(_ value: String) -> Bool {
        let bytes = Array(value.utf8)
        return (1...maximumBytes).contains(bytes.count)
            && bytes.allSatisfy { byte in
                (0x21...0x7E).contains(byte) && byte != 0x22 && byte != 0x5C
            }
    }
}

/// Incremental byte-level parser for DayWeave's narrow item SSE grammar.
/// Only a standalone heartbeat or exactly one id/event/data triple is valid.
/// Opaque cursor values are compared for exact equality and never ordered.
struct DayWeaveItemSSEParser: Sendable {
    static let maximumLineBytes = 1_024
    static let maximumFrameBytes = 2_048
    static let maximumLinesPerFrame = 4
    static let maximumFrames = 20_000
    static let maximumLines = maximumFrames * maximumLinesPerFrame
    static let maximumEvents = 10_000

    private var lineBytes = Data()
    private var frameBytes = 0
    private var frameLineCount = 0
    private var totalLineCount = 0
    private var frameCount = 0
    private var sawCarriageReturn = false
    private var heartbeatSeen = false
    private var eventID: String?
    private var eventName: String?
    private var eventData: String?
    private var eventCount = 0
    private(set) var hasObservedLiveness = false

    mutating func consume(_ byte: UInt8) throws -> String? {
        guard byte >= 0x20 || byte == 0x09 || byte == 0x0A || byte == 0x0D else {
            throw DayWeaveItemStreamProtocolError.invalidControlCharacter
        }
        if sawCarriageReturn {
            guard byte == 0x0A else {
                throw DayWeaveItemStreamProtocolError.invalidLineEnding
            }
            sawCarriageReturn = false
            return try finishLine(terminatorByteCount: 2)
        }
        if byte == 0x0D {
            sawCarriageReturn = true
            return nil
        }
        if byte == 0x0A {
            return try finishLine(terminatorByteCount: 1)
        }
        guard lineBytes.count < Self.maximumLineBytes else {
            throw DayWeaveItemStreamProtocolError.lineTooLarge
        }
        lineBytes.append(byte)
        return nil
    }

    mutating func finish() throws {
        guard !sawCarriageReturn else {
            throw DayWeaveItemStreamProtocolError.invalidLineEnding
        }
        guard lineBytes.isEmpty,
              frameBytes == 0,
              frameLineCount == 0,
              !heartbeatSeen,
              eventID == nil,
              eventName == nil,
              eventData == nil else {
            throw DayWeaveItemStreamProtocolError.incompleteFrame
        }
    }

    private mutating func finishLine(terminatorByteCount: Int) throws -> String? {
        defer { lineBytes.removeAll(keepingCapacity: true) }
        guard let line = String(data: lineBytes, encoding: .utf8) else {
            throw DayWeaveItemStreamProtocolError.invalidUTF8
        }
        guard !line.unicodeScalars.contains(where: { scalar in
            scalar.value != 0x09 && CharacterSet.controlCharacters.contains(scalar)
        }) else {
            throw DayWeaveItemStreamProtocolError.invalidControlCharacter
        }
        let addedBytes = lineBytes.count + terminatorByteCount
        guard frameBytes <= Self.maximumFrameBytes - addedBytes else {
            throw DayWeaveItemStreamProtocolError.frameTooLarge
        }
        frameBytes += addedBytes
        guard frameLineCount < Self.maximumLinesPerFrame,
              totalLineCount < Self.maximumLines else {
            throw DayWeaveItemStreamProtocolError.tooManyLines
        }
        frameLineCount += 1
        totalLineCount += 1

        if line.isEmpty {
            defer { resetFrame() }
            guard heartbeatSeen || eventID != nil || eventName != nil || eventData != nil else {
                throw DayWeaveItemStreamProtocolError.invalidEvent
            }
            guard frameCount < Self.maximumFrames else {
                throw DayWeaveItemStreamProtocolError.tooManyFrames
            }
            frameCount += 1
            if heartbeatSeen {
                guard eventID == nil, eventName == nil, eventData == nil else {
                    throw DayWeaveItemStreamProtocolError.invalidEvent
                }
                hasObservedLiveness = true
                return nil
            }
            guard let cursor = eventID,
                  DayWeaveItemCursorContract.isValidTransportToken(cursor),
                  eventName == "item-invalidation",
                  eventData == "{\"cursor\":\"\(cursor)\"}" else {
                throw DayWeaveItemStreamProtocolError.invalidEvent
            }
            guard eventCount < Self.maximumEvents else {
                throw DayWeaveItemStreamProtocolError.tooManyEvents
            }
            eventCount += 1
            hasObservedLiveness = true
            return cursor
        }

        if line.hasPrefix(":") {
            guard line == ": heartbeat",
                  !heartbeatSeen,
                  eventID == nil,
                  eventName == nil,
                  eventData == nil else {
                throw DayWeaveItemStreamProtocolError.invalidEvent
            }
            heartbeatSeen = true
            return nil
        }
        guard !heartbeatSeen else {
            throw DayWeaveItemStreamProtocolError.invalidEvent
        }

        guard let separator = line.firstIndex(of: ":") else {
            throw DayWeaveItemStreamProtocolError.invalidField
        }
        let field = line[..<separator]
        let separatorEnd = line.index(after: separator)
        guard separatorEnd < line.endIndex, line[separatorEnd] == " " else {
            throw DayWeaveItemStreamProtocolError.invalidField
        }
        let valueStart = line.index(after: separatorEnd)
        let value = line[valueStart...]
        guard !field.isEmpty, !value.isEmpty else {
            throw DayWeaveItemStreamProtocolError.invalidField
        }
        switch field {
        case "id":
            guard eventID == nil else {
                throw DayWeaveItemStreamProtocolError.duplicateField
            }
            let cursor = String(value)
            guard DayWeaveItemCursorContract.isValidTransportToken(cursor) else {
                throw DayWeaveItemStreamProtocolError.invalidCursor
            }
            eventID = cursor
        case "event":
            guard eventName == nil else {
                throw DayWeaveItemStreamProtocolError.duplicateField
            }
            eventName = String(value)
        case "data":
            guard eventData == nil else {
                throw DayWeaveItemStreamProtocolError.duplicateField
            }
            eventData = String(value)
        default:
            throw DayWeaveItemStreamProtocolError.invalidField
        }
        return nil
    }

    private mutating func resetFrame() {
        frameBytes = 0
        frameLineCount = 0
        heartbeatSeen = false
        eventID = nil
        eventName = nil
        eventData = nil
    }
}
