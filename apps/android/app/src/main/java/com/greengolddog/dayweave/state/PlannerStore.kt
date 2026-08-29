package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionDisposition
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
                activeSession = ActiveSession(id, elapsedMinutes = 0, isPaused = false),
                scheduleMessage = "Started focus session",
            )
        }
    }

    fun pauseActive(minutes: Int? = null) {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            val pauseLabel = minutes?.let { "$it minute break" } ?: "Open-ended break"
            current.copy(
                schedule = current.schedule.map {
                    if (it.id == active.itemId) it.copy(status = ItemStatus.PAUSED) else it
                },
                activeSession = active.copy(isPaused = true, pauseLabel = pauseLabel),
                scheduleMessage = "Paused · remaining work is held tentatively",
            )
        }
    }

    fun resumeActive() {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            current.copy(
                schedule = current.schedule.map {
                    if (it.id == active.itemId) it.copy(status = ItemStatus.ACTIVE) else it
                },
                activeSession = active.copy(isPaused = false, pauseLabel = null),
                scheduleMessage = "Focus session resumed",
            )
        }
    }

    fun completeActive() {
        mutate { current ->
            val active = current.activeSession ?: return@mutate current
            current.copy(
                schedule = current.schedule.map { item ->
                    if (item.id == active.itemId) {
                        item.copy(
                            status = ItemStatus.COMPLETED,
                            actualMinutes = active.elapsedMinutes.coerceAtLeast(1),
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

    private enum class PersistenceStatus {
        DISABLED,
        LOADING,
        READY,
        FAILED,
    }

    private companion object {
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
