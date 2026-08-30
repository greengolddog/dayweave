import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Occurrence-scoped Will do later", .serialized)
@MainActor
struct RecurrenceMoveTests {
    nonisolated private static let itemID = UUID(uuidString: "81000000-0000-4000-8000-000000000001")!
    nonisolated private static let childOneID = UUID(uuidString: "81000000-0000-4000-8000-000000000011")!
    nonisolated private static let childTwoID = UUID(uuidString: "81000000-0000-4000-8000-000000000012")!
    nonisolated private static let occurrenceID = UUID(uuidString: "5432cf9b-22b0-56ff-ba43-2d71e23eb904")!
    nonisolated private static let configuration = "https://api.example.test"
    nonisolated private static let now = Date(timeIntervalSince1970: 1_800_000_000)

    @Test("a split occurrence move stays anchored to the tapped session and survives restart")
    func splitMoveIsOccurrenceScopedDurableAndHorizonBounded() throws {
        let context = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let profile = PlannerStore(restoreFromPersistence: false).scheduleProfile
        let horizon = try profile.expanded(asOf: Self.now)
        let item = try Self.recurringItem()
        let first = Self.block(
            id: UUID(uuidString: "83000000-0000-4000-8000-000000000003")!,
            sessionIndex: 0,
            start: Self.now.addingTimeInterval(3_600)
        )
        let second = Self.block(
            id: UUID(uuidString: "84000000-0000-4000-8000-000000000004")!,
            sessionIndex: 1,
            start: Self.now.addingTimeInterval(5_400)
        )
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: Self.configuration,
            generatedAt: Self.now,
            asOf: Self.now,
            horizonStart: horizon.horizonStart,
            horizonEnd: horizon.horizonEnd,
            timezoneName: profile.timezoneName
        )
        let store = PlannerStore(
            blocks: [first, second],
            canonicalItems: [item],
            canonicalConfigurationIdentifier: Self.configuration,
            schedulePreviewProvenance: provenance,
            scheduleProfile: profile,
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )
        let chosenStart = second.start.addingTimeInterval(7_200)

        let move = try store.enqueueCanonicalOccurrenceMove(
            blockID: second.id,
            moveStart: chosenStart
        )

        #expect(move.startAt == first.start.addingTimeInterval(7_200))
        #expect(move.endAt == second.end.addingTimeInterval(7_200))
        #expect(move.source == Self.source)
        #expect(store.blocks == [first, second])
        #expect(store.canonicalItem(id: Self.itemID)?.earliestStartAt == nil)
        #expect(store.pendingCanonicalAuthoringMutations.isEmpty)
        #expect(store.publishedScheduleProof == nil)
        let restored = PlannerStore.live(persistence: context.persistence)
        #expect(restored.persistenceError == nil)
        #expect(restored.recurrenceOccurrenceMoves == [move])

