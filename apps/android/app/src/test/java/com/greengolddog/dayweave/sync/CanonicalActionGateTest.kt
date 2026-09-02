package com.greengolddog.dayweave.sync

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalActionGateTest {
    @Test
    fun rapidSecondActionIsRejectedUntilFirstActionReleasesGate() {
        val gate = CanonicalActionGate()

        assertTrue(gate.tryEnter())
        assertFalse(gate.tryEnter())

        gate.leave()
        assertTrue(gate.tryEnter())
        gate.leave()
    }

    @Test
    fun requiredRecoveryWaitsForCurrentActionAndThenOwnsGate() = runBlocking {
        val gate = CanonicalActionGate()
        assertTrue(gate.tryEnter())
        var recoveryEntered = false
        val recoveryAttempted = CompletableDeferred<Unit>()

        val recovery = launch {
            recoveryAttempted.complete(Unit)
            gate.enter()
            recoveryEntered = true
            assertFalse(gate.tryEnter())
            gate.leave()
        }
        recoveryAttempted.await()
        assertFalse(recoveryEntered)

        gate.leave()
        recovery.join()
        assertTrue(recoveryEntered)
        assertTrue(gate.tryEnter())
        gate.leave()
    }
}
