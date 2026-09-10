package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalEventTimingDraft
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.network.CreateCanonicalItemRequest
import com.greengolddog.dayweave.network.ReplaceCanonicalItemRequest
import com.greengolddog.dayweave.sync.toCanonicalItemReplacement
import com.greengolddog.dayweave.sync.toCreateCanonicalItemRequest
import java.time.Instant
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalStructuralAuthoringPersistenceTest {
    @Test
    fun oldSubmittedCreateAndReplaceRetainExactJournalIdentityAndIndependentDurationShape() = runBlocking {
        for (format in AUTHORING_PREDECESSORS) {
            for (durationShape in 1..2) {
                for (operation in listOf(CanonicalAuthoringOperation.CREATE, CanonicalAuthoringOperation.REPLACE)) {
                    val original = mutation(operation).copy(
                        durationRequestShapeVersion = durationShape,
                        structuralRequestShapeVersion = 1,
                        syncOrigin = ORIGIN,
                        configurationId = CONFIGURATION,
                        submittedAt = "2026-09-06T09:01:00Z",
                    )
                    val dao = FakeDao()
                    val repository = repository(dao)
                    repository.save(state(original))
                    val legacy = legacySnapshot(requireNotNull(dao.snapshot), format)
                    dao.snapshot = legacy

                    val restored = requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single()
                    val expectedDuration = if (format == PlannerSnapshotFormats.JSON_V7 ||
                        format == PlannerSnapshotFormats.JSON_V17) 1 else durationShape
                    val expected = original.copy(durationRequestShapeVersion = expectedDuration)
                    assertEquals(expected, restored)
                    assertEquals(
                        "Migration must replay the exact original HTTP bytes and field order",
                        wireRequest(expected), wireRequest(restored),
                    )
                    val request = Json.parseToJsonElement(wireRequest(restored)).jsonObject
                    val fields = (request["item"] as? JsonObject) ?: request
                    assertTrue(fields.keys.intersect(STRUCTURAL_WIRE_FIELDS).isEmpty())
                    assertEquals(expectedDuration == 2, fields.containsKey("duration_kind"))
                    assertEquals(1, restored.structuralRequestShapeVersion)
                    assertEquals(CanonicalDeadlineKind.DATE_TIME, restored.draft?.deadlineKind)
                    assertEquals(CanonicalDeadlineStrength.HARD, restored.draft?.deadlineStrength)
                    assertTrue(requireNotNull(restored.draft).hasOwnEffort)
                    assertEquals(PlannerSnapshotFormats.JSON_V25, dao.snapshot?.payloadFormat)
                    val roundTrip = requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single()
                    assertEquals("Migration cannot repeatedly upgrade request authority", restored, roundTrip)
                }
            }
        }
    }

    @Test
    fun oldUnsubmittedBoundAndConflictedJournalsAreNeverSilentlyUpgraded() = runBlocking {
        val variants = listOf(
            mutation().copy(structuralRequestShapeVersion = 1),
            mutation().copy(structuralRequestShapeVersion = 1, syncOrigin = ORIGIN, configurationId = CONFIGURATION),
            mutation().copy(
                structuralRequestShapeVersion = 1,
                disposition = CanonicalAuthoringDisposition.CONFLICTED,
                diagnostic = "Synthetic review required",
            ),
        )
        for (original in variants) {
            val dao = FakeDao()
            val repository = repository(dao)
            repository.save(state(original))
            dao.snapshot = legacySnapshot(requireNotNull(dao.snapshot), PlannerSnapshotFormats.JSON_V20)
            val restored = requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single()
            assertEquals(original, restored)
            assertFalse(restored.isSubmitted)
            assertEquals(1, restored.structuralRequestShapeVersion)
        }
    }

    @Test
    fun currentProjectRangeAndDateOnlySoftDeadlineRoundTripWithExplicitNulls() = runBlocking {
        val original = mutation().copy(draft = CanonicalItemDraft(
            kind = ItemKind.PROJECT,
            placement = CanonicalDraftPlacement.PLANNED,
            title = "Synthetic typed project",
            timezoneName = "America/New_York",
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 1_800,
            durationSeconds = 3_600,
            durationMaxSeconds = 5_400,
            durationSource = CanonicalDurationSource.ASSISTANT,
            deadlineKind = CanonicalDeadlineKind.DATE,
            deadlineDate = "2026-11-01",
            deadlineStrength = CanonicalDeadlineStrength.SOFT,
            deadlineSoftWeight = 0,
            hasOwnEffort = true,
        ))
        val dao = FakeDao()
        val repository = repository(dao)
        repository.save(state(original))
        val stored = requireNotNull(dao.snapshot)
        assertEquals(PlannerSnapshotFormats.JSON_V25, stored.payloadFormat)
        val entry = entry(stored)
        assertEquals(JsonPrimitive(2), entry["structuralRequestShapeVersion"])
        val draft = entry.getValue("draft").jsonObject
        assertTrue(draft.keys.containsAll(STRUCTURAL_FIELDS))
        assertEquals(JsonNull, draft["deadlineAt"])
        assertEquals(JsonPrimitive("2026-11-01"), draft["deadlineDate"])
        assertEquals(original, requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single())
    }

    @Test
    fun durationAndStructuralRequestShapesRemainIndependentAcrossCurrentSaveLoad() = runBlocking {
        for (durationShape in 1..2) {
            for (structuralShape in 1..2) {
                val original = mutation().copy(
                    durationRequestShapeVersion = durationShape,
                    structuralRequestShapeVersion = structuralShape,
                )
                val dao = FakeDao()
                val repository = repository(dao)
                repository.save(state(original))
                assertEquals(original, requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single())
            }
        }
    }

    @Test
    fun legacyEventEndAndUnknownTaskNeverAcquireDeadlineOrOwnEffortAuthority() = runBlocking {
        val drafts = listOf(
            CanonicalItemDraft(title = "Synthetic unknown task", timezoneName = "UTC"),
            CanonicalItemDraft(
                kind = ItemKind.EVENT,
                placement = CanonicalDraftPlacement.PLANNED,
                title = "Synthetic fixed event",
                timezoneName = "UTC",
                durationSeconds = 1_800,
                earliestStartAt = "2026-10-01T12:00:00Z",
                deadlineAt = "2026-10-01T12:30:00Z",
                eventTiming = CanonicalEventTimingDraft(
                    startsAt = "2026-10-01T12:00:00Z",
                    endsAt = "2026-10-01T12:30:00Z",
                ),
            ),
        )
        for (draft in drafts) {
            val original = mutation().copy(draft = draft, structuralRequestShapeVersion = 1)
            val dao = FakeDao()
            val repository = repository(dao)
            repository.save(state(original))
            dao.snapshot = legacySnapshot(requireNotNull(dao.snapshot), PlannerSnapshotFormats.JSON_V20)
            val restored = requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single()
            assertEquals(original, restored)
            assertEquals(CanonicalDeadlineKind.NONE, restored.draft?.deadlineKind)
            assertEquals(null, restored.draft?.deadlineStrength)
            assertFalse(requireNotNull(restored.draft).hasOwnEffort)
            assertEquals(wireRequest(original), wireRequest(restored))
        }
    }

    @Test
    fun currentPayloadRequiresEveryTypedDraftFieldAndExplicitJournalMarker() = runBlocking {
        val dao = FakeDao()
        val repository = repository(dao)
        repository.save(state(mutation()))
        val current = requireNotNull(dao.snapshot)
        val originalEntry = entry(current)
        val draft = originalEntry.getValue("draft").jsonObject
        val malformed = STRUCTURAL_FIELDS.map { field ->
            JsonObject(originalEntry + ("draft" to JsonObject(draft - field)))
        } + listOf(
            JsonObject(originalEntry - "structuralRequestShapeVersion"),
            JsonObject(originalEntry + ("structuralRequestShapeVersion" to JsonNull)),
            JsonObject(originalEntry + ("structuralRequestShapeVersion" to JsonPrimitive(3))),
        )
        for (invalid in malformed) {
            val stored = replaceEntry(current, invalid)
            dao.snapshot = stored
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals("Rejected current authority must not be rewritten", stored, dao.snapshot)
        }
    }

    @Test
    fun everyPredecessorLabelRejectsInjectedTypedDraftKeysAndAnyStructuralMarker() = runBlocking {
        val dao = FakeDao()
        val repository = repository(dao)
        repository.save(state(mutation().copy(structuralRequestShapeVersion = 1)))
        val current = requireNotNull(dao.snapshot)
        for (format in ALL_PREDECESSORS) {
            val legacy = legacySnapshot(current, format)
            val originalEntry = entry(legacy)
            val draft = originalEntry.getValue("draft").jsonObject
            val injected = STRUCTURAL_FIELDS.map { field ->
                JsonObject(originalEntry + ("draft" to JsonObject(draft + (field to JsonNull))))
            } + listOf(JsonNull, JsonPrimitive(1), JsonPrimitive(2)).map { marker ->
                JsonObject(originalEntry + ("structuralRequestShapeVersion" to marker))
            }
            for (invalid in injected) {
                val stored = replaceEntry(legacy, invalid)
                dao.snapshot = stored
                assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
                assertEquals("An injected legacy field cannot be silently stripped", stored, dao.snapshot)
            }
        }
    }

    @Test
    fun bodylessTrashAndRestoreAlsoRetainAnExplicitLegacyStructuralMarker() = runBlocking {
        for (operation in listOf(CanonicalAuthoringOperation.TRASH, CanonicalAuthoringOperation.RESTORE)) {
            val original = mutation(operation).copy(structuralRequestShapeVersion = 1)
            val dao = FakeDao()
            val repository = repository(dao)
            repository.save(state(original))
            dao.snapshot = legacySnapshot(requireNotNull(dao.snapshot), PlannerSnapshotFormats.JSON_V20)
            val restored = requireNotNull(repository.load()).pendingCanonicalAuthoringMutations.single()
            assertEquals(original, restored)
            assertEquals(2, restored.durationRequestShapeVersion)
            assertEquals(1, restored.structuralRequestShapeVersion)
            val current = requireNotNull(dao.snapshot)
            dao.snapshot = replaceEntry(current, JsonObject(entry(current) - "structuralRequestShapeVersion"))
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
        }
    }

    private fun repository(dao: FakeDao) = RoomPlannerStateRepository(dao) {
        Instant.parse("2026-09-06T10:00:00Z").toEpochMilli()
    }

    private fun wireRequest(mutation: PendingCanonicalAuthoringMutation): String {
        val draft = requireNotNull(mutation.draft)
        return if (mutation.operation == CanonicalAuthoringOperation.CREATE) {
            WIRE_JSON.encodeToString(CreateCanonicalItemRequest.serializer(), draft.toCreateCanonicalItemRequest(
                mutation.itemId, mutation.durationRequestShapeVersion, mutation.structuralRequestShapeVersion,
            ))
        } else {
            WIRE_JSON.encodeToString(ReplaceCanonicalItemRequest.serializer(), ReplaceCanonicalItemRequest(
                expectedRevision = requireNotNull(mutation.expectedRevision),
                item = draft.toCanonicalItemReplacement(
                    mutation.itemId, mutation.durationRequestShapeVersion, mutation.structuralRequestShapeVersion,
                ),
            ))
        }
    }

    private fun mutation(operation: CanonicalAuthoringOperation = CanonicalAuthoringOperation.CREATE): PendingCanonicalAuthoringMutation {
        val hasDraft = operation == CanonicalAuthoringOperation.CREATE || operation == CanonicalAuthoringOperation.REPLACE
        val base = if (operation == CanonicalAuthoringOperation.REPLACE) item() else null
        return PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = operation,
            draft = if (hasDraft) CanonicalItemDraft(
                title = "Synthetic exact legacy request",
                timezoneName = "UTC",
                durationSeconds = 1_800,
                deadlineAt = "2026-10-01T12:00:00.123456Z",
                constraints = CanonicalFlexibleConstraintsDraft(hasOwnEffort = true),
            ) else null,
            expectedRevision = if (operation == CanonicalAuthoringOperation.CREATE) null else 4,
            baseItem = base,
            createdAt = "2026-09-06T09:00:00Z",
        )
    }

    private fun state(mutation: PendingCanonicalAuthoringMutation) = DayWeaveUiState(
        canonicalItems = if (mutation.operation == CanonicalAuthoringOperation.REPLACE) listOf(item()) else emptyList(),
        canonicalSyncOrigin = mutation.syncOrigin,
        canonicalConfigurationId = mutation.configurationId,
        pendingCanonicalAuthoringMutations = listOf(mutation),
        canonicalRecentlyDeleted = if (mutation.operation == CanonicalAuthoringOperation.RESTORE) listOf(
            CanonicalRecentlyDeletedRecord(
                id = ITEM_ID,
                revision = 4,
                deletedAt = "2026-09-06T08:00:00Z",
                parentId = null,
                lastKnownItem = null,
                retentionAnchorAt = "2026-09-06T08:00:00Z",
            ),
        ) else emptyList(),
    )

    private fun item() = CanonicalItemSnapshot(
        id = ITEM_ID, kind = "task", status = "inbox", title = "Synthetic base item",
        timezoneName = "UTC", durationSeconds = 1_800,
        flexibleConstraintsJson = "{}", splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50, urgency = 50, siblingOrder = 0, isExecutable = true, revision = 4,
        createdAt = "2026-09-06T08:00:00Z", updatedAt = "2026-09-06T08:00:00Z",
    )

    private fun entry(snapshot: PlannerSnapshotEntity): JsonObject = (
        Json.parseToJsonElement(snapshot.payload).jsonObject.getValue("pendingCanonicalAuthoringMutations") as JsonArray
        ).single().jsonObject

    private fun replaceEntry(snapshot: PlannerSnapshotEntity, entry: JsonObject): PlannerSnapshotEntity {
        val root = Json.parseToJsonElement(snapshot.payload).jsonObject
        return snapshot.copy(payload = Json.encodeToString(
            JsonObject.serializer(),
            JsonObject(root + ("pendingCanonicalAuthoringMutations" to JsonArray(listOf(entry)))),
        ))
    }

    private fun legacySnapshot(snapshot: PlannerSnapshotEntity, format: String): PlannerSnapshotEntity {
        val original = entry(snapshot)
        val draft = original["draft"] as? JsonObject
        val legacy = JsonObject((original - "structuralRequestShapeVersion") +
            listOfNotNull(draft?.let { "draft" to JsonObject(it - STRUCTURAL_FIELDS) }))
        return replaceEntry(snapshot, legacy).copy(payloadFormat = format)
    }

    private class FakeDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? =
            snapshot?.asPreProgressFixtureWhenRelabelled()
        override suspend fun save(snapshot: PlannerSnapshotEntity) { this.snapshot = snapshot }
    }

    private companion object {
        const val ITEM_ID = "00000000-0000-4000-8000-000000730001"
        const val MUTATION_ID = "00000000-0000-4000-8000-000000730002"
        const val ORIGIN = "https://api.example.test/"
        const val CONFIGURATION = "synthetic-structural-binding"
        val WIRE_JSON = Json { encodeDefaults = true; explicitNulls = false }
        val STRUCTURAL_WIRE_FIELDS = setOf("deadline_kind", "deadline_date", "deadline_strength", "deadline_soft_weight", "has_own_effort")
        val STRUCTURAL_FIELDS = setOf("deadlineKind", "deadlineDate", "deadlineStrength", "deadlineSoftWeight", "hasOwnEffort")
        val AUTHORING_PREDECESSORS = listOf(
            PlannerSnapshotFormats.JSON_V7, PlannerSnapshotFormats.JSON_V17,
            PlannerSnapshotFormats.JSON_V18, PlannerSnapshotFormats.JSON_V19, PlannerSnapshotFormats.JSON_V20,
        )
        val ALL_PREDECESSORS = listOf(
            PlannerSnapshotFormats.JSON_V1, PlannerSnapshotFormats.JSON_V2,
            PlannerSnapshotFormats.JSON_V3, PlannerSnapshotFormats.JSON_V4,
            PlannerSnapshotFormats.JSON_V5, PlannerSnapshotFormats.JSON_V6,
            PlannerSnapshotFormats.JSON_V7, PlannerSnapshotFormats.JSON_V8,
            PlannerSnapshotFormats.JSON_V9, PlannerSnapshotFormats.JSON_V10,
            PlannerSnapshotFormats.JSON_V11, PlannerSnapshotFormats.JSON_V12,
            PlannerSnapshotFormats.JSON_V13, PlannerSnapshotFormats.JSON_V14,
            PlannerSnapshotFormats.JSON_V15, PlannerSnapshotFormats.JSON_V16,
            PlannerSnapshotFormats.JSON_V17, PlannerSnapshotFormats.JSON_V18,
            PlannerSnapshotFormats.JSON_V19, PlannerSnapshotFormats.JSON_V20,
        )
    }
}
