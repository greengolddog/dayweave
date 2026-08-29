package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalItemReplacement
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.PlannerApiException
import com.greengolddog.dayweave.network.PreviousScheduleAssignmentRequest
import com.greengolddog.dayweave.network.PreviousScheduleBlockRequest
import com.greengolddog.dayweave.network.ReplaceCanonicalItemRequest
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.network.RemoteItemDeltaChange
import com.greengolddog.dayweave.network.RemoteScheduleBlock
import com.greengolddog.dayweave.network.RemoteSchedulePreview
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.normalizedHttpsApiBaseUrl
import com.greengolddog.dayweave.network.validateBearerToken
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.DateTimeException
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.util.UUID
import kotlin.math.ceil
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.put

enum class CanonicalSyncPhase {
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    READY,
    SYNCING,
    CONNECTED,
    OFFLINE,
    ERROR,
}

data class CanonicalSyncState(
    val phase: CanonicalSyncPhase,
    val message: String,
    val lastInputDigest: String? = null,
    val sourceItemCount: Int = 0,
    val scheduledBlockCount: Int = 0,
) {
    val isBusy: Boolean get() = phase == CanonicalSyncPhase.SYNCING
}

enum class CanonicalRefreshOutcome {
    SUCCESS,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    CONFIGURATION_ERROR,
    TRANSIENT_NETWORK_FAILURE,
    RETRYABLE_SERVER_FAILURE,
    PERMANENT_SERVER_FAILURE,
    STALE_REVISION,
    INVALID_LOCAL_STATE,
    PROTOCOL_FAILURE,
    LOCAL_STORAGE_FAILURE,
    UNEXPECTED_FAILURE,
}

class CanonicalConfigurationChangeBlockedException : IllegalStateException(
    "A canonical item action must be reconciled before changing API credentials",
)

class CanonicalAbandonmentPersistenceException : IllegalStateException(
    "Canonical cache quarantine was not durable; existing credentials were kept",
)

