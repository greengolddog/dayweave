package com.greengolddog.dayweave.scheduler

import com.greengolddog.dayweave.model.ScheduleAvailabilityDay
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.ScheduleLocalTimeWindow
import com.greengolddog.dayweave.model.ScheduleProtectedDay
import com.greengolddog.dayweave.model.ScheduleSleepInterval
import com.greengolddog.dayweave.model.ScheduleWeekday
import java.time.Instant
import java.time.ZoneId
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ScheduleProfileExpansionTest {
    @Test
    fun configuredTimezoneAndWeekdayWindowsExpandWithVisibleProtection() {
        val profile = mondayProfile(timezoneName = "UTC")

        val expanded = profile.expandForComposition(
            fallbackZone = ZoneId.of("America/Los_Angeles"),
            horizonStart = Instant.parse("2026-09-07T00:00:00Z"),
            horizonEnd = Instant.parse("2026-09-09T00:00:00Z"),
        )

        assertEquals("UTC", expanded.planningZone.id)
        assertEquals(
            listOf(
                "2026-09-07T07:00:00Z" to "2026-09-07T10:00:00Z",
                "2026-09-07T11:00:00Z" to "2026-09-07T17:00:00Z",
            ),
            expanded.availability.map { it.start to it.end },
        )
        assertEquals(listOf("sleep", "protected_time", "sleep", "sleep"), expanded.fixedBlocks.map { it.source })
        assertTrue(expanded.fixedBlocks.all { it.isSensitive })
        assertEquals("2026-09-06T23:00:00Z", expanded.fixedBlocks.first().start)
        assertEquals("2026-09-09T06:00:00Z", expanded.fixedBlocks.last().end)
        expanded.fixedBlocks.forEach { assertEquals(8, UUID.fromString(it.id).version()) }

        val repeated = profile.expandForComposition(
            fallbackZone = ZoneId.of("Europe/Paris"),
            horizonStart = Instant.parse("2026-09-07T00:00:00Z"),
            horizonEnd = Instant.parse("2026-09-09T00:00:00Z"),
        )
        assertEquals(expanded.fixedBlocks.map { it.id }, repeated.fixedBlocks.map { it.id })
    }

    @Test
    fun legacyExpansionRetainsDeviceZoneAndTwentyFourHundredBoundary() {
        val profile = ScheduleCompositionProfileSnapshot(
            firmHorizonDays = 1,
            dayStartMinute = 23 * 60,
            dayEndMinute = 24 * 60,
        )

        val expanded = profile.expandForComposition(
            fallbackZone = ZoneId.of("America/Havana"),
            horizonStart = Instant.parse("2026-10-31T04:00:00Z"),
            horizonEnd = Instant.parse("2026-11-01T04:00:00Z"),
        )

        assertEquals("America/Havana", expanded.planningZone.id)
        assertEquals("2026-11-01T03:00:00Z", expanded.availability.single().start)
        assertEquals(expanded.availability.single().end, "2026-11-01T04:00:00Z")
        assertTrue(expanded.fixedBlocks.isEmpty())
    }

    @Test
    fun ambiguousAvailabilityUsesConservativeOffsetsAndGapFailsClosed() {
        val overlap = sundayProfile(
            timezoneName = "Europe/Paris",
            window = ScheduleLocalTimeWindow(2 * 60 + 30, 3 * 60),
            sleep = ScheduleSleepInterval(23 * 60, 60),
        ).expandForComposition(
            fallbackZone = ZoneId.of("UTC"),
            horizonStart = Instant.parse("2026-10-24T22:00:00Z"),
            horizonEnd = Instant.parse("2026-10-25T23:00:00Z"),
        )
        assertEquals("2026-10-25T01:30:00Z", overlap.availability.single().start)
        assertEquals("2026-10-25T02:00:00Z", overlap.availability.single().end)

        val gapProfile = sundayProfile(
            timezoneName = "Europe/Paris",
            window = ScheduleLocalTimeWindow(2 * 60 + 30, 3 * 60),
            sleep = ScheduleSleepInterval(23 * 60, 60),
        )
        assertThrows(ScheduleProfileExpansionException::class.java) {
            gapProfile.expandForComposition(
                fallbackZone = ZoneId.of("UTC"),
                horizonStart = Instant.parse("2026-03-28T23:00:00Z"),
                horizonEnd = Instant.parse("2026-03-29T22:00:00Z"),
            )
        }
    }

    private fun mondayProfile(timezoneName: String): ScheduleCompositionProfileSnapshot =
        profile(
            timezoneName = timezoneName,
            enabledWeekday = ScheduleWeekday.MONDAY,
            windows = listOf(
                ScheduleLocalTimeWindow(7 * 60, 10 * 60),
                ScheduleLocalTimeWindow(11 * 60, 17 * 60),
            ),
            protectedWindows = listOf(ScheduleLocalTimeWindow(10 * 60, 11 * 60)),
            sleep = ScheduleSleepInterval(23 * 60, 6 * 60),
        )

    private fun sundayProfile(
        timezoneName: String,
        window: ScheduleLocalTimeWindow,
        sleep: ScheduleSleepInterval,
    ): ScheduleCompositionProfileSnapshot = profile(
        timezoneName = timezoneName,
        enabledWeekday = ScheduleWeekday.SUNDAY,
        windows = listOf(window),
        protectedWindows = emptyList(),
        sleep = sleep,
    )

    private fun profile(
        timezoneName: String,
        enabledWeekday: ScheduleWeekday,
        windows: List<ScheduleLocalTimeWindow>,
        protectedWindows: List<ScheduleLocalTimeWindow>,
        sleep: ScheduleSleepInterval,
    ): ScheduleCompositionProfileSnapshot = ScheduleCompositionProfileSnapshot(
        dayStartMinute = windows.minOf { it.startMinute },
        dayEndMinute = windows.maxOf { it.endMinute },
        timezoneName = timezoneName,
        availability = ScheduleWeekday.entries.map { weekday ->
            ScheduleAvailabilityDay(
                weekday = weekday,
                isEnabled = weekday == enabledWeekday,
                windows = windows.takeIf { weekday == enabledWeekday }.orEmpty(),
            )
        },
        sleep = sleep,
        protectedTime = ScheduleWeekday.entries.map { weekday ->
            ScheduleProtectedDay(
                weekday = weekday,
                isEnabled = weekday == enabledWeekday && protectedWindows.isNotEmpty(),
                windows = protectedWindows.takeIf { weekday == enabledWeekday }.orEmpty(),
            )
        },
    ).also { assertTrue(it.hasValidShape()) }
}
