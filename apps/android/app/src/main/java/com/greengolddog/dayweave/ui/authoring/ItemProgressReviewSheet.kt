package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.ModalBottomSheetProperties
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.sync.ItemProgressSyncState
import java.math.BigDecimal
import java.util.UUID
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

internal enum class ItemProgressFormKind(val label: String) { PERCENTAGE("Percentage"), TIME("Time"), QUANTITY("Quantity") }

/** Content-bearing form state stays only in the protected view, never saved-instance state. */
internal data class ItemProgressComponentForm(
    val id: String,
    val name: String = "",
    val kind: ItemProgressFormKind,
    val percentage: String = "0",
    val elapsedSeconds: String = "0",
    val remainingSeconds: String = "",
    val current: String = "0",
    val unit: String = "",
    val target: String = "",
    val direction: ItemProgressDirection = ItemProgressDirection.AT_LEAST,
) {
    fun component(): ItemProgressComponent = ItemProgressComponent(id, name, when (kind) {
        ItemProgressFormKind.PERCENTAGE -> {
            require(Regex("[0-9]{1,3}(?:\\.[0-9]{1,2})?").matches(percentage))
            ItemProgressValue.Percentage(BigDecimal(percentage).movePointRight(2).intValueExact())
        }
        ItemProgressFormKind.TIME -> ItemProgressValue.Time(elapsedSeconds.progressSeconds(),
            remainingSeconds.takeIf { it.isNotEmpty() }?.progressSeconds())
        ItemProgressFormKind.QUANTITY -> ItemProgressValue.Quantity(current.progressDecimal(), unit,
            target.takeIf { it.isNotEmpty() }?.let { ItemProgressTarget(it.progressDecimal(), direction) })
    }).also(ItemProgressComponent::requireValid)

    companion object {
        fun from(component: ItemProgressComponent): ItemProgressComponentForm = when (val value = component.value) {
            is ItemProgressValue.Percentage -> ItemProgressComponentForm(component.id, component.name, ItemProgressFormKind.PERCENTAGE,
                percentage = BigDecimal.valueOf(value.basisPoints.toLong(), 2).toPlainString())
            is ItemProgressValue.Time -> ItemProgressComponentForm(component.id, component.name, ItemProgressFormKind.TIME,
                elapsedSeconds = value.elapsedSeconds.toString(), remainingSeconds = value.remainingSeconds?.toString().orEmpty())
            is ItemProgressValue.Quantity -> ItemProgressComponentForm(component.id, component.name, ItemProgressFormKind.QUANTITY,
                current = value.current, unit = value.unit, target = value.target?.value.orEmpty(),
                direction = value.target?.direction ?: ItemProgressDirection.AT_LEAST)
        }
    }
}

private fun String.progressSeconds(): Long {
    require(Regex("[0-9]+").matches(this))
    return toLong().also { require(it in 0..MAX_ITEM_PROGRESS_SECONDS) }
}

