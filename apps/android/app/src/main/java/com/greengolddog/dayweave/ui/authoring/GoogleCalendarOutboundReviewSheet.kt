package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import com.greengolddog.dayweave.model.GoogleCalendarOutboundPreviewSnapshot
import com.greengolddog.dayweave.model.GoogleCalendarOutboundTarget
import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundApprovalConfirmation
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundPhase
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundState
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundTargetOption
import java.time.Instant
import java.time.LocalDate
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * Secure, exact review surface for one Google Calendar or Tasks mutation.
 *
 * The host owns selection and all authority. In particular, approval is impossible unless the
 * coordinator supplies an opaque [GoogleCalendarOutboundApprovalConfirmation] for the preview
 * currently being shown. This component never accepts or exposes an approval capability.
 */
@Composable
internal fun GoogleCalendarOutboundReviewSheet(
    state: GoogleCalendarOutboundState,
    targets: List<GoogleCalendarOutboundTargetOption>,
    selectedTarget: GoogleCalendarOutboundTargetOption?,
    reviewDestinationDisplayName: String? = null,
    reviewItemTitle: String? = null,
    approvalConfirmation: GoogleCalendarOutboundApprovalConfirmation?,
    canRecover: Boolean,
    canDiscardExpiredRecovery: Boolean,
    onTargetSelected: (GoogleCalendarOutboundTargetOption) -> Unit,
    onRequestPreview: (GoogleCalendarOutboundTarget) -> Unit,
    onApproveAndQueue: (GoogleCalendarOutboundApprovalConfirmation) -> Unit,
    onRecover: () -> Unit,
    onDiscardExpiredRecovery: () -> Unit,
    onDismissRequest: () -> Unit,
) {
    val effectiveTarget = when {
        targets.size == 1 -> targets.single()
        selectedTarget != null && targets.contains(selectedTarget) -> selectedTarget
        else -> null
    }
    val presentedEntityKind = state.preview?.entityKind
        ?: effectiveTarget?.target?.entityKind
        ?: selectedTarget?.target?.entityKind
        ?: GoogleCalendarOutboundEntityKind.CALENDAR_EVENT
    val presentedOperation = state.preview?.operation
        ?: effectiveTarget?.target?.operation
        ?: selectedTarget?.target?.operation
        ?: GoogleCalendarOutboundOperation.UPSERT
    val previewPresentation = remember(
        state.preview,
        reviewDestinationDisplayName,
        reviewItemTitle,
    ) {
        state.preview?.toSanitizedOutboundPresentation(reviewItemTitle)?.let { presentation ->
            reviewDestinationDisplayName?.let { displayName ->
                presentation.copy(
                    destination = displayName.sanitizeDisplayText(
                        MAX_DESTINATION_DISPLAY_CHARS,
                    ),
                )
            } ?: presentation
        }
    }
    val canStartPreview = !state.hasPendingRecovery &&
        state.phase in setOf(
            GoogleCalendarOutboundPhase.READY,
            GoogleCalendarOutboundPhase.ERROR,
        )

    Dialog(
        onDismissRequest = { if (!state.isBusy) onDismissRequest() },
        properties = googleCalendarOutboundDialogProperties(state.isBusy),
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 18.dp)
                .testTag(GOOGLE_OUTBOUND_REVIEW_SHEET_TAG),
            shape = RoundedCornerShape(28.dp),
            color = MaterialTheme.colorScheme.surface,
            tonalElevation = 6.dp,
            shadowElevation = 12.dp,
        ) {
            Column(
                modifier = Modifier.padding(horizontal = 22.dp, vertical = 20.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                ReviewHeader(
                    entityKind = presentedEntityKind,
                    operation = presentedOperation,
                    isBusy = state.isBusy,
                    onDismissRequest = onDismissRequest,
                )

                Column(
                    modifier = Modifier
                        .heightIn(max = 580.dp)
                        .verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    StatusBanner(state)

                    when {
                        previewPresentation != null -> PreviewDetails(previewPresentation)
                        state.preview != null -> InvalidPreviewNotice()
                        canStartPreview -> DestinationPicker(
                            targets = targets,
                            selectedTarget = effectiveTarget,
                            entityKind = presentedEntityKind,
                            onTargetSelected = onTargetSelected,
                        )
                    }

                    if (state.hasPendingRecovery) {
                        RecoveryCard(
                            canRecover = canRecover && !state.isBusy,
                            canDiscard = canDiscardExpiredRecovery && !state.isBusy,
                            onRecover = onRecover,
                            onDiscard = onDiscardExpiredRecovery,
                        )
                    }
                }

                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.End),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    TextButton(
                        onClick = onDismissRequest,
                        enabled = !state.isBusy,
                    ) {
                        Text(if (state.phase == GoogleCalendarOutboundPhase.ACCEPTED) "Done" else "Close")
                    }

                    if (canStartPreview) {
                        Button(
                            onClick = { effectiveTarget?.target?.let(onRequestPreview) },
                            enabled = effectiveTarget != null && !state.isBusy,
                            modifier = Modifier.testTag(GOOGLE_OUTBOUND_PREVIEW_BUTTON_TAG),
                        ) {
                            Text("Review exact change")
                        }
                    }

                    if (
                        state.phase == GoogleCalendarOutboundPhase.AWAITING_APPROVAL &&
                        previewPresentation != null &&
                        approvalConfirmation != null
                    ) {
                        Button(
                            onClick = { onApproveAndQueue(approvalConfirmation) },
                            enabled = !state.isBusy,
                            modifier = Modifier.testTag(GOOGLE_OUTBOUND_APPROVE_BUTTON_TAG),
                        ) {
                            Icon(
                                Icons.Outlined.CheckCircle,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp),
                            )
                            Spacer(Modifier.size(8.dp))
                            Text("Approve & Queue")
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ReviewHeader(
    entityKind: GoogleCalendarOutboundEntityKind,
    operation: GoogleCalendarOutboundOperation,
    isBusy: Boolean,
    onDismissRequest: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Surface(
            shape = RoundedCornerShape(14.dp),
            color = MaterialTheme.colorScheme.primaryContainer,
        ) {
            Icon(
                when {
                    operation == GoogleCalendarOutboundOperation.DELETE ->
                        Icons.Outlined.DeleteOutline
                    entityKind == GoogleCalendarOutboundEntityKind.TASK ->
                        Icons.Outlined.Checklist
                    else -> Icons.Outlined.CalendarMonth
                },
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.padding(10.dp),
            )
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                when (entityKind) {
                    GoogleCalendarOutboundEntityKind.CALENDAR_EVENT ->
                        "Review Google Calendar change"
                    GoogleCalendarOutboundEntityKind.TASK ->
                        "Review Google Tasks change"
                },
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                when (entityKind to operation) {
                    GoogleCalendarOutboundEntityKind.CALENDAR_EVENT to
                        GoogleCalendarOutboundOperation.UPSERT ->
                        "One private, attendee-free event"
                    GoogleCalendarOutboundEntityKind.CALENDAR_EVENT to
                        GoogleCalendarOutboundOperation.DELETE ->
                        "Remove one mapped DayWeave event"
                    GoogleCalendarOutboundEntityKind.TASK to
                        GoogleCalendarOutboundOperation.UPSERT ->
                        "One task; planning metadata stays local"
                    GoogleCalendarOutboundEntityKind.TASK to
                        GoogleCalendarOutboundOperation.DELETE ->
                        "Remove one mapped DayWeave task"
                    else -> "One explicitly reviewed Google change"
                },
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        IconButton(
            onClick = onDismissRequest,
            enabled = !isBusy,
        ) {
            Icon(Icons.Outlined.Close, contentDescription = "Close Google publication review")
        }
    }
}

@Composable
private fun StatusBanner(state: GoogleCalendarOutboundState) {
    val isError = state.phase in setOf(
        GoogleCalendarOutboundPhase.AUTH_REQUIRED,
        GoogleCalendarOutboundPhase.ERROR,
        GoogleCalendarOutboundPhase.EXPIRED,
        GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
        GoogleCalendarOutboundPhase.RESPONSE_UNKNOWN,
    )
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = if (isError) {
            MaterialTheme.colorScheme.errorContainer
        } else {
            MaterialTheme.colorScheme.surfaceVariant
        },
    ) {
        Row(
            modifier = Modifier.padding(14.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Icon(
                if (state.hasPendingRecovery) Icons.Outlined.Lock else Icons.Outlined.CalendarMonth,
                contentDescription = null,
                tint = if (isError) {
                    MaterialTheme.colorScheme.onErrorContainer
                } else {
                    MaterialTheme.colorScheme.primary
                },
                modifier = Modifier.size(20.dp),
            )
            Text(
                state.message,
                style = MaterialTheme.typography.bodyMedium,
                color = if (isError) {
                    MaterialTheme.colorScheme.onErrorContainer
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
            )
        }
    }
}

@Composable
private fun DestinationPicker(
    targets: List<GoogleCalendarOutboundTargetOption>,
    selectedTarget: GoogleCalendarOutboundTargetOption?,
    entityKind: GoogleCalendarOutboundEntityKind,
    onTargetSelected: (GoogleCalendarOutboundTargetOption) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            "Destination",
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.SemiBold,
        )
        when {
            targets.isEmpty() -> Text(
                if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
                    "No writable owner or writer calendar is available. Enable Calendar publishing first."
                } else {
                    "No writable task list is available. Enable Google Tasks publishing first."
                },
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )

            targets.size == 1 -> DestinationSurface(targets.single().displayName)

            else -> {
                var expanded by remember(targets, selectedTarget) { mutableStateOf(false) }
                Box(modifier = Modifier.fillMaxWidth()) {
                    OutlinedButton(
                        onClick = { expanded = true },
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag(GOOGLE_OUTBOUND_DESTINATION_PICKER_TAG),
                    ) {
                        Text(
                            selectedTarget?.displayName ?: if (
                                entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT
                            ) {
                                "Choose a calendar"
                            } else {
                                "Choose a task list"
                            },
                            modifier = Modifier.weight(1f),
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Icon(Icons.Outlined.ExpandMore, contentDescription = null)
                    }
                    DropdownMenu(
                        expanded = expanded,
                        onDismissRequest = { expanded = false },
                    ) {
                        targets.forEachIndexed { index, option ->
                            DropdownMenuItem(
                                text = {
                                    Text(
                                        option.displayName,
                                        maxLines = 2,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                                },
                                onClick = {
                                    expanded = false
                                    onTargetSelected(option)
                                },
                                modifier = Modifier.testTag("google_outbound_destination_$index"),
                            )
                        }
                    }
                }
                if (selectedTarget == null) {
                    Text(
                        "Choose the exact ${if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) "calendar" else "task list"} before generating a server preview.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun DestinationSurface(displayName: String) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
    ) {
        Row(
            modifier = Modifier.padding(14.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                Icons.Outlined.CheckCircle,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSecondaryContainer,
            )
            Text(
                displayName,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium,
            )
        }
    }
}

@Composable
private fun PreviewDetails(preview: GoogleCalendarOutboundPreviewPresentation) {
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            when (preview.entityKind) {
                GoogleCalendarOutboundEntityKind.CALENDAR_EVENT -> {
                    ReviewBadge("Private")
                    ReviewBadge("No guests")
                    ReviewBadge("No conferencing")
                }
                GoogleCalendarOutboundEntityKind.TASK -> {
                    ReviewBadge("Google Task")
                    ReviewBadge("Explicit approval")
                    ReviewBadge("Planning stays local")
                }
            }
        }

        ReviewSection("Publication") {
            ReviewField("Destination", preview.destination, GOOGLE_OUTBOUND_DESTINATION_TAG)
            ReviewField("Change", preview.change, GOOGLE_OUTBOUND_CHANGE_TAG)
            ReviewField("Expires", preview.expires, GOOGLE_OUTBOUND_EXPIRY_TAG)
        }

        ReviewSection(
            if (preview.entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
                "Event"
            } else {
                "Task"
            },
        ) {
            ReviewField("Title", preview.title, GOOGLE_OUTBOUND_TITLE_TAG)
            if (preview.operation == GoogleCalendarOutboundOperation.DELETE) {
                Text(
                    if (preview.entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
                        "Google will remove only the mapped event whose retained provider identity and ownership proof still match."
                    } else {
                        "Google will remove only the mapped task whose retained provider identity and version still match."
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (preview.entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
                ReviewField(
                    "Description",
                    preview.description ?: "No description",
                    GOOGLE_OUTBOUND_DESCRIPTION_TAG,
                    subdued = preview.description == null,
                )
                ReviewField("Starts", requireNotNull(preview.starts), GOOGLE_OUTBOUND_START_TAG)
                ReviewField("Ends", requireNotNull(preview.ends), GOOGLE_OUTBOUND_END_TAG)
                ReviewField(
                    "Status",
                    requireNotNull(preview.status),
                    GOOGLE_OUTBOUND_STATUS_TAG,
                )
                ReviewField(
                    "Availability",
                    requireNotNull(preview.transparency),
                    GOOGLE_OUTBOUND_TRANSPARENCY_TAG,
                )
            } else {
                ReviewField(
                    "Notes",
                    preview.description ?: "No notes",
                    GOOGLE_OUTBOUND_DESCRIPTION_TAG,
                    subdued = preview.description == null,
                )
                ReviewField(
                    "Status",
                    requireNotNull(preview.status),
                    GOOGLE_OUTBOUND_STATUS_TAG,
                )
                ReviewField(
                    "Due",
                    preview.due ?: "No due date",
                    GOOGLE_OUTBOUND_DUE_TAG,
                    subdued = preview.due == null,
                )
                preview.completed?.let {
                    ReviewField("Completed", it, GOOGLE_OUTBOUND_COMPLETED_TAG)
                }
            }
        }

        Text(
            "Approval queues only this exact redacted preview. Delivery happens through DayWeave's durable outbox.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ReviewSection(
    title: String,
    content: @Composable () -> Unit,
) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(18.dp),
        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.55f),
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(11.dp),
        ) {
            Text(
                title,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
            )
            content()
        }
    }
}

@Composable
private fun ReviewField(
    label: String,
    value: String,
    tag: String,
    subdued: Boolean = false,
) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            value,
            modifier = Modifier.testTag(tag),
            style = MaterialTheme.typography.bodyMedium,
            color = if (subdued) {
                MaterialTheme.colorScheme.onSurfaceVariant
            } else {
                MaterialTheme.colorScheme.onSurface
            },
        )
    }
}

@Composable
private fun ReviewBadge(label: String) {
    Surface(
        shape = RoundedCornerShape(999.dp),
        color = MaterialTheme.colorScheme.primaryContainer,
    ) {
        Text(
            label,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
        )
    }
}

@Composable
private fun InvalidPreviewNotice() {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.errorContainer,
    ) {
        Text(
            "This server preview is not an exact supported Calendar or Tasks change and cannot be approved here.",
            modifier = Modifier.padding(14.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onErrorContainer,
        )
    }
}

@Composable
private fun RecoveryCard(
    canRecover: Boolean,
    canDiscard: Boolean,
    onRecover: () -> Unit,
    onDiscard: () -> Unit,
) {
    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(GOOGLE_OUTBOUND_RECOVERY_CARD_TAG),
        shape = RoundedCornerShape(18.dp),
        color = MaterialTheme.colorScheme.tertiaryContainer,
    ) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(9.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    Icons.Outlined.Lock,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onTertiaryContainer,
                )
                Text(
                    "Saved publication recovery",
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onTertiaryContainer,
                    fontWeight = FontWeight.SemiBold,
                )
            }
            Text(
                "DayWeave keeps the exact operation encrypted until the server confirms its durable queue receipt or safe expiry permits removal.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onTertiaryContainer,
            )
            if (canRecover || canDiscard) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                ) {
                    if (canRecover) {
                        OutlinedButton(
                            onClick = onRecover,
                            modifier = Modifier.testTag(GOOGLE_OUTBOUND_RECOVER_BUTTON_TAG),
                        ) {
                            Icon(
                                Icons.Outlined.Refresh,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp),
                            )
                            Spacer(Modifier.size(8.dp))
                            Text("Recover")
                        }
                    }
                    if (canDiscard) {
                        TextButton(
                            onClick = onDiscard,
                            modifier = Modifier.testTag(GOOGLE_OUTBOUND_DISCARD_BUTTON_TAG),
                        ) {
                            Text(
                                "Discard expired recovery",
                                color = MaterialTheme.colorScheme.error,
                            )
                        }
                    }
                }
            }
        }
    }
}

