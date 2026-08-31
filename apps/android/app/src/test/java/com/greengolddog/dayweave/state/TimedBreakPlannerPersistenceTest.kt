package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.unacknowledgedTimedBreakNotificationIdentity
import com.greengolddog.dayweave.notifications.PlannerTimedBreakNotificationRouteAccess
import com.greengolddog.dayweave.notifications.PlannerTimedBreakNotificationStateAccess
import com.greengolddog.dayweave.notifications.TimedBreakDeliveryCompletion
import com.greengolddog.dayweave.notifications.TimedBreakNotificationDelivery
import com.greengolddog.dayweave.notifications.TimedBreakNotificationGateway
import com.greengolddog.dayweave.notifications.TimedBreakNotificationPostResult
import com.greengolddog.dayweave.notifications.TimedBreakNotificationRouteConsumption
import com.greengolddog.dayweave.notifications.TimedBreakNotificationPresentationDecision
import com.greengolddog.dayweave.notifications.TimedBreakPreparation
import com.greengolddog.dayweave.notifications.shouldOpenTimedBreakResolution
import com.greengolddog.dayweave.notifications.timedBreakNotificationPresentationDecision
import java.time.Instant
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TimedBreakPlannerPersistenceTest {
    @Test
    fun failedEncryptedClaimSaveFailsClosedAndNeverPosts() = runBlocking {
        val initial = timedBreakState()
        val repository = FailingSavePlannerRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = restoredStore(repository, scope)
            val digest = initial.authoritativeTimedBreakNotificationIdentity()!!.digest
            val gateway = CountingNotificationGateway()

            val completion = TimedBreakNotificationDelivery(
                stateAccess = PlannerTimedBreakNotificationStateAccess(store) { DEADLINE + 1L },
                gateway = gateway,
            ).deliver(digest)

            assertEquals(TimedBreakDeliveryCompletion.SUCCESS, completion)
            assertEquals(0, gateway.posts)
            assertEquals(PlannerLoadState.PERSISTENCE_FAILED, store.loadState.value)
            assertNull(store.durableState.value!!.lastBreakEndNotificationAttemptDigest)
            assertEquals(digest, store.state.value.lastBreakEndNotificationAttemptDigest)
            assertTrue(store.state.value.activeSession!!.timedBreakEnded)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun durableStatePublishesOnlyCompletedEncryptedSavesAndNeverRegresses() = runBlocking {
        val initial = timedBreakState()
        val repository = GatedPlannerRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { DEADLINE + 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } },
            )
            assertFalse(store.durableState.value!!.activeSession!!.timedBreakEnded)
            val digest = initial.authoritativeTimedBreakNotificationIdentity()!!.digest

            val claimed = store.claimTimedBreakEndNotificationDelivery(digest)!!
            val consumed = store.recordTimedBreakNotificationRouteConsumption(digest)!!
            withTimeout(3_000) { repository.saveStarted.receive() }

            // Both newer in-memory generations exist, but neither may escape before save returns.
            assertTrue(store.state.value.activeSession!!.timedBreakEnded)
            assertEquals(digest, store.state.value.lastBreakEndNotificationAttemptDigest)
            assertEquals(digest, store.state.value.lastConsumedBreakEndNotificationDigest)
            assertFalse(store.durableState.value!!.activeSession!!.timedBreakEnded)
            assertNull(store.durableState.value!!.lastBreakEndNotificationAttemptDigest)
            assertNull(store.durableState.value!!.lastConsumedBreakEndNotificationDigest)

            repository.allowSave.send(Unit)
            assertTrue(withTimeout(3_000) { claimed.awaitDurable() })
            assertTrue(store.durableState.value!!.activeSession!!.timedBreakEnded)
            assertEquals(digest, store.durableState.value!!.lastBreakEndNotificationAttemptDigest)
            assertNull(store.durableState.value!!.lastConsumedBreakEndNotificationDigest)

            withTimeout(3_000) { repository.saveStarted.receive() }
            repository.allowSave.send(Unit)
            assertTrue(withTimeout(3_000) { consumed.awaitDurable() })
            assertTrue(store.durableState.value!!.activeSession!!.timedBreakEnded)
            assertEquals(digest, store.durableState.value!!.lastBreakEndNotificationAttemptDigest)
            assertEquals(digest, store.durableState.value!!.lastConsumedBreakEndNotificationDigest)
            assertEquals(
                listOf(digest, digest),
                repository.savedStates.map { it.lastBreakEndNotificationAttemptDigest },
            )
            assertEquals(
                listOf(null, digest),
                repository.savedStates.map { it.lastConsumedBreakEndNotificationDigest },
            )
        } finally {
            scope.cancel()
        }
        Unit
    }

    @Test
    fun restoredAttemptSuppressesDuplicateWithoutMutatingOrResumingLease() = runBlocking {
        val base = timedBreakState(timedBreakEnded = true)
        val digest = base.authoritativeTimedBreakNotificationIdentity()!!.digest
        val restored = base.copy(lastBreakEndNotificationAttemptDigest = digest)
        val repository = ImmediatePlannerRepository(restored)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { DEADLINE + 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } },
            )
            val executionBefore = store.state.value.canonicalExecutionSession

            val result = PlannerTimedBreakNotificationStateAccess(store) { DEADLINE + 1L }
                .prepare(digest)

            assertEquals(TimedBreakPreparation.ALREADY_HANDLED, result)
            assertEquals(executionBefore, store.state.value.canonicalExecutionSession)
            assertTrue(store.state.value.activeSession!!.isPaused)
            assertTrue(store.state.value.activeSession!!.timedBreakEnded)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun laggingDurablePausedStateCannotOverrideLiveResumeAfterRejectedCas() = runBlocking {
        val initial = timedBreakState()
        val repository = GatedPlannerRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { DEADLINE + 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } },
            )
            val digest = initial.authoritativeTimedBreakNotificationIdentity()!!.digest

            store.resumeActive()
            withTimeout(3_000) { repository.saveStarted.receive() }
            assertTrue(store.durableState.value!!.activeSession!!.isPaused)
            assertFalse(store.state.value.activeSession!!.isPaused)

            val result = PlannerTimedBreakNotificationStateAccess(store) { DEADLINE + 1L }
                .prepare(digest)

            assertEquals(TimedBreakPreparation.STALE, result)
            assertFalse(store.state.value.activeSession!!.timedBreakEnded)
            repository.allowSave.send(Unit)
            withTimeout(3_000) {
                store.durableState.first { it?.activeSession?.isPaused == false }
            }
        } finally {
            scope.cancel()
        }
        Unit
    }

    @Test
    fun liveDeliveryClaimPendingNeverFallsBackToOlderDurableReadyGeneration() = runBlocking {
        val initial = timedBreakState(timedBreakEnded = true)
        val repository = GatedPlannerRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { DEADLINE + 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } },
            )
            val digest = initial.authoritativeTimedBreakNotificationIdentity()!!.digest

            val pendingClaim = store.claimTimedBreakEndNotificationDelivery(digest)!!
            withTimeout(3_000) { repository.saveStarted.receive() }
            assertNull(store.durableState.value!!.lastBreakEndNotificationAttemptDigest)
            assertEquals(digest, store.state.value.lastBreakEndNotificationAttemptDigest)

            val result = PlannerTimedBreakNotificationStateAccess(store) { DEADLINE + 1L }
                .prepare(digest)

            assertEquals(TimedBreakPreparation.ALREADY_HANDLED, result)
            repository.allowSave.send(Unit)
            assertTrue(withTimeout(3_000) { pendingClaim.awaitDurable() })
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun durableTapReceiptConsumesExactRouteOnceAcrossProcessRestart() = runBlocking {
        val repository = ImmediatePlannerRepository(timedBreakState(timedBreakEnded = true))
        val firstScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val digest: String
        try {
            val store = restoredStore(repository, firstScope)
            digest = store.state.value.authoritativeTimedBreakNotificationIdentity()!!.digest

            assertEquals(
                TimedBreakNotificationRouteConsumption.CONSUMED,
                PlannerTimedBreakNotificationRouteAccess(store) { DEADLINE + 1L }
                    .consume(digest),
            )
            assertEquals(digest, store.durableState.value!!.lastConsumedBreakEndNotificationDigest)
        } finally {
            firstScope.cancel()
        }

        val restartedScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val restarted = restoredStore(repository, restartedScope)
            assertEquals(
                TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED,
                PlannerTimedBreakNotificationRouteAccess(restarted) { DEADLINE + 1L }
                    .consume(digest),
            )
            assertFalse(
                shouldOpenTimedBreakResolution(
                    durableState = restarted.durableState.value!!,
                    liveState = restarted.state.value,
                    identityDigest = digest,
                    nowEpochMillis = DEADLINE + 1L,
                ),
            )
            assertTrue(restarted.state.value.activeSession!!.isPaused)
        } finally {
            restartedScope.cancel()
        }
    }

    @Test
    fun recreatingWhileTapReceiptSaveIsPendingWaitsForDurableConsumeOnceProof() = runBlocking {
        val initial = timedBreakState(timedBreakEnded = true)
        val repository = GatedPlannerRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = restoredStore(repository, scope)
            val digest = initial.authoritativeTimedBreakNotificationIdentity()!!.digest
            val access = PlannerTimedBreakNotificationRouteAccess(store) { DEADLINE + 1L }

            val firstActivity = async { access.consume(digest) }
            withTimeout(3_000) { repository.saveStarted.receive() }
            assertEquals(digest, store.state.value.lastConsumedBreakEndNotificationDigest)
            assertNull(store.durableState.value!!.lastConsumedBreakEndNotificationDigest)

            val recreatedActivity = async { access.consume(digest) }
            assertFalse(recreatedActivity.isCompleted)

            repository.allowSave.send(Unit)
            assertEquals(
                TimedBreakNotificationRouteConsumption.CONSUMED,
                withTimeout(3_000) { firstActivity.await() },
            )
            assertEquals(
                TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED,
                withTimeout(3_000) { recreatedActivity.await() },
            )
            assertEquals(
                TimedBreakNotificationPresentationDecision.PRESENT_EXACT_BREAK,
                timedBreakNotificationPresentationDecision(
                    consumption = TimedBreakNotificationRouteConsumption.ALREADY_CONSUMED,
                    initiallyMatchedExactBreak = true,
                    currentEndedBreakKey = digest,
                ),
            )
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun keepPausedDurablyAcknowledgesExactBreakAndSurvivesRestart() = runBlocking {
        val repository = ImmediatePlannerRepository(timedBreakState(timedBreakEnded = true))
        val firstScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val digest: String
        try {
            val store = restoredStore(repository, firstScope)
            digest = store.state.value.authoritativeTimedBreakNotificationIdentity()!!.digest

            val receipt = store.acknowledgeTimedBreakEnded(digest)!!
            assertTrue(receipt.awaitDurable())

            val durable = store.durableState.value!!
            assertEquals(digest, durable.acknowledgedBreakEndDigest)
            assertEquals(digest, durable.lastBreakEndNotificationAttemptDigest)
            assertEquals(digest, durable.lastConsumedBreakEndNotificationDigest)
            assertTrue(durable.activeSession!!.isPaused)
            assertFalse(durable.activeSession!!.timedBreakEnded)
            assertEquals("paused", durable.canonicalExecutionSession!!.status)
            assertNull(durable.unacknowledgedTimedBreakNotificationIdentity())
            assertFalse(store.tickActiveSession())
        } finally {
            firstScope.cancel()
        }

        val restartedScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val restarted = restoredStore(repository, restartedScope)
            val durable = restarted.durableState.value!!
            assertEquals(digest, durable.acknowledgedBreakEndDigest)
            assertFalse(durable.activeSession!!.timedBreakEnded)
            assertFalse(restarted.tickActiveSession())
            assertNull(durable.unacknowledgedTimedBreakNotificationIdentity())

            val replacement = durable.copy(
                canonicalExecutionRevision = 9,
                canonicalExecutionSession = durable.canonicalExecutionSession!!.copy(
                    revision = 4,
                    pauseUntil = Instant.ofEpochMilli(DEADLINE + 600_000L).toString(),
                ),
                activeSession = durable.activeSession.copy(
                    pauseUntilEpochMillis = DEADLINE + 600_000L,
                ),
            )
            assertTrue(
                replacement.unacknowledgedTimedBreakNotificationIdentity()!!.digest != digest,
            )
        } finally {
            restartedScope.cancel()
        }
    }

    @Test
    fun invalidRestoredAttemptDigestIsDroppedAndPersistedBeforeBecomingDurable() = runBlocking {
        val repository = GatedPlannerRepository(
            timedBreakState().copy(lastBreakEndNotificationAttemptDigest = "not-a-digest"),
        )
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(
                initialState = DayWeaveUiState(),
                repository = repository,
                scope = scope,
                nowEpochMillis = { DEADLINE - 1L },
            )
            assertEquals(
                PlannerLoadState.READY,
                withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } },
            )
            assertNull(store.state.value.lastBreakEndNotificationAttemptDigest)
            assertNull(store.durableState.value)

            withTimeout(3_000) { repository.saveStarted.receive() }
            repository.allowSave.send(Unit)
            withTimeout(3_000) { store.durableState.first { it != null } }

            assertNull(store.durableState.value!!.lastBreakEndNotificationAttemptDigest)
            assertNull(repository.savedStates.single().lastBreakEndNotificationAttemptDigest)
        } finally {
            scope.cancel()
        }
    }

    private suspend fun restoredStore(
        repository: PlannerStateRepository,
        scope: CoroutineScope,
    ): PlannerStore = PlannerStore(
        initialState = DayWeaveUiState(),
        repository = repository,
        scope = scope,
        nowEpochMillis = { DEADLINE + 1L },
    ).also { store ->
        assertEquals(
            PlannerLoadState.READY,
            withTimeout(3_000) { store.loadState.first { it != PlannerLoadState.LOADING } },
        )
    }
}

