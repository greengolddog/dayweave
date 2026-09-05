package com.greengolddog.dayweave.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.DarkMode
import androidx.compose.material.icons.outlined.Devices
import androidx.compose.material.icons.outlined.HealthAndSafety
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.PrivacyTip
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.Shield
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.Card
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.greengolddog.dayweave.health.EnergySignalPhase
import com.greengolddog.dayweave.health.EnergySignalState
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.network.ConfigureGoogleCollectionRequest
import com.greengolddog.dayweave.network.AccountRecoveryDisclosure
import com.greengolddog.dayweave.network.AccountRecoveryIssuanceConfirmation
import com.greengolddog.dayweave.network.AccountRecoveryJournalDiscardConfirmation
import com.greengolddog.dayweave.network.AccountRecoveryPhase
import com.greengolddog.dayweave.network.AccountRecoveryState
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.security.AppLockTimeout
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.DeviceSessionRevocationConfirmation
import com.greengolddog.dayweave.sync.DeviceSessionSummary
import com.greengolddog.dayweave.sync.DeviceSessionsPhase
import com.greengolddog.dayweave.sync.DeviceSessionsState
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleAuthorizationAction
import com.greengolddog.dayweave.sync.GoogleAuthorizationRecoveryDiscardConfirmation
import com.greengolddog.dayweave.sync.GoogleAuthorizationRecoveryResetConfirmation
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateState
import com.greengolddog.dayweave.ui.components.AppLockSettingsCard
import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

