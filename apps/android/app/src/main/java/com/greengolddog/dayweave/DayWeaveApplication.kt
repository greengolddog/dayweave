package com.greengolddog.dayweave

import android.app.Application
import android.os.Build
import android.os.SystemClock
import android.util.Log
import com.greengolddog.dayweave.data.EncryptedRoomPlannerStateRepository
import com.greengolddog.dayweave.health.EnergySignalManager
import com.greengolddog.dayweave.health.EnergySignalGenerationFence
import com.greengolddog.dayweave.health.HealthConnectEnergyProvider
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStage
import com.greengolddog.dayweave.model.isNewestExecutionForProjection
import com.greengolddog.dayweave.network.DeviceAuthBindingFence
import com.greengolddog.dayweave.network.ApiBindingOperationGate
import com.greengolddog.dayweave.network.DurableDeviceAuthCoordinator
import com.greengolddog.dayweave.network.KeystoreDeviceAuthEnvelopeStore
import com.greengolddog.dayweave.network.OkHttpAssistantTransport
import com.greengolddog.dayweave.network.OkHttpCanonicalPlannerTransport
import com.greengolddog.dayweave.network.OkHttpCanonicalItemInvalidationStreamTransport
import com.greengolddog.dayweave.network.OkHttpDeviceAuthTransport
import com.greengolddog.dayweave.network.OkHttpExecutionInvalidationStreamTransport
import com.greengolddog.dayweave.network.OkHttpScheduleInvalidationStreamTransport
import com.greengolddog.dayweave.network.OkHttpExecutionTransport
import com.greengolddog.dayweave.network.OkHttpGoogleAccountsTransport
import com.greengolddog.dayweave.network.OkHttpGoogleCalendarInboundTransport
import com.greengolddog.dayweave.network.OkHttpGoogleCalendarOutboundTransport
import com.greengolddog.dayweave.network.OkHttpHabitInvalidationStreamTransport
import com.greengolddog.dayweave.network.OkHttpHabitTransport
import com.greengolddog.dayweave.network.OkHttpProposalApplicationsTransport
import com.greengolddog.dayweave.network.OkHttpSuggestionsTransport
import com.greengolddog.dayweave.notifications.TimedBreakNotificationCoordinator
import com.greengolddog.dayweave.notifications.TimedBreakNotificationRouteMailbox
import com.greengolddog.dayweave.notifications.WorkManagerTimedBreakNotificationBackend
import com.greengolddog.dayweave.notifications.cancelTimedBreakNotificationAndRestoreOnFailure
import com.greengolddog.dayweave.notifications.ensureTimedBreakNotificationChannel
import com.greengolddog.dayweave.notifications.reconcileTimedBreakNotificationStates
import com.greengolddog.dayweave.onboarding.AtomicFileOnboardingCheckpointStore
import com.greengolddog.dayweave.onboarding.OnboardingConsentBootstrap
import com.greengolddog.dayweave.onboarding.OnboardingController
import com.greengolddog.dayweave.onboarding.OnboardingControllerState
import com.greengolddog.dayweave.onboarding.OnboardingCorruptArtifactIdentity
import com.greengolddog.dayweave.onboarding.OnboardingRuntimeGate
import com.greengolddog.dayweave.security.AppAuthenticationProcessFence
import com.greengolddog.dayweave.security.AppLockController
import com.greengolddog.dayweave.security.AtomicFileAppLockSettingsStore
import com.greengolddog.dayweave.security.MonotonicClock
import com.greengolddog.dayweave.scheduler.RustScheduleComposer
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.sync.AssistantManager
import com.greengolddog.dayweave.sync.CanonicalActionGate
import com.greengolddog.dayweave.sync.CanonicalRefreshOutcome
import com.greengolddog.dayweave.sync.CanonicalSyncManager
import com.greengolddog.dayweave.sync.AtomicGoogleAuthorizationJournalStore
import com.greengolddog.dayweave.sync.AtomicGoogleCalendarImportJournalStore
import com.greengolddog.dayweave.sync.DurableCanonicalItemInvalidationCursor
import com.greengolddog.dayweave.sync.ExecutionSyncManager
import com.greengolddog.dayweave.sync.ExecutionSyncOutcome
import com.greengolddog.dayweave.sync.DurableExecutionInvalidationCursor
import com.greengolddog.dayweave.sync.ForegroundExecutionInvalidationManager
import com.greengolddog.dayweave.sync.ForegroundCanonicalItemInvalidationManager
import com.greengolddog.dayweave.sync.DurableScheduleInvalidationCursor
import com.greengolddog.dayweave.sync.DurableHabitInvalidationCursor
import com.greengolddog.dayweave.sync.ForegroundScheduleInvalidationManager
import com.greengolddog.dayweave.sync.ForegroundHabitInvalidationManager
import com.greengolddog.dayweave.sync.GoogleAccountManager
import com.greengolddog.dayweave.sync.GoogleAuthorizationAction
import com.greengolddog.dayweave.sync.GoogleAuthorizationJournalLoadResult
import com.greengolddog.dayweave.sync.GoogleCalendarImportCompletionPipeline
import com.greengolddog.dayweave.sync.GoogleCalendarImportCoordinator
import com.greengolddog.dayweave.sync.GoogleCalendarImportJournalLoadResult
import com.greengolddog.dayweave.sync.GoogleCalendarImportPersistenceReceipt
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundCoordinator
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationCoordinator
import com.greengolddog.dayweave.sync.HabitSyncManager
import com.greengolddog.dayweave.sync.HabitSyncOutcome
import com.greengolddog.dayweave.sync.LocalScheduleCompositionLauncher
import com.greengolddog.dayweave.sync.ProposalApplicationManager
import com.greengolddog.dayweave.sync.SuggestionSyncManager
import com.greengolddog.dayweave.sync.SuggestionSyncSchedulingCoordinator
import com.greengolddog.dayweave.sync.WorkManagerSuggestionSyncBackend
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateCoordinator
import com.greengolddog.dayweave.state.ScheduleCompositionProfileDraftMemory
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.flow.MutableSharedFlow

