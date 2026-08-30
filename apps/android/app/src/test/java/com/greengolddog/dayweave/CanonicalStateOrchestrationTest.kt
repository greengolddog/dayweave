package com.greengolddog.dayweave

import com.greengolddog.dayweave.sync.CanonicalRefreshOutcome
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test

class CanonicalStateOrchestrationTest {
    @Test
    fun executionTruthBracketsTerminalProjectionAndComposition() = runBlocking {
        val calls = mutableListOf<String>()
        var executionReads = 0

        refreshCanonicalStateSequence(
            executionRefresh = {
                executionReads += 1
                calls += "execution-$executionReads"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefresh = {
                calls += "canonical-projection-compose"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(
            listOf("execution-1", "canonical-projection-compose", "execution-2"),
            calls,
        )
    }

    @Test
    fun failedInitialExecutionReadNeverComposesOverUnknownTruth() = runBlocking {
        val calls = mutableListOf<String>()

        refreshCanonicalStateSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE
            },
            canonicalRefresh = {
                calls += "canonical"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(listOf("execution"), calls)
    }

    @Test
    fun foregroundCrossDeviceCompletionProjectsThenRechecksExecution() = runBlocking {
        val calls = mutableListOf<String>()
        var projectionNeeded = true

        refreshForegroundExecutionSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefreshNeeded = { projectionNeeded },
            canonicalRefresh = {
                calls += "canonical-projection-compose"
                projectionNeeded = false
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(
            listOf("execution", "canonical-projection-compose", "execution"),
            calls,
        )
    }

    @Test
    fun steadyForegroundPollDoesNotBlindlyRecompose() = runBlocking {
        val calls = mutableListOf<String>()

        refreshForegroundExecutionSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefreshNeeded = { false },
            canonicalRefresh = {
                calls += "canonical"
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(listOf("execution"), calls)
    }

    @Test
    fun foregroundCrossDeviceDeferRecomposesAndPublishesThenRechecksExecution() = runBlocking {
        val calls = mutableListOf<String>()
        var deferredSourceStillPublished = true

        refreshForegroundExecutionSequence(
            executionRefresh = {
                calls += "execution"
                ExecutionSyncOutcome.SUCCESS
            },
            canonicalRefreshNeeded = { deferredSourceStillPublished },
            canonicalRefresh = {
                calls += "canonical-compose-publish"
                deferredSourceStillPublished = false
                CanonicalRefreshOutcome.SUCCESS
            },
        )

        assertEquals(
            listOf("execution", "canonical-compose-publish", "execution"),
            calls,
        )
    }
}
