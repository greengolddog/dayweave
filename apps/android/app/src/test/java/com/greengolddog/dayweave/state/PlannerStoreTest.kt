package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PlannerStoreTest {
    @Test
    fun exactRemoteGenerationWaitsForItsOwnSaveWhileLaterUiMutationStaysNonBlocking() =
        runBlocking {
            val initial = DayWeaveUiState()
            val saveStarted = Channel<DayWeaveUiState>(Channel.UNLIMITED)
            val allowSave = Channel<Unit>(Channel.UNLIMITED)
            val repository = object : PlannerStateRepository {
                override suspend fun load(): DayWeaveUiState = initial

                override suspend fun save(state: DayWeaveUiState) {
                    saveStarted.send(state)
                    allowSave.receive()
                }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

            try {
                val store = PlannerStore(initial, repository, scope)
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }

                val receipt = requireNotNull(
                    store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
                )
                val durable = async { receipt.awaitDurable() }
                val exactSnapshot = withTimeout(3_000) { saveStarted.receive() }

                assertFalse(durable.isCompleted)
                assertTrue(store.quickCapture("Later UI edit", ItemKind.TASK))
                assertTrue(exactSnapshot.suggestions.any { it.id == "remote-proposal" })
                assertFalse(exactSnapshot.inbox.any { it.title == "Later UI edit" })

                allowSave.send(Unit)
                assertTrue(withTimeout(3_000) { durable.await() })
                val laterSnapshot = withTimeout(3_000) { saveStarted.receive() }
                assertTrue(laterSnapshot.inbox.any { it.title == "Later UI edit" })
                allowSave.send(Unit)
            } finally {
                scope.cancel()
            }
        }

    @Test
    fun exactRemoteGenerationReportsSaveFailure() = runBlocking {
        val initial = DayWeaveUiState()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                throw IllegalStateException("synthetic encrypted save failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val receipt = requireNotNull(
                store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
            )

            assertFalse(withTimeout(3_000) { receipt.awaitDurable() })
            assertEquals(
                PlannerLoadState.PERSISTENCE_FAILED,
                withTimeout(3_000) {
                    store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED }
                },
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun cancellingOneWaiterDoesNotCancelTheExactEncryptedSave() = runBlocking {
        val initial = DayWeaveUiState()
        val saveStarted = CompletableDeferred<Unit>()
        val allowSave = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveStarted.complete(Unit)
                allowSave.await()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val receipt = requireNotNull(
                store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
            )
            val firstWaiter = async { receipt.awaitDurable() }
            withTimeout(3_000) { saveStarted.await() }

            firstWaiter.cancelAndJoin()
            allowSave.complete(Unit)

            assertTrue(withTimeout(3_000) { receipt.awaitDurable() })
            assertEquals(PlannerLoadState.READY, store.loadState.value)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun cancelledRepositorySaveFailsItsAcknowledgementInsteadOfHanging() = runBlocking {
        val initial = DayWeaveUiState()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                throw CancellationException("synthetic repository cancellation")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val receipt = requireNotNull(
                store.replaceRemoteSuggestions(listOf(remoteSuggestion())),
            )

            assertFalse(withTimeout(3_000) { receipt.awaitDurable() })
            assertEquals(
                PlannerLoadState.PERSISTENCE_FAILED,
                withTimeout(3_000) {
                    store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED }
                },
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun restoreBlocksInputUntilPersistedStateIsReadyAndThenAutosaves() = runBlocking {
        val restoredState = DayWeaveUiState.preview().copy(
            protectedFreeMinutes = 37,
            scheduleMessage = "Restored from disk",
        )
        val allowLoad = CompletableDeferred<Unit>()
        val savedStates = Channel<DayWeaveUiState>(Channel.UNLIMITED)
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState {
                allowLoad.await()
                return restoredState
            }

            override suspend fun save(state: DayWeaveUiState) {
                savedStates.send(state)
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState.preview(),
                repository = repository,
                scope = scope,
            )

            assertEquals(PlannerLoadState.LOADING, store.loadState.value)
            assertFalse(store.quickCapture("Capture during restore", ItemKind.TASK))
            allowLoad.complete(Unit)

            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertEquals(restoredState, store.state.value)
            assertTrue(store.quickCapture("Capture after restore", ItemKind.TASK))
            val savedState = withTimeout(3_000) { savedStates.receive() }

            assertEquals(store.state.value, savedState)
            assertEquals("Capture after restore", savedState.inbox.first().title)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun persistenceFailureBecomesReadOnlyWithoutReplacingVisibleState() = runBlocking {
        val initial = DayWeaveUiState.preview()
        val failure = IllegalStateException("synthetic encrypted storage failure")
        val reported = CompletableDeferred<Throwable>()
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = throw failure

            override suspend fun save(state: DayWeaveUiState) = Unit
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

        try {
            val store = PlannerStore(
                initialState = initial,
                repository = repository,
                scope = scope,
                onPersistenceError = { reported.complete(it) },
            )

            withTimeout(3_000) {
                store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED }
            }
            assertEquals(failure, reported.await())
            assertEquals(initial, store.state.value)
            assertFalse(store.quickCapture("Must not be accepted", ItemKind.TASK))
            assertEquals(initial, store.state.value)
        } finally {
            scope.cancel()
        }
    }

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

    private fun remoteSuggestion() = PlanningSuggestion(
        id = "remote-proposal",
        title = "Protect recovery time",
        summary = "Keep an hour open",
        source = "Codex",
        kind = SuggestionKind.SCHEDULE_CHANGE,
        expiresInDays = 7,
        remoteRevision = 1,
        remotePayloadJson = "{}",
    )
}