class DayWeaveApplication : Application() {
    private val persistenceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val canonicalActionGate = CanonicalActionGate()
    private val privatePresentationAllowed = AtomicBoolean(false)
    private val assistantForegroundActive = AtomicBoolean(false)
    private val energySignalGenerationFence = ApplicationEnergySignalGenerationFence()
    private val consentBoundaryReconciliationActive = AtomicBoolean(false)
    val onboardingController: OnboardingController by lazy {
        OnboardingController(AtomicFileOnboardingCheckpointStore(this))
    }
    private val onboardingRuntimeGate: OnboardingRuntimeGate by lazy {
        OnboardingRuntimeGate(
            privacyAcknowledged =
                (onboardingController.state as? OnboardingControllerState.Active)
                    ?.let { it.privacyAcknowledged && it.privacyReleaseCompleted } == true,
        )
    }
    val onboardingRuntimePrivacyState
        get() = onboardingRuntimeGate.state
    private val onboardingConsentBootstrap by lazy {
        OnboardingConsentBootstrap(::launchConsentDependentServices)
    }
    private val googleAuthorizationJournalStore by lazy {
        AtomicGoogleAuthorizationJournalStore(this)
    }
    private val googleCalendarImportJournalStore by lazy {
        AtomicGoogleCalendarImportJournalStore(this)
    }
    private val localScheduleCompositionLauncher = LocalScheduleCompositionLauncher(
        scope = persistenceScope,
        actionGate = canonicalActionGate,
        compose = { generation -> canonicalSyncManager.composeLocally(generation) },
    )
    internal val scheduleCompositionProfileUpdateCoordinator by lazy {
        ScheduleCompositionProfileUpdateCoordinator(
            plannerStore = plannerStore,
            launchCanonicalAction = ::launchCanonicalAction,
        )
    }

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
                        !allowAmbiguousJournal && (
                            plannerStore.hasCredentialReplacementBlocker() ||
                                hasGoogleAuthorizationRecoveryBlocker() ||
                                hasGoogleCalendarImportRecoveryBlocker()
                        )
                    ) {
                        return false
                    }
                    if (!cancelTimedBreakNotificationForAuthoritativeTransition()) return false
                    cancelAndDrainLocalScheduleComposition()
                    if (canonicalItemInvalidationManagerDelegate.isInitialized()) {
                        canonicalItemInvalidationManager.cancelAndDrainActiveSession()
                    }
                    if (executionInvalidationManagerDelegate.isInitialized()) {
                        executionInvalidationManager.cancelAndDrainActiveSession()
                    }
                    if (scheduleInvalidationManagerDelegate.isInitialized()) {
                        scheduleInvalidationManager.cancelAndDrainActiveSession()
                    }
                    if (habitInvalidationManagerDelegate.isInitialized()) {
                        habitInvalidationManager.cancelAndDrainActiveSession()
                    }
                    val quarantined = try {
                        plannerStore.abandonCanonicalConnection()?.awaitDurable() == true
                    } finally {
                        // A failed quarantine restores the old durable lease's reminder; a
                        // successful one confirms the cancellation against empty canonical state.
                        reconcileTimedBreakNotificationAfterAuthoritativeTransition()
                    }
                    if (
                        quarantined && allowAmbiguousJournal &&
                        !googleCalendarImportCoordinator
                            .abandonPendingForConfirmedLocalDestruction()
                    ) {
                        // Keep the credentials when the final destructive recovery fence fails.
                        // Planner abandonment is idempotent, so an explicit retry remains safe.
                        return false
                    }
                    if (
                        quarantined && allowAmbiguousJournal &&
                        !googleAccountManager.abandonAuthorizationForConfirmedLocalDestruction()
                    ) {
                        // The credential writer must not strand an old OAuth ceremony behind a
                        // new binding. Retain the credentials until this explicit cleanup works.
                        return false
                    }
                    if (quarantined) {
                        if (suggestionSyncManagerDelegate.isInitialized()) {
                            suggestionSyncManager.quarantineBindingState()
                        }
                        if (assistantManagerDelegate.isInitialized()) {
                            assistantManager.quarantineBindingState()
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
                        if (habitSyncManagerDelegate.isInitialized()) {
                            habitSyncManager.quarantineBindingState()
                        }
                        if (googleAccountManagerDelegate.isInitialized()) {
                            googleAccountManager.quarantineBindingState()
                        }
                        googleCalendarImportCoordinator.quarantineBindingState()
                        if (googleCalendarOutboundCoordinatorDelegate.isInitialized()) {
                            googleCalendarOutboundCoordinator.quarantineBindingState()
                        }
                        if (googleSchedulePublicationCoordinatorDelegate.isInitialized()) {
                            googleSchedulePublicationCoordinator.quarantineBindingState()
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

    private val assistantManagerDelegate = lazy {
        AssistantManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpAssistantTransport(),
            scope = persistenceScope,
            operationAllowed = {
                privatePresentationAllowed.get() && assistantForegroundActive.get()
            },
        )
    }
    val assistantManager: AssistantManager get() = assistantManagerDelegate.value

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

    private val canonicalPlannerTransport by lazy { OkHttpCanonicalPlannerTransport() }

    private val canonicalSyncManagerDelegate = lazy {
        CanonicalSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = canonicalPlannerTransport,
            localScheduleComposer = RustScheduleComposer(),
            localCompositionLifecycleFence = localScheduleCompositionLauncher,
            cancelTimedBreakNotification =
                ::cancelTimedBreakNotificationForAuthoritativeTransition,
            reconcileTimedBreakNotification =
                ::reconcileTimedBreakNotificationAfterAuthoritativeTransition,
        )
    }
    val canonicalSyncManager: CanonicalSyncManager get() = canonicalSyncManagerDelegate.value

    private val canonicalItemInvalidationManagerDelegate = lazy {
        ForegroundCanonicalItemInvalidationManager(
            credentialStore = apiCredentialStore,
            plannerTransport = canonicalPlannerTransport,
            streamTransport = OkHttpCanonicalItemInvalidationStreamTransport(),
            durableCursor = {
                val durable = plannerStore.durableState.value
                DurableCanonicalItemInvalidationCursor(
                    syncOrigin = durable?.canonicalSyncOrigin,
                    configurationId = durable?.canonicalConfigurationId,
                    cursor = durable?.canonicalDeltaCursor,
                )
            },
            tryLaunchAuthoritativeRefresh = ::launchCanonicalAction,
            authoritativeRefresh = {
                refreshCanonicalState() == CanonicalRefreshOutcome.SUCCESS
            },
        )
    }
    private val canonicalItemInvalidationManager: ForegroundCanonicalItemInvalidationManager
        get() = canonicalItemInvalidationManagerDelegate.value

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

    private val habitSyncManagerDelegate = lazy {
        HabitSyncManager(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpHabitTransport(),
        )
    }
    val habitSyncManager: HabitSyncManager get() = habitSyncManagerDelegate.value

    private val habitInvalidationManagerDelegate = lazy {
        ForegroundHabitInvalidationManager(
            credentialStore = apiCredentialStore,
            streamTransport = OkHttpHabitInvalidationStreamTransport(),
            durableCursor = {
                val ledger = plannerStore.durableState.value?.habitLedger
                DurableHabitInvalidationCursor(
                    syncOrigin = ledger?.syncOrigin,
                    configurationId = ledger?.configurationId,
                    cursor = ledger?.deltaCursor,
                )
            },
            tryLaunchAuthoritativeRefresh = ::launchCanonicalResultAction,
            authoritativeRefresh = {
                habitSyncManager.refresh() in HABIT_REFRESH_COMPOSE_SAFE_OUTCOMES
            },
        )
    }
    private val habitInvalidationManager: ForegroundHabitInvalidationManager
        get() = habitInvalidationManagerDelegate.value

    private val executionInvalidationManagerDelegate = lazy {
        ForegroundExecutionInvalidationManager(
            credentialStore = apiCredentialStore,
            streamTransport = OkHttpExecutionInvalidationStreamTransport(),
            durableCursor = {
                val durable = plannerStore.durableState.value
                DurableExecutionInvalidationCursor(
                    syncOrigin = durable?.canonicalExecutionSyncOrigin,
                    configurationId = durable?.canonicalExecutionConfigurationId,
                    revision = durable?.canonicalExecutionRevision ?: 0,
                )
            },
            tryLaunchAuthoritativeRefresh = ::launchCanonicalAction,
            authoritativeRefresh = ::refreshForegroundExecution,
        )
    }
    private val executionInvalidationManager: ForegroundExecutionInvalidationManager
        get() = executionInvalidationManagerDelegate.value

    private val scheduleInvalidationManagerDelegate = lazy {
        ForegroundScheduleInvalidationManager(
            credentialStore = apiCredentialStore,
            streamTransport = OkHttpScheduleInvalidationStreamTransport(),
            durableCursor = {
                val durable = plannerStore.durableState.value
                val syncOrigin = durable?.canonicalSyncOrigin
                val configurationId = durable?.canonicalConfigurationId
                val hint = durable?.publishedScheduleRevisionHint?.takeIf { candidate ->
                    candidate.hasValidShape() && candidate.syncOrigin == syncOrigin &&
                        candidate.configurationId == configurationId
                }
                val proof = durable?.let { snapshot ->
                    snapshot.publishedOccurrenceMembershipProof?.takeIf { candidate ->
                        candidate.hasValidShape() &&
                            candidate.syncOrigin == syncOrigin &&
                            candidate.configurationId == configurationId &&
                            hint?.revisionNumber?.let { it >= candidate.revision.revisionNumber } ==
                            true
                    }
                }
                val installedRevision = maxOf(
                    proof?.revision?.revisionNumber ?: 0uL,
                    durable?.publishedScheduleRevision?.revisionNumber ?: 0uL,
                )
                DurableScheduleInvalidationCursor(
                    syncOrigin = syncOrigin,
                    configurationId = configurationId,
                    revision = installedRevision,
                    latestObservedRevision = maxOf(
                        installedRevision,
                        hint?.revisionNumber ?: 0uL,
                    ),
                )
            },
            recordRevisionHint = { syncOrigin, configurationId, revision ->
                val durableHint = plannerStore.durableState.value
                    ?.publishedScheduleRevisionHint
                if (
                    durableHint?.syncOrigin == syncOrigin &&
                    durableHint.configurationId == configurationId &&
                    durableHint.revisionNumber >= revision
                ) {
                    true
                } else {
                    plannerStore.recordPublishedScheduleRevisionHint(
                        syncOrigin = syncOrigin,
                        configurationId = configurationId,
                        revisionNumber = revision,
                    )?.awaitDurable() == true
                }
            },
            tryLaunchAuthoritativeRefresh = ::launchCanonicalAction,
            authoritativeRefresh = { epochResetFence ->
                val outcome = if (epochResetFence == null) {
                    canonicalSyncManager.refreshCurrentPublishedSchedule()
                } else {
                    canonicalSyncManager.refreshCurrentPublishedScheduleAfterCursorReset(
                        epochResetFence,
                    )
                }
                outcome == CanonicalRefreshOutcome.SUCCESS
            },
        )
    }
    private val scheduleInvalidationManager: ForegroundScheduleInvalidationManager
        get() = scheduleInvalidationManagerDelegate.value

    private val googleAccountManagerDelegate = lazy {
        GoogleAccountManager(
            credentialStore = apiCredentialStore,
            transport = OkHttpGoogleAccountsTransport(),
            authorizationJournalStore = googleAuthorizationJournalStore,
            operationAllowed = privatePresentationAllowed::get,
            authorizationMutationAllowed = ::googleAuthorizationMutationAllowed,
        )
    }
    val googleAccountManager: GoogleAccountManager get() = googleAccountManagerDelegate.value

    private val googleCalendarImportCoordinatorDelegate = lazy {
        GoogleCalendarImportCoordinator(
            credentialStore = apiCredentialStore,
            transport = OkHttpGoogleCalendarInboundTransport(),
            journalStore = googleCalendarImportJournalStore,
            completionPipeline = GoogleCalendarImportCompletionPipeline { input ->
                val outcome = refreshCanonicalState()
                val durable = plannerStore.durableState.value
                val durableProof = durable?.publishedScheduleProof?.takeIf { proof ->
                    proof.matchesCurrentStateAndPlan(durable)
                }
                GoogleCalendarImportPersistenceReceipt(
                    configurationId = input.configurationId,
                    apiBaseUrl = input.apiBaseUrl,
                    accountId = input.accountId,
                    completedRefreshGeneration = input.acceptedRefreshGeneration,
                    durablyPersisted = durableProof != null &&
                        outcome == CanonicalRefreshOutcome.SUCCESS &&
                        durableProof.configurationId == input.configurationId &&
                        durableProof.syncOrigin == input.apiBaseUrl,
                )
            },
            operationAllowed = {
                privatePresentationAllowed.get() &&
                    !googleAccountManager.hasAuthorizationRecoveryBlocker()
            },
            importAllowed = {
                plannerStore.state.value.pendingGoogleCalendarOutbound == null &&
                    plannerStore.state.value.pendingGoogleSchedulePublication == null
            },
        )
    }
    val googleCalendarImportCoordinator: GoogleCalendarImportCoordinator
        get() = googleCalendarImportCoordinatorDelegate.value

    private val googleCalendarOutboundCoordinatorDelegate = lazy {
        GoogleCalendarOutboundCoordinator(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpGoogleCalendarOutboundTransport(),
            googleAccountState = { googleAccountManager.state.value },
            googleImportState = { googleCalendarImportCoordinator.state.value },
            operationAllowed = {
                privatePresentationAllowed.get() &&
                    !googleAccountManager.hasAuthorizationRecoveryBlocker()
            },
        )
    }
    val googleCalendarOutboundCoordinator: GoogleCalendarOutboundCoordinator
        get() = googleCalendarOutboundCoordinatorDelegate.value

    private val googleSchedulePublicationCoordinatorDelegate = lazy {
        GoogleSchedulePublicationCoordinator(
            plannerStore = plannerStore,
            credentialStore = apiCredentialStore,
            transport = OkHttpGoogleCalendarOutboundTransport(),
            googleAccountState = { googleAccountManager.state.value },
            googleImportState = { googleCalendarImportCoordinator.state.value },
            operationAllowed = {
                privatePresentationAllowed.get() &&
                    !googleAccountManager.hasAuthorizationRecoveryBlocker()
            },
        )
    }
    val googleSchedulePublicationCoordinator: GoogleSchedulePublicationCoordinator
        get() = googleSchedulePublicationCoordinatorDelegate.value

    /** OAuth starts are additive, but still wait for every other exact write/recovery lane. */
    private fun googleAuthorizationMutationAllowed(
        action: GoogleAuthorizationAction,
        targetAccountId: String?,
    ): Boolean {
        if (
            !privatePresentationAllowed.get() ||
            plannerStore.loadState.value != PlannerLoadState.READY
        ) {
            return false
        }
        val planner = plannerStore.state.value
        if (
            planner.requiresStartupWriteRecovery() ||
            planner.pendingProposalApplicationMutation != null ||
            planner.pendingGoogleCalendarOutbound != null ||
            planner.pendingGoogleSchedulePublication?.stage?.let {
                it != GoogleSchedulePublicationStage.ACCEPTED
            } == true
        ) {
            return false
        }
        if (canonicalSyncManagerDelegate.isInitialized() && canonicalSyncManager.state.value.isBusy) {
            return false
        }
        if (executionSyncManagerDelegate.isInitialized() && executionSyncManager.state.value.isBusy) {
            return false
        }
        if (
            proposalApplicationManagerDelegate.isInitialized() &&
            proposalApplicationManager.state.value.isBusy
        ) {
            return false
        }
        if (
            googleCalendarImportCoordinatorDelegate.isInitialized() &&
            googleCalendarImportCoordinator.state.value.isBusy
        ) {
            return false
        }
        if (
            googleAuthorizationBlockedByImportRecovery(action, targetAccountId)
        ) {
            return false
        }
        if (
            googleCalendarOutboundCoordinatorDelegate.isInitialized() &&
            (
                googleCalendarOutboundCoordinator.state.value.isBusy ||
                    googleCalendarOutboundCoordinator.hasCredentialRecoveryBlocker()
            )
        ) {
            return false
        }
        if (
            googleSchedulePublicationCoordinatorDelegate.isInitialized() &&
            (
                googleSchedulePublicationCoordinator.state.value.isBusy ||
                    googleSchedulePublicationCoordinator.hasCredentialRecoveryBlocker()
            )
        ) {
            return false
        }
        return true
    }

    private fun hasGoogleAuthorizationRecoveryBlocker(): Boolean = runCatching {
        val observedAt = System.currentTimeMillis()
        when (val loaded = googleAuthorizationJournalStore.load(observedAt)) {
            GoogleAuthorizationJournalLoadResult.Empty -> false
            is GoogleAuthorizationJournalLoadResult.Loaded,
            is GoogleAuthorizationJournalLoadResult.Corrupt,
            is GoogleAuthorizationJournalLoadResult.Expired,
            -> true
            is GoogleAuthorizationJournalLoadResult.Retirable ->
                !googleAuthorizationJournalStore.removeExact(loaded.journal, observedAt) ||
                    googleAuthorizationJournalStore.load(observedAt) !=
                    GoogleAuthorizationJournalLoadResult.Empty
        }
    }.getOrDefault(true)

    private fun hasGoogleCalendarImportRecoveryBlocker(): Boolean = runCatching {
        when (val loaded = googleCalendarImportJournalStore.load(System.currentTimeMillis())) {
            is GoogleCalendarImportJournalLoadResult.Loaded -> loaded.journals.isNotEmpty()
            GoogleCalendarImportJournalLoadResult.Corrupt -> true
        }
    }.getOrDefault(true)

    private fun googleAuthorizationBlockedByImportRecovery(
        action: GoogleAuthorizationAction,
        targetAccountId: String?,
    ): Boolean = runCatching {
        val binding = apiCredentialStore.snapshot()
        blocksGoogleAuthorizationForImportRecovery(
            action = action,
            targetAccountId = targetAccountId,
            bindingConfigurationId = binding.configurationId,
            bindingBaseUrl = binding.baseUrl,
            recovery = googleCalendarImportJournalStore.load(System.currentTimeMillis()),
        )
    }.getOrDefault(true)

    private val energySignalManagerDelegate = lazy {
        EnergySignalManager(
            provider = HealthConnectEnergyProvider(this),
            plannerStore = plannerStore,
            generationFence = energySignalGenerationFence,
        )
    }
    val energySignalManager: EnergySignalManager
        get() = energySignalManagerDelegate.value

    private val suggestionSyncWorkBackend by lazy {
        WorkManagerSuggestionSyncBackend(this)
    }

    val suggestionSyncSchedulingCoordinator: SuggestionSyncSchedulingCoordinator by lazy {
        SuggestionSyncSchedulingCoordinator(
            credentialStore = apiCredentialStore,
            backend = suggestionSyncWorkBackend,
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
        if (onboardingRuntimeGate.backgroundWorkAllowed()) {
            onboardingConsentBootstrap.launchIfAllowed(onboardingRuntimeGate)
        } else {
            scheduleOnboardingConsentBoundaryReconciliation()
        }
    }

    private fun scheduleOnboardingConsentBoundaryReconciliation() {
        if (!consentBoundaryReconciliationActive.compareAndSet(false, true)) return
        persistenceScope.launch {
            try {
                while (!reconcileOnboardingConsentBoundaryOnce()) {
                    delay(CONSENT_BOUNDARY_RETRY_DELAY_MILLIS)
                }
            } catch (error: CancellationException) {
                throw error
            } finally {
                consentBoundaryReconciliationActive.set(false)
                val releaseStillNeeded =
                    (onboardingController.state as? OnboardingControllerState.Active)
                        ?.privacyAcknowledged == true &&
                    !onboardingRuntimeGate.backgroundWorkAllowed()
                if (releaseStillNeeded) {
                    scheduleOnboardingConsentBoundaryReconciliation()
                }
            }
        }
    }

    private suspend fun reconcileOnboardingConsentBoundaryOnce(): Boolean {
        if (onboardingRuntimeGate.backgroundWorkAllowed()) return true

        onboardingRuntimeGate.setDurablePrivacyAcknowledgement(false)
        closePrivatePresentationBoundary()
        return try {
            // This credential-free backend fence must not initialize the auth envelope merely to
            // cancel OS work on an unacknowledged launch.
            suggestionSyncWorkBackend.cancelAllAndAwait()
            if (!timedBreakNotificationCoordinator.cancelBeforeConsentRelease()) return false
            if (!clearPreConsentTimedBreakRoutes()) return false

            var active = onboardingController.state as? OnboardingControllerState.Active
                ?: return true
            if (!active.privacyAcknowledged) return true
            if (!active.privacyReleaseCompleted) {
                if (!onboardingController.completePrivacyRelease()) return false
                active = onboardingController.state as? OnboardingControllerState.Active
                    ?: return false
            }
            if (!active.privacyAcknowledged || !active.privacyReleaseCompleted) return false
            onboardingRuntimeGate.setDurablePrivacyAcknowledgement(true)
            onboardingConsentBootstrap.launchIfAllowed(onboardingRuntimeGate)
            reconcilePrivatePresentationBoundary()
            true
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            // Opening the process-local gate is not itself success. If bootstrap throws, close it
            // again so the retry cannot short-circuit while consent-dependent services are absent.
            onboardingRuntimeGate.setDurablePrivacyAcknowledgement(false)
            closePrivatePresentationBoundary()
            false
        }
    }

    private fun clearPreConsentTimedBreakRoutes(): Boolean {
        if (!timedBreakNotificationRoutes.revokeIssued()) return false
        val pending = timedBreakNotificationRoutes.pendingDigest.value ?: return true
        return timedBreakNotificationRoutes.consume(pending)
    }

    private fun launchConsentDependentServices() {
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
                launchCanonicalAction { recoverCurrentPublishedSchedule() }
            }
        }
    }

    /** Releases private state and network work only after the checkpoint write was verified. */
    fun acknowledgeOnboardingPrivacy(): Boolean {
        if (!onboardingController.acknowledgePrivacy()) return false
        val active = onboardingController.state as? OnboardingControllerState.Active
            ?: return false
        if (!active.privacyAcknowledged) return false
        if (onboardingRuntimeGate.backgroundWorkAllowed()) return true
        scheduleOnboardingConsentBoundaryReconciliation()
        return true
    }

    /** Exact corrupt-checkpoint recovery never touches planner, credential, or provider stores. */
    fun recoverOnboardingCheckpoint(
        expected: OnboardingCorruptArtifactIdentity,
    ): Boolean {
        if (!onboardingController.recoverCorruptExact(expected)) return false
        onboardingRuntimeGate.setDurablePrivacyAcknowledgement(false)
        closePrivatePresentationBoundary()
        scheduleOnboardingConsentBoundaryReconciliation()
        return true
    }

    fun onboardingBackgroundWorkAllowed(): Boolean =
        onboardingRuntimeGate.backgroundWorkAllowed()

    /**
     * Starts a local encrypted habit write in process scope, outside the shared network gate.
     * A screen may cancel its await without cancelling the write that owns the durable receipt.
     */
    fun launchDurableHabitAction(action: suspend () -> Boolean): Deferred<Boolean> {
        if (
            !onboardingRuntimeGate.backgroundWorkAllowed() ||
            hasGoogleAuthorizationRecoveryBlocker()
        ) {
            return CompletableDeferred(false)
        }
        return persistenceScope.launchDurableBooleanAction {
            if (
                !onboardingRuntimeGate.backgroundWorkAllowed() ||
                hasGoogleAuthorizationRecoveryBlocker()
            ) {
                false
            } else {
                action()
            }
        }
    }

    /**
     * Result-bearing shared-gate launch for workers that must distinguish rejection from a later
     * recovery-fence suppression. A non-null handle always reaches a Boolean result unless the
     * application scope itself is cancelled.
     */
    fun launchCanonicalResultAction(
        action: suspend () -> Boolean,
    ): Deferred<Boolean>? {
        if (
            !onboardingRuntimeGate.backgroundWorkAllowed() ||
            hasGoogleAuthorizationRecoveryBlocker()
        ) {
            return null
        }
        if (!canonicalActionGate.tryEnter()) return null
        return persistenceScope.async {
            try {
                try {
                    if (hasGoogleAuthorizationRecoveryBlocker()) false else action()
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    false
                }
            } finally {
                canonicalActionGate.leave()
            }
        }
    }

    /** Canonical actions outlive a transient screen/ViewModel so responses are always reconciled. */
    fun launchCanonicalAction(action: suspend () -> Unit): Boolean {
        if (
            !onboardingRuntimeGate.backgroundWorkAllowed() ||
            hasGoogleAuthorizationRecoveryBlocker()
        ) {
            return false
        }
        if (!canonicalActionGate.tryEnter()) return false
        persistenceScope.launch {
            try {
                if (!hasGoogleAuthorizationRecoveryBlocker()) action()
            } finally {
                canonicalActionGate.leave()
            }
        }
        return true
    }

    /** OAuth is the only lane allowed to own its persisted authorization journal. */
    fun launchGoogleAuthorizationAction(
        action: GoogleAuthorizationAction,
        targetAccountId: String?,
        operation: suspend () -> Unit,
    ): Boolean {
        if (
            !googleAuthorizationMutationAllowed(action, targetAccountId) ||
            !canonicalActionGate.tryEnter()
        ) {
            return false
        }
        persistenceScope.launch {
            try {
                if (googleAuthorizationMutationAllowed(action, targetAccountId)) operation()
            } finally {
                canonicalActionGate.leave()
            }
        }
        return true
    }

    /** Explicit local credential destruction owns every recovery cleanup under one gate. */
    fun launchConfirmedLocalCredentialDestruction(
        confirmed: Boolean,
        operation: suspend () -> Unit,
    ): Boolean {
        if (
            !confirmed ||
            !onboardingRuntimeGate.privatePresentationAllowed() ||
            !canonicalActionGate.tryEnter()
        ) {
            return false
        }
        persistenceScope.launch {
            try {
                operation()
            } finally {
                canonicalActionGate.leave()
            }
        }
        return true
    }

    /** Explicit OAuth-journal recovery may clear its own blocker, but no active lane may race it. */
    fun launchGoogleAuthorizationRecoveryAction(operation: suspend () -> Unit): Boolean {
        if (!privatePresentationAllowed.get() || !canonicalActionGate.tryEnter()) return false
        persistenceScope.launch {
            try {
                if (privatePresentationAllowed.get()) operation()
            } finally {
                canonicalActionGate.leave()
            }
        }
        return true
    }

    /** Keeps the durable pre-browser CAS and UI handoff on the caller's main dispatcher. */
    suspend fun runGoogleAuthorizationActionInCaller(
        action: GoogleAuthorizationAction,
        targetAccountId: String?,
        operation: suspend () -> Unit,
    ): Boolean {
        if (
            !googleAuthorizationMutationAllowed(action, targetAccountId) ||
            !canonicalActionGate.tryEnter()
        ) {
            return false
        }
        return try {
            if (!googleAuthorizationMutationAllowed(action, targetAccountId)) false else {
                operation()
                true
            }
        } finally {
            canonicalActionGate.leave()
        }
    }

    /** Queues mandatory recovery behind any active canonical mutation instead of dropping it. */
    fun enqueueCanonicalRecovery(action: suspend () -> Unit) {
        if (!onboardingRuntimeGate.backgroundWorkAllowed()) return
        persistenceScope.launch {
            canonicalActionGate.enter()
            try {
                if (!hasGoogleAuthorizationRecoveryBlocker()) action()
            } finally {
                canonicalActionGate.leave()
            }
        }
    }

    /** Starts the only device-local composition and retains it across transient recomposition. */
    fun launchLocalScheduleComposition(): Boolean =
        onboardingRuntimeGate.privatePresentationAllowed() &&
            !hasGoogleAuthorizationRecoveryBlocker() &&
            localScheduleCompositionLauncher.launch()

    /** Invalidates even non-preemptible JNI output before requesting coroutine cancellation. */
    fun cancelLocalScheduleComposition() = localScheduleCompositionLauncher.cancel()

    fun setLocalScheduleCompositionForegroundActive(active: Boolean) =
        localScheduleCompositionLauncher.setForegroundActive(
            active && onboardingRuntimeGate.foregroundProviderWorkAllowed(),
        )

    internal suspend fun cancelAndDrainLocalScheduleComposition() =
        localScheduleCompositionLauncher.cancelAndDrain()

    /** Clears memory-only proposal review content whenever locked UI becomes authoritative. */
    fun onAppPrivacyBoundaryLocked() {
        onboardingRuntimeGate.setAppUnlocked(false)
        closePrivatePresentationBoundary()
    }

    private fun closePrivatePresentationBoundary() {
        energySignalGenerationFence.close()
        privatePresentationAllowed.set(false)
        if (energySignalManagerDelegate.isInitialized()) {
            energySignalManager.quarantineForPrivacyBoundary()
        }
        if (googleAccountManagerDelegate.isInitialized()) {
            googleAccountManager.quarantineBindingState()
        }
        ScheduleCompositionProfileDraftMemory.clear()
        setLocalScheduleCompositionForegroundActive(false)
        if (canonicalItemInvalidationManagerDelegate.isInitialized()) {
            canonicalItemInvalidationManager.cancelActiveSession()
        }
        if (executionInvalidationManagerDelegate.isInitialized()) {
            executionInvalidationManager.cancelActiveSession()
        }
        if (scheduleInvalidationManagerDelegate.isInitialized()) {
            scheduleInvalidationManager.cancelActiveSession()
        }
        if (habitInvalidationManagerDelegate.isInitialized()) {
            habitInvalidationManager.cancelActiveSession()
        }
        if (proposalApplicationManagerDelegate.isInitialized()) {
            proposalApplicationManager.discardReviewForPrivacyBoundary()
        }
        if (assistantManagerDelegate.isInitialized()) {
            assistantManager.cancelForPrivacyBoundary()
        }
        if (googleCalendarImportCoordinatorDelegate.isInitialized()) {
            googleCalendarImportCoordinator.quarantineBindingState()
        }
        if (googleCalendarOutboundCoordinatorDelegate.isInitialized()) {
            googleCalendarOutboundCoordinator.quarantineBindingState()
        }
        if (googleSchedulePublicationCoordinatorDelegate.isInitialized()) {
            googleSchedulePublicationCoordinator.quarantineBindingState()
        }
    }

    /** Re-enables private provider reads only after the lock controller exposes unlocked UI. */
    fun onAppPrivacyBoundaryUnlocked() {
        onboardingRuntimeGate.setAppUnlocked(true)
        reconcilePrivatePresentationBoundary()
    }

    /** Opens the assistant gate only while the unlocked activity is STARTED. */
    fun onAppForegroundAssistantActive() {
        assistantForegroundActive.set(true)
        onboardingRuntimeGate.setActivityStarted(true)
        reconcilePrivatePresentationBoundary()
    }

    /** AI inference is foreground-only even when delayed app locking is disabled. */
    fun onAppForegroundAssistantInactive() {
        assistantForegroundActive.set(false)
        onboardingRuntimeGate.setActivityStarted(false)
        closePrivatePresentationBoundary()
    }

    private fun reconcilePrivatePresentationBoundary() {
        if (!onboardingRuntimeGate.privatePresentationAllowed()) {
            closePrivatePresentationBoundary()
            return
        }
        energySignalGenerationFence.open()
        privatePresentationAllowed.set(true)
        if (assistantForegroundActive.get() && assistantManagerDelegate.isInitialized()) {
            assistantManager.restoreForegroundState()
        }
    }

    /** Collected only by the unlocked STARTED UI; cancellation closes the response body. */
    suspend fun runForegroundExecutionInvalidations() {
        if (!onboardingRuntimeGate.foregroundProviderWorkAllowed()) return
        executionInvalidationManager.runForegroundActivation()
    }

    /** Includes the 30-second delta fallback and runs only in the unlocked STARTED UI. */
    suspend fun runForegroundCanonicalItemInvalidations() {
        if (!onboardingRuntimeGate.foregroundProviderWorkAllowed()) return
        canonicalItemInvalidationManager.runForegroundActivation()
    }

    /** Includes an immediate authoritative GET and 30-second fallback while unlocked. */
    suspend fun runForegroundScheduleInvalidations() {
        if (!onboardingRuntimeGate.foregroundProviderWorkAllowed()) return
        scheduleInvalidationManager.runForegroundActivation()
    }

    /** Content-free habit hints accelerate the independent 30-second authoritative fallback. */
    suspend fun runForegroundHabitInvalidations() {
        if (!onboardingRuntimeGate.foregroundProviderWorkAllowed()) return
        habitInvalidationManager.runForegroundActivation()
    }

    /** Startup recovery installs the immutable head without creating a competing publication. */
    suspend fun recoverCurrentPublishedSchedule(): CanonicalRefreshOutcome? {
        if (!onboardingRuntimeGate.backgroundWorkAllowed()) return null
        if (googleAccountManager.hasAuthorizationRecoveryBlocker()) return null
        proposalApplicationManager.recoverPending()
        if (plannerStore.state.value.pendingProposalApplicationMutation != null) return null
        if (habitSyncManager.refresh() !in HABIT_REFRESH_COMPOSE_SAFE_OUTCOMES) return null
        return recoverCurrentPublishedScheduleSequence(
            requiresWriteRecovery = plannerStore.state.value.requiresStartupWriteRecovery(),
            canonicalWriteRecovery = {
                refreshCanonicalStateSequence(
                    executionRefresh = executionSyncManager::refresh,
                    canonicalRefresh = canonicalSyncManager::refreshAndCompose,
                )
            },
            executionRefresh = executionSyncManager::refresh,
            replicaRefresh = canonicalSyncManager::refreshCurrentPublishedSchedule,
        )
    }

    /** Reconciles an old/remote lease both before and after replacing today's composition. */
    suspend fun refreshCanonicalState(): CanonicalRefreshOutcome? {
        if (!onboardingRuntimeGate.backgroundWorkAllowed()) return null
        if (googleAccountManager.hasAuthorizationRecoveryBlocker()) return null
        proposalApplicationManager.recoverPending()
        if (plannerStore.state.value.pendingProposalApplicationMutation != null) return null
        if (habitSyncManager.refresh() !in HABIT_REFRESH_COMPOSE_SAFE_OUTCOMES) return null
        return refreshCanonicalStateSequence(
            executionRefresh = executionSyncManager::refresh,
            canonicalRefresh = canonicalSyncManager::refreshAndCompose,
        )
    }

    /** Foreground polling promotes a newly observed eligible terminal fact without periodic churn. */
    suspend fun refreshForegroundExecution() {
        if (!onboardingRuntimeGate.foregroundProviderWorkAllowed()) return
        if (googleAccountManager.hasAuthorizationRecoveryBlocker()) return
        if (habitSyncManager.refresh() !in HABIT_REFRESH_COMPOSE_SAFE_OUTCOMES) return
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
        const val CONSENT_BOUNDARY_RETRY_DELAY_MILLIS = 5_000L
        val CANONICAL_TERMINAL_EXECUTION_STATUSES = setOf("completed", "skipped")
        val HABIT_REFRESH_COMPOSE_SAFE_OUTCOMES = setOf(
            HabitSyncOutcome.SUCCESS,
            HabitSyncOutcome.CONFLICT,
            HabitSyncOutcome.NOT_FOUND,
            HabitSyncOutcome.VALIDATION_FAILURE,
        )
    }
}