private class GatedPlannerRepository(
    private val restored: DayWeaveUiState,
) : PlannerStateRepository {
    val saveStarted = Channel<Unit>(Channel.UNLIMITED)
    val allowSave = Channel<Unit>(Channel.UNLIMITED)
    val savedStates = mutableListOf<DayWeaveUiState>()

    override suspend fun load(): DayWeaveUiState = restored

    override suspend fun save(state: DayWeaveUiState) {
        saveStarted.send(Unit)
        allowSave.receive()
        savedStates += state
    }
}

private class ImmediatePlannerRepository(
    private var restored: DayWeaveUiState,
) : PlannerStateRepository {
    override suspend fun load(): DayWeaveUiState = restored

    override suspend fun save(state: DayWeaveUiState) {
        restored = state
    }
}

private class FailingSavePlannerRepository(
    private val restored: DayWeaveUiState,
) : PlannerStateRepository {
    override suspend fun load(): DayWeaveUiState = restored

    override suspend fun save(state: DayWeaveUiState) {
        error("synthetic encrypted claim save failure")
    }
}

private class CountingNotificationGateway : TimedBreakNotificationGateway {
    var posts = 0

    override fun post(identityDigest: String): TimedBreakNotificationPostResult {
        posts += 1
        return TimedBreakNotificationPostResult.POSTED
    }

