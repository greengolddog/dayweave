package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.scheduler.*
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.data.PlannerStateRepository
import java.time.Instant
import java.time.ZoneId
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.cancel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.launch
import kotlinx.coroutines.async
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

/** Synthetic operation ownership and native store wiring, not production Device/TLS or JNI acceptance. */
class RoutinePlanningInputCoordinatorTest {
    private val instant = Instant.parse(ROUTINE_NOW)

    @Test fun offlineFixedInputRecomputeHasNoTransportOrV1AndNeverReplacesCanonicalSchedule(): Unit = runBlocking {
        val base = readyState(); val capsule = planningDisplayCapsule(base)
        // A new process need not acquire fresh network/execution admission for an actionless view.
        val initial = base.copy(routinePlanningInputCapsule = capsule, canonicalExecutionHistoryVerified = false)
        val displayClock = Instant.parse(PLANNING_DISPLAY_NOW)
        val store = PlannerStore(initial, nowEpochMillis = { displayClock.toEpochMilli() + 1 })
        val before = store.state.value
        var helperCalls = 0
        val manager = manager(store, clock = { displayClock }, capture = { error("Offline composition cannot POST") },
            composer = RoutineLifecycleScheduleComposer { items, witness ->
                helperCalls++
                assertEquals(capsule.canonicalItems, items); assertEquals(capsule.witness, witness)
                planningDisplayComposition(capsule)
            })
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.composeSavedRoutinePlanningInput())
        assertEquals(1, helperCalls)
        val saved = requireNotNull(store.durableState.value?.routinePlanningDisplaySnapshot)
        assertEquals(ROUTINE_NOW, saved.capturedAt); assertEquals(PLANNING_DISPLAY_NOW, saved.computedAt)
        assertNotNull(store.state.value.routinePlanningDisplayAdmission)
        assertNull(store.durableState.value?.routinePlanningDisplayAdmission)
        assertEquals(before, store.state.value.copy(routinePlanningDisplaySnapshot = null, routinePlanningDisplayAdmission = null))
        assertTrue(store.state.value.requiresRemoteRoutineOccurrenceComposition())
        assertFalse(store.state.value.canonicalExecutionHistoryVerified)
        store.invalidateRoutineOccurrenceAuthority()
        assertNull(store.state.value.routinePlanningDisplayAdmission)
        assertEquals(saved, store.state.value.routinePlanningDisplaySnapshot)
    }

    @Test fun offlinePendingCatchupChangedSourceClockAndBindingCannotUseTheSavedInput(): Unit = runBlocking {
        val base = readyState(); val capsule = planningDisplayCapsule(base); val initial = base.copy(routinePlanningInputCapsule = capsule)
        val displayClock = Instant.parse(PLANNING_DISPLAY_NOW)
        val variants = listOf(initial.copy(routineOccurrenceLedger = initial.routineOccurrenceLedger.copy(needsRemoteScheduleCatchUp = true)),
            initial.copy(canonicalItems = initial.canonicalItems.map { it.copy(revision = 8) }),
            initial.copy(scheduleCompositionProfile = initial.scheduleCompositionProfile.copy(dayStartMinute = 1)))
        for (changed in variants) {
            val store = PlannerStore(changed, nowEpochMillis = { displayClock.toEpochMilli() + 1 })
            val manager = manager(store, clock = { displayClock }, capture = { error("No POST") },
                composer = RoutineLifecycleScheduleComposer { _, _ -> error("Stale capsule cannot run helper") })
            assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager.composeSavedRoutinePlanningInput())
            assertEquals(changed, store.state.value)
        }
        for (clock in listOf(instant.minusSeconds(1), instant.plusSeconds(86400))) {
            val store = PlannerStore(initial, nowEpochMillis = { clock.toEpochMilli() + 1 })
            assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager(store, clock = { clock }, composer = RoutineLifecycleScheduleComposer { _, _ -> error("Wrong clock") }).composeSavedRoutinePlanningInput())
            assertNull(store.state.value.routinePlanningDisplaySnapshot)
        }
        val credentials = Credentials().apply { binding = "replacement" }
        val store = PlannerStore(initial, nowEpochMillis = { displayClock.toEpochMilli() + 1 })
        assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager(store, credentials = credentials, clock = { displayClock }).composeSavedRoutinePlanningInput())
    }

    @Test fun offlineLateHelperCancellationPrivacyReadAbaAndCredentialChangesCannotInstallOrShow(): Unit = runBlocking {
        for (change in listOf("cancel", "privacy", "read_aba", "credentials", "clock")) {
            val base = readyState(); val capsule = planningDisplayCapsule(base); val initial = base.copy(routinePlanningInputCapsule = capsule)
            var clock = Instant.parse(PLANNING_DISPLAY_NOW)
            val store = PlannerStore(initial, nowEpochMillis = { clock.toEpochMilli() + 1 })
            val composed = planningDisplayComposition(capsule); val fence = Fence(); val credentials = Credentials()
            val manager = manager(store, credentials = credentials, clock = { clock }, fence = fence, capture = { error("No POST") },
                composer = RoutineLifecycleScheduleComposer { _, _ ->
                    when (change) {
                        "cancel" -> currentCoroutineContext().cancel()
                        "privacy" -> fence.current = false
                        "read_aba" -> { store.invalidateRoutineOccurrenceAuthority(); store.invalidateRoutineOccurrenceAuthority() }
                        "credentials" -> credentials.binding = "replacement"
                        else -> clock = clock.minusNanos(1)
                    }
                    composed
                })
            val job = launch(start = CoroutineStart.UNDISPATCHED) { assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager.composeSavedRoutinePlanningInput()) }
            job.join()
            assertNull(store.state.value.routinePlanningDisplaySnapshot)
            assertNull(store.state.value.routinePlanningDisplayAdmission)
            assertEquals(initial.routinePlanningInputCapsule, store.state.value.routinePlanningInputCapsule)
            assertEquals(initial.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
        }
    }

    @Test fun credentialReplacementDuringEncryptedSaveCannotAdmitADurablePreview(): Unit = runBlocking {
        val base = readyState(); val capsule = planningDisplayCapsule(base); val initial = base.copy(routinePlanningInputCapsule = capsule)
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) { entered.complete(Unit); release.await() }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val clock = Instant.parse(PLANNING_DISPLAY_NOW)
        try {
            val store = PlannerStore(initial, repository, scope, nowEpochMillis = { clock.toEpochMilli() + 1 })
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val credentials = Credentials()
            val manager = manager(store, credentials = credentials, clock = { clock }, capture = { error("No POST") },
                composer = RoutineLifecycleScheduleComposer { _, _ -> planningDisplayComposition(capsule) })
            val attempt = async { manager.composeSavedRoutinePlanningInput() }
            withTimeout(3_000) { entered.await() }
            credentials.binding = "replacement"; release.complete(Unit)
            assertNotEquals(CanonicalRefreshOutcome.SUCCESS, withTimeout(3_000) { attempt.await() })
            assertNotNull(store.durableState.value?.routinePlanningDisplaySnapshot)
            assertNull(store.state.value.routinePlanningDisplayAdmission)
            assertEquals(initial.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
        } finally { release.complete(Unit); scope.cancel() }
    }

    @Test fun connectedPreparationStoresExactRequestWithoutInstallingPublishingOrAcknowledgingHistory() = runBlocking {
        val store = store()
        val before = store.state.value
        var ownedRequest: RoutinePlanningWitnessRequest? = null
        val manager = manager(store, capture = { request ->
            ownedRequest = request
            planningTestResponse(witness(request))
        })
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager.prepareRoutinePlanningInput())
        val saved = requireNotNull(store.durableState.value?.routinePlanningInputCapsule)
        assertEquals(ownedRequest, saved.originalRequest())
        assertEquals(encodeRoutinePlanningWitnessRequest(requireNotNull(ownedRequest)).toString(Charsets.UTF_8), saved.originalRequestJson)
        assertEquals(before, store.state.value.copy(routinePlanningInputCapsule = null))
        assertEquals(ROUTINE_NOW, saved.witness.schedule.asOf)
        assertNull(store.state.value.localScheduleCompositionProvenance)
        assertTrue(manager.state.value.message.contains("No schedule was installed or published"))
        // A decoded/restarted artifact retains its original request, never a new clock or authority.
        val decoded = ROUTINE_CAPSULE_JSON.decodeFromString<RoutinePlanningInputCapsule>(ROUTINE_CAPSULE_JSON.encodeToString(saved))
        decoded.requireValid()
        assertEquals(saved, decoded)
        assertEquals(before.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
    }

    @Test fun remoteRequiredAndTransportFailureKeepPreviousStateAndNeverInvokeHelper() = runBlocking {
        for (reason in RoutinePlanningRemoteReason.entries) {
            val store = store(); val before = store.state.value
            val manager = manager(store, capture = { RoutinePlanningWitnessResponse(1, RoutinePlanningWitnessResult.RemoteRequired(reason)) },
                composer = RoutineLifecycleScheduleComposer { _, _ -> error("Remote-required cannot invoke a helper") })
            assertEquals(CanonicalRefreshOutcome.INVALID_LOCAL_STATE, manager.prepareRoutinePlanningInput())
            assertEquals(before, store.state.value)
        }
        val store = store(); val before = store.state.value
        val manager = manager(store, capture = { throw RoutinePlanningWitnessApiException.Uncertain() })
        assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager.prepareRoutinePlanningInput())
        assertEquals(before, store.state.value)
    }

    @Test fun sameCursorReadInvalidationAndPrivacyWithdrawalRejectLateWitnessAndHelper() = runBlocking {
        for (duringHelper in listOf(false, true)) for (invalidateRead in listOf(false, true)) {
            val store = store(); val lifecycle = Fence(); val before = store.state.value
            fun invalidate() { if (invalidateRead) store.invalidateRoutineOccurrenceAuthority() else lifecycle.current = false }
            val manager = manager(store, fence = lifecycle, capture = { request ->
                if (!duringHelper) invalidate()
                planningTestResponse(witness(request))
            }, composer = RoutineLifecycleScheduleComposer { _, witness ->
                if (duringHelper) invalidate()
                composition(witness)
            })
            assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager.prepareRoutinePlanningInput())
            assertNull(store.state.value.routinePlanningInputCapsule)
            assertEquals(before.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
            assertEquals(before.schedule, store.state.value.schedule)
        }
    }

    @Test fun clockRollbackNextDayAndCredentialChangeRejectInFlightCapture() = runBlocking {
        for (change in listOf("rollback", "next_day", "credential")) {
            val store = store(); val credentials = Credentials(); var clock = instant
            val before = store.state.value
            val manager = manager(store, credentials = credentials, clock = { clock }, capture = { request ->
                when (change) {
                    "rollback" -> clock = instant.minusNanos(1)
                    "next_day" -> clock = instant.plusSeconds(86400)
                    else -> credentials.binding = "synthetic-replacement-binding"
                }
                planningTestResponse(witness(request))
            })
            assertNotEquals(change, CanonicalRefreshOutcome.SUCCESS, manager.prepareRoutinePlanningInput())
            assertEquals(before, store.state.value)
        }
    }

    @Test fun pendingOccurrenceCatchupCannotPostOrEraseTheLatch() = runBlocking {
        val ready = readyState()
        val store = store(ready.copy(routineOccurrenceLedger = ready.routineOccurrenceLedger.copy(needsRemoteScheduleCatchUp = true)))
        val before = store.state.value
        val manager = manager(store, capture = { error("Pending catch-up must prevent POST") })
        assertNotEquals(CanonicalRefreshOutcome.SUCCESS, manager.prepareRoutinePlanningInput())
        assertEquals(before, store.state.value)
    }

    @Test fun callerCancellationWithoutLifecycleWithdrawalCannotCommitALateResponse() = runBlocking {
        for (duringHelper in listOf(false, true)) {
            val store = store(); val before = store.state.value; val fence = Fence()
            val manager = manager(store, fence = fence, capture = { request ->
                if (!duringHelper) currentCoroutineContext().cancel()
                planningTestResponse(witness(request))
            }, composer = RoutineLifecycleScheduleComposer { _, witness ->
                if (duringHelper) currentCoroutineContext().cancel()
                composition(witness)
            })
            val job = launch(start = CoroutineStart.UNDISPATCHED) { manager.prepareRoutinePlanningInput() }
            job.join()
            assertTrue(job.isCancelled)
            assertTrue(fence.current)
            assertEquals(before, store.state.value)
        }
    }

    @Test fun sameBindingScopeSubstitutionAndMismatchedHelperPreservePriorCapsule() = runBlocking {
        val store = store()
        assertEquals(CanonicalRefreshOutcome.SUCCESS, manager(store).prepareRoutinePlanningInput())
        val before = store.state.value
        val foreign = manager(store, capture = { request -> planningTestResponse(witness(request).copy(userId = PLANNING_PUBLICATION)) })
        assertNotEquals(CanonicalRefreshOutcome.SUCCESS, foreign.prepareRoutinePlanningInput())
        assertEquals(before, store.state.value)
        val wrongHelper = manager(store, composer = RoutineLifecycleScheduleComposer { _, witness ->
            composition(witness).copy(occurrenceSnapshotRevision = witness.occurrenceLifecycle.snapshotRevision + 1)
        })
        assertNotEquals(CanonicalRefreshOutcome.SUCCESS, wrongHelper.prepareRoutinePlanningInput())
        assertEquals(before, store.state.value)
    }

    private fun readyState() = DayWeaveUiState(
        canonicalItems = planningTestItems(), canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = BINDING,
        canonicalDeltaCursor = "synthetic-canonical-terminal", canonicalExecutionSyncOrigin = ORIGIN,
        canonicalExecutionConfigurationId = BINDING, canonicalExecutionHistoryVerified = true,
        canonicalExecutionHistoryContinuityEstablished = true, canonicalExecutionHistoryWindowRevision = 0,
        routineOccurrenceLedger = RoutineOccurrenceLedger(syncOrigin = ORIGIN, configurationId = BINDING,
            deltaCursor = planningTestRequest().terminalCursor),
        scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(firmHorizonDays = 1),
    )

    private fun store(state: DayWeaveUiState = readyState()) = PlannerStore(state, nowEpochMillis = { instant.toEpochMilli() + 1 })
    private fun witness(request: RoutinePlanningWitnessRequest) = planningTestWitness().copy(schedule = request.schedule,
        sourceItemRevisions = request.expectedSourceItemRevisions, terminalCursor = request.terminalCursor)
    private fun composition(witness: RoutinePlanningWitness) = RoutineOccurrenceLocalComposition(
        LocalScheduleComposition(witness.localInputFingerprint, "sha256:" + "e".repeat(64), witness.sourceItemRevisions.size,
            witness.sourceItemRevisions, witness.sourceItemRevisions.size, emptyList(), emptyList(),
            RemoteSchedulePlan(witness.schedule.asOf, witness.schedule.horizonStart, witness.schedule.horizonEnd,
                emptyList(), emptyList(), emptyList(), emptyList(), RemotePlanScore(0, 0, 0uL, 0), emptyList())),
        witness.occurrenceLifecycle.snapshotRevision)

    private fun manager(store: PlannerStore, credentials: Credentials = Credentials(), clock: () -> Instant = { instant },
        fence: Fence = Fence(), capture: suspend (RoutinePlanningWitnessRequest) -> RoutinePlanningWitnessResponse = { planningTestResponse(witness(it)) },
        composer: RoutineLifecycleScheduleComposer = RoutineLifecycleScheduleComposer { _, witness -> composition(witness) },
    ) = CanonicalSyncManager(store, credentials, ForbiddenCanonicalTransport, now = clock, zoneId = { ZoneId.of("UTC") },
        localCompositionLifecycleFence = fence, routinePlanningWitnessTransport = RoutinePlanningWitnessTransport { _, request -> capture(request) },
        routineLifecycleScheduleComposer = composer)

    private class Fence : LocalCompositionLifecycleFence {
        var current = true
        override fun captureGeneration() = 1L
        override fun isCurrent(generation: Long) = current && generation == 1L
    }
    private class Credentials : ApiCredentialStore {
        var binding = BINDING
        override fun snapshot() = ApiConnectionSnapshot(ORIGIN, true, null, binding)
        override fun authenticatedConfiguration() = AuthenticatedApiConfiguration.createBound(ORIGIN, "synthetic-planning-test-only", binding)
        override fun update(baseUrl: String, bearerToken: String?) = error("No credential mutation")
        override fun clear() = error("No credential mutation")
        override fun recordSuccessfulSync(epochMillis: Long) = error("Preparation cannot acknowledge canonical sync")
    }
    private object ForbiddenCanonicalTransport : CanonicalPlannerTransport {
        override suspend fun itemDelta(configuration: AuthenticatedApiConfiguration, cursor: String?): RemoteItemDeltaPage = error("No delta")
        override suspend fun currentSchedule(configuration: AuthenticatedApiConfiguration): RemoteCurrentPublishedSchedule? = error("No current schedule")
        override suspend fun preview(configuration: AuthenticatedApiConfiguration, request: SchedulePreviewRequest): RemoteSchedulePreview = error("No remote preview")
        override suspend fun publish(configuration: AuthenticatedApiConfiguration, request: SchedulePublishHttpRequest): RemoteSchedulePublishResponse = error("No publication")
        override suspend fun createItem(configuration: AuthenticatedApiConfiguration, idempotencyKey: String, request: CreateCanonicalItemRequest): RemoteCanonicalItem = error("No create")
        override suspend fun replaceItem(configuration: AuthenticatedApiConfiguration, id: String, idempotencyKey: String, request: ReplaceCanonicalItemRequest): RemoteCanonicalItem = error("No replace")
        override suspend fun trashItem(configuration: AuthenticatedApiConfiguration, id: String, idempotencyKey: String, expectedRevision: Long): RemoteCanonicalItem = error("No trash")
        override suspend fun restoreItem(configuration: AuthenticatedApiConfiguration, id: String, idempotencyKey: String, request: CanonicalItemRevisionRequest): RemoteCanonicalItem = error("No restore")
    }
    private companion object { const val ORIGIN = "https://api.example.test/"; const val BINDING = "synthetic-planning-binding" }
}
