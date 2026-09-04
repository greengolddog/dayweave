package com.greengolddog.dayweave.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class ItemDeltaContractTest {
    @Test
    fun responseBoundAllowsOneCompleteAtomicGroupAtThePageBoundary() {
        assertEquals(300, maximumItemDeltaResponseChanges(1))
        assertEquals(349, maximumItemDeltaResponseChanges(50))
        assertEquals(499, maximumItemDeltaResponseChanges(200))
    }

    @Test
    fun responseBoundRejectsInvalidRequestLimits() {
        assertThrows(IllegalArgumentException::class.java) {
            maximumItemDeltaResponseChanges(0)
        }
    }
}
