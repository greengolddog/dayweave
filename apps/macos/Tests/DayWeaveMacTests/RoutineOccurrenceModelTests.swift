import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

enum RoutineOccurrenceTestFixtures {
    static func id(_ value: Int) -> UUID { UUID(uuidString: String(format: "00000000-0000-0000-0000-%012d", value))! }
    static let instanceID = id(100)
    static let rootID = id(1)
    static let childID = id(2)
    static let operationID = id(200)
    static let plannerID = UUID(uuidString: "00000000-0000-5000-8000-000000000001")!
    static let hash = "sha256:" + String(repeating: "1", count: 64)
    static let now = "2026-09-10T09:00:00.123456Z"
    static func manifest(count: Int = 2) -> RoutineOccurrenceManifest {
        .init(schemaVersion: 1, id: instanceID, seriesItemID: rootID, occurrenceID: plannerID,
            identity: .calendarDay(date: "2026-09-10", bucketOrdinal: 0), nominalStart: "2026-09-10T09:00:00Z", nominalEnd: "2026-09-10T10:00:00Z",
            windowStart: "2026-09-10T09:00:00Z", windowEnd: "2026-09-10T10:00:00Z", timezoneName: "UTC", definitionHash: hash,
            members: (1...count).map { index in .init(itemID: id(index), parentID: index == 1 ? nil : id(index - 1), sourceRevision: 1,
                title: "Synthetic \(index)", kind: index == 1 ? .routine : .task, recurs: index == 1, siblingOrder: 0,
                requiredForParent: true, initialOpen: .init(status: .planned)) })
    }
    static func snapshot(count: Int = 2, revision: UInt64 = 1, mode: ItemCompletionMode = .automatic) -> RoutineOccurrenceSnapshot {
        .init(schemaVersion: 1, aggregate: .init(manifest: manifest(count: count), revision: revision,
            members: (1...count).map { index in .init(itemID: id(index), revision: index == 1 ? revision : 1, status: .planned,
                requiredForParent: true, mode: index == 1 ? mode : .automatic, open: .init(status: .planned), provenance: nil,
                completedAt: nil, updatedAt: now) }), evidenceHash: hash, freshEditEligible: true,
            members: (1...count).map { index in .init(itemID: id(index), counts: .init(requiredDescendants: UInt64(count - index),
                completed: 0, incomplete: UInt64(count - index)), occurrenceEvidenceRequired: false,
                reason: index == 1 && revision > 1 ? .policyReviewed : .unchanged) })
    }
    static func command() -> RoutineOccurrenceCommand {
        .init(schemaVersion: 1, operationID: operationID, expectedInstanceRevision: 1, expectedMemberRevision: 1,
            expectedEvidenceHash: hash, action: .setPolicy(requiredForParent: true, mode: .keepOpen))
    }
    static func mutation(replayed: Bool = true) -> RoutineOccurrenceMutation {
        .init(operationID: operationID, replayed: replayed, occurrence: snapshot(revision: 2, mode: .keepOpen))
    }
    static func page() -> RoutineOccurrencePage {
        .init(schemaVersion: 1, changes: [.init(sequence: 1, occurrence: snapshot())], cursor: "opaque-current-checkpoint", hasMore: false)
    }
    static func bytes<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(value)
    }
    static func object<T: Encodable>(_ value: T) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: bytes(value)) as! [String: Any]
    }
    static func data(_ object: [String: Any]) throws -> Data { try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]) }
}

#if canImport(Testing)
@Suite("Routine occurrence closed wire and complete evidence")
struct RoutineOccurrenceModelTests {
    private typealias F = RoutineOccurrenceTestFixtures

