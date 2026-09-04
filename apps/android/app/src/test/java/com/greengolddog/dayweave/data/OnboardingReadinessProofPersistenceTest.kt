package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.OnboardingFirstItemAnchorSnapshot
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class OnboardingReadinessProofPersistenceTest {
    @Test
    fun v15RoundTripsOnlyTheContentFreePendingAnchorShape() = runBlocking {
        val dao = OnboardingProofFakeDao()
        val repository = RoomPlannerStateRepository(dao) { 42 }
        val state = pendingAnchorState()

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(state.onboardingFirstItemAnchor, restored.onboardingFirstItemAnchor)
        assertEquals(PlannerSnapshotFormats.JSON_V18, dao.snapshot?.payloadFormat)
        val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        val anchor = requireNotNull(root["onboardingFirstItemAnchor"]).jsonObject
        assertEquals(setOf("itemId", "canonicalRevision"), anchor.keys)
        assertEquals(ITEM_ID, anchor.getValue("itemId").let { (it as JsonPrimitive).content })
        assertTrue(anchor.getValue("canonicalRevision") is JsonNull)
        assertTrue(anchor.values.none { it.toString().contains("First private task") })
    }

    @Test
    fun predecessorLabelsCannotAcquireAnInjectedAnchorAndAreRewrittenAsV15() = runBlocking {
        listOf(
            PlannerSnapshotFormats.JSON_V1,
            PlannerSnapshotFormats.JSON_V2,
            PlannerSnapshotFormats.JSON_V3,
            PlannerSnapshotFormats.JSON_V4,
            PlannerSnapshotFormats.JSON_V5,
            PlannerSnapshotFormats.JSON_V6,
            PlannerSnapshotFormats.JSON_V7,
            PlannerSnapshotFormats.JSON_V8,
            PlannerSnapshotFormats.JSON_V9,
            PlannerSnapshotFormats.JSON_V10,
            PlannerSnapshotFormats.JSON_V11,
            PlannerSnapshotFormats.JSON_V12,
            PlannerSnapshotFormats.JSON_V13,
            PlannerSnapshotFormats.JSON_V14,
        ).forEach { predecessor ->
            val dao = OnboardingProofFakeDao()
            val repository = RoomPlannerStateRepository(dao) { 43 }
            repository.save(canonicalAnchorState())
            val current = requireNotNull(dao.snapshot)
            dao.snapshot = current.copy(payloadFormat = predecessor)

            val restored = requireNotNull(repository.load())

            assertNull(restored.onboardingFirstItemAnchor)
            assertEquals(PlannerSnapshotFormats.JSON_V18, dao.snapshot?.payloadFormat)
            val rewritten = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
            assertTrue(rewritten.getValue("onboardingFirstItemAnchor") is JsonNull)
        }
    }

    @Test
    fun v15RequiresTheExplicitAnchorFieldEvenWhenNull() = runBlocking {
        val dao = OnboardingProofFakeDao()
        val repository = RoomPlannerStateRepository(dao)
        repository.save(DayWeaveUiState())
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root - "onboardingFirstItemAnchor"),
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun malformedOrUnrelatedCurrentAnchorsFailClosed() = runBlocking {
        val repository = RoomPlannerStateRepository(OnboardingProofFakeDao())
        assertThrows(SerializationException::class.java) {
            runBlocking {
                repository.save(
                    DayWeaveUiState(
                        onboardingFirstItemAnchor =
                            OnboardingFirstItemAnchorSnapshot(ITEM_ID),
                    ),
                )
            }
        }
        assertThrows(SerializationException::class.java) {
            runBlocking {
                repository.save(
                    pendingAnchorState().copy(
                        onboardingFirstItemAnchor =
                            OnboardingFirstItemAnchorSnapshot("not-a-uuid"),
                    ),
                )
            }
        }
        Unit
    }

    @Test
    fun currentPayloadCannotChangeAnAnchorToAnotherRevision() = runBlocking {
        val dao = OnboardingProofFakeDao()
        val repository = RoomPlannerStateRepository(dao)
        repository.save(canonicalAnchorState())
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val anchor = root.getValue("onboardingFirstItemAnchor").jsonObject
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root + (
                        "onboardingFirstItemAnchor" to
                            JsonObject(anchor + ("canonicalRevision" to JsonPrimitive(2)))
                        ),
                ),
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun currentAnchorRejectsExpandedContentBearingShape() = runBlocking {
        val dao = OnboardingProofFakeDao()
        val repository = RoomPlannerStateRepository(dao)
        repository.save(pendingAnchorState())
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val anchor = root.getValue("onboardingFirstItemAnchor").jsonObject
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root + (
                        "onboardingFirstItemAnchor" to
                            JsonObject(anchor + ("title" to JsonPrimitive("Must not persist")))
                        ),
                ),
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    private fun pendingAnchorState(): DayWeaveUiState = DayWeaveUiState(
        onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(ITEM_ID),
        pendingCanonicalAuthoringMutations = listOf(
            PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = ITEM_ID,
                operation = CanonicalAuthoringOperation.CREATE,
                draft = plannedDraft(),
                createdAt = CREATED_AT,
            ),
        ),
    )

    private fun canonicalAnchorState(): DayWeaveUiState {
        val item = canonicalItem()
        return DayWeaveUiState(
            canonicalItems = listOf(item),
            onboardingFirstItemAnchor = OnboardingFirstItemAnchorSnapshot(
                itemId = item.id,
                canonicalRevision = item.revision,
            ),
        )
    }

    private fun plannedDraft(): CanonicalItemDraft = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.TASK,
        title = "First private task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
    )

    private fun canonicalItem(): CanonicalItemSnapshot = CanonicalItemSnapshot(
        id = ITEM_ID,
        kind = "task",
        status = "planned",
        title = "First private task",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = CREATED_AT,
        updatedAt = "2026-09-03T07:30:00Z",
    )

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val MUTATION_ID = "22222222-2222-4222-8222-222222222222"
        const val CREATED_AT = "2026-09-03T07:00:00Z"
    }
}

private class OnboardingProofFakeDao : PlannerSnapshotDao {
    var snapshot: PlannerSnapshotEntity? = null

    override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

    override suspend fun save(snapshot: PlannerSnapshotEntity) {
        this.snapshot = snapshot
    }
}
