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

class ItemCompletionSyncManagerTest {
    @Test fun lostReceiptReplaysExactBytesAfterRestartAndNewerMissingCanonicalState() = runBlocking {
        val store = PlannerStore(completionTestState(false))
        val transport = Fake().apply { write = { throw IOException("Synthetic lost response") } }
        val manager = manager(store, transport)
        assertTrue(manager.load(PROGRESS_ITEM))
        assertTrue(manager.stage(PROGRESS_ITEM, completionTestSnapshot(), true, ItemCompletionMode.KEEP_OPEN))
        assertFalse(manager.replay())
        val saved = store.state.value.itemCompletionLedger.pending.single()
        assertNotNull(saved.submittedAt)
        val restart = PlannerStore(store.state.value.copy(canonicalItems = emptyList(), itemCompletionGetProofs = emptyMap()))
        transport.read = { error("Submitted replay must not read") }
        transport.write = { completionTestResult(true) }
        assertTrue(manager(restart, transport).replay())
        assertEquals(listOf(saved.requestJson, saved.requestJson), transport.bodies)
        assertTrue(restart.state.value.canonicalItems.isEmpty())
        assertTrue(restart.state.value.itemCompletionLedger.pending.isEmpty())
        assertTrue(restart.state.value.itemCompletionLedger.needsCanonicalCatchUp)
        assertTrue(restart.state.value.itemCompletionGetProofs.isEmpty())
    }

    @Test fun neverSubmittedRestartRefreshesExactExpectedEvidenceAndRetainsOfflineIntent() = runBlocking {
        val original = completionTestState(false).copy(itemCompletionLedger = completionTestLedger().copy(pending = listOf(completionTestMutation())))
        val store = PlannerStore(original)
        val transport = Fake().apply { read = { throw IOException("Synthetic offline") } }
        assertFalse(manager(store, transport).replay())
        assertEquals(original.itemCompletionLedger.pending, store.state.value.itemCompletionLedger.pending)
        assertTrue(transport.bodies.isEmpty())
        transport.read = { completionTestSnapshot() }
        assertTrue(manager(store, transport).replay())
        assertEquals(listOf(completionTestMutation().requestJson), transport.bodies)
    }

    @Test fun verifiedGlobalDriftRequiresExplicitReviewWithoutRebasingBytes() = runBlocking {
        val saved = completionTestMutation()
        val store = PlannerStore(completionTestState(false).copy(itemCompletionLedger = completionTestLedger().copy(pending = listOf(saved))))
        val transport = Fake().apply { read = { completionTestSnapshot().copy(evidenceHash = COMPLETION_HASH.replace('1', '2')) } }
        assertTrue(manager(store, transport).replay())
        assertEquals(saved.copy(disposition = ItemCompletionDisposition.REVIEW_REQUIRED), store.state.value.itemCompletionLedger.pending.single())
        assertTrue(transport.bodies.isEmpty())
        assertTrue(store.state.value.itemCompletionLedger.needsCanonicalCatchUp)
    }

    @Test fun missingAndRevisionMismatchInvalidateEveryProofAndRetainPendingCustody() = runBlocking {
        for (missing in listOf(false, true)) {
            val store = PlannerStore(completionTestState())
            val transport = Fake().apply { read = {
                if (missing) throw ItemCompletionApiException.Definitive(ItemCompletionFailureCode.ITEM_MISSING)
                completionTestSnapshot(1, 8)
            } }
            assertFalse(manager(store, transport).load(PROGRESS_ITEM))
            assertTrue(store.state.value.itemCompletionLedger.needsCanonicalCatchUp)
            assertTrue(store.state.value.itemCompletionGetProofs.isEmpty())
            assertNull(store.state.value.currentCompletionProof(PROGRESS_ITEM))
        }
    }

    @Test fun genericErrorRetainsExactSubmittedIntentAndForbidsDiscard() = runBlocking {
        val saved = completionTestMutation(true)
        val store = PlannerStore(completionTestState(false).copy(itemCompletionLedger = completionTestLedger().copy(pending = listOf(saved))))
        val transport = Fake().apply { write = { throw ItemCompletionApiException.Uncertain() } }
        val manager = manager(store, transport)
        assertFalse(manager.replay())
        assertFalse(manager.discardReviewed(saved.operationId))
        assertEquals(saved, store.state.value.itemCompletionLedger.pending.single())
        assertTrue(store.hasCredentialReplacementBlocker())
    }

    @Test fun staleEditorCannotReplaceNewerReviewedLocalIntentAtSameGet() = runBlocking {
        val store = PlannerStore(completionTestState())
        val manager = manager(store, Fake())
        val saved = completionTestMutation().copy(disposition = ItemCompletionDisposition.REVIEW_REQUIRED)
        assertTrue(requireNotNull(store.mutateItemCompletion { it.copy(itemCompletionLedger = it.itemCompletionLedger.copy(pending = listOf(saved))) }).awaitDurable())
        assertTrue(manager.load(PROGRESS_ITEM))
        assertFalse(manager.stage(PROGRESS_ITEM, completionTestSnapshot(), false, ItemCompletionMode.AUTOMATIC, null))
        assertFalse(manager.discardReviewed(PROGRESS_COMPONENT))
        assertEquals(saved.requestJson, store.state.value.itemCompletionLedger.pending.single().requestJson)
        assertTrue(manager.discardReviewed(saved.operationId))
    }

