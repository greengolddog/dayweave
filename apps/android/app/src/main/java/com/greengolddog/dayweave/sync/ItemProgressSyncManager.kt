package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ItemProgressApiException
import com.greengolddog.dayweave.network.ItemProgressFailureCode
import com.greengolddog.dayweave.network.ItemProgressTransport
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerPersistenceReceipt
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.time.temporal.ChronoUnit
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

data class ItemProgressSyncState(val isBusy: Boolean = false, val message: String = "Saved independent progress",
    val failed: Boolean = false, val canonicalCatchUpItemIds: Set<String> = emptySet()) {
    val requiresCanonicalCatchUp: Boolean get() = canonicalCatchUpItemIds.isNotEmpty()
}

/** Selected GET is cancellable; durable bound outbox replay does not depend on a selected view. */
class ItemProgressSyncManager(
    private val store: PlannerStore,
    private val credentials: ApiCredentialStore,
    private val transport: ItemProgressTransport,
    private val now: () -> Instant = Instant::now,
    private val uuid: () -> UUID = UUID::randomUUID,
) {
    private val mutex = Mutex()
    private val mutableState = MutableStateFlow(ItemProgressSyncState())
    val state = mutableState.asStateFlow()
    private val catchUpItems = ConcurrentHashMap.newKeySet<String>()
    private val presentationGeneration = AtomicLong()
    private val presentationLock = Any()

    internal fun quarantineBindingState() = synchronized(presentationLock) {
        presentationGeneration.incrementAndGet()
        catchUpItems.clear()
        mutableState.value = ItemProgressSyncState()
    }

    suspend fun load(itemId: String, isCurrent: () -> Boolean = { true }): Boolean = operation("Loading independent progress…", isCurrent) { configuration ->
        val admittedGeneration = presentationGeneration.get()
        require(store.state.value.progressItem(itemId) != null)
        val snapshot = try { transport.get(configuration, itemId) }
        catch (error: ItemProgressApiException.Definitive) {
            if (error.code == ItemProgressFailureCode.ITEM_MISSING) throw CanonicalCatchUpRequired(itemId)
            throw error
        }
        snapshot.requireValid()
        require(snapshot.itemId == itemId)
        persist(isCurrent) { current ->
            checkBinding(current, configuration)
            val item = requireNotNull(current.progressItem(itemId))
            if (item.revision != snapshot.itemRevision) throw CanonicalCatchUpRequired(itemId)
            current.itemProgressLedger.withObservation(ItemProgressObservation(snapshot, instant(), isGetProof = true))
        }
        if (currentCoroutineContext().job.isActive && isCurrent() && presentationGeneration.get() == admittedGeneration) {
            catchUpItems.remove(itemId)
        }
    }

    suspend fun stage(itemId: String, expectedItemRevision: Long, expectedProgressRevision: Long,
        components: List<ItemProgressComponent>, replacingOperationId: String? = null,
    ): Boolean = operation("Saving independent progress securely…") { configuration ->
        if (itemId in catchUpItems) throw CanonicalCatchUpRequired(itemId)
        val request = ItemProgressRequest(operationId = uuid().toString(), expectedItemRevision = expectedItemRevision,
            expectedProgressRevision = expectedProgressRevision, components = components).also(ItemProgressRequest::requireValid)
        val mutation = PendingItemProgressMutation(operationId = request.operationId, itemId = itemId,
            syncOrigin = configuration.baseUrl.toString(), configurationId = requireNotNull(configuration.configurationId),
            expectedItemRevision = expectedItemRevision, expectedProgressRevision = expectedProgressRevision,
            requestJson = ITEM_PROGRESS_JSON.encodeToString(request), createdAt = instant(),
            wasSensitive = store.state.value.progressReviewSensitive(itemId))
        persist { current -> checkBinding(current, configuration); current.stageItemProgress(mutation, replacingOperationId) }
    }

    suspend fun replay(isCurrent: () -> Boolean = { true }): Boolean = if (store.state.value.itemProgressLedger.pending.none { it.disposition == ItemProgressDisposition.PENDING }) {
        true
    } else operation("Synchronizing saved progress changes…", isCurrent) { configuration ->
        for (saved in store.state.value.itemProgressLedger.pending.filter { it.disposition == ItemProgressDisposition.PENDING }) {
            var pending = saved
            if (pending.submittedAt == null) {
                if (pending.itemId in catchUpItems || !store.state.value.canFirstSendItemProgress(pending)) {
                    markDisposition(configuration, pending, ItemProgressDisposition.REVIEW_REQUIRED, isCurrent)
                    continue
                }
                val submitted = pending.copy(submittedAt = instant())
                persist(isCurrent) { current ->
                    checkBinding(current, configuration)
                    require(pending.itemId !in catchUpItems)
                    require(current.canFirstSendItemProgress(pending))
                    replaceExact(current.itemProgressLedger, pending, submitted)
                }
                pending = submitted
            }
            // Once submitted, current item absence/revision changes cannot suppress exact replay.
            try {
                pending.requireValid()
                require(isCurrent())
                val result = transport.put(configuration, pending.itemId, pending.requestJson)
                persist(isCurrent) { current ->
                    checkBinding(current, configuration)
                    current.itemProgressLedger.settleItemProgress(pending, result, instant())
                }
            } catch (error: ItemProgressApiException.Definitive) {
                val disposition = when (error.code) {
                    ItemProgressFailureCode.ITEM_MISSING -> ItemProgressDisposition.ITEM_MISSING
                    ItemProgressFailureCode.INVALID -> ItemProgressDisposition.REJECTED
                    else -> ItemProgressDisposition.REVIEW_REQUIRED
                }
                markDisposition(configuration, pending, disposition, isCurrent)
            }
        }
    }

    suspend fun discardReviewed(operationId: String): Boolean = operation("Removing reviewed progress draft…") { configuration ->
        persist { current ->
            checkBinding(current, configuration)
            val pending = requireNotNull(current.itemProgressLedger.pending.singleOrNull { it.operationId == operationId })
            require(pending.disposition != ItemProgressDisposition.PENDING)
            current.itemProgressLedger.copy(pending = current.itemProgressLedger.pending.filterNot { it.operationId == operationId })
        }
    }

    private suspend fun markDisposition(configuration: AuthenticatedApiConfiguration, expected: PendingItemProgressMutation,
        disposition: ItemProgressDisposition,
        isCurrent: () -> Boolean = { true },
    ) = persist(isCurrent) { current ->
        checkBinding(current, configuration)
        replaceExact(current.itemProgressLedger, expected, expected.copy(disposition = disposition))
    }

    private fun replaceExact(ledger: ItemProgressLedger, expected: PendingItemProgressMutation,
        replacement: PendingItemProgressMutation,
    ): ItemProgressLedger {
        val exact = requireNotNull(ledger.pending.singleOrNull { it.operationId == expected.operationId })
        require(exact.sameProgressCustody(expected))
        return ledger.copy(pending = ledger.pending.map { if (it.operationId == expected.operationId) replacement.copy(wasSensitive = exact.wasSensitive) else it })
    }

    private suspend fun operation(message: String, isCurrent: () -> Boolean = { true },
        block: suspend (AuthenticatedApiConfiguration) -> Unit,
    ): Boolean = mutex.withLock {
        val operationJob = currentCoroutineContext().job
        val admittedGeneration = presentationGeneration.get()
        fun active() = operationJob.isActive && isCurrent() && presentationGeneration.get() == admittedGeneration
        fun publishActive(update: () -> ItemProgressSyncState): Boolean = synchronized(presentationLock) {
            if (!active()) false else {
                mutableState.value = update()
                true
            }
        }
        if (store.loadState.first { it != PlannerLoadState.LOADING } != PlannerLoadState.READY) {
            publishActive { progressState(message = "Encrypted progress storage is unavailable", failed = true) }
            return@withLock false
        }
        val ownedBusyState = progressState(isBusy = true, message = message)
        if (!publishActive { ownedBusyState }) return@withLock false
        try {
            require(active())
            val configuration = credentials.authenticatedConfiguration() ?: error("Unconfigured")
            configuration.withBindingOperation {
                persist(isCurrent) { current ->
                    require(current.canonicalSyncOrigin == configuration.baseUrl.toString() && current.canonicalConfigurationId == configuration.configurationId)
                    val ledger = current.itemProgressLedger
                    if (ledger.syncOrigin == null) ItemProgressLedger(syncOrigin = configuration.baseUrl.toString(),
                        configurationId = requireNotNull(configuration.configurationId))
                    else { checkBinding(current, configuration); ledger }
                }
                block(configuration)
            }
            publishActive { progressState(message = "Saved progress · refresh the selected item for a current observation") }
        } catch (error: CancellationException) {
            publishActive { progressState(message = "Progress refresh paused") }
            throw error
        } catch (error: CanonicalCatchUpRequired) {
            publishActive {
                catchUpItems.add(error.itemId)
                progressState(message = "The item changed · canonical catch-up is required before editing", failed = true)
            }
            false
        } catch (_: Exception) {
            publishActive { progressState(message = "Progress unavailable · saved values and exact changes are retained", failed = true) }
            false
        } finally {
            // Still under the operation mutex: even cancellation-insensitive I/O has now drained.
            // Releasing this operation's presentation ownership is not a successful read or replay.
            synchronized(presentationLock) {
                if (presentationGeneration.get() == admittedGeneration && mutableState.value === ownedBusyState) {
                    mutableState.value = progressState(message = "Progress refresh paused")
                }
            }
        }
    }

    private fun checkBinding(current: DayWeaveUiState, configuration: AuthenticatedApiConfiguration) {
        val actual = credentials.snapshot()
        require(actual.hasBearerToken && actual.baseUrl == configuration.baseUrl.toString() && actual.configurationId == configuration.configurationId)
        require(current.canonicalSyncOrigin == configuration.baseUrl.toString() && current.canonicalConfigurationId == configuration.configurationId)
        require(current.itemProgressLedger.syncOrigin == current.canonicalSyncOrigin && current.itemProgressLedger.configurationId == current.canonicalConfigurationId)
    }

    private suspend fun persist(isCurrent: () -> Boolean = { true }, update: (DayWeaveUiState) -> ItemProgressLedger) {
        val job = currentCoroutineContext().job
        require(job.isActive && isCurrent())
        await(store.mutateItemProgress { current ->
            require(job.isActive && isCurrent())
            update(current)
        })
    }
    private suspend fun await(receipt: PlannerPersistenceReceipt?) { check(receipt != null && receipt.awaitDurable()) }
    private fun instant(): String = now().truncatedTo(ChronoUnit.MICROS).toString()
    private fun progressState(isBusy: Boolean = false, message: String, failed: Boolean = false) =
        ItemProgressSyncState(isBusy, message, failed, catchUpItems.toSet())
    private class CanonicalCatchUpRequired(val itemId: String) : IllegalStateException()
}
