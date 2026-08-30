package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.CloudSync
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.PrivacyTip
import androidx.compose.material.icons.outlined.RestoreFromTrash
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.InboxItem
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import kotlinx.coroutines.launch

@Composable
internal fun CanonicalAuthoringList(
    state: DayWeaveUiState,
    actionsEnabled: Boolean,
    retryEnabled: Boolean,
    onNewDetailed: () -> Unit,
    onOpenEditor: (CanonicalItemEditorRoute) -> Unit,
    onTrashConfirmed: suspend (String) -> Boolean,
    onRestore: suspend (String) -> Boolean,
    onDiscard: suspend (String) -> Boolean,
    onCopyConflict: suspend (String) -> Boolean,
    onReviewLegacy: (InboxItem) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val presentation = remember(
        state.canonicalItems,
        state.pendingCanonicalAuthoringMutations,
        state.canonicalRecentlyDeleted,
        state.pendingCanonicalMutation,
    ) { CanonicalAuthoringPresentation.build(state) }
    var trashCandidate by remember { mutableStateOf<CanonicalAuthoringRow?>(null) }
    var diagnostic by remember { mutableStateOf<String?>(null) }
    var actionInFlight by remember { mutableStateOf(false) }
    val coroutineScope = rememberCoroutineScope()
    val effectiveActionsEnabled = actionsEnabled && !actionInFlight

    fun perform(message: String, action: suspend () -> Boolean) {
        if (actionInFlight) return
        actionInFlight = true
        diagnostic = null
        coroutineScope.launch {
            diagnostic = if (action()) null else message
            actionInFlight = false
        }
    }

    LazyColumn(
        modifier = modifier.testTag("canonical_authoring_list"),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item(key = "canonical-authoring-intro") {
            Card(
                colors = CardDefaults.cardColors(
                    containerColor = MaterialTheme.colorScheme.secondaryContainer,
                ),
            ) {
                Column(
                    modifier = Modifier.padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(9.dp),
                ) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Icon(Icons.Outlined.Inbox, contentDescription = null)
                        Column(modifier = Modifier.weight(1f)) {
                            Text("Canonical items", style = MaterialTheme.typography.titleMedium)
                            Text(
                                "Inbox waits for triage. Planned becomes eligible for composition after sync.",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(
                            onClick = onNewDetailed,
                            enabled = effectiveActionsEnabled,
                            modifier = Modifier.testTag("canonical_new_detailed"),
                        ) {
                            Icon(Icons.Outlined.Add, contentDescription = null)
                            Text("Detailed item")
                        }
                        OutlinedButton(
                            onClick = onRetry,
                            enabled = retryEnabled && !actionInFlight,
                        ) {
                            Icon(Icons.Outlined.CloudSync, contentDescription = null)
                            Text("Retry sync")
                        }
                    }
                }
            }
        }

        diagnostic?.let { message ->
            item(key = "canonical-authoring-diagnostic") {
                Row(
                    modifier = Modifier.fillMaxWidth().testTag("canonical_authoring_diagnostic"),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.Top,
                ) {
                    Icon(
                        Icons.Outlined.ErrorOutline,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.error,
                    )
                    Text(
                        message,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        }

        canonicalSection(
            title = "Inbox",
            rows = presentation.inbox,
            emptyMessage = "Nothing is waiting for triage.",
            actionsEnabled = effectiveActionsEnabled,
            onOpenEditor = onOpenEditor,
            onTrash = { trashCandidate = it },
            onRestore = { row ->
                perform("The restore could not be queued.") { onRestore(row.itemId) }
            },
            onDiscard = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The queued change could not be discarded.") { onDiscard(mutationId) }
                }
            },
            onCopy = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The retained draft could not be copied.") {
                        onCopyConflict(mutationId)
                    }
                }
            },
        )
        canonicalSection(
            title = "Planned",
            rows = presentation.planned,
            emptyMessage = "Move an item to Planned when it is ready for composition.",
            actionsEnabled = effectiveActionsEnabled,
            onOpenEditor = onOpenEditor,
            onTrash = { trashCandidate = it },
            onRestore = { row ->
                perform("The restore could not be queued.") { onRestore(row.itemId) }
            },
            onDiscard = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The queued change could not be discarded.") { onDiscard(mutationId) }
                }
            },
            onCopy = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The retained draft could not be copied.") { onCopyConflict(mutationId) }
                }
            },
        )
        canonicalSection(
            title = "Conflicts",
            rows = presentation.conflicts,
            emptyMessage = "No authoring conflicts need review.",
            actionsEnabled = effectiveActionsEnabled,
            onOpenEditor = onOpenEditor,
            onTrash = { trashCandidate = it },
            onRestore = { row ->
                perform("The restore could not be queued.") { onRestore(row.itemId) }
            },
            onDiscard = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The conflict could not be discarded.") { onDiscard(mutationId) }
                }
            },
            onCopy = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The retained draft could not be copied.") { onCopyConflict(mutationId) }
                }
            },
        )
        canonicalSection(
            title = "Recently Deleted",
            rows = presentation.recentlyDeleted,
            emptyMessage = "Deleted items available for restore appear here.",
            actionsEnabled = effectiveActionsEnabled,
            onOpenEditor = onOpenEditor,
            onTrash = { trashCandidate = it },
            onRestore = { row ->
                perform("The restore could not be queued.") { onRestore(row.itemId) }
            },
            onDiscard = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The queued deletion or restore could not be canceled.") {
                        onDiscard(mutationId)
                    }
                }
            },
            onCopy = { row ->
                row.mutationId?.let { mutationId ->
                    perform("The retained draft could not be copied.") { onCopyConflict(mutationId) }
                }
            },
        )

        legacyCapturedSection(
            legacyItems = state.inbox,
            actionsEnabled = effectiveActionsEnabled,
            onReview = onReviewLegacy,
        )

        item(key = "canonical-authoring-local-first") {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.Top,
            ) {
                Icon(Icons.Outlined.Lock, contentDescription = null)
                Text(
                    "Create, edit, delete, and restore save encrypted local intent first. Submitted requests remain visible until authoritative reconciliation.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }

    trashCandidate?.let { row ->
        AlertDialog(
            onDismissRequest = { trashCandidate = null },
            icon = { Icon(Icons.Outlined.DeleteOutline, contentDescription = null) },
            title = { Text("Move to Recently Deleted?") },
            text = {
                Text(
                    "${row.title} will be queued for deletion. It remains recoverable from Recently Deleted after sync.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        perform("The deletion could not be queued.") {
                            onTrashConfirmed(row.itemId)
                        }
                        trashCandidate = null
                    },
                    modifier = Modifier.testTag("canonical_trash_confirm"),
                ) { Text("Move to Recently Deleted") }
            },
            dismissButton = {
                TextButton(onClick = { trashCandidate = null }) { Text("Cancel") }
            },
        )
    }
}

