package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ScheduleProfilePersistenceTest {
    @Test
    fun legacyEncryptedPayloadKeepsExactDailyWindowAndUsesCompatibilityDefaults() = runBlocking {
        val legacy = ScheduleCompositionProfileSnapshot(
            firmHorizonDays = 12,
            dayStartMinute = 8 * 60 + 15,
            dayEndMinute = 19 * 60 + 45,
            slotGranularityMinutes = 15,
        )
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 42L }

        repository.save(DayWeaveUiState(scheduleCompositionProfile = legacy))
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val profileJson = root.getValue("scheduleCompositionProfile").jsonObject
        val legacyProfileJson = JsonObject(
            profileJson - setOf("timezoneName", "availability", "sleep", "protectedTime"),
        )
        dao.snapshot = stored.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root + ("scheduleCompositionProfile" to legacyProfileJson)),
            ),
        )

        assertFalse(legacyProfileJson.containsKey("timezoneName"))
        assertFalse(legacyProfileJson.containsKey("availability"))
        assertFalse(legacyProfileJson.containsKey("sleep"))
        assertFalse(legacyProfileJson.containsKey("protectedTime"))
        assertEquals(legacy, requireNotNull(repository.load()).scheduleCompositionProfile)
        assertEquals(PlannerSnapshotFormats.JSON_V17, dao.snapshot?.payloadFormat)
    }

    @Test
    fun richProfileRoundTripsEveryFieldInsideCurrentEncryptedPayload() = runBlocking {
        val rich = requireNotNull(
            ScheduleCompositionProfileSnapshot(
                firmHorizonDays = 9,
                dayStartMinute = 8 * 60,
                dayEndMinute = 18 * 60,
            ).upgradedToWeeklySchedule("Europe/Paris"),
        )
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 43L }

        repository.save(DayWeaveUiState(scheduleCompositionProfile = rich))
        val stored = requireNotNull(dao.snapshot)
        val profileJson = Json.parseToJsonElement(stored.payload).jsonObject
            .getValue("scheduleCompositionProfile").jsonObject

        assertEquals("Europe/Paris", profileJson.getValue("timezoneName").toString().trim('"'))
        assertTrue(profileJson.containsKey("availability"))
        assertTrue(profileJson.containsKey("sleep"))
        assertTrue(profileJson.containsKey("protectedTime"))
        assertEquals(rich, requireNotNull(repository.load()).scheduleCompositionProfile)
    }

    @Test
    fun malformedRichProfileFailsBothDirectSaveAndCurrentPayloadLoad() = runBlocking {
        val rich = requireNotNull(
            ScheduleCompositionProfileSnapshot().upgradedToWeeklySchedule("UTC"),
        )
        val invalid = rich.copy(timezoneName = "Not/A_Timezone")
        val directDao = FakePlannerSnapshotDao()
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                RoomPlannerStateRepository(directDao).save(
                    DayWeaveUiState(scheduleCompositionProfile = invalid),
                )
            }
        }
        assertEquals(null, directDao.snapshot)

        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)
        repository.save(DayWeaveUiState(scheduleCompositionProfile = rich))
        val stored = requireNotNull(dao.snapshot)
        dao.snapshot = stored.copy(
            payload = stored.payload.replace("\"timezoneName\":\"UTC\"", "\"timezoneName\":\"Bad/Zone\""),
        )

        assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
        assertEquals(stored.payload.replace("\"timezoneName\":\"UTC\"", "\"timezoneName\":\"Bad/Zone\""), dao.snapshot?.payload)
    }

    private class FakePlannerSnapshotDao(
        var snapshot: PlannerSnapshotEntity? = null,
    ) : PlannerSnapshotDao {
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
        }
    }
}
