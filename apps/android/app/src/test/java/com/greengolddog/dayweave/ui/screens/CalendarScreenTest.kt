package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleDisplayHorizon
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.ScheduleItemPresentationSlice
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

class CalendarScreenTest {
    @Test
    fun firmHorizonDatesFollowLocalCalendarDaysAcrossSpringDst() {
        val zone = ZoneId.of("Europe/Madrid")
        val startDate = LocalDate.of(2026, 3, 27)
        val horizon = ScheduleDisplayHorizon(
            start = startDate.atStartOfDay(zone).toInstant(),
            end = startDate.plusDays(7).atStartOfDay(zone).toInstant(),
            timezone = zone,
        )

        assertEquals(
            (0L until 7L).map(startDate::plusDays),
            firmHorizonDates(horizon),
        )
        assertEquals(167L, Duration.between(horizon.start, horizon.end).toHours())
    }

    @Test
    fun exactReplicaHorizonIncludesEveryIntersectedLocalDate() {
        val zone = ZoneId.of("Europe/Paris")
        val horizon = ScheduleDisplayHorizon(
            start = Instant.parse("2026-09-01T21:30:00Z"),
            end = Instant.parse("2026-09-02T23:15:00Z"),
            timezone = zone,
        )

        assertEquals(
            listOf(
                LocalDate.of(2026, 9, 1),
                LocalDate.of(2026, 9, 2),
                LocalDate.of(2026, 9, 3),
            ),
            firmHorizonDates(horizon),
        )
    }

    @Test
    fun timelineLabelIncludesThePlanningDateAndUsesThePlanningZone() {
        val clippedStart = Instant.parse("2026-09-03T22:15:00Z")
        val slice = ScheduleItemPresentationSlice(
            item = ScheduleItem(
                id = "task-1",
                title = "Plan tomorrow",
                kind = ItemKind.TASK,
                startMinute = 0,
                durationMinutes = 30,
                status = ItemStatus.SCHEDULED,
            ),
            clippedStart = clippedStart,
            clippedEnd = clippedStart.plusSeconds(30 * 60),
            startTimeLabel = "00:15",
            weekStartLabel = "Fri 00:15",
            durationMinutes = 30,
            durationLabel = "30m",
        )

        assertEquals(
            "Fri, Sep 4, 2026 · 00:15",
            firmHorizonTimelineLabel(
                slice = slice,
                timezone = ZoneId.of("Europe/Paris"),
                locale = Locale.US,
            ),
        )
        assertEquals(
            "Fri, Sep 4, 2026 · 12:15 AM",
            firmHorizonTimelineLabel(
                slice = slice,
                timezone = ZoneId.of("Europe/Paris"),
                locale = Locale.US,
                use24HourFormat = false,
            ),
        )
    }

    @Test
    fun legacyTimelineLabelFallsBackToItsTimeOnlyPresentation() {
        val slice = ScheduleItemPresentationSlice(
            item = ScheduleItem(
                id = "legacy-task",
                title = "Legacy task",
                kind = ItemKind.TASK,
                startMinute = 10 * 60,
                durationMinutes = 30,
                status = ItemStatus.SCHEDULED,
            ),
            clippedStart = null,
            clippedEnd = null,
            startTimeLabel = "10:00",
            weekStartLabel = "10:00",
            durationMinutes = 30,
            durationLabel = "30m",
        )

        assertEquals(
            "Unplaced · 10:00",
            firmHorizonTimelineLabel(slice, ZoneId.of("Europe/Paris"), Locale.US),
        )
    }

    @Test
    fun ambiguousMidnightEndDoesNotAddAnEighthDate() {
        val zone = ZoneId.of("America/Havana")
        val startDate = LocalDate.of(2026, 10, 25)
        val horizon = ScheduleDisplayHorizon(
            start = Instant.parse("2026-10-25T04:00:00Z"),
            end = Instant.parse("2026-11-01T04:00:00Z"),
            timezone = zone,
        )

        assertEquals((0L until 7L).map(startDate::plusDays), firmHorizonDates(horizon))
    }

    @Test
    fun horizonDateSemanticsIncludeFullDateAndTodayState() {
        val date = LocalDate.of(2026, 9, 2)

        assertEquals(
            "Wednesday, September 2, 2026",
            firmHorizonDateContentDescription(date, isToday = false, locale = Locale.US),
        )
        assertEquals(
            "Today, Wednesday, September 2, 2026",
            firmHorizonDateContentDescription(date, isToday = true, locale = Locale.US),
        )
    }

    @Test
    fun timelineKeysRemainDistinctForSlicesOfTheSameBlock() {
        val item = ScheduleItem(
            id = "shared|id",
            title = "Shared",
            kind = ItemKind.TASK,
            startMinute = 0,
            durationMinutes = 30,
            status = ItemStatus.SCHEDULED,
        )
        fun slice(start: Instant) = ScheduleItemPresentationSlice(
            item = item,
            clippedStart = start,
            clippedEnd = start.plusSeconds(1_800),
            startTimeLabel = "00:00",
            weekStartLabel = "Mon 00:00",
            durationMinutes = 30,
            durationLabel = "30m",
        )

        assertNotEquals(
            firmHorizonTimelineKey(slice(Instant.parse("2026-09-01T00:00:00Z"))),
            firmHorizonTimelineKey(slice(Instant.parse("2026-09-02T00:00:00Z"))),
        )
    }
}
