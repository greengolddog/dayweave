package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.toCanonicalDraft
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringPresentationTest {
    @Test
    fun buildsInboxPlannedConflictAndRecentlyDeletedSectionsWithHierarchy() {
        val parent = item(PARENT_ID, "Project", "inbox")
        val child = item(CHILD_ID, "Next action", "planned", parentId = PARENT_ID)
        val conflict = PendingCanonicalAuthoringMutation(
            id = CONFLICT_MUTATION_ID,
            itemId = CONFLICT_ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = CanonicalItemDraft(
                title = "Conflicted capture",
                timezoneName = "UTC",
            ),
            createdAt = NOW,
            syncOrigin = "https://example.test/",
            submittedAt = "2026-08-30T10:00:01Z",
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "Canonical identity already exists",
        )
        val deletedItem = item(DELETED_ID, "Recover me", "inbox").copy(
            revision = 2,
            updatedAt = "2026-08-30T10:02:00Z",
            deletedAt = "2026-08-30T10:02:00Z",
        )
        val deleted = CanonicalRecentlyDeletedRecord(
            id = deletedItem.id,
            revision = deletedItem.revision,
            deletedAt = requireNotNull(deletedItem.deletedAt),
            lastKnownItem = deletedItem,
            effectiveIsSensitive = false,
            retentionAnchorAt = requireNotNull(deletedItem.deletedAt),
        )

        val presentation = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(
                canonicalItems = listOf(parent, child),
                pendingCanonicalAuthoringMutations = listOf(conflict),
                canonicalRecentlyDeleted = listOf(deleted),
            ),
        )

        assertEquals(setOf("Project", "Conflicted capture"), presentation.inbox.map { it.title }.toSet())
        assertEquals(listOf("Next action"), presentation.planned.map { it.title })
        assertEquals(listOf("Conflicted capture"), presentation.conflicts.map { it.title })
        assertEquals(listOf("Recover me"), presentation.recentlyDeleted.map { it.title })
        val childRow = presentation.planned.single()
        assertEquals(1, childRow.depth)
        assertEquals(listOf("Project"), childRow.breadcrumb)
        assertEquals(CanonicalAuthoringSyncState.CONFLICTED, presentation.conflicts.single().syncState)
    }

    @Test
    fun unsentReplacementOverlaysCanonicalBodyAndRemainsEditable() {
        val canonical = item(PARENT_ID, "Server title", "inbox")
        val replacement = PendingCanonicalAuthoringMutation(
            id = REPLACE_MUTATION_ID,
            itemId = canonical.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = canonical.toCanonicalDraft().copy(
                title = "Queued title",
                placement = CanonicalDraftPlacement.PLANNED,
            ),
            expectedRevision = canonical.revision,
            baseItem = canonical,
            createdAt = NOW,
        )

        val presentation = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(
                canonicalItems = listOf(canonical),
                pendingCanonicalAuthoringMutations = listOf(replacement),
            ),
        )

        assertTrue(presentation.inbox.isEmpty())
        val row = presentation.planned.single()
        assertEquals("Queued title", row.title)
        assertEquals(CanonicalAuthoringRowSource.PENDING_REPLACE, row.source)
        assertEquals(CanonicalAuthoringSyncState.QUEUED, row.syncState)
        assertFalse(row.isReadOnly)
        assertEquals(REPLACE_MUTATION_ID, row.mutationId)
    }

    private fun item(
        id: String,
        title: String,
        status: String,
        parentId: String? = null,
    ) = CanonicalItemSnapshot(
        id = id,
        kind = "task",
        status = status,
        title = title,
        timezoneName = "UTC",
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = status == "planned",
        revision = 1,
        createdAt = NOW,
        updatedAt = NOW,
    )

    private companion object {
        const val PARENT_ID = "11111111-1111-4111-8111-111111111111"
        const val CHILD_ID = "22222222-2222-4222-8222-222222222222"
        const val CONFLICT_ITEM_ID = "33333333-3333-4333-8333-333333333333"
        const val DELETED_ID = "44444444-4444-4444-8444-444444444444"
        const val CONFLICT_MUTATION_ID = "55555555-5555-4555-8555-555555555555"
        const val REPLACE_MUTATION_ID = "66666666-6666-4666-8666-666666666666"
        const val NOW = "2026-08-30T10:00:00Z"
    }
}
