package com.greengolddog.dayweave.ui.authoring

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.greengolddog.dayweave.model.RoutinePlanningDisplayAdmission

/** UI projection only. Exact helper bytes and timestamp strings remain in encrypted custody. */
internal data class RoutinePlanningPreviewRow(val title: String, val detail: String)
internal data class RoutinePlanningPreviewPresentation(val capturedAt: String, val computedAt: String,
    val timezone: String, val horizon: String, val blocks: List<RoutinePlanningPreviewRow>,
    val unscheduled: List<RoutinePlanningPreviewRow>, val diagnostics: List<RoutinePlanningPreviewRow>,
    val members: List<RoutinePlanningPreviewRow>)

internal fun RoutinePlanningDisplayAdmission.previewPresentation(): RoutinePlanningPreviewPresentation {
    val sources = capsule.canonicalItems.associateBy { it.id }
    val plan = composed.composition.plan
    return RoutinePlanningPreviewPresentation(capsule.capturedAt, display.computedAt, capsule.witness.schedule.timezoneName,
        "${plan.horizonStart} — ${plan.horizonEnd}", plan.blocks.map { block ->
            RoutinePlanningPreviewRow(block.title, "${block.start} — ${block.end}\n${block.kind}" +
                (block.occurrenceId?.let { " · occurrence $it" } ?: "") +
                block.explanations.joinToString(separator = "", prefix = "") { "\n${it.message}" })
        }, plan.unscheduled.map { work -> RoutinePlanningPreviewRow(sources[work.itemId]?.title ?: "Unscheduled work",
            "${work.remaining} minutes · ${work.reason}\n${work.message}" + (work.occurrenceId?.let { "\nOccurrence $it" } ?: "")) },
        plan.decisions.map { decision -> RoutinePlanningPreviewRow("${sources[decision.itemId]?.title ?: "Planning decision"} · ${decision.kind}",
            decision.message + (decision.occurrenceId?.let { "\nOccurrence $it" } ?: "")) } +
            plan.violations.map { violation -> RoutinePlanningPreviewRow("${violation.severity} · ${violation.kind}",
                violation.message + "\n" + violation.itemIds.mapNotNull { sources[it]?.title }.joinToString(", ") +
                    "\n" + violation.occurrenceIds.joinToString(", ") + (violation.start?.let { "\n$it — ${violation.end.orEmpty()}" } ?: "")) } +
            composed.composition.ignoredPreviousAssignments.map { ignored -> RoutinePlanningPreviewRow(
                sources[ignored.itemId]?.title ?: "Previous assignment", "${ignored.reason}\nRequested revision ${ignored.requestedRevision}; current ${ignored.currentRevision ?: "unavailable"}") },
        capsule.witness.occurrenceLifecycle.instances.flatMap { instance -> instance.members.map { member ->
            RoutinePlanningPreviewRow(sources[member.itemId]?.title ?: "Occurrence member",
                "${member.status.replace('_', ' ')} · source revision ${member.sourceRevision}\nOccurrence ${instance.occurrenceId}" +
                    (member.parentId?.let { "\nParent: ${sources[it]?.title ?: it}" } ?: "\nRoot"))
        } })
}

/** A secure, foreground-only, actionless surface—even when every source is canonically public. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RoutinePlanningPreviewSheet(preview: RoutinePlanningPreviewPresentation?, onDismiss: () -> Unit) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var foreground by remember(lifecycle) { mutableStateOf(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) }
    val dismiss by rememberUpdatedState(onDismiss)
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, _ ->
            foreground = lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
            if (!foreground) dismiss()
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    if (!foreground || preview == null) return
    ModalBottomSheet(onDismissRequest = onDismiss, properties = ModalBottomSheetProperties(securePolicy = SecureFlagPolicy.SecureOn)) {
        LazyColumn(Modifier.fillMaxWidth().padding(horizontal = 20.dp).testTag("routine_planning_preview"),
            contentPadding = PaddingValues(bottom = 28.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item {
                Text("Private fixed-input preview", style = MaterialTheme.typography.headlineSmall)
                Text("Display only · execution and changes are locked. The canonical schedule is unchanged.")
                Text("Input clock: ${preview.capturedAt}\nComputed: ${preview.computedAt}\n${preview.timezone}\n${preview.horizon}")
                Text("${preview.blocks.size} blocks · ${preview.unscheduled.size} unscheduled · ${preview.diagnostics.size} planning diagnostics")
            }
            itemsIndexed(preview.blocks) { index, row ->
                Column(Modifier.testTag("routine_preview_block_$index")) { Text(row.title, style = MaterialTheme.typography.titleMedium); Text(row.detail) }
            }
            itemsIndexed(preview.unscheduled) { index, row ->
                Column(Modifier.testTag("routine_preview_unscheduled_$index")) { Text(row.title, style = MaterialTheme.typography.titleMedium); Text(row.detail) }
            }
            if (preview.diagnostics.isNotEmpty()) item { Text("Planning explanations", style = MaterialTheme.typography.titleLarge) }
            itemsIndexed(preview.diagnostics) { index, row ->
                Column(Modifier.testTag("routine_preview_diagnostic_$index")) { Text(row.title, style = MaterialTheme.typography.titleMedium); Text(row.detail) }
            }
            if (preview.members.isNotEmpty()) item { Text("Complete occurrence membership", style = MaterialTheme.typography.titleLarge) }
            itemsIndexed(preview.members) { index, row ->
                Column(Modifier.testTag("routine_preview_member_$index")) { Text(row.title, style = MaterialTheme.typography.titleMedium); Text(row.detail) }
            }
            item { TextButton(onClick = onDismiss, modifier = Modifier.testTag("routine_preview_close")) { Text("Close") } }
        }
    }
}
