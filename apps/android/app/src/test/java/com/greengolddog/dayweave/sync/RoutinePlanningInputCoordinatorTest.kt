package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.scheduler.*
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.time.ZoneId
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.cancel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

/** Synthetic operation ownership and native store wiring, not production Device/TLS or JNI acceptance. */
class RoutinePlanningInputCoordinatorTest {
    private val instant = Instant.parse(ROUTINE_NOW)

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
