import AppKit
import Darwin
import Foundation
import os

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

        var failureDescription: String {
            switch self {
            case .initialize: "Codex initialization was rejected"
            case .account: "Codex account state could not be read"
            case .deviceCodeLogin: "ChatGPT sign-in could not be started"
            case .cancelLogin: "ChatGPT sign-in could not be canceled"
            case .logout: "Codex sign-out could not be completed"
            }
        }

        var timeoutSeconds: UInt64 {
            switch self {
            case .initialize, .account, .cancelLogin, .logout: 10
            case .deviceCodeLogin: 20
            }
        }
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
    private var pendingLoginID: String?
    private var retiredLoginIDs: Set<String> = []
    private var isInitialized = false
    private var queuedDeviceCodeSignIn = false
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

    func startIfNeeded() {
        guard runtime == nil, !isStopping else { return }
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

    private func send(method: String, params: [String: Any]?, kind: RequestKind) -> Bool {
        guard let runtime else {
            state = .unavailable("Codex App Server is not running")
            return false
        }
        guard pending.count < Self.maximumPendingRequests,
              nextRequestID <= Self.maximumRequestID else {
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
            guard runtime != nil else { return }
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
            state = .signedOut
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
        default:
            break
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
        guard pending.removeValue(forKey: id) != nil else { return }
        requestTimeouts.removeValue(forKey: id)?.cancel()
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
        deviceCode = nil
        verificationURL = nil
        receiveBuffer.removeAll(keepingCapacity: false)
        pending.removeAll()
        for task in requestTimeouts.values { task.cancel() }
        requestTimeouts.removeAll()
        pendingLoginID = nil
        retiredLoginIDs.removeAll()
        isInitialized = false
        cancelRequested = false
        if !keepingQueuedSignIn { queuedDeviceCodeSignIn = false }
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
        runtime.output.readabilityHandler = nil
        self.runtime = nil
        isStopping = false
        isShuttingDown = false
        resetProtocolState(keepingQueuedSignIn: false)
        runtime.cleanUpAfterTermination()
        if wasShuttingDown {
            state = .stopped
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
