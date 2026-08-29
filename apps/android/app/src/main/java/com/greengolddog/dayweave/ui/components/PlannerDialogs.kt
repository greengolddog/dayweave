package com.greengolddog.dayweave.ui.components

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AddTask
import androidx.compose.material.icons.outlined.Coffee
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.network.DeviceAuthPhase
import com.greengolddog.dayweave.network.DeviceAuthUiState
import com.greengolddog.dayweave.network.RemoteProposalCanonicalItem
import com.greengolddog.dayweave.network.RemoteProposalItemField
import com.greengolddog.dayweave.sync.ProposalApplicationApproval
import com.greengolddog.dayweave.sync.ProposalApplicationState
import java.time.Instant
import kotlinx.coroutines.delay
import kotlinx.serialization.json.JsonPrimitive

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QuickCaptureSheet(
    onDismiss: () -> Unit,
    onCapture: (String, ItemKind, Boolean) -> Boolean,
) {
    var title by remember { mutableStateOf("") }
    var kind by remember { mutableStateOf(ItemKind.TASK) }
    var isSensitive by remember { mutableStateOf(false) }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Icon(Icons.Outlined.AddTask, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                Column {
                    Text("Quick capture", style = MaterialTheme.typography.titleLarge)
                    Text(
                        "Save now; clarify duration and constraints in Inbox.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
            OutlinedTextField(
                value = title,
                onValueChange = { title = it },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("What do you need to do?") },
                minLines = 2,
            )
            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                ItemKind.entries.forEach { option ->
                    FilterChip(
                        selected = option == kind,
                        onClick = { kind = option },
                        label = { Text(option.label) },
                    )
                }
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text("Sensitive", style = MaterialTheme.typography.titleSmall)
                    Text(
                        "Keep this draft classified for privacy controls while you clarify it.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = isSensitive,
                    onCheckedChange = { isSensitive = it },
                    modifier = Modifier.testTag("quick_capture_sensitive_toggle"),
                )
            }
            Button(
                onClick = { if (onCapture(title, kind, isSensitive)) onDismiss() },
                enabled = title.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Add to Inbox")
            }
        }
    }
}

@Composable
fun PauseChooserDialog(
    onDismiss: () -> Unit,
    onPause: (Int?) -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(Icons.Outlined.Coffee, contentDescription = null) },
        title = { Text("Take a break") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(9.dp)) {
                Text("Choose a duration, or pause without an end time. DayWeave will hold later work tentatively.")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(5, 15, 30).forEach { minutes ->
                        OutlinedButton(onClick = { onPause(minutes) }) { Text("${minutes}m") }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onPause(null) }) { Text("No end time") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
fun BreakEndedDialog(
    onResume: () -> Unit,
    onExtend: () -> Unit,
    onKeepPaused: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onKeepPaused,
        icon = { Icon(Icons.Outlined.Coffee, contentDescription = null) },
        title = { Text("Your break is over") },
        text = {
            Text(
                "Resume the paused item, extend the break by 10 minutes, or close this message and keep it paused.",
            )
        },
        confirmButton = {
            TextButton(onClick = onResume) { Text("Resume") }
        },
        dismissButton = {
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                TextButton(onClick = onExtend) { Text("Extend 10m") }
                TextButton(onClick = onKeepPaused) { Text("Keep paused") }
            }
        },
    )
}

