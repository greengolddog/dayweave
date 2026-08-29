package com.greengolddog.dayweave.health

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.DerivedEnergySnapshot
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.EnergySignalSource
import com.greengolddog.dayweave.model.RecoveryBand
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class EnergySignalManagerTest {
    private val now = Instant.parse("2026-08-29T08:00:00Z")

    @Test
    fun enablingGrantedProviderStoresDerivedBandsWithoutMutatingThePlan() = runBlocking {
        val initial = DayWeaveUiState.preview()
        val store = PlannerStore(initial)
        val snapshot = syntheticSnapshot(EnergyLevel.MEDIUM)
        val provider = FakeEnergySignalProvider(
            permissionsGranted = true,
            result = EnergyProviderResult.Snapshot(snapshot),
        )
        val manager = EnergySignalManager(provider, store, now = { now })

        manager.enable()

        assertTrue(store.state.value.healthConnectSyncEnabled)
        assertEquals(snapshot, store.state.value.derivedEnergySnapshot)
        assertEquals(initial.schedule, store.state.value.schedule)
        assertEquals(EnergySignalPhase.READY, manager.state.value.phase)
        assertEquals(1, provider.readCount)
    }

    @Test
    fun deniedPermissionRemovesAutomaticSignalButKeepsManualCorrectionAndPlan() = runBlocking {
        val initial = DayWeaveUiState.preview().copy(
            healthConnectSyncEnabled = true,
            derivedEnergySnapshot = syntheticSnapshot(EnergyLevel.DEEP),
        )
        val store = PlannerStore(initial, nowEpochMillis = { now.toEpochMilli() })
        store.recordManualEnergyCheckIn(EnergyLevel.LOW)
        val provider = FakeEnergySignalProvider(permissionsGranted = false)
        val manager = EnergySignalManager(provider, store, now = { now })

        manager.onPermissionResult(emptySet())

        assertFalse(store.state.value.healthConnectSyncEnabled)
        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(EnergyLevel.LOW, store.state.value.manualEnergyCheckIn?.energy)
        assertEquals(initial.schedule, store.state.value.schedule)
        assertEquals(EnergySignalPhase.DENIED, manager.state.value.phase)
        assertEquals(0, provider.readCount)
    }

    @Test
    fun unavailableProviderFailsClosedAndNeverReads() = runBlocking {
        val store = PlannerStore(
            DayWeaveUiState.preview().copy(
                healthConnectSyncEnabled = true,
                derivedEnergySnapshot = syntheticSnapshot(EnergyLevel.DEEP),
            ),
        )
        val provider = FakeEnergySignalProvider(
            currentAvailability = EnergyProviderAvailability.UNAVAILABLE,
            permissionsGranted = true,
        )
        val manager = EnergySignalManager(provider, store, now = { now })

        manager.refresh()

        assertFalse(store.state.value.healthConnectSyncEnabled)
        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(EnergySignalPhase.UNAVAILABLE, manager.state.value.phase)
        assertEquals(0, provider.readCount)
    }

    @Test
    fun noDataClearsPreviousEstimateWithoutBlockingPlanning() = runBlocking {
        val initial = DayWeaveUiState.preview().copy(
            healthConnectSyncEnabled = true,
            derivedEnergySnapshot = syntheticSnapshot(EnergyLevel.DEEP),
        )
        val store = PlannerStore(initial)
        val provider = FakeEnergySignalProvider(result = EnergyProviderResult.NoData)
        val manager = EnergySignalManager(provider, store, now = { now })

        manager.refresh()

        assertTrue(store.state.value.healthConnectSyncEnabled)
        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(initial.schedule, store.state.value.schedule)
        assertEquals(EnergySignalPhase.NO_DATA, manager.state.value.phase)
    }

    @Test
    fun cancelledReadClearsEstimateAndLeavesStableNonBusyState() = runBlocking {
        val store = PlannerStore(
            DayWeaveUiState.preview().copy(
                healthConnectSyncEnabled = true,
                derivedEnergySnapshot = syntheticSnapshot(EnergyLevel.DEEP),
            ),
        )
        val provider = object : EnergySignalProvider {
            override val requiredPermissions: Set<String> = setOf("synthetic.read")

            override fun availability() = EnergyProviderAvailability.AVAILABLE

            override suspend fun hasRequiredPermissions(): Boolean = true

            override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult {
                throw CancellationException("synthetic cancellation")
            }
        }
        val manager = EnergySignalManager(provider, store, now = { now })

        try {
            manager.refresh()
            throw AssertionError("Expected cancellation")
        } catch (_: CancellationException) {
            // Expected: cancellation remains structured and visible to the caller.
        }

        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(EnergySignalPhase.ERROR, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
    }

    private fun syntheticSnapshot(level: EnergyLevel) = DerivedEnergySnapshot(
        energy = level,
        recovery = RecoveryBand.BALANCED,
        source = EnergySignalSource.HEALTH_CONNECT_SLEEP,
        calculatedAt = now.toString(),
    )
}
