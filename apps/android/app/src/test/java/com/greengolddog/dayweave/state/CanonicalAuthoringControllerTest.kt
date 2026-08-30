package com.greengolddog.dayweave.state

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.toCanonicalDraft
import java.time.ZoneId
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringControllerTest {
    @Test
    fun titleOnlyCaptureUsesCanonicalJournalAndLeavesLegacyInboxVisibleStateAlone() = runBlocking {
        val store = PlannerStore(DayWeaveUiState())
        val controller = CanonicalAuthoringController(store) { ZoneId.of("UTC") }

        assertTrue(controller.quickCapture("  Review launch notes  ", ItemKind.TASK, true))

        assertTrue(store.state.value.inbox.isEmpty())
        val mutation = store.state.value.pendingCanonicalAuthoringMutations.single()
        assertEquals(CanonicalAuthoringOperation.CREATE, mutation.operation)
        assertEquals("Review launch notes", mutation.draft?.title)
        assertEquals(CanonicalDraftPlacement.INBOX, mutation.draft?.placement)
        assertEquals("UTC", mutation.draft?.timezoneName)
        assertEquals(true, mutation.draft?.isSensitive)
    }

    @Test
    fun titleOnlyHabitAndEventContinueThroughDetailedEditorInsteadOfInventingContracts() =
        runBlocking {
            val store = PlannerStore(DayWeaveUiState())
            val controller = CanonicalAuthoringController(store) { ZoneId.of("UTC") }

            assertFalse(controller.quickCapture("Walk", ItemKind.HABIT, false))
            assertFalse(controller.quickCapture("Appointment", ItemKind.EVENT, false))
            assertTrue(store.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        }

    @Test
    fun legacyReviewDraftConvertsAtomicallyAndCannotLowerItsSensitivity() = runBlocking {
        val legacy = InboxItem(
            id = "proposal-synthetic-review",
            isSensitive = true,
            title = "Review suggested task",
            source = InboxSource.EXTERNAL_PROPOSAL,
            detail = "Synthetic proposal context",
        )
        val store = PlannerStore(DayWeaveUiState(inbox = listOf(legacy)))

        assertTrue(
            CanonicalAuthoringController(store).convertInboxDraft(
                inboxId = legacy.id,
                itemId = ITEM_ID,
                draft = draft(legacy.title).copy(
                    notes = legacy.detail,
                    isSensitive = false,
                ),
            ),
        )

        assertTrue(store.state.value.inbox.isEmpty())
        val mutation = store.state.value.pendingCanonicalAuthoringMutations.single()
        assertEquals(ITEM_ID, mutation.itemId)
        assertEquals(legacy.title, mutation.draft?.title)
        assertEquals(legacy.detail, mutation.draft?.notes)
        assertEquals(true, mutation.draft?.isSensitive)
    }

    @Test
    fun queuedDraftUpdatePreservesItemMutationAndIdempotencyIdentity() = runBlocking {
        val store = PlannerStore(DayWeaveUiState())
        val initial = requireNotNull(
            store.enqueueCanonicalCreate(
                draft("Original"),
                ITEM_ID,
                MUTATION_ID,
            ),
        ).mutation
        val controller = CanonicalAuthoringController(store)

        assertTrue(controller.updatePending(MUTATION_ID, draft("Edited")))

        val updated = store.state.value.pendingCanonicalAuthoringMutations.single()
        assertEquals(initial.id, updated.id)
        assertEquals(initial.itemId, updated.itemId)
        assertEquals(initial.idempotencyKey, updated.idempotencyKey)
        assertEquals("Edited", updated.draft?.title)
    }

    @Test
    fun trashNeedsExplicitConfirmationAndRestoreQueuesTheRetainedRevision() = runBlocking {
        val active = item(ITEM_ID, "Active")
        val activeStore = PlannerStore(DayWeaveUiState(canonicalItems = listOf(active)))
        val activeController = CanonicalAuthoringController(activeStore)

        assertFalse(activeController.trash(ITEM_ID, confirmed = false))
        assertTrue(activeStore.state.value.pendingCanonicalAuthoringMutations.isEmpty())
        assertTrue(activeController.trash(ITEM_ID, confirmed = true))
        assertEquals(
            CanonicalAuthoringOperation.TRASH,
            activeStore.state.value.pendingCanonicalAuthoringMutations.single().operation,
        )

        val deletedItem = active.copy(
            revision = 2,
            isExecutable = false,
            updatedAt = "2026-08-30T10:01:00Z",
            deletedAt = "2026-08-30T10:01:00Z",
        )
        val deleted = CanonicalRecentlyDeletedRecord(
            id = ITEM_ID,
            revision = deletedItem.revision,
            deletedAt = requireNotNull(deletedItem.deletedAt),
            lastKnownItem = deletedItem,
            effectiveIsSensitive = false,
            retentionAnchorAt = requireNotNull(deletedItem.deletedAt),
        )
        val deletedStore = PlannerStore(
            DayWeaveUiState(canonicalRecentlyDeleted = listOf(deleted)),
        )

        assertTrue(CanonicalAuthoringController(deletedStore).restore(ITEM_ID))
        val restore = deletedStore.state.value.pendingCanonicalAuthoringMutations.single()
        assertEquals(CanonicalAuthoringOperation.RESTORE, restore.operation)
        assertEquals(2L, restore.expectedRevision)
    }

    @Test
    fun conflictCopyRetainsOriginalAndPromotesInheritedSensitivityWhenDetached() = runBlocking {
        val parent = item(PARENT_ID, "Private project").copy(isSensitive = true)
        val child = item(ITEM_ID, "Child", parentId = PARENT_ID)
        val conflicted = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = child.toCanonicalDraft().copy(title = "Retained edit"),
            expectedRevision = child.revision,
            baseItem = child,
            createdAt = NOW,
            syncOrigin = "https://example.test/",
            submittedAt = "2026-08-30T10:00:01Z",
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "Revision changed",
        )
        val store = PlannerStore(
            DayWeaveUiState(
                canonicalItems = listOf(parent, child),
                pendingCanonicalAuthoringMutations = listOf(conflicted),
            ),
        )

        assertTrue(CanonicalAuthoringController(store).copyConflict(MUTATION_ID))

        val mutations = store.state.value.pendingCanonicalAuthoringMutations
        assertEquals(2, mutations.size)
        assertEquals(conflicted, mutations.first { it.id == MUTATION_ID })
        val copy = mutations.single { it.id != MUTATION_ID }
        assertEquals(CanonicalAuthoringOperation.CREATE, copy.operation)
        assertEquals("Retained edit", copy.draft?.title)
        assertEquals(CanonicalDraftPlacement.INBOX, copy.draft?.placement)
        assertEquals(null, copy.draft?.parentId)
        assertEquals(true, copy.draft?.isSensitive)
    }

    private fun draft(title: String) = CanonicalItemDraft(
        title = title,
        timezoneName = "UTC",
    )

    private fun item(
        id: String,
        title: String,
        parentId: String? = null,
    ) = CanonicalItemSnapshot(
        id = id,
        kind = "task",
        status = "inbox",
        title = title,
        timezoneName = "UTC",
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = false,
        revision = 1,
        createdAt = NOW,
        updatedAt = NOW,
    )

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val PARENT_ID = "22222222-2222-4222-8222-222222222222"
        const val MUTATION_ID = "33333333-3333-4333-8333-333333333333"
        const val NOW = "2026-08-30T10:00:00Z"
    }
}
