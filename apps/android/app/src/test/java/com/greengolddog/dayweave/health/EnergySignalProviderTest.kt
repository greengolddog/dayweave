package com.greengolddog.dayweave.health

import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.EnergySignalSource
import com.greengolddog.dayweave.model.RecoveryBand
import java.time.Instant
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class EnergySignalProviderTest {
    private val now = Instant.parse("2026-08-29T08:00:00Z")

    @Test
    fun recentSleepHeuristicProducesOnlyCoarseDerivedBands() {
        val low = requireNotNull(deriveEnergyFromRecentSleep(5 * 60, now))
        val medium = requireNotNull(deriveEnergyFromRecentSleep(7 * 60, now))
        val deep = requireNotNull(deriveEnergyFromRecentSleep(8 * 60, now))

        assertEquals(EnergyLevel.LOW, low.energy)
        assertEquals(RecoveryBand.LOW, low.recovery)
        assertEquals(EnergyLevel.MEDIUM, medium.energy)
        assertEquals(RecoveryBand.BALANCED, medium.recovery)
        assertEquals(EnergyLevel.DEEP, deep.energy)
        assertEquals(RecoveryBand.HIGH, deep.recovery)
        assertEquals(EnergySignalSource.HEALTH_CONNECT_SLEEP, deep.source)
        assertEquals(now.toString(), deep.calculatedAt)

        // Invalid or implausible aggregates fail closed instead of manufacturing an estimate.
        assertNull(deriveEnergyFromRecentSleep(0, now))
        assertNull(deriveEnergyFromRecentSleep(17 * 60, now))
    }

    @Test
    fun fakeProviderReturnsTheSameSyntheticResultDeterministically() = runBlocking {
        val expected = EnergyProviderResult.Snapshot(
            requireNotNull(deriveEnergyFromRecentSleep(7 * 60, now)),
        )
        val provider = FakeEnergySignalProvider(result = expected)

        assertEquals(expected, provider.readDerivedSnapshot(now))
        assertEquals(expected, provider.readDerivedSnapshot(now.plusSeconds(60)))
        assertEquals(2, provider.readCount)
    }
}
