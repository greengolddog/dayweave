package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.effectiveCanonicalSensitivity
import com.greengolddog.dayweave.model.toCanonicalDraft
import java.time.Instant

internal enum class CanonicalAuthoringRowSource {
    CANONICAL,
    LOCAL_CREATE,
    PENDING_REPLACE,
    PENDING_TRASH,
    PENDING_RESTORE,
    ACTIVE_RESTORE,
    RECENTLY_DELETED,
}

internal enum class CanonicalAuthoringSyncState {
    SYNCED,
    QUEUED,
    SUBMITTED,
    CONFLICTED,
}

internal data class CanonicalAuthoringRow(
    val itemId: String,
    val title: String,
    val kind: ItemKind,
    val placement: CanonicalDraftPlacement,
    val parentId: String?,
    val depth: Int,
    val breadcrumb: List<String>,
    val isSensitive: Boolean,
    val durationSeconds: Long?,
    val deadlineAt: String?,
    val source: CanonicalAuthoringRowSource,
    val syncState: CanonicalAuthoringSyncState,
    val mutationId: String?,
    val diagnostic: String?,
    val draft: CanonicalItemDraft?,
    val revision: Long?,
    val isReadOnly: Boolean,
    val hasMissingParent: Boolean,
    val hasHierarchyCycle: Boolean,
)

