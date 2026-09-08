package com.greengolddog.dayweave.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.produceState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import com.greengolddog.dayweave.sync.CanonicalSyncState
import com.greengolddog.dayweave.ui.authoring.CanonicalAuthoringPresentation
import com.greengolddog.dayweave.ui.authoring.CanonicalAuthoringRow
import com.greengolddog.dayweave.ui.authoring.CanonicalHierarchyBrowserPresentation
import com.greengolddog.dayweave.ui.authoring.CanonicalHierarchyBrowserRow
import com.greengolddog.dayweave.ui.authoring.CanonicalHierarchyRollupPresentation
import com.greengolddog.dayweave.ui.authoring.HierarchyRollupDisplay
import com.greengolddog.dayweave.ui.authoring.summaryLabel
import com.greengolddog.dayweave.ui.authoring.detailLabels
import com.greengolddog.dayweave.ui.authoring.CanonicalItemEditorRoute
import com.greengolddog.dayweave.ui.authoring.editorRoute
import com.greengolddog.dayweave.ui.authoring.canonicalDurationLabel
import com.greengolddog.dayweave.ui.authoring.canonicalTimingLabel
import com.greengolddog.dayweave.ui.authoring.canonicalStatusLabel
import com.greengolddog.dayweave.ui.authoring.canonicalBlockedReasonLabel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Mounted only in the private planner subtree; no content-bearing saved-instance state. */
@Composable
internal fun CanonicalHierarchyBrowserScreen(
    state: DayWeaveUiState,
    syncState: CanonicalSyncState,
    kind: ItemKind,
    actionsEnabled: Boolean,
    onOpenEditor: (CanonicalItemEditorRoute) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var query by remember(kind) { mutableStateOf("") }
    var collapsedIds by remember(kind) { mutableStateOf(emptySet<String>()) }
    var selectedId by remember(kind) { mutableStateOf<String?>(null) }
    val sourceState = remember(hierarchySourceKey(state)) { hierarchyAdmittedState(state) }
    val built by produceState<HierarchyBuildResult?>(null, sourceState) {
        // Full metadata/privacy admission can be expensive for very deep retained hierarchies.
        // Never do it on the UI thread, and never attach an old result to new source inputs.
        value = withContext(Dispatchers.Default) {
            HierarchyBuildResult(sourceState, runCatching {
                CanonicalAuthoringPresentation.build(sourceState)
            }.getOrNull(), runCatching {
                com.greengolddog.dayweave.model.CanonicalHierarchyParentAuthority.build(sourceState)
            }.getOrNull(), CanonicalHierarchyRollupPresentation.build(sourceState))
        }
    }
    val currentBuild = built?.takeIf { it.source == sourceState }
    val authoring = currentBuild?.presentation
    val parentIds = remember(authoring) { authoring?.hierarchyRows.orEmpty().mapNotNull { it.parentId }.toSet() }
    val presentation = remember(authoring, kind, query, collapsedIds) {
        CanonicalHierarchyBrowserPresentation.build(authoring?.hierarchyRows.orEmpty(), kind, query, collapsedIds)
    }
    val selected = remember(authoring, kind, selectedId) {
        selectedId?.let { id ->
            CanonicalHierarchyBrowserPresentation.build(authoring?.hierarchyRows.orEmpty(), kind).rows
                .find { it.item.itemId == id }?.item
        }
    }
    BackHandler { if (selectedId != null) selectedId = null else onBack() }
    Column(modifier.fillMaxSize().testTag("canonical_hierarchy_browser")) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = onBack, modifier = Modifier.testTag("hierarchy_back_to_more")) {
                Text("Back to More")
            }
            TextButton(
                onClick = { onOpenEditor(CanonicalItemEditorRoute.hierarchy(sourceState, kind)) },
                enabled = actionsEnabled,
                modifier = Modifier.testTag("hierarchy_create_root"),
            ) { Text(if (kind == ItemKind.GOAL) "New goal" else "New project") }
        }
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            label = { Text("Search ${if (kind == ItemKind.GOAL) "goals" else "projects"} and descendants") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp)
                .testTag("hierarchy_search"),
        )
        Text(
            hierarchyCacheMessage(state, syncState.phase),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp)
                .testTag("hierarchy_cache_status"),
        )
        LazyColumn(
            modifier = Modifier.weight(1f).testTag("hierarchy_rows"),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 88.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (presentation.rows.isEmpty()) {
                item {
                    Text(
                        when {
                            currentBuild == null -> "Reading the saved hierarchy…"
                            authoring == null -> "The saved hierarchy is unavailable. Local records have not been changed."
                            query.isNotBlank() -> "No matches in the available hierarchy."
                            state.canonicalDeltaCursor == null || state.canonicalConfigurationId == null ->
                                "This hierarchy is not fully loaded yet. Saved local drafts remain available."
                            else -> "No ${if (kind == ItemKind.GOAL) "goals" else "projects"} in this saved cache."
                        },
                        modifier = Modifier.testTag("hierarchy_empty"),
                    )
                }
            }
            items(presentation.rows, key = { it.item.itemId }) { row ->
                HierarchyRowCard(
                    row = row,
                    isStructuralParent = row.item.itemId in parentIds,
                    rollup = currentBuild?.rollups?.get(row.item.itemId) ?: HierarchyRollupDisplay.Incomplete,
                    onInspect = { selectedId = row.item.itemId },
                    onToggle = {
                        collapsedIds = if (row.item.itemId in collapsedIds) {
                            collapsedIds - row.item.itemId
                        } else {
                            collapsedIds + row.item.itemId
                        }
                    },
                    disclosureEnabled = query.isBlank(),
                )
            }
        }
    }
    if (selected != null) {
        HierarchyItemDetails(
            row = selected,
            isStructuralParent = selected.itemId in parentIds,
            rollup = currentBuild?.rollups?.get(selected.itemId) ?: HierarchyRollupDisplay.Incomplete,
            actionsEnabled = actionsEnabled,
            canAddChild = currentBuild?.parentAuthority?.let {
                it.issue(selected.itemId, "00000000-0000-4000-8000-000000000000") == null
            } == true &&
                !selected.hasUnsafeAncestry && !selected.hasMissingParent && !selected.hasHierarchyCycle,
            onAddChild = {
                val route = CanonicalItemEditorRoute.hierarchy(sourceState, ItemKind.TASK, selected.itemId)
                selectedId = null
                onOpenEditor(route)
            },
            onDismiss = { selectedId = null },
            onOpenEditor = { route ->
                selectedId = null
                onOpenEditor(route)
            },
        )
    }
}

