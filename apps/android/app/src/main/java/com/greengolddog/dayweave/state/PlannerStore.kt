package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.ActiveSession
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ChatMessage
import com.greengolddog.dayweave.model.ChatRole
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.SuggestionDisposition
import java.util.UUID
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Owns presentation state while repositories and sync adapters are still being connected.
 * All writes go through explicit intents so this class can later sit above the offline database.
 */
class PlannerStore(initialState: DayWeaveUiState = DayWeaveUiState.preview()) {
    private val mutableState = MutableStateFlow(initialState)
    val state: StateFlow<DayWeaveUiState> = mutableState.asStateFlow()

    fun navigate(destination: AppDestination) {
        mutableState.update { it.copy(destination = destination) }
    }

    fun startItem(id: String) {
        mutableState.update { current ->
            if (current.schedule.none { it.id == id }) return@update current

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
        mutableState.update { current ->
            val active = current.activeSession ?: return@update current
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
        mutableState.update { current ->
            val active = current.activeSession ?: return@update current
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
        mutableState.update { current ->
            val active = current.activeSession ?: return@update current
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
        mutableState.update { current ->
            val active = current.activeSession ?: return@update current
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
        mutableState.update { current ->
            val active = current.activeSession ?: return@update current
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

        mutableState.update { current ->
            current.copy(
                inbox = listOf(
                    InboxItem(
                        id = UUID.randomUUID().toString(),
                        title = trimmed,
                        source = InboxSource.QUICK_CAPTURE,
                        detail = "${kind.label} · needs duration and constraints",
                    ),
                ) + current.inbox,
                scheduleMessage = "Captured to Inbox · nothing was scheduled yet",
            )
        }
        return true
    }

    /**
     * Safety boundary for ChatGPT, Codex, and assistant proposals.
     * Approval stages a reviewable Inbox draft and intentionally never mutates [DayWeaveUiState.schedule].
     */
    fun approveSuggestion(id: String) {
        mutableState.update { current ->
            val suggestion = current.suggestions.firstOrNull { it.id == id }
                ?: return@update current
            if (suggestion.disposition != SuggestionDisposition.PENDING) return@update current

            current.copy(
                suggestions = current.suggestions.map {
                    if (it.id == id) it.copy(disposition = SuggestionDisposition.APPROVED_FOR_INBOX) else it
                },
                inbox = listOf(
                    InboxItem(
                        id = "proposal-${suggestion.id}",
                        title = suggestion.title,
                        source = InboxSource.EXTERNAL_PROPOSAL,
                        detail = suggestion.summary,
                        requiresReview = true,
                    ),
                ) + current.inbox,
                scheduleMessage = "Accepted as an Inbox draft · review before scheduling",
            )
        }
    }

    fun rejectSuggestion(id: String) {
        mutableState.update { current ->
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

        mutableState.update { current ->
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

    fun sendAssistantMessage(text: String): Boolean {
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return false
        mutableState.update { current ->
            current.copy(
                messages = current.messages + listOf(
                    ChatMessage(UUID.randomUUID().toString(), ChatRole.USER, trimmed),
                    ChatMessage(
                        UUID.randomUUID().toString(),
                        ChatRole.ASSISTANT,
                        "I’ll check hard constraints, deadlines, energy, and protected free time. Any schedule change will arrive as a reviewable proposal.",
                    ),
                ),
            )
        }
        return true
    }

    fun toggleCompleted() {
        mutableState.update { it.copy(showCompleted = !it.showCompleted) }
    }

    fun toggleQuietSuggestions() {
        mutableState.update { it.copy(quietSuggestions = !it.quietSuggestions) }
    }

    fun toggleDynamicColor() {
        mutableState.update { it.copy(useDynamicColor = !it.useDynamicColor) }
    }

    fun recompose() {
        mutableState.update {
            it.copy(scheduleMessage = "Recomposed · hard commitments and the focus horizon stayed fixed")
        }
    }
}
