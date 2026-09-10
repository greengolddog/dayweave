package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.assistant.AssistantContextProjector
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.sync.NativeConvergenceDisk
import com.greengolddog.dayweave.sync.NativeConvergenceSnapshotDao
import java.io.IOException
import java.nio.file.Files
import java.time.Instant
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class RoutinePlanningDisplayPersistenceTest {
    @get:Rule val temporary = TemporaryFolder()
    private val now = Instant.parse(PLANNING_DISPLAY_NOW).toEpochMilli() + 1

    @Test fun encryptedDiskRestartPreservesExactOutputAndInputButCannotRestoreDisplayPermission(): Unit = runBlocking {
        val directory = temporary.root.toPath().resolve("fixed-preview")
        fun repository(prepare: Boolean) = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
            NativeConvergenceDisk(directory, "synthetic-fixed-preview", prepare))) { now }
        val base = planningDisplayState(); val capsule = requireNotNull(base.routinePlanningInputCapsule)
        val display = planningDisplaySnapshot(capsule)
        val state = base.copy(routinePlanningDisplaySnapshot = display,
            routinePlanningDisplayAdmission = RoutinePlanningDisplayAdmission(capsule, display, display.validateAndDecode(capsule), 0, 0))
        repository(true).save(state)
        val encrypted = String(Files.readAllBytes(directory.resolve("snapshot.aesgcm")), Charsets.UTF_8)
        assertFalse(encrypted.contains(display.helperResponseJson)); assertFalse(encrypted.contains("Synthetic private note"))
        val restored = requireNotNull(repository(false).load())
        assertEquals(capsule, restored.routinePlanningInputCapsule); assertEquals(display, restored.routinePlanningDisplaySnapshot)
        assertNull(restored.routinePlanningDisplayAdmission)
        assertNull(PlannerStore(restored, nowEpochMillis = { now }).state.value.routinePlanningDisplayAdmission)
        assertEquals(base.schedule, restored.schedule); assertEquals(base.routineOccurrenceLedger, restored.routineOccurrenceLedger)
        assertEquals(display.helperResponseJson, restored.routinePlanningDisplaySnapshot?.helperResponseJson)
    }

    @Test fun v25PreservesCapsuleAndEveryPriorFieldWhileOlderLabelsRejectEvenNullPreview(): Unit = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        val base = planningDisplayState()
        val pending = routineStateTestIntent(true).copy(syncOrigin = requireNotNull(base.canonicalSyncOrigin), configurationId = requireNotNull(base.canonicalConfigurationId))
        val prior = base.copy(routineOccurrenceLedger = base.routineOccurrenceLedger.copy(
            observations = routineStateTestLedger().observations, pending = listOf(pending)))
        repository.save(prior); val saved = requireNotNull(dao.snapshot)
        val oldRoot = Json.parseToJsonElement(saved.payload).jsonObject - "routinePlanningDisplaySnapshot"
        dao.snapshot = saved.copy(payloadFormat = PlannerSnapshotFormats.JSON_V25, payload = JsonObject(oldRoot).toString())
        val restored = requireNotNull(repository.load())
        assertEquals(base.routinePlanningInputCapsule, restored.routinePlanningInputCapsule)
        assertEquals(listOf(pending), restored.routineOccurrenceLedger.pending)
        assertEquals(oldRoot, Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject - "routinePlanningDisplaySnapshot")
        assertEquals(PlannerSnapshotFormats.JSON_V26, dao.snapshot?.payloadFormat)
        for (version in 1..25) {
            val format = PlannerSnapshotFormats::class.java.getDeclaredField("JSON_V$version").get(null) as String
            for (field in listOf("routinePlanningDisplaySnapshot", "routinePlanningDisplayAdmission")) {
                val injected = saved.copy(payloadFormat = format, payload = JsonObject(oldRoot + (field to JsonNull)).toString())
                dao.snapshot = injected
                assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
                assertEquals(injected, dao.snapshot)
            }
        }
    }

    @Test fun closedPreviewShapeAndMismatchedCapsuleFailWithoutRewriting(): Unit = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        val base = planningDisplayState(); val display = planningDisplaySnapshot(requireNotNull(base.routinePlanningInputCapsule))
        repository.save(base.copy(routinePlanningDisplaySnapshot = display))
        val saved = requireNotNull(dao.snapshot); val root = Json.parseToJsonElement(saved.payload).jsonObject
        val raw = root.getValue("routinePlanningDisplaySnapshot").jsonObject
        val variants = listOf(JsonObject(root - "routinePlanningDisplaySnapshot"),
            JsonObject(root + ("routinePlanningDisplayAdmission" to JsonNull)),
            JsonObject(root + ("routinePlanningInputCapsule" to JsonNull)),
            JsonObject(root + ("routinePlanningDisplaySnapshot" to JsonObject(raw + ("executable" to JsonPrimitive(true))))),
            JsonObject(root + ("routinePlanningDisplaySnapshot" to JsonObject(raw + ("occurrenceSnapshotRevision" to JsonPrimitive(99))))))
        for (variant in variants) {
            val injected = saved.copy(payload = variant.toString()); dao.snapshot = injected
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(injected, dao.snapshot)
        }
    }

    @Test fun finalOwnershipCheckRejectsAndDurabilityAloneDoesNotMakeRowsVisible(): Unit = runBlocking {
        val store = PlannerStore(planningDisplayState(), nowEpochMillis = { now })
        val fence = requireNotNull(store.captureRoutinePlanningInputFence())
        val display = planningDisplaySnapshot(requireNotNull(fence.state.routinePlanningInputCapsule))
        var calls = 0
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) {
            store.installRoutinePlanningDisplay(fence, display) { ++calls == 1 }
        }
        assertEquals(2, calls); assertSame(fence.state, store.state.value)
        val transition = requireNotNull(store.installRoutinePlanningDisplay(fence, display) { true })
        assertTrue(transition.persistence.awaitDurable()); assertNull(store.state.value.routinePlanningDisplayAdmission)
        assertFalse(store.admitRoutinePlanningDisplay(transition.postSaveFence, display) { false })
        assertNull(store.state.value.routinePlanningDisplayAdmission)
        assertTrue(store.admitRoutinePlanningDisplay(transition.postSaveFence, display) { true })
        val after = store.state.value
        assertEquals(AssistantContextProjector.project(fence.state, Instant.parse(PLANNING_DISPLAY_NOW)),
            AssistantContextProjector.project(after, Instant.parse(PLANNING_DISPLAY_NOW)))
        assertEquals(fence.state.schedule, after.schedule); assertNull(after.localScheduleCompositionProvenance)
        store.navigate(AppDestination.CALENDAR)
        assertNull(store.state.value.routinePlanningDisplayAdmission)
        assertEquals(display, store.state.value.routinePlanningDisplaySnapshot)
    }

    @Test fun failedReplacementWriteRestoresPriorArtifactCapsuleAndAllLedgers(): Unit = runBlocking {
        val base = planningDisplayState(); val capsule = requireNotNull(base.routinePlanningInputCapsule)
        val prior = planningDisplaySnapshot(capsule); val initial = base.copy(routinePlanningDisplaySnapshot = prior)
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) { throw IOException("Synthetic preview disk failure") }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope, nowEpochMillis = { now + 2000 })
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val fence = requireNotNull(store.captureRoutinePlanningInputFence())
            val replacement = prior.copy(computedAt = Instant.parse(PLANNING_DISPLAY_NOW).plusSeconds(1).toString())
            val transition = requireNotNull(store.installRoutinePlanningDisplay(fence, replacement) { true })
            assertFalse(withTimeout(3_000) { transition.persistence.awaitDurable() })
            assertEquals(prior, store.state.value.routinePlanningDisplaySnapshot)
            assertEquals(capsule, store.state.value.routinePlanningInputCapsule)
            assertEquals(initial.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
            assertNull(store.state.value.routinePlanningDisplayAdmission)
        } finally { scope.cancel() }
    }

    @Test fun coordinatorValidatedMicrosecondComputationDoesNotFailTheStoresMillisecondClock(): Unit = runBlocking {
        val store = PlannerStore(planningDisplayState(), nowEpochMillis = { Instant.parse(PLANNING_DISPLAY_NOW).toEpochMilli() })
        val fence = requireNotNull(store.captureRoutinePlanningInputFence())
        val display = planningDisplaySnapshot(requireNotNull(fence.state.routinePlanningInputCapsule))
        val transition = requireNotNull(store.installRoutinePlanningDisplay(fence, display) { true })
        assertTrue(transition.persistence.awaitDurable())
        assertEquals(PLANNING_DISPLAY_NOW, store.state.value.routinePlanningDisplaySnapshot?.computedAt)
        assertNull(store.state.value.routinePlanningDisplayAdmission)
    }

    @Test fun newPreviewAdmissionCannotTrimOversizedCombinedStateButLaterUnchangedCustodyStillSaves(): Unit = runBlocking {
        val base = planningDisplayState(); val capsule = requireNotNull(base.routinePlanningInputCapsule)
        val display = planningDisplaySnapshot(capsule)
        val padding = MAX_ROUTINE_PLANNING_CAPSULE_SNAPSHOT_ADMISSION_BYTES - ROUTINE_CAPSULE_JSON.encodeToString(base).toByteArray().size - 200
        val large = base.copy(inbox = listOf(InboxItem("synthetic-large", title = "Synthetic", source = InboxSource.QUICK_CAPTURE, detail = "x".repeat(padding))))
        val store = PlannerStore(large, nowEpochMillis = { now }); val fence = requireNotNull(store.captureRoutinePlanningInputFence())
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) { store.installRoutinePlanningDisplay(fence, display) { true } }
        assertSame(fence.state, store.state.value)
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        repository.save(base.copy(routinePlanningDisplaySnapshot = display))
        val pending = routineStateTestIntent(true).copy(syncOrigin = requireNotNull(base.canonicalSyncOrigin), configurationId = requireNotNull(base.canonicalConfigurationId))
        val later = large.copy(routinePlanningDisplaySnapshot = display, routineOccurrenceLedger = large.routineOccurrenceLedger.copy(
            observations = routineStateTestLedger().observations, pending = listOf(pending)))
        repository.save(later)
        assertTrue(requireNotNull(dao.snapshot).payload.toByteArray().size > MAX_ROUTINE_PLANNING_CAPSULE_SNAPSHOT_ADMISSION_BYTES)
        val restored = requireNotNull(repository.load())
        assertEquals(display, restored.routinePlanningDisplaySnapshot); assertEquals(listOf(pending), restored.routineOccurrenceLedger.pending)
    }

    private class RawDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        override suspend fun load(singletonId: Int) = snapshot
        override suspend fun save(snapshot: PlannerSnapshotEntity) { this.snapshot = snapshot }
    }
}
