import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Crash-safe Google outbound workflow", .serialized)
@MainActor
struct GoogleOutboundStoreTests {
    @Test("exact intent is durable before the first preview request")
    func intentPersistsBeforePreview() async throws {
        let events = OutboundEventLog()
        let recovery = TestGoogleOutboundRecoveryStore(events: events)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            events: events,
            previewSteps: [.value(try Self.preview())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 7,
            operation: .upsert
        ))

        let saves = recovery.saved
        #expect(saves.count == 2)
        #expect(saves.first?.stage == .intent)
        #expect(saves.last?.stage == .previewed)
        #expect(
            events.snapshot() == [
                "save:intent", "preview", "save:previewed",
            ]
        )
        let calls = await transport.previewCallsSnapshot()
        #expect(calls.count == 1)
        #expect(calls[0].accountID == Self.accountID)
        #expect(calls[0].request == GoogleOutboundPreviewRequest(
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 7,
            operation: .upsert
        ))
        #expect(store.approvalConfirmation != nil)
        #expect(recovery.value?.isValid(now: Self.now) == true)
    }

    @Test("a fast-clock expired preview is durable but never actionable")
    func fastClockExpiredPreviewIsPersistedWithoutApproval() async throws {
        let clock = OutboundTestClock(Self.now)
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [
                .value(try Self.preview(
                    expiresAt: Self.now.addingTimeInterval(-4 * 60)
                )),
            ],
            approvalSteps: [.value(try Self.approval())],
            enqueueSteps: [.value(try Self.accepted())]
        )
        let store = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )

        #expect(!(await Self.prepare(store)))
        #expect(recovery.value?.stage == .previewed)
        #expect(store.approvalConfirmation == nil)
        #expect(store.status.isWaitingForSafeDiscard)
        #expect((await transport.approvalCallsSnapshot()).isEmpty)
        #expect((await transport.enqueueCallsSnapshot()).isEmpty)
        #expect(!store.discardExpiredRecovery())

        clock.advance(by: 61)
        #expect(store.discardExpiredRecovery())
        #expect(recovery.value == nil)
    }

    @Test("only explicit bound approval persists capability before enqueue and clear")
    func explicitApprovalPersistsCapabilityBeforeEnqueue() async throws {
        let events = OutboundEventLog()
        let recovery = TestGoogleOutboundRecoveryStore(events: events)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            events: events,
            previewSteps: [.value(try Self.preview())],
            approvalSteps: [.value(try Self.approval())],
            enqueueSteps: [.value(try Self.accepted())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(await Self.prepare(store))
        #expect(await transport.approvalCallsSnapshot().isEmpty)
        #expect(await transport.enqueueCallsSnapshot().isEmpty)
        let confirmation = try #require(store.approvalConfirmation)

        #expect(await store.approveAndEnqueue(confirmation))

        #expect(recovery.value == nil)
        #expect(recovery.cleared.count == 1)
        #expect(recovery.cleared.first?.stage == .approved)
        #expect(store.hasPendingRecovery == false)
        #expect(store.preview == nil)
        #expect(store.accepted?.outboxID == Self.outboxID)
        #expect(
            events.snapshot() == [
                "save:intent", "preview", "save:previewed",
                "save:approval_attempted", "approve",
                "save:approved", "enqueue", "clear",
            ]
        )
        let approved = try #require(recovery.saved.last)
        #expect(approved.stage == .approved)
        #expect(approved.approvalCapability == Self.capability)
        #expect(!store.status.message.contains(Self.capability))
        #expect(!String(describing: approved).contains(Self.capability))
        #expect(!String(reflecting: approved).contains(Self.capability))
    }

    @Test("enqueue timeout retains capability and relaunch replays the exact request after expiry")
    func enqueueTimeoutReplaysExactly() async throws {
        let clock = OutboundTestClock(Self.now)
        let events = OutboundEventLog()
        let recovery = TestGoogleOutboundRecoveryStore(events: events)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            events: events,
            previewSteps: [.value(try Self.preview())],
            approvalSteps: [.value(try Self.approval())],
            enqueueSteps: [
                .failure(.api(.transport(.timedOut))),
                .value(try Self.accepted(replayed: true)),
            ]
        )
        var store: GoogleOutboundStore? = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )
        #expect(await Self.prepare(try #require(store)))
        let confirmation = try #require(store?.approvalConfirmation)

        #expect(!(await store?.approveAndEnqueue(confirmation) ?? true))
        #expect(store?.status.isWorking == false)
        let enqueueTimeoutStatus = try #require(store?.status)
        if case .recoveryRequired = enqueueTimeoutStatus {
            // Expected: the durable capability remains available for replay.
        } else {
            Issue.record("enqueue timeout must leave a recovery-required status")
        }
        let pending = try #require(recovery.value)
        #expect(pending.stage == .approved)
        #expect(store?.hasApprovedRecovery == true)
        #expect(store?.recoveryContext?.stage == .approved)
        #expect(recovery.cleared.isEmpty)
        store = nil

        clock.advance(by: 16 * 60)
        let relaunched = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )
        #expect(relaunched.status == .expired)
        #expect(relaunched.hasApprovedRecovery)
        #expect(await relaunched.recoverPendingOperation())
        #expect(recovery.value == nil)
        let requests = await transport.enqueueCallsSnapshot()
        #expect(requests.count == 2)
        #expect(requests[0] == requests[1])
        #expect(requests[0].request.approvalCapability == Self.capability)
        #expect(recovery.cleared.count == 1)
    }

    @Test("expired unconsumed approval rejection keeps exact recovery for retry or discard")
    func expiredApprovedRejectionRetainsRecovery() async throws {
        let clock = OutboundTestClock(Self.now)
        clock.advance(by: 16 * 60)
        let approved = try Self.approvedJournal()
        let recovery = TestGoogleOutboundRecoveryStore(value: approved)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            enqueueSteps: [
                .failure(.api(.server(
                    statusCode: 409,
                    code: "google_outbound_approval_expired",
                    message: nil,
                    requestID: nil
                ))),
            ]
        )
        let store = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )

        #expect(store.status == .expired)
        #expect(store.hasApprovedRecovery)
        #expect(!(await store.recoverPendingOperation()))
        #expect(recovery.value == approved)
        #expect(recovery.cleared.isEmpty)
        #expect(store.status == .expired)
        #expect(store.hasApprovedRecovery)
        let calls = await transport.enqueueCallsSnapshot()
        #expect(calls.count == 1)
        #expect(calls[0].request == approved.enqueueRequest)
        #expect((await transport.previewCallsSnapshot()).isEmpty)
        #expect((await transport.approvalCallsSnapshot()).isEmpty)
    }

    @Test("expired acceptance check fences discard while its exact replay is in flight")
    func expiredApprovedReplayFencesDiscard() async throws {
        let clock = OutboundTestClock(Self.now)
        clock.advance(by: 16 * 60)
        let gate = OutboundAsyncGate()
        let approved = try Self.approvedJournal()
        let recovery = TestGoogleOutboundRecoveryStore(value: approved)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            enqueueSteps: [.gated(gate, try Self.accepted(replayed: true))]
        )
        let store = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )
        let replay = Task { @MainActor in
            await store.recoverPendingOperation()
        }
        await gate.waitUntilEntered()

        #expect(!store.discardExpiredRecovery())
        #expect(recovery.value == approved)
        #expect(recovery.cleared.isEmpty)

        await gate.release()
        #expect(await replay.value)
        #expect(recovery.value == nil)
        #expect(recovery.cleared == [approved])
        #expect(store.accepted?.replayed == true)
        #expect((await transport.enqueueCallsSnapshot()).count == 1)
    }

    @Test("expired preview and uncertain approval stages never send recovery requests")
    func expiredEarlierStagesRemainInert() async throws {
        let clock = OutboundTestClock(Self.now)
        clock.advance(by: 16 * 60)
        let previewed = try Self.previewedJournal()
        let attempted = try previewed.recordingApprovalAttempt()

        for journal in [previewed, attempted] {
            let recovery = TestGoogleOutboundRecoveryStore(value: journal)
            let transport = TestGoogleOutboundTransport(
                configurationIdentifier: Self.configuration
            )
            let store = Self.makeStore(
                recovery: recovery,
                transport: transport,
                now: { clock.read() }
            )

            #expect(store.status == .expired)
            #expect(!store.hasApprovedRecovery)
            #expect(!(await store.recoverPendingOperation()))
            #expect(recovery.value == journal)
            #expect((await transport.previewCallsSnapshot()).isEmpty)
            #expect((await transport.approvalCallsSnapshot()).isEmpty)
            #expect((await transport.enqueueCallsSnapshot()).isEmpty)
        }
    }

    @Test("preview timeout retains exact intent and recovery replays it")
    func previewTimeoutReplaysExactly() async throws {
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [
                .failure(.api(.transport(.timedOut))),
                .value(try Self.preview()),
            ]
        )
        var store: GoogleOutboundStore? = Self.makeStore(
            recovery: recovery,
            transport: transport
        )

        #expect(!(await Self.prepare(try #require(store))))
        #expect(store?.status.isWorking == false)
        let previewTimeoutStatus = try #require(store?.status)
        if case .recoveryRequired = previewTimeoutStatus {
            // Expected: the exact intent remains available for replay.
        } else {
            Issue.record("preview timeout must leave a recovery-required status")
        }
        #expect(recovery.value?.stage == .intent)
        store = nil

        let relaunched = Self.makeStore(recovery: recovery, transport: transport)
        #expect(await relaunched.recoverPendingOperation())
        let requests = await transport.previewCallsSnapshot()
        #expect(requests.count == 2)
        #expect(requests[0] == requests[1])
        #expect(recovery.value?.stage == .previewed)
        #expect(relaunched.approvalConfirmation != nil)
    }

    @Test("definitive preview rejection clears only its inert intent")
    func rejectedPreviewDoesNotFencePublication() async throws {
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [
                .failure(.api(.server(
                    statusCode: 409,
                    code: "conflict",
                    message: "only DayWeave-owned provider records can be changed by this endpoint",
                    requestID: nil
                ))),
                .value(try Self.taskPreview()),
            ]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(!(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 7,
            entityKind: .task,
            operation: .delete
        )))
        #expect(recovery.value == nil)
        #expect(recovery.saved.map(\.stage) == [.intent])
        #expect(recovery.cleared.map(\.stage) == [.intent])
        #expect(!store.hasPendingRecovery)
        #expect(store.recoveryContext == nil)
        #expect(store.status.isWorking == false)
        #expect((await transport.approvalCallsSnapshot()).isEmpty)
        #expect((await transport.enqueueCallsSnapshot()).isEmpty)

        #expect(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 7,
            entityKind: .task,
            operation: .upsert
        ))
        #expect(recovery.value?.stage == .previewed)
    }

    @Test("approval timeout records one attempt and recovery never approves implicitly")
    func approvalTimeoutNeverAutoApproves() async throws {
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(try Self.preview())],
            approvalSteps: [.failure(.api(.transport(.timedOut)))]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)
        #expect(await Self.prepare(store))
        let confirmation = try #require(store.approvalConfirmation)

        #expect(!(await store.approveAndEnqueue(confirmation)))
        #expect(store.status.isWorking == false)
        if case .recoveryRequired = store.status {
            // Expected: automatic recovery must not repeat approval.
        } else {
            Issue.record("approval timeout must leave a recovery-required status")
        }
        #expect(recovery.value?.stage == .approvalAttempted)
        #expect(await store.recoverPendingOperation())
        #expect((await transport.approvalCallsSnapshot()).count == 1)
        #expect((await transport.enqueueCallsSnapshot()).isEmpty)
        #expect(store.approvalConfirmation == nil)
        #expect(store.status.message.contains("response may have been lost"))
    }

    @Test("expired recovery requires explicit exact discard before a fresh generation")
    func expiryRequiresDiscardBeforeFreshPreview() async throws {
        let clock = OutboundTestClock(Self.now)
        let recovery = TestGoogleOutboundRecoveryStore()
        let firstPreview = try Self.preview(
            id: Self.previewID,
            expiresAt: Self.now.addingTimeInterval(60)
        )
        let secondPreview = try Self.preview(
            id: Self.secondPreviewID,
            expiresAt: Self.now.addingTimeInterval(16 * 60 + 1)
        )
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(firstPreview), .value(secondPreview)]
        )
        let store = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )
        #expect(await Self.prepare(store))
        let first = try #require(recovery.value)

        clock.advance(by: 6 * 60 + 1)
        #expect(!(await Self.prepare(store)))
        #expect(recovery.value == first)
        #expect(recovery.cleared.isEmpty)
        #expect(store.discardExpiredRecovery())
        #expect(await Self.prepare(store))

        let second = try #require(recovery.value)
        #expect(second.recoveryID != first.recoveryID)
        #expect(second.operationGeneration == first.operationGeneration + 1)
        #expect(second.preview?.id == Self.secondPreviewID)
        #expect(recovery.cleared == [first])
        #expect(recovery.saved.map(\.stage) == [.intent, .previewed, .intent, .previewed])
    }

    @Test("stale confirmation cannot approve a fresh preview")
    func staleConfirmationIsRejected() async throws {
        let clock = OutboundTestClock(Self.now)
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [
                .value(try Self.preview(expiresAt: Self.now.addingTimeInterval(60))),
                .value(try Self.preview(
                    id: Self.secondPreviewID,
                    expiresAt: Self.now.addingTimeInterval(16 * 60 + 1),
                    hashCharacter: "b"
                )),
            ]
        )
        let store = Self.makeStore(
            recovery: recovery,
            transport: transport,
            now: { clock.read() }
        )
        #expect(await Self.prepare(store))
        let stale = try #require(store.approvalConfirmation)
        clock.advance(by: 6 * 60 + 1)
        #expect(store.discardExpiredRecovery())
        #expect(await Self.prepare(store))

        #expect(!(await store.approveAndEnqueue(stale)))
        #expect((await transport.approvalCallsSnapshot()).isEmpty)
        #expect(recovery.value?.stage == .previewed)
        #expect(store.approvalConfirmation != stale)
    }

    @Test("mismatched preview identity and revisions never become approvable")
    func mismatchedPreviewIsRetainedAsIntent() async throws {
        let recovery = TestGoogleOutboundRecoveryStore()
        let mismatched = try Self.preview(
            accountID: UUID(uuidString: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")!,
            itemRevision: 8
        )
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(mismatched)]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(!(await Self.prepare(store)))
        #expect(recovery.value?.stage == .intent)
        #expect(store.preview == nil)
        #expect(store.approvalConfirmation == nil)
        #expect((await transport.approvalCallsSnapshot()).isEmpty)
    }

    @Test("preview entity must match the exact persisted intent")
    func previewEntityMismatchIsRejected() async throws {
        let cases: [(GoogleOutboundEntityKind, GoogleOutboundPreview)] = [
            (.calendarEvent, try Self.taskPreview()),
            (.task, try Self.preview()),
        ]
        for (expectedEntityKind, response) in cases {
            let recovery = TestGoogleOutboundRecoveryStore()
            let transport = TestGoogleOutboundTransport(
                configurationIdentifier: Self.configuration,
                previewSteps: [.value(response)]
            )
            let store = Self.makeStore(recovery: recovery, transport: transport)

            #expect(!(await store.preparePreview(
                accountID: Self.accountID,
                collectionID: Self.collectionID,
                itemID: Self.itemID,
                expectedItemRevision: 7,
                entityKind: expectedEntityKind,
                operation: .upsert
            )))
            #expect(recovery.value?.stage == .intent)
            #expect(recovery.value?.entityKind == expectedEntityKind)
            #expect(recovery.saved.map(\.stage) == [.intent])
            #expect(store.preview == nil)
            #expect(store.approvalConfirmation == nil)
            #expect((await transport.approvalCallsSnapshot()).isEmpty)
            #expect((await transport.enqueueCallsSnapshot()).isEmpty)
        }
    }

    @Test("Task publication preserves its entity binding through approval and acceptance")
    func taskPublicationIsEntityBoundEndToEnd() async throws {
        let events = OutboundEventLog()
        let recovery = TestGoogleOutboundRecoveryStore(events: events)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            events: events,
            previewSteps: [.value(try Self.taskPreview())],
            approvalSteps: [.value(try Self.approval())],
            enqueueSteps: [.value(try Self.accepted())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: Self.itemID,
            expectedItemRevision: 7,
            entityKind: .task,
            operation: .upsert
        ))
        #expect(recovery.saved.map(\.entityKind) == [.task, .task])
        #expect(store.preview?.entityKind == .task)
        #expect(store.recoveryContext?.entityKind == .task)
        let confirmation = try #require(store.approvalConfirmation)

        #expect(await store.approveAndEnqueue(confirmation))
        #expect(recovery.saved.map(\.entityKind) == [.task, .task, .task, .task])
        #expect(recovery.cleared.first?.entityKind == .task)
        #expect(recovery.value == nil)
        #expect(store.accepted?.outboxID == Self.outboxID)
        #expect(!store.status.message.localizedCaseInsensitiveContains("Calendar"))
        #expect(events.snapshot() == [
            "save:intent", "preview", "save:previewed",
            "save:approval_attempted", "approve",
            "save:approved", "enqueue", "clear",
        ])
    }

    @Test("timeout cancellation and authentication or generic conflicts never clear")
    func ambiguousAndRejectedEnqueueErrorsRetainRecovery() async throws {
        let failures: [TestOutboundFailure] = [
            .api(.transport(.cancelled)),
            .api(.server(statusCode: 401, code: "unauthorized", message: nil, requestID: nil)),
            .api(.server(statusCode: 409, code: "conflict", message: "generic", requestID: nil)),
        ]
        for failure in failures {
            let approved = try Self.approvedJournal()
            let recovery = TestGoogleOutboundRecoveryStore(value: approved)
            let transport = TestGoogleOutboundTransport(
                configurationIdentifier: Self.configuration,
                enqueueSteps: [.failure(failure)]
            )
            let store = Self.makeStore(recovery: recovery, transport: transport)

            #expect(!(await store.recoverPendingOperation()))
            #expect(recovery.value == approved)
            #expect(recovery.cleared.isEmpty)
            #expect(store.hasPendingRecovery)
        }
    }

    @Test("privacy lock fences a late preview result without dropping recovery")
    func privacyFenceRejectsLateResult() async throws {
        let gate = OutboundAsyncGate()
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.gated(gate, try Self.preview())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)
        let task = Task { @MainActor in await Self.prepare(store) }
        await gate.waitUntilEntered()

        store.setPrivacyAvailable(false)
        await gate.release()
        #expect(!(await task.value))

        #expect(store.status == .privacyProtected)
        #expect(store.preview == nil)
        #expect(store.approvalConfirmation == nil)
        #expect(recovery.value?.stage == .intent)
        #expect(recovery.cleared.isEmpty)
    }

    @Test("locked workflow does not load or expose encrypted recovery")
    func lockedWorkflowDoesNotLoadRecovery() async throws {
        let approved = try Self.approvedJournal()
        let recovery = TestGoogleOutboundRecoveryStore(value: approved)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration
        )
        let store = GoogleOutboundStore(
            recoveryStore: recovery,
            transportProvider: { transport },
            privacyAvailable: false,
            now: { Self.now }
        )

        #expect(recovery.loadCount == 0)
        #expect(!(await store.recoverPendingOperation()))
        #expect(recovery.loadCount == 0)
        #expect(store.status == .privacyProtected)
        #expect(store.preview == nil)
        #expect(store.approvalConfirmation == nil)
        #expect((await transport.previewCallsSnapshot()).isEmpty)
        #expect((await transport.approvalCallsSnapshot()).isEmpty)
        #expect((await transport.enqueueCallsSnapshot()).isEmpty)
    }

    @Test("configuration binding fences late results from an old authenticated client")
    func configurationFenceRejectsLateResult() async throws {
        let gate = OutboundAsyncGate()
        let recovery = TestGoogleOutboundRecoveryStore()
        let oldTransport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.gated(gate, try Self.preview())]
        )
        let newTransport = TestGoogleOutboundTransport(
            configurationIdentifier: "https://other.example|auth=changed"
        )
        let box = OutboundTransportBox(oldTransport)
        let store = GoogleOutboundStore(
            recoveryStore: recovery,
            transportProvider: { box.transport },
            privacyAvailable: true,
            now: { Self.now }
        )
        let task = Task { @MainActor in await Self.prepare(store) }
        await gate.waitUntilEntered()

        box.transport = newTransport
        await gate.release()
        #expect(!(await task.value))

        #expect(recovery.value?.stage == .intent)
        #expect(store.preview == nil)
        #expect(store.hasPendingRecovery)
        #expect((await oldTransport.previewCallsSnapshot()).count == 1)
        #expect((await newTransport.previewCallsSnapshot()).isEmpty)
    }

    @Test("operation lock prevents a second publication lane while preview is in flight")
    func operationLockIsExclusive() async throws {
        let gate = OutboundAsyncGate()
        let recovery = TestGoogleOutboundRecoveryStore()
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.gated(gate, try Self.preview())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)
        let first = Task { @MainActor in await Self.prepare(store) }
        await gate.waitUntilEntered()

        #expect(!(await store.preparePreview(
            accountID: Self.accountID,
            collectionID: Self.collectionID,
            itemID: UUID(uuidString: "99999999-9999-4999-8999-999999999999")!,
            expectedItemRevision: 1,
            operation: .upsert
        )))
        #expect((await transport.previewCallsSnapshot()).count == 1)
        await gate.release()
        #expect(await first.value)
        #expect((await transport.previewCallsSnapshot()).count == 1)
    }

    @Test("capability is redacted from status errors and all journal reflection")
    func capabilityNeverLeaksThroughDiagnostics() async throws {
        let approved = try Self.approvedJournal()
        let recovery = TestGoogleOutboundRecoveryStore(value: approved)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            enqueueSteps: [.failure(.message("gateway repeated \(Self.capability)"))]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(!(await store.recoverPendingOperation()))
        #expect(!store.status.message.contains(Self.capability))
        #expect(!String(describing: store.status).contains(Self.capability))
        #expect(!String(reflecting: store.status).contains(Self.capability))
        #expect(!String(describing: approved).contains(Self.capability))
        #expect(!String(reflecting: approved).contains(Self.capability))
        #expect(recovery.value == approved)
    }

    @Test("recovery clear is attempted only after an accepted response")
    func clearFailureRetainsAcceptedRecovery() async throws {
        let approved = try Self.approvedJournal()
        let events = OutboundEventLog()
        let recovery = TestGoogleOutboundRecoveryStore(
            value: approved,
            events: events,
            clearFailure: true
        )
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            events: events,
            enqueueSteps: [.value(try Self.accepted())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(!(await store.recoverPendingOperation()))
        #expect(store.status.isWorking == false)
        if case .recoveryRequired = store.status {
            // Expected: acceptance without a durable clear remains retryable.
        } else {
            Issue.record("clear failure must leave a recovery-required status")
        }
        #expect(events.snapshot() == ["enqueue", "clear"])
        #expect(recovery.value == approved)
        #expect(store.hasPendingRecovery)
        #expect(store.accepted == nil)
    }

    @Test("intent persistence failure exits working state before network I/O")
    func intentPersistenceFailureIsVisible() async throws {
        let recovery = TestGoogleOutboundRecoveryStore(saveFailure: true)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(try Self.preview())]
        )
        let store = Self.makeStore(recovery: recovery, transport: transport)

        #expect(!(await Self.prepare(store)))
        #expect(store.status.isWorking == false)
        if case .failed = store.status {
            // Expected: no durable recovery exists and no request was sent.
        } else {
            Issue.record("intent persistence failure must become a visible failure")
        }
        #expect(recovery.value == nil)
        #expect((await transport.previewCallsSnapshot()).isEmpty)
    }

    @Test("every authority transition is durable before exposure or network use")
    func transitionPersistenceFailuresStayFailClosed() async throws {
        let previewPersistence = TestGoogleOutboundRecoveryStore(failOnSaveNumber: 2)
        let previewTransport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(try Self.preview())]
        )
        let previewStore = Self.makeStore(
            recovery: previewPersistence,
            transport: previewTransport
        )
        #expect(!(await Self.prepare(previewStore)))
        #expect(previewPersistence.value?.stage == .intent)
        #expect(previewStore.preview == nil)
        #expect(previewStore.approvalConfirmation == nil)
        #expect((await previewTransport.previewCallsSnapshot()).count == 1)

        let attemptPersistence = TestGoogleOutboundRecoveryStore(failOnSaveNumber: 3)
        let attemptTransport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(try Self.preview())],
            approvalSteps: [.value(try Self.approval())]
        )
        let attemptStore = Self.makeStore(
            recovery: attemptPersistence,
            transport: attemptTransport
        )
        #expect(await Self.prepare(attemptStore))
        let confirmation = try #require(attemptStore.approvalConfirmation)
        #expect(!(await attemptStore.approveAndEnqueue(confirmation)))
        #expect(attemptPersistence.value?.stage == .previewed)
        #expect((await attemptTransport.approvalCallsSnapshot()).isEmpty)
        #expect((await attemptTransport.enqueueCallsSnapshot()).isEmpty)

        let capabilityPersistence = TestGoogleOutboundRecoveryStore(failOnSaveNumber: 4)
        let capabilityTransport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration,
            previewSteps: [.value(try Self.preview())],
            approvalSteps: [.value(try Self.approval())],
            enqueueSteps: [.value(try Self.accepted())]
        )
        let capabilityStore = Self.makeStore(
            recovery: capabilityPersistence,
            transport: capabilityTransport
        )
        #expect(await Self.prepare(capabilityStore))
        let capabilityConfirmation = try #require(capabilityStore.approvalConfirmation)
        #expect(!(await capabilityStore.approveAndEnqueue(capabilityConfirmation)))
        #expect(capabilityPersistence.value?.stage == .approvalAttempted)
        #expect(capabilityStore.approvalConfirmation == nil)
        #expect((await capabilityTransport.approvalCallsSnapshot()).count == 1)
        #expect((await capabilityTransport.enqueueCallsSnapshot()).isEmpty)
    }

    @Test("transport preflight failure creates no outbound recovery authority")
    func transportPreflightFailsBeforeJournal() async {
        let recovery = TestGoogleOutboundRecoveryStore()
        let store = GoogleOutboundStore(
            recoveryStore: recovery,
            transportProvider: { throw DurableAuthError.enrollmentRequired },
            privacyAvailable: true,
            now: { Self.now }
        )

        #expect(!(await Self.prepare(store)))
        #expect(store.status.isWorking == false)
        if case .failed = store.status {
            // Expected: transport construction precedes persist-before-send.
        } else {
            Issue.record("transport preflight failure must remain a local failure")
        }
        #expect(recovery.value == nil)
        #expect(recovery.saved.isEmpty)
        #expect(store.hasPendingRecovery == false)
    }

    @Test("only expired authority can be explicitly discarded across an old auth binding")
    func expiredRecoveryCanBeDiscardedSafely() throws {
        let clock = OutboundTestClock(Self.now)
        let approved = try Self.approvedJournal()
        let recovery = TestGoogleOutboundRecoveryStore(value: approved)
        let changedTransport = TestGoogleOutboundTransport(
            configurationIdentifier: "https://other.example|auth=changed"
        )
        let store = Self.makeStore(
            recovery: recovery,
            transport: changedTransport,
            now: { clock.read() }
        )

        #expect(store.hasPendingRecovery)
        #expect(store.status != .expired)
        #expect(!store.discardExpiredRecovery())
        #expect(recovery.value == approved)
        #expect(recovery.cleared.isEmpty)

        clock.advance(by: 16 * 60)
        store.configurationDidChange()
        #expect(store.status == .expired)
        #expect(store.discardExpiredRecovery())
        #expect(recovery.value == nil)
        #expect(recovery.cleared == [approved])
        #expect(store.hasPendingRecovery == false)
        #expect(store.status == .idle)
    }

    @Test("a displayed preview passively becomes discardable at its bound expiry")
    func passivePreviewExpiryUpdatesPresentation() async throws {
        let clock = OutboundTestClock(Self.now)
        let sleeper = OutboundManualExpirySleeper()
        let previewed = try Self.previewedJournal()
        let recovery = TestGoogleOutboundRecoveryStore(value: previewed)
        let transport = TestGoogleOutboundTransport(
            configurationIdentifier: Self.configuration
        )
        let store = GoogleOutboundStore(
            recoveryStore: recovery,
            transportProvider: { transport },
            privacyAvailable: true,
            now: { clock.read() },
            expirySleeper: { interval in
                try await sleeper.sleep(for: interval)
            }
        )

        #expect(store.preview?.id == Self.previewID)
        #expect(store.approvalConfirmation != nil)
        await sleeper.waitUntilEntered()
        clock.advance(by: 16 * 60)
        await sleeper.release()
        for _ in 0..<100 where store.status != .expired {
            await Task.yield()
        }

        #expect(store.status == .expired)
        #expect(store.preview == nil)
        #expect(store.approvalConfirmation == nil)
        #expect(store.hasPendingRecovery)
        #expect(store.discardExpiredRecovery())
    }

    @Test("journal decoding is exact, shape validation is time independent, and expiry survives restart")
    func strictRecoveryJournalValidation() throws {
        let journal = try Self.approvedJournal()
        #expect(journal.hasValidShape)
        #expect(journal.isValid(now: Self.now))
        #expect(journal.isValid(now: Self.now.addingTimeInterval(24 * 60 * 60)))
        #expect(journal.canStartFresh(at: Self.now.addingTimeInterval(11 * 60)))

        let encoder = JSONEncoder()
        let data = try encoder.encode(journal)
        #expect(try JSONDecoder().decode(GoogleOutboundRecoveryJournal.self, from: data) == journal)
        let encodedObject = try #require(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        #expect(encodedObject["version"] as? Int == GoogleOutboundRecoveryJournal.currentVersion)
        #expect(encodedObject["entity_kind"] as? String == "calendar_event")

        var legacyObject = encodedObject
        legacyObject["version"] = 1
        legacyObject.removeValue(forKey: "entity_kind")
        let migrated = try JSONDecoder().decode(
            GoogleOutboundRecoveryJournal.self,
            from: JSONSerialization.data(withJSONObject: legacyObject)
        )
        #expect(migrated == journal)
        #expect(migrated.version == GoogleOutboundRecoveryJournal.currentVersion)
        #expect(migrated.entityKind == .calendarEvent)

        var forgedLegacyTask = legacyObject
        forgedLegacyTask["preview"] = try #require(
            JSONSerialization.jsonObject(with: encoder.encode(try Self.taskPreview()))
                as? [String: Any]
        )
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(
                GoogleOutboundRecoveryJournal.self,
                from: JSONSerialization.data(withJSONObject: forgedLegacyTask)
            )
        }

        var object = encodedObject
        object["unexpected"] = true
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(
                GoogleOutboundRecoveryJournal.self,
                from: JSONSerialization.data(withJSONObject: object)
            )
        }

        object.removeValue(forKey: "unexpected")
        object.removeValue(forKey: "entity_kind")
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(
                GoogleOutboundRecoveryJournal.self,
                from: JSONSerialization.data(withJSONObject: object)
            )
        }

        object["entity_kind"] = "calendar_event"
        object["approval_expires_at"] = NSNull()
        #expect(throws: DecodingError.self) {
            try JSONDecoder().decode(
                GoogleOutboundRecoveryJournal.self,
                from: JSONSerialization.data(withJSONObject: object)
            )
        }
    }

    private static func makeStore(
        recovery: TestGoogleOutboundRecoveryStore,
        transport: any GoogleOutboundTransport,
        now: @escaping @Sendable () -> Date = { Self.now }
    ) -> GoogleOutboundStore {
        GoogleOutboundStore(
            recoveryStore: recovery,
            transportProvider: { transport },
            privacyAvailable: true,
            now: now
        )
    }

    private static func prepare(_ store: GoogleOutboundStore) async -> Bool {
        await store.preparePreview(
            accountID: accountID,
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: 7,
            operation: .upsert
        )
    }

    private static func preview(
        id: UUID = previewID,
        accountID: UUID = accountID,
        itemRevision: UInt64 = 7,
        expiresAt: Date = now.addingTimeInterval(10 * 60),
        hashCharacter: Character = "a",
        entityKind: String = "calendar_event",
        providerPayload: [String: Any]? = nil
    ) throws -> GoogleOutboundPreview {
        let object: [String: Any] = [
            "id": id.uuidString.lowercased(),
            "account_id": accountID.uuidString.lowercased(),
            "collection_id": collectionID.uuidString.lowercased(),
            "collection_revision": 3,
            "collection_display_name": "Personal",
            "item_id": itemID.uuidString.lowercased(),
            "item_revision": itemRevision,
            "entity_kind": entityKind,
            "operation": "upsert",
            "provider_resource_id": NSNull(),
            "provider_etag": NSNull(),
            "preview_hash": String(repeating: String(hashCharacter), count: 64),
            "provider_payload": providerPayload ?? [
                "summary": "Private title",
                "start": ["dateTime": "2026-08-31T08:00:00Z"],
            ],
            "expires_at": expiresAt.timeIntervalSinceReferenceDate,
        ]
        return try JSONDecoder().decode(
            GoogleOutboundPreview.self,
            from: JSONSerialization.data(withJSONObject: object)
        )
    }

    private static func taskPreview() throws -> GoogleOutboundPreview {
        try preview(entityKind: "task", providerPayload: [
            "id": "",
            "etag": NSNull(),
            "title": "Private task",
            "notes": "First line\nSecond line",
            "status": "completed",
            "due": "2026-09-01T18:00:00+00:00",
            "completed": "2026-08-31T09:15:00Z",
            "updated": NSNull(),
            "parent": NSNull(),
            "position": NSNull(),
            "links": NSNull(),
            "deleted": false,
            "hidden": false,
        ])
    }

    private static func approval(
        expiresAt: Date = now.addingTimeInterval(10 * 60)
    ) throws -> GoogleOutboundApproval {
        let object: [String: Any] = [
            "preview_id": previewID.uuidString.lowercased(),
            "approval_capability": capability,
            "expires_at": expiresAt.timeIntervalSinceReferenceDate,
        ]
        return try JSONDecoder().decode(
            GoogleOutboundApproval.self,
            from: JSONSerialization.data(withJSONObject: object)
        )
    }

    private static func accepted(replayed: Bool = false) throws -> GoogleOutboundAccepted {
        let object: [String: Any] = [
            "outbox_id": outboxID.uuidString.lowercased(),
            "replayed": replayed,
        ]
        return try JSONDecoder().decode(
            GoogleOutboundAccepted.self,
            from: JSONSerialization.data(withJSONObject: object)
        )
    }

    private static func approvedJournal() throws -> GoogleOutboundRecoveryJournal {
        try previewedJournal().recordingApprovalAttempt().recording(approval: approval())
    }

    private static func previewedJournal() throws -> GoogleOutboundRecoveryJournal {
        let intent = try GoogleOutboundRecoveryJournal(
            recoveryID: recoveryID,
            operationGeneration: 4,
            configurationIdentifier: configuration,
            accountID: accountID,
            collectionID: collectionID,
            itemID: itemID,
            expectedItemRevision: 7,
            operation: .upsert,
            intentExpiresAt: now.addingTimeInterval(35 * 60),
            createdAt: now
        )
        return try intent.recording(preview: preview())
    }

    nonisolated static let now = Date(timeIntervalSince1970: 1_788_076_800)
    static let configuration = "https://api.example.com|auth=test-binding"
    static let accountID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    static let collectionID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    static let itemID = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
    static let previewID = UUID(uuidString: "44444444-4444-4444-8444-444444444444")!
    static let secondPreviewID = UUID(uuidString: "55555555-5555-4555-8555-555555555555")!
    static let outboxID = UUID(uuidString: "66666666-6666-4666-8666-666666666666")!
    static let recoveryID = UUID(uuidString: "77777777-7777-4777-8777-777777777777")!
    static let capability = "dw_ga1_" + String(repeating: "A", count: 43)
}

