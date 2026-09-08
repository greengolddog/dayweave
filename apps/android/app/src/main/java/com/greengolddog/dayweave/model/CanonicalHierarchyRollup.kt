package com.greengolddog.dayweave.model

/** Normalized canonical seconds, independent of schedule blocks and presentation filtering. */
internal data class HierarchyRollupNode(
    val id: String,
    val parentId: String?,
    val kind: String,
    val status: String,
    val hasOwnEffort: Boolean = false,
    val recurs: Boolean = false,
    val hasChildrenOutsidePlan: Boolean = false,
    val duration: HierarchyEffortEstimate? = null,
)

internal data class HierarchyEffortEstimate(val minimum: Long, val expected: Long, val maximum: Long)

internal data class HierarchyRollupTotals(
    val completedLeafItems: Long = 0,
    val skippedLeafItems: Long = 0,
    val cancelledLeafItems: Long = 0,
    val openLeafItems: Long = 0,
    val recurringLeafItems: Long = 0,
    val fixedEvents: Long = 0,
    val unknownEstimates: Long = 0,
    val minimumEstimateSeconds: Long = 0,
    val expectedEstimateSeconds: Long = 0,
    val maximumEstimateSeconds: Long = 0,
) {
    operator fun plus(other: HierarchyRollupTotals) = HierarchyRollupTotals(
        Math.addExact(completedLeafItems, other.completedLeafItems),
        Math.addExact(skippedLeafItems, other.skippedLeafItems),
        Math.addExact(cancelledLeafItems, other.cancelledLeafItems),
        Math.addExact(openLeafItems, other.openLeafItems),
        Math.addExact(recurringLeafItems, other.recurringLeafItems),
        Math.addExact(fixedEvents, other.fixedEvents),
        Math.addExact(unknownEstimates, other.unknownEstimates),
        Math.addExact(minimumEstimateSeconds, other.minimumEstimateSeconds),
        Math.addExact(expectedEstimateSeconds, other.expectedEstimateSeconds),
        Math.addExact(maximumEstimateSeconds, other.maximumEstimateSeconds),
    )
}

/** Invalid or incomplete forests have no numerical answer, including for otherwise valid roots. */
internal object CanonicalHierarchyRollup {
    fun build(nodes: List<HierarchyRollupNode>): Map<String, HierarchyRollupTotals>? = runCatching {
        val byId = nodes.associateBy { it.id }
        require(byId.size == nodes.size)
        nodes.forEach { node ->
            requireCanonicalUuid(node.id, "hierarchy item")
            node.parentId?.let { requireCanonicalUuid(it, "hierarchy parent") }
            require(node.kind in KINDS && node.status in STATUSES && !node.hasChildrenOutsidePlan)
            require(node.parentId == null || node.parentId in byId)
            node.duration?.let {
                require(it.minimum > 0 && it.minimum <= it.expected && it.expected <= it.maximum)
            }
        }
        val children = nodes.groupBy { it.parentId }
        val pending = ArrayDeque<Pair<HierarchyRollupNode, Boolean>>()
        children[null].orEmpty().forEach { pending.addLast(it to false) }
        val order = ArrayList<HierarchyRollupNode>(nodes.size)
        val result = HashMap<String, HierarchyRollupTotals>(nodes.size)
        while (pending.isNotEmpty()) {
            val (node, inheritedRecurrence) = pending.removeFirst()
            val recurring = inheritedRecurrence || node.recurs
            order.add(node)
            val descendants = children[node.id].orEmpty()
            result[node.id] = ownTotals(node, descendants.isEmpty(), recurring)
            descendants.forEach { pending.addLast(it to recurring) }
        }
        require(order.size == nodes.size) // A rootless cycle cannot be silently omitted.
        for (node in order.asReversed()) {
            node.parentId?.let { parent ->
                result[parent] = result.getValue(parent) + result.getValue(node.id)
            }
        }
        result
    }.getOrNull()

    private fun ownTotals(node: HierarchyRollupNode, leaf: Boolean, recurring: Boolean): HierarchyRollupTotals {
        if (recurring) return HierarchyRollupTotals(recurringLeafItems = if (leaf) 1 else 0)
        val event = node.kind == "event"
        val lifecycle = if (!leaf) HierarchyRollupTotals() else when (node.status) {
            "completed" -> HierarchyRollupTotals(completedLeafItems = 1)
            "skipped" -> HierarchyRollupTotals(skippedLeafItems = 1)
            "cancelled" -> HierarchyRollupTotals(cancelledLeafItems = 1)
            else -> HierarchyRollupTotals(openLeafItems = 1)
        }
        if (event) return lifecycle.copy(fixedEvents = 1)
        if (!leaf || node.kind in CONTAINERS && !node.hasOwnEffort) return lifecycle
        val duration = node.duration ?: return lifecycle.copy(unknownEstimates = 1)
        return lifecycle.copy(
            minimumEstimateSeconds = duration.minimum,
            expectedEstimateSeconds = duration.expected,
            maximumEstimateSeconds = duration.maximum,
        )
    }

    private val KINDS = setOf("task", "project", "goal", "routine", "habit", "break", "event")
    private val CONTAINERS = setOf("project", "goal", "routine")
    private val STATUSES = setOf("inbox", "planned", "scheduled", "in_progress", "paused", "completed", "skipped", "cancelled", "blocked")
}
