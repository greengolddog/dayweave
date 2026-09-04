package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalBlockedReasonKind
import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalConstraintLevel
import com.greengolddog.dayweave.model.CanonicalDependencyDraft
import com.greengolddog.dayweave.model.CanonicalDependencyRelation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.decodeCanonicalFlexibleConstraints
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

internal data class CanonicalDependencyPresentation(
    val itemId: String,
    /** Already redacted when the predecessor is effectively sensitive or unavailable. */
    val displayTitle: String,
    val status: String?,
    val relation: CanonicalDependencyRelation?,
    val minimumLagMinutes: Long,
    val level: CanonicalConstraintLevel,
    val softWeight: Long?,
    val isSensitive: Boolean,
    val isAvailable: Boolean,
    val isReportedBlocker: Boolean = false,
) {
    val isSatisfied: Boolean get() = status == "completed"
    val isBlocking: Boolean get() = isReportedBlocker ||
        level == CanonicalConstraintLevel.HARD && !isSatisfied
}

internal data class CanonicalAuthoringRow(
    val itemId: String,
    val title: String,
    val kind: ItemKind,
    val status: String,
    val placement: CanonicalDraftPlacement,
    val parentId: String?,
    val depth: Int,
    val breadcrumb: List<String>,
    val isSensitive: Boolean,
    val durationKind: CanonicalDurationKind,
    val durationMinSeconds: Long?,
    val durationSeconds: Long?,
    val durationMaxSeconds: Long?,
    val durationSource: CanonicalDurationSource?,
    val deadlineKind: CanonicalDeadlineKind,
    val deadlineAt: String?,
    val deadlineDate: String?,
    val deadlineStrength: CanonicalDeadlineStrength?,
    val deadlineSoftWeight: Long?,
    val blockedReasonKind: CanonicalBlockedReasonKind?,
    val blockedByItemId: String?,
    val blockedReason: String?,
    val source: CanonicalAuthoringRowSource,
    val syncState: CanonicalAuthoringSyncState,
    val mutationId: String?,
    val diagnostic: String?,
    val draft: CanonicalItemDraft?,
    val revision: Long?,
    val isReadOnly: Boolean,
    val hasMissingParent: Boolean,
    val hasHierarchyCycle: Boolean,
    val dependencies: List<CanonicalDependencyPresentation> = emptyList(),
    val blockingDependencies: List<CanonicalDependencyPresentation> = emptyList(),
    val hasOpaqueDependencies: Boolean = false,
) {
    /** Opaque metadata blocks replacement, but a stable unfenced canonical row remains trashable. */
    val canTrash: Boolean
        get() = source == CanonicalAuthoringRowSource.CANONICAL && mutationId == null &&
            syncState == CanonicalAuthoringSyncState.SYNCED
}

