package com.greengolddog.dayweave.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.ScheduleWeekday
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdatePhase
import com.greengolddog.dayweave.state.ScheduleCompositionProfileUpdateState
import com.greengolddog.dayweave.state.ScheduleCompositionProfileDraftMemory
import kotlin.math.roundToInt

@Composable
internal fun PlanningProfileCard(
    profile: ScheduleCompositionProfileSnapshot,
    editBlockedMessage: String?,
    updateState: ScheduleCompositionProfileUpdateState,
    onEdit: () -> Unit,
) {
    val availabilityMessage = when {
        updateState.isSaving -> "Saving the encrypted planning profile…"
        editBlockedMessage != null -> editBlockedMessage
        else -> "Changes take effect after the firm horizon is recomposed."
    }
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("planning_profile_card")
            .semantics { stateDescription = availabilityMessage },
    ) {
        ListItem(
            headlineContent = { Text("Planning profile") },
            supportingContent = {
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    if (profile.usesWeeklySchedule) {
                        val workWindows = requireNotNull(profile.availability)
                            .sumOf { it.windows.size }
                        val protectedWindows = requireNotNull(profile.protectedTime)
                            .sumOf { it.windows.size }
                        Text("${profile.timezoneName} · $workWindows weekly work windows")
                        Text(
                            "Sleep ${formatPlanningClockMinute(requireNotNull(profile.sleep).startMinute)}–" +
                                formatPlanningClockMinute(profile.sleep.endMinute) +
                                " · $protectedWindows protected windows",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    } else {
                        Text(
                            "${formatPlanningProfileMinute(profile.dayStartMinute)}–" +
                                "${formatPlanningProfileMinute(profile.dayEndMinute)} · " +
                                "${profile.slotGranularityMinutes}-minute slots",
                        )
                    }
                    Text(
                        "${profile.slotGranularityMinutes}-minute slots · " +
                            "stability ${profile.stabilityWeight} · " +
                            "soft constraints ${profile.defaultSoftWeight}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        "Firm horizon: ${planningHorizonDayLabel(profile.firmHorizonDays)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            },
            leadingContent = { Icon(Icons.Outlined.Tune, contentDescription = null) },
            trailingContent = {
                TextButton(
                    onClick = onEdit,
                    enabled = editBlockedMessage == null && !updateState.isSaving,
                    modifier = Modifier.testTag("edit_planning_profile"),
                ) {
                    Text("Edit")
                }
            },
        )
        HorizontalDivider()
        Text(
            availabilityMessage,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
        )
    }
}

@Composable
internal fun PlanningProfileEditorDialog(
    currentProfile: ScheduleCompositionProfileSnapshot,
    editBlockedMessage: String?,
    updateState: ScheduleCompositionProfileUpdateState,
    onSave: (ScheduleCompositionProfileSnapshot) -> Unit,
    onDismiss: () -> Unit,
) {
    val formSaver = remember(currentProfile) { planningProfileFormSaver(currentProfile) }
    var form by rememberSaveable(currentProfile, stateSaver = formSaver) {
        mutableStateOf(PlanningProfileForm.from(currentProfile))
    }
    val validation = form.validate()
    val enabled = editBlockedMessage == null && !updateState.isSaving
    val unchanged = validation.profile == currentProfile
    val statusMessage = when {
        editBlockedMessage != null -> editBlockedMessage
        updateState.phase in setOf(
            ScheduleCompositionProfileUpdatePhase.BLOCKED,
            ScheduleCompositionProfileUpdatePhase.ERROR,
        ) -> updateState.message
        updateState.isSaving -> updateState.message
        else -> null
    }

    AlertDialog(
        modifier = Modifier.testTag("planning_profile_editor"),
        onDismissRequest = { if (!updateState.isSaving) onDismiss() },
        icon = { Icon(Icons.Outlined.Schedule, contentDescription = null) },
        title = { Text("Planning profile") },
        text = {
            Column(
                modifier = Modifier
                    .testTag("planning_profile_scroll")
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    "Choose where time belongs in your week. Sleep and protected time stay " +
                        "visible as fixed blocks; work is placed only inside availability.",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text("Weekly schedule", style = MaterialTheme.typography.titleSmall)
                        Text(
                            if (form.useWeeklySchedule) {
                                "Timezone, sleep, multiple daily windows, and protected time"
                            } else {
                                "One daily window in the current device timezone"
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Switch(
                        checked = form.useWeeklySchedule,
                        onCheckedChange = { form = form.copy(useWeeklySchedule = it) },
                        enabled = enabled,
                        modifier = Modifier
                            .testTag("planning_weekly_schedule")
                            .semantics { contentDescription = "Use weekly schedule" },
                    )
                }
                if (form.useWeeklySchedule) {
                    OutlinedTextField(
                        value = form.timezoneName,
                        onValueChange = { form = form.copy(timezoneName = it.take(255)) },
                        label = { Text("IANA timezone") },
                        supportingText = {
                            Text(validation.timezoneError ?: "Example: Europe/Paris")
                        },
                        singleLine = true,
                        enabled = enabled,
                        isError = validation.timezoneError != null,
                        modifier = Modifier.fillMaxWidth().testTag("planning_timezone"),
                    )
                    ClockWindowInput(
                        label = "Overnight sleep",
                        start = form.sleepStart,
                        end = form.sleepEnd,
                        error = validation.sleepError,
                        enabled = enabled,
                        tagPrefix = "planning_sleep",
                        onStartChange = {
                            form = form.copy(sleepStart = sanitizePlanningClock(it))
                        },
                        onEndChange = {
                            form = form.copy(sleepEnd = sanitizePlanningClock(it))
                        },
                    )
                    Text("Availability", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Add up to eight non-overlapping windows on each enabled day.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    form.availabilityDays.forEachIndexed { index, day ->
                        PlanningDayWindowEditor(
                            day = day,
                            enabled = enabled,
                            tagPrefix = "planning_availability",
                            emptyDayWindow = PlanningWindowForm("09:00", "17:00"),
                            onChange = { replacement ->
                                form = form.copy(
                                    availabilityDays = form.availabilityDays.replaceAt(
                                        index,
                                        replacement,
                                    ),
                                )
                            },
                        )
                    }
                    HorizontalDivider()
                    Text("Protected free time", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Protected windows remain visible and cannot be used for planned work.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    form.protectedDays.forEachIndexed { index, day ->
                        PlanningDayWindowEditor(
                            day = day,
                            enabled = enabled,
                            tagPrefix = "planning_protected",
                            emptyDayWindow = PlanningWindowForm("21:30", "22:30"),
                            onChange = { replacement ->
                                form = form.copy(
                                    protectedDays = form.protectedDays.replaceAt(
                                        index,
                                        replacement,
                                    ),
                                )
                            },
                        )
                    }
                    validation.weeklyScheduleError?.let { FormError(it) }
                } else {
                    TimeInputRow(
                        label = "Start",
                        hour = form.startHour,
                        minute = form.startMinute,
                        error = validation.startError,
                        enabled = enabled,
                        tagPrefix = "planning_start",
                        onHourChange = {
                            form = form.copy(startHour = sanitizePlanningTimePart(it))
                        },
                        onMinuteChange = {
                            form = form.copy(startMinute = sanitizePlanningTimePart(it))
                        },
                    )
                    TimeInputRow(
                        label = "End",
                        hour = form.endHour,
                        minute = form.endMinute,
                        error = validation.endError,
                        enabled = enabled,
                        tagPrefix = "planning_end",
                        onHourChange = {
                            form = form.copy(endHour = sanitizePlanningTimePart(it))
                        },
                        onMinuteChange = {
                            form = form.copy(endMinute = sanitizePlanningTimePart(it))
                        },
                    )
                }
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text("Firm horizon", style = MaterialTheme.typography.titleSmall)
                        Text(planningHorizonDayLabel(form.firmHorizonDays))
                    }
                    Slider(
                        value = form.firmHorizonDays.toFloat(),
                        onValueChange = {
                            form = form.copy(firmHorizonDays = it.roundToInt())
                        },
                        valueRange = ScheduleCompositionProfileSnapshot.MIN_FIRM_HORIZON_DAYS
                            .toFloat()..
                            ScheduleCompositionProfileSnapshot.MAX_FIRM_HORIZON_DAYS.toFloat(),
                        steps = ScheduleCompositionProfileSnapshot.MAX_FIRM_HORIZON_DAYS -
                            ScheduleCompositionProfileSnapshot.MIN_FIRM_HORIZON_DAYS - 1,
                        enabled = enabled,
                        modifier = Modifier
                            .testTag("planning_firm_horizon")
                            .semantics {
                                contentDescription = "Firm horizon"
                                stateDescription = planningHorizonDayLabel(form.firmHorizonDays)
                            },
                    )
                    validation.firmHorizonError?.let { FormError(it) }
                }
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text("Slot size", style = MaterialTheme.typography.titleSmall)
                        Text("${form.slotGranularityMinutes} min")
                    }
                    Slider(
                        value = form.slotGranularityMinutes.toFloat(),
                        onValueChange = {
                            form = form.copy(slotGranularityMinutes = it.roundToInt())
                        },
                        valueRange = MIN_SLOT_GRANULARITY_MINUTES.toFloat()..
                            MAX_SLOT_GRANULARITY_MINUTES.toFloat(),
                        steps = MAX_SLOT_GRANULARITY_MINUTES -
                            MIN_SLOT_GRANULARITY_MINUTES - 1,
                        enabled = enabled,
                        modifier = Modifier
                            .testTag("planning_slot_granularity")
                            .semantics {
                                contentDescription = "Slot size"
                                stateDescription = "${form.slotGranularityMinutes} minutes"
                            },
                    )
                    validation.granularityError?.let { FormError(it) }
                }
                WeightInput(
                    value = form.stabilityWeight,
                    label = "Stability weight",
                    explanation = "Higher values prefer the previous plan when work still fits.",
                    error = validation.stabilityWeightError,
                    enabled = enabled,
                    tag = "planning_stability_weight",
                    onValueChange = {
                        form = form.copy(stabilityWeight = sanitizePlanningWeight(it))
                    },
                )
                WeightInput(
                    value = form.defaultSoftWeight,
                    label = "Default soft-constraint weight",
                    explanation = "Higher values make flexible preferences more influential.",
                    error = validation.defaultSoftWeightError,
                    enabled = enabled,
                    tag = "planning_soft_weight",
                    onValueChange = {
                        form = form.copy(defaultSoftWeight = sanitizePlanningWeight(it))
                    },
                )
                TextButton(
                    onClick = {
                        form = PlanningProfileForm.from(ScheduleCompositionProfileSnapshot())
                    },
                    enabled = enabled && form != PlanningProfileForm.from(
                        ScheduleCompositionProfileSnapshot(),
                    ),
                    modifier = Modifier.testTag("reset_planning_profile"),
                ) {
                    Text("Reset defaults")
                }
                statusMessage?.let { message ->
                    Text(
                        message,
                        style = MaterialTheme.typography.bodySmall,
                        color = if (
                            updateState.phase == ScheduleCompositionProfileUpdatePhase.ERROR ||
                            updateState.phase == ScheduleCompositionProfileUpdatePhase.BLOCKED ||
                            editBlockedMessage != null
                        ) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        modifier = Modifier
                            .testTag("planning_profile_status")
                            .semantics { liveRegion = LiveRegionMode.Polite },
                    )
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { validation.profile?.let(onSave) },
                enabled = enabled && validation.isValid && !unchanged,
                modifier = Modifier.testTag("save_planning_profile"),
            ) {
                if (updateState.isSaving) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(18.dp).padding(end = 4.dp),
                        strokeWidth = 2.dp,
                    )
                }
                Text(if (updateState.isSaving) "Saving" else "Save")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !updateState.isSaving) {
                Text("Cancel")
            }
        },
    )
}

