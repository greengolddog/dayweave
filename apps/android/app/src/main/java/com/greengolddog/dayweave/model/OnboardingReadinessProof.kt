package com.greengolddog.dayweave.model

import kotlinx.serialization.Serializable

/**
 * Encrypted, content-free identity of the item explicitly reviewed during onboarding.
 *
 * A null revision identifies only the exact local create retained in the encrypted authoring
 * journal. It cannot prove that a canonical item or a published first plan exists.
 */
@Serializable
data class OnboardingFirstItemAnchorSnapshot(
    val itemId: String,
    val canonicalRevision: Long? = null,
) {
    fun hasValidShape(): Boolean = runCatching {
        requireCanonicalUuid(itemId, "onboarding first item")
        require(canonicalRevision == null || canonicalRevision > 0)
    }.isSuccess

    /**
     * Exact first-plan evidence. The publication proof must seal the whole current durable plan,
     * not merely contain a block that happens to reuse the anchored identifier.
     */
    fun hasExactPublishedPlanProof(state: DayWeaveUiState): Boolean {
        val revision = canonicalRevision ?: return false
        if (!hasValidShape()) return false
        if (state.validatedOnboardingFirstItemCheck() != OnboardingFirstItemCheck.CANONICAL_ITEM) {
            return false
        }
        val canonicalItem = state.canonicalItems.singleOrNull {
            it.id == itemId && it.revision == revision && it.deletedAt == null
        } ?: return false
        // A queued move can make a prospective draft a leaf, but cannot turn an
        // already-published parent block into authoritative work before sync.
        if (canonicalItem.kind != "event" && (
                !canonicalItem.isExecutable || state.canonicalItems.any {
                    it.parentId == itemId && it.deletedAt == null
                }
            )
        ) return false
        if (state.pendingSchedulePublication != null) return false
        if (state.pendingCanonicalAuthoringMutations.any { it.itemId == itemId }) return false
        val proof = state.publishedScheduleProof ?: return false
        return proof.matchesCurrentStateAndPlan(state) && proof.blocks.any { block ->
            block.itemId == itemId && block.itemRevision == revision
        }
    }

    override fun toString(): String =
        "OnboardingFirstItemAnchorSnapshot(itemId=<redacted>, canonicalRevision=$canonicalRevision)"
}

/** Which exact encrypted record currently satisfies the first-item setup check. */
enum class OnboardingFirstItemCheck {
    PENDING_CREATE,
    CANONICAL_ITEM,
}

/**
 * Pure minimum-demand predicate for a locally reviewed create.
 *
 * Event validation already proves exact fixed timing. Every other kind must be a leaf with a
 * positive duration. Goal and Routine leaves additionally need explicit `has_own_effort=true`;
 * that stored flag never gives a parent its own calendar block.
 */
fun CanonicalItemDraft.createsPlanningDemand(
    itemId: String,
    hasChildren: Boolean = false,
): Boolean = runCatching {
    val value = normalized()
    value.requireValid(itemId)
    require(value.placement == CanonicalDraftPlacement.PLANNED)
    require(value.kind == ItemKind.EVENT || !hasChildren)
    when (value.kind) {
        ItemKind.EVENT -> true
        ItemKind.TASK,
        ItemKind.HABIT,
        ItemKind.BREAK,
        -> value.durationSeconds?.let { it > 0 } == true
        ItemKind.GOAL,
        ItemKind.PROJECT,
        ItemKind.ROUTINE,
        -> value.durationSeconds?.let { it > 0 } == true &&
            value.hasOwnEffort
    }
}.getOrDefault(false)