/** The returned handle may be awaited by a shorter-lived UI job without adopting its lifetime. */
internal fun CoroutineScope.launchDurableBooleanAction(
    action: suspend () -> Boolean,
): Deferred<Boolean> = async { action() }

/** Pure orchestration seam: execution truth brackets composition and its terminal projection. */
internal suspend fun refreshCanonicalStateSequence(
    executionRefresh: suspend () -> ExecutionSyncOutcome,
    canonicalRefresh: suspend () -> CanonicalRefreshOutcome,
): CanonicalRefreshOutcome? {
    if (executionRefresh() !in EXECUTION_REFRESH_SUCCESSES) return null
    val canonicalOutcome = canonicalRefresh()
    executionRefresh()
    return canonicalOutcome
}

/** Startup must reconcile immutable write journals before attempting a read-only replica GET. */
internal fun DayWeaveUiState.requiresStartupWriteRecovery(): Boolean =
    pendingSchedulePublication != null || pendingCanonicalMutation != null ||
        pendingCanonicalAuthoringMutations.any {
            it.disposition == CanonicalAuthoringDisposition.PENDING
        } || pendingExecutionCommand != null ||
        pendingExecutionDeferIntent != null || deferredExecutionRecompositionNeeded() ||
        terminalExecutionOutcomes.values.any { outcome ->
            outcome.requiresCanonicalItemProjection && outcome.canonicalProjectionRevision == null &&
                outcome.canonicalProjectionResolution == null && isNewestExecutionForProjection(
                outcome.session,
            )
        }

