package com.greengolddog.dayweave.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ScheduleProfileModelsTest {
    @Test
    fun legacyProfileRemainsValidAndUpgradesWithoutChangingWorkCapacity() {
        val legacy = ScheduleCompositionProfileSnapshot(
            dayStartMinute = 8 * 60,
            dayEndMinute = 18 * 60,
        )

        assertTrue(legacy.hasValidShape())
        assertFalse(legacy.usesWeeklySchedule)

        val upgraded = requireNotNull(legacy.upgradedToWeeklySchedule("GMT"))
        assertNotNull(upgraded)
        assertEquals("UTC", upgraded.timezoneName)
        assertEquals(legacy.dayStartMinute, upgraded.dayStartMinute)
        assertEquals(legacy.dayEndMinute, upgraded.dayEndMinute)
        assertEquals(ScheduleWeekday.entries, upgraded.availability?.map { it.weekday })
        assertTrue(requireNotNull(upgraded.availability).all { it.windows.single().startMinute == 480 })
        assertTrue(upgraded.hasValidShape())

        assertNull(
            ScheduleCompositionProfileSnapshot(
                dayStartMinute = 0,
                dayEndMinute = 24 * 60,
            ).upgradedToWeeklySchedule("UTC"),
        )
    }

    @Test
    fun richProfileAcceptsMultipleWindowsAndDisabledDaysInExactIsoOrder() {
        val profile = validProfile()

        assertTrue(profile.hasValidShape())
        assertEquals(2, profile.availability?.first()?.windows?.size)
        assertFalse(requireNotNull(profile.availability).last().isEnabled)
        assertEquals(10 * 60, profile.protectedTime?.first()?.windows?.single()?.startMinute)
    }

    @Test
    fun rejectsPartialUnknownUnorderedOrCompatibilityMismatchedProfiles() {
        val valid = validProfile()

        assertFalse(valid.copy(timezoneName = "Mars/Olympus_Mons").hasValidShape())
        assertFalse(valid.copy(sleep = null).hasValidShape())
        assertFalse(valid.copy(availability = valid.availability?.reversed()).hasValidShape())
        assertFalse(valid.copy(dayStartMinute = valid.dayStartMinute + 1).hasValidShape())
        assertFalse(
            valid.copy(
                availability = valid.availability?.toMutableList()?.also {
                    it[1] = it[1].copy(weekday = ScheduleWeekday.MONDAY)
                },
            ).hasValidShape(),
        )
    }

    @Test
    fun rejectsOverlapsOutsideWakingTimeAndExcessiveProtection() {
        val valid = validProfile()
        val monday = requireNotNull(valid.availability).first()
        val overlappingWork = monday.copy(
            windows = listOf(
                ScheduleLocalTimeWindow(7 * 60, 12 * 60),
                ScheduleLocalTimeWindow(11 * 60, 17 * 60),
            ),
        )
        assertFalse(
            valid.copy(
                availability = valid.availability.toMutableList().also { it[0] = overlappingWork },
            ).hasValidShape(),
        )

        val outsideWaking = monday.copy(
            windows = listOf(ScheduleLocalTimeWindow(5 * 60, 7 * 60)),
        )
        assertFalse(
            valid.copy(
                dayStartMinute = 5 * 60,
                availability = valid.availability.toMutableList().also { it[0] = outsideWaking },
            ).hasValidShape(),
        )

        val overlapsWork = requireNotNull(valid.protectedTime).first().copy(
            windows = listOf(ScheduleLocalTimeWindow(9 * 60, 11 * 60)),
        )
        assertFalse(
            valid.copy(
                protectedTime = valid.protectedTime.toMutableList().also { it[0] = overlapsWork },
            ).hasValidShape(),
        )

        val tooMuchProtection = requireNotNull(valid.protectedTime).first().copy(
            windows = listOf(ScheduleLocalTimeWindow(10 * 60, 19 * 60)),
        )
        assertFalse(
            valid.copy(
                protectedTime = valid.protectedTime.toMutableList().also {
                    it[0] = tooMuchProtection
                },
            ).hasValidShape(),
        )
    }

    private fun validProfile(): ScheduleCompositionProfileSnapshot {
        val availability = ScheduleWeekday.entries.map { weekday ->
            if (weekday == ScheduleWeekday.MONDAY) {
                ScheduleAvailabilityDay(
                    weekday,
                    isEnabled = true,
                    windows = listOf(
                        ScheduleLocalTimeWindow(7 * 60, 10 * 60),
                        ScheduleLocalTimeWindow(11 * 60, 17 * 60),
                    ),
                )
            } else {
                ScheduleAvailabilityDay(weekday, isEnabled = false, windows = emptyList())
            }
        }
        val protected = ScheduleWeekday.entries.map { weekday ->
            if (weekday == ScheduleWeekday.MONDAY) {
                ScheduleProtectedDay(
                    weekday,
                    isEnabled = true,
                    windows = listOf(ScheduleLocalTimeWindow(10 * 60, 11 * 60)),
                )
            } else {
                ScheduleProtectedDay(weekday, isEnabled = false, windows = emptyList())
            }
        }
        return ScheduleCompositionProfileSnapshot(
            dayStartMinute = 7 * 60,
            dayEndMinute = 17 * 60,
            timezoneName = "Europe/Paris",
            availability = availability,
            sleep = ScheduleSleepInterval(23 * 60, 6 * 60),
            protectedTime = protected,
        )
    }
}
