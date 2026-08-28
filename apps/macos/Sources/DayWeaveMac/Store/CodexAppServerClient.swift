import AppKit
import Foundation
import os

@MainActor
final class CodexAppServerClient: ObservableObject {
    private static let logger = Logger(subsystem: "com.greengolddog.dayweave", category: "Codex")
    enum ConnectionState: Equatable {
        case stopped
        case starting
        case signedOut
        case signingIn
        case signedIn(email: String?, plan: String)
        case unavailable(String)

        var title: String {
            switch self {
            case .stopped: "Not started"
            case .starting: "Connecting…"
            case .signedOut: "Sign in required"
            case .signingIn: "Finish sign-in in your browser"
            case let .signedIn(_, plan): "Connected · \(plan.capitalized)"
            case .unavailable: "Unavailable"
            }
        }

        var isConnected: Bool {
            if case .signedIn = self { return true }
            return false
        }
    }

    private enum RequestKind {
        case initialize
        case account
        case browserLogin
        case deviceCodeLogin
        case apiKeyLogin
        case logout
    }

    @Published private(set) var state: ConnectionState = .stopped
    @Published private(set) var deviceCode: String?
    @Published private(set) var verificationURL: URL?

    private var process: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var receiveBuffer = Data()
    private var nextRequestID = 1
    private var pending: [Int: RequestKind] = [:]

    func startIfNeeded() {
        guard process == nil else { return }
        state = .starting

        let process = Process()
        let inputPipe = Pipe()
        let outputPipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["codex", "app-server", "--stdio"]
        process.standardInput = inputPipe
        process.standardOutput = outputPipe
        process.standardError = FileHandle.nullDevice

        var environment = ProcessInfo.processInfo.environment
        let inheritedPath = environment["PATH"] ?? "/usr/bin:/bin"
        environment["PATH"] = "/opt/homebrew/bin:/usr/local/bin:\(inheritedPath)"
        process.environment = environment

        let output = outputPipe.fileHandleForReading
        output.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            Task { @MainActor [weak self] in
                guard let self else { return }
                if data.isEmpty {
                    self.handleTermination()
                } else {
                    self.receive(data)
                }
            }
        }
        process.terminationHandler = { [weak self] process in
            Task { @MainActor [weak self] in
                self?.handleTermination(exitCode: process.terminationStatus)
            }
        }

        do {
            try process.run()
            Self.logger.info("Codex App Server process launched")
            self.process = process
            input = inputPipe.fileHandleForWriting
            self.output = output
            send(
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
            Self.logger.error("Codex App Server launch failed: \(error.localizedDescription, privacy: .public)")
            output.readabilityHandler = nil
            state = .unavailable("Codex CLI could not be started")
        }
    }

    func signInWithBrowser() {
        startIfNeeded()
        deviceCode = nil
        verificationURL = nil
        state = .signingIn
        send(
            method: "account/login/start",
            params: [
                "type": "chatgpt",
                "appBrand": "chatgpt",
                "codexStreamlinedLogin": true,
                "useHostedLoginSuccessPage": true,
            ],
            kind: .browserLogin
        )
    }

    func signInWithDeviceCode() {
        startIfNeeded()
        state = .signingIn
        send(
            method: "account/login/start",
            params: ["type": "chatgptDeviceCode"],
            kind: .deviceCodeLogin
        )
    }

    func signInWithAPIKey(_ key: String) {
        let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        startIfNeeded()
        state = .signingIn
        send(
            method: "account/login/start",
            params: ["type": "apiKey", "apiKey": trimmed],
            kind: .apiKeyLogin
        )
    }

    func signOut() {
        guard process != nil else { return }
        send(method: "account/logout", params: nil, kind: .logout)
    }

    func refreshAccount() {
        guard process != nil else {
            startIfNeeded()
            return
        }
        send(
            method: "account/read",
            params: ["refreshToken": false],
            kind: .account
        )
    }

