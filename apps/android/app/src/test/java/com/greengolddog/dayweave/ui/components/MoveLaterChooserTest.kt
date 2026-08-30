package com.greengolddog.dayweave.ui.components

import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.MoveLaterPlacementMode
import com.greengolddog.dayweave.model.ScheduleItem
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MoveLaterChooserTest {
    @Test
    fun presetsRoundForwardAndKeepTomorrowMorningInTheLocalZone() {
        val now = Instant.parse("2026-08-30T18:30:20Z")
        val presets = moveLaterPresets(now, ZoneId.of("Europe/Madrid"), true)

        assertEquals(listOf("In 1 hour", "In 3 hours", "Tomorrow morning"), presets.map { it.label })
        assertEquals(Instant.parse("2026-08-30T19:31:00Z"), presets[0].moveStart)
        assertEquals(Instant.parse("2026-08-30T21:31:00Z"), presets[1].moveStart)
        assertEquals(Instant.parse("2026-08-31T07:00:00Z"), presets[2].moveStart)
    }

    @Test
    fun presetDetailsFollowTheSelectedClockConvention() {
        val now = Instant.parse("2026-08-30T18:30:20Z")
        val zone = ZoneId.of("Europe/Madrid")

        val twentyFourHour = moveLaterPresets(now, zone, true).first().detail
        val twelveHour = moveLaterPresets(now, zone, false).first().detail

        org.junit.Assert.assertTrue(twentyFourHour.contains("21:31"))
        org.junit.Assert.assertTrue(twelveHour.contains("9:31"))
        org.junit.Assert.assertFalse(twelveHour.contains("21:31"))
    }

    @Test
    fun loadedPlanningDayFiltersTomorrowAndAnyCrossDayPreset() {
        val zone = ZoneId.of("Europe/Madrid")
        val now = Instant.parse("2026-08-30T18:30:20Z")

        val presets = moveLaterPresets(
            now = now,
            zoneId = zone,
            use24HourFormat = true,
            loadedPlanningDate = LocalDate.of(2026, 8, 30),
        )

        assertEquals(listOf("In 1 hour", "In 3 hours"), presets.map { it.label })
    }

    @Test
    fun customSelectionRejectsNearPastAndReturnsAnExactMinute() {
        val zone = ZoneId.of("Europe/Madrid")
        val now = Instant.parse("2026-08-30T18:30:20Z")

        assertNull(
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(20, 31),
                zone,
                now,
            ),
        )
        assertEquals(
            Instant.parse("2026-08-30T19:45:00Z"),
            customMoveStart(
                LocalDate.of(2026, 8, 30),
                LocalTime.of(21, 45, 59),
                zone,
                now,
            ),
        )
    }

    @Test
    fun customSelectionRejectsDstGapAndOverlapInsteadOfSilentlyNormalizing() {
        val zone = ZoneId.of("Europe/Madrid")
        val now = Instant.parse("2026-01-01T00:00:00Z")

        assertNull(
            customMoveStart(
                LocalDate.of(2026, 3, 29),
                LocalTime.of(2, 30),
                zone,
                now,
            ),
        )
        assertNull(
            customMoveStart(
                LocalDate.of(2026, 10, 25),
                LocalTime.of(2, 30),
                zone,
                now,
            ),
        )
    }

    @Test
    fun sensitiveConflictIsRedactedAndForcesASecureWarningWindow() {
        val public = conflict("Team meeting", isSensitive = false)
        val sensitive = conflict("Private diagnosis", isSensitive = true)

        val presentation = moveLaterConflictPresentation(listOf(public, sensitive))

        assertEquals(listOf("Team meeting", "Sensitive busy time"), presentation.labels)
        org.junit.Assert.assertFalse(presentation.labels.any { "diagnosis" in it })
        org.junit.Assert.assertTrue(presentation.requiresSecureWindow)
    }

    @Test
    fun sensitiveMovedItemForcesSecureChooserWithoutAnyConflict() {
        val noConflicts = moveLaterConflictPresentation(emptyList())

        org.junit.Assert.assertTrue(
            moveLaterRequiresSecureWindow(itemIsSensitive = true, conflicts = noConflicts),
        )
        org.junit.Assert.assertFalse(
            moveLaterRequiresSecureWindow(itemIsSensitive = false, conflicts = noConflicts),
        )
    }

    @Test
    fun oneShotCopyPromisesOnlyAnEarliestSchedulingTime() {
        assertEquals(
            "DayWeave will allow scheduling this work from the selected time, then recompose your day.",
            moveLaterChooserExplanation(MoveLaterPlacementMode.EARLIEST_START),
        )
        assertEquals(
            "DayWeave will preserve the earliest start and deadline change you approve.",
            moveLaterConfirmationPromise(MoveLaterPlacementMode.EARLIEST_START),
        )
        org.junit.Assert.assertFalse(
            moveLaterChooserExplanation(MoveLaterPlacementMode.EARLIEST_START).contains("exact"),
        )
    }

    @Test
    fun recurrenceCopyPromisesRecompositionInsideAWindowNotExactLeaves() {
        val explanation = moveLaterChooserExplanation(
            MoveLaterPlacementMode.RECOMPOSED_WINDOW,
        )

        org.junit.Assert.assertTrue(explanation.contains("recompose"))
        org.junit.Assert.assertTrue(explanation.contains("window"))
        org.junit.Assert.assertFalse(explanation.contains("exact"))
    }

    private fun conflict(title: String, isSensitive: Boolean) = ScheduleItem(
        id = title,
        title = title,
        kind = ItemKind.EVENT,
        startMinute = 9 * 60,
        durationMinutes = 30,
        status = ItemStatus.SCHEDULED,
        isSensitive = isSensitive,
        isFlexible = false,
        isHardConstraint = true,
    )
}