internal fun planningHorizonDayLabel(days: Int): String =
    "$days ${if (days == 1) "day" else "days"}"

@Composable
private fun TimeInputRow(
    label: String,
    hour: String,
    minute: String,
    error: String?,
    enabled: Boolean,
    tagPrefix: String,
    onHourChange: (String) -> Unit,
    onMinuteChange: (String) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, style = MaterialTheme.typography.titleSmall)
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedTextField(
                value = hour,
                onValueChange = onHourChange,
                label = { Text("$label hour") },
                singleLine = true,
                enabled = enabled,
                isError = error != null,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.weight(1f).testTag("${tagPrefix}_hour"),
            )
            Text(":", style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(
                value = minute,
                onValueChange = onMinuteChange,
                label = { Text("$label minute") },
                singleLine = true,
                enabled = enabled,
                isError = error != null,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.weight(1f).testTag("${tagPrefix}_minute"),
            )
        }
        error?.let { FormError(it) }
    }
}

@Composable
private fun ClockWindowInput(
    label: String,
    start: String,
    end: String,
    error: String?,
    enabled: Boolean,
    tagPrefix: String,
    onStartChange: (String) -> Unit,
    onEndChange: (String) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, style = MaterialTheme.typography.titleSmall)
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedTextField(
                value = start,
                onValueChange = onStartChange,
                label = { Text("Start") },
                supportingText = { Text("HH:mm") },
                singleLine = true,
                enabled = enabled,
                isError = error != null,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
                modifier = Modifier
                    .weight(1f)
                    .testTag("${tagPrefix}_start")
                    .semantics { contentDescription = "$label start, HH:mm" },
            )
            OutlinedTextField(
                value = end,
                onValueChange = onEndChange,
                label = { Text("End") },
                supportingText = { Text("HH:mm") },
                singleLine = true,
                enabled = enabled,
                isError = error != null,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
                modifier = Modifier
                    .weight(1f)
                    .testTag("${tagPrefix}_end")
                    .semantics { contentDescription = "$label end, HH:mm" },
            )
        }
        error?.let { FormError(it) }
    }
}

