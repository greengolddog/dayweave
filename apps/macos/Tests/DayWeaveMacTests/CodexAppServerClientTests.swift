import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@MainActor
@Suite("Codex runtime safety")
struct CodexAppServerClientTests {
    @Test("development builds fail closed instead of launching ambient Codex")
    func testDevelopmentBuildFailsClosedWithoutLaunchingAmbientCodex() {
        let client = CodexAppServerClient()

        client.startIfNeeded()

        #expect(client.state == .unavailable(CodexAppServerClient.runtimeUnavailableMessage))
        #expect(client.deviceCode == nil)
        #expect(client.verificationURL == nil)
    }

    @Test("login actions cannot bypass the disabled runtime gate")
    func testLoginActionsCannotBypassDisabledRuntimeGate() {
        let client = CodexAppServerClient()

        client.signInWithBrowser()
        #expect(client.state == .unavailable("Codex App Server is not running"))

        client.signInWithDeviceCode()
        #expect(client.state == .unavailable("Codex App Server is not running"))

        client.signInWithAPIKey("must-not-be-used")
        #expect(client.state == .unavailable("Codex App Server is not running"))
    }
}
#endif
