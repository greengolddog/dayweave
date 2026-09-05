import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Crash-safe generated-schedule Google publication", .serialized)
@MainActor
struct GoogleSchedulePublicationStoreTests {
    @Test("explicit review durably fences every authority transition and drops capability")
    func explicitReviewPersistsTransitionsAndDropsCapability() async throws {
        let recovery = TestGoogleSchedulePublicationRecoveryStore()
        let transport = TestGoogleSchedulePublicationTransport(
            configurationIdentifier: Self.configuration,
            preview: Self.preview,
            approval: Self.approval,
            acceptance: Self.acceptance,
            status: Self.completedStatus
        )
        let store = GoogleSchedulePublicationStore(
            recoveryStore: recovery,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { Self.now }
        )

        #expect(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            scheduleRevisionID: Self.scheduleRevisionID
        ))
        #expect(recovery.saved.map(\.stage) == [.intent, .previewed])
        #expect(await transport.approvalCallCount == 0)
        #expect(await transport.enqueueCallCount == 0)
        let confirmation = try #require(store.approvalConfirmation)

        #expect(await store.approveAndEnqueue(confirmation))
        #expect(recovery.saved.map(\.stage) == [
            .intent, .previewed, .approvalAttempted, .approved, .approved,
            .accepted, .accepted,
        ])
        let approved = try #require(recovery.saved.first { $0.stage == .approved })
        #expect(approved.approvalCapability == Self.capability)
        let accepted = try #require(recovery.value)
        #expect(accepted.stage == .accepted)
        #expect(accepted.approvalCapability == nil)
        #expect(accepted.approvalExpiresAt == nil)
        #expect(accepted.deliveryStatus?.state == .published)
        #expect(!accepted.description.contains(Self.capability))
        #expect(!String(reflecting: accepted).contains(Self.capability))
        #expect(store.hasPendingRecovery == false)
        #expect(store.hasSavedPublication)
        #expect(await transport.approvalCallCount == 1)
        #expect(await transport.enqueueCallCount == 1)
        #expect(await transport.statusCallCount == 1)

        #expect(store.dismissCompletedPublication())
        #expect(recovery.value == nil)
        #expect(!store.hasSavedPublication)
    }

    @Test("lost one-time approval response never retries or queues")
    func ambiguousApprovalDoesNotRetry() async throws {
        let recovery = TestGoogleSchedulePublicationRecoveryStore()
        let transport = TestGoogleSchedulePublicationTransport(
            configurationIdentifier: Self.configuration,
            preview: Self.preview,
            approval: Self.approval,
            acceptance: Self.acceptance,
            status: Self.completedStatus,
            approvalError: TestGoogleSchedulePublicationError.ambiguous(
                "lost \(Self.capability)"
            )
        )
        let store = GoogleSchedulePublicationStore(
            recoveryStore: recovery,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { Self.now }
        )
        #expect(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            scheduleRevisionID: Self.scheduleRevisionID
        ))
        let confirmation = try #require(store.approvalConfirmation)
        #expect(!(await store.approveAndEnqueue(confirmation)))
        #expect(recovery.value?.stage == .approvalAttempted)
        #expect(!store.status.message.contains(Self.capability))

        #expect(await store.recoverPendingPublication())
        #expect(await transport.approvalCallCount == 1)
        #expect(await transport.enqueueCallCount == 0)
        #expect(recovery.value?.stage == .approvalAttempted)
    }

    @Test("approved recovery requires an explicit replay lane")
    func approvedRecoveryRequiresExplicitReplay() async throws {
        let intent = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: Self.configuration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            createdAt: Self.now
        )
        let approved = try intent.recording(preview: Self.preview)
            .recordingApprovalAttempt()
            .recording(approval: Self.approval)
        let recovery = TestGoogleSchedulePublicationRecoveryStore(value: approved)
        let transport = TestGoogleSchedulePublicationTransport(
            configurationIdentifier: Self.configuration,
            preview: Self.preview,
            approval: Self.approval,
            acceptance: Self.acceptance,
            status: Self.completedStatus
        )
        let store = GoogleSchedulePublicationStore(
            recoveryStore: recovery,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { Self.now }
        )

        #expect(store.recoveryStage == .approved)
        #expect(store.status == .approvedReplayRequired)
        #expect(await store.recoverPendingPublication())
        #expect(store.status == .approvedReplayRequired)
        #expect(await transport.enqueueCallCount == 0)

        #expect(await store.replayApprovedEnqueue())
        #expect(await transport.enqueueCallCount == 1)
        #expect(recovery.value?.stage == .accepted)
        #expect(store.recoveryStage == .accepted)

        store.setPrivacyAvailable(false)
        #expect(store.recoveryStage == nil)
    }

    @Test("newer published head revokes a preview before one-shot approval")
    func newerPublishedHeadRevokesPreviewApproval() async throws {
        let fixture = try Self.makeAuthoritativePlanner(
            directoryPrefix: "DayWeaveScheduleGoogleStalePreview",
            keyByte: 61
        )
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let transport = TestGoogleSchedulePublicationTransport(
            configurationIdentifier: Self.configuration,
            preview: Self.preview,
            approval: Self.approval,
            acceptance: Self.acceptance,
            status: Self.completedStatus
        )
        let store = GoogleSchedulePublicationStore(
            recoveryStore: fixture.planner,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { Self.now }
        )

        #expect(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            scheduleRevisionID: Self.scheduleRevisionID
        ))
        let confirmation = try #require(store.approvalConfirmation)
        #expect(
            try fixture.planner.loadGoogleSchedulePublicationRecoveryJournal()?.stage
                == .previewed
        )

        try fixture.planner.persistPublishedScheduleRevisionHint(9)
        #expect(!(await store.approveAndEnqueue(confirmation)))
        #expect(await transport.approvalCallCount == 0)
        #expect(await transport.enqueueCallCount == 0)
        #expect(
            try fixture.planner.loadGoogleSchedulePublicationRecoveryJournal()?.stage
                == .previewed
        )
    }

    @Test("newer published head blocks approved replay before provider I/O")
    func newerPublishedHeadBlocksApprovedReplay() async throws {
        let fixture = try Self.makeAuthoritativePlanner(
            directoryPrefix: "DayWeaveScheduleGoogleStaleApproved",
            keyByte: 62
        )
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let intent = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: Self.configuration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            createdAt: Self.now
        )
        let previewed = try intent.recording(preview: Self.preview)
        let attempted = try previewed.recordingApprovalAttempt()
        let approved = try attempted.recording(approval: Self.approval)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(intent)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(previewed)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(attempted)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(approved)

        let transport = TestGoogleSchedulePublicationTransport(
            configurationIdentifier: Self.configuration,
            preview: Self.preview,
            approval: Self.approval,
            acceptance: Self.acceptance,
            status: Self.completedStatus
        )
        let store = GoogleSchedulePublicationStore(
            recoveryStore: fixture.planner,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { Self.now }
        )
        #expect(store.recoveryStage == .approved)

        try fixture.planner.persistPublishedScheduleRevisionHint(9)
        #expect(!(await store.replayApprovedEnqueue()))
        #expect(await transport.enqueueCallCount == 0)
        #expect(
            try fixture.planner.loadGoogleSchedulePublicationRecoveryJournal() == approved
        )
    }

    @Test("a newer head during enqueue cannot erase the provider acceptance receipt")
    func newerPublishedHeadDuringEnqueueRetainsAcceptanceReceipt() async throws {
        let fixture = try Self.makeAuthoritativePlanner(
            directoryPrefix: "DayWeaveScheduleGoogleInFlightHead",
            keyByte: 63
        )
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let intent = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: Self.configuration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            createdAt: Self.now
        )
        let previewed = try intent.recording(preview: Self.preview)
        let attempted = try previewed.recordingApprovalAttempt()
        let approved = try attempted.recording(approval: Self.approval)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(intent)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(previewed)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(attempted)
        try fixture.planner.saveGoogleSchedulePublicationRecoveryJournal(approved)

        let enqueueGate = TestGoogleSchedulePublicationEnqueueGate()
        let transport = TestGoogleSchedulePublicationTransport(
            configurationIdentifier: Self.configuration,
            preview: Self.preview,
            approval: Self.approval,
            acceptance: Self.acceptance,
            status: Self.completedStatus,
            enqueueGate: enqueueGate
        )
        let store = GoogleSchedulePublicationStore(
            recoveryStore: fixture.planner,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { Self.now }
        )

        let replay = Task { @MainActor in
            await store.replayApprovedEnqueue()
        }
        await enqueueGate.waitUntilEntered()
        try fixture.planner.persistPublishedScheduleRevisionHint(9)
        await enqueueGate.release()

        #expect(await replay.value)
        #expect(await transport.enqueueCallCount == 1)
        #expect(await transport.statusCallCount == 1)
        let loaded = try fixture.planner.loadGoogleSchedulePublicationRecoveryJournal()
        let retained = try #require(loaded)
        #expect(retained.stage == .accepted)
        #expect(retained.deliveryStatus?.state == .published)
    }

    @Test("journal decoding rejects unknown fields and reflection hides authority")
    func strictJournalShape() throws {
        let journal = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: Self.configuration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            createdAt: Self.now
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let encoded = try encoder.encode(journal)
        var object = try #require(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        object["unknown"] = true
        #expect(throws: DecodingError.self) {
            try decoder.decode(
                GoogleSchedulePublicationRecoveryJournal.self,
                from: JSONSerialization.data(withJSONObject: object)
            )
        }
        #expect(!String(reflecting: Self.approval).contains(Self.capability))
    }

    @Test("approval capability round-trips only inside encrypted planner state")
    func approvalCapabilityIsEncryptedAtRest() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveScheduleGooglePersistence-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data((0..<32).map(UInt8.init)))
        let persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: key)
        let intent = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: Self.configuration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            createdAt: Self.now
        )
        let previewed = try intent.recording(preview: Self.preview)
        let attempted = try previewed.recordingApprovalAttempt()
        let approved = try attempted.recording(approval: Self.approval)
        let snapshot = PlannerSnapshot(
            savedAt: Self.now,
            destination: .calendar,
            selectedBlockID: nil,
            blocks: [],
            suggestions: [],
            assistantMessages: [],
            lastScheduleMessage: "Schedule reviewed for Google",
            protectedFreeMinutes: 90,
            freezeHours: 2,
            showCompleted: true,
            googleSchedulePublicationRecoveryJournal: approved
        )

        try persistence.save(snapshot)
        let encrypted = try Data(contentsOf: fileURL)
        #expect(encrypted.range(of: Data(Self.capability.utf8)) == nil)
        #expect(try persistence.load()?.googleSchedulePublicationRecoveryJournal == approved)
    }

    @Test("maximum wire preview coexists with a legacy-limit planner snapshot")
    func maximumWirePreviewFitsEncryptedPlannerBudget() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveScheduleGoogleBudget-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        let fileURL = directory.appendingPathComponent("planner.snapshot.encrypted")
        let key = try PlannerEncryptionKey(data: Data(repeating: 47, count: 32))
        let persistence = EncryptedPlannerPersistence(fileURL: fileURL, key: key)
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: Self.configuration,
            generatedAt: Self.now,
            asOf: Self.now,
            horizonStart: Self.now,
            horizonEnd: Self.now.addingTimeInterval(7 * 24 * 60 * 60),
            timezoneName: "UTC"
        )
        let proof = DayWeavePublishedScheduleProof(
            configurationIdentifier: Self.configuration,
            revisionID: Self.scheduleRevisionID,
            revision: "8:\(Self.scheduleRevisionID.uuidString.lowercased())",
            revisionNumber: 8,
            inputDigest: "sha256:\(String(repeating: "b", count: 64))",
            asOf: provenance.asOf,
            horizonStart: provenance.horizonStart,
            horizonEnd: provenance.horizonEnd,
            timezoneName: provenance.timezoneName,
            publishedAt: Self.now,
            publishedBlocks: []
        )
        let largeMessage = AssistantMessage(
            id: UUID(),
            role: .assistant,
            text: String(
                repeating: "x",
                count: EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
                    - 512 * 1_024
            ),
            createdAt: Self.now
        )
        let planner = PlannerStore(
            assistantMessages: [largeMessage],
            canonicalConfigurationIdentifier: Self.configuration,
            schedulePreviewProvenance: provenance,
            publishedScheduleProof: proof,
            previewValidatedForCurrentLaunch: true,
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { Self.now }
        )
        planner.flushPersistence()
        #expect(planner.persistenceError == nil)

        let loadedBaseSnapshot = try persistence.load()
        let baseSnapshot = try #require(loadedBaseSnapshot)
        let basePlaintext = try Self.encodePlannerSnapshot(baseSnapshot)
        #expect(
            EncryptedPlannerPersistence.maximumPlaintextBytes(for: baseSnapshot)
                == EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
        )
        #expect(
            basePlaintext.count
                <= EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
        )
        #expect(
            basePlaintext.count
                >= EncryptedPlannerPersistence.legacyMaximumPlaintextBytes - 1_048_576
        )

        let boundary = try Self.makeNearMaximumWirePreview()
        #expect(
            boundary.wireBytes
                <= EncryptedPlannerPersistence.maximumSchedulePublicationPreviewTransportBytes
        )
        #expect(
            EncryptedPlannerPersistence.maximumSchedulePublicationPreviewTransportBytes
                - boundary.wireBytes < boundary.bytesPerProviderCharacter
        )

        let intent = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: 1,
            configurationIdentifier: Self.configuration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            createdAt: Self.now
        )
        try planner.saveGoogleSchedulePublicationRecoveryJournal(intent)
        let previewed = try intent.recording(preview: boundary.preview)
        try planner.saveGoogleSchedulePublicationRecoveryJournal(previewed)
        #expect(planner.persistenceError == nil)

        let loadedRestoredSnapshot = try persistence.load()
        let restored = try #require(loadedRestoredSnapshot)
        let restoredJournal = try #require(
            restored.googleSchedulePublicationRecoveryJournal
        )
        #expect(restoredJournal.stage == .previewed)
        #expect(restoredJournal.preview?.changes.count == boundary.preview.changes.count)
        #expect(
            restoredJournal.preview?.changes.last?.providerResourceID?.utf8.count
                == boundary.providerCharacters
        )
        let combinedPlaintext = try Self.encodePlannerSnapshot(restored)
        #expect(
            EncryptedPlannerPersistence.maximumPlaintextBytes(for: restored)
                == EncryptedPlannerPersistence.maximumPlaintextBytes
        )
        #expect(combinedPlaintext.count <= EncryptedPlannerPersistence.maximumPlaintextBytes)
        #expect(combinedPlaintext.count > basePlaintext.count + boundary.wireBytes)

        let envelope = try Data(contentsOf: fileURL)
        let envelopeObject = try #require(
            JSONSerialization.jsonObject(with: envelope) as? [String: Any]
        )
        let sealedBase64 = try #require(envelopeObject["sealedSnapshot"] as? String)
        let sealed = try #require(Data(base64Encoded: sealedBase64))
        #expect(
            sealed.count
                == combinedPlaintext.count
                    + EncryptedPlannerPersistence.aesGCMCombinedOverheadBytes
        )
        #expect(
            envelope.count
                == sealedBase64.utf8.count
                    + EncryptedPlannerPersistence.envelopeJSONFramingBytes
        )
        #expect(envelope.count <= EncryptedPlannerPersistence.maximumEnvelopeBytes)
    }

    @Test("publication storage limits are derived from transport and encoding bounds")
    func publicationStorageBudgetIsDerivedAndCoversMetadata() throws {
        #expect(
            EncryptedPlannerPersistence.maximumSchedulePublicationPreviewTransportBytes
                == DayWeaveAPIClient.maximumResponseBytes
        )
        #expect(DayWeaveAPIClient.maximumResponseBytes == 16 * 1_048_576)
        #expect(
            EncryptedPlannerPersistence.maximumPlaintextBytes
                == EncryptedPlannerPersistence.legacyMaximumPlaintextBytes
                    + DayWeaveAPIClient.maximumResponseBytes
                        * EncryptedPlannerPersistence.maximumJSONReencodingExpansionFactor
                    + EncryptedPlannerPersistence
                        .maximumSchedulePublicationJournalMetadataBytes
        )
        let maximumConfiguration = String(
            repeating: "\\",
            count: GoogleDisconnectRetryJournal.maximumConfigurationIdentifierBytes
        )
        let intent = try GoogleSchedulePublicationRecoveryJournal(
            operationGeneration: UInt64(Int64.max),
            configurationIdentifier: maximumConfiguration,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            expectedScheduleRevisionID: Self.scheduleRevisionID,
            intentExpiresAt: Self.now.addingTimeInterval(35 * 60),
            preview: Self.preview,
            approvalAttempted: true,
            acceptance: Self.acceptance,
            deliveryStatus: Self.completedStatus,
            createdAt: Self.now
        )
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        let journalBytes = try encoder.encode(intent).count
        let previewBytes = try encoder.encode(Self.preview).count
        #expect(
            journalBytes - previewBytes
                <= EncryptedPlannerPersistence
                    .maximumSchedulePublicationJournalMetadataBytes
        )
        let sealedLimit = EncryptedPlannerPersistence.maximumPlaintextBytes
            + EncryptedPlannerPersistence.aesGCMCombinedOverheadBytes
        let expectedBase64Limit = 4 * ((sealedLimit + 2) / 3)
        #expect(
            EncryptedPlannerPersistence.maximumEnvelopeBytes
                == expectedBase64Limit
                    + EncryptedPlannerPersistence.envelopeJSONFramingBytes
        )
    }

    @Test("preview enforces operation evidence and exact per-operation counts")
    func previewOperationEvidenceAndCounts() {
        let invalidCreate = GoogleSchedulePublicationChange(
            ordinal: 0,
            slotID: Self.slotID,
            sourceBlockID: Self.blockID,
            operation: .create,
            providerResourceID: "existing-event",
            providerETag: "etag",
            summary: "Busy",
            startsAt: Self.now.addingTimeInterval(3_600),
            endsAt: Self.now.addingTimeInterval(7_200)
        )
        #expect(!invalidCreate.hasValidShape)

        let validDelete = GoogleSchedulePublicationChange(
            ordinal: 0,
            slotID: Self.slotID,
            sourceBlockID: nil,
            operation: .delete,
            providerResourceID: "existing-event",
            providerETag: "etag",
            summary: "Previously published DayWeave block",
            startsAt: Self.now.addingTimeInterval(3_600),
            endsAt: Self.now.addingTimeInterval(7_200)
        )
        #expect(validDelete.hasValidShape)
        let forgedCounts = GoogleSchedulePublicationPreview(
            id: Self.previewID,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            collectionRevision: 4,
            collectionDisplayName: "Primary calendar",
            scheduleRevisionID: Self.scheduleRevisionID,
            scheduleRevisionNumber: 8,
            previewHash: String(repeating: "a", count: 64),
            createCount: 1,
            updateCount: 0,
            deleteCount: 0,
            noopCount: 0,
            changes: [validDelete],
            expiresAt: Self.now.addingTimeInterval(10 * 60)
        )
        #expect(!forgedCounts.hasValidShape)

        let duplicateSlots = GoogleSchedulePublicationPreview(
            id: Self.previewID,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            collectionRevision: 4,
            collectionDisplayName: "Primary calendar",
            scheduleRevisionID: Self.scheduleRevisionID,
            scheduleRevisionNumber: 8,
            previewHash: String(repeating: "a", count: 64),
            createCount: 2,
            updateCount: 0,
            deleteCount: 0,
            noopCount: 0,
            changes: [Self.preview.changes[0], GoogleSchedulePublicationChange(
                ordinal: 1,
                slotID: Self.slotID,
                sourceBlockID: UUID(),
                operation: .create,
                providerResourceID: nil,
                providerETag: nil,
                summary: "Busy",
                startsAt: Self.now.addingTimeInterval(8_000),
                endsAt: Self.now.addingTimeInterval(9_000)
            )],
            expiresAt: Self.now.addingTimeInterval(10 * 60)
        )
        #expect(!duplicateSlots.hasValidShape)
    }

    @Test("aggregate delivery state must match exact count priority")
    func aggregateStatusCountPriority() {
        let invalidDelivering = GoogleSchedulePublicationStatus(
            publicationID: Self.publicationID,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            scheduleRevisionID: Self.scheduleRevisionID,
            state: .delivering,
            totalCount: 1,
            pendingCount: 1,
            deliveringCount: 0,
            publishedCount: 0,
            conflictedCount: 0,
            failedCount: 0,
            supersededCount: 0,
            createdAt: Self.now,
            completedAt: nil,
            lastErrorCode: nil
        )
        #expect(!invalidDelivering.hasValidShape)

        let invalidPartial = GoogleSchedulePublicationStatus(
            publicationID: Self.publicationID,
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            scheduleRevisionID: Self.scheduleRevisionID,
            state: .partiallyPublished,
            totalCount: 2,
            pendingCount: 1,
            deliveringCount: 0,
            publishedCount: 1,
            conflictedCount: 0,
            failedCount: 0,
            supersededCount: 0,
            createdAt: Self.now,
            completedAt: Self.now.addingTimeInterval(10),
            lastErrorCode: nil
        )
        #expect(!invalidPartial.hasValidShape)
        #expect(Self.completedStatus.hasValidShape)
    }

    nonisolated static let now = Date(timeIntervalSince1970: 1_788_076_800)
    static let configuration = "https://api.example.com|auth=test-binding"
    static let accountID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    static let collectionID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    static let scheduleRevisionID = UUID(
        uuidString: "33333333-3333-4333-8333-333333333333"
    )!
    static let previewID = UUID(uuidString: "44444444-4444-4444-8444-444444444444")!
    static let slotID = UUID(uuidString: "55555555-5555-4555-8555-555555555555")!
    static let blockID = UUID(uuidString: "66666666-6666-4666-8666-666666666666")!
    static let publicationID = UUID(
        uuidString: "77777777-7777-4777-8777-777777777777"
    )!
    static let capability = "dw_gsa1_" + String(repeating: "A", count: 43)

    static let preview = GoogleSchedulePublicationPreview(
        id: previewID,
        accountID: accountID,
        collectionID: collectionID,
        collectionRevision: 4,
        collectionDisplayName: "Primary calendar",
        scheduleRevisionID: scheduleRevisionID,
        scheduleRevisionNumber: 8,
        previewHash: String(repeating: "a", count: 64),
        createCount: 1,
        updateCount: 0,
        deleteCount: 0,
        noopCount: 0,
        changes: [GoogleSchedulePublicationChange(
            ordinal: 0,
            slotID: slotID,
            sourceBlockID: blockID,
            operation: .create,
            providerResourceID: nil,
            providerETag: nil,
            summary: "Busy",
            startsAt: now.addingTimeInterval(3_600),
            endsAt: now.addingTimeInterval(7_200)
        )],
        expiresAt: now.addingTimeInterval(10 * 60)
    )

    static let approval = GoogleSchedulePublicationApproval(
        previewID: previewID,
        approvalCapability: capability,
        expiresAt: now.addingTimeInterval(10 * 60)
    )

    static let acceptance = GoogleSchedulePublicationAccepted(
        publicationID: publicationID,
        replayed: false
    )

    static let completedStatus = GoogleSchedulePublicationStatus(
        publicationID: publicationID,
        accountID: accountID,
        collectionID: collectionID,
        scheduleRevisionID: scheduleRevisionID,
        state: .published,
        totalCount: 1,
        pendingCount: 0,
        deliveringCount: 0,
        publishedCount: 1,
        conflictedCount: 0,
        failedCount: 0,
        supersededCount: 0,
        createdAt: now,
        completedAt: now.addingTimeInterval(10),
        lastErrorCode: nil
    )

    private static func encodePlannerSnapshot(_ snapshot: PlannerSnapshot) throws -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(snapshot)
    }

    private static func makeAuthoritativePlanner(
        directoryPrefix: String,
        keyByte: UInt8
    ) throws -> (planner: PlannerStore, directory: URL) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "\(directoryPrefix)-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let persistence = EncryptedPlannerPersistence(
            fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
            key: try PlannerEncryptionKey(data: Data(repeating: keyByte, count: 32))
        )
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configuration,
            generatedAt: now,
            asOf: now,
            horizonStart: now,
            horizonEnd: now.addingTimeInterval(7 * 24 * 60 * 60),
            timezoneName: "UTC"
        )
        let proof = DayWeavePublishedScheduleProof(
            configurationIdentifier: configuration,
            revisionID: scheduleRevisionID,
            revision: "8:\(scheduleRevisionID.uuidString.lowercased())",
            revisionNumber: 8,
            inputDigest: "sha256:\(String(repeating: "b", count: 64))",
            asOf: provenance.asOf,
            horizonStart: provenance.horizonStart,
            horizonEnd: provenance.horizonEnd,
            timezoneName: provenance.timezoneName,
            publishedAt: now,
            publishedBlocks: []
        )
        let planner = PlannerStore(
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: provenance,
            publishedScheduleProof: proof,
            previewValidatedForCurrentLaunch: true,
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { now }
        )
        planner.flushPersistence()
        guard planner.persistenceError == nil else {
            throw PlannerGoogleSchedulePublicationRecoveryError.encryptedPersistenceRequired
        }
        return (planner, directory)
    }

    private static func makeNearMaximumWirePreview() throws -> (
        preview: GoogleSchedulePublicationPreview,
        wireBytes: Int,
        providerCharacters: Int,
        bytesPerProviderCharacter: Int
    ) {
        let changeCount = GoogleSchedulePublicationPreview.maximumChanges
        let identities = (0..<changeCount).map { _ in (UUID(), UUID()) }
        let bytesPerProviderCharacter = changeCount * 2

        func makePreview(providerCharacters: Int) -> GoogleSchedulePublicationPreview {
            let providerBinding = String(repeating: "/", count: providerCharacters)
            let changes = identities.enumerated().map { index, identity in
                GoogleSchedulePublicationChange(
                    ordinal: UInt32(index),
                    slotID: identity.0,
                    sourceBlockID: identity.1,
                    operation: .update,
                    providerResourceID: providerBinding,
                    providerETag: providerBinding,
                    summary: "Busy",
                    startsAt: now.addingTimeInterval(3_600),
                    endsAt: now.addingTimeInterval(7_200)
                )
            }
            return GoogleSchedulePublicationPreview(
                id: previewID,
                accountID: accountID,
                collectionID: collectionID,
                collectionRevision: 4,
                collectionDisplayName: "Primary calendar",
                scheduleRevisionID: scheduleRevisionID,
                scheduleRevisionNumber: 8,
                previewHash: String(repeating: "a", count: 64),
                createCount: 0,
                updateCount: UInt32(changeCount),
                deleteCount: 0,
                noopCount: 0,
                changes: changes,
                expiresAt: now.addingTimeInterval(10 * 60)
            )
        }

        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        var providerCharacters = 500
        var preview = makePreview(providerCharacters: providerCharacters)
        var wireBytes = try encoder.encode(preview).count
        let transportLimit = DayWeaveAPIClient.maximumResponseBytes
        if wireBytes < transportLimit {
            providerCharacters += (transportLimit - wireBytes) / bytesPerProviderCharacter
            providerCharacters = min(providerCharacters, 1_000)
            preview = makePreview(providerCharacters: providerCharacters)
            wireBytes = try encoder.encode(preview).count
        }
        if wireBytes > transportLimit {
            let excessCharacters = (wireBytes - transportLimit + bytesPerProviderCharacter - 1)
                / bytesPerProviderCharacter
            providerCharacters -= excessCharacters
            preview = makePreview(providerCharacters: providerCharacters)
            wireBytes = try encoder.encode(preview).count
        }
        return (preview, wireBytes, providerCharacters, bytesPerProviderCharacter)
    }
}

