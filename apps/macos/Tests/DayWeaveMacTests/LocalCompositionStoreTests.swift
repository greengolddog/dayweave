import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("On-device canonical composition", .serialized)
@MainActor
struct LocalCompositionStoreTests {
    private static let configurationIdentifier =
        "https://api.example.com/gateway|auth=static-v1:\(String(repeating: "a", count: 64))"

    @Test("a signed local result installs atomically with distinct provenance")
    func happyPathInstallsAndPersistsLocalComposition() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        var priorCanonical = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        priorCanonical.status = .paused
        priorCanonical.actualMinutes = 7
        let localCapture = ScheduleBlock(
            id: UUID(),
            title: "Unpublished Inbox capture",
            kind: .task,
            start: now.addingTimeInterval(7_200),
            end: now.addingTimeInterval(9_000),
            status: .scheduled,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            syncOrigin: .local
        )
        let context = try Self.makePlanner(
            now: now,
            item: item,
            blocks: [priorCanonical, localCapture]
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = RecordingLocalComposer()
        let store = Self.makeStore(planner: context.planner, composer: composer, now: now)

        #expect(store.canRecomposeLocally)
        #expect(await store.recomposeLocally())

        #expect(await composer.calls() == 1)
        let request = try #require(await composer.lastRequest())
        #expect(request.asOf == now)
        #expect(request.horizonEnd > request.horizonStart)
        #expect(store.lastPreview == nil)
        #expect(store.warnings.isEmpty)
        #expect(store.lastLocalComposition != nil)
        #expect(store.lastLocalCompositionScore?.scheduledMinutes == 30)
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(context.planner.schedulePreviewProvenance == nil)
        let provenance = try #require(context.planner.localScheduleCompositionProvenance)
        #expect(provenance.configurationIdentifier == Self.configurationIdentifier)
        #expect(provenance.sourceItemRevisions == [item.id: item.revision])
        #expect(provenance.localInputFingerprint.hasPrefix("local-sha256:"))
        let canonical = try #require(context.planner.blocks.first { $0.sourceItemID == item.id })
        #expect(canonical.syncOrigin == .localComposition)
        #expect(canonical.status == .paused)
        #expect(canonical.actualMinutes == 7)
        #expect(context.planner.blocks.contains { $0.id == localCapture.id && $0.syncOrigin == .local })
        #expect(context.planner.canonicalPreviewFreshnessIssue == nil)
        #expect(context.planner.canMutate(canonical))
        #expect(store.canRecomposeLocally)

        let loaded = try context.persistence.load()
        let restored = try #require(loaded)
        #expect(restored.localScheduleCompositionProvenance == provenance)
        #expect(restored.schedulePreviewProvenance == nil)
        #expect(restored.pendingSchedulePublication == nil)
        #expect(restored.blocks == context.planner.blocks)
    }