/** Read-only native recovery seam kept deterministic for process-death tests. */
internal suspend fun recoverCurrentPublishedScheduleSequence(
    requiresWriteRecovery: Boolean,
    canonicalWriteRecovery: suspend () -> CanonicalRefreshOutcome?,
    executionRefresh: suspend () -> ExecutionSyncOutcome,
    replicaRefresh: suspend () -> CanonicalRefreshOutcome,
): CanonicalRefreshOutcome? {
    if (requiresWriteRecovery) return canonicalWriteRecovery()
    if (executionRefresh() !in EXECUTION_REFRESH_SUCCESSES) return null
    val outcome = replicaRefresh()
    executionRefresh()
    return outcome
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

/** Odd generations are open; every close/reopen gives stale provider callbacks a new token. */
internal class ApplicationEnergySignalGenerationFence : EnergySignalGenerationFence {
    private val generation = AtomicLong(CLOSED_GENERATION)

    override fun captureGeneration(): Long = generation.get()

    override fun isCurrent(generation: Long): Boolean =
        generation % 2L == OPEN_REMAINDER && this.generation.get() == generation

    fun open() {
        while (true) {
            val current = generation.get()
            if (current % 2L == OPEN_REMAINDER) return
            if (generation.compareAndSet(current, current + 1L)) return
        }
    }

    fun close() {
        while (true) {
            val current = generation.get()
            if (current % 2L != OPEN_REMAINDER) return
            if (generation.compareAndSet(current, current + 1L)) return
        }
    }

    private companion object {
        const val CLOSED_GENERATION = 0L
        const val OPEN_REMAINDER = 1L
    }
}

/** The narrow import-repair exception is bound to one account and credential generation. */
internal fun blocksGoogleAuthorizationForImportRecovery(
    action: GoogleAuthorizationAction,
    targetAccountId: String?,
    bindingConfigurationId: String?,
    bindingBaseUrl: String?,
    recovery: GoogleCalendarImportJournalLoadResult,
): Boolean = when (recovery) {
    GoogleCalendarImportJournalLoadResult.Corrupt -> true
    is GoogleCalendarImportJournalLoadResult.Loaded -> {
        val journals = recovery.journals
        journals.isNotEmpty() && (
            action != GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY ||
                targetAccountId == null || bindingConfigurationId == null ||
                bindingBaseUrl == null || journals.any { journal ->
                    journal.configurationId != bindingConfigurationId ||
                        journal.apiBaseUrl != bindingBaseUrl
                } || journals.any { journal -> journal.accountId != targetAccountId }
            )
    }
}

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
