package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
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
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
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
) {
    private val operationMutex = Mutex()
    private val mutableState = MutableStateFlow(initialState())
    private val json = Json {
        ignoreUnknownKeys = false
        explicitNulls = false
        encodeDefaults = true
    }
    val state: StateFlow<ExecutionSyncState> = mutableState.asStateFlow()

    suspend fun refresh(): ExecutionSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Reconciling cross-device execution…")
            try {
                ensureDeviceIdentity()
                beginHistoryVerification(configuration)
                val hadPending = plannerStore.state.value.pendingExecutionCommand != null
                if (hadPending) reconcilePending(configuration)
                reconcileSnapshot(
                    configuration,
                    transport.snapshot(configuration),
                    "Execution is synchronized across devices",
                )
                updateConnected("Execution is synchronized across devices")
                ExecutionSyncOutcome.SUCCESS
            } catch (error: Throwable) {
                handleFailure(error)
            }
        }
    }

    suspend fun start(blockId: String): ExecutionSyncOutcome = command(blockId) { context ->
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
        val sessionId = newUuid().toString()
        CommandSpec(
            type = "start",
            identity = ExecutionIdentity(
                sessionId = sessionId,
                itemId = itemId,
                itemRevision = itemRevision,
                occurrenceId = context.block.occurrenceId,
                sessionIndex = context.block.sessionIndex,
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
                put("session_index", context.block.sessionIndex)
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

    /** The execution API has no atomic "release and defer" command yet. */
    suspend fun doLater(blockId: String): ExecutionSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val state = plannerStore.state.value
            if (state.schedule.none { it.id == blockId && it.canonicalItemId != null }) {
                return@withLock ExecutionSyncOutcome.INVALID_LOCAL_STATE
            }
            updateError(
                "Will do later is unavailable while a canonical execution lease is open. " +
                    "Complete or skip this session first.",
            )
            ExecutionSyncOutcome.INVALID_LOCAL_STATE
        }
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

    private suspend fun command(
        blockId: String,
        build: (CommandContext) -> CommandSpec,
    ): ExecutionSyncOutcome = withReadyStore {
        operationMutex.withLock {
            val configuration = authenticatedConfiguration() ?: return@withLock stateOutcome()
            updateBusy("Checking the cross-device execution lease…")
            try {
                ensureDeviceIdentity()
                beginHistoryVerification(configuration)
                if (plannerStore.state.value.pendingExecutionCommand != null) {
                    reconcilePending(configuration)
                    reconcileSnapshot(
                        configuration,
                        transport.snapshot(configuration),
                        "Previous execution command reconciled",
                    )
                    updateConnected("Previous execution command reconciled; review state before retrying")
                    return@withLock ExecutionSyncOutcome.SUCCESS
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
                val staged = plannerStore.stageExecutionCommand(pending)
                if (staged == null || !staged.awaitDurable()) throw LocalExecutionStorageException()
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
                updateConnected("Execution updated across devices")
                ExecutionSyncOutcome.SUCCESS
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
        reconcileTerminalHistoryRows(
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
    ): RemoteExecutionSnapshot {
        val stable = readStableHistory(configuration, initialSnapshot)
        val continuityVerified = validateHistoryAgainstDurableState(configuration, stable)
        reconcileTerminalHistoryRows(configuration, stable, excludedSessionId = null, message)
        val receipt = plannerStore.reconcileCanonicalExecution(
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = configuration.configurationId,
            revision = stable.snapshot.revision,
            activeSession = stable.snapshot.activeSession?.toSnapshot(),
            message = message,
        )
        if (receipt == null || !receipt.awaitDurable()) throw LocalExecutionStorageException()
        persistHistoryWindow(configuration, stable, continuityVerified, message)
        if (!continuityVerified) throw ExecutionHistoryContinuityException()
        return stable.snapshot
    }

    private suspend fun reconcileTerminalHistoryRows(
        configuration: AuthenticatedApiConfiguration,
        stable: StableExecutionRead,
        excludedSessionId: String?,
        message: String,
    ) {
        // History is newest first. Only a target's newest session may control its presentation;
        // older terminal facts still belong in the immutable ledger, but a later open session must
        // not make an old completion/skip fight the authoritative active snapshot on every poll.
        val newestPresentableTerminalIds = stable.history.asSequence()
            .distinctBy { it.projectionTarget() }
            .filter { it.status in TERMINAL_STATUSES }
            .map { it.id }
            .toSet()
        stable.history.asReversed()
            .filter { it.status in TERMINAL_STATUSES && it.id != excludedSessionId }
            .forEach { terminal ->
                val alreadyDurable = plannerStore.state.value.terminalExecutionOutcomes[terminal.id]
                    ?.let { outcome ->
                        outcome.syncOrigin == configuration.baseUrl.toString() &&
                            terminal.hasSameRemoteSemantics(outcome.session) &&
                            (
                                terminal.id !in newestPresentableTerminalIds ||
                                    terminalPresentationIsConverged(terminal)
                            )
                    } == true
                if (alreadyDurable) return@forEach
                val receipt = plannerStore.reconcileCanonicalExecution(
                    syncOrigin = configuration.baseUrl.toString(),
                    configurationId = configuration.configurationId,
                    revision = stable.snapshot.revision,
                    activeSession = stable.snapshot.activeSession?.toSnapshot(),
                    changedSession = terminal.toSnapshot(),
                    message = message,
                )
                if (receipt == null || !receipt.awaitDurable()) {
                    throw LocalExecutionStorageException()
                }
            }
    }

    private fun terminalPresentationIsConverged(terminal: RemoteExecutionSession): Boolean {
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
                if (prior.status in TERMINAL_STATUSES) {
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
                    remote.status !in TERMINAL_STATUSES ||
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
            val missingTerminal = current.terminalExecutionOutcomes.values.any { outcome ->
                outcome.syncOrigin == configuration.baseUrl.toString() &&
                    outcome.session.id !in historyById
            }
            if (missingTerminal) throw InvalidExecutionProtocolException()
        }
        current.canonicalExecutionSession?.let { cached ->
            val stillActive = stable.snapshot.activeSession
                ?.hasSameImmutableIdentity(cached.immutableIdentity()) == true
            if (!stillActive) {
                val terminalMatches = stable.history.filter {
                    it.status in TERMINAL_STATUSES &&
                        it.hasSameImmutableIdentity(cached.immutableIdentity())
                }
                if (
                    terminalMatches.size > 1 ||
                    continuityVerified && terminalMatches.size != 1
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
            current.pendingExecutionCommand != null
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
            val quarantine = plannerStore.abandonCanonicalConnection()
            if (quarantine == null || !quarantine.awaitDurable()) {
                throw LocalExecutionStorageException()
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
        if (
            !prior.hasSameImmutableIdentity(pending.immutableIdentity()) ||
            !changed.hasSameImmutableIdentity(prior.immutableIdentity()) ||
            changed.revision != nextSessionRevision ||
            changed.startedAt != prior.startedAt || changed.createdAt != prior.createdAt
        ) {
            throw InvalidExecutionProtocolException()
        }
        val changedAt = Instant.parse(changed.updatedAt)
        val expectedElapsed = elapsedSeconds(prior, changedAt)
        when (pending.commandType) {
            "pause" -> {
                if (prior.status !in OPEN_STATUSES) throw InvalidExecutionProtocolException()
                val duration = command["duration_seconds"]?.jsonPrimitive?.long
                val absoluteUntil = command["pause_until"]?.jsonPrimitive?.content?.let(Instant::parse)
                val expectedUntil = duration?.let(changedAt::plusSeconds) ?: absoluteUntil
                val expectedReason = command["reason"]?.jsonPrimitive?.content ?: prior.pauseReason
                val expectedPausedAt = prior.pausedAt?.let(Instant::parse) ?: changedAt
                if (
                    changed.accumulatedSeconds != expectedElapsed ||
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
                val expectedActual = corrected ?: expectedElapsed
                val expectedPausedAt = prior.pausedAt?.let(Instant::parse)
                    ?: if (prior.status == "paused") changedAt else null
                if (
                    changed.accumulatedSeconds != expectedElapsed ||
                    changed.actualSeconds != expectedActual ||
                    changed.pausedAt?.let(Instant::parse) != expectedPausedAt
                ) {
                    throw InvalidExecutionProtocolException()
                }
            }
            else -> throw InvalidExecutionProtocolException()
        }
    }

    private fun elapsedSeconds(session: RemoteExecutionSession, at: Instant): Long {
        val runningSeconds = session.runningSince?.let { raw ->
            Duration.between(Instant.parse(raw), at).seconds.coerceAtLeast(0L)
        } ?: 0L
        return try {
            Math.addExact(session.accumulatedSeconds, runningSeconds)
        } catch (_: ArithmeticException) {
            Long.MAX_VALUE
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
                        endedAt == null && session.actualSeconds == null,
                )
                "paused" -> require(
                    runningSince == null && pausedAt != null && pausedAt >= startedAt &&
                        pausedAt <= updatedAt &&
                        (pauseUntil == null || pauseUntil > updatedAt &&
                            pauseUntil <= updatedAt.plusSeconds(MAX_PAUSE_SECONDS.toLong())) &&
                        endedAt == null && session.actualSeconds == null,
                )
                else -> require(
                    runningSince == null && pauseUntil == null && session.pauseReason == null &&
                        session.actualSeconds != null && endedAt == updatedAt &&
                        (pausedAt == null || pausedAt >= startedAt && pausedAt <= updatedAt),
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

    /** Every server-owned field must remain byte-for-byte semantic once a terminal row is cached. */
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
                if (plannerStore.state.value.pendingExecutionCommand != null) {
                    "Authentication failed. The pending command is retained for an exact retry."
                } else {
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
                if (plannerStore.state.value.pendingExecutionCommand != null) {
                    "Offline · canonical execution was not changed locally and will be retried exactly."
                } else {
                    "Offline · canonical execution was not changed locally."
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
        const val MAX_PAUSE_SECONDS = 24 * 60 * 60
        const val MAX_PENDING_REQUEST_CHARS = 64 * 1024
        const val MAX_HISTORY_SESSIONS = 100
        const val MAX_BOOTSTRAP_HISTORY_PAGES = 1_000
        const val MAX_STABLE_READ_ATTEMPTS = 2
        val NIL_UUID: UUID = UUID(0L, 0L)
        val OPEN_STATUSES = setOf("active", "paused")
        val TERMINAL_STATUSES = setOf("completed", "skipped")
        val ALL_STATUSES = OPEN_STATUSES + TERMINAL_STATUSES
    }
}