@Composable
fun EditSuggestionDialog(
    suggestion: PlanningSuggestion,
    onDismiss: () -> Unit,
    onSave: (String, String) -> Unit,
) {
    var title by remember(suggestion.id) { mutableStateOf(suggestion.title) }
    var summary by remember(suggestion.id) { mutableStateOf(suggestion.summary) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Edit proposal") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    "Editing still does not apply the proposal to your schedule.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = title,
                    onValueChange = { title = it },
                    label = { Text("Title") },
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = summary,
                    onValueChange = { summary = it },
                    label = { Text("Proposed change") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onSave(title, summary) },
                enabled = title.isNotBlank() && summary.isNotBlank(),
            ) { Text("Save draft") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
fun ProposalReviewDialog(
    proposalTitle: String,
    state: ProposalApplicationState,
    onDismiss: () -> Unit,
    onRegenerate: () -> Unit,
    onApply: (ProposalApplicationApproval) -> Unit,
) {
    val preview = state.preview ?: return
    val approval = state.exactApproval ?: return
    var confirmed by remember(approval) { mutableStateOf(false) }
    var revealSensitive by remember(approval) { mutableStateOf(false) }
    var reviewExpired by remember(approval) {
        mutableStateOf(!Instant.parse(preview.expiresAt).isAfter(Instant.now()))
    }
    val containsSensitiveValues = preview.diffs.any { diff ->
        diff.before?.isSensitive == true || diff.after?.isSensitive == true
    } || preview.implicitDiffs.any { diff ->
        diff.before.isSensitive || diff.after.isSensitive
    }
    LaunchedEffect(approval, preview.expiresAt) {
        val waitMillis = Instant.parse(preview.expiresAt).toEpochMilli() -
            System.currentTimeMillis()
        if (waitMillis > 0) delay(waitMillis)
        reviewExpired = true
        confirmed = false
    }
    AlertDialog(
        onDismissRequest = { if (!state.isBusy) onDismiss() },
        title = { Text("Review exact changes") },
        text = {
            Column(
                modifier = Modifier
                    .heightIn(max = 560.dp)
                    .verticalScroll(rememberScrollState())
                    .testTag("proposal_review_content"),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(proposalTitle, style = MaterialTheme.typography.titleMedium)
                Text(
                    state.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (preview.canApply) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.error
                    },
                )
                Text(
                    "Risk: ${preview.maximumRisk.displayLabel()} · " +
                        "${preview.commandIds.size} atomic command(s) · expires ${preview.expiresAt}",
                    style = MaterialTheme.typography.labelMedium,
                )

                if (reviewExpired) {
                    Text(
                        "This exact review expired. Regenerate it before approval.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }

                if (containsSensitiveValues) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                    ) {
                        Checkbox(
                            checked = revealSensitive,
                            onCheckedChange = { revealSensitive = it },
                            enabled = !state.isBusy,
                            modifier = Modifier.testTag("proposal_reveal_sensitive_values"),
                        )
                        Text(
                            if (revealSensitive) {
                                "Sensitive before/after values are visible until this review closes."
                            } else {
                                "Sensitive before/after values are concealed. Reveal for this review only."
                            },
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }

                ReviewSection("Direct changes") {
                    preview.diffs.forEachIndexed { index, diff ->
                        val concealIdentity = !revealSensitive &&
                            (diff.before?.isSensitive == true || diff.after?.isSensitive == true)
                        Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                            Text(
                                "${index + 1}. ${diff.operation.displayLabel()} · item " +
                                    if (concealIdentity) "Concealed" else diff.itemId,
                                style = MaterialTheme.typography.titleSmall,
                            )
                            ExactItemIdentitySnapshot(
                                before = diff.before,
                                after = diff.after,
                                revealSensitive = revealSensitive,
                            )
                            ExactChangedFieldValues(
                                fields = diff.changedFields,
                                before = diff.before,
                                after = diff.after,
                                revealSensitive = revealSensitive,
                            )
                        }
                    }
                }

                if (preview.implicitDiffs.isNotEmpty()) {
                    ReviewSection("Hierarchy side effects") {
                        preview.implicitDiffs.forEach { diff ->
                            val concealIdentity = !revealSensitive &&
                                (diff.before.isSensitive || diff.after.isSensitive)
                            Text(
                                "${diff.reason.displayLabel()} · item " +
                                    if (concealIdentity) "Concealed" else diff.itemId,
                                style = MaterialTheme.typography.titleSmall,
                            )
                            ExactItemIdentitySnapshot(
                                before = diff.before,
                                after = diff.after,
                                revealSensitive = revealSensitive,
                            )
                            ExactChangedFieldValues(
                                fields = diff.changedFields,
                                before = diff.before,
                                after = diff.after,
                                revealSensitive = revealSensitive,
                            )
                        }
                    }
                }

                if (preview.risks.isNotEmpty()) {
                    ReviewSection("Risks") {
                        preview.risks.forEach { risk ->
                            Text(
                                "${risk.level.displayLabel()}: ${risk.summary}" +
                                    if (risk.requiresExplicitApproval) " · approval required" else "",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                }

                if (preview.conflicts.isNotEmpty()) {
                    ReviewSection("Conflicts") {
                        preview.conflicts.forEach { conflict ->
                            Text(
                                "${conflict.code.displayLabel()}: ${conflict.summary}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                            )
                        }
                    }
                }

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                ) {
                    Checkbox(
                        checked = confirmed,
                        onCheckedChange = { confirmed = it },
                        enabled = preview.canApply && !reviewExpired && !state.isBusy,
                        modifier = Modifier.testTag("proposal_explicit_approval"),
                    )
                    Text(
                        "I approve this exact review as one atomic change set.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onApply(approval) },
                enabled = confirmed && preview.canApply && preview.conflicts.isEmpty() &&
                    !reviewExpired && !state.isBusy,
                modifier = Modifier.testTag("proposal_apply_exact_review"),
            ) {
                Text(if (state.isBusy) "Applying…" else "Apply exact changes")
            }
        },
        dismissButton = {
            Row {
                TextButton(onClick = onRegenerate, enabled = !state.isBusy) {
                    Text("Regenerate review")
                }
                TextButton(onClick = onDismiss, enabled = !state.isBusy) { Text("Cancel") }
            }
        },
    )
}

@Composable
private fun ReviewSection(
    title: String,
    content: @Composable () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(title, style = MaterialTheme.typography.titleSmall)
        content()
    }
}

@Composable
private fun ExactItemIdentitySnapshot(
    before: RemoteProposalCanonicalItem?,
    after: RemoteProposalCanonicalItem?,
    revealSensitive: Boolean,
) {
    val concealTransition = !revealSensitive &&
        (before?.isSensitive == true || after?.isSensitive == true)
    SelectionContainer {
        Column(verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(
                "Identity before: ${proposalReviewIdentitySnapshot(before, concealTransition)}",
                style = MaterialTheme.typography.bodySmall,
            )
            Text(
                "Identity after: ${proposalReviewIdentitySnapshot(after, concealTransition)}",
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

@Composable
private fun ExactChangedFieldValues(
    fields: List<RemoteProposalItemField>,
    before: RemoteProposalCanonicalItem?,
    after: RemoteProposalCanonicalItem?,
    revealSensitive: Boolean,
) {
    val concealTransition = !revealSensitive &&
        (before?.isSensitive == true || after?.isSensitive == true)
    fields.forEach { field ->
        Column(
            modifier = Modifier.testTag("proposal_field_${field.name.lowercase()}"),
            verticalArrangement = Arrangement.spacedBy(1.dp),
        ) {
            Text(field.displayLabel(), style = MaterialTheme.typography.labelMedium)
            SelectionContainer {
                Text(
                    "Before: ${proposalReviewFieldValue(before, field, concealTransition)}",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            SelectionContainer {
                Text(
                    "After: ${proposalReviewFieldValue(after, field, concealTransition)}",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

internal fun proposalReviewIdentitySnapshot(
    item: RemoteProposalCanonicalItem?,
    concealSensitive: Boolean,
): String {
    if (item == null) return "Not present"
    val id = if (concealSensitive) "Concealed" else item.id
    val title = if (concealSensitive) "Concealed" else JsonPrimitive(item.title).toString()
    val kind = if (concealSensitive) "Concealed" else JsonPrimitive(item.kind.name.lowercase()).toString()
    val status = if (concealSensitive) {
        "Concealed"
    } else {
        JsonPrimitive(item.status.name.lowercase()).toString()
    }
    return "id=$id · title=$title · kind=$kind · status=$status"
}

internal fun proposalReviewFieldValue(
    item: RemoteProposalCanonicalItem?,
    field: RemoteProposalItemField,
    concealSensitive: Boolean,
): String {
    if (item == null) return "Not present"
    if (concealSensitive && field != RemoteProposalItemField.IS_SENSITIVE) return "Concealed"
    fun quoted(value: String): String = JsonPrimitive(value).toString()
    fun quotedOrNull(value: String?): String = value?.let(::quoted) ?: "null"
    return when (field) {
        RemoteProposalItemField.IS_SENSITIVE -> item.isSensitive.toString()
        RemoteProposalItemField.KIND -> quoted(item.kind.name.lowercase())
        RemoteProposalItemField.STATUS -> quoted(item.status.name.lowercase())
        RemoteProposalItemField.TITLE -> quoted(item.title)
        RemoteProposalItemField.NOTES -> quotedOrNull(item.notes)
        RemoteProposalItemField.TIMEZONE_NAME -> quoted(item.timezoneName)
        RemoteProposalItemField.DURATION_SECONDS -> item.durationSeconds?.toString() ?: "null"
        RemoteProposalItemField.DEADLINE_AT -> quotedOrNull(item.deadlineAt)
        RemoteProposalItemField.EARLIEST_START_AT -> quotedOrNull(item.earliestStartAt)
        RemoteProposalItemField.RECURRENCE -> item.recurrence?.toString() ?: "null"
        RemoteProposalItemField.FLEXIBLE_CONSTRAINTS -> item.flexibleConstraints.toString()
        RemoteProposalItemField.SPLIT_POLICY -> item.splitPolicy.toString()
        RemoteProposalItemField.IMPORTANCE -> item.importance.toString()
        RemoteProposalItemField.URGENCY -> item.urgency.toString()
        RemoteProposalItemField.PARENT_ID -> quotedOrNull(item.parentId)
        RemoteProposalItemField.SIBLING_ORDER -> item.siblingOrder.toString()
        RemoteProposalItemField.IS_EXECUTABLE -> item.isExecutable.toString()
        RemoteProposalItemField.REVISION -> item.revision.toString()
        RemoteProposalItemField.COMPLETED_AT -> quotedOrNull(item.completedAt)
        RemoteProposalItemField.DELETED_AT -> quotedOrNull(item.deletedAt)
    }
}

private fun Enum<*>.displayLabel(): String =
    name.lowercase().replace('_', ' ').replaceFirstChar(Char::uppercase)

@Composable
fun ApiConnectionDialog(
    authState: DeviceAuthUiState,
    credentialReplacementBlocked: Boolean,
    onDismiss: () -> Unit,
    onUpgradeWithBootstrap: (baseUrl: String, bootstrapToken: String) -> Unit,
    onConsumeEnrollmentCode: (baseUrl: String, enrollmentCode: String) -> Unit,
    onRetryPending: () -> Unit,
    onRevokeAndSignOut: () -> Unit,
    onDestroyLocalOnly: () -> Unit,
) {
    var baseUrl by remember(authState.baseUrl) { mutableStateOf(authState.baseUrl.orEmpty()) }
    var secret by remember { mutableStateOf("") }
    var entryMode by remember { mutableStateOf(DeviceAuthEntryMode.ONE_TIME_CODE) }
    var confirmSignOut by remember { mutableStateOf(false) }
    var confirmLocalOnly by remember { mutableStateOf(false) }

    if (confirmSignOut) {
        AlertDialog(
            onDismissRequest = { confirmSignOut = false },
            title = { Text("Revoke this device session?") },
            text = {
                Text(
                    "DayWeave requires the server to confirm revocation with an empty 204 response. Local credentials and API-bound cache are removed only after that succeeds; any failure keeps local state for retry.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        secret = ""
                        confirmSignOut = false
                        onRevokeAndSignOut()
                    },
                    enabled = !authState.isBusy,
                ) { Text("Revoke & sign out") }
            },
            dismissButton = {
                TextButton(onClick = { confirmSignOut = false }) { Text("Keep session") }
            },
        )
        return
    }

    if (confirmLocalOnly) {
        AlertDialog(
            onDismissRequest = { confirmLocalOnly = false },
            title = { Text("Remove only local authentication?") },
            text = {
                Text(
                    "This cannot confirm server revocation. A device session and reviewed bootstrap authority may remain active on the server. DayWeave will quarantine API-bound cache and destroy this device’s encrypted envelope and wrapping key.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        secret = ""
                        confirmLocalOnly = false
                        onDestroyLocalOnly()
                    },
                    enabled = !authState.isBusy,
                ) { Text("Remove local state only") }
            },
            dismissButton = {
                TextButton(onClick = { confirmLocalOnly = false }) { Text("Cancel") }
            },
        )
        return
    }

    val acceptsNewEnrollment = authState.phase in setOf(
        DeviceAuthPhase.UNCONFIGURED,
        DeviceAuthPhase.LEGACY,
        DeviceAuthPhase.REAUTH,
    )
    val exactRetryAvailable = authState.phase in setOf(
        DeviceAuthPhase.ENROLLMENT_CREATION_PENDING,
        DeviceAuthPhase.ENROLLMENT_PENDING,
        DeviceAuthPhase.REFRESH_PENDING,
    )
    val activeSession = authState.phase == DeviceAuthPhase.ACTIVE
    val bindingChangeBlocked = credentialReplacementBlocked || authState.isBusy

    AlertDialog(
        onDismissRequest = {
            secret = ""
            onDismiss()
        },
        title = { Text("Durable device authentication") },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    authState.message,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (credentialReplacementBlocked) {
                    Text(
                        "Recover the exact schedule publication, proposal application, or " +
                            "canonical/execution action before enrollment or sign-out. " +
                            "Confirmed local-only removal remains " +
                            "available and will quarantine that recovery journal first.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }

                authState.baseUrl?.let { endpoint ->
                    Text("Endpoint", style = MaterialTheme.typography.labelMedium)
                    SelectionContainer { Text(endpoint, style = MaterialTheme.typography.bodySmall) }
                }
                authState.clientInstanceId?.let { clientId ->
                    Text("This Android client ID", style = MaterialTheme.typography.labelMedium)
                    SelectionContainer { Text(clientId, style = MaterialTheme.typography.bodySmall) }
                }
                authState.sessionId?.let { sessionId ->
                    Text("Session", style = MaterialTheme.typography.labelMedium)
                    SelectionContainer { Text(sessionId, style = MaterialTheme.typography.bodySmall) }
                }
                authState.accessExpiresAt?.let { expiry ->
                    Text("Current access expires $expiry", style = MaterialTheme.typography.bodySmall)
                }

                if (acceptsNewEnrollment) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilterChip(
                            selected = entryMode == DeviceAuthEntryMode.ONE_TIME_CODE,
                            onClick = {
                                secret = ""
                                entryMode = DeviceAuthEntryMode.ONE_TIME_CODE
                            },
                            label = { Text("One-time code") },
                        )
                        FilterChip(
                            selected = entryMode == DeviceAuthEntryMode.HYBRID_BOOTSTRAP,
                            onClick = {
                                secret = ""
                                entryMode = DeviceAuthEntryMode.HYBRID_BOOTSTRAP
                            },
                            label = { Text("Hybrid bootstrap") },
                        )
                    }
                    Text(
                        if (entryMode == DeviceAuthEntryMode.ONE_TIME_CODE) {
                            "Mint the dw_en1_ code for the exact client ID shown above on an already authorized device. The code and proposed session credential tuple are journaled before the first consume request."
                        } else {
                            "Use only the reviewed migration bootstrap. It authorizes enrollment creation and is never reused as an ordinary API credential after durable activation."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedTextField(
                        value = baseUrl,
                        onValueChange = { baseUrl = it },
                        modifier = Modifier.fillMaxWidth(),
                        label = { Text("HTTPS API base URL") },
                        placeholder = { Text("https://api.example.com/") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(
                            autoCorrectEnabled = false,
                            keyboardType = KeyboardType.Uri,
                        ),
                    )
                    OutlinedTextField(
                        value = secret,
                        onValueChange = { secret = it },
                        modifier = Modifier.fillMaxWidth(),
                        label = {
                            Text(
                                if (entryMode == DeviceAuthEntryMode.ONE_TIME_CODE) {
                                    "One-time dw_en1_ code"
                                } else {
                                    "Reviewed bootstrap credential"
                                },
                            )
                        },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(
                            autoCorrectEnabled = false,
                            keyboardType = KeyboardType.Password,
                        ),
                    )
                }

                if (authState.phase == DeviceAuthPhase.INCOMPATIBLE) {
                    Text(
                        "Fail-closed storage cannot be used or replaced in place. Update DayWeave first; local-only destruction is the explicit recovery of last resort.",
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        },
        confirmButton = {
            when {
                activeSession -> TextButton(
                    onClick = { confirmSignOut = true },
                    enabled = !bindingChangeBlocked,
                ) { Text("Revoke & sign out") }
                exactRetryAvailable -> TextButton(
                    onClick = onRetryPending,
                    enabled = !authState.isBusy &&
                        (
                            authState.phase == DeviceAuthPhase.REFRESH_PENDING ||
                                !credentialReplacementBlocked
                            ),
                ) { Text("Retry exact state") }
                acceptsNewEnrollment -> TextButton(
                    onClick = {
                        val submittedSecret = secret
                        secret = ""
                        if (entryMode == DeviceAuthEntryMode.ONE_TIME_CODE) {
                            onConsumeEnrollmentCode(baseUrl, submittedSecret)
                        } else {
                            onUpgradeWithBootstrap(baseUrl, submittedSecret)
                        }
                    },
                    enabled = !bindingChangeBlocked &&
                        baseUrl.trim().startsWith("https://", ignoreCase = true) &&
                        secret.isNotBlank(),
                ) {
                    Text(
                        if (entryMode == DeviceAuthEntryMode.ONE_TIME_CODE) {
                            "Consume code"
                        } else {
                            "Create enrollment"
                        },
                    )
                }
            }
        },
        dismissButton = {
            Row {
                if (authState.phase == DeviceAuthPhase.LEGACY) {
                    TextButton(
                        onClick = onRetryPending,
                        enabled = !bindingChangeBlocked,
                    ) { Text("Retry stored upgrade") }
                }
                if (authState.phase != DeviceAuthPhase.UNCONFIGURED) {
                    TextButton(
                        onClick = { confirmLocalOnly = true },
                        enabled = !authState.isBusy,
                    ) { Text("Local-only removal") }
                }
                TextButton(
                    onClick = {
                        secret = ""
                        onDismiss()
                    },
                ) { Text("Cancel") }
            }
        },
    )
}

private enum class DeviceAuthEntryMode {
    ONE_TIME_CODE,
    HYBRID_BOOTSTRAP,
}