    @Test("incomplete cache and every pending journal fail closed before helper launch")
    func preflightAndPendingJournalGates() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)

        do {
            let context = try Self.makePlanner(
                now: now,
                item: item,
                cursor: nil,
                configurationIdentifier: nil
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let store = Self.makeStore(planner: context.planner, composer: composer, now: now)
            #expect(!store.canRecomposeLocally)
            #expect(!(await store.recomposeLocally()))
            #expect(await composer.calls() == 0)
            #expect(store.localCompositionStatus.message.contains("normal Sync"))
        }

        for variant in PendingJournalVariant.allCases {
            let pending = try Self.pendingState(variant, item: item, now: now)
            let context = try Self.makePlanner(
                now: now,
                item: item,
                pendingSchedulePublication: pending.schedulePublication,
                pendingProposalApplicationMutation: pending.proposalApplication,
                pendingCanonicalMutations: pending.statusMutations,
                pendingCanonicalSensitivityMutations: pending.sensitivityMutations,
                pendingCanonicalAuthoringMutations: pending.authoringMutations,
                googleOutboundRecoveryJournal: pending.googleRecovery
            )
            defer { try? FileManager.default.removeItem(at: context.directory) }
            let composer = RecordingLocalComposer()
            let originalBlocks = context.planner.blocks
            let store = Self.makeStore(planner: context.planner, composer: composer, now: now)

            #expect(!store.canRecomposeLocally, "\(variant) should disable local composition")
            #expect(!(await store.recomposeLocally()), "\(variant) should fail closed")
            #expect(await composer.calls() == 0)
            #expect(context.planner.blocks == originalBlocks)
            #expect(store.localCompositionStatus.message.contains("normal Sync"))
        }
    }

    @Test("a canonical revision change while awaiting the helper discards its result")
    func revisionRaceDiscardsHelperResult() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let prior = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        let context = try Self.makePlanner(now: now, item: item, blocks: [prior])
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = BlockingLocalComposer()
        let store = Self.makeStore(planner: context.planner, composer: composer, now: now)
        let run = Task { @MainActor in await store.recomposeLocally() }
        await composer.waitUntilStarted()
        #expect(!store.canRecomposeLocally)

        context.planner.upsertCanonicalItem(try LocalCompositionFixture.item(revision: 2))
        await composer.release()

        #expect(!(await run.value))
        #expect(context.planner.blocks == [prior])
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(store.localCompositionStatus.message.contains("changed"))
    }

    @Test("helper failure is explicit, offline, and leaves blocks and publication untouched")
    func helperFailureHasNoSideEffectsOrNetworkFallback() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let prior = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        let context = try Self.makePlanner(now: now, item: item, blocks: [prior])
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let token = "local-helper-failure-\(UUID().uuidString)"
        URLProtocolStub.storage.reset(key: token)
        let store = CanonicalSyncStore(
            planner: context.planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: ThrowingLocalComposer(),
            now: { now }
        )

        #expect(!(await store.recomposeLocally()))

        #expect(context.planner.blocks == [prior])
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(URLProtocolStub.storage.requests(
            for: token,
            includingSchedulePublication: true
        ).isEmpty)
        #expect(store.localCompositionStatus.message.contains("normal Sync"))
        #expect(store.localCompositionStatus.message.contains("No network request"))
    }

    @Test("local success replaces prior server preview evidence")
    func localSuccessClearsPriorServerTransientState() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let token = "local-to-server-failure"
        let matchingConfigurationIdentifier =
            "https://api.example.com/gateway|auth=static-v1:e33e31f9fe3a55cde7bd83e095f8351e3192dfd3d1aa1a0524306fadfe1fe1af"
        let context = try Self.makePlanner(
            now: now,
            item: item,
            configurationIdentifier: matchingConfigurationIdentifier
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let serverPreview = try LocalCompositionFixture.serverRejectedPreview(
            item: item,
            request: LocalCompositionFixture.scheduleRequest(asOf: now)
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        URLProtocolStub.storage.reset(key: token)
        URLProtocolStub.storage.enqueue(
            key: token,
            .init(
                statusCode: 200,
                body: Data(#"{"changes":[],"next_cursor":"complete-cursor","has_more":false}"#.utf8)
            ),
            .init(statusCode: 200, body: try encoder.encode(serverPreview))
        )
        let store = CanonicalSyncStore(
            planner: context.planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: RecordingLocalComposer(),
            now: { now }
        )

        await store.sync()
        #expect(store.lastPreview != nil)
        #expect(!store.warnings.isEmpty)
        #expect(context.planner.schedulePreviewProvenance != nil)
        #expect(store.canRecomposeLocally)

        #expect(await store.recomposeLocally())

        #expect(store.lastPreview == nil)
        #expect(store.warnings.isEmpty)
        #expect(context.planner.schedulePreviewProvenance == nil)
        #expect(context.planner.localScheduleCompositionProvenance != nil)
    }

    @Test("a failed normal sync cannot keep transient local composition evidence")
    func failedNormalSyncClearsTransientLocalComposition() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let token = "local-to-server-failure"
        let matchingConfigurationIdentifier =
            "https://api.example.com/gateway|auth=static-v1:e33e31f9fe3a55cde7bd83e095f8351e3192dfd3d1aa1a0524306fadfe1fe1af"
        let context = try Self.makePlanner(
            now: now,
            item: item,
            configurationIdentifier: matchingConfigurationIdentifier
        )
        defer { try? FileManager.default.removeItem(at: context.directory) }
        URLProtocolStub.storage.reset(key: token)
        let store = CanonicalSyncStore(
            planner: context.planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: token),
            session: URLProtocolStub.makeSession(),
            localComposer: RecordingLocalComposer(),
            now: { now }
        )
        #expect(await store.recomposeLocally())
        if case .composed = store.localCompositionStatus {} else {
            Issue.record("Expected local composition evidence before normal sync")
        }
        #expect(store.lastLocalComposition != nil)
        #expect(store.lastLocalCompositionScore != nil)

        await store.sync()

        #expect(store.status.isFailure)
        if case .composed = store.localCompositionStatus {
            Issue.record("Failed normal sync must not claim a current local composition")
        }
        #expect(store.lastLocalComposition == nil)
        #expect(store.lastLocalCompositionScore == nil)
        #expect(store.localCompositionWarnings.isEmpty)
        #expect(context.planner.localScheduleCompositionProvenance != nil)
        #expect(context.planner.canonicalPreviewFreshnessIssue != nil)
        #expect(URLProtocolStub.storage.requests(for: token).count == 1)
    }

    @Test("a persistence race rolls local blocks and provenance back together")
    func persistenceFailureRollsBackAtomicInstall() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let prior = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .canonicalPreview
        )
        let context = try Self.makePlanner(now: now, item: item, blocks: [prior])
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let composer = BlockingLocalComposer()
        let store = Self.makeStore(planner: context.planner, composer: composer, now: now)
        let run = Task { @MainActor in await store.recomposeLocally() }
        await composer.waitUntilStarted()

        let otherWriter = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        otherWriter.showCompleted.toggle()
        otherWriter.flushPersistence()
        #expect(otherWriter.persistenceError == nil)
        await composer.release()

        #expect(!(await run.value))
        #expect(context.planner.persistenceError == .concurrentModification)
        #expect(context.planner.blocks == [prior])
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(try context.persistence.load()?.localScheduleCompositionProvenance == nil)
    }

    @Test("a server schedule replaces local provenance and origin")
    func serverInstallClearsLocalProvenance() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let context = try Self.makePlanner(now: now, item: item)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = Self.makeStore(
            planner: context.planner,
            composer: RecordingLocalComposer(),
            now: now
        )
        #expect(await store.recomposeLocally())
        #expect(context.planner.localScheduleCompositionProvenance != nil)

        let publication = try Self.pendingSchedulePublication(item: item, now: now)
        try context.planner.persistPendingSchedulePublication(publication)
        var serverBlock = try #require(context.planner.blocks.first { $0.sourceItemID == item.id })
        serverBlock.syncOrigin = .canonicalPreview
        try context.planner.commitPendingSchedulePublication(publication, blocks: [serverBlock])

        #expect(context.planner.pendingSchedulePublication == nil)
        #expect(context.planner.localScheduleCompositionProvenance == nil)
        #expect(context.planner.schedulePreviewProvenance == publication.provenance)
        #expect(context.planner.blocks.first { $0.sourceItemID == item.id }?.syncOrigin == .canonicalPreview)
        let loaded = try context.persistence.load()
        let restored = try #require(loaded)
        #expect(restored.localScheduleCompositionProvenance == nil)
        #expect(restored.schedulePreviewProvenance == publication.provenance)
    }

    @Test("restored, old, and revision-mismatched local schedules are not actionable")
    func localFreshnessProtectsMutationAndExecution() async throws {
        let now = try #require(ISO8601DateFormatter().date(from: "2026-08-30T08:00:00Z"))
        let item = try LocalCompositionFixture.item(revision: 1)
        let context = try Self.makePlanner(now: now, item: item)
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let store = Self.makeStore(
            planner: context.planner,
            composer: RecordingLocalComposer(),
            now: now
        )
        #expect(await store.recomposeLocally())
        let freshBlock = try #require(context.planner.blocks.first { $0.sourceItemID == item.id })
        #expect(context.planner.canMutate(freshBlock))
        #expect(context.planner.canonicalScheduleBlockActionabilityIssue(freshBlock) == nil)

        let restoredPlanner = PlannerStore(
            persistence: context.persistence,
            restoreFromPersistence: true,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        let restoredBlock = try #require(restoredPlanner.blocks.first { $0.sourceItemID == item.id })
        #expect(!restoredPlanner.canMutate(restoredBlock))
        #expect(restoredPlanner.canonicalScheduleBlockActionabilityIssue(restoredBlock) != nil)

        context.planner.upsertCanonicalItem(try LocalCompositionFixture.item(revision: 2))
        #expect(!context.planner.canMutate(freshBlock))
        #expect(context.planner.canonicalScheduleBlockActionabilityIssue(freshBlock) != nil)

        let oldProvenance = LocalCompositionFixture.provenance(
            configurationIdentifier: Self.configurationIdentifier,
            item: item,
            generatedAt: now.addingTimeInterval(-7 * 3_600),
            asOf: now
        )
        let oldBlock = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .localComposition
        )
        let oldPlanner = PlannerStore(
            blocks: [oldBlock],
            canonicalItems: [item],
            canonicalDeltaCursor: "cursor-old",
            canonicalConfigurationIdentifier: Self.configurationIdentifier,
            localScheduleCompositionProvenance: oldProvenance,
            previewValidatedForCurrentLaunch: true,
            restoreFromPersistence: false,
            now: { now }
        )
        #expect(!oldPlanner.canMutate(oldBlock))
        #expect(oldPlanner.canonicalScheduleBlockActionabilityIssue(oldBlock) != nil)
    }

    @Test("schema 11 migrates without inventing local evidence and mixed origins fail closed")
    func snapshotMigrationAndMixedOriginValidation() throws {
        let now = Date(timeIntervalSince1970: 1_788_112_000)
        let item = try LocalCompositionFixture.item(revision: 1)
        let schemaEleven = Self.snapshot(
            schemaVersion: 11,
            now: now,
            item: item,
            blocks: [],
            serverProvenance: nil,
            localProvenance: nil
        )
        let migrated = try schemaEleven.migratedToCurrentSchema()
        #expect(migrated.schemaVersion == 12)
        #expect(migrated.localScheduleCompositionProvenance == nil)

        let localProvenance = LocalCompositionFixture.provenance(
            configurationIdentifier: Self.configurationIdentifier,
            item: item,
            generatedAt: now,
            asOf: now
        )
        let localBlock = LocalCompositionFixture.renderedBlock(
            item: item,
            start: now.addingTimeInterval(1_800),
            origin: .localComposition
        )
        let serverProvenance = LocalCompositionFixture.serverProvenance(
            configurationIdentifier: Self.configurationIdentifier,
            generatedAt: now,
            asOf: now
        )
        let serverWithLocalBlock = Self.snapshot(
            now: now,
            item: item,
            blocks: [localBlock],
            serverProvenance: serverProvenance,
            localProvenance: nil
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try serverWithLocalBlock.migratedToCurrentSchema()
        }

        var canonicalBlock = localBlock
        canonicalBlock.syncOrigin = .canonicalPreview
        let localWithServerBlock = Self.snapshot(
            now: now,
            item: item,
            blocks: [canonicalBlock],
            serverProvenance: nil,
            localProvenance: localProvenance
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try localWithServerBlock.migratedToCurrentSchema()
        }

        let mismatchedConfiguration = LocalCompositionFixture.provenance(
            configurationIdentifier: "https://other.example.invalid",
            item: item,
            generatedAt: now,
            asOf: now
        )
        let mismatchedSnapshot = Self.snapshot(
            now: now,
            item: item,
            blocks: [localBlock],
            serverProvenance: nil,
            localProvenance: mismatchedConfiguration
        )
        #expect(throws: PlannerPersistenceError.snapshotDecodingFailed) {
            try mismatchedSnapshot.migratedToCurrentSchema()
        }
    }

    private static func makeStore(
        planner: PlannerStore,
        composer: any LocalScheduleComposing,
        now: Date
    ) -> CanonicalSyncStore {
        CanonicalSyncStore(
            planner: planner,
            configurationStore: LocalCompositionConfigurationStore(),
            tokenStore: TestBearerTokenStore(token: "local-composition-test-token"),
            session: URLProtocolStub.makeSession(),
            localComposer: composer,
            now: { now }
        )
    }

    private static func makePlanner(
        now: Date,
        item: DayWeaveCanonicalItem,
        blocks: [ScheduleBlock] = [],
        cursor: String? = "complete-cursor",
        configurationIdentifier: String? = Self.configurationIdentifier,
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        pendingProposalApplicationMutation: DayWeavePendingProposalApplicationMutation? = nil,
        pendingCanonicalMutations: [PendingCanonicalMutation] = [],
        pendingCanonicalSensitivityMutations: [PendingCanonicalSensitivityMutation] = [],
        pendingCanonicalAuthoringMutations: [DayWeavePendingCanonicalAuthoringMutation] = [],
        googleOutboundRecoveryJournal: GoogleOutboundRecoveryJournal? = nil
    ) throws -> (directory: URL, persistence: EncryptedPlannerPersistence, planner: PlannerStore) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveLocalComposition-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: PlannerEncryptionKey.random()
        )
        let planner = PlannerStore(
            blocks: blocks,
            canonicalItems: [item],
            canonicalDeltaCursor: cursor,
            pendingCanonicalMutations: pendingCanonicalMutations,
            pendingCanonicalSensitivityMutations: pendingCanonicalSensitivityMutations,
            canonicalConfigurationIdentifier: configurationIdentifier,
            pendingSchedulePublication: pendingSchedulePublication,
            pendingProposalApplicationMutation: pendingProposalApplicationMutation,
            pendingCanonicalAuthoringMutations: pendingCanonicalAuthoringMutations,
            googleOutboundRecoveryJournal: googleOutboundRecoveryJournal,
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        return (directory, persistence, planner)
    }

    private static func pendingState(
        _ variant: PendingJournalVariant,
        item: DayWeaveCanonicalItem,
        now: Date
    ) throws -> PendingJournalState {
        var state = PendingJournalState()
        switch variant {
        case .schedulePublication:
            state.schedulePublication = try pendingSchedulePublication(item: item, now: now)
        case .proposalApplication:
            let reviewHash = "sha256:\(String(repeating: "b", count: 64))"
            let body = try JSONEncoder().encode(DayWeaveProposalApplyRequest(
                expectedReviewHash: reviewHash
            ))
            state.proposalApplication = .apply(
                configurationIdentifier: configurationIdentifier,
                proposalIDs: [UUID()],
                proposalRevisions: [1],
                expectedCommandIDs: [UUID()],
                previewID: UUID(),
                expectedReviewHash: reviewHash,
                requestBody: body,
                idempotencyKey: "local-composition-proposal-gate",
                createdAt: now
            )
        case .googleRecovery:
            state.googleRecovery = try GoogleOutboundRecoveryJournal(
                operationGeneration: 1,
                configurationIdentifier: "google-local-composition-gate",
                accountID: UUID(),
                collectionID: UUID(),
                itemID: item.id,
                expectedItemRevision: item.revision,
                operation: .upsert,
                intentExpiresAt: now.addingTimeInterval(30 * 60),
                createdAt: now
            )
        case .statusMutation:
            state.statusMutations = [PendingCanonicalMutation(
                id: UUID(),
                itemID: item.id,
                occurrenceID: nil,
                sessionIndex: 0,
                desiredStatus: .completed,
                baseRevision: item.revision,
                createdAt: now,
                disposition: .pending,
                diagnostic: nil
            )]
        case .sensitivityMutation:
            state.sensitivityMutations = [PendingCanonicalSensitivityMutation(
                id: UUID(),
                itemID: item.id,
                desiredIsSensitive: true,
                baseRevision: item.revision,
                createdAt: now,
                disposition: .pending,
                diagnostic: nil
            )]
        case .authoringMutation:
            let draftID = UUID()
            state.authoringMutations = [DayWeavePendingCanonicalAuthoringMutation(
                itemID: draftID,
                operation: .create,
                draft: DayWeaveCanonicalItemDraft(
                    title: "Queued local draft",
                    timezoneName: "Europe/Madrid",
                    durationSeconds: 1_800
                ),
                createdAt: now
            )]
        }
        return state
    }

    private static func pendingSchedulePublication(
        item: DayWeaveCanonicalItem,
        now: Date
    ) throws -> PendingSchedulePublication {
        let request = LocalCompositionFixture.scheduleRequest(asOf: now)
        let digest = "sha256:\(String(repeating: "c", count: 64))"
        let preview = try LocalCompositionFixture.serverPreview(
            item: item,
            request: request,
            inputDigest: digest
        )
        let publish = DayWeaveSchedulePublishRequest(
            idempotencyKey: UUID(),
            expectedInputDigest: digest,
            schedule: request
        )
        return PendingSchedulePublication(
            configurationIdentifier: configurationIdentifier,
            preparedRequest: .init(
                request: publish,
                body: Data("{}".utf8),
                bodySHA256: String(repeating: "d", count: 64)
            ),
            preview: preview,
            message: "Pending server publication",
            provenance: LocalCompositionFixture.serverProvenance(
                configurationIdentifier: configurationIdentifier,
                generatedAt: now,
                asOf: now
            ),
            preparedAt: now
        )
    }

    private static func snapshot(
        schemaVersion: Int = PlannerSnapshot.currentSchemaVersion,
        now: Date,
        item: DayWeaveCanonicalItem,
        blocks: [ScheduleBlock],
        serverProvenance: SchedulePreviewProvenance?,
        localProvenance: LocalScheduleCompositionProvenance?
    ) -> PlannerSnapshot {
        PlannerSnapshot(
            schemaVersion: schemaVersion,
            savedAt: now,
            destination: .today,
            selectedBlockID: blocks.first?.id,
            blocks: blocks,
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "fixture",
            protectedFreeMinutes: 90,
            freezeHours: 2,
            showCompleted: true,
            canonicalItems: [item],
            canonicalDeltaCursor: "complete-cursor",
            canonicalTombstoneRevisions: [:],
            completedOccurrenceIDs: [],
            pendingCanonicalMutations: [],
            pendingCanonicalSensitivityMutations: [],
            recurrenceSessionOutcomes: [],
            canonicalConfigurationIdentifier: configurationIdentifier,
            schedulePreviewProvenance: serverProvenance,
            localScheduleCompositionProvenance: localProvenance,
            proposalApplicationReceipts: [],
            pendingCanonicalAuthoringMutations: [],
            canonicalTrash: [],
            localCaptureDiagnostics: [:],
            executionState: .empty
        )
    }
}

