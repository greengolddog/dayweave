package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.ExecutionDeferAssessmentSnapshot
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.PendingExecutionDeferIntent
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeferAssessmentHttpRequest
import com.greengolddog.dayweave.network.ExecutionApiException
import com.greengolddog.dayweave.network.ExecutionTransport
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.RemoteExecutionMutation
import com.greengolddog.dayweave.network.RemoteExecutionHistoryPage
import com.greengolddog.dayweave.network.RemoteExecutionSession
import com.greengolddog.dayweave.network.RemoteExecutionSnapshot
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.time.temporal.ChronoUnit
import java.util.UUID
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
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.int
import kotlinx.serialization.json.long
import kotlinx.serialization.json.put

enum class ExecutionSyncOutcome {
    SUCCESS,
    RECOVERED_COMMAND,
    APPROVAL_REQUIRED,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    CONFLICT,
    NOT_FOUND,
    VALIDATION_FAILURE,
    TRANSIENT_NETWORK_FAILURE,
    RETRYABLE_SERVER_FAILURE,
    PROTOCOL_FAILURE,
    LOCAL_STORAGE_FAILURE,
    INVALID_LOCAL_STATE,
    CONFIGURATION_CHANGED,
    UNEXPECTED_FAILURE,
}

data class ExecutionSyncState(
    val phase: CanonicalSyncPhase,
    val message: String,
) {
    val isBusy: Boolean get() = phase == CanonicalSyncPhase.SYNCING
}

