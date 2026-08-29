package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
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
