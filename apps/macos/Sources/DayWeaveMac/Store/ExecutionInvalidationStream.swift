import Foundation

/// A stream connection is deliberately separate from the ordinary execution
/// transport. Snapshot/command transports remain the source of truth and test
/// doubles that do not model foreground delivery need not implement this API.
protocol DayWeaveExecutionStreamTransport: Sendable {
    func consumeExecutionInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveExecutionStreamCompletion
}

/// Published-schedule invalidations carry only a numeric wake-up hint. The
/// authoritative JSON resource remains the sole source permitted to install
/// or clear an encrypted planner projection.
protocol DayWeaveScheduleStreamTransport: Sendable {
    func consumeScheduleInvalidations(
        after revision: UInt64,
        _ receive: @escaping @Sendable (UInt64) async -> Void
    ) async throws -> DayWeaveScheduleStreamCompletion
}

enum DayWeaveScheduleStreamCompletion: Equatable, Sendable {
    case endOfStream
    case liveEndOfStream
    /// The server head moved behind the durable local cursor (for example,
    /// after an authoritative restore). Callers must recover with GET and may
    /// never infer schedule content from this number.
    case cursorAhead(headRevision: UInt64)
}

enum DayWeaveExecutionStreamCompletion: Equatable, Sendable {
    /// The peer closed without one complete heartbeat or invalidation. This is
    /// an early transient close and reconnects with exponential backoff.
    case endOfStream
    /// At least one bounded heartbeat or validated invalidation proved that
    /// the connection made progress. A normal five-minute server close may
    /// reset reconnect backoff.
    case liveEndOfStream
    /// A 404 disables streaming only for the current foreground activation;
    /// the independent poll path continues to provide durable catch-up.
    case unsupported
}

enum DayWeaveExecutionStreamProtocolError: Error, Equatable, Sendable {
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
    case nonMonotonicRevision
    case invalidContentType
    case invalidContentEncoding
    case invalidCacheControl
    case invalidPragma
    case invalidBufferingPolicy
}

/// Incremental, byte-level parser for DayWeave's intentionally narrow SSE
/// grammar. It accepts only the server's standalone `: heartbeat` frame or
/// exactly one `id`, `event`, and `data` field per invalidation frame. Bounds
/// are per connection so an authenticated peer cannot grow work without limit
/// during the five-minute stream lifetime.
struct DayWeaveExecutionSSEParser: Sendable {
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
    private var lastEventRevision: UInt64
    private let expectedEventName: String
    private(set) var hasObservedLiveness = false

    init(
        after revision: UInt64 = 0,
        expectedEventName: String = "execution-invalidation"
    ) {
        lastEventRevision = revision
        self.expectedEventName = expectedEventName
    }