/** Canonical counterpart of [CanonicalItemDraft.createsPlanningDemand]. */
fun CanonicalItemSnapshot.createsPlanningDemand(
    canonicalItems: List<CanonicalItemSnapshot>,
    pendingAuthoringMutations: List<PendingCanonicalAuthoringMutation> = emptyList(),
    recentlyDeleted: List<CanonicalRecentlyDeletedRecord> = emptyList(),
): Boolean = runCatching {
    require(deletedAt == null)
    require(status == "planned" || status == "scheduled")
    val hasCanonicalChildren = canonicalItems.any { child ->
        child.id != id && child.parentId == id && child.deletedAt == null
    }
    // Fixed events retain their interval even when they also organize child work.
    // For flexible work, an inconsistent cached execution flag must fail closed.
    if (kind != "event") require(isExecutable == !hasCanonicalChildren)
    val hasChildren = hasEffectiveCanonicalChild(
        itemId = id,
        canonicalItems = canonicalItems,
        pendingAuthoringMutations = pendingAuthoringMutations,
        recentlyDeleted = recentlyDeleted,
    )
    // The strict authoring decoder accepts Planned but not the server-owned Scheduled state.
    val reviewable = if (status == "scheduled") copy(status = "planned") else this
    reviewable.toCanonicalDraft().createsPlanningDemand(id, hasChildren)
}.getOrDefault(false)

/**
 * Materializes the hierarchy that pending authoring will produce without inventing server state.
 * Conflicted operations do not participate in the active overlay. A bodyless restore without its
 * retained deletion record fails closed as a possible child.
 */
internal fun hasEffectiveCanonicalChild(
    itemId: String,
    canonicalItems: List<CanonicalItemSnapshot>,
    pendingAuthoringMutations: List<PendingCanonicalAuthoringMutation>,
    recentlyDeleted: List<CanonicalRecentlyDeletedRecord> = emptyList(),
): Boolean {
    val parentById = canonicalItems.asSequence()
        .filter { it.deletedAt == null }
        .associateTo(mutableMapOf()) { it.id to it.parentId }
    val deletedById = recentlyDeleted.associateBy(CanonicalRecentlyDeletedRecord::id)
    pendingAuthoringMutations.asSequence()
        .filter { it.disposition == CanonicalAuthoringDisposition.PENDING }
        .forEach { mutation ->
            when (mutation.operation) {
                CanonicalAuthoringOperation.CREATE,
                CanonicalAuthoringOperation.REPLACE,
                -> {
                    val draft = mutation.draft ?: return true
                    parentById[mutation.itemId] = draft.parentId
                }
                CanonicalAuthoringOperation.TRASH -> parentById.remove(mutation.itemId)
                CanonicalAuthoringOperation.RESTORE -> {
                    val deleted = deletedById[mutation.itemId]
                    val base = mutation.baseItem
                    if (deleted == null && base == null) return true
                    parentById[mutation.itemId] = if (deleted != null) {
                        deleted.parentId
                    } else {
                        base?.parentId
                    }
                }
            }
        }
    return parentById.any { (childId, parentId) -> childId != itemId && parentId == itemId }
}

/**
 * Returns exact first-item evidence, or null for absent, stale, conflicted, or ineligible state.
 */
fun DayWeaveUiState.validatedOnboardingFirstItemCheck(): OnboardingFirstItemCheck? {
    val anchor = onboardingFirstItemAnchor ?: return null
    if (!anchor.hasValidShape()) return null
    val revision = anchor.canonicalRevision
    if (revision == null) {
        val create = pendingCanonicalAuthoringMutations.singleOrNull { mutation ->
            mutation.itemId == anchor.itemId &&
                mutation.operation == CanonicalAuthoringOperation.CREATE
        } ?: return null
        if (create.disposition != CanonicalAuthoringDisposition.PENDING) return null
        val hasChildren = hasEffectiveCanonicalChild(
            itemId = anchor.itemId,
            canonicalItems = canonicalItems,
            pendingAuthoringMutations = pendingCanonicalAuthoringMutations,
            recentlyDeleted = canonicalRecentlyDeleted,
        )
        if (create.draft?.createsPlanningDemand(anchor.itemId, hasChildren) != true) return null
        return OnboardingFirstItemCheck.PENDING_CREATE
    }

    val item = canonicalItems.singleOrNull {
        it.id == anchor.itemId && it.deletedAt == null
    }?.takeIf { it.revision == revision } ?: return null
    return OnboardingFirstItemCheck.CANONICAL_ITEM.takeIf {
        item.createsPlanningDemand(
            canonicalItems = canonicalItems,
            pendingAuthoringMutations = pendingCanonicalAuthoringMutations,
            recentlyDeleted = canonicalRecentlyDeleted,
        )
    }
}

