package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import com.greengolddog.dayweave.model.GoogleSchedulePublicationChangeSnapshot
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStage
import com.greengolddog.dayweave.model.GoogleSchedulePublicationTarget
import com.greengolddog.dayweave.network.ScheduleGooglePublicationOperation
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationApprovalConfirmation
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationPhase
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationState
import com.greengolddog.dayweave.sync.GoogleSchedulePublicationTargetOption
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** Secure review surface; one-shot capabilities are intentionally absent from every parameter. */
@Composable
internal fun GoogleSchedulePublicationReviewSheet(
    state: GoogleSchedulePublicationState,
    targets: List<GoogleSchedulePublicationTargetOption>,
    selectedTarget: GoogleSchedulePublicationTargetOption?,
    savedDestinationDisplayName: String?,
    approvalConfirmation: GoogleSchedulePublicationApprovalConfirmation?,
    recoveryStage: GoogleSchedulePublicationStage?,
    canRecover: Boolean,
    canDiscardExpiredRecovery: Boolean,
    canDismissSettled: Boolean,
    onTargetSelected: (GoogleSchedulePublicationTargetOption) -> Unit,
    onRequestPreview: (GoogleSchedulePublicationTarget) -> Unit,
    onApproveAndQueue: (GoogleSchedulePublicationApprovalConfirmation) -> Unit,
    onRecover: () -> Unit,
    onReplayApproved: () -> Unit,
    onDiscardExpiredRecovery: () -> Unit,
    onDismissSettled: () -> Unit,
    onDismissRequest: () -> Unit,
) {
    var pendingRecoveryConfirmation by remember(recoveryStage) {
        mutableStateOf<GoogleScheduleRecoveryConfirmation?>(null)
    }
    val effectiveTarget = when {
        targets.size == 1 -> targets.single()
        selectedTarget != null && selectedTarget in targets -> selectedTarget
        else -> null
    }
    val canStart = !state.hasPendingRecovery &&
        state.phase in setOf(
            GoogleSchedulePublicationPhase.READY,
            GoogleSchedulePublicationPhase.ERROR,
        )

    Dialog(
        onDismissRequest = { if (!state.isBusy) onDismissRequest() },
        properties = DialogProperties(
            dismissOnBackPress = !state.isBusy,
            dismissOnClickOutside = !state.isBusy,
            securePolicy = SecureFlagPolicy.SecureOn,
            usePlatformDefaultWidth = false,
        ),
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 18.dp)
                .testTag(GOOGLE_SCHEDULE_PUBLICATION_SHEET_TAG),
            shape = RoundedCornerShape(28.dp),
            tonalElevation = 6.dp,
            shadowElevation = 12.dp,
        ) {
            Column(
                modifier = Modifier.padding(horizontal = 22.dp, vertical = 20.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                Header(state.isBusy, onDismissRequest)
                if (state.isBusy) LinearProgressIndicator(Modifier.fillMaxWidth())
                StatusCard(state)

                LazyColumn(
                    modifier = Modifier.heightIn(max = 500.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    if (canStart) {
                        item {
                            DestinationPicker(
                                targets,
                                effectiveTarget,
                                onTargetSelected,
                            )
                        }
                    }
                    state.preview?.let { preview ->
                        item {
                            ReviewSummary(
                                destination = savedDestinationDisplayName
                                    ?: preview.collectionDisplayName,
                                createCount = preview.createCount,
                                updateCount = preview.updateCount,
                                deleteCount = preview.deleteCount,
                                noopCount = preview.noopCount,
                            )
                        }
                        itemsIndexed(
                            items = preview.changes,
                            key = { _, change -> change.slotId },
                        ) { index, change ->
                            ChangeRow(index, change)
                        }
                    }
                    state.status?.let { status ->
                        item {
                            DeliveryProgress(
                                total = status.totalCount,
                                pending = status.pendingCount,
                                delivering = status.deliveringCount,
                                published = status.publishedCount,
                                conflicted = status.conflictedCount,
                                failed = status.failedCount,
                                superseded = status.supersededCount,
                            )
                        }
                    }
                    if (state.hasPendingRecovery) {
                        item {
                            RecoveryActions(
                                canRecover = canRecover && !state.isBusy,
                                canDiscard = canDiscardExpiredRecovery && !state.isBusy,
                                canDismissSettled = canDismissSettled && !state.isBusy,
                                onRecover = {
                                    if (googleScheduleRecoveryRequiresConfirmation(recoveryStage)) {
                                        pendingRecoveryConfirmation =
                                            GoogleScheduleRecoveryConfirmation.REPLAY_APPROVED
                                    } else {
                                        onRecover()
                                    }
                                },
                                onDiscard = {
                                    pendingRecoveryConfirmation =
                                        GoogleScheduleRecoveryConfirmation.DISCARD_EXPIRED
                                },
                                onDismissSettled = onDismissSettled,
                            )
                        }
                    }
                }

                HorizontalDivider()
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.End),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    TextButton(onClick = onDismissRequest, enabled = !state.isBusy) {
                        Text("Close")
                    }
                    if (canStart) {
                        Button(
                            onClick = { effectiveTarget?.target?.let(onRequestPreview) },
                            enabled = effectiveTarget != null && !state.isBusy,
                            modifier = Modifier.testTag(GOOGLE_SCHEDULE_PUBLICATION_PREVIEW_TAG),
                        ) { Text("Review changes") }
                    }
                    if (
                        state.phase == GoogleSchedulePublicationPhase.AWAITING_APPROVAL &&
                        state.preview != null && approvalConfirmation != null
                    ) {
                        Button(
                            onClick = { onApproveAndQueue(approvalConfirmation) },
                            enabled = !state.isBusy,
                            modifier = Modifier.testTag(GOOGLE_SCHEDULE_PUBLICATION_APPROVE_TAG),
                        ) {
                            Icon(Icons.Outlined.CheckCircle, null, Modifier.size(18.dp))
                            Spacer(Modifier.size(8.dp))
                            Text("Approve & publish")
                        }
                    }
                }
            }
        }
    }

    pendingRecoveryConfirmation?.let { confirmation ->
        AlertDialog(
            onDismissRequest = { pendingRecoveryConfirmation = null },
            title = {
                Text(
                    when (confirmation) {
                        GoogleScheduleRecoveryConfirmation.REPLAY_APPROVED ->
                            "Recover approved publication?"
                        GoogleScheduleRecoveryConfirmation.DISCARD_EXPIRED ->
                            "Discard expired recovery?"
                    },
                )
            },
            text = {
                Text(
                    when (confirmation) {
                        GoogleScheduleRecoveryConfirmation.REPLAY_APPROVED ->
                            "A prior enqueue response may have been lost. Verify Google Calendar " +
                                "or DayWeave server state before retrying. This can enqueue the " +
                                "previously approved changes; it does not create a new approval."
                        GoogleScheduleRecoveryConfirmation.DISCARD_EXPIRED ->
                            "This permanently removes the encrypted local recovery record. " +
                                "It does not cancel work already accepted by the server."
                    },
                )
            },
            confirmButton = {
                Button(
                    onClick = {
                        pendingRecoveryConfirmation = null
                        when (confirmation) {
                            GoogleScheduleRecoveryConfirmation.REPLAY_APPROVED -> onReplayApproved()
                            GoogleScheduleRecoveryConfirmation.DISCARD_EXPIRED ->
                                onDiscardExpiredRecovery()
                        }
                    },
                    modifier = Modifier.testTag(
                        when (confirmation) {
                            GoogleScheduleRecoveryConfirmation.REPLAY_APPROVED ->
                                GOOGLE_SCHEDULE_RECOVER_CONFIRM_TAG
                            GoogleScheduleRecoveryConfirmation.DISCARD_EXPIRED ->
                                GOOGLE_SCHEDULE_DISCARD_CONFIRM_TAG
                        },
                    ),
                ) {
                    Text(
                        when (confirmation) {
                            GoogleScheduleRecoveryConfirmation.REPLAY_APPROVED -> "Recover & enqueue"
                            GoogleScheduleRecoveryConfirmation.DISCARD_EXPIRED -> "Discard recovery"
                        },
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingRecoveryConfirmation = null }) {
                    Text("Cancel")
                }
            },
            properties = DialogProperties(securePolicy = SecureFlagPolicy.SecureOn),
        )
    }
}