@Composable
fun MoreScreen(
    state: DayWeaveUiState,
    onToggleCompleted: () -> Unit,
    onToggleQuietSuggestions: () -> Unit,
    onToggleDynamicColor: () -> Unit,
    scheduleCompositionProfileUpdateState: ScheduleCompositionProfileUpdateState,
    onUpdateScheduleCompositionProfile: (ScheduleCompositionProfileSnapshot) -> Unit,
    onAcknowledgeScheduleCompositionProfileUpdate: () -> Unit,
    suggestionSyncState: SuggestionSyncState,
    canonicalSyncState: CanonicalSyncState,
    deviceSessionsState: DeviceSessionsState,
    accountRecoveryState: AccountRecoveryState,
    accountRecoveryStartBlocked: Boolean,
    onRefreshDeviceSessions: () -> Unit,
    deviceSessionRevocationConfirmationProvider:
        (String) -> DeviceSessionRevocationConfirmation?,
    onRevokeRemoteDeviceSession: (DeviceSessionRevocationConfirmation) -> Unit,
    onSignOutCurrentDeviceSession: (String) -> Unit,
    onRefreshAccountRecovery: () -> Unit,
    accountRecoveryIssuanceConfirmationProvider:
        () -> AccountRecoveryIssuanceConfirmation?,
    onIssueOrRotateAccountRecoveryCode: (AccountRecoveryIssuanceConfirmation) -> Unit,
    onRetryAccountRecovery: () -> Unit,
    accountRecoveryDisclosureProvider: () -> AccountRecoveryDisclosure?,
    onAcknowledgeAccountRecoveryDisclosure: (AccountRecoveryDisclosure) -> Unit,
    accountRecoveryJournalDiscardConfirmationProvider:
        () -> AccountRecoveryJournalDiscardConfirmation?,
    onDiscardAccountRecoveryJournal: (AccountRecoveryJournalDiscardConfirmation) -> Unit,
    googleAccountState: GoogleAccountState,
    googleCalendarImportState: GoogleCalendarImportState,
    energySignalState: EnergySignalState,
    appLockState: AppLockState,
    onConfigureApiConnection: () -> Unit,
    onConnectGoogle: () -> Unit,
    onRefreshGoogle: () -> Unit,
    onRestartGoogleAuthorization: () -> Unit,
    onOpenGoogleAuthorization: (String) -> Unit,
    onReauthorizeGoogle: (String) -> Unit,
    onEnableGoogleCalendarPublishing: (String) -> Unit,
    onEnableGoogleTasksPublishing: (String) -> Unit,
    authorizationRecoveryResetConfirmationProvider:
        () -> GoogleAuthorizationRecoveryResetConfirmation?,
    onResetGoogleAuthorizationRecovery: (GoogleAuthorizationRecoveryResetConfirmation) -> Unit,
    authorizationRecoveryDiscardConfirmationProvider:
        () -> GoogleAuthorizationRecoveryDiscardConfirmation?,
    onDiscardGoogleAuthorizationRecovery:
        (GoogleAuthorizationRecoveryDiscardConfirmation) -> Unit,
    onSetGooglePaused: (String, Boolean) -> Unit,
    onRequestGoogleDisconnect: (GoogleAccountSummary) -> Unit,
    onDiscoverGoogleSources: (String) -> Unit,
    onRefreshGoogleImport: (String) -> Unit,
    onConfigureGoogleSource: (
        String,
        String,
        ConfigureGoogleCollectionRequest,
    ) -> Unit,
    onPublishGeneratedSchedule: () -> Unit = {},
    schedulePublicationHasRecovery: Boolean = false,
    onToggleHealthConnect: (Boolean) -> Unit,
    onRefreshHealthConnect: () -> Unit,
    onManageHealthConnectAccess: () -> Unit,
    onInstallHealthConnect: () -> Unit,
    onSetAppLockEnabled: (Boolean) -> Unit,
    onSetAppLockTimeout: (AppLockTimeout) -> Unit,
    onLockNow: () -> Unit,
    onOpenDeviceSecuritySettings: () -> Unit,
    canonicalPrivacyActionsEnabled: Boolean,
    onSetCanonicalItemSensitive: (String, Long, Boolean) -> Unit,
    habitStatisticsContent: (@Composable () -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    var pendingSensitivityRemoval by remember {
        mutableStateOf<CanonicalItemSnapshot?>(null)
    }
    var showPlanningProfileEditor by rememberSaveable { mutableStateOf(false) }
    var pendingGoogleAuthorizationRecoveryReset by remember {
        mutableStateOf<GoogleAuthorizationRecoveryResetConfirmation?>(null)
    }
    var pendingGoogleAuthorizationRecoveryDiscard by remember {
        mutableStateOf<GoogleAuthorizationRecoveryDiscardConfirmation?>(null)
    }
    val profileEditBlockedMessage = planningProfileEditBlockedMessage(
        state = state,
        canonicalActionBusy = canonicalSyncState.isBusy,
    )
    LaunchedEffect(scheduleCompositionProfileUpdateState.phase) {
        if (
            scheduleCompositionProfileUpdateState.phase ==
            ScheduleCompositionProfileUpdatePhase.SAVED
        ) {
            showPlanningProfileEditor = false
            onAcknowledgeScheduleCompositionProfileUpdate()
        }
    }
    LaunchedEffect(googleAccountState.authorizationRecoveryDiscardRequired) {
        if (!googleAccountState.authorizationRecoveryDiscardRequired) {
            pendingGoogleAuthorizationRecoveryDiscard = null
        }
    }
    LaunchedEffect(googleAccountState.authorizationRecoveryResetRequired) {
        if (!googleAccountState.authorizationRecoveryResetRequired) {
            pendingGoogleAuthorizationRecoveryReset = null
        }
    }
    pendingGoogleAuthorizationRecoveryReset?.let { confirmation ->
        AlertDialog(
            onDismissRequest = { pendingGoogleAuthorizationRecoveryReset = null },
            title = { Text("Discard saved Google authorization?") },
            text = {
                Text(
                    "The saved authorization record is unreadable. Discarding it cannot " +
                        "revoke a request that Google may already have accepted. Check the " +
                        "Google connection afterward before starting another request.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingGoogleAuthorizationRecoveryReset = null
                        onResetGoogleAuthorizationRecovery(confirmation)
                    },
                ) {
                    Text("Discard local record")
                }
            },
            dismissButton = {
                TextButton(
                    onClick = { pendingGoogleAuthorizationRecoveryReset = null },
                ) {
                    Text("Keep it")
                }
            },
        )
    }
    pendingGoogleAuthorizationRecoveryDiscard?.let { confirmation ->
        AlertDialog(
            onDismissRequest = { pendingGoogleAuthorizationRecoveryDiscard = null },
            title = { Text("Discard this saved Google authorization?") },
            text = {
                Text(
                    "Google may already have accepted this exact authorization. Verify the " +
                        "Google account and Planner API first. Discarding removes only " +
                        "DayWeave’s local recovery record and cannot revoke a Google grant.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingGoogleAuthorizationRecoveryDiscard = null
                        onDiscardGoogleAuthorizationRecovery(confirmation)
                    },
                    modifier = Modifier.testTag("google_confirm_authorization_discard"),
                ) {
                    Text("Discard local record")
                }
            },
            dismissButton = {
                TextButton(
                    onClick = { pendingGoogleAuthorizationRecoveryDiscard = null },
                ) {
                    Text("Keep it")
                }
            },
        )
    }
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item {
            Card {
                ListItem(
                    headlineContent = { Text("Personal workspace") },
                    supportingContent = {
                        Column {
                            Text(
                                when (canonicalSyncState.phase) {
                                    CanonicalSyncPhase.CONNECTED ->
                                        "Canonical items and schedule connected"
                                    CanonicalSyncPhase.SYNCING ->
                                        "Syncing items and composing the firm horizon"
                                    CanonicalSyncPhase.AUTH_REQUIRED ->
                                        "Planner API authentication required"
                                    CanonicalSyncPhase.NOT_CONFIGURED ->
                                        "Planner API not configured"
                                    CanonicalSyncPhase.OFFLINE ->
                                        "Offline · encrypted cached plan available"
                                    CanonicalSyncPhase.ERROR ->
                                        "Canonical planner sync error"
                                    CanonicalSyncPhase.READY ->
                                        "Canonical planner sync ready"
                                },
                            )
                            Text(
                                when (suggestionSyncState.phase) {
                                SuggestionSyncPhase.CONNECTED ->
                                    "Suggestions connected"
                                SuggestionSyncPhase.SYNCING ->
                                    "Refreshing suggestions"
                                SuggestionSyncPhase.AUTH_REQUIRED ->
                                    "Suggestion authentication required"
                                SuggestionSyncPhase.NOT_CONFIGURED ->
                                    "Suggestion API not configured"
                                SuggestionSyncPhase.OFFLINE ->
                                    "Cached suggestions available"
                                SuggestionSyncPhase.ERROR ->
                                    "Suggestion sync error"
                                SuggestionSyncPhase.READY ->
                                    "Suggestion sync ready"
                                },
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    },
                    leadingContent = { Icon(Icons.Outlined.AccountCircle, contentDescription = null) },
                    trailingContent = {
                        Icon(
                            if (canonicalSyncState.phase == CanonicalSyncPhase.CONNECTED) {
                                Icons.Outlined.CloudDone
                            } else {
                                Icons.Outlined.CloudOff
                            },
                            contentDescription = canonicalSyncState.message,
                            tint = if (canonicalSyncState.phase == CanonicalSyncPhase.CONNECTED) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    },
                )
            }
        }
        item {
            ActiveDevicesCard(
                state = deviceSessionsState,
                onRefresh = onRefreshDeviceSessions,
                revocationConfirmationProvider =
                    deviceSessionRevocationConfirmationProvider,
                onRevokeRemote = onRevokeRemoteDeviceSession,
                onSignOutCurrent = onSignOutCurrentDeviceSession,
                onConfigureApiConnection = onConfigureApiConnection,
            )
        }
        item {
            AccountRecoveryCard(
                state = accountRecoveryState,
                recoveryStartBlocked = accountRecoveryStartBlocked,
                onRefresh = onRefreshAccountRecovery,
                issuanceConfirmationProvider =
                    accountRecoveryIssuanceConfirmationProvider,
                onIssueOrRotate = onIssueOrRotateAccountRecoveryCode,
                onRetry = onRetryAccountRecovery,
                disclosureProvider = accountRecoveryDisclosureProvider,
                onAcknowledgeDisclosure = onAcknowledgeAccountRecoveryDisclosure,
                journalDiscardConfirmationProvider =
                    accountRecoveryJournalDiscardConfirmationProvider,
                onDiscardJournal = onDiscardAccountRecoveryJournal,
                onRecoverAccount = onConfigureApiConnection,
            )
        }
        habitStatisticsContent?.let { content ->
            item {
                content()
            }
        }

        item { SettingsSectionTitle("Planning") }
        item {
            PlanningProfileCard(
                profile = state.scheduleCompositionProfile,
                editBlockedMessage = profileEditBlockedMessage,
                updateState = scheduleCompositionProfileUpdateState,
                onEdit = {
                    onAcknowledgeScheduleCompositionProfileUpdate()
                    showPlanningProfileEditor = true
                },
            )
        }
        item {
            Card {
                SettingsInfo(
                    Icons.Outlined.CalendarMonth,
                    "Calendars & tasks",
                    "Google Calendar · Google Tasks",
                )
                HorizontalDivider()
                SettingsToggle(
                    Icons.Outlined.Notifications,
                    "Quiet proactive suggestions",
                    "Respect limits and quiet hours",
                    state.quietSuggestions,
                    onToggleQuietSuggestions,
                )
                HorizontalDivider()
                SettingsToggle(
                    Icons.Outlined.HealthAndSafety,
                    "Show completed items",
                    "Keep finished blocks visible in Today",
                    state.showCompleted,
                    onToggleCompleted,
                )
            }
        }

        item {
            GoogleConnectionCard(
                state = googleAccountState,
                onConfigureApiConnection = onConfigureApiConnection,
                onConnect = onConnectGoogle,
                onRefresh = onRefreshGoogle,
                onRestartAuthorization = onRestartGoogleAuthorization,
                onOpenAuthorization = onOpenGoogleAuthorization,
                onReauthorize = onReauthorizeGoogle,
                onEnableCalendarPublishing = onEnableGoogleCalendarPublishing,
                onEnableTasksPublishing = onEnableGoogleTasksPublishing,
                onRequestAuthorizationRecoveryReset = {
                    pendingGoogleAuthorizationRecoveryReset =
                        authorizationRecoveryResetConfirmationProvider()
                },
                onRequestAuthorizationRecoveryDiscard = {
                    pendingGoogleAuthorizationRecoveryDiscard =
                        authorizationRecoveryDiscardConfirmationProvider()
                },
                onSetPaused = onSetGooglePaused,
                onRequestDisconnect = onRequestGoogleDisconnect,
                calendarImportBusy = googleCalendarImportState.isBusy,
                calendarImportHasRecovery =
                    googleCalendarImportState.pendingRecoveryCount > 0,
                calendarImportReauthorizationAccountIds = googleCalendarImportState.accounts
                    .filterValues { account ->
                        account.run?.state == RemoteGoogleSyncRunState.REAUTHORIZATION_REQUIRED
                    }
                    .keys,
            )
        }

        item {
            GoogleSourcesCard(
                googleAccountState = googleAccountState,
                importState = googleCalendarImportState,
                onDiscover = onDiscoverGoogleSources,
                onRefreshOrCheck = onRefreshGoogleImport,
                onConfigure = onConfigureGoogleSource,
                onPublishGeneratedSchedule = onPublishGeneratedSchedule,
                schedulePublicationHasRecovery = schedulePublicationHasRecovery,
                schedulePublicationHasCurrentSchedule =
                    state.pendingSchedulePublication == null &&
                        state.publishedScheduleRevision != null &&
                        state.publishedScheduleProof?.matchesCurrentStateAndPlan(state) == true,
                actionsEnabled = !canonicalSyncState.isBusy,
                configurationActionsEnabled =
                    state.pendingGoogleCalendarOutbound == null &&
                    state.pendingGoogleSchedulePublication == null,
            )
        }

        item { SettingsSectionTitle("Health & context") }
        item {
            HealthConnectCard(
                enabled = state.healthConnectSyncEnabled,
                state = energySignalState,
                onToggle = onToggleHealthConnect,
                onRefresh = onRefreshHealthConnect,
                onManageAccess = onManageHealthConnectAccess,
                onInstallOrUpdate = onInstallHealthConnect,
            )
        }

        item { SettingsSectionTitle("Appearance & privacy") }
        item {
            Card {
                SettingsToggle(
                    Icons.Outlined.DarkMode,
                    "Use system accent",
                    "Apply Android dynamic color",
                    state.useDynamicColor,
                    onToggleDynamicColor,
                )
            }
        }

        item {
            AppLockSettingsCard(
                state = appLockState,
                onSetEnabled = onSetAppLockEnabled,
                onSetTimeout = onSetAppLockTimeout,
                onLockNow = onLockNow,
                onOpenDeviceSecuritySettings = onOpenDeviceSecuritySettings,
            )
        }

        item {
            SensitiveItemsCard(
                items = state.canonicalItems,
                pendingMutation = state.pendingCanonicalMutation,
                actionsEnabled = canonicalPrivacyActionsEnabled,
                onSetSensitive = { item, sensitive ->
                    if (sensitive) {
                        onSetCanonicalItemSensitive(item.id, item.revision, true)
                    } else {
                        pendingSensitivityRemoval = item
                    }
                },
            )
        }

        item {
            Text(
                "DayWeave 0.1.0 · Encrypted local plan",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 10.dp),
            )
        }
    }

    pendingSensitivityRemoval?.let { item ->
        val inherited = item.parentId?.let { parentId ->
            effectiveCanonicalSensitivity(
                state.canonicalItems,
                parentId,
                state.pendingCanonicalMutation,
            )
        } == true
        AlertDialog(
            onDismissRequest = { pendingSensitivityRemoval = null },
            icon = { Icon(Icons.Outlined.PrivacyTip, contentDescription = null) },
            title = {
                Text(if (inherited) "Remove this item’s own label?" else "Make item standard?")
            },
            text = {
                Text(
                    if (inherited) {
                        "This item will remain sensitive through its parent. Only its own label " +
                            "will be removed."
                    } else {
                        "This reduces privacy protection for this item and descendants that do " +
                            "not have another sensitive ancestor. The change is synced only " +
                            "after server confirmation."
                    },
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingSensitivityRemoval = null
                        onSetCanonicalItemSensitive(item.id, item.revision, false)
                    },
                ) {
                    Text(if (inherited) "Remove own label" else "Make standard")
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingSensitivityRemoval = null }) { Text("Cancel") }
            },
        )
    }

    if (showPlanningProfileEditor) {
        PlanningProfileEditorDialog(
            currentProfile = state.scheduleCompositionProfile,
            editBlockedMessage = profileEditBlockedMessage,
            updateState = scheduleCompositionProfileUpdateState,
            onSave = onUpdateScheduleCompositionProfile,
            onDismiss = {
                if (!scheduleCompositionProfileUpdateState.isSaving) {
                    showPlanningProfileEditor = false
                    onAcknowledgeScheduleCompositionProfileUpdate()
                }
            },
        )
    }
}