@MainActor
private final class TestGoogleOutboundRecoveryStore: GoogleOutboundRecoveryStoring {
    var value: GoogleOutboundRecoveryJournal?
    private(set) var saved: [GoogleOutboundRecoveryJournal] = []
    private(set) var cleared: [GoogleOutboundRecoveryJournal] = []
    private(set) var loadCount = 0
    private let events: OutboundEventLog?
    private let saveFailure: Bool
    private let failOnSaveNumber: Int?
    private let clearFailure: Bool
    private var saveAttemptCount = 0

    init(
        value: GoogleOutboundRecoveryJournal? = nil,
        events: OutboundEventLog? = nil,
        saveFailure: Bool = false,
        failOnSaveNumber: Int? = nil,
        clearFailure: Bool = false
    ) {
        self.value = value
        self.events = events
        self.saveFailure = saveFailure
        self.failOnSaveNumber = failOnSaveNumber
        self.clearFailure = clearFailure
    }

    func loadGoogleOutboundRecoveryJournal() throws -> GoogleOutboundRecoveryJournal? {
        loadCount += 1
        return value
    }

    func saveGoogleOutboundRecoveryJournal(_ journal: GoogleOutboundRecoveryJournal) throws {
        events?.append("save:\(journal.stage.rawValue)")
        saveAttemptCount += 1
        if saveFailure || saveAttemptCount == failOnSaveNumber {
            throw TestRecoveryStoreFailure.writeFailed
        }
        saved.append(journal)
        value = journal
    }

