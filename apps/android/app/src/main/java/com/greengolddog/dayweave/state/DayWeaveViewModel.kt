package com.greengolddog.dayweave.state

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncState
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

    val state: StateFlow<com.greengolddog.dayweave.model.DayWeaveUiState> = plannerStore.state
    val loadState: StateFlow<PlannerLoadState> = plannerStore.loadState
    val suggestionSyncState: StateFlow<SuggestionSyncState> = suggestionSyncManager.state
    val canonicalSyncState: StateFlow<CanonicalSyncState> = canonicalSyncManager.state

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
            if (canonicalSyncManager.state.value.isBusy) return
            dayWeaveApplication.launchCanonicalAction { canonicalSyncManager.start(id) }
        } else {
            plannerStore.startItem(id)
        }
    }

    fun pauseActive(minutes: Int? = null) {
        withActiveBlock(
            canonicalAction = { id -> canonicalSyncManager.pause(id, minutes) },
            localAction = { plannerStore.pauseActive(minutes) },
        )
    }

    fun resumeActive() {
        resumeActiveIfAvailable()
    }

    private fun resumeActiveIfAvailable(): Boolean =
        withActiveBlock(
            canonicalAction = canonicalSyncManager::resume,
            localAction = plannerStore::resumeActive,
        )

    fun completeActive() {
        withActiveBlock(
            canonicalAction = canonicalSyncManager::complete,
            localAction = plannerStore::completeActive,
        )
    }

    fun skipActive() {
        withActiveBlock(
            canonicalAction = canonicalSyncManager::skip,
            localAction = plannerStore::skipActive,
        )
    }

    fun doActiveLater() {
        withActiveBlock(
            canonicalAction = canonicalSyncManager::doLater,
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
                canonicalSyncManager.refreshAndCompose()
            }
        }
    }

    fun clearSuggestionConnection() {
        dayWeaveApplication.launchCanonicalAction {
            if (suggestionConnectionController.forget()) {
                canonicalSyncManager.refreshAndCompose()
            }
        }
    }

    fun sendAssistantMessage(text: String): Boolean = plannerStore.sendAssistantMessage(text)
    fun toggleCompleted() = plannerStore.toggleCompleted()
    fun toggleQuietSuggestions() = plannerStore.toggleQuietSuggestions()
    fun toggleDynamicColor() = plannerStore.toggleDynamicColor()
    fun recompose() {
        if (canonicalSyncManager.state.value.isBusy) return
        dayWeaveApplication.launchCanonicalAction { canonicalSyncManager.refreshAndCompose() }
    }

    private fun isCanonicalBlock(id: String): Boolean =
        plannerStore.state.value.schedule.firstOrNull { it.id == id }?.canonicalItemId != null

    private fun withActiveBlock(
        canonicalAction: suspend (String) -> Unit,
        localAction: () -> Unit,
    ): Boolean {
        val activeId = plannerStore.state.value.activeSession?.itemId ?: return false
        if (isCanonicalBlock(activeId)) {
            if (canonicalSyncManager.state.value.isBusy) return false
            return dayWeaveApplication.launchCanonicalAction { canonicalAction(activeId) }
        } else {
            localAction()
            return true
        }
    }
}
