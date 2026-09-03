package com.greengolddog.dayweave.health

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.DerivedEnergySnapshot
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.EnergySignalSource
import com.greengolddog.dayweave.model.RecoveryBand
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
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

    @Test
    fun closedGenerationRejectsAllEntryPointsBeforeAnyProviderTouch() = runBlocking {
        val initial = DayWeaveUiState.preview().copy(
            healthConnectSyncEnabled = true,
            derivedEnergySnapshot = syntheticSnapshot(EnergyLevel.DEEP),
        )
        val store = PlannerStore(initial)
        val provider = CountingEnergySignalProvider()
        val fence = MutableEnergySignalGenerationFence(initiallyOpen = false)
        val manager = EnergySignalManager(
            provider = provider,
            plannerStore = store,
            now = { now },
            generationFence = fence,
        )

        manager.refresh()
        manager.enable()
        manager.onPermissionResult(manager.requiredPermissions)
        manager.disable()

        assertEquals(0, provider.permissionMetadataCount.get())
        assertEquals(0, provider.availabilityCount.get())
        assertEquals(0, provider.permissionCount.get())
        assertEquals(0, provider.readCount.get())
        assertEquals(initial, store.state.value)

        manager.quarantineForPrivacyBoundary()
        assertTrue(store.state.value.healthConnectSyncEnabled)
        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(EnergySignalPhase.DISCONNECTED, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun generationClosureAfterProviderReadRejectsSnapshotBeforePersistence() = runBlocking {
        val store = PlannerStore(
            DayWeaveUiState.preview().copy(
                healthConnectSyncEnabled = true,
                derivedEnergySnapshot = null,
            ),
        )
        val fence = MutableEnergySignalGenerationFence()
        val derived = syntheticSnapshot(EnergyLevel.DEEP)
        val provider = object : EnergySignalProvider {
            override val requiredPermissions: Set<String> = setOf("synthetic.read")

            override fun availability() = EnergyProviderAvailability.AVAILABLE

            override suspend fun hasRequiredPermissions(): Boolean = true

            override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult {
                fence.close()
                return EnergyProviderResult.Snapshot(derived)
            }
        }
        val manager = EnergySignalManager(provider, store, { now }, fence)

        manager.refresh()

        assertTrue(store.state.value.healthConnectSyncEnabled)
        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(EnergySignalPhase.READING, manager.state.value.phase)
        manager.quarantineForPrivacyBoundary()
        assertEquals(EnergySignalPhase.DISCONNECTED, manager.state.value.phase)
    }

    @Test
    fun quarantineCancelsJobAndNonCooperativeReadCannotCommitAfterReturn() = runBlocking {
        val readStarted = CompletableDeferred<Unit>()
        val releaseRead = CompletableDeferred<Unit>()
        val staleSnapshot = syntheticSnapshot(EnergyLevel.DEEP)
        val store = PlannerStore(
            DayWeaveUiState.preview().copy(
                healthConnectSyncEnabled = true,
                derivedEnergySnapshot = syntheticSnapshot(EnergyLevel.LOW),
            ),
        )
        val provider = object : EnergySignalProvider {
            override val requiredPermissions: Set<String> = setOf("synthetic.read")

            override fun availability() = EnergyProviderAvailability.AVAILABLE

            override suspend fun hasRequiredPermissions(): Boolean = true

            override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult =
                withContext(NonCancellable) {
                    readStarted.complete(Unit)
                    releaseRead.await()
                    EnergyProviderResult.Snapshot(staleSnapshot)
                }
        }
        val manager = EnergySignalManager(provider, store, now = { now })
        val operation = async { manager.refresh() }
        readStarted.await()

        manager.quarantineForPrivacyBoundary()
        releaseRead.complete(Unit)
        try {
            operation.await()
            throw AssertionError("Expected privacy-boundary cancellation")
        } catch (_: CancellationException) {
            // The provider returned under NonCancellable, but the tracked operation stayed cancelled.
        }

        assertNull(store.state.value.derivedEnergySnapshot)
        assertEquals(EnergySignalPhase.DISCONNECTED, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun permissionCallbackCapturedBeforeGenerationRotationCannotMutateLaterState() = runBlocking {
        val permissionStarted = CompletableDeferred<Unit>()
        val releasePermission = CompletableDeferred<Unit>()
        val retainedSnapshot = syntheticSnapshot(EnergyLevel.MEDIUM)
        val initial = DayWeaveUiState.preview().copy(
            healthConnectSyncEnabled = true,
            derivedEnergySnapshot = retainedSnapshot,
        )
        val store = PlannerStore(initial)
        val fence = MutableEnergySignalGenerationFence()
        val provider = object : EnergySignalProvider {
            override val requiredPermissions: Set<String> = setOf("synthetic.read")
            val readCount = AtomicInteger(0)

            override fun availability() = EnergyProviderAvailability.AVAILABLE

            override suspend fun hasRequiredPermissions(): Boolean =
                withContext(NonCancellable) {
                    permissionStarted.complete(Unit)
                    releasePermission.await()
                    true
                }

            override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult {
                readCount.incrementAndGet()
                return EnergyProviderResult.NoData
            }
        }
        val manager = EnergySignalManager(provider, store, { now }, fence)
        val activeRefresh = async(start = CoroutineStart.UNDISPATCHED) { manager.refresh() }
        permissionStarted.await()
        val oldDeniedCallback = async(start = CoroutineStart.UNDISPATCHED) {
            manager.onPermissionResult(emptySet())
        }

        fence.rotateKeepingOpen()
        releasePermission.complete(Unit)
        activeRefresh.await()
        oldDeniedCallback.await()

        assertTrue(store.state.value.healthConnectSyncEnabled)
        assertEquals(retainedSnapshot, store.state.value.derivedEnergySnapshot)
        assertEquals(0, provider.readCount.get())
    }

    private fun syntheticSnapshot(level: EnergyLevel) = DerivedEnergySnapshot(
        energy = level,
        recovery = RecoveryBand.BALANCED,
        source = EnergySignalSource.HEALTH_CONNECT_SLEEP,
        calculatedAt = now.toString(),
    )

    private class CountingEnergySignalProvider : EnergySignalProvider {
        val permissionMetadataCount = AtomicInteger(0)
        val availabilityCount = AtomicInteger(0)
        val permissionCount = AtomicInteger(0)
        val readCount = AtomicInteger(0)
        override val requiredPermissions: Set<String>
            get() {
                permissionMetadataCount.incrementAndGet()
                return setOf("synthetic.read")
            }

        override fun availability(): EnergyProviderAvailability {
            availabilityCount.incrementAndGet()
            return EnergyProviderAvailability.AVAILABLE
        }

        override suspend fun hasRequiredPermissions(): Boolean {
            permissionCount.incrementAndGet()
            return true
        }

        override suspend fun readDerivedSnapshot(at: Instant): EnergyProviderResult {
            readCount.incrementAndGet()
            return EnergyProviderResult.NoData
        }
    }

    private class MutableEnergySignalGenerationFence(
        initiallyOpen: Boolean = true,
    ) : EnergySignalGenerationFence {
        private val generation = AtomicLong(1L)
        private val open = AtomicBoolean(initiallyOpen)

        override fun captureGeneration(): Long = generation.get()

        override fun isCurrent(generation: Long): Boolean =
            open.get() && this.generation.get() == generation

        fun close() {
            open.set(false)
            generation.incrementAndGet()
        }

        fun rotateKeepingOpen() {
            generation.incrementAndGet()
            open.set(true)
        }
    }
}