    func clearGoogleOutboundRecoveryJournal(
        _ expected: GoogleOutboundRecoveryJournal
    ) throws {
        events?.append("clear")
        guard value == expected else { throw TestRecoveryStoreFailure.changed }
        if clearFailure { throw TestRecoveryStoreFailure.writeFailed }
        cleared.append(expected)
        value = nil
    }
}

private enum TestRecoveryStoreFailure: Error {
    case changed
    case writeFailed
}

private struct TestOutboundCall: Equatable, Sendable {
    let accountID: UUID
    let request: GoogleOutboundPreviewRequest
}

private struct TestOutboundApprovalCall: Equatable, Sendable {
    let accountID: UUID
    let previewID: UUID
    let expectedPreviewHash: String
}

private struct TestOutboundEnqueueCall: Equatable, Sendable {
    let accountID: UUID
    let request: GoogleOutboundEnqueueRequest
}

private enum TestOutboundFailure: Error, Equatable, Sendable, LocalizedError {
    case api(DayWeaveAPIError)
    case message(String)

    var errorDescription: String? {
        switch self {
        case let .api(error): error.localizedDescription
        case let .message(message): message
        }
    }
}

private enum TestOutboundStep<Value: Sendable>: Sendable {
    case value(Value)
    case failure(TestOutboundFailure)
    case gated(OutboundAsyncGate, Value)

