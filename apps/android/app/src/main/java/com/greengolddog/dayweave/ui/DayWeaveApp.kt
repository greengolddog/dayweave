package com.greengolddog.dayweave.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.compose.animation.AnimatedVisibility
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Row
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.health.connect.client.PermissionController
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.MoveLaterPlacementMode
import com.greengolddog.dayweave.model.OnboardingFirstItemCheck
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStage
import com.greengolddog.dayweave.model.assessMoveLater
import com.greengolddog.dayweave.model.authoritativeTimedBreakNotificationIdentity
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.model.validatedOnboardingFirstItemCheck
import com.greengolddog.dayweave.notifications.TimedBreakNotificationPresentationDecision
import com.greengolddog.dayweave.notifications.TimedBreakNotificationSystemState
import com.greengolddog.dayweave.notifications.TimedBreakReminderEnableAction
import com.greengolddog.dayweave.notifications.clearValidatedRejectedNotificationRoute
import com.greengolddog.dayweave.notifications.shouldOfferCurrentTimedBreakReview
import com.greengolddog.dayweave.notifications.isExactTimedBreakResolutionCurrent
import com.greengolddog.dayweave.notifications.shouldPresentTimedBreakResolution
import com.greengolddog.dayweave.notifications.timedBreakNotificationPresentationDecision
import com.greengolddog.dayweave.notifications.timedBreakNotificationRouteStateAvailable
import com.greengolddog.dayweave.notifications.timedBreakReminderEnableAction
import com.greengolddog.dayweave.notifications.reconcileTimedBreakNotificationAuthorization
import com.greengolddog.dayweave.onboarding.OnboardingController
import com.greengolddog.dayweave.onboarding.OnboardingControllerState
import com.greengolddog.dayweave.onboarding.OnboardingRuntimePrivacyState
import com.greengolddog.dayweave.onboarding.OnboardingStep
import com.greengolddog.dayweave.health.EnergyProviderAvailability
import com.greengolddog.dayweave.health.HealthConnectIntents
import com.greengolddog.dayweave.security.AppLockController
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.security.AppLockTimeout
import com.greengolddog.dayweave.state.DayWeaveViewModel
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.ui.components.ActiveSessionBar
import com.greengolddog.dayweave.ui.components.ApiConnectionDialog
import com.greengolddog.dayweave.ui.components.AppLockedScreen
import com.greengolddog.dayweave.ui.components.BreakEndedDialog
import com.greengolddog.dayweave.ui.components.EditSuggestionDialog
import com.greengolddog.dayweave.ui.components.ExecutionDeferApprovalDialog
import com.greengolddog.dayweave.ui.components.ExecutionDeferPendingDialog
import com.greengolddog.dayweave.ui.components.MoveLaterChooserDialog
import com.greengolddog.dayweave.ui.components.PauseChooserDialog
import com.greengolddog.dayweave.ui.components.ProposalReviewDialog
import com.greengolddog.dayweave.ui.components.QuickCaptureSheet
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorMode
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorRoute
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorSheet
import com.greengolddog.dayweave.ui.authoring.GoogleCalendarOutboundReviewSheet
import com.greengolddog.dayweave.ui.authoring.GoogleSchedulePublicationReviewSheet
import com.greengolddog.dayweave.ui.authoring.canonicalDependencyEditorContext
import com.greengolddog.dayweave.ui.authoring.canonicalParentOptions
import com.greengolddog.dayweave.ui.navigation.DayWeaveNavigationBar
import com.greengolddog.dayweave.ui.screens.AssistantScreen
import com.greengolddog.dayweave.ui.screens.CalendarScreen
import com.greengolddog.dayweave.ui.screens.HabitStatisticsSection
import com.greengolddog.dayweave.ui.screens.InboxScreen
import com.greengolddog.dayweave.ui.screens.MoreScreen
import com.greengolddog.dayweave.ui.screens.PlanningProfileEditorDialog
import com.greengolddog.dayweave.ui.screens.planningProfileEditBlockedMessage
import com.greengolddog.dayweave.ui.screens.TodayHabitsSection
import com.greengolddog.dayweave.ui.screens.TodayScreen
import com.greengolddog.dayweave.ui.onboarding.DayWeaveOnboardingShell
import com.greengolddog.dayweave.ui.onboarding.OnboardingCallbacks
import com.greengolddog.dayweave.ui.onboarding.OnboardingCheckState
import com.greengolddog.dayweave.ui.onboarding.OnboardingReadiness
import com.greengolddog.dayweave.ui.onboarding.OnboardingRecoveryUiState
import com.greengolddog.dayweave.ui.onboarding.OnboardingUiState
import com.greengolddog.dayweave.ui.onboarding.apiOnboardingCheck
import com.greengolddog.dayweave.ui.onboarding.firstItemOnboardingCheck
import com.greengolddog.dayweave.ui.onboarding.firstPlanOnboardingCheck
import com.greengolddog.dayweave.ui.onboarding.googleOnboardingCheck
import com.greengolddog.dayweave.ui.onboarding.profileOnboardingCheck
import com.greengolddog.dayweave.ui.theme.DayWeaveTheme
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleCalendarImportPhase
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundPhase
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundTargetOption
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationPhase
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationTargetOption
import com.greengolddog.dayweave.network.DeviceAuthPhase
import java.time.Instant
import java.time.ZoneId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.supervisorScope
import kotlinx.coroutines.flow.StateFlow

@Composable
fun DayWeaveApp(
    appLockController: AppLockController,
    onRequestUnlock: () -> Unit,
    onSetAppLockEnabled: (Boolean) -> Unit,
    onSetAppLockTimeout: (AppLockTimeout) -> Unit,
    onLockNow: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
    onboardingController: OnboardingController,
    onboardingRuntimePrivacyState: StateFlow<OnboardingRuntimePrivacyState>,
    onAcknowledgeOnboardingPrivacy: () -> Boolean,
    onRecoverOnboardingCheckpoint: (
        com.greengolddog.dayweave.onboarding.OnboardingCorruptArtifactIdentity,
    ) -> Boolean,
    timedBreakNotificationRouteDigest: String? = null,
    onTimedBreakNotificationRouteConsumed: (String) -> Boolean = { true },
    onRequestTimedBreakNotificationPermission: () -> Unit = {},
    timedBreakNotificationSystemState: TimedBreakNotificationSystemState =
        TimedBreakNotificationSystemState.ENABLED,
    onEnableTimedBreakReminders: () -> Unit = {},
) {
    val appLockState by appLockController.state.collectAsStateWithLifecycle()
    val onboardingState = onboardingController.states.collectAsStateWithLifecycle().value
    val runtimePrivacyState =
        onboardingRuntimePrivacyState.collectAsStateWithLifecycle().value
    AppLockPresentationGate(
        appLockState = appLockState,
        lockedContent = {
            DayWeaveTheme(useDynamicColor = false) {
                AppLockedScreen(
                    state = appLockState,
                    onUnlock = onRequestUnlock,
                    onOpenDeviceSecuritySettings = onOpenDeviceSecuritySettings,
                )
            }
        },
        unlockedContent = {
            val setup = onboardingState
            when {
                setup is OnboardingControllerState.RecoveryRequired -> {
                    OpaqueOnboardingPresentation(
                        state = OnboardingUiState(
                            recovery = OnboardingRecoveryUiState.CORRUPT,
                        ),
                        callbacks = OnboardingCallbacks(
                            onResetProgressAfterWarning = {
                                onRecoverOnboardingCheckpoint(setup.artifactIdentity)
                            },
                        ),
                    )
                }
                setup is OnboardingControllerState.Active && !setup.privacyAcknowledged -> {
                    OpaqueOnboardingPresentation(
                        state = OnboardingUiState(
                            step = OnboardingStep.WELCOME,
                            privacyAcknowledged = false,
                        ),
                        callbacks = OnboardingCallbacks(
                            onPrivacyAcknowledgementChanged = { acknowledged ->
                                if (acknowledged) onAcknowledgeOnboardingPrivacy()
                            },
                        ),
                    )
                }
                shouldMountPrivatePlannerSubtree(setup, runtimePrivacyState) -> {
                    UnlockedDayWeaveApp(
                        appLockState = appLockState,
                        onSetAppLockEnabled = onSetAppLockEnabled,
                        onSetAppLockTimeout = onSetAppLockTimeout,
                        onLockNow = onLockNow,
                        onOpenDeviceSecuritySettings = onOpenDeviceSecuritySettings,
                        timedBreakNotificationRouteDigest = timedBreakNotificationRouteDigest,
                        onTimedBreakNotificationRouteConsumed =
                            onTimedBreakNotificationRouteConsumed,
                        onRequestTimedBreakNotificationPermission =
                            onRequestTimedBreakNotificationPermission,
                        timedBreakNotificationSystemState = timedBreakNotificationSystemState,
                        onEnableTimedBreakReminders = onEnableTimedBreakReminders,
                        onboardingController = onboardingController,
                    )
                }
                setup is OnboardingControllerState.Active -> {
                    PrivatePresentationWaitingScreen()
                }
            }
        },
    )
}

