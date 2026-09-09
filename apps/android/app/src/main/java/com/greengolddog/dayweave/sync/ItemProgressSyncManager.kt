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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

data class ItemProgressSyncState(val isBusy: Boolean = false, val message: String = "Saved independent progress", val failed: Boolean = false)

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

    internal fun quarantineBindingState() { mutableState.value = ItemProgressSyncState() }

    suspend fun load(itemId: String): Boolean = operation("Loading independent progress…") { configuration ->
        require(store.state.value.progressItem(itemId) != null)
        val snapshot = transport.get(configuration, itemId)
        snapshot.requireValid()
        require(snapshot.itemId == itemId)
        persist { current ->
            checkBinding(current, configuration)
            require(current.progressItem(itemId) != null)
            current.itemProgressLedger.withObservation(ItemProgressObservation(snapshot, instant(), isGetProof = true))
        }
    }

    suspend fun stage(itemId: String, expectedItemRevision: Long, expectedProgressRevision: Long,
        components: List<ItemProgressComponent>, replacingOperationId: String? = null,
    ): Boolean = operation("Saving independent progress securely…") { configuration ->
        val request = ItemProgressRequest(operationId = uuid().toString(), expectedItemRevision = expectedItemRevision,
            expectedProgressRevision = expectedProgressRevision, components = components).also(ItemProgressRequest::requireValid)
        val mutation = PendingItemProgressMutation(operationId = request.operationId, itemId = itemId,
            syncOrigin = configuration.baseUrl.toString(), configurationId = requireNotNull(configuration.configurationId),
            expectedItemRevision = expectedItemRevision, expectedProgressRevision = expectedProgressRevision,
            requestJson = ITEM_PROGRESS_JSON.encodeToString(request), createdAt = instant(),
            wasSensitive = store.state.value.progressReviewSensitive(itemId))
        persist { current -> checkBinding(current, configuration); current.stageItemProgress(mutation, replacingOperationId) }
    }

    suspend fun replay(): Boolean = if (store.state.value.itemProgressLedger.pending.none { it.disposition == ItemProgressDisposition.PENDING }) {
        true
    } else operation("Synchronizing saved progress changes…") { configuration ->
        for (saved in store.state.value.itemProgressLedger.pending.filter { it.disposition == ItemProgressDisposition.PENDING }) {
            var pending = saved
            if (pending.submittedAt == null) {
                if (!store.state.value.canFirstSendItemProgress(pending)) {
                    markDisposition(configuration, pending, ItemProgressDisposition.REVIEW_REQUIRED)
                    continue
                }
                val submitted = pending.copy(submittedAt = instant())
                persist { current ->
                    checkBinding(current, configuration)
                    require(current.canFirstSendItemProgress(pending))
                    replaceExact(current.itemProgressLedger, pending, submitted)
                }
                pending = submitted
            }
            // Once submitted, current item absence/revision changes cannot suppress exact replay.
            try {
                pending.requireValid()
                val result = transport.put(configuration, pending.itemId, pending.requestJson)
                persist { current ->
                    checkBinding(current, configuration)
                    current.itemProgressLedger.settleItemProgress(pending, result, instant())
                }
            } catch (error: ItemProgressApiException.Definitive) {
                val disposition = when (error.code) {
                    ItemProgressFailureCode.ITEM_MISSING -> ItemProgressDisposition.ITEM_MISSING
                    ItemProgressFailureCode.INVALID -> ItemProgressDisposition.REJECTED
                    else -> ItemProgressDisposition.REVIEW_REQUIRED
                }
                markDisposition(configuration, pending, disposition)
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
    ) = persist { current ->
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

    private suspend fun operation(message: String, block: suspend (AuthenticatedApiConfiguration) -> Unit): Boolean = mutex.withLock {
        if (store.loadState.first { it != PlannerLoadState.LOADING } != PlannerLoadState.READY) {
            mutableState.value = ItemProgressSyncState(message = "Encrypted progress storage is unavailable", failed = true)
            return@withLock false
        }
        mutableState.value = ItemProgressSyncState(isBusy = true, message = message)
        try {
            val configuration = credentials.authenticatedConfiguration() ?: error("Unconfigured")
            configuration.withBindingOperation {
                persist { current ->
                    require(current.canonicalSyncOrigin == configuration.baseUrl.toString() && current.canonicalConfigurationId == configuration.configurationId)
                    val ledger = current.itemProgressLedger
                    if (ledger.syncOrigin == null) ItemProgressLedger(syncOrigin = configuration.baseUrl.toString(),
                        configurationId = requireNotNull(configuration.configurationId))
                    else { checkBinding(current, configuration); ledger }
                }
                block(configuration)
            }
            mutableState.value = ItemProgressSyncState(message = "Saved progress · refresh the selected item for a current observation")
            true
        } catch (error: CancellationException) {
            mutableState.value = ItemProgressSyncState(message = "Progress refresh paused")
            throw error
        } catch (_: Exception) {
            mutableState.value = ItemProgressSyncState(message = "Progress unavailable · saved values and exact changes are retained", failed = true)
            false
        }
    }

    private fun checkBinding(current: DayWeaveUiState, configuration: AuthenticatedApiConfiguration) {
        val actual = credentials.snapshot()
        require(actual.hasBearerToken && actual.baseUrl == configuration.baseUrl.toString() && actual.configurationId == configuration.configurationId)
        require(current.canonicalSyncOrigin == configuration.baseUrl.toString() && current.canonicalConfigurationId == configuration.configurationId)
        require(current.itemProgressLedger.syncOrigin == current.canonicalSyncOrigin && current.itemProgressLedger.configurationId == current.canonicalConfigurationId)
    }

    private suspend fun persist(update: (DayWeaveUiState) -> ItemProgressLedger) = await(store.mutateItemProgress(update))
    private suspend fun await(receipt: PlannerPersistenceReceipt?) { check(receipt != null && receipt.awaitDurable()) }
    private fun instant(): String = now().truncatedTo(ChronoUnit.MICROS).toString()
}