private fun androidx.compose.foundation.lazy.LazyListScope.canonicalSection(
    title: String,
    rows: List<CanonicalAuthoringRow>,
    emptyMessage: String,
    actionsEnabled: Boolean,
    onOpenEditor: (CanonicalItemEditorRoute) -> Unit,
    onTrash: (CanonicalAuthoringRow) -> Unit,
    onRestore: (CanonicalAuthoringRow) -> Unit,
    onDiscard: (CanonicalAuthoringRow) -> Unit,
    onCopy: (CanonicalAuthoringRow) -> Unit,
) {
    item(key = "canonical-section-$title") {
        Text(
            "$title · ${rows.size}",
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.testTag("canonical_section_${title.lowercase().replace(' ', '_')}_header"),
        )
    }
    if (rows.isEmpty()) {
        item(key = "canonical-empty-$title") {
            Text(
                emptyMessage,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(bottom = 4.dp),
            )
        }
    }
    items(rows, key = { "$title:${it.itemId}" }) { row ->
        CanonicalAuthoringCard(
            row = row,
            actionsEnabled = actionsEnabled,
            onOpenEditor = onOpenEditor,
            onTrash = { onTrash(row) },
            onRestore = { onRestore(row) },
            onDiscard = { onDiscard(row) },
            onCopy = { onCopy(row) },
        )
    }
}

