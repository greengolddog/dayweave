import Foundation
#if canImport(Testing)
import Testing
@testable import DayWeaveMac

@Suite("Planner presentation timezone boundary")
struct PlannerPresentationTests {
    @Test("schedule labels require an explicit timezone and invalid zones fail to UTC")
    func explicitTimezoneLabels() {
        let block = Self.block(
            start: Self.date("2026-08-30T12:00:00Z"),
            end: Self.date("2026-08-30T13:30:00Z")
        )

        let madrid = block.timeRange(timezoneName: "Europe/Madrid")
        let newYork = block.timeRange(timezoneName: "America/New_York")
        let invalid = block.timeRange(timezoneName: "PST")
        let utc = block.timeRange(timezoneName: "UTC")

        #expect(madrid != newYork)
        #expect(madrid.contains("+02:00"))
        #expect(newYork.contains("-04:00"))
        #expect(invalid == utc)
        #expect(utc.contains("Z"))

        let madridDeadline = PlannerTimeZone.dateTimeLabel(
            block.start,
            timezoneName: "Europe/Madrid"
        )
        let newYorkDeadline = PlannerTimeZone.dateTimeLabel(
            block.start,
            timezoneName: "America/New_York"
        )
        #expect(madridDeadline != newYorkDeadline)
        #expect(madridDeadline.contains("+02:00"))
        #expect(newYorkDeadline.contains("-04:00"))
    }

    @Test("profile calendars are Monday-first and retain DST-length local days")
    func profileCalendarBuckets() throws {
        let reference = Self.date("2026-10-25T12:00:00Z")
        let week = PlannerPresentation.weekDays(
            containing: reference,
            timezoneName: "Europe/Madrid"
        )
        let calendar = PlannerPresentation.calendar(timezoneName: "Europe/Madrid")
        let interval = try #require(PlannerPresentation.dayInterval(
            containing: reference,
            timezoneName: "Europe/Madrid"
        ))
        let first = try #require(week.first)
        let last = try #require(week.last)

        #expect(week.count == 7)
        #expect(calendar.component(.weekday, from: first) == 2)
        #expect(calendar.component(.weekday, from: last) == 1)
        #expect(interval.duration == 25 * 60 * 60)
    }

    @Test("day buckets intersect overnight work and reject adjacent days")
    func profileDayIntersection() {
        let reference = Self.date("2026-08-30T12:00:00Z")
        let overnight = Self.block(
            start: Self.date("2026-08-29T21:30:00Z"),
            end: Self.date("2026-08-29T22:30:00Z")
        )
        let followingDay = Self.block(
            start: Self.date("2026-08-30T22:00:00Z"),
            end: Self.date("2026-08-30T23:00:00Z")
        )

        let selected = PlannerPresentation.blocks(
            [overnight, followingDay],
            intersectingDayContaining: reference,
            timezoneName: "Europe/Madrid"
        )

        #expect(selected.map(\.id) == [overnight.id])
    }

    @Test("external fixed inputs remain identifiable but never enter execution rollups")
    func externalFixedPresentationPredicate() {
        var fixed = Self.block(
            start: Self.date("2026-08-30T12:00:00Z"),
            end: Self.date("2026-08-30T13:00:00Z")
        )
        fixed.previewKind = "external_fixed"
        var work = fixed
        work.previewKind = "planned"

        #expect(fixed.isExternalFixedBlock)
        #expect(!fixed.contributesToExecutionPresentation)
        #expect(!work.isExternalFixedBlock)
        #expect(work.contributesToExecutionPresentation)
    }

    private static func block(start: Date, end: Date) -> ScheduleBlock {
        ScheduleBlock(
            id: UUID(),
            title: "Presentation fixture",
            kind: .task,
            start: start,
            end: end,
            status: .scheduled,
            project: "Fixture",
            notes: "",
            energy: .medium,
            isFlexible: true,
            isHardConstraint: false,
            actualMinutes: nil
        )
    }

    private static func date(_ value: String) -> Date {
        ISO8601DateFormatter().date(from: value)!
    }
}
#endif
