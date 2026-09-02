package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.DerivedEnergySnapshot
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.EnergySignalSource
import com.greengolddog.dayweave.model.ExecutionDeferAssessmentSnapshot
import com.greengolddog.dayweave.model.GoogleCalendarOutboundJournal
import com.greengolddog.dayweave.model.GoogleCalendarOutboundStage
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.LocalScheduleCompositionProvenanceSnapshot
import com.greengolddog.dayweave.model.ManualEnergyCheckIn
import com.greengolddog.dayweave.model.MoveLaterApprovalEnvelope
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.PendingExecutionCommand
import com.greengolddog.dayweave.model.PendingExecutionDeferIntent
import com.greengolddog.dayweave.model.PendingProposalApplicationMutation
import com.greengolddog.dayweave.model.PendingSchedulePublication
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.model.ProposalApplicationReceiptSnapshot
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.RecurrenceOutcomeSnapshot
import com.greengolddog.dayweave.model.RecurrenceMoveSnapshot
import com.greengolddog.dayweave.model.RecurrenceOccurrenceSourceSnapshot
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.TerminalExecutionOutcomeSnapshot
import com.greengolddog.dayweave.model.UnscheduledWorkSnapshot
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.model.requireCanonicalAuthoringJournalBudget
import com.greengolddog.dayweave.model.requireCanonicalAuthoringShape
import com.greengolddog.dayweave.model.nextCanonicalTrashRetentionExpiryEpochMillis
import com.greengolddog.dayweave.model.withCanonicalTrashRetention
import com.greengolddog.dayweave.model.withPendingSensitivityHardened
import com.greengolddog.dayweave.model.withInvalidTimedBreakNotificationAttemptAbandoned
import com.greengolddog.dayweave.model.isApplicationReady
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.model.hasValidRecurrenceSourceFor
import com.greengolddog.dayweave.model.hasOpenOrPendingExecutionForOccurrence
import com.greengolddog.dayweave.model.googleCalendarOutboundCandidate
import com.greengolddog.dayweave.model.recurrenceIdentityType
import com.greengolddog.dayweave.model.requireCanonicalUuid
import com.greengolddog.dayweave.model.usesReservedChangeSetNamespace
import com.greengolddog.dayweave.model.assessMoveLater
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.isTimedBreakNotificationDigest
import com.greengolddog.dayweave.model.isCoveredBy
import com.greengolddog.dayweave.model.isRepresentableMoveLaterSource
import com.greengolddog.dayweave.model.localScheduleCompositionStateFingerprint
import com.greengolddog.dayweave.model.strictLocalDayEndInstant
import com.greengolddog.dayweave.model.strictLocalDayStartInstant
import com.greengolddog.dayweave.network.requireScheduleInputDigest
import com.greengolddog.dayweave.network.validateProposalApplyHttpRequest
import com.greengolddog.dayweave.network.validateProposalUndoHttpRequest
import com.greengolddog.dayweave.network.validateSchedulePublishHttpRequest
import com.greengolddog.dayweave.assistant.isValidAssistantConversationText
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.util.ArrayDeque
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long

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

class LocalScheduleCompositionTransition internal constructor(
    val provenance: LocalScheduleCompositionProvenanceSnapshot,
    val persistence: PlannerPersistenceReceipt,
)

/** The exact authoring journal generation a caller must durably acknowledge before network I/O. */
class CanonicalAuthoringTransition internal constructor(
    val mutation: PendingCanonicalAuthoringMutation,
    val persistence: PlannerPersistenceReceipt,
)

fun interface CanonicalTrashCleanupCancellation {
    fun cancel()
}

/** Injectable so retention cutoffs can be tested without sleeping or touching real credentials. */
fun interface CanonicalTrashCleanupScheduler {
    fun schedule(delayMillis: Long, action: () -> Unit): CanonicalTrashCleanupCancellation
}

private class CoroutineCanonicalTrashCleanupScheduler(
    private val scope: CoroutineScope,
) : CanonicalTrashCleanupScheduler {
    override fun schedule(
        delayMillis: Long,
        action: () -> Unit,
    ): CanonicalTrashCleanupCancellation {
        val job = scope.launch {
            delay(delayMillis)
            action()
        }
        return CanonicalTrashCleanupCancellation(job::cancel)
    }
}

