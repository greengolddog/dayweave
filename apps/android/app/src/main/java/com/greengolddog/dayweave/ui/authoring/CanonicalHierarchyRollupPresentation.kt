package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalHierarchyRollup
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalSensitivityIndex
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HierarchyEffortEstimate
import com.greengolddog.dayweave.model.HierarchyRollupNode
import com.greengolddog.dayweave.model.HierarchyRollupTotals
import com.greengolddog.dayweave.model.requireCanonicalUuid

internal sealed interface HierarchyRollupDisplay {
    data class Available(val totals: HierarchyRollupTotals) : HierarchyRollupDisplay
    data object Sensitive : HierarchyRollupDisplay
    data object Incomplete : HierarchyRollupDisplay
    data object Pending : HierarchyRollupDisplay
    data object Invalid : HierarchyRollupDisplay
}

/** Numerical summaries never escape a privacy or authority-withholding decision. */
internal class CanonicalHierarchyRollupPresentation private constructor(
    private val totals: Map<String, HierarchyRollupTotals>,
    private val concealedIds: Set<String>,
    private val withheld: HierarchyRollupDisplay?,
) {
    operator fun get(id: String): HierarchyRollupDisplay = withheld ?: when {
        id in concealedIds -> HierarchyRollupDisplay.Sensitive
        else -> totals[id]?.let(HierarchyRollupDisplay::Available) ?: HierarchyRollupDisplay.Invalid
    }

    companion object {
        fun build(state: DayWeaveUiState): CanonicalHierarchyRollupPresentation {
            fun withheld(reason: HierarchyRollupDisplay) =
                CanonicalHierarchyRollupPresentation(emptyMap(), emptySet(), reason)
            if (state.canonicalConfigurationId.isNullOrBlank() || state.canonicalSyncOrigin.isNullOrBlank() ||
                state.canonicalDeltaCursor.isNullOrBlank()
            ) return withheld(HierarchyRollupDisplay.Incomplete)
            if (state.pendingCanonicalAuthoringMutations.isNotEmpty() || state.pendingCanonicalMutation != null ||
                state.pendingProposalApplicationMutation != null || state.pendingExecutionCommand != null ||
                state.hasPendingHierarchyLifecycleProjection()
            ) return withheld(HierarchyRollupDisplay.Pending)
            val items = state.canonicalItems.filter { it.deletedAt == null }
            val nodes = runCatching { items.map(CanonicalItemSnapshot::rollupNode) }.getOrNull()
                ?: return withheld(HierarchyRollupDisplay.Invalid)
            val totals = CanonicalHierarchyRollup.build(nodes) ?: return withheld(HierarchyRollupDisplay.Invalid)
            val privacy = CanonicalSensitivityIndex.build(state.canonicalItems,
                state.pendingCanonicalMutation, state.pendingCanonicalAuthoringMutations)
            val byId = items.associateBy { it.id }
            val concealed = mutableSetOf<String>()
            // Walk each newly concealed ancestor once, not once per sensitive descendant.
            items.filter { privacy[it.id] }.forEach { item ->
                var id: String? = item.id
                while (id != null && concealed.add(id)) id = byId[id]?.parentId
            }
            return CanonicalHierarchyRollupPresentation(totals, concealed, null)
        }
    }
}

private fun DayWeaveUiState.hasPendingHierarchyLifecycleProjection(): Boolean {
    val required = terminalExecutionOutcomes.values.filter { it.requiresCanonicalItemProjection }
    if (required.isEmpty()) return false
    val cachedRevisions = mutableMapOf<String, Long>()
    canonicalItems.forEach { cachedRevisions[it.id] = maxOf(cachedRevisions[it.id] ?: 0, it.revision) }
    canonicalRecentlyDeleted.forEach { cachedRevisions[it.id] = maxOf(cachedRevisions[it.id] ?: 0, it.revision) }
    return required.any { outcome ->
        if (outcome.canonicalProjectionResolution != null) false else {
            val revision = outcome.canonicalProjectionRevision
            // Lifetime receipts outlive bounded tombstones. An absent historical identity is not
            // proof that a complete saved forest is stale; known older cache evidence is.
            revision == null || cachedRevisions[outcome.session.itemId]?.let { it < revision } == true
        }
    }
}

private fun CanonicalItemSnapshot.rollupNode(): HierarchyRollupNode {
    requireCanonicalUuid(id, "hierarchy item")
    parentId?.let { requireCanonicalUuid(it, "hierarchy parent") }
    require(durationKind.isSupported && durationSource?.isSupported != false)
    val duration = when (durationKind) {
        CanonicalDurationKind.UNKNOWN -> {
            require(durationSeconds == null && durationMinSeconds == null && durationMaxSeconds == null && durationSource == null)
            null
        }
        CanonicalDurationKind.EXACT, CanonicalDurationKind.RANGE -> {
            val value = HierarchyEffortEstimate(requireNotNull(durationMinSeconds), requireNotNull(durationSeconds),
                requireNotNull(durationMaxSeconds))
            require(durationSource != null)
            if (durationKind == CanonicalDurationKind.EXACT) {
                require(value.minimum == value.expected && value.expected == value.maximum)
            } else require(value.minimum < value.maximum)
            value
        }
        else -> error("Unsupported duration")
    }
    return HierarchyRollupNode(id, parentId, kind, status, hasOwnEffort, recurrenceJson != null, duration = duration)
}

internal fun HierarchyRollupDisplay.summaryLabel(): String = when (this) {
    is HierarchyRollupDisplay.Available -> with(totals) {
        "Leaf items · $completedLeafItems completed · $openLeafItems open · " +
            "$skippedLeafItems skipped · $cancelledLeafItems cancelled"
    }
    HierarchyRollupDisplay.Sensitive -> "Summary concealed · sensitive subtree"
    HierarchyRollupDisplay.Incomplete -> "Summary unavailable · complete saved hierarchy required"
    HierarchyRollupDisplay.Pending -> "Summary withheld · local changes need review or sync"
    HierarchyRollupDisplay.Invalid -> "Summary unavailable · hierarchy or estimates cannot be verified"
}

internal fun HierarchyRollupTotals.detailLabels(): List<String> = listOf(
    "Recurring leaf items · $recurringLeafItems · occurrence achievement is separate",
    "Fixed events · $fixedEvents · not flexible effort",
    "Recorded leaf effort estimates · ${estimateSecondsLabel(expectedEstimateSeconds)} expected · " +
        "${estimateSecondsLabel(minimumEstimateSeconds)} minimum · ${estimateSecondsLabel(maximumEstimateSeconds)} maximum",
    "Unknown leaf effort estimates · $unknownEstimates · not counted as zero work",
    "Recorded lifecycle and estimates only. Not weighted progress, remaining time, or elapsed work. " +
        "The item's lifecycle is unchanged.",
)

private fun estimateSecondsLabel(seconds: Long): String {
    val hours = seconds / 3_600
    val minutes = seconds % 3_600 / 60
    val remainder = seconds % 60
    return listOfNotNull(
        hours.takeIf { it > 0 }?.let { "${it}h" },
        minutes.takeIf { it > 0 }?.let { "${it}m" },
        remainder.takeIf { it > 0 || seconds == 0L }?.let { "${it}s" },
    ).joinToString(" ")
}
