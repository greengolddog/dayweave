package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.RecurrenceOutcomeSnapshot
import com.greengolddog.dayweave.model.RecurrenceMoveSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.TerminalExecutionOutcomeSnapshot
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import java.time.Instant
import java.time.ZoneId
import java.util.ArrayDeque
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

enum class PlannerLoadState {
    LOADING,
    READY,
    PERSISTENCE_FAILED,
}

/**
 * Acknowledges the exact planner generation produced by a server reconciliation.
 *
 * Awaiting is intentionally opt-in: ordinary UI mutations remain synchronous and enqueue their
 * encrypted save without blocking the caller.
 */
class PlannerPersistenceReceipt internal constructor(
    val generation: Long,
    private val completion: CompletableDeferred<Boolean>,
) {
    suspend fun awaitDurable(): Boolean = completion.await()
}

/**
 * Owns presentation state and serializes it to an optional offline repository.
 *
 * Mutations remain synchronous after restore. While encrypted state is loading—or after a storage
 * failure—writes are rejected so an action can never target preview or stale data.
 */
class PlannerStore(
    private val initialState: DayWeaveUiState = DayWeaveUiState.preview(),
    private val repository: PlannerStateRepository? = null,
    scope: CoroutineScope? = null,
    private val onPersistenceError: (Throwable) -> Unit = {},
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) {
    private val mutableState = MutableStateFlow(initialState)
    val state: StateFlow<DayWeaveUiState> = mutableState.asStateFlow()
    private val mutableLoadState = MutableStateFlow(
        if (repository == null) PlannerLoadState.READY else PlannerLoadState.LOADING,
    )
    val loadState: StateFlow<PlannerLoadState> = mutableLoadState.asStateFlow()

    private val persistenceLock = Any()
    private val saveRequests = Channel<Unit>(Channel.CONFLATED)
    private val persistenceReady = CompletableDeferred<Boolean>()
    private val exactSaveRequests = ArrayDeque<SaveRequest>()
    private var latestNormalSaveRequest: SaveRequest? = null
    private var currentGeneration = 0L
    private var persistedGeneration = 0L
    private var persistenceStatus = if (repository == null) {
        PersistenceStatus.DISABLED
    } else {
        PersistenceStatus.LOADING
    }

    init {
        if (repository != null) {
            requireNotNull(scope) { "A CoroutineScope is required when persistence is enabled" }
            scope.launch { restore(repository) }
            scope.launch { autosave(repository) }
        }
    }

    fun navigate(destination: AppDestination) {
        mutate { it.copy(destination = destination) }
    }

    fun startItem(id: String) {
        mutate { current ->
            if (current.schedule.none { it.id == id }) return@mutate current

            val schedule = current.schedule.map { item ->
                when {
                    item.id == id -> item.copy(status = ItemStatus.ACTIVE)
                    item.status == ItemStatus.ACTIVE -> item.copy(status = ItemStatus.PAUSED)
                    else -> item
                }
            }
            current.copy(
                schedule = schedule,
                activeSession = newRunningSession(id),
                scheduleMessage = "Started focus session",
            )
        }
    }

    fun pauseActive(minutes: Int? = null) {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            val pauseLabel = minutes?.let { "$it minute break" } ?: "Open-ended break"
            val paused = pauseSession(active, minutes)
            current.copy(
                schedule = current.schedule.map {
                    if (it.id == active.itemId) it.copy(status = ItemStatus.PAUSED) else it
                },
                activeSession = paused.copy(pauseLabel = pauseLabel),
                scheduleMessage = "Paused · remaining work is held tentatively",
            )
        }
    }

    fun resumeActive() {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            if (!active.isPaused) return@mutate current
            current.copy(
                schedule = current.schedule.map {
                    if (it.id == active.itemId) it.copy(status = ItemStatus.ACTIVE) else it
                },
                activeSession = resumeSession(active),
                scheduleMessage = "Focus session resumed",
            )
        }
    }

    fun completeActive() {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            val elapsedMinutes = completedMinutes(active)
            current.copy(
                schedule = current.schedule.map { item ->
                    if (item.id == active.itemId) {
                        item.copy(
                            status = ItemStatus.COMPLETED,
                            actualMinutes = elapsedMinutes.coerceAtLeast(1),
                        )
                    } else {
                        item
                    }
                },
                activeSession = null,
                scheduleMessage = "Completed · later flexible work was checked",
            )
        }
    }

    fun skipActive() {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            current.copy(
                schedule = current.schedule.map {
                    if (it.id == active.itemId) it.copy(status = ItemStatus.SKIPPED) else it
                },
                activeSession = null,
                scheduleMessage = "Skipped · recurrence policy will decide the next occurrence",
            )
        }
    }

    fun doActiveLater() {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            current.copy(
                schedule = current.schedule.map {
                    if (it.id == active.itemId) {
                        it.copy(status = ItemStatus.SCHEDULED, startMinute = it.startMinute + 60)
                    } else {
                        it
                    }
                }.sortedBy { it.startMinute },
                activeSession = null,
                scheduleMessage = "Moved one hour later · no hard constraints were crossed",
            )
        }
    }

    /** Advances the visible timer without writing more than once per displayed minute. */
    fun tickActiveSession(): Boolean {
        val active = mutableState.value.activeSession ?: return false
        if (active.isPaused) {
            val deadline = active.pauseUntilEpochMillis ?: return false
            if (nowEpochMillis() < deadline || active.timedBreakEnded) return false
            return mutate { current ->
                val latest = current.activeSession
                if (
                    latest == null || !latest.isPaused ||
                    latest.pauseUntilEpochMillis != deadline || latest.timedBreakEnded
                ) {
                    current
                } else {
                    current.copy(
                        activeSession = latest.copy(
                            timedBreakEnded = true,
                            pauseLabel = "Break ended · choose what to do next",
                        ),
                    )
                }
            }
        }
        val elapsed = elapsedMinutes(active)
        if (elapsed == active.elapsedMinutes) return false
        return mutate { current ->
            val latest = current.activeSession?.takeIf { it.itemId == active.itemId }
                ?: return@mutate current
            current.copy(activeSession = latest.copy(elapsedMinutes = elapsedMinutes(latest)))
        }
    }

    fun timedPauseReady(): Boolean {
        val active = mutableState.value.activeSession ?: return false
        val deadline = active.pauseUntilEpochMillis ?: return false
        return active.isPaused && nowEpochMillis() >= deadline
    }

    fun quickCapture(title: String, kind: ItemKind): Boolean {
        val trimmed = title.trim()
        if (trimmed.isEmpty()) return false
        val captureId = UUID.randomUUID().toString()

        return mutate { current ->
            current.copy(
                inbox = listOf(
                    InboxItem(
                        id = captureId,
                        title = trimmed,
                        source = InboxSource.QUICK_CAPTURE,
                        detail = "${kind.label} · needs duration and constraints",
                    ),
                ) + current.inbox,
                scheduleMessage = "Captured to Inbox · nothing was scheduled yet",
            )
        }
    }

    /**
     * Safety boundary for ChatGPT, Codex, and assistant proposals.
     * Approval stages a reviewable Inbox draft and intentionally never mutates [DayWeaveUiState.schedule].
     */
    fun approveSuggestion(id: String) {
        mutate { current ->
            val suggestion = current.suggestions.firstOrNull { it.id == id }
                ?: return@mutate current
            if (suggestion.disposition != SuggestionDisposition.PENDING) return@mutate current

            current.copy(
                suggestions = current.suggestions.map {
                    if (it.id == id) it.copy(disposition = SuggestionDisposition.APPROVED_FOR_INBOX) else it
                },
                inbox = listOf(
                    suggestion.toInboxDraft(),
                ) + current.inbox,
                scheduleMessage = "Accepted as an Inbox draft · review before scheduling",
            )
        }
    }

    fun rejectSuggestion(id: String) {
        mutate { current ->
            current.copy(
                suggestions = current.suggestions.map {
                    if (it.id == id) it.copy(disposition = SuggestionDisposition.REJECTED) else it
                },
                scheduleMessage = "Suggestion rejected · your plan was not changed",
            )
        }
    }

    fun updateSuggestion(id: String, title: String, summary: String) {
        val safeTitle = title.trim()
        val safeSummary = summary.trim()
        if (safeTitle.isEmpty() || safeSummary.isEmpty()) return

        mutate { current ->
            current.copy(
                suggestions = current.suggestions.map {
                    if (it.id == id && it.disposition == SuggestionDisposition.PENDING) {
                        it.copy(title = safeTitle, summary = safeSummary)
                    } else {
                        it
                    }
                },
            )
        }
    }

    /** Replaces only server-backed proposals; local drafts remain untouched. */
    fun replaceRemoteSuggestions(
        suggestions: List<PlanningSuggestion>,
    ): PlannerPersistenceReceipt? {
        require(suggestions.all { it.remoteRevision != null }) {
            "Remote suggestions must include a server revision"
        }
        return mutateDurably { current ->
            val remoteIds = suggestions.asSequence().map(PlanningSuggestion::id).toHashSet()
            val localSuggestions = current.suggestions.filter {
                it.remoteRevision == null && it.id !in remoteIds
            }
            val drafts = missingAcceptedDrafts(current.inbox, suggestions)
            current.copy(
                suggestions = suggestions + localSuggestions,
                inbox = drafts + current.inbox,
            )
        }
    }

    /** Reconciles a mutation response without ever applying its payload to the schedule. */
    fun reconcileRemoteSuggestion(
        suggestion: PlanningSuggestion,
    ): PlannerPersistenceReceipt? {
        require(suggestion.remoteRevision != null) {
            "A reconciled remote suggestion must include a server revision"
        }
        return mutateDurably { current ->
            val replaced = current.suggestions.any {
                it.id == suggestion.id && it.remoteRevision != null
            }
            val merged = if (replaced) {
                current.suggestions.map {
                    if (it.id == suggestion.id && it.remoteRevision != null) suggestion else it
                }
            } else {
                listOf(suggestion) + current.suggestions
            }
            val drafts = missingAcceptedDrafts(current.inbox, listOf(suggestion))
            val message = when (suggestion.disposition) {
                SuggestionDisposition.APPROVED_FOR_INBOX ->
                    "Accepted as an Inbox draft · review before scheduling"
                SuggestionDisposition.REJECTED ->
                    "Suggestion rejected · your plan was not changed"
                SuggestionDisposition.EXPIRED ->
                    "Suggestion expired · your plan was not changed"
                SuggestionDisposition.PENDING -> current.scheduleMessage
            }
            current.copy(
                suggestions = merged,
                inbox = drafts + current.inbox,
                scheduleMessage = message,
            )
        }
    }

    /**
     * Replaces the canonical cache and composed timeline in one encrypted generation.
     *
     * Network code must fully validate and map a preview before calling this method. The store
     * repeats identity/revision checks so a partial or internally inconsistent result can never
     * replace the last durable plan.
     */
    fun replaceCanonicalPlan(update: CanonicalPlanUpdate): PlannerPersistenceReceipt? {
        require(update.inputDigest.startsWith("sha256:") && update.inputDigest.length > 7) {
            "Canonical schedule digest is invalid"
        }
        require(update.syncOrigin.isNotBlank() && update.deltaCursor.isNotBlank()) {
            "Canonical synchronization metadata is invalid"
        }
        require(update.planningZoneId.isNotBlank()) { "Canonical planning zone is invalid" }
        val planningZone = ZoneId.of(update.planningZoneId)
        val planningDate = Instant.parse(update.generatedAt).atZone(planningZone).toLocalDate()
        require(update.rejectedItemCount >= 0 && update.unscheduledItemCount >= 0) {
            "Canonical schedule counts cannot be negative"
        }
        require(
            update.violationCount >= update.violationMessages.size &&
                update.errorViolationCount in 0..update.violationCount &&
                update.violationMessages.size <= 100
        ) { "Canonical schedule violations are invalid" }
        require(update.protectedFreeMinutes >= 0 && update.dayScore in 0..100) {
            "Canonical schedule metrics are invalid"
        }
        val itemsById = update.items.associateBy { it.id }
        require(itemsById.size == update.items.size) { "Canonical item ids must be unique" }
        require(
            update.unscheduledWork.all {
                it.itemId in itemsById && it.remainingMinutes >= 0 && it.reason.isNotBlank()
            } && update.unscheduledWork.map { it.itemId to it.occurrenceId }.distinct().size ==
            update.unscheduledWork.size
        ) { "Canonical unscheduled work is invalid" }
        require(update.occurrenceSeriesItemIds.all { (occurrenceId, seriesItemId) ->
            runCatching { UUID.fromString(occurrenceId) }.isSuccess && seriesItemId in itemsById
        }) { "Canonical occurrence ownership is invalid" }
        require(update.items.all { it.id.isNotBlank() && it.revision > 0 && it.deletedAt == null }) {
            "Canonical items must be active, identified, positive revisions"
        }
        require(update.schedule.map { it.id }.distinct().size == update.schedule.size) {
            "Canonical schedule block ids must be unique"
        }
        require(
            update.schedule.all { block ->
                val canonicalId = block.canonicalItemId ?: return@all true
                val item = itemsById[canonicalId] ?: return@all false
                block.canonicalRevision == item.revision
            },
        ) { "Canonical schedule references an unknown or stale item revision" }

        return mutateDurably { current ->
            val canonicalBindingCompatible = current.canonicalSyncOrigin == null ||
                current.canonicalSyncOrigin == update.syncOrigin &&
                current.canonicalConfigurationId == update.configurationId
            val executionBindingCompatible = current.canonicalExecutionSyncOrigin == null ||
                current.canonicalExecutionSyncOrigin == update.syncOrigin &&
                current.canonicalExecutionConfigurationId == update.configurationId
            val sameBinding = canonicalBindingCompatible && executionBindingCompatible
            if (!sameBinding) {
                require(
                    current.canonicalItems.isEmpty() &&
                        current.canonicalDeltaCursor == null &&
                        current.pendingCanonicalMutation == null &&
                        current.pendingExecutionCommand == null &&
                        current.canonicalExecutionSession == null &&
                        current.canonicalExecutionHistoryWindow.isEmpty() &&
                        current.canonicalExecutionHistoryWindowRevision == null &&
                        !current.canonicalExecutionHistoryContinuityEstablished &&
                        !current.canonicalExecutionHistoryVerified &&
                        current.terminalExecutionOutcomes.isEmpty(),
                ) { "Credential replacement must quarantine canonical state before composition" }
            }
            val retainedTerminalOutcomes = if (sameBinding) {
                retainedTerminalExecutionOutcomes(
                    validatedTerminalExecutionOutcomes(current.terminalExecutionOutcomes)
                        .filter { it.syncOrigin == update.syncOrigin }
                        .associateBy { it.session.id },
                )
            } else {
                emptyMap()
            }
            val terminalOutcomesNewestFirst = retainedTerminalOutcomes.values.sortedWith(
                compareByDescending<TerminalExecutionOutcomeSnapshot> {
                    Instant.parse(it.session.updatedAt)
                }.thenByDescending { it.session.id },
            )
            val authoritativeLease = current.canonicalExecutionSession
                ?.takeIf { sameBinding && it.status in OPEN_EXECUTION_STATUSES }
            val freshSchedule = update.schedule.map { block ->
                val newestTerminal = terminalOutcomesNewestFirst.firstOrNull { outcome ->
                    outcome.session.matchesProjectionLineage(block)
                }
                val terminal = newestTerminal?.takeIf { outcome ->
                    !outcome.userKeptLatestItem() &&
                        (outcome.session.matches(block) || outcome.canSafelyOverlayRebased(
                        block = block,
                        itemsById = itemsById,
                        schedule = update.schedule,
                        unscheduledWork = update.unscheduledWork,
                    ))
                }
                when {
                    authoritativeLease?.matches(block) == true -> block.copy(
                        status = if (authoritativeLease.status == "active") {
                            ItemStatus.ACTIVE
                        } else {
                            ItemStatus.PAUSED
                        },
                    )
                    terminal != null -> block.copy(
                        status = terminal.session.terminalDisplayStatus(),
                        actualMinutes = terminal.session.actualMinutes(),
                    )
                    else -> block
                }
            }.sortedWith(SCHEDULE_ORDER)
            val freshIdentities = freshSchedule.mapTo(hashSetOf(), ScheduleIdentity::from)
            val retainedHistory = if (sameBinding) {
                current.schedule.filter { block ->
                    block.status in TERMINAL_SESSION_STATUSES &&
                        ScheduleIdentity.from(block) !in freshIdentities &&
                        block.canonicalItemId?.let(itemsById::get)?.revision == block.canonicalRevision &&
                        block.timelineInstant()?.atZone(planningZone)?.toLocalDate() == planningDate &&
                        (
                            block.occurrenceId?.let(current.recurrenceOutcomes::containsKey) == true ||
                                block.canonicalItemId?.let(itemsById::get)?.status in
                                setOf("completed", "skipped")
                            )
                }
            } else {
                emptyList()
            }
            val retainedRemoteLease = if (sameBinding) {
                current.canonicalExecutionSession
                    ?.takeIf { it.status in OPEN_EXECUTION_STATUSES }
                    ?.let { lease ->
                        current.schedule.filter { block ->
                            lease.matches(block) && block.status in OPEN_DISPLAY_STATUSES &&
                                freshSchedule.none { lease.matches(it) }
                        }
                    }
                    .orEmpty()
            } else {
                emptyList()
            }
            val orderedSchedule = (freshSchedule + retainedHistory + retainedRemoteLease)
                .distinctBy(ScheduleItem::id)
                .sortedWith(SCHEDULE_ORDER)
            val previousActiveBlock = current.activeSession
                ?.takeIf { sameBinding }
                ?.let { session -> current.schedule.firstOrNull { it.id == session.itemId } }
            val restoredBlock = previousActiveBlock?.let { previous ->
                orderedSchedule.firstOrNull { it.id == previous.id }
                    ?: orderedSchedule.firstOrNull {
                        ScheduleIdentity.from(it) == ScheduleIdentity.from(previous)
                    }
            }?.takeIf { it.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED) }
            val retainedSession = current.activeSession?.takeIf { restoredBlock != null }
            val restoredSession = retainedSession?.let { session ->
                val block = requireNotNull(restoredBlock)
                val transitioned = when {
                    block.status == ItemStatus.PAUSED && !session.isPaused ->
                        pauseSession(session, null)
                    block.status == ItemStatus.ACTIVE && session.isPaused ->
                        resumeSession(session)
                    else -> session
                }
                transitioned.copy(
                    itemId = block.id,
                    pauseLabel = if (block.status == ItemStatus.PAUSED) {
                        transitioned.pauseLabel ?: "Open-ended break"
                    } else {
                        null
                    },
                )
            } ?: orderedSchedule
                .firstOrNull { it.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED) }
                ?.let { block ->
                    val running = newRunningSession(block.id)
                    if (block.status == ItemStatus.PAUSED) {
                        pauseSession(running, null).copy(pauseLabel = "Open-ended break")
                    } else {
                        running
                    }
                }
            current.copy(
                canonicalItems = update.items.sortedWith(
                    compareBy({ it.parentId.orEmpty() }, { it.siblingOrder }, { it.id }),
                ),
                canonicalSyncOrigin = update.syncOrigin,
                canonicalConfigurationId = update.configurationId,
                canonicalDeltaCursor = update.deltaCursor,
                schedule = orderedSchedule,
                activeSession = restoredSession,
                scheduleInputDigest = update.inputDigest,
                scheduleGeneratedAt = update.generatedAt,
                schedulePlanningZoneId = update.planningZoneId,
                recurrenceOutcomes = if (sameBinding) {
                    current.recurrenceOutcomes.filterValues { it.itemId in itemsById }
                } else {
                    emptyMap()
                },
                recurrenceCompletionAnchors = if (sameBinding) {
                    current.recurrenceCompletionAnchors.filterKeys { it in itemsById }
                } else {
                    emptyMap()
                },
                recurrenceMoves = if (sameBinding) {
                    current.recurrenceMoves.filterValues { it.itemId in itemsById }
                } else {
                    emptyMap()
                },
                // A plan read alone cannot prove that a timed-out write will not commit later.
                // The sync manager clears this only by replaying the exact durable request.
                pendingCanonicalMutation = current.pendingCanonicalMutation,
                terminalExecutionOutcomes = retainedTerminalOutcomes,
                unscheduledWork = update.unscheduledWork,
                occurrenceSeriesItemIds = update.occurrenceSeriesItemIds,
                rejectedCanonicalItemCount = update.rejectedItemCount,
                unscheduledCanonicalItemCount = update.unscheduledItemCount,
                scheduleViolationMessages = update.violationMessages,
                scheduleViolationCount = update.violationCount,
                scheduleErrorViolationCount = update.errorViolationCount,
                protectedFreeMinutes = update.protectedFreeMinutes,
                dayScore = update.dayScore,
                scheduleMessage = update.message,
            )
        }
    }

    /** Persists the idempotency fence before a canonical mutation can leave the device. */
    fun stageCanonicalMutation(
        mutation: PendingCanonicalMutation,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingCanonicalMutation == null) {
            "A canonical mutation already needs reconciliation"
        }
        require(current.pendingExecutionCommand == null) {
            "An execution command already needs reconciliation"
        }
        require(UUID.fromString(mutation.idempotencyKey).toString() == mutation.idempotencyKey)
        require(UUID.fromString(mutation.itemId).toString() == mutation.itemId)
        require(mutation.syncOrigin == current.canonicalSyncOrigin)
        require(mutation.configurationId == current.canonicalConfigurationId)
        require(mutation.expectedRevision > 0 && mutation.targetStatus.isNotBlank())
        Instant.parse(mutation.startedAt)
        require(mutation.replacementRequestJson.isNotBlank())
        require(UUID.fromString(mutation.focusedBlockId).toString() == mutation.focusedBlockId)
        require(mutation.pauseMinutes == null || mutation.pauseMinutes in 1..24 * 60)
        mutation.terminalExecutionSessionId?.let { sessionId ->
            require(UUID.fromString(sessionId).toString() == sessionId)
            val outcome = current.terminalExecutionOutcomes[sessionId]
                ?: throw IllegalArgumentException("Terminal execution outcome is unavailable")
            validateTerminalExecutionOutcome(outcome)
            require(
                outcome.requiresCanonicalItemProjection &&
                    outcome.isProjectionWriteAuthorized() &&
                    outcome.syncOrigin == mutation.syncOrigin &&
                    outcome.session.itemId == mutation.itemId &&
                    outcome.session.itemRevision <= mutation.expectedRevision &&
                    outcome.session.status == mutation.targetStatus &&
                    current.schedule.firstOrNull { it.id == mutation.focusedBlockId }
                        ?.let { block ->
                            outcome.session.matchesProjectionTarget(
                                block = block,
                                expectedRevision = mutation.expectedRevision,
                            )
                        } == true
            ) { "Canonical projection does not match its terminal execution" }
        }
        current.copy(
            pendingCanonicalMutation = mutation,
            scheduleMessage = "Saving canonical state · awaiting authoritative confirmation",
        )
    }

    fun clearPendingCanonicalMutation(
        idempotencyKey: String,
        message: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingCanonicalMutation?.idempotencyKey == idempotencyKey) {
            "Canonical mutation fence changed during reconciliation"
        }
        current.copy(
            pendingCanonicalMutation = null,
            scheduleMessage = message,
        )
    }

    /**
     * Atomically closes a rejected projection only after an authoritative delta proved deletion.
     *
     * The delta cursor deliberately remains unchanged so the next normal compose replays and
     * persists every change from that page; only the proven item is removed from actionable local
     * state here, preventing a crash between fence release and composition from resurrecting it.
     */
    fun resolveDeletedPendingTerminalProjection(
        idempotencyKey: String,
        sessionId: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val pending = current.pendingCanonicalMutation
            ?: throw IllegalArgumentException("No canonical mutation is pending")
        require(pending.idempotencyKey == idempotencyKey)
        require(pending.terminalExecutionSessionId == sessionId)
        val outcome = requireTerminalProjection(current, sessionId)
        require(outcome.session.itemId == pending.itemId)
        val removedBlockIds = current.schedule.asSequence()
            .filter { it.canonicalItemId == pending.itemId }
            .map(ScheduleItem::id)
            .toSet()
        current.copy(
            canonicalItems = current.canonicalItems.filterNot { it.id == pending.itemId },
            schedule = current.schedule.filterNot { it.canonicalItemId == pending.itemId },
            activeSession = current.activeSession?.takeUnless { it.itemId in removedBlockIds },
            pendingCanonicalMutation = null,
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRevision = null,
                    canonicalProjectionResolution = TERMINAL_PROJECTION_ITEM_DELETED,
                    canonicalProjectionConflict = null,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            ),
            scheduleMessage = "Execution history retained · its canonical item was deleted",
        )
    }

    /** Deterministic rejection consumes one approval and releases its exact fence atomically. */
    fun rejectPendingTerminalProjectionAsConflict(
        idempotencyKey: String,
        sessionId: String,
        conflict: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(conflict.isNotBlank() && conflict.length <= MAX_TERMINAL_PROJECTION_CONFLICT_CHARS)
        val pending = current.pendingCanonicalMutation
            ?: throw IllegalArgumentException("No canonical mutation is pending")
        require(pending.idempotencyKey == idempotencyKey)
        require(pending.terminalExecutionSessionId == sessionId)
        val outcome = requireTerminalProjection(current, sessionId)
        require(outcome.session.itemId == pending.itemId)
        current.copy(
            pendingCanonicalMutation = null,
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRevision = null,
                    canonicalProjectionResolution = null,
                    canonicalProjectionConflict = conflict,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            ),
            scheduleMessage = "Execution outcome needs review before this item can run again",
        )
    }

    /** Creates the cross-device identity once, inside the encrypted planner snapshot. */
    fun ensureExecutionDeviceId(candidate: String): PlannerPersistenceReceipt? =
        mutateDurably { current ->
            val candidateId = UUID.fromString(candidate)
            require(candidateId != NIL_UUID && candidateId.toString() == candidate)
            val existing = current.executionDeviceId
            if (existing != null) {
                val existingId = UUID.fromString(existing)
                require(existingId != NIL_UUID && existingId.toString() == existing)
                current
            } else {
                current.copy(executionDeviceId = candidate)
            }
        }

    /** A confirmed terminal row or unresolved parent projection is a durable start fence. */
    fun isCanonicalExecutionStartBlocked(blockId: String): Boolean {
        val current = state.value
        val block = current.schedule.firstOrNull { it.id == blockId } ?: return true
        val itemId = block.canonicalItemId ?: return true
        val origin = current.canonicalSyncOrigin ?: return true
        if (
            current.canonicalExecutionSyncOrigin != origin ||
            current.canonicalExecutionConfigurationId != current.canonicalConfigurationId ||
            !current.canonicalExecutionHistoryVerified
        ) {
            return true
        }
        val validated = validatedTerminalExecutionOutcomes(current.terminalExecutionOutcomes)
        if (validated.isEmpty()) return false
        val outcomes = validated
            .filter { it.syncOrigin == origin }
        return outcomes.any {
            !it.userKeptLatestItem() && it.session.matches(block)
        } || outcomes.any { outcome ->
            outcome.isProjectionUnresolved() &&
                outcome.session.itemId == itemId
        }
    }

    /** Resolves an already-authoritative terminal item without issuing a duplicate write. */
    fun markTerminalProjectionApplied(
        sessionId: String,
        canonicalRevision: Long,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val outcome = requireTerminalProjection(current, sessionId)
        val item = current.canonicalItems.firstOrNull { it.id == outcome.session.itemId }
            ?: throw IllegalArgumentException("Canonical projection item is unavailable")
        require(
            item.revision == canonicalRevision && item.status == outcome.session.status &&
                canonicalRevision >= outcome.session.itemRevision
        ) { "Canonical item does not resolve this terminal execution" }
        current.copy(
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRevision = canonicalRevision,
                    canonicalProjectionResolution = null,
                    canonicalProjectionConflict = null,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            ),
            scheduleMessage = "Execution outcome already matches the latest canonical item",
        )
    }

    /** A canonical tombstone makes parent projection unnecessary while retaining exact history. */
    fun resolveDeletedTerminalProjection(
        sessionId: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val outcome = requireTerminalProjection(current, sessionId)
        require(current.canonicalItems.none { it.id == outcome.session.itemId })
        current.copy(
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRevision = null,
                    canonicalProjectionResolution = TERMINAL_PROJECTION_ITEM_DELETED,
                    canonicalProjectionConflict = null,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            ),
            scheduleMessage = "Execution history retained · its canonical item was deleted",
        )
    }

    /** Persists a reviewable conflict instead of retrying an unsafe projection forever. */
    fun recordTerminalProjectionConflict(
        sessionId: String,
        conflict: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(conflict.isNotBlank() && conflict.length <= MAX_TERMINAL_PROJECTION_CONFLICT_CHARS)
        val outcome = requireTerminalProjection(current, sessionId)
        current.copy(
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRevision = null,
                    canonicalProjectionResolution = null,
                    canonicalProjectionConflict = conflict,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            ),
            scheduleMessage = "Execution outcome needs review before this item can run again",
        )
    }

    /** Persists explicit user approval before any retry can issue network I/O. */
    fun authorizeTerminalProjectionRetry(
        sessionId: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingCanonicalMutation == null) {
            "A canonical write already needs reconciliation"
        }
        val outcome = requireTerminalProjection(current, sessionId)
        require(outcome.canonicalProjectionConflict != null)
        val authorizedAt = Instant.ofEpochMilli(nowEpochMillis()).toString()
        current.copy(
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRetryAuthorizedAt = authorizedAt,
                )
            ),
            scheduleMessage =
                "Retry approved · reconciling the terminal outcome against the latest item",
        )
    }

    /** Explicitly treats the latest incompatible item as new work while preserving old history. */
    fun keepLatestItemAfterTerminalConflict(
        sessionId: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingCanonicalMutation == null) {
            "A canonical write already needs reconciliation"
        }
        val outcome = requireTerminalProjection(current, sessionId)
        require(outcome.canonicalProjectionConflict != null)
        current.copy(
            terminalExecutionOutcomes = current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionResolution = TERMINAL_PROJECTION_USER_KEPT_LATEST,
                    canonicalProjectionConflict = null,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            ),
            scheduleMessage =
                "Terminal history kept · the latest canonical item remains separate work",
        )
    }

    /** Persists the exact command and idempotency key before execution network I/O. */
    fun stageExecutionCommand(
        command: PendingExecutionCommand,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingExecutionCommand == null) {
            "An execution command already needs reconciliation"
        }
        require(current.pendingCanonicalMutation == null) {
            "A legacy canonical mutation already needs reconciliation"
        }
        require(UUID.fromString(command.idempotencyKey).toString() == command.idempotencyKey)
        require(UUID.fromString(command.sessionId).toString() == command.sessionId)
        require(UUID.fromString(command.itemId).toString() == command.itemId)
        command.plannedBlockId?.let { require(UUID.fromString(it).toString() == it) }
        val sourceDeviceId = requireNotNull(command.sourceDeviceId) {
            "The authoritative execution device identity is unavailable"
        }
        require(UUID.fromString(sourceDeviceId).toString() == sourceDeviceId)
        command.occurrenceId?.let { require(UUID.fromString(it).toString() == it) }
        require(UUID.fromString(command.focusedBlockId).toString() == command.focusedBlockId)
        require(command.itemRevision > 0 && command.sessionIndex in 0..UShort.MAX_VALUE.toInt())
        require(command.syncOrigin == current.canonicalExecutionSyncOrigin)
        require(command.configurationId == current.canonicalExecutionConfigurationId)
        require(command.expectedRevision == current.canonicalExecutionRevision)
        require(command.commandType in EXECUTION_COMMAND_TYPES)
        require(command.requestJson.length in 2..MAX_PENDING_EXECUTION_REQUEST_CHARS)
        Instant.parse(command.startedAt)
        val focused = current.schedule.firstOrNull { it.id == command.focusedBlockId }
            ?: throw IllegalArgumentException("The execution block changed before staging")
        require(
            focused.canonicalItemId == command.itemId &&
                focused.canonicalRevision == command.itemRevision &&
                focused.occurrenceId == command.occurrenceId &&
                focused.sessionIndex == command.sessionIndex,
        ) { "The execution identity changed before staging" }
        if (command.commandType == "start") {
            require(command.plannedBlockId == focused.id)
            require(command.sourceDeviceId == current.executionDeviceId)
        } else {
            val authoritative = current.canonicalExecutionSession
                ?: throw IllegalArgumentException("The authoritative execution lease is unavailable")
            require(command.hasSameImmutableIdentity(authoritative)) {
                "The execution command is not bound to the authoritative lease"
            }
        }
        val projectionEligibleAtLeaseStart = if (command.commandType == "start") {
            requiresCanonicalItemProjection(current, focused)
        } else {
            current.canonicalExecutionSession
                ?.canonicalProjectionEligibleAtLeaseStart == true
        }
        current.copy(
            pendingExecutionCommand = command.copy(
                canonicalProjectionEligibleAtLeaseStart = projectionEligibleAtLeaseStart,
            ),
            scheduleMessage = "Saving execution command · awaiting authoritative confirmation",
        )
    }

    /** No bearer replacement may cross an unresolved or explicitly authorized server write. */
    fun hasCredentialReplacementBlocker(): Boolean {
        val current = state.value
        val projectionBlocked = runCatching {
            validatedTerminalExecutionOutcomes(current.terminalExecutionOutcomes).any {
                it.requiresCanonicalItemProjection &&
                    it.canonicalProjectionRevision == null &&
                    it.canonicalProjectionResolution == null &&
                    (
                        it.canonicalProjectionConflict == null ||
                            it.canonicalProjectionRetryAuthorizedAt != null
                        )
            }
        }.getOrElse { true }
        return current.pendingCanonicalMutation != null ||
            current.pendingExecutionCommand != null ||
            projectionBlocked
    }

    /**
     * Starts one durable history-verification cycle before any execution network read.
     *
     * A failed or cancelled poll therefore leaves starts fenced across process death. The prior
     * complete-baseline marker and window remain available solely to prove the next bounded page
     * overlaps; they never make this in-progress cycle start-safe.
     */
    fun markCanonicalExecutionHistoryUnverified(
        syncOrigin: String,
        configurationId: String?,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(syncOrigin.isNotBlank())
        current.canonicalSyncOrigin?.let { canonicalOrigin ->
            require(
                canonicalOrigin == syncOrigin &&
                    current.canonicalConfigurationId == configurationId,
            ) { "Execution credentials do not match the canonical cache binding" }
        }
        current.canonicalExecutionSyncOrigin?.let { executionOrigin ->
            require(
                executionOrigin == syncOrigin &&
                    current.canonicalExecutionConfigurationId == configurationId,
            ) { "Execution credentials do not match the durable execution binding" }
        }
        current.copy(
            canonicalExecutionSyncOrigin = syncOrigin,
            canonicalExecutionConfigurationId = configurationId,
            canonicalExecutionHistoryVerified = false,
            scheduleMessage = "Checking bounded execution history before allowing new starts",
        )
    }

    /** Persists the exact stable newest-first history window and its continuity verdict. */
    fun recordCanonicalExecutionHistoryWindow(
        syncOrigin: String,
        configurationId: String?,
        revision: Long,
        history: List<CanonicalExecutionSessionSnapshot>,
        continuityVerified: Boolean,
        message: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(
            current.canonicalExecutionSyncOrigin == syncOrigin &&
                current.canonicalExecutionConfigurationId == configurationId &&
                current.canonicalExecutionRevision == revision,
        ) { "Execution history does not match the reconciled snapshot binding" }
        require(history.map { it.id }.distinct().size == history.size) {
            "Execution history session ids must be unique"
        }
        history.forEach { session ->
            validateExecutionSession(session, mustBeOpen = false)
            require(session.canonicalProjectionEligibleAtLeaseStart == null) {
                "Server history cannot assert local projection provenance"
            }
            require(session.revision <= revision) {
                "Execution history is newer than its stable workspace snapshot"
            }
        }
        require(history.isNewestFirstExecutionHistory()) {
            "Execution history must be newest first"
        }
        require((revision == 0L) == history.isEmpty()) {
            "Execution history is incoherent with its workspace revision"
        }
        val openRows = history.filter { it.status in OPEN_EXECUTION_STATUSES }
        val active = current.canonicalExecutionSession
        require(
            active == null && openRows.isEmpty() ||
                active != null && openRows.size == 1 &&
                openRows.single().hasSameRemoteSemantics(active),
        ) { "Execution history is incoherent with its active snapshot" }
        val persistedWindow = history.take(MAX_EXECUTION_HISTORY_WINDOW)
        val previousWindow = current.canonicalExecutionHistoryWindow
        val previousById = previousWindow.associateBy { it.id }
        history.forEach { remote ->
            previousById[remote.id]?.let { prior ->
                require(remote.hasSameImmutableIdentity(prior)) {
                    "Execution history identity changed within a continuity chain"
                }
                require(
                    remote.startedAt == prior.startedAt && remote.createdAt == prior.createdAt &&
                        remote.accumulatedSeconds >= prior.accumulatedSeconds,
                ) { "Execution history rewrote immutable time or regressed accumulated work" }
                require(
                    if (prior.status in TERMINAL_EXECUTION_STATUSES) {
                        remote.hasSameRemoteSemantics(prior)
                    } else {
                        remote.revision > prior.revision ||
                            remote.revision == prior.revision && remote.hasSameRemoteSemantics(prior)
                    },
                ) { "Execution history moved backwards or mutated an immutable row" }
            }
        }
        val historyRevisionSum = runCatching {
            history.fold(0L) { total, session -> Math.addExact(total, session.revision) }
        }.getOrNull()
        val globallyComplete = historyRevisionSum == revision
        val previousIds = previousById.keys
        val firstOverlapIndex = persistedWindow.indexOfFirst { it.id in previousIds }
        val hasContiguousOverlap = if (firstOverlapIndex < 0) {
            false
        } else {
            val overlappingIds = persistedWindow.drop(firstOverlapIndex).map { it.id }
            overlappingIds == previousWindow.take(overlappingIds.size).map { it.id }
        }
        val observedRevisionDelta = runCatching {
            persistedWindow.fold(0L) { total, remote ->
                val contribution = previousById[remote.id]?.let { prior ->
                    Math.subtractExact(remote.revision, prior.revision)
                } ?: remote.revision
                require(contribution >= 0)
                Math.addExact(total, contribution)
            }
        }.getOrNull()
        val expectedRevisionDelta = current.canonicalExecutionHistoryWindowRevision?.let {
            runCatching { Math.subtractExact(revision, it) }.getOrNull()
        }
        val rollingContinuity = current.canonicalExecutionHistoryContinuityEstablished &&
            hasContiguousOverlap && expectedRevisionDelta != null && expectedRevisionDelta >= 0 &&
            observedRevisionDelta == expectedRevisionDelta
        val canVerify = globallyComplete || rollingContinuity
        require(continuityVerified == canVerify) {
            "Execution history continuity verdict is inconsistent"
        }
        current.copy(
            canonicalExecutionHistoryWindow = persistedWindow,
            canonicalExecutionHistoryWindowRevision = revision,
            canonicalExecutionHistoryContinuityEstablished = continuityVerified,
            canonicalExecutionHistoryVerified = continuityVerified,
            scheduleMessage = if (continuityVerified) {
                message
            } else {
                "Execution history is incomplete · new starts stay locked"
            },
        )
    }

    /**
     * Reconciles the global lease and, when supplied, one changed history row.
     *
     * Terminal state is applied only to the exact planned block/occurrence/session identity. It
     * never changes the canonical parent item, so finishing one split or recurring session cannot
     * silently finish its siblings.
     */
    @Suppress("LongMethod")
    fun reconcileCanonicalExecution(
        syncOrigin: String,
        configurationId: String?,
        revision: Long,
        activeSession: CanonicalExecutionSessionSnapshot?,
        changedSession: CanonicalExecutionSessionSnapshot? = null,
        clearPendingIdempotencyKey: String? = null,
        message: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(syncOrigin.isNotBlank() && revision >= 0)
        validateExecutionSession(activeSession, mustBeOpen = true)
        validateExecutionSession(changedSession, mustBeOpen = false)
        require(activeSession == null || activeSession.status in OPEN_EXECUTION_STATUSES)
        require(activeSession?.revision?.let { it <= revision } != false)
        require(changedSession?.revision?.let { it <= revision } != false)

        val sameBinding = current.canonicalExecutionSyncOrigin == syncOrigin &&
            current.canonicalExecutionConfigurationId == configurationId
        current.canonicalSyncOrigin?.let { canonicalOrigin ->
            require(
                canonicalOrigin == syncOrigin &&
                    current.canonicalConfigurationId == configurationId,
            ) { "Execution credentials do not match the canonical cache binding" }
        }
        if (sameBinding) {
            require(revision >= current.canonicalExecutionRevision) {
                "Canonical execution revision moved backwards"
            }
        } else if (
            current.canonicalExecutionSyncOrigin != null ||
            current.canonicalExecutionHistoryWindow.isNotEmpty() ||
            current.canonicalExecutionHistoryWindowRevision != null ||
            current.canonicalExecutionHistoryContinuityEstablished ||
            current.canonicalExecutionHistoryVerified ||
            current.terminalExecutionOutcomes.isNotEmpty()
        ) {
            require(
                current.pendingExecutionCommand == null &&
                    current.terminalExecutionOutcomes.isEmpty() &&
                    current.canonicalExecutionHistoryWindow.isEmpty() &&
                    current.canonicalExecutionHistoryWindowRevision == null &&
                    !current.canonicalExecutionHistoryContinuityEstablished &&
                    !current.canonicalExecutionHistoryVerified &&
                    current.canonicalExecutionSession == null,
            ) { "Credential replacement must quarantine durable execution state first" }
        }
        clearPendingIdempotencyKey?.let { key ->
            val pending = current.pendingExecutionCommand
                ?: throw IllegalArgumentException("No execution command is pending")
            require(pending.idempotencyKey == key) {
                "Execution response does not match the durable command fence"
            }
            changedSession?.let { changed ->
                require(pending.hasSameImmutableIdentity(changed)) {
                    "Execution response identity does not match the durable command fence"
                }
            }
        }

        val interruptedLocalSession = if (activeSession != null) {
            current.activeSession?.takeIf { local ->
                current.schedule.firstOrNull { it.id == local.itemId }?.canonicalItemId == null
            }
        } else {
            null
        }
        val baseSchedule = current.schedule.map { block ->
            when {
                block.canonicalItemId != null && block.status in OPEN_DISPLAY_STATUSES ->
                    block.copy(status = ItemStatus.SCHEDULED)
                block.id == interruptedLocalSession?.itemId -> {
                    val recordedMinutes = maxOf(
                        block.actualMinutes ?: 0,
                        interruptedLocalSession.elapsedMinutes,
                    )
                    block.copy(
                        status = ItemStatus.PAUSED,
                        actualMinutes = recordedMinutes.takeIf { it > 0 } ?: block.actualMinutes,
                    )
                }
                else -> block
            }
        }
        fun matchingBlock(
            session: CanonicalExecutionSessionSnapshot,
            schedule: List<ScheduleItem>,
        ): ScheduleItem? = session.plannedBlockId?.let { plannedId ->
            schedule.firstOrNull { it.id == plannedId }
        }?.takeIf { block ->
            block.canonicalItemId == session.itemId &&
                block.canonicalRevision == session.itemRevision &&
                block.occurrenceId == session.occurrenceId &&
                block.sessionIndex == session.sessionIndex
        } ?: schedule.firstOrNull { block ->
            block.canonicalItemId == session.itemId &&
                block.canonicalRevision == session.itemRevision &&
                block.occurrenceId == session.occurrenceId &&
                block.sessionIndex == session.sessionIndex
        }

        fun withProjectionProvenance(
            session: CanonicalExecutionSessionSnapshot,
        ): CanonicalExecutionSessionSnapshot {
            val priorSession = if (sameBinding) {
                sequenceOf(
                    current.canonicalExecutionSession,
                    current.terminalExecutionOutcomes[session.id]?.session,
                ).filterNotNull().firstOrNull { it.hasSameImmutableIdentity(session) }
            } else {
                null
            }
            val pendingProvenance = current.pendingExecutionCommand?.takeIf { pending ->
                pending.hasSameImmutableIdentity(session)
            }?.canonicalProjectionEligibleAtLeaseStart == true
            return session.copy(
                canonicalProjectionEligibleAtLeaseStart = when {
                    priorSession?.canonicalProjectionEligibleAtLeaseStart == true -> true
                    session.canonicalProjectionEligibleAtLeaseStart == true -> true
                    pendingProvenance -> true
                    else -> null
                },
            )
        }

        val authoritativeActiveSession = activeSession?.let(::withProjectionProvenance)
        val authoritativeChangedSession = changedSession?.let(::withProjectionProvenance)

        var schedule = baseSchedule
        var recurrenceOutcomes = current.recurrenceOutcomes
        var recurrenceMoves = current.recurrenceMoves
        var completionAnchors = current.recurrenceCompletionAnchors
        var terminalExecutionOutcomes = if (sameBinding) {
            validatedTerminalExecutionOutcomes(current.terminalExecutionOutcomes)
                .filter { it.syncOrigin == syncOrigin }
                .associateBy { it.session.id }
        } else {
            emptyMap()
        }
        if (authoritativeChangedSession?.status in TERMINAL_EXECUTION_STATUSES) {
            val changed = requireNotNull(authoritativeChangedSession)
            val focused = matchingBlock(changed, schedule)
            val existingOutcome = terminalExecutionOutcomes[changed.id]
            existingOutcome?.let { existing ->
                require(existing.session.hasSameRemoteSemantics(changed)) {
                    "A confirmed terminal execution row was mutated by the server"
                }
            }
            val immutableTerminalSession = existingOutcome?.session ?: changed
            val outcome = TerminalExecutionOutcomeSnapshot(
                syncOrigin = syncOrigin,
                session = immutableTerminalSession,
                requiresCanonicalItemProjection =
                    existingOutcome?.requiresCanonicalItemProjection == true ||
                        immutableTerminalSession.canonicalProjectionEligibleAtLeaseStart == true,
                canonicalProjectionRevision = existingOutcome?.canonicalProjectionRevision,
                canonicalProjectionResolution = existingOutcome?.canonicalProjectionResolution,
                canonicalProjectionConflict = existingOutcome?.canonicalProjectionConflict,
                canonicalProjectionRetryAuthorizedAt =
                    existingOutcome?.canonicalProjectionRetryAuthorizedAt,
                recordedAt = existingOutcome?.recordedAt ?:
                    immutableTerminalSession.endedAt ?: immutableTerminalSession.updatedAt,
            )
            terminalExecutionOutcomes = retainedTerminalExecutionOutcomes(
                terminalExecutionOutcomes + (changed.id to outcome),
            )
            if (focused != null && !outcome.userKeptLatestItem()) {
                val displayStatus = changed.terminalDisplayStatus()
                schedule = schedule.map { block ->
                    if (block.id == focused.id) {
                        block.copy(
                            status = displayStatus,
                            actualMinutes = changed.actualMinutes(),
                        )
                    } else {
                        block
                    }
                }
                focused.occurrenceId?.let { occurrenceId ->
                    val occurrenceBlocks = schedule.filter { it.occurrenceId == occurrenceId }
                    val owner = current.occurrenceSeriesItemIds[occurrenceId]
                    val hasRemainingUnscheduledWork = owner != null &&
                        current.unscheduledWork.any { work ->
                            work.occurrenceId == occurrenceId && work.remainingMinutes > 0
                        }
                    if (
                        owner != null && !hasRemainingUnscheduledWork &&
                        occurrenceBlocks.isNotEmpty() &&
                        occurrenceBlocks.all { it.status in TERMINAL_SESSION_STATUSES } &&
                        occurrenceBlocks.map(ScheduleItem::status).distinct().size == 1
                    ) {
                        recurrenceMoves = recurrenceMoves - occurrenceId
                        recurrenceOutcomes = recurrenceOutcomes + (
                            occurrenceId to RecurrenceOutcomeSnapshot(
                                itemId = owner,
                                status = displayStatus,
                                resolvedAt = changed.endedAt ?: changed.updatedAt,
                            )
                        )
                        if (displayStatus == ItemStatus.COMPLETED) {
                            completionAnchors = completionAnchors + (
                                owner to (changed.endedAt ?: changed.updatedAt)
                            )
                        }
                    }
                }
            }
        }

        if (
            authoritativeActiveSession != null &&
            matchingBlock(authoritativeActiveSession, schedule) == null
        ) {
            schedule = (schedule + remoteLeasePlaceholder(current, authoritativeActiveSession))
                .distinctBy(ScheduleItem::id)
                .sortedWith(SCHEDULE_ORDER)
        }
        val activeBlock = authoritativeActiveSession?.let { matchingBlock(it, schedule) }
        if (authoritativeActiveSession != null && activeBlock != null) {
            val displayStatus = if (authoritativeActiveSession.status == "active") {
                ItemStatus.ACTIVE
            } else {
                ItemStatus.PAUSED
            }
            schedule = schedule.map { block ->
                if (block.id == activeBlock.id) block.copy(status = displayStatus) else block
            }
        }
        val localActiveSession = when {
            authoritativeActiveSession != null && activeBlock != null ->
                authoritativeActiveSession.toActiveSession(activeBlock.id)
            authoritativeActiveSession != null -> null
            else -> current.activeSession?.takeIf { local ->
                current.schedule.firstOrNull { it.id == local.itemId }?.canonicalItemId == null
            }
        }
        current.copy(
            schedule = schedule,
            activeSession = localActiveSession,
            canonicalExecutionSyncOrigin = syncOrigin,
            canonicalExecutionConfigurationId = configurationId,
            canonicalExecutionRevision = revision,
            canonicalExecutionSession = authoritativeActiveSession,
            terminalExecutionOutcomes = terminalExecutionOutcomes,
            pendingExecutionCommand = if (clearPendingIdempotencyKey != null) {
                null
            } else {
                current.pendingExecutionCommand
            },
            recurrenceOutcomes = recurrenceOutcomes,
            recurrenceMoves = recurrenceMoves,
            recurrenceCompletionAnchors = completionAnchors,
            scheduleMessage = when {
                authoritativeActiveSession != null && activeBlock == null ->
                    "Another device owns an execution session that is not in this plan · recompose to locate it"
                interruptedLocalSession != null ->
                    "A remote execution lease appeared · local focus was paused with its elapsed minutes preserved"
                else -> message
            },
        )
    }

    /** Locally forgets all canonical execution state before credential destruction. */
    fun abandonCanonicalConnection(): PlannerPersistenceReceipt? = mutateDurably { current ->
        val canonicalBlockIds = current.schedule.asSequence()
            .filter { it.canonicalItemId != null }
            .map(ScheduleItem::id)
            .toSet()
        current.copy(
            canonicalItems = emptyList(),
            canonicalSyncOrigin = null,
            canonicalConfigurationId = null,
            canonicalDeltaCursor = null,
            schedule = current.schedule.filter { it.canonicalItemId == null },
            activeSession = current.activeSession?.takeUnless { it.itemId in canonicalBlockIds },
            scheduleInputDigest = null,
            scheduleGeneratedAt = null,
            schedulePlanningZoneId = null,
            recurrenceOutcomes = emptyMap(),
            recurrenceMoves = emptyMap(),
            recurrenceCompletionAnchors = emptyMap(),
            pendingCanonicalMutation = null,
            canonicalExecutionSyncOrigin = null,
            canonicalExecutionConfigurationId = null,
            canonicalExecutionRevision = 0,
            canonicalExecutionSession = null,
            canonicalExecutionHistoryWindow = emptyList(),
            canonicalExecutionHistoryWindowRevision = null,
            canonicalExecutionHistoryContinuityEstablished = false,
            canonicalExecutionHistoryVerified = false,
            terminalExecutionOutcomes = emptyMap(),
            pendingExecutionCommand = null,
            unscheduledWork = emptyList(),
            occurrenceSeriesItemIds = emptyMap(),
            rejectedCanonicalItemCount = 0,
            unscheduledCanonicalItemCount = 0,
            scheduleViolationMessages = emptyList(),
            scheduleViolationCount = 0,
            scheduleErrorViolationCount = 0,
            scheduleMessage =
                "API connection forgotten locally · any in-flight server action has unknown outcome",
        )
    }

    /** Durably reconciles one optimistic server mutation without inventing a new schedule. */
    fun reconcileCanonicalItem(
        item: CanonicalItemSnapshot,
        focusedBlockId: String,
        displayStatus: ItemStatus,
        pauseLabel: String? = null,
        pauseMinutes: Int? = null,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val expectedWireStatus = when (displayStatus) {
            ItemStatus.ACTIVE -> "in_progress"
            ItemStatus.PAUSED -> "paused"
            ItemStatus.COMPLETED -> "completed"
            ItemStatus.SKIPPED -> "skipped"
            ItemStatus.SCHEDULED -> "scheduled"
            else -> throw IllegalArgumentException("Unsupported canonical display status")
        }
        require(item.status == expectedWireStatus) {
            "Canonical response status does not match its display state"
        }
        val previous = current.canonicalItems.firstOrNull { it.id == item.id }
            ?: throw IllegalArgumentException("Canonical item is not cached")
        require(item.revision > previous.revision && item.deletedAt == null) {
            "Canonical item mutation must advance an active revision"
        }
        val focused = current.schedule.firstOrNull { it.id == focusedBlockId }
            ?: throw IllegalArgumentException("Focused schedule block is not cached")
        require(focused.canonicalItemId == item.id) {
            "Focused block does not belong to the canonical item"
        }
        if (displayStatus in TERMINAL_SESSION_STATUSES) {
            require(current.schedule.none { block ->
                block.id != focusedBlockId && block.canonicalItemId == item.id &&
                    block.status in TERMINAL_SESSION_STATUSES && block.status != displayStatus
            }) { "Split sessions cannot resolve to mixed terminal outcomes" }
        }
        val focusedSession = current.activeSession
            ?.takeIf { it.itemId == focusedBlockId }
        val pendingMutation = current.pendingCanonicalMutation
        val projectedExecutionMinutes = pendingMutation?.terminalExecutionSessionId?.let { sessionId ->
            val outcome = current.terminalExecutionOutcomes[sessionId]
                ?: throw IllegalArgumentException("Terminal execution projection is unavailable")
            validateTerminalExecutionOutcome(outcome)
            require(
                outcome.session.itemId == item.id && outcome.session.status == item.status,
            ) { "Terminal execution projection does not match the canonical response" }
            requireNotNull(outcome.session.actualMinutes()) {
                "Terminal execution duration is unavailable"
            }
        }
        val elapsed = focusedSession?.let(::completedMinutes) ?: 0
        val updatedSchedule = if (displayStatus == ItemStatus.SCHEDULED) {
            // A deferral invalidates every old placement for this item. Do not display the former
            // time as accepted while the full scheduler is recomposing.
            current.schedule.filterNot { it.canonicalItemId == item.id }
        } else {
            current.schedule.map { block ->
                if (block.canonicalItemId != item.id) return@map block
                block.copy(
                    status = displayStatus,
                    canonicalRevision = item.revision,
                    actualMinutes = if (
                        displayStatus == ItemStatus.COMPLETED && block.id == focusedBlockId
                    ) {
                        projectedExecutionMinutes ?: elapsed.coerceAtLeast(1)
                    } else {
                        block.actualMinutes
                    },
                )
            }
        }
        pendingMutation?.let { pending ->
            require(
                pending.itemId == item.id &&
                    pending.expectedRevision < item.revision &&
                    pending.targetStatus == item.status,
            ) { "Canonical mutation response does not match the durable uncertainty fence" }
        }
        val terminalExecutionOutcomes = pendingMutation?.terminalExecutionSessionId?.let { sessionId ->
            val outcome = current.terminalExecutionOutcomes[sessionId]
                ?: throw IllegalArgumentException("Terminal execution projection is unavailable")
            validateTerminalExecutionOutcome(outcome)
            require(
                outcome.requiresCanonicalItemProjection &&
                    outcome.isProjectionWriteAuthorized() &&
                    outcome.session.itemId == item.id &&
                    outcome.session.itemRevision <= pendingMutation.expectedRevision &&
                    outcome.session.status == item.status
            ) { "Canonical response does not match its terminal execution projection" }
            current.terminalExecutionOutcomes + (
                sessionId to outcome.copy(
                    canonicalProjectionRevision = item.revision,
                    canonicalProjectionResolution = null,
                    canonicalProjectionConflict = null,
                    canonicalProjectionRetryAuthorizedAt = null,
                )
            )
        } ?: current.terminalExecutionOutcomes
        val activeSession = when (displayStatus) {
            ItemStatus.ACTIVE -> focusedSession?.let { session ->
                if (session.isPaused) resumeSession(session) else session
            } ?: newRunningSession(focusedBlockId)
            ItemStatus.PAUSED -> pauseSession(
                focusedSession ?: newRunningSession(focusedBlockId),
                pauseMinutes,
            ).copy(
                pauseLabel = pauseLabel ?: current.activeSession?.pauseLabel ?: "Open-ended break",
            )
            else -> current.activeSession?.takeUnless { session ->
                current.schedule.firstOrNull { it.id == session.itemId }?.canonicalItemId == item.id
            }
        }
        current.copy(
            canonicalItems = current.canonicalItems.map {
                if (it.id == item.id) item else it
            },
            schedule = updatedSchedule,
            activeSession = activeSession,
            scheduleInputDigest = null,
            pendingCanonicalMutation = null,
            terminalExecutionOutcomes = terminalExecutionOutcomes,
            scheduleMessage = when (displayStatus) {
                ItemStatus.ACTIVE -> "Started focus session · canonical state synced"
                ItemStatus.PAUSED -> "Paused · canonical state synced"
                ItemStatus.COMPLETED -> "Completed · canonical state synced"
                ItemStatus.SKIPPED -> "Skipped · canonical state synced"
                ItemStatus.SCHEDULED -> "Will do later · canonical constraint synced"
                else -> "Canonical item state synced"
            },
        )
    }

    fun sendAssistantMessage(text: String): Boolean {
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return false
        val userMessageId = UUID.randomUUID().toString()
        val assistantMessageId = UUID.randomUUID().toString()
        return mutate { current ->
            current.copy(
                messages = current.messages + listOf(
                    ChatMessage(userMessageId, ChatRole.USER, trimmed),
                    ChatMessage(
                        assistantMessageId,
                        ChatRole.ASSISTANT,
                        "I’ll check hard constraints, deadlines, energy, and protected free time. Any schedule change will arrive as a reviewable proposal.",
                    ),
                ),
            )
        }
    }

    /**
     * Persists one occurrence/split-session outcome without replacing the parent canonical item.
     * The current API has no occurrence mutation endpoint; recurrence outcomes are sent back in
     * the next preview context while split-session display state is retained by exact identity.
     */
    fun reconcileLocalCanonicalSession(
        focusedBlockId: String,
        displayStatus: ItemStatus,
        pauseLabel: String? = null,
        pauseMinutes: Int? = null,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(
            displayStatus in setOf(
                ItemStatus.ACTIVE,
                ItemStatus.PAUSED,
                ItemStatus.COMPLETED,
                ItemStatus.SKIPPED,
            ),
        ) { "Unsupported local canonical session status" }
        val focused = current.schedule.firstOrNull { it.id == focusedBlockId }
            ?: throw IllegalArgumentException("Focused schedule block is not cached")
        val itemId = focused.canonicalItemId
            ?: throw IllegalArgumentException("Focused block is not canonical")
        val canonical = current.canonicalItems.firstOrNull { it.id == itemId }
            ?: throw IllegalArgumentException("Canonical item is not cached")
        require(focused.canonicalRevision == canonical.revision) {
            "Focused block has a stale canonical revision"
        }
        val focusedSession = current.activeSession?.takeIf { it.itemId == focusedBlockId }
        if (displayStatus in TERMINAL_SESSION_STATUSES) {
            require(current.unscheduledWork.none {
                it.remainingMinutes > 0 && if (focused.occurrenceId != null) {
                    it.occurrenceId == focused.occurrenceId
                } else {
                    it.itemId == itemId && it.occurrenceId == null
                }
            }) { "Visible sessions do not cover all required work" }
            val executionGroup = if (focused.occurrenceId != null) {
                current.schedule.filter { it.occurrenceId == focused.occurrenceId }
            } else {
                current.schedule.filter {
                    it.canonicalItemId == itemId && it.occurrenceId == null
                }
            }
            require(executionGroup.none { block ->
                block.id != focusedBlockId && block.status in TERMINAL_SESSION_STATUSES &&
                    block.status != displayStatus
            }) { "Split sessions cannot resolve to mixed terminal outcomes" }
        }
        val updatedSchedule = current.schedule.map { block ->
            when {
                block.id == focusedBlockId -> block.copy(
                    status = displayStatus,
                    actualMinutes = if (displayStatus == ItemStatus.COMPLETED) {
                        (focusedSession?.let(::completedMinutes) ?: 1).coerceAtLeast(1)
                    } else {
                        block.actualMinutes
                    },
                )
                displayStatus == ItemStatus.ACTIVE && block.status == ItemStatus.ACTIVE ->
                    block.copy(status = ItemStatus.PAUSED)
                else -> block
            }
        }
        val activeSession = when (displayStatus) {
            ItemStatus.ACTIVE -> focusedSession?.let { session ->
                if (session.isPaused) resumeSession(session) else session
            } ?: newRunningSession(focusedBlockId)
            ItemStatus.PAUSED -> pauseSession(
                focusedSession ?: newRunningSession(focusedBlockId),
                pauseMinutes,
            ).copy(pauseLabel = pauseLabel ?: "Open-ended break")
            else -> current.activeSession?.takeUnless { it.itemId == focusedBlockId }
        }

        var recurrenceOutcomes = current.recurrenceOutcomes
        var recurrenceMoves = current.recurrenceMoves
        var completionAnchors = current.recurrenceCompletionAnchors
        val occurrenceId = focused.occurrenceId
        if (occurrenceId != null && displayStatus in TERMINAL_SESSION_STATUSES) {
            val seriesItemId = current.occurrenceSeriesItemIds[occurrenceId]
                ?: throw IllegalArgumentException("Occurrence owner is unavailable")
            val occurrenceBlocks = updatedSchedule.filter {
                it.occurrenceId == occurrenceId
            }
            if (
                occurrenceBlocks.isNotEmpty() &&
                occurrenceBlocks.all { it.status in TERMINAL_SESSION_STATUSES }
            ) {
                require(occurrenceBlocks.map { it.status }.distinct().size == 1) {
                    "Occurrence sessions cannot resolve to mixed terminal outcomes"
                }
                recurrenceMoves = recurrenceMoves - occurrenceId
                if (occurrenceBlocks.all { it.status == ItemStatus.SKIPPED }) {
                    recurrenceOutcomes = recurrenceOutcomes + (
                        occurrenceId to RecurrenceOutcomeSnapshot(
                            itemId = seriesItemId,
                            status = ItemStatus.SKIPPED,
                            resolvedAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
                        )
                    )
                } else {
                    recurrenceOutcomes = recurrenceOutcomes + (
                        occurrenceId to RecurrenceOutcomeSnapshot(
                            itemId = seriesItemId,
                            status = ItemStatus.COMPLETED,
                            resolvedAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
                        )
                    )
                    completionAnchors = completionAnchors + (
                        seriesItemId to Instant.ofEpochMilli(nowEpochMillis()).toString()
                    )
                }
            }
        }
        val recurrenceChanged =
            recurrenceOutcomes != current.recurrenceOutcomes ||
                completionAnchors != current.recurrenceCompletionAnchors ||
                recurrenceMoves != current.recurrenceMoves
        current.copy(
            schedule = updatedSchedule,
            activeSession = activeSession,
            recurrenceOutcomes = recurrenceOutcomes,
            recurrenceMoves = recurrenceMoves,
            recurrenceCompletionAnchors = completionAnchors,
            scheduleInputDigest = current.scheduleInputDigest.takeUnless { recurrenceChanged },
            scheduleMessage = when (displayStatus) {
                ItemStatus.ACTIVE -> "Started this scheduled session"
                ItemStatus.PAUSED -> "Paused this scheduled session"
                ItemStatus.COMPLETED -> "Session completed · recurrence context saved"
                ItemStatus.SKIPPED -> "Session skipped · recurrence context saved"
                else -> current.scheduleMessage
            },
        )
    }

    fun deferLocalCanonicalSession(
        focusedBlockId: String,
        minutes: Int,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(minutes in 1..24 * 60) { "Session deferral is out of range" }
        val focused = current.schedule.firstOrNull { it.id == focusedBlockId }
            ?: throw IllegalArgumentException("Focused schedule block is not cached")
        val itemId = focused.canonicalItemId
            ?: throw IllegalArgumentException("Focused block is not canonical")
        val canonical = current.canonicalItems.firstOrNull { it.id == itemId }
            ?: throw IllegalArgumentException("Canonical item is not cached")
        require(focused.canonicalRevision == canonical.revision) {
            "Focused block has a stale canonical revision"
        }
        val occurrenceId = focused.occurrenceId
            ?: throw IllegalArgumentException(
                "A non-recurring split session cannot be deferred without remaining-work support",
            )
        val targetIds = current.schedule.asSequence()
            .filter {
                it.occurrenceId == occurrenceId
            }
            .map { it.id }
            .toSet()
        require(current.schedule.none {
            it.id in targetIds && it.id != focusedBlockId &&
                it.status in TERMINAL_SESSION_STATUSES
        }) { "A partially resolved occurrence cannot be deferred as a whole" }
        val movedBlocks = current.schedule.filter { it.id in targetIds }
        val shiftedBounds = movedBlocks.map { block ->
            val start = block.absoluteStartAt?.let(Instant::parse)
                ?: throw IllegalArgumentException("Canonical block has no exact start")
            val end = block.absoluteEndAt?.let(Instant::parse)
                ?: throw IllegalArgumentException("Canonical block has no exact end")
            start.plusSeconds(minutes.toLong() * 60L) to
                end.plusSeconds(minutes.toLong() * 60L)
        }
        val moves = current.recurrenceMoves + (
            occurrenceId to RecurrenceMoveSnapshot(
                itemId = current.occurrenceSeriesItemIds[occurrenceId]
                    ?: throw IllegalArgumentException("Occurrence owner is unavailable"),
                startAt = requireNotNull(shiftedBounds.minOfOrNull { it.first }).toString(),
                endAt = requireNotNull(shiftedBounds.maxOfOrNull { it.second }).toString(),
                movedAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
            )
        )
        current.copy(
            schedule = current.schedule.map { block ->
                if (block.id in targetIds) block.copy(status = ItemStatus.SCHEDULED) else block
            },
            activeSession = current.activeSession?.takeUnless { it.itemId in targetIds },
            recurrenceMoves = moves,
            scheduleInputDigest = null,
            scheduleMessage =
                "Move requested · the previous placement remains visible until server validation",
        )
    }

    fun toggleCompleted() {
        mutate { it.copy(showCompleted = !it.showCompleted) }
    }

    fun toggleQuietSuggestions() {
        mutate { it.copy(quietSuggestions = !it.quietSuggestions) }
    }

    fun toggleDynamicColor() {
        mutate { it.copy(useDynamicColor = !it.useDynamicColor) }
    }

    fun recompose() {
        mutate {
            it.copy(scheduleMessage = "Recomposed · hard commitments and the focus horizon stayed fixed")
        }
    }

    private fun mutate(transform: (DayWeaveUiState) -> DayWeaveUiState): Boolean {
        val mutation = mutateInternal(requireExactSave = false, transform) ?: return false
        if (mutation.shouldSignalWriter) saveRequests.trySend(Unit)
        return true
    }

    private fun newRunningSession(itemId: String): ActiveSession = ActiveSession(
        itemId = itemId,
        elapsedMinutes = 0,
        isPaused = false,
        accumulatedSeconds = 0,
        runningSinceEpochMillis = nowEpochMillis(),
    )

    private fun validateExecutionSession(
        session: CanonicalExecutionSessionSnapshot?,
        mustBeOpen: Boolean,
    ) {
        if (session == null) return
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
        require(session.status in ALL_EXECUTION_STATUSES)
        if (mustBeOpen) require(session.status in OPEN_EXECUTION_STATUSES)
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
                    session.pauseReason == null && endedAt == null && session.actualSeconds == null,
            )
            "paused" -> require(
                runningSince == null && pausedAt != null && pausedAt >= startedAt &&
                    pausedAt <= updatedAt &&
                    (pauseUntil == null || pauseUntil > updatedAt &&
                        pauseUntil <= updatedAt.plusSeconds(MAX_EXECUTION_PAUSE_SECONDS.toLong())) &&
                    endedAt == null && session.actualSeconds == null,
            )
            else -> require(
                runningSince == null && pauseUntil == null && session.pauseReason == null &&
                    session.actualSeconds != null && endedAt == updatedAt &&
                    (pausedAt == null || pausedAt >= startedAt && pausedAt <= updatedAt),
            )
        }
    }

    private fun validateTerminalExecutionOutcome(outcome: TerminalExecutionOutcomeSnapshot) {
        require(outcome.syncOrigin.isNotBlank())
        validateExecutionSession(outcome.session, mustBeOpen = false)
        require(outcome.session.status in TERMINAL_EXECUTION_STATUSES)
        Instant.parse(outcome.recordedAt)
        require(
            listOfNotNull(
                outcome.canonicalProjectionRevision,
                outcome.canonicalProjectionResolution,
                outcome.canonicalProjectionConflict,
            ).size <= 1
        ) { "Terminal execution has conflicting projection resolutions" }
        require(
            outcome.canonicalProjectionRevision == null ||
                outcome.requiresCanonicalItemProjection &&
                outcome.canonicalProjectionRevision >= outcome.session.itemRevision
        ) { "Terminal execution projection revision is invalid" }
        require(
            outcome.canonicalProjectionResolution == null ||
                outcome.requiresCanonicalItemProjection &&
                outcome.canonicalProjectionResolution in TERMINAL_PROJECTION_RESOLUTIONS
        ) { "Terminal execution projection resolution is invalid" }
        require(
            outcome.canonicalProjectionConflict == null ||
                outcome.requiresCanonicalItemProjection &&
                outcome.canonicalProjectionConflict.isNotBlank() &&
                outcome.canonicalProjectionConflict.length <=
                MAX_TERMINAL_PROJECTION_CONFLICT_CHARS
        ) { "Terminal execution projection conflict is invalid" }
        outcome.canonicalProjectionRetryAuthorizedAt?.let { authorizedAt ->
            require(
                outcome.requiresCanonicalItemProjection &&
                    outcome.canonicalProjectionRevision == null &&
                    outcome.canonicalProjectionResolution == null &&
                    outcome.canonicalProjectionConflict != null
            ) { "Terminal execution retry authorization is invalid" }
            Instant.parse(authorizedAt)
        }
        if (!outcome.requiresCanonicalItemProjection) {
            require(
                outcome.canonicalProjectionRevision == null &&
                    outcome.canonicalProjectionResolution == null &&
                    outcome.canonicalProjectionConflict == null &&
                    outcome.canonicalProjectionRetryAuthorizedAt == null
            )
        }
    }

    private fun validatedTerminalExecutionOutcomes(
        outcomes: Map<String, TerminalExecutionOutcomeSnapshot>,
    ): List<TerminalExecutionOutcomeSnapshot> {
        return outcomes.map { (sessionId, outcome) ->
            require(sessionId == outcome.session.id)
            validateTerminalExecutionOutcome(outcome)
            outcome
        }
    }

    private fun CanonicalExecutionSessionSnapshot.matches(block: ScheduleItem): Boolean =
        block.canonicalItemId == itemId &&
            block.canonicalRevision == itemRevision &&
            block.occurrenceId == occurrenceId &&
            block.sessionIndex == sessionIndex

    private fun CanonicalExecutionSessionSnapshot.matchesProjectionLineage(
        block: ScheduleItem,
    ): Boolean =
        block.canonicalItemId == itemId &&
            block.occurrenceId == occurrenceId &&
            block.sessionIndex == sessionIndex

    private fun CanonicalExecutionSessionSnapshot.matchesProjectionTarget(
        block: ScheduleItem,
        expectedRevision: Long,
    ): Boolean =
        block.canonicalItemId == itemId &&
            block.canonicalRevision == expectedRevision &&
            block.occurrenceId == occurrenceId &&
            block.sessionIndex == sessionIndex

    private fun TerminalExecutionOutcomeSnapshot.isProjectionUnresolved(): Boolean =
        requiresCanonicalItemProjection &&
            canonicalProjectionRevision == null &&
            canonicalProjectionResolution == null

    private fun TerminalExecutionOutcomeSnapshot.isProjectionPending(): Boolean =
        isProjectionUnresolved() && canonicalProjectionConflict == null

    private fun TerminalExecutionOutcomeSnapshot.isProjectionWriteAuthorized(): Boolean =
        isProjectionPending() ||
            isProjectionUnresolved() && canonicalProjectionConflict != null &&
            canonicalProjectionRetryAuthorizedAt != null

    private fun TerminalExecutionOutcomeSnapshot.userKeptLatestItem(): Boolean =
        canonicalProjectionResolution == TERMINAL_PROJECTION_USER_KEPT_LATEST

    private fun requireTerminalProjection(
        state: DayWeaveUiState,
        sessionId: String,
    ): TerminalExecutionOutcomeSnapshot {
        require(UUID.fromString(sessionId).toString() == sessionId)
        val outcome = state.terminalExecutionOutcomes[sessionId]
            ?: throw IllegalArgumentException("Terminal execution outcome is unavailable")
        validateTerminalExecutionOutcome(outcome)
        require(outcome.isProjectionUnresolved()) {
            "Terminal execution projection is already resolved"
        }
        return outcome
    }

    private fun TerminalExecutionOutcomeSnapshot.canSafelyOverlayRebased(
        block: ScheduleItem,
        itemsById: Map<String, CanonicalItemSnapshot>,
        schedule: List<ScheduleItem>,
        unscheduledWork: List<UnscheduledWorkSnapshot>,
    ): Boolean {
        if (
            !isProjectionUnresolved() || session.occurrenceId != null ||
            block.canonicalItemId != session.itemId || block.occurrenceId != null ||
            block.sessionIndex != session.sessionIndex || block.isSplittable
        ) {
            return false
        }
        val item = itemsById[session.itemId] ?: return false
        if (
            !item.isExecutable || item.recurrenceJson != null ||
            item.status in TERMINAL_CANONICAL_STATUSES && item.status != session.status
        ) {
            return false
        }
        val matching = schedule.count {
            it.canonicalItemId == item.id && it.occurrenceId == null
        }
        return matching == 1 && unscheduledWork.none {
            it.itemId == item.id && it.occurrenceId == null && it.remainingMinutes > 0
        }
    }

    private fun CanonicalExecutionSessionSnapshot.hasSameImmutableIdentity(
        other: CanonicalExecutionSessionSnapshot,
    ): Boolean =
        id == other.id &&
            itemId == other.itemId &&
            itemRevision == other.itemRevision &&
            occurrenceId == other.occurrenceId &&
            sessionIndex == other.sessionIndex &&
            plannedBlockId == other.plannedBlockId &&
            sourceDeviceId == other.sourceDeviceId

    /** Compares every server-owned field while intentionally ignoring local provenance. */
    private fun CanonicalExecutionSessionSnapshot.hasSameRemoteSemantics(
        other: CanonicalExecutionSessionSnapshot,
    ): Boolean =
        copy(canonicalProjectionEligibleAtLeaseStart = null) ==
            other.copy(canonicalProjectionEligibleAtLeaseStart = null)

    private fun List<CanonicalExecutionSessionSnapshot>.isNewestFirstExecutionHistory(): Boolean =
        zipWithNext().all { (newer, older) ->
            val newerUpdated = Instant.parse(newer.updatedAt)
            val olderUpdated = Instant.parse(older.updatedAt)
            newerUpdated > olderUpdated ||
                newerUpdated == olderUpdated && newer.id > older.id
        }

    private fun CanonicalExecutionSessionSnapshot.terminalDisplayStatus(): ItemStatus =
        when (status) {
            "completed" -> ItemStatus.COMPLETED
            "skipped" -> ItemStatus.SKIPPED
            else -> throw IllegalArgumentException("Execution is not terminal")
        }

    private fun CanonicalExecutionSessionSnapshot.actualMinutes(): Int? =
        actualSeconds?.let { seconds ->
            (seconds / 60L + if (seconds % 60L == 0L) 0L else 1L)
                .coerceAtMost(Int.MAX_VALUE.toLong())
                .toInt()
        }

    private fun remoteLeasePlaceholder(
        state: DayWeaveUiState,
        session: CanonicalExecutionSessionSnapshot,
    ): ScheduleItem {
        val item = state.canonicalItems.firstOrNull { it.id == session.itemId }
        val zone = runCatching {
            ZoneId.of(state.schedulePlanningZoneId ?: ZoneId.systemDefault().id)
        }.getOrElse { ZoneId.systemDefault() }
        val started = Instant.parse(session.startedAt)
        val localStart = started.atZone(zone).toLocalTime()
        val durationMinutes = item?.durationSeconds?.let { seconds ->
            (seconds / 60L + if (seconds % 60L == 0L) 0L else 1L)
                .coerceIn(1L, Int.MAX_VALUE.toLong())
                .toInt()
        } ?: 1
        val kind = when (item?.kind) {
            "event" -> ItemKind.EVENT
            "habit" -> ItemKind.HABIT
            "goal" -> ItemKind.GOAL
            else -> ItemKind.TASK
        }
        return ScheduleItem(
            id = session.id,
            title = item?.title ?: "Remote focus session",
            kind = kind,
            startMinute = localStart.hour * 60 + localStart.minute,
            durationMinutes = durationMinutes,
            status = if (session.status == "paused") ItemStatus.PAUSED else ItemStatus.ACTIVE,
            isFlexible = false,
            isHardConstraint = true,
            note = "Started on another device from an earlier item revision",
            canonicalItemId = session.itemId,
            occurrenceId = session.occurrenceId,
            canonicalRevision = session.itemRevision,
            sessionIndex = session.sessionIndex,
            absoluteStartAt = session.startedAt,
            planningZoneId = zone.id,
            canonicalBlockKind = "remote_execution_lease",
        )
    }

    private fun requiresCanonicalItemProjection(
        state: DayWeaveUiState,
        block: ScheduleItem,
    ): Boolean {
        val itemId = block.canonicalItemId ?: return false
        val item = state.canonicalItems.firstOrNull { it.id == itemId } ?: return false
        if (
            !item.isExecutable || item.recurrenceJson != null || block.occurrenceId != null ||
            block.isSplittable || block.canonicalRevision != item.revision
        ) {
            return false
        }
        val matchingBlocks = state.schedule.count {
            it.canonicalItemId == itemId && it.occurrenceId == null
        }
        return matchingBlocks == 1 && state.unscheduledWork.none {
            it.itemId == itemId && it.occurrenceId == null && it.remainingMinutes > 0
        }
    }

    /**
     * Keeps every immutable terminal fact for the lifetime of this credential binding.
     *
     * Server history is paged and plans can reintroduce an old split/session identity years later;
     * dropping a resolved row would therefore resurrect completed work. The encrypted Room snapshot
     * is the compact durable ledger, not a presentation cache, so it intentionally has no age/count
     * eviction policy.
     */
    private fun retainedTerminalExecutionOutcomes(
        outcomes: Map<String, TerminalExecutionOutcomeSnapshot>,
    ): Map<String, TerminalExecutionOutcomeSnapshot> {
        outcomes.forEach { (sessionId, outcome) ->
            require(sessionId == outcome.session.id)
            validateTerminalExecutionOutcome(outcome)
        }
        return outcomes.toMap()
    }

    private fun PendingExecutionCommand.hasSameImmutableIdentity(
        session: CanonicalExecutionSessionSnapshot,
    ): Boolean =
        sourceDeviceId != null &&
            sessionId == session.id &&
            itemId == session.itemId &&
            itemRevision == session.itemRevision &&
            occurrenceId == session.occurrenceId &&
            sessionIndex == session.sessionIndex &&
            plannedBlockId == session.plannedBlockId &&
            sourceDeviceId == session.sourceDeviceId

    private fun CanonicalExecutionSessionSnapshot.toActiveSession(
        blockId: String,
    ): ActiveSession {
        val now = nowEpochMillis()
        val runningSinceMillis = runningSince?.let { Instant.parse(it).toEpochMilli() }
        val runningSeconds = if (status == "active") {
            runningSinceMillis?.let { started -> ((now - started).coerceAtLeast(0L)) / 1_000L }
                ?: 0L
        } else {
            0L
        }
        val elapsedSeconds = accumulatedSeconds.saturatingAdd(runningSeconds)
        val pauseUntilMillis = pauseUntil?.let { Instant.parse(it).toEpochMilli() }
        val breakEnded = status == "paused" && pauseUntilMillis?.let { it <= now } == true
        return ActiveSession(
            itemId = blockId,
            elapsedMinutes = (elapsedSeconds / 60L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt(),
            isPaused = status == "paused",
            pauseLabel = when {
                breakEnded -> "Break ended · choose what to do next"
                pauseReason != null -> pauseReason
                pauseUntilMillis != null -> "Timed break"
                status == "paused" -> "Open-ended break"
                else -> null
            },
            accumulatedSeconds = accumulatedSeconds,
            runningSinceEpochMillis = runningSinceMillis,
            pauseUntilEpochMillis = pauseUntilMillis,
            timedBreakEnded = breakEnded,
            canonicalExecutionSessionId = id,
        )
    }

    private fun elapsedSeconds(session: ActiveSession, atEpochMillis: Long = nowEpochMillis()): Long {
        val running = if (!session.isPaused) {
            session.runningSinceEpochMillis?.let { started ->
                ((atEpochMillis - started).coerceAtLeast(0L)) / 1_000L
            } ?: 0L
        } else {
            0L
        }
        return session.accumulatedSeconds.coerceAtLeast(0L).saturatingAdd(running)
    }

    private fun elapsedMinutes(session: ActiveSession): Int =
        (elapsedSeconds(session) / 60L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

    private fun completedMinutes(session: ActiveSession): Int =
        ((elapsedSeconds(session) + 59L) / 60L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

    private fun pauseSession(session: ActiveSession, minutes: Int?): ActiveSession {
        val now = nowEpochMillis()
        val accumulated = elapsedSeconds(session, now)
        return session.copy(
            elapsedMinutes = (accumulated / 60L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt(),
            isPaused = true,
            accumulatedSeconds = accumulated,
            runningSinceEpochMillis = null,
            pauseUntilEpochMillis = minutes?.let { duration ->
                now.saturatingAdd(duration.toLong() * 60_000L)
            },
            timedBreakEnded = false,
        )
    }

    private fun resumeSession(session: ActiveSession): ActiveSession = session.copy(
        isPaused = false,
        pauseLabel = null,
        runningSinceEpochMillis = nowEpochMillis(),
        pauseUntilEpochMillis = null,
        timedBreakEnded = false,
    )

    private fun Long.saturatingAdd(other: Long): Long =
        if (other > 0 && this > Long.MAX_VALUE - other) Long.MAX_VALUE else this + other

    private fun mutateDurably(
        transform: (DayWeaveUiState) -> DayWeaveUiState,
    ): PlannerPersistenceReceipt? {
        val mutation = mutateInternal(requireExactSave = true, transform) ?: return null
        if (mutation.shouldSignalWriter) saveRequests.trySend(Unit)
        return requireNotNull(mutation.receipt)
    }

    private fun mutateInternal(
        requireExactSave: Boolean,
        transform: (DayWeaveUiState) -> DayWeaveUiState,
    ): MutationResult? = synchronized(persistenceLock) {
        if (
            persistenceStatus == PersistenceStatus.LOADING ||
            persistenceStatus == PersistenceStatus.FAILED
        ) {
            return@synchronized null
        }
        val snapshot = transform(mutableState.value)
        mutableState.value = snapshot
        currentGeneration += 1

        if (persistenceStatus != PersistenceStatus.READY) {
            val receipt = if (requireExactSave) {
                PlannerPersistenceReceipt(
                    generation = currentGeneration,
                    completion = CompletableDeferred(true),
                )
            } else {
                null
            }
            return@synchronized MutationResult(receipt, shouldSignalWriter = false)
        }

        val completion = if (requireExactSave) CompletableDeferred<Boolean>() else null
        val request = SaveRequest(
            generation = currentGeneration,
            snapshot = snapshot,
            completion = completion,
        )
        if (requireExactSave) {
            exactSaveRequests.addLast(request)
        } else {
            // Routine UI changes can coalesce, while exact server generations remain ordered.
            latestNormalSaveRequest = request
        }
        MutationResult(
            receipt = completion?.let {
                PlannerPersistenceReceipt(currentGeneration, it)
            },
            shouldSignalWriter = true,
        )
    }

    private suspend fun restore(repository: PlannerStateRepository) {
        val restored = runCatching { repository.load() }
        if (restored.isFailure) {
            markPersistenceFailed(restored.exceptionOrNull() ?: return)
            persistenceReady.complete(false)
            return
        }

        val persistedState = restored.getOrNull()
        val shouldSaveInitialState = synchronized(persistenceLock) {
            val snapshot = persistedState ?: initialState
            mutableState.value = snapshot
            currentGeneration += 1
            persistenceStatus = PersistenceStatus.READY
            if (persistedState == null) {
                latestNormalSaveRequest = SaveRequest(currentGeneration, snapshot)
                true
            } else {
                persistedGeneration = currentGeneration
                false
            }
        }
        persistenceReady.complete(true)
        mutableLoadState.value = PlannerLoadState.READY
        if (shouldSaveInitialState) saveRequests.trySend(Unit)
    }

    private suspend fun autosave(repository: PlannerStateRepository) {
        if (!persistenceReady.await()) return
        for (ignored in saveRequests) {
            while (true) {
                val request = synchronized(persistenceLock) {
                    exactSaveRequests.pollFirst()
                        ?: latestNormalSaveRequest?.also { latestNormalSaveRequest = null }
                } ?: break
                try {
                    repository.save(request.snapshot)
                } catch (error: CancellationException) {
                    markPersistenceFailed(error, request)
                    throw error
                } catch (error: Throwable) {
                    markPersistenceFailed(error, request)
                    return
                }
                synchronized(persistenceLock) {
                    persistedGeneration = maxOf(persistedGeneration, request.generation)
                    request.completion?.complete(true)
                    if (
                        latestNormalSaveRequest?.generation?.let { it <= persistedGeneration } == true
                    ) {
                        latestNormalSaveRequest = null
                    }
                }
            }
        }
    }

    private fun markPersistenceFailed(
        error: Throwable,
        failedRequest: SaveRequest? = null,
    ) {
        synchronized(persistenceLock) {
            persistenceStatus = PersistenceStatus.FAILED
            failedRequest?.completion?.complete(false)
            while (exactSaveRequests.isNotEmpty()) {
                exactSaveRequests.removeFirst().completion?.complete(false)
            }
            latestNormalSaveRequest = null
        }
        onPersistenceError(error)
        mutableLoadState.value = PlannerLoadState.PERSISTENCE_FAILED
    }

    private data class MutationResult(
        val receipt: PlannerPersistenceReceipt?,
        val shouldSignalWriter: Boolean,
    )

    private data class SaveRequest(
        val generation: Long,
        val snapshot: DayWeaveUiState,
        val completion: CompletableDeferred<Boolean>? = null,
    )

    private data class ScheduleIdentity(
        val itemId: String?,
        val occurrenceId: String?,
        val sessionIndex: Int,
    ) {
        companion object {
            fun from(block: ScheduleItem): ScheduleIdentity = ScheduleIdentity(
                itemId = block.canonicalItemId,
                occurrenceId = block.occurrenceId,
                sessionIndex = block.sessionIndex,
            )
        }
    }

    private enum class PersistenceStatus {
        DISABLED,
        LOADING,
        READY,
        FAILED,
    }

    private companion object {
        val TERMINAL_SESSION_STATUSES = setOf(ItemStatus.COMPLETED, ItemStatus.SKIPPED)
        val OPEN_DISPLAY_STATUSES = setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED)
        val OPEN_EXECUTION_STATUSES = setOf("active", "paused")
        val TERMINAL_EXECUTION_STATUSES = setOf("completed", "skipped")
        val TERMINAL_CANONICAL_STATUSES = setOf("completed", "skipped")
        val ALL_EXECUTION_STATUSES = OPEN_EXECUTION_STATUSES + TERMINAL_EXECUTION_STATUSES
        val EXECUTION_COMMAND_TYPES = setOf("start", "pause", "resume", "complete", "skip")
        const val MAX_PENDING_EXECUTION_REQUEST_CHARS = 64 * 1024
        const val MAX_EXECUTION_HISTORY_WINDOW = 100
        const val MAX_EXECUTION_PAUSE_SECONDS = 24 * 60 * 60
        const val MAX_TERMINAL_PROJECTION_CONFLICT_CHARS = 500
        val NIL_UUID: UUID = UUID(0L, 0L)
        const val TERMINAL_PROJECTION_ITEM_DELETED = "item_deleted"
        const val TERMINAL_PROJECTION_USER_KEPT_LATEST = "user_kept_latest_item"
        val TERMINAL_PROJECTION_RESOLUTIONS = setOf(
            TERMINAL_PROJECTION_ITEM_DELETED,
            TERMINAL_PROJECTION_USER_KEPT_LATEST,
        )
        val SCHEDULE_ORDER = Comparator<com.greengolddog.dayweave.model.ScheduleItem> { left, right ->
            val leftInstant = left.timelineInstant()
            val rightInstant = right.timelineInstant()
            if (leftInstant != null && rightInstant != null) {
                leftInstant.compareTo(rightInstant)
            } else {
                left.startMinute.compareTo(right.startMinute)
            }
        }

        fun missingAcceptedDrafts(
            existing: List<InboxItem>,
            suggestions: List<PlanningSuggestion>,
        ): List<InboxItem> {
            val existingIds = existing.asSequence().map(InboxItem::id).toHashSet()
            return suggestions.asSequence()
                .filter { it.disposition == SuggestionDisposition.APPROVED_FOR_INBOX }
                .filter { "proposal-${it.id}" !in existingIds }
                .map { it.toInboxDraft() }
                .toList()
        }

        fun PlanningSuggestion.toInboxDraft(): InboxItem {
            val detail = buildString {
                append(summary)
                remotePayloadJson
                    ?.takeIf { it.isNotBlank() && it != "{}" }
                    ?.let {
                        append("\n\nProposed details: ")
                        append(it)
                    }
            }
            return InboxItem(
                id = "proposal-$id",
                title = title,
                source = InboxSource.EXTERNAL_PROPOSAL,
                detail = detail,
                requiresReview = true,
            )
        }
    }
}