internal data class CanonicalAuthoringPresentation(
    val inbox: List<CanonicalAuthoringRow>,
    val planned: List<CanonicalAuthoringRow>,
    val conflicts: List<CanonicalAuthoringRow>,
    val recentlyDeleted: List<CanonicalAuthoringRow>,
) {
    val itemCount: Int get() = (inbox + planned + recentlyDeleted)
        .distinctBy(CanonicalAuthoringRow::itemId)
        .size

    companion object {
        fun build(state: DayWeaveUiState): CanonicalAuthoringPresentation {
            val pendingByItem = state.pendingCanonicalAuthoringMutations.associateBy { it.itemId }
            val allActiveById = state.canonicalItems
                .filter { it.deletedAt == null }
                .associateBy(CanonicalItemSnapshot::id)
            val activeById = allActiveById.filterValues {
                it.status in setOf(
                        CanonicalDraftPlacement.INBOX.wireValue,
                        CanonicalDraftPlacement.PLANNED.wireValue,
                    )
            }
            val nodes = linkedMapOf<String, AuthoringNode>()

            activeById.values.forEach { item ->
                val mutation = pendingByItem[item.id]
                if (mutation?.operation != CanonicalAuthoringOperation.TRASH) {
                    nodes[item.id] = AuthoringNode.fromItem(item, mutation)
                }
            }
            state.pendingCanonicalAuthoringMutations.forEach { mutation ->
                if (
                    mutation.itemId !in nodes &&
                    mutation.operation in setOf(
                        CanonicalAuthoringOperation.CREATE,
                        CanonicalAuthoringOperation.REPLACE,
                    )
                ) {
                    mutation.draft?.let { draft ->
                        nodes[mutation.itemId] = AuthoringNode.fromDraft(mutation, draft)
                    }
                }
            }

            val hierarchy = AuthoringHierarchy(nodes)
            val inbox = mutableListOf<CanonicalAuthoringRow>()
            val planned = mutableListOf<CanonicalAuthoringRow>()
            val conflicts = mutableListOf<CanonicalAuthoringRow>()
            val deleted = mutableListOf<CanonicalAuthoringRow>()

            hierarchy.orderedIds.forEach { itemId ->
                val node = nodes[itemId] ?: return@forEach
                val row = node.toRow(
                    depth = hierarchy.depthById[itemId] ?: 0,
                    breadcrumb = hierarchy.breadcrumbById[itemId].orEmpty(),
                    isSensitive = runCatching {
                        effectiveCanonicalSensitivity(
                            items = state.canonicalItems,
                            itemId = itemId,
                            pendingMutation = state.pendingCanonicalMutation,
                            pendingAuthoringMutations = state.pendingCanonicalAuthoringMutations,
                        )
                    }.getOrDefault(true),
                    hasMissingParent = itemId in hierarchy.missingParentIds,
                    hasHierarchyCycle = itemId in hierarchy.cyclicIds,
                )
                when (row.placement) {
                    CanonicalDraftPlacement.INBOX -> inbox += row
                    CanonicalDraftPlacement.PLANNED -> planned += row
                }
                if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) conflicts += row
            }

            state.pendingCanonicalAuthoringMutations
                .filter { it.operation == CanonicalAuthoringOperation.TRASH }
                .forEach { mutation ->
                    val item = mutation.baseItem ?: allActiveById[mutation.itemId]
                    val draft = item?.let { runCatching(it::toCanonicalDraft).getOrNull() }
                    val row = CanonicalAuthoringRow(
                        itemId = mutation.itemId,
                        title = item?.title ?: "Deleted item",
                        kind = item?.kind.toItemKind(),
                        placement = draft?.placement ?: item?.status.toPlacement(),
                        parentId = item?.parentId,
                        depth = 0,
                        breadcrumb = emptyList(),
                        isSensitive = sensitivityFor(state, mutation.itemId),
                        durationSeconds = item?.durationSeconds,
                        deadlineAt = item?.deadlineAt,
                        source = CanonicalAuthoringRowSource.PENDING_TRASH,
                        syncState = mutation.syncState(),
                        mutationId = mutation.id,
                        diagnostic = mutation.diagnostic,
                        draft = draft,
                        revision = item?.revision,
                        isReadOnly = true,
                        hasMissingParent = false,
                        hasHierarchyCycle = false,
                    )
                    deleted += row
                    if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) conflicts += row
                }

            state.canonicalRecentlyDeleted
                .sortedWith(
                    compareByDescending { record ->
                        runCatching { Instant.parse(record.deletedAt) }.getOrNull()
                    },
                )
                .forEach { record ->
                    val mutation = pendingByItem[record.id]
                    if (mutation != null && mutation.operation != CanonicalAuthoringOperation.RESTORE) {
                        return@forEach
                    }
                    val item = record.lastKnownItem
                    val draft = item?.let { runCatching(it::toCanonicalDraft).getOrNull() }
                    val row = CanonicalAuthoringRow(
                        itemId = record.id,
                        title = item?.title ?: "Deleted item",
                        kind = item?.kind.toItemKind(),
                        placement = draft?.placement ?: item?.status.toPlacement(),
                        parentId = record.parentId,
                        depth = 0,
                        breadcrumb = emptyList(),
                        isSensitive = record.isSensitive,
                        durationSeconds = item?.durationSeconds,
                        deadlineAt = item?.deadlineAt,
                        source = if (mutation == null) {
                            CanonicalAuthoringRowSource.RECENTLY_DELETED
                        } else {
                            CanonicalAuthoringRowSource.PENDING_RESTORE
                        },
                        syncState = mutation?.syncState() ?: CanonicalAuthoringSyncState.SYNCED,
                        mutationId = mutation?.id,
                        diagnostic = mutation?.diagnostic,
                        draft = draft,
                        revision = record.revision,
                        isReadOnly = true,
                        hasMissingParent = false,
                        hasHierarchyCycle = false,
                    )
                    deleted += row
                    if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) conflicts += row
                }

            return CanonicalAuthoringPresentation(
                inbox = inbox,
                planned = planned,
                conflicts = conflicts.distinctBy(CanonicalAuthoringRow::itemId),
                recentlyDeleted = deleted.distinctBy(CanonicalAuthoringRow::itemId),
            )
        }

        private fun sensitivityFor(state: DayWeaveUiState, itemId: String): Boolean =
            runCatching {
                effectiveCanonicalSensitivity(
                    items = state.canonicalItems,
                    itemId = itemId,
                    pendingMutation = state.pendingCanonicalMutation,
                    pendingAuthoringMutations = state.pendingCanonicalAuthoringMutations,
                )
            }.getOrDefault(true)
    }
}

