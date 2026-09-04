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
            occurrences: [occurrence],
            pauses: [],
            analytics: [],
            pendingMutations: [pending]
        )
    }

    private static func occurrence(note: String?) -> DayWeaveHabitOccurrence {
        let occurredAt = date("2026-09-04T12:30:00.123456Z")
        return .init(
            evidence: .init(
                id: UUID(uuidString: "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb")!,
                habitID: UUID(uuidString: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")!,
                plannerOccurrenceID: UUID(uuidString: "cccccccc-3333-4333-8333-cccccccccccc")!,
                sourceScheduleRevisionID: UUID(uuidString: "dddddddd-4444-4444-8444-dddddddddddd")!,
                sourceItemRevision: 3,
                policyFingerprint: "sha256:\(String(repeating: "a", count: 64))",
                identity: .object([:]),
                nominalStart: date("2026-09-04T12:00:00.123456Z"),
                nominalEnd: date("2026-09-04T13:00:00.123456Z"),
                windowStart: date("2026-09-04T11:00:00.123456Z"),
                windowEnd: date("2026-09-04T14:00:00.123456Z"),
                localDate: DayWeaveLocalDate("2026-09-04")!,
                timezoneName: "Europe/Paris",
                expectedDurationSeconds: 3_600,
                expectedQuantity: 20,
                expectedUnit: "pages"
            ),
            outcome: .init(
                revision: 1,
                status: .partial,
                progressBasisPoints: 2_500,
                quantity: 5,
                unit: "pages",
                actualSeconds: 600,
                note: note,
                occurredAt: occurredAt,
                updatedAt: date("2026-09-04T12:31:00.123456Z")
            )
        )
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
