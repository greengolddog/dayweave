package com.greengolddog.dayweave.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import androidx.compose.animation.AnimatedVisibility
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
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
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.health.EnergyProviderAvailability
import com.greengolddog.dayweave.health.HealthConnectIntents
import com.greengolddog.dayweave.security.AppLockController
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.security.AppLockTimeout
import com.greengolddog.dayweave.state.DayWeaveViewModel
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.ui.components.ActiveSessionBar
import com.greengolddog.dayweave.ui.components.ApiConnectionDialog
import com.greengolddog.dayweave.ui.components.AppLockedScreen
import com.greengolddog.dayweave.ui.components.BreakEndedDialog
import com.greengolddog.dayweave.ui.components.EditSuggestionDialog
import com.greengolddog.dayweave.ui.components.PauseChooserDialog
import com.greengolddog.dayweave.ui.components.ProposalReviewDialog
import com.greengolddog.dayweave.ui.components.QuickCaptureSheet
import com.greengolddog.dayweave.ui.navigation.DayWeaveNavigationBar
import com.greengolddog.dayweave.ui.screens.AssistantScreen
import com.greengolddog.dayweave.ui.screens.CalendarScreen
import com.greengolddog.dayweave.ui.screens.InboxScreen
import com.greengolddog.dayweave.ui.screens.MoreScreen
import com.greengolddog.dayweave.ui.screens.TodayScreen
import com.greengolddog.dayweave.ui.theme.DayWeaveTheme
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

@Composable
fun DayWeaveApp(
    appLockController: AppLockController,
    onRequestUnlock: () -> Unit,
    onSetAppLockEnabled: (Boolean) -> Unit,
    onSetAppLockTimeout: (AppLockTimeout) -> Unit,
    onLockNow: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
) {
    val appLockState by appLockController.state.collectAsStateWithLifecycle()
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
            UnlockedDayWeaveApp(
                appLockState = appLockState,
                onSetAppLockEnabled = onSetAppLockEnabled,
                onSetAppLockTimeout = onSetAppLockTimeout,
                onLockNow = onLockNow,
                onOpenDeviceSecuritySettings = onOpenDeviceSecuritySettings,
            )
        },
    )
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
            )
            PlannerLoadState.PERSISTENCE_FAILED -> PlannerPersistenceFailureScreen()
        }
    }
}