/**
 * Persistence relationship check. Eligibility is intentionally checked separately so a stale
 * reviewed anchor can remain visible for explicit recovery instead of corrupting planner storage.
 */
fun DayWeaveUiState.hasValidOnboardingFirstItemAnchorRelationship(): Boolean {
    val anchor = onboardingFirstItemAnchor ?: return true
    if (!anchor.hasValidShape()) return false
    val revision = anchor.canonicalRevision
    return if (revision != null) {
        canonicalItems.singleOrNull {
            it.id == anchor.itemId && it.deletedAt == null
        }?.revision == revision
    } else {
        pendingCanonicalAuthoringMutations.any { mutation ->
            mutation.itemId == anchor.itemId &&
                mutation.operation == CanonicalAuthoringOperation.CREATE &&
                mutation.draft?.createsPlanningDemand(anchor.itemId) == true
        }
    }
}

fun DayWeaveUiState.hasExactOnboardingFirstPlanProof(): Boolean =
    onboardingFirstItemAnchor?.hasExactPublishedPlanProof(this) == true

/**
 * Reconciles an anchor against one authoritative item generation without silently reviewing a
 * cross-device edit. Callers remain responsible for committing the returned value atomically with
 * the item/journal transition.
 */
fun reconciledOnboardingFirstItemAnchor(
    anchor: OnboardingFirstItemAnchorSnapshot?,
    canonicalItems: List<CanonicalItemSnapshot>,
    pendingAuthoringMutations: List<PendingCanonicalAuthoringMutation>,
    recentlyDeleted: List<CanonicalRecentlyDeletedRecord>,
    authoritativeMissing: Boolean = false,
): OnboardingFirstItemAnchorSnapshot? {
    if (anchor == null || !anchor.hasValidShape()) return null
    val activeMatches = canonicalItems.filter { it.id == anchor.itemId && it.deletedAt == null }
    if (activeMatches.size > 1) return null
    val item = activeMatches.singleOrNull()
    val hasChildren = hasEffectiveCanonicalChild(
        itemId = anchor.itemId,
        canonicalItems = canonicalItems,
        pendingAuthoringMutations = pendingAuthoringMutations,
        recentlyDeleted = recentlyDeleted,
    )
    if (item != null) {
        if (anchor.canonicalRevision == item.revision) return anchor
        val exactReviewedMutation = pendingAuthoringMutations.any { mutation ->
            mutation.itemId == anchor.itemId &&
                mutation.operation in if (anchor.canonicalRevision == null) {
                    setOf(CanonicalAuthoringOperation.CREATE)
                } else {
                    setOf(
                        CanonicalAuthoringOperation.CREATE,
                        CanonicalAuthoringOperation.REPLACE,
                    )
                } &&
                mutation.draft?.let { draft ->
                    draft.createsPlanningDemand(anchor.itemId, hasChildren) && draft.matches(item)
                } == true
        }
        return when {
            exactReviewedMutation -> OnboardingFirstItemAnchorSnapshot(item.id, item.revision)
            anchor.canonicalRevision != null -> null
            else -> anchor
        }
    }
    if (recentlyDeleted.any { it.id == anchor.itemId }) return null
    if (!authoritativeMissing) return anchor
    val retainedCreate = pendingAuthoringMutations.any { mutation ->
        mutation.itemId == anchor.itemId &&
            mutation.operation == CanonicalAuthoringOperation.CREATE &&
            mutation.draft?.createsPlanningDemand(anchor.itemId, hasChildren) == true
    }
    return if (retainedCreate) {
        OnboardingFirstItemAnchorSnapshot(anchor.itemId, null)
    } else {
        null
    }
}
