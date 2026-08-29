package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material3.Card
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.health.EnergySignalPhase
import com.greengolddog.dayweave.health.EnergySignalState
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.sync.GoogleAccountPhase
import com.greengolddog.dayweave.sync.GoogleAccountState
import com.greengolddog.dayweave.sync.GoogleAccountSummary

@Composable
fun MoreScreen(
    state: DayWeaveUiState,
    onToggleCompleted: () -> Unit,
    onToggleQuietSuggestions: () -> Unit,
    onToggleDynamicColor: () -> Unit,
    suggestionSyncState: SuggestionSyncState,
    canonicalSyncState: CanonicalSyncState,
    googleAccountState: GoogleAccountState,
    energySignalState: EnergySignalState,
    onConfigureApiConnection: () -> Unit,
    onConnectGoogle: () -> Unit,
    onRefreshGoogle: () -> Unit,
    onRestartGoogleAuthorization: () -> Unit,
    onOpenGoogleAuthorization: (String) -> Unit,
    onReauthorizeGoogle: (String) -> Unit,
    onSetGooglePaused: (String, Boolean) -> Unit,
    onRequestGoogleDisconnect: (GoogleAccountSummary) -> Unit,
    onToggleHealthConnect: (Boolean) -> Unit,
    onRefreshHealthConnect: () -> Unit,
    onManageHealthConnectAccess: () -> Unit,
    onInstallHealthConnect: () -> Unit,
    modifier: Modifier = Modifier,
) {
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
                HorizontalDivider()
                SettingsInfo(Icons.Outlined.PrivacyTip, "Privacy & sensitive items", "AI access, lock screen, and MCP permissions")
            }
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
                            enabled = !state.isBusy,
                        ) {
                            Text(if (account.status == "active") "Pause sync" else "Resume sync")
                        }
                    }
                    TextButton(
                        onClick = { onRequestDisconnect(account) },
                        enabled = !state.isBusy && account.status != "disconnecting",
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
