package com.greengolddog.dayweave.state

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.ExecutionSyncState
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import java.time.Instant
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

class DayWeaveViewModel(application: Application) : AndroidViewModel(application) {
    private val dayWeaveApplication = application as DayWeaveApplication
    private val plannerStore = dayWeaveApplication.plannerStore
    private val suggestionSyncManager = dayWeaveApplication.suggestionSyncManager
    private val suggestionConnectionController = dayWeaveApplication.suggestionConnectionController
    private val canonicalSyncManager = dayWeaveApplication.canonicalSyncManager
    private val executionSyncManager = dayWeaveApplication.executionSyncManager

    val state: StateFlow<com.greengolddog.dayweave.model.DayWeaveUiState> = plannerStore.state
    val loadState: StateFlow<PlannerLoadState> = plannerStore.loadState
    val suggestionSyncState: StateFlow<SuggestionSyncState> = suggestionSyncManager.state
    val canonicalSyncState: StateFlow<CanonicalSyncState> = canonicalSyncManager.state
    val executionSyncState: StateFlow<ExecutionSyncState> = executionSyncManager.state

    init {
        viewModelScope.launch {
            while (isActive) {
                delay(1_000)
                plannerStore.tickActiveSession()
            }
        }
    }

    fun navigate(destination: AppDestination) = plannerStore.navigate(destination)
    fun startItem(id: String) {
        if (isCanonicalBlock(id)) {
            if (!plannerStore.state.value.isCanonicalPlanCurrent()) {
                recompose()
                return
            }
            if (isCanonicalBusy()) return
            dayWeaveApplication.launchCanonicalAction { executionSyncManager.start(id) }
        } else {
            val current = plannerStore.state.value
            if (
                current.canonicalExecutionSession == null &&
                current.pendingExecutionCommand == null
            ) {
                plannerStore.startItem(id)
            }
        }
    }

    fun pauseActive(minutes: Int? = null) {
        withActiveBlock(
            canonicalAction = { id ->
                executionSyncManager.pause(id, minutes?.let { it * 60 })
            },
            localAction = { plannerStore.pauseActive(minutes) },
        )
    }

    fun pauseActiveUntil(until: Instant) {
        withActiveBlock(
            canonicalAction = { id -> executionSyncManager.pause(id, pauseUntil = until) },
            localAction = {
                val minutes = java.time.Duration.between(Instant.now(), until).toMinutes()
                    .coerceIn(1L, 24L * 60L)
                    .toInt()
                plannerStore.pauseActive(minutes)
            },
        )
    }

    fun resumeActive() {
        resumeActiveIfAvailable()
    }

    private fun resumeActiveIfAvailable(): Boolean =
        withActiveBlock(
            canonicalAction = executionSyncManager::resume,
            localAction = plannerStore::resumeActive,
        )

    fun completeActive() {
        withActiveBlock(
            canonicalAction = { id ->
                finishCanonicalExecution(
                    command = { executionSyncManager.complete(id) },
                    refreshCanonicalState = dayWeaveApplication::refreshCanonicalState,
                )
            },
            localAction = plannerStore::completeActive,
        )
    }

    fun skipActive() {
        withActiveBlock(
            canonicalAction = { id ->
                finishCanonicalExecution(
                    command = { executionSyncManager.skip(id) },
                    refreshCanonicalState = dayWeaveApplication::refreshCanonicalState,
                )
            },
            localAction = plannerStore::skipActive,
        )
    }

    fun doActiveLater() {
        withActiveBlock(
            canonicalAction = executionSyncManager::doLater,
            localAction = plannerStore::doActiveLater,
        )
    }
    fun quickCapture(title: String, kind: ItemKind): Boolean = plannerStore.quickCapture(title, kind)
    fun approveSuggestion(id: String) {
        viewModelScope.launch { suggestionSyncManager.accept(id) }
    }

    fun rejectSuggestion(id: String) {
        viewModelScope.launch { suggestionSyncManager.reject(id) }
    }

    fun updateSuggestion(id: String, title: String, summary: String) {
        viewModelScope.launch { suggestionSyncManager.edit(id, title, summary) }
    }

    fun refreshSuggestions() {
        viewModelScope.launch { suggestionSyncManager.refresh() }
    }