private enum PendingJournalVariant: String, CaseIterable {
    case schedulePublication
    case proposalApplication
    case googleRecovery
    case statusMutation
    case sensitivityMutation
    case authoringMutation
}

private struct PendingJournalState {
    var schedulePublication: PendingSchedulePublication?
    var proposalApplication: DayWeavePendingProposalApplicationMutation?
    var googleRecovery: GoogleOutboundRecoveryJournal?
    var statusMutations: [PendingCanonicalMutation] = []
    var sensitivityMutations: [PendingCanonicalSensitivityMutation] = []
    var authoringMutations: [DayWeavePendingCanonicalAuthoringMutation] = []
}

private struct LocalCompositionConfigurationStore: SuggestionAPIConfigurationStoring {
    func loadBaseURL() -> String? { "https://api.example.com/gateway" }
    func saveBaseURL(_ value: String) {}
}

private enum LocalCompositionFixture {
    static let itemID = UUID(uuidString: "b1000000-0000-4000-8000-000000000001")!
    static let plannedBlockID = UUID(uuidString: "b2000000-0000-4000-8000-000000000002")!

    static func item(revision: UInt64) throws -> DayWeaveCanonicalItem {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data("""
        {"id":"\(itemID.uuidString.lowercased())","is_sensitive":false,"kind":"task",
        "status":"scheduled","title":"Compose locally","notes":"private notes",
        "timezone_name":"Europe/Madrid","duration_seconds":1800,"deadline_at":null,
        "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":50,"urgency":50,
        "parent_id":null,"sibling_order":0,"is_executable":true,"revision":\(revision),
        "created_at":"2026-08-30T08:00:00Z","updated_at":"2026-08-30T08:00:00Z",
        "completed_at":null,"deleted_at":null}
        """.utf8))
    }

    static func makeComposition(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) -> LocalScheduleComposition {
        let item = canonicalItems[0]
        let start = max(schedule.asOf.addingTimeInterval(3_600), schedule.horizonStart)
        let end = start.addingTimeInterval(1_800)
        let plan = DayWeaveSchedulePreview.Plan(
            asOf: schedule.asOf,
            horizonStart: schedule.horizonStart,
            horizonEnd: schedule.horizonEnd,
            blocks: [.init(
                id: plannedBlockID,
                isSensitive: false,
                itemID: item.id,
                occurrenceID: nil,
                externalBlockID: nil,
                title: item.title,
                start: start,
                end: end,
                sessionIndex: 0,
                kind: "planned",
                explanations: [.init(code: "local", message: "Composed on this Mac.")]
            )],
            unscheduled: [],
            decisions: [],
            violations: [],
            score: .init(
                scheduledMinutes: 30,
                unscheduledMinutes: 0,
                softPenalty: 0,
                movedMinutes: 0
            ),
            occurrences: []
        )
        return LocalScheduleComposition(
            localInputFingerprint: "local-sha256:\(String(repeating: "e", count: 64))",
            sourceItemCount: canonicalItems.count,
            sourceItemRevisions: Dictionary(
                uniqueKeysWithValues: canonicalItems.map { ($0.id, $0.revision) }
            ),
            acceptedItemCount: canonicalItems.count,
            rejectedItems: [],
            ignoredPreviousAssignments: [],
            plan: plan
        )
    }

    static func renderedBlock(
        item: DayWeaveCanonicalItem,
        start: Date,
        origin: ScheduleBlockOrigin
    ) -> ScheduleBlock {
        ScheduleBlock(
            id: plannedBlockID,
            title: item.title,
            kind: .task,
            start: start,
            end: start.addingTimeInterval(1_800),
            status: .scheduled,
            project: nil,
            notes: item.notes ?? "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            sourceItemID: item.id,
            sourceItemRevision: item.revision,
            sessionIndex: 0,
            syncOrigin: origin,
            previewKind: "planned"
        )
    }

    static func scheduleRequest(asOf: Date) -> DayWeaveSchedulePreviewRequest {
        let calendar = Calendar.autoupdatingCurrent
        let start = calendar.startOfDay(for: asOf)
        return .init(
            asOf: asOf,
            horizonStart: start,
            horizonEnd: calendar.date(byAdding: .day, value: 7, to: start)
                ?? start.addingTimeInterval(7 * 86_400),
            timezoneName: planningTimezone,
            availability: [],
            fixedBlocks: [],
            previousAssignments: [],
            config: .init(
                slotGranularityMinutes: 5,
                stabilityWeight: 4,
                defaultSoftWeight: 100
            ),
            recurrenceContext: [:]
        )
    }

    static func serverPreview(
        item: DayWeaveCanonicalItem,
        request: DayWeaveSchedulePreviewRequest,
        inputDigest: String
    ) throws -> DayWeaveSchedulePreview {
        let composition = makeComposition(canonicalItems: [item], schedule: request)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let plan = try JSONSerialization.jsonObject(with: encoder.encode(composition.plan))
        let object: [String: Any] = [
            "input_digest": inputDigest,
            "source_item_count": 1,
            "accepted_item_count": 1,
            "source_item_revisions": [item.id.uuidString.lowercased(): item.revision],
            "rejected_items": [],
            "ignored_previous_assignments": [],
            "plan": plan,
        ]
        let data = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveSchedulePreview.self, from: data)
    }

    static func serverRejectedPreview(
        item: DayWeaveCanonicalItem,
        request: DayWeaveSchedulePreviewRequest
    ) throws -> DayWeaveSchedulePreview {
        let plan = DayWeaveSchedulePreview.Plan(
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            blocks: [],
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
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let planObject = try JSONSerialization.jsonObject(with: encoder.encode(plan))
        let object: [String: Any] = [
            "input_digest": "sha256:\(String(repeating: "f", count: 64))",
            "source_item_count": 1,
            "accepted_item_count": 0,
            "source_item_revisions": [item.id.uuidString.lowercased(): item.revision],
            "rejected_items": [[
                "item_id": item.id.uuidString.lowercased(),
                "is_sensitive": false,
                "title": item.title,
                "reason": "fixture warning",
            ]],
            "ignored_previous_assignments": [],
            "plan": planObject,
        ]
        let data = try JSONSerialization.data(withJSONObject: object)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveSchedulePreview.self, from: data)
    }

    static func provenance(
        configurationIdentifier: String,
        item: DayWeaveCanonicalItem,
        generatedAt: Date,
        asOf: Date
    ) -> LocalScheduleCompositionProvenance {
        let request = scheduleRequest(asOf: asOf)
        return .init(
            configurationIdentifier: configurationIdentifier,
            localInputFingerprint: "local-sha256:\(String(repeating: "e", count: 64))",
            generatedAt: generatedAt,
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            timezoneName: request.timezoneName,
            sourceItemRevisions: [item.id: item.revision]
        )
    }

    static func serverProvenance(
        configurationIdentifier: String,
        generatedAt: Date,
        asOf: Date
    ) -> SchedulePreviewProvenance {
        let request = scheduleRequest(asOf: asOf)
        return .init(
            configurationIdentifier: configurationIdentifier,
            generatedAt: generatedAt,
            asOf: request.asOf,
            horizonStart: request.horizonStart,
            horizonEnd: request.horizonEnd,
            timezoneName: request.timezoneName
        )
    }

    private static var planningTimezone: String {
        let identifier = TimeZone.autoupdatingCurrent.identifier
        return identifier == "GMT" ? "UTC" : identifier
    }
}

