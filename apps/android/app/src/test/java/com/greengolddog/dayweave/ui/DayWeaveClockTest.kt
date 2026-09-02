package com.greengolddog.dayweave.ui

import java.time.Instant
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Test

class DayWeaveClockTest {
    @Test
    fun clockWakesAtTheNextExactMinute() {
        assertEquals(
            29_750L,
            plannerClockDelayMillis(
                reference = Instant.parse("2026-09-01T07:00:30.250Z"),
                zoneId = ZoneId.of("UTC"),
                exactHorizonEnd = null,
            ),
        )
    }

    @Test
    fun clockWakesAtAnEarlierFirmHorizonEdge() {
        val reference = Instant.parse("2026-09-01T07:00:30.250Z")

        assertEquals(
            5_500L,
            plannerClockDelayMillis(
                reference = reference,
                zoneId = ZoneId.of("UTC"),
                exactHorizonEnd = reference.plusMillis(5_500),
            ),
        )
    }

    @Test
    fun clockObservesTheEarlierRepeatedLocalMidnight() {
        assertEquals(
            500L,
            plannerClockDelayMillis(
                reference = Instant.parse("2026-11-01T03:59:59.500Z"),
                zoneId = ZoneId.of("America/Havana"),
                exactHorizonEnd = null,
            ),
        )
    }
}
