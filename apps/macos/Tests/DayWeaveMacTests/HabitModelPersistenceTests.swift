import CryptoKit
import Foundation
#if canImport(Testing)
import Testing
#endif
@testable import DayWeaveMac

#if canImport(Testing)
@Suite("Habit models and encrypted persistence", .serialized)
struct HabitModelPersistenceTests {
    @Test("calendar dates reject impossible or non-canonical values")
    func localDateValidation() {
        #expect(DayWeaveLocalDate("2028-02-29") != nil)
        #expect(DayWeaveLocalDate("2027-02-29") == nil)
        #expect(DayWeaveLocalDate("2026-9-04") == nil)
        #expect(DayWeaveLocalDate("0000-01-01") == nil)
        #expect(DayWeaveLocalDate("1899-12-31") == nil)
        #expect(DayWeaveLocalDate("2201-01-01") == nil)
    }

    @Test("every outcome status enforces its frozen evidence invariants")
    func outcomeValidation() {
        let date = Self.date("2026-09-04T12:30:00.123456Z")
        #expect(DayWeaveHabitOutcomeInput.completed(occurredAt: date).hasValidShape)
        #expect(DayWeaveHabitOutcomeInput(
            status: .partial,
            progressBasisPoints: 1,
            occurredAt: date
        ).hasValidShape)
        #expect(!DayWeaveHabitOutcomeInput(
            status: .partial,
            progressBasisPoints: 0,
            occurredAt: date
        ).hasValidShape)
        #expect(DayWeaveHabitOutcomeInput(
            status: .skipped,
            progressBasisPoints: 9_999,
            quantity: 3,
            unit: "pages",
            actualSeconds: 120,
            note: "Partial effort kept",
            occurredAt: date
        ).hasValidShape)
        #expect(!DayWeaveHabitOutcomeInput(
            status: .unresolved,
            progressBasisPoints: 0,
            note: "must be empty",
            occurredAt: date
        ).hasValidShape)
        #expect(!DayWeaveHabitOutcomeInput(
            status: .completed,
            progressBasisPoints: 10_000,
            quantity: 1,
            unit: nil,
            occurredAt: date
        ).hasValidShape)
    }

    @Test("habit text limits count Unicode scalars and match server control semantics")
    func habitTextUnicodeScalarValidation() {
        let occurredAt = Self.date("2026-09-04T12:30:00.123456Z")
        let decomposedCharacter = "e\u{301}"
        let maximumUnit = String(repeating: decomposedCharacter, count: 100)
        let oversizedUnit = maximumUnit + "x"
        let maximumNote = String(repeating: decomposedCharacter, count: 5_000)
        let oversizedNote = maximumNote + "x"
        let formatScalarUnit = "steps\u{200D}daily"

        #expect(maximumUnit.count == 100)
        #expect(maximumUnit.unicodeScalars.count == 200)
        #expect(oversizedUnit.count == 101)
        #expect(oversizedUnit.unicodeScalars.count == 201)
        #expect(DayWeaveHabitOutcomeInput.completed(
            quantity: 1,
            unit: maximumUnit,
            occurredAt: occurredAt
        ).hasValidShape)
        #expect(!DayWeaveHabitOutcomeInput.completed(
            quantity: 1,
            unit: oversizedUnit,
            occurredAt: occurredAt
        ).hasValidShape)
        #expect(!DayWeaveHabitOutcomeInput.completed(
            quantity: 1,
            unit: "pa\u{0}ges",
            occurredAt: occurredAt
        ).hasValidShape)
        #expect(DayWeaveHabitOutcomeInput.completed(
            quantity: 1,
            unit: formatScalarUnit,
            occurredAt: occurredAt
        ).hasValidShape)

        #expect(maximumNote.count == 5_000)
        #expect(maximumNote.unicodeScalars.count == 10_000)
        #expect(DayWeaveHabitOutcomeInput.skipped(
            note: maximumNote,
            occurredAt: occurredAt
        ).hasValidShape)
        #expect(!DayWeaveHabitOutcomeInput.skipped(
            note: oversizedNote,
            occurredAt: occurredAt
        ).hasValidShape)

        #expect(DayWeaveHabitQuantityTotal(unit: maximumUnit, amount: 1).hasValidShape)
        #expect(!DayWeaveHabitQuantityTotal(unit: oversizedUnit, amount: 1).hasValidShape)
        #expect(Self.occurrence(note: nil, unit: maximumUnit).evidence.hasValidShape)
        #expect(!Self.occurrence(note: nil, unit: oversizedUnit).evidence.hasValidShape)
    }

    @Test("authoritative recurrence identity accepts every exact core variant")
    func recurrenceIdentityVariants() {
        let identities: [JSONValue] = [
            .object([
                "type": .string("calendar_day"),
                "date": .string("2026-09-04"),
                "bucket_ordinal": .number(JSONNumber(UInt64(UInt16.max - 1))),
            ]),
            .object([
                "type": .string("calendar_week"),
                "week_key": .number(JSONNumber(integerLiteral: 2_461_288)),
                "bucket_ordinal": .number(JSONNumber(UInt64(UInt16.max - 1))),
            ]),
            .object([
                "type": .string("calendar_month"),
                "year": .number(JSONNumber(integerLiteral: 2_026)),
                "month": .number(JSONNumber(UInt64(9))),
                "bucket_ordinal": .number(JSONNumber(UInt64(UInt16.max - 1))),
            ]),
            .object([
                "type": .string("rolling_minutes"),
                "index": .number(JSONNumber(UInt64(UInt32.max))),
                "anchor": .string("2026-09-01T08:00:00.123456+02:00"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00.123456Z"),
            ]),
            .object([
                "type": .string("rolling_month"),
                "cycle": .number(JSONNumber(integerLiteral: Int64(Int32.max))),
                "index": .number(JSONNumber(UInt64(UInt16.max - 1))),
                "anchor": .string("2026-09-01T08:00:00+18:00"),
            ]),
            .object([
                "type": .string("custom_rule"),
                "rule_id": .string("123e4567-e89b-52d3-a456-426614174000"),
                "sequence": .number(JSONNumber(UInt64(9_999))),
                "date": .string("2026-09-04"),
            ]),
        ]

        for identity in identities {
            #expect(Self.evidence(identity: identity).hasValidShape)
        }
    }

    @Test("malformed or context-free recurrence evidence fails closed")
    func recurrenceIdentityAndContextRejection() {
        let malformed: [JSONValue] = [
            .object([:]),
            .object(["type": .string("custom")]),
            .object([
                "type": .string("calendar_day"),
                "date": .string("2026-09-04"),
                "bucket_ordinal": .number(JSONNumber(UInt64(0))),
                "future": .bool(true),
            ]),
            .object([
                "type": .string("calendar_month"),
                "year": .number(JSONNumber(integerLiteral: 2_026)),
                "month": .number(JSONNumber(UInt64(13))),
                "bucket_ordinal": .number(JSONNumber(UInt64(0))),
            ]),
            .object([
                "type": .string("calendar_day"),
                "date": .string("2026-09-04"),
                "bucket_ordinal": .number(JSONNumber(UInt64(UInt16.max))),
            ]),
            .object([
                "type": .string("rolling_minutes"),
                "index": .number(JSONNumber(integerLiteral: -1)),
                "anchor": .string("2026-09-01T08:00:00Z"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00.1234567Z"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00.123456000Z"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00.120Z"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00.000Z"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00+00:00"),
            ]),
            .object([
                "type": .string("after_completion"),
                "anchor": .string("2026-09-01T08:00:00-00:00"),
            ]),
            .object([
                "type": .string("rolling_month"),
                "cycle": .number(JSONNumber(integerLiteral: Int64(Int32.max) + 1)),
                "index": .number(JSONNumber(UInt64(0))),
                "anchor": .string("2026-09-01T08:00:00Z"),
            ]),
            .object([
                "type": .string("rolling_month"),
                "cycle": .number(JSONNumber(integerLiteral: 0)),
                "index": .number(JSONNumber(UInt64(UInt16.max))),
                "anchor": .string("2026-09-01T08:00:00Z"),
            ]),
            .object([
                "type": .string("custom_rule"),
                "rule_id": .string("123e4567-e89b-42d3-a456-426614174000"),
                "sequence": .number(JSONNumber(UInt64(0))),
                "date": .string("2026-09-04"),
            ]),
            .object([
                "type": .string("custom_rule"),
                "rule_id": .string("123e4567-e89b-52d3-2456-426614174000"),
                "sequence": .number(JSONNumber(UInt64(0))),
                "date": .string("2026-09-04"),
            ]),
            .object([
                "type": .string("custom_rule"),
                "rule_id": .string("123e4567-e89b-52d3-a456-426614174000"),
                "sequence": .number(JSONNumber(UInt64(10_000))),
                "date": .string("2026-09-04"),
            ]),
        ]
        for identity in malformed {
            #expect(!Self.evidence(identity: identity).hasValidShape)
        }

        let calendarDay = JSONValue.object([
            "type": .string("calendar_day"),
            "date": .string("2026-09-04"),
            "bucket_ordinal": .number(JSONNumber(UInt64(0))),
        ])
        let wrongWeek = JSONValue.object([
            "type": .string("calendar_week"),
            "week_key": .number(JSONNumber(integerLiteral: 2_461_200)),
            "bucket_ordinal": .number(JSONNumber(UInt64(0))),
        ])
        let wrongMonth = JSONValue.object([
            "type": .string("calendar_month"),
            "year": .number(JSONNumber(integerLiteral: 2_026)),
            "month": .number(JSONNumber(UInt64(8))),
            "bucket_ordinal": .number(JSONNumber(UInt64(0))),
        ])
        let wrongCustomDate = JSONValue.object([
            "type": .string("custom_rule"),
            "rule_id": .string("123e4567-e89b-52d3-a456-426614174000"),
            "sequence": .number(JSONNumber(UInt64(0))),
            "date": .string("2026-09-03"),
        ])
        let rolling = JSONValue.object([
            "type": .string("rolling_minutes"),
            "index": .number(JSONNumber(UInt64(0))),
            "anchor": .string("2026-09-01T08:00:00Z"),
        ])

        #expect(!Self.evidence(
            identity: calendarDay,
            plannerOccurrenceID: UUID(uuidString: "cccccccc-3333-4333-8333-cccccccccccc")!
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: calendarDay,
            plannerOccurrenceID: UUID(uuidString: "cccccccc-3333-5333-0333-cccccccccccc")!
        ).hasValidShape)
        let plannerOccurrenceID = UUID(
            uuidString: "cccccccc-3333-5333-8333-cccccccccccc"
        )!
        #expect(!Self.evidence(
            identity: calendarDay,
            id: plannerOccurrenceID,
            plannerOccurrenceID: plannerOccurrenceID
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: calendarDay,
            localDate: DayWeaveLocalDate("2026-09-03")!
        ).hasValidShape)
        #expect(!Self.evidence(identity: wrongWeek).hasValidShape)
        #expect(!Self.evidence(identity: wrongMonth).hasValidShape)
        #expect(!Self.evidence(identity: wrongCustomDate).hasValidShape)
        #expect(!Self.evidence(identity: calendarDay, timezoneName: "Mars/Olympus").hasValidShape)
        #expect(!Self.evidence(identity: calendarDay, timezoneName: "PST").hasValidShape)
        #expect(!Self.evidence(identity: calendarDay, timezoneName: "GMT+2").hasValidShape)
        #expect(!Self.evidence(identity: calendarDay, timezoneName: "GMT+01:00").hasValidShape)
        #expect(!Self.evidence(
            identity: calendarDay,
            timezoneName: "UTC\u{0}"
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: calendarDay,
            timezoneName: String(repeating: "A", count: 101)
        ).hasValidShape)
        #expect(Self.evidence(
            identity: calendarDay,
            expectedDurationSeconds: 31_622_400
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: calendarDay,
            expectedDurationSeconds: 31_622_401
        ).hasValidShape)

        let crossingStart = Self.date("2026-09-04T21:30:00.000000Z")
        let crossingEnd = Self.date("2026-09-04T22:30:00.000000Z")
        #expect(!Self.evidence(
            identity: calendarDay,
            nominalStart: crossingStart,
            nominalEnd: crossingEnd
        ).hasValidShape)
        #expect(Self.evidence(
            identity: rolling,
            nominalStart: crossingStart,
            nominalEnd: crossingEnd
        ).hasValidShape)

        let submicrosecond = Date(timeIntervalSince1970: 1_788_527_800.123_456_5)
        #expect(!Self.evidence(identity: rolling, nominalStart: submicrosecond).hasValidShape)
    }

    @Test("all evidence instants stay within RFC 3339's four-digit year domain")
    func recurrenceEvidenceDateYearBounds() {
        let yearOne = Date(timeIntervalSince1970: -62_135_596_800)
        let beforeYearOne = yearOne.addingTimeInterval(-1)
        let yearTenThousand = Date(timeIntervalSince1970: 253_402_300_800)
        let lastYear9999Second = yearTenThousand.addingTimeInterval(-1)
        #expect(DayWeaveHabitOccurrenceEvidence.isValidEvidenceDate(yearOne))
        #expect(DayWeaveHabitOccurrenceEvidence.isValidEvidenceDate(lastYear9999Second))
        #expect(!DayWeaveHabitOccurrenceEvidence.isValidEvidenceDate(beforeYearOne))
        #expect(!DayWeaveHabitOccurrenceEvidence.isValidEvidenceDate(yearTenThousand))

        let rolling = JSONValue.object([
            "type": .string("rolling_minutes"),
            "index": .number(JSONNumber(UInt64(0))),
            "anchor": .string("2026-09-01T08:00:00Z"),
        ])
        let ordinaryStart = Self.date("2026-09-04T12:00:00Z")
        let ordinaryEnd = Self.date("2026-09-04T13:00:00Z")

        #expect(!Self.evidence(
            identity: rolling,
            nominalStart: beforeYearOne,
            nominalEnd: ordinaryEnd,
            windowStart: beforeYearOne.addingTimeInterval(-3_600),
            windowEnd: ordinaryEnd.addingTimeInterval(3_600)
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: rolling,
            nominalStart: ordinaryStart,
            nominalEnd: yearTenThousand,
            windowStart: ordinaryStart.addingTimeInterval(-3_600),
            windowEnd: yearTenThousand.addingTimeInterval(3_600)
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: rolling,
            nominalStart: ordinaryStart,
            nominalEnd: ordinaryEnd,
            windowStart: beforeYearOne,
            windowEnd: ordinaryEnd.addingTimeInterval(3_600)
        ).hasValidShape)
        #expect(!Self.evidence(
            identity: rolling,
            nominalStart: ordinaryStart,
            nominalEnd: ordinaryEnd,
            windowStart: ordinaryStart.addingTimeInterval(-3_600),
            windowEnd: yearTenThousand
        ).hasValidShape)
    }

    @Test("calendar evidence accepts 23-hour and 25-hour IANA local days")
    func recurrenceEvidenceAcrossDSTTransitions() {
        func calendarDay(_ value: String) -> JSONValue {
            .object([
                "type": .string("calendar_day"),
                "date": .string(value),
                "bucket_ordinal": .number(JSONNumber(UInt64(0))),
            ])
        }

        #expect(Self.evidence(
            identity: calendarDay("2026-03-29"),
            nominalStart: Self.date("2026-03-28T23:00:00.000000Z"),
            nominalEnd: Self.date("2026-03-29T22:00:00.000000Z"),
            localDate: DayWeaveLocalDate("2026-03-29")!
        ).hasValidShape)
        #expect(Self.evidence(
            identity: calendarDay("2026-10-25"),
            nominalStart: Self.date("2026-10-24T22:00:00.000000Z"),
            nominalEnd: Self.date("2026-10-25T23:00:00.000000Z"),
            localDate: DayWeaveLocalDate("2026-10-25")!
        ).hasValidShape)
    }

    @Test("shared occurrence evidence fixture stays aligned with the habit protocol")
    func sharedOccurrenceEvidenceFixture() throws {
        let fixture = try Self.habitOccurrenceEvidenceFixture()
        #expect(fixture.schema == "dayweave.habit-occurrence-evidence-fixtures/1")

        let caseNames = (fixture.validCases + fixture.invalidCases).map(\.name)
        #expect(Set(caseNames).count == caseNames.count, "Fixture case names must be unique")

        for fixtureCase in fixture.validCases {
            let merged = Self.mergedEvidence(
                base: fixture.baseEvidence,
                patch: fixtureCase.patch
            )
            let mergedData = try Self.encoder().encode(merged)
            let evidence = try Self.decoder().decode(
                DayWeaveHabitOccurrenceEvidence.self,
                from: mergedData
            )
            #expect(evidence.hasValidShape, "Expected valid fixture: \(fixtureCase.name)")

            let reencodedData = try Self.encoder().encode(evidence)
            let reencoded = try JSONDecoder().decode(JSONValue.self, from: reencodedData)
            #expect(
                Self.normalizedEvidenceWire(reencoded)
                    == Self.normalizedEvidenceWire(merged),
                "Re-encoded fixture changed protocol fields: \(fixtureCase.name)"
            )
        }

        for fixtureCase in fixture.invalidCases {
            let merged = Self.mergedEvidence(
                base: fixture.baseEvidence,
                patch: fixtureCase.patch
            )
            let mergedData = try Self.encoder().encode(merged)
            let accepted = try? Self.decoder().decode(
                DayWeaveHabitOccurrenceEvidence.self,
                from: mergedData
            )
            #expect(
                accepted?.hasValidShape != true,
                "Expected invalid fixture to fail closed: \(fixtureCase.name)"
            )
        }
    }

    @Test("strict outcome decoding rejects unknown and omitted nullable keys")
    func strictOutcomeDecoding() {
        let unknown = Data("""
        {"status":"completed","progress_basis_points":10000,"quantity":null,"unit":null,"actual_seconds":null,"note":null,"occurred_at":"2026-09-04T12:30:00.000000Z","future":true}
        """.utf8)
        let omitted = Data("""
        {"status":"completed","progress_basis_points":10000,"occurred_at":"2026-09-04T12:30:00.000000Z"}
        """.utf8)
        #expect(throws: (any Error).self) {
            try Self.decoder().decode(DayWeaveHabitOutcomeInput.self, from: unknown)
        }
        #expect(throws: (any Error).self) {
            try Self.decoder().decode(DayWeaveHabitOutcomeInput.self, from: omitted)
        }
    }

    @Test("missed resolutions decode every frozen action and reject widened or mismatched shapes")
    func missedResolutionContract() throws {
        let base = """
        {"occurrence_evidence_id":"bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb","habit_id":"aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa","source_planner_occurrence_id":"cccccccc-3333-5333-8333-cccccccccccc","revision":REPLACE_REVISION,"configured_policy":"REPLACE_POLICY","action":REPLACE_ACTION,"created_at":"2026-09-04T12:00:00.000000Z","updated_at":"REPLACE_UPDATED"}
        """
        func decode(
            _ action: String,
            revision: Int,
            policy: String = "ask",
            updated: String = "2026-09-04T12:30:00.000000Z"
        ) throws -> DayWeaveHabitMissedResolution {
            let json = base
                .replacingOccurrences(of: "REPLACE_REVISION", with: String(revision))
                .replacingOccurrences(of: "REPLACE_POLICY", with: policy)
                .replacingOccurrences(of: "REPLACE_ACTION", with: action)
                .replacingOccurrences(of: "REPLACE_UPDATED", with: updated)
            return try Self.decoder().decode(
                DayWeaveHabitMissedResolution.self,
                from: Data(json.utf8)
            )
        }

        let decision = try decode(#"{"type":"decision_required"}"#, revision: 1)
        let skipped = try decode(#"{"type":"skip"}"#, revision: 2)
        let carried = try decode(
            #"{"type":"carry","window_start":"2026-09-04T12:30:00.000000Z","window_end":"2026-09-05T12:30:00.000000Z"}"#,
            revision: 2
        )
        let reduced = try decode(
            #"{"type":"reduce_frequency","suppressed_planner_occurrence_ids":["dddddddd-4444-5444-8444-dddddddddddd"]}"#,
            revision: 2
        )
        let cancelled = try decode(
            #"{"type":"cancelled","reason":"source_completed","resume_action":"carry"}"#,
            revision: 2
        )
        #expect(decision.action.isDecisionRequired)
        #expect(skipped.hasValidShape && carried.hasValidShape)
        #expect(reduced.hasValidShape && cancelled.hasValidShape)
        #expect(decision.canTransition(to: cancelled))

        let skippedCancellation = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: skipped.occurrenceEvidenceID,
            habitID: skipped.habitID,
            sourcePlannerOccurrenceID: skipped.sourcePlannerOccurrenceID,
            revision: 3,
            configuredPolicy: .ask,
            action: .cancelled(reason: .sourcePaused, resumeAction: .skip),
            createdAt: skipped.createdAt,
            updatedAt: skipped.updatedAt.addingTimeInterval(1)
        )
        let wrongSkippedCancellation = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: skipped.occurrenceEvidenceID,
            habitID: skipped.habitID,
            sourcePlannerOccurrenceID: skipped.sourcePlannerOccurrenceID,
            revision: 3,
            configuredPolicy: .ask,
            action: .cancelled(reason: .sourcePaused, resumeAction: .carry),
            createdAt: skipped.createdAt,
            updatedAt: skipped.updatedAt.addingTimeInterval(1)
        )
        #expect(skipped.canTransition(to: skippedCancellation))
        #expect(!skipped.canTransition(to: wrongSkippedCancellation))

        let reprompted = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: carried.occurrenceEvidenceID,
            habitID: carried.habitID,
            sourcePlannerOccurrenceID: carried.sourcePlannerOccurrenceID,
            revision: 3,
            configuredPolicy: .ask,
            action: .decisionRequired,
            createdAt: carried.createdAt,
            updatedAt: carried.updatedAt.addingTimeInterval(86_400)
        )
        #expect(carried.canTransition(to: reprompted))
        let askRecarried = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: carried.occurrenceEvidenceID,
            habitID: carried.habitID,
            sourcePlannerOccurrenceID: carried.sourcePlannerOccurrenceID,
            revision: 3,
            configuredPolicy: .ask,
            action: .carry(
                windowStart: carried.updatedAt.addingTimeInterval(1),
                windowEnd: carried.updatedAt.addingTimeInterval(3_601)
            ),
            createdAt: carried.createdAt,
            updatedAt: carried.updatedAt.addingTimeInterval(1)
        )
        #expect(!carried.canTransition(to: askRecarried))

        #expect(throws: (any Error).self) {
            try decode(#"{"type":"skip","future":true}"#, revision: 2)
        }
        #expect(throws: (any Error).self) {
            try decode(
                #"{"type":"cancelled","reason":"source_paused","resume_action":"carry"}"#,
                revision: 2,
                policy: "skip"
            )
        }
        #expect(throws: (any Error).self) {
            try decode(
                #"{"type":"carry","window_start":"2030-01-01T00:00:00.000000Z","window_end":"2030-01-02T00:00:00.000000Z"}"#,
                revision: 2
            )
        }
        #expect(throws: (any Error).self) {
            try decode(
                #"{"type":"reduce_frequency","suppressed_planner_occurrence_ids":["dddddddd-4444-4444-8444-dddddddddddd"]}"#,
                revision: 2
            )
        }

        let invalidReduction = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: reduced.occurrenceEvidenceID,
            habitID: reduced.habitID,
            sourcePlannerOccurrenceID: reduced.sourcePlannerOccurrenceID,
            revision: reduced.revision,
            configuredPolicy: reduced.configuredPolicy,
            action: .reduceFrequency(suppressedPlannerOccurrenceIDs: [
                UUID(uuidString: "dddddddd-4444-4444-8444-dddddddddddd")!,
            ]),
            createdAt: reduced.createdAt,
            updatedAt: reduced.updatedAt
        )
        #expect(!invalidReduction.hasValidShape)
        let context = try Context()
        defer { context.remove() }
        let invalidSnapshot = DayWeaveHabitClientSnapshot(
            savedAt: reduced.updatedAt,
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: "aGVhZDox",
            deltaCaughtUp: true,
            occurrences: [.init(
                evidence: Self.occurrence(note: nil).evidence,
                outcome: nil,
                missedResolution: invalidReduction
            )],
            pauses: [],
            analytics: [],
            pendingMutations: []
        )
        #expect(!invalidSnapshot.hasValidShape)
        #expect(throws: HabitPersistenceError.invalidSnapshot) {
            try context.persistence().preflightSave(invalidSnapshot)
        }
    }

    @Test("habit policy fingerprints match the server canonical vector")
    func habitPolicyFingerprintContract() throws {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let item = try decoder.decode(DayWeaveCanonicalItem.self, from: Data(#"""
        {
          "id":"00112233-4455-6677-8899-aabbccddeeff","is_sensitive":false,
          "kind":"habit","status":"scheduled","title":"Fingerprint vector","notes":null,
          "timezone_name":"Europe/Paris","duration_kind":"range",
          "duration_min_seconds":1200,"duration_seconds":2400,
          "duration_max_seconds":3600,"duration_source":"user",
          "deadline_kind":"none","deadline_at":null,"deadline_date":null,
          "deadline_strength":null,"deadline_soft_weight":null,"earliest_start_at":null,
          "recurrence":{"rrule":"FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,FR;COUNT=8","type":"custom"},
          "flexible_constraints":{"habit_minimum_spacing_minutes":45,
            "habit_missed_policy":"reduce_frequency",
            "habit_target":{"amount":12,"unit":"reps"},
            "preserves_streak_when_paused":false},
          "split_policy":{"type":"splittable","minimum_chunk_seconds":600,
            "maximum_chunk_seconds":1800},
          "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
          "has_own_effort":false,"blocked_reason_kind":null,"blocked_by_item_id":null,
          "blocked_reason":null,"is_executable":true,"revision":7,
          "created_at":"2026-09-04T10:00:00Z","updated_at":"2026-09-04T10:00:00Z",
          "completed_at":null,"deleted_at":null
        }
        """#.utf8))

        #expect(
            item.habitPolicyFingerprint
                == "sha256:4bfc50898f2b4f24cda17d040b21647e4d5ba5fe7fab7e7409024217c8249ebf"
        )
    }

    @Test("missed review prompts require a current executable leaf and active source lifecycle")
    func missedReviewEligibility() throws {
        let habitID = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")!
        func item(
            executable: Bool,
            revision: UInt64 = 3,
            title: String = "Private habit"
        ) throws -> DayWeaveCanonicalItem {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            return try decoder.decode(DayWeaveCanonicalItem.self, from: Data("""
            {"id":"\(habitID.uuidString.lowercased())","is_sensitive":false,
            "kind":"habit","status":"scheduled","title":"\(title)","notes":null,
            "timezone_name":"Europe/Paris","duration_seconds":3600,"deadline_at":null,
            "earliest_start_at":null,"recurrence":{"type":"daily","times_per_day":1},
            "flexible_constraints":{},"split_policy":{"type":"indivisible"},
            "importance":50,"urgency":50,"parent_id":null,"sibling_order":0,
            "is_executable":\(executable),"revision":\(revision),
            "created_at":"2026-09-04T10:00:00Z","updated_at":"2026-09-04T10:00:00Z",
            "completed_at":null,"deleted_at":null}
            """.utf8))
        }

        let active = try item(executable: true)
        #expect(
            active.habitPolicyFingerprint
                == "sha256:27ceb688ce161cdafc212a38e048744105d6cb22b79e7196d6f3327ff3c3af18"
        )
        let evidence = Self.evidence(
            identity: .object([
                "type": .string("calendar_day"),
                "date": .string("2026-09-04"),
                "bucket_ordinal": .number(.init(UInt64(0))),
            ]),
            policyFingerprint: try #require(active.habitPolicyFingerprint)
        )
        let resolution = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: evidence.id,
            habitID: evidence.habitID,
            sourcePlannerOccurrenceID: evidence.plannerOccurrenceID,
            revision: 1,
            configuredPolicy: .ask,
            action: .decisionRequired,
            createdAt: evidence.windowEnd,
            updatedAt: evidence.windowEnd
        )
        let occurrence = DayWeaveHabitOccurrence(
            evidence: evidence,
            outcome: nil,
            missedResolution: resolution
        )
        #expect(MissedHabitDecisionEligibility.allows(
            occurrence,
            item: active,
            canonicalItems: [active],
            pauses: []
        ))

        let harmlessEdit = try item(
            executable: true,
            revision: active.revision + 1,
            title: "Renamed private habit"
        )
        #expect(MissedHabitDecisionEligibility.allows(
            occurrence,
            item: harmlessEdit,
            canonicalItems: [harmlessEdit],
            pauses: []
        ))

        var changedPolicy = active
        changedPolicy.recurrence = .object([
            "type": .string("daily"),
            "times_per_day": .number(.init(UInt64(2))),
        ])
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: changedPolicy,
            canonicalItems: [changedPolicy],
            pauses: []
        ))

        var terminalItem = active
        terminalItem.status = .completed
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: terminalItem,
            canonicalItems: [terminalItem],
            pauses: []
        ))
        var blockedItem = active
        blockedItem.status = .blocked
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: blockedItem,
            canonicalItems: [blockedItem],
            pauses: []
        ))
        var futureStatusItem = active
        futureStatusItem.status = .unknown("future_status")
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: futureStatusItem,
            canonicalItems: [futureStatusItem],
            pauses: []
        ))
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: try item(executable: false),
            canonicalItems: [],
            pauses: []
        ))

        var child = active
        child.parentID = active.id
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: active,
            canonicalItems: [active, child],
            pauses: []
        ))

        let terminalOutcome = DayWeaveHabitOutcome(
            revision: 1,
            status: .completed,
            progressBasisPoints: 10_000,
            quantity: nil,
            unit: nil,
            actualSeconds: nil,
            note: nil,
            occurredAt: evidence.windowEnd,
            updatedAt: evidence.windowEnd
        )
        #expect(!MissedHabitDecisionEligibility.allows(
            .init(
                evidence: evidence,
                outcome: terminalOutcome,
                missedResolution: resolution
            ),
            item: active,
            canonicalItems: [active],
            pauses: []
        ))

        let pause = DayWeaveHabitPause(
            id: UUID(),
            habitID: evidence.habitID,
            revision: 1,
            startedAt: evidence.windowStart,
            endedAt: evidence.windowEnd,
            preservesStreak: true,
            createdAt: evidence.windowStart,
            updatedAt: evidence.windowEnd
        )
        #expect(!MissedHabitDecisionEligibility.allows(
            occurrence,
            item: active,
            canonicalItems: [active],
            pauses: [pause]
        ))

        let targetEvidence = Self.evidence(
            identity: .object([
                "type": .string("calendar_day"),
                "date": .string("2026-09-05"),
                "bucket_ordinal": .number(.init(UInt64(0))),
            ]),
            id: UUID(uuidString: "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbc")!,
            plannerOccurrenceID: UUID(
                uuidString: "cccccccc-3333-5333-8333-cccccccccccd"
            )!,
            nominalStart: Self.date("2026-09-05T12:00:00.123456Z"),
            localDate: DayWeaveLocalDate("2026-09-05")!,
            policyFingerprint: try #require(active.habitPolicyFingerprint)
        )
        let reduction = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: evidence.id,
            habitID: evidence.habitID,
            sourcePlannerOccurrenceID: evidence.plannerOccurrenceID,
            revision: 2,
            configuredPolicy: .ask,
            action: .reduceFrequency(
                suppressedPlannerOccurrenceIDs: [targetEvidence.plannerOccurrenceID]
            ),
            createdAt: evidence.windowEnd,
            updatedAt: evidence.windowEnd
        )
        let targetDecision = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: targetEvidence.id,
            habitID: targetEvidence.habitID,
            sourcePlannerOccurrenceID: targetEvidence.plannerOccurrenceID,
            revision: 1,
            configuredPolicy: .ask,
            action: .decisionRequired,
            createdAt: targetEvidence.windowEnd,
            updatedAt: targetEvidence.windowEnd
        )
        func checkpointOccurrence(
            evidence: DayWeaveHabitOccurrenceEvidence,
            outcome: DayWeaveHabitOutcome? = nil,
            resolution: DayWeaveHabitMissedResolution?
        ) -> HabitCompositionCheckpoint.Occurrence {
            .init(
                id: evidence.id,
                habitID: evidence.habitID,
                plannerOccurrenceID: evidence.plannerOccurrenceID,
                sourceItemRevision: evidence.sourceItemRevision,
                policyFingerprint: evidence.policyFingerprint,
                nominalStart: evidence.nominalStart,
                windowStart: evidence.windowStart,
                windowEnd: evidence.windowEnd,
                expectedDurationSeconds: evidence.expectedDurationSeconds,
                outcome: outcome.map {
                    .init(
                        revision: $0.revision,
                        status: $0.status,
                        progressBasisPoints: $0.progressBasisPoints,
                        occurredAt: $0.occurredAt
                    )
                },
                missedResolution: resolution,
                identity: evidence.identity,
                nominalEnd: evidence.nominalEnd,
                localDate: evidence.localDate
            )
        }
        func decisionIDs(
            sourceOutcome: DayWeaveHabitOutcome?,
            includeTargetInPublication: Bool = true,
            targetPublicationState: String = "generated",
            proofVersion: Int = DayWeavePublishedScheduleProof.currentVersion,
            latestHintRevision: UInt64 = 1
        ) -> Set<UUID> {
            let checkpoint = HabitCompositionCheckpoint(
                configurationIdentifier: "test",
                deltaCursor: "cursor",
                deltaCaughtUp: true,
                occurrences: [
                    checkpointOccurrence(
                        evidence: evidence,
                        outcome: sourceOutcome,
                        resolution: reduction
                    ),
                    checkpointOccurrence(
                        evidence: targetEvidence,
                        resolution: targetDecision
                    ),
                ],
                pauses: [],
                pendingMutationIDs: [],
                hasActiveOperation: false,
                operationGeneration: 1
            )
            let revisionID = UUID(uuidString: "dddddddd-4444-4444-8444-dddddddddddd")!
            let membership = [
                DayWeavePublishedScheduleOccurrenceProof(
                    plannerOccurrenceID: evidence.plannerOccurrenceID,
                    seriesItemID: active.id,
                    state: "generated"
                ),
                includeTargetInPublication ? .init(
                    plannerOccurrenceID: targetEvidence.plannerOccurrenceID,
                    seriesItemID: active.id,
                    state: targetPublicationState
                ) : nil,
            ]
                .compactMap { $0 }
                .sorted {
                    $0.plannerOccurrenceID.uuidString
                        < $1.plannerOccurrenceID.uuidString
                }
            let proof = DayWeavePublishedScheduleProof(
                version: proofVersion,
                configurationIdentifier: checkpoint.configurationIdentifier ?? "",
                revisionID: revisionID,
                revision: "1:\(revisionID.uuidString.lowercased())",
                revisionNumber: 1,
                inputDigest: "sha256:\(String(repeating: "a", count: 64))",
                asOf: evidence.nominalStart,
                horizonStart: evidence.windowStart.addingTimeInterval(-60),
                horizonEnd: targetEvidence.windowEnd.addingTimeInterval(60),
                timezoneName: "UTC",
                publishedAt: evidence.nominalStart,
                publishedBlocks: [],
                publishedOccurrences: proofVersion
                    == DayWeavePublishedScheduleProof.currentVersion ? membership : nil
            )
            return MissedHabitDecisionEligibility.effectiveDecisionIDs(
                checkpoint: checkpoint,
                canonicalItems: [active],
                publishedScheduleProof: proof,
                publishedScheduleLatestHintRevision: latestHintRevision
            )
        }
        #expect(!decisionIDs(sourceOutcome: nil).contains(targetEvidence.id))
        #expect(decisionIDs(sourceOutcome: terminalOutcome).contains(targetEvidence.id))
        #expect(decisionIDs(
            sourceOutcome: nil,
            includeTargetInPublication: false
        ).contains(targetEvidence.id))
        #expect(decisionIDs(
            sourceOutcome: nil,
            targetPublicationState: "completed"
        ).contains(targetEvidence.id))
        #expect(!decisionIDs(
            sourceOutcome: nil,
            targetPublicationState: "skipped"
        ).contains(targetEvidence.id))
        #expect(decisionIDs(
            sourceOutcome: nil,
            proofVersion: 2
        ).contains(targetEvidence.id))
        #expect(decisionIDs(
            sourceOutcome: nil,
            latestHintRevision: 2
        ).contains(targetEvidence.id))
    }

    @Test("encrypted missed-choice journals retain only server-authority inputs")
    func encryptedMissedChoiceJournal() throws {
        let context = try Context()
        defer { context.remove() }
        let occurrence = Self.occurrence(note: nil)
        let createdAt = Self.date("2026-09-04T12:31:00.123456Z")
        let resolution = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: occurrence.id,
            habitID: occurrence.evidence.habitID,
            sourcePlannerOccurrenceID: occurrence.evidence.plannerOccurrenceID,
            revision: 1,
            configuredPolicy: .ask,
            action: .decisionRequired,
            createdAt: createdAt,
            updatedAt: createdAt
        )
        let authoritative = DayWeaveHabitOccurrence(
            evidence: occurrence.evidence,
            outcome: occurrence.outcome,
            missedResolution: resolution
        )
        let operationID = UUID(uuidString: "eeeeeeee-5555-4555-8555-eeeeeeeeeeee")!
        let pending = DayWeavePendingHabitMutation.missedResolution(.init(
            habitID: authoritative.evidence.habitID,
            occurrenceID: authoritative.id,
            idempotencyKey: "habit-missed-resolution:test",
            command: .init(
                operationID: operationID,
                expectedRevision: 1,
                action: .carry
            ),
            createdAt: createdAt,
            conflictDetected: false
        ))
        let snapshot = DayWeaveHabitClientSnapshot(
            savedAt: createdAt,
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: "aGVhZDox",
            deltaCaughtUp: true,
            occurrences: [authoritative],
            pauses: [],
            analytics: [],
            pendingMutations: [pending]
        )

        _ = try context.persistence().save(snapshot, expectedRevision: .missing)
        let restored = try #require(context.persistence().loadRevisioned().snapshot)
        #expect(restored == snapshot)
        guard case let .missedResolution(value) = try #require(restored.pendingMutations.first)
        else {
            Issue.record("Expected a missed-resolution journal")
            return
        }
        #expect(value.command.action == .carry)
        #expect(value.command.expectedRevision == 1)
    }

    @Test("analytics partitions and rounded adherence must be self-consistent")
    func analyticsTotalsValidation() {
        let valid = DayWeaveHabitAnalyticsTotals(
            expected: 5,
            eligible: 4,
            completed: 3,
            partial: 1,
            skipped: 0,
            missed: 0,
            excused: 1,
            unresolved: 0,
            adherenceBasisPoints: 8_125,
            actualSecondsTotal: 60,
            quantityTotals: [.init(unit: "pages", amount: 2)]
        )
        #expect(valid.hasValidShape)
        let wrongAdherence = DayWeaveHabitAnalyticsTotals(
            expected: 5,
            eligible: 4,
            completed: 3,
            partial: 1,
            skipped: 0,
            missed: 0,
            excused: 1,
            unresolved: 0,
            adherenceBasisPoints: 10_001,
            actualSecondsTotal: 60,
            quantityTotals: []
        )
        #expect(!wrongAdherence.hasValidShape)
        let oversizedProjection = DayWeaveHabitAnalyticsTotals(
            expected: 50_001,
            eligible: 50_001,
            completed: 50_001,
            partial: 0,
            skipped: 0,
            missed: 0,
            excused: 0,
            unresolved: 0,
            adherenceBasisPoints: 10_000,
            actualSecondsTotal: 0,
            quantityTotals: []
        )
        #expect(!oversizedProjection.hasValidShape)
        let legitimateAggregateQuantity = DayWeaveHabitAnalyticsTotals(
            expected: 2,
            eligible: 2,
            completed: 2,
            partial: 0,
            skipped: 0,
            missed: 0,
            excused: 0,
            unresolved: 0,
            adherenceBasisPoints: 10_000,
            actualSecondsTotal: 0,
            quantityTotals: [.init(unit: "steps", amount: 2_000_000_000_000)]
        )
        #expect(legitimateAggregateQuantity.hasValidShape)
    }

    @Test("aggregate adherence weights the authoritative partial-progress score")
    func adherencePresentationUsesProgress() {
        func analytics(
            id: UUID,
            totals: DayWeaveHabitAnalyticsTotals
        ) -> DayWeaveHabitAnalytics {
            .init(
                habitID: id,
                startDate: DayWeaveLocalDate("2026-09-01")!,
                endDate: DayWeaveLocalDate("2026-09-30")!,
                bucket: .month,
                totals: totals,
                currentStreak: 0,
                longestStreak: 0,
                trends: [],
                supportiveFactCodes: []
            )
        }
        let partial = DayWeaveHabitAnalyticsTotals(
            expected: 2,
            eligible: 2,
            completed: 0,
            partial: 2,
            skipped: 0,
            missed: 0,
            excused: 0,
            unresolved: 0,
            adherenceBasisPoints: 5_000,
            actualSecondsTotal: 0,
            quantityTotals: []
        )
        let complete = DayWeaveHabitAnalyticsTotals(
            expected: 1,
            eligible: 1,
            completed: 1,
            partial: 0,
            skipped: 0,
            missed: 0,
            excused: 0,
            unresolved: 0,
            adherenceBasisPoints: 10_000,
            actualSecondsTotal: 0,
            quantityTotals: []
        )

        #expect(HabitStatisticsPresentation.adherencePercent([
            analytics(id: UUID(uuidString: "11111111-1111-4111-8111-111111111111")!, totals: partial),
            analytics(id: UUID(uuidString: "22222222-2222-4222-8222-222222222222")!, totals: complete),
        ]) == 67)
    }

    @Test("encrypted snapshots round-trip private notes and exact microseconds")
    func encryptedRoundTrip() throws {
        let context = try Context()
        defer { context.remove() }
        let persistence = context.persistence()
        let snapshot = Self.snapshot(binding: "origin-a|auth=device-a", note: "private reflection")

        let revision = try persistence.save(snapshot, expectedRevision: .missing)
        let restored = try persistence.loadRevisioned()

        #expect(restored.revision == revision)
        #expect(restored.snapshot == snapshot)
        let raw = try Data(contentsOf: context.fileURL)
        #expect(!String(decoding: raw, as: UTF8.self).contains("private reflection"))
        let permissions = try FileManager.default.attributesOfItem(atPath: context.fileURL.path)[.posixPermissions] as? NSNumber
        #expect(permissions?.intValue == 0o600)
    }

    @Test("authenticated snapshots reject non-canonical integer spellings")
    func encryptedSnapshotRejectsIntegralFloatIdentity() throws {
        let context = try Context()
        defer { context.remove() }
        let persistence = context.persistence()
        let snapshot = Self.snapshot(binding: "origin-a|auth=device-a", note: nil)
        _ = try persistence.save(snapshot, expectedRevision: .missing)

        let canonicalPlaintext = try Self.encoder().encode(snapshot)
        let canonicalText = try #require(String(data: canonicalPlaintext, encoding: .utf8))
        let alteredText = canonicalText.replacingOccurrences(
            of: "\"bucket_ordinal\":0",
            with: "\"bucket_ordinal\":0.0"
        )
        #expect(alteredText != canonicalText)
        let sealed = try AES.GCM.seal(
            Data(alteredText.utf8),
            using: SymmetricKey(data: Data(repeating: 7, count: 32)),
            authenticating: Data("DayWeave.HabitSnapshot|1|AES.GCM.256".utf8)
        )
        let combined = try #require(sealed.combined)
        let envelope = try JSONSerialization.data(
            withJSONObject: [
                "cipher": "AES.GCM.256",
                "magic": "DAYWEAVE-ENCRYPTED-HABITS",
                "payload": combined.base64EncodedString(),
                "version": 1,
            ],
            options: [.sortedKeys]
        )
        try envelope.write(to: context.fileURL)

        #expect(throws: HabitPersistenceError.invalidSnapshot) {
            try persistence.loadRevisioned()
        }
    }

    @Test("a different key cannot authenticate the private habit cache")
    func wrongKeyFails() throws {
        let context = try Context()
        defer { context.remove() }
        _ = try context.persistence(byte: 7).save(
            Self.snapshot(binding: "origin-a|auth=device-a", note: "secret"),
            expectedRevision: .missing
        )
        #expect(throws: HabitPersistenceError.authenticationFailed) {
            try context.persistence(byte: 8).loadRevisioned()
        }
    }

    @Test("compare-and-swap prevents another process from being overwritten")
    func concurrentModificationFailsClosed() throws {
        let context = try Context()
        defer { context.remove() }
        let persistence = context.persistence()
        _ = try persistence.save(
            Self.snapshot(binding: "origin-a|auth=device-a", note: nil),
            expectedRevision: .missing
        )
        #expect(throws: HabitPersistenceError.concurrentModification) {
            try persistence.save(
                Self.snapshot(binding: "origin-a|auth=device-a", note: "later"),
                expectedRevision: .missing
            )
        }
    }

    @Test("private content cannot exist without an origin and credential binding")
    func missingBindingFailsClosed() throws {
        let context = try Context()
        defer { context.remove() }
        let invalid = Self.snapshot(binding: nil, note: "private")
        #expect(!invalid.hasValidShape)
        #expect(throws: HabitPersistenceError.invalidSnapshot) {
            try context.persistence().preflightSave(invalid)
        }
    }

    @Test("legacy snapshots reset their tail cursor for correction-safe full replay")
    func legacySnapshotMigratesDeltaCaughtUpFailClosed() throws {
        let snapshot = Self.snapshot(binding: "origin-a|auth=device-a", note: "private")
        var object = try #require(
            try JSONSerialization.jsonObject(with: Self.encoder().encode(snapshot))
                as? [String: Any]
        )
        object.removeValue(forKey: "deltaCaughtUp")
        let legacy = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])

        let restored = try Self.decoder().decode(DayWeaveHabitClientSnapshot.self, from: legacy)

        #expect(restored.deltaCursor == nil)
        #expect(!restored.deltaCaughtUp)
        #expect(restored.hasValidShape)
    }

    @Test("schema-one terminal snapshots retain their cursor but require missed-resolution catch-up")
    func schemaOneSnapshotRevokesDeltaAuthority() throws {
        let snapshot = Self.snapshot(binding: "origin-a|auth=device-a", note: "private")
        var object = try #require(
            try JSONSerialization.jsonObject(with: Self.encoder().encode(snapshot))
                as? [String: Any]
        )
        object["schemaVersion"] = 1
        let legacy = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])

        let restored = try Self.decoder().decode(DayWeaveHabitClientSnapshot.self, from: legacy)

        #expect(restored.schemaVersion == DayWeaveHabitClientSnapshot.currentSchemaVersion)
        #expect(restored.deltaCursor == snapshot.deltaCursor)
        #expect(!restored.deltaCaughtUp)
        #expect(restored.hasValidShape)
    }

    @Test("authenticated schema-one snapshots cannot mint missed scheduling or replay authority")
    func schemaOneSnapshotStripsInjectedMissedAuthority() throws {
        let context = try Context()
        defer { context.remove() }
        let base = Self.occurrence(note: nil)
        let resolutionTime = base.evidence.windowEnd
        let resolution = DayWeaveHabitMissedResolution(
            occurrenceEvidenceID: base.id,
            habitID: base.evidence.habitID,
            sourcePlannerOccurrenceID: base.evidence.plannerOccurrenceID,
            revision: 1,
            configuredPolicy: .ask,
            action: .decisionRequired,
            createdAt: resolutionTime,
            updatedAt: resolutionTime
        )
        let occurrence = DayWeaveHabitOccurrence(
            evidence: base.evidence,
            outcome: base.outcome,
            missedResolution: resolution
        )
        let reconcileOperationID = UUID(
            uuidString: "11111111-aaaa-4aaa-8aaa-111111111111"
        )!
        let resolveOperationID = UUID(
            uuidString: "22222222-bbbb-4bbb-8bbb-222222222222"
        )!
        let pauseOperationID = UUID(
            uuidString: "33333333-cccc-4ccc-8ccc-333333333333"
        )!
        let pauseID = UUID(uuidString: "44444444-dddd-4ddd-8ddd-444444444444")!
        let genuineLegacyMutation = DayWeavePendingHabitMutation.pauseStart(.init(
            habitID: occurrence.evidence.habitID,
            idempotencyKey: "habit-pause:legacy-test",
            command: .init(
                operationID: pauseOperationID,
                pauseID: pauseID,
                startedAt: resolutionTime
            ),
            createdAt: resolutionTime,
            conflictDetected: false
        ))
        let injectedReconcile = DayWeavePendingHabitMutation.missedReconcile(.init(
            idempotencyKey: "habit-missed-reconcile:injected-test",
            command: .init(operationID: reconcileOperationID),
            limit: 200,
            createdAt: resolutionTime,
            conflictDetected: false
        ))
        let injectedResolution = DayWeavePendingHabitMutation.missedResolution(.init(
            habitID: occurrence.evidence.habitID,
            occurrenceID: occurrence.id,
            idempotencyKey: "habit-missed-resolution:injected-test",
            command: .init(
                operationID: resolveOperationID,
                expectedRevision: resolution.revision,
                action: .carry
            ),
            createdAt: resolutionTime,
            conflictDetected: false
        ))
        let injected = DayWeaveHabitClientSnapshot(
            savedAt: resolutionTime,
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: "aGVhZDox",
            deltaCaughtUp: true,
            occurrences: [occurrence],
            pauses: [],
            analytics: [],
            pendingMutations: [
                genuineLegacyMutation,
                injectedReconcile,
                injectedResolution,
            ]
        )
        #expect(injected.hasValidShape)
        var object = try #require(
            try JSONSerialization.jsonObject(with: Self.encoder().encode(injected))
                as? [String: Any]
        )
        object["schemaVersion"] = 1
        try Self.writeAuthenticatedSnapshot(
            JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
            to: context.fileURL
        )

        let restored = try #require(context.persistence().loadRevisioned().snapshot)

        #expect(restored.schemaVersion == DayWeaveHabitClientSnapshot.currentSchemaVersion)
        #expect(restored.deltaCursor == injected.deltaCursor)
        #expect(!restored.deltaCaughtUp)
        #expect(restored.occurrences.count == 1)
        #expect(restored.occurrences[0].missedResolution == nil)
        #expect(restored.pendingMutations == [genuineLegacyMutation])
        #expect(restored.hasValidShape)
    }

    @Test("a terminal pagination verdict requires a durable cursor")
    func terminalDeltaRequiresCursor() {
        let invalid = DayWeaveHabitClientSnapshot(
            savedAt: Self.date("2026-09-04T12:31:00.123456Z"),
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: nil,
            deltaCaughtUp: true,
            occurrences: [],
            pauses: [],
            analytics: [],
            pendingMutations: []
        )
        #expect(!invalid.hasValidShape)
    }

    @Test("unresolved outbox relations fail closed while reviewed conflicts remain recoverable")
    func outboxRelationsAndReviewedConflict() {
        let valid = Self.snapshot(binding: "origin-a|auth=device-a", note: nil)
        let pending = valid.pendingMutations[0]
        let missingTarget = DayWeaveHabitClientSnapshot(
            savedAt: valid.savedAt,
            configurationIdentifier: valid.configurationIdentifier,
            deltaCursor: valid.deltaCursor,
            deltaCaughtUp: true,
            occurrences: [],
            pauses: [],
            analytics: [],
            pendingMutations: [pending]
        )
        #expect(!missingTarget.hasValidShape)

        let reviewed = DayWeaveHabitClientSnapshot(
            savedAt: valid.savedAt,
            configurationIdentifier: valid.configurationIdentifier,
            deltaCursor: valid.deltaCursor,
            deltaCaughtUp: true,
            occurrences: [],
            pauses: [],
            analytics: [],
            pendingMutations: [pending.markingConflict()]
        )
        #expect(reviewed.hasValidShape)
    }

    @Test("overlapping and multiply-open pause histories are rejected")
    func pauseTopologyFailsClosed() {
        let habitID = UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")!
        let start = Self.date("2026-09-04T08:00:00.000000Z")
        let firstOpen = Self.pause(id: UUID(), habitID: habitID, startedAt: start)
        let secondOpen = Self.pause(
            id: UUID(),
            habitID: habitID,
            startedAt: start.addingTimeInterval(3_600)
        )
        let multipleOpen = DayWeaveHabitClientSnapshot(
            savedAt: Self.date("2026-09-04T12:31:00.123456Z"),
            configurationIdentifier: "origin-a|auth=device-a",
            deltaCursor: "aGVhZDox",
            deltaCaughtUp: true,
            occurrences: [],
            pauses: [firstOpen, secondOpen],
            analytics: [],
            pendingMutations: []
        )
        #expect(!multipleOpen.hasValidShape)

        let overlapping = DayWeaveHabitClientSnapshot(
            savedAt: multipleOpen.savedAt,
            configurationIdentifier: multipleOpen.configurationIdentifier,
            deltaCursor: multipleOpen.deltaCursor,
            deltaCaughtUp: true,
            occurrences: [],
            pauses: [
                Self.pause(
                    id: UUID(),
                    habitID: habitID,
                    startedAt: start,
                    endedAt: start.addingTimeInterval(7_200)
                ),
                Self.pause(
                    id: UUID(),
                    habitID: habitID,
                    startedAt: start.addingTimeInterval(3_600),
                    endedAt: start.addingTimeInterval(10_800)
                ),
            ],
            analytics: [],
            pendingMutations: []
        )
        #expect(!overlapping.hasValidShape)
    }

    @Test("symlink substitution is rejected before encrypted bytes are read")
    func symlinkReadRejected() throws {
        let context = try Context()
        defer { context.remove() }
        let target = context.root.appendingPathComponent("target")
        try Data("not a cache".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: context.fileURL, withDestinationURL: target)
        #expect(throws: HabitPersistenceError.readFailed) {
            try context.persistence().loadRevisioned()
        }
    }

    @Test("crash-orphaned encrypted temporary siblings are removed under the cache lock")
    func orphanedTemporaryFileIsRemoved() throws {
        let context = try Context()
        defer { context.remove() }
        let orphan = context.root.appendingPathComponent(
            ".habits.snapshot.encrypted.33333333-3333-4333-8333-333333333333.tmp"
        )
        try Data("encrypted orphan".utf8).write(to: orphan)

        #expect(try context.persistence().loadRevisioned().snapshot == nil)
        #expect(!FileManager.default.fileExists(atPath: orphan.path))
    }

    private static func snapshot(
        binding: String?,
        note: String?
    ) -> DayWeaveHabitClientSnapshot {
        let occurrence = occurrence(note: note)
        let operationID = UUID(uuidString: "eeeeeeee-5555-4555-8555-eeeeeeeeeeee")!
        let pending = DayWeavePendingHabitMutation.outcome(.init(
            habitID: occurrence.evidence.habitID,
            occurrenceID: occurrence.id,
            idempotencyKey: "habit-occurrence:\(operationID.uuidString.lowercased())",
            command: .init(
                operationID: operationID,
                expectedRevision: occurrence.outcome?.revision ?? 0,
                outcome: .init(
                    status: .skipped,
                    progressBasisPoints: 2_500,
                    quantity: 5,
                    unit: "pages",
                    actualSeconds: 600,
                    note: note,
                    occurredAt: date("2026-09-04T12:30:00.123456Z")
                )
            ),
            createdAt: date("2026-09-04T12:30:01.123456Z"),
            conflictDetected: false
        ))
        return .init(
            savedAt: date("2026-09-04T12:31:00.123456Z"),
            configurationIdentifier: binding,
            deltaCursor: "aGVhZDox",
            deltaCaughtUp: true,
            occurrences: [occurrence],
            pauses: [],
            analytics: [],
            pendingMutations: [pending]
        )
    }

    private static func occurrence(
        note: String?,
        unit: String = "pages"
    ) -> DayWeaveHabitOccurrence {
        let occurredAt = date("2026-09-04T12:30:00.123456Z")
        return .init(
            evidence: evidence(
                identity: .object([
                    "type": .string("calendar_day"),
                    "date": .string("2026-09-04"),
                    "bucket_ordinal": .number(JSONNumber(UInt64(0))),
                ]),
                expectedUnit: unit
            ),
            outcome: .init(
                revision: 1,
                status: .partial,
                progressBasisPoints: 2_500,
                quantity: 5,
                unit: unit,
                actualSeconds: 600,
                note: note,
                occurredAt: occurredAt,
                updatedAt: date("2026-09-04T12:31:00.123456Z")
            )
        )
    }

    private static func evidence(
        identity: JSONValue,
        id: UUID = UUID(uuidString: "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb")!,
        plannerOccurrenceID: UUID = UUID(
            uuidString: "cccccccc-3333-5333-8333-cccccccccccc"
        )!,
        nominalStart: Date = date("2026-09-04T12:00:00.123456Z"),
        nominalEnd: Date? = nil,
        windowStart: Date? = nil,
        windowEnd: Date? = nil,
        localDate: DayWeaveLocalDate = DayWeaveLocalDate("2026-09-04")!,
        timezoneName: String = "Europe/Paris",
        expectedUnit: String = "pages",
        expectedDurationSeconds: UInt64 = 3_600,
        policyFingerprint: String = "sha256:\(String(repeating: "a", count: 64))"
    ) -> DayWeaveHabitOccurrenceEvidence {
        let resolvedEnd = nominalEnd ?? nominalStart.addingTimeInterval(3_600)
        return .init(
            id: id,
            habitID: UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")!,
            plannerOccurrenceID: plannerOccurrenceID,
            sourceScheduleRevisionID: UUID(
                uuidString: "dddddddd-4444-4444-8444-dddddddddddd"
            )!,
            sourceItemRevision: 3,
            policyFingerprint: policyFingerprint,
            identity: identity,
            nominalStart: nominalStart,
            nominalEnd: resolvedEnd,
            windowStart: windowStart ?? nominalStart.addingTimeInterval(-3_600),
            windowEnd: windowEnd ?? resolvedEnd.addingTimeInterval(3_600),
            localDate: localDate,
            timezoneName: timezoneName,
            expectedDurationSeconds: expectedDurationSeconds,
            expectedQuantity: 20,
            expectedUnit: expectedUnit
        )
    }

    private struct HabitOccurrenceEvidenceFixture: Decodable {
        let schema: String
        let baseEvidence: [String: JSONValue]
        let validCases: [HabitOccurrenceEvidenceFixtureCase]
        let invalidCases: [HabitOccurrenceEvidenceFixtureCase]

        private enum CodingKeys: String, CodingKey {
            case schema
            case baseEvidence = "base_evidence"
            case validCases = "valid_cases"
            case invalidCases = "invalid_cases"
        }
    }

    private struct HabitOccurrenceEvidenceFixtureCase: Decodable {
        let name: String
        let patch: [String: JSONValue]
    }

    private static func habitOccurrenceEvidenceFixture() throws
        -> HabitOccurrenceEvidenceFixture {
        var repository = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { repository.deleteLastPathComponent() }
        let data = try Data(contentsOf: repository.appendingPathComponent(
            "fixtures/habit-protocol/occurrence-evidence-v1.json"
        ))
        return try JSONDecoder().decode(HabitOccurrenceEvidenceFixture.self, from: data)
    }

    private static func mergedEvidence(
        base: [String: JSONValue],
        patch: [String: JSONValue]
    ) -> JSONValue {
        var merged = base
        for (key, value) in patch { merged[key] = value }
        return .object(merged)
    }

    /// Foundation's Codable representations uppercase UUIDs and the persistence encoder writes
    /// whole-second dates with `.000Z`. Normalize only those spellings before comparing the
    /// re-encoded model with the server-wire fixture; nested recurrence identity JSON remains exact.
    private static func normalizedEvidenceWire(_ value: JSONValue) -> JSONValue {
        guard case var .object(fields) = value else { return value }
        for key in [
            "id", "habit_id", "planner_occurrence_id", "source_schedule_revision_id",
        ] {
            guard case let .string(raw)? = fields[key] else { continue }
            fields[key] = .string(raw.lowercased())
        }
        for key in ["nominal_start", "nominal_end", "window_start", "window_end"] {
            guard case let .string(raw)? = fields[key],
                  let date = CanonicalRFC3339Instant(raw)?.exactlyRepresentableDate,
                  let canonical = CanonicalRFC3339Instant(date: date) else { continue }
            fields[key] = .string(canonical.canonicalUTCString)
        }
        return .object(fields)
    }

    private static func pause(
        id: UUID,
        habitID: UUID,
        startedAt: Date,
        endedAt: Date? = nil
    ) -> DayWeaveHabitPause {
        .init(
            id: id,
            habitID: habitID,
            revision: 1,
            startedAt: startedAt,
            endedAt: endedAt,
            preservesStreak: true,
            createdAt: startedAt,
            updatedAt: endedAt ?? startedAt
        )
    }

    private static func encoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(CanonicalRFC3339Instant(date: date)!.canonicalUTCString)
        }
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }

    private static func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let text = try container.decode(String.self)
            guard let instant = CanonicalRFC3339Instant(text),
                  instant.hasPostgresPrecision,
                  let date = instant.exactlyRepresentableDate else {
                throw DecodingError.dataCorruptedError(in: container, debugDescription: "date")
            }
            return date
        }
        return decoder
    }

    private static func writeAuthenticatedSnapshot(_ plaintext: Data, to fileURL: URL) throws {
        let sealed = try AES.GCM.seal(
            plaintext,
            using: SymmetricKey(data: Data(repeating: 7, count: 32)),
            authenticating: Data("DayWeave.HabitSnapshot|1|AES.GCM.256".utf8)
        )
        let combined = try #require(sealed.combined)
        let envelope = try JSONSerialization.data(
            withJSONObject: [
                "cipher": "AES.GCM.256",
                "magic": "DAYWEAVE-ENCRYPTED-HABITS",
                "payload": combined.base64EncodedString(),
                "version": 1,
            ],
            options: [.sortedKeys]
        )
        try envelope.write(to: fileURL)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: Int16(0o600))],
            ofItemAtPath: fileURL.path
        )
    }

    private static func date(_ text: String) -> Date {
        CanonicalRFC3339Instant(text)!.exactlyRepresentableDate!
    }

    private struct Context {
        let root: URL
        let fileURL: URL

        init() throws {
            root = FileManager.default.temporaryDirectory
                .appendingPathComponent("DayWeaveHabitTests-\(UUID().uuidString)", isDirectory: true)
            fileURL = root.appendingPathComponent("habits.snapshot.encrypted")
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        }

        func persistence(byte: UInt8 = 7) -> EncryptedHabitPersistence {
            let key = try! PlannerEncryptionKey(data: Data(repeating: byte, count: 32))
            return .init(fileURL: fileURL, key: key)
        }

        func remove() {
            try? FileManager.default.removeItem(at: root)
        }
    }
}
#endif