private fun String.progressDecimal(): String {
    require(length <= 64 && Regex("-?[0-9]+(?:\\.[0-9]+)?").matches(this))
    return BigDecimal(this).stripTrailingZeros().toPlainString().also(::requireCanonicalProgressDecimal)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ItemProgressReviewSheet(
    state: DayWeaveUiState,
    itemId: String,
    syncState: ItemProgressSyncState,
    actionsEnabled: Boolean,
    onLoad: suspend (String) -> Boolean,
    onSave: suspend (String, Long, Long, List<ItemProgressComponent>, String?) -> Boolean,
    onRetry: () -> Unit,
    onDiscardReviewed: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val item = remember(state.canonicalItems, state.canonicalConfigurationId, state.canonicalSyncOrigin,
        state.canonicalDeltaCursor, itemId) { state.progressItem(itemId) }
    val ledger = state.itemProgressLedger.takeIf {
        it.configurationId == state.canonicalConfigurationId && it.syncOrigin == state.canonicalSyncOrigin
    }
    val observation = ledger?.observations?.get(itemId)
    val pending = ledger?.pending?.singleOrNull { it.itemId == itemId }
    var reviewed by remember(itemId, state.canonicalConfigurationId) { mutableStateOf<ItemProgressSnapshot?>(null) }
    var forms by remember(itemId, state.canonicalConfigurationId) { mutableStateOf<List<ItemProgressComponentForm>?>(null) }
    var replacing by remember(itemId, state.canonicalConfigurationId) { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    val sensitive = remember(state.canonicalItems, state.pendingCanonicalMutation, state.pendingCanonicalAuthoringMutations,
        state.itemProgressLedger.pending, itemId) { state.progressReviewSensitive(itemId) }
    var reviewedSensitive by remember(itemId, state.canonicalConfigurationId) { mutableStateOf(sensitive) }
    SideEffect { if (sensitive) reviewedSensitive = true }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(itemId, state.canonicalConfigurationId, state.canonicalSyncOrigin, item != null, lifecycle) {
        if (item == null) return@LaunchedEffect
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            var failures = 0
            while (isActive) {
                val success = onLoad(itemId)
                failures = if (success) 0 else (failures + 1).coerceAtMost(4)
                delay(if (success) 5_000L else 5_000L * (1L shl failures))
            }
        }
    }
    val issue = state.progressReviewIssue(itemId)
    LaunchedEffect(observation, pending, issue) {
        if (forms == null && observation?.isGetProof == true && issue == null && pending == null) {
            reviewed = observation.snapshot
            forms = observation.snapshot.components.map(ItemProgressComponentForm::from)
        }
    }
    val scope = rememberCoroutineScope()
    val parsed = forms?.let { runCatching { it.map(ItemProgressComponentForm::component).also(::requireProgressComponents) } }
    val currentReview = reviewed?.let { baseline -> observation?.isGetProof == true &&
        observation.snapshot.itemRevision == baseline.itemRevision && observation.snapshot.revision == baseline.revision &&
        item?.revision == baseline.itemRevision } == true
    ModalBottomSheet(onDismissRequest = { if (!saving) onDismiss() },
        properties = ModalBottomSheetProperties(securePolicy = canonicalEditorSecurePolicy(sensitive, false, reviewedSensitive))) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(20.dp).testTag("item_progress_sheet"),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Independent progress", style = MaterialTheme.typography.headlineSmall)
            if (item == null) {
                Text("This item or its protected hierarchy is no longer available.")
                Text(syncState.message, modifier = Modifier.testTag("item_progress_sync_status"))
                if (pending != null) {
                    Text("Saved progress intent is protected. Component names and values remain hidden.",
                        modifier = Modifier.testTag("item_progress_protected_recovery"))
                    if (pending.disposition == ItemProgressDisposition.PENDING) {
                        Text("Retry the exact saved change to determine its outcome.")
                        TextButton(onClick = onRetry, enabled = actionsEnabled && !syncState.isBusy,
                            modifier = Modifier.testTag("item_progress_retry")) { Text("Retry exact saved change") }
                    } else {
                        Text("The saved change was rejected. Restore the item before a new review, or discard this intent.")
                        TextButton(onClick = { onDiscardReviewed(pending.operationId) },
                            enabled = actionsEnabled && !syncState.isBusy,
                            modifier = Modifier.testTag("item_progress_discard")) { Text("Discard reviewed intent") }
                    }
                }
            } else {
                Text(item.title, style = MaterialTheme.typography.titleMedium)
                Text("Self-reported components · separate from descendant totals, goal measures, timers and scheduling. " +
                    "Saving never completes this item.")
                Text(syncState.message, modifier = Modifier.testTag("item_progress_sync_status"))
                observation?.let {
                    Text(if (it.isGetProof) "Saved observation · revision ${it.snapshot.revision} · ${it.observedAt}" else
                        "Historical write confirmed · load the current progress before a new edit.")
                    if (it.snapshot.components.isEmpty()) Text("Saved observation: no independent components recorded.")
                    it.snapshot.components.forEach { component ->
                        Text("Saved · ${component.name} · ${progressValueLabel(component.value)}")
                    }
                }
                if (pending != null) {
                    Text(if (pending.disposition == ItemProgressDisposition.PENDING) {
                        "Exact saved change awaiting confirmation. These queued values are not confirmed current progress."
                    } else "Saved intent requires explicit review. Refreshing does not erase it.",
                        modifier = Modifier.testTag("item_progress_pending"))
                    pending.request().components.forEach { Text("Queued · ${it.name} · ${progressValueLabel(it.value)}") }
                    if (pending.disposition == ItemProgressDisposition.PENDING) {
                        TextButton(onClick = onRetry, enabled = actionsEnabled && !syncState.isBusy,
                            modifier = Modifier.testTag("item_progress_retry")) { Text("Retry exact saved change") }
                    } else {
                        TextButton(onClick = { onDiscardReviewed(pending.operationId); forms = null; reviewed = null; replacing = null },
                            enabled = actionsEnabled && !syncState.isBusy,
                            modifier = Modifier.testTag("item_progress_discard")) { Text("Discard reviewed intent") }
                    }
                }
                issue?.let { Text(it, modifier = Modifier.testTag("item_progress_issue")) }
                if (observation?.isGetProof == true && issue == null) {
                    TextButton(onClick = {
                        reviewed = observation.snapshot
                        forms = (pending?.request()?.components ?: observation.snapshot.components).map(ItemProgressComponentForm::from)
                        replacing = pending?.operationId
                        message = null
                    }, enabled = !saving, modifier = Modifier.testTag("item_progress_review_latest")) {
                        Text(if (pending == null) "Review latest saved observation" else "Review saved intent against latest observation")
                    }
                }
                if (forms != null) {
                    if (!currentReview) Text("The reviewed baseline changed. Review the latest observation before saving.")
                    forms.orEmpty().forEachIndexed { index, form ->
                        ProgressComponentFields(form, index, !saving, onChange = { replacement ->
                            forms = forms.orEmpty().mapIndexed { position, previous -> if (position == index) replacement else previous }
                        }, onRemove = { forms = forms.orEmpty().filterIndexed { position, _ -> position != index } })
                    }
                    if (forms.orEmpty().size < 16) {
                        ItemProgressFormKind.entries.forEach { kind ->
                            TextButton(onClick = { forms = forms.orEmpty() + ItemProgressComponentForm(UUID.randomUUID().toString(), kind = kind) },
                                enabled = !saving, modifier = Modifier.testTag("item_progress_add_${kind.name.lowercase()}")) { Text("Add ${kind.label.lowercase()}") }
                        }
                    }
                    if (parsed?.isFailure == true) Text("Check component names, values and units. Percentages allow two decimal places; quantities allow six.")
                    Button(onClick = {
                        val baseline = reviewed ?: return@Button
                        val values = parsed?.getOrNull() ?: return@Button
                        saving = true
                        scope.launch {
                            try {
                                val saved = onSave(itemId, baseline.itemRevision, baseline.revision, values, replacing)
                                if (saved) { forms = null; reviewed = null; replacing = null }
                                message = if (saved) "Exact progress change saved securely." else "Could not save. Your review has been retained."
                            } finally { saving = false }
                        }
                    }, enabled = actionsEnabled && !syncState.isBusy && !saving && currentReview && issue == null && parsed?.isSuccess == true,
                        modifier = Modifier.testTag("item_progress_save")) { Text("Save independent progress") }
                }
                message?.let { Text(it) }
            }
            TextButton(onClick = onDismiss, enabled = !saving) { Text("Close") }
        }
    }
}

