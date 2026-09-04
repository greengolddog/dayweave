package com.greengolddog.dayweave.ui

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertTrue
import org.junit.Test

class ForegroundExecutionLifecycleTest {
    @Test
    fun unexpectedStreamFailureCannotCancelPollingFallback() = runBlocking {
        val pollingStarted = CompletableDeferred<Unit>()
        val streamFailed = CompletableDeferred<Unit>()
        val workers = async {
            runForegroundInvalidationWorkers(
                executionInvalidationStream = {
                    streamFailed.complete(Unit)
                    error("synthetic stream defect")
                },
                canonicalItemInvalidations = null,
                polling = {
                    pollingStarted.complete(Unit)
                    awaitCancellation()
                },
            )
        }

        withTimeout(2_000) { pollingStarted.await() }
        withTimeout(2_000) { streamFailed.await() }

        assertTrue(workers.isActive)
        workers.cancelAndJoin()
    }

    @Test
    fun unexpectedCanonicalItemWorkerFailureCannotCancelExecutionPollingFallback() = runBlocking {
        val pollingStarted = CompletableDeferred<Unit>()
        val itemWorkerFailed = CompletableDeferred<Unit>()
        val executionStreamStarted = CompletableDeferred<Unit>()
        val workers = async {
            runForegroundInvalidationWorkers(
                executionInvalidationStream = {
                    executionStreamStarted.complete(Unit)
                    awaitCancellation()
                },
                canonicalItemInvalidations = {
                    itemWorkerFailed.complete(Unit)
                    error("synthetic item stream defect")
                },
                polling = {
                    pollingStarted.complete(Unit)
                    awaitCancellation()
                },
            )
        }

        withTimeout(2_000) { pollingStarted.await() }
        withTimeout(2_000) { executionStreamStarted.await() }
        withTimeout(2_000) { itemWorkerFailed.await() }

        assertTrue(workers.isActive)
        workers.cancelAndJoin()
    }

    @Test
    fun unexpectedHabitWorkerFailureCannotCancelOtherForegroundWorkers() = runBlocking {
        val pollingStarted = CompletableDeferred<Unit>()
        val habitWorkerFailed = CompletableDeferred<Unit>()
        val executionStreamStarted = CompletableDeferred<Unit>()
        val workers = async {
            runForegroundInvalidationWorkers(
                executionInvalidationStream = {
                    executionStreamStarted.complete(Unit)
                    awaitCancellation()
                },
                canonicalItemInvalidations = null,
                habitInvalidations = {
                    habitWorkerFailed.complete(Unit)
                    error("synthetic habit stream defect")
                },
                polling = {
                    pollingStarted.complete(Unit)
                    awaitCancellation()
                },
            )
        }

        withTimeout(2_000) { pollingStarted.await() }
        withTimeout(2_000) { executionStreamStarted.await() }
        withTimeout(2_000) { habitWorkerFailed.await() }

        assertTrue(workers.isActive)
        workers.cancelAndJoin()
    }
}
