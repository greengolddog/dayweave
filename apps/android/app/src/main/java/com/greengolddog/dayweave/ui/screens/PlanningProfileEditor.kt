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
                    Text(
                        "${formatPlanningProfileMinute(profile.dayStartMinute)}–" +
                            "${formatPlanningProfileMinute(profile.dayEndMinute)} · " +
                            "${profile.slotGranularityMinutes}-minute slots",
                    )
                    Text(
                        "Stability ${profile.stabilityWeight} · " +
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
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    "Set the daily usable window and how many local calendar days belong to " +
                        "the rolling firm plan. The scheduler keeps hard commitments fixed.",
                    style = MaterialTheme.typography.bodyMedium,
                )
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
