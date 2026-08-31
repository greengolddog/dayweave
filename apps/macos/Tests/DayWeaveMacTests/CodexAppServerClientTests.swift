import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@MainActor
@Suite("Codex app-server boundary")
struct CodexAppServerClientTests {
    @Test("an unsealed test bundle fails closed instead of launching ambient Codex")
    func testUnsealedBundleFailsClosedWithoutLaunchingAmbientCodex() {
        let client = CodexAppServerClient()

        client.startIfNeeded()

        #expect(client.state == .unavailable(CodexAppServerClient.runtimeUnavailableMessage))
        #expect(client.deviceCode == nil)
        #expect(client.verificationURL == nil)
    }

    @Test("device login waits for initialization and uses only the managed device-code method")
    func testDeviceLoginIsOrderedAndManaged() async throws {
        let harness = try CodexProtocolHarness()
        let opened = OpenedURLRecorder()
        let client = CodexAppServerClient(
            launcher: harness,
            verificationPageOpener: { opened.urls.append($0); return true }
        )
        defer {
            client.shutDown()
            harness.cleanUp()
        }

        client.signInWithDeviceCode()
        let initialize = try await harness.nextClientMessage()
        #expect(initialize["method"] as? String == "initialize")
        #expect(initialize["jsonrpc"] == nil)
        let initializeParams = try #require(initialize["params"] as? [String: Any])
        #expect((initializeParams["capabilities"] as? [String: Any])?["experimentalApi"] as? Bool == false)
        await assertNoClientMessage(harness)

        try harness.sendServerMessage([
            "id": try requestID(initialize),
            "result": initializeResult(home: harness.codexHome),
        ])
        let initialized = try await harness.nextClientMessage()
        #expect(initialized["method"] as? String == "initialized")
        #expect((initialized["params"] as? [String: Any])?.isEmpty == true)
        let accountRead = try await harness.nextClientMessage()
        #expect(accountRead["method"] as? String == "account/read")
        #expect((accountRead["params"] as? [String: Any])?["refreshToken"] as? Bool == false)
        await assertNoClientMessage(harness)

        try harness.sendServerMessage([
            "id": try requestID(accountRead),
            "result": ["account": NSNull(), "requiresOpenaiAuth": true],
        ])
        let login = try await harness.nextClientMessage()
        #expect(login["method"] as? String == "account/login/start")
        let loginParams = try #require(login["params"] as? [String: Any])
        #expect(loginParams.count == 1)
        #expect(loginParams["type"] as? String == "chatgptDeviceCode")

        let loginID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(login),
            "result": [
                "type": "chatgptDeviceCode",
                "loginId": loginID,
                "verificationUrl": "https://auth.openai.com/codex/device",
                "userCode": "ABCD-1234",
            ],
        ])
        #expect(await eventually { client.deviceCode == "ABCD-1234" })
        #expect(client.verificationURL?.absoluteString == "https://auth.openai.com/codex/device")
        #expect(opened.urls == [URL(string: "https://auth.openai.com/codex/device")!])

        try harness.sendServerMessage([
            "method": "account/login/completed",
            "emittedAtMs": 1_787_986_845_132 as Int64,
            "params": [
                "loginId": loginID,
                "success": true,
                "error": NSNull(),
            ],
        ])
        let signedInRead = try await harness.nextClientMessage()
        #expect(signedInRead["method"] as? String == "account/read")
        try harness.sendServerMessage([
            "id": try requestID(signedInRead),
            "result": [
                "account": [
                    "type": "chatgpt",
                    "email": "person@example.com",
                    "planType": "plus",
                ],
                "requiresOpenaiAuth": true,
            ],
        ])
        #expect(await eventually {
            client.state == .signedIn(email: "person@example.com", plan: "plus")
        })
        #expect(client.deviceCode == nil)
        #expect(client.verificationURL == nil)
    }

    @Test("device-code cancellation is bound to the returned login identifier")
    func testDeviceCodeCancellationIsBound() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(
            launcher: harness,
            verificationPageOpener: { _ in true }
        )
        defer {
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedOut(client, harness: harness)

        client.signInWithDeviceCode()
        let login = try await harness.nextClientMessage()
        let loginID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(login),
            "result": [
                "type": "chatgptDeviceCode",
                "loginId": loginID,
                "verificationUrl": "https://auth.openai.com/codex/device",
                "userCode": "WXYZ-9876",
            ],
        ])
        #expect(await eventually { client.deviceCode == "WXYZ-9876" })

        client.cancelSignIn()
        #expect(client.deviceCode == nil)
        #expect(client.verificationURL == nil)
        let cancel = try await harness.nextClientMessage()
        #expect(cancel["method"] as? String == "account/login/cancel")
        #expect((cancel["params"] as? [String: Any])?["loginId"] as? String == loginID)

        try harness.sendServerMessage([
            "id": try requestID(cancel),
            "result": ["status": "canceled"],
        ])
        #expect(await eventually { client.state == .signedOut })
        try harness.sendServerMessage([
            "method": "account/login/completed",
            "params": ["loginId": loginID, "success": false, "error": "Canceled"],
        ])
        #expect(await eventually { client.state == .signedOut })
    }

    @Test("server requests are denied and cannot cross the host approval boundary")
    func testServerRequestsAreDenied() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        defer {
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedOut(client, harness: harness)

        try harness.sendServerMessage([
            "method": "remoteControl/status/changed",
            "emittedAtMs": 1_787_986_845_132 as Int64,
            "params": ["status": "disabled"],
        ])
        await assertNoClientMessage(harness)
        #expect(client.state == .signedOut)

        try harness.sendServerMessage([
            "id": "approval-1",
            "method": "item/commandExecution/requestApproval",
            "params": ["command": "touch /tmp/should-not-run"],
        ])
        let denial = try await harness.nextClientMessage()
        #expect(denial["id"] as? String == "approval-1")
        let error = try #require(denial["error"] as? [String: Any])
        #expect((error["code"] as? NSNumber)?.intValue == -32_001)
        #expect((error["message"] as? String)?.contains("denies") == true)
        #expect(client.state == .signedOut)
    }

    @Test("an untrusted verification origin fails closed without opening it")
    func testUntrustedVerificationOriginFailsClosed() async throws {
        let harness = try CodexProtocolHarness()
        let opened = OpenedURLRecorder()
        let client = CodexAppServerClient(
            launcher: harness,
            verificationPageOpener: { opened.urls.append($0); return true }
        )
        defer {
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedOut(client, harness: harness)

        client.signInWithDeviceCode()
        let login = try await harness.nextClientMessage()
        try harness.sendServerMessage([
            "id": try requestID(login),
            "result": [
                "type": "chatgptDeviceCode",
                "loginId": UUID().uuidString.lowercased(),
                "verificationUrl": "https://auth.openai.com.evil.example/codex/device",
                "userCode": "ABCD-1234",
            ],
        ])

        #expect(await eventually {
            client.state == .unavailable("Codex returned an invalid device-code ceremony")
        })
        #expect(opened.urls.isEmpty)
        #expect(client.deviceCode == nil)
        #expect(client.verificationURL == nil)
    }

    @Test("ambient API-key account state is rejected and the runtime is stopped")
    func testAPIKeyAccountStateFailsClosed() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        defer {
            client.shutDown()
            harness.cleanUp()
        }

        client.startIfNeeded()
        let initialize = try await harness.nextClientMessage()
        try harness.sendServerMessage([
            "id": try requestID(initialize),
            "result": initializeResult(home: harness.codexHome),
        ])
        _ = try await harness.nextClientMessage() // initialized
        let account = try await harness.nextClientMessage()
        try harness.sendServerMessage([
            "id": try requestID(account),
            "result": [
                "account": ["type": "apiKey"],
                "requiresOpenaiAuth": true,
            ],
        ])

        #expect(await eventually {
            client.state == .unavailable(CodexAppServerClient.managedLoginMessage)
        })
        #expect(await eventually { !harness.process.isRunning })
    }

    @Test("ambiguous JSONL responses fail closed and clear an exposed ceremony")
    func testAmbiguousResponseClearsCeremony() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        defer {
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedOut(client, harness: harness)

        client.signInWithDeviceCode()
        let login = try await harness.nextClientMessage()
        let loginID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(login),
            "result": [
                "type": "chatgptDeviceCode",
                "loginId": loginID,
                "verificationUrl": "https://auth.openai.com/codex/device",
                "userCode": "ABCD-1234",
            ],
        ])
        #expect(await eventually { client.deviceCode != nil })

        client.cancelSignIn()
        let cancel = try await harness.nextClientMessage()
        try harness.sendServerMessage([
            "id": try requestID(cancel),
            "result": ["status": "canceled"],
            "error": ["code": -1, "message": "ambiguous"],
        ])

        #expect(await eventually {
            client.state == .unavailable("Codex App Server emitted an ambiguous response")
        })
        #expect(client.deviceCode == nil)
        #expect(client.verificationURL == nil)
    }

    @Test("planner conversations use contained read-only turns and stream bounded typed events")
    func testConversationLifecycleAndInterruptAreBoundToExactIdentifiers() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        let recorder = CodexConversationEventRecorder()
        let eventTask = Task { @MainActor in
            for await event in client.conversationEvents() {
                recorder.events.append(event)
            }
        }
        defer {
            eventTask.cancel()
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedIn(client, harness: harness)

        let threadTask = Task {
            try await client.startConversationThread(developerInstructions: "Planner only")
        }
        let threadRequest = try await harness.nextClientMessage()
        #expect(threadRequest["method"] as? String == "thread/start")
        let threadParams = try #require(threadRequest["params"] as? [String: Any])
        #expect(Set(threadParams.keys) == [
            "approvalPolicy", "cwd", "developerInstructions", "ephemeral", "personality",
            "sandbox", "serviceName",
        ])
        #expect(threadParams["approvalPolicy"] as? String == "never")
        #expect(threadParams["sandbox"] as? String == "read-only")
        #expect(threadParams["ephemeral"] as? Bool == true)
        #expect(threadParams["cwd"] as? String == harness.codexHome.path)
        let threadID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(threadRequest),
            "result": conversationThreadStartResult(
                home: harness.codexHome,
                threadID: threadID
            ),
        ])
        #expect(try await threadTask.value == CodexConversationThread(id: threadID))

        let turnTask = Task {
            try await client.startConversationTurn(
                threadID: threadID,
                input: "redacted context and user message"
            )
        }
        let turnRequest = try await harness.nextClientMessage()
        #expect(turnRequest["method"] as? String == "turn/start")
        let turnParams = try #require(turnRequest["params"] as? [String: Any])
        #expect(Set(turnParams.keys) == ["clientUserMessageId", "input", "threadId"])
        #expect(turnParams["threadId"] as? String == threadID)
        let input = try #require(turnParams["input"] as? [[String: Any]])
        #expect(input.count == 1)
        #expect(input[0]["type"] as? String == "text")
        #expect(input[0]["text"] as? String == "redacted context and user message")

        let turnID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(turnRequest),
            "result": ["turn": conversationTurn(id: turnID, status: "inProgress")],
        ])
        #expect(try await turnTask.value == CodexConversationTurn(id: turnID, threadID: threadID))

        try harness.sendServerMessage([
            "method": "turn/started",
            "params": [
                "threadId": threadID,
                "turn": conversationTurn(id: turnID, status: "inProgress"),
            ],
        ])
        let itemID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "method": "item/started",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "startedAtMs": 1_787_986_845_132 as Int64,
                "item": agentMessage(id: itemID, text: "", phase: "final_answer"),
            ],
        ])
        try harness.sendServerMessage([
            "method": "item/agentMessage/delta",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "itemId": itemID,
                "delta": "A safer plan",
            ],
        ])
        try harness.sendServerMessage([
            "method": "item/completed",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "completedAtMs": 1_787_986_845_232 as Int64,
                "item": agentMessage(id: itemID, text: "A safer plan", phase: "final_answer"),
            ],
        ])

        let interruptTask = Task {
            try await client.interruptConversationTurn(threadID: threadID, turnID: turnID)
        }
        let interrupt = try await harness.nextClientMessage()
        #expect(interrupt["method"] as? String == "turn/interrupt")
        let interruptParams = try #require(interrupt["params"] as? [String: Any])
        #expect(interruptParams["threadId"] as? String == threadID)
        #expect(interruptParams["turnId"] as? String == turnID)
        try harness.sendServerMessage([
            "id": try requestID(interrupt),
            "result": [String: Any](),
        ])
        try await interruptTask.value
        try harness.sendServerMessage([
            "method": "turn/completed",
            "params": [
                "threadId": threadID,
                "turn": conversationTurn(id: turnID, status: "interrupted"),
            ],
        ])

        #expect(await eventually {
            recorder.events.contains(.agentMessageDelta(
                threadID: threadID,
                turnID: turnID,
                itemID: itemID,
                phase: .finalAnswer,
                delta: "A safer plan"
            )) && recorder.events.contains(.turnCompleted(
                threadID: threadID,
                turnID: turnID,
                outcome: .interrupted
            ))
        })
        #expect(client.state == .signedIn(email: "person@example.com", plan: "plus"))
    }

    @Test("conversation events with an unbound item identity stop the runtime")
    func testUnboundAgentDeltaFailsClosed() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        defer {
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedIn(client, harness: harness)
        let identifiers = try await startConversation(client, harness: harness)

        try harness.sendServerMessage([
            "method": "item/agentMessage/delta",
            "params": [
                "threadId": identifiers.threadID,
                "turnId": identifiers.turnID,
                "itemId": UUID().uuidString.lowercased(),
                "delta": "unbound",
            ],
        ])

        #expect(await eventually {
            client.state == .unavailable("Codex emitted an invalid agent-message delta")
        })
        #expect(await eventually { !harness.process.isRunning })
    }

    @Test("the session controller streams replies and routes proposals without changing the plan")
    func testConversationControllerRoutesOnlyReviewableProposals() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        let persistenceContext = try Self.makePersistence()
        let block = ScheduleBlock(
            id: UUID(),
            title: "Deep work",
            kind: .task,
            start: Date(timeIntervalSince1970: 1_787_980_000),
            end: Date(timeIntervalSince1970: 1_787_983_600),
            status: .scheduled,
            project: nil,
            notes: "NEVER-SEND-THIS-NOTE",
            energy: .deep,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil,
            placementReason: "NEVER-SEND-THIS-DIAGNOSTIC"
        )
        let responseDate = Date(timeIntervalSince1970: 1_787_986_845)
        let store = PlannerStore(
            blocks: [block],
            persistence: persistenceContext.persistence,
            restoreFromPersistence: false,
            now: { responseDate }
        )
        let controller = CodexConversationController(
            client: client,
            contextProvider: store,
            suggestionRouter: CodexSuggestionInboxRouter(planner: store),
            now: { responseDate }
        )
        defer {
            controller.shutDown()
            client.shutDown()
            harness.cleanUp()
            try? FileManager.default.removeItem(at: persistenceContext.directory)
        }
        try await initializeSignedIn(client, harness: harness)

        controller.send("Protect my focus time")
        let threadRequest = try await harness.nextClientMessage()
        let threadID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(threadRequest),
            "result": conversationThreadStartResult(
                home: harness.codexHome,
                threadID: threadID
            ),
        ])
        let turnRequest = try await harness.nextClientMessage()
        let turnParams = try #require(turnRequest["params"] as? [String: Any])
        let inputs = try #require(turnParams["input"] as? [[String: Any]])
        let transported = try #require(inputs.first?["text"] as? String)
        #expect(transported.contains("Deep work"))
        #expect(transported.contains("Protect my focus time"))
        #expect(!transported.contains("NEVER-SEND-THIS-NOTE"))
        #expect(!transported.contains("NEVER-SEND-THIS-DIAGNOSTIC"))

        let turnID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(turnRequest),
            "result": ["turn": conversationTurn(id: turnID, status: "inProgress")],
        ])
        #expect(await eventually { controller.isTurnActive })

        let itemID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "method": "item/started",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "startedAtMs": 1_787_986_845_132 as Int64,
                "item": agentMessage(id: itemID, text: "", phase: "final_answer"),
            ],
        ])
        try harness.sendServerMessage([
            "method": "item/agentMessage/delta",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "itemId": itemID,
                "delta": "Keep the morning block intact.",
            ],
        ])
        let completedText = """
        Keep the morning block intact.
        <dayweave-item-drafts-v1>{
          "schema": "dayweave.item-drafts/1",
          "drafts": [{
            "summary": "Keep the deep-work block fixed in the morning.",
            "item": {
              "kind": "task",
              "title": "Protect focus block",
              "notes": null,
              "timezone_name": "UTC",
              "duration_seconds": 3600,
              "deadline_at": null,
              "earliest_start_at": null,
              "recurrence": null,
              "flexible_constraints": {},
              "split_policy": {"type": "indivisible"},
              "importance": 80,
              "urgency": 70
            }
          }]
        }</dayweave-item-drafts-v1>
        """
        try harness.sendServerMessage([
            "method": "item/completed",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "completedAtMs": 1_787_986_845_232 as Int64,
                "item": agentMessage(id: itemID, text: completedText, phase: "final_answer"),
            ],
        ])
        try harness.sendServerMessage([
            "method": "turn/completed",
            "params": [
                "threadId": threadID,
                "turn": conversationTurn(id: turnID, status: "completed"),
            ],
        ])

        #expect(await eventually {
            controller.activity == .idle && controller.lastProposalCount == 1
        })
        #expect(controller.messages.last?.text == "Keep the morning block intact.")
        #expect(controller.messages.last?.delivery == .complete)
        #expect(store.suggestions.count == 1)
        let routedSuggestion = try #require(store.suggestions.first)
        #expect(routedSuggestion.state == .pending)
        #expect(routedSuggestion.summary == "Keep the deep-work block fixed in the morning.")
        let routedItemDraft: PlanningSuggestionItemDraft?
        if case let .canonicalItemDraft(itemDraft) = routedSuggestion.payload {
            routedItemDraft = itemDraft
        } else {
            routedItemDraft = nil
        }
        let itemDraft = try #require(routedItemDraft)
        #expect(itemDraft.version == PlanningSuggestionItemDraft.currentVersion)
        #expect(itemDraft.draft.title == "Protect focus block")
        #expect(itemDraft.draft.kind == .task)
        #expect(itemDraft.draft.durationSeconds == 3_600)
        #expect(itemDraft.draft.importance == 80)
        #expect(itemDraft.draft.urgency == 70)
        #expect(itemDraft.draft.isSensitive)
        #expect(itemDraft.draft.status == .inbox)
        #expect(store.blocks == [block])
    }

    @Test("privacy suspension interrupts the active turn, terminates Codex, and preserves its home")
    func testPrivacySuspensionInterruptsAndTerminatesWithoutDeletingCodexHome() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        defer {
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedIn(client, harness: harness)
        let identifiers = try await startConversation(client, harness: harness)
        let retainedLoginState = harness.codexHome.appendingPathComponent("retained-login-state")
        try Data("device-local managed login state".utf8).write(to: retainedLoginState)

        client.suspendForPrivacyBoundary()

        let interrupt = try await harness.nextClientMessage()
        #expect(interrupt["method"] as? String == "turn/interrupt")
        let parameters = try #require(interrupt["params"] as? [String: Any])
        #expect(parameters["threadId"] as? String == identifiers.threadID)
        #expect(parameters["turnId"] as? String == identifiers.turnID)
        #expect(client.state == .stopped)
        #expect(await eventually { !harness.process.isRunning })
        #expect(FileManager.default.fileExists(atPath: retainedLoginState.path))
    }

    @Test("reactivation queued during privacy teardown starts a fresh contained runtime")
    func testPrivacySuspensionSupportsImmediateReactivation() async throws {
        let launcher = RestartingCodexProtocolLauncher()
        let client = CodexAppServerClient(launcher: launcher, verificationPageOpener: { _ in true })
        defer {
            client.shutDown()
            launcher.cleanUp()
        }

        client.startIfNeeded()
        let firstHarness = try #require(launcher.harnesses.first)
        let initialize = try await firstHarness.nextClientMessage()
        try firstHarness.sendServerMessage([
            "id": try requestID(initialize),
            "result": initializeResult(home: firstHarness.codexHome),
        ])
        _ = try await firstHarness.nextClientMessage() // initialized
        let account = try await firstHarness.nextClientMessage()
        try firstHarness.sendServerMessage([
            "id": try requestID(account),
            "result": ["account": NSNull(), "requiresOpenaiAuth": true],
        ])
        #expect(await eventually { client.state == .signedOut })

        client.suspendForPrivacyBoundary()
        client.startIfNeeded()

        #expect(await eventually { launcher.harnesses.count == 2 })
        let secondHarness = try #require(launcher.harnesses.last)
        #expect(secondHarness !== firstHarness)
        let secondInitialize = try await secondHarness.nextClientMessage()
        #expect(secondInitialize["method"] as? String == "initialize")
        #expect(client.state == .starting)
    }

    @Test("queued Codex completion cannot mutate transcript or Inbox after privacy suspension")
    func testPrivacySuspensionInvalidatesLateConversationEvents() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        let persistenceContext = try Self.makePersistence()
        let store = PlannerStore(
            persistence: persistenceContext.persistence,
            restoreFromPersistence: false
        )
        let controller = CodexConversationController(
            client: client,
            contextProvider: store,
            suggestionRouter: CodexSuggestionInboxRouter(planner: store)
        )
        defer {
            controller.shutDown()
            client.shutDown()
            harness.cleanUp()
            try? FileManager.default.removeItem(at: persistenceContext.directory)
        }
        try await initializeSignedIn(client, harness: harness)

        controller.send("Protect this plan")
        let threadRequest = try await harness.nextClientMessage()
        let threadID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(threadRequest),
            "result": conversationThreadStartResult(home: harness.codexHome, threadID: threadID),
        ])
        let turnRequest = try await harness.nextClientMessage()
        let turnID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(turnRequest),
            "result": ["turn": conversationTurn(id: turnID, status: "inProgress")],
        ])
        #expect(await eventually { controller.isTurnActive })

        let itemID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "method": "item/started",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "startedAtMs": 1_787_986_845_132 as Int64,
                "item": agentMessage(id: itemID, text: "", phase: "final_answer"),
            ],
        ])
        try harness.sendServerMessage([
            "method": "item/agentMessage/delta",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "itemId": itemID,
                "delta": "Partial private response",
            ],
        ])
        #expect(await eventually { controller.messages.last?.text == "Partial private response" })

        let completedText = """
        This must never land.
        <dayweave-item-drafts-v1>{
          "schema": "dayweave.item-drafts/1",
          "drafts": [{
            "summary": "Must not reach the Inbox.",
            "item": {
              "kind": "task",
              "title": "Late proposal",
              "notes": null,
              "timezone_name": "UTC",
              "duration_seconds": 1800,
              "deadline_at": null,
              "earliest_start_at": null,
              "recurrence": null,
              "flexible_constraints": {},
              "split_policy": {"type": "indivisible"},
              "importance": 50,
              "urgency": 50
            }
          }]
        }</dayweave-item-drafts-v1>
        """
        // Keep these bytes queued while the main actor crosses the privacy
        // boundary. A previously scheduled receive callback must fail closed.
        try harness.sendServerMessage([
            "method": "item/completed",
            "params": [
                "threadId": threadID,
                "turnId": turnID,
                "completedAtMs": 1_787_986_845_232 as Int64,
                "item": agentMessage(id: itemID, text: completedText, phase: "final_answer"),
            ],
        ])
        try harness.sendServerMessage([
            "method": "turn/completed",
            "params": [
                "threadId": threadID,
                "turn": conversationTurn(id: turnID, status: "completed"),
            ],
        ])

        controller.suspendForPrivacyBoundary()
        let messagesAfterSuspension = controller.messages
        #expect(controller.messages.last?.text == "Partial private response")
        #expect(controller.messages.last?.delivery == .interrupted)
        #expect(controller.activity == .idle)
        #expect(!controller.isTurnActive)
        #expect(store.suggestions.isEmpty)

        let interrupt = try await harness.nextClientMessage()
        #expect(interrupt["method"] as? String == "turn/interrupt")
        #expect(await eventually { !harness.process.isRunning })
        try? await Task.sleep(for: .milliseconds(50))

        #expect(controller.messages == messagesAfterSuspension)
        #expect(store.suggestions.isEmpty)
        #expect(controller.lastProposalCount == 0)
        #expect(client.state == .stopped)
    }

    @Test("a stalled conversation event consumer cannot create an unbounded queue")
    func testConversationEventBufferOverflowFailsClosed() async throws {
        let harness = try CodexProtocolHarness()
        let client = CodexAppServerClient(launcher: harness, verificationPageOpener: { _ in true })
        let stalledStream = client.conversationEvents()
        defer {
            withExtendedLifetime(stalledStream) {}
            client.shutDown()
            harness.cleanUp()
        }
        try await initializeSignedIn(client, harness: harness)
        let identifiers = try await startConversation(client, harness: harness)
        let itemID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "method": "item/started",
            "params": [
                "threadId": identifiers.threadID,
                "turnId": identifiers.turnID,
                "startedAtMs": 1_787_986_845_132 as Int64,
                "item": agentMessage(id: itemID, text: "", phase: "final_answer"),
            ],
        ])
        for _ in 0..<65 {
            try harness.sendServerMessage([
                "method": "item/agentMessage/delta",
                "params": [
                    "threadId": identifiers.threadID,
                    "turnId": identifiers.turnID,
                    "itemId": itemID,
                    "delta": "x",
                ],
            ])
        }

        #expect(await eventually {
            client.state == .unavailable("The Codex conversation event consumer fell behind")
        })
        #expect(await eventually { !harness.process.isRunning })
    }

    private func initializeSignedOut(
        _ client: CodexAppServerClient,
        harness: CodexProtocolHarness
    ) async throws {
        client.startIfNeeded()
        let initialize = try await harness.nextClientMessage()
        #expect(initialize["method"] as? String == "initialize")
        try harness.sendServerMessage([
            "id": try requestID(initialize),
            "result": initializeResult(home: harness.codexHome),
        ])
        let initialized = try await harness.nextClientMessage()
        #expect(initialized["method"] as? String == "initialized")
        let account = try await harness.nextClientMessage()
        #expect(account["method"] as? String == "account/read")
        try harness.sendServerMessage([
            "id": try requestID(account),
            "result": ["account": NSNull(), "requiresOpenaiAuth": true],
        ])
        #expect(await eventually { client.state == .signedOut })
    }

    private func initializeSignedIn(
        _ client: CodexAppServerClient,
        harness: CodexProtocolHarness
    ) async throws {
        client.startIfNeeded()
        let initialize = try await harness.nextClientMessage()
        try harness.sendServerMessage([
            "id": try requestID(initialize),
            "result": initializeResult(home: harness.codexHome),
        ])
        _ = try await harness.nextClientMessage() // initialized
        let account = try await harness.nextClientMessage()
        try harness.sendServerMessage([
            "id": try requestID(account),
            "result": [
                "account": [
                    "type": "chatgpt",
                    "email": "person@example.com",
                    "planType": "plus",
                ],
                "requiresOpenaiAuth": true,
            ],
        ])
        #expect(await eventually {
            client.state == .signedIn(email: "person@example.com", plan: "plus")
        })
    }

    private func startConversation(
        _ client: CodexAppServerClient,
        harness: CodexProtocolHarness
    ) async throws -> (threadID: String, turnID: String) {
        let threadTask = Task {
            try await client.startConversationThread(developerInstructions: "Planner only")
        }
        let threadRequest = try await harness.nextClientMessage()
        let threadID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(threadRequest),
            "result": conversationThreadStartResult(
                home: harness.codexHome,
                threadID: threadID
            ),
        ])
        _ = try await threadTask.value

        let turnTask = Task {
            try await client.startConversationTurn(threadID: threadID, input: "context")
        }
        let turnRequest = try await harness.nextClientMessage()
        let turnID = UUID().uuidString.lowercased()
        try harness.sendServerMessage([
            "id": try requestID(turnRequest),
            "result": ["turn": conversationTurn(id: turnID, status: "inProgress")],
        ])
        _ = try await turnTask.value
        return (threadID, turnID)
    }

    private func conversationThreadStartResult(home: URL, threadID: String) -> [String: Any] {
        [
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "cwd": home.path,
            "instructionSources": [],
            "model": "gpt-test",
            "modelProvider": "openai",
            "reasoningEffort": NSNull(),
            "sandbox": ["type": "readOnly", "networkAccess": false],
            "serviceTier": NSNull(),
            "thread": [
                "cliVersion": "0.150.1",
                "createdAt": 1_787_986_845 as Int64,
                "cwd": home.path,
                "ephemeral": true,
                "id": threadID,
                "modelProvider": "openai",
                "path": NSNull(),
                "preview": "",
                "projectId": NSNull(),
                "sessionId": threadID,
                "source": "appServer",
                "status": ["type": "idle"],
                "turns": [],
                "updatedAt": 1_787_986_845 as Int64,
            ],
        ]
    }

    private func conversationTurn(id: String, status: String) -> [String: Any] {
        ["id": id, "items": [], "status": status]
    }

    private func agentMessage(id: String, text: String, phase: String) -> [String: Any] {
        ["id": id, "type": "agentMessage", "text": text, "phase": phase]
    }

    private func initializeResult(home: URL) -> [String: Any] {
        [
            "codexHome": home.path,
            "platformFamily": "unix",
            "platformOs": "macos",
            "userAgent": "dayweave-test/0.1.0",
        ]
    }

    private static func makePersistence() throws -> (
        directory: URL,
        persistence: EncryptedPlannerPersistence
    ) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "DayWeaveCodexAppServerClientTests-\(UUID().uuidString)",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        let key = try PlannerEncryptionKey(data: Data(repeating: 43, count: 32))
        return (
            directory,
            EncryptedPlannerPersistence(
                fileURL: directory.appendingPathComponent("planner.snapshot.encrypted"),
                key: key
            )
        )
    }

    private func requestID(_ message: [String: Any]) throws -> Int {
        let number = try #require(message["id"] as? NSNumber)
        return number.intValue
    }

    private func eventually(
        _ predicate: @MainActor () -> Bool,
        attempts: Int = 500
    ) async -> Bool {
        for _ in 0..<attempts {
            if predicate() { return true }
            try? await Task.sleep(for: .milliseconds(10))
        }
        return predicate()
    }

    private func assertNoClientMessage(_ harness: CodexProtocolHarness) async {
        try? await Task.sleep(for: .milliseconds(30))
        #expect(harness.drainClientMessages().isEmpty)
    }
}