@MainActor
private final class TestGoogleSchedulePublicationRecoveryStore:
    GoogleSchedulePublicationRecoveryStoring
{
    var value: GoogleSchedulePublicationRecoveryJournal?
    private(set) var saved: [GoogleSchedulePublicationRecoveryJournal] = []

    init(value: GoogleSchedulePublicationRecoveryJournal? = nil) {
        self.value = value
    }

    func loadGoogleSchedulePublicationRecoveryJournal() throws
        -> GoogleSchedulePublicationRecoveryJournal? {
        value
    }

    func saveGoogleSchedulePublicationRecoveryJournal(
        _ journal: GoogleSchedulePublicationRecoveryJournal
    ) throws {
        saved.append(journal)
        value = journal
    }

    func clearGoogleSchedulePublicationRecoveryJournal(
        _ expected: GoogleSchedulePublicationRecoveryJournal
    ) throws {
        guard value == expected else { throw TestGoogleSchedulePublicationError.changed }
        value = nil
    }
}

private actor TestGoogleSchedulePublicationEnqueueGate {
    private var entered = false
    private var released = false
    private var entryWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiters: [CheckedContinuation<Void, Never>] = []

    func enterAndWait() async {
        entered = true
        let waiters = entryWaiters
        entryWaiters.removeAll()
        waiters.forEach { $0.resume() }
        guard !released else { return }
        await withCheckedContinuation { continuation in
            releaseWaiters.append(continuation)
        }
    }

    func waitUntilEntered() async {
        guard !entered else { return }
        await withCheckedContinuation { continuation in
            entryWaiters.append(continuation)
        }
    }

    func release() {
        released = true
        let waiters = releaseWaiters
        releaseWaiters.removeAll()
        waiters.forEach { $0.resume() }
    }
}

