package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalBreakCategory
import com.greengolddog.dayweave.model.CanonicalBufferPolicyDraft
import com.greengolddog.dayweave.model.CanonicalConstraintLevel
import com.greengolddog.dayweave.model.CanonicalConstraintStrengthDraft
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalEventTimingDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceKind
import com.greengolddog.dayweave.model.CanonicalRecurrencePeriod
import com.greengolddog.dayweave.model.CanonicalRecurrenceSemantics
import com.greengolddog.dayweave.model.CanonicalSchedulingConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalSplitDraft
import com.greengolddog.dayweave.model.CanonicalSplitKind
import com.greengolddog.dayweave.model.CanonicalWeekday
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalItemEditorFormTest {
    @Test
    fun formRoundTripsTheTypedDraftContractWithoutJsonEditing() {
        val draft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.ROUTINE,
            isSensitive = true,
            title = "Deep work block",
            notes = "Protect the first hour.",
            timezoneName = "UTC",
            durationSeconds = 3_600,
            earliestStartAt = "2026-08-31T08:00:00Z",
            deadlineAt = "2026-08-31T18:00:00Z",
            recurrence = CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.WEEKLY,
                occurrencesPerPeriod = 2,
                weekdays = listOf(CanonicalWeekday.MONDAY, CanonicalWeekday.WEDNESDAY),
            ),
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.DEEP,
                tags = listOf("focus", "work"),
                preferredStartMinute = 9 * 60,
                minimumGapMinutes = 15,
                maximumSessions = 2,
            ),
            split = CanonicalSplitDraft(
                kind = CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = 900,
                maximumChunkSeconds = 1_800,
            ),
            importance = 85,
            urgency = 65,
            parentId = PARENT_ID,
            siblingOrder = 7,
        )
        draft.requireValid(ITEM_ID)

        val rebuilt = CanonicalItemEditorForm.from(draft).draft(ITEM_ID).getOrThrow()

        assertEquals(draft.normalized(), rebuilt)
    }

    @Test
    fun inboxEventMayStayIncompleteButPlannedEventRequiresExactBounds() {
        val initial = newCanonicalDetailedDraft(
            title = "Appointment",
            kind = ItemKind.EVENT,
        )
        val blankForm = CanonicalItemEditorForm.from(initial)

        assertNull(blankForm.validationIssue(ITEM_ID))
        assertNotNull(
            blankForm.copy(placement = CanonicalDraftPlacement.PLANNED)
                .validationIssue(ITEM_ID),
        )
        assertEquals(null, initial.eventTiming)
        assertEquals(null, initial.earliestStartAt)
        assertEquals(null, initial.deadlineAt)

        val result = blankForm.copy(
            eventStart = "2026-08-31T09:00:00Z",
            eventEnd = "2026-08-31T10:30:00Z",
        ).draft(ITEM_ID).getOrThrow()

        assertEquals("2026-08-31T09:00:00Z", result.eventTiming?.startsAt)
        assertEquals("2026-08-31T10:30:00Z", result.eventTiming?.endsAt)
        assertEquals(5_400L, result.durationSeconds)
        assertEquals(result.eventTiming?.startsAt, result.earliestStartAt)
        assertEquals(result.eventTiming?.endsAt, result.deadlineAt)
    }

    @Test
    fun detailedHabitStartsWithReviewableTypedDailyRecurrence() {
        val draft = newCanonicalDetailedDraft("Morning walk", ItemKind.HABIT)

        assertNull(CanonicalItemEditorForm.from(draft).validationIssue(ITEM_ID))
        assertEquals(CanonicalRecurrenceKind.DAILY, draft.recurrence?.kind)
        assertEquals(1, draft.recurrence?.occurrencesPerPeriod)
    }

    @Test
    fun richControlsProduceTypedFrequencyConstraintsAndSplitExtensions() {
        val form = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Write chapter", ItemKind.TASK),
        ).copy(
            placement = CanonicalDraftPlacement.PLANNED,
            hasDuration = true,
            durationSeconds = "7200",
            recurrenceKind = CanonicalRecurrenceKind.FREQUENCY,
            recurrenceCount = "3",
            recurrencePeriod = CanonicalRecurrencePeriod.WEEK,
            recurrenceSemantics = CanonicalRecurrenceSemantics.CALENDAR,
            recurrenceMinimumSpacingMinutes = "1440",
            weekdays = setOf(
                CanonicalWeekday.MONDAY,
                CanonicalWeekday.WEDNESDAY,
                CanonicalWeekday.FRIDAY,
            ),
            energy = EnergyLevel.DEEP,
            energyStrength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
            tags = listOf("focus", "writing"),
            schedulingSpecified = true,
            constraintEarliest = CanonicalInstantConstraintForm(
                value = "2026-09-03T08:00:00+02:00",
                strength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
            ),
            constraintLatest = CanonicalInstantConstraintForm(
                value = "2026-09-30T18:00:00+02:00",
                strength = CanonicalStrengthForm(CanonicalConstraintLevel.SOFT, "250"),
            ),
            allowedWeekdays = CanonicalWeekday.entries.take(5).toSet(),
            allowedWeekdaysStrength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
            preferredDailyWindows = listOf(
                CanonicalDailyWindowForm(
                    weekdays = setOf(CanonicalWeekday.MONDAY),
                    startMinute = "540",
                    endMinute = "720",
                ),
            ),
            requiredContexts = listOf(
                CanonicalStringConstraintForm(
                    value = "computer",
                    strength = CanonicalStrengthForm(CanonicalConstraintLevel.HARD),
                ),
            ),
            requiredLocation = CanonicalStringConstraintForm(value = "home"),
            maximumDailyWork = CanonicalMinutesConstraintForm(value = "180"),
            bufferBeforeMinutes = "10",
            bufferAfterMinutes = "15",
            bufferSpecified = true,
            bufferStrength = CanonicalStrengthForm(
                CanonicalConstraintLevel.SOFT,
                "90",
            ),
            isSplittable = true,
            minimumChunkSeconds = "1800",
            maximumChunkSeconds = "3600",
            maximumSessions = "3",
            minimumGapMinutes = "30",
            maximumSplitDays = "2",
        )

        val draft = form.draft(ITEM_ID).getOrThrow()

        assertEquals(CanonicalRecurrenceKind.FREQUENCY, draft.recurrence?.kind)
        assertEquals(1_440L, draft.recurrence?.minimumSpacingMinutes)
        assertEquals(
            CanonicalConstraintStrengthDraft.hard(),
            draft.constraints.energyStrength,
        )
        assertEquals("computer", draft.constraints.scheduling?.requiredContexts?.single()?.value)
        assertEquals(15L, draft.constraints.scheduling?.buffers?.afterMinutes)
        assertEquals(3, draft.constraints.maximumSessions)
        assertEquals(2, draft.constraints.maximumSplitDays)
        assertEquals(
            draft,
            CanonicalItemEditorForm.from(draft).draft(ITEM_ID).getOrThrow(),
        )
    }

    @Test
    fun kindSpecificControlsProduceHabitRoutineGoalAndBreakMetadata() {
        val habit = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Hydrate", ItemKind.HABIT),
        ).copy(
            hasHabitTarget = true,
            habitTargetAmount = "8",
            habitTargetUnit = "glasses",
            preservesStreakWhenPaused = false,
            preservesStreakSpecified = true,
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(8L, habit.constraints.habitTarget?.amount)
        assertEquals(false, habit.constraints.preservesStreakWhenPaused)

        val routine = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Review", ItemKind.ROUTINE),
        ).copy(
            routineOrdered = true,
            routineOrderedSpecified = true,
            hasOwnEffort = true,
            hasOwnEffortSpecified = true,
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(true, routine.constraints.routineOrdered)
        assertEquals(true, routine.constraints.hasOwnEffort)

        val goal = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Book", ItemKind.GOAL),
        ).copy(
            goalMeasures = listOf(CanonicalGoalMeasureForm("chapters", "12", "3", "chapters")),
            goalMeasuresSpecified = true,
            hasGoalWeeklyAllocation = true,
            goalWeeklyMinimumMinutes = "120",
            goalWeeklyMaximumMinutes = "300",
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(12L, goal.constraints.goalMeasures?.single()?.target)
        assertEquals(300L, goal.constraints.goalWeeklyAllocation?.maximumMinutes)

        val breakDraft = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Move", ItemKind.BREAK),
        ).copy(
            breakCategory = CanonicalBreakCategory.MOVEMENT,
            breakCategorySpecified = true,
            breakMandatory = true,
            breakMandatorySpecified = true,
            breakPromptToResume = false,
            breakPromptSpecified = true,
        ).draft(ITEM_ID).getOrThrow()
        assertEquals(CanonicalBreakCategory.MOVEMENT, breakDraft.constraints.breakCategory)
        assertEquals(true, breakDraft.constraints.breakMandatory)
        assertEquals(false, breakDraft.constraints.breakPromptToResume)
    }

    @Test
    fun plannedUnknownDurationAndInboxHabitWithoutRecurrenceStayEditable() {
        val planned = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Estimate later", ItemKind.TASK),
        ).copy(
            placement = CanonicalDraftPlacement.PLANNED,
            hasDuration = false,
        ).draft(ITEM_ID)
        assertTrue(planned.isSuccess)
        assertNull(planned.getOrThrow().durationSeconds)

        val inboxHabit = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Maybe daily", ItemKind.HABIT),
        ).copy(recurrenceKind = null).draft(ITEM_ID)
        assertTrue(inboxHabit.isSuccess)
        assertNull(inboxHabit.getOrThrow().recurrence)
    }

    @Test
    fun tagContainingCommaAndUnqualifiedZeroBufferRoundTripLosslessly() {
        val draft = newCanonicalDetailedDraft("Metadata", ItemKind.TASK).copy(
            constraints = CanonicalFlexibleConstraintsDraft(
                tags = listOf("focus,writing"),
                scheduling = CanonicalSchedulingConstraintsDraft(
                    buffers = CanonicalBufferPolicyDraft(0, 0, null),
                ),
            ),
        )

        val form = CanonicalItemEditorForm.from(draft)
        val rebuilt = form.draft(ITEM_ID).getOrThrow()

        assertEquals(listOf("focus,writing"), form.tags)
        assertEquals(listOf("focus,writing"), rebuilt.constraints.tags)
        assertTrue(form.bufferSpecified)
        assertNull(rebuilt.constraints.scheduling?.buffers?.strength)
        assertEquals(0L, rebuilt.constraints.scheduling?.buffers?.beforeMinutes)
    }

    @Test
    fun retainedCustomRruleCannotBeCreatedChangedOrConvertedInTheForm() {
        val custom = newCanonicalDetailedDraft("Legacy recurrence", ItemKind.ROUTINE).copy(
            recurrence = CanonicalRecurrenceDraft(
                CanonicalRecurrenceKind.CUSTOM,
                rrule = "FREQ=MONTHLY;BYDAY=1MO,-1FR",
            ),
        )
        val retained = CanonicalItemEditorForm.from(custom)

        assertEquals(custom, retained.draft(ITEM_ID).getOrThrow())
        assertTrue(retained.copy(recurrenceRrule = "FREQ=DAILY").draft(ITEM_ID).isFailure)
        assertTrue(
            retained.copy(recurrenceKind = CanonicalRecurrenceKind.DAILY)
                .draft(ITEM_ID).isFailure,
        )
        val fresh = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("New", ItemKind.TASK),
        ).copy(
            recurrenceKind = CanonicalRecurrenceKind.CUSTOM,
            recurrenceRrule = "FREQ=DAILY",
        )
        assertTrue(fresh.draft(ITEM_ID).isFailure)
    }

    @Test
    fun fractionalEventBoundsDoNotInventAnIntegralDuration() {
        val result = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Precise event", ItemKind.EVENT),
        ).copy(
            eventStart = "2026-08-31T09:00:00.000001Z",
            eventEnd = "2026-08-31T10:00:00.000002Z",
        ).draft(ITEM_ID).getOrThrow()

        assertNull(result.durationSeconds)
        assertEquals(result.eventTiming?.startsAt, result.earliestStartAt)
        assertEquals(result.eventTiming?.endsAt, result.deadlineAt)

        val unchanged = result.copy(
            earliestStartAt = null,
            deadlineAt = null,
            durationSeconds = null,
            eventTiming = CanonicalEventTimingDraft(
                "2026-08-31T09:00:00.000001Z",
                "2026-08-31T10:00:00.000002Z",
            ),
        )
        val rebuilt = CanonicalItemEditorForm.from(unchanged).draft(ITEM_ID).getOrThrow()
        assertNull(rebuilt.durationSeconds)
        assertNull(rebuilt.earliestStartAt)
        assertNull(rebuilt.deadlineAt)
    }

    @Test
    fun incompleteInboxEventMetadataIsVisiblePreservedAndClearedOnlyExplicitly() {
        val source = newCanonicalDetailedDraft("Candidate meeting", ItemKind.EVENT).copy(
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.LOW,
                tags = listOf("family,calendar"),
                scheduling = CanonicalSchedulingConstraintsDraft(
                    buffers = CanonicalBufferPolicyDraft(5, 0, null),
                    includesNullOccurrenceWindow = true,
                ),
                hasOwnEffort = false,
            ),
        )
        source.requireValid(ITEM_ID)
        val form = CanonicalItemEditorForm.from(source)

        assertEquals(EnergyLevel.LOW, form.energy)
        assertEquals(listOf("family,calendar"), form.tags)
        assertTrue(form.bufferSpecified)
        assertTrue(form.includesNullOccurrenceWindow)
        assertEquals(source, form.draft(ITEM_ID).getOrThrow())

        val timed = form.copy(
            placement = CanonicalDraftPlacement.PLANNED,
            eventStart = "2026-08-31T09:00:00Z",
            eventEnd = "2026-08-31T10:00:00Z",
        )
        assertTrue(timed.draft(ITEM_ID).isFailure)
        val cleared = timed.withoutEventFlexibleMetadata().draft(ITEM_ID).getOrThrow()
        assertEquals(CanonicalFlexibleConstraintsDraft(), cleared.constraints)
        assertEquals(3_600L, cleared.durationSeconds)
        assertEquals("2026-08-31T09:00:00Z", cleared.eventTiming?.startsAt)
    }

    @Test
    fun longEventBoundsRemainAuthorableWithNoCanonicalDuration() {
        val result = CanonicalItemEditorForm.from(
            newCanonicalDetailedDraft("Long hold", ItemKind.EVENT),
        ).copy(
            placement = CanonicalDraftPlacement.PLANNED,
            eventStart = "2026-01-01T00:00:00Z",
            eventEnd = "2028-01-01T00:00:00Z",
        ).draft(ITEM_ID).getOrThrow()

        assertNull(result.durationSeconds)
        assertEquals("2028-01-01T00:00:00Z", result.eventTiming?.endsAt)
    }

    @Test
    fun legacyReviewRouteCarriesProvenanceIntoEditableNotes() {
        val source = InboxItem(
            id = "proposal-synthetic",
            isSensitive = true,
            title = "Suggested next action",
            source = InboxSource.EXTERNAL_PROPOSAL,
            detail = "Review this synthetic context before scheduling.",
        )

        val route = CanonicalItemEditorRoute.fromInbox(source)

        assertEquals(source.id, route.sourceInboxId)
        assertEquals(source.title, route.initialDraft.title)
        assertEquals(source.detail, route.initialDraft.notes)
        assertEquals(true, route.initialDraft.isSensitive)
        assertEquals(CanonicalItemEditorMode.CREATE, route.mode)
    }

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val PARENT_ID = "22222222-2222-4222-8222-222222222222"
    }
}
