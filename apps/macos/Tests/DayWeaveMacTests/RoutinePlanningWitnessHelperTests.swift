import Darwin
import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Qualified routine helper v2 boundary", .serialized)
@MainActor
struct RoutinePlanningWitnessHelperTests {
    @Test("every producer-qualified case retains exact helper fingerprint and positive or empty lifecycle head")
    func producerParity() throws {
        let cases = try fixtures()
        #expect(!cases.isEmpty)
        for fixture in cases {
            try fixture.witness.validate(canonicalItems: fixture.items)
            let result = try decode(fixture.response, witness: fixture.witness)
            #expect(result.rawOutput == (try bytes(fixture.response)))
            #expect(result.occurrenceSnapshotRevision == fixture.witness.occurrenceLifecycle.snapshotRevision)
            #expect(result.composition.localInputFingerprint == fixture.witness.localInputFingerprint)
            #expect(result.composition.sourceItemRevisions == fixture.witness.sourceItemRevisions)
            #expect(result.composition.rejectedItems.isEmpty)
        }
    }

    @Test("v2 retains exact stdout framing and timestamp strings, not a re-encoded display plan")
    func rawOutputFidelity() throws {
        for fixture in try fixtures() {
            let raw = Data(" \n\t".utf8) + (try bytes(fixture.response)) + Data("\n ".utf8)
            let decoded = try SchedulerHelperClient.decodeOccurrenceOutput(output(raw), witness: fixture.witness)
            #expect(decoded.rawOutput == raw)
            #expect(decoded.composition.localInputFingerprint == fixture.witness.localInputFingerprint)
        }
    }

    @Test("v2 process invocation uses the normalized complete witness without changing v1 or forwarding notes")
    func exactV2Request() async throws {
        for fixture in try fixtures() {
            let bundle = try OccurrenceHelperBundle()
            defer { bundle.cleanUp() }
            let runner = OccurrenceHelperRunner(result: output(try bytes(fixture.response)))
            let signature = OccurrenceHelperSignature()
            let client = SchedulerHelperClient(testingLocator: bundle,
                processRunner: runner, signatureValidator: signature)
            let result = try await client.composeOccurrences(canonicalItems: fixture.items, witness: fixture.witness)
            #expect(result.occurrenceSnapshotRevision == fixture.witness.occurrenceLifecycle.snapshotRevision)
            #expect(signature.calls == 1)
            let request = try object(JSONSerialization.jsonObject(with: #require(await runner.lastInput())))
            #expect(Set(request.keys) == ["protocol", "version", "operation", "request"])
            #expect(request["version"] as? Int == 2)
            #expect(request["operation"] as? String == "compose")
            let body = try object(request["request"])
            #expect(Set(body.keys) == ["canonical_items", "schedule", "occurrence_lifecycle"])
            #expect(try jsonEqual(body["schedule"], fixture.helperRequestBody["schedule"]))
            #expect(try jsonEqual(body["occurrence_lifecycle"], fixture.helperRequestBody["occurrence_lifecycle"]))
            let items = try #require(body["canonical_items"] as? [[String: Any]])
            #expect(items.count == fixture.items.count)
            #expect(items.allSatisfy { $0["notes"] is NSNull })
            #expect(try jsonEqual(normalizeTimestampSpelling(items), normalizeTimestampSpelling(fixture.helperRequestBody["canonical_items"])))
        }
    }

    @Test("v2 refuses version downgrade, malformed scalar kinds, duplicate keys and trailing data")
    func strictEnvelopeAndNumbers() throws {
        let fixture = try #require(fixtures().first)
        let raw = try bytes(fixture.response)
        let text = try #require(String(data: raw, encoding: .utf8))
        let invalidVersions: [Any] = [1, true, 2.0, "2", NSNull()]
        for value in invalidVersions {
            var changed = fixture.response
            changed["version"] = value
            let changedBytes: Data
            if value is Double, !(value is Bool) {
                // JSONSerialization may normalize integral doubles; preserve the raw token.
                changedBytes = Data(text.replacingOccurrences(of: "\"version\":2", with: "\"version\":2.0").utf8)
            } else { changedBytes = try bytes(changed) }
            #expect(throws: SchedulerHelperClientError.invalidResponse) {
                try SchedulerHelperClient.decodeOccurrenceOutput(output(changedBytes), witness: fixture.witness)
            }
        }
        let duplicate = Data(text.replacingOccurrences(of: "\"version\":2", with: "\"version\":2,\"\\u0076ersion\":2").utf8)
        for invalid in [duplicate, raw + raw, raw + Data(" PRIVATE-TRAILING".utf8), Data([0xff])] {
            #expect(throws: SchedulerHelperClientError.invalidResponse) {
                try SchedulerHelperClient.decodeOccurrenceOutput(output(invalid), witness: fixture.witness)
            }
        }
    }