/** Only these display-safe fields are allowed to cross from provider JSON into Compose. */
internal data class GoogleCalendarOutboundPreviewPresentation(
    val entityKind: GoogleCalendarOutboundEntityKind,
    val operation: GoogleCalendarOutboundOperation,
    val destination: String,
    val change: String,
    val title: String,
    val description: String?,
    val starts: String? = null,
    val ends: String? = null,
    val due: String? = null,
    val completed: String? = null,
    val status: String? = null,
    val transparency: String? = null,
    val expires: String,
)

internal fun GoogleCalendarOutboundPreviewSnapshot.toSanitizedOutboundPresentation(
    fallbackTitle: String? = null,
):
    GoogleCalendarOutboundPreviewPresentation? = runCatching {
    val payload = providerPayload
    val expiry = Instant.parse(expiresAt)
    val destination = collectionDisplayName.sanitizeDisplayText(MAX_DESTINATION_DISPLAY_CHARS)
    val expires = expiry.atZone(ZoneId.systemDefault()).format(expiryFormatter())
    if (operation == GoogleCalendarOutboundOperation.DELETE) {
        require(payload.isEmpty())
        GoogleCalendarOutboundPreviewPresentation(
            entityKind = entityKind,
            operation = operation,
            destination = destination,
            change = if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
                "Delete existing event"
            } else {
                "Delete existing task"
            },
            title = fallbackTitle?.sanitizeDisplayText(MAX_GENERAL_DISPLAY_CHARS)
                ?.takeIf(String::isNotEmpty)
                ?: if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
                    "Mapped DayWeave event"
                } else {
                    "Mapped DayWeave task"
                },
            description = null,
            expires = expires,
        )
    } else if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
        val title = requireNotNull(payload.displayString("summary"))
        val description = payload.displayString("description", MAX_DESCRIPTION_DISPLAY_CHARS)
        val status = when (payload.requiredDisplayString("status")) {
            "confirmed" -> "Confirmed"
            "tentative" -> "Tentative"
            else -> error("Unsupported Calendar status")
        }
        val transparency = when (payload.requiredDisplayString("transparency")) {
            "opaque" -> "Busy"
            "transparent" -> "Free"
            else -> error("Unsupported Calendar transparency")
        }
        val start = payload.requiredCalendarBoundary("start")
        val end = payload.requiredCalendarBoundary("end")
        require(start.isAllDay == end.isAllDay)
        GoogleCalendarOutboundPreviewPresentation(
            entityKind = entityKind,
            operation = operation,
            destination = destination,
            change = if (providerResourceId == null) {
                "Create new event"
            } else {
                "Update existing event"
            },
            title = title,
            description = description,
            starts = start.displayLabel(),
            ends = end.displayLabel(),
            status = status,
            transparency = transparency,
            expires = expires,
        )
    } else {
        val status = when (payload.requiredDisplayString("status")) {
            "needsAction" -> "Needs action"
            "completed" -> "Completed"
            else -> error("Unsupported Google Task status")
        }
        GoogleCalendarOutboundPreviewPresentation(
            entityKind = entityKind,
            operation = operation,
            destination = destination,
            change = if (providerResourceId == null) {
                "Create new task"
            } else {
                "Update existing task"
            },
            title = payload.requiredDisplayString("title"),
            description = payload.displayString("notes", MAX_DESCRIPTION_DISPLAY_CHARS),
            due = payload.displayInstant("due"),
            completed = payload.displayInstant("completed"),
            status = status,
            expires = expires,
        )
    }
}.getOrNull()