    @Test("shared producer fixtures agree with closed native semantic admission")
    func sharedContract() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let raw = try Data(contentsOf: root.appendingPathComponent("fixtures/routine-occurrences/wire-v1.json"))
        let fixture = try RoutineOccurrenceRawFixture.read(raw)
        for (entries, expected) in [(fixture.valid, true), (fixture.invalid, false)] {
            for entry in entries {
                let kind = entry.kind, name = entry.name, data = entry.value
                let accepted: Bool
                switch kind {
                case "snapshot": accepted = (try? RoutineOccurrenceValidation.decode(RoutineOccurrenceSnapshot.self, from: data))?.isValid == true
                case "command": accepted = (try? RoutineOccurrenceValidation.decode(RoutineOccurrenceCommand.self, from: data))?.isValid(for: F.id(3)) == true
                case "mutation": accepted = (try? RoutineOccurrenceValidation.decode(RoutineOccurrenceMutation.self, from: data))?.isValid == true
                case "page": accepted = (try? RoutineOccurrenceValidation.decode(RoutineOccurrencePage.self, from: data))?.isValid == true
                default: Issue.record("Unknown fixture kind \(kind)"); continue
                }
                #expect(accepted == expected, "Fixture \(name)")
            }
        }
    }

    @Test("fixture slices preserve fractional and maximum-int64 lexical tokens")
    func fixtureLexicalTokens() throws {
        let raw = Data(#"{"schema_version":1,"valid":[{"name":"integer","kind":"command","value":{"revision":9223372036854775807,"nested":{"text":"escaped \" bracket } ]"}}}],"invalid":[{"name":"fraction","kind":"command","value":{"revision":1.0,"exponent":1e0}}]}"#.utf8)
        let fixture = try RoutineOccurrenceRawFixture.read(raw)
        #expect(String(decoding: fixture.valid[0].value, as: UTF8.self) == #"{"revision":9223372036854775807,"nested":{"text":"escaped \" bracket } ]"}}"#)
        #expect(String(decoding: fixture.invalid[0].value, as: UTF8.self) == #"{"revision":1.0,"exponent":1e0}"#)
        #expect(StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: fixture.valid[0].value))
        #expect(!StrictJSONObjectKeyScanner.hasUniqueKeysAndCanonicalIntegers(in: fixture.invalid[0].value))
    }

    @Test("required nullables remain explicit through encode/decode")
    func nullableShape() throws {
        let snapshot = F.snapshot()
        #expect(try RoutineOccurrenceValidation.decode(RoutineOccurrenceSnapshot.self, from: F.bytes(snapshot)) == snapshot)
        let definition = try F.object(snapshot.aggregate.manifest.members[0])
        #expect(definition["parent_id"] is NSNull)
        let state = try F.object(snapshot.aggregate.members[0])
        #expect(state["provenance"] is NSNull && state["completed_at"] is NSNull)
        for key in ["provenance", "completed_at"] {
            var missing = state; missing.removeValue(forKey: key)
            #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceMemberState.self, from: F.data(missing)) }
        }
        var missing = definition; missing.removeValue(forKey: "parent_id")
        #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceMemberDefinition.self, from: F.data(missing)) }
    }

    @Test("duplicate raw keys and noncanonical integer tokens cannot become evidence")
    func rawGrammar() throws {
        let body = String(decoding: try F.bytes(F.command()), as: UTF8.self)
        for token in ["\"schema_version\":1,\"schema_version\":1", "\"schema_version\":1,\"schema_versi\\u006fn\":1",
                      "\"schema_version\":1.0", "\"schema_version\":1e0", "\"schema_version\":-0"] {
            let data = Data(body.replacingOccurrences(of: "\"schema_version\":1", with: token).utf8)
            #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceCommand.self, from: data) }
        }
    }

    @Test("exact member sets, missing parents and cycles fail before recursion")
    func topologyAndMembership() throws {
        let original = try F.object(F.manifest(count: 3))
        for mode in 0..<5 {
            var object = original
            var members = object["members"] as! [[String: Any]]
            switch mode {
            case 0: members[1]["item_id"] = members[0]["item_id"]
            case 1: members[1]["parent_id"] = F.id(99).uuidString
            case 2: members[1]["parent_id"] = members[2]["item_id"]
            case 3: members[1]["parent_id"] = NSNull()
            default: members[0]["kind"] = "project"
            }
            object["members"] = members
            #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceManifest.self, from: F.data(object)) }
        }
        var aggregate = try F.object(F.snapshot().aggregate)
        var states = aggregate["members"] as! [[String: Any]]
        states[1]["item_id"] = states[0]["item_id"]; aggregate["members"] = states
        #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceAggregate.self, from: F.data(aggregate)) }
    }

    @Test("five thousand levels decode iteratively and the member ceiling is exact")
    func deepAndBounded() throws {
        let deep = F.snapshot(count: 5_000)
        #expect(try RoutineOccurrenceValidation.decode(RoutineOccurrenceSnapshot.self, from: F.bytes(deep)) == deep)
        #expect(F.manifest(count: 10_000).isValid)
        #expect(!F.manifest(count: 10_001).isValid)
        let oversized = Data(repeating: 32, count: RoutineOccurrenceValidation.maximumBytes + 1)
        #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceSnapshot.self, from: oversized) }
    }

    @Test("server aggregate must be a fixed point and counts cannot fabricate readiness")
    func fixedPoint() throws {
        var object = try F.object(F.snapshot())
        var aggregate = object["aggregate"] as! [String: Any]
        var states = aggregate["members"] as! [[String: Any]]
        states[1]["status"] = "completed"; states[1]["completed_at"] = F.now
        aggregate["members"] = states; object["aggregate"] = aggregate
        #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceSnapshot.self, from: F.data(object)) }
        object = try F.object(F.snapshot())
        var evaluations = object["members"] as! [[String: Any]]
        evaluations[0]["counts"] = ["required_descendants": 1, "completed": 1, "incomplete": 0, "occurrence_evidence_required": 0]
        object["members"] = evaluations
        #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceSnapshot.self, from: F.data(object)) }
    }

    @Test("manual completion preserves unfinished descendants and exact reopening custody")
    func manualParent() throws {
        var object = try F.object(F.snapshot().aggregate)
        var states = object["members"] as! [[String: Any]]
        states[0]["status"] = "completed"; states[0]["mode"] = "complete"; states[0]["completed_at"] = F.now
        states[0]["provenance"] = ["kind": "manual", "reopen": states[0]["open"]!]
        object["members"] = states
        let value = try RoutineOccurrenceValidation.decode(RoutineOccurrenceAggregate.self, from: F.data(object))
        #expect(value.members[0].status == .completed && value.members[1].status == .planned)
        states[0]["provenance"] = NSNull(); object["members"] = states
        #expect(throws: RoutineOccurrenceError.invalidData) { try RoutineOccurrenceValidation.decode(RoutineOccurrenceAggregate.self, from: F.data(object)) }
    }

    @Test("wide-offset identity anchors remain exact without weakening canonical authoring")
    func anchorsAndWindows() throws {
        for anchor in ["2026-09-10T09:00:00.123456+23:59", "2026-09-10T09:00:00-23:59", "2026-09-10T09:00:00+00:00"] {
            var object = try F.object(F.manifest())
            object["identity"] = ["type": "after_completion", "anchor": anchor]
            object["window_start"] = "2026-09-12T15:00:00Z"; object["window_end"] = "2026-09-12T16:00:00Z"
            let value = try RoutineOccurrenceValidation.decode(RoutineOccurrenceManifest.self, from: F.data(object))
            #expect(value.identity == .afterCompletion(anchor: anchor))
        }
        for anchor in ["2026-09-10T09:00:00+24:00", "2026-09-10T09:00:00.1234567Z", "2026-02-30T09:00:00Z"] {
            #expect(!RoutineOccurrenceValidation.anchor(anchor))
        }
    }

    @Test("Foundation-only timezone aliases cannot authorize rolling or after-completion instances")
    func unsupportedTimezoneAliases() throws {
        let identities: [[String: Any]] = [
            ["type": "after_completion", "anchor": "2026-09-10T09:00:00Z"],
            ["type": "rolling_minutes", "index": 1, "anchor": "2026-09-10T09:00:00Z"],
        ]
        for identity in identities {
            for timezone in ["PST", "GMT+2"] {
                var object = try F.object(F.manifest())
                object["identity"] = identity
                object["timezone_name"] = timezone
                #expect(throws: RoutineOccurrenceError.invalidData) {
                    try RoutineOccurrenceValidation.decode(RoutineOccurrenceManifest.self, from: F.data(object))
                }
            }
            for timezone in ["UTC", "America/Los_Angeles"] {
                var object = try F.object(F.manifest())
                object["identity"] = identity
                object["timezone_name"] = timezone
                #expect(try RoutineOccurrenceValidation.decode(RoutineOccurrenceManifest.self, from: F.data(object)).isValid)
            }
        }
    }

    @Test("historical receipt binds ledger path, operation, target and both exact increments")
    func receiptBinding() throws {
        let mutation = F.mutation(), command = F.command()
        #expect(mutation.matches(instanceID: F.instanceID, memberID: F.rootID, command: command))
        #expect(!mutation.matches(instanceID: F.plannerID, memberID: F.rootID, command: command))
        #expect(!mutation.matches(instanceID: F.instanceID, memberID: F.childID, command: command))
        let overflow = RoutineOccurrenceCommand(schemaVersion: 1, operationID: F.operationID, expectedInstanceRevision: UInt64(Int64.max), expectedMemberRevision: 1,
            expectedEvidenceHash: F.hash, action: command.action)
        #expect(overflow.isValid && !mutation.matches(instanceID: F.instanceID, memberID: F.rootID, command: overflow))
        let selfBlocked = RoutineOccurrenceCommand(schemaVersion: 1, operationID: F.operationID, expectedInstanceRevision: 1, expectedMemberRevision: 1,
            expectedEvidenceHash: F.hash, action: .reopen(open: .init(status: .blocked, blockedReasonKind: .dependency, blockedByItemID: F.rootID)))
        #expect(!selfBlocked.isValid(for: F.rootID))
        let bytes = Data(" \n".utf8) + (try command.bytes()) + Data("\n ".utf8)
        #expect(try RoutineOccurrenceValidation.decode(RoutineOccurrenceCommand.self, from: bytes) == command)
    }

    @Test("pages are whole-instance, ordered and opaque with explicit terminal state")
    func pages() {
        #expect(F.page().isCurrentStatePage)
        let updates = RoutineOccurrencePage(schemaVersion: 1, changes: [.init(sequence: 1, occurrence: F.snapshot()),
            .init(sequence: 2, occurrence: F.snapshot(revision: 2, mode: .keepOpen))], cursor: "opaque-next", hasMore: true)
        #expect(updates.isValid && !updates.isCurrentStatePage)
        #expect(!RoutineOccurrencePage(schemaVersion: 1, changes: Array(updates.changes.reversed()), cursor: "opaque", hasMore: false).isValid)
        #expect(!RoutineOccurrencePage(schemaVersion: 1, changes: [], cursor: "opaque", hasMore: true).isValid)
        #expect(!RoutineOccurrencePage(schemaVersion: 1, changes: [], cursor: String(repeating: "x", count: 513), hasMore: false).isValid)
        #expect(RoutineOccurrencePage(schemaVersion: 1, changes: [], cursor: "opaque", hasMore: false).isValid)
    }

    @Test("large programmatic pages stop at eight MiB without encoding the whole page")
    func programmaticPageByteBudget() throws {
        let base = F.snapshot(count: 5_000)
        let source = base.aggregate.manifest
        #expect(base.isValid)
        let oneChangeBytes = try F.bytes(RoutineOccurrenceChange(sequence: 1, occurrence: base)).count
        #expect(oneChangeBytes > 1_000_000 && oneChangeBytes < RoutineOccurrenceValidation.maximumBytes)
        // Share immutable source/member arrays. The test itself never creates
        // the hundreds-of-megabytes encoded page whose allocation is prevented.
        let changes = (1...100).map { index -> RoutineOccurrenceChange in
            let manifest = RoutineOccurrenceManifest(schemaVersion: 1, id: F.id(20_000 + index),
                seriesItemID: source.seriesItemID,
                occurrenceID: UUID(uuidString: String(format: "00000000-0000-5000-8000-%012d", index))!,
                identity: source.identity, nominalStart: source.nominalStart, nominalEnd: source.nominalEnd,
                windowStart: source.windowStart, windowEnd: source.windowEnd, timezoneName: source.timezoneName,
                definitionHash: source.definitionHash, members: source.members)
            return .init(sequence: UInt64(index), occurrence: .init(schemaVersion: 1,
                aggregate: .init(manifest: manifest, revision: 1, members: base.aggregate.members),
                evidenceHash: base.evidenceHash, freshEditEligible: true, members: base.members))
        }
        let one = RoutineOccurrencePage(schemaVersion: 1, changes: [changes[0]], cursor: "opaque/checkpoint", hasMore: true)
        #expect(one.isValid)
        #expect(try F.bytes(one).count <= RoutineOccurrenceValidation.maximumBytes)
        let tooMany = RoutineOccurrencePage(schemaVersion: 1, changes: changes, cursor: "opaque/checkpoint", hasMore: false)
        #expect(!tooMany.isValid)
    }
}