private actor RecordingLocalComposer: LocalScheduleComposing {
    private var callCount = 0
    private var request: DayWeaveSchedulePreviewRequest?

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        callCount += 1
        request = schedule
        return LocalCompositionFixture.makeComposition(
            canonicalItems: canonicalItems,
            schedule: schedule
        )
    }

    func calls() -> Int { callCount }
    func lastRequest() -> DayWeaveSchedulePreviewRequest? { request }
}

private actor BlockingLocalComposer: LocalScheduleComposing {
    private var started = false
    private var released = false
    private var continuation: CheckedContinuation<Void, Never>?

    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        started = true
        await withCheckedContinuation { continuation in
            if released {
                continuation.resume()
            } else {
                self.continuation = continuation
            }
        }
        return LocalCompositionFixture.makeComposition(
            canonicalItems: canonicalItems,
            schedule: schedule
        )
    }

    func waitUntilStarted() async {
        while !started { await Task.yield() }
    }

    func release() {
        released = true
        continuation?.resume()
        continuation = nil
    }
}

private enum LocalCompositionTestError: Error {
    case helperFailed
}

private struct ThrowingLocalComposer: LocalScheduleComposing {
    func compose(
        canonicalItems: [DayWeaveCanonicalItem],
        schedule: DayWeaveSchedulePreviewRequest
    ) async throws -> LocalScheduleComposition {
        throw LocalCompositionTestError.helperFailed
    }
}
#endif