private data class CalendarBoundary(
    val date: LocalDate?,
    val dateTime: OffsetDateTime?,
    val timeZone: ZoneId,
) {
    val isAllDay: Boolean
        get() = date != null

    init {
        require((date == null) != (dateTime == null))
    }

    fun displayLabel(): String = if (date != null) {
        "${date.format(allDayDateFormatter())} · All day · ${timeZone.id}"
    } else {
        "${requireNotNull(dateTime).toInstant().atZone(timeZone).format(eventTimeFormatter())} · " +
            timeZone.id
    }
}

private fun JsonObject.requiredCalendarBoundary(key: String): CalendarBoundary {
    val boundary = this[key] as? JsonObject ?: error("Missing Calendar boundary")
    val date = boundary.displayString("date")?.let(LocalDate::parse)
    val dateTime = boundary.displayString("dateTime")?.let(OffsetDateTime::parse)
    val timeZone = boundary.requiredDisplayString("timeZone")
    return CalendarBoundary(
        date = date,
        dateTime = dateTime,
        timeZone = ZoneId.of(timeZone),
    )
}

private fun JsonObject.requiredDisplayString(key: String): String =
    requireNotNull(displayString(key))

private fun JsonObject.displayString(
    key: String,
    maximumCharacters: Int = MAX_GENERAL_DISPLAY_CHARS,
): String? = (this[key] as? JsonPrimitive)
    ?.takeIf(JsonPrimitive::isString)
    ?.content
    ?.sanitizeDisplayText(maximumCharacters)