@Composable
internal fun ActiveDevicesCard(
    state: DeviceSessionsState,
    onRefresh: () -> Unit,
    revocationConfirmationProvider: (String) -> DeviceSessionRevocationConfirmation?,
    onRevokeRemote: (DeviceSessionRevocationConfirmation) -> Unit,
    onSignOutCurrent: (String) -> Unit,
    onConfigureApiConnection: () -> Unit,
    referenceTime: Instant = Instant.now(),
) {
    var pendingRemote by remember {
        mutableStateOf<DeviceSessionRevocationConfirmation?>(null)
    }
    var pendingCurrentSessionId by remember { mutableStateOf<String?>(null) }
    val lifecycleOwner = LocalLifecycleOwner.current

    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) {
                pendingRemote = null
                pendingCurrentSessionId = null
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    LaunchedEffect(state) {
        pendingRemote = pendingRemote?.takeIf { confirmation ->
            state.canRevokeRemoteSessions &&
                state.sessions.singleOrNull { it.id == confirmation.sessionId }?.let { session ->
                    !session.isCurrent && session.revision == confirmation.sessionRevision
                } == true
        }
        pendingCurrentSessionId = pendingCurrentSessionId?.takeIf { currentSessionId ->
            state.canRevokeRemoteSessions &&
                state.sessions.singleOrNull { it.id == currentSessionId }?.isCurrent == true
        }
    }

    pendingRemote?.let { confirmation ->
        val remote = state.sessions.singleOrNull {
            it.id == confirmation.sessionId && !it.isCurrent &&
                it.revision == confirmation.sessionRevision
        }
        if (remote != null) {
            AlertDialog(
                onDismissRequest = { pendingRemote = null },
                icon = { Icon(Icons.Outlined.Devices, contentDescription = null) },
                title = { Text("Revoke ${remote.deviceLabel}?") },
                text = {
                    Text(
                        "That device will lose DayWeave access and must be enrolled again. " +
                            "DayWeave will verify the server’s active-device list before " +
                            "showing the revocation as complete.",
                    )
                },
                confirmButton = {
                    TextButton(
                        onClick = {
                            pendingRemote = null
                            onRevokeRemote(confirmation)
                        },
                        enabled = state.canRevokeRemoteSessions,
                        colors = ButtonDefaults.textButtonColors(
                            contentColor = MaterialTheme.colorScheme.error,
                        ),
                        modifier = Modifier.testTag("confirm_remote_device_revocation"),
                    ) { Text("Revoke device") }
                },
                dismissButton = {
                    TextButton(onClick = { pendingRemote = null }) { Text("Keep device") }
                },
            )
        }
    }

    pendingCurrentSessionId?.let { currentSessionId ->
        AlertDialog(
            onDismissRequest = { pendingCurrentSessionId = null },
            icon = { Icon(Icons.Outlined.PhoneAndroid, contentDescription = null) },
            title = { Text("Revoke this device session?") },
            text = {
                Text(
                    "DayWeave will ask the server to revoke this exact session. Local " +
                        "credentials and API-bound data are cleared only after revocation is " +
                        "confirmed; a failure keeps them available for retry.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingCurrentSessionId = null
                        onSignOutCurrent(currentSessionId)
                    },
                    enabled = state.canRevokeRemoteSessions,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag("confirm_current_device_sign_out"),
                ) { Text("Revoke & sign out") }
            },
            dismissButton = {
                TextButton(onClick = { pendingCurrentSessionId = null }) {
                    Text("Keep session")
                }
            },
        )
    }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("active_devices_card")
            .semantics { stateDescription = state.message },
    ) {
        ListItem(
            headlineContent = { Text("Active devices") },
            supportingContent = { Text(state.message) },
            leadingContent = { Icon(Icons.Outlined.Devices, contentDescription = null) },
            trailingContent = {
                if (state.isBusy) {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp))
                } else {
                    IconButton(
                        onClick = onRefresh,
                        enabled = state.phase !in setOf(
                            DeviceSessionsPhase.NOT_CONFIGURED,
                            DeviceSessionsPhase.AUTH_REQUIRED,
                        ),
                        modifier = Modifier.testTag("refresh_active_devices"),
                    ) {
                        Icon(Icons.Outlined.Sync, contentDescription = "Refresh active devices")
                    }
                }
            },
        )

        state.sessions.forEach { session ->
            HorizontalDivider()
            ListItem(
                modifier = Modifier.testTag("active_device_${session.id}"),
                headlineContent = {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            session.deviceLabel,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        if (session.isCurrent) {
                            Surface(
                                shape = MaterialTheme.shapes.small,
                                color = MaterialTheme.colorScheme.primaryContainer,
                            ) {
                                Text(
                                    "This device",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                                    modifier = Modifier.padding(horizontal = 7.dp, vertical = 3.dp),
                                )
                            }
                        }
                    }
                },
                supportingContent = {
                    Text(deviceSessionSupportingText(session, referenceTime))
                },
                leadingContent = {
                    Icon(
                        if (session.clientKind == "android") {
                            Icons.Outlined.PhoneAndroid
                        } else {
                            Icons.Outlined.Devices
                        },
                        contentDescription = null,
                    )
                },
                trailingContent = {
                    TextButton(
                        onClick = {
                            if (session.isCurrent) {
                                pendingCurrentSessionId = session.id
                            } else {
                                pendingRemote =
                                    revocationConfirmationProvider(session.id)
                            }
                        },
                        enabled = state.canRevokeRemoteSessions,
                        colors = ButtonDefaults.textButtonColors(
                            contentColor = MaterialTheme.colorScheme.error,
                        ),
                        modifier = Modifier.testTag(
                            if (session.isCurrent) {
                                "sign_out_current_device"
                            } else {
                                "revoke_device_${session.id}"
                            },
                        ),
                    ) {
                        Text(if (session.isCurrent) "Sign out" else "Revoke")
                    }
                },
            )
        }

        if (
            state.phase in setOf(
                DeviceSessionsPhase.NOT_CONFIGURED,
                DeviceSessionsPhase.AUTH_REQUIRED,
            )
        ) {
            TextButton(
                onClick = onConfigureApiConnection,
                enabled = !state.isBusy,
                modifier = Modifier.padding(horizontal = 8.dp),
            ) { Text("Set up device access") }
        }
        Text(
            deviceSessionInventoryPrivacyMessage(state),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
        )
    }
}

