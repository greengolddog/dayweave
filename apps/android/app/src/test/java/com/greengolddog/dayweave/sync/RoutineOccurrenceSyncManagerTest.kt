package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.junit.Assert.*
import org.junit.Test

class RoutineOccurrenceSyncManagerTest {
    private val selected = RoutineOccurrenceSelection(ROUTINE_ROOT, ROUTINE_PLANNER_ID)

    @Test fun uncachedSelectionRequiresExactLookupAndLiveLeaseBeforeStaging() = runBlocking {
        val store = PlannerStore(routineStateTestUi(RoutineOccurrenceLedger()))
        val transport = Fake()
        val manager = manager(store, transport)
        assertFalse(manager.stage(selected, routineTestSnapshot(), ROUTINE_CHILD, RoutineOccurrenceAction.SetOutcome("completed")))
        manager.select(selected)
        assertTrue(manager.load(selected))
        assertEquals(listOf(selected), transport.lookups)
        assertTrue(store.state.value.routineOccurrenceLedger.needsRemoteScheduleCatchUp)
        assertFalse(manager.stage(selected, routineTestSnapshot(), ROUTINE_CHILD, RoutineOccurrenceAction.SetOutcome("completed")))
    }

    @Test fun currentSelectedReviewStagesOneExactInstanceIntentAndRejectsSiblingCas() = runBlocking {
        val store = PlannerStore(routineStateTestUi())
        val manager = manager(store, Fake())
        manager.select(selected)
        assertTrue(manager.load(selected))
        assertTrue(manager.stage(selected, routineTestSnapshot(), ROUTINE_CHILD, RoutineOccurrenceAction.SetOutcome("completed")))
        val saved = store.state.value.routineOccurrenceLedger.pending.single()
        assertEquals(ROUTINE_INSTANCE, saved.instanceId)
        assertNull(saved.submittedAt)
        assertEquals(routineTestRequest(), saved.request)
        assertFalse(manager.stage(selected, routineTestSnapshot(), ROUTINE_OPTIONAL, RoutineOccurrenceAction.SetOutcome("skipped")))
        assertEquals(1, store.state.value.routineOccurrenceLedger.pending.size)
    }

    @Test fun neverSubmittedRestartRequiresFreshGetAndNeverRebasesEvidence() = runBlocking {
        val saved = routineStateTestIntent()
        val store = PlannerStore(routineStateTestUi(routineStateTestLedger().copy(pending = listOf(saved))))
        val transport = Fake().apply { read = { throw IOException("Synthetic offline") } }
        val manager = manager(store, transport)
        assertFalse(manager.replay())
        assertEquals(saved, store.state.value.routineOccurrenceLedger.pending.single())
        assertTrue(transport.bodies.isEmpty())
        transport.read = { routineTestSnapshot().copy(evidenceHash = "sha256:" + "b".repeat(64)) }
        assertTrue(manager.replay())
        assertEquals(saved.copy(disposition = RoutineOccurrenceDisposition.REVIEW_REQUIRED), store.state.value.routineOccurrenceLedger.pending.single())
        assertTrue(transport.bodies.isEmpty())
    }

    @Test fun lostReplyReplaysOriginalBytesAfterRestartWithoutCurrentSourceOrGet() = runBlocking {
        val saved = routineStateTestIntent()
        val store = PlannerStore(routineStateTestUi(routineStateTestLedger().copy(pending = listOf(saved))))
        val transport = Fake().apply { write = { throw IOException("Synthetic lost reply") } }
        assertFalse(manager(store, transport).replay())
        val submitted = store.state.value.routineOccurrenceLedger.pending.single()
        assertNotNull(submitted.submittedAt)
        val newer = routineTestSnapshot(true).let { it.copy(aggregate = it.aggregate.copy(revision = 3), freshEditEligible = false) }
        val restart = PlannerStore(store.state.value.copy(canonicalItems = emptyList(),
            routineOccurrenceLedger = store.state.value.routineOccurrenceLedger.observeRoutineOccurrence(RoutineOccurrenceObservation(newer, ROUTINE_NOW))))
        transport.read = { error("Submitted requests never need a GET") }
        transport.write = { routineTestMutation(true) }
        assertTrue(manager(restart, transport).replay())
        assertEquals(listOf(saved.requestJson, saved.requestJson), transport.bodies)
        assertEquals(newer, restart.state.value.routineOccurrenceLedger.observations[ROUTINE_INSTANCE]?.snapshot)
        assertEquals(mapOf(ROUTINE_INSTANCE to 2L), restart.state.value.routineOccurrenceLedger.minimumCatchUpRevisions)
        assertTrue(restart.hasCredentialReplacementBlocker())
    }

