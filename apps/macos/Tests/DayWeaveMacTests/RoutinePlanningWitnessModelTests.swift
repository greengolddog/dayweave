import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

enum RoutinePlanningWitnessTestFixtures {
    static let rootID = UUID(uuidString: "00000000-0000-4000-8000-000000000001")!
    static let leafID = UUID(uuidString: "00000000-0000-4000-8000-000000000002")!
    static let inboxID = UUID(uuidString: "00000000-0000-4000-8000-000000000003")!
    static let occurrenceID = UUID(uuidString: "00000000-0000-5000-8000-000000000004")!
    static let cursor = "DWR1.synthetic-terminal"
    static func data(_ value: Any) throws -> Data { try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys, .fragmentsAllowed]) }
    static func schedule() -> [String: Any] {
        ["as_of": "2026-09-10T09:00:00.123456Z", "horizon_start": "2026-09-10T00:00:00Z",
         "horizon_end": "2026-09-11T00:00:00Z", "timezone_name": "UTC", "availability": [],
         "fixed_blocks": [], "previous_assignments": [], "manual_placements": [], "manual_placement_releases": [],
         "config": ["slot_granularity_minutes": 5, "stability_weight": 4, "default_soft_weight": 100],
         "recurrence_context": ["calendar": ["time_zone_id": NSNull(), "week_starts_on": "monday", "days": []],
            "completion_anchors": [:], "rolling_anchors": [:], "minimum_spacing": [:],
            "completed_occurrence_ids": [], "pauses": [], "exceptions": []]]
    }
    static func revisions() -> [String: UInt64] { [rootID.uuidString.lowercased(): 4, leafID.uuidString.lowercased(): 7, inboxID.uuidString.lowercased(): 9] }
    static func requestObject() -> [String: Any] {
        ["schema_version": 1, "schedule": schedule(), "expected_source_item_revisions": revisions(), "terminal_cursor": cursor]
    }
    static func witnessObject(head: UInt64 = 7, empty: Bool = false) -> [String: Any] {
        let instance: [String: Any] = ["root_item_id": rootID.uuidString.lowercased(), "occurrence_id": occurrenceID.uuidString.lowercased(),
            "identity": ["type": "calendar_day", "date": "2026-09-10", "bucket_ordinal": 0],
            "members": [(rootID, nil as UUID?, UInt64(4), "not_started"), (leafID, rootID, UInt64(7), "completed"), (inboxID, rootID, UInt64(9), "not_started")].map { id, parent, revision, status in
                ["item_id": id.uuidString.lowercased(), "parent_id": parent?.uuidString.lowercased() as Any? ?? NSNull(), "source_revision": revision, "status": status] as [String: Any]
            }]
        return ["workspace_id": "00000000-0000-4000-8000-000000000010", "user_id": "00000000-0000-4000-8000-000000000011",
            "request_fingerprint": "routine-witness-request-sha256:" + String(repeating: "a", count: 64),
            "witness_fingerprint": "routine-witness-capture-sha256:" + String(repeating: "b", count: 64),
            "local_input_fingerprint": "local-sha256:" + String(repeating: "c", count: 64),
            "calendar_projection_fingerprint": "routine-witness-calendar-sha256:" + String(repeating: "d", count: 64),
            "source_item_revisions": revisions(), "terminal_cursor": cursor, "schedule": schedule(),
            "occurrence_lifecycle": ["snapshot_revision": head, "instances": empty ? [] : [instance]],
            "execution_snapshot_revision": 3, "habit_change_head": 5, "published_schedule_revision_id": NSNull()]
    }
    static func request() throws -> RoutinePlanningWitnessRequest {
        try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessRequest.self, from: data(requestObject()))
    }
    static func witness(head: UInt64 = 7, empty: Bool = false) throws -> RoutinePlanningWitness {
        try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: data(witnessObject(head: head, empty: empty)))
    }
    static func response() -> [String: Any] { ["schema_version": 1, "result": ["status": "qualified", "witness": witnessObject()]] }
    static func canonicalItems() throws -> [DayWeaveCanonicalItem] {
        let items = [rootID, leafID, inboxID].map { id -> [String: Any] in
            ["id": id.uuidString.lowercased(), "is_sensitive": false,
             "kind": id == rootID ? "routine" : "task", "status": id == inboxID ? "inbox" : "planned",
             "title": "Synthetic private routine", "notes": NSNull(), "timezone_name": "UTC",
             "duration_seconds": id == rootID ? NSNull() : 300, "deadline_at": NSNull(), "earliest_start_at": NSNull(),
             "recurrence": id == rootID ? ["type": "daily", "times_per_day": 1] : NSNull(), "flexible_constraints": [:],
             "split_policy": ["type": "indivisible"], "importance": 1, "urgency": 1,
             "parent_id": id == rootID ? NSNull() : rootID.uuidString.lowercased(), "sibling_order": 0,
             "is_executable": id != rootID, "revision": revisions()[id.uuidString.lowercased()]!,
             "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-10T00:00:00Z", "completed_at": NSNull(), "deleted_at": NSNull()]
        }
        return try SchedulerHelperClient.decoder.decode([DayWeaveCanonicalItem].self, from: data(items))
    }
}

