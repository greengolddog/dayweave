package com.greengolddog.dayweave

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertTrue
import org.junit.Test

class DurableHabitActionTest {
    @Test
    fun cancellingTheUiWaiterDoesNotCancelApplicationOwnedStaging() = runBlocking {
        val applicationScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val stageStarted = CompletableDeferred<Unit>()
        val allowDurableReceipt = CompletableDeferred<Unit>()
        val durableResult = applicationScope.launchDurableBooleanAction {
            stageStarted.complete(Unit)
            allowDurableReceipt.await()
            true
        }
        stageStarted.await()

        val uiWaiter = launch { durableResult.await() }
        uiWaiter.cancelAndJoin()

        assertTrue(durableResult.isActive)
        allowDurableReceipt.complete(Unit)
        assertTrue(durableResult.await())
        applicationScope.cancel()
    }
}