    func resolve() async throws -> Value {
        switch self {
        case let .value(value): return value
        case let .failure(.api(error)): throw error
        case let .failure(error): throw error
        case let .gated(gate, value):
            await gate.enterAndWait()
            return value
        }
    }
}

private actor TestGoogleOutboundTransport: GoogleOutboundTransport {
    nonisolated let configurationIdentifier: String
    private let events: OutboundEventLog?
    private var previewSteps: [TestOutboundStep<GoogleOutboundPreview>]
    private var approvalSteps: [TestOutboundStep<GoogleOutboundApproval>]
    private var enqueueSteps: [TestOutboundStep<GoogleOutboundAccepted>]
    private var previewCalls: [TestOutboundCall] = []
    private var approvalCalls: [TestOutboundApprovalCall] = []
    private var enqueueCalls: [TestOutboundEnqueueCall] = []

    init(
        configurationIdentifier: String,
        events: OutboundEventLog? = nil,
        previewSteps: [TestOutboundStep<GoogleOutboundPreview>] = [],
        approvalSteps: [TestOutboundStep<GoogleOutboundApproval>] = [],
        enqueueSteps: [TestOutboundStep<GoogleOutboundAccepted>] = []
    ) {
        self.configurationIdentifier = configurationIdentifier
        self.events = events
        self.previewSteps = previewSteps
        self.approvalSteps = approvalSteps
        self.enqueueSteps = enqueueSteps
    }

    func previewGoogleOutbound(
        accountID: UUID,
        request: GoogleOutboundPreviewRequest
    ) async throws -> GoogleOutboundPreview {
        events?.append("preview")
        previewCalls.append(.init(accountID: accountID, request: request))
        guard !previewSteps.isEmpty else { throw TestOutboundFailure.message("unexpected preview") }
        return try await previewSteps.removeFirst().resolve()
    }

    func approveGoogleOutbound(
        accountID: UUID,
        previewID: UUID,
        expectedPreviewHash: String
    ) async throws -> GoogleOutboundApproval {
        events?.append("approve")
        approvalCalls.append(.init(
            accountID: accountID,
            previewID: previewID,
            expectedPreviewHash: expectedPreviewHash
        ))
        guard !approvalSteps.isEmpty else { throw TestOutboundFailure.message("unexpected approve") }
        return try await approvalSteps.removeFirst().resolve()
    }

    func enqueueGoogleOutbound(
        accountID: UUID,
        request: GoogleOutboundEnqueueRequest
    ) async throws -> GoogleOutboundAccepted {
        events?.append("enqueue")
        enqueueCalls.append(.init(accountID: accountID, request: request))
        guard !enqueueSteps.isEmpty else { throw TestOutboundFailure.message("unexpected enqueue") }
        return try await enqueueSteps.removeFirst().resolve()
    }

    func previewCallsSnapshot() -> [TestOutboundCall] { previewCalls }
    func approvalCallsSnapshot() -> [TestOutboundApprovalCall] { approvalCalls }
    func enqueueCallsSnapshot() -> [TestOutboundEnqueueCall] { enqueueCalls }
}