@Composable
private fun PlanningDayWindowEditor(
    day: PlanningDayForm,
    enabled: Boolean,
    tagPrefix: String,
    emptyDayWindow: PlanningWindowForm,
    onChange: (PlanningDayForm) -> Unit,
) {
    val dayTag = day.weekday.name.lowercase()
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("${tagPrefix}_$dayTag")
            .padding(vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(day.weekday.displayName(), style = MaterialTheme.typography.titleSmall)
            Switch(
                checked = day.isEnabled,
                onCheckedChange = { checked ->
                    onChange(
                        day.copy(
                            isEnabled = checked,
                            windows = if (checked) {
                                day.windows.ifEmpty { listOf(emptyDayWindow) }
                            } else {
                                emptyList()
                            },
                        ),
                    )
                },
                enabled = enabled,
                modifier = Modifier
                    .testTag("${tagPrefix}_${dayTag}_enabled")
                    .semantics {
                        contentDescription =
                            "${day.weekday.displayName()} ${tagPrefix.windowKindLabel()} enabled"
                    },
            )
        }
        if (day.isEnabled) {
            day.windows.forEachIndexed { index, window ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = window.start,
                        onValueChange = { raw ->
                            onChange(
                                day.copy(
                                    windows = day.windows.replaceAt(
                                        index,
                                        window.copy(start = sanitizePlanningClock(raw)),
                                    ),
                                ),
                            )
                        },
                        label = { Text("Start") },
                        singleLine = true,
                        enabled = enabled,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
                        modifier = Modifier
                            .weight(1f)
                            .testTag("${tagPrefix}_${dayTag}_${index}_start")
                            .semantics {
                                contentDescription = day.windowContentDescription(
                                    tagPrefix,
                                    index,
                                    "start",
                                )
                            },
                    )
                    OutlinedTextField(
                        value = window.end,
                        onValueChange = { raw ->
                            onChange(
                                day.copy(
                                    windows = day.windows.replaceAt(
                                        index,
                                        window.copy(end = sanitizePlanningClock(raw)),
                                    ),
                                ),
                            )
                        },
                        label = { Text("End") },
                        singleLine = true,
                        enabled = enabled,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Text),
                        modifier = Modifier
                            .weight(1f)
                            .testTag("${tagPrefix}_${dayTag}_${index}_end")
                            .semantics {
                                contentDescription = day.windowContentDescription(
                                    tagPrefix,
                                    index,
                                    "end",
                                )
                            },
                    )
                    TextButton(
                        onClick = {
                            val remaining = day.windows.filterIndexed { at, _ -> at != index }
                            onChange(day.copy(isEnabled = remaining.isNotEmpty(), windows = remaining))
                        },
                        enabled = enabled,
                        modifier = Modifier
                            .testTag("${tagPrefix}_${dayTag}_${index}_remove")
                            .semantics {
                                contentDescription = day.windowContentDescription(
                                    tagPrefix,
                                    index,
                                    "remove",
                                )
                            },
                    ) {
                        Text("Remove")
                    }
                }
            }
            TextButton(
                onClick = { onChange(day.copy(windows = day.windows + emptyDayWindow)) },
                enabled = enabled && day.windows.size < 8,
                modifier = Modifier
                    .testTag("${tagPrefix}_${dayTag}_add")
                    .semantics {
                        contentDescription =
                            "Add ${day.weekday.displayName()} ${tagPrefix.windowKindLabel()} window"
                    },
            ) {
                Text("Add window")
            }
        }
    }
}

