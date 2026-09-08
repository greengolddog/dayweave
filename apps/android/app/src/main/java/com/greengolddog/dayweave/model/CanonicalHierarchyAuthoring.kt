package com.greengolddog.dayweave.model

/** Read-only projection of the exact parent choices the current offline journal can support. */
internal class CanonicalHierarchyParentAuthority private constructor(
    private val nodes: Map<String, Node>,
    private val unsafeIds: Set<String>,
    private val hasActiveExecution: Boolean,
) {
    internal data class Node(val id: String, val title: String, val parentId: String?, val status: String)

    fun issue(parentId: String?, childId: String): String? {
        if (parentId == null) return null
        if (hasActiveExecution) return "Only detached Inbox capture is available during active execution"
        val parent = nodes[parentId] ?: return "The selected parent is unavailable or not admitted"
        if (parentId in unsafeIds) return "The selected ancestry is incomplete or has unresolved local changes"
        if (parent.status !in setOf("inbox", "planned", "blocked")) {
            return "The selected parent is executing or terminal"
        }
        var cursor: String? = parentId
        while (cursor != null) {
            if (cursor == childId) return "An item cannot be moved into its own descendants"
            cursor = nodes[cursor]?.parentId
        }
        return null
    }

    fun options(excludingItemId: String): List<Node> {
        if (hasActiveExecution) return emptyList()
        val children = nodes.values.groupBy { it.parentId }
        val excluded = mutableSetOf(excludingItemId)
        val pending = ArrayDeque<String>().apply { add(excludingItemId) }
        while (pending.isNotEmpty()) {
            children[pending.removeFirst()].orEmpty().forEach { if (excluded.add(it.id)) pending.add(it.id) }
        }
        return nodes.values.filter {
            it.id !in excluded && it.id !in unsafeIds && it.status in setOf("inbox", "planned", "blocked")
        }.sortedWith(compareBy({ it.title.lowercase() }, { it.id.lowercase() }))
    }

    companion object {
        fun build(state: DayWeaveUiState, allowPendingRestores: Boolean = false): CanonicalHierarchyParentAuthority {
            val nodes = linkedMapOf<String, Node>()
            val unsafe = mutableSetOf<String>()
            val canonical = state.canonicalItems.associateBy { it.id }
            if (state.canonicalConfigurationId != null) {
                state.canonicalItems.forEach { item ->
                    if (item.deletedAt == null) {
                        if (nodes.put(item.id, Node(item.id, item.title, item.parentId, item.status)) != null ||
                            runCatching(item::requireCanonicalAuthoringShape).isFailure
                        ) unsafe.add(item.id)
                    }
                }
            }
            val seen = mutableSetOf<String>()
            state.pendingCanonicalAuthoringMutations.forEach { mutation ->
                val id = mutation.itemId
                if (!seen.add(id) || mutation.isSubmitted || mutation.syncOrigin != null ||
                    mutation.configurationId != null || mutation.disposition != CanonicalAuthoringDisposition.PENDING ||
                    runCatching(mutation::requireValid).isFailure
                ) {
                    unsafe.add(id)
                    return@forEach
                }
                when (mutation.operation) {
                    CanonicalAuthoringOperation.CREATE -> {
                        val draft = requireNotNull(mutation.draft)
                        if (canonical[id] != null) unsafe.add(id)
                        nodes[id] = Node(id, draft.title, draft.parentId, draft.placement.wireValue)
                    }
                    CanonicalAuthoringOperation.REPLACE -> {
                        val draft = requireNotNull(mutation.draft)
                        val current = canonical[id]
                        if (id !in nodes || current?.revision != mutation.expectedRevision || current != mutation.baseItem) {
                            unsafe.add(id)
                        }
                        nodes[id] = Node(id, draft.title, draft.parentId, draft.placement.wireValue)
                    }
                    CanonicalAuthoringOperation.RESTORE -> {
                        val retained = state.canonicalRecentlyDeleted.firstOrNull { it.id == id }
                        if (allowPendingRestores && retained != null && state.canonicalConfigurationId != null) {
                            // Existing identity-only restore trees keep their topological recovery
                            // semantics after content retention expires. This grants no new capture.
                            nodes[id] = Node(id, retained.lastKnownItem?.title.orEmpty(), retained.parentId,
                                retained.lastKnownItem?.status ?: mutation.baseItem?.status ?: "planned")
                        } else {
                            nodes.remove(id)
                            unsafe.add(id)
                        }
                    }
                    CanonicalAuthoringOperation.TRASH -> {
                        nodes.remove(id)
                        unsafe.add(id)
                    }
                }
            }
            // Resolve each chain once. Unsafe closure is independent of input or UUID order.
            val resolved = mutableSetOf<String>()
            nodes.keys.forEach { start ->
                if (start in resolved) return@forEach
                val path = mutableListOf<String>()
                val visiting = mutableSetOf<String>()
                var cursor: String? = start
                var invalid = false
                while (cursor != null) {
                    if (cursor in unsafe) { invalid = true; break }
                    if (cursor in resolved) break
                    val node = nodes[cursor]
                    if (node == null || !visiting.add(cursor)) { invalid = true; break }
                    path.add(cursor)
                    cursor = node.parentId
                }
                if (invalid) unsafe.addAll(path)
                resolved.addAll(path)
            }
            return CanonicalHierarchyParentAuthority(nodes, unsafe, state.canonicalExecutionSession != null)
        }
    }
}
