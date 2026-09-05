package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.HabitLedgerSnapshot
import com.greengolddog.dayweave.model.HabitMissedReconcileCommandSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceEvidenceSnapshot
import com.greengolddog.dayweave.model.HabitOccurrenceSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeCommandSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeInputSnapshot
import com.greengolddog.dayweave.model.HabitOutcomeStatusSnapshot
import com.greengolddog.dayweave.model.PendingHabitMissedReconcile
import com.greengolddog.dayweave.model.PendingHabitMutation
import com.greengolddog.dayweave.model.PendingHabitMutationDisposition
import com.greengolddog.dayweave.model.PendingHabitMutationKind
import java.io.File
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class HabitPersistenceTest {
    @Test
    fun sharedOccurrenceEvidenceFixturesDefineTheAndroidContract() {
        val document = fixtureJson.parseToJsonElement(
            habitOccurrenceEvidenceFixture().readText(),
        ).jsonObject
        assertEquals(
            "dayweave.habit-occurrence-evidence-fixtures/1",
            document.getValue("schema").jsonPrimitive.content,
        )
        val base = document.getValue("base_evidence").jsonObject
        val validCases = document.getValue("valid_cases").jsonArray
        val invalidCases = document.getValue("invalid_cases").jsonArray
        assertTrue(validCases.isNotEmpty())
        assertTrue(invalidCases.isNotEmpty())
        val names = mutableSetOf<String>()

        validCases.forEach { element ->
            val case = element.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            assertTrue("duplicate fixture case $name", names.add(name))
            val merged = JsonObject(base + case.getValue("patch").jsonObject)
            val wire = fixtureJson.decodeFromJsonElement<SharedHabitEvidenceWire>(merged)
            wire.snapshot().requireValid()
            assertEquals(
                "$name did not retain its exact wire value",
                merged,
                fixtureJson.encodeToJsonElement(wire),
            )
        }

        invalidCases.forEach { element ->
            val case = element.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            assertTrue("duplicate fixture case $name", names.add(name))
            val merged = JsonObject(base + case.getValue("patch").jsonObject)
            val accepted = runCatching {
                fixtureJson.decodeFromJsonElement<SharedHabitEvidenceWire>(merged)
                    .snapshot()
                    .requireValid()
            }.isSuccess
            assertTrue("invalid fixture was accepted: $name", !accepted)
        }
    }

    @Test
    fun authoritativeCacheAndExactOutboxRoundTripInCurrentEncryptedPayload() = runBlocking {
        val ledger = ledger()
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }

        repository.save(DayWeaveUiState(habitLedger = ledger))
        val restored = requireNotNull(repository.load())

        assertEquals(ledger, restored.habitLedger)
        assertEquals(PlannerSnapshotFormats.JSON_V20, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("Good start"))
        assertTrue(restored.habitLedger.toString().contains("content=<redacted>"))
        assertTrue(
            restored.habitLedger.pendingMutations.single().toString().contains("request=<redacted>"),
        )
    }

    @Test
    fun exactMissedReconcileJournalRoundTripsAndCurrentPayloadRequiresRecoveryField() = runBlocking {
        val pending = pendingMissedReconcile(DIFFERENT_OPERATION_ID)
        val journaled = ledger().copy(
            deltaCaughtUp = false,
            pendingMissedReconcile = pending,
        ).also(HabitLedgerSnapshot::requireValid)
        assertThrows(IllegalArgumentException::class.java) {
            journaled.copy(deltaCaughtUp = true).requireValid()
        }
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { 1_100 }

        repository.save(DayWeaveUiState(habitLedger = journaled))
        assertEquals(journaled, requireNotNull(repository.load()).habitLedger)
        assertTrue(pending.toString().contains("request=<redacted>"))

        repository.save(DayWeaveUiState(habitLedger = ledger()))
        val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        val encodedLedger = root.getValue("habitLedger").jsonObject
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root + ("habitLedger" to JsonObject(encodedLedger - "pendingMissedReconcile")),
                ),
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
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
        assertEquals(PlannerSnapshotFormats.JSON_V20, dao.snapshot?.payloadFormat)
    }

    @Test
    fun v18LabelCannotInjectMissedProjectionOrDecisionReplayAuthority() = runBlocking {
        val expected = ledger()
        val dao = FakeDao()
        val repository = RoomPlannerStateRepository(dao) { 2_100 }
        repository.save(DayWeaveUiState(habitLedger = expected))
        val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        val encodedLedger = root.getValue("habitLedger").jsonObject
        val occurrences = encodedLedger.getValue("occurrences").jsonObject
        val occurrence = occurrences.getValue(OCCURRENCE_ID).jsonObject
        val forgedResolution = Json.parseToJsonElement(
            """{"occurrenceEvidenceId":"$OCCURRENCE_ID","habitId":"$HABIT_ID","sourcePlannerOccurrenceId":"$PLANNER_OCCURRENCE_ID","revision":99,"configuredPolicy":"ask","action":{"type":"skip"},"createdAt":"2026-09-01T09:01:00Z","updatedAt":"2026-09-01T09:01:00Z"}""",
        )
        val forgedPending = Json.parseToJsonElement(
            """{"schemaVersion":1,"kind":"missed_resolution","habitId":"$HABIT_ID","targetId":"$OCCURRENCE_ID","expectedRevision":99,"idempotencyKey":"$DIFFERENT_OPERATION_ID","requestJson":"{}","createdAt":"2026-09-01T09:01:00Z","syncOrigin":"$ORIGIN","configurationId":"$CONFIGURATION_ID","disposition":"pending"}""",
        )
        val forgedReconcile = fixtureJson.encodeToJsonElement(
            pendingMissedReconcile(DIFFERENT_OPERATION_ID),
        )
        val forgedOccurrences = JsonObject(
            occurrences + (OCCURRENCE_ID to JsonObject(occurrence +
                ("missedResolution" to forgedResolution))),
        )
        val pending = encodedLedger.getValue("pendingMutations").jsonArray
        val forgedLedger = JsonObject(
            encodedLedger + mapOf(
                "occurrences" to forgedOccurrences,
                "pendingMutations" to JsonArray(pending + forgedPending),
                "pendingMissedReconcile" to forgedReconcile,
            ),
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payloadFormat = PlannerSnapshotFormats.JSON_V18,
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root + ("habitLedger" to forgedLedger)),
            ),
        )

        val restored = requireNotNull(repository.load())

        assertEquals(expected.copy(deltaCaughtUp = false), restored.habitLedger)
        assertEquals(expected.deltaCursor, restored.habitLedger.deltaCursor)
        assertFalse(restored.habitLedger.deltaCaughtUp)
        assertEquals(PlannerSnapshotFormats.JSON_V20, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"missedResolution\":null"))
        assertTrue(!requireNotNull(dao.snapshot).payload.contains("missed_resolution"))
        assertNull(restored.habitLedger.pendingMissedReconcile)
        assertTrue(!requireNotNull(dao.snapshot).payload.contains(DIFFERENT_OPERATION_ID))
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

    @Test
    fun persistedHabitEvidenceDateHorizonIsInclusiveAndRejectsOutsideYears() {
        val evidence = ledger().occurrences.values.single().evidence

        listOf(1900, 2200).forEach { year -> evidence.withLocalYear(year).requireValid() }
        listOf(1899, 2201).forEach { year ->
            assertThrows(IllegalArgumentException::class.java) {
                evidence.withLocalYear(year).requireValid()
            }
        }
    }

    @Test
    fun persistedHabitEvidenceRejectsLedgerIdEqualToPlannerOccurrenceId() {
        val evidence = ledger().occurrences.values.single().evidence

        assertThrows(IllegalArgumentException::class.java) {
            evidence.copy(id = evidence.plannerOccurrenceId).requireValid()
        }
    }

    @Test
    fun persistedHabitEvidenceRequiresRfcUuidVariantBoundedInstantsAndIanaTimezone() {
        val evidence = ledger().occurrences.values.single().evidence

        assertThrows(IllegalArgumentException::class.java) {
            evidence.copy(plannerOccurrenceId = NON_RFC_VARIANT_PLANNER_OCCURRENCE_ID)
                .requireValid()
        }

        evidence.copy(
            windowStart = "0001-01-01T00:00:00Z",
            windowEnd = "9999-12-31T23:59:59.999999Z",
        ).requireValid()

        listOf(
            evidence.copy(windowStart = "0000-01-01T00:00:00Z"),
            evidence.copy(windowEnd = "+10000-01-01T00:00:00Z"),
            evidence.copy(timezoneName = "+02:00"),
            evidence.copy(timezoneName = "SystemV/EST5"),
        ).forEach { invalidEvidence ->
            assertThrows(IllegalArgumentException::class.java) {
                invalidEvidence.requireValid()
            }
        }
    }

    @Test
    fun persistedHabitIdentityRejectsNoncanonicalNumbersAndAnchors() {
        val evidence = ledger().occurrences.values.single().evidence
        val invalidIdentities = listOf(
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":-0}""",
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0.0}""",
            """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0e0}""",
            """{"type":"after_completion","anchor":"2026-09-01T07:00:00.123400Z"}""",
            """{"type":"after_completion","anchor":"2026-09-01T07:00:00+00:00"}""",
            """{"type":"after_completion","anchor":"2026-09-01T07:00:00-00:00"}""",
        ).map { raw -> Json.parseToJsonElement(raw).jsonObject }

        invalidIdentities.forEach { identity ->
            assertThrows(IllegalArgumentException::class.java) {
                evidence.copy(identity = identity).requireValid()
            }
        }
    }

    private fun HabitOccurrenceEvidenceSnapshot.withLocalYear(
        year: Int,
    ): HabitOccurrenceEvidenceSnapshot = copy(
        identity = JsonObject(identity + ("date" to JsonPrimitive("$year-09-01"))),
        nominalStart = nominalStart.replace("2026", year.toString()),
        nominalEnd = nominalEnd.replace("2026", year.toString()),
        windowStart = windowStart.replace("2026", year.toString()),
        windowEnd = windowEnd.replace("2026", year.toString()),
        localDate = "$year-09-01",
    )

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

    private fun pendingMissedReconcile(operationId: String) = PendingHabitMissedReconcile(
        idempotencyKey = operationId,
        requestJson = HabitMissedReconcileCommandSnapshot(operationId).encoded(),
        limit = 25,
        createdAt = "2026-09-01T07:31:00Z",
    ).also(PendingHabitMissedReconcile::requireValid)

    private fun habitOccurrenceEvidenceFixture(): File {
        val relative = "fixtures/habit-protocol/occurrence-evidence-v1.json"
        return generateSequence(File(requireNotNull(System.getProperty("user.dir")))) {
            it.parentFile
        }
            .map { File(it, relative) }
            .firstOrNull(File::isFile)
            ?: error("Unable to locate $relative")
    }

    @Serializable
    private data class SharedHabitEvidenceWire(
        val id: String,
        @SerialName("habit_id") val habitId: String,
        @SerialName("planner_occurrence_id") val plannerOccurrenceId: String,
        @SerialName("source_schedule_revision_id") val sourceScheduleRevisionId: String,
        @SerialName("source_item_revision") val sourceItemRevision: Long,
        @SerialName("policy_fingerprint") val policyFingerprint: String,
        val identity: JsonObject,
        @SerialName("nominal_start") val nominalStart: String,
        @SerialName("nominal_end") val nominalEnd: String,
        @SerialName("window_start") val windowStart: String,
        @SerialName("window_end") val windowEnd: String,
        @SerialName("local_date") val localDate: String,
        @SerialName("timezone_name") val timezoneName: String,
        @SerialName("expected_duration_seconds") val expectedDurationSeconds: Long?,
        @SerialName("expected_quantity") val expectedQuantity: Long?,
        @SerialName("expected_unit") val expectedUnit: String?,
    ) {
        fun snapshot() = HabitOccurrenceEvidenceSnapshot(
            id = id,
            habitId = habitId,
            plannerOccurrenceId = plannerOccurrenceId,
            sourceScheduleRevisionId = sourceScheduleRevisionId,
            sourceItemRevision = sourceItemRevision,
            policyFingerprint = policyFingerprint,
            identity = identity,
            nominalStart = nominalStart,
            nominalEnd = nominalEnd,
            windowStart = windowStart,
            windowEnd = windowEnd,
            localDate = localDate,
            timezoneName = timezoneName,
            expectedDurationSeconds = expectedDurationSeconds,
            expectedQuantity = expectedQuantity,
            expectedUnit = expectedUnit,
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
        val fixtureJson = Json {
            ignoreUnknownKeys = false
            encodeDefaults = true
        }
        const val ORIGIN = "https://api.example.test/tenant/"
        const val CONFIGURATION_ID = "habit-binding"
        const val HABIT_ID = "11111111-1111-4111-8111-111111111111"
        const val OCCURRENCE_ID = "22222222-2222-4222-8222-222222222222"
        const val PLANNER_OCCURRENCE_ID = "33333333-3333-5333-8333-333333333333"
        const val NON_RFC_VARIANT_PLANNER_OCCURRENCE_ID =
            "33333333-3333-5333-0333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val OPERATION_ID = "55555555-5555-4555-8555-555555555555"
        const val DIFFERENT_OPERATION_ID = "66666666-6666-4666-8666-666666666666"
    }
}