internal data class CanonicalAuthoringPresentation(
    val inbox: List<CanonicalAuthoringRow>,
    val planned: List<CanonicalAuthoringRow>,
    val blocked: List<CanonicalAuthoringRow>,
    val conflicts: List<CanonicalAuthoringRow>,
    val recentlyDeleted: List<CanonicalAuthoringRow>,
) {
    val itemCount: Int get() = (inbox + planned + blocked + recentlyDeleted)
        .distinctBy(CanonicalAuthoringRow::itemId)
        .size

    val activeRowsByItemId: Map<String, CanonicalAuthoringRow>
        get() = (inbox + planned + blocked).associateBy(CanonicalAuthoringRow::itemId)

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
                        "blocked",
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
            val dependencyResolver = DependencyPresentationResolver(state)
            val inbox = mutableListOf<CanonicalAuthoringRow>()
            val planned = mutableListOf<CanonicalAuthoringRow>()
            val blocked = mutableListOf<CanonicalAuthoringRow>()
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
                    dependencyResolver = dependencyResolver,
                )
                when (row.status) {
                    CanonicalDraftPlacement.INBOX.wireValue -> inbox += row
                    CanonicalDraftPlacement.PLANNED.wireValue -> planned += row
                    "blocked" -> blocked += row
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
                        status = item?.status ?: draft?.placement?.wireValue ?: "inbox",
                        placement = draft?.placement ?: item?.status.toPlacement(),
                        parentId = item?.parentId,
                        depth = 0,
                        breadcrumb = emptyList(),
                        isSensitive = sensitivityFor(state, mutation.itemId),
                        durationKind = item?.durationKind ?: inferredDurationKind(
                            item?.durationSeconds,
                        ),
                        durationMinSeconds = item?.durationMinSeconds ?: item?.durationSeconds,
                        durationSeconds = item?.durationSeconds,
                        durationMaxSeconds = item?.durationMaxSeconds ?: item?.durationSeconds,
                        durationSource = item?.durationSource ?: inferredDurationSource(
                            item?.durationSeconds,
                        ),
                        deadlineKind = item?.deadlineKind ?: inferredDeadlineKind(
                            item?.kind,
                            item?.deadlineAt,
                        ),
                        deadlineAt = item?.deadlineAt,
                        deadlineDate = item?.deadlineDate,
                        deadlineStrength = item?.deadlineStrength ?: inferredDeadlineStrength(
                            item?.kind,
                            item?.deadlineAt,
                        ),
                        deadlineSoftWeight = item?.deadlineSoftWeight,
                        blockedReasonKind = item?.blockedReasonKind,
                        blockedByItemId = item?.blockedByItemId,
                        blockedReason = item?.blockedReason,
                        source = CanonicalAuthoringRowSource.PENDING_TRASH,
                        syncState = mutation.syncState(),
                        mutationId = mutation.id,
                        diagnostic = mutation.diagnostic,
                        draft = draft,
                        revision = item?.revision,
                        isReadOnly = true,
                        hasMissingParent = false,
                        hasHierarchyCycle = false,
                        hasOpaqueDependencies = item?.let {
                            runCatching(it::decodeCanonicalFlexibleConstraints).isFailure
                        } ?: false,
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
                        status = item?.status ?: draft?.placement?.wireValue ?: "inbox",
                        placement = draft?.placement ?: item?.status.toPlacement(),
                        parentId = record.parentId,
                        depth = 0,
                        breadcrumb = emptyList(),
                        isSensitive = record.isSensitive,
                        durationKind = item?.durationKind ?: inferredDurationKind(
                            item?.durationSeconds,
                        ),
                        durationMinSeconds = item?.durationMinSeconds ?: item?.durationSeconds,
                        durationSeconds = item?.durationSeconds,
                        durationMaxSeconds = item?.durationMaxSeconds ?: item?.durationSeconds,
                        durationSource = item?.durationSource ?: inferredDurationSource(
                            item?.durationSeconds,
                        ),
                        deadlineKind = item?.deadlineKind ?: inferredDeadlineKind(
                            item?.kind,
                            item?.deadlineAt,
                        ),
                        deadlineAt = item?.deadlineAt,
                        deadlineDate = item?.deadlineDate,
                        deadlineStrength = item?.deadlineStrength ?: inferredDeadlineStrength(
                            item?.kind,
                            item?.deadlineAt,
                        ),
                        deadlineSoftWeight = item?.deadlineSoftWeight,
                        blockedReasonKind = item?.blockedReasonKind,
                        blockedByItemId = item?.blockedByItemId,
                        blockedReason = item?.blockedReason,
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
                        hasOpaqueDependencies = item?.let {
                            runCatching(it::decodeCanonicalFlexibleConstraints).isFailure
                        } ?: false,
                    )
                    deleted += row
                    if (row.syncState == CanonicalAuthoringSyncState.CONFLICTED) conflicts += row
                }

            return CanonicalAuthoringPresentation(
                inbox = inbox,
                planned = planned,
                blocked = blocked,
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
    val status: String,
    val placement: CanonicalDraftPlacement,
    val parentId: String?,
    val siblingOrder: Long,
    val durationKind: CanonicalDurationKind,
    val durationMinSeconds: Long?,
    val durationSeconds: Long?,
    val durationMaxSeconds: Long?,
    val durationSource: CanonicalDurationSource?,
    val deadlineKind: CanonicalDeadlineKind,
    val deadlineAt: String?,
    val deadlineDate: String?,
    val deadlineStrength: CanonicalDeadlineStrength?,
    val deadlineSoftWeight: Long?,
    val blockedReasonKind: CanonicalBlockedReasonKind?,
    val blockedByItemId: String?,
    val blockedReason: String?,
    val dependencies: List<CanonicalDependencyDraft>,
    val hasOpaqueDependencies: Boolean,
    val mutation: PendingCanonicalAuthoringMutation?,
    val draft: CanonicalItemDraft?,
    val revision: Long?,
    val unsupported: Boolean,
    val unsupportedDiagnostic: String?,
) {
    fun toRow(
        depth: Int,
        breadcrumb: List<String>,
        isSensitive: Boolean,
        hasMissingParent: Boolean,
        hasHierarchyCycle: Boolean,
        dependencyResolver: DependencyPresentationResolver,
    ): CanonicalAuthoringRow {
        val source = when (mutation?.operation) {
            CanonicalAuthoringOperation.CREATE -> CanonicalAuthoringRowSource.LOCAL_CREATE
            CanonicalAuthoringOperation.REPLACE -> CanonicalAuthoringRowSource.PENDING_REPLACE
            CanonicalAuthoringOperation.RESTORE -> CanonicalAuthoringRowSource.ACTIVE_RESTORE
            CanonicalAuthoringOperation.TRASH -> error("Trash nodes are not active rows")
            null -> CanonicalAuthoringRowSource.CANONICAL
        }
        val reportedBlockerId = blockedByItemId
            ?.takeIf { blockedReasonKind == CanonicalBlockedReasonKind.DEPENDENCY }
        val dependencyPresentation = dependencies.map { dependency ->
            dependencyResolver.resolve(dependency).let { presented ->
                if (presented.itemId == reportedBlockerId) {
                    presented.copy(isReportedBlocker = true)
                } else {
                    presented
                }
            }
        }
        val compatibilityBlocker = reportedBlockerId
            ?.takeIf { blockedId -> dependencyPresentation.none { it.itemId == blockedId } }
            ?.let(dependencyResolver::resolveCompatibilityBlocker)
        val allDependencies = dependencyPresentation + listOfNotNull(compatibilityBlocker)
        return CanonicalAuthoringRow(
            itemId = itemId,
            title = title,
            kind = kind,
            status = status,
            placement = placement,
            parentId = parentId,
            depth = depth,
            breadcrumb = breadcrumb,
            isSensitive = isSensitive,
            durationKind = durationKind,
            durationMinSeconds = durationMinSeconds,
            durationSeconds = durationSeconds,
            durationMaxSeconds = durationMaxSeconds,
            durationSource = durationSource,
            deadlineKind = deadlineKind,
            deadlineAt = deadlineAt,
            deadlineDate = deadlineDate,
            deadlineStrength = deadlineStrength,
            deadlineSoftWeight = deadlineSoftWeight,
            blockedReasonKind = blockedReasonKind,
            blockedByItemId = blockedByItemId,
            blockedReason = blockedReason,
            source = source,
            syncState = mutation?.syncState() ?: CanonicalAuthoringSyncState.SYNCED,
            mutationId = mutation?.id,
            diagnostic = mutation?.diagnostic ?: unsupportedDiagnostic,
            draft = draft,
            revision = revision,
            isReadOnly = unsupported || mutation?.isSubmitted == true ||
                mutation?.disposition == CanonicalAuthoringDisposition.CONFLICTED ||
                mutation?.operation == CanonicalAuthoringOperation.RESTORE ||
                hasMissingParent || hasHierarchyCycle,
            hasMissingParent = hasMissingParent,
            hasHierarchyCycle = hasHierarchyCycle,
            dependencies = allDependencies,
            blockingDependencies = allDependencies.filter(CanonicalDependencyPresentation::isBlocking),
            hasOpaqueDependencies = hasOpaqueDependencies,
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
            val decodedAttempt = if (pendingDraft == null) {
                runCatching(item::toCanonicalDraft)
            } else {
                Result.success(pendingDraft)
            }
            val decoded = decodedAttempt.getOrNull()
            val dependencyDecode = if (decoded == null) {
                runCatching(item::decodeCanonicalFlexibleConstraints)
            } else {
                null
            }
            val dependencies = decoded?.constraints?.scheduling?.dependencies
                ?: dependencyDecode?.getOrNull()?.scheduling?.dependencies
                ?: emptyList()
            val usesPendingDraft = pendingDraft != null
            val presentedDuration = decoded?.durationSeconds ?: item.durationSeconds
            val presentedDeadline = decoded?.deadlineAt ?: item.deadlineAt
            val presentedKind = decoded?.kind ?: item.kind.toItemKind()
            return AuthoringNode(
                itemId = item.id,
                title = decoded?.title ?: item.title,
                kind = presentedKind,
                status = if (usesPendingDraft) {
                    requireNotNull(decoded).placement.wireValue
                } else {
                    item.status
                },
                placement = decoded?.placement ?: item.status.toPlacement(),
                parentId = decoded?.parentId ?: item.parentId,
                siblingOrder = decoded?.siblingOrder ?: item.siblingOrder,
                durationKind = if (usesPendingDraft) {
                    inferredDurationKind(presentedDuration)
                } else {
                    item.durationKind
                },
                durationMinSeconds = if (usesPendingDraft) {
                    presentedDuration
                } else {
                    item.durationMinSeconds
                },
                durationSeconds = presentedDuration,
                durationMaxSeconds = if (usesPendingDraft) {
                    presentedDuration
                } else {
                    item.durationMaxSeconds
                },
                durationSource = if (usesPendingDraft) {
                    inferredDurationSource(presentedDuration)
                } else {
                    item.durationSource
                },
                deadlineKind = if (usesPendingDraft) {
                    inferredDeadlineKind(presentedKind.name.lowercase(), presentedDeadline)
                } else {
                    item.deadlineKind
                },
                deadlineAt = presentedDeadline,
                deadlineDate = if (usesPendingDraft) null else item.deadlineDate,
                deadlineStrength = if (usesPendingDraft) {
                    inferredDeadlineStrength(presentedKind.name.lowercase(), presentedDeadline)
                } else {
                    item.deadlineStrength
                },
                deadlineSoftWeight = if (usesPendingDraft) null else item.deadlineSoftWeight,
                blockedReasonKind = if (usesPendingDraft) null else item.blockedReasonKind,
                blockedByItemId = if (usesPendingDraft) null else item.blockedByItemId,
                blockedReason = if (usesPendingDraft) null else item.blockedReason,
                dependencies = dependencies,
                hasOpaqueDependencies = dependencyDecode?.isFailure == true,
                mutation = mutation,
                draft = decoded,
                revision = item.revision,
                unsupported = decoded == null,
                unsupportedDiagnostic = decodedAttempt.exceptionOrNull()?.message
                    ?.takeIf(String::isNotBlank)
                    ?: if (decoded == null) "This scheduling metadata is read-only." else null,
            )
        }

        fun fromDraft(
            mutation: PendingCanonicalAuthoringMutation,
            draft: CanonicalItemDraft,
        ): AuthoringNode = AuthoringNode(
            itemId = mutation.itemId,
            title = draft.title,
            kind = draft.kind,
            status = draft.placement.wireValue,
            placement = draft.placement,
            parentId = draft.parentId,
            siblingOrder = draft.siblingOrder,
            durationKind = inferredDurationKind(draft.durationSeconds),
            durationMinSeconds = draft.durationSeconds,
            durationSeconds = draft.durationSeconds,
            durationMaxSeconds = draft.durationSeconds,
            durationSource = inferredDurationSource(draft.durationSeconds),
            deadlineKind = inferredDeadlineKind(draft.kind.name.lowercase(), draft.deadlineAt),
            deadlineAt = draft.deadlineAt,
            deadlineDate = null,
            deadlineStrength = inferredDeadlineStrength(
                draft.kind.name.lowercase(),
                draft.deadlineAt,
            ),
            deadlineSoftWeight = null,
            blockedReasonKind = null,
            blockedByItemId = null,
            blockedReason = null,
            dependencies = draft.constraints.scheduling?.dependencies.orEmpty(),
            hasOpaqueDependencies = false,
            mutation = mutation,
            draft = draft,
            revision = null,
            unsupported = false,
            unsupportedDiagnostic = null,
        )
    }
}

