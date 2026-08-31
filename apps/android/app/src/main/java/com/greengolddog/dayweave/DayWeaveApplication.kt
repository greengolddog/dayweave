package com.greengolddog.dayweave

import android.app.Application
import android.os.Build
import android.os.SystemClock
import android.util.Log
import com.greengolddog.dayweave.data.EncryptedRoomPlannerStateRepository
import com.greengolddog.dayweave.health.EnergySignalManager
import com.greengolddog.dayweave.health.HealthConnectEnergyProvider
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.network.DeviceAuthBindingFence
import com.greengolddog.dayweave.network.ApiBindingOperationGate
import com.greengolddog.dayweave.network.DurableDeviceAuthCoordinator
import com.greengolddog.dayweave.network.KeystoreDeviceAuthEnvelopeStore
import com.greengolddog.dayweave.network.OkHttpCanonicalPlannerTransport
import com.greengolddog.dayweave.network.OkHttpDeviceAuthTransport
import com.greengolddog.dayweave.network.OkHttpExecutionTransport
import com.greengolddog.dayweave.network.OkHttpGoogleAccountsTransport
import com.greengolddog.dayweave.network.OkHttpProposalApplicationsTransport
import com.greengolddog.dayweave.network.OkHttpSuggestionsTransport
import com.greengolddog.dayweave.notifications.TimedBreakNotificationCoordinator
import com.greengolddog.dayweave.notifications.TimedBreakNotificationRouteMailbox
import com.greengolddog.dayweave.notifications.WorkManagerTimedBreakNotificationBackend
import com.greengolddog.dayweave.notifications.cancelTimedBreakNotificationAndRestoreOnFailure
import com.greengolddog.dayweave.notifications.ensureTimedBreakNotificationChannel
import com.greengolddog.dayweave.notifications.reconcileTimedBreakNotificationStates
import com.greengolddog.dayweave.security.AppAuthenticationProcessFence
import com.greengolddog.dayweave.security.AppLockController
import com.greengolddog.dayweave.security.AtomicFileAppLockSettingsStore
import com.greengolddog.dayweave.security.MonotonicClock
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.sync.CanonicalActionGate
import com.greengolddog.dayweave.sync.CanonicalRefreshOutcome
import com.greengolddog.dayweave.sync.CanonicalSyncManager
import com.greengolddog.dayweave.sync.ExecutionSyncManager
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import com.greengolddog.dayweave.sync.GoogleAccountManager
import com.greengolddog.dayweave.sync.ProposalApplicationManager
import com.greengolddog.dayweave.sync.SuggestionSyncManager
import com.greengolddog.dayweave.sync.SuggestionSyncSchedulingCoordinator
import com.greengolddog.dayweave.sync.WorkManagerSuggestionSyncBackend
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.flow.MutableSharedFlow

class DayWeaveApplication : Application() {
    private val persistenceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val canonicalActionGate = CanonicalActionGate()

    val appAuthenticationProcessFence = AppAuthenticationProcessFence()

    /** Trusted notification routes survive lock/task/process boundaries without public extras. */
    internal val timedBreakNotificationRoutes: TimedBreakNotificationRouteMailbox by lazy {
        TimedBreakNotificationRouteMailbox(this)
    }

    val appLockController: AppLockController by lazy {
        AppLockController(
            settingsStore = AtomicFileAppLockSettingsStore(this),
            clock = MonotonicClock(SystemClock::elapsedRealtime),
        )
    }

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

    private val deviceAuthEnvelopeStore by lazy {
        KeystoreDeviceAuthEnvelopeStore(
            context = this,
            configuredBaseUrl = BuildConfig.DAYWEAVE_API_BASE_URL,
        )
    }

    private val apiBindingOperationGate = ApiBindingOperationGate()

