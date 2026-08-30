package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.requireCanonicalTimezoneName
import java.time.ZoneId

/**
 * Small, network-free application layer for user-initiated canonical authoring.
 *
 * Every successful call below only appends to (or removes from) the encrypted authoring journal.
 * Submission and reconciliation remain owned by the canonical sync path.
 */
internal class CanonicalAuthoringController(
    private val plannerStore: PlannerStore,
    private val zoneId: () -> ZoneId = ZoneId::systemDefault,
) {
    suspend fun quickCapture(
        title: String,
        kind: ItemKind,
        isSensitive: Boolean,
    ): Boolean {
        val draft = canonicalTitleOnlyDraft(
            title = title,
            kind = kind,
            isSensitive = isSensitive,
            timezoneName = canonicalDeviceTimezoneName(zoneId()),
        ) ?: return false
        return create(draft)
    }

    suspend fun create(
        draft: CanonicalItemDraft,
        itemId: String? = null,
    ): Boolean = (if (itemId == null) {
        plannerStore.enqueueCanonicalCreate(draft)
    } else {
        plannerStore.enqueueCanonicalCreate(draft, itemId)
    }).awaitDurable()

    suspend fun convertInboxDraft(
        inboxId: String,
        itemId: String,
        draft: CanonicalItemDraft,
    ): Boolean = plannerStore.enqueueCanonicalCreateFromInbox(
        inboxId = inboxId,
        draft = draft,
        itemId = itemId,
    ).awaitDurable()

    suspend fun replace(itemId: String, draft: CanonicalItemDraft): Boolean =
        plannerStore.enqueueCanonicalReplace(itemId, draft).awaitDurable()

    suspend fun updatePending(mutationId: String, draft: CanonicalItemDraft): Boolean =
        plannerStore.updateCanonicalAuthoringDraft(mutationId, draft).awaitDurable()

    /** The confirmation flag keeps accidental direct callers from bypassing destructive UI. */
    suspend fun trash(itemId: String, confirmed: Boolean): Boolean =
        confirmed && plannerStore.enqueueCanonicalTrash(itemId).awaitDurable()

    suspend fun restore(itemId: String): Boolean =
        plannerStore.enqueueCanonicalRestore(itemId).awaitDurable()

    suspend fun discard(mutationId: String): Boolean =
        plannerStore.discardCanonicalAuthoringMutation(mutationId)?.awaitDurable() == true

    /**
     * Copies a retained create/replace conflict to a fresh standalone Inbox identity.
     * The original conflict deliberately remains until the user explicitly discards it.
     */
    suspend fun copyConflict(mutationId: String): Boolean =
        plannerStore.duplicateConflictedCanonicalDraft(mutationId).awaitDurable()

    private suspend fun CanonicalAuthoringTransition?.awaitDurable(): Boolean =
        this?.persistence?.awaitDurable() == true
}

/**
 * Title-only capture is intentionally narrower than the detailed editor. Habits need an explicit
 * recurrence and events need exact timing, so those choices must continue through the editor.
 */
internal fun canonicalTitleOnlyDraft(
    title: String,
    kind: ItemKind,
    isSensitive: Boolean,
    timezoneName: String,
): CanonicalItemDraft? {
    if (kind == ItemKind.HABIT || kind == ItemKind.EVENT) return null
    val normalizedTitle = title.trim()
    if (normalizedTitle.isEmpty()) return null
    return CanonicalItemDraft(
        placement = CanonicalDraftPlacement.INBOX,
        kind = kind,
        isSensitive = isSensitive,
        title = normalizedTitle,
        timezoneName = timezoneName,
    )
}

internal fun canonicalDeviceTimezoneName(zoneId: ZoneId = ZoneId.systemDefault()): String =
    zoneId.id.takeIf { runCatching { requireCanonicalTimezoneName(it) }.isSuccess } ?: "UTC"
