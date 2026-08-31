import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Execution invalidation SSE parser")
struct ExecutionInvalidationStreamTests {
    @Test("comments and strict CRLF frames yield canonical increasing revisions")
    func parsesHeartbeatsAndInvalidations() throws {
        let wire = ": heartbeat\r\n\r\n"
            + "id: 7\r\nevent: execution-invalidation\r\n"
            + "data: {\"revision\":7}\r\n\r\n"
            + ": heartbeat\r\n\r\n"
            + "id: 8\r\nevent: execution-invalidation\r\n"
            + "data: {\"revision\":8}\r\n\r\n"

        #expect(try Self.parse(Data(wire.utf8)) == [7, 8])
    }

    @Test("event id and JSON revision must be exact canonical integers")
    func rejectsNonCanonicalOrMismatchedRevisions() {
        let invalidFrames = [
            "id: 07\nevent: execution-invalidation\ndata: {\"revision\":7}\n\n",
            "id: 7\nevent: execution-invalidation\ndata: {\"revision\":8}\n\n",
            "id: 7\nevent: execution-invalidation\ndata: {\"revision\":7.0}\n\n",
            "id: 7\nevent: execution-invalidation\ndata: {\"revision\":7,\"revision\":7}\n\n",
            "id: 7\nevent: execution-invalidation\ndata: {\"revision\":7,\"future\":true}\n\n",
            "id: 9223372036854775808\nevent: execution-invalidation\ndata: {\"revision\":9223372036854775808}\n\n",
        ]
        for wire in invalidFrames {
            #expect(throws: DayWeaveExecutionStreamProtocolError.invalidEvent) {
                _ = try Self.parse(Data(wire.utf8))
            }
        }
    }

    @Test("one connection cannot move its invalidation revision backward")
    func rejectsNonMonotonicFrames() {
        let wire = "id: 12\nevent: execution-invalidation\n"
            + "data: {\"revision\":12}\n\n"
            + "id: 11\nevent: execution-invalidation\n"
            + "data: {\"revision\":11}\n\n"
        #expect(throws: DayWeaveExecutionStreamProtocolError.nonMonotonicRevision) {
            _ = try Self.parse(Data(wire.utf8))
        }
    }

    @Test("the durable resume cursor is the first monotonicity boundary")
    func rejectsRevisionAtOrBeforeResumeCursor() {
        for revision in [4, 5] {
            let wire = "id: \(revision)\nevent: execution-invalidation\n"
                + "data: {\"revision\":\(revision)}\n\n"
            #expect(throws: DayWeaveExecutionStreamProtocolError.nonMonotonicRevision) {
                _ = try Self.parse(Data(wire.utf8), after: 5)
            }
        }
    }

    @Test("only exact standalone heartbeat and server event frames are accepted")
    func rejectsPermissiveSSEGrammar() {
        let invalidFrames = [
            ": keepalive\n\n",
            "\n",
            ": heartbeat\nid: 1\nevent: execution-invalidation\n"
                + "data: {\"revision\":1}\n\n",
            "id: 1\n: heartbeat\nevent: execution-invalidation\n"
                + "data: {\"revision\":1}\n\n",
            "id:1\nevent: execution-invalidation\ndata: {\"revision\":1}\n\n",
            "id: 1\nevent: execution-invalidation\ndata: { \"revision\" : 1 }\n\n",
        ]
        for wire in invalidFrames {
            #expect(throws: DayWeaveExecutionStreamProtocolError.self) {
                _ = try Self.parse(Data(wire.utf8))
            }
        }
    }

    @Test("duplicate fields, invalid UTF-8, and incomplete EOF fail closed")
    func rejectsMalformedFraming() throws {
        let duplicate = """
        id: 1
        id: 1
        event: execution-invalidation
        data: {"revision":1}

        """
        #expect(throws: DayWeaveExecutionStreamProtocolError.duplicateField) {
            _ = try Self.parse(Data(duplicate.utf8))
        }

        var invalidUTF8 = Data("id: ".utf8)
        invalidUTF8.append(0xFF)
        invalidUTF8.append(0x0A)
        #expect(throws: DayWeaveExecutionStreamProtocolError.invalidUTF8) {
            _ = try Self.parse(invalidUTF8)
        }

        var nulHeartbeat = Data(": heartbeat".utf8)
        nulHeartbeat.append(0x00)
        nulHeartbeat.append(contentsOf: [0x0A, 0x0A])
        #expect(throws: DayWeaveExecutionStreamProtocolError.invalidControlCharacter) {
            _ = try Self.parse(nulHeartbeat)
        }

        let deleteControl = Data([0x3A, 0x7F, 0x0A, 0x0A])
        #expect(throws: DayWeaveExecutionStreamProtocolError.invalidControlCharacter) {
            _ = try Self.parse(deleteControl)
        }

        let incomplete = Data("id: 2\nevent: execution-invalidation\n".utf8)
        #expect(throws: DayWeaveExecutionStreamProtocolError.incompleteFrame) {
            _ = try Self.parse(incomplete)
        }
    }

    @Test("line and frame buffers are independently bounded")
    func enforcesMemoryBounds() {
        let oversizedLine = Data(repeating: 0x61, count: DayWeaveExecutionSSEParser.maximumLineBytes + 1)
        #expect(throws: DayWeaveExecutionStreamProtocolError.lineTooLarge) {
            _ = try Self.parse(oversizedLine)
        }

        let longID = String(repeating: "7", count: 1_019)
        let longEvent = String(repeating: "e", count: 1_017)
        let oversizedFrame = Data(
            ("id: \(longID)\n"
                + "event: \(longEvent)\n"
                + "data: x\n").utf8
        )
        #expect(throws: DayWeaveExecutionStreamProtocolError.frameTooLarge) {
            _ = try Self.parse(oversizedFrame)
        }
    }

    @Test("heartbeats count toward the per-connection frame bound")
    func boundsHeartbeatFrames() {
        let wire = String(
            repeating: ": heartbeat\n\n",
            count: DayWeaveExecutionSSEParser.maximumFrames + 1
        )
        #expect(throws: DayWeaveExecutionStreamProtocolError.tooManyFrames) {
            _ = try Self.parse(Data(wire.utf8))
        }
    }

    private static func parse(_ bytes: Data, after revision: UInt64 = 0) throws -> [UInt64] {
        var parser = DayWeaveExecutionSSEParser(after: revision)
        var revisions: [UInt64] = []
        for byte in bytes {
            if let revision = try parser.consume(byte) {
                revisions.append(revision)
            }
        }
        try parser.finish()
        return revisions
    }
}
#endif
