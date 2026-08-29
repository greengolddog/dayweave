import AppKit
import Darwin
import Foundation
import os

enum CodexConversationClientError: LocalizedError, Equatable, Sendable {
    case notSignedIn
    case unavailable(String)
    case busy
    case invalidInput
    case requestFailed(String)
    case connectionClosed(String)

    var errorDescription: String? {
        switch self {
        case .notSignedIn:
            "Sign in with ChatGPT before starting a conversation."
        case let .unavailable(message), let .requestFailed(message), let .connectionClosed(message):
            message
        case .busy:
            "Wait for the current Codex request to finish."
        case .invalidInput:
            "The conversation request was empty, oversized, or otherwise invalid."
        }
    }
}

struct CodexConversationThread: Equatable, Sendable {
    let id: String
}

struct CodexConversationTurn: Equatable, Sendable {
    let id: String
    let threadID: String
}

enum CodexAgentMessagePhase: Equatable, Sendable {
    case commentary
    case finalAnswer
    case unspecified
}

enum CodexConversationTurnOutcome: Equatable, Sendable {
    case completed
    case interrupted
    case failed(String)
}

enum CodexConversationEvent: Equatable, Sendable {
    case threadStarted(threadID: String)
    case turnStarted(threadID: String, turnID: String)
    case agentMessageDelta(
        threadID: String,
        turnID: String,
        itemID: String,
        phase: CodexAgentMessagePhase,
        delta: String
    )
    case agentMessageCompleted(
        threadID: String,
        turnID: String,
        itemID: String,
        phase: CodexAgentMessagePhase,
        text: String
    )
    case turnCompleted(
        threadID: String,
        turnID: String,
        outcome: CodexConversationTurnOutcome
    )
    case connectionClosed(String)
}

@MainActor
final class CodexAppServerClient: ObservableObject {
    private static let logger = Logger(subsystem: "com.greengolddog.dayweave", category: "Codex")
    static let runtimeUnavailableMessage = CodexRuntimeLauncher.unavailableMessage
    static let managedLoginMessage =
        "This contained runtime supports managed ChatGPT device-code login only."

    private static let maximumLineBytes = 1_048_576
    private static let maximumBufferedBytes = 2 * maximumLineBytes
    private static let maximumPendingRequests = 32
    private static let maximumRequestID = 1_000_000_000
    private static let maximumConversationThreads = 8
    private static let maximumConversationSubscribers = 4
    private static let maximumConversationInputBytes = 96 * 1_024
    private static let maximumDeveloperInstructionBytes = 16 * 1_024
    private static let maximumAgentDeltaBytes = 32 * 1_024
    private static let maximumAgentMessageBytes = 128 * 1_024
    private static let maximumAgentMessagesPerTurn = 16
    private static let allowedPlanTypes: Set<String> = [
        "free", "go", "plus", "pro", "prolite", "team",
        "self_serve_business_prolite", "self_serve_business_usage_based", "business",
        "ent26", "enterprise_cbp_automation", "enterprise_cbp_usage_based", "enterprise",
        "edu", "edu_plus", "edu_pro", "unknown",
    ]

    enum ConnectionState: Equatable {
        case stopped
        case starting
        case signedOut
        case signingIn
        case cancellingSignIn
        case signedIn(email: String?, plan: String)
        case unavailable(String)

        var title: String {
            switch self {
            case .stopped: "Not started"
            case .starting: "Connecting…"
            case .signedOut: "Sign in required"
            case .signingIn: "Finish ChatGPT sign-in"
            case .cancellingSignIn: "Canceling sign-in…"
            case let .signedIn(_, plan): "Connected · \(Self.displayName(for: plan))"
            case .unavailable: "Unavailable"
            }
        }

        var isConnected: Bool {
            if case .signedIn = self { return true }
            return false
        }

        var failureMessage: String? {
            if case let .unavailable(message) = self { return message }
            return nil
        }

        var isBusy: Bool {
            switch self {
            case .starting, .signingIn, .cancellingSignIn: true
            default: false
            }
        }

        private static func displayName(for plan: String) -> String {
            switch plan {
            case "prolite": "Pro Lite"
            case "self_serve_business_prolite": "Business Pro Lite"
            case "self_serve_business_usage_based": "Business"
            case "enterprise_cbp_automation", "enterprise_cbp_usage_based": "Enterprise"
            case "edu_plus": "Education Plus"
            case "edu_pro": "Education Pro"
            default:
                plan
                    .split(separator: "_")
                    .map { $0.capitalized }
                    .joined(separator: " ")
            }
        }
    }

    private enum RequestKind: Equatable {
        case initialize
        case account
        case deviceCodeLogin
        case cancelLogin(loginID: String)
        case logout
        case threadStart
        case turnStart(threadID: String)
        case interrupt(threadID: String, turnID: String)

        var failureDescription: String {
            switch self {
            case .initialize: "Codex initialization was rejected"
            case .account: "Codex account state could not be read"
            case .deviceCodeLogin: "ChatGPT sign-in could not be started"
            case .cancelLogin: "ChatGPT sign-in could not be canceled"
            case .logout: "Codex sign-out could not be completed"
            case .threadStart: "Codex could not start a private conversation"
            case .turnStart: "Codex could not start the response"
            case .interrupt: "Codex could not stop the response"
            }
        }

        var timeoutSeconds: UInt64 {
            switch self {
            case .initialize, .account, .cancelLogin, .logout, .interrupt: 10
            case .deviceCodeLogin, .threadStart, .turnStart: 20
            }
        }

        var isConversationRequest: Bool {
            switch self {
            case .threadStart, .turnStart, .interrupt: true
            default: false
            }
        }
    }

    private enum ConversationRequestCompletion {
        case thread(CheckedContinuation<CodexConversationThread, any Error>)
        case turn(CheckedContinuation<CodexConversationTurn, any Error>)
        case interrupt(CheckedContinuation<Void, any Error>)
    }

    private struct AgentMessageIdentity: Equatable {
        let threadID: String
        let turnID: String
        let phase: CodexAgentMessagePhase
    }

    private enum ParsedTurnStatus: Equatable {
        case inProgress
        case completed
        case interrupted
        case failed(String)
    }

    @Published private(set) var state: ConnectionState = .stopped
    @Published private(set) var deviceCode: String?
    @Published private(set) var verificationURL: URL?

    private let launcher: any CodexRuntimeLaunching
    private let verificationPageOpener: (URL) -> Bool
    private var runtime: CodexRuntimeSession?
    private var receiveBuffer = Data()
    private var nextRequestID = 1
    private var pending: [Int: RequestKind] = [:]
    private var requestTimeouts: [Int: Task<Void, Never>] = [:]
    private var conversationRequestCompletions: [Int: ConversationRequestCompletion] = [:]
    private var conversationEventContinuations: [
        UUID: AsyncStream<CodexConversationEvent>.Continuation
    ] = [:]
    private var knownConversationThreadIDs: Set<String> = []
    private var activeConversationTurns: [String: String] = [:]
    private var agentMessageIdentities: [String: AgentMessageIdentity] = [:]
    private var completedAgentMessageIDs: Set<String> = []
    private var pendingLoginID: String?
    private var retiredLoginIDs: Set<String> = []
    private var isInitialized = false
    private var queuedDeviceCodeSignIn = false
    private var restartWhenStopped = false
    private var cancelRequested = false
    private var isStopping = false
    private var isShuttingDown = false

