import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Encrypted schedule profile", .serialized)
@MainActor
struct ScheduleProfileTests {
    @Test("schema 12 derives the legacy profile and schema 13 rejects corrupt profile data")
    func migrationAndCorruption() throws {
        let savedAt = Self.date("2026-08-30T08:00:00Z")
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: "fixture",
            generatedAt: savedAt,
            asOf: savedAt,
            horizonStart: Self.date("2026-08-29T22:00:00Z"),
            horizonEnd: Self.date("2026-09-05T22:00:00Z"),
            timezoneName: "Europe/Madrid"
        )
        let injected = try ScheduleProfile.legacyDefault(
            timezoneName: "Asia/Tokyo",
            protectedFreeMinutes: 90
        )
        let schema12 = PlannerSnapshot(
            schemaVersion: 12,
            savedAt: savedAt,
            destination: .today,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "legacy",
            protectedFreeMinutes: 90,
            scheduleProfile: injected,
            freezeHours: 2,
            showCompleted: true,
            schedulePreviewProvenance: provenance
        )

        let migrated = try schema12.migratedToCurrentSchema()
        #expect(migrated.schemaVersion == PlannerSnapshot.currentSchemaVersion)
        #expect(migrated.scheduleProfile?.timezoneName == "Europe/Madrid")
        #expect(migrated.scheduleProfile?.protectedFreeMinutes == 90)
        let expectedLegacyWindow = try ScheduleLocalTimeWindow(
            start: ScheduleLocalTime(hour: 6, minute: 0),
            end: ScheduleLocalTime(hour: 21, minute: 30)
        )
        #expect(migrated.scheduleProfile?.availability.allSatisfy {
            $0.windows == [expectedLegacyWindow]
        } == true)

        let current = PlannerSnapshot(
            savedAt: savedAt,
            destination: .today,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "current",
            protectedFreeMinutes: 90,
            scheduleProfile: try ScheduleProfile.legacyDefault(
                timezoneName: "Europe/Madrid",
                protectedFreeMinutes: 90
            ),
            freezeHours: 2,
            showCompleted: true
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        var object = try #require(
            JSONSerialization.jsonObject(with: encoder.encode(current)) as? [String: Any]
        )
        var profileObject = try #require(object["scheduleProfile"] as? [String: Any])
        profileObject["timezoneName"] = "PST"
        object["scheduleProfile"] = profileObject
        let corrupt = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        #expect(throws: (any Error).self) {
            _ = try decoder.decode(PlannerSnapshot.self, from: corrupt)
        }
    }

    @Test("Madrid spring and fall horizons and overnight sleep use local calendar days")
    func madridDSTAndOvernightSleep() throws {
        let profile = try Self.profile(
            timezoneName: "Europe/Madrid",
            sleepStart: (22, 0),
            sleepEnd: (6, 0)
        )
        let spring = try profile.expanded(asOf: Self.date("2026-03-29T12:00:00Z"))
        #expect(spring.horizonStart == Self.date("2026-03-28T23:00:00Z"))
        #expect(spring.horizonEnd == Self.date("2026-04-04T22:00:00Z"))
        #expect(spring.horizonEnd.timeIntervalSince(spring.horizonStart) == 167 * 3_600)
        let springPriorSleep = try #require(spring.fixedBlocks.first {
            $0.source == "sleep" && $0.start < spring.horizonStart
        })
        #expect(springPriorSleep.start == Self.date("2026-03-28T21:00:00Z"))
        #expect(springPriorSleep.end == Self.date("2026-03-29T04:00:00Z"))
        #expect(springPriorSleep.end.timeIntervalSince(springPriorSleep.start) == 7 * 3_600)

        let fall = try profile.expanded(asOf: Self.date("2026-10-25T12:00:00Z"))
        #expect(fall.horizonStart == Self.date("2026-10-24T22:00:00Z"))
        #expect(fall.horizonEnd == Self.date("2026-10-31T23:00:00Z"))
        #expect(fall.horizonEnd.timeIntervalSince(fall.horizonStart) == 169 * 3_600)
        let fallPriorSleep = try #require(fall.fixedBlocks.first {
            $0.source == "sleep" && $0.start < fall.horizonStart
        })
        #expect(fallPriorSleep.start == Self.date("2026-10-24T20:00:00Z"))
        #expect(fallPriorSleep.end == Self.date("2026-10-25T05:00:00Z"))
        #expect(fallPriorSleep.end.timeIntervalSince(fallPriorSleep.start) == 9 * 3_600)

        let repeated = try profile.expanded(asOf: Self.date("2026-03-29T12:00:00Z"))
        #expect(repeated.fixedBlocks.map(\.id) == spring.fixedBlocks.map(\.id))
        #expect(spring.fixedBlocks.allSatisfy { $0.isSensitive })
        #expect(spring.fixedBlocks.allSatisfy { block in
            let bytes = block.id.uuid
            return (bytes.6 >> 4) == 8 && (bytes.8 & 0xc0) == 0x80
        })
        #expect(springPriorSleep.id.uuidString.lowercased()
            == "d032d368-3a96-8d57-b02a-7f2509a8bf7f")
    }

    @Test("fall-back overlap retains every sensitive wall-clock fixed interval")
    func fallBackAmbiguousProtectedBoundary() async throws {
        let sleepStart = try ScheduleLocalTime(hour: 2, minute: 30)
        let sleepEnd = try ScheduleLocalTime(hour: 1, minute: 0)
        let protectedWindow = try Self.window(1, 30, 2, 30)
        var availability: [ScheduleAvailabilityDay] = []
        var protectedTime: [ScheduleProtectedDay] = []
        for weekday in ScheduleWeekday.allCases {
            let isSunday = weekday == .sunday
            let availabilityEndHour = isSunday ? 1 : 2
            availability.append(try .init(
                weekday: weekday,
                isEnabled: true,
                windows: [try Self.window(1, 0, availabilityEndHour, 30)]
            ))
            protectedTime.append(try .init(
                weekday: weekday,
                isEnabled: isSunday,
                windows: isSunday ? [protectedWindow] : []
            ))
        }
        let profile = try ScheduleProfile(
            timezoneName: "Europe/Madrid",
            availability: availability,
            sleep: ScheduleSleepInterval(start: sleepStart, end: sleepEnd),
            protectedTime: protectedTime,
            defaultEnergy: .medium,
            contexts: [],
            location: nil
        )
        let asOf = Self.date("2026-10-24T22:00:00Z")
        let expanded = try profile.expanded(asOf: asOf)
        let protectedBlock = try #require(expanded.fixedBlocks.first {
            $0.source == "protected_time"
        })
        let expectedSleepStart = Self.date("2026-10-25T00:30:00Z")
        let sameDaySleep = try #require(expanded.fixedBlocks.first {
            $0.source == "sleep" && $0.start == expectedSleepStart
        })
        #expect(protectedBlock.start == Self.date("2026-10-24T23:30:00Z"))
        #expect(protectedBlock.end == Self.date("2026-10-25T01:30:00Z"))
        #expect(sameDaySleep.end == Self.date("2026-10-26T00:00:00Z"))
        #expect(protectedBlock.start < sameDaySleep.end)
        #expect(sameDaySleep.start < protectedBlock.end)
        #expect(protectedBlock.isSensitive)
        #expect(sameDaySleep.isSensitive)
        #expect(Set(expanded.fixedBlocks.map(\.id)).count == expanded.fixedBlocks.count)
        try CanonicalSyncStore.validateFixedBlockCoverage(
            returnedExternalBlockIDs: Set(expanded.fixedBlocks.map(\.id)),
            request: Self.request(from: expanded)
        )
        let repeated = try profile.expanded(asOf: asOf)
        #expect(repeated.fixedBlocks.map(\.id) == expanded.fixedBlocks.map(\.id))

        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let item = try Self.canonicalItem()
        let planner = PlannerStore(
            canonicalItems: [item],
            canonicalDeltaCursor: "complete",
            canonicalConfigurationIdentifier: "fixture",
            scheduleProfile: profile,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { asOf }
        )
        planner.flushPersistence()
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: ScheduleProfileConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: "profile-overlap-token"),
            session: URLProtocolStub.makeSession(),
            localComposer: ProfileOverlapComposer(),
            now: { asOf }
        )
        #expect(!(await sync.recomposeLocally()))
        #expect(planner.blocks.isEmpty)
        #expect(planner.localScheduleCompositionProvenance == nil)
    }

    @Test("inactive weekdays, multiple windows, normalization, and protected coverage expand exactly")
    func weekdayWindowsAndCoverage() throws {
        let mondayAvailability = try [
            Self.window(6, 0, 8, 0),
            Self.window(9, 0, 12, 0),
            Self.window(13, 0, 20, 0),
        ]
        let mondayProtected = try [
            Self.window(8, 0, 9, 0),
            Self.window(12, 0, 13, 0),
        ]
        var availability: [ScheduleAvailabilityDay] = []
        var protectedTime: [ScheduleProtectedDay] = []
        for weekday in ScheduleWeekday.allCases.reversed() {
            availability.append(try .init(
                weekday: weekday,
                isEnabled: weekday == .monday,
                windows: weekday == .monday ? mondayAvailability : []
            ))
            protectedTime.append(try .init(
                weekday: weekday,
                isEnabled: weekday == .monday,
                windows: weekday == .monday ? mondayProtected : []
            ))
        }
        let profile = try ScheduleProfile(
            timezoneName: "Europe/Madrid",
            availability: availability,
            sleep: ScheduleSleepInterval(
                start: ScheduleLocalTime(hour: 22, minute: 0),
                end: ScheduleLocalTime(hour: 6, minute: 0)
            ),
            protectedTime: protectedTime,
            defaultEnergy: .deep,
            contexts: [" Office  ", "DEEP\nWORK"],
            location: "  Home   desk "
        )
        #expect(profile.availability.map(\.weekday) == ScheduleWeekday.allCases)
        #expect(profile.contexts == ["deep work", "office"])
        #expect(profile.location == "Home desk")
        #expect(profile.protectedFreeMinutes == 0)

        let expanded = try profile.expanded(asOf: Self.date("2026-08-31T05:00:00Z"))
        #expect(expanded.availability.count == 3)
        #expect(expanded.availability[0].start == Self.date("2026-08-31T05:00:00Z"))
        #expect(expanded.availability.allSatisfy {
            $0.contexts == ["deep work", "office"]
                && $0.location == "Home desk"
                && $0.energy == "deep"
        })
        #expect(expanded.fixedBlocks.count(where: { $0.source == "protected_time" }) == 2)
        #expect(expanded.fixedBlocks.count(where: { $0.source == "sleep" }) == 8)
        try CanonicalSyncStore.validateFixedBlockCoverage(
            returnedExternalBlockIDs: Set(expanded.fixedBlocks.map(\.id)),
            request: Self.request(from: expanded)
        )
    }

    @Test("profile commits are fenced, transactional, and clear transient evidence only after CAS success")
    func transactionRollbackAndCommitBoundary() async throws {
        let now = Self.date("2026-08-30T08:00:00Z")
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let base = try Self.profile(timezoneName: "Europe/Madrid")
        let planner = PlannerStore(
            canonicalDeltaCursor: "complete",
            canonicalConfigurationIdentifier: "fixture",
            scheduleProfile: base,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        planner.flushPersistence()
        let composer = ProfileRecordingComposer()
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: ScheduleProfileConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: "profile-local-token"),
            session: URLProtocolStub.makeSession(),
            localComposer: composer,
            now: { now }
        )
        #expect(await sync.recomposeLocally())
        #expect(sync.lastLocalComposition != nil)
        let firstRequest = try #require(await composer.lastRequest())
        #expect(firstRequest.timezoneName == "Europe/Madrid")
        #expect(Set(firstRequest.fixedBlocks.map(\.source)) == ["sleep"])

        let lockedCandidate = try Self.copy(base, contexts: ["locked"])
        #expect(planner.beginCanonicalSync())
        #expect(throws: PlannerScheduleProfileError.mutationFenceActive) {
            try planner.updateScheduleProfile(
                lockedCandidate,
                expectedCurrentProfile: base
            )
        }
        planner.endCanonicalSync()
        #expect(sync.lastLocalComposition != nil)

        let committed = try Self.copy(base, contexts: ["home"])
        try planner.updateScheduleProfile(committed, expectedCurrentProfile: base)
        #expect(planner.scheduleProfile == committed)
        #expect(planner.localScheduleCompositionProvenance == nil)
        #expect(sync.lastPreview == nil)
        #expect(sync.lastLocalComposition == nil)
        #expect(sync.lastLocalCompositionScore == nil)
        #expect(sync.localCompositionWarnings.isEmpty)
        if case .ready = sync.localCompositionStatus {} else {
            Issue.record("A committed profile change must clear local transient status")
        }

        #expect(await sync.recomposeLocally())
        let installedBlocks = planner.blocks
        let installedProvenance = planner.localScheduleCompositionProvenance
        #expect(sync.lastLocalComposition != nil)
        let competing = PlannerStore(
            persistence: context.persistence,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        competing.freezeHours = 3
        competing.flushPersistence()
        #expect(competing.persistenceError == nil)

        let rejected = try Self.copy(committed, contexts: ["office"])
        #expect(throws: PlannerPersistenceError.concurrentModification) {
            try planner.updateScheduleProfile(
                rejected,
                expectedCurrentProfile: committed
            )
        }
        #expect(planner.scheduleProfile == committed)
        #expect(planner.blocks == installedBlocks)
        #expect(planner.localScheduleCompositionProvenance == installedProvenance)
        #expect(sync.lastLocalComposition != nil)
        #expect(sync.lastLocalCompositionScore != nil)
    }

    @Test("server preview and local helper receive the same persisted profile expansion")
    func serverAndLocalRequestsUseProfile() async throws {
        let now = Self.date("2026-08-30T08:00:00Z")
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let profile = try Self.copy(
            Self.profile(timezoneName: "Europe/Madrid"),
            contexts: ["deep work"],
            location: "Madrid"
        )
        let planner = PlannerStore(
            scheduleProfile: profile,
            persistence: context.persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        planner.flushPersistence()
        let token = "profile-server-token"
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"complete","has_more":false}"#.utf8)
            ),
            .init(
                statusCode: 500,
                body: Data(#"{"error":{"code":"fixture","message":"stop after capture"}}"#.utf8)
            )
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: ScheduleProfileConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: ProfileRecordingComposer(),
            now: { now }
        )

        await sync.sync()

        let previewRequest = try #require(URLProtocolStub.storage.requests(for: token).first {
            $0.url.path.hasSuffix("/v1/schedule/preview")
        })
        let body = try #require(previewRequest.jsonBody)
        #expect(body["timezone_name"] as? String == "Europe/Madrid")
        let availability = try #require(body["availability"] as? [[String: Any]])
        #expect(availability.first?["contexts"] as? [String] == ["deep work"])
        #expect(availability.first?["location"] as? String == "Madrid")
        let fixed = try #require(body["fixed_blocks"] as? [[String: Any]])
        #expect(!fixed.isEmpty)
        #expect(Set(fixed.compactMap { $0["source"] as? String }) == ["sleep"])
    }

    private static func request(
        from expanded: ExpandedScheduleProfile
    ) -> DayWeaveSchedulePreviewRequest {
        .init(
            asOf: expanded.asOf,
            horizonStart: expanded.horizonStart,
            horizonEnd: expanded.horizonEnd,
            timezoneName: expanded.timezoneName,
            availability: expanded.availability,
            fixedBlocks: expanded.fixedBlocks,
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )
    }

    private static func profile(
        timezoneName: String,
        sleepStart: (Int, Int) = (23, 0),
        sleepEnd: (Int, Int) = (6, 0)
    ) throws -> ScheduleProfile {
        let sleepStart = try ScheduleLocalTime(hour: sleepStart.0, minute: sleepStart.1)
        let sleepEnd = try ScheduleLocalTime(hour: sleepEnd.0, minute: sleepEnd.1)
        let window = try ScheduleLocalTimeWindow(start: sleepEnd, end: sleepStart)
        var availability: [ScheduleAvailabilityDay] = []
        var protectedTime: [ScheduleProtectedDay] = []
        for weekday in ScheduleWeekday.allCases {
            availability.append(try .init(
                weekday: weekday,
                isEnabled: true,
                windows: [window]
            ))
            protectedTime.append(try .init(
                weekday: weekday,
                isEnabled: false,
                windows: []
            ))
        }
        return try ScheduleProfile(
            timezoneName: timezoneName,
            availability: availability,
            sleep: ScheduleSleepInterval(start: sleepStart, end: sleepEnd),
            protectedTime: protectedTime,
            defaultEnergy: .medium,
            contexts: [],
            location: nil
        )
    }

    private static func copy(
        _ profile: ScheduleProfile,
        contexts: [String],
        location: String? = nil
    ) throws -> ScheduleProfile {
        try ScheduleProfile(
            timezoneName: profile.timezoneName,
            availability: profile.availability,
            sleep: profile.sleep,
            protectedTime: profile.protectedTime,
            defaultEnergy: profile.defaultEnergy,
            contexts: contexts,
            location: location ?? profile.location
        )
    }

    private static func window(
        _ startHour: Int,
        _ startMinute: Int,
        _ endHour: Int,
        _ endMinute: Int
    ) throws -> ScheduleLocalTimeWindow {
        try .init(
            start: ScheduleLocalTime(hour: startHour, minute: startMinute),
            end: ScheduleLocalTime(hour: endHour, minute: endMinute)
        )
    }

    private static func persistenceContext() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveScheduleProfile-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: PlannerEncryptionKey.random()
            )
        )
    }

    private static func date(_ value: String) -> Date {
        guard let date = ISO8601DateFormatter().date(from: value) else {
            preconditionFailure("Invalid fixed ISO-8601 fixture: \(value)")
        }
        return date
    }

    private static func canonicalItem() throws -> DayWeaveCanonicalItem {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data("""
        {"id":"f1000000-0000-4000-8000-000000000001","is_sensitive":true,
        "kind":"task","status":"scheduled","title":"Must not overlap protection",
        "notes":null,"timezone_name":"Europe/Madrid","duration_seconds":900,
        "deadline_at":null,"earliest_start_at":null,"recurrence":null,
        "flexible_constraints":{},"split_policy":{"type":"indivisible"},
        "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
        "is_executable":true,"revision":1,"created_at":"2026-10-24T22:00:00Z",
        "updated_at":"2026-10-24T22:00:00Z","completed_at":null,"deleted_at":null}
        """.utf8))
    }
}

