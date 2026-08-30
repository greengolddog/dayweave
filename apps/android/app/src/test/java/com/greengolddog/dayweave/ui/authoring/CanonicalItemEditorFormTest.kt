package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceKind
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
    fun eventRequiresUserSuppliedExactBoundsAndDerivesNoInventedTime() {
        val initial = newCanonicalDetailedDraft(
            title = "Appointment",
            kind = ItemKind.EVENT,
        )
        val blankForm = CanonicalItemEditorForm.from(initial)

        assertNotNull(blankForm.validationIssue(ITEM_ID))
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
