import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Expired-break alternative handoff", .serialized)
@MainActor
struct BreakAlternativePresentationTests {
    nonisolated private static let configuration = "https://api.example.test"
    nonisolated private static let binding = "break-alternative-binding"
    nonisolated private static let baseDate = Date(
        timeIntervalSince1970: 1_800_000_000
    )
    nonisolated private static let now = baseDate.addingTimeInterval(71)
    nonisolated private static let sourceItemID = id(1)
    nonisolated private static let sourceBlockID = id(2)
    nonisolated private static let sourceSessionID = id(3)
    nonisolated private static let deviceID = id(4)

    @Test("the policy includes only current published executable leaves in schedule order")
    func candidateIncludeExcludeMatrix() throws {
        let sourceOccurrenceID = Self.id(9)
        let source = try Self.pausedSession(occurrenceID: sourceOccurrenceID)
        let earlierID = Self.id(10)
        let laterID = Self.id(11)
        let sameItemID = Self.id(12)
        let eventID = Self.id(13)
        let breakID = Self.id(14)
        let hardID = Self.id(15)
        let localID = Self.id(16)
        let parentID = Self.id(17)
        let childID = Self.id(18)
        let terminalID = Self.id(19)
        let staleID = Self.id(20)
        let completedID = Self.id(21)
        let pinnedID = Self.id(22)
        let sameOccurrenceID = Self.id(23)
        let fixedID = Self.id(24)

        var blocks = [Self.sourceBlock(occurrenceID: sourceOccurrenceID)]
        blocks.append(Self.block(
            id: laterID,
            itemID: laterID,
            title: "Later leaf",
            startOffset: 7_200,
            placementReason: "  Protects the deep-work window.  "
        ))
        blocks.append(Self.block(
            id: earlierID,
            itemID: earlierID,
            title: "Earlier leaf",
            startOffset: 3_600,
            placementReason: "unsafe\nreason"
        ))
        blocks.append(Self.block(
            id: sameItemID,
            itemID: Self.sourceItemID,
            title: "Another source session",
            startOffset: 8_000,
            sessionIndex: 1
        ))
        blocks.append(Self.block(
            id: eventID,
            itemID: eventID,
            title: "Calendar event",
            startOffset: 4_000,
            kind: .event
        ))
        blocks.append(Self.block(
            id: breakID,
            itemID: breakID,
            title: "Protected break",
            startOffset: 4_200,
            kind: .breakTime
        ))
        blocks.append(Self.block(
            id: hardID,
            itemID: hardID,
            title: "Hard block",
            startOffset: 4_400,
            isHardConstraint: true
        ))
        blocks.append(Self.block(
            id: localID,
            itemID: localID,
            title: "Local-only draft",
            startOffset: 4_600,
            origin: .local
        ))
        blocks.append(Self.block(
            id: parentID,
            itemID: parentID,
            title: "Hierarchy parent",
            startOffset: 4_800
        ))
        blocks.append(Self.block(
            id: terminalID,
            itemID: terminalID,
            title: "Already finished session",
            startOffset: 5_000
        ))
        blocks.append(Self.block(
            id: staleID,
            itemID: staleID,
            title: "Stale revision",
            startOffset: 5_200,
            itemRevision: 1
        ))
        blocks.append(Self.block(
            id: completedID,
            itemID: completedID,
            title: "Completed block",
            startOffset: 5_400,
            status: .completed
        ))
        var pinned = Self.block(
            id: pinnedID,
            itemID: pinnedID,
            title: "Pinned placement",
            startOffset: 5_600
        )
        pinned.previewKind = "pinned"
        blocks.append(pinned)
        blocks.append(Self.block(
            id: sameOccurrenceID,
            itemID: sameOccurrenceID,
            title: "Same recurring occurrence",
            startOffset: 5_800,
            occurrenceID: sourceOccurrenceID
        ))
        var fixed = Self.block(
            id: fixedID,
            itemID: fixedID,
            title: "Non-flexible placement",
            startOffset: 6_000
        )
        fixed.isFlexible = false
        blocks.append(fixed)

        let items = [
            try Self.item(id: Self.sourceItemID, status: "paused"),
            try Self.item(id: earlierID),
            try Self.item(id: laterID),
            try Self.item(id: eventID, kind: "event"),
            try Self.item(id: breakID, kind: "break"),
            try Self.item(id: hardID),
            try Self.item(id: localID),
            try Self.item(id: parentID),
            try Self.item(id: childID, parentID: parentID),
            try Self.item(id: terminalID),
            try Self.item(id: staleID, revision: 2),
            try Self.item(id: completedID),
            try Self.item(id: pinnedID),
            try Self.item(id: sameOccurrenceID),
            try Self.item(id: fixedID),
        ]
        let terminal = try Self.terminalSession(itemID: terminalID)
        var state = Self.executionState(active: source, acknowledged: true)
        state.revision = source.revision + terminal.revision
        state.historyWindow = [terminal, source]
        state.historyWindowRevision = state.revision
        state.terminalOutcomes[terminal.id] = .init(
            session: terminal,
            recordedAt: Self.now,
            projection: .notRequired
        )
        let planner = Self.planner(blocks: blocks, items: items, state: state)

        let presentation = try #require(BreakAlternativePolicy.presentation(
            source: .init(session: source),
            selectedCandidateID: nil,
            planner: planner
        ))

        #expect(presentation.candidates.map(\.id) == [earlierID, laterID])
        #expect(presentation.candidates.map(\.isNextInPlan) == [true, false])
        #expect(presentation.candidates[0].placementReason == nil)
        #expect(presentation.candidates[1].placementReason
            == "Protects the deep-work window.")
    }

    @Test("incomplete or stale publication evidence fails closed")
    func incompletePublicationEvidenceFailsClosed() throws {
        let source = try Self.pausedSession()
        let candidateID = Self.id(30)
        let blocks = [
            Self.sourceBlock(),
            Self.block(
                id: candidateID,
                itemID: candidateID,
                title: "Candidate",
                startOffset: 3_600
            ),
        ]
        let items = [
            try Self.item(id: Self.sourceItemID, status: "paused"),
            try Self.item(id: candidateID),
        ]
        let state = Self.executionState(active: source, acknowledged: true)

        let unpublished = Self.planner(
            blocks: blocks,
            items: items,
            state: state,
            includeProof: false
        )
        #expect(BreakAlternativePolicy.presentation(
            source: .init(session: source),
            selectedCandidateID: nil,
            planner: unpublished
        )?.candidates.isEmpty == true)

        let incompleteProof = Self.planner(
            blocks: blocks,
            items: items,
            state: state,
            proofBlocks: [Self.proof(for: blocks[0])]
        )
        #expect(BreakAlternativePolicy.presentation(
            source: .init(session: source),
            selectedCandidateID: nil,
            planner: incompleteProof
        )?.candidates.isEmpty == true)

        let staleLaunch = Self.planner(
            blocks: blocks,
            items: items,
            state: state,
            previewValidatedForCurrentLaunch: false
        )
        #expect(BreakAlternativePolicy.presentation(
            source: .init(session: source),
            selectedCandidateID: nil,
            planner: staleLaunch
        )?.candidates.isEmpty == true)
    }

    @Test("global execution and publication fences fail the handoff closed")
    func globalFenceMatrixFailsClosed() throws {
        let source = try Self.pausedSession()
        let candidateID = Self.id(32)
        let blocks = [
            Self.sourceBlock(),
            Self.block(
                id: candidateID,
                itemID: candidateID,
                title: "Candidate",
                startOffset: 3_600
            ),
        ]
        let items = [
            try Self.item(id: Self.sourceItemID, status: "paused"),
            try Self.item(id: candidateID),
        ]
        let sourceReference = BreakAlternativeHandoffSource(session: source)
        let baseState = Self.executionState(active: source, acknowledged: true)
        func candidateIDs(in planner: PlannerStore) -> [UUID] {
            BreakAlternativePolicy.presentation(
                source: sourceReference,
                selectedCandidateID: nil,
                planner: planner
            )?.candidates.map(\.id) ?? []
        }

        #expect(candidateIDs(in: Self.planner(
            blocks: blocks,
            items: items,
            state: baseState
        )) == [candidateID])

        var commandState = baseState
        let resume = DayWeaveExecutionCommand.resume(sessionID: source.id)
        let resumeRequest = DayWeaveExecutionCommandRequest(
            expectedRevision: source.revision,
            command: resume
        )
        commandState.pendingCommand = DayWeavePendingExecutionCommand(
            idempotencyKey: "break-alternative-pending-resume",
            bindingIdentifier: Self.binding,
            expectedRevision: source.revision,
            identity: .init(session: source),
            command: resume,
            encodedRequest: try DayWeaveExecutionWireCodec.encode(resumeRequest),
            priorSession: source,
            focusedBlockID: Self.sourceBlockID,
            canonicalProjectionEligibleAtLeaseStart: true,
            stagedAt: Self.baseDate
        )
        #expect(candidateIDs(in: Self.planner(
            blocks: blocks,
            items: items,
            state: commandState
        )).isEmpty)

        #expect(candidateIDs(in: Self.planner(
            blocks: blocks,
            items: items,
            state: baseState,
            pendingSchedulePublication: try Self.pendingSchedulePublication()
        )).isEmpty)

        let deferred = try Self.deferredSession()
        var deferredState = baseState
        deferredState.revision += deferred.revision
        deferredState.historyWindow = [deferred, source]
        deferredState.historyWindowRevision = deferredState.revision
        deferredState.terminalOutcomes[deferred.id] = .init(
            session: deferred,
            recordedAt: deferred.updatedAt,
            projection: .notRequired
        )
        #expect(candidateIDs(in: Self.planner(
            blocks: blocks,
            items: items,
            state: deferredState,
            deferredExecutionPublicationSessionIDs: [deferred.id]
        )).isEmpty)

        let canonicalMutation = PendingCanonicalMutation(
            id: Self.id(33),
            itemID: candidateID,
            occurrenceID: nil,
            sessionIndex: 0,
            desiredStatus: .completed,
            baseRevision: 1,
            createdAt: Self.baseDate,
            disposition: .pending,
            diagnostic: nil
        )
        #expect(candidateIDs(in: Self.planner(
            blocks: blocks,
            items: items,
            state: baseState,
            pendingCanonicalMutations: [canonicalMutation]
        )).isEmpty)

        #expect(candidateIDs(in: Self.planner(
            blocks: blocks,
            items: items,
            state: baseState,
            pendingProposalApplicationMutation: try Self.pendingProposalMutation()
        )).isEmpty)

        let locked = Self.planner(blocks: blocks, items: items, state: baseState)
        #expect(locked.beginExecutionSync())
        #expect(!locked.canMutatePlan)
        #expect(candidateIDs(in: locked).isEmpty)
        locked.endCanonicalSync()
    }

    @Test("Choose another durably acknowledges without an execution command and survives restart")
    func exactAcknowledgmentHasNoCommandAndDoesNotReopen() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let fixture = try Self.actionFixture(persistence: context.persistence)
        fixture.planner.destination = .calendar
        let notifications = RecordingAlternativeNotificationCoordinator()
        let transport = NoCommandExecutionTransport()
        let sync = Self.sync(
            planner: fixture.planner,
            transport: transport,
            notifications: notifications
        )

        #expect(await sync.chooseAnotherAfterExpiredBreak() == .success)
        #expect(fixture.planner.executionState.activeSession == fixture.source)
        #expect(fixture.planner.executionState.activeSession?.status == .paused)
        #expect(fixture.planner.executionState.acknowledgedExpiredPause
            == .init(sessionID: fixture.source.id, revision: fixture.source.revision))
        #expect(fixture.planner.destination == .today)
        #expect(sync.breakAlternativePresentation?.candidates.map(\.id)
            == fixture.candidateIDs)
        #expect(await transport.commandCount == 0)
        #expect(await transport.snapshotCount == 0)
        #expect(await notifications.observations.first?.sessionID == fixture.source.id)
        #expect(await notifications.observations.first?.acknowledged
            == .init(sessionID: fixture.source.id, revision: fixture.source.revision))

        let restartedPlanner = PlannerStore(
            persistence: context.persistence,
            now: { Self.now }
        )
        let restarted = Self.sync(
            planner: restartedPlanner,
            transport: NoCommandExecutionTransport(),
            notifications: DayWeaveNoopBreakNotificationCoordinator()
        )
        #expect(!restarted.expiredBreakChoiceRequired)
        #expect(!restarted.expiredBreakResolutionShouldBePresented)
        #expect(restartedPlanner.executionState.activeSession?.status == .paused)
    }

    @Test("notification cancellation failure keeps the resolver open and never routes")
    func cancellationFailureStaysOpen() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let fixture = try Self.actionFixture(persistence: context.persistence)
        fixture.planner.destination = .calendar
        let notifications = FailingAlternativeNotificationCoordinator()
        let transport = NoCommandExecutionTransport()
        let sync = Self.sync(
            planner: fixture.planner,
            transport: transport,
            notifications: notifications
        )

        #expect(await sync.chooseAnotherAfterExpiredBreak() == .unexpectedFailure)
        #expect(sync.expiredBreakChoiceRequired)
        #expect(sync.expiredBreakResolutionShouldBePresented)
        #expect(fixture.planner.executionState.acknowledgedExpiredPause == nil)
        #expect(fixture.planner.destination == .calendar)
        #expect(sync.breakAlternativePresentation == nil)
        #expect(await transport.commandCount == 0)
        #expect(await notifications.reconcileCount >= 2)
    }

    @Test("a fresh coordinator removes the captured request hidden during delivery")
    func freshCoordinatorHiddenDeliveryStillCancelsExact() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let fixture = try Self.actionFixture(persistence: context.persistence)
        let identifier = try #require(
            DayWeaveBreakNotificationContract.descriptor(for: fixture.source)?.identifier
        )
        let center = FreshTransitionAlternativeNotificationCenter(
            inFlightIdentifier: identifier
        )
        let notifications = DayWeaveBreakNotificationCoordinator(
            center: center,
            now: { Self.now },
            removalVerificationDelay: .zero,
            sleep: { _ in }
        )
        let sync = Self.sync(
            planner: fixture.planner,
            transport: NoCommandExecutionTransport(),
            notifications: notifications
        )

        #expect(await sync.chooseAnotherAfterExpiredBreak() == .success)
        #expect(fixture.planner.executionState.acknowledgedExpiredPause
            == .init(sessionID: fixture.source.id, revision: fixture.source.revision))
        #expect(await center.removedPendingIdentifiers == [identifier])
        #expect(await center.removedDeliveredIdentifiers == [identifier])
        #expect(await center.allIdentifiersAreAbsent)
    }

    @Test("a lease change during cancellation is reconciled and cannot be acknowledged")
    func stateChangeDuringCancellationFailsExact() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let fixture = try Self.actionFixture(persistence: context.persistence)
        fixture.planner.destination = .calendar
        let notifications = GatedAlternativeNotificationCoordinator()
        let sync = Self.sync(
            planner: fixture.planner,
            transport: NoCommandExecutionTransport(),
            notifications: notifications
        )

        let action = Task { @MainActor in
            await sync.chooseAnotherAfterExpiredBreak()
        }
        await notifications.waitUntilEntered()
        let replacement = try Self.pausedSession(
            id: Self.id(31),
            revision: 3,
            pauseUntil: Self.now.addingTimeInterval(600)
        )
        var replacementState = Self.executionState(
            active: replacement,
            acknowledged: false
        )
        replacementState.revision = replacement.revision
        replacementState.historyWindowRevision = replacement.revision
        try fixture.planner.persistExecutionState(replacementState)
        await notifications.release()

        #expect(await action.value == .conflict)
        #expect(fixture.planner.executionState.activeSession == replacement)
        #expect(fixture.planner.executionState.acknowledgedExpiredPause == nil)
        #expect(fixture.planner.destination == .calendar)
        #expect(sync.breakAlternativePresentation == nil)
        #expect(await notifications.observations.last?.sessionID == replacement.id)
        #expect(await notifications.observations.last?.acknowledged == nil)
    }

    @Test("storage failure restores the unresolved lease and authoritative reminder input")
    func persistenceFailureStaysOpenAndReconcilesReminder() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let fixture = try Self.actionFixture(persistence: context.persistence)
        fixture.planner.destination = .calendar
        fixture.planner.flushPersistence()
        let writer = PlannerStore(persistence: context.persistence, now: { Self.now })
        writer.lastScheduleMessage = "A newer process committed first"
        writer.flushPersistence()
        let notifications = RecordingAlternativeNotificationCoordinator()
        let sync = Self.sync(
            planner: fixture.planner,
            transport: NoCommandExecutionTransport(),
            notifications: notifications
        )

        #expect(await sync.chooseAnotherAfterExpiredBreak() == .localStorageFailure)
        #expect(sync.expiredBreakChoiceRequired)
        #expect(fixture.planner.executionState.acknowledgedExpiredPause == nil)
        #expect(fixture.planner.destination == .calendar)
        #expect(sync.breakAlternativePresentation == nil)
        #expect(await notifications.observations.count == 2)
        #expect(await notifications.observations.last?.sessionID == fixture.source.id)
        #expect(await notifications.observations.last?.acknowledged == nil)

        let restarted = PlannerStore(persistence: context.persistence, now: { Self.now })
        #expect(restarted.executionState.acknowledgedExpiredPause == nil)
    }

    @Test("candidate selection only highlights and an invalidating refresh clears it")
    func selectionIsNonMutatingAndRefreshSafe() async throws {
        let context = try Self.persistenceContext()
        defer { try? FileManager.default.removeItem(at: context.directory) }
        let fixture = try Self.actionFixture(persistence: context.persistence)
        let transport = NoCommandExecutionTransport()
        let sync = Self.sync(
            planner: fixture.planner,
            transport: transport,
            notifications: RecordingAlternativeNotificationCoordinator()
        )
        #expect(await sync.chooseAnotherAfterExpiredBreak() == .success)
        let leaseBeforeSelection = fixture.planner.executionState
        let blocksBeforeSelection = fixture.planner.blocks
        let selectedID = try #require(fixture.candidateIDs.first)

        sync.selectBreakAlternative(selectedID)
        #expect(sync.breakAlternativePresentation?.selectedCandidateID == selectedID)
        #expect(fixture.planner.selectedBlockID == selectedID)
        #expect(fixture.planner.executionState == leaseBeforeSelection)
        #expect(fixture.planner.blocks == blocksBeforeSelection)
        #expect(await transport.commandCount == 0)

        let index = try #require(fixture.planner.blocks.firstIndex {
            $0.id == selectedID
        })
        fixture.planner.blocks[index].status = .completed
        #expect(sync.breakAlternativePresentation?.selectedCandidateID == nil)
        sync.reconcileBreakAlternativeSelection()
        #expect(fixture.planner.selectedBlockID == nil)
        #expect(fixture.planner.executionState == leaseBeforeSelection)
        #expect(await transport.commandCount == 0)
    }

    @Test("the empty handoff gives explicit paused-session guidance")
    func emptyGuidanceIsExplicit() {
        #expect(BreakAlternativePresentation.emptyGuidance
            == "Your current item remains paused. Move it later, complete it, or skip it before another item can start.")
        #expect(BreakAlternativePresentation.selectionGuidance.contains(
            "only highlights"
        ))
    }

    @Test("the handoff remains inside RootView's external app-lock boundary")
    func appLockStillWrapsRootView() throws {
        let package = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let appSource = try String(
            contentsOf: package.appendingPathComponent(
                "Sources/DayWeaveMac/DayWeaveMacApp.swift"
            ),
            encoding: .utf8
        )
        let rootSource = try String(
            contentsOf: package.appendingPathComponent(
                "Sources/DayWeaveMac/Views/RootView.swift"
            ),
            encoding: .utf8
        )
        let onboardingHostSource = try String(
            contentsOf: package.appendingPathComponent(
                "Sources/DayWeaveMac/Views/DayWeaveOnboardingHost.swift"
            ),
            encoding: .utf8
        )

        #expect(appSource.contains("if appLock.isContentAvailable"))
        #expect(appSource.contains("DayWeaveOnboardingHost()"))
        #expect(onboardingHostSource.contains("RootView()"))
        #expect(rootSource.contains("BreakAlternativeHandoffView"))
        #expect(!appSource.contains("BreakAlternativeHandoffView"))
        #expect(!onboardingHostSource.contains("BreakAlternativeHandoffView"))
    }

    private struct ActionFixture {
        let planner: PlannerStore
        let source: DayWeaveExecutionSession
        let candidateIDs: [UUID]
    }

    private static func actionFixture(
        persistence: EncryptedPlannerPersistence
    ) throws -> ActionFixture {
        let source = try pausedSession()
        let firstID = id(40)
        let secondID = id(41)
        let blocks = [
            sourceBlock(),
            block(
                id: firstID,
                itemID: firstID,
                title: "First alternative",
                startOffset: 3_600,
                placementReason: "Fits the current energy window."
            ),
            block(
                id: secondID,
                itemID: secondID,
                title: "Second alternative",
                startOffset: 5_400
            ),
        ]
        let items = [
            try item(id: sourceItemID, status: "paused"),
            try item(id: firstID),
            try item(id: secondID),
        ]
        return .init(
            planner: planner(
                blocks: blocks,
                items: items,
                state: executionState(active: source, acknowledged: false),
                persistence: persistence
            ),
            source: source,
            candidateIDs: [firstID, secondID]
        )
    }

    private static func planner(
        blocks: [ScheduleBlock],
        items: [DayWeaveCanonicalItem],
        state: DayWeaveExecutionDurableState,
        includeProof: Bool = true,
        proofBlocks: [DayWeavePublishedScheduleBlockProof]? = nil,
        previewValidatedForCurrentLaunch: Bool = true,
        pendingCanonicalMutations: [PendingCanonicalMutation] = [],
        deferredExecutionPublicationSessionIDs: Set<UUID> = [],
        pendingSchedulePublication: PendingSchedulePublication? = nil,
        pendingProposalApplicationMutation:
            DayWeavePendingProposalApplicationMutation? = nil,
        persistence: EncryptedPlannerPersistence? = nil
    ) -> PlannerStore {
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configuration,
            generatedAt: baseDate,
            asOf: baseDate,
            horizonStart: baseDate.addingTimeInterval(-3_600),
            horizonEnd: baseDate.addingTimeInterval(86_400),
            timezoneName: "UTC"
        )
        let revisionID = id(900)
        let publishedBlocks = proofBlocks ?? blocks.compactMap { block in
            block.syncOrigin == .canonicalPreview ? proof(for: block) : nil
        }
        let publicationProof = includeProof ? DayWeavePublishedScheduleProof(
            configurationIdentifier: configuration,
            revisionID: revisionID,
            revision: "1:\(revisionID.uuidString.lowercased())",
            revisionNumber: 1,
            inputDigest: "sha256:\(String(repeating: "a", count: 64))",
            asOf: provenance.asOf,
            horizonStart: provenance.horizonStart,
            horizonEnd: provenance.horizonEnd,
            timezoneName: provenance.timezoneName,
            publishedAt: baseDate,
            publishedBlocks: publishedBlocks
        ) : nil
        return PlannerStore(
            blocks: blocks,
            canonicalItems: items,
            pendingCanonicalMutations: pendingCanonicalMutations,
            deferredExecutionPublicationSessionIDs:
                deferredExecutionPublicationSessionIDs,
            canonicalConfigurationIdentifier: configuration,
            schedulePreviewProvenance: provenance,
            publishedScheduleProof: publicationProof,
            pendingSchedulePublication: pendingSchedulePublication,
            pendingProposalApplicationMutation:
                pendingProposalApplicationMutation,
            executionState: state,
            previewValidatedForCurrentLaunch: previewValidatedForCurrentLaunch,
            persistence: persistence,
            restoreFromPersistence: false,
            autosaveDelay: .seconds(60),
            now: { now }
        )
    }

    private static func sync(
        planner: PlannerStore,
        transport: any DayWeaveExecutionTransport,
        notifications: any DayWeaveBreakNotificationCoordinating
    ) -> ExecutionSyncStore {
        let connection = DayWeaveExecutionConnection(
            canonicalConfigurationIdentifier: configuration,
            bindingIdentifier: binding,
            transport: transport
        )
        return ExecutionSyncStore(
            planner: planner,
            connectionProvider: { connection },
            now: { now },
            makeUUID: { id(901) },
            breakNotificationCoordinator: notifications
        )
    }

    private static func executionState(
        active: DayWeaveExecutionSession,
        acknowledged: Bool
    ) -> DayWeaveExecutionDurableState {
        var state = DayWeaveExecutionDurableState.empty
        state.deviceID = deviceID
        state.bindingIdentifier = binding
        state.revision = active.revision
        state.activeSession = active
        state.historyWindow = [active]
        state.historyWindowRevision = active.revision
        state.historyContinuityEstablished = true
        state.historyVerified = true
        state.leaseProjectionEligibility[active.id] = true
        state.presentedBlockIDs = [sourceBlockID]
        if acknowledged {
            state.acknowledgedExpiredPause = .init(
                sessionID: active.id,
                revision: active.revision
            )
        }
        return state
    }

    private static func sourceBlock(
        occurrenceID: UUID? = nil
    ) -> ScheduleBlock {
        block(
            id: sourceBlockID,
            itemID: sourceItemID,
            title: "Paused source",
            startOffset: 0,
            status: .paused,
            occurrenceID: occurrenceID
        )
    }

    private static func block(
        id: UUID,
        itemID: UUID,
        title: String,
        startOffset: TimeInterval,
        status: PlannerItemStatus = .scheduled,
        kind: PlannerItemKind = .task,
        isHardConstraint: Bool = false,
        origin: ScheduleBlockOrigin = .canonicalPreview,
        itemRevision: UInt64 = 1,
        sessionIndex: UInt16 = 0,
        occurrenceID: UUID? = nil,
        placementReason: String? = nil
    ) -> ScheduleBlock {
        ScheduleBlock(
            id: id,
            title: title,
            kind: kind,
            start: baseDate.addingTimeInterval(startOffset),
            end: baseDate.addingTimeInterval(startOffset + 1_800),
            status: status,
            project: nil,
            notes: "",
            energy: .medium,
            isFlexible: !isHardConstraint,
            isHardConstraint: isHardConstraint,
            actualMinutes: nil,
            sourceItemID: itemID,
            sourceItemRevision: itemRevision,
            occurrenceID: occurrenceID,
            sessionIndex: sessionIndex,
            syncOrigin: origin,
            placementReason: placementReason,
            previewKind: "planned",
            occurrenceFullyScheduled: true
        )
    }

    private static func proof(
        for block: ScheduleBlock
    ) -> DayWeavePublishedScheduleBlockProof {
        DayWeavePublishedScheduleBlockProof(
            id: block.id,
            itemID: block.sourceItemID!,
            itemRevision: block.sourceItemRevision!,
            occurrenceID: block.occurrenceID,
            sessionIndex: block.sessionIndex!,
            start: block.start,
            end: block.end,
            kind: block.previewKind!
        )
    }

    private static func item(
        id: UUID,
        kind: String = "task",
        status: String = "scheduled",
        parentID: UUID? = nil,
        revision: UInt64 = 1
    ) throws -> DayWeaveCanonicalItem {
        let parent = parentID.map { "\"\($0.uuidString.lowercased())\"" } ?? "null"
        let data = Data(#"""
        {
          "id":"\#(id.uuidString.lowercased())","is_sensitive":false,
          "kind":"\#(kind)","status":"\#(status)","title":"Item \#(id.uuidString.prefix(4))",
          "notes":null,"timezone_name":"UTC","duration_seconds":1800,
          "deadline_at":null,"earliest_start_at":null,"recurrence":null,
          "flexible_constraints":{},"split_policy":{"type":"indivisible"},
          "importance":50,"urgency":50,"parent_id":\#(parent),"sibling_order":0,
          "is_executable":true,"revision":\#(revision),
          "created_at":"2027-01-15T08:00:00Z","updated_at":"2027-01-15T08:00:00Z",
          "completed_at":null,"deleted_at":null
        }
        """#.utf8)
        return try decoder().decode(DayWeaveCanonicalItem.self, from: data)
    }

    private static func pausedSession(
        id: UUID = sourceSessionID,
        revision: UInt64 = 2,
        pauseUntil: Date = baseDate.addingTimeInterval(70),
        occurrenceID: UUID? = nil
    ) throws -> DayWeaveExecutionSession {
        try session(
            id: id,
            itemID: sourceItemID,
            status: .paused,
            revision: revision,
            updatedAt: baseDate.addingTimeInterval(10),
            pauseUntil: pauseUntil,
            occurrenceID: occurrenceID,
            endedAt: nil
        )
    }

    private static func terminalSession(
        itemID: UUID
    ) throws -> DayWeaveExecutionSession {
        try session(
            id: id(700),
            itemID: itemID,
            status: .completed,
            revision: 2,
            updatedAt: baseDate.addingTimeInterval(20),
            pauseUntil: nil,
            occurrenceID: nil,
            endedAt: baseDate.addingTimeInterval(20)
        )
    }

    private static func deferredSession() throws -> DayWeaveExecutionSession {
        let updatedAt = baseDate.addingTimeInterval(20)
        return try session(
            id: id(34),
            itemID: id(35),
            status: .deferred,
            revision: 2,
            updatedAt: updatedAt,
            pauseUntil: nil,
            occurrenceID: nil,
            endedAt: updatedAt,
            moveStart: updatedAt.addingTimeInterval(3_600),
            moveEnd: updatedAt.addingTimeInterval(5_400)
        )
    }

    private static func session(
        id: UUID,
        itemID: UUID,
        status: DayWeaveExecutionStatus,
        revision: UInt64,
        updatedAt: Date,
        pauseUntil: Date?,
        occurrenceID: UUID?,
        endedAt: Date?,
        moveStart: Date? = nil,
        moveEnd: Date? = nil
    ) throws -> DayWeaveExecutionSession {
        let isPaused = status == .paused
        let isTerminal = !status.isOpen
        let object: [String: Any] = [
            "id": id.uuidString.lowercased(),
            "item_id": itemID.uuidString.lowercased(),
            "item_revision": 1,
            "occurrence_id": occurrenceID?.uuidString.lowercased() ?? NSNull(),
            "session_index": 0,
            "planned_block_id": (itemID == sourceItemID
                ? sourceBlockID : itemID).uuidString.lowercased(),
            "source_device_id": deviceID.uuidString.lowercased(),
            "status": status.rawValue,
            "revision": revision,
            "accumulated_seconds": 10,
            "actual_seconds": isTerminal ? 10 : NSNull(),
            "started_at": format(baseDate),
            "running_since": NSNull(),
            "paused_at": isPaused ? format(updatedAt) : NSNull(),
            "pause_until": pauseUntil.map(format) ?? NSNull(),
            "pause_reason": NSNull(),
            "move_start": moveStart.map(format) ?? NSNull(),
            "move_end": moveEnd.map(format) ?? NSNull(),
            "ended_at": endedAt.map(format) ?? NSNull(),
            "created_at": format(baseDate),
            "updated_at": format(updatedAt),
        ]
        return try decoder().decode(
            DayWeaveExecutionSession.self,
            from: JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        )
    }

    private static func pendingSchedulePublication() throws
        -> PendingSchedulePublication {
        let horizonStart = baseDate.addingTimeInterval(-3_600)
        let horizonEnd = baseDate.addingTimeInterval(86_400)
        let digest = "sha256:\(String(repeating: "b", count: 64))"
        let previewData = Data(#"""
        {
          "input_digest":"\#(digest)","source_item_count":0,
          "accepted_item_count":0,"source_item_revisions":{},
          "rejected_items":[],"ignored_previous_assignments":[],
          "plan":{"as_of":"\#(format(baseDate))",
          "horizon_start":"\#(format(horizonStart))",
          "horizon_end":"\#(format(horizonEnd))","blocks":[],
          "unscheduled":[],"decisions":[],"violations":[],
          "score":{"scheduled_minutes":0,"unscheduled_minutes":0,
          "soft_penalty":0,"moved_minutes":0},"occurrences":[]}
        }
        """#.utf8)
        let preview = try decoder().decode(
            DayWeaveSchedulePreview.self,
            from: previewData
        )
        let request = DayWeaveSchedulePreviewRequest(
            asOf: baseDate,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            timezoneName: "UTC",
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
        let provenance = SchedulePreviewProvenance(
            configurationIdentifier: configuration,
            generatedAt: baseDate,
            asOf: baseDate,
            horizonStart: horizonStart,
            horizonEnd: horizonEnd,
            timezoneName: "UTC"
        )
        return PendingSchedulePublication(
            configurationIdentifier: configuration,
            preparedRequest: .init(
                request: .init(
                    idempotencyKey: id(36),
                    expectedInputDigest: digest,
                    schedule: request
                ),
                body: Data("{}".utf8),
                bodySHA256: String(repeating: "d", count: 64)
            ),
            preview: preview,
            message: "Pending schedule publication",
            provenance: provenance,
            preparedAt: baseDate
        )
    }

    private static func pendingProposalMutation() throws
        -> DayWeavePendingProposalApplicationMutation {
        let reviewHash = "sha256:\(String(repeating: "c", count: 64))"
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return .apply(
            configurationIdentifier: configuration,
            proposalIDs: [id(37)],
            proposalRevisions: [1],
            expectedCommandIDs: [id(38)],
            previewID: id(39),
            expectedReviewHash: reviewHash,
            requestBody: try encoder.encode(DayWeaveProposalApplyRequest(
                expectedReviewHash: reviewHash
            )),
            idempotencyKey: "break-alternative-proposal",
            createdAt: baseDate
        )
    }

    private static func persistenceContext() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveBreakAlternativeTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let key = try PlannerEncryptionKey(data: Data(repeating: 37, count: 32))
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: key
            )
        )
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)
            guard let date = ISO8601DateFormatter().date(from: value) else {
                throw DecodingError.dataCorruptedError(
                    in: container,
                    debugDescription: "Invalid fixture date"
                )
            }
            return date
        }
        return decoder
    }

    nonisolated private static func format(_ date: Date) -> String {
        ISO8601DateFormatter().string(from: date)
    }

    nonisolated private static func id(_ number: Int) -> UUID {
        UUID(uuidString: String(
            format: "71000000-0000-4000-8000-%012d",
            number
        ))!
    }
}

private actor NoCommandExecutionTransport: DayWeaveExecutionTransport {
    private(set) var commandCount = 0
    private(set) var snapshotCount = 0

    func executionSnapshot() async throws -> DayWeaveExecutionSnapshot {
        snapshotCount += 1
        throw DayWeaveAPIError.responseDecodingFailed
    }

    func executionHistoryPage(
        limit: Int,
        offset: Int
    ) async throws -> DayWeaveExecutionHistoryPage {
        _ = limit
        _ = offset
        throw DayWeaveAPIError.responseDecodingFailed
    }

    func assessExecutionDefer(
        _ request: DayWeaveDeferAssessmentRequest
    ) async throws -> DayWeaveDeferAssessment {
        _ = request
        throw DayWeaveAPIError.responseDecodingFailed
    }

    nonisolated func encodedExecutionCommand(
        _ request: DayWeaveExecutionCommandRequest
    ) throws -> Data {
        try DayWeaveExecutionWireCodec.encode(request)
    }

    func applyExecutionCommand(
        encodedRequest: Data,
        idempotencyKey: String
    ) async throws -> DayWeaveExecutionMutation {
        _ = encodedRequest
        _ = idempotencyKey
        commandCount += 1
        throw DayWeaveAPIError.responseDecodingFailed
    }
}

private actor RecordingAlternativeNotificationCoordinator:
    DayWeaveBreakNotificationCoordinating
{
    struct Observation: Sendable {
        let sessionID: UUID?
        let acknowledged: DayWeaveExecutionSessionVersion?
    }

    private(set) var observations: [Observation] = []

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        .authorized
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .authorized
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        observations.append(.init(
            sessionID: session?.id,
            acknowledged: acknowledged
        ))
        guard let descriptor = DayWeaveBreakNotificationContract.descriptor(
            for: session
        ) else { return .canceled }
        return descriptor.version == acknowledged ? .canceled : .scheduled
    }

    func cancelExact(
        identifier: String,
        session: DayWeaveExecutionSession,
        acknowledged: DayWeaveExecutionSessionVersion
    ) async -> DayWeaveBreakNotificationReconcileResult {
        guard DayWeaveBreakNotificationContract.descriptor(for: session)?.identifier
                == identifier else {
            return .cancellationUnavailable
        }
        return await reconcile(session: session, acknowledged: acknowledged)
    }
}

private actor FailingAlternativeNotificationCoordinator:
    DayWeaveBreakNotificationCoordinating
{
    private(set) var reconcileCount = 0

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        .authorized
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .authorized
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        _ = session
        _ = acknowledged
        reconcileCount += 1
        return .cancellationUnavailable
    }

    func cancelExact(
        identifier: String,
        session: DayWeaveExecutionSession,
        acknowledged: DayWeaveExecutionSessionVersion
    ) async -> DayWeaveBreakNotificationReconcileResult {
        _ = identifier
        return await reconcile(session: session, acknowledged: acknowledged)
    }
}

private actor GatedAlternativeNotificationCoordinator:
    DayWeaveBreakNotificationCoordinating
{
    struct Observation: Sendable {
        let sessionID: UUID?
        let acknowledged: DayWeaveExecutionSessionVersion?
    }

    private(set) var observations: [Observation] = []
    private var firstEntered = false
    private var entryContinuations: [CheckedContinuation<Void, Never>] = []
    private var releaseContinuation: CheckedContinuation<Void, Never>?

    func authorizationState() async -> DayWeaveNotificationAuthorizationState {
        .authorized
    }

    func requestAuthorization() async -> DayWeaveNotificationAuthorizationRequestResult {
        .authorized
    }

    func reconcile(
        session: DayWeaveExecutionSession?,
        acknowledged: DayWeaveExecutionSessionVersion?
    ) async -> DayWeaveBreakNotificationReconcileResult {
        observations.append(.init(
            sessionID: session?.id,
            acknowledged: acknowledged
        ))
        if !firstEntered {
            firstEntered = true
            let waiters = entryContinuations
            entryContinuations.removeAll()
            waiters.forEach { $0.resume() }
            await withCheckedContinuation { continuation in
                releaseContinuation = continuation
            }
        }
        guard let descriptor = DayWeaveBreakNotificationContract.descriptor(
            for: session
        ) else { return .canceled }
        return descriptor.version == acknowledged ? .canceled : .scheduled
    }

    func cancelExact(
        identifier: String,
        session: DayWeaveExecutionSession,
        acknowledged: DayWeaveExecutionSessionVersion
    ) async -> DayWeaveBreakNotificationReconcileResult {
        guard DayWeaveBreakNotificationContract.descriptor(for: session)?.identifier
                == identifier else {
            return .cancellationUnavailable
        }
        return await reconcile(session: session, acknowledged: acknowledged)
    }

    func waitUntilEntered() async {
        guard !firstEntered else { return }
        await withCheckedContinuation { continuation in
            entryContinuations.append(continuation)
        }
    }

    func release() {
        releaseContinuation?.resume()
        releaseContinuation = nil
    }
}

private actor FreshTransitionAlternativeNotificationCenter:
    DayWeaveBreakNotificationCenter
{
    private var inFlight: Set<String>
    private var pending: Set<String> = []
    private var delivered: Set<String> = []
    private(set) var removedPendingIdentifiers: [String] = []
    private(set) var removedDeliveredIdentifiers: [String] = []

    init(inFlightIdentifier: String) {
        inFlight = [inFlightIdentifier]
    }

    var allIdentifiersAreAbsent: Bool {
        pending.isEmpty && inFlight.isEmpty && delivered.isEmpty
    }

    func authorizationState() -> DayWeaveNotificationAuthorizationState {
        .authorized
    }

    func requestAuthorization() throws -> Bool { true }

    func pendingRequestIdentifiers() -> [String] {
        // A freshly launched process can query after Notification Center has
        // removed the request from pending but before it appears as delivered.
        Array(pending)
    }

    func deliveredRequestIdentifiers() -> [String] {
        Array(delivered)
    }

    func add(_ request: DayWeaveBreakNotificationRequest) throws {
        pending.insert(request.identifier)
    }

    func removePendingRequestIdentifiers(_ identifiers: [String]) {
        removedPendingIdentifiers.append(contentsOf: identifiers)
        pending.subtract(identifiers)
    }

    func removeDeliveredRequestIdentifiers(_ identifiers: [String]) {
        // Model the pending-to-delivered transition occurring between the
        // initial snapshots and the explicit removal calls.
        delivered.formUnion(inFlight)
        inFlight.removeAll()
        removedDeliveredIdentifiers.append(contentsOf: identifiers)
        delivered.subtract(identifiers)
    }
}
#endif
