package com.greengolddog.dayweave

import android.app.Application
import android.util.Log
import com.greengolddog.dayweave.network.KeystoreApiCredentialStore
import com.greengolddog.dayweave.network.OkHttpSuggestionsTransport
import com.greengolddog.dayweave.network.OkHttpCanonicalPlannerTransport
import com.greengolddog.dayweave.network.OkHttpExecutionTransport
import com.greengolddog.dayweave.data.EncryptedRoomPlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.sync.SuggestionSyncSchedulingCoordinator
import com.greengolddog.dayweave.sync.SuggestionConnectionController
import com.greengolddog.dayweave.sync.SuggestionSyncManager
import com.greengolddog.dayweave.sync.CanonicalSyncManager
import com.greengolddog.dayweave.sync.CanonicalRefreshOutcome
import com.greengolddog.dayweave.sync.CanonicalActionGate
import com.greengolddog.dayweave.sync.ExecutionSyncManager
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import com.greengolddog.dayweave.sync.WorkManagerSuggestionSyncBackend
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

class DayWeaveApplication : Application() {
    private val persistenceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val canonicalActionGate = CanonicalActionGate()

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

    val canonicalSyncManager: CanonicalSyncManager by lazy {
        CanonicalSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpCanonicalPlannerTransport(),
        )
    }

    val executionSyncManager: ExecutionSyncManager by lazy {
        ExecutionSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpExecutionTransport(),
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
            canonicalSyncManager = canonicalSyncManager,
        )
    }

    override fun onCreate() {
        super.onCreate()
        suggestionSyncSchedulingCoordinator.onAppStart()
        launchCanonicalAction { refreshCanonicalState() }
    }

    /** Canonical actions outlive a transient screen/ViewModel so responses are always reconciled. */
    fun launchCanonicalAction(action: suspend () -> Unit): Boolean {
        if (!canonicalActionGate.tryEnter()) return false
        persistenceScope.launch {
            try {
                action()
            } finally {
                canonicalActionGate.leave()
            }
        }
        return true
    }

    /** Reconciles an old/remote lease both before and after replacing today's composition. */
    suspend fun refreshCanonicalState() {
        refreshCanonicalStateSequence(
            executionRefresh = executionSyncManager::refresh,
            canonicalRefresh = canonicalSyncManager::refreshAndCompose,
        )
    }

    /** Foreground polling promotes a newly observed eligible terminal fact without periodic churn. */
    suspend fun refreshForegroundExecution() {
        refreshForegroundExecutionSequence(
            executionRefresh = executionSyncManager::refresh,
            terminalProjectionNeeded = {
                val state = plannerStore.state.value
                state.terminalExecutionOutcomes.values.any { outcome ->
                    outcome.syncOrigin == state.canonicalSyncOrigin &&
                        outcome.requiresCanonicalItemProjection &&
                        outcome.canonicalProjectionRevision == null &&
                        outcome.canonicalProjectionResolution == null &&
                        (
                            outcome.canonicalProjectionConflict == null ||
                                outcome.canonicalProjectionRetryAuthorizedAt != null
                        )
                }
            },
            canonicalRefresh = canonicalSyncManager::refreshAndCompose,
        )
    }

    private companion object {
        const val LOG_TAG = "DayWeavePersistence"
    }
}

/** Pure orchestration seam: execution truth brackets composition and its terminal projection. */
internal suspend fun refreshCanonicalStateSequence(
    executionRefresh: suspend () -> ExecutionSyncOutcome,
    canonicalRefresh: suspend () -> CanonicalRefreshOutcome,
) {
    if (executionRefresh() != ExecutionSyncOutcome.SUCCESS) return
    canonicalRefresh()
    executionRefresh()
}

/** Runs the expensive compose/projection pass only when the execution poll discovered work. */
internal suspend fun refreshForegroundExecutionSequence(
    executionRefresh: suspend () -> ExecutionSyncOutcome,
    terminalProjectionNeeded: () -> Boolean,
    canonicalRefresh: suspend () -> CanonicalRefreshOutcome,
) {
    if (executionRefresh() != ExecutionSyncOutcome.SUCCESS || !terminalProjectionNeeded()) return
    if (canonicalRefresh() == CanonicalRefreshOutcome.SUCCESS) executionRefresh()
}