    @Test fun cancellationInsensitiveReadRetainsLeaseUntilDrainAndCannotCommitAfterQuarantine() = runBlocking {
        val store = PlannerStore(completionTestState(false))
        val started = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        val transport = Fake().apply { read = { withContext(NonCancellable) { started.complete(Unit); release.await(); completionTestSnapshot() } } }
        val manager = manager(store, transport)
        val old = launch { manager.load(PROGRESS_ITEM) }
        withTimeout(2_000) { started.await() }
        old.cancel(); yield()
        assertTrue(manager.state.value.isBusy)
        assertTrue(manager.replay())
        assertTrue(manager.state.value.isBusy)
        manager.quarantineBindingState()
        release.complete(Unit); old.join()
        assertEquals(ItemCompletionSyncState(), manager.state.value)
        assertTrue(store.state.value.itemCompletionGetProofs.isEmpty())
    }

    @Test fun diskFailureAtReviewSubmissionOrSettlementRestoresDurableCustody() = runBlocking {
        for (failure in listOf("review", "submission", "settlement")) {
            val initial = completionTestState(false).let { state -> if (failure == "review") state else state.copy(
                itemCompletionLedger = state.itemCompletionLedger.copy(pending = listOf(completionTestMutation(failure == "settlement")))) }
            val repository = object : PlannerStateRepository {
                override suspend fun load() = initial
                override suspend fun save(state: DayWeaveUiState) {
                    if (when (failure) {
                        "review" -> state.itemCompletionLedger.pending.isNotEmpty()
                        "submission" -> state.itemCompletionLedger.pending.any { it.submittedAt != null }
                        else -> state.itemCompletionLedger.pending.isEmpty()
                    }) throw IOException("Synthetic disk failure")
                }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val store = PlannerStore(initial, repository, scope)
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
                val transport = Fake(); val manager = manager(store, transport)
                val result = if (failure == "review") {
                    assertTrue(manager.load(PROGRESS_ITEM)); manager.stage(PROGRESS_ITEM, completionTestSnapshot(), true, ItemCompletionMode.KEEP_OPEN)
                } else manager.replay()
                assertFalse(result)
                assertEquals(initial.itemCompletionLedger.pending, store.state.value.itemCompletionLedger.pending)
                assertEquals(if (failure == "settlement") 1 else 0, transport.bodies.size)
                assertTrue(store.state.value.itemCompletionGetProofs.isEmpty())
            } finally { scope.cancel() }
        }
    }

    @Test fun authorityChangesDuringSubmittedSavePreventFirstPutButRetainExactReplayCustody() = runBlocking {
        val saving = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        val initial = completionTestState(false).copy(itemCompletionLedger = completionTestLedger().copy(pending = listOf(completionTestMutation())))
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) {
                if (state.itemCompletionLedger.pending.any { it.submittedAt != null }) { saving.complete(Unit); release.await() }
            }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val transport = Fake(); val subject = manager(store, transport)
            val first = async { subject.replay() }
            withTimeout(3_000) { saving.await() }
            store.invalidateItemCompletionReadProofs()
            release.complete(Unit)
            assertFalse(withTimeout(3_000) { first.await() })
            assertTrue(transport.bodies.isEmpty())
            assertNotNull(store.state.value.itemCompletionLedger.pending.single().submittedAt)
            assertTrue(subject.replay())
            assertEquals(listOf(completionTestMutation().requestJson), transport.bodies)
        } finally { release.complete(Unit); scope.cancel() }
    }

    @Test fun settledReceiptStillBlocksCredentialRemovalUntilTerminalCanonicalCatchUp() = runBlocking {
        val store = PlannerStore(completionTestState(false).copy(itemCompletionLedger = completionTestLedger().copy(
            pending = listOf(completionTestMutation(true)))))
        assertTrue(manager(store, Fake()).replay())
        assertTrue(store.state.value.itemCompletionLedger.pending.isEmpty())
        assertTrue(store.hasCredentialReplacementBlocker())
        assertThrows(IllegalArgumentException::class.java) { store.abandonCanonicalConnection() }
        val restart = PlannerStore(store.state.value.copy(itemCompletionGetProofs = emptyMap()))
        assertTrue(restart.hasCredentialReplacementBlocker())
        val expected = restart.state.value
        assertTrue(requireNotNull(restart.installItemProgressCanonicalEvidence(expected,
            expected.canonicalItems.map { it.copy(revision = 8, splitPolicyJson = "{\"type\":\"indivisible\"}") },
            "synthetic-completion-terminal", { true })).awaitDurable())
        assertFalse(restart.state.value.itemCompletionLedger.needsCanonicalCatchUp)
        assertFalse(restart.hasCredentialReplacementBlocker())
    }

    private fun manager(store: PlannerStore, transport: Fake) = ItemCompletionSyncManager(store, GenerationBoundCredentialStore(), transport,
        now = { Instant.parse(PROGRESS_NOW) }, uuid = { UUID.fromString(PROGRESS_OPERATION) })
    private class Fake : ItemCompletionTransport {
        val bodies = mutableListOf<String>()
        var read: suspend () -> ItemCompletionSnapshot = { completionTestSnapshot() }
        var write: suspend () -> ItemCompletionMutationResult = { completionTestResult() }
        override suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String) = read()
        override suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemCompletionMutationResult {
            bodies += requestJson; return write()
        }
    }
}