@Composable
internal fun AccountRecoveryCard(
    state: AccountRecoveryState,
    recoveryStartBlocked: Boolean,
    onRefresh: () -> Unit,
    issuanceConfirmationProvider: () -> AccountRecoveryIssuanceConfirmation?,
    onIssueOrRotate: (AccountRecoveryIssuanceConfirmation) -> Unit,
    onRetry: () -> Unit,
    disclosureProvider: () -> AccountRecoveryDisclosure?,
    onAcknowledgeDisclosure: (AccountRecoveryDisclosure) -> Unit,
    journalDiscardConfirmationProvider: () -> AccountRecoveryJournalDiscardConfirmation?,
    onDiscardJournal: (AccountRecoveryJournalDiscardConfirmation) -> Unit,
    onRecoverAccount: () -> Unit,
) {
    var pendingIssuance by remember {
        mutableStateOf<AccountRecoveryIssuanceConfirmation?>(null)
    }
    var disclosure by remember { mutableStateOf<AccountRecoveryDisclosure?>(null) }
    var pendingDiscard by remember {
        mutableStateOf<AccountRecoveryJournalDiscardConfirmation?>(null)
    }
    val lifecycleOwner = LocalLifecycleOwner.current
    val context = LocalContext.current
    val coroutineScope = rememberCoroutineScope()

    DisposableEffect(lifecycleOwner, disclosure) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) {
                disclosure?.code?.let { clearRecoveryCodeClipboard(context, it) }
                disclosure = null
                pendingIssuance = null
                pendingDiscard = null
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            disclosure?.code?.let { clearRecoveryCodeClipboard(context, it) }
        }
    }

    LaunchedEffect(state) {
        if (!state.canIssueOrRotate || state.isBusy) pendingIssuance = null
        if (!state.disclosureReady) disclosure = null
        if (!state.discardAvailable) pendingDiscard = null
    }

    pendingDiscard?.let { confirmation ->
        AlertDialog(
            onDismissRequest = { pendingDiscard = null },
            icon = { Icon(Icons.Outlined.Shield, contentDescription = null) },
            title = {
                Text(
                    if (confirmation.repairsUnreadableState) {
                        "Remove unreadable recovery state?"
                    } else {
                        "Discard saved recovery request?"
                    },
                )
            },
            text = {
                Text(
                    if (confirmation.repairsUnreadableState) {
                        "Only the unreadable account-recovery journal will be removed. Your " +
                            "device session is preserved, but a request that reached the server " +
                            "may still have changed the active recovery code."
                    } else {
                        "The exact encrypted retry tuple will be destroyed. The request may " +
                            "already have reached the server, so verify account recovery state " +
                            "before creating or using another code."
                    },
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingDiscard = null
                        onDiscardJournal(confirmation)
                    },
                    enabled = state.discardAvailable && !state.isBusy,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag("confirm_discard_account_recovery_journal"),
                ) { Text("Discard recovery state") }
            },
            dismissButton = {
                TextButton(onClick = { pendingDiscard = null }) { Text("Keep for retry") }
            },
            properties = DialogProperties(securePolicy = SecureFlagPolicy.SecureOn),
        )
    }

    pendingIssuance?.let { confirmation ->
        val rotating = state.currentCodeId != null
        AlertDialog(
            onDismissRequest = { pendingIssuance = null },
            icon = { Icon(Icons.Outlined.Shield, contentDescription = null) },
            title = { Text(if (rotating) "Rotate recovery code?" else "Create recovery code?") },
            text = {
                Text(
                    if (rotating) {
                        "The current recovery code will stop working immediately. The new code " +
                            "is shown once and remains encrypted on this device until you confirm " +
                            "that it is saved."
                    } else {
                        "The new code can replace every device session and connected client. " +
                            "Store it somewhere private; DayWeave shows it only until you confirm " +
                            "that it is saved."
                    },
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingIssuance = null
                        onIssueOrRotate(confirmation)
                    },
                    enabled = state.canIssueOrRotate && !state.isBusy,
                    modifier = Modifier.testTag("confirm_account_recovery_issue"),
                ) { Text(if (rotating) "Rotate code" else "Create code") }
            },
            dismissButton = {
                TextButton(onClick = { pendingIssuance = null }) { Text("Cancel") }
            },
            properties = DialogProperties(securePolicy = SecureFlagPolicy.SecureOn),
        )
    }

    disclosure?.let { revealed ->
        AlertDialog(
            onDismissRequest = {
                clearRecoveryCodeClipboard(context, revealed.code)
                disclosure = null
            },
            icon = { Icon(Icons.Outlined.Shield, contentDescription = null) },
            title = {
                Text(
                    if (revealed.source == "successor") {
                        "Save your successor code"
                    } else {
                        "Save your recovery code"
                    },
                )
            },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(
                        "Anyone with this code can replace every active DayWeave connection. " +
                            "Keep it separate from this device.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        shape = MaterialTheme.shapes.medium,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            revealed.code,
                            modifier = Modifier
                                .padding(12.dp)
                                .testTag("account_recovery_code_value"),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    TextButton(
                        onClick = {
                            copyRecoveryCodeToClipboard(context, revealed.code)
                            coroutineScope.launch {
                                delay(RECOVERY_CLIPBOARD_TTL_MILLIS)
                                clearRecoveryCodeClipboard(context, revealed.code)
                            }
                        },
                        modifier = Modifier.testTag("copy_account_recovery_code"),
                    ) { Text("Copy code") }
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        clearRecoveryCodeClipboard(context, revealed.code)
                        disclosure = null
                        onAcknowledgeDisclosure(revealed)
                    },
                    modifier = Modifier.testTag("acknowledge_account_recovery_code"),
                ) { Text("I saved it") }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        clearRecoveryCodeClipboard(context, revealed.code)
                        disclosure = null
                    },
                ) { Text("Hide for now") }
            },
            properties = DialogProperties(securePolicy = SecureFlagPolicy.SecureOn),
        )
    }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("account_recovery_card")
            .semantics { stateDescription = state.message },
    ) {
        ListItem(
            headlineContent = { Text("Account recovery") },
            supportingContent = { Text(state.message) },
            leadingContent = { Icon(Icons.Outlined.Shield, contentDescription = null) },
            trailingContent = {
                if (state.isBusy) {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp))
                } else {
                    IconButton(
                        onClick = onRefresh,
                        enabled = state.phase != AccountRecoveryPhase.LOCKED,
                        modifier = Modifier.testTag("refresh_account_recovery"),
                    ) {
                        Icon(Icons.Outlined.Sync, contentDescription = "Refresh account recovery")
                    }
                }
            },
        )
        state.currentCodeCreatedAt?.let { createdAt ->
            Text(
                "Active code created $createdAt",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        Row(
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            if (state.canIssueOrRotate) {
                TextButton(
                    onClick = { pendingIssuance = issuanceConfirmationProvider() },
                    enabled = !state.isBusy,
                    modifier = Modifier.testTag("issue_or_rotate_account_recovery"),
                ) {
                    Text(if (state.currentCodeId == null) "Create code" else "Rotate code")
                }
            }
            if (state.disclosureReady) {
                TextButton(
                    onClick = { disclosure = disclosureProvider() },
                    enabled = !state.isBusy,
                    modifier = Modifier.testTag("reveal_account_recovery_code"),
                ) { Text("Reveal & save") }
            }
            if (state.retryAvailable) {
                TextButton(
                    onClick = onRetry,
                    enabled = !state.isBusy,
                    modifier = Modifier.testTag("retry_account_recovery"),
                ) { Text("Retry exact request") }
            }
            if (state.discardAvailable) {
                TextButton(
                    onClick = {
                        pendingDiscard = journalDiscardConfirmationProvider()
                    },
                    enabled = !state.isBusy,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag("discard_account_recovery_journal"),
                ) {
                    Text(if (state.repairRequired) "Repair state" else "Discard request")
                }
            }
            if (!state.retryAvailable && !state.disclosureReady) {
                TextButton(
                    onClick = onRecoverAccount,
                    enabled = !state.isBusy && !recoveryStartBlocked,
                    modifier = Modifier.testTag("open_account_recovery"),
                ) { Text("Use recovery code") }
            }
        }
        if (recoveryStartBlocked) {
            Text(
                "Finish the saved Planner or Google operation before using a recovery code.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        Text(
            "Recovery metadata is fetched directly and kept only in memory while unlocked. " +
                "A pending request or unacknowledged code stays encrypted in Android Keystore storage.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
        )
    }
}

private fun copyRecoveryCodeToClipboard(context: Context, code: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val clip = ClipData.newPlainText("DayWeave recovery code", code)
    clip.description.extras = PersistableBundle().apply {
        putBoolean("android.content.extra.IS_SENSITIVE", true)
    }
    clipboard.setPrimaryClip(clip)
}

private fun clearRecoveryCodeClipboard(context: Context, expectedCode: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val current = runCatching {
        clipboard.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString()
    }.getOrNull()
    if (current != expectedCode) return
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
        clipboard.clearPrimaryClip()
    } else {
        clipboard.setPrimaryClip(ClipData.newPlainText("", ""))
    }
}