/// Test-only slice reader. JSON grammar/duplicates are checked before indexing;
/// only metadata strings are decoded. A fixture value is never parsed/re-encoded
/// by Foundation, which otherwise changes `1.0` into an integer token.
private struct RoutineOccurrenceRawFixture {
    struct Entry {
        let name: String
        let kind: String
        let value: Data
    }
    let valid: [Entry]
    let invalid: [Entry]

    static func read(_ data: Data) throws -> Self {
        guard data.count <= 8 * 1_024 * 1_024, StrictJSONObjectKeyScanner.hasUniqueKeys(in: data) else {
            throw RoutineOccurrenceError.invalidData
        }
        let root = try Slices(data).object()
        guard Set(root.keys) == ["schema_version", "valid", "invalid"],
              let schema = root["schema_version"], try JSONDecoder().decode(Int.self, from: schema) == 1,
              let valid = root["valid"], let invalid = root["invalid"] else { throw RoutineOccurrenceError.invalidData }
        return try .init(valid: entries(valid), invalid: entries(invalid))
    }

    private static func entries(_ data: Data) throws -> [Entry] {
        try Slices(data).array().map { data in
            let fields = try Slices(data).object()
            guard Set(fields.keys) == ["name", "kind", "value"], let name = fields["name"],
                  let kind = fields["kind"], let value = fields["value"] else { throw RoutineOccurrenceError.invalidData }
            return try .init(name: JSONDecoder().decode(String.self, from: name),
                kind: JSONDecoder().decode(String.self, from: kind), value: value)
        }
    }

