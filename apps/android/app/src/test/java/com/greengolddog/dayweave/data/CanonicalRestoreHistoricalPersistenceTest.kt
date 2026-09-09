package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import java.time.Instant
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalRestoreHistoricalPersistenceTest {
    @Test
    fun submittedPendingRestoreRestartsExactlyBesideNewerRetainedTombstone() = runBlocking {
        for (retainNewerBody in listOf(false, true)) {
            val state = state(retainNewerBody)
            val mutation = state.pendingCanonicalAuthoringMutations.single()
            val dao = SnapshotDao()
            RoomPlannerStateRepository(dao) { NOW_MILLIS }.save(state)
            val saved = requireNotNull(dao.snapshot)
            val originalJournal = Json.parseToJsonElement(saved.payload).jsonObject
                .getValue("pendingCanonicalAuthoringMutations")

            // Only serialized state crosses this fresh repository/DAO boundary.
            val restartedDao = SnapshotDao(saved.copy())
            val restarted = RoomPlannerStateRepository(restartedDao) { NOW_MILLIS }
            val restored = requireNotNull(restarted.load())

            assertEquals(mutation, restored.pendingCanonicalAuthoringMutations.single())
            assertTrue(restored.pendingCanonicalAuthoringMutations.single().isSubmitted)
            assertEquals(CanonicalAuthoringDisposition.PENDING, mutation.disposition)
            assertEquals(2L, mutation.expectedRevision)
            assertEquals(4L, restored.canonicalRecentlyDeleted.single().revision)
            assertEquals(state.canonicalRecentlyDeleted, restored.canonicalRecentlyDeleted)
            assertTrue(restored.canonicalItems.isEmpty())
            assertEquals(state.canonicalDeltaCursor, restored.canonicalDeltaCursor)
            assertEquals(state.canonicalConfigurationId, restored.canonicalConfigurationId)
            assertEquals(state.canonicalSyncOrigin, restored.canonicalSyncOrigin)

            restarted.save(restored)
            assertEquals(
                originalJournal,
                Json.parseToJsonElement(requireNotNull(restartedDao.snapshot).payload).jsonObject
                    .getValue("pendingCanonicalAuthoringMutations"),
            )
        }
    }

    @Test
    fun neverSubmittedRestoreStillRejectsNewerDeletedRevision() = runBlocking {
        val submitted = state(retainNewerBody = true)
        val unsubmitted = submitted.copy(
            pendingCanonicalAuthoringMutations = submitted.pendingCanonicalAuthoringMutations.map {
                it.copy(submittedAt = null)
            },
        )
        val dao = SnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { NOW_MILLIS }
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.save(unsubmitted) }
        }
        assertEquals(null, dao.snapshot)
    }

    private fun state(retainNewerBody: Boolean): DayWeaveUiState {
        val base = CanonicalItemSnapshot(
            id = ITEM_ID,
            isSensitive = true,
            kind = "task",
            status = "planned",
            title = "Synthetic original deleted item",
            timezoneName = "UTC",
            durationSeconds = 1_800,
            flexibleConstraintsJson = "{}",
            splitPolicyJson = "{\"type\":\"indivisible\"}",
            importance = 50,
            urgency = 50,
            siblingOrder = 0,
            isExecutable = false,
            revision = 2,
            createdAt = "2026-08-30T08:00:00Z",
            updatedAt = "2026-08-30T09:00:00Z",
            deletedAt = "2026-08-30T09:00:00Z",
        )
        val newer = base.copy(
            revision = 4,
            title = "Synthetic subsequently deleted item",
            updatedAt = "2026-08-30T11:00:00Z",
            deletedAt = "2026-08-30T11:00:00Z",
        )
        return DayWeaveUiState(
            canonicalSyncOrigin = "https://api.example.test/",
            canonicalConfigurationId = "synthetic-restore-connection",
            canonicalDeltaCursor = "synthetic-newer-tombstone-cursor",
            pendingCanonicalAuthoringMutations = listOf(
                PendingCanonicalAuthoringMutation(
                    id = "77000000-2222-4333-8444-200000000002",
                    itemId = ITEM_ID,
                    operation = CanonicalAuthoringOperation.RESTORE,
                    expectedRevision = base.revision,
                    baseItem = base,
                    createdAt = "2026-08-30T10:00:00Z",
                    syncOrigin = "https://api.example.test/",
                    configurationId = "synthetic-restore-connection",
                    submittedAt = "2026-08-30T10:01:00Z",
                ),
            ),
            canonicalRecentlyDeleted = listOf(
                CanonicalRecentlyDeletedRecord(
                    id = ITEM_ID,
                    revision = newer.revision,
                    deletedAt = requireNotNull(newer.deletedAt),
                    lastKnownItem = newer.takeIf { retainNewerBody },
                    retentionAnchorAt = requireNotNull(newer.deletedAt),
                ),
            ),
        )
    }

    private class SnapshotDao(var snapshot: PlannerSnapshotEntity? = null) : PlannerSnapshotDao {
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
        }
    }

    private companion object {
        const val ITEM_ID = "77000000-2222-4333-8444-200000000001"
        val NOW_MILLIS = Instant.parse("2026-08-30T12:00:00Z").toEpochMilli()
    }
}