        let outsideContext = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: outsideContext.directory) }
        let outside = PlannerStore(
            blocks: [first, second], canonicalItems: [item],
            canonicalConfigurationIdentifier: Self.configuration,
            schedulePreviewProvenance: provenance, scheduleProfile: profile,
            previewValidatedForCurrentLaunch: true,
            persistence: outsideContext.persistence, restoreFromPersistence: false,
            now: { Self.now }
        )
        let futureStart = horizon.horizonEnd.addingTimeInterval(3_600)
        let futureMove = try outside.enqueueCanonicalOccurrenceMove(
            blockID: second.id,
            moveStart: futureStart
        )
        let futureHorizon = try profile.expanded(asOf: futureMove.startAt)
        #expect(futureMove.startAt > horizon.horizonEnd)
        #expect(futureMove.startAt >= futureHorizon.horizonStart)
        #expect(futureMove.endAt <= futureHorizon.horizonEnd)
    }

    @Test("canonical preview emits the exact occurrence move exception")
    func previewCarriesExactMoveException() async throws {
        let context = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let token = "recurrence-move-preview-token"
        let baseURL = try DayWeaveAPIBaseURL("https://api.example.com/gateway")
        let configuration = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        ).configurationIdentifier
        let profile = PlannerStore(restoreFromPersistence: false).scheduleProfile
        let horizon = try profile.expanded(asOf: Self.now)
        let currentHorizon = try profile.expanded(asOf: Self.now)
        let move = RecurrenceOccurrenceMove(
            itemID: Self.itemID,
            occurrenceID: Self.occurrenceID,
            startAt: currentHorizon.horizonEnd.addingTimeInterval(7_200),
            endAt: currentHorizon.horizonEnd.addingTimeInterval(9_000),
            movedAt: Self.now,
            source: Self.source
        )
        let planner = PlannerStore(
            canonicalItems: [try Self.recurringItem()],
            canonicalDeltaCursor: "move-before",
            recurrenceOccurrenceMoves: [move],
            canonicalConfigurationIdentifier: configuration,
            scheduleProfile: profile,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"move-after","has_more":false}"#.utf8
                )
            ),
            .init(
                statusCode: 500,
                body: Data(#"{"error":{"code":"fixture_stop","message":"stop after request"}}"#.utf8)
            )
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: RecurrenceMoveAPIConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            now: { Self.now }
        )

        await sync.sync()

        let preview = try #require(URLProtocolStub.storage.requests(for: token).first {
            $0.url.path == "/gateway/v1/schedule/preview"
        })
        let body = try #require(preview.jsonBody)
        let recurrence = try #require(body["recurrence_context"] as? [String: Any])
        let exceptions = try #require(recurrence["exceptions"] as? [[String: Any]])
        let exception = try #require(exceptions.first)
        #expect(exception["item_id"] as? String == Self.itemID.uuidString.lowercased())
        let selector = try #require(exception["selector"] as? [String: Any])
        #expect(selector["id"] as? String == Self.occurrenceID.uuidString.lowercased())
        let action = try #require(exception["action"] as? [String: Any])
        #expect(action["type"] as? String == "move")
        #expect(action["start"] as? String == Self.format(move.startAt))
        #expect(action["end"] as? String == Self.format(move.endAt))
        let source = try #require(action["source"] as? [String: Any])
        #expect(source["item_revision"] as? Int == 1)
        let identity = try #require(source["identity"] as? [String: Any])
        #expect(identity["type"] as? String == "calendar_day")
        #expect(identity["date"] as? String == "2027-01-15")
        #expect(identity["bucket_ordinal"] as? Int == 0)
        #expect(source["nominal_start"] as? String == Self.source.nominalStart)
        #expect(source["nominal_end"] as? String == Self.source.nominalEnd)
        #expect(source["local_date"] as? String == "2027-01-15")
        #expect(source["ordinal"] as? Int == 0)
        #expect(move.startAt > horizon.horizonEnd)
    }

    @Test("a series edit durably prunes its stale move before another preview")
    func staleSourceRevisionIsDurablyPruned() throws {
        let context = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let profile = PlannerStore(restoreFromPersistence: false).scheduleProfile
        let horizon = try profile.expanded(asOf: Self.now)
        let move = RecurrenceOccurrenceMove(
            itemID: Self.itemID,
            occurrenceID: Self.occurrenceID,
            startAt: horizon.horizonStart.addingTimeInterval(7_200),
            endAt: horizon.horizonStart.addingTimeInterval(9_000),
            movedAt: Self.now,
            source: Self.source
        )
        let planner = PlannerStore(
            canonicalItems: [try Self.recurringItem()],
            canonicalDeltaCursor: "stale-before",
            recurrenceOccurrenceMoves: [move],
            canonicalConfigurationIdentifier: Self.configuration,
            scheduleProfile: profile,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )
        planner.flushPersistence()
        planner.applyCanonicalDelta(
            [.upsert(try Self.recurringItem(revision: 2))],
            nextCursor: "series-revision-2"
        )

        #expect(planner.recurrenceOccurrenceMoves.isEmpty)
        #expect(planner.publishedScheduleProof == nil)
        #expect(planner.lastScheduleMessage.contains("obsolete recurring move"))
        let restored = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(restored.persistenceError == nil)
        #expect(restored.canonicalItem(id: Self.itemID)?.revision == 2)
        #expect(restored.recurrenceOccurrenceMoves.isEmpty)
    }

    @Test("a future cross-horizon move survives intervening days then expires durably")
    func occurrenceMoveExpiresAfterTargetHorizon() throws {
        let context = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let profile = PlannerStore(restoreFromPersistence: false).scheduleProfile
        let move = RecurrenceOccurrenceMove(
            itemID: Self.itemID,
            occurrenceID: Self.occurrenceID,
            startAt: Self.now.addingTimeInterval(10 * 86_400),
            endAt: Self.now.addingTimeInterval(10 * 86_400 + 1_800),
            movedAt: Self.now,
            source: Self.source
        )
        let first = PlannerStore(
            canonicalItems: [try Self.recurringItem()],
            recurrenceOccurrenceMoves: [move],
            canonicalConfigurationIdentifier: Self.configuration,
            scheduleProfile: profile,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )
        first.flushPersistence()

        let intervening = PlannerStore(
            persistence: context.persistence,
            now: { move.startAt.addingTimeInterval(-2 * 86_400) }
        )
        #expect(intervening.recurrenceOccurrenceMoves == [move])

        let after = move.endAt.addingTimeInterval(2 * 86_400)
        let expired = PlannerStore(persistence: context.persistence, now: { after })
        #expect(expired.recurrenceOccurrenceMoves.isEmpty)
        let relaunched = PlannerStore(persistence: context.persistence, now: { after })
        #expect(relaunched.persistenceError == nil)
        #expect(relaunched.recurrenceOccurrenceMoves.isEmpty)
    }

    @Test("published preview occurrence metadata is retained exactly for a later move")
    func publishedPreviewRetainsExactOccurrenceSource() async throws {
        let context = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let token = "recurrence-source-render-token"
        let baseURL = try DayWeaveAPIBaseURL("https://api.example.com/gateway")
        let configuration = DayWeaveAPIClient(
            baseURL: baseURL,
            session: URLProtocolStub.makeSession(),
            bearerToken: token
        ).configurationIdentifier
        let profile = try ScheduleProfile.legacyDefault(
            timezoneName: "UTC",
            protectedFreeMinutes: 0
        )
        let expanded = try profile.expanded(asOf: Self.now)
        let firstStart = Self.now.addingTimeInterval(3_600)
        let firstEnd = firstStart.addingTimeInterval(900)
        let secondStart = firstEnd.addingTimeInterval(900)
        let secondEnd = secondStart.addingTimeInterval(900)
        let nominalStart = Self.preciseTimestamp(firstStart, fraction: "123456")
        let nominalEnd = Self.preciseTimestamp(firstStart.addingTimeInterval(1_800), fraction: "123456")
        let localDate = String(nominalStart.prefix(10))
        let fixedBlocks = expanded.fixedBlocks.map { fixed in
            """
            {"id":"\(fixed.id.uuidString.lowercased())","is_sensitive":\(fixed.isSensitive),
             "item_id":null,"occurrence_id":null,"external_block_id":"\(fixed.id.uuidString.lowercased())",
             "title":"\(fixed.title)","start":"\(Self.format(fixed.start))","end":"\(Self.format(fixed.end))",
             "session_index":0,"kind":"external_fixed","explanations":[]}
            """
        }.joined(separator: ",")
        let fixedSuffix = fixedBlocks.isEmpty ? "" : ",\(fixedBlocks)"
        let preview = """
        {"input_digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
         "source_item_count":3,"accepted_item_count":3,
         "source_item_revisions":{"\(Self.itemID.uuidString.lowercased())":1,
          "\(Self.childOneID.uuidString.lowercased())":2,
          "\(Self.childTwoID.uuidString.lowercased())":3},
         "rejected_items":[],"ignored_previous_assignments":[],"plan":{
          "as_of":"\(Self.format(Self.now))","horizon_start":"\(Self.format(expanded.horizonStart))",
          "horizon_end":"\(Self.format(expanded.horizonEnd))","blocks":[
           {"id":"85000000-0000-4000-8000-000000000005","is_sensitive":false,
            "item_id":"\(Self.childOneID.uuidString.lowercased())","occurrence_id":"\(Self.occurrenceID.uuidString.lowercased())",
            "external_block_id":null,"title":"First child","start":"\(Self.preciseTimestamp(firstStart, fraction: "123456"))",
            "end":"\(Self.preciseTimestamp(firstEnd, fraction: "123456"))","session_index":0,
            "kind":"planned","explanations":[]},
           {"id":"86000000-0000-4000-8000-000000000006","is_sensitive":false,
            "item_id":"\(Self.childTwoID.uuidString.lowercased())","occurrence_id":"\(Self.occurrenceID.uuidString.lowercased())",
            "external_block_id":null,"title":"Second child","start":"\(Self.preciseTimestamp(secondStart, fraction: "123456"))",
            "end":"\(Self.preciseTimestamp(secondEnd, fraction: "123456"))","session_index":0,
            "kind":"planned","explanations":[]}\(fixedSuffix)],"unscheduled":[],"decisions":[],"violations":[],
          "score":{"scheduled_minutes":30,"unscheduled_minutes":0,"soft_penalty":0,"moved_minutes":0},
          "occurrences":[{"id":"\(Self.occurrenceID.uuidString.lowercased())",
           "series_item_id":"\(Self.itemID.uuidString.lowercased())",
           "identity":{"type":"calendar_day","date":"\(localDate)","bucket_ordinal":0},
           "nominal_start":"\(nominalStart)",
           "nominal_end":"\(nominalEnd)","window_start":"\(nominalStart)","window_end":"\(nominalEnd)",
           "local_date":"\(localDate)","ordinal":0,"state":"generated"}]}}
        """
        let planner = PlannerStore(
            canonicalItems: [
                try Self.hierarchyRootItem(),
                try Self.hierarchyChildItem(id: Self.childOneID, revision: 2, title: "First child"),
                try Self.hierarchyChildItem(id: Self.childTwoID, revision: 3, title: "Second child"),
            ],
            canonicalDeltaCursor: "source-before",
            canonicalConfigurationIdentifier: configuration,
            scheduleProfile: profile,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(
                    #"{"changes":[],"next_cursor":"source-after","has_more":false}"#.utf8
                )
            ),
            .init(statusCode: 200, body: Data(preview.utf8))
        )
        let sync = CanonicalSyncStore(
            planner: planner,
            configurationStore: RecurrenceMoveAPIConfigurationStore(
                baseURL: baseURL.url.absoluteString
            ),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            now: { Self.now }
        )

        await sync.sync()

        guard case .online = sync.status else {
            Issue.record("Expected exact recurrence preview to publish; got \(sync.status)")
            return
        }
        let rendered = planner.blocks.filter { $0.occurrenceID == Self.occurrenceID }
        #expect(rendered.count == 2)
        let expectedSource = RecurrenceMoveSource(
            itemRevision: 1,
            identity: .calendarDay(date: localDate, bucketOrdinal: 0),
            nominalStart: nominalStart,
            nominalEnd: nominalEnd,
            localDate: localDate,
            ordinal: 0
        )
        #expect(rendered.allSatisfy { $0.recurrenceMoveSource == expectedSource })
        #expect(Set(rendered.compactMap(\.sourceItemID)) == [Self.childOneID, Self.childTwoID])
        #expect(rendered.allSatisfy { $0.recurrenceSeriesItemID == Self.itemID })
        let focused = try #require(rendered.first { $0.sourceItemID == Self.childTwoID })
        let move = try planner.enqueueCanonicalOccurrenceMove(
            blockID: focused.id,
            moveStart: focused.start.addingTimeInterval(3_600)
        )
        #expect(move.source == expectedSource)
        #expect(move.itemID == Self.itemID)
        let restored = PlannerStore.live(persistence: context.persistence)
        #expect(restored.recurrenceOccurrenceMoves == [move])
        #expect(restored.blocks.filter { $0.occurrenceID == Self.occurrenceID }
            .allSatisfy { $0.recurrenceMoveSource == expectedSource })
    }

    @Test("all recurrence identities decode strictly and re-emit their exact wire shape")
    func typedOccurrenceIdentitiesRoundTripExactly() throws {
        let payloads = [
            #"{"type":"calendar_day","date":"2027-01-15","bucket_ordinal":2}"#,
            #"{"type":"calendar_week","week_key":2461421,"bucket_ordinal":3}"#,
            #"{"type":"calendar_month","year":2027,"month":1,"bucket_ordinal":4}"#,
            #"{"type":"rolling_minutes","index":19,"anchor":"2027-01-15T08:00:00.123456789Z"}"#,
            #"{"type":"after_completion","anchor":"2027-01-15T08:00:00.123456789+01:00"}"#,
            #"{"type":"rolling_month","cycle":7,"index":5,"anchor":"2027-01-15T08:00:00.123456789Z"}"#,
            #"{"type":"custom"}"#,
        ]
        let decoder = JSONDecoder()
        let encoder = JSONEncoder()

        for payload in payloads {
            let data = Data(payload.utf8)
            let identity = try decoder.decode(RecurrenceOccurrenceIdentity.self, from: data)
            #expect(identity.hasValidShape)
            let expected = try decoder.decode(JSONValue.self, from: data)
            let emitted = try decoder.decode(
                JSONValue.self,
                from: encoder.encode(identity)
            )
            #expect(emitted == expected)
        }
    }

    @Test("preview occurrence identities reject missing, unknown, extra, and malformed fields")
    func invalidOccurrenceIdentitiesFailClosed() throws {
        let malformedIdentities = [
            #"{"type":"not_a_rule"}"#,
            #"{"type":"calendar_day","date":"2027-01-15"}"#,
            #"{"type":"calendar_day","date":"2027-02-30","bucket_ordinal":0}"#,
            #"{"type":"calendar_week","week_key":2461421,"bucket_ordinal":0,"extra":true}"#,
            #"{"type":"calendar_month","year":2027,"month":13,"bucket_ordinal":0}"#,
            #"{"type":"rolling_minutes","index":-1,"anchor":"2027-01-15T08:00:00Z"}"#,
            #"{"type":"after_completion","anchor":"2027-01-15T08:00:00.1234567890Z"}"#,
            #"{"type":"custom","bucket_ordinal":0}"#,
        ]
        let decoder = JSONDecoder()
        for payload in malformedIdentities {
            #expect(throws: (any Error).self) {
                try decoder.decode(
                    RecurrenceOccurrenceIdentity.self,
                    from: Data(payload.utf8)
                )
            }
        }

        let occurrenceWithoutIdentity = #"""
        {
          "id":"5432cf9b-22b0-56ff-ba43-2d71e23eb904",
          "series_item_id":"81000000-0000-4000-8000-000000000001",
          "nominal_start":"2027-01-15T08:00:00Z",
          "nominal_end":"2027-01-15T08:30:00Z",
          "window_start":"2027-01-15T08:00:00Z",
          "window_end":"2027-01-15T08:30:00Z",
          "local_date":"2027-01-15","ordinal":0,"state":"generated"
        }
        """#
        #expect(throws: (any Error).self) {
            try decoder.decode(
                DayWeaveSchedulePreview.Plan.Occurrence.self,
                from: Data(occurrenceWithoutIdentity.utf8)
            )
        }
    }

    @Test("custom recurrence identities remain visible but cannot authorize a move")
    func customIdentityMoveFailsClosed() throws {
        let context = try Self.persistence()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let profile = PlannerStore(restoreFromPersistence: false).scheduleProfile
        let horizon = try profile.expanded(asOf: Self.now)
        let customSource = RecurrenceMoveSource(
            itemRevision: 1,
            identity: .custom,
            nominalStart: "2027-01-15T08:00:00.123456Z",
            nominalEnd: "2027-01-15T08:30:00.123456Z",
            localDate: nil,
            ordinal: 0
        )
        #expect(customSource.hasValidShape)
        #expect(!customSource.canAuthorizeOccurrenceMove)
        var block = Self.block(
            id: UUID(uuidString: "87000000-0000-4000-8000-000000000007")!,
            sessionIndex: 0,
            start: Self.now.addingTimeInterval(3_600)
        )
        block.recurrenceMoveSource = customSource
        let store = PlannerStore(
            blocks: [block],
            canonicalItems: [try Self.recurringItem()],
            canonicalConfigurationIdentifier: Self.configuration,
            schedulePreviewProvenance: .init(
                configurationIdentifier: Self.configuration,
                generatedAt: Self.now,
                asOf: Self.now,
                horizonStart: horizon.horizonStart,
                horizonEnd: horizon.horizonEnd,
                timezoneName: profile.timezoneName
            ),
            scheduleProfile: profile,
            previewValidatedForCurrentLaunch: true,
            persistence: context.persistence,
            restoreFromPersistence: false,
            now: { Self.now }
        )

        #expect(throws: PlannerRecurrenceMoveError.invalidOccurrence) {
            try store.enqueueCanonicalOccurrenceMove(
                blockID: block.id,
                moveStart: block.start.addingTimeInterval(3_600)
            )
        }
        #expect(store.recurrenceOccurrenceMoves.isEmpty)
    }

    private static func block(id: UUID, sessionIndex: UInt16, start: Date) -> ScheduleBlock {
        ScheduleBlock(
            id: id, title: "Split routine", kind: .habit,
            start: start, end: start.addingTimeInterval(900), status: .scheduled,
            project: nil, notes: "", energy: .medium, isFlexible: true,
            isHardConstraint: false, actualMinutes: nil, sourceItemID: itemID,
            sourceItemRevision: 1, occurrenceID: occurrenceID,
            sessionIndex: sessionIndex, syncOrigin: .canonicalPreview,
            previewKind: "planned", occurrenceFullyScheduled: true,
            recurrenceMoveSource: source
        )
    }

    private static let source = RecurrenceMoveSource(
        itemRevision: 1,
        identity: .calendarDay(date: "2027-01-15", bucketOrdinal: 0),
        nominalStart: "2027-01-15T08:00:00.123456Z",
        nominalEnd: "2027-01-15T08:30:00.123456Z",
        localDate: "2027-01-15",
        ordinal: 0
    )

    private static func recurringItem(revision: UInt64 = 1) throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {
          "id":"\#(itemID.uuidString.lowercased())","is_sensitive":false,
          "kind":"habit","status":"scheduled","title":"Split routine","notes":null,
          "timezone_name":"UTC","duration_seconds":1800,"deadline_at":null,
          "earliest_start_at":null,"recurrence":{"type":"daily","times_per_day":1},
          "flexible_constraints":{},"split_policy":{"type":"splittable",
          "minimum_chunk_seconds":900,"maximum_chunk_seconds":900},
          "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
          "is_executable":true,"revision":\#(revision),"created_at":"2027-01-15T08:00:00Z",
          "updated_at":"2027-01-15T08:00:00Z","completed_at":null,"deleted_at":null
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func hierarchyRootItem() throws -> DayWeaveCanonicalItem {
        let data = Data(#"""
        {
          "id":"\#(itemID.uuidString.lowercased())","is_sensitive":false,
          "kind":"routine","status":"scheduled","title":"Morning routine","notes":null,
          "timezone_name":"UTC","duration_seconds":null,"deadline_at":null,
          "earliest_start_at":null,"recurrence":{"type":"daily","times_per_day":1},
          "flexible_constraints":{},"split_policy":{"type":"indivisible"},
          "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
          "is_executable":false,"revision":1,"created_at":"2027-01-15T08:00:00Z",
          "updated_at":"2027-01-15T08:00:00Z","completed_at":null,"deleted_at":null
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func hierarchyChildItem(
        id: UUID,
        revision: UInt64,
        title: String
    ) throws -> DayWeaveCanonicalItem {
        let escapedTitle = title.replacingOccurrences(of: "\"", with: "\\\"")
        let data = Data(#"""
        {
          "id":"\#(id.uuidString.lowercased())","is_sensitive":false,
          "kind":"task","status":"scheduled","title":"\#(escapedTitle)","notes":null,
          "timezone_name":"UTC","duration_seconds":900,"deadline_at":null,
          "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
          "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
          "parent_id":"\#(itemID.uuidString.lowercased())","sibling_order":0,
          "is_executable":true,"revision":\#(revision),
          "created_at":"2027-01-15T08:00:00Z","updated_at":"2027-01-15T08:00:00Z",
          "completed_at":null,"deleted_at":null
        }
        """#.utf8)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func format(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private static func preciseTimestamp(_ date: Date, fraction: String) -> String {
        let whole = Date(timeIntervalSince1970: floor(date.timeIntervalSince1970))
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: whole).replacingOccurrences(of: "Z", with: ".\(fraction)Z")
    }

    private static func persistence() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveRecurrenceMoveTests-\(UUID().uuidString)", isDirectory: true
        )
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let key = try PlannerEncryptionKey(data: Data(repeating: 89, count: 32))
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: key
            )
        )
    }
}

private struct RecurrenceMoveAPIConfigurationStore: SuggestionAPIConfigurationStoring {
    let baseURL: String
    func loadBaseURL() -> String? { baseURL }
    func saveBaseURL(_ value: String) {}
}
#endif