@Composable
private fun CanonicalAuthoringCard(
    row: CanonicalAuthoringRow,
    actionsEnabled: Boolean,
    onOpenEditor: (CanonicalItemEditorRoute) -> Unit,
    onTrash: () -> Unit,
    onRestore: () -> Unit,
    onDiscard: () -> Unit,
    onCopy: () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth().testTag("canonical_row_${row.itemId}"),
        colors = CardDefaults.cardColors(
            containerColor = if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) {
                MaterialTheme.colorScheme.errorContainer
            } else {
                MaterialTheme.colorScheme.surface
            },
        ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Spacer(Modifier.width((row.depth.coerceAtMost(5) * 12).dp))
            Column(modifier = Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.Top,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            row.title,
                            style = MaterialTheme.typography.titleMedium,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (row.breadcrumb.isNotEmpty()) {
                            Text(
                                row.breadcrumb.joinToString(" › "),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                    Text(
                        row.syncState.label(),
                        style = MaterialTheme.typography.labelSmall,
                        color = if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.primary
                        },
                    )
                }
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(row.kind.label, style = MaterialTheme.typography.labelMedium)
                    Text("· ${row.placement.label()}", style = MaterialTheme.typography.labelMedium)
                    row.durationSeconds?.let { seconds ->
                        Text("· ${durationLabel(seconds)}", style = MaterialTheme.typography.labelMedium)
                    }
                    if (row.isSensitive) {
                        Icon(
                            Icons.Outlined.PrivacyTip,
                            contentDescription = "Sensitive canonical item",
                            tint = MaterialTheme.colorScheme.tertiary,
                        )
                        Text(
                            "SENSITIVE",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.tertiary,
                        )
                    }
                }
                row.deadlineAt?.let { deadline ->
                    Text(
                        "Due ${formatInstant(deadline)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (row.hasMissingParent || row.hasHierarchyCycle) {
                    Text(
                        if (row.hasHierarchyCycle) "Hierarchy cycle" else "Parent unavailable",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                row.diagnostic?.let {
                    Text(it, style = MaterialTheme.typography.bodySmall)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    val editable = row.draft != null && !row.isReadOnly &&
                        row.source in setOf(
                            CanonicalAuthoringRowSource.CANONICAL,
                            CanonicalAuthoringRowSource.LOCAL_CREATE,
                            CanonicalAuthoringRowSource.PENDING_REPLACE,
                        )
                    if (editable) {
                        OutlinedButton(
                            onClick = {
                                val draft = requireNotNull(row.draft)
                                onOpenEditor(
                                    if (row.mutationId == null) {
                                        CanonicalItemEditorRoute(
                                            itemId = row.itemId,
                                            initialDraft = draft,
                                            mode = CanonicalItemEditorMode.REPLACE,
                                        )
                                    } else {
                                        CanonicalItemEditorRoute(
                                            itemId = row.itemId,
                                            initialDraft = draft,
                                            mode = CanonicalItemEditorMode.UPDATE_PENDING,
                                            mutationId = row.mutationId,
                                        )
                                    },
                                )
                            },
                            enabled = actionsEnabled,
                        ) {
                            Icon(Icons.Outlined.Edit, contentDescription = null)
                            Text("Edit")
                        }
                    }
                    when (row.source) {
                        CanonicalAuthoringRowSource.CANONICAL -> OutlinedButton(
                            onClick = onTrash,
                            enabled = actionsEnabled && !row.isReadOnly,
                        ) {
                            Icon(Icons.Outlined.DeleteOutline, contentDescription = null)
                            Text("Delete")
                        }
                        CanonicalAuthoringRowSource.LOCAL_CREATE,
                        CanonicalAuthoringRowSource.PENDING_REPLACE,
                        -> {
                            if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) {
                                OutlinedButton(onClick = onCopy, enabled = actionsEnabled) {
                                    Icon(Icons.Outlined.ContentCopy, contentDescription = null)
                                    Text("Copy as new")
                                }
                            }
                            if (row.syncState != CanonicalAuthoringSyncState.SUBMITTED) {
                                TextButton(onClick = onDiscard, enabled = actionsEnabled) {
                                    Text(
                                        if (row.source == CanonicalAuthoringRowSource.LOCAL_CREATE) {
                                            "Discard"
                                        } else {
                                            "Keep server version"
                                        },
                                    )
                                }
                            }
                        }
                        CanonicalAuthoringRowSource.RECENTLY_DELETED -> OutlinedButton(
                            onClick = onRestore,
                            enabled = actionsEnabled,
                        ) {
                            Icon(Icons.Outlined.RestoreFromTrash, contentDescription = null)
                            Text("Restore")
                        }
                        CanonicalAuthoringRowSource.PENDING_TRASH,
                        CanonicalAuthoringRowSource.PENDING_RESTORE,
                        -> if (row.syncState != CanonicalAuthoringSyncState.SUBMITTED) {
                            TextButton(onClick = onDiscard, enabled = actionsEnabled) {
                                Text("Cancel")
                            }
                        }
                        CanonicalAuthoringRowSource.ACTIVE_RESTORE ->
                            if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) {
                                TextButton(onClick = onDiscard, enabled = actionsEnabled) {
                                    Text("Keep active")
                                }
                            }
                    }
                }
            }
        }
    }
}

