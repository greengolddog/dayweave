package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.EditNote
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalEventTimingDraft
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceDraft
import com.greengolddog.dayweave.model.CanonicalRecurrenceKind
import com.greengolddog.dayweave.model.CanonicalSplitDraft
import com.greengolddog.dayweave.model.CanonicalSplitKind
import com.greengolddog.dayweave.model.CanonicalWeekday
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.EnergyLevel
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.state.canonicalDeviceTimezoneName
import java.time.Duration
import java.time.Instant
import java.util.UUID
import kotlin.math.roundToInt
import kotlinx.coroutines.launch

internal enum class CanonicalItemEditorMode {
    CREATE,
    REPLACE,
    UPDATE_PENDING,
}

internal data class CanonicalItemEditorRoute(
    val itemId: String,
    val initialDraft: CanonicalItemDraft,
    val mode: CanonicalItemEditorMode,
    val mutationId: String? = null,
    val sourceInboxId: String? = null,
) {
    val routeId: String = mutationId ?: sourceInboxId ?: "$mode:$itemId"

    companion object {
        fun create(
            title: String = "",
            kind: ItemKind = ItemKind.TASK,
            isSensitive: Boolean = false,
        ): CanonicalItemEditorRoute = CanonicalItemEditorRoute(
            itemId = UUID.randomUUID().toString(),
            initialDraft = newCanonicalDetailedDraft(title, kind, isSensitive),
            mode = CanonicalItemEditorMode.CREATE,
        )

        fun fromInbox(item: InboxItem): CanonicalItemEditorRoute {
            val route = create(
                title = item.title,
                kind = ItemKind.TASK,
                isSensitive = item.isSensitive,
            )
            return route.copy(
                initialDraft = route.initialDraft.copy(
                    notes = item.detail.takeIf(String::isNotBlank),
                ),
                sourceInboxId = item.id,
            )
        }
    }
}

internal data class CanonicalParentOption(
    val id: String,
    val title: String,
)

