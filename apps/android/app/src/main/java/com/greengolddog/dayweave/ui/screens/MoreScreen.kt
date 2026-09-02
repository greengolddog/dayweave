package com.greengolddog.dayweave.ui.screens

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
import androidx.compose.material.icons.outlined.HealthAndSafety
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.PrivacyTip
import androidx.compose.material.icons.outlined.Shield
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.Card
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.health.EnergySignalPhase
import com.greengolddog.dayweave.health.EnergySignalState
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.PendingCanonicalMutation
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.network.GoogleInboundCollectionRole
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.security.AppLockState
import com.greengolddog.dayweave.security.AppLockTimeout
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary
import com.greengolddog.dayweave.sync.GoogleCalendarImportState
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateState
import com.greengolddog.dayweave.ui.components.AppLockSettingsCard

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
    onSetGooglePaused: (String, Boolean) -> Unit,
    onRequestGoogleDisconnect: (GoogleAccountSummary) -> Unit,
    onDiscoverGoogleSources: (String) -> Unit,
    onRefreshGoogleImport: (String) -> Unit,
    onConfigureGoogleSource: (
        String,
        String,
        Long,
        RemoteGoogleCollectionKind,
        GoogleInboundCollectionRole,
    ) -> Unit,
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
    modifier: Modifier = Modifier,
) {
    var pendingSensitivityRemoval by remember {
        mutableStateOf<CanonicalItemSnapshot?>(null)
    }
    var showPlanningProfileEditor by rememberSaveable { mutableStateOf(false) }
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
                                        "Syncing items and composing Today"
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
                onSetPaused = onSetGooglePaused,
                onRequestDisconnect = onRequestGoogleDisconnect,
                calendarImportBusy = googleCalendarImportState.isBusy,
                calendarImportHasRecovery =
                    googleCalendarImportState.pendingRecoveryCount > 0,
            )
        }

        item {
            GoogleSourcesCard(
                googleAccountState = googleAccountState,
                importState = googleCalendarImportState,
                onDiscover = onDiscoverGoogleSources,
                onRefreshOrCheck = onRefreshGoogleImport,
                onConfigure = onConfigureGoogleSource,
                actionsEnabled = !canonicalSyncState.isBusy,
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
private fun GoogleConnectionCard(
    state: GoogleAccountState,
    onConfigureApiConnection: () -> Unit,
    onConnect: () -> Unit,
    onRefresh: () -> Unit,
    onRestartAuthorization: () -> Unit,
    onOpenAuthorization: (String) -> Unit,
    onReauthorize: (String) -> Unit,
    onSetPaused: (String, Boolean) -> Unit,
    onRequestDisconnect: (GoogleAccountSummary) -> Unit,
    calendarImportBusy: Boolean,
    calendarImportHasRecovery: Boolean,
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
                Text("Authorization failed? Start over")
            }
        }
        state.accounts.forEach { account ->
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
                    if (account.hasCalendar) add("Calendar")
                    if (account.hasTasks) add("Tasks")
                }.joinToString(" · ").ifEmpty { "Authorization incomplete" }
                Text(
                    "$capabilities · ${account.status.replace('_', ' ')}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    if (account.status == "reauthorization_required") {
                        TextButton(
                            onClick = { onReauthorize(account.id) },
                            enabled = !state.isBusy,
                        ) {
                            Text("Reauthorize")
                        }
                    } else if (account.status in setOf("active", "paused")) {
                        TextButton(
                            onClick = { onSetPaused(account.id, account.status == "active") },
                            enabled = !state.isBusy && !calendarImportBusy &&
                                (!calendarImportHasRecovery || account.status == "paused"),
                        ) {
                            Text(if (account.status == "active") "Pause sync" else "Resume sync")
                        }
                    }
                    TextButton(
                        onClick = { onRequestDisconnect(account) },
                        enabled = !state.isBusy && !calendarImportBusy &&
                            !calendarImportHasRecovery &&
                            account.status != "disconnecting",
                    ) {
                        Text("Disconnect")
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
