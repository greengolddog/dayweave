import Darwin
import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Local scheduler helper boundary", .serialized)
struct SchedulerHelperClientTests {
    @Test("compose redacts notes and preserves the strict canonical projection")
    func composeRedactsNotesAndPreservesWireFields() async throws {
        let fixture = try HelperBundleFixture()
        defer { fixture.cleanUp() }
        let itemID = UUID(uuidString: "00000000-0000-4000-8000-000000000001")!
        let secret = "PRIVATE-NOTES-MUST-NOT-CROSS"
        let item = try makeItem(id: itemID, notes: secret)
        let runner = RecordingSchedulerHelperRunner(
            result: .init(
                standardOutput: compositionResponse(itemID: itemID),
                standardError: Data(),
                termination: .exited(0)
            )
        )
        let client = SchedulerHelperClient(
            testingLocator: FixedSchedulerHelperLocator(location: fixture.location),
            processRunner: runner,
            signatureValidator: TestSchedulerHelperSignatureValidator()
        )

        let composition = try await client.compose(
            canonicalItems: [item],
            schedule: scheduleRequest()
        )

        #expect(composition.localInputFingerprint == "local-sha256:" + String(repeating: "a", count: 64))
        #expect(composition.sourceItemRevisions == [itemID: 7])
        #expect(composition.plan.blocks.isEmpty)
        let input = try #require(await runner.lastInput())
        #expect(input.range(of: Data(secret.utf8)) == nil)
        let json = try #require(JSONSerialization.jsonObject(with: input) as? [String: Any])
        #expect(json["protocol"] as? String == "dayweave.scheduler.helper")
        #expect(json["version"] as? Int == 1)
        #expect(json["operation"] as? String == "compose")
        let request = try #require(json["request"] as? [String: Any])
        let items = try #require(request["canonical_items"] as? [[String: Any]])
        let projected = try #require(items.first)
        #expect(projected["notes"] is NSNull)
        #expect(projected["revision"] as? Int == 7)
        #expect(projected["kind"] as? String == "task")
        #expect(projected["status"] as? String == "planned")
        #expect(projected["created_at"] as? String == "2026-08-30T10:11:12.000000Z")
        #expect(projected["updated_at"] as? String == "2026-08-30T10:11:13.000000Z")
    }