internal data class CanonicalItemEditorForm(
    val source: CanonicalItemDraft,
    val placement: CanonicalDraftPlacement,
    val kind: ItemKind,
    val isSensitive: Boolean,
    val title: String,
    val notes: String,
    val timezoneName: String,
    val hasDuration: Boolean,
    val durationSeconds: String,
    val earliestStartAt: String,
    val deadlineAt: String,
    val recurrenceKind: CanonicalRecurrenceKind?,
    val recurrenceCount: String,
    val recurrenceIntervalMinutes: String,
    val weekdays: Set<CanonicalWeekday>,
    val energy: EnergyLevel?,
    val tags: String,
    val preferredStartMinute: String,
    val minimumGapMinutes: String,
    val maximumSessions: String,
    val isSplittable: Boolean,
    val minimumChunkSeconds: String,
    val maximumChunkSeconds: String,
    val importance: Int,
    val urgency: Int,
    val parentId: String?,
    val siblingOrder: String,
    val eventStart: String,
    val eventEnd: String,
    val eventAllDay: Boolean,
    val eventTentative: Boolean,
    val eventBusy: Boolean,
) {
    val supportsRecurrence: Boolean
        get() = kind in setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE)

    fun withKind(value: ItemKind): CanonicalItemEditorForm = copy(
        kind = value,
        recurrenceKind = when {
            value == ItemKind.HABIT && recurrenceKind == null -> CanonicalRecurrenceKind.DAILY
            value !in setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE) -> null
            else -> recurrenceKind
        },
        isSplittable = isSplittable && value != ItemKind.EVENT,
    )

    fun draft(itemId: String): Result<CanonicalItemDraft> = runCatching {
        val ordinaryDuration = if (hasDuration) {
            durationSeconds.requiredLong("Duration")
        } else {
            null
        }
        val event = if (kind == ItemKind.EVENT) {
            CanonicalEventTimingDraft(
                startsAt = eventStart.required("Event start"),
                endsAt = eventEnd.required("Event end"),
                allDay = eventAllDay,
                tentative = eventTentative,
                busy = eventBusy,
            )
        } else {
            null
        }
        val eventDuration = event?.let {
            Duration.between(Instant.parse(it.startsAt), Instant.parse(it.endsAt)).seconds
        }
        val duration = eventDuration ?: ordinaryDuration
        val recurrence = recurrenceKind?.let { recurrenceType ->
            when (recurrenceType) {
                CanonicalRecurrenceKind.DAILY,
                CanonicalRecurrenceKind.MONTHLY,
                -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    occurrencesPerPeriod = recurrenceCount.requiredInt("Repeat count"),
                )
                CanonicalRecurrenceKind.WEEKLY -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    occurrencesPerPeriod = recurrenceCount.requiredInt("Repeat count"),
                    weekdays = CanonicalWeekday.entries.filter(weekdays::contains),
                )
                CanonicalRecurrenceKind.EVERY_INTERVAL,
                CanonicalRecurrenceKind.AFTER_COMPLETION,
                -> CanonicalRecurrenceDraft(
                    kind = recurrenceType,
                    intervalSeconds = Math.multiplyExact(
                        recurrenceIntervalMinutes.requiredLong("Repeat interval"),
                        60L,
                    ),
                )
            }
        }
        val constraints = if (event == null) {
            CanonicalFlexibleConstraintsDraft(
                energy = energy,
                tags = tags.split(',').map(String::trim).filter(String::isNotEmpty),
                preferredStartMinute = preferredStartMinute.optionalInt("Preferred start"),
                minimumGapMinutes = minimumGapMinutes.optionalInt("Minimum gap") ?: 0,
                maximumSessions = maximumSessions.optionalInt("Maximum sessions"),
            )
        } else {
            CanonicalFlexibleConstraintsDraft()
        }
        val split = if (event == null && isSplittable) {
            CanonicalSplitDraft(
                kind = CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = minimumChunkSeconds.requiredLong("Minimum chunk"),
                maximumChunkSeconds = maximumChunkSeconds.requiredLong("Maximum chunk"),
            )
        } else {
            CanonicalSplitDraft()
        }
        source.copy(
            placement = placement,
            kind = kind,
            isSensitive = isSensitive,
            title = title,
            notes = notes,
            timezoneName = timezoneName,
            durationSeconds = duration,
            deadlineAt = event?.endsAt ?: deadlineAt.optional("Deadline"),
            earliestStartAt = event?.startsAt ?: earliestStartAt.optional("Earliest start"),
            recurrence = recurrence,
            constraints = constraints,
            split = split,
            importance = importance,
            urgency = urgency,
            parentId = parentId,
            siblingOrder = siblingOrder.requiredLong("Sibling order"),
            eventTiming = event,
        ).normalized().also { it.requireValid(itemId) }
    }

    fun validationIssue(itemId: String): String? = draft(itemId).exceptionOrNull()?.let {
        it.message?.takeIf(String::isNotBlank) ?: "Review the highlighted item details."
    }

    companion object {
        fun from(draft: CanonicalItemDraft): CanonicalItemEditorForm =
            CanonicalItemEditorForm(
                source = draft,
                placement = draft.placement,
                kind = draft.kind,
                isSensitive = draft.isSensitive,
                title = draft.title,
                notes = draft.notes.orEmpty(),
                timezoneName = draft.timezoneName,
                hasDuration = draft.durationSeconds != null,
                durationSeconds = (draft.durationSeconds ?: 30L * 60L).toString(),
                earliestStartAt = draft.earliestStartAt.orEmpty(),
                deadlineAt = draft.deadlineAt.orEmpty(),
                recurrenceKind = draft.recurrence?.kind,
                recurrenceCount = (draft.recurrence?.occurrencesPerPeriod ?: 1).toString(),
                recurrenceIntervalMinutes =
                    ((draft.recurrence?.intervalSeconds ?: 24L * 60L * 60L) / 60L).toString(),
                weekdays = draft.recurrence?.weekdays.orEmpty().toSet(),
                energy = draft.constraints.energy,
                tags = draft.constraints.tags.joinToString(", "),
                preferredStartMinute = draft.constraints.preferredStartMinute?.toString().orEmpty(),
                minimumGapMinutes = draft.constraints.minimumGapMinutes.toString(),
                maximumSessions = draft.constraints.maximumSessions?.toString().orEmpty(),
                isSplittable = draft.split.kind == CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = (draft.split.minimumChunkSeconds ?: 15L * 60L).toString(),
                maximumChunkSeconds =
                    (draft.split.maximumChunkSeconds ?: draft.durationSeconds ?: 30L * 60L).toString(),
                importance = draft.importance,
                urgency = draft.urgency,
                parentId = draft.parentId,
                siblingOrder = draft.siblingOrder.toString(),
                eventStart = draft.eventTiming?.startsAt.orEmpty(),
                eventEnd = draft.eventTiming?.endsAt.orEmpty(),
                eventAllDay = draft.eventTiming?.allDay ?: false,
                eventTentative = draft.eventTiming?.tentative ?: false,
                eventBusy = draft.eventTiming?.busy ?: true,
            )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CanonicalItemEditorSheet(
    route: CanonicalItemEditorRoute,
    parentOptions: List<CanonicalParentOption>,
    onDismiss: () -> Unit,
    onSave: suspend (CanonicalItemDraft) -> Boolean,
) {
    var form by remember(route.routeId) {
        mutableStateOf(CanonicalItemEditorForm.from(route.initialDraft))
    }
    var saveError by remember(route.routeId) { mutableStateOf<String?>(null) }
    var isSaving by remember(route.routeId) { mutableStateOf(false) }
    val coroutineScope = rememberCoroutineScope()
    val issue = form.validationIssue(route.itemId)

    ModalBottomSheet(onDismissRequest = { if (!isSaving) onDismiss() }) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(max = 820.dp)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Icon(Icons.Outlined.EditNote, contentDescription = null)
                Column {
                    Text(
                        when (route.mode) {
                            CanonicalItemEditorMode.CREATE -> "New detailed item"
                            CanonicalItemEditorMode.REPLACE -> "Edit canonical item"
                            CanonicalItemEditorMode.UPDATE_PENDING -> "Edit queued change"
                        },
                        style = MaterialTheme.typography.titleLarge,
                    )
                    Text(
                        "Saved to the encrypted journal before any sync.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            EditorSection("Identity") {
                OutlinedTextField(
                    value = form.title,
                    onValueChange = { form = form.copy(title = it) },
                    label = { Text("Title") },
                    modifier = Modifier.fillMaxWidth().testTag("canonical_editor_title"),
                    singleLine = true,
                )
                OutlinedTextField(
                    value = form.notes,
                    onValueChange = { form = form.copy(notes = it) },
                    label = { Text("Notes") },
                    modifier = Modifier.fillMaxWidth(),
                    minLines = 3,
                )
                ChoiceRow {
                    ItemKind.entries.forEach { option ->
                        FilterChip(
                            selected = form.kind == option,
                            onClick = { form = form.withKind(option) },
                            label = { Text(option.label) },
                        )
                    }
                }
                LabeledSwitch(
                    title = "Sensitive",
                    detail = "Protect the title, notes, and inherited child context.",
                    checked = form.isSensitive,
                    onCheckedChange = { form = form.copy(isSensitive = it) },
                    testTag = "canonical_editor_sensitive",
                )
            }

            EditorSection("Planning") {
                ChoiceRow {
                    CanonicalDraftPlacement.entries.forEach { placement ->
                        FilterChip(
                            selected = form.placement == placement,
                            onClick = { form = form.copy(placement = placement) },
                            label = {
                                Text(if (placement == CanonicalDraftPlacement.INBOX) "Inbox" else "Planned")
                            },
                        )
                    }
                }
                OutlinedTextField(
                    value = form.timezoneName,
                    onValueChange = { form = form.copy(timezoneName = it) },
                    label = { Text("IANA timezone") },
                    modifier = Modifier.fillMaxWidth(),
                    singleLine = true,
                )
                if (form.kind != ItemKind.EVENT) {
                    LabeledSwitch(
                        title = "Duration estimate",
                        detail = "Required when the item can be split.",
                        checked = form.hasDuration,
                        onCheckedChange = { form = form.copy(hasDuration = it) },
                    )
                    if (form.hasDuration) {
                        NumberField(
                            value = form.durationSeconds,
                            onValueChange = { form = form.copy(durationSeconds = it) },
                            label = "Duration (seconds)",
                        )
                    }
                    InstantField(
                        value = form.earliestStartAt,
                        onValueChange = { form = form.copy(earliestStartAt = it) },
                        label = "Earliest start (optional ISO-8601)",
                    )
                    InstantField(
                        value = form.deadlineAt,
                        onValueChange = { form = form.copy(deadlineAt = it) },
                        label = "Deadline (optional ISO-8601)",
                    )
                }
                PrioritySlider("Importance", form.importance) {
                    form = form.copy(importance = it)
                }
                PrioritySlider("Urgency", form.urgency) {
                    form = form.copy(urgency = it)
                }
            }

            if (form.kind == ItemKind.EVENT) {
                EditorSection("Exact event timing") {
                    Text(
                        "Events require explicit canonical instants; DayWeave will not invent them.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    InstantField(
                        value = form.eventStart,
                        onValueChange = { form = form.copy(eventStart = it) },
                        label = "Starts at (ISO-8601)",
                    )
                    InstantField(
                        value = form.eventEnd,
                        onValueChange = { form = form.copy(eventEnd = it) },
                        label = "Ends at (ISO-8601)",
                    )
                    LabeledSwitch("All day", "Bounds must be local midnight.", form.eventAllDay, {
                        form = form.copy(eventAllDay = it)
                    })
                    LabeledSwitch("Tentative", "Mark this firm block tentative.", form.eventTentative, {
                        form = form.copy(eventTentative = it)
                    })
                    LabeledSwitch("Busy", "Reserve the event interval.", form.eventBusy, {
                        form = form.copy(eventBusy = it)
                    })
                }
            }

            if (form.supportsRecurrence) {
                EditorSection("Recurrence") {
                    ChoiceRow {
                        FilterChip(
                            selected = form.recurrenceKind == null,
                            onClick = {
                                if (form.kind != ItemKind.HABIT) form = form.copy(recurrenceKind = null)
                            },
                            enabled = form.kind != ItemKind.HABIT,
                            label = { Text("None") },
                        )
                        CanonicalRecurrenceKind.entries.forEach { option ->
                            FilterChip(
                                selected = form.recurrenceKind == option,
                                onClick = { form = form.copy(recurrenceKind = option) },
                                label = { Text(option.editorLabel()) },
                            )
                        }
                    }
                    when (form.recurrenceKind) {
                        CanonicalRecurrenceKind.DAILY,
                        CanonicalRecurrenceKind.WEEKLY,
                        CanonicalRecurrenceKind.MONTHLY,
                        -> NumberField(
                            value = form.recurrenceCount,
                            onValueChange = { form = form.copy(recurrenceCount = it) },
                            label = "Occurrences per period",
                        )
                        CanonicalRecurrenceKind.EVERY_INTERVAL,
                        CanonicalRecurrenceKind.AFTER_COMPLETION,
                        -> NumberField(
                            value = form.recurrenceIntervalMinutes,
                            onValueChange = { form = form.copy(recurrenceIntervalMinutes = it) },
                            label = "Interval (minutes)",
                        )
                        null -> Unit
                    }
                    if (form.recurrenceKind == CanonicalRecurrenceKind.WEEKLY) {
                        ChoiceRow {
                            CanonicalWeekday.entries.forEach { weekday ->
                                FilterChip(
                                    selected = weekday in form.weekdays,
                                    onClick = {
                                        form = form.copy(
                                            weekdays = if (weekday in form.weekdays) {
                                                form.weekdays - weekday
                                            } else {
                                                form.weekdays + weekday
                                            },
                                        )
                                    },
                                    label = { Text(weekday.name.take(2)) },
                                )
                            }
                        }
                    }
                }
            }

            if (form.kind != ItemKind.EVENT) {
                EditorSection("Flexible constraints") {
                    Text("Energy", style = MaterialTheme.typography.labelLarge)
                    ChoiceRow {
                        FilterChip(
                            selected = form.energy == null,
                            onClick = { form = form.copy(energy = null) },
                            label = { Text("Any") },
                        )
                        EnergyLevel.entries.forEach { energy ->
                            FilterChip(
                                selected = form.energy == energy,
                                onClick = { form = form.copy(energy = energy) },
                                label = { Text(energy.label) },
                            )
                        }
                    }
                    OutlinedTextField(
                        value = form.tags,
                        onValueChange = { form = form.copy(tags = it) },
                        label = { Text("Tags (comma separated)") },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    NumberField(
                        value = form.preferredStartMinute,
                        onValueChange = { form = form.copy(preferredStartMinute = it) },
                        label = "Preferred minute of day (optional)",
                    )
                    NumberField(
                        value = form.minimumGapMinutes,
                        onValueChange = { form = form.copy(minimumGapMinutes = it) },
                        label = "Minimum gap (minutes)",
                    )
                    NumberField(
                        value = form.maximumSessions,
                        onValueChange = { form = form.copy(maximumSessions = it) },
                        label = "Maximum sessions (optional)",
                    )
                }

                EditorSection("Split policy") {
                    LabeledSwitch(
                        title = "Splittable",
                        detail = "Allow the scheduler to compose multiple sessions.",
                        checked = form.isSplittable,
                        onCheckedChange = { form = form.copy(isSplittable = it) },
                    )
                    if (form.isSplittable) {
                        NumberField(
                            value = form.minimumChunkSeconds,
                            onValueChange = { form = form.copy(minimumChunkSeconds = it) },
                            label = "Minimum chunk (seconds)",
                        )
                        NumberField(
                            value = form.maximumChunkSeconds,
                            onValueChange = { form = form.copy(maximumChunkSeconds = it) },
                            label = "Maximum chunk (seconds)",
                        )
                    }
                }
            }

            EditorSection("Hierarchy") {
                ParentPicker(
                    selectedId = form.parentId,
                    options = parentOptions,
                    onSelected = { form = form.copy(parentId = it) },
                )
                NumberField(
                    value = form.siblingOrder,
                    onValueChange = { form = form.copy(siblingOrder = it) },
                    label = "Sibling order",
                )
            }

            (saveError ?: issue)?.let { message ->
                Row(
                    modifier = Modifier.fillMaxWidth().testTag("canonical_editor_diagnostic"),
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
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth().padding(bottom = 28.dp),
                horizontalArrangement = Arrangement.End,
            ) {
                OutlinedButton(onClick = onDismiss, enabled = !isSaving) { Text("Cancel") }
                Spacer(Modifier.width(10.dp))
                Button(
                    onClick = {
                        val draft = form.draft(route.itemId).getOrNull() ?: return@Button
                        isSaving = true
                        saveError = null
                        coroutineScope.launch {
                            if (!onSave(draft)) {
                                saveError = "The exact change could not be saved. Review pending actions and try again."
                            }
                            isSaving = false
                        }
                    },
                    enabled = issue == null && !isSaving,
                    modifier = Modifier.testTag("canonical_editor_save"),
                ) {
                    Text(
                        when (route.mode) {
                            CanonicalItemEditorMode.CREATE -> if (isSaving) "Saving…" else "Queue item"
                            CanonicalItemEditorMode.REPLACE -> if (isSaving) "Saving…" else "Queue changes"
                            CanonicalItemEditorMode.UPDATE_PENDING ->
                                if (isSaving) "Saving…" else "Update queued change"
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun EditorSection(
    title: String,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(11.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        content()
        HorizontalDivider(modifier = Modifier.padding(top = 5.dp))
    }
}

@Composable
private fun ChoiceRow(content: @Composable RowScope.() -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        content = content,
    )
}

@Composable
private fun LabeledSwitch(
    title: String,
    detail: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    testTag: String? = null,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            modifier = testTag?.let { Modifier.testTag(it) } ?: Modifier,
        )
    }
}

@Composable
private fun NumberField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        modifier = Modifier.fillMaxWidth(),
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
    )
}

@Composable
private fun InstantField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        placeholder = { Text("2026-08-30T10:00:00Z") },
        modifier = Modifier.fillMaxWidth(),
        singleLine = true,
    )
}

@Composable
private fun PrioritySlider(
    label: String,
    value: Int,
    onValueChange: (Int) -> Unit,
) {
    Column {
        Text("$label · $value", style = MaterialTheme.typography.labelLarge)
        Slider(
            value = value.toFloat(),
            onValueChange = { onValueChange(it.roundToInt()) },
            valueRange = 0f..100f,
            steps = 99,
        )
    }
}

@Composable
private fun ParentPicker(
    selectedId: String?,
    options: List<CanonicalParentOption>,
    onSelected: (String?) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val title = options.firstOrNull { it.id == selectedId }?.title ?: "No parent"
    Box {
        OutlinedButton(onClick = { expanded = true }) { Text("Parent · $title") }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text("No parent") },
                onClick = {
                    onSelected(null)
                    expanded = false
                },
            )
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option.title) },
                    onClick = {
                        onSelected(option.id)
                        expanded = false
                    },
                )
            }
        }
    }
}