internal fun shouldMountPrivatePlannerSubtree(
    setup: OnboardingControllerState?,
    runtimePrivacyState: OnboardingRuntimePrivacyState?,
): Boolean = setup is OnboardingControllerState.Active &&
    setup.privacyAcknowledged &&
    setup.privacyReleaseCompleted &&
    runtimePrivacyState?.privatePresentationAllowed == true

internal fun onboardingFirstItemEditorRoute(
    durableState: com.greengolddog.dayweave.model.DayWeaveUiState?,
): CanonicalItemEditorRoute? {
    durableState ?: return CanonicalItemEditorRoute.create()
    val anchor = durableState.onboardingFirstItemAnchor
        ?: return CanonicalItemEditorRoute.create()
    return when (durableState.validatedOnboardingFirstItemCheck()) {
        OnboardingFirstItemCheck.PENDING_CREATE -> {
            val mutation = durableState.pendingCanonicalAuthoringMutations.singleOrNull {
                it.itemId == anchor.itemId &&
                    it.operation ==
                    com.greengolddog.dayweave.model.CanonicalAuthoringOperation.CREATE
            }?.takeUnless { it.isSubmitted } ?: return null
            CanonicalItemEditorRoute(
                itemId = anchor.itemId,
                initialDraft = requireNotNull(mutation.draft),
                mode = CanonicalItemEditorMode.UPDATE_PENDING,
                mutationId = mutation.id,
            )
        }
        OnboardingFirstItemCheck.CANONICAL_ITEM -> {
            val item = durableState.canonicalItems.singleOrNull {
                it.id == anchor.itemId && it.revision == anchor.canonicalRevision &&
                    it.deletedAt == null
            } ?: return null
            val draft = runCatching {
                (if (item.status == "scheduled") item.copy(status = "planned") else item)
                    .toCanonicalDraft()
            }.getOrNull() ?: return null
            CanonicalItemEditorRoute(
                itemId = item.id,
                initialDraft = draft,
                mode = CanonicalItemEditorMode.REPLACE,
            )
        }
        null -> CanonicalItemEditorRoute.create()
    }
}

/** Re-reads every authoritative flow at the click boundary instead of trusting a Compose frame. */
private fun DayWeaveViewModel.currentOnboardingReadiness(
    profileReviewed: Boolean,
): OnboardingReadiness {
    val canonical = canonicalSyncState.value
    val execution = executionSyncState.value
    val effectiveCanonical = if (
        execution.phase in setOf(
            CanonicalSyncPhase.AUTH_REQUIRED,
            CanonicalSyncPhase.SYNCING,
            CanonicalSyncPhase.OFFLINE,
            CanonicalSyncPhase.ERROR,
        )
    ) {
        canonical.copy(phase = execution.phase, message = execution.message)
    } else {
        canonical
    }
    return OnboardingReadiness(
        api = apiOnboardingCheck(deviceAuthState.value, effectiveCanonical),
        google = googleOnboardingCheck(
            googleAccountState.value,
            googleCalendarImportState.value,
        ),
        profile = profileOnboardingCheck(
            current = state.value,
            durable = durableState.value,
            update = scheduleCompositionProfileUpdateState.value,
            profileReviewed = profileReviewed,
        ),
        firstItem = firstItemOnboardingCheck(durableState.value),
        firstPlan = firstPlanOnboardingCheck(durableState.value),
    )
}

@Composable
private fun OpaqueOnboardingPresentation(
    state: OnboardingUiState,
    callbacks: OnboardingCallbacks,
) {
    DayWeaveTheme(useDynamicColor = false) {
        DayWeaveOnboardingShell(state = state, callbacks = callbacks)
    }
}

@Composable
private fun PrivatePresentationWaitingScreen() {
    DayWeaveTheme(useDynamicColor = false) {
        PlannerRestoreScreen(message = "Securing your private workspace…")
    }
}

/** Keeps every unlocked subtree, including any open Dialog window, below one lock boundary. */
@Composable
internal fun AppLockPresentationGate(
    appLockState: AppLockState,
    lockedContent: @Composable () -> Unit,
    unlockedContent: @Composable () -> Unit,
) {
    if (appLockState.isLocked) lockedContent() else unlockedContent()
}

@Composable
private fun UnlockedDayWeaveApp(
    appLockState: AppLockState,
    onSetAppLockEnabled: (Boolean) -> Unit,
    onSetAppLockTimeout: (AppLockTimeout) -> Unit,
    onLockNow: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
    timedBreakNotificationRouteDigest: String?,
    onTimedBreakNotificationRouteConsumed: (String) -> Boolean,
    onRequestTimedBreakNotificationPermission: () -> Unit,
    timedBreakNotificationSystemState: TimedBreakNotificationSystemState,
    onEnableTimedBreakReminders: () -> Unit,
    onboardingController: OnboardingController,
    viewModel: DayWeaveViewModel = viewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val loadState by viewModel.loadState.collectAsStateWithLifecycle()
    DayWeaveTheme(useDynamicColor = state.useDynamicColor) {
        when (loadState) {
            PlannerLoadState.LOADING -> PlannerRestoreScreen()
            PlannerLoadState.READY -> DayWeaveRoot(
                viewModel = viewModel,
                appLockState = appLockState,
                onSetAppLockEnabled = onSetAppLockEnabled,
                onSetAppLockTimeout = onSetAppLockTimeout,
                onLockNow = onLockNow,
                onOpenDeviceSecuritySettings = onOpenDeviceSecuritySettings,
                timedBreakNotificationRouteDigest = timedBreakNotificationRouteDigest,
                onTimedBreakNotificationRouteConsumed =
                    onTimedBreakNotificationRouteConsumed,
                onRequestTimedBreakNotificationPermission =
                    onRequestTimedBreakNotificationPermission,
                timedBreakNotificationSystemState = timedBreakNotificationSystemState,
                onEnableTimedBreakReminders = onEnableTimedBreakReminders,
                onboardingController = onboardingController,
            )
            PlannerLoadState.PERSISTENCE_FAILED -> PlannerPersistenceFailureScreen()
        }
    }
}