    @Test("unsupported canonical representations are refused before launch")
    func unsupportedCanonicalItemsAreRefused() throws {
        let unsupportedObjects = [
            itemObject(kind: "future_kind"),
            itemObject(status: "future_status"),
            itemObject(splitPolicy: #"{"type":"future_policy"}"#),
            itemObject(extraField: #", "future_field":"retained""#),
            itemObject(revision: 0),
        ]
        for object in unsupportedObjects {
            let item = try decodeItem(object)
            #expect(throws: SchedulerHelperClientError.unsupportedCanonicalItem) {
                try SchedulerHelperCanonicalItemWire(validating: item)
            }
        }
    }

    @Test("server-decoded numeric habit fields remain valid helper input")
    func numericHabitFieldsAreProjected() async throws {
        let fixture = try HelperBundleFixture()
        defer { fixture.cleanUp() }
        let item = try decodeItem(itemObject(
            kind: "habit",
            recurrence: #"{"type":"daily","times_per_day":2}"#,
            flexibleConstraints: #"{"maximum_sessions":3,"minimum_gap_minutes":5}"#
        ))
        #expect(item.hasNonRoundTrippableJSONNumber)
        let runner = RecordingSchedulerHelperRunner(
            result: .init(
                standardOutput: compositionResponse(itemID: item.id),
                standardError: Data(),
                termination: .exited(0)
            )
        )
        let client = testClient(fixture: fixture, runner: runner)

        _ = try await client.compose(canonicalItems: [item], schedule: scheduleRequest())

        let input = try #require(await runner.lastInput())
        let json = try #require(JSONSerialization.jsonObject(with: input) as? [String: Any])
        let request = try #require(json["request"] as? [String: Any])
        let items = try #require(request["canonical_items"] as? [[String: Any]])
        let projected = try #require(items.first)
        let recurrence = try #require(projected["recurrence"] as? [String: Any])
        let constraints = try #require(projected["flexible_constraints"] as? [String: Any])
        #expect(recurrence["times_per_day"] as? Int == 2)
        #expect(constraints["maximum_sessions"] as? Int == 3)
        #expect(constraints["minimum_gap_minutes"] as? Int == 5)
    }

    @Test("one response is required and helper text is never echoed")
    func exactResponseAndNonEchoErrors() async throws {
        let fixture = try HelperBundleFixture()
        defer { fixture.cleanUp() }
        let item = try makeItem()
        let valid = compositionResponse(itemID: item.id)
        let concatenated = valid + valid
        let invalidRunner = RecordingSchedulerHelperRunner(
            result: .init(
                standardOutput: concatenated,
                standardError: Data(),
                termination: .exited(0)
            )
        )
        let invalidClient = testClient(fixture: fixture, runner: invalidRunner)
        do {
            _ = try await invalidClient.compose(
                canonicalItems: [item],
                schedule: scheduleRequest()
            )
            Issue.record("Expected concatenated responses to be rejected")
        } catch let error as SchedulerHelperClientError {
            #expect(error == .invalidResponse)
        }

        let trailingRunner = RecordingSchedulerHelperRunner(
            result: .init(
                standardOutput: valid + Data(" trailing-private-junk".utf8),
                standardError: Data(),
                termination: .exited(0)
            )
        )
        let trailingClient = testClient(fixture: fixture, runner: trailingRunner)
        do {
            _ = try await trailingClient.compose(
                canonicalItems: [item],
                schedule: scheduleRequest()
            )
            Issue.record("Expected trailing non-whitespace to be rejected")
        } catch let error as SchedulerHelperClientError {
            #expect(error == .invalidResponse)
        }

        let privateText = "PRIVATE-HELPER-DIAGNOSTIC"
        let rejectedRunner = RecordingSchedulerHelperRunner(
            result: .init(
                standardOutput: Data("""
                {"protocol":"dayweave.scheduler.helper","version":1,
                 "result":{"type":"error","error":{"code":"invalid_request",
                 "message":"\(privateText)"}}}
                """.utf8),
                standardError: Data(),
                termination: .exited(2)
            )
        )
        let rejectedClient = testClient(fixture: fixture, runner: rejectedRunner)
        do {
            _ = try await rejectedClient.compose(
                canonicalItems: [item],
                schedule: scheduleRequest()
            )
            Issue.record("Expected helper rejection")
        } catch let error as SchedulerHelperClientError {
            #expect(error == .requestRejected)
            #expect(!error.localizedDescription.contains(privateText))
        }
    }

    @Test("executable validation rejects alternate, linked, and nonregular paths")
    func executableValidationFailsClosed() throws {
        let valid = try HelperBundleFixture()
        defer { valid.cleanUp() }
        _ = try SchedulerHelperExecutableValidator.validate(valid.location)
        #expect(throws: SchedulerHelperClientError.invalidCodeSignature) {
            try ProductionSchedulerHelperCodeSignatureValidator().validate(
                executableURL: valid.executableURL,
                hostBundleURL: valid.bundleURL
            )
        }

        let alternate = valid.root.appendingPathComponent("alternate")
        try Data("x".utf8).write(to: alternate)
        #expect(chmod(alternate.path, 0o500) == 0)
        #expect(throws: SchedulerHelperClientError.unsafeExecutable) {
            try SchedulerHelperExecutableValidator.validate(
                .init(bundleURL: valid.bundleURL, executableURL: alternate)
            )
        }