internal fun newCanonicalDetailedDraft(
    title: String = "",
    kind: ItemKind = ItemKind.TASK,
    isSensitive: Boolean = false,
): CanonicalItemDraft = CanonicalItemDraft(
    placement = CanonicalDraftPlacement.INBOX,
    kind = kind,
    isSensitive = isSensitive,
    title = title.trim(),
    timezoneName = canonicalDeviceTimezoneName(),
    recurrence = if (kind == ItemKind.HABIT) {
        CanonicalRecurrenceDraft(
            kind = CanonicalRecurrenceKind.DAILY,
            occurrencesPerPeriod = 1,
        )
    } else {
        null
    },
)

internal fun canonicalParentOptions(
    state: DayWeaveUiState,
    excludingItemId: String,
): List<CanonicalParentOption> {
    data class ParentNode(val id: String, val title: String, val parentId: String?)

    val nodes = state.canonicalItems
        .filter { it.deletedAt == null && it.status in setOf("inbox", "planned") }
        .associate { it.id to ParentNode(it.id, it.title, it.parentId) }
        .toMutableMap()
    state.pendingCanonicalAuthoringMutations.forEach { mutation ->
        when (mutation.operation) {
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.TRASH ->
                nodes.remove(mutation.itemId)
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.CREATE,
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.REPLACE,
            -> mutation.draft?.let { draft ->
                nodes[mutation.itemId] = ParentNode(mutation.itemId, draft.title, draft.parentId)
            }
            com.greengolddog.dayweave.model.CanonicalAuthoringOperation.RESTORE -> Unit
        }
    }
    val excluded = mutableSetOf(excludingItemId)
    var changed = true
    while (changed) {
        changed = false
        nodes.values.forEach { node ->
            if (node.parentId in excluded && excluded.add(node.id)) changed = true
        }
    }
    return nodes.values
        .filter { it.id !in excluded }
        .sortedWith(compareBy({ it.title.lowercase() }, { it.id }))
        .map { CanonicalParentOption(it.id, it.title) }
}