private const val RECOVERY_CLIPBOARD_TTL_MILLIS = 60_000L

internal fun deviceSessionInventoryPrivacyMessage(state: DeviceSessionsState): String =
    if (state.phase == DeviceSessionsPhase.READY && !state.currentSessionCanRevoke) {
        "Read-only access: this device can view active sessions, but it cannot revoke or sign " +
            "out sessions. The list is fetched directly and kept only in memory while unlocked."
    } else {
        "Fetched directly from your Planner API and kept only in memory while DayWeave " +
            "is unlocked. Revocation actions are unavailable when the list is stale or offline."
    }

internal fun deviceSessionSupportingText(
    session: DeviceSessionSummary,
    referenceTime: Instant,
): String {
    val platform = if (session.clientKind == "android") "Android" else "macOS"
    val elapsedSeconds = Duration.between(session.lastSeenAt, referenceTime)
        .seconds
        .coerceAtLeast(0)
    val activity = when {
        elapsedSeconds < 60 -> "just now"
        elapsedSeconds < 3_600 -> {
            val minutes = elapsedSeconds / 60
            "$minutes ${if (minutes == 1L) "minute" else "minutes"} ago"
        }
        elapsedSeconds < 86_400 -> {
            val hours = elapsedSeconds / 3_600
            "$hours ${if (hours == 1L) "hour" else "hours"} ago"
        }
        else -> {
            val days = elapsedSeconds / 86_400
            "$days ${if (days == 1L) "day" else "days"} ago"
        }
    }
    return "$platform · DayWeave ${session.clientVersion} · Active $activity"
}