    fun updateSuggestionConnection(baseUrl: String, bearerToken: String?) {
        dayWeaveApplication.launchCanonicalAction {
            if (suggestionConnectionController.update(baseUrl, bearerToken)) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    fun clearSuggestionConnection() {
        dayWeaveApplication.launchCanonicalAction {
            if (suggestionConnectionController.forget()) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    fun sendAssistantMessage(text: String): Boolean = plannerStore.sendAssistantMessage(text)
    fun toggleCompleted() = plannerStore.toggleCompleted()
    fun toggleQuietSuggestions() = plannerStore.toggleQuietSuggestions()
    fun toggleDynamicColor() = plannerStore.toggleDynamicColor()
    fun recompose() {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction { dayWeaveApplication.refreshCanonicalState() }
    }

    /** Called only while the application UI is STARTED; the process action gate coalesces races. */
    fun refreshExecution() {
        if (isCanonicalBusy()) return
        dayWeaveApplication.launchCanonicalAction {
            dayWeaveApplication.refreshForegroundExecution()
        }
    }

    fun keepLatestItemAfterTerminalConflict(sessionId: String) {
        if (isCanonicalBusy() || plannerStore.state.value.pendingCanonicalMutation != null) return
        dayWeaveApplication.launchCanonicalAction {
            val current = plannerStore.state.value
            if (
                current.pendingCanonicalMutation != null ||
                current.terminalExecutionOutcomes[sessionId]?.canonicalProjectionConflict == null
            ) {
                return@launchCanonicalAction
            }
            plannerStore.keepLatestItemAfterTerminalConflict(sessionId)?.awaitDurable()
        }
    }

    fun retryTerminalProjection(sessionId: String) {
        if (isCanonicalBusy() || plannerStore.state.value.pendingCanonicalMutation != null) return
        dayWeaveApplication.launchCanonicalAction {
            val current = plannerStore.state.value
            val outcome = current.terminalExecutionOutcomes[sessionId]
            if (
                current.pendingCanonicalMutation != null ||
                outcome?.canonicalProjectionConflict == null ||
                outcome.canonicalProjectionRetryAuthorizedAt != null
            ) {
                return@launchCanonicalAction
            }
            val authorization = plannerStore.authorizeTerminalProjectionRetry(sessionId)
                ?: return@launchCanonicalAction
            if (authorization.awaitDurable()) {
                dayWeaveApplication.refreshCanonicalState()
            }
        }
    }

    private fun isCanonicalBlock(id: String): Boolean =
        executionActionTarget(plannerStore.state.value, id) == ExecutionActionTarget.SERVER

    private fun withActiveBlock(
        canonicalAction: suspend (String) -> Any?,
        localAction: () -> Unit,
    ): Boolean {
        val activeId = plannerStore.state.value.activeSession?.itemId ?: return false
        if (isCanonicalBlock(activeId)) {
            if (isCanonicalBusy()) return false
            return dayWeaveApplication.launchCanonicalAction { canonicalAction(activeId) }
        } else {
            localAction()
            return true
        }
    }

    private fun isCanonicalBusy(): Boolean =
        canonicalSyncManager.state.value.isBusy || executionSyncManager.state.value.isBusy
}

/** Terminal execution success immediately enters the application projection/recompose sequence. */
internal suspend fun finishCanonicalExecution(
    command: suspend () -> ExecutionSyncOutcome,
    refreshCanonicalState: suspend () -> Unit,
): ExecutionSyncOutcome {
    val outcome = command()
    if (outcome == ExecutionSyncOutcome.SUCCESS) refreshCanonicalState()
    return outcome
}

internal enum class ExecutionActionTarget { LOCAL, SERVER }

/** Keeps canonical work fail-closed even if a refreshed plan temporarily cannot locate its block. */
internal fun executionActionTarget(
    state: DayWeaveUiState,
    blockId: String,
): ExecutionActionTarget = if (
    state.schedule.firstOrNull { it.id == blockId }?.canonicalItemId != null ||
    (
        state.activeSession?.itemId == blockId &&
            state.activeSession.canonicalExecutionSessionId != null
        )
) {
    ExecutionActionTarget.SERVER
} else {
    ExecutionActionTarget.LOCAL
}