    private func send(method: String, params: [String: Any]?, kind: RequestKind) {
        guard let input else {
            if process == nil { state = .unavailable("Codex App Server is not running") }
            return
        }
        let id = nextRequestID
        nextRequestID += 1
        var payload: [String: Any] = ["id": id, "method": method]
        if let params { payload["params"] = params }

        do {
            var data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
            data.append(0x0A)
            try input.write(contentsOf: data)
            pending[id] = kind
        } catch {
            state = .unavailable("Could not communicate with Codex App Server")
        }
    }

    private func sendNotification(method: String) {
        guard let input else { return }
        do {
            var data = try JSONSerialization.data(
                withJSONObject: ["method": method],
                options: [.sortedKeys]
            )
            data.append(0x0A)
            try input.write(contentsOf: data)
        } catch {
            state = .unavailable("Could not finish Codex initialization")
        }
    }

    private func receive(_ data: Data) {
        receiveBuffer.append(data)
        while let newline = receiveBuffer.firstIndex(of: 0x0A) {
            let line = receiveBuffer[..<newline]
            receiveBuffer.removeSubrange(...newline)
            guard !line.isEmpty,
                  let object = try? JSONSerialization.jsonObject(with: Data(line)),
                  let message = object as? [String: Any]
            else { continue }
            handle(message)
        }
    }

    private func handle(_ message: [String: Any]) {
        if let method = message["method"] as? String {
            handleNotification(method: method, params: message["params"] as? [String: Any])
            return
        }
        guard let id = message["id"] as? Int, let kind = pending.removeValue(forKey: id) else {
            return
        }
        if let error = message["error"] as? [String: Any] {
            let text = error["message"] as? String ?? "Codex returned an error"
            state = .unavailable(text)
            return
        }
        let result = message["result"] as? [String: Any] ?? [:]

        switch kind {
        case .initialize:
            sendNotification(method: "initialized")
            refreshAccount()
        case .account:
            updateAccount(from: result)
        case .browserLogin:
            if let value = result["authUrl"] as? String, let url = URL(string: value) {
                NSWorkspace.shared.open(url)
                state = .signingIn
            } else {
                state = .unavailable("Codex did not return a login URL")
            }
        case .deviceCodeLogin:
            deviceCode = result["userCode"] as? String
            verificationURL = (result["verificationUrl"] as? String).flatMap(URL.init(string:))
            if let verificationURL { NSWorkspace.shared.open(verificationURL) }
            state = .signingIn
        case .apiKeyLogin:
            refreshAccount()
        case .logout:
            deviceCode = nil
            verificationURL = nil
            state = .signedOut
        }
    }

    private func handleNotification(method: String, params: [String: Any]?) {
        switch method {
        case "account/login/completed":
            if params?["success"] as? Bool == true {
                refreshAccount()
            } else {
                state = .signedOut
            }
        case "account/updated":
            refreshAccount()
        default:
            break
        }
    }

    private func updateAccount(from result: [String: Any]) {
        guard let account = result["account"] as? [String: Any] else {
            state = .signedOut
            return
        }
        let type = account["type"] as? String
        switch type {
        case "chatgpt":
            state = .signedIn(
                email: account["email"] as? String,
                plan: account["planType"] as? String ?? "ChatGPT"
            )
        case "apiKey":
            state = .signedIn(email: nil, plan: "API key")
        default:
            state = .unavailable("Unsupported Codex account mode")
        }
    }

    private func handleTermination(exitCode: Int32? = nil) {
        output?.readabilityHandler = nil
        process = nil
        input = nil
        output = nil
        pending.removeAll()
        if case .stopped = state { return }
        let detail = exitCode.map { " (exit \($0))" } ?? ""
        Self.logger.error("Codex App Server stopped\(detail, privacy: .public)")
        state = .unavailable("Codex App Server stopped\(detail)")
    }
}
