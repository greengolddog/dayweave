import Foundation

/// Habit invalidations contain no habit or outcome data. The opaque cursor is
/// only a wake-up hint; the encrypted cursor committed with `/habits/.../delta`
/// remains the sole durable position and source of truth.
protocol DayWeaveHabitStreamTransport: Sendable {
    func consumeHabitInvalidations(
        after cursor: String,
        _ receive: @escaping @Sendable (String) async -> Void
    ) async throws -> DayWeaveHabitStreamCompletion
}

enum DayWeaveHabitStreamCompletion: Equatable, Sendable {
    case endOfStream
    case liveEndOfStream
    /// A 404 disables the optional stream for this foreground activation;
    /// independent polling continues to drain the authoritative delta.
    case unsupported
}

enum DayWeaveHabitStreamProtocolError: Error, Equatable, Sendable {
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
    case invalidCacheControl
    case invalidPragma
    case invalidBufferingPolicy
}

enum DayWeaveHabitCursorContract {
    static let maximumBytes = 256

    /// This deliberately validates only the established habit transport
    /// alphabet. Cursor structure and ordering remain opaque to the client.
    static func isValidTransportToken(_ value: String) -> Bool {
        let bytes = Array(value.utf8)
        return (1...maximumBytes).contains(bytes.count) && bytes.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte)
                || (97...122).contains(byte) || byte == 45 || byte == 95
        }
    }
}

/// Incremental byte-level parser for the narrow habit SSE grammar. It accepts
/// only a standalone heartbeat or one exact id/event/data triple per frame.
struct DayWeaveHabitSSEParser: Sendable {
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
            throw DayWeaveHabitStreamProtocolError.invalidControlCharacter
        }
        if sawCarriageReturn {
            guard byte == 0x0A else {
                throw DayWeaveHabitStreamProtocolError.invalidLineEnding
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
            throw DayWeaveHabitStreamProtocolError.lineTooLarge
        }
        lineBytes.append(byte)
        return nil
    }

    mutating func finish() throws {
        guard !sawCarriageReturn else {
            throw DayWeaveHabitStreamProtocolError.invalidLineEnding
        }
        guard lineBytes.isEmpty,
              frameBytes == 0,
              frameLineCount == 0,
              !heartbeatSeen,
              eventID == nil,
              eventName == nil,
              eventData == nil else {
            throw DayWeaveHabitStreamProtocolError.incompleteFrame
        }
    }

    private mutating func finishLine(terminatorByteCount: Int) throws -> String? {
        defer { lineBytes.removeAll(keepingCapacity: true) }
        guard let line = String(data: lineBytes, encoding: .utf8) else {
            throw DayWeaveHabitStreamProtocolError.invalidUTF8
        }
        guard !line.unicodeScalars.contains(where: { scalar in
            scalar.value != 0x09 && CharacterSet.controlCharacters.contains(scalar)
        }) else {
            throw DayWeaveHabitStreamProtocolError.invalidControlCharacter
        }
        let addedBytes = lineBytes.count + terminatorByteCount
        guard frameBytes <= Self.maximumFrameBytes - addedBytes else {
            throw DayWeaveHabitStreamProtocolError.frameTooLarge
        }
        frameBytes += addedBytes
        guard frameLineCount < Self.maximumLinesPerFrame,
              totalLineCount < Self.maximumLines else {
            throw DayWeaveHabitStreamProtocolError.tooManyLines
        }
        frameLineCount += 1
        totalLineCount += 1

        if line.isEmpty {
            defer { resetFrame() }
            guard heartbeatSeen || eventID != nil || eventName != nil || eventData != nil else {
                throw DayWeaveHabitStreamProtocolError.invalidEvent
            }
            guard frameCount < Self.maximumFrames else {
                throw DayWeaveHabitStreamProtocolError.tooManyFrames
            }
            frameCount += 1
            if heartbeatSeen {
                guard eventID == nil, eventName == nil, eventData == nil else {
                    throw DayWeaveHabitStreamProtocolError.invalidEvent
                }
                hasObservedLiveness = true
                return nil
            }
            guard let cursor = eventID,
                  DayWeaveHabitCursorContract.isValidTransportToken(cursor),
                  eventName == "habit-invalidation",
                  eventData == "{\"cursor\":\"\(cursor)\"}" else {
                throw DayWeaveHabitStreamProtocolError.invalidEvent
            }
            guard eventCount < Self.maximumEvents else {
                throw DayWeaveHabitStreamProtocolError.tooManyEvents
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
                throw DayWeaveHabitStreamProtocolError.invalidEvent
            }
            heartbeatSeen = true
            return nil
        }
        guard !heartbeatSeen else {
            throw DayWeaveHabitStreamProtocolError.invalidEvent
        }

        guard let separator = line.firstIndex(of: ":") else {
            throw DayWeaveHabitStreamProtocolError.invalidField
        }
        let field = line[..<separator]
        let separatorEnd = line.index(after: separator)
        guard separatorEnd < line.endIndex, line[separatorEnd] == " " else {
            throw DayWeaveHabitStreamProtocolError.invalidField
        }
        let valueStart = line.index(after: separatorEnd)
        let value = line[valueStart...]
        guard !field.isEmpty, !value.isEmpty else {
            throw DayWeaveHabitStreamProtocolError.invalidField
        }
        switch field {
        case "id":
            guard eventID == nil else {
                throw DayWeaveHabitStreamProtocolError.duplicateField
            }
            let cursor = String(value)
            guard DayWeaveHabitCursorContract.isValidTransportToken(cursor) else {
                throw DayWeaveHabitStreamProtocolError.invalidCursor
            }
            eventID = cursor
        case "event":
            guard eventName == nil else {
                throw DayWeaveHabitStreamProtocolError.duplicateField
            }
            eventName = String(value)
        case "data":
            guard eventData == nil else {
                throw DayWeaveHabitStreamProtocolError.duplicateField
            }
            eventData = String(value)
        default:
            throw DayWeaveHabitStreamProtocolError.invalidField
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
