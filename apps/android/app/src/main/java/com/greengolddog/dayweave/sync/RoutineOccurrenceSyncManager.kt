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

/** Exact planner identity, never a date or a template-derived replacement identity. */
data class RoutineOccurrenceSelection(val seriesItemId: String, val occurrenceId: String) {
    val key: String get() = "$seriesItemId/$occurrenceId"
    companion object {
        fun fromKey(key: String): RoutineOccurrenceSelection {
            val parts = key.split('/')
            require(parts.size == 2)
            return RoutineOccurrenceSelection(parts[0], parts[1])
        }
    }
}

data class RoutineOccurrenceSyncState(
    val isBusy: Boolean = false,
    val message: String = "Occurrence history is private",
    val failed: Boolean = false,
    val selection: RoutineOccurrenceSelection? = null,
    val reviewed: RoutineOccurrenceSnapshot? = null,
)

/** Minted by a newly composed, new-key authenticated publication, including current-head dedupe. */
class RoutineOccurrenceRemoteScheduleWitness internal constructor(
    val authorityGeneration: Long, val publicationId: String, val operationId: String,
    val syncOrigin: String, val configurationId: String,
)

/** Live GET permission stays in memory. Durable requests keep their original typed and wire custody. */
class RoutineOccurrenceSyncManager(
    private val store: PlannerStore,
    private val credentials: ApiCredentialStore,
    private val transport: RoutineOccurrenceTransport,
    private val now: () -> Instant = Instant::now,
    private val uuid: () -> UUID = UUID::randomUUID,
    private val protectedWorkAllowed: () -> Boolean = { true },
) {
    private val mutex = Mutex()
    private val presentationLock = Any()
    private val generation = AtomicLong()
    private val selectionGeneration = AtomicLong()
    private var lease: Lease? = null
    private val mutableState = MutableStateFlow(RoutineOccurrenceSyncState())
    val state = mutableState.asStateFlow()

    private data class Lease(val selection: RoutineOccurrenceSelection, val snapshot: RoutineOccurrenceSnapshot,
        val localEvidence: String, val selectionGeneration: Long, val operationGeneration: Long,
        val isCurrent: () -> Boolean)

    internal fun quarantineBindingState() = synchronized(presentationLock) {
        generation.incrementAndGet()
        selectionGeneration.incrementAndGet()
        lease = null
        store.invalidateRoutineOccurrenceAuthority()
        mutableState.value = RoutineOccurrenceSyncState()
    }

    fun select(selection: RoutineOccurrenceSelection?): Long = synchronized(presentationLock) {
        val selectedGeneration = selectionGeneration.incrementAndGet()
        lease = null
        store.invalidateRoutineOccurrenceAuthority()
        mutableState.value = RoutineOccurrenceSyncState(selection = selection)
        selectedGeneration
    }

    fun clearSelection(expectedGeneration: Long) = synchronized(presentationLock) {
        if (selectionGeneration.get() == expectedGeneration) select(null)
    }

    suspend fun load(selection: RoutineOccurrenceSelection, isCurrent: () -> Boolean = { true }): Boolean {
        val selectedGeneration = selectionGeneration.get()
        return operation("Loading private occurrence…", isCurrent) { configuration, active ->
            val snapshot = read(configuration, selection, null, active)
            require(selectionGeneration.get() == selectedGeneration && active())
            synchronized(presentationLock) {
                require(selectionGeneration.get() == selectedGeneration && active())
                lease = Lease(selection, snapshot, store.state.value.completionLocalEvidence(), selectedGeneration,
                    store.state.value.routineOccurrenceAuthorityGeneration, isCurrent)
                mutableState.value = mutableState.value.copy(selection = selection, reviewed = snapshot)
            }
        }
    }

    suspend fun stage(selection: RoutineOccurrenceSelection, reviewed: RoutineOccurrenceSnapshot,
        memberId: String, action: RoutineOccurrenceAction,
    ): Boolean = operation("Saving exact occurrence intent securely…", revokeLease = false) { configuration, active ->
        val admitted = synchronized(presentationLock) { requireNotNull(lease) }
        fun reviewedCurrent(current: DayWeaveUiState): Boolean = admitted.selection == selection &&
            admitted.snapshot == reviewed && admitted.selectionGeneration == selectionGeneration.get() &&
            admitted.operationGeneration == current.routineOccurrenceAuthorityGeneration && admitted.isCurrent() &&
            admitted.localEvidence == current.completionLocalEvidence() && reviewed.freshEditEligible &&
            !current.hasCompletionMutationBlocker()
        require(reviewedCurrent(store.state.value))
        val member = reviewed.aggregate.members.single { it.itemId == memberId }
        require(!reviewed.members.single { it.itemId == memberId }.occurrenceEvidenceRequired)
        val isParent = reviewed.aggregate.manifest.members.any { it.parentId == memberId }
        when (action) {
            is RoutineOccurrenceAction.SetOutcome -> require(!isParent)
            is RoutineOccurrenceAction.Reopen -> require(!isParent && action.open == member.open)
            is RoutineOccurrenceAction.SetPolicy -> require(isParent || action.mode == ItemCompletionMode.AUTOMATIC)
        }
        val request = RoutineOccurrenceRequest(operationId = uuid().toString(),
            expectedInstanceRevision = reviewed.aggregate.revision, expectedMemberRevision = member.revision,
            expectedEvidenceHash = reviewed.evidenceHash, action = action).also { it.requireValid(memberId) }
        val pending = PendingRoutineOccurrenceMutation(operationId = request.operationId,
            instanceId = reviewed.aggregate.manifest.id, memberId = memberId,
            syncOrigin = configuration.baseUrl.toString(), configurationId = requireNotNull(configuration.configurationId),
            requestJson = ITEM_PROGRESS_JSON.encodeToString(request), request = request, createdAt = instant())
        persist(configuration, active) { current ->
            require(reviewedCurrent(current))
            current.routineOccurrenceLedger.enqueueRoutineOccurrence(pending)
        }
        synchronized(presentationLock) { lease = null }
    }

    suspend fun replay(isCurrent: () -> Boolean = { true }): Boolean =
        operation("Synchronizing exact occurrence changes…", isCurrent) { configuration, active ->
            for (saved in store.state.value.routineOccurrenceLedger.pending.filter {
                it.disposition == RoutineOccurrenceDisposition.PENDING }) {
                var pending = saved
                if (pending.submittedAt == null) {
                    val before = store.state.value
                    require(!before.hasCompletionMutationBlocker())
                    val manifest = before.routineOccurrenceLedger.observations.getValue(pending.instanceId).snapshot.aggregate.manifest
                    val fresh = read(configuration, RoutineOccurrenceSelection(manifest.seriesItemId, manifest.occurrenceId), pending.instanceId, active)
                    if (!fresh.freshEditEligible || fresh.aggregate.revision != pending.request.expectedInstanceRevision ||
                        fresh.aggregate.members.single { it.itemId == pending.memberId }.revision != pending.request.expectedMemberRevision ||
                        fresh.evidenceHash != pending.request.expectedEvidenceHash) {
                        persist(configuration, active) { it.routineOccurrenceLedger.resolveRoutineOccurrence(pending, RoutineOccurrenceDisposition.REVIEW_REQUIRED) }
                        continue
                    }
                    val evidence = store.state.value.completionLocalEvidence()
                    val authorityGeneration = store.state.value.routineOccurrenceAuthorityGeneration
                    persist(configuration, active) { current ->
                        require(current.completionLocalEvidence() == evidence && !current.hasCompletionMutationBlocker() &&
                            current.routineOccurrenceAuthorityGeneration == authorityGeneration)
                        current.routineOccurrenceLedger.markRoutineOccurrenceSubmitted(pending, instant())
                    }
                    pending = store.state.value.routineOccurrenceLedger.pending.single { it.operationId == pending.operationId }
                    require(store.state.value.completionLocalEvidence() == evidence && active() &&
                        store.state.value.routineOccurrenceAuthorityGeneration == Math.addExact(authorityGeneration, 1))
                }
                // Even a missing template or ineligible current GET cannot revoke an exact retry.
                try {
                    require(active())
                    val result = transport.put(configuration, pending.instanceId, pending.memberId, pending.requestJson)
                    persist(configuration, active) { it.routineOccurrenceLedger.settleRoutineOccurrence(pending, result, instant()) }
                } catch (error: RoutineOccurrenceApiException.Definitive) {
                    if (error.code == RoutineOccurrenceFailureCode.INVALID_CURSOR) throw error
                    val disposition = when (error.code) {
                        RoutineOccurrenceFailureCode.OCCURRENCE_MISSING, RoutineOccurrenceFailureCode.MEMBER_MISSING -> RoutineOccurrenceDisposition.INSTANCE_MISSING
                        RoutineOccurrenceFailureCode.INVALID, RoutineOccurrenceFailureCode.TOO_LARGE,
                        RoutineOccurrenceFailureCode.LEAF_REQUIRED, RoutineOccurrenceFailureCode.PARENT_REQUIRED -> RoutineOccurrenceDisposition.REJECTED
                        else -> RoutineOccurrenceDisposition.REVIEW_REQUIRED
                    }
                    persist(configuration, active) { it.routineOccurrenceLedger.resolveRoutineOccurrence(pending, disposition) }
                }
            }
        }

    /** Read a complete bounded chain even while an exact command is unresolved. */
    suspend fun refresh(isCurrent: () -> Boolean = { true }, cold: Boolean = false): Boolean {
        val refreshed = operation("Refreshing private occurrence history…", isCurrent) { configuration, active ->
            val expected = store.state.value.routineOccurrenceLedger
            val currentList = cold || expected.deltaCursor == null
            val pages = ArrayList<RoutineOccurrencePage>()
            val cursors = HashSet<String>()
            var cursor = if (currentList) null else expected.deltaCursor
            var bytes = 0L
            var members = 0L
            do {
                require(active() && pages.size < MAX_ROUTINE_OCCURRENCE_TERMINAL_PAGES)
                val page = if (currentList) transport.list(configuration, cursor) else transport.delta(configuration, cursor)
                require(active() && cursors.add(page.cursor))
                // Each page is already byte-bounded by transport; cap the retained whole chain too.
                bytes += routineStateEncodedBytes(RoutineOccurrencePage.serializer(), page,
                    MAX_ROUTINE_OCCURRENCE_TERMINAL_BYTES - bytes.toInt())
                members += page.changes.sumOf { it.occurrence.aggregate.members.size.toLong() }
                require(bytes <= MAX_ROUTINE_OCCURRENCE_TERMINAL_BYTES && members <= MAX_ROUTINE_OCCURRENCE_TERMINAL_MEMBER_VISITS)
                pages.add(page)
                cursor = page.cursor
            } while (page.hasMore)
            persist(configuration, active) { it.routineOccurrenceLedger.installRoutineOccurrenceTerminal(expected, pages, instant(),
                isCurrentList = currentList, startingCursor = if (currentList) null else expected.deltaCursor) }
        }
        // Invalid/stale cursors, bounded delta overflow and missing receipt coverage recover through
        // a complete current list. Neither failed attempt advances or erases the old checkpoint.
        return if (!refreshed && !cold && isCurrent() && protectedWorkAllowed()) refresh(isCurrent, cold = true) else refreshed
    }

    suspend fun discard(operationId: String): Boolean = operation("Removing resolved occurrence intent…") { configuration, active ->
        persist(configuration, active) { current ->
            val pending = current.routineOccurrenceLedger.pending.single { it.operationId == operationId }
            current.routineOccurrenceLedger.discardRoutineOccurrence(pending)
        }
    }

    /** Only the caller's fresh authenticated compose can acknowledge the exact terminal capture. */
    suspend fun catchUpSchedule(isCurrent: () -> Boolean = { true },
        composeFresh: suspend () -> RoutineOccurrenceRemoteScheduleWitness?,
    ): Boolean =
        operation("Refreshing the occurrence-aware schedule…", isCurrent) { configuration, active ->
            val expected = store.state.value.routineOccurrenceLedger
            if (expected.needsRemoteScheduleCatchUp) {
                require(expected.pending.isEmpty() && expected.minimumCatchUpRevisions.isEmpty() && expected.deltaCursor != null)
                require(store.state.value.pendingSchedulePublication == null)
                val authorityGeneration = store.state.value.routineOccurrenceAuthorityGeneration
                val witness = requireNotNull(composeFresh())
                require(active() && store.state.value.pendingSchedulePublication == null &&
                    store.state.value.routineOccurrenceAuthorityGeneration == authorityGeneration &&
                    witness.authorityGeneration == authorityGeneration &&
                    witness.publicationId == store.state.value.publishedScheduleProof?.revision?.id &&
                    witness.syncOrigin == configuration.baseUrl.toString() && witness.configurationId == configuration.configurationId)
                persist(configuration, active) { it.routineOccurrenceLedger.acknowledgeRoutineOccurrenceRemoteScheduleCatchUp(expected) }
            }
        }

    private suspend fun read(configuration: AuthenticatedApiConfiguration, selection: RoutineOccurrenceSelection,
        instanceId: String?, active: () -> Boolean,
    ): RoutineOccurrenceSnapshot {
        val evidence = store.state.value.completionLocalEvidence()
        val authorityGeneration = store.state.value.routineOccurrenceAuthorityGeneration
        val snapshot = if (instanceId == null) transport.lookup(configuration, selection.seriesItemId, selection.occurrenceId)
            else transport.get(configuration, instanceId)
        snapshot.requireValid()
        require(snapshot.aggregate.manifest.seriesItemId == selection.seriesItemId && snapshot.aggregate.manifest.occurrenceId == selection.occurrenceId)
        require(instanceId == null || snapshot.aggregate.manifest.id == instanceId)
        persist(configuration, active) { current ->
            require(current.completionLocalEvidence() == evidence && current.routineOccurrenceAuthorityGeneration == authorityGeneration)
            val previous = current.routineOccurrenceLedger.observations[snapshot.aggregate.manifest.id]?.snapshot
            current.routineOccurrenceLedger.observeRoutineOccurrence(RoutineOccurrenceObservation(snapshot, instant())).let { observed ->
                // A GET can disclose policy/source drift without a canonical delta or changed status.
                // Current review is not a witness that the existing publication used this head.
                if (previous != snapshot) observed.copy(needsRemoteScheduleCatchUp = true) else observed
            }
        }
        return snapshot
    }

    private suspend fun operation(message: String, isCurrent: () -> Boolean = { true }, revokeLease: Boolean = true,
        block: suspend (AuthenticatedApiConfiguration, () -> Boolean) -> Unit,
    ): Boolean = mutex.withLock {
        val job = currentCoroutineContext().job
        val admittedGeneration = generation.get()
        fun active() = job.isActive && isCurrent() && protectedWorkAllowed() && generation.get() == admittedGeneration
        fun publish(value: RoutineOccurrenceSyncState): Boolean = synchronized(presentationLock) {
            if (!active()) false else { mutableState.value = value; true }
        }
        if (store.loadState.first { it != PlannerLoadState.LOADING } != PlannerLoadState.READY || !active()) return@withLock false
        if (revokeLease) synchronized(presentationLock) { lease = null; store.invalidateRoutineOccurrenceAuthority() }
        publish(mutableState.value.copy(isBusy = true, message = message, failed = false, reviewed = if (revokeLease) null else mutableState.value.reviewed))
        try {
            val configuration = credentials.authenticatedConfiguration() ?: error("Unconfigured")
            configuration.withBindingOperation {
                persist(configuration, ::active, allowUnbound = true) { it.routineOccurrenceLedger.bindRoutineOccurrences(
                    configuration.baseUrl.toString(), requireNotNull(configuration.configurationId)) }
                block(configuration, ::active)
            }
            publish(mutableState.value.copy(isBusy = false, message = "Private occurrence history refreshed", failed = false))
        } catch (error: CancellationException) {
            publish(mutableState.value.copy(isBusy = false, reviewed = null, message = "Occurrence refresh paused"))
            throw error
        } catch (_: Exception) {
            publish(mutableState.value.copy(isBusy = false, reviewed = null,
                message = "Occurrence unavailable · exact intent is retained; refresh or retry", failed = true))
            false
        }
    }

    private suspend fun persist(configuration: AuthenticatedApiConfiguration, active: () -> Boolean,
        allowUnbound: Boolean = false, update: (DayWeaveUiState) -> RoutineOccurrenceLedger,
    ) {
        require(active())
        val receipt = store.mutateRoutineOccurrences { current ->
            require(active())
            val actual = credentials.snapshot()
            require(actual.hasBearerToken && actual.baseUrl == configuration.baseUrl.toString() && actual.configurationId == configuration.configurationId)
            require(current.canonicalSyncOrigin == actual.baseUrl && current.canonicalConfigurationId == actual.configurationId)
            require(allowUnbound && current.routineOccurrenceLedger.syncOrigin == null ||
                current.routineOccurrenceLedger.syncOrigin == actual.baseUrl && current.routineOccurrenceLedger.configurationId == actual.configurationId)
            update(current)
        }
        check(receipt != null && receipt.awaitDurable())
        require(active())
    }
    private fun instant(): String = now().truncatedTo(ChronoUnit.MICROS).toString()
}
