package com.greengolddog.dayweave.state

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.sync.SuggestionSyncState
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

class DayWeaveViewModel(application: Application) : AndroidViewModel(application) {
    private val dayWeaveApplication = application as DayWeaveApplication
    private val plannerStore = dayWeaveApplication.plannerStore
    private val suggestionSyncManager = dayWeaveApplication.suggestionSyncManager

    val state: StateFlow<com.greengolddog.dayweave.model.DayWeaveUiState> = plannerStore.state
    val loadState: StateFlow<PlannerLoadState> = plannerStore.loadState
    val suggestionSyncState: StateFlow<SuggestionSyncState> = suggestionSyncManager.state

    init {
        viewModelScope.launch {
            val restored = loadState.first { it != PlannerLoadState.LOADING }
            if (restored == PlannerLoadState.READY) suggestionSyncManager.refreshIfNeeded()
        }
    }

    fun navigate(destination: AppDestination) = plannerStore.navigate(destination)
    fun startItem(id: String) = plannerStore.startItem(id)
    fun pauseActive(minutes: Int? = null) = plannerStore.pauseActive(minutes)
    fun resumeActive() = plannerStore.resumeActive()
    fun completeActive() = plannerStore.completeActive()
    fun skipActive() = plannerStore.skipActive()
    fun doActiveLater() = plannerStore.doActiveLater()
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
        viewModelScope.launch {
            if (suggestionSyncManager.updateConnection(baseUrl, bearerToken)) {
                suggestionSyncManager.refresh()
            }
        }
    }

    fun clearSuggestionConnection() {
        viewModelScope.launch { suggestionSyncManager.clearConnection() }
    }

    fun sendAssistantMessage(text: String): Boolean = plannerStore.sendAssistantMessage(text)
    fun toggleCompleted() = plannerStore.toggleCompleted()
    fun toggleQuietSuggestions() = plannerStore.toggleQuietSuggestions()
    fun toggleDynamicColor() = plannerStore.toggleDynamicColor()
    fun recompose() = plannerStore.recompose()
}