private data class HierarchyBuildResult(
    val source: DayWeaveUiState,
    val presentation: CanonicalAuthoringPresentation?,
    val parentAuthority: com.greengolddog.dayweave.model.CanonicalHierarchyParentAuthority?,
    val rollups: CanonicalHierarchyRollupPresentation,
)

/** No wall clock, schedule, search, disclosure, selection, or connection phase in forest inputs. */
internal fun hierarchySourceKey(state: DayWeaveUiState): List<Any?> = listOf(
    state.canonicalConfigurationId, state.canonicalSyncOrigin, state.canonicalDeltaCursor,
    state.canonicalItems, state.pendingCanonicalAuthoringMutations, state.canonicalRecentlyDeleted,
    state.pendingCanonicalMutation, state.scheduleCompositionProfile, state.canonicalExecutionSession,
    state.terminalExecutionOutcomes, state.pendingProposalApplicationMutation, state.pendingExecutionCommand,
)

/** Never promote an unbound legacy canonical cache into an admitted hierarchy. */
internal fun hierarchyAdmittedState(state: DayWeaveUiState): DayWeaveUiState =
    if (state.canonicalConfigurationId != null) state else state.copy(
        canonicalItems = emptyList(),
        canonicalRecentlyDeleted = emptyList(),
        pendingCanonicalAuthoringMutations = state.pendingCanonicalAuthoringMutations.filter {
            it.operation == CanonicalAuthoringOperation.CREATE && it.configurationId == null &&
                it.syncOrigin == null && !it.isSubmitted
        },
    )

internal fun hierarchyCacheMessage(state: DayWeaveUiState, phase: CanonicalSyncPhase): String = when {
    state.canonicalConfigurationId == null ->
        "Canonical workspace not loaded. Local queued drafts are retained."
    state.canonicalDeltaCursor == null ->
        if (phase == CanonicalSyncPhase.SYNCING) "Loading the canonical hierarchy…" else
            "Initial hierarchy sync is incomplete. Available cached items and drafts are shown."
    phase == CanonicalSyncPhase.OFFLINE -> "Offline · saved encrypted hierarchy and queued drafts"
    phase == CanonicalSyncPhase.SYNCING -> "Refreshing · saved hierarchy and queued drafts remain available"
    phase != CanonicalSyncPhase.CONNECTED -> "Saved hierarchy · refresh unavailable; queued drafts are retained"
    else -> "Canonical hierarchy · includes unscheduled items and local queued changes"
}

