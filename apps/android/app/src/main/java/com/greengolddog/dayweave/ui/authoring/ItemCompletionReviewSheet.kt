package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.sync.ItemCompletionSyncState
import kotlinx.coroutines.launch

/** No saveable form or unprotected content-bearing recovery surface. Polling never replaces edits. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ItemCompletionReviewSheet(
    state: DayWeaveUiState, itemId: String, syncState: ItemCompletionSyncState, actionsEnabled: Boolean,
    onObserve: suspend (String) -> Unit,
    onSave: suspend (String, ItemCompletionSnapshot, Boolean, ItemCompletionMode, String?) -> Boolean,
    onRetry: () -> Unit, onDiscardReviewed: (String) -> Unit, onDismiss: () -> Unit,
) {
    val item = state.completionItem(itemId)
    val ledger = state.itemCompletionLedger.takeIf {
        it.syncOrigin == state.canonicalSyncOrigin && it.configurationId == state.canonicalConfigurationId
    }
    val pending = ledger?.pending?.singleOrNull { it.itemId == itemId }
    val proof = state.currentCompletionProof(itemId)
    val issue = state.completionReviewIssue(itemId)
    var reviewed by remember(itemId, state.canonicalConfigurationId) { mutableStateOf<ItemCompletionSnapshot?>(null) }
    var required by remember(itemId, state.canonicalConfigurationId) { mutableStateOf(true) }
    var mode by remember(itemId, state.canonicalConfigurationId) { mutableStateOf(ItemCompletionMode.AUTOMATIC) }
    var replacing by remember(itemId, state.canonicalConfigurationId) { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    val sensitive = state.completionReviewSensitive(itemId) || ledger?.needsCanonicalCatchUp == true ||
        proof == null || reviewed != null && reviewed != proof.snapshot
    var reviewedSensitive by remember(itemId, state.canonicalConfigurationId) { mutableStateOf(sensitive) }
    SideEffect { if (sensitive) reviewedSensitive = true }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(itemId, state.canonicalConfigurationId, item != null, lifecycle) {
        if (item != null) lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { onObserve(itemId) }
    }
    fun reviewLatest() {
        val snapshot = proof?.snapshot ?: return
        if (reviewed == null) {
            required = pending?.request()?.requiredForParent ?: snapshot.state.requiredForParent
            mode = pending?.request()?.mode ?: snapshot.state.mode
        }
        reviewed = snapshot
        replacing = pending?.operationId
        failed = false
    }
    LaunchedEffect(proof, pending, issue) { if (reviewed == null && proof != null && pending == null && issue == null) reviewLatest() }
    val currentReview = reviewed != null && reviewed == proof?.snapshot && pending?.operationId == replacing && issue == null
    val canOverride = state.canonicalItems.any { it.deletedAt == null && it.parentId == itemId } ||
        proof?.snapshot?.state?.provenance != null || proof?.snapshot?.state?.mode?.let { it != ItemCompletionMode.AUTOMATIC } == true
    val scope = rememberCoroutineScope()
    ModalBottomSheet(onDismissRequest = { if (!saving) onDismiss() }, properties = ModalBottomSheetProperties(
        securePolicy = canonicalEditorSecurePolicy(sensitive, false, reviewedSensitive))) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(20.dp).testTag("item_completion_sheet"),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Completion policy", style = MaterialTheme.typography.headlineSmall)
            Text("Protected evidence · descendant privacy can change remotely.")
            Text(syncState.message, modifier = Modifier.testTag("item_completion_status"))
            if (item == null) {
                Text("This item or its protected hierarchy is unavailable. Saved intent remains protected.",
                    modifier = Modifier.testTag("item_completion_protected_recovery"))
            } else {
                Text(item.title, style = MaterialTheme.typography.titleMedium)
                Text("Parent lifecycle is separate from independent progress and executable work. " +
                    "Complete does not waive unfinished required descendants.")
                proof?.snapshot?.let { snapshot ->
                    Text("Current required descendants · ${snapshot.counts.completed} completed · " +
                        "${snapshot.counts.incomplete} incomplete · ${snapshot.counts.occurrenceEvidenceRequired} need occurrence evidence")
                    Text("Current policy · ${snapshot.state.mode.label()} · revision ${snapshot.state.revision}")
                    if (snapshot.occurrenceEvidenceRequired) Text("Recurring completion evidence is unresolved; manual completion is unavailable.")
                } ?: Text("Refresh current evidence before reviewing. Saved write receipts are historical, not current proof.")
                issue?.let { Text(it, modifier = Modifier.testTag("item_completion_issue")) }
                if (proof != null && issue == null) {
                    TextButton(onClick = ::reviewLatest, enabled = !saving, modifier = Modifier.testTag("item_completion_review_latest")) {
                        Text(if (pending == null) "Review latest evidence" else "Review saved intent against latest evidence")
                    }
                }
                if (reviewed != null) {
                    Row {
                        Checkbox(checked = required, onCheckedChange = { required = it }, enabled = !saving,
                            modifier = Modifier.testTag("item_completion_required"))
                        Text("Required for parent completion")
                    }
                    ItemCompletionMode.entries.forEach { choice ->
                        FilterChip(selected = mode == choice, onClick = { mode = choice }, label = { Text(choice.label()) },
                            enabled = !saving && (choice == ItemCompletionMode.AUTOMATIC || canOverride) &&
                                (choice != ItemCompletionMode.COMPLETE || reviewed?.occurrenceEvidenceRequired == false),
                            modifier = Modifier.testTag("item_completion_mode_${choice.name.lowercase()}"))
                    }
                    Text("Automatic follows required descendants. Keep open prevents automatic completion. " +
                        "Complete records an explicit parent override. Legacy terminal repair is not available here.")
                    if (!currentReview) Text("Evidence changed. Your fields are preserved; review the latest evidence explicitly.")
                    Button(onClick = {
                        val snapshot = reviewed ?: return@Button
                        saving = true
                        scope.launch { try { failed = !onSave(itemId, snapshot, required, mode, replacing) } finally { saving = false } }
                    }, enabled = actionsEnabled && currentReview && !saving && !syncState.isBusy,
                        modifier = Modifier.testTag("item_completion_save")) { Text("Save reviewed completion change") }
                    if (failed) Text("The change was not saved. Keep this review and retry after refreshing evidence.")
                }
            }
            if (pending != null) {
                Text("Saved operation · …${pending.operationId.takeLast(8)} · ${pending.disposition.name.lowercase().replace('_', ' ')}")
                if (pending.disposition == ItemCompletionDisposition.PENDING) {
                    Text("Its exact bytes remain saved until an authoritative outcome is known.")
                    TextButton(onClick = onRetry, enabled = actionsEnabled && !syncState.isBusy,
                        modifier = Modifier.testTag("item_completion_retry")) { Text("Retry exact saved change") }
                } else {
                    TextButton(onClick = { onDiscardReviewed(pending.operationId) }, enabled = actionsEnabled && !syncState.isBusy,
                        modifier = Modifier.testTag("item_completion_discard")) { Text("Discard reviewed intent") }
                }
            }
            TextButton(onClick = onRetry, enabled = actionsEnabled && !syncState.isBusy) { Text("Refresh current evidence") }
            TextButton(onClick = onDismiss, enabled = !saving) { Text("Close") }
        }
    }
}

private fun ItemCompletionMode.label(): String = when (this) {
    ItemCompletionMode.AUTOMATIC -> "Automatic"
    ItemCompletionMode.KEEP_OPEN -> "Keep open"
    ItemCompletionMode.COMPLETE -> "Complete"
}