    convenience init() {
        self.init(
            launcher: CodexRuntimeLauncher(),
            verificationPageOpener: { NSWorkspace.shared.open($0) }
        )
    }

    init(
        launcher: any CodexRuntimeLaunching,
        verificationPageOpener: @escaping (URL) -> Bool = { NSWorkspace.shared.open($0) }
    ) {
        self.launcher = launcher
        self.verificationPageOpener = verificationPageOpener
    }

    func conversationEvents() -> AsyncStream<CodexConversationEvent> {
        let subscriberID = UUID()
        return AsyncStream(bufferingPolicy: .bufferingNewest(64)) { continuation in
            guard conversationEventContinuations.count < Self.maximumConversationSubscribers else {
                continuation.finish()
                return
            }
            conversationEventContinuations[subscriberID] = continuation
            continuation.onTermination = { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.conversationEventContinuations.removeValue(forKey: subscriberID)
                }
            }
        }
    }

    private func handleThreadStart(_ result: [String: Any], requestID: Int) {
        let allowedKeys: Set<String> = [
            "approvalPolicy", "approvalsReviewer", "cwd", "instructionSources", "model",
            "modelProvider", "reasoningEffort", "sandbox", "serviceTier", "thread",
        ]
        guard Set(result.keys).isSubset(of: allowedKeys),
              result["approvalPolicy"] as? String == "never",
              let reviewer = result["approvalsReviewer"] as? String,
              ["user", "auto_review", "guardian_subagent"].contains(reviewer),
              let runtime,
              let cwd = result["cwd"] as? String,
              URL(fileURLWithPath: cwd).standardizedFileURL
                == runtime.codexHome.standardizedFileURL,
              let model = result["model"] as? String,
              !model.isEmpty,
              model.utf8.count <= 256,
              result["modelProvider"] as? String == "openai",
              isReadOnlySandbox(result["sandbox"]),
              areEmptyInstructionSources(result["instructionSources"]),
              let thread = result["thread"] as? [String: Any],
              let threadID = validatedConversationThreadID(thread, requireEphemeral: true),
              knownConversationThreadIDs.count < Self.maximumConversationThreads,
              let completion = conversationRequestCompletions.removeValue(forKey: requestID),
              case let .thread(continuation) = completion else {
            stopForProtocolFailure("Codex returned an invalid private-thread response")
            return
        }
        knownConversationThreadIDs.insert(threadID)
        continuation.resume(returning: CodexConversationThread(id: threadID))
    }

    private func handleTurnStart(
        _ result: [String: Any],
        requestID: Int,
        expectedThreadID: String
    ) {
        guard Set(result.keys) == ["turn"],
              knownConversationThreadIDs.contains(expectedThreadID),
              activeConversationTurns[expectedThreadID] == nil,
              let turn = result["turn"] as? [String: Any],
              let parsed = validatedTurn(turn),
              case .inProgress = parsed.status,
              let completion = conversationRequestCompletions.removeValue(forKey: requestID),
              case let .turn(continuation) = completion else {
            stopForProtocolFailure("Codex returned an invalid turn-start response")
            return
        }
        activeConversationTurns[expectedThreadID] = parsed.id
        emitConversationEvent(.turnStarted(
            threadID: expectedThreadID,
            turnID: parsed.id
        ))
        continuation.resume(returning: CodexConversationTurn(
            id: parsed.id,
            threadID: expectedThreadID
        ))
    }

    private func handleTurnInterrupt(
        _ result: [String: Any],
        requestID: Int,
        expectedThreadID: String,
        expectedTurnID: String
    ) {
        guard result.isEmpty,
              knownConversationThreadIDs.contains(expectedThreadID),
              activeConversationTurns[expectedThreadID] == expectedTurnID
                || activeConversationTurns[expectedThreadID] == nil,
              let completion = conversationRequestCompletions.removeValue(forKey: requestID),
              case let .interrupt(continuation) = completion else {
            stopForProtocolFailure("Codex returned an invalid turn-interrupt response")
            return
        }
        continuation.resume(returning: ())
    }

    func startConversationThread(
        developerInstructions: String
    ) async throws -> CodexConversationThread {
        guard case .signedIn = state else { throw CodexConversationClientError.notSignedIn }
        guard let runtime, isInitialized else {
            throw CodexConversationClientError.unavailable("Codex App Server is not ready")
        }
        guard !developerInstructions.isEmpty,
              developerInstructions.utf8.count <= Self.maximumDeveloperInstructionBytes else {
            throw CodexConversationClientError.invalidInput
        }
        guard knownConversationThreadIDs.count < Self.maximumConversationThreads,
              !pending.values.contains(.threadStart) else {
            throw CodexConversationClientError.busy
        }

        return try await withCheckedThrowingContinuation { continuation in
            _ = send(
                method: "thread/start",
                params: [
                    "approvalPolicy": "never",
                    "cwd": runtime.codexHome.path,
                    "developerInstructions": developerInstructions,
                    "ephemeral": true,
                    "personality": "friendly",
                    "sandbox": "read-only",
                    "serviceName": "dayweave",
                ],
                kind: .threadStart,
                conversationCompletion: .thread(continuation)
            )
        }
    }

    func startConversationTurn(
        threadID: String,
        input: String
    ) async throws -> CodexConversationTurn {
        guard case .signedIn = state else { throw CodexConversationClientError.notSignedIn }
        guard runtime != nil, isInitialized else {
            throw CodexConversationClientError.unavailable("Codex App Server is not ready")
        }
        guard isValidOpaqueIdentifier(threadID),
              knownConversationThreadIDs.contains(threadID),
              input.utf8.count <= Self.maximumConversationInputBytes,
              !input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw CodexConversationClientError.invalidInput
        }
        guard activeConversationTurns[threadID] == nil,
              !pending.values.contains(where: {
                  if case let .turnStart(candidate) = $0 { return candidate == threadID }
                  return false
              }) else {
            throw CodexConversationClientError.busy
        }

        return try await withCheckedThrowingContinuation { continuation in
            _ = send(
                method: "turn/start",
                params: [
                    "threadId": threadID,
                    "clientUserMessageId": UUID().uuidString.lowercased(),
                    "input": [["type": "text", "text": input]],
                ],
                kind: .turnStart(threadID: threadID),
                conversationCompletion: .turn(continuation)
            )
        }
    }

    func interruptConversationTurn(
        threadID: String,
        turnID: String
    ) async throws {
        guard case .signedIn = state else { throw CodexConversationClientError.notSignedIn }
        guard runtime != nil, isInitialized else {
            throw CodexConversationClientError.unavailable("Codex App Server is not ready")
        }
        guard isValidOpaqueIdentifier(threadID),
              isValidOpaqueIdentifier(turnID),
              knownConversationThreadIDs.contains(threadID),
              activeConversationTurns[threadID] == turnID else {
            throw CodexConversationClientError.invalidInput
        }
        guard !pending.values.contains(where: {
            if case let .interrupt(candidateThread, candidateTurn) = $0 {
                return candidateThread == threadID && candidateTurn == turnID
            }
            return false
        }) else {
            throw CodexConversationClientError.busy
        }

        try await withCheckedThrowingContinuation { continuation in
            _ = send(
                method: "turn/interrupt",
                params: ["threadId": threadID, "turnId": turnID],
                kind: .interrupt(threadID: threadID, turnID: turnID),
                conversationCompletion: .interrupt(continuation)
            )
        }
    }

    func startIfNeeded() {
        guard runtime == nil, !isStopping else {
            if isStopping, isShuttingDown {
                restartWhenStopped = true
            }
            return
        }
        restartWhenStopped = false
        resetProtocolState(keepingQueuedSignIn: true)
        state = .starting

        do {
            let runtime = try launcher.launch()
            self.runtime = runtime
            runtime.output.readabilityHandler = { [weak self] handle in
                let data = handle.availableData
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    if data.isEmpty {
                        self.stopForProtocolFailure("Codex App Server closed its output")
                    } else {
                        self.receive(data)
                    }
                }
            }
            runtime.process.terminationHandler = { [weak self] process in
                Task { @MainActor [weak self] in
                    self?.handleTermination(exitCode: process.terminationStatus)
                }
            }
            guard runtime.process.isRunning else {
                handleTermination(exitCode: runtime.process.terminationStatus)
                return
            }
            Self.logger.info("Verified contained Codex App Server launched")
            _ = send(
                method: "initialize",
                params: [
                    "clientInfo": [
                        "name": "dayweave",
                        "title": "DayWeave",
                        "version": "0.1.0",
                    ],
                    "capabilities": ["experimentalApi": false],
                ],
                kind: .initialize
            )
        } catch {
            Self.logger.error("Verified Codex runtime launch failed")
            resetProtocolState(keepingQueuedSignIn: false)
            state = .unavailable(Self.runtimeUnavailableMessage)
        }
    }

    func signInWithDeviceCode() {
        switch state {
        case .signedIn, .signingIn, .cancellingSignIn:
            return
        default:
            break
        }
        queuedDeviceCodeSignIn = true
        deviceCode = nil
        verificationURL = nil
        if runtime == nil {
            startIfNeeded()
        } else {
            beginQueuedDeviceCodeSignInIfReady()
        }
    }

    func cancelSignIn() {
        queuedDeviceCodeSignIn = false
        cancelRequested = true
        deviceCode = nil
        verificationURL = nil

        if let pendingLoginID {
            requestLoginCancellation(loginID: pendingLoginID)
            return
        }
        if pending.values.contains(.deviceCodeLogin) {
            state = .cancellingSignIn
            return
        }
        cancelRequested = false
        state = isInitialized ? .signedOut : .starting
    }

    @discardableResult
    func openVerificationPage() -> Bool {
        guard let verificationURL, isAllowedVerificationURL(verificationURL) else { return false }
        return verificationPageOpener(verificationURL)
    }

    func retry() {
        guard runtime == nil else { return }
        startIfNeeded()
    }

    func shutDown() {
        restartWhenStopped = false
        guard let runtime else {
            resetProtocolState(keepingQueuedSignIn: false)
            state = .stopped
            return
        }
        isShuttingDown = true
        isStopping = true
        state = .stopped
        runtime.output.readabilityHandler = nil
        resetProtocolState(keepingQueuedSignIn: false)
        terminate(runtime)
    }

    /// Best-effort interrupts every turn whose identity was attested on this
    /// connection, then tears down the authenticated runtime. `CodexRuntimeSession`
    /// cleanup removes only the private executable copy; the device-local
    /// `CODEX_HOME` containing managed login state remains in place.
    func suspendForPrivacyBoundary() {
        let turns = activeConversationTurns.sorted { lhs, rhs in
            if lhs.key != rhs.key { return lhs.key < rhs.key }
            return lhs.value < rhs.value
        }
        for (threadID, turnID) in turns where !pending.values.contains(where: {
            if case let .interrupt(candidateThreadID, candidateTurnID) = $0 {
                return candidateThreadID == threadID && candidateTurnID == turnID
            }
            return false
        }) {
            _ = send(
                method: "turn/interrupt",
                params: ["threadId": threadID, "turnId": turnID],
                kind: .interrupt(threadID: threadID, turnID: turnID)
            )
        }
        shutDown()
    }

    func signOut() {
        guard runtime != nil, isInitialized,
              case .signedIn = state,
              !pending.values.contains(.logout) else { return }
        _ = send(method: "account/logout", params: nil, kind: .logout)
    }

    func refreshAccount() {
        guard runtime != nil, isInitialized else {
            startIfNeeded()
            return
        }
        guard !pending.values.contains(.account) else { return }
        _ = send(
            method: "account/read",
            params: ["refreshToken": false],
            kind: .account
        )
    }

    private func beginQueuedDeviceCodeSignInIfReady() {
        guard queuedDeviceCodeSignIn,
              isInitialized,
              pendingLoginID == nil,
              !pending.values.contains(.account),
              !pending.values.contains(.deviceCodeLogin),
              !pending.values.contains(where: {
                  if case .cancelLogin = $0 { return true }
                  return false
              }) else { return }
        guard case .signedOut = state else { return }

        queuedDeviceCodeSignIn = false
        cancelRequested = false
        state = .signingIn
        _ = send(
            method: "account/login/start",
            params: ["type": "chatgptDeviceCode"],
            kind: .deviceCodeLogin
        )
    }

    private func requestLoginCancellation(loginID: String) {
        guard isInitialized,
              !pending.values.contains(where: {
                  if case .cancelLogin = $0 { return true }
                  return false
              }) else { return }
        state = .cancellingSignIn
        _ = send(
            method: "account/login/cancel",
            params: ["loginId": loginID],
            kind: .cancelLogin(loginID: loginID)
        )
    }

    private func send(
        method: String,
        params: [String: Any]?,
        kind: RequestKind,
        conversationCompletion: ConversationRequestCompletion? = nil
    ) -> Bool {
        guard let runtime else {
            if let conversationCompletion {
                resume(
                    conversationCompletion,
                    throwing: CodexConversationClientError.unavailable(
                        "Codex App Server is not running"
                    )
                )
            }
            state = .unavailable("Codex App Server is not running")
            return false
        }
        guard pending.count < Self.maximumPendingRequests,
              nextRequestID <= Self.maximumRequestID else {
            if let conversationCompletion {
                resume(conversationCompletion, throwing: CodexConversationClientError.busy)
            }
            stopForProtocolFailure("Codex request capacity was exceeded")
            return false
        }
        let id = nextRequestID
        nextRequestID += 1
        var payload: [String: Any] = ["id": id, "method": method]
        if let params { payload["params"] = params }

        do {
            var data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
            guard data.count <= Self.maximumLineBytes else {
                throw CodexClientProtocolError.messageTooLarge
            }
            data.append(0x0A)
            pending[id] = kind
            if let conversationCompletion {
                conversationRequestCompletions[id] = conversationCompletion
            }
            requestTimeouts[id] = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .seconds(kind.timeoutSeconds))
                guard !Task.isCancelled else { return }
                self?.requestTimedOut(id: id)
            }
            try runtime.input.write(contentsOf: data)
            return true
        } catch {
            pending.removeValue(forKey: id)
            requestTimeouts.removeValue(forKey: id)?.cancel()
            if let completion = conversationRequestCompletions.removeValue(forKey: id) {
                resume(
                    completion,
                    throwing: CodexConversationClientError.connectionClosed(
                        "Could not communicate with Codex App Server"
                    )
                )
            }
            stopForProtocolFailure("Could not communicate with Codex App Server")
            return false
        }
    }

    private func sendInitializedNotification() -> Bool {
        writeUntracked([
            "method": "initialized",
            "params": [String: Any](),
        ])
    }

    private func denyServerRequest(id: IncomingRequestID) {
        _ = writeUntracked([
            "id": id.jsonValue,
            "error": [
                "code": -32_001,
                "message": "DayWeave denies server-initiated requests",
            ],
        ])
    }

    private func writeUntracked(_ payload: [String: Any]) -> Bool {
        guard let runtime else { return false }
        do {
            var data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
            guard data.count <= Self.maximumLineBytes else {
                throw CodexClientProtocolError.messageTooLarge
            }
            data.append(0x0A)
            try runtime.input.write(contentsOf: data)
            return true
        } catch {
            stopForProtocolFailure("Could not communicate with Codex App Server")
            return false
        }
    }

    private func receive(_ data: Data) {
        guard runtime != nil, !isStopping else { return }
        guard data.count <= Self.maximumBufferedBytes,
              receiveBuffer.count <= Self.maximumBufferedBytes - data.count else {
            stopForProtocolFailure("Codex App Server output exceeded its safety bound")
            return
        }
        receiveBuffer.append(data)
        while let newline = receiveBuffer.firstIndex(of: 0x0A) {
            let line = Data(receiveBuffer[..<newline])
            receiveBuffer.removeSubrange(...newline)
            guard !line.isEmpty, line.count <= Self.maximumLineBytes,
                  let object = try? JSONSerialization.jsonObject(with: line),
                  let message = object as? [String: Any] else {
                stopForProtocolFailure("Codex App Server emitted an invalid protocol message")
                return
            }
            handle(message)
            // A fatal protocol decision starts asynchronous process teardown. Do not
            // keep interpreting lines that were already present in the same read,
            // because they could overwrite the first (authoritative) failure state.
            guard runtime != nil, !isStopping else { return }
        }
        if receiveBuffer.count > Self.maximumLineBytes {
            stopForProtocolFailure("Codex App Server emitted an oversized protocol line")
        }
    }

    private func handle(_ message: [String: Any]) {
        if let method = message["method"] as? String {
            guard !method.isEmpty, method.utf8.count <= 256 else {
                stopForProtocolFailure("Codex App Server emitted an invalid method")
                return
            }
            if let id = incomingRequestID(message["id"]) {
                let allowedKeys: Set<String> = ["id", "method", "params"]
                guard Set(message.keys).isSubset(of: allowedKeys),
                      message["params"] == nil || message["params"] is [String: Any] else {
                    stopForProtocolFailure("Codex App Server emitted an invalid request")
                    return
                }
                denyServerRequest(id: id)
            } else if message["id"] == nil {
                let allowedKeys: Set<String> = ["method", "params", "emittedAtMs"]
                guard Set(message.keys).isSubset(of: allowedKeys),
                      message["params"] == nil || message["params"] is [String: Any],
                      message["emittedAtMs"] == nil
                        || exactSignedInteger(message["emittedAtMs"]).map({ $0 >= 0 }) == true else {
                    stopForProtocolFailure("Codex App Server emitted an invalid notification")
                    return
                }
                handleNotification(method: method, params: message["params"] as? [String: Any])
            } else {
                stopForProtocolFailure("Codex App Server emitted an invalid request identifier")
            }
            return
        }

        guard let id = exactInteger(message["id"]),
              let kind = pending.removeValue(forKey: id) else {
            stopForProtocolFailure("Codex App Server emitted an unexpected response")
            return
        }
        requestTimeouts.removeValue(forKey: id)?.cancel()
        let hasResult = message.keys.contains("result")
        let hasError = message.keys.contains("error")
        let allowedResponseKeys: Set<String> = hasResult
            ? ["id", "result"]
            : ["id", "error"]
        guard hasResult != hasError,
              Set(message.keys) == allowedResponseKeys else {
            stopForProtocolFailure("Codex App Server emitted an ambiguous response")
            return
        }
        if hasError {
            guard isValidRPCError(message["error"]) else {
                stopForProtocolFailure("Codex App Server emitted an invalid error response")
                return
            }
            if kind.isConversationRequest {
                let detail = (message["error"] as? [String: Any])?["message"] as? String
                let failure = detail.map { "\(kind.failureDescription): \($0)" }
                    ?? kind.failureDescription
                if let completion = conversationRequestCompletions.removeValue(forKey: id) {
                    resume(
                        completion,
                        throwing: CodexConversationClientError.requestFailed(failure)
                    )
                }
                return
            }
            stopForProtocolFailure(kind.failureDescription)
            return
        }
        guard let result = message["result"] as? [String: Any] else {
            stopForProtocolFailure("Codex App Server emitted an invalid response body")
            return
        }

        switch kind {
        case .initialize:
            guard let runtime,
                  result["platformOs"] as? String == "macos",
                  result["platformFamily"] as? String == "unix",
                  let codexHome = result["codexHome"] as? String,
                  URL(fileURLWithPath: codexHome).standardizedFileURL
                    == runtime.codexHome.standardizedFileURL,
                  let userAgent = result["userAgent"] as? String,
                  !userAgent.isEmpty,
                  userAgent.utf8.count <= 512,
                  sendInitializedNotification() else {
                stopForProtocolFailure("Codex initialization attestation failed")
                return
            }
            isInitialized = true
            refreshAccount()
        case .account:
            updateAccount(from: result)
        case .deviceCodeLogin:
            handleDeviceCodeLogin(result)
        case let .cancelLogin(loginID):
            handleCancelLogin(result, loginID: loginID)
        case .logout:
            guard result.isEmpty else {
                stopForProtocolFailure("Codex returned an invalid sign-out response")
                return
            }
            deviceCode = nil
            verificationURL = nil
            pendingLoginID = nil
            queuedDeviceCodeSignIn = false
            cancelRequested = false
            invalidateConversationState(message: "ChatGPT signed out")
            state = .signedOut
        case .threadStart:
            handleThreadStart(result, requestID: id)
        case let .turnStart(threadID):
            handleTurnStart(result, requestID: id, expectedThreadID: threadID)
        case let .interrupt(threadID, turnID):
            handleTurnInterrupt(
                result,
                requestID: id,
                expectedThreadID: threadID,
                expectedTurnID: turnID
            )
        }
    }

    private func handleDeviceCodeLogin(_ result: [String: Any]) {
        guard result["type"] as? String == "chatgptDeviceCode",
              let loginID = result["loginId"] as? String,
              UUID(uuidString: loginID) != nil,
              let userCode = result["userCode"] as? String,
              (4...32).contains(userCode.utf8.count),
              userCode.utf8.allSatisfy({ $0.isASCIIAlphaNumeric || $0 == 45 }),
              let value = result["verificationUrl"] as? String,
              let url = URL(string: value),
              isAllowedVerificationURL(url) else {
            stopForProtocolFailure("Codex returned an invalid device-code ceremony")
            return
        }
        pendingLoginID = loginID
        if cancelRequested {
            deviceCode = nil
            verificationURL = nil
            requestLoginCancellation(loginID: loginID)
        } else {
            deviceCode = userCode
            verificationURL = url
            _ = openVerificationPage()
            state = .signingIn
        }
    }

    private func handleCancelLogin(_ result: [String: Any], loginID: String) {
        guard let status = result["status"] as? String,
              result.count == 1,
              status == "canceled" || status == "notFound" else {
            stopForProtocolFailure("Codex returned an invalid sign-in cancellation")
            return
        }
        deviceCode = nil
        verificationURL = nil
        cancelRequested = false
        guard retiredLoginIDs.count < 8 else {
            stopForProtocolFailure("Codex sign-in cancellation capacity was exceeded")
            return
        }
        if pendingLoginID == loginID { pendingLoginID = nil }
        retiredLoginIDs.insert(loginID)
        if status == "canceled" {
            state = .signedOut
        } else {
            state = .signingIn
            refreshAccount()
        }
    }

    private func handleNotification(method: String, params: [String: Any]?) {
        switch method {
        case "account/login/completed":
            guard let params,
                  let loginID = params["loginId"] as? String,
                  let success = params["success"] as? Bool,
                  isValidLoginCompletionError(params["error"], success: success) else {
                stopForProtocolFailure("Codex emitted an invalid login completion")
                return
            }
            if retiredLoginIDs.remove(loginID) != nil {
                if success { refreshAccount() }
                return
            }
            guard loginID == pendingLoginID else {
                stopForProtocolFailure("Codex emitted an invalid login completion")
                return
            }
            pendingLoginID = nil
            cancelRequested = false
            deviceCode = nil
            verificationURL = nil
            if success {
                state = .signingIn
                refreshAccount()
            } else {
                state = .signedOut
            }
        case "account/updated":
            guard let params,
                  isAllowedAccountUpdate(params) else {
                stopForProtocolFailure("Codex emitted an invalid account update")
                return
            }
            refreshAccount()
        case "thread/started":
            handleThreadStartedNotification(params)
        case "turn/started":
            handleTurnStartedNotification(params)
        case "item/started":
            handleItemStartedNotification(params)
        case "item/agentMessage/delta":
            handleAgentMessageDeltaNotification(params)
        case "item/completed":
            handleItemCompletedNotification(params)
        case "turn/completed":
            handleTurnCompletedNotification(params)
        default:
            break
        }
    }

    private func handleThreadStartedNotification(_ params: [String: Any]?) {
        guard let params,
              Set(params.keys) == ["thread"],
              let thread = params["thread"] as? [String: Any],
              let threadID = validatedConversationThreadID(thread, requireEphemeral: true),
              knownConversationThreadIDs.contains(threadID) else {
            stopForProtocolFailure("Codex emitted an invalid thread-started notification")
            return
        }
        emitConversationEvent(.threadStarted(threadID: threadID))
    }

    private func handleTurnStartedNotification(_ params: [String: Any]?) {
        guard let params,
              Set(params.keys) == ["threadId", "turn"],
              let threadID = params["threadId"] as? String,
              knownConversationThreadIDs.contains(threadID),
              let turn = params["turn"] as? [String: Any],
              let parsed = validatedTurn(turn),
              case .inProgress = parsed.status,
              activeConversationTurns[threadID] == parsed.id else {
            stopForProtocolFailure("Codex emitted an invalid turn-started notification")
            return
        }
        emitConversationEvent(.turnStarted(threadID: threadID, turnID: parsed.id))
    }

    private func handleItemStartedNotification(_ params: [String: Any]?) {
        guard let envelope = validatedItemEnvelope(params, timestampKey: "startedAtMs") else {
            stopForProtocolFailure("Codex emitted an invalid item-started notification")
            return
        }
        guard envelope.type == "agentMessage" else { return }
        guard let message = validatedAgentMessage(envelope.item) else {
            stopForProtocolFailure("Codex emitted an invalid agent-message item")
            return
        }
        let identity = AgentMessageIdentity(
            threadID: envelope.threadID,
            turnID: envelope.turnID,
            phase: message.phase
        )
        guard !completedAgentMessageIDs.contains(message.id) else {
            stopForProtocolFailure("Codex restarted a completed agent message")
            return
        }
        if let existing = agentMessageIdentities[message.id] {
            guard existing == identity else {
                stopForProtocolFailure("Codex reused an agent-message identifier")
                return
            }
        } else {
            guard agentMessageIdentities.values.count(where: {
                $0.threadID == envelope.threadID && $0.turnID == envelope.turnID
            }) < Self.maximumAgentMessagesPerTurn else {
                stopForProtocolFailure("Codex exceeded the agent-message safety bound")
                return
            }
            agentMessageIdentities[message.id] = identity
        }
    }

    private func handleAgentMessageDeltaNotification(_ params: [String: Any]?) {
        guard let params,
              Set(params.keys) == ["delta", "itemId", "threadId", "turnId"],
              let threadID = params["threadId"] as? String,
              let turnID = params["turnId"] as? String,
              let itemID = params["itemId"] as? String,
              isValidOpaqueIdentifier(itemID),
              activeConversationTurns[threadID] == turnID,
              let identity = agentMessageIdentities[itemID],
              identity.threadID == threadID,
              identity.turnID == turnID,
              !completedAgentMessageIDs.contains(itemID),
              let delta = params["delta"] as? String,
              delta.utf8.count <= Self.maximumAgentDeltaBytes else {
            stopForProtocolFailure("Codex emitted an invalid agent-message delta")
            return
        }
        emitConversationEvent(.agentMessageDelta(
            threadID: threadID,
            turnID: turnID,
            itemID: itemID,
            phase: identity.phase,
            delta: delta
        ))
    }

    private func handleItemCompletedNotification(_ params: [String: Any]?) {
        guard let envelope = validatedItemEnvelope(params, timestampKey: "completedAtMs") else {
            stopForProtocolFailure("Codex emitted an invalid item-completed notification")
            return
        }
        guard envelope.type == "agentMessage" else { return }
        guard let message = validatedAgentMessage(envelope.item) else {
            stopForProtocolFailure("Codex emitted an invalid completed agent message")
            return
        }
        let identity = AgentMessageIdentity(
            threadID: envelope.threadID,
            turnID: envelope.turnID,
            phase: message.phase
        )
        guard !completedAgentMessageIDs.contains(message.id) else {
            stopForProtocolFailure("Codex completed an agent message more than once")
            return
        }
        if let existing = agentMessageIdentities[message.id] {
            guard existing.threadID == identity.threadID,
                  existing.turnID == identity.turnID else {
                stopForProtocolFailure("Codex completed an agent message on the wrong turn")
                return
            }
        } else {
            guard agentMessageIdentities.values.count(where: {
                $0.threadID == envelope.threadID && $0.turnID == envelope.turnID
            }) < Self.maximumAgentMessagesPerTurn else {
                stopForProtocolFailure("Codex exceeded the agent-message safety bound")
                return
            }
        }
        agentMessageIdentities[message.id] = identity
        completedAgentMessageIDs.insert(message.id)
        emitConversationEvent(.agentMessageCompleted(
            threadID: envelope.threadID,
            turnID: envelope.turnID,
            itemID: message.id,
            phase: message.phase,
            text: message.text
        ))
    }

    private func handleTurnCompletedNotification(_ params: [String: Any]?) {
        guard let params,
              Set(params.keys) == ["threadId", "turn"],
              let threadID = params["threadId"] as? String,
              knownConversationThreadIDs.contains(threadID),
              let turn = params["turn"] as? [String: Any],
              let parsed = validatedTurn(turn),
              activeConversationTurns[threadID] == parsed.id else {
            stopForProtocolFailure("Codex emitted an invalid turn-completed notification")
            return
        }
        let outcome: CodexConversationTurnOutcome
        switch parsed.status {
        case .completed:
            outcome = .completed
        case .interrupted:
            outcome = .interrupted
        case let .failed(message):
            outcome = .failed(message)
        case .inProgress:
            stopForProtocolFailure("Codex completed a turn with an in-progress status")
            return
        }
        activeConversationTurns.removeValue(forKey: threadID)
        agentMessageIdentities = agentMessageIdentities.filter {
            $0.value.threadID != threadID || $0.value.turnID != parsed.id
        }
        completedAgentMessageIDs = completedAgentMessageIDs.filter {
            agentMessageIdentities[$0] != nil
        }
        emitConversationEvent(.turnCompleted(
            threadID: threadID,
            turnID: parsed.id,
            outcome: outcome
        ))
    }

    private func validatedConversationThreadID(
        _ thread: [String: Any],
        requireEphemeral: Bool
    ) -> String? {
        let allowedKeys: Set<String> = [
            "agentNickname", "agentRole", "cliVersion", "createdAt", "cwd", "ephemeral",
            "forkedFromId", "gitInfo", "id", "modelProvider", "name", "parentThreadId",
            "path", "preview", "projectId", "recencyAt", "section", "sectionEnteredAt",
            "sessionId", "source", "status", "threadSource", "turns", "updatedAt",
        ]
        guard Set(thread.keys).isSubset(of: allowedKeys),
              let id = thread["id"] as? String,
              isValidOpaqueIdentifier(id),
              let sessionID = thread["sessionId"] as? String,
              sessionID == id,
              thread["modelProvider"] as? String == "openai",
              let ephemeral = thread["ephemeral"] as? Bool,
              !requireEphemeral || ephemeral,
              let cliVersion = thread["cliVersion"] as? String,
              !cliVersion.isEmpty,
              cliVersion.utf8.count <= 128,
              let runtime,
              let cwd = thread["cwd"] as? String,
              URL(fileURLWithPath: cwd).standardizedFileURL
                == runtime.codexHome.standardizedFileURL,
              let createdAt = exactSignedInteger(thread["createdAt"]),
              createdAt >= 0,
              let updatedAt = exactSignedInteger(thread["updatedAt"]),
              updatedAt >= 0,
              let preview = thread["preview"] as? String,
              preview.utf8.count <= 8 * 1_024,
              thread.keys.contains("projectId"),
              isNullOrBoundedString(thread["projectId"], maximumBytes: 128),
              let turns = thread["turns"] as? [Any],
              turns.isEmpty,
              isValidThreadSource(thread["source"]),
              isValidThreadStatus(thread["status"]),
              isNullOrBoundedString(thread["path"], maximumBytes: 4 * 1_024),
              !requireEphemeral || thread["path"] == nil || thread["path"] is NSNull else {
            return nil
        }
        return id
    }

    private func validatedTurn(
        _ turn: [String: Any]
    ) -> (id: String, status: ParsedTurnStatus)? {
        let allowedKeys: Set<String> = [
            "completedAt", "durationMs", "error", "id", "items", "itemsView", "startedAt",
            "status",
        ]
        guard Set(turn.keys).isSubset(of: allowedKeys),
              let id = turn["id"] as? String,
              isValidOpaqueIdentifier(id),
              let items = turn["items"] as? [Any],
              items.count <= 2_048,
              turn["itemsView"] == nil
                || (turn["itemsView"] as? String).map({
                    ["full", "notLoaded", "summary"].contains($0)
                }) == true,
              isNullOrNonnegativeInteger(turn["startedAt"]),
              isNullOrNonnegativeInteger(turn["completedAt"]),
              isNullOrNonnegativeInteger(turn["durationMs"]),
              let rawStatus = turn["status"] as? String else { return nil }

        let status: ParsedTurnStatus
        switch rawStatus {
        case "inProgress":
            guard turn["error"] == nil || turn["error"] is NSNull else { return nil }
            status = .inProgress
        case "completed":
            guard turn["error"] == nil || turn["error"] is NSNull else { return nil }
            status = .completed
        case "interrupted":
            guard turn["error"] == nil || turn["error"] is NSNull else { return nil }
            status = .interrupted
        case "failed":
            guard let error = turn["error"] as? [String: Any],
                  Set(error.keys).isSubset(of: ["additionalDetails", "codexErrorInfo", "message"]),
                  let message = error["message"] as? String,
                  !message.isEmpty,
                  message.utf8.count <= 2_048 else { return nil }
            status = .failed(message)
        default:
            return nil
        }
        return (id, status)
    }

    private func validatedItemEnvelope(
        _ params: [String: Any]?,
        timestampKey: String
    ) -> (threadID: String, turnID: String, type: String, item: [String: Any])? {
        guard let params,
              Set(params.keys) == ["item", timestampKey, "threadId", "turnId"],
              let timestamp = exactSignedInteger(params[timestampKey]),
              timestamp >= 0,
              let threadID = params["threadId"] as? String,
              let turnID = params["turnId"] as? String,
              knownConversationThreadIDs.contains(threadID),
              activeConversationTurns[threadID] == turnID,
              let item = params["item"] as? [String: Any],
              let itemID = item["id"] as? String,
              isValidOpaqueIdentifier(itemID),
              let type = item["type"] as? String,
              !type.isEmpty,
              type.utf8.count <= 64 else { return nil }
        return (threadID, turnID, type, item)
    }

    private func validatedAgentMessage(
        _ item: [String: Any]
    ) -> (id: String, phase: CodexAgentMessagePhase, text: String)? {
        let allowedKeys: Set<String> = [
            "delivery", "id", "memoryCitation", "phase", "text", "type",
        ]
        guard Set(item.keys).isSubset(of: allowedKeys),
              item["type"] as? String == "agentMessage",
              let id = item["id"] as? String,
              isValidOpaqueIdentifier(id),
              let text = item["text"] as? String,
              text.utf8.count <= Self.maximumAgentMessageBytes,
              item["delivery"] == nil || item["delivery"] is NSNull
                || item["delivery"] as? String == "async",
              item["memoryCitation"] == nil || item["memoryCitation"] is NSNull
                || item["memoryCitation"] is [String: Any] else { return nil }

        let phase: CodexAgentMessagePhase
        if item["phase"] == nil || item["phase"] is NSNull {
            phase = .unspecified
        } else {
            switch item["phase"] as? String {
            case "commentary": phase = .commentary
            case "final_answer": phase = .finalAnswer
            default: return nil
            }
        }
        return (id, phase, text)
    }

    private func isReadOnlySandbox(_ value: Any?) -> Bool {
        guard let sandbox = value as? [String: Any],
              Set(sandbox.keys).isSubset(of: ["networkAccess", "type"]),
              sandbox["type"] as? String == "readOnly" else { return false }
        return sandbox["networkAccess"] == nil || sandbox["networkAccess"] as? Bool == false
    }

    private func areEmptyInstructionSources(_ value: Any?) -> Bool {
        guard value != nil else { return true }
        guard let sources = value as? [Any] else { return false }
        return sources.isEmpty
    }

    private func isNullOrBoundedString(_ value: Any?, maximumBytes: Int) -> Bool {
        if value == nil || value is NSNull { return true }
        guard let value = value as? String else { return false }
        return value.utf8.count <= maximumBytes
    }

    private func isNullOrNonnegativeInteger(_ value: Any?) -> Bool {
        if value == nil || value is NSNull { return true }
        return exactSignedInteger(value).map { $0 >= 0 } == true
    }

    private func isValidThreadSource(_ value: Any?) -> Bool {
        if let source = value as? String {
            return ["appServer", "unknown"].contains(source)
        }
        guard let source = value as? [String: Any],
              Set(source.keys) == ["custom"],
              let custom = source["custom"] as? String else { return false }
        return !custom.isEmpty && custom.utf8.count <= 128
    }

    private func isValidThreadStatus(_ value: Any?) -> Bool {
        guard let status = value as? [String: Any],
              let type = status["type"] as? String else { return false }
        switch type {
        case "notLoaded", "idle", "systemError":
            return Set(status.keys) == ["type"]
        case "active":
            return Set(status.keys) == ["activeFlags", "type"]
                && status["activeFlags"] is [Any]
        default:
            return false
        }
    }

    private func updateAccount(from result: [String: Any]) {
        guard let requiresOpenAIAuth = result["requiresOpenaiAuth"] as? Bool else {
            stopForProtocolFailure("Codex returned an invalid account response")
            return
        }
        guard let rawAccount = result["account"], !(rawAccount is NSNull) else {
            deviceCode = nil
            verificationURL = nil
            invalidateConversationState(message: "ChatGPT sign-in is required")
            state = .signedOut
            beginQueuedDeviceCodeSignInIfReady()
            return
        }
        guard let account = rawAccount as? [String: Any],
              account["type"] as? String == "chatgpt",
              requiresOpenAIAuth else {
            stopForProtocolFailure(Self.managedLoginMessage)
            return
        }
        guard account.keys.contains("email") else {
            stopForProtocolFailure("Codex returned invalid managed-account metadata")
            return
        }
        let email: String?
        if account["email"] is NSNull {
            email = nil
        } else if let value = account["email"] as? String,
                  !value.isEmpty,
                  value.utf8.count <= 320 {
            email = value
        } else {
            stopForProtocolFailure("Codex returned invalid managed-account metadata")
            return
        }
        guard
              let plan = account["planType"] as? String,
              Self.allowedPlanTypes.contains(plan) else {
            stopForProtocolFailure("Codex returned invalid managed-account metadata")
            return
        }
        deviceCode = nil
        verificationURL = nil
        queuedDeviceCodeSignIn = false
        state = .signedIn(email: email, plan: plan)
    }

    private func requestTimedOut(id: Int) {
        guard let kind = pending.removeValue(forKey: id) else { return }
        requestTimeouts.removeValue(forKey: id)?.cancel()
        if let completion = conversationRequestCompletions.removeValue(forKey: id) {
            resume(
                completion,
                throwing: CodexConversationClientError.connectionClosed(
                    "\(kind.failureDescription): Codex App Server did not respond in time"
                )
            )
        }
        stopForProtocolFailure("Codex App Server did not respond in time")
    }

    private func isValidRPCError(_ value: Any?) -> Bool {
        guard let error = value as? [String: Any],
              let code = exactSignedInteger(error["code"]),
              code != 0,
              let message = error["message"] as? String,
              !message.isEmpty,
              message.utf8.count <= 2_048 else { return false }
        let allowedKeys: Set<String> = ["code", "message", "data"]
        return Set(error.keys).isSubset(of: allowedKeys)
    }

    private func isValidLoginCompletionError(_ value: Any?, success: Bool) -> Bool {
        if value == nil || value is NSNull { return true }
        guard !success,
              let message = value as? String,
              !message.isEmpty,
              message.utf8.count <= 2_048 else { return false }
        return true
    }

    private func isAllowedAccountUpdate(_ params: [String: Any]) -> Bool {
        let allowedKeys: Set<String> = ["authMode", "planType"]
        guard Set(params.keys).isSubset(of: allowedKeys) else { return false }
        if let authMode = params["authMode"], !(authMode is NSNull) {
            guard authMode as? String == "chatgpt" else { return false }
        }
        if let plan = params["planType"], !(plan is NSNull) {
            guard let plan = plan as? String, Self.allowedPlanTypes.contains(plan) else {
                return false
            }
        }
        return true
    }

    private func resetProtocolState(keepingQueuedSignIn: Bool) {
        failAllConversationRequests(
            with: CodexConversationClientError.connectionClosed("Codex connection was reset")
        )
        deviceCode = nil
        verificationURL = nil
        receiveBuffer.removeAll(keepingCapacity: false)
        pending.removeAll()
        for task in requestTimeouts.values { task.cancel() }
        requestTimeouts.removeAll()
        pendingLoginID = nil
        retiredLoginIDs.removeAll()
        knownConversationThreadIDs.removeAll()
        activeConversationTurns.removeAll()
        agentMessageIdentities.removeAll()
        completedAgentMessageIDs.removeAll()
        isInitialized = false
        cancelRequested = false
        if !keepingQueuedSignIn { queuedDeviceCodeSignIn = false }
    }

    private func invalidateConversationState(message: String) {
        let requestIDs = Array(conversationRequestCompletions.keys)
        for id in requestIDs {
            pending.removeValue(forKey: id)
            requestTimeouts.removeValue(forKey: id)?.cancel()
        }
        failAllConversationRequests(
            with: CodexConversationClientError.connectionClosed(message)
        )
        let hadConversation = !knownConversationThreadIDs.isEmpty
            || !activeConversationTurns.isEmpty
            || !agentMessageIdentities.isEmpty
            || !completedAgentMessageIDs.isEmpty
        knownConversationThreadIDs.removeAll()
        activeConversationTurns.removeAll()
        agentMessageIdentities.removeAll()
        completedAgentMessageIDs.removeAll()
        if hadConversation {
            emitConversationEvent(.connectionClosed(message))
        }
    }

    private func failAllConversationRequests(with error: CodexConversationClientError) {
        let completions = Array(conversationRequestCompletions.values)
        conversationRequestCompletions.removeAll()
        for completion in completions {
            resume(completion, throwing: error)
        }
    }

    private func resume(
        _ completion: ConversationRequestCompletion,
        throwing error: CodexConversationClientError
    ) {
        switch completion {
        case let .thread(continuation): continuation.resume(throwing: error)
        case let .turn(continuation): continuation.resume(throwing: error)
        case let .interrupt(continuation): continuation.resume(throwing: error)
        }
    }

    private func emitConversationEvent(_ event: CodexConversationEvent) {
        var terminatedSubscriberIDs: [UUID] = []
        var didDrop = false
        for (subscriberID, continuation) in conversationEventContinuations {
            switch continuation.yield(event) {
            case .enqueued:
                break
            case .dropped:
                didDrop = true
                continuation.finish()
                terminatedSubscriberIDs.append(subscriberID)
            case .terminated:
                terminatedSubscriberIDs.append(subscriberID)
            @unknown default:
                didDrop = true
                continuation.finish()
                terminatedSubscriberIDs.append(subscriberID)
            }
        }
        for subscriberID in terminatedSubscriberIDs {
            conversationEventContinuations.removeValue(forKey: subscriberID)
        }
        if didDrop, runtime != nil {
            stopForProtocolFailure("The Codex conversation event consumer fell behind")
        }
    }

    private func stopForProtocolFailure(_ message: String) {
        guard let runtime else {
            if !isStopping {
                resetProtocolState(keepingQueuedSignIn: false)
                state = .unavailable(message)
            }
            return
        }
        deviceCode = nil
        verificationURL = nil
        queuedDeviceCodeSignIn = false
        cancelRequested = false
        isInitialized = false
        state = .unavailable(message)
        invalidateConversationState(message: message)
        pending.removeAll()
        for task in requestTimeouts.values { task.cancel() }
        requestTimeouts.removeAll()
        runtime.output.readabilityHandler = nil
        guard !isStopping else { return }
        isStopping = true
        terminate(runtime)
    }

    private func terminate(_ runtime: CodexRuntimeSession) {
        if !runtime.process.isRunning {
            handleTermination(exitCode: runtime.process.terminationStatus)
            return
        }
        runtime.process.terminate()
        let process = runtime.process
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(2))
            guard let self, self.runtime === runtime, process.isRunning else { return }
            kill(process.processIdentifier, SIGKILL)
        }
    }

    private func handleTermination(exitCode: Int32? = nil) {
        guard let runtime else { return }
        let wasShuttingDown = isShuttingDown
        let shouldRestart = wasShuttingDown && restartWhenStopped
        restartWhenStopped = false
        if !wasShuttingDown {
            let detail = exitCode.map { " (exit \($0))" } ?? ""
            invalidateConversationState(message: "Codex App Server stopped\(detail)")
        }
        runtime.output.readabilityHandler = nil
        self.runtime = nil
        isStopping = false
        isShuttingDown = false
        resetProtocolState(keepingQueuedSignIn: false)
        runtime.cleanUpAfterTermination()
        if wasShuttingDown {
            state = .stopped
            if shouldRestart {
                startIfNeeded()
            }
            return
        }
        if case .unavailable = state { return }
        let detail = exitCode.map { " (exit \($0))" } ?? ""
        Self.logger.error("Codex App Server stopped\(detail, privacy: .public)")
        state = .unavailable("Codex App Server stopped\(detail)")
    }

    private func exactInteger(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let integer = number.int64Value
        guard integer >= 0,
              integer <= Int64(Self.maximumRequestID),
              number.doubleValue == Double(integer) else { return nil }
        return Int(integer)
    }

    private func exactSignedInteger(_ value: Any?) -> Int64? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let integer = number.int64Value
        guard number.doubleValue == Double(integer) else { return nil }
        return integer
    }

    private func isValidOpaqueIdentifier(_ value: String) -> Bool {
        guard (1...128).contains(value.utf8.count) else { return false }
        return value.utf8.allSatisfy {
            $0.isASCIIAlphaNumeric || $0 == 45 || $0 == 46 || $0 == 58 || $0 == 95
        }
    }

    private func incomingRequestID(_ value: Any?) -> IncomingRequestID? {
        if let value = value as? String,
           (1...128).contains(value.utf8.count),
           !value.unicodeScalars.contains(where: { $0.value < 32 }) {
            return .string(value)
        }
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let integer = number.int64Value
        guard integer >= -Int64(Self.maximumRequestID),
              integer <= Int64(Self.maximumRequestID),
              number.doubleValue == Double(integer) else { return nil }
        return .integer(integer)
    }

    private func isAllowedVerificationURL(_ url: URL) -> Bool {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            return false
        }
        return components.scheme == "https"
            && components.host?.lowercased() == "auth.openai.com"
            && (components.port == nil || components.port == 443)
            && components.user == nil
            && components.password == nil
            && components.path == "/codex/device"
            && components.query == nil
            && components.fragment == nil
    }
}

private enum IncomingRequestID {
    case integer(Int64)
    case string(String)

    var jsonValue: Any {
        switch self {
        case let .integer(value): value
        case let .string(value): value
        }
    }
}

private enum CodexClientProtocolError: Error {
    case messageTooLarge
}

private extension UInt8 {
    var isASCIIAlphaNumeric: Bool {
        (48...57).contains(self) || (65...90).contains(self) || (97...122).contains(self)
    }
}
