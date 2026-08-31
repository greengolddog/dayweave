package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ScheduleCompositionProfileUpdateCoordinatorTest {
    @Test
    fun viewModelRouteReportsSuccessOnlyAfterItsExactEncryptedSave() = runBlocking {
        val initial = DayWeaveUiState()
        val saveStarted = Channel<DayWeaveUiState>(Channel.UNLIMITED)
        val allowSave = Channel<Unit>(Channel.UNLIMITED)
        val repository = object : PlannerStateRepository {
            override suspend fun load(): DayWeaveUiState = initial

            override suspend fun save(state: DayWeaveUiState) {
                saveStarted.send(state)
                allowSave.receive()
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val coordinator = ScheduleCompositionProfileUpdateCoordinator(store) { action ->
                scope.launch { action() }
                true
            }
            val profile = ScheduleCompositionProfileSnapshot(
                dayStartMinute = 8 * 60,
                dayEndMinute = 21 * 60 + 30,
                slotGranularityMinutes = 10,
                stabilityWeight = 8,
                defaultSoftWeight = 250,
            )
            val firstViewModelRoute = coordinator::update
            // A recreated ViewModel resolves the same Application-owned coordinator and state.
            val recreatedViewModelRoute = coordinator::update

            assertTrue(firstViewModelRoute(profile))
            assertFalse(
                recreatedViewModelRoute(profile.copy(dayStartMinute = 9 * 60)),
            )
            assertEquals(
                ScheduleCompositionProfileUpdatePhase.SAVING,
                coordinator.state.value.phase,
            )
            assertEquals(profile, coordinator.state.value.requestedProfile)
            val pendingSave = withTimeout(3_000) { saveStarted.receive() }
            assertEquals(profile, pendingSave.scheduleCompositionProfile)
            assertEquals(
                ScheduleCompositionProfileUpdatePhase.SAVING,
                coordinator.state.value.phase,
            )

            allowSave.send(Unit)
            val saved = withTimeout(3_000) {
                coordinator.state.first {
                    it.phase == ScheduleCompositionProfileUpdatePhase.SAVED
                }
            }
            assertEquals(profile, saved.requestedProfile)
            assertEquals(profile, store.durableState.value?.scheduleCompositionProfile)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun alreadyDurableNoOpIsReportedAsSuccessful() = runBlocking {
        val store = PlannerStore(DayWeaveUiState())
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val coordinator = ScheduleCompositionProfileUpdateCoordinator(store) { action ->
                scope.launch { action() }
                true
            }

            assertTrue(coordinator.update(ScheduleCompositionProfileSnapshot()))
            val saved = withTimeout(3_000) {
                coordinator.state.first {
                    it.phase == ScheduleCompositionProfileUpdatePhase.SAVED
                }
            }

            assertEquals(ScheduleCompositionProfileSnapshot(), saved.requestedProfile)
            assertEquals(
                ScheduleCompositionProfileSnapshot(),
                store.durableState.value?.scheduleCompositionProfile,
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun cancellationIsRethrownAndNeverBecomesDurableSuccess() = runBlocking {
        val store = PlannerStore(DayWeaveUiState())
        lateinit var capturedAction: suspend () -> Unit
        val coordinator = ScheduleCompositionProfileUpdateCoordinator(
            plannerStore = store,
            persistProfile = { throw CancellationException("synthetic lifecycle cancellation") },
            launchCanonicalAction = { action ->
                capturedAction = action
                true
            },
        )

        assertTrue(
            coordinator.update(ScheduleCompositionProfileSnapshot(dayStartMinute = 8 * 60)),
        )
        val outcome = runCatching { capturedAction() }

        assertTrue(outcome.exceptionOrNull() is CancellationException)
        assertEquals(ScheduleCompositionProfileUpdatePhase.ERROR, coordinator.state.value.phase)
        assertFalse(coordinator.state.value.phase == ScheduleCompositionProfileUpdatePhase.SAVED)
    }

    @Test
    fun sharedActionGateRejectionLeavesProfileUntouchedAndVisibleAsBlocked() {
        val store = PlannerStore(DayWeaveUiState())
        val coordinator = ScheduleCompositionProfileUpdateCoordinator(store) { false }
        val profile = ScheduleCompositionProfileSnapshot(dayStartMinute = 8 * 60)

        assertFalse(coordinator.update(profile))

        assertEquals(
            ScheduleCompositionProfileSnapshot(),
            store.state.value.scheduleCompositionProfile,
        )
        assertEquals(ScheduleCompositionProfileUpdatePhase.BLOCKED, coordinator.state.value.phase)
        assertTrue(requireNotNull(coordinator.state.value.message).contains("planner action"))
    }

    @Test
    fun presentationAndStoreShareTheSameExecutionUncertaintyFence() {
        val active = DayWeaveUiState(
            activeSession = ActiveSession(
                itemId = "local-item",
                elapsedMinutes = 1,
                isPaused = false,
            ),
        )
        val store = PlannerStore(active)
        var launched = false
        val coordinator = ScheduleCompositionProfileUpdateCoordinator(store) {
            launched = true
            true
        }

        assertEquals(
            ScheduleCompositionProfileEditBlocker.EXECUTION_CHANGE,
            active.scheduleCompositionProfileEditBlocker(),
        )
        assertFalse(
            coordinator.update(ScheduleCompositionProfileSnapshot(dayStartMinute = 8 * 60)),
        )
        assertFalse(launched)
        assertEquals(ScheduleCompositionProfileUpdatePhase.BLOCKED, coordinator.state.value.phase)
    }
}
