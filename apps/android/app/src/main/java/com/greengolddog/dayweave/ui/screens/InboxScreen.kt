package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.CloudDone
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.GppGood
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.KeyOff
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.SuggestionSyncState
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

@Composable
fun InboxScreen(
    state: DayWeaveUiState,
    onApprove: (String) -> Unit,
    onReject: (String) -> Unit,
    onEdit: (PlanningSuggestion) -> Unit,
    syncState: SuggestionSyncState,
    onRefresh: () -> Unit,
    onConfigureConnection: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var tab by remember { mutableIntStateOf(0) }
    Column(modifier = modifier) {
        Column(
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Surface(
                color = MaterialTheme.colorScheme.secondaryContainer,
                shape = MaterialTheme.shapes.large,
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(14.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Outlined.GppGood, contentDescription = null)
                    Column {
                        Text("Proposal-only safety", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "ChatGPT, Codex, and AI suggestions cannot change your calendar directly. Accepting creates a reviewable Inbox draft.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }

        PrimaryTabRow(selectedTabIndex = tab) {
            Tab(
                selected = tab == 0,
                onClick = { tab = 0 },
                text = { Text("Captured (${state.inbox.size})") },
            )
            Tab(
                selected = tab == 1,
                onClick = { tab = 1 },
                text = { Text("Suggestions (${state.pendingSuggestionCount})") },
            )
        }

        if (tab == 0) {
            CapturedList(state.inbox, Modifier.weight(1f))
        } else {
            SuggestionList(
                suggestions = state.suggestions,
                onApprove = onApprove,
                onReject = onReject,
                onEdit = onEdit,
                syncState = syncState,
                onRefresh = onRefresh,
                onConfigureConnection = onConfigureConnection,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun CapturedList(items: List<InboxItem>, modifier: Modifier = Modifier) {
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        if (items.isEmpty()) {
            item { EmptyInbox("Nothing needs clarification.") }
        }
        items(items, key = { it.id }) { item ->
            Card {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(14.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.Top,
                ) {
                    Icon(Icons.Outlined.Inbox, contentDescription = null)
                    Column(modifier = Modifier.weight(1f)) {
                        Text(item.title, style = MaterialTheme.typography.titleMedium)
                        Text(item.source.label, style = MaterialTheme.typography.labelMedium)
                        if (item.detail.isNotEmpty()) {
                            Text(
                                item.detail,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                    if (item.requiresReview) {
                        Text(
                            "REVIEW",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun SuggestionList(
    suggestions: List<PlanningSuggestion>,
    onApprove: (String) -> Unit,
    onReject: (String) -> Unit,
    onEdit: (PlanningSuggestion) -> Unit,
    syncState: SuggestionSyncState,
    onRefresh: () -> Unit,
    onConfigureConnection: () -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(
        modifier = modifier,
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item(key = "suggestion-connection") {
            SuggestionConnectionCard(
                syncState = syncState,
                onRefresh = onRefresh,
                onConfigureConnection = onConfigureConnection,
            )
        }
        val pending = suggestions.filter { it.disposition == SuggestionDisposition.PENDING }
        if (pending.isEmpty()) {
            item { EmptyInbox("No proposals are waiting for review.") }
        }
        items(pending, key = { it.id }) { suggestion ->
            val remoteActionAvailable = suggestion.remoteRevision == null ||
                syncState.phase !in setOf(
                    SuggestionSyncPhase.NOT_CONFIGURED,
                    SuggestionSyncPhase.AUTH_REQUIRED,
                    SuggestionSyncPhase.SYNCING,
                )
            Card(
                colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
                elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
            ) {
                Column(
                    modifier = Modifier.padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Row(verticalAlignment = Alignment.Top) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(suggestion.kind.label, style = MaterialTheme.typography.labelMedium)
                            Text(suggestion.title, style = MaterialTheme.typography.titleLarge)
                        }
                        IconButton(
                            onClick = { onEdit(suggestion) },
                            enabled = remoteActionAvailable,
                        ) {
                            Icon(Icons.Outlined.Edit, contentDescription = "Edit proposal")
                        }
                    }
                    Text(suggestion.summary, style = MaterialTheme.typography.bodyMedium)
                    suggestion.remotePayloadJson
                        ?.takeIf { it.isNotBlank() && it != "{}" }
                        ?.let { payload ->
                            Text(
                                "Proposed details: $payload",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 6,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    Text(
                        "${suggestion.source} · expires in ${suggestion.expiresInDays} days",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Button(
                            onClick = { onApprove(suggestion.id) },
                            modifier = Modifier.weight(1f),
                            enabled = remoteActionAvailable,
                        ) {
                            Text("Accept draft")
                        }
                        OutlinedButton(
                            onClick = { onReject(suggestion.id) },
                            modifier = Modifier.weight(1f),
                            enabled = remoteActionAvailable,
                        ) {
                            Icon(Icons.Outlined.DeleteOutline, contentDescription = null)
                            Text("Reject")
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun SuggestionConnectionCard(
    syncState: SuggestionSyncState,
    onRefresh: () -> Unit,
    onConfigureConnection: () -> Unit,
) {
    val statusColor = when (syncState.phase) {
        SuggestionSyncPhase.CONNECTED -> MaterialTheme.colorScheme.primary
        SuggestionSyncPhase.SYNCING, SuggestionSyncPhase.READY ->
            MaterialTheme.colorScheme.onSurfaceVariant
        else -> MaterialTheme.colorScheme.error
    }
    val statusIcon = when (syncState.phase) {
        SuggestionSyncPhase.CONNECTED -> Icons.Outlined.CloudDone
        SuggestionSyncPhase.AUTH_REQUIRED -> Icons.Outlined.KeyOff
        else -> Icons.Outlined.CloudOff
    }
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
        ),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (syncState.isBusy) {
                    CircularProgressIndicator(
                        modifier = Modifier.padding(2.dp),
                        strokeWidth = 2.dp,
                    )
                } else {
                    Icon(
                        statusIcon,
                        contentDescription = null,
                        tint = statusColor,
                    )
                }
                Column(modifier = Modifier.weight(1f)) {
                    Text("Authenticated suggestion sync", style = MaterialTheme.typography.titleMedium)
                    Text(
                        syncState.baseUrl ?: "No API endpoint configured",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            Text(syncState.message, style = MaterialTheme.typography.bodySmall, color = statusColor)
            Text(
                "Last successful sync: ${formatLastSync(syncState.lastSuccessfulSyncEpochMillis)}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onRefresh,
                    enabled = syncState.baseUrl != null &&
                        syncState.hasStoredToken &&
                        !syncState.isBusy,
                ) {
                    Icon(Icons.Outlined.Refresh, contentDescription = null)
                    Text("Refresh")
                }
                OutlinedButton(onClick = onConfigureConnection, enabled = !syncState.isBusy) {
                    Icon(Icons.Outlined.Settings, contentDescription = null)
                    Text("Connection")
                }
            }
        }
    }
}

private fun formatLastSync(epochMillis: Long?): String {
    if (epochMillis == null) return "Never"
    return DateTimeFormatter
        .ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)
        .withZone(ZoneId.systemDefault())
        .format(Instant.ofEpochMilli(epochMillis))
}

@Composable
private fun EmptyInbox(message: String) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(vertical = 48.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(Icons.Outlined.Inbox, contentDescription = null)
        Text(message, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