private enum class GoogleScheduleRecoveryConfirmation {
    REPLAY_APPROVED,
    DISCARD_EXPIRED,
}

internal fun googleScheduleRecoveryRequiresConfirmation(
    stage: GoogleSchedulePublicationStage?,
): Boolean = stage == GoogleSchedulePublicationStage.APPROVED

@Composable
private fun Header(isBusy: Boolean, onDismissRequest: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Surface(
            shape = RoundedCornerShape(14.dp),
            color = MaterialTheme.colorScheme.primaryContainer,
        ) {
            Icon(
                Icons.Outlined.CalendarMonth,
                null,
                Modifier.padding(10.dp),
                tint = MaterialTheme.colorScheme.onPrimaryContainer,
            )
        }
        Column(Modifier.weight(1f)) {
            Text(
                "Publish generated schedule",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                "Private, opaque Calendar events · no attendees or conferencing",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        IconButton(onClick = onDismissRequest, enabled = !isBusy) {
            Icon(Icons.Outlined.Close, "Close schedule publication review")
        }
    }
}

@Composable
private fun StatusCard(state: GoogleSchedulePublicationState) {
    val warning = state.phase in setOf(
        GoogleSchedulePublicationPhase.CONFLICT,
        GoogleSchedulePublicationPhase.FAILED,
        GoogleSchedulePublicationPhase.RESPONSE_UNKNOWN,
        GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
        GoogleSchedulePublicationPhase.EXPIRED,
        GoogleSchedulePublicationPhase.AUTH_REQUIRED,
    )
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = if (warning) MaterialTheme.colorScheme.errorContainer
        else MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Row(
            Modifier.padding(14.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Icon(
                if (state.hasPendingRecovery) Icons.Outlined.Lock
                else Icons.Outlined.CalendarMonth,
                null,
                Modifier.size(20.dp),
            )
            Text(state.message, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun DestinationPicker(
    targets: List<GoogleSchedulePublicationTargetOption>,
    selected: GoogleSchedulePublicationTargetOption?,
    onSelected: (GoogleSchedulePublicationTargetOption) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Destination", style = MaterialTheme.typography.titleSmall)
        if (targets.isEmpty()) {
            Text(
                "No selected owner/writer calendar is available. Refresh Google sources and enable a writable calendar.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            OutlinedButton(
                onClick = { expanded = true },
                modifier = Modifier.fillMaxWidth().testTag(GOOGLE_SCHEDULE_DESTINATION_TAG),
            ) {
                Text(
                    selected?.displayName ?: "Choose Google account and calendar",
                    modifier = Modifier.weight(1f),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Icon(Icons.Outlined.ExpandMore, null)
            }
            DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                targets.forEach { target ->
                    DropdownMenuItem(
                        text = {
                            Text(target.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        },
                        onClick = {
                            expanded = false
                            onSelected(target)
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ReviewSummary(
    destination: String,
    createCount: Int,
    updateCount: Int,
    deleteCount: Int,
    noopCount: Int,
) {
    Surface(shape = RoundedCornerShape(16.dp), color = MaterialTheme.colorScheme.secondaryContainer) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(destination, style = MaterialTheme.typography.titleSmall, maxLines = 2)
            Text(
                "$createCount new · $updateCount updates · $deleteCount removals · $noopCount unchanged",
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                "Only the exact list shown here will be approved.",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
            )
        }
    }
}

@Composable
private fun ChangeRow(index: Int, change: GoogleSchedulePublicationChangeSnapshot) {
    Column(
        modifier = Modifier.fillMaxWidth().testTag("google_schedule_change_$index"),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                when (change.operation) {
                    ScheduleGooglePublicationOperation.CREATE -> "NEW"
                    ScheduleGooglePublicationOperation.UPDATE -> "UPDATE"
                    ScheduleGooglePublicationOperation.DELETE -> "REMOVE"
                    ScheduleGooglePublicationOperation.NOOP -> "UNCHANGED"
                },
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.primary,
            )
            Text(
                change.summary,
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Text(
            formatScheduleRange(change.startsAt, change.endsAt),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    }
}

@Composable
private fun DeliveryProgress(
    total: Int,
    pending: Int,
    delivering: Int,
    published: Int,
    conflicted: Int,
    failed: Int,
    superseded: Int,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("Delivery", style = MaterialTheme.typography.titleSmall)
        Text(
            "$published of $total published · ${pending + delivering} remaining",
            style = MaterialTheme.typography.bodyMedium,
        )
        if (conflicted + failed + superseded > 0) {
            Text(
                "$conflicted conflicts · $failed failed · $superseded superseded",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

@Composable
private fun RecoveryActions(
    canRecover: Boolean,
    canDiscard: Boolean,
    canDismissSettled: Boolean,
    onRecover: () -> Unit,
    onDiscard: () -> Unit,
    onDismissSettled: () -> Unit,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        if (canRecover) {
            OutlinedButton(onClick = onRecover) {
                Icon(Icons.Outlined.Refresh, null, Modifier.size(18.dp))
                Spacer(Modifier.size(6.dp))
                Text("Check / recover")
            }
        }
        if (canDiscard) TextButton(onClick = onDiscard) { Text("Discard expired") }
        if (canDismissSettled) TextButton(onClick = onDismissSettled) { Text("Done") }
    }
}

private fun formatScheduleRange(start: String, end: String): String = runCatching {
    val zone = ZoneId.systemDefault()
    val formatter = DateTimeFormatter.ofPattern("EEE, MMM d · HH:mm")
    val from = Instant.parse(start).atZone(zone)
    val to = Instant.parse(end).atZone(zone)
    "${formatter.format(from)} – ${DateTimeFormatter.ofPattern("HH:mm").format(to)}"
}.getOrDefault("Scheduled time unavailable")

internal const val GOOGLE_SCHEDULE_PUBLICATION_SHEET_TAG =
    "google_schedule_publication_review_sheet"
internal const val GOOGLE_SCHEDULE_PUBLICATION_PREVIEW_TAG =
    "google_schedule_publication_preview"
internal const val GOOGLE_SCHEDULE_PUBLICATION_APPROVE_TAG =
    "google_schedule_publication_approve"
internal const val GOOGLE_SCHEDULE_DESTINATION_TAG = "google_schedule_publication_destination"
internal const val GOOGLE_SCHEDULE_RECOVER_CONFIRM_TAG =
    "google_schedule_publication_recover_confirm"
internal const val GOOGLE_SCHEDULE_DISCARD_CONFIRM_TAG =
    "google_schedule_publication_discard_confirm"