    internal val deviceAuthCoordinator: DurableDeviceAuthCoordinator by lazy {
        DurableDeviceAuthCoordinator(
            store = deviceAuthEnvelopeStore,
            transport = OkHttpDeviceAuthTransport(),
            clientVersion = BuildConfig.VERSION_NAME,
            deviceLabel = listOf(Build.MANUFACTURER, Build.MODEL)
                .map(String::trim)
                .filter(String::isNotBlank)
                .distinct()
                .joinToString(" ")
                .take(200)
                .ifBlank { "Personal Android device" },
            bindingOperationGate = apiBindingOperationGate,
            bindingFence = object : DeviceAuthBindingFence {
                private suspend fun quarantineBinding(
                    allowAmbiguousJournal: Boolean,
                ): Boolean {
                    val loaded = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
                    if (
                        loaded != PlannerLoadState.READY ||
                        !allowAmbiguousJournal && plannerStore.hasCredentialReplacementBlocker()
                    ) {
                        return false
                    }
                    if (!cancelTimedBreakNotificationForAuthoritativeTransition()) return false
                    val quarantined = try {
                        plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
                    } finally {
                        // A failed quarantine restores the old durable lease's reminder; a
                        // successful one confirms the cancellation against empty canonical state.
                        reconcileTimedBreakNotificationAfterAuthoritativeTransition()
                    }
                    if (quarantined) {
                        if (suggestionSyncManagerDelegate.isInitialized()) {
                            suggestionSyncManager.quarantineBindingState()
                        }
                        if (proposalApplicationManagerDelegate.isInitialized()) {
                            proposalApplicationManager.quarantineBindingState()
                        }
                        if (canonicalSyncManagerDelegate.isInitialized()) {
                            canonicalSyncManager.quarantineBindingState()
                        }
                        if (executionSyncManagerDelegate.isInitialized()) {
                            executionSyncManager.quarantineBindingState()
                        }
                        if (googleAccountManagerDelegate.isInitialized()) {
                            googleAccountManager.quarantineBindingState()
                        }
                    }
                    return quarantined
                }

                override suspend fun beforeBindingChange(
                    previousBaseUrl: String?,
                    previousBindingId: String?,
                    nextBaseUrl: String?,
                    nextBindingId: String?,
                ): Boolean = quarantineBinding(allowAmbiguousJournal = false)

                override suspend fun beforeConfirmedLocalDestruction(
                    previousBaseUrl: String?,
                    previousBindingId: String?,
                ): Boolean = quarantineBinding(allowAmbiguousJournal = true)
            },
        )
    }

    private val apiCredentialStore by lazy { deviceAuthCoordinator }

    private val suggestionSyncManagerDelegate = lazy {
        SuggestionSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpSuggestionsTransport(),
        )
    }
    val suggestionSyncManager: SuggestionSyncManager get() = suggestionSyncManagerDelegate.value

    private val proposalApplicationManagerDelegate = lazy {
        ProposalApplicationManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpProposalApplicationsTransport(),
        )
    }
    val proposalApplicationManager: ProposalApplicationManager
        get() = proposalApplicationManagerDelegate.value

    private val canonicalSyncManagerDelegate = lazy {
        CanonicalSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpCanonicalPlannerTransport(),
            cancelTimedBreakNotification =
                ::cancelTimedBreakNotificationForAuthoritativeTransition,
            reconcileTimedBreakNotification =
                ::reconcileTimedBreakNotificationAfterAuthoritativeTransition,
        )
    }
    val canonicalSyncManager: CanonicalSyncManager get() = canonicalSyncManagerDelegate.value

    private val executionSyncManagerDelegate = lazy {
        ExecutionSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpExecutionTransport(),
            cancelTimedBreakNotification =
                ::cancelTimedBreakNotificationForAuthoritativeTransition,
            reconcileTimedBreakNotification =
                ::reconcileTimedBreakNotificationAfterAuthoritativeTransition,
        )
    }
    val executionSyncManager: ExecutionSyncManager get() = executionSyncManagerDelegate.value

    private val googleAccountManagerDelegate = lazy {
        GoogleAccountManager(
            credentialStore = apiCredentialStore,
            transport = OkHttpGoogleAccountsTransport(),
        )
    }
    val googleAccountManager: GoogleAccountManager get() = googleAccountManagerDelegate.value

    val energySignalManager: EnergySignalManager by lazy {
        EnergySignalManager(
            provider = HealthConnectEnergyProvider(this),
            plannerStore = plannerStore,
        )
    }

    val suggestionSyncSchedulingCoordinator: SuggestionSyncSchedulingCoordinator by lazy {
        SuggestionSyncSchedulingCoordinator(
            credentialStore = apiCredentialStore,
            backend = WorkManagerSuggestionSyncBackend(this),
        )
    }

    private val timedBreakNotificationCoordinator: TimedBreakNotificationCoordinator by lazy {
        TimedBreakNotificationCoordinator(
            backend = WorkManagerTimedBreakNotificationBackend(this),
        )
    }
    private val timedBreakNotificationReconcileRequests = MutableSharedFlow<Unit>(
        extraBufferCapacity = 1,
    )

    internal suspend fun cancelTimedBreakNotificationForAuthoritativeTransition(): Boolean =
        cancelTimedBreakNotificationAndRestoreOnFailure(
            coordinator = timedBreakNotificationCoordinator,
            unchangedDurableState = plannerStore.durableState.value,
            queueReconciliationRetry = {
                timedBreakNotificationReconcileRequests.emit(Unit)
            },
        )

    internal suspend fun reconcileTimedBreakNotificationAfterAuthoritativeTransition() {
        val durable = plannerStore.durableState.value ?: return
        if (!timedBreakNotificationCoordinator.reconcile(durable)) {
            timedBreakNotificationReconcileRequests.emit(Unit)
        }
    }

    override fun onCreate() {
        super.onCreate()
        ensureTimedBreakNotificationChannel(this)
        persistenceScope.launch {
            reconcileTimedBreakNotificationStates(
                durableStates = merge(
                    plannerStore.durableState.filterNotNull(),
                    timedBreakNotificationReconcileRequests.mapNotNull {
                        plannerStore.durableState.value
                    },
                ),
                coordinator = timedBreakNotificationCoordinator,
            )
        }
        persistenceScope.launch {
            deviceAuthCoordinator.recoverPendingOrUpgradeLegacy()
            suggestionSyncSchedulingCoordinator.onAppStart()
            if (deviceAuthCoordinator.snapshot().hasBearerToken) {
                launchCanonicalAction { refreshCanonicalState() }
            }
        }
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

    /** Clears memory-only proposal review content whenever locked UI becomes authoritative. */
    fun onAppPrivacyBoundaryLocked() {
        if (proposalApplicationManagerDelegate.isInitialized()) {
            proposalApplicationManager.discardReviewForPrivacyBoundary()
        }
    }

    /** Reconciles an old/remote lease both before and after replacing today's composition. */
    suspend fun refreshCanonicalState() {
        proposalApplicationManager.recoverPending()
        if (plannerStore.state.value.pendingProposalApplicationMutation != null) return
        refreshCanonicalStateSequence(
            executionRefresh = executionSyncManager::refresh,
            canonicalRefresh = canonicalSyncManager::refreshAndCompose,
        )
    }

    /** Foreground polling promotes a newly observed eligible terminal fact without periodic churn. */
    suspend fun refreshForegroundExecution() {
        refreshForegroundExecutionSequence(
            executionRefresh = executionSyncManager::refresh,
            canonicalRefreshNeeded = {
                val state = plannerStore.state.value
                val terminalProjectionNeeded = state.terminalExecutionOutcomes.values.any { outcome ->
                    outcome.syncOrigin == state.canonicalSyncOrigin &&
                        outcome.session.status in CANONICAL_TERMINAL_EXECUTION_STATUSES &&
                        state.isNewestExecutionForProjection(outcome.session) &&
                        outcome.requiresCanonicalItemProjection &&
                        outcome.canonicalProjectionRevision == null &&
                        outcome.canonicalProjectionResolution == null &&
                        (
                            outcome.canonicalProjectionConflict == null ||
                                outcome.canonicalProjectionRetryAuthorizedAt != null
                        )
                }
                terminalProjectionNeeded || state.deferredExecutionRecompositionNeeded()
            },
            canonicalRefresh = canonicalSyncManager::refreshAndCompose,
        )
    }

    private companion object {
        const val LOG_TAG = "DayWeavePersistence"
        val CANONICAL_TERMINAL_EXECUTION_STATUSES = setOf("completed", "skipped")
    }
}