    @Test fun onlyAdmittedPutRejectionMakesSubmittedIntentDiscardable() = runBlocking {
        val saved = routineStateTestIntent(true)
        val store = PlannerStore(routineStateTestUi(routineStateTestLedger().copy(pending = listOf(saved))))
        val transport = Fake().apply { write = { throw RoutineOccurrenceApiException.Uncertain() } }
        val manager = manager(store, transport)
        assertFalse(manager.replay())
        assertFalse(manager.discard(saved.operationId))
        assertEquals(saved, store.state.value.routineOccurrenceLedger.pending.single())
        transport.write = { throw RoutineOccurrenceApiException.Definitive(RoutineOccurrenceFailureCode.INSTANCE_STALE) }
        assertTrue(manager.replay())
        assertEquals(RoutineOccurrenceDisposition.REVIEW_REQUIRED, store.state.value.routineOccurrenceLedger.pending.single().disposition)
        assertTrue(manager.discard(saved.operationId))
    }

    @Test fun getMissingDoesNotResolveOriginalCommandOrPretendNoPutEffect() = runBlocking {
        val saved = routineStateTestIntent()
        val store = PlannerStore(routineStateTestUi(routineStateTestLedger().copy(pending = listOf(saved))))
        val transport = Fake().apply { read = { throw RoutineOccurrenceApiException.Definitive(RoutineOccurrenceFailureCode.OCCURRENCE_MISSING) } }
        assertFalse(manager(store, transport).replay())
        assertEquals(saved, store.state.value.routineOccurrenceLedger.pending.single())
    }

    @Test fun terminalHistoryDrainsWhilePendingAndMissingReceiptCoverageRetainsCursorUntilColdRecovery() = runBlocking {
        val saved = routineStateTestIntent(true)
        val initial = routineStateTestLedger().copy(pending = listOf(saved), deltaCursor = "DWR1.before")
        val store = PlannerStore(routineStateTestUi(initial))
        val transport = Fake()
        val manager = manager(store, transport)
        assertTrue(manager.refresh())
        assertEquals(listOf(saved), store.state.value.routineOccurrenceLedger.pending)
        assertTrue(manager.replay())
        val afterReceipt = store.state.value.routineOccurrenceLedger
        transport.pages = { RoutineOccurrencePage(1, emptyList(), "DWR1.newer", false) }
        assertFalse(manager.refresh())
        assertEquals(afterReceipt, store.state.value.routineOccurrenceLedger)
        transport.pages = { routineTestPage().copy(cursor = "DWR1.cold", changes = listOf(RoutineOccurrenceChange(2, routineTestMutation().occurrence))) }
        assertTrue(manager.refresh(cold = true))
        assertEquals("DWR1.cold", store.state.value.routineOccurrenceLedger.deltaCursor)
        assertTrue(store.state.value.routineOccurrenceLedger.minimumCatchUpRevisions.isEmpty())
        assertTrue(store.state.value.routineOccurrenceLedger.needsRemoteScheduleCatchUp)
    }

    @Test fun lateSelectedResponseCannotOutliveSelectionPrivacyOrCanonicalAba() = runBlocking {
        for (boundary in 0..2) {
            val store = PlannerStore(routineStateTestUi(RoutineOccurrenceLedger()))
            val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
            var visible = true
            val transport = Fake().apply { read = { withContext(NonCancellable) { entered.complete(Unit); release.await(); routineTestSnapshot() } } }
            val manager = manager(store, transport)
            manager.select(selected)
            val work = async { manager.load(selected) { visible } }
            entered.await()
            when (boundary) {
                0 -> { manager.select(null); manager.select(selected) }
                1 -> { visible = false; manager.quarantineBindingState() }
                else -> store.invalidateItemCompletionReadProofs()
            }
            release.complete(Unit)
            assertFalse(work.await())
            assertNull(manager.state.value.reviewed)
            assertTrue(store.state.value.routineOccurrenceLedger.observations.isEmpty())
        }
    }

