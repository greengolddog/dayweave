package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.LocalScheduleCompositionProvenanceSnapshot
import com.greengolddog.dayweave.model.MoveLaterApprovalEnvelope
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PendingSchedulePublication
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.RecurrenceOccurrenceSourceSnapshot
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import com.greengolddog.dayweave.model.assessMoveLater
import com.greengolddog.dayweave.model.exactFirmHorizonDayCount
import com.greengolddog.dayweave.model.hasOpenOrPendingExecutionForOccurrence
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.model.isRepresentableMoveLaterSource
import com.greengolddog.dayweave.model.isCoveredBy
import com.greengolddog.dayweave.model.recurrenceIdentityObject
import com.greengolddog.dayweave.model.recurrenceIdentityType
import com.greengolddog.dayweave.model.toApprovalEnvelope
import com.greengolddog.dayweave.model.validatedRecurrenceIdentityJson
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.CanonicalItemReplacement
import com.greengolddog.dayweave.network.CanonicalItemRevisionRequest
import com.greengolddog.dayweave.network.CanonicalPlannerTransport
import com.greengolddog.dayweave.network.CreateCanonicalItemRequest
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.PlannerApiException
import com.greengolddog.dayweave.network.PreviousScheduleAssignmentRequest
import com.greengolddog.dayweave.network.PreviousScheduleBlockRequest
import com.greengolddog.dayweave.network.ReplaceCanonicalItemRequest
import com.greengolddog.dayweave.network.RemoteCanonicalItem
import com.greengolddog.dayweave.network.RemoteItemDeltaChange
import com.greengolddog.dayweave.network.RemotePublishedScheduleRevision
import com.greengolddog.dayweave.network.RemoteScheduleBlock
import com.greengolddog.dayweave.network.RemoteSchedulePreview
import com.greengolddog.dayweave.network.RemoteSchedulePublishResponse
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.ScheduleConfigRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SchedulePublishRequest
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.buildSchedulePublishHttpRequest
import com.greengolddog.dayweave.network.normalizedHttpsApiBaseUrl
import com.greengolddog.dayweave.network.validateBearerToken
import com.greengolddog.dayweave.scheduler.LocalScheduleComposer
import com.greengolddog.dayweave.scheduler.LocalScheduleCompositionProtocolException
import com.greengolddog.dayweave.scheduler.LocalScheduleCompositionRejectedException
import com.greengolddog.dayweave.scheduler.LocalScheduleCompositionRequestException
import com.greengolddog.dayweave.scheduler.LocalScheduleCompositionRequestTooLargeException
import com.greengolddog.dayweave.scheduler.ScheduleProfileExpansionException
import com.greengolddog.dayweave.scheduler.compositionZone
import com.greengolddog.dayweave.scheduler.expandForComposition
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
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
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
    "A canonical server action must be reconciled before changing API credentials",
)

class CanonicalAbandonmentPersistenceException : IllegalStateException(
    "Canonical cache quarantine was not durable; existing credentials were kept",
)

interface LocalCompositionLifecycleFence {
    fun captureGeneration(): Long
    fun isCurrent(generation: Long): Boolean
}

object UnfencedLocalCompositionLifecycle : LocalCompositionLifecycleFence {
    override fun captureGeneration(): Long = 0
    override fun isCurrent(generation: Long): Boolean = generation == 0L
}