/** Pure orchestration seam: execution truth brackets composition and its terminal projection. */
internal suspend fun refreshCanonicalStateSequence(
    executionRefresh: suspend () -> ExecutionSyncOutcome,
    canonicalRefresh: suspend () -> CanonicalRefreshOutcome,
) {
    if (executionRefresh() !in EXECUTION_REFRESH_SUCCESSES) return
    canonicalRefresh()
    executionRefresh()
}

/** Runs the expensive compose/projection pass only when the execution poll discovered work. */
internal suspend fun refreshForegroundExecutionSequence(
    executionRefresh: suspend () -> ExecutionSyncOutcome,
    canonicalRefreshNeeded: () -> Boolean,
    canonicalRefresh: suspend () -> CanonicalRefreshOutcome,
) {
    if (executionRefresh() !in EXECUTION_REFRESH_SUCCESSES || !canonicalRefreshNeeded()) return
    if (canonicalRefresh() == CanonicalRefreshOutcome.SUCCESS) executionRefresh()
}

private val EXECUTION_REFRESH_SUCCESSES = setOf(
    ExecutionSyncOutcome.SUCCESS,
    ExecutionSyncOutcome.RECOVERED_COMMAND,
)

/** A remote Defer must replace and republish its exact source block before execution can restart. */
internal fun DayWeaveUiState.deferredExecutionRecompositionNeeded(): Boolean {
    val currentOrigin = canonicalSyncOrigin ?: return false
    return terminalExecutionOutcomes.values.any { outcome ->
        val session = outcome.session
        val sourceBlockId = session.plannedBlockId
        outcome.syncOrigin == currentOrigin && session.status == "deferred" &&
            sourceBlockId != null && (
                schedule.any { block ->
                    block.id == sourceBlockId &&
                        block.canonicalItemId == session.itemId &&
                        block.canonicalRevision == session.itemRevision &&
                        block.occurrenceId == session.occurrenceId &&
                        block.sessionIndex == session.sessionIndex
                } || publishedScheduleProof?.blocks?.any { block ->
                    block.id == sourceBlockId && block.itemId == session.itemId &&
                        block.itemRevision == session.itemRevision &&
                        block.occurrenceId == session.occurrenceId &&
                        block.sessionIndex == session.sessionIndex
                } == true
            )
    }
}