        let linked = try HelperBundleFixture()
        defer { linked.cleanUp() }
        let secondLink = linked.root.appendingPathComponent("second-link")
        #expect(Darwin.link(linked.executableURL.path, secondLink.path) == 0)
        #expect(throws: SchedulerHelperClientError.unsafeExecutable) {
            try SchedulerHelperExecutableValidator.validate(linked.location)
        }

        let symlinked = try HelperBundleFixture()
        defer { symlinked.cleanUp() }
        let target = symlinked.root.appendingPathComponent("target")
        try Data("target".utf8).write(to: target)
        #expect(chmod(target.path, 0o500) == 0)
        try FileManager.default.removeItem(at: symlinked.executableURL)
        try FileManager.default.createSymbolicLink(
            at: symlinked.executableURL,
            withDestinationURL: target
        )
        #expect(throws: SchedulerHelperClientError.unsafeExecutable) {
            try SchedulerHelperExecutableValidator.validate(symlinked.location)
        }

        let nonregular = try HelperBundleFixture()
        defer { nonregular.cleanUp() }
        try FileManager.default.removeItem(at: nonregular.executableURL)
        try FileManager.default.createDirectory(at: nonregular.executableURL, withIntermediateDirectories: false)
        #expect(throws: SchedulerHelperClientError.unsafeExecutable) {
            try SchedulerHelperExecutableValidator.validate(nonregular.location)
        }
    }

    @Test("dedicated workers use no inherited environment and drain both outputs")
    func runnerInvocationAndConcurrentDrain() async throws {
        let fixture = try HelperBundleFixture(script: """
        #!/bin/sh
        if [ "$#" -ne 0 ] || [ "${HOME+x}" = x ]; then
          printf 'unsafe invocation' >&2
          exit 9
        fi
        /bin/dd if=/dev/zero bs=65536 count=4 2>/dev/null
        /bin/dd if=/dev/zero bs=65536 count=4 1>&2 2>/dev/null
        """)
        defer { fixture.cleanUp() }
        let executable = try SchedulerHelperExecutableValidator.validate(fixture.location)

        let result = try await SchedulerHelperProcessRunner().run(
            executable: executable,
            standardInput: Data(repeating: 0x41, count: 1_024 * 1_024),
            timeout: .seconds(5)
        )

        #expect(result.termination == .exited(0))
        #expect(result.standardOutput.count == 4 * 65_536)
        #expect(result.standardError.count == 4 * 65_536)
    }

    @Test("parallel runners complete without cooperative executor capacity")
    func parallelRunnersDoNotDependOnCooperativeExecutorCapacity() async throws {
        let fixture = try HelperBundleFixture(script: """
        #!/bin/sh
        /bin/cat >/dev/null
        printf 'stdout'
        printf 'stderr' >&2
        """)
        defer { fixture.cleanUp() }
        let executable = try SchedulerHelperExecutableValidator.validate(fixture.location)
        let runner = SchedulerHelperProcessRunner()
        let input = Data(repeating: 0x41, count: 256 * 1_024)

        let results = try await withThrowingTaskGroup(
            of: SchedulerHelperProcessResult.self,
            returning: [SchedulerHelperProcessResult].self
        ) { group in
            for _ in 0..<2 {
                group.addTask {
                    try await runner.run(
                        executable: executable,
                        standardInput: input,
                        timeout: .seconds(5)
                    )
                }
            }
            var results: [SchedulerHelperProcessResult] = []
            for try await result in group {
                results.append(result)
            }
            return results
        }

        #expect(results.count == 2)
        #expect(results.allSatisfy { $0.termination == .exited(0) })
        #expect(results.allSatisfy { $0.standardOutput == Data("stdout".utf8) })
        #expect(results.allSatisfy { $0.standardError == Data("stderr".utf8) })
    }

    @Test("early stdin close leaves bounded rejection authoritative")
    func earlyStandardInputClose() async throws {
        let fixture = try HelperBundleFixture(script: """
        #!/bin/sh
        exec 0<&-
        printf '%s\n' '{"protocol":"dayweave.scheduler.helper","version":1,"result":{"type":"error","error":{"code":"invalid_request","message":"Rejected."}}}'
        exit 2
        """)
        defer { fixture.cleanUp() }
        let executable = try SchedulerHelperExecutableValidator.validate(fixture.location)

        let result = try await SchedulerHelperProcessRunner().run(
            executable: executable,
            standardInput: Data(repeating: 0x41, count: 2 * 1_024 * 1_024),
            timeout: .seconds(60)
        )

        #expect(result.termination == .exited(2))
        #expect(result.standardError.isEmpty)
        #expect(result.standardOutput.contains(Data("invalid_request".utf8)))
    }

    @Test("fast-exit output overflow and oversized input remain bounded")
    func processStreamLimits() async throws {
        let fixture = try HelperBundleFixture(script: """
        #!/bin/sh
        /usr/bin/head -c 16777217 /dev/zero
        """)
        defer { fixture.cleanUp() }
        let executable = try SchedulerHelperExecutableValidator.validate(fixture.location)
        let runner = SchedulerHelperProcessRunner(terminationGrace: .milliseconds(40))

        do {
            _ = try await runner.run(
                executable: executable,
                standardInput: Data(),
                timeout: .seconds(60)
            )
            Issue.record("Expected stdout overflow")
        } catch let error as SchedulerHelperClientError {
            #expect(error == .outputTooLarge)
        }

        do {
            _ = try await runner.run(
                executable: executable,
                standardInput: Data(
                    repeating: 0x41,
                    count: SchedulerHelperClient.maximumStandardInputBytes + 1
                ),
                timeout: .seconds(60)
            )
            Issue.record("Expected stdin overflow")
        } catch let error as SchedulerHelperClientError {
            #expect(error == .inputTooLarge)
        }
    }

    @Test("timeout sends TERM then KILL and reaps an uncooperative helper")
    func timeoutTerminatesAndReaps() async throws {
        let fixture = try HelperBundleFixture()
        defer { fixture.cleanUp() }
        try fixture.installUncooperativeScript()
        let executable = try SchedulerHelperExecutableValidator.validate(fixture.location)
        let runner = SchedulerHelperProcessRunner(terminationGrace: .milliseconds(200))

        do {
            let result = try await runner.run(
                executable: executable,
                standardInput: Data(),
                timeout: .seconds(3)
            )
            Issue.record("Expected timeout, got \(result)")
        } catch let error as SchedulerHelperClientError {
            #expect(error == .timedOut)
        }

        try assertTerminatedAndReaped(fixture)
    }

    @Test("cancellation sends TERM then KILL and reaps an uncooperative helper")
    func cancellationTerminatesAndReaps() async throws {
        let fixture = try HelperBundleFixture()
        defer { fixture.cleanUp() }
        try fixture.installUncooperativeScript()
        let executable = try SchedulerHelperExecutableValidator.validate(fixture.location)
        let runner = SchedulerHelperProcessRunner(terminationGrace: .milliseconds(200))
        let task = Task {
            try await runner.run(
                executable: executable,
                standardInput: Data(),
                timeout: .seconds(60)
            )
        }
        try await waitForFile(fixture.pidFile)
        task.cancel()
        do {
            _ = try await task.value
            Issue.record("Expected cancellation")
        } catch is CancellationError {
            // Expected.
        }

        try assertTerminatedAndReaped(fixture)
    }

    private func testClient(
        fixture: HelperBundleFixture,
        runner: RecordingSchedulerHelperRunner
    ) -> SchedulerHelperClient {
        SchedulerHelperClient(
            testingLocator: FixedSchedulerHelperLocator(location: fixture.location),
            processRunner: runner,
            signatureValidator: TestSchedulerHelperSignatureValidator()
        )
    }

    private func makeItem(
        id: UUID = UUID(uuidString: "00000000-0000-4000-8000-000000000001")!,
        notes: String = "notes"
    ) throws -> DayWeaveCanonicalItem {
        try decodeItem(itemObject(id: id, notes: notes))
    }

    private func decodeItem(_ object: String) throws -> DayWeaveCanonicalItem {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(DayWeaveCanonicalItem.self, from: Data(object.utf8))
    }

    private func itemObject(
        id: UUID = UUID(uuidString: "00000000-0000-4000-8000-000000000001")!,
        notes: String = "notes",
        kind: String = "task",
        status: String = "planned",
        splitPolicy: String = #"{"type":"indivisible"}"#,
        recurrence: String = "null",
        flexibleConstraints: String = "{}",
        revision: UInt64 = 7,
        extraField: String = ""
    ) -> String {
        """
        {"id":"\(id.uuidString.lowercased())","is_sensitive":true,
         "kind":"\(kind)","status":"\(status)","title":"Private title",
         "notes":"\(notes)","timezone_name":"UTC","duration_seconds":1800,
         "deadline_at":"2026-08-31T10:00:00Z","earliest_start_at":"2026-08-30T12:00:00Z",
         "recurrence":\(recurrence),"flexible_constraints":\(flexibleConstraints),
         "split_policy":\(splitPolicy),"importance":60,"urgency":70,
         "parent_id":null,"sibling_order":3,"is_executable":true,
         "revision":\(revision),"created_at":"2026-08-30T10:11:12Z",
         "updated_at":"2026-08-30T10:11:13Z","completed_at":null,"deleted_at":null
         \(extraField)}
        """
    }

    private func scheduleRequest() -> DayWeaveSchedulePreviewRequest {
        DayWeaveSchedulePreviewRequest(
            asOf: Date(timeIntervalSince1970: 1_788_083_472),
            horizonStart: Date(timeIntervalSince1970: 1_788_048_000),
            horizonEnd: Date(timeIntervalSince1970: 1_788_134_400),
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
    }

    private func compositionResponse(itemID: UUID) -> Data {
        Data("""
        {"protocol":"dayweave.scheduler.helper","version":1,
         "result":{"type":"composition","composition":{
         "local_input_fingerprint":"local-sha256:\(String(repeating: "a", count: 64))",
         "source_item_count":1,"source_item_revisions":{"\(itemID.uuidString.lowercased())":7},
         "accepted_item_count":1,"rejected_items":[],"ignored_previous_assignments":[],
         "plan":{"as_of":"2026-08-30T10:11:12Z",
         "horizon_start":"2026-08-30T00:00:00Z","horizon_end":"2026-08-31T00:00:00Z",
         "blocks":[],"unscheduled":[],"decisions":[],"violations":[],
         "score":{"scheduled_minutes":0,"unscheduled_minutes":0,
         "soft_penalty":0,"moved_minutes":0},"occurrences":[]}}}}
        """.utf8)
    }

    private func waitForFile(_ url: URL) async throws {
        for _ in 0..<1_000 {
            if FileManager.default.fileExists(atPath: url.path) { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        throw SchedulerHelperTestError.fileDidNotAppear
    }

    private func assertTerminatedAndReaped(_ fixture: HelperBundleFixture) throws {
        let pidText = try String(contentsOf: fixture.pidFile, encoding: .utf8)
        let pid = try #require(pid_t(pidText.trimmingCharacters(in: .whitespacesAndNewlines)))
        #expect(try String(contentsOf: fixture.termFile, encoding: .utf8) == "term")
        errno = 0
        #expect(Darwin.kill(pid, 0) == -1)
        #expect(errno == ESRCH)
    }
}

private struct FixedSchedulerHelperLocator: SchedulerHelperLocating {
    let location: SchedulerHelperLocation
    func locate() throws -> SchedulerHelperLocation { location }
}

private struct TestSchedulerHelperSignatureValidator: SchedulerHelperCodeSignatureValidating {
    func validate(executableURL: URL, hostBundleURL: URL) throws {}
}

private actor RecordingSchedulerHelperRunner: SchedulerHelperProcessRunning {
    private let result: SchedulerHelperProcessResult
    private var inputs: [Data] = []

    init(result: SchedulerHelperProcessResult) {
        self.result = result
    }

    func run(
        executable: ValidatedSchedulerHelperExecutable,
        standardInput: Data,
        timeout: Duration
    ) async throws -> SchedulerHelperProcessResult {
        inputs.append(standardInput)
        return result
    }

    func lastInput() -> Data? { inputs.last }
}

private final class HelperBundleFixture: @unchecked Sendable {
    let root: URL
    let bundleURL: URL
    let executableURL: URL
    let pidFile: URL
    let termFile: URL
    private var cleaned = false

    var location: SchedulerHelperLocation {
        .init(bundleURL: bundleURL, executableURL: executableURL)
    }

    init(script: String = "#!/bin/sh\nexit 0\n") throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "dayweave-helper-test-\(UUID().uuidString.lowercased())",
            isDirectory: true
        )
        bundleURL = root.appendingPathComponent("DayWeave.app", isDirectory: true)
        executableURL = bundleURL
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("Helpers", isDirectory: true)
            .appendingPathComponent("dayweave-scheduler-helper")
        pidFile = root.appendingPathComponent("pid")
        termFile = root.appendingPathComponent("term")
        try FileManager.default.createDirectory(
            at: executableURL.deletingLastPathComponent(),
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        try install(script)
    }

    func installUncooperativeScript() throws {
        try install("""
        #!/usr/bin/python3
        import os
        import signal

        def handle_term(_signal, _frame):
            with open(\(pythonString(termFile.path)), "w", encoding="ascii") as marker:
                marker.write("term")

        signal.signal(signal.SIGTERM, handle_term)
        with open(\(pythonString(pidFile.path)), "w", encoding="ascii") as marker:
            marker.write(str(os.getpid()))
        while True:
            signal.pause()
        """)
    }

    func cleanUp() {
        guard !cleaned else { return }
        cleaned = true
        try? FileManager.default.removeItem(at: root)
    }

    private func install(_ script: String) throws {
        try? FileManager.default.removeItem(at: executableURL)
        try Data(script.utf8).write(to: executableURL, options: .atomic)
        guard chmod(executableURL.path, 0o500) == 0 else {
            throw SchedulerHelperTestError.couldNotInstallExecutable
        }
    }

    private func pythonString(_ value: String) -> String {
        let data = try? JSONSerialization.data(withJSONObject: value, options: .fragmentsAllowed)
        return data.flatMap { String(data: $0, encoding: .utf8) }?
            .replacingOccurrences(of: "\\/", with: "/") ?? "\"\""
    }
}

private enum SchedulerHelperTestError: Error {
    case couldNotInstallExecutable
    case fileDidNotAppear
}
#endif
