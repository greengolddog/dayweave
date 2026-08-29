package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.RecurrenceOutcomeSnapshot
import com.greengolddog.dayweave.model.RecurrenceMoveSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.SuggestionDisposition
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
        if (active.isPaused) return false
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
            val sameOrigin = current.canonicalSyncOrigin == update.syncOrigin
            val freshSchedule = update.schedule.sortedWith(SCHEDULE_ORDER)
            val freshIdentities = freshSchedule.mapTo(hashSetOf(), ScheduleIdentity::from)
            val retainedHistory = if (sameOrigin) {
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
            val orderedSchedule = (freshSchedule + retainedHistory)
                .distinctBy(ScheduleItem::id)
                .sortedWith(SCHEDULE_ORDER)
            val previousActiveBlock = current.activeSession
                ?.takeIf { sameOrigin }
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
                canonicalDeltaCursor = update.deltaCursor,
                schedule = orderedSchedule,
                activeSession = restoredSession,
                scheduleInputDigest = update.inputDigest,
                scheduleGeneratedAt = update.generatedAt,
                schedulePlanningZoneId = update.planningZoneId,
                recurrenceOutcomes = if (sameOrigin) {
                    current.recurrenceOutcomes.filterValues { it.itemId in itemsById }
                } else {
                    emptyMap()
                },
                recurrenceCompletionAnchors = if (sameOrigin) {
                    current.recurrenceCompletionAnchors.filterKeys { it in itemsById }
                } else {
                    emptyMap()
                },
                recurrenceMoves = if (sameOrigin) {
                    current.recurrenceMoves.filterValues { it.itemId in itemsById }
                } else {
                    emptyMap()
                },
                // A plan read alone cannot prove that a timed-out write will not commit later.
                // The sync manager clears this only by replaying the exact durable request.
                pendingCanonicalMutation = current.pendingCanonicalMutation,
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
        require(UUID.fromString(mutation.idempotencyKey).toString() == mutation.idempotencyKey)
        require(UUID.fromString(mutation.itemId).toString() == mutation.itemId)
        require(mutation.syncOrigin == current.canonicalSyncOrigin)
        require(mutation.expectedRevision > 0 && mutation.targetStatus.isNotBlank())
        Instant.parse(mutation.startedAt)
        require(mutation.replacementRequestJson.isNotBlank())
        require(UUID.fromString(mutation.focusedBlockId).toString() == mutation.focusedBlockId)
        require(mutation.pauseMinutes == null || mutation.pauseMinutes in 1..24 * 60)
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

    /** Locally forgets all canonical execution state before credential destruction. */
    fun abandonCanonicalConnection(): PlannerPersistenceReceipt? = mutateDurably { current ->
        val canonicalBlockIds = current.schedule.asSequence()
            .filter { it.canonicalItemId != null }
            .map(ScheduleItem::id)
            .toSet()
        current.copy(
            canonicalItems = emptyList(),
            canonicalSyncOrigin = null,
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
                        elapsed.coerceAtLeast(1)
                    } else {
                        block.actualMinutes
                    },
                )
            }
        }
        current.pendingCanonicalMutation?.let { pending ->
            require(
                pending.itemId == item.id &&
                    pending.expectedRevision < item.revision &&
                    pending.targetStatus == item.status,
            ) { "Canonical mutation response does not match the durable uncertainty fence" }
        }
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
        )
    }

    private fun resumeSession(session: ActiveSession): ActiveSession = session.copy(
        isPaused = false,
        pauseLabel = null,
        runningSinceEpochMillis = nowEpochMillis(),
        pauseUntilEpochMillis = null,
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
