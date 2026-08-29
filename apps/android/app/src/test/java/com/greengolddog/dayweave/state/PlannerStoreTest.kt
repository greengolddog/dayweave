package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import java.time.Instant
import java.util.UUID
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
import org.junit.Assert.assertNotNull
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

        val accepted = store.quickCapture(
            title = "Call the dentist",
            kind = ItemKind.TASK,
            isSensitive = true,
        )

        assertTrue(accepted)
        assertEquals(scheduleBefore, store.state.value.schedule)
        assertEquals("Call the dentist", store.state.value.inbox.first().title)
        assertEquals(InboxSource.QUICK_CAPTURE, store.state.value.inbox.first().source)
        assertTrue(store.state.value.inbox.first().requiresReview)
        assertTrue(store.state.value.inbox.first().isSensitive)
    }

    @Test
    fun ancestorSensitivityAcknowledgementImmediatelyProtectsCachedDescendantBlocks() {
        val parentId = "77777777-7777-4777-8777-777777777777"
        val parent = canonicalItem("planned", 1).copy(
            id = parentId,
            title = "SYNTHETIC-PRIVATE-PARENT",
            isExecutable = false,
        )
        val child = canonicalItem("planned", 1).copy(
            id = CANONICAL_ITEM_ID,
            title = "SYNTHETIC-PRIVATE-CHILD",
            parentId = parentId,
        )
        val block = canonicalBlock(ItemStatus.SCHEDULED, 1).copy(isSensitive = false)
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(parent, child),
                schedule = listOf(block),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
            ),
        )
        val pending = PendingCanonicalMutation(
            idempotencyKey = "88888888-8888-4888-8888-888888888888",
            syncOrigin = CANONICAL_ORIGIN,
            configurationId = "connection-1",
            itemId = parentId,
            expectedRevision = 1,
            targetStatus = "planned",
            targetIsSensitive = true,
            startedAt = "2026-08-29T08:00:00Z",
            replacementRequestJson = "{}",
            focusedBlockId = parentId,
            displayStatus = ItemStatus.SCHEDULED,
        )

        assertNotNull(store.stageCanonicalMutation(pending))
        assertTrue(store.state.value.schedule.single().isSensitive)
        val restarted = PlannerStore(
            store.state.value.copy(schedule = listOf(block.copy(isSensitive = false))),
        )
        assertTrue(restarted.state.value.schedule.single().isSensitive)
        assertTrue(
            requireNotNull(restarted.state.value.pendingCanonicalMutation).targetIsSensitive,
        )
        assertNotNull(
            store.reconcileCanonicalItemSensitivity(
                parent.copy(
                    isSensitive = true,
                    revision = 2,
                    updatedAt = "2026-08-29T08:01:00Z",
                ),
            ),
        )

        val current = store.state.value
        assertTrue(current.canonicalItems.first { it.id == parentId }.isSensitive)
        assertFalse(current.canonicalItems.first { it.id == CANONICAL_ITEM_ID }.isSensitive)
        assertTrue(current.schedule.single().isSensitive)
        assertEquals(1L, current.schedule.single().canonicalRevision)
        assertNull(current.pendingCanonicalMutation)
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
    fun elapsedTimerAndTimedPauseUseMonotonicExecutionFields() {
        var now = 0L
        val block = ScheduleItem(
            id = "timed",
            title = "Timed focus",
            kind = ItemKind.TASK,
            startMinute = 9 * 60,
            durationMinutes = 30,
            status = ItemStatus.ACTIVE,
        )
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                schedule = listOf(block),
                activeSession = ActiveSession(
                    itemId = block.id,
                    elapsedMinutes = 0,
                    isPaused = false,
                    accumulatedSeconds = 0,
                    runningSinceEpochMillis = 0,
                ),
            ),
            nowEpochMillis = { now },
        )

        now = 61_000
        assertTrue(store.tickActiveSession())
        assertEquals(1, store.state.value.activeSession?.elapsedMinutes)
        store.pauseActive(1)
        assertFalse(store.timedPauseReady())

        now = 121_000
        assertTrue(store.timedPauseReady())
        assertTrue(store.tickActiveSession())
        assertTrue(store.state.value.activeSession?.timedBreakEnded == true)
        assertTrue(store.state.value.activeSession?.isPaused == true)

        store.pauseActive(1)
        assertFalse(store.state.value.activeSession?.timedBreakEnded ?: true)
        assertFalse(store.timedPauseReady())
        now = 181_000
        assertTrue(store.tickActiveSession())
        assertTrue(store.state.value.activeSession?.timedBreakEnded == true)
        store.resumeActive()
        now = 241_000
        store.tickActiveSession()

        assertEquals(2, store.state.value.activeSession?.elapsedMinutes)
        assertFalse(store.state.value.activeSession?.isPaused ?: true)
        assertNull(store.state.value.activeSession?.pauseUntilEpochMillis)
    }

    @Test
    fun authoritativePlanStatusTransitionsPreserveAContinuouslyCorrectTimer() {
        var now = 0L
        val activeItem = canonicalItem(status = "in_progress", revision = 7)
        val activeBlock = canonicalBlock(ItemStatus.ACTIVE, revision = 7)
        val store = PlannerStore(
            initialState = DayWeaveUiState(
                canonicalItems = listOf(activeItem),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(activeBlock),
                activeSession = ActiveSession(
                    itemId = CANONICAL_BLOCK_ID,
                    elapsedMinutes = 0,
                    isPaused = false,
                    accumulatedSeconds = 0,
                    runningSinceEpochMillis = 0,
                ),
            ),
            nowEpochMillis = { now },
        )

        now = 61_000
        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = canonicalItem(status = "paused", revision = 8),
                block = canonicalBlock(ItemStatus.PAUSED, revision = 8),
                cursor = "cursor-1",
            ),
        )

        val paused = requireNotNull(store.state.value.activeSession)
        assertTrue(paused.isPaused)
        assertEquals(61L, paused.accumulatedSeconds)
        assertEquals(1, paused.elapsedMinutes)
        assertNull(paused.runningSinceEpochMillis)

        now = 121_000
        store.replaceCanonicalPlan(
            canonicalUpdate(
                item = canonicalItem(status = "in_progress", revision = 9),
                block = canonicalBlock(ItemStatus.ACTIVE, revision = 9),
                cursor = "cursor-2",
            ),
        )
        assertFalse(requireNotNull(store.state.value.activeSession).isPaused)
        assertEquals(121_000L, store.state.value.activeSession?.runningSinceEpochMillis)

        now = 181_000
        store.tickActiveSession()
        assertEquals(2, store.state.value.activeSession?.elapsedMinutes)
        assertEquals(61L, store.state.value.activeSession?.accumulatedSeconds)
    }

    @Test
    fun confirmedCompleteSurvivesFreshScheduledCompositionAndRestartFence() {
        assertTerminalExecutionSurvivesComposition(
            wireStatus = "completed",
            displayStatus = ItemStatus.COMPLETED,
        )
    }

    @Test
    fun confirmedSkipSurvivesFreshScheduledCompositionAndRestartFence() {
        assertTerminalExecutionSurvivesComposition(
            wireStatus = "skipped",
            displayStatus = ItemStatus.SKIPPED,
        )
    }

    @Test
    fun compositionUsesNewestTerminalSessionForTheSameProjectionTarget() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val older = executionSession(status = "active", revision = 1).copy(
            id = "33333333-3333-4333-8333-333333333333",
            status = "skipped",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        val newer = executionSession(status = "active", revision = 1).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:02:00Z",
            updatedAt = "1970-01-01T01:02:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                older,
                message = "Older skip",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                4,
                null,
                newer,
                message = "Newer completion",
            ),
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-1",
                ),
            ),
        )

        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertEquals(2, store.state.value.terminalExecutionOutcomes.size)
    }

    @Test
    fun compositionKeepsANewerOpenLeaseAboveOlderTerminalHistory() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val terminal = executionSession(status = "active", revision = 1).copy(
            id = "33333333-3333-4333-8333-333333333333",
            status = "completed",
            revision = 2,
            accumulatedSeconds = 30,
            actualSeconds = 30,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        val active = executionSession(status = "active", revision = 1).copy(
            startedAt = "1970-01-01T01:02:00Z",
            runningSince = "1970-01-01T01:02:00Z",
            createdAt = "1970-01-01T01:02:00Z",
            updatedAt = "1970-01-01T01:02:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                terminal,
                message = "Older completion",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                3,
                active,
                message = "New active lease",
            ),
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-1",
                ),
            ),
        )

        assertEquals(ItemStatus.ACTIVE, store.state.value.schedule.single().status)
        assertEquals(EXECUTION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
    }

    @Test
    fun firstCanonicalPlanRetainsExecutionHistoryAlreadyBoundToTheSameCredentials() {
        val store = PlannerStore()
        val terminal = executionSession(status = "active", revision = 1).copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Execution bootstrapped first",
            ),
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 7),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7),
                    cursor = "cursor-1",
                ).copy(configurationId = "connection-1"),
            ),
        )

        assertTrue(EXECUTION_ID in store.state.value.terminalExecutionOutcomes)
        assertFalse(
            store.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .requiresCanonicalItemProjection,
        )
        assertNull(
            store.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID).session
                .canonicalProjectionEligibleAtLeaseStart,
        )
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
    }

    @Test
    fun leaseEligibilitySurvivesRevisionAdvanceBeforeTerminalHistoryArrives() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val running = executionSession(status = "active", revision = 1, projectionEligible = true)
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = running,
                message = "Running",
            ),
        )
        assertEquals(
            true,
            store.state.value.canonicalExecutionSession
                ?.canonicalProjectionEligibleAtLeaseStart,
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 8),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 8),
                    cursor = "cursor-1",
                ),
            ),
        )
        val terminal = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:01:30Z",
            updatedAt = "1970-01-01T01:01:30Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Ended",
            ),
        )

        val outcome = store.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
        assertTrue(outcome.requiresCanonicalItemProjection)
        assertEquals(7L, outcome.session.itemRevision)
        assertEquals(true, outcome.session.canonicalProjectionEligibleAtLeaseStart)
        assertTrue(store.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 8),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 8),
                    cursor = "cursor-2",
                ),
            ),
        )
        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
    }

    @Test
    fun remoteLeaseFromOlderItemRevisionGetsAnActionableDurablePlaceholder() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 8)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 8)),
                schedulePlanningZoneId = "UTC",
            ),
        )
        val remote = executionSession(status = "active", revision = 1)

        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = remote,
                message = "Remote lease",
            ),
        )

        val placeholder = requireNotNull(store.state.value.activeItem)
        assertEquals(EXECUTION_ID, placeholder.id)
        assertEquals(7L, placeholder.canonicalRevision)
        assertEquals("remote_execution_lease", placeholder.canonicalBlockKind)
        assertEquals(ItemStatus.ACTIVE, placeholder.status)
        assertEquals(EXECUTION_ID, store.state.value.activeSession?.canonicalExecutionSessionId)
        assertNull(
            store.state.value.canonicalExecutionSession
                ?.canonicalProjectionEligibleAtLeaseStart,
        )

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 9),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 9),
                    cursor = "cursor-new",
                ).copy(configurationId = "connection-1"),
            ),
        )
        assertEquals(EXECUTION_ID, store.state.value.activeItem?.id)
        assertEquals(2, store.state.value.schedule.size)
    }

    @Test
    fun keepLatestResolutionSurvivesRestartAndSuppressesSameRevisionHistoryOverlay() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val running = executionSession(status = "active", revision = 1, projectionEligible = true)
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                1,
                running,
                message = "Running",
            ),
        )
        val terminal = running.copy(
            status = "completed",
            revision = 2,
            accumulatedSeconds = 60,
            actualSeconds = 60,
            runningSince = null,
            endedAt = "1970-01-01T01:01:00Z",
            updatedAt = "1970-01-01T01:01:00Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                terminal,
                message = "Ended",
            ),
        )
        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-1",
                ).copy(
                    schedule = emptyList(),
                    unscheduledItemCount = 1,
                    unscheduledWork = listOf(
                        UnscheduledWorkSnapshot(
                            itemId = CANONICAL_ITEM_ID,
                            remainingMinutes = 60,
                            reason = "capacity",
                        ),
                    ),
                ),
            ),
        )
        requireNotNull(
            store.recordTerminalProjectionConflict(
                EXECUTION_ID,
                "The same-revision item is only partially scheduled.",
            ),
        )
        requireNotNull(store.keepLatestItemAfterTerminalConflict(EXECUTION_ID))

        val restarted = PlannerStore(
            store.state.value.copy(
                canonicalConfigurationId = "connection-1",
                canonicalExecutionHistoryContinuityEstablished = true,
                canonicalExecutionHistoryVerified = true,
            ),
        )
        requireNotNull(
            restarted.replaceCanonicalPlan(
                canonicalUpdate(
                    canonicalItem("planned", 7),
                    canonicalBlock(ItemStatus.SCHEDULED, 7),
                    "cursor-2",
                ).copy(configurationId = "connection-1"),
            ),
        )
        assertEquals(ItemStatus.SCHEDULED, restarted.state.value.schedule.single().status)
        assertFalse(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        requireNotNull(
            restarted.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                3,
                null,
                terminal,
                message = "History refreshed",
            ),
        )
        assertEquals(ItemStatus.SCHEDULED, restarted.state.value.schedule.single().status)
        assertFalse(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
        assertEquals(
            "user_kept_latest_item",
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionResolution,
        )
    }

    @Test
    fun retryAuthorizationIsDurableAndOneShot() {
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                canonicalDeltaCursor = "cursor-0",
                schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
            ),
        )
        val running = executionSession(status = "active", revision = 1, projectionEligible = true)
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                1,
                running,
                message = "Running",
            ),
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                CANONICAL_ORIGIN,
                "connection-1",
                2,
                null,
                running.copy(
                    status = "completed",
                    revision = 2,
                    accumulatedSeconds = 60,
                    actualSeconds = 60,
                    runningSince = null,
                    endedAt = "1970-01-01T01:01:00Z",
                    updatedAt = "1970-01-01T01:01:00Z",
                ),
                message = "Ended",
            ),
        )
        requireNotNull(
            store.recordTerminalProjectionConflict(EXECUTION_ID, "Approval is required."),
        )
        requireNotNull(store.authorizeTerminalProjectionRetry(EXECUTION_ID))

        val restarted = PlannerStore(store.state.value)
        assertNotNull(
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionRetryAuthorizedAt,
        )
        requireNotNull(
            restarted.recordTerminalProjectionConflict(EXECUTION_ID, "Approval is required."),
        )
        assertNull(
            restarted.state.value.terminalExecutionOutcomes.getValue(EXECUTION_ID)
                .canonicalProjectionRetryAuthorizedAt,
        )
    }

    @Test
    fun terminalLedgerNeverEvictsAnImmutableSessionOutcome() {
        val unresolvedSessionId = UUID.nameUUIDFromBytes("unresolved".toByteArray()).toString()
        val initial = DayWeaveUiState(
            canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
        )
        val store = PlannerStore(initial)
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminalExecution(
                    sessionId = unresolvedSessionId,
                    itemId = CANONICAL_ITEM_ID,
                    endedAt = "1970-01-01T00:00:01Z",
                ),
                message = "Unresolved",
            ),
        )
        val base = Instant.parse("1970-01-02T00:00:00Z")
        val historyIds = (0 until 256).map { index ->
            val sessionId = UUID.nameUUIDFromBytes("history-session-$index".toByteArray()).toString()
            val itemId = UUID.nameUUIDFromBytes("history-item-$index".toByteArray()).toString()
            requireNotNull(
                store.reconcileCanonicalExecution(
                    syncOrigin = CANONICAL_ORIGIN,
                    configurationId = "connection-1",
                    revision = 4L + index * 2L,
                    activeSession = null,
                    changedSession = terminalExecution(
                        sessionId = sessionId,
                        itemId = itemId,
                        endedAt = base.plusSeconds(maxOf(0, index - 1).toLong()).toString(),
                    ),
                    message = "History",
                ),
            )
            sessionId
        }

        val retained = store.state.value.terminalExecutionOutcomes
        assertEquals(257, retained.size)
        assertTrue(unresolvedSessionId in retained)
        assertTrue(historyIds.first() in retained)
        assertTrue(historyIds.last() in retained)
        assertTrue(retained.values.all { it.session.revision == 2L })
    }

    @Test
    fun terminalSplitDoesNotResolveOccurrenceWhileAuthoritativeMinutesRemainUnscheduled() {
        val occurrenceId = "66666666-6666-4666-8666-666666666666"
        val block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7).copy(
            occurrenceId = occurrenceId,
            durationMinutes = 30,
            isSplittable = true,
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(
                    canonicalItem(status = "planned", revision = 7).copy(
                        recurrenceJson = "{\"frequency\":\"daily\"}",
                        splitPolicyJson = "{\"type\":\"splittable\"}",
                    ),
                ),
                canonicalSyncOrigin = CANONICAL_ORIGIN,
                canonicalConfigurationId = "connection-1",
                schedule = listOf(block),
                unscheduledWork = listOf(
                    UnscheduledWorkSnapshot(
                        itemId = CANONICAL_ITEM_ID,
                        occurrenceId = occurrenceId,
                        remainingMinutes = 90,
                        reason = "capacity",
                    ),
                ),
                occurrenceSeriesItemIds = mapOf(occurrenceId to CANONICAL_ITEM_ID),
            ),
        )
        val terminal = executionSession(status = "active", revision = 1).copy(
            occurrenceId = occurrenceId,
            plannedBlockId = CANONICAL_BLOCK_ID,
            status = "completed",
            revision = 2,
            accumulatedSeconds = 1_800,
            actualSeconds = 1_800,
            runningSince = null,
            endedAt = "1970-01-01T01:30:00Z",
            updatedAt = "1970-01-01T01:30:00Z",
        )

        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "One split ended",
            ),
        )

        assertEquals(ItemStatus.COMPLETED, store.state.value.schedule.single().status)
        assertFalse(occurrenceId in store.state.value.recurrenceOutcomes)
        assertFalse(CANONICAL_ITEM_ID in store.state.value.recurrenceCompletionAnchors)
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

    private fun canonicalItem(status: String, revision: Long) = CanonicalItemSnapshot(
        id = CANONICAL_ITEM_ID,
        kind = "task",
        status = status,
        title = "Canonical timer",
        timezoneName = "UTC",
        durationSeconds = 3_600,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = revision,
        createdAt = "1970-01-01T00:00:00Z",
        updatedAt = "1970-01-01T00:00:00Z",
    )

    private fun canonicalBlock(status: ItemStatus, revision: Long) = ScheduleItem(
        id = CANONICAL_BLOCK_ID,
        title = "Canonical timer",
        kind = ItemKind.TASK,
        startMinute = 60,
        durationMinutes = 60,
        status = status,
        canonicalItemId = CANONICAL_ITEM_ID,
        canonicalRevision = revision,
        absoluteStartAt = "1970-01-01T01:00:00Z",
        absoluteEndAt = "1970-01-01T02:00:00Z",
        planningZoneId = "UTC",
        canonicalBlockKind = "planned",
    )

    private fun canonicalUpdate(
        item: CanonicalItemSnapshot,
        block: ScheduleItem,
        cursor: String,
    ) = CanonicalPlanUpdate(
        items = listOf(item),
        schedule = listOf(block),
        syncOrigin = CANONICAL_ORIGIN,
        configurationId = "connection-1",
        deltaCursor = cursor,
        inputDigest = "sha256:${"a".repeat(64)}",
        generatedAt = "1970-01-01T00:00:00Z",
        planningZoneId = "UTC",
        rejectedItemCount = 0,
        unscheduledItemCount = 0,
        protectedFreeMinutes = 0,
        dayScore = 100,
        violationMessages = emptyList(),
        violationCount = 0,
        errorViolationCount = 0,
        unscheduledWork = emptyList(),
        occurrenceSeriesItemIds = emptyMap(),
        message = "Updated",
    )

    private fun assertTerminalExecutionSurvivesComposition(
        wireStatus: String,
        displayStatus: ItemStatus,
    ) {
        val initial = DayWeaveUiState(
            canonicalItems = listOf(canonicalItem(status = "planned", revision = 7)),
            canonicalSyncOrigin = CANONICAL_ORIGIN,
            canonicalConfigurationId = "connection-1",
            canonicalDeltaCursor = "cursor-0",
            schedule = listOf(canonicalBlock(ItemStatus.SCHEDULED, revision = 7)),
        )
        val store = PlannerStore(initial)
        val running = executionSession(
            status = "active",
            revision = 1,
            projectionEligible = true,
        ).copy(
            plannedBlockId = "33333333-3333-4333-8333-333333333333",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 1,
                activeSession = running,
                message = "Running",
            ),
        )
        val terminal = running.copy(
            status = wireStatus,
            revision = 2,
            accumulatedSeconds = 90,
            actualSeconds = 90,
            runningSince = null,
            endedAt = "1970-01-01T01:01:30Z",
            updatedAt = "1970-01-01T01:01:30Z",
        )
        requireNotNull(
            store.reconcileCanonicalExecution(
                syncOrigin = CANONICAL_ORIGIN,
                configurationId = "connection-1",
                revision = 2,
                activeSession = null,
                changedSession = terminal,
                message = "Ended",
            ),
        )
        val outcome = requireNotNull(store.state.value.terminalExecutionOutcomes[EXECUTION_ID])
        assertTrue(outcome.requiresCanonicalItemProjection)
        assertEquals(displayStatus, store.state.value.schedule.single().status)

        requireNotNull(
            store.replaceCanonicalPlan(
                canonicalUpdate(
                    item = canonicalItem(status = "planned", revision = 7),
                    block = canonicalBlock(ItemStatus.SCHEDULED, revision = 7),
                    cursor = "cursor-1",
                ),
            ),
        )

        assertEquals(displayStatus, store.state.value.schedule.single().status)
        val restarted = PlannerStore(store.state.value)
        assertEquals(displayStatus, restarted.state.value.schedule.single().status)
        assertTrue(restarted.isCanonicalExecutionStartBlocked(CANONICAL_BLOCK_ID))
    }

    private fun executionSession(
        status: String,
        revision: Long,
        projectionEligible: Boolean = false,
    ) = CanonicalExecutionSessionSnapshot(
        id = EXECUTION_ID,
        itemId = CANONICAL_ITEM_ID,
        itemRevision = 7,
        sessionIndex = 0,
        plannedBlockId = CANONICAL_BLOCK_ID,
        sourceDeviceId = DEVICE_ID,
        status = status,
        revision = revision,
        accumulatedSeconds = 0,
        startedAt = "1970-01-01T01:00:00Z",
        runningSince = "1970-01-01T01:00:00Z",
        createdAt = "1970-01-01T01:00:00Z",
        updatedAt = "1970-01-01T01:00:00Z",
        canonicalProjectionEligibleAtLeaseStart = projectionEligible.takeIf { it },
    )

    private fun terminalExecution(
        sessionId: String,
        itemId: String,
        endedAt: String,
    ) = CanonicalExecutionSessionSnapshot(
        id = sessionId,
        itemId = itemId,
        itemRevision = 7,
        sessionIndex = 0,
        plannedBlockId = null,
        sourceDeviceId = DEVICE_ID,
        status = "completed",
        revision = 2,
        accumulatedSeconds = 60,
        actualSeconds = 60,
        startedAt = "1970-01-01T00:00:00Z",
        runningSince = null,
        endedAt = endedAt,
        createdAt = "1970-01-01T00:00:00Z",
        updatedAt = endedAt,
    )

    private companion object {
        const val CANONICAL_ORIGIN = "https://api.example.test/"
        const val CANONICAL_ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val CANONICAL_BLOCK_ID = "22222222-2222-4222-8222-222222222222"
        const val EXECUTION_ID = "44444444-4444-4444-8444-444444444444"
        const val DEVICE_ID = "55555555-5555-4555-8555-555555555555"
    }
}
