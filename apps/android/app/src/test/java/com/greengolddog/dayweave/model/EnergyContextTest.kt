package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class EnergyContextTest {
    private val now = Instant.parse("2026-08-29T10:00:00Z")
    private val zone = ZoneId.of("Europe/Madrid")

    @Test
    fun manualCorrectionOverridesEstimateAndCanReturnToIt() {
        val state = DayWeaveUiState.preview().copy(
            schedule = listOf(
                ScheduleItem(
                    id = "low-task",
                    title = "Low task",
                    kind = ItemKind.TASK,
                    startMinute = 10 * 60,
                    durationMinutes = 20,
                    status = ItemStatus.SCHEDULED,
                    energy = EnergyLevel.LOW,
                ),
                ScheduleItem(
                    id = "deep-task",
                    title = "Deep task",
                    kind = ItemKind.TASK,
                    startMinute = 11 * 60,
                    durationMinutes = 60,
                    status = ItemStatus.SCHEDULED,
                    energy = EnergyLevel.DEEP,
                ),
            ),
            healthConnectSyncEnabled = true,
            derivedEnergySnapshot = DerivedEnergySnapshot(
                energy = EnergyLevel.DEEP,
                recovery = RecoveryBand.HIGH,
                source = EnergySignalSource.HEALTH_CONNECT_SLEEP,
                calculatedAt = now.minusSeconds(60).toString(),
            ),
        )
        val store = PlannerStore(state, nowEpochMillis = { now.toEpochMilli() })

        store.recordManualEnergyCheckIn(EnergyLevel.LOW)

        assertEquals(
            EnergyLevel.LOW,
            store.state.value.effectiveEnergySignal(now, zone)?.energy,
        )
        assertEquals(
            "Low task",
            store.state.value.energyFitCandidate(now, zone)?.title,
        )

        store.recordManualEnergyCheckIn(EnergyLevel.MEDIUM)

        assertEquals(
            EnergyLevel.MEDIUM,
            store.state.value.effectiveEnergySignal(now, zone)?.energy,
        )

        store.clearManualEnergyCheckIn()

        assertEquals(
            EnergyLevel.DEEP,
            store.state.value.effectiveEnergySignal(now, zone)?.energy,
        )
    }

    @Test
    fun staleAutomaticEstimateIsNotUsedForPlanning() {
        val state = DayWeaveUiState(
            derivedEnergySnapshot = DerivedEnergySnapshot(
                energy = EnergyLevel.DEEP,
                recovery = RecoveryBand.HIGH,
                source = EnergySignalSource.HEALTH_CONNECT_SLEEP,
                calculatedAt = now.minusSeconds(19 * 60 * 60).toString(),
            ),
        )

        assertNull(state.effectiveEnergySignal(now, zone))
        assertNull(state.energyFitCandidate(now, zone))
    }
}
