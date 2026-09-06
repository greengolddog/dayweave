package com.greengolddog.dayweave.model

import java.util.UUID
import kotlin.random.Random
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalSensitivityIndexTest {
    @Test
    fun ownAndInheritedMarksDoNotProtectUnrelatedPublicBranches() {
        val root = item(1, sensitive = true)
        val child = item(2, parentId = root.id)
        val grandchild = item(3, parentId = child.id)
        val publicRoot = item(4)
        val publicChild = item(5, parentId = publicRoot.id)
        val index = assertParity(listOf(root, child, grandchild, publicRoot, publicChild))

        listOf(root, child, grandchild).forEach { assertTrue(index[it.id]) }
        listOf(publicRoot, publicChild).forEach { assertFalse(index[it.id]) }
        assertTrue(index[id(99)])
    }

    @Test
    fun retainedDiamondPathsAreNotMistakenForCyclesAndEitherPathCanRaisePrivacy() {
        val root = item(1)
        val left = item(2, parentId = root.id)
        val right = item(3, parentId = root.id)
        val moved = item(4, parentId = left.id)
        val descendant = item(5, parentId = moved.id)
        val replacement = replace(moved, moved.toCanonicalDraft().copy(parentId = right.id))
        replacement.requireValid()
        val public = listOf(root, left, right, moved, descendant)
        val publicIndex = assertParity(public, authoring = listOf(replacement))
        public.forEach { assertFalse(publicIndex[it.id]) }

        for (markedId in listOf(root.id, left.id, right.id, moved.id)) {
            val marked = public.map { if (it.id == markedId) it.copy(isSensitive = true) else it }
            val index = assertParity(marked, authoring = listOf(replacement))
            assertTrue(index[moved.id])
            assertTrue(index[descendant.id])
        }
    }

    @Test
    fun queuedDeclassificationRetainsCanonicalAndBasePrivacyUntilConfirmed() {
        val sensitiveParent = item(1, sensitive = true)
        val moved = item(2, sensitive = true, parentId = sensitiveParent.id)
        val child = item(3, parentId = moved.id)
        val draft = moved.toCanonicalDraft().copy(parentId = null, isSensitive = false)
        val replacement = replace(moved, draft)
        val items = listOf(sensitiveParent, moved, child)
        val pending = assertParity(items, authoring = listOf(replacement))
        assertTrue(pending[moved.id])
        assertTrue(pending[child.id])

        val publicMoved = moved.copy(parentId = null, isSensitive = false, revision = 2)
        val refreshed = listOf(sensitiveParent, publicMoved, child)
        val retainedBase = assertParity(refreshed, authoring = listOf(replacement))
        assertTrue(retainedBase[moved.id])
        assertTrue(retainedBase[child.id])
        val confirmed = assertParity(refreshed)
        assertFalse(confirmed[moved.id])
        assertFalse(confirmed[child.id])
    }

    @Test
    fun pendingCreatesAndLegacyMarksRaisePrivacyWithoutLoweringExistingMarks() {
        val publicRoot = item(1)
        val canonicalChild = item(2, parentId = publicRoot.id)
        val localChild = create(3, parentId = canonicalChild.id)
        val ownMarkedCreate = create(4, sensitive = true)
        val localGrandchild = create(5, parentId = ownMarkedCreate.itemId)
        val authoring = listOf(localGrandchild, localChild, ownMarkedCreate)
        val index = assertParity(
            listOf(publicRoot, canonicalChild),
            pending = mark(publicRoot.id, true),
            authoring = authoring,
        )
        (1..5).forEach { assertTrue(index[id(it)]) }
        val removal = assertParity(
            listOf(publicRoot.copy(isSensitive = true), canonicalChild),
            pending = mark(publicRoot.id, false),
            authoring = authoring,
        )
        assertTrue(removal[canonicalChild.id])
        assertTrue(removal[localChild.itemId])
        val unchanged = assertParity(listOf(publicRoot, canonicalChild), pending = mark(publicRoot.id, false))
        assertFalse(unchanged[publicRoot.id])
        assertFalse(unchanged[canonicalChild.id])
    }

    @Test
    fun malformedAuthoringJournalProtectsEveryItemEvenInUnrelatedBranches() {
        val items = listOf(item(1), item(2))
        val valid = create(3)
        val malformed = listOf(
            valid.copy(schemaVersion = -1),
            valid.copy(idempotencyKey = "not-the-journal-identity"),
            valid.copy(draft = null),
            valid.copy(draft = requireNotNull(valid.draft).copy(parentId = valid.itemId)),
            valid.copy(disposition = CanonicalAuthoringDisposition.CONFLICTED, diagnostic = null),
            valid.copy(operation = CanonicalAuthoringOperation.RESTORE),
        )
        malformed.forEach { mutation ->
            assertTrue(runCatching(mutation::requireValid).isFailure)
            val index = assertParity(items, authoring = listOf(mutation))
            (1..4).forEach { assertTrue(index[id(it)]) }
        }
    }

    @Test
    fun bodylessRestoreProtectsItsIdentityAndDescendantsWithoutHidingIndependentPublicRows() {
        val restoredId = id(1)
        val child = item(2, parentId = restoredId)
        val independent = item(3)
        val restore = PendingCanonicalAuthoringMutation(
            id = id(101), itemId = restoredId,
            operation = CanonicalAuthoringOperation.RESTORE,
            expectedRevision = 2, createdAt = NOW,
        )
        restore.requireValid()
        val index = assertParity(listOf(child, independent), authoring = listOf(restore))
        assertTrue(index[restoredId])
        assertTrue(index[child.id])
        assertFalse(index[independent.id])

        val knownBody = item(1).copy(deletedAt = NOW, revision = 2)
        val bodyBacked = restore.copy(baseItem = knownBody)
        bodyBacked.requireValid()
        val complete = assertParity(listOf(child, independent), authoring = listOf(bodyBacked))
        assertFalse(complete[restoredId])
        assertFalse(complete[child.id])
    }

    @Test
    fun retainedTrashBodyKeepsItsOldProtectiveAncestorPath() {
        val privateAncestor = item(1, sensitive = true)
        val retained = item(2, parentId = privateAncestor.id)
        val descendant = item(3, parentId = retained.id)
        val independent = item(4)
        val trash = PendingCanonicalAuthoringMutation(
            id = id(102), itemId = retained.id,
            operation = CanonicalAuthoringOperation.TRASH,
            expectedRevision = retained.revision, baseItem = retained, createdAt = NOW,
        )
        trash.requireValid()
        val index = assertParity(listOf(privateAncestor, descendant, independent), authoring = listOf(trash))
        assertTrue(index[retained.id])
        assertTrue(index[descendant.id])
        assertFalse(index[independent.id])
    }

    @Test
    fun missingParentsSelfCyclesAndRootlessCyclesProtectAllTheirDescendants() {
        val public = item(1)
        val missing = item(2, parentId = id(99))
        val missingChild = item(3, parentId = missing.id)
        val self = item(4, parentId = id(4))
        val selfChild = item(5, parentId = self.id)
        val cycleA = item(6, parentId = id(7))
        val cycleB = item(7, parentId = cycleA.id)
        val cycleChild = item(8, parentId = cycleB.id)
        val items = listOf(public, missing, missingChild, self, selfChild, cycleA, cycleB, cycleChild)
        val index = assertParity(items)
        assertFalse(index[public.id])
        items.drop(1).forEach { assertTrue(index[it.id]) }
    }

    @Test
    fun duplicateCanonicalIdentitiesUnionOwnMarksAndPossibleParentsInsteadOfLastWriteWinning() {
        val publicParent = item(1)
        val privateParent = item(2, sensitive = true)
        val duplicate = item(3, parentId = publicParent.id)
        val descendant = item(4, parentId = duplicate.id)
        val roots = listOf(publicParent, privateParent, descendant)
        val twoParents = assertParity(roots + duplicate + duplicate.copy(parentId = privateParent.id))
        assertFalse(twoParents[publicParent.id])
        assertTrue(twoParents[duplicate.id])
        assertTrue(twoParents[descendant.id])
        val ownMark = assertParity(roots + duplicate.copy(isSensitive = true) + duplicate)
        assertTrue(ownMark[descendant.id])
        val identicalPublicDuplicates = assertParity(roots + duplicate + duplicate)
        assertFalse(identicalPublicDuplicates[duplicate.id])
        assertFalse(identicalPublicDuplicates[descendant.id])
    }

    @Test
    fun generatedGraphsAndValidPendingOverlaysMatchExistingPerItemResolver() {
        repeat(32) { seed ->
            val random = Random(seed)
            val items = (1..24).map { value ->
                item(
                    value,
                    sensitive = random.nextInt(8) == 0,
                    parentId = if (value == 1 || random.nextInt(4) == 0) null else id(random.nextInt(1, value)),
                )
            }
            val replacements = items.shuffled(random).take(6).map { base ->
                val otherIds = items.map { it.id }.filter { it != base.id }
                replace(base, base.toCanonicalDraft().copy(
                    parentId = if (random.nextInt(3) == 0) null else otherIds.random(random),
                    isSensitive = random.nextInt(5) == 0,
                ))
            }
            val creates = (25..28).map { value ->
                create(value, sensitive = random.nextInt(5) == 0, parentId = id(random.nextInt(1, value)))
            }
            val authoring = replacements + creates
            authoring.forEach(PendingCanonicalAuthoringMutation::requireValid)
            val pending = if (seed % 2 == 0) mark(id(1), seed % 4 == 0) else null
            assertParity(items.shuffled(random), pending, authoring.shuffled(random))
        }
    }

    @Test
    fun tenThousandLevelBatchLookupIsIterativeForPublicSensitiveAndUnresolvedChains() {
        val items = (1..10_000).map { value ->
            item(value, parentId = if (value == 1) null else id(value - 1))
        }
        val public = CanonicalSensitivityIndex.build(items.reversed())
        items.forEach { assertFalse(public[it.id]) }
        assertTrue(public[id(10_001)])
        assertEquals(effectiveCanonicalSensitivity(items, id(10_000)), public[id(10_000)])

        val sensitive = CanonicalSensitivityIndex.build(
            items.map { if (it.id == id(1)) it.copy(isSensitive = true) else it },
        )
        items.forEach { assertTrue(sensitive[it.id]) }
        val missing = CanonicalSensitivityIndex.build(
            items.map { if (it.id == id(1)) it.copy(parentId = id(10_001)) else it },
        )
        items.forEach { assertTrue(missing[it.id]) }
        val cycle = CanonicalSensitivityIndex.build(
            items.map { if (it.id == id(1)) it.copy(parentId = id(10_000)) else it },
        )
        items.forEach { assertTrue(cycle[it.id]) }
    }

    private fun assertParity(
        items: List<CanonicalItemSnapshot>,
        pending: PendingCanonicalMutation? = null,
        authoring: List<PendingCanonicalAuthoringMutation> = emptyList(),
    ): CanonicalSensitivityIndex {
        val index = CanonicalSensitivityIndex.build(items, pending, authoring)
        val reversed = CanonicalSensitivityIndex.build(items.reversed(), pending, authoring.reversed())
        val ids = items.map { it.id } + authoring.map { it.itemId } + listOfNotNull(pending?.itemId) + id(99_999)
        ids.distinct().forEach { itemId ->
            val expected = effectiveCanonicalSensitivity(items, itemId, pending, authoring)
            assertEquals("Per-item oracle for $itemId", expected, index[itemId])
            assertEquals("Reversed source for $itemId", expected, reversed[itemId])
        }
        return index
    }

    private fun item(value: Int, sensitive: Boolean = false, parentId: String? = null) = CanonicalItemSnapshot(
        id = id(value), isSensitive = sensitive, kind = "task", status = "inbox", title = "Item $value",
        timezoneName = "UTC", flexibleConstraintsJson = "{}", splitPolicyJson = """{"type":"indivisible"}""",
        importance = 50, urgency = 50, parentId = parentId, siblingOrder = 0, isExecutable = true,
        revision = 1, createdAt = NOW, updatedAt = NOW,
    )

    private fun create(value: Int, sensitive: Boolean = false, parentId: String? = null) =
        PendingCanonicalAuthoringMutation(
            id = id(value + 1_000), itemId = id(value), operation = CanonicalAuthoringOperation.CREATE,
            draft = CanonicalItemDraft(title = "Created $value", timezoneName = "UTC", isSensitive = sensitive, parentId = parentId),
            createdAt = NOW,
        )

    private fun replace(base: CanonicalItemSnapshot, draft: CanonicalItemDraft) =
        PendingCanonicalAuthoringMutation(
            id = id(UUID.fromString(base.id).leastSignificantBits.toInt() + 2_000),
            itemId = base.id, operation = CanonicalAuthoringOperation.REPLACE,
            draft = draft, expectedRevision = base.revision, baseItem = base, createdAt = NOW,
        )

    private fun mark(itemId: String, sensitive: Boolean) = PendingCanonicalMutation(
        idempotencyKey = "synthetic-sensitivity-mark", syncOrigin = "https://example.test/",
        itemId = itemId, expectedRevision = 1, targetStatus = "inbox", targetIsSensitive = sensitive,
        startedAt = NOW, replacementRequestJson = "{}", focusedBlockId = id(10_000),
        displayStatus = ItemStatus.NOT_STARTED,
    )

    private fun id(value: Int): String = UUID(0, value.toLong()).toString()

    private companion object {
        const val NOW = "2026-08-30T10:00:00Z"
    }
}
