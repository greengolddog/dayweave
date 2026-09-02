package com.greengolddog.dayweave.ui.components

import android.app.DatePickerDialog
import android.app.TimePickerDialog
import android.text.format.DateFormat
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
import androidx.compose.material.icons.outlined.Schedule
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import com.greengolddog.dayweave.model.ExecutionDeferAssessmentSnapshot
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.MoveLaterAssessment
import com.greengolddog.dayweave.model.MoveLaterApprovalEnvelope
import com.greengolddog.dayweave.model.MoveLaterPlacementMode
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.ScheduleDisplayHorizon
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.toApprovalEnvelope
import com.greengolddog.dayweave.network.DeviceAuthPhase
import com.greengolddog.dayweave.network.DeviceAuthUiState
import com.greengolddog.dayweave.network.RemoteProposalCanonicalItem
import com.greengolddog.dayweave.network.RemoteProposalItemField
import com.greengolddog.dayweave.sync.ProposalApplicationApproval
import com.greengolddog.dayweave.sync.ProposalApplicationState
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneId
import java.time.ZonedDateTime
import java.time.format.DateTimeFormatter
import java.time.temporal.ChronoUnit
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QuickCaptureSheet(
    onDismiss: () -> Unit,
    onCapture: suspend (String, ItemKind, Boolean) -> Boolean,
    onContinueWithDetails: (String, ItemKind, Boolean) -> Unit,
) {
    var title by remember { mutableStateOf("") }
    var kind by remember { mutableStateOf(ItemKind.TASK) }
    var isSensitive by remember { mutableStateOf(false) }
    var isSaving by remember { mutableStateOf(false) }
    var saveError by remember { mutableStateOf<String?>(null) }
    val coroutineScope = rememberCoroutineScope()
    val requiresDetails = kind == ItemKind.HABIT || kind == ItemKind.EVENT

    ModalBottomSheet(onDismissRequest = { if (!isSaving) onDismiss() }) {
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
                        modifier = Modifier.testTag(
                            "quick_capture_kind_${option.name.lowercase()}",
                        ),
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
            if (requiresDetails) {
                Text(
                    if (kind == ItemKind.HABIT) {
                        "Habits need an explicit recurrence before they can be queued."
                    } else {
                        "Events need exact start and end instants; DayWeave will not invent them."
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            saveError?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag("quick_capture_diagnostic"),
                )
            }
            Button(
                onClick = {
                    if (requiresDetails) {
                        onContinueWithDetails(title, kind, isSensitive)
                        onDismiss()
                    } else {
                        isSaving = true
                        saveError = null
                        coroutineScope.launch {
                            if (onCapture(title, kind, isSensitive)) {
                                onDismiss()
                            } else {
                                saveError = "The capture could not be saved to the encrypted journal."
                            }
                            isSaving = false
                        }
                    }
                },
                enabled = title.isNotBlank() && !isSaving,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(
                    when {
                        isSaving -> "Saving…"
                        requiresDetails -> "Continue to details"
                        else -> "Add to Inbox"
                    },
                )
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

internal data class MoveLaterPreset(
    val label: String,
    val detail: String,
    val moveStart: Instant,
)

internal data class MoveLaterDateBounds(
    val firstDate: LocalDate,
    val lastDateInclusive: LocalDate,
) {
    init {
        require(!lastDateInclusive.isBefore(firstDate))
    }

    operator fun contains(date: LocalDate): Boolean =
        !date.isBefore(firstDate) && !date.isAfter(lastDateInclusive)
}

internal fun moveLaterDateBounds(
    horizon: ScheduleDisplayHorizon,
    zoneId: ZoneId,
): MoveLaterDateBounds? {
    if (horizon.timezone != zoneId || horizon.start >= horizon.end) return null
    val firstDate = horizon.start.atZone(zoneId).toLocalDate()
    val lastDate = runCatching { horizon.end.minusNanos(1) }.getOrNull()
        ?.atZone(zoneId)
        ?.toLocalDate()
        ?: return null
    return runCatching { MoveLaterDateBounds(firstDate, lastDate) }.getOrNull()
}

internal fun moveLaterPickerDateBounds(
    planningHorizon: ScheduleDisplayHorizon?,
    moveAnchor: Instant,
    zoneId: ZoneId,
    serverAuthoritativeExecution: Boolean,
): MoveLaterDateBounds? {
    val anchorDate = moveAnchor.atZone(zoneId).toLocalDate()
    if (serverAuthoritativeExecution) {
        return MoveLaterDateBounds(anchorDate, anchorDate.plusDays(6))
    }
    val horizonBounds = planningHorizon?.let { moveLaterDateBounds(it, zoneId) } ?: return null
    val firstSelectable = maxOf(horizonBounds.firstDate, anchorDate)
    return if (firstSelectable <= horizonBounds.lastDateInclusive) {
        MoveLaterDateBounds(firstSelectable, horizonBounds.lastDateInclusive)
    } else {
        null
    }
}

internal data class MoveLaterConflictPresentation(
    val labels: List<String>,
    val requiresSecureWindow: Boolean,
)

internal fun moveLaterConflictPresentation(
    blocks: List<ScheduleItem>,
): MoveLaterConflictPresentation = MoveLaterConflictPresentation(
    labels = blocks.map { block ->
        if (block.isSensitive) "Sensitive busy time" else block.title
    }.distinct().take(3),
    requiresSecureWindow = blocks.any(ScheduleItem::isSensitive),
)

internal fun moveLaterRequiresSecureWindow(
    itemIsSensitive: Boolean,
    conflicts: MoveLaterConflictPresentation,
): Boolean = itemIsSensitive || conflicts.requiresSecureWindow

internal fun moveLaterPresets(
    now: Instant,
    zoneId: ZoneId,
    use24HourFormat: Boolean,
    allowedDateBounds: MoveLaterDateBounds? = null,
    serverAuthoritativeExecution: Boolean = false,
): List<MoveLaterPreset> {
    fun afterHours(hours: Long): Instant {
        val candidate = now.plus(hours, ChronoUnit.HOURS)
        if (serverAuthoritativeExecution) return roundUpToExecutionDeferSlot(candidate)
        val minute = candidate.atZone(zoneId).truncatedTo(ChronoUnit.MINUTES)
        return (if (minute.toInstant() < candidate) minute.plusMinutes(1) else minute).toInstant()
    }
    val tomorrowMorning = ZonedDateTime.of(
        now.atZone(zoneId).toLocalDate().plusDays(1),
        LocalTime.of(9, 0),
        zoneId,
    ).toInstant()
    val formatter = DateTimeFormatter.ofPattern(
        if (use24HourFormat) "EEE, MMM d · HH:mm z" else "EEE, MMM d · h:mm a z",
    )
    fun detail(instant: Instant) = instant.atZone(zoneId).format(formatter)
    val inOneHour = afterHours(1)
    val inThreeHours = afterHours(3)
    return listOf(
        MoveLaterPreset("In 1 hour", detail(inOneHour), inOneHour),
        MoveLaterPreset("In 3 hours", detail(inThreeHours), inThreeHours),
        MoveLaterPreset("Tomorrow morning", detail(tomorrowMorning), tomorrowMorning),
    ).filter { preset ->
        allowedDateBounds == null || preset.moveStart.atZone(zoneId).toLocalDate() in allowedDateBounds
    }
}

internal fun customMoveStart(
    date: LocalDate,
    time: LocalTime,
    zoneId: ZoneId,
    now: Instant,
    serverAuthoritativeExecution: Boolean = false,
): Instant? {
    val localDateTime = LocalDateTime.of(date, time.truncatedTo(ChronoUnit.MINUTES))
    val offset = zoneId.rules.getValidOffsets(localDateTime).singleOrNull() ?: return null
    val selected = localDateTime.toInstant(offset)
    return selected.takeIf {
        if (serverAuthoritativeExecution) {
            isSafeExecutionDeferTarget(it, now)
        } else {
            it > now.plusSeconds(MIN_CUSTOM_MOVE_LEAD_SECONDS)
        }
    }
}

internal fun roundUpToExecutionDeferSlot(candidate: Instant): Instant {
    val remainder = Math.floorMod(candidate.epochSecond, EXECUTION_DEFER_SLOT_SECONDS)
    val roundedSeconds = if (remainder == 0L && candidate.nano == 0) {
        candidate.epochSecond
    } else {
        Math.addExact(candidate.epochSecond, EXECUTION_DEFER_SLOT_SECONDS - remainder)
    }
    return Instant.ofEpochSecond(roundedSeconds)
}

internal fun isSafeExecutionDeferTarget(target: Instant, reference: Instant): Boolean =
    target.nano == 0 && Math.floorMod(target.epochSecond, EXECUTION_DEFER_SLOT_SECONDS) == 0L &&
        !target.isBefore(reference.plusSeconds(EXECUTION_DEFER_TARGET_LEAD_SECONDS))

internal fun moveLaterChooserExplanation(placementMode: MoveLaterPlacementMode): String =
    when (placementMode) {
        MoveLaterPlacementMode.EXACT ->
            "DayWeave will preserve the exact remaining time and publish its new placement."
        MoveLaterPlacementMode.RECOMPOSED_WINDOW ->
            "DayWeave will move this occurrence window and recompose its sessions inside it."
        MoveLaterPlacementMode.EARLIEST_START ->
            "DayWeave will allow scheduling this work from the selected time, then recompose " +
                "the firm horizon."
    }

internal fun moveLaterConfirmationPromise(placementMode: MoveLaterPlacementMode): String =
    when (placementMode) {
        MoveLaterPlacementMode.EXACT ->
            "DayWeave will preserve the exact move you approve."
        MoveLaterPlacementMode.RECOMPOSED_WINDOW ->
            "DayWeave will preserve the occurrence window and recompose inside it."
        MoveLaterPlacementMode.EARLIEST_START ->
            "DayWeave will preserve the earliest start and deadline change you approve."
    }

@Composable
internal fun MoveLaterChooserDialog(
    itemTitle: String,
    itemIsSensitive: Boolean,
    placementMode: MoveLaterPlacementMode,
    zoneId: ZoneId,
    referenceNow: Instant = Instant.now(),
    planningHorizon: ScheduleDisplayHorizon?,
    notBefore: Instant? = null,
    serverAuthoritativeExecution: Boolean = false,
    assessMove: (Instant) -> MoveLaterAssessment?,
    onDismiss: () -> Unit,
    onMove: (Instant, MoveLaterApprovalEnvelope?) -> Unit,
) {
    val context = LocalContext.current
    val use24HourFormat = DateFormat.is24HourFormat(context)
    val moveAnchor = remember(referenceNow, notBefore) {
        maxOf(referenceNow, notBefore ?: referenceNow)
    }
    val pickerBounds = remember(
        planningHorizon,
        moveAnchor,
        zoneId,
        serverAuthoritativeExecution,
    ) {
        moveLaterPickerDateBounds(
            planningHorizon = planningHorizon,
            moveAnchor = moveAnchor,
            zoneId = zoneId,
            serverAuthoritativeExecution = serverAuthoritativeExecution,
        )
    }
    val presets = remember(
        moveAnchor,
        zoneId,
        use24HourFormat,
        pickerBounds,
        serverAuthoritativeExecution,
    ) {
        moveLaterPresets(
            moveAnchor,
            zoneId,
            use24HourFormat,
            allowedDateBounds = pickerBounds,
            serverAuthoritativeExecution = serverAuthoritativeExecution,
        )
    }
    var customError by remember { mutableStateOf<String?>(null) }
    var pendingConfirmation by remember {
        mutableStateOf<Pair<Instant, MoveLaterAssessment>?>(null)
    }
    val initialCustom = presets.lastOrNull()?.moveStart?.atZone(zoneId) ?: run {
        val anchor = moveAnchor.atZone(zoneId)
        val firstDate = pickerBounds?.firstDate
        if (firstDate == null || firstDate == anchor.toLocalDate()) {
            anchor
        } else {
            firstDate.atTime(9, 0).atZone(zoneId)
        }
    }

    fun requestMove(selected: Instant) {
        if (serverAuthoritativeExecution) {
            customError = null
            onMove(selected, null)
            return
        }
        val assessment = assessMove(selected)
        when {
            assessment == null -> {
                customError = "The exact move window could not be verified. Recompose and try again."
            }
            !assessment.fitsFirmHorizonDay -> {
                customError =
                    "That move falls outside the exact firm horizon or crosses a planning-day " +
                        "boundary. Choose a time that keeps the whole move inside one horizon day."
            }
            assessment.crossesUnrelaxableHardDeadline -> {
                customError =
                    "That exact replacement ends after a hard deadline this action cannot " +
                        "safely relax. Change the item constraint first."
            }
            assessment.requiresConfirmation -> {
                customError = null
                pendingConfirmation = selected to assessment
            }
            else -> onMove(selected, assessment.toApprovalEnvelope())
        }
    }

    fun chooseCustomTime() {
        val selectableBounds = pickerBounds
        if (selectableBounds == null) {
            customError = if (serverAuthoritativeExecution) {
                "No safe future deferral date is available. Refresh and try again."
            } else {
                "The exact firm horizon is no longer available. Recompose and try again."
            }
            return
        }
        DatePickerDialog(
            context,
            { _, year, zeroBasedMonth, day ->
                val date = LocalDate.of(year, zeroBasedMonth + 1, day)
                if (date !in selectableBounds) {
                    customError = if (serverAuthoritativeExecution) {
                        "Choose a date inside the available seven-day deferral window."
                    } else {
                        "Choose a date inside the current exact firm horizon."
                    }
                    return@DatePickerDialog
                }
                TimePickerDialog(
                    context,
                    { _, hour, minute ->
                        val localDateTime = LocalDateTime.of(
                            date,
                            LocalTime.of(hour, minute),
                        )
                        val hasOneExactOffset =
                            zoneId.rules.getValidOffsets(localDateTime).size == 1
                        val selected = customMoveStart(
                            date = date,
                            time = localDateTime.toLocalTime(),
                            zoneId = zoneId,
                            now = maxOf(Instant.now(), notBefore ?: Instant.MIN),
                            serverAuthoritativeExecution = serverAuthoritativeExecution,
                        )
                        if (selected == null) {
                            customError = if (hasOneExactOffset) {
                                if (serverAuthoritativeExecution) {
                                    "Choose a five-minute slot at least ten minutes from now."
                                } else {
                                    "Choose a time at least a minute from now."
                                }
                            } else {
                                "That clock time is unavailable or ambiguous because of " +
                                    "daylight saving. Choose another time."
                            }
                        } else {
                            requestMove(selected)
                        }
                    },
                    initialCustom.hour,
                    initialCustom.minute,
                    use24HourFormat,
                ).show()
            },
            initialCustom.year,
            initialCustom.monthValue - 1,
            initialCustom.dayOfMonth,
        ).also { dialog ->
            val deviceZone = ZoneId.systemDefault()
            dialog.datePicker.minDate = selectableBounds.firstDate.atStartOfDay(deviceZone)
                .toInstant().toEpochMilli()
            dialog.datePicker.maxDate = selectableBounds.lastDateInclusive.plusDays(1)
                .atStartOfDay(deviceZone).toInstant().toEpochMilli() - 1
        }.show()
    }

    pendingConfirmation?.let { (selected, assessment) ->
        val formatter = DateTimeFormatter.ofPattern(
            if (use24HourFormat) "EEE, MMM d · HH:mm z" else "EEE, MMM d · h:mm a z",
        )
        val conflictPresentation = moveLaterConflictPresentation(
            assessment.overlappingHardBlocks,
        )
        val requiresSecureWindow = moveLaterRequiresSecureWindow(
            itemIsSensitive,
            conflictPresentation,
        )
        val details = buildList {
            if (assessment.sourceRequiresOverride) {
                add(
                    "The current placement is pinned. Moving it overrides that exact " +
                        "stability lock; its underlying task constraints remain unchanged.",
                )
            }
            assessment.crossedDeadlines.forEach { risk ->
                val deadline = Instant.parse(risk.deadline)
                val itemTargetEnd = Instant.parse(risk.targetEnd)
                val deadlineText = deadline.atZone(zoneId).format(formatter)
                val endText = itemTargetEnd.atZone(zoneId).format(formatter)
                add(
                    if (
                        assessment.canonicalDeadlineRelaxation == itemTargetEnd &&
                        risk.isCanonicalField
                    ) {
                        "This will extend the item deadline from $deadlineText to $endText."
                    } else if (!risk.isHard) {
                        when (assessment.placementMode) {
                            MoveLaterPlacementMode.EXACT ->
                                "The exact replacement ends at $endText, after its preferred " +
                                    "deadline at $deadlineText."
                            MoveLaterPlacementMode.RECOMPOSED_WINDOW ->
                                "The recomposed occurrence may place an item as late as " +
                                    "$endText, after its preferred deadline at $deadlineText."
                            MoveLaterPlacementMode.EARLIEST_START ->
                                "The earliest possible finish is $endText, after the preferred " +
                                    "deadline at $deadlineText."
                        }
                    } else {
                        "The moved work may end at $endText, after its deadline at $deadlineText."
                    },
                )
            }
            if (assessment.overlappingHardBlocks.isNotEmpty()) {
                val titles = conflictPresentation.labels.joinToString(", ")
                add("The replacement overlaps fixed or hard time: $titles.")
            }
        }
        AlertDialog(
            onDismissRequest = { pendingConfirmation = null },
            icon = { Icon(Icons.Outlined.Schedule, contentDescription = null) },
            title = { Text("Move despite this conflict?") },
            text = {
                Text(
                    details.joinToString("\n\n") +
                        "\n\n${moveLaterConfirmationPromise(assessment.placementMode)}",
                    modifier = Modifier.testTag("move_later_warning"),
                )
            },
            confirmButton = {
                Button(
                    onClick = { onMove(selected, assessment.toApprovalEnvelope()) },
                    modifier = Modifier.testTag("move_later_confirm_conflict"),
                ) { Text("Move anyway") }
            },
            dismissButton = {
                TextButton(onClick = { pendingConfirmation = null }) { Text("Go back") }
            },
            properties = DialogProperties(
                securePolicy = if (requiresSecureWindow) {
                    SecureFlagPolicy.SecureOn
                } else {
                    SecureFlagPolicy.Inherit
                },
            ),
        )
        return
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(Icons.Outlined.Schedule, contentDescription = null) },
        title = { Text("When should this continue?") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(
                    itemTitle,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 2,
                )
                Text(
                    if (serverAuthoritativeExecution) {
                        "DayWeave will save this target, confirm an exact Pause, then ask the " +
                            "server to assess the current published plan. Any warning is shown " +
                            "before the Defer command is saved."
                    } else {
                        moveLaterChooserExplanation(placementMode)
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                presets.forEachIndexed { index, preset ->
                    OutlinedButton(
                        onClick = {
                            val currentReference = maxOf(
                                Instant.now(),
                                notBefore ?: Instant.MIN,
                            )
                            val valid = if (serverAuthoritativeExecution) {
                                isSafeExecutionDeferTarget(preset.moveStart, currentReference)
                            } else {
                                preset.moveStart > currentReference.plusSeconds(
                                    MIN_CUSTOM_MOVE_LEAD_SECONDS,
                                )
                            }
                            if (!valid) {
                                customError = "That option has passed; choose another time."
                            } else {
                                requestMove(preset.moveStart)
                            }
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .testTag("move_later_preset_$index"),
                    ) {
                        Column(modifier = Modifier.fillMaxWidth()) {
                            Text(preset.label)
                            Text(
                                preset.detail,
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
                customError?.let { message ->
                    Text(
                        message,
                        modifier = Modifier
                            .semantics {
                                liveRegion = LiveRegionMode.Assertive
                                error(message)
                            }
                            .testTag("move_later_error"),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = ::chooseCustomTime,
                modifier = Modifier.testTag("move_later_custom"),
            ) { Text("Custom date & time") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        properties = DialogProperties(
            securePolicy = if (itemIsSensitive) {
                SecureFlagPolicy.SecureOn
            } else {
                SecureFlagPolicy.Inherit
            },
        ),
    )
}

internal data class ExecutionDeferWarningPresentation(
    val messages: List<String>,
    val conflictingBlockCount: Int,
    val requiresSecureWindow: Boolean,
)

internal fun executionDeferWarningPresentation(
    assessment: ExecutionDeferAssessmentSnapshot,
    sourceIsSensitive: Boolean,
    sensitiveBlockIds: Set<String>,
): ExecutionDeferWarningPresentation {
    val conflictIds = assessment.violations
        .flatMap { it.conflictingBlockIds }
        .distinct()
    return ExecutionDeferWarningPresentation(
        messages = assessment.violations
            .distinctBy { it.code to it.message }
            .map { violation -> "${violation.code}: ${violation.message}" },
        conflictingBlockCount = conflictIds.size,
        // Even content-free restriction metadata reveals private schedule shape and timing. Treat
        // every authoritative conflict review as privacy-sensitive, matching the macOS surface.
        requiresSecureWindow = assessment.violations.isNotEmpty() || sourceIsSensitive ||
            conflictIds.any(sensitiveBlockIds::contains),
    )
}

/** Exact, content-free approval surface restored from the encrypted defer intent. */
@Composable
internal fun ExecutionDeferApprovalDialog(
    assessment: ExecutionDeferAssessmentSnapshot,
    sourceIsSensitive: Boolean,
    sensitiveBlockIds: Set<String>,
    zoneId: ZoneId,
    onApprove: (String) -> Unit,
    onKeepPaused: () -> Unit,
) {
    val presentation = executionDeferWarningPresentation(
        assessment,
        sourceIsSensitive,
        sensitiveBlockIds,
    )
    val formatter = DateTimeFormatter.ofPattern("EEE, MMM d · HH:mm z")
    val moveStart = runCatching {
        Instant.parse(assessment.moveStart).atZone(zoneId).format(formatter)
    }.getOrDefault("the selected time")
    val moveEnd = runCatching {
        Instant.parse(assessment.moveEnd).atZone(zoneId).format(formatter)
    }.getOrDefault("the assessed end")
    AlertDialog(
        // Dismissal explicitly cancels only the unsent target; it never resumes or closes work.
        onDismissRequest = onKeepPaused,
        icon = { Icon(Icons.Outlined.Schedule, contentDescription = null) },
        title = { Text("Move despite these restrictions?") },
        text = {
            Column(
                verticalArrangement = Arrangement.spacedBy(10.dp),
                modifier = Modifier
                    .heightIn(max = 420.dp)
                    .verticalScroll(rememberScrollState())
                    .testTag("execution_defer_warning"),
            ) {
                Text(
                    "The server assessed the exact paused work from $moveStart to $moveEnd.",
                    style = MaterialTheme.typography.bodySmall,
                )
                presentation.messages.forEach { message -> Text("• $message") }
                if (presentation.conflictingBlockCount > 0) {
                    Text(
                        "${presentation.conflictingBlockCount} fixed schedule block(s) conflict " +
                            "with this placement. Titles are hidden in this review.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Text(
                    "Approval applies only to this exact assessment. Any plan, Calendar, item, " +
                        "execution, or target change requires a new review.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            Button(
                onClick = { onApprove(assessment.assessmentDigest) },
                modifier = Modifier.testTag("execution_defer_approve"),
            ) { Text("Approve and move") }
        },
        dismissButton = {
            TextButton(
                onClick = onKeepPaused,
                modifier = Modifier.testTag("execution_defer_keep_paused"),
            ) { Text("Keep paused") }
        },
        properties = DialogProperties(
            securePolicy = if (presentation.requiresSecureWindow) {
                SecureFlagPolicy.SecureOn
            } else {
                SecureFlagPolicy.Inherit
            },
        ),
    )
}

@Composable
internal fun ExecutionDeferPendingDialog(
    moveStart: String,
    statusMessage: String,
    zoneId: ZoneId,
    sourceIsSensitive: Boolean,
    onRetry: () -> Unit,
    onKeepPaused: () -> Unit,
) {
    val formatter = DateTimeFormatter.ofPattern("EEE, MMM d · HH:mm z")
    val target = runCatching {
        Instant.parse(moveStart).atZone(zoneId).format(formatter)
    }.getOrDefault("the saved time")
    AlertDialog(
        onDismissRequest = onKeepPaused,
        icon = { Icon(Icons.Outlined.Schedule, contentDescription = null) },
        title = { Text("Move target is still pending") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("Target: $target")
                Text(statusMessage, style = MaterialTheme.typography.bodySmall)
                Text(
                    "Retry keeps the same target, safely rechecks the paused session, and requests " +
                        "new evidence if the saved assessment is no longer exact. Keep paused " +
                        "cancels only this unsent move; it does not resume or finish the session.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            Button(
                onClick = onRetry,
                modifier = Modifier.testTag("execution_defer_retry_assessment"),
            ) { Text("Retry assessment") }
        },
        dismissButton = {
            TextButton(
                onClick = onKeepPaused,
                modifier = Modifier.testTag("execution_defer_cancel_pending"),
            ) { Text("Keep paused") }
        },
        properties = DialogProperties(
            securePolicy = if (sourceIsSensitive) {
                SecureFlagPolicy.SecureOn
            } else {
                SecureFlagPolicy.Inherit
            },
        ),
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

private const val MIN_CUSTOM_MOVE_LEAD_SECONDS = 60L
private const val EXECUTION_DEFER_SLOT_SECONDS = 5 * 60L
private const val EXECUTION_DEFER_ASSESSMENT_TTL_SECONDS = 5 * 60L
private const val EXECUTION_DEFER_TARGET_LEAD_SECONDS =
    EXECUTION_DEFER_ASSESSMENT_TTL_SECONDS + EXECUTION_DEFER_SLOT_SECONDS