@Composable
private fun PlannerRestoreScreen(message: String = "Opening your encrypted plan…") {
    Column(
        modifier = Modifier.fillMaxSize(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        CircularProgressIndicator()
        Text(
            text = message,
            modifier = Modifier.padding(top = 16.dp),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun PlannerPersistenceFailureScreen() {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text("Encrypted plan unavailable", style = MaterialTheme.typography.headlineSmall)
        Text(
            text = "DayWeave kept the existing database unchanged and disabled editing. Close and reopen the app; if this continues, use an explicit recovery flow before resetting local data.",
            modifier = Modifier.padding(top = 12.dp),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DayWeaveRoot(
    viewModel: DayWeaveViewModel,
    appLockState: AppLockState,
    onSetAppLockEnabled: (Boolean) -> Unit,
    onSetAppLockTimeout: (AppLockTimeout) -> Unit,
    onLockNow: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
    timedBreakNotificationRouteDigest: String?,
    onTimedBreakNotificationRouteConsumed: (String) -> Boolean,
    onRequestTimedBreakNotificationPermission: () -> Unit,
    timedBreakNotificationSystemState: TimedBreakNotificationSystemState,
    onEnableTimedBreakReminders: () -> Unit,
    onboardingController: OnboardingController,
) {
    val lifecycleOwner = LocalLifecycleOwner.current
    val state by viewModel.state.collectAsStateWithLifecycle()
    val durableState by viewModel.durableState.collectAsStateWithLifecycle()
    val suggestionSyncState by viewModel.suggestionSyncState.collectAsStateWithLifecycle()
    val assistantState by viewModel.assistantState.collectAsStateWithLifecycle()
    val proposalApplicationState by viewModel.proposalApplicationState.collectAsStateWithLifecycle()
    val canonicalSyncState by viewModel.canonicalSyncState.collectAsStateWithLifecycle()
    val executionSyncState by viewModel.executionSyncState.collectAsStateWithLifecycle()
    val habitSyncState by viewModel.habitSyncState.collectAsStateWithLifecycle()
    val googleAccountState by viewModel.googleAccountState.collectAsStateWithLifecycle()
    val googleCalendarImportState by
        viewModel.googleCalendarImportState.collectAsStateWithLifecycle()
    val googleCalendarOutboundState by
        viewModel.googleCalendarOutboundState.collectAsStateWithLifecycle()
    val googleSchedulePublicationState by
        viewModel.googleSchedulePublicationState.collectAsStateWithLifecycle()
    val energySignalState by viewModel.energySignalState.collectAsStateWithLifecycle()
    val deviceAuthState by viewModel.deviceAuthState.collectAsStateWithLifecycle()
    // Unlike durable planner projections, this private inventory must observe its memory-only
    // quarantine even while the Activity lifecycle is stopped.
    val deviceSessionsState by viewModel.deviceSessionsState.collectAsState()
    val timedBreakNotificationPermissionRequestDigest by
        viewModel.timedBreakNotificationPermissionRequestDigest.collectAsStateWithLifecycle()
    val scheduleCompositionProfileUpdateState by
        viewModel.scheduleCompositionProfileUpdateState.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val coroutineScope = rememberCoroutineScope()
    val uriHandler = LocalUriHandler.current
    val healthPermissionLauncher = rememberLauncherForActivityResult(
        contract = PermissionController.createRequestPermissionResultContract(),
        onResult = viewModel::onHealthConnectPermissionResult,
    )
    val effectiveCanonicalSyncState = if (
        executionSyncState.phase in setOf(
            CanonicalSyncPhase.AUTH_REQUIRED,
            CanonicalSyncPhase.SYNCING,
            CanonicalSyncPhase.OFFLINE,
            CanonicalSyncPhase.ERROR,
        )
    ) {
        canonicalSyncState.copy(
            phase = executionSyncState.phase,
            message = executionSyncState.message,
        )
    } else {
        canonicalSyncState
    }
    val canonicalExecutionActionsEnabled =
        !canonicalSyncState.isBusy && !executionSyncState.isBusy && !habitSyncState.isBusy &&
            !proposalApplicationState.isBusy && !googleCalendarOutboundState.isBusy &&
            !googleSchedulePublicationState.isBusy &&
            state.pendingCanonicalMutation == null && state.pendingExecutionCommand == null &&
            state.pendingExecutionDeferIntent == null &&
            state.pendingProposalApplicationMutation == null &&
            state.pendingGoogleCalendarOutbound == null &&
            state.pendingGoogleSchedulePublication?.stage?.let {
                it != GoogleSchedulePublicationStage.ACCEPTED
            } != true
    val canonicalAuthoringActionsEnabled = canonicalExecutionActionsEnabled &&
        state.canonicalExecutionSession == null &&
        state.pendingSchedulePublication == null &&
        state.pendingCanonicalAuthoringMutations.none {
            it.isSubmitted &&
                it.disposition == com.greengolddog.dayweave.model.CanonicalAuthoringDisposition.PENDING
        }
    var showQuickCapture by remember { mutableStateOf(false) }
    var showPauseChooser by remember { mutableStateOf(false) }
    var moveLaterTargetId by remember { mutableStateOf<String?>(null) }
    var showApiConnection by remember { mutableStateOf(false) }
    var showOnboardingProfileEditor by remember { mutableStateOf(false) }
    var editingSuggestion by remember { mutableStateOf<PlanningSuggestion?>(null) }
    var disconnectingGoogleAccount by remember { mutableStateOf<GoogleAccountSummary?>(null) }
    var googlePublicationReview by remember {
        mutableStateOf<GoogleCalendarPublicationReview?>(null)
    }
    var showGooglePublicationReview by remember { mutableStateOf(false) }
    var showGoogleSchedulePublicationReview by remember { mutableStateOf(false) }
    var selectedGoogleScheduleTarget by remember {
        mutableStateOf<GoogleSchedulePublicationTargetOption?>(null)
    }
    var googleOutboundClockMillis by remember { mutableLongStateOf(System.currentTimeMillis()) }
    var plannerClockMillis by remember { mutableLongStateOf(System.currentTimeMillis()) }
    var canonicalEditorRoute by remember { mutableStateOf<CanonicalItemEditorRoute?>(null) }
    var dismissedBreakKey by rememberSaveable { mutableStateOf<String?>(null) }
    var authorizedNotificationBreakDigest by rememberSaveable {
        mutableStateOf<String?>(null)
    }
    var rejectedNotificationLaunchBreakKey by rememberSaveable {
        mutableStateOf<String?>(null)
    }
    var replayedRejectedNotificationBreakKey by rememberSaveable {
        mutableStateOf<String?>(null)
    }
    var pendingRejectedNotificationRouteDigest by rememberSaveable {
        mutableStateOf<String?>(null)
    }
    val timedBreakReminderEnableAction = timedBreakReminderEnableAction(
        durableState = durableState,
        nowEpochMillis = System.currentTimeMillis(),
        sdkInt = Build.VERSION.SDK_INT,
        systemState = timedBreakNotificationSystemState,
    )
    val observedOnboardingState =
        onboardingController.states.collectAsStateWithLifecycle().value
    val activeOnboardingState = (observedOnboardingState as? OnboardingControllerState.Active)
        ?.takeIf { !it.completed && !it.setupDeferredForSession }
    val deferredOnboardingState = (observedOnboardingState as? OnboardingControllerState.Active)
        ?.takeIf { !it.completed && it.setupDeferredForSession }
    val onboardingReadiness = OnboardingReadiness(
        api = apiOnboardingCheck(deviceAuthState, effectiveCanonicalSyncState),
        google = googleOnboardingCheck(googleAccountState, googleCalendarImportState),
        profile = profileOnboardingCheck(
            current = state,
            durable = durableState,
            update = scheduleCompositionProfileUpdateState,
            profileReviewed =
                (observedOnboardingState as? OnboardingControllerState.Active)
                    ?.profileReviewed == true,
        ),
        firstItem = firstItemOnboardingCheck(durableState),
        firstPlan = firstPlanOnboardingCheck(durableState),
    )
    val plannerClockZone = ZoneId.systemDefault()
    val plannerClockReference = Instant.ofEpochMilli(plannerClockMillis)
    val plannerHorizonEnd = state.scheduleDisplayHorizon(
        reference = plannerClockReference,
        currentZone = plannerClockZone,
    )?.end
    LaunchedEffect(lifecycleOwner, plannerClockZone, plannerHorizonEnd) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (isActive) {
                val reference = Instant.now()
                plannerClockMillis = reference.toEpochMilli()
                delay(
                    plannerClockDelayMillis(
                        reference = reference,
                        zoneId = plannerClockZone,
                        exactHorizonEnd = plannerHorizonEnd,
                    ),
                )
            }
        }
    }
    LaunchedEffect(
        state.pendingGoogleCalendarOutbound?.recoveryId,
        state.pendingGoogleCalendarOutbound?.stage,
    ) {
        if (state.pendingGoogleCalendarOutbound != null) {
            showGooglePublicationReview = true
            while (isActive) {
                googleOutboundClockMillis = System.currentTimeMillis()
                delay(1_000)
            }
        }
    }
    LaunchedEffect(
        state.pendingGoogleSchedulePublication?.recoveryId,
        state.pendingGoogleSchedulePublication?.stage,
    ) {
        if (state.pendingGoogleSchedulePublication != null) {
            showGoogleSchedulePublicationReview = true
            while (isActive) {
                googleOutboundClockMillis = System.currentTimeMillis()
                delay(1_000)
            }
        }
    }
    LaunchedEffect(
        lifecycleOwner,
        viewModel,
        timedBreakNotificationPermissionRequestDigest,
    ) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            val digest = timedBreakNotificationPermissionRequestDigest
                ?: return@repeatOnLifecycle
            if (viewModel.takeTimedBreakNotificationPermissionRequest(digest)) {
                onRequestTimedBreakNotificationPermission()
            }
        }
    }

    LaunchedEffect(
        state.destination,
        deviceAuthState.sessionId,
        deviceAuthState.isConfigured,
    ) {
        if (state.destination == AppDestination.MORE && deviceAuthState.isConfigured) {
            viewModel.refreshDeviceSessions()
        }
    }

    LaunchedEffect(
        lifecycleOwner,
        viewModel,
        deviceAuthState.baseUrl,
        deviceAuthState.sessionId,
        deviceAuthState.isConfigured,
    ) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            viewModel.setLocalScheduleCompositionForegroundActive(true)
            try {
                viewModel.refreshExecution()
                viewModel.refreshGoogleAccounts()
                viewModel.refreshEnergySignal()
                if (deviceAuthState.isConfigured) {
                    viewModel.refreshDeviceSessions()
                }
                runForegroundInvalidationWorkers(
                    executionInvalidationStream = if (deviceAuthState.isConfigured) {
                        viewModel::collectForegroundExecutionInvalidations
                    } else {
                        null
                    },
                    canonicalItemInvalidations = if (deviceAuthState.isConfigured) {
                        viewModel::collectForegroundCanonicalItemInvalidations
                    } else {
                        null
                    },
                    scheduleInvalidations = if (deviceAuthState.isConfigured) {
                        viewModel::collectForegroundScheduleInvalidations
                    } else {
                        null
                    },
                    habitInvalidations = if (deviceAuthState.isConfigured) {
                        viewModel::collectForegroundHabitInvalidations
                    } else {
                        null
                    },
                    polling = {
                        // Polling remains the durable fallback for old servers and missed publishes.
                        while (isActive) {
                            delay(EXECUTION_REFRESH_INTERVAL_MILLIS)
                            viewModel.refreshExecution()
                        }
                    },
                )
            } finally {
                // JNI cannot be preempted, so invalidate its generation before this lifecycle
                // owner can become background-visible or the unlocked subtree can disappear.
                viewModel.setLocalScheduleCompositionForegroundActive(false)
            }
        }
    }

    LaunchedEffect(
        scheduleCompositionProfileUpdateState.phase,
        showOnboardingProfileEditor,
    ) {
        if (
            showOnboardingProfileEditor &&
            scheduleCompositionProfileUpdateState.phase ==
            ScheduleCompositionProfileUpdatePhase.SAVED &&
            onboardingController.markProfileReviewed()
        ) {
            showOnboardingProfileEditor = false
            viewModel.acknowledgeScheduleCompositionProfileUpdate()
        }
    }

    if (activeOnboardingState != null) {
        val onboardingUiState = OnboardingUiState(
            step = activeOnboardingState.currentStep,
            privacyAcknowledged = true,
            readiness = onboardingReadiness,
        )
        DayWeaveOnboardingShell(
            state = onboardingUiState,
            callbacks = OnboardingCallbacks(
                onConnectThisPhone = { showApiConnection = true },
                onChooseGoogleResources = {
                    viewModel.navigate(AppDestination.MORE)
                    onboardingController.deferSetupForSession()
                },
                onReviewWeekProfile = { showOnboardingProfileEditor = true },
                onOpenNotificationSettings = onEnableTimedBreakReminders,
                onCreateFirstItem = {
                    canonicalEditorRoute = onboardingFirstItemEditorRoute(durableState)
                    if (canonicalEditorRoute == null) viewModel.recompose()
                },
                onComposeFirstPlan = viewModel::recompose,
                onBack = { onboardingController.back() },
                onSetUpLater = { onboardingController.deferSetupForSession() },
                onContinue = {
                    val current = onboardingController.state as?
                        OnboardingControllerState.Active
                    if (current != null) {
                        val currentReadiness = viewModel.currentOnboardingReadiness(
                            profileReviewed = current.profileReviewed,
                        )
                        val prerequisiteReady = current.currentStep == OnboardingStep.WELCOME ||
                            currentReadiness.checkFor(current.currentStep) ==
                            OnboardingCheckState.READY
                        onboardingController.advance(prerequisiteReady)
                    }
                },
                onFinish = {
                    val current = onboardingController.state as?
                        OnboardingControllerState.Active
                    if (current != null) {
                        onboardingController.complete(
                            viewModel.currentOnboardingReadiness(
                                profileReviewed = current.profileReviewed,
                            ).allReady,
                        )
                    }
                },
            ),
        )
    } else {
        Scaffold(
        modifier = Modifier.fillMaxSize(),
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(if (state.destination == AppDestination.TODAY) "DayWeave" else state.destination.label)
                        if (state.destination == AppDestination.TODAY) {
                            Text(
                                "Your day, composed around you",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                },
                actions = {
                    if (state.destination == AppDestination.TODAY || state.destination == AppDestination.CALENDAR) {
                        IconButton(
                            onClick = viewModel::composeOnDevice,
                            enabled = !canonicalSyncState.isBusy && !executionSyncState.isBusy &&
                                !proposalApplicationState.isBusy,
                        ) {
                            Icon(
                                Icons.Outlined.PhoneAndroid,
                                contentDescription = "Compose on this device",
                            )
                        }
                        IconButton(onClick = viewModel::recompose) {
                            Icon(Icons.Outlined.AutoAwesome, contentDescription = "Recompose schedule")
                        }
                    }
                    val planningSurface = state.destination == AppDestination.TODAY ||
                        state.destination == AppDestination.CALENDAR
                    val syncIcon = when {
                        planningSurface && effectiveCanonicalSyncState.phase == CanonicalSyncPhase.CONNECTED ->
                            Icons.Outlined.CloudDone
                        planningSurface && effectiveCanonicalSyncState.phase == CanonicalSyncPhase.SYNCING ->
                            Icons.Outlined.Sync
                        !planningSurface && suggestionSyncState.phase == SuggestionSyncPhase.CONNECTED ->
                            Icons.Outlined.CloudDone
                        !planningSurface && suggestionSyncState.phase == SuggestionSyncPhase.SYNCING ->
                            Icons.Outlined.Sync
                        else -> Icons.Outlined.CloudOff
                    }
                    Icon(
                        syncIcon,
                        contentDescription = if (planningSurface) {
                            effectiveCanonicalSyncState.message
                        } else {
                            suggestionSyncState.message
                        },
                        tint = if (
                            (planningSurface && effectiveCanonicalSyncState.phase == CanonicalSyncPhase.CONNECTED) ||
                            (!planningSurface && suggestionSyncState.phase == SuggestionSyncPhase.CONNECTED)
                        ) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = MaterialTheme.colorScheme.background,
                ),
            )
        },
        bottomBar = {
            Column {
                if (deferredOnboardingState != null) {
                    Surface(
                        color = MaterialTheme.colorScheme.primaryContainer,
                        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    "Finish setting up DayWeave",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                Text(
                                    "Your private workspace is available; guided setup will " +
                                        "resume at the same step.",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                            TextButton(onClick = { onboardingController.resumeSetup() }) {
                                Text("Resume")
                            }
                        }
                    }
                }
                if (timedBreakReminderEnableAction != TimedBreakReminderEnableAction.NONE) {
                    Surface(
                        color = MaterialTheme.colorScheme.secondaryContainer,
                        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    "Break reminders are off",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                Text(
                                    if (
                                        timedBreakReminderEnableAction ==
                                        TimedBreakReminderEnableAction.OPEN_NOTIFICATION_SETTINGS
                                    ) {
                                        "Enable them in Android settings for this future break."
                                    } else {
                                        "Enable notifications for this future break."
                                    },
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                            TextButton(onClick = onEnableTimedBreakReminders) {
                                Text("Enable reminders")
                            }
                        }
                    }
                }
                if (replayedRejectedNotificationBreakKey != null) {
                    Surface(
                        color = MaterialTheme.colorScheme.tertiaryContainer,
                        contentColor = MaterialTheme.colorScheme.onTertiaryContainer,
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(
                                    "Reminder changed",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                Text(
                                    "A different break can be reviewed separately.",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                            }
                            TextButton(
                                onClick = {
                                    if (
                                        clearValidatedRejectedNotificationRoute(
                                            pendingRejectedNotificationRouteDigest,
                                            onTimedBreakNotificationRouteConsumed,
                                        )
                                    ) {
                                        dismissedBreakKey = null
                                        authorizedNotificationBreakDigest = null
                                        replayedRejectedNotificationBreakKey = null
                                        pendingRejectedNotificationRouteDigest = null
                                    }
                                },
                            ) { Text("Review current break") }
                            TextButton(
                                onClick = {
                                    if (
                                        clearValidatedRejectedNotificationRoute(
                                            pendingRejectedNotificationRouteDigest,
                                            onTimedBreakNotificationRouteConsumed,
                                        )
                                    ) {
                                        dismissedBreakKey = replayedRejectedNotificationBreakKey
                                        authorizedNotificationBreakDigest = null
                                        replayedRejectedNotificationBreakKey = null
                                        pendingRejectedNotificationRouteDigest = null
                                    }
                                },
                            ) { Text("Not now") }
                        }
                    }
                }
                if (state.pendingGoogleCalendarOutbound != null) {
                    Surface(
                        color = MaterialTheme.colorScheme.secondaryContainer,
                        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Icon(Icons.Outlined.CalendarMonth, contentDescription = null)
                            Column(
                                modifier = Modifier
                                    .weight(1f)
                                    .padding(horizontal = 10.dp),
                            ) {
                                Text(
                                    "Google Calendar publication saved",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                Text(
                                    googleCalendarOutboundState.message,
                                    style = MaterialTheme.typography.bodySmall,
                                    maxLines = 2,
                                )
                            }
                            TextButton(onClick = { showGooglePublicationReview = true }) {
                                Text("Review")
                            }
                        }
                    }
                }
                if (state.pendingGoogleSchedulePublication != null) {
                    Surface(
                        color = MaterialTheme.colorScheme.tertiaryContainer,
                        contentColor = MaterialTheme.colorScheme.onTertiaryContainer,
                    ) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Icon(Icons.Outlined.CalendarMonth, contentDescription = null)
                            Column(
                                modifier = Modifier.weight(1f).padding(horizontal = 10.dp),
                            ) {
                                Text(
                                    "Generated schedule publication saved",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                Text(
                                    googleSchedulePublicationState.message,
                                    style = MaterialTheme.typography.bodySmall,
                                    maxLines = 2,
                                )
                            }
                            TextButton(onClick = { showGoogleSchedulePublicationReview = true }) {
                                Text("View")
                            }
                        }
                    }
                }
                val active = state.activeItem
                val session = state.activeSession
                AnimatedVisibility(visible = active != null && session != null) {
                    if (active != null && session != null) {
                        ActiveSessionBar(
                            item = active,
                            session = session,
                            actionsEnabled = active.canonicalItemId == null ||
                                canonicalExecutionActionsEnabled,
                            onPause = { showPauseChooser = true },
                            onResume = viewModel::resumeActive,
                            onComplete = viewModel::completeActive,
                        )
                    }
                }
                DayWeaveNavigationBar(
                    selected = state.destination,
                    pendingSuggestionCount = state.pendingSuggestionCount,
                    onSelect = viewModel::navigate,
                )
            }
        },
        floatingActionButton = {
            if (state.destination != AppDestination.ASSISTANT) {
                FloatingActionButton(onClick = { showQuickCapture = true }) {
                    Icon(Icons.Outlined.Add, contentDescription = "Quick capture")
                }
            }
        },
        ) { innerPadding ->
            when (state.destination) {
            AppDestination.TODAY -> TodayScreen(
                state = state,
                syncState = effectiveCanonicalSyncState,
                canonicalExecutionActionsEnabled = canonicalExecutionActionsEnabled,
                reference = plannerClockReference,
                currentZone = plannerClockZone,
                onStart = viewModel::startItem,
                onPause = { showPauseChooser = true },
                onResume = viewModel::resumeActive,
                onComplete = viewModel::completeActive,
                onSkip = viewModel::skipActive,
                onLater = {
                    state.activeItem?.takeIf { it.canonicalItemId != null }?.let { active ->
                        moveLaterTargetId = active.id
                    }
                },
                onSkipScheduled = viewModel::skipScheduled,
                onLaterScheduled = { moveLaterTargetId = it },
                onRetryTerminalProjection = viewModel::retryTerminalProjection,
                onKeepLatestItem = viewModel::keepLatestItemAfterTerminalConflict,
                onEnergyCheckIn = viewModel::recordManualEnergyCheckIn,
                onClearManualEnergyCheckIn = viewModel::clearManualEnergyCheckIn,
                habitContent = {
                    TodayHabitsSection(
                        state = state,
                        syncState = habitSyncState,
                        reference = plannerClockReference,
                        currentZone = plannerClockZone,
                        onRefresh = viewModel::refreshHabits,
                        onRecordOutcome = viewModel::recordHabitOutcome,
                        onResolveMissed = viewModel::resolveMissedHabit,
                        onStartPause = viewModel::startHabitPause,
                        onResumePause = viewModel::resumeHabitPause,
                        onDiscardReviewedMutation =
                            viewModel::discardReviewedHabitMutation,
                    )
                },
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.CALENDAR -> CalendarScreen(
                state = state,
                reference = plannerClockReference,
                currentZone = plannerClockZone,
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.INBOX -> InboxScreen(
                state = state,
                onApprove = viewModel::approveSuggestion,
                onReject = viewModel::rejectSuggestion,
                onEdit = { editingSuggestion = it },
                proposalApplicationState = proposalApplicationState,
                onReviewProposal = viewModel::reviewProposal,
                onUndoProposal = viewModel::undoProposalApplication,
                onRecoverProposal = viewModel::recoverProposalApplication,
                syncState = suggestionSyncState,
                onRefresh = viewModel::refreshSuggestions,
                onConfigureConnection = { showApiConnection = true },
                canonicalActionsEnabled = canonicalAuthoringActionsEnabled,
                canonicalRetryEnabled = canonicalExecutionActionsEnabled,
                onNewCanonicalItem = {
                    canonicalEditorRoute = CanonicalItemEditorRoute.create()
                },
                onOpenCanonicalEditor = { canonicalEditorRoute = it },
                onTrashCanonicalItem = { itemId ->
                    viewModel.trashCanonicalItem(itemId, confirmed = true)
                },
                onRestoreCanonicalItem = viewModel::restoreCanonicalItem,
                onDiscardCanonicalMutation = viewModel::discardCanonicalAuthoringMutation,
                onCopyCanonicalConflict = viewModel::copyConflictedCanonicalDraft,
                onReviewLegacyDraft = { draft ->
                    canonicalEditorRoute = CanonicalItemEditorRoute.fromInbox(draft)
                },
                onRetryCanonicalAuthoring = viewModel::retryCanonicalAuthoring,
                googleOutboundBlocked = state.pendingGoogleCalendarOutbound != null ||
                    googleCalendarOutboundState.isBusy ||
                    state.pendingGoogleSchedulePublication?.stage?.let {
                        it != GoogleSchedulePublicationStage.ACCEPTED
                    } == true,
                googlePublishingTargets = viewModel::googleCalendarPublishingTargets,
                onRequestGooglePublication = { itemId, targets ->
                    if (
                        targets.isNotEmpty() &&
                        viewModel.resetGoogleCalendarPublicationPresentation()
                    ) {
                        googlePublicationReview = GoogleCalendarPublicationReview(
                            itemId = itemId,
                            selectedTarget = targets.singleOrNull(),
                        )
                        showGooglePublicationReview = true
                    }
                },
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.ASSISTANT -> AssistantScreen(
                state = state,
                assistantState = assistantState,
                onSend = viewModel::sendAssistantMessage,
                onStop = viewModel::cancelAssistantTurn,
                onConfigureConnection = { showApiConnection = true },
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.MORE -> MoreScreen(
                state = state,
                onToggleCompleted = viewModel::toggleCompleted,
                onToggleQuietSuggestions = viewModel::toggleQuietSuggestions,
                onToggleDynamicColor = viewModel::toggleDynamicColor,
                scheduleCompositionProfileUpdateState =
                    scheduleCompositionProfileUpdateState,
                onUpdateScheduleCompositionProfile =
                    viewModel::updateScheduleCompositionProfile,
                onAcknowledgeScheduleCompositionProfileUpdate =
                    viewModel::acknowledgeScheduleCompositionProfileUpdate,
                suggestionSyncState = suggestionSyncState,
                canonicalSyncState = effectiveCanonicalSyncState,
                deviceSessionsState = deviceSessionsState,
                onRefreshDeviceSessions = viewModel::refreshDeviceSessions,
                deviceSessionRevocationConfirmationProvider =
                    viewModel::deviceSessionRevocationConfirmation,
                onRevokeRemoteDeviceSession = viewModel::revokeRemoteDeviceSession,
                onSignOutCurrentDeviceSession = viewModel::signOutDeviceSessionIfCurrent,
                googleAccountState = googleAccountState,
                googleCalendarImportState = googleCalendarImportState,
                energySignalState = energySignalState,
                appLockState = appLockState,
                onConfigureApiConnection = { showApiConnection = true },
                onConnectGoogle = viewModel::connectGoogleAccount,
                onRefreshGoogle = viewModel::refreshGoogleAccounts,
                onRestartGoogleAuthorization = viewModel::restartGoogleAuthorization,
                onOpenGoogleAuthorization = { url ->
                    viewModel.openGoogleAuthorization(url) { currentUrl ->
                        uriHandler.openUri(currentUrl)
                    }
                },
                onReauthorizeGoogle = viewModel::reauthorizeGoogleAccount,
                onEnableGoogleCalendarPublishing =
                    viewModel::enableGoogleCalendarPublishing,
                onEnableGoogleTasksPublishing = viewModel::enableGoogleTasksPublishing,
                authorizationRecoveryResetConfirmationProvider =
                    viewModel::googleAuthorizationRecoveryResetConfirmation,
                onResetGoogleAuthorizationRecovery =
                    viewModel::resetGoogleAuthorizationRecovery,
                authorizationRecoveryDiscardConfirmationProvider =
                    viewModel::googleAuthorizationRecoveryDiscardConfirmation,
                onDiscardGoogleAuthorizationRecovery =
                    viewModel::discardOrphanedGoogleAuthorizationRecovery,
                onSetGooglePaused = viewModel::setGoogleAccountPaused,
                onRequestGoogleDisconnect = { disconnectingGoogleAccount = it },
                onDiscoverGoogleSources = viewModel::discoverGoogleSources,
                onRefreshGoogleImport = viewModel::refreshGoogleImport,
                onConfigureGoogleSource = viewModel::configureGoogleSource,
                onPublishGeneratedSchedule = {
                    selectedGoogleScheduleTarget = viewModel.googleSchedulePublishingTargets()
                        .singleOrNull()
                    showGoogleSchedulePublicationReview = true
                },
                schedulePublicationHasRecovery =
                    state.pendingGoogleSchedulePublication != null,
                onToggleHealthConnect = { enabled ->
                    when {
                        !enabled -> viewModel.disableHealthConnect()
                        energySignalState.permissionGranted -> viewModel.enableHealthConnect()
                        energySignalState.availability == EnergyProviderAvailability.AVAILABLE ->
                            healthPermissionLauncher.launch(viewModel.healthConnectPermissions)
                        energySignalState.availability == EnergyProviderAvailability.UPDATE_REQUIRED ->
                            context.startActivitySafely(
                                HealthConnectIntents.installOrUpdate(context),
                                HealthConnectIntents.browserFallback(),
                            )
                    }
                },
                onRefreshHealthConnect = viewModel::refreshEnergySignal,
                onManageHealthConnectAccess = {
                    context.startActivitySafely(HealthConnectIntents.manageAccess())
                },
                onInstallHealthConnect = {
                    context.startActivitySafely(
                        HealthConnectIntents.installOrUpdate(context),
                        HealthConnectIntents.browserFallback(),
                    )
                },
                onSetAppLockEnabled = onSetAppLockEnabled,
                onSetAppLockTimeout = onSetAppLockTimeout,
                onLockNow = onLockNow,
                onOpenDeviceSecuritySettings = onOpenDeviceSecuritySettings,
                canonicalPrivacyActionsEnabled = canonicalExecutionActionsEnabled &&
                    state.canonicalSyncOrigin != null &&
                    effectiveCanonicalSyncState.phase in setOf(
                        CanonicalSyncPhase.READY,
                        CanonicalSyncPhase.CONNECTED,
                ),
                onSetCanonicalItemSensitive = viewModel::setCanonicalItemSensitive,
                habitStatisticsContent = {
                    HabitStatisticsSection(
                        state = state,
                        syncState = habitSyncState,
                        reference = plannerClockReference,
                        currentZone = plannerClockZone,
                        onRefreshAnalytics = viewModel::refreshHabitAnalytics,
                        onStartPause = viewModel::startHabitPause,
                        onResumePause = viewModel::resumeHabitPause,
                    )
                },
                modifier = Modifier.padding(innerPadding),
            )
            }
        }
    }

    if (showOnboardingProfileEditor) {
        PlanningProfileEditorDialog(
            currentProfile = state.scheduleCompositionProfile,
            editBlockedMessage = planningProfileEditBlockedMessage(
                state = state,
                canonicalActionBusy = effectiveCanonicalSyncState.isBusy,
            ),
            updateState = scheduleCompositionProfileUpdateState,
            onSave = viewModel::updateScheduleCompositionProfile,
            onDismiss = {
                if (!scheduleCompositionProfileUpdateState.isSaving) {
                    showOnboardingProfileEditor = false
                    viewModel.acknowledgeScheduleCompositionProfileUpdate()
                }
            },
        )
    }

    if (showQuickCapture) {
        QuickCaptureSheet(
            onDismiss = { showQuickCapture = false },
            onCapture = viewModel::quickCapture,
            onContinueWithDetails = { title, kind, isSensitive ->
                canonicalEditorRoute = CanonicalItemEditorRoute.create(
                    title = title,
                    kind = kind,
                    isSensitive = isSensitive,
                )
            },
        )
    }

    canonicalEditorRoute?.let { route ->
        CanonicalItemEditorSheet(
            route = route,
            parentOptions = canonicalParentOptions(state, route.itemId),
            dependencyContext = canonicalDependencyEditorContext(state, route.itemId),
            onDismiss = { canonicalEditorRoute = null },
            onSave = { draft ->
                val saved = when (route.mode) {
                    CanonicalItemEditorMode.CREATE -> route.sourceInboxId?.let { inboxId ->
                        viewModel.convertInboxDraft(inboxId, route.itemId, draft)
                    } ?: if (
                        activeOnboardingState?.currentStep == OnboardingStep.FIRST_ITEM
                    ) {
                        viewModel.createOnboardingFirstItem(route.itemId, draft)
                    } else {
                        viewModel.createCanonicalItem(route.itemId, draft)
                    }
                    CanonicalItemEditorMode.REPLACE ->
                        viewModel.replaceCanonicalItem(route.itemId, draft)
                    CanonicalItemEditorMode.UPDATE_PENDING ->
                        viewModel.updatePendingCanonicalItem(
                            mutationId = requireNotNull(route.mutationId),
                            draft = draft,
                        )
                }
                if (saved) canonicalEditorRoute = null
                saved
            },
        )
    }

    if (showPauseChooser) {
        PauseChooserDialog(
            onDismiss = { showPauseChooser = false },
            onPause = { minutes ->
                viewModel.pauseActive(minutes)
                showPauseChooser = false
            },
        )
    }

    val requestedMoveTarget = moveLaterTargetId?.let { targetId ->
        state.schedule.firstOrNull { it.id == targetId }
    }
    val requestedMoveZone = requestedMoveTarget?.let { target ->
        if (target.canonicalItemId == null) {
            plannerClockZone
        } else {
            listOfNotNull(state.schedulePlanningZoneId, target.planningZoneId)
                .firstNotNullOfOrNull { raw -> runCatching { ZoneId.of(raw) }.getOrNull() }
        }
    }
    val requestedServerAuthoritativeExecution = requestedMoveTarget?.let { target ->
        state.activeSession?.itemId == target.id &&
            state.canonicalExecutionSession?.id ==
            state.activeSession?.canonicalExecutionSessionId
    } == true
    val requestedPlanningHorizon = if (
        requestedMoveTarget != null && requestedMoveZone != null &&
        !requestedServerAuthoritativeExecution
    ) {
        state.scheduleDisplayHorizon(
            reference = plannerClockReference,
            currentZone = requestedMoveZone,
        )
    } else {
        null
    }
    val canRenderMoveLater = requestedMoveTarget != null && requestedMoveZone != null &&
        (requestedServerAuthoritativeExecution || requestedPlanningHorizon != null)
    LaunchedEffect(
        moveLaterTargetId,
        requestedMoveTarget,
        requestedMoveZone,
        requestedServerAuthoritativeExecution,
        requestedPlanningHorizon,
    ) {
        if (moveLaterTargetId != null && !canRenderMoveLater) moveLaterTargetId = null
    }
    moveLaterTargetId?.takeIf { canRenderMoveLater }?.let { targetId ->
        val target = requireNotNull(requestedMoveTarget)
        val moveZone = requireNotNull(requestedMoveZone)
            MoveLaterChooserDialog(
                itemTitle = target.title,
                itemIsSensitive = target.isSensitive,
                placementMode = when {
                    target.status in setOf(ItemStatus.ACTIVE, ItemStatus.PAUSED) ->
                        MoveLaterPlacementMode.EXACT
                    target.occurrenceId != null -> MoveLaterPlacementMode.RECOMPOSED_WINDOW
                    else -> MoveLaterPlacementMode.EARLIEST_START
                },
                zoneId = moveZone,
                referenceNow = plannerClockReference,
                planningHorizon = requestedPlanningHorizon,
                notBefore = target.timelineInstant(),
                serverAuthoritativeExecution = requestedServerAuthoritativeExecution,
                assessMove = { moveStart ->
                    state.assessMoveLater(targetId, moveStart)
                },
                onDismiss = { moveLaterTargetId = null },
                onMove = { moveStart, approval ->
                    if (state.activeSession?.itemId == targetId) {
                        viewModel.doActiveLater(moveStart)
                    } else {
                        viewModel.doScheduledLater(targetId, moveStart, approval)
                    }
                    moveLaterTargetId = null
                },
            )
    }

    state.pendingExecutionDeferIntent?.let { intent ->
        val assessment = intent.assessment
        if (
            assessment?.approvalRequired == true &&
            intent.approvedAssessmentDigest != assessment.assessmentDigest
        ) {
            val source = state.schedule.firstOrNull { it.id == intent.focusedBlockId }
            val zone = listOfNotNull(state.schedulePlanningZoneId, source?.planningZoneId)
                .firstNotNullOfOrNull { raw -> runCatching { ZoneId.of(raw) }.getOrNull() }
                ?: ZoneId.systemDefault()
            val sensitiveBlockIds = state.schedule.asSequence()
                .filter { it.isSensitive }
                .map { it.id }
                .toSet()
            ExecutionDeferApprovalDialog(
                assessment = assessment,
                sourceIsSensitive = source?.isSensitive != false,
                sensitiveBlockIds = sensitiveBlockIds,
                zoneId = zone,
                onApprove = viewModel::approveActiveLater,
                onKeepPaused = viewModel::cancelActiveLater,
            )
        }
    }

    state.pendingExecutionDeferIntent?.let { intent ->
        val assessment = intent.assessment
        val isWaitingForApproval = assessment?.approvalRequired == true &&
            intent.approvedAssessmentDigest != assessment.assessmentDigest
        if (
            !isWaitingForApproval && state.pendingExecutionCommand == null &&
            executionSyncState.phase in setOf(
                CanonicalSyncPhase.ERROR,
                CanonicalSyncPhase.OFFLINE,
                CanonicalSyncPhase.AUTH_REQUIRED,
            )
        ) {
            val source = state.schedule.firstOrNull { it.id == intent.focusedBlockId }
            val zone = listOfNotNull(state.schedulePlanningZoneId, source?.planningZoneId)
                .firstNotNullOfOrNull { raw -> runCatching { ZoneId.of(raw) }.getOrNull() }
                ?: ZoneId.systemDefault()
            ExecutionDeferPendingDialog(
                moveStart = intent.moveStart,
                statusMessage = executionSyncState.message,
                zoneId = zone,
                sourceIsSensitive = source?.isSensitive != false,
                onRetry = viewModel::refreshExecution,
                onKeepPaused = viewModel::cancelActiveLater,
            )
        }
    }

    val endedBreakIdentity = state.authoritativeTimedBreakNotificationIdentity()
    val endedBreak = state.activeSession?.takeIf {
        it.timedBreakEnded && (
            endedBreakIdentity == null ||
                endedBreakIdentity.digest != state.acknowledgedBreakEndDigest
            )
    }
    val endedBreakKey = endedBreak?.let { session ->
        endedBreakIdentity?.digest
            ?: "local:${session.itemId}:${session.pauseUntilEpochMillis}"
    }
    val liveBreakRouteKey = listOf(
        endedBreakIdentity?.digest,
        state.activeSession?.timedBreakEnded?.toString(),
        state.acknowledgedBreakEndDigest,
    ).joinToString(separator = ":")
    val durableBreakIdentity = durableState?.authoritativeTimedBreakNotificationIdentity()
    val durableBreakRouteKey = listOf(
        durableBreakIdentity?.digest,
        durableState?.activeSession?.timedBreakEnded?.toString(),
        durableState?.acknowledgedBreakEndDigest,
    ).joinToString(separator = ":")
    val durableBreakStateAvailable = timedBreakNotificationRouteStateAvailable(durableState)
    LaunchedEffect(
        timedBreakNotificationRouteDigest,
        durableBreakStateAvailable,
        durableBreakRouteKey,
        liveBreakRouteKey,
        endedBreakKey,
    ) {
        val encryptedState = durableState
        if (encryptedState != null) {
            timedBreakNotificationRouteDigest?.let { digest ->
                val initiallyMatches = isExactTimedBreakResolutionCurrent(
                    durableState = encryptedState,
                    liveState = state,
                    identityDigest = digest,
                )
                when (
                    timedBreakNotificationPresentationDecision(
                        consumption = viewModel.consumeTimedBreakNotificationRoute(digest),
                        initiallyMatchedExactBreak = initiallyMatches,
                        currentEndedBreakKey = endedBreakKey,
                    )
                ) {
                    TimedBreakNotificationPresentationDecision.PRESENT_EXACT_BREAK -> {
                        dismissedBreakKey = null
                        authorizedNotificationBreakDigest = digest
                        rejectedNotificationLaunchBreakKey = null
                        replayedRejectedNotificationBreakKey = null
                        pendingRejectedNotificationRouteDigest = null
                        onTimedBreakNotificationRouteConsumed(digest)
                    }
                    TimedBreakNotificationPresentationDecision.OFFER_CURRENT_BREAK_REVIEW -> {
                        authorizedNotificationBreakDigest = null
                        rejectedNotificationLaunchBreakKey = endedBreakKey
                        replayedRejectedNotificationBreakKey = null
                        // Keep stale A durable until the user explicitly reviews or dismisses B.
                        // A crash before that choice reconstructs this generic fence instead of
                        // letting A indirectly retarget the ordinary B resolver.
                        pendingRejectedNotificationRouteDigest = digest
                    }
                    TimedBreakNotificationPresentationDecision
                        .OFFER_CURRENT_BREAK_REVIEW_NON_MODAL -> {
                        authorizedNotificationBreakDigest = null
                        rejectedNotificationLaunchBreakKey = null
                        replayedRejectedNotificationBreakKey = endedBreakKey
                        pendingRejectedNotificationRouteDigest = digest
                    }
                    TimedBreakNotificationPresentationDecision.SUPPRESS_CURRENT_BREAK -> {
                        authorizedNotificationBreakDigest = null
                        rejectedNotificationLaunchBreakKey = null
                        replayedRejectedNotificationBreakKey = null
                        pendingRejectedNotificationRouteDigest = null
                        if (endedBreakKey != null) dismissedBreakKey = endedBreakKey
                        onTimedBreakNotificationRouteConsumed(digest)
                    }
                    TimedBreakNotificationPresentationDecision.RETRY_AFTER_STATE_SETTLES -> Unit
                }
            }
        }
    }
    LaunchedEffect(endedBreakKey) {
        val transition = reconcileTimedBreakNotificationAuthorization(
            authorizedDigest = authorizedNotificationBreakDigest,
            endedBreakKey = endedBreakKey,
        )
        if (transition.authorizedDigest != authorizedNotificationBreakDigest) {
            authorizedNotificationBreakDigest = transition.authorizedDigest
            rejectedNotificationLaunchBreakKey = transition.changedBreakReviewKey
        }
        if (
            replayedRejectedNotificationBreakKey != null &&
            replayedRejectedNotificationBreakKey != endedBreakKey
        ) {
            replayedRejectedNotificationBreakKey = null
        }
    }
    val presentationSuppressionKey = rejectedNotificationLaunchBreakKey
        ?: replayedRejectedNotificationBreakKey
    val showCurrentBreakReview = shouldOfferCurrentTimedBreakReview(
        endedBreakKey = endedBreakKey,
        pendingNotificationDigest = timedBreakNotificationRouteDigest,
        rejectedNotificationLaunchBreakKey = rejectedNotificationLaunchBreakKey,
        validatedRejectedRouteDigest = pendingRejectedNotificationRouteDigest,
    )
    if (showCurrentBreakReview) {
        AlertDialog(
            onDismissRequest = {},
            title = { Text("Reminder changed") },
            text = {
                Text(
                    "That reminder is no longer current. Review the current break separately.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        if (
                            clearValidatedRejectedNotificationRoute(
                                pendingRejectedNotificationRouteDigest,
                                onTimedBreakNotificationRouteConsumed,
                            )
                        ) {
                            // This explicit second step authorizes the normal resolver for B; the
                            // stale notification A itself never receives presentation authority.
                            dismissedBreakKey = null
                            authorizedNotificationBreakDigest = null
                            rejectedNotificationLaunchBreakKey = null
                            pendingRejectedNotificationRouteDigest = null
                        }
                    },
                ) { Text("Review current break") }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        if (
                            clearValidatedRejectedNotificationRoute(
                                pendingRejectedNotificationRouteDigest,
                                onTimedBreakNotificationRouteConsumed,
                            )
                        ) {
                            dismissedBreakKey = endedBreakKey
                            authorizedNotificationBreakDigest = null
                            rejectedNotificationLaunchBreakKey = null
                            pendingRejectedNotificationRouteDigest = null
                        }
                    },
                ) { Text("Not now") }
            },
        )
    }
    if (
        !showCurrentBreakReview &&
        endedBreak != null && shouldPresentTimedBreakResolution(
            endedBreakKey = endedBreakKey,
            dismissedBreakKey = dismissedBreakKey,
            pendingNotificationDigest = timedBreakNotificationRouteDigest,
            authorizedNotificationDigest = authorizedNotificationBreakDigest,
            rejectedNotificationLaunchBreakKey = presentationSuppressionKey,
        )
    ) {
        BreakEndedDialog(
            onResume = {
                viewModel.resumeActive()
            },
            onExtend = {
                viewModel.pauseActive(10)
            },
            onKeepPaused = {
                val canonicalDigest = endedBreakIdentity?.digest
                if (canonicalDigest == null) {
                    if (endedBreak.canonicalExecutionSessionId == null) {
                        // Device-local timers have no server lease or notification receipt.
                        dismissedBreakKey = endedBreakKey
                    } else {
                        // A canonical projection mismatch cannot authorize a local-only dismissal.
                        viewModel.refreshExecution()
                    }
                } else {
                    coroutineScope.launch {
                        if (viewModel.keepTimedBreakPaused(canonicalDigest)) {
                            dismissedBreakKey = canonicalDigest
                        }
                    }
                }
            },
        )
    }

    editingSuggestion?.let { suggestion ->
        EditSuggestionDialog(
            suggestion = suggestion,
            onDismiss = { editingSuggestion = null },
            onSave = { title, summary ->
                viewModel.updateSuggestion(suggestion.id, title, summary)
                editingSuggestion = null
            },
        )
    }

    proposalApplicationState.preview?.let { preview ->
        val proposalId = proposalApplicationState.activeProposalId
            ?: preview.proposals.singleOrNull()?.proposalId
            ?: return@let
        val proposalTitle = state.suggestions.firstOrNull { it.id == proposalId }?.title
            ?: "Proposal"
        ProposalReviewDialog(
            proposalTitle = proposalTitle,
            state = proposalApplicationState,
            onDismiss = { viewModel.discardProposalReview(proposalId) },
            onRegenerate = { viewModel.reviewProposal(proposalId) },
            onApply = viewModel::applyReviewedProposal,
        )
    }

    if (showApiConnection) {
        ApiConnectionDialog(
            authState = deviceAuthState,
            credentialReplacementBlocked = viewModel.hasCredentialReplacementBlocker(),
            googleAuthorizationRecoveryDiscardRequired =
                googleAccountState.authorizationRecoveryDiscardRequired,
            onDismiss = { showApiConnection = false },
            onUpgradeWithBootstrap = viewModel::upgradeDeviceAuthentication,
            onConsumeEnrollmentCode = viewModel::consumeDeviceEnrollmentCode,
            onRetryPending = viewModel::retryDeviceAuthentication,
            onRevokeAndSignOut = viewModel::signOutDeviceSession,
            onDestroyLocalOnly = {
                viewModel.destroyLocalDeviceAuthentication(confirmed = true)
            },
            googleAuthorizationRecoveryDiscardConfirmationProvider =
                viewModel::googleAuthorizationRecoveryDiscardConfirmation,
            onDiscardGoogleAuthorizationRecovery =
                viewModel::discardOrphanedGoogleAuthorizationRecovery,
        )
    }

    disconnectingGoogleAccount?.let { account ->
        AlertDialog(
            onDismissRequest = { disconnectingGoogleAccount = null },
            title = { Text("Disconnect Google?") },
            text = {
                Text(
                    "DayWeave will revoke its Google grant for ${account.label} and stop Calendar and Tasks sync. Your Google data will not be deleted.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        viewModel.disconnectGoogleAccount(account.id)
                        disconnectingGoogleAccount = null
                    },
                ) {
                    Text("Disconnect")
                }
            },
            dismissButton = {
                TextButton(onClick = { disconnectingGoogleAccount = null }) {
                    Text("Cancel")
                }
            },
        )
    }

    if (
        showGooglePublicationReview &&
        (
            googlePublicationReview != null ||
                state.pendingGoogleCalendarOutbound != null ||
                googleCalendarOutboundState.phase == GoogleCalendarOutboundPhase.ACCEPTED
        )
    ) {
        val currentTargets = googlePublicationReview?.let { review ->
            viewModel.googleCalendarPublishingTargets(review.itemId)
        }.orEmpty()
        val selectedTarget = googlePublicationReview?.selectedTarget?.takeIf {
            it in currentTargets
        } ?: currentTargets.singleOrNull()
        val reviewDestination = googlePublicationReview?.selectedTarget
            ?: viewModel.pendingGoogleCalendarDestination()
        val reviewTarget = selectedTarget ?: reviewDestination
        val reviewItemId = googlePublicationReview?.itemId
            ?: state.pendingGoogleCalendarOutbound?.itemId
        val reviewItemTitle = reviewItemId?.let { itemId ->
            state.canonicalItems.singleOrNull { it.id == itemId }?.title
                ?: state.canonicalRecentlyDeleted.singleOrNull { it.id == itemId }
                    ?.lastKnownItem?.title
        }
        GoogleCalendarOutboundReviewSheet(
            state = googleCalendarOutboundState,
            targets = currentTargets,
            selectedTarget = reviewTarget,
            reviewDestinationDisplayName = reviewDestination?.displayName,
            reviewItemTitle = reviewItemTitle,
            approvalConfirmation = viewModel.googleCalendarApprovalConfirmation(),
            canRecover = state.pendingGoogleCalendarOutbound != null &&
                googleCalendarOutboundState.phase in setOf(
                    GoogleCalendarOutboundPhase.AUTH_REQUIRED,
                    GoogleCalendarOutboundPhase.OFFLINE,
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                ),
            canDiscardExpiredRecovery = state.pendingGoogleCalendarOutbound
                ?.canDiscardExpiredAt(Instant.ofEpochMilli(googleOutboundClockMillis)) == true,
            onTargetSelected = { target ->
                googlePublicationReview?.let { review ->
                    googlePublicationReview = GoogleCalendarPublicationReview(
                        itemId = review.itemId,
                        selectedTarget = target,
                    )
                }
            },
            onRequestPreview = { target ->
                googlePublicationReview?.itemId?.let { itemId ->
                    viewModel.prepareGoogleCalendarPreview(itemId, target)
                }
            },
            onApproveAndQueue = { confirmation ->
                viewModel.approveGoogleCalendarPreview(confirmation)
            },
            onRecover = { viewModel.recoverGoogleCalendarOutbound() },
            onDiscardExpiredRecovery = {
                viewModel.discardExpiredGoogleCalendarOutbound()
            },
            onDismissRequest = {
                showGooglePublicationReview = false
                googlePublicationReview = null
                viewModel.resetGoogleCalendarPublicationPresentation()
            },
        )
    }

    if (showGoogleSchedulePublicationReview) {
        val scheduleTargets = viewModel.googleSchedulePublishingTargets()
        val selectedScheduleTarget = selectedGoogleScheduleTarget?.takeIf {
            it in scheduleTargets
        } ?: scheduleTargets.singleOrNull()
        val savedScheduleDestination = viewModel.pendingGoogleScheduleDestination()
        GoogleSchedulePublicationReviewSheet(
            state = googleSchedulePublicationState,
            targets = scheduleTargets,
            selectedTarget = selectedScheduleTarget ?: savedScheduleDestination,
            savedDestinationDisplayName = savedScheduleDestination?.displayName,
            approvalConfirmation =
                viewModel.googleSchedulePublicationApprovalConfirmation(),
            recoveryStage = state.pendingGoogleSchedulePublication?.stage,
            canRecover = state.pendingGoogleSchedulePublication != null &&
                (
                    state.pendingGoogleSchedulePublication?.stage ==
                        GoogleSchedulePublicationStage.APPROVED ||
                        googleSchedulePublicationState.phase in setOf(
                            GoogleSchedulePublicationPhase.PENDING,
                            GoogleSchedulePublicationPhase.AUTH_REQUIRED,
                            GoogleSchedulePublicationPhase.OFFLINE,
                            GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
                        )
                ),
            canDiscardExpiredRecovery = state.pendingGoogleSchedulePublication
                ?.canDiscardExpiredAt(Instant.ofEpochMilli(googleOutboundClockMillis)) == true,
            canDismissSettled = state.pendingGoogleSchedulePublication?.status?.isTerminal == true,
            onTargetSelected = { selectedGoogleScheduleTarget = it },
            onRequestPreview = viewModel::prepareGoogleSchedulePublicationPreview,
            onApproveAndQueue = viewModel::approveGoogleSchedulePublication,
            onRecover = { viewModel.recoverGoogleSchedulePublication() },
            onReplayApproved = { viewModel.replayApprovedGoogleSchedulePublication() },
            onDiscardExpiredRecovery = {
                if (viewModel.discardExpiredGoogleSchedulePublication()) {
                    showGoogleSchedulePublicationReview = false
                    selectedGoogleScheduleTarget = null
                }
            },
            onDismissSettled = {
                if (viewModel.dismissSettledGoogleSchedulePublication()) {
                    showGoogleSchedulePublicationReview = false
                    selectedGoogleScheduleTarget = null
                }
            },
            onDismissRequest = {
                showGoogleSchedulePublicationReview = false
                selectedGoogleScheduleTarget = null
            },
        )
    }
}