/** Pulls canonical deltas, composes the rolling firm horizon, then commits both atomically. */
class CanonicalSyncManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: CanonicalPlannerTransport,
    private val now: () -> Instant = Instant::now,
    private val zoneId: () -> ZoneId = ZoneId::systemDefault,
    /** Optional so existing tests/fakes and older embedders remain source-compatible. */
    private val localScheduleComposer: LocalScheduleComposer? = null,
    private val localCompositionLifecycleFence: LocalCompositionLifecycleFence =
        UnfencedLocalCompositionLifecycle,
    private val newPublicationIdempotencyKey: () -> String = { UUID.randomUUID().toString() },
    private val cancelTimedBreakNotification: suspend () -> Boolean = { true },
    private val reconcileTimedBreakNotification: suspend () -> Unit = {},
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

    private suspend fun <T> withTimedBreakNotificationBarrier(
        transition: suspend () -> T,
    ): T {
        if (!cancelTimedBreakNotification()) throw LocalPlannerStorageException()
        return try {
            transition()
        } finally {
            withContext(NonCancellable) { reconcileTimedBreakNotification() }
        }
    }

    /** Called only while the process-wide binding writer excludes every old response mutation. */
    internal fun quarantineBindingState() {
        mutableState.value = stateFrom(ApiConnectionSnapshot(null, false, null, null))
    }

    /** Serializes credential replacement/forget with every canonical request and reconciliation. */
    suspend fun <T> withConfigurationLock(
        change: suspend () -> T,
    ): T =
        operationMutex.withLock {
            if (plannerStore.hasCredentialReplacementBlocker()) {
                updateError(
                "Recover the pending proposal or canonical action before changing the API connection.",
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
        val quarantined = withTimedBreakNotificationBarrier {
            plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
        }
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
                "Recover the exact pending schedule/proposal operation, reconcile the pending " +
                    "canonical/execution action, or explicitly forget the connection before " +
                    "replacing its bearer token.",
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
            planner.pendingSchedulePublication != null ||
            planner.pendingProposalApplicationMutation != null ||
            planner.proposalApplications.isNotEmpty() ||
            planner.publishedScheduleRevision != null ||
            planner.publishedScheduleProof != null ||
            planner.schedule.any { it.canonicalItemId != null } ||
            planner.canonicalExecutionSyncOrigin != null ||
            planner.canonicalExecutionSession != null ||
            planner.canonicalExecutionHistoryWindow.isNotEmpty() ||
            planner.canonicalExecutionHistoryWindowRevision != null ||
            planner.canonicalExecutionHistoryContinuityEstablished ||
            planner.canonicalExecutionHistoryVerified ||
            planner.pendingExecutionCommand != null ||
            planner.pendingExecutionDeferIntent != null ||
            planner.terminalExecutionOutcomes.isNotEmpty()
        if (
            connection.baseUrl != null || connection.hasBearerToken ||
            hasCredentialBoundPlannerState
        ) {
            val durable = withTimedBreakNotificationBarrier {
                plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
            }
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
                message = "Syncing canonical items and composing the firm horizon…",
                lastInputDigest = plannerStore.state.value.scheduleInputDigest,
                sourceItemCount = plannerStore.state.value.canonicalItems.size,
                scheduledBlockCount = plannerStore.state.value.schedule.size,
            )
            try {
                configuration.withBindingOperation {
                    val instant = now()
                    val planningZone = compositionPlanningZone()
                    ensureDurableWorkspaceBinding(configuration)
                    var update = recoverOrPublishAcceptedSchedule(
                        configuration = configuration,
                        instant = instant,
                        planningZone = planningZone,
                    )
                    var projectionPasses = 0
                    var projectionResult = projectPendingTerminalExecution(configuration)
                    while (
                        projectionPasses < MAX_TERMINAL_PROJECTION_RELOADS &&
                        projectionResult in TERMINAL_PROJECTION_RELOAD_RESULTS
                    ) {
                        projectionPasses += 1
                        update = recoverOrPublishAcceptedSchedule(
                            configuration = configuration,
                            instant = instant,
                            planningZone = planningZone,
                        )
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
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    /**
     * Recovers the server's exact immutable schedule head without previewing or publishing.
     * SSE callers use this same path; a revision hint itself never mutates planner state.
     */
    suspend fun refreshCurrentPublishedSchedule(): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; the cached plan was kept.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        return operationMutex.withLock {
            val resolution = authenticatedConfiguration()
            if (resolution is ConfigurationResolution.Failed) return@withLock resolution.outcome
            val configuration = (resolution as ConfigurationResolution.Ready).configuration
            mutableState.value = CanonicalSyncState(
                phase = CanonicalSyncPhase.SYNCING,
                message = "Checking the current published schedule…",
                lastInputDigest = plannerStore.state.value.scheduleInputDigest,
                sourceItemCount = plannerStore.state.value.canonicalItems.size,
                scheduledBlockCount = plannerStore.state.value.schedule.size,
            )
            try {
                configuration.withBindingOperation {
                    ensureDurableWorkspaceBinding(configuration)
                    val expected = plannerStore.state.value
                    if (plannerStore.durableState.value != expected) {
                        throw LocalPlannerStorageException()
                    }
                    if (expected.hasReplicaBlockingMutation()) {
                        throw CurrentScheduleReplicaBlockedException()
                    }
                    var current = transport.currentSchedule(configuration)
                    ensureConfigurationCurrent(configuration)
                    if (current == null) {
                        val receipt = plannerStore.installNoCurrentPublishedSchedule(
                            expectedState = expected,
                            syncOrigin = configuration.baseUrl.toString(),
                            configurationId = requireNotNull(configuration.configurationId),
                        )
                        if (receipt != null && !receipt.awaitDurable()) {
                            throw LocalPlannerStorageException()
                        }
                        ensureConfigurationCurrent(configuration)
                        updateReplicaSuccess("No schedule has been published for this workspace yet")
                        return@withBindingOperation CanonicalRefreshOutcome.SUCCESS
                    }
                    val canonical = loadDelta(configuration)
                    ensureConfigurationCurrent(configuration)
                    // Publication can advance while item deltas are draining. Refetch the
                    // immutable head so the exact revision map is validated against the newest
                    // canonical generation observed under this binding fence.
                    current = transport.currentSchedule(configuration)
                    ensureConfigurationCurrent(configuration)
                    if (current == null) {
                        val receipt = plannerStore.installNoCurrentPublishedSchedule(
                            expectedState = expected,
                            syncOrigin = configuration.baseUrl.toString(),
                            configurationId = requireNotNull(configuration.configurationId),
                        )
                        if (receipt != null && !receipt.awaitDurable()) {
                            throw LocalPlannerStorageException()
                        }
                        ensureConfigurationCurrent(configuration)
                        updateReplicaSuccess("No schedule has been published for this workspace yet")
                        return@withBindingOperation CanonicalRefreshOutcome.SUCCESS
                    }
                    val revision = validateCurrentScheduleRevision(
                        requireNotNull(current).revision,
                        requireNotNull(current).schedule,
                    )
                    val planningZone = ZoneId.of(revision.timezoneName)
                    val generatedAt = Instant.parse(requireNotNull(current).schedule.plan.asOf)
                    val profile = expected.scheduleCompositionProfile
                    if (!profile.hasValidShape()) throw RemotePlannerMappingException()
                    val replicaHorizonStart = Instant.parse(revision.horizonStart)
                    val replicaHorizonEnd = Instant.parse(revision.horizonEnd)
                    val update = mapPreview(
                        preview = requireNotNull(current).schedule,
                        canonicalItems = canonical.items,
                        syncOrigin = configuration.baseUrl.toString(),
                        deltaCursor = canonical.cursor,
                        generatedAt = generatedAt,
                        planningZone = planningZone,
                        expectedHorizonStart = replicaHorizonStart,
                        expectedHorizonEnd = replicaHorizonEnd,
                        availability = availabilityWithinHorizon(
                            horizonStart = replicaHorizonStart,
                            horizonEnd = replicaHorizonEnd,
                            planningZone = planningZone,
                            profile = profile,
                        ),
                        allowExternalFixed = true,
                        requireExactConfiguredHorizon = false,
                        preservationState = expected,
                    ).copy(
                        configurationId = configuration.configurationId,
                        message = "Installed published schedule revision ${revision.revisionNumber}",
                    )
                    val receipt = plannerStore.installCurrentPublishedSchedule(
                        expectedState = expected,
                        update = update,
                        revision = revision,
                    )
                    if (receipt != null && !receipt.awaitDurable()) {
                        throw LocalPlannerStorageException()
                    }
                    ensureConfigurationCurrent(configuration)
                    updateReplicaSuccess(update.message)
                    CanonicalRefreshOutcome.SUCCESS
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private fun updateReplicaSuccess(message: String) {
        val installed = plannerStore.state.value
        mutableState.value = CanonicalSyncState(
            phase = CanonicalSyncPhase.CONNECTED,
            message = message,
            lastInputDigest = installed.scheduleInputDigest,
            sourceItemCount = installed.canonicalItems.size,
            scheduledBlockCount = installed.schedule.size,
        )
    }

    private fun com.greengolddog.dayweave.model.DayWeaveUiState.hasReplicaBlockingMutation(): Boolean =
        pendingSchedulePublication != null ||
            pendingProposalApplicationMutation != null ||
            pendingCanonicalMutation != null ||
            pendingCanonicalAuthoringMutations.any {
                it.disposition == CanonicalAuthoringDisposition.PENDING
            } ||
            pendingExecutionCommand != null || pendingExecutionDeferIntent != null

    private fun validateCurrentScheduleRevision(
        remote: RemotePublishedScheduleRevision,
        schedule: RemoteSchedulePreview,
    ): PublishedScheduleRevisionSnapshot = try {
        val revisionId = UUID.fromString(remote.id)
        require(revisionId != NIL_UUID && revisionId.toString() == remote.id)
        require(remote.revisionNumber > 0uL)
        require(remote.revision == "${remote.revisionNumber}:${remote.id}")
        require(remote.inputDigest.matches(DIGEST_PATTERN))
        require(remote.inputDigest == schedule.inputDigest)
        val horizonStart = Instant.parse(remote.horizonStart)
        val horizonEnd = Instant.parse(remote.horizonEnd)
        require(horizonStart < horizonEnd)
        require(Duration.between(horizonStart, horizonEnd) <= Duration.ofDays(90))
        require(remote.horizonStart == schedule.plan.horizonStart)
        require(remote.horizonEnd == schedule.plan.horizonEnd)
        require(Instant.parse(schedule.plan.horizonStart) == horizonStart)
        require(Instant.parse(schedule.plan.horizonEnd) == horizonEnd)
        val asOf = Instant.parse(schedule.plan.asOf)
        require(horizonStart <= asOf && asOf < horizonEnd)
        require(remote.timezoneName in SERVER_NAMED_TIMEZONE_IDS)
        requireNotNull(runCatching { ZoneId.of(remote.timezoneName) }.getOrNull())
        val publishedAt = Instant.parse(remote.publishedAt)
        require(!publishedAt.isAfter(now().plusSeconds(PUBLICATION_CLOCK_SKEW_SECONDS)))
        PublishedScheduleRevisionSnapshot(
            id = remote.id,
            revision = remote.revision,
            revisionNumber = remote.revisionNumber,
            inputDigest = remote.inputDigest,
            horizonStart = remote.horizonStart,
            horizonEnd = remote.horizonEnd,
            timezoneName = remote.timezoneName,
            publishedAt = remote.publishedAt,
        )
    } catch (error: IllegalArgumentException) {
        throw RemotePlannerMappingException(error)
    }

    /**
     * Explicitly composes the current rolling firm horizon without any HTTP, cursor, publication,
     * or proof mutation. The result is an encrypted display-only generation and remains
     * execution-locked.
     */
    suspend fun composeLocally(
        admittedLifecycleGeneration: Long? = null,
    ): CanonicalRefreshOutcome {
        val loadState = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (loadState != PlannerLoadState.READY) {
            updateError("Encrypted planner storage is unavailable; the cached plan was kept.")
            return CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE
        }
        return operationMutex.withLock {
            val composer = localScheduleComposer ?: run {
                updateError("The bundled on-device scheduler is unavailable in this build.")
                return@withLock CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
            val resolution = authenticatedConfiguration()
            if (resolution is ConfigurationResolution.Failed) return@withLock resolution.outcome
            val configuration = (resolution as ConfigurationResolution.Ready).configuration
            try {
                configuration.withBindingOperation {
                    val lifecycleGeneration = admittedLifecycleGeneration
                        ?: localCompositionLifecycleFence.captureGeneration()
                    if (!localCompositionLifecycleFence.isCurrent(lifecycleGeneration)) {
                        throw LocalCompositionGenerationChangedException()
                    }
                    val expected = plannerStore.state.value
                    val durable = plannerStore.durableState.value
                    val origin = configuration.baseUrl.toString()
                    val configurationId = configuration.configurationId
                        ?: throw LocalCompositionUnavailableException(
                            "Sync once with device authentication before composing on this device.",
                        )
                    val cursor = expected.canonicalDeltaCursor
                        ?: throw LocalCompositionUnavailableException(
                            "Sync canonical items once before composing on this device.",
                        )
                    requireLocalCompositionPreflight(
                        expected = expected,
                        durable = durable,
                        origin = origin,
                        configurationId = configurationId,
                    )
                    val instant = now()
                    val planningZone = compositionPlanningZone(expected.scheduleCompositionProfile)
                    val planningDate = instant.atZone(planningZone).toLocalDate()
                    val request = previewRequest(
                        instant = instant,
                        planningZone = planningZone,
                        canonicalItems = expected.canonicalItems,
                        syncOrigin = origin,
                        configurationId = configurationId,
                        cachedState = expected,
                    )
                    mutableState.value = CanonicalSyncState(
                        phase = CanonicalSyncPhase.SYNCING,
                        message = "Composing the firm horizon with the bundled scheduler…",
                        lastInputDigest = null,
                        sourceItemCount = expected.canonicalItems.size,
                        scheduledBlockCount = expected.schedule.size,
                    )
                    if (!localCompositionLifecycleFence.isCurrent(lifecycleGeneration)) {
                        throw LocalCompositionGenerationChangedException()
                    }
                    val composition = composer.compose(expected.canonicalItems, request)
                    requireLocalCompositionCommitFence(
                        configuration = configuration,
                        lifecycleGeneration = lifecycleGeneration,
                        expected = expected,
                        capturedAt = instant,
                        planningZone = planningZone,
                        planningDate = planningDate,
                    )
                    val update = mapPreview(
                        preview = composition.asRemotePreview(),
                        canonicalItems = expected.canonicalItems,
                        syncOrigin = origin,
                        deltaCursor = cursor,
                        generatedAt = instant,
                        planningZone = planningZone,
                        expectedHorizonStart = parseTimestamp(request.horizonStart).toInstant(),
                        expectedHorizonEnd = parseTimestamp(request.horizonEnd).toInstant(),
                        availability = request.availability,
                        inputDigestPattern = LOCAL_FINGERPRINT_PATTERN,
                        preservationState = expected,
                    ).copy(configurationId = configurationId)
                    val provenance = LocalScheduleCompositionProvenanceSnapshot(
                        syncOrigin = origin,
                        configurationId = configurationId,
                        deltaCursor = cursor,
                        localInputFingerprint = composition.localInputFingerprint,
                        scheduleRequestFingerprint = composition.scheduleRequestFingerprint,
                        // Replaced by the store with the exact installed-state digest inside the
                        // same encrypted generation.
                        stateInputFingerprint = EMPTY_SHA256_FINGERPRINT,
                        generatedAt = instant.toString(),
                        asOf = request.asOf,
                        horizonStart = request.horizonStart,
                        horizonEnd = request.horizonEnd,
                        timezoneName = request.timezoneName,
                        sourceItemRevisions = composition.sourceItemRevisions,
                    )
                    // Mapping can be large and JNI is not preemptible. Revalidate the complete
                    // admission/binding/time/state fence at the last point before durable install.
                    requireLocalCompositionCommitFence(
                        configuration = configuration,
                        lifecycleGeneration = lifecycleGeneration,
                        expected = expected,
                        capturedAt = instant,
                        planningZone = planningZone,
                        planningDate = planningDate,
                    )
                    val transition = plannerStore.installLocalScheduleComposition(
                        expectedState = expected,
                        update = update,
                        provenance = provenance,
                    ) ?: throw LocalPlannerStorageException()
                    if (!transition.persistence.awaitDurable()) throw LocalPlannerStorageException()
                    val installed = plannerStore.durableState.value
                    if (
                        installed?.localScheduleCompositionProvenance != transition.provenance ||
                        !transition.provenance.matchesState(installed)
                    ) {
                        throw LocalPlannerStorageException()
                    }
                    mutableState.value = CanonicalSyncState(
                        phase = CanonicalSyncPhase.READY,
                        message = installed.scheduleMessage,
                        lastInputDigest = null,
                        sourceItemCount = update.items.size,
                        scheduledBlockCount = update.schedule.size,
                    )
                    CanonicalRefreshOutcome.SUCCESS
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private suspend fun requireLocalCompositionCommitFence(
        configuration: AuthenticatedApiConfiguration,
        lifecycleGeneration: Long,
        expected: com.greengolddog.dayweave.model.DayWeaveUiState,
        capturedAt: Instant,
        planningZone: ZoneId,
        planningDate: LocalDate,
    ) {
        ensureConfigurationCurrent(configuration)
        val currentTime = now()
        if (
            !localCompositionLifecycleFence.isCurrent(lifecycleGeneration) ||
            compositionPlanningZone(expected.scheduleCompositionProfile) != planningZone ||
            currentTime < capturedAt ||
            currentTime.atZone(planningZone).toLocalDate() != planningDate ||
            plannerStore.state.value != expected ||
            plannerStore.durableState.value != expected
        ) {
            throw LocalCompositionGenerationChangedException()
        }
    }

    private fun requireLocalCompositionPreflight(
        expected: com.greengolddog.dayweave.model.DayWeaveUiState,
        durable: com.greengolddog.dayweave.model.DayWeaveUiState?,
        origin: String,
        configurationId: String,
    ) {
        if (durable == null || durable != expected) {
            throw LocalCompositionUnavailableException(
                "Wait for encrypted planner changes to finish saving, then compose again.",
            )
        }
        if (!expected.scheduleCompositionProfile.hasValidShape()) {
            throw LocalCompositionUnavailableException(
                "Review the saved scheduling profile before composing on this device.",
            )
        }
        if (
            expected.canonicalSyncOrigin != origin ||
            expected.canonicalConfigurationId != configurationId ||
            expected.canonicalDeltaCursor.isNullOrBlank()
        ) {
            throw LocalCompositionUnavailableException(
                "The encrypted canonical cache does not match the current API connection.",
            )
        }
        if (
            expected.pendingSchedulePublication != null ||
            expected.pendingProposalApplicationMutation != null ||
            expected.pendingCanonicalMutation != null ||
            expected.pendingCanonicalAuthoringMutations.isNotEmpty() ||
            expected.pendingExecutionCommand != null ||
            expected.pendingExecutionDeferIntent != null ||
            expected.canonicalExecutionSession != null ||
            expected.activeSession != null ||
            unresolvedLocalExecutionMessage(expected) != null
        ) {
            throw LocalCompositionUnavailableException(
                "Finish or reconcile the pending canonical work before composing on this device.",
            )
        }
        if (
            expected.canonicalExecutionSyncOrigin != origin ||
            expected.canonicalExecutionConfigurationId != configurationId ||
            !expected.canonicalExecutionHistoryVerified ||
            !expected.canonicalExecutionHistoryContinuityEstablished ||
            expected.canonicalExecutionHistoryWindowRevision !=
            expected.canonicalExecutionRevision ||
            (expected.canonicalExecutionRevision == 0L) !=
            expected.canonicalExecutionHistoryWindow.isEmpty() ||
            expected.canonicalExecutionHistoryWindow.any {
                it.revision > expected.canonicalExecutionRevision
            }
        ) {
            throw LocalCompositionUnavailableException(
                "Sync and verify bounded execution history before composing on this device.",
            )
        }
        if (expected.terminalExecutionOutcomes.values.any { outcome ->
                outcome.requiresCanonicalItemProjection &&
                    outcome.canonicalProjectionRevision == null &&
                    outcome.canonicalProjectionResolution == null &&
                    expected.isNewestExecutionForProjection(outcome.session)
            }
        ) {
            throw LocalCompositionUnavailableException(
                "Finish the authoritative terminal projection before composing on this device.",
            )
        }
    }

    private suspend fun loadConsistentPlan(
        configuration: AuthenticatedApiConfiguration,
        instant: Instant,
        planningZone: ZoneId,
        forceCanonicalRebuild: Boolean = false,
    ): AcceptedCanonicalPreview {
        for (attempt in 1..MAX_SNAPSHOT_ATTEMPTS) {
            val canonical = loadDelta(configuration, forceCanonicalRebuild)
            val profile = plannerStore.state.value.scheduleCompositionProfile
            if (!profile.hasValidShape()) throw RemotePlannerMappingException()
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
                val update = mapPreview(
                    preview = preview,
                    canonicalItems = canonical.items,
                    syncOrigin = configuration.baseUrl.toString(),
                    deltaCursor = canonical.cursor,
                    generatedAt = instant,
                    planningZone = planningZone,
                    expectedHorizonStart = parseTimestamp(request.horizonStart).toInstant(),
                    expectedHorizonEnd = parseTimestamp(request.horizonEnd).toInstant(),
                    availability = request.availability,
                ).copy(configurationId = configuration.configurationId)
                return AcceptedCanonicalPreview(request, update)
            } catch (error: RemoteSnapshotChangedException) {
                if (attempt == MAX_SNAPSHOT_ATTEMPTS) throw error
                // Neither transient delta nor preview has touched durable state. Pull again from
                // the last persisted cursor and require a preview of that exact revision map.
            }
        }
        throw RemoteSnapshotChangedException()
    }

    private suspend fun recoverOrPublishAcceptedSchedule(
        configuration: AuthenticatedApiConfiguration,
        instant: Instant,
        planningZone: ZoneId,
    ): CanonicalPlanUpdate {
        var recoveredStaleOrReplayCount = 0
        while (true) {
            try {
                plannerStore.state.value.pendingSchedulePublication?.let { pending ->
                    return resumeSchedulePublication(configuration, pending)
                }
                val pendingResolution = reconcilePendingMutation(configuration)
                if (pendingResolution != PendingMutationResolution.SUPERSEDED) {
                    projectPendingTerminalExecution(configuration)
                }
                val authoring = publishPendingCanonicalAuthoringMutations(
                    configuration = configuration,
                    instant = instant,
                    planningZone = planningZone,
                )
                val loaded = loadConsistentPlan(
                    configuration = configuration,
                    instant = instant,
                    planningZone = planningZone,
                    forceCanonicalRebuild = authoring.conflictedCount > 0,
                )
                var message = loaded.update.message
                if (pendingResolution == PendingMutationResolution.SUPERSEDED) {
                    message += " A pending action was superseded by newer canonical state."
                }
                if (recoveredStaleOrReplayCount > 0) {
                    message += " A stale or replayed publication was safely recomposed."
                }
                if (authoring.appliedCount > 0) {
                    message += " Published ${authoring.appliedCount} saved canonical " +
                        "change${if (authoring.appliedCount == 1) "" else "s"}."
                }
                if (authoring.conflictedCount > 0) {
                    message += " ${authoring.conflictedCount} saved canonical " +
                        "change${if (authoring.conflictedCount == 1) " needs" else "s need"} " +
                        "conflict review."
                }
                if (authoring.deferredCount > 0) {
                    message += " ${authoring.deferredCount} dependent canonical " +
                        "change${if (authoring.deferredCount == 1) " remains" else "s remain"} " +
                        "queued behind conflict review."
                }
                val accepted = loaded.copy(update = loaded.update.copy(message = message))
                if (plannerStore.state.value.pendingCanonicalAuthoringMutations.any {
                        it.disposition == CanonicalAuthoringDisposition.PENDING
                    }
                ) {
                    // A dependency can remain queued behind a conflicted parent. Persist the fresh
                    // canonical view, but never publish a schedule that omits its local overlay.
                    persistCanonicalAuthoringPreflight(configuration, accepted.update)
                    return accepted.update
                }
                return publishAcceptedSchedule(
                    configuration,
                    accepted,
                )
            } catch (_: ReplayedSchedulePublicationNeedsFreshSnapshotException) {
                recoveredStaleOrReplayCount += 1
            } catch (error: StaleSchedulePublicationRejectedException) {
                val cleared = try {
                    plannerStore.discardStaleSchedulePublication(error.expected)
                } catch (_: IllegalArgumentException) {
                    throw CanonicalConfigurationChangedException()
                }
                if (cleared == null || !cleared.awaitDurable()) {
                    throw LocalPlannerStorageException()
                }
                ensureConfigurationCurrent(configuration)
                recoveredStaleOrReplayCount += 1
            }
            if (
                recoveredStaleOrReplayCount >
                MAX_SCHEDULE_PUBLICATION_RECOVERY_RECOMPOSITIONS
            ) {
                throw SchedulePublicationRecoveryExhaustedException()
            }
        }
    }

    /**
     * Installs a fresh, unpublished canonical view before sending queued authoring operations.
     * This both establishes the exact credential binding for first-sync drafts and gives submitted
     * requests one authoritative cache observation before their immutable idempotent replay.
     */
    private suspend fun publishPendingCanonicalAuthoringMutations(
        configuration: AuthenticatedApiConfiguration,
        instant: Instant,
        planningZone: ZoneId,
    ): CanonicalAuthoringPushSummary {
        if (plannerStore.state.value.pendingCanonicalAuthoringMutations.none {
                it.disposition == CanonicalAuthoringDisposition.PENDING
            }
        ) {
            return CanonicalAuthoringPushSummary()
        }
        val preflight = loadConsistentPlan(configuration, instant, planningZone)
        rebaseCanonicalAuthoringPreflight(configuration, preflight.update.items)
        persistCanonicalAuthoringPreflight(configuration, preflight.update)

        // A crash can leave one submitted request beside older unsubmitted siblings. Reconcile
        // that ambiguous request first; the store's topological order remains intact because a
        // submitted request cannot retain an unresolved dependency.
        val ordered = plannerStore.sortedCanonicalAuthoringMutations()
            .filter { it.disposition == CanonicalAuthoringDisposition.PENDING }
            .let { mutations ->
                mutations.filter(PendingCanonicalAuthoringMutation::isSubmitted) +
                    mutations.filterNot(PendingCanonicalAuthoringMutation::isSubmitted)
            }
        var appliedCount = 0
        var conflictedCount = 0
        for (original in ordered) {
            var mutation = plannerStore.canonicalAuthoringMutation(original.id)
                ?.takeIf { it.disposition == CanonicalAuthoringDisposition.PENDING }
                ?: continue
            if (mutation.isSubmitted) {
                when (reconcileCanonicalAuthoringFromCache(configuration, mutation)) {
                    CanonicalAuthoringCacheResolution.APPLIED -> {
                        appliedCount += 1
                        continue
                    }
                    CanonicalAuthoringCacheResolution.CONFLICTED -> {
                        conflictedCount += 1
                        break
                    }
                    CanonicalAuthoringCacheResolution.NO_EVIDENCE -> Unit
                }
            } else {
                if (mutation.syncOrigin == null) {
                    val bound = plannerStore.bindCanonicalAuthoringMutation(
                        id = mutation.id,
                        syncOrigin = configuration.baseUrl.toString(),
                        configurationId = configuration.configurationId,
                    ) ?: throw LocalPlannerStorageException()
                    if (!bound.persistence.awaitDurable()) throw LocalPlannerStorageException()
                    mutation = bound.mutation
                }
                ensureConfigurationCurrent(configuration)
                val submitted = try {
                    plannerStore.markCanonicalAuthoringSubmitted(mutation.id)
                } catch (_: IllegalArgumentException) {
                    return CanonicalAuthoringPushSummary(
                        appliedCount = appliedCount,
                        conflictedCount = conflictedCount,
                        deferredCount = ordered.count { queued ->
                            plannerStore.canonicalAuthoringMutation(queued.id)
                                ?.disposition == CanonicalAuthoringDisposition.PENDING
                        },
                    )
                } ?: throw LocalPlannerStorageException()
                if (!submitted.persistence.awaitDurable()) throw LocalPlannerStorageException()
                mutation = submitted.mutation
            }
            ensureConfigurationCurrent(configuration)
            try {
                val remote = sendCanonicalAuthoringMutation(configuration, mutation)
                ensureConfigurationCurrent(configuration)
                val response = mapCanonicalItem(
                    remote,
                    requireActive = mutation.operation != CanonicalAuthoringOperation.TRASH,
                )
                val receipt = try {
                    plannerStore.applyCanonicalAuthoringResponse(mutation, response)
                } catch (error: IllegalArgumentException) {
                    throw CanonicalAuthoringResponseException(error)
                } ?: throw LocalPlannerStorageException()
                if (!receipt.awaitDurable()) throw LocalPlannerStorageException()
                appliedCount += 1
                if (hasQueuedMutationForAffectedParent(mutation, response)) {
                    val refreshed = loadConsistentPlan(
                        configuration = configuration,
                        instant = instant,
                        planningZone = planningZone,
                        forceCanonicalRebuild = true,
                    )
                    rebaseCanonicalAuthoringPreflight(configuration, refreshed.update.items)
                    persistCanonicalAuthoringPreflight(configuration, refreshed.update)
                }
            } catch (error: PlannerApiException.CanonicalMutationRejected) {
                ensureConfigurationCurrent(configuration)
                persistCanonicalAuthoringConflict(
                    mutation,
                    "The server rejected this saved canonical hierarchy or revision. " +
                        "Review the retained change before copying or discarding it.",
                )
                conflictedCount += 1
                break
            } catch (error: PlannerApiException.Validation) {
                ensureConfigurationCurrent(configuration)
                persistCanonicalAuthoringConflict(
                    mutation,
                    "The server rejected this saved canonical item contract (HTTP " +
                        "${error.statusCode}). Review the retained change before copying or " +
                        "discarding it.",
                )
                conflictedCount += 1
                break
            }
        }
        val deferredCount = plannerStore.state.value.pendingCanonicalAuthoringMutations.count {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        }
        return CanonicalAuthoringPushSummary(appliedCount, conflictedCount, deferredCount)
    }

    private suspend fun persistCanonicalAuthoringPreflight(
        configuration: AuthenticatedApiConfiguration,
        update: CanonicalPlanUpdate,
    ) {
        ensureConfigurationCurrent(configuration)
        val receipt = try {
            plannerStore.replaceCanonicalPlan(update)
        } catch (error: IllegalArgumentException) {
            throw CanonicalConfigurationChangedException()
        } ?: throw LocalPlannerStorageException()
        if (!receipt.awaitDurable()) throw LocalPlannerStorageException()
        ensureConfigurationCurrent(configuration)
    }

    private suspend fun rebaseCanonicalAuthoringPreflight(
        configuration: AuthenticatedApiConfiguration,
        authoritativeItems: List<CanonicalItemSnapshot>,
    ) {
        ensureConfigurationCurrent(configuration)
        val receipt = try {
            plannerStore.rebaseUnsubmittedCanonicalAuthoringBases(authoritativeItems)
        } catch (error: IllegalArgumentException) {
            throw CanonicalAuthoringResponseException(error)
        } ?: throw LocalPlannerStorageException()
        if (!receipt.awaitDurable()) throw LocalPlannerStorageException()
        ensureConfigurationCurrent(configuration)
    }

    private fun hasQueuedMutationForAffectedParent(
        mutation: PendingCanonicalAuthoringMutation,
        response: CanonicalItemSnapshot,
    ): Boolean {
        val affectedParentIds = setOfNotNull(
            mutation.baseItem?.parentId,
            mutation.draft?.parentId,
            response.parentId,
        )
        if (affectedParentIds.isEmpty()) return false
        return plannerStore.state.value.pendingCanonicalAuthoringMutations.any { queued ->
            queued.disposition == CanonicalAuthoringDisposition.PENDING &&
                !queued.isSubmitted && queued.itemId in affectedParentIds
        }
    }

    private suspend fun reconcileCanonicalAuthoringFromCache(
        configuration: AuthenticatedApiConfiguration,
        mutation: PendingCanonicalAuthoringMutation,
    ): CanonicalAuthoringCacheResolution {
        val state = plannerStore.state.value
        val candidate = when (mutation.operation) {
            CanonicalAuthoringOperation.CREATE -> state.canonicalItems.firstOrNull {
                it.id == mutation.itemId
            }
            CanonicalAuthoringOperation.REPLACE,
            CanonicalAuthoringOperation.RESTORE,
            -> state.canonicalItems.firstOrNull {
                it.id == mutation.itemId &&
                    it.revision > requireNotNull(mutation.expectedRevision)
            }
            CanonicalAuthoringOperation.TRASH -> state.canonicalRecentlyDeleted.firstOrNull {
                it.id == mutation.itemId &&
                    it.revision > requireNotNull(mutation.expectedRevision)
            }?.lastKnownItem?.takeIf { it.deletedAt != null }
        } ?: return CanonicalAuthoringCacheResolution.NO_EVIDENCE
        ensureConfigurationCurrent(configuration)
        val receipt = try {
            plannerStore.applyCanonicalAuthoringResponse(mutation, candidate)
        } catch (_: IllegalArgumentException) {
            persistCanonicalAuthoringConflict(
                mutation,
                "The canonical item now has different content or revision state. Review the " +
                    "retained local change before copying or discarding it.",
            )
            return CanonicalAuthoringCacheResolution.CONFLICTED
        } ?: throw LocalPlannerStorageException()
        if (!receipt.awaitDurable()) throw LocalPlannerStorageException()
        return CanonicalAuthoringCacheResolution.APPLIED
    }

    private suspend fun persistCanonicalAuthoringConflict(
        mutation: PendingCanonicalAuthoringMutation,
        diagnostic: String,
    ) {
        val receipt = try {
            plannerStore.markCanonicalAuthoringConflict(mutation.id, diagnostic)
        } catch (error: IllegalArgumentException) {
            throw CanonicalConfigurationChangedException()
        } ?: throw LocalPlannerStorageException()
        if (!receipt.persistence.awaitDurable()) throw LocalPlannerStorageException()
    }

    private suspend fun sendCanonicalAuthoringMutation(
        configuration: AuthenticatedApiConfiguration,
        mutation: PendingCanonicalAuthoringMutation,
    ): RemoteCanonicalItem = when (mutation.operation) {
        CanonicalAuthoringOperation.CREATE -> transport.createItem(
            configuration = configuration,
            idempotencyKey = mutation.idempotencyKey,
            request = requireNotNull(mutation.draft).toCreateCanonicalItemRequest(mutation.itemId),
        )
        CanonicalAuthoringOperation.REPLACE -> transport.replaceItem(
            configuration = configuration,
            id = mutation.itemId,
            idempotencyKey = mutation.idempotencyKey,
            request = ReplaceCanonicalItemRequest(
                expectedRevision = requireNotNull(mutation.expectedRevision),
                item = requireNotNull(mutation.draft).toCanonicalItemReplacement(mutation.itemId),
            ),
        )
        CanonicalAuthoringOperation.TRASH -> transport.trashItem(
            configuration = configuration,
            id = mutation.itemId,
            idempotencyKey = mutation.idempotencyKey,
            expectedRevision = requireNotNull(mutation.expectedRevision),
        )
        CanonicalAuthoringOperation.RESTORE -> transport.restoreItem(
            configuration = configuration,
            id = mutation.itemId,
            idempotencyKey = mutation.idempotencyKey,
            request = CanonicalItemRevisionRequest(
                expectedRevision = requireNotNull(mutation.expectedRevision),
            ),
        )
    }

    private fun CanonicalItemDraft.toCanonicalItemReplacement(
        itemId: String,
    ): CanonicalItemReplacement {
        val value = normalized().also { it.requireValid(itemId) }
        return CanonicalItemReplacement(
            isSensitive = value.isSensitive,
            kind = value.kind.name.lowercase(),
            status = value.placement.wireValue,
            title = value.title,
            notes = value.notes,
            timezoneName = value.timezoneName,
            durationSeconds = value.durationSeconds,
            deadlineAt = value.deadlineAt,
            earliestStartAt = value.earliestStartAt,
            recurrence = value.recurrence?.toCanonicalJson(),
            flexibleConstraints = value.constraints.toCanonicalJson(
                value.eventTiming,
                value.durationSeconds,
                value.timezoneName,
            ),
            splitPolicy = value.split.toCanonicalJson(value.durationSeconds),
            importance = value.importance,
            urgency = value.urgency,
            parentId = value.parentId,
            siblingOrder = value.siblingOrder,
        )
    }

    private fun CanonicalItemDraft.toCreateCanonicalItemRequest(
        itemId: String,
    ): CreateCanonicalItemRequest {
        val fields = toCanonicalItemReplacement(itemId)
        return CreateCanonicalItemRequest(
            id = itemId,
            isSensitive = fields.isSensitive,
            kind = fields.kind,
            status = fields.status,
            title = fields.title,
            notes = fields.notes,
            timezoneName = fields.timezoneName,
            durationSeconds = fields.durationSeconds,
            deadlineAt = fields.deadlineAt,
            earliestStartAt = fields.earliestStartAt,
            recurrence = fields.recurrence,
            flexibleConstraints = fields.flexibleConstraints,
            splitPolicy = fields.splitPolicy,
            importance = fields.importance,
            urgency = fields.urgency,
            parentId = fields.parentId,
            siblingOrder = fields.siblingOrder,
        )
    }

    private suspend fun publishAcceptedSchedule(
        configuration: AuthenticatedApiConfiguration,
        accepted: AcceptedCanonicalPreview,
    ): CanonicalPlanUpdate {
        ensureConfigurationCurrent(configuration)
        val idempotencyKey = newPublicationIdempotencyKey()
        val pending = try {
            val canonicalKey = UUID.fromString(idempotencyKey)
            require(canonicalKey != NIL_UUID && canonicalKey.toString() == idempotencyKey)
            val publishRequest = SchedulePublishRequest(
                idempotencyKey = idempotencyKey,
                expectedInputDigest = accepted.update.inputDigest,
                schedule = accepted.request,
            )
            PendingSchedulePublication(
                schemaVersion = SCHEDULE_PUBLICATION_JOURNAL_VERSION,
                idempotencyKey = idempotencyKey,
                syncOrigin = configuration.baseUrl.toString(),
                configurationId = configuration.configurationId,
                preparedAt = now().toString(),
                request = buildSchedulePublishHttpRequest(configuration, publishRequest),
                candidate = accepted.update,
            ).also(plannerStore::validateSchedulePublication)
        } catch (error: IllegalArgumentException) {
            throw SchedulePublicationContractException(error)
        }
        val staged = try {
            plannerStore.stageSchedulePublication(pending)
        } catch (error: IllegalArgumentException) {
            throw CanonicalConfigurationChangedException()
        }
        if (staged == null || !staged.awaitDurable()) throw LocalPlannerStorageException()
        ensureConfigurationCurrent(configuration)
        if (plannerStore.state.value.pendingSchedulePublication != pending) {
            throw CanonicalConfigurationChangedException()
        }
        return resumeSchedulePublication(configuration, pending)
    }

    private suspend fun resumeSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingSchedulePublication,
    ): CanonicalPlanUpdate {
        try {
            plannerStore.validateSchedulePublication(pending)
        } catch (error: IllegalArgumentException) {
            throw SchedulePublicationContractException(error)
        }
        if (
            pending.syncOrigin != configuration.baseUrl.toString() ||
            pending.configurationId != configuration.configurationId ||
            plannerStore.state.value.pendingSchedulePublication != pending
        ) {
            throw CanonicalConfigurationChangedException()
        }
        ensureConfigurationCurrent(configuration)
        val response = try {
            transport.publish(configuration, pending.request)
        } catch (error: PlannerApiException.SchedulePublicationStale) {
            throw StaleSchedulePublicationRejectedException(pending, error)
        }
        val receivedAt = now()
        ensureConfigurationCurrent(configuration)
        if (plannerStore.state.value.pendingSchedulePublication != pending) {
            throw CanonicalConfigurationChangedException()
        }
        val revision = validateSchedulePublishResponse(pending, response, receivedAt)
        if (response.replayed) {
            val resolved = try {
                plannerStore.resolveReplayedSchedulePublication(pending, revision)
            } catch (_: IllegalArgumentException) {
                throw CanonicalConfigurationChangedException()
            }
            if (resolved == null || !resolved.awaitDurable()) throw LocalPlannerStorageException()
            ensureConfigurationCurrent(configuration)
            throw ReplayedSchedulePublicationNeedsFreshSnapshotException()
        }
        val committed = try {
            plannerStore.commitSchedulePublication(
                expected = pending,
                revision = revision,
                replayed = response.replayed,
            )
        } catch (error: IllegalArgumentException) {
            throw CanonicalConfigurationChangedException()
        }
        if (committed == null || !committed.awaitDurable()) throw LocalPlannerStorageException()
        ensureConfigurationCurrent(configuration)
        return pending.candidate
    }

    private fun validateSchedulePublishResponse(
        pending: PendingSchedulePublication,
        response: RemoteSchedulePublishResponse,
        receivedAt: Instant,
    ): PublishedScheduleRevisionSnapshot = try {
        val remote = response.revision
        val revisionId = UUID.fromString(remote.id)
        require(revisionId != NIL_UUID && revisionId.toString() == remote.id)
        require(remote.revisionNumber > 0uL)
        require(remote.revision == "${remote.revisionNumber}:${remote.id}")
        val exactRequest = mutationJson.decodeFromString<SchedulePublishRequest>(
            pending.request.bodyJson,
        )
        require(remote.inputDigest == exactRequest.expectedInputDigest)
        require(remote.horizonStart == exactRequest.schedule.horizonStart)
        require(remote.horizonEnd == exactRequest.schedule.horizonEnd)
        require(remote.timezoneName == exactRequest.schedule.timezoneName)
        val publishedAt = requireNotNull(
            runCatching { Instant.parse(remote.publishedAt) }.getOrNull(),
        )
        require(!publishedAt.isAfter(receivedAt.plusSeconds(PUBLICATION_CLOCK_SKEW_SECONDS)))
        PublishedScheduleRevisionSnapshot(
            id = remote.id,
            revision = remote.revision,
            revisionNumber = remote.revisionNumber,
            inputDigest = remote.inputDigest,
            horizonStart = remote.horizonStart,
            horizonEnd = remote.horizonEnd,
            timezoneName = remote.timezoneName,
            publishedAt = remote.publishedAt,
        )
    } catch (error: IllegalArgumentException) {
        throw SchedulePublicationContractException(error)
    }

    suspend fun start(blockId: String): CanonicalRefreshOutcome = focusTransitionMutex.withLock {
        val current = plannerStore.state.value
        if (!current.isCanonicalPlanCurrent(now(), zoneId())) {
            updateError("This cached plan is not current. Recompose before starting new work.")
            return@withLock CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        val block = current.schedule.firstOrNull { it.id == blockId }
        if (block?.canonicalItemId != null && !current.hasPublishedExecutionAuthority(block)) {
            updateError("This block has no durable exact publication proof. Recompose first.")
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

    /** Skips only a one-shot, wholly scheduled canonical item. */
    suspend fun skipScheduled(blockId: String): CanonicalRefreshOutcome {
        val block = plannerStore.state.value.schedule.firstOrNull { it.id == blockId }
            ?: return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        if (!plannerStore.state.value.hasPublishedExecutionAuthority(block)) {
            updateError("Sync and publish this on-device plan before changing canonical work.")
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (
            block.status != ItemStatus.SCHEDULED || block.occurrenceId != null ||
            block.kind !in setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE, ItemKind.GOAL) ||
            !block.isFlexible || block.isHardConstraint ||
            block.canonicalBlockKind == "external_fixed" ||
            requiresLocalSessionState(blockId) || hasUnscheduledRemaining(blockId)
        ) {
            updateError(
                "Only a fully scheduled one-shot item can be skipped from the timeline.",
            )
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        return recomposeAfterTerminalAction(
            mutateCanonicalBlock(
                blockId = blockId,
                targetStatus = "skipped",
                displayStatus = ItemStatus.SKIPPED,
                allowedStatuses = setOf(ItemStatus.SCHEDULED),
            ),
        )
    }

    suspend fun doLater(
        blockId: String,
        moveStart: Instant,
        approval: MoveLaterApprovalEnvelope? = null,
    ): CanonicalRefreshOutcome {
        val exactMoveStart = moveStart.truncatedTo(java.time.temporal.ChronoUnit.SECONDS)
        if (exactMoveStart != moveStart || exactMoveStart <= now()) {
            updateError("Choose a future whole-second time for this work.")
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        val planner = plannerStore.state.value
        val block = planner.schedule.firstOrNull { it.id == blockId }
            ?: return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        if (!planner.hasPublishedExecutionAuthority(block)) {
            updateError("Sync and publish this on-device plan before changing canonical work.")
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (block.status != ItemStatus.SCHEDULED) {
            updateError("Active or paused synced work must use its exact execution lease to move.")
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (!block.isRepresentableMoveLaterSource()) {
            updateError("This fixed source cannot be safely represented as moved work.")
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        block.occurrenceId?.let { occurrenceId ->
            if (planner.hasOpenOrPendingExecutionForOccurrence(occurrenceId)) {
                updateError(
                    "Pause, finish, skip, or defer the active occurrence session before " +
                        "moving its scheduled siblings.",
                )
                return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
            val occurrenceBlocks = planner.schedule.filter { it.occurrenceId == occurrenceId }
            if (
                occurrenceBlocks.isEmpty() || occurrenceBlocks.any { sibling ->
                    sibling.status != ItemStatus.SCHEDULED ||
                        !sibling.isRepresentableMoveLaterSource()
                } || planner.unscheduledWork.any { work ->
                    work.occurrenceId == occurrenceId && work.remainingMinutes > 0
                }
            ) {
                updateError(
                    "Every session in this occurrence must be fully scheduled and flexible " +
                        "before it can move as one unit.",
                )
                return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
        }
        val assessment = planner.assessMoveLater(blockId, exactMoveStart, now())
        if (assessment == null) {
            updateError("The exact move window could not be verified. Recompose and try again.")
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (!assessment.fitsFirmHorizonDay) {
            updateError(
                "That move falls outside the exact firm horizon or crosses a planning-day " +
                    "boundary. Keep the whole move inside one horizon day.",
            )
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (assessment.crossesUnrelaxableHardDeadline) {
            updateError(
                "This occurrence cannot move beyond its hard deadline. Change the item " +
                    "constraint first.",
            )
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        val reviewedApproval = approval ?: assessment.takeUnless {
            it.requiresConfirmation
        }?.toApprovalEnvelope()
        if (!assessment.isCoveredBy(reviewedApproval)) {
            updateError(
                "The placement risks changed after review. Review the current warning before moving.",
            )
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        val usesLocalSessionState = requiresLocalSessionState(blockId)
        block.occurrenceId?.let { occurrenceId ->
            val identityType = recurrenceIdentityType(
                planner.recurrenceOccurrenceSources[occurrenceId]?.identityJson,
            )
            if (identityType == null || identityType == "custom") {
                updateError(
                    "This recurrence does not yet have a movable per-occurrence identity.",
                )
                return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
            }
        }
        if (usesLocalSessionState && block.occurrenceId == null) {
            updateError(
                "This split task cannot be moved as a whole without risking credited " +
                    "or unscheduled work.",
            )
            return CanonicalRefreshOutcome.INVALID_LOCAL_STATE
        }
        if (usesLocalSessionState) {
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
            val saved = deferLocalCanonicalBlock(blockId, exactMoveStart, reviewedApproval)
            return if (saved == CanonicalRefreshOutcome.SUCCESS) refreshAndCompose() else saved
        }
        val mutation = mutateCanonicalBlock(
            blockId = blockId,
            targetStatus = "scheduled",
            displayStatus = ItemStatus.SCHEDULED,
            allowedStatuses = setOf(
                ItemStatus.SCHEDULED,
            ),
            deferUntil = exactMoveStart,
            moveLaterStart = exactMoveStart,
            moveLaterApproval = reviewedApproval,
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
                configuration.withBindingOperation {
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
                    return@withBindingOperation CanonicalRefreshOutcome.SUCCESS
                }
                val mutation = replaceCanonicalItemSensitivity(
                    configuration = configuration,
                    item = item,
                    targetIsSensitive = isSensitive,
                )
                if (mutation == PendingMutationResolution.SUPERSEDED) {
                    updateError("A newer item revision superseded the privacy change; review it again.")
                    return@withBindingOperation CanonicalRefreshOutcome.STALE_REVISION
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
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
        val uncertainState = plannerStore.state.value
        val uncertain = uncertainState.pendingCanonicalMutation
        if (outcome == CanonicalRefreshOutcome.SUCCESS || uncertain == null) return outcome
        val previous = uncertainState.canonicalItems.firstOrNull {
            it.id == uncertain.itemId && it.revision == uncertain.expectedRevision
        }
        val requestedReplacement = runCatching {
            mutationJson.decodeFromString<ReplaceCanonicalItemRequest>(
                uncertain.replacementRequestJson,
            ).item
        }.getOrNull()
        val reconciled = refreshAndCompose()
        if (reconciled != CanonicalRefreshOutcome.SUCCESS) return outcome
        val authoritative = plannerStore.state.value.canonicalItems.firstOrNull {
            it.id == uncertain.itemId
        }
        return if (
            authoritative != null && authoritative.revision > uncertain.expectedRevision &&
            authoritative.status == uncertain.targetStatus &&
            authoritative.isSensitive == uncertain.targetIsSensitive &&
            previous != null && requestedReplacement != null &&
            matchesReplacement(authoritative, previous, requestedReplacement)
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

    private fun unresolvedLocalExecutionMessage(
        state: com.greengolddog.dayweave.model.DayWeaveUiState = plannerStore.state.value,
    ): String? {
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

    private suspend fun deferLocalCanonicalBlock(
        blockId: String,
        moveStart: Instant,
        approval: MoveLaterApprovalEnvelope?,
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
                block?.canonicalItemId == null ||
                block.status !in setOf(
                    ItemStatus.SCHEDULED,
                    ItemStatus.ACTIVE,
                    ItemStatus.PAUSED,
                ) ||
                !requiresLocalSessionState(blockId)
            ) {
                return@withLock handleFailure(InvalidCanonicalTransitionException())
            }
            val currentAssessment = initial.assessMoveLater(blockId, moveStart, now())
            if (
                currentAssessment == null || !currentAssessment.fitsFirmHorizonDay ||
                currentAssessment.crossesUnrelaxableHardDeadline ||
                !currentAssessment.isCoveredBy(approval)
            ) {
                return@withLock handleFailure(MoveLaterRisksChangedException())
            }
            mutableState.value = CanonicalSyncState(
                CanonicalSyncPhase.SYNCING,
                "Saving a recurrence/session deferral…",
            )
            try {
                val receipt = plannerStore.deferLocalCanonicalSession(blockId, moveStart, approval)
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
        moveLaterStart: Instant? = null,
        moveLaterApproval: MoveLaterApprovalEnvelope? = null,
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
                configuration.withBindingOperation {
                val requestedBlock = initial.schedule.firstOrNull { it.id == blockId }
                    ?: throw InvalidCanonicalTransitionException()
                if (requestedBlock.status !in allowedStatuses) {
                    throw InvalidCanonicalTransitionException()
                }
                if (requestedBlock.canonicalItemId == null) {
                    throw InvalidCanonicalTransitionException()
                }
                val moveAssessment = moveLaterStart?.let { moveStart ->
                    initial.assessMoveLater(blockId, moveStart, now())?.takeIf { assessment ->
                        assessment.fitsFirmHorizonDay &&
                            !assessment.crossesUnrelaxableHardDeadline &&
                            assessment.isCoveredBy(moveLaterApproval)
                    } ?: throw MoveLaterRisksChangedException()
                }

                replaceCanonicalBlock(
                    configuration = configuration,
                    blockId = blockId,
                    targetStatus = targetStatus,
                    displayStatus = displayStatus,
                    pauseLabel = pauseLabel,
                    pauseMinutes = pauseMinutes,
                    deferUntil = deferUntil,
                    relaxDeadlineUntil = moveAssessment?.canonicalDeadlineRelaxation,
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
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
        val uncertainState = plannerStore.state.value
        val uncertain = uncertainState.pendingCanonicalMutation
        if (outcome == CanonicalRefreshOutcome.SUCCESS || uncertain == null) return outcome
        val previous = uncertainState.canonicalItems.firstOrNull {
            it.id == uncertain.itemId && it.revision == uncertain.expectedRevision
        }
        val requestedReplacement = runCatching {
            mutationJson.decodeFromString<ReplaceCanonicalItemRequest>(
                uncertain.replacementRequestJson,
            ).item
        }.getOrNull()
        val reconciled = refreshAndCompose()
        if (reconciled != CanonicalRefreshOutcome.SUCCESS) return outcome
        val authoritative = plannerStore.state.value.canonicalItems.firstOrNull {
            it.id == uncertain.itemId
        }
        return if (
            authoritative != null && authoritative.revision > uncertain.expectedRevision &&
            authoritative.status == uncertain.targetStatus &&
            authoritative.isSensitive == uncertain.targetIsSensitive &&
            previous != null && requestedReplacement != null &&
            matchesReplacement(authoritative, previous, requestedReplacement)
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
        relaxDeadlineUntil: Instant? = null,
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
            deadlineAt = relaxDeadlineUntil?.toString() ?: item.deadlineAt,
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
        deadlineAt: String? = item.deadlineAt,
    ) = CanonicalItemReplacement(
        isSensitive = targetIsSensitive,
        kind = item.kind,
        status = targetStatus,
        title = item.title,
        notes = item.notes,
        timezoneName = item.timezoneName,
        durationSeconds = item.durationSeconds,
        deadlineAt = deadlineAt,
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
                it.syncOrigin == origin &&
                    it.session.status in TERMINAL_CANONICAL_STATUSES &&
                    current.isNewestExecutionForProjection(it.session) &&
                    it.requiresCanonicalItemProjection &&
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
        forceCanonicalRebuild: Boolean = false,
    ): CanonicalDeltaSnapshot {
        val cached = plannerStore.state.value
        val sameBinding = cached.canonicalSyncOrigin == configuration.baseUrl.toString() &&
            cached.canonicalConfigurationId == configuration.configurationId
        val firstCursor = cached.canonicalDeltaCursor.takeIf {
            sameBinding && !forceCanonicalRebuild
        }
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

    private fun compositionPlanningZone(
        profile: ScheduleCompositionProfileSnapshot =
            plannerStore.state.value.scheduleCompositionProfile,
    ): ZoneId = try {
        profile.compositionZone(zoneId())
    } catch (error: ScheduleProfileExpansionException) {
        throw RemotePlannerMappingException(error)
    }

    private fun availabilityWithinHorizon(
        horizonStart: Instant,
        horizonEnd: Instant,
        planningZone: ZoneId,
        profile: ScheduleCompositionProfileSnapshot,
    ): List<ScheduleAvailabilityRequest> {
        return try {
            val expanded = profile.expandForComposition(
                fallbackZone = planningZone,
                horizonStart = horizonStart,
                horizonEnd = horizonEnd,
            )
            if (expanded.planningZone != planningZone) throw RemotePlannerMappingException()
            expanded.availability
        } catch (error: ScheduleProfileExpansionException) {
            throw RemotePlannerMappingException(error)
        }
    }

    private fun previewRequest(
        instant: Instant,
        planningZone: ZoneId,
        canonicalItems: List<CanonicalItemSnapshot>,
        syncOrigin: String,
        configurationId: String?,
        cachedState: com.greengolddog.dayweave.model.DayWeaveUiState =
            plannerStore.state.value,
    ): SchedulePreviewRequest {
        val date = instant.atZone(planningZone).toLocalDate()
        val cached = cachedState
        val profile = cached.scheduleCompositionProfile
        if (!profile.hasValidShape()) throw RemotePlannerMappingException()
        val horizonStart = localMinute(
            date = date,
            zone = planningZone,
            minute = 0,
            boundary = AvailabilityBoundary.START,
        )
        val horizonEnd = localMinute(
            date = date.plusDays(profile.firmHorizonDays.toLong()),
            zone = planningZone,
            minute = 0,
            boundary = AvailabilityBoundary.END,
        )
        val expandedProfile = try {
            profile.expandForComposition(
                fallbackZone = planningZone,
                horizonStart = horizonStart.toInstant(),
                horizonEnd = horizonEnd.toInstant(),
            ).also {
                if (it.planningZone != planningZone) throw RemotePlannerMappingException()
            }
        } catch (error: ScheduleProfileExpansionException) {
            throw RemotePlannerMappingException(error)
        }
        val availability = expandedProfile.availability
        val itemsById = canonicalItems.associateBy(CanonicalItemSnapshot::id)
        val revisions = canonicalItems.associate { it.id to it.revision }
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
            .filter { (occurrenceId, _) ->
                runCatching { UUID.fromString(occurrenceId).version() == 5 }
                    .getOrDefault(false)
            }
            .filter { (_, move) ->
                move.source?.let { source ->
                    source.itemId == move.itemId &&
                        revisions[move.itemId] == source.itemRevision
                } == true
            }
            .filter { (_, move) ->
                val movedAt = parseTimestamp(move.movedAt).toInstant()
                val moveEnd = parseTimestamp(move.endAt).toInstant()
                // A future move must suppress its original occurrence on every intervening
                // preview, then expire immediately after its destination day.
                movedAt < relevantOutcomeEnd && moveEnd >= horizonStart.toInstant()
            }
            .onEach { (occurrenceId, move) ->
                validateUuid(occurrenceId)
                validateUuid(move.itemId)
                val source = move.source ?: throw RemotePlannerMappingException()
                val nominalStart = parseTimestamp(source.nominalStart)
                if (source.itemId !in itemsById) throw RemotePlannerMappingException()
                if (
                    recurrenceIdentityObject(source.identityJson) == null ||
                    recurrenceIdentityType(source.identityJson) == "custom" ||
                    parseTimestamp(move.startAt) >= parseTimestamp(move.endAt) ||
                    nominalStart.toInstant() >= parseTimestamp(source.nominalEnd).toInstant() ||
                    source.itemRevision <= 0 || source.ordinal !in 0..UInt.MAX_VALUE.toLong() ||
                    source.localDate?.let { rawDate ->
                        runCatching {
                            LocalDate.parse(rawDate) == nominalStart.toLocalDate()
                        }.getOrDefault(false)
                    } == false
                ) {
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
        val previousSchedule = if (
            sameOrigin &&
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
                    val startInstant = parseTimestamp(start).toInstant()
                    val endInstant = parseTimestamp(end).toInstant()
                    if (
                        startInstant >= endInstant || endInstant <= horizonStart.toInstant() ||
                        startInstant >= horizonEnd.toInstant()
                    ) {
                        return@mapNotNull null
                    }
                    PreviousScheduleBlockRequest(
                        start = startInstant.toString(),
                        end = endInstant.toString(),
                        sessionIndex = block.sessionIndex ?: return@mapNotNull null,
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
            availability = availability,
            fixedBlocks = expandedProfile.fixedBlocks,
            config = ScheduleConfigRequest(
                slotGranularityMinutes = profile.slotGranularityMinutes,
                stabilityWeight = profile.stabilityWeight,
                defaultSoftWeight = profile.defaultSoftWeight,
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
                                            put(
                                                "source",
                                                buildJsonObject {
                                                    val source = requireNotNull(move.source)
                                                    put("item_revision", source.itemRevision)
                                                    put(
                                                        "identity",
                                                        requireNotNull(
                                                            recurrenceIdentityObject(
                                                                source.identityJson,
                                                            ),
                                                        ),
                                                    )
                                                    put("nominal_start", source.nominalStart)
                                                    put("nominal_end", source.nominalEnd)
                                                    put(
                                                        "local_date",
                                                        source.localDate?.let(::JsonPrimitive)
                                                            ?: JsonNull,
                                                    )
                                                    put("ordinal", source.ordinal)
                                                },
                                            )
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
        planningZone: ZoneId,
        expectedHorizonStart: Instant,
        expectedHorizonEnd: Instant,
        availability: List<ScheduleAvailabilityRequest>,
        inputDigestPattern: Regex = DIGEST_PATTERN,
        preservationState: com.greengolddog.dayweave.model.DayWeaveUiState? = null,
        allowExternalFixed: Boolean = false,
        requireExactConfiguredHorizon: Boolean = true,
    ): CanonicalPlanUpdate {
        val items = canonicalItems.associateBy(CanonicalItemSnapshot::id)
        if (
            preview.sourceItemCount !in 0..MAX_CANONICAL_ITEMS ||
            preview.sourceItemRevisions.size != preview.sourceItemCount ||
            preview.acceptedItemCount < 0 ||
            preview.acceptedItemCount + preview.rejectedItems.size != preview.sourceItemCount ||
            !preview.inputDigest.matches(inputDigestPattern) ||
            preview.rejectedItems.size > MAX_CANONICAL_ITEMS ||
            preview.plan.blocks.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.unscheduled.size > MAX_CANONICAL_ITEMS ||
            preview.ignoredPreviousAssignments.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.decisions.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.violations.size > MAX_SCHEDULE_BLOCKS ||
            preview.plan.occurrences.size > MAX_SCHEDULE_BLOCKS ||
            preview.manualPlacementAssessments.size > MAX_MANUAL_PLACEMENTS
        ) {
            throw RemotePlannerMappingException()
        }
        validateManualPlacementAssessments(preview, items)
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
        val horizonShapeIsValid = if (requireExactConfiguredHorizon) {
            exactFirmHorizonDayCount(
                expectedHorizonStart,
                expectedHorizonEnd,
                planningZone,
            ) != null
        } else {
            expectedHorizonStart < expectedHorizonEnd &&
                Duration.between(expectedHorizonStart, expectedHorizonEnd) <=
                MAX_PUBLISHED_REPLICA_HORIZON_DURATION
        }
        if (
            !horizonShapeIsValid ||
            parseTimestamp(preview.plan.asOf).toInstant() != generatedAt ||
            parseTimestamp(preview.plan.horizonStart).toInstant() != expectedHorizonStart ||
            parseTimestamp(preview.plan.horizonEnd).toInstant() != expectedHorizonEnd
        ) {
            throw RemotePlannerMappingException()
        }

        val occurrencesById = preview.plan.occurrences.associateBy { occurrence ->
            validateOccurrenceUuid(occurrence.id)
            validateUuid(occurrence.seriesItemId)
            if (validatedRecurrenceIdentityJson(occurrence.identity) == null) {
                throw RemotePlannerMappingException()
            }
            if (
                occurrence.seriesItemId !in items || occurrence.ordinal !in 0..UInt.MAX_VALUE.toLong() ||
                items[occurrence.seriesItemId]?.recurrenceJson == null ||
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
                val localDate = runCatching { LocalDate.parse(raw) }.getOrElse {
                    throw RemotePlannerMappingException(it)
                }
                if (occurrence.seriesItemId !in items || localDate != nominalStart.toLocalDate()) {
                    throw RemotePlannerMappingException()
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
                work.itemId !in items || work.remaining !in 0..MAX_WIRE_U32 ||
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

        // Validate occurrence ownership before mapping. Non-executable Calendar context is
        // deliberately stripped of execution identity below, but its remote identity must still
        // be internally consistent with the published occurrence table.
        preview.plan.blocks.forEach { block ->
            block.occurrenceId?.let { occurrenceId ->
                val occurrence = occurrencesById[occurrenceId]
                    ?: throw RemotePlannerMappingException()
                val itemId = block.itemId ?: throw RemotePlannerMappingException()
                if (!itemBelongsToSeries(itemId, occurrence.seriesItemId, items)) {
                    throw RemotePlannerMappingException()
                }
            }
        }
        validateRemotePlanGeometry(
            preview = preview,
            items = items,
            horizonStart = expectedHorizonStart,
            horizonEnd = expectedHorizonEnd,
        )

        val externalBlockIds = preview.plan.blocks.mapNotNull { it.externalBlockId }
        if (externalBlockIds.distinct().size != externalBlockIds.size) {
            throw RemotePlannerMappingException()
        }
        val schedule = preview.plan.blocks.map { block ->
            mapScheduleBlock(
                block = block,
                items = items,
                planningZone = planningZone,
                horizonStart = expectedHorizonStart,
                horizonEnd = expectedHorizonEnd,
                allowExternalFixed = allowExternalFixed,
            )
        }.let {
            preserveLocalSessionState(
                composed = it,
                items = items,
                syncOrigin = syncOrigin,
                cached = preservationState ?: plannerStore.state.value,
            )
        }
        if (schedule.sumOf(::estimatedScheduleItemBytes) > MAX_SCHEDULE_CACHE_ESTIMATED_BYTES) {
            throw RemotePlannerMappingException()
        }
        if (schedule.map(ScheduleItem::id).distinct().size != schedule.size) {
            throw RemotePlannerMappingException()
        }
        val canonicalSchedule = schedule.filter { it.canonicalItemId != null }
        if (
            canonicalSchedule.map {
                Triple(it.canonicalItemId, it.occurrenceId, it.sessionIndex)
            }.distinct().size != canonicalSchedule.size ||
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
                violation.itemIds.distinct().size != violation.itemIds.size ||
                violation.occurrenceIds.distinct().size != violation.occurrenceIds.size ||
                violation.message.isBlank() ||
                violation.message.length > MAX_VIOLATION_MESSAGE_CHARS
            ) {
                throw RemotePlannerMappingException()
            }
            violation.itemIds.forEach { id ->
                validateUuid(id)
                if (id !in items) throw RemotePlannerMappingException()
            }
            violation.occurrenceIds.forEach { occurrenceId ->
                validateOccurrenceUuid(occurrenceId)
                val occurrence = occurrencesById[occurrenceId]
                    ?: throw RemotePlannerMappingException()
                if (violation.itemIds.none { itemId ->
                        itemBelongsToSeries(itemId, occurrence.seriesItemId, items)
                    }
                ) {
                    throw RemotePlannerMappingException()
                }
            }
            val violationStart = violation.start?.let(::parseTimestamp)
            val violationEnd = violation.end?.let(::parseTimestamp)
            if (
                (violationStart == null) != (violationEnd == null) ||
                violationStart != null && violationEnd != null && violationStart >= violationEnd
            ) {
                throw RemotePlannerMappingException()
            }
            violation.message
        }
        val score = preview.plan.score
        if (
            score.scheduledMinutes !in 0..MAX_WIRE_U32 ||
            score.unscheduledMinutes !in 0..MAX_WIRE_U32 ||
            score.movedMinutes !in 0..MAX_WIRE_U32
        ) {
            throw RemotePlannerMappingException()
        }
        val totalWork = score.scheduledMinutes + score.unscheduledMinutes
        val completionScore = if (totalWork == 0L) {
            100
        } else {
            ((score.scheduledMinutes * 100L) / totalWork).toInt()
        }
        val penalty = (score.softPenalty / 100uL)
            .coerceAtMost(MAX_SCORE_PENALTY.toULong()).toInt()
        val dayScore = (completionScore - penalty).coerceIn(0, 100)
        val protectedMinutes = protectedMinutes(
            schedule,
            availability,
            expectedHorizonStart,
            expectedHorizonEnd,
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
            occurrenceSources = preview.plan.occurrences.associate { occurrence ->
                val series = items[occurrence.seriesItemId]
                    ?: throw RemotePlannerMappingException()
                occurrence.id to RecurrenceOccurrenceSourceSnapshot(
                    itemId = occurrence.seriesItemId,
                    itemRevision = series.revision,
                    identityJson = validatedRecurrenceIdentityJson(occurrence.identity)
                        ?: throw RemotePlannerMappingException(),
                    nominalStart = occurrence.nominalStart,
                    nominalEnd = occurrence.nominalEnd,
                    localDate = occurrence.localDate,
                    ordinal = occurrence.ordinal,
                )
            },
            message = message,
        )
    }

    /** Rechecks the core's non-overlap and score invariants before any replica can be installed. */
    private fun validateRemotePlanGeometry(
        preview: RemoteSchedulePreview,
        items: Map<String, CanonicalItemSnapshot>,
        horizonStart: Instant,
        horizonEnd: Instant,
    ) {
        var latestAnyEnd: Instant? = null
        var latestPlannedEnd: Instant? = null
        var scheduledMinutes = 0L
        val ordered = preview.plan.blocks.sortedWith(
            compareBy<RemoteScheduleBlock>(
                { parseTimestamp(it.start).toInstant() },
                { parseTimestamp(it.end).toInstant() },
                RemoteScheduleBlock::id,
            ),
        )
        ordered.forEach { block ->
            val start = parseTimestamp(block.start).toInstant()
            val end = parseTimestamp(block.end).toInstant()
            if (start >= end || end <= horizonStart || start >= horizonEnd) {
                throw RemotePlannerMappingException()
            }
            when (block.kind) {
                "planned", "pinned" -> {
                    val item = block.itemId?.let(items::get)
                        ?: throw RemotePlannerMappingException()
                    if (
                        !item.isExecutable || block.externalBlockId != null ||
                        block.kind == "planned" &&
                        (start < horizonStart || end > horizonEnd) ||
                        block.kind == "planned" && latestAnyEnd?.let { it > start } == true ||
                        block.kind == "pinned" && latestPlannedEnd?.let { it > start } == true
                    ) {
                        throw RemotePlannerMappingException()
                    }
                    val duration = Duration.between(start, end)
                    if (duration.nano != 0 || duration.seconds % 60L != 0L) {
                        throw RemotePlannerMappingException()
                    }
                    scheduledMinutes = try {
                        Math.addExact(scheduledMinutes, duration.toMinutes())
                    } catch (error: ArithmeticException) {
                        throw RemotePlannerMappingException(error)
                    }
                    if (block.kind == "planned") latestPlannedEnd = end
                }
                "calendar_event" -> {
                    val item = block.itemId?.let(items::get)
                        ?: throw RemotePlannerMappingException()
                    if (
                        item.kind != "event" || block.externalBlockId != null ||
                        latestPlannedEnd?.let { it > start } == true
                    ) {
                        throw RemotePlannerMappingException()
                    }
                }
                "external_fixed" -> if (
                    block.itemId != null || block.occurrenceId != null ||
                    block.externalBlockId == null || block.id != block.externalBlockId ||
                    latestPlannedEnd?.let { it > start } == true
                ) {
                    throw RemotePlannerMappingException()
                }
                else -> throw RemotePlannerMappingException()
            }
            if (latestAnyEnd == null || end > requireNotNull(latestAnyEnd)) latestAnyEnd = end
        }
        val unscheduledMinutes = preview.plan.unscheduled.fold(0L) { total, work ->
            try {
                Math.addExact(total, work.remaining)
            } catch (error: ArithmeticException) {
                throw RemotePlannerMappingException(error)
            }
        }
        if (
            preview.plan.score.scheduledMinutes != scheduledMinutes.coerceAtMost(MAX_WIRE_U32) ||
            preview.plan.score.unscheduledMinutes != unscheduledMinutes.coerceAtMost(MAX_WIRE_U32)
        ) {
            throw RemotePlannerMappingException()
        }
    }

    private fun mapScheduleBlock(
        block: RemoteScheduleBlock,
        items: Map<String, CanonicalItemSnapshot>,
        planningZone: ZoneId,
        horizonStart: Instant,
        horizonEnd: Instant,
        allowExternalFixed: Boolean = false,
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
        if (block.kind !in SUPPORTED_BLOCK_KINDS) {
            throw RemotePlannerMappingException()
        }
        val actualStart = parseTimestamp(block.start).atZoneSameInstant(planningZone)
        val actualEnd = parseTimestamp(block.end).atZoneSameInstant(planningZone)
        if (horizonStart >= horizonEnd) {
            throw RemotePlannerMappingException()
        }
        val actualStartInstant = actualStart.toInstant()
        val actualEndInstant = actualEnd.toInstant()
        if (actualEndInstant <= horizonStart || actualStartInstant >= horizonEnd) {
            throw RemotePlannerMappingException()
        }
        // Keep one immutable geometry across preview, publication, and replica refresh. Calendar
        // presentation clips these exact bounds to the visible horizon without mutating authority.
        val start = actualStart
        val end = actualEnd
        val exactDuration = Duration.between(start.toInstant(), end.toInstant())
        if (exactDuration.isZero || exactDuration.isNegative) {
            throw RemotePlannerMappingException()
        }
        val durationMinutesLong = try {
            val roundedSeconds = Math.addExact(
                exactDuration.seconds,
                if (exactDuration.nano == 0) 0L else 1L,
            )
            Math.addExact(roundedSeconds, 59L) / 60L
        } catch (error: ArithmeticException) {
            throw RemotePlannerMappingException(error)
        }
        val startMinute = start.hour * 60 + start.minute
        if (
            durationMinutesLong <= 0L || durationMinutesLong > Int.MAX_VALUE - startMinute
        ) {
            throw RemotePlannerMappingException()
        }
        val durationMinutes = durationMinutesLong.toInt()
        val isExternal = block.kind == "external_fixed"
        if (
            isExternal != (block.externalBlockId != null) ||
            isExternal && (
                !allowExternalFixed || block.itemId != null || block.occurrenceId != null ||
                    block.id != block.externalBlockId
                )
        ) {
            throw RemotePlannerMappingException()
        }
        val canonical = block.itemId?.let { itemId ->
            validateUuid(itemId)
            items[itemId] ?: throw RemotePlannerMappingException()
        }
        if (!isExternal && (canonical == null || block.externalBlockId != null)) {
            throw RemotePlannerMappingException()
        }
        if (
            canonical != null &&
            ((!canonical.isExecutable && block.kind != "calendar_event") ||
                block.title != canonical.title ||
                block.isSensitive != effectiveSensitivity(canonical, items))
        ) {
            throw RemotePlannerMappingException()
        }
        val itemKind = canonical?.let { mapItemKind(it.kind) } ?: ItemKind.EVENT
        val status = canonical?.let { mapItemStatus(it.status) } ?: ItemStatus.SCHEDULED
        val splitType = canonical?.splitPolicyJson
            ?.let(JsonObjectParser::parse)
            ?.get("type")
            ?.let { it as? JsonPrimitive }
            ?.contentOrNull
        val constraints = canonical?.flexibleConstraintsJson?.let(JsonObjectParser::parse)
        val energyValue = constraints?.get("energy")?.let(::energyValue)
        val hard = block.kind in setOf("pinned", "calendar_event", "external_fixed")
        return ScheduleItem(
            id = block.id,
            isSensitive = block.isSensitive,
            title = block.title,
            kind = itemKind,
            startMinute = startMinute,
            durationMinutes = durationMinutes,
            status = status,
            project = canonical?.parentId?.let { items[it]?.title },
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
            // Non-executable Calendar context remains visible but cannot acquire execution
            // authority merely because it was present in a published schedule snapshot.
            canonicalItemId = canonical?.takeIf { it.isExecutable }?.id,
            occurrenceId = block.occurrenceId.takeIf { canonical?.isExecutable == true },
            canonicalRevision = canonical?.takeIf { it.isExecutable }?.revision,
            sessionIndex = block.sessionIndex,
            // Fresh composition display geometry is clipped only to the complete captured firm
            // horizon. Publication/execution identity always retains the server's exact bounds;
            // replica display geometry also remains exact so its immutable proof is reproducible.
            absoluteStartAt = actualStart.toInstant().toString(),
            absoluteEndAt = actualEnd.toInstant().toString(),
            planningZoneId = planningZone.id,
            canonicalBlockKind = block.kind,
        )
    }

    private fun validateManualPlacementAssessments(
        preview: RemoteSchedulePreview,
        items: Map<String, CanonicalItemSnapshot>,
    ) {
        val occurrences = preview.plan.occurrences.associateBy { it.id }
        val placementIds = mutableSetOf<String>()
        var violationCount = 0
        var conflictFactCount = 0
        preview.manualPlacementAssessments.forEach { assessment ->
            validateUuid(assessment.placementId)
            if (
                !placementIds.add(assessment.placementId) ||
                !assessment.environmentDigest.matches(DIGEST_PATTERN) ||
                !assessment.approvalDigest.matches(DIGEST_PATTERN) ||
                assessment.approvalRequired && assessment.violations.isEmpty() ||
                assessment.violations.size > MAX_MANUAL_PLACEMENT_VIOLATIONS
            ) {
                throw RemotePlannerMappingException()
            }
            assessment.violations.forEach { violation ->
                if (
                    violationCount >= MAX_MANUAL_PLACEMENT_VIOLATIONS ||
                    violation.conflictingBlocks.size >
                    MAX_MANUAL_PLACEMENT_CONFLICT_FACTS - conflictFactCount
                ) {
                    throw RemotePlannerMappingException()
                }
                violationCount += 1
                conflictFactCount += violation.conflictingBlocks.size
                if (
                    violation.code !in SUPPORTED_MANUAL_PLACEMENT_VIOLATION_CODES ||
                    violation.message.isBlank() ||
                    violation.message.length > MAX_VIOLATION_MESSAGE_CHARS ||
                    violation.itemIds.size > MAX_CANONICAL_ITEMS ||
                    violation.occurrenceIds.size > MAX_SCHEDULE_BLOCKS ||
                    violation.conflictingBlockIds.size > MAX_SCHEDULE_BLOCKS ||
                    violation.conflictingBlocks.size > MAX_SCHEDULE_BLOCKS ||
                    violation.itemIds.distinct().size != violation.itemIds.size ||
                    violation.occurrenceIds.distinct().size != violation.occurrenceIds.size ||
                    violation.conflictingBlockIds.distinct().size !=
                    violation.conflictingBlockIds.size ||
                    violation.conflictingBlocks.map { it.blockId }.distinct().size !=
                    violation.conflictingBlocks.size
                ) {
                    throw RemotePlannerMappingException()
                }
                violation.itemIds.forEach { id ->
                    validateUuid(id)
                    if (id !in items) throw RemotePlannerMappingException()
                }
                violation.occurrenceIds.forEach { id ->
                    validateOccurrenceUuid(id)
                    val occurrence = occurrences[id]
                        ?: throw RemotePlannerMappingException()
                    if (violation.itemIds.none { itemId ->
                            itemBelongsToSeries(itemId, occurrence.seriesItemId, items)
                        }
                    ) {
                        throw RemotePlannerMappingException()
                    }
                }
                if (
                    violation.conflictingBlockIds.toSet() !=
                    violation.conflictingBlocks.mapTo(hashSetOf()) { it.blockId }
                ) {
                    throw RemotePlannerMappingException()
                }
                val start = parseTimestamp(violation.start)
                val end = parseTimestamp(violation.end)
                if (start >= end) throw RemotePlannerMappingException()
                validateManualPlacementBoundaries(violation, start, end)
                violation.conflictingBlocks.forEach { conflict ->
                    validateUuid(conflict.blockId)
                    val conflictItem = conflict.itemId?.let { id ->
                        validateUuid(id)
                        items[id] ?: throw RemotePlannerMappingException()
                    }
                    conflict.occurrenceId?.let { id ->
                        validateOccurrenceUuid(id)
                        val occurrence = occurrences[id]
                            ?: throw RemotePlannerMappingException()
                        if (
                            conflictItem == null ||
                            !itemBelongsToSeries(conflictItem.id, occurrence.seriesItemId, items)
                        ) {
                            throw RemotePlannerMappingException()
                        }
                    }
                    conflict.externalBlockId?.let(::validateUuid)
                    if (
                        conflict.kind !in SUPPORTED_BLOCK_KINDS ||
                        (conflict.kind == "external_fixed") !=
                        (conflict.externalBlockId != null) ||
                        conflict.kind == "external_fixed" &&
                        conflict.blockId != conflict.externalBlockId ||
                        conflict.kind == "external_fixed" &&
                        (conflict.itemId != null || conflict.occurrenceId != null) ||
                        conflict.kind != "external_fixed" && conflictItem == null ||
                        conflict.kind in setOf("planned", "pinned") &&
                        conflictItem?.isExecutable != true ||
                        conflict.kind == "calendar_event" && conflictItem?.kind != "event" ||
                        parseTimestamp(conflict.start) >= parseTimestamp(conflict.end)
                    ) {
                        throw RemotePlannerMappingException()
                    }
                }
            }
        }
    }

    private fun validateManualPlacementBoundaries(
        violation: com.greengolddog.dayweave.network.RemoteManualPlacementViolation,
        start: OffsetDateTime,
        end: OffsetDateTime,
    ) {
        val boundaryStart = violation.boundaryStart?.let(::parseTimestamp)
        val boundaryEnd = violation.boundaryEnd?.let(::parseTimestamp)
        val valid = when (violation.code) {
            "earliest_start", "minimum_notice" ->
                boundaryStart != null && boundaryEnd == null && start < boundaryStart
            "latest_finish" ->
                boundaryStart == null && boundaryEnd != null && end > boundaryEnd
            "preferred_absolute_window" ->
                boundaryStart != null && boundaryEnd != null && boundaryStart < boundaryEnd
            "forbidden_window" ->
                boundaryStart != null && boundaryEnd != null && boundaryStart < boundaryEnd &&
                    start < boundaryEnd && boundaryStart < end
            "buffer_compressed" ->
                boundaryStart != null && boundaryEnd != null && boundaryStart < boundaryEnd &&
                    boundaryStart <= start && boundaryEnd >= end
            else -> boundaryStart == null && boundaryEnd == null
        }
        if (!valid) throw RemotePlannerMappingException()
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
        cached: com.greengolddog.dayweave.model.DayWeaveUiState,
    ): List<ScheduleItem> {
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
        availability: List<ScheduleAvailabilityRequest>,
        horizonStart: Instant,
        horizonEnd: Instant,
    ): Int {
        val availableRanges = availability.mapNotNull { window ->
            val exactStart = parseTimestamp(window.start).toInstant()
            val exactEnd = parseTimestamp(window.end).toInstant()
            if (exactStart >= exactEnd) throw RemotePlannerMappingException()
            val start = exactStart.coerceAtLeast(horizonStart)
            val end = exactEnd.coerceAtMost(horizonEnd)
            if (start >= end) return@mapNotNull null
            start to end
        }.mergedInstantRanges()
        val scheduledRanges = schedule.mapNotNull { block ->
            val exactStart = block.absoluteStartAt ?: return@mapNotNull null
            val exactEnd = block.absoluteEndAt ?: return@mapNotNull null
            val start = parseTimestamp(exactStart).toInstant()
            val end = parseTimestamp(exactEnd).toInstant()
            if (end > start) start to end else null
        }.mergedInstantRanges()
        var freeWholeMinutes = 0L
        var firstRelevantScheduledRange = 0
        availableRanges.forEach { (availableStart, availableEnd) ->
            while (
                firstRelevantScheduledRange < scheduledRanges.size &&
                scheduledRanges[firstRelevantScheduledRange].second <= availableStart
            ) {
                firstRelevantScheduledRange += 1
            }
            var cursor = availableStart
            var scheduledIndex = firstRelevantScheduledRange
            while (
                scheduledIndex < scheduledRanges.size &&
                scheduledRanges[scheduledIndex].first < availableEnd
            ) {
                val (scheduledStart, scheduledEnd) = scheduledRanges[scheduledIndex]
                val blockedStart = scheduledStart.coerceAtLeast(availableStart)
                val blockedEnd = scheduledEnd.coerceAtMost(availableEnd)
                if (cursor < blockedStart) {
                    freeWholeMinutes += Duration.between(cursor, blockedStart).toMinutes()
                }
                if (blockedEnd > cursor) cursor = blockedEnd
                if (cursor >= availableEnd) break
                scheduledIndex += 1
            }
            if (cursor < availableEnd) {
                freeWholeMinutes += Duration.between(cursor, availableEnd).toMinutes()
            }
        }
        return freeWholeMinutes
            .coerceAtMost(Int.MAX_VALUE.toLong())
            .toInt()
    }

    private fun List<Pair<Instant, Instant>>.mergedInstantRanges(): List<Pair<Instant, Instant>> {
        if (isEmpty()) return emptyList()
        val merged = mutableListOf<Pair<Instant, Instant>>()
        var activeStart: Instant? = null
        var activeEnd: Instant? = null
        for ((start, end) in sortedWith(compareBy({ it.first }, { it.second }))) {
            if (activeStart == null) {
                activeStart = start
                activeEnd = end
            } else if (start <= requireNotNull(activeEnd)) {
                if (end > requireNotNull(activeEnd)) activeEnd = end
            } else {
                merged += requireNotNull(activeStart) to requireNotNull(activeEnd)
                activeStart = start
                activeEnd = end
            }
        }
        if (activeStart != null) {
            merged += requireNotNull(activeStart) to requireNotNull(activeEnd)
        }
        return merged
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
        val hasCanonicalCache = current.canonicalSyncOrigin != null ||
            current.canonicalDeltaCursor != null || current.canonicalItems.isNotEmpty() ||
            current.canonicalRecentlyDeleted.isNotEmpty() ||
            current.pendingCanonicalAuthoringMutations.isNotEmpty() ||
            current.pendingCanonicalMutation != null ||
            current.publishedScheduleRevision != null ||
            current.publishedScheduleProof != null
        val pendingPublicationMismatch = current.pendingSchedulePublication?.let { pending ->
            pending.syncOrigin != origin || pending.configurationId != configurationId
        } ?: false
        val proposalBindingMismatch = current.pendingProposalApplicationMutation?.let { pending ->
            pending.syncOrigin != origin || pending.configurationId != configurationId
        } ?: current.proposalApplications.values.any { receipt ->
            receipt.syncOrigin != origin || receipt.configurationId != configurationId
        }
        val hasExecutionState = current.canonicalExecutionSyncOrigin != null ||
            current.canonicalExecutionSession != null ||
            current.canonicalExecutionHistoryWindow.isNotEmpty() ||
            current.canonicalExecutionHistoryWindowRevision != null ||
            current.canonicalExecutionHistoryContinuityEstablished ||
            current.canonicalExecutionHistoryVerified ||
            current.terminalExecutionOutcomes.isNotEmpty() ||
            current.pendingExecutionCommand != null ||
            current.pendingExecutionDeferIntent != null
        val canonicalMismatch = pendingPublicationMismatch || proposalBindingMismatch || hasCanonicalCache &&
            (current.canonicalSyncOrigin != origin ||
                current.canonicalConfigurationId != configurationId)
        val executionMismatch = hasExecutionState &&
            (current.canonicalExecutionSyncOrigin != origin ||
                current.canonicalExecutionConfigurationId != configurationId)
        if (!canonicalMismatch && !executionMismatch) return
        if (plannerStore.hasCredentialReplacementBlocker()) {
            throw CanonicalConfigurationChangedException()
        }
        withTimedBreakNotificationBarrier {
            val receipt = plannerStore.abandonCanonicalConnection()
            if (receipt == null || !receipt.awaitDurable()) throw LocalPlannerStorageException()
        }
    }

    private fun parseJsonObject(raw: String): JsonObject = JsonObjectParser.parse(raw)

    private fun handleFailure(error: Throwable): CanonicalRefreshOutcome {
        if (error is CancellationException) {
            mutableState.value = stateFrom(credentialStore.snapshot())
            throw error
        }
        if (error is ApiBindingChangedException) {
            val snapshot = credentialStore.snapshot()
            mutableState.value = stateFrom(snapshot)
            return if (snapshot.hasBearerToken) {
                CanonicalRefreshOutcome.CONFIGURATION_ERROR
            } else {
                CanonicalRefreshOutcome.NOT_CONFIGURED
            }
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
            is PlannerApiException.CanonicalMutationInProgress -> Triple(
                CanonicalSyncPhase.OFFLINE,
                "The saved canonical change is still being committed. Its exact retry was kept.",
                CanonicalRefreshOutcome.RETRYABLE_SERVER_FAILURE,
            )
            is PlannerApiException.CanonicalMutationRejected -> Triple(
                CanonicalSyncPhase.ERROR,
                "The saved canonical change needs conflict review.",
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
            is SchedulePublicationContractException,
            is CanonicalAuthoringResponseException,
            -> Triple(
                CanonicalSyncPhase.ERROR,
                "The server planner contract is incompatible with this DayWeave build.",
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            )
            is LocalScheduleCompositionProtocolException,
            is LocalScheduleCompositionRejectedException,
            is LocalScheduleCompositionRequestException,
            is LocalScheduleCompositionRequestTooLargeException,
            -> Triple(
                CanonicalSyncPhase.ERROR,
                "The bundled scheduler rejected or returned an invalid local composition.",
                CanonicalRefreshOutcome.PROTOCOL_FAILURE,
            )
            is LocalCompositionGenerationChangedException -> Triple(
                CanonicalSyncPhase.READY,
                "Planner inputs changed; the on-device result was discarded safely.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is LocalCompositionUnavailableException -> Triple(
                CanonicalSyncPhase.READY,
                error.message ?: "On-device composition is not available yet.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is CurrentScheduleReplicaBlockedException -> Triple(
                CanonicalSyncPhase.READY,
                "Finish or reconcile the pending canonical action before installing a remote schedule.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is SchedulePublicationRecoveryExhaustedException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Canonical items kept changing during publication. Recompose again to publish a fresh schedule.",
                CanonicalRefreshOutcome.STALE_REVISION,
            )
            is InvalidCanonicalTransitionException -> Triple(
                CanonicalSyncPhase.ERROR,
                "This action no longer matches the cached item state. Recompose and try again.",
                CanonicalRefreshOutcome.INVALID_LOCAL_STATE,
            )
            is MoveLaterRisksChangedException -> Triple(
                CanonicalSyncPhase.ERROR,
                "The placement risks changed after review. Review the current warning before moving.",
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
                "A server outcome could not be recorded in encrypted local storage. Restart " +
                    "before continuing so the durable recovery journal remains authoritative.",
                CanonicalRefreshOutcome.LOCAL_STORAGE_FAILURE,
            )
            is PlannerApiException.Http -> if (
                error.statusCode in setOf(408, 425, 429) || error.statusCode in 500..599
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
                "The firm horizon could not be recomposed; the encrypted cached plan was kept.",
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

    private data class AcceptedCanonicalPreview(
        val request: SchedulePreviewRequest,
        val update: CanonicalPlanUpdate,
    )

    private data class CanonicalAuthoringPushSummary(
        val appliedCount: Int = 0,
        val conflictedCount: Int = 0,
        val deferredCount: Int = 0,
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

    private enum class CanonicalAuthoringCacheResolution {
        NO_EVIDENCE,
        APPLIED,
        CONFLICTED,
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

    private enum class AvailabilityBoundary {
        START,
        END,
    }

    private class RemotePlannerMappingException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid remote planner contract", cause)

    private class RemoteSnapshotChangedException :
        IllegalArgumentException("Canonical snapshot changed during composition")

    private class SchedulePublicationContractException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid schedule publication contract", cause)

    private class CanonicalAuthoringResponseException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid canonical authoring response", cause)

    private class ReplayedSchedulePublicationNeedsFreshSnapshotException :
        IllegalStateException("Replayed schedule publication requires a fresh snapshot")

    private class StaleSchedulePublicationRejectedException(
        val expected: PendingSchedulePublication,
        cause: Throwable,
    ) : IllegalStateException("Schedule publication was rejected as stale", cause)

    private class SchedulePublicationRecoveryExhaustedException :
        IllegalStateException("Schedule publication changed repeatedly")

    private class InvalidCanonicalTransitionException :
        IllegalStateException("Invalid canonical transition")

    private class MoveLaterRisksChangedException :
        IllegalStateException("Move-later risks changed after review")

    private class LocalPlannerStorageException :
        IllegalStateException("Canonical mutation was not durably persisted")

    private class CanonicalConfigurationChangedException :
        IllegalStateException("Canonical API configuration changed")

    private class CanonicalMutationNeedsReconciliationException :
        IllegalStateException("Canonical mutation needs reconciliation")

    private class RecurrenceContextCapacityException :
        IllegalStateException("Recurrence preview context capacity exceeded")

    private class LocalCompositionUnavailableException(message: String) :
        IllegalStateException(message)

    private class LocalCompositionGenerationChangedException :
        IllegalStateException("Local composition input generation changed")

    private class CurrentScheduleReplicaBlockedException :
        IllegalStateException("A pending canonical action blocks schedule replication")

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
        private val MAX_PUBLISHED_REPLICA_HORIZON_DURATION: Duration = Duration.ofDays(90)
        private val SERVER_NAMED_TIMEZONE_IDS = ZoneId.getAvailableZoneIds()
        private const val MAX_CANONICAL_ITEMS = 10_000
        private const val MAX_SCHEDULE_PUBLICATION_RECOVERY_RECOMPOSITIONS = 1
        private const val MAX_CANONICAL_CACHE_ESTIMATED_BYTES = 24L * 1024L * 1024L
        private const val CANONICAL_ITEM_OBJECT_OVERHEAD_BYTES = 512L
        private const val MAX_DELTA_PAGES = 512
        private const val MAX_SNAPSHOT_ATTEMPTS = 3
        private const val MAX_DELTA_PAGE_SIZE = 50
        private const val MAX_DELTA_CHANGES = MAX_DELTA_PAGES * MAX_DELTA_PAGE_SIZE
        private const val MAX_SCHEDULE_BLOCKS = 10_000
        private const val MAX_SCHEDULE_CACHE_ESTIMATED_BYTES = 8L * 1024L * 1024L
        private const val SCHEDULE_ITEM_OBJECT_OVERHEAD_BYTES = 512L
        private const val MAX_PENDING_MUTATION_JSON_CHARS = 2 * 1024 * 1024
        private const val MAX_RECURRENCE_CONTEXT_IDS = 9_000
        private const val OUTCOME_CONTEXT_MARGIN_SECONDS = 2L * 24L * 60L * 60L
        private const val MAX_WIRE_U32 = 4_294_967_295L
        private const val MAX_SCORE_PENALTY = 20
        private const val MAX_CURSOR_CHARS = 4_096
        private const val MAX_PAUSE_MINUTES = 24 * 60
        private const val FREEZE_HORIZON_SECONDS = 2L * 60L * 60L
        private const val MAX_TITLE_CHARS = 500
        private const val MAX_NOTES_CHARS = 100_000
        private const val MAX_RECURRENCE_BYTES = 16 * 1024
        private const val MAX_CONSTRAINT_BYTES = 32 * 1024
        private const val MAX_VIOLATION_MESSAGE_CHARS = 2_000
        private const val MAX_REMOTE_MESSAGE_CHARS = 4_000
        private const val MAX_BLOCK_EXPLANATIONS = 64
        private const val MAX_MANUAL_PLACEMENTS = 64
        private const val MAX_MANUAL_PLACEMENT_VIOLATIONS = 4_096
        private const val MAX_MANUAL_PLACEMENT_CONFLICT_FACTS = 4_096
        private const val MAX_PERSISTED_VIOLATION_MESSAGES = 100
        private const val SCHEDULE_PUBLICATION_JOURNAL_VERSION = 1
        private const val PUBLICATION_CLOCK_SKEW_SECONDS = 5 * 60L
        private val NIL_UUID = UUID(0L, 0L)
        private val DIGEST_PATTERN = Regex("^sha256:[0-9a-f]{64}$")
        private val LOCAL_FINGERPRINT_PATTERN = Regex("^local-sha256:[0-9a-f]{64}$")
        private const val EMPTY_SHA256_FINGERPRINT =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
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
        private val SUPPORTED_MANUAL_PLACEMENT_VIOLATION_CODES = setOf(
            "outside_availability",
            "earliest_start",
            "latest_finish",
            "minimum_notice",
            "allowed_weekday",
            "preferred_daily_window",
            "preferred_absolute_window",
            "forbidden_window",
            "required_context",
            "required_location",
            "required_capabilities",
            "energy",
            "dependency",
            "maximum_daily_work",
            "maximum_weekly_work",
            "buffer_compressed",
            "immutable_overlap",
        )

        private fun stateFrom(snapshot: ApiConnectionSnapshot): CanonicalSyncState = when {
            snapshot.baseUrl == null -> CanonicalSyncState(
                CanonicalSyncPhase.NOT_CONFIGURED,
                "Add an HTTPS DayWeave API URL and bearer token to compose your canonical plan.",
            )
            !snapshot.hasBearerToken -> CanonicalSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Add a bearer token to sync canonical items and compose the firm horizon.",
            )
            else -> CanonicalSyncState(
                CanonicalSyncPhase.READY,
                "Ready to sync canonical items and compose the firm horizon.",
            )
        }

        private fun mapCanonicalItem(
            remote: RemoteCanonicalItem,
            requireActive: Boolean = true,
        ): CanonicalItemSnapshot {
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
                remote.revision <= 0 || requireActive && remote.deletedAt != null ||
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
            remote.deletedAt?.let(::parseTimestamp)
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

        private fun validateOccurrenceUuid(raw: String) {
            validateUuid(raw)
            if (UUID.fromString(raw).version() != 5) throw RemotePlannerMappingException()
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

        private fun localMinute(
            date: LocalDate,
            zone: ZoneId,
            minute: Int,
            boundary: AvailabilityBoundary,
        ): ZonedDateTime {
            val isNextLocalStartOfDay = minute == MINUTES_PER_DAY
            val localDateTime = if (isNextLocalStartOfDay) {
                date.plusDays(1).atStartOfDay()
            } else {
                date.atTime(LocalTime.of(minute / 60, minute % 60))
            }
            val validOffsets = zone.rules.getValidOffsets(localDateTime)
            if (validOffsets.isEmpty()) throw RemotePlannerMappingException()
            val effectiveBoundary = if (isNextLocalStartOfDay) {
                AvailabilityBoundary.START
            } else {
                boundary
            }
            val offset = when (effectiveBoundary) {
                AvailabilityBoundary.START -> validOffsets.last()
                AvailabilityBoundary.END -> validOffsets.first()
            }
            return ZonedDateTime.ofStrict(localDateTime, offset, zone)
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
