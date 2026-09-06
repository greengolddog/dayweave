package com.greengolddog.dayweave.ui.screens

import com.greengolddog.dayweave.data.PlannerSnapshotDao
import com.greengolddog.dayweave.data.PlannerSnapshotEntity
import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.model.AppDestination
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.toCanonicalDraft
import com.greengolddog.dayweave.sync.CanonicalSyncPhase
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalHierarchyBrowserStateTest {
    @Test
    fun unboundCursorAndCachedRowsAreNotEvidenceOfALoadedWorkspace() {
        val state = DayWeaveUiState(
            canonicalItems = listOf(item(1)),
            canonicalDeltaCursor = "retained-old-cursor",
        )
        for (phase in CanonicalSyncPhase.entries) {
            val message = hierarchyCacheMessage(state, phase)
            assertTrue(message.contains("workspace not loaded"))
            assertFalse(message.contains("Canonical hierarchy ·"))
            assertTrue(hierarchyAdmittedState(state).canonicalItems.isEmpty())
        }
    }

    @Test
    fun missingInitialCursorNeverClaimsAnEmptyOrCompleteHierarchy() {
        val state = boundState(cursor = null)
        assertTrue(hierarchyCacheMessage(state, CanonicalSyncPhase.SYNCING).startsWith("Loading"))
        for (phase in listOf(CanonicalSyncPhase.CONNECTED, CanonicalSyncPhase.OFFLINE, CanonicalSyncPhase.ERROR)) {
            val message = hierarchyCacheMessage(state, phase)
            assertTrue(message.contains("Initial hierarchy sync is incomplete"))
            assertTrue(message.contains("cached items and drafts"))
            assertFalse(message.contains("workspace yet"))
        }
        assertEquals(state, hierarchyAdmittedState(state))
    }

    @Test
    fun admittedOfflineAndFailedRefreshStateKeepsItsActualCacheAndQueue() {
        val state = boundState().copy(
            canonicalItems = listOf(item(1)),
            pendingCanonicalAuthoringMutations = listOf(create(2).copy(
                syncOrigin = ORIGIN,
                configurationId = BINDING,
            )),
        )
        assertEquals(state, hierarchyAdmittedState(state))
        assertTrue(hierarchyCacheMessage(state, CanonicalSyncPhase.OFFLINE).startsWith("Offline"))
        assertTrue(hierarchyCacheMessage(state, CanonicalSyncPhase.SYNCING).contains("remain available"))
        for (phase in listOf(CanonicalSyncPhase.ERROR, CanonicalSyncPhase.AUTH_REQUIRED, CanonicalSyncPhase.READY)) {
            val message = hierarchyCacheMessage(state, phase)
            assertTrue(message.contains("Saved hierarchy"))
            assertTrue(message.contains("refresh unavailable"))
            assertTrue(message.contains("drafts are retained"))
        }
        assertTrue(hierarchyCacheMessage(state, CanonicalSyncPhase.CONNECTED).contains("includes unscheduled items"))
    }

    @Test
    fun unboundAdmissionRetainsOnlyUnsubmittedLocalCreateDrafts() {
        val active = item(1)
        val deleted = item(2).copy(revision = 2, deletedAt = NOW)
        val local = create(10)
        val bound = create(11).copy(syncOrigin = ORIGIN, configurationId = BINDING)
        // Legacy submitted journals can have an origin but no configuration ID.
        // They are still account-bound content, not new unbound capture drafts.
        val legacySubmitted = create(12).copy(syncOrigin = ORIGIN, submittedAt = NOW)
        val legacyOriginOnly = create(13).copy(syncOrigin = ORIGIN)
        val replacement = PendingCanonicalAuthoringMutation(
            id = id(101), itemId = active.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = active.toCanonicalDraft().copy(title = "Queued replacement"),
            expectedRevision = active.revision, baseItem = active, createdAt = NOW,
        )
        val restore = PendingCanonicalAuthoringMutation(
            id = id(102), itemId = deleted.id,
            operation = CanonicalAuthoringOperation.RESTORE,
            expectedRevision = deleted.revision, baseItem = deleted, createdAt = NOW,
        )
        val trashTarget = item(3)
        val trash = PendingCanonicalAuthoringMutation(
            id = id(103), itemId = trashTarget.id,
            operation = CanonicalAuthoringOperation.TRASH,
            expectedRevision = trashTarget.revision, baseItem = trashTarget, createdAt = NOW,
        )
        val state = DayWeaveUiState(
            canonicalItems = listOf(active, trashTarget),
            canonicalRecentlyDeleted = listOf(CanonicalRecentlyDeletedRecord(
                id = deleted.id, revision = deleted.revision, deletedAt = NOW,
                lastKnownItem = deleted, retentionAnchorAt = NOW,
            )),
            pendingCanonicalAuthoringMutations = listOf(
                local, bound, legacySubmitted, legacyOriginOnly, replacement, restore, trash,
            ),
        )
        val admitted = hierarchyAdmittedState(state)
        assertTrue(admitted.canonicalItems.isEmpty())
        assertTrue(admitted.canonicalRecentlyDeleted.isEmpty())
        assertEquals(listOf(local), admitted.pendingCanonicalAuthoringMutations)
        assertEquals(2, state.canonicalItems.size)
        assertEquals(7, state.pendingCanonicalAuthoringMutations.size)
    }

    @Test
    fun goalsAndProjectsDestinationsRoundTripThroughRealSnapshotPersistence() = runBlocking {
        val dao = FakeDao()
        for (destination in listOf(AppDestination.GOALS, AppDestination.PROJECTS)) {
            val source = DayWeaveUiState(destination = destination)
            RoomPlannerStateRepository(dao) { REFERENCE_MILLIS }.save(source)
            val restored = requireNotNull(RoomPlannerStateRepository(dao) { REFERENCE_MILLIS }.load())
            assertEquals(destination, restored.destination)
            assertTrue(restored.canonicalItems.isEmpty())
            assertTrue(restored.pendingCanonicalAuthoringMutations.isEmpty())
        }
        assertEquals(2, dao.saves)
    }

    private fun boundState(cursor: String? = "admitted-item-cursor") = DayWeaveUiState(
        canonicalSyncOrigin = ORIGIN,
        canonicalConfigurationId = BINDING,
        canonicalDeltaCursor = cursor,
    )

    private fun item(value: Int) = CanonicalItemSnapshot(
        id = id(value), kind = "task", status = "inbox", title = "Item $value",
        timezoneName = "UTC", flexibleConstraintsJson = "{}",
        splitPolicyJson = """{"type":"indivisible"}""",
        importance = 50, urgency = 50, siblingOrder = 0, isExecutable = true,
        revision = 1, createdAt = NOW, updatedAt = NOW,
    )

    private fun create(value: Int) = PendingCanonicalAuthoringMutation(
        id = id(value + 100), itemId = id(value), operation = CanonicalAuthoringOperation.CREATE,
        draft = CanonicalItemDraft(title = "Local outcome $value", kind = ItemKind.GOAL, timezoneName = "UTC"),
        createdAt = NOW,
    )

    private fun id(value: Int): String = UUID(0, value.toLong()).toString()

    private class FakeDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        var saves = 0

        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
            saves += 1
        }
    }

    private companion object {
        const val ORIGIN = "https://example.test/"
        const val BINDING = "synthetic-binding"
        const val NOW = "2026-08-30T10:00:00Z"
        val REFERENCE_MILLIS = Instant.parse(NOW).toEpochMilli()
    }
}