@MainActor
private final class OpenedURLRecorder {
    var urls: [URL] = []
}

@MainActor
private final class CodexConversationEventRecorder {
    var events: [CodexConversationEvent] = []
}

@MainActor
private final class RestartingCodexProtocolLauncher: CodexRuntimeLaunching {
    private(set) var harnesses: [CodexProtocolHarness] = []

    func launch() throws -> CodexRuntimeSession {
        let harness = try CodexProtocolHarness()
        harnesses.append(harness)
        return try harness.launch()
    }

    func cleanUp() {
        for harness in harnesses {
            harness.cleanUp()
        }
    }
}

@MainActor
private final class CodexProtocolHarness: CodexRuntimeLaunching {
    let codexHome: URL
    let process: Process

    private let base: URL
    private let clientInputWriter: FileHandle
    private let clientInputReader: FileHandle
    private let serverOutputWriter: FileHandle
    private let session: CodexRuntimeSession
    private var clientBuffer = Data()
    private var queuedClientMessages: [[String: Any]] = []
    private var didLaunch = false
    private var didCleanUp = false

    init() throws {
        base = FileManager.default.temporaryDirectory
            .appendingPathComponent("dayweave-codex-test-\(UUID().uuidString.lowercased())", isDirectory: true)
        codexHome = base.appendingPathComponent("home", isDirectory: true)
        let runtimeRoot = base.appendingPathComponent("runtime", isDirectory: true)
        let inputURL = base.appendingPathComponent("client-input.jsonl")
        try FileManager.default.createDirectory(
            at: codexHome,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        try FileManager.default.createDirectory(
            at: runtimeRoot,
            withIntermediateDirectories: false,
            attributes: [.posixPermissions: 0o700]
        )
        #expect(FileManager.default.createFile(atPath: inputURL.path, contents: nil))
        clientInputWriter = try FileHandle(forWritingTo: inputURL)
        clientInputReader = try FileHandle(forReadingFrom: inputURL)

        let output = Pipe()
        serverOutputWriter = output.fileHandleForWriting
        process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sleep")
        process.arguments = ["60"]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()

        session = CodexRuntimeSession(
            process: process,
            input: clientInputWriter,
            output: output.fileHandleForReading,
            codexHome: codexHome,
            runtimeRoot: runtimeRoot,
            runtimeRootIdentity: try CodexRuntimeLauncher.identity(
                of: runtimeRoot,
                followSymlink: false
            )
        )
    }

    func launch() throws -> CodexRuntimeSession {
        guard !didLaunch else { throw CodexProtocolHarnessError.launchedTwice }
        didLaunch = true
        return session
    }

    func sendServerMessage(_ message: [String: Any]) throws {
        var data = try JSONSerialization.data(withJSONObject: message, options: [.sortedKeys])
        data.append(0x0A)
        try serverOutputWriter.write(contentsOf: data)
    }

    func nextClientMessage() async throws -> [String: Any] {
        for _ in 0..<100 {
            drainClientInput()
            if !queuedClientMessages.isEmpty {
                return queuedClientMessages.removeFirst()
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        throw CodexProtocolHarnessError.timedOutWaitingForClient
    }

    func drainClientMessages() -> [[String: Any]] {
        drainClientInput()
        let messages = queuedClientMessages
        queuedClientMessages.removeAll()
        return messages
    }

    func cleanUp() {
        guard !didCleanUp else { return }
        didCleanUp = true
        try? serverOutputWriter.close()
        try? clientInputReader.close()
        if process.isRunning {
            process.terminate()
            process.waitUntilExit()
        }
        if FileManager.default.fileExists(atPath: base.path) {
            try? FileManager.default.removeItem(at: base)
        }
    }

    private func drainClientInput() {
        try? clientInputWriter.synchronize()
        if let data = try? clientInputReader.readToEnd(), !data.isEmpty {
            clientBuffer.append(data)
        }
        while let newline = clientBuffer.firstIndex(of: 0x0A) {
            let line = Data(clientBuffer[..<newline])
            clientBuffer.removeSubrange(...newline)
            guard !line.isEmpty,
                  let object = try? JSONSerialization.jsonObject(with: line),
                  let message = object as? [String: Any] else { continue }
            queuedClientMessages.append(message)
        }
    }
}

private enum CodexProtocolHarnessError: Error {
    case launchedTwice
    case timedOutWaitingForClient
}
#endif