private data class AuthoringNode(
    val itemId: String,
    val title: String,
    val kind: ItemKind,
    val placement: CanonicalDraftPlacement,
    val parentId: String?,
    val siblingOrder: Long,
    val durationSeconds: Long?,
    val deadlineAt: String?,
    val mutation: PendingCanonicalAuthoringMutation?,
    val draft: CanonicalItemDraft?,
    val revision: Long?,
    val unsupported: Boolean,
) {
    fun toRow(
        depth: Int,
        breadcrumb: List<String>,
        isSensitive: Boolean,
        hasMissingParent: Boolean,
        hasHierarchyCycle: Boolean,
    ): CanonicalAuthoringRow {
        val source = when (mutation?.operation) {
            CanonicalAuthoringOperation.CREATE -> CanonicalAuthoringRowSource.LOCAL_CREATE
            CanonicalAuthoringOperation.REPLACE -> CanonicalAuthoringRowSource.PENDING_REPLACE
            CanonicalAuthoringOperation.RESTORE -> CanonicalAuthoringRowSource.ACTIVE_RESTORE
            CanonicalAuthoringOperation.TRASH -> error("Trash nodes are not active rows")
            null -> CanonicalAuthoringRowSource.CANONICAL
        }
        return CanonicalAuthoringRow(
            itemId = itemId,
            title = title,
            kind = kind,
            placement = placement,
            parentId = parentId,
            depth = depth,
            breadcrumb = breadcrumb,
            isSensitive = isSensitive,
            durationSeconds = durationSeconds,
            deadlineAt = deadlineAt,
            source = source,
            syncState = mutation?.syncState() ?: CanonicalAuthoringSyncState.SYNCED,
            mutationId = mutation?.id,
            diagnostic = mutation?.diagnostic,
            draft = draft,
            revision = revision,
            isReadOnly = unsupported || mutation?.isSubmitted == true ||
                mutation?.disposition == CanonicalAuthoringDisposition.CONFLICTED ||
                mutation?.operation == CanonicalAuthoringOperation.RESTORE ||
                hasMissingParent || hasHierarchyCycle,
            hasMissingParent = hasMissingParent,
            hasHierarchyCycle = hasHierarchyCycle,
        )
    }

    companion object {
        fun fromItem(
            item: CanonicalItemSnapshot,
            mutation: PendingCanonicalAuthoringMutation?,
        ): AuthoringNode {
            val pendingDraft = mutation?.draft?.takeIf {
                mutation.operation == CanonicalAuthoringOperation.CREATE ||
                    mutation.operation == CanonicalAuthoringOperation.REPLACE
            }
            val decoded = pendingDraft ?: runCatching(item::toCanonicalDraft).getOrNull()
            return AuthoringNode(
                itemId = item.id,
                title = decoded?.title ?: item.title,
                kind = decoded?.kind ?: item.kind.toItemKind(),
                placement = decoded?.placement ?: item.status.toPlacement(),
                parentId = decoded?.parentId ?: item.parentId,
                siblingOrder = decoded?.siblingOrder ?: item.siblingOrder,
                durationSeconds = decoded?.durationSeconds ?: item.durationSeconds,
                deadlineAt = decoded?.deadlineAt ?: item.deadlineAt,
                mutation = mutation,
                draft = decoded,
                revision = item.revision,
                unsupported = decoded == null,
            )
        }

        fun fromDraft(
            mutation: PendingCanonicalAuthoringMutation,
            draft: CanonicalItemDraft,
        ): AuthoringNode = AuthoringNode(
            itemId = mutation.itemId,
            title = draft.title,
            kind = draft.kind,
            placement = draft.placement,
            parentId = draft.parentId,
            siblingOrder = draft.siblingOrder,
            durationSeconds = draft.durationSeconds,
            deadlineAt = draft.deadlineAt,
            mutation = mutation,
            draft = draft,
            revision = null,
            unsupported = false,
        )
    }
}

