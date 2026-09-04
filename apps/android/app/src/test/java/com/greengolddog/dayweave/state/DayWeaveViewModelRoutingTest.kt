package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DayWeaveViewModelRoutingTest {
    @Test
    fun canonicalBlockRoutesToServerWhileLocalBlockStaysLocal() {
        val canonical = block("canonical", canonicalItemId = "item")
        val local = block("local", canonicalItemId = null)
        val state = DayWeaveUiState(schedule = listOf(canonical, local))

        assertEquals(ExecutionActionTarget.SERVER, executionActionTarget(state, "canonical"))
        assertEquals(ExecutionActionTarget.LOCAL, executionActionTarget(state, "local"))
    }

    @Test
    fun temporarilyUnmatchedServerSessionStillFailsClosed() {
        val state = DayWeaveUiState(
            activeSession = ActiveSession(
                itemId = "missing-block",
                elapsedMinutes = 1,
                isPaused = true,
                canonicalExecutionSessionId = "session",
            ),
        )

        assertEquals(
            ExecutionActionTarget.SERVER,
            executionActionTarget(state, "missing-block"),
        )
    }

    @Test
    fun successfulTerminalCommandImmediatelyRunsApplicationRefreshSequence() = runBlocking {
        val calls = mutableListOf<String>()

        val outcome = finishCanonicalExecution(
            command = {
                calls += "complete"
                ExecutionSyncOutcome.SUCCESS
            },
            refreshCanonicalState = { calls += "refresh" },
        )

        assertEquals(ExecutionSyncOutcome.SUCCESS, outcome)
        assertEquals(listOf("complete", "refresh"), calls)
    }

    @Test
    fun failedTerminalCommandDoesNotPretendToProjectCanonicalState() = runBlocking {
        var refreshed = false

        val outcome = finishCanonicalExecution(
            command = { ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE },
            refreshCanonicalState = { refreshed = true },
        )

        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, outcome)
        assertEquals(false, refreshed)
    }

    @Test
    fun successfulDeferImmediatelyRunsComposeAndPublicationRefresh() = runBlocking {
        val calls = mutableListOf<String>()

        val outcome = deferCanonicalExecutionAndRefresh(
            command = {
                calls += "defer"
                ExecutionSyncOutcome.SUCCESS
            },
            refreshCanonicalState = { calls += "compose-publish" },
        )

        assertEquals(ExecutionSyncOutcome.SUCCESS, outcome)
        assertEquals(listOf("defer", "compose-publish"), calls)
    }

    @Test
    fun failedDeferRetainsRecoveryStateWithoutClaimingARefresh() = runBlocking {
        var refreshed = false

        val outcome = deferCanonicalExecutionAndRefresh(
            command = { ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE },
            refreshCanonicalState = { refreshed = true },
        )

        assertEquals(ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE, outcome)
        assertEquals(false, refreshed)
    }

    @Test
    fun recoveredPriorDeferStillRunsComposeAndPublicationRefresh() = runBlocking {
        val calls = mutableListOf<String>()

        val outcome = deferCanonicalExecutionAndRefresh(
            command = {
                calls += "recover-prior-defer"
                ExecutionSyncOutcome.RECOVERED_COMMAND
            },
            refreshCanonicalState = { calls += "compose-publish" },
        )

        assertEquals(ExecutionSyncOutcome.RECOVERED_COMMAND, outcome)
        assertEquals(listOf("recover-prior-defer", "compose-publish"), calls)
    }

    @Test
    fun durableCanonicalAuthoringSchedulesBackgroundSyncWithoutChangingLocalSuccess() = runBlocking {
        val calls = mutableListOf<String>()

        val saved = persistCanonicalAuthoringThenScheduleSync(
            persist = {
                calls += "persist"
                true
            },
            scheduleSync = { calls += "sync" },
        )

        assertEquals(true, saved)
        assertEquals(listOf("persist", "sync"), calls)
    }

    @Test
    fun failedCanonicalAuthoringDoesNotStartSync() = runBlocking {
        var syncScheduled = false

        val saved = persistCanonicalAuthoringThenScheduleSync(
            persist = { false },
            scheduleSync = { syncScheduled = true },
        )

        assertEquals(false, saved)
        assertEquals(false, syncScheduled)
    }

    @Test
    fun durableHabitOutcomeSchedulesBestEffortBackgroundSync() = runBlocking {
        val calls = mutableListOf<String>()

        val saved = persistHabitOutcomeThenScheduleSync(
            persist = {
                calls += "persist-exact-outcome"
                true
            },
            scheduleSync = { calls += "sync" },
        )

        assertTrue(saved)
        assertEquals(listOf("persist-exact-outcome", "sync"), calls)
    }

    @Test
    fun rejectedHabitOutcomePersistenceKeepsTheActionRetryableWithoutStartingSync() = runBlocking {
        var syncScheduled = false

        val saved = persistHabitOutcomeThenScheduleSync(
            persist = { false },
            scheduleSync = { syncScheduled = true },
        )

        assertFalse(saved)
        assertFalse(syncScheduled)
    }

    @Test
    fun habitActionAdmissionPropagatesBusyAndSharedGateRejection() {
        var launchCalls = 0

        assertFalse(
            admitHabitAction(habitBusy = true) {
                launchCalls += 1
                true
            },
        )
        assertEquals(0, launchCalls)
        assertFalse(
            admitHabitAction(habitBusy = false) {
                launchCalls += 1
                false
            },
        )
        assertEquals(1, launchCalls)
        assertTrue(
            admitHabitAction(habitBusy = false) {
                launchCalls += 1
                true
            },
        )
        assertEquals(2, launchCalls)
    }

    private fun block(id: String, canonicalItemId: String?) = ScheduleItem(
        id = id,
        title = id,
        kind = ItemKind.TASK,
        startMinute = 0,
        durationMinutes = 30,
        status = ItemStatus.SCHEDULED,
        canonicalItemId = canonicalItemId,
        canonicalRevision = canonicalItemId?.let { 1 },
    )
}
