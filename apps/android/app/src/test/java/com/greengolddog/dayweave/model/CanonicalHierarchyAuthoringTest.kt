package com.greengolddog.dayweave.model

import androidx.compose.ui.window.SecureFlagPolicy
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.ui.authoring.*
import java.time.Instant
import java.util.UUID
import org.junit.Assert.*
import org.junit.Test

class CanonicalHierarchyAuthoringTest {
    @Test
    fun genericRootAndNestedPresetsUseProfileAndNeverDesignateOnboarding() {
        val parent = item("parent").copy(isSensitive = true, kind = "project")
        val state = state(parent).copy(scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(timezoneName = "Europe/Istanbul"))
        listOf(ItemKind.GOAL, ItemKind.PROJECT).forEach { kind ->
            val route = CanonicalItemEditorRoute.hierarchy(state, kind)
            assertTrue(route.isHierarchyCreation)
            assertEquals("", route.initialDraft.title)
            assertEquals(kind, route.initialDraft.kind)
            assertEquals(CanonicalDraftPlacement.INBOX, route.initialDraft.placement)
            assertEquals("Europe/Istanbul", route.initialDraft.timezoneName)
            assertNull(route.initialDraft.durationSeconds)
            assertNull(route.initialDraft.recurrence)
            assertNull(route.initialDraft.parentId)
            assertNull(route.sourceInboxId)
        }
        val child = CanonicalItemEditorRoute.hierarchy(state, ItemKind.TASK, parent.id)
        val protectedOption = canonicalParentOptions(state, child.itemId).single()
        assertFalse(protectedOption.title.contains(parent.title))
        assertTrue(protectedOption.title.contains(parent.id.takeLast(8)))
        assertTrue(protectedOption.isSensitive)
        assertTrue(child.minimumSensitive)
        assertFalse(child.initialDraft.isSensitive)
        assertEquals(parent.id, child.initialDraft.parentId)
        val detached = CanonicalItemEditorForm.from(child.initialDraft).copy(title = "Reviewed child", parentId = null)
            .draft(child.itemId).getOrThrow()
        assertFalse(detached.isSensitive)
        assertEquals(SecureFlagPolicy.SecureOn, canonicalEditorSecurePolicy(false, false, child.minimumSensitive))
        assertEquals(SecureFlagPolicy.SecureOn, canonicalEditorSecurePolicy(true, false, false))
        assertEquals(SecureFlagPolicy.SecureOn, canonicalEditorSecurePolicy(false, true, false))
        assertEquals(SecureFlagPolicy.Inherit, canonicalEditorSecurePolicy(false, false, false))
        val store = PlannerStore(state)
        assertNotNull(store.enqueueCanonicalCreate(detached, child.itemId, id("child-create")))
        assertNull(store.state.value.onboardingFirstItemAnchor)
    }

    @Test
    fun blockedDirectParentIsAllowedWhileItsBodyRemainsReadOnlyAndTerminalAncestorIsContext() {
        val ancestor = item("ancestor").copy(status = "completed", completedAt = NOW)
        val blocked = item("parent", ancestor.id).copy(status = "blocked",
            blockedReasonKind = CanonicalBlockedReasonKind.MANUAL, blockedReason = "Waiting for review")
        val source = state(ancestor, blocked)
        assertNull(CanonicalHierarchyParentAuthority.build(source).issue(blocked.id, id("child")))
        assertNotNull(CanonicalHierarchyParentAuthority.build(source).issue(ancestor.id, id("child")))
        assertThrows(IllegalArgumentException::class.java) { blocked.requireCanonicalReplacementSupport() }
        assertNotNull(PlannerStore(source).enqueueCanonicalCreate(draft().copy(parentId = blocked.id), id("child"), id("create")))
    }

    @Test
    fun chosenMissingGrandparentCyclesTrashAndUnadmittedCacheCannotGrantAttachment() {
        val missing = item("parent", id("missing"))
        val cyclic = item("parent", id("ancestor"))
        val ancestor = item("ancestor", cyclic.id)
        listOf(state(missing), state(cyclic, ancestor), DayWeaveUiState(canonicalItems = listOf(item("parent"))),
            state(item("parent").copy(deletedAt = NOW))).forEach { source ->
            assertNotNull(CanonicalHierarchyParentAuthority.build(source).issue(id("parent"), id("child")))
            assertThrows(IllegalArgumentException::class.java) {
                PlannerStore(source).enqueueCanonicalCreate(draft().copy(parentId = id("parent")), id("child"), id("create"))
            }
        }
        // A malformed unrelated branch does not prevent ordinary detached Inbox capture.
        assertNotNull(PlannerStore(state(missing)).enqueueCanonicalCreate(draft(), id("detached"), id("capture")))
    }

    @Test
    fun localCreateAndExactPendingReplacementParentsSupportTopologicalOfflineCapture() {
        val store = PlannerStore(state(), nowEpochMillis = { Instant.parse(NOW).toEpochMilli() })
        val parent = requireNotNull(store.enqueueCanonicalCreate(draft().copy(kind = ItemKind.PROJECT), id("parent"), id("parent-create")))
        assertNull(CanonicalHierarchyParentAuthority.build(store.state.value).issue(parent.mutation.itemId, id("child")))
        assertNotNull(store.enqueueCanonicalCreate(draft().copy(parentId = parent.mutation.itemId), id("child"), id("child-create")))
        assertEquals(listOf(id("parent-create"), id("child-create")), store.sortedCanonicalAuthoringMutations().map { it.id })
        val canonical = item("canonical")
        val editing = PlannerStore(state(canonical), nowEpochMillis = { Instant.parse(NOW).toEpochMilli() })
        assertNotNull(editing.enqueueCanonicalReplace(canonical.id, canonical.toCanonicalDraft().copy(title = "Edited parent"), id("edit")))
        assertNotNull(editing.enqueueCanonicalCreate(draft().copy(parentId = canonical.id), id("child"), id("child-create")))
        assertEquals(listOf(id("edit"), id("child-create")), editing.sortedCanonicalAuthoringMutations().map { it.id })
    }