private data class CanonicalAuthoringRefreshOverlay(
    val items: List<CanonicalItemSnapshot>,
    val mutations: List<PendingCanonicalAuthoringMutation>,
    val deleted: List<CanonicalRecentlyDeletedRecord>,
)

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
    cleanupScheduler: CanonicalTrashCleanupScheduler? = null,
) {
    private val canonicalTrashCleanupScheduler = cleanupScheduler
        ?: scope?.let(::CoroutineCanonicalTrashCleanupScheduler)
    private val mutableState = MutableStateFlow(
        initialState
            .withCanonicalTrashRetention(nowEpochMillis())
            .withPendingSensitivityHardened()
            .withInvalidRecurrenceMoveSourcesAbandoned()
            .withInvalidExecutionDeferIntentAbandoned()
            .withInvalidTimedBreakNotificationAttemptAbandoned()
            .withBoundedAssistantMessages()
            .withInvalidLocalScheduleCompositionAbandoned()
            .also { requireCanonicalAuthoringJournalBudget(it.pendingCanonicalAuthoringMutations) },
    )
    val state: StateFlow<DayWeaveUiState> = mutableState.asStateFlow()
    private val mutableLoadState = MutableStateFlow(
        if (repository == null) PlannerLoadState.READY else PlannerLoadState.LOADING,
    )
    val loadState: StateFlow<PlannerLoadState> = mutableLoadState.asStateFlow()
    /** Last generation confirmed written to SQLCipher; null until encrypted restore/save succeeds. */
    private val mutableDurableState = MutableStateFlow<DayWeaveUiState?>(
        if (repository == null) mutableState.value else null,
    )
    val durableState: StateFlow<DayWeaveUiState?> = mutableDurableState.asStateFlow()

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
    private var canonicalTrashCleanupCancellation: CanonicalTrashCleanupCancellation? = null
    private var canonicalTrashCleanupToken = 0L

    init {
        if (repository != null) {
            requireNotNull(scope) { "A CoroutineScope is required when persistence is enabled" }
            scope.launch { restore(repository) }
            scope.launch { autosave(repository) }
        } else {
            synchronized(persistenceLock) {
                scheduleCanonicalTrashCleanupLocked(mutableState.value)
            }
        }
    }

    fun navigate(destination: AppDestination) {
        mutate { it.copy(destination = destination) }
    }

    fun startItem(id: String) {
        mutate { current ->
            val target = current.schedule.firstOrNull { it.id == id } ?: return@mutate current
            // Canonical and helper-composed canonical-looking blocks must enter through the
            // server-authoritative execution path, never this device-local timer.
            if (target.canonicalItemId != null) return@mutate current

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
                publishedScheduleProof = null,
                scheduleMessage = "Moved one hour later · no hard constraints were crossed",
            )
        }
    }

    /** Advances the visible timer without writing more than once per displayed minute. */
    fun tickActiveSession(): Boolean {
        val observed = mutableState.value
        val active = observed.activeSession ?: return false
        if (active.isPaused) {
            val deadline = active.pauseUntilEpochMillis ?: return false
            val identity = observed.authoritativeTimedBreakNotificationIdentity()
            if (
                nowEpochMillis() < deadline || active.timedBreakEnded ||
                identity != null && identity.digest == observed.acknowledgedBreakEndDigest
            ) {
                return false
            }
            return mutate { current ->
                val latest = current.activeSession
                val latestIdentity = current.authoritativeTimedBreakNotificationIdentity()
                if (
                    latest == null || !latest.isPaused ||
                    latest.pauseUntilEpochMillis != deadline || latest.timedBreakEnded ||
                    latestIdentity != null &&
                    latestIdentity.digest == current.acknowledgedBreakEndDigest
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

    /**
     * Durably claims the exact delivery before any OS alert is attempted. This intentionally
     * favors at-most-once alerts: process death after this receipt may lose a banner, but can never
     * replay one whose external side effect was ambiguous. The in-app resolver is exposed in the
     * same encrypted generation and remains the reliable fallback.
     */
    fun claimTimedBreakEndNotificationDelivery(
        expectedDigest: String,
    ): PlannerPersistenceReceipt? {
        require(isTimedBreakNotificationDigest(expectedDigest))
        var matched = false
        val receipt = mutateDurably { current ->
            val identity = current.authoritativeTimedBreakNotificationIdentity()
            val active = current.activeSession
            if (
                identity?.digest != expectedDigest ||
                active == null ||
                nowEpochMillis() < identity.deadlineEpochMillis ||
                current.lastBreakEndNotificationAttemptDigest == expectedDigest ||
                current.acknowledgedBreakEndDigest == expectedDigest
            ) {
                current
            } else {
                matched = true
                current.copy(
                    activeSession = active.copy(
                        timedBreakEnded = true,
                        pauseLabel = "Break ended · choose what to do next",
                    ),
                    lastBreakEndNotificationAttemptDigest = expectedDigest,
                )
            }
        }
        return receipt.takeIf { matched }
    }

    /** Durably consumes one exact opaque tap without changing the authoritative execution lease. */
    fun recordTimedBreakNotificationRouteConsumption(
        expectedDigest: String,
    ): PlannerPersistenceReceipt? {
        require(isTimedBreakNotificationDigest(expectedDigest))
        var matched = false
        val receipt = mutateDurably { current ->
            val identity = current.authoritativeTimedBreakNotificationIdentity()
            val active = current.activeSession
            if (
                identity?.digest != expectedDigest || active?.timedBreakEnded != true ||
                nowEpochMillis() < identity.deadlineEpochMillis ||
                current.acknowledgedBreakEndDigest == expectedDigest ||
                current.lastConsumedBreakEndNotificationDigest == expectedDigest
            ) {
                current
            } else {
                matched = true
                current.copy(lastConsumedBreakEndNotificationDigest = expectedDigest)
            }
        }
        return receipt.takeIf { matched }
    }

    /**
     * Durably marks one stale opaque tap as processed without changing execution. The shared tap
     * receipt is separate from exact consumption, so a stale intent can never erase proof that
     * the current break was already handled.
     */
    fun recordTimedBreakNotificationRouteRejection(
        expectedDigest: String,
    ): PlannerPersistenceReceipt? {
        require(isTimedBreakNotificationDigest(expectedDigest))
        var matched = false
        val receipt = mutateDurably { current ->
            val identity = current.authoritativeTimedBreakNotificationIdentity()
            val active = current.activeSession
            val isExactEndedRoute = identity?.digest == expectedDigest &&
                active?.timedBreakEnded == true &&
                nowEpochMillis() >= identity.deadlineEpochMillis &&
                current.acknowledgedBreakEndDigest != expectedDigest
            if (
                isExactEndedRoute ||
                current.lastRejectedBreakEndNotificationDigest == expectedDigest
            ) {
                current
            } else {
                matched = true
                current.copy(lastRejectedBreakEndNotificationDigest = expectedDigest)
            }
        }
        return receipt.takeIf { matched }
    }

    /**
     * Acknowledges only the exact ended break. The server lease remains paused, while durable
     * notification/tap receipts prevent delayed work or process recreation from reopening it.
     */
    fun acknowledgeTimedBreakEnded(
        expectedDigest: String,
    ): PlannerPersistenceReceipt? {
        require(isTimedBreakNotificationDigest(expectedDigest))
        var matched = false
        val receipt = mutateDurably { current ->
            val identity = current.authoritativeTimedBreakNotificationIdentity()
            val active = current.activeSession
            if (
                identity?.digest != expectedDigest || active?.timedBreakEnded != true ||
                nowEpochMillis() < identity.deadlineEpochMillis ||
                current.acknowledgedBreakEndDigest == expectedDigest
            ) {
                current
            } else {
                matched = true
                current.copy(
                    activeSession = active.copy(
                        timedBreakEnded = false,
                        pauseLabel = "Paused · break ended",
                    ),
                    lastBreakEndNotificationAttemptDigest = expectedDigest,
                    lastConsumedBreakEndNotificationDigest = expectedDigest,
                    acknowledgedBreakEndDigest = expectedDigest,
                )
            }
        }
        return receipt.takeIf { matched }
    }

    fun quickCapture(title: String, kind: ItemKind, isSensitive: Boolean = false): Boolean {
        val trimmed = title.trim()
        if (trimmed.isEmpty()) return false
        val captureId = UUID.randomUUID().toString()

        return mutate { current ->
            current.copy(
                inbox = listOf(
                    InboxItem(
                        id = captureId,
                        isSensitive = isSensitive,
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
            if (
                suggestion.disposition != SuggestionDisposition.PENDING ||
                suggestion.usesReservedChangeSetNamespace
            ) return@mutate current

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
                SuggestionDisposition.TRANSACTIONALLY_APPLIED ->
                    "Proposal was applied transactionally · refreshing canonical state"
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
        validateCanonicalPlanUpdate(update)
        return mutateDurably { current ->
            require(current.pendingSchedulePublication == null) {
                "A schedule publication must be reconciled before direct plan replacement"
            }
            require(current.pendingProposalApplicationMutation == null) {
                "A proposal application must be reconciled before direct plan replacement"
            }
            canonicalPlanState(current, update).copy(
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
            )
        }
    }

    /**
     * Atomically installs the exact immutable head returned by `/v1/schedule/current`.
     *
     * The caller must map both the canonical-item generation and schedule before entering this
     * boundary. Equality with the last SQLCipher-confirmed state prevents a response for one item
     * generation or credential binding from being installed into another.
     */
    fun installCurrentPublishedSchedule(
        expectedState: DayWeaveUiState,
        update: CanonicalPlanUpdate,
        revision: PublishedScheduleRevisionSnapshot,
    ): PlannerPersistenceReceipt? {
        validateCanonicalPlanUpdate(update)
        val proof = currentPublishedScheduleProof(update, revision)
        return mutateDurablyWithSnapshot { current ->
            require(current == expectedState && mutableDurableState.value == expectedState) {
                "Planner state changed while the published schedule was in flight"
            }
            requireCurrentScheduleReplicaPreflight(current, update)
            val accepted = canonicalPlanState(current, update)
            require(accepted.scheduleInputDigest == revision.inputDigest)
            accepted.copy(
                pendingSchedulePublication = null,
                publishedScheduleRevision = revision,
                publishedScheduleProof = proof,
                scheduleMessage = update.message,
            ).also { installed ->
                require(proof.matchesStateBinding(installed))
                require(proof.matchesPublishedPlan(installed.schedule))
            }
        }?.receipt
    }

    /** Clears stale publication authority only for an exact durable binding and state generation. */
    fun installNoCurrentPublishedSchedule(
        expectedState: DayWeaveUiState,
        syncOrigin: String,
        configurationId: String,
    ): PlannerPersistenceReceipt? = mutateDurablyWithSnapshot { current ->
        require(current == expectedState && mutableDurableState.value == expectedState) {
            "Planner state changed while the empty schedule head was in flight"
        }
        require(current.canonicalSyncOrigin == null || current.canonicalSyncOrigin == syncOrigin)
        require(
            current.canonicalConfigurationId == null ||
                current.canonicalConfigurationId == configurationId,
        )
        requireNoReplicaBlockingMutation(current)
        val retainedLeaseIds = current.canonicalExecutionSession
            ?.takeIf { it.status in OPEN_EXECUTION_STATUSES }
            ?.let { lease ->
                current.schedule.filter { block -> lease.matches(block) }
                    .mapTo(hashSetOf(), ScheduleItem::id)
            }
            .orEmpty()
        current.copy(
            schedule = current.schedule.filter { block ->
                block.canonicalBlockKind == "remote_execution_lease" ||
                    block.id in retainedLeaseIds
            },
            activeSession = current.activeSession?.takeIf { session ->
                session.itemId in retainedLeaseIds
            },
            pendingSchedulePublication = null,
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
            scheduleInputDigest = null,
            localScheduleCompositionProvenance = null,
            scheduleGeneratedAt = null,
            schedulePlanningZoneId = null,
            rejectedCanonicalItemCount = 0,
            unscheduledCanonicalItemCount = 0,
            scheduleViolationMessages = emptyList(),
            scheduleViolationCount = 0,
            scheduleErrorViolationCount = 0,
            unscheduledWork = emptyList(),
            occurrenceSeriesItemIds = emptyMap(),
            recurrenceOccurrenceSources = emptyMap(),
            scheduleMessage = "No schedule has been published for this workspace yet",
        )
    }?.receipt

    private fun requireCurrentScheduleReplicaPreflight(
        current: DayWeaveUiState,
        update: CanonicalPlanUpdate,
    ) {
        requireNoReplicaBlockingMutation(current)
        require(update.configurationId != null)
        require(
            current.canonicalSyncOrigin == null ||
                current.canonicalSyncOrigin == update.syncOrigin,
        )
        require(
            current.canonicalConfigurationId == null ||
                current.canonicalConfigurationId == update.configurationId,
        )
        require(
            current.canonicalExecutionSyncOrigin == null ||
                current.canonicalExecutionSyncOrigin == update.syncOrigin,
        )
        require(
            current.canonicalExecutionConfigurationId == null ||
                current.canonicalExecutionConfigurationId == update.configurationId,
        )
    }

    private fun requireNoReplicaBlockingMutation(current: DayWeaveUiState) {
        require(current.pendingSchedulePublication == null)
        require(current.pendingProposalApplicationMutation == null)
        require(current.pendingCanonicalMutation == null)
        require(current.pendingCanonicalAuthoringMutations.none {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        })
        require(current.pendingExecutionCommand == null)
        require(current.pendingExecutionDeferIntent == null)
    }

    private fun currentPublishedScheduleProof(
        update: CanonicalPlanUpdate,
        revision: PublishedScheduleRevisionSnapshot,
    ): PublishedScheduleProofSnapshot {
        val revisionId = UUID.fromString(revision.id)
        require(revisionId != NIL_UUID && revisionId.toString() == revision.id)
        require(revision.revisionNumber > 0uL)
        require(revision.revision == "${revision.revisionNumber}:${revision.id}")
        requireScheduleInputDigest(revision.inputDigest)
        require(revision.inputDigest == update.inputDigest)
        val horizonStart = Instant.parse(revision.horizonStart)
        val horizonEnd = Instant.parse(revision.horizonEnd)
        val asOf = Instant.parse(update.generatedAt)
        require(horizonStart <= asOf && asOf < horizonEnd)
        require(revision.timezoneName == update.planningZoneId)
        requireNotNull(runCatching { ZoneId.of(revision.timezoneName) }.getOrNull())
        requireNotNull(runCatching { Instant.parse(revision.publishedAt) }.getOrNull())
        val blocks = update.schedule.filter {
            it.canonicalBlockKind != null && it.canonicalBlockKind != "remote_execution_lease"
        }.map(PublishedScheduleBlockProofSnapshot::from).sortedBy { it.id }
        return PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = update.syncOrigin,
            configurationId = requireNotNull(update.configurationId),
            revision = revision,
            asOf = update.generatedAt,
            blocks = blocks,
        ).also { proof ->
            require(proof.hasValidShape())
            require(proof.matchesPublishedPlan(update.schedule))
        }
    }

    /**
     * Atomically installs one bundled-core composition only if every input generation is unchanged.
     *
     * The local fingerprint is retained as encrypted display provenance. Server publication
     * evidence is intentionally removed, so this schedule remains non-actionable until an
     * authoritative preview is published and reconciled.
     */
    fun installLocalScheduleComposition(
        expectedState: DayWeaveUiState,
        update: CanonicalPlanUpdate,
        provenance: LocalScheduleCompositionProvenanceSnapshot,
    ): LocalScheduleCompositionTransition? {
        validateCanonicalPlanUpdate(update, requireServerDigest = false)
        require(provenance.hasValidShape()) { "Local schedule provenance is invalid" }
        require(
            update.inputDigest == provenance.localInputFingerprint &&
                update.syncOrigin == provenance.syncOrigin &&
                update.configurationId == provenance.configurationId &&
                update.deltaCursor == provenance.deltaCursor &&
                update.generatedAt == provenance.generatedAt &&
                update.planningZoneId == provenance.timezoneName &&
                update.items.associate { it.id to it.revision } == provenance.sourceItemRevisions,
        ) { "Local schedule provenance does not match its composition" }
        var installedProvenance: LocalScheduleCompositionProvenanceSnapshot? = null
        val mutation = mutateDurablyWithSnapshot { current ->
            require(current == expectedState && mutableDurableState.value == expectedState) {
                "Planner state changed while the local composition was in flight"
            }
            requireLocalScheduleCompositionPreflight(current, provenance)
            val installedWithoutProvenance = canonicalPlanState(current, update).copy(
                pendingSchedulePublication = null,
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                localScheduleCompositionProvenance = null,
                scheduleMessage =
                    "Composed on this device · sync before starting or changing canonical work",
            )
            val exactProvenance = provenance.copy(
                stateInputFingerprint =
                    installedWithoutProvenance.localScheduleCompositionStateFingerprint(),
            )
            val installed = installedWithoutProvenance.copy(
                localScheduleCompositionProvenance = exactProvenance,
            )
            require(exactProvenance.matchesState(installed)) {
                "Local schedule composition did not retain its exact source generation"
            }
            installedProvenance = exactProvenance
            installed
        } ?: return null
        return LocalScheduleCompositionTransition(
            provenance = requireNotNull(installedProvenance),
            persistence = requireNotNull(mutation.receipt),
        )
    }

    private fun requireLocalScheduleCompositionPreflight(
        current: DayWeaveUiState,
        provenance: LocalScheduleCompositionProvenanceSnapshot,
    ) {
        require(
            current.canonicalSyncOrigin == provenance.syncOrigin &&
                current.canonicalConfigurationId == provenance.configurationId &&
                current.canonicalDeltaCursor == provenance.deltaCursor &&
                current.canonicalItems.associate { it.id to it.revision } ==
                provenance.sourceItemRevisions,
        ) { "Local composition needs one exact durable canonical binding" }
        require(
            current.canonicalExecutionSyncOrigin == provenance.syncOrigin &&
                current.canonicalExecutionConfigurationId == provenance.configurationId &&
                current.canonicalExecutionHistoryVerified &&
                current.canonicalExecutionHistoryContinuityEstablished &&
                current.canonicalExecutionHistoryWindowRevision ==
                current.canonicalExecutionRevision &&
                (current.canonicalExecutionRevision == 0L) ==
                current.canonicalExecutionHistoryWindow.isEmpty() &&
                current.canonicalExecutionHistoryWindow.all {
                    it.revision <= current.canonicalExecutionRevision
                },
        ) { "Verified execution history does not match the canonical binding" }
        require(
            current.pendingSchedulePublication == null &&
                current.pendingProposalApplicationMutation == null &&
                current.pendingCanonicalMutation == null &&
                current.pendingCanonicalAuthoringMutations.isEmpty() &&
                current.pendingExecutionCommand == null &&
                current.pendingExecutionDeferIntent == null &&
                current.canonicalExecutionSession == null &&
                current.activeSession == null &&
                current.schedule.none {
                    it.canonicalItemId != null &&
                        it.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED)
                },
        ) { "A pending or active canonical operation blocks local composition" }
        require(
            current.terminalExecutionOutcomes.values.none { outcome ->
                outcome.requiresCanonicalItemProjection &&
                    outcome.canonicalProjectionRevision == null &&
                    outcome.canonicalProjectionResolution == null &&
                    current.isNewestExecutionForProjection(outcome.session)
            },
        ) { "An authoritative terminal projection must finish before local composition" }
    }

    private fun validateCanonicalPlanUpdate(
        update: CanonicalPlanUpdate,
        requireServerDigest: Boolean = true,
    ) {
        if (requireServerDigest) {
            requireScheduleInputDigest(update.inputDigest)
        } else {
            require(
                update.inputDigest.length == LOCAL_SCHEDULE_FINGERPRINT_PREFIX.length + 64 &&
                    update.inputDigest.startsWith(LOCAL_SCHEDULE_FINGERPRINT_PREFIX) &&
                    update.inputDigest.drop(LOCAL_SCHEDULE_FINGERPRINT_PREFIX.length).all {
                        it in '0'..'9' || it in 'a'..'f'
                    },
            ) { "Local composition fingerprint is invalid" }
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
            runCatching { UUID.fromString(occurrenceId).version() == 5 }.getOrDefault(false) &&
                seriesItemId in itemsById
        }) { "Canonical occurrence ownership is invalid" }
        require(update.occurrenceSources.keys == update.occurrenceSeriesItemIds.keys)
        require(update.occurrenceSources.all { (occurrenceId, source) ->
            val item = itemsById[source.itemId] ?: return@all false
            update.occurrenceSeriesItemIds[occurrenceId] == source.itemId &&
                source.hasValidRecurrenceSourceFor(item)
        }) { "Canonical occurrence source envelopes are invalid" }
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
    }

    private fun canonicalPlanState(
        current: DayWeaveUiState,
        update: CanonicalPlanUpdate,
    ): DayWeaveUiState {
            val planningZone = ZoneId.of(update.planningZoneId)
            val planningDate = Instant.parse(update.generatedAt).atZone(planningZone).toLocalDate()
            val itemsById = update.items.associateBy { it.id }
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
                        current.pendingSchedulePublication == null &&
                        current.pendingCanonicalMutation == null &&
                        current.pendingExecutionCommand == null &&
                        current.pendingExecutionDeferIntent == null &&
                        current.canonicalExecutionSession == null &&
                        current.canonicalExecutionHistoryWindow.isEmpty() &&
                        current.canonicalExecutionHistoryWindowRevision == null &&
                        !current.canonicalExecutionHistoryContinuityEstablished &&
                        !current.canonicalExecutionHistoryVerified &&
                        current.terminalExecutionOutcomes.isEmpty(),
                ) { "Credential replacement must quarantine canonical state before composition" }
            }
            val retainedClosedOutcomes = if (sameBinding) {
                retainedClosedExecutionOutcomes(
                    validatedClosedExecutionOutcomes(current.terminalExecutionOutcomes)
                        .filter { it.syncOrigin == update.syncOrigin }
                        .associateBy { it.session.id },
                )
            } else {
                emptyMap()
            }
            val closedOutcomesNewestFirst = retainedClosedOutcomes.values.sortedWith(
                compareByDescending<TerminalExecutionOutcomeSnapshot> {
                    Instant.parse(it.session.updatedAt)
                }.thenByDescending { it.session.id },
            )
            val authoritativeLease = current.canonicalExecutionSession
                ?.takeIf { sameBinding && it.status in OPEN_EXECUTION_STATUSES }
            val freshSchedule = update.schedule.map { block ->
                val newestClosed = closedOutcomesNewestFirst.firstOrNull { outcome ->
                    outcome.session.matchesProjectionLineage(block)
                }
                val terminal = newestClosed?.takeIf { outcome ->
                    outcome.session.status in TERMINAL_EXECUTION_STATUSES &&
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
            val authoringOverlay = canonicalAuthoringRefreshOverlay(
                current = current,
                freshItems = update.items,
                sameBinding = sameBinding,
            )
            val reconciledItemsById = authoringOverlay.items.associateBy(CanonicalItemSnapshot::id)
            val reconciledItemIds = reconciledItemsById.keys
            val authoringSafeSchedule = orderedSchedule.filter { block ->
                val exactCanonicalRevision = block.canonicalItemId?.let { canonicalId ->
                    reconciledItemsById[canonicalId]?.revision == block.canonicalRevision
                } != false
                val exactRemoteLeasePlaceholder = authoritativeLease?.let { lease ->
                    block.canonicalBlockKind == "remote_execution_lease" &&
                        lease.matches(block) && block.canonicalRevision == lease.itemRevision
                } == true
                exactCanonicalRevision || exactRemoteLeasePlaceholder
            }
            val authoringFilteredSchedule = authoringSafeSchedule.size != orderedSchedule.size
            val authoringProofInvalidated = authoringFilteredSchedule ||
                authoringOverlay.mutations.any {
                    it.disposition == CanonicalAuthoringDisposition.PENDING
                }
            return current.copy(
                canonicalItems = authoringOverlay.items,
                pendingCanonicalAuthoringMutations = authoringOverlay.mutations,
                canonicalRecentlyDeleted = authoringOverlay.deleted,
                canonicalSyncOrigin = update.syncOrigin,
                canonicalConfigurationId = update.configurationId,
                canonicalDeltaCursor = update.deltaCursor,
                schedule = authoringSafeSchedule,
                activeSession = restoredSession?.takeIf { session ->
                    authoringSafeSchedule.any { it.id == session.itemId }
                },
                publishedScheduleRevision = current.publishedScheduleRevision.takeUnless {
                    authoringProofInvalidated
                },
                publishedScheduleProof = current.publishedScheduleProof.takeUnless {
                    authoringProofInvalidated
                },
                scheduleInputDigest = update.inputDigest.takeUnless { authoringProofInvalidated },
                localScheduleCompositionProvenance = null,
                scheduleGeneratedAt = update.generatedAt,
                schedulePlanningZoneId = update.planningZoneId,
                recurrenceOutcomes = if (sameBinding) {
                    current.recurrenceOutcomes.filterValues { it.itemId in reconciledItemIds }
                } else {
                    emptyMap()
                },
                recurrenceCompletionAnchors = if (sameBinding) {
                    current.recurrenceCompletionAnchors.filterKeys { it in reconciledItemIds }
                } else {
                    emptyMap()
                },
                recurrenceMoves = if (sameBinding) {
                    current.recurrenceMoves.filterValues { move ->
                        move.source?.let { source ->
                            source.itemId == move.itemId &&
                                reconciledItemsById[move.itemId]?.revision == source.itemRevision &&
                                runCatching {
                                    Instant.parse(move.endAt) >=
                                        planningDate.atStartOfDay(planningZone).toInstant()
                                }.getOrDefault(false)
                        } == true
                    }
                } else {
                    emptyMap()
                },
                // A plan read alone cannot prove that a timed-out write will not commit later.
                // The sync manager clears this only by replaying the exact durable request.
                pendingCanonicalMutation = current.pendingCanonicalMutation,
                terminalExecutionOutcomes = retainedClosedOutcomes,
                unscheduledWork = update.unscheduledWork.filter { it.itemId in reconciledItemIds },
                occurrenceSeriesItemIds = update.occurrenceSeriesItemIds.filterValues {
                    it in reconciledItemIds
                },
                recurrenceOccurrenceSources = update.occurrenceSources.filterValues { source ->
                    reconciledItemsById[source.itemId]?.revision == source.itemRevision
                },
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

    /** Writes the exact publication tuple before its first network send. */
    fun stageSchedulePublication(
        publication: PendingSchedulePublication,
    ): PlannerPersistenceReceipt? {
        validateSchedulePublicationJournal(publication)
        return mutateDurably { current ->
            require(current.pendingSchedulePublication == null) {
                "A schedule publication already needs exact reconciliation"
            }
            require(current.pendingCanonicalMutation == null) {
                "A canonical mutation must be reconciled before schedule publication"
            }
            require(current.pendingExecutionCommand == null) {
                "An execution command must be reconciled before schedule publication"
            }
            require(current.pendingExecutionDeferIntent == null) {
                "A move-later intent must be reconciled before schedule publication"
            }
            require(current.pendingProposalApplicationMutation == null) {
                "A proposal application must be reconciled before schedule publication"
            }
            require(!current.hasPendingCanonicalAuthoringOverlay()) {
                "Pending canonical authoring must be reconciled before schedule publication"
            }
            current.canonicalSyncOrigin?.let { origin ->
                require(
                    origin == publication.syncOrigin &&
                        current.canonicalConfigurationId == publication.configurationId,
                ) { "Publication binding does not match the canonical cache" }
            }
            current.canonicalExecutionSyncOrigin?.let { origin ->
                require(
                    origin == publication.syncOrigin &&
                        current.canonicalExecutionConfigurationId == publication.configurationId,
                ) { "Publication binding does not match canonical execution state" }
            }
            current.copy(
                pendingSchedulePublication = publication,
                scheduleMessage =
                    "Publishing validated schedule · awaiting authoritative confirmation",
            )
        }
    }

    /**
     * Installs the locally accepted candidate and clears its exact journal in one generation.
     * Equality against the complete expected journal is the stale-response/CAS fence.
     */
    fun commitSchedulePublication(
        expected: PendingSchedulePublication,
        revision: PublishedScheduleRevisionSnapshot,
        replayed: Boolean,
    ): PlannerPersistenceReceipt? {
        validateSchedulePublicationJournal(expected)
        validatePublishedScheduleRevision(expected, revision)
        require(!replayed) { "A replayed publication cannot grant execution authority" }
        val proof = publishedScheduleProof(expected, revision)
        return mutateDurably { current ->
            require(current.pendingSchedulePublication == expected) {
                "Schedule publication changed before its response was committed"
            }
            val accepted = canonicalPlanState(current, expected.candidate)
            require(accepted.scheduleInputDigest == revision.inputDigest) {
                "Published schedule no longer matches the local canonical authoring overlay"
            }
            accepted.copy(
                pendingSchedulePublication = null,
                publishedScheduleRevision = revision,
                publishedScheduleProof = proof,
            )
        }
    }

    /**
     * An exact replay proves that this request committed once, but not that its revision is still
     * current. Clear only the matching journal and invalidate every local publication proof before
     * pulling and publishing a fresh snapshot.
     */
    fun resolveReplayedSchedulePublication(
        expected: PendingSchedulePublication,
        revision: PublishedScheduleRevisionSnapshot,
    ): PlannerPersistenceReceipt? {
        validateSchedulePublicationJournal(expected)
        validatePublishedScheduleRevision(expected, revision)
        return mutateDurably { current ->
            require(current.pendingSchedulePublication == expected) {
                "Schedule publication changed before its replay was resolved"
            }
            current.copy(
                pendingSchedulePublication = null,
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                scheduleMessage =
                    "An exact publication replay may be superseded · recomposing before use",
            )
        }
    }

    /** A typed stale rejection proves this candidate did not publish and is safe to discard. */
    fun discardStaleSchedulePublication(
        expected: PendingSchedulePublication,
    ): PlannerPersistenceReceipt? {
        validateSchedulePublicationJournal(expected)
        return mutateDurably { current ->
            require(current.pendingSchedulePublication == expected) {
                "Schedule publication changed before its stale rejection was resolved"
            }
            current.copy(
                pendingSchedulePublication = null,
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                scheduleMessage =
                    "The validated preview became stale · recomposing before schedule use",
            )
        }
    }

    /** Used before every first send and restart replay so a corrupted candidate never leaves disk. */
    fun validateSchedulePublication(publication: PendingSchedulePublication) {
        validateSchedulePublicationJournal(publication)
    }

    /** Persists one exact reviewed apply/undo request before any network byte can leave. */
    fun stageProposalApplicationMutation(
        mutation: PendingProposalApplicationMutation,
    ): PlannerPersistenceReceipt? {
        validateProposalApplicationJournal(mutation)
        return mutateDurably { current ->
            require(current.pendingProposalApplicationMutation == null) {
                "A proposal application already needs exact reconciliation"
            }
            require(current.pendingSchedulePublication == null) {
                "A schedule publication must be reconciled before applying a proposal"
            }
            require(current.pendingCanonicalMutation == null) {
                "A canonical mutation must be reconciled before applying a proposal"
            }
            require(current.pendingExecutionCommand == null) {
                "An execution command must be reconciled before applying a proposal"
            }
            require(current.pendingExecutionDeferIntent == null) {
                "A move-later intent must be reconciled before applying a proposal"
            }
            require(!current.hasPendingCanonicalAuthoringOverlay()) {
                "Canonical authoring must be reconciled before applying a proposal"
            }
            current.canonicalSyncOrigin?.let { origin ->
                require(
                    mutation.syncOrigin == origin &&
                        mutation.configurationId == current.canonicalConfigurationId,
                ) { "Proposal application binding does not match the canonical cache" }
            }
            current.canonicalExecutionSyncOrigin?.let { origin ->
                require(
                    mutation.syncOrigin == origin &&
                        mutation.configurationId == current.canonicalExecutionConfigurationId,
                ) { "Proposal application binding does not match canonical execution state" }
            }
            require(current.proposalApplications.values.all {
                it.syncOrigin == mutation.syncOrigin &&
                    it.configurationId == mutation.configurationId
            }) { "Proposal application binding does not match retained receipts" }
            when (mutation.kind) {
                ProposalApplicationMutationKind.APPLY -> {
                    require(current.proposalApplications[mutation.proposalId] == null) {
                        "This proposal already has a durable application receipt"
                    }
                    val proposal = current.suggestions.firstOrNull {
                        it.id == mutation.proposalId && it.remoteRevision != null
                    } ?: throw IllegalArgumentException("The reviewed proposal is not cached")
                    require(
                        proposal.disposition == SuggestionDisposition.PENDING &&
                            proposal.remoteRevision == mutation.expectedProposalRevision &&
                            proposal.isApplicationReady,
                    ) { "The reviewed proposal changed before application" }
                    proposal.remoteExpiresAt?.let { expiresAt ->
                        require(Instant.parse(expiresAt).toEpochMilli() > nowEpochMillis()) {
                            "The reviewed proposal has expired"
                        }
                    }
                }
                ProposalApplicationMutationKind.UNDO -> {
                    val receipt = current.proposalApplications[mutation.proposalId]
                        ?: throw IllegalArgumentException("The applied proposal receipt is unavailable")
                    require(
                        receipt.status == ProposalApplicationStatusSnapshot.APPLIED &&
                            receipt.applicationId == mutation.applicationId &&
                            receipt.applicationRevision == mutation.expectedApplicationRevision &&
                            receipt.appliedProposalRevision == mutation.expectedProposalRevision &&
                            receipt.commandIds == mutation.expectedCommandIds &&
                            receipt.syncOrigin == mutation.syncOrigin &&
                            receipt.configurationId == mutation.configurationId,
                    ) { "The proposal receipt changed before undo" }
                    require(Instant.parse(receipt.undoExpiresAt).toEpochMilli() > nowEpochMillis()) {
                        "The proposal undo window has expired"
                    }
                }
            }
            current.copy(
                pendingProposalApplicationMutation = mutation,
                scheduleMessage = when (mutation.kind) {
                    ProposalApplicationMutationKind.APPLY ->
                        "Applying the exact reviewed proposal · awaiting confirmation"
                    ProposalApplicationMutationKind.UNDO ->
                        "Undoing the proposal application · awaiting confirmation"
                },
            )
        }
    }

    /** Clears the matching exact journal and installs its content-free receipt atomically. */
    fun commitProposalApplicationMutation(
        expected: PendingProposalApplicationMutation,
        receipt: ProposalApplicationReceiptSnapshot,
    ): PlannerPersistenceReceipt? {
        validateProposalApplicationJournal(expected)
        validateProposalApplicationReceipt(receipt)
        require(receipt.syncOrigin == expected.syncOrigin)
        require(receipt.configurationId == expected.configurationId)
        require(receipt.proposalId == expected.proposalId)
        require(receipt.commandIds == expected.expectedCommandIds)
        when (expected.kind) {
            ProposalApplicationMutationKind.APPLY -> require(
                expected.expectedProposalRevision < Long.MAX_VALUE &&
                    receipt.appliedProposalRevision == expected.expectedProposalRevision + 1L &&
                    (receipt.status == ProposalApplicationStatusSnapshot.APPLIED ||
                        receipt.status == ProposalApplicationStatusSnapshot.UNDONE),
            ) { "Apply recovery returned an invalid application receipt" }
            ProposalApplicationMutationKind.UNDO -> require(
                receipt.appliedProposalRevision == expected.expectedProposalRevision &&
                receipt.status == ProposalApplicationStatusSnapshot.UNDONE &&
                    receipt.applicationId == expected.applicationId &&
                    receipt.applicationRevision ==
                    requireNotNull(expected.expectedApplicationRevision) + 1L,
            ) { "Undo recovery returned a mismatched receipt" }
        }
        return mutateDurably { current ->
            require(current.pendingProposalApplicationMutation == expected) {
                "Proposal application fence changed during reconciliation"
            }
            expected.applicationId?.let { applicationId ->
                val previous = current.proposalApplications[expected.proposalId]
                    ?: throw IllegalArgumentException("The applied receipt was lost during undo")
                require(
                    previous.applicationId == applicationId &&
                        previous.applicationRevision == expected.expectedApplicationRevision &&
                        previous.status == ProposalApplicationStatusSnapshot.APPLIED &&
                        previous.proposalId == receipt.proposalId &&
                        previous.appliedProposalRevision == receipt.appliedProposalRevision &&
                        previous.commandIds == receipt.commandIds &&
                        previous.affectedItemIds == receipt.affectedItemIds &&
                        previous.appliedAt == receipt.appliedAt &&
                        previous.undoExpiresAt == receipt.undoExpiresAt,
                ) { "Undo receipt does not preserve the applied application identity" }
            }
            current.withProposalApplicationReceipt(receipt).copy(
                pendingProposalApplicationMutation = null,
                scheduleMessage = if (receipt.status == ProposalApplicationStatusSnapshot.UNDONE) {
                    "Proposal application undone · refreshing canonical items and schedule"
                } else {
                    "Proposal applied transactionally · refreshing canonical items and schedule"
                },
            )
        }
    }

    /** Stores an authoritative lookup result without inventing an application or Inbox draft. */
    fun recordProposalApplicationReceipt(
        receipt: ProposalApplicationReceiptSnapshot,
    ): PlannerPersistenceReceipt? {
        validateProposalApplicationReceipt(receipt)
        return mutateDurably { current ->
            require(current.pendingProposalApplicationMutation == null) {
                "An exact proposal request must be reconciled before recording another receipt"
            }
            current.canonicalSyncOrigin?.let { origin ->
                require(origin == receipt.syncOrigin &&
                    current.canonicalConfigurationId == receipt.configurationId)
            }
            require(current.proposalApplications.values.all {
                it.syncOrigin == receipt.syncOrigin &&
                    it.configurationId == receipt.configurationId
            }) { "Proposal application receipts cannot cross API bindings" }
            current.proposalApplications[receipt.proposalId]?.let { previous ->
                require(previous.applicationId == receipt.applicationId)
                require(receipt.applicationRevision >= previous.applicationRevision)
            }
            current.withProposalApplicationReceipt(receipt)
        }
    }

    /** Clears only a definitively uncommitted request; ambiguous results retain the exact journal. */
    fun clearPendingProposalApplicationMutation(
        expected: PendingProposalApplicationMutation,
        message: String,
    ): PlannerPersistenceReceipt? {
        validateProposalApplicationJournal(expected)
        return mutateDurably { current ->
            require(current.pendingProposalApplicationMutation == expected) {
                "Proposal application fence changed during reconciliation"
            }
            current.copy(
                pendingProposalApplicationMutation = null,
                scheduleMessage = message,
            )
        }
    }

    fun validateProposalApplicationMutation(mutation: PendingProposalApplicationMutation) {
        validateProposalApplicationJournal(mutation)
    }

    private fun DayWeaveUiState.withProposalApplicationReceipt(
        receipt: ProposalApplicationReceiptSnapshot,
    ): DayWeaveUiState = copy(
        proposalApplications = proposalApplications + (receipt.proposalId to receipt),
        suggestions = suggestions.map { suggestion ->
            if (suggestion.id == receipt.proposalId && suggestion.remoteRevision != null) {
                suggestion.copy(
                    disposition = SuggestionDisposition.TRANSACTIONALLY_APPLIED,
                    remoteRevision = maxOf(
                        suggestion.remoteRevision,
                        receipt.appliedProposalRevision,
                    ),
                )
            } else {
                suggestion
            }
        },
        inbox = inbox.filterNot { it.id == "proposal-${receipt.proposalId}" },
        publishedScheduleRevision = null,
        publishedScheduleProof = null,
        scheduleInputDigest = null,
    )

    private fun validateProposalApplicationJournal(
        mutation: PendingProposalApplicationMutation,
    ) {
        require(mutation.schemaVersion == PROPOSAL_APPLICATION_JOURNAL_VERSION)
        listOf(mutation.idempotencyKey, mutation.proposalId).forEach(::requireCanonicalUuid)
        require(mutation.syncOrigin.isNotBlank() && mutation.expectedProposalRevision > 0L)
        mutation.configurationId?.let { require(it.isNotBlank()) }
        requireNotNull(runCatching { Instant.parse(mutation.preparedAt) }.getOrNull())
        require(mutation.expectedCommandIds.size in 1..MAX_PROPOSAL_APPLICATION_COMMANDS)
        require(mutation.expectedCommandIds.distinct().size == mutation.expectedCommandIds.size)
        mutation.expectedCommandIds.forEach(::requireCanonicalUuid)
        when (mutation.kind) {
            ProposalApplicationMutationKind.APPLY -> {
                val previewId = requireNotNull(mutation.previewId)
                val reviewHash = requireNotNull(mutation.expectedReviewHash)
                requireCanonicalUuid(previewId)
                require(reviewHash.isSha256ReviewHash())
                require(mutation.applicationId == null && mutation.expectedApplicationRevision == null)
                validateProposalApplyHttpRequest(
                    expectedBaseUrl = mutation.syncOrigin,
                    request = mutation.request,
                    previewId = previewId,
                    expectedReviewHash = reviewHash,
                )
            }
            ProposalApplicationMutationKind.UNDO -> {
                val applicationId = requireNotNull(mutation.applicationId)
                val expectedApplicationRevision =
                    requireNotNull(mutation.expectedApplicationRevision)
                requireCanonicalUuid(applicationId)
                require(expectedApplicationRevision > 0L)
                require(mutation.previewId == null && mutation.expectedReviewHash == null)
                validateProposalUndoHttpRequest(
                    expectedBaseUrl = mutation.syncOrigin,
                    request = mutation.request,
                    applicationId = applicationId,
                    expectedApplicationRevision = expectedApplicationRevision,
                )
            }
        }
    }

    private fun validateProposalApplicationReceipt(
        receipt: ProposalApplicationReceiptSnapshot,
    ) {
        require(receipt.schemaVersion == PROPOSAL_APPLICATION_RECEIPT_VERSION)
        listOf(receipt.applicationId, receipt.proposalId).forEach(::requireCanonicalUuid)
        require(receipt.syncOrigin.isNotBlank() && receipt.appliedProposalRevision > 0L)
        receipt.configurationId?.let { require(it.isNotBlank()) }
        require(receipt.commandIds.size in 1..MAX_PROPOSAL_APPLICATION_COMMANDS)
        require(receipt.affectedItemIds.isNotEmpty())
        require(receipt.commandIds.distinct().size == receipt.commandIds.size)
        require(receipt.affectedItemIds.distinct().size == receipt.affectedItemIds.size)
        receipt.commandIds.forEach(::requireCanonicalUuid)
        receipt.affectedItemIds.forEach(::requireCanonicalUuid)
        val appliedAt = Instant.parse(receipt.appliedAt)
        val undoExpiresAt = Instant.parse(receipt.undoExpiresAt)
        require(undoExpiresAt > appliedAt)
        when (receipt.status) {
            ProposalApplicationStatusSnapshot.APPLIED -> require(
                receipt.applicationRevision == 1L && receipt.undoneAt == null,
            )
            ProposalApplicationStatusSnapshot.UNDONE -> require(
                receipt.applicationRevision == 2L &&
                    requireNotNull(receipt.undoneAt).let(Instant::parse).let { undoneAt ->
                        undoneAt >= appliedAt && undoneAt <= undoExpiresAt
                    },
            )
        }
    }

    private fun requireCanonicalUuid(raw: String) {
        val parsed = UUID.fromString(raw)
        require(parsed != NIL_UUID && parsed.toString() == raw)
    }

    private fun String.isSha256ReviewHash(): Boolean =
        length == 71 && startsWith("sha256:") &&
            drop(7).all { it in '0'..'9' || it in 'a'..'f' }

    private fun validateSchedulePublicationJournal(publication: PendingSchedulePublication) {
        require(publication.schemaVersion == SCHEDULE_PUBLICATION_JOURNAL_VERSION)
        val idempotencyKey = UUID.fromString(publication.idempotencyKey)
        require(idempotencyKey != NIL_UUID && idempotencyKey.toString() == publication.idempotencyKey)
        require(publication.syncOrigin.isNotBlank())
        publication.configurationId?.let { require(it.isNotBlank()) }
        requireNotNull(runCatching { Instant.parse(publication.preparedAt) }.getOrNull())
        validateCanonicalPlanUpdate(publication.candidate)
        require(publication.candidate.syncOrigin == publication.syncOrigin)
        require(publication.candidate.configurationId == publication.configurationId)
        val request = validateSchedulePublishHttpRequest(
            expectedBaseUrl = publication.syncOrigin,
            request = publication.request,
        )
        require(request.idempotencyKey == publication.idempotencyKey)
        require(request.expectedInputDigest == publication.candidate.inputDigest)
        require(request.schedule.asOf == publication.candidate.generatedAt)
        require(request.schedule.timezoneName == publication.candidate.planningZoneId)
    }

    private fun validatePublishedScheduleRevision(
        publication: PendingSchedulePublication,
        revision: PublishedScheduleRevisionSnapshot,
    ) {
        val revisionId = UUID.fromString(revision.id)
        require(revisionId != NIL_UUID && revisionId.toString() == revision.id)
        require(revision.revisionNumber > 0uL)
        require(revision.revision == "${revision.revisionNumber}:${revision.id}")
        requireScheduleInputDigest(revision.inputDigest)
        val request = validateSchedulePublishHttpRequest(
            expectedBaseUrl = publication.syncOrigin,
            request = publication.request,
        )
        require(revision.inputDigest == request.expectedInputDigest)
        require(revision.horizonStart == request.schedule.horizonStart)
        require(revision.horizonEnd == request.schedule.horizonEnd)
        require(revision.timezoneName == request.schedule.timezoneName)
        requireNotNull(runCatching { Instant.parse(revision.publishedAt) }.getOrNull())
    }

    private fun publishedScheduleProof(
        publication: PendingSchedulePublication,
        revision: PublishedScheduleRevisionSnapshot,
    ): PublishedScheduleProofSnapshot {
        val configurationId = requireNotNull(publication.configurationId) {
            "A schedule publication needs an opaque credential binding"
        }
        val request = validateSchedulePublishHttpRequest(
            expectedBaseUrl = publication.syncOrigin,
            request = publication.request,
        )
        val publicationBlocks = publication.candidate.schedule.filter {
            it.canonicalBlockKind != null && it.canonicalBlockKind != "remote_execution_lease"
        }
        val blocks = publicationBlocks.map(PublishedScheduleBlockProofSnapshot::from)
            .sortedBy { it.id }
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = publication.syncOrigin,
            configurationId = configurationId,
            revision = revision,
            asOf = request.schedule.asOf,
            blocks = blocks,
        )
        require(proof.hasValidShape()) { "Published schedule proof is invalid" }
        require(proof.blocks.size == publicationBlocks.size)
        require(proof.matchesPublishedPlan(publication.candidate.schedule)) {
            "Published schedule proof does not match the accepted candidate"
        }
        return proof
    }

    fun canonicalAuthoringMutation(id: String): PendingCanonicalAuthoringMutation? =
        state.value.pendingCanonicalAuthoringMutations.firstOrNull { it.id == id }

    private fun MutationResult.canonicalAuthoringTransition(
        id: String,
    ): CanonicalAuthoringTransition = CanonicalAuthoringTransition(
        mutation = requireNotNull(snapshot.pendingCanonicalAuthoringMutations.firstOrNull {
            it.id == id
        }) { "Canonical authoring mutation was removed during durable normalization" },
        persistence = requireNotNull(receipt),
    )

    /**
     * A composed plan can omit Inbox parents and can race a restore performed elsewhere. Retain
     * every canonical cache row needed by the local authoring graph, while accepting a strictly
     * newer active upsert as authoritative reconciliation of a queued restore.
     */
    private fun canonicalAuthoringRefreshOverlay(
        current: DayWeaveUiState,
        freshItems: List<CanonicalItemSnapshot>,
        sameBinding: Boolean,
    ): CanonicalAuthoringRefreshOverlay {
        if (!sameBinding) {
            require(current.pendingCanonicalAuthoringMutations.all { it.syncOrigin == null }) {
                "Bound canonical authoring cannot cross a credential refresh"
            }
            return CanonicalAuthoringRefreshOverlay(
                items = freshItems.sortedCanonicalItems(),
                mutations = current.pendingCanonicalAuthoringMutations,
                deleted = emptyList(),
            )
        }
        val freshById = freshItems.associateBy(CanonicalItemSnapshot::id)
        val reconciledRestoreItemIds = current.pendingCanonicalAuthoringMutations.asSequence()
            .filter { it.operation == CanonicalAuthoringOperation.RESTORE }
            .filter { mutation ->
                freshById[mutation.itemId]?.revision?.let { revision ->
                    revision > requireNotNull(mutation.expectedRevision)
                } == true
            }
            .map(PendingCanonicalAuthoringMutation::itemId)
            .toSet()
        val mutations = current.pendingCanonicalAuthoringMutations.filterNot {
            it.itemId in reconciledRestoreItemIds &&
                it.operation == CanonicalAuthoringOperation.RESTORE
        }
        val overlayMutations = mutations.filter {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        }
        val retainedRestoreItemIds = overlayMutations.asSequence()
            .filter { it.operation == CanonicalAuthoringOperation.RESTORE }
            .map(PendingCanonicalAuthoringMutation::itemId)
            .toSet()
        val requiredCurrentIds = mutableSetOf<String>()
        overlayMutations.forEach { mutation ->
            when (mutation.operation) {
                CanonicalAuthoringOperation.REPLACE,
                CanonicalAuthoringOperation.TRASH,
                -> requiredCurrentIds += mutation.itemId
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.RESTORE,
                -> Unit
            }
            mutation.draft?.parentId?.let(requiredCurrentIds::add)
        }
        val currentById = current.canonicalItems.associateBy(CanonicalItemSnapshot::id)
        val pendingItemIds = overlayMutations.mapTo(hashSetOf()) { it.itemId }
        var frontier = requiredCurrentIds.toList()
        while (frontier.isNotEmpty()) {
            val next = mutableListOf<String>()
            frontier.forEach { itemId ->
                currentById[itemId]?.parentId?.let { parentId ->
                    if (parentId !in pendingItemIds && requiredCurrentIds.add(parentId)) {
                        next += parentId
                    }
                }
            }
            frontier = next
        }
        val overlayByItem = overlayMutations.associateBy(PendingCanonicalAuthoringMutation::itemId)
        val mergedItems = (
            freshItems.filterNot {
                it.id in retainedRestoreItemIds || it.id in requiredCurrentIds
            } + requiredCurrentIds.mapNotNull { itemId ->
                val mutationBase = overlayByItem[itemId]?.baseItem
                freshById[itemId]?.takeIf { it == mutationBase } ?: currentById[itemId]
            }
            ).distinctBy(CanonicalItemSnapshot::id).sortedCanonicalItems()
        val activeIds = mergedItems.mapTo(hashSetOf()) { it.id }
        val deleted = current.canonicalRecentlyDeleted.filterNot {
            it.id in reconciledRestoreItemIds || it.id in activeIds
        }
        val overlayState = current.copy(
            canonicalItems = mergedItems,
            pendingCanonicalAuthoringMutations = mutations,
            canonicalRecentlyDeleted = deleted,
        )
        validateCanonicalAuthoringOverlay(overlayState)
        return CanonicalAuthoringRefreshOverlay(mergedItems, mutations, deleted)
    }

    private fun List<CanonicalItemSnapshot>.sortedCanonicalItems(): List<CanonicalItemSnapshot> =
        sortedWith(compareBy({ it.parentId.orEmpty() }, { it.siblingOrder }, { it.id }))

    fun sortedCanonicalAuthoringMutations(): List<PendingCanonicalAuthoringMutation> =
        dependencySortedCanonicalAuthoringMutations(state.value)

    /**
     * Rebases only unsent replace/trash requests when an authoritative refresh proves that the
     * server changed derived hierarchy metadata, but no user-authored field. Child mutations
     * increment parent revisions, so this fence is required before an offline parent batch sends.
     */
    fun rebaseUnsubmittedCanonicalAuthoringBases(
        authoritativeItems: List<CanonicalItemSnapshot>,
    ): PlannerPersistenceReceipt? {
        authoritativeItems.forEach(CanonicalItemSnapshot::requireCanonicalAuthoringShape)
        val authoritativeById = authoritativeItems.associateBy(CanonicalItemSnapshot::id)
        require(authoritativeById.size == authoritativeItems.size) {
            "Authoritative canonical item ids must be unique"
        }
        return mutateDurably { current ->
            var changed = false
            val mutations = current.pendingCanonicalAuthoringMutations.map { mutation ->
                if (
                    mutation.isSubmitted ||
                    mutation.disposition != CanonicalAuthoringDisposition.PENDING ||
                    mutation.operation !in setOf(
                        CanonicalAuthoringOperation.REPLACE,
                        CanonicalAuthoringOperation.TRASH,
                    )
                ) {
                    return@map mutation
                }
                val base = mutation.baseItem ?: return@map mutation
                val authoritative = authoritativeById[mutation.itemId] ?: return@map mutation
                val expectedRevision = requireNotNull(mutation.expectedRevision)
                if (
                    authoritative.revision <= expectedRevision ||
                    !authoritative.sameAuthoredFields(base)
                ) {
                    return@map mutation
                }
                changed = true
                mutation.copy(
                    expectedRevision = authoritative.revision,
                    baseItem = authoritative,
                    syncOrigin = null,
                    configurationId = null,
                ).also(PendingCanonicalAuthoringMutation::requireValid)
            }
            if (!changed) {
                current
            } else {
                current.copy(
                    pendingCanonicalAuthoringMutations = mutations,
                    publishedScheduleRevision = null,
                    publishedScheduleProof = null,
                    scheduleInputDigest = null,
                    scheduleMessage =
                        "Queued hierarchy changes rebased to authoritative parent revisions",
                )
            }
        }
    }

    fun enqueueCanonicalCreate(
        draft: CanonicalItemDraft,
        itemId: String = UUID.randomUUID().toString(),
        mutationId: String = UUID.randomUUID().toString(),
    ): CanonicalAuthoringTransition? = enqueueCanonicalAuthoring(
        itemId = itemId,
        mutationId = mutationId,
        operation = CanonicalAuthoringOperation.CREATE,
    ) { _ ->
        PendingCanonicalAuthoringMutation(
            id = mutationId,
            itemId = itemId,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft.normalized(),
            createdAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
        )
    }

    /** Atomically converts one legacy/proposal review draft into canonical local intent. */
    fun enqueueCanonicalCreateFromInbox(
        inboxId: String,
        draft: CanonicalItemDraft,
        itemId: String = UUID.randomUUID().toString(),
        mutationId: String = UUID.randomUUID().toString(),
    ): CanonicalAuthoringTransition? = enqueueCanonicalAuthoring(
        itemId = itemId,
        mutationId = mutationId,
        operation = CanonicalAuthoringOperation.CREATE,
        consumeInboxId = inboxId,
    ) { current ->
        val source = current.inbox.firstOrNull { it.id == inboxId }
            ?: throw IllegalArgumentException("Inbox review draft is unavailable")
        PendingCanonicalAuthoringMutation(
            id = mutationId,
            itemId = itemId,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft.copy(
                isSensitive = draft.isSensitive || source.isSensitive,
            ).normalized(),
            createdAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
        )
    }

    fun enqueueCanonicalReplace(
        itemId: String,
        draft: CanonicalItemDraft,
        mutationId: String = UUID.randomUUID().toString(),
    ): CanonicalAuthoringTransition? = enqueueCanonicalAuthoring(
        itemId = itemId,
        mutationId = mutationId,
        operation = CanonicalAuthoringOperation.REPLACE,
    ) { current ->
        val base = current.canonicalItems.firstOrNull { it.id == itemId && it.deletedAt == null }
            ?: throw IllegalArgumentException("Canonical item is not active")
        PendingCanonicalAuthoringMutation(
            id = mutationId,
            itemId = itemId,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = draft.normalized(),
            expectedRevision = base.revision,
            baseItem = base,
            createdAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
        )
    }

    fun enqueueCanonicalTrash(
        itemId: String,
        mutationId: String = UUID.randomUUID().toString(),
    ): CanonicalAuthoringTransition? = enqueueCanonicalAuthoring(
        itemId = itemId,
        mutationId = mutationId,
        operation = CanonicalAuthoringOperation.TRASH,
    ) { current ->
        val base = current.canonicalItems.firstOrNull { it.id == itemId && it.deletedAt == null }
            ?: throw IllegalArgumentException("Canonical item is not active")
        PendingCanonicalAuthoringMutation(
            id = mutationId,
            itemId = itemId,
            operation = CanonicalAuthoringOperation.TRASH,
            expectedRevision = base.revision,
            baseItem = base,
            createdAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
        )
    }

    fun enqueueCanonicalRestore(
        itemId: String,
        mutationId: String = UUID.randomUUID().toString(),
    ): CanonicalAuthoringTransition? = enqueueCanonicalAuthoring(
        itemId = itemId,
        mutationId = mutationId,
        operation = CanonicalAuthoringOperation.RESTORE,
    ) { current ->
        val deleted = current.canonicalRecentlyDeleted.firstOrNull { it.id == itemId }
            ?: throw IllegalArgumentException("Recently-deleted item is unavailable")
        val exactBase = deleted.lastKnownItem?.takeIf {
            it.revision == deleted.revision && it.deletedAt != null
        }
        PendingCanonicalAuthoringMutation(
            id = mutationId,
            itemId = itemId,
            operation = CanonicalAuthoringOperation.RESTORE,
            expectedRevision = deleted.revision,
            baseItem = exactBase,
            createdAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
        )
    }

    /** Replaces an unsent create/replace body without changing its durable request identity. */
    fun updateCanonicalAuthoringDraft(
        id: String,
        draft: CanonicalItemDraft,
    ): CanonicalAuthoringTransition? {
        val durable = mutateDurablyWithSnapshot { current ->
            requireCanonicalAuthoringEnqueueFence(
                current,
                allowDetachedInboxCapture = true,
            )
            val index = current.pendingCanonicalAuthoringMutations.indexOfFirst { it.id == id }
            require(index >= 0) { "Canonical authoring mutation is unavailable" }
            val existing = current.pendingCanonicalAuthoringMutations[index]
            require(
                !existing.isSubmitted &&
                    existing.disposition == CanonicalAuthoringDisposition.PENDING &&
                    existing.operation in setOf(
                        CanonicalAuthoringOperation.CREATE,
                        CanonicalAuthoringOperation.REPLACE,
                    ),
            ) { "Only an unsent create or replacement draft can be edited" }
            val replacement = existing.copy(
                draft = draft.normalized(),
                // A crash can leave a persistently bound request before its submission
                // generation. Editing is safe only because no network byte left; make the
                // next sync bind the changed body again under the active credentials.
                syncOrigin = null,
                configurationId = null,
            ).also(PendingCanonicalAuthoringMutation::requireValid)
            if (current.canonicalExecutionSession != null) {
                require(replacement.isDetachedInboxCapture()) {
                    "Only a detached Inbox capture is editable during active execution"
                }
            }
            validateCanonicalAuthoringCurrentState(current, replacement)
            validateCanonicalAuthoringHierarchy(current, replacement)
            current.copy(
                pendingCanonicalAuthoringMutations = current.pendingCanonicalAuthoringMutations
                    .replaceAt(index, replacement),
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                scheduleMessage = "Canonical draft updated locally",
            )
        } ?: return null
        return durable.canonicalAuthoringTransition(id)
    }

    /**
     * Copies a submitted conflict into a detached, editable Inbox identity while retaining the
     * original recovery record. Inherited sensitivity is promoted before ancestry is removed.
     */
    fun duplicateConflictedCanonicalDraft(
        id: String,
        newItemId: String = UUID.randomUUID().toString(),
        newMutationId: String = UUID.randomUUID().toString(),
    ): CanonicalAuthoringTransition? = enqueueCanonicalAuthoring(
        itemId = newItemId,
        mutationId = newMutationId,
        operation = CanonicalAuthoringOperation.CREATE,
    ) { current ->
        val source = current.pendingCanonicalAuthoringMutations.firstOrNull { it.id == id }
            ?: throw IllegalArgumentException("Canonical authoring conflict is unavailable")
        require(
            source.disposition == CanonicalAuthoringDisposition.CONFLICTED &&
                source.operation in setOf(
                    CanonicalAuthoringOperation.CREATE,
                    CanonicalAuthoringOperation.REPLACE,
                ),
        ) { "Only a conflicted create or replacement can be copied" }
        val sourceDraft = requireNotNull(source.draft)
        val mustRemainSensitive = effectiveCanonicalSensitivity(
            current.canonicalItems,
            source.itemId,
            current.pendingCanonicalMutation,
            current.pendingCanonicalAuthoringMutations,
        )
        PendingCanonicalAuthoringMutation(
            id = newMutationId,
            itemId = newItemId,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = sourceDraft.copy(
                placement = CanonicalDraftPlacement.INBOX,
                isSensitive = sourceDraft.isSensitive || mustRemainSensitive,
                parentId = null,
                siblingOrder = 0,
            ).normalized(),
            createdAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
        )
    }

    /** Adds one reviewable local operation without claiming any server action has started. */
    private fun enqueueCanonicalAuthoring(
        itemId: String,
        mutationId: String,
        operation: CanonicalAuthoringOperation,
        consumeInboxId: String? = null,
        makeMutation: (DayWeaveUiState) -> PendingCanonicalAuthoringMutation,
    ): CanonicalAuthoringTransition? {
        requireCanonicalUuid(itemId, "canonical authoring item")
        requireCanonicalUuid(mutationId, "canonical authoring mutation")
        val durable = mutateDurablyWithSnapshot { current ->
            requireCanonicalAuthoringEnqueueFence(
                current,
                allowDetachedInboxCapture = operation == CanonicalAuthoringOperation.CREATE,
            )
            require(current.pendingCanonicalAuthoringMutations.size < MAX_CANONICAL_AUTHORING_QUEUE)
            require(current.pendingCanonicalAuthoringMutations.none { it.id == mutationId }) {
                "This canonical authoring request identity already exists"
            }
            require(current.pendingCanonicalAuthoringMutations.none { it.itemId == itemId }) {
                "This canonical item already has a queued operation"
            }
            val mutation = makeMutation(current)
            require(mutation.operation == operation && mutation.id == mutationId &&
                mutation.itemId == itemId)
            mutation.requireValid()
            if (current.canonicalExecutionSession != null) {
                require(mutation.isDetachedInboxCapture()) {
                    "Only a detached Inbox capture is available during active execution"
                }
            }
            validateCanonicalAuthoringCurrentState(current, mutation)
            validateCanonicalAuthoringHierarchy(current, mutation)
            current.copy(
                inbox = if (consumeInboxId == null) {
                    current.inbox
                } else {
                    require(current.inbox.any { it.id == consumeInboxId })
                    current.inbox.filterNot { it.id == consumeInboxId }
                },
                pendingCanonicalAuthoringMutations =
                    current.pendingCanonicalAuthoringMutations + mutation,
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                scheduleMessage = "Canonical ${operation.name.lowercase()} saved locally",
            )
        } ?: return null
        return durable.canonicalAuthoringTransition(mutationId)
    }

    /** Binds an unsubmitted draft to the exact active credentials immediately before first send. */
    fun bindCanonicalAuthoringMutation(
        id: String,
        syncOrigin: String,
        configurationId: String?,
    ): CanonicalAuthoringTransition? {
        val durable = mutateDurablyWithSnapshot { current ->
            val index = current.pendingCanonicalAuthoringMutations.indexOfFirst { it.id == id }
            require(index >= 0) { "Canonical authoring mutation is unavailable" }
            val existing = current.pendingCanonicalAuthoringMutations[index]
            require(!existing.isSubmitted && existing.disposition == CanonicalAuthoringDisposition.PENDING)
            require(current.canonicalSyncOrigin == syncOrigin &&
                current.canonicalConfigurationId == configurationId) {
                "Canonical authoring binding does not match the active cache"
            }
            require(existing.syncOrigin == null && existing.configurationId == null ||
                existing.syncOrigin == syncOrigin && existing.configurationId == configurationId) {
                "Canonical authoring mutation is already bound elsewhere"
            }
            val replacement = existing.copy(
                syncOrigin = syncOrigin,
                configurationId = configurationId,
            ).also(PendingCanonicalAuthoringMutation::requireValid)
            current.copy(
                pendingCanonicalAuthoringMutations = current.pendingCanonicalAuthoringMutations
                    .replaceAt(index, replacement),
            )
        } ?: return null
        return durable.canonicalAuthoringTransition(id)
    }

    /** The returned generation must be durable before this exact request can leave the device. */
    fun markCanonicalAuthoringSubmitted(id: String): CanonicalAuthoringTransition? {
        val durable = mutateDurablyWithSnapshot { current ->
            requireCanonicalAuthoringSubmissionFence(current, id)
            val index = current.pendingCanonicalAuthoringMutations.indexOfFirst { it.id == id }
            require(index >= 0)
            val existing = current.pendingCanonicalAuthoringMutations[index]
            require(!existing.isSubmitted && existing.disposition == CanonicalAuthoringDisposition.PENDING)
            require(canonicalAuthoringDependencies(current)[existing.id].orEmpty().isEmpty()) {
                "A dependent canonical parent or child mutation must be confirmed first"
            }
            require(existing.syncOrigin == current.canonicalSyncOrigin &&
                existing.configurationId == current.canonicalConfigurationId &&
                existing.syncOrigin != null)
            validateCanonicalAuthoringCurrentState(current, existing)
            validateCanonicalAuthoringHierarchy(current, existing)
            val replacement = existing.copy(
                submittedAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
            ).also(PendingCanonicalAuthoringMutation::requireValid)
            current.copy(
                pendingCanonicalAuthoringMutations = current.pendingCanonicalAuthoringMutations
                    .replaceAt(index, replacement),
                scheduleMessage = "Canonical change submitted · awaiting authoritative confirmation",
            )
        } ?: return null
        return durable.canonicalAuthoringTransition(id)
    }

    /** Converts one exact rejected request into a reviewable, non-retrying local record. */
    fun markCanonicalAuthoringConflict(
        id: String,
        diagnostic: String,
    ): CanonicalAuthoringTransition? {
        val bounded = diagnostic.trim().take(PendingCanonicalAuthoringMutation.MAX_DIAGNOSTIC_CHARS)
        require(bounded.isNotEmpty())
        val durable = mutateDurablyWithSnapshot { current ->
            val index = current.pendingCanonicalAuthoringMutations.indexOfFirst { it.id == id }
            require(index >= 0)
            val existing = current.pendingCanonicalAuthoringMutations[index]
            require(existing.isSubmitted && existing.disposition == CanonicalAuthoringDisposition.PENDING)
            val replacement = existing.copy(
                disposition = CanonicalAuthoringDisposition.CONFLICTED,
                diagnostic = bounded,
            ).also(PendingCanonicalAuthoringMutation::requireValid)
            val replaced = current.pendingCanonicalAuthoringMutations
                .replaceAt(index, replacement)
            val reconciled = if (
                existing.operation in setOf(
                    CanonicalAuthoringOperation.CREATE,
                    CanonicalAuthoringOperation.RESTORE,
                )
            ) {
                conflictAuthoringDependingOnUnavailableParent(
                    current = current,
                    mutations = replaced,
                    unavailableParentId = existing.itemId,
                    diagnostic = "The queued parent change was rejected; review this draft",
                )
            } else {
                replaced
            }
            current.copy(
                pendingCanonicalAuthoringMutations = reconciled,
                scheduleMessage = "Canonical change needs review",
            )
        } ?: return null
        return durable.canonicalAuthoringTransition(id)
    }

    fun discardCanonicalAuthoringMutation(id: String): PlannerPersistenceReceipt? =
        mutateDurably { current ->
            val existing = current.pendingCanonicalAuthoringMutations.firstOrNull { it.id == id }
                ?: throw IllegalArgumentException("Canonical authoring mutation is unavailable")
            require(!existing.isSubmitted || existing.disposition == CanonicalAuthoringDisposition.CONFLICTED) {
                "An unresolved submitted mutation cannot be discarded"
            }
            val remaining = current.pendingCanonicalAuthoringMutations.filterNot { it.id == id }
            val candidate = current.copy(pendingCanonicalAuthoringMutations = remaining)
            validateCanonicalAuthoringOverlay(candidate)
            candidate
        }

    /** Installs one strictly matched mutation response and clears the same durable journal. */
    fun applyCanonicalAuthoringResponse(
        expected: PendingCanonicalAuthoringMutation,
        response: CanonicalItemSnapshot,
    ): PlannerPersistenceReceipt? {
        expected.requireValid()
        return mutateDurably { current ->
            val index = current.pendingCanonicalAuthoringMutations.indexOfFirst { it.id == expected.id }
            require(index >= 0) { "Canonical authoring fence is unavailable" }
            val durableExpected = current.pendingCanonicalAuthoringMutations[index]
            require(durableExpected.isExactRetentionProjectionOf(expected)) {
                "Canonical authoring fence changed during reconciliation"
            }
            require(durableExpected.isSubmitted &&
                durableExpected.disposition == CanonicalAuthoringDisposition.PENDING)
            require(current.canonicalSyncOrigin == durableExpected.syncOrigin &&
                current.canonicalConfigurationId == durableExpected.configurationId) {
                "Canonical authoring response crossed its API binding"
            }
            validateCanonicalAuthoringResponse(
                expected = durableExpected,
                response = response,
                retainedTrashBase = current.canonicalItems.firstOrNull {
                    it.id == durableExpected.itemId &&
                        it.revision == durableExpected.expectedRevision
                },
            )
            val withoutMutation = current.pendingCanonicalAuthoringMutations.filterNot {
                it.id == durableExpected.id
            }
            val removedBlockIds = current.schedule.asSequence()
                .filter { it.canonicalItemId == durableExpected.itemId }
                .map(ScheduleItem::id)
                .toSet()
            val newerActive = current.canonicalItems.firstOrNull {
                it.id == response.id && it.revision > response.revision
            }
            val newerDeleted = current.canonicalRecentlyDeleted.firstOrNull {
                it.id == response.id && it.revision > response.revision
            }
            val responseIsSuperseded = newerActive != null || newerDeleted != null
            val deletedSensitivity = if (
                durableExpected.operation == CanonicalAuthoringOperation.TRASH
            ) {
                effectiveCanonicalSensitivity(
                    current.canonicalItems,
                    response.id,
                    current.pendingCanonicalMutation,
                    current.pendingCanonicalAuthoringMutations,
                )
            } else {
                response.isSensitive
            }
            val activeItems = if (responseIsSuperseded) {
                current.canonicalItems
            } else when (durableExpected.operation) {
                CanonicalAuthoringOperation.TRASH ->
                    current.canonicalItems.filterNot { it.id == response.id }
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                CanonicalAuthoringOperation.RESTORE,
                -> current.canonicalItems.upsertCanonical(response)
            }
            val deleted = if (responseIsSuperseded) {
                current.canonicalRecentlyDeleted
            } else when (durableExpected.operation) {
                CanonicalAuthoringOperation.TRASH -> current.canonicalRecentlyDeleted
                    .upsertRecentlyDeleted(
                        response.toRecentlyDeletedRecord(deletedSensitivity),
                    )
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                CanonicalAuthoringOperation.RESTORE,
                -> current.canonicalRecentlyDeleted.filterNot { it.id == response.id }
            }
            current.copy(
                canonicalItems = activeItems,
                canonicalRecentlyDeleted = deleted,
                pendingCanonicalAuthoringMutations = withoutMutation,
                schedule = current.schedule.filterNot { it.id in removedBlockIds },
                activeSession = current.activeSession?.takeUnless { it.itemId in removedBlockIds },
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                scheduleMessage = "Canonical ${durableExpected.operation.name.lowercase()} confirmed · recompose required",
            )
        }
    }

    /** Retention may erase only the recovery body; every request and conflict fence stays exact. */
    private fun PendingCanonicalAuthoringMutation.isExactRetentionProjectionOf(
        expected: PendingCanonicalAuthoringMutation,
    ): Boolean = this == expected || (
        expected.operation in setOf(
            CanonicalAuthoringOperation.TRASH,
            CanonicalAuthoringOperation.RESTORE,
        ) && expected.baseItem != null && this == expected.copy(baseItem = null)
        )

    /** Retains a delta tombstone without inventing a full deleted item. */
    fun recordCanonicalRecentlyDeleted(
        record: CanonicalRecentlyDeletedRecord,
    ): PlannerPersistenceReceipt? {
        record.requireValid()
        return mutateDurably { current ->
            val pending = current.pendingCanonicalAuthoringMutations.firstOrNull {
                it.itemId == record.id
            }
            require(pending == null || pending.operation == CanonicalAuthoringOperation.RESTORE) {
                "A non-restore authoring operation must be reconciled before this tombstone"
            }
            if (pending?.expectedRevision?.let { record.revision < it } == true) {
                return@mutateDurably current
            }
            val active = current.canonicalItems.firstOrNull { it.id == record.id }
            if (active != null && active.revision > record.revision) return@mutateDurably current
            val removedBlockIds = current.schedule.asSequence()
                .filter { it.canonicalItemId == record.id }
                .map(ScheduleItem::id)
                .toSet()
            val effectiveSensitivity = record.isSensitive || active?.let {
                effectiveCanonicalSensitivity(
                    current.canonicalItems,
                    it.id,
                    current.pendingCanonicalMutation,
                    current.pendingCanonicalAuthoringMutations,
                )
            } == true
            val retained = record.copy(
                lastKnownItem = record.lastKnownItem ?: active?.takeIf {
                    it.revision < record.revision
                },
                effectiveIsSensitive = effectiveSensitivity,
            ).also(CanonicalRecentlyDeletedRecord::requireValid)
            val restoreReconciledMutations = if (pending == null) {
                current.pendingCanonicalAuthoringMutations
            } else {
                val replacement = when {
                    !pending.isSubmitted -> pending.copy(
                        expectedRevision = retained.revision,
                        baseItem = retained.lastKnownItem?.takeIf {
                            it.revision == retained.revision && it.deletedAt != null
                        } ?: pending.baseItem?.takeIf { retained.revision == pending.expectedRevision },
                    ).also(PendingCanonicalAuthoringMutation::requireValid)
                    record.revision > requireNotNull(pending.expectedRevision) &&
                        pending.disposition == CanonicalAuthoringDisposition.PENDING -> pending.copy(
                            disposition = CanonicalAuthoringDisposition.CONFLICTED,
                            diagnostic = "A newer deletion superseded the submitted restore",
                        ).also(PendingCanonicalAuthoringMutation::requireValid)
                    else -> pending
                }
                current.pendingCanonicalAuthoringMutations.map {
                    if (it.id == replacement.id) replacement else it
                }
            }
            val retainedParentRestore = restoreReconciledMutations.any {
                it.itemId == record.id && it.operation == CanonicalAuthoringOperation.RESTORE &&
                    it.disposition == CanonicalAuthoringDisposition.PENDING
            }
            val updatedMutations = if (retainedParentRestore) {
                restoreReconciledMutations
            } else {
                conflictAuthoringDependingOnUnavailableParent(
                    current = current,
                    mutations = restoreReconciledMutations,
                    unavailableParentId = record.id,
                    diagnostic = "The selected parent was deleted remotely; review this draft",
                )
            }
            val candidate = current.copy(
                canonicalItems = current.canonicalItems.filterNot { it.id == record.id },
                canonicalRecentlyDeleted = current.canonicalRecentlyDeleted
                    .upsertRecentlyDeleted(retained),
                pendingCanonicalAuthoringMutations = updatedMutations,
                schedule = current.schedule.filterNot { it.id in removedBlockIds },
                activeSession = current.activeSession?.takeUnless { it.itemId in removedBlockIds },
                publishedScheduleRevision = null,
                publishedScheduleProof = null,
                scheduleInputDigest = null,
                scheduleMessage = "Canonical deletion retained for restore",
            )
            validateCanonicalAuthoringOverlay(candidate)
            candidate
        }
    }

    private fun requireCanonicalAuthoringEnqueueFence(
        current: DayWeaveUiState,
        allowDetachedInboxCapture: Boolean = false,
    ) {
        require(current.pendingSchedulePublication == null)
        require(current.pendingProposalApplicationMutation == null)
        require(current.pendingCanonicalMutation == null)
        require(current.pendingExecutionCommand == null)
        require(current.pendingExecutionDeferIntent == null)
        require(allowDetachedInboxCapture || current.canonicalExecutionSession == null) {
            "Schedule-affecting authoring is unavailable during an active execution lease"
        }
        require(current.pendingCanonicalAuthoringMutations.none {
            it.isSubmitted && it.disposition == CanonicalAuthoringDisposition.PENDING
        }) { "A submitted canonical authoring change needs reconciliation" }
    }

    private fun DayWeaveUiState.hasUnresolvedCanonicalAuthoring(): Boolean =
        pendingCanonicalAuthoringMutations.any {
            it.isSubmitted && it.disposition == CanonicalAuthoringDisposition.PENDING
        }

    private fun DayWeaveUiState.hasPendingCanonicalAuthoringOverlay(): Boolean =
        pendingCanonicalAuthoringMutations.any {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        }

    private fun DayWeaveUiState.hasExecutionBlockingCanonicalAuthoringOverlay(): Boolean =
        pendingCanonicalAuthoringMutations.any {
            it.disposition == CanonicalAuthoringDisposition.PENDING &&
                !it.isDetachedInboxCapture()
        }

    private fun PendingCanonicalAuthoringMutation.isDetachedInboxCapture(): Boolean =
        !isSubmitted && operation == CanonicalAuthoringOperation.CREATE &&
            draft?.placement == CanonicalDraftPlacement.INBOX && draft.parentId == null

    private fun DayWeaveUiState.hasSubmittedCanonicalAuthoring(): Boolean =
        pendingCanonicalAuthoringMutations.any(PendingCanonicalAuthoringMutation::isSubmitted)

    private fun DayWeaveUiState.hasUnresolvedTerminalProjection(): Boolean = runCatching {
        validatedClosedExecutionOutcomes(terminalExecutionOutcomes).any {
            it.session.status in TERMINAL_EXECUTION_STATUSES &&
                isNewestExecutionForProjection(it.session) &&
                it.requiresCanonicalItemProjection &&
                it.canonicalProjectionRevision == null &&
                it.canonicalProjectionResolution == null &&
                (it.canonicalProjectionConflict == null ||
                    it.canonicalProjectionRetryAuthorizedAt != null)
        }
    }.getOrElse { true }

    private fun requireCanonicalAuthoringSubmissionFence(
        current: DayWeaveUiState,
        id: String,
    ) {
        require(current.pendingSchedulePublication == null)
        require(current.pendingProposalApplicationMutation == null)
        require(current.pendingCanonicalMutation == null)
        require(current.pendingExecutionCommand == null)
        require(current.pendingExecutionDeferIntent == null)
        require(current.canonicalExecutionSession == null)
        require(!current.hasUnresolvedTerminalProjection()) {
            "A terminal execution projection must be resolved before canonical authoring"
        }
        require(current.pendingCanonicalAuthoringMutations.none {
            it.id != id && it.isSubmitted &&
                it.disposition == CanonicalAuthoringDisposition.PENDING
        })
    }

    private fun dependencySortedCanonicalAuthoringMutations(
        current: DayWeaveUiState,
    ): List<PendingCanonicalAuthoringMutation> {
        val remaining = current.pendingCanonicalAuthoringMutations.associateBy { it.id }.toMutableMap()
        val dependencies = canonicalAuthoringDependencies(current)
        val result = mutableListOf<PendingCanonicalAuthoringMutation>()
        val stableOrder = compareBy<PendingCanonicalAuthoringMutation> {
            Instant.parse(it.createdAt)
        }.thenBy { it.id }
        while (remaining.isNotEmpty()) {
            val ready = remaining.values
                .filter { mutation -> dependencies[mutation.id].orEmpty().none(remaining::containsKey) }
                .minWithOrNull(stableOrder)
            requireNotNull(ready) { "Canonical authoring dependencies contain a cycle" }
            result += ready
            remaining.remove(ready.id)
        }
        return result
    }

    private fun canonicalAuthoringDependencies(
        current: DayWeaveUiState,
    ): Map<String, Set<String>> {
        val mutations = current.pendingCanonicalAuthoringMutations
        val byItem = mutations.associateBy(PendingCanonicalAuthoringMutation::itemId)
        val activeById = current.canonicalItems.associateBy(CanonicalItemSnapshot::id)
        return mutations.associate { mutation ->
            val dependencies = mutableSetOf<String>()
            val proposedParentId = when (mutation.operation) {
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                -> mutation.draft?.parentId
                CanonicalAuthoringOperation.RESTORE -> current.canonicalRecentlyDeleted
                    .firstOrNull { it.id == mutation.itemId }
                    ?.parentId
                CanonicalAuthoringOperation.TRASH -> null
            }
            if (mutation.operation != CanonicalAuthoringOperation.TRASH) {
                proposedParentId?.let(byItem::get)?.takeIf {
                    it.operation != CanonicalAuthoringOperation.TRASH
                }?.let { dependencies += it.id }
            } else {
                mutations.asSequence()
                    .filter { child ->
                        val retainedParentId = child.baseItem?.parentId ?: activeById[child.itemId]
                            ?.takeIf { it.revision == child.expectedRevision }
                            ?.parentId
                        child.id != mutation.id && retainedParentId == mutation.itemId &&
                            child.operation in setOf(
                                CanonicalAuthoringOperation.REPLACE,
                                CanonicalAuthoringOperation.TRASH,
                            )
                    }
                    .forEach { dependencies += it.id }
            }
            mutation.id to dependencies
        }
    }

    private fun validateCanonicalAuthoringHierarchy(
        current: DayWeaveUiState,
        candidate: PendingCanonicalAuthoringMutation,
    ) {
        candidate.requireValid()
        val allMutations = current.pendingCanonicalAuthoringMutations
            .filterNot { it.id == candidate.id }
            .plus(candidate)
            .filter { it.disposition == CanonicalAuthoringDisposition.PENDING }
        val activeById = current.canonicalItems.associateBy(CanonicalItemSnapshot::id).toMutableMap()
        val draftById = mutableMapOf<String, CanonicalItemDraft>()
        val restoredStatusById = mutableMapOf<String, String>()
        val parentById = activeById.mapValues { it.value.parentId }.toMutableMap()
        allMutations.forEach { mutation ->
            when (mutation.operation) {
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                -> {
                    val draft = requireNotNull(mutation.draft)
                    draftById[mutation.itemId] = draft
                    parentById[mutation.itemId] = draft.parentId
                }
                CanonicalAuthoringOperation.TRASH -> {
                    activeById.remove(mutation.itemId)
                    parentById.remove(mutation.itemId)
                }
                CanonicalAuthoringOperation.RESTORE -> {
                    val deleted = current.canonicalRecentlyDeleted.firstOrNull {
                        it.id == mutation.itemId
                    } ?: throw IllegalArgumentException("Restore record is unavailable")
                    parentById[mutation.itemId] = deleted.parentId
                    // A bodyless journal can only exist after a body-backed restore already
                    // passed this validator. Retain that eligibility after the privacy cutoff;
                    // the authoritative parent response is validated again before children send.
                    restoredStatusById[mutation.itemId] = deleted.lastKnownItem?.status
                        ?: mutation.baseItem?.status
                        ?: CanonicalDraftPlacement.PLANNED.wireValue
                }
            }
        }
        if (candidate.operation == CanonicalAuthoringOperation.TRASH) {
            require(parentById.values.none { it == candidate.itemId }) {
                "An item with active or queued children cannot be deleted"
            }
        }
        parentById.forEach { (itemId, parentId) ->
            if (parentId == null) return@forEach
            require(parentId in parentById) { "Canonical parent is unavailable" }
            val parentDraft = draftById[parentId]
            val parentStatus = parentDraft?.placement?.wireValue ?: activeById[parentId]?.status
                ?: restoredStatusById[parentId]
            require(parentStatus == "inbox" || parentStatus == "planned") {
                "An executing or terminal item cannot become a parent"
            }
            require(itemId != parentId)
        }
        parentById.keys.forEach { start ->
            val visited = mutableSetOf<String>()
            var currentId: String? = start
            while (currentId != null) {
                require(visited.add(currentId)) { "Canonical hierarchy would contain a cycle" }
                currentId = parentById[currentId]
            }
        }
    }

    private fun validateCanonicalAuthoringOverlay(current: DayWeaveUiState) {
        val overlayMutations = current.pendingCanonicalAuthoringMutations.filter {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        }
        if (overlayMutations.isNotEmpty()) {
            overlayMutations.forEach {
                validateCanonicalAuthoringHierarchy(current, it)
            }
            return
        }
        val parentById = current.canonicalItems.associate { it.id to it.parentId }
        parentById.values.filterNotNull().forEach { parentId ->
            require(parentId in parentById) { "Canonical parent is unavailable" }
        }
        parentById.keys.forEach { start ->
            val visited = mutableSetOf<String>()
            var itemId: String? = start
            while (itemId != null) {
                require(visited.add(itemId)) { "Canonical hierarchy contains a cycle" }
                itemId = parentById[itemId]
            }
        }
    }

    /**
     * A server tombstone wins over local draft ancestry. Keep the exact draft for explicit user
     * recovery, but remove it from the materialized overlay by marking it conflicted. Descendant
     * creates are closed transitively because their locally-created parent is no longer active.
     */
    private fun conflictAuthoringDependingOnUnavailableParent(
        current: DayWeaveUiState,
        mutations: List<PendingCanonicalAuthoringMutation>,
        unavailableParentId: String,
        diagnostic: String,
    ): List<PendingCanonicalAuthoringMutation> {
        val unavailableParentIds = mutableSetOf(unavailableParentId)
        val conflictedIds = mutableSetOf<String>()
        var changed: Boolean
        do {
            changed = false
            mutations.forEach { mutation ->
                val proposedParentId = when (mutation.operation) {
                    CanonicalAuthoringOperation.CREATE,
                    CanonicalAuthoringOperation.REPLACE,
                    -> mutation.draft?.parentId
                    CanonicalAuthoringOperation.RESTORE -> current.canonicalRecentlyDeleted
                        .firstOrNull { it.id == mutation.itemId }
                        ?.parentId
                    CanonicalAuthoringOperation.TRASH -> null
                }
                if (mutation.disposition != CanonicalAuthoringDisposition.PENDING ||
                    mutation.id in conflictedIds ||
                    mutation.operation !in setOf(
                        CanonicalAuthoringOperation.CREATE,
                        CanonicalAuthoringOperation.REPLACE,
                        CanonicalAuthoringOperation.RESTORE,
                    ) || proposedParentId !in unavailableParentIds
                ) {
                    return@forEach
                }
                conflictedIds += mutation.id
                if (mutation.operation in setOf(
                        CanonicalAuthoringOperation.CREATE,
                        CanonicalAuthoringOperation.RESTORE,
                    )
                ) {
                    unavailableParentIds += mutation.itemId
                }
                changed = true
            }
        } while (changed)
        if (conflictedIds.isEmpty()) return mutations
        return mutations.map { mutation ->
            if (mutation.id !in conflictedIds) mutation else mutation.copy(
                disposition = CanonicalAuthoringDisposition.CONFLICTED,
                diagnostic = diagnostic,
            ).also(PendingCanonicalAuthoringMutation::requireValid)
        }
    }

    private fun validateCanonicalAuthoringCurrentState(
        current: DayWeaveUiState,
        mutation: PendingCanonicalAuthoringMutation,
    ) {
        val active = current.canonicalItems.firstOrNull { it.id == mutation.itemId }
        val deleted = current.canonicalRecentlyDeleted.firstOrNull { it.id == mutation.itemId }
        when (mutation.operation) {
            CanonicalAuthoringOperation.CREATE -> require(active == null && deleted == null) {
                "Canonical create identity already exists"
            }
            CanonicalAuthoringOperation.REPLACE,
            -> require(active == mutation.baseItem && deleted == null) {
                "Canonical item changed after this draft was created"
            }
            CanonicalAuthoringOperation.TRASH -> require(
                deleted == null && if (mutation.baseItem != null) {
                    active == mutation.baseItem
                } else {
                    active?.revision == mutation.expectedRevision
                },
            ) { "Canonical item changed after this deletion was queued" }
            CanonicalAuthoringOperation.RESTORE -> require(
                active == null && deleted?.revision == mutation.expectedRevision,
            ) { "Recently-deleted item changed after this restore was queued" }
        }
    }

    private fun validateCanonicalAuthoringResponse(
        expected: PendingCanonicalAuthoringMutation,
        response: CanonicalItemSnapshot,
        retainedTrashBase: CanonicalItemSnapshot? = null,
    ) {
        response.requireCanonicalAuthoringShape()
        require(response.id == expected.itemId)
        val expectedResponseRevision = when (expected.operation) {
            CanonicalAuthoringOperation.CREATE -> 1L
            CanonicalAuthoringOperation.REPLACE,
            CanonicalAuthoringOperation.TRASH,
            CanonicalAuthoringOperation.RESTORE,
            -> Math.addExact(requireNotNull(expected.expectedRevision), 1L)
        }
        require(response.revision == expectedResponseRevision)
        when (expected.operation) {
            CanonicalAuthoringOperation.CREATE,
            CanonicalAuthoringOperation.REPLACE,
            -> require(requireNotNull(expected.draft).matches(response)) {
                "Canonical authoring response does not match the exact draft"
            }
            CanonicalAuthoringOperation.TRASH -> {
                require(response.deletedAt != null)
                // Deletion requests contain only identity, expected revision, and the durable
                // idempotency key. Keep the stronger authored-field check while the short-lived
                // recovery body is available, but never make exact response reconciliation
                // depend on retaining plaintext beyond the privacy boundary.
                (expected.baseItem ?: retainedTrashBase)?.let { exactBase ->
                    require(exactBase.id == expected.itemId &&
                        exactBase.revision == expected.expectedRevision &&
                        exactBase.deletedAt == null)
                    require(response.sameAuthoredFields(exactBase))
                }
            }
            CanonicalAuthoringOperation.RESTORE -> {
                require(response.deletedAt == null)
                expected.baseItem?.let { require(response.sameAuthoredFields(it)) }
            }
        }
        if (expected.operation != CanonicalAuthoringOperation.TRASH) {
            require(response.deletedAt == null)
        }
    }

    private fun CanonicalItemSnapshot.sameAuthoredFields(other: CanonicalItemSnapshot): Boolean =
        id == other.id && isSensitive == other.isSensitive && kind == other.kind &&
            status == other.status && title == other.title && notes == other.notes &&
            timezoneName == other.timezoneName && durationSeconds == other.durationSeconds &&
            deadlineAt.sameInstant(other.deadlineAt) &&
            earliestStartAt.sameInstant(other.earliestStartAt) &&
            recurrenceJson == other.recurrenceJson &&
            flexibleConstraintsJson == other.flexibleConstraintsJson &&
            splitPolicyJson == other.splitPolicyJson && importance == other.importance &&
            urgency == other.urgency && parentId == other.parentId &&
            siblingOrder == other.siblingOrder

    private fun String?.sameInstant(other: String?): Boolean = when {
        this == null || other == null -> this == other
        else -> Instant.parse(this) == Instant.parse(other)
    }

    private fun CanonicalItemSnapshot.toRecentlyDeletedRecord(
        effectiveIsSensitive: Boolean,
    ) =
        CanonicalRecentlyDeletedRecord(
            id = id,
            revision = revision,
            deletedAt = requireNotNull(deletedAt),
            parentId = parentId,
            lastKnownItem = this,
            effectiveIsSensitive = effectiveIsSensitive,
            retentionAnchorAt = minOf(
                Instant.parse(requireNotNull(deletedAt)),
                Instant.ofEpochMilli(nowEpochMillis()),
            ).toString(),
        ).also(CanonicalRecentlyDeletedRecord::requireValid)

    private fun List<CanonicalItemSnapshot>.upsertCanonical(
        item: CanonicalItemSnapshot,
    ): List<CanonicalItemSnapshot> =
        (filterNot { it.id == item.id } + item).sortedWith(
            compareBy<CanonicalItemSnapshot> { it.siblingOrder }.thenBy { it.id },
        )

    private fun List<CanonicalRecentlyDeletedRecord>.upsertRecentlyDeleted(
        record: CanonicalRecentlyDeletedRecord,
    ): List<CanonicalRecentlyDeletedRecord> {
        val previous = firstOrNull { it.id == record.id }
        if (previous != null && previous.revision > record.revision) return this
        val retained = if (
            previous?.revision == record.revision && record.lastKnownItem == null &&
            previous.lastKnownItem != null
        ) {
            record.copy(lastKnownItem = previous.lastKnownItem)
        } else {
            record
        }
        val privacyRetained = if (previous?.isSensitive == true && !retained.isSensitive) {
            retained.copy(effectiveIsSensitive = true)
        } else {
            retained
        }
        val earliestAnchor = listOfNotNull(
            previous?.retentionAnchorAt?.let(Instant::parse),
            privacyRetained.retentionAnchorAt?.let(Instant::parse),
        ).minOrNull()
        val anchored = if (earliestAnchor == null) privacyRetained else privacyRetained.copy(
            retentionAnchorAt = earliestAnchor.toString(),
        )
        return filterNot { it.id == record.id } + anchored
    }

    private fun <T> List<T>.replaceAt(index: Int, replacement: T): List<T> =
        mapIndexed { currentIndex, value -> if (currentIndex == index) replacement else value }

    /** Persists the idempotency fence before a canonical mutation can leave the device. */
    fun stageCanonicalMutation(
        mutation: PendingCanonicalMutation,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingSchedulePublication == null) {
            "A schedule publication must be reconciled before a canonical mutation"
        }
        require(current.pendingCanonicalMutation == null) {
            "A canonical mutation already needs reconciliation"
        }
        require(current.pendingExecutionCommand == null) {
            "An execution command already needs reconciliation"
        }
        require(current.pendingExecutionDeferIntent == null) {
            "A move-later intent already needs reconciliation"
        }
        require(current.pendingProposalApplicationMutation == null) {
            "A proposal application already needs reconciliation"
        }
        require(!current.hasExecutionBlockingCanonicalAuthoringOverlay()) {
            "A canonical authoring change already needs reconciliation"
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
        val cachedItem = current.canonicalItems.firstOrNull { it.id == mutation.itemId }
            ?: throw IllegalArgumentException("Canonical item is not cached")
        require(cachedItem.revision == mutation.expectedRevision)
        if (cachedItem.isSensitive != mutation.targetIsSensitive) {
            require(
                mutation.targetStatus == cachedItem.status &&
                    mutation.focusedBlockId == mutation.itemId &&
                    mutation.pauseLabel == null &&
                    mutation.pauseMinutes == null &&
                    mutation.terminalExecutionSessionId == null
            ) { "Sensitivity replacement must not change execution state" }
        }
        mutation.terminalExecutionSessionId?.let { sessionId ->
            require(UUID.fromString(sessionId).toString() == sessionId)
            val outcome = current.terminalExecutionOutcomes[sessionId]
                ?: throw IllegalArgumentException("Terminal execution outcome is unavailable")
            validateClosedExecutionOutcome(outcome)
            require(
                outcome.session.status in TERMINAL_EXECUTION_STATUSES &&
                    current.isNewestExecutionForProjection(outcome.session) &&
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
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
            scheduleInputDigest = null,
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

    /** A confirmed closed row or unresolved parent projection is a durable start fence. */
    fun isCanonicalExecutionStartBlocked(blockId: String): Boolean {
        val current = state.value
        val block = current.schedule.firstOrNull { it.id == blockId } ?: return true
        if (block.status != ItemStatus.SCHEDULED) return true
        if (!current.hasPublishedExecutionAuthority(block)) return true
        val itemId = block.canonicalItemId ?: return true
        val origin = current.canonicalSyncOrigin ?: return true
        if (
            current.canonicalExecutionSyncOrigin != origin ||
            current.canonicalExecutionConfigurationId != current.canonicalConfigurationId ||
            !current.canonicalExecutionHistoryVerified
        ) {
            return true
        }
        val validated = runCatching {
            validatedClosedExecutionOutcomes(current.terminalExecutionOutcomes)
        }.getOrElse { return true }
        if (validated.isEmpty()) return false
        val outcomes = validated
            .filter { it.syncOrigin == origin }
        return outcomes.any { outcome ->
            outcome.session.status == "deferred" && outcome.session.matches(block) ||
                outcome.session.status in TERMINAL_EXECUTION_STATUSES &&
                !outcome.userKeptLatestItem() && outcome.session.matches(block)
        } || outcomes.any { outcome ->
            outcome.session.status in TERMINAL_EXECUTION_STATUSES &&
                current.isNewestExecutionForProjection(outcome.session) &&
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
        require(current.isNewestExecutionForProjection(outcome.session)) {
            "A newer execution session supersedes this terminal projection"
        }
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
        require(current.isNewestExecutionForProjection(outcome.session)) {
            "A newer execution session supersedes this terminal projection"
        }
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
        require(current.isNewestExecutionForProjection(outcome.session)) {
            "A newer execution session supersedes this terminal projection"
        }
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

    /** Persists the user's exact move target before Pause can cross the network boundary. */
    fun stageExecutionDeferIntent(
        intent: PendingExecutionDeferIntent,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingExecutionDeferIntent == null) {
            "A move-later intent already needs reconciliation"
        }
        require(current.pendingSchedulePublication == null) {
            "A schedule publication must be reconciled before moving later"
        }
        require(current.pendingCanonicalMutation == null) {
            "A canonical mutation must be reconciled before moving later"
        }
        require(current.pendingProposalApplicationMutation == null) {
            "A proposal application must be reconciled before moving later"
        }
        require(!current.hasExecutionBlockingCanonicalAuthoringOverlay()) {
            "A canonical authoring change must be reconciled before moving later"
        }
        require(
            intent.syncOrigin == current.canonicalExecutionSyncOrigin &&
                intent.configurationId == current.canonicalExecutionConfigurationId,
        ) { "Move-later intent does not match the execution binding" }
        require(intent.schemaVersion == EXECUTION_DEFER_INTENT_SCHEMA_VERSION) {
            "Unsupported move-later intent schema"
        }
        require(intent.assessment == null && intent.approvedAssessmentDigest == null) {
            "A new move target cannot carry prior assessment authority"
        }
        require(
            intent.approvedConflictTargetEnd == null && intent.approvedDeadlineRisks.isEmpty() &&
                !intent.approvedSourceOverride && intent.approvedItemRevisions.isEmpty() &&
                intent.approvedHardBlockIds.isEmpty() && intent.approvedHardConflicts.isEmpty(),
        ) { "Legacy local move approvals cannot authorize execution" }
        listOf(
            intent.sessionId,
            intent.itemId,
            intent.plannedBlockId,
            intent.sourceDeviceId,
            intent.focusedBlockId,
        ).forEach { raw -> require(UUID.fromString(raw).toString() == raw) }
        intent.occurrenceId?.let { require(UUID.fromString(it).toString() == it) }
        require(intent.itemRevision > 0 && intent.sessionIndex in 0..UShort.MAX_VALUE.toInt())
        val sourceStart = Instant.parse(intent.sourceStart)
        val sourceEnd = Instant.parse(intent.sourceEnd)
        val moveStart = Instant.parse(intent.moveStart)
        val stagedAt = Instant.parse(intent.stagedAt)
        val sourceDuration = Duration.between(sourceStart, sourceEnd)
        require(
            sourceStart < sourceEnd && sourceDuration.nano == 0 &&
                moveStart > stagedAt && moveStart.nano == 0,
        ) { "Move-later intent does not contain exact future bounds" }
        val authoritative = current.canonicalExecutionSession
            ?: throw IllegalArgumentException("The authoritative execution lease is unavailable")
        require(authoritative.status in OPEN_EXECUTION_STATUSES)
        require(intent.hasSameImmutableIdentity(authoritative)) {
            "Move-later intent does not match the authoritative lease"
        }
        current.pendingExecutionCommand?.let { command ->
            require(command.commandType in setOf("pause", "defer")) {
                "A different execution command must be reconciled before moving later"
            }
            require(intent.hasSameImmutableIdentity(command)) {
                "The pending execution command belongs to a different lease"
            }
        }
        val focused = current.schedule.firstOrNull { it.id == intent.focusedBlockId }
            ?: throw IllegalArgumentException("The execution source block is unavailable")
        require(
            intent.focusedBlockId == intent.plannedBlockId &&
                focused.canonicalItemId == intent.itemId &&
                focused.canonicalRevision == intent.itemRevision &&
                focused.occurrenceId == intent.occurrenceId &&
                focused.sessionIndex == intent.sessionIndex &&
                focused.absoluteStartAt == intent.sourceStart &&
                focused.absoluteEndAt == intent.sourceEnd,
        ) { "Move-later intent does not match the exact source block" }
        require(current.hasPublishedExecutionAuthority(focused)) {
            "The execution source has no current immutable publication seal"
        }
        val proof = current.publishedScheduleProof?.blocks?.singleOrNull {
            it.id == intent.plannedBlockId
        } ?: throw IllegalArgumentException("The execution source has no publication proof")
        require(
            proof.itemId == intent.itemId && proof.itemRevision == intent.itemRevision &&
                proof.occurrenceId == intent.occurrenceId &&
                proof.sessionIndex == intent.sessionIndex && proof.start == intent.sourceStart &&
                proof.end == intent.sourceEnd,
        ) { "Move-later intent does not match its publication proof" }
        current.copy(
            pendingExecutionDeferIntent = intent,
            scheduleMessage = "Move target saved · confirming an exact pause before assessment",
        )
    }

    /** Persists one exact server assessment and revokes every approval from older evidence. */
    fun recordExecutionDeferAssessment(
        sessionId: String,
        assessment: ExecutionDeferAssessmentSnapshot,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val intent = current.pendingExecutionDeferIntent
            ?: throw IllegalArgumentException("No move-later intent is pending")
        require(intent.sessionId == sessionId) { "A different move-later intent is pending" }
        current.requireCurrentExecutionDeferAssessment(intent, assessment, requireFresh = true)
        current.copy(
            pendingExecutionDeferIntent = intent.copy(
                assessment = assessment,
                // A replacement response never inherits approval, even if another assessment had
                // the same target or superficially identical conflict messages.
                approvedAssessmentDigest = null,
            ),
            scheduleMessage = if (assessment.approvalRequired) {
                "Move assessed · review the content-free placement warnings"
            } else {
                "Move assessed safely · preparing the exact deferred placement"
            },
        )
    }

    /** Persists explicit approval for exactly one current assessment digest. */
    fun approveExecutionDeferAssessment(
        sessionId: String,
        assessmentDigest: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val intent = current.pendingExecutionDeferIntent
            ?: throw IllegalArgumentException("No move-later intent is pending")
        require(intent.sessionId == sessionId) { "A different move-later intent is pending" }
        val assessment = intent.assessment
            ?: throw IllegalArgumentException("The authoritative move assessment is unavailable")
        current.requireCurrentExecutionDeferAssessment(intent, assessment, requireFresh = true)
        require(assessment.approvalRequired && assessment.violations.isNotEmpty()) {
            "This move does not require approval"
        }
        require(assessmentDigest == assessment.assessmentDigest) {
            "Move approval does not match the current assessment"
        }
        current.copy(
            pendingExecutionDeferIntent = intent.copy(
                approvedAssessmentDigest = assessmentDigest,
            ),
            scheduleMessage = "Move warning approved · saving the exact defer command",
        )
    }

    /** Keeps the paused lease and chosen target while discarding stale authorization evidence. */
    fun clearExecutionDeferAssessment(
        sessionId: String,
        message: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val intent = current.pendingExecutionDeferIntent
            ?: throw IllegalArgumentException("No move-later intent is pending")
        require(intent.sessionId == sessionId) { "A different move-later intent is pending" }
        current.copy(
            pendingExecutionDeferIntent = intent.copy(
                assessment = null,
                approvedAssessmentDigest = null,
            ),
            scheduleMessage = message,
        )
    }

    fun clearExecutionDeferIntent(
        sessionId: String,
        message: String,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val intent = current.pendingExecutionDeferIntent
            ?: throw IllegalArgumentException("No move-later intent is pending")
        require(intent.sessionId == sessionId) { "A different move-later intent is pending" }
        current.copy(
            pendingExecutionDeferIntent = null,
            scheduleMessage = message,
        )
    }

    /** Persists the exact command and idempotency key before execution network I/O. */
    fun stageExecutionCommand(
        command: PendingExecutionCommand,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        require(current.pendingSchedulePublication == null) {
            "A schedule publication must be reconciled before an execution command"
        }
        require(current.pendingExecutionCommand == null) {
            "An execution command already needs reconciliation"
        }
        require(current.pendingCanonicalMutation == null) {
            "A legacy canonical mutation already needs reconciliation"
        }
        require(current.pendingProposalApplicationMutation == null) {
            "A proposal application already needs reconciliation"
        }
        require(!current.hasExecutionBlockingCanonicalAuthoringOverlay()) {
            "A canonical authoring change already needs reconciliation"
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
        require(
            command.commandType != "defer" || current.pendingExecutionDeferIntent != null,
        ) { "A new Defer command requires a durable authoritative assessment intent" }
        current.pendingExecutionDeferIntent?.let { intent ->
            require(command.commandType in setOf("pause", "defer")) {
                "Only the pending move-later transition can change this lease"
            }
            require(intent.hasSameImmutableIdentity(command)) {
                "Execution command does not match the pending move-later intent"
            }
            if (command.commandType == "defer") {
                val assessment = intent.assessment
                    ?: throw IllegalArgumentException(
                        "An authoritative move assessment is required before Defer",
                    )
                current.requireCurrentExecutionDeferAssessment(
                    intent,
                    assessment,
                    requireFresh = true,
                )
                require(
                    if (assessment.approvalRequired) {
                        intent.approvedAssessmentDigest == assessment.assessmentDigest
                    } else {
                        intent.approvedAssessmentDigest == null
                    },
                ) { "Move approval does not match the exact authoritative assessment" }
                require(command.matchesExecutionDeferAssessment(assessment)) {
                    "The exact Defer command does not match its authoritative assessment"
                }
            }
        }
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
            require(focused.status == ItemStatus.SCHEDULED) {
                "Only a scheduled canonical block can start"
            }
            require(current.hasPublishedExecutionAuthority(focused)) {
                "The focused block has no durable exact publication authority"
            }
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
        val preservableAuthoringIds = current.preservableUnboundCanonicalCreates()
            .mapTo(hashSetOf(), PendingCanonicalAuthoringMutation::id)
        return current.pendingCanonicalMutation != null ||
            current.pendingSchedulePublication != null ||
            current.pendingExecutionCommand != null ||
            current.pendingExecutionDeferIntent != null ||
            current.pendingProposalApplicationMutation != null ||
            current.pendingGoogleCalendarOutbound != null ||
            current.pendingCanonicalAuthoringMutations.any {
                it.id !in preservableAuthoringIds
            } ||
            current.hasUnresolvedTerminalProjection()
    }

    /**
     * Compare-and-saves one exact outbound recovery stage before its consequential network call.
     * A fresh intent must still name the current synced fixed event; later responses may only
     * advance the immutable saved intent through the model's audited state machine.
     */
    suspend fun replaceGoogleCalendarOutboundJournal(
        expected: GoogleCalendarOutboundJournal?,
        replacement: GoogleCalendarOutboundJournal,
    ): Boolean {
        val receipt = try {
            mutateDurably { current ->
                require(current.pendingGoogleCalendarOutbound == expected) {
                    "Google Calendar outbound recovery changed"
                }
                if (expected == null) {
                    require(replacement.stage == GoogleCalendarOutboundStage.INTENT)
                    require(
                        current.canonicalSyncOrigin == replacement.apiBaseUrl &&
                            current.canonicalConfigurationId == replacement.configurationId,
                    ) { "Google Calendar outbound recovery crosses the canonical binding" }
                    val candidate = current.googleCalendarOutboundCandidate(replacement.itemId)
                    require(
                        candidate?.expectedItemRevision == replacement.expectedItemRevision &&
                            candidate.entityKind == replacement.entityKind &&
                            candidate.operation == replacement.operation,
                    ) { "Google Calendar outbound item is no longer publishable" }
                    require(
                        current.pendingSchedulePublication == null &&
                            current.pendingProposalApplicationMutation == null &&
                            current.pendingCanonicalMutation == null &&
                            current.pendingExecutionCommand == null &&
                            current.pendingExecutionDeferIntent == null,
                    ) { "Another canonical write must finish before Google publication" }
                } else {
                    require(expected.canTransitionTo(replacement)) {
                        "Google Calendar outbound recovery transition is invalid"
                    }
                    if (
                        replacement.stage == GoogleCalendarOutboundStage.PREVIEWED ||
                        replacement.stage == GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED
                    ) {
                        val candidate = current.googleCalendarOutboundCandidate(replacement.itemId)
                        require(
                            candidate?.expectedItemRevision == replacement.expectedItemRevision &&
                                candidate.entityKind == replacement.entityKind &&
                                candidate.operation == replacement.operation,
                        ) { "Google Calendar outbound item changed before approval" }
                    }
                }
                current.copy(pendingGoogleCalendarOutbound = replacement)
            }
        } catch (_: IllegalArgumentException) {
            null
        } catch (_: IllegalStateException) {
            null
        } ?: return false
        return receipt.awaitDurable() &&
            durableState.value?.pendingGoogleCalendarOutbound == replacement
    }

    /** Clears only an exact approved journal after a strictly validated durable-outbox 202. */
    suspend fun clearGoogleCalendarOutboundAfterAcceptance(
        expected: GoogleCalendarOutboundJournal,
    ): Boolean {
        if (expected.stage != GoogleCalendarOutboundStage.APPROVED) return false
        val receipt = try {
            mutateDurably { current ->
                require(current.pendingGoogleCalendarOutbound == expected) {
                    "Google Calendar outbound recovery changed"
                }
                current.copy(pendingGoogleCalendarOutbound = null)
            }
        } catch (_: IllegalArgumentException) {
            null
        } catch (_: IllegalStateException) {
            null
        } ?: return false
        return receipt.awaitDurable() && durableState.value?.pendingGoogleCalendarOutbound == null
    }

    /** User-authorized escape hatch after every possible server authority has safely expired. */
    suspend fun discardExpiredGoogleCalendarOutbound(
        expected: GoogleCalendarOutboundJournal,
        now: Instant,
    ): Boolean {
        if (!expected.canDiscardExpiredAt(now)) return false
        val receipt = try {
            mutateDurably { current ->
                require(current.pendingGoogleCalendarOutbound == expected) {
                    "Google Calendar outbound recovery changed"
                }
                current.copy(pendingGoogleCalendarOutbound = null)
            }
        } catch (_: IllegalArgumentException) {
            null
        } catch (_: IllegalStateException) {
            null
        } ?: return false
        return receipt.awaitDurable() && durableState.value?.pendingGoogleCalendarOutbound == null
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
                    if (prior.status in CLOSED_EXECUTION_STATUSES) {
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
        changedSessionControlsPresentation: Boolean = true,
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
            current.terminalExecutionOutcomes.isNotEmpty() ||
            current.pendingExecutionDeferIntent != null
        ) {
            require(
                current.pendingExecutionCommand == null &&
                    current.pendingExecutionDeferIntent == null &&
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
        var closedExecutionOutcomes = if (sameBinding) {
            validatedClosedExecutionOutcomes(current.terminalExecutionOutcomes)
                .filter { it.syncOrigin == syncOrigin }
                .associateBy { it.session.id }
        } else {
            emptyMap()
        }
        if (authoritativeChangedSession?.status in CLOSED_EXECUTION_STATUSES) {
            val changed = requireNotNull(authoritativeChangedSession)
            val isCanonicalTerminal = changed.status in TERMINAL_EXECUTION_STATUSES
            val focused = matchingBlock(changed, schedule)
            val existingOutcome = closedExecutionOutcomes[changed.id]
            existingOutcome?.let { existing ->
                require(existing.session.hasSameRemoteSemantics(changed)) {
                    "A confirmed closed execution row was mutated by the server"
                }
            }
            val immutableClosedSession = existingOutcome?.session ?: changed
            val outcome = TerminalExecutionOutcomeSnapshot(
                syncOrigin = syncOrigin,
                session = immutableClosedSession,
                requiresCanonicalItemProjection = isCanonicalTerminal && (
                    existingOutcome?.requiresCanonicalItemProjection == true ||
                        immutableClosedSession.canonicalProjectionEligibleAtLeaseStart == true
                    ),
                canonicalProjectionRevision = existingOutcome
                    ?.canonicalProjectionRevision?.takeIf { isCanonicalTerminal },
                canonicalProjectionResolution = existingOutcome
                    ?.canonicalProjectionResolution?.takeIf { isCanonicalTerminal },
                canonicalProjectionConflict = existingOutcome
                    ?.canonicalProjectionConflict?.takeIf { isCanonicalTerminal },
                canonicalProjectionRetryAuthorizedAt = existingOutcome
                    ?.canonicalProjectionRetryAuthorizedAt?.takeIf { isCanonicalTerminal },
                recordedAt = existingOutcome?.recordedAt ?:
                    immutableClosedSession.endedAt ?: immutableClosedSession.updatedAt,
            )
            closedExecutionOutcomes = retainedClosedExecutionOutcomes(
                closedExecutionOutcomes + (changed.id to outcome),
            )
            if (changed.status == "deferred") {
                focused?.let { deferredBlock ->
                    schedule = schedule.map { block ->
                        if (block.id == deferredBlock.id) {
                            block.copy(status = ItemStatus.SCHEDULED, actualMinutes = null)
                        } else {
                            block
                        }
                    }
                }
                changed.occurrenceId?.let { occurrenceId ->
                    val staleOutcome = recurrenceOutcomes[occurrenceId]
                    recurrenceOutcomes = recurrenceOutcomes - occurrenceId
                    val owner = current.occurrenceSeriesItemIds[occurrenceId]
                    if (
                        owner != null && staleOutcome?.status == ItemStatus.COMPLETED &&
                        completionAnchors[owner] == staleOutcome.resolvedAt
                    ) {
                        val previousCompletion = recurrenceOutcomes.values.asSequence()
                            .filter {
                                it.itemId == owner && it.status == ItemStatus.COMPLETED
                            }
                            .maxByOrNull { Instant.parse(it.resolvedAt) }
                        completionAnchors = if (previousCompletion == null) {
                            completionAnchors - owner
                        } else {
                            completionAnchors + (owner to previousCompletion.resolvedAt)
                        }
                    }
                }
            }
            if (
                isCanonicalTerminal && changedSessionControlsPresentation && focused != null &&
                !outcome.userKeptLatestItem()
            ) {
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
        val recurrenceChanged = recurrenceOutcomes != current.recurrenceOutcomes ||
            recurrenceMoves != current.recurrenceMoves ||
            completionAnchors != current.recurrenceCompletionAnchors
        val executionDeferred = authoritativeChangedSession?.status == "deferred"
        current.copy(
            schedule = schedule,
            activeSession = localActiveSession,
            canonicalExecutionSyncOrigin = syncOrigin,
            canonicalExecutionConfigurationId = configurationId,
            canonicalExecutionRevision = revision,
            canonicalExecutionSession = authoritativeActiveSession,
            terminalExecutionOutcomes = closedExecutionOutcomes,
            pendingExecutionCommand = if (clearPendingIdempotencyKey != null) {
                null
            } else {
                current.pendingExecutionCommand
            },
            pendingExecutionDeferIntent = current.pendingExecutionDeferIntent?.takeUnless { intent ->
                authoritativeChangedSession?.let { changed ->
                    changed.id == intent.sessionId && changed.status in CLOSED_EXECUTION_STATUSES
                } == true
            },
            recurrenceOutcomes = recurrenceOutcomes,
            recurrenceMoves = recurrenceMoves,
            recurrenceCompletionAnchors = completionAnchors,
            publishedScheduleRevision = current.publishedScheduleRevision
                .takeUnless { recurrenceChanged || executionDeferred },
            publishedScheduleProof = current.publishedScheduleProof
                .takeUnless { recurrenceChanged || executionDeferred },
            scheduleInputDigest = current.scheduleInputDigest
                .takeUnless { recurrenceChanged || executionDeferred },
            scheduleMessage = when {
                executionDeferred ->
                    "Move saved · publishing the exact remaining-work placement"
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
        require(!current.hasSubmittedCanonicalAuthoring()) {
            "Every submitted canonical authoring write must be explicitly resolved before disconnecting"
        }
        val canonicalBlockIds = current.schedule.asSequence()
            .filter { it.canonicalItemId != null }
            .map(ScheduleItem::id)
            .toSet()
        val preservedCreates = current.preservableUnboundCanonicalCreates()
        current.copy(
            suggestions = current.suggestions.filter { it.remoteRevision == null },
            inbox = current.inbox.filter { it.source != InboxSource.EXTERNAL_PROPOSAL },
            canonicalItems = emptyList(),
            pendingCanonicalAuthoringMutations = preservedCreates,
            canonicalRecentlyDeleted = emptyList(),
            canonicalSyncOrigin = null,
            canonicalConfigurationId = null,
            canonicalDeltaCursor = null,
            pendingSchedulePublication = null,
            pendingGoogleCalendarOutbound = null,
            pendingProposalApplicationMutation = null,
            proposalApplications = emptyMap(),
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
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
            pendingExecutionDeferIntent = null,
            lastBreakEndNotificationAttemptDigest = null,
            lastConsumedBreakEndNotificationDigest = null,
            lastRejectedBreakEndNotificationDigest = null,
            acknowledgedBreakEndDigest = null,
            unscheduledWork = emptyList(),
            occurrenceSeriesItemIds = emptyMap(),
            recurrenceOccurrenceSources = emptyMap(),
            rejectedCanonicalItemCount = 0,
            unscheduledCanonicalItemCount = 0,
            scheduleViolationMessages = emptyList(),
            scheduleViolationCount = 0,
            scheduleErrorViolationCount = 0,
            scheduleMessage =
                "API connection forgotten locally · any in-flight server action has unknown outcome",
        )
    }

    /** Captures with no server dependency can safely survive a change of remote identity. */
    private fun DayWeaveUiState.preservableUnboundCanonicalCreates(): List<PendingCanonicalAuthoringMutation> {
        var preservedCreates = pendingCanonicalAuthoringMutations.filter {
            it.operation == CanonicalAuthoringOperation.CREATE && !it.isSubmitted &&
                it.syncOrigin == null && it.disposition == CanonicalAuthoringDisposition.PENDING
        }
        while (true) {
            val preservedIds = preservedCreates.mapTo(mutableSetOf()) { it.itemId }
            val selfContained = preservedCreates.filter { mutation ->
                mutation.draft?.parentId?.let { it in preservedIds } != false
            }
            if (selfContained.size == preservedCreates.size) return preservedCreates
            preservedCreates = selfContained
        }
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
            validateClosedExecutionOutcome(outcome)
            require(
                outcome.session.status in TERMINAL_EXECUTION_STATUSES &&
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
                    pending.targetStatus == item.status &&
                    pending.targetIsSensitive == item.isSensitive,
            ) { "Canonical mutation response does not match the durable uncertainty fence" }
        }
        val terminalExecutionOutcomes = pendingMutation?.terminalExecutionSessionId?.let { sessionId ->
            val outcome = current.terminalExecutionOutcomes[sessionId]
                ?: throw IllegalArgumentException("Terminal execution projection is unavailable")
            validateClosedExecutionOutcome(outcome)
            require(
                outcome.session.status in TERMINAL_EXECUTION_STATUSES &&
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
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
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

    /**
     * Durably applies only an acknowledged own-sensitivity replacement. Placement and execution
     * state stay intact; effective sensitivity is recomputed for every scheduled descendant so an
     * ancestor promotion cannot briefly expose a child through the local cache.
     */
    fun reconcileCanonicalItemSensitivity(
        item: CanonicalItemSnapshot,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
        val pending = current.pendingCanonicalMutation
            ?: throw IllegalArgumentException("No canonical mutation is pending")
        val previous = current.canonicalItems.firstOrNull { it.id == item.id }
            ?: throw IllegalArgumentException("Canonical item is not cached")
        require(
            pending.itemId == item.id &&
                pending.expectedRevision == previous.revision &&
                item.revision > previous.revision &&
                pending.targetStatus == item.status &&
                pending.targetIsSensitive == item.isSensitive &&
                previous.status == item.status &&
                previous.isSensitive != item.isSensitive &&
                pending.terminalExecutionSessionId == null &&
                item.deletedAt == null
        ) { "Sensitivity response does not match the durable uncertainty fence" }

        val updatedItems = current.canonicalItems.map { existing ->
            if (existing.id == item.id) item else existing
        }
        val updatedSchedule = current.schedule.map { block ->
            val canonicalId = block.canonicalItemId ?: return@map block
            block.copy(
                isSensitive = effectiveSensitivity(updatedItems, canonicalId),
                canonicalRevision = if (canonicalId == item.id) {
                    item.revision
                } else {
                    block.canonicalRevision
                },
            )
        }
        current.copy(
            canonicalItems = updatedItems,
            schedule = updatedSchedule,
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
            scheduleInputDigest = null,
            pendingCanonicalMutation = null,
            scheduleMessage = if (item.isSensitive) {
                "Marked sensitive · descendants now inherit this protection"
            } else if (item.parentId?.let { parentId ->
                    effectiveSensitivity(updatedItems, parentId)
                } == true
            ) {
                "Own sensitive label removed · parent protection still applies"
            } else {
                "Sensitive label removed after confirmation"
            },
        )
    }

    /**
     * Legacy synchronous seam retained for isolated store callers.
     *
     * It records only the user's message. A provider reply may be appended solely through the
     * durable assistant-turn boundary below; the store never fabricates an AI response.
     */
    fun sendAssistantMessage(text: String): Boolean {
        val trimmed = text.trim()
        if (!isValidAssistantText(trimmed, MAX_ASSISTANT_USER_MESSAGE_BYTES)) return false
        val userMessageId = UUID.randomUUID().toString()
        return mutate { current ->
            current.copy(
                messages = appendBoundedAssistantMessage(
                    current.messages,
                    ChatMessage(userMessageId, ChatRole.USER, trimmed),
                ),
            )
        }
    }

    /** Encrypts the exact user turn before any paid or privacy-sensitive provider request begins. */
    fun appendAssistantUserMessageDurably(
        messageId: String,
        text: String,
    ): PlannerPersistenceReceipt? {
        val trimmed = text.trim()
        requireValidAssistantMessageId(messageId)
        require(isValidAssistantText(trimmed, MAX_ASSISTANT_USER_MESSAGE_BYTES)) {
            "Assistant user message is empty or exceeds the byte limit"
        }
        return mutateDurably { current ->
            require(current.messages.none { it.id == messageId }) {
                "Assistant message identity already exists"
            }
            current.copy(
                messages = appendBoundedAssistantMessage(
                    current.messages,
                    ChatMessage(messageId, ChatRole.USER, trimmed),
                ),
            )
        }
    }

    /**
     * Stores only a completed, request-bound provider reply.
     *
     * Network orchestration owns privacy/configuration generation checks. Requiring the durable
     * user anchor here prevents an orphaned or replayed provider result from entering the transcript.
     */
    fun appendAssistantReplyDurably(
        userMessageId: String,
        messageId: String,
        text: String,
    ): PlannerPersistenceReceipt? {
        val trimmed = text.trim()
        requireValidAssistantMessageId(userMessageId)
        requireValidAssistantMessageId(messageId)
        require(isValidAssistantText(trimmed, MAX_ASSISTANT_REPLY_BYTES)) {
            "Assistant reply is empty or exceeds the byte limit"
        }
        return mutateDurably { current ->
            require(
                current.messages.any {
                    it.id == userMessageId && it.role == ChatRole.USER
                },
            ) { "Assistant reply has no durable user-message anchor" }
            require(current.messages.none { it.id == messageId }) {
                "Assistant message identity already exists"
            }
            current.copy(
                messages = appendBoundedAssistantMessage(
                    current.messages,
                    ChatMessage(messageId, ChatRole.ASSISTANT, trimmed),
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
            publishedScheduleRevision = current.publishedScheduleRevision
                .takeUnless { recurrenceChanged },
            publishedScheduleProof = current.publishedScheduleProof
                .takeUnless { recurrenceChanged },
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
        moveStart: Instant,
        approval: MoveLaterApprovalEnvelope? = null,
    ): PlannerPersistenceReceipt? = mutateDurably { current ->
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
        require(!current.hasOpenOrPendingExecutionForOccurrence(occurrenceId)) {
            "An active, paused, or pending execution lease still owns this occurrence"
        }
        require(UUID.fromString(occurrenceId).version() == 5) {
            "The occurrence identity is not a server-issued v5 UUID"
        }
        val occurrenceSource = current.recurrenceOccurrenceSources[occurrenceId]
            ?: throw IllegalArgumentException("The exact occurrence source is unavailable")
        val occurrenceSeries = current.canonicalItems.firstOrNull {
            it.id == occurrenceSource.itemId
        } ?: throw IllegalArgumentException("The occurrence series is unavailable")
        require(
            occurrenceSource.itemId == current.occurrenceSeriesItemIds[occurrenceId] &&
                occurrenceSource.hasValidRecurrenceSourceFor(occurrenceSeries) &&
                recurrenceIdentityType(occurrenceSource.identityJson) != "custom",
        ) { "The occurrence source no longer matches its series revision" }
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
        require(
            movedBlocks.isNotEmpty() && movedBlocks.all { block ->
                block.status == ItemStatus.SCHEDULED &&
                    block.isRepresentableMoveLaterSource()
            },
        ) { "Every occurrence session must be scheduled and flexible before moving" }
        require(current.unscheduledWork.none { work ->
            work.occurrenceId == occurrenceId && work.remainingMinutes > 0
        }) { "An occurrence with unscheduled remaining work cannot move as a partial span" }
        val assessedAt = Instant.ofEpochMilli(nowEpochMillis())
        val currentAssessment = current.assessMoveLater(
            focusedBlockId,
            moveStart,
            assessedAt,
        )
        require(
            currentAssessment != null && currentAssessment.fitsFirmHorizonDay &&
                !currentAssessment.crossesUnrelaxableHardDeadline &&
                currentAssessment.isCoveredBy(approval),
        ) { "Move-later risks changed after the user's review" }
        val focusedStart = focused.absoluteStartAt?.let(Instant::parse)
            ?: throw IllegalArgumentException("Canonical block has no exact start")
        val shift = Duration.between(focusedStart, moveStart)
        require(!shift.isNegative && !shift.isZero && shift.nano == 0) {
            "Session deferral must start later on a whole second"
        }
        val shiftedBounds = movedBlocks.map { block ->
            val start = block.absoluteStartAt?.let(Instant::parse)
                ?: throw IllegalArgumentException("Canonical block has no exact start")
            val end = block.absoluteEndAt?.let(Instant::parse)
                ?: throw IllegalArgumentException("Canonical block has no exact end")
            start.plus(shift) to end.plus(shift)
        }
        val movedWindowStart = requireNotNull(shiftedBounds.minOfOrNull { it.first })
        val movedWindowEnd = requireNotNull(shiftedBounds.maxOfOrNull { it.second })
        val planningZone = listOfNotNull(current.schedulePlanningZoneId, focused.planningZoneId)
            .firstNotNullOfOrNull { raw -> runCatching { ZoneId.of(raw) }.getOrNull() }
            ?: throw IllegalArgumentException("The planning timezone is unavailable")
        val firmHorizon = current.scheduleDisplayHorizon(assessedAt, planningZone)
            ?: throw IllegalArgumentException("The exact firm horizon is unavailable")
        val targetDate = movedWindowStart.atZone(planningZone).toLocalDate()
        val targetDayStart = strictLocalDayStartInstant(targetDate, planningZone)
            ?: throw IllegalArgumentException("The target planning day has no exact start")
        val targetDayEnd = strictLocalDayEndInstant(targetDate.plusDays(1), planningZone)
            ?: throw IllegalArgumentException("The target planning day has no exact end")
        require(
            firmHorizon.timezone == planningZone &&
                movedWindowStart >= firmHorizon.start && movedWindowEnd <= firmHorizon.end &&
                movedWindowStart >= targetDayStart && movedWindowEnd <= targetDayEnd,
        ) {
            "A recurrence move must fit wholly inside one firm-horizon planning day"
        }
        val moves = current.recurrenceMoves + (
            occurrenceId to RecurrenceMoveSnapshot(
                itemId = current.occurrenceSeriesItemIds[occurrenceId]
                    ?: throw IllegalArgumentException("Occurrence owner is unavailable"),
                startAt = movedWindowStart.toString(),
                endAt = movedWindowEnd.toString(),
                movedAt = assessedAt.toString(),
                source = occurrenceSource,
            )
        )
        current.copy(
            schedule = current.schedule.map { block ->
                if (block.id in targetIds) block.copy(status = ItemStatus.SCHEDULED) else block
            },
            activeSession = current.activeSession?.takeUnless { it.itemId in targetIds },
            recurrenceMoves = moves,
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
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

    /** Persists firm-horizon/work-window/weight changes and revokes old schedule authority. */
    internal fun updateScheduleCompositionProfile(
        profile: ScheduleCompositionProfileSnapshot,
    ): Boolean {
        require(profile.hasValidShape())
        return mutate { current -> current.withScheduleCompositionProfile(profile) }
    }

    /** Exact encrypted save used by the settings UI before it reports success. */
    internal fun updateScheduleCompositionProfileDurably(
        profile: ScheduleCompositionProfileSnapshot,
    ): PlannerPersistenceReceipt? {
        require(profile.hasValidShape())
        return mutateDurably { current -> current.withScheduleCompositionProfile(profile) }
    }

    private fun DayWeaveUiState.withScheduleCompositionProfile(
        profile: ScheduleCompositionProfileSnapshot,
    ): DayWeaveUiState {
        if (scheduleCompositionProfile == profile) return this
        require(scheduleCompositionProfileEditBlocker() == null) {
            "A canonical action must reconcile before changing the scheduling profile"
        }
        return copy(
            scheduleCompositionProfile = profile,
            publishedScheduleRevision = null,
            publishedScheduleProof = null,
            scheduleInputDigest = null,
            localScheduleCompositionProvenance = null,
            scheduleMessage = "Scheduling profile changed · recompose to refresh the firm horizon",
        )
    }

    fun enableHealthConnectSync(): Boolean = mutate { current ->
        current.copy(healthConnectSyncEnabled = true)
    }

    /** Stops provider reads and removes the retained derived estimate; manual input is untouched. */
    fun disableHealthConnectSync(): Boolean = mutate { current ->
        current.copy(
            healthConnectSyncEnabled = false,
            derivedEnergySnapshot = null,
        )
    }

    fun replaceDerivedEnergySnapshot(snapshot: DerivedEnergySnapshot?): Boolean {
        snapshot?.let {
            require(it.source != EnergySignalSource.MANUAL_CHECK_IN)
            Instant.parse(it.calculatedAt)
        }
        return mutate { current ->
            if (!current.healthConnectSyncEnabled) current else {
                current.copy(derivedEnergySnapshot = snapshot)
            }
        }
    }

    fun recordManualEnergyCheckIn(energy: EnergyLevel): Boolean = mutate { current ->
        current.copy(
            manualEnergyCheckIn = ManualEnergyCheckIn(
                energy = energy,
                checkedInAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
            ),
        )
    }

    fun clearManualEnergyCheckIn(): Boolean = mutate { current ->
        current.copy(manualEnergyCheckIn = null)
    }

    fun recompose() {
        mutate {
            it.copy(
                publishedScheduleProof = null,
                scheduleMessage =
                    "Recomposed · hard commitments and the focus horizon stayed fixed",
            )
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
                    session.pauseReason == null && endedAt == null &&
                    session.actualSeconds == null && moveStart == null && moveEnd == null,
            )
            "paused" -> require(
                runningSince == null && pausedAt != null && pausedAt >= startedAt &&
                    pausedAt <= updatedAt &&
                    (pauseUntil == null || pauseUntil > updatedAt &&
                        pauseUntil <= updatedAt.plusSeconds(MAX_EXECUTION_PAUSE_SECONDS.toLong())) &&
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
    }

    private fun validateClosedExecutionOutcome(outcome: TerminalExecutionOutcomeSnapshot) {
        require(outcome.syncOrigin.isNotBlank())
        validateExecutionSession(outcome.session, mustBeOpen = false)
        require(outcome.session.status in CLOSED_EXECUTION_STATUSES)
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
        if (outcome.session.status == "deferred") {
            require(!outcome.requiresCanonicalItemProjection) {
                "Deferred execution cannot project a canonical terminal state"
            }
        }
    }

    private fun validatedClosedExecutionOutcomes(
        outcomes: Map<String, TerminalExecutionOutcomeSnapshot>,
    ): List<TerminalExecutionOutcomeSnapshot> {
        return outcomes.map { (sessionId, outcome) ->
            require(sessionId == outcome.session.id)
            validateClosedExecutionOutcome(outcome)
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
        session.status in TERMINAL_EXECUTION_STATUSES &&
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
        validateClosedExecutionOutcome(outcome)
        require(outcome.session.status in TERMINAL_EXECUTION_STATUSES) {
            "Execution closure does not have a canonical terminal projection"
        }
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
            session.status !in TERMINAL_EXECUTION_STATUSES ||
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
            isSensitive = item?.let { canonical ->
                effectiveCanonicalSensitivity(
                    state.canonicalItems,
                    canonical.id,
                    state.pendingCanonicalMutation,
                    state.pendingCanonicalAuthoringMutations,
                )
            } ?: true,
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
     * Keeps every immutable closed fact for the lifetime of this credential binding.
     *
     * Server history is paged and plans can reintroduce an old split/session identity years later;
     * dropping a resolved row would therefore resurrect completed work. The encrypted Room snapshot
     * is the compact durable ledger, not a presentation cache, so it intentionally has no age/count
     * eviction policy.
     */
    private fun retainedClosedExecutionOutcomes(
        outcomes: Map<String, TerminalExecutionOutcomeSnapshot>,
    ): Map<String, TerminalExecutionOutcomeSnapshot> {
        outcomes.forEach { (sessionId, outcome) ->
            require(sessionId == outcome.session.id)
            validateClosedExecutionOutcome(outcome)
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

    private fun PendingExecutionDeferIntent.hasSameImmutableIdentity(
        session: CanonicalExecutionSessionSnapshot,
    ): Boolean =
        sessionId == session.id && itemId == session.itemId &&
            itemRevision == session.itemRevision && occurrenceId == session.occurrenceId &&
            sessionIndex == session.sessionIndex && plannedBlockId == session.plannedBlockId &&
            sourceDeviceId == session.sourceDeviceId

    private fun PendingExecutionDeferIntent.hasSameImmutableIdentity(
        command: PendingExecutionCommand,
    ): Boolean =
        sessionId == command.sessionId && itemId == command.itemId &&
            itemRevision == command.itemRevision && occurrenceId == command.occurrenceId &&
            sessionIndex == command.sessionIndex && plannedBlockId == command.plannedBlockId &&
            sourceDeviceId == command.sourceDeviceId && focusedBlockId == command.focusedBlockId

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
    ): PlannerPersistenceReceipt? = mutateDurablyWithSnapshot(transform)?.receipt

    /** Returns the exact post-normalization snapshot represented by the durable receipt. */
    private fun mutateDurablyWithSnapshot(
        transform: (DayWeaveUiState) -> DayWeaveUiState,
    ): MutationResult? {
        val mutation = mutateInternal(requireExactSave = true, transform) ?: return null
        if (mutation.shouldSignalWriter) saveRequests.trySend(Unit)
        requireNotNull(mutation.receipt)
        return mutation
    }

    /**
     * Legacy or corrupt recurrence source metadata cannot authorize a later cross-horizon Move.
     * Quarantine only those envelopes and their dependent moves; current canonical/schedule truth
     * remains available, and removing a previously effective move invalidates publication proof.
     */
    private fun DayWeaveUiState.withInvalidRecurrenceMoveSourcesAbandoned(): DayWeaveUiState {
        if (recurrenceOccurrenceSources.isEmpty() && recurrenceMoves.isEmpty()) return this
        val itemsById = canonicalItems.associateBy(CanonicalItemSnapshot::id)
        val validOccurrenceSources = recurrenceOccurrenceSources.filter { (occurrenceId, source) ->
            runCatching {
                val id = UUID.fromString(occurrenceId)
                require(id != NIL_UUID && id.version() == 5 && id.toString() == occurrenceId)
                require(occurrenceSeriesItemIds[occurrenceId] == source.itemId)
                val item = requireNotNull(itemsById[source.itemId])
                require(source.hasValidRecurrenceSourceFor(item))
            }.isSuccess
        }
        val validMoves = recurrenceMoves.filter { (occurrenceId, move) ->
            runCatching {
                val id = UUID.fromString(occurrenceId)
                require(id != NIL_UUID && id.version() == 5 && id.toString() == occurrenceId)
                require(move.itemId == move.source?.itemId)
                val item = requireNotNull(itemsById[move.itemId])
                require(requireNotNull(move.source).hasValidRecurrenceSourceFor(item))
                require(recurrenceIdentityType(move.source.identityJson) != "custom")
                val start = Instant.parse(move.startAt)
                val end = Instant.parse(move.endAt)
                val movedAt = Instant.parse(move.movedAt)
                require(
                    start.toString() == move.startAt && end.toString() == move.endAt &&
                        movedAt.toString() == move.movedAt && start < end,
                )
            }.isSuccess
        }
        if (
            validOccurrenceSources == recurrenceOccurrenceSources && validMoves == recurrenceMoves
        ) {
            return this
        }
        val moveWasAbandoned = validMoves.size != recurrenceMoves.size
        return copy(
            recurrenceOccurrenceSources = validOccurrenceSources,
            recurrenceMoves = validMoves,
            publishedScheduleRevision = publishedScheduleRevision.takeUnless { moveWasAbandoned },
            publishedScheduleProof = publishedScheduleProof.takeUnless { moveWasAbandoned },
            scheduleInputDigest = scheduleInputDigest.takeUnless { moveWasAbandoned },
            scheduleMessage = if (moveWasAbandoned) {
                "A saved recurrence move lacked verifiable source identity and was abandoned"
            } else {
                scheduleMessage
            },
        )
    }

    private fun DayWeaveUiState.requireCurrentExecutionDeferAssessment(
        intent: PendingExecutionDeferIntent,
        assessment: ExecutionDeferAssessmentSnapshot,
        requireFresh: Boolean,
    ) {
        fun requireUuid(raw: String) {
            val parsed = UUID.fromString(raw)
            require(parsed != NIL_UUID && parsed.toString() == raw)
        }
        fun requireDigest(raw: String) {
            require(
                raw.length == 71 && raw.startsWith("sha256:") &&
                    raw.drop(7).all { it in '0'..'9' || it in 'a'..'f' },
            )
        }
        fun requireServerInstant(raw: String): Instant {
            require(raw.length <= MAX_EXECUTION_DEFER_TIMESTAMP_CHARS && raw.none(Char::isISOControl))
            return Instant.parse(raw).also { parsed ->
                // PostgreSQL is the durable protocol clock; evidence with finer precision cannot
                // be reproduced by the server's stale-assessment transaction.
                require(parsed.nano % 1_000 == 0)
            }
        }
        require(intent.schemaVersion == EXECUTION_DEFER_INTENT_SCHEMA_VERSION)
        require(
            intent.approvedConflictTargetEnd == null && intent.approvedDeadlineRisks.isEmpty() &&
                !intent.approvedSourceOverride && intent.approvedItemRevisions.isEmpty() &&
                intent.approvedHardBlockIds.isEmpty() && intent.approvedHardConflicts.isEmpty(),
        )
        listOf(
            assessment.sessionId,
            assessment.itemId,
            assessment.sourceScheduleRevisionId,
            assessment.sourceBlockId,
        ).forEach(::requireUuid)
        assessment.occurrenceId?.let(::requireUuid)
        requireDigest(assessment.environmentDigest)
        requireDigest(assessment.assessmentDigest)
        require(
            assessment.executionRevision > 0 && assessment.sessionRevision > 0 &&
                assessment.sessionRevision <= assessment.executionRevision &&
                assessment.itemRevision > 0 &&
                assessment.sourceSessionIndex in 0 until UShort.MAX_VALUE.toInt() &&
                assessment.replacementSessionIndex in 0..UShort.MAX_VALUE.toInt() &&
                assessment.replacementSessionIndex > assessment.sourceSessionIndex,
        )
        require(
            assessment.actualSeconds >= 0 && assessment.creditedSourceSeconds >= 0 &&
                assessment.plannedDurationSeconds in
                1..MAX_DEFER_MOVE_WINDOW_SECONDS.toLong() &&
                assessment.remainingDurationSeconds in
                1..MAX_DEFER_MOVE_WINDOW_SECONDS.toLong() &&
                assessment.creditedSourceSeconds <= assessment.plannedDurationSeconds &&
                assessment.plannedDurationSeconds - assessment.creditedSourceSeconds ==
                assessment.remainingDurationSeconds,
        )
        val moveStart = requireServerInstant(assessment.moveStart)
        val moveEnd = requireServerInstant(assessment.moveEnd)
        val expiresAt = requireServerInstant(assessment.expiresAt)
        val moveDuration = Duration.between(moveStart, moveEnd)
        require(
            moveStart < moveEnd && moveDuration.nano == 0 &&
                moveDuration.seconds == assessment.remainingDurationSeconds &&
                expiresAt < moveStart,
        )
        if (requireFresh) {
            require(expiresAt > Instant.ofEpochMilli(nowEpochMillis()))
        }
        require(
            assessment.violations.size <= MAX_EXECUTION_DEFER_VIOLATIONS &&
                assessment.approvalRequired == assessment.violations.isNotEmpty(),
        )
        assessment.violations.forEach { violation ->
            require(violation.code in EXECUTION_DEFER_VIOLATION_CODES)
            require(
                violation.message.isNotBlank() &&
                    violation.message.length <= MAX_EXECUTION_DEFER_MESSAGE_CHARS &&
                    violation.message.none { it.isISOControl() },
            )
            require(
                violation.itemIds.size <= MAX_EXECUTION_DEFER_REFERENCES &&
                    violation.occurrenceIds.size <= MAX_EXECUTION_DEFER_REFERENCES &&
                    violation.conflictingBlockIds.size <= MAX_EXECUTION_DEFER_REFERENCES &&
                    violation.conflictingBlocks.size <= MAX_EXECUTION_DEFER_REFERENCES &&
                    violation.itemIds.distinct().size == violation.itemIds.size &&
                    violation.occurrenceIds.distinct().size == violation.occurrenceIds.size &&
                    violation.conflictingBlockIds.distinct().size ==
                    violation.conflictingBlockIds.size &&
                    violation.conflictingBlocks.map { it.blockId }.distinct().size ==
                    violation.conflictingBlocks.size,
            )
            violation.itemIds.forEach(::requireUuid)
            violation.occurrenceIds.forEach(::requireUuid)
            violation.conflictingBlockIds.forEach(::requireUuid)
            val violationStart = requireServerInstant(violation.start)
            val violationEnd = requireServerInstant(violation.end)
            require(violationStart < violationEnd)
            violation.boundaryStart?.let(::requireServerInstant)
            violation.boundaryEnd?.let(::requireServerInstant)
            require(
                violation.conflictingBlocks.map { it.blockId }.toSet() ==
                    violation.conflictingBlockIds.toSet(),
            )
            violation.conflictingBlocks.forEach { conflict ->
                requireUuid(conflict.blockId)
                conflict.itemId?.let(::requireUuid)
                conflict.occurrenceId?.let(::requireUuid)
                conflict.externalBlockId?.let(::requireUuid)
                require(conflict.kind in EXECUTION_DEFER_BLOCK_KINDS)
                require(
                    requireServerInstant(conflict.start) < requireServerInstant(conflict.end),
                )
            }
        }

        val lease = requireNotNull(canonicalExecutionSession)
        require(lease.status == "paused" && lease.runningSince == null)
        require(intent.hasSameImmutableIdentity(lease))
        require(
            assessment.sessionId == intent.sessionId &&
                assessment.executionRevision == canonicalExecutionRevision &&
                assessment.sessionRevision == lease.revision &&
                assessment.itemId == intent.itemId &&
                assessment.itemRevision == intent.itemRevision &&
                assessment.occurrenceId == intent.occurrenceId &&
                assessment.sourceSessionIndex == intent.sessionIndex &&
                assessment.sourceBlockId == intent.plannedBlockId &&
                assessment.actualSeconds == lease.accumulatedSeconds &&
                assessment.moveStart == intent.moveStart,
        )
        val sourceDuration = Duration.between(
            Instant.parse(intent.sourceStart),
            Instant.parse(intent.sourceEnd),
        )
        require(
            sourceDuration.nano == 0 &&
                sourceDuration.seconds == assessment.plannedDurationSeconds,
        )
        val source = schedule.single { it.id == intent.focusedBlockId }
        require(hasPublishedExecutionAuthority(source))
        val publication = requireNotNull(publishedScheduleProof)
        require(
            publication.blocks.single { it.id == intent.plannedBlockId }.let { proof ->
                    proof.itemId == intent.itemId && proof.itemRevision == intent.itemRevision &&
                        proof.occurrenceId == intent.occurrenceId &&
                        proof.sessionIndex == intent.sessionIndex &&
                        proof.start == intent.sourceStart && proof.end == intent.sourceEnd
                },
        )
    }

    private fun PendingExecutionCommand.matchesExecutionDeferAssessment(
        assessment: ExecutionDeferAssessmentSnapshot,
    ): Boolean = runCatching {
        val root = EXECUTION_JOURNAL_JSON.parseToJsonElement(requestJson).jsonObject
        require(root.keys == setOf("expected_revision", "command"))
        val revision = root.getValue("expected_revision").jsonPrimitive
        require(!revision.isString && revision.long == assessment.executionRevision)
        val body = root.getValue("command").jsonObject
        val expectedKeys = mutableSetOf(
            "type",
            "session_id",
            "move_start",
            "move_end",
            "actual_seconds",
            "assessment_digest",
        )
        if (assessment.approvalRequired) expectedKeys += "approved_assessment_digest"
        require(body.keys == expectedKeys)
        val type = body.getValue("type").jsonPrimitive
        val session = body.getValue("session_id").jsonPrimitive
        val moveStart = body.getValue("move_start").jsonPrimitive
        val moveEnd = body.getValue("move_end").jsonPrimitive
        val actual = body.getValue("actual_seconds").jsonPrimitive
        val assessmentDigest = body.getValue("assessment_digest").jsonPrimitive
        require(type.isString && type.content == "defer")
        require(session.isString && session.content == assessment.sessionId)
        require(moveStart.isString && moveStart.content == assessment.moveStart)
        require(moveEnd.isString && moveEnd.content == assessment.moveEnd)
        require(!actual.isString && actual.long == assessment.actualSeconds)
        require(
            assessmentDigest.isString && assessmentDigest.content == assessment.assessmentDigest,
        )
        val approved = body["approved_assessment_digest"]?.jsonPrimitive
        require(approved?.isString != false)
        require(approved?.content == assessment.assessmentDigest.takeIf { assessment.approvalRequired })
    }.isSuccess

    /**
     * A user-level Defer intent may outlive a process, but it may never outlive its exact lease,
     * publication proof, or credential binding. Invalid/superseded intent is safe to abandon: no
     * server write is inferred, while the reconciled active/paused lease remains untouched.
     */
    private fun DayWeaveUiState.withInvalidExecutionDeferIntentAbandoned(): DayWeaveUiState {
        val intent = pendingExecutionDeferIntent ?: return this
        val baseValid = runCatching {
            require(intent.schemaVersion == EXECUTION_DEFER_INTENT_SCHEMA_VERSION)
            require(
                intent.approvedConflictTargetEnd == null &&
                    intent.approvedDeadlineRisks.isEmpty() &&
                    !intent.approvedSourceOverride && intent.approvedItemRevisions.isEmpty() &&
                    intent.approvedHardBlockIds.isEmpty() &&
                    intent.approvedHardConflicts.isEmpty(),
            )
            require(
                intent.syncOrigin.isNotBlank() && intent.syncOrigin.length <= 4_096 &&
                    intent.syncOrigin.none(Char::isISOControl),
            )
            require(
                intent.configurationId?.let {
                    it.isNotBlank() && it.length <= 4_096 && it.none(Char::isISOControl)
                } != false,
            )
            listOf(
                intent.sessionId,
                intent.itemId,
                intent.plannedBlockId,
                intent.sourceDeviceId,
                intent.focusedBlockId,
            ).forEach { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            intent.occurrenceId?.let { raw ->
                val id = UUID.fromString(raw)
                require(id != NIL_UUID && id.toString() == raw)
            }
            require(intent.itemRevision > 0 && intent.sessionIndex in 0..UShort.MAX_VALUE.toInt())
            require(
                intent.syncOrigin == canonicalSyncOrigin &&
                    intent.configurationId == canonicalConfigurationId &&
                    intent.syncOrigin == canonicalExecutionSyncOrigin &&
                    intent.configurationId == canonicalExecutionConfigurationId,
            )
            val sourceStart = Instant.parse(intent.sourceStart)
            val sourceEnd = Instant.parse(intent.sourceEnd)
            val moveStart = Instant.parse(intent.moveStart)
            val stagedAt = Instant.parse(intent.stagedAt)
            require(
                sourceStart.toString() == intent.sourceStart &&
                    sourceEnd.toString() == intent.sourceEnd &&
                    moveStart.toString() == intent.moveStart &&
                    stagedAt.toString() == intent.stagedAt,
            )
            val sourceDuration = Duration.between(sourceStart, sourceEnd)
            require(
                sourceDuration.nano == 0 &&
                    sourceDuration.seconds in 1..MAX_DEFER_MOVE_WINDOW_SECONDS.toLong() &&
                    moveStart.nano == 0 && moveStart > stagedAt &&
                    moveStart > Instant.ofEpochMilli(nowEpochMillis()),
            )
            val lease = requireNotNull(canonicalExecutionSession)
            require(lease.status in OPEN_EXECUTION_STATUSES)
            require(intent.hasSameImmutableIdentity(lease))
            val remainingFloor = Math.subtractExact(
                sourceDuration.seconds,
                lease.accumulatedSeconds,
            )
            require(remainingFloor in 1..MAX_DEFER_MOVE_WINDOW_SECONDS.toLong())
            val source = schedule.single { it.id == intent.focusedBlockId }
            require(
                intent.focusedBlockId == intent.plannedBlockId &&
                    source.canonicalItemId == intent.itemId &&
                    source.canonicalRevision == intent.itemRevision &&
                    source.occurrenceId == intent.occurrenceId &&
                    source.sessionIndex == intent.sessionIndex &&
                    source.absoluteStartAt == intent.sourceStart &&
                    source.absoluteEndAt == intent.sourceEnd,
            )
            require(hasPublishedExecutionAuthority(source))
            val proofEnvelope = requireNotNull(publishedScheduleProof)
            require(
                proofEnvelope.syncOrigin == intent.syncOrigin &&
                    proofEnvelope.configurationId == intent.configurationId,
            )
            val proof = proofEnvelope.blocks.single { it.id == intent.plannedBlockId }
            require(
                proof.itemId == intent.itemId && proof.itemRevision == intent.itemRevision &&
                    proof.occurrenceId == intent.occurrenceId &&
                    proof.sessionIndex == intent.sessionIndex &&
                    proof.start == intent.sourceStart && proof.end == intent.sourceEnd,
            )
            pendingExecutionCommand?.let { command ->
                require(command.commandType in setOf("pause", "defer"))
                require(intent.hasSameImmutableIdentity(command))
            }
        }.isSuccess
        if (!baseValid) {
            return copy(
                pendingExecutionDeferIntent = null,
                scheduleMessage =
                    "Saved move could not be verified and was abandoned safely · " +
                        "authoritative execution state was retained",
            )
        }

        val assessment = intent.assessment
        if (assessment == null) {
            return if (intent.approvedAssessmentDigest == null) this else copy(
                pendingExecutionDeferIntent = intent.copy(approvedAssessmentDigest = null),
                scheduleMessage = "Unbound move approval was discarded safely",
            )
        }
        val evidenceValid = runCatching {
            requireCurrentExecutionDeferAssessment(intent, assessment, requireFresh = true)
            require(
                if (assessment.approvalRequired) {
                    intent.approvedAssessmentDigest == null ||
                        intent.approvedAssessmentDigest == assessment.assessmentDigest
                } else {
                    intent.approvedAssessmentDigest == null
                },
            )
        }.isSuccess
        return if (evidenceValid) this else copy(
            pendingExecutionDeferIntent = intent.copy(
                assessment = null,
                approvedAssessmentDigest = null,
            ),
            scheduleMessage =
                "Move assessment expired or became stale · the paused target was retained",
        )
    }

    /** Corrupt, stale, or server-authorized local provenance is never allowed to survive restore. */
    private fun DayWeaveUiState.withInvalidLocalScheduleCompositionAbandoned(): DayWeaveUiState {
        val provenance = localScheduleCompositionProvenance ?: return this
        return if (provenance.matchesState(this)) this else copy(
            localScheduleCompositionProvenance = null,
            scheduleMessage = "Saved on-device composition became stale and was discarded safely",
        )
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
        val previous = mutableState.value
        val transformed = transform(previous)
            .withCanonicalTrashRetention(nowEpochMillis())
            .withPendingSensitivityHardened()
            .withInvalidRecurrenceMoveSourcesAbandoned()
            .withInvalidExecutionDeferIntentAbandoned()
            .withInvalidTimedBreakNotificationAttemptAbandoned()
            .withBoundedAssistantMessages()
        // Kotlin data-class copy preserves unchanged input references but not body-property memos.
        // Transfer a verified digest/result only across an O(1) exact structural identity fence.
        transformed.inheritLocalScheduleCompositionMemo(previous)
        transformed.inheritPublishedScheduleValidationMemo(previous)
        val snapshot = transformed.withInvalidLocalScheduleCompositionAbandoned()
            .also { requireCanonicalAuthoringJournalBudget(it.pendingCanonicalAuthoringMutations) }
        mutableState.value = snapshot
        currentGeneration += 1
        scheduleCanonicalTrashCleanupLocked(snapshot)

        if (persistenceStatus != PersistenceStatus.READY) {
            mutableDurableState.value = snapshot
            val receipt = if (requireExactSave) {
                PlannerPersistenceReceipt(
                    generation = currentGeneration,
                    completion = CompletableDeferred(true),
                )
            } else {
                null
            }
            return@synchronized MutationResult(
                receipt = receipt,
                shouldSignalWriter = false,
                snapshot = snapshot,
            )
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
            snapshot = snapshot,
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
            val snapshot = (persistedState ?: initialState)
                .withCanonicalTrashRetention(nowEpochMillis())
                .withPendingSensitivityHardened()
                .withInvalidRecurrenceMoveSourcesAbandoned()
                .withInvalidExecutionDeferIntentAbandoned()
                .withInvalidTimedBreakNotificationAttemptAbandoned()
                .withBoundedAssistantMessages()
                .withInvalidLocalScheduleCompositionAbandoned()
                .also { requireCanonicalAuthoringJournalBudget(it.pendingCanonicalAuthoringMutations) }
            mutableState.value = snapshot
            currentGeneration += 1
            persistenceStatus = PersistenceStatus.READY
            scheduleCanonicalTrashCleanupLocked(snapshot)
            if (persistedState == null || snapshot != persistedState) {
                latestNormalSaveRequest = SaveRequest(currentGeneration, snapshot)
                true
            } else {
                persistedGeneration = currentGeneration
                mutableDurableState.value = snapshot
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
                    mutableDurableState.value = request.snapshot
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

    /** Fail closed on legacy/corrupt transcript members and retain only the newest bounded suffix. */
    private fun DayWeaveUiState.withBoundedAssistantMessages(): DayWeaveUiState {
        val sanitized = messages.filter { message ->
            isValidAssistantMessageId(message.id) && when (message.role) {
                ChatRole.USER ->
                    isValidAssistantText(message.text, MAX_ASSISTANT_USER_MESSAGE_BYTES)
                ChatRole.ASSISTANT ->
                    isValidAssistantText(message.text, MAX_ASSISTANT_REPLY_BYTES)
            }
        }
        val uniqueNewest = sanitized.asReversed()
            .distinctBy(ChatMessage::id)
            .asReversed()
        val retainedNewest = ArrayDeque<ChatMessage>()
        var retainedBytes = 0
        for (message in uniqueNewest.asReversed()) {
            val bytes = message.text.toByteArray(Charsets.UTF_8).size
            if (
                retainedNewest.size >= MAX_ASSISTANT_MESSAGES ||
                retainedBytes + bytes > MAX_ASSISTANT_TRANSCRIPT_BYTES
            ) {
                break
            }
            retainedNewest.addFirst(message)
            retainedBytes += bytes
        }
        val retained = retainedNewest.toList()
        return if (retained == messages) this else copy(messages = retained)
    }

    private fun appendBoundedAssistantMessage(
        messages: List<ChatMessage>,
        message: ChatMessage,
    ): List<ChatMessage> = DayWeaveUiState(messages = messages + message)
        .withBoundedAssistantMessages()
        .messages

    private fun requireValidAssistantMessageId(id: String) {
        require(isValidAssistantMessageId(id)) { "Assistant message identity is invalid" }
    }

    private fun isValidAssistantMessageId(id: String): Boolean =
        id.isNotBlank() && id.length <= MAX_ASSISTANT_MESSAGE_ID_CHARS &&
            id.none(Char::isISOControl)

    private fun isValidAssistantText(text: String, maximumBytes: Int): Boolean =
        text.isValidAssistantConversationText(maximumBytes)

    private fun scheduleCanonicalTrashCleanupLocked(snapshot: DayWeaveUiState) {
        canonicalTrashCleanupCancellation?.cancel()
        canonicalTrashCleanupCancellation = null
        canonicalTrashCleanupToken += 1L
        val scheduler = canonicalTrashCleanupScheduler ?: return
        val now = nowEpochMillis()
        val deadline = snapshot.nextCanonicalTrashRetentionExpiryEpochMillis(now) ?: return
        val token = canonicalTrashCleanupToken
        val delayMillis = (deadline - now).coerceAtLeast(1L)
        canonicalTrashCleanupCancellation = scheduler.schedule(delayMillis) {
            val isCurrent = synchronized(persistenceLock) {
                token == canonicalTrashCleanupToken
            }
            if (isCurrent) {
                // An exact save prevents privacy cleanup from being coalesced behind routine UI IO.
                mutateDurably { it }
            }
        }
    }

    private fun markPersistenceFailed(
        error: Throwable,
        failedRequest: SaveRequest? = null,
    ) {
        synchronized(persistenceLock) {
            persistenceStatus = PersistenceStatus.FAILED
            canonicalTrashCleanupCancellation?.cancel()
            canonicalTrashCleanupCancellation = null
            canonicalTrashCleanupToken += 1L
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
        val snapshot: DayWeaveUiState,
    )

    private data class SaveRequest(
        val generation: Long,
        val snapshot: DayWeaveUiState,
        val completion: CompletableDeferred<Boolean>? = null,
    )

    private data class ScheduleIdentity(
        val itemId: String?,
        val occurrenceId: String?,
        val sessionIndex: Int?,
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
        val CLOSED_EXECUTION_STATUSES = TERMINAL_EXECUTION_STATUSES + "deferred"
        val TERMINAL_CANONICAL_STATUSES = setOf("completed", "skipped")
        val ALL_EXECUTION_STATUSES = OPEN_EXECUTION_STATUSES + CLOSED_EXECUTION_STATUSES
        val EXECUTION_COMMAND_TYPES = setOf(
            "start",
            "pause",
            "resume",
            "complete",
            "skip",
            "defer",
        )
        const val LOCAL_SCHEDULE_FINGERPRINT_PREFIX = "local-sha256:"
        const val MAX_PENDING_EXECUTION_REQUEST_CHARS = 64 * 1024
        const val MAX_EXECUTION_HISTORY_WINDOW = 100
        const val MAX_EXECUTION_PAUSE_SECONDS = 24 * 60 * 60
        const val MAX_DEFER_MOVE_WINDOW_SECONDS = 24 * 60 * 60
        const val EXECUTION_DEFER_INTENT_SCHEMA_VERSION = 1
        const val MAX_EXECUTION_DEFER_VIOLATIONS = 10_000
        const val MAX_EXECUTION_DEFER_REFERENCES = 10_000
        const val MAX_EXECUTION_DEFER_MESSAGE_CHARS = 1_000
        const val MAX_EXECUTION_DEFER_TIMESTAMP_CHARS = 64
        const val MAX_ASSISTANT_USER_MESSAGE_BYTES = 8 * 1024
        const val MAX_ASSISTANT_REPLY_BYTES = 32 * 1024
        const val MAX_ASSISTANT_MESSAGE_ID_CHARS = 128
        const val MAX_ASSISTANT_MESSAGES = 200
        const val MAX_ASSISTANT_TRANSCRIPT_BYTES = 512 * 1024
        val EXECUTION_DEFER_VIOLATION_CODES = setOf(
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
        val EXECUTION_DEFER_BLOCK_KINDS = setOf(
            "planned",
            "pinned",
            "calendar_event",
            "external_fixed",
        )
        val EXECUTION_JOURNAL_JSON = Json { ignoreUnknownKeys = false }
        const val MAX_TERMINAL_PROJECTION_CONFLICT_CHARS = 500
        const val SCHEDULE_PUBLICATION_JOURNAL_VERSION = 1
        const val PROPOSAL_APPLICATION_JOURNAL_VERSION = 1
        const val PROPOSAL_APPLICATION_RECEIPT_VERSION = 1
        const val MAX_PROPOSAL_APPLICATION_COMMANDS = 100
        const val MAX_CANONICAL_AUTHORING_QUEUE = 100
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

        /** Resolves inherited sensitivity and fails closed on a missing or cyclic ancestor. */
        fun effectiveSensitivity(items: List<CanonicalItemSnapshot>, itemId: String): Boolean =
            effectiveCanonicalSensitivity(items, itemId)

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
