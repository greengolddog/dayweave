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

class ItemProgressSyncManagerTest {
    @Test fun reviewAndSubmittedMarkerAreDurableBeforeTransmissionWithoutCanonicalEffects() = runBlocking {
        val store = PlannerStore(progressTestState())
        val before = store.state.value
        val transport = FakeProgressTransport()
        val manager = manager(store, transport)
        val components = listOf(ItemProgressComponent(PROGRESS_COMPONENT, "Synthetic complete percentage", ItemProgressValue.Percentage(10_000)))
        assertTrue(manager.stage(PROGRESS_ITEM, 7, 0, components))
        val reviewed = requireNotNull(store.durableState.value).itemProgressLedger.pending.single()
        assertNull(reviewed.submittedAt)
        assertEquals(emptyList<String>(), transport.bodies)
        transport.putHandler = { id, body ->
            assertEquals(body, requireNotNull(store.durableState.value).itemProgressLedger.pending.single().requestJson)
            assertNotNull(requireNotNull(store.durableState.value).itemProgressLedger.pending.single().submittedAt)
            success(id, body)
        }
        assertTrue(manager.replay())
        assertEquals(before, store.state.value.copy(itemProgressLedger = before.itemProgressLedger))
        assertTrue(store.state.value.itemProgressLedger.pending.isEmpty())
        assertFalse(store.state.value.itemProgressLedger.observations.getValue(PROGRESS_ITEM).isGetProof)
        assertNotNull(store.state.value.progressReviewIssue(PROGRESS_ITEM))
        transport.getHandler = { progressTestSnapshot(1).copy(components = components) }
        assertTrue(manager.load(PROGRESS_ITEM))
        assertNull(store.state.value.progressReviewIssue(PROGRESS_ITEM))
    }

    @Test fun lostResponseRestartsWithExactBytesEvenAfterItemDisappears() = runBlocking {
        val pending = progressTestMutation().let { it.copy(requestJson = " \n${it.requestJson}\n ") }
        val store = PlannerStore(progressTestState().copy(itemProgressLedger = progressTestLedger().copy(pending = listOf(pending))))
        val transport = FakeProgressTransport().apply { putHandler = { _, _ -> throw IOException("Synthetic loss") } }
        assertFalse(manager(store, transport).replay())
        val durable = requireNotNull(store.durableState.value)
        assertNotNull(durable.itemProgressLedger.pending.single().submittedAt)
        val restarted = PlannerStore(durable.copy(canonicalItems = emptyList()))
        transport.putHandler = { id, body -> success(id, body, replayed = true) }
        assertTrue(manager(restarted, transport).replay())
        assertEquals(listOf(pending.requestJson, pending.requestJson), transport.bodies)
        assertTrue(restarted.state.value.itemProgressLedger.pending.isEmpty())
        assertTrue(restarted.state.value.canonicalItems.isEmpty())
    }

    @Test fun lateHistoricalReplaySettlesOnlyItsOperationAndKeepsNewerGetObservation() = runBlocking {
        val pending = progressTestMutation(submitted = true)
        val store = PlannerStore(progressTestState().copy(itemProgressLedger = progressTestLedger().copy(pending = listOf(pending))))
        val newer = ItemProgressObservation(progressTestSnapshot(3, 9), PROGRESS_NOW, true)
        val transport = FakeProgressTransport().apply { putHandler = { id, body ->
            assertTrue(requireNotNull(store.mutateItemProgress { it.itemProgressLedger.withObservation(newer) }).awaitDurable())
            success(id, body, replayed = true)
        } }
        assertTrue(manager(store, transport).replay())
        assertTrue(store.state.value.itemProgressLedger.pending.isEmpty())
        assertEquals(newer, store.state.value.itemProgressLedger.observations[PROGRESS_ITEM])
    }