    mutating func consume(_ byte: UInt8) throws -> UInt64? {
        guard byte >= 0x20 || byte == 0x09 || byte == 0x0A || byte == 0x0D else {
            throw DayWeaveExecutionStreamProtocolError.invalidControlCharacter
        }
        if sawCarriageReturn {
            guard byte == 0x0A else {
                throw DayWeaveExecutionStreamProtocolError.invalidLineEnding
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
            throw DayWeaveExecutionStreamProtocolError.lineTooLarge
        }
        lineBytes.append(byte)
        return nil
    }

    mutating func finish() throws {
        guard !sawCarriageReturn else {
            throw DayWeaveExecutionStreamProtocolError.invalidLineEnding
        }
        guard lineBytes.isEmpty,
              frameBytes == 0,
              frameLineCount == 0,
              !heartbeatSeen,
              eventID == nil,
              eventName == nil,
              eventData == nil else {
            throw DayWeaveExecutionStreamProtocolError.incompleteFrame
        }
    }

    private mutating func finishLine(terminatorByteCount: Int) throws -> UInt64? {
        defer { lineBytes.removeAll(keepingCapacity: true) }
        guard let line = String(data: lineBytes, encoding: .utf8) else {
            throw DayWeaveExecutionStreamProtocolError.invalidUTF8
        }
        guard !line.unicodeScalars.contains(where: { scalar in
            scalar.value != 0x09 && CharacterSet.controlCharacters.contains(scalar)
        }) else {
            throw DayWeaveExecutionStreamProtocolError.invalidControlCharacter
        }
        let addedBytes = lineBytes.count + terminatorByteCount
        guard frameBytes <= Self.maximumFrameBytes - addedBytes else {
            throw DayWeaveExecutionStreamProtocolError.frameTooLarge
        }
        frameBytes += addedBytes
        guard frameLineCount < Self.maximumLinesPerFrame,
              totalLineCount < Self.maximumLines else {
            throw DayWeaveExecutionStreamProtocolError.tooManyLines
        }
        frameLineCount += 1
        totalLineCount += 1

        if line.isEmpty {
            defer { resetFrame() }
            guard heartbeatSeen || eventID != nil || eventName != nil || eventData != nil else {
                throw DayWeaveExecutionStreamProtocolError.invalidEvent
            }
            guard frameCount < Self.maximumFrames else {
                throw DayWeaveExecutionStreamProtocolError.tooManyFrames
            }
            frameCount += 1
            if heartbeatSeen {
                guard eventID == nil, eventName == nil, eventData == nil else {
                    throw DayWeaveExecutionStreamProtocolError.invalidEvent
                }
                hasObservedLiveness = true
                return nil
            }
            guard let id = eventID,
                  eventName == expectedEventName,
                  let data = eventData,
                  let idRevision = Self.canonicalRevision(id),
                  data == "{\"revision\":\(idRevision)}" else {
                throw DayWeaveExecutionStreamProtocolError.invalidEvent
            }
            guard idRevision > lastEventRevision else {
                throw DayWeaveExecutionStreamProtocolError.nonMonotonicRevision
            }
            guard eventCount < Self.maximumEvents else {
                throw DayWeaveExecutionStreamProtocolError.tooManyEvents
            }
            eventCount += 1
            lastEventRevision = idRevision
            hasObservedLiveness = true
            return idRevision
        }

        if line.hasPrefix(":") {
            guard line == ": heartbeat",
                  !heartbeatSeen,
                  eventID == nil,
                  eventName == nil,
                  eventData == nil else {
                throw DayWeaveExecutionStreamProtocolError.invalidEvent
            }
            heartbeatSeen = true
            return nil
        }
        guard !heartbeatSeen else {
            throw DayWeaveExecutionStreamProtocolError.invalidEvent
        }

        let field: Substring
        let value: Substring
        if let separator = line.firstIndex(of: ":") {
            field = line[..<separator]
            let separatorEnd = line.index(after: separator)
            guard separatorEnd < line.endIndex, line[separatorEnd] == " " else {
                throw DayWeaveExecutionStreamProtocolError.invalidField
            }
            let valueStart = line.index(after: separatorEnd)
            value = line[valueStart...]
        } else {
            throw DayWeaveExecutionStreamProtocolError.invalidField
        }
        guard !field.isEmpty, !value.isEmpty else {
            throw DayWeaveExecutionStreamProtocolError.invalidField
        }
        switch field {
        case "id":
            guard eventID == nil else {
                throw DayWeaveExecutionStreamProtocolError.duplicateField
            }
            eventID = String(value)
        case "event":
            guard eventName == nil else {
                throw DayWeaveExecutionStreamProtocolError.duplicateField
            }
            eventName = String(value)
        case "data":
            guard eventData == nil else {
                throw DayWeaveExecutionStreamProtocolError.duplicateField
            }
            eventData = String(value)
        default:
            throw DayWeaveExecutionStreamProtocolError.invalidField
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

    private static func canonicalRevision(_ value: String) -> UInt64? {
        let bytes = Array(value.utf8)
        guard !bytes.isEmpty,
              bytes.allSatisfy({ (0x30...0x39).contains($0) }),
              bytes.count == 1 || bytes.first != 0x30,
              let revision = UInt64(value),
              revision <= UInt64(Int64.max) else { return nil }
        return revision
    }

}