    private struct Slices {
        let bytes: [UInt8]
        var index = 0
        init(_ data: Data) { bytes = Array(data) }
        func object() throws -> [String: Data] { var copy = self; return try copy.readObject() }
        func array() throws -> [Data] { var copy = self; return try copy.readArray() }

        private mutating func readObject() throws -> [String: Data] {
            try consume(123)
            var result: [String: Data] = [:]
            skipWhitespace()
            if take(125) { return result }
            repeat {
                let key = try JSONDecoder().decode(String.self, from: value())
                try consume(58)
                result[key] = try value()
                skipWhitespace()
                if take(125) { return result }
                try consume(44)
            } while index < bytes.count
            throw RoutineOccurrenceError.invalidData
        }

        private mutating func readArray() throws -> [Data] {
            try consume(91)
            var result: [Data] = []
            skipWhitespace()
            if take(93) { return result }
            repeat {
                result.append(try value())
                skipWhitespace()
                if take(93) { return result }
                try consume(44)
            } while index < bytes.count
            throw RoutineOccurrenceError.invalidData
        }

        private mutating func value() throws -> Data {
            skipWhitespace()
            guard index < bytes.count else { throw RoutineOccurrenceError.invalidData }
            let start = index
            if bytes[index] == 34 {
                try string()
            } else if bytes[index] == 123 || bytes[index] == 91 {
                var depth = 0
                repeat {
                    guard index < bytes.count else { throw RoutineOccurrenceError.invalidData }
                    switch bytes[index] {
                    case 34: try string(); continue
                    case 123, 91: depth += 1
                    case 125, 93: depth -= 1
                    default: break
                    }
                    guard depth <= 64 else { throw RoutineOccurrenceError.invalidData }
                    index += 1
                } while depth > 0
            } else {
                while index < bytes.count, ![9, 10, 13, 32, 44, 93, 125].contains(bytes[index]) { index += 1 }
            }
            guard index > start else { throw RoutineOccurrenceError.invalidData }
            return Data(bytes[start..<index])
        }

        private mutating func string() throws {
            try consume(34)
            while index < bytes.count {
                let byte = bytes[index]; index += 1
                if byte == 34 { return }
                if byte == 92 {
                    guard index < bytes.count else { throw RoutineOccurrenceError.invalidData }
                    index += 1
                }
            }
            throw RoutineOccurrenceError.invalidData
        }
        private mutating func skipWhitespace() {
            while index < bytes.count, [9, 10, 13, 32].contains(bytes[index]) { index += 1 }
        }
        private mutating func take(_ byte: UInt8) -> Bool {
            guard index < bytes.count, bytes[index] == byte else { return false }
            index += 1; return true
        }
        private mutating func consume(_ byte: UInt8) throws {
            skipWhitespace()
            guard take(byte) else { throw RoutineOccurrenceError.invalidData }
        }
    }
}
#endif
