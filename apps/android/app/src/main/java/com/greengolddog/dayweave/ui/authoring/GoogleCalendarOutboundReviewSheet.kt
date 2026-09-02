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
import androidx.compose.material.icons.outlined.Close
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
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundApprovalConfirmation
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundPhase
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundState
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundTargetOption
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * Secure, exact review surface for the first Google Calendar outbound slice.
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
    val previewPresentation = remember(state.preview, reviewDestinationDisplayName) {
        state.preview?.toSanitizedOutboundPresentation()?.let { presentation ->
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
                Icons.Outlined.CalendarMonth,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.padding(10.dp),
            )
        }
        Column(modifier = Modifier.weight(1f)) {
            Text(
                "Publish to Google Calendar",
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                "One private, attendee-free event",
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        IconButton(
            onClick = onDismissRequest,
            enabled = !isBusy,
        ) {
            Icon(Icons.Outlined.Close, contentDescription = "Close Google Calendar review")
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
                "No writable owner or writer calendar is available. Enable publishing in Google settings first.",
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
                            selectedTarget?.displayName ?: "Choose a calendar",
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
                        "Choose the exact calendar before generating a server preview.",
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
            ReviewBadge("Private")
            ReviewBadge("No guests")
            ReviewBadge("No conferencing")
        }

        ReviewSection("Publication") {
            ReviewField("Destination", preview.destination, GOOGLE_OUTBOUND_DESTINATION_TAG)
            ReviewField("Change", preview.change, GOOGLE_OUTBOUND_CHANGE_TAG)
            ReviewField("Expires", preview.expires, GOOGLE_OUTBOUND_EXPIRY_TAG)
        }

        ReviewSection("Event") {
            ReviewField("Title", preview.title, GOOGLE_OUTBOUND_TITLE_TAG)
            ReviewField(
                "Description",
                preview.description ?: "No description",
                GOOGLE_OUTBOUND_DESCRIPTION_TAG,
                subdued = preview.description == null,
            )
            ReviewField("Starts", preview.starts, GOOGLE_OUTBOUND_START_TAG)
            ReviewField("Ends", preview.ends, GOOGLE_OUTBOUND_END_TAG)
            ReviewField("Status", preview.status, GOOGLE_OUTBOUND_STATUS_TAG)
            ReviewField(
                "Availability",
                preview.transparency,
                GOOGLE_OUTBOUND_TRANSPARENCY_TAG,
            )
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
            "This server preview is not a timed private event and cannot be approved here.",
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
    val destination: String,
    val change: String,
    val title: String,
    val description: String?,
    val starts: String,
    val ends: String,
    val status: String,
    val transparency: String,
    val expires: String,
)

internal fun GoogleCalendarOutboundPreviewSnapshot.toSanitizedOutboundPresentation():
    GoogleCalendarOutboundPreviewPresentation? = runCatching {
    val payload = providerPayload
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
    val start = payload.requiredTimedBoundary("start")
    val end = payload.requiredTimedBoundary("end")
    val expiry = Instant.parse(expiresAt)
    GoogleCalendarOutboundPreviewPresentation(
        destination = collectionDisplayName.sanitizeDisplayText(MAX_DESTINATION_DISPLAY_CHARS),
        change = if (providerResourceId == null) "Create new event" else "Update existing event",
        title = title,
        description = description,
        starts = start.displayLabel(),
        ends = end.displayLabel(),
        status = status,
        transparency = transparency,
        expires = expiry.atZone(ZoneId.systemDefault()).format(expiryFormatter()),
    )
}.getOrNull()

private data class TimedBoundary(
    val dateTime: OffsetDateTime,
    val timeZone: ZoneId,
) {
    fun displayLabel(): String =
        "${dateTime.toInstant().atZone(timeZone).format(eventTimeFormatter())} · ${timeZone.id}"
}

private fun JsonObject.requiredTimedBoundary(key: String): TimedBoundary {
    val boundary = this[key] as? JsonObject ?: error("Missing Calendar boundary")
    val dateTime = boundary.requiredDisplayString("dateTime")
    val timeZone = boundary.requiredDisplayString("timeZone")
    return TimedBoundary(
        dateTime = OffsetDateTime.parse(dateTime),
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
internal const val GOOGLE_OUTBOUND_EXPIRY_TAG = "google_outbound_preview_expiry"

private const val MAX_GENERAL_DISPLAY_CHARS = 1_024
// These bounds cover every value accepted by the outbound model, so approval never sees a
// truncated title, description, or destination while the full provider payload stays private.
private const val MAX_DESCRIPTION_DISPLAY_CHARS = 256 * 1_024
private const val MAX_DESTINATION_DISPLAY_CHARS = 4_420
private fun eventTimeFormatter(): DateTimeFormatter =
    DateTimeFormatter.ofPattern("EEE, MMM d · HH:mm", Locale.getDefault())

private fun expiryFormatter(): DateTimeFormatter =
    DateTimeFormatter.ofPattern("MMM d, yyyy · HH:mm z", Locale.getDefault())