private struct ScheduleProfileConfigurationStore: SuggestionAPIConfigurationStoring {
    func loadBaseURL() -> String? { "https://api.example.com/gateway" }
    func saveBaseURL(_ value: String) {}
}

private actor ProfileRecordingComposer: LocalScheduleComposing {
    private var request: DayWeaveSchedulePreviewRequest?

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        request = schedule
        let fixed = schedule.fixedBlocks
            .filter { $0.end > schedule.horizonStart && $0.start < schedule.horizonEnd }
            .map { block in
                DayWeaveSchedulePreview.Plan.Block(
                    id: block.id,
                    isSensitive: block.isSensitive,
                    itemID: nil,
                    occurrenceID: nil,
                    externalBlockID: block.id,
                    title: block.title,
                    start: block.start,
                    end: block.end,
                    sessionIndex: 0,
                    kind: "external_fixed",
                    explanations: [.init(code: block.source, message: "Schedule profile")]
                )
            }
        return LocalScheduleComposition(
            localInputFingerprint: "local-sha256:\(String(repeating: "c", count: 64))",
            sourceItemCount: canonicalItems.count,
            sourceItemRevisions: Dictionary(
                uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
            ),
            acceptedItemCount: canonicalItems.count,
            rejectedItems: [],
            ignoredPreviousAssignments: [],
            plan: .init(
                asOf: schedule.asOf,
                horizonStart: schedule.horizonStart,
                horizonEnd: schedule.horizonEnd,
                blocks: fixed,
                unscheduled: [],
                decisions: [],
                violations: [],
                score: .init(
                    scheduledMinutes: 0,
                    unscheduledMinutes: 0,
                    softPenalty: 0,
                    movedMinutes: 0
                ),
                occurrences: []
            )
        )
    }

    func lastRequest() -> DayWeaveSchedulePreviewRequest? { request }
}