private class DependencyPresentationResolver(state: DayWeaveUiState) {
    private data class Target(
        val id: String,
        val title: String,
        val status: String,
        val isSensitive: Boolean,
    )

    private val targets: Map<String, Target>

    init {
        data class MutableTarget(
            val id: String,
            val title: String,
            val status: String,
        )

        val projected = state.canonicalItems
            .filter { it.deletedAt == null }
            .associate { item ->
                item.id to MutableTarget(item.id, item.title, item.status)
            }
            .toMutableMap()
        state.pendingCanonicalAuthoringMutations.forEach { mutation ->
            when (mutation.operation) {
                CanonicalAuthoringOperation.TRASH -> projected.remove(mutation.itemId)
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                -> mutation.draft?.let { draft ->
                    projected[mutation.itemId] = MutableTarget(
                        id = mutation.itemId,
                        title = draft.title,
                        status = draft.placement.wireValue,
                    )
                }
                CanonicalAuthoringOperation.RESTORE -> Unit
            }
        }
        targets = projected.mapValues { (itemId, target) ->
            val isSensitive = runCatching {
                effectiveCanonicalSensitivity(
                    items = state.canonicalItems,
                    itemId = itemId,
                    pendingMutation = state.pendingCanonicalMutation,
                    pendingAuthoringMutations = state.pendingCanonicalAuthoringMutations,
                )
            }.getOrDefault(true)
            Target(
                id = target.id,
                title = target.title,
                status = target.status,
                isSensitive = isSensitive,
            )
        }
    }

