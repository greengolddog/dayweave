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
        expectedDurationSeconds: UInt64 = 3_600
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
            policyFingerprint: "sha256:\(String(repeating: "a", count: 64))",
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
            guard let date = CanonicalRFC3339Instant(text)?.exactlyRepresentableDate else {
                throw DecodingError.dataCorruptedError(in: container, debugDescription: "date")
            }
            return date
        }
        return decoder
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