/** Owns the server-authoritative, cross-device execution lease. */
class ExecutionSyncManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: ExecutionTransport,
    private val now: () -> Instant = Instant::now,
    private val newUuid: () -> UUID = UUID::randomUUID,
    private val cancelTimedBreakNotification: suspend () -> Boolean = { true },
    private val reconcileTimedBreakNotification: suspend () -> Unit = {},
) {
    private val operationMutex = Mutex()
    private val mutableState = MutableStateFlow(initialState())
    private val json = Json {
        ignoreUnknownKeys = false
        explicitNulls = false
        encodeDefaults = true
    }
    val state: StateFlow<ExecutionSyncState> = mutableState.asStateFlow()

    /** Called only while the process-wide binding writer excludes every old response mutation. */
    internal fun quarantineBindingState() {
        mutableState.value = ExecutionSyncState(
            phase = CanonicalSyncPhase.NOT_CONFIGURED,
            message = "Connect the DayWeave API to synchronize execution",
        )
    }

    suspend fun refresh(): ExecutionSyncOutcome {
        val intentBeforeRefresh = plannerStore.state.value.pendingExecutionDeferIntent
        val outcome = refreshExecutionState()
        if (outcome != ExecutionSyncOutcome.SUCCESS) return outcome
        intentBeforeRefresh?.let { intent ->
            deferredClosureOutcome(intent)?.let { return it }
        }
        val intent = plannerStore.state.value.pendingExecutionDeferIntent
            ?: return ExecutionSyncOutcome.SUCCESS
        return continueDeferIntent(intent)
    }

    private suspend fun refreshExecutionState(): ExecutionSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Reconciling cross-device execution…")
            try {
                configuration.withBindingOperation {
                    ensureDeviceIdentity()
                    beginHistoryVerification(configuration)
                    val pending = plannerStore.state.value.pendingExecutionCommand
                    val pendingNeedsNotificationBarrier =
                        pending != null && pending.commandType != "start"
                    withTimedBreakNotificationBarrier(
                        required = pendingNeedsNotificationBarrier,
                    ) {
                        if (pending != null) reconcilePending(configuration)
                        reconcileSnapshot(
                            configuration,
                            transport.snapshot(configuration),
                            "Execution is synchronized across devices",
                            notificationBarrierAlreadyHeld = pendingNeedsNotificationBarrier,
                        )
                    }
                    updateConnected("Execution is synchronized across devices")
                    ExecutionSyncOutcome.SUCCESS
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    suspend fun start(blockId: String): ExecutionSyncOutcome = command(
        blockId = blockId,
        requireReconciledHabitOutbox = true,
    ) { context ->
        if (
            context.snapshot.activeSession != null ||
            plannerStore.state.value.activeSession != null
        ) throw InvalidExecutionStateException(
            "Finish, skip, or pause the current cross-device session first.",
        )
        if (context.block.status != ItemStatus.SCHEDULED) {
            throw InvalidExecutionStateException("This scheduled block cannot be started now.")
        }
        if (plannerStore.isCanonicalExecutionStartBlocked(blockId)) {
            throw InvalidExecutionStateException(
                "This execution already ended or its canonical outcome still needs reconciliation.",
            )
        }
        val itemId = context.block.canonicalItemId
            ?: throw InvalidExecutionStateException("This block is not canonical.")
        val itemRevision = context.block.canonicalRevision
            ?: throw InvalidExecutionStateException("The canonical item revision is unavailable.")
        val sessionIndex = context.block.sessionIndex
            ?: throw InvalidExecutionStateException("The server session index is unavailable.")
        val sessionId = newUuid().toString()
        CommandSpec(
            type = "start",
            identity = ExecutionIdentity(
                sessionId = sessionId,
                itemId = itemId,
                itemRevision = itemRevision,
                occurrenceId = context.block.occurrenceId,
                sessionIndex = sessionIndex,
                plannedBlockId = context.block.id,
                sourceDeviceId = context.deviceId,
            ),
            focusedBlockId = blockId,
            command = buildJsonObject {
                put("type", "start")
                put("session_id", sessionId)
                put("item_id", itemId)
                put("item_revision", itemRevision)
                context.block.occurrenceId?.let { put("occurrence_id", it) }
                put("session_index", sessionIndex)
                put("planned_block_id", context.block.id)
                put("device_id", context.deviceId)
            },
        )
    }

    suspend fun pause(
        blockId: String,
        durationSeconds: Int? = null,
        pauseUntil: Instant? = null,
        reason: String? = null,
    ): ExecutionSyncOutcome = command(blockId) { context ->
        require(durationSeconds == null || durationSeconds in 1..MAX_PAUSE_SECONDS)
        require(durationSeconds == null || pauseUntil == null)
        pauseUntil?.let { until ->
            require(until > now() && until <= now().plusSeconds(MAX_PAUSE_SECONDS.toLong()))
        }
        reason?.let { require(it.isNotBlank() && it.length <= 500) }
        val active = context.requireActiveSession(blockId)
        CommandSpec(
            type = "pause",
            identity = active.immutableIdentity(),
            focusedBlockId = blockId,
            command = buildJsonObject {
                put("type", "pause")
                put("session_id", active.id)
                durationSeconds?.let { put("duration_seconds", it) }
                pauseUntil?.let { put("pause_until", it.toString()) }
                reason?.let { put("reason", it) }
            },
        )
    }

    suspend fun resume(blockId: String): ExecutionSyncOutcome = command(blockId) { context ->
        val active = context.requireActiveSession(blockId)
        if (active.status != "paused") {
            throw InvalidExecutionStateException("The canonical session is not paused.")
        }
        CommandSpec(
            type = "resume",
            identity = active.immutableIdentity(),
            focusedBlockId = blockId,
            command = buildJsonObject {
                put("type", "resume")
                put("session_id", active.id)
            },
        )
    }

    suspend fun complete(
        blockId: String,
        actualSeconds: Long? = null,
    ): ExecutionSyncOutcome = finish(blockId, "complete", actualSeconds)

    suspend fun skip(
        blockId: String,
        actualSeconds: Long? = null,
    ): ExecutionSyncOutcome = finish(blockId, "skip", actualSeconds)

    /**
     * Closes an exact server-owned lease and reserves its unconsumed work at [moveStart].
     *
     * A running lease is paused first. That gives the client one server-confirmed, whole-second
     * accumulated value instead of estimating time while two network requests are in flight.
     * The Defer then carries that value as `actual_seconds`, and its move window is exactly the
     * published source duration minus the confirmed accumulation.
     */
    suspend fun doLater(
        blockId: String,
        moveStart: Instant,
    ): ExecutionSyncOutcome {
        val exactMoveStart = moveStart.truncatedTo(ChronoUnit.SECONDS)
        if (!isSafeExecutionDeferTarget(exactMoveStart, now()) || exactMoveStart != moveStart) {
            updateError(
                "Choose a five-minute slot at least ten minutes from now for this work.",
            )
            return ExecutionSyncOutcome.INVALID_LOCAL_STATE
        }

        var preparedIntent: PendingExecutionDeferIntent? = null
        val preparation = withReadyStore {
            try {
                val current = plannerStore.state.value
                val existing = current.pendingExecutionDeferIntent
                if (existing != null) {
                    if (
                        existing.focusedBlockId != blockId ||
                        existing.moveStart != exactMoveStart.toString()
                    ) {
                        throw InvalidExecutionStateException(
                            "Another move-later request is still reconciling.",
                        )
                    }
                    preparedIntent = existing
                    return@withReadyStore ExecutionSyncOutcome.SUCCESS
                }
                val session = current.canonicalExecutionSession
                    ?: throw InvalidExecutionStateException(
                        "Only an active or paused synced session can be moved later.",
                    )
                if (session.status !in OPEN_STATUSES) {
                    throw InvalidExecutionStateException(
                        "Only an active or paused synced session can be moved later.",
                    )
                }
                val block = current.schedule.firstOrNull { it.id == blockId }
                    ?: throw InvalidExecutionStateException(
                        "The exact published source block is unavailable.",
                    )
                val plannedBlockId = session.plannedBlockId
                    ?: throw InvalidExecutionStateException(
                        "The execution lease has no published source block.",
                    )
                if (
                    plannedBlockId != block.id || block.canonicalItemId != session.itemId ||
                    block.canonicalRevision != session.itemRevision ||
                    block.occurrenceId != session.occurrenceId ||
                    block.sessionIndex != session.sessionIndex
                ) {
                    throw InvalidExecutionStateException(
                        "The execution lease no longer matches its published source block.",
                    )
                }
                val plannedSeconds = block.exactPublishedDurationSeconds()
                val remainingFloor = runCatching {
                    Math.subtractExact(plannedSeconds, session.accumulatedSeconds)
                }.getOrElse {
                    throw InvalidExecutionStateException("The remaining duration is unavailable.")
                }
                if (remainingFloor !in 1..MAX_DEFER_MOVE_WINDOW_SECONDS.toLong()) {
                    throw InvalidExecutionStateException(
                        "No supported whole-second planned work remains to move later.",
                    )
                }
                val stagedAt = now()
                if (!isSafeExecutionDeferTarget(exactMoveStart, stagedAt)) {
                    throw InvalidExecutionStateException(
                        "The selected five-minute slot is now too close for a safe assessment.",
                    )
                }
                val intent = PendingExecutionDeferIntent(
                    schemaVersion = EXECUTION_DEFER_INTENT_SCHEMA_VERSION,
                    syncOrigin = current.canonicalExecutionSyncOrigin
                        ?: throw InvalidExecutionStateException(
                            "The execution binding is unavailable.",
                        ),
                    configurationId = current.canonicalExecutionConfigurationId,
                    sessionId = session.id,
                    itemId = session.itemId,
                    itemRevision = session.itemRevision,
                    occurrenceId = session.occurrenceId,
                    sessionIndex = session.sessionIndex,
                    plannedBlockId = plannedBlockId,
                    sourceDeviceId = session.sourceDeviceId,
                    focusedBlockId = blockId,
                    sourceStart = requireNotNull(block.absoluteStartAt),
                    sourceEnd = requireNotNull(block.absoluteEndAt),
                    moveStart = exactMoveStart.toString(),
                    stagedAt = stagedAt.toString(),
                )
                val receipt = plannerStore.stageExecutionDeferIntent(intent)
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalExecutionStorageException()
                }
                preparedIntent = plannerStore.state.value.pendingExecutionDeferIntent
                    ?: throw LocalExecutionStorageException()
                ExecutionSyncOutcome.SUCCESS
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
        if (preparation != ExecutionSyncOutcome.SUCCESS) return preparation
        return continueDeferIntent(requireNotNull(preparedIntent))
    }

    /** Records explicit approval for the exact visible server assessment, then continues it. */
    suspend fun approveDefer(assessmentDigest: String): ExecutionSyncOutcome {
        val approved = withReadyStore {
            try {
                val intent = plannerStore.state.value.pendingExecutionDeferIntent
                    ?: throw InvalidExecutionStateException(
                        "There is no authoritative move warning to approve.",
                    )
                val assessment = intent.assessment
                    ?: throw InvalidExecutionStateException(
                        "The move assessment expired; it will be checked again.",
                    )
                if (
                    !assessment.approvalRequired ||
                    assessment.assessmentDigest != assessmentDigest
                ) {
                    throw InvalidExecutionStateException(
                        "The move warning changed. Review the latest assessment.",
                    )
                }
                val receipt = plannerStore.approveExecutionDeferAssessment(
                    intent.sessionId,
                    assessmentDigest,
                )
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalExecutionStorageException()
                }
                ExecutionSyncOutcome.SUCCESS
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
        if (approved != ExecutionSyncOutcome.SUCCESS) return approved
        val intent = plannerStore.state.value.pendingExecutionDeferIntent
            ?: return invalidLocalState("The approved move is no longer pending.")
        return continueDeferIntent(intent)
    }

    /** Cancels only the unsent move intent; an already journaled command remains immutable. */
    suspend fun cancelDefer(): ExecutionSyncOutcome = withReadyStore {
        try {
            val current = plannerStore.state.value
            if (current.pendingExecutionCommand != null) {
                throw InvalidExecutionStateException(
                    "The exact execution command must reconcile before this move can be canceled.",
                )
            }
            val intent = current.pendingExecutionDeferIntent
                ?: return@withReadyStore ExecutionSyncOutcome.SUCCESS
            val receipt = plannerStore.clearExecutionDeferIntent(
                intent.sessionId,
                "Move canceled · the authoritative execution session remains paused",
            )
            if (receipt == null || !receipt.awaitDurable()) {
                throw LocalExecutionStorageException()
            }
            updateConnected("Move canceled · the session remains paused")
            ExecutionSyncOutcome.SUCCESS
        } catch (error: Throwable) {
            handleFailure(error)
        }
    }

    private suspend fun continueDeferIntent(
        intent: PendingExecutionDeferIntent,
    ): ExecutionSyncOutcome {
        val moveStart = runCatching { Instant.parse(intent.moveStart) }.getOrNull()
            ?: return invalidLocalState("The saved move time is invalid; the session was kept safe.")
        if (moveStart <= now()) {
            if (!clearDeferIntent(intent, "Move time expired · the session remains paused")) {
                return handleFailure(LocalExecutionStorageException())
            }
            return invalidLocalState(
                "The selected move time passed while synchronizing; choose a new time. " +
                    "The session remains paused.",
            )
        }

        val recoveryNow = now()
        val hasFreshAssessment = intent.assessment?.let { assessment ->
            runCatching { Instant.parse(assessment.expiresAt) > recoveryNow }.getOrDefault(false)
        } == true
        if (!hasFreshAssessment && !isSafeExecutionDeferTarget(moveStart, recoveryNow)) {
            if (!clearDeferIntent(
                    intent,
                    "Move target became too close for assessment · execution was left unchanged",
                )
            ) {
                return handleFailure(LocalExecutionStorageException())
            }
            return invalidLocalState(
                "Choose a new five-minute target at least ten minutes away. " +
                    "The current execution session was left unchanged.",
            )
        }

        val paused = ensurePausedForDefer(intent)
        deferredClosureOutcome(intent)?.let { return it }
        if (paused != ExecutionSyncOutcome.SUCCESS) return paused
        if (plannerStore.state.value.pendingExecutionDeferIntent?.sessionId != intent.sessionId) {
            return invalidLocalState(
                "The saved move was no longer valid after pausing. The session remains paused.",
            )
        }

        repeat(MAX_RECONCILED_COMMAND_ATTEMPTS) {
            var currentIntent = plannerStore.state.value.pendingExecutionDeferIntent
                ?: return invalidLocalState(
                    "The saved move is no longer available. The session remains paused.",
                )
            val assessment = currentIntent.assessment
            if (assessment == null || runCatching {
                    Instant.parse(assessment.expiresAt) <= now()
                }.getOrDefault(true)
            ) {
                if (!isSafeExecutionDeferTarget(moveStart, now())) {
                    if (!clearDeferIntent(
                            currentIntent,
                            "Move target became too close for reassessment · execution was retained",
                        )
                    ) {
                        return handleFailure(LocalExecutionStorageException())
                    }
                    return invalidLocalState(
                        "Choose a new five-minute target at least ten minutes away. " +
                            "No assessment or Defer was sent; review the current session.",
                    )
                }
                if (assessment != null && !clearDeferAssessmentForRetry(currentIntent)) {
                    return handleFailure(LocalExecutionStorageException())
                }
                val assessmentOutcome = assessPausedDefer(currentIntent)
                deferredClosureOutcome(currentIntent)?.let { return it }
                if (assessmentOutcome != ExecutionSyncOutcome.SUCCESS) return assessmentOutcome
                currentIntent = plannerStore.state.value.pendingExecutionDeferIntent
                    ?: return invalidLocalState(
                        "The assessed move is no longer pending. The session remains paused.",
                    )
            }
            val currentAssessment = currentIntent.assessment
                ?: return invalidLocalState(
                    "The authoritative move assessment is unavailable. The session remains paused.",
                )
            if (
                currentAssessment.approvalRequired &&
                currentIntent.approvedAssessmentDigest != currentAssessment.assessmentDigest
            ) {
                updateConnected("Move assessed · approve the exact content-free warnings to continue")
                return ExecutionSyncOutcome.APPROVAL_REQUIRED
            }

            val outcome = deferPaused(currentIntent, currentAssessment)
            deferredClosureOutcome(currentIntent)?.let { return it }
            if (outcome != ExecutionSyncOutcome.SUCCESS) {
                val after = plannerStore.state.value
                val canReassess = outcome in setOf(
                    ExecutionSyncOutcome.CONFLICT,
                    ExecutionSyncOutcome.VALIDATION_FAILURE,
                    ExecutionSyncOutcome.INVALID_LOCAL_STATE,
                ) && after.pendingExecutionCommand == null &&
                    after.canonicalExecutionSession?.let { session ->
                        session.id == currentIntent.sessionId && session.status == "paused"
                    } == true
                if (canReassess) {
                    if (!clearDeferAssessmentForRetry(currentIntent)) {
                        return handleFailure(LocalExecutionStorageException())
                    }
                    return@repeat
                }
                return outcome
            }
            val current = plannerStore.state.value
            if (
                current.pendingExecutionCommand != null ||
                current.canonicalExecutionSession?.let {
                    it.id != intent.sessionId || it.status != "paused"
                } != false
            ) {
                return invalidLocalState(
                    "Execution changed while the move was being prepared; review it before retrying.",
                )
            }
        }
        return invalidLocalState(
            "A previous execution command was reconciled; review the paused session before retrying.",
        )
    }

    private suspend fun ensurePausedForDefer(
        intent: PendingExecutionDeferIntent,
    ): ExecutionSyncOutcome {
        repeat(MAX_RECONCILED_COMMAND_ATTEMPTS) {
            val current = plannerStore.state.value.canonicalExecutionSession
            if (current?.id != intent.sessionId) {
                return invalidLocalState(
                    "Execution changed while the saved move was reconciling; review the session.",
                )
            }
            when (current?.status) {
                "paused" -> return ExecutionSyncOutcome.SUCCESS
                "active" -> {
                    val outcome = pauseForDefer(intent)
                    if (outcome != ExecutionSyncOutcome.SUCCESS) {
                        val reconciled = plannerStore.state.value.canonicalExecutionSession
                        if (reconciled?.id == intent.sessionId && reconciled.status == "paused") {
                            return ExecutionSyncOutcome.SUCCESS
                        }
                        return outcome
                    }
                }
                else -> return invalidLocalState(
                    "Only an active or paused synced session can be moved later.",
                )
            }
        }
        return if (
            plannerStore.state.value.canonicalExecutionSession?.let {
                it.id == intent.sessionId && it.status == "paused"
            } == true
        ) {
            ExecutionSyncOutcome.SUCCESS
        } else {
            invalidLocalState(
                "A previous execution command was reconciled; review the session before retrying.",
            )
        }
    }

    private suspend fun assessPausedDefer(
        intent: PendingExecutionDeferIntent,
    ): ExecutionSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Checking the exact paused move against the published plan…")
            try {
                configuration.withBindingOperation {
                    ensureDeviceIdentity()
                    beginHistoryVerification(configuration)
                    val pending = plannerStore.state.value.pendingExecutionCommand
                    val pendingNeedsNotificationBarrier =
                        pending != null && pending.commandType != "start"
                    val snapshot = withTimedBreakNotificationBarrier(
                        required = pendingNeedsNotificationBarrier,
                    ) {
                        if (pending != null) reconcilePending(configuration)
                        reconcileSnapshot(
                            configuration,
                            transport.snapshot(configuration),
                            "Paused execution lease checked before move assessment",
                            notificationBarrierAlreadyHeld = pendingNeedsNotificationBarrier,
                        )
                    }
                    val paused = snapshot.activeSession
                        ?: throw InvalidExecutionStateException(
                            "The paused execution lease is no longer active.",
                        )
                    if (
                        paused.id != intent.sessionId || paused.status != "paused" ||
                        paused.runningSince != null || paused.itemId != intent.itemId ||
                        paused.itemRevision != intent.itemRevision ||
                        paused.occurrenceId != intent.occurrenceId ||
                        paused.sessionIndex != intent.sessionIndex ||
                        paused.plannedBlockId != intent.plannedBlockId ||
                        paused.sourceDeviceId != intent.sourceDeviceId
                    ) {
                        throw InvalidExecutionStateException(
                            "Execution changed before the authoritative move assessment.",
                        )
                    }
                    val moveStart = Instant.parse(intent.moveStart)
                    if (!isSafeExecutionDeferTarget(moveStart, now())) {
                        throw InvalidExecutionStateException(
                            "The selected five-minute slot is too close for a fresh assessment.",
                        )
                    }
                    val assessment = transport.assessDefer(
                        configuration,
                        DeferAssessmentHttpRequest(
                            expectedRevision = snapshot.revision,
                            sessionId = paused.id,
                            moveStart = intent.moveStart,
                            actualSeconds = paused.accumulatedSeconds,
                        ),
                    )
                    ensureConfigurationCurrent(configuration)
                    val receipt = try {
                        plannerStore.recordExecutionDeferAssessment(
                            intent.sessionId,
                            assessment,
                        )
                    } catch (error: IllegalArgumentException) {
                        throw InvalidExecutionProtocolException(error)
                    }
                    if (receipt == null || !receipt.awaitDurable()) {
                        throw LocalExecutionStorageException()
                    }
                    updateConnected(
                        if (assessment.approvalRequired) {
                            "Move assessed · explicit approval is required"
                        } else {
                            "Move assessed · no placement warning requires approval"
                        },
                    )
                    ExecutionSyncOutcome.SUCCESS
                }
            } catch (_: ExecutionApiException.Conflict) {
                if (!clearDeferAssessmentForRetry(intent)) {
                    return@withLock handleFailure(LocalExecutionStorageException())
                }
                updateError(
                    "The plan or execution revision changed during assessment; the paused " +
                        "target was kept for a fresh check.",
                )
                ExecutionSyncOutcome.CONFLICT
            } catch (_: ExecutionApiException.Validation) {
                if (!clearDeferAssessmentForRetry(intent)) {
                    return@withLock handleFailure(LocalExecutionStorageException())
                }
                updateError(
                    "The server rejected this move target; the session remains paused for review.",
                )
                ExecutionSyncOutcome.VALIDATION_FAILURE
            } catch (_: ExecutionApiException.NotFound) {
                if (!clearDeferAssessmentForRetry(intent)) {
                    return@withLock handleFailure(LocalExecutionStorageException())
                }
                updateError("The execution source was not found; the saved target was retained.")
                ExecutionSyncOutcome.NOT_FOUND
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private suspend fun clearDeferAssessmentForRetry(
        intent: PendingExecutionDeferIntent,
    ): Boolean {
        val current = plannerStore.state.value.pendingExecutionDeferIntent ?: return true
        if (current.sessionId != intent.sessionId) return false
        if (current.assessment == null && current.approvedAssessmentDigest == null) return true
        val receipt = plannerStore.clearExecutionDeferAssessment(
            intent.sessionId,
            "Move evidence changed · retaining the paused target for reassessment",
        )
        return receipt?.awaitDurable() == true
    }

    private suspend fun pauseForDefer(
        intent: PendingExecutionDeferIntent,
    ): ExecutionSyncOutcome = command(intent.focusedBlockId) { context ->
        val active = context.requireActiveSession(intent.focusedBlockId)
        if (active.id != intent.sessionId || active.status != "active") {
            throw InvalidExecutionStateException(
                "The execution lease changed before its exact pause.",
            )
        }
        CommandSpec(
            type = "pause",
            identity = active.immutableIdentity(),
            focusedBlockId = intent.focusedBlockId,
            command = buildJsonObject {
                put("type", "pause")
                put("session_id", active.id)
            },
        )
    }

    private suspend fun deferPaused(
        intent: PendingExecutionDeferIntent,
        assessment: ExecutionDeferAssessmentSnapshot,
    ): ExecutionSyncOutcome = command(intent.focusedBlockId) { context ->
        val paused = context.requireActiveSession(intent.focusedBlockId)
        if (
            paused.id != intent.sessionId || paused.status != "paused" ||
            paused.runningSince != null
        ) {
            throw InvalidExecutionStateException(
                "The server must confirm an exact pause before this work can move.",
            )
        }
        if (
            context.block.id != intent.plannedBlockId ||
            context.block.absoluteStartAt != intent.sourceStart ||
            context.block.absoluteEndAt != intent.sourceEnd
        ) {
            throw InvalidExecutionStateException(
                "The exact published source changed before this work could move.",
            )
        }
        if (
            context.snapshot.revision != assessment.executionRevision ||
            paused.revision != assessment.sessionRevision ||
            paused.accumulatedSeconds != assessment.actualSeconds ||
            paused.itemId != assessment.itemId ||
            paused.itemRevision != assessment.itemRevision ||
            paused.occurrenceId != assessment.occurrenceId ||
            paused.sessionIndex != assessment.sourceSessionIndex ||
            intent.plannedBlockId != assessment.sourceBlockId ||
            intent.moveStart != assessment.moveStart
        ) {
            throw InvalidExecutionStateException(
                "The paused execution revision changed; the move must be assessed again.",
            )
        }
        if (Instant.parse(assessment.expiresAt) <= now()) {
            throw InvalidExecutionStateException(
                "The authoritative move assessment expired before command staging.",
            )
        }
        if (
            if (assessment.approvalRequired) {
                intent.approvedAssessmentDigest != assessment.assessmentDigest
            } else {
                intent.approvedAssessmentDigest != null
            }
        ) throw InvalidExecutionStateException(
            "The exact authoritative move warning has not been approved.",
        )
        CommandSpec(
            type = "defer",
            identity = paused.immutableIdentity(),
            focusedBlockId = intent.focusedBlockId,
            command = buildJsonObject {
                put("type", "defer")
                put("session_id", paused.id)
                put("move_start", assessment.moveStart)
                put("move_end", assessment.moveEnd)
                put("actual_seconds", assessment.actualSeconds)
                put("assessment_digest", assessment.assessmentDigest)
                intent.approvedAssessmentDigest?.let {
                    put("approved_assessment_digest", it)
                }
            },
        )
    }

    private fun ScheduleItem.exactPublishedDurationSeconds(): Long {
        val start = absoluteStartAt?.let(Instant::parse)
            ?: throw InvalidExecutionStateException("The published start is unavailable.")
        val end = absoluteEndAt?.let(Instant::parse)
            ?: throw InvalidExecutionStateException("The published end is unavailable.")
        val duration = Duration.between(start, end)
        if (duration.isNegative || duration.isZero || duration.nano != 0) {
            throw InvalidExecutionStateException("The published duration is not a whole second.")
        }
        return duration.seconds
    }

    private fun exactDeferredClosure(
        intent: PendingExecutionDeferIntent,
    ): CanonicalExecutionSessionSnapshot? {
        val current = plannerStore.state.value
        val deferred = current.terminalExecutionOutcomes[intent.sessionId]?.session
            ?: return null
        val assessment = intent.assessment ?: return null
        return deferred.takeIf {
            it.status == "deferred" && it.itemId == intent.itemId &&
                it.itemRevision == intent.itemRevision && it.occurrenceId == intent.occurrenceId &&
                it.sessionIndex == intent.sessionIndex &&
                it.plannedBlockId == intent.plannedBlockId &&
                it.sourceDeviceId == intent.sourceDeviceId &&
                it.actualSeconds == assessment.actualSeconds &&
                it.moveStart == assessment.moveStart && it.moveEnd == assessment.moveEnd
        }
    }

    private suspend fun deferredClosureOutcome(
        intent: PendingExecutionDeferIntent,
    ): ExecutionSyncOutcome? {
        val deferred = exactDeferredClosure(intent) ?: return null
        if (!clearDeferIntent(intent, "Move confirmed · publishing the replacement placement")) {
            return handleFailure(LocalExecutionStorageException())
        }
        return if (deferred.moveStart == intent.moveStart) {
            ExecutionSyncOutcome.SUCCESS
        } else {
            updateConnected(
                "Recovered the previously requested move; the newly selected time was not sent.",
            )
            ExecutionSyncOutcome.RECOVERED_COMMAND
        }
    }

    private suspend fun clearDeferIntent(
        intent: PendingExecutionDeferIntent,
        message: String,
    ): Boolean {
        val current = plannerStore.state.value.pendingExecutionDeferIntent ?: return true
        if (current.sessionId != intent.sessionId) return false
        val receipt = plannerStore.clearExecutionDeferIntent(intent.sessionId, message)
        return receipt?.awaitDurable() == true
    }

    private fun invalidLocalState(message: String): ExecutionSyncOutcome {
        updateError(message)
        return ExecutionSyncOutcome.INVALID_LOCAL_STATE
    }

    private suspend fun finish(
        blockId: String,
        type: String,
        actualSeconds: Long?,
    ): ExecutionSyncOutcome = command(blockId) { context ->
        require(actualSeconds == null || actualSeconds >= 0)
        val active = context.requireActiveSession(blockId)
        CommandSpec(
            type = type,
            identity = active.immutableIdentity(),
            focusedBlockId = blockId,
            command = buildJsonObject {
                put("type", type)
                put("session_id", active.id)
                actualSeconds?.let { put("actual_seconds", it) }
            },
        )
    }

    private suspend fun <T> withTimedBreakNotificationBarrier(
        required: Boolean,
        transition: suspend () -> T,
    ): T {
        if (!required) return transition()
        if (!cancelTimedBreakNotification()) throw LocalExecutionStorageException()
        return try {
            transition()
        } finally {
            // A failed or rejected transition must restore the unchanged durable reminder; a
            // successful one reconciles cancellation against the replacement/closed lease.
            withContext(NonCancellable) { reconcileTimedBreakNotification() }
        }
    }

    private suspend fun command(
        blockId: String,
        requireReconciledHabitOutbox: Boolean = false,
        build: (CommandContext) -> CommandSpec,
    ): ExecutionSyncOutcome = withReadyStore {
        operationMutex.withLock {
            if (
                requireReconciledHabitOutbox &&
                plannerStore.state.value.habitLedger.pendingMutations.isNotEmpty()
            ) {
                updateError("Synchronize saved habit changes before starting canonical work.")
                return@withLock ExecutionSyncOutcome.INVALID_LOCAL_STATE
            }
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Checking the cross-device execution lease…")
            try {
                configuration.withBindingOperation {
                ensureDeviceIdentity()
                beginHistoryVerification(configuration)
                val existingPending = plannerStore.state.value.pendingExecutionCommand
                if (existingPending != null) {
                    val pendingNeedsNotificationBarrier = existingPending.commandType != "start"
                    withTimedBreakNotificationBarrier(
                        required = pendingNeedsNotificationBarrier,
                    ) {
                        reconcilePending(configuration)
                        reconcileSnapshot(
                            configuration,
                            transport.snapshot(configuration),
                            "Previous execution command reconciled",
                            notificationBarrierAlreadyHeld = pendingNeedsNotificationBarrier,
                        )
                    }
                    updateConnected("Previous execution command reconciled; review state before retrying")
                    return@withBindingOperation ExecutionSyncOutcome.SUCCESS
                }
                val snapshot = reconcileSnapshot(
                    configuration,
                    transport.snapshot(configuration),
                    "Execution lease checked",
                )
                val local = plannerStore.state.value
                val block = local.schedule.firstOrNull { it.id == blockId }
                    ?: throw InvalidExecutionStateException("The scheduled block is unavailable.")
                if (block.canonicalItemId == null) {
                    throw InvalidExecutionStateException("The scheduled block is not canonical.")
                }
                val deviceId = local.executionDeviceId
                    ?: throw LocalExecutionStorageException()
                val spec = build(CommandContext(snapshot, block, deviceId))
                val requestJson = commandRequest(snapshot.revision, spec.command)
                val pending = PendingExecutionCommand(
                    idempotencyKey = newUuid().toString(),
                    syncOrigin = configuration.baseUrl.toString(),
                    configurationId = configuration.configurationId,
                    expectedRevision = snapshot.revision,
                    sessionId = spec.identity.sessionId,
                    itemId = spec.identity.itemId,
                    itemRevision = spec.identity.itemRevision,
                    occurrenceId = spec.identity.occurrenceId,
                    sessionIndex = spec.identity.sessionIndex,
                    plannedBlockId = spec.identity.plannedBlockId,
                    sourceDeviceId = spec.identity.sourceDeviceId,
                    commandType = spec.type,
                    requestJson = requestJson,
                    focusedBlockId = spec.focusedBlockId,
                    startedAt = now().toString(),
                )
                withTimedBreakNotificationBarrier(required = spec.type != "start") {
                    val staged = plannerStore.stageExecutionCommand(pending)
                    if (staged == null || !staged.awaitDurable()) {
                        throw LocalExecutionStorageException()
                    }
                    updateBusy("Applying ${spec.type} across devices…")
                    try {
                        applyPending(configuration, pending)
                    } catch (error: ExecutionApiException.Conflict) {
                        reconcileRejectedCommand(
                            configuration,
                            pending,
                            ExecutionSyncOutcome.CONFLICT,
                            "Execution changed on another device; authoritative state was restored.",
                        )
                    } catch (error: ExecutionApiException.NotFound) {
                        reconcileRejectedCommand(
                            configuration,
                            pending,
                            ExecutionSyncOutcome.NOT_FOUND,
                            "The item or session no longer exists; authoritative state was restored.",
                        )
                    } catch (error: ExecutionApiException.Validation) {
                        reconcileRejectedCommand(
                            configuration,
                            pending,
                            ExecutionSyncOutcome.VALIDATION_FAILURE,
                            "The command was rejected; authoritative execution state was restored.",
                        )
                    }
                }
                updateConnected("Execution updated across devices")
                ExecutionSyncOutcome.SUCCESS
                }
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    private suspend fun reconcilePending(configuration: AuthenticatedApiConfiguration) {
        val pending = plannerStore.state.value.pendingExecutionCommand ?: return
        if (pending.syncOrigin != configuration.baseUrl.toString()) {
            throw ConfigurationChangedException()
        }
        if (pending.configurationId != configuration.configurationId) {
            throw ConfigurationChangedException()
        }
        try {
            applyPending(configuration, pending)
        } catch (error: ExecutionApiException.Conflict) {
            reconcileRejectedCommand(
                configuration,
                pending,
                ExecutionSyncOutcome.CONFLICT,
                "Execution changed on another device; authoritative state was restored.",
            )
        } catch (error: ExecutionApiException.NotFound) {
            reconcileRejectedCommand(
                configuration,
                pending,
                ExecutionSyncOutcome.NOT_FOUND,
                "The item or session no longer exists; authoritative state was restored.",
            )
        } catch (error: ExecutionApiException.Validation) {
            reconcileRejectedCommand(
                configuration,
                pending,
                ExecutionSyncOutcome.VALIDATION_FAILURE,
                "The command was rejected; authoritative execution state was restored.",
            )
        }
    }

    private suspend fun applyPending(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingExecutionCommand,
    ) {
        val command = validatePendingBody(pending)
        val mutation = transport.command(
            configuration = configuration,
            idempotencyKey = pending.idempotencyKey,
            requestJson = pending.requestJson,
        )
        ensureConfigurationCurrent(configuration)
        validateMutation(mutation, pending, command)
        val receipt = plannerStore.reconcileCanonicalExecution(
            syncOrigin = pending.syncOrigin,
            configurationId = pending.configurationId,
            revision = mutation.revision,
            activeSession = mutation.activeSession?.toSnapshot(),
            changedSession = mutation.changedSession.toSnapshot(),
            clearPendingIdempotencyKey = pending.idempotencyKey,
            message = if (mutation.replayed) {
                "Recovered the exact execution command after an interrupted response"
            } else {
                "Execution command confirmed by the server"
            },
        )
        if (receipt == null || !receipt.awaitDurable()) throw LocalExecutionStorageException()
    }

    private suspend fun reconcileRejectedCommand(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingExecutionCommand,
        outcome: ExecutionSyncOutcome,
        message: String,
    ) {
        val identity = pending.immutableIdentity()
        val stable = readStableHistory(
            configuration,
            transport.snapshot(configuration),
            forceComplete = true,
        )
        val continuityVerified = validateHistoryAgainstDurableState(configuration, stable)
        val matchingHistory = stable.history.filter { it.hasSameImmutableIdentity(identity) }
        if (matchingHistory.size > 1) throw InvalidExecutionProtocolException()
        val changed = matchingHistory.singleOrNull()
        val activeMatches = stable.snapshot.activeSession?.hasSameImmutableIdentity(identity) == true
        if (changed?.status in OPEN_STATUSES && !activeMatches) {
            throw InvalidExecutionProtocolException()
        }
        val completeAbsenceProven = exactHistoryRevisionSum(stable.history) ==
            stable.snapshot.revision
        if (changed == null && !activeMatches && !completeAbsenceProven) {
            throw UnresolvedPendingCommandException()
        }
        val newestPresentableTerminalIds = stable.newestPresentableTerminalIds()
        reconcileClosedHistoryRows(
            configuration = configuration,
            stable = stable,
            excludedSessionId = changed?.id,
            message = "Reconciling immutable execution history",
        )
        val receipt = plannerStore.reconcileCanonicalExecution(
            syncOrigin = pending.syncOrigin,
            configurationId = pending.configurationId,
            revision = stable.snapshot.revision,
            activeSession = stable.snapshot.activeSession?.toSnapshot(),
            changedSession = changed?.toSnapshot(),
            clearPendingIdempotencyKey = pending.idempotencyKey,
            message = message,
            changedSessionControlsPresentation =
                changed != null && changed.id in newestPresentableTerminalIds,
        )
        if (receipt == null || !receipt.awaitDurable()) throw LocalExecutionStorageException()
        persistHistoryWindow(
            configuration = configuration,
            stable = stable,
            continuityVerified = continuityVerified,
            message = message,
        )
        if (!continuityVerified) throw ExecutionHistoryContinuityException()
        throw ReconciledCommandRejectionException(outcome, message)
    }

    private suspend fun reconcileSnapshot(
        configuration: AuthenticatedApiConfiguration,
        initialSnapshot: RemoteExecutionSnapshot,
        message: String,
        notificationBarrierAlreadyHeld: Boolean = false,
    ): RemoteExecutionSnapshot {
        val stable = readStableHistory(configuration, initialSnapshot)
        val continuityVerified = validateHistoryAgainstDurableState(configuration, stable)
        val currentTimedBreak = plannerStore.durableState.value
            ?.authoritativeTimedBreakNotificationIdentity()
        val remote = stable.snapshot.activeSession
        val exactTimedBreakSurvives = currentTimedBreak != null &&
            stable.snapshot.revision == currentTimedBreak.executionRevision &&
            remote?.status == "paused" && remote.id == currentTimedBreak.sessionId &&
            remote.revision == currentTimedBreak.sessionRevision &&
            remote.pauseUntil?.let { raw ->
                runCatching { Instant.parse(raw).toEpochMilli() }.getOrNull()
            } == currentTimedBreak.deadlineEpochMillis
        return withTimedBreakNotificationBarrier(
            required = !notificationBarrierAlreadyHeld &&
                currentTimedBreak != null && !exactTimedBreakSurvives,
        ) {
            reconcileClosedHistoryRows(configuration, stable, excludedSessionId = null, message)
            val receipt = plannerStore.reconcileCanonicalExecution(
                syncOrigin = configuration.baseUrl.toString(),
                configurationId = configuration.configurationId,
                revision = stable.snapshot.revision,
                activeSession = remote?.toSnapshot(),
                message = message,
            )
            if (receipt == null || !receipt.awaitDurable()) {
                throw LocalExecutionStorageException()
            }
            persistHistoryWindow(configuration, stable, continuityVerified, message)
            if (!continuityVerified) throw ExecutionHistoryContinuityException()
            stable.snapshot
        }
    }

    private suspend fun reconcileClosedHistoryRows(
        configuration: AuthenticatedApiConfiguration,
        stable: StableExecutionRead,
        excludedSessionId: String?,
        message: String,
    ) {
        // History is newest first. All closed facts belong in the immutable lifetime ledger, but
        // only a target's newest session may control completion/skip presentation. A later open or
        // deferred session must not let an older terminal fact fight the authoritative state.
        val newestPresentableTerminalIds = stable.newestPresentableTerminalIds()
        stable.history.asReversed()
            .filter { it.status in CLOSED_STATUSES && it.id != excludedSessionId }
            .forEach { closed ->
                val isCanonicalTerminal = closed.status in TERMINAL_STATUSES
                val alreadyDurable = plannerStore.state.value.terminalExecutionOutcomes[closed.id]
                    ?.let { outcome ->
                        outcome.syncOrigin == configuration.baseUrl.toString() &&
                            closed.hasSameRemoteSemantics(outcome.session) &&
                            (
                                !isCanonicalTerminal ||
                                    closed.id !in newestPresentableTerminalIds ||
                                    terminalPresentationIsConverged(closed)
                            )
                    } == true
                if (alreadyDurable) return@forEach
                val receipt = plannerStore.reconcileCanonicalExecution(
                    syncOrigin = configuration.baseUrl.toString(),
                    configurationId = configuration.configurationId,
                    revision = stable.snapshot.revision,
                    activeSession = stable.snapshot.activeSession?.toSnapshot(),
                    changedSession = closed.toSnapshot(),
                    message = message,
                    changedSessionControlsPresentation =
                        closed.id in newestPresentableTerminalIds,
                )
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalExecutionStorageException()
                }
            }
    }

    private fun StableExecutionRead.newestPresentableTerminalIds(): Set<String> {
        val authoritativeActiveTarget = snapshot.activeSession?.projectionTarget()
        return history.asSequence()
            .filter { session -> session.projectionTarget() != authoritativeActiveTarget }
            .distinctBy { it.projectionTarget() }
            .filter { it.status in TERMINAL_STATUSES }
            .map { it.id }
            .toSet()
    }

    private fun terminalPresentationIsConverged(terminal: RemoteExecutionSession): Boolean {
        if (terminal.status !in TERMINAL_STATUSES) return false
        val current = plannerStore.state.value
        val outcome = current.terminalExecutionOutcomes[terminal.id] ?: return false
        if (outcome.canonicalProjectionResolution == "user_kept_latest_item") return true
        val focused = current.schedule.firstOrNull { block ->
            block.canonicalItemId == terminal.itemId &&
                block.canonicalRevision == terminal.itemRevision &&
                block.occurrenceId == terminal.occurrenceId &&
                block.sessionIndex == terminal.sessionIndex &&
                (terminal.plannedBlockId == null || block.id == terminal.plannedBlockId)
        } ?: current.schedule.firstOrNull { block ->
            block.canonicalItemId == terminal.itemId &&
                block.canonicalRevision == terminal.itemRevision &&
                block.occurrenceId == terminal.occurrenceId &&
                block.sessionIndex == terminal.sessionIndex
        }
        val expectedStatus = when (terminal.status) {
            "completed" -> ItemStatus.COMPLETED
            "skipped" -> ItemStatus.SKIPPED
            else -> return false
        }
        val expectedMinutes = terminal.actualSeconds?.let { seconds ->
            (seconds / 60L + if (seconds % 60L == 0L) 0L else 1L)
                .coerceAtMost(Int.MAX_VALUE.toLong())
                .toInt()
        }
        if (focused != null && (
                focused.status != expectedStatus || focused.actualMinutes != expectedMinutes
            )
        ) {
            return false
        }
        val occurrenceId = terminal.occurrenceId ?: return true
        val occurrenceBlocks = current.schedule.filter { it.occurrenceId == occurrenceId }
        val shouldResolveOccurrence = current.unscheduledWork.none {
            it.occurrenceId == occurrenceId && it.remainingMinutes > 0
        } && occurrenceBlocks.isNotEmpty() &&
            occurrenceBlocks.all { it.status in setOf(ItemStatus.COMPLETED, ItemStatus.SKIPPED) } &&
            occurrenceBlocks.map { it.status }.distinct().size == 1
        if (!shouldResolveOccurrence) return true
        val owner = current.occurrenceSeriesItemIds[occurrenceId] ?: return false
        val recurrenceOutcome = current.recurrenceOutcomes[occurrenceId] ?: return false
        return recurrenceOutcome.itemId == owner && recurrenceOutcome.status == expectedStatus
    }

    private fun RemoteExecutionSession.projectionTarget() = ExecutionProjectionTarget(
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
    )

    private suspend fun persistHistoryWindow(
        configuration: AuthenticatedApiConfiguration,
        stable: StableExecutionRead,
        continuityVerified: Boolean,
        message: String,
    ) {
        val receipt = plannerStore.recordCanonicalExecutionHistoryWindow(
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = configuration.configurationId,
            revision = stable.snapshot.revision,
            history = stable.history.map { it.toSnapshot() },
            continuityVerified = continuityVerified,
            message = message,
        )
        if (receipt == null || !receipt.awaitDurable()) throw LocalExecutionStorageException()
    }

    /** Snapshot-before/paged-history/snapshot-after prevents mixing concurrent generations. */
    private suspend fun readStableHistory(
        configuration: AuthenticatedApiConfiguration,
        initialSnapshot: RemoteExecutionSnapshot,
        forceComplete: Boolean = false,
    ): StableExecutionRead {
        var before = initialSnapshot
        repeat(MAX_STABLE_READ_ATTEMPTS) {
            validateSnapshot(before)
            ensureConfigurationCurrent(configuration)
            val firstPage = transport.history(
                configuration = configuration,
                limit = MAX_HISTORY_SESSIONS,
                offset = 0,
            )
            validateHistoryPage(firstPage, requestedOffset = 0)
            val current = plannerStore.state.value
            val priorIds = current.canonicalExecutionHistoryWindow.mapTo(hashSetOf()) { it.id }
            val firstPageOverlaps = firstPage.sessions.any { it.id in priorIds }
            val activeLeaseMissingFromFirstPage = before.activeSession?.let { active ->
                firstPage.sessions.none { it.id == active.id }
            } == true
            val needsCompleteBootstrap = firstPage.nextOffset != null && (
                forceComplete || !current.canonicalExecutionHistoryContinuityEstablished ||
                    !firstPageOverlaps || activeLeaseMissingFromFirstPage
            )
            val history = if (needsCompleteBootstrap) {
                readCompleteHistory(configuration, firstPage)
            } else {
                firstPage.sessions
            }
            val after = transport.snapshot(configuration)
            validateSnapshot(after)
            ensureConfigurationCurrent(configuration)
            if (before == after) {
                history.forEach(::validateSession)
                val stable = StableExecutionRead(after, history)
                validateStableRemoteRead(stable)
                return stable
            }
            before = after
        }
        throw UnstableExecutionReadException()
    }

    private suspend fun readCompleteHistory(
        configuration: AuthenticatedApiConfiguration,
        firstPage: RemoteExecutionHistoryPage,
    ): List<RemoteExecutionSession> {
        val history = firstPage.sessions.toMutableList()
        var nextOffset = firstPage.nextOffset
        var pages = 1
        while (nextOffset != null) {
            if (pages >= MAX_BOOTSTRAP_HISTORY_PAGES) {
                throw InvalidExecutionProtocolException()
            }
            val requestedOffset = nextOffset
            val page = transport.history(
                configuration = configuration,
                limit = MAX_HISTORY_SESSIONS,
                offset = requestedOffset,
            )
            validateHistoryPage(page, requestedOffset)
            history += page.sessions
            nextOffset = page.nextOffset
            pages += 1
            ensureConfigurationCurrent(configuration)
        }
        return history
    }

    private fun validateHistoryPage(
        page: RemoteExecutionHistoryPage,
        requestedOffset: Long,
    ) {
        if (page.sessions.size > MAX_HISTORY_SESSIONS) {
            throw InvalidExecutionProtocolException()
        }
        val expectedNext = runCatching {
            Math.addExact(requestedOffset, page.sessions.size.toLong())
        }.getOrElse { throw InvalidExecutionProtocolException(it) }
        if (
            page.nextOffset != null && (
                page.sessions.size != MAX_HISTORY_SESSIONS ||
                    page.nextOffset != expectedNext || page.nextOffset <= requestedOffset
            )
        ) {
            throw InvalidExecutionProtocolException()
        }
    }

    /** Validates one bracketed server view before any durable fence can be released. */
    private fun validateStableRemoteRead(stable: StableExecutionRead) {
        val snapshot = stable.snapshot
        val history = stable.history
        if (history.map { it.id }.distinct().size != history.size) {
            throw InvalidExecutionProtocolException()
        }
        if (!history.zipWithNext().all { (newer, older) ->
                val newerAt = Instant.parse(newer.updatedAt)
                val olderAt = Instant.parse(older.updatedAt)
                newerAt > olderAt || newerAt == olderAt && newer.id > older.id
            }
        ) {
            throw InvalidExecutionProtocolException()
        }
        if (history.any { it.revision > snapshot.revision }) {
            throw InvalidExecutionProtocolException()
        }
        if (exactHistoryRevisionSum(history) > snapshot.revision) {
            throw InvalidExecutionProtocolException()
        }
        if ((snapshot.revision == 0L) != history.isEmpty()) {
            throw InvalidExecutionProtocolException()
        }
        val openRows = history.filter { it.status in OPEN_STATUSES }
        val active = snapshot.activeSession
        if (
            active == null && openRows.isNotEmpty() ||
            active != null && (openRows.size != 1 || openRows.single() != active)
        ) {
            throw InvalidExecutionProtocolException()
        }
    }

    /**
     * Checks immutable terminal rows and proves that the bounded page is globally complete.
     *
     * The server increments the workspace revision exactly once for every execution command, and
     * each command contributes exactly one revision to one session. Therefore the sum of the
     * current session revisions equals the workspace revision iff no session is missing. A false
     * result is persistable for diagnostics but never start-safe.
     */
    private fun validateHistoryAgainstDurableState(
        configuration: AuthenticatedApiConfiguration,
        stable: StableExecutionRead,
    ): Boolean {
        val current = plannerStore.state.value
        if (
            current.canonicalExecutionSyncOrigin != configuration.baseUrl.toString() ||
            current.canonicalExecutionConfigurationId != configuration.configurationId ||
            stable.snapshot.revision < current.canonicalExecutionRevision
        ) {
            throw ConfigurationChangedException()
        }
        val historyById = stable.history.associateBy { it.id }
        val priorWindow = current.canonicalExecutionHistoryWindow
        if (
            priorWindow.size > MAX_HISTORY_SESSIONS ||
            priorWindow.map { it.id }.distinct().size != priorWindow.size
        ) {
            throw InvalidExecutionProtocolException()
        }
        priorWindow.forEach { prior ->
            validateSession(prior.toRemoteSession())
            if (prior.canonicalProjectionEligibleAtLeaseStart != null) {
                throw InvalidExecutionProtocolException()
            }
        }
        if (!priorWindow.zipWithNext().all { (newer, older) ->
                val newerAt = Instant.parse(newer.updatedAt)
                val olderAt = Instant.parse(older.updatedAt)
                newerAt > olderAt || newerAt == olderAt && newer.id > older.id
            }
        ) {
            throw InvalidExecutionProtocolException()
        }
        val priorById = priorWindow.associateBy { it.id }
        stable.history.forEach { remote ->
            priorById[remote.id]?.let { prior ->
                if (!remote.hasSameImmutableIdentity(prior.immutableIdentity())) {
                    throw InvalidExecutionProtocolException()
                }
                if (
                    remote.startedAt != prior.startedAt || remote.createdAt != prior.createdAt ||
                    remote.accumulatedSeconds < prior.accumulatedSeconds
                ) {
                    throw InvalidExecutionProtocolException()
                }
                if (prior.status in CLOSED_STATUSES) {
                    if (!remote.hasSameRemoteSemantics(prior)) {
                        throw InvalidExecutionProtocolException()
                    }
                } else if (
                    remote.revision < prior.revision ||
                    remote.revision == prior.revision && !remote.hasSameRemoteSemantics(prior)
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
            current.terminalExecutionOutcomes[remote.id]?.let { outcome ->
                if (
                    outcome.syncOrigin != configuration.baseUrl.toString() ||
                    remote.status !in CLOSED_STATUSES ||
                    !remote.hasSameRemoteSemantics(outcome.session)
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
        }
        val historyRevisionSum = exactHistoryRevisionSum(stable.history)
        val globallyComplete = historyRevisionSum == stable.snapshot.revision
        val previousIds = priorById.keys
        val firstOverlapIndex = stable.history.indexOfFirst { it.id in previousIds }
        val hasContiguousOverlap = if (firstOverlapIndex < 0) {
            false
        } else {
            val overlappingIds = stable.history.drop(firstOverlapIndex).map { it.id }
            overlappingIds == priorWindow.take(overlappingIds.size).map { it.id }
        }
        val observedRevisionDelta = runCatching {
            stable.history.fold(0L) { total, remote ->
                val contribution = priorById[remote.id]?.let { prior ->
                    Math.subtractExact(remote.revision, prior.revision)
                } ?: remote.revision
                require(contribution >= 0)
                Math.addExact(total, contribution)
            }
        }.getOrElse { throw InvalidExecutionProtocolException(it) }
        val expectedRevisionDelta = current.canonicalExecutionHistoryWindowRevision?.let {
            runCatching { Math.subtractExact(stable.snapshot.revision, it) }
                .getOrElse { error -> throw InvalidExecutionProtocolException(error) }
        }
        val rollingContinuity = current.canonicalExecutionHistoryContinuityEstablished &&
            hasContiguousOverlap && expectedRevisionDelta != null && expectedRevisionDelta >= 0 &&
            observedRevisionDelta == expectedRevisionDelta
        val continuityVerified = globallyComplete || rollingContinuity
        if (globallyComplete) {
            if (priorWindow.any { it.id !in historyById }) {
                throw InvalidExecutionProtocolException()
            }
            val missingClosed = current.terminalExecutionOutcomes.values.any { outcome ->
                outcome.syncOrigin == configuration.baseUrl.toString() &&
                    outcome.session.id !in historyById
            }
            if (missingClosed) throw InvalidExecutionProtocolException()
        }
        current.canonicalExecutionSession?.let { cached ->
            val stillActive = stable.snapshot.activeSession
                ?.hasSameImmutableIdentity(cached.immutableIdentity()) == true
            if (!stillActive) {
                val closedMatches = stable.history.filter {
                    it.status in CLOSED_STATUSES &&
                        it.hasSameImmutableIdentity(cached.immutableIdentity())
                }
                if (
                    closedMatches.size > 1 ||
                    continuityVerified && closedMatches.size != 1
                ) {
                    throw UnresolvedRemoteTransitionException()
                }
            }
        }
        if (stable.snapshot.revision == current.canonicalExecutionRevision) {
            val cached = current.canonicalExecutionSession
            val remote = stable.snapshot.activeSession
            val semanticallyEqual = when {
                cached == null || remote == null -> cached == null && remote == null
                else -> remote.hasSameRemoteSemantics(cached)
            }
            if (!semanticallyEqual) throw InvalidExecutionProtocolException()
        }
        return continuityVerified
    }

    private fun exactHistoryRevisionSum(history: List<RemoteExecutionSession>): Long = try {
        history.fold(0L) { total, session -> Math.addExact(total, session.revision) }
    } catch (error: ArithmeticException) {
        throw InvalidExecutionProtocolException(error)
    }

    /** Durably fences starts and refuses to reuse any cache under an unknown credential binding. */
    private suspend fun beginHistoryVerification(
        configuration: AuthenticatedApiConfiguration,
    ) {
        val origin = configuration.baseUrl.toString()
        val configurationId = configuration.configurationId
        val current = plannerStore.state.value
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
            current.pendingExecutionCommand != null ||
            current.pendingExecutionDeferIntent != null
        val canonicalMismatch = hasCanonicalState &&
            (current.canonicalSyncOrigin != origin ||
                current.canonicalConfigurationId != configurationId)
        val executionMismatch = hasExecutionState &&
            (current.canonicalExecutionSyncOrigin != origin ||
                current.canonicalExecutionConfigurationId != configurationId)
        if (canonicalMismatch || executionMismatch) {
            if (plannerStore.hasCredentialReplacementBlocker()) {
                throw ConfigurationChangedException()
            }
            withTimedBreakNotificationBarrier(required = true) {
                val quarantine = plannerStore.abandonCanonicalConnection()
                if (quarantine == null || !quarantine.awaitDurable()) {
                    throw LocalExecutionStorageException()
                }
            }
        }
        val receipt = plannerStore.markCanonicalExecutionHistoryUnverified(
            syncOrigin = origin,
            configurationId = configurationId,
        )
        if (receipt == null || !receipt.awaitDurable()) throw LocalExecutionStorageException()
    }

    private suspend fun ensureDeviceIdentity() {
        val existing = plannerStore.state.value.executionDeviceId
        if (existing != null) {
            require(UUID.fromString(existing).toString() == existing)
            return
        }
        val receipt = plannerStore.ensureExecutionDeviceId(newUuid().toString())
        if (receipt == null || !receipt.awaitDurable()) throw LocalExecutionStorageException()
    }

    private fun validatePendingBody(pending: PendingExecutionCommand): JsonObject = try {
            require(pending.requestJson.length in 2..MAX_PENDING_REQUEST_CHARS)
            val identity = pending.immutableIdentity()
            require(pending.expectedRevision in 0 until Long.MAX_VALUE)
            listOf(
                pending.idempotencyKey,
                identity.sessionId,
                identity.itemId,
                identity.sourceDeviceId,
            ).forEach { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            identity.occurrenceId?.let { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            identity.plannedBlockId?.let { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            val root = json.parseToJsonElement(pending.requestJson).jsonObject
            require(root.keys == setOf("expected_revision", "command"))
            val expectedRevision = root.getValue("expected_revision").jsonPrimitive
            require(!expectedRevision.isString && expectedRevision.long == pending.expectedRevision)
            val command = root.getValue("command").jsonObject
            require(command.requireString("type") == pending.commandType)
            require(command.requireString("session_id") == pending.sessionId)
            when (pending.commandType) {
                "start" -> {
                    val expectedKeys = mutableSetOf(
                        "type",
                        "session_id",
                        "item_id",
                        "item_revision",
                        "session_index",
                        "device_id",
                    )
                    identity.occurrenceId?.let { expectedKeys += "occurrence_id" }
                    identity.plannedBlockId?.let { expectedKeys += "planned_block_id" }
                    require(command.keys == expectedKeys)
                    require(command.requireString("item_id") == identity.itemId)
                    require(command.requireLong("item_revision") == identity.itemRevision)
                    require(command["occurrence_id"]?.jsonPrimitive?.let {
                        require(it.isString)
                        it.content
                    } == identity.occurrenceId)
                    require(command.requireLong("session_index") == identity.sessionIndex.toLong())
                    require(command["planned_block_id"]?.jsonPrimitive?.let {
                        require(it.isString)
                        it.content
                    } == identity.plannedBlockId)
                    require(command.requireString("device_id") == identity.sourceDeviceId)
                }
                "pause" -> {
                    require(
                        command.keys.containsAll(setOf("type", "session_id")) &&
                            command.keys.all {
                                it in setOf(
                                    "type",
                                    "session_id",
                                    "duration_seconds",
                                    "pause_until",
                                    "reason",
                                )
                            },
                    )
                    val duration = command["duration_seconds"]?.jsonPrimitive?.let { primitive ->
                        require(!primitive.isString)
                        primitive.long
                    }
                    val until = command["pause_until"]?.jsonPrimitive?.let { primitive ->
                        require(primitive.isString)
                        Instant.parse(primitive.content)
                    }
                    require(duration == null || duration in 1..MAX_PAUSE_SECONDS.toLong())
                    require(duration == null || until == null)
                    command["reason"]?.jsonPrimitive?.let { primitive ->
                        require(primitive.isString)
                        require(
                            primitive.content.isNotBlank() &&
                                primitive.content.codePointCount(0, primitive.content.length) <= 500,
                        )
                    }
                }
                "resume" -> require(command.keys == setOf("type", "session_id"))
                "complete", "skip" -> {
                    require(
                        command.keys.containsAll(setOf("type", "session_id")) &&
                            command.keys.all { it in setOf("type", "session_id", "actual_seconds") },
                    )
                    command["actual_seconds"]?.jsonPrimitive?.let { primitive ->
                        require(!primitive.isString && primitive.long >= 0)
                    }
                }
                "defer" -> {
                    val expectedKeys = mutableSetOf(
                        "type",
                        "session_id",
                        "move_start",
                        "move_end",
                        "actual_seconds",
                        "assessment_digest",
                    )
                    if ("approved_assessment_digest" in command) {
                        expectedKeys += "approved_assessment_digest"
                    }
                    require(
                        command.keys == expectedKeys,
                    )
                    val moveStart = Instant.parse(command.requireString("move_start"))
                    val moveEnd = Instant.parse(command.requireString("move_end"))
                    val requestStartedAt = Instant.parse(pending.startedAt)
                    val duration = Duration.between(moveStart, moveEnd)
                    require(
                        moveStart > requestStartedAt && moveEnd > moveStart &&
                            duration.nano == 0 &&
                            duration.seconds in 1..MAX_DEFER_MOVE_WINDOW_SECONDS.toLong(),
                    )
                    require(command.requireLong("actual_seconds") >= 0)
                    val assessmentDigest = command.requireString("assessment_digest")
                    require(assessmentDigest.isCanonicalSha256Digest())
                    command["approved_assessment_digest"]?.jsonPrimitive?.let { primitive ->
                        require(primitive.isString && primitive.content == assessmentDigest)
                    }
                }
                else -> throw InvalidExecutionProtocolException()
            }
            command
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            throw InvalidExecutionProtocolException(error)
        }

    private fun validateMutation(
        mutation: RemoteExecutionMutation,
        pending: PendingExecutionCommand,
        command: JsonObject,
    ) {
        val expectedGlobalRevision = runCatching {
            Math.addExact(pending.expectedRevision, 1L)
        }.getOrElse { throw InvalidExecutionProtocolException(it) }
        if (mutation.revision != expectedGlobalRevision) {
            throw InvalidExecutionProtocolException()
        }
        validateSession(mutation.changedSession)
        mutation.activeSession?.let(::validateSession)
        val changed = mutation.changedSession
        if (
            changed.revision > mutation.revision ||
            !changed.hasSameImmutableIdentity(pending.immutableIdentity())
        ) {
            throw InvalidExecutionProtocolException()
        }
        val expectedStatus = when (pending.commandType) {
            "start", "resume" -> "active"
            "pause" -> "paused"
            "complete" -> "completed"
            "skip" -> "skipped"
            "defer" -> "deferred"
            else -> throw InvalidExecutionProtocolException()
        }
        if (changed.status != expectedStatus) {
            throw InvalidExecutionProtocolException()
        }
        if (expectedStatus in OPEN_STATUSES) {
            if (mutation.activeSession != changed) {
                throw InvalidExecutionProtocolException()
            }
        } else if (mutation.activeSession != null) {
            throw InvalidExecutionProtocolException()
        }
        if (pending.commandType == "start") {
            if (
                plannerStore.state.value.canonicalExecutionSession != null ||
                changed.revision != 1L || changed.accumulatedSeconds != 0L
            ) {
                throw InvalidExecutionProtocolException()
            }
            return
        }

        val prior = plannerStore.state.value.canonicalExecutionSession?.toRemoteSession()
            ?: throw InvalidExecutionProtocolException()
        validateSession(prior)
        val nextSessionRevision = runCatching { Math.addExact(prior.revision, 1L) }
            .getOrElse { throw InvalidExecutionProtocolException(it) }
        val changedAt = Instant.parse(changed.updatedAt)
        val priorUpdatedAt = Instant.parse(prior.updatedAt)
        if (
            !prior.hasSameImmutableIdentity(pending.immutableIdentity()) ||
            !changed.hasSameImmutableIdentity(prior.immutableIdentity()) ||
            changed.revision != nextSessionRevision ||
            changed.startedAt != prior.startedAt || changed.createdAt != prior.createdAt ||
            changed.accumulatedSeconds < prior.accumulatedSeconds || changedAt < priorUpdatedAt
        ) {
            throw InvalidExecutionProtocolException()
        }
        // The server measures a running interval from a private monotonic anchor. Public protocol
        // instants can be ahead of that anchor after wall-clock repair (and can differ at
        // sub-second precision), so they cannot safely reproduce accumulated_seconds. A paused
        // lease has no running interval and therefore retains its exact prior accumulation.
        val accumulatedMatchesClosedAnchor = prior.status != "paused" ||
            changed.accumulatedSeconds == prior.accumulatedSeconds
        when (pending.commandType) {
            "pause" -> {
                if (prior.status !in OPEN_STATUSES) throw InvalidExecutionProtocolException()
                val duration = command["duration_seconds"]?.jsonPrimitive?.long
                val absoluteUntil = command["pause_until"]?.jsonPrimitive?.content?.let(Instant::parse)
                val expectedUntil = duration?.let(changedAt::plusSeconds) ?: absoluteUntil
                val expectedReason = command["reason"]?.jsonPrimitive?.content ?: prior.pauseReason
                val expectedPausedAt = prior.pausedAt?.let(Instant::parse) ?: changedAt
                if (
                    !accumulatedMatchesClosedAnchor ||
                    changed.pausedAt?.let(Instant::parse) != expectedPausedAt ||
                    changed.pauseUntil?.let(Instant::parse) != expectedUntil ||
                    changed.pauseReason != expectedReason
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
            "resume" -> {
                if (
                    prior.status != "paused" || changed.accumulatedSeconds != prior.accumulatedSeconds
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
            "complete", "skip" -> {
                if (prior.status !in OPEN_STATUSES) throw InvalidExecutionProtocolException()
                val corrected = command["actual_seconds"]?.jsonPrimitive?.long
                val expectedPausedAt = prior.pausedAt?.let(Instant::parse)
                    ?: if (prior.status == "paused") changedAt else null
                if (
                    !accumulatedMatchesClosedAnchor ||
                    changed.actualSeconds != (corrected ?: changed.accumulatedSeconds) ||
                    changed.pausedAt?.let(Instant::parse) != expectedPausedAt
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
            "defer" -> {
                if (prior.status != "paused") throw InvalidExecutionProtocolException()
                val corrected = command.requireLong("actual_seconds")
                val expectedMoveStart = Instant.parse(command.requireString("move_start"))
                val expectedMoveEnd = Instant.parse(command.requireString("move_end"))
                val expectedPausedAt = prior.pausedAt?.let(Instant::parse) ?: changedAt
                if (
                    changed.accumulatedSeconds != prior.accumulatedSeconds ||
                    changed.actualSeconds != corrected || corrected != prior.accumulatedSeconds ||
                    changed.pausedAt?.let(Instant::parse) != expectedPausedAt ||
                    changed.moveStart?.let(Instant::parse) != expectedMoveStart ||
                    changed.moveEnd?.let(Instant::parse) != expectedMoveEnd
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
            else -> throw InvalidExecutionProtocolException()
        }
    }

    private fun validateSnapshot(snapshot: RemoteExecutionSnapshot) {
        if (snapshot.revision < 0) throw InvalidExecutionProtocolException()
        snapshot.activeSession?.let { session ->
            validateSession(session)
            if (
                snapshot.revision == 0L || session.status !in OPEN_STATUSES ||
                session.revision > snapshot.revision
            ) {
                throw InvalidExecutionProtocolException()
            }
        }
    }

    private fun validateSession(session: RemoteExecutionSession) {
        try {
            listOf(session.id, session.itemId, session.sourceDeviceId).forEach { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            session.occurrenceId?.let { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            session.plannedBlockId?.let { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            require(session.itemRevision > 0 && session.revision > 0)
            require(session.sessionIndex in 0..UShort.MAX_VALUE.toInt())
            require(session.accumulatedSeconds >= 0 && session.actualSeconds?.let { it >= 0 } != false)
            require(session.status in ALL_STATUSES)
            val startedAt = Instant.parse(session.startedAt)
            val runningSince = session.runningSince?.let(Instant::parse)
            val pausedAt = session.pausedAt?.let(Instant::parse)
            val pauseUntil = session.pauseUntil?.let(Instant::parse)
            val endedAt = session.endedAt?.let(Instant::parse)
            val moveStart = session.moveStart?.let(Instant::parse)
            val moveEnd = session.moveEnd?.let(Instant::parse)
            val createdAt = Instant.parse(session.createdAt)
            val updatedAt = Instant.parse(session.updatedAt)
            require(createdAt == startedAt && updatedAt >= createdAt)
            require(
                session.pauseReason?.let {
                    it.isNotBlank() && it.codePointCount(0, it.length) <= 500
                } != false,
            )
            if (session.revision == 1L) {
                require(
                    session.status == "active" && session.accumulatedSeconds == 0L &&
                        runningSince == startedAt && updatedAt == startedAt,
                )
            }
            when (session.status) {
                "active" -> require(
                    runningSince == updatedAt && pausedAt == null && pauseUntil == null &&
                        session.pauseReason == null &&
                        endedAt == null && session.actualSeconds == null &&
                        moveStart == null && moveEnd == null,
                )
                "paused" -> require(
                    runningSince == null && pausedAt != null && pausedAt >= startedAt &&
                        pausedAt <= updatedAt &&
                        (pauseUntil == null || pauseUntil > updatedAt &&
                            pauseUntil <= updatedAt.plusSeconds(MAX_PAUSE_SECONDS.toLong())) &&
                        endedAt == null && session.actualSeconds == null &&
                        moveStart == null && moveEnd == null,
                )
                "completed", "skipped" -> require(
                    runningSince == null && pauseUntil == null && session.pauseReason == null &&
                        session.actualSeconds != null && endedAt == updatedAt &&
                        (pausedAt == null || pausedAt >= startedAt && pausedAt <= updatedAt) &&
                        moveStart == null && moveEnd == null,
                )
                "deferred" -> require(
                    runningSince == null && pauseUntil == null && session.pauseReason == null &&
                        session.actualSeconds != null && endedAt == updatedAt &&
                        (pausedAt == null || pausedAt >= startedAt && pausedAt <= updatedAt) &&
                        moveStart != null && moveStart > endedAt &&
                        moveEnd != null && moveEnd > moveStart &&
                        Duration.between(moveStart, moveEnd).let { duration ->
                            duration.nano == 0 && duration <=
                                Duration.ofSeconds(MAX_DEFER_MOVE_WINDOW_SECONDS.toLong())
                        },
                )
            }
        } catch (error: Exception) {
            throw InvalidExecutionProtocolException(error)
        }
    }

    private fun RemoteExecutionSession.toSnapshot() = CanonicalExecutionSessionSnapshot(
        id = id,
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
        plannedBlockId = plannedBlockId,
        sourceDeviceId = sourceDeviceId,
        status = status,
        revision = revision,
        accumulatedSeconds = accumulatedSeconds,
        actualSeconds = actualSeconds,
        startedAt = startedAt,
        runningSince = runningSince,
        pausedAt = pausedAt,
        pauseUntil = pauseUntil,
        pauseReason = pauseReason,
        endedAt = endedAt,
        moveStart = moveStart,
        moveEnd = moveEnd,
        createdAt = createdAt,
        updatedAt = updatedAt,
    )

    private fun CanonicalExecutionSessionSnapshot.toRemoteSession() = RemoteExecutionSession(
        id = id,
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
        plannedBlockId = plannedBlockId,
        sourceDeviceId = sourceDeviceId,
        status = status,
        revision = revision,
        accumulatedSeconds = accumulatedSeconds,
        actualSeconds = actualSeconds,
        startedAt = startedAt,
        runningSince = runningSince,
        pausedAt = pausedAt,
        pauseUntil = pauseUntil,
        pauseReason = pauseReason,
        endedAt = endedAt,
        moveStart = moveStart,
        moveEnd = moveEnd,
        createdAt = createdAt,
        updatedAt = updatedAt,
    )

    private fun RemoteExecutionSession.immutableIdentity() = ExecutionIdentity(
        sessionId = id,
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
        plannedBlockId = plannedBlockId,
        sourceDeviceId = sourceDeviceId,
    )

    private fun CanonicalExecutionSessionSnapshot.immutableIdentity() = ExecutionIdentity(
        sessionId = id,
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
        plannedBlockId = plannedBlockId,
        sourceDeviceId = sourceDeviceId,
    )

    private fun PendingExecutionCommand.immutableIdentity() = ExecutionIdentity(
        sessionId = sessionId,
        itemId = itemId,
        itemRevision = itemRevision,
        occurrenceId = occurrenceId,
        sessionIndex = sessionIndex,
        plannedBlockId = plannedBlockId,
        sourceDeviceId = sourceDeviceId ?: throw InvalidExecutionProtocolException(),
    )

    private fun RemoteExecutionSession.hasSameImmutableIdentity(identity: ExecutionIdentity): Boolean =
        immutableIdentity() == identity

    /** Every server-owned field must remain byte-for-byte semantic once a closed row is cached. */
    private fun RemoteExecutionSession.hasSameRemoteSemantics(
        snapshot: CanonicalExecutionSessionSnapshot,
    ): Boolean = this == snapshot.toRemoteSession()

    private fun commandRequest(expectedRevision: Long, command: JsonObject): String =
        buildJsonObject {
            put("expected_revision", expectedRevision)
            put("command", command)
        }.toString()

    private fun JsonObject.requireString(name: String): String =
        getValue(name).jsonPrimitive.let { primitive ->
            require(primitive.isString)
            primitive.content
        }

    private fun JsonObject.requireLong(name: String): Long =
        getValue(name).jsonPrimitive.let { primitive ->
            require(!primitive.isString)
            primitive.long
        }

    private fun String.isCanonicalSha256Digest(): Boolean =
        length == 71 && startsWith("sha256:") &&
            drop(7).all { it in '0'..'9' || it in 'a'..'f' }

    private fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        val snapshot = credentialStore.snapshot()
        if (snapshot.baseUrl == null) {
            mutableState.value = ExecutionSyncState(
                CanonicalSyncPhase.NOT_CONFIGURED,
                "Configure the DayWeave API before canonical execution.",
            )
            return null
        }
        if (!snapshot.hasBearerToken) {
            mutableState.value = ExecutionSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Enter the bearer token to reconcile execution.",
            )
            return null
        }
        return try {
            credentialStore.authenticatedConfiguration().also {
                if (it == null) {
                    mutableState.value = ExecutionSyncState(
                        CanonicalSyncPhase.AUTH_REQUIRED,
                        "Enter the bearer token to reconcile execution.",
                    )
                }
            }
        } catch (_: SecureCredentialException) {
            mutableState.value = ExecutionSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "The encrypted bearer token is unavailable. Re-enter it to reconnect.",
            )
            null
        } catch (_: InvalidApiConfigurationException) {
            updateError("The stored API URL is invalid.")
            null
        }
    }

    private fun ensureConfigurationCurrent(configuration: AuthenticatedApiConfiguration) {
        val current = credentialStore.snapshot()
        if (
            current.baseUrl != configuration.baseUrl.toString() ||
            current.configurationId != configuration.configurationId ||
            !current.hasBearerToken
        ) {
            throw ConfigurationChangedException()
        }
    }

    private suspend fun withReadyStore(
        block: suspend () -> ExecutionSyncOutcome,
    ): ExecutionSyncOutcome {
        val load = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (load != PlannerLoadState.READY) {
            return handleFailure(LocalExecutionStorageException())
        }
        return block()
    }

    private fun handleFailure(error: Throwable): ExecutionSyncOutcome {
        if (error is CancellationException) {
            mutableState.value = initialState()
            throw error
        }
        if (error is ApiBindingChangedException) {
            mutableState.value = initialState()
            return when (mutableState.value.phase) {
                CanonicalSyncPhase.NOT_CONFIGURED -> ExecutionSyncOutcome.NOT_CONFIGURED
                CanonicalSyncPhase.AUTH_REQUIRED -> ExecutionSyncOutcome.AUTH_REQUIRED
                else -> ExecutionSyncOutcome.CONFIGURATION_CHANGED
            }
        }
        val (phase, message, outcome) = when (error) {
            is ReconciledCommandRejectionException -> Triple(
                CanonicalSyncPhase.ERROR,
                error.safeMessage,
                error.outcome,
            )
            is UnresolvedPendingCommandException -> Triple(
                CanonicalSyncPhase.ERROR,
                "The rejected retry could not be matched to stable server history; the exact command remains fenced.",
                ExecutionSyncOutcome.CONFLICT,
            )
            is UnresolvedRemoteTransitionException -> Triple(
                CanonicalSyncPhase.ERROR,
                "The previous execution lease is absent from bounded server history; local state remains locked.",
                ExecutionSyncOutcome.PROTOCOL_FAILURE,
            )
            is UnstableExecutionReadException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Execution changed during reconciliation; retry after the cross-device state settles.",
                ExecutionSyncOutcome.CONFLICT,
            )
            is ExecutionHistoryContinuityException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Execution history is incomplete within the bounded 100-session sync window. " +
                    "New starts remain locked until every authoritative revision can be proven.",
                ExecutionSyncOutcome.PROTOCOL_FAILURE,
            )
            is ExecutionApiException.Authentication -> Triple(
                CanonicalSyncPhase.AUTH_REQUIRED,
                when {
                    plannerStore.state.value.pendingExecutionCommand != null ->
                        "Authentication failed. The pending command is retained for an exact retry."
                    plannerStore.state.value.pendingExecutionDeferIntent != null ->
                        "Authentication failed. The move request is saved for reconciliation."
                    else ->
                        "Authentication failed. Re-enter the bearer token to reconcile execution."
                },
                ExecutionSyncOutcome.AUTH_REQUIRED,
            )
            is ExecutionApiException.InvalidResponse,
            is InvalidExecutionProtocolException,
            -> Triple(
                CanonicalSyncPhase.ERROR,
                "The server execution response is incompatible; no local success was invented.",
                ExecutionSyncOutcome.PROTOCOL_FAILURE,
            )
            is ExecutionApiException.Http -> Triple(
                if (error.statusCode == 408 || error.statusCode == 429 || error.statusCode >= 500) {
                    CanonicalSyncPhase.OFFLINE
                } else {
                    CanonicalSyncPhase.ERROR
                },
                "The execution API returned HTTP ${error.statusCode}; the exact command is retained.",
                if (error.statusCode == 408 || error.statusCode == 429 || error.statusCode >= 500) {
                    ExecutionSyncOutcome.RETRYABLE_SERVER_FAILURE
                } else {
                    ExecutionSyncOutcome.UNEXPECTED_FAILURE
                },
            )
            is IOException -> Triple(
                CanonicalSyncPhase.OFFLINE,
                when {
                    plannerStore.state.value.pendingExecutionCommand != null ->
                        "Offline · the exact execution command will be retried."
                    plannerStore.state.value.pendingExecutionDeferIntent != null ->
                        "Offline · the selected move time is saved and will resume after sync."
                    else -> "Offline · canonical execution was not changed locally."
                },
                ExecutionSyncOutcome.TRANSIENT_NETWORK_FAILURE,
            )
            is LocalExecutionStorageException -> Triple(
                CanonicalSyncPhase.ERROR,
                "Encrypted planner storage is unavailable; execution was disabled.",
                ExecutionSyncOutcome.LOCAL_STORAGE_FAILURE,
            )
            is InvalidExecutionStateException -> Triple(
                CanonicalSyncPhase.ERROR,
                error.message ?: "The canonical execution transition is unavailable.",
                ExecutionSyncOutcome.INVALID_LOCAL_STATE,
            )
            is ConfigurationChangedException -> Triple(
                CanonicalSyncPhase.ERROR,
                "API settings changed during execution reconciliation; the exact command remains fenced.",
                ExecutionSyncOutcome.CONFIGURATION_CHANGED,
            )
            else -> Triple(
                CanonicalSyncPhase.ERROR,
                "Canonical execution could not be reconciled safely.",
                ExecutionSyncOutcome.UNEXPECTED_FAILURE,
            )
        }
        mutableState.value = ExecutionSyncState(phase, message)
        return outcome
    }

    private fun updateBusy(message: String) {
        mutableState.value = ExecutionSyncState(CanonicalSyncPhase.SYNCING, message)
    }

    private fun updateConnected(message: String) {
        mutableState.value = ExecutionSyncState(CanonicalSyncPhase.CONNECTED, message)
    }

    private fun updateError(message: String) {
        mutableState.value = ExecutionSyncState(CanonicalSyncPhase.ERROR, message)
    }

    private fun initialState(): ExecutionSyncState {
        val connection = credentialStore.snapshot()
        return when {
            connection.baseUrl == null -> ExecutionSyncState(
                CanonicalSyncPhase.NOT_CONFIGURED,
                "Configure the API to synchronize execution.",
            )
            !connection.hasBearerToken -> ExecutionSyncState(
                CanonicalSyncPhase.AUTH_REQUIRED,
                "Enter the bearer token to synchronize execution.",
            )
            else -> ExecutionSyncState(
                CanonicalSyncPhase.READY,
                "Ready to reconcile cross-device execution.",
            )
        }
    }

    private fun stateOutcome(): ExecutionSyncOutcome = when (mutableState.value.phase) {
        CanonicalSyncPhase.NOT_CONFIGURED -> ExecutionSyncOutcome.NOT_CONFIGURED
        CanonicalSyncPhase.AUTH_REQUIRED -> ExecutionSyncOutcome.AUTH_REQUIRED
        else -> ExecutionSyncOutcome.UNEXPECTED_FAILURE
    }

    private data class CommandContext(
        val snapshot: RemoteExecutionSnapshot,
        val block: ScheduleItem,
        val deviceId: String,
    ) {
        fun requireActiveSession(blockId: String): RemoteExecutionSession {
            val active = snapshot.activeSession
                ?: throw InvalidExecutionStateException("No canonical execution lease is open.")
            val matches = active.itemId == block.canonicalItemId &&
                active.itemRevision == block.canonicalRevision &&
                active.occurrenceId == block.occurrenceId &&
                active.sessionIndex == block.sessionIndex
            if (!matches) {
                throw InvalidExecutionStateException(
                    "Another device owns a different execution session.",
                )
            }
            return active
        }
    }

    private data class CommandSpec(
        val type: String,
        val identity: ExecutionIdentity,
        val focusedBlockId: String,
        val command: JsonObject,
    )

    private data class ExecutionIdentity(
        val sessionId: String,
        val itemId: String,
        val itemRevision: Long,
        val occurrenceId: String?,
        val sessionIndex: Int,
        val plannedBlockId: String?,
        val sourceDeviceId: String,
    )

    private data class ExecutionProjectionTarget(
        val itemId: String,
        val itemRevision: Long,
        val occurrenceId: String?,
        val sessionIndex: Int,
    )

    private data class StableExecutionRead(
        val snapshot: RemoteExecutionSnapshot,
        val history: List<RemoteExecutionSession>,
    )

    private class LocalExecutionStorageException : IllegalStateException()
    private class ConfigurationChangedException : IllegalStateException()
    private class UnresolvedPendingCommandException : IllegalStateException()
    private class UnresolvedRemoteTransitionException : IllegalStateException()
    private class UnstableExecutionReadException : IllegalStateException()
    private class ExecutionHistoryContinuityException : IllegalStateException()
    private class ReconciledCommandRejectionException(
        val outcome: ExecutionSyncOutcome,
        val safeMessage: String,
    ) : IllegalStateException()
    private class InvalidExecutionProtocolException(cause: Throwable? = null) :
        IllegalArgumentException("Invalid execution protocol", cause)

    private class InvalidExecutionStateException(message: String) : IllegalStateException(message)

    private companion object {
        const val EXECUTION_DEFER_INTENT_SCHEMA_VERSION = 1
        const val EXECUTION_DEFER_SLOT_SECONDS = 5 * 60L
        const val EXECUTION_DEFER_ASSESSMENT_TTL_SECONDS = 5 * 60L
        const val EXECUTION_DEFER_TARGET_LEAD_SECONDS =
            EXECUTION_DEFER_ASSESSMENT_TTL_SECONDS + EXECUTION_DEFER_SLOT_SECONDS
        const val MAX_PAUSE_SECONDS = 24 * 60 * 60
        const val MAX_DEFER_MOVE_WINDOW_SECONDS = 24 * 60 * 60
        const val MAX_PENDING_REQUEST_CHARS = 64 * 1024
        const val MAX_HISTORY_SESSIONS = 100
        const val MAX_BOOTSTRAP_HISTORY_PAGES = 1_000
        const val MAX_STABLE_READ_ATTEMPTS = 2
        const val MAX_RECONCILED_COMMAND_ATTEMPTS = 2
        val NIL_UUID: UUID = UUID(0L, 0L)
        val OPEN_STATUSES = setOf("active", "paused")
        val TERMINAL_STATUSES = setOf("completed", "skipped")
        val CLOSED_STATUSES = TERMINAL_STATUSES + "deferred"
        val ALL_STATUSES = OPEN_STATUSES + CLOSED_STATUSES

        fun isSafeExecutionDeferTarget(target: Instant, reference: Instant): Boolean =
            target.nano == 0 && target.epochSecond % EXECUTION_DEFER_SLOT_SECONDS == 0L &&
                !target.isBefore(reference.plusSeconds(EXECUTION_DEFER_TARGET_LEAD_SECONDS))
    }
}