@Composable
private fun SensitiveItemsCard(
    items: List<CanonicalItemSnapshot>,
    pendingMutation: PendingCanonicalMutation?,
    actionsEnabled: Boolean,
    onSetSensitive: (CanonicalItemSnapshot, Boolean) -> Unit,
) {
    val activeItems = items.filter { it.deletedAt == null }
    val effectiveCount = activeItems.count { item ->
        effectiveCanonicalSensitivity(activeItems, item.id, pendingMutation)
    }
    Card(modifier = Modifier.fillMaxWidth().testTag("sensitive_items_card")) {
        ListItem(
            headlineContent = { Text("Sensitive items") },
            supportingContent = {
                Text(
                    if (activeItems.isEmpty()) {
                        "Connect and sync canonical items to manage their privacy."
                    } else {
                        "$effectiveCount of ${activeItems.size} protected. Children inherit every " +
                            "sensitive parent."
                    },
                )
            },
            leadingContent = { Icon(Icons.Outlined.PrivacyTip, contentDescription = null) },
        )
        if (activeItems.isNotEmpty()) {
            HorizontalDivider()
            canonicalHierarchy(activeItems).forEach { entry ->
                val effective = effectiveCanonicalSensitivity(
                    activeItems,
                    entry.item.id,
                    pendingMutation,
                )
                val inherited = !entry.item.isSensitive && effective
                val pendingPromotion = pendingMutation?.let { pending ->
                    pending.itemId == entry.item.id && pending.targetIsSensitive &&
                        !entry.item.isSensitive
                } == true
                val pendingRemoval = pendingMutation?.let { pending ->
                    pending.itemId == entry.item.id && !pending.targetIsSensitive &&
                        entry.item.isSensitive
                } == true
                ListItem(
                    modifier = Modifier.testTag("sensitive_item_${entry.item.id}"),
                    headlineContent = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (entry.depth > 0) {
                                Spacer(Modifier.width((entry.depth.coerceAtMost(6) * 12).dp))
                            }
                            Text(entry.item.title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        }
                    },
                    supportingContent = {
                        Text(
                            when {
                                pendingPromotion -> "Protection pending server confirmation"
                                pendingRemoval ->
                                    "Removal pending · protection remains until confirmation"
                                entry.item.isSensitive -> "Marked sensitive · children inherit"
                                inherited -> "Sensitive through parent"
                                else -> "Standard privacy"
                            },
                        )
                    },
                    leadingContent = {
                        Icon(
                            if (effective) Icons.Outlined.Shield else Icons.Outlined.PrivacyTip,
                            contentDescription = if (effective) "Sensitive" else "Standard privacy",
                            tint = if (effective) {
                                MaterialTheme.colorScheme.tertiary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                    },
                    trailingContent = {
                        Switch(
                            checked = entry.item.isSensitive || pendingPromotion,
                            onCheckedChange = { onSetSensitive(entry.item, it) },
                            enabled = actionsEnabled,
                            modifier = Modifier.testTag("sensitive_toggle_${entry.item.id}"),
                        )
                    },
                )
            }
        }
        Text(
            if (actionsEnabled) {
                "Turning protection off always asks for confirmation."
            } else {
                "Privacy changes are available when canonical sync is ready and no write is pending."
            },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
        )
    }
}

private data class CanonicalHierarchyEntry(
    val item: CanonicalItemSnapshot,
    val depth: Int,
)

/** Stable server hierarchy order without using private titles as a sorting key. */
private fun canonicalHierarchy(items: List<CanonicalItemSnapshot>): List<CanonicalHierarchyEntry> {
    val byId = items.associateBy(CanonicalItemSnapshot::id)
    val children = items.groupBy(CanonicalItemSnapshot::parentId)
    val visited = mutableSetOf<String>()
    val result = mutableListOf<CanonicalHierarchyEntry>()
    val order = compareBy<CanonicalItemSnapshot>({ it.siblingOrder }, { it.id })

    fun append(root: CanonicalItemSnapshot) {
        val remaining = ArrayDeque<Pair<CanonicalItemSnapshot, Int>>()
        remaining.addLast(root to 0)
        while (remaining.isNotEmpty()) {
            val (item, depth) = remaining.removeLast()
            if (!visited.add(item.id)) continue
            result += CanonicalHierarchyEntry(item, depth)
            children[item.id].orEmpty().sortedWith(order).asReversed().forEach { child ->
                remaining.addLast(child to depth + 1)
            }
        }
    }

    items.filter { it.parentId == null || it.parentId !in byId }
        .sortedWith(order)
        .forEach(::append)
    // Invalid/cyclic cache entries remain visible for repair while effective sensitivity fails shut.
    items.filterNot { it.id in visited }.sortedWith(order).forEach(::append)
    return result
}

@Composable
private fun HealthConnectCard(
    enabled: Boolean,
    state: EnergySignalState,
    onToggle: (Boolean) -> Unit,
    onRefresh: () -> Unit,
    onManageAccess: () -> Unit,
    onInstallOrUpdate: () -> Unit,
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("health_connect_settings_card")
            .semantics { stateDescription = state.message },
    ) {
        ListItem(
            headlineContent = { Text("Health Connect") },
            supportingContent = { Text(state.message) },
            leadingContent = {
                Icon(Icons.Outlined.HealthAndSafety, contentDescription = null)
            },
            trailingContent = {
                if (state.isBusy) {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp))
                } else {
                    Switch(
                        checked = enabled,
                        onCheckedChange = onToggle,
                        enabled = state.phase !in setOf(
                            EnergySignalPhase.UNAVAILABLE,
                            EnergySignalPhase.UPDATE_REQUIRED,
                        ),
                        modifier = Modifier.testTag("health_connect_sync_toggle"),
                    )
                }
            },
        )
        Text(
            "Opt-in foreground reads use only the recent sleep-duration aggregate. DayWeave " +
                "retains encrypted energy/recovery bands and a calculation time—not raw health " +
                "records—and never sends this signal to the server.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            when (state.phase) {
                EnergySignalPhase.UPDATE_REQUIRED -> TextButton(onClick = onInstallOrUpdate) {
                    Text("Install or update")
                }
                EnergySignalPhase.PERMISSION_REQUIRED,
                EnergySignalPhase.DENIED,
                -> TextButton(onClick = { onToggle(true) }) {
                    Text("Allow sleep access")
                }
                else -> if (enabled && !state.isBusy) {
                    TextButton(onClick = onRefresh) { Text("Refresh estimate") }
                }
            }
            if (state.phase !in setOf(EnergySignalPhase.UNAVAILABLE, EnergySignalPhase.UPDATE_REQUIRED)) {
                TextButton(onClick = onManageAccess) { Text("Manage access") }
            }
        }
        Text(
            "Planning remains available when Health Connect is off, unavailable, or denied. " +
                "The estimate is a planning aid, not medical guidance.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
        )
    }
}

