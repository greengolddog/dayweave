package com.greengolddog.dayweave

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ApplicationEnergySignalGenerationFenceTest {
    @Test
    fun onlyTheCurrentOpenLifecycleGenerationIsAdmitted() {
        val fence = ApplicationEnergySignalGenerationFence()
        val initiallyClosed = fence.captureGeneration()

        assertFalse(fence.isCurrent(initiallyClosed))

        fence.open()
        val firstOpen = fence.captureGeneration()
        assertTrue(fence.isCurrent(firstOpen))

        fence.open()
        assertTrue(fence.isCurrent(firstOpen))

        fence.close()
        assertFalse(fence.isCurrent(firstOpen))
        val closed = fence.captureGeneration()
        assertFalse(fence.isCurrent(closed))

        fence.open()
        val secondOpen = fence.captureGeneration()
        assertNotEquals(firstOpen, secondOpen)
        assertTrue(fence.isCurrent(secondOpen))
        assertFalse(fence.isCurrent(firstOpen))
    }
}
