package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.ScheduleItem
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class PlannerStateRepositoryTest {
    @Test
    fun legacyV2PayloadDefaultsSensitivityAndIsRewrittenAsV3() = runBlocking {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 7,
                payloadFormat = PlannerSnapshotFormats.JSON_V2,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 11 }

        val restored = requireNotNull(repository.load())

        assertFalse(restored.schedule.single().isSensitive)
        assertFalse(restored.canonicalItems.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V3, dao.snapshot?.payloadFormat)
        assertEquals(11L, dao.snapshot?.updatedAtEpochMillis)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":false"))
    }

    @Test
    fun sensitiveCanarySurvivesEncryptedSnapshotRoundTrip() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 13 }
        val state = DayWeaveUiState(
            schedule = listOf(
                ScheduleItem(
                    id = "SYNTHETIC-SENSITIVE-BLOCK-ANDROID",
                    isSensitive = true,
                    title = "SYNTHETIC-SENSITIVE-BLOCK-TITLE",
                    kind = ItemKind.TASK,
                    startMinute = 540,
                    durationMinutes = 30,
                    status = ItemStatus.SCHEDULED,
                ),
            ),
            canonicalItems = listOf(sensitiveCanonicalItem()),
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertTrue(restored.schedule.single().isSensitive)
        assertTrue(restored.canonicalItems.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V3, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":true"))
    }

    @Test
    fun currentV3PayloadMissingSensitivityFailsClosed() {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 17,
                payloadFormat = PlannerSnapshotFormats.JSON_V3,
            ),
        )
        val repository = RoomPlannerStateRepository(dao)

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(PlannerSnapshotFormats.JSON_V3, dao.snapshot?.payloadFormat)
        assertEquals(17L, dao.snapshot?.updatedAtEpochMillis)
    }

    private fun sensitiveCanonicalItem() = CanonicalItemSnapshot(
        id = "SYNTHETIC-SENSITIVE-CANONICAL-ANDROID",
        isSensitive = true,
        kind = "task",
        status = "planned",
        title = "SYNTHETIC-SENSITIVE-CANONICAL-TITLE",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-29T08:00:00Z",
        updatedAt = "2026-08-29T08:00:00Z",
    )

    private class FakePlannerSnapshotDao(
        var snapshot: PlannerSnapshotEntity? = null,
    ) : PlannerSnapshotDao {
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
        }
    }

    private companion object {
        const val LEGACY_V2_PAYLOAD = """
            {
              "schedule": [{
                "id": "SYNTHETIC-LEGACY-V2-BLOCK",
                "title": "SYNTHETIC-LEGACY-V2-BLOCK-TITLE",
                "kind": "TASK",
                "startMinute": 540,
                "durationMinutes": 30,
                "status": "SCHEDULED"
              }],
              "canonicalItems": [{
                "id": "SYNTHETIC-LEGACY-V2-CANONICAL",
                "kind": "task",
                "status": "planned",
                "title": "SYNTHETIC-LEGACY-V2-CANONICAL-TITLE",
                "timezoneName": "UTC",
                "durationSeconds": 1800,
                "flexibleConstraintsJson": "{}",
                "splitPolicyJson": "{\"type\":\"indivisible\"}",
                "importance": 50,
                "urgency": 50,
                "siblingOrder": 0,
                "isExecutable": true,
                "revision": 1,
                "createdAt": "2026-08-29T08:00:00Z",
                "updatedAt": "2026-08-29T08:00:00Z"
              }]
            }
        """
    }
}
