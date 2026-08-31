package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.state.ScheduleCompositionProfileDraftMemory
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PlanningProfileFormTest {
    @Test
    fun profileRoundTripsThroughExplicit24HourFieldsIncludingEndOfDay() {
        val profile = ScheduleCompositionProfileSnapshot(
            dayStartMinute = 0,
            dayEndMinute = 24 * 60,
            slotGranularityMinutes = 60,
            stabilityWeight = 0,
            defaultSoftWeight = MAX_SCHEDULER_WEIGHT,
        )

        val form = PlanningProfileForm.from(profile)

        assertEquals("00", form.startHour)
        assertEquals("00", form.startMinute)
        assertEquals("24", form.endHour)
        assertEquals("00", form.endMinute)
        assertEquals(profile, form.validate().profile)
        assertEquals("24:00", formatPlanningProfileMinute(profile.dayEndMinute))
    }

    @Test
    fun validationRejectsMalformedOrReversedWorkWindows() {
        val reversed = PlanningProfileForm.from(ScheduleCompositionProfileSnapshot()).copy(
            startHour = "22",
            endHour = "07",
        ).validate()
        assertNull(reversed.profile)
        assertEquals("End must be later than start.", reversed.endError)

        val invalidEndOfDay = PlanningProfileForm.from(
            ScheduleCompositionProfileSnapshot(),
        ).copy(endHour = "24", endMinute = "01").validate()
        assertNull(invalidEndOfDay.profile)
        assertTrue(requireNotNull(invalidEndOfDay.endError).contains("24:00"))

        val midnightEnd = PlanningProfileForm.from(
            ScheduleCompositionProfileSnapshot(),
        ).copy(endHour = "00", endMinute = "00").validate()
        assertNull(midnightEnd.profile)
        assertTrue(requireNotNull(midnightEnd.endError).contains("00:01"))

        val oversizedPart = PlanningProfileForm.from(
            ScheduleCompositionProfileSnapshot(),
        ).copy(startHour = "007").validate()
        assertNull(oversizedPart.profile)
        assertTrue(oversizedPart.startError != null)
    }

    @Test
    fun validationAcceptsEveryNativeBoundaryAndRejectsOutOfRangeControls() {
        val minimum = PlanningProfileForm.from(ScheduleCompositionProfileSnapshot()).copy(
            slotGranularityMinutes = MIN_SLOT_GRANULARITY_MINUTES,
            stabilityWeight = MIN_SCHEDULER_WEIGHT.toString(),
            defaultSoftWeight = MIN_SCHEDULER_WEIGHT.toString(),
        ).validate()
        assertTrue(minimum.isValid)

        val maximum = PlanningProfileForm.from(ScheduleCompositionProfileSnapshot()).copy(
            slotGranularityMinutes = MAX_SLOT_GRANULARITY_MINUTES,
            stabilityWeight = MAX_SCHEDULER_WEIGHT.toString(),
            defaultSoftWeight = MAX_SCHEDULER_WEIGHT.toString(),
        ).validate()
        assertTrue(maximum.isValid)

        val invalid = PlanningProfileForm.from(ScheduleCompositionProfileSnapshot()).copy(
            slotGranularityMinutes = MAX_SLOT_GRANULARITY_MINUTES + 1,
            stabilityWeight = (MAX_SCHEDULER_WEIGHT + 1).toString(),
            defaultSoftWeight = "",
        ).validate()
        assertFalse(invalid.isValid)
        assertTrue(invalid.granularityError != null)
        assertTrue(invalid.stabilityWeightError != null)
        assertTrue(invalid.defaultSoftWeightError != null)
    }

    @Test
    fun numericInputsDiscardUnsupportedCharactersAndRemainBoundedInLength() {
        assertEquals("07", sanitizePlanningTimePart("0a75"))
        assertEquals("07", sanitizePlanningTimePart("0a7:5"))
        assertEquals("1000000", sanitizePlanningWeight("1,000,000-extra"))
    }

    @Test
    fun presentationBlocksLocalCompositionAndPrioritizesDurableUncertaintyReason() {
        assertEquals(
            PLANNING_PROFILE_ACTION_BUSY_MESSAGE,
            planningProfileEditBlockedMessage(
                state = DayWeaveUiState(),
                canonicalActionBusy = true,
            ),
        )
        assertNull(
            planningProfileEditBlockedMessage(
                state = DayWeaveUiState(),
                canonicalActionBusy = false,
            ),
        )

        val activeState = DayWeaveUiState(
            activeSession = ActiveSession(
                itemId = "local-item",
                elapsedMinutes = 1,
                isPaused = false,
            ),
        )
        assertEquals(
            "Finish or reconcile the current focus action first.",
            planningProfileEditBlockedMessage(activeState, canonicalActionBusy = true),
        )
    }

    @Test
    fun processMemorySnapshotRestoresEveryUnsavedFieldWithoutPuttingValuesInItsToken() {
        val draft = PlanningProfileForm(
            startHour = "08",
            startMinute = "17",
            endHour = "21",
            endMinute = "43",
            slotGranularityMinutes = 13,
            stabilityWeight = "765",
            defaultSoftWeight = "4321",
        )

        val baseline = ScheduleCompositionProfileSnapshot()
        val token = ScheduleCompositionProfileDraftMemory.retain(
            baseline = baseline,
            nextValues = draft.toDraftMemoryValues(),
        )

        assertEquals(token, UUID.fromString(token).toString())
        assertEquals(
            draft,
            ScheduleCompositionProfileDraftMemory.restore(token, baseline)
                ?.let(::planningProfileFormFromDraftMemoryValues),
        )
        assertNull(
            ScheduleCompositionProfileDraftMemory.restore(
                token,
                baseline.copy(dayStartMinute = 8 * 60),
            ),
        )
        assertNull(planningProfileFormFromDraftMemoryValues(listOf("incomplete")))
        assertNull(
            planningProfileFormFromDraftMemoryValues(
                draft.toDraftMemoryValues().toMutableList().also { it[4] = "not-a-number" },
            ),
        )
        ScheduleCompositionProfileDraftMemory.clear()
        assertNull(ScheduleCompositionProfileDraftMemory.restore(token, baseline))
    }
}
