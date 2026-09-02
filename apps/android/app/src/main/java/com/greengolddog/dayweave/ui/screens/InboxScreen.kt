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
import androidx.compose.material.icons.automirrored.outlined.Undo
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.ProposalApplicationReceiptSnapshot
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.isApplicationReady
import com.greengolddog.dayweave.model.usesReservedChangeSetNamespace
import com.greengolddog.dayweave.sync.ProposalApplicationPhase
import com.greengolddog.dayweave.sync.ProposalApplicationState
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundTargetOption
import com.greengolddog.dayweave.sync.SuggestionSyncPhase
import com.greengolddog.dayweave.sync.SuggestionSyncState
import com.greengolddog.dayweave.ui.authoring.CanonicalAuthoringList
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorRoute
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

@Composable
internal fun InboxScreen(
    state: DayWeaveUiState,
    onApprove: (String) -> Unit,
    onReject: (String) -> Unit,
    onEdit: (PlanningSuggestion) -> Unit,
    proposalApplicationState: ProposalApplicationState,
    onReviewProposal: (String) -> Unit,
    onUndoProposal: (String) -> Unit,
    onRecoverProposal: () -> Unit,
    syncState: SuggestionSyncState,
    onRefresh: () -> Unit,
    onConfigureConnection: () -> Unit,
    canonicalActionsEnabled: Boolean,
    canonicalRetryEnabled: Boolean,
    onNewCanonicalItem: () -> Unit,
    onOpenCanonicalEditor: (CanonicalItemEditorRoute) -> Unit,
    onTrashCanonicalItem: suspend (String) -> Boolean,
    onRestoreCanonicalItem: suspend (String) -> Boolean,
    onDiscardCanonicalMutation: suspend (String) -> Boolean,
    onCopyCanonicalConflict: suspend (String) -> Boolean,
    onReviewLegacyDraft: (InboxItem) -> Unit,
    onRetryCanonicalAuthoring: () -> Unit,
    googleOutboundBlocked: Boolean,
    googlePublishingTargets: (String) -> List<GoogleCalendarOutboundTargetOption>,
    onRequestGooglePublication: (
        String,
        List<GoogleCalendarOutboundTargetOption>,
    ) -> Unit,
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
                            "AI suggestions never change your plan directly. Advisory ideas become Inbox drafts; typed change sets require an exact diff review and explicit approval.",
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
                text = { Text("Items") },
            )
            Tab(
                selected = tab == 1,
                onClick = { tab = 1 },
                text = {
                    Text(
                        "Suggestions (${state.pendingSuggestionCount + state.proposalApplications.size})",
                    )
                },
            )
        }

        if (tab == 0) {
            CanonicalAuthoringList(
                state = state,
                actionsEnabled = canonicalActionsEnabled,
                retryEnabled = canonicalRetryEnabled,
                onNewDetailed = onNewCanonicalItem,
                onOpenEditor = onOpenCanonicalEditor,
                onTrashConfirmed = onTrashCanonicalItem,
                onRestore = onRestoreCanonicalItem,
                onDiscard = onDiscardCanonicalMutation,
                onCopyConflict = onCopyCanonicalConflict,
                onReviewLegacy = onReviewLegacyDraft,
                onRetry = onRetryCanonicalAuthoring,
                googleOutboundBlocked = googleOutboundBlocked,
                googlePublishingTargets = googlePublishingTargets,
                onRequestGooglePublication = onRequestGooglePublication,
                modifier = Modifier.weight(1f),
            )
        } else {
            SuggestionList(
                suggestions = state.suggestions,
                onApprove = onApprove,
                onReject = onReject,
                onEdit = onEdit,
                receipts = state.proposalApplications.values.toList(),
                pendingRecovery = state.pendingProposalApplicationMutation != null,
                proposalApplicationState = proposalApplicationState,
                onReviewProposal = onReviewProposal,
                onUndoProposal = onUndoProposal,
                onRecoverProposal = onRecoverProposal,
                syncState = syncState,
                onRefresh = onRefresh,
                onConfigureConnection = onConfigureConnection,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun SuggestionList(
    suggestions: List<PlanningSuggestion>,
    onApprove: (String) -> Unit,
    onReject: (String) -> Unit,
    onEdit: (PlanningSuggestion) -> Unit,
    receipts: List<ProposalApplicationReceiptSnapshot>,
    pendingRecovery: Boolean,
    proposalApplicationState: ProposalApplicationState,
    onReviewProposal: (String) -> Unit,
    onUndoProposal: (String) -> Unit,
    onRecoverProposal: () -> Unit,
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
        if (pendingRecovery) {
            item(key = "proposal-application-recovery") {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.errorContainer,
                    ),
                    modifier = Modifier.testTag("proposal_recovery_card"),
                ) {
                    Column(
                        modifier = Modifier.padding(14.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text("Interrupted proposal operation", style = MaterialTheme.typography.titleMedium)
                        Text(
                            proposalApplicationState.message,
                            style = MaterialTheme.typography.bodySmall,
                        )
                        OutlinedButton(
                            onClick = onRecoverProposal,
                            enabled = !proposalApplicationState.isBusy,
                        ) { Text("Retry exact recovery") }
                    }
                }
            }
        } else if (proposalApplicationState.phase == ProposalApplicationPhase.ERROR) {
            item(key = "proposal-application-error") {
                Text(
                    proposalApplicationState.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
        receipts.sortedByDescending { it.appliedAt }.forEach { receipt ->
            item(key = "proposal-receipt-${receipt.applicationId}") {
                ProposalReceiptCard(receipt, proposalApplicationState, onUndoProposal)
            }
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
            val typedChangeSet = suggestion.isApplicationReady
            val unknownProtectedChangeSet = suggestion.usesReservedChangeSetNamespace &&
                !typedChangeSet
            val proposalActionAvailable = remoteActionAvailable &&
                !proposalApplicationState.isBusy && !pendingRecovery
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
                            enabled = proposalActionAvailable,
                        ) {
                            Icon(Icons.Outlined.Edit, contentDescription = "Edit proposal")
                        }
                    }
                    Text(suggestion.summary, style = MaterialTheme.typography.bodyMedium)
                    suggestion.remotePayloadJson
                        ?.takeUnless { suggestion.usesReservedChangeSetNamespace }
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
                    suggestion.remoteSourceReference?.let { reference ->
                        Text(
                            "Source reference: $reference",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    if (unknownProtectedChangeSet) {
                        Text(
                            "Protected schema ${suggestion.remotePayloadSchema} is not supported by this version. It cannot be accepted through the advisory path.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Button(
                            onClick = {
                                if (typedChangeSet) {
                                    onReviewProposal(suggestion.id)
                                } else {
                                    onApprove(suggestion.id)
                                }
                            },
                            modifier = Modifier.weight(1f),
                            enabled = proposalActionAvailable && !unknownProtectedChangeSet,
                        ) {
                            Text(if (typedChangeSet) "Review exact changes" else "Accept draft")
                        }
                        OutlinedButton(
                            onClick = { onReject(suggestion.id) },
                            modifier = Modifier.weight(1f),
                            enabled = proposalActionAvailable,
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
private fun ProposalReceiptCard(
    receipt: ProposalApplicationReceiptSnapshot,
    applicationState: ProposalApplicationState,
    onUndo: (String) -> Unit,
) {
    val undoAvailable = receipt.status == ProposalApplicationStatusSnapshot.APPLIED &&
        runCatching { Instant.parse(receipt.undoExpiresAt).isAfter(Instant.now()) }.getOrDefault(false)
    Card(modifier = Modifier.testTag("proposal_receipt_${receipt.proposalId}")) {
        Column(
            modifier = Modifier.padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(7.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        if (receipt.status == ProposalApplicationStatusSnapshot.APPLIED) {
                            "Proposal applied"
                        } else {
                            "Proposal application undone"
                        },
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        "${receipt.affectedItemIds.size} affected item(s) · " +
                            "${receipt.commandIds.size} command(s)",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Icon(Icons.Outlined.CloudDone, contentDescription = null)
            }
            Text(
                "Applied ${formatReceiptTime(receipt.appliedAt)} · " +
                    "undo until ${formatReceiptTime(receipt.undoExpiresAt)}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (receipt.status == ProposalApplicationStatusSnapshot.UNDONE) {
                Text(
                    "Undone ${receipt.undoneAt?.let(::formatReceiptTime) ?: "unknown time"}",
                    style = MaterialTheme.typography.labelSmall,
                )
            } else {
                OutlinedButton(
                    onClick = { onUndo(receipt.proposalId) },
                    enabled = undoAvailable && !applicationState.isBusy,
                    modifier = Modifier.testTag("proposal_undo_${receipt.proposalId}"),
                ) {
                    Icon(Icons.AutoMirrored.Outlined.Undo, contentDescription = null)
                    Text(if (undoAvailable) "Undo application" else "Undo window expired")
                }
            }
        }
    }
}

private fun formatReceiptTime(raw: String): String = runCatching {
    DateTimeFormatter
        .ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(raw))
}.getOrDefault(raw)

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