/** Pulls canonical deltas, composes one local day server-side, then commits both atomically. */
class CanonicalSyncManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: CanonicalPlannerTransport,
    private val now: () -> Instant = Instant::now,
    private val zoneId: () -> ZoneId = ZoneId::systemDefault,
    private val dayStartMinute: Int = DEFAULT_DAY_START_MINUTE,
    private val dayEndMinute: Int = DEFAULT_DAY_END_MINUTE,
) {
    private val operationMutex = Mutex()
    private val focusTransitionMutex = Mutex()
    private val mutableState = MutableStateFlow(stateFrom(credentialStore.snapshot()))
    private val mutationJson = Json {
        encodeDefaults = true
        explicitNulls = false
        ignoreUnknownKeys = false
    }
    val state: StateFlow<CanonicalSyncState> = mutableState.asStateFlow()

    init {
        require(dayStartMinute in 0 until MINUTES_PER_DAY)
        require(dayEndMinute in 1..MINUTES_PER_DAY && dayEndMinute > dayStartMinute)
    }

    /** Serializes credential replacement/forget with every canonical request and reconciliation. */
    suspend fun <T> withConfigurationLock(
        change: suspend () -> T,
    ): T =
        operationMutex.withLock {
            if (plannerStore.hasCredentialReplacementBlocker()) {
                updateError(
                    "Reconnect the pending canonical action before changing the API connection.",
                )
                throw CanonicalConfigurationChangeBlockedException()
            }
            change()
        }

    /** Cancels background users, durably quarantines canonical state, then destroys credentials. */
    suspend fun forgetConfiguration(
        cancelBackgroundWork: suspend () -> Boolean,
        clearCredentials: suspend () -> Boolean,
    ): Boolean = operationMutex.withLock {
        if (!cancelBackgroundWork()) return@withLock false
        val quarantined = plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
        if (!quarantined) {
            updateError("Encrypted canonical state could not be quarantined; credentials were kept.")
            throw CanonicalAbandonmentPersistenceException()
        }
        clearCredentials()
    }

    /**
     * Treats every bearer replacement as an unknown subject/workspace, even on the same origin.
     *
     * No server-issued immutable identity exists yet, so a pending write can never be rebound. A
     * safe replacement first durably quarantines every canonical/execution cache generation while
     * the old credential is still installed; a persistence failure therefore keeps the old token.
     */
    suspend fun <T> withConfigurationUpdateLock(
        requestedBaseUrl: String,
        bearerToken: String?,
        change: suspend () -> T,
    ): T = operationMutex.withLock {
        val requestedOrigin = runCatching {
            normalizedHttpsApiBaseUrl(requestedBaseUrl)
        }.getOrNull()
        val connection = credentialStore.snapshot()
        val isCredentialReplacement = bearerToken != null
        if (!isCredentialReplacement) {
            // The credential store independently rejects URL changes without a replacement token.
            // A normalized same-URL save is intentionally a no-op and preserves configurationId.
            return@withLock change()
        }
        // Validate secret syntax before any durable cache quarantine. The same validator is used by
        // the credential store, so a rejected replacement cannot erase the old workspace locally.
        validateBearerToken(requireNotNull(bearerToken))
        if (plannerStore.hasCredentialReplacementBlocker()) {
            updateError(
                "Reconcile the pending canonical/execution action or explicitly forget the " +
                    "connection before replacing its bearer token.",
            )
            throw CanonicalConfigurationChangeBlockedException()
        }
        if (requestedOrigin == null) {
            // Let the connection store report the precise validation error without destroying the
            // old cache. No new identity can be installed through an invalid origin.
            return@withLock change()
        }
        val planner = plannerStore.state.value
        val hasCredentialBoundPlannerState = planner.canonicalSyncOrigin != null ||
            planner.canonicalDeltaCursor != null || planner.canonicalItems.isNotEmpty() ||
            planner.schedule.any { it.canonicalItemId != null } ||
            planner.canonicalExecutionSyncOrigin != null ||
            planner.canonicalExecutionSession != null ||
            planner.canonicalExecutionHistoryWindow.isNotEmpty() ||
            planner.canonicalExecutionHistoryWindowRevision != null ||
            planner.canonicalExecutionHistoryContinuityEstablished ||
            planner.canonicalExecutionHistoryVerified ||
            planner.pendingExecutionCommand != null ||
            planner.terminalExecutionOutcomes.isNotEmpty()
        if (
            connection.baseUrl != null || connection.hasBearerToken ||
            hasCredentialBoundPlannerState
        ) {
            val durable = plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
            if (!durable) {
                updateError(
                    "Encrypted canonical state could not be quarantined; the old bearer token was kept.",
                )
                throw CanonicalAbandonmentPersistenceException()
            }
        }
        change()
    }

    suspend fun refreshAndCompose(): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; the cached plan was kept.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        unresolvedLocalExecutionMessage()?.let { message ->
            if (plannerStore.state.value.pendingCanonicalMutation == null) {
                updateError(message)
                return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
        }
        return operationMutex.withLock {
            val resolution = authenticatedConfiguration()
            if (resolution is ConfigurationResolution.Failed) return@withLock resolution.outcome
            val configuration = (resolution as ConfigurationResolution.Ready).configuration
            mutableState.value = CanonicalSyncState(
                phase = CanonicalSyncPhase.SYNCING,
                message = "Syncing canonical items and composing today…",
                lastInputDigest = plannerStore.state.value.scheduleInputDigest,
                sourceItemCount = plannerStore.state.value.canonicalItems.size,
                scheduledBlockCount = plannerStore.state.value.schedule.size,
            )
            try {
                val instant = now()
                val planningZone = zoneId()
                ensureDurableWorkspaceBinding(configuration)
                val pendingResolution = reconcilePendingMutation(configuration)
                if (pendingResolution != PendingMutationResolution.SUPERSEDED) {
                    projectPendingTerminalExecution(configuration)
                }
                var loadedUpdate = loadConsistentPlan(
                    configuration = configuration,
                    instant = instant,
                    planningZone = planningZone,
                )
                var update = if (pendingResolution == PendingMutationResolution.SUPERSEDED) {
                    loadedUpdate.copy(
                        message = "${loadedUpdate.message} A pending action was superseded by newer canonical state.",
                    )
                } else {
                    loadedUpdate
                }
                ensureConfigurationCurrent(configuration)
                persistCanonicalPlan(update)
                var projectionPasses = 0
                var projectionResult = projectPendingTerminalExecution(configuration)
                while (
                    projectionPasses < MAX_TERMINAL_PROJECTION_RELOADS &&
                    projectionResult in TERMINAL_PROJECTION_RELOAD_RESULTS
                ) {
                    projectionPasses += 1
                    loadedUpdate = loadConsistentPlan(
                        configuration = configuration,
                        instant = instant,
                        planningZone = planningZone,
                    )
                    update = loadedUpdate
                    ensureConfigurationCurrent(configuration)
                    persistCanonicalPlan(update)
                    projectionResult = projectPendingTerminalExecution(configuration)
                }
                val metadataSaved = runCatching {
                    credentialStore.recordSuccessfulSync(instant.toEpochMilli())
                }.isSuccess
                mutableState.value = CanonicalSyncState(
                    phase = if (metadataSaved) {
                        CanonicalSyncPhase.CONNECTED
                    } else {
                        CanonicalSyncPhase.ERROR
                    },
                    message = if (metadataSaved) {
                        update.message
                    } else {
                        "${update.message} Last-sync metadata could not be saved."
                    },
                    lastInputDigest = update.inputDigest,
                    sourceItemCount = update.items.size,
                    scheduledBlockCount = update.schedule.size,
                )
                if (metadataSaved) {
                    CanonicalRefreshOutcome.SUCCESS
                } else {
                    CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private suspend fun loadConsistentPlan(
        configuration: AuthenticatedApiConfiguration,
        instant: Instant,
        planningZone: ZoneId,
    ): CanonicalPlanUpdate {
        val planningDate = instant.atZone(planningZone).toLocalDate()
        for (attempt in 1..MAX_SNAPSHOT_ATTEMPTS) {
            val canonical = loadDelta(configuration)
            val request = previewRequest(
                instant,
                planningZone,
                canonical.items,
                configuration.baseUrl.toString(),
                configuration.configurationId,
            )
            val preview = transport.preview(configuration, request)
            ensureConfigurationCurrent(configuration)
            try {
                return mapPreview(
                    preview = preview,
                    canonicalItems = canonical.items,
                    syncOrigin = configuration.baseUrl.toString(),
                    deltaCursor = canonical.cursor,
                    generatedAt = instant,
                    planningDate = planningDate,
                    planningZone = planningZone,
                    availabilityStart = localMinute(
                        planningDate,
                        planningZone,
                        dayStartMinute,
                    ),
                    availabilityEnd = localMinute(
                        planningDate,
                        planningZone,
                        dayEndMinute,
                    ),
                ).copy(configurationId = configuration.configurationId)
            } catch (error: RemoteSnapshotChangedException) {
                if (attempt == MAX_SNAPSHOT_ATTEMPTS) throw error
                // Neither transient delta nor preview has touched durable state. Pull again from
                // the last persisted cursor and require a preview of that exact revision map.
            }
        }
        throw RemoteSnapshotChangedException()
    }

    private suspend fun persistCanonicalPlan(update: CanonicalPlanUpdate) {
        val receipt = plannerStore.replaceCanonicalPlan(update)
        if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
    }

    suspend fun start(blockId: String): CanonicalRefreshOutcome = focusTransitionMutex.withLock {
        if (!plannerStore.state.value.isCanonicalPlanCurrent(now(), zoneId())) {
            updateError("This cached plan is not for today. Recompose before starting new work.")
            return@withLock CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (hasUnscheduledRemaining(blockId)) {
            return@withLock incompleteScheduledWorkFailure()
        }
        pauseOtherActiveBeforeLocalFocus(blockId)?.let { failure -> return failure }
        if (!requiresLocalSessionState(blockId)) {
            return@withLock mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = "in_progress",
                displayStatus = ItemStatus.ACTIVE,
                allowedStatuses = setOf(ItemStatus.SCHEDULED, ItemStatus.PAUSED),
            )
        }
        mutateLocalCanonicalBlock(
            blockId,
            ItemStatus.ACTIVE,
            setOf(ItemStatus.SCHEDULED, ItemStatus.PAUSED),
        )
    }

    suspend fun pause(blockId: String, minutes: Int? = null): CanonicalRefreshOutcome {
        require(minutes == null || minutes in 1..MAX_PAUSE_MINUTES)
        if (requiresLocalSessionState(blockId)) {
            return mutateLocalCanonicalBlock(
                blockId = blockId,
                displayStatus = ItemStatus.PAUSED,
                allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
                pauseLabel = minutes?.let { "$it minute break" } ?: "Open-ended break",
                pauseMinutes = minutes,
            )
        }
        return mutateCanonicalBlock(
            blockId = blockId,
            targetStatus = "paused",
            displayStatus = ItemStatus.PAUSED,
            allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            pauseLabel = minutes?.let { "$it minute break" } ?: "Open-ended break",
            pauseMinutes = minutes,
        )
    }

    suspend fun resume(blockId: String): CanonicalRefreshOutcome = focusTransitionMutex.withLock {
        pauseOtherActiveBeforeLocalFocus(blockId)?.let { failure -> return failure }
        if (!requiresLocalSessionState(blockId)) {
            return@withLock mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = "in_progress",
                displayStatus = ItemStatus.ACTIVE,
                allowedStatuses = setOf(ItemStatus.PAUSED),
            )
        }
        mutateLocalCanonicalBlock(
            blockId,
            ItemStatus.ACTIVE,
            setOf(ItemStatus.PAUSED),
        )
    }

    suspend fun complete(blockId: String): CanonicalRefreshOutcome {
        if (hasUnscheduledRemaining(blockId)) return incompleteScheduledWorkFailure()
        conflictingTerminalOutcome(blockId, ItemStatus.COMPLETED)?.let { return it }
        val parentResolution = terminalParentResolution(blockId, ItemStatus.COMPLETED)
        val outcome = if (parentResolution != null) {
            mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = parentResolution.wireStatus,
                displayStatus = parentResolution.displayStatus,
                allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            )
        } else if (requiresLocalSessionState(blockId)) {
            mutateLocalCanonicalBlock(
                blockId,
                ItemStatus.COMPLETED,
                setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            )
        } else {
            mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = "completed",
                displayStatus = ItemStatus.COMPLETED,
                allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            )
        }
        return recomposeAfterTerminalAction(outcome)
    }

    suspend fun skip(blockId: String): CanonicalRefreshOutcome {
        if (hasUnscheduledRemaining(blockId)) return incompleteScheduledWorkFailure()
        conflictingTerminalOutcome(blockId, ItemStatus.SKIPPED)?.let { return it }
        val parentResolution = terminalParentResolution(blockId, ItemStatus.SKIPPED)
        val outcome = if (parentResolution != null) {
            mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = parentResolution.wireStatus,
                displayStatus = parentResolution.displayStatus,
                allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            )
        } else if (requiresLocalSessionState(blockId)) {
            mutateLocalCanonicalBlock(
                blockId,
                ItemStatus.SKIPPED,
                setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            )
        } else {
            mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = "skipped",
                displayStatus = ItemStatus.SKIPPED,
                allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            )
        }
        return recomposeAfterTerminalAction(outcome)
    }

    suspend fun doLater(blockId: String): CanonicalRefreshOutcome {
        if (requiresLocalSessionState(blockId)) {
            val block = plannerStore.state.value.schedule.firstOrNull { it.id == blockId }
            if (block?.occurrenceId == null) {
                updateError(
                    "This split task cannot be deferred safely until remaining-work support is available.",
                )
                return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
            val occurrenceHasTerminalSibling = plannerStore.state.value.schedule.any {
                it.id != block.id && it.occurrenceId == block.occurrenceId &&
                    it.status in TERMINAL_DISPLAY_STATUSES
            }
            if (occurrenceHasTerminalSibling) {
                updateError(
                    "This occurrence already has finished sessions and cannot be moved as a whole.",
                )
                return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
            val saved = deferLocalCanonicalBlock(blockId)
            return if (saved == CanonicalRefreshOutcome.SUCCESS) refreshAndCompose() else saved
        }
        val mutation = mutateCanonicalBlock(
            blockId = blockId,
            targetStatus = "scheduled",
            displayStatus = ItemStatus.SCHEDULED,
            allowedStatuses = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED),
            deferUntil = now().plusSeconds(DO_LATER_SECONDS),
        )
        return if (mutation == CanonicalRefreshOutcome.SUCCESS) {
            refreshAndCompose()
        } else {
            mutation
        }
    }

    /**
     * Replaces only an item's own sensitivity bit under the same durable idempotency fence used
     * for execution-state writes. A network ambiguity therefore remains replayable after process
     * death, and a stale revision can never be silently rebased into a declassification.
     */
    suspend fun setItemSensitivity(
        itemId: String,
        expectedRevision: Long,
        isSensitive: Boolean,
    ): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; privacy was not changed.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        val outcome = operationMutex.withLock {
            val resolution = authenticatedConfiguration()
            if (resolution is ConfigurationResolution.Failed) return@withLock resolution.outcome
            val configuration = (resolution as ConfigurationResolution.Ready).configuration
            val initial = plannerStore.state.value
            if (initial.pendingCanonicalMutation != null) {
                return@withLock handleFailure(CanonicalMutationNeedsReconciliationException())
            }
            mutableState.value = CanonicalSyncState(
                phase = CanonicalSyncPhase.SYNCING,
                message = "Saving sensitive-item setting…",
                lastInputDigest = initial.scheduleInputDigest,
                sourceItemCount = initial.canonicalItems.size,
                scheduledBlockCount = initial.schedule.size,
            )
            try {
                val item = initial.canonicalItems.firstOrNull { it.id == itemId }
                    ?: throw InvalidCanonicalTransitionException()
                if (
                    expectedRevision <= 0 || item.revision != expectedRevision ||
                    item.deletedAt != null || item.revision == Long.MAX_VALUE ||
                    initial.canonicalSyncOrigin != configuration.baseUrl.toString() ||
                    initial.canonicalConfigurationId != configuration.configurationId
                ) {
                    throw InvalidCanonicalTransitionException()
                }
                if (item.isSensitive == isSensitive) {
                    mutableState.value = mutableState.value.copy(
                        phase = CanonicalSyncPhase.CONNECTED,
                        message = "Sensitive-item setting is already current",
                    )
                    return@withLock CanonicalRefreshOutcome.SUCCESS
                }
                val mutation = replaceCanonicalItemSensitivity(
                    configuration = configuration,
                    item = item,
                    targetIsSensitive = isSensitive,
                )
                if (mutation == PendingMutationResolution.SUPERSEDED) {
                    updateError("A newer item revision superseded the privacy change; review it again.")
                    return@withLock CanonicalRefreshOutcome.STALE_REVISION
                }
                val savedAt = now()
                val metadataSaved = runCatching {
                    credentialStore.recordSuccessfulSync(savedAt.toEpochMilli())
                }.isSuccess
                val current = plannerStore.state.value
                mutableState.value = CanonicalSyncState(
                    phase = if (metadataSaved) {
                        CanonicalSyncPhase.CONNECTED
                    } else {
                        CanonicalSyncPhase.ERROR
                    },
                    message = if (metadataSaved) {
                        current.scheduleMessage
                    } else {
                        "${current.scheduleMessage} Last-sync metadata could not be saved."
                    },
                    lastInputDigest = current.scheduleInputDigest,
                    sourceItemCount = current.canonicalItems.size,
                    scheduledBlockCount = current.schedule.size,
                )
                if (metadataSaved) {
                    CanonicalRefreshOutcome.SUCCESS
                } else {
                    CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
        val uncertain = plannerStore.state.value.pendingCanonicalMutation
        if (outcome == CanonicalRefreshOutcome.SUCCESS || uncertain == null) return outcome
        val reconciled = refreshAndCompose()
        if (reconciled != CanonicalRefreshOutcome.SUCCESS) return outcome
        val authoritative = plannerStore.state.value.canonicalItems.firstOrNull {
            it.id == uncertain.itemId
        }
        return if (
            authoritative != null && authoritative.revision > uncertain.expectedRevision &&
            authoritative.status == uncertain.targetStatus &&
            authoritative.isSensitive == uncertain.targetIsSensitive
        ) {
            CanonicalRefreshOutcome.SUCCESS
        } else {
            updateError("The uncertain privacy change was reconciled and was not applied.")
            outcome
        }
    }

    private suspend fun recomposeAfterTerminalAction(
        outcome: CanonicalRefreshOutcome,
    ): CanonicalRefreshOutcome {
        if (outcome != CanonicalRefreshOutcome.SUCCESS) return outcome
        // A partial split session is durable locally, but the current preview contract cannot
        // express remaining work. Keep the accepted plan intact until its sibling sessions resolve.
        if (unresolvedLocalExecutionMessage() != null) return outcome
        return refreshAndCompose()
    }

    private fun terminalParentResolution(
        blockId: String,
        requestedStatus: ItemStatus,
    ): ParentTerminalResolution? {
        val state = plannerStore.state.value
        val block = state.schedule.firstOrNull { it.id == blockId } ?: return null
        val itemId = block.canonicalItemId ?: return null
        val item = state.canonicalItems.firstOrNull { it.id == itemId } ?: return null
        if (item.recurrenceJson != null || block.occurrenceId != null) return null
        val group = state.schedule.filter {
            it.canonicalItemId == itemId && it.occurrenceId == block.occurrenceId
        }
        val splitType = parseJsonObject(item.splitPolicyJson)["type"]
            ?.let { it as? JsonPrimitive }
            ?.contentOrNull
        if (splitType != "splittable" && !block.isSplittable && group.size <= 1) return null
        val resultingStatuses = group.map {
            if (it.id == blockId) requestedStatus else it.status
        }
        if (resultingStatuses.any { it !in TERMINAL_DISPLAY_STATUSES }) return null
        val displayStatus = if (resultingStatuses.all { it == ItemStatus.SKIPPED }) {
            ItemStatus.SKIPPED
        } else {
            ItemStatus.COMPLETED
        }
        return ParentTerminalResolution(
            wireStatus = if (displayStatus == ItemStatus.SKIPPED) "skipped" else "completed",
            displayStatus = displayStatus,
        )
    }

    private fun conflictingTerminalOutcome(
        blockId: String,
        requestedStatus: ItemStatus,
    ): CanonicalRefreshOutcome? {
        val state = plannerStore.state.value
        val block = state.schedule.firstOrNull { it.id == blockId } ?: return null
        val itemId = block.canonicalItemId ?: return null
        val group = if (block.occurrenceId != null) {
            state.schedule.filter { it.occurrenceId == block.occurrenceId }
        } else {
            state.schedule.filter {
                it.canonicalItemId == itemId && it.occurrenceId == null
            }
        }
        if (group.any { sibling ->
                sibling.id != blockId && sibling.status in TERMINAL_DISPLAY_STATUSES &&
                    sibling.status != requestedStatus
            }
        ) {
            updateError(
                "Split sessions must use one final outcome; complete or skip the remaining sessions consistently.",
            )
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        return null
    }

    private fun hasUnscheduledRemaining(blockId: String): Boolean {
        val state = plannerStore.state.value
        val block = state.schedule.firstOrNull { it.id == blockId } ?: return false
        val itemId = block.canonicalItemId ?: return false
        return state.unscheduledWork.any {
            it.remainingMinutes > 0 && if (block.occurrenceId != null) {
                it.occurrenceId == block.occurrenceId
            } else {
                it.itemId == itemId && it.occurrenceId == null
            }
        }
    }

    private fun incompleteScheduledWorkFailure(): CanonicalRefreshOutcome {
        updateError(
            "This visible session does not cover all required work; compose more capacity before completing or skipping it.",
        )
        return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
    }

    private fun unresolvedLocalExecutionMessage(): String? {
        val state = plannerStore.state.value
        if (state.schedule.any {
                it.canonicalItemId != null &&
                    it.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED)
            }
        ) {
            return "Finish, skip, or defer the active canonical session before recomposing."
        }
        val items = state.canonicalItems.associateBy(CanonicalItemSnapshot::id)
        val localGroups = state.schedule
            .filter { it.canonicalItemId != null }
            .groupBy { block ->
                val occurrenceId = block.occurrenceId
                if (occurrenceId != null) {
                    state.occurrenceSeriesItemIds[occurrenceId] to occurrenceId
                } else {
                    block.canonicalItemId to null
                }
            }
            .filter { (identity, blocks) ->
                val item = identity.first?.let(items::get) ?: return@filter false
                val split = parseJsonObject(item.splitPolicyJson)["type"]
                    ?.let { it as? JsonPrimitive }
                    ?.contentOrNull == "splittable"
                item.recurrenceJson != null || identity.second != null || split || blocks.size > 1
            }
        if (localGroups.values.any { blocks ->
                blocks.any { it.status in TERMINAL_DISPLAY_STATUSES } &&
                    blocks.any { it.status !in TERMINAL_DISPLAY_STATUSES }
            }
        ) {
            return "Finish the remaining split sessions before recomposing; partial remaining work is kept locally."
        }
        return null
    }

    private fun requiresLocalSessionState(blockId: String): Boolean {
        val state = plannerStore.state.value
        val block = state.schedule.firstOrNull { it.id == blockId } ?: return false
        val itemId = block.canonicalItemId ?: return false
        val item = state.canonicalItems.firstOrNull { it.id == itemId } ?: return false
        val matchingBlocks = state.schedule.count {
            it.canonicalItemId == itemId && it.occurrenceId == block.occurrenceId
        }
        val splitType = parseJsonObject(item.splitPolicyJson)["type"]
            ?.let { it as? JsonPrimitive }
            ?.contentOrNull
        return item.recurrenceJson != null || block.occurrenceId != null ||
            block.isSplittable || splitType == "splittable" || matchingBlocks > 1
    }

    private fun pauseOtherActiveBeforeLocalFocus(
        blockId: String,
    ): CanonicalRefreshOutcome? {
        val activeId = plannerStore.state.value.activeSession?.itemId ?: return null
        if (activeId == blockId) return null
        updateError("Finish, skip, or defer the current session before starting another one.")
        return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
    }

    private suspend fun mutateLocalCanonicalBlock(
        blockId: String,
        displayStatus: ItemStatus,
        allowedStatuses: Set<ItemStatus>,
        pauseLabel: String? = null,
        pauseMinutes: Int? = null,
    ): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; the cached plan was kept.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        return operationMutex.withLock {
            val initial = plannerStore.state.value
            val previousSyncState = mutableState.value
            val block = initial.schedule.firstOrNull { it.id == blockId }
            if (
                block?.canonicalItemId == null || block.status !in allowedStatuses ||
                !requiresLocalSessionState(blockId)
            ) {
                return@withLock handleFailure(InvalidCanonicalTransitionException())
            }
            mutableState.value = CanonicalSyncState(
                phase = CanonicalSyncPhase.SYNCING,
                message = "Saving session state…",
                lastInputDigest = initial.scheduleInputDigest,
                sourceItemCount = initial.canonicalItems.size,
                scheduledBlockCount = initial.schedule.size,
            )
            try {
                val receipt = plannerStore.reconcileLocalCanonicalSession(
                    focusedBlockId = blockId,
                    displayStatus = displayStatus,
                    pauseLabel = pauseLabel,
                    pauseMinutes = pauseMinutes,
                )
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalPlannerStorageException()
                }
                val current = plannerStore.state.value
                mutableState.value = CanonicalSyncState(
                    phase = settledLocalPhase(previousSyncState.phase),
                    message = current.scheduleMessage,
                    lastInputDigest = current.scheduleInputDigest,
                    sourceItemCount = current.canonicalItems.size,
                    scheduledBlockCount = current.schedule.size,
                )
                CanonicalRefreshOutcome.SUCCESS
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private suspend fun deferLocalCanonicalBlock(blockId: String): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; the cached plan was kept.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        return operationMutex.withLock {
            val initial = plannerStore.state.value
            val previousSyncState = mutableState.value
            val block = initial.schedule.firstOrNull { it.id == blockId }
            if (
                block?.canonicalItemId == null ||
                block.status !in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED) ||
                !requiresLocalSessionState(blockId)
            ) {
                return@withLock handleFailure(InvalidCanonicalTransitionException())
            }
            mutableState.value = CanonicalSyncState(
                CanonicalSyncPhase.SYNCING,
                "Saving a recurrence/session deferral…",
            )
            try {
                val receipt = plannerStore.deferLocalCanonicalSession(blockId, 60)
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalPlannerStorageException()
                }
                val current = plannerStore.state.value
                mutableState.value = CanonicalSyncState(
                    phase = settledLocalPhase(previousSyncState.phase),
                    message = current.scheduleMessage,
                    lastInputDigest = current.scheduleInputDigest,
                    sourceItemCount = current.canonicalItems.size,
                    scheduledBlockCount = current.schedule.size,
                )
                // The caller recomposes after this lock is released. Until then the prior exact
                // placement remains visibly pending, never locally accepted as conflict-free.
                CanonicalRefreshOutcome.SUCCESS
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private fun settledLocalPhase(previous: CanonicalSyncPhase): CanonicalSyncPhase = when (previous) {
        CanonicalSyncPhase.SYNCING -> stateFrom(credentialStore.snapshot()).phase
        else -> previous
    }

    /**
     * Replaces the complete mutable item contract under optimistic concurrency, then updates the
     * encrypted cache only after the server response is validated. The UI can render the accepted
     * response while the exact encrypted save is in flight, but success is not reported until that
     * generation is durable; a crash is recoverable from the still-unadvanced delta cursor.
     */
    private suspend fun mutateCanonicalBlock(
        blockId: String,
        targetStatus: String,
        displayStatus: ItemStatus,
        allowedStatuses: Set<ItemStatus>,
        pauseLabel: String? = null,
        pauseMinutes: Int? = null,
        deferUntil: Instant? = null,
    ): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; the cached plan was kept.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        val outcome = operationMutex.withLock {
            val resolution = authenticatedConfiguration()
            if (resolution is ConfigurationResolution.Failed) return@withLock resolution.outcome
            val configuration = (resolution as ConfigurationResolution.Ready).configuration
            val initial = plannerStore.state.value
            if (initial.pendingCanonicalMutation != null) {
                return@withLock handleFailure(CanonicalMutationNeedsReconciliationException())
            }
            mutableState.value = CanonicalSyncState(
                phase = CanonicalSyncPhase.SYNCING,
                message = "Saving canonical item state…",
                lastInputDigest = initial.scheduleInputDigest,
                sourceItemCount = initial.canonicalItems.size,
                scheduledBlockCount = initial.schedule.size,
            )
            try {
                val requestedBlock = initial.schedule.firstOrNull { it.id == blockId }
                    ?: throw InvalidCanonicalTransitionException()
                if (requestedBlock.status !in allowedStatuses) {
                    throw InvalidCanonicalTransitionException()
                }
                if (requestedBlock.canonicalItemId == null) {
                    throw InvalidCanonicalTransitionException()
                }

                replaceCanonicalBlock(
                    configuration = configuration,
                    blockId = blockId,
                    targetStatus = targetStatus,
                    displayStatus = displayStatus,
                    pauseLabel = pauseLabel,
                    pauseMinutes = pauseMinutes,
                    deferUntil = deferUntil,
                )
                val savedAt = now()
                val metadataSaved = runCatching {
                    credentialStore.recordSuccessfulSync(savedAt.toEpochMilli())
                }.isSuccess
                val current = plannerStore.state.value
                mutableState.value = CanonicalSyncState(
                    phase = if (metadataSaved) {
                        CanonicalSyncPhase.CONNECTED
                    } else {
                        CanonicalSyncPhase.ERROR
                    },
                    message = if (metadataSaved) {
                        current.scheduleMessage
                    } else {
                        "${current.scheduleMessage} Last-sync metadata could not be saved."
                    },
                    lastInputDigest = current.scheduleInputDigest,
                    sourceItemCount = current.canonicalItems.size,
                    scheduledBlockCount = current.schedule.size,
                )
                if (metadataSaved) {
                    CanonicalRefreshOutcome.SUCCESS
                } else {
                    CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
        val uncertain = plannerStore.state.value.pendingCanonicalMutation
        if (outcome == CanonicalRefreshOutcome.SUCCESS || uncertain == null) return outcome
        val reconciled = refreshAndCompose()
        if (reconciled != CanonicalRefreshOutcome.SUCCESS) return outcome
        val authoritative = plannerStore.state.value.canonicalItems.firstOrNull {
            it.id == uncertain.itemId
        }
        return if (
            authoritative != null && authoritative.revision > uncertain.expectedRevision &&
            authoritative.status == uncertain.targetStatus &&
            authoritative.isSensitive == uncertain.targetIsSensitive
        ) {
            CanonicalRefreshOutcome.SUCCESS
        } else {
            updateError("The uncertain action was reconciled and was not applied by the server.")
            outcome
        }
    }

    private suspend fun replaceCanonicalBlock(
        configuration: AuthenticatedApiConfiguration,
        blockId: String,
        targetStatus: String,
        displayStatus: ItemStatus,
        pauseLabel: String? = null,
        pauseMinutes: Int? = null,
        deferUntil: Instant? = null,
        terminalExecutionSessionId: String? = null,
    ): PendingMutationResolution {
        val current = plannerStore.state.value
        val block = current.schedule.firstOrNull { it.id == blockId }
            ?: throw InvalidCanonicalTransitionException()
        val itemId = block.canonicalItemId ?: throw InvalidCanonicalTransitionException()
        val item = current.canonicalItems.firstOrNull { it.id == itemId }
            ?: throw InvalidCanonicalTransitionException()
        if (
            block.canonicalRevision != item.revision || !item.isExecutable ||
            item.revision == Long.MAX_VALUE ||
            current.canonicalSyncOrigin != configuration.baseUrl.toString() ||
            current.canonicalConfigurationId != configuration.configurationId
        ) {
            throw InvalidCanonicalTransitionException()
        }
        val earliestStartAt = deferUntil?.let { threshold ->
            val existing = item.earliestStartAt?.let(::parseTimestamp)?.toInstant()
            if (existing != null && existing >= threshold) item.earliestStartAt else threshold.toString()
        } ?: item.earliestStartAt
        val replacement = canonicalReplacement(
            item = item,
            targetStatus = targetStatus,
            targetIsSensitive = item.isSensitive,
            earliestStartAt = earliestStartAt,
        )
        val idempotencyKey = UUID.randomUUID().toString()
        val replaceRequest = ReplaceCanonicalItemRequest(
            expectedRevision = item.revision,
            item = replacement,
        )
        val pending = PendingCanonicalMutation(
                idempotencyKey = idempotencyKey,
                syncOrigin = configuration.baseUrl.toString(),
                configurationId = configuration.configurationId,
                itemId = item.id,
                expectedRevision = item.revision,
                targetStatus = targetStatus,
                targetIsSensitive = item.isSensitive,
                startedAt = now().toString(),
                replacementRequestJson = mutationJson.encodeToString(replaceRequest),
                focusedBlockId = blockId,
                displayStatus = displayStatus,
                pauseLabel = pauseLabel,
                pauseMinutes = pauseMinutes,
                terminalExecutionSessionId = terminalExecutionSessionId,
        )
        val pendingReceipt = plannerStore.stageCanonicalMutation(pending)
        if (pendingReceipt == null || !pendingReceipt.awaitDurable()) {
            throw LocalPlannerStorageException()
        }
        return sendAndReconcileCanonicalMutation(
            configuration = configuration,
            pending = pending,
            previous = item,
            request = replaceRequest,
            reconcileConflict = terminalExecutionSessionId != null,
        )
    }

    private suspend fun replaceCanonicalItemSensitivity(
        configuration: AuthenticatedApiConfiguration,
        item: CanonicalItemSnapshot,
        targetIsSensitive: Boolean,
    ): PendingMutationResolution {
        val current = plannerStore.state.value
        if (
            current.canonicalItems.firstOrNull { it.id == item.id } != item ||
            current.canonicalSyncOrigin != configuration.baseUrl.toString() ||
            current.canonicalConfigurationId != configuration.configurationId
        ) {
            throw InvalidCanonicalTransitionException()
        }
        val replaceRequest = ReplaceCanonicalItemRequest(
            expectedRevision = item.revision,
            item = canonicalReplacement(
                item = item,
                targetStatus = item.status,
                targetIsSensitive = targetIsSensitive,
                earliestStartAt = item.earliestStartAt,
            ),
        )
        val pending = PendingCanonicalMutation(
            idempotencyKey = UUID.randomUUID().toString(),
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = configuration.configurationId,
            itemId = item.id,
            expectedRevision = item.revision,
            targetStatus = item.status,
            targetIsSensitive = targetIsSensitive,
            startedAt = now().toString(),
            replacementRequestJson = mutationJson.encodeToString(replaceRequest),
            // Sensitivity writes can target an unscheduled parent. The canonical UUID is a stable
            // non-secret sentinel; sensitivity reconciliation never treats it as a block ID.
            focusedBlockId = item.id,
            displayStatus = mapItemStatus(item.status),
        )
        val pendingReceipt = plannerStore.stageCanonicalMutation(pending)
        if (pendingReceipt == null || !pendingReceipt.awaitDurable()) {
            throw LocalPlannerStorageException()
        }
        return sendAndReconcileCanonicalMutation(
            configuration = configuration,
            pending = pending,
            previous = item,
            request = replaceRequest,
            reconcileConflict = true,
        )
    }

    private fun canonicalReplacement(
        item: CanonicalItemSnapshot,
        targetStatus: String,
        targetIsSensitive: Boolean,
        earliestStartAt: String?,
    ) = CanonicalItemReplacement(
        isSensitive = targetIsSensitive,
        kind = item.kind,
        status = targetStatus,
        title = item.title,
        notes = item.notes,
        timezoneName = item.timezoneName,
        durationSeconds = item.durationSeconds,
        deadlineAt = item.deadlineAt,
        earliestStartAt = earliestStartAt,
        recurrence = item.recurrenceJson?.let(::parseJsonObject),
        flexibleConstraints = parseJsonObject(item.flexibleConstraintsJson),
        splitPolicy = parseJsonObject(item.splitPolicyJson),
        importance = item.importance,
        urgency = item.urgency,
        parentId = item.parentId,
        siblingOrder = item.siblingOrder,
    )

    /**
     * Projects one confirmed terminal execution onto an eligible one-shot parent item.
     *
     * The execution row itself is already durable. This second write deliberately uses the same
     * canonical replacement fence as every user item mutation, so response loss and process death
     * replay the exact body/idempotency key instead of issuing a second status transition.
     */
    private suspend fun projectPendingTerminalExecution(
        configuration: AuthenticatedApiConfiguration,
    ): TerminalProjectionResult {
        val current = plannerStore.state.value
        val origin = configuration.baseUrl.toString()
        val unresolved = current.terminalExecutionOutcomes.values
            .filter {
                it.syncOrigin == origin && it.requiresCanonicalItemProjection &&
                    it.canonicalProjectionRevision == null &&
                    it.canonicalProjectionResolution == null
            }
        val outcome = unresolved
            .filter {
                it.canonicalProjectionConflict == null ||
                    it.canonicalProjectionRetryAuthorizedAt != null
            }
            .minWithOrNull(
                compareBy<com.greengolddog.dayweave.model.TerminalExecutionOutcomeSnapshot> {
                    Instant.parse(it.recordedAt)
                }.thenBy { it.session.id },
            )
            ?: return if (unresolved.isEmpty()) {
                TerminalProjectionResult.NONE
            } else {
                TerminalProjectionResult.CONFLICT
            }
        val session = outcome.session
        val item = current.canonicalItems.firstOrNull { it.id == session.itemId }
        if (item == null) {
            if (current.canonicalDeltaCursor == null) {
                return persistTerminalProjectionConflict(
                    outcome.session.id,
                    "Canonical cache is incomplete; refresh before resolving this execution outcome.",
                    outcome.canonicalProjectionConflict,
                )
            }
            val receipt = plannerStore.resolveDeletedTerminalProjection(session.id)
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return TerminalProjectionResult.RESOLVED_WITHOUT_WRITE
        }
        if (item.status in TERMINAL_CANONICAL_STATUSES) {
            if (item.status != session.status) {
                return persistTerminalProjectionConflict(
                    session.id,
                    "Execution ended as ${session.status}, but the latest canonical item is " +
                        "${item.status}. Choose which result should remain authoritative.",
                    outcome.canonicalProjectionConflict,
                )
            }
            val receipt = plannerStore.markTerminalProjectionApplied(session.id, item.revision)
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return TerminalProjectionResult.RESOLVED_WITHOUT_WRITE
        }
        val itemBlocks = current.schedule.filter {
            it.canonicalItemId == item.id && it.occurrenceId == null
        }
        val block = itemBlocks.singleOrNull()?.takeIf {
            it.sessionIndex == session.sessionIndex && it.canonicalRevision == item.revision
        }
        val splitType = parseJsonObject(item.splitPolicyJson)["type"]
            ?.let { it as? JsonPrimitive }
            ?.contentOrNull
        val conflict = when {
            current.canonicalSyncOrigin != origin ||
                current.canonicalConfigurationId != configuration.configurationId ->
                "The latest item belongs to a different canonical connection."
            current.pendingCanonicalMutation != null ->
                "Another canonical write must be reconciled before this execution outcome."
            item.status !in TERMINAL_PROJECTION_SOURCE_STATUSES ->
                "The latest canonical status '${item.status}' cannot accept an execution outcome."
            !item.isExecutable ->
                "The latest canonical item is no longer an executable leaf."
            item.recurrenceJson != null || session.occurrenceId != null ->
                "The latest canonical item is recurring; its old one-shot outcome cannot be rebased."
            splitType != "indivisible" || block?.isSplittable == true ->
                "The latest canonical item is splittable; its old one-shot outcome cannot be rebased."
            itemBlocks.size != 1 || block == null ->
                "The latest canonical item is not represented by one fully scheduled session."
            current.unscheduledWork.any {
                it.itemId == item.id && it.occurrenceId == null && it.remainingMinutes > 0
            } -> "The latest canonical item still has unscheduled work."
            else -> null
        }
        if (conflict != null) {
            return persistTerminalProjectionConflict(
                session.id,
                conflict,
                outcome.canonicalProjectionConflict,
            )
        }
        val displayStatus = when (session.status) {
            "completed" -> ItemStatus.COMPLETED
            "skipped" -> ItemStatus.SKIPPED
            else -> throw InvalidCanonicalTransitionException()
        }
        return when (
            replaceCanonicalBlock(
            configuration = configuration,
            blockId = requireNotNull(block).id,
            targetStatus = session.status,
            displayStatus = displayStatus,
            terminalExecutionSessionId = session.id,
            )
        ) {
            PendingMutationResolution.APPLIED -> TerminalProjectionResult.APPLIED_WRITE
            PendingMutationResolution.SUPERSEDED -> TerminalProjectionResult.NEEDS_RELOAD
            PendingMutationResolution.NONE -> throw InvalidCanonicalTransitionException()
        }
    }

    private suspend fun persistTerminalProjectionConflict(
        sessionId: String,
        conflict: String,
        previousConflict: String?,
    ): TerminalProjectionResult {
        val outcome = plannerStore.state.value.terminalExecutionOutcomes[sessionId]
        if (
            conflict == previousConflict &&
            outcome?.canonicalProjectionRetryAuthorizedAt == null
        ) {
            return TerminalProjectionResult.CONFLICT
        }
        val receipt = plannerStore.recordTerminalProjectionConflict(sessionId, conflict)
        if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
        return TerminalProjectionResult.CONFLICT
    }

    /** Replays exactly one durable request; a read-only refresh can never clear write uncertainty. */
    private suspend fun reconcilePendingMutation(
        configuration: AuthenticatedApiConfiguration,
    ): PendingMutationResolution {
        val pending = plannerStore.state.value.pendingCanonicalMutation
            ?: return PendingMutationResolution.NONE
        if (
            pending.syncOrigin != configuration.baseUrl.toString() ||
            pending.configurationId != configuration.configurationId
        ) {
            throw CanonicalMutationNeedsReconciliationException()
        }
        validateUuid(pending.idempotencyKey)
        validateUuid(pending.itemId)
        validateUuid(pending.focusedBlockId)
        pending.terminalExecutionSessionId?.let(::validateUuid)
        parseTimestamp(pending.startedAt)
        if (
            pending.expectedRevision <= 0 || pending.replacementRequestJson.length >
            MAX_PENDING_MUTATION_JSON_CHARS
        ) {
            throw RemotePlannerMappingException()
        }
        val request = try {
            mutationJson.decodeFromString<ReplaceCanonicalItemRequest>(
                pending.replacementRequestJson,
            )
        } catch (error: IllegalArgumentException) {
            throw RemotePlannerMappingException(error)
        }
        val previous = plannerStore.state.value.canonicalItems.firstOrNull {
            it.id == pending.itemId
        } ?: throw InvalidCanonicalTransitionException()
        if (
            request.expectedRevision != pending.expectedRevision ||
            request.item.status != pending.targetStatus ||
            request.item.isSensitive != pending.targetIsSensitive ||
            previous.revision != pending.expectedRevision
        ) {
            throw RemotePlannerMappingException()
        }
        return sendAndReconcileCanonicalMutation(
            configuration = configuration,
            pending = pending,
            previous = previous,
            request = request,
            reconcileConflict = true,
        )
    }

    /** Sends one exact durable body and classifies every non-success response conservatively. */
    private suspend fun sendAndReconcileCanonicalMutation(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingCanonicalMutation,
        previous: CanonicalItemSnapshot,
        request: ReplaceCanonicalItemRequest,
        reconcileConflict: Boolean,
    ): PendingMutationResolution {
        val response = try {
            transport.replaceItem(
                configuration = configuration,
                id = pending.itemId,
                idempotencyKey = pending.idempotencyKey,
                request = request,
            )
        } catch (conflict: PlannerApiException.Conflict) {
            if (pending.terminalExecutionSessionId != null) {
                return reconcileRejectedTerminalProjection(
                    configuration = configuration,
                    pending = pending,
                    previous = previous,
                    request = request,
                    rejection = TerminalProjectionRejection.CONFLICT,
                    originalError = conflict,
                )
            }
            if (!reconcileConflict) throw conflict
            return reconcileExpiredOrSupersededPendingMutation(
                configuration = configuration,
                pending = pending,
                previous = previous,
                request = request,
                originalConflict = conflict,
            )
        } catch (notFound: PlannerApiException.Http) {
            if (pending.terminalExecutionSessionId == null || notFound.statusCode != 404) {
                throw notFound
            }
            return reconcileRejectedTerminalProjection(
                configuration = configuration,
                pending = pending,
                previous = previous,
                request = request,
                rejection = TerminalProjectionRejection.NOT_FOUND,
                originalError = notFound,
            )
        } catch (rejected: PlannerApiException.Validation) {
            if (
                pending.terminalExecutionSessionId == null ||
                rejected.statusCode !in setOf(400, 422)
            ) {
                throw rejected
            }
            return reconcileRejectedTerminalProjection(
                configuration = configuration,
                pending = pending,
                previous = previous,
                request = request,
                rejection = TerminalProjectionRejection.DETERMINISTIC_REJECTION,
                originalError = rejected,
            )
        }
        ensureConfigurationCurrent(configuration)
        if (
            response.id != pending.itemId || response.status != pending.targetStatus ||
            response.isSensitive != pending.targetIsSensitive ||
            response.revision != pending.expectedRevision + 1
        ) {
            throw RemotePlannerMappingException()
        }
        val mapped = mapCanonicalItem(response)
        if (!matchesReplacement(mapped, previous, request.item)) {
            throw RemotePlannerMappingException()
        }
        val receipt = reconcileCanonicalMutation(mapped, pending, previous)
        if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
        return PendingMutationResolution.APPLIED
    }

    /**
     * A rejected terminal PUT can release its fence only after a complete authoritative delta.
     * Any read, mapping, or configuration failure escapes before local state changes, retaining the
     * exact request and idempotency key for a later unambiguous replay.
     */
    private suspend fun reconcileRejectedTerminalProjection(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingCanonicalMutation,
        previous: CanonicalItemSnapshot,
        request: ReplaceCanonicalItemRequest,
        rejection: TerminalProjectionRejection,
        originalError: Throwable,
    ): PendingMutationResolution {
        val sessionId = requireNotNull(pending.terminalExecutionSessionId)
        val authoritative = loadDelta(configuration).items.firstOrNull { it.id == pending.itemId }
        ensureConfigurationCurrent(configuration)
        if (authoritative == null) {
            val receipt = plannerStore.resolveDeletedPendingTerminalProjection(
                idempotencyKey = pending.idempotencyKey,
                sessionId = sessionId,
            )
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return PendingMutationResolution.APPLIED
        }
        if (rejection == TerminalProjectionRejection.NOT_FOUND) {
            // A 404 with a still-readable item is contradictory. Keep the exact write fence; a
            // later authoritative read may prove a tombstone, but this read cannot.
            throw originalError
        }
        if (rejection == TerminalProjectionRejection.DETERMINISTIC_REJECTION) {
            if (
                authoritative.revision > pending.expectedRevision &&
                authoritative.status == pending.targetStatus &&
                authoritative.isSensitive == pending.targetIsSensitive &&
                matchesReplacement(authoritative, previous, request.item)
            ) {
                val receipt = reconcileCanonicalMutation(authoritative, pending, previous)
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalPlannerStorageException()
                }
                return PendingMutationResolution.APPLIED
            }
            val statusCode = (originalError as? PlannerApiException.Validation)?.statusCode
                ?: throw originalError
            val receipt = plannerStore.rejectPendingTerminalProjectionAsConflict(
                idempotencyKey = pending.idempotencyKey,
                sessionId = sessionId,
                conflict = "The server rejected this terminal status with HTTP $statusCode. " +
                    "Authoritative item revision ${authoritative.revision} does not exactly " +
                    "match the approved terminal replacement; review it before retrying.",
            )
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return PendingMutationResolution.APPLIED
        }
        if (
            authoritative.revision > pending.expectedRevision &&
            authoritative.status == pending.targetStatus &&
            authoritative.isSensitive == pending.targetIsSensitive
        ) {
            val receipt = reconcileCanonicalMutation(authoritative, pending, previous)
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return PendingMutationResolution.APPLIED
        }
        if (authoritative.revision > pending.expectedRevision) {
            val receipt = plannerStore.clearPendingCanonicalMutation(
                idempotencyKey = pending.idempotencyKey,
                message = "Pending terminal projection was superseded by newer canonical state",
            )
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return PendingMutationResolution.SUPERSEDED
        }
        // A 409 and an item still at the expected revision are a mixed or unstable read. Retain
        // the exact uncertainty fence; no new body or key may be invented.
        throw originalError
    }

    private suspend fun reconcileExpiredOrSupersededPendingMutation(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingCanonicalMutation,
        previous: CanonicalItemSnapshot,
        request: ReplaceCanonicalItemRequest,
        originalConflict: PlannerApiException.Conflict,
    ): PendingMutationResolution {
        val authoritative = loadDelta(configuration).items.firstOrNull { it.id == pending.itemId }
        ensureConfigurationCurrent(configuration)
        if (authoritative != null && authoritative.revision <= pending.expectedRevision) {
            // The original write may still be in progress. Keep the fence and retry the exact key.
            throw originalConflict
        }
        if (
            authoritative != null && authoritative.status == pending.targetStatus &&
            authoritative.isSensitive == pending.targetIsSensitive &&
            matchesReplacement(authoritative, previous, request.item)
        ) {
            val receipt = reconcileCanonicalMutation(authoritative, pending, previous)
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
            return PendingMutationResolution.APPLIED
        }
        // A greater revision (or tombstone) proves the expected revision can no longer commit.
        val receipt = plannerStore.clearPendingCanonicalMutation(
            idempotencyKey = pending.idempotencyKey,
            message = "Pending action was superseded by newer canonical state",
        )
        if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
        return PendingMutationResolution.SUPERSEDED
    }

    private fun reconcileCanonicalMutation(
        item: CanonicalItemSnapshot,
        pending: PendingCanonicalMutation,
        previous: CanonicalItemSnapshot,
    ) = if (pending.targetIsSensitive != previous.isSensitive) {
        plannerStore.reconcileCanonicalItemSensitivity(item)
    } else {
        plannerStore.reconcileCanonicalItem(
            item = item,
            focusedBlockId = pending.focusedBlockId,
            displayStatus = pending.displayStatus,
            pauseLabel = pending.pauseLabel,
            pauseMinutes = pending.pauseMinutes,
        )
    }

    private fun matchesReplacement(
        actual: CanonicalItemSnapshot,
        previous: CanonicalItemSnapshot,
        replacement: CanonicalItemReplacement,
    ): Boolean =
        actual.isSensitive == replacement.isSensitive &&
            actual.kind == replacement.kind &&
            actual.title == replacement.title &&
            actual.notes == replacement.notes &&
            actual.timezoneName == replacement.timezoneName &&
            actual.durationSeconds == replacement.durationSeconds &&
            sameOptionalTimestamp(actual.deadlineAt, replacement.deadlineAt) &&
            sameOptionalTimestamp(actual.earliestStartAt, replacement.earliestStartAt) &&
            actual.recurrenceJson?.let(::parseJsonObject) == replacement.recurrence &&
            parseJsonObject(actual.flexibleConstraintsJson) == replacement.flexibleConstraints &&
            parseJsonObject(actual.splitPolicyJson) == replacement.splitPolicy &&
            actual.importance == replacement.importance &&
            actual.urgency == replacement.urgency &&
            actual.parentId == replacement.parentId &&
            actual.siblingOrder == replacement.siblingOrder &&
            actual.isExecutable == previous.isExecutable &&
            sameOptionalTimestamp(actual.createdAt, previous.createdAt) &&
            parseTimestamp(actual.updatedAt) >= parseTimestamp(previous.updatedAt) &&
            if (replacement.status == "completed") {
                actual.completedAt != null
            } else {
                actual.completedAt == null
            }

    private fun sameOptionalTimestamp(left: String?, right: String?): Boolean = when {
        left == null || right == null -> left == right
        else -> parseTimestamp(left).toInstant() == parseTimestamp(right).toInstant()
    }

    private suspend fun loadDelta(
        configuration: AuthenticatedApiConfiguration,
    ): CanonicalDeltaSnapshot {
        val cached = plannerStore.state.value
        val sameBinding = cached.canonicalSyncOrigin == configuration.baseUrl.toString() &&
            cached.canonicalConfigurationId == configuration.configurationId
        val firstCursor = cached.canonicalDeltaCursor.takeIf { sameBinding }
        val initialItems = if (firstCursor == null) {
            emptyList()
        } else {
            cached.canonicalItems
        }
        return try {
            loadDeltaPages(configuration, firstCursor, initialItems)
        } catch (error: PlannerApiException.Validation) {
            if (firstCursor == null || error.statusCode != 422) throw error
            // A server restore or repository replacement intentionally invalidates its opaque
            // cursor scope. Rebuild from the beginning instead of merging across repositories.
            loadDeltaPages(configuration, null, emptyList())
        }
    }

    private suspend fun loadDeltaPages(
        configuration: AuthenticatedApiConfiguration,
        initialCursor: String?,
        initialItems: List<CanonicalItemSnapshot>,
    ): CanonicalDeltaSnapshot {
        val items = initialItems.associateByTo(linkedMapOf(), CanonicalItemSnapshot::id)
        if (items.size != initialItems.size) throw RemotePlannerMappingException()
        var retainedBytes = initialItems.sumOf(::estimatedCanonicalItemBytes)
        if (retainedBytes > MAX_CANONICAL_CACHE_ESTIMATED_BYTES) {
            throw RemotePlannerMappingException()
        }
        var cursor = initialCursor
        var pageCount = 0
        var changeCount = 0
        val seenCursors = mutableSetOf<String?>()
        while (true) {
            if (++pageCount > MAX_DELTA_PAGES) throw RemotePlannerMappingException()
            if (cursor != null && !validCursor(cursor)) throw RemotePlannerMappingException()
            if (!seenCursors.add(cursor)) throw RemotePlannerMappingException()
            val page = transport.itemDelta(configuration, cursor)
            if (
                page.changes.size > MAX_DELTA_PAGE_SIZE || !validCursor(page.nextCursor) ||
                (page.hasMore && page.nextCursor in seenCursors)
            ) {
                throw RemotePlannerMappingException()
            }
            changeCount += page.changes.size
            if (changeCount > MAX_DELTA_CHANGES) throw RemotePlannerMappingException()
            page.changes.forEach { change ->
                retainedBytes += applyDeltaChange(items, change)
                if (
                    items.size > MAX_CANONICAL_ITEMS || retainedBytes < 0 ||
                    retainedBytes > MAX_CANONICAL_CACHE_ESTIMATED_BYTES
                ) {
                    throw RemotePlannerMappingException()
                }
            }
            cursor = page.nextCursor
            if (!page.hasMore) break
        }
        if (items.size > MAX_CANONICAL_ITEMS) throw RemotePlannerMappingException()
        validateHierarchy(items)
        return CanonicalDeltaSnapshot(
            items = items.values.sortedWith(
                compareBy({ it.parentId.orEmpty() }, { it.siblingOrder }, { it.id }),
            ),
            cursor = requireNotNull(cursor),
        )
    }

    private fun applyDeltaChange(
        items: MutableMap<String, CanonicalItemSnapshot>,
        change: RemoteItemDeltaChange,
    ): Long = when (change.type) {
            "upsert" -> {
                if (change.tombstone != null) throw RemotePlannerMappingException()
                val incoming = mapCanonicalItem(change.item ?: throw RemotePlannerMappingException())
                val existing = items[incoming.id]
                if (existing != null && incoming.revision < existing.revision) {
                    throw RemotePlannerMappingException()
                }
                if (existing != null && incoming.revision == existing.revision && incoming != existing) {
                    throw RemotePlannerMappingException()
                }
                items[incoming.id] = incoming
                estimatedCanonicalItemBytes(incoming) -
                    (existing?.let(::estimatedCanonicalItemBytes) ?: 0L)
            }
            "tombstone" -> {
                if (change.item != null) throw RemotePlannerMappingException()
                val tombstone = change.tombstone ?: throw RemotePlannerMappingException()
                validateUuid(tombstone.id)
                tombstone.parentId?.let(::validateUuid)
                validateTimestamp(tombstone.deletedAt)
                if (tombstone.revision <= 0) throw RemotePlannerMappingException()
                val existing = items[tombstone.id]
                if (existing != null && tombstone.revision <= existing.revision) {
                    throw RemotePlannerMappingException()
                }
                items.remove(tombstone.id)
                -(existing?.let(::estimatedCanonicalItemBytes) ?: 0L)
            }
            else -> throw RemotePlannerMappingException()
        }

    private fun estimatedCanonicalItemBytes(item: CanonicalItemSnapshot): Long =
        CANONICAL_ITEM_OBJECT_OVERHEAD_BYTES + 2L * listOfNotNull(
            item.id,
            item.kind,
            item.status,
            item.title,
            item.notes,
            item.timezoneName,
            item.deadlineAt,
            item.earliestStartAt,
            item.recurrenceJson,
            item.flexibleConstraintsJson,
            item.splitPolicyJson,
            item.parentId,
            item.createdAt,
            item.updatedAt,
            item.completedAt,
            item.deletedAt,
        ).sumOf { it.length.toLong() }

    private fun previewRequest(
        instant: Instant,
        planningZone: ZoneId,
        canonicalItems: List<CanonicalItemSnapshot>,
        syncOrigin: String,
        configurationId: String?,
    ): SchedulePreviewRequest {
        val date = instant.atZone(planningZone).toLocalDate()
        val horizonStart = date.atStartOfDay(planningZone)
        val horizonEnd = date.plusDays(1).atStartOfDay(planningZone)
        val availableStart = localMinute(date, planningZone, dayStartMinute)
        val availableEnd = localMinute(date, planningZone, dayEndMinute)
        val revisions = canonicalItems.associate { it.id to it.revision }
        val cached = plannerStore.state.value
        val sameOrigin = cached.canonicalSyncOrigin == syncOrigin &&
            cached.canonicalConfigurationId == configurationId
        val relevantOutcomeStart = horizonStart.toInstant().minusSeconds(OUTCOME_CONTEXT_MARGIN_SECONDS)
        val relevantOutcomeEnd = horizonEnd.toInstant().plusSeconds(OUTCOME_CONTEXT_MARGIN_SECONDS)
        val recurrenceOutcomes = (if (sameOrigin) cached.recurrenceOutcomes else emptyMap()).entries
            .sortedBy { it.key }
            .filter { (_, outcome) -> outcome.itemId in revisions }
            .filter { (_, outcome) ->
                val resolved = parseTimestamp(outcome.resolvedAt).toInstant()
                resolved >= relevantOutcomeStart && resolved < relevantOutcomeEnd
            }
            .onEach { (occurrenceId, outcome) ->
                validateUuid(occurrenceId)
                validateUuid(outcome.itemId)
                if (
                    outcome.status !in setOf(ItemStatus.COMPLETED, ItemStatus.SKIPPED)
                ) {
                    throw RemotePlannerMappingException()
                }
            }
        val completionAnchors = (if (sameOrigin) {
            cached.recurrenceCompletionAnchors
        } else {
            emptyMap()
        }).entries
            .sortedBy { it.key }
            .filter { (itemId, _) -> itemId in revisions }
            .onEach { (itemId, timestamp) ->
                validateUuid(itemId)
                parseTimestamp(timestamp)
            }
        val recurrenceMoves = (if (sameOrigin) cached.recurrenceMoves else emptyMap()).entries
            .sortedBy { it.key }
            .filter { (_, move) -> move.itemId in revisions }
            .filter { (_, move) ->
                val movedAt = parseTimestamp(move.movedAt).toInstant()
                movedAt >= relevantOutcomeStart && movedAt < relevantOutcomeEnd
            }
            .onEach { (occurrenceId, move) ->
                validateUuid(occurrenceId)
                validateUuid(move.itemId)
                if (parseTimestamp(move.startAt) >= parseTimestamp(move.endAt)) {
                    throw RemotePlannerMappingException()
                }
            }
        if (
            recurrenceOutcomes.size + completionAnchors.size + recurrenceMoves.size >
            MAX_RECURRENCE_CONTEXT_IDS
        ) {
            throw RecurrenceContextCapacityException()
        }
        val completedOccurrenceIds = recurrenceOutcomes
            .filter { (_, outcome) -> outcome.status == ItemStatus.COMPLETED }
            .map { (occurrenceId, _) -> occurrenceId }
        val skippedOccurrences = recurrenceOutcomes
            .filter { (_, outcome) -> outcome.status == ItemStatus.SKIPPED }
        val generatedForPlanningDate = cached.scheduleGeneratedAt
            ?.let { runCatching { Instant.parse(it).atZone(planningZone).toLocalDate() }.getOrNull() }
            ?.let { it == date }
            ?: false
        val previousSchedule = if (
            generatedForPlanningDate && sameOrigin &&
            cached.schedulePlanningZoneId == planningZone.id
        ) {
            cached.schedule
        } else {
            emptyList()
        }
        val previous = previousSchedule
            .asSequence()
            .filter { block ->
                block.canonicalBlockKind in setOf("planned", "pinned") ||
                    (block.canonicalBlockKind == null && block.isFlexible)
            }
            .filter { block ->
                val itemId = block.canonicalItemId ?: return@filter false
                revisions[itemId] == block.canonicalRevision
            }
            .groupBy { it.canonicalItemId to it.occurrenceId }
            .map { (identity, blocks) ->
                val itemId = requireNotNull(identity.first)
                val exactBlocks = blocks.sortedWith { left, right ->
                    val leftInstant = left.timelineInstant()
                    val rightInstant = right.timelineInstant()
                    when {
                        leftInstant != null && rightInstant != null ->
                            leftInstant.compareTo(rightInstant)
                        else -> left.startMinute.compareTo(right.startMinute)
                    }
                }.mapNotNull { block ->
                    val start = block.absoluteStartAt ?: return@mapNotNull null
                    val end = block.absoluteEndAt ?: return@mapNotNull null
                    PreviousScheduleBlockRequest(
                        start = parseTimestamp(start).toInstant().toString(),
                        end = parseTimestamp(end).toInstant().toString(),
                        sessionIndex = block.sessionIndex,
                    )
                }
                PreviousScheduleAssignmentRequest(
                    itemId = itemId,
                    itemRevision = requireNotNull(revisions[itemId]),
                    occurrenceId = identity.second,
                    blocks = exactBlocks,
                    // `pinned` applies to the whole assignment. Never freeze a far-future split
                    // session merely because one sibling session is inside the horizon.
                    pinned = exactBlocks.isNotEmpty() && exactBlocks.all { exact ->
                        val start = parseTimestamp(exact.start).toInstant()
                        val end = parseTimestamp(exact.end).toInstant()
                        start >= instant && end <= instant.plusSeconds(FREEZE_HORIZON_SECONDS)
                    },
                )
            }
            .filter { it.blocks.isNotEmpty() }
            .sortedWith(compareBy({ it.itemId }, { it.occurrenceId.orEmpty() }))
        return SchedulePreviewRequest(
            asOf = instant.toString(),
            horizonStart = horizonStart.toInstant().toString(),
            horizonEnd = horizonEnd.toInstant().toString(),
            timezoneName = planningZone.id,
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = availableStart.toInstant().toString(),
                    end = availableEnd.toInstant().toString(),
                ),
            ),
            previousAssignments = previous,
            recurrenceContext = buildJsonObject {
                put(
                    "completed_occurrence_ids",
                    buildJsonArray {
                        completedOccurrenceIds.forEach { add(JsonPrimitive(it)) }
                    },
                )
                put(
                    "completion_anchors",
                    buildJsonObject {
                        completionAnchors.forEach { (itemId, timestamp) ->
                            put(itemId, timestamp)
                        }
                    },
                )
                put(
                    "exceptions",
                    buildJsonArray {
                        skippedOccurrences.forEach { (occurrenceId, outcome) ->
                            add(
                                buildJsonObject {
                                    put("item_id", outcome.itemId)
                                    put(
                                        "selector",
                                        buildJsonObject {
                                            put("type", "occurrence")
                                            put("id", occurrenceId)
                                        },
                                    )
                                    put(
                                        "action",
                                        buildJsonObject { put("type", "skip") },
                                    )
                                },
                            )
                        }
                        recurrenceMoves.forEach { (occurrenceId, move) ->
                            add(
                                buildJsonObject {
                                    put("item_id", move.itemId)
                                    put(
                                        "selector",
                                        buildJsonObject {
                                            put("type", "occurrence")
                                            put("id", occurrenceId)
                                        },
                                    )
                                    put(
                                        "action",
                                        buildJsonObject {
                                            put("type", "move")
                                            put("start", move.startAt)
                                            put("end", move.endAt)
                                        },
                                    )
                                },
                            )
                        }
                    },
                )
            },
        )
    }

    private fun mapPreview(
        preview: RemoteSchedulePreview,
        canonicalItems: List<CanonicalItemSnapshot>,
        syncOrigin: String,
        deltaCursor: String,
        generatedAt: Instant,
        planningDate: LocalDate,
        planningZone: ZoneId,
        availabilityStart: ZonedDateTime,
        availabilityEnd: ZonedDateTime,
    ): CanonicalPlanUpdate {
        val items = canonicalItems.associateBy(CanonicalItemSnapshot::id)
        if (
            preview.sourceItemCount !in 0..MAX_CANONICAL_ITEMS ||
            preview.sourceItemRevisions.size != preview.sourceItemCount ||
            preview.acceptedItemCount < 0 ||
            preview.acceptedItemCount + preview.rejectedItems.size != preview.sourceItemCount ||
            !preview.inputDigest.matches(DIGEST_PATTERN) ||
            preview.rejectedItems.size > MAX_CANONICAL_ITEMS ||
            preview.plan.blocks.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.unscheduled.size > MAX_CANONICAL_ITEMS ||
            preview.ignoredPreviousAssignments.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.decisions.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.violations.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.occurrences.size > MAX_SCHEDULE_BLOCKS
        ) {
            throw RemotePlannerMappingException()
        }
        preview.sourceItemRevisions.forEach { (id, revision) ->
            validateUuid(id)
            if (revision <= 0) throw RemotePlannerMappingException()
        }
        val expectedRevisions = items.mapValues { (_, item) -> item.revision }
        if (preview.sourceItemRevisions != expectedRevisions) {
            throw RemoteSnapshotChangedException()
        }
        val rejectedIds = preview.rejectedItems.map { rejected ->
            validateUuid(rejected.itemId)
            val item = items[rejected.itemId]
            if (
                rejected.title.isBlank() || rejected.reason.isBlank() || rejected.itemId !in items ||
                item?.title != rejected.title ||
                rejected.isSensitive != effectiveSensitivity(item, items)
            ) {
                throw RemotePlannerMappingException()
            }
            rejected.itemId
        }
        if (rejectedIds.distinct().size != rejectedIds.size) throw RemotePlannerMappingException()
        val expectedHorizonStart = planningDate.atStartOfDay(planningZone).toInstant()
        val expectedHorizonEnd = planningDate.plusDays(1).atStartOfDay(planningZone).toInstant()
        if (
            parseTimestamp(preview.plan.asOf).toInstant() != generatedAt ||
            parseTimestamp(preview.plan.horizonStart).toInstant() != expectedHorizonStart ||
            parseTimestamp(preview.plan.horizonEnd).toInstant() != expectedHorizonEnd
        ) {
            throw RemotePlannerMappingException()
        }

        val occurrencesById = preview.plan.occurrences.associateBy { occurrence ->
            validateUuid(occurrence.id)
            validateUuid(occurrence.seriesItemId)
            if (
                occurrence.seriesItemId !in items || occurrence.ordinal !in 0..UInt.MAX_VALUE.toLong() ||
                occurrence.state !in SUPPORTED_OCCURRENCE_STATES
            ) {
                throw RemotePlannerMappingException()
            }
            val nominalStart = parseTimestamp(occurrence.nominalStart)
            val nominalEnd = parseTimestamp(occurrence.nominalEnd)
            val windowStart = parseTimestamp(occurrence.windowStart)
            val windowEnd = parseTimestamp(occurrence.windowEnd)
            if (nominalStart >= nominalEnd || windowStart >= windowEnd) {
                throw RemotePlannerMappingException()
            }
            occurrence.localDate?.let { raw ->
                runCatching { LocalDate.parse(raw) }.getOrElse {
                    throw RemotePlannerMappingException(it)
                }
            }
            occurrence.id
        }
        if (occurrencesById.size != preview.plan.occurrences.size) {
            throw RemotePlannerMappingException()
        }
        preview.plan.unscheduled.forEach { work ->
            validateUuid(work.itemId)
            work.occurrenceId?.let(::validateUuid)
            if (
                work.itemId !in items || work.remaining !in 0..MAX_PLAN_MINUTES ||
                work.reason !in SUPPORTED_UNSCHEDULED_REASONS ||
                work.message.isBlank() || work.message.length > MAX_REMOTE_MESSAGE_CHARS ||
                work.occurrenceId?.let { occurrenceId ->
                    occurrencesById[occurrenceId]?.let { occurrence ->
                        !itemBelongsToSeries(work.itemId, occurrence.seriesItemId, items)
                    } != false
                } == true
            ) {
                throw RemotePlannerMappingException()
            }
        }
        if (
            preview.plan.unscheduled.map { it.itemId to it.occurrenceId }.distinct().size !=
            preview.plan.unscheduled.size
        ) {
            throw RemotePlannerMappingException()
        }
        preview.plan.decisions.forEach { decision ->
            validateUuid(decision.itemId)
            decision.occurrenceId?.let(::validateUuid)
            if (
                decision.itemId !in items || decision.kind !in SUPPORTED_DECISION_KINDS ||
                decision.message.isBlank() || decision.message.length > MAX_REMOTE_MESSAGE_CHARS ||
                decision.occurrenceId?.let { occurrenceId ->
                    occurrencesById[occurrenceId]?.let { occurrence ->
                        !itemBelongsToSeries(decision.itemId, occurrence.seriesItemId, items)
                    } != false
                } == true
            ) {
                throw RemotePlannerMappingException()
            }
        }
        preview.ignoredPreviousAssignments.forEach { ignored ->
            validateUuid(ignored.itemId)
            if (
                ignored.itemId !in items || ignored.requestedRevision <= 0 ||
                ignored.currentRevision?.let { it <= 0 } == true || ignored.reason.isBlank() ||
                ignored.reason.length > MAX_REMOTE_MESSAGE_CHARS
            ) {
                throw RemotePlannerMappingException()
            }
        }

        val schedule = preview.plan.blocks.map { block ->
            mapScheduleBlock(block, items, planningDate, planningZone)
        }.let { preserveLocalSessionState(it, items, syncOrigin) }
        if (schedule.sumOf(::estimatedScheduleItemBytes) > MAX_SCHEDULE_CACHE_ESTIMATED_BYTES) {
            throw RemotePlannerMappingException()
        }
        if (schedule.map(ScheduleItem::id).distinct().size != schedule.size) {
            throw RemotePlannerMappingException()
        }
        if (
            schedule.map { Triple(it.canonicalItemId, it.occurrenceId, it.sessionIndex) }
                .distinct().size != schedule.size ||
            schedule.count { it.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED) } > 1
        ) {
            throw RemotePlannerMappingException()
        }
        if (schedule.any { it.canonicalItemId in rejectedIds }) {
            throw RemotePlannerMappingException()
        }
        schedule.forEach { block ->
            block.occurrenceId?.let { occurrenceId ->
                val occurrence = occurrencesById[occurrenceId]
                val itemId = block.canonicalItemId
                if (
                    occurrence == null || itemId == null ||
                    !itemBelongsToSeries(itemId, occurrence.seriesItemId, items)
                ) {
                    throw RemotePlannerMappingException()
                }
            }
        }
        val violationMessages = preview.plan.violations.map { violation ->
            if (
                violation.kind !in SUPPORTED_VIOLATION_KINDS ||
                violation.severity !in SUPPORTED_VIOLATION_SEVERITIES ||
                violation.itemIds.size > MAX_CANONICAL_ITEMS ||
                violation.occurrenceIds.size > MAX_SCHEDULE_BLOCKS ||
                violation.penalty < 0 || violation.message.isBlank() ||
                violation.message.length > MAX_VIOLATION_MESSAGE_CHARS
            ) {
                throw RemotePlannerMappingException()
            }
            violation.itemIds.forEach { id ->
                validateUuid(id)
                if (id !in items) throw RemotePlannerMappingException()
            }
            violation.occurrenceIds.forEach(::validateUuid)
            val violationStart = violation.start?.let(::parseTimestamp)
            val violationEnd = violation.end?.let(::parseTimestamp)
            if (violationStart != null && violationEnd != null && violationStart >= violationEnd) {
                throw RemotePlannerMappingException()
            }
            violation.message
        }
        val score = preview.plan.score
        if (
            score.scheduledMinutes < 0 || score.unscheduledMinutes < 0 ||
            score.softPenalty < 0 || score.movedMinutes < 0 ||
            score.scheduledMinutes > MAX_PLAN_MINUTES ||
            score.unscheduledMinutes > MAX_PLAN_MINUTES ||
            score.movedMinutes > MAX_PLAN_MINUTES
        ) {
            throw RemotePlannerMappingException()
        }
        val totalWork = score.scheduledMinutes + score.unscheduledMinutes
        val completionScore = if (totalWork == 0L) {
            100
        } else {
            ((score.scheduledMinutes * 100L) / totalWork).toInt()
        }
        val penalty = (score.softPenalty / 100L).coerceAtMost(MAX_SCORE_PENALTY.toLong()).toInt()
        val dayScore = (completionScore - penalty).coerceIn(0, 100)
        val protectedMinutes = protectedMinutes(
            schedule,
            planningDate,
            planningZone,
            availabilityStart,
            availabilityEnd,
        )
        val message = when {
            preview.rejectedItems.isNotEmpty() ->
                "Composed with ${preview.rejectedItems.size} canonical item(s) needing repair"
            preview.plan.unscheduled.isNotEmpty() ->
                "Composed · ${preview.plan.unscheduled.size} item(s) still need capacity"
            schedule.isEmpty() && canonicalItems.isEmpty() ->
                "Canonical workspace is empty · capture and review your first item"
            else -> "Composed ${schedule.size} block(s) from ${canonicalItems.size} canonical item(s)"
        }
        val unscheduledWork = preview.plan.unscheduled.map { work ->
            UnscheduledWorkSnapshot(
                itemId = work.itemId,
                occurrenceId = work.occurrenceId,
                remainingMinutes = work.remaining,
                reason = work.reason,
            )
        }
        return CanonicalPlanUpdate(
            items = canonicalItems,
            schedule = schedule,
            syncOrigin = syncOrigin,
            deltaCursor = deltaCursor,
            inputDigest = preview.inputDigest,
            generatedAt = generatedAt.toString(),
            planningZoneId = planningZone.id,
            rejectedItemCount = preview.rejectedItems.size,
            unscheduledItemCount = preview.plan.unscheduled.size,
            protectedFreeMinutes = protectedMinutes,
            dayScore = dayScore,
            violationMessages = violationMessages.take(MAX_PERSISTED_VIOLATION_MESSAGES),
            violationCount = violationMessages.size,
            errorViolationCount = preview.plan.violations.count { it.severity == "error" },
            unscheduledWork = unscheduledWork,
            occurrenceSeriesItemIds = preview.plan.occurrences.associate {
                it.id to it.seriesItemId
            },
            message = message,
        )
    }

    private fun mapScheduleBlock(
        block: RemoteScheduleBlock,
        items: Map<String, CanonicalItemSnapshot>,
        planningDate: LocalDate,
        planningZone: ZoneId,
    ): ScheduleItem {
        validateUuid(block.id)
        block.occurrenceId?.let(::validateUuid)
        block.externalBlockId?.let(::validateUuid)
        if (block.title.isBlank() || block.sessionIndex !in 0..UShort.MAX_VALUE.toInt()) {
            throw RemotePlannerMappingException()
        }
        if (block.explanations.size > MAX_BLOCK_EXPLANATIONS) {
            throw RemotePlannerMappingException()
        }
        block.explanations.forEach { explanation ->
            if (
                explanation.code !in SUPPORTED_EXPLANATION_CODES || explanation.message.isBlank() ||
                explanation.message.length > MAX_REMOTE_MESSAGE_CHARS
            ) {
                throw RemotePlannerMappingException()
            }
        }
        if (
            block.kind !in SUPPORTED_BLOCK_KINDS || block.kind == "external_fixed" ||
            block.externalBlockId != null
        ) {
            // Android currently sends no fixed blocks, so accepting one would make the response
            // describe input this client never authorized.
            throw RemotePlannerMappingException()
        }
        val actualStart = parseTimestamp(block.start).atZoneSameInstant(planningZone)
        val actualEnd = parseTimestamp(block.end).atZoneSameInstant(planningZone)
        val horizonStart = planningDate.atStartOfDay(planningZone)
        val horizonEnd = planningDate.plusDays(1).atStartOfDay(planningZone)
        if (actualEnd <= horizonStart || actualStart >= horizonEnd) {
            throw RemotePlannerMappingException()
        }
        val start = if (actualStart < horizonStart) horizonStart else actualStart
        val end = if (actualEnd > horizonEnd) horizonEnd else actualEnd
        val durationSeconds = Duration.between(start.toInstant(), end.toInstant()).seconds
        if (
            durationSeconds <= 0 || start.toLocalDate() != planningDate || end > horizonEnd
        ) {
            throw RemotePlannerMappingException()
        }
        val durationMinutes = ceil(durationSeconds / 60.0).toInt()
        if (durationMinutes !in 1..MAX_BLOCK_MINUTES) throw RemotePlannerMappingException()
        val canonical = block.itemId?.let { itemId ->
            validateUuid(itemId)
            items[itemId] ?: throw RemotePlannerMappingException()
        } ?: throw RemotePlannerMappingException()
        if (
            !canonical.isExecutable || block.title != canonical.title ||
            block.isSensitive != effectiveSensitivity(canonical, items)
        ) {
            throw RemotePlannerMappingException()
        }
        val itemKind = mapItemKind(canonical.kind)
        val status = mapItemStatus(canonical.status)
        val splitType = canonical.splitPolicyJson
            .let(JsonObjectParser::parse)
            .get("type")
            ?.let { it as? JsonPrimitive }
            ?.contentOrNull
        val constraints = canonical.flexibleConstraintsJson.let(JsonObjectParser::parse)
        val energyValue = constraints?.get("energy")?.let(::energyValue)
        val hard = block.kind in setOf("pinned", "calendar_event", "external_fixed")
        return ScheduleItem(
            id = block.id,
            isSensitive = block.isSensitive,
            title = block.title,
            kind = itemKind,
            startMinute = start.hour * 60 + start.minute,
            durationMinutes = durationMinutes,
            status = status,
            project = canonical.parentId?.let { items[it]?.title },
            energy = when (energyValue) {
                "low" -> EnergyLevel.LOW
                "deep" -> EnergyLevel.DEEP
                else -> EnergyLevel.MEDIUM
            },
            isFlexible = !hard,
            isHardConstraint = hard,
            isSplittable = splitType == "splittable",
            // Notes remain available once in canonicalItems. Copying a large note into every
            // split block amplifies an otherwise bounded preview during encrypted serialization.
            note = "",
            canonicalItemId = canonical.id,
            occurrenceId = block.occurrenceId,
            canonicalRevision = canonical.revision,
            sessionIndex = block.sessionIndex,
            absoluteStartAt = start.toInstant().toString(),
            absoluteEndAt = end.toInstant().toString(),
            planningZoneId = planningZone.id,
            canonicalBlockKind = block.kind,
        )
    }

    /** Mirrors the server's cycle-safe ancestor propagation and fails closed on partial trees. */
    private fun effectiveSensitivity(
        item: CanonicalItemSnapshot?,
        items: Map<String, CanonicalItemSnapshot>,
    ): Boolean {
        var current = item ?: return true
        val visited = mutableSetOf<String>()
        while (true) {
            if (!visited.add(current.id)) return true
            if (current.isSensitive) return true
            val parentId = current.parentId ?: return false
            current = items[parentId] ?: return true
        }
    }

    private fun preserveLocalSessionState(
        composed: List<ScheduleItem>,
        items: Map<String, CanonicalItemSnapshot>,
        syncOrigin: String,
    ): List<ScheduleItem> {
        val cached = plannerStore.state.value
        if (cached.canonicalSyncOrigin != syncOrigin) return composed
        val previous = cached.schedule
        val composedCounts = composed.groupingBy { it.canonicalItemId to it.occurrenceId }
            .eachCount()
        return composed.map { fresh ->
            if (fresh.occurrenceId in cached.recurrenceMoves) return@map fresh
            val old = previous.firstOrNull {
                it.canonicalItemId == fresh.canonicalItemId &&
                    it.occurrenceId == fresh.occurrenceId &&
                    it.sessionIndex == fresh.sessionIndex &&
                    it.canonicalRevision == fresh.canonicalRevision
            } ?: return@map fresh
            val item = fresh.canonicalItemId?.let(items::get) ?: return@map fresh
            val isSplit = parseJsonObject(item.splitPolicyJson)["type"]
                ?.let { it as? JsonPrimitive }
                ?.contentOrNull == "splittable"
            val isSessionLocal = fresh.occurrenceId != null || isSplit ||
                composedCounts[fresh.canonicalItemId to fresh.occurrenceId].orZero() > 1
            if (
                isSessionLocal && old.status in setOf(
                    ItemStatus.ACTIVE,
                    ItemStatus.PAUSED,
                    ItemStatus.COMPLETED,
                    ItemStatus.SKIPPED,
                )
            ) {
                fresh.copy(status = old.status, actualMinutes = old.actualMinutes)
            } else {
                fresh
            }
        }
    }

    private fun estimatedScheduleItemBytes(item: ScheduleItem): Long =
        SCHEDULE_ITEM_OBJECT_OVERHEAD_BYTES + 2L * listOfNotNull(
            item.id,
            item.title,
            item.project,
            item.note,
            item.canonicalItemId,
            item.occurrenceId,
            item.absoluteStartAt,
            item.absoluteEndAt,
            item.planningZoneId,
            item.canonicalBlockKind,
        ).sumOf(String::length)

    private fun Int?.orZero(): Int = this ?: 0

    private fun itemBelongsToSeries(
        itemId: String,
        seriesItemId: String,
        items: Map<String, CanonicalItemSnapshot>,
    ): Boolean {
        var current: String? = itemId
        while (current != null) {
            if (current == seriesItemId) return true
            current = items[current]?.parentId
        }
        return false
    }

    private fun protectedMinutes(
        schedule: List<ScheduleItem>,
        planningDate: LocalDate,
        planningZone: ZoneId,
        availabilityStart: ZonedDateTime,
        availabilityEnd: ZonedDateTime,
    ): Int {
        val ranges = schedule.mapNotNull { block ->
            val exactStart = block.absoluteStartAt ?: return@mapNotNull null
            val exactEnd = block.absoluteEndAt ?: return@mapNotNull null
            val start = parseTimestamp(exactStart).toInstant()
                .coerceAtLeast(availabilityStart.toInstant())
            val end = parseTimestamp(exactEnd).toInstant()
                .coerceAtMost(availabilityEnd.toInstant())
            if (end > start) start to end else null
        }.sortedBy { it.first }
        var occupied = 0L
        var activeStart: Instant? = null
        var activeEnd: Instant? = null
        for ((start, end) in ranges) {
            if (activeStart == null) {
                activeStart = start
                activeEnd = end
            } else if (start <= requireNotNull(activeEnd)) {
                if (end > requireNotNull(activeEnd)) activeEnd = end
            } else {
                occupied += Duration.between(
                    requireNotNull(activeStart),
                    requireNotNull(activeEnd),
                ).toMinutes()
                activeStart = start
                activeEnd = end
            }
        }
        if (activeStart != null) {
            occupied += Duration.between(activeStart, requireNotNull(activeEnd)).toMinutes()
        }
        val available = Duration.between(
            availabilityStart.toInstant(),
            availabilityEnd.toInstant(),
        ).toMinutes()
        return (available - occupied).coerceAtLeast(0).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
    }

    private fun authenticatedConfiguration(): ConfigurationResolution {
        val snapshot = credentialStore.snapshot()
        if (snapshot.baseUrl == null) {
            mutableState.value = stateFrom(snapshot)
            return ConfigurationResolution.Failed(CanonicalRefreshOutcome.NOT_CONFIGURED)
        }
        if (!snapshot.hasBearerToken) {
            mutableState.value = stateFrom(snapshot)
            return ConfigurationResolution.Failed(CanonicalRefreshOutcome.AUTH_REQUIRED)
        }
        return try {
            credentialStore.authenticatedConfiguration()?.let(ConfigurationResolution::Ready)
                ?: ConfigurationResolution.Failed(CanonicalRefreshOutcome.AUTH_REQUIRED)
        } catch (_: SecureCredentialException) {
            mutableState.value = CanonicalSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "The encrypted bearer token is unavailable. Re-enter it to compose a plan.",
            )
            ConfigurationResolution.Failed(CanonicalRefreshOutcome.AUTH_REQUIRED)
        } catch (_: InvalidApiConfigurationException) {
            updateError("The stored API URL is invalid. Update the connection settings.")
            ConfigurationResolution.Failed(CanonicalRefreshOutcome.CONFIGURATION_ERROR)
        } catch (_: IllegalStateException) {
            updateError("Secure API credentials are unavailable on this device.")
            ConfigurationResolution.Failed(CanonicalRefreshOutcome.CONFIGURATION_ERROR)
        }
    }

    private fun ensureConfigurationCurrent(configuration: AuthenticatedApiConfiguration) {
        val expected = configuration.configurationId ?: return
        val current = credentialStore.snapshot()
        if (
            current.configurationId != expected ||
            current.baseUrl != configuration.baseUrl.toString()
        ) {
            throw CanonicalConfigurationChangedException()
        }
    }

    /** Rejects or quarantines every cache whose opaque credential binding is unknown. */
    private suspend fun ensureDurableWorkspaceBinding(
        configuration: AuthenticatedApiConfiguration,
    ) {
        val current = plannerStore.state.value
        val origin = configuration.baseUrl.toString()
        val configurationId = configuration.configurationId
        val hasCanonicalState = current.canonicalSyncOrigin != null ||
            current.canonicalDeltaCursor != null || current.canonicalItems.isNotEmpty() ||
            current.pendingCanonicalMutation != null
        val hasExecutionState = current.canonicalExecutionSyncOrigin != null ||
            current.canonicalExecutionSession != null ||
            current.canonicalExecutionHistoryWindow.isNotEmpty() ||
            current.canonicalExecutionHistoryWindowRevision != null ||
            current.canonicalExecutionHistoryContinuityEstablished ||
            current.canonicalExecutionHistoryVerified ||
            current.terminalExecutionOutcomes.isNotEmpty() ||
            current.pendingExecutionCommand != null
        val canonicalMismatch = hasCanonicalState &&
            (current.canonicalSyncOrigin != origin ||
                current.canonicalConfigurationId != configurationId)
        val executionMismatch = hasExecutionState &&
            (current.canonicalExecutionSyncOrigin != origin ||
                current.canonicalExecutionConfigurationId != configurationId)
        if (!canonicalMismatch && !executionMismatch) return
        if (plannerStore.hasCredentialReplacementBlocker()) {
            throw CanonicalConfigurationChangedException()
        }
        val receipt = plannerStore.abandonCanonicalConnection()
        if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
    }

    private fun parseJsonObject(raw: String): JsonObject = JsonObjectParser.parse(raw)

    private fun handleFailure(error: Throwable): CanonicalRefreshOutcome {
        if (error is CancellationException) {
            mutableState.value = stateFrom(credentialStore.snapshot())
            throw error
        }
        val (phase, message, outcome) = when (error) {
            is PlannerApiException.Authentication -> Triple(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Authentication failed. Check or replace the stored bearer token.",
                CanonicalRefreshOutcome.AUTH_REQUIRED,
            )
            is PlannerApiException.Conflict -> Triple(
                CanonicalSyncPhase.ERROR,
                "This item changed on another client. Recompose to load its latest revision.",
                CanonicalRefreshOutcome.STALE_REVISION,
            )
            is PlannerApiException.Validation -> Triple(
                CanonicalSyncPhase.ERROR,
                "The server rejected the canonical plan input (HTTP ${error.statusCode}).",
                CanonicalRefreshOutcome.PERMANENT_SERVER_FAILURE,
            )
            is PlannerApiException.InvalidResponse,
            is RemotePlannerMappingException,
            is RemoteSnapshotChangedException,
            -> Triple(
                CanonicalSyncPhase.ERROR,
                "The server planner contract is incompatible with this DayWeave build.",
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            )
            is InvalidCanonicalTransitionException -> Triple(
                CanonicalSyncPhase.ERROR,
                "This action no longer matches the cached item state. Recompose and try again.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is CanonicalMutationNeedsReconciliationException -> Triple(
                CanonicalSyncPhase.ERROR,
                "A previous item action needs authoritative reconciliation before another action.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is CanonicalConfigurationChangedException -> Triple(
                CanonicalSyncPhase.READY,
                "API connection changed; the previous response was discarded.",
                CanonicalRefreshOutcome.CONFIGURATION_ERROR,
            )
            is RecurrenceContextCapacityException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Too many recurrence outcomes are active for one preview. Review or archive old outcomes.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is LocalPlannerStorageException -> Triple(
                CanonicalSyncPhase.ERROR,
                "The server accepted the action, but encrypted local storage failed. Recompose before continuing.",
                CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
            )
            is PlannerApiException.Http -> if (
                error.statusCode == 408 || error.statusCode == 429 || error.statusCode in 500..599
            ) {
                Triple(
                    CanonicalSyncPhase.OFFLINE,
                    "The planner API is temporarily unavailable. Showing the encrypted cached plan.",
                    CanonicalRefreshOutcome.RETRYABLE_SERVER_FAILURE,
                )
            } else {
                Triple(
                    CanonicalSyncPhase.ERROR,
                    "The planner API returned HTTP ${error.statusCode}.",
                    CanonicalRefreshOutcome.PERMANENT_SERVER_FAILURE,
                )
            }
            is IOException -> Triple(
                CanonicalSyncPhase.OFFLINE,
                "Offline or unable to reach the API. Showing the encrypted cached plan.",
                CanonicalRefreshOutcome.TRANSIENT_NETWORK_FAILURE,
            )
            else -> Triple(
                CanonicalSyncPhase.ERROR,
                "Today could not be recomposed; the encrypted cached plan was kept.",
                CanonicalRefreshOutcome.UNEXPECTED_FAILURE,
            )
        }
        val cached = plannerStore.state.value
        mutableState.value = CanonicalSyncState(
            phase = phase,
            message = message,
            lastInputDigest = cached.scheduleInputDigest,
            sourceItemCount = cached.canonicalItems.size,
            scheduledBlockCount = cached.schedule.size,
        )
        return outcome
    }

    private fun updateError(message: String) {
        val cached = plannerStore.state.value
        mutableState.value = CanonicalSyncState(
            phase = CanonicalSyncPhase.ERROR,
            message = message,
            lastInputDigest = cached.scheduleInputDigest,
            sourceItemCount = cached.canonicalItems.size,
            scheduledBlockCount = cached.schedule.size,
        )
    }

    private sealed interface ConfigurationResolution {
        data class Ready(val configuration: AuthenticatedApiConfiguration) : ConfigurationResolution

        data class Failed(val outcome: CanonicalRefreshOutcome) : ConfigurationResolution
    }

    private data class CanonicalDeltaSnapshot(
        val items: List<CanonicalItemSnapshot>,
        val cursor: String,
    )

    private data class ParentTerminalResolution(
        val wireStatus: String,
        val displayStatus: ItemStatus,
    )

    private enum class PendingMutationResolution {
        NONE,
        APPLIED,
        SUPERSEDED,
    }

    private enum class TerminalProjectionResult {
        NONE,
        APPLIED_WRITE,
        NEEDS_RELOAD,
        RESOLVED_WITHOUT_WRITE,
        CONFLICT,
    }

    private enum class TerminalProjectionRejection {
        CONFLICT,
        NOT_FOUND,
        DETERMINISTIC_REJECTION,
    }

    private class RemotePlannerMappingException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid remote planner contract", cause)

    private class RemoteSnapshotChangedException :
        IllegalArgumentException("Canonical snapshot changed during composition")

    private class InvalidCanonicalTransitionException :
        IllegalStateException("Invalid canonical transition")

    private class LocalPlannerStorageException :
        IllegalStateException("Canonical mutation was not durably persisted")

    private class CanonicalConfigurationChangedException :
        IllegalStateException("Canonical API configuration changed")

    private class CanonicalMutationNeedsReconciliationException :
        IllegalStateException("Canonical mutation needs reconciliation")

    private class RecurrenceContextCapacityException :
        IllegalStateException("Recurrence preview context capacity exceeded")

    private object JsonObjectParser {
        private val json = Json

        fun parse(raw: String): JsonObject = try {
            json.parseToJsonElement(raw) as? JsonObject ?: throw RemotePlannerMappingException()
        } catch (error: IllegalArgumentException) {
            throw RemotePlannerMappingException(error)
        }
    }

    companion object {
        private val TERMINAL_DISPLAY_STATUSES = setOf(ItemStatus.COMPLETED, ItemStatus.SKIPPED)
        private const val MINUTES_PER_DAY = 24 * 60
        private const val DEFAULT_DAY_START_MINUTE = 7 * 60
        private const val DEFAULT_DAY_END_MINUTE = 22 * 60
        private const val MAX_BLOCK_MINUTES = 2 * MINUTES_PER_DAY
        private const val MAX_CANONICAL_ITEMS = 10_000
        private const val MAX_CANONICAL_CACHE_ESTIMATED_BYTES = 24L * 1024L * 1024L
        private const val CANONICAL_ITEM_OBJECT_OVERHEAD_BYTES = 512L
        private const val MAX_DELTA_PAGES = 512
        private const val MAX_SNAPSHOT_ATTEMPTS = 3
        private const val MAX_DELTA_PAGE_SIZE = 50
        private const val MAX_DELTA_CHANGES = MAX_DELTA_PAGES * MAX_DELTA_PAGE_SIZE
        private const val MAX_SCHEDULE_BLOCKS = 2_000
        private const val MAX_SCHEDULE_CACHE_ESTIMATED_BYTES = 8L * 1024L * 1024L
        private const val SCHEDULE_ITEM_OBJECT_OVERHEAD_BYTES = 512L
        private const val MAX_PENDING_MUTATION_JSON_CHARS = 2 * 1024 * 1024
        private const val MAX_RECURRENCE_CONTEXT_IDS = 9_000
        private const val OUTCOME_CONTEXT_MARGIN_SECONDS = 2L * 24L * 60L * 60L
        private const val MAX_PLAN_MINUTES = 90L * MINUTES_PER_DAY
        private const val MAX_SCORE_PENALTY = 20
        private const val MAX_CURSOR_CHARS = 4_096
        private const val MAX_PAUSE_MINUTES = 24 * 60
        private const val DO_LATER_SECONDS = 60L * 60L
        private const val FREEZE_HORIZON_SECONDS = 2L * 60L * 60L
        private const val MAX_TITLE_CHARS = 500
        private const val MAX_NOTES_CHARS = 100_000
        private const val MAX_RECURRENCE_BYTES = 16 * 1024
        private const val MAX_CONSTRAINT_BYTES = 32 * 1024
        private const val MAX_VIOLATION_MESSAGE_CHARS = 2_000
        private const val MAX_REMOTE_MESSAGE_CHARS = 4_000
        private const val MAX_BLOCK_EXPLANATIONS = 64
        private const val MAX_PERSISTED_VIOLATION_MESSAGES = 100
        private val DIGEST_PATTERN = Regex("^sha256:[0-9a-f]{64}$")
        private val SUPPORTED_BLOCK_KINDS = setOf(
            "planned",
            "pinned",
            "calendar_event",
            "external_fixed",
        )
        private val SUPPORTED_EXPLANATION_CODES = setOf(
            "fixed_event",
            "pinned",
            "hard_deadline",
            "goal_progress",
            "habit_or_routine",
            "priority",
            "preferred_window",
            "context_match",
            "energy_match",
            "dependency",
            "stable_time",
            "earliest_available",
            "split_session",
        )
        private val SUPPORTED_UNSCHEDULED_REASONS = setOf(
            "missing_duration",
            "no_capacity",
            "hard_constraint",
            "blocked",
            "dependency_unavailable",
            "dependency_cycle",
            "session_limit",
        )
        private val SUPPORTED_DECISION_KINDS = setOf(
            "container_rolled_up",
            "terminal_item_ignored",
            "fixed_event_retained",
            "scheduled",
            "partially_scheduled",
            "kept_pinned",
        )
        private val SUPPORTED_OCCURRENCE_STATES = setOf(
            "generated",
            "completed",
            "paused",
            "skipped",
        )
        private val SUPPORTED_VIOLATION_KINDS = setOf(
            "soft_constraint",
            "fixed_overlap",
            "pinned_conflict",
            "deadline_risk",
            "dependency",
            "buffer_compressed",
            "capacity",
        )
        private val SUPPORTED_VIOLATION_SEVERITIES = setOf("warning", "error")

        private fun stateFrom(snapshot: ApiConnectionSnapshot): CanonicalSyncState = when {
            snapshot.baseUrl == null -> CanonicalSyncState(
                CanonicalSyncPhase.NOT_CONFIGURED,
                "Add an HTTPS DayWeave API URL and bearer token to compose your canonical plan.",
            )
            !snapshot.hasBearerToken -> CanonicalSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Add a bearer token to sync canonical items and compose Today.",
            )
            else -> CanonicalSyncState(
                CanonicalSyncPhase.READY,
                "Ready to sync canonical items and compose Today.",
            )
        }

        private fun mapCanonicalItem(remote: RemoteCanonicalItem): CanonicalItemSnapshot {
            validateUuid(remote.id)
            remote.parentId?.let(::validateUuid)
            val recurrenceJson = remote.recurrence?.toString()
            val constraintsJson = remote.flexibleConstraints.toString()
            val splitPolicyJson = remote.splitPolicy.toString()
            if (
                remote.kind !in SUPPORTED_ITEM_KINDS || remote.status !in SUPPORTED_ITEM_STATUSES ||
                remote.title.isBlank() || remote.title != remote.title.trim() ||
                remote.title.codePointCount(0, remote.title.length) > MAX_TITLE_CHARS ||
                remote.notes?.let {
                    it.codePointCount(0, it.length) > MAX_NOTES_CHARS
                } == true ||
                remote.recurrence?.let { it !is JsonObject } == true ||
                recurrenceJson?.toByteArray(Charsets.UTF_8)?.size?.let {
                    it > MAX_RECURRENCE_BYTES
                } == true ||
                constraintsJson.toByteArray(Charsets.UTF_8).size > MAX_CONSTRAINT_BYTES ||
                splitPolicyJson.length > 1_024 ||
                remote.revision <= 0 || remote.deletedAt != null ||
                remote.importance !in 0..100 || remote.urgency !in 0..100 ||
                remote.siblingOrder !in 0..1_000_000 ||
                remote.durationSeconds?.let { it <= 0 || it > 366L * 24L * 60L * 60L } == true
            ) {
                throw RemotePlannerMappingException()
            }
            try {
                ZoneId.of(remote.timezoneName)
            } catch (error: DateTimeException) {
                throw RemotePlannerMappingException(error)
            }
            val deadline = remote.deadlineAt?.let(::parseTimestamp)
            val earliest = remote.earliestStartAt?.let(::parseTimestamp)
            val created = parseTimestamp(remote.createdAt)
            val updated = parseTimestamp(remote.updatedAt)
            remote.completedAt?.let(::parseTimestamp)
            if (
                earliest != null && deadline != null && earliest >= deadline ||
                created > updated
            ) {
                throw RemotePlannerMappingException()
            }
            val splitType = remote.splitPolicy["type"]
                ?.let { it as? JsonPrimitive }
                ?.contentOrNull
                ?: throw RemotePlannerMappingException()
            when (splitType) {
                "indivisible" -> Unit
                "splittable" -> {
                    val minimum = remote.splitPolicy["minimum_chunk_seconds"]
                        ?.let { it as? JsonPrimitive }
                        ?.intOrNull
                    val maximum = remote.splitPolicy["maximum_chunk_seconds"]
                        ?.let { it as? JsonPrimitive }
                        ?.intOrNull
                    if (
                        minimum == null || maximum == null || minimum <= 0 || maximum < minimum ||
                        remote.durationSeconds == null || maximum > remote.durationSeconds
                    ) {
                        throw RemotePlannerMappingException()
                    }
                }
                else -> throw RemotePlannerMappingException()
            }
            return CanonicalItemSnapshot(
                id = remote.id,
                isSensitive = remote.isSensitive,
                kind = remote.kind,
                status = remote.status,
                title = remote.title,
                notes = remote.notes,
                timezoneName = remote.timezoneName,
                durationSeconds = remote.durationSeconds,
                deadlineAt = remote.deadlineAt,
                earliestStartAt = remote.earliestStartAt,
                recurrenceJson = recurrenceJson,
                flexibleConstraintsJson = constraintsJson,
                splitPolicyJson = splitPolicyJson,
                importance = remote.importance,
                urgency = remote.urgency,
                parentId = remote.parentId,
                siblingOrder = remote.siblingOrder,
                isExecutable = remote.isExecutable,
                revision = remote.revision,
                createdAt = remote.createdAt,
                updatedAt = remote.updatedAt,
                completedAt = remote.completedAt,
                deletedAt = remote.deletedAt,
            )
        }

        private fun validateHierarchy(items: Map<String, CanonicalItemSnapshot>) {
            val parentIds = items.values.mapNotNull(CanonicalItemSnapshot::parentId).toSet()
            if (parentIds.any { items[it]?.isExecutable != false }) {
                throw RemotePlannerMappingException()
            }
            items.values.forEach { item ->
                val parent = item.parentId ?: return@forEach
                if (parent == item.id || parent !in items) throw RemotePlannerMappingException()
                val visited = mutableSetOf(item.id)
                var cursor: String? = parent
                while (cursor != null) {
                    if (!visited.add(cursor)) throw RemotePlannerMappingException()
                    cursor = items[cursor]?.parentId
                }
            }
        }

        private fun mapItemKind(kind: String): ItemKind = when (kind) {
            "event" -> ItemKind.EVENT
            "task" -> ItemKind.TASK
            "habit" -> ItemKind.HABIT
            "routine" -> ItemKind.ROUTINE
            "goal" -> ItemKind.GOAL
            "break" -> ItemKind.BREAK
            else -> throw RemotePlannerMappingException()
        }

        private fun mapItemStatus(status: String): ItemStatus = when (status) {
            "inbox", "planned" -> ItemStatus.SCHEDULED
            "scheduled" -> ItemStatus.SCHEDULED
            "in_progress" -> ItemStatus.ACTIVE
            "paused" -> ItemStatus.PAUSED
            "completed" -> ItemStatus.COMPLETED
            "skipped" -> ItemStatus.SKIPPED
            "cancelled" -> ItemStatus.CANCELED
            else -> throw RemotePlannerMappingException()
        }

        private fun energyValue(element: kotlinx.serialization.json.JsonElement): String? = when (
            element
        ) {
            is JsonPrimitive -> element.contentOrNull
            is JsonObject -> element["value"]?.let { it as? JsonPrimitive }?.contentOrNull
            else -> null
        }

        private fun validateUuid(raw: String) {
            try {
                if (UUID.fromString(raw).toString() != raw) throw IllegalArgumentException()
            } catch (error: IllegalArgumentException) {
                throw RemotePlannerMappingException(error)
            }
        }

        private fun parseTimestamp(raw: String): OffsetDateTime = try {
            OffsetDateTime.parse(raw, DateTimeFormatter.ISO_OFFSET_DATE_TIME)
        } catch (error: DateTimeException) {
            throw RemotePlannerMappingException(error)
        }

        private fun validateTimestamp(raw: String) {
            parseTimestamp(raw)
        }

        private fun validCursor(cursor: String): Boolean =
            cursor.isNotBlank() && cursor.length <= MAX_CURSOR_CHARS &&
                cursor.none(Char::isISOControl)

        private fun localMinute(date: LocalDate, zone: ZoneId, minute: Int): ZonedDateTime =
            if (minute == MINUTES_PER_DAY) {
                date.plusDays(1).atStartOfDay(zone)
            } else {
                date.atTime(LocalTime.of(minute / 60, minute % 60)).atZone(zone)
            }

        private val SUPPORTED_ITEM_KINDS = setOf(
            "event",
            "task",
            "habit",
            "routine",
            "goal",
            "break",
        )
        private val SUPPORTED_ITEM_STATUSES = setOf(
            "inbox",
            "planned",
            "scheduled",
            "in_progress",
            "paused",
            "completed",
            "skipped",
            "cancelled",
        )
        private val TERMINAL_CANONICAL_STATUSES = setOf("completed", "skipped")
        private val TERMINAL_PROJECTION_SOURCE_STATUSES = setOf(
            "planned",
            "scheduled",
            "in_progress",
            "paused",
        )
        private const val MAX_TERMINAL_PROJECTION_RELOADS = 2
        private val TERMINAL_PROJECTION_RELOAD_RESULTS = setOf(
            TerminalProjectionResult.APPLIED_WRITE,
            TerminalProjectionResult.NEEDS_RELOAD,
        )
    }
}

private fun Instant.coerceAtLeast(minimum: Instant): Instant = if (this < minimum) minimum else this

private fun Instant.coerceAtMost(maximum: Instant): Instant = if (this > maximum) maximum else this