    @Test
    fun ambiguousOrStaleParentJournalCannotBeHiddenAtSaveOrSubmission() {
        val parent = item("parent")
        val edit = PendingCanonicalAuthoringMutation(id = id("edit"), itemId = parent.id,
            operation = CanonicalAuthoringOperation.REPLACE, draft = parent.toCanonicalDraft().copy(title = "Edited"),
            expectedRevision = parent.revision, baseItem = parent, createdAt = NOW)
        val conflict = edit.copy(disposition = CanonicalAuthoringDisposition.CONFLICTED, diagnostic = "Needs review")
        val bound = edit.copy(syncOrigin = ORIGIN, configurationId = CONFIGURATION)
        val submitted = bound.copy(submittedAt = NOW)
        val stale = edit.copy(baseItem = parent.copy(revision = 2), expectedRevision = 2)
        listOf(conflict, bound, submitted, stale).forEach { journal ->
            val source = state(parent).copy(pendingCanonicalAuthoringMutations = listOf(journal))
            assertNotNull(CanonicalHierarchyParentAuthority.build(source).issue(parent.id, id("child")))
            assertThrows(IllegalArgumentException::class.java) {
                PlannerStore(source).enqueueCanonicalCreate(draft().copy(parentId = parent.id), id("child"), id("create"))
            }
            val child = PendingCanonicalAuthoringMutation(id = id("child-create"), itemId = id("child"),
                operation = CanonicalAuthoringOperation.CREATE, draft = draft().copy(parentId = parent.id),
                createdAt = NOW, syncOrigin = ORIGIN, configurationId = CONFIGURATION)
            val recovery = PlannerStore(source.copy(pendingCanonicalAuthoringMutations = listOf(journal, child)))
            assertThrows(IllegalArgumentException::class.java) { recovery.markCanonicalAuthoringSubmitted(child.id) }
            assertEquals(child, recovery.state.value.pendingCanonicalAuthoringMutations.last())
        }
    }

    @Test
    fun boundUnsubmittedJournalCannotBeEditedOrRebasedByDerivedRevisionRefresh() {
        val parent = item("parent")
        val store = PlannerStore(state(parent), nowEpochMillis = { Instant.parse(NOW).toEpochMilli() })
        assertNotNull(store.enqueueCanonicalReplace(parent.id, parent.toCanonicalDraft().copy(title = "Edited"), id("edit")))
        val bound = requireNotNull(store.bindCanonicalAuthoringMutation(id("edit"), ORIGIN, CONFIGURATION)).mutation
        assertThrows(IllegalArgumentException::class.java) {
            store.updateCanonicalAuthoringDraft(bound.id, requireNotNull(bound.draft).copy(title = "Changed after bind"))
        }
        store.rebaseUnsubmittedCanonicalAuthoringBases(listOf(parent.copy(revision = 2, isExecutable = false)))
        assertEquals(bound, store.state.value.pendingCanonicalAuthoringMutations.single())
    }

    @Test
    fun activeExecutionStillAllowsOnlyDetachedInboxCapture() {
        val parent = item("parent")
        val execution = CanonicalExecutionSessionSnapshot(id = id("session"), itemId = parent.id,
            itemRevision = 1, sessionIndex = 0, sourceDeviceId = id("device"), status = "running", revision = 1,
            accumulatedSeconds = 0, startedAt = NOW, runningSince = NOW, createdAt = NOW, updatedAt = NOW)
        val source = state(parent).copy(canonicalExecutionSession = execution)
        assertThrows(IllegalArgumentException::class.java) {
            PlannerStore(source).enqueueCanonicalCreate(draft().copy(parentId = parent.id), id("nested"), id("create"))
        }
        assertNotNull(PlannerStore(source).enqueueCanonicalCreate(draft().copy(kind = ItemKind.PROJECT), id("root"), id("capture")))
    }

    private fun state(vararg items: CanonicalItemSnapshot) = DayWeaveUiState(canonicalItems = items.toList(),
        canonicalConfigurationId = CONFIGURATION, canonicalSyncOrigin = ORIGIN)
    private fun draft() = CanonicalItemDraft(title = "Reviewed item", timezoneName = "UTC")
    private fun item(name: String, parent: String? = null) = CanonicalItemSnapshot(id = id(name), isSensitive = false,
        kind = "task", status = "planned", title = "Synthetic $name", timezoneName = "UTC", durationSeconds = 1800,
        flexibleConstraintsJson = "{}", splitPolicyJson = """{"type":"indivisible"}""", importance = 50, urgency = 50,
        parentId = parent, siblingOrder = 0, isExecutable = true, revision = 1, createdAt = NOW, updatedAt = NOW)
    private fun id(name: String) = UUID.nameUUIDFromBytes(name.toByteArray()).toString()

    private companion object {
        const val NOW = "2026-09-01T00:00:00Z"
        const val ORIGIN = "https://planner.synthetic.invalid"
        const val CONFIGURATION = "synthetic-bound-configuration"
    }
}
