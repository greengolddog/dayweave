package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.SuggestionDisposition
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PlannerStoreTest {
    @Test
    fun blankQuickCaptureIsRejectedWithoutChangingInbox() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val before = store.state.value.inbox

        val accepted = store.quickCapture("   ", ItemKind.TASK)

        assertFalse(accepted)
        assertEquals(before, store.state.value.inbox)
    }

    @Test
    fun quickCaptureAddsReviewableInboxItemButDoesNotScheduleIt() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val scheduleBefore = store.state.value.schedule

        val accepted = store.quickCapture("Call the dentist", ItemKind.TASK)

        assertTrue(accepted)
        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals("Call the dentist", store.state.value.inbox.first().title)
        assertEquals(InboxSource.QUICK_CAPTURE, store.state.value.inbox.first().source)
        assertTrue(store.state.value.inbox.first().requiresReview)
    }

    @Test
    fun approvingExternalSuggestionCannotMutateSchedule() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val scheduleBefore = store.state.value.schedule
        val suggestion = store.state.value.suggestions.first()

        store.approveSuggestion(suggestion.id)

        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals(
            SuggestionDisposition.APPROVED_FOR_INBOX,
            store.state.value.suggestions.first { it.id == suggestion.id }.disposition,
        )
        val proposalDraft = store.state.value.inbox.first()
        assertEquals(InboxSource.EXTERNAL_PROPOSAL, proposalDraft.source)
        assertTrue(proposalDraft.requiresReview)
    }

    @Test
    fun rejectingSuggestionLeavesPlanUntouched() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val scheduleBefore = store.state.value.schedule
        val suggestion = store.state.value.suggestions.first()

        store.rejectSuggestion(suggestion.id)

        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals(
            SuggestionDisposition.REJECTED,
            store.state.value.suggestions.first { it.id == suggestion.id }.disposition,
        )
    }

    @Test
    fun startingAnotherItemMaintainsSingleActiveSession() {
        val store = PlannerStore(DayWeaveUiState.preview())

        store.startItem("scheduler-tests")

        val state = store.state.value
        assertEquals("scheduler-tests", state.activeSession?.itemId)
        assertEquals(1, state.schedule.count { it.status == ItemStatus.ACTIVE })
        assertEquals(ItemStatus.PAUSED, state.schedule.first { it.id == "architecture" }.status)
    }

    @Test
    fun pauseCanBeTimedAndResumeClearsPausePlan() {
        val store = PlannerStore(DayWeaveUiState.preview())

        store.pauseActive(15)

        assertTrue(store.state.value.activeSession?.isPaused == true)
        assertEquals("15 minute break", store.state.value.activeSession?.pauseLabel)
        assertEquals(ItemStatus.PAUSED, store.state.value.activeItem?.status)

        store.resumeActive()

        assertFalse(store.state.value.activeSession?.isPaused ?: true)
        assertNull(store.state.value.activeSession?.pauseLabel)
        assertEquals(ItemStatus.ACTIVE, store.state.value.activeItem?.status)
    }

    @Test
    fun willDoLaterEndsSessionAndMovesItemOneHour() {
        val store = PlannerStore(DayWeaveUiState.preview())
        val original = store.state.value.activeItem ?: error("Preview must have an active item")

        store.doActiveLater()

        val moved = store.state.value.schedule.first { it.id == original.id }
        assertNull(store.state.value.activeSession)
        assertEquals(ItemStatus.SCHEDULED, moved.status)
        assertEquals(original.startMinute + 60, moved.startMinute)
    }
}