private fun JsonObject.displayInstant(key: String): String? {
    val raw = displayString(key) ?: return null
    return Instant.parse(raw)
        .atZone(ZoneId.systemDefault())
        .format(expiryFormatter())
}

private fun String.sanitizeDisplayText(maximumCharacters: Int): String {
    val normalized = buildString(length.coerceAtMost(maximumCharacters)) {
        this@sanitizeDisplayText.forEach { character ->
            when {
                length >= maximumCharacters -> return@forEach
                character == '\n' || character == '\t' -> append(character)
                !character.isISOControl() -> append(character)
            }
        }
    }
    return if (length > maximumCharacters && normalized.isNotEmpty()) "$normalized…" else normalized
}

internal fun googleCalendarOutboundDialogProperties(isBusy: Boolean) = DialogProperties(
    dismissOnBackPress = !isBusy,
    dismissOnClickOutside = !isBusy,
    securePolicy = SecureFlagPolicy.SecureOn,
    usePlatformDefaultWidth = false,
)

internal const val GOOGLE_OUTBOUND_REVIEW_SHEET_TAG = "google_outbound_review_sheet"
internal const val GOOGLE_OUTBOUND_DESTINATION_PICKER_TAG = "google_outbound_destination_picker"
internal const val GOOGLE_OUTBOUND_PREVIEW_BUTTON_TAG = "google_outbound_preview"
internal const val GOOGLE_OUTBOUND_APPROVE_BUTTON_TAG = "google_outbound_approve"
internal const val GOOGLE_OUTBOUND_RECOVERY_CARD_TAG = "google_outbound_recovery"
internal const val GOOGLE_OUTBOUND_RECOVER_BUTTON_TAG = "google_outbound_recover"
internal const val GOOGLE_OUTBOUND_DISCARD_BUTTON_TAG = "google_outbound_discard"
internal const val GOOGLE_OUTBOUND_DESTINATION_TAG = "google_outbound_preview_destination"
internal const val GOOGLE_OUTBOUND_CHANGE_TAG = "google_outbound_preview_change"
internal const val GOOGLE_OUTBOUND_TITLE_TAG = "google_outbound_preview_title"
internal const val GOOGLE_OUTBOUND_DESCRIPTION_TAG = "google_outbound_preview_description"
internal const val GOOGLE_OUTBOUND_START_TAG = "google_outbound_preview_start"
internal const val GOOGLE_OUTBOUND_END_TAG = "google_outbound_preview_end"
internal const val GOOGLE_OUTBOUND_STATUS_TAG = "google_outbound_preview_status"
internal const val GOOGLE_OUTBOUND_TRANSPARENCY_TAG = "google_outbound_preview_transparency"
internal const val GOOGLE_OUTBOUND_DUE_TAG = "google_outbound_preview_due"
internal const val GOOGLE_OUTBOUND_COMPLETED_TAG = "google_outbound_preview_completed"
internal const val GOOGLE_OUTBOUND_EXPIRY_TAG = "google_outbound_preview_expiry"

private const val MAX_GENERAL_DISPLAY_CHARS = 1_024
// These bounds cover every value accepted by the outbound model, so approval never sees a
// truncated title, description, or destination while the full provider payload stays private.
private const val MAX_DESCRIPTION_DISPLAY_CHARS = 256 * 1_024
private const val MAX_DESTINATION_DISPLAY_CHARS = 4_420
private fun allDayDateFormatter(): DateTimeFormatter =
    DateTimeFormatter.ofPattern("EEE, MMM d, uuuu", Locale.getDefault())

private fun eventTimeFormatter(): DateTimeFormatter =
    DateTimeFormatter.ofPattern("EEE, MMM d · HH:mm", Locale.getDefault())

private fun expiryFormatter(): DateTimeFormatter =
    DateTimeFormatter.ofPattern("MMM d, yyyy · HH:mm z", Locale.getDefault())
