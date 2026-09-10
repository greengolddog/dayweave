import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Private inert fixed-input routine presentation", .serialized)
@MainActor
struct RoutinePlanningDisplayPresentationTests {
    @Test("all producer cases retain original timestamps and full flat lifecycle without active schedule metadata")
    func producerPresentation() throws {
        let samples = try qualified()
        #expect(!samples.isEmpty)
        for sample in samples {
            let (capsule, composition) = try decode(sample)
            let artifact = try RoutinePlanningDisplayPlan(capsule: capsule, composition: composition,
                generatedAt: capsule.capturedAt.addingTimeInterval(1))
            let presentation = try RoutinePlanningDisplayPresentation(artifact: artifact, capsule: capsule)
            let asOf = try RoutinePlanningShape.string(capsule.witness.schedule.fields["as_of"])
            let start = try RoutinePlanningShape.string(capsule.witness.schedule.fields["horizon_start"])
            let end = try RoutinePlanningShape.string(capsule.witness.schedule.fields["horizon_end"])
            #expect(presentation.asOf == asOf)
            #expect(presentation.horizonStart == start)
            #expect(presentation.horizonEnd == end)
            #expect(presentation.blocks.count == composition.composition.plan.blocks.count)
            #expect(presentation.unscheduled.count == composition.composition.plan.unscheduled.count)
            #expect(presentation.members.count == capsule.witness.occurrenceLifecycle.instances.reduce(0) { $0 + $1.members.count })
            #expect(Set(presentation.members.map(\.id)).count == presentation.members.count)
            #expect(presentation.occurrenceSnapshotRevision == capsule.witness.occurrenceLifecycle.snapshotRevision)
            let raw = try RoutinePlanningWitnessValidation.decode(JSONValue.self, from: composition.rawOutput)
            let envelope = try RoutinePlanningShape.map(raw)
            let result = try RoutinePlanningShape.map(envelope["result"])
            let value = try RoutinePlanningShape.map(result["composition"])
            let plan = try RoutinePlanningShape.map(value["plan"])
            for row in try RoutinePlanningShape.array(plan["blocks"], maximum: 50_000) {
                let fields = try RoutinePlanningShape.map(row)
                let rawID = try RoutinePlanningShape.string(fields["id"])
                let id = try #require(UUID(uuidString: rawID))
                let displayed = try #require(presentation.blocks.first { $0.id == id })
                let rawStart = try RoutinePlanningShape.string(fields["start"])
                let rawEnd = try RoutinePlanningShape.string(fields["end"])
                #expect(displayed.originalStart == rawStart)
                #expect(displayed.originalEnd == rawEnd)
            }
            #expect(artifact.rawOutput == composition.rawOutput)
        }
    }