private actor OutboundAsyncGate {
    private var entered = false
    private var released = false
    private var releaseContinuation: CheckedContinuation<Void, Never>?

    func enterAndWait() async {
        entered = true
        if released { return }
        await withCheckedContinuation { continuation in
            releaseContinuation = continuation
        }
    }

    func waitUntilEntered() async {
        while !entered { await Task.yield() }
    }

    func release() {
        released = true
        releaseContinuation?.resume()
        releaseContinuation = nil
    }
}

private actor OutboundManualExpirySleeper {
    private var entered = false
    private var continuation: CheckedContinuation<Void, any Error>?

    func sleep(for _: TimeInterval) async throws {
        entered = true
        try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
        }
    }

    func waitUntilEntered() async {
        while !entered { await Task.yield() }
    }

    func release() {
        continuation?.resume(returning: ())
        continuation = nil
    }
}

private final class OutboundEventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var events: [String] = []

    func append(_ event: String) {
        lock.lock()
        events.append(event)
        lock.unlock()
    }

    func snapshot() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return events
    }
}

private final class OutboundTestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var date: Date

    init(_ date: Date) { self.date = date }

    func read() -> Date {
        lock.lock()
        defer { lock.unlock() }
        return date
    }

    func advance(by interval: TimeInterval) {
        lock.lock()
        date = date.addingTimeInterval(interval)
        lock.unlock()
    }
}

@MainActor
private final class OutboundTransportBox {
    var transport: any GoogleOutboundTransport

    init(_ transport: any GoogleOutboundTransport) {
        self.transport = transport
    }
}
#endif