@Composable
private fun ProgressComponentFields(form: ItemProgressComponentForm, index: Int, enabled: Boolean,
    onChange: (ItemProgressComponentForm) -> Unit, onRemove: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("${index + 1}. ${form.kind.label}", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(form.name, { onChange(form.copy(name = it)) }, label = { Text("Component name") },
            enabled = enabled, modifier = Modifier.fillMaxWidth().testTag("item_progress_name_$index"))
        when (form.kind) {
            ItemProgressFormKind.PERCENTAGE -> OutlinedTextField(form.percentage, { onChange(form.copy(percentage = it)) },
                label = { Text("Percent · 0 to 100") }, enabled = enabled,
                modifier = Modifier.fillMaxWidth().testTag("item_progress_percentage_$index"))
            ItemProgressFormKind.TIME -> {
                OutlinedTextField(form.elapsedSeconds, { onChange(form.copy(elapsedSeconds = it)) }, label = { Text("Recorded elapsed seconds") },
                    enabled = enabled, modifier = Modifier.fillMaxWidth().testTag("item_progress_elapsed_$index"))
                OutlinedTextField(form.remainingSeconds, { onChange(form.copy(remainingSeconds = it)) }, label = { Text("Remaining seconds · blank if unknown") },
                    enabled = enabled, modifier = Modifier.fillMaxWidth().testTag("item_progress_remaining_$index"))
            }
            ItemProgressFormKind.QUANTITY -> {
                OutlinedTextField(form.current, { onChange(form.copy(current = it)) }, label = { Text("Current quantity") }, enabled = enabled,
                    modifier = Modifier.fillMaxWidth().testTag("item_progress_current_$index"))
                OutlinedTextField(form.unit, { onChange(form.copy(unit = it)) }, label = { Text("Unit") }, enabled = enabled,
                    modifier = Modifier.fillMaxWidth().testTag("item_progress_unit_$index"))
                OutlinedTextField(form.target, { onChange(form.copy(target = it)) }, label = { Text("Target · blank if none") }, enabled = enabled,
                    modifier = Modifier.fillMaxWidth().testTag("item_progress_target_$index"))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ItemProgressDirection.entries.forEach { direction -> FilterChip(form.direction == direction,
                        onClick = { onChange(form.copy(direction = direction)) }, enabled = enabled,
                        label = { Text(if (direction == ItemProgressDirection.AT_LEAST) "At least" else "At most") }) }
                }
            }
        }
        TextButton(onClick = onRemove, enabled = enabled) { Text("Remove component") }
    }
}

internal fun progressValueLabel(value: ItemProgressValue): String = when (value) {
    is ItemProgressValue.Percentage -> "${BigDecimal.valueOf(value.basisPoints.toLong(), 2).toPlainString()}%"
    is ItemProgressValue.Time -> "${value.elapsedSeconds}s recorded · ${value.remainingSeconds?.let { "${it}s remaining" } ?: "remaining unknown"}"
    is ItemProgressValue.Quantity -> "${value.current} ${value.unit}" + (value.target?.let {
        " · ${if (it.direction == ItemProgressDirection.AT_LEAST) "at least" else "at most"} ${it.value}"
    } ?: " · no target")
}
