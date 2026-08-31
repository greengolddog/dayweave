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
            runForegroundExecutionWorkers(
                invalidationStream = {
                    streamFailed.complete(Unit)
                    error("synthetic stream defect")
                },
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
}