@Composable
private fun PlannerRestoreScreen() {
    Column(
        modifier = Modifier.fillMaxSize(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        CircularProgressIndicator()
        Text(
            text = "Opening your encrypted plan…",
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
) {
    val lifecycleOwner = LocalLifecycleOwner.current
    val state by viewModel.state.collectAsStateWithLifecycle()
    val suggestionSyncState by viewModel.suggestionSyncState.collectAsStateWithLifecycle()
    val proposalApplicationState by viewModel.proposalApplicationState.collectAsStateWithLifecycle()
    val canonicalSyncState by viewModel.canonicalSyncState.collectAsStateWithLifecycle()
    val executionSyncState by viewModel.executionSyncState.collectAsStateWithLifecycle()
    val googleAccountState by viewModel.googleAccountState.collectAsStateWithLifecycle()
    val energySignalState by viewModel.energySignalState.collectAsStateWithLifecycle()
    val deviceAuthState by viewModel.deviceAuthState.collectAsStateWithLifecycle()
    val context = LocalContext.current
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
        !canonicalSyncState.isBusy && !executionSyncState.isBusy &&
            !proposalApplicationState.isBusy &&
            state.pendingCanonicalMutation == null && state.pendingExecutionCommand == null &&
            state.pendingProposalApplicationMutation == null
    var showQuickCapture by remember { mutableStateOf(false) }
    var showPauseChooser by remember { mutableStateOf(false) }
    var showApiConnection by remember { mutableStateOf(false) }
    var editingSuggestion by remember { mutableStateOf<PlanningSuggestion?>(null) }
    var disconnectingGoogleAccount by remember { mutableStateOf<GoogleAccountSummary?>(null) }
    var dismissedBreakKey by rememberSaveable { mutableStateOf<String?>(null) }

    LaunchedEffect(lifecycleOwner, viewModel) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            viewModel.refreshExecution()
            viewModel.refreshGoogleAccounts()
            viewModel.refreshEnergySignal()
            while (isActive) {
                delay(EXECUTION_REFRESH_INTERVAL_MILLIS)
                viewModel.refreshExecution()
            }
        }
    }

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
                onStart = viewModel::startItem,
                onPause = { showPauseChooser = true },
                onResume = viewModel::resumeActive,
                onComplete = viewModel::completeActive,
                onSkip = viewModel::skipActive,
                onLater = viewModel::doActiveLater,
                onRetryTerminalProjection = viewModel::retryTerminalProjection,
                onKeepLatestItem = viewModel::keepLatestItemAfterTerminalConflict,
                onEnergyCheckIn = viewModel::recordManualEnergyCheckIn,
                onClearManualEnergyCheckIn = viewModel::clearManualEnergyCheckIn,
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.CALENDAR -> CalendarScreen(
                state = state,
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
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.ASSISTANT -> AssistantScreen(
                state = state,
                onSend = viewModel::sendAssistantMessage,
                modifier = Modifier.padding(innerPadding),
            )
            AppDestination.MORE -> MoreScreen(
                state = state,
                onToggleCompleted = viewModel::toggleCompleted,
                onToggleQuietSuggestions = viewModel::toggleQuietSuggestions,
                onToggleDynamicColor = viewModel::toggleDynamicColor,
                suggestionSyncState = suggestionSyncState,
                canonicalSyncState = effectiveCanonicalSyncState,
                googleAccountState = googleAccountState,
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
                onSetGooglePaused = viewModel::setGoogleAccountPaused,
                onRequestGoogleDisconnect = { disconnectingGoogleAccount = it },
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
                modifier = Modifier.padding(innerPadding),
            )
        }
    }

    if (showQuickCapture) {
        QuickCaptureSheet(
            onDismiss = { showQuickCapture = false },
            onCapture = viewModel::quickCapture,
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

    val endedBreak = state.activeSession?.takeIf { it.timedBreakEnded }
    val endedBreakKey = endedBreak?.let { session ->
        "${session.canonicalExecutionSessionId ?: session.itemId}:${session.pauseUntilEpochMillis}"
    }
    if (endedBreak != null && endedBreakKey != dismissedBreakKey) {
        BreakEndedDialog(
            onResume = {
                dismissedBreakKey = endedBreakKey
                viewModel.resumeActive()
            },
            onExtend = {
                dismissedBreakKey = endedBreakKey
                viewModel.pauseActive(10)
            },
            onKeepPaused = { dismissedBreakKey = endedBreakKey },
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
            credentialReplacementBlocked = state.pendingSchedulePublication != null ||
                state.pendingProposalApplicationMutation != null ||
                state.pendingCanonicalMutation != null ||
                state.pendingExecutionCommand != null ||
                state.terminalExecutionOutcomes.values.any {
                    it.requiresCanonicalItemProjection &&
                        it.canonicalProjectionRevision == null &&
                        it.canonicalProjectionResolution == null &&
                        (
                            it.canonicalProjectionConflict == null ||
                                it.canonicalProjectionRetryAuthorizedAt != null
                            )
            },
            onDismiss = { showApiConnection = false },
            onUpgradeWithBootstrap = viewModel::upgradeDeviceAuthentication,
            onConsumeEnrollmentCode = viewModel::consumeDeviceEnrollmentCode,
            onRetryPending = viewModel::retryDeviceAuthentication,
            onRevokeAndSignOut = viewModel::signOutDeviceSession,
            onDestroyLocalOnly = {
                viewModel.destroyLocalDeviceAuthentication(confirmed = true)
            },
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
}

private const val EXECUTION_REFRESH_INTERVAL_MILLIS = 30_000L

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