@Composable
internal fun GoogleConnectionCard(
    state: GoogleAccountState,
    onConfigureApiConnection: () -> Unit,
    onConnect: () -> Unit,
    onRefresh: () -> Unit,
    onRestartAuthorization: () -> Unit,
    onOpenAuthorization: (String) -> Unit,
    onReauthorize: (String) -> Unit,
    onEnableCalendarPublishing: (String) -> Unit,
    onEnableTasksPublishing: (String) -> Unit,
    onRequestAuthorizationRecoveryReset: () -> Unit,
    onRequestAuthorizationRecoveryDiscard: () -> Unit,
    onSetPaused: (String, Boolean) -> Unit,
    onRequestDisconnect: (GoogleAccountSummary) -> Unit,
    calendarImportBusy: Boolean,
    calendarImportHasRecovery: Boolean,
    calendarImportReauthorizationAccountIds: Set<String> = emptySet(),
) {
    Card {
        ListItem(
            headlineContent = { Text("Google connection") },
            supportingContent = { Text(state.message) },
            leadingContent = {
                Icon(
                    if (state.phase == GoogleAccountPhase.CONNECTED) {
                        Icons.Outlined.CloudDone
                    } else {
                        Icons.Outlined.CloudOff
                    },
                    contentDescription = null,
                )
            },
            trailingContent = {
                if (state.isBusy) {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp))
                } else {
                    IconButton(onClick = onRefresh) {
                        Icon(Icons.Outlined.Sync, contentDescription = "Refresh Google status")
                    }
                }
            },
        )
        state.authorization?.let { authorization ->
            HorizontalDivider()
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Button(
                    onClick = { onOpenAuthorization(authorization.url) },
                    enabled = !state.isBusy,
                ) {
                    Text("Open Google")
                }
                TextButton(onClick = onRefresh, enabled = !state.isBusy) {
                    Text("I’ve finished")
                }
            }
            TextButton(
                onClick = onRestartAuthorization,
                enabled = !state.isBusy,
                modifier = Modifier.padding(horizontal = 8.dp),
            ) {
                Text("Retry saved request")
            }
        }
        state.authorizationRecovery?.takeIf { state.authorization == null }?.let { recovery ->
            HorizontalDivider()
            Column(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    "Saved ${googleAuthorizationActionLabel(recovery.action)} request",
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    if (recovery.browserWindowExpired) {
                        "The browser window has closed. DayWeave is retaining this exact " +
                            "record briefly while any in-flight Google callback settles."
                    } else if (recovery.browserOpened) {
                        "Google may already have accepted this request. Check the connection " +
                            "before retrying the exact saved request."
                    } else {
                        "The authorization URL is intentionally not stored. Resume the exact " +
                            "saved request to obtain a new browser handoff."
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(
                    onClick = if (
                        recovery.browserOpened || recovery.browserWindowExpired
                    ) {
                        onRefresh
                    } else {
                        onRestartAuthorization
                    },
                    enabled = !state.isBusy && recovery.belongsToCurrentConfiguration,
                    modifier = Modifier.testTag("google_resume_authorization"),
                ) {
                    Text(
                        if (recovery.browserOpened || recovery.browserWindowExpired) {
                            "Check Google status"
                        } else {
                            "Resume saved authorization"
                        },
                    )
                }
            }
        }
        if (state.authorizationRecoveryResetRequired) {
            HorizontalDivider()
            Column(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    "Saved Google authorization is unreadable",
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.error,
                )
                Text(
                    "DayWeave will not start another authorization until you explicitly " +
                        "discard the local recovery record.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(
                    onClick = onRequestAuthorizationRecoveryReset,
                    enabled = !state.isBusy,
                    modifier = Modifier.testTag("google_reset_authorization_recovery"),
                ) {
                    Text("Review discard")
                }
            }
        }
        if (
            state.authorizationRecoveryDiscardRequired &&
            !state.authorizationRecoveryResetRequired
        ) {
            HorizontalDivider()
            Column(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    "Saved Google authorization needs review",
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.error,
                )
                Text(
                    "It belongs to a different or unavailable Planner API connection. Google " +
                        "may already have accepted it, so verify the account before discarding " +
                        "the local recovery record.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(
                    onClick = onRequestAuthorizationRecoveryDiscard,
                    enabled = !state.isBusy,
                    modifier = Modifier.testTag("google_review_authorization_discard"),
                ) {
                    Text("Review discard")
                }
            }
        }
        state.accounts.forEachIndexed { accountIndex, account ->
            HorizontalDivider()
            Column(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    account.label + if (account.isDefault) " · default" else "",
                    style = MaterialTheme.typography.titleSmall,
                )
                val capabilities = buildList {
                    if (account.hasCalendar) add("Calendar import")
                    if (account.hasCalendarWriteScope) add("Calendar publish")
                    if (account.hasTasks) add("Tasks import")
                    if (account.hasTasksWriteScope) add("Tasks publish")
                }.joinToString(" · ").ifEmpty { "Authorization incomplete" }
                Text(
                    "$capabilities · ${account.status.replace('_', ' ')}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                val authorizationRecoveryBlocksMutation =
                    state.phase != GoogleAccountPhase.RECOVERY_REQUIRED &&
                    state.authorization == null && state.authorizationRecovery == null &&
                    !state.authorizationRecoveryResetRequired &&
                    !state.authorizationRecoveryDiscardRequired
                val canStartAuthorization = !state.isBusy &&
                    authorizationRecoveryBlocksMutation
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    if (
                        account.status == "reauthorization_required" ||
                        account.id in calendarImportReauthorizationAccountIds
                    ) {
                        TextButton(
                            onClick = { onReauthorize(account.id) },
                            enabled = canStartAuthorization,
                        ) {
                            Text("Reauthorize")
                        }
                    } else if (account.status in setOf("active", "paused")) {
                        TextButton(
                            onClick = { onSetPaused(account.id, account.status == "active") },
                            enabled = !state.isBusy && !calendarImportBusy &&
                                authorizationRecoveryBlocksMutation &&
                                (!calendarImportHasRecovery || account.status == "paused"),
                        ) {
                            Text(if (account.status == "active") "Pause sync" else "Resume sync")
                        }
                    }
                    TextButton(
                        onClick = { onRequestDisconnect(account) },
                        enabled = !state.isBusy && !calendarImportBusy &&
                            authorizationRecoveryBlocksMutation &&
                            !calendarImportHasRecovery &&
                            account.status != "disconnecting",
                    ) {
                        Text("Disconnect")
                    }
                }
                if (account.status in setOf("active", "paused")) {
                    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        if (!account.hasCalendarWriteScope) {
                            TextButton(
                                onClick = { onEnableCalendarPublishing(account.id) },
                                enabled = canStartAuthorization,
                                modifier = Modifier.testTag(
                                    "google_enable_calendar_publishing_$accountIndex",
                                ),
                            ) {
                                Text("Enable Calendar publishing")
                            }
                        }
                        if (!account.hasTasksWriteScope) {
                            TextButton(
                                onClick = { onEnableTasksPublishing(account.id) },
                                enabled = canStartAuthorization,
                                modifier = Modifier.testTag(
                                    "google_enable_tasks_publishing_$accountIndex",
                                ),
                            ) {
                                Text("Enable Tasks publishing")
                            }
                        }
                    }
                } else if (
                    account.status == "reauthorization_required" &&
                    account.hasTasksWriteScope
                ) {
                    TextButton(
                        onClick = { onEnableTasksPublishing(account.id) },
                        enabled = canStartAuthorization,
                        modifier = Modifier.testTag(
                            "google_renew_tasks_publishing_$accountIndex",
                        ),
                    ) {
                        Text("Renew Tasks publishing")
                    }
                }
            }
        }
        if (state.requiresPlannerApiConfiguration) {
            HorizontalDivider()
            TextButton(
                onClick = onConfigureApiConnection,
                enabled = !state.isBusy,
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
            ) {
                Text("Configure planner API")
            }
        } else if (
            state.authorization == null && state.phase != GoogleAccountPhase.RECOVERY_REQUIRED &&
            state.authorizationRecovery == null &&
            !state.authorizationRecoveryResetRequired &&
            !state.authorizationRecoveryDiscardRequired &&
            state.phase != GoogleAccountPhase.NOT_CONFIGURED && state.accounts.none {
                it.status in setOf("disconnecting", "revocation_failed")
            }
        ) {
            HorizontalDivider()
            TextButton(
                onClick = onConnect,
                enabled = !state.isBusy,
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
            ) {
                Text(if (state.accounts.isEmpty()) "Connect Google" else "Add Google account")
            }
        }
    }
}

private fun googleAuthorizationActionLabel(action: GoogleAuthorizationAction): String = when (action) {
    GoogleAuthorizationAction.CONNECT_READ_ONLY -> "Google connection"
    GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY -> "Google reauthorization"
    GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING -> "Calendar publishing"
    GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING -> "Tasks publishing"
}

@Composable
private fun SettingsSectionTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(horizontal = 4.dp),
    )
}

@Composable
private fun SettingsInfo(icon: ImageVector, title: String, subtitle: String) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(subtitle) },
        leadingContent = { Icon(icon, contentDescription = null) },
    )
}

@Composable
private fun SettingsToggle(
    icon: ImageVector,
    title: String,
    subtitle: String,
    checked: Boolean,
    onToggle: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onToggle).padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Icon(icon, contentDescription = null)
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(checked = checked, onCheckedChange = { onToggle() })
    }
}
