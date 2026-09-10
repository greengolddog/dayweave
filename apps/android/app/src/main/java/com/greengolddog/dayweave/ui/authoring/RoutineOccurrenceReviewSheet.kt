package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.sync.RoutineOccurrenceSelection
import com.greengolddog.dayweave.sync.RoutineOccurrenceSyncState
import kotlinx.coroutines.launch

/** Every title, blocker, status and journal remains inside a secure, foreground-only sheet. */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
internal fun RoutineOccurrenceReviewSheet(
    state: DayWeaveUiState, selection: RoutineOccurrenceSelection, sync: RoutineOccurrenceSyncState,
    actionsEnabled: Boolean, onObserve: suspend (RoutineOccurrenceSelection) -> Unit,
    onSave: suspend (RoutineOccurrenceSelection, RoutineOccurrenceSnapshot, String, RoutineOccurrenceAction) -> Boolean,
    onRetry: (Boolean) -> Unit, onDiscard: (String) -> Unit, onDismiss: () -> Unit,
) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var foreground by remember(lifecycle) { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) }
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, _ -> foreground = lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED) }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(selection, state.canonicalConfigurationId, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { onObserve(selection) }
    }
    val ledger = state.routineOccurrenceLedger.takeIf {
        foreground && sync.selection == selection &&
        it.syncOrigin == state.canonicalSyncOrigin && it.configurationId == state.canonicalConfigurationId
    }
    val live = sync.reviewed?.takeIf { foreground && sync.selection == selection }
    val snapshot = live ?: ledger?.observations?.values?.firstOrNull {
        it.snapshot.aggregate.manifest.seriesItemId == selection.seriesItemId &&
            it.snapshot.aggregate.manifest.occurrenceId == selection.occurrenceId
    }?.snapshot
    val pending = snapshot?.let { value -> ledger?.pending?.singleOrNull { it.instanceId == value.aggregate.manifest.id } }
    val rows = remember(snapshot) { snapshot?.reviewRows().orEmpty() }
    var saving by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val current = actionsEnabled && !saving && !sync.isBusy && live != null &&
        ledger?.needsRemoteScheduleCatchUp == false && ledger.minimumCatchUpRevisions.isEmpty() && !state.hasCompletionMutationBlocker()
    fun save(row: RoutineOccurrenceRow, action: RoutineOccurrenceAction) {
        val reviewed = live ?: return
        saving = true
        scope.launch {
            try { failed = !onSave(selection, reviewed, row.definition.itemId, action) }
            finally { saving = false }
        }
    }
    ModalBottomSheet(onDismissRequest = { if (!saving) onDismiss() },
        properties = ModalBottomSheetProperties(securePolicy = SecureFlagPolicy.SecureOn)) {
        LazyColumn(Modifier.fillMaxWidth().padding(horizontal = 20.dp).testTag("routine_occurrence_sheet"),
            contentPadding = PaddingValues(bottom = 28.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item {
                Text("This occurrence", style = MaterialTheme.typography.headlineSmall)
                Text("Private history · changes apply to this exact occurrence.")
                Text(sync.message, modifier = Modifier.testTag("routine_occurrence_status"))
                if (snapshot == null) Text("This occurrence has not been admitted or its history is unavailable. Refresh to resolve its exact identity.")
                else Text("${snapshot.aggregate.manifest.nominalStart} · revision ${snapshot.aggregate.revision}")
                if (live == null && snapshot != null) Text("Saved history · refresh current evidence before making a change.")
                if (live?.freshEditEligible == false) Text("The current definition or execution no longer permits a fresh edit. Saved history remains available.")
                if (ledger?.needsRemoteScheduleCatchUp == true) Text("Remote schedule catch-up is required before another change.")
                if (failed) Text("The change was not saved. Refresh and review the current evidence.")
            }
            pending?.let { saved -> item {
                Text("Saved change · ${saved.disposition.name.lowercase().replace('_', ' ')}")
                Text(if (saved.submittedAt != null && saved.disposition == RoutineOccurrenceDisposition.PENDING)
                    "The reply is unresolved. Retry sends the exact saved request." else "Review this saved intent before discarding it.")
                TextButton(onClick = { onRetry(false) }, enabled = actionsEnabled && !saving && !sync.isBusy,
                    modifier = Modifier.testTag("routine_occurrence_retry")) { Text("Retry exact saved change") }
                if (saved.submittedAt == null || saved.disposition != RoutineOccurrenceDisposition.PENDING) {
                    TextButton(onClick = { onDiscard(saved.operationId) }, enabled = actionsEnabled && !saving && !sync.isBusy,
                        modifier = Modifier.testTag("routine_occurrence_discard")) { Text("Discard saved intent") }
                }
            } }
            items(rows, key = { it.definition.itemId }) { row ->
                Column(Modifier.fillMaxWidth().padding(start = (row.depth.coerceAtMost(6) * 12).dp)
                    .testTag("routine_member_${row.definition.itemId}"), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(row.definition.title, style = MaterialTheme.typography.titleMedium)
                    Text("${row.state.status.replace('_', ' ')} · depth ${row.depth}")
                    val counts = row.evaluation.counts
                    Text("Required descendants: ${counts.completed} done · ${counts.incomplete} incomplete · ${counts.occurrenceEvidenceRequired} need separate evidence")
                    if (row.state.open.status == "blocked") Text("Reopens blocked: ${row.state.open.blockedReason ?: "Waiting on a dependency"}")
                    row.state.completedAt?.let { Text("Completed $it") }
                    val enabled = row.canChange(current, live?.freshEditEligible == true, pending != null)
                    if (row.evaluation.occurrenceEvidenceRequired) Text("Independent recurrence requires its own qualified occurrence evidence.")
                    if (!row.isParent) {
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            TextButton(onClick = { save(row, RoutineOccurrenceAction.SetOutcome("completed")) }, enabled = enabled,
                                modifier = Modifier.testTag("routine_done_${row.definition.itemId}")) { Text("Done") }
                            TextButton(onClick = { save(row, RoutineOccurrenceAction.SetOutcome("skipped")) }, enabled = enabled,
                                modifier = Modifier.testTag("routine_skip_${row.definition.itemId}")) { Text("Skipped") }
                            TextButton(onClick = { save(row, RoutineOccurrenceAction.Reopen(row.state.open)) },
                                enabled = enabled && row.state.status !in setOf("inbox", "planned", "blocked"),
                                modifier = Modifier.testTag("routine_reopen_${row.definition.itemId}")) { Text("Reopen exactly") }
                        }
                    }
                    if (row.definition.parentId != null) Row {
                        Checkbox(checked = row.state.requiredForParent,
                            onCheckedChange = { save(row, RoutineOccurrenceAction.SetPolicy(it, row.state.mode)) }, enabled = enabled,
                            modifier = Modifier.testTag("routine_required_${row.definition.itemId}"))
                        Text("Required for parent completion")
                    }
                    if (row.isParent) {
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            ItemCompletionMode.entries.forEach { mode ->
                                FilterChip(selected = row.state.mode == mode,
                                    onClick = { save(row, RoutineOccurrenceAction.SetPolicy(row.state.requiredForParent, mode)) },
                                    enabled = enabled, label = { Text(when (mode) {
                                        ItemCompletionMode.AUTOMATIC -> "Automatic"
                                        ItemCompletionMode.KEEP_OPEN -> "Keep open"
                                        ItemCompletionMode.COMPLETE -> "Complete"
                                    }) })
                            }
                        }
                        Text("Parent completion keeps unfinished children and their timers independent.")
                    }
                    HorizontalDivider()
                }
            }
            item {
                TextButton(onClick = { onRetry(false) }, enabled = actionsEnabled && !sync.isBusy && !saving) { Text("Refresh history and schedule") }
                TextButton(onClick = { onRetry(true) }, enabled = actionsEnabled && !sync.isBusy && !saving,
                    modifier = Modifier.testTag("routine_occurrence_cold_refresh")) { Text("Recover from full history") }
                TextButton(onClick = onDismiss, enabled = !saving) { Text("Close") }
            }
        }
    }
}