    @Test fun onlyDefinitiveRejectionAllowsExplicitDiscardAndStopsAutomaticReplay() = runBlocking {
        for (code in ItemProgressFailureCode.entries) {
            val store = PlannerStore(progressTestState())
            val transport = FakeProgressTransport().apply { putHandler = { _, _ -> throw ItemProgressApiException.Definitive(code) } }
            val manager = manager(store, transport)
            assertTrue(manager.stage(PROGRESS_ITEM, 7, 0, progressTestComponents()))
            assertFalse(manager.discardReviewed(PROGRESS_OPERATION))
            assertTrue(manager.replay())
            val expected = when (code) {
                ItemProgressFailureCode.ITEM_MISSING -> ItemProgressDisposition.ITEM_MISSING
                ItemProgressFailureCode.INVALID -> ItemProgressDisposition.REJECTED
                else -> ItemProgressDisposition.REVIEW_REQUIRED
            }
            assertEquals(expected, store.state.value.itemProgressLedger.pending.single().disposition)
            assertTrue(manager.replay())
            assertEquals(1, transport.bodies.size)
            assertTrue(store.hasCredentialReplacementBlocker())
            assertTrue(manager.discardReviewed(PROGRESS_OPERATION))
            assertFalse(store.hasCredentialReplacementBlocker())
        }
    }

    @Test fun transportAuthenticationAndMalformedResponseFailuresRetainSubmittedCustody() = runBlocking {
        for (failure in listOf(IOException("Synthetic gateway"), ItemProgressApiException.Authentication(), ItemProgressApiException.Uncertain())) {
            val store = PlannerStore(progressTestState())
            val transport = FakeProgressTransport().apply { putHandler = { _, _ -> throw failure } }
            val manager = manager(store, transport)
            assertTrue(manager.stage(PROGRESS_ITEM, 7, 0, progressTestComponents()))
            assertFalse(manager.replay())
            val saved = store.state.value.itemProgressLedger.pending.single()
            assertNotNull(saved.submittedAt)
            assertEquals(ItemProgressDisposition.PENDING, saved.disposition)
            assertFalse(manager.discardReviewed(PROGRESS_OPERATION))
            assertFalse(manager.replay())
            assertEquals(listOf(saved.requestJson, saved.requestJson), transport.bodies)
        }
    }

    @Test fun changedCanonicalOrProgressBaselineBeforeFirstSendRequiresReviewWithoutRebase() = runBlocking {
        val pending = progressTestMutation()
        val variants = listOf(progressTestState().copy(canonicalItems = listOf(progressTestItem().copy(revision = 8))),
            progressTestState().copy(itemProgressLedger = progressTestLedger(progressTestSnapshot(2))))
        for (state in variants) {
            val store = PlannerStore(state.copy(itemProgressLedger = state.itemProgressLedger.copy(pending = listOf(pending))))
            val transport = FakeProgressTransport()
            assertTrue(manager(store, transport).replay())
            assertEquals(pending.copy(disposition = ItemProgressDisposition.REVIEW_REQUIRED), store.state.value.itemProgressLedger.pending.single())
            assertTrue(transport.bodies.isEmpty())
        }
    }

    @Test fun failedGetPreservesBothConfirmedObservationAndReviewedIntent() = runBlocking {
        val state = progressTestState().copy(itemProgressLedger = progressTestLedger(progressTestSnapshot(2))
            .copy(pending = listOf(progressTestMutation(submitted = true))))
        val store = PlannerStore(state)
        val transport = FakeProgressTransport().apply { getHandler = { throw IOException("Synthetic offline") } }
        assertFalse(manager(store, transport).load(PROGRESS_ITEM))
        assertEquals(state.itemProgressLedger, store.state.value.itemProgressLedger)
    }

    @Test fun changedCredentialBindingRejectsLateGetWithoutReplacingOldObservation() = runBlocking {
        val store = PlannerStore(progressTestState())
        val credentials = GenerationBoundCredentialStore()
        val transport = FakeProgressTransport().apply { getHandler = {
            credentials.configurationId = "synthetic-other-binding"
            progressTestSnapshot(2)
        } }
        assertFalse(manager(store, transport, credentials).load(PROGRESS_ITEM))
        assertEquals(progressTestLedger(), store.state.value.itemProgressLedger)
    }