private class AuthoringHierarchy(nodes: Map<String, AuthoringNode>) {
    val orderedIds: List<String>
    val depthById: Map<String, Int>
    val breadcrumbById: Map<String, List<String>>
    val missingParentIds: Set<String>
    val cyclicIds: Set<String>

    init {
        val order = compareBy<AuthoringNode>({ it.siblingOrder }, { it.title.lowercase() }, { it.itemId })
        val children = mutableMapOf<String, MutableList<AuthoringNode>>()
        val roots = mutableListOf<AuthoringNode>()
        val missing = mutableSetOf<String>()
        nodes.values.forEach { node ->
            val parentId = node.parentId
            if (parentId == null) {
                roots += node
            } else if (parentId == node.itemId || parentId !in nodes) {
                roots += node
                if (parentId !in nodes) missing += node.itemId
            } else {
                children.getOrPut(parentId) { mutableListOf() } += node
            }
        }
        roots.sortWith(order)
        children.values.forEach { it.sortWith(order) }

        val ordered = mutableListOf<String>()
        val depths = mutableMapOf<String, Int>()
        val breadcrumbs = mutableMapOf<String, List<String>>()
        val visited = mutableSetOf<String>()
        val stack = ArrayDeque<HierarchyFrame>()
        roots.asReversed().forEach { stack.addLast(HierarchyFrame(it, 0, emptyList())) }
        while (stack.isNotEmpty()) {
            val frame = stack.removeLast()
            if (!visited.add(frame.node.itemId)) continue
            ordered += frame.node.itemId
            depths[frame.node.itemId] = frame.depth
            breadcrumbs[frame.node.itemId] = frame.breadcrumb.takeLast(MAX_BREADCRUMB_DEPTH)
            val childBreadcrumb = (frame.breadcrumb + frame.node.title).takeLast(MAX_BREADCRUMB_DEPTH)
            children[frame.node.itemId].orEmpty().asReversed().forEach { child ->
                stack.addLast(HierarchyFrame(child, frame.depth + 1, childBreadcrumb))
            }
        }

        val cycles = mutableSetOf<String>()
        nodes.values.sortedWith(order).forEach { start ->
            if (start.itemId in visited) return@forEach
            val chain = mutableListOf<AuthoringNode>()
            val chainIndex = mutableMapOf<String, Int>()
            var current: AuthoringNode? = start
            while (current != null && current.itemId !in visited) {
                val repeatedAt = chainIndex[current.itemId]
                if (repeatedAt != null) {
                    cycles += chain.drop(repeatedAt).map(AuthoringNode::itemId)
                    break
                }
                chainIndex[current.itemId] = chain.size
                chain += current
                current = current.parentId?.let(nodes::get)
            }
            chain.forEach { node ->
                if (visited.add(node.itemId)) {
                    ordered += node.itemId
                    depths[node.itemId] = 0
                    breadcrumbs[node.itemId] = emptyList()
                }
            }
        }

        orderedIds = ordered
        depthById = depths
        breadcrumbById = breadcrumbs
        missingParentIds = missing
        cyclicIds = cycles
    }

    private data class HierarchyFrame(
        val node: AuthoringNode,
        val depth: Int,
        val breadcrumb: List<String>,
    )

    private companion object {
        const val MAX_BREADCRUMB_DEPTH = 32
    }
}

private fun PendingCanonicalAuthoringMutation.syncState(): CanonicalAuthoringSyncState = when {
    disposition == CanonicalAuthoringDisposition.CONFLICTED ->
        CanonicalAuthoringSyncState.CONFLICTED
    isSubmitted -> CanonicalAuthoringSyncState.SUBMITTED
    else -> CanonicalAuthoringSyncState.QUEUED
}

private fun String?.toItemKind(): ItemKind = ItemKind.entries.firstOrNull {
    it.name.equals(this, ignoreCase = true)
} ?: ItemKind.TASK

private fun String?.toPlacement(): CanonicalDraftPlacement =
    CanonicalDraftPlacement.entries.firstOrNull { it.wireValue == this }
        ?: CanonicalDraftPlacement.INBOX
