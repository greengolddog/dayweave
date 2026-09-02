package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.CanonicalTrashRetentionPolicy
import java.time.Instant
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringPersistenceTest {
    @Test
    fun v6SnapshotMigratesToEmptyAuthoringCollectionsAndIsRewrittenAsV11() = runBlocking {
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { 41 }
        repository.save(DayWeaveUiState())
        val current = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload) as JsonObject
        val legacy = JsonObject(
            current.filterKeys {
                it != "pendingCanonicalAuthoringMutations" && it != "canonicalRecentlyDeleted"
            },
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(JsonObject.serializer(), legacy),
            payloadFormat = PlannerSnapshotFormats.JSON_V6,
            updatedAtEpochMillis = 7,
        )

        val restored = requireNotNull(repository.load())

        assertTrue(restored.pendingCanonicalAuthoringMutations.isEmpty())
        assertTrue(restored.canonicalRecentlyDeleted.isEmpty())
        assertEquals(PlannerSnapshotFormats.JSON_V11, dao.snapshot?.payloadFormat)
        assertEquals(41L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun exactSubmittedJournalAndRecentlyDeletedItemRoundTrip() = runBlocking {
        val mutation = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft(),
            createdAt = "2026-08-30T10:00:00Z",
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            submittedAt = "2026-08-30T10:01:00Z",
        )
        val deletedItem = canonicalItem(DELETED_ID).copy(
            revision = 8,
            isExecutable = false,
            updatedAt = "2026-08-30T09:00:00Z",
            deletedAt = "2026-08-30T09:00:00Z",
        )
        val deleted = CanonicalRecentlyDeletedRecord(
            id = deletedItem.id,
            revision = deletedItem.revision,
            deletedAt = requireNotNull(deletedItem.deletedAt),
            parentId = deletedItem.parentId,
            lastKnownItem = deletedItem,
            retentionAnchorAt = requireNotNull(deletedItem.deletedAt),
        )
        val state = DayWeaveUiState(
            canonicalSyncOrigin = ORIGIN,
            canonicalConfigurationId = CONFIGURATION_ID,
            pendingCanonicalAuthoringMutations = listOf(mutation),
            canonicalRecentlyDeleted = listOf(deleted),
        )
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { REFERENCE_MILLIS }

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(mutation, restored.pendingCanonicalAuthoringMutations.single())
        assertEquals(deleted, restored.canonicalRecentlyDeleted.single())
        assertTrue(requireNotNull(dao.snapshot).payload.contains("Canonical Android draft"))
        assertEquals(PlannerSnapshotFormats.JSON_V11, dao.snapshot?.payloadFormat)
    }

    @Test
    fun malformedJournalAndCurrentSnapshotMissingCollectionsFailClosed() = runBlocking {
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao)
        val malformed = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft(),
            idempotencyKey = "different-content",
            createdAt = "2026-08-30T10:00:00Z",
        )
        assertThrows(SerializationException::class.java) {
            runBlocking {
                repository.save(
                    DayWeaveUiState(pendingCanonicalAuthoringMutations = listOf(malformed)),
                )
            }
        }

        repository.save(DayWeaveUiState())
        val current = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload) as JsonObject
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(current - "canonicalRecentlyDeleted"),
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun pendingAuthoringCannotPersistWithACurrentPlanDigest() = runBlocking {
        val mutation = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft(),
            createdAt = "2026-08-30T10:00:00Z",
        )
        val repository = RoomPlannerStateRepository(FakeDao()) { REFERENCE_MILLIS }

        assertThrows(SerializationException::class.java) {
            runBlocking {
                repository.save(
                    DayWeaveUiState(
                        pendingCanonicalAuthoringMutations = listOf(mutation),
                        scheduleInputDigest = "sha256:${"a".repeat(64)}",
                    ),
                )
            }
        }
        Unit
    }

    @Test
    fun conflictedChildDraftWithDeletedParentRoundTripsOutsideMaterializedHierarchy() = runBlocking {
        val deletedParent = deletedRecord(DELETED_ID, "2026-08-30T09:00:00Z")
        val conflictedChild = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft().copy(parentId = deletedParent.id),
            createdAt = "2026-08-30T10:00:00Z",
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "The selected parent was deleted remotely; review this draft",
        )
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { REFERENCE_MILLIS }

        repository.save(
            DayWeaveUiState(
                pendingCanonicalAuthoringMutations = listOf(conflictedChild),
                canonicalRecentlyDeleted = listOf(deletedParent),
            ),
        )
        val restored = requireNotNull(repository.load())

        assertEquals(conflictedChild, restored.pendingCanonicalAuthoringMutations.single())
        assertEquals(deletedParent.id, restored.canonicalRecentlyDeleted.single().id)
        assertTrue(restored.canonicalItems.isEmpty())
    }

    @Test
    fun currentSnapshotPrunesExpiredTrashButPinsBodylessRestoreMetadata() = runBlocking {
        val expired = deletedRecord(
            id = DELETED_ID,
            deletedAt = Instant.ofEpochMilli(
                REFERENCE_MILLIS -
                    (CanonicalTrashRetentionPolicy.RETENTION_SECONDS + 1L) * 1_000L,
            ).toString(),
        )
        val pinned = deletedRecord(
            id = PINNED_DELETED_ID,
            deletedAt = Instant.ofEpochMilli(
                REFERENCE_MILLIS -
                    (CanonicalTrashRetentionPolicy.RETENTION_SECONDS + 2L) * 1_000L,
            ).toString(),
        )
        val restore = PendingCanonicalAuthoringMutation(
            id = RESTORE_MUTATION_ID,
            itemId = pinned.id,
            operation = CanonicalAuthoringOperation.RESTORE,
            expectedRevision = pinned.revision,
            baseItem = pinned.lastKnownItem,
            createdAt = pinned.deletedAt,
        )
        val dao = FakeDao()
        var clock = Instant.parse(pinned.deletedAt).toEpochMilli() + 1_000L
        val repository = RoomPlannerStateRepository(dao) { clock }
        repository.save(
            DayWeaveUiState(
                pendingCanonicalAuthoringMutations = listOf(restore),
                canonicalRecentlyDeleted = listOf(expired, pinned),
            ),
        )
        clock = REFERENCE_MILLIS

        val restored = requireNotNull(repository.load())

        assertEquals(1, restored.canonicalRecentlyDeleted.size)
        assertEquals(pinned.id, restored.canonicalRecentlyDeleted.single().id)
        assertEquals(null, restored.canonicalRecentlyDeleted.single().lastKnownItem)
        assertEquals(null, restored.pendingCanonicalAuthoringMutations.single().baseItem)
        assertTrue(requireNotNull(dao.snapshot).payload.contains(PINNED_DELETED_ID))
        assertTrue(!requireNotNull(dao.snapshot).payload.contains(DELETED_ID))
    }

    private fun draft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.INBOX,
        kind = ItemKind.TASK,
        isSensitive = true,
        title = "Canonical Android draft",
        timezoneName = "UTC",
        durationSeconds = 1_800,
    )

    private fun canonicalItem(id: String) = CanonicalItemSnapshot(
        id = id,
        isSensitive = true,
        kind = "task",
        status = "planned",
        title = "Deleted canonical item",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 7,
        createdAt = "2026-08-30T08:00:00Z",
        updatedAt = "2026-08-30T08:00:00Z",
    )

    private fun deletedRecord(id: String, deletedAt: String): CanonicalRecentlyDeletedRecord {
        val item = canonicalItem(id).copy(
            revision = 8,
            isExecutable = false,
            createdAt = Instant.parse(deletedAt).minusSeconds(60).toString(),
            updatedAt = deletedAt,
            deletedAt = deletedAt,
        )
        return CanonicalRecentlyDeletedRecord(
            id = id,
            revision = item.revision,
            deletedAt = deletedAt,
            lastKnownItem = item,
            retentionAnchorAt = deletedAt,
        )
    }

    private class FakeDao(
        var snapshot: PlannerSnapshotEntity? = null,
    ) : PlannerSnapshotDao {
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
        }
    }

    private companion object {
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION_ID = "connection-1"
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val DELETED_ID = "22222222-2222-4222-8222-222222222222"
        const val MUTATION_ID = "33333333-3333-4333-8333-333333333333"
        const val PINNED_DELETED_ID = "44444444-4444-4444-8444-444444444444"
        const val RESTORE_MUTATION_ID = "55555555-5555-4555-8555-555555555555"
        const val REFERENCE_MILLIS = 1_788_086_400_000L
    }
}
