package com.greengolddog.dayweave.sync

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
}
