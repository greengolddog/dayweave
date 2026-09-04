package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.GoogleSchedulePublicationJournal
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleSchedulePublicationPersistenceTest {
    @Test
    fun v14RoundTripsScheduleRecoveryAndV13CannotAcquireInjectedAuthority() = runBlocking {
        val dao = SchedulePublicationFakeDao()
        val repository = RoomPlannerStateRepository(dao) {
            java.time.Instant.parse("2026-09-03T12:01:00Z").toEpochMilli()
        }
        val journal = validJournal()
        repository.save(
            DayWeaveUiState(
                canonicalSyncOrigin = journal.apiBaseUrl,
                canonicalConfigurationId = journal.configurationId,
                pendingGoogleSchedulePublication = journal,
            ),
        )

        assertEquals(journal, repository.load()?.pendingGoogleSchedulePublication)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)

        val current = requireNotNull(dao.snapshot)
        assertTrue(
            Json.parseToJsonElement(current.payload).jsonObject
                .containsKey("pendingGoogleSchedulePublication"),
        )
        dao.snapshot = current.copy(payloadFormat = PlannerSnapshotFormats.JSON_V13)

        assertNull(repository.load()?.pendingGoogleSchedulePublication)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
    }

    @Test
    fun v14RequiresExplicitScheduleRecoveryRootField() = runBlocking {
        val dao = SchedulePublicationFakeDao()
        val repository = RoomPlannerStateRepository(dao) { 0 }
        repository.save(DayWeaveUiState())
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root - "pendingGoogleSchedulePublication"),
            ),
        )

        org.junit.Assert.assertThrows(kotlinx.serialization.SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    private fun validJournal() = GoogleSchedulePublicationJournal(
        recoveryId = "11111111-1111-4111-8111-111111111111",
        operationGeneration = 1,
        configurationId = "test-binding",
        apiBaseUrl = "https://api.example.test/",
        accountId = "22222222-2222-4222-8222-222222222222",
        collectionId = "33333333-3333-4333-8333-333333333333",
        expectedScheduleRevisionId = "44444444-4444-4444-8444-444444444444",
        intentExpiresAt = "2026-09-03T12:30:00Z",
        createdAt = "2026-09-03T12:00:00Z",
    )
}

private class SchedulePublicationFakeDao : PlannerSnapshotDao {
    var snapshot: PlannerSnapshotEntity? = null

    override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

    override suspend fun save(snapshot: PlannerSnapshotEntity) {
        this.snapshot = snapshot
    }
}