internal fun plannerClockDelayMillis(
    reference: Instant,
    zoneId: ZoneId,
    exactHorizonEnd: Instant?,
): Long {
    val nowMillis = reference.toEpochMilli()
    val nextMinuteMillis = Math.addExact(
        nowMillis - Math.floorMod(nowMillis, PLANNER_CLOCK_TICK_MILLIS),
        PLANNER_CLOCK_TICK_MILLIS,
    )
    val nextLocalDayMillis = reference.atZone(zoneId).toLocalDate().plusDays(1)
        .atStartOfDay(zoneId)
        .toInstant()
        .toEpochMilli()
    val exactEdgeMillis = exactHorizonEnd?.toEpochMilli()?.takeIf { it > nowMillis }
    val wakeAt = listOfNotNull(nextMinuteMillis, nextLocalDayMillis, exactEdgeMillis).min()
    return (wakeAt - nowMillis).coerceIn(1L, PLANNER_CLOCK_TICK_MILLIS)
}

private class GoogleCalendarPublicationReview(
    val itemId: String,
    val selectedTarget: GoogleCalendarOutboundTargetOption?,
) {
    override fun toString(): String = "GoogleCalendarPublicationReview(<redacted>)"
}

private const val PLANNER_CLOCK_TICK_MILLIS = 60_000L
private const val EXECUTION_REFRESH_INTERVAL_MILLIS = 30_000L

/** A stream bug or protocol failure can never cancel the independent polling fallback. */
internal suspend fun runForegroundInvalidationWorkers(
    executionInvalidationStream: (suspend () -> Unit)?,
    canonicalItemInvalidations: (suspend () -> Unit)?,
    scheduleInvalidations: (suspend () -> Unit)? = null,
    habitInvalidations: (suspend () -> Unit)? = null,
    polling: suspend () -> Unit,
) = supervisorScope {
    listOfNotNull(
        executionInvalidationStream,
        canonicalItemInvalidations,
        scheduleInvalidations,
        habitInvalidations,
    ).forEach {
        collectInvalidations ->
        launch {
            try {
                collectInvalidations()
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                // Stream status is intentionally silent; foreground polling remains authoritative.
            }
        }
    }
    polling()
}

private fun Context.startActivitySafely(primary: Intent, fallback: Intent? = null) {
    try {
        startActivity(primary)
    } catch (_: ActivityNotFoundException) {
        fallback?.let { secondary ->
            try {
                startActivity(secondary)
            } catch (_: ActivityNotFoundException) {
                // The integration remains off; core planning is intentionally unaffected.
            }
        }
    }
}
