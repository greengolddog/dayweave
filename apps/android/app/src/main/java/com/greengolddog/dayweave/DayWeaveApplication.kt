package com.greengolddog.dayweave

import android.app.Application
import android.util.Log
import com.greengolddog.dayweave.network.KeystoreApiCredentialStore
import com.greengolddog.dayweave.network.OkHttpSuggestionsTransport
import com.greengolddog.dayweave.data.EncryptedRoomPlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.sync.SuggestionSyncSchedulingCoordinator
import com.greengolddog.dayweave.sync.SuggestionConnectionController
import com.greengolddog.dayweave.sync.SuggestionSyncManager
import com.greengolddog.dayweave.sync.WorkManagerSuggestionSyncBackend
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob

class DayWeaveApplication : Application() {
    private val persistenceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    val plannerStore: PlannerStore by lazy {
        PlannerStore(
            initialState = DayWeaveUiState(),
            repository = EncryptedRoomPlannerStateRepository(this),
            scope = persistenceScope,
            onPersistenceError = { error ->
                Log.e(
                    LOG_TAG,
                    "Encrypted planner persistence unavailable (${error.javaClass.simpleName})",
                )
            },
        )
    }

    private val apiCredentialStore by lazy {
        KeystoreApiCredentialStore(
            context = this,
            configuredBaseUrl = BuildConfig.DAYWEAVE_API_BASE_URL,
        )
    }

    val suggestionSyncManager: SuggestionSyncManager by lazy {
        SuggestionSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpSuggestionsTransport(),
        )
    }

    val suggestionSyncSchedulingCoordinator: SuggestionSyncSchedulingCoordinator by lazy {
        SuggestionSyncSchedulingCoordinator(
            credentialStore = apiCredentialStore,
            backend = WorkManagerSuggestionSyncBackend(this),
        )
    }

    val suggestionConnectionController: SuggestionConnectionController by lazy {
        SuggestionConnectionController(
            syncManager = suggestionSyncManager,
            schedulingCoordinator = suggestionSyncSchedulingCoordinator,
        )
    }

    override fun onCreate() {
        super.onCreate()
        suggestionSyncSchedulingCoordinator.onAppStart()
    }

    private companion object {
        const val LOG_TAG = "DayWeavePersistence"
    }
}