    fun resolve(dependency: CanonicalDependencyDraft): CanonicalDependencyPresentation {
        val target = targets[dependency.itemId]
        return CanonicalDependencyPresentation(
            itemId = dependency.itemId,
            displayTitle = target?.safeDisplayTitle()
                ?: "Unavailable item · ${dependency.itemId.take(8)}",
            status = target?.status,
            relation = dependency.relation,
            minimumLagMinutes = dependency.minimumLagMinutes,
            level = dependency.strength.level,
            softWeight = dependency.strength.weight,
            isSensitive = target?.isSensitive ?: true,
            isAvailable = target != null,
        )
    }

    fun resolveCompatibilityBlocker(itemId: String): CanonicalDependencyPresentation {
        val target = targets[itemId]
        return CanonicalDependencyPresentation(
            itemId = itemId,
            displayTitle = target?.safeDisplayTitle() ?: "Unavailable item · ${itemId.take(8)}",
            status = target?.status,
            relation = null,
            minimumLagMinutes = 0,
            level = CanonicalConstraintLevel.HARD,
            softWeight = null,
            isSensitive = target?.isSensitive ?: true,
            isAvailable = target != null,
            isReportedBlocker = true,
        )
    }

    private fun Target.safeDisplayTitle(): String = if (isSensitive) {
        "Sensitive item · ${id.take(8)}"
    } else {
        title.trim().ifEmpty { "Untitled item" }
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

private fun inferredDurationKind(durationSeconds: Long?): CanonicalDurationKind =
    if (durationSeconds == null) CanonicalDurationKind.UNKNOWN else CanonicalDurationKind.EXACT

private fun inferredDurationSource(durationSeconds: Long?): CanonicalDurationSource? =
    if (durationSeconds == null) null else CanonicalDurationSource.USER

private fun inferredDeadlineKind(kind: String?, deadlineAt: String?): CanonicalDeadlineKind =
    if (kind == "event" || deadlineAt == null) {
        CanonicalDeadlineKind.NONE
    } else {
        CanonicalDeadlineKind.DATE_TIME
    }

private fun inferredDeadlineStrength(
    kind: String?,
    deadlineAt: String?,
): CanonicalDeadlineStrength? = if (kind == "event" || deadlineAt == null) {
    null
} else {
    CanonicalDeadlineStrength.HARD
}