private fun String.required(label: String): String = trim().takeIf(String::isNotEmpty)
    ?: throw IllegalArgumentException("$label is required")

private fun String.requiredLong(label: String): Long = required(label).toLongOrNull()
    ?: throw IllegalArgumentException("$label must be a whole number")

private fun String.requiredInt(label: String): Int = required(label).toIntOrNull()
    ?: throw IllegalArgumentException("$label must be a whole number")

private fun String.optionalInt(label: String): Int? = trim().takeIf(String::isNotEmpty)?.let {
    it.toIntOrNull() ?: throw IllegalArgumentException("$label must be a whole number")
}

private fun String.optional(label: String): String? = trim().takeIf(String::isNotEmpty)?.also {
    runCatching { Instant.parse(it) }.getOrElse {
        throw IllegalArgumentException("$label must be an ISO-8601 instant")
    }
}

private fun CanonicalRecurrenceKind.editorLabel(): String = when (this) {
    CanonicalRecurrenceKind.DAILY -> "Daily"
    CanonicalRecurrenceKind.WEEKLY -> "Weekly"
    CanonicalRecurrenceKind.MONTHLY -> "Monthly"
    CanonicalRecurrenceKind.EVERY_INTERVAL -> "Every interval"
    CanonicalRecurrenceKind.AFTER_COMPLETION -> "After completion"
}