    @Test fun failedDurableReviewRollsBackAndNeverTransmits() = runBlocking {
        val initial = progressTestState()
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) {
                if (state.itemProgressLedger.pending.isNotEmpty()) throw IOException("Synthetic disk failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val transport = FakeProgressTransport()
            assertFalse(withTimeout(3_000) { manager(store, transport).stage(PROGRESS_ITEM, 7, 0, progressTestComponents()) })
            assertEquals(PlannerLoadState.PERSISTENCE_FAILED, store.loadState.value)
            assertTrue(store.state.value.itemProgressLedger.pending.isEmpty())
            assertEquals(initial.itemProgressLedger, store.durableState.value?.itemProgressLedger)
            assertTrue(transport.bodies.isEmpty())
        } finally { scope.cancel() }
    }

    @Test fun failedDurableSubmittedMarkerPreservesUnsubmittedBytesAndPreventsTransmission() = runBlocking {
        val pending = progressTestMutation()
        val initial = progressTestState().copy(itemProgressLedger = progressTestLedger().copy(pending = listOf(pending)))
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) {
                if (state.itemProgressLedger.pending.any { it.submittedAt != null }) throw IOException("Synthetic disk failure")
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val transport = FakeProgressTransport()
            assertFalse(withTimeout(3_000) { manager(store, transport).replay() })
            assertEquals(listOf(pending), store.state.value.itemProgressLedger.pending)
            assertEquals(listOf(pending), store.durableState.value?.itemProgressLedger?.pending)
            assertTrue(transport.bodies.isEmpty())
        } finally { scope.cancel() }
    }

    @Test fun cancellationAfterDurableSubmissionPreservesExactReplayCustody() = runBlocking {
        val store = PlannerStore(progressTestState())
        val transport = FakeProgressTransport().apply { putHandler = { _, _ -> throw CancellationException("Synthetic cancellation") } }
        val manager = manager(store, transport)
        assertTrue(manager.stage(PROGRESS_ITEM, 7, 0, progressTestComponents()))
        assertThrows(CancellationException::class.java) { runBlocking { manager.replay() } }
        val saved = store.state.value.itemProgressLedger.pending.single()
        assertNotNull(saved.submittedAt)
        assertEquals(ItemProgressDisposition.PENDING, saved.disposition)
        assertEquals(saved.requestJson, transport.bodies.single())
    }

    @Test fun privacyCanHardenWhileExactOperationIsInFlightWithoutBlockingSettlement() = runBlocking {
        val store = PlannerStore(progressTestState())
        val transport = FakeProgressTransport().apply { putHandler = { id, body ->
            assertTrue(requireNotNull(store.mutateItemProgress { state -> state.itemProgressLedger.copy(
                pending = state.itemProgressLedger.pending.map { it.copy(wasSensitive = true) }) }).awaitDurable())
            success(id, body)
        } }
        val manager = manager(store, transport)
        assertTrue(manager.stage(PROGRESS_ITEM, 7, 0, progressTestComponents()))
        assertTrue(manager.replay())
        assertTrue(store.state.value.itemProgressLedger.pending.isEmpty())
    }

    private fun manager(store: PlannerStore, transport: FakeProgressTransport,
        credentials: GenerationBoundCredentialStore = GenerationBoundCredentialStore(),
    ) = ItemProgressSyncManager(store, credentials, transport, now = { Instant.parse(PROGRESS_NOW) },
        uuid = { UUID.fromString(PROGRESS_OPERATION) })

    private fun success(itemId: String, body: String, replayed: Boolean = false): ItemProgressMutationResult {
        val request = decodeExactItemProgress<ItemProgressRequest>(body)
        return ItemProgressMutationResult(request.operationId, replayed, ItemProgressSnapshot(1, itemId,
            request.expectedItemRevision, request.expectedProgressRevision + 1, request.components, PROGRESS_NOW))
    }

    private inner class FakeProgressTransport : ItemProgressTransport {
        val bodies = mutableListOf<String>()
        var getHandler: suspend (String) -> ItemProgressSnapshot = { progressTestSnapshot() }
        var putHandler: suspend (String, String) -> ItemProgressMutationResult = { id, body -> success(id, body) }
        override suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String) = getHandler(itemId)
        override suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemProgressMutationResult {
            bodies += requestJson
            return putHandler(itemId, requestJson)
        }
    }
}
