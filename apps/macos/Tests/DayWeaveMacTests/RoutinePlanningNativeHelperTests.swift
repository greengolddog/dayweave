import Darwin
import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

/// Opt-in real helper-process/codec integration. The locator and signature
/// admission are test-only; this does not establish packaged-host signing,
/// owner-device UI, authenticated service or provider acceptance.
@Suite("Real fixed-input routine helper and display custody", .serialized)
@MainActor
struct RoutinePlanningNativeHelperTests {
    @Test("actual Rust process and native codec retain every qualified producer plan",
        .enabled(if: ProcessInfo.processInfo.environment["DAYWEAVE_ROUTINE_NATIVE_HELPER_TEST"] == "1"))
    func actualHelper() async throws {
        let path = try #require(ProcessInfo.processInfo.environment["DAYWEAVE_ROUTINE_NATIVE_HELPER_PATH"])
        let source = URL(fileURLWithPath: path)
        #expect(source.isFileURL && source.path.hasPrefix("/"))
        let bundle = try NativeRoutineHelperBundle(source: source)
        defer { bundle.cleanUp() }
        let client = SchedulerHelperClient(testingLocator: bundle,
            processRunner: SchedulerHelperProcessRunner(), signatureValidator: TestOnlySignatureAdmission())
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let corpus = try RoutinePlanningCorpusValue(data: Data(contentsOf:
            root.appendingPathComponent("fixtures/routine-planning-witness/wire-v1.json"))).object()
        let samples = try #require(corpus["qualified"]).array()
        #expect(!samples.isEmpty)
        for sample in samples {
            let fields = try sample.object()
            let requestBytes = try #require(fields["request"]).data
            let request = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessRequest.self, from: requestBytes)
            let response = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self,
                from: #require(fields["response"]).data)
            guard case let .qualified(witness) = response.result else { throw RoutinePlanningWitnessError.invalidData }
            let items = try SchedulerHelperClient.decoder.decode([DayWeaveCanonicalItem].self,
                from: #require(fields["canonical_items"]).data)
            try witness.requireMatches(request)
            try witness.validate(canonicalItems: items)

            let composition = try await client.composeOccurrences(canonicalItems: items, witness: witness)
            #expect(composition.occurrenceSnapshotRevision == witness.occurrenceLifecycle.snapshotRevision)
            #expect(composition.composition.localInputFingerprint == witness.localInputFingerprint)
            #expect(composition.composition.sourceItemRevisions == witness.sourceItemRevisions)
            #expect(composition.composition.acceptedItemCount == items.count)
            #expect(composition.composition.rejectedItems.isEmpty)
            let actual = try RoutinePlanningWitnessValidation.decode(JSONValue.self, from: composition.rawOutput)
            let expected = try RoutinePlanningWitnessValidation.decode(JSONValue.self, from: #require(fields["helper_response"]).data)
            #expect(actual == expected)

            // Construct inert data only. This test never grants a live planner
            // capture, presentation, publication or execution admission.
            let origin = "https://synthetic.example", binding = origin + "|auth=synthetic-native-binding"
            let planner = PlannerStore(canonicalItems: items, canonicalDeltaCursor: "synthetic-canonical-terminal",
                canonicalConfigurationIdentifier: binding,
                routineOccurrenceState: .init(configurationIdentifier: binding, terminalDeltaCursor: request.terminalCursor),
                scheduleProfile: try ScheduleProfile.legacyDefault(timezoneName: "UTC", protectedFreeMinutes: 30),
                restoreFromPersistence: false)
            guard case let .string(clock)? = request.schedule.fields["as_of"],
                  let capturedAt = SchedulerHelperRFC3339.date(from: clock) else {
                throw RoutinePlanningWitnessError.invalidData
            }
            let capsule = try RoutinePlanningInputCapsule(origin: origin, configurationIdentifier: binding,
                originalRequestBody: requestBytes, request: request, witness: witness, canonicalItems: items,
                environment: .init(planner: planner, habitCheckpoint: nil), capturedAt: capturedAt)
            let display = try RoutinePlanningDisplayPlan(capsule: capsule, composition: composition,
                generatedAt: capturedAt.addingTimeInterval(1))
            let encoder = JSONEncoder(); encoder.dateEncodingStrategy = .millisecondsSince1970
            let decoder = JSONDecoder(); decoder.dateDecodingStrategy = .millisecondsSince1970
            let restoredCapsule = try decoder.decode(RoutinePlanningInputCapsule.self, from: encoder.encode(capsule))
            let restored = try decoder.decode(RoutinePlanningDisplayPlan.self, from: encoder.encode(display))
            #expect(restored.rawOutput == composition.rawOutput)
            #expect(try restored.validatedComposition(capsule: restoredCapsule) == composition)
            let presentation = try RoutinePlanningDisplayPresentation(artifact: restored, capsule: restoredCapsule)
            #expect(presentation.asOf == clock)
            #expect(presentation.blocks.count == composition.composition.plan.blocks.count)
            #expect(presentation.unscheduled.count == composition.composition.plan.unscheduled.count)
        }
    }
}

private struct TestOnlySignatureAdmission: SchedulerHelperCodeSignatureValidating {
    func validate(executableURL: URL, hostBundleURL: URL) throws {}
}

private final class NativeRoutineHelperBundle: SchedulerHelperLocating, @unchecked Sendable {
    let root: URL
    let location: SchedulerHelperLocation
    init(source: URL) throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appendingPathComponent("dayweave-native-routine-helper-\(UUID().uuidString.lowercased())")
        let bundle = root.appendingPathComponent("Synthetic.app")
        let helper = bundle.appendingPathComponent("Contents/Helpers/dayweave-scheduler-helper")
        try FileManager.default.createDirectory(at: helper.deletingLastPathComponent(), withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        do {
            try FileManager.default.copyItem(at: source, to: helper)
            guard chmod(helper.path, 0o500) == 0 else { throw SchedulerHelperClientError.unsafeExecutable }
            location = .init(bundleURL: bundle, executableURL: helper)
        } catch {
            try? FileManager.default.removeItem(at: root)
            throw error
        }
    }
    func locate() throws -> SchedulerHelperLocation { location }
    func cleanUp() { try? FileManager.default.removeItem(at: root) }
}
#endif
