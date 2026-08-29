package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.DarkMode
import androidx.compose.material.icons.outlined.HealthAndSafety
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.PrivacyTip
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState

@Composable
fun MoreScreen(
    state: DayWeaveUiState,
    onToggleCompleted: () -> Unit,
    onToggleQuietSuggestions: () -> Unit,
    onToggleDynamicColor: () -> Unit,
    suggestionSyncState: SuggestionSyncState,
    canonicalSyncState: CanonicalSyncState,
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
                SettingsLink(Icons.Outlined.CalendarMonth, "Calendars & tasks", "Google Calendar · Google Tasks")
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
                SettingsLink(Icons.Outlined.PrivacyTip, "Privacy & sensitive items", "AI access, lock screen, and MCP permissions")
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
private fun SettingsSectionTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(horizontal = 4.dp),
    )
}

@Composable
private fun SettingsLink(icon: ImageVector, title: String, subtitle: String) {
    ListItem(
        modifier = Modifier.clickable { },
        headlineContent = { Text(title) },
        supportingContent = { Text(subtitle) },
        leadingContent = { Icon(icon, contentDescription = null) },
        trailingContent = { Icon(Icons.Outlined.ChevronRight, contentDescription = null) },
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