private actor TestGoogleSchedulePublicationTransport: GoogleSchedulePublicationTransport {
    nonisolated let configurationIdentifier: String
    let preview: GoogleSchedulePublicationPreview
    let approval: GoogleSchedulePublicationApproval
    let acceptance: GoogleSchedulePublicationAccepted
    let status: GoogleSchedulePublicationStatus
    let approvalError: TestGoogleSchedulePublicationError?
    let enqueueGate: TestGoogleSchedulePublicationEnqueueGate?
    private(set) var approvalCallCount = 0
    private(set) var enqueueCallCount = 0
    private(set) var statusCallCount = 0

    init(
        configurationIdentifier: String,
        preview: GoogleSchedulePublicationPreview,
        approval: GoogleSchedulePublicationApproval,
        acceptance: GoogleSchedulePublicationAccepted,
        status: GoogleSchedulePublicationStatus,
        approvalError: TestGoogleSchedulePublicationError? = nil,
        enqueueGate: TestGoogleSchedulePublicationEnqueueGate? = nil
    ) {
        self.configurationIdentifier = configurationIdentifier
        self.preview = preview
        self.approval = approval
        self.acceptance = acceptance
        self.status = status
        self.approvalError = approvalError
        self.enqueueGate = enqueueGate
    }

    func previewGoogleSchedulePublication(
        accountID: UUID,
        request: GoogleSchedulePublicationPreviewRequest
    ) async throws -> GoogleSchedulePublicationPreview {
        preview
    }

    func approveGoogleSchedulePublication(
        accountID: UUID,
        previewID: UUID,
        expectedPreviewHash: String
    ) async throws -> GoogleSchedulePublicationApproval {
        approvalCallCount += 1
        if let approvalError { throw approvalError }
        return approval
    }

    func enqueueGoogleSchedulePublication(
        accountID: UUID,
        request: GoogleSchedulePublicationEnqueueRequest
    ) async throws -> GoogleSchedulePublicationAccepted {
        enqueueCallCount += 1
        await enqueueGate?.enterAndWait()
        return acceptance
    }

    func googleSchedulePublicationStatus(
        accountID: UUID,
        publicationID: UUID
    ) async throws -> GoogleSchedulePublicationStatus {
        statusCallCount += 1
        return status
    }
}

private enum TestGoogleSchedulePublicationError: Error, LocalizedError {
    case ambiguous(String)
    case changed

    var errorDescription: String? {
        switch self {
        case let .ambiguous(message): message
        case .changed: "changed"
        }
    }
}
#endif