private fun androidx.compose.foundation.lazy.LazyListScope.legacyCapturedSection(
    legacyItems: List<InboxItem>,
    actionsEnabled: Boolean,
    onReview: (InboxItem) -> Unit,
) {
    item(key = "legacy-captured-header") {
        Text(
            "Local review drafts · ${legacyItems.size}",
            style = MaterialTheme.typography.titleMedium,
        )
    }
    if (legacyItems.isEmpty()) {
        item(key = "legacy-captured-empty") {
            Text(
                "No legacy or proposal-derived local drafts.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    items(legacyItems, key = { "legacy:${it.id}" }) { item ->
        LegacyCapturedCard(item, actionsEnabled, onReview)
    }
}

@Composable
private fun LegacyCapturedCard(
    item: InboxItem,
    actionsEnabled: Boolean,
    onReview: (InboxItem) -> Unit,
) {
    Card {
        Row(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Icon(Icons.Outlined.History, contentDescription = null)
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
                OutlinedButton(
                    onClick = { onReview(item) },
                    enabled = actionsEnabled,
                    modifier = Modifier.padding(top = 8.dp),
                ) {
                    Icon(Icons.Outlined.Edit, contentDescription = null)
                    Text("Review as item")
                }
            }
            if (item.isSensitive) {
                Column(horizontalAlignment = Alignment.End) {
                    Icon(Icons.Outlined.PrivacyTip, contentDescription = "Sensitive draft")
                    Text("SENSITIVE", style = MaterialTheme.typography.labelSmall)
                }
            }
        }
    }
}

private fun CanonicalAuthoringSyncState.label(): String = when (this) {
    CanonicalAuthoringSyncState.SYNCED -> "SYNCED"
    CanonicalAuthoringSyncState.QUEUED -> "QUEUED"
    CanonicalAuthoringSyncState.SUBMITTED -> "RECOVERING"
    CanonicalAuthoringSyncState.CONFLICTED -> "CONFLICT"
}

private fun com.greengolddog.dayweave.model.CanonicalDraftPlacement.label(): String =
    if (this == com.greengolddog.dayweave.model.CanonicalDraftPlacement.INBOX) {
        "Inbox"
    } else {
        "Planned"
    }

private fun durationLabel(seconds: Long): String = when {
    seconds % 3_600L == 0L -> "${seconds / 3_600L}h"
    seconds % 60L == 0L -> "${seconds / 60L}m"
    else -> "${seconds}s"
}

private fun formatInstant(value: String): String = runCatching {
    DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrDefault(value)