@Composable
private fun HierarchyRowCard(
    row: CanonicalHierarchyBrowserRow,
    isStructuralParent: Boolean,
    rollup: HierarchyRollupDisplay,
    onInspect: () -> Unit,
    onToggle: () -> Unit,
    disclosureEnabled: Boolean,
) {
    val item = row.item
    Card(
        onClick = onInspect,
        modifier = Modifier.fillMaxWidth().padding(start = (row.depth.coerceAtMost(5) * 10).dp)
            .testTag("hierarchy_row_${item.itemId}")
            .semantics {
                stateDescription = listOfNotNull(
                    if (row.isSearchMatch) "Search match" else null,
                    if (row.isContext) "Parent context" else null,
                    if (row.hasChildren) {
                        if (row.isCollapsed) "Collapsed" else "Expanded"
                    } else null,
                    if (item.isReadOnly) "Read-only" else null,
                ).joinToString(" · ")
            },
    ) {
        Row(Modifier.padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(item.title, style = MaterialTheme.typography.titleSmall)
                Text(
                    hierarchyStateLabel(item),
                    style = MaterialTheme.typography.labelSmall,
                )
                if (row.isContext) Text("Parent context", style = MaterialTheme.typography.labelSmall)
                if (item.isSensitive) Text("Sensitive", style = MaterialTheme.typography.labelSmall)
                if (item.breadcrumb.isNotEmpty()) {
                    Text(
                        item.breadcrumb.takeLast(3).joinToString(" › "),
                        maxLines = 1, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Text(hierarchyStoredDurationLabel(item, isStructuralParent), style = MaterialTheme.typography.bodySmall)
                canonicalTimingLabel(item)?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                Text(rollup.summaryLabel(), style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag("hierarchy_rollup_${item.itemId}"))
                if (item.hasMissingParent) Text("Parent unavailable · read-only")
                if (item.hasHierarchyCycle) Text("Hierarchy cycle · read-only")
                if (item.hasUnsafeAncestry && !item.hasMissingParent && !item.hasHierarchyCycle) {
                    Text("Ancestor hierarchy unavailable or cyclic · read-only")
                }
            }
            if (row.hasChildren) {
                TextButton(
                    onClick = onToggle,
                    enabled = disclosureEnabled,
                    modifier = Modifier.testTag("hierarchy_disclosure_${item.itemId}")
                        .semantics {
                            contentDescription = "${if (row.isCollapsed) "Expand" else "Collapse"} ${item.title}"
                        },
                ) { Text(if (row.isCollapsed) "+" else "−") }
            }
        }
    }
}

private fun hierarchyStateLabel(row: CanonicalAuthoringRow): String =
    "${row.kind.label} · ${canonicalStatusLabel(row.status)} · " +
        row.syncState.name.lowercase().replaceFirstChar(Char::uppercase)

internal fun hierarchyStoredDurationLabel(row: CanonicalAuthoringRow, isStructuralParent: Boolean): String =
    if (isStructuralParent && row.kind != ItemKind.EVENT) {
        "Stored item estimate · ${canonicalDurationLabel(row)} · excluded from leaf totals"
    } else canonicalDurationLabel(row)

@Composable
private fun HierarchyItemDetails(
    row: CanonicalAuthoringRow,
    isStructuralParent: Boolean,
    rollup: HierarchyRollupDisplay,
    actionsEnabled: Boolean,
    canAddChild: Boolean,
    onAddChild: () -> Unit,
    onDismiss: () -> Unit,
    onOpenEditor: (CanonicalItemEditorRoute) -> Unit,
) {
    val route = row.editorRoute()
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(row.title) },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()).testTag("hierarchy_item_details"),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(hierarchyStateLabel(row))
                if (row.isSensitive) Text("Sensitive · protected by the current and pending hierarchy")
                if (row.breadcrumb.isNotEmpty()) Text(row.breadcrumb.takeLast(8).joinToString(" › "))
                Text(hierarchyStoredDurationLabel(row, isStructuralParent))
                canonicalTimingLabel(row)?.let { Text(it) }
                Text(rollup.summaryLabel(), modifier = Modifier.testTag("hierarchy_rollup_detail"))
                if (rollup is HierarchyRollupDisplay.Available) {
                    rollup.totals.detailLabels().forEach { Text(it) }
                }
                canonicalBlockedReasonLabel(row)?.let { Text(it) }
                row.notes?.takeIf(String::isNotBlank)?.let { Text(it) }
                if (row.hasMissingParent) Text("Parent unavailable. Hierarchy editing is read-only.")
                if (row.hasHierarchyCycle) Text("Hierarchy cycle. Hierarchy editing is read-only.")
                if (row.hasUnsafeAncestry && !row.hasMissingParent && !row.hasHierarchyCycle) {
                    Text("An ancestor is missing or cyclic. Hierarchy editing is read-only.")
                }
                row.diagnostic?.let { Text(it) }
                if (route == null) Text("Read-only. Existing canonical editing restrictions apply.")
            }
        },
        confirmButton = {
            Column {
                if (canAddChild) {
                    TextButton(onClick = onAddChild, enabled = actionsEnabled,
                        modifier = Modifier.testTag("hierarchy_add_child")) { Text("Add subtask") }
                }
                if (route != null) {
                    TextButton(
                        onClick = { onOpenEditor(route) },
                        enabled = actionsEnabled,
                        modifier = Modifier.testTag("hierarchy_edit"),
                    ) { Text("Review / edit") }
                }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Close") } },
    )
}