    override fun cancel() = Unit
}

private fun timedBreakState(timedBreakEnded: Boolean = false): DayWeaveUiState {
    val session = CanonicalExecutionSessionSnapshot(
        id = SESSION_ID,
        itemId = ITEM_ID,
        itemRevision = 2,
        sessionIndex = 0,
        plannedBlockId = BLOCK_ID,
        sourceDeviceId = DEVICE_ID,
        status = "paused",
        revision = 3,
        accumulatedSeconds = 300,
        startedAt = "2026-09-01T06:00:00Z",
        pausedAt = "2026-09-01T06:05:00Z",
        pauseUntil = Instant.ofEpochMilli(DEADLINE).toString(),
        createdAt = "2026-09-01T06:00:00Z",
        updatedAt = "2026-09-01T06:05:00Z",
    )
    return DayWeaveUiState(
        canonicalExecutionRevision = 7,
        canonicalExecutionSession = session,
        activeSession = ActiveSession(
            itemId = BLOCK_ID,
            elapsedMinutes = 5,
            isPaused = true,
            accumulatedSeconds = 300,
            pauseUntilEpochMillis = DEADLINE,
            timedBreakEnded = timedBreakEnded,
            canonicalExecutionSessionId = SESSION_ID,
        ),
    )
}

private const val SESSION_ID = "11111111-1111-4111-8111-111111111111"
private const val ITEM_ID = "22222222-2222-4222-8222-222222222222"
private const val BLOCK_ID = "33333333-3333-4333-8333-333333333333"
private const val DEVICE_ID = "44444444-4444-4444-8444-444444444444"
private val DEADLINE = Instant.parse("2026-09-01T06:10:00Z").toEpochMilli()
