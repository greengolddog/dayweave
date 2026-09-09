package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.time.temporal.ChronoUnit
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.job
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

data class ItemCompletionSyncState(
    val isBusy: Boolean = false,
    val message: String = "Completion policy is separate from independent progress",
    val failed: Boolean = false,
)

/** Exact encrypted commands survive uncertainty; current GET permission never survives restart. */
class ItemCompletionSyncManager(
    private val store: PlannerStore,
    private val credentials: ApiCredentialStore,
    private val transport: ItemCompletionTransport,
    private val now: () -> Instant = Instant::now,
    private val uuid: () -> UUID = UUID::randomUUID,
) {
    private val mutex = Mutex()
    private val presentationLock = Any()
    private val generation = AtomicLong()
    private val mutableState = MutableStateFlow(ItemCompletionSyncState())
    val state = mutableState.asStateFlow()

    internal fun quarantineBindingState() = synchronized(presentationLock) {
        generation.incrementAndGet()
        store.invalidateItemCompletionReadProofs()
        mutableState.value = ItemCompletionSyncState()
    }

    suspend fun load(itemId: String, isCurrent: () -> Boolean = { true }): Boolean =
        operation("Loading completion evidence…", isCurrent) { configuration, active ->
            require(store.state.value.completionItem(itemId) != null)
            readCurrent(configuration, itemId, active)
        }

    suspend fun stage(itemId: String, reviewed: ItemCompletionSnapshot, requiredForParent: Boolean,
        mode: ItemCompletionMode, replacingOperationId: String? = null,
    ): Boolean = operation("Saving reviewed completion intent securely…") { configuration, active ->
        require(store.state.value.currentCompletionProof(itemId)?.snapshot == reviewed)
        val request = ItemCompletionRequest(operationId = uuid().toString(), expectedItemRevision = reviewed.itemRevision,
            expectedCompletionRevision = reviewed.state.revision, expectedEvidenceHash = reviewed.evidenceHash,
            requiredForParent = requiredForParent, mode = mode).also { it.requireValid(itemId) }
        val pending = PendingItemCompletionMutation(operationId = request.operationId, itemId = itemId,
            syncOrigin = configuration.baseUrl.toString(), configurationId = requireNotNull(configuration.configurationId),
            requestJson = ITEM_PROGRESS_JSON.encodeToString(request), createdAt = instant(),
            wasSensitive = store.state.value.completionReviewSensitive(itemId))
        persist(configuration, active) { current ->
            require(current.currentCompletionProof(itemId)?.snapshot == reviewed)
            current.copy(itemCompletionLedger = current.stageItemCompletion(pending, replacingOperationId))
        }
    }

    suspend fun replay(isCurrent: () -> Boolean = { true }): Boolean {
        if (store.loadState.value == PlannerLoadState.READY && store.state.value.itemCompletionLedger.pending.none {
                it.disposition == ItemCompletionDisposition.PENDING }) return true
        return operation("Synchronizing exact completion changes…", isCurrent) { configuration, active ->
            for (saved in store.state.value.itemCompletionLedger.pending.filter { it.disposition == ItemCompletionDisposition.PENDING }) {
                var pending = saved
                if (pending.submittedAt == null) {
                    // Missing runtime proof is not a rejection. An unknown/offline read retains
                    // the never-submitted command; only verified drift requires explicit review.
                    if (store.state.value.itemCompletionLedger.needsCanonicalCatchUp) throw CatchUpRequired()
                    require(!store.state.value.hasCompletionMutationBlocker())
                    val fresh = try { readCurrent(configuration, pending.itemId, active) }
                    catch (error: ItemCompletionApiException.Definitive) {
                        if (error.code != ItemCompletionFailureCode.ITEM_MISSING) throw error
                        markDisposition(configuration, pending, ItemCompletionDisposition.ITEM_MISSING, active)
                        continue
                    }
                    val request = pending.request()
                    if (!fresh.matchesReview(request)) {
                        markDisposition(configuration, pending, ItemCompletionDisposition.REVIEW_REQUIRED, active)
                        continue
                    }
                    val submitted = pending.copy(submittedAt = instant())
                    var firstSendEvidence: String? = null
                    persist(configuration, active) { current ->
                        require(!current.hasCompletionMutationBlocker())
                        require(current.currentCompletionProof(pending.itemId)?.snapshot == fresh)
                        current.copy(itemCompletionLedger = replaceExact(current.itemCompletionLedger, pending, submitted)).also {
                            firstSendEvidence = it.fenceCompletionEvidence(current).completionLocalEvidence()
                        }
                    }
                    pending = submitted
                    // A save may suspend while canonical/execution authority changes. The durable
                    // submitted marker keeps exact custody, but this first attempt must not send.
                    require(store.state.value.completionLocalEvidence() == firstSendEvidence &&
                        !store.state.value.hasCompletionMutationBlocker() && !store.state.value.itemCompletionLedger.needsCanonicalCatchUp)
                }
                // Historical replay is independent of the selected view or newer/missing items.
                try {
                    require(active())
                    val result = transport.put(configuration, pending.itemId, pending.requestJson)
                    persist(configuration, active) { current ->
                        current.copy(itemCompletionLedger = current.itemCompletionLedger.settleItemCompletion(pending, result, instant()),
                            itemCompletionGetProofs = emptyMap())
                    }
                } catch (error: ItemCompletionApiException.Definitive) {
                    val disposition = when (error.code) {
                        ItemCompletionFailureCode.ITEM_MISSING -> ItemCompletionDisposition.ITEM_MISSING
                        ItemCompletionFailureCode.INVALID, ItemCompletionFailureCode.PARENT_REQUIRED -> ItemCompletionDisposition.REJECTED
                        else -> ItemCompletionDisposition.REVIEW_REQUIRED
                    }
                    markDisposition(configuration, pending, disposition, active)
                }
            }
        }
    }

    suspend fun discardReviewed(operationId: String): Boolean = operation("Removing reviewed completion intent…") { configuration, active ->
        persist(configuration, active) { current ->
            val pending = requireNotNull(current.itemCompletionLedger.pending.singleOrNull { it.operationId == operationId })
            require(pending.disposition != ItemCompletionDisposition.PENDING)
            current.copy(itemCompletionLedger = current.itemCompletionLedger.copy(
                pending = current.itemCompletionLedger.pending.filterNot { it.operationId == operationId }),
                itemCompletionGetProofs = emptyMap())
        }
    }

    private suspend fun readCurrent(configuration: AuthenticatedApiConfiguration, itemId: String, active: () -> Boolean): ItemCompletionSnapshot {
        val before = store.state.value
        require(!before.hasCompletionMutationBlocker() && !before.itemCompletionLedger.needsCanonicalCatchUp)
        val localEvidence = before.completionLocalEvidence()
        val snapshot = try { transport.get(configuration, itemId) }
        catch (error: ItemCompletionApiException.Definitive) {
            if (error.code == ItemCompletionFailureCode.ITEM_MISSING) markCatchUp(configuration, active)
            throw error
        }
        snapshot.requireValid()
        require(snapshot.itemId == itemId && active())
        val current = store.state.value
        require(current.completionLocalEvidence() == localEvidence && !current.hasCompletionMutationBlocker())
        if (current.completionItem(itemId)?.revision != snapshot.itemRevision) {
            markCatchUp(configuration, active)
            throw CatchUpRequired()
        }
        persist(configuration, active) { latest ->
            require(latest.completionLocalEvidence() == localEvidence && !latest.hasCompletionMutationBlocker())
            require(latest.completionItem(itemId)?.revision == snapshot.itemRevision && !latest.itemCompletionLedger.needsCanonicalCatchUp)
            val ledger = latest.itemCompletionLedger.withCompletionObservation(ItemCompletionObservation(snapshot, instant()))
            require(ledger.observations[itemId]?.snapshot == snapshot)
            latest.copy(itemCompletionLedger = ledger, itemCompletionGetProofs = mapOf(itemId to ItemCompletionReadProof(snapshot, localEvidence)))
        }
        return snapshot
    }

    private suspend fun markCatchUp(configuration: AuthenticatedApiConfiguration, active: () -> Boolean) =
        persist(configuration, active) { current -> current.copy(
            itemCompletionLedger = current.itemCompletionLedger.copy(needsCanonicalCatchUp = true), itemCompletionGetProofs = emptyMap()) }

    private suspend fun markDisposition(configuration: AuthenticatedApiConfiguration, expected: PendingItemCompletionMutation,
        disposition: ItemCompletionDisposition, active: () -> Boolean,
    ) = persist(configuration, active) { current -> current.copy(
        itemCompletionLedger = replaceExact(current.itemCompletionLedger, expected, expected.copy(disposition = disposition))
            .copy(needsCanonicalCatchUp = true), itemCompletionGetProofs = emptyMap()) }

    private fun replaceExact(ledger: ItemCompletionLedger, expected: PendingItemCompletionMutation,
        replacement: PendingItemCompletionMutation,
    ): ItemCompletionLedger {
        val current = requireNotNull(ledger.pending.singleOrNull { it.operationId == expected.operationId })
        require(current.sameCompletionCustody(expected))
        return ledger.copy(pending = ledger.pending.map {
            if (it.operationId == expected.operationId) replacement.copy(wasSensitive = current.wasSensitive) else it
        })
    }

    private suspend fun operation(message: String, isCurrent: () -> Boolean = { true },
        block: suspend (AuthenticatedApiConfiguration, () -> Boolean) -> Unit,
    ): Boolean = mutex.withLock {
        val job = currentCoroutineContext().job
        val admittedGeneration = generation.get()
        fun active() = job.isActive && isCurrent() && generation.get() == admittedGeneration
        fun publish(value: ItemCompletionSyncState): Boolean = synchronized(presentationLock) {
            if (!active()) false else { mutableState.value = value; true }
        }
        if (store.loadState.first { it != PlannerLoadState.LOADING } != PlannerLoadState.READY) {
            publish(ItemCompletionSyncState(message = "Encrypted completion storage is unavailable", failed = true))
            return@withLock false
        }
        val busy = ItemCompletionSyncState(isBusy = true, message = message)
        if (!publish(busy)) return@withLock false
        try {
            val configuration = credentials.authenticatedConfiguration() ?: error("Unconfigured")
            configuration.withBindingOperation {
                require(active())
                persist(configuration, ::active, allowUnbound = true) { current ->
                    if (current.itemCompletionLedger.syncOrigin == null) current.copy(itemCompletionLedger = ItemCompletionLedger(
                        syncOrigin = configuration.baseUrl.toString(), configurationId = requireNotNull(configuration.configurationId))) else current
                }
                block(configuration, ::active)
            }
            publish(ItemCompletionSyncState(message = if (store.state.value.itemCompletionLedger.needsCanonicalCatchUp)
                "Completion receipt retained · canonical catch-up is required" else "Completion evidence saved securely"))
        } catch (error: CancellationException) {
            publish(ItemCompletionSyncState(message = "Completion refresh paused"))
            throw error
        } catch (_: Exception) {
            publish(ItemCompletionSyncState(message = "Completion unavailable · exact intent is retained; refresh or retry", failed = true))
            false
        } finally {
            synchronized(presentationLock) {
                if (generation.get() == admittedGeneration && mutableState.value === busy) {
                    mutableState.value = ItemCompletionSyncState(message = "Completion refresh paused")
                }
            }
        }
    }

    private suspend fun persist(configuration: AuthenticatedApiConfiguration, active: () -> Boolean,
        allowUnbound: Boolean = false, update: (DayWeaveUiState) -> DayWeaveUiState,
    ) {
        require(active())
        val receipt = store.mutateItemCompletion { current ->
            require(active())
            val actual = credentials.snapshot()
            require(actual.hasBearerToken && actual.baseUrl == configuration.baseUrl.toString() && actual.configurationId == configuration.configurationId)
            require(current.canonicalSyncOrigin == actual.baseUrl && current.canonicalConfigurationId == actual.configurationId)
            require(allowUnbound && current.itemCompletionLedger.syncOrigin == null ||
                current.itemCompletionLedger.syncOrigin == actual.baseUrl && current.itemCompletionLedger.configurationId == actual.configurationId)
            update(current)
        }
        check(receipt != null && receipt.awaitDurable())
        require(active())
    }

    private fun ItemCompletionSnapshot.matchesReview(request: ItemCompletionRequest): Boolean =
        itemRevision == request.expectedItemRevision && state.revision == request.expectedCompletionRevision && evidenceHash == request.expectedEvidenceHash
    private fun instant(): String = now().truncatedTo(ChronoUnit.MICROS).toString()
    private class CatchUpRequired : IllegalStateException()
}
