package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.toCanonicalDraft
import java.io.File
import java.util.UUID
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalHierarchyBrowserPresentationTest {
    @Test
    fun consumesSharedNativeProjectionContract() {
        val relative = "fixtures/hierarchy-browser/projection-v1.json"
        val fixture = generateSequence(File(requireNotNull(System.getProperty("user.dir")))) {
            it.parentFile
        }.map { File(it, relative) }.firstOrNull(File::isFile)
            ?: error("Missing shared hierarchy fixture: $relative")
        val document = Json.parseToJsonElement(fixture.readText()).jsonObject
        assertEquals("dayweave.hierarchy-browser-fixtures/1", document.getValue("schema").jsonPrimitive.content)
        val items = document.getValue("nodes").jsonArray.map { node ->
            val fields = node.jsonObject
            val status = fields.getValue("status").jsonPrimitive.content
            item(1, status = status).copy(
                id = fields.getValue("id").jsonPrimitive.content,
                parentId = fields.getValue("parent_id").jsonPrimitive.contentOrNull,
                siblingOrder = fields.getValue("sibling_order").jsonPrimitive.int.toLong(),
                title = fields.getValue("title").jsonPrimitive.content,
                kind = fields.getValue("kind").jsonPrimitive.content,
            )
        }
        val source = sourceRows(DayWeaveUiState(canonicalItems = items))
        document.getValue("cases").jsonArray.forEach { case ->
            val fields = case.jsonObject
            val name = fields.getValue("name").jsonPrimitive.content
            val kind = when (fields.getValue("scope").jsonPrimitive.content) {
                "goals" -> ItemKind.GOAL
                "projects" -> ItemKind.PROJECT
                else -> error("Unknown fixture scope")
            }
            val projection = CanonicalHierarchyBrowserPresentation.build(
                source,
                kind,
                query = fields.getValue("query").jsonPrimitive.content,
                collapsedIds = fixtureIds(fields.getValue("collapsed").jsonArray).toSet(),
            )
            assertEquals(
                "$name reversed raw browser input",
                projection,
                CanonicalHierarchyBrowserPresentation.build(
                    source.reversed(),
                    kind,
                    query = fields.getValue("query").jsonPrimitive.content,
                    collapsedIds = fixtureIds(fields.getValue("collapsed").jsonArray).toSet(),
                ),
            )
            assertEquals(name, fixtureIds(fields.getValue("visible").jsonArray), projection.ids())
            assertEquals(name, fields.getValue("depths").jsonArray.map { it.jsonPrimitive.int }, projection.rows.map { it.depth })
            assertEquals(name, fixtureIds(fields.getValue("scope_context").jsonArray), projection.rows.filter { it.isContext }.map { it.item.itemId })
            assertEquals(name, fixtureIds(fields.getValue("search_matches").jsonArray), projection.rows.filter { it.isSearchMatch }.map { it.item.itemId })
            assertEquals(name, if (kind == ItemKind.GOAL) 7 else 9, projection.eligibleCount)
        }
    }

    @Test
    fun unscheduledOutcomesRetainEveryCanonicalStatusAndUnsupportedDescendant() {
        val goal = item(1, kind = "goal", title = "Long-term outcome").copy(isExecutable = false)
        val statuses = listOf(
            "inbox", "planned", "scheduled", "in_progress", "paused", "blocked",
            "completed", "skipped", "cancelled", "future_status",
        )
        val children = statuses.mapIndexed { index, status ->
            item(index + 2, status = status, parentId = goal.id, title = "Action $status")
        }
        val unsupported = item(20, parentId = goal.id, title = "Future metadata").copy(
            flexibleConstraintsJson = """{"future_constraints":{"version":2}}""",
        )
        val state = DayWeaveUiState(canonicalItems = children + unsupported + goal)
        val browser = browser(state, ItemKind.GOAL)

        assertEquals(12, browser.eligibleCount)
        assertEquals((children + unsupported + goal).map { it.id }.toSet(), browser.ids().toSet())
        val rows = browser.rows.associateBy { it.item.itemId }
        val outcome = requireNotNull(rows[goal.id])
        assertEquals(CanonicalDurationKind.UNKNOWN, outcome.item.durationKind)
        assertNull(outcome.item.durationSeconds)
        assertTrue(outcome.hasChildren)
        assertFalse(outcome.isContext)
        children.forEach { child ->
            val row = requireNotNull(rows[child.id])
            assertEquals(child.status, row.item.status)
            assertEquals(1, row.depth)
            if (child.status !in setOf("inbox", "planned")) assertTrue(row.item.isReadOnly)
        }
        assertTrue(requireNotNull(rows[unsupported.id]).item.isReadOnly)
        assertTrue(state.schedule.isEmpty())
    }

    @Test
    fun projectSubtreeKeepsMixedKindsAndAncestorsWithoutUnrelatedRowsOrDuplicates() {
        val ancestor = item(1, title = "Context ancestor").copy(isExecutable = false)
        val project = item(2, kind = "project", parentId = ancestor.id).copy(isExecutable = false)
        val task = item(3, parentId = project.id).copy(isExecutable = false)
        val nestedProject = item(4, kind = "project", parentId = task.id).copy(isExecutable = false)
        val nestedGoal = item(5, kind = "goal", parentId = nestedProject.id)
        val unrelated = item(6, title = "Unrelated action")
        val canonical = listOf(ancestor, project, task, nestedProject, nestedGoal, unrelated)
        val first = browser(DayWeaveUiState(canonicalItems = canonical), ItemKind.PROJECT)
        val reversed = browser(DayWeaveUiState(canonicalItems = canonical.reversed()), ItemKind.PROJECT)

        assertEquals(first, reversed)
        assertEquals(listOf(ancestor.id, project.id, task.id, nestedProject.id, nestedGoal.id), first.ids())
        assertEquals(listOf(0, 1, 2, 3, 4), first.rows.map { it.depth })
        assertEquals(4, first.eligibleCount)
        assertEquals(listOf(ancestor.id), first.rows.filter { it.isContext }.map { it.item.itemId })
        assertEquals(1, first.rows.count { it.item.itemId == nestedProject.id })
        assertFalse(first.rows.last().hasChildren)
    }

    @Test
    fun collapsePreservesCountsAndSearchRevealsCompleteMatchingAncestry() {
        val context = item(1, title = "Outside context").copy(isExecutable = false)
        val goal = item(2, kind = "goal", parentId = context.id, title = "Outcome").copy(isExecutable = false)
        val branch = item(3, parentId = goal.id, title = "Intermediate step").copy(isExecutable = false)
        val target = item(4, parentId = branch.id, title = "Find the Needle")
        val sibling = item(5, parentId = goal.id, title = "Different action")
        val rows = sourceRows(DayWeaveUiState(canonicalItems = listOf(sibling, target, branch, goal, context)))
        val collapsed = CanonicalHierarchyBrowserPresentation.build(
            rows, ItemKind.GOAL, collapsedIds = setOf(goal.id),
        )
        assertEquals(listOf(context.id, goal.id), collapsed.ids())
        assertEquals(4, collapsed.eligibleCount)
        assertTrue(collapsed.rows.last().hasChildren)
        assertTrue(collapsed.rows.last().isCollapsed)

        val searched = CanonicalHierarchyBrowserPresentation.build(
            rows, ItemKind.GOAL, query = "needle", collapsedIds = setOf(context.id, goal.id, branch.id),
        )
        assertEquals(listOf(context.id, goal.id, branch.id, target.id), searched.ids())
        assertEquals(listOf(target.id), searched.rows.filter { it.isSearchMatch }.map { it.item.itemId })
        assertEquals(listOf(context.id), searched.rows.filter { it.isContext }.map { it.item.itemId })
        assertTrue(searched.rows.none { it.isCollapsed })
        assertEquals(4, searched.eligibleCount)

        val rootMatch = CanonicalHierarchyBrowserPresentation.build(rows, ItemKind.GOAL, query = "Outcome")
        assertEquals(listOf(context.id, goal.id), rootMatch.ids())
        val outsideMatch = CanonicalHierarchyBrowserPresentation.build(rows, ItemKind.GOAL, query = "Outside context")
        assertTrue(outsideMatch.rows.isEmpty())
        assertEquals(4, outsideMatch.eligibleCount)
    }

    @Test
    fun equalSiblingLabelsHaveDeterministicIdentityOrder() {
        val goal = item(1, kind = "goal").copy(isExecutable = false)
        val children = (2..6).map { item(it, parentId = goal.id, title = "Same label") }
        val first = browser(DayWeaveUiState(canonicalItems = children.reversed() + goal), ItemKind.GOAL)
        val second = browser(DayWeaveUiState(canonicalItems = listOf(goal) + children), ItemKind.GOAL)

        assertEquals(first, second)
        assertEquals(listOf(goal.id) + children.map { it.id }, first.ids())
    }

    @Test
    fun pendingMoveToRootUsesExplicitNullButRetainsOldAncestorPrivacy() {
        val oldParent = item(1, kind = "project", title = "Private ancestor").copy(
            isSensitive = true, isExecutable = false,
        )
        val moved = item(2, parentId = oldParent.id, title = "Moving branch").copy(isExecutable = false)
        val goal = item(3, kind = "goal", parentId = moved.id)
        val replacement = PendingCanonicalAuthoringMutation(
            id = id(100),
            itemId = moved.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = moved.toCanonicalDraft().copy(parentId = null),
            expectedRevision = moved.revision,
            baseItem = moved,
            createdAt = NOW,
        )
        val state = DayWeaveUiState(
            canonicalItems = listOf(oldParent, moved, goal),
            pendingCanonicalAuthoringMutations = listOf(replacement),
        )
        val projected = browser(state, ItemKind.GOAL)

        assertEquals(listOf(moved.id, goal.id), projected.ids())
        assertNull(projected.rows.first().item.parentId)
        assertEquals(listOf(0, 1), projected.rows.map { it.depth })
        assertEquals(CanonicalAuthoringRowSource.PENDING_REPLACE, projected.rows.first().item.source)
        assertEquals(CanonicalAuthoringSyncState.QUEUED, projected.rows.first().item.syncState)
        assertTrue(projected.rows.all { it.item.isSensitive })

        val confirmed = browser(
            state.copy(
                canonicalItems = listOf(oldParent, moved.copy(parentId = null, revision = 2), goal),
                pendingCanonicalAuthoringMutations = emptyList(),
            ),
            ItemKind.GOAL,
        )
        assertTrue(confirmed.rows.none { it.item.isSensitive })
        assertEquals(projected.ids(), confirmed.ids())
    }

    @Test
    fun queuedCreatesAppearWithoutScheduleAndPendingTrashDoesNotMasqueradeAsActiveWork() {
        val parent = create(1, title = "Queued parent")
        val goal = create(2, title = "Queued outcome", kind = ItemKind.GOAL, parentId = parent.itemId)
        val deleted = item(3, kind = "goal", title = "Queued deletion")
        val trash = PendingCanonicalAuthoringMutation(
            id = id(103),
            itemId = deleted.id,
            operation = CanonicalAuthoringOperation.TRASH,
            expectedRevision = deleted.revision,
            baseItem = deleted,
            createdAt = NOW,
        )
        val state = DayWeaveUiState(
            canonicalItems = listOf(deleted),
            pendingCanonicalAuthoringMutations = listOf(goal, trash, parent),
        )
        val authoring = CanonicalAuthoringPresentation.build(state)
        val browser = CanonicalHierarchyBrowserPresentation.build(authoring.hierarchyRows, ItemKind.GOAL)

        assertEquals(listOf(parent.itemId, goal.itemId), browser.ids())
        assertEquals(1, browser.eligibleCount)
        assertTrue(browser.rows.all { it.item.source == CanonicalAuthoringRowSource.LOCAL_CREATE })
        assertTrue(browser.rows.all { it.item.syncState == CanonicalAuthoringSyncState.QUEUED })
        assertEquals(listOf(deleted.id), authoring.recentlyDeleted.map { it.itemId })
        assertTrue(state.schedule.isEmpty())
    }

    @Test
    fun pendingMoveIntoSensitiveParentRaisesDescendantPrivacyImmediately() {
        val sensitive = item(1, kind = "project").copy(isSensitive = true)
        val moving = item(2).copy(isExecutable = false)
        val goal = item(3, kind = "goal", parentId = moving.id)
        val canonical = listOf(sensitive, moving, goal)
        assertTrue(browser(DayWeaveUiState(canonicalItems = canonical), ItemKind.GOAL).rows.none {
            it.item.isSensitive
        })
        val replacement = PendingCanonicalAuthoringMutation(
            id = id(100),
            itemId = moving.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = moving.toCanonicalDraft().copy(parentId = sensitive.id),
            expectedRevision = moving.revision,
            baseItem = moving,
            createdAt = NOW,
        )
        val browser = browser(
            DayWeaveUiState(
                canonicalItems = canonical,
                pendingCanonicalAuthoringMutations = listOf(replacement),
            ),
            ItemKind.GOAL,
        )
        assertEquals(listOf(sensitive.id, moving.id, goal.id), browser.ids())
        assertTrue(browser.rows.all { it.item.isSensitive })
        assertNull(moving.parentId)
        assertEquals(sensitive.id, browser.rows[1].item.parentId)
    }

    @Test
    fun submittedAndConflictedDraftsKeepTheirUnresolvedAuthorityVisible() {
        val submitted = create(1, title = "Submitted outcome", kind = ItemKind.GOAL).copy(
            syncOrigin = "https://example.test/", submittedAt = NOW,
        )
        val conflicted = create(2, title = "Conflicted outcome", kind = ItemKind.GOAL).copy(
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "Remote revision changed",
        )
        val browser = browser(
            DayWeaveUiState(pendingCanonicalAuthoringMutations = listOf(submitted, conflicted)),
            ItemKind.GOAL,
        )
        val rows = browser.rows.associateBy { it.item.itemId }

        assertEquals(2, browser.eligibleCount)
        assertEquals(CanonicalAuthoringSyncState.SUBMITTED, requireNotNull(rows[submitted.itemId]).item.syncState)
        assertEquals(CanonicalAuthoringSyncState.CONFLICTED, requireNotNull(rows[conflicted.itemId]).item.syncState)
        assertTrue(browser.rows.all { it.item.isReadOnly })
    }

    @Test
    fun missingAndCyclicAncestryRemainsVisibleReadOnlyAndSensitive() {
        val missing = item(1, kind = "goal", parentId = id(99))
        val selfCycle = item(2, kind = "goal", parentId = id(2)).copy(isExecutable = false)
        val cycleA = item(3, kind = "goal", parentId = id(4)).copy(isExecutable = false)
        val cycleB = item(4, parentId = id(3)).copy(isExecutable = false)
        val descendant = item(5, parentId = cycleB.id)
        val browser = browser(
            DayWeaveUiState(canonicalItems = listOf(descendant, cycleB, cycleA, selfCycle, missing)),
            ItemKind.GOAL,
        )
        val rows = browser.rows.associateBy { it.item.itemId }

        assertEquals(setOf(missing.id, selfCycle.id, cycleA.id, cycleB.id, descendant.id), rows.keys)
        assertTrue(requireNotNull(rows[missing.id]).item.hasMissingParent)
        assertTrue(requireNotNull(rows[missing.id]).item.isReadOnly)
        listOf(selfCycle, cycleA, cycleB).forEach { source ->
            val row = requireNotNull(rows[source.id]).item
            assertTrue(row.hasHierarchyCycle)
            assertTrue(row.isReadOnly)
        }
        assertTrue(browser.rows.all { it.item.isSensitive })
        assertEquals(browser.rows.size, browser.ids().distinct().size)
    }

    @Test
    fun cycleDescendantOrderedBeforeCycleMembersCannotBecomeEditableDuringFallbackTraversal() {
        val descendant = item(1, parentId = id(3), title = "First by identity")
        val cycleGoal = item(2, kind = "goal", parentId = id(3)).copy(isExecutable = false)
        val cycleTask = item(3, parentId = id(2)).copy(isExecutable = false)
        val items = listOf(descendant, cycleGoal, cycleTask)
        for (canonical in listOf(items, items.reversed())) {
            val source = sourceRows(DayWeaveUiState(canonicalItems = canonical))
            for (rows in listOf(source, source.reversed())) {
                val browser = CanonicalHierarchyBrowserPresentation.build(rows, ItemKind.GOAL)
                assertEquals(items.map { it.id }.toSet(), browser.ids().toSet())
                assertEquals(3, browser.rows.size)
                val first = browser.rows.single { it.item.itemId == descendant.id }.item
                // The child is not itself a cycle member. Its inherited unsafe
                // ancestry must still win before any UUID-ordered fallback walk.
                assertFalse(first.hasHierarchyCycle)
                assertFalse(first.hasMissingParent)
                assertTrue(first.isReadOnly)
                assertNull(first.editorRoute())
                assertTrue(browser.rows.all { it.item.isReadOnly })
                assertTrue(browser.rows.all { it.item.isSensitive })
            }
        }
    }

    @Test
    fun fiveThousandLevelBrowserTraversalAndSearchDoNotRequireRecursion() {
        // Isolate browser traversal from authoring metadata/privacy decoding:
        // every row already has its fully resolved presentation contract.
        val template = sourceRows(DayWeaveUiState(canonicalItems = listOf(item(1)))).single()
        val rows = (1..5_000).map { index ->
            template.copy(
                itemId = id(index),
                title = "Level $index",
                kind = if (index == 1) ItemKind.GOAL else ItemKind.TASK,
                parentId = if (index == 1) null else id(index - 1),
                depth = 0,
                breadcrumb = emptyList(),
            )
        }
        val browser = CanonicalHierarchyBrowserPresentation.build(rows.reversed(), ItemKind.GOAL)
        assertEquals(5_000, browser.eligibleCount)
        assertEquals(rows.map { it.itemId }, browser.ids())
        assertEquals(4_999, browser.rows.last().depth)
        assertEquals(4_999, browser.rows.count { it.hasChildren })

        val searched = CanonicalHierarchyBrowserPresentation.build(
            rows, ItemKind.GOAL, query = "Level 5000", collapsedIds = setOf(id(1), id(2)),
        )
        assertEquals(browser.ids(), searched.ids())
        assertEquals(listOf(id(5_000)), searched.rows.filter { it.isSearchMatch }.map { it.item.itemId })
        assertTrue(searched.rows.none { it.isCollapsed })
    }

    @Test
    fun fiveThousandCanonicalLevelsRetainFullTopologyAndPrivacyThroughAuthoringProjection() {
        val items = (1..5_000).map { index ->
            item(
                index,
                kind = if (index == 1) "goal" else "task",
                status = "inbox",
                parentId = if (index == 1) null else id(index - 1),
            ).copy(isExecutable = index == 5_000)
        }
        val authoring = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = items.reversed()),
        )
        val browser = CanonicalHierarchyBrowserPresentation.build(authoring.hierarchyRows, ItemKind.GOAL)

        assertEquals(5_000, authoring.hierarchyRows.size)
        assertEquals(items.map { it.id }, browser.ids())
        assertEquals(5_000, browser.eligibleCount)
        assertEquals(4_999, browser.rows.last().depth)
        assertEquals(4_999, browser.rows.last().item.depth)
        assertTrue(browser.rows.none { it.item.isSensitive })
        assertTrue(browser.rows.none { it.item.hasMissingParent || it.item.hasHierarchyCycle })
        assertTrue(browser.rows.all { it.item.breadcrumb.size <= 32 })
    }

    private fun browser(state: DayWeaveUiState, kind: ItemKind) =
        CanonicalHierarchyBrowserPresentation.build(sourceRows(state), kind)

    private fun sourceRows(state: DayWeaveUiState) =
        CanonicalAuthoringPresentation.build(state).hierarchyRows

    private fun CanonicalHierarchyBrowserPresentation.ids() = rows.map { it.item.itemId }

    private fun item(
        value: Int,
        kind: String = "task",
        status: String = "planned",
        parentId: String? = null,
        title: String = "Item $value",
    ) = CanonicalItemSnapshot(
        id = id(value),
        kind = kind,
        status = status,
        title = title,
        timezoneName = "UTC",
        flexibleConstraintsJson = "{}",
        splitPolicyJson = """{"type":"indivisible"}""",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = NOW,
        updatedAt = NOW,
        completedAt = NOW.takeIf { status == "completed" },
    )

    private fun create(
        value: Int,
        title: String,
        kind: ItemKind = ItemKind.TASK,
        parentId: String? = null,
    ) = PendingCanonicalAuthoringMutation(
        id = id(value + 100),
        itemId = id(value),
        operation = CanonicalAuthoringOperation.CREATE,
        draft = CanonicalItemDraft(title = title, kind = kind, parentId = parentId, timezoneName = "UTC"),
        createdAt = NOW,
    )

    private fun id(value: Int): String = UUID(0, value.toLong()).toString()

    private fun fixtureIds(values: JsonArray): List<String> = values.map {
        // Fixture suffixes are decimal-padded labels, not numeric UUID values.
        "00000000-0000-0000-0000-${it.jsonPrimitive.int.toString().padStart(12, '0')}"
    }

    private companion object {
        const val NOW = "2026-08-30T10:00:00Z"
    }
}
