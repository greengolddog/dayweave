package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.ItemKind

internal data class CanonicalHierarchyBrowserRow(
    val item: CanonicalAuthoringRow,
    val depth: Int,
    val isContext: Boolean,
    val isSearchMatch: Boolean,
    val hasChildren: Boolean,
    val isCollapsed: Boolean,
)

/** A local projection only: visibility never grants scheduling, execution, or write authority. */
internal data class CanonicalHierarchyBrowserPresentation(
    val rows: List<CanonicalHierarchyBrowserRow>,
    val eligibleCount: Int,
) {
    companion object {
        fun build(
            rows: List<CanonicalAuthoringRow>,
            kind: ItemKind,
            query: String = "",
            collapsedIds: Set<String> = emptySet(),
        ): CanonicalHierarchyBrowserPresentation {
            require(kind == ItemKind.GOAL || kind == ItemKind.PROJECT)
            val orderedRows = rows.sortedWith(compareBy({ it.siblingOrder }, { it.itemId.lowercase() }))
            val byId = orderedRows.associateBy(CanonicalAuthoringRow::itemId)
            val children = orderedRows.groupBy(CanonicalAuthoringRow::parentId)
            val eligible = linkedSetOf<String>()
            val pending = ArrayDeque<String>()
            // Malformed ancestor protection must not depend on which fallback root sorts first.
            // A descendant may have a lower UUID than the rootless cycle it belongs to.
            val unsafeIds = mutableSetOf<String>()
            orderedRows.filter { it.hasMissingParent || it.hasHierarchyCycle || it.hasUnsafeAncestry }
                .forEach { pending.addLast(it.itemId) }
            while (pending.isNotEmpty()) {
                val id = pending.removeLast()
                if (!unsafeIds.add(id)) continue
                children[id].orEmpty().forEach { pending.addLast(it.itemId) }
            }
            orderedRows.filter { it.kind == kind }.forEach { pending.addLast(it.itemId) }
            while (pending.isNotEmpty()) {
                val id = pending.removeLast()
                if (!eligible.add(id)) continue
                children[id].orEmpty().forEach { pending.addLast(it.itemId) }
            }
            val search = query.trim()
            val matches = if (search.isEmpty()) emptySet() else eligible.filterTo(mutableSetOf()) {
                byId.getValue(it).title.contains(search, ignoreCase = true)
            }
            val included = if (search.isEmpty()) eligible.toMutableSet() else matches.toMutableSet()
            val ancestorVisited = mutableSetOf<String>()
            included.toList().forEach { id ->
                var parent = byId[id]?.parentId
                while (parent != null && parent in byId && ancestorVisited.add(parent)) {
                    included += parent
                    parent = byId[parent]?.parentId
                }
            }
            val visible = mutableListOf<CanonicalHierarchyBrowserRow>()
            val visited = mutableSetOf<String>()
            data class Frame(val id: String, val depth: Int, val hidden: Boolean)
            val stack = ArrayDeque<Frame>()
            fun walk(root: CanonicalAuthoringRow) {
                stack.addLast(Frame(root.itemId, 0, false))
                while (stack.isNotEmpty()) {
                    val frame = stack.removeLast()
                    if (!visited.add(frame.id)) continue
                    val row = byId.getValue(frame.id)
                    val descendants = children[frame.id].orEmpty().filter {
                        it.itemId != frame.id && it.itemId in included
                    }
                    val collapsed = search.isEmpty() && row.itemId in collapsedIds
                    if (!frame.hidden) {
                        visible += CanonicalHierarchyBrowserRow(
                            item = if (frame.id in unsafeIds) row.copy(isReadOnly = true) else row,
                            depth = frame.depth,
                            isContext = frame.id !in eligible,
                            isSearchMatch = frame.id in matches,
                            hasChildren = descendants.isNotEmpty(),
                            isCollapsed = collapsed,
                        )
                    }
                    descendants.asReversed().forEach {
                        stack.addLast(Frame(it.itemId, frame.depth + 1, frame.hidden || collapsed))
                    }
                }
            }
            orderedRows.filter {
                it.itemId in included &&
                    (it.parentId !in included || it.parentId == it.itemId)
            }.forEach(::walk)
            // Invalid cycles are retained once as diagnosed rows, never silently lost.
            orderedRows.filter { it.itemId in included && it.itemId !in visited }.forEach(::walk)
            return CanonicalHierarchyBrowserPresentation(visible, eligible.size)
        }
    }
}
