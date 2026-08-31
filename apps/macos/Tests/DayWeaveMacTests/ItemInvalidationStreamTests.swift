import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Item invalidation SSE parser")
struct ItemInvalidationStreamTests {
    @Test("opaque cursors retain arrival order without numeric ordering")
    func parsesOpaqueCursorsAndHeartbeats() throws {
        let wire = ": heartbeat\r\n\r\n"
            + "id: z-head\r\nevent: item-invalidation\r\n"
            + "data: {\"cursor\":\"z-head\"}\r\n\r\n"
            + "id: a-head\nevent: item-invalidation\n"
            + "data: {\"cursor\":\"a-head\"}\n\n"
        #expect(try Self.parse(Data(wire.utf8)) == ["z-head", "a-head"])
    }

    @Test("id and sole JSON cursor must agree byte for byte")
    func rejectsMismatchedOrPermissiveJSON() {
        for wire in [
            "id: cursor-a\nevent: item-invalidation\ndata: {\"cursor\":\"cursor-b\"}\n\n",
            "id: cursor-a\nevent: item-invalidation\ndata: { \"cursor\" : \"cursor-a\" }\n\n",
            "id: cursor-a\nevent: item-invalidation\ndata: {\"cursor\":\"cursor-a\",\"future\":true}\n\n",
            "id: cursor-a\nevent: other\ndata: {\"cursor\":\"cursor-a\"}\n\n",
        ] {
            #expect(throws: DayWeaveItemStreamProtocolError.invalidEvent) {
                _ = try Self.parse(Data(wire.utf8))
            }
        }
    }

    @Test("cursor validation is transport-only and bounded")
    func validatesOpaqueTransportToken() {
        #expect(DayWeaveItemCursorContract.isValidTransportToken("opaque._:-token"))
        for cursor in [
            "",
            "has space",
            "has\tseparator",
            "has\"quote",
            "has\\slash",
            String(repeating: "x", count: DayWeaveItemCursorContract.maximumBytes + 1),
        ] {
            #expect(!DayWeaveItemCursorContract.isValidTransportToken(cursor))
        }
    }

    @Test("only a standalone exact heartbeat is accepted")
    func rejectsPermissiveCommentsAndEmptyFrames() {
        for wire in [
            ": keepalive\n\n",
            "\n",
            ": heartbeat\nid: cursor\nevent: item-invalidation\n"
                + "data: {\"cursor\":\"cursor\"}\n\n",
            "id:cursor\nevent: item-invalidation\ndata: {\"cursor\":\"cursor\"}\n\n",
        ] {
            #expect(throws: DayWeaveItemStreamProtocolError.self) {
                _ = try Self.parse(Data(wire.utf8))
            }
        }
    }

    @Test("duplicate fields, NUL, invalid UTF-8 and incomplete EOF fail closed")
    func rejectsMalformedFraming() {
        let duplicate = "id: cursor\nid: cursor\nevent: item-invalidation\n"
            + "data: {\"cursor\":\"cursor\"}\n\n"
        #expect(throws: DayWeaveItemStreamProtocolError.duplicateField) {
            _ = try Self.parse(Data(duplicate.utf8))
        }

        var nul = Data(": heartbeat".utf8)
        nul.append(0)
        nul.append(contentsOf: [0x0A, 0x0A])
        #expect(throws: DayWeaveItemStreamProtocolError.invalidControlCharacter) {
            _ = try Self.parse(nul)
        }

        var invalidUTF8 = Data("id: ".utf8)
        invalidUTF8.append(0xFF)
        invalidUTF8.append(0x0A)
        #expect(throws: DayWeaveItemStreamProtocolError.invalidUTF8) {
            _ = try Self.parse(invalidUTF8)
        }

        #expect(throws: DayWeaveItemStreamProtocolError.incompleteFrame) {
            _ = try Self.parse(Data("id: cursor\nevent: item-invalidation\n".utf8))
        }
    }

    @Test("line, frame and heartbeat totals are independently bounded")
    func enforcesResourceBounds() {
        let oversizedLine = Data(
            repeating: 0x61,
            count: DayWeaveItemSSEParser.maximumLineBytes + 1
        )
        #expect(throws: DayWeaveItemStreamProtocolError.lineTooLarge) {
            _ = try Self.parse(oversizedLine)
        }

        let heartbeats = String(
            repeating: ": heartbeat\n\n",
            count: DayWeaveItemSSEParser.maximumFrames + 1
        )
        #expect(throws: DayWeaveItemStreamProtocolError.tooManyFrames) {
            _ = try Self.parse(Data(heartbeats.utf8))
        }
    }

    private static func parse(_ data: Data) throws -> [String] {
        var parser = DayWeaveItemSSEParser()
        var cursors: [String] = []
        for byte in data {
            if let cursor = try parser.consume(byte) { cursors.append(cursor) }
        }
        try parser.finish()
        return cursors
    }
}
#endif