private struct ProfileOverlapComposer: LocalScheduleComposing {
    private static let plannedBlockID = UUID(
        uuidString: "f2000000-0000-4000-8000-000000000002"
    )!

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        let item = try #require(canonicalItems.first)
        let protectedBlock = try #require(schedule.fixedBlocks.first {
            $0.source == "protected_time"
        })
        let overlappingSleep = try #require(schedule.fixedBlocks.first {
            $0.source == "sleep"
                && $0.start < protectedBlock.end
                && protectedBlock.start < $0.end
        })
        let overlapStart = max(protectedBlock.start, overlappingSleep.start)
        let overlapEnd = min(protectedBlock.end, overlappingSleep.end)
        let plannedStart = overlapStart.addingTimeInterval(5 * 60)
        let plannedEnd = min(overlapEnd, plannedStart.addingTimeInterval(15 * 60))
        let fixed = schedule.fixedBlocks.map { block in
            DayWeaveSchedulePreview.Plan.Block(
                id: block.id,
                isSensitive: block.isSensitive,
                itemID: nil,
                occurrenceID: nil,
                externalBlockID: block.id,
                title: block.title,
                start: block.start,
                end: block.end,
                sessionIndex: 0,
                kind: "external_fixed",
                explanations: [.init(code: block.source, message: "Schedule profile")]
            )
        }
        let planned = DayWeaveSchedulePreview.Plan.Block(
            id: Self.plannedBlockID,
            isSensitive: true,
            itemID: item.id,
            occurrenceID: nil,
            externalBlockID: nil,
            title: item.title,
            start: plannedStart,
            end: plannedEnd,
            sessionIndex: 0,
            kind: "planned",
            explanations: [.init(code: "invalid-overlap", message: "Fixture")]
        )
        let blocks = (fixed + [planned]).sorted {
            if $0.start != $1.start { return $0.start < $1.start }
            if $0.end != $1.end { return $0.end < $1.end }
            return $0.id.uuidString < $1.id.uuidString
        }
        return LocalScheduleComposition(
            localInputFingerprint: "local-sha256:\(String(repeating: "d", count: 64))",
            sourceItemCount: 1,
            sourceItemRevisions: [item.id: item.revision],
            acceptedItemCount: 1,
            rejectedItems: [],
            ignoredPreviousAssignments: [],
            plan: .init(
                asOf: schedule.asOf,
                horizonStart: schedule.horizonStart,
                horizonEnd: schedule.horizonEnd,
                blocks: blocks,
                unscheduled: [],
                decisions: [],
                violations: [],
                score: .init(
                    scheduledMinutes: UInt32(plannedEnd.timeIntervalSince(plannedStart) / 60),
                    unscheduledMinutes: 0,
                    softPenalty: 0,
                    movedMinutes: 0
                ),
                occurrences: []
            )
        )
    }
}
#endif
