package com.greengolddog.dayweave.network

import org.junit.Assert.*
import org.junit.Test

class ForegroundNetworkReconnectHintsTest {
    @Test fun availabilityAndValidationRecoveryEmitOnlyEdges() {
        val hints = ForegroundNetworkReconnectHints<String>()
        assertTrue(hints.available("synthetic-wifi"))
        assertFalse(hints.available("synthetic-wifi"))
        assertTrue(hints.validated("synthetic-wifi", true))
        assertFalse(hints.validated("synthetic-wifi", true))
        assertFalse(hints.validated("synthetic-wifi", false))
        assertTrue(hints.validated("synthetic-wifi", true))
    }

    @Test fun obsoleteNetworkCallbacksCannotReplaceCurrentNetworkState() {
        val hints = ForegroundNetworkReconnectHints<String>()
        assertTrue(hints.available("synthetic-wifi"))
        assertTrue(hints.available("synthetic-cellular"))
        hints.lost("synthetic-wifi")
        assertFalse(hints.validated("synthetic-wifi", true))
        assertTrue(hints.validated("synthetic-cellular", true))
        hints.lost("synthetic-cellular")
        assertFalse(hints.validated("synthetic-cellular", true))
        assertTrue(hints.available("synthetic-cellular"))
    }
}