@Suite("Authenticated routine planning witness wire")
struct RoutinePlanningWitnessModelTests {
    private typealias F = RoutinePlanningWitnessTestFixtures

    @Test("current-source joins include omitted Inbox and do not use first-source revisions")
    func completeJoin() throws {
        let witness = try F.witness()
        try witness.requireMatches(F.request()); try witness.validate(canonicalItems: F.canonicalItems())
        var sources = try F.canonicalItems(); sources.removeLast()
        #expect(throws: RoutinePlanningWitnessError.invalidData) { try witness.validate(canonicalItems: sources) }
        var stale = F.witnessObject(); var revisions = F.revisions(); revisions[F.inboxID.uuidString.lowercased()] = 1
        stale["source_item_revisions"] = revisions
        #expect(throws: RoutinePlanningWitnessError.invalidData) {
            try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: F.data(stale))
        }
    }

    @Test("positive empty heads remain v2 evidence")
    func emptyHead() throws {
        #expect(try F.witness(head: 13, empty: true).occurrenceLifecycle.snapshotRevision == 13)
        #expect(throws: RoutinePlanningWitnessError.invalidData) { try F.witness(head: 0) }
        #expect(try F.witness(head: 0, empty: true).occurrenceLifecycle.instances.isEmpty)
    }

    @Test("all remote reasons contain no witness")
    func remoteReasons() throws {
        for reason in RoutinePlanningRemoteReason.allCases {
            let response = RoutinePlanningWitnessResponse(result: .remoteRequired(reason))
            #expect(try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: RoutinePlanningWitnessValidation.encode(response)) == response)
        }
        let raw: [String: Any] = ["schema_version": 1, "result": ["status": "remote_required", "reason": "source_ineligible", "witness": F.witnessObject()]]
        #expect(throws: RoutinePlanningWitnessError.invalidData) { try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: F.data(raw)) }
    }

    @Test("exact numeric tokens, UUID aliases, hashes and closed fields are required")
    func strictTokens() throws {
        let raw = String(decoding: try F.data(F.response()), as: UTF8.self)
        for replacement in ["\"schema_version\":1.0", "\"schema_version\":1e0", "\"schema_version\":1,\"schema_versi\\u006fn\":1"] {
            let changed = raw.replacingOccurrences(of: "\"schema_version\":1", with: replacement)
            #expect(throws: RoutinePlanningWitnessError.invalidData) { try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: Data(changed.utf8)) }
        }
        for key in ["workspace_id", "user_id", "published_schedule_revision_id"] {
            var changed = F.witnessObject(); changed[key] = "00000000-0000-0000-0000-000000000000"
            #expect(throws: RoutinePlanningWitnessError.invalidData) { try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: F.data(changed)) }
        }
        for key in ["request_fingerprint", "witness_fingerprint", "local_input_fingerprint", "calendar_projection_fingerprint"] {
            var changed = F.witnessObject(); changed[key] = "sha256:" + String(repeating: "a", count: 64)
            #expect(throws: RoutinePlanningWitnessError.invalidData) { try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: F.data(changed)) }
        }
    }

    @Test("only documented normalization fields may change")
    func normalization() throws {
        let request = try F.request()
        for key in ["timezone_name", "config", "calendar", "rolling_anchors", "minimum_spacing"] {
            var wire = F.witnessObject(), schedule = F.schedule()
            if key == "timezone_name" { schedule[key] = "Europe/London" }
            else if key == "config" { schedule[key] = ["slot_granularity_minutes": 5, "stability_weight": 8, "default_soft_weight": 100] }
            else {
                var context = schedule["recurrence_context"] as! [String: Any]
                if key == "calendar" { context[key] = ["time_zone_id": "UTC", "week_starts_on": "monday", "days": []] }
                else { context[key] = [F.rootID.uuidString.lowercased(): key == "rolling_anchors" ? "2026-09-10T00:00:00Z" as Any : 10] }
                schedule["recurrence_context"] = context
            }
            wire["schedule"] = schedule
            let changed = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: F.data(wire))
            #expect(throws: RoutinePlanningWitnessError.invalidData) { try changed.requireMatches(request) }
        }
        var wire = F.witnessObject(), schedule = F.schedule()
        schedule["as_of"] = "2026-09-10T11:00:00.123456+02:00"
        wire["schedule"] = schedule
        try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: F.data(wire)).requireMatches(request)
        let encoded = try RoutinePlanningWitnessValidation.encode(F.witness())
        #expect(String(decoding: encoded, as: UTF8.self).contains("2026-09-10T09:00:00.123456Z"))
    }

    @Test("logical depth is flat and cycles or missing parents never qualify")
    func deepTree() throws {
        let members = (1...5_000).map { index in
            RoutinePlanningLifecycleMember(itemID: UUID(uuidString: String(format: "00000000-0000-4000-8000-%012d", index))!,
                parentID: index == 1 ? nil : UUID(uuidString: String(format: "00000000-0000-4000-8000-%012d", index - 1))!,
                sourceRevision: 1, status: .notStarted)
        }
        let tree = RoutinePlanningLifecycleInstance(rootItemID: F.rootID, occurrenceID: F.occurrenceID,
            identity: .calendarDay(date: "2026-09-10", bucketOrdinal: 0), members: members)
        try tree.validate()
        let missing = RoutinePlanningLifecycleInstance(rootItemID: F.rootID, occurrenceID: F.occurrenceID, identity: tree.identity, members: Array(members.dropFirst()))
        #expect(throws: RoutinePlanningWitnessError.invalidData) { try missing.validate() }
    }

    @Test("shared producer corpus joins exact request, scope, source and lifecycle evidence")
    func sharedCorpus() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/routine-planning-witness/wire-v1.json"))
        let corpus = try RoutinePlanningCorpusValue(data: data).object()
        let qualified = try #require(corpus["qualified"]).array()
        var bases: [String: (RoutinePlanningWitnessRequest, RoutinePlanningWitness, [DayWeaveCanonicalItem])] = [:]
        for rawItem in qualified {
            let item = try rawItem.object()
            let name = try #require(item["name"]).string()
            let request = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessRequest.self, from: #require(item["request"]).data)
            let response = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: #require(item["response"]).data)
            guard case let .qualified(witness) = response.result else { Issue.record("Producer case must qualify"); continue }
            let sources = try SchedulerHelperClient.decoder.decode([DayWeaveCanonicalItem].self, from: #require(item["canonical_items"]).data)
            try witness.requireMatches(request); try witness.validate(canonicalItems: sources)
            bases[name] = (request, witness, sources)
        }
        #expect(bases.count == qualified.count && !bases.isEmpty)
        for rawRemote in try #require(corpus["remote_required"]).array() {
            let remote = try rawRemote.object()
            let response = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self,
                from: #require(remote["response"]).data)
            guard case .remoteRequired = response.result else { Issue.record("Producer remote case must not qualify"); continue }
        }
        for rawInvalid in try #require(corpus["invalid"]).array() {
            let invalid = try rawInvalid.object()
            let name = try #require(invalid["name"]).string(), baseName = try #require(invalid["base"]).string()
            let base = try #require(bases[baseName]), bytes = try #require(invalid["response"]).data
            #expect(throws: RoutinePlanningWitnessError.invalidData, "\(name)") {
                let response = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: bytes)
                guard case let .qualified(witness) = response.result else { throw RoutinePlanningWitnessError.invalidData }
                try witness.requireMatches(base.0, expectedWorkspaceID: base.1.workspaceID, expectedUserID: base.1.userID)
                try witness.validate(canonicalItems: base.2)
            }
        }
        for rawInvalid in try #require(corpus["raw_invalid"]).array() {
            let invalid = try rawInvalid.object(), raw = try #require(invalid["raw"]).string()
            #expect(throws: RoutinePlanningWitnessError.invalidData) {
                try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: Data(raw.utf8))
            }
        }
    }
}
#endif