    @Test fun oldSelectedSessionCleanupCannotClearReplacementSelection() = runBlocking {
        val store = PlannerStore(routineStateTestUi())
        val manager = manager(store, Fake())
        val old = manager.select(selected)
        val replacement = manager.select(selected)
        manager.clearSelection(old)
        assertEquals(selected, manager.state.value.selection)
        assertTrue(manager.load(selected))
        manager.clearSelection(old)
        assertNotNull(manager.state.value.reviewed)
        manager.clearSelection(replacement)
        assertNull(manager.state.value.selection)
        assertNull(manager.state.value.reviewed)
    }

    @Test fun restoredRepositoryAndInitialStateCannotMintDeferAdmission() = runBlocking {
        val admission = RoutineOccurrenceDeferAdmission(ROUTINE_HASH, 9, ROUTINE_INSTANCE, PROGRESS_ORIGIN, PROGRESS_CONFIGURATION)
        val original = routineStateTestUi().copy(routineOccurrenceAuthorityGeneration = 9, routineOccurrenceDeferAdmission = admission)
        assertNull(PlannerStore(original).state.value.routineOccurrenceDeferAdmission)
        val repository = object : PlannerStateRepository {
            override suspend fun load() = original
            override suspend fun save(state: DayWeaveUiState) = Unit
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(original, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            assertNull(store.state.value.routineOccurrenceDeferAdmission)
            assertNull(store.durableState.value?.routineOccurrenceDeferAdmission)
            assertEquals(0L, store.state.value.routineOccurrenceAuthorityGeneration)
        } finally { scope.cancel() }
    }

    @Test fun originalSubmittedSaveIsDurableBeforePutAndLateFencePreventsFirstSend() = runBlocking {
        val initial = routineStateTestUi(routineStateTestLedger().copy(pending = listOf(routineStateTestIntent())))
        val saving = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) {
                if (state.routineOccurrenceLedger.pending.any { it.submittedAt != null }) { saving.complete(Unit); release.await() }
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val transport = Fake(); val manager = manager(store, transport)
            val work = async { manager.replay() }
            withTimeout(3_000) { saving.await() }
            store.invalidateRoutineOccurrenceAuthority()
            release.complete(Unit)
            assertFalse(withTimeout(3_000) { work.await() })
            assertTrue(transport.bodies.isEmpty())
            assertNotNull(store.state.value.routineOccurrenceLedger.pending.single().submittedAt)
            assertTrue(manager.replay())
        } finally { release.complete(Unit); scope.cancel() }
    }

    @Test fun headChangeWithoutCanonicalDeltaKeepsDurableScheduleLatchAndCannotUseOldProof() = runBlocking {
        val store = PlannerStore(routineStateTestUi())
        val before = store.state.value.canonicalDeltaCursor
        val manager = manager(store, Fake())
        assertTrue(manager.refresh())
        assertEquals(before, store.state.value.canonicalDeltaCursor)
        assertTrue(store.state.value.routineOccurrenceLedger.needsRemoteScheduleCatchUp)
        var composed = false
        assertFalse(manager.catchUpSchedule { composed = true; null })
        assertTrue(composed)
        assertTrue(store.state.value.routineOccurrenceLedger.needsRemoteScheduleCatchUp)
    }

    private fun manager(store: PlannerStore, transport: Fake) = RoutineOccurrenceSyncManager(store,
        GenerationBoundCredentialStore(), transport, now = { Instant.parse(ROUTINE_NOW) }, uuid = { UUID.fromString(ROUTINE_OPERATION) })
    private class Fake : RoutineOccurrenceTransport {
        val bodies = mutableListOf<String>()
        val lookups = mutableListOf<RoutineOccurrenceSelection>()
        var read: suspend () -> RoutineOccurrenceSnapshot = { routineTestSnapshot() }
        var write: suspend () -> RoutineOccurrenceMutationResult = { routineTestMutation() }
        var pages: suspend () -> RoutineOccurrencePage = { routineTestPage() }
        override suspend fun lookup(configuration: AuthenticatedApiConfiguration, seriesItemId: String, occurrenceId: String): RoutineOccurrenceSnapshot {
            lookups += RoutineOccurrenceSelection(seriesItemId, occurrenceId); return read()
        }
        override suspend fun get(configuration: AuthenticatedApiConfiguration, instanceId: String) = read()
        override suspend fun put(configuration: AuthenticatedApiConfiguration, instanceId: String, memberId: String, requestJson: String): RoutineOccurrenceMutationResult {
            bodies += requestJson; return write()
        }
        override suspend fun list(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int) = pages()
        override suspend fun delta(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int) = pages()
    }
}