private fun ScheduleWeekday.displayName(): String =
    name.lowercase().replaceFirstChar { it.uppercase() }

private fun String.windowKindLabel(): String =
    if (endsWith("protected")) "protected-time" else "availability"

private fun PlanningDayForm.windowContentDescription(
    tagPrefix: String,
    index: Int,
    action: String,
): String =
    "${weekday.displayName()} ${tagPrefix.windowKindLabel()} window ${index + 1} $action"

private fun <T> List<T>.replaceAt(index: Int, replacement: T): List<T> =
    mapIndexed { current, value -> if (current == index) replacement else value }

@Composable
private fun WeightInput(
    value: String,
    label: String,
    explanation: String,
    error: String?,
    enabled: Boolean,
    tag: String,
    onValueChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        supportingText = { Text(error ?: "$explanation Range: 0–1,000,000.") },
        singleLine = true,
        enabled = enabled,
        isError = error != null,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(tag),
    )
}

@Composable
private fun FormError(message: String) {
    Text(
        message,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.error,
    )
}

private fun planningProfileFormSaver(
    baseline: ScheduleCompositionProfileSnapshot,
): Saver<PlanningProfileForm, String> = Saver(
    save = { form ->
        ScheduleCompositionProfileDraftMemory.retain(
            baseline = baseline,
            nextValues = form.toDraftMemoryValues(),
        )
    },
    restore = { token ->
        ScheduleCompositionProfileDraftMemory.restore(token, baseline)
            ?.let(::planningProfileFormFromDraftMemoryValues)
    },
)
