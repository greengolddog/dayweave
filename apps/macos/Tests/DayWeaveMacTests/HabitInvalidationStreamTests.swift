import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Habit invalidation SSE parser")
struct HabitInvalidationStreamTests {
    @Test("opaque cursors retain arrival order and heartbeats carry no content")
    func parsesOpaqueCursorsAndHeartbeats() throws {
        let wire = ": heartbeat\r\n\r\n"
            + "id: z_head\r\nevent: habit-invalidation\r\n"
            + "data: {\"cursor\":\"z_head\"}\r\n\r\n"
            + "id: a-head\nevent: habit-invalidation\n"
            + "data: {\"cursor\":\"a-head\"}\n\n"
        #expect(try Self.parse(Data(wire.utf8)) == ["z_head", "a-head"])
    }

    @Test("id and sole JSON cursor must agree byte for byte")
    func rejectsMismatchedOrPermissiveJSON() {
        for wire in [
            "id: cursor-a\nevent: habit-invalidation\ndata: {\"cursor\":\"cursor-b\"}\n\n",
            "id: cursor-a\nevent: habit-invalidation\ndata: { \"cursor\" : \"cursor-a\" }\n\n",
            "id: cursor-a\nevent: habit-invalidation\ndata: {\"cursor\":\"cursor-a\",\"future\":true}\n\n",
            "id: cursor-a\nevent: item-invalidation\ndata: {\"cursor\":\"cursor-a\"}\n\n",
        ] {
            #expect(throws: DayWeaveHabitStreamProtocolError.invalidEvent) {
                _ = try Self.parse(Data(wire.utf8))
            }
        }
    }

    @Test("cursor validation exactly matches encrypted habit persistence")
    func validatesHabitCursorContract() {
        for cursor in ["opaque_token-1", "A", String(repeating: "x", count: 256)] {
            #expect(DayWeaveHabitCursorContract.isValidTransportToken(cursor))
        }
        for cursor in [
            "",
            "has space",
            "has.dot",
            "has:colon",
            "has/slash",
            String(repeating: "x", count: DayWeaveHabitCursorContract.maximumBytes + 1),
        ] {
            #expect(!DayWeaveHabitCursorContract.isValidTransportToken(cursor))
        }
    }

    @Test("comments, malformed framing, controls and incomplete EOF fail closed")
    func rejectsMalformedFraming() {
        for wire in [
            ": keepalive\n\n",
            "\n",
            ": heartbeat\nid: cursor\nevent: habit-invalidation\n"
                + "data: {\"cursor\":\"cursor\"}\n\n",
            "id:cursor\nevent: habit-invalidation\ndata: {\"cursor\":\"cursor\"}\n\n",
        ] {
            #expect(throws: DayWeaveHabitStreamProtocolError.self) {
                _ = try Self.parse(Data(wire.utf8))
            }
        }

        let duplicate = "id: cursor\nid: cursor\nevent: habit-invalidation\n"
            + "data: {\"cursor\":\"cursor\"}\n\n"
        #expect(throws: DayWeaveHabitStreamProtocolError.duplicateField) {
            _ = try Self.parse(Data(duplicate.utf8))
        }

        var nul = Data(": heartbeat".utf8)
        nul.append(0)
        nul.append(contentsOf: [0x0A, 0x0A])
        #expect(throws: DayWeaveHabitStreamProtocolError.invalidControlCharacter) {
            _ = try Self.parse(nul)
        }

        #expect(throws: DayWeaveHabitStreamProtocolError.incompleteFrame) {
            _ = try Self.parse(Data("id: cursor\nevent: habit-invalidation\n".utf8))
        }
    }

    @Test("line and connection work are independently bounded")
    func enforcesResourceBounds() {
        let oversizedLine = Data(
            repeating: 0x61,
            count: DayWeaveHabitSSEParser.maximumLineBytes + 1
        )
        #expect(throws: DayWeaveHabitStreamProtocolError.lineTooLarge) {
            _ = try Self.parse(oversizedLine)
        }

        let heartbeats = String(
            repeating: ": heartbeat\n\n",
            count: DayWeaveHabitSSEParser.maximumFrames + 1
        )
        #expect(throws: DayWeaveHabitStreamProtocolError.tooManyFrames) {
            _ = try Self.parse(Data(heartbeats.utf8))
        }
    }

    private static func parse(_ data: Data) throws -> [String] {
        var parser = DayWeaveHabitSSEParser()
        var cursors: [String] = []
        for byte in data {
            if let cursor = try parser.consume(byte) { cursors.append(cursor) }
        }
        try parser.finish()
        return cursors
    }
}
#endif
