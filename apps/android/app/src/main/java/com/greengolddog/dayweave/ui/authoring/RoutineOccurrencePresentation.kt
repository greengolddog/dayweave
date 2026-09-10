package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.sync.RoutineOccurrenceSelection

internal data class RoutineOccurrenceRow(
    val definition: RoutineOccurrenceMemberDefinition,
    val state: RoutineOccurrenceMemberState,
    val evaluation: RoutineOccurrenceMemberEvaluation,
    val depth: Int,
    val isParent: Boolean,
)

/** One iterative complete tree for display and control qualification, including omitted blocks. */
internal fun RoutineOccurrenceSnapshot.reviewRows(): List<RoutineOccurrenceRow> {
    val topology = aggregate.manifest.validatedTopology()
    val states = aggregate.members.associateBy { it.itemId }
    val evaluations = members.associateBy { it.itemId }
    val result = ArrayList<RoutineOccurrenceRow>(states.size)
    val pending = ArrayDeque<Pair<String, Int>>()
    pending.addLast(aggregate.manifest.seriesItemId to 0)
    while (pending.isNotEmpty()) {
        val (id, depth) = pending.removeLast()
        val children = topology.children[id].orEmpty()
        result.add(RoutineOccurrenceRow(topology.definitions.getValue(id), states.getValue(id),
            evaluations.getValue(id), depth, children.isNotEmpty()))
        children.sortedWith(compareBy<String> { topology.definitions.getValue(it).siblingOrder }.thenBy { it })
            .asReversed().forEach { pending.addLast(it to depth + 1) }
    }
    return result
}

internal fun DayWeaveUiState.routineOccurrenceSelection(block: ScheduleItem): RoutineOccurrenceSelection? {
    val occurrenceId = block.occurrenceId ?: return null
    val root = occurrenceSeriesItemIds[occurrenceId] ?: return null
    // The server emitted this exact root/occurrence mapping. Do not derive identity from dates.
    if (canonicalItems.singleOrNull { it.id == root }?.kind?.let { it !in setOf("task", "routine") } == true) return null
    return RoutineOccurrenceSelection(root, occurrenceId)
}

internal fun RoutineOccurrenceRow.canChange(reviewed: Boolean, eligible: Boolean, pending: Boolean): Boolean =
    reviewed && eligible && !pending && !evaluation.occurrenceEvidenceRequired