    @Test("helper outputs cannot substitute a different head, fingerprint, source map or schedule")
    func exactWitnessBinding() throws {
        let fixture = try #require(fixtures().first { !$0.witness.sourceItemRevisions.isEmpty })
        let mismatches: [(String, Any)] = [
            ("occurrence_snapshot_revision", fixture.witness.occurrenceLifecycle.snapshotRevision + 1),
            ("occurrence_snapshot_revision", true),
            ("occurrence_snapshot_revision", NSNull()),
            ("local_input_fingerprint", "local-sha256:" + String(repeating: "f", count: 64)),
            ("source_item_revisions", [:] as [String: UInt64]),
            ("source_item_count", 0),
            ("accepted_item_count", 0),
        ]
        for (key, value) in mismatches {
            let changed = try changingComposition(fixture.response) { $0[key] = value }
            #expect(throws: SchedulerHelperClientError.invalidResponse) {
                try decode(changed, witness: fixture.witness)
            }
        }
        let changed = try changingPlan(fixture.response) { $0["as_of"] = "2000-01-01T00:00:00Z" }
        #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(changed, witness: fixture.witness) }
        let missingHead = try changingComposition(fixture.response) { _ = $0.removeValue(forKey: "occurrence_snapshot_revision") }
        #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(missingHead, witness: fixture.witness) }
    }

    @Test("v2 clock binding rejects distinct distant microseconds that Foundation Date collapses")
    func exactDistantMicroseconds() throws {
        let fixture = try #require(fixtures().first { $0.witness.occurrenceLifecycle.instances.isEmpty })
        let clocks = ["as_of": "9999-09-10T09:00:00.000001Z",
            "horizon_start": "9999-09-10T00:00:00.000001Z", "horizon_end": "9999-09-11T00:00:00.000001Z"]
        var wire = try object(JSONSerialization.jsonObject(with: RoutinePlanningWitnessValidation.encode(fixture.witness)))
        var schedule = try object(wire["schedule"])
        for (key, value) in clocks { schedule[key] = value }
        wire["schedule"] = schedule
        let witness = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self, from: bytes(wire))
        let baseline = try changingPlan(fixture.response) { plan in
            for (key, value) in clocks { plan[key] = value }
        }
        // This synthetic envelope isolates the decoder's fixed-clock binding.
        _ = try decode(baseline, witness: witness)
        for (key, value) in clocks {
            let shifted = value.replacingOccurrences(of: ".000001Z", with: ".000002Z")
            let exactDifference = try RoutinePlanningShape.instant(.string(shifted)) - RoutinePlanningShape.instant(.string(value))
            let displayDate = try #require(SchedulerHelperRFC3339.date(from: value))
            let shiftedDisplayDate = try #require(SchedulerHelperRFC3339.date(from: shifted))
            #expect(exactDifference == 1)
            #expect(displayDate == shiftedDisplayDate)
            let changed = try changingPlan(baseline) { $0[key] = shifted }
            #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(changed, witness: witness) }
        }
    }

    @Test("every nested plan object is closed, including JSON-backed decisions and violations")
    func closedNestedPlan() throws {
        let fixture = try #require(fixtures().first)
        let id = "10000000-0000-4000-8000-000000000001"
        let badNested: [(String, Any)] = [
            ("blocks", [["id": id, "is_sensitive": true, "item_id": id, "occurrence_id": NSNull(),
                "external_block_id": NSNull(), "title": "Synthetic", "start": "2026-08-30T00:00:00Z",
                "end": "2026-08-30T01:00:00Z", "session_index": 1, "kind": "planned",
                "explanations": [["code": "earliest_available", "message": "Synthetic", "unknown": true]]]]),
            ("unscheduled", [["item_id": id, "occurrence_id": NSNull(), "remaining": 1,
                "reason": "no_capacity", "message": "Synthetic", "unknown": true]]),
            ("decisions", [["item_id": id, "occurrence_id": NSNull(), "kind": "scheduled",
                "message": "Synthetic", "unknown": true]]),
            ("violations", [["kind": "capacity", "severity": "warning", "item_ids": [], "occurrence_ids": [],
                "start": NSNull(), "end": NSNull(), "penalty": 0, "message": "Synthetic", "unknown": true]]),
            ("score", ["scheduled_minutes": 0, "unscheduled_minutes": 0, "soft_penalty": 0, "moved_minutes": 0, "unknown": 0]),
            ("occurrences", [["id": id, "series_item_id": id, "identity": ["type": "custom", "unknown": true],
                "nominal_start": "2026-08-30T00:00:00Z", "nominal_end": "2026-08-31T00:00:00Z",
                "window_start": "2026-08-30T00:00:00Z", "window_end": "2026-08-31T00:00:00Z",
                "local_date": NSNull(), "ordinal": 1, "state": "generated"]]),
        ]
        for (key, value) in badNested {
            let changed = try changingPlan(fixture.response) { $0[key] = value }
            #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(changed, witness: fixture.witness) }
        }
        let changed = try changingPlan(fixture.response) { $0["unknown"] = true }
        #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(changed, witness: fixture.witness) }
    }

    @Test("malformed JSON-backed values cannot bypass typed display decoding")
    func typedJsonBackedFields() throws {
        let fixture = try #require(fixtures().first)
        let id = "10000000-0000-4000-8000-000000000001"
        let invalidMessages: [Any] = [true, 1, ["private": "not a string"]]
        for message in invalidMessages {
            let changed = try changingPlan(fixture.response) {
                $0["decisions"] = [["item_id": id, "occurrence_id": NSNull(), "kind": "scheduled", "message": message]]
            }
            #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(changed, witness: fixture.witness) }
        }
        let changed = try changingPlan(fixture.response) {
            $0["violations"] = [["kind": "capacity", "severity": "warning", "item_ids": [true], "occurrence_ids": [],
                "start": NSNull(), "end": NSNull(), "penalty": true, "message": "Synthetic"]]
        }
        #expect(throws: SchedulerHelperClientError.invalidResponse) { try decode(changed, witness: fixture.witness) }
    }

    @Test("output and diagnostic bounds, exit codes and private errors stay fail closed")
    func outputCustody() throws {
        let fixture = try #require(fixtures().first)
        let valid = try bytes(fixture.response)
        let rejected = try bytes(["protocol": "dayweave.scheduler.helper", "version": 2,
            "result": ["type": "error", "error": ["code": "invalid_request", "message": "PRIVATE-HELPER-DETAIL"]]])
        #expect(throws: SchedulerHelperClientError.requestRejected) {
            try SchedulerHelperClient.decodeOccurrenceOutput(output(rejected, termination: .exited(2)), witness: fixture.witness)
        }
        #expect(throws: SchedulerHelperClientError.invalidResponse) {
            try SchedulerHelperClient.decodeOccurrenceOutput(output(rejected), witness: fixture.witness)
        }
        #expect(throws: SchedulerHelperClientError.invalidResponse) {
            try SchedulerHelperClient.decodeOccurrenceOutput(output(valid, standardError: Data("PRIVATE-STDERR".utf8)), witness: fixture.witness)
        }
        #expect(throws: SchedulerHelperClientError.unexpectedTermination) {
            try SchedulerHelperClient.decodeOccurrenceOutput(output(valid, termination: .signaled(9)), witness: fixture.witness)
        }
        #expect(throws: SchedulerHelperClientError.outputTooLarge) {
            try SchedulerHelperClient.decodeOccurrenceOutput(output(Data(repeating: 0, count: SchedulerHelperClient.maximumStandardOutputBytes + 1)), witness: fixture.witness)
        }
    }

    @Test("input source mismatch is refused before signature validation or process launch")
    func sourceMismatchBeforeLaunch() async throws {
        let fixture = try #require(fixtures().first { !$0.items.isEmpty })
        let bundle = try OccurrenceHelperBundle()
        defer { bundle.cleanUp() }
        let runner = OccurrenceHelperRunner(result: output(try bytes(fixture.response)))
        let signature = OccurrenceHelperSignature()
        let client = SchedulerHelperClient(testingLocator: bundle, processRunner: runner, signatureValidator: signature)
        await #expect(throws: SchedulerHelperClientError.unsupportedCanonicalItem) {
            try await client.composeOccurrences(canonicalItems: [], witness: fixture.witness)
        }
        #expect(signature.calls == 0)
        #expect(await runner.lastInput() == nil)
    }

    @Test("signature rejection and oversized canonical input never launch v2")
    func signatureAndInputBounds() async throws {
        let fixture = try #require(fixtures().first { !$0.items.isEmpty })
        let bundle = try OccurrenceHelperBundle()
        defer { bundle.cleanUp() }
        let runner = OccurrenceHelperRunner(result: output(try bytes(fixture.response)))
        let signature = OccurrenceHelperSignature(failure: .invalidCodeSignature)
        let client = SchedulerHelperClient(testingLocator: bundle, processRunner: runner, signatureValidator: signature)
        await #expect(throws: SchedulerHelperClientError.invalidCodeSignature) {
            try await client.composeOccurrences(canonicalItems: fixture.items, witness: fixture.witness)
        }
        #expect(signature.calls == 1)
        #expect(await runner.lastInput() == nil)
        let oversized = Array(repeating: try #require(fixture.items.first), count: 10_001)
        await #expect(throws: SchedulerHelperClientError.inputTooLarge) {
            try await client.composeOccurrences(canonicalItems: oversized, witness: fixture.witness)
        }
        #expect(signature.calls == 1)
        #expect(await runner.lastInput() == nil)
    }

    @Test("cancellation after an uninterruptible runner returns never admits its v2 result")
    func canceledOutputIsRejected() async throws {
        let fixture = try #require(fixtures().first)
        let bundle = try OccurrenceHelperBundle()
        defer { bundle.cleanUp() }
        let runner = OccurrenceHelperRunner(result: output(try bytes(fixture.response)), holdsOutput: true)
        let client = SchedulerHelperClient(testingLocator: bundle,
            processRunner: runner, signatureValidator: OccurrenceHelperSignature())
        let items = fixture.items, witness = fixture.witness
        let task = Task { try await client.composeOccurrences(canonicalItems: items, witness: witness) }
        await runner.waitForInput()
        task.cancel()
        await runner.release()
        await #expect(throws: CancellationError.self) { try await task.value }
    }

    private struct Fixture {
        let witness: RoutinePlanningWitness
        let items: [DayWeaveCanonicalItem]
        let response: [String: Any]
        let helperRequestBody: [String: Any]
    }

    private func fixtures() throws -> [Fixture] {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let corpus = try object(JSONSerialization.jsonObject(with: Data(contentsOf:
            root.appendingPathComponent("fixtures/routine-planning-witness/wire-v1.json"))))
        let qualified = try #require(corpus["qualified"] as? [[String: Any]])
        return try qualified.map { entry in
            let response = try object(entry["response"])
            let result = try object(response["result"])
            let witness = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitness.self,
                from: bytes(try object(result["witness"])))
            let items = try SchedulerHelperClient.decoder.decode([DayWeaveCanonicalItem].self,
                from: JSONSerialization.data(withJSONObject: #require(entry["canonical_items"])))
            let helperRequest = try object(entry["helper_request"])
            return Fixture(witness: witness, items: items, response: try object(entry["helper_response"]),
                helperRequestBody: try object(helperRequest["request"]))
        }
    }

    private func object(_ value: Any?) throws -> [String: Any] { try #require(value as? [String: Any]) }
    private func bytes(_ value: [String: Any]) throws -> Data { try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
    private func output(_ bytes: Data, standardError: Data = Data(), termination: SchedulerHelperTermination = .exited(0)) -> SchedulerHelperProcessResult {
        .init(standardOutput: bytes, standardError: standardError, termination: termination)
    }
    private func decode(_ value: [String: Any], witness: RoutinePlanningWitness) throws -> RoutineOccurrenceLocalComposition {
        try SchedulerHelperClient.decodeOccurrenceOutput(output(bytes(value)), witness: witness)
    }
    private func jsonEqual(_ lhs: Any?, _ rhs: Any?) throws -> Bool {
        let left = try #require(lhs), right = try #require(rhs)
        return try JSONSerialization.data(withJSONObject: left, options: [.sortedKeys, .fragmentsAllowed])
            == JSONSerialization.data(withJSONObject: right, options: [.sortedKeys, .fragmentsAllowed])
    }
    /// Canonical item Date values use the helper's fixed microsecond spelling;
    /// producer whole-second Z values are semantically identical. All fields,
    /// scalar kinds, nullable metadata and array ordering are still compared.
    private func normalizeTimestampSpelling(_ value: Any?) -> Any {
        if let object = value as? [String: Any] { return object.mapValues { normalizeTimestampSpelling($0) } }
        if let array = value as? [Any] { return array.map { normalizeTimestampSpelling($0) } }
        if let string = value as? String, let id = UUID(uuidString: string) { return id.uuidString.lowercased() }
        if let string = value as? String, let date = SchedulerHelperRFC3339.date(from: string),
           let normalized = try? SchedulerHelperRFC3339.string(from: date) { return normalized }
        return value ?? NSNull()
    }
    private func changingComposition(_ value: [String: Any], _ mutation: (inout [String: Any]) -> Void) throws -> [String: Any] {
        var root = value, result = try object(value["result"]), composition = try object(result["composition"])
        mutation(&composition); result["composition"] = composition; root["result"] = result; return root
    }
    private func changingPlan(_ value: [String: Any], _ mutation: (inout [String: Any]) -> Void) throws -> [String: Any] {
        var root = value, result = try object(value["result"]), composition = try object(result["composition"]), plan = try object(composition["plan"])
        mutation(&plan); composition["plan"] = plan; result["composition"] = composition; root["result"] = result; return root
    }
}

private final class OccurrenceHelperBundle: SchedulerHelperLocating, @unchecked Sendable {
    let root: URL
    let location: SchedulerHelperLocation
    init() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("dayweave-occurrence-helper-\(UUID().uuidString.lowercased())")
        let bundle = root.appendingPathComponent("DayWeave.app"), helper = bundle.appendingPathComponent("Contents/Helpers/dayweave-scheduler-helper")
        try FileManager.default.createDirectory(at: helper.deletingLastPathComponent(), withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: helper)
        guard chmod(helper.path, 0o500) == 0 else { throw SchedulerHelperClientError.unsafeExecutable }
        location = .init(bundleURL: bundle, executableURL: helper)
    }
    func locate() throws -> SchedulerHelperLocation { location }
    func cleanUp() { try? FileManager.default.removeItem(at: root) }
}

private final class OccurrenceHelperSignature: SchedulerHelperCodeSignatureValidating, @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    private let failure: SchedulerHelperClientError?
    init(failure: SchedulerHelperClientError? = nil) { self.failure = failure }
    var calls: Int { lock.withLock { count } }
    func validate(executableURL: URL, hostBundleURL: URL) throws {
        lock.withLock { count += 1 }
        if let failure { throw failure }
    }
}

private actor OccurrenceHelperRunner: SchedulerHelperProcessRunning {
    let result: SchedulerHelperProcessResult
    private let holdsOutput: Bool
    private var input: Data?
    private var inputWaiters: [CheckedContinuation<Void, Never>] = []
    private var outputWaiter: CheckedContinuation<SchedulerHelperProcessResult, Never>?
    init(result: SchedulerHelperProcessResult, holdsOutput: Bool = false) {
        self.result = result; self.holdsOutput = holdsOutput
    }
    func run(executable: ValidatedSchedulerHelperExecutable, standardInput: Data, timeout: Duration) async throws -> SchedulerHelperProcessResult {
        input = standardInput
        if holdsOutput {
            return await withCheckedContinuation { continuation in
                outputWaiter = continuation
                inputWaiters.forEach { $0.resume() }; inputWaiters.removeAll()
            }
        }
        inputWaiters.forEach { $0.resume() }; inputWaiters.removeAll()
        return result
    }
    func lastInput() -> Data? { input }
    func waitForInput() async {
        if input != nil { return }
        await withCheckedContinuation { inputWaiters.append($0) }
    }
    func release() { outputWaiter?.resume(returning: result); outputWaiter = nil }
}
#endif
