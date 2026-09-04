package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceEvidenceSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeCommandSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class HabitPersistenceTest {
    @Test
    fun authoritativeCacheAndExactOutboxRoundTripInCurrentEncryptedPayload() = runBlocking {
        val ledger = ledger()
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }

        repository.save(DayWeaveUiState(habitLedger = ledger))
        val restored = requireNotNull(repository.load())

        assertEquals(ledger, restored.habitLedger)
        assertEquals(PlannerSnapshotFormats.JSON_V17, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("Good start"))
        assertTrue(restored.habitLedger.toString().contains("content=<redacted>"))
        assertTrue(
            restored.habitLedger.pendingMutations.single().toString().contains("request=<redacted>"),
        )
    }

    @Test
    fun v16LabelCannotAcquireAnInjectedHabitOutbox() = runBlocking {
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { 2_000 }
        repository.save(DayWeaveUiState(habitLedger = ledger()))
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payloadFormat = PlannerSnapshotFormats.JSON_V16,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(HabitLedgerSnapshot(), restored.habitLedger)
        assertEquals(PlannerSnapshotFormats.JSON_V17, dao.snapshot?.payloadFormat)
    }

    @Test
    fun currentLabelRequiresLedgerFieldAndRejectsMismatchedReplayAuthority() = runBlocking {
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao)
        repository.save(DayWeaveUiState())
        val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root - "habitLedger"),
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }

        repository.save(DayWeaveUiState(habitLedger = ledger()))
        val withLedger = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        val habitLedger = requireNotNull(withLedger["habitLedger"]).jsonObject
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    withLedger + ("habitLedger" to JsonObject(habitLedger - "pendingMutations")),
                ),
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }

        repository.save(DayWeaveUiState(habitLedger = ledger()))
        val withoutCatchUpMarker = Json.parseToJsonElement(
            requireNotNull(dao.snapshot).payload,
        ).jsonObject
        val ledgerWithoutCatchUpMarker = requireNotNull(
            withoutCatchUpMarker["habitLedger"],
        ).jsonObject
        val missingCatchUpMarkerLedger = JsonObject(
            ledgerWithoutCatchUpMarker - "deltaCaughtUp",
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    withoutCatchUpMarker + ("habitLedger" to missingCatchUpMarkerLedger),
                ),
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }

        repository.save(DayWeaveUiState())
        val withInvalidation = Json.parseToJsonElement(
            requireNotNull(dao.snapshot).payload,
        ).jsonObject
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(withInvalidation - "pendingSchedulePublicationInvalidated"),
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }

        val malformedPending = ledger().pendingMutations.single().copy(
            idempotencyKey = DIFFERENT_OPERATION_ID,
        )
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                repository.save(
                    DayWeaveUiState(
                        habitLedger = ledger().copy(pendingMutations = listOf(malformedPending)),
                    ),
                )
            }
        }
        Unit
    }

    @Test
    fun pendingAuthorityRequiresItsAuthoritativeTargetButReviewedFailuresRemainRestorable() =
        runBlocking {
            val dao = FakeDao()
            val repository = RoomPlannerStateRepository(dao)
            repository.save(DayWeaveUiState(habitLedger = ledger()))
            val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
            val encodedLedger = requireNotNull(root["habitLedger"]).jsonObject
            val missingOccurrenceLedger = JsonObject(
                encodedLedger + ("occurrences" to JsonObject(emptyMap())),
            )
            dao.snapshot = requireNotNull(dao.snapshot).copy(
                payload = Json.encodeToString(
                    JsonObject.serializer(),
                    JsonObject(root + ("habitLedger" to missingOccurrenceLedger)),
                ),
            )

            assertThrows(SerializationException::class.java) {
                runBlocking { repository.load() }
            }

            listOf(
                PendingHabitMutationDisposition.CONFLICT,
                PendingHabitMutationDisposition.NOT_FOUND,
                PendingHabitMutationDisposition.REJECTED,
            ).forEach { disposition ->
                val reviewed = ledger().copy(
                    occurrences = emptyMap(),
                    pendingMutations = listOf(
                        ledger().pendingMutations.single().copy(disposition = disposition),
                    ),
                ).also(HabitLedgerSnapshot::requireValid)
                repository.save(DayWeaveUiState(habitLedger = reviewed))

                assertEquals(reviewed, requireNotNull(repository.load()).habitLedger)
            }
        }

    private fun ledger(): HabitLedgerSnapshot {
        val occurrence = HabitOccurrenceSnapshot(
            evidence = HabitOccurrenceEvidenceSnapshot(
                id = OCCURRENCE_ID,
                habitId = HABIT_ID,
                plannerOccurrenceId = PLANNER_OCCURRENCE_ID,
                sourceScheduleRevisionId = SCHEDULE_REVISION_ID,
                sourceItemRevision = 7,
                policyFingerprint = "sha256:${"a".repeat(64)}",
                identity = JsonObject(
                    mapOf(
                        "type" to JsonPrimitive("calendar_day"),
                        "date" to JsonPrimitive("2026-09-01"),
                        "bucket_ordinal" to JsonPrimitive(0),
                    ),
                ),
                nominalStart = "2026-09-01T07:00:00Z",
                nominalEnd = "2026-09-01T07:30:00Z",
                windowStart = "2026-09-01T06:00:00Z",
                windowEnd = "2026-09-01T09:00:00Z",
                localDate = "2026-09-01",
                timezoneName = "Europe/Paris",
                expectedDurationSeconds = 1_800,
                expectedQuantity = 20,
                expectedUnit = "pages",
            ),
            outcome = null,
        )
        val command = HabitOutcomeCommandSnapshot(
            operationId = OPERATION_ID,
            expectedRevision = 0,
            outcome = HabitOutcomeInputSnapshot(
                status = HabitOutcomeStatusSnapshot.PARTIAL,
                progressBasisPoints = 3_500,
                quantity = 7,
                unit = "pages",
                actualSeconds = 600,
                note = "Good start",
                occurredAt = "2026-09-01T07:30:00Z",
            ),
        )
        val pending = PendingHabitMutation(
            schemaVersion = PendingHabitMutation.CURRENT_SCHEMA_VERSION,
            kind = PendingHabitMutationKind.OUTCOME,
            habitId = HABIT_ID,
            targetId = OCCURRENCE_ID,
            expectedRevision = 0,
            idempotencyKey = OPERATION_ID,
            requestJson = command.encoded(),
            createdAt = "2026-09-01T07:31:00Z",
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
        )
        return HabitLedgerSnapshot(
            syncOrigin = ORIGIN,
            configurationId = CONFIGURATION_ID,
            deltaCursor = "42",
            deltaCaughtUp = true,
            occurrences = mapOf(OCCURRENCE_ID to occurrence),
            pendingMutations = listOf(pending),
        ).also(HabitLedgerSnapshot::requireValid)
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
        const val ORIGIN = "https://api.example.test/tenant/"
        const val CONFIGURATION_ID = "habit-binding"
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val OCCURRENCE_ID = "22222222-2222-4222-8222-222222222222"
        const val PLANNER_OCCURRENCE_ID = "33333333-3333-5333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val OPERATION_ID = "55555555-5555-4555-8555-555555555555"
        const val DIFFERENT_OPERATION_ID = "66666666-6666-4666-8666-666666666666"
    }
}
