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

    private func initializeResult(home: URL) -> [String: Any] {
        [
            "codexHome": home.path,
            "platformFamily": "unix",
            "platformOs": "macos",
            "userAgent": "dayweave-test/0.1.0",
        ]
    }

    private func requestID(_ message: [String: Any]) throws -> Int {
        let number = try #require(message["id"] as? NSNumber)
        return number.intValue
    }

    private func eventually(
        _ predicate: @MainActor () -> Bool,
        attempts: Int = 100
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