    @Test("display joins reject unrelated source and nonmember references even when helper envelope is well-formed")
    func unrelatedReferences() throws {
        let samples = try qualified()
        for sample in samples {
            let (capsule, original) = try decode(sample)
            var envelope = try #require(JSONSerialization.jsonObject(with: original.rawOutput) as? [String: Any])
            var result = try #require(envelope["result"] as? [String: Any])
            var composed = try #require(result["composition"] as? [String: Any])
            var plan = try #require(composed["plan"] as? [String: Any])
            let blocks = try #require(plan["blocks"] as? [[String: Any]])
            guard let index = blocks.firstIndex(where: { $0["occurrence_id"] is String }) else { continue }
            let replacements = [UUID(), capsule.canonicalItems.first { $0.kind == .project }?.id].compactMap { $0 }
            for replacement in replacements {
                var changed = blocks; changed[index]["item_id"] = replacement.uuidString.lowercased()
                plan["blocks"] = changed; composed["plan"] = plan
                result["composition"] = composed; envelope["result"] = result
                let output = try JSONSerialization.data(withJSONObject: envelope, options: [.sortedKeys])
                let composition = try SchedulerHelperClient.decodeOccurrenceOutput(
                    .init(standardOutput: output, standardError: Data(), termination: .exited(0)), witness: capsule.witness)
                let artifact = try RoutinePlanningDisplayPlan(capsule: capsule, composition: composition,
                    generatedAt: capsule.capturedAt.addingTimeInterval(1))
                #expect(throws: RoutinePlanningWitnessError.self) {
                    try RoutinePlanningDisplayPresentation(artifact: artifact, capsule: capsule)
                }
            }
        }
    }

    private func qualified() throws -> [RoutinePlanningCorpusValue] {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let corpus = try RoutinePlanningCorpusValue(data: Data(contentsOf:
            root.appendingPathComponent("fixtures/routine-planning-witness/wire-v1.json"))).object()
        return try #require(corpus["qualified"]).array()
    }

    /// Producer bytes are an inert presentation fixture, not an authenticated
    /// native capture or real process execution. Those have separate gates.
    private func decode(_ sample: RoutinePlanningCorpusValue) throws -> (RoutinePlanningInputCapsule, RoutineOccurrenceLocalComposition) {
        let fields = try sample.object(), requestBytes = try #require(fields["request"]).data
        let request = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessRequest.self, from: requestBytes)
        let response = try RoutinePlanningWitnessValidation.decode(RoutinePlanningWitnessResponse.self, from: #require(fields["response"]).data)
        guard case let .qualified(witness) = response.result else { throw RoutinePlanningWitnessError.invalidData }
        let items = try SchedulerHelperClient.decoder.decode([DayWeaveCanonicalItem].self, from: #require(fields["canonical_items"]).data)
        let origin = "https://synthetic.example", binding = origin + "|auth=synthetic-display-binding"
        let planner = PlannerStore(canonicalItems: items, canonicalDeltaCursor: "synthetic-canonical-terminal",
            canonicalConfigurationIdentifier: binding,
            routineOccurrenceState: .init(configurationIdentifier: binding, terminalDeltaCursor: request.terminalCursor),
            scheduleProfile: try ScheduleProfile.legacyDefault(timezoneName: "UTC", protectedFreeMinutes: 30),
            restoreFromPersistence: false)
        let clock = try RoutinePlanningShape.string(request.schedule.fields["as_of"])
        let capturedAt = try #require(SchedulerHelperRFC3339.date(from: clock))
        let capsule = try RoutinePlanningInputCapsule(origin: origin, configurationIdentifier: binding,
            originalRequestBody: requestBytes, request: request, witness: witness, canonicalItems: items,
            environment: .init(planner: planner, habitCheckpoint: nil), capturedAt: capturedAt)
        let helperBytes = try #require(fields["helper_response"]).data
        let composition = try SchedulerHelperClient.decodeOccurrenceOutput(
            .init(standardOutput: helperBytes, standardError: Data(), termination: .exited(0)), witness: witness)
        return (capsule, composition)
    }
}

/// A bounded synthetic empty plan for coordinator ownership tests. This is not
/// scheduler parity evidence; the opt-in native helper test runs the real process.
enum RoutinePlanningDisplaySyntheticComposition {
    static func make(witness: RoutinePlanningWitness) throws -> RoutineOccurrenceLocalComposition {
        let plan: [String: Any] = ["as_of": try RoutinePlanningShape.string(witness.schedule.fields["as_of"]),
            "horizon_start": try RoutinePlanningShape.string(witness.schedule.fields["horizon_start"]),
            "horizon_end": try RoutinePlanningShape.string(witness.schedule.fields["horizon_end"]),
            "blocks": [], "unscheduled": [], "decisions": [], "violations": [], "occurrences": [],
            "score": ["scheduled_minutes": 0, "unscheduled_minutes": 0, "soft_penalty": 0, "moved_minutes": 0]]
        let object: [String: Any] = ["protocol": "dayweave.scheduler.helper", "version": 2,
            "result": ["type": "composition", "composition": [
                "local_input_fingerprint": witness.localInputFingerprint,
                "source_item_revisions": Dictionary(uniqueKeysWithValues: witness.sourceItemRevisions.map { ($0.key.uuidString.lowercased(), $0.value) }),
                "source_item_count": witness.sourceItemRevisions.count, "accepted_item_count": witness.sourceItemRevisions.count,
                "rejected_items": [], "ignored_previous_assignments": [],
                "occurrence_snapshot_revision": witness.occurrenceLifecycle.snapshotRevision, "plan": plan]]]
        let bytes = Data(" \n\t".utf8) + (try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]))
        return try SchedulerHelperClient.decodeOccurrenceOutput(
            .init(standardOutput: bytes, standardError: Data(), termination: .exited(0)), witness: witness)
    }
}
#endif
