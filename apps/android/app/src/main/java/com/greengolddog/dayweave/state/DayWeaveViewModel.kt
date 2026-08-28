package com.greengolddog.dayweave.state

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import com.greengolddog.dayweave.DayWeaveApplication
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ItemKind
import kotlinx.coroutines.flow.StateFlow

class DayWeaveViewModel(application: Application) : AndroidViewModel(application) {
    private val plannerStore = (application as DayWeaveApplication).plannerStore

    val state: StateFlow<com.greengolddog.dayweave.model.DayWeaveUiState> = plannerStore.state
    val loadState: StateFlow<PlannerLoadState> = plannerStore.loadState

    fun navigate(destination: AppDestination) = plannerStore.navigate(destination)
    fun startItem(id: String) = plannerStore.startItem(id)
    fun pauseActive(minutes: Int? = null) = plannerStore.pauseActive(minutes)
    fun resumeActive() = plannerStore.resumeActive()
    fun completeActive() = plannerStore.completeActive()
    fun skipActive() = plannerStore.skipActive()
    fun doActiveLater() = plannerStore.doActiveLater()
    fun quickCapture(title: String, kind: ItemKind): Boolean = plannerStore.quickCapture(title, kind)
    fun approveSuggestion(id: String) = plannerStore.approveSuggestion(id)
    fun rejectSuggestion(id: String) = plannerStore.rejectSuggestion(id)
    fun updateSuggestion(id: String, title: String, summary: String) =
        plannerStore.updateSuggestion(id, title, summary)

    fun sendAssistantMessage(text: String): Boolean = plannerStore.sendAssistantMessage(text)
    fun toggleCompleted() = plannerStore.toggleCompleted()
    fun toggleQuietSuggestions() = plannerStore.toggleQuietSuggestions()
    fun toggleDynamicColor() = plannerStore.toggleDynamicColor()
    fun recompose() = plannerStore.recompose()
}
